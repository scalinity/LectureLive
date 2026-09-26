//! The reading panes' contents (M7 plan §H). Task 8's is the transcript: each closed segment under
//! its `HH:MM:SS` in a dim gutter, the text wrapped with a hanging indent; the open utterance below
//! it on the teal live edge, its settled words in ink and its tentative ones dim.
//!
//! Only the rows on screen are ever wrapped (plan §H): following, from the last segment upward;
//! scrolled, from the anchor downward. The work per frame is proportional to the visible rows, not
//! the lecture's length; there is no wrap cache and no per-word span. The reader's place is a
//! semantic anchor — a segment id and the position in its text where the top row begins — so a
//! rewrap at any width finds the same words, and nothing about the transcript is copied here.

use std::borrow::Cow;
use std::ops::Range;

use lecturelive_core::session::segments::SegmentSource;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::state::{Line, View};

/// The time gutter: `HH:MM:SS`.
const TIME: usize = 8;
/// Where the text begins: the gutter, then two cells.
const LANE: usize = TIME + 2;
/// Rows one wheel notch moves: a reading step, as a pager's wheel does, never a page.
pub(crate) const WHEEL: usize = 3;
/// A recovered segment's mark, under its time.
const RECOVERED: &str = "recovered";
const DIM: Style = Style::new().add_modifier(Modifier::DIM);
const ZWJ: char = '\u{200D}';

#[cfg(test)]
thread_local! {
    /// Segments wrapped on this thread: the tests' proof that a frame's work follows the rows on
    /// screen and not the lecture's length.
    pub(crate) static WRAPPED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

// ---------------------------------------------------------------------------------------------
// Wrapping, in terminal cells.

/// What wrapping sees: a break, a place a row may end, or a glyph that is never split.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Unit {
    Newline,
    Space,
    /// A character with every zero-width mark after it (combining marks, variation selectors) and
    /// whatever a zero-width joiner joins to it: what the terminal draws as one, and its cells.
    Glyph(usize),
}

/// `text` as byte ranges of units. A tab is a space: Ratatui would drop it as a control.
fn units(text: &str) -> impl Iterator<Item = (usize, usize, Unit)> + '_ {
    let mut chars = text.char_indices().peekable();
    std::iter::from_fn(move || {
        let (start, c) = chars.next()?;
        let mut end = start + c.len_utf8();
        match c {
            '\n' => return Some((start, end, Unit::Newline)),
            ' ' | '\t' => return Some((start, end, Unit::Space)),
            _ => {}
        }
        let mut joined = c == ZWJ;
        while let Some(&(i, n)) = chars.peek() {
            if matches!(n, '\n' | ' ' | '\t') || !(joined || n.width() == Some(0)) {
                break;
            }
            joined = n == ZWJ;
            end = i + n.len_utf8();
            chars.next();
        }
        Some((start, end, Unit::Glyph(text[start..end].width())))
    })
}

/// The rows `text` wraps into at `width` cells, as byte ranges of it. Rows break at spaces where
/// they can, and a word wider than the row is cut between glyphs, never inside one; a newline
/// always breaks. Spaces at a row's ends are not drawn. A glyph wider than the row still gets a row
/// of its own, so every row makes progress; the pane clips it. Always at least one row.
fn wrap(text: &str, width: usize) -> Vec<Range<usize>> {
    let width = width.max(1);
    let mut rows = Vec::new();
    // The row being filled: where it begins, the cells used so far (spaces included), whether it
    // holds a glyph yet, the space run in progress, and the last place it could break: where the
    // text before the spaces ends, where the next word starts, and the cells used before that word.
    let (mut row, mut used, mut began) = (0, 0, false);
    let mut run: Option<usize> = None;
    let mut brk: Option<(usize, usize, usize)> = None;
    for (start, end, unit) in units(text) {
        match unit {
            Unit::Newline => {
                rows.push(row..run.unwrap_or(start).max(row));
                (row, used, began, run, brk) = (end, 0, false, None, None);
            }
            Unit::Space if !began => row = end,
            Unit::Space => {
                run.get_or_insert(start);
                used += 1;
            }
            Unit::Glyph(w) => {
                if let Some(space) = run.take() {
                    brk = Some((space, start, used));
                }
                if !began {
                    (row, used, began) = (start, w, true);
                } else if used + w <= width {
                    used += w;
                } else {
                    match brk.take() {
                        Some((before, next, at)) => {
                            rows.push(row..before);
                            (row, used) = (next, used - at + w);
                            if used > width && row < start {
                                rows.push(row..start); // the word itself is wider than the row
                                (row, used) = (start, w);
                            }
                        }
                        None => {
                            rows.push(row..start);
                            (row, used) = (start, w);
                        }
                    }
                }
            }
        }
    }
    if began || rows.is_empty() {
        rows.push(row..run.unwrap_or(text.len()).max(row));
    }
    while rows.len() > 1 && rows.last().is_some_and(|r| r.is_empty()) {
        rows.pop();
    }
    rows
}

