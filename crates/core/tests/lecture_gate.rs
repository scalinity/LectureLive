//! The M3 gate (docs/milestones.md): a whole lecture headless, from first word to study page, against
//! fake STT, REST and chat endpoints; failed and truncated answers leave the notes untouched; a legacy
//! folder migrates and its pending lines reach the next snapshot. No network.
mod support;

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Local;
use lecturelive_core::notes::chat::{ChatClient, ChatConfig};
use lecturelive_core::notes::embeds::embeds_in;
use lecturelive_core::session::coordinator::{SessionConfig, Store};
use lecturelive_core::session::files::LectureFiles;
use lecturelive_core::session::folder::{self, How};
use lecturelive_core::session::lecture::{self, Command, Event, Lecture, Op, SlideWatch};
use lecturelive_core::session::segments::{self, NewSegment, SegmentLog, SegmentSource};
use lecturelive_core::session::sidecar::Sidecar;
use lecturelive_core::session::spend::{self, Spend};
use lecturelive_core::stt::rest::{spawn_recovery, RestClient, RestConfig};
use lecturelive_core::stt::stream::{self, SttConfig};
use serde_json::Value;
use support::fake_sse::{self, Reply};
use support::sources::Talking;
use support::{fake_rest, fake_stt};
use tokio::sync::mpsc;

const TITLE: &str = "# Machine Learning — Week 01 — Optimisation — 2026-09-25";
const NAME: &str = "Week 01 — Optimisation";

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn files(dir: &Path) -> LectureFiles {
    LectureFiles::standard(dir, chrono::NaiveDate::from_ymd_opt(2026, 9, 25).unwrap())
}

fn user_text(body: &Value) -> String {
    let c = &body["messages"][1]["content"];
    c.as_str().map(str::to_string).unwrap_or_else(|| c[0]["text"].as_str().unwrap_or_default().to_string())
}

/// The model, played by the fake: notes that place every embed, a polish that keeps them, a page within budget.
fn respond(body: &Value) -> Reply {
    let system = body["messages"][0]["content"].as_str().unwrap_or_default();
    let user = user_text(body);
    if system.starts_with("You are the note-taker") {
        let embeds: Vec<&str> = user.lines().filter(|l| l.starts_with("![Slide ")).collect();
        fake_sse::answer(&format!("## Gradient descent\n- Steps against the gradient.\n{}", embeds.join("\n")), 1_000_000)
    } else if system.starts_with("You turn raw") {
        let title = user.lines().next().unwrap_or_default().trim_start_matches("Use this exact title line: ").to_string();
        let doc = user.split("Notes as written during the lecture:\n<<<\n").nth(1).and_then(|s| s.split("\n>>>").next()).unwrap_or_default();
        fake_sse::answer(&format!("{title}\n\nThe lecture covered gradient descent.\n\n## Gradient descent\n- Steps against the gradient.\n{}\n\n## Key takeaways\n- Scale the rate.", embeds_in(doc).join("\n")), 2_000_000)
    } else {
        fake_sse::answer(&fake_sse::study_page(300), 3_000_000)
    }
}

fn chat(url: &str, ledger: &Path) -> ChatClient {
    let spend = Spend::open(ledger, "Machine Learning", NAME, chrono::NaiveDate::from_ymd_opt(2026, 9, 25).unwrap()).unwrap();
    ChatClient::new(ChatConfig { url: url.into(), idle_timeout: ms(5_000), ..ChatConfig::new("test-key".into()) }, Some(spend)).unwrap()
}

fn lecture_for(files: &LectureFiles, url: &str, ledger: &Path) -> Lecture {
    let spend = Spend::open(ledger, "Machine Learning", NAME, files.date).unwrap();
    Lecture { files: files.clone(), course: "Machine Learning".into(), name: NAME.into(), title: TITLE.into(), chat: chat(url, ledger), spend }
}

