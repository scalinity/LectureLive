//! The notes' Markdown as the notes pane draws it (M7 plan §H Notes): pulldown-cmark's events turned
//! into a small owned vocabulary of blocks — headings, prose, list items, code lines, table rows,
//! slide embeds, rules — each one text with its style changes by byte. Nothing here knows a width:
//! the pane wraps a block when it draws it, so a document is parsed once, when the canonical notes
//! change, and never per frame.
//!
//! The notes are split first at LectureLive's own `<!-- HH:MM:SS -->` markers into chunks, each
//! drawn under its time. Only an exact marker line splits; any other HTML is shown as the literal
//! text it is. Tables are the one extension enabled: TeX such as `\( \sigma / \sqrt{n} \)` stays
//! literal text, backslashes included, and a link is its text alone — never a terminal hyperlink.

use std::ops::Range;

use pulldown_cmark::{Event as Md, HeadingLevel, Options, Parser, Tag, TagEnd};

use crate::plain;

#[cfg(test)]
thread_local! {
    /// Bytes of Markdown parsed on this thread: the tests' proof that a draw never parses.
    pub(crate) static PARSED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How a run of text is set: bold, italic, code (dim), or any mix of them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Mark {
    pub(crate) bold: bool,
    pub(crate) italic: bool,
    pub(crate) code: bool,
}

/// What opens a list item's first row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Lead {
    Bullet,
    Number(u64),
}

/// What a block is, for the pane: how it is indented and set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// `#` to `######`, by level.
    Heading(u8),
    /// Prose: a paragraph, a list item's later paragraph, raw HTML as literal text.
    Para,
    /// A list item's first paragraph, after its bullet or number.
    Item(Lead),
    /// One line of a code block, its leading spaces counted apart so they survive wrapping.
    Code(u16),
    /// A table row: its cells' text, two spaces apart.
    Row { head: bool },
    /// An image — in LectureLive's notes, a slide: the text is its label, `dest` its file.
    Slide { dest: String },
    /// A thematic break.
    Rule,
}

/// One block: its kind, its text (already cleaned for display), where its styles change, and the
/// list and quote nesting it sits in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Block {
    pub(crate) kind: Kind,
    pub(crate) text: String,
    /// `(byte, mark)`, ascending: each mark holds from its byte to the next entry's.
    pub(crate) marks: Vec<(usize, Mark)>,
    /// List nesting: 0 outside a list.
    pub(crate) depth: u8,
    /// Blockquote nesting.
    pub(crate) quote: u8,
}

impl Block {
    /// The mark at byte `at` of the text.
    pub(crate) fn mark_at(&self, at: usize) -> Mark {
        self.marks.iter().take_while(|(b, _)| *b <= at).last().map_or(Mark::default(), |(_, m)| *m)
    }
}

/// A stretch of the notes under one marker: its time, where it lies in the document, its blocks.
/// The material before the first marker — the title — is a chunk with no time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Chunk {
    /// `HH:MM:SS`, from the marker.
    pub(crate) time: Option<String>,
    /// The bytes of the document this chunk was parsed from, after its marker line.
    pub(crate) source: Range<usize>,
    pub(crate) blocks: Vec<Block>,
}

/// The time an exact LectureLive marker line names: `<!-- HH:MM:SS -->`, nothing else on the line.
fn marker(line: &str) -> Option<&str> {
    let time = line.trim_end().strip_prefix("<!-- ")?.strip_suffix(" -->")?;
    let b = time.as_bytes();
    let digits = |r: Range<usize>| b[r].iter().all(u8::is_ascii_digit);
    (b.len() == 8 && b[2] == b':' && b[5] == b':' && digits(0..2) && digits(3..5) && digits(6..8)).then_some(time)
}

