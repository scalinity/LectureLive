//! The study page (spec §6.4): one distillation request over the whole notes and slides within a
//! word budget, one revision on overshoot, re-cut at every <h2>, cached against the prompt and the
//! notes in the Python CLI's own file, and filled into notes_template.html in one pass.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;
use chrono::NaiveDate;
use regex::{Captures, Regex};
use serde_json::{json, Map, Value};

use crate::fsutil::write_atomic;
use crate::notes::chat::{ChatClient, ChatError, ChatRequest, Content, Image};
use crate::notes::embeds::clean_output;
use crate::notes::prompts::{page_system, page_user, revise_system, revise_user};
use crate::pyjson::{dumps, round_int};
use crate::session::notesfile::sha256_hex;
use crate::session::spend::SpendKind;

/// The study page's design, embedded unchanged at build time (spec §3.1, §6.4).
pub const TEMPLATE: &str = include_str!("../../../../notes_template.html");
/// At the default (high) one request over a two-hour lecture ran past ten minutes (spec §6.4).
pub const EFFORT: &str = "medium";
const TIMEOUT: Duration = Duration::from_secs(1_200);
const EMBED_JPEG_QUALITY: u32 = 80;

fn re(p: &str) -> Regex {
    Regex::new(p).expect("a valid pattern")
}