/// Waits for an event matching `want`, panicking on a failure event or after 30 s.
async fn until(events: &mut mpsc::UnboundedReceiver<Event>, what: &str, want: impl Fn(&Event) -> bool) -> Event {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let e = tokio::time::timeout_at(deadline, events.recv()).await.unwrap_or_else(|_| panic!("no {what} within 30 s")).expect("the lecture is running");
        if let Event::SnapshotFailed(m) | Event::PolishFailed(m) | Event::PolishStopped(m) | Event::PageFailed(m) = &e {
            panic!("waiting for {what}: {m}");
        }
        if want(&e) {
            return e;
        }
    }
}

async fn segments_seen(events: &mut mpsc::UnboundedReceiver<Event>, n: usize) {
    for _ in 0..n {
        until(events, "a segment", |e| matches!(e, Event::Session(lecturelive_core::session::coordinator::Notification::Segment(_)))).await;
    }
}

#[tokio::test]
async fn a_whole_lecture_runs_headless_from_first_word_to_study_page() {
    let dir = tempfile::tempdir().unwrap();
    let f = files(dir.path());
    let ledger = dir.path().join("spend.jsonl");
    let (_, report) = folder::open(&f, TITLE, false).unwrap();
    assert_eq!(report.how, How::Created);
    segments::session_marker(&f.transcript, Local::now()).unwrap();

    let stt = fake_stt::start(fake_stt::Config::default()).await;
    let rest = fake_rest::start(None).await;
    let sse = fake_sse::start(respond).await;
    let stt_cfg = SttConfig { url: stt.url.clone(), backoff_unit: ms(1), connect_timeout: ms(2_000), send_timeout: ms(2_000), idle_timeout: ms(2_000), finalize_wait: ms(2_000), done_wait: ms(2_000), ..SttConfig::new("test-key".into(), vec![]) };
    let rest_cfg = RestConfig { url: rest.url.clone(), retry_unit: ms(1), request_timeout: ms(5_000), file_wait: ms(5_000), ..RestConfig::new("test-key".into(), vec![]) };
    let lec = Arc::new(lecture_for(&f, &sse.url, &ledger));
    let session = SessionConfig {
        dir: f.dir.clone(),
        stem: f.stem.clone(),
        stt: Some(stream::spawn(stt_cfg).unwrap()),
        recovery: Some(spawn_recovery(RestClient::new(rest_cfg).unwrap())),
        spend: Some(lec.spend.clone()),
        ..Default::default()
    };
    let (cmd, cmd_rx) = mpsc::unbounded_channel();
    let (ev_tx, mut ev) = mpsc::unbounded_channel();
    let source = Talking { pace: ms(2), fake: stt.state.clone() };
    let run = tokio::spawn(lecture::run(lec.clone(), session, Box::new(source), SlideWatch { screenshots: None, poll: ms(20) }, cmd_rx, ev_tx));

    segments_seen(&mut ev, 2).await;
    support::slides::png(&f.slides.join("Screenshot dropped in.png"));
    until(&mut ev, "the slide", |e| matches!(e, Event::Slide { index: 1, .. })).await;
    cmd.send(Command::Op(Op::Snapshot(String::new()))).unwrap();
    until(&mut ev, "the first snapshot", |e| matches!(e, Event::Committed { slides: 1, .. })).await;
    segments_seen(&mut ev, 1).await;
    cmd.send(Command::Op(Op::Snapshot("focus on momentum".into()))).unwrap();
    until(&mut ev, "the hinted snapshot", |e| matches!(e, Event::Committed { .. })).await;
    cmd.send(Command::Op(Op::Polish)).unwrap();
    until(&mut ev, "the polish", |e| matches!(e, Event::Polished { .. })).await;
    let page = until(&mut ev, "the page", |e| matches!(e, Event::Page { .. })).await;
    cmd.send(Command::Stop).unwrap();
    let report = tokio::time::timeout(Duration::from_secs(30), run).await.expect("the lecture stops").unwrap().unwrap();

    assert_eq!(report.unresolved, 0);
    let notes = std::fs::read_to_string(&f.notes).unwrap();
    assert!(notes.starts_with(TITLE), "{notes}");
    assert_eq!(notes.matches("![Slide 1](slides/slide_01_").count(), 1, "{notes}");
    let backups: Vec<String> = std::fs::read_dir(f.state_dir()).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.starts_with("lecture_notes_20260925_") && n.ends_with(".md")).collect();
    assert_eq!(backups.len(), 1);
    let raw = std::fs::read_to_string(f.state_dir().join(&backups[0])).unwrap();
    assert!(raw.matches("<!-- ").count() >= 2, "the two snapshots, and the one polish takes first when speech arrived since: {raw}");
    let sc = Sidecar::load(&f.sidecar()).unwrap().unwrap();
    assert_eq!(sc.notes.segment_cursor, segments::read(&f.segments()).unwrap().len() as u64, "the last snapshot took everything");
    assert_eq!(sc.notes.slide_index, 1);
    assert!(!f.journal().exists());
    assert!(std::fs::read_to_string(&f.transcript).unwrap().starts_with("--- started "));
    let Event::Page { outcome, .. } = page else { unreachable!() };
    assert_eq!(outcome.path, dir.path().join("Optimisation.html"));
    assert!(std::fs::read_to_string(&outcome.path).unwrap().contains("<title>Optimisation — Machine Learning</title>"));
    let kinds: Vec<String> = spend::read(&ledger).unwrap().into_iter().map(|e| e.what).collect();
    for (kind, at_least) in [("transcribe", 1), ("notes", 2), ("polish", 1), ("page", 1)] {
        assert!(kinds.iter().filter(|k| *k == kind).count() >= at_least, "{kind}: {kinds:?}");
    }
    let bodies = sse.state.bodies();
    assert!(bodies.iter().any(|b| user_text(b).contains("Focus hint from the student: focus on momentum")));
    let first = &bodies[0]["messages"][1]["content"];
    assert!(first[1]["image_url"]["url"].as_str().unwrap().starts_with("data:image/png;base64,"), "the slide goes with its snapshot");
    assert!(user_text(&bodies[0]).contains(">>> Slide 1 shown (embed: ![Slide 1](slides/slide_01_"));
}

