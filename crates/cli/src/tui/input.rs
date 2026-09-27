//! The keyboard, pure (M7 plan §H key map): what a key means, in priority order — the app's Ctrl
//! chords before anything else, then an open overlay's keys, the reading keys, Enter, Tab and Esc,
//! and last the hint line's own editing, which `tui-input` does. And the hint line itself: its
//! value, its 8 KiB limit, pasted text made one line and never a submission, and the Enter debounce
//! that keeps a held key from sending a stream of snapshots.
//!
//! Nothing here sends anything: the reactor does, once per accepted key (plan §C 8).

use std::time::{Duration, Instant};

use ratatui::crossterm::event::{Event as TermEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use tui_input::backend::crossterm::EventHandler;
use tui_input::Input;

use super::panes::Move;

/// The most a hint may hold, in UTF-8 bytes.
pub(crate) const HINT_CAP: usize = 8 * 1024;
/// An Enter is taken only this long after the Enter event before it, whatever that one did.
pub(crate) const ENTER_QUIET: Duration = Duration::from_millis(300);

/// Which overlay covers the body: one at a time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Overlay {
    #[default]
    None,
    Help,
    Activity,
}

/// What a key asks for, before the state it lands in is consulted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Key {
    Nothing,
    /// Ctrl-C: the next stop stage.
    Stop,
    /// Ctrl-Z: a notice only.
    Suspend,
    /// Ctrl-L: a full redraw.
    Clear,
    /// Ctrl-X: cancel this TUI's notes requests.
    Cancel,
    /// Ctrl-H or F1. (Ctrl-G is taken by a system-wide shortcut on the person's Mac.)
    Help,
    /// Ctrl-O.
    Activity,
    /// Ctrl-T: the reading pane fills the body.
    Zoom,
    /// Ctrl-S (Task 11): capture now, or watch the one window offered.
    Capture,
    /// Esc with an overlay open.
    Close,
    /// A reading move: the overlay's, else the focused pane's.
    Read(Move),
    /// Tab (`true`) or Shift-Tab.
    Focus(bool),
    Enter,
    /// Anything else goes to the hint line's editor.
    Edit,
}

/// A key's meaning (plan §H), in priority order: a release is nothing; the app's Ctrl chords are
/// taken before the editor can see them (Ctrl-S is Task 11's capture action); F1 is help; Esc
/// closes an overlay before it means "back to live"; then the reading keys, Tab, Enter, and the
/// editor's keys last. A key with Ctrl and Alt together is text (AltGr), not a chord.
pub(crate) fn classify(key: &KeyEvent, overlay: Overlay) -> Key {
    if key.kind == KeyEventKind::Release {
        return Key::Nothing;
    }
    if key.modifiers - KeyModifiers::SHIFT == KeyModifiers::CONTROL {
        if let KeyCode::Char(c) = key.code {
            return match c.to_ascii_lowercase() {
                'c' => Key::Stop,
                'z' => Key::Suspend,
                'l' => Key::Clear,
                'x' => Key::Cancel,
                'h' => Key::Help,
                'o' => Key::Activity,
                't' => Key::Zoom,
                's' => Key::Capture,
                _ => Key::Edit,
            };
        }
    }
    match key.code {
        KeyCode::F(1) => Key::Help,
        KeyCode::Esc if overlay != Overlay::None => Key::Close,
        KeyCode::Esc => Key::Read(Move::Live),
        KeyCode::Up if key.modifiers.is_empty() => Key::Read(Move::Up(1)),
        KeyCode::Down if key.modifiers.is_empty() => Key::Read(Move::Down(1)),
        KeyCode::PageUp => Key::Read(Move::PageUp),
        KeyCode::PageDown => Key::Read(Move::PageDown),
        KeyCode::Tab => Key::Focus(true),
        KeyCode::BackTab => Key::Focus(false),
        KeyCode::Enter => Key::Enter,
        _ => Key::Edit,
    }
}

