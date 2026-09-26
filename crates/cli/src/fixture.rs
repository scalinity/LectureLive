//! A scripted stand-in for `lecture::run` (M7 plan §C 15, §H): the pipe and terminal tests run the real
//! folder, `prepare`, frontend and stop controller around it, with no audio, key or network. The module is
//! compiled only with debug assertions: a build without them has no fake engine.

use std::time::Duration;

use anyhow::Result;
use chrono::Local;
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
            _ => anyhow::bail!("LECTURELIVE_CLI_FIXTURE names no scenario {name:?}; there are \"quiet\", \"transcript\", \"slow-stop\", \"panic\", \"init-fail-raw\", \"init-fail-mouse\", \"init-fail\" and \"draw-fail\""),
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

/// The release build's abort on panic, in a dev build: a panic cannot unwind out of an `extern "C"` function,
/// so the panic hook runs and the process aborts, with no destructor between.
extern "C" fn panic_without_unwinding() {
    panic!("the scripted panic (LECTURELIVE_CLI_FIXTURE=panic)");
}

/// Runs `scenario` until the first `Stop`: then `SourceEnded`, and the end 200 ms later. Segment ids
/// continue the folder's log and revisions its notes, though nothing is written.
pub(crate) async fn run(scenario: Scenario, files: &LectureFiles, mut commands: UnboundedReceiver<Command>, events: UnboundedSender<Event>) -> Result<StopReport> {
    let first = segments::read(&files.segments())?.len() as u64;
    let mut revision = Sidecar::load(&files.sidecar())?.map_or(0, |sc| sc.notes.revision);
    let (mut next, mut words, mut open) = (first, 0, true);
    // Scripted ups and downs for the session view (plan Task 6): a transcript gap that recovery
    // resolves, and a transcription reconnect. Only dim presentation comes of these; nothing here
    // writes a file.
    let mut happen = interval_at(Instant::now() + Duration::from_secs(2), Duration::from_secs(1));
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
    loop {
        tokio::select! {
            _ = &mut panic_at, if scenario == Scenario::Panic => panic_without_unwinding(),
            _ = level.tick() => { let _ = events.send(Event::Session(Notification::Level(0.05))); }
            _ = happen.tick() => {
                step += 1;
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
            c = commands.recv(), if open => match c {
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
                Some(Command::Stop) => break,
                Some(_) => {}
                None => open = false, // input closed: the session runs until it is stopped
            },
        }
    }
    let _ = events.send(Event::Session(Notification::SourceEnded));
    if scenario == Scenario::SlowStop {
        // The drain, until core has counted a second stop; then the last snapshot, which no stop cuts short.
        while let Some(c) = commands.recv().await {
            if matches!(c, Command::Stop) {
                break;
            }
        }
        tokio::time::sleep(Duration::from_secs(60)).await;
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    Ok(StopReport { segments: next - first, ..Default::default() })
}