/// A closed segment's rows at `width`: its text's, and a second row for the `recovered` mark when
/// its text takes only one. That row lies past the text's end, so an anchor can name it.
fn segment_rows(line: &Line, width: usize) -> Vec<Range<usize>> {
    #[cfg(test)]
    WRAPPED.with(|n| n.set(n.get() + 1));
    let mut rows = wrap(&line.text, width);
    if line.source == SegmentSource::Recovered && rows.len() < 2 {
        let past = line.text.len() + 1;
        rows.push(past..past);
    }
    rows
}

/// The open utterance as one text, and where its settled part ends.
fn open_text(v: &View) -> Option<(String, usize)> {
    v.open.as_ref().map(|o| (format!("{}{}", o.stable, o.tentative), o.stable.len()))
}

// ---------------------------------------------------------------------------------------------
// The reader's place.

/// Where the reader is (plan §F's per-pane scroll, UI-local): following the live end, or held at
/// an anchor. Nothing here copies the transcript; it names places in the view's own segments.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Scroll {
    held: Option<Held>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Held {
    anchor: Anchor,
    /// Closed segments the view held when the reader left the live end: every one after is new
    /// below. The closed transcript is the log's contiguous prefix, so this is a durable id.
    seen: usize,
}

/// The top row while scrolled: a closed segment's id and the byte in its text where the row begins
/// (past the end for the `recovered` row). Width-independent, so a rewrap at any width puts the
/// same words at the top.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Anchor {
    pub(crate) id: u64,
    pub(crate) at: usize,
}

/// A reading move.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Move {
    Up(usize),
    Down(usize),
    /// The body's height less two rows, so two rows of context stay on screen.
    PageUp,
    PageDown,
    /// Back to following the live end.
    Live,
}

/// A row: the item (a closed segment's index, or the open utterance at `closed.len()`) and the row
/// within it, at one width.
type Pos = (usize, usize);

/// The transcript at one text width, wrapping an item only when a walk reaches it.
struct Rows<'a> {
    v: &'a View,
    width: usize,
    open: Option<(String, usize)>,
}

impl Rows<'_> {
    fn new(v: &View, width: usize) -> Rows<'_> {
        Rows { v, width, open: open_text(v) }
    }

    fn items(&self) -> usize {
        self.v.closed.len() + usize::from(self.open.is_some())
    }

    fn of(&self, item: usize) -> Vec<Range<usize>> {
        match self.v.closed.get(item) {
            Some(line) => segment_rows(line, self.width),
            None => self.open.as_ref().map_or_else(|| vec![0..0], |(t, _)| wrap(t, self.width)),
        }
    }

    fn last(&self) -> Option<Pos> {
        let item = self.items().checked_sub(1)?;
        Some((item, self.of(item).len() - 1))
    }

    /// `k` rows up from `p`, stopping at the first row.
    fn up(&self, (mut item, mut row): Pos, mut k: usize) -> Pos {
        while k > 0 {
            if row > 0 {
                let step = row.min(k);
                (row, k) = (row - step, k - step);
            } else if item == 0 {
                break;
            } else {
                item -= 1;
                (row, k) = (self.of(item).len() - 1, k - 1);
            }
        }
        (item, row)
    }

    /// `k` rows down from `p`, stopping at the last row.
    fn down(&self, (mut item, mut row): Pos, mut k: usize) -> Pos {
        let mut len = self.of(item).len();
        while k > 0 {
            if row + 1 < len {
                let step = (len - 1 - row).min(k);
                (row, k) = (row + step, k - step);
            } else if item + 1 < self.items() {
                item += 1;
                len = self.of(item).len();
                (row, k) = (0, k - 1);
            } else {
                break;
            }
        }
        (item, row)
    }

    /// Whether everything from `p` to the end fits in `height` rows: counts no further than that.
    fn ends_within(&self, (item, row): Pos, height: usize) -> bool {
        let mut n = self.of(item).len() - row;
        for i in item + 1..self.items() {
            if n > height {
                return false;
            }
            n += self.of(i).len();
        }
        n <= height
    }

    /// The top row while following: `height` rows above the end, or the first row.
    fn live_top(&self, height: usize) -> Option<Pos> {
        Some(self.up(self.last()?, height - 1))
    }

    /// The anchor's row, today: its segment, and the row its position falls in. An id past the
    /// closed transcript (it never shrinks, but nothing here trusts that) is its last row.
    fn find(&self, a: Anchor) -> Option<Pos> {
        let last = self.v.closed.len().checked_sub(1)?;
        let item = usize::try_from(a.id).unwrap_or(usize::MAX).min(last);
        let rows = self.of(item);
        let row = if item as u64 == a.id { rows.iter().rposition(|r| r.start <= a.at).unwrap_or(0) } else { rows.len() - 1 };
        Some((item, row))
    }

    /// The anchor naming `p`. The open utterance is not a place to hold: its words are replaced
    /// as they settle, so a row there is held at the last closed row instead.
    fn anchor(&self, (item, row): Pos) -> Option<Anchor> {
        let (item, row) = if item < self.v.closed.len() { (item, row) } else { (item.checked_sub(1)?, usize::MAX) };
        let line = &self.v.closed[item];
        let rows = segment_rows(line, self.width);
        let row = row.min(rows.len() - 1);
        Some(Anchor { id: line.id, at: rows[row].start })
    }
}

