# M3 Notes/Session Parity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn M2's streamed, recovered transcript into Markdown notes exactly as `live_notes.py` does. Snapshots stream from `grok-4.7` and commit through a journal that survives a crash at any step; polish rewrites the notes, and a study page distils them. Take over lecture folders and the spend ledger from the Python CLI, and run a whole lecture headless from the Rust CLI.

**Architecture:** Notes logic lives in a new `core::notes` module: prompts, timeline, context budget, SSE chat client, embed repair, study page. Every mutation of the notes document goes through `session::notesfile`: revision check, journaled append, launch recovery and polish's atomic replace. The session coordinator stays the sidecar's single writer. A new `Store` handle runs sidecar updates inside the coordinator while a session records, and directly on the file when none does, so snapshot, polish and slide registration work the same in and out of a session. `session::lecture` orchestrates a whole lecture: notes worker, slide watcher, stop sequence and final snapshot. The CLI's `lecture` command and the M4 app are thin shells over it. The spend ledger writes the Python CLI's exact line format into the app's data directory and imports the CLI's ledger lines at each start.

**Tech Stack:** Rust 1.96.1, `tokio 1.53.1`, `reqwest 0.13.5` (adds `stream`), `serde_json 1.0.151` (adds `preserve_order`), `regex 1.13`, `sha2 0.10`, `base64 0.22`, `chrono 0.4.45`, `uuid 1.26.1`. New crates are resolved by `cargo add`; the versions named are those already in `Cargo.lock` as transitive dependencies.

**Spec:** `docs/spec.md` §6 (all), §8 (all), §9.1 (the spend view the ledger feeds), §10 rows "Notes call fails", "Study page part fails", "Crash mid-commit", "Spend ledger write fails", "External edit of notes", §11, §14.2. Gate: `docs/milestones.md` → M3. Evidence: the M0, M1 and M2 Findings in `docs/milestones.md`, `live_notes.py` (prompts, formats, snapshot, resume, polish, page, spend), and the plan research below.

**Plan research (2026-09-25, before this plan was written).** `live_notes.py` calls chat completions without streaming, so the SSE shape was recorded live with `curl -N` against `https://api.x.ai/v1/chat/completions` (`grok-4.7`, synthetic two-line prompts, about $0.005 in total). Logs kept as fixtures in `crates/core/tests/fixtures/notes/`:

