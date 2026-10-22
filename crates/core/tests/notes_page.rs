//! The study page gate against a fake endpoint (spec §6.4, §11): a fixture lecture distils within its
//! budget, and an unchanged one re-renders from its cached parts without requests. No network.
mod support;

use std::path::{Path, PathBuf};
use std::time::Duration;

use lecturelive_core::notes::chat::{ChatClient, ChatConfig};
use lecturelive_core::notes::page::{self, make_page, TEMPLATE};
use lecturelive_core::session::spend::Spend;
use support::fake_sse::{self, study_page};

/// The fixture lecture in a folder of its own, with its three slides drawn.
fn lecture() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let notes = dir.path().join("lecture_notes_20260925.md");
    std::fs::copy(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/notes/lecture/fixture_lecture.md"), &notes).unwrap();
    for name in ["slide_01_100512.png", "slide_02_101830.png", "slide_03_103245.png"] {
        support::slides::png(&dir.path().join("slides").join(name));
    }
    (dir, notes)
}

fn client(url: &str, dir: &Path) -> ChatClient {
    let spend = Spend::open(&dir.join("spend.jsonl"), "Machine Learning", "Week 03 — Optimisation", chrono::NaiveDate::from_ymd_opt(2026, 9, 25).unwrap()).unwrap();
    ChatClient::new(ChatConfig { url: url.into(), idle_timeout: Duration::from_secs(5), ..ChatConfig::new("test-key".into()) }, Some(spend)).unwrap()
}

#[tokio::test]
async fn the_fixture_lecture_distils_within_its_budget_and_rerenders_from_cache_without_requests() {
    let (dir, notes) = lecture();
    let budget = page::page_budget(&std::fs::read_to_string(&notes).unwrap()) as usize;
    let fake = fake_sse::start(move |body| {
        let system = body["system"].as_str().unwrap();
        // A first draft half again over budget; the revision within it.
        let words = if system.starts_with("You shorten") { budget * 9 / 10 } else { budget * 3 / 2 };
        fake_sse::answer(&study_page(words))
    })
    .await;
    let chat = client(&fake.url, dir.path());
    let cache = dir.path().join(".live_notes/lecture_notes_20260925.page.json");
    let name = "Week 03 — Optimisation";

    let first = make_page(&chat, "Machine Learning", &notes, &cache, name, TEMPLATE, &|_| {}).await.unwrap();
    assert_eq!(fake.state.requests(), 2, "a draft over budget gets one revision");
    let bodies = fake.state.bodies();
    assert_eq!(bodies[0]["output_config"]["effort"], "high");
    assert_eq!(bodies[0]["messages"][0]["content"].as_array().unwrap().len(), 4, "the notes and every slide image");
    assert!(bodies[1]["system"].as_str().unwrap().starts_with("You shorten"));
    assert!(bodies[1]["messages"][0]["content"].is_string(), "the revision carries no images");
    assert!(!first.cached);
    assert!(first.words <= first.budget as usize, "{} words of {}", first.words, first.budget);
    assert!(first.missing.is_empty(), "complete: {:?}", first.missing);
    assert_eq!(first.path, dir.path().join("Optimisation.html"));
    let html = std::fs::read_to_string(&first.path).unwrap();
    assert!(!html.contains("{{"), "every placeholder filled");
    assert!(html.contains("<title>Optimisation — Machine Learning</title>"));
    assert!(html.contains("data:image/jpeg;base64,"), "the slides are embedded");
    let cleared = lecturelive_core::notes::archive::clear(&first.path).unwrap();
    assert_eq!(cleared.files, 3);
    assert_eq!(std::fs::read_dir(dir.path().join("slides")).unwrap().count(), 0);

    let again = make_page(&chat, "Machine Learning", &notes, &cache, name, TEMPLATE, &|_| {}).await.unwrap();
    assert!(again.cached);
    assert_eq!(fake.state.requests(), 2, "no request for unchanged notes");
    assert_eq!(std::fs::read_to_string(&again.path).unwrap(), html);

    let redesigned = TEMPLATE.replace("<main id=\"notes\">", "<main id=\"notes\" data-design=\"2\">");
    let rerendered = make_page(&chat, "Machine Learning", &notes, &cache, name, &redesigned, &|_| {}).await.unwrap();
    assert!(rerendered.cached && fake.state.requests() == 2, "a template change alone costs nothing");
    assert!(std::fs::read_to_string(&rerendered.path).unwrap().contains("data-design=\"2\""));

    std::fs::write(&notes, std::fs::read_to_string(&notes).unwrap() + "\n- One more point.\n").unwrap();
    make_page(&chat, "Machine Learning", &notes, &cache, name, TEMPLATE, &|_| {}).await.unwrap();
    assert_eq!(fake.state.requests(), 4, "changed notes typeset again");
    let bodies = fake.state.bodies();
    assert_eq!(bodies[2]["messages"][0]["content"][1]["source"]["media_type"], "image/jpeg", "a new request reads the image from the finished HTML");
}