/// A folder with two logged segments and a notes file, for snapshots without a session.
fn offline_folder(dir: &Path) -> (LectureFiles, Store) {
    let f = files(dir);
    folder::open(&f, TITLE, false).unwrap();
    let mut log = SegmentLog::open(dir, &f.stem).unwrap();
    let anchor = chrono::TimeZone::with_ymd_and_hms(&Local, 2026, 9, 25, 10, 0, 0).unwrap();
    for (k, text) in ["Gradient descent steps downhill.", "The rate sets the step."].iter().enumerate() {
        log.append(NewSegment { recording_id: uuid::Uuid::nil(), start_sample: k as u64 * 32_000, end_sample: k as u64 * 32_000 + 16_000, text: text.to_string(), words: vec![], source: SegmentSource::Live }, anchor).unwrap();
    }
    (f.clone(), Store::offline(Sidecar::load(&f.sidecar()).unwrap().unwrap(), f.sidecar()))
}

#[tokio::test]
async fn failed_empty_and_truncated_answers_leave_the_notes_untouched_and_the_batch_pending() {
    let dir = tempfile::tempdir().unwrap();
    let (f, store) = offline_folder(dir.path());
    let replies = Arc::new(Mutex::new(vec![
        Reply::Stream(fake_sse::pieces(fake_sse::fixture("stream_usage.sse")[..9_000].to_vec(), 64)), // cut off mid-answer
        Reply::Stream(vec![b"data: [DONE]\n\n".to_vec()]),                                            // empty
        Reply::Stream(fake_sse::pieces(fake_sse::fixture("stream_length.sse"), 64)),                  // truncated by length
    ]));
    let queue = replies.clone();
    let sse = fake_sse::start(move |b| queue.lock().unwrap().pop().map_or_else(|| respond(b), |r| r)).await;
    let lec = lecture_for(&f, &sse.url, &dir.path().join("spend.jsonl"));
    let (ev_tx, _ev) = mpsc::unbounded_channel();
    let untouched = std::fs::read(&f.notes).unwrap();
    for _ in 0..3 {
        assert!(lec.snapshot(&store, "", &ev_tx).await.is_err());
        assert_eq!(std::fs::read(&f.notes).unwrap(), untouched);
        assert_eq!(Sidecar::load(&f.sidecar()).unwrap().unwrap().notes.segment_cursor, 0, "the batch stays pending");
    }
    assert!(!f.journal().exists(), "a failed answer never starts a commit");
    lec.snapshot(&store, "", &ev_tx).await.unwrap();
    let notes = std::fs::read_to_string(&f.notes).unwrap();
    assert_eq!(notes.matches("<!-- ").count(), 1);
    assert_eq!(Sidecar::load(&f.sidecar()).unwrap().unwrap().notes.segment_cursor, 2);
    assert!(user_text(&sse.state.bodies()[3]).contains("Gradient descent steps downhill."), "the pending material went again");

    // A polish whose snapshot fails stops before its own request.
    SegmentLog::open(dir.path(), &f.stem).unwrap().append(NewSegment { recording_id: uuid::Uuid::nil(), start_sample: 96_000, end_sample: 112_000, text: "Momentum.".into(), words: vec![], source: SegmentSource::Live }, Local::now()).unwrap();
    replies.lock().unwrap().push(Reply::Status(503, "{}".into()));
    let (ev_tx, mut ev) = mpsc::unbounded_channel();
    assert!(!lec.polish(&store, &ev_tx).await);
    assert!(matches!(ev.recv().await, Some(Event::PolishStopped(_)) | Some(Event::Busy(_))));
    assert!(sse.state.bodies().iter().all(|b| !b["messages"][0]["content"].as_str().unwrap().starts_with("You turn raw")));
    assert_eq!(std::fs::read_to_string(&f.notes).unwrap(), notes);
}