/// The text's width and the rows in a pane body; none when not one cell of text fits.
fn text_size(body: Rect) -> Option<(usize, usize)> {
    let width = (body.width as usize).checked_sub(LANE).filter(|w| *w > 0)?;
    (body.height > 0).then_some((width, body.height as usize))
}

impl Scroll {
    /// Whether the reader follows the live end.
    #[cfg(test)]
    pub(crate) fn following(&self) -> bool {
        self.held.is_none()
    }

    /// The anchor, while scrolled.
    #[cfg(test)]
    pub(crate) fn anchor(&self) -> Option<Anchor> {
        self.held.map(|h| h.anchor)
    }

    /// Closed segments that arrived below since the reader left the live end; none while following.
    pub(crate) fn unseen(&self, v: &View) -> Option<usize> {
        self.held.map(|h| v.closed.len().saturating_sub(h.seen))
    }

    /// One reading move through the transcript in `body`, as it was last drawn. Moving up leaves
    /// the live end; reaching it going down follows again. Returns whether anything changed. The
    /// canonical transcript is only read.
    pub(crate) fn apply(&mut self, m: Move, v: &View, body: Rect) -> bool {
        if m == Move::Live {
            return self.held.take().is_some();
        }
        let Some((width, height)) = text_size(body) else { return false };
        let page = height.saturating_sub(2).max(1);
        let rows = Rows::new(v, width);
        match (m, self.held) {
            (Move::Up(_) | Move::PageUp, None) => {
                let k = if m == Move::PageUp { page } else { m.rows() };
                let Some(live) = rows.live_top(height) else { return false };
                if live == (0, 0) {
                    return false; // it all fits: there is nothing above
                }
                let Some(anchor) = rows.anchor(rows.up(live, k)) else { return false };
                self.held = Some(Held { anchor, seen: v.closed.len() });
                true
            }
            (Move::Up(_) | Move::PageUp, Some(held)) => {
                let k = if m == Move::PageUp { page } else { m.rows() };
                let Some(at) = rows.find(held.anchor) else { return self.held.take().is_some() };
                let Some(anchor) = rows.anchor(rows.up(at, k)) else { return false };
                let moved = anchor != held.anchor;
                self.held = Some(Held { anchor, ..held });
                moved
            }
            (Move::Down(_) | Move::PageDown, Some(held)) => {
                let k = if m == Move::PageDown { page } else { m.rows() };
                let Some(at) = rows.find(held.anchor) else { return self.held.take().is_some() };
                let to = rows.down(at, k);
                if to.0 >= v.closed.len() || rows.ends_within(to, height) {
                    self.held = None;
                    return true;
                }
                let Some(anchor) = rows.anchor(to) else { return false };
                self.held = Some(Held { anchor, ..held });
                anchor != held.anchor
            }
            (Move::Down(_) | Move::PageDown, None) | (Move::Live, _) => false,
        }
    }
}