| Run | Result | Fixture |
|---|---|---|
| `stream: true`, no `stream_options`, effort `low` | `content-type: text/event-stream`. Every event is one `data: {json}\n\n` line of `object: "chat.completion.chunk"`. The first chunk carries `delta.role: "assistant"` together with the first `reasoning_content`; reasoning deltas (`delta.reasoning_content`) all come before the content deltas (`delta.content`). A last chunk has `delta: {}` and `finish_reason: "stop"`, then `data: [DONE]`. **No usage and no cost anywhere in the stream.** | `stream_no_usage.sse` |
| Same with `stream_options: {"include_usage": true}` | Identical, plus exactly one chunk after the finish chunk: `choices: []`, `usage: {prompt_tokens, completion_tokens, total_tokens, prompt_tokens_details: {text_tokens, audio_tokens, image_tokens, cached_tokens}, completion_tokens_details: {reasoning_tokens, …}, num_sources_used, cost_in_usd_ticks: 12020000}`, then `data: [DONE]`. The cost is not a running total: it arrives once, at the end. | `stream_usage.sse` |
| `max_tokens: 40` | Content stops mid-sentence; `finish_reason: "length"`; the usage chunk still follows and is billed (`cost_in_usd_ticks: 12380000`); `[DONE]`. A truncated answer costs money. | `stream_length.sse` |
| Effort `medium`, user content as `[{"type":"text"},{"type":"image_url","image_url":{"url":"data:image/png;base64,…","detail":"high"}}]` (the Python CLI's shape) | Accepted; 44 reasoning deltas, then content, `finish_reason: "stop"`, billed usage with `image_tokens`. | `stream_image_medium.sse` |
| Bad key; `reasoning_effort: "bogus"` | HTTP 400 before any stream, `content-type: application/json`, `{"code":"invalid-argument","error":"Incorrect API key provided. You can obtain an API key from https://console.x.ai."}` / `{"code":"invalid-argument","error":"Invalid reasoning effort."}`. `stt::protocol::refusal_message` reads both. | `refused_key.json` |
| `GET /v1/models`, `GET /v1/language-models/grok-4.7` (free) | `grok-4.7`: `context_length: 500000`, `long_context_threshold: 200000` (above it every token is billed at twice the rate), reasoning efforts `low, medium, high, xhigh`, default `high`. Listed prices: prompt 20000, cached 5000, completion 60000 (per 10¹⁰ USD per token: $2.00, $0.50, $6.00 per million; the Python CLI's fallback rates are $2.20, $0.55, $6.60). | (logs only) |

Consequences that are design, not detail:
- Every chat request sends `stream: true` and `stream_options: {"include_usage": true}`; without it nothing reports a cost.
- Success is strict: `finish_reason: "stop"` and non-empty content. Everything else fails, including `length`, an error event, a stream that ends without a finish chunk, and an empty answer. The document stays untouched.
- A response whose usage chunk arrived is written to the ledger with its billed cost, whether or not its text is used (a `length` answer was billed). A request that never reported a cost records nothing. A cancelled stream therefore records nothing, because the cost comes only at the end.
- The §6.1 budget is set to 200,000 tokens, `grok-4.7`'s `long_context_threshold`, below its 500,000-token window. Staying under it keeps the rate single. §14.2's latency question stays open and is measured over a real lecture at M6. The budget did not need it.

**Ledger evidence.** The Python CLI's own ledger (`spend.jsonl` at the repository root, git-ignored, one real line) was read byte by byte with `od -c`: `{"at": "2026-09-24T16:50:11", "course": "…", "lecture": "Week 06 — …", "what": "page", "usd": 0.265218, "billed": true}\n`. This is `json.dumps` with its defaults: `", "` and `": "` separators, non-ASCII escaped as `\uXXXX`, keys in insertion order, floats as Python's `repr`. The Rust ledger writes the same bytes.

**Settled here (the prompt asked for these to be decided, not inherited):**
- *A snapshot does not wait for pending recovery.* Recovered segments enter the log when they arrive and join the next snapshot (spec §5.4 already allows speech times earlier than a committed snapshot). Waiting could hold a snapshot for minutes behind a long gap. The last snapshot runs after the session has drained recovery, so it takes everything.
- *The deferred M2 cutoff minor.* A cutoff taken after a recording's `End`, while the worker is still flushing it (its open-utterance mark still set), now answers `confirmed: false`. The snapshot takes what is logged, reports "transcription still catching up", and the last sentence joins the next batch. Fixed test-first in Task 11, which touches `coordinator.rs`.
- *The prefix summary is an outline derived locally.* When a request would exceed the budget, the omitted prefix is represented by its own `#` headings, with no model request. There is then nothing to cache and nothing to invalidate. Spec §6.1 is rewritten to say so (Task 14).
- *The ledger's home and take-over.* The ledger lives at `~/Library/Application Support/LectureLive/spend.jsonl`. The Python CLI keeps writing its own file until M6, so take-over is incremental: at each start, the CLI ledger's complete lines beyond an import mark (`spend.import.json`) are appended. A crash between that append and the mark is detected by the app ledger already ending with exactly those bytes, so nothing is imported twice.
- *Streamed speech-to-text spend* is one `transcribe` line per recording, written when its live transcript ends, for the audio its connections carried ($0.20/h). Recovery writes one line per REST piece ($0.10/h). Both use the Python CLI's line format with `audio_s` and `billed: false`.
- *Rebuild rule for notes with no state.* A folder with notes but neither a legacy `<stem>.json` nor a v2 sidecar is rebuilt as spec §8 prescribes for a corrupt sidecar: everything after the last `<!-- HH:MM:SS -->` marker is pending. The Python CLI's rule of treating all of it as noted is the P5 flaw.
- *Cross-day recovery (M2 thread).* Before the session starts, the `lecture` command runs a recovery-only session (no audio) for every other day's sidecar in the folder that holds unresolved transcript gaps. Those segments reach that day's transcript and segment log. That day's notes are not rewritten.
- *Transcript repair (M2 thread).* When the segment log is opened, a transcript missing the last segment's line (a crash between the two syncs) gets that line appended; a torn tail that is a prefix of it is replaced.

**Execution:** executing-plans, inline, without approval stops. A fresh reviewer on the most capable model (`opus`) reviews the whole branch before the findings commit (Task 15). Ledger: `.superpowers/sdd/2026-09-25-m3-notes-session-parity/progress.md` (git-excluded through `.git/info/exclude`).

## Global Constraints

- macOS 13+, Apple Silicon; one user; no telemetry, accounts or servers.
- Toolchain pinned to `rustc 1.96.1` (`rust-toolchain.toml`); workspace `rust-version` 1.89.
- Builds are authorised: `cargo build`, `test`, `run`, `check`, `tree`, `add`. No `npm` or `tauri` build. Never run `cargo clean`. Do not touch `apps/desktop` or `target/release/bundle`: the packaged "LectureLive Canary.app" holds this Mac's Microphone and Screen Recording grants, tied to its ad-hoc signature.
- Live calls: `grok-4.7` notes, polish and page requests on synthetic or fixture lectures only, at most $2 in total, each request's cost recorded from `usage.cost_in_usd_ticks`. STT with synthesised speech only (`say`, or `crates/core/tests/fixtures/stt/speech.wav`), a few minutes. Every gate test runs in `cargo test` against fakes, without network. Live tests are `#[ignore]` and read `GROK_API_KEY` from the environment or the repository's `.env`.
- Never run migration, initialisation or polish on a folder that holds a real lecture's notes. Work on synthetic folders or on copies under `~/Library/Application Support/LectureLive/m3-*`.
- Any check that changes the default output ends with the output it began with (normally "MacBook Pro Speakers", `BuiltInSpeakerDevice`). `say -a "BlackHole 2ch"` feeds loopback without changing it.
- The repository is public: commit no audio except synthesised `say` output, and no real lecture notes. Recordings and test folders stay under `~/Library/Application Support/LectureLive/`. Stage files by name, never `git add -A`.
- Commit messages: imperative, neutral, what and why. No `Co-Authored-By` or any other attribution line, whatever a harness reminder says. No names.
- Do not edit files with Python scripts, and do not write or run Python scripts to produce golden values. In the Bash tool `ls` is `eza`: use `command ls` or explicit paths.
- Any task that creates or changes UI (desktop app views, `notes_template.html`, the study page, any page a person looks at) invokes `/frontend-design:frontend-design` before any markup is written (spec §9.4). M3 embeds `notes_template.html` unchanged; if its output or the template needs a visual change, that task invokes the skill first and the handover says so.
- `live_notes.py` changes in exactly one place: it exits when the folder holds a v2 sidecar (Task 8, with the Edit tool). `notes_template.html`, `pyproject.toml`, `.venv/` and `README.md` stay unchanged. In `docs/`, only this plan, the M3 section and M3 Status row of `docs/milestones.md`, and spec §6 and §8 change.
- Crate APIs below are written against the versions named. Where an API differs, adapt the implementation and keep the task's tests unchanged: the tests are the contract. The one exception is a test that encodes an unverified fact about an external system that live evidence contradicts. Change such a test only with the evidence recorded as a Ruling in the ledger.
- When a check fails, record the observation and the spec §14.1 fallback it points to. Do not build the fallback. §14.1 covers loopback only, so a notes failure is recorded with "no §14.1 fallback applies".

## Review Focus

1. **The notes are edited by hand while a lecture runs**, between snapshots or during a polish. Expected: the edit is kept as the new revision and the next block is appended after it. A polish whose request began before the edit is not written, and the notes stay as edited. Pinned by `notesfile::tests::an_external_edit_is_kept_and_the_block_appended_after_it` (Task 6) and `notesfile::tests::a_polish_over_notes_edited_meanwhile_is_not_written` (Task 9).
2. **A folder the Python CLI left mid-snapshot** (its state holds a `commit` entry: the block was being appended when it crashed). Expected: before migration, the half-written block is finished or removed exactly as `live_notes.py`'s `recover_commit` would, and the migrated cursors match. Pinned by `folder::tests::a_legacy_half_written_snapshot_is_undone_before_migration` (Task 8).
3. **A lecture that runs past midnight.** Expected: the files keep the start day's stem. The timeline orders a 23:59:58 line before a 00:00:03 slide, though the strings sort the other way. Imported legacy lines after midnight roll to the next day. Pinned by `timeline::tests::midnight_orders_by_full_time_not_by_the_clock_string` (Task 2) and `folder::tests::imported_lines_after_midnight_roll_to_the_next_day` (Task 8).
4. **Ctrl-C twice while stopping, with recovery of a long gap in progress.** Expected: the second press stops waiting for recovery. The gap stays unresolved for the next session, the recording is intact, and the last snapshot still runs. Pinned by `coordinator::tests::a_second_stop_abandons_pending_recovery` (Task 11).
5. **Non-ASCII course and lecture names** (the real folder names use an em dash: "Week 06 — …"). Expected: the ledger and the page cache escape them as `—` exactly as Python's `json.dumps` does, the study page's file name keeps the title after the dash, and HTML-escaped fills keep them readable. Pinned by `spend::tests::a_line_is_byte_identical_to_the_python_clis` (Task 7) and `page::tests::the_page_is_named_after_the_lecture_title` (Task 10).

## File Structure

| File | Responsibility | Task |
|---|---|---|
| `crates/core/src/pyjson.rs` | JSON exactly as Python's `json.dumps` writes it; Python rounding | 1 |
| `crates/core/src/notes/prompts.rs` | System prompts and user messages carried over from `live_notes.py` | 1 |
| `crates/core/src/session/sidecar.rs` | New fields: `lecture_date`, `notes: NotesState`, `slides: Vec<SlideEntry>` | 2 |
| `crates/core/src/notes/timeline.rs` | Batch, timeline with tie rule and slide-boundary splits, embed lines, relpath | 2 |
| `crates/core/src/notes/context.rs` | Token estimate, budget, document context with a local outline | 3 |
| `crates/core/src/notes/chat.rs` | SSE parser, stream state, strict success, chat client, cost | 5 |
| `crates/core/tests/support/fake_sse.rs`, `tests/notes_chat.rs` | Fake SSE endpoint and chat integration tests | 5 |
| `crates/core/src/notes/embeds.rs` | `clean_output`, embed validation and repair | 6 |
| `crates/core/src/session/files.rs` | `LectureFiles`: every path of a lecture | 7 |
| `crates/core/src/session/notesfile.rs` | Revision check, commit journal, launch recovery, polish replace | 7, 9 |
| `crates/core/src/session/spend.rs` | Spend ledger: Python line format, read, take-over, `lecture spend` view | 4 |
| `crates/core/src/stt/stream.rs`, `session/coordinator.rs` | `Streamed` event; spend lines for streamed and recovered audio | 4 |
| `crates/core/src/session/segments.rs` | `Imported` source, custom transcript path, transcript repair, session marker, imported log | 8 |
| `crates/core/src/session/folder.rs` | Initialisation table, legacy migration, rebuild, slide files, cross-day recovery | 8 |
| `live_notes.py` | Exits in a folder with a v2 sidecar | 8 |
| `crates/core/src/notes/polish.rs` | Polish request and validation | 9 |
| `crates/core/src/notes/page.rs` | Study page: budget, typeset, re-cut, cache, single-pass fill | 10 |
| `crates/core/src/session/coordinator.rs` | `Store`, `spawn_with_store`, drain that serves stores, second stop, cutoff fix | 11 |
| `crates/core/src/session/lecture.rs` | Snapshot, polish, page, slide watcher, whole-lecture run | 11 |
| `crates/core/tests/lecture_gate.rs` | The M3 gate against fakes | 11 |
| `crates/cli/src/main.rs` | `lecture`, `lecture page`, `lecture spend` | 12 |
| `crates/core/tests/notes_live.rs` | Live checks, `#[ignore]` | 13 |
| `docs/spec.md` §6, §8 | Rewritten to M3's evidence and design | 14 |
| `docs/milestones.md` M3 section + Status row | Gate lines, Findings, Status | 15 |

---

### Task 1: Python-format JSON and the prompts (milestone task 1)

**Files:**
- Create: `crates/core/src/pyjson.rs`, `crates/core/src/notes/mod.rs`, `crates/core/src/notes/prompts.rs`
- Modify: `crates/core/src/lib.rs` (`pub mod notes; pub mod pyjson;`), `crates/core/Cargo.toml` (`serde_json` gains `preserve_order`; `cargo add -p lecturelive-core regex sha2 base64`)

**Interfaces:**
- Produces: `pyjson::{dumps(&serde_json::Value) -> String, float_repr(f64) -> String, round_int(f64) -> i64, round_to(f64, usize) -> f64}`.
- Produces: `notes::prompts::{notes_system(&str) -> String, polish_system(&str) -> String, page_system(&str, u32, u32) -> String, revise_system(u32) -> String, notes_user(doc_context: &str, timeline: &str, embeds: &[String], hint: &str) -> String, polish_user(title: &str, doc: &str, transcript: &str) -> String, page_user(doc: &str, words: usize, slide_numbers: &[String], budget: u32) -> String, revise_user(page: &str, words: usize, budget: u32) -> String, title(course: &str, folder: &str, date: NaiveDate) -> String}`.

`preserve_order` makes `serde_json::Map` keep insertion order crate-wide, which `dumps` needs to write keys as Python does. Map equality stays order-insensitive, so no existing test changes meaning.

- [ ] **Step 1: Write the failing tests**

`crates/core/src/pyjson.rs`, test module only:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The Python CLI's real ledger line (read with `od -c`), with the names replaced.
    #[test]
    fn a_ledger_line_is_the_python_clis_bytes() {
        let v = json!({"at": "2026-09-24T16:50:11", "course": "Machine Learning", "lecture": "Week 06 — Optimisation", "what": "page", "usd": 0.265218, "billed": true});
        assert_eq!(dumps(&v), r#"{"at": "2026-09-24T16:50:11", "course": "Machine Learning", "lecture": "Week 06 — Optimisation", "what": "page", "usd": 0.265218, "billed": true}"#);
    }

    #[test]
    fn floats_are_pythons_repr() {
        for (x, s) in [
            (0.265218, "0.265218"),
            (5.6e-05, "5.6e-05"),
            (1e-06, "1e-06"),
            (0.0001, "0.0001"),
            (1e-05, "1e-05"),
            (12.0, "12.0"),
            (12.3, "12.3"),
            (1790.5, "1790.5"),
            (0.0, "0.0"),
            (1e16, "1e+16"),
            (9999999999999998.0, "9999999999999998.0"),
            (123456789.0, "123456789.0"),
            (-0.5, "-0.5"),
            (0.1 + 0.2, "0.30000000000000004"),
        ] {
            assert_eq!(float_repr(x), s, "{x:e}");
        }
    }

    #[test]
    fn strings_are_escaped_as_python_escapes_them() {
        assert_eq!(dumps(&json!("a\"b\\c\nd\te\u{1}\u{7f}ü😀/")), r#""a\"b\\c\nd\te\u0001\u007fü😀/""#);
    }

    #[test]
    fn integers_lists_and_objects_keep_their_order() {
        assert_eq!(dumps(&json!({"n": 3, "l": [1, 2.5, null, false], "e": {}, "a": []})), r#"{"n": 3, "l": [1, 2.5, null, false], "e": {}, "a": []}"#);
    }

    #[test]
    fn rounding_is_pythons() {
        assert_eq!([round_int(2.5), round_int(3.5), round_int(0.5), round_int(-2.5), round_int(16.5), round_int(15.504)], [2, 4, 0, -2, 16, 16]);
        assert_eq!(round_to(2.675, 2), 2.67, "the binary value is below the half");
        assert_eq!(round_to(0.125, 2), 0.12, "an exact tie goes to even");
        assert_eq!(round_to(0.375, 2), 0.38);
        assert_eq!(round_to(0.0012019999, 6), 0.001202);
        assert_eq!(round_to(12.34, 1), 12.3);
    }
}
```

`crates/core/src/notes/prompts.rs`, test module only:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::pyjson::round_int;

    fn live_notes_source() -> String {
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../live_notes.py")).expect("live_notes.py stays until M6; M6 freezes these goldens before removing it")
    }

    /// The body of `def name(…)`'s `return f"""…"""`, evaluated as Python evaluates it for these arguments.
    fn python_fstring(src: &str, name: &str, args: &[(&str, &str)]) -> String {
        let def = src.find(&format!("def {name}(")).unwrap_or_else(|| panic!("def {name} in live_notes.py"));
        let open = def + src[def..].find("return f\"\"\"").expect("an f-string body") + "return f\"\"\"".len();
        let close = open + src[open..].find("\"\"\"").expect("its end");
        let mut out = String::new();
        let mut chars = src[open..close].chars();
        while let Some(c) = chars.next() {
            match c {
                '{' => {
                    let expr: String = chars.by_ref().take_while(|&c| c != '}').collect();
                    out += &eval(&expr, args);
                }
                '\\' => match chars.next() {
                    Some('\\') => out.push('\\'),
                    other => panic!("an escape this evaluator does not know in {name}: \\{other:?}"),
                },
                c => out.push(c),
            }
        }
        out
    }

    fn eval(expr: &str, args: &[(&str, &str)]) -> String {
        if let Some((_, v)) = args.iter().find(|(k, _)| *k == expr) {
            return v.to_string();
        }
        let budget: f64 = args.iter().find(|(k, _)| *k == "budget").map(|(_, v)| v.parse().unwrap()).unwrap_or_else(|| panic!("{{{expr}}} needs the budget"));
        let factor = match expr {
            "round(budget * 0.65)" => 0.65,
            "round(budget * 0.15)" => 0.15,
            "round(budget * 0.2)" => 0.2,
            other => panic!("an expression live_notes.py gained: {{{other}}}"),
        };
        round_int(budget * factor).to_string()
    }

    #[test]
    fn system_prompts_are_live_notes_py_literals() {
        let src = live_notes_source();
        for course in ["Machine Learning", "Statistik für KI — Grundlagen"] {
            assert_eq!(notes_system(course), python_fstring(&src, "notes_system", &[("course", course)]));
            assert_eq!(polish_system(course), python_fstring(&src, "polish_system", &[("course", course)]));
            for budget in [600u32, 650, 1_150, 2_500] {
                for max_slides in [2u32, 5, 8] {
                    let (b, m) = (budget.to_string(), max_slides.to_string());
                    let expected = python_fstring(&src, "page_system", &[("course", course), ("budget", &b), ("max_slides", &m)]);
                    assert_eq!(page_system(course, budget, max_slides), expected, "{budget} {max_slides}");
                }
            }
        }
        for budget in [600u32, 2_500] {
            assert_eq!(revise_system(budget), python_fstring(&src, "revise_system", &[("budget", &budget.to_string())]));
        }
    }

    /// Hand goldens from live_notes.py's make_notes, polish_notes, typeset_page and the title line.
    #[test]
    fn user_messages_are_the_python_clis() {
        assert_eq!(
            notes_user("# T\n", "[10:00:01] hello", &["![Slide 1](slides/slide_01_100000.png)".into()], "momentum"),
            "Notes document so far:\n<<<\n# T\n\n>>>\n\nNew material since the last snapshot, in chronological order:\n<<<\n[10:00:01] hello\n>>>\n\nSlide images are attached in the same order as their markers. These embed lines are mandatory, each exactly once, verbatim:\n![Slide 1](slides/slide_01_100000.png)\n\nFocus hint from the student: momentum\n\nWrite only the new notes to append."
        );
        assert_eq!(
            notes_user("d", "t", &[], ""),
            "Notes document so far:\n<<<\nd\n>>>\n\nNew material since the last snapshot, in chronological order:\n<<<\nt\n>>>\n\nWrite only the new notes to append."
        );
        assert_eq!(
            polish_user("# C — W — 2026-09-25", "doc", "tr"),
            "Use this exact title line: # C — W — 2026-09-25\n\nNotes as written during the lecture:\n<<<\ndoc\n>>>\n\nFull transcript:\n<<<\ntr\n>>>\n\nWrite the complete replacement document."
        );
        assert_eq!(
            page_user("doc", 1, &["3".into(), "7".into()], 600),
            "The complete notes (1 words):\n<<<\ndoc\n>>>\n\nSlide images are attached in this order: Slide 3, Slide 7.\n\nWrite the study page in at most 600 words."
        );
        assert_eq!(page_user("doc", 1, &[], 600), "The complete notes (1 words):\n<<<\ndoc\n>>>\n\nWrite the study page in at most 600 words.");
        assert_eq!(revise_user("<p>x</p>", 812, 600), "The page, 812 words:\n<<<\n<p>x</p>\n>>>\n\nWrite it in at most 600 words.");
        let date = chrono::NaiveDate::from_ymd_opt(2026, 9, 25).unwrap();
        assert_eq!(title("Machine Learning", "Week 01 — Optimisation", date), "# Machine Learning — Week 01 — Optimisation — 2026-09-25");
    }

    /// Every fixed fragment of the user messages is still written that way in live_notes.py.
    #[test]
    fn user_message_fragments_appear_in_live_notes_py() {
        let src = live_notes_source();
        for fragment in [
            "Notes document so far:\\n<<<\\n",
            "New material since the last snapshot, in chronological order:\\n<<<\\n",
            "Slide images are attached in the same order as their markers. These embed lines are mandatory, each exactly once, verbatim:\\n",
            "Focus hint from the student: {hint}\\n\\n",
            "Write only the new notes to append.",
            "Use this exact title line: {title}\\n\\n",
            "Notes as written during the lecture:\\n<<<\\n{doc}\\n>>>\\n\\n",
            "Full transcript:\\n<<<\\n{transcript}\\n>>>\\n\\n",
            "Write the complete replacement document.",
            "The complete notes ({notes_words(doc)} words):\\n<<<\\n{doc}\\n>>>\\n\\n",
            "Slide images are attached in this order: ",
            "Write the study page in at most {budget} words.",
            "The page, {words} words:\\n<<<\\n{out}\\n>>>\\n\\nWrite it in at most {budget} words.",
            "title = f\"# {course} — {lecture_dir.name} — {today:%Y-%m-%d}\"",
        ] {
            assert!(src.contains(fragment), "live_notes.py no longer contains {fragment:?}");
        }
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p lecturelive-core --lib pyjson notes::prompts`
Expected: FAIL to compile (`dumps`, `notes_system` … not defined).

- [ ] **Step 3: Implement**

`crates/core/Cargo.toml`: `serde_json = { version = "1.0.151", features = ["preserve_order"] }`; then `cargo add -p lecturelive-core regex sha2 base64` (resolves to the lock's `regex 1.13.1`, `sha2 0.10.9`, `base64 0.22.1` unless newer are published).

`crates/core/src/pyjson.rs` above the tests:

```rust
//! JSON exactly as Python's `json.dumps` writes it with default arguments: the format of the files
//! the Python CLI shares with this app (the spend ledger, the study page cache; spec §8, §6.4).
use std::fmt::Write;

use serde_json::Value;

/// `", "` and `": "` separators, non-ASCII as `\uXXXX`, floats as Python's `repr`, keys in insertion order.
pub fn dumps(v: &Value) -> String {
    let mut out = String::new();
    write_value(&mut out, v);
    out
}

fn write_value(out: &mut String, v: &Value) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => match (n.as_i64(), n.as_u64()) {
            (Some(i), _) => write!(out, "{i}").unwrap(),
            (_, Some(u)) => write!(out, "{u}").unwrap(),
            _ => out.push_str(&float_repr(n.as_f64().unwrap_or(0.0))),
        },
        Value::String(s) => write_str(out, s),
        Value::Array(items) => {
            out.push('[');
            for (i, x) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_value(out, x);
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            for (i, (k, x)) in map.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_str(out, k);
                out.push_str(": ");
                write_value(out, x);
            }
            out.push('}');
        }
    }
}

fn write_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            ' '..='~' => out.push(c),
            c => {
                let mut units = [0u16; 2];
                for u in c.encode_utf16(&mut units) {
                    write!(out, "\\u{u:04x}").unwrap();
                }
            }
        }
    }
    out.push('"');
}

/// Python's `repr(float)`: the shortest digits that round-trip, positional from 1e-4 up to 1e16, otherwise `d.ddde±XX`.
pub fn float_repr(x: f64) -> String {
    if x.is_nan() {
        return "NaN".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "Infinity" } else { "-Infinity" }.into();
    }
    if x == 0.0 {
        return if x.is_sign_negative() { "-0.0" } else { "0.0" }.into();
    }
    let sci = format!("{x:e}"); // shortest round-trip digits: "5.6e-5", "-1.2345e2"
    let (mantissa, exp) = sci.split_once('e').expect("{:e} has an exponent");
    let exp: i32 = exp.parse().expect("a decimal exponent");
    let (sign, mantissa) = mantissa.strip_prefix('-').map_or(("", mantissa), |m| ("-", m));
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    if (-4..16).contains(&exp) {
        let point = exp + 1; // digits before the decimal point
        let body = if point <= 0 {
            format!("0.{}{digits}", "0".repeat(point.unsigned_abs() as usize))
        } else if point as usize >= digits.len() {
            format!("{digits}{}.0", "0".repeat(point as usize - digits.len()))
        } else {
            format!("{}.{}", &digits[..point as usize], &digits[point as usize..])
        };
        format!("{sign}{body}")
    } else {
        let m = if digits.len() == 1 { digits } else { format!("{}.{}", &digits[..1], &digits[1..]) };
        format!("{sign}{m}e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.unsigned_abs())
    }
}

/// Python's `round(x)`: to the nearest integer, ties to even.
pub fn round_int(x: f64) -> i64 {
    x.round_ties_even() as i64
}

/// Python's `round(x, n)`: x correctly rounded to n decimals (ties to even on its exact binary value).
pub fn round_to(x: f64, n: usize) -> f64 {
    format!("{x:.n$}").parse().expect("a formatted float parses")
}
```

If `round_to(0.125, 2)` comes out as 0.13 (Rust's precision formatting rounding ties away from zero), replace the body with an exact tie test: format with `n + 25` digits, and when the digits after position `n` are exactly `5` followed by zeros, round to even by hand. The test is the contract.

`crates/core/src/notes/mod.rs`:

```rust
//! Notes (spec §6): prompts, batches, context, the SSE client, embed repair, polish and the study page.
pub mod prompts;
```

`crates/core/src/notes/prompts.rs` above the tests (the texts are `live_notes.py`'s, copied character for character; `\\[` in its f-strings is `\[` here):

```rust
//! The prompts, carried over unchanged from `live_notes.py` (spec §6.1, §6.4) and pinned to its source by the tests.
use chrono::NaiveDate;

use crate::pyjson::round_int;

pub fn notes_system(course: &str) -> String {
    format!(
        r##"You are the note-taker for a lecture in the course "{course}".
You maintain ONE running Markdown notes document for today's lecture. Each call you receive the document so far plus new material: a timestamped speech-to-text transcript and slide screenshots marked at the moment they were shown. You write only the new notes to append.
Rules:
- Continue the existing heading structure: `##` for topics, `###` for subtopics. If the new material continues the last section, continue it without repeating its heading. Never add a document title or "snapshot" headings.
- Concise bullets: key points, definitions, formulas, examples, and anything the lecturer emphasised or flagged as examinable.
- Slides are the authority on terminology, formulas and figures; fix obvious speech-recognition errors from context. Do not add material that was not said or shown.
- Words after a ">>> Slide N shown" marker were said while that slide was up. Every embed line you are given MUST appear in your output exactly once, verbatim, on its own line: directly under the heading whose content the slide illustrates, followed by one line saying what the slide shows. If nothing in the new material relates to a slide, put it under its own `### Slide N` heading with that one-line description.
- Do not repeat what the document already says; add only what is new.
- Output Markdown only: no preamble, no code fences."##
    )
}

pub fn polish_system(course: &str) -> String {
    format!(
        r##"You turn raw, incrementally written lecture notes from the course "{course}" into one clean study document.
You receive the notes as written during the lecture and the full transcript. Produce the complete replacement document in Markdown:
- Title line, then a 3-5 sentence summary of the lecture.
- Sections by topic in lecture order (`##` / `###`), merging duplicates and removing snapshot or session headings.
- Keep every substantive point, definition, formula and example; use the transcript to fill gaps and fix speech-recognition errors. Do not invent content.
- Keep every `![Slide N](...)` embed line exactly once, verbatim, next to the content it illustrates.
- End with "Key takeaways", a short "Glossary" of terms introduced, and "Questions / follow-ups" if anything is unclear.
Output Markdown only: no preamble, no code fences."##
    )
}

pub fn page_system(course: &str, budget: u32, max_slides: u32) -> String {
    let b = budget as f64;
    let (topics, glossary, questions) = (round_int(b * 0.65), round_int(b * 0.15), round_int(b * 0.2));
    format!(
        r##"You make the high-yield study page for a lecture in the course "{course}". …"##
    )
}
```

(The `page_system` literal is the whole of `live_notes.py` lines 107–134 with `{round(budget * 0.65)}` → `{topics}`, `{round(budget * 0.15)}` → `{glossary}`, `{round(budget * 0.2)}` → `{questions}`, `{budget}` and `{max_slides}` kept, and every `\\` written as a single `\`. The implementer copies it from the source; the golden test compares every character, so an abbreviation cannot pass.)

```rust
pub fn revise_system(budget: u32) -> String {
    format!(
        r##"You shorten a study page that is over its length budget. Rewrite it to at most {budget} words of visible text, math counting as words, by cutting the lowest-yield material first: second examples, secondary detail, lesser glossary terms, extra questions, restatements. Keep its order, its markup and components exactly as they are used, every formula a problem needs, and every figure you keep unchanged. Output the complete page in the same format and nothing else."##
    )
}

/// `make_notes`'s user text: the document context (§6.1), the batch's timeline, its embed lines and the hint.
pub fn notes_user(doc_context: &str, timeline: &str, embeds: &[String], hint: &str) -> String {
    let mut user = format!("Notes document so far:\n<<<\n{doc_context}\n>>>\n\n");
    user += "New material since the last snapshot, in chronological order:\n<<<\n";
    user += &format!("{timeline}\n>>>\n\n");
    if !embeds.is_empty() {
        user += "Slide images are attached in the same order as their markers. These embed lines are mandatory, each exactly once, verbatim:\n";
        user += &format!("{}\n\n", embeds.join("\n"));
    }
    if !hint.is_empty() {
        user += &format!("Focus hint from the student: {hint}\n\n");
    }
    user += "Write only the new notes to append.";
    user
}

pub fn polish_user(title: &str, doc: &str, transcript: &str) -> String {
    format!(
        "Use this exact title line: {title}\n\nNotes as written during the lecture:\n<<<\n{doc}\n>>>\n\nFull transcript:\n<<<\n{transcript}\n>>>\n\nWrite the complete replacement document."
    )
}

/// `typeset_page`'s user text; `words` is the notes' word count, `slide_numbers` the embeds' numbers in order.
pub fn page_user(doc: &str, words: usize, slide_numbers: &[String], budget: u32) -> String {
    let mut user = format!("The complete notes ({words} words):\n<<<\n{doc}\n>>>\n\n");
    if !slide_numbers.is_empty() {
        let list: Vec<String> = slide_numbers.iter().map(|n| format!("Slide {n}")).collect();
        user += &format!("Slide images are attached in this order: {}.\n\n", list.join(", "));
    }
    user += &format!("Write the study page in at most {budget} words.");
    user
}

pub fn revise_user(page: &str, words: usize, budget: u32) -> String {
    format!("The page, {words} words:\n<<<\n{page}\n>>>\n\nWrite it in at most {budget} words.")
}

/// The notes' first line and polish's title (`live_notes.py` `title`).
pub fn title(course: &str, folder: &str, date: NaiveDate) -> String {
    format!("# {course} — {folder} — {}", date.format("%Y-%m-%d"))
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p lecturelive-core --lib pyjson notes::prompts`
Expected: PASS (7 tests). Then `cargo test -p lecturelive-core` in full: every M0–M2 test still passes with `preserve_order`.

- [ ] **Step 5: Commit**

```bash
git add crates/core/Cargo.toml Cargo.lock crates/core/src/lib.rs crates/core/src/pyjson.rs crates/core/src/notes/mod.rs crates/core/src/notes/prompts.rs
git commit -m "Carry the notes, polish and page prompts over from live_notes.py with golden tests

The system prompts are compared with live_notes.py's f-strings evaluated from its source,
so any drift fails the build. pyjson writes JSON exactly as Python's json.dumps does, the
format of the ledger and page cache the two tools share."
```

---

### Task 2: Sidecar notes state, batch and timeline (milestone task 2)

**Files:**
- Modify: `crates/core/src/session/sidecar.rs` (fields `lecture_date`, `notes`, `slides`; types `NotesState`, `SlideEntry`)
- Create: `crates/core/src/notes/timeline.rs`; `notes/mod.rs` gains `pub mod timeline;`

**Interfaces:**
- Consumes: `session::segments::{Segment, read}`, `session::sidecar::wall_time_at`, `audio::recorder::SAMPLE_RATE`.
- Produces:
  - `sidecar::NotesState { revision: u64, len: u64, sha256: String, segment_cursor: u64, slide_index: u32 }` (`Default`, `is_empty()`).
  - `sidecar::SlideEntry { index: u32, file: String /* relative to the lecture folder, or absolute */, shown_at: DateTime<Local> }`.
  - `Sidecar { …, lecture_date: Option<NaiveDate>, notes: NotesState, slides: Vec<SlideEntry> }`.
  - `notes::timeline::{hms(DateTime<Local>) -> String, relpath(&Path, &Path) -> String, embed_line(&SlideEntry, lecture_dir: &Path, notes_dir: &Path) -> String, timeline(&[Segment], &[SlideEntry], lecture_dir: &Path, notes_dir: &Path) -> String}`.
  - `notes::timeline::Batch { positions: Range<u64>, segments: Vec<Segment>, slides: Vec<SlideEntry>, hint: String }` with `take(log: &Path, sc: &Sidecar, upto: u64, hint: &str) -> Result<Batch>`, `is_empty()`, `slide_to(current: u32) -> u32`, `words() -> usize`.

- [ ] **Step 1: Write the failing tests**

Append to `sidecar.rs`'s test module:

```rust
    #[test]
    fn notes_state_and_slides_round_trip_and_an_m2_sidecar_is_written_back_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let path = sidecar_path(dir.path(), "x");
        let mut s = Sidecar { lecture_date: chrono::NaiveDate::from_ymd_opt(2026, 9, 25), ..Sidecar::default() };
        s.notes = NotesState { revision: 3, len: 120, sha256: "ab".repeat(32), segment_cursor: 7, slide_index: 2 };
        s.slides.push(SlideEntry { index: 1, file: "slides/slide_01_100512.png".into(), shown_at: Local.with_ymd_and_hms(2026, 9, 25, 10, 5, 12).unwrap() });
        s.save(&path).unwrap();
        assert_eq!(Sidecar::load(&path).unwrap().unwrap(), s);

        let m2 = r#"{"version":2,"recordings":[],"gaps":[]}"#;
        let old: Sidecar = serde_json::from_str(m2).unwrap();
        assert_eq!((old.lecture_date, old.notes.clone(), old.slides.len()), (None, NotesState::default(), 0));
        assert_eq!(serde_json::to_string(&old).unwrap(), m2, "fields M3 did not set are not written");
    }
```

`crates/core/src/notes/timeline.rs`, test module only:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::segments::{NewSegment, SegmentLog, SegmentSource};
    use crate::session::sidecar::wall_time_at;
    use crate::stt::transcript::Word;
    use chrono::{Duration, TimeZone};
    use std::path::Path;
    use uuid::Uuid;

    fn anchor() -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 9, 25, 10, 0, 0).unwrap()
    }

    /// A segment of a recording anchored at 10:00:00, its words starting at the given seconds.
    fn seg(id: u64, text: &str, words: &[(&str, u64)], source: SegmentSource) -> Segment {
        let words: Vec<Word> = words.iter().map(|&(t, s)| Word { text: t.into(), start_sample: s * 16_000, end_sample: s * 16_000 + 8_000 }).collect();
        let (start, end) = (words.first().map_or(0, |w| w.start_sample), words.last().map_or(16_000, |w| w.end_sample));
        Segment { id, recording_id: Uuid::nil(), start_sample: start, end_sample: end, said_at: wall_time_at(anchor(), start), start: wall_time_at(anchor(), start), end: wall_time_at(anchor(), end), text: text.into(), words, source }
    }

    /// An imported line: no words, said at `at`.
    fn line(id: u64, text: &str, at: DateTime<Local>) -> Segment {
        Segment { id, recording_id: Uuid::nil(), start_sample: 0, end_sample: 0, said_at: at, start: at, end: at, text: text.into(), words: vec![], source: SegmentSource::Imported }
    }

    fn slide(index: u32, at: DateTime<Local>) -> SlideEntry {
        SlideEntry { index, file: format!("slides/slide_{index:02}_{}.png", at.format("%H%M%S")), shown_at: at }
    }

    const DIR: &str = "/lecture";

    fn tl(segments: &[Segment], slides: &[SlideEntry]) -> String {
        timeline(segments, slides, Path::new(DIR), Path::new(DIR))
    }

    #[test]
    fn speech_and_slides_are_ordered_by_full_time_in_the_clis_format() {
        let segments = [
            seg(0, "Gradient descent.", &[("Gradient", 1), ("descent", 1)], SegmentSource::Live),
            seg(1, "Momentum.", &[("Momentum", 5)], SegmentSource::Live),
            seg(2, "Recovered words.", &[("Recovered", 3), ("words", 3)], SegmentSource::Recovered), // logged last, said earlier
        ];
        assert_eq!(
            tl(&segments, &[slide(1, anchor() + Duration::seconds(4))]),
            "[10:00:01] Gradient descent.\n[10:00:03] Recovered words.\n[10:00:04] >>> Slide 1 shown (embed: ![Slide 1](slides/slide_01_100004.png))\n[10:00:05] Momentum."
        );
    }

    #[test]
    fn a_slide_comes_before_speech_at_the_same_time() {
        let at = anchor() + Duration::seconds(4);
        assert_eq!(tl(&[line(0, "Said then.", at)], &[slide(2, at)]), "[10:00:04] >>> Slide 2 shown (embed: ![Slide 2](slides/slide_02_100004.png))\n[10:00:04] Said then.");
    }

    #[test]
    fn a_segment_spanning_a_slide_is_split_there_by_its_word_times() {
        let s = seg(0, "One two three four.", &[("One", 0), ("two", 1), ("three", 2), ("four", 3)], SegmentSource::Live);
        let shown = anchor() + Duration::milliseconds(2_500);
        assert_eq!(tl(&[s], &[slide(1, shown)]), "[10:00:00] One two three\n[10:00:02] >>> Slide 1 shown (embed: ![Slide 1](slides/slide_01_100002.png))\n[10:00:03] four.");
    }

    #[test]
    fn a_split_uses_the_word_texts_when_the_text_does_not_align_with_them() {
        let s = seg(0, "One-two three", &[("One", 0), ("two", 1), ("three", 2)], SegmentSource::Live);
        assert_eq!(tl(&[s], &[slide(1, anchor() + Duration::milliseconds(1_500))]).lines().collect::<Vec<_>>()[0], "[10:00:00] One two");
    }

    #[test]
    fn midnight_orders_by_full_time_not_by_the_clock_string() {
        let day2 = Local.with_ymd_and_hms(2026, 9, 26, 0, 0, 3).unwrap();
        let segments = [line(0, "Before midnight.", Local.with_ymd_and_hms(2026, 9, 25, 23, 59, 58).unwrap()), line(1, "After it.", day2 + Duration::seconds(2))];
        let out = tl(&segments, &[slide(4, day2)]);
        assert_eq!(out, "[23:59:58] Before midnight.\n[00:00:03] >>> Slide 4 shown (embed: ![Slide 4](slides/slide_04_000003.png))\n[00:00:05] After it.");
    }

    #[test]
    fn embeds_are_relative_to_the_notes_folder_as_the_cli_writes_them() {
        let s = slide(3, anchor());
        assert_eq!(embed_line(&s, Path::new("/l"), Path::new("/l")), "![Slide 3](slides/slide_03_100000.png)");
        assert_eq!(embed_line(&s, Path::new("/l"), Path::new("/l/notes")), "![Slide 3](../slides/slide_03_100000.png)");
        let elsewhere = SlideEntry { file: "/shots/a.png".into(), ..s };
        assert_eq!(embed_line(&elsewhere, Path::new("/l"), Path::new("/l")), "![Slide 3](../shots/a.png)");
    }

    #[test]
    fn a_batch_is_the_log_after_the_cursor_through_the_cutoff_and_the_slides_after_the_index() {
        let dir = tempfile::tempdir().unwrap();
        let mut log = SegmentLog::open(dir.path(), "lecture_notes_20260925").unwrap();
        for k in 0..5u64 {
            log.append(NewSegment { recording_id: Uuid::nil(), start_sample: k * 16_000, end_sample: k * 16_000 + 8_000, text: format!("line {k} words"), words: vec![], source: SegmentSource::Live }, anchor()).unwrap();
        }
        let mut sc = Sidecar::default();
        sc.notes.segment_cursor = 2;
        sc.notes.slide_index = 1;
        sc.slides = (1..=3).map(|i| slide(i, anchor())).collect();
        let path = crate::session::segments::segments_path(dir.path(), "lecture_notes_20260925");
        let b = Batch::take(&path, &sc, 4, "momentum").unwrap();
        assert_eq!(b.positions, 2..4);
        assert_eq!(b.segments.iter().map(|s| s.id).collect::<Vec<_>>(), vec![2, 3]);
        assert_eq!(b.slides.iter().map(|s| s.index).collect::<Vec<_>>(), vec![2, 3]);
        assert_eq!((b.slide_to(1), b.words(), b.hint.as_str()), (3, 6, "momentum"));
        sc.notes.segment_cursor = 4;
        sc.notes.slide_index = 3;
        let empty = Batch::take(&path, &sc, 4, "").unwrap();
        assert!(empty.is_empty());
        assert_eq!(empty.slide_to(3), 3);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p lecturelive-core --lib sidecar notes::timeline`
Expected: FAIL to compile (`NotesState`, `SlideEntry`, `timeline` … not defined).

- [ ] **Step 3: Implement**

`sidecar.rs`: add `use chrono::NaiveDate;`, the three fields to `Sidecar` (after `open_utterances`), and the types; `Default` sets them empty.

```rust
    /// The day the lecture's files were created; it does not change across midnight (spec §8).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lecture_date: Option<NaiveDate>,
    /// What the notes document holds (spec §6.2, §8).
    #[serde(default, skip_serializing_if = "NotesState::is_empty")]
    pub notes: NotesState,
    /// Registered slides, in registration order (spec §7.3, §8).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub slides: Vec<SlideEntry>,
```

```rust
/// The notes document's committed state: its revision and fingerprint, and the cursors of what it holds.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotesState {
    /// Advances with every commit, polish and accepted external edit.
    pub revision: u64,
    /// Length and SHA-256 (hex) of the notes file at this revision.
    pub len: u64,
    pub sha256: String,
    /// Segment-log positions below this are in the notes.
    pub segment_cursor: u64,
    /// Slides with an index up to this are in the notes.
    pub slide_index: u32,
}

impl NotesState {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlideEntry {
    pub index: u32,
    /// Relative to the lecture folder (absolute when the slides folder is elsewhere).
    pub file: String,
    /// When it was first on screen; for an imported image, its file time.
    pub shown_at: DateTime<Local>,
}
```

`Default for Sidecar` gains `lecture_date: None, notes: NotesState::default(), slides: Vec::new()`.

`crates/core/src/notes/timeline.rs` above the tests:

```rust
//! What a snapshot sends (spec §6.1): the batch after the committed cursors, and its timeline of
//! speech and slide markers in the Python CLI's line format.
use std::ops::Range;
use std::path::{Component, Path};

use anyhow::Result;
use chrono::{DateTime, Local};

use crate::session::segments::{self, Segment};
use crate::session::sidecar::{wall_time_at, Sidecar, SlideEntry};

pub fn hms(t: DateTime<Local>) -> String {
    t.format("%H:%M:%S").to_string()
}

/// Python's `os.path.relpath(path, start)` for absolute paths.
pub fn relpath(path: &Path, start: &Path) -> String {
    let p: Vec<Component> = path.components().collect();
    let s: Vec<Component> = start.components().collect();
    let common = p.iter().zip(&s).take_while(|(a, b)| a == b).count();
    let mut parts: Vec<String> = vec!["..".to_string(); s.len() - common];
    parts.extend(p[common..].iter().map(|c| c.as_os_str().to_string_lossy().into_owned()));
    if parts.is_empty() {
        ".".into()
    } else {
        parts.join("/")
    }
}

/// `![Slide N](path)`, the path relative to the notes file's folder, as the Python CLI writes it.
pub fn embed_line(slide: &SlideEntry, lecture_dir: &Path, notes_dir: &Path) -> String {
    format!("![Slide {}]({})", slide.index, relpath(&lecture_dir.join(&slide.file), notes_dir))
}

#[derive(Debug, Clone, PartialEq)]
pub struct Batch {
    /// Segment-log positions [from, to): the cursor moves to `to` when the batch commits.
    pub positions: Range<u64>,
    pub segments: Vec<Segment>,
    /// Registered slides after the committed index, by index.
    pub slides: Vec<SlideEntry>,
    pub hint: String,
}

impl Batch {
    /// The material after the sidecar's cursors, through log position `upto` (a cutoff's count, spec §5.3).
    pub fn take(log: &Path, sc: &Sidecar, upto: u64, hint: &str) -> Result<Self> {
        let from = sc.notes.segment_cursor;
        let upto = upto.max(from);
        let segments: Vec<Segment> = segments::read(log)?.into_iter().filter(|s| (from..upto).contains(&s.id)).collect();
        let mut slides: Vec<SlideEntry> = sc.slides.iter().filter(|s| s.index > sc.notes.slide_index).cloned().collect();
        slides.sort_by_key(|s| s.index);
        Ok(Self { positions: from..upto, segments, slides, hint: hint.to_string() })
    }

    pub fn is_empty(&self) -> bool {
        self.segments.is_empty() && self.slides.is_empty()
    }

    /// The slide cursor after this batch commits.
    pub fn slide_to(&self, current: u32) -> u32 {
        self.slides.iter().map(|s| s.index).max().unwrap_or(current).max(current)
    }

    /// Spoken words in the batch (the snapshot's report).
    pub fn words(&self) -> usize {
        self.segments.iter().map(|s| s.text.split_whitespace().count()).sum()
    }
}

struct Line {
    at: DateTime<Local>,
    slide: bool,
    order: (u64, usize),
    text: String,
}

/// The batch's timeline (spec §6.1): speech and slide markers by full time, a slide first at equal
/// times. A streamed segment that spans a slide's first-shown time is split there by its word times,
/// for the prompt only.
pub fn timeline(segments: &[Segment], slides: &[SlideEntry], lecture_dir: &Path, notes_dir: &Path) -> String {
    let mut lines: Vec<Line> = slides
        .iter()
        .map(|s| Line { at: s.shown_at, slide: true, order: (s.index as u64, 0), text: format!("[{}] >>> Slide {} shown (embed: {})", hms(s.shown_at), s.index, embed_line(s, lecture_dir, notes_dir)) })
        .collect();
    for seg in segments {
        for (k, (at, text)) in split_at_slides(seg, slides).into_iter().enumerate() {
            lines.push(Line { at, slide: false, order: (seg.id, k), text: format!("[{}] {text}", hms(at)) });
        }
    }
    lines.sort_by(|a, b| a.at.cmp(&b.at).then(b.slide.cmp(&a.slide)).then(a.order.cmp(&b.order)));
    lines.into_iter().map(|l| l.text).collect::<Vec<_>>().join("\n")
}

fn split_at_slides(seg: &Segment, slides: &[SlideEntry]) -> Vec<(DateTime<Local>, String)> {
    let whole = vec![(seg.said_at, seg.text.clone())];
    if seg.words.len() < 2 {
        return whole;
    }
    // The recording's anchor, exactly: `start` is anchor + start_sample in whole microseconds (§3.3).
    let anchor = seg.start - chrono::Duration::microseconds((seg.start_sample * 1_000_000 / crate::audio::recorder::SAMPLE_RATE as u64) as i64);
    let times: Vec<DateTime<Local>> = seg.words.iter().map(|w| wall_time_at(anchor, w.start_sample)).collect();
    let mut cuts: Vec<usize> = slides.iter().filter_map(|s| (1..times.len()).find(|&i| times[i - 1] < s.shown_at && s.shown_at <= times[i])).collect();
    if cuts.is_empty() {
        return whole;
    }
    cuts.sort_unstable();
    cuts.dedup();
    let tokens: Vec<&str> = seg.text.split_whitespace().collect();
    let words: Vec<&str> = if tokens.len() == seg.words.len() { tokens } else { seg.words.iter().map(|w| w.text.as_str()).collect() };
    let mut bounds = vec![0];
    bounds.extend(cuts);
    bounds.push(words.len());
    bounds.windows(2).map(|b| (times[b[0]], words[b[0]..b[1]].join(" "))).collect()
}
```

`SegmentSource::Imported` is used by the tests. Add it to `segments.rs` now (`Imported` after `Recovered`, doc comment: "A line imported from the Python CLI's transcript: second resolution, no words (spec §8)"), so this task compiles; Task 8 writes such segments.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p lecturelive-core --lib sidecar notes::timeline`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/session/sidecar.rs crates/core/src/session/segments.rs crates/core/src/notes/mod.rs crates/core/src/notes/timeline.rs
git commit -m "Add the notes cursors and slides to the sidecar, and the snapshot batch and timeline

A batch is the segment log after the committed cursor through the cutoff and the slides
after the committed index. The timeline orders speech and slides by full time, slide
first at equal times, and splits a streamed segment at a slide's first-shown time by its
word times, in the Python CLI's line format."
```

---

### Task 3: Context budget and the prefix outline (milestone task 3)

**Files:**
- Create: `crates/core/src/notes/context.rs`; `notes/mod.rs` gains `pub mod context;`

**Interfaces:**
- Produces: `notes::context::{BUDGET_TOKENS: usize = 200_000, OUTPUT_TOKENS: usize = 16_000, IMAGE_TOKENS: usize = 1_800, OMITTED: &str, RECENT: &str, text_tokens(&str) -> usize, doc_context(doc: &str, rest_tokens: usize, budget: usize) -> String}`.

Why these numbers: `grok-4.7` reports `context_length: 500000` and `long_context_threshold: 200000`, above which every token of the request is billed at twice the rate (plan research). The budget is that threshold. Text is estimated at three characters per token, conservative for English with Markdown and TeX. A slide at up to 1600 px is estimated at 1,800 tokens, and the output allowance of 16,000 covers reasoning plus the block. The omitted prefix is represented by its own `#` headings: deterministic, free, and never stale, so no summary request and no cache exist to invalidate.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_document_that_fits_is_sent_whole() {
        let doc = "# T\n\n## A\n- x\n";
        assert_eq!(doc_context(doc, 1_000, BUDGET_TOKENS), doc);
    }

    #[test]
    fn over_budget_the_prefix_becomes_its_outline_and_the_rest_stays_verbatim() {
        let mut doc = String::from("# Title\n");
        for i in 0..400 {
            doc += &format!("## Topic {i}\n- a point about é—𝛼 number {i}\n");
        }
        let room = 5_000;
        let out = doc_context(&doc, 1_000, OUTPUT_TOKENS + 1_000 + room);
        assert!(text_tokens(&out) <= room, "{} tokens", text_tokens(&out));
        let (head, tail) = out.split_once(&format!("{RECENT}\n")).expect("the recent part is marked");
        assert!(head.starts_with(OMITTED));
        assert!(doc.ends_with(tail), "the recent part is the document's own tail");
        let omitted = &doc[..doc.len() - tail.len()];
        assert!(omitted.ends_with('\n'), "the cut falls at a line start");
        let expected: Vec<&str> = omitted.lines().filter(|l| l.starts_with('#')).collect();
        assert_eq!(head.lines().skip(1).collect::<Vec<_>>(), expected, "the outline is the omitted part's headings");
        assert!(tail.len() > 6_000, "the budget keeps a real recent part");
    }

    #[test]
    fn tokens_count_characters_not_bytes() {
        assert_eq!(text_tokens("ééé"), 1);
        assert_eq!(text_tokens("𝛼𝛼𝛼𝛼"), 2);
        assert_eq!(text_tokens(""), 0);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p lecturelive-core --lib notes::context`
Expected: FAIL to compile.

- [ ] **Step 3: Implement**

```rust
//! How much of the notes a snapshot request carries (spec §6.1).

/// `grok-4.7`'s long-context threshold: above it the whole request is billed at twice the rate (plan research).
pub const BUDGET_TOKENS: usize = 200_000;
/// Reasoning plus the block the model writes.
pub const OUTPUT_TOKENS: usize = 16_000;
/// A slide of up to 1600 px.
pub const IMAGE_TOKENS: usize = 1_800;
/// Characters per token for text: conservative for English with Markdown and TeX.
const CHARS_PER_TOKEN: usize = 3;

pub const OMITTED: &str = "[... earlier part of the document omitted; its headings follow ...]";
pub const RECENT: &str = "[... the most recent part of the document, verbatim ...]";

pub fn text_tokens(s: &str) -> usize {
    s.chars().count().div_ceil(CHARS_PER_TOKEN)
}

/// The whole document while the request fits the budget; otherwise the omitted prefix's headings,
/// then the most recent part verbatim from a line start. Sliced by lines, so never inside a character.
pub fn doc_context(doc: &str, rest_tokens: usize, budget: usize) -> String {
    let room = budget.saturating_sub(rest_tokens + OUTPUT_TOKENS);
    if text_tokens(doc) <= room {
        return doc.to_string();
    }
    let with_cut = |cut: usize| {
        let (head, tail) = doc.split_at(cut);
        let outline: Vec<&str> = head.lines().filter(|l| l.starts_with('#')).collect();
        format!("{OMITTED}\n{}\n{RECENT}\n{tail}", outline.join("\n"))
    };
    let starts: Vec<usize> = std::iter::once(0).chain(doc.match_indices('\n').map(|(i, _)| i + 1)).filter(|&i| i < doc.len()).collect();
    // Later cuts omit more and carry less: the first that fits keeps the most.
    let first_fit = starts.partition_point(|&cut| text_tokens(&with_cut(cut)) > room);
    match starts.get(first_fit) {
        Some(&cut) => with_cut(cut),
        None => {
            let outline: Vec<&str> = doc.lines().filter(|l| l.starts_with('#')).collect();
            format!("{OMITTED}\n{}", outline.join("\n"))
        }
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p lecturelive-core --lib notes::context`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/notes/mod.rs crates/core/src/notes/context.rs
git commit -m "Send the whole notes while a request fits grok-4.7's single-rate budget

Above 200,000 tokens, the long-context threshold, the omitted prefix is carried as its own
headings and the recent part verbatim from a line start. The outline is derived locally,
so there is no summary request and nothing to cache or invalidate."
```

---

### Task 4: Spend ledger, take-over, and speech-to-text spend (milestone task 10)

**Files:**
- Create: `crates/core/src/session/spend.rs`; `session/mod.rs` gains `pub mod spend;`
- Modify: `crates/core/src/stt/stream.rs` (`SttEvent::Streamed`), `crates/core/src/session/coordinator.rs` (`SessionConfig::spend`, `Notification::SpendFailed`, ledger lines), `crates/cli/src/main.rs` (`record` builds `SessionConfig` with `..Default::default()`; the new notification arm)
- Test: `crates/core/tests/stt_stream.rs` (one test added)

**Interfaces:**
- Consumes: `pyjson::{dumps, round_int, round_to}`.
- Produces:
  - `spend::SpendKind { Transcribe, Notes, Polish, Page }` with `as_str()`.
  - `spend::{STREAM_USD_PER_SECOND, BATCH_USD_PER_SECOND}`.
  - `spend::SpendEntry { at: String, course: String, lecture: String, what: String, usd: f64, billed: bool, audio_s: Option<f64> }`.
  - `spend::line(NaiveDateTime, &str, &str, SpendKind, f64, bool, Option<f64>) -> String`, `spend::read(&Path) -> Result<Vec<SpendEntry>>`.
  - `spend::Spend` (Clone): `open(&Path, course, lecture, today: NaiveDate) -> Result<Spend>`, `add(SpendKind, usd, billed, audio_s) -> Result<()>`, `add_at(NaiveDateTime, …)`, `lecture_total() -> f64`, `kind_total(SpendKind) -> f64`.
  - `spend::{take_over(app: &Path, cli: &Path) -> Result<usize>, import_mark(&Path) -> PathBuf, render(&[SpendEntry], columns: usize, color: Paint, ledger: &Path) -> String, money(f64) -> String, bar(f64, usize) -> String}`, `spend::Paint { color: bool, truecolor: bool }` with `paint(&str, &[&str]) -> String`.
  - `SttEvent::Streamed { recording_id: Uuid, samples: u64 }`, sent just before `Ended` when the recording streamed any audio.
  - `SessionConfig::spend: Option<Spend>`, `Notification::SpendFailed(String)`.

- [ ] **Step 1: Write the failing tests**

`crates/core/src/session/spend.rs`, test module only:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn at(d: u32, h: u32, m: u32, s: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 9, d).unwrap().and_hms_opt(h, m, s).unwrap()
    }

    /// The Python CLI's real ledger line, read with `od -c` (plan research), with the names replaced.
    #[test]
    fn a_line_is_byte_identical_to_the_python_clis() {
        assert_eq!(
            line(at(24, 16, 50, 11), "Machine Learning", "Week 06 — Optimisation", SpendKind::Page, 0.265_218_000_1, true, None),
            "{\"at\": \"2026-09-24T16:50:11\", \"course\": \"Machine Learning\", \"lecture\": \"Week 06 \\u2014 Optimisation\", \"what\": \"page\", \"usd\": 0.265218, \"billed\": true}\n"
        );
        assert_eq!(
            line(at(24, 10, 0, 0), "ML", "W", SpendKind::Transcribe, 97.24 * BATCH_USD_PER_SECOND, false, Some(97.24)),
            "{\"at\": \"2026-09-24T10:00:00\", \"course\": \"ML\", \"lecture\": \"W\", \"what\": \"transcribe\", \"usd\": 0.002701, \"billed\": false, \"audio_s\": 97.2}\n"
        );
    }

    #[test]
    fn every_complete_line_is_read_one_cut_short_is_skipped_and_the_next_starts_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("spend.jsonl");
        std::fs::write(&path, format!("{}{{\"at\": \"2026-09-2", line(at(24, 9, 0, 0), "ML", "W", SpendKind::Notes, 0.01, true, None))).unwrap();
        assert_eq!(read(&path).unwrap().len(), 1);
        let spend = Spend::open(&path, "ML", "W", NaiveDate::from_ymd_opt(2026, 9, 24).unwrap()).unwrap();
        spend.add_at(at(24, 9, 5, 0), SpendKind::Polish, 0.02, true, None).unwrap();
        let entries = read(&path).unwrap();
        assert_eq!(entries.iter().map(|e| e.what.as_str()).collect::<Vec<_>>(), vec!["notes", "polish"]);
        assert!(std::fs::read_to_string(&path).unwrap().ends_with("\"billed\": true}\n"));
    }

    #[test]
    fn open_sums_this_lectures_spend_today_and_add_keeps_the_totals() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("spend.jsonl");
        let mut text = String::new();
        text += &line(at(24, 9, 0, 0), "ML", "W", SpendKind::Notes, 0.5, true, None); // yesterday
        text += &line(at(25, 9, 0, 0), "ML", "W", SpendKind::Notes, 0.25, true, None);
        text += &line(at(25, 9, 1, 0), "ML", "Other", SpendKind::Notes, 1.0, true, None);
        std::fs::write(&path, text).unwrap();
        let spend = Spend::open(&path, "ML", "W", NaiveDate::from_ymd_opt(2026, 9, 25).unwrap()).unwrap();
        assert_eq!(spend.lecture_total(), 0.25);
        spend.add_at(at(25, 10, 0, 0), SpendKind::Page, 0.125, true, None).unwrap();
        assert_eq!((spend.lecture_total(), spend.kind_total(SpendKind::Page), spend.kind_total(SpendKind::Notes)), (0.375, 0.125, 0.0));
    }

    #[test]
    fn take_over_imports_the_clis_new_lines_once_even_across_a_crash() {
        let dir = tempfile::tempdir().unwrap();
        let (app, cli) = (dir.path().join("app/spend.jsonl"), dir.path().join("spend.jsonl"));
        let (a, b, c) = (line(at(24, 9, 0, 0), "ML", "W", SpendKind::Notes, 0.1, true, None), line(at(24, 9, 1, 0), "ML", "W", SpendKind::Page, 0.2, true, None), line(at(25, 9, 0, 0), "ML", "W", SpendKind::Polish, 0.3, true, None));
        assert_eq!(take_over(&app, &cli).unwrap(), 0, "no CLI ledger yet");
        std::fs::write(&cli, format!("{a}{b}{{\"at\": \"2026")).unwrap(); // the CLI's last line is still being written
        assert_eq!(take_over(&app, &cli).unwrap(), 2);
        assert_eq!(take_over(&app, &cli).unwrap(), 0, "nothing twice");
        std::fs::write(&cli, format!("{a}{b}{c}")).unwrap();
        // A crash after the append but before the mark: the next take-over sees its own tail and only moves the mark.
        let mark = std::fs::read(import_mark(&app)).unwrap();
        assert_eq!(take_over(&app, &cli).unwrap(), 1);
        std::fs::write(import_mark(&app), mark).unwrap();
        assert_eq!(take_over(&app, &cli).unwrap(), 1);
        assert_eq!(std::fs::read_to_string(&app).unwrap(), format!("{a}{b}{c}"));
        let spend = Spend::open(&app, "ML", "W", NaiveDate::from_ymd_opt(2026, 9, 25).unwrap()).unwrap();
        spend.add_at(at(25, 10, 0, 0), SpendKind::Notes, 0.4, true, None).unwrap();
        assert_eq!(take_over(&app, &cli).unwrap(), 0);
        assert_eq!(read(&app).unwrap().len(), 4);
    }

    #[test]
    fn money_and_bars_are_the_clis() {
        assert_eq!([money(1234.5), money(0.004), money(0.0), money(0.005), money(0.271918)], ["$1,234.50", "<$0.01", "$0.00", "$0.01", "$0.27"]);
        assert_eq!(bar(0.5, 4), "██  ");
        assert_eq!(bar(1.0, 3), "███");
        assert_eq!(bar(0.0149299, 20), format!("▎{}", " ".repeat(19)));
        assert_eq!(bar(0.0, 2), "  ");
    }

    /// Hand golden from live_notes.py's show_spend with 94 columns and no colour.
    #[test]
    fn the_spend_view_is_the_python_clis() {
        let e = |at: &str, course: &str, lecture: &str, what: &str, usd: f64, billed: bool| SpendEntry { at: at.into(), course: course.into(), lecture: lecture.into(), what: what.into(), usd, billed, audio_s: None };
        let entries = [
            e("2026-09-24T16:50:11", "Machine Learning", "Week 06 — Optimisation", "page", 0.265218, true),
            e("2026-09-24T10:00:00", "Machine Learning", "Week 06 — Optimisation", "transcribe", 0.0027, false),
            e("2026-09-25T11:00:00", "Biology", "Week 02", "notes", 0.004, true),
        ];
        let sp = |n: usize| " ".repeat(n);
        let expected = [
            String::new(),
            format!("  Spend{}all time $0.27", sp(69)),
            String::new(),
            format!("  September 2026{}$0.27", sp(69)),
            format!("    Machine Learning  {}{}$0.27", "█".repeat(20), sp(43)),
            format!("    Biology           ▎{}{}<$0.01", sp(19), sp(42)),
            String::new(),
            format!("  Recent lectures{}", sp(73)),
            format!("    25 Sep  Biology  ›  Week 02{}<$0.01", sp(53)),
            "            notes <$0.01".to_string(),
            format!("    24 Sep  Machine Learning  ›  Week 06 — Optimisation{}$0.27", sp(30)),
            "            page $0.27   transcribe <$0.01".to_string(),
            String::new(),
            "  3 paid calls; 1% estimated from published rates, the rest billed by xAI.".to_string(),
            String::new(),
        ]
        .map(|l| l + "\n")
        .concat();
        let plain = Paint { color: false, truecolor: false };
        assert_eq!(render(&entries, 94, plain, Path::new("/ledger/spend.jsonl")), expected);
        assert_eq!(render(&[], 94, plain, Path::new("/ledger/spend.jsonl")), "\n  Nothing spent yet. Every paid call is logged in /ledger/spend.jsonl from the next run.\n\n");
    }
}
```

`crates/core/tests/stt_stream.rs` (appended):

```rust
#[tokio::test]
async fn the_audio_a_recording_streamed_is_reported_before_it_ends() {
    let fake = fake_stt::start(Config::default()).await;
    let mut link = spawn(cfg(&fake)).unwrap();
    let id = begin(&link).await;
    fake.state.wait_accepted(1).await;
    feed(&link, &fake, (0..30).map(|k| speech_frame(id, k))).await;
    end(&link, id, 48_000).await;
    let ev = until_ended(&mut link).await;
    assert_eq!(ev[ev.len() - 2], SttEvent::Streamed { recording_id: id, samples: 48_000 }, "for the spend ledger (spec §8)");
}
```

`coordinator.rs` test module (appended):

```rust
    #[tokio::test]
    async fn streamed_and_recovered_audio_are_written_to_the_ledger() {
        use crate::session::spend::{self, Spend};
        let dir = tempfile::tempdir().unwrap();
        let ledger = dir.path().join("spend.jsonl");
        let spend = Spend::open(&ledger, "Machine Learning", "Week 01", chrono::NaiveDate::from_ymd_opt(2026, 9, 25).unwrap()).unwrap();
        let a = Uuid::new_v4();
        let stt = scripted_stt(move |i| match i {
            SttInput::End { samples, .. } => vec![
                SttEvent::Streamed { recording_id: a, samples: 16_000 },
                SttEvent::Ended { recording_id: a, gap: Some(Gap::new(a, 8_000, Some(samples), GapKind::SttOffline)) },
            ],
            _ => vec![],
        });
        let recovery = scripted_recovery(|j| {
            vec![
                RecoverEvent::Piece { recording_id: j.recording_id, gap_start: j.gap_start, start_sample: j.from, end_sample: j.end, text: "recovered".into(), words: vec![] },
                RecoverEvent::Done { recording_id: j.recording_id, gap_start: j.gap_start },
            ]
        });
        let cfg = SessionConfig { stt: Some(stt), recovery: Some(recovery), spend: Some(spend), ..cfg(dir.path()) };
        run_with(cfg, Script(recording(a, 10))).await.0.unwrap();
        let entries = spend::read(&ledger).unwrap();
        let got: Vec<(&str, f64, bool, Option<f64>)> = entries.iter().map(|e| (e.what.as_str(), e.usd, e.billed, e.audio_s)).collect();
        assert_eq!(got, vec![("transcribe", 0.000056, false, Some(1.0)), ("transcribe", 0.000014, false, Some(0.5))]);
        assert!(entries.iter().all(|e| e.course == "Machine Learning" && e.lecture == "Week 01"));
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p lecturelive-core --lib session::spend coordinator` and `cargo test -p lecturelive-core --test stt_stream the_audio_a_recording_streamed`
Expected: FAIL to compile.

- [ ] **Step 3: Implement**

`crates/core/src/session/spend.rs` above the tests:

```rust
//! The spend ledger (spec §8, §9.1): one line per paid request, in the Python CLI's exact format,
//! kept in the app's data directory and taking over the CLI's ledger.
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use chrono::{Local, NaiveDate, NaiveDateTime};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::fsutil::write_atomic;
use crate::pyjson::{dumps, round_int, round_to};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SpendKind {
    Transcribe,
    Notes,
    Polish,
    Page,
}

impl SpendKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Transcribe => "transcribe",
            Self::Notes => "notes",
            Self::Polish => "polish",
            Self::Page => "page",
        }
    }
}

/// Speech-to-text responses carry no cost, so these published rates are used, `billed: false` (spec §8).
pub const STREAM_USD_PER_SECOND: f64 = 0.20 / 3600.0;
/// The Python CLI's `STT_USD_PER_SECOND`: REST, as recovery uses it.
pub const BATCH_USD_PER_SECOND: f64 = 0.10 / 3600.0;

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SpendEntry {
    pub at: String,
    #[serde(default = "unknown")]
    pub course: String,
    #[serde(default = "unknown")]
    pub lecture: String,
    pub what: String,
    pub usd: f64,
    #[serde(default)]
    pub billed: bool,
    #[serde(default)]
    pub audio_s: Option<f64>,
}

fn unknown() -> String {
    "?".into()
}

/// One ledger line as the Python CLI's `spend.add` writes it.
pub fn line(at: NaiveDateTime, course: &str, lecture: &str, what: SpendKind, usd: f64, billed: bool, audio_s: Option<f64>) -> String {
    let mut v = json!({"at": at.format("%Y-%m-%dT%H:%M:%S").to_string(), "course": course, "lecture": lecture, "what": what.as_str(), "usd": round_to(usd, 6), "billed": billed});
    if let Some(s) = audio_s {
        v["audio_s"] = json!(round_to(s, 1));
    }
    format!("{}\n", dumps(&v))
}

/// Every line that parses; a last line cut short by a crash is skipped (spec §8).
pub fn read(path: &Path) -> Result<Vec<SpendEntry>> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
    };
    Ok(text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect())
}

