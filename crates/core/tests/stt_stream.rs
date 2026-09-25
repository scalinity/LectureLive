mod support;

use std::sync::atomic::Ordering::SeqCst;
use std::time::Duration;

use lecturelive_core::audio::frame::Frame;
use lecturelive_core::session::sidecar::{Gap, GapKind};
use lecturelive_core::stt::stream::{spawn, SttConfig, SttEvent, SttInput, SttLink};
use lecturelive_core::stt::transcript::Utterance;
use support::fake_stt::{self, Config, FakeStt, Mode, Outage, Refusal};
use support::{fixtures, speech};
use uuid::Uuid;

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn cfg(fake: &FakeStt) -> SttConfig {
    SttConfig {
        url: fake.url.clone(),
        backoff_unit: ms(1),
        connect_timeout: ms(2_000),
        send_timeout: ms(2_000),
        idle_timeout: ms(2_000),
        done_wait: ms(2_000),
        ..SttConfig::new("test-key".into(), vec![])
    }
}

fn speech_frame(id: Uuid, k: u64) -> Frame {
    Frame { recording_id: id, sample_offset: k * 1600, valid_samples: 1600, pcm16: speech::frame_pcm(k) }
}

/// The frames of a recording `samples` long, silent, the last one partial.
fn silent_frames(id: Uuid, samples: u64) -> Vec<Frame> {
    (0..samples.div_ceil(1600))
        .map(|k| Frame { recording_id: id, sample_offset: k * 1600, valid_samples: (samples - k * 1600).min(1600) as u32, pcm16: [0; 1600] })
        .collect()
}

/// Sends frames paced like a fast source and tells the fake how far the audio has got.
async fn feed(link: &SttLink, fake: &FakeStt, frames: impl IntoIterator<Item = Frame>) {
    for f in frames {
        let end = f.sample_offset + f.valid_samples as u64;
        link.input.send(SttInput::Frame(f)).await.unwrap();
        fake.state.feed.store(end, SeqCst);
        tokio::time::sleep(ms(1)).await;
    }
}

async fn next_event(link: &mut SttLink) -> SttEvent {
    tokio::time::timeout(Duration::from_secs(20), link.events.recv()).await.expect("an event within 20 s").expect("the worker is running")
}

async fn until_ended(link: &mut SttLink) -> Vec<SttEvent> {
    let mut seen = Vec::new();
    loop {
        let e = next_event(link).await;
        let ended = matches!(e, SttEvent::Ended { .. });
        seen.push(e);
        if ended {
            return seen;
        }
    }
}

fn utterances(ev: &[SttEvent]) -> Vec<Utterance> {
    ev.iter()
        .filter_map(|e| match e {
            SttEvent::Utterance { utterance, .. } => Some(utterance.clone()),
            _ => None,
        })
        .collect()
}

fn spans(ev: &[SttEvent]) -> Vec<(u64, u64, String)> {
    utterances(ev).into_iter().map(|u| (u.start_sample, u.end_sample, u.text)).collect()
}

fn origins(ev: &[SttEvent]) -> Vec<(u64, Option<Gap>)> {
    ev.iter()
        .filter_map(|e| match e {
            SttEvent::Connected { origin, gap, .. } => Some((*origin, gap.clone())),
            _ => None,
        })
        .collect()
}

fn gaps(ev: &[SttEvent]) -> Vec<Gap> {
    ev.iter()
        .filter_map(|e| match e {
            SttEvent::Connected { gap, .. } | SttEvent::Ended { gap, .. } => gap.clone(),
            _ => None,
        })
        .collect()
}

/// Every synthetic word exactly once: the live words, plus the words recovery will find in each gap.
fn assert_partition(ev: &[SttEvent], len: u64) {
    let mut words: Vec<(u64, String)> = utterances(ev).iter().flat_map(|u| u.words.iter().map(|w| (w.start_sample, w.text.clone()))).collect();
    for g in gaps(ev) {
        words.extend(speech::words_starting(g.start_sample, g.end_sample.unwrap()).into_iter().map(|w| (speech::word_start(w), speech::word_text(w))));
    }
    words.sort();
    assert_eq!(words.into_iter().map(|w| w.1).collect::<Vec<_>>(), speech::expected_words(len));
}

async fn begin(link: &SttLink) -> Uuid {
    let id = Uuid::new_v4();
    link.input.send(SttInput::Begin { recording_id: id }).await.unwrap();
    id
}

async fn end(link: &SttLink, id: Uuid, samples: u64) {
    link.input.send(SttInput::End { recording_id: id, samples }).await.unwrap();
}