impl Move {
    fn rows(self) -> usize {
        match self {
            Move::Up(k) | Move::Down(k) => k,
            _ => 0,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Drawing.

/// A row's text as drawn: a tab becomes the space it was wrapped as.
fn drawn(text: &str) -> Cow<'_, str> {
    if text.contains('\t') {
        Cow::Owned(text.replace('\t', " "))
    } else {
        Cow::Borrowed(text)
    }
}

/// The transcript into `body`: from the anchor down while scrolled, else ending at the live edge.
/// A transcript shorter than the body starts at its top. `edge` is the live edge's glyph and
/// `teal` its style: the theme's, so `NO_COLOR` and ASCII hold here as in the frame.
pub(crate) fn transcript(buf: &mut Buffer, body: Rect, v: &View, scroll: &Scroll, edge: &str, teal: Style) {
    let Some((width, height)) = text_size(body) else { return };
    let rows = Rows::new(v, width);
    if rows.items() == 0 {
        buf.set_stringn(body.x, body.y, "Nothing said yet.", body.width as usize, DIM);
        return;
    }
    let top = match scroll.held {
        Some(h) => rows.find(h.anchor),
        None => rows.live_top(height),
    };
    let Some((mut item, mut row)) = top else { return };
    let (x, text_x) = (body.x, body.x + LANE as u16);
    let mut y = body.y;
    while y < body.bottom() && item < rows.items() {
        let wrapped = rows.of(item);
        for (i, r) in wrapped.iter().enumerate().skip(row) {
            if y >= body.bottom() {
                break;
            }
            match v.closed.get(item) {
                Some(line) => {
                    if i == 0 {
                        buf.set_stringn(x, y, line.said_at.format("%H:%M:%S").to_string(), TIME, DIM);
                    } else if i == 1 && line.source == SegmentSource::Recovered {
                        buf.set_stringn(x, y, RECOVERED, LANE, DIM);
                    }
                    // the `recovered` row past the text's end has no text
                    let text = line.text.get(r.clone()).unwrap_or("");
                    buf.set_stringn(text_x, y, drawn(text), width, Style::new());
                }
                None => {
                    let Some((text, settled)) = &rows.open else { break };
                    buf.set_stringn(x + TIME as u16, y, edge, 1, teal);
                    // one span each side of the settled/tentative boundary, only where a row crosses it
                    let (a, b) = (r.start, r.end.min(*settled).max(r.start));
                    let (after, _) = buf.set_stringn(text_x, y, drawn(&text[a..b]), width, Style::new());
                    let used = (after - text_x) as usize;
                    buf.set_stringn(after, y, drawn(&text[b..r.end]), width.saturating_sub(used), DIM);
                }
            }
            y += 1;
        }
        (item, row) = (item + 1, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::hydrate::Hydration;
    use crate::tui::state::{Identity, SourceKind};
    use chrono::{DateTime, Local, TimeZone};
    use lecturelive_core::session::coordinator::Notification;
    use lecturelive_core::session::lecture::Event;
    use lecturelive_core::session::segments::Segment;

    fn cells(text: &str, width: usize) -> Vec<&str> {
        wrap(text, width).into_iter().map(|r| &text[r]).collect()
    }

    fn w(s: &str) -> usize {
        s.width()
    }

    // ---- wrapping -----------------------------------------------------------------------------

    #[test]
    fn ascii_wraps_at_spaces_with_nothing_past_the_width() {
        assert_eq!(cells("the quick brown fox jumps over the lazy dog", 10), vec!["the quick", "brown fox", "jumps over", "the lazy", "dog"]);
        assert_eq!(cells("exactly ten", 11), vec!["exactly ten"], "a row may fill its width");
        assert_eq!(cells("a  b   c", 3), vec!["a", "b", "c"], "a run of spaces is one break");
        assert_eq!(cells("a  b   c", 4), vec!["a  b", "c"], "and kept inside a row");
        assert_eq!(cells("  leading and trailing  ", 40), vec!["leading and trailing"], "spaces at a row's ends are not drawn");
        assert_eq!(cells("", 10), vec![""], "always one row");
        assert_eq!(cells("wrap, punctuation; stays: attached.", 12), vec!["wrap,", "punctuation;", "stays:", "attached."]);
    }

    #[test]
    fn a_word_wider_than_the_row_is_cut_between_glyphs() {
        assert_eq!(cells("antidisestablishmentarianism", 10), vec!["antidisest", "ablishment", "arianism"]);
        assert_eq!(cells("see https://example.org/a/very/long/path ok", 12), vec!["see", "https://exam", "ple.org/a/ve", "ry/long/path", "ok"]);
    }

    /// CJK is two cells a character, with no spaces to break at: rows break between characters
    /// and never hold more cells than the width — a byte or char count would say otherwise.
    #[test]
    fn cjk_wraps_by_cells() {
        let text = "機械学習の最適化について説明します";
        let rows = cells(text, 10);
        assert_eq!(rows, vec!["機械学習の", "最適化につ", "いて説明し", "ます"]);
        assert!(rows.iter().all(|r| w(r) <= 10));
        assert_eq!(text.len(), 51, "bytes");
        assert_eq!(cells(text, 9)[0], "機械学習", "an odd width leaves the last cell empty rather than halving a glyph");
        assert_eq!(cells("漢", 1), vec!["漢"], "a glyph wider than the row still gets one of its own");
    }

    /// A combining mark is part of the glyph before it: zero cells, never alone at a row's start.
    #[test]
    fn combining_marks_stay_on_their_letter() {
        let e = "e\u{301}"; // e + combining acute
        let text = format!("caf{e} caf{e} caf{e}");
        assert_eq!(cells(&text, 4), vec![format!("caf{e}"); 3]);
        let long = format!("{e}{e}{e}{e}{e}{e}");
        let rows = cells(&long, 4);
        assert_eq!(rows, vec![format!("{e}{e}{e}{e}"), format!("{e}{e}")]);
        assert!(rows.iter().all(|r| !r.starts_with('\u{301}')));
    }

    /// Emoji: two cells; a presentation selector or a joined sequence is one glyph.
    #[test]
    fn emoji_are_wide_and_joined_sequences_hold_together() {
        assert_eq!(cells("🎓🎓🎓", 5), vec!["🎓🎓", "🎓"]);
        let heart = "❤\u{FE0F}";
        assert_eq!(w(heart), 2);
        let hearts = format!("{heart}{heart}{heart}");
        assert_eq!(cells(&hearts, 4), vec![format!("{heart}{heart}"), heart.to_string()]);
        let family = "👩\u{200D}🔬";
        let families = format!("{family}{family}");
        let rows = cells(&families, 2);
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.chars().next() != Some(ZWJ)), "a joiner never starts a row");
    }

    /// Every row's cells, as Ratatui draws them, fit the width: the wrap and the buffer agree.
    #[test]
    fn rows_fit_as_ratatui_draws_them() {
        let samples = ["the quick brown fox", "機械学習の最適化", "cafe\u{301} na\u{303}o", "🎓 graduation ❤\u{FE0F} 👩\u{200D}🔬", "mixed 漢字 and emoji 🎓 in one line", "tab\tseparated\twords"];
        for text in samples {
            for width in 1..=24 {
                for r in wrap(text, width) {
                    let mut b = Buffer::empty(Rect::new(0, 0, 80, 1));
                    let (end, _) = b.set_stringn(0, 0, drawn(&text[r.clone()]), 80, Style::new());
                    let glyphs = units(&text[r.clone()]).filter(|u| matches!(u.2, Unit::Glyph(_))).count();
                    assert!(end as usize <= width || glyphs == 1, "{text:?} at {width}: {:?} takes {end} cells", &text[r]);
                }
            }
        }
    }

    #[test]
    fn a_newline_always_breaks() {
        assert_eq!(cells("first line\nsecond", 40), vec!["first line", "second"]);
        assert_eq!(cells("a\n\nb", 40), vec!["a", "", "b"], "a blank line between");
        assert_eq!(cells("ends with one\n", 40), vec!["ends with one"]);
        assert_eq!(cells("trailing  \nspaces", 40), vec!["trailing", "spaces"]);
    }

    #[test]
    fn tiny_widths_never_panic_and_always_progress() {
        for text in ["", " ", "a", "ab cd", "漢字", "e\u{301}", "\n", "🎓 x", "word\tword"] {
            for width in 0..4 {
                let rows = wrap(text, width);
                assert!(!rows.is_empty());
                assert!(rows.windows(2).all(|p| p[0].start < p[1].start), "{text:?} at {width}: {rows:?}");
                assert!(rows.iter().all(|r| r.start <= r.end && r.end <= text.len()));
            }
        }
    }

    // ---- the pane -----------------------------------------------------------------------------

    fn identity() -> Identity {
        Identity { course: "Machine Learning".into(), lecture: "Week 03 — Optimisation".into(), input: "BlackHole 2ch".into(), kind: SourceKind::Loopback, notes_file: "n.md".into(), transcript_file: "t.txt".into() }
    }

    fn at(secs: u64) -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 9, 26, 10, 0, 0).unwrap() + chrono::Duration::seconds(secs as i64)
    }