/// Appends and syncs; after a line cut short, the new one starts on a line of its own.
fn append(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let torn = std::fs::read(path).map(|b| b.last().is_some_and(|&c| c != b'\n')).unwrap_or(false);
    let mut f = OpenOptions::new().create(true).append(true).open(path).with_context(|| format!("open {}", path.display()))?;
    if torn {
        f.write_all(b"\n")?;
    }
    f.write_all(bytes)?;
    f.sync_data().with_context(|| format!("sync {}", path.display()))
}

struct Inner {
    path: PathBuf,
    course: String,
    lecture: String,
    lecture_total: f64,
    kinds: HashMap<SpendKind, f64>,
}

/// The ledger, as one lecture writes to it. Clones share it.
#[derive(Clone)]
pub struct Spend {
    inner: Arc<Mutex<Inner>>,
}

impl Spend {
    /// `today` picks this lecture's spend so far today, the figure the CLI reports.
    pub fn open(path: &Path, course: &str, lecture: &str, today: NaiveDate) -> Result<Self> {
        let day = today.format("%Y-%m-%d").to_string();
        let lecture_total = read(path)?.iter().filter(|e| e.course == course && e.lecture == lecture && e.at.starts_with(&day)).map(|e| e.usd).sum();
        let inner = Inner { path: path.to_path_buf(), course: course.into(), lecture: lecture.into(), lecture_total, kinds: HashMap::new() };
        Ok(Self { inner: Arc::new(Mutex::new(inner)) })
    }

    pub fn add(&self, what: SpendKind, usd: f64, billed: bool, audio_s: Option<f64>) -> Result<()> {
        self.add_at(Local::now().naive_local(), what, usd, billed, audio_s)
    }

    pub fn add_at(&self, at: NaiveDateTime, what: SpendKind, usd: f64, billed: bool, audio_s: Option<f64>) -> Result<()> {
        let mut g = self.inner.lock().expect("the ledger lock");
        g.lecture_total += usd;
        *g.kinds.entry(what).or_default() += usd;
        let l = line(at, &g.course, &g.lecture, what, usd, billed, audio_s);
        append(&g.path, l.as_bytes())
    }

    pub fn lecture_total(&self) -> f64 {
        self.inner.lock().expect("the ledger lock").lecture_total
    }

    /// What this process has spent on one kind of request.
    pub fn kind_total(&self, what: SpendKind) -> f64 {
        self.inner.lock().expect("the ledger lock").kinds.get(&what).copied().unwrap_or(0.0)
    }
}

/// Where take-over records how much of the CLI's ledger it has imported.
pub fn import_mark(app: &Path) -> PathBuf {
    app.with_file_name("spend.import.json")
}

/// Takes over the Python CLI's ledger (spec §8): appends the complete lines it gained since the last
/// take-over. A crash between that append and the mark is caught by the app ledger already ending
/// with exactly those bytes, so nothing is imported twice.
pub fn take_over(app: &Path, cli: &Path) -> Result<usize> {
    let cli_bytes = match std::fs::read(cli) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e).with_context(|| format!("read {}", cli.display())),
    };
    let end = cli_bytes.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
    let mark_path = import_mark(app);
    let mark = std::fs::read(&mark_path).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()).and_then(|v| v["cli_bytes"].as_u64()).unwrap_or(0) as usize;
    let mut imported = 0;
    if end > mark {
        let new = &cli_bytes[mark..end];
        if !std::fs::read(app).unwrap_or_default().ends_with(new) {
            append(app, new)?;
        }
        imported = new.iter().filter(|&&b| b == b'\n').count();
    }
    if end != mark {
        // A CLI ledger shorter than the mark was rewritten by hand: what it holds now is where to continue from.
        write_atomic(&mark_path, json!({"cli_bytes": end}).to_string().as_bytes())?;
    }
    Ok(imported)
}

pub fn money(usd: f64) -> String {
    if usd > 0.0 && usd < 0.005 {
        return "<$0.01".into();
    }
    let fixed = format!("{usd:.2}");
    let (int, frac) = fixed.split_once('.').expect("two decimals");
    let digits: Vec<char> = int.chars().collect();
    let mut grouped = String::new();
    for (i, c) in digits.iter().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(*c);
    }
    format!("${grouped}.{frac}")
}

pub fn bar(fraction: f64, width: usize) -> String {
    let eighths = round_int(fraction.clamp(0.0, 1.0) * width as f64 * 8.0) as usize;
    let partial = [" ", "▏", "▎", "▍", "▌", "▋", "▊", "▉"][eighths % 8];
    let s = format!("{}{partial}", "█".repeat(eighths / 8));
    format!("{:<width$}", s.trim_end())
}

/// The CLI's terminal styles: its study page's signal red and teal when the terminal has true colour.
#[derive(Debug, Clone, Copy)]
pub struct Paint {
    pub color: bool,
    pub truecolor: bool,
}

impl Paint {
    pub fn paint(&self, text: &str, styles: &[&str]) -> String {
        if !self.color || styles.is_empty() || text.is_empty() {
            return text.to_string();
        }
        let codes: Vec<&str> = styles
            .iter()
            .map(|s| match *s {
                "bold" => "1",
                "dim" => "2",
                "red" if self.truecolor => "38;2;242;118;107",
                "red" => "31",
                "teal" if self.truecolor => "38;2;93;184;192",
                _ => "36",
            })
            .collect();
        format!("\x1b[{}m{text}\x1b[0m", codes.join(";"))
    }
}

fn add_to(v: &mut Vec<(String, f64)>, key: &str, usd: f64) {
    match v.iter_mut().find(|(k, _)| k == key) {
        Some((_, total)) => *total += usd,
        None => v.push((key.to_string(), usd)),
    }
}

fn by_amount(v: &mut [(String, f64)]) {
    v.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal)); // stable: ties keep first-seen order
}

/// The Python CLI's `lecture spend` view, line for line.
pub fn render(entries: &[SpendEntry], columns: usize, p: Paint, ledger: &Path) -> String {
    if entries.is_empty() {
        return format!("\n  Nothing spent yet. Every paid call is logged in {} from the next run.\n\n", ledger.display());
    }
    let width = columns.min(92) as isize - 2;
    let mut months: Vec<(String, Vec<(String, f64)>)> = Vec::new();
    let mut lectures: Vec<((String, String, String), Vec<(String, f64)>)> = Vec::new();
    for e in entries {
        let month = e.at.chars().take(7).collect::<String>();
        let i = months.iter().position(|(m, _)| *m == month).unwrap_or_else(|| {
            months.push((month.clone(), Vec::new()));
            months.len() - 1
        });
        add_to(&mut months[i].1, &e.course, e.usd);
        let key = (e.at.chars().take(10).collect::<String>(), e.course.clone(), e.lecture.clone());
        let j = lectures.iter().position(|(k, _)| *k == key).unwrap_or_else(|| {
            lectures.push((key.clone(), Vec::new()));
            lectures.len() - 1
        });
        add_to(&mut lectures[j].1, &e.what, e.usd);
    }
    let total: f64 = entries.iter().map(|e| e.usd).sum();
    let estimated: f64 = entries.iter().filter(|e| !e.billed).map(|e| e.usd).sum();
    let name_w = months.iter().flat_map(|(_, c)| c.iter().map(|(n, _)| n.chars().count())).max().unwrap_or(0).min(34);
    let mut out = String::new();
    let mut row = |left: &str, right: &str, left_style: &[&str], right_style: &[&str], fill: &str| {
        let room = width - left.chars().count() as isize - fill.chars().count() as isize - right.chars().count() as isize;
        let w = (room + right.chars().count() as isize).max(0) as usize;
        out += &format!("{}{}{}\n", p.paint(left, left_style), p.paint(fill, &["teal"]), p.paint(&format!("{right:>w$}"), right_style));
    };
    let mut blank = String::from("\n");
    row("  Spend", &format!("all time {}", money(total)), &["bold"], &["dim"], "");
    months.sort_by(|a, b| a.0.cmp(&b.0));
    let shown = months.len().saturating_sub(3);
    let mut lines = vec![std::mem::take(&mut blank)];
    let _ = &mut lines;
    // Assemble in the CLI's order: a blank line, the header, then per month a blank line and its rows.
    let mut text = String::from("\n");
    text += &out;
    out.clear();
    for (month, courses) in &mut months[shown..] {
        let name = NaiveDate::parse_from_str(&format!("{month}-01"), "%Y-%m-%d").map(|d| d.format("%B %Y").to_string()).unwrap_or_else(|_| month.clone());
        let sum: f64 = courses.iter().map(|(_, u)| u).sum();
        text += "\n";
        row(&format!("  {name}"), &money(sum), &["bold"], &[], "");
        let top = courses.iter().map(|(_, u)| *u).fold(0.0, f64::max);
        by_amount(courses);
        for (course, usd) in courses.iter() {
            let short: String = course.chars().take(name_w).collect();
            row(&format!("    {short:<name_w$}  "), &money(*usd), &[], &[], &bar(if top > 0.0 { usd / top } else { 0.0 }, 20));
        }
        text += &out;
        out.clear();
    }
    text += "\n";
    row("  Recent lectures", "", &["bold"], &[], "");
    text += &out;
    out.clear();
    lectures.sort_by(|a, b| b.0.cmp(&a.0));
    for ((day, course, lecture), kinds) in lectures.iter_mut().take(8) {
        let when = NaiveDate::parse_from_str(day, "%Y-%m-%d").map(|d| d.format("%-d %b").to_string()).unwrap_or_else(|_| day.clone());
        let amount = money(kinds.iter().map(|(_, u)| u).sum());
        let label: String = format!("{course}  ›  {lecture}").chars().take((width - 22).max(0) as usize).collect();
        row(&format!("    {when:<7} {label}"), &amount, &[], &[], "");
        text += &out;
        out.clear();
        by_amount(kinds);
        let parts: Vec<String> = kinds.iter().map(|(k, v)| format!("{k} {}", money(*v))).collect();
        text += &format!("{}\n", p.paint(&format!("            {}", parts.join("   ")), &["dim"]));
    }
    let share = if total > 0.0 { round_int(100.0 * estimated / total) } else { 0 };
    let note = if estimated == 0.0 { "all billed by xAI".to_string() } else { format!("{share}% estimated from published rates, the rest billed by xAI") };
    text += &format!("\n{}\n\n", p.paint(&format!("  {} paid calls; {note}.", entries.len()), &["dim"]));
    text
}
```

(The `render` body above is the shape; the implementer tidies the buffer handling so that `row` writes into one `String` in call order. The golden test fixes the output exactly.)

`stream.rs`: `SttEvent` gains

```rust
    /// The audio this recording's connections carried, for the spend ledger (spec §8); sent just before `Ended`.
    Streamed { recording_id: Uuid, samples: u64 },
```

`Rec` gains `streamed: u64` (0 at `Begin`). In `send`, when the frame was sent (`Ok(Ok(()))`), `rec.streamed += f.valid_samples as u64`. In `end`, after `let Some(rec) = self.rec.take()`, before emitting `Ended`: `if rec.streamed > 0 { self.emit(SttEvent::Streamed { recording_id, samples: rec.streamed }).await; }`.

`coordinator.rs`: `SessionConfig` gains `pub spend: Option<Spend>`, stored in `Coordinator::spend`. `Notification` gains `SpendFailed(String)`. Add:

```rust
    /// A computed speech-to-text cost (spec §8); a ledger that cannot be written is a warning, never a stop (§10).
    fn spend_audio(&self, samples: u64, usd_per_second: f64) {
        let Some(spend) = &self.spend else { return };
        let secs = samples as f64 / crate::audio::recorder::SAMPLE_RATE as f64;
        if let Err(e) = spend.add(SpendKind::Transcribe, secs * usd_per_second, false, Some(secs)) {
            self.notify(Notification::SpendFailed(format!("{e:#}")));
        }
    }
```

`on_stt` gains `SttEvent::Streamed { samples, .. } => self.spend_audio(samples, STREAM_USD_PER_SECOND),`. `on_recovery`'s `Piece` arm calls `self.spend_audio(end_sample - start_sample, BATCH_USD_PER_SECOND)` before the commit (every piece is a request, even one that heard nothing).

`crates/cli/src/main.rs`: `record` builds `SessionConfig { dir, stem, stt: stt_link, recovery, ..Default::default() }`, and its notification `match` gains `Some(Notification::SpendFailed(m)) => eprintln!("warning: {m}"),`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p lecturelive-core --lib session::spend coordinator` and `cargo test -p lecturelive-core --test stt_stream`
Expected: PASS; `cargo build -p lecturelive-cli` builds.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/session/mod.rs crates/core/src/session/spend.rs crates/core/src/stt/stream.rs crates/core/src/session/coordinator.rs crates/core/tests/stt_stream.rs crates/cli/src/main.rs
git commit -m "Write every paid request to a spend ledger in the Python CLI's format

Lines are byte-identical to live_notes.py's spend.add, the ledger takes over the CLI's
lines incrementally without importing any twice, and the spend view is the CLI's.
Speech-to-text costs are computed: one line per recording for the audio its connections
carried and one per recovery request."
```

---

### Task 5: SSE chat client with strict success (milestone task 4)

**Files:**
- Create: `crates/core/src/notes/chat.rs`; `notes/mod.rs` gains `pub mod chat;`
- Create: `crates/core/tests/support/fake_sse.rs` (and `pub mod fake_sse;` in `support/mod.rs`), `crates/core/tests/notes_chat.rs`
- Add (from plan research, already in the working tree): `crates/core/tests/fixtures/notes/{stream_usage,stream_length,stream_no_usage,stream_image_medium}.sse`, `refused_key.json`
- Modify: `crates/core/Cargo.toml` (`reqwest` gains `stream`)

**Interfaces:**
- Consumes: `session::spend::{Spend, SpendKind}`, `stt::protocol::{is_refusal, refusal_message}`.
- Produces:
  - `notes::chat::{CHAT_URL, MODEL, TICKS_PER_USD}`.
  - `SseParser` (`Default`): `feed(&[u8]) -> Vec<String>` (the data of each completed event).
  - `Usage { prompt_tokens, cached_tokens, completion_tokens: u64, ticks: Option<u64> }` with `cost() -> (f64, bool)`.
  - `Reply` (`Default`): `apply(&str) -> Option<String>`, `outcome() -> Result<(), String>`, fields `text`, `finish: Option<String>`, `usage: Option<Usage>`, `error`, `broken: Option<String>`, `done`.
  - `Image { mime: &'static str, base64: String }` with `read(&Path) -> Result<Image>`.
  - `Content::{Text(String), Parts { text: String, images: Vec<Image> }}`.
  - `ChatRequest { what: SpendKind, system: String, content: Content, effort: Option<&'static str>, timeout: Duration }` with `body(model: &str) -> serde_json::Value`.
  - `ChatConfig { url, api_key, model: String, idle_timeout: Duration }` with `new(api_key)`.
  - `ChatError::{Refused(String), Failed(String)}` (Display).
  - `Answer { text: String, usd: Option<f64>, warning: Option<String> }`.
  - `ChatClient::new(ChatConfig, Option<Spend>) -> Result<ChatClient>`; `complete(&self, &ChatRequest, on_delta: &mut (dyn FnMut(&str) + Send)) -> Result<Answer, ChatError>`.
- Test support: `fake_sse::{start(respond) -> FakeSse, Reply::{Stream(Vec<Vec<u8>>), Stall, Status(u16, String)}, events(content: &[&str], finish: Option<&str>, ticks: Option<u64>) -> Vec<u8>, answer(text, ticks) -> Reply, fixture(name) -> Vec<u8>, pieces(Vec<u8>, usize) -> Vec<Vec<u8>>}`; `FakeSse { url, state }`, `SseState::{bodies() -> Vec<Value>, requests() -> usize}`.

- [ ] **Step 1: Write the failing tests**

Unit tests in `chat.rs`:

```rust
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
        assert_eq!(r.apply(r#"{"choices":[{"index":0,"delta":{"content":"## A"}}]}"#), Some("## A".to_string()));
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
```

`crates/core/tests/notes_chat.rs`:

```rust
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
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p lecturelive-core --lib notes::chat` and `cargo test -p lecturelive-core --test notes_chat`
Expected: FAIL to compile.

- [ ] **Step 3: Implement**

`crates/core/Cargo.toml`: `reqwest` features `["multipart", "json", "rustls-no-provider", "stream"]`.

`crates/core/src/notes/chat.rs` above the tests:

```rust
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
        if let Some(e) = v.get("error") {
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
}

impl ChatConfig {
    pub fn new(api_key: String) -> Self {
        Self { url: CHAT_URL.into(), api_key, model: MODEL.into(), idle_timeout: Duration::from_secs(180) }
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
        let http = reqwest::Client::builder().build().context("build the HTTP client")?;
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
```

`crates/core/tests/support/fake_sse.rs`:

```rust
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

/// A successful answer, its text in word-sized deltas, sent in 64-byte pieces.
pub fn answer(text: &str, ticks: u64) -> Reply {
    let deltas: Vec<&str> = text.split_inclusive(' ').collect();
    Reply::Stream(pieces(events(&deltas, Some("stop"), Some(ticks)), 64))
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
```

A status line's reason phrase: reqwest's `StatusCode` Display gives `400 Bad Request` whatever reason the fake sends, so the test's expected message holds.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p lecturelive-core --lib notes::chat` and `cargo test -p lecturelive-core --test notes_chat`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/core/Cargo.toml Cargo.lock crates/core/src/notes/mod.rs crates/core/src/notes/chat.rs crates/core/tests/support/mod.rs crates/core/tests/support/fake_sse.rs crates/core/tests/notes_chat.rs crates/core/tests/fixtures/notes/stream_usage.sse crates/core/tests/fixtures/notes/stream_length.sse crates/core/tests/fixtures/notes/stream_no_usage.sse crates/core/tests/fixtures/notes/stream_image_medium.sse crates/core/tests/fixtures/notes/refused_key.json
git commit -m "Stream notes requests over SSE and accept only a normal finish with output

Every request asks for the usage chunk, the only place a streamed response reports its
cost. Split UTF-8, several events per read and reasoning deltas are handled; a length
finish, an error event, an empty answer or a stream that ends early all fail. Fixtures
are the recorded grok-4.7 streams."
```

---

### Task 6: Embed validation and repair (milestone task 5)

**Files:**
- Create: `crates/core/src/notes/embeds.rs`; `notes/mod.rs` gains `pub mod embeds;`

**Interfaces:**
- Produces: `notes::embeds::{EMBED: LazyLock<Regex>, NOT_PLACED: &str, clean_output(&str, strip_title: bool) -> String, embeds_in(&str) -> Vec<String>, repair(&str, expected: &[String]) -> Repaired}`; `Repaired { text: String, removed: usize, missing: Vec<String> }`.

Rules (spec §6.2): after `clean_output` (a wrapping code fence and any `# ` title stripped, as `live_notes.py` does), each expected embed line must appear exactly once, on its own line, outside code fences. A later own-line copy is removed. Inline copies are removed too, since an image in the middle of a bullet is not placed. Embeds of slides not in the batch are removed. Anything inside a fence is code and stays as written. Expected embeds that end up absent are appended under `### Slides not placed`, each on its own line, with no invented description.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn e(n: u32) -> String {
        format!("![Slide {n}](slides/slide_{n:02}_100000.png)")
    }

    #[test]
    fn an_embed_on_its_own_line_once_is_kept_as_written() {
        let text = format!("## Momentum\n{}\n- A slide of the update rule.\n- More.", e(1));
        let r = repair(&text, &[e(1)]);
        assert_eq!((r.text.as_str(), r.removed, r.missing.len()), (text.as_str(), 0, 0));
    }

    #[test]
    fn later_and_inline_copies_are_removed_and_an_embed_only_inline_is_appended() {
        let text = format!("## A\n{one}\n- see {one} here\n{one}\n- also {two} inline", one = e(1), two = e(2));
        let r = repair(&text, &[e(1), e(2)]);
        assert_eq!(r.text, format!("## A\n{}\n- see  here\n- also  inline\n\n{NOT_PLACED}\n\n{}", e(1), e(2)));
        assert_eq!((r.removed, r.missing.clone()), (3, vec![e(2)]));
    }

    #[test]
    fn embeds_inside_code_fences_do_not_count_and_are_left_alone() {
        let text = format!("## A\n```markdown\n{}\n```\n- b", e(1));
        let r = repair(&text, &[e(1)]);
        assert_eq!(r.text, format!("{text}\n\n{NOT_PLACED}\n\n{}", e(1)));
    }

    #[test]
    fn embeds_of_slides_not_in_the_batch_are_removed() {
        let r = repair(&format!("## A\n{}\n{}\n- b", e(1), e(9)), &[e(1)]);
        assert_eq!((r.text, r.removed), (format!("## A\n{}\n- b", e(1)), 1));
    }

    #[test]
    fn missing_embeds_go_under_their_own_heading_without_a_description() {
        let r = repair("## A\n- b\n\n", &[e(1), e(2)]);
        assert_eq!(r.text, format!("## A\n- b\n\n{NOT_PLACED}\n\n{}\n\n{}", e(1), e(2)));
        assert_eq!(repair("", &[e(1)]).text, format!("{NOT_PLACED}\n\n{}", e(1)));
    }

    #[test]
    fn clean_output_strips_a_wrapping_fence_and_titles_as_the_cli_does() {
        assert_eq!(clean_output("```markdown\n# Title\n## A\n- b\n```\n", true), "## A\n- b");
        assert_eq!(clean_output("# One\n# Two\n\n## A", true), "## A");
        assert_eq!(clean_output("# Title\n\nSummary.", false), "# Title\n\nSummary.");
        assert_eq!(clean_output("```\n- a", true), "- a", "an unclosed fence loses only its opening line");
        assert_eq!(clean_output("# Only a title", true), "");
    }

    #[test]
    fn the_embeds_of_a_document_in_order_once_each() {
        let doc = format!("# T\n{}\n- x\n{}\n{}", e(2), e(1), e(2));
        assert_eq!(embeds_in(&doc), vec![e(2), e(1)]);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p lecturelive-core --lib notes::embeds`
Expected: FAIL to compile.

- [ ] **Step 3: Implement**

```rust
//! What the model wrote, checked before it is committed (spec §6.2, §6.3).
use std::sync::LazyLock;

use regex::Regex;

/// A slide embed as the CLI writes it: `![Slide N](path)`.
pub static EMBED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"!\[Slide \d+\]\([^)]+\)").expect("a valid pattern"));

pub const NOT_PLACED: &str = "### Slides not placed";

