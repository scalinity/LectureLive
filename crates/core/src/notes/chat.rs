//! The notes model over SSE (spec §6.2): the Claude Messages API with `stream: true`, Opus 5.5 at high
//! effort with adaptive thinking. Thinking deltas come before the text, the stop reason arrives in
//! `message_delta`, and usage is tokens only (no price), so every cost here is computed.
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use base64::Engine;
use futures_util::StreamExt;
use serde_json::{json, Value};

use crate::session::spend::{Spend, SpendKind};
use crate::stt::protocol::is_refusal;

pub const CHAT_URL: &str = "https://api.anthropic.com/v1/messages";
pub const MODEL: &str = "claude-opus-5-5";
/// Every request runs at this effort unless it names another.
pub const EFFORT: &str = "high";
/// Thinking counts toward this; streaming keeps a large ceiling clear of HTTP timeouts.
const MAX_TOKENS: u32 = 64_000;
const API_VERSION: &str = "2023-06-01";
/// A request the safety classifiers decline is re-run on Anthropic's recommended fallback model, inside the same call.
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
/// Claude Opus 5.5 list prices per token (input, cache read, output).
const USD_PER_TOKEN: (f64, f64, f64) = (4.0e-6, 0.2e-6, 20.0e-6);

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
    /// Everything sent, cached or not.
    pub prompt_tokens: u64,
    pub cached_tokens: u64,
    /// Thinking included: it is billed as output.
    pub completion_tokens: u64,
}

impl Usage {
    /// What the request cost; Anthropic reports tokens, not a price, so the figure is never the billed one.
    pub fn cost(&self) -> (f64, bool) {
        let (input, cached, output) = USD_PER_TOKEN;
        (self.prompt_tokens.saturating_sub(self.cached_tokens) as f64 * input + self.cached_tokens as f64 * cached + self.completion_tokens as f64 * output, false)
    }
}

/// The response as its events arrive.
#[derive(Debug, Default)]
pub struct Reply {
    pub text: String,
    pub finish: Option<String>,
    /// Why a refusal was given (`stop_details.category`).
    pub category: Option<String>,
    pub usage: Option<Usage>,
    /// An error the server sent inside the stream.
    pub error: Option<String>,
    /// The connection failed, stalled or timed out.
    pub broken: Option<String>,
    pub done: bool,
}

/// What one event adds that can be shown at once.
#[derive(Debug, Clone, PartialEq)]
pub enum Delta {
    /// A piece of the answer.
    Text(String),
    /// A piece of the model's summary of its thinking: shown while it works, and never part of the answer.
    Thinking(String),
}