    fn seg(id: u64, text: &str, source: SegmentSource) -> Event {
        let t = at(id * 5);
        Event::Session(Notification::Segment(Segment { id, recording_id: Default::default(), start_sample: id * 16_000, end_sample: (id + 1) * 16_000, said_at: t, start: t, end: t, text: text.into(), words: Vec::new(), source }))
    }

    fn open(stable: &str, tentative: &str) -> Event {
        Event::Session(Notification::Open { stable: stable.into(), tentative: tentative.into() })
    }

    fn view(n: u64) -> View {
        let mut v = View::new(identity(), Hydration::empty(), Vec::new());
        for id in 0..n {
            v.reduce(&seg(id, &line(id), SegmentSource::Live), at(0));
        }
        v
    }

    /// Two-hour-lecture-shaped lines: 1 to 3 rows at 40 cells, varying.
    fn line(id: u64) -> String {
        let words = ["the", "gradient", "points", "uphill", "so", "we", "step", "against", "it", "scaled", "by", "the", "learning", "rate"];
        (0..6 + id as usize % 17).map(|k| words[(id as usize + k) % words.len()]).collect::<Vec<_>>().join(" ") + &format!(" ({id})")
    }

    const TEAL: Style = Style::new().fg(ratatui::style::Color::Cyan);

