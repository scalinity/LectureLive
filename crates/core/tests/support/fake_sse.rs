//! A fake chat-completions endpoint (spec §11): answers each request by a script, streaming SSE the
//! way api.x.ai does (plan research: reasoning, content, a finish chunk, the usage chunk, [DONE]).
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
}

impl SseState {
    /// Every request's JSON body, in arrival order.
    pub fn bodies(&self) -> Vec<Value> {
        self.bodies.lock().unwrap().clone()
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
    let url = format!("http://{}/v1/chat/completions", listener.local_addr().unwrap());
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

/// A response's events as api.x.ai streams them.
pub fn events(content: &[&str], finish: Option<&str>, ticks: Option<u64>) -> Vec<u8> {
    let chunk = |choices: Value, usage: Option<Value>| {
        let mut v = json!({"id": "fake", "object": "chat.completion.chunk", "created": 0, "model": "grok-4.7", "choices": choices});
        if let Some(u) = usage {
            v["usage"] = u;
        }
        format!("data: {v}\n\n")
    };
    let mut out = chunk(json!([{"index": 0, "delta": {"reasoning_content": "Thinking", "role": "assistant"}}]), None);
    for c in content {
        out += &chunk(json!([{"index": 0, "delta": {"content": c}}]), None);
    }
    if let Some(f) = finish {
        out += &chunk(json!([{"index": 0, "delta": {}, "finish_reason": f}]), None);
    }
    if let Some(t) = ticks {
        out += &chunk(json!([]), Some(json!({"prompt_tokens": 100, "completion_tokens": 20, "total_tokens": 120, "prompt_tokens_details": {"cached_tokens": 0}, "cost_in_usd_ticks": t})));
    }
    if finish.is_some() {
        out += "data: [DONE]\n\n";
    }
    out.into_bytes()
}

/// A successful answer, its text in word-sized deltas, sent in 1 KB pieces.
pub fn answer(text: &str, ticks: u64) -> Reply {
    let deltas: Vec<&str> = text.split_inclusive(' ').collect();
    Reply::Stream(pieces(events(&deltas, Some("stop"), Some(ticks)), 1024))
}

pub fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/fixtures/notes/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
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
            "<div class=\"keystone\">\\[ \\theta \\leftarrow \\theta - \\eta \\nabla L \\]</div>\n<p class=\"lede\">Gradient descent and its variants.</p>\n<section class=\"part\"><h2>Descent</h2><p>{filler}</p><div class=\"formula\" data-name=\"update\">\\[ \\theta_{{t+1}} = \\theta_t - \\eta g_t \\]</div></section>\n<section class=\"part\"><h2>Glossary</h2><dl class=\"glossary\"><dt>Step</dt><dd>One update.</dd></dl></section>\n<section class=\"part\"><h2>Key takeaways</h2><ul class=\"takeaways\"><li>Scale the rate.</li></ul></section>\n<ol class=\"quiz\"><li><p class=\"q\">Why decay the rate?</p><div class=\"answer\">To settle.</div></li></ol>"
        )
    };
    let base = lecturelive_core::notes::page::visible_words(&page(""));
    let filler: Vec<String> = (0..words.saturating_sub(base)).map(|i| format!("w{i}")).collect();
    page(&filler.join(" "))
}
