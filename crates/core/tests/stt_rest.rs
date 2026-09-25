mod support;

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering::SeqCst;
use std::time::Duration;

use lecturelive_core::audio::frame::Frame;
use lecturelive_core::audio::recorder::Recorder;
use lecturelive_core::stt::protocol::ServerWord;
use lecturelive_core::stt::rest::{spawn_recovery, RecoverEvent, RecoverJob, RestClient, RestConfig, RestError};
use support::{fake_rest, speech};
use uuid::Uuid;

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn cfg(url: &str) -> RestConfig {
    RestConfig { url: url.into(), retry_unit: ms(1), request_timeout: ms(5_000), file_wait: ms(2_000), ..RestConfig::new("test-key".into(), vec!["gradient descent".into()]) }
}

fn synthetic(frames: Range<u64>) -> Vec<i16> {
    frames.flat_map(speech::frame_pcm).collect()
}

fn write_frames(r: &mut Recorder, frames: Range<u64>) {
    let id = Uuid::new_v4();
    for k in frames {
        r.write_frame(&Frame { recording_id: id, sample_offset: k * 1600, valid_samples: 1600, pcm16: speech::frame_pcm(k) }).unwrap();
    }
}

fn record(dir: &Path, frames: Range<u64>) -> PathBuf {
    let mut r = Recorder::create(&dir.join("recordings"), "session_20260925_100000").unwrap();
    write_frames(&mut r, frames);
    r.finalize().unwrap()
}

async fn until_done(link: &mut lecturelive_core::stt::rest::RecoveryLink) -> Vec<(u64, u64, Vec<String>)> {
    let mut pieces = Vec::new();
    loop {
        match tokio::time::timeout(Duration::from_secs(20), link.events.recv()).await.unwrap().unwrap() {
            RecoverEvent::Piece { start_sample, end_sample, words, .. } => pieces.push((start_sample, end_sample, words.into_iter().map(|w| w.text).collect())),
            RecoverEvent::Done { .. } => return pieces,
            other => panic!("{other:?}"),
        }
    }
}

#[tokio::test]
async fn a_clip_is_posted_as_a_wav_with_the_cli_fields() {
    let fake = fake_rest::start(None).await;
    let t = RestClient::new(cfg(&fake.url)).unwrap().transcribe(&synthetic(0..40)).await.unwrap();
    assert_eq!(t.text, "w0 w1 w2 w3 w4 w5 w6 w7");
    assert_eq!(t.words[1], ServerWord { text: "w1".into(), start: 0.5, end: 0.9 });
    assert_eq!(fake.state.fields(), vec![("language".into(), "en".into()), ("format".into(), "true".into()), ("keyterm".into(), "gradient descent".into())]);
    assert_eq!(fake.state.auth(), vec!["Bearer test-key".to_string()]);
}

#[tokio::test]
async fn a_refusal_is_reported_as_one() {
    let fake = fake_rest::start(Some(400)).await;
    let err = RestClient::new(cfg(&fake.url)).unwrap().transcribe(&synthetic(0..10)).await.unwrap_err();
    assert_eq!(err, RestError::Refused("400 Bad Request: Incorrect API key provided. You can obtain an API key from https://console.x.ai.".into()));
}

#[tokio::test]
async fn recovery_reads_the_gap_from_the_recording_and_commits_it_in_pieces() {
    let dir = tempfile::tempdir().unwrap();
    let wav = record(dir.path(), 0..700);
    let fake = fake_rest::start(None).await;
    let mut link = spawn_recovery(RestClient::new(cfg(&fake.url)).unwrap());
    link.jobs.send(RecoverJob { recording_id: Uuid::new_v4(), gap_start: 80_000, from: 80_000, end: 1_120_000, wav }).unwrap();
    let pieces = until_done(&mut link).await;
    assert!(pieces.len() >= 3, "65 s go in at least three pieces: {}", pieces.len());
    assert_eq!((pieces[0].0, pieces.last().unwrap().1), (80_000, 1_120_000));
    assert!(pieces.windows(2).all(|w| w[0].1 == w[1].0), "the pieces tile the gap");
    let words: Vec<String> = pieces.iter().flat_map(|p| p.2.clone()).collect();
    let expected: Vec<String> = speech::words_starting(80_000, 1_120_000).into_iter().map(speech::word_text).collect();
    assert_eq!(words, expected);
    assert_eq!(fake.state.requests.load(SeqCst), pieces.len());
}

#[tokio::test]
async fn recovery_waits_for_the_recorder_to_write_the_gap() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Recorder::create(&dir.path().join("recordings"), "session").unwrap();
    write_frames(&mut r, 0..10);
    r.checkpoint().unwrap();
    let wav = r.path().to_path_buf();
    let writer = std::thread::spawn(move || {
        std::thread::sleep(ms(300));
        write_frames(&mut r, 10..40);
        r.finalize().unwrap();
    });
    let fake = fake_rest::start(None).await;
    let mut link = spawn_recovery(RestClient::new(cfg(&fake.url)).unwrap());
    link.jobs.send(RecoverJob { recording_id: Uuid::new_v4(), gap_start: 0, from: 0, end: 40 * 1600, wav }).unwrap();
    let pieces = until_done(&mut link).await;
    writer.join().unwrap();
    assert_eq!(pieces.iter().flat_map(|p| p.2.clone()).collect::<Vec<_>>(), speech::expected_words(40 * 1600));
}

#[tokio::test]
async fn a_refused_recovery_ends_recovery_for_the_session() {
    let dir = tempfile::tempdir().unwrap();
    let wav = record(dir.path(), 0..20);
    let fake = fake_rest::start(Some(400)).await;
    let mut link = spawn_recovery(RestClient::new(cfg(&fake.url)).unwrap());
    for gap_start in [0, 16_000] {
        let _ = link.jobs.send(RecoverJob { recording_id: Uuid::new_v4(), gap_start, from: gap_start, end: gap_start + 16_000, wav: wav.clone() });
    }
    assert!(matches!(link.events.recv().await, Some(RecoverEvent::Failed { refused: true, gap_start: 0, .. })));
    assert!(link.events.recv().await.is_none(), "the worker has ended");
    assert_eq!(fake.state.requests.load(SeqCst), 1);
}
