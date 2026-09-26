//! The notes model over SSE (spec §6.2): chat completions with `stream: true` and the usage chunk.
//! Plan research: reasoning deltas come before content, the finish chunk carries `finish_reason`,
//! and the cost arrives once, in a chunk of its own after it, only when `include_usage` is asked.
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use base64::Engine;
use futures_util::StreamExt;
use serde_json::{json, Value};

use crate::session::spend::{Spend, SpendKind};
use crate::stt::protocol::{is_refusal, refusal_message};

pub const CHAT_URL: &str = "https://api.x.ai/v1/chat/completions";
pub const MODEL: &str = "grok-4.7";
pub const TICKS_PER_USD: f64 = 1e10;
/// The Python CLI's fallback rates per token (input, cached, output), used only when a response reports tokens but no cost.
const USD_PER_TOKEN: (f64, f64, f64) = (2.20e-6, 0.55e-6, 6.60e-6);
/// Above this the whole request is billed at twice the rate.
const LONG_PROMPT_TOKENS: u64 = 200_000;

/// SSE framing: bytes in as they arrive, the data of each completed event out. A line is decoded
/// only once its newline has arrived, so a character split across reads is never broken.
#[derive(Debug, Default)]
pub struct SseParser {
    buf: Vec<u8>,
    data: Vec<String>,
}

impl SseParser {
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(bytes);
        let mut events = Vec::new();
        while let Some(i) = self.buf.iter().position(|&b| b == b'\n') {
            let raw: Vec<u8> = self.buf.drain(..=i).collect();
            let line = String::from_utf8_lossy(&raw[..raw.len() - 1]);
            let line = line.strip_suffix('\r').unwrap_or(&line);
            if line.is_empty() {
                if !self.data.is_empty() {
                    events.push(std::mem::take(&mut self.data).join("\n"));
                }
            } else if let Some(d) = line.strip_prefix("data:") {
                self.data.push(d.strip_prefix(' ').unwrap_or(d).to_string());
            } // comments (":…") and other fields carry nothing used here
        }
        events
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub cached_tokens: u64,
    pub completion_tokens: u64,
    pub ticks: Option<u64>,
}

impl Usage {
    fn from_json(u: &Value) -> Self {
        Self {
            prompt_tokens: u["prompt_tokens"].as_u64().unwrap_or(0),
            cached_tokens: u["prompt_tokens_details"]["cached_tokens"].as_u64().unwrap_or(0),
            completion_tokens: u["completion_tokens"].as_u64().unwrap_or(0),
            ticks: u["cost_in_usd_ticks"].as_u64(),
        }
    }

    /// What the request cost and whether that is the billed figure (the Python CLI's `chat_cost`).
    pub fn cost(&self) -> (f64, bool) {
        if let Some(t) = self.ticks {
            return (t as f64 / TICKS_PER_USD, true);
        }
        let (input, cached, output) = USD_PER_TOKEN;
        let usd = self.prompt_tokens.saturating_sub(self.cached_tokens) as f64 * input + self.cached_tokens as f64 * cached + self.completion_tokens as f64 * output;
        (if self.prompt_tokens > LONG_PROMPT_TOKENS { usd * 2.0 } else { usd }, false)
    }
}

/// The response as its events arrive.
#[derive(Debug, Default)]
pub struct Reply {
    pub text: String,
    pub finish: Option<String>,
    pub usage: Option<Usage>,
    /// An error the server sent inside the stream.
    pub error: Option<String>,
    /// The connection failed, stalled or timed out.
    pub broken: Option<String>,
    pub done: bool,
}

impl Reply {
    /// Applies one event's data; returns the content it adds. Role, reasoning and usage add none.
    pub fn apply(&mut self, data: &str) -> Option<String> {
        if data == "[DONE]" {
            self.done = true;
            return None;
        }
        let v: Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(e) => {
                self.error.get_or_insert(format!("an unreadable event: {e}"));
                return None;
            }
        };
        if let Some(e) = v.get("error").filter(|e| !e.is_null()) {
            self.error.get_or_insert(e.get("message").and_then(Value::as_str).map_or_else(|| e.to_string(), str::to_string));
            return None;
        }
        if let Some(u) = v.get("usage").filter(|u| !u.is_null()) {
            self.usage = Some(Usage::from_json(u));
        }
        let choice = v["choices"].get(0)?;
        if let Some(f) = choice["finish_reason"].as_str() {
            self.finish = Some(f.to_string());
        }
        let delta = choice["delta"]["content"].as_str().filter(|s| !s.is_empty())?;
        self.text.push_str(delta);
        Some(delta.to_string())
    }

    /// Spec §6.2: successful only with a normal finish reason and non-empty output.
    pub fn outcome(&self) -> Result<(), String> {
        if let Some(e) = &self.error {
            return Err(format!("the server reported an error: {e}"));
        }
        match self.finish.as_deref() {
            Some("stop") if !self.text.trim().is_empty() => Ok(()),
            Some("stop") => Err("the response was empty".into()),
            Some(other) => Err(format!("the response ended early (finish_reason {other:?})")),
            None => Err(match &self.broken {
                Some(e) => format!("the stream broke off before the response finished: {e}"),
                None => "the stream ended before the response finished".into(),
            }),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Image {
    pub mime: &'static str,
    pub base64: String,
}

impl Image {
    /// A slide as the Python CLI attaches it: JPEG or PNG by its extension.
    pub fn read(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
        let jpeg = path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("jpg") || e.eq_ignore_ascii_case("jpeg"));
        Ok(Self { mime: if jpeg { "image/jpeg" } else { "image/png" }, base64: base64::engine::general_purpose::STANDARD.encode(bytes) })
    }
}

#[derive(Debug, Clone)]
pub enum Content {
    Text(String),
    Parts { text: String, images: Vec<Image> },
}

#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub what: SpendKind,
    pub system: String,
    pub content: Content,
    pub effort: Option<&'static str>,
    pub timeout: Duration,
}

