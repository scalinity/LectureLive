//! The responsive frame (M7 plan §H): one pure [`layout`] of `frame.area()` on every draw into a
//! ladder of five shapes — wide, normal, stacked, narrow, too small — and the chrome drawn into it:
//! the two header rows, the pane headings and tabs, the rules, the notice line, the prompt line and
//! the keys. Border depth 0: one `│` between columns and dim `─` rules. The panes themselves hold
//! placeholders until Tasks 9 and 11 fill them; the transcript is `panes.rs`'s (Task 8).
//!
//! Tokens (plan §H): ink and paper are the terminal's own colours, never set; graphite is `DIM`;
//! teal and signal are truecolour when the terminal says so, else ANSI cyan and red, and nothing at
//! all under `NO_COLOR`, where words, glyphs, bold, dim and reverse still carry every meaning.
//! Chrome glyphs fall back to ASCII when the locale is not UTF-8; lecture text never changes.

use std::time::Duration;

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::Frame;

use lecturelive_core::audio::level::dbfs;
use lecturelive_core::session::coordinator::SttStatus;
use lecturelive_core::session::spend::{self, Paint};

use crate::stop::Stage;

use super::input::{Hint, Overlay};
use super::panes::{self, ActivityScroll, NoteScroll, Preview, Scroll};
use super::state::{Lane, View};

pub(crate) const SUSPEND: &str = "Suspending would stop the recording. Stop the lecture first (Ctrl-C).";

/// The notice for a Ctrl-C the stop controller did not take: a held key repeating, or too soon after the stage before.
pub(crate) fn not_yet(stage: Stage) -> &'static str {
    match stage {
        Stage::StopWaiting => "Wait a moment, then Ctrl-C again to quit at once.",
        _ => "Wait a moment, then Ctrl-C again to stop waiting.",
    }
}

// ---------------------------------------------------------------------------------------------
// Layout: pure, from the area alone.

/// The smallest terminal the lecture view is drawn in; below it, the safety view.
pub(crate) const MIN_WIDTH: u16 = 60;
pub(crate) const MIN_HEIGHT: u16 = 16;
/// The wide shape's slides column.
const SLIDES_WIDTH: u16 = 28;
/// Between two columns: a space, the `│`, a space.
const GAP: u16 = 3;

/// The layout ladder (plan §H), chosen from the terminal's size alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Variant {
    /// ≥132 × ≥28: transcript, notes and slides side by side.
    Wide,
    /// ≥100 × ≥20: transcript and slides tabbed on the left, notes on the right.
    Normal,
    /// 60–99 × ≥36: transcript and slides tabbed above, notes below — a half-screen window.
    Stacked,
    /// 60–99 × 16–35, and ≥100 × 16–19: one pane at a time, tabbed.
    Narrow,
    /// <60 or <16: the safety view.
    TooSmall,
}

pub(crate) fn variant(width: u16, height: u16) -> Variant {
    if width < MIN_WIDTH || height < MIN_HEIGHT {
        Variant::TooSmall
    } else if width >= 132 && height >= 28 {
        Variant::Wide
    } else if width >= 100 && height >= 20 {
        Variant::Normal
    } else if height >= 36 {
        Variant::Stacked // width is 60–99 here: 100 and over at this height is normal
    } else {
        Variant::Narrow
    }
}

/// A reading pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pane {
    Transcript,
    Notes,
    Slides,
}

/// One column of the body: its heading row, its body, and the panes it can show — tabs when there
/// is more than one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Column {
    pub(crate) heading: Rect,
    pub(crate) body: Rect,
    pub(crate) tabs: &'static [Pane],
    /// Its heading is drawn as a rule (stacked's notes): the heading divides the two panes.
    pub(crate) ruled: bool,
}

impl Column {
    /// The pane this column shows: the focused one if it is among its tabs, else its first.
    pub(crate) fn shown(&self, focus: Pane) -> Pane {
        if self.tabs.contains(&focus) {
            focus
        } else {
            self.tabs[0]
        }
    }
}

/// Every rectangle of one frame. Nothing outside this decides where anything is drawn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Layout {
    pub(crate) variant: Variant,
    pub(crate) area: Rect,
    pub(crate) header: [Rect; 2],
    /// Full-width dim rules (wide and normal: under the header and above the notice).
    pub(crate) rules: Vec<Rect>,
    pub(crate) columns: Vec<Column>,
    /// One-cell-wide vertical rules between columns, heading row and body.
    pub(crate) separators: Vec<Rect>,
    pub(crate) notice: Rect,
    pub(crate) prompt: Rect,
    pub(crate) keys: Rect,
}

impl Layout {
    /// Rows that are not a pane's body: the chrome the clutter budget counts (plan §H).
    #[cfg(test)]
    pub(crate) fn chrome_rows(&self) -> u16 {
        let mut rows: Vec<u16> = self.columns.iter().flat_map(|c| c.body.rows().map(|r| r.y)).collect();
        rows.sort_unstable();
        rows.dedup();
        self.area.height - rows.len() as u16
    }
}

/// The frame's rectangles for `area` (plan §H). A one-cell gutter left and right; rows from the top:
/// the two header rows, a rule (wide and normal), the heading or tab row, the body, a rule (wide and
/// normal), the notice, the prompt, the keys. Stacked divides its body with the notes heading
/// instead of rules, and narrow has none.
pub(crate) fn layout(area: Rect) -> Layout {
    let variant = variant(area.width, area.height);
    let mut l = Layout { variant, area, header: [Rect::default(); 2], rules: Vec::new(), columns: Vec::new(), separators: Vec::new(), notice: Rect::default(), prompt: Rect::default(), keys: Rect::default() };
    if variant == Variant::TooSmall {
        return l;
    }
    let (x, w, h) = (area.x + 1, area.width - 2, area.height);
    let row = |y: u16| Rect::new(x, area.y + y, w, 1);
    l.header = [row(0), row(1)];
    l.notice = row(h - 3);
    l.prompt = row(h - 2);
    l.keys = row(h - 1);
    let ruled = matches!(variant, Variant::Wide | Variant::Normal);
    let (heading, end) = if ruled {
        l.rules = vec![row(2), row(h - 4)];
        (3, h - 4)
    } else {
        (2, h - 3)
    };
    let (body_y, body_h) = (area.y + heading + 1, end - heading - 1);
    let column = |x: u16, w: u16, tabs: &'static [Pane]| Column { heading: Rect::new(x, area.y + heading, w, 1), body: Rect::new(x, body_y, w, body_h), tabs, ruled: false };
    let separator = |x: u16| Rect::new(x, area.y + heading, 1, body_h + 1);
    match variant {
        Variant::Wide => {
            let rest = w - SLIDES_WIDTH - 2 * GAP;
            let (t, n) = (rest * 2 / 5, rest - rest * 2 / 5);
            let (xn, xs) = (x + t + GAP, x + t + GAP + n + GAP);
            l.columns = vec![column(x, t, &[Pane::Transcript]), column(xn, n, &[Pane::Notes]), column(xs, SLIDES_WIDTH, &[Pane::Slides])];
            l.separators = vec![separator(xn - 2), separator(xs - 2)];
        }
        Variant::Normal => {
            let left = (w - GAP) * 2 / 5;
            let xn = x + left + GAP;
            l.columns = vec![column(x, left, &[Pane::Transcript, Pane::Slides]), column(xn, w - GAP - left, &[Pane::Notes])];
            l.separators = vec![separator(xn - 2)];
        }
        Variant::Stacked => {
            let top = (body_h - 1) * 2 / 5;
            l.columns = vec![
                Column { heading: row(heading), body: Rect::new(x, body_y, w, top), tabs: &[Pane::Transcript, Pane::Slides], ruled: false },
                Column { heading: Rect::new(x, body_y + top, w, 1), body: Rect::new(x, body_y + top + 1, w, body_h - top - 1), tabs: &[Pane::Notes], ruled: true },
            ];
        }
        Variant::Narrow => l.columns = vec![column(x, w, &[Pane::Transcript, Pane::Notes, Pane::Slides])],
        Variant::TooSmall => unreachable!("returned above"),
    }
    l
}

// ---------------------------------------------------------------------------------------------
// Theme and glyphs.

/// The chrome's characters: Unicode where the locale is UTF-8, ASCII otherwise.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Glyphs {
    dot: &'static str,
    warn: &'static str,
    notes: &'static str,
    slide: &'static str,
    page: &'static str,
    done: &'static str,
    ellipsis: &'static str,
    rule: &'static str,
    bar: &'static str,
    times: &'static str,
    chevron: &'static str,
    meter_on: &'static str,
    meter_off: &'static str,
    /// The live edge: the open utterance's rows.
    edge: &'static str,
    /// A bullet list item's mark.
    bullet: &'static str,
    /// Keys as the footer and help name them.
    enter: &'static str,
    polish: &'static str,
    updown: &'static str,
    backtab: &'static str,
    /// The help overlay's border corners: top left, top right, bottom left, bottom right.
    corners: [&'static str; 4],
}

const UNICODE: Glyphs = Glyphs { dot: "●", warn: "▲", notes: "◆", slide: "▣", page: "✦", done: "✓", ellipsis: "…", rule: "─", bar: "│", times: "×", chevron: "›", meter_on: "■", meter_off: "□", edge: "▎", bullet: "•", enter: "⏎", polish: "polish⏎", updown: "↑↓", backtab: "⇧Tab", corners: ["┌", "┐", "└", "┘"] };
const ASCII: Glyphs = Glyphs { dot: "*", warn: "!", notes: "*", slide: "[]", page: "*", done: "+", ellipsis: "...", rule: "-", bar: "|", times: "x", chevron: ">", meter_on: "#", meter_off: "-", edge: "|", bullet: "-", enter: "Enter", polish: "polish Enter", updown: "Up/Down", backtab: "Shift-Tab", corners: ["+", "+", "+", "+"] };

/// Whether the effective locale is UTF-8: the first of `LC_ALL`, `LC_CTYPE`, `LANG` that is set
/// and not empty decides, as the C library's own lookup does.
pub(crate) fn utf8_locale(lc_all: Option<&str>, lc_ctype: Option<&str>, lang: Option<&str>) -> bool {
    [lc_all, lc_ctype, lang].into_iter().flatten().find(|v| !v.is_empty()).is_some_and(|v| {
        let v = v.to_ascii_uppercase();
        v.contains("UTF-8") || v.contains("UTF8")
    })
}

/// The frame's semantic styles and glyphs (plan §H tokens).
#[derive(Debug)]
pub(crate) struct Theme {
    paint: Paint,
    glyphs: &'static Glyphs,
}

const BOLD: Style = Style::new().add_modifier(Modifier::BOLD);
const DIM: Style = Style::new().add_modifier(Modifier::DIM);
const INK: Style = Style::new();

impl Theme {
    /// Colour as the plain CLI decides it (`NO_COLOR`, `COLORTERM`), glyphs from the locale.
    pub(crate) fn detect() -> Theme {
        let var = |k: &str| std::env::var(k).ok();
        Theme::new(crate::plain::paint(), utf8_locale(var("LC_ALL").as_deref(), var("LC_CTYPE").as_deref(), var("LANG").as_deref()))
    }

    pub(crate) fn new(paint: Paint, unicode: bool) -> Theme {
        Theme { paint, glyphs: if unicode { &UNICODE } else { &ASCII } }
    }

    fn colour(&self, rgb: (u8, u8, u8), ansi: Color) -> Style {
        match (self.paint.color, self.paint.truecolor) {
            (false, _) => INK,
            (true, true) => INK.fg(Color::Rgb(rgb.0, rgb.1, rgb.2)),
            (true, false) => INK.fg(ansi),
        }
    }

    /// Live and focus: the selected pane or tab, the marks of good news.
    fn teal(&self) -> Style {
        self.colour((93, 184, 192), Color::Cyan)
    }

    /// Recording and attention: the recording dot, a failure, a gap waiting.
    fn signal(&self) -> Style {
        self.colour((242, 118, 107), Color::Red)
    }
}

// ---------------------------------------------------------------------------------------------
// Words.

/// The stage as one word or phrase. Stop waiting is reversed: the next Ctrl-C ends the process at once.
fn phase(stage: Stage) -> Span<'static> {
    match stage {
        Stage::Listening => Span::styled("Listening", BOLD),
        Stage::Stopping => Span::styled("Stopping", BOLD),
        Stage::StopWaiting => Span::styled("Stop waiting", BOLD.add_modifier(Modifier::REVERSED)),
    }
}

/// What a stopping stage means for the lecture (plan §G's words), in the header's first row: the
/// full sentence, then shorter phrasings of the same meaning, longest first. The row takes the
/// first that fits and never cuts one mid-sentence; where none fits, the phase and clock say it.
fn meaning(stage: Stage) -> &'static [&'static str] {
    match stage {
        Stage::Listening => &[],
        Stage::Stopping => &["finishing the transcript and recovery, then a last snapshot", "finishing transcript + recovery, then last snapshot", "finishing transcript + recovery"],
        Stage::StopWaiting => &["no longer waiting for recovery or queued requests; what runs now and the last snapshot still finish", "not waiting for recovery or queued requests; the last snapshot still finishes", "not waiting for recovery; last snapshot finishes", "not waiting for recovery"],
    }
}

/// The safety view's keys: what still works below the minimum size (typing is ignored there).
const KEYS: [(&str, fn(Stage) -> &'static str); 2] = [("^L", redraw), ("^C", stop_key)];

fn redraw(_: Stage) -> &'static str {
    "redraw"
}

fn stop_key(stage: Stage) -> &'static str {
    match stage {
        Stage::Listening => "stop",
        Stage::Stopping => "stop waiting",
        Stage::StopWaiting => "quit at once",
    }
}

/// The empty hint line, while stopping: Enter takes nothing now, and the line says what happens instead.
const STOPPING_PROMPT: &str = "stopping: the last snapshot takes what is left";

