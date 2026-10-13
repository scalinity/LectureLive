//! A fake Messages endpoint (spec §11): answers each request by a script, streaming SSE the way
//! api.anthropic.com does (message_start, a thinking block, text deltas, message_delta with the stop reason, message_stop).
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

pub enum Reply {
    /// These bytes, written in these pieces with a pause between them; then the connection closes.
    Stream(Vec<Vec<u8>>),
    /// The headers, then nothing.
    Stall,
    Status(u16, String),
}

#[derive(Default)]
pub struct SseState {
    bodies: Mutex<Vec<Value>>,
    heads: Mutex<Vec<String>>,
}

impl SseState {
    /// Every request's JSON body, in arrival order.
    pub fn bodies(&self) -> Vec<Value> {
        self.bodies.lock().unwrap().clone()
    }

    /// Every request's header block (request line included), in arrival order.
    pub fn heads(&self) -> Vec<String> {
        self.heads.lock().unwrap().clone()
    }

    pub fn requests(&self) -> usize {
        self.bodies.lock().unwrap().len()
    }
}

pub struct FakeSse {
    pub url: String,
    pub state: Arc<SseState>,
}

type Respond = dyn Fn(&Value) -> Reply + Send + Sync;

pub async fn start(respond: impl Fn(&Value) -> Reply + Send + Sync + 'static) -> FakeSse {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1/messages", listener.local_addr().unwrap());
    let state = Arc::new(SseState::default());
    let (st, respond): (_, Arc<Respond>) = (state.clone(), Arc::new(respond));
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            let (st, respond) = (st.clone(), respond.clone());
            tokio::spawn(async move { handle(tcp, &*respond, &st).await });
        }
    });
    FakeSse { url, state }
}

/// A response's events as the Messages API streams them: an empty thinking block, the text in deltas,
/// the stop reason and, when `usage` is set, the token counts at the start and in `message_delta`.
pub fn events(content: &[&str], stop: Option<&str>, usage: bool) -> Vec<u8> {
    events_thinking(&[], content, stop, usage)
}