/// The Python CLI's `clean_output`: a wrapping code fence removed, and with `strip_title` every leading `# ` title line.
pub fn clean_output(text: &str, strip_title: bool) -> String {
    let mut text = text.trim().to_string();
    if text.starts_with("```") {
        let mut lines: Vec<&str> = text.lines().skip(1).collect();
        if lines.last().is_some_and(|l| l.trim().starts_with("```")) {
            lines.pop();
        }
        text = lines.join("\n").trim().to_string();
    }
    while strip_title && text.starts_with("# ") {
        text = text.split_once('\n').map_or(String::new(), |(_, rest)| rest.trim_start().to_string());
    }
    text
}

/// A document's embeds in order of first appearance, once each.
pub fn embeds_in(doc: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for m in EMBED.find_iter(doc) {
        if !out.iter().any(|e| e == m.as_str()) {
            out.push(m.as_str().to_string());
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq)]
pub struct Repaired {
    pub text: String,
    /// Copies and unexpected embeds taken out.
    pub removed: usize,
    /// Expected embeds that were appended under `### Slides not placed`.
    pub missing: Vec<String>,
}

pub fn repair(text: &str, expected: &[String]) -> Repaired {
    let mut placed = vec![false; expected.len()];
    let (mut out, mut removed, mut fenced) = (Vec::new(), 0, false);
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with("```") || t.starts_with("~~~") {
            fenced = !fenced;
            out.push(line.to_string());
            continue;
        }
        if fenced {
            out.push(line.to_string());
            continue;
        }
        if let Some(i) = expected.iter().position(|e| e == t) {
            if placed[i] {
                removed += 1;
            } else {
                placed[i] = true;
                out.push(line.to_string());
            }
            continue;
        }
        let found = EMBED.find_iter(line).count();
        if found == 0 {
            out.push(line.to_string());
            continue;
        }
        removed += found;
        let rest = EMBED.replace_all(line, "");
        if !rest.trim().is_empty() {
            out.push(rest.trim_end().to_string());
        }
    }
    let missing: Vec<String> = expected.iter().zip(&placed).filter(|(_, p)| !**p).map(|(e, _)| e.clone()).collect();
    let body = out.join("\n");
    let text = match (missing.is_empty(), body.trim().is_empty()) {
        (true, _) => body,
        (false, true) => format!("{NOT_PLACED}\n\n{}", missing.join("\n\n")),
        (false, false) => format!("{}\n\n{NOT_PLACED}\n\n{}", body.trim_end(), missing.join("\n\n")),
    };
    Repaired { text, removed, missing }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p lecturelive-core --lib notes::embeds`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/notes/mod.rs crates/core/src/notes/embeds.rs
git commit -m "Place every slide embed exactly once on its own line before a commit

Later and inline copies and embeds of slides outside the batch are removed, code fences
are left alone, and embeds the model left out are appended under 'Slides not placed'
without an invented description."
```

---

### Task 7: Lecture files, the commit journal and launch recovery (milestone task 6)

**Files:**
- Create: `crates/core/src/session/files.rs`, `crates/core/src/session/notesfile.rs`; `session/mod.rs` gains `pub mod files; pub mod notesfile;`

**Interfaces:**
- Consumes: `fsutil::write_atomic`, `notes::timeline::hms`, `session::sidecar::{sidecar_path, Sidecar}`, `session::segments::segments_path`.
- Produces:
  - `files::LectureFiles { dir: PathBuf, stem: String, date: NaiveDate, notes: PathBuf, transcript: PathBuf, slides: PathBuf }` (Clone) with `standard(dir: &Path, date: NaiveDate) -> Self`, `custom(dir, date, notes: Option<PathBuf>, transcript: Option<PathBuf>, slides: Option<PathBuf>) -> Self`, `state_dir()`, `sidecar()`, `segments()`, `journal()`, `legacy_state()`, `page_cache()`, `notes_dir() -> &Path`.
  - `notesfile::{Journal, Recovered::{Nothing, NotAppended, Completed, Truncated}, sha256_hex(&[u8]) -> String, accept_external_edit(&LectureFiles, &mut Sidecar) -> Result<bool>, block(DateTime<Local>, &str) -> String, commit(&LectureFiles, &mut Sidecar, block: &str, segments_to: u64, slides_to: u32) -> Result<()>, recover(&LectureFiles, &mut Sidecar) -> Result<Recovered>}`.
  - Test-only: `pub(crate) enum Step { Journal, Append, Cursors, Clear }`, `pub(crate) fn commit_through(…, last: Step)`.

The commit, one step per line (spec §6.2):
1. Accept any external edit as the new revision, then write the journal atomically: `op_id`, the cursors before and after, the notes' length and SHA-256 before, and the block's length and SHA-256.
2. Append the block `\n<!-- HH:MM:SS -->\n{notes}\n` to the notes and fsync.
3. Advance the revision, fingerprint and cursors in the sidecar, and save it atomically.
4. Delete the journal.

Recovery at launch, when a journal exists:
- The notes do not begin with the recorded "before" (length and hash): stop, change nothing, keep the journal.
- Nothing after "before": not appended; the material stays pending.
- Exactly the block after it (length and hash): completed. The cursors advance, unless step 3 already did.
- A shorter tail: a torn append; truncate to "before", and the material stays pending.
- A longer tail, or one of the block's length with the wrong hash: stop, change nothing, keep the journal.

In each case where it continues, recovery saves the sidecar and deletes the journal.

- [ ] **Step 1: Write the failing tests**

`notesfile.rs`, test module only:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use std::io::Write;

    fn folder() -> (tempfile::TempDir, LectureFiles, Sidecar) {
        let dir = tempfile::tempdir().unwrap();
        let files = LectureFiles::standard(dir.path(), NaiveDate::from_ymd_opt(2026, 9, 25).unwrap());
        std::fs::create_dir_all(files.state_dir()).unwrap();
        std::fs::write(&files.notes, "# Machine Learning — Week 01 — 2026-09-25\n").unwrap();
        let mut sc = Sidecar::default();
        assert!(accept_external_edit(&files, &mut sc).unwrap(), "a new file is revision 1");
        sc.save(&files.sidecar()).unwrap();
        (dir, files, sc)
    }

    const BLOCK: &str = "\n<!-- 10:05:00 -->\n## Gradient descent\n- Steps against the gradient.\n";

    #[derive(Debug, Clone, Copy)]
    enum Crash {
        TmpJournal,
        After(Step),
        MidAppend,
    }

    #[test]
    fn a_crash_at_every_commit_step_recovers_to_the_block_exactly_once() {
        for crash in [Crash::TmpJournal, Crash::After(Step::Journal), Crash::MidAppend, Crash::After(Step::Append), Crash::After(Step::Cursors), Crash::After(Step::Clear)] {
            let (_dir, files, sc0) = folder();
            let before = std::fs::read_to_string(&files.notes).unwrap();
            let mut sc = sc0.clone();
            match crash {
                Crash::TmpJournal => std::fs::write(files.journal().with_extension("tmp"), b"{\"op_id\":").unwrap(),
                Crash::After(step) => commit_through(&files, &mut sc, BLOCK, 5, 2, step).unwrap(),
                Crash::MidAppend => {
                    commit_through(&files, &mut sc, BLOCK, 5, 2, Step::Journal).unwrap();
                    std::fs::OpenOptions::new().append(true).open(&files.notes).unwrap().write_all(&BLOCK.as_bytes()[..20]).unwrap();
                }
            }
            // The process dies here: memory is gone, the files stay.
            let mut sc = Sidecar::load(&files.sidecar()).unwrap().unwrap();
            let outcome = recover(&files, &mut sc).unwrap();
            let expected = match crash {
                Crash::TmpJournal | Crash::After(Step::Clear) => Recovered::Nothing,
                Crash::After(Step::Journal) => Recovered::NotAppended,
                Crash::MidAppend => Recovered::Truncated,
                Crash::After(_) => Recovered::Completed,
            };
            assert_eq!(outcome, expected, "{crash:?}");
            let appended = matches!(crash, Crash::After(Step::Append | Step::Cursors | Step::Clear));
            let notes = std::fs::read_to_string(&files.notes).unwrap();
            assert_eq!(notes, if appended { format!("{before}{BLOCK}") } else { before.clone() }, "{crash:?}");
            assert_eq!((sc.notes.segment_cursor, sc.notes.slide_index), if appended { (5, 2) } else { (0, 0) }, "{crash:?}");
            assert_eq!((sc.notes.len, sc.notes.sha256.clone()), (notes.len() as u64, sha256_hex(notes.as_bytes())), "{crash:?}");
            assert!(!files.journal().exists(), "{crash:?}");
            assert_eq!(Sidecar::load(&files.sidecar()).unwrap().unwrap(), sc, "what recovery decided is saved ({crash:?})");
            if !appended {
                commit(&files, &mut sc, BLOCK, 5, 2).unwrap();
            }
            assert_eq!(std::fs::read_to_string(&files.notes).unwrap(), format!("{before}{BLOCK}"), "the batch lands exactly once ({crash:?})");
        }
    }

    #[test]
    fn the_journal_records_the_batch_and_both_fingerprints() {
        let (_dir, files, mut sc) = folder();
        let before = std::fs::read(&files.notes).unwrap();
        commit_through(&files, &mut sc, BLOCK, 5, 2, Step::Journal).unwrap();
        let j: Journal = serde_json::from_slice(&std::fs::read(files.journal()).unwrap()).unwrap();
        assert_eq!((j.segments, j.slides), ([0, 5], [0, 2]));
        assert_eq!((j.before_len, j.before_sha256), (before.len() as u64, sha256_hex(&before)));
        assert_eq!((j.block_len, j.block_sha256), (BLOCK.len() as u64, sha256_hex(BLOCK.as_bytes())));
    }

    #[test]
    fn notes_that_no_longer_begin_as_recorded_stop_recovery_and_nothing_is_truncated() {
        let (_dir, files, mut sc) = folder();
        commit_through(&files, &mut sc, BLOCK, 5, 2, Step::Journal).unwrap();
        std::fs::write(&files.notes, "# Edited by hand while the app was down\n- and more\n").unwrap();
        let err = recover(&files, &mut sc).unwrap_err();
        assert!(format!("{err:#}").contains("by hand"), "{err:#}");
        assert_eq!(std::fs::read_to_string(&files.notes).unwrap(), "# Edited by hand while the app was down\n- and more\n");
        assert!(files.journal().exists(), "the journal stays for the person to look at");
    }

    #[test]
    fn notes_that_grew_past_the_block_stop_recovery_and_nothing_is_truncated() {
        let (_dir, files, mut sc) = folder();
        commit_through(&files, &mut sc, BLOCK, 5, 2, Step::Append).unwrap();
        std::fs::OpenOptions::new().append(true).open(&files.notes).unwrap().write_all(b"- typed after the crash\n").unwrap();
        let grown = std::fs::read(&files.notes).unwrap();
        assert!(recover(&files, &mut sc).is_err());
        assert_eq!(std::fs::read(&files.notes).unwrap(), grown);
        assert!(files.journal().exists());
    }

    #[test]
    fn an_external_edit_is_kept_and_the_block_appended_after_it() {
        let (_dir, files, mut sc) = folder();
        std::fs::OpenOptions::new().append(true).open(&files.notes).unwrap().write_all(b"\nMy own note, typed in an editor.\n").unwrap();
        let edited = std::fs::read_to_string(&files.notes).unwrap();
        commit(&files, &mut sc, BLOCK, 5, 2).unwrap();
        assert_eq!(std::fs::read_to_string(&files.notes).unwrap(), format!("{edited}{BLOCK}"));
        assert_eq!(sc.notes.revision, 3, "the edit and the commit are one revision each");
    }

    #[test]
    fn a_block_is_the_clis_marker_and_text() {
        let at = chrono::Local.with_ymd_and_hms(2026, 9, 25, 10, 5, 0).unwrap();
        assert_eq!(block(at, "## A\n- b"), "\n<!-- 10:05:00 -->\n## A\n- b\n");
    }
}
```

(`use chrono::TimeZone;` in the test module for `with_ymd_and_hms`.)

`files.rs` tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_standard_files_take_the_clis_names() {
        let f = LectureFiles::standard(Path::new("/l"), NaiveDate::from_ymd_opt(2026, 9, 25).unwrap());
        assert_eq!(f.stem, "lecture_notes_20260925");
        assert_eq!((f.notes.as_path(), f.transcript.as_path(), f.slides.as_path()), (Path::new("/l/lecture_notes_20260925.md"), Path::new("/l/lecture_transcript_20260925.txt"), Path::new("/l/slides")));
        assert_eq!(f.sidecar(), Path::new("/l/.live_notes/lecture_notes_20260925.v2.json"));
        assert_eq!(f.journal(), Path::new("/l/.live_notes/lecture_notes_20260925.journal.json"));
        assert_eq!(f.legacy_state(), Path::new("/l/.live_notes/lecture_notes_20260925.json"));
        assert_eq!(f.page_cache(), Path::new("/l/.live_notes/lecture_notes_20260925.page.json"));
        assert_eq!(f.segments(), Path::new("/l/.live_notes/lecture_notes_20260925.segments.jsonl"));
    }

    #[test]
    fn custom_notes_name_the_state_files_as_the_cli_does() {
        let f = LectureFiles::custom(Path::new("/l"), NaiveDate::from_ymd_opt(2026, 9, 25).unwrap(), Some("/l/week1.md".into()), Some("/l/t.txt".into()), Some("/shots".into()));
        assert_eq!(f.stem, "week1");
        assert_eq!(f.sidecar(), Path::new("/l/.live_notes/week1.v2.json"));
        assert_eq!((f.transcript.as_path(), f.slides.as_path()), (Path::new("/l/t.txt"), Path::new("/shots")));
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p lecturelive-core --lib session::files session::notesfile`
Expected: FAIL to compile.

- [ ] **Step 3: Implement**

`crates/core/src/session/files.rs`:

```rust
//! Every path of a lecture (spec §8), named as the Python CLI names them.
use std::path::{Path, PathBuf};

use chrono::NaiveDate;

use crate::session::segments::segments_path;
use crate::session::sidecar::sidecar_path;

#[derive(Debug, Clone, PartialEq)]
pub struct LectureFiles {
    /// The lecture folder: `.live_notes/` and `recordings/` live here.
    pub dir: PathBuf,
    /// The notes file's stem: every state file is named after it.
    pub stem: String,
    /// The lecture day, fixed when its files were created.
    pub date: NaiveDate,
    pub notes: PathBuf,
    pub transcript: PathBuf,
    pub slides: PathBuf,
}

impl LectureFiles {
    pub fn standard(dir: &Path, date: NaiveDate) -> Self {
        Self::custom(dir, date, None, None, None)
    }

    /// The CLI's `--notes`, `--transcript` and `--slides-dir`, each defaulting to the standard name.
    pub fn custom(dir: &Path, date: NaiveDate, notes: Option<PathBuf>, transcript: Option<PathBuf>, slides: Option<PathBuf>) -> Self {
        let stamp = date.format("%Y%m%d");
        let notes = notes.unwrap_or_else(|| dir.join(format!("lecture_notes_{stamp}.md")));
        let stem = notes.file_stem().map_or_else(|| format!("lecture_notes_{stamp}"), |s| s.to_string_lossy().into_owned());
        Self {
            dir: dir.to_path_buf(),
            stem,
            date,
            notes,
            transcript: transcript.unwrap_or_else(|| dir.join(format!("lecture_transcript_{stamp}.txt"))),
            slides: slides.unwrap_or_else(|| dir.join("slides")),
        }
    }

    pub fn state_dir(&self) -> PathBuf {
        self.dir.join(".live_notes")
    }

    pub fn sidecar(&self) -> PathBuf {
        sidecar_path(&self.dir, &self.stem)
    }

    pub fn segments(&self) -> PathBuf {
        segments_path(&self.dir, &self.stem)
    }

    pub fn journal(&self) -> PathBuf {
        self.state_dir().join(format!("{}.journal.json", self.stem))
    }

    /// The Python CLI's state file.
    pub fn legacy_state(&self) -> PathBuf {
        self.state_dir().join(format!("{}.json", self.stem))
    }

    pub fn page_cache(&self) -> PathBuf {
        self.state_dir().join(format!("{}.page.json", self.stem))
    }

    pub fn notes_dir(&self) -> &Path {
        self.notes.parent().unwrap_or(&self.dir)
    }
}
```

`crates/core/src/session/notesfile.rs` above the tests:

```rust
//! Every change to the notes document (spec §6.2, §6.3, §8): the revision check, the journaled
//! snapshot commit, its recovery at launch, and polish's replace.
use std::fs::OpenOptions;
use std::io::Write;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::fsutil::write_atomic;
use crate::notes::timeline::hms;
use crate::session::files::LectureFiles;
use crate::session::sidecar::Sidecar;

/// A commit in progress (spec §6.2): present only between the journal step and the clear step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Journal {
    pub op_id: Uuid,
    /// The segment cursor before and after the block.
    pub segments: [u64; 2],
    /// The slide index before and after the block.
    pub slides: [u32; 2],
    pub before_len: u64,
    pub before_sha256: String,
    pub block_len: u64,
    pub block_sha256: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Recovered {
    #[default]
    Nothing,
    /// The block never reached the notes: its material is pending.
    NotAppended,
    /// The block was fully written: the cursors now say so.
    Completed,
    /// Part of the block was written and has been removed: its material is pending.
    Truncated,
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

fn fingerprint(files: &LectureFiles) -> Result<(u64, String)> {
    let bytes = std::fs::read(&files.notes).with_context(|| format!("read {}", files.notes.display()))?;
    Ok((bytes.len() as u64, sha256_hex(&bytes)))
}

/// Spec §8: the notes as they are on disk are the current revision. An edit made outside the app
/// becomes a new revision; it is never overwritten from memory. True when there was one.
pub fn accept_external_edit(files: &LectureFiles, sc: &mut Sidecar) -> Result<bool> {
    let (len, sha) = fingerprint(files)?;
    if sc.notes.len == len && sc.notes.sha256 == sha {
        return Ok(false);
    }
    sc.notes.revision += 1;
    sc.notes.len = len;
    sc.notes.sha256 = sha;
    Ok(true)
}

/// The block a snapshot appends: the CLI's `<!-- HH:MM:SS -->` marker, then the notes.
pub fn block(at: DateTime<Local>, notes: &str) -> String {
    format!("\n<!-- {} -->\n{notes}\n", hms(at))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Step {
    Journal,
    Append,
    Cursors,
    Clear,
}

/// Appends a snapshot's block and moves the cursors to `segments_to` and `slides_to` (spec §6.2).
pub fn commit(files: &LectureFiles, sc: &mut Sidecar, block: &str, segments_to: u64, slides_to: u32) -> Result<()> {
    commit_through(files, sc, block, segments_to, slides_to, Step::Clear)
}

/// The commit, stopping after `last`: the fault-injection tests crash between every pair of steps.
pub(crate) fn commit_through(files: &LectureFiles, sc: &mut Sidecar, block: &str, segments_to: u64, slides_to: u32, last: Step) -> Result<()> {
    accept_external_edit(files, sc)?;
    let j = Journal {
        op_id: Uuid::new_v4(),
        segments: [sc.notes.segment_cursor, segments_to],
        slides: [sc.notes.slide_index, slides_to],
        before_len: sc.notes.len,
        before_sha256: sc.notes.sha256.clone(),
        block_len: block.len() as u64,
        block_sha256: sha256_hex(block.as_bytes()),
    };
    write_atomic(&files.journal(), &serde_json::to_vec_pretty(&j)?).context("write the commit journal")?;
    if last == Step::Journal {
        return Ok(());
    }
    let mut notes = OpenOptions::new().append(true).open(&files.notes).with_context(|| format!("open {}", files.notes.display()))?;
    notes.write_all(block.as_bytes()).and_then(|()| notes.sync_all()).with_context(|| format!("append to {}", files.notes.display()))?;
    if last == Step::Append {
        return Ok(());
    }
    advance(files, sc, &j)?;
    if last == Step::Cursors {
        return Ok(());
    }
    clear(files)
}

fn advance(files: &LectureFiles, sc: &mut Sidecar, j: &Journal) -> Result<()> {
    let (len, sha) = fingerprint(files)?;
    let n = &mut sc.notes;
    n.revision += 1;
    n.len = len;
    n.sha256 = sha;
    n.segment_cursor = j.segments[1];
    n.slide_index = j.slides[1];
    sc.save(&files.sidecar())
}

fn clear(files: &LectureFiles) -> Result<()> {
    match std::fs::remove_file(files.journal()) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).context("clear the commit journal"),
    }
}

/// Launch recovery (spec §6.2): finishes or undoes an interrupted commit. Any state it cannot
/// verify stops with an error and changes nothing; nothing is truncated without a verified prefix.
pub fn recover(files: &LectureFiles, sc: &mut Sidecar) -> Result<Recovered> {
    let path = files.journal();
    let j: Journal = match std::fs::read(&path) {
        Ok(b) => serde_json::from_slice(&b).with_context(|| format!("the commit journal {} is unreadable: check {} by hand, then delete the journal", path.display(), files.notes.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Recovered::Nothing),
        Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
    };
    let data = std::fs::read(&files.notes).with_context(|| format!("read {}", files.notes.display()))?;
    let before = j.before_len as usize;
    let stop = |what: &str| -> anyhow::Error {
        anyhow::anyhow!("{what} since the interrupted snapshot in {}, so nothing was changed. Check {} by hand, then delete the journal to go on", path.display(), files.notes.display())
    };
    if data.len() < before || sha256_hex(&data[..before]) != j.before_sha256 {
        return Err(stop("the notes no longer begin as they did"));
    }
    let tail = &data[before..];
    let outcome = if tail.is_empty() {
        Recovered::NotAppended
    } else if tail.len() as u64 == j.block_len && sha256_hex(tail) == j.block_sha256 {
        Recovered::Completed
    } else if (tail.len() as u64) < j.block_len {
        let f = OpenOptions::new().write(true).open(&files.notes)?;
        f.set_len(j.before_len)?;
        f.sync_all()?;
        Recovered::Truncated
    } else {
        return Err(stop("the notes grew past the snapshot's block"));
    };
    if outcome == Recovered::Completed {
        let applied = sc.notes.segment_cursor == j.segments[1] && sc.notes.slide_index == j.slides[1] && sc.notes.len == data.len() as u64;
        if !applied {
            advance(files, sc, &j)?;
        }
    } else {
        sc.notes.len = j.before_len;
        sc.notes.sha256 = j.before_sha256.clone();
        sc.save(&files.sidecar())?;
    }
    clear(files)?;
    Ok(outcome)
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p lecturelive-core --lib session::files session::notesfile`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/session/mod.rs crates/core/src/session/files.rs crates/core/src/session/notesfile.rs
git commit -m "Commit snapshot blocks through a journal that recovers from a crash at any step

