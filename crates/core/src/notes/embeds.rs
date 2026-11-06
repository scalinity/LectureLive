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

/// The text's lines as CommonMark ends them, at an LF, a CRLF or a lone CR: where each starts, its text and its ending.
pub fn split_lines(text: &str) -> Vec<(usize, &str, &str)> {
    let b = text.as_bytes();
    let (mut out, mut start, mut i) = (Vec::new(), 0, 0);
    while i < b.len() {
        let end = match b[i] {
            b'\n' => i + 1,
            b'\r' if b.get(i + 1) == Some(&b'\n') => i + 2,
            b'\r' => i + 1,
            _ => {
                i += 1;
                continue;
            }
        };
        out.push((start, &text[start..i], &text[i..end]));
        (start, i) = (end, end);
    }
    if start < b.len() {
        out.push((start, &text[start..], ""));
    }
    out
}

/// Notes as a CommonMark parser reads them: their lines, which of them a code or HTML block holds, and where a slide
/// embed renders as an image. An embed spelled anywhere else (in code, in HTML, in a link's title, escaped) is literal
/// text, and the snapshot's repair, the reference count and the embed edits all leave it as it is.
pub struct Reading<'a> {
    pub lines: Vec<(usize, &'a str, &'a str)>,
    /// Whether a code block (fenced, with its fences, or indented) or an HTML block holds the line.
    pub literal: Vec<bool>,
    images: Vec<std::ops::Range<usize>>,
}

pub fn read(text: &str) -> Reading<'_> {
    use pulldown_cmark::{Event, Parser, Tag};
    let (mut blocks, mut images) = (Vec::new(), Vec::new());
    for (event, span) in Parser::new(text).into_offset_iter() {
        match event {
            Event::Start(Tag::CodeBlock(_) | Tag::HtmlBlock) => blocks.push(span),
            Event::Start(Tag::Image { .. }) => images.push(span),
            _ => {}
        }
    }
    let lines = split_lines(text);
    let literal = lines.iter().map(|&(start, line, _)| blocks.iter().any(|b| b.start <= start + line.len() && start < b.end)).collect();
    Reading { lines, literal, images }
}

impl Reading<'_> {
    /// The slide embeds on line `i` that render as images, as ranges of the line.
    pub fn embeds(&self, i: usize) -> Vec<std::ops::Range<usize>> {
        let (start, line, _) = self.lines[i];
        EMBED.find_iter(line).map(|m| m.range()).filter(|r| self.images.iter().any(|s| s.start == start + r.start && s.end == start + r.end)).collect()
    }

    /// The embed line `i` holds on its own, when it renders as an image.
    pub fn own_embed(&self, i: usize) -> Option<&str> {
        let line = self.lines[i].1;
        match self.embeds(i).as_slice() {
            [r] if line.trim() == &line[r.clone()] => Some(line.trim()),
            _ => None,
        }
    }
}

/// Spec §6.2: each expected embed exactly once, on its own line, as an image. Later and inline copies and embeds of
/// slides outside the batch are removed; what is literal text stays as written; expected embeds still absent are
/// appended under their own heading, without a description.
pub fn repair(text: &str, expected: &[String]) -> Repaired {
    let mut placed = vec![false; expected.len()];
    let (mut out, mut removed) = (Vec::new(), 0);
    let notes = read(text);
    for (i, &(_, line, _)) in notes.lines.iter().enumerate() {
        let found = notes.embeds(i);
        if found.is_empty() {
            out.push(line.to_string());
            continue;
        }
        if let Some(k) = notes.own_embed(i).and_then(|own| expected.iter().position(|e| e == own)) {
            if placed[k] {
                removed += 1;
            } else {
                placed[k] = true;
                out.push(line.to_string());
            }
            continue;
        }
        removed += found.len();
        let mut rest = line.to_string();
        for r in found.into_iter().rev() {
            rest.replace_range(r, "");
        }
        if !rest.trim().is_empty() {
            out.push(rest.trim_end().to_string());
        }
    }
    let missing: Vec<String> = expected.iter().zip(&placed).filter(|(_, p)| !**p).map(|(e, _)| e.clone()).collect();
    let body = out.join("\n");
    let section = format!("{NOT_PLACED}\n\n{}", missing.join("\n\n"));
    let text = match (missing.is_empty(), body.trim().is_empty()) {
        (true, _) => body,
        (false, true) => section,
        (false, false) => {
            let appended = format!("{}\n\n{section}", body.trim_end());
            if shown(&appended, &missing) { appended } else { before_open_block(&body, &section).filter(|t| shown(t, &missing)).unwrap_or(appended) }
        }
    };
    Repaired { text, removed, missing }
}