/// Parts of a line, each with a priority (0 is kept longest): the most that fit `room` in their
/// own order, three spaces apart — the lowest priorities go first. None when not even the first
/// fits.
fn fit_parts(parts: Vec<(u8, Vec<Span<'static>>)>, room: usize) -> Option<Vec<Span<'static>>> {
    let join = |keep: &[bool]| {
        let mut spans = Vec::new();
        for (part, _) in parts.iter().zip(keep).filter(|(_, k)| **k) {
            if !spans.is_empty() {
                spans.push(Span::raw("   "));
            }
            spans.extend(part.1.iter().cloned());
        }
        spans
    };
    let mut order: Vec<usize> = (0..parts.len()).collect();
    order.sort_by_key(|&i| parts[i].0);
    let mut keep = vec![false; parts.len()];
    for i in order {
        keep[i] = true;
        if width(&join(&keep)) > room {
            keep[i] = false;
        }
    }
    let spans = join(&keep);
    (!spans.is_empty()).then_some(spans)
}

/// A key and what it does, in the footer's vocabulary: the chord bold, the action dim.
fn chord(key: &str, action: &str, style: Style) -> Vec<Span<'static>> {
    let mut spans = vec![Span::styled(key.to_string(), BOLD)];
    if !action.is_empty() {
        spans.extend([Span::raw(" "), Span::styled(action.to_string(), style)]);
    }
    spans
}

/// The footer (plan §H): only keys that do something now, `^C` always last. Listening, Enter and
/// `polish⏎`; `^X cancel` only while a request of this TUI's is running or queued; `^T back` while
/// zoomed; with an overlay open, its own keys. As the line narrows the lower priorities go — the
/// stop and help keys stay.
fn footer(v: &View, c: &Chrome, room: usize) -> Vec<Span<'static>> {
    let (t, g) = (c.theme, c.theme.glyphs);
    let listening = v.phase == Stage::Listening;
    let mut parts: Vec<(u8, Vec<Span<'static>>)> = Vec::new();
    match c.overlay {
        Overlay::Activity | Overlay::Help => {
            parts.push((2, chord(g.updown, "scroll", DIM)));
            parts.push((1, chord("Esc", "close", DIM)));
        }
        Overlay::None => {
            if listening {
                parts.push((2, chord(g.enter, "snapshot", DIM)));
                parts.push((4, chord(g.polish, "", DIM)));
            }
            if v.work.mine() {
                parts.push((2, chord("^X", "cancel", DIM)));
            }
            parts.push((5, chord("Tab", "pane", DIM)));
            if c.zoom {
                parts.push((3, chord("^T", "back", DIM)));
            }
            parts.push((6, chord("^O", "activity", DIM)));
            parts.push((1, chord("^H", "help", DIM)));
        }
    }
    let stop = if v.phase == Stage::StopWaiting { t.signal() } else { DIM };
    parts.push((0, chord("^C", stop_key(v.phase), stop)));
    fit_parts(parts, room).unwrap_or_else(|| clip(chord("^C", stop_key(v.phase), stop), room, g.ellipsis))
}

/// The spend as the header says it. A sum over nothing is `-0.0`, which `{:.2}` prints as "-0.00":
/// anything that rounds to zero cents from at or below zero is zero. A real negative amount keeps
/// its sign, and a positive fraction of a cent keeps its "<$0.01".
fn today(usd: f64) -> String {
    let usd = if usd <= 0.0 && usd > -0.005 { 0.0 } else { usd };
    format!("{} today", spend::money(usd))
}

fn clock(elapsed: Duration) -> String {
    let s = elapsed.as_secs();
    format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

fn width(spans: &[Span]) -> usize {
    spans.iter().map(Span::width).sum()
}

/// `text` cut to `max` cells with the ellipsis, when it does not fit.
fn fit(text: &str, max: usize, ellipsis: &str) -> String {
    if Span::raw(text).width() <= max {
        return text.to_string();
    }
    let room = max.saturating_sub(Span::raw(ellipsis).width());
    let (mut out, mut used) = (String::new(), 0);
    let mut buf = [0u8; 4];
    for c in text.chars() {
        let w = Span::raw(&*c.encode_utf8(&mut buf)).width();
        if used + w > room {
            break;
        }
        out.push(c);
        used += w;
    }
    if max >= Span::raw(ellipsis).width() {
        out.push_str(ellipsis);
    }
    out
}

/// Spans cut to `max` cells: the first span that overflows ends, with the ellipsis, and nothing after it is drawn.
fn clip(spans: Vec<Span<'static>>, max: usize, ellipsis: &str) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    let mut used = 0;
    for s in spans {
        if used + s.width() <= max {
            used += s.width();
            out.push(s);
        } else {
            out.push(Span::styled(fit(&s.content, max - used, ellipsis), s.style));
            break;
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Drawing.

/// What the frame shows besides the session view: the reactor's clock and its own notice (a
/// refused key or command), the focused pane and zoom, the reader's places in the transcript, the
/// notes and the activity, the parsed preview, the hint line, the overlay, the theme.
pub(crate) struct Chrome<'a> {
    pub(crate) elapsed: Duration,
    pub(crate) refused: Option<&'a str>,
    pub(crate) focus: Pane,
    pub(crate) zoom: bool,
    pub(crate) transcript: &'a Scroll,
    pub(crate) notes: &'a NoteScroll,
    /// The preview as last parsed, at most ten times a second.
    pub(crate) preview: &'a Preview,
    pub(crate) hint: &'a Hint,
    pub(crate) overlay: Overlay,
    pub(crate) activity: &'a ActivityScroll,
    /// The help overlay's first row shown.
    pub(crate) help: usize,
    pub(crate) theme: &'a Theme,
}

/// What a frame drew where: the reading keys move what the person sees, in the rows they see it
/// in; below the minimum size nothing is on screen and typing is ignored.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Drawn {
    pub(crate) small: bool,
    pub(crate) transcript: Option<Rect>,
    pub(crate) notes: Option<Rect>,
    /// The activity overlay's body.
    pub(crate) activity: Option<Rect>,
    /// The furthest the help overlay can scroll at this size.
    pub(crate) help_max: usize,
}

/// The rows between the header and the notice, across the whole width: where a zoomed pane and an
/// overlay go.
fn body_area(l: &Layout) -> Rect {
    let top = l.columns.iter().map(|c| c.heading.y).min().unwrap_or(0);
    let bottom = l.columns.iter().map(|c| c.body.bottom()).max().unwrap_or(top);
    Rect::new(l.header[0].x, top, l.header[0].width, bottom - top)
}

/// Ctrl-T (plan §H): the focused pane fills the body — one column, its heading naming every pane,
/// the header, notice, prompt and keys where they were. The breakpoints are untouched.
fn zoom(l: &mut Layout) {
    let area = body_area(l);
    l.columns = vec![Column { heading: Rect { height: 1, ..area }, body: Rect::new(area.x, area.y + 1, area.width, area.height.saturating_sub(1)), tabs: &[Pane::Transcript, Pane::Notes, Pane::Slides], ruled: false }];
    l.separators.clear();
}

/// Draws the frame, and says where the panes and the overlay went.
pub(crate) fn render(frame: &mut Frame, v: &View, c: &Chrome) -> Drawn {
    let mut l = layout(frame.area());
    if l.variant == Variant::TooSmall {
        too_small(frame, v, c);
        return Drawn { small: true, ..Drawn::default() };
    }
    if c.zoom {
        zoom(&mut l);
    }
    let mut drawn = Drawn::default();
    let g = c.theme.glyphs;
    frame.render_widget(Line::from(first_row(v, c, l.header[0].width as usize)), l.header[0]);
    frame.render_widget(Line::from(clock(c.elapsed)).right_aligned(), l.header[0]);
    let (left, right) = second_row(v, c.theme, l.header[1].width as usize);
    frame.render_widget(Line::from(left), l.header[1]);
    if let Some(right) = right {
        frame.render_widget(Line::from(right).right_aligned(), l.header[1]);
    }
    for r in &l.rules {
        frame.render_widget(Span::styled(g.rule.repeat(r.width as usize), DIM), *r);
    }
    for s in &l.separators {
        for row in s.rows() {
            frame.render_widget(Span::styled(g.bar, DIM), row);
        }
    }
    for col in &l.columns {
        let (tabs, named) = heading(col, v, c);
        let room = (col.heading.width as usize).saturating_sub(named + 2);
        frame.render_widget(Line::from(tabs), col.heading);
        match col.shown(c.focus) {
            Pane::Transcript => {
                if let Some(lane) = c.transcript.unseen(v).and_then(|n| reading_lane(n, room, c.theme)) {
                    frame.render_widget(Line::from(lane).right_aligned(), col.heading);
                }
                panes::transcript(frame.buffer_mut(), col.body, v, c.transcript, g.edge, c.theme.teal());
                drawn.transcript = Some(col.body);
            }
            Pane::Notes => {
                // stacked's heading is a rule: a space ends the rule before the lane
                let gap = usize::from(col.ruled);
                if let Some(mut lane) = work_lane(v, c.notes.scrolled(), room.saturating_sub(gap), c.theme) {
                    if col.ruled {
                        lane.insert(0, Span::raw(" "));
                    }
                    frame.render_widget(Line::from(lane).right_aligned(), col.heading);
                }
                let ink = panes::Ink { edge: g.edge, bullet: g.bullet, quote: g.bar, slide: g.slide, rule: g.rule, teal: c.theme.teal() };
                panes::notes(frame.buffer_mut(), col.body, v, c.preview, c.notes, &ink);
                drawn.notes = Some(col.body);
            }
            Pane::Slides if col.body.height > 0 => {
                frame.render_widget(Span::styled(fit("Slides show here.", col.body.width as usize, g.ellipsis), DIM), Rect { height: 1, ..col.body });
            }
            Pane::Slides => {}
        }
    }
    match c.overlay {
        Overlay::None => {}
        Overlay::Help => drawn.help_max = help(frame, body_area(&l), c),
        Overlay::Activity => drawn.activity = Some(activity(frame, body_area(&l), v, c)),
    }
    frame.render_widget(Line::from(notice(v, c, l.notice.width as usize)), l.notice);
    prompt(frame, l.prompt, v, c);
    frame.render_widget(Line::from(footer(v, c, l.keys.width as usize)), l.keys);
    drawn
}

/// A notice kind's mark and its style: red for a warning, teal for good news, dim for busy text.
pub(crate) fn mark(kind: &str, t: &Theme) -> (&'static str, Style) {
    let g = t.glyphs;
    match kind {
        "notes" => (g.notes, t.teal()),
        "slide" => (g.slide, t.teal()),
        "page" => (g.page, t.teal()),
        "done" => (g.done, t.teal()),
        "dim" => (g.ellipsis, DIM),
        _ => (g.warn, t.signal()),
    }
}

/// The hint line (plan §H): the mark, then the hint as typed — scrolled so the cursor stays in the
/// field — or, empty, what Enter does now, one cell after the cursor. The terminal's own cursor sits where the next character
/// goes, counted in cells as `tui-input` counts them.
fn prompt(frame: &mut Frame, rect: Rect, v: &View, c: &Chrome) {
    let (t, g) = (c.theme, c.theme.glyphs);
    let mark_w = Span::raw(g.notes).width() as u16;
    if rect.width <= mark_w + 1 {
        return;
    }
    frame.render_widget(Span::styled(g.notes, t.teal()), rect);
    let (x0, field) = (rect.x + mark_w + 1, (rect.width - mark_w - 1) as usize);
    let value = c.hint.value();
    if value.is_empty() {
        let words = if v.phase == Stage::Listening { format!("a hint, or {} for a snapshot", g.enter) } else { STOPPING_PROMPT.to_string() };
        // the cursor waits on its own cell, so its block never hides the placeholder's first letter
        if field > 1 {
            frame.render_widget(Span::styled(fit(&words, field - 1, g.ellipsis), DIM), Rect::new(x0 + 1, rect.y, field as u16 - 1, 1));
        }
        frame.set_cursor_position((x0, rect.y));
        return;
    }
    // what scrolled off the left: whole characters, as `tui-input` counts its scroll
    let scroll = c.hint.scroll(field);
    let (mut skipped, mut from) = (0, 0);
    for (i, ch) in value.char_indices() {
        let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if skipped >= scroll && w > 0 {
            break;
        }
        skipped += w;
        from = i + ch.len_utf8();
    }
    frame.buffer_mut().set_stringn(x0, rect.y, &value[from..], field, INK);
    let at = c.hint.cursor().saturating_sub(skipped).min(field - 1);
    frame.set_cursor_position((x0 + at as u16, rect.y));
}

/// The help overlay (plan §H): one single-line border, at most 64 columns, centred in the body; the
/// grammar and the keys by group, the three stop stages in §G's words, and the spend sentence. Taller
/// than the body, it scrolls itself — nothing behind it moves. Returns how far it can scroll.
fn help(frame: &mut Frame, area: Rect, c: &Chrome) -> usize {
    let g = c.theme.glyphs;
    let w = area.width.min(64);
    if w < 12 || area.height < 3 {
        return 0;
    }
    let inner = (w - 4) as usize;
    let lines = help_lines(g, inner);
    let h = (lines.len() + 2).min(area.height as usize) as u16;
    let rect = Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h);
    frame.render_widget(ratatui::widgets::Clear, rect);
    let visible = (h - 2) as usize;
    let max = lines.len().saturating_sub(visible);
    let first = c.help.min(max);
    let [tl, tr, bl, br] = g.corners;
    let title = format!("{}{} Help ", tl, g.rule);
    let top = format!("{title}{}{tr}", g.rule.repeat((w as usize).saturating_sub(Span::raw(title.as_str()).width() + 1)));
    frame.render_widget(Line::from(vec![Span::styled(title.clone(), DIM), Span::styled(top[title.len()..].to_string(), DIM)]), Rect { height: 1, ..rect });
    frame.buffer_mut().set_stringn(rect.x + 3, rect.y, "Help", 4, BOLD);
    let more = match (first > 0, first < max) {
        (_, true) => format!(" {} more ", g.updown),
        (true, false) => format!(" {} back ", g.updown),
        (false, false) => " Esc closes ".to_string(),
    };
    let fill = (w as usize).saturating_sub(Span::raw(more.as_str()).width() + 3);
    frame.render_widget(Span::styled(format!("{bl}{}{more}{}{br}", g.rule.repeat(fill), g.rule), DIM), Rect { y: rect.bottom() - 1, height: 1, ..rect });
    for row in 1..h - 1 {
        frame.buffer_mut().set_stringn(rect.x, rect.y + row, g.bar, 1, DIM);
        frame.buffer_mut().set_stringn(rect.right() - 1, rect.y + row, g.bar, 1, DIM);
    }
    for (i, line) in lines.into_iter().skip(first).take(visible).enumerate() {
        frame.render_widget(line, Rect::new(rect.x + 2, rect.y + 1 + i as u16, inner as u16, 1));
    }
    max
}

/// The help's rows at `inner` cells: group names bold — they part the groups, so no blank rows do —
/// each key bold in a column with what it does
/// wrapped beside it, the spend sentence dim at the end.
fn help_lines(g: &Glyphs, inner: usize) -> Vec<Line<'static>> {
    let hint = format!("hint {}", g.enter);
    let tabs = format!("Tab {}", g.backtab);
    let groups: [(&str, Vec<(&str, &str)>); 5] = [
        ("Notes", vec![(g.enter, "a snapshot now"), (hint.as_str(), "a snapshot that focuses on the hint"), (g.polish, "polish the notes; a snapshot comes first"), ("^X", "cancel your notes requests, running and queued")]),
        ("Reading", vec![(g.updown, "a row in the focused pane"), ("PgUp PgDn", "a page"), (tabs.as_str(), "the next or previous pane"), ("Esc", "back to live"), ("^T", "the focused pane fills the body; again to go back")]),
        ("Slides", vec![("Tab", "reaches the Slides pane")]),
        ("App", vec![("^O", "activity: this session's notices"), ("^H F1", "this help"), ("^L", "redraw the screen"), ("^Z", "nothing: suspending would stop the recording")]),
        (
            "Stopping",
            vec![
                ("^C once", "stop: finish the transcript and recovery, then a last snapshot"),
                ("^C twice", "stop waiting: recovery and queued requests wait for the next session; what runs now and the last snapshot still finish"),
                ("^C thrice", "quit at once; the next session in this folder picks up what was left"),
            ],
        ),
    ];
    let mut out = Vec::new();
    // the key column: the widest key and two cells, so no key ever runs into what it does
    let widest = groups.iter().flat_map(|(_, keys)| keys.iter().map(|(k, _)| Span::raw(*k).width())).max().unwrap_or(0);
    let key = (widest + 2).min(inner / 2);
    for (group, keys) in groups {
        out.push(Line::from(Span::styled(group, BOLD)));
        for (k, what) in keys {
            for (j, r) in panes::wrap(what, inner.saturating_sub(key).max(1)).into_iter().enumerate() {
                let left = if j == 0 { format!("{k}{}", " ".repeat(key.saturating_sub(Span::raw(k).width()))) } else { " ".repeat(key) };
                out.push(Line::from(vec![Span::styled(left, BOLD), Span::raw(what[r].to_string())]));
            }
        }
    }
    out.push(Line::from(""));
    for r in panes::wrap(SPEND, inner) {
        out.push(Line::from(Span::styled(SPEND[r].to_string(), DIM)));
    }
    out
}

/// Plan §H's one sentence on spend.
const SPEND: &str = "Spend is this lecture's cost today; speech-to-text is added when a recording closes.";

/// The activity overlay (plan §H): it fills the body — a heading, then the records under their
/// times, newest last. Returns its body, where the reading keys move it.
fn activity(frame: &mut Frame, area: Rect, v: &View, c: &Chrome) -> Rect {
    let t = c.theme;
    frame.render_widget(ratatui::widgets::Clear, area);
    frame.render_widget(Span::styled("Activity", t.teal().add_modifier(Modifier::BOLD)), Rect { height: 1, ..area });
    let body = Rect::new(area.x, area.y + 1, area.width, area.height.saturating_sub(1));
    let sign = |kind: &str| mark(kind, t);
    panes::activity(frame.buffer_mut(), body, &v.activity, c.activity, &sign);
    body
}

/// The scrolled transcript's heading lane, right of its tabs: what arrived below and how to get
/// back, in the footer's chord vocabulary. Shorter forms as the heading narrows; nothing when even
/// the count does not fit.
fn reading_lane(new: usize, room: usize, t: &Theme) -> Option<Vec<Span<'static>>> {
    let back = [Span::raw("   "), Span::styled("Esc", BOLD), Span::raw(" "), Span::styled("live", DIM)];
    let forms: Vec<Vec<Span<'static>>> = if new == 0 {
        vec![back[1..].to_vec()]
    } else {
        let count = |words: String| Span::styled(words, t.teal());
        vec![
            [vec![count(format!("{new} new below"))], back.to_vec()].concat(),
            [vec![count(format!("{new} new"))], back.to_vec()].concat(),
            vec![count(format!("{new} new"))],
        ]
    };
    forms.into_iter().find(|f| width(f) <= room)
}

