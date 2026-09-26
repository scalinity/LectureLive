//! The Task-6 view: Task 5's minimal screen grown a live header row, enough to prove the session
//! view is real — the phase, the clock, the course, the input and its level, the connection's words,
//! the gaps waiting, the spend sample, the current notice. Task 7's responsive frame replaces it
//! (plan §K): no layout ladder, panes, theme or goldens live here yet. Modifiers only (no colour
//! yet), no borders; the phase row and the keys row win when the terminal is tiny.

use std::time::Duration;

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::Frame;

use lecturelive_core::audio::level::dbfs;
use lecturelive_core::session::coordinator::SttStatus;
use lecturelive_core::session::spend;

use crate::stop::Stage;

use super::state::View;

const BOLD: Style = Style::new().add_modifier(Modifier::BOLD);
const DIM: Style = Style::new().add_modifier(Modifier::DIM);

pub(crate) const SUSPEND: &str = "Suspending would stop the recording. Stop the lecture first (Ctrl-C).";

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

/// The connection's words, as the desktop says them (plan §F): the typed state in a few words,
/// never parsed back.
fn stt_words(s: &SttStatus) -> String {
    match s {
        SttStatus::Connected => "transcribing".into(),
        SttStatus::Retrying { after, reason } => format!("reconnecting in {} s ({reason})", after.as_secs()),
        SttStatus::Refused(m) => format!("refused: {m}"),
        SttStatus::ServerError(m) => format!("server: {m}"),
        SttStatus::Stopped(m) => format!("stopped: {m}"),
    }
}

/// One notice as the notice line shows it: its mark, what it is, what happened — the plain CLI's own
/// characters, already cleaned at the projection's boundary.
fn notice_line(kind: &str, label: &str, detail: &str) -> String {
    let mark = match kind {
        "notes" => "◆",
        "slide" => "▣",
        "page" => "✦",
        "done" => "✓",
        "dim" => "…",
        _ => "▲",
    };
    format!("{mark} {label}  {detail}")
}

fn dim(text: String) -> Line<'static> {
    Line::from(vec![Span::raw(" "), Span::styled(text, DIM)])
}

/// The header's live row (plan §H's second row, restrained): the input, its level or the silence
/// warning, the connection, the gaps waiting, the spend so far.
fn live_row(v: &View) -> Line<'static> {
    let mut spans = vec![Span::raw(" "), Span::raw(v.identity.input.clone())];
    match (v.level, v.silence) {
        (_, true) => spans.extend([Span::raw("  "), Span::styled("no signal", BOLD)]),
        (Some(l), false) => spans.extend([Span::raw("  "), Span::styled(format!("{} {:.0} dB", spend::bar((l.clamp(0.0, 1.0)) as f64, 4), dbfs(l)), DIM)]),
        (None, false) => {}
    }
    if let Some(s) = &v.stt {
        spans.extend([Span::raw("  "), Span::styled(stt_words(s), DIM)]);
    }
    spans.extend([Span::raw("  "), Span::styled(format!("{} gap{}", v.gaps, if v.gaps == 1 { "" } else { "s" }), DIM)]);
    if let Some(usd) = v.spend {
        spans.extend([Span::raw("  "), Span::styled(format!("{} today", spend::money(usd)), DIM)]);
    }
    Line::from(spans)
}