/// `doc[from..]` split at its marker lines: the stretch before the first (no time), then one per
/// marker, each from after its marker line to the next one.
fn pieces(doc: &str, from: usize) -> Vec<(Option<String>, Range<usize>)> {
    let mut out = vec![(None, from..from)];
    let mut at = from;
    for line in doc[from..].split_inclusive('\n') {
        let end = at + line.len();
        match marker(line) {
            Some(time) => {
                out.last_mut().expect("one piece at least").1.end = at;
                out.push((Some(time.to_string()), end..end));
            }
            None => out.last_mut().expect("one piece at least").1.end = end,
        }
        at = end;
    }
    out
}

/// The whole document as chunks: what a hydration does, once.
pub(crate) fn chunks(doc: &str) -> Vec<Chunk> {
    let mut out = Vec::new();
    extend(&mut out, doc, 0);
    out
}

/// Chunks for what was appended to `doc` at `from`, as a commit appends a block: only the new text
/// is parsed. Material before the new block's first marker continues the last chunk, which is then
/// parsed again (whitespace alone only moves its end). The result is the same as [`chunks`] of the
/// whole document.
pub(crate) fn extend(chunks: &mut Vec<Chunk>, doc: &str, from: usize) {
    for (i, (time, range)) in pieces(doc, from).into_iter().enumerate() {
        if i == 0 {
            let text = &doc[range.clone()];
            match chunks.last_mut() {
                Some(last) => {
                    last.source.end = range.end;
                    if !text.trim().is_empty() {
                        last.blocks = parse(&doc[last.source.clone()]);
                    }
                }
                None if !text.trim().is_empty() => chunks.push(Chunk { time: None, blocks: parse(text), source: range }),
                None => {}
            }
            continue;
        }
        chunks.push(Chunk { time, blocks: parse(&doc[range.clone()]), source: range });
    }
}

/// Markdown into blocks: headings, paragraphs, nested lists, code, quotes, tables (the one extension
/// on), images; links become their text and raw HTML its literal text. Every piece of text is
/// cleaned again as it enters, since an entity can decode into a control character.
pub(crate) fn parse(src: &str) -> Vec<Block> {
    #[cfg(test)]
    PARSED.with(|n| n.set(n.get() + src.len()));
    let mut b = Builder::default();
    for (event, range) in Parser::new_ext(src, Options::ENABLE_TABLES).into_offset_iter() {
        b.event(event, range, src);
    }
    b.flush();
    b.blocks
}

#[derive(Default)]
struct Builder {
    blocks: Vec<Block>,
    cur: Option<Block>,
    bold: u32,
    italic: u32,
    quote: u8,
    /// Each open list: the next number of an ordered one, `None` for bullets.
    lists: Vec<Option<u64>>,
    /// The lead the next text of a list item opens with, until it is used.
    lead: Option<Lead>,
    /// A code block's text, while inside one.
    code: Option<String>,
    /// An image's destination and its alt text, while inside one.
    image: Option<(String, String)>,
    /// The cells of the table row being built.
    cell: usize,
}

impl Builder {
    fn depth(&self) -> u8 {
        self.lists.len().min(u8::MAX as usize) as u8
    }

    fn start(&mut self, kind: Kind) {
        self.flush();
        self.cur = Some(Block { kind, text: String::new(), marks: Vec::new(), depth: self.depth(), quote: self.quote });
    }

    /// The block text is going into: the one open, else a paragraph — a list item's first when its
    /// lead is still waiting (a tight list has no paragraph events).
    fn open(&mut self) -> &mut Block {
        if self.cur.is_none() {
            let kind = self.lead.take().map_or(Kind::Para, Kind::Item);
            self.start(kind);
        }
        self.cur.as_mut().expect("opened above")
    }

    fn flush(&mut self) {
        if let Some(mut b) = self.cur.take() {
            let kept = b.text.trim_end().len();
            b.text.truncate(kept);
            b.marks.retain(|(at, _)| *at < kept.max(1));
            if !b.text.is_empty() || matches!(b.kind, Kind::Heading(_) | Kind::Row { .. }) {
                self.blocks.push(b);
            }
        }
    }

