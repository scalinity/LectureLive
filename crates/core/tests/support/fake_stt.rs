//! A fake Grok STT websocket (spec §11). It replays a recorded fixture in causal order, or plays
//! synthetic speech (`speech`), with injected disconnects, silences and refusals.
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::{http, Message};
use tokio_tungstenite::WebSocketStream;

use super::fixtures::Fixture;
use super::speech::{clip_start, Listener};

/// What the real server answers an unknown key with: HTTP 400 at the upgrade.
pub const BAD_KEY_BODY: &str = r#"{"code":"Client specified an invalid argument","error":"Incorrect API key provided. You can obtain an API key from https://console.x.ai."}"#;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    Status(u16),
    TcpClose,
}

/// The connection that hears `at_sample` vanishes (or goes silent, `freeze`). New connections are
/// then refused until the test's source has fed `refuse_for` more samples.
#[derive(Clone, Copy, Debug)]
pub struct Outage {
    pub at_sample: u64,
    pub refuse_for: u64,
    pub refusal: Refusal,
    pub freeze: bool,
}

#[derive(Default)]
pub enum Mode {
    #[default]
    Synthetic,
    Replay(Fixture),
}

#[derive(Default)]
pub struct Config {
    pub mode: Mode,
    pub outages: Vec<Outage>,
    pub refuse_all: Option<Refusal>,
    pub refuse_after_drop: Option<Refusal>,
    pub drop_on_finalize: bool,
    pub mute_after_audio_done: bool,
}

#[derive(Default)]
pub struct State {
    pub attempts: AtomicUsize,
    pub accepted: AtomicUsize,
    pub frames: AtomicUsize,
    texts: Mutex<Vec<String>>,
    /// End of the audio the test's source has produced, in samples.
    pub feed: AtomicU64,
    refuse_until: AtomicU64,
    dropped: AtomicUsize,
}

impl State {
    pub fn texts(&self) -> Vec<String> {
        self.texts.lock().unwrap().clone()
    }

    pub async fn wait_accepted(&self, n: usize) {
        wait(|| self.accepted.load(SeqCst) >= n, "connections").await
    }

    pub async fn wait_frames(&self, n: usize) {
        wait(|| self.frames.load(SeqCst) >= n, "frames").await
    }
}

async fn wait(done: impl Fn() -> bool, what: &str) {
    for _ in 0..5_000 {
        if done() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    panic!("the fake STT server never saw enough {what}");
}

pub struct FakeStt {
    pub url: String,
    pub state: Arc<State>,
}

pub async fn start(cfg: Config) -> FakeStt {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "ws://{}/v1/stt?model=grok-voice-transcribe-2.0&encoding=pcm&sample_rate=16000&interim_results=true&language=en",
        listener.local_addr().unwrap()
    );
    let state = Arc::new(State::default());
    let (cfg, st) = (Arc::new(cfg), state.clone());
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            let (cfg, st) = (cfg.clone(), st.clone());
            tokio::spawn(async move { serve(tcp, &cfg, &st).await });
        }
    });
    FakeStt { url, state }
}

fn refusal(cfg: &Config, st: &State) -> Option<Refusal> {
    let dropped = st.dropped.load(SeqCst);
    cfg.refuse_all
        .or(if dropped > 0 { cfg.refuse_after_drop } else { None })
        .or_else(|| (st.feed.load(SeqCst) < st.refuse_until.load(SeqCst)).then(|| cfg.outages[dropped - 1].refusal))
}