#[tokio::test]
async fn natural_endpoints_replay_to_exact_utterances() {
    let fake = fake_stt::start(Config { mode: Mode::Replay(fixtures::load("endpoint_pauses")), ..Default::default() }).await;
    let mut link = spawn(cfg(&fake)).unwrap();
    let id = begin(&link).await;
    fake.state.wait_accepted(1).await;
    feed(&link, &fake, silent_frames(id, 167_615)).await;
    end(&link, id, 167_615).await;
    let ev = until_ended(&mut link).await;
    assert_eq!(origins(&ev), vec![(0, None)]);
    assert_eq!(
        spans(&ev),
        vec![
            (16, 49_280, "Gradient descent updates the weights.".to_string()),
            (59_520, 117_760, "The learning rate controls the step size.".to_string()),
            (128_640, 167_616, "Momentum smooths the updates.".to_string()),
        ]
    );
    assert_eq!(ev.last(), Some(&SttEvent::Ended { recording_id: id, gap: None }));
    assert_eq!(fake.state.frames.load(SeqCst), 105);
    assert_eq!(fake.state.texts(), vec![r#"{"type":"audio.done"}"#.to_string()]);
}

#[tokio::test]
async fn each_recording_gets_its_own_connection_from_its_first_frame() {
    let fake = fake_stt::start(Config::default()).await;
    let mut link = spawn(cfg(&fake)).unwrap();
    for n in [50, 30] {
        let id = begin(&link).await;
        feed(&link, &fake, (0..n).map(|k| speech_frame(id, k))).await;
        end(&link, id, n * 1600).await;
        let ev = until_ended(&mut link).await;
        assert_eq!(origins(&ev), vec![(0, None)]);
        assert!(gaps(&ev).is_empty(), "{:?}", gaps(&ev));
        assert_partition(&ev, n * 1600);
    }
    assert_eq!(fake.state.accepted.load(SeqCst), 2);
}

#[tokio::test]
async fn a_dropped_connection_reconnects_and_its_unclosed_audio_becomes_a_gap() {
    let outage = Outage { at_sample: 30 * 16_000, refuse_for: 3 * 16_000, refusal: Refusal::Status(503), freeze: false };
    let fake = fake_stt::start(Config { outages: vec![outage], ..Default::default() }).await;
    let mut link = spawn(cfg(&fake)).unwrap();
    let id = begin(&link).await;
    feed(&link, &fake, (0..600).map(|k| speech_frame(id, k))).await;
    end(&link, id, 600 * 1600).await;
    let ev = until_ended(&mut link).await;
    let o = origins(&ev);
    assert_eq!(o.len(), 2, "{o:?}");
    let gap = o[1].1.clone().expect("the reconnect reports what it missed");
    assert_eq!((gap.start_sample, gap.kind, gap.resolved), (speech::endpoint(5), GapKind::SttOffline, false));
    assert_eq!(gap.end_sample, Some(o[1].0));
    assert!(o[1].0 > 30 * 16_000);
    assert!(ev.iter().any(|e| matches!(e, SttEvent::Retrying { .. })));
    assert_partition(&ev, 600 * 1600);
}

#[tokio::test]
async fn after_a_long_outage_the_stream_resumes_at_most_five_seconds_back() {
    let outage = Outage { at_sample: 10 * 16_000, refuse_for: 20 * 16_000, refusal: Refusal::TcpClose, freeze: false };
    let fake = fake_stt::start(Config { outages: vec![outage], ..Default::default() }).await;
    let mut link = spawn(cfg(&fake)).unwrap();
    let id = begin(&link).await;
    feed(&link, &fake, (0..400).map(|k| speech_frame(id, k))).await;
    end(&link, id, 400 * 1600).await;
    let ev = until_ended(&mut link).await;
    let o = origins(&ev);
    assert_eq!(o.len(), 2, "{o:?}");
    assert!(o[1].0 >= 25 * 16_000, "refused until 30 s, and the hold keeps only 5 s: resumed at {}", o[1].0);
    assert_partition(&ev, 400 * 1600);
}

#[tokio::test]
async fn frames_that_never_reached_the_writer_restart_the_epoch_with_an_overflow_gap() {
    let fake = fake_stt::start(Config::default()).await;
    let mut link = spawn(cfg(&fake)).unwrap();
    let id = begin(&link).await;
    feed(&link, &fake, (0..100).chain(110..200).map(|k| speech_frame(id, k))).await;
    end(&link, id, 200 * 1600).await;
    let ev = until_ended(&mut link).await;
    let o = origins(&ev);
    assert_eq!(o[0], (0, None));
    assert_eq!(o[1].0, 110 * 1600);
    assert_eq!(o[1].1.as_ref().map(|g| g.kind), Some(GapKind::SttOverflow));
    assert_partition(&ev, 200 * 1600);
}

#[tokio::test]
async fn a_refusal_stops_stt_for_the_session_without_retrying() {
    let fake = fake_stt::start(Config { refuse_all: Some(Refusal::Status(400)), ..Default::default() }).await;
    let mut link = spawn(cfg(&fake)).unwrap();
    for first in [true, false] {
        let id = begin(&link).await;
        feed(&link, &fake, (0..20).map(|k| speech_frame(id, k))).await;
        end(&link, id, 32_000).await;
        let ev = until_ended(&mut link).await;
        assert_eq!(gaps(&ev), vec![Gap::new(id, 0, Some(32_000), GapKind::SttRefused)]);
        let refused = SttEvent::Refused("400 Bad Request: Incorrect API key provided. You can obtain an API key from https://console.x.ai.".into());
        assert_eq!(ev.contains(&refused), first);
        assert!(!ev.iter().any(|e| matches!(e, SttEvent::Retrying { .. })));
    }
    assert_eq!(fake.state.attempts.load(SeqCst), 1);
}

#[tokio::test]
async fn a_refusal_after_a_drop_stops_reconnecting() {
    let outage = Outage { at_sample: 10 * 16_000, refuse_for: 0, refusal: Refusal::Status(503), freeze: false };
    let fake = fake_stt::start(Config { outages: vec![outage], refuse_after_drop: Some(Refusal::Status(401)), ..Default::default() }).await;
    let mut link = spawn(cfg(&fake)).unwrap();
    let id = begin(&link).await;
    feed(&link, &fake, (0..200).map(|k| speech_frame(id, k))).await;
    end(&link, id, 200 * 1600).await;
    let ev = until_ended(&mut link).await;
    assert!(ev.iter().any(|e| matches!(e, SttEvent::Refused(_))));
    assert_eq!(fake.state.attempts.load(SeqCst), 2);
    assert_eq!(gaps(&ev), vec![Gap::new(id, speech::endpoint(1), Some(200 * 1600), GapKind::SttRefused)]);
    assert_partition(&ev, 200 * 1600);
}

#[tokio::test]
async fn a_silent_connection_is_treated_as_lost() {
    let outage = Outage { at_sample: 10 * 16_000, refuse_for: 0, refusal: Refusal::Status(503), freeze: true };
    let fake = fake_stt::start(Config { outages: vec![outage], ..Default::default() }).await;
    let mut link = spawn(SttConfig { idle_timeout: ms(300), ..cfg(&fake) }).unwrap();
    let id = begin(&link).await;
    feed(&link, &fake, (0..300).map(|k| speech_frame(id, k))).await;
    end(&link, id, 300 * 1600).await;
    let ev = until_ended(&mut link).await;
    assert!(ev.iter().any(|e| matches!(e, SttEvent::Retrying { reason, .. } if reason.contains("no message"))), "{ev:?}");
    assert_eq!(origins(&ev).len(), 2);
    assert_partition(&ev, 300 * 1600);
}

#[tokio::test]
async fn a_recording_that_ends_while_offline_ends_with_a_gap() {
    let fake = fake_stt::start(Config { refuse_all: Some(Refusal::TcpClose), ..Default::default() }).await;
    let mut link = spawn(cfg(&fake)).unwrap();
    let id = begin(&link).await;
    feed(&link, &fake, (0..30).map(|k| speech_frame(id, k))).await;
    end(&link, id, 48_000).await;
    let ev = tokio::time::timeout(Duration::from_secs(2), until_ended(&mut link)).await.expect("no hang");
    assert_eq!(gaps(&ev), vec![Gap::new(id, 0, Some(48_000), GapKind::SttOffline)]);
    assert!(fake.state.attempts.load(SeqCst) > 1, "a transient failure is retried");
}

#[tokio::test]
async fn a_missing_transcript_done_leaves_the_unclosed_tail_as_a_gap() {
    let fake = fake_stt::start(Config { mute_after_audio_done: true, ..Default::default() }).await;
    let mut link = spawn(SttConfig { done_wait: ms(200), ..cfg(&fake) }).unwrap();
    let id = begin(&link).await;
    feed(&link, &fake, (0..60).map(|k| speech_frame(id, k))).await;
    end(&link, id, 96_000).await;
    let ev = until_ended(&mut link).await;
    assert_eq!(gaps(&ev), vec![Gap::new(id, speech::endpoint(0), Some(96_000), GapKind::SttOffline)]);
    assert_partition(&ev, 96_000);
}
