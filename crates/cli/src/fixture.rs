//! A scripted stand-in for `lecture::run` (M7 plan §C 15, §H): the pipe and terminal tests run the real
//! folder, `prepare`, frontend and stop controller around it, with no audio, key or network. The module is
//! compiled only with debug assertions: a build without them has no fake engine.

use std::time::Duration;

use anyhow::Result;
use chrono::Local;
use lecturelive_core::capture::detect::Region;
use lecturelive_core::capture::select::{Descriptor, Selection, SizedRegion};
use lecturelive_core::capture::window::WindowInfo;
use lecturelive_core::capture::worker::CaptureState;
use lecturelive_core::session::coordinator::{Notification, StopReport, SttStatus};
use lecturelive_core::session::files::LectureFiles;
use lecturelive_core::session::lecture::{Command, Event, Op};
use lecturelive_core::session::segments::{self, Segment, SegmentSource};
use lecturelive_core::session::sidecar::{Gap, GapKind, Sidecar};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::time::{interval, interval_at, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Scenario {
    /// Transcribing: a level every 100 ms, a segment each second; each operation commits a block.
    Quiet,
    /// As quiet, but the transcript is a lecture's: a backlog to scroll from the start, then speech a few
    /// words at a time on the open utterance until each sentence closes, and now and then a recovered
    /// segment while someone is still speaking (plan Task 8's manual check).
    Transcript,
    /// As quiet, but stopping takes as long as core's can: after the first `Stop` the transcript and recovery
    /// drain until a second `Stop`, and the last snapshot then takes a minute. Only stage 3 ends it at once.
    SlowStop,
    /// As quiet, then a second in, a panic that cannot unwind: the release build's `panic = "abort"`, in a dev build.
    Panic,
    /// As quiet; the TUI fails to enter the alternate screen, with raw mode already taken.
    InitFailRaw,
    /// As quiet; the TUI fails to capture the mouse, with raw mode, the alternate screen and paste taken.
    InitFailMouse,
    /// As quiet; the TUI fails to make its drawing surface, with raw mode, the alternate screen, paste
    /// and mouse capture taken.
    InitFail,
    /// As quiet; the TUI's second draw fails.
    DrawFail,
    /// The hint line's check (plan Task 10): notes already committed as rich Markdown, then each
    /// request runs in turn — a snapshot streams its preview and commits; a polish runs its snapshot,
    /// polishes and typesets a page; a snapshot hinted `stall` writes until one `Cancel` — and every
    /// command received is logged, one line each, to `fixture-commands.log` in the lecture folder.
    /// The first stop drains for up to 8 s (a second stop ends it), long enough to try Enter while stopping, then a last snapshot runs.
    Ops,
    /// The slides pane's and Ctrl-S's check (plan Task 11): capture walks its states — unbound,
    /// watching, asking (one window offered, at a size the saved selection remembers), paused,
    /// denied, failing, asking unsafely (a size with no region), found again — while slides are
    /// registered auto, unsettled and manual. `capture-now` and `bind` are logged like `ops`
    /// logs its commands; the selections it would save go to `.live_notes/fixture-capture.json`,
    /// never the person's data folder, and no window server is reached.
    Capture,
    /// The failure table's check (plan Task 12): a non-fatal walk through what can go wrong — a
    /// transcript gap that recovery then fails to fill, a spend-ledger failure, a transcription
    /// reconnect that comes back, a failed snapshot, a failed page — while the lecture stays
    /// alive, so the header, the notice line and the activity overlay can all be inspected. No
    /// API, no audio; stop with Ctrl-C.
    Failures,
    /// A session that dies of its own disk (plan Task 12): the lecture starts normally, says
    /// `Failed` — the notice the frontends word as "session failed" — and the engine then returns
    /// `Err` whose message carries a hostile terminal-control payload after the recognizable
    /// words, so the terminal is restored before the ordinary `Error: …` (cleaned only when
    /// stderr is a terminal) and exit 1.
    SessionFail,
    /// Two hours of history in one moment (plan Task 13): 1,440 closed segments five seconds
    /// apart and the canonical notes document of exactly 6,671 content words, emitted as fast as
    /// the channel takes them, then a marker notice — and after it, quiet until Stop. Nothing is
    /// paced and nothing repeats: the session exists to load long state and then be still.
    TwoHour,
    /// Pathological retained state (plan Task 13): 10,000 closed segments, a notes document of
    /// exactly 100,000 content words and 200 registered slides, then the same quiet until Stop.
    Stress,
    /// A writing burst (plan Task 13): 5,000 preview deltas at 500 a second — one every 2 ms for
    /// ten seconds — with one closed speech segment every 500 ms (20 in all), then one commit so
    /// the preview ends and canonical notes win, then quiet until Stop. Deadline-driven: a late
    /// tick catches up; a delta is never skipped and never added.
    Burst,
}

impl Scenario {
    pub(crate) fn parse(name: &str) -> Result<Scenario> {
        match name {
            "quiet" => Ok(Scenario::Quiet),
            "transcript" => Ok(Scenario::Transcript),
            "slow-stop" => Ok(Scenario::SlowStop),
            "panic" => Ok(Scenario::Panic),
            "init-fail-raw" => Ok(Scenario::InitFailRaw),
            "init-fail-mouse" => Ok(Scenario::InitFailMouse),
            "init-fail" => Ok(Scenario::InitFail),
            "draw-fail" => Ok(Scenario::DrawFail),
            "ops" => Ok(Scenario::Ops),
            "capture" => Ok(Scenario::Capture),
            "failures" => Ok(Scenario::Failures),
            "session-fail" => Ok(Scenario::SessionFail),
            "two-hour" => Ok(Scenario::TwoHour),
            "stress" => Ok(Scenario::Stress),
            "burst" => Ok(Scenario::Burst),
            _ => anyhow::bail!("LECTURELIVE_CLI_FIXTURE names no scenario {name:?}; there are \"quiet\", \"transcript\", \"slow-stop\", \"panic\", \"init-fail-raw\", \"init-fail-mouse\", \"init-fail\", \"draw-fail\", \"ops\", \"capture\", \"failures\", \"session-fail\", \"two-hour\", \"stress\" and \"burst\""),
        }
    }
}

/// Segments already in the lecture when the `transcript` scenario starts: enough to scroll.
const BACKLOG: usize = 60;

/// What the `transcript` scenario's lecturer says, in turn.
const SENTENCES: [&str; 8] = [
    "Right, so where we stopped last time was the sampling distribution of the mean.",
    "Each sample gives a different mean, and those means have a spread of their own.",
    "That spread shrinks as the sample grows, but only with the square root of n.",
    "So to halve the standard error you need four times the data, which is expensive.",
    "Keep that trade-off in mind when we get to the confidence intervals after the break.",
    "A question from the chat: does this assume the population is normal?",
    "Not for the mean with a reasonable sample, and that is the central limit theorem.",
    "Let's check it with the simulation from the lab rather than take my word for it.",
];

/// The `ops` scenario's command log, in the lecture folder the test made: one line per command.
pub(crate) const COMMAND_LOG: &str = "fixture-commands.log";

/// Where the `capture` scenario's selections are kept (plan Task 11): in the lecture folder the
/// test made, never the person's data folder.
pub(crate) const CAPTURE_JSON: &str = "fixture-capture.json";

// ---------------------------------------------------------------------------------------------
// The Task-13 payloads: exact shapes, generated once, shared by the scenarios and their tests.

/// The `two-hour` scenario's exact segment count: 1,440 closed segments, five seconds apart —
/// 1,440 × 5 s = 7,200 s = two hours of history.
pub(crate) const TWO_HOUR_SEGMENTS: u64 = 1_440;
/// The `two-hour` notes document's exact content-word count.
pub(crate) const TWO_HOUR_WORDS: usize = 6_671;
/// The `stress` scenario's exact retained state.
pub(crate) const STRESS_SEGMENTS: u64 = 10_000;
pub(crate) const STRESS_WORDS: usize = 100_000;
pub(crate) const STRESS_SLIDES: u32 = 200;
/// The `burst` scenario's exact shape: 5,000 preview deltas at 500 a second (one every 2 ms, for
/// ten seconds) with one closed speech segment every 500 ms.
pub(crate) const BURST_DELTAS: usize = 5_000;
pub(crate) const BURST_PER_SECOND: u64 = 500;
pub(crate) const BURST_SPEECH: u64 = 20;

/// The deterministic content-word counter for the scripted notes documents (plan Task 13): the
/// words a person reads — whitespace-separated tokens holding at least one alphanumeric — once
/// the timestamp markers are taken out. Markdown punctuation (`##`, `-`, emphasis) carries no
/// content of its own. One definition, shared by the generator and the tests that pin it.
pub(crate) fn content_words(document: &str) -> usize {
    fn in_text(text: &str) -> usize {
        text.split_whitespace().filter(|t| t.chars().any(char::is_alphanumeric)).count()
    }
    let mut words = 0;
    let mut rest = document;
    while let Some(a) = rest.find("<!--") {
        words += in_text(&rest[..a]);
        let marker = &rest[a..];
        match marker.find("-->") {
            Some(b) => rest = &marker[b + 3..],
            None => return words, // an unterminated marker marks nothing readable after it
        }
    }
    words + in_text(rest)
}

/// The word pool the scripted documents draw from: plain, alphanumeric words only, so a tail
/// paragraph built from it counts exactly as many content words as it has words.
const POOL: [&str; 32] = ["the", "standard", "error", "shrinks", "with", "square", "root", "of", "sample", "size", "larger", "samples", "reduce", "quadrupling", "only", "halves", "it", "distinct", "from", "spread", "observations", "sampling", "distributions", "each", "gives", "different", "mean", "means", "their", "own", "trade", "off"];

/// The scripted notes document with exactly `exact` content words (plan Task 13): a title, then
/// timestamp-marked sections — heading, paragraph, nested bullets — whose final piece is a
/// paragraph trimmed to land on the exact count. Deterministic, realistic in shape (never one
/// long line) and valid for the existing Markdown parser.
pub(crate) fn notes_document(exact: usize) -> String {
    let mut doc = String::from("# LectureLive notes\n\n");
    let mut have = content_words(&doc);
    let (mut section, mut cursor) = (0usize, 0usize);
    while have < exact {
        let remaining = exact - have;
        let seconds = (section as u64 * 61) % 86_400;
        let marker = format!("<!-- {:02}:{:02}:{:02} -->", seconds / 3_600, seconds / 60 % 60, seconds % 60);
        let piece = if remaining >= 35 {
            // a whole section: 5 (heading) + 12 (paragraph) + 6 + 6 + 6 (bullets) content words
            format!("{marker}\n## Section {section}: sampling and spread\n\nThe standard error shrinks with the square root of the sample size.\n\n- Larger samples reduce the standard error.\n  - Quadrupling the sample only halves it.\n- Distinct from the spread of observations.\n\n")
        } else {
            // the tail: a paragraph of exactly the words that remain, nothing else
            let words: Vec<&str> = (0..remaining).map(|i| POOL[(cursor + i) % POOL.len()]).collect();
            format!("{marker}\n\n{}\n\n", words.join(" "))
        };
        have += content_words(&piece);
        doc.push_str(&piece);
        cursor += 16;
        section += 1;
    }
    doc
}

/// One scripted backlog line: a sentence or two of lecture-speak, varying in length so the rows
/// it wraps into vary too. The last line says the backlog ends, so it reads as a mark on screen.
fn long_line(k: u64, total: u64) -> String {
    const PHRASES: [&str; 8] = [
        "We distinguish the sample statistic from the population parameter",
        "Increasing the sample size reduces that variability",
        "but only with the square root of n",
        "so to halve the error you need four times the data",
        "keep that trade-off in mind for the confidence intervals",
        "a question from the chat: does this assume a normal population",
        "not for the mean with a reasonable sample, which is the central limit theorem",
        "let us check it with the simulation from the lab",
    ];
    if k + 1 == total {
        return format!("Line {k}: the scripted backlog ends here; the session is now quiet.");
    }
    let mut text = format!("Line {k}: {}.", PHRASES[(k as usize) % PHRASES.len()]);
    if k % 3 == 1 {
        text.push_str(&format!(" {}", PHRASES[((k + 3) as usize) % PHRASES.len()]));
    }
    text
}

/// The long sessions' backlog as one event list (plan Task 13), shared by the scenarios and the
/// in-process tests so both measure the very same payload: the connection, `segments` closed
/// lines five seconds apart (mostly live, every ninety-seventh recovered — history, never
/// breaking the id order), the notes document of exactly `words` content words as one commit,
/// then the registered slides — `index % 3`: auto settled, auto unsettled, manual.
fn backlog_events(segments: u64, words: usize, slides: u32, first: u64, revision: u64) -> Vec<Event> {
    let start = chrono::TimeZone::with_ymd_and_hms(&Local, 2026, 9, 26, 9, 0, 0).unwrap();
    let mut out = Vec::with_capacity(segments as usize + slides as usize + 2);
    out.push(Event::Session(Notification::Stt(SttStatus::Connected)));
    for k in 0..segments {
        let (id, said_at) = (first + k, start + chrono::Duration::seconds(k as i64 * 5));
        let source = if k % 97 == 13 { SegmentSource::Recovered } else { SegmentSource::Live };
        out.push(Event::Session(Notification::Segment(Segment {
            id,
            recording_id: Default::default(),
            start_sample: id * 80_000,
            end_sample: (id + 1) * 80_000,
            said_at,
            start: said_at,
            end: said_at,
            text: long_line(k, segments),
            words: Vec::new(),
            source,
        })));
    }
    out.push(Event::Committed { words, slides: slides as usize, block: format!("\n{}", notes_document(words)), usd: 0.0, confirmed: true, removed: 0, missing: 0, revision: revision + 1 });
    for index in 1..=slides {
        let shown_at = start + chrono::Duration::seconds(index as i64);
        let (auto, uncertain) = match index % 3 {
            0 => (true, false),
            1 => (true, true),
            _ => (false, false),
        };
        let name = shown_at.format("%H%M%S");
        out.push(Event::Slide { index, file: format!("slides/slide_{index:03}_{name}.png"), auto, uncertain, shown_at });
    }
    out
}

/// The `two-hour` scenario's exact backlog (plan Task 13).
pub(crate) fn two_hour_events(first: u64, revision: u64) -> Vec<Event> {
    backlog_events(TWO_HOUR_SEGMENTS, TWO_HOUR_WORDS, 0, first, revision)
}

/// The `stress` scenario's exact retained state (plan Task 13).
pub(crate) fn stress_events(first: u64, revision: u64) -> Vec<Event> {
    backlog_events(STRESS_SEGMENTS, STRESS_WORDS, STRESS_SLIDES, first, revision)
}

/// A command received, as the `ops` and `capture` logs record it: `snapshot<TAB>hint`, `polish`,
/// `cancel`, `stop`, `pause`, `resume`, `quit`, `capture-now`, or a `bind` with the window and enough of the selection to
/// tell which candidate and region were sent.
fn log_command(files: &LectureFiles, c: &Command) {
    use std::io::Write;
    let line = match c {
        Command::Op(Op::Snapshot(hint)) => format!("snapshot\t{hint}"),
        Command::Op(Op::Polish) => "polish".into(),
        Command::Cancel => "cancel".into(),
        Command::Stop => "stop".into(),
        Command::Pause => "pause".into(),
        Command::Resume => "resume".into(),
        Command::Quit => "quit".into(),
        Command::CaptureNow(_) => "capture-now".into(),
        Command::Bind { window, selection } => format!("bind\t{window}\t{}", selection_id(selection)),
        other => format!("{other:?}"),
    };
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(files.dir.join(COMMAND_LOG)) {
        let _ = writeln!(f, "{line}");
    }
}

/// A bound selection's log identity: the saved window's app, title and size, and its region.
fn selection_id(s: &Selection) -> String {
    let d = &s.descriptor;
    let r = &s.region;
    format!("{}|{}|{}|{}×{}|({},{},{},{})", d.bundle_id.as_deref().unwrap_or("-"), d.app, d.title, d.width, d.height, r.x, r.y, r.w, r.h)
}

/// The `capture` scenario's saved window: Zoom's meeting window at 1600 × 900 with its smaller
/// size remembered, as the app would have it after a first lecture. The asking state offers this
/// same window at 1280 × 720, so Ctrl-S's watch-it has a region to reuse.
pub(crate) fn saved_selection() -> Selection {
    Selection {
        descriptor: Descriptor { bundle_id: Some("us.zoom.xos".into()), app: "zoom.us".into(), title: "Zoom Meeting".into(), width: 1600, height: 900 },
        region: Region { x: 0.1, y: 0.1, w: 0.8, h: 0.8 },
        leave_out: vec![],
        sizes: vec![SizedRegion { width: 1280, height: 720, region: Region { x: 0.05, y: 0.12, w: 0.9, h: 0.7 } }],
    }
}

/// One scripted happening of the `capture` scenario's timeline.
enum Cap {
    State(CaptureState),
    Moved { selection: Selection, note: String },
    Slide { auto: bool, uncertain: bool },
}

fn window(id: u32, title: &str, w: u32, h: u32) -> WindowInfo {
    WindowInfo { id, app: "zoom.us".into(), bundle_id: Some("us.zoom.xos".into()), title: title.into(), width: w, height: h, on_screen: true }
}

/// The `capture` scenario's timeline, one step every 1.5 s: each capture state far enough from the
/// last to try Ctrl-S in it, slides registered between, ending healthy after a relocation.
fn capture_step(step: u32) -> Option<Cap> {
    Some(match step {
        1 => Cap::State(CaptureState::Unbound),
        2 => Cap::State(CaptureState::Watching { window: "Zoom Meeting".into() }),
        3 => Cap::Slide { auto: true, uncertain: false },
        4 => Cap::Slide { auto: true, uncertain: true },
        5 => Cap::Slide { auto: false, uncertain: false },
        6 => Cap::State(CaptureState::Asking { window: "Zoom Meeting".into(), reason: "Zoom Meeting is 1280 × 720 now; it was 1600 × 900".into(), candidates: vec![window(42, "Zoom Meeting", 1280, 720)] }),
        7 | 8 => return None, // the asking stays: a moment to press ^S in
        9 => Cap::State(CaptureState::Paused { window: "Zoom Meeting".into(), reason: "not on screen and cannot be captured (minimised?); capture resumes when it is back".into() }),
        10 => Cap::State(CaptureState::Denied),
        11 => Cap::State(CaptureState::Failing { window: "Zoom Meeting".into(), reason: "3 failed captures in a row".into() }),
        12 => Cap::State(CaptureState::Asking { window: "Zoom Meeting".into(), reason: "it is 999 × 600 now and the slide was not found in it; check the region".into(), candidates: vec![window(43, "Zoom Meeting", 999, 600)] }),
        13 => Cap::Moved { selection: saved_selection().with_size(1280, 720, Region { x: 0.05, y: 0.12, w: 0.9, h: 0.7 }), note: "Zoom Meeting is 1280 × 720 again; watching its slide there".into() },
        14 => Cap::State(CaptureState::Watching { window: "Zoom Meeting".into() }),
        15 => Cap::Slide { auto: true, uncertain: false },
        _ => return None,
    })
}

/// The notes a scripted snapshot writes: the shapes the notes pane draws — headings, emphasis, TeX,
/// a nested list, a quote, code, a table and a slide embed — and the hint it was asked to focus on.
fn scripted_notes(n: u32, hint: &str) -> String {
    let focus = if hint.is_empty() { String::new() } else { format!("- Focus asked for: *{hint}*.\n") };
    format!(
        "## Snapshot {n}: sampling and spread\n- The **standard error** shrinks with the sample, as \\( \\sigma / \\sqrt{{n}} \\).\n  - Quadrupling the sample only halves it.\n{focus}\
> The lecturer: keep the histogram from the lab in mind.\n\n```\nse = sd / sqrt(n)\n```\n\n| Term | Meaning |\n|---|---|\n| SE | spread of the mean |\n\n![Slide 1](slides/slide_01_100000.png)\n"
    )
}

/// A request the `ops` scenario is running, and how far it has got.
struct Job {
    op: Op,
    step: u32,
}

/// The release build's abort on panic, in a dev build: a panic cannot unwind out of an `extern "C"` function,
/// so the panic hook runs and the process aborts, with no destructor between.
extern "C" fn panic_without_unwinding() {
    panic!("the scripted panic (LECTURELIVE_CLI_FIXTURE=panic)");
}

/// The long scripted sessions, `two-hour` and `stress` (plan Task 13): the exact retained state
/// above, emitted as fast as the channel takes it — no pacing, so loading is over in a moment —
/// then one marker notice. After the marker the session is genuinely quiet: nothing at all until
/// Stop, so a loaded session's steady-state draw rate and CPU are the reactor's own.
async fn long_session(scenario: Scenario, files: &LectureFiles, mut commands: UnboundedReceiver<Command>, events: UnboundedSender<Event>) -> Result<StopReport> {
    let first = segments::read(&files.segments())?.len() as u64;
    let revision = Sidecar::load(&files.sidecar())?.map_or(0, |sc| sc.notes.revision);
    let (backlog, name) = if scenario == Scenario::TwoHour { (two_hour_events(first, revision), "two-hour") } else { (stress_events(first, revision), "stress") };
    let segments = backlog.iter().filter(|e| matches!(e, Event::Session(Notification::Segment(_)))).count() as u64;
    for e in backlog {
        let _ = events.send(e);
    }
    let _ = events.send(Event::Warning(format!("the scripted {name} session is loaded and now quiet")));
    while let Some(c) = commands.recv().await {
        if matches!(c, Command::Stop) {
            break;
        }
    }
    let _ = events.send(Event::Session(Notification::SourceEnded));
    tokio::time::sleep(Duration::from_millis(200)).await;
    Ok(StopReport { segments, ..Default::default() })
}

/// The `burst` scenario (plan Task 13): 5,000 preview deltas at 500 a second, deadline-driven —
/// each delta waits for its own 2 ms deadline, and a late tick catches up without sleeping rather
/// than skipping or adding anything — with one closed speech segment every 500 ms (20 in all).
/// The burst ends with one commit, so the preview it wrote ends and canonical notes win; then the
/// same quiet as the long sessions. A `Stop` mid-burst ends it at once, its deltas delivered.
async fn burst_session(files: &LectureFiles, mut commands: UnboundedReceiver<Command>, events: UnboundedSender<Event>) -> Result<StopReport> {
    let first = segments::read(&files.segments())?.len() as u64;
    let mut revision = Sidecar::load(&files.sidecar())?.map_or(0, |sc| sc.notes.revision);
    let base = Local::now();
    let _ = events.send(Event::Session(Notification::Stt(SttStatus::Connected)));
    // the active burst begins exactly here: the marker is on disk before the stream's clock starts
    let _ = std::fs::create_dir_all(files.state_dir());
    let _ = std::fs::write(files.state_dir().join(BURST_STARTED), base.to_rfc3339());
    #[cfg(test)]
    burst_boundary(true);
    let t0 = Instant::now();
    let (mut sent, mut stopped) = (0u64, false);
    for delta in 0..BURST_DELTAS {
        let due = t0 + Duration::from_millis(delta as u64 * 1_000 / BURST_PER_SECOND);
        if Instant::now() < due {
            tokio::select! {
                _ = tokio::time::sleep_until(due) => {}
                c = commands.recv() => match c {
                    Some(Command::Stop) | None => { stopped = true; }
                    Some(_) => {}
                }
            }
        }
        if stopped {
            break;
        }
        let _ = events.send(Event::Preview(format!("burst {delta}: more of the notes as they are written, ")));
        sent += 1;
        if sent % (BURST_PER_SECOND / 2) == 0 {
            // every 500 ms of burst: one closed speech segment, ids in order
            let k = sent / (BURST_PER_SECOND / 2);
            let said_at = base + chrono::Duration::milliseconds(500 * (k - 1) as i64);
            let s = Segment { id: first + k - 1, recording_id: Default::default(), start_sample: (first + k - 1) * 16_000, end_sample: (first + k) * 16_000, said_at, start: said_at, end: said_at, text: format!("Burst speech {k} of {BURST_SPEECH}: the notes keep up a clause at a time."), words: Vec::new(), source: SegmentSource::Live };
            let _ = events.send(Event::Session(Notification::Segment(s)));
        }
    }
    // the active burst ends exactly here: its last delta is sent, and everything after this is the
    // quiet post-burst state no measurement of the burst may include
    let _ = std::fs::write(files.state_dir().join(BURST_FINISHED), sent.to_string());
    #[cfg(test)]
    burst_boundary(false);
    revision += 1;
    let _ = events.send(Event::Committed { words: BURST_SPEECH as usize * 10, slides: 0, block: format!("\n<!-- {} -->\n## The burst's notes\n\n- 5,000 deltas folded into one canonical commit.\n", base.format("%H:%M:%S")), usd: 0.01, confirmed: true, removed: 0, missing: 0, revision });
    let _ = events.send(Event::Warning("the scripted burst is complete and now quiet".into()));
    if !stopped {
        while let Some(c) = commands.recv().await {
            if matches!(c, Command::Stop) {
                break;
            }
        }
    }
    let _ = events.send(Event::Session(Notification::SourceEnded));
    tokio::time::sleep(Duration::from_millis(200)).await;
    let speech = sent / (BURST_PER_SECOND / 2); // one closed segment per 500 ms of burst delivered
    Ok(StopReport { segments: speech, ..Default::default() })
}

/// The `burst` scenario's active-interval markers (plan Task 13's remediation): written into the
/// lecture folder the test made — `started` immediately before the 5,000-delta stream begins its
/// clock, `finished` immediately after its last delta has been sent — so a process-level harness
/// can measure exactly the active burst, never the load-in or the quiet after it. Fixture
/// artifacts only: never canonical lecture state, never production data.
pub(crate) const BURST_STARTED: &str = "fixture-burst-started";
pub(crate) const BURST_FINISHED: &str = "fixture-burst-finished";

// The same boundaries for an in-process test (plan Task 13's remediation): recorded where the
// burst itself begins and ends, at the source, so the reactor benchmarks filter their samples
// by the fixture's own active interval instead of positional guesses. Test-only, and never
// compiled into a build without test assertions; a current-thread runtime keeps the fixture and
// its reader on one thread, which is what makes the observer exact rather than a shared global.
#[cfg(test)]
thread_local! {
    pub(crate) static BURST_INTERVAL: std::cell::RefCell<Vec<(bool, std::time::Instant)>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// The burst's boundary as the fixture crossed it: `(began, when)`. Test-only instrumentation:
/// nothing in production reads it, and no measurement depends on it outside tests.
#[cfg(test)]
fn burst_boundary(began: bool) {
    BURST_INTERVAL.with(|b| b.borrow_mut().push((began, std::time::Instant::now())));
}

/// Runs `scenario` until the first `Stop`: then `SourceEnded`, and the end 200 ms later. Segment ids
/// continue the folder's log and revisions its notes, though nothing is written.
pub(crate) async fn run(scenario: Scenario, files: &LectureFiles, mut commands: UnboundedReceiver<Command>, events: UnboundedSender<Event>) -> Result<StopReport> {
    match scenario {
        Scenario::TwoHour | Scenario::Stress => return long_session(scenario, files, commands, events).await,
        Scenario::Burst => return burst_session(files, commands, events).await,
        _ => {}
    }
    let first = segments::read(&files.segments())?.len() as u64;
    let mut revision = Sidecar::load(&files.sidecar())?.map_or(0, |sc| sc.notes.revision);
    let (mut next, mut words, mut open) = (first, 0, true);
    // A quit ends the session without its drain or last snapshot, as core's does.
    let mut quit = false;
    // Scripted ups and downs for the session view (plan Task 6): a transcript gap that recovery
    // resolves, and a transcription reconnect. Only dim presentation comes of these; nothing here
    // writes a file. The `failures` scenario walks its own, longer list (plan Task 12); a session
    // that dies of its disk fails on its own clock below.
    let failing_session = scenario == Scenario::SessionFail;
    let failures = scenario == Scenario::Failures;
    let mut happen = interval_at(Instant::now() + Duration::from_secs(2), Duration::from_secs(if failures { 2 } else { 1 }));
    let mut step = 0u32;
    let mut gapped: Option<Gap> = None;
    let _ = events.send(Event::Session(Notification::Stt(SttStatus::Connected)));
    let lecture = scenario == Scenario::Transcript;
    // The lecture's backlog: an hour of sentences said before this session's first frame.
    let segment_at = |id: u64, said_at, text: String, source| Segment { id, recording_id: Default::default(), start_sample: id * 16_000, end_sample: (id + 1) * 16_000, said_at, start: said_at, end: said_at, text, words: Vec::new(), source };
    if lecture {
        for k in 0..BACKLOG {
            let said_at = Local::now() - chrono::Duration::seconds((BACKLOG - k) as i64 * 7);
            let _ = events.send(Event::Session(Notification::Segment(segment_at(next, said_at, SENTENCES[k % SENTENCES.len()].to_string(), SegmentSource::Live))));
            next += 1;
        }
    }
    // Speech: a word every 300 ms on the open utterance, its last two still tentative, until the
    // sentence closes as a live segment; every fourth sentence, a recovered one lands mid-speech.
    let (mut spoken, mut said) = (0usize, BACKLOG);
    let mut speech = interval_at(Instant::now() + Duration::from_millis(300), Duration::from_millis(300));
    let mut level = interval(Duration::from_millis(100));
    let mut segment = interval_at(Instant::now() + Duration::from_secs(1), Duration::from_secs(1));
    let panic_at = tokio::time::sleep(Duration::from_secs(1));
    tokio::pin!(panic_at);
    // The `session-fail` scenario (plan Task 12): the session dies of its own disk — a notice
    // first, a moment for the frontends to show it, then the engine's own `Err`.
    let disk_at = tokio::time::sleep(Duration::from_secs(3));
    tokio::pin!(disk_at);
    // The `ops` scenario: notes already committed, a slide they embed, and requests run in turn.
    let ops = scenario == Scenario::Ops;
    // The `capture` scenario (plan Task 11): the timeline above, and its own commands.
    let cap = scenario == Scenario::Capture;
    let (mut cap_step, mut watching, mut next_slide): (u32, bool, u32) = (0, false, 0);
    let mut cap_clock = interval_at(Instant::now() + Duration::from_millis(1500), Duration::from_millis(1500));
    let (mut job, mut queued, mut snapshots): (Option<Job>, std::collections::VecDeque<Op>, u32) = (None, Default::default(), 0);
    let mut work = interval(Duration::from_millis(120));
    let mut page_at: Option<Instant> = None;
    if ops {
        let shown_at = Local::now();
        let _ = events.send(Event::Slide { index: 1, file: "slides/slide_01_100000.png".into(), auto: true, uncertain: false, shown_at });
        revision += 1;
        snapshots += 1;
        let block = format!("\n<!-- {} -->\n{}", shown_at.format("%H:%M:%S"), scripted_notes(snapshots, ""));
        let _ = events.send(Event::Committed { words: 120, slides: 1, block, usd: 0.0, confirmed: true, removed: 0, missing: 0, revision });
    }
    loop {
        tokio::select! {
            _ = &mut panic_at, if scenario == Scenario::Panic => panic_without_unwinding(),
            _ = &mut disk_at, if failing_session => {
                let _ = events.send(Event::Session(Notification::Failed("disk full in the scripted session".into())));
                tokio::time::sleep(Duration::from_millis(500)).await; // a moment for the notice to show
                // The engine's own error carries a hostile payload after the recognizable words —
                // Task 12's audit case: the notice is cleaned by the projection, and this string
                // only by the final stderr boundary when stderr is a terminal.
                anyhow::bail!("disk full in the scripted session \x1b[31mcsi\x1b[0m \x1b[2J \x1b[H \x1b]0;owned title\x07 \x1b]52;c;cGF5bWU=\x07 \x1b]8;;https://evil.example\x1b\\link\x1b]8;;\x1b\\ bel\x07 cr\rrewritten del\x7f end");
            }
            _ = work.tick(), if job.is_some() => {
                let Job { op, step } = job.as_mut().expect("a job");
                *step += 1;
                let notes = |n, hint: &str| scripted_notes(n, hint);
                let finished = match op {
                    Op::Snapshot(hint) if hint == "stall" => {
                        if *step == 1 {
                            let _ = events.send(Event::Busy("snapshot, stalled until it is cancelled".into()));
                            let _ = events.send(Event::Preview("Stalled until cancelled: this request waits for Ctrl-X ".into()));
                        }
                        false
                    }
                    Op::Snapshot(hint) => {
                        let text = notes(snapshots + 1, hint);
                        let parts: Vec<&str> = text.split_inclusive(' ').collect();
                        let per = parts.len().div_ceil(6);
                        match *step {
                            1 => { let _ = events.send(Event::Busy(format!("snapshot, {words} words to the script"))); false }
                            2..=7 => {
                                let k = (*step - 2) as usize * per;
                                let _ = events.send(Event::Preview(parts[k.min(parts.len())..(k + per).min(parts.len())].concat()));
                                false
                            }
                            _ => {
                                (revision, snapshots) = (revision + 1, snapshots + 1);
                                let block = format!("\n<!-- {} -->\n{text}", Local::now().format("%H:%M:%S"));
                                let _ = events.send(Event::Committed { words, slides: 0, block, usd: 0.01, confirmed: true, removed: 0, missing: 0, revision });
                                words = 0;
                                true
                            }
                        }
                    }
                    Op::Polish => match *step {
                        1 => { let _ = events.send(Event::Busy("snapshot before the polish".into())); false }
                        2..=4 => { let _ = events.send(Event::Preview(format!("The polish's own snapshot, part {} ", *step - 1))); false }
                        5 => {
                            (revision, snapshots) = (revision + 1, snapshots + 1);
                            let block = format!("\n<!-- {} -->\n{}", Local::now().format("%H:%M:%S"), notes(snapshots, "before the polish"));
                            let _ = events.send(Event::Committed { words, slides: 0, block, usd: 0.01, confirmed: true, removed: 0, missing: 0, revision });
                            let _ = events.send(Event::Busy("polishing the notes with the script".into()));
                            false
                        }
                        6..=14 => false,
                        _ => {
                            revision += 1;
                            let _ = events.send(Event::Polished { backup: files.state_dir().join("notes_before_polish.md"), usd: 0.03, revision });
                            page_at = Some(Instant::now() + Duration::from_secs(3));
                            true
                        }
                    },
                };
                if finished {
                    job = queued.pop_front().map(|op| Job { op, step: 0 });
                }
            }
            _ = tokio::time::sleep_until(page_at.unwrap_or_else(Instant::now)), if page_at.is_some() => {
                page_at = None;
                let outcome = lecturelive_core::notes::page::PageOutcome { path: files.dir.join("lecture_page.html"), words: 900, budget: 1200, cached: false, missing: Vec::new() };
                let _ = events.send(Event::Page { outcome, usd: 0.05 });
            }
            _ = level.tick() => { let _ = events.send(Event::Session(Notification::Level(0.05))); }
            _ = happen.tick() => {
                step += 1;
                if failures {
                    // The failure table's non-fatal walk (plan Task 12): each kind once, in an
                    // order that keeps the notice line moving while the lecture stays alive.
                    match step {
                        1 => { let _ = events.send(Event::Session(Notification::Gap(Gap::new(Default::default(), 32_000, Some(48_000), GapKind::SttOffline)))); }
                        2 => { let _ = events.send(Event::Session(Notification::RecoveryFailed("the recovery server was busy with a backlog; the gap waits for the next session".into()))); }
                        3 => { let _ = events.send(Event::Session(Notification::SpendFailed("the spend ledger could not be written; today's total may be incomplete".into()))); }
                        4 => { let _ = events.send(Event::Session(Notification::Stt(SttStatus::Retrying { after: Duration::from_secs(5), reason: "socket closed by the server".into() }))); }
                        5 => { let _ = events.send(Event::Session(Notification::Stt(SttStatus::Connected))); }
                        6 => { let _ = events.send(Event::SnapshotFailed("the model refused; everything is kept for the next one".into())); }
                        7 => { let _ = events.send(Event::PageFailed("the model refused. The notes are unchanged; `lecture page` tries again.".into())); }
                        _ => {}
                    }
                } else {
                    match step {
                        1 => {
                            let g = Gap::new(Default::default(), 32_000, Some(48_000), GapKind::SttOffline);
                            let _ = events.send(Event::Session(Notification::Gap(g.clone())));
                            gapped = Some(g);
                        }
                        2 => {
                            if let Some(mut g) = gapped.take() {
                                g.resolved = true;
                                let _ = events.send(Event::Session(Notification::Recovered(g)));
                            }
                        }
                        3 => { let _ = events.send(Event::Session(Notification::Stt(SttStatus::Retrying { after: Duration::from_secs(1), reason: "socket closed".into() }))); }
                        4 => { let _ = events.send(Event::Session(Notification::Stt(SttStatus::Connected))); }
                        _ => {}
                    }
                }
            }
            _ = speech.tick(), if lecture => {
                let sentence: Vec<&str> = SENTENCES[said % SENTENCES.len()].split(' ').collect();
                spoken += 1;
                if spoken < sentence.len() {
                    let settled = spoken.saturating_sub(2);
                    let tentative = sentence[settled..spoken].iter().map(|w| format!(" {w}")).collect::<String>();
                    let _ = events.send(Event::Session(Notification::Open { stable: sentence[..settled].join(" "), tentative: if settled == 0 { tentative.trim_start().to_string() } else { tentative } }));
                    if spoken == 3 && said % 4 == 0 {
                        let earlier = Local::now() - chrono::Duration::seconds(95);
                        let _ = events.send(Event::Session(Notification::Segment(segment_at(next, earlier, "Words from the minute the connection dropped, recovered from the recording.".into(), SegmentSource::Recovered))));
                        next += 1;
                    }
                } else {
                    let text = sentence.join(" ");
                    words += sentence.len();
                    let _ = events.send(Event::Session(Notification::Segment(segment_at(next, Local::now(), text, SegmentSource::Live))));
                    (next, said, spoken) = (next + 1, said + 1, 0);
                }
            }
            _ = segment.tick(), if !lecture => {
                let now = Local::now();
                let text = format!("scripted line {next}");
                // the open utterance the line is about to finalise, as the live display opens and closes it
                let _ = events.send(Event::Session(Notification::Open { stable: "scripted".into(), tentative: format!(" line {next}") }));
                words += text.split_whitespace().count();
                let s = Segment { id: next, recording_id: Default::default(), start_sample: next * 16_000, end_sample: (next + 1) * 16_000, said_at: now, start: now, end: now, text, words: Vec::new(), source: SegmentSource::Live };
                let _ = events.send(Event::Session(Notification::Segment(s)));
                next += 1;
            }
            _ = cap_clock.tick(), if cap => {
                cap_step += 1;
                match capture_step(cap_step) {
                    Some(Cap::State(s)) => {
                        watching = matches!(s, CaptureState::Watching { .. });
                        let _ = events.send(Event::Capture(s));
                    }
                    Some(Cap::Moved { selection, note }) => {
                        watching = true; // core rebinds as it relocates
                        let _ = events.send(Event::CaptureMoved { selection, note });
                    }
                    Some(Cap::Slide { auto, uncertain }) => {
                        next_slide += 1;
                        let _ = events.send(Event::Slide { index: next_slide, file: format!("slides/slide_{next_slide:02}_{}.png", Local::now().format("%H%M%S")), auto, uncertain, shown_at: Local::now() });
                    }
                    None => {}
                }
            }
            c = commands.recv(), if open => match c {
                // Pause and resume, as core answers them (spec §9.6): its event, and the log line where a log is kept.
                Some(c @ (Command::Pause | Command::Resume)) => {
                    if ops || cap {
                        log_command(files, &c);
                    }
                    let _ = events.send(Event::Paused(matches!(c, Command::Pause)));
                }
                // Quit, as core takes it: the session ends at once, with no drain and no last snapshot.
                Some(Command::Quit) => {
                    if ops || cap {
                        log_command(files, &Command::Quit);
                    }
                    quit = true;
                    break;
                }
                Some(c) if (ops || cap) && !matches!(c, Command::Stop) => {
                    log_command(files, &c);
                    match c {
                        Command::Op(op) if job.is_none() => job = Some(Job { op, step: 0 }),
                        Command::Op(op) => queued.push_back(op),
                        // as core: what runs stops with its own event, and what waits is dropped silently
                        Command::Cancel => {
                            if let Some(j) = job.take() {
                                let what = if j.op == Op::Polish { "the polish" } else { "the snapshot" };
                                let _ = events.send(Event::Cancelled(what.into()));
                            }
                            queued.clear();
                        }
                        // A manual capture, as core's worker answers it: the reply says whether the
                        // frame was taken, and the slide itself arrives as its own canonical event —
                        // never fabricated from the reply. Its file name says how it was taken.
                        Command::CaptureNow(reply) => {
                            if watching {
                                let _ = reply.send(Ok(()));
                                next_slide += 1;
                                let _ = events.send(Event::Slide { index: next_slide, file: format!("slides/captured-{next_slide}_{}.png", Local::now().format("%H%M%S")), auto: false, uncertain: false, shown_at: Local::now() });
                            } else {
                                let _ = reply.send(Err("no window is being watched: choose it once in the LectureLive app".into()));
                            }
                        }
                        // The person's Ctrl-S on a safe offering: watched from this moment.
                        Command::Bind { selection, .. } => {
                            watching = true;
                            let _ = events.send(Event::Capture(CaptureState::Watching { window: selection.descriptor.label() }));
                        }
                        _ => {}
                    }
                }
                Some(Command::Op(op)) => {
                    let (busy, what, detail) = match op {
                        Op::Snapshot(hint) => (format!("snapshot, {words} words to the script"), "snapshot", if hint.is_empty() { "no focus hint".to_string() } else { format!("focus: {hint}") }),
                        Op::Polish => ("polishing the notes with the script".to_string(), "polish", "the notes as they are".to_string()),
                    };
                    let _ = events.send(Event::Busy(busy));
                    for k in 0..5 {
                        let _ = events.send(Event::Preview(format!("part {k} ")));
                    }
                    revision += 1;
                    let block = format!("<!-- {} -->\n## Scripted {what}\n- {detail}", Local::now().format("%H:%M:%S"));
                    let _ = events.send(Event::Committed { words, slides: 0, block, usd: 0.0, confirmed: true, removed: 0, missing: 0, revision });
                    words = 0;
                }
                Some(Command::Stop) => {
                    if ops || cap {
                        log_command(files, &Command::Stop);
                    }
                    break;
                }
                Some(_) => {}
                None => open = false, // input closed: the session runs until it is stopped
            },
        }
    }
    let _ = events.send(Event::Session(Notification::SourceEnded));
    if ops && !quit {
        // The drain, until a second stop or 8 s; then the last snapshot, which writes a while. A quit during it ends it.
        if let Ok(Some(c)) = tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                match commands.recv().await {
                    Some(c) if matches!(c, Command::Stop | Command::Quit) => return Some(c),
                    Some(c) => log_command(files, &c),
                    None => return None,
                }
            }
        })
        .await
        {
            quit = matches!(c, Command::Quit);
            log_command(files, &c);
        }
    }
    if ops && !quit {
        let _ = events.send(Event::Busy("snapshot, the last of the lecture".into()));
        for part in ["## What was left\n", "- The last words of the ", "lecture, folded in ", "by the last snapshot. "] {
            let _ = events.send(Event::Preview(part.into()));
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        revision += 1;
        let block = format!("\n<!-- {} -->\n## What was left\n- The last words of the lecture, folded in by the last snapshot.\n", Local::now().format("%H:%M:%S"));
        let _ = events.send(Event::Committed { words, slides: 0, block, usd: 0.0, confirmed: true, removed: 0, missing: 0, revision });
    }
    if scenario == Scenario::SlowStop && !quit {
        // The drain, until core has counted a second stop; then the last snapshot, which no stop cuts short — but a quit does.
        while let Some(c) = commands.recv().await {
            if matches!(c, Command::Stop | Command::Quit) {
                quit = matches!(c, Command::Quit);
                break;
            }
        }
        if !quit {
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    Ok(StopReport { segments: next - first, ..Default::default() })
}

/// The Task-13 payloads' exact shapes, measured on the scenarios themselves (plan Task 13): the
/// counts are the contract, and the tests pin them against the same generators the scenarios run.
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use tokio::sync::mpsc::unbounded_channel;
    use crate::fixture;

    fn folder() -> (tempfile::TempDir, LectureFiles) {
        let dir = tempfile::tempdir().unwrap();
        let files = LectureFiles::standard(dir.path(), chrono::NaiveDate::from_ymd_opt(2026, 9, 26).unwrap());
        std::fs::create_dir_all(files.state_dir()).unwrap();
        (dir, files)
    }

    /// Runs `scenario` until its marker notice names it loaded, then stops it: every event it
    /// sent, each with when it arrived, and the report it returned.
    async fn script(scenario: Scenario) -> (Vec<(Event, std::time::Instant)>, Result<StopReport>) {
        let (dir, files) = folder();
        let (ev_tx, mut ev_rx) = unbounded_channel();
        let (cmd_tx, cmd_rx) = unbounded_channel();
        let run = tokio::spawn({
            let files = files.clone();
            async move { fixture::run(scenario, &files, cmd_rx, ev_tx).await }
        });
        let mut out = Vec::new();
        let deadline = tokio::time::sleep(Duration::from_secs(60)); // the burst alone needs >10 s
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                e = ev_rx.recv() => match e {
                    Some(e) => {
                        let done = matches!(&e, Event::Warning(w) if w.contains("now quiet"));
                        out.push((e, std::time::Instant::now()));
                        if done {
                            cmd_tx.send(Command::Stop).unwrap();
                            break;
                        }
                    }
                    None => break,
                },
                _ = &mut deadline => panic!("{scenario:?} never finished after {} events", out.len()),
            }
        }
        let report = run.await.unwrap();
        while let Ok(e) = ev_rx.try_recv() {
            out.push((e, std::time::Instant::now()));
        }
        drop(dir);
        (out, report)
    }

    fn segments_of(events: &[(Event, std::time::Instant)]) -> Vec<&Segment> {
        events.iter().map(|(e, _)| e).filter_map(|e| match e {
            Event::Session(Notification::Segment(s)) => Some(s),
            _ => None,
        }).collect()
    }

    /// The counter's own definition, first: markers and Markdown punctuation are not content, and
    /// the generator lands on its exact counts — the two Task-13 documents included.
    #[test]
    fn the_content_word_counter_counts_words_not_marks() {
        assert_eq!(content_words("<!-- 09:00:00 -->\n## A\n\n- one two.\n"), 3);
        assert_eq!(content_words("# Title\n\nSome *emphasised* text.\n"), 4);
        assert_eq!(content_words(""), 0);
        assert_eq!(content_words(&notes_document(TWO_HOUR_WORDS)), TWO_HOUR_WORDS, "the two-hour document is exactly 6,671 content words");
        assert_eq!(content_words(&notes_document(STRESS_WORDS)), STRESS_WORDS, "the stress document is exactly 100,000");
    }

    #[tokio::test]
    async fn two_hour_fixture_has_exact_shape() {
        let (events, report) = script(Scenario::TwoHour).await;
        let segments = segments_of(&events);
        assert_eq!(segments.len(), TWO_HOUR_SEGMENTS as usize, "1,440 closed segments");
        assert_eq!(segments.iter().map(|s| s.id).collect::<Vec<_>>(), (0..TWO_HOUR_SEGMENTS).collect::<Vec<_>>(), "ids 0..=1439, contiguous and in order");
        assert!(segments.iter().any(|s| s.source == SegmentSource::Recovered), "an occasional recovered line");
        assert_eq!(segments.iter().map(|s| (s.end_sample - s.start_sample) / 16_000).collect::<HashSet<_>>(), [5].into_iter().collect::<HashSet<_>>(), "five-second spacing: 1,440 × 5 s = two hours");
        let committed: Vec<&Event> = events.iter().map(|(e, _)| e).filter(|e| matches!(e, Event::Committed { .. })).collect();
        assert_eq!(committed.len(), 1);
        match committed[0] {
            Event::Committed { words, block, revision, .. } => {
                assert_eq!((*words, *revision), (TWO_HOUR_WORDS, 1), "the commit says its own word count, at the next revision");
                assert_eq!(content_words(block), TWO_HOUR_WORDS, "the document is exactly 6,671 content words");
            }
            _ => unreachable!(),
        }
        assert!(!events.iter().any(|(e, _)| matches!(e, Event::Slide { .. })), "no slides in the two-hour session");
        assert!(matches!(events.last().map(|(e, _)| e), Some(Event::Session(Notification::SourceEnded))), "it stays alive until Stop");
        assert_eq!(report.unwrap().segments, TWO_HOUR_SEGMENTS);
    }

    #[tokio::test]
    async fn stress_fixture_has_exact_shape() {
        let (events, report) = script(Scenario::Stress).await;
        let segments = segments_of(&events);
        assert_eq!(segments.len(), STRESS_SEGMENTS as usize, "10,000 closed segments");
        assert_eq!(segments.iter().map(|s| s.id).collect::<Vec<_>>(), (0..STRESS_SEGMENTS).collect::<Vec<_>>(), "ids 0..=9,999, contiguous and in order");
        let committed: Vec<&Event> = events.iter().map(|(e, _)| e).filter(|e| matches!(e, Event::Committed { .. })).collect();
        assert_eq!(committed.len(), 1);
        match committed[0] {
            Event::Committed { words, block, revision, .. } => {
                assert_eq!((*words, *revision), (STRESS_WORDS, 1));
                assert_eq!(content_words(block), STRESS_WORDS, "exactly 100,000 content words");
            }
            _ => unreachable!(),
        }
        let slides: Vec<&Event> = events.iter().map(|(e, _)| e).filter(|e| matches!(e, Event::Slide { .. })).collect();
        assert_eq!(slides.len(), STRESS_SLIDES as usize, "200 registered slides");
        assert_eq!(
            slides.iter().map(|e| match e { Event::Slide { index, .. } => *index, _ => 0 }).collect::<Vec<_>>(),
            (1..=STRESS_SLIDES).collect::<Vec<_>>(),
            "indices 1..=200"
        );
        for e in &slides {
            match e {
                // the representative mix, on the index % 3 cycle: auto settled / auto unsettled / manual
                Event::Slide { index, auto, uncertain, .. } => assert_eq!((*auto, *uncertain), (index % 3 != 2, index % 3 == 1), "slide {index}"),
                _ => unreachable!(),
            }
        }
        assert!(matches!(events.last().map(|(e, _)| e), Some(Event::Session(Notification::SourceEnded))));
        assert_eq!(report.unwrap().segments, STRESS_SEGMENTS);
    }

    /// The burst's exact shape and its real 500/s pacing (plan Task 13): 5,000 deltas, never
    /// skipped and never duplicated however the scheduler behaved; 20 speech segments; one
    /// terminal commit after the last delta; and the ten seconds the schedule demands.
    #[tokio::test]
    async fn burst_fixture_has_exact_shape() {
        let (events, report) = script(Scenario::Burst).await;
        let deltas: Vec<&String> = events.iter().filter_map(|(e, _)| match e { Event::Preview(d) => Some(d), _ => None }).collect();
        assert_eq!(deltas.len(), BURST_DELTAS, "exactly 5,000 deltas");
        assert_eq!(
            deltas.iter().map(|d| d.trim_start_matches("burst ").split(':').next().unwrap().parse::<u64>().unwrap()).collect::<Vec<_>>(),
            (0..BURST_DELTAS as u64).collect::<Vec<_>>(),
            "in order, one of each: none skipped, none added"
        );
        let segments = segments_of(&events);
        assert_eq!(segments.len(), BURST_SPEECH as usize, "20 closed speech segments");
        assert_eq!(segments.iter().map(|s| s.id).collect::<Vec<_>>(), (0..BURST_SPEECH).collect::<Vec<_>>(), "deterministic sequential ids");
        // the terminal event: the last preview is followed by the commit that ends it
        let last_preview = events.iter().rposition(|(e, _)| matches!(e, Event::Preview(_))).unwrap();
        let commit = events.iter().rposition(|(e, _)| matches!(e, Event::Committed { .. })).unwrap();
        assert!(commit > last_preview, "the commit ends the preview");
        match &events[commit].0 {
            Event::Committed { revision, .. } => assert_eq!(*revision, 1, "canonical notes win at revision 1"),
            _ => unreachable!(),
        }
        // the real rate: the deltas' schedule is 2 ms apart, so 5,000 of them span ten seconds —
        // generous bounds, for a machine that briefly fell behind and caught up
        let first = events.iter().find(|(e, _)| matches!(e, Event::Preview(_))).unwrap().1;
        let last = events.iter().rposition(|(e, _)| matches!(e, Event::Preview(_))).unwrap();
        let span = events[last].1 - first;
        assert!(span >= std::time::Duration::from_secs(8) && span <= std::time::Duration::from_secs(15), "the burst spanned {span:?} (500/s for 10 s)");
        assert_eq!(report.unwrap().segments, BURST_SPEECH);
    }
}
