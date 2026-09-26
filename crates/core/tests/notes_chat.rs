//! The SSE chat client against a fake endpoint replaying recorded streams (spec §6.2, §11). No network.
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
async fn a_recorded_stream_in_small_pieces_gives_its_text_and_billed_cost() {
    let (out, seen, ledger) = once(|| Reply::Stream(fake_sse::pieces(fake_sse::fixture("stream_usage.sse"), 7))).await;
    let answer = out.unwrap();
    assert_eq!(answer.text, RECORDED_TEXT);
    assert_eq!(seen, RECORDED_TEXT, "the preview is the content deltas");
    assert_eq!(answer.usd, Some(0.001202));
    assert_eq!(ledger.iter().map(|e| (e.what.as_str(), e.usd, e.billed)).collect::<Vec<_>>(), vec![("notes", 0.001202, true)]);
}

#[tokio::test]
async fn a_length_finish_fails_but_its_billed_cost_is_recorded() {
    let (out, _, ledger) = once(|| Reply::Stream(fake_sse::pieces(fake_sse::fixture("stream_length.sse"), 64))).await;
    assert!(matches!(out, Err(ChatError::Failed(ref m)) if m.contains("length")), "{out:?}");
    assert_eq!(ledger.iter().map(|e| e.usd).collect::<Vec<_>>(), vec![0.001238]);
}

#[tokio::test]
async fn a_stream_without_usage_succeeds_and_records_nothing() {
    let (out, _, ledger) = once(|| Reply::Stream(fake_sse::pieces(fake_sse::fixture("stream_no_usage.sse"), 64))).await;
    assert_eq!(out.unwrap().usd, None);
    assert!(ledger.is_empty());
}

#[tokio::test]
async fn an_empty_stream_fails_and_records_nothing() {
    let (out, _, ledger) = once(|| Reply::Stream(vec![b"data: [DONE]\n\n".to_vec()])).await;
    assert!(matches!(out, Err(ChatError::Failed(ref m)) if m.contains("before the response finished")), "{out:?}");
    assert!(ledger.is_empty());
}

#[tokio::test]
async fn a_finish_with_no_content_fails_and_its_cost_is_recorded() {
    let (out, _, ledger) = once(|| Reply::Stream(vec![fake_sse::events(&[], Some("stop"), Some(1_000))])).await;
    assert!(matches!(out, Err(ChatError::Failed(ref m)) if m.contains("empty")), "{out:?}");
    assert_eq!(ledger.len(), 1);
}

#[tokio::test]
async fn a_stream_cut_off_mid_answer_fails_and_records_nothing() {
    let (out, seen, ledger) = once(|| {
        let bytes = fake_sse::fixture("stream_usage.sse");
        Reply::Stream(fake_sse::pieces(bytes[..bytes.len() * 3 / 4].to_vec(), 64))
    })
    .await;
    assert!(matches!(out, Err(ChatError::Failed(ref m)) if m.contains("before the response finished")), "{out:?}");
    assert!(!seen.is_empty(), "the preview showed what arrived");
    assert!(ledger.is_empty(), "the cost comes only at the end");
}

#[tokio::test]
async fn a_refused_key_is_a_refusal_with_the_servers_message() {
    let body = String::from_utf8(fake_sse::fixture("refused_key.json")).unwrap();
    let (out, _, ledger) = once(move || Reply::Status(400, body.clone())).await;
    assert_eq!(out.unwrap_err(), ChatError::Refused("400 Bad Request: Incorrect API key provided. You can obtain an API key from https://console.x.ai.".into()));
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
async fn requests_carry_the_python_clis_messages_and_ask_for_the_cost() {
    let dir = tempfile::tempdir().unwrap();
    let fake = fake_sse::start(|_| fake_sse::answer("## A\n- b", 1_000)).await;
    let req = ChatRequest {
        what: SpendKind::Page,
        system: "the system prompt".into(),
        content: Content::Parts { text: "the user text".into(), images: vec![Image { mime: "image/png", base64: "iVBORw0K".into() }] },
        effort: Some("medium"),
        timeout: Duration::from_secs(10),
    };
    let (out, _) = ask(&client(&fake.url, &dir.path().join("spend.jsonl")), &req).await;
    assert_eq!(out.unwrap().text, "## A\n- b");
    let body = &fake.state.bodies()[0];
    assert_eq!(body["model"], "grok-4.7");
    assert_eq!(body["stream"], true);
    assert_eq!(body["stream_options"]["include_usage"], true);
    assert_eq!(body["reasoning_effort"], "medium");
    assert_eq!(body["messages"][0], serde_json::json!({"role": "system", "content": "the system prompt"}));
    assert_eq!(body["messages"][1]["content"][0], serde_json::json!({"type": "text", "text": "the user text"}));
    assert_eq!(body["messages"][1]["content"][1], serde_json::json!({"type": "image_url", "image_url": {"url": "data:image/png;base64,iVBORw0K", "detail": "high"}}));
    assert_eq!(spend::read(&dir.path().join("spend.jsonl")).unwrap()[0].what, "page");
}

/// M6, network loss for LectureLive alone: a connect address carries chat requests.
#[tokio::test]
async fn a_connect_address_carries_chat_requests() {
    let dir = tempfile::tempdir().unwrap();
    let fake = fake_sse::start(|_| fake_sse::answer("ok", 1_000)).await;
    let addr: std::net::SocketAddr = fake.url.trim_start_matches("http://").split('/').next().unwrap().parse().unwrap();
    let c = ChatClient::new(ChatConfig { url: fake.url.replace(&addr.to_string(), "lecturelive.invalid"), connect_to: Some(addr), ..ChatConfig::new("test-key".into()) }, None).unwrap();
    let (out, _) = ask(&c, &request()).await;
    assert_eq!(out.unwrap().text, "ok");
    drop(dir);
}

/// M3 minor: an `"error": null` key is no error.
#[tokio::test]
async fn an_error_key_that_is_null_is_not_an_error() {
    let body = b"data: {\"choices\":[{\"delta\":{\"content\":\"fine\"},\"finish_reason\":null}],\"error\":null}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: {\"choices\":[],\"usage\":{\"cost_in_usd_ticks\":1000}}\n\ndata: [DONE]\n\n".to_vec();
    let (out, _, _) = once(move || Reply::Stream(vec![body.clone()])).await;
    assert_eq!(out.unwrap().text, "fine");
}
