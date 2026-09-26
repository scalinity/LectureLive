//! The live STT connection (spec §5.1, §5.4): one worker per session writes every frame in order.
//! Each connection is an epoch whose server times count from the first frame sent on it.
use std::collections::VecDeque;
use std::time::Duration;

use anyhow::{ensure, Result};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{self, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use uuid::Uuid;

use super::protocol::{self, is_refusal, refusal_message, to_samples, Partial, ServerMsg, AUDIO_DONE, FINALIZE, SERVER_RESOLUTION};
use super::transcript::{Transcript, Update, Utterance};
use crate::audio::frame::{Frame, FRAME_SAMPLES};
use crate::session::sidecar::{Gap, GapKind};

pub const STT_URL: &str = "wss://api.x.ai/v1/stt?model=grok-voice-transcribe-2.0&encoding=pcm&sample_rate=16000&interim_results=true&language=en";
/// Frames and control messages between the coordinator and the writer.
pub const INPUT_QUEUE: usize = 64;
const MAX_KEYTERMS: usize = 100;
const MAX_KEYTERM_CHARS: usize = 50;
/// Frames held while connecting (5 s): a short outage is streamed late instead of recovered.
const HOLD_FRAMES: usize = 50;
const MAX_BACKOFF_UNITS: u32 = 30;

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(Debug, Clone)]
pub struct SttConfig {
    pub url: String,
    pub api_key: String,
    pub keyterms: Vec<String>,
    /// Reconnect waits are 1, 2, 4 … 30 of these.
    pub backoff_unit: Duration,
    /// Through `transcript.created`.
    pub connect_timeout: Duration,
    /// One frame's send; longer means the connection is stuck.
    pub send_timeout: Duration,
    /// The server speaks every second or two while audio flows: this long without a message means the connection is dead.
    pub idle_timeout: Duration,
    /// How long a cutoff waits for the final that settles it (spec §5.3).
    pub finalize_wait: Duration,
    /// After `audio.done`, the wait for `transcript.done`.
    pub done_wait: Duration,
    /// Connect here for the URL's host (TLS still for that host): a check's forwarder (`net::API_ADDR_VAR`).
    pub connect_to: Option<std::net::SocketAddr>,
}

impl SttConfig {
    pub fn new(api_key: String, keyterms: Vec<String>) -> Self {
        Self {
            url: STT_URL.into(),
            api_key,
            keyterms,
            backoff_unit: Duration::from_secs(1),
            connect_timeout: Duration::from_secs(10),
            send_timeout: Duration::from_secs(5),
            idle_timeout: Duration::from_secs(5),
            finalize_wait: Duration::from_secs(3),
            done_wait: Duration::from_secs(5),
            connect_to: crate::net::api_addr(),
        }
    }

    /// The connection URL: every keyterm validated, then encoded as its own `keyterm` parameter.
    pub fn request_url(&self) -> Result<String> {
        validate_keyterms(&self.keyterms)?;
        if self.keyterms.is_empty() {
            return Ok(self.url.clone());
        }
        let mut url = reqwest::Url::parse(&self.url)?;
        url.query_pairs_mut().extend_pairs(self.keyterms.iter().map(|k| ("keyterm", k.trim())));
        Ok(url.into())
    }
}

pub fn validate_keyterms(terms: &[String]) -> Result<()> {
    ensure!(terms.len() <= MAX_KEYTERMS, "at most {MAX_KEYTERMS} keyterms, {} given", terms.len());
    for t in terms {
        let n = t.trim().chars().count();
        ensure!((1..=MAX_KEYTERM_CHARS).contains(&n), "keyterm {t:?} must have 1 to {MAX_KEYTERM_CHARS} characters");
    }
    Ok(())
}

#[derive(Debug)]
pub enum SttInput {
    Begin { recording_id: Uuid },
    Frame(Frame),
    End { recording_id: Uuid, samples: u64 },
    /// Finalize after the frames through `sample`, the end of the last frame forwarded before this (spec §5.3).
    Cutoff { id: u64, recording_id: Uuid, sample: u64 },
}

