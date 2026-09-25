//! Live checks against api.x.ai (spec §6), on synthetic material only, within the M3 cap of $2:
//!     cargo test -p lecturelive-core --test notes_live -- --ignored --nocapture --test-threads 1
//! Each prints its cost from usage.cost_in_usd_ticks.
mod support;

use std::path::Path;
use std::time::Duration;

use lecturelive_core::notes::chat::{ChatClient, ChatConfig};
use lecturelive_core::notes::page::{make_page, TEMPLATE};
use lecturelive_core::session::coordinator::Store;
use lecturelive_core::session::files::LectureFiles;
use lecturelive_core::session::folder;
use lecturelive_core::session::lecture::Lecture;
use lecturelive_core::session::segments::{NewSegment, SegmentLog, SegmentSource};
use lecturelive_core::session::sidecar::Sidecar;
use lecturelive_core::session::spend::{self, Spend};
use tokio::sync::mpsc;

fn api_key() -> String {
    if let Ok(k) = std::env::var("GROK_API_KEY") {
        return k;
    }
    let env = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.env")).expect("GROK_API_KEY in the environment or the repository's .env");
    env.lines().find_map(|l| l.strip_prefix("GROK_API_KEY=")).map(|v| v.trim().trim_matches('"').to_string()).expect("GROK_API_KEY in .env")
}

fn live(ledger: &Path, lecture: &str) -> (ChatClient, Spend) {
    let spend = Spend::open(ledger, "M3 synthetic", lecture, chrono::Local::now().date_naive()).unwrap();
    (ChatClient::new(ChatConfig::new(api_key()), Some(spend.clone())).unwrap(), spend)
}

#[tokio::test]
#[ignore]
async fn a_live_snapshot_streams_commits_and_reports_its_billed_cost() {
    let dir = tempfile::tempdir().unwrap();
    let files = LectureFiles::standard(dir.path(), chrono::NaiveDate::from_ymd_opt(2026, 9, 25).unwrap());
    let title = "# M3 synthetic — Week 01 — Optimisation — 2026-09-25";
    folder::open(&files, title, false).unwrap();
    let mut log = SegmentLog::open(dir.path(), &files.stem).unwrap();
    let anchor = chrono::TimeZone::with_ymd_and_hms(&chrono::Local, 2026, 9, 25, 10, 0, 0).unwrap();
    for (k, text) in ["Gradient descent moves the weights against the gradient of the loss.", "The learning rate sets how far each step goes; too large and it diverges.", "Momentum keeps a running average of past gradients to smooth the path."].iter().enumerate() {
        log.append(NewSegment { recording_id: uuid::Uuid::nil(), start_sample: k as u64 * 96_000, end_sample: k as u64 * 96_000 + 80_000, text: text.to_string(), words: vec![], source: SegmentSource::Live }, anchor).unwrap();
    }
    let ledger = dir.path().join("spend.jsonl");
    let (chat, spend) = live(&ledger, "Week 01 — Optimisation");
    let lec = Lecture { files: files.clone(), course: "M3 synthetic".into(), name: "Week 01 — Optimisation".into(), title: title.into(), chat, spend };
    let (tx, _rx) = mpsc::unbounded_channel();
    lec.snapshot(&Store::offline(Sidecar::load(&files.sidecar()).unwrap().unwrap(), files.sidecar()), "", &tx).await.unwrap();
    let notes = std::fs::read_to_string(&files.notes).unwrap();
    println!("{notes}");
    assert_eq!(notes.matches("<!-- ").count(), 1);
    let entries = spend::read(&ledger).unwrap();
    println!("cost: ${:.6} (billed: {})", entries[0].usd, entries[0].billed);
    assert!(entries.len() == 1 && entries[0].billed && entries[0].what == "notes");
}

#[tokio::test]
#[ignore]
async fn the_fixture_lecture_distils_within_its_budget_live_and_rerenders_free() {
    let dir = tempfile::tempdir().unwrap();
    let notes = dir.path().join("lecture_notes_20260925.md");
    std::fs::copy(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/notes/lecture/fixture_lecture.md"), &notes).unwrap();
    for name in ["slide_01_100512.png", "slide_02_101830.png", "slide_03_103245.png"] {
        support::slides::png(&dir.path().join("slides").join(name));
    }
    let ledger = dir.path().join("spend.jsonl");
    let (chat, _) = live(&ledger, "Week 03 — Optimisation");
    let cache = dir.path().join(".live_notes/lecture_notes_20260925.page.json");
    let first = make_page(&chat, "M3 synthetic", &notes, &cache, "Week 03 — Optimisation", TEMPLATE, &|m| println!("… {m}")).await.unwrap();
    let paid = spend::read(&ledger).unwrap();
    println!("{} words of {} (missing: {:?}); {} requests, ${:.4}", first.words, first.budget, first.missing, paid.len(), paid.iter().map(|e| e.usd).sum::<f64>());
    let keep = std::env::var("M3_PAGE_OUT").ok();
    if let Some(out) = keep {
        std::fs::copy(&first.path, Path::new(&out)).unwrap(); // for looking at, outside the repository
    }
    assert!(first.words <= first.budget as usize, "{} words of {}", first.words, first.budget);
    assert!(first.missing.is_empty(), "{:?}", first.missing);
    let again = make_page(&chat, "M3 synthetic", &notes, &cache, "Week 03 — Optimisation", TEMPLATE, &|_| {}).await.unwrap();
    assert!(again.cached);
    assert_eq!(spend::read(&ledger).unwrap().len(), paid.len(), "no request for an unchanged lecture");
    let _ = Duration::ZERO;
}
