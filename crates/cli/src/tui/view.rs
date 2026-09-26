//! The minimal view of Task 5 (M7 plan §K): the stop stage, the elapsed time, the events drained and the
//! keys, enough to prove the terminal's lifecycle. Task 7's frame replaces it. Modifiers only (no colour
//! yet), no borders; the phase row and the keys row win when the terminal is tiny.

use std::time::Duration;

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::Frame;

use crate::stop::Stage;

const NAME: &str = "LectureLive";
const BOLD: Style = Style::new().add_modifier(Modifier::BOLD);
const DIM: Style = Style::new().add_modifier(Modifier::DIM);

pub(crate) const SUSPEND: &str = "Suspending would stop the recording. Stop the lecture first (Ctrl-C).";

/// What the view shows.
pub(crate) struct Status {
    pub(crate) stage: Stage,
    pub(crate) elapsed: Duration,
    pub(crate) events: u64,
    pub(crate) notice: Option<&'static str>,
}

/// The notice for a Ctrl-C the stop controller did not take: a held key repeating, or too soon after the stage before.
pub(crate) fn not_yet(stage: Stage) -> &'static str {
    match stage {
        Stage::StopWaiting => "Wait a moment, then Ctrl-C again to quit at once.",
        _ => "Wait a moment, then Ctrl-C again to stop waiting.",
    }
}

/// The stage as one word or phrase. Stop waiting is reversed: the next Ctrl-C ends the process at once.
fn phase(stage: Stage) -> Span<'static> {
    match stage {
        Stage::Listening => Span::styled("Listening", BOLD),
        Stage::Stopping => Span::styled("Stopping", BOLD),
        Stage::StopWaiting => Span::styled("Stop waiting", BOLD.add_modifier(Modifier::REVERSED)),
    }
}

/// What the stage means for the lecture (plan §G's words).
fn meaning(stage: Stage) -> &'static str {
    match stage {
        Stage::Listening => "Recording the lecture.",
        Stage::Stopping => "Finishing the transcript and recovery, then a last snapshot.",
        Stage::StopWaiting => "No longer waiting for recovery or queued requests; they wait for the next session. What is running now, and the last snapshot, still finish.",
    }
}

fn keys(stage: Stage) -> &'static str {
    match stage {
        Stage::Listening => "^C stop   ^L redraw",
        Stage::Stopping => "^C stop waiting   ^L redraw",
        Stage::StopWaiting => "^C quit at once   ^L redraw",
    }
}

fn clock(elapsed: Duration) -> String {
    let s = elapsed.as_secs();
    format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

fn dim(text: String) -> Line<'static> {
    Line::from(vec![Span::raw(" "), Span::styled(text, DIM)])
}

