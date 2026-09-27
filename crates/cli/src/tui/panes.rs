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
use std::time::{Duration, Instant};

use lecturelive_core::capture::worker::CaptureState;
use lecturelive_core::session::segments::SegmentSource;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::capture;

use super::markdown::{self, Block, Chunk, Kind, Lead};
use super::state::{Line, Notes, View, PREVIEW_CAP};

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
pub(crate) fn wrap(text: &str, width: usize) -> Vec<Range<usize>> {
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

// ---------------------------------------------------------------------------------------------
// The notes (Task 9).
//
// Committed chunks are parsed once, when the canonical notes change (`state.rs`); the preview is
// parsed here at most every [`PREVIEW_EVERY`]. A draw only wraps the blocks it shows: following,
// from the last block upward, as the transcript does. The walker below names a place as a block and
// a row within it, so a scroll anchor (Task 10) can hold a block and a byte of its text.

/// The notes' chrome glyphs and live accent: the theme's, so `NO_COLOR` and ASCII hold here too.
pub(crate) struct Ink<'a> {
    pub(crate) edge: &'a str,
    pub(crate) bullet: &'a str,
    pub(crate) quote: &'a str,
    pub(crate) slide: &'a str,
    pub(crate) rule: &'a str,
    /// The attention mark (denied capture in the slides pane).
    pub(crate) warn: &'a str,
    pub(crate) teal: Style,
}

/// The preview is parsed at most this often (plan §F: ≤10 Hz), however fast its deltas come.
pub(crate) const PREVIEW_EVERY: Duration = Duration::from_millis(100);

/// The preview as last parsed, for drawing (UI-local, the reactor's): which preview it is, how much
/// of it was seen, when it was parsed, and the blocks.
#[derive(Debug, Default)]
pub(crate) struct Preview {
    epoch: u64,
    /// The preview's length when last looked at, and the length of what was shown of it.
    raw: usize,
    shown: usize,
    at: Option<Instant>,
    blocks: Vec<Block>,
}

impl Preview {
    /// Brings the parsed preview up to `notes`' at `now`, unless the last parse is under
    /// [`PREVIEW_EVERY`] old; then returns when to look again. A preview that ended is dropped at
    /// once, and a new one never shows an earlier one's blocks.
    pub(crate) fn refresh(&mut self, notes: &Notes, now: Instant) -> Option<Instant> {
        let Some(text) = &notes.preview else {
            *self = Preview::default();
            return None;
        };
        if self.epoch != notes.epoch {
            *self = Preview { epoch: notes.epoch, ..Preview::default() };
        }
        if self.at.is_some() && self.raw == text.len() {
            return None;
        }
        if let Some(due) = self.at.map(|at| at + PREVIEW_EVERY).filter(|due| now < *due) {
            return Some(due);
        }
        self.raw = text.len();
        let shown = shown(text);
        if self.at.is_none() || shown.len() != self.shown {
            self.blocks = markdown::parse(shown);
            self.shown = shown.len();
        }
        self.at = Some(now);
        None
    }

    /// The blocks to draw for `notes`' preview: none unless they were parsed from this very one.
    fn blocks<'a>(&'a self, notes: &Notes) -> &'a [Block] {
        if notes.preview.is_some() && self.epoch == notes.epoch {
            &self.blocks
        } else {
            &[]
        }
    }
}

/// What of a preview is shown: its first [`PREVIEW_CAP`] bytes at most, up to the last whitespace in
/// them, so a word the model is still extending never flashes half-written.
fn shown(text: &str) -> &str {
    let mut cap = text.len().min(PREVIEW_CAP);
    while !text.is_char_boundary(cap) {
        cap -= 1;
    }
    let t = &text[..cap];
    t.char_indices().rev().find(|(_, c)| c.is_whitespace()).map_or("", |(i, c)| &t[..i + c.len_utf8()])
}

/// A notes block: a committed chunk's `(chunk, block)`, or the preview's at `chunk == chunks.len()`.
type Item = (usize, usize);
/// A row: the block, and the row within it.
type NotePos = (Item, usize);

/// A block's row: blank (the space before a chunk or a `##`), or a stretch of its text.
#[derive(Clone, Debug, PartialEq)]
enum NoteRow {
    Blank,
    Text(Range<usize>),
}

/// The notes at one text width: the committed chunks, then the preview's blocks, each block wrapped
/// only when a walk reaches it.
struct NoteRows<'a> {
    chunks: &'a [Chunk],
    preview: &'a [Block],
    width: usize,
}

/// A bullet's cells: `•` and its ASCII `-` alike, so scrolling and drawing agree on every row.
const BULLET: usize = 1;

impl<'a> NoteRows<'a> {
    fn block(&self, (c, b): Item) -> Option<&'a Block> {
        match c.cmp(&self.chunks.len()) {
            std::cmp::Ordering::Less => self.chunks[c].blocks.get(b),
            std::cmp::Ordering::Equal => self.preview.get(b),
            std::cmp::Ordering::Greater => None,
        }
    }

    fn blocks_in(&self, c: usize) -> usize {
        if c < self.chunks.len() {
            self.chunks[c].blocks.len()
        } else {
            self.preview.len()
        }
    }

    fn next(&self, (c, b): Item) -> Option<Item> {
        if b + 1 < self.blocks_in(c) {
            return Some((c, b + 1));
        }
        (c + 1..=self.chunks.len()).find(|&c| self.blocks_in(c) > 0).map(|c| (c, 0))
    }

    fn prev(&self, (c, b): Item) -> Option<Item> {
        if b > 0 {
            return Some((c, b - 1));
        }
        (0..c).rev().find(|&c| self.blocks_in(c) > 0).map(|c| (c, self.blocks_in(c) - 1))
    }

    fn last(&self) -> Option<Item> {
        (0..=self.chunks.len()).rev().find(|&c| self.blocks_in(c) > 0).map(|c| (c, self.blocks_in(c) - 1))
    }

    fn preview(&self, (c, _): Item) -> bool {
        c == self.chunks.len()
    }

    /// A blank row before a chunk's first block (none before the very first) and before a `#` or
    /// `##` inside one: the notes' sections breathe; `###` and below do not.
    fn separated(&self, item: Item) -> bool {
        match item.1 {
            0 => self.prev(item).is_some(),
            _ => self.block(item).is_some_and(|b| matches!(b.kind, Kind::Heading(l) if l <= 2)),
        }
    }

    /// Where a block's text begins in the lane, and its lead (a bullet or number) with the cell it
    /// starts at: quotes first, two cells each, then two cells a list level; a list item's text
    /// hangs after its lead. Clamped so a deep nesting still leaves half the lane for text.
    fn geometry(&self, b: &Block) -> (usize, Option<(usize, String)>) {
        let quote = b.quote as usize * 2;
        let level = b.depth.saturating_sub(1) as usize * 2;
        let (hang, lead) = match &b.kind {
            Kind::Item(lead) => {
                let (text, w) = match lead {
                    Lead::Bullet => (String::new(), BULLET),
                    Lead::Number(n) => (format!("{n}."), format!("{n}.").len()),
                };
                (quote + level + w + 1, Some((quote + level, text)))
            }
            Kind::Code(indent) => (quote + level + if b.depth > 0 { 2 } else { 0 } + 2 + *indent as usize, None),
            _ => (quote + level + if b.depth > 0 { 2 } else { 0 }, None),
        };
        (hang.min(self.width / 2), lead.map(|(at, t)| (at.min(self.width / 2), t)))
    }

    fn of(&self, item: Item) -> Vec<NoteRow> {
        let Some(b) = self.block(item) else { return vec![NoteRow::Blank] };
        let mut rows = Vec::new();
        if self.separated(item) {
            rows.push(NoteRow::Blank);
        }
        match b.kind {
            Kind::Slide { .. } | Kind::Rule => rows.push(NoteRow::Text(0..b.text.len())),
            _ => {
                let (hang, _) = self.geometry(b);
                rows.extend(wrap(&b.text, self.width.saturating_sub(hang).max(1)).into_iter().map(NoteRow::Text));
            }
        }
        rows
    }

    fn up(&self, (mut item, mut row): NotePos, mut k: usize) -> NotePos {
        while k > 0 {
            if row > 0 {
                let step = row.min(k);
                (row, k) = (row - step, k - step);
            } else if let Some(p) = self.prev(item) {
                (item, row, k) = (p, self.of(p).len() - 1, k - 1);
            } else {
                break;
            }
        }
        (item, row)
    }

    /// The top row while following: `height` rows above the end, or the first row.
    fn live_top(&self, height: usize) -> Option<NotePos> {
        let last = self.last()?;
        Some(self.up((last, self.of(last).len() - 1), height - 1))
    }

    fn down(&self, (mut item, mut row): NotePos, mut k: usize) -> NotePos {
        let mut len = self.of(item).len();
        while k > 0 {
            if row + 1 < len {
                let step = (len - 1 - row).min(k);
                (row, k) = (row + step, k - step);
            } else if let Some(n) = self.next(item) {
                (item, row, k) = (n, 0, k - 1);
                len = self.of(item).len();
            } else {
                break;
            }
        }
        (item, row)
    }

    /// Whether everything from `p` to the end fits in `height` rows: counts no further than that.
    fn ends_within(&self, (item, row): NotePos, height: usize) -> bool {
        let mut n = self.of(item).len() - row;
        let mut at = item;
        while let Some(next) = self.next(at) {
            if n > height {
                return false;
            }
            n += self.of(next).len();
            at = next;
        }
        n <= height
    }

    /// The anchor's row, today. A chunk the notes no longer hold (a polish replaced them all) is
    /// nowhere: the reader goes back to live; a block past a chunk's end is its last.
    fn find(&self, a: NoteAnchor) -> Option<NotePos> {
        let blocks = self.chunks.get(a.chunk)?.blocks.len();
        let item = (a.chunk, a.block.min(blocks.checked_sub(1)?));
        let rows = self.of(item);
        let row = match a.at {
            _ if item.1 != a.block => rows.len() - 1,
            None => 0,
            Some(at) => rows.iter().rposition(|r| matches!(r, NoteRow::Text(t) if t.start <= at)).unwrap_or(0),
        };
        Some((item, row))
    }

    /// The anchor naming `p`. The preview is no place to hold — it is replaced when it commits —
    /// so a row in it holds at the last committed row instead.
    fn anchor(&self, (item, row): NotePos) -> Option<NoteAnchor> {
        let (item, row) = if self.preview(item) { (self.prev(item).filter(|p| !self.preview(*p))?, usize::MAX) } else { (item, row) };
        let rows = self.of(item);
        let at = match &rows[row.min(rows.len() - 1)] {
            NoteRow::Blank => None,
            NoteRow::Text(r) => Some(r.start),
        };
        Some(NoteAnchor { chunk: item.0, block: item.1, at })
    }
}

