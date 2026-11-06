//! Every change to the notes document (spec §6.2, §6.3, §8): the revision check, the journaled
//! snapshot commit, its recovery at launch, and polish's replace.
use std::fs::OpenOptions;
use std::io::Write;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::fsutil::write_atomic;
use crate::notes::timeline::hms;
use crate::session::files::LectureFiles;
use crate::session::sidecar::Sidecar;

/// A commit in progress (spec §6.2): present only between the journal step and the clear step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Journal {
    pub op_id: Uuid,
    /// The segment cursor before and after the block.
    pub segments: [u64; 2],
    /// The slide index before and after the block.
    pub slides: [u32; 2],
    pub before_len: u64,
    pub before_sha256: String,
    pub block_len: u64,
    pub block_sha256: String,
    /// The block itself: a torn append is truncated only when what reached the notes is its start.
    #[serde(default)]
    pub block: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Recovered {
    #[default]
    Nothing,
    /// The block never reached the notes: its material is pending.
    NotAppended,
    /// The block was fully written: the cursors now say so.
    Completed,
    /// Part of the block was written and has been removed: its material is pending.
    Truncated,
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

fn fingerprint(files: &LectureFiles) -> Result<(u64, String)> {
    let bytes = std::fs::read(&files.notes).with_context(|| format!("read {}", files.notes.display()))?;
    Ok((bytes.len() as u64, sha256_hex(&bytes)))
}

/// Spec §8: the notes as they are on disk are the current revision. An edit made outside the app
/// becomes a new revision; it is never overwritten from memory. True when there was one.
pub fn accept_external_edit(files: &LectureFiles, sc: &mut Sidecar) -> Result<bool> {
    let (len, sha) = fingerprint(files)?;
    if sc.notes.len == len && sc.notes.sha256 == sha {
        return Ok(false);
    }
    sc.notes.revision += 1;
    sc.notes.len = len;
    sc.notes.sha256 = sha;
    Ok(true)
}

/// The block a snapshot appends: the CLI's `<!-- HH:MM:SS -->` marker, then the notes.
pub fn block(at: DateTime<Local>, notes: &str) -> String {
    format!("\n<!-- {} -->\n{notes}\n", hms(at))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Step {
    Journal,
    Append,
    Cursors,
    Clear,
}

/// Appends a snapshot's block and moves the cursors to `segments_to` and `slides_to` (spec §6.2).
pub fn commit(files: &LectureFiles, sc: &mut Sidecar, block: &str, segments_to: u64, slides_to: u32) -> Result<()> {
    commit_through(files, sc, block, segments_to, slides_to, Step::Clear)
}

/// The commit, stopping after `last`: the fault-injection tests crash between every pair of steps.
pub(crate) fn commit_through(files: &LectureFiles, sc: &mut Sidecar, block: &str, segments_to: u64, slides_to: u32, last: Step) -> Result<()> {
    // A commit that failed earlier in this session left its journal: repair it before building on the notes.
    if files.journal().exists() && recover(files, sc)? == Recovered::Completed {
        bail!("an earlier snapshot turned out to be complete, so this batch is already partly in the notes; try again");
    }
    accept_external_edit(files, sc)?;
    let j = Journal {
        op_id: Uuid::new_v4(),
        segments: [sc.notes.segment_cursor, segments_to],
        slides: [sc.notes.slide_index, slides_to],
        before_len: sc.notes.len,
        before_sha256: sc.notes.sha256.clone(),
        block_len: block.len() as u64,
        block_sha256: sha256_hex(block.as_bytes()),
        block: block.to_string(),
    };
    write_atomic(&files.journal(), &serde_json::to_vec_pretty(&j)?).context("write the commit journal")?;
    if last == Step::Journal {
        return Ok(());
    }
    let mut notes = OpenOptions::new().append(true).open(&files.notes).with_context(|| format!("open {}", files.notes.display()))?;
    notes.write_all(block.as_bytes()).and_then(|()| notes.sync_all()).with_context(|| format!("append to {}", files.notes.display()))?;
    if last == Step::Append {
        return Ok(());
    }
    advance(files, sc, &j)?;
    if last == Step::Cursors {
        return Ok(());
    }
    clear(files)
}

fn advance(files: &LectureFiles, sc: &mut Sidecar, j: &Journal) -> Result<()> {
    let (len, sha) = fingerprint(files)?;
    let n = &mut sc.notes;
    n.revision += 1;
    n.len = len;
    n.sha256 = sha;
    n.segment_cursor = j.segments[1];
    n.slide_index = j.slides[1];
    sc.save(&files.sidecar())
}

fn clear(files: &LectureFiles) -> Result<()> {
    match std::fs::remove_file(files.journal()) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).context("clear the commit journal"),
    }
}