static SLIDE_EMBED: LazyLock<Regex> = LazyLock::new(|| re(r"!\[Slide (\d+)\]\(([^)]+)\)"));
static TAGS: LazyLock<Regex> = LazyLock::new(|| re(r"(?is)<svg\b.*?</svg>|<[^>]+>"));
static UNSAFE: LazyLock<Regex> = LazyLock::new(|| re(r#"(?is)<script\b.*?</script\s*>|<style\b.*?</style\s*>|\s(?:on\w+|style)\s*=\s*(?:"[^"]*"|'[^']*'|[^\s>]+)"#));
static KEYSTONE: LazyLock<Regex> = LazyLock::new(|| re(r#"(?s)<div class="keystone">.*?</div>"#));
static LEDE: LazyLock<Regex> = LazyLock::new(|| re(r#"(?s)<p class="lede">.*?</p>"#));
static QUIZ: LazyLock<Regex> = LazyLock::new(|| re(r#"(?s)<ol class="quiz">.*</ol>"#));
static SECTION_TAG: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)</?section\b[^>]*>"));
static H2: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)<h2[\s>]"));
static SRC: LazyLock<Regex> = LazyLock::new(|| re(r#"src="([^"]+)""#));
static PLACEHOLDER: LazyLock<Regex> = LazyLock::new(|| re(r"\{\{(\w+)\}\}"));
static STAMP: LazyLock<Regex> = LazyLock::new(|| re(r"\d{8}"));
static ENTITY: LazyLock<Regex> = LazyLock::new(|| re(r"&(#[0-9]+|#[xX][0-9a-fA-F]+|[a-zA-Z]+);"));

pub fn notes_words(doc: &str) -> usize {
    SLIDE_EMBED.replace_all(doc, "").split_whitespace().count()
}

/// Visible words the page may use: 30% of the notes, in fifties, from 600 to 2,500.
pub fn page_budget(doc: &str) -> u32 {
    (round_int(notes_words(doc) as f64 * 0.3 / 50.0) * 50).clamp(600, 2_500) as u32
}

/// Slides the page may redraw: 30% of them, from 2 to 8.
pub fn max_slides(slides: usize) -> u32 {
    round_int(slides as f64 * 0.3).clamp(2, 8) as u32
}

/// The HTML entities a page uses, as Python's `html.unescape` reads them; others stay as written.
pub fn unescape(s: &str) -> String {
    ENTITY
        .replace_all(s, |c: &Captures| {
            let e = &c[1];
            let ch = if let Some(hex) = e.strip_prefix("#x").or_else(|| e.strip_prefix("#X")) {
                u32::from_str_radix(hex, 16).ok().and_then(char::from_u32)
            } else if let Some(dec) = e.strip_prefix('#') {
                dec.parse().ok().and_then(char::from_u32)
            } else {
                match e {
                    "amp" => Some('&'),
                    "lt" => Some('<'),
                    "gt" => Some('>'),
                    "quot" => Some('"'),
                    "apos" => Some('\''),
                    "nbsp" => Some('\u{a0}'),
                    "ndash" => Some('–'),
                    "mdash" => Some('—'),
                    "hellip" => Some('…'),
                    "times" => Some('×'),
                    "minus" => Some('−'),
                    _ => None,
                }
            };
            ch.map_or_else(|| c[0].to_string(), String::from)
        })
        .into_owned()
}

/// Python's `html.escape(s, quote=True)`.
pub fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#x27;")
}

/// Words a reader sees, the measure the budget is checked against; drawings are not counted.
pub fn visible_words(html: &str) -> usize {
    unescape(&TAGS.replace_all(html, " ")).split_whitespace().count()
}

/// The template owns all styling and behaviour: generated fragments may not bring their own.
pub fn strip_unsafe(html: &str) -> String {
    UNSAFE.replace_all(html, "").into_owned()
}

#[derive(Debug, Clone, PartialEq)]
pub struct Fills {
    pub keystone: String,
    pub summary: String,
    pub content: String,
    /// `{"N": "path"}` as the CLI writes it, with `</` escaped.
    pub slides: String,
}

impl Fills {
    /// The parts spec §6.4 promises that this page lacks.
    pub fn missing(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if self.keystone.is_empty() {
            out.push("keystone");
        }
        if self.summary.is_empty() {
            out.push("summary");
        }
        if !self.content.contains("<section class=\"part\">") {
            out.push("sections");
        }
        for (class, name) in [("glossary", "glossary"), ("takeaways", "takeaways"), ("quiz", "questions")] {
            if !self.content.contains(&format!("class=\"{class}\"")) {
                out.push(name);
            }
        }
        out
    }

    fn to_json(&self) -> Value {
        json!({"keystone": self.keystone, "summary": self.summary, "content": self.content, "slides": self.slides})
    }

    fn from_json(v: &Value) -> Option<Self> {
        let s = |k: &str| v[k].as_str().map(str::to_string);
        Some(Self { keystone: s("keystone")?, summary: s("summary")?, content: s("content")?, slides: s("slides")? })
    }
}

/// The CLI's cut of the model's page: keystone, lede and quiz lifted out, sections re-cut at every <h2>.
pub fn cut(out: &str, embeds: &[(String, String)]) -> Fills {
    let keystone = KEYSTONE.find(out).map(|m| m.as_str().to_string());
    let lede = LEDE.find(out).map(|m| m.as_str().to_string());
    let quiz = QUIZ.find(out).map(|m| m.as_str().to_string());
    let mut body = out.to_string();
    for m in [&keystone, &lede, &quiz].into_iter().flatten() {
        body = body.replace(m.as_str(), "");
    }
    let body = SECTION_TAG.replace_all(&body, "").into_owned();
    let mut bounds = vec![0];
    bounds.extend(H2.find_iter(&body).map(|m| m.start()).filter(|&s| s > 0));
    bounds.push(body.len());
    let mut sections: Vec<String> = bounds.windows(2).map(|w| body[w[0]..w[1]].trim()).filter(|c| !c.is_empty()).map(|c| format!("<section class=\"part\">\n{c}\n</section>")).collect();
    if let Some(q) = &quiz {
        sections.push(format!("<section class=\"part check\">\n<h2>Check yourself</h2>\n{q}\n</section>"));
    }
    let mut map = Map::new();
    for (n, p) in embeds {
        map.insert(n.clone(), json!(p));
    }
    Fills { keystone: keystone.unwrap_or_default(), summary: lede.unwrap_or_default(), content: sections.join("\n"), slides: dumps(&Value::Object(map)).replace("</", "<\\/") }
}

/// One pass: text inside the generated content is never read as a placeholder.
pub fn fill(template: &str, fills: &HashMap<&str, String>) -> String {
    PLACEHOLDER.replace_all(template, |c: &Captures| fills.get(&c[1]).cloned().unwrap_or_else(|| c[0].to_string())).into_owned()
}

/// The page is named after the lecture: the folder's title without its week prefix, with the date
/// only when the folder holds more than one day's notes.
pub fn page_path(notes: &Path, lecture_name: &str) -> PathBuf {
    let dir = notes.parent().unwrap_or(Path::new("."));
    let stem = notes.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let title = lecture_name.split_once(" — ").map(|(_, t)| t).filter(|t| !t.is_empty()).unwrap_or(lecture_name);
    let mut name = title.replace(['/', ':'], "-").trim().to_string();
    if name.is_empty() {
        name = stem.clone();
    }
    if let Some(stamp) = STAMP.find(&stem) {
        let is_notes = |e: &std::fs::DirEntry| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.starts_with("lecture_notes_") && n.ends_with(".md")
        };
        let days = std::fs::read_dir(dir).map(|e| e.flatten().filter(is_notes).count()).unwrap_or(0);
        if days > 1 {
            if let Ok(d) = NaiveDate::parse_from_str(stamp.as_str(), "%Y%m%d") {
                name += &format!(" ({})", d.format("%-d %b"));
            }
        }
    }
    dir.join(format!("{name}.html"))
}

/// A slide as a JPEG data URI (quality 80, subscripts still legible), or None if the file is gone.
pub fn slide_uri(path: &Path) -> Option<String> {
    if !path.exists() {
        return None;
    }
    let jpeg = std::env::temp_dir().join(format!("lecturelive-slide-{}.jpg", uuid::Uuid::new_v4()));
    let quality = EMBED_JPEG_QUALITY.to_string();
    let converted = Command::new("sips").args(["-s", "format", "jpeg", "-s", "formatOptions", &quality]).arg(path).arg("--out").arg(&jpeg).output().is_ok_and(|o| o.status.success());
    let (data, mime) = match std::fs::read(&jpeg).ok().filter(|_| converted) {
        Some(b) => (b, "image/jpeg"),
        None => (std::fs::read(path).ok()?, "image/png"),
    };
    let _ = std::fs::remove_file(&jpeg);
    Some(format!("data:{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(data)))
}

pub fn load_cache(path: &Path, key: &str) -> Option<(Fills, usize)> {
    let v: Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    if v["source"].as_str() != Some(key) {
        return None;
    }
    Some((Fills::from_json(&v["fills"])?, v["words"].as_u64()? as usize))
}

pub fn save_cache(path: &Path, key: &str, fills: &Fills, words: usize, budget: u32) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    write_atomic(path, dumps(&json!({"source": key, "fills": fills.to_json(), "words": words, "budget": budget})).as_bytes())
}

/// The page's HTML, written atomically beside the notes; slides are read fresh and embedded.
pub fn render(fills: &Fills, course: &str, notes: &Path, lecture_name: &str, template: &str) -> Result<PathBuf> {
    let (week, title) = match lecture_name.split_once(" — ") {
        Some((w, t)) if !t.is_empty() => (w, t),
        _ => ("", lecture_name),
    };
    let dir = notes.parent().unwrap_or(Path::new("."));
    let stem = notes.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let date = STAMP.find(&stem).and_then(|m| NaiveDate::parse_from_str(m.as_str(), "%Y%m%d").ok()).map(|d| d.format("%-d %B %Y").to_string()).unwrap_or_default();
    let paths: Vec<(String, String)> = match serde_json::from_str::<Value>(&fills.slides) {
        Ok(Value::Object(m)) => m.into_iter().filter_map(|(n, p)| p.as_str().map(|p| (n, p.to_string()))).collect(),
        _ => Vec::new(),
    };
    let mut uris: HashMap<String, String> = HashMap::new();
    for (_, p) in &paths {
        if !uris.contains_key(p) {
            uris.insert(p.clone(), slide_uri(&dir.join(p)).unwrap_or_else(|| p.clone()));
        }
    }
    let slides: Map<String, Value> = paths.iter().map(|(n, p)| (n.clone(), json!(uris[p]))).collect();
    let content = SRC.replace_all(&fills.content, |c: &Captures| match uris.get(&unescape(&c[1])) {
        Some(u) => format!("src=\"{u}\""),
        None => c[0].to_string(),
    });
    let values: HashMap<&str, String> = [
        ("keystone", fills.keystone.clone()),
        ("summary", fills.summary.clone()),
        ("content", content.into_owned()),
        ("slides", dumps(&Value::Object(slides))),
        ("week", escape(week)),
        ("title", escape(title)),
        ("course", escape(course)),
        ("date", date),
    ]
    .into();
    let path = page_path(notes, lecture_name);
    write_atomic(&path, fill(template, &values).as_bytes()).with_context(|| format!("write {}", path.display()))?;
    Ok(path)
}

/// One request with one retry: a failed request would otherwise cost the whole page.
async fn ask(chat: &ChatClient, system: String, content: Content) -> Result<String> {
    let req = ChatRequest { what: SpendKind::Page, system, content, effort: Some(EFFORT), timeout: TIMEOUT };
    let answer = match chat.complete(&req, &mut |_| {}).await {
        Ok(a) => a,
        Err(ChatError::Failed(_)) => chat.complete(&req, &mut |_| {}).await.map_err(|e| anyhow!("{e}"))?,
        Err(e) => bail!("{e}"),
    };
    Ok(strip_unsafe(&clean_output(&answer.text, false)))
}

#[derive(Debug, Clone, PartialEq)]
pub struct PageOutcome {
    pub path: PathBuf,
    pub words: usize,
    pub budget: u32,
    /// Filled from the cache, without a request.
    pub cached: bool,
    pub missing: Vec<&'static str>,
}

/// The CLI's `render_html`: typeset (or take the cached parts), then fill the template.
pub async fn make_page(chat: &ChatClient, course: &str, notes: &Path, cache: &Path, lecture_name: &str, template: &str, progress: &(dyn Fn(String) + Send + Sync)) -> Result<PageOutcome> {
    let doc = std::fs::read_to_string(notes).with_context(|| format!("read {}", notes.display()))?;
    let budget = page_budget(&doc);
    let embeds: Vec<(String, String)> = SLIDE_EMBED.captures_iter(&doc).map(|c| (c[1].to_string(), c[2].to_string())).collect();
    let system = page_system(course, budget, max_slides(embeds.len()));
    let key = sha256_hex(format!("{system}{doc}").as_bytes());
    let (fills, words, cached) = match load_cache(cache, &key) {
        Some((fills, words)) => (fills, words, true),
        None => {
            let words_in = notes_words(&doc);
            let numbers: Vec<String> = embeds.iter().map(|(n, _)| n.clone()).collect();
            let dir = notes.parent().unwrap_or(Path::new("."));
            let images = embeds.iter().map(|(_, p)| Image::read(&dir.join(p))).collect::<Result<Vec<_>>>()?;
            progress(format!("distilling {words_in} words into at most {budget}"));
            let mut out = ask(chat, system.clone(), Content::Parts { text: page_user(&doc, words_in, &numbers, budget), images }).await?;
            let mut words = visible_words(&out);
            if words as f64 > budget as f64 * 1.1 {
                progress(format!("cutting the draft from {words} words to {budget}"));
                out = ask(chat, revise_system(budget), Content::Text(revise_user(&out, words, budget))).await?;
                words = visible_words(&out);
            }
            let fills = cut(&out, &embeds);
            save_cache(cache, &key, &fills, words, budget)?;
            (fills, words, false)
        }
    };
    let path = render(&fills, course, notes, lecture_name, template)?;
    Ok(PageOutcome { path, words, budget, cached, missing: fills.missing() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(n: usize) -> String {
        (0..n).map(|i| format!("w{i}")).collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn the_budget_is_thirty_percent_of_the_notes_in_fifties_between_600_and_2500() {
        assert_eq!(page_budget(""), 600);
        assert_eq!(page_budget(&words(2_500)), 750);
        assert_eq!(page_budget(&words(2_584)), 800, "15.504 rounds up");
        assert_eq!(page_budget(&words(2_750)), 800, "16.5 rounds to even, as Python rounds");
        assert_eq!(page_budget(&words(10_000)), 2_500);
        assert_eq!(page_budget(&format!("{} ![Slide 1](slides/a.png)", words(2_500))), 750, "embeds are not words");
        assert_eq!([max_slides(0), max_slides(5), max_slides(10), max_slides(15), max_slides(40)], [2, 2, 3, 4, 8]);
    }

    #[test]
    fn visible_words_skip_markup_and_drawings_and_split_on_non_breaking_spaces() {
        let html = "<p>one &amp; two</p><svg viewBox=\"0 0 1 1\"><text>not counted</text></svg><p>three&nbsp;four</p>";
        assert_eq!(visible_words(html), 5);
        assert_eq!(unescape("&lt;a&gt; &#39;b&#x27; &quot;c&quot; &amp;amp;"), "<a> 'b' \"c\" &amp;");
        assert_eq!(escape("A & B <x> \"q\" Rock'n'Roll"), "A &amp; B &lt;x&gt; &quot;q&quot; Rock&#x27;n&#x27;Roll");
    }

    #[test]
    fn scripts_styles_and_event_or_style_attributes_are_stripped() {
        let html = "<p onclick=\"x()\" style='color:red' class=\"a\">a</p><script>alert(1)</script><STYLE>p{}</style><svg onload=go>b</svg>";
        assert_eq!(strip_unsafe(html), "<p class=\"a\">a</p><svg>b</svg>");
    }

    #[test]
    fn the_page_is_recut_at_every_h2_whatever_the_models_own_wrappers() {
        let out = "<div class=\"keystone\">\\[ x \\]</div>\n<p class=\"lede\">Lede.</p>\n<section class=\"part\"><h2>A</h2><p>a</p></section>\n<h2>B</h2><p>b</p>\n<section class=\"part\"><h2>Glossary</h2><dl class=\"glossary\"><dt>t</dt><dd>d</dd></dl></section>\n<ol class=\"quiz\"><li><p class=\"q\">Q?</p><div class=\"answer\">A.</div></li></ol>";
        let f = cut(out, &[("3".into(), "slides/x</y.png".into()), ("7".into(), "slides/b.png".into())]);
        assert_eq!(f.keystone, "<div class=\"keystone\">\\[ x \\]</div>");
        assert_eq!(f.summary, "<p class=\"lede\">Lede.</p>");
        assert_eq!(
            f.content,
            "<section class=\"part\">\n<h2>A</h2><p>a</p>\n</section>\n<section class=\"part\">\n<h2>B</h2><p>b</p>\n</section>\n<section class=\"part\">\n<h2>Glossary</h2><dl class=\"glossary\"><dt>t</dt><dd>d</dd></dl>\n</section>\n<section class=\"part check\">\n<h2>Check yourself</h2>\n<ol class=\"quiz\"><li><p class=\"q\">Q?</p><div class=\"answer\">A.</div></li></ol>\n</section>"
        );
        assert_eq!(f.slides, "{\"3\": \"slides/x<\\/y.png\", \"7\": \"slides/b.png\"}", "the CLI's JSON, safe inside a script element");
        assert_eq!(f.missing(), vec!["takeaways"]);
    }

    #[test]
    fn the_page_is_named_after_the_lecture_title() {
        let dir = tempfile::tempdir().unwrap();
        let notes = dir.path().join("lecture_notes_20260925.md");
        std::fs::write(&notes, "").unwrap();
        assert_eq!(page_path(&notes, "Week 06 — Statistical Analysis Methods"), dir.path().join("Statistical Analysis Methods.html"));
        assert_eq!(page_path(&notes, "Week 07 — Tests: t/z"), dir.path().join("Tests- t-z.html"));
        assert_eq!(page_path(&notes, "Optimisation"), dir.path().join("Optimisation.html"));
        std::fs::write(dir.path().join("lecture_notes_20260918.md"), "").unwrap();
        assert_eq!(page_path(&notes, "Week 06 — Statistical Analysis Methods"), dir.path().join("Statistical Analysis Methods (25 Sep).html"), "the date only when the folder holds two days");
    }

    #[test]
    fn the_template_is_filled_in_one_pass() {
        let fills: HashMap<&str, String> = [("title", "T &amp; U".to_string()), ("content", "<p>{{title}} stays</p>".to_string())].into();
        assert_eq!(fill("<t>{{title}}</t>{{content}}{{unknown}}", &fills), "<t>T &amp; U</t><p>{{title}} stays</p>{{unknown}}");
        assert!(TEMPLATE.contains("{{content}}") && TEMPLATE.contains("{{slides}}"), "the repository's template is embedded");
    }

    #[test]
    fn the_cache_is_the_clis_file_keyed_to_prompt_and_notes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".live_notes/x.page.json");
        let f = Fills { keystone: "k".into(), summary: "s".into(), content: "c — é".into(), slides: "{}".into() };
        save_cache(&path, "abc", &f, 612, 650).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"source\": \"abc\", \"fills\": {\"keystone\": \"k\", \"summary\": \"s\", \"content\": \"c \\u2014 \\u00e9\", \"slides\": \"{}\"}, \"words\": 612, \"budget\": 650}");
        assert_eq!(load_cache(&path, "abc"), Some((f, 612)));
        assert_eq!(load_cache(&path, "abd"), None);
    }
}
