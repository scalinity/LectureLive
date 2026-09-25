//! The M2 gate (docs/milestones.md): exact outputs on protocol fixtures; disconnects of 3, 15, 45
//! and 300 s with no duplicate or missing committed intervals; REST recovery exercised; a 4xx
//! stops STT without a reconnect loop while recording continues. No network.
mod support;

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::Arc;
use std::time::Duration;

use lecturelive_core::audio::source::Source;
use lecturelive_core::session::coordinator::{self, CutoffResult, Notification, SessionConfig, StopReport, SttStatus};
use lecturelive_core::session::segments::{self, segments_path, transcript_path, Segment, SegmentSource};
use lecturelive_core::session::sidecar::{sidecar_path, Gap, GapKind, Sidecar};
use lecturelive_core::stt::protocol::FINALIZE;
use lecturelive_core::stt::rest::{spawn_recovery, RestClient, RestConfig};
use lecturelive_core::stt::stream::{self, SttConfig};
use support::fake_stt::{self, Config, Mode, Outage, Refusal};
use support::sources::{anchor, Silence, Speech};
use support::{fake_rest, fixtures, speech};

const STEM: &str = "lecture_notes_20260925";

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn links(stt_url: &str, rest_url: &str) -> SessionConfig {
    let stt = SttConfig {
        url: stt_url.into(),
        backoff_unit: ms(1),
        connect_timeout: ms(2_000),
        send_timeout: ms(2_000),
        idle_timeout: ms(2_000),
        finalize_wait: ms(2_000),
        done_wait: ms(2_000),
        ..SttConfig::new("test-key".into(), vec![])
    };
    let rest = RestConfig { url: rest_url.into(), retry_unit: ms(1), request_timeout: ms(5_000), file_wait: ms(5_000), ..RestConfig::new("test-key".into(), vec![]) };
    SessionConfig { stt: Some(stream::spawn(stt).unwrap()), recovery: Some(spawn_recovery(RestClient::new(rest).unwrap())), ..Default::default() }
}

struct Run {
    report: StopReport,
    notes: Vec<Notification>,
    segments: Vec<Segment>,
    sidecar: Sidecar,
    transcript: String,
}

fn read_back(dir: &Path, report: StopReport, notes: Vec<Notification>) -> Run {
    Run {
        report,
        notes,
        segments: segments::read(&segments_path(dir, STEM)).unwrap(),
        sidecar: Sidecar::load(&sidecar_path(dir, STEM)).unwrap().unwrap(),
        transcript: std::fs::read_to_string(transcript_path(dir, STEM)).unwrap(),
    }
}

async fn session(dir: &Path, source: impl Source, stt_url: &str, rest_url: &str) -> Run {
    let cfg = SessionConfig { dir: dir.to_path_buf(), stem: STEM.into(), ..links(stt_url, rest_url) };
    let (handle, mut notes) = coordinator::spawn(cfg, Box::new(source));
    let report = handle.finish().await.unwrap();
    let mut seen = Vec::new();
    while let Ok(n) = notes.try_recv() {
        seen.push(n);
    }
    read_back(dir, report, seen)
}

/// Every synthetic word exactly once, live or recovered, and no committed interval twice.
fn assert_committed_once(run: &Run, len: u64) {
    let mut words: Vec<(u64, String)> = run.segments.iter().flat_map(|s| s.words.iter().map(|w| (w.start_sample, w.text.clone()))).collect();
    words.sort();
    assert_eq!(words.into_iter().map(|w| w.1).collect::<Vec<_>>(), speech::expected_words(len), "missing or duplicate words");
    let mut spans: Vec<(u64, u64)> = run.segments.iter().map(|s| (s.start_sample, s.end_sample)).collect();
    spans.sort();
    assert!(spans.windows(2).all(|w| w[0].1 <= w[1].0), "overlapping segments: {spans:?}");
    assert_eq!(run.transcript.lines().count(), run.segments.len(), "one transcript line per segment");
}