/// As `events`, with the pieces of the model's thinking summary streamed in the thinking block before the text.
pub fn events_thinking(thoughts: &[&str], content: &[&str], stop: Option<&str>, usage: bool) -> Vec<u8> {
    let event = |name: &str, v: Value| format!("event: {name}\ndata: {v}\n\n");
    let counts = |input: u64, output: u64| if usage { json!({"input_tokens": input, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0, "output_tokens": output}) } else { Value::Null };
    let mut start = json!({"id": "msg_fake", "type": "message", "role": "assistant", "model": "claude-opus-5-5", "content": [], "stop_reason": null});
    if usage {
        start["usage"] = counts(100, 1);
    }
    let mut out = event("message_start", json!({"type": "message_start", "message": start}));
    out += &event("content_block_start", json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": ""}}));
    out += &event("ping", json!({"type": "ping"}));
    for t in thoughts {
        out += &event("content_block_delta", json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": t}}));
    }
    out += &event("content_block_stop", json!({"type": "content_block_stop", "index": 0}));
    out += &event("content_block_start", json!({"type": "content_block_start", "index": 1, "content_block": {"type": "text", "text": ""}}));
    for c in content {
        out += &event("content_block_delta", json!({"type": "content_block_delta", "index": 1, "delta": {"type": "text_delta", "text": c}}));
    }
    out += &event("content_block_stop", json!({"type": "content_block_stop", "index": 1}));
    if let Some(reason) = stop {
        let mut delta = json!({"type": "message_delta", "delta": {"stop_reason": reason, "stop_details": null}});
        if usage {
            delta["usage"] = counts(100, 20);
        }
        out += &event("message_delta", delta);
        out += &event("message_stop", json!({"type": "message_stop"}));
    }
    out.into_bytes()
}

/// A successful answer with its token counts, its text in word-sized deltas, sent in 1 KB pieces.
pub fn answer(text: &str) -> Reply {
    Reply::Stream(pieces(answer_bytes(text), 1024))
}

pub fn answer_bytes(text: &str) -> Vec<u8> {
    let deltas: Vec<&str> = text.split_inclusive(' ').collect();
    events_thinking(&["Thinking about ", "the notes."], &deltas, Some("end_turn"), true)
}

/// An error body as the API sends it.
pub fn error_body(kind: &str, message: &str) -> String {
    json!({"type": "error", "error": {"type": kind, "message": message}}).to_string()
}

pub fn pieces(bytes: Vec<u8>, n: usize) -> Vec<Vec<u8>> {
    bytes.chunks(n).map(<[u8]>::to_vec).collect()
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

async fn handle(mut tcp: TcpStream, respond: &Respond, st: &SseState) {
    let mut buf = Vec::new();
    let mut chunk = vec![0u8; 65_536];
    let head_end = loop {
        if let Some(i) = find(&buf, b"\r\n\r\n") {
            break i + 4;
        }
        match tcp.read(&mut chunk).await {
            Ok(n) if n > 0 => buf.extend_from_slice(&chunk[..n]),
            _ => return,
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let len: usize = head.lines().find_map(|l| l.split_once(':').filter(|(k, _)| k.trim().eq_ignore_ascii_case("content-length")).map(|(_, v)| v.trim().parse().unwrap())).expect("reqwest sends the JSON body's length");
    while buf.len() < head_end + len {
        match tcp.read(&mut chunk).await {
            Ok(n) if n > 0 => buf.extend_from_slice(&chunk[..n]),
            _ => return,
        }
    }
    let body: Value = serde_json::from_slice(&buf[head_end..head_end + len]).unwrap();
    st.bodies.lock().unwrap().push(body.clone());
    st.heads.lock().unwrap().push(head);
    const STREAM_HEAD: &str = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncache-control: no-cache\r\nconnection: close\r\n\r\n";
    match respond(&body) {
        Reply::Status(code, text) => {
            let resp = format!("HTTP/1.1 {code} Refused\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{text}", text.len());
            let _ = tcp.write_all(resp.as_bytes()).await;
        }
        Reply::Stream(pieces) => {
            let _ = tcp.write_all(STREAM_HEAD.as_bytes()).await;
            for p in pieces {
                if tcp.write_all(&p).await.is_err() {
                    return;
                }
                let _ = tcp.flush().await;
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        }
        Reply::Stall => {
            let _ = tcp.write_all(STREAM_HEAD.as_bytes()).await;
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
    }
    let _ = tcp.shutdown().await;
}

/// A complete study page in the prompt's vocabulary with exactly `words` visible words.
pub fn study_page(words: usize) -> String {
    let page = |filler: &str| {
        format!(
            "<div class=\"keystone\">\\[ \\theta \\leftarrow \\theta - \\eta \\nabla L \\]</div>\n<p class=\"lede\">Gradient descent and its variants.</p>\n<section class=\"part\"><h2>Descent</h2><figure class=\"widget\" data-title=\"Step size\"><figcaption>Try: drag the step size.</figcaption><script type=\"text/widget\">w.slider(\"eta\", {{ min: 0.01, max: 1, value: 0.1 }}); w.draw((ctx, v, W, H) => {{ ctx.fillRect(0, 0, W * v.eta, H); }});</script></figure><p>{filler}</p><aside class=\"eli5\"><p>Walking downhill in fog.</p></aside><div class=\"formula\" data-name=\"update\">\\[ \\theta_{{t+1}} = \\theta_t - \\eta g_t \\]</div></section>\n<section class=\"part\"><h2>Glossary</h2><dl class=\"glossary\"><dt>Step</dt><dd>One update.</dd></dl></section>\n<section class=\"part\"><h2>Key takeaways</h2><ul class=\"takeaways\"><li>Scale the rate.</li></ul></section>\n<ol class=\"quiz\"><li><p class=\"q\">Why decay the rate?</p><div class=\"answer\">To settle.</div></li></ol>"
        )
    };
    let base = lecturelive_core::notes::page::visible_words(&page(""));
    let filler: Vec<String> = (0..words.saturating_sub(base)).map(|i| format!("w{i}")).collect();
    page(&filler.join(" "))
}
