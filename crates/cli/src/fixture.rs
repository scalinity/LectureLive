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
use lecturelive_core::session::sidecar::Sidecar;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::time::{interval, interval_at, Instant};

#[derive(Clone, Copy, Debug)]
pub(crate) enum Scenario {
    /// Transcribing: a level every 100 ms, a segment each second; each operation commits a block.
    Quiet,
}

impl Scenario {
    pub(crate) fn parse(name: &str) -> Result<Scenario> {
        match name {
            "quiet" => Ok(Scenario::Quiet),
            _ => anyhow::bail!("LECTURELIVE_CLI_FIXTURE names no scenario {name:?}; there is \"quiet\""),
        }
    }
}

/// Runs `scenario` until the first `Stop`: then `SourceEnded`, and the end 200 ms later. Segment ids
/// continue the folder's log and revisions its notes, though nothing is written.
pub(crate) async fn run(scenario: Scenario, files: &LectureFiles, mut commands: UnboundedReceiver<Command>, events: UnboundedSender<Event>) -> Result<StopReport> {
    let Scenario::Quiet = scenario;
    let first = segments::read(&files.segments())?.len() as u64;
    let mut revision = Sidecar::load(&files.sidecar())?.map_or(0, |sc| sc.notes.revision);
    let (mut next, mut words, mut open) = (first, 0, true);
    let _ = events.send(Event::Session(Notification::Stt(SttStatus::Connected)));
    let mut level = interval(Duration::from_millis(100));
    let mut segment = interval_at(Instant::now() + Duration::from_secs(1), Duration::from_secs(1));
    loop {
        tokio::select! {
            _ = level.tick() => { let _ = events.send(Event::Session(Notification::Level(0.05))); }
            _ = segment.tick() => {
                let now = Local::now();
                let text = format!("scripted line {next}");
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
    tokio::time::sleep(Duration::from_millis(200)).await;
    Ok(StopReport { segments: next - first, ..Default::default() })
}