#[derive(Debug, Clone, PartialEq)]
pub enum SttEvent {
    /// A connection began streaming the recording at `origin`; `gap` is the audio before it that no connection transcribed.
    Connected { recording_id: Uuid, origin: u64, gap: Option<Gap> },
    /// The open utterance changed (display only).
    Open { recording_id: Uuid, stable: String, tentative: String },
    /// A closed utterance with text: commit it.
    Utterance { recording_id: Uuid, utterance: Utterance },
    /// The audio this recording's connections carried, for the spend ledger (spec §8); sent just before `Ended`.
    Streamed { recording_id: Uuid, samples: u64 },
    /// The recording's live transcript is finished; `gap` is its tail that no connection transcribed.
    Ended { recording_id: Uuid, gap: Option<Gap> },
    /// The cutoff is settled: confirmed when the recording's transcript is committed through it.
    Cutoff { id: u64, confirmed: bool },
    Retrying { after: Duration, reason: String },
    /// An `error` message; the connection carries on.
    ServerError(String),
    /// A 4xx at the upgrade: no further connections this session.
    Refused(String),
}

pub struct SttLink {
    pub input: mpsc::Sender<SttInput>,
    pub events: mpsc::Receiver<SttEvent>,
}

/// Starts the session's STT worker. A bad keyterm fails here, before any recording starts.
pub fn spawn(cfg: SttConfig) -> Result<SttLink> {
    let url = cfg.request_url()?;
    let (input, rx) = mpsc::channel(INPUT_QUEUE);
    let (tx, events) = mpsc::channel(256);
    let worker = Worker { cfg, url, events: tx, rec: None, epoch: None, connecting: None, retry_at: None, failures: 0, refused: false };
    tokio::spawn(worker.run(rx));
    Ok(SttLink { input, events })
}

enum ConnectError {
    Refused(String),
    Transient(String),
}

struct Epoch {
    ws: Ws,
    /// The recording sample of the first frame sent: server time 0.
    origin: Option<u64>,
    transcript: Transcript,
    sent_to: u64,
    heard_at: Instant,
    cutoffs: Vec<PendingCutoff>,
}

struct PendingCutoff {
    id: u64,
    sample: u64,
    deadline: Instant,
}

struct Rec {
    id: Uuid,
    /// The transcript is settled up to here: closed utterances, or audio a reported gap covers.
    settled: u64,
    /// The offset the next frame should have; another one means frames were dropped before the writer.
    next: u64,
    received_to: u64,
    held: VecDeque<Frame>,
    /// Why the audio after `settled` may lack a transcript: the kind of the gap that reports it.
    cause: Option<GapKind>,
    /// Audio sent on this recording's connections.
    streamed: u64,
}

struct Worker {
    cfg: SttConfig,
    url: String,
    events: mpsc::Sender<SttEvent>,
    rec: Option<Rec>,
    epoch: Option<Epoch>,
    connecting: Option<JoinHandle<Result<Ws, ConnectError>>>,
    retry_at: Option<Instant>,
    failures: u32,
    refused: bool,
}

impl Worker {
    async fn run(mut self, mut input: mpsc::Receiver<SttInput>) {
        loop {
            let (retry, idle, cutoff) = (self.retry_at, self.idle_deadline(), self.cutoff_deadline());
            tokio::select! {
                i = input.recv() => match i {
                    Some(i) => self.on_input(i).await,
                    None => break,
                },
                m = next_message(&mut self.epoch) => self.on_message(m).await,
                r = connected(&mut self.connecting) => {
                    self.connecting = None;
                    self.on_connect(r).await;
                }
                _ = sleep_until(retry) => {
                    self.retry_at = None;
                    self.connect();
                }
                _ = sleep_until(idle) => self.lost(GapKind::SttOffline, format!("no message from the server for {:?}", self.cfg.idle_timeout)).await,
                _ = sleep_until(cutoff) => self.expire_cutoffs().await,
            }
        }
        if let Some((id, to)) = self.rec.as_ref().map(|r| (r.id, r.received_to)) {
            self.end(id, to).await;
        }
    }

