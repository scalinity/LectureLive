//! The plain frontend's presentation (M7 plan §E): the `say`/`show` event lines, the start-up and
//! end reports, the stdin grammar and the words for each stop stage, exactly as the CLI prints them today.

use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lecturelive_core::audio::level::SilenceWatch;
use lecturelive_core::audio::{input, loopback};
use lecturelive_core::notes::page::PageOutcome;
use lecturelive_core::session::coordinator::{Notification, SttStatus, StopReport};
use lecturelive_core::session::files::LectureFiles;
use lecturelive_core::session::folder::How;
use lecturelive_core::session::lecture::{Command as LectureCommand, Event, Op};
use lecturelive_core::session::notesfile::Recovered;
use lecturelive_core::session::segments::SegmentSource;
use lecturelive_core::session::spend::{self, Spend};
use lecturelive_core::session::start;

use crate::capture;
use crate::stop::{Origin, Stage, Step, StopController};

pub(crate) fn secs(samples: u64) -> f64 {
    samples as f64 / 16_000.0
}

/// A message's own final period dropped before the sentence that follows it.
pub(crate) fn sentence(m: &str) -> &str {
    m.trim_end().trim_end_matches('.')
}

pub(crate) fn paint() -> spend::Paint {
    use std::io::IsTerminal;
    spend::Paint { color: std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none(), truecolor: matches!(std::env::var("COLORTERM").as_deref(), Ok("truecolor" | "24bit")) }
}

/// The CLI's `say`: one event line, its mark, what it is, what happened.
pub(crate) fn say(out: &mut impl Write, p: spend::Paint, kind: &str, label: &str, detail: &str) {
    let (mark, colour) = match kind {
        "slide" => ("▣", "teal"),
        "notes" => ("◆", "teal"),
        "page" => ("✦", "teal"),
        "done" => ("✓", "teal"),
        _ => ("▲", "red"),
    };
    writeln!(out, "{}", format!("  {} {}  {detail}", p.paint(mark, &[colour]), p.paint(label, &["bold"])).trim_end()).unwrap_or_else(|e| panic!("failed printing to stdout: {e}"));
}

/// An event's own wording, without any terminal styling of its own: which mark, what it is, what
/// happened. The plain CLI prints it through `say`; the TUI shows the same words in its activity and
/// notice line (passing a colour-off `Paint`, so nothing styled ever enters its view). Events the
/// CLI prints as transcript or dim lines — segments, `Recording`, a first `transcribing`, `Busy`,
/// `Preview` — carry no notice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Notice {
    /// The mark's kind, as `say` names it: "slide", "notes", "page", "done", or anything else for a
    /// warning.
    pub(crate) kind: &'static str,
    pub(crate) label: String,
    pub(crate) detail: String,
}

fn plural(n: usize, word: &str) -> String {
    format!("{n} {word}{}", if n == 1 { "" } else { "s" })
}

