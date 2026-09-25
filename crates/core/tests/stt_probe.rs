use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use lecturelive_core::stt::probe::{probe, ProbeOptions};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use tokio_tungstenite::tungstenite::Message;

#[tokio::test]
async fn probe_sends_paced_pcm_frames_and_logs_every_message() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut auth = None;
        let mut ws = tokio_tungstenite::accept_hdr_async(tcp, |req: &Request, resp: Response| {
            auth = req.headers().get("authorization").map(|v| v.to_str().unwrap().to_string());
            Ok(resp)
        })
        .await
        .unwrap();
        ws.send(Message::Text(r#"{"type":"transcript.created"}"#.into())).await.unwrap();
        let (mut sizes, mut texts) = (Vec::new(), Vec::new());
        while let Some(Ok(msg)) = ws.next().await {
            match msg {
                Message::Binary(b) => {
                    sizes.push(b.len());
                    if sizes.len() == 2 {
                        ws.send(Message::Text(r#"{"type":"transcript.partial","transcript":"hi","is_final":false,"speech_final":false,"words":[]}"#.into())).await.unwrap();
                    }
                }
                Message::Text(t) => {
                    let t = t.to_string();
                    texts.push(t.clone());
                    if t == r#"{"type":"audio.done"}"# {
                        ws.send(Message::Text(r#"{"type":"transcript.done","transcript":"hi","words":[]}"#.into())).await.unwrap();
                        ws.close(None).await.ok();
                        break;
                    }
                }
                _ => {}
            }
        }
        (auth, sizes, texts)
    });

    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("probe.jsonl");
    let summary = probe(ProbeOptions {
        url: format!("ws://{addr}/v1/stt"),
        api_key: "test-key".into(),
        pcm: vec![0i16; 1600 * 3 + 100],
        pace: Duration::ZERO,
        finalize_after_frames: Some(2),
        finalize_message: r#"{"type":"finalize"}"#.into(),
        log_path: log.clone(),
    })
    .await
    .unwrap();

    let (auth, sizes, texts) = server.await.unwrap();
    assert_eq!(auth.as_deref(), Some("Bearer test-key"));
    assert_eq!(sizes, vec![3200, 3200, 3200, 200]); // little-endian PCM16, last frame partial
    assert_eq!(texts, vec![r#"{"type":"finalize"}"#.to_string(), r#"{"type":"audio.done"}"#.to_string()]);
    assert!(summary.saw_created && summary.saw_done);
    assert_eq!(summary.frames_sent, 4);
    let lines = std::fs::read_to_string(&log).unwrap();
    assert!(lines.lines().count() >= 5, "log:\n{lines}");
    assert!(lines.contains("transcript.partial"));
}