/// Launch recovery (spec §6.2): finishes or undoes an interrupted commit. Any state it cannot
/// verify stops with an error and changes nothing; nothing is truncated without a verified prefix.
pub fn recover(files: &LectureFiles, sc: &mut Sidecar) -> Result<Recovered> {
    let path = files.journal();
    let j: Journal = match std::fs::read(&path) {
        Ok(b) => serde_json::from_slice(&b).with_context(|| format!("the commit journal {} is unreadable: check {} by hand, then delete the journal", path.display(), files.notes.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Recovered::Nothing),
        Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
    };
    let data = std::fs::read(&files.notes).with_context(|| format!("read {}", files.notes.display()))?;
    let before = j.before_len as usize;
    let stop = |what: &str| anyhow::anyhow!("{what} since the interrupted snapshot in {}, so nothing was changed. Check {} by hand, then delete the journal to go on", path.display(), files.notes.display());
    if data.len() < before || sha256_hex(&data[..before]) != j.before_sha256 {
        return Err(stop("the notes no longer begin as they did"));
    }
    let tail = &data[before..];
    let outcome = if tail.is_empty() {
        Recovered::NotAppended
    } else if tail.len() as u64 == j.block_len && sha256_hex(tail) == j.block_sha256 {
        Recovered::Completed
    } else if (tail.len() as u64) < j.block_len && !j.block.is_empty() && j.block.as_bytes().starts_with(tail) {
        let f = OpenOptions::new().write(true).open(&files.notes)?;
        f.set_len(j.before_len)?;
        f.sync_all()?;
        Recovered::Truncated
    } else {
        return Err(stop("the notes hold other text after where the snapshot began (typed by hand, or grown past its block)"));
    };
    if outcome == Recovered::Completed {
        let applied = sc.notes.segment_cursor == j.segments[1] && sc.notes.slide_index == j.slides[1] && sc.notes.len == data.len() as u64;
        if !applied {
            advance(files, sc, &j)?;
        }
    } else {
        sc.notes.len = j.before_len;
        sc.notes.sha256 = j.before_sha256.clone();
        sc.save(&files.sidecar())?;
    }
    clear(files)?;
    Ok(outcome)
}

/// Polish's replace (spec §6.3). `based_on` is the SHA-256 of the notes the polish was made from:
/// if they have changed since, nothing is written. Returns the backup's path.
pub fn replace(files: &LectureFiles, sc: &mut Sidecar, polished: &str, based_on: &str, at: DateTime<Local>) -> Result<std::path::PathBuf> {
    write_replacement(files, sc, &format!("{}\n", polished.trim_end()), based_on, at)
}

/// `replace`, writing `text` exactly as it is.
fn write_replacement(files: &LectureFiles, sc: &mut Sidecar, text: &str, based_on: &str, at: DateTime<Local>) -> Result<std::path::PathBuf> {
    let current = std::fs::read(&files.notes).with_context(|| format!("read {}", files.notes.display()))?;
    if sha256_hex(&current) != based_on {
        bail!("the notes changed while they were being polished, so the polished version was not written; {} is as it was left", files.notes.display());
    }
    accept_external_edit(files, sc)?;
    let base = format!("{}_{}", files.stem, at.format("%H%M%S"));
    let backup = (1..).map(|n| files.state_dir().join(if n == 1 { format!("{base}.md") } else { format!("{base}_{n}.md") })).find(|p| !p.exists()).expect("an unused name");
    std::fs::write(&backup, &current).and_then(|()| std::fs::File::open(&backup)?.sync_all()).with_context(|| format!("back up the notes to {}", backup.display()))?;
    write_atomic(&files.notes, text.as_bytes()).with_context(|| format!("write {}", files.notes.display()))?;
    let (len, sha) = fingerprint(files)?;
    sc.notes.revision += 1;
    sc.notes.len = len;
    sc.notes.sha256 = sha;
    sc.save(&files.sidecar())?;
    Ok(backup)
}

/// How the notes refer to one slide's file (spec §7.7). Links can be written in more ways than are worth parsing
/// (inline, by reference, in HTML with or without quotes, encoded, escaped, with a fragment or a query, through `./`, in
/// another case on a disk that ignores case), so the notes are searched for what every one of them keeps. A slide's file
/// is named `slide_NN_HHMMSS`, and its six capture-time digits survive all of these: digits have no Markdown escape, no
/// named HTML reference and no case, and percent and numeric escapes are undone first. The notes write times with colons,
/// so the digits stand together only in a file name; two captures in one second only count each other, which keeps
/// both. A file named otherwise is searched for by its stem, case folded and with backslash escapes taken out. A count
/// too high only keeps a slide. A slide may leave the lecture only when it is not named at all, or named once, by its
/// own embed on a line of its own outside code.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct References {
    /// Where the decoded notes name the file's stem.
    pub mentions: usize,
    /// Own-line copies of its embed outside code.
    pub own_line_embeds: usize,
}

fn percent_decoded(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match (bytes[i], bytes.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(std::str::from_utf8(h).ok()?, 16).ok())) {
            (b'%', Some(b)) => {
                out.push(b);
                i += 3;
            }
            (b, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A numeric character reference, with or without its semicolon, as HTML reads it.
static CHARACTER: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r"&#(?:[xX]([0-9a-fA-F]+)|([0-9]+));?").expect("a valid pattern"));

/// Every named character reference HTML defines whose text is ASCII: 54 of the WHATWG table's 2,231. No named reference
/// spells a digit, and the only letters any spells are `fj`, so the rest cannot build a capture's digits, an escape, or
/// `javascript:`. The eight without a semicolon are the ones HTML also reads bare.
const ASCII_NAMED: [(&str, &str); 54] = [
    ("AMP", "&"), ("AMP;", "&"), ("DiacriticalGrave;", "`"), ("GT", ">"), ("GT;", ">"), ("Hat;", "^"), ("LT", "<"),
    ("LT;", "<"), ("NewLine;", "\n"), ("QUOT", "\""), ("QUOT;", "\""), ("Tab;", "\t"), ("UnderBar;", "_"),
    ("VerticalLine;", "|"), ("amp", "&"), ("amp;", "&"), ("apos;", "'"), ("ast;", "*"), ("bsol;", "\\"),
    ("colon;", ":"), ("comma;", ","), ("commat;", "@"), ("dollar;", "$"), ("equals;", "="), ("excl;", "!"),
    ("fjlig;", "fj"), ("grave;", "`"), ("gt", ">"), ("gt;", ">"), ("lbrace;", "{"), ("lbrack;", "["), ("lcub;", "{"),
    ("lowbar;", "_"), ("lpar;", "("), ("lsqb;", "["), ("lt", "<"), ("lt;", "<"), ("midast;", "*"), ("num;", "#"),
    ("percnt;", "%"), ("period;", "."), ("plus;", "+"), ("quest;", "?"), ("quot", "\""), ("quot;", "\""),
    ("rbrace;", "}"), ("rbrack;", "]"), ("rcub;", "}"), ("rpar;", ")"), ("rsqb;", "]"), ("semi;", ";"), ("sol;", "/"),
    ("verbar;", "|"), ("vert;", "|"),
];

/// A named character reference: a name, with its semicolon if it has one.
static NAMED: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r"&([A-Za-z][A-Za-z0-9]*;?)").expect("a valid pattern"));