/// Why the hint line took nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refused {
    /// The field is at its limit.
    Full,
    /// A paste would take it past its limit; nothing of it was inserted.
    PasteTooLong,
}

/// The hint line: always live (plan §H), edited by `tui-input`, and the time of the last Enter.
#[derive(Debug, Default)]
pub(crate) struct Hint {
    input: Input,
    last_enter: Option<Instant>,
}

impl Hint {
    pub(crate) fn value(&self) -> &str {
        self.input.value()
    }

    /// The cursor's column in the text, in terminal cells.
    pub(crate) fn cursor(&self) -> usize {
        self.input.visual_cursor()
    }

    /// The cells of text scrolled off the left of a field `width` cells wide, keeping the cursor
    /// inside it.
    pub(crate) fn scroll(&self, width: usize) -> usize {
        self.input.visual_scroll(width.saturating_sub(1))
    }

    /// An editing key, through `tui-input`. An edit that would take the field past its limit is
    /// not made. Whether anything changed.
    pub(crate) fn edit(&mut self, key: KeyEvent) -> Result<bool, Refused> {
        let mut next = self.input.clone();
        if next.handle_event(&TermEvent::Key(key)).is_none() {
            return Ok(false);
        }
        if next.value().len() > HINT_CAP {
            return Err(Refused::Full);
        }
        self.input = next;
        Ok(true)
    }

    /// A bracketed paste: text, never a key. Line breaks and tabs become spaces and anything that
    /// could act on the terminal is cleaned away, so a pasted "polish", newline or Ctrl-C submits
    /// and stops nothing. All of it goes in at the cursor, or — past the limit — none of it.
    pub(crate) fn paste(&mut self, text: &str) -> Result<bool, Refused> {
        let text: String = crate::plain::clean(text).chars().map(|c| if c == '\n' || c == '\t' { ' ' } else { c }).collect();
        if text.is_empty() {
            return Ok(false);
        }
        if self.input.value().len() + text.len() > HINT_CAP {
            return Err(Refused::PasteTooLong);
        }
        let at = self.input.cursor();
        let value = self.input.value();
        let split = value.char_indices().nth(at).map_or(value.len(), |(i, _)| i);
        let joined = format!("{}{text}{}", &value[..split], &value[split..]);
        self.input = Input::new(joined).with_cursor(at + text.chars().count());
        Ok(true)
    }

    /// An Enter key event at `now`: taken only [`ENTER_QUIET`] after the one before, and every one
    /// — taken or not — starts the quiet again, so a held Enter's repeats are never taken.
    pub(crate) fn enter(&mut self, now: Instant) -> bool {
        let taken = self.last_enter.is_none_or(|t| now.saturating_duration_since(t) >= ENTER_QUIET);
        self.last_enter = Some(now);
        taken
    }

    /// The hint was sent: the field empties.
    pub(crate) fn clear(&mut self) {
        self.input.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plain::parse_line;
    use lecturelive_core::session::lecture::Op;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn typed(s: &str) -> Hint {
        let mut h = Hint::default();
        for c in s.chars() {
            h.edit(key(KeyCode::Char(c))).unwrap();
        }
        h
    }

    /// The app's chords come first: tui-input never sees Ctrl-X, -H, -O, -T, -C, -Z, -L or -S; its
    /// own Ctrl-A/E/W/U/K still edit; Ctrl-S is the capture action (Task 11).
    #[test]
    fn app_chords_are_taken_before_the_editor() {
        for (c, want) in [('c', Key::Stop), ('z', Key::Suspend), ('l', Key::Clear), ('x', Key::Cancel), ('h', Key::Help), ('o', Key::Activity), ('t', Key::Zoom), ('s', Key::Capture)] {
            assert_eq!(classify(&ctrl(c), Overlay::None), want, "^{c}");
            assert_eq!(classify(&ctrl(c), Overlay::Help), want, "^{c} with help open");
        }
        for c in ['a', 'e', 'w', 'u', 'k', 'b', 'f'] {
            assert_eq!(classify(&ctrl(c), Overlay::None), Key::Edit, "^{c}");
        }
        assert_eq!(classify(&KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL | KeyModifiers::ALT), Overlay::None), Key::Edit, "AltGr text");
        assert_eq!(classify(&key(KeyCode::F(1)), Overlay::None), Key::Help);
        // Ctrl-H (0x08) is help; Backspace arrives as its own key (Terminal sends 0x7F) and still edits
        assert_eq!(classify(&key(KeyCode::Backspace), Overlay::None), Key::Edit);
        let mut h = typed("ab");
        h.edit(key(KeyCode::Backspace)).unwrap();
        assert_eq!(h.value(), "a");
        assert_eq!(classify(&ctrl('g'), Overlay::None), Key::Edit, "Ctrl-G no longer opens help");
        let mut release = ctrl('c');
        release.kind = KeyEventKind::Release;
        assert_eq!(classify(&release, Overlay::None), Key::Nothing);
    }