    fn draw(v: &View, s: &Scroll, width: u16, height: u16) -> Vec<String> {
        let mut b = Buffer::empty(Rect::new(0, 0, width, height));
        let area = b.area;
        transcript(&mut b, area, v, s, "▎", TEAL);
        (0..height).map(|y| (0..width).map(|x| b[(x, y)].symbol()).collect::<String>().trim_end().to_string()).collect()
    }

    fn body(width: u16, height: u16) -> Rect {
        Rect::new(0, 0, width, height)
    }

    #[test]
    fn following_shows_the_live_tail_and_the_open_utterance_under_it() {
        let mut v = view(3);
        let s = Scroll::default();
        let l = draw(&v, &s, 50, 6);
        assert_eq!(l[0], "10:00:00  the gradient points uphill so we (0)");
        assert!(l.iter().all(|r| r.is_empty() || r.starts_with("10:0") || r.starts_with("          ")), "a hanging indent: {l:?}");
        v.reduce(&open("the denominator changes", " with the root of n"), at(0));
        let l = draw(&v, &s, 60, 20);
        let last = l.iter().rposition(|r| !r.is_empty()).unwrap();
        assert_eq!(l[last], "        ▎ the denominator changes with the root of n", "no time: the live edge in its place");
        // appending keeps the bottom live
        for id in 3..40 {
            v.reduce(&seg(id, &line(id), SegmentSource::Live), at(0));
            let l = draw(&v, &s, 50, 8);
            let last = l.iter().rposition(|r| !r.is_empty()).unwrap();
            assert!(l[last].contains(&format!("({id})")) && (id < 5 || last == 7), "{id}: the newest is on the last row: {l:?}");
        }
        v.reduce(&open("still speaking", ""), at(0));
        assert_eq!(draw(&v, &s, 50, 8)[7], "        ▎ still speaking");
        // the final segment replaces the open utterance in the same place
        v.reduce(&seg(40, "still speaking", SegmentSource::Live), at(0));
        let l = draw(&v, &s, 50, 8);
        assert_eq!(l[7], "10:03:20  still speaking");
        assert!(!l.iter().any(|r| r.contains('▎')), "the open utterance closed");
    }

    /// Plan §J: recovery commits history; it must not close what is being said now — and the pane
    /// shows both: the recovered segment in its place with its mark, the open utterance still live.
    #[test]
    fn recovered_segment_does_not_close_live_open() {
        let mut v = view(2);
        v.reduce(&open("gradient", " descent"), at(0));
        v.reduce(&seg(2, "words the network lost", SegmentSource::Recovered), at(0));
        assert!(v.open.is_some());
        let l = draw(&v, &Scroll::default(), 60, 12);
        let r = l.iter().position(|r| r.contains("words the network lost")).unwrap();
        assert_eq!(l[r], "10:00:10  words the network lost");
        assert_eq!(l[r + 1], "recovered", "the mark under its time, on a row of its own");
        assert_eq!(l[r + 2], "        ▎ gradient descent", "and the live utterance still open below");
        // wrapped, the mark shares the second row with the text
        let mut v = view(0);
        v.reduce(&seg(0, "a recovered stretch of transcript that wraps onto more rows", SegmentSource::Recovered), at(0));
        let l = draw(&v, &Scroll::default(), 40, 4);
        assert_eq!(l[..3], ["10:00:00  a recovered stretch of", "recovered transcript that wraps onto", "          more rows"]);
        // an imported one carries no mark
        let mut v = view(0);
        v.reduce(&seg(0, "imported", SegmentSource::Imported), at(0));
        assert_eq!(draw(&v, &Scroll::default(), 40, 3), vec!["10:00:00  imported", "", ""]);
    }