#[tokio::test]
async fn a_legacy_folder_migrates_and_its_pending_lines_reach_the_next_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let f = files(dir.path());
    std::fs::create_dir_all(f.state_dir()).unwrap();
    std::fs::write(&f.notes, format!("{TITLE}\n\n<!-- 10:00:12 -->\n## Intro\n- one, two\n")).unwrap();
    let noted = "--- started 10:00:00 ---\n[10:00:05] one\n[10:00:09] two\n";
    std::fs::write(&f.transcript, format!("{noted}--- resumed 10:20:00 ---\n[10:20:03] three\n")).unwrap();
    std::fs::write(f.legacy_state(), serde_json::json!({"transcript_offset": noted.len(), "slide_index": 0}).to_string()).unwrap();
    let (sc, report) = folder::open(&f, TITLE, false).unwrap();
    assert_eq!((report.how, report.pending_segments), (How::Migrated, 1));
    let sse = fake_sse::start(respond).await;
    let lec = lecture_for(&f, &sse.url, &dir.path().join("spend.jsonl"));
    let (ev_tx, _ev) = mpsc::unbounded_channel();
    lec.snapshot(&Store::offline(sc, f.sidecar()), "", &ev_tx).await.unwrap();
    let sent = user_text(&sse.state.bodies()[0]);
    assert!(sent.contains("[10:20:03] three") && !sent.contains("[10:00:05] one"), "{sent}");
    assert!(sent.contains("## Intro\n- one, two"), "the notes so far go with it");
    assert_eq!(Sidecar::load(&f.sidecar()).unwrap().unwrap().notes.segment_cursor, 3);
}