    fn idle_deadline(&self) -> Option<Instant> {
        self.epoch.as_ref().filter(|e| e.origin.is_some()).map(|e| e.heard_at + self.cfg.idle_timeout)
    }

    async fn emit(&self, e: SttEvent) {
        let _ = self.events.send(e).await;
    }

    async fn on_input(&mut self, i: SttInput) {
        match i {
            SttInput::Begin { recording_id } => {
                let cause = self.refused.then_some(GapKind::SttRefused);
                self.rec = Some(Rec { id: recording_id, settled: 0, next: 0, received_to: 0, held: VecDeque::new(), cause, streamed: 0 });
                self.failures = 0;
                self.connect();
            }
            SttInput::Frame(f) => self.on_frame(f).await,
            SttInput::End { recording_id, samples } => {
                if self.rec.as_ref().is_some_and(|r| r.id == recording_id) {
                    self.end(recording_id, samples).await;
                }
            }
            SttInput::Cutoff { id, recording_id, sample } => self.cutoff(id, recording_id, sample).await,
        }
    }

    fn connect(&mut self) {
        if self.refused || self.rec.is_none() || self.epoch.is_some() || self.connecting.is_some() {
            return;
        }
        let (url, key, limit, to) = (self.url.clone(), self.cfg.api_key.clone(), self.cfg.connect_timeout, self.cfg.connect_to);
        self.connecting = Some(tokio::spawn(async move {
            match tokio::time::timeout(limit, open(&url, &key, to)).await {
                Ok(r) => r,
                Err(_) => Err(ConnectError::Transient(format!("no transcript.created within {limit:?}"))),
            }
        }));
    }

    async fn on_connect(&mut self, r: Result<Ws, ConnectError>) {
        match r {
            Ok(ws) => {
                self.failures = 0;
                if self.rec.is_none() {
                    return;
                }
                self.epoch = Some(Epoch { ws, origin: None, transcript: Transcript::new(0), sent_to: 0, heard_at: Instant::now(), cutoffs: Vec::new() });
                let held: Vec<Frame> = self.rec.as_mut().map(|r| r.held.drain(..).collect()).unwrap_or_default();
                for f in held {
                    if !self.send(f).await {
                        break;
                    }
                }
            }
            Err(ConnectError::Refused(msg)) => {
                self.refused = true;
                if let Some(r) = self.rec.as_mut() {
                    r.cause = Some(GapKind::SttRefused);
                    r.held.clear();
                }
                self.emit(SttEvent::Refused(msg)).await;
            }
            Err(ConnectError::Transient(reason)) => self.retry(reason).await,
        }
    }

    async fn retry(&mut self, reason: String) {
        let units = 1u32.checked_shl(self.failures).unwrap_or(u32::MAX).min(MAX_BACKOFF_UNITS);
        self.failures = self.failures.saturating_add(1);
        let after = self.cfg.backoff_unit * units;
        self.retry_at = Some(Instant::now() + after);
        self.emit(SttEvent::Retrying { after, reason }).await;
    }

    async fn on_frame(&mut self, f: Frame) {
        let Some(rec) = self.rec.as_mut().filter(|r| r.id == f.recording_id) else { return };
        let dropped = f.sample_offset != rec.next;
        rec.next = f.sample_offset + FRAME_SAMPLES as u64;
        rec.received_to = f.sample_offset + f.valid_samples as u64;
        if dropped {
            rec.held.clear(); // what is held must stay contiguous
            rec.cause.get_or_insert(GapKind::SttOverflow);
            if self.epoch.is_some() {
                self.lost(GapKind::SttOverflow, format!("frames before sample {} did not reach the writer", f.sample_offset)).await;
            }
        }
        if self.refused {
            return;
        }
        if self.epoch.is_some() {
            self.send(f).await;
            return;
        }
        let Some(rec) = self.rec.as_mut() else { return };
        if rec.held.len() == HOLD_FRAMES {
            rec.held.pop_front();
        }
        rec.held.push_back(f);
    }