async fn disconnect(outage_secs: u64, refusal: Refusal) {
    let dir = tempfile::tempdir().unwrap();
    let outage = Outage { at_sample: 30 * 16_000, refuse_for: outage_secs * 16_000, refusal, freeze: false };
    let fake = fake_stt::start(Config { outages: vec![outage], ..Default::default() }).await;
    let rest = fake_rest::start(None).await;
    let frames = (60 + outage_secs) * 10;
    let run = session(dir.path(), Speech { frames, pace: ms(2), fake: fake.state.clone() }, &fake.url, &rest.url).await;
    let len = frames * 1600;
    assert_eq!(run.report.recordings.len(), 1);
    assert_eq!(run.report.recordings[0].1, len, "the recording is whole");
    assert_committed_once(&run, len);
    let stt_gaps: Vec<&Gap> = run.sidecar.gaps.iter().filter(|g| g.kind.is_transcript()).collect();
    assert!(!stt_gaps.is_empty() && stt_gaps.iter().all(|g| g.resolved), "{stt_gaps:?}");
    assert!(rest.state.requests.load(SeqCst) >= 1, "REST recovery was exercised");
    assert!(run.segments.iter().any(|s| s.source == SegmentSource::Recovered));
    assert_eq!(run.report.unresolved, 0);
    assert!(run.sidecar.open_utterances.is_empty());
    if outage_secs > 5 {
        let covered = stt_gaps.iter().any(|g| g.start_sample < 30 * 16_000 && g.end_sample.unwrap() >= (30 + outage_secs - 5) * 16_000);
        assert!(covered, "a gap spans the outage, less the 5 s held for the new connection: {stt_gaps:?}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_3_s_disconnect_commits_every_interval_once() {
    disconnect(3, Refusal::TcpClose).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_15_s_disconnect_commits_every_interval_once() {
    disconnect(15, Refusal::Status(503)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_45_s_disconnect_commits_every_interval_once() {
    disconnect(45, Refusal::TcpClose).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_300_s_disconnect_commits_every_interval_once() {
    disconnect(300, Refusal::Status(503)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refusal_stops_stt_without_a_reconnect_loop_while_recording_continues() {
    let dir = tempfile::tempdir().unwrap();
    let fake = fake_stt::start(Config { refuse_all: Some(Refusal::Status(400)), ..Default::default() }).await;
    let rest = fake_rest::start(Some(400)).await;
    let run = session(dir.path(), Speech { frames: 200, pace: ms(2), fake: fake.state.clone() }, &fake.url, &rest.url).await;
    assert_eq!(run.report.recordings[0].1, 200 * 1600, "recording continued");
    assert_eq!(fake.state.attempts.load(SeqCst), 1, "one attempt, no reconnect loop");
    assert_eq!(rest.state.requests.load(SeqCst), 1, "recovery tried once, then stopped");
    let refusals = run.notes.iter().filter(|n| matches!(n, Notification::Stt(SttStatus::Refused(_)))).count();
    assert_eq!(refusals, 1);
    let id = run.sidecar.recordings[0].id;
    assert_eq!(run.sidecar.gaps, vec![Gap::new(id, 0, Some(200 * 1600), GapKind::SttRefused)]);
    assert!(run.segments.is_empty());
    assert_eq!(run.report.unresolved, 1, "the gap waits for a session with a working key");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refusal_after_a_disconnect_stops_reconnecting_and_recovery_fills_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let outage = Outage { at_sample: 10 * 16_000, refuse_for: 0, refusal: Refusal::Status(503), freeze: false };
    let fake = fake_stt::start(Config { outages: vec![outage], refuse_after_drop: Some(Refusal::Status(401)), ..Default::default() }).await;
    let rest = fake_rest::start(None).await;
    let run = session(dir.path(), Speech { frames: 300, pace: ms(2), fake: fake.state.clone() }, &fake.url, &rest.url).await;
    assert_eq!(fake.state.attempts.load(SeqCst), 2);
    assert_eq!(run.report.recordings[0].1, 300 * 1600);
    assert_committed_once(&run, 300 * 1600);
    assert!(run.sidecar.gaps.iter().any(|g| g.kind == GapKind::SttRefused && g.resolved));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_endpoint_fixture_writes_exact_segments_and_transcript_lines() {
    let dir = tempfile::tempdir().unwrap();
    let fake = fake_stt::start(Config { mode: Mode::Replay(fixtures::load("endpoint_pauses")), ..Default::default() }).await;
    let rest = fake_rest::start(None).await;
    let run = session(dir.path(), Silence { samples: 167_615, fake: fake.state.clone(), pause_after: None }, &fake.url, &rest.url).await;
    assert_eq!(
        run.transcript,
        "[10:00:00] Gradient descent updates the weights.\n[10:00:04] The learning rate controls the step size.\n[10:00:08] Momentum smooths the updates.\n"
    );
    assert_eq!(
        run.segments.iter().map(|s| (s.id, s.start_sample, s.end_sample, s.source)).collect::<Vec<_>>(),
        vec![(0, 16, 49_280, SegmentSource::Live), (1, 59_520, 117_760, SegmentSource::Live), (2, 128_640, 167_616, SegmentSource::Live)]
    );
    assert_eq!(run.segments[1].said_at, anchor() + chrono::Duration::milliseconds(4_143));
    assert!(run.sidecar.gaps.is_empty(), "{:?}", run.sidecar.gaps);
    assert_eq!(rest.state.requests.load(SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cutoff_through_the_session_is_confirmed_on_the_json_fixture() {
    let dir = tempfile::tempdir().unwrap();
    let fake = fake_stt::start(Config { mode: Mode::Replay(fixtures::load("finalize_json")), ..Default::default() }).await;
    let rest = fake_rest::start(None).await;
    let go = Arc::new(AtomicBool::new(false));
    let source = Silence { samples: 137_838, fake: fake.state.clone(), pause_after: Some((30, go.clone())) };
    let cfg = SessionConfig { dir: dir.path().to_path_buf(), stem: STEM.into(), ..links(&fake.url, &rest.url) };
    let (handle, _notes) = coordinator::spawn(cfg, Box::new(source));
    fake.state.wait_frames(30).await;
    let resume = async {
        // The recorded final came while the next frames went out: resume once the finalize is sent.
        while !fake.state.texts().iter().any(|t| t == FINALIZE) {
            tokio::time::sleep(ms(1)).await;
        }
        go.store(true, SeqCst);
    };
    let (cut, ()) = tokio::join!(handle.cutoff(), resume);
    assert_eq!(cut.unwrap(), CutoffResult { confirmed: true, segments: 1 });
    let report = handle.finish().await.unwrap();
    let run = read_back(dir.path(), report, vec![]);
    assert_eq!(
        run.transcript,
        "[10:00:00] Transfer learning reuses pre-trained weights. The\n[10:00:03] losses cross entropy over the vocabulary, gradient descent updates the weights after every batch.\n"
    );
}