/// Strips what could act on a terminal from text about to be shown (M7 plan §C 12), whole sequences
/// at a time, so no escape's payload is left behind as fake lecture text:
/// - CSI (`ESC [` or C1 `U+009B`): its parameter and intermediate characters and its final one;
/// - OSC, DCS, SOS, PM and APC (`ESC ] P X ^ _` or their C1 forms): everything up to BEL or ST, so a
///   title, a clipboard write or a hyperlink's target goes, and a hyperlink's visible text stays;
/// - any other escape: `ESC`, its intermediates and its final character;
/// - every remaining C0 and C1 control and DEL.
///
/// A carriage return would rewrite the line it ends, so CR LF and a lone CR each become one `\n`.
/// `\n` and tab stay, as do Unicode, punctuation, spaces and Markdown. A sequence broken by a
/// character that cannot belong to it ends there, and that character is kept as text. Display
/// cleaning only: canonical files are never altered.
pub(crate) fn clean(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\n' | '\t' => out.push(c),
            '\r' => {
                chars.next_if_eq(&'\n');
                out.push('\n');
            }
            '\x1b' => match chars.peek() {
                Some('[') => {
                    chars.next();
                    skip_csi(&mut chars);
                }
                Some(']' | 'P' | 'X' | '^' | '_') => {
                    chars.next();
                    skip_string(&mut chars);
                }
                Some(' '..='/') => {
                    while chars.next_if(|c| matches!(c, ' '..='/')).is_some() {}
                    chars.next_if(|c| matches!(c, '0'..='~'));
                }
                Some('0'..='~') => {
                    chars.next();
                }
                _ => {}
            },
            '\u{9b}' => skip_csi(&mut chars),
            '\u{90}' | '\u{98}' | '\u{9d}' | '\u{9e}' | '\u{9f}' => skip_string(&mut chars),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// A control sequence's body after its introducer: parameter and intermediate characters
/// (`0x20..=0x3F`), then one final character (`0x40..=0x7E`).
fn skip_csi(chars: &mut std::iter::Peekable<std::str::Chars>) {
    while chars.next_if(|c| matches!(c, ' '..='?')).is_some() {}
    chars.next_if(|c| matches!(c, '@'..='~'));
}

/// A control string's body after its introducer, up to its terminator: BEL or C1 ST are consumed; an
/// ESC is left for [`clean`], which takes `ESC \` as the ST it is (or starts the next sequence).
fn skip_string(chars: &mut std::iter::Peekable<std::str::Chars>) {
    while let Some(&c) = chars.peek() {
        match c {
            '\x07' | '\u{9c}' => {
                chars.next();
                return;
            }
            '\x1b' => return,
            _ => {
                chars.next();
            }
        }
    }
}

/// The input-gone notice (plan §H's notice line, top priority): the fixed sentence. The plain CLI
/// appends its offer of the other inputs after it; the TUI shows the sentence as it is.
pub(crate) fn input_gone(uid: &str) -> Notice {
    Notice { kind: "warn", label: "input gone".into(), detail: format!("{uid}; waiting for it to return, and nothing switches by itself") }
}

/// The loopback silence warning (spec §4.3), as both frontends' watches say it.
pub(crate) fn no_signal() -> Notice {
    Notice { kind: "warn", label: "no signal".into(), detail: format!("10 s of silence on BlackHole: is Zoom's Speaker \"{}\"?", loopback::LOOPBACK_NAME) }
}

/// The committed line's detail: what was folded in and what it cost. The amount is painted dim by
/// the plain CLI and plain for the TUI — the same words either way.
fn committed_detail(words: usize, slides: usize, usd: f64, confirmed: bool, missing: usize, p: spend::Paint) -> String {
    let mut detail = format!("{} and {} folded in  {}", plural(words, "word"), plural(slides, "slide"), p.paint(&spend::money(usd), &["dim"]));
    if missing > 0 {
        detail += &format!("  ({} not placed by the model, listed at the end)", plural(missing, "slide"));
    }
    if !confirmed {
        detail += "  (transcription still catching up; the rest goes into the next snapshot)";
    }
    detail
}

/// The polished line's detail: where the previous version went, and what the polish cost.
fn polished_detail(backup: &Path, usd: f64, p: spend::Paint) -> String {
    let name = backup.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    format!("previous version in .live_notes/{name}  {}", p.paint(&spend::money(usd), &["dim"]))
}

/// The page line's detail: what was typeset, how long it came out, what it cost.
fn page_detail(outcome: &PageOutcome, usd: f64, p: spend::Paint) -> String {
    let name = outcome.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let length = p.paint(&format!("{} words of {} allowed", outcome.words, outcome.budget), &[if outcome.words > outcome.budget as usize { "red" } else { "dim" }]);
    let mut detail = format!("{name}  {length}  {}", p.paint(&spend::money(usd), &["dim"]));
    if outcome.cached {
        detail += "  (notes unchanged since they were typeset: only the design reapplied, free)";
    }
    if !outcome.missing.is_empty() {
        detail += &format!("  (missing: {})", outcome.missing.join(", "));
    }
    detail
}

/// One notice in the CLI's line: `say`, from the shared wording.
fn say_notice(out: &mut impl Write, p: spend::Paint, n: &Notice) {
    say(out, p, n.kind, &n.label, &n.detail);
}

/// An event as a notice, in the plain CLI's own words (the TUI's activity and notice line show the
/// same wording, with `p` colour-off so no styling enters the view). Capture states and
/// relocations use the terminal's own words ([`capture::state_words`]), not the desktop's.
pub(crate) fn notice(e: &Event, p: spend::Paint, words: &capture::Words) -> Option<Notice> {
    let n = |kind: &'static str, label: &str, detail: String| Some(Notice { kind, label: label.into(), detail });
    match e {
        Event::Session(m) => match m {
            Notification::Gap(g) => n("warn", "gap", format!("{:?} from sample {} to {:?} of {}", g.kind, g.start_sample, g.end_sample, g.recording_id)),
            Notification::DeviceGone { uid } => Some(input_gone(uid)),
            Notification::DeviceBack { uid } => n("done", "input back", format!("{uid}; recording continues in a new file")),
            Notification::Failed(m) => n("warn", "session failed", m.clone()),
            Notification::Stt(s) => match s {
                SttStatus::Retrying { after, reason } => n("warn", "transcription interrupted", format!("{reason}; reconnecting in {} s", after.as_secs())),
                SttStatus::Refused(m) => n("warn", "transcription refused", format!("{}. Recording continues without it.", sentence(m))),
                SttStatus::ServerError(m) => n("warn", "transcription server", m.clone()),
                SttStatus::Stopped(m) => n("warn", "transcription stopped", m.clone()),
                SttStatus::Connected => None,
            },
            Notification::Recovered(g) => n("done", "recovered", format!("the transcript of {:.1}–{:.1} s of recording {}", secs(g.start_sample), g.end_sample.map_or(0.0, secs), g.recording_id)),
            Notification::RecoveryFailed(m) => n("warn", "recovery", m.clone()),
            Notification::SpendFailed(m) => n("warn", "spend", m.clone()),
            _ => None,
        },
        Event::NothingNew => n("notes", "snapshot", "nothing new since the last one".into()),
        Event::Committed { words, slides, usd, confirmed, missing, .. } => n("notes", "notes", committed_detail(*words, *slides, *usd, *confirmed, *missing, p)),
        Event::SnapshotFailed(m) => n("warn", "snapshot failed", m.clone()),
        Event::Polished { backup, usd, .. } => n("done", "polished", polished_detail(backup, *usd, p)),
        Event::PolishStopped(m) => n("warn", "polish stopped", m.clone()),
        Event::PolishFailed(m) => n("warn", "polish failed", m.clone()),
        Event::Cancelled(what) => n("warn", "cancelled", format!("{what}; nothing was written, everything is kept for the next snapshot")),
        Event::Page { outcome, usd } => n("done", "page", page_detail(outcome, *usd, p)),
        Event::PageFailed(m) => n("warn", "page failed", m.clone()),
        Event::Slide { index, file, auto, uncertain, .. } => {
            let how = match (auto, uncertain) {
                (true, true) => " (auto, still changing)",
                (true, false) => " (auto)",
                _ => "",
            };
            let name = Path::new(file).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            n("slide", &format!("slide {index}{how}"), format!("{name}, into the next snapshot"))
        }
        Event::Capture(s) => {
            let (kind, label, detail) = capture::state_words(s, words);
            n(kind, label, detail)
        }
        Event::CaptureMoved { note, .. } => {
            let (kind, label, detail) = capture::moved_words(note);
            n(kind, label, detail)
        }
        Event::Warning(m) => n("warn", "warning", m.clone()),
        Event::Busy(_) | Event::Preview(_) => None,
    }
}

/// The fallback the CLI offers while an input is gone (spec §4.1): the other inputs, to start again with.
pub(crate) fn other_inputs(gone: &str) -> String {
    let others: Vec<String> = input::list_inputs().unwrap_or_default().into_iter().filter(|i| i.uid != gone).map(|i| format!("{} ({})", i.name, i.uid)).collect();
    if others.is_empty() {
        return "No other input is connected; Ctrl-C stops.".into();
    }
    format!("To record from another input, stop (Ctrl-C) and start again with --device <UID>: {}.", others.join(", "))
}

/// Prints the lecture's events in the CLI's lines; the loopback silence warning as `record` gives it.
/// The say-lines come from [`notice`], so the TUI's activity says the same things (M7 plan §F);
/// capture states arrive in the terminal's own `words`.
pub(crate) fn show(out: &mut impl Write, p: spend::Paint, e: &Event, watch: &mut Option<SilenceWatch>, words: &capture::Words) {
    // The plain CLI's own lines first — the transcript, the recording path, a first `transcribing`,
    // the busy text — and the events whose wording needs the machine (the gone input's offer).
    match e {
        Event::Session(Notification::Segment(s)) => {
            let tag = if s.source == SegmentSource::Recovered { p.paint("  (recovered)", &["dim"]) } else { String::new() };
            writeln!(out, "  {}  {}{tag}", p.paint(&s.said_at.format("%H:%M:%S").to_string(), &["dim"]), s.text).unwrap_or_else(|e| panic!("failed printing to stdout: {e}"));
        }
        Event::Session(Notification::Level(l)) => {
            if watch.as_mut().is_some_and(|w| w.observe(*l)) {
                say_notice(out, p, &no_signal());
            }
        }
        Event::Session(Notification::Recording { path }) => { writeln!(out, "{}", p.paint(&format!("  recording to {}", path.display()), &["dim"])).unwrap_or_else(|e| panic!("failed printing to stdout: {e}")); }
        // The notice is the fixed sentence; the offer of the other inputs is the plain CLI's own.
        Event::Session(Notification::DeviceGone { uid }) => {
            let n = input_gone(uid);
            say(out, p, n.kind, &n.label, &format!("{}. {}", n.detail, other_inputs(uid)));
        }
        Event::Session(Notification::Stt(SttStatus::Connected)) => { writeln!(out, "{}", p.paint("  transcribing", &["dim"])).unwrap_or_else(|e| panic!("failed printing to stdout: {e}")); }
        Event::Busy(m) => { writeln!(out, "{}", p.paint(&format!("  … {m}"), &["dim"])).unwrap_or_else(|e| panic!("failed printing to stdout: {e}")); }
        Event::Preview(_) => {} // the committed block is printed instead, as the CLI does
        Event::Committed { block, .. } => {
            for line in block.trim().lines().filter(|l| !l.starts_with("<!-- ")) {
                writeln!(out, "  {} {}", p.paint("│", &["teal"]), p.paint(line, &["dim"])).unwrap_or_else(|e| panic!("failed printing to stdout: {e}"));
            }
            if let Some(n) = notice(e, p, words) {
                say_notice(out, p, &n);
            }
        }
        _ => {
            if let Some(n) = notice(e, p, words) {
                say_notice(out, p, &n);
            }
        }
    }
}

/// The stdin grammar: an empty line takes a snapshot, `polish` polishes, and any other line is the snapshot's hint.
pub(crate) fn parse_line(line: &str) -> Op {
    let text = line.trim().to_string();
    if text.eq_ignore_ascii_case("polish") { Op::Polish } else { Op::Snapshot(text) }
}

/// The start-up report's records (M7 plan Task 6): what launch did to the recordings and what the
/// folder's initialisation found, in the order [`print_prepared`] prints them. The TUI seeds its
/// activity with the same records, so both frontends open with the same words.
pub(crate) fn startup_records(ready: &start::Prepared, files: &LectureFiles) -> Vec<Notice> {
    let mut out = Vec::new();
    let mut n = |kind: &'static str, label: &str, detail: String| out.push(Notice { kind, label: label.into(), detail });
    for (path, lost) in &ready.launch.repaired {
        n("done", "repaired", format!("{} ({:.1} s)", path.display(), secs(*lost)));
    }
    for path in &ready.launch.missing {
        n("warn", "missing", format!("{} (marked as a gap)", path.display()));
    }
    for path in &ready.launch.pruned {
        n("done", "deleted", format!("{} (retention)", path.display()));
    }
    let init = &ready.init;
    match init.how {
        How::Created => n("notes", "notes", format!("{} created", files.notes.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default())),
        How::Migrated => n("notes", "migrated", "this folder's Python CLI state is now the app's (the Python CLI no longer writes here)".into()),
        How::Rebuilt => n("warn", "rebuilt", "the state was rebuilt from the notes: everything after the last <!-- --> marker is pending".into()),
        How::Resumed => {}
    }
    if let Some(m) = init.legacy_commit {
        n("warn", "recovered", m.to_string());
    }
    if let Some(kept) = &init.corrupt_kept {
        n("warn", "rebuilt", format!("the corrupt state is kept as {}", kept.display()));
    }
    match init.journal {
        Recovered::Completed => n("done", "recovered", "the last snapshot was fully written".into()),
        Recovered::Truncated => n("warn", "recovered", "removed a half-written snapshot; its material is queued again".into()),
        Recovered::NotAppended => n("warn", "recovered", "an interrupted snapshot never reached the notes; its material is queued again".into()),
        Recovered::Nothing => {}
    }
    if init.external_edit {
        n("notes", "notes", "edited outside the app since the last session; kept as they are".into());
    }
    for (stem, r) in &ready.other_days {
        n("done", "recovered", format!("{stem}'s transcript gaps{}", if r.unresolved > 0 { format!(", {} still waiting", r.unresolved) } else { String::new() }));
    }
    out
}

/// The start-up report after `prepare`: what launch did to the recordings, what the folder's
/// initialisation found, then the lecture's header lines.
pub(crate) fn print_prepared(out: &mut impl Write, p: spend::Paint, ready: &start::Prepared, files: &LectureFiles, course: &str, name: &str, input_name: &str) {
    for n in startup_records(ready, files) {
        say_notice(out, p, &n);
    }
    let init = &ready.init;

    writeln!(out).unwrap_or_else(|e| panic!("failed printing to stdout: {e}"));
    writeln!(out, "  {}  {}  {name}", p.paint(course, &["bold"]), p.paint("›", &["dim"])).unwrap_or_else(|e| panic!("failed printing to stdout: {e}"));
    let waiting = if init.pending_segments > 0 || init.pending_slides > 0 {
        format!(", resumed with {} and {} for the next snapshot", plural(init.pending_segments as usize, "line"), plural(init.pending_slides, "slide"))
    } else {
        String::new()
    };
    writeln!(out, "{}", p.paint(&format!("  listening on {input_name}{waiting}"), &["dim"])).unwrap_or_else(|e| panic!("failed printing to stdout: {e}"));
    writeln!(out, "{}", p.paint("  ⏎ snapshot   a hint ⏎   polish ⏎   ^C stop", &["dim"])).unwrap_or_else(|e| panic!("failed printing to stdout: {e}"));
    writeln!(out).unwrap_or_else(|e| panic!("failed printing to stdout: {e}"));
}

/// The end summary: what was saved, what still waits for the next session, and what the lecture cost today.
pub(crate) fn print_end(out: &mut impl Write, p: spend::Paint, files: &LectureFiles, report: &StopReport, spend: &Spend) {
    let file_name = |f: &Path| f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    writeln!(out).unwrap_or_else(|e| panic!("failed printing to stdout: {e}"));
    writeln!(out, "  {} {}  {}  {}", p.paint("✓", &["teal"]), p.paint("saved", &["bold"]), file_name(&files.notes), file_name(&files.transcript)).unwrap_or_else(|e| panic!("failed printing to stdout: {e}"));
    if let Some(e) = &report.last_snapshot {
        say(out, p, "warn", "notes", &format!("the last snapshot failed ({e}); the next session in this folder adds what it missed"));
    }
    if report.unresolved > 0 {
        writeln!(out, "{}", p.paint(&format!("    {} still to recover; the next session in this folder does it", plural(report.unresolved, "transcript gap")), &["dim"])).unwrap_or_else(|e| panic!("failed printing to stdout: {e}"));
    }
    writeln!(out, "{}", p.paint(&format!("    {} spent on this lecture today; `lecture spend` has the rest", spend::money(spend.lecture_total())), &["dim"])).unwrap_or_else(|e| panic!("failed printing to stdout: {e}"));
    writeln!(out).unwrap_or_else(|e| panic!("failed printing to stdout: {e}"));
}

/// The stdin worker: every line of stdin becomes the grammar's command, until stdin ends or the channel closes.
pub(crate) fn read_commands(stdin_tx: tokio::sync::mpsc::UnboundedSender<LectureCommand>) {
    std::thread::spawn(move || {
        for line in std::io::stdin().lines().map_while(Result::ok) {
            if stdin_tx.send(LectureCommand::Op(parse_line(&line))).is_err() {
                return;
            }
        }
    });
}

/// One stop request as plain handles it (M7 plan §G): the shared controller's next stage, its words,
/// then the `Stop`s it asks for. Stage 3 sends nothing; the caller quits.
pub(crate) fn request_stop(out: &mut impl Write, p: spend::Paint, controller: &mut StopController, origin: Origin, stop_tx: &tokio::sync::mpsc::UnboundedSender<LectureCommand>) -> Result<Step, tokio::sync::mpsc::error::SendError<LectureCommand>> {
    let step = controller.advance(origin, Instant::now());
    match step {
        Step::Advance { stage: Stage::Stopping, .. } => say(out, p, "notes", "stopping", "finishing the transcript and recovery, then a last snapshot (Ctrl-C again stops waiting for recovery)"),
        Step::Advance { stage: Stage::StopWaiting, .. } => say(out, p, "warn", "stopping", "no longer waiting for recovery or queued requests; its gaps wait for the next session (Ctrl-C again quits at once)"),
        // The recording is durable to its last second and gaps and journals are on disk: the next
        // session in this folder repairs, recovers and notes what is left.
        Step::Quit => say(out, p, "warn", "quit", "stopped at once; the next session in this folder picks up what was left"),
        Step::Advance { stage: Stage::Listening, .. } | Step::Ignored(_) => {}
    }
    if let Step::Advance { stops_to_send, .. } = step {
        for _ in 0..stops_to_send {
            stop_tx.send(LectureCommand::Stop)?;
        }
    }
    Ok(step)
}

/// Ctrl-C as the plain CLI counts it, on the controller the `--secs` timer shares: the first stop,
/// then stopping the wait for recovery, then quitting at once.
pub(crate) fn stop_on_ctrl_c(stop_tx: tokio::sync::mpsc::UnboundedSender<LectureCommand>, p: spend::Paint, controller: Arc<Mutex<StopController>>) {
    tokio::spawn(async move {
        while tokio::signal::ctrl_c().await.is_ok() {
            let step = request_stop(&mut std::io::stdout().lock(), p, &mut controller.lock().expect("the stop controller"), Origin::Signal, &stop_tx);
            match step {
                Ok(Step::Quit) => std::process::exit(130),
                Ok(_) => {}
                Err(_) => return,
            }
        }
    });
}

/// The `--secs` timer: the first stop when the limit passes, silent as it has always been.
pub(crate) fn stop_after_secs(limit: u64, timer_tx: tokio::sync::mpsc::UnboundedSender<LectureCommand>, p: spend::Paint, controller: Arc<Mutex<StopController>>) {
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(limit)).await;
        let _ = request_stop(&mut std::io::sink(), p, &mut controller.lock().expect("the stop controller"), Origin::Timer, &timer_tx);
    });
}