    /// Sends one frame on the live connection; the first fixes the epoch's origin. False if the connection was lost.
    async fn send(&mut self, f: Frame) -> bool {
        let (Some(rec), Some(epoch)) = (self.rec.as_mut(), self.epoch.as_mut()) else { return false };
        let mut started = None;
        if epoch.origin.is_none() {
            let origin = f.sample_offset;
            let gap = (origin > rec.settled).then(|| Gap::new(rec.id, rec.settled, Some(origin), rec.cause.unwrap_or(GapKind::SttOffline)));
            rec.cause = None;
            rec.settled = rec.settled.max(origin);
            epoch.origin = Some(origin);
            epoch.transcript = Transcript::new(origin);
            epoch.heard_at = Instant::now();
            started = Some(SttEvent::Connected { recording_id: rec.id, origin, gap });
        }
        let bytes: Vec<u8> = f.pcm().iter().flat_map(|s| s.to_le_bytes()).collect();
        let sent = tokio::time::timeout(self.cfg.send_timeout, epoch.ws.send(Message::Binary(bytes.into()))).await;
        if matches!(sent, Ok(Ok(()))) {
            epoch.sent_to = f.sample_offset + f.valid_samples as u64;
            rec.streamed += f.valid_samples as u64;
        }
        if let Some(e) = started {
            self.emit(e).await;
        }
        match sent {
            Ok(Ok(())) => true,
            Ok(Err(e)) => {
                self.lost(GapKind::SttOffline, e.to_string()).await;
                false
            }
            Err(_) => {
                self.lost(GapKind::SttOffline, format!("a frame took over {:?} to send", self.cfg.send_timeout)).await;
                false
            }
        }
    }

    /// The connection is gone: what it had not closed waits for the next gap, and so do its cutoffs.
    async fn lost(&mut self, cause: GapKind, reason: String) {
        let Some(epoch) = self.epoch.take() else { return };
        if let Some(r) = self.rec.as_mut() {
            r.cause.get_or_insert(cause);
        }
        for c in epoch.cutoffs {
            self.emit(SttEvent::Cutoff { id: c.id, confirmed: false }).await;
        }
        self.retry(reason).await;
    }

    async fn on_message(&mut self, m: Option<Result<Message, tungstenite::Error>>) {
        if let Some(e) = self.epoch.as_mut() {
            e.heard_at = Instant::now();
        }
        match m {
            Some(Ok(Message::Text(t))) => match protocol::parse(&t) {
                Ok(ServerMsg::Partial(p)) => self.on_partial(&p).await,
                Ok(ServerMsg::Error { message }) => self.emit(SttEvent::ServerError(message)).await,
                Ok(_) => {}
                Err(e) => self.emit(SttEvent::ServerError(format!("unreadable server message: {e}"))).await,
            },
            Some(Ok(Message::Close(_))) | None => self.lost(GapKind::SttOffline, "the server closed the connection".into()).await,
            Some(Err(e)) => self.lost(GapKind::SttOffline, e.to_string()).await,
            Some(Ok(_)) => {}
        }
    }

    async fn on_partial(&mut self, p: &Partial) {
        let (Some(rec), Some(epoch)) = (self.rec.as_mut(), self.epoch.as_mut()) else { return };
        if epoch.origin.is_none() {
            return;
        }
        let Some(update) = epoch.transcript.apply(p) else { return };
        let recording_id = rec.id;
        let mut out = Vec::new();
        match update {
            Update::Open { stable, tentative } => out.push(SttEvent::Open { recording_id, stable, tentative }),
            Update::Closed(u) => {
                rec.settled = rec.settled.max(u.end_sample);
                let settled = rec.settled;
                out.push(if u.text.is_empty() {
                    SttEvent::Open { recording_id, stable: String::new(), tentative: String::new() }
                } else {
                    SttEvent::Utterance { recording_id, utterance: u }
                });
                // A cutoff is confirmed by the close that settles the recording through it.
                epoch.cutoffs.retain(|c| {
                    let done = c.sample <= settled + SERVER_RESOLUTION;
                    if done {
                        out.push(SttEvent::Cutoff { id: c.id, confirmed: true });
                    }
                    !done
                });
            }
        }
        for e in out {
            self.emit(e).await;
        }
    }