/// Undoes the ASCII named references, reading a bare one as HTML does: the longest name without a semicolon that the
/// reference starts with.
fn named_decoded(text: &str) -> String {
    let ascii = |name: &str| ASCII_NAMED.iter().find(|(n, _)| *n == name).map(|(_, v)| *v);
    NAMED
        .replace_all(text, |c: &regex::Captures| {
            let name = &c[1];
            if let Some(v) = ascii(name) {
                return v.to_string();
            }
            let bare = ASCII_NAMED.iter().filter(|(n, _)| !n.ends_with(';') && name.starts_with(n)).max_by_key(|(n, _)| n.len());
            bare.map_or_else(|| c[0].to_string(), |(n, v)| format!("{v}{}", &name[n.len()..]))
        })
        .into_owned()
}

/// The notes as a reader of their links sees them. Every step between a link's text and the file it opens is applied
/// together, again and again until nothing changes, so the steps undo in whatever order they were stacked: HTML reads its
/// character references (numeric ones by their code point: those HTML remaps, 0x80 to 0x9F and a few others, become no
/// ASCII either way), Markdown drops a backslash, a browser drops tabs and line breaks from a URL and undoes its percent
/// escapes. Each pass only shortens the text, so it settles; `None` when a name is escaped more deeply than
/// [`DEPTH`] passes undo.
fn decoded_notes(doc: &str) -> Option<String> {
    let mut text = doc.to_string();
    for _ in 0..DEPTH {
        let numeric = CHARACTER.replace_all(&text, |c: &regex::Captures| {
            let n = c.get(1).map_or_else(|| c[2].parse::<u32>().ok(), |h| u32::from_str_radix(h.as_str(), 16).ok());
            n.and_then(char::from_u32).map_or(c[0].to_string(), |ch| ch.to_string())
        });
        let named = named_decoded(&numeric);
        let bare: String = named.chars().filter(|c| !matches!(c, '\\' | '\t' | '\n' | '\r')).collect();
        let next = percent_decoded(&bare);
        if next == text {
            return Some(text);
        }
        text = next;
    }
    None
}

const DEPTH: usize = 32;

/// Whether a link in the slides' folders (`slides/`, `slides/removed/`) leads to `file` under another name: the notes
/// may name the link. A link that cannot be read counts, since it might, and so does a folder that cannot be listed; only
/// a folder that is not there holds no link.
pub fn has_alias(slides: &std::path::Path, file: &std::path::Path) -> bool {
    let Ok(target) = file.canonicalize() else { return true };
    for dir in [slides.to_path_buf(), slides.join("removed")] {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return true,
        };
        for entry in entries {
            let Ok(entry) = entry else { return true };
            let Ok(kind) = entry.file_type() else { return true };
            if kind.is_symlink() && entry.path().canonicalize().map_or(true, |p| p == target) {
                return true;
            }
        }
    }
    false
}

/// Styles and scripts in the notes: a style spells a name with its own escapes (in a `<style>`, a `style` attribute, a
/// linked sheet, or SVG, whose attributes are read as styles) and a script builds one (in a `<script>`, an `on…`
/// handler, a `javascript:` link), so no search of the text finds what they refer to. Notes that hold any keep every
/// capture. Looked for in the notes as written and as decoded, where an escaped `javascript:` shows itself.
static STYLE_OR_SCRIPT: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r"(?i)<\s*(?:style|script|link|svg)\b|\b(?:style|on[a-z][a-z0-9]*)\s*=|\bjavascript\s*:").expect("a valid pattern"));

static SLIDE_STEM: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r"(?i)^slide_\d+_(\d{6})$").expect("a valid pattern"));

fn opaque_in(doc: &str, decoded: Option<&str>) -> bool {
    decoded.is_none_or(|t| STYLE_OR_SCRIPT.is_match(doc) || STYLE_OR_SCRIPT.is_match(t))
}

/// Whether these notes hold a style or a script, or escapes deeper than the count follows: then every capture counts as
/// referred to, and tidying keeps them all (spec §7.7).
pub fn opaque(doc: &str) -> bool {
    opaque_in(doc, decoded_notes(doc).as_deref())
}

pub fn references(doc: &str, file: &std::path::Path, embed: &str) -> References {
    let stem = file.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let decoded = decoded_notes(doc);
    let opaque = opaque_in(doc, decoded.as_deref());
    let text = decoded.unwrap_or_default();
    // Every capture is named slide_NN_HHMMSS; a file named otherwise could be spelled in ways the digits are not, so it
    // stays.
    let mentions = SLIDE_STEM.captures(&stem).map_or(2, |c| text.matches(&c[1]).count());
    let mentions = if opaque { mentions.max(2) } else { mentions };
    let notes = crate::notes::embeds::read(doc);
    let own_line_embeds = (0..notes.lines.len()).filter(|&i| notes.own_embed(i) == Some(embed)).count();
    References { mentions, own_line_embeds }
}