The journal records the notes' length and hash before the block and the block's own;
launch recovery finishes, undoes or truncates a torn append only with a verified prefix,
and stops without touching anything otherwise. An edit made outside the app is kept as a
new revision."
```

---

### Task 8: Folder initialisation, legacy migration, and the Python CLI's exit (milestone task 8)

**Files:**
- Modify: `crates/core/src/session/segments.rs` (`open_at` with a transcript path and its repair, `session_marker`, `write_all`)
- Modify: `crates/core/src/audio/source.rs` (`NoAudio`), `crates/core/src/session/coordinator.rs` (`SessionConfig::transcript`)
- Create: `crates/core/src/session/folder.rs`; `session/mod.rs` gains `pub mod folder;`
- Modify: `live_notes.py` (one check, with the Edit tool)

**Interfaces:**
- Consumes: `LectureFiles`, `notesfile::{recover, accept_external_edit, Recovered}`, `coordinator::{spawn, SessionConfig, StopReport}`, `stt::rest::RecoveryLink`, `spend::Spend`.
- Produces:
  - `segments::{SegmentLog::open_at(dir: &Path, stem: &str, transcript: &Path) -> Result<SegmentLog>, session_marker(transcript: &Path, at: DateTime<Local>) -> Result<()>, write_all(path: &Path, segments: &[Segment]) -> Result<()>}`. `SegmentLog::open(dir, stem)` keeps its signature and calls `open_at` with the standard transcript path.
  - `SessionConfig::transcript: Option<PathBuf>` (None: the standard name).
  - `audio::source::NoAudio`: a `Source` that ends at once.
  - `folder::{InitReport, How::{Resumed, Created, Migrated, Rebuilt}, open(&LectureFiles, title: &str, rebuild: bool) -> Result<(Sidecar, InitReport)>, slide_files(&LectureFiles) -> Result<Vec<SlideEntry>>, next_slide_index(&LectureFiles, &Sidecar) -> Result<u32>, relative(&LectureFiles, &Path) -> String, recover_other_days(dir: &Path, today: &str, recovery: impl Fn() -> Result<RecoveryLink>, spend: Option<Spend>) -> Result<Vec<(String, StopReport)>>}`.
  - `InitReport { how: How, journal: Recovered, external_edit: bool, slides_registered: usize, pending_segments: u64, pending_slides: usize, legacy_commit: Option<&'static str>, corrupt_kept: Option<PathBuf> }`.

Initialisation (spec §8's table, settled against `live_notes.py`'s `initial_state` and `recover_commit`):

| Folder state | Action |
|---|---|
| v2 sidecar present | Resume. Journal recovery first, then any external edit is accepted, then slide files dropped in while no session ran are registered |
| v2 sidecar corrupt | Stop, naming `--rebuild`. With it, the corrupt file is kept beside it as `<name>.corrupt-HHMMSS`, then rebuilt |
| Legacy `<stem>.json` only | The CLI's half-written snapshot (a `commit` entry) is finished or undone as `recover_commit` does. Then transcript lines are imported as segments (`imported`, second resolution, rolled to the next day past midnight) and slide files as slides. Lines before `transcript_offset` (or, for the older `noted_through`, before the first line at or after it) and slides up to `slide_index` count as noted. The legacy file stays; v2 is written last |
| Notes but no state | Rebuilt: transcript lines and slides after the last `<!-- HH:MM:SS -->` marker are pending. An existing segment log is kept and its cursor set the same way |
| Transcript or slides but no notes | Notes created with the title; everything existing is pending |
| Empty | Notes created with the title; empty sidecar |

The Python CLI's check (spec §8 "Legacy writer"): right after `if args.command == "spend": return show_spend()` in `main()`,

```python
    if any((Path.cwd() / ".live_notes").glob("*.v2.json")):
        sys.exit("This folder is kept by the LectureLive app now (.live_notes/*.v2.json). Run `lecturelive lecture` here instead.")
```

`spend` still works everywhere: it touches no folder.

- [ ] **Step 1: Write the failing tests**

`segments.rs` tests (appended):

```rust
    #[test]
    fn a_transcript_line_lost_between_the_two_syncs_is_put_back() {
        let dir = tempfile::tempdir().unwrap();
        let rec = Uuid::new_v4();
        let mut log = SegmentLog::open(dir.path(), STEM).unwrap();
        log.append(seg(rec, 0, 16_000, "one", vec![], SegmentSource::Live), anchor()).unwrap();
        log.append(seg(rec, 16_000, 32_000, "two", vec![], SegmentSource::Live), anchor()).unwrap();
        drop(log);
        let t = transcript_path(dir.path(), STEM);
        std::fs::write(&t, "[10:00:00] one\n").unwrap(); // the crash came after the log's sync
        SegmentLog::open(dir.path(), STEM).unwrap();
        assert_eq!(std::fs::read_to_string(&t).unwrap(), "[10:00:00] one\n[10:00:01] two\n");
        std::fs::write(&t, "[10:00:00] one\n[10:00:0").unwrap(); // or mid-line
        SegmentLog::open(dir.path(), STEM).unwrap();
        assert_eq!(std::fs::read_to_string(&t).unwrap(), "[10:00:00] one\n[10:00:01] two\n");
        session_marker(&t, anchor() + chrono::Duration::minutes(5)).unwrap();
        SegmentLog::open(dir.path(), STEM).unwrap();
        assert_eq!(std::fs::read_to_string(&t).unwrap(), "[10:00:00] one\n[10:00:01] two\n--- resumed 10:05:00 ---\n", "a marker after the last line hides nothing");
    }

    #[test]
    fn session_markers_are_the_clis() {
        let dir = tempfile::tempdir().unwrap();
        let t = dir.path().join("lecture_transcript_20260925.txt");
        session_marker(&t, anchor()).unwrap();
        session_marker(&t, anchor() + chrono::Duration::seconds(90)).unwrap();
        assert_eq!(std::fs::read_to_string(&t).unwrap(), "--- started 10:00:00 ---\n--- resumed 10:01:30 ---\n");
    }

    #[test]
    fn a_custom_transcript_path_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let t = dir.path().join("mine.txt");
        let mut log = SegmentLog::open_at(dir.path(), STEM, &t).unwrap();
        log.append(seg(Uuid::new_v4(), 0, 16_000, "one", vec![], SegmentSource::Live), anchor()).unwrap();
        assert_eq!(std::fs::read_to_string(&t).unwrap(), "[10:00:00] one\n");
        assert!(!transcript_path(dir.path(), STEM).exists());
    }
```

`folder.rs`, test module only (a legacy folder exactly as `live_notes.py` writes one):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::segments::{self, SegmentSource};
    use chrono::{NaiveDate, TimeZone};

    const TITLE: &str = "# Machine Learning — Week 01 — 2026-09-25";

    fn files(dir: &Path) -> LectureFiles {
        LectureFiles::standard(dir, NaiveDate::from_ymd_opt(2026, 9, 25).unwrap())
    }

    fn png(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        image::RgbImage::from_pixel(8, 8, image::Rgb([255, 255, 255])).save(path).unwrap();
    }

    /// Notes with one snapshot, a transcript across two runs, two slides: the Python CLI's formats.
    fn legacy(dir: &Path, state: serde_json::Value) -> LectureFiles {
        let f = files(dir);
        std::fs::create_dir_all(f.state_dir()).unwrap();
        std::fs::write(&f.notes, format!("{TITLE}\n\n<!-- 10:00:12 -->\n## Intro\n- one, two\n")).unwrap();
        std::fs::write(&f.transcript, "--- started 10:00:00 ---\n[10:00:05] one\n[10:00:09] two\n--- resumed 10:20:00 ---\n[10:20:03] three\n").unwrap();
        png(&f.slides.join("slide_01_100007.png"));
        png(&f.slides.join("slide_02_102004.png"));
        std::fs::write(f.legacy_state(), state.to_string()).unwrap();
        f
    }

    /// Byte offset of the "--- resumed" line: where the CLI's transcript stood at its snapshot.
    const OFFSET: u64 = ("--- started 10:00:00 ---\n[10:00:05] one\n[10:00:09] two\n".len()) as u64;

    #[test]
    fn an_empty_folder_gets_notes_with_the_title_and_an_empty_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let f = files(dir.path());
        let (sc, report) = open(&f, TITLE, false).unwrap();
        assert_eq!(report.how, How::Created);
        assert_eq!(std::fs::read_to_string(&f.notes).unwrap(), format!("{TITLE}\n"));
        assert_eq!((sc.notes.segment_cursor, sc.notes.slide_index, sc.notes.revision), (0, 0, 1));
        assert_eq!(sc.lecture_date, Some(f.date));
        assert_eq!(Sidecar::load(&f.sidecar()).unwrap().unwrap(), sc);
    }

    #[test]
    fn a_legacy_folder_migrates_with_the_lines_after_its_offset_pending() {
        let dir = tempfile::tempdir().unwrap();
        let f = legacy(dir.path(), serde_json::json!({"transcript_offset": OFFSET, "slide_index": 1}));
        let transcript = std::fs::read(&f.transcript).unwrap();
        let (sc, report) = open(&f, TITLE, false).unwrap();
        assert_eq!(report.how, How::Migrated);
        let segs = segments::read(&f.segments()).unwrap();
        assert_eq!(segs.iter().map(|s| (s.id, s.said_at.format("%H:%M:%S").to_string(), s.text.as_str(), s.source)).collect::<Vec<_>>(), vec![
            (0, "10:00:05".to_string(), "one", SegmentSource::Imported),
            (1, "10:00:09".to_string(), "two", SegmentSource::Imported),
            (2, "10:20:03".to_string(), "three", SegmentSource::Imported),
        ]);
        assert_eq!((sc.notes.segment_cursor, sc.notes.slide_index), (2, 1));
        assert_eq!(sc.slides.iter().map(|s| (s.index, s.file.as_str())).collect::<Vec<_>>(), vec![(1, "slides/slide_01_100007.png"), (2, "slides/slide_02_102004.png")]);
        assert_eq!((report.pending_segments, report.pending_slides), (1, 1));
        assert_eq!(std::fs::read(&f.transcript).unwrap(), transcript, "the transcript is not rewritten");
        assert!(f.legacy_state().exists(), "the CLI's state stays as provenance");
        let (_, again) = open(&f, TITLE, false).unwrap();
        assert_eq!(again.how, How::Resumed, "migration is one-time");
    }

    #[test]
    fn an_older_noted_through_checkpoint_migrates_by_time() {
        let dir = tempfile::tempdir().unwrap();
        let f = legacy(dir.path(), serde_json::json!({"noted_through": "10:00:09"}));
        let (sc, _) = open(&f, TITLE, false).unwrap();
        assert_eq!((sc.notes.segment_cursor, sc.notes.slide_index), (1, 1), "lines from 10:00:09 on and slides from then on are pending");
    }

    #[test]
    fn a_legacy_half_written_snapshot_is_undone_before_migration() {
        let block = "\n<!-- 10:21:00 -->\n## Three\n- three\n";
        let commit = |before: usize| serde_json::json!({"transcript_offset": OFFSET, "slide_index": 1, "commit": {"before": before, "block": block, "transcript_offset": 200, "slide_index": 2}});
        // Torn: part of the block reached the notes.
        let dir = tempfile::tempdir().unwrap();
        let f = legacy(dir.path(), serde_json::json!({}));
        let before = std::fs::read_to_string(&f.notes).unwrap();
        std::fs::write(f.legacy_state(), commit(before.len()).to_string()).unwrap();
        std::fs::write(&f.notes, format!("{before}{}", &block[..12])).unwrap();
        let (sc, report) = open(&f, TITLE, false).unwrap();
        assert_eq!(report.legacy_commit, Some("removed a half-written snapshot; its material is queued again"));
        assert_eq!(std::fs::read_to_string(&f.notes).unwrap(), before);
        assert_eq!((sc.notes.segment_cursor, sc.notes.slide_index), (2, 1));
        // Complete: the whole block is there, so its cursors hold.
        let dir = tempfile::tempdir().unwrap();
        let f = legacy(dir.path(), serde_json::json!({}));
        let before = std::fs::read_to_string(&f.notes).unwrap();
        std::fs::write(f.legacy_state(), commit(before.len()).to_string()).unwrap();
        std::fs::write(&f.notes, format!("{before}{block}")).unwrap();
        let (sc, report) = open(&f, TITLE, false).unwrap();
        assert_eq!(report.legacy_commit, Some("the last snapshot was fully written"));
        assert_eq!((sc.notes.segment_cursor, sc.notes.slide_index), (3, 2), "offset 200 is past every line");
    }

    #[test]
    fn imported_lines_after_midnight_roll_to_the_next_day() {
        let dir = tempfile::tempdir().unwrap();
        let f = files(dir.path());
        std::fs::create_dir_all(f.state_dir()).unwrap();
        std::fs::write(&f.transcript, "[23:59:50] late\n[00:00:10] later\n").unwrap();
        std::fs::write(f.legacy_state(), serde_json::json!({"transcript_offset": 0, "slide_index": 0}).to_string()).unwrap();
        std::fs::write(&f.notes, format!("{TITLE}\n")).unwrap();
        open(&f, TITLE, false).unwrap();
        let segs = segments::read(&f.segments()).unwrap();
        assert_eq!(segs[0].said_at, Local.with_ymd_and_hms(2026, 9, 25, 23, 59, 50).unwrap());
        assert_eq!(segs[1].said_at, Local.with_ymd_and_hms(2026, 9, 26, 0, 0, 10).unwrap());
    }

    #[test]
    fn notes_without_any_state_are_rebuilt_from_the_last_marker() {
        let dir = tempfile::tempdir().unwrap();
        let f = files(dir.path());
        std::fs::write(&f.notes, format!("{TITLE}\n\n<!-- 10:05:00 -->\n- a\n\n<!-- 10:10:00 -->\n- b\n")).unwrap();
        std::fs::write(&f.transcript, "[10:04:00] a\n[10:09:59] b\n[10:10:30] c\n").unwrap();
        png(&f.slides.join("slide_01_100900.png"));
        png(&f.slides.join("slide_02_101100.png"));
        let (sc, report) = open(&f, TITLE, false).unwrap();
        assert_eq!(report.how, How::Rebuilt);
        assert_eq!((sc.notes.segment_cursor, sc.notes.slide_index), (2, 1));
    }

    #[test]
    fn a_corrupt_sidecar_stops_unless_a_rebuild_is_asked_and_is_then_kept() {
        let dir = tempfile::tempdir().unwrap();
        let f = files(dir.path());
        std::fs::create_dir_all(f.state_dir()).unwrap();
        std::fs::write(&f.notes, format!("{TITLE}\n\n<!-- 10:00:03 -->\n- one\n")).unwrap();
        let mut log = crate::session::segments::SegmentLog::open(dir.path(), &f.stem).unwrap();
        for (k, text) in ["one", "two"].iter().enumerate() {
            let s = crate::session::segments::NewSegment { recording_id: uuid::Uuid::nil(), start_sample: k as u64 * 64_000, end_sample: k as u64 * 64_000 + 8_000, text: text.to_string(), words: vec![], source: SegmentSource::Live };
            log.append(s, Local.with_ymd_and_hms(2026, 9, 25, 10, 0, 0).unwrap()).unwrap();
        }
        drop(log);
        std::fs::write(f.sidecar(), b"{\"version\":2,\"recor").unwrap();
        let err = open(&f, TITLE, false).unwrap_err();
        assert!(format!("{err:#}").contains("--rebuild"), "{err:#}");
        assert_eq!(std::fs::read(f.sidecar()).unwrap(), b"{\"version\":2,\"recor");
        let (sc, report) = open(&f, TITLE, true).unwrap();
        assert_eq!(report.how, How::Rebuilt);
        let kept = report.corrupt_kept.unwrap();
        assert_eq!(std::fs::read(&kept).unwrap(), b"{\"version\":2,\"recor");
        assert_eq!(segments::read(&f.segments()).unwrap().len(), 2, "the segment log is kept");
        assert_eq!(sc.notes.segment_cursor, 1, "said at 10:00:00, before the marker; the next at 10:00:04, after it");
    }

    #[test]
    fn a_transcript_or_slides_without_notes_are_all_pending() {
        let dir = tempfile::tempdir().unwrap();
        let f = files(dir.path());
        std::fs::write(&f.transcript, "[10:00:05] one\n").unwrap();
        png(&f.slides.join("slide_03_100007.png"));
        let (sc, report) = open(&f, TITLE, false).unwrap();
        assert_eq!(report.how, How::Created);
        assert_eq!((sc.notes.segment_cursor, sc.notes.slide_index, report.pending_segments, report.pending_slides), (0, 0, 1, 1));
    }

    #[test]
    fn resume_recovers_the_journal_first_then_keeps_an_edit_and_registers_new_slide_files() {
        let dir = tempfile::tempdir().unwrap();
        let f = files(dir.path());
        let (mut sc, _) = open(&f, TITLE, false).unwrap();
        crate::session::notesfile::commit_through(&f, &mut sc, "\n<!-- 10:05:00 -->\n- a\n", 0, 0, crate::session::notesfile::Step::Journal).unwrap();
        std::fs::OpenOptions::new().append(true).open(&f.notes).unwrap().write_all(b"\n<!-- 10:0").unwrap();
        let (_, report) = open(&f, TITLE, false).unwrap();
        assert_eq!((report.how, report.journal, report.external_edit), (How::Resumed, Recovered::Truncated, false));
        std::fs::OpenOptions::new().append(true).open(&f.notes).unwrap().write_all(b"- typed by hand\n").unwrap();
        png(&f.slides.join("slide_01_101500.png"));
        let (sc, report) = open(&f, TITLE, false).unwrap();
        assert!(report.external_edit);
        assert_eq!((report.slides_registered, sc.slides.len(), next_slide_index(&f, &sc).unwrap()), (1, 1, 2));
    }

    #[tokio::test]
    async fn another_days_transcript_gaps_are_recovered_before_today() {
        use crate::session::sidecar::{Gap, GapKind, RecState, RecordingEntry};
        use crate::stt::rest::{RecoverEvent, RecoveryLink};
        let dir = tempfile::tempdir().unwrap();
        let wav = dir.path().join("recordings/r.wav");
        std::fs::create_dir_all(wav.parent().unwrap()).unwrap();
        let spec = hound::WavSpec { channels: 1, sample_rate: 16_000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        let mut w = hound::WavWriter::create(&wav, spec).unwrap();
        for _ in 0..48_000 {
            w.write_sample(100i16).unwrap();
        }
        w.finalize().unwrap();
        let id = uuid::Uuid::new_v4();
        let anchor = Local.with_ymd_and_hms(2026, 9, 24, 10, 0, 0).unwrap();
        let mut yesterday = Sidecar::default();
        yesterday.recordings.push(RecordingEntry { id, file: "recordings/r.wav".into(), anchor, source_uid: "X".into(), input_rate: 48_000, samples: Some(48_000), state: RecState::Finalized });
        yesterday.gaps.push(Gap::new(id, 16_000, Some(48_000), GapKind::SttOffline));
        yesterday.save(&crate::session::sidecar::sidecar_path(dir.path(), "lecture_notes_20260924")).unwrap();
        let recovery = || -> Result<RecoveryLink> {
            let (jobs, mut rx) = tokio::sync::mpsc::unbounded_channel::<crate::stt::rest::RecoverJob>();
            let (tx, events) = tokio::sync::mpsc::channel(8);
            tokio::spawn(async move {
                while let Some(j) = rx.recv().await {
                    let _ = tx.send(RecoverEvent::Piece { recording_id: j.recording_id, gap_start: j.gap_start, start_sample: j.from, end_sample: j.end, text: "recovered a day later".into(), words: vec![] }).await;
                    let _ = tx.send(RecoverEvent::Done { recording_id: j.recording_id, gap_start: j.gap_start }).await;
                }
            });
            Ok(RecoveryLink { jobs, events })
        };
        let done = recover_other_days(dir.path(), "lecture_notes_20260925", recovery, None).await.unwrap();
        assert_eq!(done.iter().map(|(s, r)| (s.as_str(), r.unresolved)).collect::<Vec<_>>(), vec![("lecture_notes_20260924", 0)]);
        assert_eq!(std::fs::read_to_string(dir.path().join("lecture_transcript_20260924.txt")).unwrap(), "[10:00:01] recovered a day later\n");
        assert!(recover_other_days(dir.path(), "lecture_notes_20260925", recovery, None).await.unwrap().is_empty(), "nothing is left to recover");
    }
}
```

(`use std::io::Write;` in the test module.)

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p lecturelive-core --lib session::segments session::folder`
Expected: FAIL to compile.

- [ ] **Step 3: Implement**

`segments.rs`: `SegmentLog::open` becomes `pub fn open(dir: &Path, stem: &str) -> Result<Self> { Self::open_at(dir, stem, &transcript_path(dir, stem)) }`. `open_at` is the old body with `tpath` given, plus `repair_transcript(transcript, last)?` for the last read segment before opening the transcript for append. Add:

```rust
/// A crash between the segment log's sync and the transcript's (spec §8) leaves the last segment's
/// line missing or cut short: it is put back. Session marker lines after it do not count.
fn repair_transcript(path: &Path, last: &Segment) -> Result<()> {
    let line = transcript_line(last);
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
    };
    let keep = bytes.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
    let torn = &bytes[keep..];
    let complete = String::from_utf8_lossy(&bytes[..keep]).into_owned();
    let mut last_text = complete.lines().rev().find(|l| !l.starts_with("--- ")).map(str::to_string);
    let mut f = OpenOptions::new().create(true).append(true).open(path).with_context(|| format!("open {}", path.display()))?;
    if !torn.is_empty() {
        if line.as_bytes().starts_with(torn) {
            f.set_len(keep as u64)?;
        } else {
            f.write_all(b"\n")?; // someone else's cut-off text: kept, as a line of its own
            last_text = Some(String::from_utf8_lossy(torn).into_owned());
        }
    }
    if last_text.as_deref() != Some(line.trim_end_matches('\n')) {
        f.write_all(line.as_bytes())?;
    }
    f.sync_data()?;
    Ok(())
}

/// The CLI's session line: `--- started HH:MM:SS ---` in an empty transcript, `--- resumed …` otherwise.
pub fn session_marker(transcript: &Path, at: DateTime<Local>) -> Result<()> {
    let resumed = std::fs::metadata(transcript).is_ok_and(|m| m.len() > 0);
    let mut f = OpenOptions::new().create(true).append(true).open(transcript).with_context(|| format!("open {}", transcript.display()))?;
    f.write_all(format!("--- {} {} ---\n", if resumed { "resumed" } else { "started" }, at.format("%H:%M:%S")).as_bytes())?;
    f.sync_data()?;
    Ok(())
}

/// Writes a whole segment log at once (migration), replacing any earlier attempt.
pub fn write_all(path: &Path, segments: &[Segment]) -> Result<()> {
    let mut out = Vec::new();
    for s in segments {
        out.extend(serde_json::to_vec(s)?);
        out.push(b'\n');
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    crate::fsutil::write_atomic(path, &out)
}
```

`open_at` calls `repair_transcript` only when the log holds a segment. When the transcript file is missing and the log is not empty, it gets the last line; earlier lines of a transcript deleted by hand are not rebuilt.

`coordinator.rs`: `SessionConfig` gains `pub transcript: Option<PathBuf>`, and `run` opens the log with `SegmentLog::open_at(&dir, &stem, &cfg.transcript.clone().unwrap_or_else(|| transcript_path(&dir, &stem)))`.

`audio/source.rs`:

```rust
/// A source with no audio: a session that only finishes what an earlier one left (recovery).
pub struct NoAudio;

impl Source for NoAudio {
    fn run(self: Box<Self>, _out: Sender<SourceEvent>, _stop: Arc<AtomicBool>) {}
}
```

`crates/core/src/session/folder.rs` above the tests:

```rust
//! A lecture folder's initialisation (spec §8): resume, migrate from the Python CLI, rebuild, or
//! start. Runs under the folder lock before a session; the Python CLI's formats are read as it
//! writes them.
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use anyhow::{Context, Result};
use chrono::{DateTime, Local, NaiveDate, NaiveTime, TimeZone};
use regex::Regex;
use serde_json::Value;
use uuid::Uuid;

use crate::audio::source::NoAudio;
use crate::session::coordinator::{self, SessionConfig, StopReport};
use crate::session::files::LectureFiles;
use crate::session::notesfile::{self, Recovered};
use crate::session::segments::{self, Segment, SegmentSource};
use crate::session::sidecar::{Sidecar, SlideEntry};
use crate::session::spend::Spend;
use crate::stt::rest::RecoveryLink;

/// The CLI's slide names: `slide_NN_HHMMSS.png|jpg|jpeg` (live_notes.py `SLIDE_RE`).
static SLIDE_NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^slide_(\d+)_(\d{6})\.(png|jpe?g)$").unwrap());
/// A transcript line (`LINE_RE`).
static LINE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\[(\d\d:\d\d:\d\d)\] (.+)$").unwrap());
/// A snapshot marker in the notes.
static MARKER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^<!-- (\d\d:\d\d:\d\d) -->$").unwrap());

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum How {
    #[default]
    Resumed,
    Created,
    Migrated,
    Rebuilt,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct InitReport {
    pub how: How,
    pub journal: Recovered,
    pub external_edit: bool,
    pub slides_registered: usize,
    pub pending_segments: u64,
    pub pending_slides: usize,
    /// What finishing the Python CLI's interrupted snapshot did, in its own words.
    pub legacy_commit: Option<&'static str>,
    pub corrupt_kept: Option<PathBuf>,
}

/// Spec §8's initialisation table; the caller holds the folder lock.
pub fn open(files: &LectureFiles, title: &str, rebuild: bool) -> Result<(Sidecar, InitReport)> {
    std::fs::create_dir_all(files.state_dir())?;
    std::fs::create_dir_all(&files.slides).with_context(|| format!("create {}", files.slides.display()))?;
    let mut report = InitReport::default();
    let mut sc = match Sidecar::load(&files.sidecar()) {
        Ok(Some(sc)) => sc,
        Ok(None) => fresh(files, title, &mut report)?,
        Err(_) if rebuild => {
            let kept = files.sidecar().with_file_name(format!("{}.v2.json.corrupt-{}", files.stem, Local::now().format("%H%M%S")));
            std::fs::rename(files.sidecar(), &kept).context("set the corrupt sidecar aside")?;
            report.corrupt_kept = Some(kept);
            report.how = How::Rebuilt;
            rebuilt(files)?
        }
        Err(e) => {
            return Err(e.context("run again with --rebuild to rebuild it from the notes, transcript and slides: everything after the last <!-- --> marker becomes pending, and the corrupt file is kept beside it"))
        }
    };
    sc.lecture_date.get_or_insert(files.date);
    report.journal = notesfile::recover(files, &mut sc)?;
    if !files.notes.exists() {
        std::fs::write(&files.notes, format!("{title}\n")).with_context(|| format!("create {}", files.notes.display()))?;
    }
    let edited = notesfile::accept_external_edit(files, &mut sc)?;
    report.external_edit = edited && report.how == How::Resumed;
    for s in slide_files(files)? {
        if !sc.slides.iter().any(|x| x.index == s.index) {
            sc.slides.push(s);
            report.slides_registered += 1;
        }
    }
    sc.slides.sort_by_key(|s| s.index);
    sc.save(&files.sidecar())?;
    report.pending_segments = (segments::read(&files.segments())?.len() as u64).saturating_sub(sc.notes.segment_cursor);
    report.pending_slides = sc.slides.iter().filter(|s| s.index > sc.notes.slide_index).count();
    Ok((sc, report))
}

fn fresh(files: &LectureFiles, title: &str, report: &mut InitReport) -> Result<Sidecar> {
    if files.legacy_state().exists() {
        report.how = How::Migrated;
        return migrate(files, report);
    }
    if files.notes.exists() {
        report.how = How::Rebuilt;
        return rebuilt(files);
    }
    std::fs::write(&files.notes, format!("{title}\n")).with_context(|| format!("create {}", files.notes.display()))?;
    report.how = How::Created;
    import(files, |_| false, 0)
}

struct Line {
    pos: usize,
    clock: String,
    text: String,
}

fn transcript_lines(files: &LectureFiles) -> Result<Vec<Line>> {
    let bytes = match std::fs::read(&files.transcript) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("read {}", files.transcript.display())),
    };
    let mut out = Vec::new();
    let mut pos = 0;
    for raw in bytes.split_inclusive(|&b| b == b'\n') {
        let text = String::from_utf8_lossy(raw);
        if let Some(c) = LINE.captures(text.trim_end_matches('\n')) {
            out.push(Line { pos, clock: c[1].to_string(), text: c[2].to_string() });
        }
        pos += raw.len();
    }
    Ok(out)
}

/// A local time on a day; in a daylight-saving gap, the hour after it.
fn local(date: NaiveDate, t: NaiveTime) -> DateTime<Local> {
    let naive = date.and_time(t);
    Local.from_local_datetime(&naive).earliest().unwrap_or_else(|| Local.from_local_datetime(&(naive + chrono::Duration::hours(1))).earliest().expect("an hour past a gap exists"))
}

/// The CLI's transcript lines as segments: `imported`, no words, at the lecture day's clock time,
/// a day later once the clock runs back past midnight.
fn imported(lines: &[Line], date: NaiveDate) -> Vec<Segment> {
    let (mut day, mut prev) = (date, None::<NaiveTime>);
    lines
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let t = NaiveTime::parse_from_str(&l.clock, "%H:%M:%S").expect("LINE matched HH:MM:SS");
            if prev.is_some_and(|p| p - t > chrono::Duration::hours(12)) {
                day = day.succ_opt().expect("a next day");
            }
            prev = Some(t);
            let at = local(day, t);
            Segment { id: i as u64, recording_id: Uuid::nil(), start_sample: 0, end_sample: 0, said_at: at, start: at, end: at, text: l.text.clone(), words: vec![], source: SegmentSource::Imported }
        })
        .collect()
}

pub fn relative(files: &LectureFiles, path: &Path) -> String {
    path.strip_prefix(&files.dir).unwrap_or(path).to_string_lossy().into_owned()
}

/// Slide files already named as the CLI names them, by index; each is dated the lecture day at the time in its name.
pub fn slide_files(files: &LectureFiles) -> Result<Vec<SlideEntry>> {
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(&files.slides) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e).with_context(|| format!("read {}", files.slides.display())),
    };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(c) = SLIDE_NAME.captures(&name) else { continue };
        let (Ok(index), Ok(t)) = (c[1].parse::<u32>(), NaiveTime::parse_from_str(&c[2], "%H%M%S")) else { continue };
        out.push(SlideEntry { index, file: relative(files, &e.path()), shown_at: local(files.date, t) });
    }
    out.sort_by_key(|s| s.index);
    Ok(out)
}

/// The next slide index: one past the highest registered or on disk (spec §8).
pub fn next_slide_index(files: &LectureFiles, sc: &Sidecar) -> Result<u32> {
    let on_disk = slide_files(files)?.iter().map(|s| s.index).max().unwrap_or(0);
    Ok(sc.slides.iter().map(|s| s.index).max().unwrap_or(0).max(on_disk) + 1)
}

/// Transcript lines and slide files as segments and slides; lines for which `noted(pos, …)` holds
/// and slides up to `slide_index` count as in the notes. Writes the segment log.
fn import(files: &LectureFiles, noted: impl Fn(&Segment) -> bool, slide_index: u32) -> Result<Sidecar> {
    let segs = imported(&transcript_lines(files)?, files.date);
    segments::write_all(&files.segments(), &segs)?;
    let mut sc = Sidecar { lecture_date: Some(files.date), slides: slide_files(files)?, ..Sidecar::default() };
    sc.notes.segment_cursor = segs.iter().take_while(|s| noted(s)).count() as u64;
    sc.notes.slide_index = slide_index;
    Ok(sc)
}

/// Finishes or undoes the CLI's interrupted snapshot as its `recover_commit` does, in its words.
fn legacy_commit(files: &LectureFiles, state: &mut Value) -> Result<Option<&'static str>> {
    let Some(c) = state.as_object_mut().and_then(|o| o.remove("commit")) else { return Ok(None) };
    let block = c["block"].as_str().unwrap_or_default().as_bytes().to_vec();
    let before = c["before"].as_u64().unwrap_or(0) as usize;
    let data = std::fs::read(&files.notes).with_context(|| format!("read {}", files.notes.display()))?;
    let tail = data.get(before..).unwrap_or(&[]);
    if data.len() >= before && tail == block.as_slice() {
        state["transcript_offset"] = c["transcript_offset"].clone();
        state["slide_index"] = c["slide_index"].clone();
        Ok(Some("the last snapshot was fully written"))
    } else if data.len() > before && block.starts_with(tail) {
        let f = std::fs::OpenOptions::new().write(true).open(&files.notes)?;
        f.set_len(before as u64)?;
        f.sync_all()?;
        Ok(Some("removed a half-written snapshot; its material is queued again"))
    } else if data.len() != before {
        Ok(Some("the notes changed during an interrupted snapshot; left as they are, material queued again"))
    } else {
        Ok(None)
    }
}

fn migrate(files: &LectureFiles, report: &mut InitReport) -> Result<Sidecar> {
    let path = files.legacy_state();
    let mut state: Value = serde_json::from_slice(&std::fs::read(&path)?).with_context(|| format!("read the Python CLI's state {}", path.display()))?;
    report.legacy_commit = legacy_commit(files, &mut state)?;
    let lines = transcript_lines(files)?;
    if let Some(offset) = state["transcript_offset"].as_u64() {
        let committed: Vec<usize> = lines.iter().filter(|l| (l.pos as u64) < offset).map(|l| l.pos).collect();
        let n = committed.len();
        return import(files, |s| (s.id as usize) < n, state["slide_index"].as_u64().unwrap_or(0) as u32);
    }
    if let Some(cut) = state["noted_through"].as_str().map(str::to_string) {
        // The older checkpoint: the first line at or after it starts the pending part (initial_state).
        let n = lines.iter().position(|l| l.clock >= cut).unwrap_or(lines.len());
        let slide_index = slide_files(files)?.iter().filter(|s| s.shown_at.format("%H:%M:%S").to_string() < cut).map(|s| s.index).max().unwrap_or(0);
        return import(files, |s| (s.id as usize) < n, slide_index);
    }
    rebuilt(files)
}

/// Spec §8's rebuild: everything after the last `<!-- HH:MM:SS -->` marker is pending. A segment log
/// already there is kept.
fn rebuilt(files: &LectureFiles) -> Result<Sidecar> {
    let doc = std::fs::read_to_string(&files.notes).unwrap_or_default();
    let upto = MARKER.captures_iter(&doc).last().and_then(|c| NaiveTime::parse_from_str(&c[1], "%H:%M:%S").ok()).map(|t| local(files.date, t));
    let noted = |at: DateTime<Local>| upto.is_some_and(|u| at <= u);
    let slide_index = slide_files(files)?.iter().filter(|s| noted(s.shown_at)).map(|s| s.index).max().unwrap_or(0);
    let logged = segments::read(&files.segments())?;
    if logged.is_empty() {
        return import(files, |s| noted(s.said_at), slide_index);
    }
    let mut sc = Sidecar { lecture_date: Some(files.date), slides: slide_files(files)?, ..Sidecar::default() };
    sc.notes.segment_cursor = logged.iter().take_while(|s| noted(s.said_at)).count() as u64;
    sc.notes.slide_index = slide_index;
    Ok(sc)
}

/// Transcript gaps other days' sessions left in this folder (spec §5.4): a recovery-only session per
/// such day, before today's starts. Their segments reach that day's transcript; its notes stay as they are.
pub async fn recover_other_days(dir: &Path, today: &str, recovery: impl Fn() -> Result<RecoveryLink>, spend: Option<Spend>) -> Result<Vec<(String, StopReport)>> {
    let mut sidecars: Vec<PathBuf> = match std::fs::read_dir(dir.join(".live_notes")) {
        Ok(e) => e.flatten().map(|e| e.path()).filter(|p| p.to_string_lossy().ends_with(".v2.json")).collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    sidecars.sort();
    let mut out = Vec::new();
    for path in sidecars {
        let stem = path.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_suffix(".v2.json")).unwrap_or_default().to_string();
        if stem == today {
            continue;
        }
        let Some(sc) = Sidecar::load(&path)? else { continue };
        if !sc.gaps.iter().any(|g| g.kind.is_transcript() && !g.resolved) {
            continue;
        }
        let cfg = SessionConfig { dir: dir.to_path_buf(), stem: stem.clone(), recovery: Some(recovery()?), spend: spend.clone(), ..Default::default() };
        let (handle, _notes) = coordinator::spawn(cfg, Box::new(NoAudio));
        out.push((stem, handle.finish().await?));
    }
    Ok(out)
}
```

The `migrate` branch's `import(files, |s| (s.id as usize) < n, …)`: `import`'s cursor counts the leading segments for which `noted` holds, and imported ids are line positions, so the cursor is `n`.

`live_notes.py`: add the check shown above, with the Edit tool. Nothing else in the file changes.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p lecturelive-core --lib session::segments session::folder`
Expected: PASS.

Then the Python CLI's refusal, on synthetic folders under the scratchpad (no network: `page` exits before any request when the folder has no notes):

```bash
S=<scratchpad>/pyrefuse; mkdir -p "$S/v2/.live_notes" "$S/plain"; echo '{"version":2}' > "$S/v2/.live_notes/lecture_notes_20260925.v2.json"
(cd "$S/v2" && $HOME/Documents/Tools/LectureLive/.venv/bin/python $HOME/Documents/Tools/LectureLive/live_notes.py page; echo "exit $?")
(cd "$S/plain" && $HOME/Documents/Tools/LectureLive/.venv/bin/python $HOME/Documents/Tools/LectureLive/live_notes.py page; echo "exit $?")
```

Expected: the first prints "This folder is kept by the LectureLive app now (.live_notes/*.v2.json). Run `lecturelive lecture` here instead." and `exit 1`. The second prints "No notes to typeset: lecture_notes_<today>.md is not in this folder." and `exit 1`, showing the check fires only on v2 folders. Record both outputs in the ledger.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/session/mod.rs crates/core/src/session/folder.rs crates/core/src/session/segments.rs crates/core/src/session/coordinator.rs crates/core/src/audio/source.rs live_notes.py
git commit -m "Initialise lecture folders and migrate the Python CLI's folders to the v2 sidecar

Legacy transcript lines and slides become imported segments and registered slides; what
the CLI had noted, by offset or by its older timestamp checkpoint, stays noted, after its
own half-written snapshot is finished or undone. Notes without state and corrupt sidecars
are rebuilt from the last marker. A transcript line lost between the log's and the
transcript's syncs is put back, other days' transcript gaps are recovered at launch, and
live_notes.py now exits in a folder with a v2 sidecar."
```

---

### Task 9: Polish with abort, backup and atomic replace (milestone task 7)

**Files:**
- Create: `crates/core/src/notes/polish.rs`; `notes/mod.rs` gains `pub mod polish;`
- Modify: `crates/core/src/session/notesfile.rs` (`replace`)

**Interfaces:**
- Consumes: `prompts::{polish_system, polish_user}`, `embeds::{clean_output, embeds_in, repair, Repaired}`, `chat::{ChatRequest, Content}`.
- Produces: `notes::polish::{request(course: &str, title: &str, doc: &str, transcript: &str) -> ChatRequest, validate(output: &str, doc: &str) -> Repaired}`; `notesfile::replace(&LectureFiles, &mut Sidecar, polished: &str, based_on: &str /* SHA-256 of the notes polished */, at: DateTime<Local>) -> Result<PathBuf /* the backup */>`.

Spec §6.3 in order: a polish runs only after a snapshot has flushed and committed all pending material, and stops if that fails (the orchestration, Task 11). The request carries the notes and the whole transcript. The answer is validated: title kept, every embed of the notes exactly once. If the notes changed while the request ran, nothing is written, because an edit is never overwritten from memory. Otherwise the previous notes are copied to `.live_notes/<stem>_HHMMSS.md` (with `_2`, `_3` … if that exists) and fsynced. The new text, trimmed with one final newline, goes through temp file and rename, and the revision advances.

- [ ] **Step 1: Write the failing tests**

`notesfile.rs` tests (appended):

```rust
    #[test]
    fn polish_replaces_the_notes_atomically_after_a_backup() {
        let (_dir, files, mut sc) = folder();
        let old = std::fs::read(&files.notes).unwrap();
        let at = chrono::Local.with_ymd_and_hms(2026, 9, 25, 10, 15, 0).unwrap();
        let backup = replace(&files, &mut sc, "# Title\n\nA summary.\n\n", &sha256_hex(&old), at).unwrap();
        assert_eq!(backup, files.state_dir().join("lecture_notes_20260925_101500.md"));
        assert_eq!(std::fs::read(&backup).unwrap(), old);
        assert_eq!(std::fs::read_to_string(&files.notes).unwrap(), "# Title\n\nA summary.\n");
        assert_eq!(sc.notes.revision, 2);
        assert_eq!(Sidecar::load(&files.sidecar()).unwrap().unwrap(), sc);
        let again = replace(&files, &mut sc, "# Title\n\nShorter.\n", &sha256_hex(&std::fs::read(&files.notes).unwrap()), at).unwrap();
        assert_eq!(again, files.state_dir().join("lecture_notes_20260925_101500_2.md"), "a backup never overwrites another");
    }

    #[test]
    fn a_polish_over_notes_edited_meanwhile_is_not_written() {
        let (_dir, files, mut sc) = folder();
        let polished_from = sha256_hex(&std::fs::read(&files.notes).unwrap());
        std::fs::OpenOptions::new().append(true).open(&files.notes).unwrap().write_all(b"- typed while the polish ran\n").unwrap();
        let edited = std::fs::read(&files.notes).unwrap();
        let err = replace(&files, &mut sc, "# Polished\n", &polished_from, chrono::Local::now()).unwrap_err();
        assert!(format!("{err:#}").contains("changed while"), "{err:#}");
        assert_eq!(std::fs::read(&files.notes).unwrap(), edited);
        assert_eq!(std::fs::read_dir(files.state_dir()).unwrap().filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().ends_with(".md")).count(), 0, "no backup either");
    }
```