/// The notes heading's lane, right of its name: what the notes work is doing — this TUI's own
/// request at the head (teal: it is live), how many wait behind it, and the study page
/// typesetting — and, while the reader is scrolled up, the way back. From typed state only (plan §C 13).
/// As the heading narrows the page goes first, then the queue, then the way back; the head's state
/// goes last.
fn work_lane(v: &View, scrolled: bool, room: usize, t: &Theme) -> Option<Vec<Span<'static>>> {
    let w = &v.work;
    let head = match w.lane() {
        Lane::Idle => None,
        Lane::Snapshot => Some("snapshot: writing"),
        Lane::PolishSnapshotFirst => Some("polish: snapshot first"),
        Lane::Polishing => Some("polishing"),
        Lane::LastSnapshot => Some("last snapshot: writing"),
    };
    let mut parts: Vec<(u8, Vec<Span<'static>>)> = Vec::new();
    if let Some(head) = head {
        parts.push((0, vec![Span::styled(head, t.teal())]));
    }
    if scrolled {
        parts.push((1, chord("Esc", "live", DIM)));
    }
    if w.queued() > 0 {
        parts.push((2, vec![Span::styled(format!("{} queued", w.queued()), DIM)]));
    }
    if w.page() {
        parts.push((3, vec![Span::styled("study page: typesetting", DIM)]));
    }
    fit_parts(parts, room)
}

/// Header row 1: the recording dot, the phase, then the course and lecture — or, while stopping,
/// what the stage is doing — cut to what is left beside the clock (drawn right-aligned over it).
/// The dot is red only while recording; stopping, it is dim, and the word says the rest.
fn first_row(v: &View, c: &Chrome, max: usize) -> Vec<Span<'static>> {
    let g = c.theme.glyphs;
    let dot = if v.phase == Stage::Listening { c.theme.signal() } else { DIM };
    let mut spans = vec![Span::styled(g.dot, dot), Span::raw(" "), phase(v.phase)];
    let room = max.saturating_sub(width(&spans) + 3 + 2 + clock(c.elapsed).len());
    let middle = match meaning(v.phase) {
        [] if room >= 8 => Some(fit(&format!("{} {} {}", v.identity.course, g.chevron, v.identity.lecture), room, g.ellipsis)),
        [] => None,
        phrasings => phrasings.iter().find(|m| Span::raw(**m).width() <= room).map(|m| m.to_string()),
    };
    if let Some(m) = middle {
        spans.extend([Span::raw("   "), Span::styled(m, DIM)]);
    }
    spans
}

/// Header row 2: the input and its health, the connection, the gaps waiting — and the spend, drawn
/// at the right. As the row narrows (plan §H): the spend goes, then the input's name, then the
/// connection's words shorten. The health, the connection's state and the gap count never go.
fn second_row(v: &View, t: &Theme, max: usize) -> (Vec<Span<'static>>, Option<Vec<Span<'static>>>) {
    let g = t.glyphs;
    let health: Vec<Span<'static>> = match (&v.input_gone, v.silence, v.level) {
        (Some(_), _, _) => vec![Span::styled(format!("{} input gone", g.warn), t.signal().add_modifier(Modifier::BOLD))],
        (None, true, _) => vec![Span::styled("no signal", t.signal().add_modifier(Modifier::BOLD))],
        (None, false, Some(l)) => {
            let on = (((dbfs(l) + 60.0) / 60.0 * 8.0).round().clamp(0.0, 8.0)) as usize;
            vec![Span::raw(g.meter_on.repeat(on)), Span::styled(g.meter_off.repeat(8 - on), DIM)]
        }
        (None, false, None) => Vec::new(),
    };
    let (full, short) = match &v.stt {
        None => (Span::styled("connecting", DIM), Span::styled(format!("STT {}", g.ellipsis), DIM)),
        Some(SttStatus::Connected) => (Span::styled("transcribing", DIM), Span::styled("STT ok", DIM)),
        Some(s) => {
            let words = match s {
                SttStatus::Retrying { after, reason } => format!("reconnecting in {} s ({reason})", after.as_secs()),
                SttStatus::Refused(m) => format!("refused: {m}"),
                SttStatus::ServerError(m) => format!("server: {m}"),
                SttStatus::Stopped(m) => format!("stopped: {m}"),
                SttStatus::Connected => unreachable!("matched above"),
            };
            (Span::styled(words, t.signal()), Span::styled(format!("STT {}", g.warn), t.signal()))
        }
    };
    let n = v.gaps();
    let gaps = Span::styled(format!("{n} gap{}", if n == 1 { "" } else { "s" }), if n > 0 { t.signal() } else { DIM });
    let spend = v.spend.map(|usd| vec![Span::styled(today(usd), DIM)]);
    let row = |name: bool, stt: &Span<'static>| {
        let mut parts: Vec<Vec<Span<'static>>> = Vec::new();
        if name {
            parts.push(vec![Span::raw(v.identity.input.clone())]);
        }
        if !health.is_empty() {
            parts.push(health.clone());
        }
        parts.push(vec![stt.clone()]);
        parts.push(vec![gaps.clone()]);
        let mut spans = Vec::new();
        for (i, p) in parts.into_iter().enumerate() {
            if i > 0 {
                spans.push(Span::raw("  "));
            }
            spans.extend(p);
        }
        spans
    };
    let fits = |s: &[Span], right: usize| width(s) + if right > 0 { right + 2 } else { 0 } <= max;
    let left = row(true, &full);
    if let Some(r) = &spend {
        if fits(&left, width(r)) {
            return (left, spend);
        }
    }
    for candidate in [left, row(false, &full)] {
        if fits(&candidate, 0) {
            return (candidate, None);
        }
    }
    (clip(row(false, &short), max, g.ellipsis), None)
}

/// A column's heading: the pane's name, or its tabs with the one shown bold in teal and the others
/// dim. Slides carry their count once there is one. Stacked's notes heading is a rule with its name in it.
/// Returns the spans and the cells the names take, before any rule after them.
fn heading(col: &Column, v: &View, c: &Chrome) -> (Vec<Span<'static>>, usize) {
    let t = c.theme;
    let name = |p: Pane| match p {
        Pane::Transcript => "Transcript".to_string(),
        Pane::Notes => "Notes".to_string(),
        Pane::Slides if v.slides.is_empty() => "Slides".to_string(),
        Pane::Slides => format!("Slides {}", v.slides.len()),
    };
    let shown = col.shown(c.focus);
    let style = |p: Pane| match (p == shown, p == c.focus, col.tabs.len() > 1) {
        (true, true, _) => t.teal().add_modifier(Modifier::BOLD),
        (true, false, true) => BOLD,
        (true, false, false) => INK,
        (false, _, _) => DIM,
    };
    let mut spans = Vec::new();
    if col.ruled {
        spans.push(Span::styled(format!("{}{} ", t.glyphs.rule, t.glyphs.rule), DIM));
    }
    for (i, &p) in col.tabs.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("   "));
        }
        spans.push(Span::styled(name(p), style(p)));
    }
    let named = width(&spans);
    if col.ruled {
        let rest = (col.heading.width as usize).saturating_sub(named + 1);
        spans.push(Span::styled(format!(" {}", t.glyphs.rule.repeat(rest)), DIM));
    }
    (spans, named)
}

/// The notice line: a refused stop first — it is about the keys just below — else the view's
/// current notice, its mark red for a warning and teal otherwise, its label bold. One line, cut to
/// fit; it never moves anything else.
fn notice(v: &View, c: &Chrome, max: usize) -> Vec<Span<'static>> {
    let (t, g) = (c.theme, c.theme.glyphs);
    let spans = if let Some(r) = c.refused {
        vec![Span::raw(r.to_string())]
    } else if let Some(n) = &v.notice {
        let (mark, style) = mark(n.kind, t);
        vec![Span::styled(mark, style), Span::raw(" "), Span::styled(n.label.clone(), BOLD), Span::raw("  "), Span::raw(n.detail.clone())]
    } else {
        Vec::new()
    };
    clip(spans, max, g.ellipsis)
}

fn keys(stage: Stage, t: &Theme) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for (i, (chord, action)) in KEYS.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("   "));
        }
        let style = if stage == Stage::StopWaiting && *chord == "^C" { t.signal() } else { DIM };
        spans.extend([Span::styled(*chord, BOLD), Span::raw(" "), Span::styled(action(stage), style)]);
    }
    spans
}