    fn push(&mut self, text: &str, code: bool) {
        let mark = Mark { bold: self.bold > 0, italic: self.italic > 0, code };
        let cur = self.open();
        let text = if cur.text.is_empty() { text.trim_start() } else { text };
        if text.is_empty() {
            return;
        }
        if cur.marks.last().is_none_or(|(_, m)| *m != mark) {
            cur.marks.push((cur.text.len(), mark));
        }
        cur.text.push_str(&plain::clean(text));
    }

    fn event(&mut self, event: Md, range: Range<usize>, src: &str) {
        match event {
            Md::Start(tag) => self.open_tag(tag),
            Md::End(tag) => self.close_tag(tag),
            Md::Text(t) => {
                if let Some(code) = self.code.as_mut() {
                    code.push_str(&t);
                } else if let Some((_, alt)) = self.image.as_mut() {
                    alt.push_str(&t);
                } else {
                    // A backslash escape drops its backslash, and `\(`, `\[`, `\\` are TeX here, not
                    // escapes: where this text begins just after an escaping backslash, it is put back.
                    let escaped = range.start > 0 && src.as_bytes()[range.start - 1] == b'\\' && t.starts_with(|c: char| c.is_ascii_punctuation());
                    if escaped {
                        self.push("\\", false);
                    }
                    self.push(&t, false);
                }
            }
            Md::Code(t) => self.push(&t, true),
            Md::InlineMath(t) | Md::DisplayMath(t) => self.push(&t, false),
            Md::Html(t) | Md::InlineHtml(t) => self.push(&t, false),
            Md::SoftBreak => {
                if let Some((_, alt)) = self.image.as_mut() {
                    alt.push(' ');
                } else if self.cur.as_ref().is_some_and(|c| !c.text.is_empty()) {
                    self.push(" ", false);
                }
            }
            Md::HardBreak => self.push("\n", false),
            Md::Rule => {
                self.start(Kind::Rule);
                let b = self.cur.take().expect("started");
                self.blocks.push(b);
            }
            Md::TaskListMarker(done) => self.push(if done { "[x] " } else { "[ ] " }, false),
            Md::FootnoteReference(_) => {}
        }
    }

    fn open_tag(&mut self, tag: Tag) {
        match tag {
            Tag::Paragraph => self.flush(),
            Tag::Heading { level, .. } => {
                self.lead = None;
                self.start(Kind::Heading(heading(level)));
            }
            Tag::BlockQuote(_) => {
                self.flush();
                self.quote = self.quote.saturating_add(1);
            }
            Tag::CodeBlock(_) => {
                self.flush();
                self.lead = None;
                self.code = Some(String::new());
            }
            Tag::HtmlBlock => self.start(Kind::Para),
            Tag::List(first) => {
                self.flush();
                self.lead = None;
                self.lists.push(first);
            }
            Tag::Item => {
                self.flush();
                self.lead = Some(match self.lists.last_mut() {
                    Some(Some(n)) => {
                        *n += 1;
                        Lead::Number(*n - 1)
                    }
                    _ => Lead::Bullet,
                });
            }
            Tag::Emphasis => self.italic += 1,
            Tag::Strong => self.bold += 1,
            Tag::Image { dest_url, .. } => self.image = Some((dest_url.to_string(), String::new())),
            Tag::Table(_) => self.flush(),
            Tag::TableHead => self.row(true),
            Tag::TableRow => self.row(false),
            Tag::TableCell => {
                if self.cell > 0 {
                    let cur = self.open();
                    cur.marks.push((cur.text.len(), Mark::default()));
                    cur.text.push_str("  ");
                }
                self.cell += 1;
            }
            _ => {}
        }
    }

    fn row(&mut self, head: bool) {
        self.start(Kind::Row { head });
        self.cell = 0;
    }