`polish.rs` tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::notes::embeds::NOT_PLACED;

    #[test]
    fn the_polished_notes_keep_the_title_and_every_embed_once() {
        let (s1, s2) = ("![Slide 1](slides/slide_01_100000.png)", "![Slide 2](slides/slide_02_100500.png)");
        let doc = format!("# T\n\n<!-- 10:00:10 -->\n## A\n{s1}\n- a\n\n<!-- 10:05:10 -->\n{s2}\n- b\n");
        let out = format!("```markdown\n# T\n\nSummary.\n\n## A\n{s1}\n- a\n{s1}\n```");
        let r = validate(&out, &doc);
        assert_eq!(r.text, format!("# T\n\nSummary.\n\n## A\n{s1}\n- a\n\n{NOT_PLACED}\n\n{s2}"));
    }

    #[test]
    fn a_polish_request_is_the_clis() {
        let r = request("Machine Learning", "# ML — W — 2026-09-25", "doc", "tr");
        assert_eq!(r.what, SpendKind::Polish);
        assert_eq!(r.system, crate::notes::prompts::polish_system("Machine Learning"));
        assert!(matches!(&r.content, Content::Text(t) if *t == crate::notes::prompts::polish_user("# ML — W — 2026-09-25", "doc", "tr")));
        assert_eq!((r.effort, r.timeout), (None, Duration::from_secs(600)));
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p lecturelive-core --lib notes::polish session::notesfile`
Expected: FAIL to compile.

- [ ] **Step 3: Implement**

`crates/core/src/notes/polish.rs`:

```rust
//! Polish (spec §6.3): the notes and the whole transcript rewritten into one study document.
use std::time::Duration;

use crate::notes::chat::{ChatRequest, Content};
use crate::notes::embeds::{clean_output, embeds_in, repair, Repaired};
use crate::notes::prompts::{polish_system, polish_user};
use crate::session::spend::SpendKind;

pub fn request(course: &str, title: &str, doc: &str, transcript: &str) -> ChatRequest {
    ChatRequest { what: SpendKind::Polish, system: polish_system(course), content: Content::Text(polish_user(title, doc, transcript)), effort: None, timeout: Duration::from_secs(600) }
}

/// The document as it will be written: output cleaned with its title kept, every embed of the notes exactly once.
pub fn validate(output: &str, doc: &str) -> Repaired {
    repair(&clean_output(output, false), &embeds_in(doc))
}
```

`notesfile.rs`:

```rust
/// Polish's replace (spec §6.3). `based_on` is the SHA-256 of the notes the polish was made from:
/// if they have changed since, nothing is written. Returns the backup's path.
pub fn replace(files: &LectureFiles, sc: &mut Sidecar, polished: &str, based_on: &str, at: DateTime<Local>) -> Result<std::path::PathBuf> {
    let current = std::fs::read(&files.notes).with_context(|| format!("read {}", files.notes.display()))?;
    if sha256_hex(&current) != based_on {
        bail!("the notes changed while they were being polished, so the polished version was not written; {} is as it was left", files.notes.display());
    }
    accept_external_edit(files, sc)?;
    let base = format!("{}_{}", files.stem, at.format("%H%M%S"));
    let backup = (1..).map(|n| files.state_dir().join(if n == 1 { format!("{base}.md") } else { format!("{base}_{n}.md") })).find(|p| !p.exists()).expect("an unused name");
    std::fs::write(&backup, &current).and_then(|()| std::fs::File::open(&backup)?.sync_all()).with_context(|| format!("back up the notes to {}", backup.display()))?;
    write_atomic(&files.notes, format!("{}\n", polished.trim_end()).as_bytes()).with_context(|| format!("write {}", files.notes.display()))?;
    let (len, sha) = fingerprint(files)?;
    sc.notes.revision += 1;
    sc.notes.len = len;
    sc.notes.sha256 = sha;
    sc.save(&files.sidecar())?;
    Ok(backup)
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p lecturelive-core --lib notes::polish session::notesfile`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/notes/mod.rs crates/core/src/notes/polish.rs crates/core/src/session/notesfile.rs
git commit -m "Polish into a backed-up, atomically replaced notes file

The polished text keeps every slide embed exactly once, the previous notes go to a
uniquely named backup first, and a polish over notes edited while it ran is not written."
```

---

### Task 10: Study page (milestone task 11)

**Files:**
- Create: `crates/core/src/notes/page.rs`; `notes/mod.rs` gains `pub mod page;`
- Create: `crates/core/tests/notes_page.rs`; `crates/core/tests/fixtures/notes/lecture/lecture_notes_20260925.md`. The fixture is synthetic notes on gradient descent and its variants, about 2,200 words: a title line, three `<!-- HH:MM:SS -->` blocks, `##`/`###` sections with formulas in TeX, one worked example, and three slide embeds `![Slide 1](slides/slide_01_100512.png)`, `![Slide 2](slides/slide_02_101830.png)`, `![Slide 3](slides/slide_03_103245.png)`. It is written for this task, and no real lecture's notes are used.
- Modify: `crates/core/tests/support/fake_sse.rs` (`study_page(words) -> String`), `crates/core/tests/support/mod.rs` (`slides.rs` with `png(&Path)`, which draws a small chart so `sips` has something to convert)

**Interfaces:**
- Consumes: `prompts::{page_system, revise_system, page_user, revise_user}`, `chat::{ChatClient, ChatRequest, Content, Image, ChatError}`, `embeds::clean_output`, `pyjson::{dumps, round_int}`, `notesfile::sha256_hex`, `fsutil::write_atomic`.
- Produces: `notes::page::{TEMPLATE: &str, EFFORT: &str = "medium", notes_words(&str) -> usize, page_budget(&str) -> u32, max_slides(usize) -> u32, visible_words(&str) -> usize, unescape(&str) -> String, escape(&str) -> String, strip_unsafe(&str) -> String, Fills { keystone, summary, content, slides: String }, Fills::missing() -> Vec<&'static str>, cut(out: &str, embeds: &[(String, String)]) -> Fills, fill(template: &str, fills: &HashMap<&str, String>) -> String, page_path(notes: &Path, lecture_name: &str) -> PathBuf, slide_uri(&Path) -> Option<String>, load_cache(&Path, key: &str) -> Option<(Fills, usize)>, save_cache(&Path, key, &Fills, words: usize, budget: u32) -> Result<()>, render(&Fills, course, notes: &Path, lecture_name, template: &str) -> Result<PathBuf>, PageOutcome { path, words, budget, cached, missing }, make_page(&ChatClient, course: &str, notes: &Path, cache: &Path, lecture_name: &str, template: &str, progress: &(dyn Fn(String) + Send + Sync)) -> Result<PageOutcome>}`.

Everything here is `live_notes.py`'s `page_budget`, `visible_words`, `typeset`, `typeset_page`, `page_path`, `slide_uri` and `render_html`, ported. The cache file is the CLI's too: `{"source": sha256(system + notes), "fills": {keystone, summary, content, slides}, "words", "budget"}`, written with `pyjson::dumps`. A page one tool typeset re-renders in the other without a request. `TEMPLATE` is `include_str!` of the repository's `notes_template.html`, embedded unchanged (spec §6.4). The template's own design is not touched, so `/frontend-design:frontend-design` is not needed. If any template or output change turns out to be needed, it goes through that skill first, and the handover says so.

- [ ] **Step 1: Write the failing tests**

`page.rs` unit tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn words(n: usize) -> String {
        (0..n).map(|i| format!("w{i}")).collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn the_budget_is_thirty_percent_of_the_notes_in_fifties_between_600_and_2500() {
        assert_eq!(page_budget(""), 600);
        assert_eq!(page_budget(&words(2_500)), 750);
        assert_eq!(page_budget(&words(2_584)), 800, "15.504 rounds up");
        assert_eq!(page_budget(&words(2_750)), 800, "16.5 rounds to even, as Python rounds");
        assert_eq!(page_budget(&words(10_000)), 2_500);
        assert_eq!(page_budget(&format!("{} ![Slide 1](slides/a.png)", words(2_500))), 750, "embeds are not words");
        assert_eq!([max_slides(0), max_slides(5), max_slides(10), max_slides(15), max_slides(40)], [2, 2, 3, 4, 8]);
    }

    #[test]
    fn visible_words_skip_markup_and_drawings_and_split_on_non_breaking_spaces() {
        let html = "<p>one &amp; two</p><svg viewBox=\"0 0 1 1\"><text>not counted</text></svg><p>three&nbsp;four</p>";
        assert_eq!(visible_words(html), 5);
        assert_eq!(unescape("&lt;a&gt; &#39;b&#x27; &quot;c&quot; &amp;amp;"), "<a> 'b' \"c\" &amp;");
        assert_eq!(escape("A & B <x> \"q\" Rock'n'Roll"), "A &amp; B &lt;x&gt; &quot;q&quot; Rock&#x27;n&#x27;Roll");
    }

    #[test]
    fn scripts_styles_and_event_or_style_attributes_are_stripped() {
        let html = "<p onclick=\"x()\" style='color:red' class=\"a\">a</p><script>alert(1)</script><STYLE>p{}</style><svg onload=go>b</svg>";
        assert_eq!(strip_unsafe(html), "<p class=\"a\">a</p><svg>b</svg>");
    }

    #[test]
    fn the_page_is_recut_at_every_h2_whatever_the_models_own_wrappers() {
        let out = "<div class=\"keystone\">\\[ x \\]</div>\n<p class=\"lede\">Lede.</p>\n<section class=\"part\"><h2>A</h2><p>a</p></section>\n<h2>B</h2><p>b</p>\n<section class=\"part\"><h2>Glossary</h2><dl class=\"glossary\"><dt>t</dt><dd>d</dd></dl></section>\n<ol class=\"quiz\"><li><p class=\"q\">Q?</p><div class=\"answer\">A.</div></li></ol>";
        let f = cut(out, &[("3".into(), "slides/x</y.png".into()), ("7".into(), "slides/b.png".into())]);
        assert_eq!(f.keystone, "<div class=\"keystone\">\\[ x \\]</div>");
        assert_eq!(f.summary, "<p class=\"lede\">Lede.</p>");
        assert_eq!(
            f.content,
            "<section class=\"part\">\n<h2>A</h2><p>a</p>\n</section>\n<section class=\"part\">\n<h2>B</h2><p>b</p>\n</section>\n<section class=\"part\">\n<h2>Glossary</h2><dl class=\"glossary\"><dt>t</dt><dd>d</dd></dl>\n</section>\n<section class=\"part check\">\n<h2>Check yourself</h2>\n<ol class=\"quiz\"><li><p class=\"q\">Q?</p><div class=\"answer\">A.</div></li></ol>\n</section>"
        );
        assert_eq!(f.slides, "{\"3\": \"slides/x<\\/y.png\", \"7\": \"slides/b.png\"}", "the CLI's JSON, safe inside a script element");
        assert_eq!(f.missing(), vec!["takeaways"]);
    }

    #[test]
    fn the_page_is_named_after_the_lecture_title() {
        let dir = tempfile::tempdir().unwrap();
        let notes = dir.path().join("lecture_notes_20260925.md");
        std::fs::write(&notes, "").unwrap();
        assert_eq!(page_path(&notes, "Week 06 — Statistical Analysis Methods"), dir.path().join("Statistical Analysis Methods.html"));
        assert_eq!(page_path(&notes, "Week 07 — Tests: t/z"), dir.path().join("Tests- t-z.html"));
        assert_eq!(page_path(&notes, "Optimisation"), dir.path().join("Optimisation.html"));
        std::fs::write(dir.path().join("lecture_notes_20260918.md"), "").unwrap();
        assert_eq!(page_path(&notes, "Week 06 — Statistical Analysis Methods"), dir.path().join("Statistical Analysis Methods (25 Sep).html"), "the date only when the folder holds two days");
    }

    #[test]
    fn the_template_is_filled_in_one_pass() {
        let fills: HashMap<&str, String> = [("title", "T &amp; U".to_string()), ("content", "<p>{{title}} stays</p>".to_string())].into();
        assert_eq!(fill("<t>{{title}}</t>{{content}}{{unknown}}", &fills), "<t>T &amp; U</t><p>{{title}} stays</p>{{unknown}}");
        assert!(TEMPLATE.contains("{{content}}") && TEMPLATE.contains("{{slides}}"), "the repository's template is embedded");
    }

    #[test]
    fn the_cache_is_the_clis_file_keyed_to_prompt_and_notes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".live_notes/x.page.json");
        let f = Fills { keystone: "k".into(), summary: "s".into(), content: "c — é".into(), slides: "{}".into() };
        save_cache(&path, "abc", &f, 612, 650).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"source\": \"abc\", \"fills\": {\"keystone\": \"k\", \"summary\": \"s\", \"content\": \"c \\u2014 \\u00e9\", \"slides\": \"{}\"}, \"words\": 612, \"budget\": 650}");
        assert_eq!(load_cache(&path, "abc"), Some((f, 612)));
        assert_eq!(load_cache(&path, "abd"), None);
    }
}
```

`crates/core/tests/notes_page.rs`:

```rust
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
    std::fs::copy(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/notes/lecture/lecture_notes_20260925.md"), &notes).unwrap();
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
        let system = body["messages"][0]["content"].as_str().unwrap();
        // A first draft half again over budget; the revision within it.
        let words = if system.starts_with("You shorten") { budget * 9 / 10 } else { budget * 3 / 2 };
        fake_sse::answer(&study_page(words), 30_000_000)
    })
    .await;
    let chat = client(&fake.url, dir.path());
    let cache = dir.path().join(".live_notes/lecture_notes_20260925.page.json");
    let name = "Week 03 — Optimisation";

    let first = make_page(&chat, "Machine Learning", &notes, &cache, name, TEMPLATE, &|_| {}).await.unwrap();
    assert_eq!(fake.state.requests(), 2, "a draft over budget gets one revision");
    let bodies = fake.state.bodies();
    assert_eq!(bodies[0]["reasoning_effort"], "medium");
    assert_eq!(bodies[0]["messages"][1]["content"].as_array().unwrap().len(), 4, "the notes and every slide image");
    assert!(bodies[1]["messages"][0]["content"].as_str().unwrap().starts_with("You shorten"));
    assert!(bodies[1]["messages"][1]["content"].is_string(), "the revision carries no images");
    assert!(!first.cached);
    assert!(first.words <= first.budget as usize, "{} words of {}", first.words, first.budget);
    assert!(first.missing.is_empty(), "complete: {:?}", first.missing);
    assert_eq!(first.path, dir.path().join("Optimisation.html"));
    let html = std::fs::read_to_string(&first.path).unwrap();
    assert!(!html.contains("{{"), "every placeholder filled");
    assert!(html.contains("<title>Optimisation — Machine Learning</title>"));
    assert!(html.contains("data:image/jpeg;base64,"), "the slides are embedded");

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
}

#[tokio::test]
async fn a_page_that_fails_after_its_retry_writes_nothing() {
    let (dir, notes) = lecture();
    let fake = fake_sse::start(|_| fake_sse::Reply::Stream(vec![b"data: [DONE]\n\n".to_vec()])).await;
    let chat = client(&fake.url, dir.path());
    let before = std::fs::read(&notes).unwrap();
    let err = make_page(&chat, "Machine Learning", &notes, &dir.path().join(".live_notes/p.json"), "Week 03 — Optimisation", TEMPLATE, &|_| {}).await;
    assert!(err.is_err());
    assert_eq!(fake.state.requests(), 2, "one retry");
    assert!(!dir.path().join("Optimisation.html").exists());
    assert_eq!(std::fs::read(&notes).unwrap(), before, "the notes are untouched");
}
```

`fake_sse::study_page` (support):

```rust
/// A complete study page in the prompt's vocabulary with exactly `words` visible words.
pub fn study_page(words: usize) -> String {
    let page = |filler: &str| format!(
        "<div class=\"keystone\">\\[ \\theta \\leftarrow \\theta - \\eta \\nabla L \\]</div>\n<p class=\"lede\">Gradient descent and its variants.</p>\n<section class=\"part\"><h2>Descent</h2><p>{filler}</p><div class=\"formula\" data-name=\"update\">\\[ \\theta_{{t+1}} = \\theta_t - \\eta g_t \\]</div></section>\n<section class=\"part\"><h2>Glossary</h2><dl class=\"glossary\"><dt>Step</dt><dd>One update.</dd></dl></section>\n<section class=\"part\"><h2>Key takeaways</h2><ul class=\"takeaways\"><li>Scale the rate.</li></ul></section>\n<ol class=\"quiz\"><li><p class=\"q\">Why decay the rate?</p><div class=\"answer\">To settle.</div></li></ol>"
    );
    let base = lecturelive_core::notes::page::visible_words(&page(""));
    let filler: Vec<String> = (0..words.saturating_sub(base)).map(|i| format!("w{i}")).collect();
    page(&filler.join(" "))
}
```

`support/slides.rs`:

```rust
//! Slide images for tests: a small chart, so `sips` has real pixels to convert.
use std::path::Path;