pub(crate) fn render(frame: &mut Frame, s: &Status) {
    let rows: Vec<Rect> = frame.area().rows().collect();
    let h = rows.len();
    // Keys on the last row from 2 rows up, the notice above them from 4; the top rows take what is left.
    let bottom = match h {
        0..=1 => 0,
        2..=3 => 1,
        _ => 2,
    };
    let header = Line::from(vec![Span::raw(" "), phase(s.stage), Span::raw("  "), Span::raw(clock(s.elapsed))]);
    let used = header.width();
    let events = format!("{} event{} received", s.events, if s.events == 1 { "" } else { "s" });
    for (line, row) in [header, dim(meaning(s.stage).into()), dim(events)].into_iter().zip(&rows[..h - bottom]) {
        frame.render_widget(line, *row);
    }
    if let Some(&first) = rows.first() {
        if first.width as usize >= used + 2 + NAME.len() + 1 {
            // The style on the span, not the line: a line's own style would dim its whole row, the phase included.
            frame.render_widget(Line::from(Span::styled(NAME, DIM)).right_aligned(), Rect { width: first.width - 1, ..first });
        }
    }
    if bottom >= 1 {
        frame.render_widget(dim(keys(s.stage).into()), rows[h - 1]);
    }
    if let (2, Some(notice)) = (bottom, s.notice) {
        frame.render_widget(Line::from(vec![Span::raw(" "), Span::raw(notice)]), rows[h - 2]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::Terminal;

    fn status(stage: Stage) -> Status {
        Status { stage, elapsed: Duration::from_secs(3725), events: 124, notice: None }
    }

    fn drawn(width: u16, height: u16, s: &Status) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| render(f, s)).unwrap();
        terminal.backend().buffer().clone()
    }

    fn lines(b: &Buffer) -> Vec<String> {
        (0..b.area.height).map(|y| (0..b.area.width).map(|x| b[(x, y)].symbol()).collect::<String>().trim_end().to_string()).collect()
    }

    #[test]
    fn renders_at_a_normal_size() {
        let b = drawn(80, 24, &status(Stage::Listening));
        let l = lines(&b);
        assert_eq!(l[0], format!(" Listening  1:02:05{}LectureLive", " ".repeat(80 - 19 - 12)));
        assert_eq!(l[1], " Recording the lecture.");
        assert_eq!(l[2], " 124 events received");
        assert_eq!(l[23], " ^C stop   ^L redraw");
        assert!(l[3..23].iter().all(String::is_empty), "{l:#?}");
    }

    #[test]
    fn tiny_terminals_draw_what_fits_without_panicking() {
        for (w, h) in [(0, 0), (1, 1), (0, 5), (5, 0), (2, 1), (9, 2), (12, 3), (40, 8), (1, 24), (200, 1), (60, 4)] {
            for stage in [Stage::Listening, Stage::Stopping, Stage::StopWaiting] {
                let mut s = status(stage);
                s.notice = Some(SUSPEND);
                let b = drawn(w, h, &s);
                if h >= 1 {
                    let shown = format!(" {}", phase(stage).content);
                    assert!(lines(&b)[0].starts_with(shown.trim_end().get(..shown.len().min(w as usize)).unwrap().trim_end()), "{w}×{h}: {:?}", lines(&b));
                }
            }
        }
        let l = lines(&drawn(40, 2, &status(Stage::Stopping)));
        assert_eq!(l, vec![" Stopping  1:02:05          LectureLive".to_string(), " ^C stop waiting   ^L redraw".to_string()], "two rows: the phase and the keys");
        let l = lines(&drawn(30, 2, &status(Stage::Stopping)));
        assert_eq!(l[0], " Stopping  1:02:05", "the name goes when it would crowd the phase");
    }

    #[test]
    fn the_phase_is_shown_and_stop_waiting_stands_out() {
        for (stage, word) in [(Stage::Listening, "Listening"), (Stage::Stopping, "Stopping"), (Stage::StopWaiting, "Stop waiting")] {
            let b = drawn(80, 24, &status(stage));
            assert!(lines(&b)[0].starts_with(&format!(" {word}  1:02:05")), "{:?}", lines(&b)[0]);
            let reversed = b[(1, 0)].modifier.contains(Modifier::REVERSED);
            assert_eq!(reversed, stage == Stage::StopWaiting, "{word}");
            assert!(b[(1, 0)].modifier.contains(Modifier::BOLD));
            assert!(!b[(1, 0)].modifier.contains(Modifier::DIM), "the dim name leaves the phase alone");
            assert_eq!(b[(word.len() as u16 + 3, 0)].modifier, Modifier::empty(), "the elapsed time is plain");
            assert!(b[(70, 0)].modifier.contains(Modifier::DIM), "the name is dim");
        }
    }

    #[test]
    fn the_key_hint_follows_the_stop_stage() {
        for (stage, hint) in [(Stage::Listening, " ^C stop   ^L redraw"), (Stage::Stopping, " ^C stop waiting   ^L redraw"), (Stage::StopWaiting, " ^C quit at once   ^L redraw")] {
            assert_eq!(lines(&drawn(80, 24, &status(stage)))[23], hint);
        }
    }

    #[test]
    fn a_notice_sits_above_the_keys() {
        let mut s = status(Stage::Listening);
        s.notice = Some(SUSPEND);
        let l = lines(&drawn(80, 24, &s));
        assert_eq!(l[22], format!(" {SUSPEND}"));
        let l = lines(&drawn(80, 3, &s));
        assert!(!l.iter().any(|r| r.contains("Suspending")), "below 4 rows the phase and keys win: {l:?}");
    }
}