    fn cutoff_deadline(&self) -> Option<Instant> {
        self.epoch.as_ref().and_then(|e| e.cutoffs.iter().map(|c| c.deadline).min())
    }

    async fn cutoff(&mut self, id: u64, recording_id: Uuid, sample: u64) {
        let settled = match self.rec.as_ref().filter(|r| r.id == recording_id) {
            Some(r) => r.settled,
            None => return self.emit(SttEvent::Cutoff { id, confirmed: false }).await,
        };
        if sample <= settled + SERVER_RESOLUTION {
            return self.emit(SttEvent::Cutoff { id, confirmed: true }).await;
        }
        let (deadline, send_timeout) = (Instant::now() + self.cfg.finalize_wait, self.cfg.send_timeout);
        let Some(epoch) = self.epoch.as_mut().filter(|e| e.origin.is_some() && e.sent_to >= sample) else {
            // The cutoff's audio is not on this connection: it is left to a gap.
            return self.emit(SttEvent::Cutoff { id, confirmed: false }).await;
        };
        epoch.cutoffs.push(PendingCutoff { id, sample, deadline });
        let sent = tokio::time::timeout(send_timeout, epoch.ws.send(Message::Text(FINALIZE.into()))).await;
        if !matches!(sent, Ok(Ok(()))) {
            self.lost(GapKind::SttOffline, "the finalize could not be sent".into()).await;
        }
    }

    async fn expire_cutoffs(&mut self) {
        let now = Instant::now();
        let Some(epoch) = self.epoch.as_mut() else { return };
        let mut expired = Vec::new();
        epoch.cutoffs.retain(|c| {
            if c.deadline <= now {
                expired.push(c.id);
            }
            c.deadline > now
        });
        for id in expired {
            self.emit(SttEvent::Cutoff { id, confirmed: false }).await;
        }
    }

    /// The recording ended: flush what the server holds, then report the tail no connection transcribed.
    async fn end(&mut self, recording_id: Uuid, samples: u64) {
        if let Some(h) = self.connecting.take() {
            h.abort();
        }
        self.retry_at = None;
        if self.epoch.as_ref().is_some_and(|e| e.origin.is_some()) {
            self.finish().await;
        }
        if let Some(epoch) = self.epoch.take() {
            let settled = self.rec.as_ref().map_or(0, |r| r.settled);
            for c in epoch.cutoffs {
                self.emit(SttEvent::Cutoff { id: c.id, confirmed: c.sample <= settled + SERVER_RESOLUTION }).await;
            }
        }
        let Some(rec) = self.rec.take() else { return };
        if rec.streamed > 0 {
            self.emit(SttEvent::Streamed { recording_id, samples: rec.streamed }).await;
        }
        let cause = rec.cause.unwrap_or(if samples > rec.received_to { GapKind::SttOverflow } else { GapKind::SttOffline });
        let gap = (samples > rec.settled + SERVER_RESOLUTION).then(|| Gap::new(recording_id, rec.settled, Some(samples), cause));
        self.emit(SttEvent::Ended { recording_id, gap }).await;
    }

    /// Sends `audio.done` and reads the flushed finals until `transcript.done`.
    async fn finish(&mut self) {
        let deadline = Instant::now() + self.cfg.done_wait;
        let Some(epoch) = self.epoch.as_mut() else { return };
        if !matches!(tokio::time::timeout(self.cfg.send_timeout, epoch.ws.send(Message::Text(AUDIO_DONE.into()))).await, Ok(Ok(()))) {
            return;
        }
        loop {
            let Ok(m) = tokio::time::timeout_at(deadline, next_message(&mut self.epoch)).await else { return };
            match m {
                Some(Ok(Message::Text(t))) => match protocol::parse(&t) {
                    Ok(ServerMsg::Done { duration }) => {
                        let origin = self.epoch.as_ref().and_then(|e| e.origin);
                        if let (Some(rec), Some(origin)) = (self.rec.as_mut(), origin) {
                            rec.settled = rec.settled.max(origin + to_samples(duration));
                        }
                        return;
                    }
                    Ok(ServerMsg::Partial(p)) => self.on_partial(&p).await,
                    Ok(ServerMsg::Error { message }) => self.emit(SttEvent::ServerError(message)).await,
                    _ => {}
                },
                Some(Ok(_)) => {}
                _ => return, // closed or failed before transcript.done: the tail becomes a gap
            }
        }
    }
}