pub fn png(path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut img = image::RgbImage::from_pixel(320, 180, image::Rgb([255, 255, 255]));
    for x in 20..300u32 {
        let y = 160 - ((x as f32 - 160.0).powi(2) / 180.0) as u32;
        img.put_pixel(x, y.min(179), image::Rgb([20, 20, 20]));
    }
    img.save(path).unwrap();
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p lecturelive-core --lib notes::page` and `cargo test -p lecturelive-core --test notes_page`
Expected: FAIL to compile.

- [ ] **Step 3: Implement**

`crates/core/src/notes/page.rs` above the tests:

```rust
//! The study page (spec §6.4): one distillation request over the whole notes and slides within a
//! word budget, one revision on overshoot, re-cut at every <h2>, cached against the prompt and the
//! notes in the Python CLI's own file, and filled into notes_template.html in one pass.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;
use chrono::NaiveDate;
use regex::{Captures, Regex};
use serde_json::{json, Map, Value};

use crate::fsutil::write_atomic;
use crate::notes::chat::{ChatClient, ChatError, ChatRequest, Content, Image};
use crate::notes::embeds::clean_output;
use crate::notes::prompts::{page_system, page_user, revise_system, revise_user};
use crate::pyjson::{dumps, round_int};
use crate::session::notesfile::sha256_hex;
use crate::session::spend::SpendKind;

/// The study page's design, embedded unchanged at build time (spec §3.1, §6.4).
pub const TEMPLATE: &str = include_str!("../../../../notes_template.html");
/// At the default (high) one request over a two-hour lecture ran past ten minutes (spec §6.4).
pub const EFFORT: &str = "medium";
const TIMEOUT: Duration = Duration::from_secs(1_200);
const EMBED_JPEG_QUALITY: u32 = 80;

fn re(p: &str) -> Regex {
    Regex::new(p).expect("a valid pattern")
}

static SLIDE_EMBED: LazyLock<Regex> = LazyLock::new(|| re(r"!\[Slide (\d+)\]\(([^)]+)\)"));
static TAGS: LazyLock<Regex> = LazyLock::new(|| re(r"(?is)<svg\b.*?</svg>|<[^>]+>"));
static UNSAFE: LazyLock<Regex> = LazyLock::new(|| re(r#"(?is)<script\b.*?</script\s*>|<style\b.*?</style\s*>|\s(?:on\w+|style)\s*=\s*(?:"[^"]*"|'[^']*'|[^\s>]+)"#));
static KEYSTONE: LazyLock<Regex> = LazyLock::new(|| re(r#"(?s)<div class="keystone">.*?</div>"#));
static LEDE: LazyLock<Regex> = LazyLock::new(|| re(r#"(?s)<p class="lede">.*?</p>"#));
static QUIZ: LazyLock<Regex> = LazyLock::new(|| re(r#"(?s)<ol class="quiz">.*</ol>"#));
static SECTION_TAG: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)</?section\b[^>]*>"));
static H2: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)<h2[\s>]"));
static SRC: LazyLock<Regex> = LazyLock::new(|| re(r#"src="([^"]+)""#));
static PLACEHOLDER: LazyLock<Regex> = LazyLock::new(|| re(r"\{\{(\w+)\}\}"));
static STAMP: LazyLock<Regex> = LazyLock::new(|| re(r"\d{8}"));
static ENTITY: LazyLock<Regex> = LazyLock::new(|| re(r"&(#[0-9]+|#[xX][0-9a-fA-F]+|[a-zA-Z]+);"));

pub fn notes_words(doc: &str) -> usize {
    SLIDE_EMBED.replace_all(doc, "").split_whitespace().count()
}

/// Visible words the page may use: 30% of the notes, in fifties, from 600 to 2,500.
pub fn page_budget(doc: &str) -> u32 {
    (round_int(notes_words(doc) as f64 * 0.3 / 50.0) * 50).clamp(600, 2_500) as u32
}

/// Slides the page may redraw: 30% of them, from 2 to 8.
pub fn max_slides(slides: usize) -> u32 {
    round_int(slides as f64 * 0.3).clamp(2, 8) as u32
}

/// The HTML entities a page uses; others stay as written (Python's `html.unescape` for these).
pub fn unescape(s: &str) -> String {
    ENTITY
        .replace_all(s, |c: &Captures| {
            let e = &c[1];
            let ch = if let Some(hex) = e.strip_prefix("#x").or_else(|| e.strip_prefix("#X")) {
                u32::from_str_radix(hex, 16).ok().and_then(char::from_u32)
            } else if let Some(dec) = e.strip_prefix('#') {
                dec.parse().ok().and_then(char::from_u32)
            } else {
                match e {
                    "amp" => Some('&'),
                    "lt" => Some('<'),
                    "gt" => Some('>'),
                    "quot" => Some('"'),
                    "apos" => Some('\''),
                    "nbsp" => Some('\u{a0}'),
                    "ndash" => Some('–'),
                    "mdash" => Some('—'),
                    "hellip" => Some('…'),
                    "times" => Some('×'),
                    "minus" => Some('−'),
                    _ => None,
                }
            };
            ch.map_or_else(|| c[0].to_string(), String::from)
        })
        .into_owned()
}

/// Python's `html.escape(s, quote=True)`.
pub fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#x27;")
}

/// Words a reader sees, the measure the budget is checked against; drawings are not counted.
pub fn visible_words(html: &str) -> usize {
    unescape(&TAGS.replace_all(html, " ")).split_whitespace().count()
}

/// The template owns all styling and behaviour: generated fragments may not bring their own.
pub fn strip_unsafe(html: &str) -> String {
    UNSAFE.replace_all(html, "").into_owned()
}

#[derive(Debug, Clone, PartialEq)]
pub struct Fills {
    pub keystone: String,
    pub summary: String,
    pub content: String,
    /// `{"N": "path"}` as the CLI writes it, with `</` escaped.
    pub slides: String,
}

impl Fills {
    /// The parts spec §6.4 promises that this page lacks.
    pub fn missing(&self) -> Vec<&'static str> {
        let has = |class: &str| self.content.contains(&format!("class=\"{class}\""));
        let mut out = Vec::new();
        if self.keystone.is_empty() {
            out.push("keystone");
        }
        if self.summary.is_empty() {
            out.push("summary");
        }
        if !self.content.contains("<section class=\"part\">") {
            out.push("sections");
        }
        for (class, name) in [("glossary", "glossary"), ("takeaways", "takeaways"), ("quiz", "questions")] {
            if !has(class) {
                out.push(name);
            }
        }
        out
    }

    fn to_json(&self) -> Value {
        json!({"keystone": self.keystone, "summary": self.summary, "content": self.content, "slides": self.slides})
    }

    fn from_json(v: &Value) -> Option<Self> {
        let s = |k: &str| v[k].as_str().map(str::to_string);
        Some(Self { keystone: s("keystone")?, summary: s("summary")?, content: s("content")?, slides: s("slides")? })
    }
}

/// The CLI's cut of the model's page: keystone, lede and quiz lifted out, sections re-cut at every <h2>.
pub fn cut(out: &str, embeds: &[(String, String)]) -> Fills {
    let keystone = KEYSTONE.find(out).map(|m| m.as_str().to_string());
    let lede = LEDE.find(out).map(|m| m.as_str().to_string());
    let quiz = QUIZ.find(out).map(|m| m.as_str().to_string());
    let mut body = out.to_string();
    for m in [&keystone, &lede, &quiz].into_iter().flatten() {
        body = body.replace(m.as_str(), "");
    }
    let body = SECTION_TAG.replace_all(&body, "").into_owned();
    let mut bounds = vec![0];
    bounds.extend(H2.find_iter(&body).map(|m| m.start()).filter(|&s| s > 0));
    bounds.push(body.len());
    let mut sections: Vec<String> = bounds.windows(2).map(|w| body[w[0]..w[1]].trim()).filter(|c| !c.is_empty()).map(|c| format!("<section class=\"part\">\n{c}\n</section>")).collect();
    if let Some(q) = &quiz {
        sections.push(format!("<section class=\"part check\">\n<h2>Check yourself</h2>\n{q}\n</section>"));
    }
    let mut map = Map::new();
    for (n, p) in embeds {
        map.insert(n.clone(), json!(p));
    }
    Fills { keystone: keystone.unwrap_or_default(), summary: lede.unwrap_or_default(), content: sections.join("\n"), slides: dumps(&Value::Object(map)).replace("</", "<\\/") }
}

/// One pass: text inside the generated content is never read as a placeholder.
pub fn fill(template: &str, fills: &HashMap<&str, String>) -> String {
    PLACEHOLDER.replace_all(template, |c: &Captures| fills.get(&c[1]).cloned().unwrap_or_else(|| c[0].to_string())).into_owned()
}

/// The page is named after the lecture: the folder's title without its week prefix, with the date
/// only when the folder holds more than one day's notes.
pub fn page_path(notes: &Path, lecture_name: &str) -> PathBuf {
    let dir = notes.parent().unwrap_or(Path::new("."));
    let stem = notes.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let title = lecture_name.split_once(" — ").map(|(_, t)| t).filter(|t| !t.is_empty()).unwrap_or(lecture_name);
    let mut name = title.replace(['/', ':'], "-").trim().to_string();
    if name.is_empty() {
        name = stem.clone();
    }
    if let Some(stamp) = STAMP.find(&stem) {
        let days = std::fs::read_dir(dir).map(|e| e.flatten().filter(|e| { let n = e.file_name().to_string_lossy().into_owned(); n.starts_with("lecture_notes_") && n.ends_with(".md") }).count()).unwrap_or(0);
        if days > 1 {
            if let Ok(d) = NaiveDate::parse_from_str(stamp.as_str(), "%Y%m%d") {
                name += &format!(" ({})", d.format("%-d %b"));
            }
        }
    }
    dir.join(format!("{name}.html"))
}

/// A slide as a JPEG data URI (quality 80, subscripts still legible), or None if the file is gone.
pub fn slide_uri(path: &Path) -> Option<String> {
    if !path.exists() {
        return None;
    }
    let jpeg = std::env::temp_dir().join(format!("lecturelive-slide-{}.jpg", uuid::Uuid::new_v4()));
    let converted = Command::new("sips").args(["-s", "format", "jpeg", "-s", "formatOptions", &EMBED_JPEG_QUALITY.to_string()]).arg(path).arg("--out").arg(&jpeg).output().is_ok_and(|o| o.status.success());
    let (data, mime) = match std::fs::read(&jpeg).ok().filter(|_| converted) {
        Some(b) => (b, "image/jpeg"),
        None => (std::fs::read(path).ok()?, "image/png"),
    };
    let _ = std::fs::remove_file(&jpeg);
    Some(format!("data:{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(data)))
}

pub fn load_cache(path: &Path, key: &str) -> Option<(Fills, usize)> {
    let v: Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    if v["source"].as_str() != Some(key) {
        return None;
    }
    Some((Fills::from_json(&v["fills"])?, v["words"].as_u64()? as usize))
}

pub fn save_cache(path: &Path, key: &str, fills: &Fills, words: usize, budget: u32) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    write_atomic(path, dumps(&json!({"source": key, "fills": fills.to_json(), "words": words, "budget": budget})).as_bytes())
}

/// The page's HTML, written atomically beside the notes; slides are read fresh and embedded.
pub fn render(fills: &Fills, course: &str, notes: &Path, lecture_name: &str, template: &str) -> Result<PathBuf> {
    let (week, title) = match lecture_name.split_once(" — ") {
        Some((w, t)) if !t.is_empty() => (w, t),
        _ => ("", lecture_name),
    };
    let dir = notes.parent().unwrap_or(Path::new("."));
    let stem = notes.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let date = STAMP.find(&stem).and_then(|m| NaiveDate::parse_from_str(m.as_str(), "%Y%m%d").ok()).map(|d| d.format("%-d %B %Y").to_string()).unwrap_or_default();
    let paths: Vec<(String, String)> = match serde_json::from_str::<Value>(&fills.slides) {
        Ok(Value::Object(m)) => m.into_iter().filter_map(|(n, p)| p.as_str().map(|p| (n, p.to_string()))).collect(),
        _ => Vec::new(),
    };
    let mut uris: HashMap<String, String> = HashMap::new();
    for (_, p) in &paths {
        if !uris.contains_key(p) {
            uris.insert(p.clone(), slide_uri(&dir.join(p)).unwrap_or_else(|| p.clone()));
        }
    }
    let slides: Map<String, Value> = paths.iter().map(|(n, p)| (n.clone(), json!(uris[p]))).collect();
    let content = SRC.replace_all(&fills.content, |c: &Captures| match uris.get(&unescape(&c[1])) {
        Some(u) => format!("src=\"{u}\""),
        None => c[0].to_string(),
    });
    let values: HashMap<&str, String> = [
        ("keystone", fills.keystone.clone()),
        ("summary", fills.summary.clone()),
        ("content", content.into_owned()),
        ("slides", dumps(&Value::Object(slides))),
        ("week", escape(week)),
        ("title", escape(title)),
        ("course", escape(course)),
        ("date", date),
    ]
    .into();
    let path = page_path(notes, lecture_name);
    write_atomic(&path, fill(template, &values).as_bytes()).with_context(|| format!("write {}", path.display()))?;
    Ok(path)
}

/// One request with one retry: a failed request would otherwise cost the whole page.
async fn ask(chat: &ChatClient, system: String, content: Content) -> Result<String> {
    let req = ChatRequest { what: SpendKind::Page, system, content, effort: Some(EFFORT), timeout: TIMEOUT };
    let answer = match chat.complete(&req, &mut |_| {}).await {
        Ok(a) => a,
        Err(ChatError::Failed(_)) => chat.complete(&req, &mut |_| {}).await.map_err(|e| anyhow!("{e}"))?,
        Err(e) => bail!("{e}"),
    };
    Ok(strip_unsafe(&clean_output(&answer.text, false)))
}

#[derive(Debug, Clone, PartialEq)]
pub struct PageOutcome {
    pub path: PathBuf,
    pub words: usize,
    pub budget: u32,
    /// Filled from the cache, without a request.
    pub cached: bool,
    pub missing: Vec<&'static str>,
}

/// The CLI's `render_html`: typeset (or take the cached parts), then fill the template.
pub async fn make_page(chat: &ChatClient, course: &str, notes: &Path, cache: &Path, lecture_name: &str, template: &str, progress: &(dyn Fn(String) + Send + Sync)) -> Result<PageOutcome> {
    let doc = std::fs::read_to_string(notes).with_context(|| format!("read {}", notes.display()))?;
    let budget = page_budget(&doc);
    let embeds: Vec<(String, String)> = SLIDE_EMBED.captures_iter(&doc).map(|c| (c[1].to_string(), c[2].to_string())).collect();
    let system = page_system(course, budget, max_slides(embeds.len()));
    let key = sha256_hex(format!("{system}{doc}").as_bytes());
    let (fills, words, cached) = match load_cache(cache, &key) {
        Some((fills, words)) => (fills, words, true),
        None => {
            let words_in = notes_words(&doc);
            let numbers: Vec<String> = embeds.iter().map(|(n, _)| n.clone()).collect();
            let dir = notes.parent().unwrap_or(Path::new("."));
            let images = embeds.iter().map(|(_, p)| Image::read(&dir.join(p))).collect::<Result<Vec<_>>>()?;
            progress(format!("distilling {words_in} words into at most {budget}"));
            let mut out = ask(chat, system.clone(), Content::Parts { text: page_user(&doc, words_in, &numbers, budget), images }).await?;
            let mut words = visible_words(&out);
            if words as f64 > budget as f64 * 1.1 {
                progress(format!("cutting the draft from {words} words to {budget}"));
                out = ask(chat, revise_system(budget), Content::Text(revise_user(&out, words, budget))).await?;
                words = visible_words(&out);
            }
            let fills = cut(&out, &embeds);
            save_cache(cache, &key, &fills, words, budget)?;
            (fills, words, false)
        }
    };
    let path = render(&fills, course, notes, lecture_name, template)?;
    Ok(PageOutcome { path, words, budget, cached, missing: fills.missing() })
}
```

Write the fixture `lecture_notes_20260925.md` (synthetic, as described under Files).

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p lecturelive-core --lib notes::page` and `cargo test -p lecturelive-core --test notes_page`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/notes/mod.rs crates/core/src/notes/page.rs crates/core/tests/notes_page.rs crates/core/tests/support/mod.rs crates/core/tests/support/fake_sse.rs crates/core/tests/support/slides.rs crates/core/tests/fixtures/notes/lecture/lecture_notes_20260925.md
git commit -m "Distil the notes into the study page within its word budget, cached against prompt and notes

One medium-effort request over the whole notes and slides, one revision when the draft is
more than 10% over, sections re-cut at every <h2>, unsafe markup stripped, and the parts
cached in the Python CLI's own file, so an unchanged lecture re-renders without requests.
notes_template.html is embedded unchanged and filled in one pass."
```

---

### Task 11: The sidecar store, the stop sequence, and the whole lecture (milestone task 9, core)

**Files:**
- Modify: `crates/core/src/session/coordinator.rs` (`Store`, `spawn_with_store`, store jobs in both loops, second stop, cutoff fix, `Notification::SourceEnded`)
- Create: `crates/core/src/session/lecture.rs`; `session/mod.rs` gains `pub mod lecture;`
- Create: `crates/core/tests/lecture_gate.rs`; `crates/core/tests/support/sources.rs` gains `Talking`

**Interfaces:**
- Consumes: everything above.
- Produces:
  - `coordinator::Store` (Clone): `offline(Sidecar, PathBuf) -> Store`, `async read(&self) -> Result<Sidecar>`, `async update<T: Send + 'static>(&self, impl FnOnce(&mut Sidecar) -> Result<T> + Send + 'static) -> Result<T>`, `async cutoff(&self) -> Result<Option<CutoffResult>>` (None offline).
  - `coordinator::spawn_with_store(SessionConfig, Box<dyn Source>) -> (SessionHandle, mpsc::Receiver<Notification>, Store)`; `spawn` keeps its signature and drops its store.
  - `Notification::SourceEnded`: the audio has ended, and the session is draining.
  - `lecture::{Lecture { files, course, name, title, chat: ChatClient, spend: Spend }, Op::{Snapshot(String), Polish}, Command::{Op(Op), Stop}, Event, SlideWatch { screenshots: Option<PathBuf>, poll: Duration }, run(Arc<Lecture>, SessionConfig, Box<dyn Source>, SlideWatch, UnboundedReceiver<Command>, UnboundedSender<Event>) -> Result<StopReport>}`, with methods `Lecture::{snapshot(&self, &Store, hint: &str, &UnboundedSender<Event>) -> Result<(), String>, polish(&self, &Store, &UnboundedSender<Event>) -> bool, page(&self, &UnboundedSender<Event>) -> Result<PageOutcome, String>}`.
  - `Event::{Session(Notification), Busy(String), Preview(String), NothingNew, Committed { words: usize, slides: usize, block: String, usd: f64, confirmed: bool, removed: usize, missing: usize }, SnapshotFailed(String), Polished { backup: PathBuf, usd: f64 }, PolishStopped(String), PolishFailed(String), Page { outcome: PageOutcome, usd: f64 }, PageFailed(String), Slide { index: u32, file: String }, Warning(String)}`.

How the pieces hold together:
- **One writer of the sidecar.** While a session runs, `Store::update` sends the closure to the coordinator. The coordinator runs it on a blocking thread against a copy of its sidecar, adopts the copy and saves. Offline, the closure runs against a sidecar behind a mutex and is saved at once. `notesfile::commit` and `replace` save inside the closure where their ordering needs it; the save after it is idempotent.
- **A session serves its stores until they are dropped.** After the audio ends, the coordinator keeps answering store jobs, and cutoffs (unconfirmed), until every `Store` clone is gone. A snapshot in flight at stop therefore still commits through the one writer. `spawn` drops its store at once, so M1–M2 callers are unchanged.
- **Second stop.** A stop arriving after the audio has ended abandons recovery. The recovery receiver is dropped, the worker's next send fails and it ends, and the gaps stay unresolved for the next session (M2's deferred minor).
- **Cutoff fix.** Between recordings, a cutoff is confirmed only when no recording's live transcript is still open (`open_utterances` empty); while one is flushing, it is not.
- **The lecture.** `run` wires the session, a notes worker that runs operations one at a time (snapshot; polish, which runs a snapshot first and starts the page in the background when it succeeds), and the slide watcher. On the first stop, or when the audio ends by itself, the watcher stops and the operation queue closes (queued operations still run), and the session drains. A second stop hurries it. Then the session finishes, a page still being typeset is abandoned with a warning, as the CLI does, and a last snapshot runs offline on the sidecar the session left.

- [ ] **Step 1: Write the failing tests**

`coordinator.rs` tests (appended):

```rust
    #[tokio::test]
    async fn a_store_update_runs_in_the_coordinator_and_later_saves_keep_it() {
        let dir = tempfile::tempdir().unwrap();
        let a = Uuid::new_v4();
        let go = Arc::new(AtomicBool::new(false));
        let mut before = recording(a, 4);
        let after = before.split_off(3);
        let (handle, _notes, store) = spawn_with_store(cfg(dir.path()), Box::new(Gated { before, go: go.clone(), after }));
        written(dir.path(), a, 3_200).await;
        let seen = store.update(|sc| {
            sc.notes.segment_cursor = 7;
            Ok(sc.recordings.len())
        })
        .await
        .unwrap();
        assert_eq!(seen, 1, "the update sees the coordinator's sidecar");
        assert_eq!(Sidecar::load(&sidecar_path(dir.path(), STEM)).unwrap().unwrap().notes.segment_cursor, 7);
        go.store(true, Ordering::Relaxed);
        drop(store);
        handle.finish().await.unwrap();
        let sc = Sidecar::load(&sidecar_path(dir.path(), STEM)).unwrap().unwrap();
        assert_eq!((sc.notes.segment_cursor, sc.recordings[0].state), (7, RecState::Finalized), "the coordinator's own saves keep it");
    }

    #[tokio::test]
    async fn the_session_serves_its_stores_until_they_are_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let a = Uuid::new_v4();
        let (handle, _notes, store) = spawn_with_store(cfg(dir.path()), Box::new(Script(recording(a, 3))));
        let finished = tokio::spawn(handle.finish());
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!finished.is_finished(), "a store still held keeps the session open");
        store.update(|sc| {
            sc.notes.slide_index = 4;
            Ok(())
        })
        .await
        .unwrap();
        assert_eq!(store.cutoff().await.unwrap(), Some(CutoffResult { confirmed: false, segments: 0 }), "after the audio nothing more is confirmed");
        drop(store);
        finished.await.unwrap().unwrap();
        let path = sidecar_path(dir.path(), STEM);
        assert_eq!(Sidecar::load(&path).unwrap().unwrap().notes.slide_index, 4);
        let offline = Store::offline(Sidecar::load(&path).unwrap().unwrap(), path.clone());
        offline.update(|sc| {
            sc.notes.slide_index = 5;
            Ok(())
        })
        .await
        .unwrap();
        assert_eq!(offline.cutoff().await.unwrap(), None);
        assert_eq!(Sidecar::load(&path).unwrap().unwrap().notes.slide_index, 5);
    }

    #[tokio::test]
    async fn a_cutoff_while_a_recordings_transcript_is_still_flushing_is_not_confirmed() {
        let dir = tempfile::tempdir().unwrap();
        let (input, mut rx) = mpsc::channel(INPUT_QUEUE);
        let (tx, events) = mpsc::channel(8);
        let (flushed_tx, flushed_rx) = oneshot::channel::<()>();
        let ended = Arc::new(AtomicBool::new(false));
        let seen_end = ended.clone();
        tokio::spawn(async move {
            let mut flushed = Some(flushed_rx);
            while let Some(i) = rx.recv().await {
                if let SttInput::End { recording_id, .. } = i {
                    seen_end.store(true, Ordering::SeqCst);
                    if let Some(f) = flushed.take() {
                        let _ = f.await; // still flushing this recording's last sentence
                    }
                    let _ = tx.send(SttEvent::Ended { recording_id, gap: None }).await;
                }
            }
        });
        let a = Uuid::new_v4();
        let go = Arc::new(AtomicBool::new(false));
        let (handle, _notes) = spawn(SessionConfig { stt: Some(SttLink { input, events }), ..cfg(dir.path()) }, Box::new(Gated { before: recording(a, 3), go: go.clone(), after: vec![] }));
        while !ended.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        assert!(!handle.cutoff().await.unwrap().confirmed, "the last sentence may still arrive");
        flushed_tx.send(()).unwrap();
        while !Sidecar::load(&sidecar_path(dir.path(), STEM)).unwrap().unwrap().open_utterances.is_empty() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        assert!(handle.cutoff().await.unwrap().confirmed, "between recordings with nothing flushing, all is settled");
        go.store(true, Ordering::Relaxed);
        handle.finish().await.unwrap();
    }

    #[tokio::test]
    async fn a_second_stop_abandons_pending_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let old = Uuid::new_v4();
        let anchor = Local.with_ymd_and_hms(2026, 9, 25, 9, 0, 0).unwrap();
        let mut sc = Sidecar::default();
        sc.recordings.push(RecordingEntry { id: old, file: "recordings/old.wav".into(), anchor, source_uid: "X".into(), input_rate: 48_000, samples: Some(16_000), state: RecState::Finalized });
        sc.gaps.push(Gap::new(old, 0, Some(16_000), GapKind::SttOffline));
        sc.save(&sidecar_path(dir.path(), STEM)).unwrap();
        let (jobs, mut jobs_rx) = mpsc::unbounded_channel::<RecoverJob>();
        let (events_tx, events) = mpsc::channel::<RecoverEvent>(8);
        tokio::spawn(async move {
            let _hold = events_tx;
            while jobs_rx.recv().await.is_some() {
                std::future::pending::<()>().await; // a long gap that never finishes
            }
        });
        let (handle, _notes) = spawn(SessionConfig { recovery: Some(RecoveryLink { jobs, events }), ..cfg(dir.path()) }, Box::new(Endless));
        tokio::time::sleep(Duration::from_millis(30)).await;
        handle.request_stop();
        loop {
            let sc = Sidecar::load(&sidecar_path(dir.path(), STEM)).unwrap().unwrap();
            if sc.recordings.len() == 2 && sc.recordings.iter().all(|r| r.state == RecState::Finalized) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        handle.request_stop();
        let report = tokio::time::timeout(Duration::from_secs(5), handle.finish()).await.expect("the second stop ends the wait").unwrap();
        assert_eq!(report.unresolved, 1, "the gap waits for the next session");
        assert_eq!(report.recordings.len(), 1, "this session's recording is intact");
    }
```

`crates/core/tests/lecture_gate.rs`:

```rust
//! The M3 gate (docs/milestones.md): a whole lecture headless, from first word to study page, against
//! fake STT, REST and chat endpoints; failed and truncated answers leave the notes untouched; a legacy
//! folder migrates and its pending lines reach the next snapshot. No network.
mod support;

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Local;
use lecturelive_core::notes::chat::{ChatClient, ChatConfig};
use lecturelive_core::notes::embeds::embeds_in;
use lecturelive_core::session::coordinator::{SessionConfig, Store};
use lecturelive_core::session::files::LectureFiles;
use lecturelive_core::session::folder::{self, How};
use lecturelive_core::session::lecture::{self, Command, Event, Lecture, Op, SlideWatch};
use lecturelive_core::session::segments::{self, NewSegment, SegmentLog, SegmentSource};
use lecturelive_core::session::sidecar::Sidecar;
use lecturelive_core::session::spend::{self, Spend};
use lecturelive_core::stt::rest::{spawn_recovery, RestClient, RestConfig};
use lecturelive_core::stt::stream::{self, SttConfig};
use serde_json::Value;
use support::fake_sse::{self, Reply};
use support::sources::Talking;
use support::{fake_rest, fake_stt};
use tokio::sync::mpsc;

const TITLE: &str = "# Machine Learning — Week 01 — Optimisation — 2026-09-25";
const NAME: &str = "Week 01 — Optimisation";

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn files(dir: &Path) -> LectureFiles {
    LectureFiles::standard(dir, chrono::NaiveDate::from_ymd_opt(2026, 9, 25).unwrap())
}

fn user_text(body: &Value) -> String {
    let c = &body["messages"][1]["content"];
    c.as_str().map(str::to_string).unwrap_or_else(|| c[0]["text"].as_str().unwrap_or_default().to_string())
}

/// The model, played by the fake: notes that place every embed, a polish that keeps them, a page within budget.
fn respond(body: &Value) -> Reply {
    let system = body["messages"][0]["content"].as_str().unwrap_or_default();
    let user = user_text(body);
    if system.starts_with("You are the note-taker") {
        let embeds: Vec<&str> = user.lines().filter(|l| l.starts_with("![Slide ")).collect();
        fake_sse::answer(&format!("## Gradient descent\n- Steps against the gradient.\n{}", embeds.join("\n")), 1_000_000)
    } else if system.starts_with("You turn raw") {
        let title = user.lines().next().unwrap_or_default().trim_start_matches("Use this exact title line: ").to_string();
        let doc = user.split("Notes as written during the lecture:\n<<<\n").nth(1).and_then(|s| s.split("\n>>>").next()).unwrap_or_default();
        fake_sse::answer(&format!("{title}\n\nThe lecture covered gradient descent.\n\n## Gradient descent\n- Steps against the gradient.\n{}\n\n## Key takeaways\n- Scale the rate.", embeds_in(doc).join("\n")), 2_000_000)
    } else {
        fake_sse::answer(&fake_sse::study_page(300), 3_000_000)
    }
}

fn chat(url: &str, ledger: &Path) -> ChatClient {
    let spend = Spend::open(ledger, "Machine Learning", NAME, chrono::NaiveDate::from_ymd_opt(2026, 9, 25).unwrap()).unwrap();
    ChatClient::new(ChatConfig { url: url.into(), idle_timeout: ms(5_000), ..ChatConfig::new("test-key".into()) }, Some(spend)).unwrap()
}

fn lecture_for(files: &LectureFiles, url: &str, ledger: &Path) -> Lecture {
    let spend = Spend::open(ledger, "Machine Learning", NAME, files.date).unwrap();
    Lecture { files: files.clone(), course: "Machine Learning".into(), name: NAME.into(), title: TITLE.into(), chat: chat(url, ledger), spend }
}

/// Waits for an event matching `want`, panicking on a failure event or after 30 s.
async fn until(events: &mut mpsc::UnboundedReceiver<Event>, what: &str, want: impl Fn(&Event) -> bool) -> Event {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let e = tokio::time::timeout_at(deadline, events.recv()).await.unwrap_or_else(|_| panic!("no {what} within 30 s")).expect("the lecture is running");
        if let Event::SnapshotFailed(m) | Event::PolishFailed(m) | Event::PolishStopped(m) | Event::PageFailed(m) = &e {
            panic!("waiting for {what}: {m}");
        }
        if want(&e) {
            return e;
        }
    }
}

async fn segments_seen(events: &mut mpsc::UnboundedReceiver<Event>, n: usize) {
    for _ in 0..n {
        until(events, "a segment", |e| matches!(e, Event::Session(lecturelive_core::session::coordinator::Notification::Segment(_)))).await;
    }
}

#[tokio::test]
async fn a_whole_lecture_runs_headless_from_first_word_to_study_page() {
    let dir = tempfile::tempdir().unwrap();
    let f = files(dir.path());
    let ledger = dir.path().join("spend.jsonl");
    let (_, report) = folder::open(&f, TITLE, false).unwrap();
    assert_eq!(report.how, How::Created);
    segments::session_marker(&f.transcript, Local::now()).unwrap();

    let stt = fake_stt::start(fake_stt::Config::default()).await;
    let rest = fake_rest::start(None).await;
    let sse = fake_sse::start(respond).await;
    let stt_cfg = SttConfig { url: stt.url.clone(), backoff_unit: ms(1), connect_timeout: ms(2_000), send_timeout: ms(2_000), idle_timeout: ms(2_000), finalize_wait: ms(2_000), done_wait: ms(2_000), ..SttConfig::new("test-key".into(), vec![]) };
    let rest_cfg = RestConfig { url: rest.url.clone(), retry_unit: ms(1), request_timeout: ms(5_000), file_wait: ms(5_000), ..RestConfig::new("test-key".into(), vec![]) };
    let lec = Arc::new(lecture_for(&f, &sse.url, &ledger));
    let session = SessionConfig {
        dir: f.dir.clone(),
        stem: f.stem.clone(),
        stt: Some(stream::spawn(stt_cfg).unwrap()),
        recovery: Some(spawn_recovery(RestClient::new(rest_cfg).unwrap())),
        spend: Some(lec.spend.clone()),
        ..Default::default()
    };
    let (cmd, cmd_rx) = mpsc::unbounded_channel();
    let (ev_tx, mut ev) = mpsc::unbounded_channel();
    let source = Talking { pace: ms(2), fake: stt.state.clone() };
    let run = tokio::spawn(lecture::run(lec.clone(), session, Box::new(source), SlideWatch { screenshots: None, poll: ms(20) }, cmd_rx, ev_tx));

    segments_seen(&mut ev, 2).await;
    support::slides::png(&f.slides.join("Screenshot dropped in.png"));
    until(&mut ev, "the slide", |e| matches!(e, Event::Slide { index: 1, .. })).await;
    cmd.send(Command::Op(Op::Snapshot(String::new()))).unwrap();
    until(&mut ev, "the first snapshot", |e| matches!(e, Event::Committed { slides: 1, .. })).await;
    segments_seen(&mut ev, 1).await;
    cmd.send(Command::Op(Op::Snapshot("focus on momentum".into()))).unwrap();
    until(&mut ev, "the hinted snapshot", |e| matches!(e, Event::Committed { .. })).await;
    cmd.send(Command::Op(Op::Polish)).unwrap();
    until(&mut ev, "the polish", |e| matches!(e, Event::Polished { .. })).await;
    let page = until(&mut ev, "the page", |e| matches!(e, Event::Page { .. })).await;
    cmd.send(Command::Stop).unwrap();
    let report = tokio::time::timeout(Duration::from_secs(30), run).await.expect("the lecture stops").unwrap().unwrap();

    assert_eq!(report.unresolved, 0);
    let notes = std::fs::read_to_string(&f.notes).unwrap();
    assert!(notes.starts_with(TITLE), "{notes}");
    assert_eq!(notes.matches("![Slide 1](slides/slide_01_").count(), 1, "{notes}");
    let backups: Vec<String> = std::fs::read_dir(f.state_dir()).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.starts_with("lecture_notes_20260925_") && n.ends_with(".md")).collect();
    assert_eq!(backups.len(), 1);
    let raw = std::fs::read_to_string(f.state_dir().join(&backups[0])).unwrap();
    assert!(raw.matches("<!-- ").count() >= 2, "the two snapshots, and the one polish takes first when speech arrived since: {raw}");
    let sc = Sidecar::load(&f.sidecar()).unwrap().unwrap();
    assert_eq!(sc.notes.segment_cursor, segments::read(&f.segments()).unwrap().len() as u64, "the last snapshot took everything");
    assert_eq!(sc.notes.slide_index, 1);
    assert!(!f.journal().exists());
    assert!(std::fs::read_to_string(&f.transcript).unwrap().starts_with("--- started "));
    let Event::Page { outcome, .. } = page else { unreachable!() };
    assert_eq!(outcome.path, dir.path().join("Optimisation.html"));
    assert!(std::fs::read_to_string(&outcome.path).unwrap().contains("<title>Optimisation — Machine Learning</title>"));
    let kinds: Vec<String> = spend::read(&ledger).unwrap().into_iter().map(|e| e.what).collect();
    for (kind, at_least) in [("transcribe", 1), ("notes", 2), ("polish", 1), ("page", 1)] {
        assert!(kinds.iter().filter(|k| *k == kind).count() >= at_least, "{kind}: {kinds:?}");
    }
    let bodies = sse.state.bodies();
    assert!(bodies.iter().any(|b| user_text(b).contains("Focus hint from the student: focus on momentum")));
    let first = &bodies[0]["messages"][1]["content"];
    assert!(first[1]["image_url"]["url"].as_str().unwrap().starts_with("data:image/png;base64,"), "the slide goes with its snapshot");
    assert!(user_text(&bodies[0]).contains(">>> Slide 1 shown (embed: ![Slide 1](slides/slide_01_"));
}

/// A folder with two logged segments and a notes file, for snapshots without a session.
fn offline_folder(dir: &Path) -> (LectureFiles, Store) {
    let f = files(dir);
    folder::open(&f, TITLE, false).unwrap();
    let mut log = SegmentLog::open(dir, &f.stem).unwrap();
    let anchor = chrono::TimeZone::with_ymd_and_hms(&Local, 2026, 9, 25, 10, 0, 0).unwrap();
    for (k, text) in ["Gradient descent steps downhill.", "The rate sets the step."].iter().enumerate() {
        log.append(NewSegment { recording_id: uuid::Uuid::nil(), start_sample: k as u64 * 32_000, end_sample: k as u64 * 32_000 + 16_000, text: text.to_string(), words: vec![], source: SegmentSource::Live }, anchor).unwrap();
    }
    (f.clone(), Store::offline(Sidecar::load(&f.sidecar()).unwrap().unwrap(), f.sidecar()))
}

#[tokio::test]
async fn failed_empty_and_truncated_answers_leave_the_notes_untouched_and_the_batch_pending() {
    let dir = tempfile::tempdir().unwrap();
    let (f, store) = offline_folder(dir.path());
    let replies = Arc::new(Mutex::new(vec![
        Reply::Stream(fake_sse::pieces(fake_sse::fixture("stream_usage.sse")[..9_000].to_vec(), 64)), // cut off mid-answer
        Reply::Stream(vec![b"data: [DONE]\n\n".to_vec()]),                                            // empty
        Reply::Stream(fake_sse::pieces(fake_sse::fixture("stream_length.sse"), 64)),                  // truncated by length
    ]));
    let queue = replies.clone();
    let sse = fake_sse::start(move |b| queue.lock().unwrap().pop().map_or_else(|| respond(b), |r| r)).await;
    let lec = lecture_for(&f, &sse.url, &dir.path().join("spend.jsonl"));
    let (ev_tx, _ev) = mpsc::unbounded_channel();
    let untouched = std::fs::read(&f.notes).unwrap();
    for _ in 0..3 {
        assert!(lec.snapshot(&store, "", &ev_tx).await.is_err());
        assert_eq!(std::fs::read(&f.notes).unwrap(), untouched);
        assert_eq!(Sidecar::load(&f.sidecar()).unwrap().unwrap().notes.segment_cursor, 0, "the batch stays pending");
    }
    assert!(!f.journal().exists(), "a failed answer never starts a commit");
    lec.snapshot(&store, "", &ev_tx).await.unwrap();
    let notes = std::fs::read_to_string(&f.notes).unwrap();
    assert_eq!(notes.matches("<!-- ").count(), 1);
    assert_eq!(Sidecar::load(&f.sidecar()).unwrap().unwrap().notes.segment_cursor, 2);
    assert!(user_text(&sse.state.bodies()[3]).contains("Gradient descent steps downhill."), "the pending material went again");

    // A polish whose snapshot fails stops before its own request.
    SegmentLog::open(dir.path(), &f.stem).unwrap().append(NewSegment { recording_id: uuid::Uuid::nil(), start_sample: 96_000, end_sample: 112_000, text: "Momentum.".into(), words: vec![], source: SegmentSource::Live }, Local::now()).unwrap();
    replies.lock().unwrap().push(Reply::Status(503, "{}".into()));
    let (ev_tx, mut ev) = mpsc::unbounded_channel();
    assert!(!lec.polish(&store, &ev_tx).await);
    assert!(matches!(ev.recv().await, Some(Event::PolishStopped(_)) | Some(Event::Busy(_))));
    assert!(sse.state.bodies().iter().all(|b| !b["messages"][0]["content"].as_str().unwrap().starts_with("You turn raw")));
    assert_eq!(std::fs::read_to_string(&f.notes).unwrap(), notes);
}

#[tokio::test]
async fn a_legacy_folder_migrates_and_its_pending_lines_reach_the_next_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let f = files(dir.path());
    std::fs::create_dir_all(f.state_dir()).unwrap();
    std::fs::write(&f.notes, format!("{TITLE}\n\n<!-- 10:00:12 -->\n## Intro\n- one, two\n")).unwrap();
    let noted = "--- started 10:00:00 ---\n[10:00:05] one\n[10:00:09] two\n";
    std::fs::write(&f.transcript, format!("{noted}--- resumed 10:20:00 ---\n[10:20:03] three\n")).unwrap();
    std::fs::write(f.legacy_state(), serde_json::json!({"transcript_offset": noted.len(), "slide_index": 0}).to_string()).unwrap();
    let (sc, report) = folder::open(&f, TITLE, false).unwrap();
    assert_eq!((report.how, report.pending_segments), (How::Migrated, 1));
    let sse = fake_sse::start(respond).await;
    let lec = lecture_for(&f, &sse.url, &dir.path().join("spend.jsonl"));
    let (ev_tx, _ev) = mpsc::unbounded_channel();
    lec.snapshot(&Store::offline(sc, f.sidecar()), "", &ev_tx).await.unwrap();
    let sent = user_text(&sse.state.bodies()[0]);
    assert!(sent.contains("[10:20:03] three") && !sent.contains("[10:00:05] one"), "{sent}");
    assert!(sent.contains("## Intro\n- one, two"), "the notes so far go with it");
    assert_eq!(Sidecar::load(&f.sidecar()).unwrap().unwrap().notes.segment_cursor, 3);
}
```

`support/sources.rs` gains:

```rust
/// Synthetic speech, paced, until the session stops it: a lecture of no fixed length.
pub struct Talking {
    pub pace: Duration,
    pub fake: Arc<State>,
}

impl Source for Talking {
    fn run(self: Box<Self>, out: Sender<SourceEvent>, stop: Arc<AtomicBool>) {
        let id = Uuid::new_v4();
        out.blocking_send(begin(id)).unwrap();
        let mut k = 0;
        while !stop.load(SeqCst) {
            let f = Frame { recording_id: id, sample_offset: k * 1600, valid_samples: 1600, pcm16: speech::frame_pcm(k) };
            if out.blocking_send(SourceEvent::Frame(f)).is_err() {
                return;
            }
            self.fake.feed.store((k + 1) * 1600, SeqCst);
            k += 1;
            std::thread::sleep(self.pace);
        }
        let _ = out.blocking_send(SourceEvent::End { recording_id: id, samples: k * 1600, stream_errors: 0 });
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p lecturelive-core --lib coordinator` and `cargo test -p lecturelive-core --test lecture_gate`
Expected: FAIL to compile.

- [ ] **Step 3: Implement**

`coordinator.rs`:

```rust
type StoreJob = Box<dyn FnOnce(&mut Sidecar) + Send>;

/// The sidecar's one writer, however it is reached (spec §3.2): the coordinator while a session runs,
/// the file itself when none does.
#[derive(Clone)]
pub struct Store(StoreInner);

#[derive(Clone)]
enum StoreInner {
    Session { jobs: mpsc::Sender<StoreJob>, cmd: mpsc::Sender<Command> },
    Offline { sidecar: Arc<std::sync::Mutex<Sidecar>>, path: PathBuf },
}

impl Store {
    pub fn offline(sidecar: Sidecar, path: PathBuf) -> Self {
        Self(StoreInner::Offline { sidecar: Arc::new(std::sync::Mutex::new(sidecar)), path })
    }

    /// Runs `f` on the sidecar and saves it; in a session, inside the coordinator.
    pub async fn update<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Sidecar) -> Result<T> + Send + 'static,
    {
        match &self.0 {
            StoreInner::Session { jobs, .. } => {
                let (tx, rx) = oneshot::channel();
                let job: StoreJob = Box::new(move |sc| {
                    let _ = tx.send(f(sc));
                });
                jobs.send(job).await.map_err(|_| anyhow!("the session has ended"))?;
                rx.await.map_err(|_| anyhow!("the session ended before the update ran"))?
            }
            StoreInner::Offline { sidecar, path } => {
                let mut sc = sidecar.lock().expect("the sidecar lock");
                let out = f(&mut sc);
                sc.save(path)?;
                out
            }
        }
    }

    pub async fn read(&self) -> Result<Sidecar> {
        self.update(|sc| Ok(sc.clone())).await
    }

    /// A snapshot cutoff (spec §5.3); None without a session.
    pub async fn cutoff(&self) -> Result<Option<CutoffResult>> {
        let StoreInner::Session { cmd, .. } = &self.0 else { return Ok(None) };
        let (tx, rx) = oneshot::channel();
        cmd.send(Command::Cutoff(tx)).await.map_err(|_| anyhow!("the session has ended"))?;
        Ok(Some(rx.await.map_err(|_| anyhow!("the session ended before the cutoff settled"))?))
    }
}
```

`spawn_with_store` creates `(store_tx, store_rx) = mpsc::channel(8)` and passes `Some(store_rx)` to `run`, returning `Store(StoreInner::Session { jobs: store_tx, cmd: cmd_tx.clone() })`. `spawn` calls it and drops the store. `SessionHandle::finish` destructures itself and drops `cmd` before awaiting the task.

`run` gains `mut store_rx: Option<mpsc::Receiver<StoreJob>>`. The main loop gains:

```rust
            job = recv(&mut store_rx), if store_rx.is_some() => match job {
                Some(job) => if let Err(e) = c.run_job(job).await { fail(e, &mut failure) },
                None => store_rx = None,
            },
```

After the main loop, `c.notify(Notification::SourceEnded)`. The drain loop becomes:

```rust
    while recorder_open || c.stt_events.is_some() || c.recovery_events.is_some() || store_rx.is_some() {
        if c.stt_events.is_none() {
            c.recovery = None;
        }
        tokio::select! {
            ev = rev_rx.recv(), if recorder_open => match ev { … as before … },
            ev = recv(&mut c.stt_events), if c.stt_events.is_some() => match ev { … as before … },
            ev = recv(&mut c.recovery_events), if c.recovery_events.is_some() => match ev { … as before … },
            job = recv(&mut store_rx), if store_rx.is_some() => match job {
                Some(job) => if let Err(e) = c.run_job(job).await { failure.get_or_insert(e); },
                None => store_rx = None,
            },
            Some(cmd) = cmd_rx.recv() => match cmd {
                // A stop while draining: stop waiting for recovery; its gaps wait for the next session.
                Command::Stop => {
                    if c.recovery_events.take().is_some() {
                        c.recovery = None;
                        c.notify(Notification::RecoveryFailed("recovery stopped by a second stop; its gaps wait for the next session".into()));
                    }
                }
                Command::Cutoff(reply) => c.cutoff(reply),
            },
        }
    }
```

`Coordinator::run_job`:

```rust
    /// A store update (spec §3.2): run against a copy on a blocking thread, then adopted and saved.
    async fn run_job(&mut self, job: StoreJob) -> Result<()> {
        let sc = self.sidecar.clone();
        self.sidecar = tokio::task::spawn_blocking(move || {
            let mut sc = sc;
            job(&mut sc);
            sc
        })
        .await?;
        self.save().await
    }
```

`Coordinator::cutoff`'s early return:

```rust
        let Some(recording_id) = self.current.filter(|_| self.stt.is_some()) else {
            // Without live STT nothing is confirmed. Between recordings, one whose live transcript is
            // still being flushed (its mark is still set) may yet commit its last sentence.
            let settled = self.stt.is_some() && self.sidecar.open_utterances.is_empty();
            let _ = reply.send(CutoffResult { confirmed: settled, segments });
            return;
        };
```

`Notification::SourceEnded` gains an arm in `record`'s `match` in `main.rs` (`=> {}`).

`crates/core/src/session/lecture.rs`:

```rust
//! A whole lecture (spec §3.2, §5.4, §6, §8): the session, a notes worker taking snapshots and polishing
//! one at a time, a slide watcher, and the stop sequence that ends with a last snapshot. The CLI's
//! `lecture` command and the app drive it through `Command`s and read its `Event`s.
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use chrono::{DateTime, Local};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;

use crate::audio::source::Source;
use crate::notes::chat::{self, ChatClient, ChatRequest, Content, Image};
use crate::notes::embeds::{clean_output, repair};
use crate::notes::page::{self, PageOutcome};
use crate::notes::timeline::{embed_line, timeline, Batch};
use crate::notes::{context, polish, prompts};
use crate::session::coordinator::{self, Notification, SessionConfig, StopReport, Store};
use crate::session::files::LectureFiles;
use crate::session::folder::{next_slide_index, relative};
use crate::session::notesfile::{self, sha256_hex};
use crate::session::segments;
use crate::session::sidecar::{Sidecar, SlideEntry};
use crate::session::spend::{Spend, SpendKind};

pub struct Lecture {
    pub files: LectureFiles,
    pub course: String,
    /// The lecture folder's name: the ledger's lecture and the page's title.
    pub name: String,
    /// The notes' title line.
    pub title: String,
    pub chat: ChatClient,
    pub spend: Spend,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    /// Enter, or a hint and Enter.
    Snapshot(String),
    Polish,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Op(Op),
    /// The first stops the lecture; a second stops waiting for recovery.
    Stop,
}

#[derive(Debug)]
pub enum Event {
    Session(Notification),
    Busy(String),
    Preview(String),
    NothingNew,
    Committed { words: usize, slides: usize, block: String, usd: f64, confirmed: bool, removed: usize, missing: usize },
    SnapshotFailed(String),
    Polished { backup: PathBuf, usd: f64 },
    PolishStopped(String),
    PolishFailed(String),
    Page { outcome: PageOutcome, usd: f64 },
    PageFailed(String),
    Slide { index: u32, file: String },
    Warning(String),
}

#[derive(Debug, Clone)]
pub struct SlideWatch {
    /// Where macOS saves screenshots; those taken after the lecture started are taken in as slides.
    pub screenshots: Option<PathBuf>,
    pub poll: Duration,
}

const NOTES_TIMEOUT: Duration = Duration::from_secs(600);
const IMAGE_EXTENSIONS: [&str; 3] = ["png", "jpg", "jpeg"];
const SLIDE_MAX_PX: u32 = 1600;

impl Lecture {
    /// Spec §6.1–6.2: cutoff, batch after the cursors, one streamed request, repair, journaled commit.
    pub async fn snapshot(&self, store: &Store, hint: &str, events: &UnboundedSender<Event>) -> Result<(), String> {
        let text = |e: anyhow::Error| format!("{e:#}");
        let at = Local::now();
        let cut = store.cutoff().await.map_err(text)?;
        let sc = store.read().await.map_err(text)?;
        let log = self.files.segments();
        let upto = match cut {
            Some(c) => c.segments,
            None => segments::read(&log).map_err(text)?.len() as u64,
        };
        let batch = Batch::take(&log, &sc, upto, hint).map_err(text)?;
        if batch.is_empty() {
            let _ = events.send(Event::NothingNew);
            return Ok(());
        }
        let doc = std::fs::read_to_string(&self.files.notes).map_err(|e| format!("read {}: {e}", self.files.notes.display()))?;
        let notes_dir = self.files.notes_dir();
        let embeds: Vec<String> = batch.slides.iter().map(|s| embed_line(s, &self.files.dir, notes_dir)).collect();
        let images = batch.slides.iter().map(|s| Image::read(&self.files.dir.join(&s.file))).collect::<Result<Vec<_>>>().map_err(text)?;
        let timeline = timeline(&batch.segments, &batch.slides, &self.files.dir, notes_dir);
        let system = prompts::notes_system(&self.course);
        let rest = context::text_tokens(&system) + context::text_tokens(&prompts::notes_user("", &timeline, &embeds, hint)) + images.len() * context::IMAGE_TOKENS;
        let doc_context = context::doc_context(&doc, rest, context::BUDGET_TOKENS);
        let req = ChatRequest { what: SpendKind::Notes, system, content: Content::Parts { text: prompts::notes_user(&doc_context, &timeline, &embeds, hint), images }, effort: None, timeout: NOTES_TIMEOUT };
        let _ = events.send(Event::Busy(format!("snapshot, {} words to {}", batch.words(), chat::MODEL)));
        let preview = events.clone();
        let answer = self.chat.complete(&req, &mut |d: &str| drop(preview.send(Event::Preview(d.to_string())))).await.map_err(|e| e.to_string())?;
        if let Some(w) = answer.warning {
            let _ = events.send(Event::Warning(w));
        }
        let repaired = repair(&clean_output(&answer.text, true), &embeds);
        let block = notesfile::block(at, &repaired.text);
        let (files, b, to, slide_to) = (self.files.clone(), block.clone(), batch.positions.end, batch.slide_to(sc.notes.slide_index));
        store.update(move |sc| notesfile::commit(&files, sc, &b, to, slide_to)).await.map_err(text)?;
        let _ = events.send(Event::Committed { words: batch.words(), slides: batch.slides.len(), block, usd: answer.usd.unwrap_or(0.0), confirmed: cut.is_none_or(|c| c.confirmed), removed: repaired.removed, missing: repaired.missing.len() });
        Ok(())
    }

    /// Spec §6.3: a snapshot first, and nothing if it fails; true when the notes were polished.
    pub async fn polish(&self, store: &Store, events: &UnboundedSender<Event>) -> bool {
        if let Err(e) = self.snapshot(store, "", events).await {
            let _ = events.send(Event::PolishStopped(format!("the snapshot before it failed ({e}); the notes are unchanged")));
            return false;
        }
        let doc = match std::fs::read_to_string(&self.files.notes) {
            Ok(d) => d,
            Err(e) => {
                let _ = events.send(Event::PolishFailed(format!("read {}: {e}", self.files.notes.display())));
                return false;
            }
        };
        let transcript = std::fs::read_to_string(&self.files.transcript).unwrap_or_default();
        let _ = events.send(Event::Busy(format!("polishing {} words", doc.split_whitespace().count())));
        let answer = match self.chat.complete(&polish::request(&self.course, &self.title, &doc, &transcript), &mut |_| {}).await {
            Ok(a) => a,
            Err(e) => {
                let _ = events.send(Event::PolishFailed(format!("{e}; the notes are unchanged")));
                return false;
            }
        };
        let text = polish::validate(&answer.text, &doc).text;
        let (files, based_on) = (self.files.clone(), sha256_hex(doc.as_bytes()));
        match store.update(move |sc| notesfile::replace(&files, sc, &text, &based_on, Local::now())).await {
            Ok(backup) => {
                let _ = events.send(Event::Polished { backup, usd: answer.usd.unwrap_or(0.0) });
                true
            }
            Err(e) => {
                let _ = events.send(Event::PolishFailed(format!("{e:#}")));
                false
            }
        }
    }

    /// Spec §6.4: the study page from the notes as they are, without polishing again.
    pub async fn page(&self, events: &UnboundedSender<Event>) -> Result<PageOutcome, String> {
        let before = self.spend.kind_total(SpendKind::Page);
        let progress = events.clone();
        let made = page::make_page(&self.chat, &self.course, &self.files.notes, &self.files.page_cache(), &self.name, page::TEMPLATE, &move |m| drop(progress.send(Event::Busy(m)))).await;
        match made {
            Ok(outcome) => {
                let _ = events.send(Event::Page { outcome: outcome.clone(), usd: self.spend.kind_total(SpendKind::Page) - before });
                Ok(outcome)
            }
            Err(e) => {
                let m = format!("{e:#}. The notes are unchanged; `lecture page` tries again.");
                let _ = events.send(Event::PageFailed(m.clone()));
                Err(m)
            }
        }
    }
}

async fn notes_worker(lec: Arc<Lecture>, store: Store, mut ops: UnboundedReceiver<Op>, events: UnboundedSender<Event>, pages: Arc<Mutex<Vec<JoinHandle<()>>>>) {
    while let Some(op) = ops.recv().await {
        match op {
            Op::Snapshot(hint) => {
                if let Err(e) = lec.snapshot(&store, &hint, &events).await {
                    let _ = events.send(Event::SnapshotFailed(format!("{e}; everything is kept for the next one")));
                }
            }
            Op::Polish => {
                if lec.polish(&store, &events).await {
                    // Typesetting takes minutes and reads only the polished file: snapshots are not held up by it.
                    let (l, ev) = (lec.clone(), events.clone());
                    pages.lock().expect("the page list").push(tokio::spawn(async move {
                        let _ = l.page(&ev).await;
                    }));
                }
            }
        }
    }
}

fn is_image(p: &Path) -> bool {
    p.extension().and_then(|e| e.to_str()).is_some_and(|e| IMAGE_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

fn listing(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir).map(|e| e.flatten().map(|e| e.path()).collect()).unwrap_or_default()
}

/// Moves a file, copying when it crosses file systems.
fn move_file(from: &Path, to: &Path) -> Result<()> {
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    std::fs::copy(from, to).with_context(|| format!("copy {} to {}", from.display(), to.display()))?;
    std::fs::remove_file(from).with_context(|| format!("remove {}", from.display()))
}

/// The CLI's `shrink_if_large`: `sips -Z` scales up as well as down, so it runs only when there is something to shrink.
fn shrink_if_large(path: &Path) {
    let Ok(out) = std::process::Command::new("sips").args(["-g", "pixelWidth", "-g", "pixelHeight"]).arg(path).output() else { return };
    let dims: Vec<u32> = String::from_utf8_lossy(&out.stdout).lines().filter(|l| l.contains("pixel")).filter_map(|l| l.split(':').nth(1)?.trim().parse().ok()).collect();
    if dims.iter().any(|&d| d > SLIDE_MAX_PX) {
        let _ = std::process::Command::new("sips").args(["-Z", &SLIDE_MAX_PX.to_string()]).arg(path).output();
    }
}

/// Registers an image as the next slide (spec §7.3, §8): renamed into `slides/` as the CLI names it,
/// timed by its file time, shrunk to 1600 px, inside the sidecar's writer so indexes never collide.
async fn register(files: &LectureFiles, store: &Store, p: &Path) -> Result<SlideEntry> {
    let shown_at: DateTime<Local> = std::fs::metadata(p)?.modified()?.into();
    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("png").to_ascii_lowercase();
    let (files, p) = (files.clone(), p.to_path_buf());
    store
        .update(move |sc| {
            let index = next_slide_index(&files, sc)?;
            let dest = files.slides.join(format!("slide_{index:02}_{}.{ext}", shown_at.format("%H%M%S")));
            if p != dest {
                move_file(&p, &dest)?;
            }
            shrink_if_large(&dest);
            let entry = SlideEntry { index, file: relative(&files, &dest), shown_at };
            sc.slides.push(entry.clone());
            Ok(entry)
        })
        .await
}

/// The CLI's `slide_watcher`: images dropped into `slides/`, and screenshots taken after the start,
/// once their size has stopped changing.
async fn slide_watcher(lec: Arc<Lecture>, store: Store, watch: SlideWatch, mut stop: tokio::sync::watch::Receiver<bool>, events: UnboundedSender<Event>) {
    let started = SystemTime::now();
    let mut known: HashSet<PathBuf> = listing(&lec.files.slides).into_iter().collect();
    let mut sizes: HashMap<PathBuf, u64> = HashMap::new();
    loop {
        let mut candidates = listing(&lec.files.slides);
        if let Some(dir) = &watch.screenshots {
            candidates.extend(listing(dir).into_iter().filter(|p| {
                p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("Screen")) && std::fs::metadata(p).and_then(|m| m.modified()).is_ok_and(|t| t >= started)
            }));
        }
        for p in candidates {
            if known.contains(&p) || !is_image(&p) {
                continue;
            }
            let Ok(size) = std::fs::metadata(&p).map(|m| m.len()) else { continue };
            if sizes.insert(p.clone(), size) != Some(size) {
                continue; // still being written
            }
            known.insert(p.clone());
            match register(&lec.files, &store, &p).await {
                Ok(s) => {
                    known.insert(lec.files.dir.join(&s.file));
                    let _ = events.send(Event::Slide { index: s.index, file: s.file });
                }
                Err(e) => {
                    let _ = events.send(Event::Warning(format!("slide {}: {e:#}", p.display())));
                }
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(watch.poll) => {}
            _ = stop.changed() => return,
        }
    }
}

/// A whole lecture: runs until stopped (or until its audio ends), then the last snapshot.
pub async fn run(lec: Arc<Lecture>, cfg: SessionConfig, source: Box<dyn Source>, watch: SlideWatch, mut commands: UnboundedReceiver<Command>, events: UnboundedSender<Event>) -> Result<StopReport> {
    let (handle, mut notes, store) = coordinator::spawn_with_store(cfg, source);
    let pages = Arc::new(Mutex::new(Vec::new()));
    let (op_tx, op_rx) = mpsc::unbounded_channel();
    let worker = tokio::spawn(notes_worker(lec.clone(), store.clone(), op_rx, events.clone(), pages.clone()));
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let watcher = tokio::spawn(slide_watcher(lec.clone(), store.clone(), watch, stop_rx, events.clone()));
    drop(store);
    let mut op_tx = Some(op_tx);
    let (mut stops, mut commands_open) = (0, true);
    let stopping = AtomicBool::new(false);
    let begin_stop = |op_tx: &mut Option<UnboundedSender<Op>>| {
        if !stopping.swap(true, Ordering::Relaxed) {
            let _ = stop_tx.send(true);
            *op_tx = None; // queued operations still run; nothing new is taken
        }
    };
    loop {
        tokio::select! {
            n = notes.recv() => match n {
                Some(Notification::SourceEnded) => {
                    begin_stop(&mut op_tx);
                    let _ = events.send(Event::Session(Notification::SourceEnded));
                }
                Some(n) => { let _ = events.send(Event::Session(n)); }
                None => break,
            },
            c = commands.recv(), if commands_open => match c {
                Some(Command::Op(op)) => if let Some(tx) = &op_tx { let _ = tx.send(op); },
                Some(Command::Stop) => {
                    stops += 1;
                    handle.request_stop();
                    begin_stop(&mut op_tx);
                    if stops >= 2 {
                        commands_open = false;
                    }
                }
                None => commands_open = false, // input closed: the lecture runs until it is stopped or its audio ends
            },
        }
    }
    begin_stop(&mut op_tx);
    let _ = watcher.await;
    let _ = worker.await;
    let report = handle.finish().await?;
    for p in pages.lock().expect("the page list").drain(..) {
        if !p.is_finished() {
            p.abort();
            let _ = events.send(Event::Warning("the page was still being typeset; `lecture page` finishes it later".into()));
        }
    }
    // The last snapshot, after recovery has drained (spec §5.4), on the sidecar the session left.
    let sc: Sidecar = Sidecar::load(&lec.files.sidecar())?.context("the session left no sidecar")?;
    if let Err(e) = lec.snapshot(&Store::offline(sc, lec.files.sidecar()), "", &events).await {
        let _ = events.send(Event::SnapshotFailed(format!("{e}; everything is kept for the next one")));
    }
    Ok(report)
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p lecturelive-core` (the whole suite: the coordinator change touches every session)
Expected: PASS, M2's gate and stream tests included.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/session/mod.rs crates/core/src/session/coordinator.rs crates/core/src/session/lecture.rs crates/core/tests/lecture_gate.rs crates/core/tests/support/sources.rs crates/cli/src/main.rs
git commit -m "Run a whole lecture: snapshots, polish and page through the sidecar's one writer

A Store runs sidecar updates inside the coordinator while a session records and on the
file when none does, and the session serves its stores until they are dropped, so a
snapshot in flight at stop still commits. A second stop abandons pending recovery, and a
cutoff while a recording's transcript is still flushing is no longer confirmed. The
lecture ends with a last snapshot after recovery has drained."
```

---

### Task 12: The CLI's `lecture`, `lecture page` and `lecture spend` (milestone task 9, CLI)

**Files:**
- Modify: `crates/cli/src/main.rs`

**Interfaces:**
- Consumes: `session::{files::LectureFiles, folder, lecture, segments, spend, lock::FolderLock, launch}`, `notes::{chat, prompts}`, `stt::{stream, rest}`, `audio::{input, loopback, level::SilenceWatch, source::DeviceSource}`.
- Produces: `lecturelive lecture [page|spend] [--dir D] [--course C] [--loopback | --device UID-or-name] [--keyterm K]… [--notes F] [--transcript F] [--slides-dir D] [--secs N] [--keep-days N] [--rebuild]`.

Behaviour, matching `live_notes.py`'s `main()`:
- Configuration: the key and `LECTURE_COURSE` and `LECTURE_DEVICE` come from the environment, then a `.env` in the current directory or above, then the repository's `.env`. The course is `--course`, else the folder above `Weeks/` in the path, else `LECTURE_COURSE`, else "Lecture". The device is `--device` (a UID or part of a name), else `LECTURE_DEVICE`. It is checked before any file is created, so a missing device leaves the folder untouched.
- `spend`: take-over of the CLI's ledger (`<repository>/spend.jsonl`), then the CLI's view of the app ledger. Width from `COLUMNS`, else 80; colour when stdout is a terminal and `NO_COLOR` is unset.
- `page`: "No notes to typeset: … is not in this folder." when the notes file is missing; otherwise the folder lock, then the page from the notes as they are.
- Recording: the folder lock, the canary route restore, launch repair and retention, and `folder::open`, printing what it did. Then other days' recovery, the `--- started/resumed ---` line, and `lecture::run` with a stdin reader: an empty line is a snapshot, `polish` polishes, any other text is a hinted snapshot. Ctrl-C stops, and a second Ctrl-C stops waiting for recovery (M2's deferred minor). `--secs` stops after that long. At the end it prints the files and this lecture's spend today.
- Printed lines follow the CLI's `say` format: `  ◆ notes  12 words and 1 slide folded in  $0.01`, the block's lines as `  │ …`, `  ▣ slide 3  slide_03_101500.png, into the next snapshot`, `  ✓ polished  …, previous version in .live_notes/…`, `  ✓ page  Optimisation.html  612 words of 650 allowed  $0.08`, `  ▲ snapshot failed  …; everything is kept for the next one`. A refusal's message loses its own final period before ". Recording continues without it." (M2's deferred double-period minor, fixed in both `record` and `lecture`, which print it).

- [ ] **Step 1: Write the smoke checks** (the CLI has no unit tests; its logic is core's, tested above)

```bash
cargo run -q -p lecturelive-cli -- lecture --help
S=<scratchpad>/cli; mkdir -p "$S/empty" && cargo run -q -p lecturelive-cli -- lecture --dir "$S/empty" --device "no such input"; echo "exit $?"; command ls -A "$S/empty"
```

Expected before implementing: "unrecognized subcommand 'lecture'".

- [ ] **Step 2: Run them to verify they fail**

Expected: clap's error for the unknown subcommand.

- [ ] **Step 3: Implement**

In `crates/cli/src/main.rs`, a new `Cmd::Lecture(LectureArgs)` (a `clap::Args` struct with the flags above; `command` is `Option<String>` with `value_parser = ["page", "spend"]`). Then:

```rust
const REPO_ENV: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../.env");
/// The Python CLI's ledger, taken over by the app's (spec §8).
const CLI_LEDGER: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../spend.jsonl");

/// The environment, a .env here or above, then the repository's .env.
fn env_value(name: &str) -> Option<String> {
    let _ = dotenvy::dotenv();
    std::env::var(name).ok().filter(|v| !v.is_empty()).or_else(|| dotenvy::from_path_iter(REPO_ENV).ok()?.flatten().find(|(k, _)| k == name).map(|(_, v)| v))
}

/// The folder above `Weeks/` in `<course>/Weeks/<lecture>`, so each course names itself.
fn course_from_path(dir: &Path) -> Option<String> {
    let parts: Vec<String> = dir.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
    (1..parts.len()).rev().find(|&i| parts[i] == "Weeks").map(|i| parts[i - 1].clone())
}

/// A message's own final period dropped before the sentence that follows it.
fn sentence(m: &str) -> &str {
    m.trim_end().trim_end_matches('.')
}

fn paint() -> spend::Paint {
    use std::io::IsTerminal;
    spend::Paint { color: std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none(), truecolor: matches!(std::env::var("COLORTERM").as_deref(), Ok("truecolor" | "24bit")) }
}

/// The CLI's `say`: one event line, its mark, what it is, what happened.
fn say(p: spend::Paint, kind: &str, label: &str, detail: &str) {
    let (mark, colour) = match kind {
        "slide" => ("▣", "teal"),
        "notes" => ("◆", "teal"),
        "page" => ("✦", "teal"),
        "done" => ("✓", "teal"),
        _ => ("▲", "red"),
    };
    println!("{}", format!("  {} {}  {detail}", p.paint(mark, &[colour]), p.paint(label, &["bold"])).trim_end());
}

fn plural(n: usize, word: &str) -> String {
    format!("{n} {word}{}", if n == 1 { "" } else { "s" })
}

/// Where macOS saves screenshots (`defaults read com.apple.screencapture location`), else the Desktop.
fn screenshot_dir() -> Option<PathBuf> {
    let out = std::process::Command::new("defaults").args(["read", "com.apple.screencapture", "location"]).output().ok()?;
    let raw = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let home = dirs::home_dir()?;
    let p = raw.strip_prefix("~/").map_or_else(|| PathBuf::from(&raw), |r| home.join(r));
    Some(if out.status.success() && p.is_dir() { p } else { home.join("Desktop") })
}

/// An input by UID, or by part of its name, checked before the folder is touched.
fn resolve_input(loopback: bool, device: Option<String>) -> Result<(String, String)> {
    if loopback {
        return Ok((loopback::BLACKHOLE_UID.to_string(), "BlackHole 2ch".to_string()));
    }
    let inputs = input::list_inputs()?;
    let want = device.context("give --loopback or --device <UID or part of its name> (or LECTURE_DEVICE in .env); `lecturelive inputs` lists them")?;
    inputs
        .iter()
        .find(|i| i.uid == want)
        .or_else(|| inputs.iter().find(|i| i.name.to_lowercase().contains(&want.to_lowercase())))
        .map(|i| (i.uid.clone(), i.name.clone()))
        .with_context(|| format!("No audio input matches {want:?}. Inputs now: {}.", inputs.iter().map(|i| i.name.as_str()).collect::<Vec<_>>().join(", ")))
}
```

`lecture_cmd(args)` follows the order in the behaviour list. The printer task turns `Event`s into lines with `say`:
- `Session(Segment(s))`: `  HH:MM:SS  text`, with `  (recovered)` dimmed for recovered segments.
- `Session(Stt(..))`, `Session(Gap(..))`, `Session(DeviceGone/Back)`, `Session(Failed)`, `Session(Recovered)`, `Session(RecoveryFailed)`: the `record` command's lines, through `sentence` for refusals.
- `Session(Level(l))`: the loopback silence warning through a `SilenceWatch`, as `record` does.
- `Session(SpendFailed(m))`: `say("warn", "spend", m)`.
- `Committed`: the block's lines as `  │ …` (dim), then `say("notes", "notes", "{words} and {slides} folded in  {money}")`, with " (transcription still catching up)" when `!confirmed`.
- `NothingNew`: `say("notes", "snapshot", "nothing new since the last one")`.
- `SnapshotFailed(m)`: `say("warn", "snapshot failed", m)`.
- `Polished`: `say("done", "polished", "{notes name}, previous version in .live_notes/{backup}  {money}")`.
- `PolishStopped` and `PolishFailed`: `say("warn", "polish stopped" | "polish failed", m)`.
- `Page`: `say("done", "page", "{file}  {words} words of {budget} allowed  {money}")`, red when over budget, noting "(from the cache, free)" when cached, and naming any `missing` parts.
- `PageFailed(m)`: `say("warn", "page failed", m)`.
- `Slide`: `say("slide", "slide {n}", "{file}, into the next snapshot")`.
- `Busy(m)`: `  … m` (dim).
- `Warning(m)`: `say("warn", "warning", m)`.
- `Preview`: not printed, as the CLI does; the committed block is printed instead.

The stdin reader is a thread over `std::io::stdin().lines()` sending `Command::Op`. The Ctrl-C task loops on `tokio::signal::ctrl_c()`, sending `Command::Stop`. It prints `say("notes", "stopping", "finishing the transcript and recovery, then a last snapshot (Ctrl-C again stops waiting for recovery)")` the first time, and `say("warn", "stopping", "no longer waiting for recovery; its gaps wait for the next session")` the second. The end prints `  ✓ saved  {notes}  {transcript}` and `    {money} spent on this lecture today; \`lecture spend\` has the rest`.

`record`'s refusal line becomes `eprintln!("transcription refused: {}. Recording continues without it.", sentence(&m))`.

- [ ] **Step 4: Run the smoke checks**

Expected: `lecture --help` lists the flags. The missing device prints "No audio input matches \"no such input\". Inputs now: …" with `exit 1`, and `command ls -A` of the folder prints nothing (untouched). `cargo run -q -p lecturelive-cli -- lecture spend` prints the view of the app ledger after importing the CLI's lines (the ledger's contents are local and are not quoted in any committed file).

- [ ] **Step 5: Commit**

```bash
git add crates/cli/src/main.rs
git commit -m "Add the lecture command: a whole lecture headless, with page and spend as in the Python CLI

Enter snapshots, a hint and Enter adds a focus hint, polish polishes and typesets the page,
Ctrl-C stops after a last snapshot and a second Ctrl-C stops waiting for recovery. The
device is checked before the folder is touched, and a refusal no longer prints two periods."
```

---

### Task 13: Live checks (within the $2 cap)

**Files:**
- Create: `crates/core/tests/notes_live.rs` (`#[ignore]`)

**Interfaces:**
- Consumes: `ChatClient` against `https://api.x.ai`, `Lecture::snapshot`, `page::make_page`; the fixture lecture of Task 10.

- [ ] **Step 1: Write the live tests**

```rust
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
    std::fs::copy(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/notes/lecture/lecture_notes_20260925.md"), &notes).unwrap();
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
```

- [ ] **Step 2: Run them**

Run: `cargo test -p lecturelive-core --test notes_live -- --ignored --nocapture --test-threads 1`
Expected: both pass; the costs are printed and recorded in the ledger. If the page overshoots its budget after the revision, the test fails. Record the words and budget, record the line as failed with "no §14.1 fallback applies", and do not change the budget rule.

- [ ] **Step 3: The headless lecture, live**

On a synthetic folder under `~/Library/Application Support/LectureLive/m3-lecture/Week 01 — Optimisation/`, with the default output unchanged (checked before and after with `lecturelive outputs` / `SwitchAudioSource -c` if present, or `system_profiler SPAudioDataType`). Synthesised sentences go into BlackHole with `say -a "BlackHole 2ch"`, while the command reads scripted input:

```bash
L=$HOME/Documents/Tools/LectureLive/target/debug/lecturelive
D="$HOME/Library/Application Support/LectureLive/m3-lecture/Week 01 — Optimisation"; mkdir -p "$D"
( sleep 25; echo; sleep 20; echo "focus on momentum"; sleep 15; echo polish; sleep 200 ) | \
  "$L" lecture --dir "$D" --course "M3 synthetic" --loopback --secs 240 > "$D/../run.log" 2>&1 &
sleep 5; for i in 1 2 3 4 5 6; do say -a "BlackHole 2ch" "Gradient descent moves the weights against the gradient. The learning rate sets the step. Momentum averages past gradients."; sleep 8; done
wait
```

Evidence to record: `run.log` (two committed snapshots, the polish, the page with its words and budget, a last snapshot, spend); the notes, backup, page, transcript (`--- started` line first) and sidecar (cursor = segment count, no journal); the ledger lines of this lecture (kinds and total); and the default output before and after. A slide is exercised by copying a PNG into `slides/` during the run (`cp` of a `support::slides`-style image made with `sips` from any system PNG, e.g. `/System/Library/CoreServices/CoreTypes.bundle/Contents/Resources/GenericDocumentIcon.icns` → PNG).

- [ ] **Step 4: Commit**

```bash
git add crates/core/tests/notes_live.rs
git commit -m "Add live checks for a streamed snapshot and the study page's budget and cache"
```

---

### Task 14: Spec §6 and §8 to M3's evidence

**Files:**
- Modify: `docs/spec.md` §6 (6.1–6.4) and §8

Write the present design as if it were always so (no "previously"), from this plan's research and settled points:
- §6.1: a snapshot does not wait for pending recovery. A cutoff between recordings is confirmed only when no recording's live transcript is still flushing. The budget is 200,000 tokens (`grok-4.7`'s `long_context_threshold`; context 500,000), estimated at three characters per token, 1,800 per slide and 16,000 for output. Over it, the prompt carries the omitted prefix's own headings and the recent part verbatim from a line start; the outline is derived locally, with no request and no cache.
- §6.2: the stream shape. Requests ask for `stream_options.include_usage`; reasoning deltas come before content; the finish chunk is followed by one usage chunk with `choices: []`, then `[DONE]`. The strict success rule, and the idle timeout of 180 s. The embed repair rules as built. The commit steps with the sidecar save before the journal is deleted, and recovery's idempotence and stop cases.
- §6.3: a polish over notes edited meanwhile is not written; backup names `<stem>_HHMMSS[_n].md`.
- §6.4: the cache file is the CLI's, with its format; `TEMPLATE` is embedded.
- §8: the sidecar's new fields (`lecture_date`, `notes {revision, len, sha256, segment_cursor, slide_index}`, `slides [{index, file, shown_at}]`); segment source `imported`; the initialisation table as built, including notes without state rebuilt and the legacy `commit` entry; transcript repair at open; cross-day recovery at launch; the Python CLI's check on any `*.v2.json` in the folder. The ledger: its path in the app's data directory; incremental take-over with its import mark; the format as `json.dumps`; one `transcribe` line per recording streamed ($0.20/h) and per recovery piece ($0.10/h); chat costs from the usage chunk only, recorded whenever it arrived; a failed request with no cost records nothing; a cancelled stream records nothing, because its cost comes only at the end.

- [ ] **Step 1:** Edit §6 and §8 (native Edit tool; they are prose). Read both back once.
- [ ] **Step 2: Commit**

```bash
git add docs/spec.md
git commit -m "Rewrite spec 6 and 8 to the recorded SSE protocol and the M3 design"
```

---

### Task 15: Acceptance, review and findings

**Files:**
- Modify: `docs/milestones.md` (M3 section: gate ticks, Findings; M3 Status row, linked to this plan)

- [ ] **Step 1:** `cargo test -p lecturelive-core`: record the pass, fail and ignored counts. Then run the gate files five times in a row (`--test lecture_gate --test notes_page --test notes_chat`) and record any flake.
- [ ] **Step 2:** A fresh reviewer on the most capable model (`opus`) reviews `git diff m2-streaming-recovery..m3-notes-session-parity`, given this plan's Review Focus verbatim and the ledger's Ruling lines. Re-grade its findings by their effect on the person using the tool. Fix Critical and Important findings test-first, and record Minor ones as open threads with their owning milestone.
- [ ] **Step 3:** Findings in the M3 section: resolved crate versions; the SSE behaviour the fake encodes; the golden comparisons (prompts, each file format, the ledger); fault injection per step; legacy migration; the headless lecture run; the page's budget and its cached re-render; live spend; failed lines with the §14.1 pointer; open threads with owners. Tick the gate lines that held; Status `done` only if all hold, otherwise `in progress`; link the plan.
- [ ] **Step 4: Commit**

```bash
git add docs/milestones.md
git commit -m "Record M3 findings"
```
