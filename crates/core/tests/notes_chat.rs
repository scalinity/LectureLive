//! The SSE chat client against a fake endpoint replaying Messages streams (spec §6.2, §11). No network.
mod support;

use std::path::Path;
use std::time::Duration;

use lecturelive_core::notes::chat::{Answer, ChatClient, ChatConfig, ChatError, ChatRequest, Content, Image};
use lecturelive_core::session::spend::{self, Spend, SpendKind};
use support::fake_sse::{self, Reply};

const RECORDED_TEXT: &str = "- Gradient descent minimizes a loss by repeatedly stepping parameters in the opposite direction of the gradient.\n- The learning rate controls step size: too large and it diverges; too small and it converges slowly.";

fn client(url: &str, ledger: &Path) -> ChatClient {
    let spend = Spend::open(ledger, "Machine Learning", "Week 01", chrono::NaiveDate::from_ymd_opt(2026, 9, 25).unwrap()).unwrap();
    ChatClient::new(ChatConfig { url: url.into(), idle_timeout: Duration::from_millis(300), ..ChatConfig::new("test-key".into()) }, Some(spend)).unwrap()
}

fn request() -> ChatRequest {
    ChatRequest { what: SpendKind::Notes, system: "sys".into(), content: Content::Text("hi".into()), effort: None, timeout: Duration::from_secs(10) }
}

async fn ask(c: &ChatClient, r: &ChatRequest) -> (Result<Answer, ChatError>, String) {
    let mut seen = String::new();
    let out = c.complete(r, &mut |d: &str| seen.push_str(d)).await;
    (out, seen)
}

async fn once(reply: impl Fn() -> Reply + Send + Sync + 'static) -> (Result<Answer, ChatError>, String, Vec<spend::SpendEntry>) {
    let dir = tempfile::tempdir().unwrap();
    let ledger = dir.path().join("spend.jsonl");
    let fake = fake_sse::start(move |_| reply()).await;
    let (out, seen) = ask(&client(&fake.url, &ledger), &request()).await;
    (out, seen, spend::read(&ledger).unwrap())
}

#[tokio::test]
async fn a_stream_in_small_pieces_gives_its_text_and_computed_cost() {
    let (out, seen, ledger) = once(|| Reply::Stream(fake_sse::pieces(fake_sse::answer_bytes(RECORDED_TEXT), 7))).await;
    let answer = out.unwrap();
    assert_eq!(answer.text, RECORDED_TEXT);
    assert_eq!(seen, RECORDED_TEXT, "the preview is the text deltas");
    // 100 tokens in and 20 out at Opus 5.5's $4 and $20 per million.
    let want = 100.0 * 4.0e-6 + 20.0 * 20.0e-6;
    assert!((answer.usd.unwrap() - want).abs() < 1e-12);
    assert_eq!(ledger.iter().map(|e| (e.what.as_str(), e.billed)).collect::<Vec<_>>(), vec![("notes", false)], "a computed figure is never the billed one");
    assert!((ledger[0].usd - want).abs() < 1e-9);
}

#[tokio::test]
async fn a_max_tokens_stop_fails_but_its_cost_is_recorded() {
    let (out, _, ledger) = once(|| Reply::Stream(vec![fake_sse::events(&["- cut off "], Some("max_tokens"), true)])).await;
    assert!(matches!(out, Err(ChatError::Failed(ref m)) if m.contains("max_tokens")), "{out:?}");
    assert_eq!(ledger.len(), 1);
}

#[tokio::test]
async fn a_stream_without_usage_succeeds_and_records_nothing() {
    let (out, _, ledger) = once(|| Reply::Stream(vec![fake_sse::events(&["- a"], Some("end_turn"), false)])).await;
    assert_eq!(out.unwrap().usd, None);
    assert!(ledger.is_empty());
}

#[tokio::test]
async fn an_empty_stream_fails_and_records_nothing() {
    let (out, _, ledger) = once(|| Reply::Stream(vec![fake_sse::events(&[], None, false)])).await;
    assert!(matches!(out, Err(ChatError::Failed(ref m)) if m.contains("before the response finished")), "{out:?}");
    assert!(ledger.is_empty());
}

#[tokio::test]
async fn a_stop_with_no_text_fails_and_its_cost_is_recorded() {
    let (out, _, ledger) = once(|| Reply::Stream(vec![fake_sse::events(&[], Some("end_turn"), true)])).await;
    assert!(matches!(out, Err(ChatError::Failed(ref m)) if m.contains("empty")), "{out:?}");
    assert_eq!(ledger.len(), 1);
}

#[tokio::test]
async fn a_stream_cut_off_mid_answer_fails_and_records_the_input_it_spent() {
    let (out, seen, ledger) = once(|| {
        let bytes = fake_sse::answer_bytes(RECORDED_TEXT);
        Reply::Stream(fake_sse::pieces(bytes[..bytes.len() * 3 / 4].to_vec(), 64))
    })
    .await;
    assert!(matches!(out, Err(ChatError::Failed(ref m)) if m.contains("before the response finished")), "{out:?}");
    assert!(!seen.is_empty(), "the preview showed what arrived");
    assert_eq!(ledger.len(), 1, "message_start reported the input tokens, which are billed even when the stream breaks");
    assert!((ledger[0].usd - 100.0 * 4.0e-6 - 1.0 * 20.0e-6).abs() < 1e-9, "the input and the one output token reported so far: {}", ledger[0].usd);
}

