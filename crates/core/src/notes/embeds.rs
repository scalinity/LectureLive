//! What the model wrote, checked before it is committed (spec §6.2, §6.3).
use std::sync::LazyLock;

use regex::Regex;

/// A slide embed as the CLI writes it: `![Slide N](path)`.
pub static EMBED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"!\[Slide \d+\]\([^)]+\)").expect("a valid pattern"));

pub const NOT_PLACED: &str = "### Slides not placed";

/// The Python CLI's `clean_output`: a wrapping code fence removed, and with `strip_title` every leading `# ` title line.
pub fn clean_output(text: &str, strip_title: bool) -> String {
    // Lines are rejoined with LF below, so every line break is made one first, a lone CR included.
    let mut text = text.replace("\r\n", "\n").replace('\r', "\n").trim().to_string();
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
/// Which lines are code, as a CommonMark parser reads them: what it keeps literal rather than reading as Markdown. That
/// is every line of a code block (fenced, with its fences, or indented) or an HTML block, in whatever list item or quote
/// holds it, and a line that starts inside a code span or inline HTML. An unclosed fence runs to the end of its
/// container, so what is uncertain counts as code, which keeps it. The one reading the snapshot's repair, the reference
/// count and the embed edits share.
pub fn code_lines(lines: &[&str]) -> Vec<bool> {
    use pulldown_cmark::{Event, Parser, Tag};
    // A CR left inside a line is a line break to the parser; as an LF it stays one byte, where it was.
    let text = lines.iter().map(|l| l.replace('\r', "\n")).collect::<Vec<_>>().join("\n");
    let (mut blocks, mut spans) = (Vec::new(), Vec::new());
    for (event, span) in Parser::new(&text).into_offset_iter() {
        match event {
            Event::Start(Tag::CodeBlock(_) | Tag::HtmlBlock) => blocks.push(span),
            Event::Code(_) | Event::InlineHtml(_) => spans.push(span),
            _ => {}
        }
    }
    let mut start = 0;
    lines
        .iter()
        .map(|line| {
            let end = start + line.len();
            // A block covers whole lines; a code span or inline HTML makes a line code only where the line starts in it.
            let first = end - line.trim_start().len();
            let code = blocks.iter().any(|b| b.start <= end && start < b.end) || spans.iter().any(|s| s.contains(&first));
            start = end + 1;
            code
        })
        .collect()
}

pub fn repair(text: &str, expected: &[String]) -> Repaired {
    let mut placed = vec![false; expected.len()];
    let (mut out, mut removed) = (Vec::new(), 0);
    // The lines are joined with LF again, so a lone CR is written as the line break it is.
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
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
        // M03: a fence four spaces in from where its block opened is code; a block opened inside a list closes there.
        assert_eq!(code_lines(&["```", "    ```", "![Slide 1](slides/slide_01_100000.png)", "```", "out"]), [true, true, true, true, false]);
        assert_eq!(code_lines(&["- item", "    ```", "    code", "    ```", "after"]), [false, true, true, true, false]);
        // M05: closing is measured from the block's container, not the opener: a top-level fence closes within three
        // columns of the margin, however far in it opened; inside a list item, from the item's content.
        for opener in [" ```markdown", "  ```markdown", "   ```markdown"] {
            assert_eq!(code_lines(&[opener, "    ```", "![Slide 1](slides/slide_01_100000.png)", "```", "out"]), [true, true, true, true, false], "{opener:?}");
        }
        assert_eq!(code_lines(&["1. item", "   ```", "   code", "   ```", "after"]), [false, true, true, true, false]);
        assert_eq!(code_lines(&["- a", "  - b", "      ```", "        ```", "      ```", "after"]), [false, false, true, true, true, false]);
    }

    /// M06: the first fence ends the inner list, so the second block is the outer item's and its six-space line is code.
    const AFTER_A_NESTED_LIST: &str = "- outer\n  - inner\n\n  ```text\n  first example\n  ```\n\n    ```markdown\n      ```\n    ![Slide 1](slides/slide_01_100000.png)\n  ```\n";
    /// M07: a tab after a list marker runs to the next four-column stop, so the item's fence closes on the tabbed line.
    const AFTER_A_TABBED_ITEM: &str = "-\titem\n\t```rust\n\tlet x = 1;\n\t```\n\n```markdown\n![Slide 1](slides/slide_01_100000.png)\n```\n";

    #[test]
    fn code_follows_list_items_tab_stops_and_indented_blocks() {
        let lines = |t: &'static str| t.lines().collect::<Vec<_>>();
        assert_eq!(code_lines(&lines(AFTER_A_NESTED_LIST)), [false, false, false, true, true, true, false, true, true, true, true]);
        assert_eq!(code_lines(&lines(AFTER_A_TABBED_ITEM)), [false, true, true, true, false, true, true, true]);
        for marker in ["-\titem", "1.\titem"] {
            assert_eq!(code_lines(&[marker, "\t```rust", "\t```", "", "![Slide 1](slides/slide_01_100000.png)"]), [false, true, true, false, false], "{marker:?}");
        }
        // M08: fence-like lines indented four columns are an indented code block, which ends at the next line at the margin.
        let indented = ["## Example", "", "    ```rust", "    let x = 1;", "    ```", "", "## Slides", "![Slide 1](slides/slide_01_100000.png)"];
        assert_eq!(code_lines(&indented), [false, false, true, true, true, false, false, false]);
    }

    /// M09: four-space backticks cannot open a fence after a paragraph, so they delimit a code span across lines.
    const IN_A_CODE_SPAN: &str = "Paragraph\n    ```markdown\n![Slide 1](slides/slide_01_100000.png)\n    ```\n";
    /// M09: what an HTML block holds is not read as Markdown.
    const IN_AN_HTML_BLOCK: &str = "<pre>\n```markdown\n![Slide 1](slides/slide_01_100000.png)\n```\n</pre>\n";
    /// M10: a lone CR then a CRLF is two line breaks, so the indented lines after the blank line are code.
    const AFTER_A_LONE_CR: &str = "Paragraph\r\r\n    ~~~markdown\n    ![Slide 1](slides/slide_01_100000.png)\n    ~~~\n";

    #[test]
    fn code_spans_html_and_lone_carriage_returns_are_read_as_the_parser_reads_them() {
        let lines = |t: &'static str| t.lines().collect::<Vec<_>>();
        assert_eq!(code_lines(&lines(IN_A_CODE_SPAN)), [false, true, true, true]);
        assert_eq!(code_lines(&lines(IN_AN_HTML_BLOCK)), [true; 5]);
        assert_eq!(code_lines(&lines(AFTER_A_LONE_CR)), [false, true, true, true]);
        // A line that only holds a code span somewhere is still text: its inline copies are the repair's to remove.
        assert_eq!(code_lines(&["See `x` and ![Slide 2](slides/slide_02_100000.png)"]), [false]);
    }

    #[test]
    fn a_literal_embed_in_a_shifted_fence_is_left_alone_by_the_repair() {
        // M05-M10 through repair: the line is code, so it is no embed of the batch to place or drop.
        for text in [" ```markdown\n    ```\n![Slide 1](slides/slide_01_100000.png)\n```\n", AFTER_A_NESTED_LIST, AFTER_A_TABBED_ITEM, IN_A_CODE_SPAN, IN_AN_HTML_BLOCK, AFTER_A_LONE_CR] {
            assert_eq!(repair(text, &[]).text.lines().filter(|l| l.trim().starts_with("![Slide 1]")).count(), 1, "{text:?}");
        }
        // A literal copy is not a placement, so the slide is placed after it; and the blank line the lone CR made stays.
        for text in [IN_A_CODE_SPAN, IN_AN_HTML_BLOCK, AFTER_A_LONE_CR] {
            assert_eq!(repair(text, &[e(1)]).missing, [e(1)], "{text:?}");
        }
        assert!(repair(AFTER_A_LONE_CR, &[]).text.starts_with("Paragraph\n\n    ~~~markdown"));
        // M11: a snapshot's answer is unwrapped first, and the break survives that as well.
        let r = repair(&clean_output(&format!("```markdown\n{AFTER_A_LONE_CR}```"), true), &[]);
        assert_eq!(r.removed, 0);
        assert!(r.text.starts_with("Paragraph\n\n    ~~~markdown"), "{:?}", r.text);
    }

    #[test]
    fn a_placed_embed_after_an_indented_example_is_not_placed_again() {
        // M08 through repair.
        let text = format!("## Example\n\n    ```rust\n    let x = 1;\n    ```\n\n## Slides\n{}", e(1));
        let r = repair(&text, &[e(1)]);
        assert_eq!((r.text.as_str(), r.missing.len()), (text.as_str(), 0));
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