/// Whether every one of `embeds` stands on its own line as an image in `text`.
fn shown(text: &str, embeds: &[String]) -> bool {
    let notes = read(text);
    let own: Vec<&str> = (0..notes.lines.len()).filter_map(|i| notes.own_embed(i)).collect();
    embeds.iter().all(|e| own.contains(&e.as_str()))
}

/// `section` placed before the block that runs to the end of `body`: an answer cut off inside a fence or an HTML
/// comment would otherwise hold it as literal text.
fn before_open_block(body: &str, section: &str) -> Option<String> {
    let notes = read(body);
    let last = (0..notes.lines.len()).rev().find(|&i| !notes.lines[i].1.trim().is_empty())?;
    if !notes.literal[last] {
        return None;
    }
    let first = (0..=last).rev().take_while(|&i| notes.literal[i]).last()?;
    let (head, tail) = body.split_at(notes.lines[first].0);
    Some(if head.trim().is_empty() { format!("{section}\n\n{tail}") } else { format!("{}\n\n{section}\n\n{tail}", head.trim_end()) })
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

    /// Which lines hold one slide embed on their own, as an image.
    fn images(text: &str) -> Vec<bool> {
        let notes = read(text);
        (0..notes.lines.len()).map(|i| notes.own_embed(i).is_some()).collect()
    }

    #[test]
    fn a_fence_closes_only_on_its_own_kind_and_length() {
        // R14: a ``` line inside a ~~~~ block is code, not the block's end.
        let text = format!("~~~~markdown\n```\n{}\n~~~~\n{}\n````\n```\n{}\n````\n{}", e(1), e(2), e(3), e(4));
        assert_eq!(images(&text), [false, false, false, false, true, false, false, false, false, true]);
        assert_eq!(images(&format!("```rust\n{}\n``` not a close\n{}\n```", e(1), e(2))), [false; 5]);
        // M03: a fence four spaces in from where its block opened is code; a block opened inside a list closes there.
        assert_eq!(images(&format!("```\n    ```\n{}\n```\n{}", e(1), e(2))), [false, false, false, false, true]);
        assert_eq!(images(&format!("- item\n    ```\n    {}\n    ```\n{}", e(1), e(2))), [false, false, false, false, true]);
        // M05: closing is measured from the block's container, not the opener: a top-level fence closes within three
        // columns of the margin, however far in it opened; inside a list item, from the item's content.
        for opener in [" ```markdown", "  ```markdown", "   ```markdown"] {
            assert_eq!(images(&format!("{opener}\n    ```\n{}\n```\n{}", e(1), e(2))), [false, false, false, false, true], "{opener:?}");
        }
        assert_eq!(images(&format!("1. item\n   ```\n   {}\n   ```\n{}", e(1), e(2))), [false, false, false, false, true]);
        assert_eq!(images(&format!("- a\n  - b\n      ```\n        ```\n      {}\n      ```\n{}", e(1), e(2))), [false, false, false, false, false, false, true]);
    }

    /// M06: the first fence ends the inner list, so the second block is the outer item's and its six-space line is code.
    const AFTER_A_NESTED_LIST: &str = "- outer\n  - inner\n\n  ```text\n  first example\n  ```\n\n    ```markdown\n      ```\n    ![Slide 1](slides/slide_01_100000.png)\n  ```\n";
    /// M07: a tab after a list marker runs to the next four-column stop, so the item's fence closes on the tabbed line.
    const AFTER_A_TABBED_ITEM: &str = "-\titem\n\t```rust\n\tlet x = 1;\n\t```\n\n```markdown\n![Slide 1](slides/slide_01_100000.png)\n```\n";

    #[test]
    fn code_follows_list_items_tab_stops_and_indented_blocks() {
        // The literal embed stays text, and one written after the block is an image again.
        for text in [AFTER_A_NESTED_LIST, AFTER_A_TABBED_ITEM] {
            let seen = images(&format!("{text}\n{}", e(2)));
            assert_eq!((seen.iter().filter(|s| **s).count(), seen.last()), (1, Some(&true)), "{text:?}");
        }
        for marker in ["-\titem", "1.\titem"] {
            assert_eq!(images(&format!("{marker}\n\t```rust\n\t{}\n\t```\n\n{}", e(1), e(2))), [false, false, false, false, false, true], "{marker:?}");
        }
        // M08: fence-like lines indented four columns are an indented code block, which ends at the next line at the margin.
        let indented = format!("## Example\n\n    ```rust\n    {}\n    ```\n\n## Slides\n{}", e(1), e(2));
        assert_eq!(images(&indented), [false, false, false, false, false, false, false, true]);
    }

    /// M09: four-space backticks cannot open a fence after a paragraph, so they delimit a code span across lines.
    const IN_A_CODE_SPAN: &str = "Paragraph\n    ```markdown\n![Slide 1](slides/slide_01_100000.png)\n    ```\n";
    /// M09: what an HTML block holds is not read as Markdown.
    const IN_AN_HTML_BLOCK: &str = "<pre>\n```markdown\n![Slide 1](slides/slide_01_100000.png)\n```\n</pre>\n";
    /// M10: a lone CR then a CRLF is two line breaks, so the indented lines after the blank line are code.
    const AFTER_A_LONE_CR: &str = "Paragraph\r\r\n    ~~~markdown\n    ![Slide 1](slides/slide_01_100000.png)\n    ~~~\n";

    #[test]
    fn code_spans_html_and_lone_carriage_returns_are_read_as_the_parser_reads_them() {
        for text in [IN_A_CODE_SPAN, IN_AN_HTML_BLOCK, AFTER_A_LONE_CR] {
            assert!(!images(text).contains(&true), "{text:?}");
        }
        // M14: a lone CR ends a line, so an embed after one stands on its own line.
        assert_eq!(images(&format!("## Notes\r{}\r\n", e(1))), [false, true]);
        // An image beside a code span is still an image, one the repair takes out as an inline copy.
        let notes = read("See `x` and ![Slide 2](slides/slide_02_100000.png)");
        assert_eq!((notes.embeds(0).len(), notes.own_embed(0)), (1, None));
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
    fn only_what_renders_as_an_image_is_placed_or_taken_out() {
        // M12: an image after leading inline code is an inline copy, so it goes and the slide is placed once below it.
        let r = repair(&format!("## Weights\n`w` is the weight: {}", e(1)), &[e(1)]);
        assert_eq!((r.text, r.removed), (format!("## Weights\n`w` is the weight:\n\n{NOT_PLACED}\n\n{}", e(1)), 1));
        // M12: an embed spelled inside a code span is text, and stays as it is.
        let literal = format!("Write `{}` literally.", e(9));
        assert_eq!((repair(&literal, &[]).text, repair(&literal, &[]).removed), (literal.clone(), 0));
        // M13: an embed spelled in a link reference's title is no image either, so the slide is still placed.
        let titled = format!("[reference]: /reference \"\n{}\n\"\n\n[reference]", e(1));
        let r = repair(&titled, &[e(1)]);
        assert_eq!(r.missing, [e(1)]);
        assert!(r.text.starts_with(&titled), "{:?}", r.text);
    }

    #[test]
    fn a_missing_slide_is_placed_before_a_block_left_open() {
        // M15: an answer cut off inside a fence or an HTML comment would hold the appended slides as literal text.
        for open in ["~~~text\nexample", "<!-- unfinished\nexample"] {
            let r = repair(&format!("## Example\n{open}"), &[e(1)]);
            assert_eq!(r.text, format!("## Example\n\n{NOT_PLACED}\n\n{}\n\n{open}", e(1)), "{open:?}");
        }
        // A block that closes leaves the slides at the end, as before.
        let closed = "## Example\n~~~text\nexample\n~~~";
        assert_eq!(repair(closed, &[e(1)]).text, format!("{closed}\n\n{NOT_PLACED}\n\n{}", e(1)));
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