pub(crate) fn render(frame: &mut Frame, v: &View, elapsed: Duration, refused: Option<&str>) {
    let rows: Vec<Rect> = frame.area().rows().collect();
    let h = rows.len();
    // Keys on the last row from 2 rows up, the notice above them from 4; the top rows take what is left.
    let bottom = match h {
        0..=1 => 0,
        2..=3 => 1,
        _ => 2,
    };
    let header = Line::from(vec![
        Span::raw(" "),
        phase(v.phase),
        Span::raw("  "),
        Span::raw(clock(elapsed)),
        Span::raw("  "),
        Span::styled(format!("{} › {}", v.identity.course, v.identity.lecture), DIM),
    ]);
    for (line, row) in [header, live_row(v), dim(meaning(v.phase).into())].into_iter().zip(&rows[..h - bottom]) {
        frame.render_widget(line, *row);
    }
    if bottom >= 1 {
        frame.render_widget(dim(keys(v.phase).into()), rows[h - 1]);
    }
    if bottom == 2 {
        // A refused stop first — it is about the keys just below it — else the view's current notice.
        let text = match refused {
            Some(n) => format!(" {n}"),
            None => v.notice.as_ref().map(|n| format!(" {}", notice_line(n.kind, &n.label, &n.detail))).unwrap_or_default(),
        };
        if !text.is_empty() {
            frame.render_widget(Line::from(Span::raw(text)), rows[h - 2]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plain;
    use crate::tui::hydrate::Hydration;
    use crate::tui::state::{Identity, SourceKind};
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::Terminal;

    fn identity() -> Identity {
        Identity { course: "Machine Learning".into(), lecture: "Week 03 — Optimisation".into(), input: "BlackHole 2ch".into(), kind: SourceKind::Loopback, notes_file: "lecture_notes_20260926.md".into(), transcript_file: "lecture_transcript_20260926.txt".into() }
    }

    fn view(stage: Stage) -> View {
        let mut v = View::new(identity(), Hydration::empty(), Vec::new());
        v.phase = stage;
        v.level = Some(0.05);
        v.stt = Some(SttStatus::Connected);
        v.spend = Some(0.0);
        v
    }

    fn drawn(width: u16, height: u16, v: &View, refused: Option<&str>) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| render(f, v, Duration::from_secs(3725), refused)).unwrap();
        terminal.backend().buffer().clone()
    }

    fn lines(b: &Buffer) -> Vec<String> {
        (0..b.area.height).map(|y| (0..b.area.width).map(|x| b[(x, y)].symbol()).collect::<String>().trim_end().to_string()).collect()
    }

    /// The header proves the projection live: the phase and the clock, then the input, its level,
    /// the connection's words, the gaps waiting and the spend sample.
    #[test]
    fn renders_the_projection_at_a_normal_size() {
        let v = view(Stage::Listening);
        let l = lines(&drawn(110, 24, &v, None));
        assert_eq!(l[0], " Listening  1:02:05  Machine Learning › Week 03 — Optimisation");
        assert_eq!(l[1], format!(" BlackHole 2ch  {} -26 dB  transcribing  0 gaps  $0.00 today", spend::bar(0.05, 4)));
        assert_eq!(l[2], " Recording the lecture.");
        assert_eq!(l[23], " ^C stop   ^L redraw");
        assert!(l[3..22].iter().all(String::is_empty), "{l:#?}");
    }

    /// The live row follows the latest values: silence outranks the meter, a reconnect replaces the
    /// words, a gap count over one is marked red later (Task 7); here, only the truth of it.
    #[test]
    fn the_live_row_shows_the_latest_telemetry() {
        let mut v = view(Stage::Listening);
        v.silence = true;
        assert!(lines(&drawn(110, 24, &v, None))[1].contains("no signal"), "silence outranks the meter");
        v.silence = false;
        v.stt = Some(SttStatus::Retrying { after: Duration::from_secs(1), reason: "socket closed".into() });
        let row = lines(&drawn(110, 24, &v, None))[1].clone();
        assert!(row.contains("reconnecting in 1 s (socket closed)"), "{row}");
        v.gaps = 1;
        assert!(lines(&drawn(110, 24, &v, None))[1].contains("1 gap"), "the singular count");
        v.level = None;
        v.stt = None;
        v.spend = None;
        assert_eq!(lines(&drawn(110, 24, &v, None))[1], " BlackHole 2ch  1 gap", "absent values are not faked");
    }

    /// The current notice sits above the keys, in the plain CLI's characters; a refused stop outranks
    /// it, being about the keys themselves.
    #[test]
    fn the_notice_line_sits_above_the_keys() {
        let mut v = view(Stage::Listening);
        v.notice = Some(plain::input_gone("Receiver_UID"));
        let l = lines(&drawn(110, 24, &v, None));
        assert_eq!(l[22], " ▲ input gone  Receiver_UID; waiting for it to return, and nothing switches by itself");
        let l = lines(&drawn(110, 24, &v, Some(SUSPEND)));
        assert_eq!(l[22], format!(" {SUSPEND}"), "a refused stop outranks the notice");
        let l = lines(&drawn(110, 3, &v, Some(SUSPEND)));
        assert!(!l.iter().any(|r| r.contains("Suspending")), "below 4 rows the header and keys win: {l:?}");
    }

    #[test]
    fn tiny_terminals_draw_what_fits_without_panicking() {
        for (w, h) in [(0, 0), (1, 1), (0, 5), (5, 0), (2, 1), (9, 2), (12, 3), (40, 8), (1, 24), (200, 1), (60, 4), (60, 2)] {
            for stage in [Stage::Listening, Stage::Stopping, Stage::StopWaiting] {
                let mut v = view(stage);
                v.notice = Some(plain::input_gone("UID"));
                let b = drawn(w, h, &v, Some(SUSPEND));
                if h >= 1 {
                    let shown = format!(" {}", phase(stage).content);
                    assert!(lines(&b)[0].starts_with(shown.trim_end().get(..shown.len().min(w as usize)).unwrap().trim_end()), "{w}×{h}: {:?}", lines(&b));
                }
            }
        }
        let l = lines(&drawn(40, 2, &view(Stage::Stopping), None));
        assert_eq!(l[0], " Stopping  1:02:05  Machine Learning › W", "two rows: the header (clipped) and the keys");
        assert_eq!(l[1], " ^C stop waiting   ^L redraw");
    }

    #[test]
    fn the_phase_is_shown_and_stop_waiting_stands_out() {
        for (stage, word) in [(Stage::Listening, "Listening"), (Stage::Stopping, "Stopping"), (Stage::StopWaiting, "Stop waiting")] {
            let b = drawn(110, 24, &view(stage), None);
            assert!(lines(&b)[0].starts_with(&format!(" {word}  1:02:05")), "{:?}", lines(&b)[0]);
            let reversed = b[(1, 0)].modifier.contains(Modifier::REVERSED);
            assert_eq!(reversed, stage == Stage::StopWaiting, "{word}");
            assert!(b[(1, 0)].modifier.contains(Modifier::BOLD));
            assert!(!b[(1, 0)].modifier.contains(Modifier::DIM), "the dim course leaves the phase alone");
            assert_eq!(b[(word.len() as u16 + 3, 0)].modifier, Modifier::empty(), "the clock is plain");
        }
    }

    #[test]
    fn the_key_hint_follows_the_stop_stage() {
        for (stage, hint) in [(Stage::Listening, " ^C stop   ^L redraw"), (Stage::Stopping, " ^C stop waiting   ^L redraw"), (Stage::StopWaiting, " ^C quit at once   ^L redraw")] {
            assert_eq!(lines(&drawn(110, 24, &view(stage), None))[23], hint);
        }
    }
}