#[tokio::test]
async fn a_refusal_is_a_failure_that_names_its_category() {
    let stream = br#"event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"refusal","stop_details":{"type":"refusal","category":"bio","explanation":null}},"usage":{"input_tokens":50,"output_tokens":0}}

event: message_stop
data: {"type":"message_stop"}

"#
    .to_vec();
    let (out, _, _) = once(move || Reply::Stream(vec![stream.clone()])).await;
    assert!(matches!(out, Err(ChatError::Failed(ref m)) if m == "Claude declined to answer (bio)"), "{out:?}");
}

#[tokio::test]
async fn a_refused_key_is_a_refusal_with_the_servers_message() {
    let body = fake_sse::error_body("authentication_error", "invalid x-api-key");
    let (out, _, ledger) = once(move || Reply::Status(401, body.clone())).await;
    assert_eq!(out.unwrap_err(), ChatError::Refused("401 Unauthorized: invalid x-api-key".into()));
    assert!(ledger.is_empty());
}

#[tokio::test]
async fn a_server_error_is_a_failure_not_a_refusal() {
    let (out, _, _) = once(|| Reply::Status(503, "{}".into())).await;
    assert!(matches!(out, Err(ChatError::Failed(ref m)) if m.starts_with("503")), "{out:?}");
}

#[tokio::test]
async fn a_stalled_stream_fails_after_the_idle_timeout() {
    let started = std::time::Instant::now();
    let (out, _, _) = once(|| Reply::Stall).await;
    assert!(matches!(out, Err(ChatError::Failed(_))), "{out:?}");
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[tokio::test]
async fn requests_are_opus_5_5_with_the_key_the_version_and_the_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let fake = fake_sse::start(|_| fake_sse::answer("## A\n- b")).await;
    let req = ChatRequest {
        what: SpendKind::Page,
        system: "the system prompt".into(),
        content: Content::Parts { text: "the user text".into(), images: vec![Image { mime: "image/png", base64: "iVBORw0K".into() }] },
        effort: None,
        timeout: Duration::from_secs(10),
    };
    let (out, _) = ask(&client(&fake.url, &dir.path().join("spend.jsonl")), &req).await;
    assert_eq!(out.unwrap().text, "## A\n- b");
    let body = &fake.state.bodies()[0];
    assert_eq!(body["model"], "claude-opus-5-5");
    assert_eq!(body["stream"], true);
    assert_eq!(body["thinking"], serde_json::json!({"type": "adaptive"}));
    assert_eq!(body["output_config"]["effort"], "high");
    assert_eq!(body["fallbacks"], "default");
    assert_eq!(body["system"], "the system prompt");
    assert_eq!(body["messages"][0]["role"], "user");
    assert_eq!(body["messages"][0]["content"][0], serde_json::json!({"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "iVBORw0K"}}));
    assert_eq!(body["messages"][0]["content"][1], serde_json::json!({"type": "text", "text": "the user text"}));
    let head = fake.state.heads()[0].to_lowercase();
    for line in ["x-api-key: test-key", "anthropic-version: 2023-06-01", "anthropic-beta: server-side-fallback-2026-07-01"] {
        assert!(head.contains(line), "{line} in {head}");
    }
    assert!(!head.contains("authorization"), "{head}");
    assert_eq!(spend::read(&dir.path().join("spend.jsonl")).unwrap()[0].what, "page");
}

/// M6, network loss for LectureLive alone: a connect address carries chat requests.
#[tokio::test]
async fn a_connect_address_carries_chat_requests() {
    let dir = tempfile::tempdir().unwrap();
    let fake = fake_sse::start(|_| fake_sse::answer("ok")).await;
    let addr: std::net::SocketAddr = fake.url.trim_start_matches("http://").split('/').next().unwrap().parse().unwrap();
    let c = ChatClient::new(ChatConfig { url: fake.url.replace(&addr.to_string(), "lecturelive.invalid"), connect_to: Some(addr), ..ChatConfig::new("test-key".into()) }, None).unwrap();
    let (out, _) = ask(&c, &request()).await;
    assert_eq!(out.unwrap().text, "ok");
    drop(dir);
}

/// An `error` event inside the stream (Anthropic sends `overloaded_error` this way) fails the answer with its message.
#[tokio::test]
async fn an_error_event_in_the_stream_fails_with_its_message() {
    let mut bytes = fake_sse::events(&["- fine"], None, false);
    bytes.extend_from_slice(b"event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n\n");
    let (out, _, _) = once(move || Reply::Stream(vec![bytes.clone()])).await;
    assert!(matches!(out, Err(ChatError::Failed(ref m)) if m.contains("Overloaded")), "{out:?}");
}

/// §10 "Spend ledger write fails": the answer is kept, with a warning.
#[tokio::test]
async fn a_ledger_that_cannot_be_written_is_a_warning_and_the_answer_is_kept() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let ledger = dir.path().join("spend.jsonl");
    let fake = fake_sse::start(|_| fake_sse::answer("kept")).await;
    let c = client(&fake.url, &ledger);
    std::fs::write(&ledger, "").unwrap();
    std::fs::set_permissions(&ledger, std::fs::Permissions::from_mode(0o400)).unwrap();
    let (out, _) = ask(&c, &request()).await;
    let a = out.unwrap();
    assert_eq!(a.text, "kept");
    assert!(a.warning.as_deref().is_some_and(|w| w.contains("spend ledger")), "{:?}", a.warning);
}
