//! Live checks against api.x.ai (spec §5), with synthesised speech only, a few seconds each.
//! GROK_API_KEY comes from the environment or the repository's .env:
//!     cargo test -p lecturelive-core --test stt_live -- --ignored --nocapture
use std::time::Duration;

use lecturelive_core::audio::frame::Frame;
use lecturelive_core::session::sidecar::{Gap, GapKind};
use lecturelive_core::stt::stream::{spawn, SttConfig, SttEvent, SttInput, SttLink};
use uuid::Uuid;

fn api_key() -> String {
    if let Ok(k) = std::env::var("GROK_API_KEY") {
        return k;
    }
    let env = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.env")).expect("GROK_API_KEY in the environment or the repository's .env");
    env.lines().find_map(|l| l.strip_prefix("GROK_API_KEY=")).map(|v| v.trim().trim_matches('"').to_string()).expect("GROK_API_KEY in .env")
}

fn speech_wav() -> Vec<i16> {
    hound::WavReader::open(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/stt/speech.wav")).unwrap().samples::<i16>().map(|s| s.unwrap()).collect()
}

async fn next_event(link: &mut SttLink) -> SttEvent {
    tokio::time::timeout(Duration::from_secs(30), link.events.recv()).await.expect("an event within 30 s").expect("the worker is running")
}

async fn until_ended(link: &mut SttLink) -> Vec<SttEvent> {
    let mut seen = Vec::new();
    loop {
        let e = next_event(link).await;
        println!("{e:?}");
        let ended = matches!(e, SttEvent::Ended { .. });
        seen.push(e);
        if ended {
            return seen;
        }
    }
}

#[tokio::test]
#[ignore]
async fn live_stream_confirms_a_cutoff_and_flushes_at_the_end() {
    let pcm = speech_wav();
    let mut link = spawn(SttConfig::new(api_key(), vec!["cross-entropy".into()])).unwrap();
    let id = Uuid::new_v4();
    link.input.send(SttInput::Begin { recording_id: id }).await.unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await; // let the handshake finish, so the stream starts at sample 0
    for (k, chunk) in pcm.chunks(1600).enumerate() {
        let mut pcm16 = [0i16; 1600];
        pcm16[..chunk.len()].copy_from_slice(chunk);
        let frame = Frame { recording_id: id, sample_offset: k as u64 * 1600, valid_samples: chunk.len() as u32, pcm16 };
        link.input.send(SttInput::Frame(frame)).await.unwrap();
        if k + 1 == 30 {
            link.input.send(SttInput::Cutoff { id: 1, recording_id: id, sample: 48_000 }).await.unwrap();
        }
        tokio::time::sleep(Duration::from_millis(20)).await; // five times real time
    }
    link.input.send(SttInput::End { recording_id: id, samples: pcm.len() as u64 }).await.unwrap();
    let ev = until_ended(&mut link).await;
    assert!(matches!(ev.first(), Some(SttEvent::Connected { origin: 0, gap: None, .. })), "{:?}", ev.first());
    assert!(ev.contains(&SttEvent::Cutoff { id: 1, confirmed: true }));
    let u: Vec<_> = ev
        .iter()
        .filter_map(|e| match e {
            SttEvent::Utterance { utterance, .. } => Some(utterance.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(u.len(), 2, "{u:?}");
    assert_eq!(u[0].end_sample, 48_000, "the finalize closes exactly at the cutoff");
    assert!(u[1].text.to_lowercase().contains("gradient descent"), "{}", u[1].text);
    assert_eq!(ev.last(), Some(&SttEvent::Ended { recording_id: id, gap: None }));
}

#[tokio::test]
#[ignore]
async fn live_refusal_of_a_bad_key_stops_stt() {
    let mut link = spawn(SttConfig::new("xai-not-a-real-key".into(), vec![])).unwrap();
    let id = Uuid::new_v4();
    link.input.send(SttInput::Begin { recording_id: id }).await.unwrap();
    link.input.send(SttInput::Frame(Frame { recording_id: id, sample_offset: 0, valid_samples: 1600, pcm16: [0; 1600] })).await.unwrap();
    loop {
        match next_event(&mut link).await {
            SttEvent::Refused(msg) => {
                println!("{msg}");
                assert!(msg.starts_with("400 Bad Request: Incorrect API key"), "{msg}");
                break;
            }
            SttEvent::Retrying { reason, .. } => panic!("a bad key must not be retried: {reason}"),
            _ => {}
        }
    }
    link.input.send(SttInput::End { recording_id: id, samples: 1600 }).await.unwrap();
    let ev = until_ended(&mut link).await;
    assert_eq!(ev.last(), Some(&SttEvent::Ended { recording_id: id, gap: Some(Gap::new(id, 0, Some(1600), GapKind::SttRefused)) }));
}