/// The safety view below 60×16: the phase and the clock first, so it is plain the lecture goes on,
/// then why nothing else is drawn and what size is needed, then the keys on the last row. Rows go
/// in that priority when even this does not fit; every line is clipped to the cells there are.
fn too_small(frame: &mut Frame, v: &View, c: &Chrome) {
    let area = frame.area();
    let (t, g) = (c.theme, c.theme.glyphs);
    let head = Line::from(vec![Span::raw(" "), Span::styled(g.dot, if v.phase == Stage::Listening { t.signal() } else { DIM }), Span::raw(" "), phase(v.phase), Span::raw("  "), Span::raw(clock(c.elapsed))]);
    let mut keys_line = vec![Span::raw(" ")];
    keys_line.extend(keys(v.phase, t));
    let middle = [
        Some(Line::from(format!(" Too small for the lecture view ({}{}{}).", area.width, g.times, area.height))),
        Some(Line::from(format!(" {MIN_WIDTH}{}{MIN_HEIGHT} needed. Recording goes on.", g.times))),
        c.refused.map(|r| Line::from(format!(" {r}"))),
    ];
    let rows: Vec<Rect> = area.rows().collect();
    let Some((&first, rest)) = rows.split_first() else { return };
    frame.render_widget(head, first);
    let Some((&last, between)) = rest.split_last() else { return };
    frame.render_widget(Line::from(keys_line), last);
    for (line, row) in middle.into_iter().flatten().zip(between) {
        frame.render_widget(line, *row);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plain;
    use crate::tui::hydrate::Hydration;
    use crate::tui::state::{Identity, SourceKind};
    use chrono::Local;
    use lecturelive_core::session::coordinator::Notification;
    use lecturelive_core::session::lecture::Event;
    use lecturelive_core::session::segments::{Segment, SegmentSource};
    use lecturelive_core::session::sidecar::{Gap, GapKind};
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::Terminal;

    const TRUE: Paint = Paint { color: true, truecolor: true };
    const ANSI: Paint = Paint { color: true, truecolor: false };
    const OFF: Paint = Paint { color: false, truecolor: false };
    const TEAL: Color = Color::Rgb(93, 184, 192);
    const SIGNAL: Color = Color::Rgb(242, 118, 107);

    /// The golden sizes (plan §J), with the person's own Terminal window, 127×36, measured in Task 5.
    const SIZES: [(u16, u16); 7] = [(140, 40), (110, 32), (80, 24), (72, 45), (60, 16), (40, 8), (127, 36)];

    fn identity() -> Identity {
        Identity { course: "Machine Learning".into(), lecture: "Week 03 — Optimisation".into(), input: "BlackHole 2ch".into(), kind: SourceKind::Loopback, notes_file: "lecture_notes_20260926.md".into(), transcript_file: "lecture_transcript_20260926.txt".into() }
    }

    fn view(stage: Stage) -> View {
        let mut v = View::new(identity(), Hydration::empty(), Vec::new());
        v.phase = stage;
        v.level = Some(0.05);
        v.stt = Some(SttStatus::Connected);
        v.spend = Some(0.18);
        v
    }

    fn gap(v: &mut View, start: u64) {
        v.reduce(&Event::Session(Notification::Gap(Gap::new(Default::default(), start, None, GapKind::SttOffline))), Local::now());
    }

    /// The frame's chrome as a test draws it: the hint empty, no overlay, the notes and activity
    /// following, at 0:42:18.
    fn chrome<'a>(theme: &'a Theme, transcript: &'a Scroll, preview: &'a Preview, focus: Pane, refused: Option<&'a str>) -> Chrome<'a> {
        Chrome { elapsed: Duration::from_secs(42 * 60 + 18), refused, focus, zoom: false, transcript, notes: Box::leak(Box::default()), preview, hint: Box::leak(Box::default()), overlay: Overlay::None, activity: Box::leak(Box::default()), help: 0, theme }
    }

    fn drawn_with(width: u16, height: u16, v: &View, refused: Option<&str>, theme: &Theme) -> Terminal<TestBackend> {
        drawn_scrolled(width, height, v, &Scroll::default(), refused, theme)
    }

    fn drawn_scrolled(width: u16, height: u16, v: &View, scroll: &Scroll, refused: Option<&str>, theme: &Theme) -> Terminal<TestBackend> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let preview = Preview::default();
        let chrome = chrome(theme, scroll, &preview, Pane::Transcript, refused);
        terminal.draw(|f| {
            render(f, v, &chrome);
        })
        .unwrap();
        terminal
    }

    fn drawn(width: u16, height: u16, v: &View, refused: Option<&str>) -> Buffer {
        drawn_with(width, height, v, refused, &Theme::new(TRUE, true)).backend().buffer().clone()
    }

    fn lines(b: &Buffer) -> Vec<String> {
        (0..b.area.height).map(|y| (0..b.area.width).map(|x| b[(x, y)].symbol()).collect::<String>().trim_end().to_string()).collect()
    }

    /// The cell where `text` starts on row `y`.
    fn at(b: &Buffer, y: u16, text: &str) -> (u16, u16) {
        let row: Vec<&str> = (0..b.area.width).map(|x| b[(x, y)].symbol()).collect();
        let first = text.chars().next().unwrap().to_string();
        let x = (0..row.len()).find(|&x| row[x] == first && row[x..].concat().starts_with(text)).unwrap_or_else(|| panic!("{text:?} not on row {y}: {:?}", row.concat()));
        (x as u16, y)
    }

    // ---- layout ---------------------------------------------------------------------------

    /// Plan §H's ladder at its edges, and the person's measured Terminal window.
    #[test]
    fn the_ladder_turns_at_its_breakpoints() {
        for ((w, h), want) in [
            ((132, 28), Variant::Wide),
            ((131, 28), Variant::Normal),
            ((132, 27), Variant::Normal),
            ((100, 20), Variant::Normal),
            ((99, 36), Variant::Stacked),
            ((99, 35), Variant::Narrow),
            ((60, 36), Variant::Stacked),
            ((60, 16), Variant::Narrow),
            ((59, 16), Variant::TooSmall),
            ((60, 15), Variant::TooSmall),
            ((127, 36), Variant::Normal),
            ((300, 100), Variant::Wide),
            ((100, 36), Variant::Normal),
            ((100, 19), Variant::Narrow),
            ((200, 16), Variant::Narrow),
            ((80, 24), Variant::Narrow),
            ((72, 45), Variant::Stacked),
            ((0, 0), Variant::TooSmall),
        ] {
            assert_eq!(variant(w, h), want, "{w}×{h}");
            assert_eq!(layout(Rect::new(0, 0, w, h)).variant, want, "{w}×{h}");
        }
    }

    /// The Task-5 measurement: 127×36 is normal, transcript and slides tabbed left, notes right.
    #[test]
    fn the_measured_terminal_is_normal() {
        let l = layout(Rect::new(0, 0, 127, 36));
        assert_eq!(l.variant, Variant::Normal);
        assert_eq!(l.columns.iter().map(|c| c.tabs).collect::<Vec<_>>(), vec![&[Pane::Transcript, Pane::Slides][..], &[Pane::Notes][..]]);
        assert_eq!((l.columns[0].body.width, l.columns[1].body.width), (48, 74), "40% and 60% of the 122 cells between the gutters and the gap");
    }

    /// Every rectangle lies inside the area and no two overlap, at every size the lecture view is
    /// drawn in: nothing a pane draws can land on another pane or on the chrome.
    #[test]
    fn rects_stay_inside_the_area_and_never_overlap() {
        for w in (60..=220).step_by(7).chain([99, 100, 131, 132]) {
            for h in (16..=70).step_by(3).chain([19, 20, 27, 28, 35, 36]) {
                let area = Rect::new(0, 0, w, h);
                let l = layout(area);
                let mut rects: Vec<Rect> = l.header.to_vec();
                rects.extend(&l.rules);
                rects.extend(&l.separators);
                for c in &l.columns {
                    rects.extend([c.heading, c.body]);
                }
                rects.extend([l.notice, l.prompt, l.keys]);
                for (i, a) in rects.iter().enumerate() {
                    assert!(area.contains(a.as_position()) && a.right() <= area.right() && a.bottom() <= area.bottom(), "{w}×{h}: {a:?} outside");
                    for b in &rects[i + 1..] {
                        assert!(a.is_empty() || b.is_empty() || !a.intersects(*b), "{w}×{h}: {a:?} overlaps {b:?}");
                    }
                }
                assert!(l.columns.iter().all(|c| c.body.height >= 1), "{w}×{h}: every pane has a body row");
            }
        }
    }

    /// Plan §H's row budget: the notice, prompt and keys are the last three rows in every shape, and
    /// the chrome stays within the clutter budget (8 of 40 wide; 6 of 16 at the minimum).
    #[test]
    fn the_bottom_rows_and_the_chrome_budget() {
        for (w, h) in [(140, 40), (110, 32), (127, 36), (80, 24), (72, 45), (60, 16), (99, 36), (132, 28)] {
            let l = layout(Rect::new(0, 0, w, h));
            assert_eq!((l.notice.y, l.prompt.y, l.keys.y), (h - 3, h - 2, h - 1), "{w}×{h}");
            assert_eq!((l.header[0].y, l.header[1].y), (0, 1));
            let budget = match l.variant {
                Variant::Wide | Variant::Normal => 8,
                Variant::Stacked => 7,
                _ => 6,
            };
            assert_eq!(l.chrome_rows(), budget, "{w}×{h} {:?}", l.variant);
            assert!(l.rules.is_empty() == !matches!(l.variant, Variant::Wide | Variant::Normal), "rules only where there is room for them");
        }
        assert_eq!(layout(Rect::new(0, 0, 140, 40)).chrome_rows(), 8);
        assert_eq!(layout(Rect::new(0, 0, 60, 16)).chrome_rows(), 6);
    }

    /// A long notice is cut to its one row; no body row moves, whatever the notice holds.
    #[test]
    fn a_long_notice_is_cut_to_its_line_and_moves_nothing() {
        let quiet = view(Stage::Listening);
        let mut loud = view(Stage::Listening);
        loud.notice = Some(plain::Notice { kind: "warn", label: "snapshot failed".into(), detail: "the model said something very long ".repeat(20) });
        for (w, h) in SIZES {
            let (a, b) = (lines(&drawn(w, h, &quiet, None)), lines(&drawn(w, h, &loud, None)));
            if variant(w, h) == Variant::TooSmall {
                continue;
            }
            let notice_row = (h - 3) as usize;
            for y in 0..h as usize {
                if y != notice_row {
                    assert_eq!(a[y], b[y], "{w}×{h} row {y}");
                }
            }
            assert!(b[notice_row].ends_with('…') && b[notice_row].contains("snapshot failed"), "{w}×{h}: {:?}", b[notice_row]);
            assert!(Span::raw(b[notice_row].as_str()).width() <= w as usize - 1);
        }
    }

    #[test]
    fn nothing_panics_at_any_size() {
        for (w, h) in [(0, 0), (1, 1), (0, 5), (5, 0), (1, 24), (200, 1), (40, 8), (59, 100), (1000, 3), (60, 15), (3, 2), (12, 3), (60, 16), (100, 19), (132, 28), (500, 200)] {
            for stage in [Stage::Listening, Stage::Stopping, Stage::StopWaiting] {
                let mut v = view(stage);
                v.notice = Some(plain::input_gone("UID"));
                for theme in [Theme::new(TRUE, true), Theme::new(OFF, false)] {
                    let _ = drawn_with(w, h, &v, Some(SUSPEND), &theme);
                }
            }
        }
    }

    // ---- words and degradation ------------------------------------------------------------

    /// Plan §H's header at full width: phase, course › lecture and clock; input, meter,
    /// connection, gaps, and the spend at the right.
    #[test]
    fn the_header_at_full_width() {
        let l = lines(&drawn(140, 40, &view(Stage::Listening), None));
        assert!(l[0].starts_with(" ● Listening   Machine Learning › Week 03 — Optimisation") && l[0].ends_with("0:42:18"), "{:?}", l[0]);
        assert!(l[1].starts_with(" BlackHole 2ch  ■■■■■□□□  transcribing  0 gaps") && l[1].ends_with("$0.18 today"), "{:?}", l[1]);
    }

    /// As the header narrows, lower-priority fields go first — spend, then the input's name, then
    /// the connection's words shorten — and the phase, the clock, the health and the gaps stay.
    #[test]
    fn the_header_degrades_by_priority() {
        let mut v = view(Stage::Listening);
        gap(&mut v, 0);
        gap(&mut v, 16_000);
        v.stt = Some(SttStatus::Retrying { after: Duration::from_secs(12), reason: "the socket closed after 30 s without data".into() });
        let row = |w: u16| lines(&drawn(w, 24, &v, None))[1].clone();
        let wide = lines(&drawn(160, 40, &v, None))[1].clone();
        assert!(wide.contains("BlackHole 2ch") && wide.contains("reconnecting in 12 s") && wide.ends_with("$0.18 today"), "{wide}");
        let mid = row(90);
        assert!(!mid.contains("today") && mid.contains("reconnecting in 12 s") && mid.contains("2 gaps"), "the spend goes first: {mid}");
        let narrow = row(60);
        assert!(!narrow.contains("BlackHole") && narrow.contains("STT ▲") && narrow.contains("2 gaps") && narrow.contains("■"), "{narrow}");
        for w in [60, 70, 80, 90, 99, 100, 127, 140] {
            let l = lines(&drawn(w, 24, &v, None));
            assert!(l[0].contains("Listening") && l[0].ends_with("0:42:18"), "{w}: {:?}", l[0]);
            assert!(l[1].contains("2 gaps"), "{w}: {:?}", l[1]);
        }
        let row1 = |w: u16| lines(&drawn(w, 24, &view(Stage::Listening), None))[0].clone();
        assert!(row1(80).contains("Machine Learning › Week 03 —"), "{}", row1(80));
        assert!(row1(60).contains("Machine Learning ›") && row1(60).contains('…'), "the course and lecture are cut, not the clock: {}", row1(60));
    }

    /// The stopping stages say what they do in the header's first row, in place of the course.
    #[test]
    fn stopping_says_what_it_is_doing() {
        let l = lines(&drawn(140, 40, &view(Stage::Stopping), None));
        assert!(l[0].starts_with(" ● Stopping   finishing the transcript and recovery, then a last snapshot"), "{:?}", l[0]);
        let l = lines(&drawn(140, 40, &view(Stage::StopWaiting), None));
        assert!(l[0].starts_with(" ● Stop waiting   no longer waiting for recovery"), "{:?}", l[0]);
    }

    /// Seen in Apple Terminal at 80×25: the stopping sentence was cut into "…then a last snap…".
    /// Each narrower header takes a whole shorter phrasing instead; the phase and the clock stay.
    #[test]
    fn stopping_prose_degrades_to_whole_phrases() {
        for (w, h, stage, want) in [
            (140, 40, Stage::Stopping, "finishing the transcript and recovery, then a last snapshot"),
            (80, 25, Stage::Stopping, "finishing transcript + recovery, then last snapshot"),
            (80, 24, Stage::Stopping, "finishing transcript + recovery, then last snapshot"),
            (72, 45, Stage::Stopping, "finishing transcript + recovery"),
            (60, 16, Stage::Stopping, "finishing transcript + recovery"),
            (140, 40, Stage::StopWaiting, "no longer waiting for recovery or queued requests; what runs now and the last snapshot still finish"),
            (110, 32, Stage::StopWaiting, "not waiting for recovery or queued requests; the last snapshot still finishes"),
            (80, 25, Stage::StopWaiting, "not waiting for recovery; last snapshot finishes"),
            (60, 16, Stage::StopWaiting, "not waiting for recovery"),
        ] {
            let l = lines(&drawn(w, h, &view(stage), None));
            let phase = if stage == Stage::Stopping { "Stopping" } else { "Stop waiting" };
            assert_eq!(l[0], format!(" ● {phase}   {want}{}0:42:18", " ".repeat(w as usize - 1 - 7 - 6 - phase.len() - want.chars().count())), "{w}×{h}");
            assert!(!l[0].contains('…'), "{w}×{h}: a whole phrase, never a cut one: {:?}", l[0]);
        }
        // too narrow for any phrasing: the phase and the clock alone, no fragment
        let mut b = Buffer::empty(Rect::new(0, 0, 30, 1));
        let theme = Theme::new(TRUE, true);
        let (scroll, preview) = (Scroll::default(), Preview::default());
        let chrome = chrome(&theme, &scroll, &preview, Pane::Transcript, None);
        ratatui::widgets::Widget::render(Line::from(first_row(&view(Stage::StopWaiting), &chrome, 30)), b.area, &mut b);
        assert_eq!(lines(&b)[0], "● Stop waiting");
    }

    /// Seen in Apple Terminal: "$-0.00 today" for an empty ledger. A sum over nothing is -0.0.
    #[test]
    fn zero_spend_never_renders_negative_zero() {
        for (usd, want) in [(-0.0, "$0.00 today"), (0.0, "$0.00 today"), (-0.004, "$0.00 today"), (0.004, "<$0.01 today"), (0.18, "$0.18 today"), (-0.25, "$-0.25 today"), (1234.5, "$1,234.50 today")] {
            assert_eq!(today(usd), want, "{usd:?}");
        }
        let mut v = view(Stage::Listening);
        v.spend = Some([0.0f64; 0].iter().sum());
        assert!(v.spend.unwrap().is_sign_negative(), "the empty ledger's own sum");
        let l = lines(&drawn(140, 40, &v, None));
        assert!(l[1].ends_with(" $0.00 today"), "{:?}", l[1]);
    }

    /// The footer offers only what this build does, generated from its key table.
    #[test]
    fn the_footer_follows_the_stop_stage() {
        for (stage, keys, safety) in [
            (Stage::Listening, " ⏎ snapshot   polish⏎   Tab pane   ^O activity   ^H help   ^C stop", " ^L redraw   ^C stop"),
            (Stage::Stopping, " Tab pane   ^O activity   ^H help   ^C stop waiting", " ^L redraw   ^C stop waiting"),
            (Stage::StopWaiting, " Tab pane   ^O activity   ^H help   ^C quit at once", " ^L redraw   ^C quit at once"),
        ] {
            assert_eq!(lines(&drawn(110, 32, &view(stage), None))[31], keys, "while stopping Enter takes nothing, and the footer does not offer it");
            assert_eq!(lines(&drawn(40, 8, &view(stage), None))[7], safety, "the safety view keeps the keys that work there");
        }
        let l = lines(&drawn(110, 32, &view(Stage::Listening), None));
        assert_eq!(l[30], " ◆  a hint, or ⏎ for a snapshot", "the empty hint line says what Enter does, one cell after the cursor");
        assert_eq!(lines(&drawn(110, 32, &view(Stage::Stopping), None))[30], format!(" ◆  {STOPPING_PROMPT}"));
        assert!(!l.join("").contains("^S"), "no capture key before Task 11");
    }

    /// A refused stop outranks the view's notice, being about the keys just below it.
    #[test]
    fn a_refused_stop_outranks_the_notice() {
        let mut v = view(Stage::Listening);
        v.notice = Some(plain::input_gone("Receiver_UID"));
        let l = lines(&drawn(110, 32, &v, None));
        assert!(l[29].starts_with(" ▲ input gone  Receiver_UID"), "{:?}", l[29]);
        let l = lines(&drawn(110, 32, &v, Some(SUSPEND)));
        assert_eq!(l[29], format!(" {SUSPEND}"));
    }

    /// Below 60×16 only the safety view: the phase and clock, why, what size, the keys — and nothing
    /// written outside the cells there are.
    #[test]
    fn the_safety_view_says_why_and_what_size() {
        let l = lines(&drawn(40, 8, &view(Stage::Listening), None));
        assert_eq!(l[0], " ● Listening  0:42:18");
        assert_eq!(l[1], " Too small for the lecture view (40×8).");
        assert_eq!(l[2], " 60×16 needed. Recording goes on.");
        assert_eq!(l[7], " ^L redraw   ^C stop");
        let l = lines(&drawn(59, 3, &view(Stage::Stopping), Some("Wait a moment, then Ctrl-C again to stop waiting.")));
        assert_eq!(l, vec![" ● Stopping  0:42:18", " Too small for the lecture view (59×3).", " ^L redraw   ^C stop waiting"]);
        let l = lines(&drawn(30, 2, &view(Stage::Listening), None));
        assert_eq!(l, vec![" ● Listening  0:42:18", " ^L redraw   ^C stop"]);
    }

    // ---- theme --------------------------------------------------------------------------------

    #[test]
    fn the_locale_decides_the_glyphs() {
        for (all, ctype, lang, want) in [
            (None, None, Some("en_US.UTF-8"), true),
            (None, None, Some("C.UTF-8"), true),
            (None, Some("en_GB.utf8"), None, true),
            (Some("C"), None, Some("en_US.UTF-8"), false),
            (Some(""), None, Some("en_US.UTF-8"), true),
            (None, Some("C"), Some("en_US.UTF-8"), false),
            (None, None, None, false),
            (None, None, Some("en_US.ISO8859-1"), false),
        ] {
            assert_eq!(utf8_locale(all, ctype, lang), want, "{all:?} {ctype:?} {lang:?}");
        }
    }

    /// Teal marks the focused pane and the good-news marks; truecolour when the terminal says so,
    /// ANSI cyan otherwise, and no colour under `NO_COLOR` — where bold still marks it.
    #[test]
    fn teal_is_the_focus_accent() {
        for (paint, want) in [(TRUE, Some(TEAL)), (ANSI, Some(Color::Cyan)), (OFF, None)] {
            let b = drawn_with(140, 40, &view(Stage::Listening), None, &Theme::new(paint, true)).backend().buffer().clone();
            let (x, y) = at(&b, 3, "Transcript");
            assert_eq!(b[(x, y)].fg, want.unwrap_or(Color::Reset), "{paint:?}");
            assert!(b[(x, y)].modifier.contains(Modifier::BOLD), "the focused heading is bold in every theme");
            let (x, y) = at(&b, 3, "Notes");
            assert_eq!((b[(x, y)].fg, b[(x, y)].modifier), (Color::Reset, Modifier::empty()), "an unfocused heading is plain ink");
            let (x, y) = at(&b, 38, "◆");
            assert_eq!(b[(x, y)].fg, want.unwrap_or(Color::Reset), "the prompt's mark");
        }
    }

    /// Signal red marks recording and attention: the dot while listening, gaps waiting, a failing
    /// connection, a warning's mark. The words carry each without colour.
    #[test]
    fn signal_red_marks_attention_and_words_carry_it_without_colour() {
        let mut v = view(Stage::Listening);
        gap(&mut v, 0);
        gap(&mut v, 8_000);
        v.stt = Some(SttStatus::Refused("bad key".into()));
        v.notice = Some(plain::Notice { kind: "warn", label: "snapshot failed".into(), detail: "timed out".into() });
        for (paint, want) in [(TRUE, SIGNAL), (ANSI, Color::Red), (OFF, Color::Reset)] {
            let b = drawn_with(140, 40, &v, None, &Theme::new(paint, true)).backend().buffer().clone();
            for (y, text) in [(0, "●"), (1, "2 gaps"), (1, "refused: bad key"), (37, "▲")] {
                let (x, y) = at(&b, y, text);
                assert_eq!(b[(x, y)].fg, want, "{text} {paint:?}");
            }
            let l = lines(&b);
            assert!(l[1].contains("2 gaps") && l[1].contains("refused: bad key") && l[37].contains("▲ snapshot failed"), "the words say it: {l:?}");
        }
        // stopping, the dot no longer claims a recording
        let b = drawn(140, 40, &view(Stage::Stopping), None);
        assert_eq!((b[(1, 0)].fg, b[(1, 0)].modifier), (Color::Reset, Modifier::DIM));
        // silence and a gone input are words, in the header's health place
        let mut quiet = view(Stage::Listening);
        quiet.silence = true;
        assert!(lines(&drawn(60, 16, &quiet, None))[1].contains("no signal"));
        let mut gone = View::new(Identity { kind: SourceKind::Input, ..identity() }, Hydration::empty(), Vec::new());
        gone.reduce(&Event::Session(Notification::DeviceGone { uid: "Receiver_UID".into() }), Local::now());
        let l = lines(&drawn_with(60, 16, &gone, None, &Theme::new(OFF, false)).backend().buffer().clone());
        assert!(l[1].contains("! input gone"), "{:?}", l[1]);
    }

    /// `NO_COLOR`: no cell carries a colour, and bold, dim and reverse remain.
    #[test]
    fn no_color_keeps_the_modifiers_and_drops_every_colour() {
        let mut v = view(Stage::StopWaiting);
        gap(&mut v, 0);
        v.notice = Some(plain::Notice { kind: "notes", label: "notes".into(), detail: "412 words".into() });
        for (w, h) in SIZES {
            let b = drawn_with(w, h, &v, None, &Theme::new(OFF, true)).backend().buffer().clone();
            assert!(b.content.iter().all(|c| c.fg == Color::Reset && c.bg == Color::Reset), "{w}×{h}");
        }
        let b = drawn_with(140, 40, &v, None, &Theme::new(OFF, true)).backend().buffer().clone();
        let (x, y) = at(&b, 0, "Stop waiting");
        assert!(b[(x, y)].modifier.contains(Modifier::BOLD | Modifier::REVERSED));
        assert!(b[(0, 2)].modifier.is_empty() && b[(1, 2)].modifier.contains(Modifier::DIM), "the rule is dim");
    }

    /// Nothing sets a background anywhere: light and dark terminals both work (plan §H).
    #[test]
    fn no_background_is_ever_set() {
        let mut v = view(Stage::Listening);
        gap(&mut v, 0);
        for (w, h) in SIZES {
            let b = drawn(w, h, &v, None);
            assert!(b.content.iter().all(|c| c.bg == Color::Reset), "{w}×{h}");
        }
    }

    /// In a tabbed column the pane shown is bold teal and the others dim; slides carry their count.
    #[test]
    fn the_selected_tab_stands_out() {
        let mut v = view(Stage::Listening);
        let b = drawn(110, 32, &v, None);
        let (x, y) = at(&b, 3, "Transcript");
        assert_eq!((b[(x, y)].fg, b[(x, y)].modifier), (TEAL, Modifier::BOLD));
        let (x, y) = at(&b, 3, "Slides");
        assert_eq!(b[(x, y)].modifier, Modifier::DIM);
        assert!(lines(&b)[3].contains("Transcript   Slides") && !lines(&b)[3].contains("Slides 0"));
        v.reduce(&Event::Slide { index: 18, file: "slides/slide_18_104152.png".into(), auto: true, uncertain: false, shown_at: Local::now() }, Local::now());
        assert!(lines(&drawn(110, 32, &v, None))[3].contains("Slides 1"));
        let l = lines(&drawn(80, 24, &view(Stage::Listening), None));
        assert!(l[2].starts_with(" Transcript   Notes   Slides"), "narrow: every pane a tab: {:?}", l[2]);
    }

    /// ASCII chrome where the locale is not UTF-8; the lecture's own text is never transliterated.
    #[test]
    fn ascii_chrome_leaves_the_lecture_text_alone() {
        let mut v = view(Stage::Listening);
        v.notice = Some(plain::Notice { kind: "slide", label: "slide 3".into(), detail: "slide_03_100000.png, into the next snapshot".into() });
        for (w, h) in SIZES {
            let l = lines(drawn_with(w, h, &v, None, &Theme::new(TRUE, false)).backend().buffer());
            let text = l.join("\n").replace("Week 03 — Optimisation", "").replace("Week 03 —", "");
            assert!(text.is_ascii(), "{w}×{h}: {text}");
        }
        let l = lines(drawn_with(140, 40, &v, None, &Theme::new(TRUE, false)).backend().buffer());
        assert!(l[0].starts_with(" * Listening   Machine Learning > Week 03 — Optimisation"), "{:?}", l[0]);
        assert!(l[1].contains("BlackHole 2ch  #####---  transcribing"), "{:?}", l[1]);
        assert!(l[2].trim().chars().all(|c| c == '-') && l[4].contains(" | "), "{:?} {:?}", l[2], l[4]);
        assert!(l[37].starts_with(" [] slide 3") && l[38].starts_with(" *  a hint, or Enter for a snapshot"), "{:?} {:?}", l[37], l[38]);
        assert_eq!(lines(drawn_with(40, 8, &v, None, &Theme::new(TRUE, false)).backend().buffer())[2], " 60x16 needed. Recording goes on.");
    }

    // ---- goldens ------------------------------------------------------------------------------

    /// Compares a frame's text with `crates/cli/goldens/{name}.txt` (plan §J). `LECTURELIVE_GOLDENS=update`
    /// rewrites the file instead; the diff is then reviewed before it is committed.
    fn golden(name: &str, terminal: &Terminal<TestBackend>) {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("goldens").join(format!("{name}.txt"));
        let got = terminal.backend().to_string();
        if std::env::var("LECTURELIVE_GOLDENS").as_deref() == Ok("update") {
            std::fs::write(&path, &got).unwrap();
            return;
        }
        let want = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}; write it with LECTURELIVE_GOLDENS=update and review it", path.display()));
        assert!(got == want, "{name} differs from its golden; if the change is meant, LECTURELIVE_GOLDENS=update and review the diff\n--- golden\n{want}--- drawn\n{got}");
    }

    // ---- the transcript pane (Task 8) ---------------------------------------------------------

    /// The live-transcript sizes: the golden sizes, and 80×25, the person's Apple Terminal at the
    /// end of the Task-7 sitting.
    const TRANSCRIPT_SIZES: [(u16, u16); 8] = [(140, 40), (110, 32), (127, 36), (80, 24), (80, 25), (72, 45), (60, 16), (40, 8)];

    fn said(id: u64, hms: (u32, u32, u32), text: &str, source: SegmentSource) -> Event {
        let t = chrono::TimeZone::with_ymd_and_hms(&Local, 2026, 9, 26, hms.0, hms.1, hms.2).unwrap();
        Event::Session(Notification::Segment(Segment { id, recording_id: Default::default(), start_sample: id * 80_000, end_sample: (id + 1) * 80_000, said_at: t, start: t, end: t, text: text.into(), words: Vec::new(), source }))
    }

    /// A lecture in progress: closed segments, some wrapping; one recovered after a dropped
    /// connection (its time earlier than the segment before it: the log's order is the order it
    /// was written); the open utterance, settled and tentative.
    fn live() -> View {
        let mut v = view(Stage::Listening);
        for (id, (hms, text, source)) in [
            ((10, 38, 40), "Right, let's pick up where we stopped on Tuesday.", SegmentSource::Live),
            ((10, 38, 46), "We had a population, and one sample from it, and we computed a mean.", SegmentSource::Live),
            ((10, 38, 58), "The question I left you with was how far that one number could be from the truth.", SegmentSource::Live),
            ((10, 39, 14), "Several of you said it depends on the sample, which is exactly right, and exactly the problem.", SegmentSource::Live),
            ((10, 39, 31), "A different sample gives a different mean.", SegmentSource::Live),
            ((10, 39, 40), "So the mean is itself a random quantity, with a distribution we can reason about.", SegmentSource::Live),
            ((10, 39, 58), "That shift in view, from one number to a distribution of numbers, is the whole of today.", SegmentSource::Live),
            ((10, 40, 20), "Keep the picture of the histogram from the lab in mind.", SegmentSource::Live),
            ((10, 40, 52), "Last week we saw how a single sample can mislead, so today is about what happens when we repeat the experiment.", SegmentSource::Live),
            ((10, 41, 5), "Imagine drawing a hundred samples of size n from one population and taking the mean of each.", SegmentSource::Live),
            ((10, 41, 19), "Those means have a distribution of their own: the sampling distribution of the mean.", SegmentSource::Live),
            ((10, 41, 12), "Its centre sits on the population mean.", SegmentSource::Recovered),
            ((10, 41, 44), "The spread is the interesting part.", SegmentSource::Live),
            ((10, 42, 3), "We distinguish the sample statistic from the population parameter.", SegmentSource::Live),
            ((10, 42, 11), "Increasing the sample size reduces that variability, and the formula says by how much: σ/√n.", SegmentSource::Live),
        ]
        .into_iter()
        .enumerate()
        {
            v.reduce(&said(id as u64, hms, text, source), Local::now());
        }
        v.reduce(&Event::Session(Notification::Open { stable: "The denominator changes with the square root of n,".into(), tentative: " so quadrupling the sample only halves".into() }), Local::now());
        v
    }

    /// `v` scrolled up `rows` in the transcript's body at `w`×`h`, then `arrivals` more segments.
    fn scrolled(mut v: View, w: u16, h: u16, rows: usize, arrivals: u64) -> (View, Scroll) {
        let body = layout(Rect::new(0, 0, w, h)).columns[0].body;
        let mut s = Scroll::default();
        assert!(s.apply(panes::Move::Up(rows), &v, body), "{w}×{h}: something to scroll");
        let next = v.closed.len() as u64;
        for k in 0..arrivals {
            v.reduce(&said(next + k, (10, 42, 20 + k as u32 * 6), &format!("A later sentence, number {k}, arrived while the reader was up there."), SegmentSource::Live), Local::now());
        }
        (v, s)
    }

    /// Plan Task 8's goldens: the live transcript at every size (the safety view shows none of it),
    /// and scrolled with arrivals below.
    #[test]
    fn goldens_for_the_live_transcript() {
        let theme = Theme::new(TRUE, true);
        for (w, h) in TRANSCRIPT_SIZES {
            let name = if variant(w, h) == Variant::TooSmall { format!("too_small_transcript_{w}x{h}") } else { format!("transcript_{w}x{h}") };
            golden(&name, &drawn_with(w, h, &live(), None, &theme));
        }
        for (w, h) in [(140, 40), (110, 32), (80, 25)] {
            let (v, s) = scrolled(live(), w, h, 4, 2);
            golden(&format!("transcript_scrolled_{w}x{h}"), &drawn_scrolled(w, h, &v, &s, None, &theme));
        }
        golden("transcript_ascii_80x25", &drawn_with(80, 25, &live(), None, &Theme::new(OFF, false)));
    }

    #[test]
    fn the_transcript_fills_its_pane_in_every_layout() {
        for (w, h) in TRANSCRIPT_SIZES {
            let l = lines(&drawn(w, h, &live(), None));
            let text = l.join("\n");
            if variant(w, h) == Variant::TooSmall {
                assert!(!text.contains("denominator") && !text.contains('▎') && !text.contains("10:4"), "{w}×{h}: no transcript in the safety view");
                continue;
            }
            let body = layout(Rect::new(0, 0, w, h)).columns[0].body;
            let last = (body.bottom() - 1) as usize;
            assert!(l[last].contains("▎"), "{w}×{h}: following ends on the live edge: {:?}", l[last]);
            assert!(text.contains("10:42:11") || h < 20, "{w}×{h}");
            assert!(!text.contains("The transcript shows here"), "{w}×{h}");
        }
    }

    /// The pane's styles: the gutter and `recovered` dim; the live edge teal on every open row;
    /// settled words in ink, tentative dim; the focused heading keeps its accent; no background.
    #[test]
    fn the_transcript_styles() {
        let b = drawn(140, 40, &live(), None);
        let l = lines(&b);
        let row = |needle: &str| l.iter().position(|r| r.contains(needle)).unwrap_or_else(|| panic!("{needle:?} in {l:?}")) as u16;
        let (x, y) = at(&b, row("10:42:03"), "10:42:03");
        assert!((x..x + 8).all(|x| b[(x, y)].modifier == Modifier::DIM && b[(x, y)].fg == Color::Reset), "the gutter is dim");
        let (tx, ty) = at(&b, y, "We distinguish");
        assert_eq!((b[(tx, ty)].modifier, b[(tx, ty)].fg, tx - x), (Modifier::empty(), Color::Reset, 10), "text in ink, two cells after the gutter");
        let (rx, ry) = at(&b, row("recovered"), "recovered");
        assert_eq!((b[(rx, ry)].modifier, rx), (Modifier::DIM, x), "the mark dim, in the time's lane");
        let edges: Vec<u16> = (4..36).filter(|&y| b[(x + 8, y)].symbol() == "▎").collect();
        assert_eq!(edges.len(), 4, "the open utterance wraps onto four rows of the 140-column transcript: {l:?}");
        for &y in &edges {
            assert_eq!(b[(x + 8, y)].fg, TEAL, "every open row carries the teal edge");
            assert!((x..x + 8).all(|x| b[(x, y)].symbol() == " "), "and no time");
        }
        let (sx, sy) = at(&b, edges[0], "The denominator");
        assert_eq!(b[(sx, sy)].modifier, Modifier::empty(), "settled words in ink");
        let (qx, qy) = at(&b, *edges.last().unwrap(), "halves");
        assert_eq!(b[(qx, qy)].modifier, Modifier::DIM, "tentative words dim");
        let (hx, hy) = at(&b, 3, "Transcript");
        assert_eq!((b[(hx, hy)].fg, b[(hx, hy)].modifier), (TEAL, Modifier::BOLD), "the heading keeps Task 7's accent");
        for (w, h) in TRANSCRIPT_SIZES {
            assert!(drawn(w, h, &live(), None).content.iter().all(|c| c.bg == Color::Reset), "{w}×{h}: no background");
        }
    }

    /// `NO_COLOR`: no colour anywhere, and the live edge is still drawn — the glyph and its place
    /// carry the meaning. ASCII: the edge is `|`; the lecture's own words stay as spoken.
    #[test]
    fn the_live_edge_without_colour_and_in_ascii() {
        let b = drawn_with(80, 25, &live(), None, &Theme::new(OFF, true)).backend().buffer().clone();
        assert!(b.content.iter().all(|c| c.fg == Color::Reset && c.bg == Color::Reset));
        let l = lines(&b);
        assert!(l.iter().filter(|r| r.starts_with("         ▎ ")).count() >= 2, "{l:?}");
        let l = lines(drawn_with(80, 25, &live(), None, &Theme::new(OFF, false)).backend().buffer());
        assert!(l.iter().filter(|r| r.starts_with("         | ")).count() >= 2, "{l:?}");
        assert!(l.iter().any(|r| r.contains("σ/√n")), "the transcript is never transliterated: {l:?}");
        assert!(!l.join("").contains('▎'));
    }

    /// Scrolled, the heading says what arrived below and how to get back; the lane shortens with
    /// the column and never pushes the tabs.
    #[test]
    fn the_scrolled_heading_counts_what_is_new_below() {
        for (w, h, want) in [(140, 40, " Transcript         2 new below   Esc live │ Notes"), (110, 32, " Transcript   Slides       2 new   Esc live │ Notes"), (80, 25, " Transcript   Notes   Slides                             2 new below   Esc live")] {
            let (v, s) = scrolled(live(), w, h, 4, 2);
            let b = drawn_scrolled(w, h, &v, &s, None, &Theme::new(TRUE, true)).backend().buffer().clone();
            let row = if variant(w, h) == Variant::Narrow { 2 } else { 3 };
            assert!(lines(&b)[row].starts_with(want), "{w}×{h}: {:?}", lines(&b)[row]);
            let (x, y) = at(&b, row as u16, "2 new");
            assert_eq!(b[(x, y)].fg, TEAL);
            let (x, y) = at(&b, row as u16, "Esc");
            assert_eq!(b[(x, y)].modifier, Modifier::BOLD);
        }
        let (v, s) = scrolled(live(), 140, 40, 4, 1);
        assert!(lines(&drawn_scrolled(140, 40, &v, &s, None, &Theme::new(TRUE, true)).backend().buffer().clone())[3].contains("1 new below   Esc live"));
        let (v, s) = scrolled(live(), 140, 40, 4, 0);
        let l = lines(&drawn_scrolled(140, 40, &v, &s, None, &Theme::new(TRUE, true)).backend().buffer().clone());
        assert!(l[3].contains("Esc live") && !l[3].contains("new"), "{:?}", l[3]);
    }

    /// Every stage at every golden size; 40×8 is the safety view. Unicode chrome, colour on (the
    /// goldens hold text only: styles are the assertions above).
    #[test]
    fn goldens_for_every_stage_and_size() {
        for (stage, name) in [(Stage::Listening, "listening"), (Stage::Stopping, "stopping"), (Stage::StopWaiting, "stop_waiting")] {
            for (w, h) in SIZES {
                let name = if variant(w, h) == Variant::TooSmall { format!("too_small_{name}_{w}x{h}") } else { format!("{name}_{w}x{h}") };
                golden(&name, &drawn_with(w, h, &view(stage), None, &Theme::new(TRUE, true)));
            }
        }
        golden("listening_ascii_80x24", &drawn_with(80, 24, &view(Stage::Listening), None, &Theme::new(OFF, false)));
    }

    // ---- the notes pane (Task 9) --------------------------------------------------------------

    /// The notes of the lecture `live()` transcribes, as LectureLive writes them.
    const NOTES: &str = "# Statistics — Week 04 — Sampling\n\n<!-- 10:39:12 -->\n## Sampling distributions\n### Standard error\n\
- Larger samples reduce **standard error**, by \\( \\sigma / \\sqrt{n} \\).\n  - Quadrupling *n* only halves it.\n\
- Distinct from the spread of the observations: `sd` versus `se`.\n\n![Slide 17](slides/slide_17_103941.png)\n\n\
> Keep the histogram from the lab in mind.\n\n```\nse = sd / sqrt(n)\n```\n\n| n | se |\n|---|---|\n| 25 | 2.0 |\n| 100 | 1.0 |\n\n\
<!-- 10:41:52 -->\n## Sample statistic and parameter\n- The **statistic** is computed from the sample; the parameter belongs to the population.\n\
- 標本平均 is the sample mean 🎓\n";

    /// One moment for every test event, so the activity's times are the same in every run.
    fn fixed() -> chrono::DateTime<Local> {
        chrono::TimeZone::with_ymd_and_hms(&Local, 2026, 9, 26, 10, 43, 5).unwrap()
    }

    const PROVISIONAL: &str = "## Provisional heading\n- tentative words the model is still writing about quadrupling ";
    const REAL: &str = "\n<!-- 10:43:05 -->\n## Confidence intervals\n- An interval built to cover the mean in repeated samples.\n";

    #[derive(Clone, Copy, Debug)]
    enum Work {
        Committed,
        Writing,
        AfterCommit,
        PolishFirst,
        Polishing,
        Page,
    }

    fn commit(v: &mut View, block: &str, revision: u64) {
        v.reduce(&Event::Committed { words: 60, slides: 0, block: block.into(), usd: 0.02, confirmed: true, removed: 0, missing: 0, revision }, fixed());
    }

    /// The live lecture with its notes, the notes work in state `w`.
    fn noted(w: Work) -> View {
        use crate::tui::hydrate::NotesSnapshot;
        use crate::tui::state::OwnOp;
        let mut v = live();
        v.merge(Hydration { notes: NotesSnapshot::At { revision: 1, document: NOTES.into() }, ..Hydration::empty() });
        v.reduce(&Event::Slide { index: 17, file: "slides/slide_17_103941.png".into(), auto: true, uncertain: false, shown_at: chrono::TimeZone::with_ymd_and_hms(&Local, 2026, 9, 26, 10, 39, 41).unwrap() }, fixed());
        v.notice = None;
        let preview = |v: &mut View| v.reduce(&Event::Preview(PROVISIONAL.into()), fixed());
        match w {
            Work::Committed => {}
            Work::Writing => {
                v.work.submit(OwnOp::Snapshot);
                preview(&mut v);
            }
            Work::AfterCommit => {
                v.work.submit(OwnOp::Snapshot);
                preview(&mut v);
                commit(&mut v, REAL, 2);
            }
            Work::PolishFirst => {
                v.work.submit(OwnOp::Polish);
                v.work.submit(OwnOp::Snapshot);
                preview(&mut v);
            }
            Work::Polishing => {
                v.work.submit(OwnOp::Polish);
                preview(&mut v);
                commit(&mut v, REAL, 2);
            }
            Work::Page => {
                v.work.submit(OwnOp::Polish);
                v.reduce(&Event::NothingNew, fixed());
                v.reduce(&Event::Polished { backup: "b.md".into(), usd: 0.04, revision: 2 }, fixed());
                v.work.submit(OwnOp::Snapshot);
                v.work.submit(OwnOp::Snapshot);
            }
        }
        v
    }

    /// A frame as the reactor draws it: the preview refreshed first, `focus` the reading pane.
    fn framed(width: u16, height: u16, v: &View, focus: Pane, theme: &Theme) -> Terminal<TestBackend> {
        let mut preview = Preview::default();
        preview.refresh(&v.notes, std::time::Instant::now());
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let scroll = Scroll::default();
        let chrome = chrome(theme, &scroll, &preview, focus, None);
        terminal.draw(|f| {
            render(f, v, &chrome);
        })
        .unwrap();
        terminal
    }

    /// Narrow shows one pane: there the notes are drawn with the notes as the tab shown.
    fn focus_for(w: u16, h: u16) -> Pane {
        if variant(w, h) == Variant::Narrow { Pane::Notes } else { Pane::Transcript }
    }

    /// The notes body in a frame, as rows: the column the notes are in.
    fn notes_rows(b: &Buffer, w: u16, h: u16) -> Vec<String> {
        let l = layout(Rect::new(0, 0, w, h));
        let col = l.columns.iter().find(|c| c.tabs.contains(&Pane::Notes)).unwrap();
        col.body.rows().map(|r| {
            let (mut s, mut x) = (String::new(), r.x);
            while x < r.right() {
                let sym = b[(x, r.y)].symbol();
                s.push_str(sym);
                x += (Span::raw(sym).width() as u16).max(1); // a wide glyph once, not its covered cell
            }
            s.trim_end().to_string()
        })
        .collect()
    }

    /// Plan Task 9's goldens: committed notes at every size (narrow with the notes tab shown), the
    /// snapshot being written, after its commit, and the polish and page lanes.
    #[test]
    fn goldens_for_the_notes() {
        let theme = Theme::new(TRUE, true);
        let all = [(140, 40), (110, 32), (127, 36), (80, 24), (80, 25), (72, 45), (60, 16)];
        for (work, name, sizes) in [
            (Work::Committed, "committed", &all[..]),
            (Work::Writing, "writing", &[(140, 40), (110, 32), (127, 36), (80, 25), (72, 45), (60, 16)][..]),
            (Work::AfterCommit, "after_commit", &[(140, 40), (80, 25), (72, 45)][..]),
            (Work::PolishFirst, "polish_first", &[(110, 32), (72, 45)][..]),
            (Work::Polishing, "polishing", &[(110, 32), (60, 16)][..]),
            (Work::Page, "page", &[(140, 40), (80, 25)][..]),
        ] {
            for &(w, h) in sizes {
                golden(&format!("notes_{name}_{w}x{h}"), &framed(w, h, &noted(work), focus_for(w, h), &theme));
            }
        }
        golden("notes_ascii_80x25", &framed(80, 25, &noted(Work::Writing), Pane::Notes, &Theme::new(OFF, false)));
    }

    /// The notes fill their pane in every layout that shows them, and the transcript is not
    /// disturbed where both are on screen.
    #[test]
    fn the_notes_fill_their_pane_in_every_layout() {
        for (w, h) in TRANSCRIPT_SIZES {
            let b = framed(w, h, &noted(Work::Writing), focus_for(w, h), &Theme::new(TRUE, true)).backend().buffer().clone();
            let text = lines(&b).join("\n");
            if variant(w, h) == Variant::TooSmall {
                assert!(!text.contains("writing") && !text.contains("statistic"), "{w}×{h}: nothing of the notes in the safety view");
                continue;
            }
            let rows = notes_rows(&b, w, h);
            let last = rows.iter().rposition(|r| !r.is_empty()).unwrap();
            assert!(rows[last].contains('▎'), "{w}×{h}: following ends on the preview: {rows:?}");
            assert!(!text.contains("Nothing written yet"), "{w}×{h}");
            if variant(w, h) != Variant::Narrow {
                assert!(text.contains("10:42:11"), "{w}×{h}: the transcript is still there");
            }
        }
    }

    /// Plan Task 9's gate: after a commit's update, not one cell of the preview remains, and the
    /// committed chunk is there — whether or not the parsed preview was refreshed before the draw.
    #[test]
    fn no_preview_cell_survives_its_commit() {
        let (w, h) = (140, 40);
        let theme = Theme::new(TRUE, true);
        let mut v = noted(Work::Writing);
        let mut stale = Preview::default();
        stale.refresh(&v.notes, std::time::Instant::now());
        let before = notes_rows(framed(w, h, &v, Pane::Transcript, &theme).backend().buffer(), w, h);
        assert!(before.iter().any(|r| r.starts_with("writing ▎ Provisional heading")), "{before:?}");
        assert!(before.iter().any(|r| r.contains("tentative words")));
        commit(&mut v, REAL, 2);
        let check = |b: &Buffer| {
            let after = notes_rows(b, w, h);
            let text = after.join("\n");
            for gone in ["writing", "▎", "Provisional", "tentative", "still", "quadrupling"] {
                assert!(!text.contains(gone), "{gone:?} survived the commit: {after:?}");
            }
            assert!(after.iter().any(|r| r == "10:43:05  Confidence intervals"), "{after:?}");
            assert!(after.iter().any(|r| r.contains("An interval built to cover the mean")));
        };
        check(framed(w, h, &v, Pane::Transcript, &theme).backend().buffer());
        // the reactor refreshes before it draws; even a frame drawn with the old parse shows none of it
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        let scroll = Scroll::default();
        let chrome = chrome(&theme, &scroll, &stale, Pane::Transcript, None);
        terminal.draw(|f| {
            render(f, &v, &chrome);
        })
        .unwrap();
        check(terminal.backend().buffer());
    }

    /// The heading lane: the head's state teal, what waits and the page dim; at a narrower heading
    /// the page goes first, then the queue — and it never pushes the heading's name.
    #[test]
    fn the_notes_heading_says_what_the_work_is_doing() {
        let theme = Theme::new(TRUE, true);
        for (work, want) in [
            (Work::Committed, None),
            (Work::Writing, Some("snapshot: writing")),
            (Work::PolishFirst, Some("polish: snapshot first   1 queued")),
            (Work::Polishing, Some("polishing")),
            (Work::Page, Some("snapshot: writing   1 queued   study page: typesetting")),
        ] {
            let b = framed(140, 40, &noted(work), Pane::Transcript, &theme).backend().buffer().clone();
            let row = &lines(&b)[3];
            match want {
                Some(want) => assert!(row.contains(&format!("{want} │ Slides")), "{work:?}: right-aligned in the notes heading: {row:?}"),
                None => assert!(!row.contains("writing") && !row.contains("polish"), "{row:?}"),
            }
        }
        let b = framed(140, 40, &noted(Work::Page), Pane::Transcript, &theme).backend().buffer().clone();
        let (x, y) = at(&b, 3, "snapshot: writing");
        assert_eq!(b[(x, y)].fg, TEAL);
        let (x, y) = at(&b, 3, "1 queued");
        assert_eq!(b[(x, y)].modifier, Modifier::DIM);
        let (x, y) = at(&b, 3, "study page");
        assert_eq!(b[(x, y)].modifier, Modifier::DIM);
        // narrower: the page goes, then the queue
        let l = lines(&framed(100, 24, &noted(Work::Page), Pane::Transcript, &theme).backend().buffer().clone());
        assert!(l[3].contains("snapshot: writing   1 queued") && !l[3].contains("study page"), "{:?}", l[3]);
        let l = lines(&framed(60, 16, &noted(Work::Page), Pane::Notes, &theme).backend().buffer().clone());
        assert!(l[2].starts_with(" Transcript   Notes   Slides") && l[2].ends_with("snapshot: writing"), "{:?}", l[2]);
        // stacked: the lane ends the notes rule
        let l = lines(&framed(72, 45, &noted(Work::Writing), Pane::Transcript, &theme).backend().buffer().clone());
        let rule = l.iter().find(|r| r.contains("── Notes ")).unwrap();
        assert!(rule.ends_with("─ snapshot: writing"), "{rule:?}");
    }

    /// `NO_COLOR`: no colour, and the preview still reads as provisional by its word and its edge.
    /// ASCII: the bullet, quote, slide and edge fall back; the notes' own words stay as written.
    #[test]
    fn the_notes_without_colour_and_in_ascii() {
        let b = framed(140, 40, &noted(Work::Writing), Pane::Transcript, &Theme::new(OFF, true)).backend().buffer().clone();
        assert!(b.content.iter().all(|c| c.fg == Color::Reset && c.bg == Color::Reset));
        let rows = notes_rows(&b, 140, 40);
        assert!(rows.iter().any(|r| r.starts_with("writing ▎ Provisional")) && rows.iter().any(|r| r.contains("• Larger")), "{rows:?}");
        let b = framed(80, 25, &noted(Work::Writing), Pane::Notes, &Theme::new(OFF, false)).backend().buffer().clone();
        let rows = notes_rows(&b, 80, 25);
        let text = rows.join("\n");
        for want in ["writing | Provisional", "        | - tentative", "- 標本平均 is the sample mean 🎓", "| Keep the histogram"] {
            assert!(text.contains(want), "{want:?} in {rows:?}");
        }
        assert!(!text.contains('▎') && !text.contains('•') && !text.contains('│'), "{rows:?}");
        let b = framed(140, 40, &noted(Work::Committed), Pane::Transcript, &Theme::new(OFF, false)).backend().buffer().clone();
        assert!(notes_rows(&b, 140, 40).iter().any(|r| r.contains("[] Slide 17  10:39:41")));
        for (w, h) in TRANSCRIPT_SIZES {
            assert!(framed(w, h, &noted(Work::Writing), focus_for(w, h), &Theme::new(TRUE, true)).backend().buffer().content.iter().all(|c| c.bg == Color::Reset), "{w}×{h}: no background");
        }
    }

    // ---- the hint line, overlays and zoom (Task 10) -------------------------------------------

    /// What the reactor's own state shows, for a test frame.
    #[derive(Default)]
    struct Look {
        focus: Option<Pane>,
        zoom: bool,
        overlay: Overlay,
        hint: Hint,
        help: usize,
        activity: ActivityScroll,
        notes: NoteScroll,
        refused: Option<&'static str>,
    }

    fn looked(w: u16, h: u16, v: &View, look: &Look, theme: &Theme) -> (Terminal<TestBackend>, Drawn) {
        let mut preview = Preview::default();
        preview.refresh(&v.notes, std::time::Instant::now());
        let scroll = Scroll::default();
        let chrome = Chrome { elapsed: Duration::from_secs(42 * 60 + 18), refused: look.refused, focus: look.focus.unwrap_or(Pane::Transcript), zoom: look.zoom, transcript: &scroll, notes: &look.notes, preview: &preview, hint: &look.hint, overlay: look.overlay, activity: &look.activity, help: look.help, theme };
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        let mut drawn = Drawn::default();
        terminal.draw(|f| drawn = render(f, v, &chrome)).unwrap();
        (terminal, drawn)
    }

    fn typed(text: &str) -> Hint {
        let mut h = Hint::default();
        h.paste(text).unwrap();
        h
    }

    /// Plan Task 10: the cursor the frame sets is where `tui-input`'s `visual_cursor()` says, in
    /// cells after the prompt's mark — for ASCII, CJK, emoji and combining marks — and the text
    /// drawn before it takes exactly those cells. A hint longer than the field scrolls, and the
    /// cursor never leaves the prompt's row or its width.
    #[test]
    fn the_cursor_sits_where_tui_input_counts_it() {
        let theme = Theme::new(TRUE, true);
        for text in ["focus on treatment differences", "機械学習の最適化", "🎓 graduation ❤\u{FE0F}", "cafe\u{301} na\u{303}o", "👩\u{200D}🔬 lab"] {
            let look = Look { hint: typed(text), ..Look::default() };
            let (mut t, _) = looked(110, 32, &view(Stage::Listening), &look, &theme);
            let cursor = t.get_cursor_position().unwrap();
            assert_eq!((cursor.x, cursor.y), (3 + look.hint.cursor() as u16, 30), "{text:?}");
            let b = t.backend().buffer();
            let (mut drawn, mut x) = (String::new(), 3);
            while x < cursor.x {
                let sym = b[(x, 30)].symbol();
                drawn.push_str(sym);
                x += (Span::raw(sym).width() as u16).max(1); // a wide glyph once, not its covered cell
            }
            assert_eq!(x, cursor.x, "{text:?}: the cursor lands after a whole glyph");
            assert_eq!(drawn, text, "the cells before the cursor hold the hint exactly");
        }
        // longer than the field: the cursor stays inside it, on the last cell at most
        for w in [60, 80, 140] {
            let look = Look { hint: typed(&"漢字 and words ".repeat(20)), ..Look::default() };
            let (mut t, _) = looked(w, 24, &view(Stage::Listening), &look, &theme);
            let cursor = t.get_cursor_position().unwrap();
            assert!(cursor.x >= 3 && cursor.x < w - 1 && cursor.y == 22, "{w}: {cursor:?}");
            assert_eq!(cursor.x as usize, 3 + look.hint.cursor() - look.hint.scroll(w as usize - 4), "{w}");
        }
        // the empty field: the cursor waits at its start
        let (mut t, _) = looked(110, 32, &view(Stage::Listening), &Look::default(), &theme);
        assert_eq!(t.get_cursor_position().unwrap(), ratatui::layout::Position::new(3, 30));
    }

    /// The hint in ink after the teal mark; empty, the dim placeholder.
    #[test]
    fn the_hint_line_styles() {
        let theme = Theme::new(TRUE, true);
        let (t, _) = looked(110, 32, &view(Stage::Listening), &Look { hint: typed("focus on treatment"), ..Look::default() }, &theme);
        let b = t.backend().buffer();
        assert_eq!(b[(1, 30)].fg, TEAL);
        assert_eq!((b[(3, 30)].symbol(), b[(3, 30)].modifier), ("f", Modifier::empty()));
        let (mut t, _) = looked(110, 32, &view(Stage::Listening), &Look::default(), &theme);
        assert_eq!(t.get_cursor_position().unwrap(), ratatui::layout::Position::new(3, 30));
        let b = t.backend().buffer();
        assert_eq!(b[(3, 30)].symbol(), " ", "the cursor's cell is empty: its block hides no letter");
        assert_eq!((b[(4, 30)].symbol(), b[(4, 30)].modifier), ("a", Modifier::DIM), "the placeholder starts after it");
    }

    /// The activity with enough history to have let some go.
    fn busy_day() -> View {
        let mut v = noted(Work::Committed);
        for k in 0..510 {
            v.reduce(&Event::Warning(format!("warning number {k}")), fixed());
            if k % 7 == 0 {
                v.reduce(&Event::Busy(format!("snapshot, {k} words to the model")), fixed());
            }
        }
        commit(&mut v, REAL, 2);
        v.reduce(&Event::SnapshotFailed("the model timed out after 600 s; everything is kept for the next one".into()), fixed());
        v
    }

    /// Plan Task 10's goldens: the hint with content, help, activity, the notes and slides as the
    /// shown tab in narrow, and the zoomed transcript and notes.
    #[test]
    fn goldens_for_the_hint_overlays_and_zoom() {
        let theme = Theme::new(TRUE, true);
        let hint = || typed("focus on treatment differences and the 95% interval");
        for (w, h) in [(140, 40), (60, 16)] {
            golden(&format!("hint_{w}x{h}"), &looked(w, h, &noted(Work::Writing), &Look { hint: hint(), ..Look::default() }, &theme).0);
        }
        for (w, h) in [(140, 40), (110, 32), (127, 36), (80, 25), (72, 45), (60, 16)] {
            golden(&format!("help_{w}x{h}"), &looked(w, h, &noted(Work::Writing), &Look { overlay: Overlay::Help, ..Look::default() }, &theme).0);
        }
        for (w, h) in [(140, 40), (80, 25), (60, 16)] {
            golden(&format!("activity_{w}x{h}"), &looked(w, h, &busy_day(), &Look { overlay: Overlay::Activity, ..Look::default() }, &theme).0);
        }
        for (w, h) in [(80, 25), (60, 16)] {
            golden(&format!("narrow_notes_{w}x{h}"), &looked(w, h, &noted(Work::Writing), &Look { focus: Some(Pane::Notes), ..Look::default() }, &theme).0);
            golden(&format!("narrow_slides_{w}x{h}"), &looked(w, h, &noted(Work::Writing), &Look { focus: Some(Pane::Slides), ..Look::default() }, &theme).0);
        }
        for (w, h) in [(110, 32), (72, 45)] {
            golden(&format!("zoom_transcript_{w}x{h}"), &looked(w, h, &noted(Work::Writing), &Look { zoom: true, ..Look::default() }, &theme).0);
        }
        for (w, h) in [(140, 40), (80, 25)] {
            golden(&format!("zoom_notes_{w}x{h}"), &looked(w, h, &noted(Work::Writing), &Look { zoom: true, focus: Some(Pane::Notes), ..Look::default() }, &theme).0);
        }
        golden("help_ascii_80x25", &looked(80, 25, &noted(Work::Writing), &Look { overlay: Overlay::Help, ..Look::default() }, &Theme::new(OFF, false)).0);
        golden("activity_ascii_80x25", &looked(80, 25, &busy_day(), &Look { overlay: Overlay::Activity, ..Look::default() }, &Theme::new(OFF, false)).0);
    }

    /// The help overlay: one border at most 64 wide, centred in the body; the grammar, the groups,
    /// the stop stages and the spend sentence; no capture key yet; it scrolls itself when taller
    /// than the body, and says so.
    #[test]
    fn the_help_overlay() {
        let theme = Theme::new(TRUE, true);
        let (t, drawn) = looked(140, 40, &view(Stage::Listening), &Look { overlay: Overlay::Help, ..Look::default() }, &theme);
        let l = lines(t.backend().buffer());
        let top = l.iter().position(|r| r.contains("┌─ Help ")).unwrap();
        let bottom = l.iter().rposition(|r| r.contains('┘')).unwrap();
        let x = l[top].find('┌').unwrap();
        let width = l[top][x..].chars().position(|c| c == '┐').unwrap() + 1;
        assert_eq!(width, 64);
        assert_eq!(x - 1, (138 - 64) / 2, "centred in the body");
        let text = l[top..=bottom].join("\n");
        for want in ["Notes", "a snapshot now", "hint ⏎", "polish⏎", "^X", "Reading", "PgUp PgDn", "Tab ⇧Tab", "^T", "Slides", "App", "^O", "^H F1", "^L", "^Z", "Stopping", "^C once", "^C twice", "^C thrice", "Spend is this lecture's cost today;", "recording closes."] {
            assert!(text.contains(want), "{want:?} in\n{text}");
        }
        let prose: String = l[top + 1..bottom].iter().map(|r| r.chars().skip(x + 2).take(60).collect::<String>().trim().to_string()).collect::<Vec<_>>().join(" ");
        let prose = prose.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(prose.contains("stop waiting: recovery and queued requests wait for the next session; what runs now and the last snapshot still finish"), "stage 2 never claims to abort what runs: {prose}");
        assert!(prose.contains("Spend is this lecture's cost today; speech-to-text is added when a recording closes."), "{prose}");
        assert!(!text.contains("^S"));
        assert_eq!(drawn.help_max, 0, "it all fits at 140×40");
        assert!(l[bottom].contains("Esc closes"));
        // small: it scrolls, and the border says there is more
        let (t, drawn) = looked(60, 16, &view(Stage::Listening), &Look { overlay: Overlay::Help, ..Look::default() }, &theme);
        assert!(drawn.help_max > 0);
        assert!(lines(t.backend().buffer()).iter().any(|r| r.contains("↑↓ more")));
        let (t, _) = looked(60, 16, &view(Stage::Listening), &Look { overlay: Overlay::Help, help: drawn.help_max, ..Look::default() }, &theme);
        let l = lines(t.backend().buffer());
        assert!(l.iter().any(|r| r.contains("recording closes.")), "scrolled to the end: {l:?}");
        // ASCII border
        let (t, _) = looked(80, 25, &view(Stage::Listening), &Look { overlay: Overlay::Help, ..Look::default() }, &Theme::new(OFF, false));
        let l = lines(t.backend().buffer());
        let top = l.iter().position(|r| r.contains("+- Help ")).unwrap();
        let bottom = l.iter().rposition(|r| r.contains("more -+") || r.contains("closes -+")).unwrap();
        assert!(l[top..=bottom].iter().all(|r| r.is_ascii()), "the box is ASCII (the lecture's own title never is transliterated): {l:?}");
        assert!(l.iter().any(|r| r.contains("| Tab Shift-Tab  the next or previous pane")), "a key never runs into what it does: {l:?}");
        // header, notice, prompt and keys stay
        let (t, _) = looked(110, 32, &view(Stage::Listening), &Look { overlay: Overlay::Help, ..Look::default() }, &theme);
        let l = lines(t.backend().buffer());
        assert!(l[0].contains("Listening") && l[30].starts_with(" ◆ ") && l[31] == " ↑↓ scroll   Esc close   ^C stop", "{:?}", &l[29..]);
    }

    /// The activity overlay fills the body: newest last, under their times, marked as the notice
    /// line marks them; the first line says older records are gone once the ring has wrapped.
    #[test]
    fn the_activity_overlay() {
        let theme = Theme::new(TRUE, true);
        let v = busy_day();
        let (t, drawn) = looked(140, 40, &v, &Look { overlay: Overlay::Activity, ..Look::default() }, &theme);
        let b = t.backend().buffer().clone();
        let l = lines(&b);
        let body = drawn.activity.unwrap();
        assert_eq!((body.x, body.width), (1, 138), "the whole width");
        assert!(l[3].starts_with(" Activity"), "{:?}", l[3]);
        let last = (body.bottom() - 1) as usize;
        assert!(l[last].contains("▲ snapshot failed  the model timed out"), "newest last: {:?}", l[last]);
        let (x, y) = at(&b, last as u16, "▲");
        assert_eq!(b[(x, y)].fg, SIGNAL);
        let (x, y) = at(&b, last as u16, "snapshot failed");
        assert_eq!(b[(x, y)].modifier, Modifier::BOLD);
        assert_eq!(b[(1, last as u16)].modifier, Modifier::DIM, "the time gutter");
        let rows = l[body.y as usize..last + 1].join("\n");
        assert!(rows.contains("… snapshot, "), "busy text as it was said");
        // scrolled to the top: the first line says what is gone
        let mut s = ActivityScroll::default();
        let sign = |kind: &str| mark(kind, &theme);
        assert!(s.apply(panes::Move::Up(100_000), &v.activity, body, &sign));
        let (t, _) = looked(140, 40, &v, &Look { overlay: Overlay::Activity, activity: s, ..Look::default() }, &theme);
        assert_eq!(lines(t.backend().buffer())[body.y as usize], format!(" {}", panes::OLDER_GONE));
        // a ring that never wrapped says nothing of the kind
        let (t, _) = looked(140, 40, &noted(Work::Committed), &Look { overlay: Overlay::Activity, ..Look::default() }, &theme);
        assert!(!lines(t.backend().buffer()).join("\n").contains(panes::OLDER_GONE));
        // ASCII marks
        let (t, _) = looked(80, 25, &v, &Look { overlay: Overlay::Activity, ..Look::default() }, &Theme::new(OFF, false));
        let l = lines(t.backend().buffer());
        assert!(l.iter().any(|r| r.contains("! snapshot failed")) && l.iter().any(|r| r.contains("... snapshot,")), "{l:?}");
    }

    /// Ctrl-T: the focused pane fills the body — its heading names every pane — while the header,
    /// notice, prompt and keys stay where they were; the breakpoints are the ladder's.
    #[test]
    fn zoom_fills_the_body_with_the_focused_pane() {
        let theme = Theme::new(TRUE, true);
        for (w, h) in [(140, 40), (110, 32), (72, 45), (80, 25), (60, 16)] {
            for focus in [Pane::Transcript, Pane::Notes] {
                let v = noted(Work::Writing);
                let (t, drawn) = looked(w, h, &v, &Look { zoom: true, focus: Some(focus), ..Look::default() }, &theme);
                let (plain, _) = looked(w, h, &v, &Look { focus: Some(focus), ..Look::default() }, &theme);
                let (l, p) = (lines(t.backend().buffer()), lines(plain.backend().buffer()));
                let body = if focus == Pane::Transcript { drawn.transcript } else { drawn.notes }.unwrap();
                assert_eq!((body.x, body.width), (1, w - 2), "{w}×{h} {focus:?}: the whole width");
                assert!(drawn.transcript.is_none() || drawn.notes.is_none(), "one pane");
                assert_eq!(l[..2], p[..2], "{w}×{h}: the header stays");
                assert_eq!(l[h as usize - 3..h as usize - 1], p[h as usize - 3..h as usize - 1], "{w}×{h}: the notice and the prompt stay");
                assert!(l[h as usize - 1].contains("^T back") && l[h as usize - 1].ends_with("^C stop"), "{w}×{h}: the keys say how to go back: {:?}", l[h as usize - 1]);
                assert!(!l.join("").contains('│') || focus == Pane::Notes, "{w}×{h}: no column rule");
            }
        }
        let (t, _) = looked(110, 32, &noted(Work::Writing), &Look { zoom: true, ..Look::default() }, &theme);
        let l = lines(t.backend().buffer());
        assert!(l[3].starts_with(" Transcript   Notes   Slides"), "{:?}", l[3]);
        assert_eq!(layout(Rect::new(0, 0, 110, 32)).variant, Variant::Normal, "the ladder is unchanged");
    }

    /// Narrow shows only the focused pane; Slides is a placeholder until Task 11.
    #[test]
    fn narrow_shows_the_focused_pane() {
        let theme = Theme::new(TRUE, true);
        let (t, d) = looked(80, 25, &noted(Work::Writing), &Look { focus: Some(Pane::Slides), ..Look::default() }, &theme);
        let l = lines(t.backend().buffer());
        assert!(l[3].contains("Slides show here.") && d.transcript.is_none() && d.notes.is_none());
        let (x, y) = at(t.backend().buffer(), 2, "Slides 1");
        assert_eq!(t.backend().buffer()[(x, y)].fg, TEAL);
    }

    /// A scrolled notes pane says the way back in its heading.
    #[test]
    fn scrolled_notes_say_esc_live() {
        let theme = Theme::new(TRUE, true);
        let v = noted(Work::Writing);
        let (_, drawn) = looked(80, 25, &v, &Look { focus: Some(Pane::Notes), ..Look::default() }, &theme);
        let mut notes = NoteScroll::default();
        let mut preview = Preview::default();
        preview.refresh(&v.notes, std::time::Instant::now());
        assert!(notes.apply(panes::Move::Up(3), &v, &preview, drawn.notes.unwrap()));
        let (t, _) = looked(80, 25, &v, &Look { focus: Some(Pane::Notes), notes, ..Look::default() }, &theme);
        assert!(lines(t.backend().buffer())[2].ends_with("snapshot: writing   Esc live"), "{:?}", lines(t.backend().buffer())[2]);
    }

    /// Both bullet glyphs are one cell: the notes walker counts them as one.
    #[test]
    fn bullets_are_one_cell() {
        assert_eq!((Span::raw(UNICODE.bullet).width(), Span::raw(ASCII.bullet).width()), (1, 1));
    }
}
