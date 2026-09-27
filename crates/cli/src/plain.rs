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
use lecturelive_core::session::sidecar::Gap;
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

/// The plain frontend's output context (plan §C 11, Task 12): how its lines are painted, and
/// whether the stream they go to is a terminal — two independent facts. A TTY cleans untrusted
/// text of terminal controls whatever `NO_COLOR` says (safety is not colour); a pipe keeps the
/// bytes exactly as they were, controls and all, so every ordinary control-free line is
/// byte-identical to what the CLI printed before.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Display {
    pub(crate) paint: spend::Paint,
    pub(crate) tty: bool,
}

impl Display {
    /// The session's context, read once as the command starts: colour as the plain CLI has always
    /// decided it, and stdout's terminal-ness from `IsTerminal` — never from the colour choice.
    pub(crate) fn stdout() -> Display {
        use std::io::IsTerminal;
        Display { tty: std::io::stdout().is_terminal(), paint: paint() }
    }

    /// Untrusted text on its way to this display: unchanged, byte for byte, when it is not a
    /// terminal; cleaned whole when it is — before any painting of our own, so LectureLive's own
    /// styling survives and injected sequences do not.
    fn safe<'a>(&self, text: &'a str) -> std::borrow::Cow<'a, str> {
        if self.tty { std::borrow::Cow::Owned(clean(text)) } else { std::borrow::Cow::Borrowed(text) }
    }
}

/// The CLI's `say`: one event line, its mark, what it is, what happened. The label and detail are
/// the event's own words — cleaned of terminal controls first when the display is a TTY.
pub(crate) fn say(out: &mut impl Write, d: Display, kind: &str, label: &str, detail: &str) {
    say_line(out, d, kind, label, detail, true)
}

/// A say-line whose detail already embeds the CLI's own painting and was built from cleaned
/// fragments — the committed, polished and page results. Only its label is cleaned here: cleaning
/// the detail again would strip LectureLive's own styling along with everyone else's sequences,
/// inverting the order plan §C 12 asks (clean, then paint).
fn say_painted(out: &mut impl Write, d: Display, kind: &str, label: &str, detail: &str) {
    say_line(out, d, kind, label, detail, false)
}