/// Takes slides' embeds out of the notes, or puts another slide's embed in one's place (spec §7.7): each change is
/// `(embed, replacement)`, the embed as it is written on its own line. An embed is changed only where it stands once,
/// on its own line as an image; any other copy (inline, repeated, literal, or the line edited by hand) leaves that one
/// as it is. The notes are backed up and written as polish writes them. Returns which of `changes` were made.
pub fn change_embeds(files: &LectureFiles, sc: &mut Sidecar, doc: &str, changes: &[(String, Option<String>)], at: DateTime<Local>) -> Result<Vec<usize>> {
    // Each line keeps its own ending, so only the lines changed differ from the notes as they were.
    let notes = crate::notes::embeds::read(doc);
    let lines: Vec<&str> = notes.lines.iter().map(|l| l.1).collect();
    let code = &notes.literal;
    let mut out: Vec<Option<String>> = notes.lines.iter().map(|(_, line, ending)| Some(format!("{line}{ending}"))).collect();
    let mut made = Vec::new();
    let mut removed = Vec::new();
    for (k, (embed, replacement)) in changes.iter().enumerate() {
        let own: Vec<usize> = (0..lines.len()).filter(|&i| notes.own_embed(i) == Some(embed.as_str())).collect();
        let anywhere = lines.iter().map(|l| l.matches(embed.as_str()).count()).sum::<usize>();
        if own.len() == 1 && anywhere == 1 {
            let ending = notes.lines[own[0]].2;
            out[own[0]] = replacement.as_ref().map(|r| format!("{r}{ending}"));
            if replacement.is_none() {
                removed.push(own[0]);
            }
            made.push(k);
        }
    }
    if made.is_empty() {
        return Ok(made);
    }
    // Only what the change touched is tidied, outside code: the blank lines a removed embed leaves side by side become
    // one, and a "Slides not placed" heading whose section it emptied goes with that section's blank lines.
    let blank = |out: &[Option<String>], i: usize| out[i].as_deref().is_some_and(|l| l.trim().is_empty());
    removed.sort_unstable();
    for &r in &removed {
        let prev = (0..r).rev().find(|&i| out[i].is_some());
        let next = (r + 1..out.len()).find(|&i| out[i].is_some());
        if let (Some(p), Some(n)) = (prev, next) {
            if !code[p] && !code[n] && blank(&out, p) && blank(&out, n) {
                out[n] = None;
            }
        }
    }
    for h in (0..lines.len()).filter(|&i| !code[i] && lines[i].trim() == crate::notes::embeds::NOT_PLACED) {
        let end = (h + 1..lines.len()).find(|&i| !code[i] && lines[i].starts_with('#')).unwrap_or(lines.len());
        let emptied = removed.iter().any(|&r| r > h && r < end) && (h + 1..end).all(|i| out[i].is_none() || blank(&out, i));
        if emptied {
            for line in out.iter_mut().take(end).skip(h) {
                *line = None;
            }
        }
    }
    // And the file ends as it did: an emptied section at the end leaves no blank line behind.
    let kept: String = out.into_iter().flatten().collect();
    let kept = format!("{}{}", kept.trim_end(), &doc[doc.trim_end().len()..]);
    write_replacement(files, sc, &kept, &sha256_hex(doc.as_bytes()), at)?;
    Ok(made)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDate, TimeZone};
    use std::io::Write;

    fn folder() -> (tempfile::TempDir, LectureFiles, Sidecar) {
        let dir = tempfile::tempdir().unwrap();
        let files = LectureFiles::standard(dir.path(), NaiveDate::from_ymd_opt(2026, 9, 25).unwrap());
        std::fs::create_dir_all(files.state_dir()).unwrap();
        std::fs::write(&files.notes, "# Machine Learning — Week 01 — 2026-09-25\n").unwrap();
        let mut sc = Sidecar::default();
        assert!(accept_external_edit(&files, &mut sc).unwrap(), "a new file is revision 1");
        sc.save(&files.sidecar()).unwrap();
        (dir, files, sc)
    }

    const BLOCK: &str = "\n<!-- 10:05:00 -->\n## Gradient descent\n- Steps against the gradient.\n";

    #[test]
    fn an_embed_on_its_own_line_goes_with_one_blank_line_and_the_notes_are_backed_up() {
        let (_dir, files, mut sc) = folder();
        let doc = "# T\n\n## Builds\n\n![Slide 1](slides/slide_01_100000.png)\n\nA partial slide.\n\n![Slide 2](slides/slide_02_100001.png)\n\n```\n![Slide 3](slides/slide_03_100002.png)\n```\nInline ![Slide 4](slides/slide_04_100003.png) here.\n";
        std::fs::write(&files.notes, doc).unwrap();
        let changes: Vec<(String, Option<String>)> = [1, 3, 4].iter().map(|n| (format!("![Slide {n}](slides/slide_{n:02}_10000{}.png)", n - 1), None)).collect();
        accept_external_edit(&files, &mut sc).unwrap();
        let before = sc.notes.revision;
        let made = change_embeds(&files, &mut sc, &std::fs::read_to_string(&files.notes).unwrap(), &changes, chrono::Local::now()).unwrap();
        assert_eq!(made, [0], "the fenced and the inline copies stay");
        let after = std::fs::read_to_string(&files.notes).unwrap();
        assert_eq!(after, "# T\n\n## Builds\n\nA partial slide.\n\n![Slide 2](slides/slide_02_100001.png)\n\n```\n![Slide 3](slides/slide_03_100002.png)\n```\nInline ![Slide 4](slides/slide_04_100003.png) here.\n");
        assert_eq!(sc.notes.revision, before + 1);
        assert_eq!(sc.notes.sha256, sha256_hex(after.as_bytes()));
        let backups: Vec<_> = std::fs::read_dir(files.state_dir()).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().ends_with(".md")).collect();
        assert_eq!(std::fs::read_to_string(backups[0].path()).unwrap(), doc);
    }

    #[test]
    fn a_replacement_takes_the_embeds_place_and_an_emptied_not_placed_heading_goes() {
        let (_dir, files, mut sc) = folder();
        std::fs::write(&files.notes, "# T\n\n![Slide 1](slides/a.png)\n\n### Slides not placed\n\n![Slide 2](slides/b.png)\n").unwrap();
        let made = change_embeds(&files, &mut sc, &std::fs::read_to_string(&files.notes).unwrap(), &[("![Slide 1](slides/a.png)".into(), Some("![Slide 5](slides/e.png)".into())), ("![Slide 2](slides/b.png)".into(), None)], chrono::Local::now()).unwrap();
        assert_eq!(made, [0, 1]);
        assert_eq!(std::fs::read_to_string(&files.notes).unwrap(), "# T\n\n![Slide 5](slides/e.png)\n");
    }

    #[test]
    fn only_the_lines_around_the_removed_embed_change() {
        // F007: blank lines and a "not placed" heading elsewhere, inside code, are the notes' own.
        let (_dir, files, mut sc) = folder();
        let code = "```markdown\nfirst\n\n\nsecond\n### Slides not placed\n## Next\n```";
        let doc = format!("# T\n\nText.\n\n\nMore.\n\n{code}\n\n![Slide 1](slides/a.png)\n\nAfter.\n");
        std::fs::write(&files.notes, &doc).unwrap();
        change_embeds(&files, &mut sc, &std::fs::read_to_string(&files.notes).unwrap(), &[("![Slide 1](slides/a.png)".into(), None)], chrono::Local::now()).unwrap();
        assert_eq!(std::fs::read_to_string(&files.notes).unwrap(), format!("# T\n\nText.\n\n\nMore.\n\n{code}\n\nAfter.\n"));
    }

    #[test]
    fn a_slide_is_referred_to_wherever_its_name_stands_however_the_link_is_written() {
        // F013, F015, F016: inline, by reference, unquoted, encoded once or twice, with a fragment or a query.
        let file = std::path::Path::new("/lec/slides/slide_04_100000.png");
        let embed = "![Slide 4](slides/slide_04_100000.png)";
        let mentions = |doc: &str| references(doc, file, embed).mentions;
        assert_eq!(references("# T\n", file, embed), References::default());
        assert_eq!(references(&format!("# T\n\n{embed}\n"), file, embed), References { mentions: 1, own_line_embeds: 1 });
        for doc in [
            "![detail](slides/%73lide_04_100000.png)",
            "![detail][s]\n\n[s]: slides/%73lide_04_100000.png",
            "<img src=slides/%73lide_04_100000.png>",
            "![d](slides/removed/%73lide_04_100000.png#detail)",
            "![d](slides/%73lide_04_100000.png?v=2)",
            "![d](slides/%2573lide_04_100000.png)",
            "<img src=\"slides/&#115;lide_04_100000.png\">",
            "See [it](slides/./slide_04_100000.png).",
            "![detail](slides/slide\\_04\\_100000.png)",
            "<img src=\"slides/slide&lowbar;04&lowbar;100000.png\">",
            "![detail](slides/removed/SLIDE_04_100000.png)",
            "<img src=\"slides/removed/slide_04_&#49&#48&#48&#48&#48&#48.png\">",
            "<img src=\"slides/slide_04_&#x31&#x30&#x30&#x30&#x30&#x30.png\">",
            "<img src=\"slides/removed/slide_04_10&#10;0000.png\">",
            "<img src=\"slides/slide_04_10&NewLine;0000.png\">",
            "<img src=\"slides/slide_04_10\n0000.png\">",
            "<img src=\"slides/removed/slide_04_1&percnt;300000.png\">",
            "![detail](slides/removed/slide_04_1\\%300000.png)",
            "<img src=\"slides/removed/slide_04_1%3&Tab;00000.png\">",
            "<img src=\"slides/slide_04_1&amp;percnt;3&#48;0000.png\">",
        ] {
            assert_eq!(mentions(doc), 1, "{doc}");
        }
        assert_eq!(mentions(&format!("{embed}\n\n![d](slides/slide\\_04\\_100000.png)")), 2, "an escaped second reference");
        assert_eq!(mentions(&format!("{embed}\n\n<img src=\"slides/slide_04_&#49&#48&#48&#48&#48&#48.png\">")), 2, "and an unterminated one");
        // F024: escapes stacked in any order undo together.
        for other in ["<img src=\"slides/slide_04_1&percnt;300000.png\">", "![d](slides/slide_04_1\\%300000.png)", "<img src=\"slides/slide_04_1%3&Tab;00000.png\">"] {
            assert_eq!(mentions(&format!("{embed}\n\n{other}")), 2, "{other}");
        }
        let deep = format!("<img src=\"slides/slide_04_1%{}300000.png\">", "25".repeat(40));
        assert_eq!(mentions(&deep), 2, "escaped more deeply than the passes follow, every capture stays");
        assert_eq!(mentions("<!-- 10:00:00 -->\n\n## At 10:00:00"), 0, "times in the notes are written with colons");
        let other = std::path::Path::new("/lec/slides/removed/board_photo.png");
        assert_eq!(references("<img src=\"slides/removed/board&lowbar;photo.png\">", other, "").mentions, 2, "a name of another shape always stays");
        assert_eq!(references("# T\n", other, "").mentions, 2, "even with nothing naming it");
        assert_eq!(mentions(&format!("{embed}\n\nSee [it](slides/./slide_04_100000.png).")), 2);
        assert_eq!(mentions("![x](slides/slide_05_100001.png)"), 0);
        assert_eq!(mentions("![x](slides/slide_05_100000.png)"), 1, "a capture in the same second counts: it only keeps");
    }

    #[test]
    fn named_references_read_as_html_reads_them() {
        assert_eq!(named_decoded("a&ampb &lt;x&gt; &colon; &lowbar;&UnderBar; &fjlig; &nbsp; &unknown; &percnt"), "a&b <x> : __ fj &nbsp; &unknown; &percnt");
    }

    #[test]
    fn notes_with_styles_or_scripts_keep_every_capture() {
        // F023: CSS reads `\31 ` as `1`, so a style can name the file without its digits standing together.
        let file = std::path::Path::new("/lec/slides/removed/slide_04_100000.png");
        let embed = "![Slide 4](slides/slide_04_100000.png)";
        let figure = "<div style=\"background-image:url('slides/removed/slide_04_\\31 00000.png')\"></div>";
        // F025: a script builds a name in a handler or a `javascript:` link as well, escaped or not.
        let handler = "<img src=\"missing.png\" onerror=\"this.src='slides/removed/slide_04_'+'10'+'0000.png'\">";
        let link = "<a href=\"java&#x73;cript:void(location='slides/removed/slide_04_'+'100'+'000.png')\">it</a>";
        for doc in [
            figure.to_string(),
            format!("{embed}\n\n{figure}"),
            "<STYLE>p{}</STYLE>".into(),
            "<script>x()</script>".into(),
            "<link rel=stylesheet href=a.css>".into(),
            "<svg><rect mask=\"url('x.png')\"/></svg>".into(),
            handler.into(),
            format!("{embed}\n\n{handler}"),
            link.into(),
            "<a href=\"jav&Tab;ascript:go()\">it</a>".into(),
            // F026: HTML reads every named reference, `&colon;` among them.
            "<a href=\"javascript&colon;void(location='slides/removed/slide_04_'+'100'+'000.png')\">it</a>".into(),
            "<a href=\"javascript&#58void(0)\">it</a>".into(),
        ] {
            assert_eq!(references(&doc, file, embed).mentions, 2, "{doc}");
        }
        assert_eq!(references(&format!("# T\n\n{embed}\n\nIn this style, $\\beta = 1$; merge on = id, one-hot, JavaScript.\n"), file, embed).mentions, 1, "prose, maths and code words keep the common path");
    }

    #[test]
    fn a_folder_that_cannot_be_listed_may_hold_a_link() {
        // F022: a traversable folder that cannot be listed hides any link in it.
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let slides = dir.path().join("slides");
        std::fs::create_dir_all(slides.join("removed")).unwrap();
        let target = slides.join("removed/slide_04_100000.png");
        std::fs::write(&target, b"png").unwrap();
        assert!(!has_alias(&slides, &target), "no links, nothing to keep for");
        std::fs::remove_dir(slides.join("removed")).ok();
        let moved = slides.join("slide_04_100000.png");
        std::fs::write(&moved, b"png").unwrap();
        assert!(!has_alias(&slides, &moved), "a missing removed/ holds no link");
        std::fs::create_dir_all(slides.join("removed")).unwrap();
        std::fs::rename(&moved, &target).unwrap();
        std::os::unix::fs::symlink(&target, slides.join("detail.png")).unwrap();
        std::fs::set_permissions(&slides, std::fs::Permissions::from_mode(0o111)).unwrap();
        let unlisted = has_alias(&slides, &target);
        std::fs::set_permissions(&slides, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(unlisted, "a folder that cannot be listed may hold one");
    }

    #[test]
    fn an_edit_decided_on_notes_since_changed_is_refused() {
        // R15: the caller decided what to take out on one reading; the person saved another link since.
        let (_dir, files, mut sc) = folder();
        std::fs::write(&files.notes, "# T\n\n![Slide 1](slides/a.png)\n").unwrap();
        accept_external_edit(&files, &mut sc).unwrap();
        let decided = std::fs::read_to_string(&files.notes).unwrap();
        std::fs::write(&files.notes, format!("{decided}\n[detail](slides/a.png)\n")).unwrap();
        let now = std::fs::read_to_string(&files.notes).unwrap();
        assert!(change_embeds(&files, &mut sc, &decided, &[("![Slide 1](slides/a.png)".into(), None)], chrono::Local::now()).is_err());
        assert_eq!(std::fs::read_to_string(&files.notes).unwrap(), now, "the newer notes are as the person left them");
    }

    #[test]
    fn an_embed_edit_changes_only_its_own_line() {
        // R27: the notes were rebuilt with \n endings and their trailing whitespace trimmed.
        let (_dir, files, mut sc) = folder();
        std::fs::write(&files.notes, "# T\r\n\r\nKept  \r\n\r\n![Slide 1](slides/a.png)\r\n\r\nAfter.\r\n\r\n").unwrap();
        accept_external_edit(&files, &mut sc).unwrap();
        let doc = std::fs::read_to_string(&files.notes).unwrap();
        change_embeds(&files, &mut sc, &doc, &[("![Slide 1](slides/a.png)".into(), None)], chrono::Local::now()).unwrap();
        assert_eq!(std::fs::read_to_string(&files.notes).unwrap(), "# T\r\n\r\nKept  \r\n\r\nAfter.\r\n\r\n");
    }

    #[test]
    fn a_literal_embed_in_code_is_not_the_notes_embed() {
        // R14: ``` inside a ~~~~ block did not close it. M06: a fence after a nested list is the outer item's, so a line
        // four columns past it does not close it. M07: a tab after a list marker runs to the next four-column stop.
        for notes in [
            "# T\n\n~~~~markdown\n```\n![Slide 1](slides/a.png)\n~~~~\n",
            "# T\n\n- outer\n  - inner\n\n  ```text\n  first\n  ```\n\n    ```markdown\n      ```\n    ![Slide 1](slides/a.png)\n  ```\n",
            "# T\n\n-\titem\n\t```rust\n\tlet x = 1;\n\t```\n\n```markdown\n![Slide 1](slides/a.png)\n```\n",
            // M09: a code span across lines, and an HTML block. M10: a lone CR then a CRLF leave a blank line.
            "# T\n\nParagraph\n    ```markdown\n![Slide 1](slides/a.png)\n    ```\n",
            "# T\n\n<pre>\n```markdown\n![Slide 1](slides/a.png)\n```\n</pre>\n",
            "# T\n\nParagraph\r\r\n    ~~~markdown\n    ![Slide 1](slides/a.png)\n    ~~~\n",
            // M12: inside a code span after prose. M13: in a link reference's title.
            "# T\n\nWrite `![Slide 1](slides/a.png)` literally.\n",
            "# T\n\n[r]: /r \"\n![Slide 1](slides/a.png)\n\"\n\n[r]\n",
            // M16: a fence whose lines end at a lone CR.
            "# T\n\n```markdown\r\r![Slide 1](slides/a.png)\r```\n",
        ] {
            let (_dir, files, mut sc) = folder();
            std::fs::write(&files.notes, notes).unwrap();
            accept_external_edit(&files, &mut sc).unwrap();
            assert_eq!(references(notes, std::path::Path::new("/lec/slides/a.png"), "![Slide 1](slides/a.png)").own_line_embeds, 0, "{notes:?}");
            assert!(change_embeds(&files, &mut sc, notes, &[("![Slide 1](slides/a.png)".into(), None)], chrono::Local::now()).unwrap().is_empty());
            assert_eq!(std::fs::read_to_string(&files.notes).unwrap(), notes);
        }
    }

    #[test]
    fn a_lone_carriage_return_ends_a_line_for_the_count_and_the_edit() {
        // M14: the parser reads the heading and the embed as two lines, so the count and the edit read them so too.
        let (_dir, files, mut sc) = folder();
        let notes = "# T\r\n\r\n## Notes\r![Slide 1](slides/a.png)\r\n\r\nAfter.\r\n";
        std::fs::write(&files.notes, notes).unwrap();
        accept_external_edit(&files, &mut sc).unwrap();
        assert_eq!(references(notes, std::path::Path::new("/lec/slides/a.png"), "![Slide 1](slides/a.png)").own_line_embeds, 1);
        assert_eq!(change_embeds(&files, &mut sc, notes, &[("![Slide 1](slides/a.png)".into(), None)], chrono::Local::now()).unwrap(), [0]);
        assert_eq!(std::fs::read_to_string(&files.notes).unwrap(), "# T\r\n\r\n## Notes\r\r\nAfter.\r\n");
    }

    #[test]
    fn nothing_to_change_writes_nothing() {
        let (_dir, files, mut sc) = folder();
        let before = std::fs::read_to_string(&files.notes).unwrap();
        assert!(change_embeds(&files, &mut sc, &std::fs::read_to_string(&files.notes).unwrap(), &[("![Slide 9](slides/x.png)".into(), None)], chrono::Local::now()).unwrap().is_empty());
        assert_eq!(std::fs::read_to_string(&files.notes).unwrap(), before);
    }

    #[derive(Debug, Clone, Copy)]
    enum Crash {
        TmpJournal,
        After(Step),
        MidAppend,
    }

    #[test]
    fn a_crash_at_every_commit_step_recovers_to_the_block_exactly_once() {
        for crash in [Crash::TmpJournal, Crash::After(Step::Journal), Crash::MidAppend, Crash::After(Step::Append), Crash::After(Step::Cursors), Crash::After(Step::Clear)] {
            let (_dir, files, sc0) = folder();
            let before = std::fs::read_to_string(&files.notes).unwrap();
            let mut sc = sc0.clone();
            match crash {
                Crash::TmpJournal => std::fs::write(files.journal().with_extension("tmp"), b"{\"op_id\":").unwrap(),
                Crash::After(step) => commit_through(&files, &mut sc, BLOCK, 5, 2, step).unwrap(),
                Crash::MidAppend => {
                    commit_through(&files, &mut sc, BLOCK, 5, 2, Step::Journal).unwrap();
                    std::fs::OpenOptions::new().append(true).open(&files.notes).unwrap().write_all(&BLOCK.as_bytes()[..20]).unwrap();
                }
            }
            // The process dies here: memory is gone, the files stay.
            let mut sc = Sidecar::load(&files.sidecar()).unwrap().unwrap();
            let outcome = recover(&files, &mut sc).unwrap();
            let expected = match crash {
                Crash::TmpJournal | Crash::After(Step::Clear) => Recovered::Nothing,
                Crash::After(Step::Journal) => Recovered::NotAppended,
                Crash::MidAppend => Recovered::Truncated,
                Crash::After(_) => Recovered::Completed,
            };
            assert_eq!(outcome, expected, "{crash:?}");
            let appended = matches!(crash, Crash::After(Step::Append | Step::Cursors | Step::Clear));
            let notes = std::fs::read_to_string(&files.notes).unwrap();
            assert_eq!(notes, if appended { format!("{before}{BLOCK}") } else { before.clone() }, "{crash:?}");
            assert_eq!((sc.notes.segment_cursor, sc.notes.slide_index), if appended { (5, 2) } else { (0, 0) }, "{crash:?}");
            assert_eq!((sc.notes.len, sc.notes.sha256.clone()), (notes.len() as u64, sha256_hex(notes.as_bytes())), "{crash:?}");
            assert!(!files.journal().exists(), "{crash:?}");
            assert_eq!(Sidecar::load(&files.sidecar()).unwrap().unwrap(), sc, "what recovery decided is saved ({crash:?})");
            if !appended {
                commit(&files, &mut sc, BLOCK, 5, 2).unwrap();
            }
            assert_eq!(std::fs::read_to_string(&files.notes).unwrap(), format!("{before}{BLOCK}"), "the batch lands exactly once ({crash:?})");
        }
    }

    #[test]
    fn the_journal_records_the_batch_and_both_fingerprints() {
        let (_dir, files, mut sc) = folder();
        let before = std::fs::read(&files.notes).unwrap();
        commit_through(&files, &mut sc, BLOCK, 5, 2, Step::Journal).unwrap();
        let j: Journal = serde_json::from_slice(&std::fs::read(files.journal()).unwrap()).unwrap();
        assert_eq!((j.segments, j.slides), ([0, 5], [0, 2]));
        assert_eq!((j.before_len, j.before_sha256), (before.len() as u64, sha256_hex(&before)));
        assert_eq!((j.block_len, j.block_sha256), (BLOCK.len() as u64, sha256_hex(BLOCK.as_bytes())));
    }

    #[test]
    fn notes_that_no_longer_begin_as_recorded_stop_recovery_and_nothing_is_truncated() {
        let (_dir, files, mut sc) = folder();
        commit_through(&files, &mut sc, BLOCK, 5, 2, Step::Journal).unwrap();
        std::fs::write(&files.notes, "# Edited by hand while the app was down\n- and more\n").unwrap();
        let err = recover(&files, &mut sc).unwrap_err();
        assert!(format!("{err:#}").contains("by hand"), "{err:#}");
        assert_eq!(std::fs::read_to_string(&files.notes).unwrap(), "# Edited by hand while the app was down\n- and more\n");
        assert!(files.journal().exists(), "the journal stays for the person to look at");
    }

    #[test]
    fn notes_that_grew_past_the_block_stop_recovery_and_nothing_is_truncated() {
        let (_dir, files, mut sc) = folder();
        commit_through(&files, &mut sc, BLOCK, 5, 2, Step::Append).unwrap();
        std::fs::OpenOptions::new().append(true).open(&files.notes).unwrap().write_all(b"- typed after the crash\n").unwrap();
        let grown = std::fs::read(&files.notes).unwrap();
        assert!(recover(&files, &mut sc).is_err());
        assert_eq!(std::fs::read(&files.notes).unwrap(), grown);
        assert!(files.journal().exists());
    }

    #[test]
    fn an_external_edit_is_kept_and_the_block_appended_after_it() {
        let (_dir, files, mut sc) = folder();
        std::fs::OpenOptions::new().append(true).open(&files.notes).unwrap().write_all(b"\nMy own note, typed in an editor.\n").unwrap();
        let edited = std::fs::read_to_string(&files.notes).unwrap();
        commit(&files, &mut sc, BLOCK, 5, 2).unwrap();
        assert_eq!(std::fs::read_to_string(&files.notes).unwrap(), format!("{edited}{BLOCK}"));
        assert_eq!(sc.notes.revision, 3, "the edit and the commit are one revision each");
    }

    #[test]
    fn a_block_is_the_clis_marker_and_text() {
        let at = chrono::Local.with_ymd_and_hms(2026, 9, 25, 10, 5, 0).unwrap();
        assert_eq!(block(at, "## A\n- b"), "\n<!-- 10:05:00 -->\n## A\n- b\n");
    }

    #[test]
    fn polish_replaces_the_notes_atomically_after_a_backup() {
        let (_dir, files, mut sc) = folder();
        let old = std::fs::read(&files.notes).unwrap();
        let at = chrono::Local.with_ymd_and_hms(2026, 9, 25, 10, 15, 0).unwrap();
        let backup = replace(&files, &mut sc, "# Title\n\nA summary.\n\n", &sha256_hex(&old), at).unwrap();
        assert_eq!(backup, files.state_dir().join("lecture_notes_20260925_101500.md"));
        assert_eq!(std::fs::read(&backup).unwrap(), old);
        assert_eq!(std::fs::read_to_string(&files.notes).unwrap(), "# Title\n\nA summary.\n");
        assert_eq!(sc.notes.revision, 2);
        assert_eq!(Sidecar::load(&files.sidecar()).unwrap().unwrap(), sc);
        let again = replace(&files, &mut sc, "# Title\n\nShorter.\n", &sha256_hex(&std::fs::read(&files.notes).unwrap()), at).unwrap();
        assert_eq!(again, files.state_dir().join("lecture_notes_20260925_101500_2.md"), "a backup never overwrites another");
    }

    #[test]
    fn a_polish_over_notes_edited_meanwhile_is_not_written() {
        let (_dir, files, mut sc) = folder();
        let polished_from = sha256_hex(&std::fs::read(&files.notes).unwrap());
        std::fs::OpenOptions::new().append(true).open(&files.notes).unwrap().write_all(b"- typed while the polish ran\n").unwrap();
        let edited = std::fs::read(&files.notes).unwrap();
        let err = replace(&files, &mut sc, "# Polished\n", &polished_from, chrono::Local::now()).unwrap_err();
        assert!(format!("{err:#}").contains("changed while"), "{err:#}");
        assert_eq!(std::fs::read(&files.notes).unwrap(), edited);
        assert_eq!(std::fs::read_dir(files.state_dir()).unwrap().filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().ends_with(".md")).count(), 0, "no backup either");
    }

    /// Final review, Important 1: a tail shorter than the block that is not its start was typed by
    /// hand after the crash; recovery must stop, never delete it.
    #[test]
    fn a_short_tail_that_is_not_the_blocks_start_stops_recovery_and_is_kept() {
        let (_dir, files, mut sc) = folder();
        commit_through(&files, &mut sc, BLOCK, 5, 2, Step::Journal).unwrap();
        std::fs::OpenOptions::new().append(true).open(&files.notes).unwrap().write_all(b"- my own note\n").unwrap();
        let typed = std::fs::read(&files.notes).unwrap();
        let mut sc = Sidecar::load(&files.sidecar()).unwrap().unwrap();
        assert!(recover(&files, &mut sc).is_err(), "a tail that is not the block's start is not a torn append");
        assert_eq!(std::fs::read(&files.notes).unwrap(), typed, "the hand-typed note is kept");
        assert!(files.journal().exists());
    }

    /// Final review, Important 2: a commit that failed mid-append during a session leaves its journal
    /// and a fragment; the next commit repairs them first instead of appending after the fragment.
    #[test]
    fn a_commit_after_a_failed_one_in_the_same_session_repairs_it_first() {
        let (_dir, files, mut sc) = folder();
        let before = std::fs::read_to_string(&files.notes).unwrap();
        commit_through(&files, &mut sc, BLOCK, 5, 2, Step::Journal).unwrap();
        std::fs::OpenOptions::new().append(true).open(&files.notes).unwrap().write_all(&BLOCK.as_bytes()[..20]).unwrap(); // the append failed here
        let next = "\n<!-- 10:10:00 -->\n## Momentum\n- Averages past gradients.\n";
        commit(&files, &mut sc, next, 5, 2).unwrap(); // same process, same in-memory sidecar
        assert_eq!(std::fs::read_to_string(&files.notes).unwrap(), format!("{before}{next}"), "no fragment before the new block");
        assert!(!files.journal().exists());
    }

    /// When the failed commit had in fact completed, its batch is already noted: the caller's batch is
    /// stale and the commit refuses, so nothing is noted twice.
    #[test]
    fn a_commit_after_one_that_completed_unrecorded_refuses_its_stale_batch() {
        let (_dir, files, mut sc) = folder();
        let before = std::fs::read_to_string(&files.notes).unwrap();
        commit_through(&files, &mut sc, BLOCK, 5, 2, Step::Append).unwrap(); // saving the cursors failed
        let mut stale = sc.clone();
        stale.notes.segment_cursor = 0;
        stale.notes.slide_index = 0;
        stale.notes.len = before.len() as u64;
        stale.notes.sha256 = sha256_hex(before.as_bytes());
        let err = commit(&files, &mut stale, "\n<!-- 10:10:00 -->\n- again\n", 5, 2).unwrap_err();
        assert!(format!("{err:#}").contains("try again"), "{err:#}");
        assert_eq!(std::fs::read_to_string(&files.notes).unwrap(), format!("{before}{BLOCK}"), "the block is there once");
        assert_eq!((stale.notes.segment_cursor, stale.notes.slide_index), (5, 2), "the cursors now say so");
    }
}