/// Task 1's characterization goldens (M7 plan §J): the plain lines the CLI prints today, byte for
/// byte, so the module split changes none of them.
#[cfg(test)]
mod goldens {
    use super::*;
    use std::path::PathBuf;

    use lecturelive_core::capture::detect::Region;
    use lecturelive_core::capture::select::{Descriptor, Selection};
    use lecturelive_core::capture::worker::CaptureState;
    use lecturelive_core::notes::page::PageOutcome;
    use lecturelive_core::session::folder::InitReport;
    use lecturelive_core::session::launch::LaunchReport;
    use lecturelive_core::session::segments::{self, Segment};
    use lecturelive_core::session::sidecar::{Gap, Sidecar};
    use lecturelive_core::session::spend::SpendKind;

    const OFF: spend::Paint = spend::Paint { color: false, truecolor: false };
    const ANSI: spend::Paint = spend::Paint { color: true, truecolor: false };
    const TRUE: spend::Paint = spend::Paint { color: true, truecolor: true };

    fn shown(p: spend::Paint, e: &Event, watch: &mut Option<SilenceWatch>) -> String {
        let mut out = Vec::new();
        show(&mut out, p, e, watch, &words());
        String::from_utf8(out).unwrap()
    }

    fn plain(e: &Event) -> String {
        shown(OFF, e, &mut None)
    }