/// The reader's place in the notes (plan §F, UI-local): following the end, or held at an anchor.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct NoteScroll {
    held: Option<NoteAnchor>,
}

/// The notes' top row while scrolled: a committed block — its chunk and its place in the chunk —
/// and the byte of its text where the row begins (none: the blank row before it). No row number
/// and no width is kept, so a rewrap at any width puts the same words at the top.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NoteAnchor {
    pub(crate) chunk: usize,
    pub(crate) block: usize,
    pub(crate) at: Option<usize>,
}

impl NoteScroll {
    /// Whether the reader has left the notes' end.
    pub(crate) fn scrolled(&self) -> bool {
        self.held.is_some()
    }

    #[cfg(test)]
    pub(crate) fn anchor(&self) -> Option<NoteAnchor> {
        self.held
    }

    /// One reading move through the notes in `body`, as last drawn: as the transcript's (up leaves
    /// the end, down to it follows again, Esc goes back). Returns whether anything changed; the
    /// notes are only read, never parsed.
    pub(crate) fn apply(&mut self, m: Move, v: &View, preview: &Preview, body: Rect) -> bool {
        if m == Move::Live {
            return self.held.take().is_some();
        }
        let Some((width, height)) = text_size(body) else { return false };
        let page = height.saturating_sub(2).max(1);
        let rows = NoteRows { chunks: &v.notes.chunks, preview: preview.blocks(&v.notes), width };
        let k = match m {
            Move::PageUp | Move::PageDown => page,
            _ => m.rows(),
        };
        match (m, self.held) {
            (Move::Up(_) | Move::PageUp, held) => {
                let at = match held {
                    Some(a) => match rows.find(a) {
                        Some(at) => at,
                        None => return self.held.take().is_some(),
                    },
                    None => match rows.live_top(height) {
                        Some(top) if top.1 > 0 || rows.prev(top.0).is_some() => top,
                        _ => return false, // it all fits: nothing above
                    },
                };
                let Some(anchor) = rows.anchor(rows.up(at, k)) else { return false };
                let moved = Some(anchor) != self.held;
                self.held = Some(anchor);
                moved
            }
            (Move::Down(_) | Move::PageDown, Some(a)) => {
                let Some(at) = rows.find(a) else { return self.held.take().is_some() };
                let to = rows.down(at, k);
                if rows.preview(to.0) || rows.ends_within(to, height) {
                    self.held = None;
                    return true;
                }
                let Some(anchor) = rows.anchor(to) else { return false };
                self.held = Some(anchor);
                anchor != a
            }
            (Move::Down(_) | Move::PageDown, None) | (Move::Live, _) => false,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The activity overlay (Task 10): the ring's records, newest last, each under its time with the
// notice line's mark, label and detail. The ring holds at most 500 records, so the overlay wraps
// all of them when it draws — bounded work, unlike the notes.

/// What the overlay says first once the ring has let records go.
pub(crate) const OLDER_GONE: &str = "Older notices are not kept";

/// The reader's place in the activity: following the newest, or held at a record — by the number
/// the ring gives it, which a newer record never changes — and a row within it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct ActivityScroll {
    held: Option<(u64, usize)>,
}

/// A record's mark and its style, by the notice kind: the frame's own (the notice line's).
pub(crate) type Sign<'a> = &'a dyn Fn(&str) -> (&'a str, Style);

/// One overlay row: a record's number (the truncation line is the number before the first kept),
/// the row within it, and the stretch of its text.
struct ActivityRow {
    id: u64,
    row: usize,
    text: Range<usize>,
}

/// A record's text as the overlay sets it: `label  detail`, or the busy text alone.
fn record_text(a: &super::state::Activity) -> String {
    if a.kind == "dim" {
        a.detail.clone()
    } else {
        format!("{}  {}", a.label, a.detail)
    }
}

fn activity_rows(ring: &super::state::Ring, width: usize, sign: Sign) -> Vec<ActivityRow> {
    let mut rows = Vec::new();
    if ring.wrapped() {
        rows.push(ActivityRow { id: ring.first() - 1, row: 0, text: 0..0 });
    }
    for (k, a) in ring.iter().enumerate() {
        let hang = sign(a.kind).0.width() + 1;
        for (row, text) in wrap(&record_text(a), width.saturating_sub(hang).max(1)).into_iter().enumerate() {
            rows.push(ActivityRow { id: ring.first() + k as u64, row, text });
        }
    }
    rows
}

impl ActivityScroll {
    /// The index of the top row: the held one, or the last screenful while following.
    fn top(&self, rows: &[ActivityRow], height: usize) -> usize {
        match self.held {
            Some((id, row)) => rows.iter().position(|r| (r.id, r.row) >= (id, row)).unwrap_or(0),
            None => rows.len().saturating_sub(height),
        }
    }

    /// One reading move in the overlay's `body`: as the panes' (up leaves the newest; down to the
    /// end follows again). New records arriving while held move nothing.
    pub(crate) fn apply(&mut self, m: Move, ring: &super::state::Ring, body: Rect, sign: Sign) -> bool {
        if m == Move::Live {
            return self.held.take().is_some();
        }
        let Some((width, height)) = text_size(body) else { return false };
        let rows = activity_rows(ring, width, sign);
        let top = self.top(&rows, height);
        let k = match m {
            Move::PageUp | Move::PageDown => height.saturating_sub(2).max(1),
            _ => m.rows(),
        };
        let to = match m {
            Move::Up(_) | Move::PageUp => top.saturating_sub(k),
            _ => top + k,
        };
        if to + height >= rows.len() {
            return self.held.take().is_some();
        }
        let held = Some((rows[to].id, rows[to].row));
        let moved = held != self.held;
        self.held = held;
        moved
    }
}

/// The activity into `body`: records under their times, marked as the notice line marks them, the
/// label bold and the detail after it; busy text dim. First, once records have gone, a line saying so.
pub(crate) fn activity(buf: &mut Buffer, body: Rect, ring: &super::state::Ring, scroll: &ActivityScroll, sign: Sign) {
    let Some((width, height)) = text_size(body) else { return };
    let rows = activity_rows(ring, width, sign);
    if rows.is_empty() {
        buf.set_stringn(body.x, body.y, "Nothing has happened yet.", body.width as usize, DIM);
        return;
    }
    let (x, text_x) = (body.x, body.x + LANE as u16);
    let records: Vec<&super::state::Activity> = ring.iter().collect();
    for (y, r) in (body.y..body.bottom()).zip(&rows[scroll.top(&rows, height)..]) {
        let Some(a) = r.id.checked_sub(ring.first()).and_then(|k| records.get(k as usize)) else {
            buf.set_stringn(x, y, OLDER_GONE, body.width as usize, DIM);
            continue;
        };
        let (glyph, style) = sign(a.kind);
        if r.row == 0 {
            buf.set_stringn(x, y, a.at.format("%H:%M:%S").to_string(), TIME, DIM);
            buf.set_stringn(text_x, y, glyph, width, style);
        }
        let at = text_x + glyph.width() as u16 + 1;
        let room = (body.right().saturating_sub(at)) as usize;
        let text = record_text(a);
        let bold = if a.kind == "dim" { 0 } else { a.label.len() };
        let (b, rest) = (r.text.start.min(bold).max(r.text.start), r.text.end.min(bold).max(r.text.start));
        let body_style = if a.kind == "dim" { DIM } else { Style::new() };
        let (after, _) = buf.set_stringn(at, y, drawn(&text[b..rest]), room, Style::new().add_modifier(Modifier::BOLD));
        buf.set_stringn(after, y, drawn(&text[rest..r.text.end]), room.saturating_sub((after - at) as usize), body_style);
    }
}

// ---------------------------------------------------------------------------------------------
// The slides pane (Task 11): the capture state, then the registered slides, newest last. Textual
// only — no thumbnails (plan §M) — each slide its two rows: the time it was shown and `Slide N`,
// then how it came to be (auto, auto while it still changes, or the person's own). The reader's
// place is a slide's canonical index, never a row number, so a resize (a slide's rows never
// rewrap) keeps it exactly, and a slide refreshed in place moves nothing.

/// A slide's rows in the pane: its label, then how it came to be.
const SLIDE_ROWS: usize = 2;

/// One drawn row of the capture block: its parts, already clipped by the draw.
type BlockRow = Vec<(String, Style)>;

/// The reader's place in the slides (plan §F per-pane scroll, UI-local): following the newest,
/// or held at a slide's canonical index and the row within it.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SlidesScroll {
    held: Option<SlidesHeld>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct SlidesHeld {
    /// The slide's registration index: durable, unlike a row number.
    index: u32,
    /// 0: the label row; 1: the provenance row.
    row: usize,
    /// Slides the view held when the reader left the newest: every one after is new below.
    seen: usize,
}

impl SlidesScroll {
    /// Whether the reader has left the newest slide.
    #[cfg(test)]
    pub(crate) fn following(&self) -> bool {
        self.held.is_none()
    }

    /// The anchor, while scrolled.
    #[cfg(test)]
    pub(crate) fn anchor(&self) -> Option<(u32, usize)> {
        self.held.map(|h| (h.index, h.row))
    }

    /// Slides registered below since the reader left the newest; none while following.
    pub(crate) fn unseen(&self, v: &View) -> Option<usize> {
        self.held.map(|h| v.slides.len().saturating_sub(h.seen))
    }

    /// Where a slide's index is in the view's ordered list, as its first flat row.
    fn at(v: &View, index: u32) -> Option<usize> {
        v.slides.iter().position(|s| s.index == index).map(|p| p * SLIDE_ROWS)
    }

    /// One reading move through the slides in `list`, as it was last drawn: as the panes' (up
    /// leaves the newest, down to it follows again, Esc goes back). Returns whether anything moved.
    pub(crate) fn apply(&mut self, m: Move, v: &View, list: Rect) -> bool {
        if m == Move::Live {
            return self.held.take().is_some();
        }
        let (height, total) = (list.height as usize, v.slides.len() * SLIDE_ROWS);
        if height == 0 || total == 0 {
            return false;
        }
        let live_top = total.saturating_sub(height);
        let k = match m {
            Move::PageUp | Move::PageDown => height.saturating_sub(2).max(1),
            _ => m.rows(),
        };
        let held = match m {
            Move::Up(_) | Move::PageUp => {
                let here = match self.held {
                    Some(h) => Self::at(v, h.index).map(|r| r + h.row).unwrap_or(live_top),
                    None if live_top == 0 => return false, // it all fits: nothing above
                    None => live_top,
                };
                let to = here.saturating_sub(k);
                let held = Some(SlidesHeld { index: v.slides[to / SLIDE_ROWS].index, row: to % SLIDE_ROWS, seen: self.held.map_or(v.slides.len(), |h| h.seen) });
                if self.held == held {
                    return false;
                }
                held
            }
            Move::Down(_) | Move::PageDown => {
                let Some(h) = self.held else { return false };
                let Some(here) = Self::at(v, h.index).map(|r| r + h.row) else { return self.held.take().is_some() };
                let to = (here + k).min(total - 1);
                if to >= live_top {
                    self.held = None;
                    return true;
                }
                let held = Some(SlidesHeld { index: v.slides[to / SLIDE_ROWS].index, row: to % SLIDE_ROWS, seen: h.seen });
                if self.held == held {
                    return false;
                }
                held
            }
            Move::Live => unreachable!("taken above"),
        };
        self.held = held;
        true
    }
}

/// The capture state as the pane heads with it (plan §H): the teal live edge while a window is
/// watched, the attention word in signal red while capture needs the person, the Ctrl-S row the
/// state allows — wrapped to the pane's width, never boxed.
fn capture_block(state: &CaptureState, words: &capture::Words, action: &capture::Action, width: usize, ink: &Ink, signal: Style) -> Vec<BlockRow> {
    let mut rows: Vec<BlockRow> = Vec::new();
    let plain = Style::new();
    // One logical line: a lead (a glyph or an attention word) then its text, wrapped beside it on
    // the first row and two cells in below — a small block that keeps the pane's narrow width. A
    // glyph lead takes one space, as the transcript's live edge does; a word takes two.
    let line = |rows: &mut Vec<BlockRow>, lead: Vec<(&str, Style)>, text: String, style: Style| {
        let lead_w: usize = lead.iter().map(|(s, _)| s.width()).sum();
        let gap = if lead_w <= 2 { 1 } else { 2 };
        let beside = lead_w + gap;
        if lead_w > 0 && beside > width / 2 {
            // a lead that would leave no room beside it (the narrow pane): its own row, then the
            // text at the pane's full width
            rows.push(lead.iter().map(|(s, st)| (s.to_string(), *st)).collect());
            for r in wrap(&text, width.max(1)) {
                rows.push(vec![(text[r].to_string(), style)]);
            }
            return;
        }
        for (i, r) in wrap(&text, width.saturating_sub(beside.max(2)).max(1)).into_iter().enumerate() {
            let mut row: BlockRow = Vec::new();
            if i == 0 && lead_w > 0 {
                row.extend(lead.iter().map(|(s, st)| (s.to_string(), *st)));
                row.push((" ".repeat(gap), plain));
            } else if lead_w > 0 {
                row.push(("  ".to_string(), plain));
            }
            row.push((text[r].to_string(), style));
            rows.push(row);
        }
    };
    match state {
        CaptureState::Watching { window } => {
            line(&mut rows, vec![(ink.edge, ink.teal)], format!("Watching {window}"), plain);
            if matches!(action, capture::Action::Now) {
                rows.push(ctrl_s("capture now"));
            }
        }
        CaptureState::Unbound => {
            line(&mut rows, vec![], format!("No Zoom window chosen for {} yet: choose it once in the LectureLive app.", words.course), plain);
            line(&mut rows, vec![], "Screenshots (⌘⇧4) still become slides.".into(), DIM);
        }
        CaptureState::Paused { window, reason } => line(&mut rows, vec![("Paused", signal)], format!("{window}: {reason}"), plain),
        CaptureState::Asking { reason, .. } => {
            line(&mut rows, vec![("Asking", signal)], reason.clone(), plain);
            if matches!(action, capture::Action::Watch { .. }) {
                rows.push(ctrl_s("watch it"));
            }
        }
        CaptureState::Denied => {
            line(&mut rows, vec![(ink.warn, signal)], format!("Screen Recording is off for {}", words.host), signal);
            line(&mut rows, vec![], "System Settings → Privacy & Security → Screen & System Audio Recording,".into(), DIM);
            line(&mut rows, vec![], format!("then quit and reopen {}.", words.host), DIM);
        }
        CaptureState::Failing { window, reason } => line(&mut rows, vec![("Capture failing", signal)], format!("{window}: {reason}"), plain),
    }
    rows
}

/// The Ctrl-S row, in the footer's chord vocabulary: the chord bold, what it does dim.
fn ctrl_s(what: &str) -> BlockRow {
    vec![("  ".into(), Style::new()), ("^S".into(), Style::new().add_modifier(Modifier::BOLD)), (" ".into(), Style::new()), (what.into(), DIM)]
}

/// Draws the slides pane into `body` — the capture state, a breath, then the registered slides,
/// following the newest or scrolled from the anchor — and says where the list went, for the
/// reading keys: none when the capture block left no room.
pub(crate) fn slides(buf: &mut Buffer, body: Rect, v: &View, scroll: &SlidesScroll, action: &capture::Action, words: &capture::Words, ink: &Ink, signal: Style) -> Option<Rect> {
    if body.height == 0 {
        return None;
    }
    let mut y = body.y;
    let block = v.capture.as_ref().map(|s| capture_block(s, words, action, body.width as usize, ink, signal)).unwrap_or_default();
    for row in &block {
        if y >= body.bottom() {
            return None;
        }
        let mut x = body.x;
        for (text, style) in row {
            let (after, _) = buf.set_stringn(x, y, text, (body.right().saturating_sub(x)) as usize, *style);
            x = after;
        }
        y += 1;
    }
    if !block.is_empty() && y < body.bottom() {
        y += 1; // a breath between the state and the slides
    }
    let list = Rect::new(body.x, y, body.width, body.bottom().saturating_sub(y));
    let total = v.slides.len() * SLIDE_ROWS;
    if total == 0 {
        if block.is_empty() {
            buf.set_stringn(body.x, body.y, "No slides yet.", body.width as usize, DIM);
        }
        return None;
    }
    let live_top = total.saturating_sub(list.height as usize);
    let start = match scroll.held {
        Some(h) => SlidesScroll::at(v, h.index).map(|r| r + h.row).unwrap_or(live_top),
        None => live_top,
    };
    for (row, y) in (start..total).zip(list.y..list.bottom()) {
        let s = &v.slides[row / SLIDE_ROWS];
        if row % SLIDE_ROWS == 0 {
            buf.set_stringn(list.x, y, s.shown_at.format("%H:%M:%S").to_string(), TIME, DIM);
            let (after, _) = buf.set_stringn(list.x + LANE as u16, y, ink.slide, 2, ink.teal);
            buf.set_stringn(after + 1, y, format!("Slide {}", s.index), (list.right().saturating_sub(after + 1)) as usize, Style::new());
        } else {
            // the provenance under the label row, as the transcript's `recovered` sits under its time
            let how = match (s.auto, s.uncertain) {
                (true, false) => "auto",
                (true, true) => "auto / unsettled",
                _ => "manual",
            };
            buf.set_stringn(list.x, y, how, list.width as usize, DIM);
        }
    }
    Some(list)
}

/// The time a slide embed was shown: the registered slide whose file it names (by its path, or its
/// file name when the notes live in another folder); none when no registered slide matches.
fn slide_time(v: &View, dest: &str) -> Option<String> {
    let name = std::path::Path::new(dest).file_name()?;
    v.slides.iter().find(|s| s.file == dest || std::path::Path::new(&s.file).file_name() == Some(name)).map(|s| s.shown_at.format("%H:%M:%S").to_string())
}

/// The notes into `body`: from the anchor down while scrolled, else following their end — the
/// committed chunks under their times, then the preview being written on the live edge (`writing`
/// in the gutter, the edge on its every row, its text dim) so it is never taken for notes. Empty,
/// it says so.
pub(crate) fn notes(buf: &mut Buffer, body: Rect, v: &View, preview: &Preview, scroll: &NoteScroll, ink: &Ink) {
    let Some((width, height)) = text_size(body) else { return };
    let rows = NoteRows { chunks: &v.notes.chunks, preview: preview.blocks(&v.notes), width };
    let top = scroll.held.and_then(|a| rows.find(a)).or_else(|| rows.live_top(height));
    let Some((mut item, mut row)) = top else {
        buf.set_stringn(body.x, body.y, "Nothing written yet.", body.width as usize, DIM);
        return;
    };
    let (x, text_x) = (body.x, body.x + LANE as u16);
    let mut y = body.y;
    loop {
        let Some(b) = rows.block(item) else { break };
        let separated = rows.separated(item);
        let preview = rows.preview(item);
        for (i, r) in rows.of(item).into_iter().enumerate().skip(row) {
            if y >= body.bottom() {
                return;
            }
            let first = i == usize::from(separated);
            if preview {
                if !(separated && i == 0) {
                    buf.set_stringn(x + TIME as u16, y, ink.edge, 1, ink.teal);
                }
                if first && item.1 == 0 {
                    buf.set_stringn(x, y, "writing", TIME - 1, ink.teal);
                }
            } else if first && item.1 == 0 {
                if let Some(t) = &rows.chunks[item.0].time {
                    buf.set_stringn(x, y, t, TIME, DIM);
                }
            }
            if let NoteRow::Text(range) = r {
                note_row(buf, (text_x, y), width, &rows, b, range, first, preview, v, ink);
            }
            y += 1;
        }
        row = 0;
        match rows.next(item) {
            Some(n) if y < body.bottom() => item = n,
            _ => break,
        }
    }
}

/// One row of a block's text at `(x, y)` in a lane `width` wide: its quote rules, its lead on the
/// first row, then the text by its marks — headings and table heads bold, code dim, emphasis
/// italic — all of it dim in the preview.
#[allow(clippy::too_many_arguments)]
fn note_row(buf: &mut Buffer, (x, y): (u16, u16), width: usize, rows: &NoteRows, b: &Block, range: Range<usize>, first: bool, preview: bool, v: &View, ink: &Ink) {
    let over = if preview { DIM } else { Style::new() };
    for q in 0..b.quote as usize {
        if q * 2 < width {
            buf.set_stringn(x + (q * 2) as u16, y, ink.quote, 1, DIM);
        }
    }
    let (hang, lead) = rows.geometry(b);
    if let (true, Some((at, number))) = (first, lead) {
        let mark = if number.is_empty() { ink.bullet } else { number.as_str() };
        buf.set_stringn(x + at as u16, y, mark, width.saturating_sub(at), DIM);
    }
    let end = x + width as u16;
    let mut at = x + hang as u16;
    match &b.kind {
        Kind::Slide { dest } => {
            let (after, _) = buf.set_stringn(at, y, ink.slide, (end.saturating_sub(at)) as usize, ink.teal.patch(over));
            let (after, _) = buf.set_stringn(after, y, format!(" {}", b.text), (end.saturating_sub(after)) as usize, over);
            if let Some(t) = slide_time(v, dest) {
                buf.set_stringn(after, y, format!("  {t}"), (end.saturating_sub(after)) as usize, DIM);
            }
        }
        Kind::Rule => {
            buf.set_stringn(at, y, ink.rule.repeat(width.saturating_sub(hang)), (end.saturating_sub(at)) as usize, DIM);
        }
        kind => {
            let base = match kind {
                Kind::Heading(_) | Kind::Row { head: true } => Style::new().add_modifier(Modifier::BOLD),
                Kind::Code(_) => DIM,
                _ => Style::new(),
            }
            .patch(over);
            // one span per change of mark inside the row
            let mut from = range.start;
            let bounds = b.marks.iter().map(|(at, _)| *at).filter(|at| *at > range.start && *at < range.end).chain([range.end]);
            for to in bounds {
                let m = b.mark_at(from);
                let mut style = base;
                if m.bold {
                    style = style.add_modifier(Modifier::BOLD);
                }
                if m.italic {
                    style = style.add_modifier(Modifier::ITALIC);
                }
                if m.code {
                    style = style.add_modifier(Modifier::DIM);
                }
                let (after, _) = buf.set_stringn(at, y, drawn(&b.text[from..to]), (end.saturating_sub(at)) as usize, style);
                (at, from) = (after, to);
            }
        }
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

    // ---- the notes (Task 9) -------------------------------------------------------------------

    /// Notes shaped as LectureLive writes them: a title, then chunks under their markers.
    pub(crate) const NOTES: &str = "# Statistics — Week 04\n\n<!-- 10:39:12 -->\n## Sampling distributions\n### Standard error\n\
- Larger samples reduce **standard error**.\n  - by \\( \\sigma / \\sqrt{n} \\)\n- Distinct from the *spread* of `observations`.\n\n\
![Slide 17](slides/slide_17_103941.png)\n\n> the lecturer's aside\n\n```\nse = sd / sqrt(n)\n    indented\n```\n\n\
| n | se |\n|---|---|\n| 25 | 2.0 |\n\n<!-- 10:41:52 -->\n## Sample size\n- 標本サイズ 🎓\n";

    const NOTE_INK: Ink<'static> = Ink { edge: "▎", bullet: "•", quote: "│", slide: "▣", rule: "─", warn: "▲", teal: TEAL };

    fn notes_view(doc: &str) -> View {
        use crate::tui::hydrate::NotesSnapshot;
        let mut v = View::new(identity(), Hydration::empty(), Vec::new());
        v.merge(Hydration { notes: NotesSnapshot::At { revision: 1, document: doc.into() }, ..Hydration::empty() });
        v.reduce(&Event::Slide { index: 17, file: "slides/slide_17_103941.png".into(), auto: true, uncertain: false, shown_at: Local.with_ymd_and_hms(2026, 9, 26, 10, 39, 41).unwrap() }, at(0));
        v
    }

    fn draw_notes(v: &View, p: &Preview, width: u16, height: u16) -> (Buffer, Vec<String>) {
        let mut b = Buffer::empty(Rect::new(0, 0, width, height));
        let area = b.area;
        notes(&mut b, area, v, p, &NoteScroll::default(), &NOTE_INK);
        (b.clone(), (0..height).map(|y| row_text(&b, y)).collect())
    }

    /// A buffer row as it reads: a wide glyph once, not followed by the cell it covers.
    fn row_text(b: &Buffer, y: u16) -> String {
        let (mut s, mut x) = (String::new(), 0);
        while x < b.area.width {
            let sym = b[(x, y)].symbol();
            s.push_str(sym);
            x += (sym.width() as u16).max(1);
        }
        s.trim_end().to_string()
    }

    fn parsed(v: &View) -> Preview {
        let mut p = Preview::default();
        p.refresh(&v.notes, Instant::now());
        p
    }

    /// Plan §H's mapping, row by row: the title untimed; each chunk's time on its first row; `##`
    /// and `###` bold with a blank row before a chunk; bullets with a hanging indent, nested by two;
    /// the slide embed as its label and time; the quote on its rule; code indented; a table row as
    /// cells two spaces apart; TeX and Unicode as they are.
    #[test]
    fn committed_notes_draw_as_restrained_markdown() {
        let v = notes_view(NOTES);
        let (_, l) = draw_notes(&v, &Preview::default(), 60, 20);
        assert_eq!(
            l[..17],
            [
                "          Statistics — Week 04",
                "",
                "10:39:12  Sampling distributions",
                "          Standard error",
                "          • Larger samples reduce standard error.",
                "            • by \\( \\sigma / \\sqrt{n} \\)",
                "          • Distinct from the spread of observations.",
                "          ▣ Slide 17  10:39:41",
                "          │ the lecturer's aside",
                "            se = sd / sqrt(n)",
                "                indented",
                "          n  se",
                "          25  2.0",
                "",
                "10:41:52  Sample size",
                "          • 標本サイズ 🎓",
                "",
            ]
        );
        // wrapped, a bullet's text hangs after its bullet
        let (_, l) = draw_notes(&v, &Preview::default(), 36, 30);
        let r = l.iter().position(|r| r.contains("• Larger")).unwrap();
        assert_eq!(l[r..r + 2], ["          • Larger samples reduce", "            standard error."]);
    }

    /// The styles, as cells: time dim; headings bold; bullets dim; bold, italic and inline code as
    /// marked; code blocks dim; the quote rule dim, its words ink; the slide mark teal, its time dim.
    #[test]
    fn committed_notes_styles() {
        let v = notes_view(NOTES);
        let (b, l) = draw_notes(&v, &Preview::default(), 60, 20);
        let cell = |row: usize, text: &str| {
            let x = l[row].find(text).map(|i| l[row][..i].chars().map(|c| c.to_string().width()).sum::<usize>()).unwrap_or_else(|| panic!("{text:?} in {:?}", l[row]));
            &b[(x as u16, row as u16)]
        };
        assert_eq!(cell(2, "10:39:12").modifier, Modifier::DIM, "the chunk's time");
        assert_eq!(cell(2, "Sampling").modifier, Modifier::BOLD, "##");
        assert_eq!(cell(3, "Standard").modifier, Modifier::BOLD, "###");
        assert_eq!(cell(0, "Statistics").modifier, Modifier::BOLD, "#");
        assert_eq!(cell(4, "•").modifier, Modifier::DIM, "the bullet");
        assert_eq!(cell(4, "Larger").modifier, Modifier::empty());
        assert_eq!(cell(4, "standard").modifier, Modifier::BOLD);
        assert_eq!(cell(6, "spread").modifier, Modifier::ITALIC);
        assert_eq!(cell(6, "observations").modifier, Modifier::DIM, "inline code");
        let slide = cell(7, "▣");
        assert_eq!((slide.fg, slide.modifier), (ratatui::style::Color::Cyan, Modifier::empty()), "the slide mark teal");
        assert_eq!(cell(7, "Slide").modifier, Modifier::empty());
        assert_eq!(cell(7, "10:39:41").modifier, Modifier::DIM);
        assert_eq!(cell(8, "│").modifier, Modifier::DIM, "the quote rule");
        assert_eq!(cell(8, "the lecturer").modifier, Modifier::empty(), "quoted words in ink");
        assert_eq!(cell(9, "se =").modifier, Modifier::DIM, "a code block");
        assert_eq!(cell(11, "n").modifier, Modifier::BOLD, "the table's head");
        assert_eq!(cell(12, "25").modifier, Modifier::empty());
        assert!(b.content.iter().all(|c| c.bg == ratatui::style::Color::Reset), "no background");
    }

    /// A slide the notes embed that is not registered keeps its label and invents no time.
    #[test]
    fn an_unregistered_slide_has_no_time() {
        let mut v = View::new(identity(), Hydration::empty(), Vec::new());
        v.merge(Hydration { notes: crate::tui::hydrate::NotesSnapshot::At { revision: 1, document: "<!-- 10:00:00 -->\n![Slide 9](slides/slide_09_100000.png)\n".into() }, ..Hydration::empty() });
        assert_eq!(draw_notes(&v, &Preview::default(), 60, 2).1[0], "10:00:00  ▣ Slide 9");
        // and one registered under the notes' folder matches by its file name
        v.reduce(&Event::Slide { index: 9, file: "../slides/slide_09_100000.png".into(), auto: false, uncertain: false, shown_at: at(61) }, at(0));
        assert_eq!(draw_notes(&v, &Preview::default(), 60, 2).1[0], "10:00:00  ▣ Slide 9  10:01:01");
    }

    /// Following, the newest material sits at the bottom; empty, the pane says so.
    #[test]
    fn the_notes_follow_their_end() {
        let v = notes_view(NOTES);
        let (_, l) = draw_notes(&v, &Preview::default(), 60, 5);
        assert_eq!(l[3..], ["10:41:52  Sample size", "          • 標本サイズ 🎓"]);
        let empty = View::new(identity(), Hydration::empty(), Vec::new());
        assert_eq!(draw_notes(&empty, &Preview::default(), 40, 2).1, ["Nothing written yet.", ""]);
    }

    /// The preview on the live edge: `writing` teal in the gutter of its first row, the teal edge
    /// on every one of its rows, its words dim — under the committed notes, with no time.
    #[test]
    fn the_preview_is_written_on_the_live_edge() {
        let mut v = notes_view(NOTES);
        v.reduce(&Event::Preview("## Confidence intervals\n- An interval that covers the mean in **95%** of samples ".into()), at(0));
        let p = parsed(&v);
        let (b, l) = draw_notes(&v, &p, 50, 10);
        assert_eq!(l[6..], ["", "writing ▎ Confidence intervals", "        ▎ • An interval that covers the mean in", "        ▎   95% of samples"]);
        for y in 7..10 {
            assert_eq!(b[(8, y)].fg, ratatui::style::Color::Cyan, "the edge on row {y}");
            for x in 10..50 {
                let c = &b[(x, y)];
                assert!(c.symbol() == " " || c.modifier.contains(Modifier::DIM), "preview text is dim at {x},{y}: {:?}", c.symbol());
            }
        }
        assert_eq!(b[(8, 6)].symbol(), " ", "the blank before the preview is not its row");
        assert!((0..7).all(|x| b[(x, 7)].fg == ratatui::style::Color::Cyan), "writing, teal");
        assert!(b[(10, 7)].modifier.contains(Modifier::BOLD | Modifier::DIM), "a heading being written: bold, and dim");
    }

    /// Only whole words: the preview shows up to its last whitespace, and nothing before any.
    #[test]
    fn the_preview_shows_only_complete_words() {
        assert_eq!(shown("the half-writ"), "the ");
        assert_eq!(shown("ends clean "), "ends clean ");
        assert_eq!(shown("line\nnext"), "line\n");
        assert_eq!(shown("nospace"), "");
        assert_eq!(shown(""), "");
        assert_eq!(shown("標本 サイ"), "標本 ");
    }

    /// Past the cap only the first 1 MiB is shown, cut at a character and a word, never inside one.
    #[test]
    fn the_preview_display_is_capped() {
        let text = "é ".repeat(PREVIEW_CAP); // 3 bytes each, so the cap falls inside a character
        let s = shown(&text);
        assert!(s.len() <= PREVIEW_CAP && s.len() > PREVIEW_CAP - 4 && s.ends_with(' '));
    }

    /// Plan §F: the preview is parsed at most every 100 ms however fast its deltas come; a new
    /// preview is parsed at once and never shows an earlier one's blocks; an ended one drops at once.
    #[test]
    fn the_preview_is_parsed_at_most_ten_times_a_second() {
        let mut v = notes_view("");
        let mut p = Preview::default();
        let t0 = Instant::now();
        markdown::PARSED.with(|n| n.set(0));
        let mut parses = 0;
        // 500 deltas over one second, one every 2 ms, a frame looked at after each
        for k in 0..500u64 {
            v.reduce(&Event::Preview(format!("w{k} ")), at(0));
            let before = markdown::PARSED.with(|n| n.get());
            let due = p.refresh(&v.notes, t0 + Duration::from_millis(2 * k));
            if markdown::PARSED.with(|n| n.get()) != before {
                parses += 1;
            } else {
                assert!(due.is_some_and(|d| d > t0 + Duration::from_millis(2 * k)), "a skipped parse says when to look again");
            }
        }
        assert!((10..=11).contains(&parses), "{parses} parses in a second");
        // the same preview, nothing new: nothing to do
        let later = t0 + Duration::from_secs(5);
        p.refresh(&v.notes, later);
        let before = markdown::PARSED.with(|n| n.get());
        assert_eq!(p.refresh(&v.notes, later + Duration::from_secs(1)), None);
        assert_eq!(markdown::PARSED.with(|n| n.get()), before);
        // the preview ends: gone at once, whatever the clock
        v.reduce(&Event::NothingNew, at(0));
        assert!(p.blocks(&v.notes).is_empty());
        p.refresh(&v.notes, later);
        // a new one, 1 ms later: parsed at once, and it is its own
        v.reduce(&Event::Preview("fresh words ".into()), at(0));
        assert!(p.blocks(&v.notes).is_empty(), "the old parse is never drawn for the new preview");
        p.refresh(&v.notes, later + Duration::from_millis(1));
        assert_eq!(p.blocks(&v.notes)[0].text, "fresh words");
    }

    /// A draw wraps; it never parses — committed chunks were parsed when they changed, the preview
    /// when it was refreshed.
    #[test]
    fn a_draw_never_parses() {
        let mut v = notes_view(&NOTES.repeat(20));
        v.reduce(&Event::Preview("## being written now ".into()), at(0));
        let p = parsed(&v);
        markdown::PARSED.with(|n| n.set(0));
        for (w, h) in [(140, 40), (60, 10), (80, 24)] {
            draw_notes(&v, &p, w, h);
        }
        assert_eq!(markdown::PARSED.with(|n| n.get()), 0);
    }

    /// Tiny and odd bodies draw nothing past their edge and never panic.
    #[test]
    fn notes_in_narrow_bodies_never_panic() {
        let mut v = notes_view(&(NOTES.to_string() + "- a\n  - b\n    - c\n      - d\n        - e\n          - f\n            - g\n              - h\n"));
        v.reduce(&Event::Preview("> > > deep\n1. one ".into()), at(0));
        let p = parsed(&v);
        for (w, h) in [(0, 0), (1, 1), (10, 3), (11, 3), (12, 5), (20, 30), (0, 5), (40, 0)] {
            let mut b = Buffer::empty(Rect::new(0, 0, w.max(1), h.max(1)));
            notes(&mut b, Rect::new(0, 0, w, h), &v, &p, &NoteScroll::default(), &NOTE_INK);
        }
    }

    /// The notes' place is semantic: a block and a byte of its text. A resize rewraps from the
    /// same words; up and back down to the end follows again; a document replaced whole (a polish)
    /// returns the reader to live rather than pointing into what is gone.
    #[test]
    fn notes_scroll_holds_its_words_across_resize_and_replacement() {
        let v = notes_view(&NOTES.repeat(6));
        let p = Preview::default();
        let mut s = NoteScroll::default();
        let body = |w, h| Rect::new(0, 0, w, h);
        assert!(!s.apply(Move::Down(1), &v, &p, body(80, 12)), "following: nothing below");
        assert!(s.apply(Move::Up(20), &v, &p, body(80, 12)));
        let held = s.anchor().unwrap();
        let draw = |s: &NoteScroll, w: u16, h: u16| {
            let mut b = Buffer::empty(Rect::new(0, 0, w, h));
            let area = b.area;
            notes(&mut b, area, &v, &p, s, &NOTE_INK);
            (0..h).map(|y| row_text(&b, y)).collect::<Vec<_>>()
        };
        let at80 = draw(&s, 80, 12);
        let at140 = draw(&s, 140, 30);
        let top = |rows: &[String]| rows[0].trim_start().chars().take(12).collect::<String>();
        assert_eq!(top(&at80), top(&at140), "the same words on top at either width");
        assert_eq!(draw(&s, 80, 12), at80, "140 → 80: the same rows");
        assert_eq!(s.anchor(), Some(held), "a resize moves no anchor");
        // down to the end follows again
        for _ in 0..200 {
            s.apply(Move::PageDown, &v, &p, body(80, 12));
        }
        assert!(!s.scrolled());
        // held while a commit appends below: the place does not move
        s.apply(Move::PageUp, &v, &p, body(80, 12));
        let before = draw(&s, 80, 12);
        let mut more = notes_view(&NOTES.repeat(6));
        more.reduce(&Event::Committed { words: 1, slides: 0, block: "\n<!-- 11:00:00 -->\n## Appended\n".into(), usd: 0.0, confirmed: true, removed: 0, missing: 0, revision: 2 }, at(0));
        let mut b = Buffer::empty(Rect::new(0, 0, 80, 12));
        let area = b.area;
        notes(&mut b, area, &more, &p, &s, &NOTE_INK);
        assert_eq!((0..12).map(|y| row_text(&b, y)).collect::<Vec<_>>(), before);
        // a polished document replaces them all: an anchor into a chunk that is gone goes live
        let mut polished = notes_view("# Polished\n\n<!-- 09:00:00 -->\n## Short now\n");
        polished.reduce(&Event::Polished { backup: "b.md".into(), usd: 0.0, revision: 3 }, at(0));
        let mut far = s.clone();
        far.held = Some(NoteAnchor { chunk: 40, block: 3, at: Some(10) });
        let mut b = Buffer::empty(Rect::new(0, 0, 80, 12));
        let area = b.area;
        notes(&mut b, area, &polished, &p, &far, &NOTE_INK);
        assert!((0..12).any(|y| row_text(&b, y).contains("Short now")), "drawn from the live end, not from memory");
        assert!(far.apply(Move::Up(1), &polished, &p, body(80, 12)) || !far.scrolled());
        // a block index past its chunk's end holds at the chunk's last block
        let rows = NoteRows { chunks: &v.notes.chunks, preview: &[], width: 70 };
        let (item, _) = rows.find(NoteAnchor { chunk: 1, block: 999, at: Some(0) }).unwrap();
        assert_eq!(item, (1, v.notes.chunks[1].blocks.len() - 1));
    }

    /// The preview is no place to hold: a move that would anchor in it holds at the last
    /// committed row, and a move down into it follows.
    #[test]
    fn the_preview_is_never_an_anchor() {
        let mut v = notes_view(NOTES);
        v.reduce(&Event::Preview("## being written\n- a\n- b\n- c\n- d\n- e\n- f ".into()), at(0));
        let p = parsed(&v);
        let mut s = NoteScroll::default();
        assert!(s.apply(Move::Up(1), &v, &p, Rect::new(0, 0, 60, 6)));
        let a = s.anchor().unwrap();
        assert!(a.chunk < v.notes.chunks.len(), "{a:?}");
        assert!(s.apply(Move::Down(1), &v, &p, Rect::new(0, 0, 60, 6)) && !s.scrolled());
    }

    // ---- the slides pane (Task 11) -------------------------------------------------------------

    use lecturelive_core::capture::detect::Region;
    use lecturelive_core::capture::select::{Descriptor, Selection, SizedRegion};
    use lecturelive_core::capture::window::WindowInfo;
    use lecturelive_core::capture::worker::CaptureState;

    /// Zoom's meeting window, saved with a region at each of its two sizes (as in capture.rs).
    fn saved_selection() -> Selection {
        Selection {
            descriptor: Descriptor { bundle_id: Some("us.zoom.xos".into()), app: "zoom.us".into(), title: "Zoom Meeting".into(), width: 1600, height: 900 },
            region: Region { x: 0.1, y: 0.1, w: 0.8, h: 0.8 },
            leave_out: vec![],
            sizes: vec![SizedRegion { width: 1280, height: 720, region: Region { x: 0.05, y: 0.12, w: 0.9, h: 0.7 } }],
        }
    }

    fn words() -> crate::capture::Words {
        crate::capture::Words { course: identity().course, host: "Terminal" }
    }

    const SIGNAL: Style = Style::new().fg(ratatui::style::Color::Red);

    fn draw_slides(v: &View, s: &SlidesScroll, action: &crate::capture::Action, width: u16, height: u16) -> (Buffer, Vec<String>) {
        let mut b = Buffer::empty(Rect::new(0, 0, width, height));
        let area = b.area;
        slides(&mut b, area, v, s, action, &words(), &NOTE_INK, SIGNAL);
        (b.clone(), (0..height).map(|y| row_text(&b, y)).collect())
    }

    /// A lecture with `n` slides of mixed provenance, watching a window.
    fn slides_view(n: u32, state: CaptureState) -> View {
        let mut v = view(0);
        v.reduce(&Event::Capture(state), at(0));
        for index in 1..=n {
            let (auto, uncertain) = match index % 3 {
                0 => (true, true),
                1 => (false, false),
                _ => (true, false),
            };
            v.reduce(&Event::Slide { index, file: format!("slides/slide_{index:02}_100000.png"), auto, uncertain, shown_at: at(u64::from(index) * 60) }, at(0));
        }
        v
    }

    /// The pane heads with the capture state and lists the slides newest last, each with its
    /// time, its number and how it came to be; empty, it says so.
    #[test]
    fn the_slides_pane_shows_state_then_provenance() {
        let watch = crate::capture::Action::Now;
        let v = slides_view(3, CaptureState::Watching { window: "Zoom Meeting".into() });
        let (b, l) = draw_slides(&v, &SlidesScroll::default(), &watch, 28, 12);
        assert_eq!(
            l[..8],
            [
                "▎ Watching Zoom Meeting",
                "  ^S capture now",
                "",
                "10:01:00  ▣ Slide 1",
                "manual",
                "10:02:00  ▣ Slide 2",
                "auto",
                "10:03:00  ▣ Slide 3",
            ]
        );
        assert_eq!(b[(0, 0)].fg, ratatui::style::Color::Cyan, "the live edge");
        assert!(b[(2, 1)].modifier.contains(Modifier::BOLD), "^S bold");
        assert!(b[(5, 1)].modifier.contains(Modifier::DIM), "capture now dim");
        assert_eq!(b[(0, 3)].modifier, Modifier::DIM, "the time gutter");
        assert_eq!(b[(10, 3)].fg, ratatui::style::Color::Cyan, "the slide glyph teal");
        assert_eq!(b[(10, 3)].modifier, Modifier::empty());
        assert_eq!(b[(0, 4)].modifier, Modifier::DIM, "the provenance dim, under the label row");
        // an unsettled auto says so
        let (_, l) = draw_slides(&slides_view(3, CaptureState::Watching { window: "Zoom Meeting".into() }), &SlidesScroll::default(), &watch, 28, 12);
        assert_eq!(l[8], "auto / unsettled");
        // empty: the placeholder is gone, the state still heads the pane
        let empty = slides_view(0, CaptureState::Watching { window: "Zoom Meeting".into() });
        let (_, l) = draw_slides(&empty, &SlidesScroll::default(), &watch, 28, 4);
        assert_eq!(l[..2], ["▎ Watching Zoom Meeting", "  ^S capture now"]);
        // no state and no slides: the pane says so (the fixture-less goldens)
        let mut none = view(0);
        none.capture = None;
        let (_, l) = draw_slides(&none, &SlidesScroll::default(), &crate::capture::Action::Refused(crate::capture::Refusal::None), 28, 3);
        assert_eq!(l[0], "No slides yet.");
    }

    /// Each attention state heads the pane with its own attention word, and only a safe asking
    /// offers `^S watch it`; denied wraps its instructions inside the pane.
    #[test]
    fn attention_states_head_the_pane_and_only_a_safe_asking_offers_watch() {
        let none = crate::capture::Action::Refused(crate::capture::Refusal::None);
        let asking = |candidates: Vec<WindowInfo>| CaptureState::Asking { window: "Zoom Meeting".into(), reason: "Zoom Meeting is 1280 × 720 now; it was 1600 × 900".into(), candidates };
        let candidate = WindowInfo { id: 42, app: "zoom.us".into(), bundle_id: Some("us.zoom.xos".into()), title: "Zoom Meeting".into(), width: 1280, height: 720, on_screen: true };
        let v = slides_view(1, asking(vec![candidate]));
        let watch = crate::capture::action(v.capture.as_ref(), Some(&saved_selection()));
        let (b, l) = draw_slides(&v, &SlidesScroll::default(), &watch, 28, 10);
        assert!(l[0].starts_with("Asking  Zoom Meeting is"), "{l:?}");
        assert_eq!(l[3], "  ^S watch it");
        assert_eq!(b[(0, 0)].fg, ratatui::style::Color::Red, "the attention word");
        // the same state without a saved region: no Ctrl-S row
        let refused = crate::capture::action(v.capture.as_ref(), None);
        let (_, l) = draw_slides(&v, &SlidesScroll::default(), &refused, 28, 10);
        assert!(!l.iter().any(|r| r.contains("^S")), "{l:?}");
        // paused, failing, unbound
        for (state, first) in [
            (CaptureState::Paused { window: "Zoom Meeting".into(), reason: "the window is minimised".into() }, "Paused  Zoom Meeting: the"),
            (CaptureState::Failing { window: "Zoom Meeting".into(), reason: "3 failed captures in a row".into() }, "Capture failing"),
            (CaptureState::Unbound, "No Zoom window chosen for"),
        ] {
            let v = slides_view(1, state);
            let (_, l) = draw_slides(&v, &SlidesScroll::default(), &none, 28, 10);
            assert!(l[0].starts_with(first), "{:?}: {:?}", v.capture, l);
        }
        // denied: the warning, then the steps, wrapped inside 28 columns
        let v = slides_view(1, CaptureState::Denied);
        let (b, l) = draw_slides(&v, &SlidesScroll::default(), &none, 28, 10);
        assert!(l[0].starts_with("▲ Screen Recording is off"), "{l:?}");
        assert!(l.iter().take(4).any(|r| r.contains("System Settings → Privacy")), "{l:?}");
        assert!(l.iter().take(7).any(|r| r.contains("then quit and reopen")), "{l:?}");
        assert!(l.iter().take(7).any(|r| r.contains("Terminal.")), "{l:?}");
        assert_eq!(b[(0, 0)].fg, ratatui::style::Color::Red);
        assert!(l.iter().all(|r| r.chars().count() <= 28), "nothing runs past the pane: {l:?}");
    }

    /// The slides follow their newest; up leaves the newest and counts what arrives below, Esc
    /// goes back, and the anchor is a slide's index, so a resize keeps the reader's place.
    #[test]
    fn the_slides_follow_scroll_and_keep_their_anchor() {
        let watch = crate::capture::Action::Now;
        let v = slides_view(9, CaptureState::Watching { window: "Zoom Meeting".into() });
        let mut s = SlidesScroll::default();
        let list = Rect::new(0, 3, 28, 8); // below the two-row state and its breath
        let (_, live) = draw_slides(&v, &s, &watch, 28, 11);
        assert!(live[9].contains("Slide 9") && live[9].starts_with("10:09:00"), "the newest at the bottom: {live:?}");
        assert_eq!(live[10].trim(), "auto / unsettled");
        assert!(s.apply(Move::Up(2), &v, list));
        assert!(!s.following());
        assert_eq!(s.anchor().unwrap(), (5, 0), "a slide's index, not a row number");
        let (_, up) = draw_slides(&v, &s, &watch, 28, 11);
        assert!(up[3].contains("Slide 5"), "{up:?}");
        // a new slide below moves nothing and is counted
        let more = slides_view(10, CaptureState::Watching { window: "Zoom Meeting".into() });
        assert_eq!(s.unseen(&more), Some(1));
        let (_, still) = draw_slides(&more, &s, &watch, 28, 11);
        assert_eq!(still[3], up[3], "the reader's place held");
        // a resize: the list is taller, the same slide stays on top
        let (_, at40) = draw_slides(&more, &s, &watch, 40, 18);
        assert!(at40[3].contains("Slide 5"), "the anchor's slide on top at another width: {at40:?}");
        // down to the end follows again; Esc from mid-way goes back
        assert!(s.apply(Move::Down(100), &v, list));
        assert!(s.following());
        s.apply(Move::PageUp, &v, list);
        assert!(!s.following());
        assert_eq!(s.unseen(&v), Some(0));
        assert!(s.apply(Move::Live, &v, list));
        assert!(s.following());
        // a slide refreshed in place (the same index) moves nothing
        let mut refreshed = slides_view(10, CaptureState::Watching { window: "Zoom Meeting".into() });
        s.apply(Move::Up(1), &v, list);
        let held = s.anchor();
        refreshed.reduce(&Event::Slide { index: 7, file: "slides/slide_07_100000.png".into(), auto: false, uncertain: false, shown_at: at(421) }, at(0));
        assert_eq!(s.anchor(), held, "a refresh is not a move");
    }

    /// Tiny bodies draw nothing past their edge and never panic, with or without a state.
    #[test]
    fn slides_in_tiny_bodies_never_panic() {
        for v in [slides_view(3, CaptureState::Watching { window: "Zoom Meeting".into() }), slides_view(3, CaptureState::Denied), slides_view(0, CaptureState::Unbound)] {
            for (w, h) in [(0, 0), (1, 1), (5, 2), (10, 3), (11, 3), (28, 1), (28, 2), (0, 5), (40, 0)] {
                let mut b = Buffer::empty(Rect::new(0, 0, w.max(1), h.max(1)));
                let mut s = SlidesScroll::default();
                slides(&mut b, Rect::new(0, 0, w, h), &v, &s, &crate::capture::Action::Now, &words(), &NOTE_INK, SIGNAL);
                for m in [Move::Up(1), Move::PageUp, Move::Down(1), Move::PageDown, Move::Live] {
                    s.apply(m, &v, Rect::new(0, 0, w, h));
                }
            }
        }
    }

    /// ASCII chrome: the live edge, the warning mark and the slide glyph fall back; the wording
    /// keeps its Unicode (⌘⇧4, →) as the lecture's own words do.
    #[test]
    fn ascii_slides_chrome_falls_back() {
        let ink = Ink { edge: "|", bullet: "-", quote: "|", slide: "[]", rule: "-", warn: "!", teal: TEAL };
        let mut b = Buffer::empty(Rect::new(0, 0, 28, 10));
        let area = b.area;
        let v = slides_view(2, CaptureState::Watching { window: "Zoom Meeting".into() });
        slides(&mut b, area, &v, &SlidesScroll::default(), &crate::capture::Action::Now, &words(), &ink, SIGNAL);
        let l: Vec<String> = (0..10).map(|y| row_text(&b, y)).collect();
        assert!(l[0].starts_with("| Watching Zoom Meeting"), "{l:?}");
        assert!(l[3].contains("[] Slide 1"), "{l:?}");
        let mut b = Buffer::empty(Rect::new(0, 0, 28, 6));
        let area = b.area;
        let v = slides_view(0, CaptureState::Denied);
        slides(&mut b, area, &v, &SlidesScroll::default(), &crate::capture::Action::Refused(crate::capture::Refusal::None), &words(), &ink, SIGNAL);
        assert!(row_text(&b, 0).starts_with("! Screen Recording is off"), "{:?}", row_text(&b, 0));
    }
}