    fn close_tag(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::HtmlBlock | TagEnd::TableHead | TagEnd::TableRow | TagEnd::Table => self.flush(),
            TagEnd::BlockQuote(_) => {
                self.flush();
                self.quote = self.quote.saturating_sub(1);
            }
            TagEnd::CodeBlock => {
                let code = self.code.take().unwrap_or_default();
                for line in code.strip_suffix('\n').unwrap_or(&code).split('\n') {
                    let rest = line.trim_start_matches([' ', '\t']);
                    let lead = &line[..line.len() - rest.len()];
                    let indent = lead.chars().map(|c| if c == '\t' { 4 } else { 1 }).sum::<usize>().min(u16::MAX as usize) as u16;
                    let text = plain::clean(rest.trim_end());
                    self.blocks.push(Block { kind: Kind::Code(indent), marks: vec![(0, Mark { code: true, ..Mark::default() })], text, depth: self.depth(), quote: self.quote });
                }
            }
            TagEnd::List(_) => {
                self.flush();
                self.lists.pop();
                self.lead = None;
            }
            TagEnd::Item => {
                self.flush();
                self.lead = None;
            }
            TagEnd::Emphasis => self.italic = self.italic.saturating_sub(1),
            TagEnd::Strong => self.bold = self.bold.saturating_sub(1),
            TagEnd::Image => {
                // A slide embed is a block of its own, even in the middle of a paragraph: what came
                // before it ends there, and what follows starts a new paragraph.
                let (dest, alt) = self.image.take().unwrap_or_default();
                self.flush();
                self.lead = None;
                self.blocks.push(Block { kind: Kind::Slide { dest: plain::clean(&dest) }, text: plain::clean(alt.trim()), marks: Vec::new(), depth: self.depth(), quote: self.quote });
            }
            _ => {}
        }
    }
}