/// Final review, Important 4: a slide whose file was deleted after it was registered is skipped with
/// a warning; the snapshot commits and moves past it instead of failing for the rest of the lecture.
#[tokio::test]
async fn a_registered_slide_whose_file_is_gone_is_skipped_with_a_warning() {
    use lecturelive_core::session::sidecar::SlideEntry;
    let dir = tempfile::tempdir().unwrap();
    let (f, _) = offline_folder(dir.path());
    support::slides::png(&f.slides.join("slide_01_100001.png"));
    let at = chrono::TimeZone::with_ymd_and_hms(&Local, 2026, 9, 25, 10, 0, 1).unwrap();
    let mut sc = Sidecar::load(&f.sidecar()).unwrap().unwrap();
    sc.slides = vec![
        SlideEntry { index: 1, file: "slides/slide_01_100001.png".into(), shown_at: at },
        SlideEntry { index: 2, file: "slides/slide_02_100002.png".into(), shown_at: at }, // deleted by hand
    ];
    sc.save(&f.sidecar()).unwrap();
    let sse = fake_sse::start(respond).await;
    let lec = lecture_for(&f, &sse.url, &dir.path().join("spend.jsonl"));
    let (ev_tx, mut ev) = mpsc::unbounded_channel();
    lec.snapshot(&Store::offline(sc, f.sidecar()), "", &ev_tx).await.expect("a missing slide file does not stop the notes");
    drop(ev_tx);
    let mut warned = false;
    while let Some(e) = ev.recv().await {
        warned |= matches!(&e, Event::Warning(m) if m.contains("slide_02_100002.png"));
    }
    assert!(warned, "the person is told which slide was skipped");
    let sent = &sse.state.bodies()[0];
    assert_eq!(sent["messages"][1]["content"].as_array().unwrap().len(), 2, "the text and the one slide that exists");
    assert!(!user_text(sent).contains("slide_02_"), "the missing slide is not offered to the model");
    let notes = std::fs::read_to_string(&f.notes).unwrap();
    assert!(notes.contains("![Slide 1](slides/slide_01_100001.png)") && !notes.contains("Slide 2"));
    assert_eq!(Sidecar::load(&f.sidecar()).unwrap().unwrap().notes.slide_index, 2, "the cursor moves past the missing slide");
}

/// Final review, Important 3: a second stop also drops the operations still queued behind the one in
/// flight, so a person who asked for a polish and then wants out is not held for minutes.
#[tokio::test]
async fn a_second_stop_skips_queued_operations() {
    let dir = tempfile::tempdir().unwrap();
    let f = files(dir.path());
    let ledger = dir.path().join("spend.jsonl");
    folder::open(&f, TITLE, false).unwrap();
    let stt = fake_stt::start(fake_stt::Config::default()).await;
    let sse = fake_sse::start(|_| Reply::Stall).await; // every request hangs until the idle timeout
    let stt_cfg = SttConfig { url: stt.url.clone(), backoff_unit: ms(1), connect_timeout: ms(2_000), send_timeout: ms(2_000), idle_timeout: ms(2_000), finalize_wait: ms(2_000), done_wait: ms(2_000), ..SttConfig::new("test-key".into(), vec![]) };
    let spend = Spend::open(&ledger, "Machine Learning", NAME, f.date).unwrap();
    let chat = ChatClient::new(ChatConfig { url: sse.url.clone(), idle_timeout: ms(1_000), ..ChatConfig::new("test-key".into()) }, Some(spend.clone())).unwrap();
    let lec = Arc::new(Lecture { files: f.clone(), course: "Machine Learning".into(), name: NAME.into(), title: TITLE.into(), chat, spend });
    let session = SessionConfig { dir: f.dir.clone(), stem: f.stem.clone(), stt: Some(stream::spawn(stt_cfg).unwrap()), ..Default::default() };
    let (cmd, cmd_rx) = mpsc::unbounded_channel();
    let (ev_tx, mut ev) = mpsc::unbounded_channel();
    let run = tokio::spawn(lecture::run(lec, session, Box::new(Talking { pace: ms(2), fake: stt.state.clone() }), SlideWatch { screenshots: None, poll: ms(20) }, cmd_rx, ev_tx));
    segments_seen(&mut ev, 1).await;
    cmd.send(Command::Op(Op::Snapshot(String::new()))).unwrap();
    until(&mut ev, "the snapshot in flight", |e| matches!(e, Event::Busy(m) if m.starts_with("snapshot"))).await;
    cmd.send(Command::Op(Op::Polish)).unwrap();
    cmd.send(Command::Stop).unwrap();
    cmd.send(Command::Stop).unwrap();
    tokio::time::timeout(Duration::from_secs(20), run).await.expect("the lecture ends").unwrap().unwrap();
    let systems: Vec<String> = sse.state.bodies().iter().map(|b| b["messages"][0]["content"].as_str().unwrap_or_default().chars().take(22).collect()).collect();
    assert_eq!(systems.iter().filter(|s| s.starts_with("You are the note-taker")).count(), 2, "the snapshot in flight and the last one: {systems:?}");
    assert!(!systems.iter().any(|s| s.starts_with("You turn raw")), "the queued polish was skipped");
}