#[tokio::test]
async fn a_page_that_fails_after_its_retry_writes_nothing() {
    let (dir, notes) = lecture();
    let fake = fake_sse::start(|_| fake_sse::Reply::Stream(vec![fake_sse::events(&[], None, false)])).await;
    let chat = client(&fake.url, dir.path());
    let before = std::fs::read(&notes).unwrap();
    let err = make_page(&chat, "Machine Learning", &notes, &dir.path().join(".live_notes/p.json"), "Week 03 — Optimisation", TEMPLATE, &|_| {}).await;
    assert!(err.is_err());
    assert_eq!(fake.state.requests(), 2, "one retry");
    assert!(!dir.path().join("Optimisation.html").exists());
    assert_eq!(std::fs::read(&notes).unwrap(), before, "the notes are untouched");
}

/// The text of a request's user message: its slide images come as parts of their own.
fn request_text(body: &serde_json::Value) -> String {
    body["messages"][0]["content"].as_array().unwrap().iter().find_map(|p| (p["type"] == "text").then(|| p["text"].as_str().unwrap().to_string())).expect("a text part")
}

/// A notebook and a dataset beside the notes (spec §6.4): the request carries them, the page carries the data for its
/// widgets, and a changed dataset typesets again where a changed template alone does not.
#[tokio::test]
async fn the_notebook_and_data_beside_the_notes_reach_the_request_and_the_page() {
    let (dir, notes) = lecture();
    let live = dir.path().join("Live Coding");
    std::fs::create_dir_all(&live).unwrap();
    std::fs::write(live.join("cases.csv"), "area,label\n100,benign\n300,malignant\n").unwrap();
    std::fs::write(live.join("lab.ipynb"), serde_json::json!({"cells": [{"cell_type": "code", "source": "print(auc)", "outputs": [{"output_type": "stream", "text": "AUROC 0.959\n"}]}]}).to_string()).unwrap();
    let budget = page::page_budget(&std::fs::read_to_string(&notes).unwrap()) as usize;
    let fake = fake_sse::start(move |_| fake_sse::answer(&study_page(budget / 2))).await;
    let chat = client(&fake.url, dir.path());
    let cache = dir.path().join(".live_notes/lecture_notes_20260925.page.json");
    let name = "Week 03 — Optimisation";

    let first = make_page(&chat, "Machine Learning", &notes, &cache, name, TEMPLATE, &|_| {}).await.unwrap();
    let text = request_text(&fake.state.bodies()[0]);
    assert!(text.starts_with("Course materials") && text.contains("AUROC 0.959") && text.contains("w.data(\"cases\"): 2 rows"), "{text}");
    let html = std::fs::read_to_string(&first.path).unwrap();
    assert!(html.contains(r#"id="course-data">{"cases":{"columns":["area","label"],"rows":[[100.0,"benign"],[300.0,"malignant"]]}}</script>"#), "the page carries the data");

    let again = make_page(&chat, "Machine Learning", &notes, &cache, name, TEMPLATE, &|_| {}).await.unwrap();
    assert!(again.cached && fake.state.requests() == 1, "the same materials are a cache hit");
    std::fs::write(live.join("cases.csv"), "area,label\n100,benign\n300,malignant\n250,benign\n").unwrap();
    let changed = make_page(&chat, "Machine Learning", &notes, &cache, name, TEMPLATE, &|_| {}).await.unwrap();
    assert!(!changed.cached && fake.state.requests() == 2, "a changed dataset typesets again");

    // a lecture with neither sends the notes alone and carries an empty dataset block
    let (bare, notes) = lecture();
    let chat = client(&fake.url, bare.path());
    let plain = make_page(&chat, "Machine Learning", &notes, &bare.path().join(".live_notes/p.json"), name, TEMPLATE, &|_| {}).await.unwrap();
    let last = fake.state.bodies().pop().unwrap();
    assert!(request_text(&last).starts_with("The complete notes"), "no materials section");
    assert!(std::fs::read_to_string(&plain.path).unwrap().contains(r#"id="course-data">{}</script>"#));
}