fn heading(level: HeadingLevel) -> u8 {
    level as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The canonical notes of a real-shaped lecture (core's own fixture): a title, three marked
    /// chunks, `##` and `###` headings, bullets, bold, slide embeds and TeX.
    const LECTURE: &str = include_str!("../../../core/tests/fixtures/notes/lecture/fixture_lecture.md");

    /// A focused document for what the fixture does not hold.
    const RICH: &str = "# Title\n\n<!-- 10:41:52 -->\n## Sampling\n\
Plain, **bold**, *italic*, ***both*** and `code`; the [link text](https://example.org/secret) only.\n\n\
- one\n  - two\n    - three with \\( \\sigma / \\sqrt{n} \\)\n- back to one\n\n\
1. first\n2. second\n\n\
```\nfn main() {\n    println!(\"hi\");\n}\n```\n\n\
> quoted *words*\n\n\
| Method | Rate |\n|---|---|\n| SGD | 0.1 |\n\n\
<div>raw <b>html</b></div>\n\n\
<!-- not a marker -->\n\n\
![Slide 18](slides/slide_18_104152.png)\nThe caption after it.\n\n\
機械学習 🎓 cafe\u{301}\n";

    fn texts(blocks: &[Block]) -> Vec<(&Kind, &str)> {
        blocks.iter().map(|b| (&b.kind, b.text.as_str())).collect()
    }

    #[test]
    fn only_exact_markers_split_the_notes() {
        assert_eq!(marker("<!-- 10:41:52 -->"), Some("10:41:52"));
        assert_eq!(marker("<!-- 10:41:52 -->  \n"), Some("10:41:52"));
        for not in ["<!-- 10:41 -->", "<!--10:41:52-->", " <!-- 10:41:52 -->", "<!-- 10:41:52 --> x", "<!-- aa:bb:cc -->", "<!-- not a marker -->"] {
            assert_eq!(marker(not), None, "{not:?}");
        }
        let c = chunks(RICH);
        assert_eq!(c.iter().map(|c| c.time.as_deref()).collect::<Vec<_>>(), vec![None, Some("10:41:52")]);
        assert_eq!(texts(&c[0].blocks), vec![(&Kind::Heading(1), "Title")]);
    }

    #[test]
    fn the_lecture_fixture_splits_into_its_timed_chunks() {
        let c = chunks(LECTURE);
        assert_eq!(c.iter().map(|c| c.time.as_deref()).collect::<Vec<_>>(), vec![None, Some("10:05:12"), Some("10:18:30"), Some("10:32:45")]);
        assert_eq!(c[0].blocks[0].kind, Kind::Heading(1));
        assert!(c[0].blocks[0].text.starts_with("Machine Learning — Week 03"));
        let first = &c[1].blocks;
        assert_eq!((&first[0].kind, first[0].text.as_str()), (&Kind::Heading(2), "Why optimisation matters"));
        assert!(first.iter().any(|b| b.kind == Kind::Heading(3) && b.text == "The gradient"));
        assert!(first.iter().any(|b| b.kind == Kind::Slide { dest: "slides/slide_01_100512.png".into() } && b.text == "Slide 1"));
        // TeX stays literal, backslashes and all
        assert!(first.iter().any(|b| b.text.contains(r"parameters \(\theta\) that make a loss \(L(\theta)\) small")), "{:?}", texts(first));
        // bold inside a bullet
        let exam = c.iter().flat_map(|c| &c.blocks).find(|b| b.text.starts_with("Examinable:")).unwrap();
        assert_eq!(exam.kind, Kind::Item(Lead::Bullet));
        assert!(exam.mark_at(0).bold && !exam.mark_at(exam.text.len() - 1).bold);
        for c in &c {
            assert_eq!(&LECTURE[c.source.clone()].trim().is_empty(), &c.blocks.is_empty());
        }
    }

    #[test]
    fn the_vocabulary_holds_what_the_pane_needs() {
        let c = chunks(RICH);
        let b = &c[1].blocks;
        let find = |t: &str| b.iter().find(|x| x.text.starts_with(t)).unwrap_or_else(|| panic!("{t:?} in {:?}", texts(b)));
        assert_eq!(b[0].kind, Kind::Heading(2));
        let prose = find("Plain,");
        assert_eq!(prose.text, "Plain, bold, italic, both and code; the link text only.");
        let at = |w: &str| prose.mark_at(prose.text.find(w).unwrap());
        assert_eq!(at("bold"), Mark { bold: true, ..Mark::default() });
        assert_eq!(at("italic"), Mark { italic: true, ..Mark::default() });
        assert_eq!(at("both"), Mark { bold: true, italic: true, code: false });
        assert_eq!(at("code"), Mark { code: true, ..Mark::default() });
        assert_eq!(at("link text"), Mark::default());
        assert!(!prose.text.contains("example.org"), "a link is its text, never its target");
        assert_eq!((find("one").kind.clone(), find("one").depth), (Kind::Item(Lead::Bullet), 1));
        assert_eq!(find("two").depth, 2);
        let three = find("three");
        assert_eq!((three.depth, three.text.as_str()), (3, r"three with \( \sigma / \sqrt{n} \)"), "TeX is literal");
        assert_eq!(find("back to one").depth, 1);
        assert_eq!(find("first").kind, Kind::Item(Lead::Number(1)));
        assert_eq!(find("second").kind, Kind::Item(Lead::Number(2)));
        let code: Vec<(&Kind, &str)> = b.iter().filter(|x| matches!(x.kind, Kind::Code(_))).map(|x| (&x.kind, x.text.as_str())).collect();
        assert_eq!(code, vec![(&Kind::Code(0), "fn main() {"), (&Kind::Code(4), "println!(\"hi\");"), (&Kind::Code(0), "}")]);
        let quote = find("quoted");
        assert_eq!((quote.quote, quote.mark_at(7).italic), (1, true));
        assert_eq!((&find("Method").kind, find("Method").text.as_str()), (&Kind::Row { head: true }, "Method  Rate"));
        assert_eq!((&find("SGD").kind, find("SGD").text.as_str()), (&Kind::Row { head: false }, "SGD  0.1"));
        assert_eq!(find("<div>").text, "<div>raw <b>html</b></div>", "raw HTML is literal text");
        assert_eq!(find("<!-- not").text, "<!-- not a marker -->", "a comment that is not a marker is literal too");
        let slide = b.iter().position(|x| matches!(x.kind, Kind::Slide { .. })).unwrap();
        assert_eq!((&b[slide].kind, b[slide].text.as_str()), (&Kind::Slide { dest: "slides/slide_18_104152.png".into() }, "Slide 18"));
        assert_eq!(b[slide + 1].text, "The caption after it.", "the rest of the paragraph after the slide is its own");
        assert_eq!(b.last().unwrap().text, "機械学習 🎓 cafe\u{301}", "Unicode as it is");
    }

    #[test]
    fn escapes_that_are_tex_keep_their_backslashes() {
        let one = |s: &str| parse(s).remove(0).text;
        assert_eq!(one(r"\( \sigma / \sqrt{n} \)"), r"\( \sigma / \sqrt{n} \)");
        assert_eq!(one(r"\[ x \] and a \\ break"), r"\[ x \] and a \\ break");
        assert_eq!(one(r"a\\(b"), r"a\\(b");
        assert_eq!(one(r"\_ and \* stay"), r"\_ and \* stay");
    }

    /// Text cannot carry a terminal control past the parser: an entity decodes to one, and is
    /// cleaned away as it enters.
    #[test]
    fn decoded_entities_are_cleaned() {
        let b = parse("red &#27;[31m text &#x7; and `code\u{1b}]0;x\u{7}`");
        assert!(!b[0].text.chars().any(|c| c.is_control()), "{:?}", b[0].text);
    }

    /// A commit appends: parsing only its new text gives what parsing the whole document gives.
    #[test]
    fn extending_by_a_commit_equals_parsing_the_whole() {
        let mut held = chunks("# Title\n");
        let mut doc = String::from("# Title\n");
        for block in ["\n<!-- 10:00:00 -->\n## A\n- a\n", "\n<!-- 10:05:00 -->\n## B\n- b\n", "a tail with no marker, continuing B\n", "\n<!-- 10:09:00 -->\n## C\n"] {
            let from = doc.len();
            doc.push_str(block);
            PARSED.with(|n| n.set(0));
            extend(&mut held, &doc, from);
            let parsed = PARSED.with(|n| n.get());
            assert_eq!(held, chunks(&doc), "after {block:?}");
            if block.starts_with('\n') {
                assert!(parsed <= block.len(), "only the new block was parsed: {parsed} of {}", doc.len());
            }
        }
        // the fixture, commit by commit
        let marks: Vec<usize> = LECTURE.match_indices("\n<!-- ").map(|(i, _)| i).collect();
        let mut held = chunks(&LECTURE[..marks[0]]);
        for (k, &at) in marks.iter().enumerate() {
            let end = marks.get(k + 1).copied().unwrap_or(LECTURE.len());
            extend(&mut held, &LECTURE[..end], at);
        }
        assert_eq!(held, chunks(LECTURE));
    }

    #[test]
    fn nothing_breaks_on_odd_input() {
        for s in ["", "\n\n", "<!-- 10:00:00 -->", "<!-- 10:00:00 -->\n", "- \n-\n  -", "```\nunclosed", "| a |\n|---|", "![](x)", "> > >", "***", "\\", "`"] {
            let _ = chunks(s);
        }
        assert!(chunks("").is_empty());
        assert_eq!(chunks("<!-- 10:00:00 -->\n").len(), 1, "a marker with nothing under it yet is a chunk");
    }
}