impl ChatRequest {
    pub fn body(&self, model: &str) -> Value {
        let content = match &self.content {
            Content::Text(t) => json!(t),
            Content::Parts { text, images } => {
                let mut parts = vec![json!({"type": "text", "text": text})];
                parts.extend(images.iter().map(|i| json!({"type": "image_url", "image_url": {"url": format!("data:{};base64,{}", i.mime, i.base64), "detail": "high"}})));
                Value::Array(parts)
            }
        };
        let mut body = json!({
            "model": model,
            "messages": [{"role": "system", "content": self.system}, {"role": "user", "content": content}],
            "stream": true,
            "stream_options": {"include_usage": true},
        });
        if let Some(e) = self.effort {
            body["reasoning_effort"] = json!(e);
        }
        body
    }
}

#[derive(Debug, Clone)]
pub struct ChatConfig {
    pub url: String,
    pub api_key: String,
    pub model: String,
    /// Longest silence inside a stream; reasoning deltas keep a healthy one talking.
    pub idle_timeout: Duration,
    /// Connect here for the URL's host (TLS still for that host): a check's forwarder (`net::API_ADDR_VAR`).
    pub connect_to: Option<std::net::SocketAddr>,
}

impl ChatConfig {
    pub fn new(api_key: String) -> Self {
        Self { url: CHAT_URL.into(), api_key, model: MODEL.into(), idle_timeout: Duration::from_secs(180), connect_to: crate::net::api_addr() }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ChatError {
    /// A 4xx: the key or a parameter is refused.
    Refused(String),
    Failed(String),
}

impl std::fmt::Display for ChatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(m) => write!(f, "refused: {m}"),
            Self::Failed(m) => f.write_str(m),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Answer {
    pub text: String,
    /// What the response reported it cost.
    pub usd: Option<f64>,
    /// The ledger could not be written (spec §10: a warning; the answer is kept).
    pub warning: Option<String>,
}

pub struct ChatClient {
    http: reqwest::Client,
    cfg: ChatConfig,
    spend: Option<Spend>,
}

impl ChatClient {
    pub fn new(cfg: ChatConfig, spend: Option<Spend>) -> Result<Self> {
        // reqwest is built without a TLS provider of its own; rustls uses ring (M2 Findings).
        let _ = rustls::crypto::ring::default_provider().install_default();
        let http = crate::net::resolved(reqwest::Client::builder(), &cfg.url, cfg.connect_to).build().context("build the HTTP client")?;
        Ok(Self { http, cfg, spend })
    }

    /// Streams one request; content deltas go to `on_delta` as they arrive. The cost a response
    /// reported is written to the ledger whether or not its text is used; a request that reported
    /// none records nothing.
    pub async fn complete(&self, req: &ChatRequest, on_delta: &mut (dyn FnMut(&str) + Send)) -> Result<Answer, ChatError> {
        let mut reply = Reply::default();
        let sent = match tokio::time::timeout(req.timeout, self.stream(req, &mut reply, on_delta)).await {
            Ok(r) => r,
            Err(_) => {
                reply.broken.get_or_insert(format!("no complete answer within {:?}", req.timeout));
                Ok(())
            }
        };
        let cost = reply.usage.as_ref().map(Usage::cost);
        let mut warning = None;
        if let (Some((usd, billed)), Some(spend)) = (cost, &self.spend) {
            if let Err(e) = spend.add(req.what, usd, billed, None) {
                warning = Some(format!("the spend ledger could not be written: {e:#}"));
            }
        }
        sent?;
        reply.outcome().map_err(ChatError::Failed)?;
        Ok(Answer { text: reply.text, usd: cost.map(|(usd, _)| usd), warning })
    }

    async fn stream(&self, req: &ChatRequest, reply: &mut Reply, on_delta: &mut (dyn FnMut(&str) + Send)) -> Result<(), ChatError> {
        let resp = self.http.post(&self.cfg.url).bearer_auth(&self.cfg.api_key).json(&req.body(&self.cfg.model)).send().await.map_err(|e| ChatError::Failed(e.to_string()))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            let msg = format!("{status}: {}", refusal_message(&body));
            return Err(if is_refusal(status.as_u16()) { ChatError::Refused(msg) } else { ChatError::Failed(msg) });
        }
        let mut parser = SseParser::default();
        let mut body = resp.bytes_stream();
        loop {
            match tokio::time::timeout(self.cfg.idle_timeout, body.next()).await {
                Err(_) => {
                    reply.broken = Some(format!("nothing arrived for {:?}", self.cfg.idle_timeout));
                    return Ok(());
                }
                Ok(None) => return Ok(()),
                Ok(Some(Err(e))) => {
                    reply.broken = Some(e.to_string());
                    return Ok(());
                }
                Ok(Some(Ok(bytes))) => {
                    for data in parser.feed(&bytes) {
                        if let Some(d) = reply.apply(&data) {
                            on_delta(&d);
                        }
                        if reply.done {
                            return Ok(());
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RECORDED: &[u8] = include_bytes!("../../tests/fixtures/notes/stream_image_medium.sse");

    fn replay(chunks: &[&[u8]]) -> (Reply, String) {
        let (mut parser, mut reply, mut preview) = (SseParser::default(), Reply::default(), String::new());
        for c in chunks {
            for data in parser.feed(c) {
                if let Some(d) = reply.apply(&data) {
                    preview += &d;
                }
            }
        }
        (reply, preview)
    }

    #[test]
    fn every_split_of_a_recorded_stream_gives_the_same_reply() {
        let (whole, preview) = replay(&[RECORDED]);
        assert_eq!((whole.finish.as_deref(), whole.done), (Some("stop"), true));
        assert_eq!(whole.usage.as_ref().and_then(|u| u.ticks), Some(14_780_000));
        assert_eq!(preview, whole.text, "only content reaches the preview");
        assert!(whole.outcome().is_ok());
        for at in 1..RECORDED.len() {
            let (r, p) = replay(&[&RECORDED[..at], &RECORDED[at..]]);
            assert_eq!((&r.text, &r.finish, &r.usage, &p), (&whole.text, &whole.finish, &whole.usage, &preview), "split at {at}");
        }
    }

    #[test]
    fn a_character_split_across_reads_is_whole_in_the_text() {
        let stream = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"é—𝛼 ∇\"}}]}\n\ndata: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n".as_bytes();
        for at in 1..stream.len() {
            let (r, _) = replay(&[&stream[..at], &stream[at..]]);
            assert_eq!(r.text, "é—𝛼 ∇", "split at {at}");
        }
    }

    #[test]
    fn several_events_in_one_read_crlf_lines_and_comments_are_framed() {
        let mut p = SseParser::default();
        assert_eq!(p.feed(b": keep-alive\n\ndata: a\r\n\r\ndata: b\n\ndata: c"), vec!["a".to_string(), "b".to_string()]);
        assert_eq!(p.feed(b"\n\n"), vec!["c".to_string()]);
        assert_eq!(p.feed(b"data: x\ndata: y\n\n"), vec!["x\ny".to_string()], "multi-line data joins with newlines");
    }

    #[test]
    fn non_content_deltas_never_reach_the_preview() {
        let mut r = Reply::default();
        assert_eq!(r.apply(r#"{"choices":[{"index":0,"delta":{"role":"assistant","reasoning_content":"Let me think"}}]}"#), None);
        assert_eq!(r.apply(r#"{"choices":[{"index":0,"delta":{"content":""}}]}"#), None);
        assert_eq!(r.apply(r#"{"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":2,"cost_in_usd_ticks":5000}}"#), None);
        assert_eq!(r.apply(r###"{"choices":[{"index":0,"delta":{"content":"## A"}}]}"###), Some("## A".to_string()));
        assert_eq!(r.text, "## A");
        assert_eq!(r.usage.as_ref().unwrap().cost(), (5e-7, true));
    }

    #[test]
    fn success_is_a_normal_finish_with_output() {
        let with = |text: &str, finish: Option<&str>, error: Option<&str>| Reply { text: text.into(), finish: finish.map(String::from), error: error.map(String::from), ..Reply::default() }.outcome();
        assert!(with("- a", Some("stop"), None).is_ok());
        assert!(with("  \n", Some("stop"), None).unwrap_err().contains("empty"));
        assert!(with("- a", Some("length"), None).unwrap_err().contains("length"));
        assert!(with("- a", Some("content_filter"), None).unwrap_err().contains("content_filter"));
        assert!(with("- a", None, None).unwrap_err().contains("before the response finished"));
        assert!(with("- a", Some("stop"), Some("overloaded")).unwrap_err().contains("overloaded"));
    }

    #[test]
    fn a_cost_without_ticks_is_computed_from_the_clis_rates() {
        let u = Usage { prompt_tokens: 1_000, cached_tokens: 200, completion_tokens: 100, ticks: None };
        let (usd, billed) = u.cost();
        assert!(!billed);
        assert!((usd - (800.0 * 2.20e-6 + 200.0 * 0.55e-6 + 100.0 * 6.60e-6)).abs() < 1e-12);
        let long = Usage { prompt_tokens: 200_001, cached_tokens: 0, completion_tokens: 0, ticks: None };
        assert!((long.cost().0 - 200_001.0 * 2.20e-6 * 2.0).abs() < 1e-9);
    }
}
