//! What the model wrote, checked before it is committed (spec §6.2, §6.3).
use std::sync::LazyLock;

use regex::Regex;

/// A slide embed as the CLI writes it: `![Slide N](path)`.
pub static EMBED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"!\[Slide \d+\]\([^)]+\)").expect("a valid pattern"));

pub const NOT_PLACED: &str = "### Slides not placed";

/// The Python CLI's `clean_output`: a wrapping code fence removed, and with `strip_title` every leading `# ` title line.
pub fn clean_output(text: &str, strip_title: bool) -> String {
    let mut text = text.trim().to_string();
    if text.starts_with("```") {
        let mut lines: Vec<&str> = text.lines().skip(1).collect();
        if lines.last().is_some_and(|l| l.trim().starts_with("```")) {
            lines.pop();
        }
        text = lines.join("\n").trim().to_string();
    }
    while strip_title && text.starts_with("# ") {
        text = text.split_once('\n').map_or(String::new(), |(_, rest)| rest.trim_start().to_string());
    }
    text
}

/// A document's embeds in order of first appearance, once each.
pub fn embeds_in(doc: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for m in EMBED.find_iter(doc) {
        if !out.iter().any(|e| e == m.as_str()) {
            out.push(m.as_str().to_string());
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq)]
pub struct Repaired {
    pub text: String,
    /// Copies and unexpected embeds taken out.
    pub removed: usize,
    /// Expected embeds that were appended under `### Slides not placed`.
    pub missing: Vec<String>,
}

/// Spec §6.2: each expected embed exactly once, on its own line, outside code fences. Later and
/// inline copies and embeds of slides outside the batch are removed; what fences hold is code and
/// stays; expected embeds still absent are appended under their own heading, without a description.
/// Which lines are fenced code, the fences included, as CommonMark reads them: a fence is three or more backticks or
/// tildes, and only a fence of the same character, at least as long and with nothing after it, closes it. The one reading
/// the snapshot's repair, the reference count and the embed edits share.
pub fn code_lines(lines: &[&str]) -> Vec<bool> {
    let mut open: Option<(char, usize)> = None;
    lines
        .iter()
        .map(|line| {
            let t = line.trim();
            let fence = t.chars().next().filter(|c| matches!(c, '`' | '~')).map(|c| (c, t.chars().take_while(|x| *x == c).count())).filter(|(_, n)| *n >= 3);
            match (open, fence) {
                (None, Some(f)) => open = Some(f),
                (Some((c, n)), Some((d, m))) if c == d && m >= n && t[m..].trim().is_empty() => open = None,
                (None, None) => return false,
                _ => {}
            }
            true
        })
        .collect()
}

pub fn repair(text: &str, expected: &[String]) -> Repaired {
    let mut placed = vec![false; expected.len()];
    let (mut out, mut removed) = (Vec::new(), 0);
    let lines: Vec<&str> = text.lines().collect();
    let code = code_lines(&lines);
    for (line, code) in lines.iter().zip(code) {
        let t = line.trim();
        if code {
            out.push(line.to_string());
            continue;
        }
        if let Some(i) = expected.iter().position(|e| e == t) {
            if placed[i] {
                removed += 1;
            } else {
                placed[i] = true;
                out.push(line.to_string());
            }
            continue;
        }
        let found = EMBED.find_iter(line).count();
        if found == 0 {
            out.push(line.to_string());
            continue;
        }
        removed += found;
        let rest = EMBED.replace_all(line, "");
        if !rest.trim().is_empty() {
            out.push(rest.trim_end().to_string());
        }
    }
    let missing: Vec<String> = expected.iter().zip(&placed).filter(|(_, p)| !**p).map(|(e, _)| e.clone()).collect();
    let body = out.join("\n");
    let text = match (missing.is_empty(), body.trim().is_empty()) {
        (true, _) => body,
        (false, true) => format!("{NOT_PLACED}\n\n{}", missing.join("\n\n")),
        (false, false) => format!("{}\n\n{NOT_PLACED}\n\n{}", body.trim_end(), missing.join("\n\n")),
    };
    Repaired { text, removed, missing }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(n: u32) -> String {
        format!("![Slide {n}](slides/slide_{n:02}_100000.png)")
    }

    #[test]
    fn an_embed_on_its_own_line_once_is_kept_as_written() {
        let text = format!("## Momentum\n{}\n- A slide of the update rule.\n- More.", e(1));
        let r = repair(&text, &[e(1)]);
        assert_eq!((r.text.as_str(), r.removed, r.missing.len()), (text.as_str(), 0, 0));
    }

    #[test]
    fn later_and_inline_copies_are_removed_and_an_embed_only_inline_is_appended() {
        let text = format!("## A\n{one}\n- see {one} here\n{one}\n- also {two} inline", one = e(1), two = e(2));
        let r = repair(&text, &[e(1), e(2)]);
        assert_eq!(r.text, format!("## A\n{}\n- see  here\n- also  inline\n\n{NOT_PLACED}\n\n{}", e(1), e(2)));
        assert_eq!((r.removed, r.missing.clone()), (3, vec![e(2)]));
    }

    #[test]
    fn a_fence_closes_only_on_its_own_kind_and_length() {
        // R14: a ``` line inside a ~~~~ block is code, not the block's end.
        let lines = ["~~~~markdown", "```", "![Slide 1](slides/a.png)", "~~~~", "after", "````", "```", "````", "out"];
        assert_eq!(code_lines(&lines), [true, true, true, true, false, true, true, true, false]);
        assert_eq!(code_lines(&["```rust", "x", "``` not a close", "```"]), [true, true, true, true]);
    }

    #[test]
    fn embeds_inside_code_fences_do_not_count_and_are_left_alone() {
        let text = format!("## A\n```markdown\n{}\n```\n- b", e(1));
        let r = repair(&text, &[e(1)]);
        assert_eq!(r.text, format!("{text}\n\n{NOT_PLACED}\n\n{}", e(1)));
    }

    #[test]
    fn embeds_of_slides_not_in_the_batch_are_removed() {
        let r = repair(&format!("## A\n{}\n{}\n- b", e(1), e(9)), &[e(1)]);
        assert_eq!((r.text, r.removed), (format!("## A\n{}\n- b", e(1)), 1));
    }

    #[test]
    fn missing_embeds_go_under_their_own_heading_without_a_description() {
        let r = repair("## A\n- b\n\n", &[e(1), e(2)]);
        assert_eq!(r.text, format!("## A\n- b\n\n{NOT_PLACED}\n\n{}\n\n{}", e(1), e(2)));
        assert_eq!(repair("", &[e(1)]).text, format!("{NOT_PLACED}\n\n{}", e(1)));
    }

    #[test]
    fn clean_output_strips_a_wrapping_fence_and_titles_as_the_cli_does() {
        assert_eq!(clean_output("```markdown\n# Title\n## A\n- b\n```\n", true), "## A\n- b");
        assert_eq!(clean_output("# One\n# Two\n\n## A", true), "## A");
        assert_eq!(clean_output("# Title\n\nSummary.", false), "# Title\n\nSummary.");
        assert_eq!(clean_output("```\n- a", true), "- a", "an unclosed fence loses only its opening line");
        assert_eq!(clean_output("# Only a title", true), "");
    }

    #[test]
    fn the_embeds_of_a_document_in_order_once_each() {
        let doc = format!("# T\n{}\n- x\n{}\n{}", e(2), e(1), e(2));
        assert_eq!(embeds_in(&doc), vec![e(2), e(1)]);
    }
}
