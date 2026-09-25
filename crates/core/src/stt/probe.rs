use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

pub struct ProbeOptions {
    pub url: String,
    pub api_key: String,
    pub pcm: Vec<i16>,
    pub pace: Duration,
    pub finalize_after_frames: Option<usize>,
    pub finalize_message: String,
    pub log_path: PathBuf,
}

#[derive(Debug)]
pub struct ProbeSummary {
    pub frames_sent: usize,
    pub messages: usize,
    pub saw_created: bool,
    pub saw_done: bool,
}

fn frame_bytes(frame: &[i16]) -> Vec<u8> {
    frame.iter().flat_map(|s| s.to_le_bytes()).collect()
}

pub async fn probe(opts: ProbeOptions) -> Result<ProbeSummary> {
    let mut req = opts.url.as_str().into_client_request()?;
    req.headers_mut().insert("Authorization", format!("Bearer {}", opts.api_key).parse()?);
    let (ws, _) = tokio_tungstenite::connect_async(req).await.context("connect")?;
    let (mut tx, mut rx) = ws.split();
    let mut log = tokio::fs::File::create(&opts.log_path).await?;
    let started = Instant::now();
    let mut summary = ProbeSummary { frames_sent: 0, messages: 0, saw_created: false, saw_done: false };

    async fn write_line(log: &mut tokio::fs::File, started: Instant, dir: &str, msg: Value) -> Result<()> {
        let line = json!({ "t_ms": started.elapsed().as_millis() as u64, "dir": dir, "msg": msg });
        log.write_all(format!("{line}\n").as_bytes()).await?;
        Ok(())
    }

    // Wait for transcript.created before sending audio.
    loop {
        let Some(msg) = rx.next().await else { bail!("closed before transcript.created") };
        if let Message::Text(t) = msg? {
            let v: Value = serde_json::from_str(&t).unwrap_or(Value::String(t.to_string()));
            summary.messages += 1;
            let created = v["type"] == "transcript.created";
            write_line(&mut log, started, "in", v).await?;
            if created {
                summary.saw_created = true;
                break;
            }
        }
    }

    let reader = tokio::spawn(async move {
        let mut lines = Vec::new();
        while let Some(Ok(msg)) = rx.next().await {
            if let Message::Text(t) = msg {
                lines.push((Instant::now(), t.to_string()));
            }
        }
        lines
    });

    for (i, frame) in opts.pcm.chunks(1600).enumerate() {
        tx.send(Message::Binary(frame_bytes(frame).into())).await?;
        summary.frames_sent += 1;
        write_line(&mut log, started, "out", json!({ "binary_bytes": frame.len() * 2 })).await?;
        if opts.finalize_after_frames == Some(i + 1) {
            tx.send(Message::Text(opts.finalize_message.clone().into())).await?;
            write_line(&mut log, started, "out", Value::String(opts.finalize_message.clone())).await?;
        }
        if !opts.pace.is_zero() {
            tokio::time::sleep(opts.pace).await;
        }
    }
    tx.send(Message::Text("audio.done".into())).await?;
    write_line(&mut log, started, "out", Value::String("audio.done".into())).await?;

    let received = tokio::time::timeout(Duration::from_secs(30), reader).await.context("waiting for transcript.done")??;
    for (at, t) in received {
        let v: Value = serde_json::from_str(&t).unwrap_or(Value::String(t));
        summary.messages += 1;
        summary.saw_done |= v["type"] == "transcript.done";
        let line = json!({ "t_ms": at.duration_since(started).as_millis() as u64, "dir": "in", "msg": v });
        log.write_all(format!("{line}\n").as_bytes()).await?;
    }
    log.flush().await?;
    Ok(summary)
}