/// Cancel (spec §6.2, §10): the request in flight stops, the polish queued behind it never sends, the
/// notes are untouched, and the next snapshot sends the same material. The state the session serves
/// carries the revision its events report.
#[tokio::test]
async fn a_cancelled_snapshot_writes_nothing_drops_the_queue_and_its_material_goes_again() {
    use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
    use tokio::sync::oneshot;
    let dir = tempfile::tempdir().unwrap();
    let f = files(dir.path());
    let ledger = dir.path().join("spend.jsonl");
    folder::open(&f, TITLE, false).unwrap();
    let stt = fake_stt::start(fake_stt::Config::default()).await;
    let first = Arc::new(AtomicBool::new(true));
    let sse = fake_sse::start(move |b| if first.swap(false, SeqCst) { Reply::Stall } else { respond(b) }).await;
    let stt_cfg = SttConfig { url: stt.url.clone(), backoff_unit: ms(1), connect_timeout: ms(2_000), send_timeout: ms(2_000), idle_timeout: ms(2_000), finalize_wait: ms(2_000), done_wait: ms(2_000), ..SttConfig::new("test-key".into(), vec![]) };
    let lec = Arc::new(lecture_for(&f, &sse.url, &ledger));
    let session = SessionConfig { dir: f.dir.clone(), stem: f.stem.clone(), stt: Some(stream::spawn(stt_cfg).unwrap()), ..Default::default() };
    let (cmd, cmd_rx) = mpsc::unbounded_channel();
    let (ev_tx, mut ev) = mpsc::unbounded_channel();
    let run = tokio::spawn(lecture::run(lec, session, Box::new(Talking { pace: ms(2), fake: stt.state.clone() }), SlideWatch { screenshots: None, poll: ms(20) }, cmd_rx, ev_tx));
    segments_seen(&mut ev, 1).await;
    let untouched = std::fs::read(&f.notes).unwrap();
    cmd.send(Command::Op(Op::Snapshot("first".into()))).unwrap();
    until(&mut ev, "the snapshot in flight", |e| matches!(e, Event::Busy(m) if m.starts_with("snapshot"))).await;
    while sse.state.requests() == 0 {
        tokio::time::sleep(ms(5)).await; // the request is on the wire, stalled
    }
    cmd.send(Command::Op(Op::Polish)).unwrap();
    cmd.send(Command::Cancel).unwrap();
    until(&mut ev, "the cancel", |e| matches!(e, Event::Cancelled(what) if what == "the snapshot")).await;
    assert_eq!(std::fs::read(&f.notes).unwrap(), untouched, "nothing is written");
    let state = |cmd: &mpsc::UnboundedSender<Command>| {
        let (tx, rx) = oneshot::channel();
        cmd.send(Command::State(tx)).unwrap();
        rx
    };
    let sc = state(&cmd).await.unwrap().unwrap();
    assert_eq!(sc.notes.segment_cursor, 0, "the batch stays pending");
    cmd.send(Command::Op(Op::Snapshot(String::new()))).unwrap();
    let Event::Committed { revision, .. } = until(&mut ev, "the next snapshot", |e| matches!(e, Event::Committed { .. })).await else { unreachable!() };
    let sc = state(&cmd).await.unwrap().unwrap();
    assert_eq!(sc.notes.revision, revision, "the event carries the revision the state reports");
    assert!(sc.notes.segment_cursor >= 1);
    cmd.send(Command::Stop).unwrap();
    tokio::time::timeout(Duration::from_secs(30), run).await.expect("the lecture stops").unwrap().unwrap();
    let bodies = sse.state.bodies();
    assert!(!bodies.iter().any(|b| b["messages"][0]["content"].as_str().unwrap_or_default().starts_with("You turn raw")), "the queued polish never sent its request");
    let cancelled: Vec<String> = user_text(&bodies[0]).lines().filter(|l| l.starts_with('[')).map(str::to_string).collect();
    assert!(!cancelled.is_empty());
    assert!(cancelled.iter().all(|l| user_text(&bodies[1]).contains(l.as_str())), "the cancelled material went again");
    assert!(spend::read(&ledger).unwrap().iter().filter(|e| e.what == "notes").count() <= bodies.len() - 1, "a cancelled stream records nothing");
}