/// One say-line, its detail cleaned on a TTY unless it was built pre-cleaned and painted.
fn say_line(out: &mut impl Write, d: Display, kind: &str, label: &str, detail: &str, clean_detail: bool) {
    let (mark, colour) = match kind {
        "slide" => ("▣", "teal"),
        "notes" => ("◆", "teal"),
        "page" => ("✦", "teal"),
        "done" => ("✓", "teal"),
        _ => ("▲", "red"),
    };
    let label = d.safe(label);
    let detail = clean_detail.then(|| d.safe(detail).into_owned()).unwrap_or_else(|| detail.to_string());
    writeln!(out, "{}", format!("  {} {}  {detail}", d.paint.paint(mark, &[colour]), d.paint.paint(&label, &["bold"])).trim_end()).unwrap_or_else(|e| panic!("failed printing to stdout: {e}"));
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

/// The input-gone notice (plan §H's notice line, top priority). A single source's fixed sentence
/// says both facts: LectureLive waits, and nothing is recorded from it meanwhile. A mixed source
/// says the other truth — one input of two is away and Zoom's loopback keeps recording — so the
/// single-source promise is never said where it is false. The uid is the label, the one bold slot
/// the line has, as the plan bolds it. The plain CLI appends its offer of the other inputs after
/// either; the TUI shows the sentence as it is.
pub(crate) fn input_gone(uid: &str, mixed: bool) -> Notice {
    let detail = if mixed { "is unplugged. LectureLive waits for it; Zoom's audio still records meanwhile." } else { "is unplugged. LectureLive waits for it and records nothing meanwhile." };
    Notice { kind: "warn", label: uid.to_string(), detail: detail.into() }
}

/// A gap's interval as the notices say it: "0.2–1.0 s", or "2.0 s onward" while it is still open.
fn gap_span(g: &Gap) -> String {
    match g.end_sample {
        Some(end) => format!("{:.1}–{:.1} s", secs(g.start_sample), secs(end)),
        None => format!("{:.1} s onward", secs(g.start_sample)),
    }
}

/// One gap in the words both frontends share (spec §5.4): a transcript gap still has its audio, so
/// it waits for recovery to fill the words in; an audio gap's recording was lost for the interval
/// and nothing can transcribe what was never captured — so it is never said to be waiting for
/// recovery. No kind is named by its `Debug` vocabulary.
pub(crate) fn gap_words(g: &Gap) -> Notice {
    if g.kind.is_transcript() {
        Notice { kind: "warn", label: "gap".into(), detail: format!("no transcription of {} of recording {}; recovery fills it in when it can", gap_span(g), g.recording_id) }
    } else {
        Notice { kind: "warn", label: "gap".into(), detail: format!("the recording's audio was lost for {} of recording {}; it cannot be recovered from the recording", gap_span(g), g.recording_id) }
    }
}

/// The loopback silence warning (spec §4.3), as both frontends' watches say it.
pub(crate) fn no_signal() -> Notice {
    Notice { kind: "warn", label: "no signal".into(), detail: format!("10 s of silence on BlackHole: is Zoom's Speaker \"{}\"?", loopback::LOOPBACK_NAME) }
}

/// The committed line's detail: what was folded in and what it cost. The amount is painted dim by
/// the plain CLI and plain for the TUI — the same words either way. Every fragment here is the
/// CLI's own (counts and money), so there is nothing to clean; the paint embeds as it is built,
/// after cleaning, never under it.
fn committed_detail(words: usize, slides: usize, usd: f64, confirmed: bool, missing: usize, d: Display) -> String {
    let mut detail = format!("{} and {} folded in  {}", plural(words, "word"), plural(slides, "slide"), d.paint.paint(&spend::money(usd), &["dim"]));
    if missing > 0 {
        detail += &format!("  ({} not placed by the model, listed at the end)", plural(missing, "slide"));
    }
    if !confirmed {
        detail += "  (transcription still catching up; the rest goes into the next snapshot)";
    }
    detail
}

/// The polished line's detail: where the previous version went, and what the polish cost. The
/// backup's name is the folder's own word for it, cleaned as it enters on a TTY — before the
/// money's paint, so our own styling is never cleaned away with anyone else's.
fn polished_detail(backup: &Path, usd: f64, d: Display) -> String {
    let name = d.safe(&backup.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()).into_owned();
    format!("previous version in .live_notes/{name}  {}", d.paint.paint(&spend::money(usd), &["dim"]))
}

/// The page line's detail: what was typeset, how long it came out, what it cost. The page's name
/// and the missing list are cleaned first on a TTY; the length keeps its over-budget red and the
/// money its dim, embedded after the cleaning.
fn page_detail(outcome: &PageOutcome, usd: f64, d: Display) -> String {
    let name = d.safe(&outcome.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()).into_owned();
    let length = d.paint.paint(&format!("{} words of {} allowed", outcome.words, outcome.budget), &[if outcome.words > outcome.budget as usize { "red" } else { "dim" }]);
    let mut detail = format!("{name}  {length}  {}", d.paint.paint(&spend::money(usd), &["dim"]));
    if outcome.cached {
        detail += "  (notes unchanged since they were typeset: only the design reapplied, free)";
    }
    if !outcome.missing.is_empty() {
        detail += &format!("  (missing: {})", d.safe(&outcome.missing.join(", ")));
    }
    detail
}

/// One notice in the CLI's line: `say`, from the shared wording, cleaned when the display is a TTY.
fn say_notice(out: &mut impl Write, d: Display, n: &Notice) {
    say(out, d, n.kind, &n.label, &n.detail);
}

/// One notice whose detail already embeds the CLI's own painting (the committed, polished and page
/// results): its untrusted fragments were cleaned as it was built, so only the label is cleaned
/// again here.
fn say_notice_painted(out: &mut impl Write, d: Display, n: &Notice) {
    say_painted(out, d, n.kind, &n.label, &n.detail);
}

/// An event as a notice, in the plain CLI's own words (the TUI's activity and notice line show the
/// same wording, with `d` colour-off and never a TTY, so no styling enters the view and the TUI
/// cleans at its own boundary as it records). Capture states and relocations use the terminal's
/// own words ([`capture::state_words`]), not the desktop's. `mixed` says whether a gone input is
/// one of two (Zoom's loopback keeps recording), which alone changes the input-gone sentence.
pub(crate) fn notice(e: &Event, d: Display, words: &capture::Words, mixed: bool) -> Option<Notice> {
    let n = |kind: &'static str, label: &str, detail: String| Some(Notice { kind, label: label.into(), detail });
    match e {
        Event::Session(m) => match m {
            Notification::Gap(g) => Some(gap_words(g)),
            Notification::DeviceGone { uid } => Some(input_gone(uid, mixed)),
            Notification::DeviceBack { uid } => n("done", "input back", format!("{uid}; recording continues in a new file")),
            Notification::Failed(m) => n("warn", "session failed", m.clone()),
            Notification::Stt(s) => match s {
                SttStatus::Retrying { after, reason } => n("warn", "transcription interrupted", format!("{reason}; reconnecting in {} s", after.as_secs())),
                SttStatus::Refused(m) => n("warn", "transcription refused", format!("{}. Recording continues without it.", sentence(m))),
                SttStatus::ServerError(m) => n("warn", "transcription server", m.clone()),
                SttStatus::Stopped(m) => n("warn", "transcription stopped", m.clone()),
                SttStatus::Connected => None,
            },
            // Core resolves a transcript gap by saying so. A `Recovered` for an audio gap — which
            // core never sends, they are born resolved — is answered with the truth instead: the
            // interval's audio was lost, so there is no transcript to recover (plan Task 12).
            Notification::Recovered(g) if g.kind.is_transcript() => n("done", "recovered", format!("the transcript of {} of recording {}", gap_span(g), g.recording_id)),
            Notification::Recovered(g) => n("warn", "gap", format!("the recording's audio for {} of recording {} was lost; there is no transcript to recover", gap_span(g), g.recording_id)),
            Notification::RecoveryFailed(m) => n("warn", "recovery", m.clone()),
            Notification::SpendFailed(m) => n("warn", "spend", m.clone()),
            _ => None,
        },
        Event::NothingNew => n("notes", "snapshot", "nothing new since the last one".into()),
        Event::Committed { words, slides, usd, confirmed, missing, .. } => n("notes", "notes", committed_detail(*words, *slides, *usd, *confirmed, *missing, d)),
        Event::SnapshotFailed(m) => n("warn", "snapshot failed", m.clone()),
        Event::Polished { backup, usd, .. } => n("done", "polished", polished_detail(backup, *usd, d)),
        Event::PolishStopped(m) => n("warn", "polish stopped", m.clone()),
        Event::PolishFailed(m) => n("warn", "polish failed", m.clone()),
        Event::Cancelled(what) => n("warn", "cancelled", format!("{what}; nothing was written, everything is kept for the next snapshot")),
        Event::Page { outcome, usd } => n("done", "page", page_detail(outcome, *usd, d)),
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
/// capture states arrive in the terminal's own `words`. Every event-sourced string is cleaned
/// before it is written when `d` is a TTY (plan §C 12, Task 12); a pipe sees the bytes as they are.
pub(crate) fn show(out: &mut impl Write, d: Display, e: &Event, watch: &mut Option<SilenceWatch>, words: &capture::Words, mixed: bool) {
    // The plain CLI's own lines first — the transcript, the recording path, a first `transcribing`,
    // the busy text — and the events whose wording needs the machine (the gone input's offer).
    match e {
        Event::Session(Notification::Segment(s)) => {
            let tag = if s.source == SegmentSource::Recovered { d.paint.paint("  (recovered)", &["dim"]) } else { String::new() };
            let text = d.safe(&s.text);
            writeln!(out, "  {}  {}{tag}", d.paint.paint(&s.said_at.format("%H:%M:%S").to_string(), &["dim"]), text.as_ref()).unwrap_or_else(|e| panic!("failed printing to stdout: {e}"));
        }
        Event::Session(Notification::Level(l)) => {
            if watch.as_mut().is_some_and(|w| w.observe(*l)) {
                say_notice(out, d, &no_signal());
            }
        }
        Event::Session(Notification::Recording { path }) => {
            let shown = d.safe(&path.display().to_string()).into_owned();
            writeln!(out, "{}", d.paint.paint(&format!("  recording to {shown}"), &["dim"])).unwrap_or_else(|e| panic!("failed printing to stdout: {e}"));
        }
        // The notice is the fixed sentence; the offer of the other inputs is the plain CLI's own.
        Event::Session(Notification::DeviceGone { uid }) => {
            let n = input_gone(uid, mixed);
            say(out, d, n.kind, &n.label, &format!("{}. {}", sentence(&n.detail), other_inputs(uid)));
        }
        Event::Session(Notification::Stt(SttStatus::Connected)) => { writeln!(out, "{}", d.paint.paint("  transcribing", &["dim"])).unwrap_or_else(|e| panic!("failed printing to stdout: {e}")); }
        Event::Busy(m) => { writeln!(out, "{}", d.paint.paint(&format!("  … {}", d.safe(m)), &["dim"])).unwrap_or_else(|e| panic!("failed printing to stdout: {e}")); }
        Event::Preview(_) => {} // the committed block is printed instead, as the CLI does
        Event::Committed { block, .. } => {
            for line in block.trim().lines().filter(|l| !l.starts_with("<!-- ")) {
                writeln!(out, "  {} {}", d.paint.paint("│", &["teal"]), d.paint.paint(d.safe(line).as_ref(), &["dim"])).unwrap_or_else(|e| panic!("failed printing to stdout: {e}"));
            }
            if let Some(n) = notice(e, d, words, mixed) {
                say_notice_painted(out, d, &n);
            }
        }
        // The polished and page results embed the CLI's own painting (the money's dim, the page
        // length's red), built from fragments already cleaned above: whole-detail cleaning would
        // strip our own styling with everyone else's sequences.
        Event::Polished { .. } | Event::Page { .. } => {
            if let Some(n) = notice(e, d, words, mixed) {
                say_notice_painted(out, d, &n);
            }
        }
        _ => {
            if let Some(n) = notice(e, d, words, mixed) {
                say_notice(out, d, &n);
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
/// initialisation found, then the lecture's header lines. The lecture's own names — course, folder,
/// input — are as untrusted as anything else the folder says, so they are cleaned on a TTY.
pub(crate) fn print_prepared(out: &mut impl Write, d: Display, ready: &start::Prepared, files: &LectureFiles, course: &str, name: &str, input_name: &str) {
    for n in startup_records(ready, files) {
        say_notice(out, d, &n);
    }
    let init = &ready.init;
    let (course, name, input_name) = (&d.safe(course), &d.safe(name), &d.safe(input_name));

    writeln!(out).unwrap_or_else(|e| panic!("failed printing to stdout: {e}"));
    writeln!(out, "  {}  {}  {name}", d.paint.paint(course, &["bold"]), d.paint.paint("›", &["dim"])).unwrap_or_else(|e| panic!("failed printing to stdout: {e}"));
    let waiting = if init.pending_segments > 0 || init.pending_slides > 0 {
        format!(", resumed with {} and {} for the next snapshot", plural(init.pending_segments as usize, "line"), plural(init.pending_slides, "slide"))
    } else {
        String::new()
    };
    writeln!(out, "{}", d.paint.paint(&format!("  listening on {input_name}{waiting}"), &["dim"])).unwrap_or_else(|e| panic!("failed printing to stdout: {e}"));
    writeln!(out, "{}", d.paint.paint("  ⏎ snapshot   a hint ⏎   polish ⏎   ^C stop", &["dim"])).unwrap_or_else(|e| panic!("failed printing to stdout: {e}"));
    writeln!(out).unwrap_or_else(|e| panic!("failed printing to stdout: {e}"));
}

/// The end summary: what was saved, what still waits for the next session, and what the lecture cost
/// today. The file names are the folder's own words; the failure is core's — both cleaned on a TTY.
pub(crate) fn print_end(out: &mut impl Write, d: Display, files: &LectureFiles, report: &StopReport, spend: &Spend) {
    let file_name = |f: &Path| d.safe(&f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()).into_owned();
    writeln!(out).unwrap_or_else(|e| panic!("failed printing to stdout: {e}"));
    writeln!(out, "  {} {}  {}  {}", d.paint.paint("✓", &["teal"]), d.paint.paint("saved", &["bold"]), file_name(&files.notes), file_name(&files.transcript)).unwrap_or_else(|e| panic!("failed printing to stdout: {e}"));
    if let Some(e) = &report.last_snapshot {
        say(out, d, "warn", "notes", &format!("the last snapshot failed ({e}); the next session in this folder adds what it missed"));
    }
    if report.unresolved > 0 {
        writeln!(out, "{}", d.paint.paint(&format!("    {} still to recover; the next session in this folder does it", plural(report.unresolved, "transcript gap")), &["dim"])).unwrap_or_else(|e| panic!("failed printing to stdout: {e}"));
    }
    writeln!(out, "{}", d.paint.paint(&format!("    {} spent on this lecture today; `lecture spend` has the rest", spend::money(spend.lecture_total())), &["dim"])).unwrap_or_else(|e| panic!("failed printing to stdout: {e}"));
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
/// then the `Stop`s it asks for. Stage 3 sends nothing; the caller quits. Its words are all the
/// CLI's own, so the display context is only for the line itself.
pub(crate) fn request_stop(out: &mut impl Write, d: Display, controller: &mut StopController, origin: Origin, stop_tx: &tokio::sync::mpsc::UnboundedSender<LectureCommand>) -> Result<Step, tokio::sync::mpsc::error::SendError<LectureCommand>> {
    let step = controller.advance(origin, Instant::now());
    match step {
        Step::Advance { stage: Stage::Stopping, .. } => say(out, d, "notes", "stopping", "finishing the transcript and recovery, then a last snapshot (Ctrl-C again stops waiting for recovery)"),
        Step::Advance { stage: Stage::StopWaiting, .. } => say(out, d, "warn", "stopping", "no longer waiting for recovery or queued requests; its gaps wait for the next session (Ctrl-C again quits at once)"),
        // The recording is durable to its last second and gaps and journals are on disk: the next
        // session in this folder repairs, recovers and notes what is left.
        Step::Quit => say(out, d, "warn", "quit", "stopped at once; the next session in this folder picks up what was left"),
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
pub(crate) fn stop_on_ctrl_c(stop_tx: tokio::sync::mpsc::UnboundedSender<LectureCommand>, d: Display, controller: Arc<Mutex<StopController>>) {
    tokio::spawn(async move {
        while tokio::signal::ctrl_c().await.is_ok() {
            let step = request_stop(&mut std::io::stdout().lock(), d, &mut controller.lock().expect("the stop controller"), Origin::Signal, &stop_tx);
            match step {
                Ok(Step::Quit) => std::process::exit(130),
                Ok(_) => {}
                Err(_) => return,
            }
        }
    });
}

/// The `--secs` timer: the first stop when the limit passes, silent as it has always been.
pub(crate) fn stop_after_secs(limit: u64, timer_tx: tokio::sync::mpsc::UnboundedSender<LectureCommand>, d: Display, controller: Arc<Mutex<StopController>>) {
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(limit)).await;
        let _ = request_stop(&mut std::io::sink(), d, &mut controller.lock().expect("the stop controller"), Origin::Timer, &timer_tx);
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
    use lecturelive_core::session::sidecar::{Gap, GapKind, Sidecar};
    use lecturelive_core::session::spend::SpendKind;

    const OFF: spend::Paint = spend::Paint { color: false, truecolor: false };
    const ANSI: spend::Paint = spend::Paint { color: true, truecolor: false };
    const TRUE: spend::Paint = spend::Paint { color: true, truecolor: true };

    /// The plain display a pipe gives: no colour, and no cleaning — bytes as they are.
    fn pipe(p: spend::Paint) -> Display {
        Display { paint: p, tty: false }
    }

    /// The plain display a terminal gives: colour as decided, cleaning on.
    fn term(p: spend::Paint) -> Display {
        Display { paint: p, tty: true }
    }

    fn shown(d: Display, e: &Event, watch: &mut Option<SilenceWatch>) -> String {
        let mut out = Vec::new();
        show(&mut out, d, e, watch, &words(), false);
        String::from_utf8(out).unwrap()
    }

    fn plain(e: &Event) -> String {
        shown(pipe(OFF), e, &mut None)
    }

    /// The words the tests print capture states in: a fixed course and host, so no test depends on
    /// the machine running it.
    fn words() -> capture::Words {
        capture::Words { course: "Machine Learning".into(), host: "Terminal" }
    }

    fn scratch(name: &str) -> PathBuf {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!("lecturelive-goldens-{}-{name}-{}", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
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
        // Task 12: user-facing gap words, one sentence for each kind of truth — never `Debug`.
        assert_eq!(plain(&Event::Session(Notification::Gap(gap()))), "  ▲ gap  the recording's audio was lost for 0.2–1.0 s of recording 11111111-1111-1111-1111-111111111111; it cannot be recovered from the recording\n");
        let transcript = Gap { kind: GapKind::SttOffline, ..gap() };
        assert_eq!(plain(&Event::Session(Notification::Gap(transcript.clone()))), "  ▲ gap  no transcription of 0.2–1.0 s of recording 11111111-1111-1111-1111-111111111111; recovery fills it in when it can\n");
        assert_eq!(plain(&Event::Session(Notification::DeviceBack { uid: "BlackHole-UID".into() })), "  ✓ input back  BlackHole-UID; recording continues in a new file\n");
        assert_eq!(plain(&Event::Session(Notification::Failed("disk full".into()))), "  ▲ session failed  disk full\n");
        assert_eq!(plain(&Event::Session(Notification::Recovered(transcript))), "  ✓ recovered  the transcript of 0.2–1.0 s of recording 11111111-1111-1111-1111-111111111111\n");
        // a `Recovered` for an audio gap, which core never sends, is answered with the truth
        assert_eq!(plain(&Event::Session(Notification::Recovered(gap()))), "  ▲ gap  the recording's audio for 0.2–1.0 s of recording 11111111-1111-1111-1111-111111111111 was lost; there is no transcript to recover\n");
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
            seen += &shown(pipe(OFF), &Event::Session(Notification::Level(quiet)), &mut watch);
        }
        assert_eq!(seen, "");
        seen += &shown(pipe(OFF), &Event::Session(Notification::Level(quiet)), &mut watch);
        assert_eq!(seen, "  ▲ no signal  10 s of silence on BlackHole: is Zoom's Speaker \"LectureLive Loopback\"?\n");
        seen += &shown(pipe(OFF), &Event::Session(Notification::Level(quiet)), &mut watch);
        assert_eq!(seen, "  ▲ no signal  10 s of silence on BlackHole: is Zoom's Speaker \"LectureLive Loopback\"?\n", "one warning per silent stretch");
    }

    /// `DeviceGone`'s offer lists this machine's inputs, so only the prefix and the offer's shape are pinned.
    #[test]
    fn device_gone_keeps_a_stable_prefix_before_the_machine_input_list() {
        let line = plain(&Event::Session(Notification::DeviceGone { uid: "Gone-UID".into() }));
        let prefix = "  ▲ Gone-UID  is unplugged. LectureLive waits for it and records nothing meanwhile. ";
        let rest = line.strip_prefix(prefix).unwrap_or_else(|| panic!("the stable prefix was lost: {line:?}"));
        assert!(
            rest == "No other input is connected; Ctrl-C stops."
                || rest.starts_with("To record from another input, stop (Ctrl-C) and start again with --device <UID>: "),
            "the machine's inputs follow the fixed offer: {rest:?}"
        );
    }
    /// Plan §H's input-gone sentences, by source: a single input records nothing while it is away;
    /// a mixed source's missing mic never implies Zoom stopped. The uid is the label — the line's
    /// one bold slot — and nothing switches by itself in either case.
    #[test]
    fn input_gone_says_what_still_records_by_source() {
        let single = input_gone("Receiver_UID", false);
        assert_eq!((single.kind, single.label.as_str()), ("warn", "Receiver_UID"));
        assert_eq!(single.detail, "is unplugged. LectureLive waits for it and records nothing meanwhile.");
        let mixed = input_gone("Receiver_UID", true);
        assert_eq!(mixed.label, "Receiver_UID");
        assert_eq!(mixed.detail, "is unplugged. LectureLive waits for it; Zoom's audio still records meanwhile.");
        assert!(!mixed.detail.contains("records nothing"), "a missing mic is not the loopback stopping");
    }

    /// A mixed source's gone input prints its own sentence and the same offer, never the
    /// single-source promise that nothing is recorded.
    #[test]
    fn a_mixed_source_gone_input_does_not_say_recording_stopped() {
        let mut out = Vec::new();
        show(&mut out, pipe(OFF), &Event::Session(Notification::DeviceGone { uid: "Mic-UID".into() }), &mut None, &words(), true);
        let line = String::from_utf8(out).unwrap();
        let prefix = "  ▲ Mic-UID  is unplugged. LectureLive waits for it; Zoom's audio still records meanwhile. ";
        let rest = line.strip_prefix(prefix).unwrap_or_else(|| panic!("{line:?}"));
        assert!(rest == "No other input is connected; Ctrl-C stops." || rest.starts_with("To record from another input"), "{rest:?}");
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
        show(&mut out, pipe(OFF), &Event::Capture(CaptureState::Denied), &mut None, &capture::Words { course: "Machine Learning".into(), host: "iTerm" }, false);
        assert_eq!(String::from_utf8(out).unwrap(), "  ▲ screen recording  Screen Recording is off for iTerm: System Settings → Privacy & Security → Screen & System Audio Recording, then quit and reopen iTerm.\n");
        let mut out = Vec::new();
        show(&mut out, pipe(OFF), &Event::Capture(CaptureState::Unbound), &mut None, &capture::Words { course: "Statistics".into(), host: "this terminal app" }, false);
        assert_eq!(String::from_utf8(out).unwrap(), "  ▣ no window  No Zoom window chosen for Statistics yet: choose it once in the LectureLive app. Screenshots (⌘⇧4) still become slides.\n");
    }

    #[test]
    fn say_paints_its_marks_and_labels() {
        assert_eq!(shown(pipe(ANSI), &Event::NothingNew, &mut None), "  \u{1b}[36m◆\u{1b}[0m \u{1b}[1msnapshot\u{1b}[0m  nothing new since the last one\n");
        assert_eq!(shown(pipe(TRUE), &Event::NothingNew, &mut None), "  \u{1b}[38;2;93;184;192m◆\u{1b}[0m \u{1b}[1msnapshot\u{1b}[0m  nothing new since the last one\n");
        assert_eq!(shown(pipe(ANSI), &Event::SnapshotFailed("the model refused".into()), &mut None), "  \u{1b}[31m▲\u{1b}[0m \u{1b}[1msnapshot failed\u{1b}[0m  the model refused\n");
        assert_eq!(shown(pipe(TRUE), &Event::SnapshotFailed("the model refused".into()), &mut None), "  \u{1b}[38;2;242;118;107m▲\u{1b}[0m \u{1b}[1msnapshot failed\u{1b}[0m  the model refused\n");
    }

    #[test]
    fn dim_and_teal_run_through_the_block_and_busy_lines() {
        assert_eq!(shown(pipe(ANSI), &Event::Busy("polishing 40 words".into()), &mut None), "\u{1b}[2m  … polishing 40 words\u{1b}[0m\n");
        assert_eq!(shown(pipe(TRUE), &Event::Busy("polishing 40 words".into()), &mut None), "\u{1b}[2m  … polishing 40 words\u{1b}[0m\n");
        assert_eq!(shown(pipe(ANSI), &Event::Session(Notification::Stt(SttStatus::Connected)), &mut None), "\u{1b}[2m  transcribing\u{1b}[0m\n");
        let e = Event::Committed { words: 486, slides: 2, block: "<!-- 10:42:03 -->\n## Sampling".into(), usd: 0.02, confirmed: true, removed: 0, missing: 0, revision: 3 };
        assert_eq!(shown(pipe(TRUE), &e, &mut None), "  \u{1b}[38;2;93;184;192m│\u{1b}[0m \u{1b}[2m## Sampling\u{1b}[0m\n  \u{1b}[38;2;93;184;192m◆\u{1b}[0m \u{1b}[1mnotes\u{1b}[0m  486 words and 2 slides folded in  \u{1b}[2m$0.02\u{1b}[0m\n");
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
        print_prepared(&mut out, pipe(OFF), ready, &files(), "Machine Learning", "Week 03 — Optimisation", "BlackHole 2ch");
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
        print_prepared(&mut out, pipe(TRUE), &prepared(InitReport::default()), &files(), "Machine Learning", "Week 03 — Optimisation", "BlackHole 2ch");
        assert_eq!(String::from_utf8(out).unwrap(), "\n  \u{1b}[1mMachine Learning\u{1b}[0m  \u{1b}[2m›\u{1b}[0m  Week 03 — Optimisation\n\u{1b}[2m  listening on BlackHole 2ch\u{1b}[0m\n\u{1b}[2m  ⏎ snapshot   a hint ⏎   polish ⏎   ^C stop\u{1b}[0m\n\n");
    }

    fn ended(report: StopReport, spend: &Spend) -> String {
        let mut out = Vec::new();
        print_end(&mut out, pipe(OFF), &files(), &report, spend);
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
        print_end(&mut out, pipe(TRUE), &files(), &StopReport::default(), &ledger());
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
        say(&mut Broken, pipe(OFF), "notes", "notes", "nothing new");
    }

    #[test]
    #[should_panic(expected = "failed printing to stdout")]
    fn a_failed_show_line_panics_as_println_did() {
        show(&mut Broken, pipe(OFF), &Event::Busy("polishing 40 words".into()), &mut None, &words(), false);
    }

    #[test]
    #[should_panic(expected = "failed printing to stdout")]
    fn a_failed_prepared_line_panics_as_println_did() {
        print_prepared(&mut Broken, pipe(OFF), &prepared(InitReport::default()), &files(), "Machine Learning", "Week 03 — Optimisation", "BlackHole 2ch");
    }

    #[test]
    #[should_panic(expected = "failed printing to stdout")]
    fn a_failed_end_line_panics_as_println_did() {
        print_end(&mut Broken, pipe(OFF), &files(), &StopReport::default(), &ledger());
    }

    const STOPPING: &str = "  ◆ stopping  finishing the transcript and recovery, then a last snapshot (Ctrl-C again stops waiting for recovery)\n";
    const STOP_WAITING: &str = "  ▲ stopping  no longer waiting for recovery or queued requests; its gaps wait for the next session (Ctrl-C again quits at once)\n";
    const QUIT: &str = "  ▲ quit  stopped at once; the next session in this folder picks up what was left\n";

    /// One Ctrl-C through plain's handler: what it printed, how many `Stop`s it sent, and the step.
    fn ctrl_c(controller: &mut StopController) -> (String, usize, Step) {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut out = Vec::new();
        let step = request_stop(&mut out, pipe(OFF), controller, Origin::Signal, &tx).unwrap();
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
        let step = request_stop(&mut std::io::sink(), pipe(OFF), &mut controller, Origin::Timer, &tx).unwrap();
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

    /// One payload carrying every hostile sequence the boundary must neutralise: a CSI colour and
    /// a clear-screen, OSC title (BEL- and ST-terminated), an OSC 52 clipboard write, an OSC 8
    /// hyperlink with its target, BEL, CR and DEL.
    const HOSTILE: &str = "say\x1b[31mthis\x1b[0m\x1b[2J\x1b[H\x1b]0;owned title\x07\x1b]52;c;cGF5bWU\x07\x1b]8;;https://evil.example\x1b\\click\x1b]8;;\x1b\\\x07part\rwholed\x7f";

    /// The visible words that survive the payload above, as `clean` leaves them.
    const HOSTILE_LEFT: &str = "saythisclickpart\nwholed";

    /// Plan §C 12 / Task 12: `NO_COLOR` is not "no safety". On a TTY whose colour is off — exactly
    /// the `NO_COLOR` case — every hostile sequence still goes from every dynamic line plain
    /// prints: transcript text, the recording path, busy text, failure notices, a committed block,
    /// the input uid, a slide's file, capture states, and the start-up and end reports. Not one
    /// executable control, and not one ESC, survives.
    #[test]
    fn plain_tty_cleans_controls_with_no_color() {
        let d = term(OFF);
        let mut watch = None;
        let cases: Vec<Event> = vec![
            Event::Session(Notification::Segment(Segment { id: 0, recording_id: Default::default(), start_sample: 0, end_sample: 1, said_at: segment("live").said_at, start: segment("live").start, end: segment("live").end, text: HOSTILE.into(), words: Vec::new(), source: SegmentSource::Live })),
            Event::Session(Notification::Recording { path: PathBuf::from(format!("/tmp/{}/rec.wav", HOSTILE)) }),
            Event::Busy(HOSTILE.into()),
            Event::SnapshotFailed(HOSTILE.into()),
            Event::PolishFailed(HOSTILE.into()),
            Event::PageFailed(HOSTILE.into()),
            Event::Session(Notification::Failed(HOSTILE.into())),
            Event::Session(Notification::RecoveryFailed(HOSTILE.into())),
            Event::Session(Notification::SpendFailed(HOSTILE.into())),
            Event::Session(Notification::Stt(SttStatus::Retrying { after: Duration::from_secs(2), reason: HOSTILE.into() })),
            Event::Session(Notification::DeviceGone { uid: HOSTILE.into() }),
            Event::Slide { index: 1, file: format!("slides/{HOSTILE}.png").into(), auto: true, uncertain: false, shown_at: chrono::Local::now() },
            Event::Capture(CaptureState::Paused { window: HOSTILE.into(), reason: HOSTILE.into() }),
            Event::CaptureMoved { selection: Selection { descriptor: Descriptor { bundle_id: None, app: HOSTILE.into(), title: HOSTILE.into(), width: 1, height: 1 }, region: Region::WHOLE, leave_out: vec![], sizes: vec![] }, note: HOSTILE.into() },
            Event::Warning(HOSTILE.into()),
            Event::Committed { words: 1, slides: 0, block: format!("<!-- 10:00:00 -->\n{HOSTILE}\n"), usd: 0.0, confirmed: true, removed: 0, missing: 0, revision: 1 },
        ];
        for e in &cases {
            let mut out = Vec::new();
            show(&mut out, d, e, &mut watch, &words(), false);
            let text = String::from_utf8(out).unwrap();
            assert!(!text.contains('\x1b') && !text.contains('\x07') && !text.contains('\r') && !text.contains('\x7f'), "{e:?} left a control: {text:?}");
            assert!(text.contains("saythis") || text.contains("click"), "{e:?} lost the visible words: {text:?}");
        }
        // the start-up report's own names and the end summary's files and failure
        let mut ready = prepared(InitReport::default());
        ready.launch.missing = vec![PathBuf::from(format!("/tmp/lec/{HOSTILE}.wav"))];
        let mut out = Vec::new();
        print_prepared(&mut out, d, &ready, &files(), HOSTILE, HOSTILE, HOSTILE);
        let text = String::from_utf8(out).unwrap();
        assert!(!text.contains('\x1b') && !text.contains('\x07') && !text.contains('\r'), "{text:?}");
        assert!(text.contains(&clean(HOSTILE)), "the names are cleaned, not dropped: {text:?}");
        let mut out = Vec::new();
        print_end(&mut out, d, &files(), &StopReport { last_snapshot: Some(HOSTILE.into()), ..Default::default() }, &ledger());
        let text = String::from_utf8(out).unwrap();
        assert!(!text.contains('\x1b') && !text.contains('\x07') && !text.contains('\r'), "{text:?}");
        assert!(text.contains("the last snapshot failed (saythis"), "{text:?}");
        // the cleaner itself has already proved what a payload reduces to; the boundary agrees
        assert_eq!(clean(HOSTILE), HOSTILE_LEFT);
    }

    /// Cleaning happens before painting, so our own styling survives and the payload's sequences
    /// do not: with colour on, a TTY line still carries LectureLive's escapes (the mark's, the
    /// label's) while every hostile CSI, OSC and control from the event is gone. Were cleaning run
    /// on the painted output, our own sequences would be stripped with the rest.
    #[test]
    fn plain_tty_cleans_untrusted_text_before_painting() {
        let d = term(TRUE);
        let mut out = Vec::new();
        show(&mut out, d, &Event::SnapshotFailed(HOSTILE.into()), &mut None, &words(), false);
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("\u{1b}[38;2;242;118;107m▲\u{1b}[0m \u{1b}[1msnapshot failed\u{1b}[0m"), "our own paint is intact: {text:?}");
        assert!(!text.contains("\x1b[31m") && !text.contains("\x1b[2J") && !text.contains("\x1b]0;") && !text.contains("\x1b]52;") && !text.contains("\x1b]8;"), "the payload's sequences are gone: {text:?}");
        assert!(!text.contains('\x07') && !text.contains('\r') && !text.contains('\x7f'));
        assert!(text.contains("saythis"), "the words it carried stay: {text:?}");
        // the transcript's own line keeps its dim time but never the payload's controls
        let mut out = Vec::new();
        show(&mut out, d, &Event::Session(Notification::Segment(Segment { id: 0, recording_id: Default::default(), start_sample: 0, end_sample: 1, said_at: segment("live").said_at, start: segment("live").start, end: segment("live").end, text: HOSTILE.into(), words: Vec::new(), source: SegmentSource::Live })), &mut None, &words(), false);
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("\u{1b}[2m"), "our own dim time: {text:?}");
        assert!(!text.contains("\x1b[31m") && !text.contains("\x1b]"), "none of the payload's: {text:?}");
    }

    /// A pipe is not a terminal: the same hostile bytes reach it untouched, exactly as the CLI
    /// printed them before Task 12, so ordinary control-free output is byte-identical and the
    /// pipe tests' zero-escape guarantee keeps its meaning (nothing new is injected either).
    #[test]
    fn a_pipe_leaves_the_bytes_as_they_are() {
        let mut out = Vec::new();
        show(&mut out, pipe(OFF), &Event::SnapshotFailed(HOSTILE.into()), &mut None, &words(), false);
        let text = String::from_utf8(out).unwrap();
        assert_eq!(text, format!("  ▲ snapshot failed  {HOSTILE}\n"), "not a terminal: no cleaning, byte for byte");
        // and control-free lines are identical either way
        for e in [Event::NothingNew, Event::SnapshotFailed("timed out".into())] {
            let (mut a, mut b) = (Vec::new(), Vec::new());
            show(&mut a, pipe(OFF), &e, &mut None, &words(), false);
            show(&mut b, term(OFF), &e, &mut None, &words(), false);
            assert_eq!(a, b, "control-free text is the same on a TTY and a pipe");
        }
    }

    /// The shared wording, unstyled: what the TUI's activity and notice line hold, and what `show`
    /// then paints (already pinned byte for byte by the goldens above).
    #[test]
    fn notice_carries_the_plain_words_without_styling() {
        let off = Display { paint: spend::Paint { color: false, truecolor: false }, tty: false };
        let on = Display { paint: spend::Paint { color: true, truecolor: true }, tty: false };
        let w = &words();
        let e = Event::Committed { words: 486, slides: 2, block: "<!-- 10:42:03 -->\n## Sampling".into(), usd: 0.02, confirmed: true, removed: 0, missing: 0, revision: 3 };
        let n = notice(&e, off, w, false).unwrap();
        assert_eq!((n.kind, n.label.as_str(), n.detail.as_str()), ("notes", "notes", "486 words and 2 slides folded in  $0.02"));
        assert!(!n.detail.contains('\x1b'), "the TUI's wording carries no styling");
        assert_eq!(notice(&e, on, w, false).unwrap().detail, "486 words and 2 slides folded in  \x1b[2m$0.02\x1b[0m", "the plain CLI's own paint is unchanged");
        assert_eq!(notice(&Event::Session(Notification::Open { stable: "a".into(), tentative: "b".into() }), off, w, false), None);
        assert_eq!(notice(&Event::Busy("snapshot, 12 words".into()), off, w, false), None, "busy text is shown verbatim, never a notice");
        assert_eq!(notice(&Event::Session(Notification::Stt(SttStatus::Connected)), off, w, false), None);
        let gone = notice(&Event::Session(Notification::DeviceGone { uid: "Gone-UID".into() }), off, w, false).unwrap();
        assert_eq!((gone.kind, gone.label.as_str()), ("warn", "Gone-UID"));
        assert_eq!(gone.detail, "is unplugged. LectureLive waits for it and records nothing meanwhile.", "the offer is the plain CLI's own addition");
        assert_eq!(notice(&Event::Session(Notification::DeviceGone { uid: "Gone-UID".into() }), off, w, true).unwrap().detail, "is unplugged. LectureLive waits for it; Zoom's audio still records meanwhile.");
        // capture states use the terminal's wording, unstyled
        let n = notice(&Event::Capture(CaptureState::Watching { window: "Zoom Meeting".into() }), on, w, false).unwrap();
        assert_eq!((n.kind, n.label.as_str(), n.detail.as_str()), ("slide", "watching", "Zoom Meeting"));
        let n = notice(&Event::Capture(CaptureState::Denied), on, w, false).unwrap();
        assert_eq!(n.detail, "Screen Recording is off for Terminal: System Settings → Privacy & Security → Screen & System Audio Recording, then quit and reopen Terminal.", "no styling and no host guessing");
    }

    /// Task 12's order, proven on the real combination: a colour TTY. The committed, polished and
    /// page lines embed the CLI's own painting, and their untrusted fragments are cleaned as the
    /// detail is built — so on a terminal with colour our own escapes survive (the dim money, the
    /// over-budget red) while a hostile payload's do not.
    #[test]
    fn our_own_paint_survives_tty_cleaning_and_theirs_does_not() {
        let e = Event::Committed { words: 486, slides: 2, block: "<!-- 10:42:03 -->\n## Sampling".into(), usd: 0.02, confirmed: true, removed: 0, missing: 0, revision: 3 };
        assert_eq!(shown(term(TRUE), &e, &mut None), "  \u{1b}[38;2;93;184;192m│\u{1b}[0m \u{1b}[2m## Sampling\u{1b}[0m\n  \u{1b}[38;2;93;184;192m◆\u{1b}[0m \u{1b}[1mnotes\u{1b}[0m  486 words and 2 slides folded in  \u{1b}[2m$0.02\u{1b}[0m\n");
        assert_eq!(shown(term(ANSI), &e, &mut None), "  \u{1b}[36m│\u{1b}[0m \u{1b}[2m## Sampling\u{1b}[0m\n  \u{1b}[36m◆\u{1b}[0m \u{1b}[1mnotes\u{1b}[0m  486 words and 2 slides folded in  \u{1b}[2m$0.02\u{1b}[0m\n", "ANSI colour keeps the dim money too");
        // the page over budget keeps its red length
        let over = Event::Page { outcome: PageOutcome { path: PathBuf::from("/tmp/lec/study_page.html"), words: 1600, budget: 1500, cached: false, missing: vec![] }, usd: 0.03 };
        let line = shown(term(ANSI), &over, &mut None);
        assert!(line.contains("\u{1b}[31m1600 words of 1500 allowed\u{1b}[0m") && line.contains("\u{1b}[2m$0.03\u{1b}[0m"), "{line:?}");
        // the polished backup's name is the folder's own word: cleaned on the TTY, painted after
        // (a slash-free payload, so the path's own file_name sees one component either way)
        let name = "notes_say\x1b[31mthis\x1b]0;owned\x07\x1b]52;c;cGF5\x07click\x07part\rwholed\x7f";
        let hostile = Event::Polished { backup: PathBuf::from(format!("/tmp/lec/.live_notes/{name}.md")), usd: 0.04, revision: 5 };
        let line = shown(term(ANSI), &hostile, &mut None);
        assert!(line.contains("notes_saythisclickpart") && !line.contains("\x1b]"), "the payload's sequences are gone from the name: {line:?}");
        assert!(line.contains("\u{1b}[2m$0.04\u{1b}[0m"), "our own dim money survives it: {line:?}");
        // and control-free painted lines are byte-identical on a TTY and a pipe
        let same = |e: &Event| {
            let (mut a, mut b) = (Vec::new(), Vec::new());
            show(&mut a, pipe(ANSI), e, &mut None, &words(), false);
            show(&mut b, term(ANSI), e, &mut None, &words(), false);
            a == b
        };
        assert!(same(&e) && same(&over), "control-free painted lines do not differ on a TTY");
        assert!(!same(&hostile), "the hostile name is the one thing a TTY cleans");
        assert_eq!(shown(pipe(ANSI), &hostile, &mut None), format!("  \u{1b}[36m✓\u{1b}[0m \u{1b}[1mpolished\u{1b}[0m  previous version in .live_notes/{name}.md  \u{1b}[2m$0.04\u{1b}[0m\n"), "a pipe keeps the bytes as they were");
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
