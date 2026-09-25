mod support;

use std::sync::atomic::Ordering::SeqCst;

use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use support::fake_stt::{self, Config, Mode, Refusal, BAD_KEY_BODY};
use support::{fixtures, speech};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::{self, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

type Client = WebSocketStream<MaybeTlsStream<TcpStream>>;

async fn next_text(ws: &mut Client) -> Option<Value> {
    loop {
        match ws.next().await? {
            Ok(Message::Text(t)) => return Some(serde_json::from_str(&t).unwrap()),
            Ok(Message::Close(_)) | Err(_) => return None,
            Ok(_) => {}
        }
    }
}

async fn all_texts(ws: &mut Client) -> Vec<Value> {
    let mut out = Vec::new();
    while let Some(v) = next_text(ws).await {
        out.push(v);
    }
    out
}

#[tokio::test]
async fn replay_answers_in_the_recorded_causal_order() {
    let fake = fake_stt::start(Config { mode: Mode::Replay(fixtures::load("finalize_json")), ..Default::default() }).await;
    let (mut ws, _) = tokio_tungstenite::connect_async(fake.url.as_str()).await.unwrap();
    assert_eq!(next_text(&mut ws).await.unwrap()["type"], "transcript.created");
    let frame = Message::Binary(vec![0u8; 3200].into());
    for _ in 0..30 {
        ws.send(frame.clone()).await.unwrap();
    }
    ws.send(Message::Text(r#"{"type":"finalize"}"#.into())).await.unwrap();
    for _ in 30..87 {
        ws.send(frame.clone()).await.unwrap();
    }
    ws.send(Message::Text(r#"{"type":"audio.done"}"#.into())).await.unwrap();
    let want: Vec<Value> = fixtures::load("finalize_json").steps.iter().map(|s| serde_json::from_str(&s.msg).unwrap()).collect();
    assert_eq!(all_texts(&mut ws).await, want);
    assert_eq!(fake.state.frames.load(SeqCst), 87);
}

#[tokio::test]
async fn synthetic_speech_closes_utterances_at_endpoints_and_at_a_finalize() {
    let fake = fake_stt::start(Config::default()).await;
    let (mut ws, _) = tokio_tungstenite::connect_async(fake.url.as_str()).await.unwrap();
    next_text(&mut ws).await.unwrap();
    for k in 0..60 {
        let bytes: Vec<u8> = speech::frame_pcm(k).iter().flat_map(|s| s.to_le_bytes()).collect();
        ws.send(Message::Binary(bytes.into())).await.unwrap();
    }
    ws.send(Message::Text(r#"{"type":"finalize"}"#.into())).await.unwrap();
    ws.send(Message::Text(r#"{"type":"audio.done"}"#.into())).await.unwrap();
    let msgs = all_texts(&mut ws).await;
    let finals: Vec<(String, f64)> = msgs
        .iter()
        .filter(|m| m["speech_final"] == true)
        .map(|m| (m["text"].as_str().unwrap().to_string(), m["start"].as_f64().unwrap() + m["duration"].as_f64().unwrap()))
        .collect();
    assert_eq!(finals.len(), 2);
    assert_eq!(finals[0].0, "w0 w1 w2 w3 w4 w5 w6 w7");
    assert!((finals[0].1 - 4.1).abs() < 1e-9);
    assert_eq!(finals[1].0, "w8 w9 w10");
    assert!((finals[1].1 - 6.0).abs() < 1e-9);
    assert_eq!(msgs.last().unwrap()["type"], "transcript.done");
    assert_eq!(msgs.last().unwrap()["duration"], 6.0);
}

#[tokio::test]
async fn a_refusal_answers_the_upgrade_with_its_status_and_body() {
    let fake = fake_stt::start(Config { refuse_all: Some(Refusal::Status(400)), ..Default::default() }).await;
    let err = tokio_tungstenite::connect_async(fake.url.as_str()).await.unwrap_err();
    let tungstenite::Error::Http(resp) = err else { panic!("expected an HTTP refusal, got {err}") };
    assert_eq!(resp.status(), 400);
    assert_eq!(resp.body().as_deref(), Some(BAD_KEY_BODY.as_bytes()));
    assert_eq!(fake.state.attempts.load(SeqCst), 1);
}