    /// Stable text in ink, tentative dim, a teal edge on every open row; the gutter dim.
    #[test]
    fn open_rows_keep_the_settled_and_tentative_styles_across_wraps() {
        let mut v = view(1);
        v.reduce(&open("the settled words run on across", " and the tentative ones follow them"), at(0));
        let mut b = Buffer::empty(Rect::new(0, 0, 30, 10));
        let area = b.area;
        transcript(&mut b, area, &v, &Scroll::default(), "▎", TEAL);
        let rows: Vec<u16> = (0..10).filter(|&y| b[(8, y)].symbol() == "▎").collect();
        assert!(rows.len() >= 3, "{rows:?}");
        for &y in &rows {
            assert_eq!(b[(8, y)].fg, ratatui::style::Color::Cyan, "every open row has its teal edge");
            for x in 0..8 {
                assert_eq!(b[(x, y)].symbol(), " ", "no time on an open row");
            }
        }
        let styles: Vec<(String, Modifier)> = rows.iter().flat_map(|&y| (10..30).map(move |x| (x, y))).map(|p| (b[p].symbol().to_string(), b[p].modifier)).filter(|(s, _)| s != " ").collect();
        let text: String = styles.iter().map(|(s, _)| s.as_str()).collect();
        let split = text.find("and").unwrap();
        assert!(styles[..split].iter().all(|(_, m)| m.is_empty()), "settled: ink");
        assert!(styles[split..].iter().all(|(_, m)| *m == Modifier::DIM), "tentative: dim");
        assert_eq!(b[(0, 0)].modifier, Modifier::DIM, "the time is dim");
        assert!(b.content.iter().all(|c| c.bg == ratatui::style::Color::Reset), "no background anywhere");
    }

    #[test]
    fn narrow_bodies_draw_nothing_rather_than_underflow() {
        let mut v = view(5);
        v.reduce(&open("x", "y"), at(0));
        for (w, h) in [(0, 0), (1, 1), (5, 3), (10, 3), (11, 3), (0, 5), (40, 0)] {
            let mut b = Buffer::empty(Rect::new(0, 0, w.max(1), h.max(1)));
            transcript(&mut b, Rect::new(0, 0, w, h), &v, &Scroll::default(), "▎", TEAL);
            let mut s = Scroll::default();
            for m in [Move::Up(1), Move::PageUp, Move::Down(1), Move::PageDown, Move::Live] {
                s.apply(m, &v, Rect::new(0, 0, w, h));
            }
        }
        assert_eq!(draw(&v, &Scroll::default(), 11, 2), vec!["        ▎ x", "        ▎ y"], "one cell of text is enough");
    }

    // ---- scrolling ----------------------------------------------------------------------------

    /// Plan §J: scrolled up, Esc returns to live; already live, it changes nothing.
    #[test]
    fn scroll_then_esc_returns_to_live() {
        let v = view(60);
        let (mut s, b) = (Scroll::default(), body(50, 10));
        assert!(!s.apply(Move::Live, &v, b), "live already");
        assert!(s.apply(Move::Up(1), &v, b));
        assert!(!s.following());
        assert_ne!(draw(&v, &s, 50, 10), draw(&v, &Scroll::default(), 50, 10));
        assert!(s.apply(Move::Live, &v, b));
        assert!(s.following() && s.unseen(&v).is_none());
        assert_eq!(draw(&v, &s, 50, 10), draw(&v, &Scroll::default(), 50, 10), "the live bottom again");
    }

