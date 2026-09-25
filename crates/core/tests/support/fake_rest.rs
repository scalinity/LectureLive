//! A fake Grok REST transcription endpoint (spec §11): answers each clip of synthetic speech with
//! the words that start in it.
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use super::fake_stt::BAD_KEY_BODY;
use super::speech;

#[derive(Default)]
pub struct RestState {
    pub requests: AtomicUsize,
    fields: Mutex<Vec<(String, String)>>,
    auth: Mutex<Vec<String>>,
}

impl RestState {
    /// The form's text fields of every request, in order.
    pub fn fields(&self) -> Vec<(String, String)> {
        self.fields.lock().unwrap().clone()
    }

    pub fn auth(&self) -> Vec<String> {
        self.auth.lock().unwrap().clone()
    }
}

pub struct FakeRest {
    pub url: String,
    pub state: Arc<RestState>,
}

pub async fn start(refuse: Option<u16>) -> FakeRest {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1/stt", listener.local_addr().unwrap());
    let state = Arc::new(RestState::default());
    let st = state.clone();
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            let st = st.clone();
            tokio::spawn(async move { handle(tcp, refuse, &st).await });
        }
    });
    FakeRest { url, state }
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    hay.get(from..)?.windows(needle.len()).position(|w| w == needle).map(|i| i + from)
}

/// The parts of a multipart/form-data body, as (field name, content).
fn multipart(body: &[u8], boundary: &str) -> Vec<(String, Vec<u8>)> {
    let delim = format!("--{boundary}");
    let mut parts = Vec::new();
    let mut at = find(body, delim.as_bytes(), 0).expect("a first boundary") + delim.len();
    while let Some(next) = find(body, delim.as_bytes(), at) {
        let part = &body[at..next]; // "\r\n<headers>\r\n\r\n<content>\r\n"
        let head_end = find(part, b"\r\n\r\n", 0).expect("part headers");
        let head = String::from_utf8_lossy(&part[..head_end]);
        let name = head.split("name=\"").nth(1).and_then(|s| s.split('"').next()).expect("a field name").to_string();
        parts.push((name, part[head_end + 4..part.len() - 2].to_vec()));
        at = next + delim.len();
    }
    parts
}

async fn read_more(tcp: &mut TcpStream, buf: &mut Vec<u8>) -> bool {
    let mut chunk = vec![0u8; 65_536];
    match tcp.read(&mut chunk).await {
        Ok(n) if n > 0 => {
            buf.extend_from_slice(&chunk[..n]);
            true
        }
        _ => false,
    }
}

async fn handle(mut tcp: TcpStream, refuse: Option<u16>, st: &RestState) {
    let mut buf = Vec::new();
    let head_end = loop {
        if let Some(i) = find(&buf, b"\r\n\r\n", 0) {
            break i + 4;
        }
        if !read_more(&mut tcp, &mut buf).await {
            return;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let header = |name: &str| head.lines().find_map(|l| l.split_once(':').filter(|(k, _)| k.trim().eq_ignore_ascii_case(name)).map(|(_, v)| v.trim().to_string()));
    let len: usize = header("content-length").expect("reqwest sends the length of a form it can measure").parse().unwrap();
    while buf.len() < head_end + len {
        if !read_more(&mut tcp, &mut buf).await {
            return;
        }
    }
    st.requests.fetch_add(1, SeqCst);
    st.auth.lock().unwrap().extend(header("authorization"));
    let (status, reason, body) = match refuse {
        Some(code) => (code, "Bad Request", BAD_KEY_BODY.to_string()),
        None => {
            let ctype = header("content-type").expect("a multipart form");
            let boundary = ctype.split("boundary=").nth(1).expect("a boundary").trim_matches('"').to_string();
            let mut wav = Vec::new();
            for (name, value) in multipart(&buf[head_end..head_end + len], &boundary) {
                if name == "file" {
                    wav = value;
                } else {
                    st.fields.lock().unwrap().push((name, String::from_utf8(value).unwrap()));
                }
            }
            let pcm: Vec<i16> = hound::WavReader::new(std::io::Cursor::new(wav)).unwrap().samples::<i16>().map(|s| s.unwrap()).collect();
            (200, "OK", speech::transcribe_clip(&pcm).to_string())
        }
    };
    let resp = format!("HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len());
    let _ = tcp.write_all(resp.as_bytes()).await;
    let _ = tcp.shutdown().await;
}