    /// Esc closes an overlay before it acts on a pane; the reading keys, Tab and Enter are named;
    /// everything else is the editor's.
    #[test]
    fn named_keys_and_esc_priority() {
        assert_eq!(classify(&key(KeyCode::Esc), Overlay::Activity), Key::Close);
        assert_eq!(classify(&key(KeyCode::Esc), Overlay::Help), Key::Close);
        assert_eq!(classify(&key(KeyCode::Esc), Overlay::None), Key::Read(Move::Live));
        assert_eq!(classify(&key(KeyCode::Up), Overlay::None), Key::Read(Move::Up(1)));
        assert_eq!(classify(&key(KeyCode::PageDown), Overlay::None), Key::Read(Move::PageDown));
        assert_eq!(classify(&key(KeyCode::Tab), Overlay::None), Key::Focus(true));
        assert_eq!(classify(&KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT), Overlay::None), Key::Focus(false));
        assert_eq!(classify(&key(KeyCode::Enter), Overlay::None), Key::Enter);
        for k in [KeyCode::Left, KeyCode::Right, KeyCode::Home, KeyCode::End, KeyCode::Backspace, KeyCode::Delete, KeyCode::Char('q')] {
            assert_eq!(classify(&key(k), Overlay::None), Key::Edit, "{k:?}");
        }
    }

    /// tui-input's editing, as the hint line uses it.
    #[test]
    fn the_editor_edits() {
        let mut h = typed("focus on treatment");
        h.edit(ctrl('w')).unwrap();
        assert_eq!(h.value(), "focus on ");
        h.edit(ctrl('a')).unwrap();
        h.edit(key(KeyCode::Delete)).unwrap();
        assert_eq!(h.value(), "ocus on ");
        h.edit(ctrl('e')).unwrap();
        h.edit(key(KeyCode::Backspace)).unwrap();
        assert_eq!(h.value(), "ocus on");
        h.edit(key(KeyCode::Home)).unwrap();
        h.edit(ctrl('k')).unwrap();
        assert_eq!(h.value(), "");
        let mut h = typed("abc");
        h.edit(ctrl('u')).unwrap();
        assert_eq!(h.value(), "");
        assert_eq!(h.edit(key(KeyCode::Up)), Ok(false), "not an editing key");
    }

    /// Plan §J `paste_cannot_submit`: pasted text — a "polish", line breaks, a Ctrl-C byte, an
    /// escape — becomes one line of the hint and nothing else; the grammar only ever sees it when
    /// a real Enter follows.
    #[test]
    fn paste_cannot_submit() {
        let mut h = typed("see ");
        assert_eq!(h.paste("polish\r\nthe\rproof\n\twith\u{3} \x1b[31mcare"), Ok(true));
        assert_eq!(h.value(), "see polish the proof  with care");
        assert!(!h.value().chars().any(char::is_control));
        let mut only = Hint::default();
        only.paste("polish\n").unwrap();
        assert_eq!(only.value(), "polish ", "a pasted polish is text");
        assert_eq!(parse_line(only.value()), Op::Polish, "until a real Enter reads it");
        // inserted at the cursor
        let mut h = typed("ab");
        h.edit(key(KeyCode::Left)).unwrap();
        h.paste("漢字").unwrap();
        assert_eq!(h.value(), "a漢字b");
        h.edit(key(KeyCode::Char('!'))).unwrap();
        assert_eq!(h.value(), "a漢字!b", "the cursor is after the paste");
        assert_eq!(Hint::default().paste("\u{3}\x1b[2J"), Ok(false), "nothing left to insert");
    }

    /// Plan §J `over_cap_paste_is_refused`: a paste that would pass 8 KiB inserts nothing and
    /// leaves the field exactly as it was; typing stops at the limit too.
    #[test]
    fn over_cap_paste_is_refused() {
        let mut h = typed("keep me");
        h.edit(key(KeyCode::Left)).unwrap();
        let (before, cursor) = (h.value().to_string(), h.cursor());
        assert_eq!(h.paste(&"x".repeat(HINT_CAP - 6)), Err(Refused::PasteTooLong));
        assert_eq!((h.value(), h.cursor()), (before.as_str(), cursor), "exactly as it was");
        assert_eq!(h.paste(&"x".repeat(HINT_CAP - 7)), Ok(true), "exactly to the limit is fine");
        assert_eq!(h.value().len(), HINT_CAP);
        assert_eq!(h.edit(key(KeyCode::Char('y'))), Err(Refused::Full));
        assert_eq!(h.value().len(), HINT_CAP);
        let mut wide = Hint::default();
        wide.paste(&"é".repeat(HINT_CAP / 2)).unwrap();
        assert_eq!(wide.edit(key(KeyCode::Char('é'))), Err(Refused::Full), "bytes, not characters");
    }

    /// Held Enter: taken at once, then only after a 300 ms quiet since the last Enter event —
    /// macOS's first repeat after a long delay may be taken; the fast repeats after it never are.
    #[test]
    fn enter_is_debounced_against_a_held_key() {
        let (mut h, t0) = (Hint::default(), Instant::now());
        let ms = |n| t0 + Duration::from_millis(n);
        assert!(h.enter(t0));
        assert!(!h.enter(ms(100)));
        assert!(!h.enter(ms(350)), "300 ms since the last event, not since the last taken");
        assert!(h.enter(ms(700)), "a real second press");
        // a held key: first press, a 500 ms repeat delay, then repeats every 33 ms for two seconds
        let mut h = Hint::default();
        let mut taken = usize::from(h.enter(t0));
        let mut t = 500;
        taken += usize::from(h.enter(ms(t)));
        while t < 2500 {
            t += 33;
            taken += usize::from(h.enter(ms(t)));
        }
        assert_eq!(taken, 2, "the press and the first repeat, never the stream");
    }

    /// Plan Task 10: tui-input's `visual_cursor()` counts cells as the field draws them — ASCII,
    /// CJK, emoji and combining marks — and the scroll keeps the cursor inside the field.
    #[test]
    fn the_cursor_is_counted_in_cells() {
        for (text, cells) in [("abc", 3), ("漢字", 4), ("🎓x", 3), ("cafe\u{301}", 4), ("👩\u{200D}🔬", 2)] {
            let h = typed(text);
            assert_eq!(h.cursor(), cells, "{text:?}");
            assert_eq!(h.cursor(), unicode_width::UnicodeWidthStr::width(text), "{text:?}");
        }
        let h = typed(&"漢".repeat(30));
        for width in [5, 10, 11, 58] {
            let s = h.scroll(width);
            assert!(h.cursor() - s < width, "{width}: the cursor at {} of a {width}-cell field", h.cursor() - s);
        }
    }
}