async fn open(url: &str, key: &str, connect_to: Option<std::net::SocketAddr>) -> Result<Ws, ConnectError> {
    let mut req = url.into_client_request().map_err(|e| ConnectError::Refused(format!("bad STT URL: {e}")))?;
    let auth = format!("Bearer {key}").parse().map_err(|_| ConnectError::Refused("the API key is not a valid header value".into()))?;
    req.headers_mut().insert("Authorization", auth);
    let connected = match connect_to {
        // A forwarder in between: TCP to it, TLS and the upgrade for the URL's host.
        Some(addr) => match TcpStream::connect(addr).await {
            Ok(tcp) => tokio_tungstenite::client_async_tls_with_config(req, tcp, None, None).await,
            Err(e) => return Err(ConnectError::Transient(e.to_string())),
        },
        None => tokio_tungstenite::connect_async(req).await,
    };
    let mut ws = match connected {
        Ok((ws, _)) => ws,
        Err(tungstenite::Error::Http(resp)) => {
            let status = resp.status();
            let body = resp.body().as_deref().map(String::from_utf8_lossy).unwrap_or_default();
            let msg = format!("{status}: {}", refusal_message(&body));
            return Err(if is_refusal(status.as_u16()) { ConnectError::Refused(msg) } else { ConnectError::Transient(msg) });
        }
        Err(e) => return Err(ConnectError::Transient(e.to_string())),
    };
    while let Some(m) = ws.next().await {
        match m {
            Ok(Message::Text(t)) => {
                if let Ok(ServerMsg::Created) = protocol::parse(&t) {
                    return Ok(ws);
                }
            }
            Ok(_) => {}
            Err(e) => return Err(ConnectError::Transient(e.to_string())),
        }
    }
    Err(ConnectError::Transient("closed before transcript.created".into()))
}

async fn next_message(epoch: &mut Option<Epoch>) -> Option<Result<Message, tungstenite::Error>> {
    match epoch {
        Some(e) => e.ws.next().await,
        None => std::future::pending().await,
    }
}

async fn connected(h: &mut Option<JoinHandle<Result<Ws, ConnectError>>>) -> Result<Ws, ConnectError> {
    match h {
        Some(h) => h.await.unwrap_or_else(|e| Err(ConnectError::Transient(format!("the connect task failed: {e}")))),
        None => std::future::pending().await,
    }
}

async fn sleep_until(at: Option<Instant>) {
    match at {
        Some(t) => tokio::time::sleep_until(t).await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(terms: Vec<String>) -> SttConfig {
        SttConfig::new("k".into(), terms)
    }

    #[test]
    fn keyterms_are_validated_and_encoded_one_by_one() {
        let url = cfg(vec!["gradient descent".into(), "Q&A".into(), "Softmax–Temperatur".into()]).request_url().unwrap();
        assert!(url.starts_with(STT_URL), "{url}");
        assert!(url.ends_with("&keyterm=gradient+descent&keyterm=Q%26A&keyterm=Softmax%E2%80%93Temperatur"), "{url}");
        assert_eq!(cfg(vec![]).request_url().unwrap(), STT_URL);
        assert!(cfg(vec!["x".repeat(51)]).request_url().is_err());
        assert!(cfg(vec!["ü".repeat(50)]).request_url().is_ok(), "the limit counts characters, not bytes");
        assert!(cfg((0..101).map(|i| format!("t{i}")).collect()).request_url().is_err());
        assert!(cfg(vec!["  ".into()]).request_url().is_err());
    }
}