impl Reply {
    /// Applies one event's data; returns what it adds to show. Pings and usage add nothing.
    pub fn apply(&mut self, data: &str) -> Option<Delta> {
        let v: Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(e) => {
                self.error.get_or_insert(format!("an unreadable event: {e}"));
                return None;
            }
        };
        match v["type"].as_str()? {
            "error" => {
                let e = &v["error"];
                self.error.get_or_insert(e["message"].as_str().map_or_else(|| e.to_string(), str::to_string));
            }
            "message_start" => self.add_usage(&v["message"]["usage"]),
            "message_delta" => {
                if let Some(reason) = v["delta"]["stop_reason"].as_str() {
                    self.finish = Some(reason.to_string());
                    self.category = v["delta"]["stop_details"]["category"].as_str().map(str::to_string);
                }
                self.add_usage(&v["usage"]);
            }
            // The model that declined wrote part of an answer; the fallback model's answer replaces it.
            "content_block_start" if v["content_block"]["type"] == "fallback" => self.text.clear(),
            "content_block_delta" if v["delta"]["type"] == "text_delta" => {
                let delta = v["delta"]["text"].as_str().filter(|s| !s.is_empty())?;
                self.text.push_str(delta);
                return Some(Delta::Text(delta.to_string()));
            }
            "content_block_delta" if v["delta"]["type"] == "thinking_delta" => {
                return Some(Delta::Thinking(v["delta"]["thinking"].as_str().filter(|s| !s.is_empty())?.to_string()));
            }
            "message_stop" => self.done = true,
            _ => {}
        }
        None
    }

    /// Usage arrives twice, at the start and in `message_delta`, and the later figures are the running totals.
    fn add_usage(&mut self, u: &Value) {
        if u.is_null() {
            return;
        }
        let usage = self.usage.get_or_insert(Usage { prompt_tokens: 0, cached_tokens: 0, completion_tokens: 0 });
        if let Some(input) = u["input_tokens"].as_u64() {
            let (read, written) = (u["cache_read_input_tokens"].as_u64().unwrap_or(0), u["cache_creation_input_tokens"].as_u64().unwrap_or(0));
            usage.prompt_tokens = input + read + written;
            usage.cached_tokens = read;
        }
        if let Some(output) = u["output_tokens"].as_u64() {
            usage.completion_tokens = output;
        }
    }

    /// Spec §6.2: successful only with a normal stop and non-empty output.
    pub fn outcome(&self) -> Result<(), String> {
        if let Some(e) = &self.error {
            return Err(format!("the server reported an error: {e}"));
        }
        match self.finish.as_deref() {
            Some("end_turn" | "stop_sequence") if !self.text.trim().is_empty() => Ok(()),
            Some("end_turn" | "stop_sequence") => Err("the response was empty".into()),
            Some("refusal") => Err(format!("Claude declined to answer{}", self.category.as_deref().map(|c| format!(" ({c})")).unwrap_or_default())),
            Some(other) => Err(format!("the response ended early (stop_reason {other:?})")),
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
    /// Images go before the text they are about, as Anthropic recommends.
    pub fn body(&self, model: &str) -> Value {
        let content = match &self.content {
            Content::Text(t) => json!(t),
            Content::Parts { text, images } => {
                let mut parts: Vec<Value> = images.iter().map(|i| json!({"type": "image", "source": {"type": "base64", "media_type": i.mime, "data": i.base64}})).collect();
                parts.push(json!({"type": "text", "text": text}));
                Value::Array(parts)
            }
        };
        json!({
            "model": model,
            "max_tokens": MAX_TOKENS,
            "stream": true,
            "thinking": {"type": "adaptive", "display": "summarized"},
            "output_config": {"effort": self.effort.unwrap_or(EFFORT)},
            "fallbacks": "default",
            "system": self.system,
            "messages": [{"role": "user", "content": content}],
        })
    }
}

#[derive(Debug, Clone)]
pub struct ChatConfig {
    pub url: String,
    pub api_key: String,
    pub model: String,
    /// Longest silence inside a stream; pings and thinking deltas keep a healthy one talking.
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
        self.complete_with_thinking(req, on_delta, &mut |_| {}).await
    }

    /// As `complete`, and the pieces of the model's summary of its thinking go to `on_thinking` as they arrive: what
    /// can be shown while the model works and no word of the answer has streamed.
    pub async fn complete_with_thinking(&self, req: &ChatRequest, on_delta: &mut (dyn FnMut(&str) + Send), on_thinking: &mut (dyn FnMut(&str) + Send)) -> Result<Answer, ChatError> {
        let mut reply = Reply::default();
        let sent = match tokio::time::timeout(req.timeout, self.stream(req, &mut reply, on_delta, on_thinking)).await {
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

    async fn stream(&self, req: &ChatRequest, reply: &mut Reply, on_delta: &mut (dyn FnMut(&str) + Send), on_thinking: &mut (dyn FnMut(&str) + Send)) -> Result<(), ChatError> {
        let resp = self.http.post(&self.cfg.url).header("x-api-key", &self.cfg.api_key).header("anthropic-version", API_VERSION).header("anthropic-beta", FALLBACK_BETA).json(&req.body(&self.cfg.model)).send().await.map_err(|e| ChatError::Failed(e.to_string()))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            let msg = format!("{status}: {}", api_error(&body));
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
                        match reply.apply(&data) {
                            Some(Delta::Text(d)) => on_delta(&d),
                            Some(Delta::Thinking(d)) => on_thinking(&d),
                            None => {}
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

/// The message of an Anthropic error body (`{"type": "error", "error": {"message": …}}`), else the body as it came.
fn api_error(body: &str) -> String {
    serde_json::from_str::<Value>(body).ok().and_then(|v| v["error"]["message"].as_str().map(str::to_string)).unwrap_or_else(|| body.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stream as the Messages API sends it: an empty thinking block, then text with a multibyte character, usage twice, a ping.
    const STREAM: &str = concat!(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"usage\":{\"input_tokens\":120,\"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":30,\"output_tokens\":1}}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}\n\n",
        "event: ping\ndata: {\"type\": \"ping\"}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"abc\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"## é—𝛼 \"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"∇\\n- a\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\",\"stop_details\":null},\"usage\":{\"input_tokens\":120,\"cache_read_input_tokens\":30,\"output_tokens\":55}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
    );

    fn replay(chunks: &[&[u8]]) -> (Reply, String) {
        let (mut parser, mut reply, mut preview) = (SseParser::default(), Reply::default(), String::new());
        for c in chunks {
            for data in parser.feed(c) {
                if let Some(Delta::Text(d)) = reply.apply(&data) {
                    preview += &d;
                }
            }
        }
        (reply, preview)
    }

    #[test]
    fn every_split_of_a_stream_gives_the_same_reply() {
        let bytes = STREAM.as_bytes();
        let (whole, preview) = replay(&[bytes]);
        assert_eq!((whole.finish.as_deref(), whole.done), (Some("end_turn"), true));
        assert_eq!(whole.text, "## é—𝛼 ∇\n- a");
        assert_eq!(whole.usage, Some(Usage { prompt_tokens: 150, cached_tokens: 30, completion_tokens: 55 }), "the later figures replace the first");
        assert_eq!(preview, whole.text, "only text reaches the preview");
        assert!(whole.outcome().is_ok());
        for at in 1..bytes.len() {
            let (r, p) = replay(&[&bytes[..at], &bytes[at..]]);
            assert_eq!((&r.text, &r.finish, &r.usage, &p), (&whole.text, &whole.finish, &whole.usage, &preview), "split at {at}");
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
    fn thinking_is_told_apart_from_text_and_only_text_reaches_the_preview_and_the_answer() {
        let mut r = Reply::default();
        assert_eq!(r.apply(r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Let me think"}}"#), Some(Delta::Thinking("Let me think".to_string())));
        assert_eq!(r.apply(r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":""}}"#), None);
        assert_eq!(r.apply(r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":""}}"#), None);
        assert_eq!(r.apply(r#"{"type":"ping"}"#), None);
        assert_eq!(r.apply(r#"{"type":"message_delta","delta":{"stop_reason":null},"usage":{"output_tokens":2}}"#), None);
        assert_eq!(r.apply(r###"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"## A"}}"###), Some(Delta::Text("## A".to_string())));
        assert_eq!((r.text.as_str(), r.finish), ("## A", None), "the thinking is not in the answer");
    }

    #[test]
    fn success_is_a_normal_stop_with_output() {
        let with = |text: &str, finish: Option<&str>, error: Option<&str>| Reply { text: text.into(), finish: finish.map(String::from), error: error.map(String::from), ..Reply::default() }.outcome();
        assert!(with("- a", Some("end_turn"), None).is_ok());
        assert!(with("  \n", Some("end_turn"), None).unwrap_err().contains("empty"));
        assert!(with("- a", Some("max_tokens"), None).unwrap_err().contains("max_tokens"));
        assert!(with("- a", None, None).unwrap_err().contains("before the response finished"));
        assert!(with("- a", Some("end_turn"), Some("overloaded")).unwrap_err().contains("overloaded"));
    }

    #[test]
    fn a_refusal_names_its_category_and_an_error_event_is_kept() {
        let (r, _) = replay(&[br#"data: {"type":"message_delta","delta":{"stop_reason":"refusal","stop_details":{"type":"refusal","category":"cyber","explanation":null}},"usage":{"output_tokens":3}}

"#]);
        assert_eq!(r.outcome().unwrap_err(), "Claude declined to answer (cyber)");
        let (e, _) = replay(&[br#"data: {"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}

"#]);
        assert!(e.outcome().unwrap_err().contains("Overloaded"));
    }

    #[test]
    fn what_a_declining_model_wrote_is_dropped_when_the_fallback_answers() {
        let (r, _) = replay(&[concat!(
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"half an ans\"}}\n\n",
            "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"fallback\"}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"text_delta\",\"text\":\"the answer\"}}\n\n",
        )
        .as_bytes()]);
        assert_eq!(r.text, "the answer");
    }

    #[test]
    fn a_cost_is_computed_from_opus_5_5_list_prices() {
        let u = Usage { prompt_tokens: 1_000, cached_tokens: 200, completion_tokens: 100 };
        let (usd, billed) = u.cost();
        assert!(!billed, "Anthropic reports tokens, not a price");
        assert!((usd - (800.0 * 4.0e-6 + 200.0 * 0.2e-6 + 100.0 * 20.0e-6)).abs() < 1e-12);
    }

    #[test]
    fn a_request_is_opus_5_5_on_high_with_adaptive_thinking_and_the_fallback() {
        let image = Image { mime: "image/png", base64: "QUJD".into() };
        let req = |effort, content| ChatRequest { what: SpendKind::Notes, system: "sys".into(), content, effort, timeout: Duration::from_secs(1) };
        let body = req(None, Content::Parts { text: "words".into(), images: vec![image] }).body(MODEL);
        assert_eq!((body["model"].as_str(), body["stream"].as_bool(), body["fallbacks"].as_str()), (Some("claude-opus-5-5"), Some(true), Some("default")));
        assert_eq!((&body["thinking"], &body["output_config"]["effort"], &body["system"]), (&json!({"type": "adaptive", "display": "summarized"}), &json!("high"), &json!("sys")));
        assert!(body.get("temperature").is_none() && body.get("stream_options").is_none());
        let parts = body["messages"][0]["content"].as_array().unwrap();
        assert_eq!(parts[0], json!({"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "QUJD"}}), "images go first");
        assert_eq!(parts[1], json!({"type": "text", "text": "words"}));
        let text = req(Some("medium"), Content::Text("hi".into())).body(MODEL);
        assert_eq!((&text["messages"][0]["content"], &text["output_config"]["effort"]), (&json!("hi"), &json!("medium")));
    }

    #[test]
    fn an_error_body_gives_its_message() {
        assert_eq!(api_error(r#"{"type":"error","error":{"type":"invalid_request_error","message":"max_tokens: too large"}}"#), "max_tokens: too large");
        assert_eq!(api_error("  not json "), "not json");
    }
}