async fn serve(tcp: TcpStream, cfg: &Config, st: &State) {
    st.attempts.fetch_add(1, SeqCst);
    let refusal = refusal(cfg, st);
    if refusal == Some(Refusal::TcpClose) {
        return; // dropped before the handshake
    }
    let answer = |_: &Request, resp: Response| -> Result<Response, ErrorResponse> {
        let Some(Refusal::Status(code)) = refusal else { return Ok(resp) };
        let body = if code == 400 { BAD_KEY_BODY.to_string() } else { json!({"error": format!("status {code}")}).to_string() };
        Err(http::Response::builder().status(code).header("content-type", "application/json").body(Some(body)).unwrap())
    };
    let Ok(mut ws) = tokio_tungstenite::accept_hdr_async(tcp, answer).await else { return };
    st.accepted.fetch_add(1, SeqCst);
    let created = json!({"type": "transcript.created", "id": "00000000-0000-0000-0000-000000000000"});
    if ws.send(Message::Text(created.to_string().into())).await.is_err() {
        return;
    }
    match &cfg.mode {
        Mode::Replay(f) => replay(ws, f, st).await,
        Mode::Synthetic => synthetic(ws, cfg, st).await,
    }
}

async fn replay(mut ws: WebSocketStream<TcpStream>, f: &Fixture, st: &State) {
    let (mut frames, mut texts, mut next) = (0, 0, 0);
    loop {
        while let Some(step) = f.steps.get(next).filter(|s| s.frames <= frames && s.texts <= texts) {
            if ws.send(Message::Text(step.msg.clone().into())).await.is_err() {
                return;
            }
            next += 1;
        }
        if next == f.steps.len() {
            let _ = ws.close(None).await;
            return;
        }
        match ws.next().await {
            Some(Ok(Message::Binary(_))) => {
                frames += 1;
                st.frames.fetch_add(1, SeqCst);
            }
            Some(Ok(Message::Text(t))) => {
                texts += 1;
                st.texts.lock().unwrap().push(t.to_string());
            }
            Some(Ok(_)) => {}
            _ => return,
        }
    }
}

async fn synthetic(mut ws: WebSocketStream<TcpStream>, cfg: &Config, st: &State) {
    let mut ear = Listener::default();
    while let Some(Ok(msg)) = ws.next().await {
        let out: Vec<Value> = match msg {
            Message::Binary(b) => {
                st.frames.fetch_add(1, SeqCst);
                let pcm: Vec<i16> = b.chunks_exact(2).map(|x| i16::from_le_bytes([x[0], x[1]])).collect();
                let at = clip_start(&pcm);
                let fired = st.dropped.load(SeqCst);
                if let Some(o) = cfg.outages.get(fired).filter(|o| at + pcm.len() as u64 > o.at_sample) {
                    st.refuse_until.store(o.at_sample + o.refuse_for, SeqCst);
                    st.dropped.fetch_add(1, SeqCst);
                    if o.freeze {
                        let _held_open = ws;
                        return std::future::pending().await;
                    }
                    return; // gone mid-utterance, without a close frame
                }
                ear.frame(at, pcm.len() as u64)
            }
            Message::Text(t) => {
                st.texts.lock().unwrap().push(t.to_string());
                match serde_json::from_str::<Value>(&t) {
                    Err(e) => vec![json!({"type": "error", "message": format!("Invalid message: {e}")})],
                    Ok(v) if v["type"] == "finalize" => {
                        if cfg.drop_on_finalize {
                            return;
                        }
                        let at = ear.heard_to;
                        ear.close(at, true)
                    }
                    Ok(v) if v["type"] == "audio.done" => {
                        if cfg.mute_after_audio_done {
                            let _held_open = ws;
                            return std::future::pending().await;
                        }
                        let at = ear.heard_to;
                        let mut out = ear.close(at, false);
                        out.push(json!({"type": "transcript.done", "text": "", "words": [], "duration": ear.heard_secs()}));
                        for m in out {
                            let _ = ws.send(Message::Text(m.to_string().into())).await;
                        }
                        let _ = ws.close(None).await;
                        return;
                    }
                    Ok(_) => vec![],
                }
            }
            _ => vec![],
        };
        for m in out {
            if ws.send(Message::Text(m.to_string().into())).await.is_err() {
                return;
            }
        }
    }
}