    /// The words the tests print capture states in: a fixed course and host, so no test depends on
    /// the machine running it.
    fn words() -> capture::Words {
        capture::Words { course: "Machine Learning".into(), host: "Terminal" }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lecturelive-goldens-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A fixed gap, read the way core reads one, so no test needs the uuid crate.
    fn gap() -> Gap {
        let p = scratch("gap").join("day.v2.json");
        std::fs::write(&p, r#"{"version":2,"gaps":[{"recording_id":"11111111-1111-1111-1111-111111111111","start_sample":3200,"end_sample":16000,"kind":"recorder_overflow","resolved":false}]}"#).unwrap();
        Sidecar::load(&p).unwrap().unwrap().gaps[0].clone()
    }

    /// A fixed segment at a fixed instant, read the way core reads one (`live` or `recovered`).
    fn segment(source: &str) -> Segment {
        let p = scratch("segment").join("day.segments.jsonl");
        let mut line = format!(r#"{{"id":0,"recording_id":"11111111-1111-1111-1111-111111111111","start_sample":1600,"end_sample":8000,"said_at":"2026-09-26T10:42:03+02:00","start":"2026-09-26T10:42:03+02:00","end":"2026-09-26T10:42:08+02:00","text":"gradient descent","words":[],"source":"{source}"}}"#);
        line.push('\n');
        std::fs::write(&p, line).unwrap();
        segments::read(&p).unwrap()[0].clone()
    }

    #[test]
    fn session_notification_lines_colour_off() {
        let seg = segment("live");
        assert_eq!(plain(&Event::Session(Notification::Segment(seg.clone()))), format!("  {}  gradient descent\n", seg.said_at.format("%H:%M:%S")));
        let rec = segment("recovered");
        assert_eq!(plain(&Event::Session(Notification::Segment(rec.clone()))), format!("  {}  gradient descent  (recovered)\n", rec.said_at.format("%H:%M:%S")));
        assert_eq!(plain(&Event::Session(Notification::Recording { path: PathBuf::from("/tmp/lec/recordings/session_1.wav") })), "  recording to /tmp/lec/recordings/session_1.wav\n");
        assert_eq!(plain(&Event::Session(Notification::Gap(gap()))), "  ▲ gap  RecorderOverflow from sample 3200 to Some(16000) of 11111111-1111-1111-1111-111111111111\n");
        assert_eq!(plain(&Event::Session(Notification::DeviceBack { uid: "BlackHole-UID".into() })), "  ✓ input back  BlackHole-UID; recording continues in a new file\n");
        assert_eq!(plain(&Event::Session(Notification::Failed("disk full".into()))), "  ▲ session failed  disk full\n");
        assert_eq!(plain(&Event::Session(Notification::Recovered(gap()))), "  ✓ recovered  the transcript of 0.2–1.0 s of recording 11111111-1111-1111-1111-111111111111\n");
        assert_eq!(plain(&Event::Session(Notification::RecoveryFailed("server busy".into()))), "  ▲ recovery  server busy\n");
        assert_eq!(plain(&Event::Session(Notification::SpendFailed("ledger unwritable".into()))), "  ▲ spend  ledger unwritable\n");
        assert_eq!(plain(&Event::Session(Notification::Open { stable: "gra".into(), tentative: "descent".into() })), "", "the terminal shows closed utterances only");
        assert_eq!(plain(&Event::Session(Notification::SourceEnded)), "");
    }

    #[test]
    fn stt_status_lines_colour_off() {
        assert_eq!(plain(&Event::Session(Notification::Stt(SttStatus::Connected))), "  transcribing\n");
        assert_eq!(plain(&Event::Session(Notification::Stt(SttStatus::Retrying { after: Duration::from_secs(5), reason: "socket closed".into() }))), "  ▲ transcription interrupted  socket closed; reconnecting in 5 s\n");
        assert_eq!(plain(&Event::Session(Notification::Stt(SttStatus::Refused("bad key.".into())))), "  ▲ transcription refused  bad key. Recording continues without it.\n");
        assert_eq!(plain(&Event::Session(Notification::Stt(SttStatus::ServerError("500 again".into())))), "  ▲ transcription server  500 again\n");
        assert_eq!(plain(&Event::Session(Notification::Stt(SttStatus::Stopped("the worker stopped".into())))), "  ▲ transcription stopped  the worker stopped\n");
    }

    #[test]
    fn level_lines_carry_only_the_loopback_silence_warning() {
        let quiet = 10f32.powf(-80.0 / 20.0); // −80 dBFS, below the −60 dBFS threshold
        assert_eq!(plain(&Event::Session(Notification::Level(0.5))), "", "without a watch nothing is printed");
        let mut watch = Some(SilenceWatch::new(-60.0, 10));
        let mut seen = String::new();
        for _ in 0..9 {
            seen += &shown(OFF, &Event::Session(Notification::Level(quiet)), &mut watch);
        }
        assert_eq!(seen, "");
        seen += &shown(OFF, &Event::Session(Notification::Level(quiet)), &mut watch);
        assert_eq!(seen, "  ▲ no signal  10 s of silence on BlackHole: is Zoom's Speaker \"LectureLive Loopback\"?\n");
        seen += &shown(OFF, &Event::Session(Notification::Level(quiet)), &mut watch);
        assert_eq!(seen, "  ▲ no signal  10 s of silence on BlackHole: is Zoom's Speaker \"LectureLive Loopback\"?\n", "one warning per silent stretch");
    }

    /// `DeviceGone`'s offer lists this machine's inputs, so only the prefix and the offer's shape are pinned.
    #[test]
    fn device_gone_keeps_a_stable_prefix_before_the_machine_input_list() {
        let line = plain(&Event::Session(Notification::DeviceGone { uid: "Gone-UID".into() }));
        let prefix = "  ▲ input gone  Gone-UID; waiting for it to return, and nothing switches by itself. ";
        let rest = line.strip_prefix(prefix).unwrap_or_else(|| panic!("the stable prefix was lost: {line:?}"));
        assert!(
            rest == "No other input is connected; Ctrl-C stops."
                || rest.starts_with("To record from another input, stop (Ctrl-C) and start again with --device <UID>: "),
            "the machine's inputs follow the fixed offer: {rest:?}"
        );
    }

    #[test]
    fn busy_preview_and_snapshot_outcomes_colour_off() {
        assert_eq!(plain(&Event::Busy("snapshot, 12 words to grok-4".into())), "  … snapshot, 12 words to grok-4\n");
        assert_eq!(plain(&Event::Preview("partial answer".into())), "", "the committed block is printed instead");
        assert_eq!(plain(&Event::NothingNew), "  ◆ snapshot  nothing new since the last one\n");
        assert_eq!(plain(&Event::SnapshotFailed("the model refused; everything is kept for the next one".into())), "  ▲ snapshot failed  the model refused; everything is kept for the next one\n");
        assert_eq!(plain(&Event::Cancelled("the snapshot".into())), "  ▲ cancelled  the snapshot; nothing was written, everything is kept for the next snapshot\n");
    }

    #[test]
    fn committed_lines_colour_off() {
        let block = "\n<!-- 10:42:03 -->\n## Sampling distributions\n\n- Larger samples reduce standard error.\n";
        let e = Event::Committed { words: 486, slides: 2, block: block.into(), usd: 0.02, confirmed: true, removed: 0, missing: 0, revision: 3 };
        assert_eq!(plain(&e), "  │ ## Sampling distributions\n  │ \n  │ - Larger samples reduce standard error.\n  ◆ notes  486 words and 2 slides folded in  $0.02\n");
        let e = Event::Committed { words: 1, slides: 0, block: "<!-- 10:43:00 -->\n- one line".into(), usd: 0.001, confirmed: false, removed: 0, missing: 2, revision: 4 };
        assert_eq!(plain(&e), "  │ - one line\n  ◆ notes  1 word and 0 slides folded in  <$0.01  (2 slides not placed by the model, listed at the end)  (transcription still catching up; the rest goes into the next snapshot)\n");
    }

    #[test]
    fn polish_outcomes_colour_off() {
        let e = Event::Polished { backup: PathBuf::from("/tmp/lec/.live_notes/lecture_notes_20260926_104152.md"), usd: 0.04, revision: 5 };
        assert_eq!(plain(&e), "  ✓ polished  previous version in .live_notes/lecture_notes_20260926_104152.md  $0.04\n");
        assert_eq!(plain(&Event::PolishStopped("the snapshot before it failed (read notes); the notes are unchanged".into())), "  ▲ polish stopped  the snapshot before it failed (read notes); the notes are unchanged\n");
        assert_eq!(plain(&Event::PolishFailed("read notes: denied; the notes are unchanged".into())), "  ▲ polish failed  read notes: denied; the notes are unchanged\n");
    }

    #[test]
    fn page_outcomes_colour_off() {
        let outcome = PageOutcome { path: PathBuf::from("/tmp/lec/study_page.html"), words: 1200, budget: 1500, cached: false, missing: vec![] };
        assert_eq!(plain(&Event::Page { outcome, usd: 0.03 }), "  ✓ page  study_page.html  1200 words of 1500 allowed  $0.03\n");
        let outcome = PageOutcome { path: PathBuf::from("/tmp/lec/study_page.html"), words: 1600, budget: 1500, cached: true, missing: vec!["slide 3"] };
        assert_eq!(plain(&Event::Page { outcome, usd: 0.0 }), "  ✓ page  study_page.html  1600 words of 1500 allowed  $0.00  (notes unchanged since they were typeset: only the design reapplied, free)  (missing: slide 3)\n");
        assert_eq!(plain(&Event::PageFailed("the model refused. The notes are unchanged; `lecture page` tries again.".into())), "  ▲ page failed  the model refused. The notes are unchanged; `lecture page` tries again.\n");
    }

    #[test]
    fn slide_and_capture_lines_colour_off() {
        let now = chrono::Local::now();
        assert_eq!(plain(&Event::Slide { index: 17, file: "slides/slide_17_103941.png".into(), auto: true, uncertain: false, shown_at: now }), "  ▣ slide 17 (auto)  slide_17_103941.png, into the next snapshot\n");
        assert_eq!(plain(&Event::Slide { index: 18, file: "slides/slide_18_104152.png".into(), auto: true, uncertain: true, shown_at: now }), "  ▣ slide 18 (auto, still changing)  slide_18_104152.png, into the next snapshot\n");
        assert_eq!(plain(&Event::Slide { index: 19, file: "slides/slide_19_104201.png".into(), auto: false, uncertain: false, shown_at: now }), "  ▣ slide 19  slide_19_104201.png, into the next snapshot\n");
        // Task 11: the terminal's own capture words, never the desktop's "slides strip" (plan §H).
        assert_eq!(plain(&Event::Capture(CaptureState::Unbound)), "  ▣ no window  No Zoom window chosen for Machine Learning yet: choose it once in the LectureLive app. Screenshots (⌘⇧4) still become slides.\n");
        assert_eq!(plain(&Event::Capture(CaptureState::Watching { window: "Zoom Meeting".into() })), "  ▣ watching  Zoom Meeting\n");
        assert_eq!(plain(&Event::Capture(CaptureState::Paused { window: "Zoom Meeting".into(), reason: "the window is minimised".into() })), "  ▲ paused  Zoom Meeting: the window is minimised\n");
        assert_eq!(plain(&Event::Capture(CaptureState::Asking { window: "Zoom Meeting".into(), reason: "it is not where it was".into(), candidates: vec![] })), "  ▲ asking  it is not where it was; choose or update “Zoom Meeting” in the LectureLive app\n");
        assert_eq!(plain(&Event::Capture(CaptureState::Denied)), "  ▲ screen recording  Screen Recording is off for Terminal: System Settings → Privacy & Security → Screen & System Audio Recording, then quit and reopen Terminal.\n");
        assert_eq!(plain(&Event::Capture(CaptureState::Failing { window: "Zoom Meeting".into(), reason: "3 failed captures in a row".into() })), "  ▲ capture failing  Zoom Meeting: 3 failed captures in a row\n");
        let selection = Selection { descriptor: Descriptor { bundle_id: Some("us.zoom.xos".into()), app: "Zoom".into(), title: "Zoom Meeting".into(), width: 1600, height: 900 }, region: Region::WHOLE, leave_out: vec![], sizes: vec![] };
        assert_eq!(plain(&Event::CaptureMoved { selection, note: "the slide moved with the window; watching it at the new size".into() }), "  ▣ found again  the slide moved with the window; watching it at the new size\n");
        assert_eq!(plain(&Event::Warning("slide 5 (slides/slide_5.png) is no longer on disk; left out of the notes".into())), "  ▲ warning  slide 5 (slides/slide_5.png) is no longer on disk; left out of the notes\n");
    }

    /// The host is part of the wording: a session in iTerm says iTerm, not the test's Terminal.
    #[test]
    fn capture_denied_names_the_host() {
        let mut out = Vec::new();
        show(&mut out, OFF, &Event::Capture(CaptureState::Denied), &mut None, &capture::Words { course: "Machine Learning".into(), host: "iTerm" });
        assert_eq!(String::from_utf8(out).unwrap(), "  ▲ screen recording  Screen Recording is off for iTerm: System Settings → Privacy & Security → Screen & System Audio Recording, then quit and reopen iTerm.\n");
        let mut out = Vec::new();
        show(&mut out, OFF, &Event::Capture(CaptureState::Unbound), &mut None, &capture::Words { course: "Statistics".into(), host: "this terminal app" });
        assert_eq!(String::from_utf8(out).unwrap(), "  ▣ no window  No Zoom window chosen for Statistics yet: choose it once in the LectureLive app. Screenshots (⌘⇧4) still become slides.\n");
    }

    #[test]
    fn say_paints_its_marks_and_labels() {
        assert_eq!(shown(ANSI, &Event::NothingNew, &mut None), "  \u{1b}[36m◆\u{1b}[0m \u{1b}[1msnapshot\u{1b}[0m  nothing new since the last one\n");
        assert_eq!(shown(TRUE, &Event::NothingNew, &mut None), "  \u{1b}[38;2;93;184;192m◆\u{1b}[0m \u{1b}[1msnapshot\u{1b}[0m  nothing new since the last one\n");
        assert_eq!(shown(ANSI, &Event::SnapshotFailed("the model refused".into()), &mut None), "  \u{1b}[31m▲\u{1b}[0m \u{1b}[1msnapshot failed\u{1b}[0m  the model refused\n");
        assert_eq!(shown(TRUE, &Event::SnapshotFailed("the model refused".into()), &mut None), "  \u{1b}[38;2;242;118;107m▲\u{1b}[0m \u{1b}[1msnapshot failed\u{1b}[0m  the model refused\n");
    }

    #[test]
    fn dim_and_teal_run_through_the_block_and_busy_lines() {
        assert_eq!(shown(ANSI, &Event::Busy("polishing 40 words".into()), &mut None), "\u{1b}[2m  … polishing 40 words\u{1b}[0m\n");
        assert_eq!(shown(TRUE, &Event::Busy("polishing 40 words".into()), &mut None), "\u{1b}[2m  … polishing 40 words\u{1b}[0m\n");
        assert_eq!(shown(ANSI, &Event::Session(Notification::Stt(SttStatus::Connected)), &mut None), "\u{1b}[2m  transcribing\u{1b}[0m\n");
        let e = Event::Committed { words: 486, slides: 2, block: "<!-- 10:42:03 -->\n## Sampling".into(), usd: 0.02, confirmed: true, removed: 0, missing: 0, revision: 3 };
        assert_eq!(shown(TRUE, &e, &mut None), "  \u{1b}[38;2;93;184;192m│\u{1b}[0m \u{1b}[2m## Sampling\u{1b}[0m\n  \u{1b}[38;2;93;184;192m◆\u{1b}[0m \u{1b}[1mnotes\u{1b}[0m  486 words and 2 slides folded in  \u{1b}[2m$0.02\u{1b}[0m\n");
    }

    #[test]
    fn parse_line_pins_the_stdin_grammar() {
        assert_eq!(parse_line(""), Op::Snapshot(String::new()));
        assert_eq!(parse_line("polish"), Op::Polish);
        assert_eq!(parse_line("Polish"), Op::Polish);
        assert_eq!(parse_line("  POLISH  "), Op::Polish);
        assert_eq!(parse_line("polish the notes"), Op::Snapshot("polish the notes".into()));
        assert_eq!(parse_line("  a hint about variance  "), Op::Snapshot("a hint about variance".into()));
    }

    fn date() -> chrono::NaiveDate {
        chrono::NaiveDate::from_ymd_opt(2026, 9, 26).unwrap()
    }

    fn files() -> LectureFiles {
        LectureFiles::standard(Path::new("/tmp/lec"), date())
    }

    fn prepared(init: InitReport) -> start::Prepared {
        start::Prepared { launch: LaunchReport::default(), init, other_days: Vec::new() }
    }

    fn rendered(ready: &start::Prepared) -> String {
        let mut out = Vec::new();
        print_prepared(&mut out, OFF, ready, &files(), "Machine Learning", "Week 03 — Optimisation", "BlackHole 2ch");
        String::from_utf8(out).unwrap()
    }

    const HEADER: &str = "\n  Machine Learning  ›  Week 03 — Optimisation\n  listening on BlackHole 2ch\n  ⏎ snapshot   a hint ⏎   polish ⏎   ^C stop\n\n";

    #[test]
    fn print_prepared_for_a_created_folder() {
        assert_eq!(rendered(&prepared(InitReport { how: How::Created, ..Default::default() })), format!("  ◆ notes  lecture_notes_20260926.md created\n{HEADER}"));
    }

    #[test]
    fn print_prepared_for_a_migrated_or_rebuilt_folder() {
        assert_eq!(rendered(&prepared(InitReport { how: How::Migrated, ..Default::default() })), format!("  ◆ migrated  this folder's Python CLI state is now the app's (the Python CLI no longer writes here)\n{HEADER}"));
        assert_eq!(rendered(&prepared(InitReport { how: How::Rebuilt, ..Default::default() })), format!("  ▲ rebuilt  the state was rebuilt from the notes: everything after the last <!-- --> marker is pending\n{HEADER}"));
    }

    #[test]
    fn print_prepared_for_each_journal_recovery() {
        assert_eq!(rendered(&prepared(InitReport { journal: Recovered::Completed, ..Default::default() })), format!("  ✓ recovered  the last snapshot was fully written\n{HEADER}"));
        assert_eq!(rendered(&prepared(InitReport { journal: Recovered::Truncated, ..Default::default() })), format!("  ▲ recovered  removed a half-written snapshot; its material is queued again\n{HEADER}"));
        assert_eq!(rendered(&prepared(InitReport { journal: Recovered::NotAppended, ..Default::default() })), format!("  ▲ recovered  an interrupted snapshot never reached the notes; its material is queued again\n{HEADER}"));
    }

    #[test]
    fn print_prepared_for_kept_corrupt_state_legacy_commit_and_external_edit() {
        let init = InitReport { corrupt_kept: Some(PathBuf::from("/tmp/lec/.live_notes/lecture_notes_20260926.v2.json.corrupt-104152")), ..Default::default() };
        assert_eq!(rendered(&prepared(init)), format!("  ▲ rebuilt  the corrupt state is kept as /tmp/lec/.live_notes/lecture_notes_20260926.v2.json.corrupt-104152\n{HEADER}"));
        let init = InitReport { legacy_commit: Some("finished the Python CLI's interrupted snapshot"), ..Default::default() };
        assert_eq!(rendered(&prepared(init)), format!("  ▲ recovered  finished the Python CLI's interrupted snapshot\n{HEADER}"));
        let init = InitReport { external_edit: true, ..Default::default() };
        assert_eq!(rendered(&prepared(init)), format!("  ◆ notes  edited outside the app since the last session; kept as they are\n{HEADER}"));
    }

    #[test]
    fn print_prepared_for_other_days_and_pending_material() {
        let mut ready = prepared(InitReport::default());
        ready.other_days = vec![
            ("lecture_notes_20260920".into(), StopReport { unresolved: 2, ..Default::default() }),
            ("lecture_notes_20260919".into(), StopReport::default()),
        ];
        assert_eq!(rendered(&ready), format!("  ✓ recovered  lecture_notes_20260920's transcript gaps, 2 still waiting\n  ✓ recovered  lecture_notes_20260919's transcript gaps\n{HEADER}"));
        let init = InitReport { pending_segments: 12, pending_slides: 1, ..Default::default() };
        assert_eq!(rendered(&prepared(init)).lines().nth(2).unwrap(), "  listening on BlackHole 2ch, resumed with 12 lines and 1 slide for the next snapshot");
    }

    #[test]
    fn print_prepared_for_launch_repair_and_retention() {
        let mut ready = prepared(InitReport::default());
        ready.launch = LaunchReport {
            repaired: vec![(PathBuf::from("/tmp/lec/recordings/session_1.wav"), 19200)],
            missing: vec![PathBuf::from("/tmp/lec/recordings/session_2.wav")],
            pruned: vec![PathBuf::from("/tmp/lec/recordings/old.wav")],
            untranscribed: Vec::new(),
        };
        assert_eq!(rendered(&ready), format!("  ✓ repaired  /tmp/lec/recordings/session_1.wav (1.2 s)\n  ▲ missing  /tmp/lec/recordings/session_2.wav (marked as a gap)\n  ✓ deleted  /tmp/lec/recordings/old.wav (retention)\n{HEADER}"));
    }

    #[test]
    fn print_prepared_paints_the_header() {
        let mut out = Vec::new();
        print_prepared(&mut out, TRUE, &prepared(InitReport::default()), &files(), "Machine Learning", "Week 03 — Optimisation", "BlackHole 2ch");
        assert_eq!(String::from_utf8(out).unwrap(), "\n  \u{1b}[1mMachine Learning\u{1b}[0m  \u{1b}[2m›\u{1b}[0m  Week 03 — Optimisation\n\u{1b}[2m  listening on BlackHole 2ch\u{1b}[0m\n\u{1b}[2m  ⏎ snapshot   a hint ⏎   polish ⏎   ^C stop\u{1b}[0m\n\n");
    }

    fn ended(report: StopReport, spend: &Spend) -> String {
        let mut out = Vec::new();
        print_end(&mut out, OFF, &files(), &report, spend);
        String::from_utf8(out).unwrap()
    }

    fn ledger() -> Spend {
        Spend::open(&scratch("ledger").join("spend.jsonl"), "Machine Learning", "Week 03 — Optimisation", date()).unwrap()
    }

    #[test]
    fn print_end_for_a_clean_lecture() {
        // A lecture that spent nothing today sums an empty day, which `money` renders as $-0.00.
        assert_eq!(ended(StopReport::default(), &ledger()), "\n  ✓ saved  lecture_notes_20260926.md  lecture_transcript_20260926.txt\n    $-0.00 spent on this lecture today; `lecture spend` has the rest\n\n");
    }

    #[test]
    fn print_end_with_failures_and_spend() {
        let spend = ledger();
        spend.add(SpendKind::Notes, 0.02, true, None).unwrap();
        let report = StopReport { unresolved: 2, last_snapshot: Some("the model refused".into()), ..Default::default() };
        assert_eq!(ended(report, &spend), "\n  ✓ saved  lecture_notes_20260926.md  lecture_transcript_20260926.txt\n  ▲ notes  the last snapshot failed (the model refused); the next session in this folder adds what it missed\n    2 transcript gaps still to recover; the next session in this folder does it\n    $0.02 spent on this lecture today; `lecture spend` has the rest\n\n");
    }

    #[test]
    fn print_end_paints_the_saved_line() {
        let mut out = Vec::new();
        print_end(&mut out, TRUE, &files(), &StopReport::default(), &ledger());
        assert_eq!(String::from_utf8(out).unwrap(), "\n  \u{1b}[38;2;93;184;192m✓\u{1b}[0m \u{1b}[1msaved\u{1b}[0m  lecture_notes_20260926.md  lecture_transcript_20260926.txt\n\u{1b}[2m    $-0.00 spent on this lecture today; `lecture spend` has the rest\u{1b}[0m\n\n");
    }

    /// A writer whose writes fail (a broken pipe): the seam panics as `println!` did, never swallowing a line.
    struct Broken;

    impl std::io::Write for Broken {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    #[should_panic(expected = "failed printing to stdout")]
    fn a_failed_say_line_panics_as_println_did() {
        say(&mut Broken, OFF, "notes", "notes", "nothing new");
    }

    #[test]
    #[should_panic(expected = "failed printing to stdout")]
    fn a_failed_show_line_panics_as_println_did() {
        show(&mut Broken, OFF, &Event::Busy("polishing 40 words".into()), &mut None, &words());
    }

    #[test]
    #[should_panic(expected = "failed printing to stdout")]
    fn a_failed_prepared_line_panics_as_println_did() {
        print_prepared(&mut Broken, OFF, &prepared(InitReport::default()), &files(), "Machine Learning", "Week 03 — Optimisation", "BlackHole 2ch");
    }

    #[test]
    #[should_panic(expected = "failed printing to stdout")]
    fn a_failed_end_line_panics_as_println_did() {
        print_end(&mut Broken, OFF, &files(), &StopReport::default(), &ledger());
    }

    const STOPPING: &str = "  ◆ stopping  finishing the transcript and recovery, then a last snapshot (Ctrl-C again stops waiting for recovery)\n";
    const STOP_WAITING: &str = "  ▲ stopping  no longer waiting for recovery or queued requests; its gaps wait for the next session (Ctrl-C again quits at once)\n";
    const QUIT: &str = "  ▲ quit  stopped at once; the next session in this folder picks up what was left\n";

    /// One Ctrl-C through plain's handler: what it printed, how many `Stop`s it sent, and the step.
    fn ctrl_c(controller: &mut StopController) -> (String, usize, Step) {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut out = Vec::new();
        let step = request_stop(&mut out, OFF, controller, Origin::Signal, &tx).unwrap();
        let mut sent = 0;
        while let Ok(LectureCommand::Stop) = rx.try_recv() {
            sent += 1;
        }
        (String::from_utf8(out).unwrap(), sent, step)
    }

    #[test]
    fn ctrl_c_stop_lines_are_unchanged() {
        let mut controller = StopController::default();
        assert_eq!(ctrl_c(&mut controller), (STOPPING.into(), 1, Step::Advance { stage: Stage::Stopping, stops_to_send: 1 }));
        assert_eq!(ctrl_c(&mut controller), (STOP_WAITING.into(), 1, Step::Advance { stage: Stage::StopWaiting, stops_to_send: 1 }));
        assert_eq!(ctrl_c(&mut controller), (QUIT.into(), 0, Step::Quit));
    }

    /// Plan §B (a), the Task 2 ruling: the `--secs` timer's silent `Stop` is stage 1, so the next Ctrl-C
    /// prints the stage-2 words core then acts on.
    #[test]
    fn secs_then_ctrl_c_prints_the_stop_waiting_line() {
        let mut controller = StopController::default();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let step = request_stop(&mut std::io::sink(), OFF, &mut controller, Origin::Timer, &tx).unwrap();
        assert_eq!(step, Step::Advance { stage: Stage::Stopping, stops_to_send: 1 });
        assert!(matches!(rx.try_recv(), Ok(LectureCommand::Stop)) && rx.try_recv().is_err());
        assert_eq!(ctrl_c(&mut controller), (STOP_WAITING.into(), 1, Step::Advance { stage: Stage::StopWaiting, stops_to_send: 1 }));
        assert_eq!(ctrl_c(&mut controller), (QUIT.into(), 0, Step::Quit));
    }

    /// Plain prints nothing when the audio ends and never feeds `SourceEnded` to its controller, so a
    /// Ctrl-C after it still means "stop": one `Stop`, the stage-1 words, both true of core then.
    #[test]
    fn plain_after_source_ended_still_means_stop() {
        let mut controller = StopController::default();
        assert_eq!(plain(&Event::Session(Notification::SourceEnded)), "");
        assert_eq!(ctrl_c(&mut controller), (STOPPING.into(), 1, Step::Advance { stage: Stage::Stopping, stops_to_send: 1 }));
        // The TUI feeds it: there the same Ctrl-C stops waiting, with two `Stop`s.
        let mut fed = StopController::default();
        fed.advance(Origin::SourceEnded, std::time::Instant::now());
        assert_eq!(ctrl_c(&mut fed), (STOP_WAITING.into(), 2, Step::Advance { stage: Stage::StopWaiting, stops_to_send: 2 }));
    }

    /// M7 plan §C 12 and §J: nothing shown can act on a terminal. The introducers go (ESC and every
    /// C0/C1 control), so each sequence's payload is left as the inert text it then is.
    #[test]
    fn display_text_cannot_emit_terminal_controls() {
        // Whole sequences go, payload and all; what a person would read stays.
        let cases = [
            ("\x1b[31mred\x1b[0m", "red"),                                   // a CSI colour
            ("\x1b[1;38;2;255;0;0mbold red", "bold red"),                    // a truecolour CSI
            ("before\x1b[2J\x1b[Hafter", "beforeafter"),                     // clear screen, cursor home
            ("\x1b]0;popup title\x07said", "said"),                          // an OSC title, BEL-terminated
            ("\x1b]2;window title\x1b\\said", "said"),                       // an OSC title, ST-terminated
            ("\x1b]52;c;QmFzZTY0\x07copied?", "copied?"),                    // an OSC 52 clipboard write
            ("\x1b]8;;http://x.example\x1b\\click\x1b]8;;\x1b\\", "click"), // an OSC 8 hyperlink: its text stays
            ("\x1bPq#0;2;0;0;0\x1b\\sixel", "sixel"),                        // a DCS string
            ("\x1bXsos\x1b\\\x1b^pm\x1b\\\x1b_apc\x1b\\end", "end"),         // SOS, PM, APC
            ("line\u{9b}31mfeed\u{85}next", "linefeednext"),                 // C1 CSI and NEL as characters
            ("\u{9d}0;c1 title\u{9c}text", "text"),                          // a C1 OSC ended by C1 ST
            ("fine\x07audio", "fineaudio"),                                  // BEL
            ("was written\rwas rewritten", "was written\nwas rewritten"),    // CR cannot rewrite the line
            ("windows\r\nline", "windows\nline"),                            // CR LF is one line break
            ("a\x00b\x1fc\x7fd\x1b", "abcd"),                                // C0, DEL, a trailing ESC
            ("\x1b(Bcharset \x1b7saved\x1b8", "charset saved"),              // nF and Fp escapes
            ("\x1b[émoji", "émoji"),                                         // a broken CSI keeps the text
            ("unterminated \x1b]0;title", "unterminated "),                  // an OSC that never ends
        ];
        for (text, want) in cases {
            let safe = clean(text);
            assert_eq!(safe, want, "{text:?}");
            assert!(safe.chars().all(|c| c == '\n' || c == '\t' || !c.is_control()), "{text:?} left a control: {safe:?}");
        }
        // what people and models actually write survives, Markdown and all
        for kept in [
            "Voilà — naïve… 🎓 ελληνικά 数学 👩🏽‍🔬",
            "## Heading\n\n- bullet **bold** `code` [link](https://example.org)\n![Slide 3](slides/slide_3.png)\n",
            "col1\tcol2\n  indented [x] ~tilde~ ^caret^ _under_ \\back\n",
        ] {
            assert_eq!(clean(kept), kept);
        }
    }

    /// The shared wording, unstyled: what the TUI's activity and notice line hold, and what `show`
    /// then paints (already pinned byte for byte by the goldens above).
    #[test]
    fn notice_carries_the_plain_words_without_styling() {
        let off = spend::Paint { color: false, truecolor: false };
        let on = spend::Paint { color: true, truecolor: true };
        let w = &words();
        let e = Event::Committed { words: 486, slides: 2, block: "<!-- 10:42:03 -->\n## Sampling".into(), usd: 0.02, confirmed: true, removed: 0, missing: 0, revision: 3 };
        let n = notice(&e, off, w).unwrap();
        assert_eq!((n.kind, n.label.as_str(), n.detail.as_str()), ("notes", "notes", "486 words and 2 slides folded in  $0.02"));
        assert!(!n.detail.contains('\x1b'), "the TUI's wording carries no styling");
        assert_eq!(notice(&e, on, w).unwrap().detail, "486 words and 2 slides folded in  \x1b[2m$0.02\x1b[0m", "the plain CLI's own paint is unchanged");
        assert_eq!(notice(&Event::Session(Notification::Open { stable: "a".into(), tentative: "b".into() }), off, w), None);
        assert_eq!(notice(&Event::Busy("snapshot, 12 words".into()), off, w), None, "busy text is shown verbatim, never a notice");
        assert_eq!(notice(&Event::Session(Notification::Stt(SttStatus::Connected)), off, w), None);
        let gone = notice(&Event::Session(Notification::DeviceGone { uid: "Gone-UID".into() }), off, w).unwrap();
        assert_eq!((gone.kind, gone.label.as_str()), ("warn", "input gone"));
        assert_eq!(gone.detail, "Gone-UID; waiting for it to return, and nothing switches by itself", "the offer is the plain CLI's own addition");
        // capture states use the terminal's wording, unstyled
        let n = notice(&Event::Capture(CaptureState::Watching { window: "Zoom Meeting".into() }), on, w).unwrap();
        assert_eq!((n.kind, n.label.as_str(), n.detail.as_str()), ("slide", "watching", "Zoom Meeting"));
        let n = notice(&Event::Capture(CaptureState::Denied), on, w).unwrap();
        assert_eq!(n.detail, "Screen Recording is off for Terminal: System Settings → Privacy & Security → Screen & System Audio Recording, then quit and reopen Terminal.", "no styling and no host guessing");
    }

    /// The start-up records are what `print_prepared` prints — the pinned goldens cover the printing —
    /// so the TUI's seed says what the plain report said.
    #[test]
    fn startup_records_are_the_printed_lines() {
        let mut ready = prepared(InitReport { how: How::Rebuilt, journal: Recovered::Truncated, ..Default::default() });
        ready.launch.repaired = vec![(PathBuf::from("/tmp/lec/recordings/session_1.wav"), 19200)];
        let records = startup_records(&ready, &files());
        assert_eq!(
            records.iter().map(|n| (n.kind, n.label.as_str(), n.detail.as_str())).collect::<Vec<_>>(),
            vec![
                ("done", "repaired", "/tmp/lec/recordings/session_1.wav (1.2 s)"),
                ("warn", "rebuilt", "the state was rebuilt from the notes: everything after the last <!-- --> marker is pending"),
                ("warn", "recovered", "removed a half-written snapshot; its material is queued again"),
            ]
        );
    }
}