    #[test]
    fn up_moves_one_row_and_down_to_the_end_follows_again() {
        let v = view(30);
        let (mut s, b) = (Scroll::default(), body(50, 8));
        let live = draw(&v, &s, 50, 8);
        s.apply(Move::Up(1), &v, b);
        let one = draw(&v, &s, 50, 8);
        assert_eq!(one[1..], live[..7], "one row up: everything moved down by one");
        s.apply(Move::Up(4), &v, b);
        for k in 0..4 {
            assert!(s.apply(Move::Down(1), &v, b));
            assert!(!s.following(), "{k}");
        }
        assert_eq!(draw(&v, &s, 50, 8), one);
        assert!(s.apply(Move::Down(1), &v, b));
        assert!(s.following(), "reaching the end follows again");
        assert!(!s.apply(Move::Down(1), &v, b), "down while live does nothing");
        // page up keeps two rows of context; page down to the end follows
        s.apply(Move::PageUp, &v, b);
        let page = draw(&v, &s, 50, 8);
        assert_eq!(page[6..], live[..2], "two rows of context");
        s.apply(Move::PageUp, &v, b);
        s.apply(Move::PageDown, &v, b);
        assert!(!s.following());
        assert!(s.apply(Move::PageDown, &v, b));
        assert!(s.following());
    }

    #[test]
    fn nothing_above_means_nothing_to_scroll() {
        let (v, b) = (view(2), body(50, 10));
        let mut s = Scroll::default();
        assert!(!s.apply(Move::Up(1), &v, b));
        assert!(!s.apply(Move::PageUp, &v, b));
        assert!(s.following());
        let empty = view(0);
        assert!(!s.apply(Move::Up(1), &empty, b));
        assert_eq!(draw(&empty, &s, 50, 3), vec!["Nothing said yet.", "", ""], "the empty transcript says so");
    }

    #[test]
    fn the_top_stops_at_the_first_row() {
        let v = view(30);
        let (mut s, b) = (Scroll::default(), body(50, 8));
        s.apply(Move::Up(10_000), &v, b);
        assert_eq!(s.anchor(), Some(Anchor { id: 0, at: 0 }));
        assert!(!s.apply(Move::Up(1), &v, b), "already at the top");
        assert!(draw(&v, &s, 50, 8)[0].starts_with("10:00:00  the gradient"));
    }

    /// Scrolled, new transcript neither moves the reader nor goes uncounted; open updates are not
    /// new segments.
    #[test]
    fn arrivals_while_scrolled_count_below_and_move_nothing() {
        let mut v = view(40);
        let (mut s, b) = (Scroll::default(), body(50, 8));
        s.apply(Move::PageUp, &v, b);
        let (held, shown) = (s.anchor(), draw(&v, &s, 50, 8));
        assert_eq!(s.unseen(&v), Some(0));
        for k in 0..5 {
            v.reduce(&open("words", &format!(" {k}")), at(0));
        }
        assert_eq!(s.unseen(&v), Some(0), "an open utterance is not a new segment");
        v.reduce(&seg(40, "one", SegmentSource::Live), at(0));
        v.reduce(&seg(41, "two", SegmentSource::Recovered), at(0));
        v.reduce(&seg(42, "three", SegmentSource::Live), at(0));
        assert_eq!(s.unseen(&v), Some(3));
        assert_eq!((s.anchor(), draw(&v, &s, 50, 8)), (held, shown), "the reader's place held");
    }

    /// Plan §J: following survives a resize; scrolled, the anchor's words stay on top at every
    /// width, and 140 → 80 → 140 comes back to the same rows.
    #[test]
    fn resize_preserves_follow_and_anchor() {
        let mut v = view(50);
        v.reduce(&open("being said", " now"), at(0));
        let mut s = Scroll::default();
        for (w, h) in [(140, 40), (80, 24), (140, 40)] {
            let l = draw(&v, &s, w, h);
            assert!(l.iter().rev().find(|r| !r.is_empty()).unwrap().contains("▎ being said now"), "{w}×{h} follows");
        }
        assert!(s.following());
        s.apply(Move::Up(25), &v, body(140, 30));
        let (held, before) = (s.anchor().unwrap(), draw(&v, &s, 140, 30));
        // at 80 the top row is the one of the anchored segment that holds the anchored words
        let line = &v.closed[held.id as usize];
        let rows = segment_rows(line, 80 - LANE);
        let r = rows[rows.iter().rposition(|r| r.start <= held.at).unwrap()].clone();
        assert!(r.start <= held.at && (held.at < r.end || r.is_empty()));
        assert_eq!(draw(&v, &s, 80, 24)[0].get(LANE..).unwrap_or(""), &line.text[r], "80: the anchor's words on top");
        assert_eq!(s.anchor(), Some(held), "a resize does not move the anchor");
        assert_eq!(draw(&v, &s, 140, 30), before, "back at 140, the same rows");
        assert!(!s.following());
    }
}
