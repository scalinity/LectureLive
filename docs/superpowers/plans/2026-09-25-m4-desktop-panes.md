# M4 Desktop App: Transcript + Notes Panes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Grow the M0 canary app into the lecture app: a Tauri adapter that owns one `session::lecture::run` per window and streams it to the frontend, and a Svelte 5 frontend with a transcript pane, a notes pane, a control bar, a spend view and Keychain storage of the API key. A reload or a hidden window restores the full view. Hint, cancel and polish behave as in the CLI. Bursts and a two-hour lecture stay within a frame, and rendered Markdown can neither run script nor load anything remote.

**Architecture:** Core gains what the app needs and the CLI lacked: `Command::Cancel` (the notes request in flight and those queued behind it stop and nothing is written), `Command::State` (a consistent sidecar copy served by the session's own store), the document revision on `Committed` and `Polished`, and `session::start::prepare` (the start-up sequence the CLI ran inline, with progress for other days' recovery). The desktop crate's `adapter` is a pure `Pump`: it maps each `lecture::Event` to one message on one of three streams (status over a Tauri event; transcript and notes over one Tauri `Channel` each), stamps every message with the session id and one rising sequence number, and keeps a mirror of what a reload needs (the open utterance, the preview so far, recent notices, the status). `get_session_state` holds the pump's lock, so its sequence watermark is exact, and assembles the sidecar (through `Command::State`), the segment log and the notes file, retried until the file's hash matches the sidecar's revision. The frontend's one rune store (`session.svelte.ts`) attaches its listener and channels, then hydrates, then applies only messages above the watermark, once per animation frame.

**Tech Stack:** Rust 1.96.1, `tauri 2.11.6` (+ feature `protocol-asset`), `tauri-plugin-dialog 2.7.3`, `security-framework 3.7.0` (already in `Cargo.lock` through `rustls-platform-verifier`); `svelte 5.57.1`, `@sveltejs/kit 2.70.3`, `vite 8.3.1`, `@tauri-apps/api 2.11.1`, `@tauri-apps/plugin-dialog 2.7.3`, `marked 18.0.14`, `dompurify 3.4.15` (spec §13), `vitest 5.0.2`, `jsdom 30.1.1`.

**Spec:** `docs/spec.md` §3.2, §3.6, §6.2 (cancellation), §8 (files the panes read), §9 (all), §10 rows "Notes call fails, truncates or is cancelled" and "Spend ledger write fails", §11 (UI testing). Gate: `docs/milestones.md` → M4. Evidence: the M0–M3 Findings in `docs/milestones.md` and the plan research below.

**Plan research (2026-09-25, before this plan was written).**

| Question | Evidence | Consequence |
|---|---|---|
| Channel API in `@tauri-apps/api 2.11.1` | `new Channel<T>(onmessage)`; the JS side reorders by a per-message `index`, so one channel's messages arrive in order. A channel is serialised as `__CHANNEL__:<id>` and its callback dies with the page. Rust: `tauri::ipc::Channel<T>::send(T) -> tauri::Result<()>`, `Clone`. | Order between notes deltas and the committed block is kept by putting both on one channel. Every page load calls `attach(transcript, notes)`, which replaces the adapter's channels. |
| Event API | `listen<T>(name, cb) -> Promise<UnlistenFn>`; no order is promised between an event and a channel message. | Status is idempotent (a whole `Status` each time) plus notices; nothing on the status stream depends on its order relative to the channels. |
| Asset protocol | Cargo feature `protocol-asset`; `tauri.conf.json` `app.security.assetProtocol {enable, scope}`; runtime `app.asset_protocol_scope().allow_directory(path, false)` (`tauri-2.11.6/src/scope/fs.rs:351`). On macOS `convertFileSrc(p)` yields `asset://localhost/<encoded path>`. | The static scope is empty; opening a lecture folder allows that lecture's `slides/` only, not recursively. |
| CSP in dev | `tauri-2.11.6/src/manager/webview.rs:42`: `PROXY_DEV_SERVER = cfg!(all(dev, mobile))`. On macOS the webview loads `devUrl` directly, so `tauri.conf.json`'s `csp` never reaches a dev page. SvelteKit's `kit.csp` does: with `mode: "auto"` the dev server answered `content-security-policy: default-src 'self'; … script-src 'self' 'nonce-…'; style-src 'self' 'unsafe-inline'; …` and put the nonce on its one inline script (checked with `curl -sI localhost:1420`, then reverted). | The CSP is `kit.csp` (header in dev, meta tag in a built page) and the same directives in `tauri.conf.json` for the custom protocol of a built app. Script has a nonce and no `unsafe-inline`, so an injected inline handler cannot run even if the sanitiser failed. `style-src 'unsafe-inline'` stays because Svelte and Vite inject styles. A built page is not checked in M4 (no build is authorised). |
| Versions that install with Vite 8 | `npm view`: `marked 18.0.14` (no peers), `dompurify` latest 3.4.16 and 3.4.15 exists, `vitest 5.0.2` (peer `vite ^6.4 ‖ ^7 ‖ ^8`), `jsdom 30.1.1`, `@tauri-apps/plugin-dialog 2.7.3`. Rust `tauri-plugin-dialog` 2.7.3 (3.0 is an alpha). | `dompurify` is pinned at 3.4.15 as spec §13 names it. |
| Keychain crate | `keyring` is at 4.2.0, a new major line built on `keyring-core` and per-platform store crates. `security-framework 3.7.0` is already compiled into this build (`cargo tree -i security-framework`: through `rustls-platform-verifier`) and has `passwords::{get,set,delete}_generic_password`. | The Keychain is reached through `security-framework`: no new crate. Spec §13 still names `keyring`; §13 is not M4's to edit, so this is an open thread for M6's docs pass. |
| Fonts | `notes_template.html` loads Atkinson Hyperlegible Next/Mono from Google Fonts. The app's CSP forbids that, and neither face is installed on this Mac (`~/Library/Fonts`, `/Library/Fonts`). | The app names Atkinson Hyperlegible Next first and falls back to the system face. Installing the font locally (SIL OFL) makes the app use it; no font is bundled. |
| Core notification rates | `Notification::Level` is sent once a second (the CLI's `record` counts seconds by it), not the 10/s spec §3.6 names. `Notification::Open` follows every interim (about 1/s) and is not sent after a close with text. `Coordinator::notify` uses `try_send` on a 256-slot channel, so a notification can be dropped under load. | The meter updates once a second. A live `Segment` closes the open utterance. A dropped segment notification is detected by the segment id and repaired by rehydration. |

**Settled here (the prompt asked for these to be decided, not inherited):**
- *One lecture per window.* The adapter holds at most one running `lecture::run`, spawned on Tauri's own runtime (spec §3.2: no second Tokio runtime). A folder can be open without a session (idle), and its notes and transcript are shown from the files.
- *Transport.* Status and notices go over one Tauri event, `status`; transcript updates and notes messages each go over one `Channel`. Every message carries `session` (a UUID per opened folder or started lecture) and `seq` (one counter across all three streams, assigned under the pump's lock). The frontend drops a message whose session is not its own, whose seq is at or below the hydration watermark, or whose seq is at or below the last one on its stream.
- *Revision coherence.* `Event::Committed` and `Event::Polished` carry the revision their commit produced. The frontend appends a committed block only at `revision == held + 1`, ignores one it already holds, and rehydrates on a jump. `get_session_state` reads the notes file until its SHA-256 matches the sidecar's `notes.sha256` (three tries), so the document and its revision are one state.
- *Cancel.* A new `Command::Cancel` stops the notes request in flight (snapshot, or polish and the snapshot before it) and drops the operations queued behind it. Nothing is written, the batch stays pending, and a cancelled stream records no spend (spec §8: its cost would have come at the end). The page is not cancellable in M4: it runs beside the notes worker and is billed as it goes. The CLI has no cancel, so this adds one command; the other Ops are the CLI's.
- *Stop.* The control bar offers Stop, which finishes the transcript and recovery and then takes the last snapshot. While stopping, it becomes Stop waiting: the second `Command::Stop`, which drops recovery and queued ops. Quitting the app is the third level, as the CLI's third Ctrl-C: the recording is durable to its last second, and the next session repairs and notes what is left.
- *State reads.* `Command::State` is served through the session's store while the session runs. From the moment the lecture begins stopping (when its store is dropped so the session can end), and when no session runs, the sidecar is read from its file, which the sidecar's one writer saves atomically before every answer (spec §8).
- *Start-up.* `session::start::prepare` runs what the CLI's `lecture` command ran inline: launch repair and retention, folder initialisation, other days' recovery (now with a callback per day, which fixes the M3 minor "no progress"), and the session marker. Both the CLI and the app call it.
- *Hidden window.* On `visibilitychange` to hidden, the store stops applying messages and discards them. On visible, it rehydrates from `get_session_state`. Whether WKWebView fires `visibilitychange` when a Tauri window is hidden is checked in the running app (Task 11); if it does not, the store also listens to Tauri's window focus events and the Finding says so.
- *Frame work* is measured, per animation frame, as the time to apply that frame's queued messages, flush Svelte (`flushSync`) and force layout (one `scrollHeight` read of each pane). It is measured in the real engine: the Tauri dev app in WKWebView, driven by a fixture transport (Task 10).
- *Deferred minors taken:* other days' recovery gets a progress line (Task 1); polish's ledger warning reaches the status (Task 1, `lecture.rs` is touched anyway). Not taken: the page's ledger warning (`page.rs` is not touched), the lone `-` after an inline embed (`embeds.rs` is not touched), the flaky `a_stuck_stt_worker_loses_frames_not_control_messages` (`coordinator.rs` is not touched unless it fails a run of this milestone; if it does, it is paced as M2 paced the fixture source and a Ruling records it).

**Visual design** (the milestone-level `/frontend-design:frontend-design` run, before any pane is built; spec §9.4):
- *Subject and job.* A listening desk kept open beside Zoom during a lecture. At arm's length it must answer: is it hearing the lecture, what was just said, what went into the notes. Its commands are the CLI's commands.
- *Palette* (the study page's own tokens, so the app and the page are one family): paper `#F4F6F8` / `#0E1522`, ink `#14213D` / `#E3E8F0`, graphite `#5A6478` / `#98A3B5` (secondary text, tentative words), rule `#D6DCE4` / `#263247`, plate `#E7ECF1` / `#172235` (the bars), signal `#B3261E` / `#F2766B` (stop, gaps, failures), teal `#1D6B72` / `#5DB8C0` (live, the meter, the notes preview). Light and dark follow the system (`prefers-color-scheme`).
- *Type.* `"Atkinson Hyperlegible Next", -apple-system, system-ui, sans-serif` for everything; numbers (times, money, elapsed) use `font-variant-numeric: tabular-nums` in the same face, not a monospace. Base 18 px, ratio 1.2 (15 / 18 / 21.6 / 25.9 px), body line-height 1.55, measure at most 72ch in the notes. The large-type toggle scales the root to 125%; every size is in rem.
- *Layout.* A status strip on top (state, source and meter, elapsed; STT, gaps, spend on the right); three flat columns divided by rules (transcript, notes, the slides strip that M5 fills); the command line at the bottom. No cards and no shadows. Radius 6 px on controls only.

```
┌ ● Listening  BlackHole 2ch ▮▮▮▮▯  0:42:10 ─────────── Transcribing · 0 gaps · $0.14 today  Aa ┐
│ 10:02  So the learning rate…    │ # Machine Learning — Week 06           │ Slides     │
│ 10:03  If we take a step…       │ ## Gradient descent                     │ (M5)       │
│ 10:04  and the gradient is| ░░  │ ▎◆ writing: ## Momentum …               │            │
│               [Jump to live]    │                                         │            │
└ ◆ [ a hint, or ⏎ for a snapshot ___________________ ] Snapshot  Polish  Study page   Stop ┘
```
- *The one bold element* is the command line. It speaks the CLI's grammar: an empty ⏎ is a snapshot, a hint then ⏎ is a hinted snapshot. Notices use the CLI's marks: ◆ notes, ▣ slide, ✦ page, ✓ done, ▲ warning.
- *Motion:* only the fades that carry meaning. A new word fades in (opacity 0→1, 180 ms); a tentative word turning stable changes colour (graphite → ink, 180 ms) without remounting; a finished block of the notes preview fades in once. `prefers-reduced-motion` turns all three off.
- *Copy:* sentence case, plain verbs, no all-caps labels, and no arrows appended to buttons. A button keeps its name through the flow ("Snapshot" gives "Snapshot taken"). Errors say what happened and what to do.
- *Reviewed against the brief:* a monospace face for data labels was dropped (a generated-design default) for tabular numbers in the text face; card panes were dropped for flat columns; the accent is the project's teal, not a new colour.

**Execution:** executing-plans, inline, without approval stops. A fresh reviewer on the most capable model (`opus`) reviews the whole branch before the findings commit (Task 13). Ledger: `.superpowers/sdd/2026-09-25-m4-desktop-panes/progress.md` (git-excluded through `.git/info/exclude`).

## Global Constraints

- macOS 13+, Apple Silicon; one user; no telemetry, accounts or servers.
- Toolchain pinned to `rustc 1.96.1` (`rust-toolchain.toml`); workspace `rust-version` 1.89.
- Builds authorised: `cargo build`, `test`, `run`, `check`, `tree`, `add` (debug profile). Frontend, in `apps/desktop`: `npm install` (and `npm install <package>` for `marked`, `dompurify`, `@tauri-apps/plugin-dialog` and test tooling), `npm run dev`, `npm run tauri dev`, `npm test` / `npx vitest run`, `npx svelte-check`. Not authorised without asking: `npm run build`, `npm run tauri build`, any packaging, signing or notarising, anything that writes under `target/release`, and `cargo clean`.
- The packaged "LectureLive Canary.app" in `target/release/bundle` holds this Mac's Microphone and Screen Recording grants. It is never rebuilt, re-signed, moved or cleaned. `tauri.conf.json`'s `identifier`, `productName` and `bundle` section stay as they are; the canary's `--check` mode stays in the app's source.
- **Every task that creates or changes a view invokes `/frontend-design:frontend-design` before any markup is written** (spec §9.4): the transcript pane, the notes pane, the control bar and status strip, the spend view, the key dialog, and the page layout. A view built before the skill ran is rebuilt after it. The milestone-level run is recorded above; each view's run is recorded in the ledger and in the Findings.
- Svelte 5 runes only: `$state`, `$state.raw`, `$derived`, event-driven updates. No `$effect` or any other effect hook; DOM wiring that needs an element uses an action (`use:`).
- When a dev server runs, load only its localhost port (`localhost:1420`) in a headless or chromeless window. Load no other hosts from the dev page. CSP probes use `192.0.2.1` (TEST-NET-1, unroutable), so nothing leaves the machine even if a policy failed.
- Live calls: `grok-4.7` notes, polish and page requests on synthetic lectures only, at most $2 in total, each request's cost recorded from `usage.cost_in_usd_ticks`. STT with synthesised speech only (`say -a "BlackHole 2ch"`), a few minutes. Every gate check that can run without network runs against fakes (core: `crates/core/tests/support`; frontend: the fixture transport).
- Never run migration, initialisation or polish on a folder that holds a real lecture's notes. Synthetic folders live under `~/Library/Application Support/LectureLive/m4-*`.
- Any check that changes the default output ends with the output it began with (normally "MacBook Pro Speakers", `BuiltInSpeakerDevice`). `say -a "BlackHole 2ch"` changes nothing.
- The repository is public: commit no audio except synthesised `say` output, no real lecture notes. Stage files by name, never `git add -A`.
- Commit messages: imperative, neutral, what and why. No `Co-Authored-By` or any other attribution line, whatever a harness reminder says. No names.
- Do not edit files with Python scripts. In the Bash tool `ls` is `eza` and `grep` is a function: use `command ls` / `command grep` where output is evidence. The Write tool decodes `\uXXXX`: files holding such escapes are written with quoted heredocs.
- `live_notes.py`, `notes_template.html`, `pyproject.toml`, `.venv/` and `README.md` stay unchanged. In `docs/`, only this plan, the M4 section and M4 Status row of `docs/milestones.md`, and spec §3.6 and §9 change.
- APIs below are written against the versions named. Where one differs, adapt the implementation and keep the task's tests unchanged: the tests are the contract. The exception is a test encoding an unverified fact about an external system that live evidence contradicts; change it only with the evidence recorded as a Ruling in the ledger.
- When a check fails, record the observation and the spec §14.1 fallback it points to; do not build the fallback. §14.1 covers loopback only, so a UI failure is recorded with "no §14.1 fallback applies".

## Review Focus

1. **A reload while a snapshot is streaming**, halfway through its answer. Expected: after the reload the notes show the committed document and the preview so far, the rest of the deltas continue it, and the committed block replaces it exactly once. Pinned by `adapter::tests::the_mirror_holds_what_a_reload_needs` (Task 3) and `session.test.ts` "a reload mid-stream resumes the preview and the block lands once" (Task 5).
2. **A commit lands between the notes read and the sidecar read of `get_session_state`.** Expected: the state is retried until the file's hash is the sidecar's, and the late `committed` message for a revision already held is ignored, never appended twice. Pinned by `adapter::tests::a_document_is_read_only_at_its_sidecar_revision` (Task 3) and `session.test.ts` "a committed revision already held is ignored and a jump rehydrates" (Task 5).
3. **Hostile Markdown in the notes** (the model's answer, or text typed into the file by hand): script tags, event handlers, `javascript:` links, remote images and stylesheets, `srcset`, SVG, `<base>`, `<meta refresh>`, a `data:` image, a path that climbs out of `slides/`. Expected: nothing runs and nothing remote is requested; only registered slide files render, through the asset protocol. Pinned by `markdown.test.ts` "hostile markdown renders inert" (Task 7) and the in-app CSP probe (Task 11).
4. **Cancel pressed while a polish is waiting behind a snapshot.** Expected: the snapshot stops, the queued polish never sends its request, the notes are unchanged, and the next snapshot sends the same material. Pinned by `lecture_gate::a_cancelled_snapshot_writes_nothing_drops_the_queue_and_its_material_goes_again` (Task 1).
5. **A segment notification dropped by the coordinator's full channel.** Expected: the transcript does not silently miss a line; the next segment's id reveals the hole and the store rehydrates. Pinned by `session.test.ts` "a hole in segment ids rehydrates" (Task 5).

## File Structure

| File | Responsibility | Task |
|---|---|---|
| `crates/core/src/session/lecture.rs` | `Command::{Cancel, State}`, `Event::Cancelled`, revisions on `Committed`/`Polished`, cancellable snapshot and polish, polish's ledger warning | 1 |
| `crates/core/src/session/start.rs` | `prepare`: launch repair, folder open, other days' recovery with progress, session marker | 1 |
| `crates/core/src/session/files.rs` | `course_from_path` (moved from the CLI) | 1 |
| `crates/core/src/session/folder.rs` | `recover_other_days` takes a per-day callback | 1 |
| `crates/core/src/session/spend.rs` | `summary`: the spend view's aggregation, which `render` now uses | 1 |
| `crates/core/tests/lecture_gate.rs` | Cancel and state gate test | 1 |
| `crates/cli/src/main.rs` | Uses `prepare`, `course_from_path`; prints the new events | 1 |
| `apps/desktop/src-tauri/src/keychain.rs` | The API key in the macOS Keychain | 2 |
| `apps/desktop/src-tauri/src/wire.rs` | Message and state types sent to the frontend | 3 |
| `apps/desktop/src-tauri/src/adapter.rs` | `Pump`: event → message mapping, sequence, mirror, state assembly | 3 |
| `apps/desktop/src-tauri/src/app.rs` | The running lecture: folder, start, stop, ops, state, page, spend, key, checks | 3, 8, 9 |
| `apps/desktop/src-tauri/src/canary.rs` | M0's `--check` mode, moved unchanged from `lib.rs` | 3 |
| `apps/desktop/src-tauri/src/lib.rs` | Builder, plugins, command registration | 3 |
| `apps/desktop/src-tauri/{Cargo.toml,tauri.conf.json,capabilities/default.json}` | `protocol-asset`, dialog plugin, CSP, asset scope, window | 3 |
| `apps/desktop/{package.json,vite.config.js,svelte.config.js}` | Test tooling, `kit.csp` | 4, 7 |
| `apps/desktop/src/lib/wire.ts` | TypeScript mirror of `wire.rs` | 4 |
| `apps/desktop/src/lib/transport.ts` | `Transport` interface; the Tauri transport | 4 |
| `apps/desktop/src/lib/fixture.ts` | Fixture transport: scripted sessions for tests, bench and browser preview | 4, 10 |
| `apps/desktop/src/lib/words.ts` | Word IDs across open-utterance updates | 5 |
| `apps/desktop/src/lib/session.svelte.ts` | The rune store: attach, hydrate, sequence checks, frame drain, bounded preview, visibility | 5 |
| `apps/desktop/src/lib/theme.css` | Design tokens and base styles | 4 |
| `apps/desktop/src/lib/Transcript.svelte` | Transcript pane | 6 |
| `apps/desktop/src/lib/markdown.ts` | `marked` + DOMPurify + slide URLs; chunking; preview blocks | 7 |
| `apps/desktop/src/lib/Notes.svelte` | Notes pane | 7 |
| `apps/desktop/src/lib/{StatusStrip,CommandLine,FolderPicker}.svelte` | Control bar | 8 |
| `apps/desktop/src/lib/KeyDialog.svelte` | API key dialog | 8 |
| `apps/desktop/src/lib/SpendView.svelte` | Spend view | 9 |
| `apps/desktop/src/lib/{bench,checks}.ts` | Frame-time measurement; in-app checks | 10, 11 |
| `apps/desktop/src/routes/+page.svelte`, `src/app.html` | Layout, start-up | 6–8 |
| `docs/spec.md` §3.6, §9 | Rewritten to what is built | 12 |
| `docs/milestones.md` M4 section + Status row | Gate, Findings, Status | 13 |

---

### Task 1: Core for the app: cancel, state, revisions, start-up, spend summary (supports milestone tasks 1, 5, 7)

**Files:**
- Create: `crates/core/src/session/start.rs`
- Modify: `crates/core/src/session/{mod.rs,lecture.rs,files.rs,folder.rs,spend.rs}`, `crates/core/tests/lecture_gate.rs`, `crates/cli/src/main.rs`

**Interfaces:**
- Produces: `lecture::Command::{Op(Op), Cancel, Stop, State(tokio::sync::oneshot::Sender<Result<Sidecar, String>>)}` (derives `Debug` only).
- Produces: `lecture::Event::Committed { revision: u64, .. }`, `Event::Polished { backup, usd, revision: u64 }`, `Event::Cancelled(String)` (what was cancelled: "the snapshot" or "the polish").
- Produces: `lecture::Cancel` (`Cancel::never()`); `Lecture::snapshot_with(&self, &Store, &str, &UnboundedSender<Event>, &mut Cancel) -> Result<(), OpError>`, `Lecture::polish_with(&self, &Store, &UnboundedSender<Event>, &mut Cancel) -> bool`; `enum OpError { Cancelled, Failed(String) }`. `snapshot` and `polish` keep their signatures.
- Produces: `start::{prepare(files: &LectureFiles, title: &str, rebuild: bool, retention: Retention, recovery: impl Fn() -> Result<RecoveryLink>, spend: Option<Spend>, on_other_day: &mut (dyn FnMut(&str) + Send)) -> Result<Prepared>}`, `Prepared { launch: LaunchReport, init: InitReport, other_days: Vec<(String, StopReport)> }`.
- Produces: `files::course_from_path(&Path) -> Option<String>`; `folder::recover_other_days(dir, today, recovery, spend, on_start: &mut (dyn FnMut(&str) + Send))`.
- Produces: `spend::{summary(&[SpendEntry]) -> Summary, Summary { total, estimated, calls, months: Vec<Month>, recent: Vec<Recent> }, Month { key, label, total, courses: Vec<(String, f64)> }, Recent { day, label, course, lecture, total, kinds: Vec<(String, f64)> }}` (all `Serialize`).

- [ ] **Step 1: Write the failing tests**

`crates/core/tests/lecture_gate.rs`, appended (imports: `std::sync::atomic::{AtomicBool, Ordering::SeqCst}`, `tokio::sync::oneshot`):

```rust
/// Cancel (spec §6.2, §10): the request in flight stops, the polish queued behind it never sends, the
/// notes are untouched, and the next snapshot sends the same material. The state the session serves
/// carries the revision its events report.
#[tokio::test]
async fn a_cancelled_snapshot_writes_nothing_drops_the_queue_and_its_material_goes_again() {
    let dir = tempfile::tempdir().unwrap();
    let f = files(dir.path());
    let ledger = dir.path().join("spend.jsonl");
    folder::open(&f, TITLE, false).unwrap();
    let stt = fake_stt::start(fake_stt::Config::default()).await;
    let first = Arc::new(AtomicBool::new(true));
    let sse = fake_sse::start(move |b| if first.swap(false, SeqCst) { Reply::Stall } else { respond(b) }).await;
    let stt_cfg = SttConfig { url: stt.url.clone(), backoff_unit: ms(1), connect_timeout: ms(2_000), send_timeout: ms(2_000), idle_timeout: ms(2_000), finalize_wait: ms(2_000), done_wait: ms(2_000), ..SttConfig::new("test-key".into(), vec![]) };
    let lec = Arc::new(lecture_for(&f, &sse.url, &ledger));
    let session = SessionConfig { dir: f.dir.clone(), stem: f.stem.clone(), stt: Some(stream::spawn(stt_cfg).unwrap()), ..Default::default() };
    let (cmd, cmd_rx) = mpsc::unbounded_channel();
    let (ev_tx, mut ev) = mpsc::unbounded_channel();
    let run = tokio::spawn(lecture::run(lec, session, Box::new(Talking { pace: ms(2), fake: stt.state.clone() }), SlideWatch { screenshots: None, poll: ms(20) }, cmd_rx, ev_tx));
    segments_seen(&mut ev, 1).await;
    let untouched = std::fs::read(&f.notes).unwrap();
    cmd.send(Command::Op(Op::Snapshot("first".into()))).unwrap();
    until(&mut ev, "the snapshot in flight", |e| matches!(e, Event::Busy(m) if m.starts_with("snapshot"))).await;
    cmd.send(Command::Op(Op::Polish)).unwrap();
    cmd.send(Command::Cancel).unwrap();
    until(&mut ev, "the cancel", |e| matches!(e, Event::Cancelled(what) if what == "the snapshot")).await;
    assert_eq!(std::fs::read(&f.notes).unwrap(), untouched, "nothing is written");
    let state = |cmd: &mpsc::UnboundedSender<Command>| {
        let (tx, rx) = oneshot::channel();
        cmd.send(Command::State(tx)).unwrap();
        rx
    };
    let sc = state(&cmd).await.unwrap().unwrap();
    assert_eq!(sc.notes.segment_cursor, 0, "the batch stays pending");
    cmd.send(Command::Op(Op::Snapshot(String::new()))).unwrap();
    let Event::Committed { revision, .. } = until(&mut ev, "the next snapshot", |e| matches!(e, Event::Committed { .. })).await else { unreachable!() };
    let sc = state(&cmd).await.unwrap().unwrap();
    assert_eq!(sc.notes.revision, revision, "the event carries the revision the state reports");
    assert!(sc.notes.segment_cursor >= 1);
    cmd.send(Command::Stop).unwrap();
    tokio::time::timeout(Duration::from_secs(30), run).await.expect("the lecture stops").unwrap().unwrap();
    let bodies = sse.state.bodies();
    assert!(!bodies.iter().any(|b| b["messages"][0]["content"].as_str().unwrap_or_default().starts_with("You turn raw")), "the queued polish never sent its request");
    let cancelled: Vec<String> = user_text(&bodies[0]).lines().filter(|l| l.starts_with('[')).map(str::to_string).collect();
    assert!(!cancelled.is_empty());
    assert!(cancelled.iter().all(|l| user_text(&bodies[1]).contains(l.as_str())), "the cancelled material went again");
    assert!(!spend::read(&ledger).unwrap().iter().any(|e| e.what == "notes" && e.usd == 0.0), "a cancelled stream records nothing");
}
```

`crates/core/src/session/files.rs`, in `mod tests`:

```rust
    #[test]
    fn the_course_is_the_folder_above_weeks() {
        assert_eq!(course_from_path(Path::new("/u/Machine Learning/Weeks/Week 06 — Optimisation")).as_deref(), Some("Machine Learning"));
        assert_eq!(course_from_path(Path::new("/u/Lectures/Week 1")), None);
    }
```

`crates/core/src/session/folder.rs`, the existing other-days test gains a progress assertion (the only change to it):

```rust
        let mut seen = Vec::new();
        let done = recover_other_days(dir.path(), "lecture_notes_20260925", recovery, None, &mut |s: &str| seen.push(s.to_string())).await.unwrap();
        assert_eq!(seen, vec!["lecture_notes_20260924"], "each day's recovery is announced before it runs");
```
(the later call passes `&mut |_: &str| {}`).

`crates/core/src/session/start.rs`, test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::folder::How;

    #[tokio::test]
    async fn prepare_opens_a_fresh_folder_and_marks_the_session() {
        let dir = tempfile::tempdir().unwrap();
        let files = LectureFiles::standard(dir.path(), chrono::NaiveDate::from_ymd_opt(2026, 9, 25).unwrap());
        let no_recovery = || -> Result<RecoveryLink> { anyhow::bail!("no recovery in this test") };
        let mut seen = Vec::new();
        let p = prepare(&files, "# T", false, Retention::KeepAll, no_recovery, None, &mut |s: &str| seen.push(s.to_string())).await.unwrap();
        assert_eq!(p.init.how, How::Created);
        assert!(p.other_days.is_empty() && seen.is_empty());
        assert!(std::fs::read_to_string(&files.transcript).unwrap().starts_with("--- started "));
        assert_eq!(std::fs::read_to_string(&files.notes).unwrap(), "# T\n");
    }
}
```

`crates/core/src/session/spend.rs`, in `mod tests`:

```rust
    #[test]
    fn the_summary_holds_what_the_spend_view_shows() {
        let e = |at: &str, course: &str, lecture: &str, what: &str, usd: f64, billed: bool| SpendEntry { at: at.into(), course: course.into(), lecture: lecture.into(), what: what.into(), usd, billed, audio_s: None };
        let s = summary(&[
            e("2026-09-24T16:50:11", "Machine Learning", "Week 06 — Optimisation", "page", 0.265218, true),
            e("2026-09-24T10:00:00", "Machine Learning", "Week 06 — Optimisation", "transcribe", 0.0027, false),
            e("2026-09-25T11:00:00", "Biology", "Week 02", "notes", 0.004, true),
        ]);
        assert_eq!(s.calls, 3);
        assert!((s.total - 0.271918).abs() < 1e-9 && (s.estimated - 0.0027).abs() < 1e-9);
        assert_eq!(s.months.len(), 1);
        assert_eq!((s.months[0].key.as_str(), s.months[0].label.as_str()), ("2026-09", "September 2026"));
        assert_eq!(s.months[0].courses.iter().map(|(c, _)| c.as_str()).collect::<Vec<_>>(), ["Machine Learning", "Biology"]);
        assert_eq!(s.recent.iter().map(|r| (r.label.as_str(), r.course.as_str())).collect::<Vec<_>>(), [("25 Sep", "Biology"), ("24 Sep", "Machine Learning")]);
        assert_eq!(s.recent[1].kinds.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(), ["page", "transcribe"]);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p lecturelive-core a_cancelled_snapshot 2>&1 | tail -5`, `cargo test -p lecturelive-core --lib the_course_is 2>&1 | tail -3`, `cargo test -p lecturelive-core --lib the_summary 2>&1 | tail -3`
Expected: compile errors (`Command::Cancel`, `Command::State`, `course_from_path`, `summary`, `start` not found).

- [ ] **Step 3: Implement**

`lecture.rs`:
- `Command` gains `Cancel` and `State(oneshot::Sender<Result<Sidecar, String>>)`; derives `Debug` only.
- `Cancel` wraps `tokio::sync::watch::Receiver<u64>` and the generation it was taken at; `cancelled(&mut self)` resolves once the watched value exceeds it; `never()` holds a receiver whose sender is kept alive in a `static`-free way (a sender leaked into an `Arc` inside the struct, so `changed()` never fires).
- `run` keeps a `watch::channel(0u64)`; each `Command::Op` is sent to the worker as `(op, *gen.borrow())`; `Command::Cancel` bumps the generation. The worker skips an op whose tag is below the current generation and gives each op it runs a `Cancel` at that generation.
- `snapshot_with` races only the chat request: `tokio::select! { a = self.chat.complete(..) => a, _ = cancel.cancelled() => return Err(OpError::Cancelled) }`. The commit after it is not raced, so a cancel can never leave a commit half-reported. `store.update` returns `sc.notes.revision` after `notesfile::commit`; `Committed` carries it.
- `polish_with` passes the same `Cancel` to its snapshot and races its own request; on cancel it sends `Event::Cancelled("the polish")` and returns false. Its `answer.warning` is sent as `Event::Warning` (M3 minor). `store.update` returns `(backup, sc.notes.revision)`; `Polished` carries it.
- The worker maps `OpError::Cancelled` to `Event::Cancelled("the snapshot")` and `Failed(m)` to `SnapshotFailed` as before.
- `run` keeps `let mut state_store = Some(store.clone())` instead of dropping its store; `begin_stop` sets it to `None`. `Command::State(tx)` spawns `store.read()` when it is `Some`, and otherwise answers with `Sidecar::load(&lec.files.sidecar())`.

`start.rs`: `prepare` runs `launch::recover(&files.dir, retention, now)`, `folder::open(files, title, rebuild)`, `folder::recover_other_days(&files.dir, &files.stem, recovery, spend, on_other_day)`, then `segments::session_marker(&files.transcript, now)`, and returns what each reported. The caller holds the folder lock and undoes a canary route first, as before.

`files.rs`: `course_from_path` moves here from `crates/cli/src/main.rs` unchanged.

`spend.rs`: `summary` computes what `render` computed inline (months in first-seen order, the last three by key, courses by amount; lectures by day, course and lecture, the eight newest, kinds by amount; totals). `render` is rewritten over `summary`, and its golden test stays unchanged.

`cli/src/main.rs`: `lecture_cmd` calls `start::prepare` and prints the same lines from its report, with a `say(p, "notes", "recovering", "<stem>'s transcript gaps")` line from the callback; `course_from_path` comes from core; `show` prints `Event::Cancelled(what)` as `say(p, "warn", "cancelled", &format!("{what}; nothing was written, everything is kept for the next snapshot"))`. The CLI never sends `Cancel` or `State`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p lecturelive-core 2>&1 | command grep -E "^test result|FAILED|panicked"` and `cargo build -p lecturelive-cli`
Expected: every result line `ok`; 229 + 4 new = 233 passed, 9 ignored.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/session/start.rs crates/core/src/session/mod.rs crates/core/src/session/lecture.rs crates/core/src/session/files.rs crates/core/src/session/folder.rs crates/core/src/session/spend.rs crates/core/tests/lecture_gate.rs crates/cli/src/main.rs
git commit -m "Let a lecture cancel its notes request, serve its state and report revisions, and share its start-up"
```

---

### Task 2: The API key in the Keychain (milestone task 6)

**Files:**
- Create: `apps/desktop/src-tauri/src/keychain.rs`
- Modify: `apps/desktop/src-tauri/Cargo.toml` (`security-framework = "3.7"`, `dotenvy = "0.15"`, `uuid`, `tokio` with `sync`, `anyhow`)

**Interfaces:**
- Produces: `keychain::{SERVICE: &str = "com.lecturelive.app", ACCOUNT: &str = "GROK_API_KEY", get(service) -> anyhow::Result<Option<String>>, set(service, key: &str) -> anyhow::Result<()>, delete(service) -> anyhow::Result<()>, env_key() -> Option<String>}`. `env_key` reads `GROK_API_KEY` from the environment, then the repository's `.env` (`concat!(env!("CARGO_MANIFEST_DIR"), "/../../../.env")`).

The key never crosses to the frontend: commands report only whether a key is stored and whether an environment key exists (Task 8).

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// Touches the login Keychain under a test service, so it runs by hand:
    /// `cargo test -p desktop keychain -- --ignored`.
    #[test]
    #[ignore]
    fn a_key_is_stored_read_replaced_and_deleted() {
        let service = format!("com.lecturelive.test.{}", uuid::Uuid::new_v4());
        assert_eq!(get(&service).unwrap(), None);
        set(&service, "xai-first").unwrap();
        assert_eq!(get(&service).unwrap().as_deref(), Some("xai-first"));
        set(&service, "xai-second").unwrap();
        assert_eq!(get(&service).unwrap().as_deref(), Some("xai-second"));
        delete(&service).unwrap();
        assert_eq!(get(&service).unwrap(), None);
    }
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p desktop keychain -- --ignored 2>&1 | tail -5`
Expected: compile error (`get`, `set`, `delete` not found).

- [ ] **Step 3: Implement**

`get` maps `errSecItemNotFound` (-25300) to `Ok(None)`; `set` replaces (`set_generic_password` updates an existing item); `delete` treats not-found as done. `env_key` uses `dotenvy::from_path_iter` as the CLI's `env_value` does.

- [ ] **Step 4: Run it to verify it passes**

Run: `cargo test -p desktop keychain -- --ignored 2>&1 | tail -5`
Expected: 1 passed. If macOS shows a Keychain prompt, the run is moved to the person's sitting and the ledger says so.

- [ ] **Step 5: Commit**

```bash
git add apps/desktop/src-tauri/src/keychain.rs apps/desktop/src-tauri/Cargo.toml Cargo.lock
git commit -m "Keep the API key in the macOS Keychain for the app"
```

---

### Task 3: Tauri adapter (milestone task 1)

**Files:**
- Create: `apps/desktop/src-tauri/src/{wire.rs,adapter.rs,app.rs,canary.rs}`
- Modify: `apps/desktop/src-tauri/src/lib.rs`, `apps/desktop/src-tauri/Cargo.toml` (`tauri` feature `protocol-asset`; `tauri-plugin-dialog = "2"`), `apps/desktop/src-tauri/tauri.conf.json`, `apps/desktop/src-tauri/capabilities/default.json`

**Interfaces:**
- Consumes (Task 1): `lecture::{run, Command, Event, Lecture, Op, SlideWatch}`, `start::prepare`, `files::course_from_path`; (Task 2): `keychain`.
- Produces (`wire.rs`, all `Serialize`, `#[serde(tag = "type", rename_all = "snake_case")]` on enums):
  - `Envelope<T> { session: String, seq: u64, #[serde(flatten)] msg: T }`
  - `TranscriptMsg::{Open { utterance: u64, stable: String, tentative: String }, Closed { utterance: u64, segment: SegmentView }, Segment { segment: SegmentView }}`; `SegmentView { id: u64, at: String /* HH:MM:SS */, text: String, recovered: bool }`
  - `NotesMsg::{Delta { op: u64, text: String }, Committed { op: u64, revision: u64, block: String }, Ended { op: u64, outcome: Outcome, message: String }, Polished { revision: u64 }}`; `Outcome::{NothingNew, Failed, Cancelled}`
  - `StatusMsg::{Status(Status), Notice(Notice), Slide(SlideView)}`; `Status { phase: Phase, folder: Option<FolderView>, source: Option<String>, level_dbfs: Option<f32>, stt: String, stt_ok: bool, busy: Option<String>, gaps: usize, started_at: Option<String>, spend_usd: f64, silence: bool }`; `Phase::{Idle, Starting, Running, Stopping, StoppingNow, Ended}`; `Notice { kind: NoticeKind /* notes|slide|page|done|warn */, label: String, detail: String, at: String }`; `SlideView { index: u32, file: String, path: String }`; `FolderView { dir: String, course: String, name: String, notes_dir: String, page: Option<String> }`
  - `SessionState { session, seq, status, notices: Vec<Notice>, segments: Vec<SegmentView>, open: Option<OpenView>, revision: u64, document: String, preview: Option<PreviewView>, op: u64, slides: Vec<SlideView>, pending_segments: u64, pending_slides: usize }`; `OpenView { utterance, stable, tentative }`; `PreviewView { op, text }`
- Produces (`adapter.rs`): `trait Sink: Send + Sync { fn send(&self, stream: Stream, msg: serde_json::Value); }`, `enum Stream { Status, Transcript, Notes }`; `Pump::new(session: String, sink: Arc<dyn Sink>, spend: Option<Spend>, loopback: bool)`, `Pump::apply(&mut self, e: Event)`, `Pump::set_status(&mut self, f: impl FnOnce(&mut Status))`, `Pump::notice(&mut self, kind, label, detail)`, `Pump::state(&self, sc: Option<&Sidecar>, segments: &[Segment], document: String, files: Option<&LectureFiles>) -> SessionState`, `read_document(files: &LectureFiles, notes: &NotesState) -> Option<String>`.
- Produces (commands, `app.rs`, all `async`, errors as `String`): `attach(transcript: Channel<Value>, notes: Channel<Value>)`, `get_session_state() -> SessionState`, `select_folder(dir: String) -> FolderView`, `inputs() -> Vec<InputView { name, uid }>`, `loopback_status() -> LoopbackView { present, blackhole_present }`, `start_lecture(source: String /* "loopback" or a UID */) -> String /* session */`, `stop() -> u32 /* stop level */`, `snapshot(hint: String)`, `polish()`, `cancel()`, `open_page()`, `spend_summary() -> spend::Summary` (Task 9), `key_status() -> KeyStatus { stored, env_available }`, `save_key(key: String)`, `import_key_from_env()` (Task 8), `check_config() -> Option<CheckConfig { mode, dir }>`, `check_report(name: String, json: String)`, `exit_app()` (Tasks 10–11).

- [ ] **Step 1: Write the failing tests** (`adapter.rs`, test module)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Local, TimeZone};
    use lecturelive_core::session::coordinator::{Notification, SttStatus};
    use lecturelive_core::session::segments::{Segment, SegmentSource};
    use std::sync::Mutex;

    #[derive(Default)]
    struct Recorded(Mutex<Vec<(Stream, Value)>>);
    impl Sink for Recorded {
        fn send(&self, stream: Stream, msg: Value) {
            self.0.lock().unwrap().push((stream, msg));
        }
    }
    impl Recorded {
        fn take(&self) -> Vec<(Stream, Value)> {
            std::mem::take(&mut *self.0.lock().unwrap())
        }
        fn on(&self, s: Stream) -> Vec<Value> {
            self.take().into_iter().filter(|(k, _)| *k == s).map(|(_, v)| v).collect()
        }
    }

    fn pump() -> (Pump, Arc<Recorded>) {
        let sink = Arc::new(Recorded::default());
        (Pump::new("s1".into(), sink.clone(), None, false), sink)
    }

    fn seg(id: u64, text: &str, source: SegmentSource) -> Segment {
        let at = Local.with_ymd_and_hms(2026, 9, 25, 10, 2, 3).unwrap();
        Segment { id, recording_id: uuid::Uuid::nil(), start_sample: 0, end_sample: 16_000, said_at: at, start: at, end: at, text: text.into(), words: vec![], source }
    }

    fn committed(revision: u64) -> Event {
        Event::Committed { words: 12, slides: 1, block: "\n<!-- 10:02:03 -->\n## A\n".into(), usd: 0.02, confirmed: true, removed: 0, missing: 0, revision }
    }

    #[test]
    fn every_message_carries_the_session_and_one_rising_sequence() {
        let (mut p, sink) = pump();
        p.apply(Event::Session(Notification::Open { stable: "the".into(), tentative: "rate".into() }));
        p.apply(Event::Preview("## A".into()));
        p.apply(Event::Session(Notification::Segment(seg(0, "the rate", SegmentSource::Live))));
        p.apply(committed(2));
        p.apply(Event::Slide { index: 1, file: "slides/slide_01_100203.png".into() });
        let all = sink.take();
        assert!(all.len() >= 5);
        assert!(all.iter().all(|(_, m)| m["session"] == "s1"));
        let seqs: Vec<u64> = all.iter().map(|(_, m)| m["seq"].as_u64().unwrap()).collect();
        assert!(seqs.windows(2).all(|w| w[1] == w[0] + 1) && seqs[0] == 1, "{seqs:?}");
        assert_eq!(p.seq(), *seqs.last().unwrap());
    }

    #[test]
    fn a_live_segment_closes_the_open_utterance_and_a_recovered_one_does_not() {
        let (mut p, sink) = pump();
        p.apply(Event::Session(Notification::Open { stable: "the".into(), tentative: "rate".into() }));
        let open = sink.on(Stream::Transcript);
        assert_eq!((open[0]["type"].as_str(), open[0]["utterance"].as_u64(), open[0]["tentative"].as_str()), (Some("open"), Some(1), Some("rate")));
        p.apply(Event::Session(Notification::Segment(seg(0, "recovered words", SegmentSource::Recovered))));
        let r = sink.on(Stream::Transcript);
        assert_eq!((r[0]["type"].as_str(), r[0]["segment"]["recovered"].as_bool(), r[0]["segment"]["at"].as_str()), (Some("segment"), Some(true), Some("10:02:03")));
        assert!(p.mirror().open.is_some(), "recovery does not close what is being said");
        p.apply(Event::Session(Notification::Segment(seg(1, "the rate", SegmentSource::Live))));
        let c = sink.on(Stream::Transcript);
        assert_eq!((c[0]["type"].as_str(), c[0]["utterance"].as_u64(), c[0]["segment"]["id"].as_u64()), (Some("closed"), Some(1), Some(1)));
        assert!(p.mirror().open.is_none());
        p.apply(Event::Session(Notification::Open { stable: String::new(), tentative: "next".into() }));
        assert_eq!(sink.on(Stream::Transcript)[0]["utterance"].as_u64(), Some(2));
    }

    #[test]
    fn deltas_share_an_op_with_the_result_that_ends_them() {
        let (mut p, sink) = pump();
        p.apply(Event::Preview("## A".into()));
        p.apply(Event::Preview("\n- b".into()));
        p.apply(committed(4));
        p.apply(Event::Preview("## C".into()));
        p.apply(Event::SnapshotFailed("stream broke".into()));
        p.apply(Event::NothingNew);
        p.apply(Event::Cancelled("the snapshot".into()));
        p.apply(Event::Polished { backup: "/l/.live_notes/b.md".into(), usd: 0.01, revision: 5 });
        let n = sink.on(Stream::Notes);
        let brief: Vec<(String, u64)> = n.iter().map(|m| (m["type"].as_str().unwrap().to_string(), m["op"].as_u64().unwrap_or(0))).collect();
        assert_eq!(brief, [("delta", 1), ("delta", 1), ("committed", 1), ("delta", 2), ("ended", 2), ("ended", 3), ("ended", 4), ("polished", 0)].map(|(t, o)| (t.to_string(), o)));
        assert_eq!(n[2]["revision"].as_u64(), Some(4));
        assert_eq!([n[4]["outcome"].as_str(), n[5]["outcome"].as_str(), n[6]["outcome"].as_str()], [Some("failed"), Some("nothing_new"), Some("cancelled")]);
        assert_eq!(n[7]["revision"].as_u64(), Some(5));
    }

    #[test]
    fn the_mirror_holds_what_a_reload_needs() {
        let (mut p, _sink) = pump();
        p.apply(Event::Busy("snapshot, 12 words to grok-4.7".into()));
        p.apply(Event::Session(Notification::Open { stable: "the".into(), tentative: "rate".into() }));
        p.apply(Event::Preview("## A".into()));
        p.apply(Event::Preview("\n- b".into()));
        p.apply(Event::Warning("slide 2 is no longer on disk".into()));
        let s = p.state(None, &[seg(0, "said", SegmentSource::Live)], "# T\n".into(), None);
        assert_eq!(s.seq, p.seq());
        assert_eq!(s.preview.as_ref().map(|v| (v.op, v.text.as_str())), Some((1, "## A\n- b")));
        assert_eq!(s.open.as_ref().map(|o| (o.utterance, o.tentative.as_str())), Some((1, "rate")));
        assert_eq!(s.status.busy.as_deref(), Some("snapshot, 12 words to grok-4.7"));
        assert_eq!(s.notices.last().map(|n| n.detail.as_str()), Some("slide 2 is no longer on disk"));
        assert_eq!((s.segments.len(), s.document.as_str(), s.op), (1, "# T\n", 1));
        p.apply(committed(1));
        let s = p.state(None, &[], String::new(), None);
        assert!(s.preview.is_none() && s.status.busy.is_none(), "the block replaced the preview");
    }

    #[test]
    fn status_changes_are_sent_whole_and_stop_levels_are_phases() {
        let (mut p, sink) = pump();
        p.set_status(|s| s.phase = Phase::Running);
        p.apply(Event::Session(Notification::Stt(SttStatus::Retrying { after: std::time::Duration::from_secs(4), reason: "closed".into() })));
        let st = sink.on(Stream::Status);
        let last = st.iter().rev().find(|m| m["type"] == "status").unwrap();
        assert_eq!((last["phase"].as_str(), last["stt_ok"].as_bool()), (Some("running"), Some(false)));
        assert!(last["stt"].as_str().unwrap().contains("4 s"));
        p.apply(Event::Session(Notification::Level(0.5)));
        p.apply(Event::Session(Notification::Level(0.5)));
        assert_eq!(sink.on(Stream::Status).iter().filter(|m| m["type"] == "status").count(), 1, "an unchanged status is not sent again");
        p.set_status(|s| s.phase = Phase::Stopping);
        p.set_status(|s| s.phase = Phase::StoppingNow);
        let phases: Vec<String> = sink.on(Stream::Status).iter().map(|m| m["phase"].as_str().unwrap().to_string()).collect();
        assert_eq!(phases, ["stopping", "stopping_now"]);
    }

    #[test]
    fn ten_silent_seconds_on_loopback_raise_the_warning_once() {
        let sink = Arc::new(Recorded::default());
        let mut p = Pump::new("s1".into(), sink.clone(), None, true);
        for _ in 0..12 {
            p.apply(Event::Session(Notification::Level(0.0)));
        }
        let msgs = sink.take();
        assert_eq!(msgs.iter().filter(|(_, m)| m["type"] == "notice" && m["kind"] == "warn").count(), 1);
        assert_eq!(p.mirror().status.silence, true);
        p.apply(Event::Session(Notification::Level(0.3)));
        assert_eq!(p.mirror().status.silence, false);
    }

    #[test]
    fn a_document_is_read_only_at_its_sidecar_revision() {
        let dir = tempfile::tempdir().unwrap();
        let files = LectureFiles::standard(dir.path(), chrono::NaiveDate::from_ymd_opt(2026, 9, 25).unwrap());
        std::fs::write(&files.notes, "# T\n").unwrap();
        let mut notes = lecturelive_core::session::sidecar::NotesState::default();
        notes.len = 4;
        notes.sha256 = lecturelive_core::session::notesfile::sha256_hex(b"# T\n");
        assert_eq!(read_document(&files, &notes).as_deref(), Some("# T\n"));
        std::fs::write(&files.notes, "# T\n\n<!-- 10:00:00 -->\n## A\n").unwrap(); // a commit landed after the sidecar was read
        assert_eq!(read_document(&files, &notes), None);
    }
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p desktop adapter 2>&1 | tail -5`
Expected: compile errors (`Pump`, `Sink`, `Stream`, `read_document` not found).

- [ ] **Step 3: Implement**

`wire.rs`: the types above. `SegmentView::from(&Segment)` formats `said_at` as `%H:%M:%S`, `recovered: source == Recovered`.

`adapter.rs`: `Pump` holds `session`, `seq`, `sink`, `spend`, `silence: Option<SilenceWatch>`, and `Mirror { status: Status, open: Option<OpenView>, utterance: u64, preview: Option<PreviewView>, op: u64 /* starts at 1 */, notices: VecDeque<Notice> /* last 50 */, slides: Vec<SlideView> }`. `emit(stream, msg)` increments `seq`, wraps `Envelope` and calls `sink.send`. `apply` matches each event:

| Event | Stream: message | Mirror |
|---|---|---|
| `Session(Open{stable,tentative})` | transcript `open` | `open` set; a new utterance id when there was none |
| `Session(Segment(s))`, live | transcript `closed` with the open utterance id (a new id if none was open) | `open` cleared |
| `Session(Segment(s))`, recovered or imported | transcript `segment` | unchanged |
| `Preview(d)` | notes `delta{op}` | `preview.text += d` |
| `Committed{revision,..}` | notes `committed{op,revision,block}`, notice `notes` ("12 words and 1 slide folded in, $0.02"; "transcription still catching up" when unconfirmed; the missing count when any) | preview cleared, busy cleared, `op += 1` |
| `NothingNew` / `SnapshotFailed(m)` / `Cancelled(what)` | notes `ended{op,outcome,message}`, a notice | preview cleared, busy cleared, `op += 1` |
| `Polished{revision,..}` | notes `polished{revision}`, notice `done` | busy cleared |
| `PolishStopped(m)` / `PolishFailed(m)` / `PageFailed(m)` / `Warning(m)` | notice `warn` | busy cleared (not for `Warning`) |
| `Page{outcome,usd}` | notice `page`, `folder.page` updated | busy cleared |
| `Busy(m)` | status | `busy = Some(m)` |
| `Slide{index,file}` | status `slide`, notice `slide` | `slides` pushed |
| `Session(Level(l))` | status (rounded to 1 dB, so an unchanged level sends nothing) | `level_dbfs`; `silence` from `SilenceWatch::new(-60.0, 10)` when loopback, one warn notice on the rising edge |
| `Session(Stt(s))` | status | `stt`, `stt_ok` in the CLI's words ("transcribing", "reconnecting in 4 s (closed)", "refused: …") |
| `Session(Gap(g))` | notice `warn` | `gaps += 1` when `g.kind.is_transcript() && !g.resolved` |
| `Session(Recovered(g))` | notice `done` | `gaps -= 1` (saturating) |
| `Session(DeviceGone/DeviceBack/Failed/RecoveryFailed/SpendFailed)` | notice | — |
| `Session(Recording{..}/SourceEnded)` | — | `SourceEnded` sets phase `Stopping` |

After every event, `spend_usd = spend.lecture_total()`; the status is sent whenever it differs from the last one sent. `state()` builds `SessionState` from the mirror and its arguments: `revision` from `sc.notes.revision`, pending segments `segments.len() - sc.notes.segment_cursor`, pending slides those above `sc.notes.slide_index`, slides from `sc.slides` with absolute paths (`files.dir.join(file)`). `read_document` reads the notes file and returns it only when its length and SHA-256 are `notes.len`/`notes.sha256`.

`app.rs`: `App` (Tauri managed state) holds `pump: Arc<tokio::sync::Mutex<Pump>>`, `sink: Arc<TauriSink>`, `folder: Mutex<Option<OpenFolder { files, course, name, title }>>` and `running: Mutex<Option<Running { commands: UnboundedSender<Command>, lecture: Arc<Lecture>, stops: u32, _lock: FolderLock }>>`. `TauriSink` sends status with `app.emit("status", v)` and the two streams through the channels `attach` stored; with no channel attached a message is dropped (the next hydration covers it).
- `select_folder(dir)`: refused while a session runs. Canonicalises the directory; course from `course_from_path`, else `LECTURE_COURSE` from the environment, else "Lecture"; `name` is the folder's name; `LectureFiles::standard(dir, today)`; `title = prompts::title(course, name, today)`; allows the asset scope on `files.slides` (not recursive); makes a new `Pump` with a new session id and `phase: Idle`, `folder: Some(..)`, `page` when `page::page_path(notes, name)` exists.
- `start_lecture(source)`: key from the Keychain, else a clear error naming the key dialog; microphone permission as the CLI checks it; `spend::take_over(app_ledger, CLI_LEDGER)` with `CLI_LEDGER = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../spend.jsonl")`; `FolderLock::acquire`; `launch::restore_abandoned_route`; a new `Pump` (new session id, `phase: Starting`, `source`, `started_at`); `start::prepare` with a notice per other day ("recovering 24 Sep's transcript gaps") and notices for its reports in the CLI's words; then `lecture::run` spawned with `tauri::async_runtime::spawn`, and a pump task that applies each event under the lock. When `run` returns: phase `Ended`, a `done` notice ("saved; 2 transcript gaps still to recover; the next session in this folder does it"), `running = None`. Returns the session id.
- `stop()`: `stops += 1`; sends `Command::Stop`; phase `Stopping` at 1, `StoppingNow` at 2 and above; returns the level.
- `snapshot(hint)`, `polish()`, `cancel()`: `Command::Op(Op::Snapshot(hint))`, `Command::Op(Op::Polish)`, `Command::Cancel`.
- `get_session_state()`: holds the pump lock throughout. Sidecar: `Command::State` when a session runs (falling back to the file if the reply is dropped), else `Sidecar::load`. Document: `read_document`, re-reading the sidecar and retrying up to three times; after that the file as it is (an external edit waits for the next session to be accepted). Segments: `segments::read(files.segments())` (missing log: empty).
- `open_page()`: the running lecture's `Lecture`, or one built for the open folder with the key and the folder lock; runs `Lecture::page` with its events applied to the pump (busy line and notice); on success runs `open <path>`.
- `inputs()`, `loopback_status()`: `input::list_inputs`, `loopback::status` on a blocking thread.

`canary.rs`: M0's `route_sync`, `record_sync`, `windows_sync`, `capture_sync`, `run_check`, `log_line`, `log_check`, unchanged; `lib.rs` keeps the `--check` branch in `setup` and registers the new commands only.

`tauri.conf.json`: window `title: "LectureLive"`, `width: 1440`, `height: 900`, `minWidth: 960`, `minHeight: 600`; `security.csp` set to the directives of Task 7; `security.assetProtocol: { enable: true, scope: [] }`. `identifier`, `productName` and `bundle` unchanged. `capabilities/default.json` adds `"dialog:allow-open"`.

- [ ] **Step 4: Run them to verify they pass**

Run: `cargo test -p desktop 2>&1 | command grep -E "^test result|FAILED|panicked"` and `cargo test -p lecturelive-core 2>&1 | command grep -E "^test result|FAILED"`
Expected: the seven adapter tests pass (the Keychain test ignored); core unchanged at 233 passed.

- [ ] **Step 5: Commit**

```bash
git add apps/desktop/src-tauri/src/wire.rs apps/desktop/src-tauri/src/adapter.rs apps/desktop/src-tauri/src/app.rs apps/desktop/src-tauri/src/canary.rs apps/desktop/src-tauri/src/lib.rs apps/desktop/src-tauri/Cargo.toml apps/desktop/src-tauri/tauri.conf.json apps/desktop/src-tauri/capabilities/default.json Cargo.lock
git commit -m "Adapt a running lecture to the app: sequenced status, transcript and notes streams and a coherent state"
```

---

### Task 4: Frontend foundation: tooling, tokens, wire types and transports (supports milestone task 2)

**Files:**
- Create: `apps/desktop/src/lib/{wire.ts,transport.ts,fixture.ts,theme.css}`
- Modify: `apps/desktop/package.json` (via `npm install`), `apps/desktop/vite.config.js` (vitest `test` block), `apps/desktop/src/app.html` (title "LectureLive")

**Interfaces:**
- Produces: `wire.ts` types mirroring `wire.rs` exactly (`Envelope<T> = T & { session: string; seq: number }`, `TranscriptMsg`, `NotesMsg`, `StatusMsg`, `Status`, `Phase`, `Notice`, `SlideView`, `FolderView`, `SessionState`, `SegmentView`, `OpenView`, `PreviewView`).
- Produces: `transport.ts`: `interface Transport { listenStatus(cb: (m: Envelope<StatusMsg>) => void): Promise<() => void>; attach(onTranscript: (m: Envelope<TranscriptMsg>) => void, onNotes: (m: Envelope<NotesMsg>) => void): Promise<void>; state(): Promise<SessionState>; call<T = unknown>(cmd: string, args?: Record<string, unknown>): Promise<T>; assetUrl(path: string): string }`; `tauriTransport(): Transport`.
- Produces: `fixture.ts`: `class FixtureTransport implements Transport` with `state` (a `SessionState` it serves), `emitStatus`, `emitTranscript`, `emitNotes` (each stamps `session` and the next `seq`), `calls: [string, unknown][]`, and `script(steps)` helpers used by Tasks 5, 10 and the browser preview (`?fixture=demo`).

- [ ] **Step 1: Install and configure**

```bash
cd apps/desktop
npm install marked@18.0.14 dompurify@3.4.15 @tauri-apps/plugin-dialog@2.7.3
npm install -D vitest@5.0.2 jsdom@30.1.1
```
`package.json` scripts gain `"test": "vitest run"`. `vite.config.js` gains `test: { environment: "jsdom", include: ["src/**/*.test.ts"] }` and, under `process.env.VITEST`, `resolve: { conditions: ["browser"] }` so Svelte's client runtime is used.

- [ ] **Step 2: Write the failing test** (`src/lib/fixture.test.ts`)

```ts
import { describe, expect, test } from "vitest";
import { FixtureTransport, emptyState } from "./fixture";

describe("fixture transport", () => {
  test("stamps one rising sequence across streams and records calls", async () => {
    const t = new FixtureTransport(emptyState("s1"));
    const seen: number[] = [];
    await t.listenStatus((m) => seen.push(m.seq));
    await t.attach((m) => seen.push(m.seq), (m) => seen.push(m.seq));
    t.emitTranscript({ type: "open", utterance: 1, stable: "the", tentative: "rate" });
    t.emitNotes({ type: "delta", op: 1, text: "## A" });
    t.emitStatus({ type: "notice", kind: "done", label: "saved", detail: "", at: "10:00:00" });
    expect(seen).toEqual([1, 2, 3]);
    await t.call("snapshot", { hint: "focus" });
    expect(t.calls).toEqual([["snapshot", { hint: "focus" }]]);
    expect((await t.state()).session).toBe("s1");
  });
});
```

- [ ] **Step 3: Run it to verify it fails**

Run: `cd apps/desktop && npx vitest run src/lib/fixture.test.ts`
Expected: FAIL, cannot resolve `./fixture`.

- [ ] **Step 4: Implement** `wire.ts`, `transport.ts` (`listen("status")`, two `new Channel(...)` passed to `invoke("attach", { transcript, notes })`, `invoke("get_session_state")`, `convertFileSrc`), `fixture.ts`, and `theme.css` with the tokens of the design direction above (`:root` light values, `@media (prefers-color-scheme: dark)` dark values, `html.large { font-size: 125% }`, the type scale as custom properties, the two fade keyframes, `@media (prefers-reduced-motion: reduce)` turning them off).

- [ ] **Step 5: Run it to verify it passes**

Run: `npx vitest run && npx svelte-check --tsconfig ./tsconfig.json 2>&1 | tail -3`
Expected: 1 passed; svelte-check 0 errors.

- [ ] **Step 6: Commit**

```bash
git add apps/desktop/package.json apps/desktop/package-lock.json apps/desktop/vite.config.js apps/desktop/src/app.html apps/desktop/src/lib/wire.ts apps/desktop/src/lib/transport.ts apps/desktop/src/lib/fixture.ts apps/desktop/src/lib/fixture.test.ts apps/desktop/src/lib/theme.css
git commit -m "Add the frontend's wire types, transports, design tokens and test tooling"
```

---

### Task 5: The rune store (milestone task 2)

**Files:**
- Create: `apps/desktop/src/lib/{words.ts,words.test.ts,session.svelte.ts,session.test.ts}`

**Interfaces:**
- Consumes (Task 4): `Transport`, wire types, `FixtureTransport`.
- Produces: `words.ts`: `type Word = { id: number; text: string; stable: boolean }`, `nextWords(prev: Word[], stable: string, tentative: string, fresh: () => number): Word[]`.
- Produces: `session.svelte.ts`: `class Session` with reactive fields `ready`, `status: Status`, `notices: Notice[]`, `segments: SegmentView[]`, `open: { utterance: number; words: Word[] } | null`, `revision: number`, `document: string`, `committed: string[]` (blocks appended since the document was read), `preview: { op: number; text: string } | null`, `previewShown: string` (the preview as last parsed: whole words only, at most every 100 ms), `slides: SlideView[]`, `folder: FolderView | null`; methods `init(t: Transport, opts?: { schedule?: (f: (now: number) => void) => void; now?: () => number }): Promise<void>`, `drain(now: number): void`, `setHidden(hidden: boolean): Promise<void>`, `hydrate(): Promise<void>`, `dispose(): void`, `onFrame(cb: () => void): () => void`, and the actions `snapshot(hint)`, `polish()`, `cancel()`, `stop()`, `start(source)`, `selectFolder(dir)`, `openPage()`. `export const session = new Session()`.

- [ ] **Step 1: Write the failing tests**

`words.test.ts`:

```ts
import { expect, test } from "vitest";
import { nextWords } from "./words";

const counter = () => { let n = 0; return () => ++n; };
const brief = (ws: { id: number; text: string; stable: boolean }[]) => ws.map((w) => [w.id, w.text, w.stable]);

test("an unchanged prefix keeps its ids, promotion keeps the id, replacements get new ones", () => {
  const fresh = counter();
  const a = nextWords([], "the rate", "sets", fresh);
  expect(brief(a)).toEqual([[1, "the", true], [2, "rate", true], [3, "sets", false]]);
  const b = nextWords(a, "the rate sets", "the step", fresh);
  expect(brief(b)).toEqual([[1, "the", true], [2, "rate", true], [3, "sets", true], [4, "the", false], [5, "step", false]]);
  const c = nextWords(b, "the rate sets", "the size", fresh);
  expect(c.map((w) => w.id)).toEqual([1, 2, 3, 4, 6]);
});

test("a shorter hypothesis drops the words it no longer holds", () => {
  const fresh = counter();
  const a = nextWords([], "", "one two three", fresh);
  expect(nextWords(a, "", "one two", fresh).map((w) => w.id)).toEqual([1, 2]);
});
```

`session.test.ts` (each test builds a `FixtureTransport` and a fresh `Session`; `schedule` collects frames so the test calls `drain` itself):

```ts
import { describe, expect, test } from "vitest";
import { Session } from "./session.svelte";
import { FixtureTransport, emptyState } from "./fixture";

function setup(state = emptyState("s1")) {
  const t = new FixtureTransport(state);
  const s = new Session();
  let clock = 0;
  const frames: ((now: number) => void)[] = [];
  const ready = s.init(t, { schedule: (f) => frames.push(f), now: () => clock });
  const frame = (advance = 16) => { clock += advance; const f = frames.splice(0); f.forEach((g) => g(clock)); };
  return { t, s, ready, frame, tick: (ms: number) => (clock += ms) };
}

describe("session store", () => {
  test("listeners attach before the state is read, and messages at or below its watermark are dropped", async () => {
    const st = emptyState("s1");
    st.seq = 6;
    const { t, s, ready, frame } = setup(st);
    t.holdState(); // the state answer waits until released
    await Promise.resolve();
    t.emitTranscript({ type: "segment", segment: { id: 0, at: "10:00:00", text: "old", recovered: false } }, 5);
    t.emitTranscript({ type: "segment", segment: { id: 0, at: "10:00:00", text: "new", recovered: false } }, 7);
    t.releaseState();
    await ready;
    frame();
    expect(t.order).toEqual(["listen", "attach", "state"]);
    expect(s.segments.map((x) => x.text)).toEqual(["new"]);
  });

  test("a message from another session or a repeated sequence is dropped", async () => {
    const { t, s, ready, frame } = setup();
    await ready;
    t.emitTranscript({ type: "segment", segment: { id: 0, at: "10:00:00", text: "a", recovered: false } });
    t.emitTranscript({ type: "segment", segment: { id: 1, at: "10:00:01", text: "b", recovered: false } }, undefined, "other");
    t.replayLast();
    frame();
    expect(s.segments.map((x) => x.text)).toEqual(["a"]);
  });

  test("a committed block replaces the preview; a revision already held is ignored and a jump rehydrates", async () => {
    const st = emptyState("s1");
    st.revision = 3;
    st.document = "# T\n";
    const { t, s, ready, frame } = setup(st);
    await ready;
    t.emitNotes({ type: "delta", op: 1, text: "## A" });
    frame();
    expect(s.preview?.text).toBe("## A");
    t.emitNotes({ type: "committed", op: 1, revision: 4, block: "\n<!-- 10:00:00 -->\n## A\n" });
    t.emitNotes({ type: "committed", op: 1, revision: 4, block: "\n<!-- 10:00:00 -->\n## A\n" });
    frame();
    expect(s.preview).toBeNull();
    expect(s.committed).toHaveLength(1);
    expect(s.revision).toBe(4);
    const before = t.stateReads;
    t.emitNotes({ type: "committed", op: 2, revision: 6, block: "x" });
    frame();
    await Promise.resolve();
    expect(t.stateReads).toBe(before + 1);
  });

  test("a reload mid-stream resumes the preview and the block lands once", async () => {
    const st = emptyState("s1");
    st.revision = 1;
    st.document = "# T\n";
    st.preview = { op: 2, text: "## Half an" };
    st.op = 2;
    st.seq = 10;
    const { t, s, ready, frame } = setup(st);
    t.setSeq(10);
    await ready;
    t.emitNotes({ type: "delta", op: 2, text: "swer" });
    frame();
    expect(s.preview?.text).toBe("## Half answer");
    t.emitNotes({ type: "committed", op: 2, revision: 2, block: "\n<!-- 10:00:00 -->\n## Half answer\n" });
    frame();
    expect(s.committed).toEqual(["\n<!-- 10:00:00 -->\n## Half answer\n"]);
  });

  test("a hole in segment ids rehydrates", async () => {
    const { t, s, ready, frame } = setup();
    await ready;
    t.emitTranscript({ type: "segment", segment: { id: 0, at: "10:00:00", text: "a", recovered: false } });
    t.emitTranscript({ type: "segment", segment: { id: 2, at: "10:00:02", text: "c", recovered: false } });
    const before = t.stateReads;
    frame();
    await Promise.resolve();
    expect(t.stateReads).toBe(before + 1);
    expect(s.segments.map((x) => x.id)).not.toContain(2);
  });

  test("while hidden nothing is applied; on return the store rehydrates", async () => {
    const { t, s, ready, frame } = setup();
    await ready;
    await s.setHidden(true);
    t.emitNotes({ type: "delta", op: 1, text: "## A" });
    frame();
    expect(s.preview).toBeNull();
    t.state_.preview = { op: 1, text: "## A and more" };
    t.state_.seq = t.lastSeq;
    await s.setHidden(false);
    expect(s.preview?.text).toBe("## A and more");
  });

  test("the preview is parsed at most every 100 ms however many deltas arrive", async () => {
    const { t, s, ready, frame } = setup();
    await ready;
    for (let i = 0; i < 500; i++) {
      t.emitNotes({ type: "delta", op: 1, text: "word " });
      if (i % 8 === 7) frame(16);
    }
    frame(16);
    expect(s.previewParses).toBeLessThanOrEqual(Math.ceil((63 * 16) / 100) + 1);
    expect(s.preview?.text.length).toBe(500 * 5);
  });

  test("an open utterance keeps word ids across updates and a live close clears it", async () => {
    const { t, s, ready, frame } = setup();
    await ready;
    t.emitTranscript({ type: "open", utterance: 1, stable: "the", tentative: "rate" });
    frame();
    const ids = s.open!.words.map((w) => w.id);
    t.emitTranscript({ type: "open", utterance: 1, stable: "the rate", tentative: "sets" });
    frame();
    expect(s.open!.words.slice(0, 2).map((w) => w.id)).toEqual(ids);
    t.emitTranscript({ type: "closed", utterance: 1, segment: { id: 0, at: "10:00:00", text: "the rate sets", recovered: false } });
    frame();
    expect(s.open).toBeNull();
    expect(s.segments.map((x) => x.text)).toEqual(["the rate sets"]);
  });
});
```

`FixtureTransport` gains what these use: `holdState()`, `releaseState()`, `order`, `stateReads`, `replayLast()`, `setSeq(n)`, `lastSeq`, `state_`, and an optional `seq` and `session` override on each `emit*`.

- [ ] **Step 2: Run them to verify they fail**

Run: `npx vitest run src/lib/words.test.ts src/lib/session.test.ts`
Expected: FAIL, modules not found.

- [ ] **Step 3: Implement**

`nextWords`: split both strings on whitespace; walk the new list against the old one position by position; while the text is equal keep the id (with the new stable flag), from the first difference on assign `fresh()`.

`Session`: `init` calls `listenStatus`, then `attach`, then `hydrate`; every message before hydration ends is queued; `hydrate` replaces everything from the state and sets `watermark = state.seq`, `session = state.session`, `lastSeq` per stream to the watermark; then queued messages above the watermark are replayed. `accept(stream, m)`: drop when hidden, when `m.session !== session` (except a status message carrying a phase after `start`/`selectFolder`, which trigger a hydrate themselves), when `m.seq <= lastSeq[stream]`; otherwise queue it and `schedule(drain)` once. `drain(now)`: applies the queue in order, then parses the preview if it changed and 100 ms have passed since the last parse (the text up to its last whitespace: the unfinished trailing word stays back), then calls the `onFrame` callbacks. Rules: `segment`/`closed` whose `id` is not `last id + 1` → `hydrate()` instead; `committed` at `revision == this.revision + 1` appends to `committed`, at or below is ignored, above → `hydrate()`; `polished` → `hydrate()`; `ended` clears the preview of its op; `delta` of an op older than the current preview's is ignored. `setHidden(true)` drops the queue and sets hidden; `setHidden(false)` hydrates. `document.addEventListener("visibilitychange", …)` is added in `init` when `document` exists, and removed in `dispose`; `import.meta.hot?.dispose(() => session.dispose())` sits in the module. No `$effect`.

- [ ] **Step 4: Run them to verify they pass**

Run: `npx vitest run && npx svelte-check --tsconfig ./tsconfig.json 2>&1 | tail -3`
Expected: all passed; 0 errors.

- [ ] **Step 5: Commit**

```bash
git add apps/desktop/src/lib/words.ts apps/desktop/src/lib/words.test.ts apps/desktop/src/lib/session.svelte.ts apps/desktop/src/lib/session.test.ts apps/desktop/src/lib/fixture.ts
git commit -m "Add the session rune store: attach before hydrating, sequence and revision checks, frame drain, bounded preview"
```

---

### Task 6: Transcript pane (milestone task 3)

**Files:**
- Create: `apps/desktop/src/lib/Transcript.svelte`
- Modify: `apps/desktop/src/routes/+page.svelte` (the layout, from the design direction)

**Interfaces:**
- Consumes (Task 5): `session.segments`, `session.open`, `session.onFrame`.
- Produces: `<Transcript />`; an action `pin(node)` that keeps the pane at the bottom after each frame while pinned; `pinned` state and a "Jump to live" button.

- [ ] **Step 1: Invoke `/frontend-design:frontend-design`** for the transcript pane and the page layout, with the milestone design direction above as the brief's context. Record the run in the ledger. No markup before it.
- [ ] **Step 2: Build the pane.** Closed segments: one `<p>` each, keyed by `id`, the time in graphite with tabular numbers, the text as one text node; a recovered segment carries a quiet "recovered" mark; `content-visibility: auto; contain-intrinsic-size: auto 3.2em` on each so a two-hour transcript lays out only what is visible. The open utterance: one `<span>` per word keyed by its id, class `tentative` (graphite) or stable (ink) with a 180 ms colour transition; a word's first mount runs the 180 ms fade-in. Pin to bottom: the `scroll` handler sets `pinned` when within 4 px of the end; the `pin` action registers an `onFrame` callback that sets `scrollTop = scrollHeight` while pinned. "Jump to live" appears when not pinned. Empty state: "The transcript appears here once the lecture starts."
- [ ] **Step 3: Check it** in the browser preview: `npm run dev`, then `http://localhost:1420/?fixture=demo` (the fixture transport plays a short scripted lecture) in the Playwright browser, light and dark (`emulateMedia`), normal and large type. Screenshots are read, then deleted.
- [ ] **Step 4: Run** `npx vitest run && npx svelte-check --tsconfig ./tsconfig.json 2>&1 | tail -3`. Expected: all passed, 0 errors.
- [ ] **Step 5: Commit**

```bash
git add apps/desktop/src/lib/Transcript.svelte apps/desktop/src/routes/+page.svelte apps/desktop/src/lib/fixture.ts
git commit -m "Add the transcript pane: word IDs, fade-in, collapsed closed utterances, pin to bottom"
```

---

### Task 7: Notes pane, sanitiser and CSP (milestone task 4)

**Files:**
- Create: `apps/desktop/src/lib/{markdown.ts,markdown.test.ts,Notes.svelte}`
- Modify: `apps/desktop/svelte.config.js` (`kit.csp`), `apps/desktop/src-tauri/tauri.conf.json` (`security.csp`, same directives), `apps/desktop/src/routes/+page.svelte`

**Interfaces:**
- Produces: `markdown.ts`: `type SlideCtx = { notesDir: string; slides: Set<string> /* absolute paths of registered slides */; toUrl: (abs: string) => string }`, `render(md: string, ctx: SlideCtx): string` (sanitised HTML), `chunks(doc: string): { time: string | null; md: string }[]` (split at `<!-- HH:MM:SS -->` lines, the marker dropped and its time kept), `previewBlocks(text: string): string[]` (top-level Markdown blocks through `marked.lexer`; all but the last are complete), `resolve(base: string, rel: string): string | null` (a POSIX join; null for a URL, a scheme or `//`).
- CSP directives (both places): `default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' asset: http://asset.localhost; connect-src 'self' ipc: http://ipc.localhost ws://localhost:1420 ws://127.0.0.1:1420; font-src 'self'; object-src 'none'; frame-src 'none'; base-uri 'none'; form-action 'none'` (SvelteKit adds the nonce to `script-src`).

- [ ] **Step 1: Write the failing tests** (`markdown.test.ts`, jsdom)

```ts
import { describe, expect, test } from "vitest";
import { chunks, previewBlocks, render, resolve } from "./markdown";

const ctx = { notesDir: "/L/Week 1", slides: new Set(["/L/Week 1/slides/slide_01_100203.png"]), toUrl: (p: string) => "asset://localhost/" + encodeURIComponent(p) };
const REMOTE = /^(https?:|\/\/|javascript:|data:|vbscript:|file:)/i;

function inert(html: string) {
  const root = document.createElement("div");
  root.innerHTML = html;
  for (const el of Array.from(root.querySelectorAll("*"))) {
    expect(["SCRIPT", "IFRAME", "FRAME", "OBJECT", "EMBED", "STYLE", "LINK", "META", "BASE", "FORM", "SVG", "MATH", "VIDEO", "AUDIO", "SOURCE", "INPUT", "BUTTON", "TEXTAREA"]).not.toContain(el.tagName.toUpperCase());
    for (const a of Array.from(el.attributes)) {
      expect(a.name.toLowerCase().startsWith("on"), `${el.tagName} ${a.name}`).toBe(false);
      expect(["style", "srcset", "srcdoc", "action", "formaction", "poster", "background", "ping", "xlink:href"]).not.toContain(a.name.toLowerCase());
      if (a.name === "src" || a.name === "href") expect(a.value, `${el.tagName} ${a.name}`).not.toMatch(REMOTE);
    }
  }
  return root;
}

describe("rendered markdown", () => {
  test("hostile markdown renders inert", () => {
    const hostile = [
      "<script>alert(1)</script>",
      '<img src=x onerror="alert(1)">',
      "![remote](https://evil.example/a.png)",
      "![data](data:image/svg+xml;base64,PHN2Zz48L3N2Zz4=)",
      "[js](javascript:alert(1)) [web](https://evil.example) [proto](//evil.example/x)",
      '<iframe src="https://evil.example"></iframe><object data="https://evil.example"></object><embed src="https://evil.example">',
      '<svg><script>alert(1)</script><image href="https://evil.example/a.png"/></svg><math><mi>x</mi></math>',
      '<style>@import url(https://evil.example/a.css);</style><link rel="stylesheet" href="https://evil.example/a.css">',
      '<div style="background:url(https://evil.example/a.png)">x</div>',
      '<meta http-equiv="refresh" content="0;url=https://evil.example"><base href="https://evil.example/">',
      '<form action="https://evil.example"><button formaction="https://evil.example">go</button><input value=1></form>',
      '<video src="https://evil.example/v.mp4" poster="https://evil.example/p.png"></video><img srcset="https://evil.example/a.png 1x">',
      "![Slide 1](../../../etc/passwd.png) ![Slide 2](slides/slide_02_999999.png)",
      '<a href="https://evil.example" ping="https://evil.example">x</a><details open ontoggle="alert(1)">d</details>',
    ].join("\n\n");
    const root = inert(render(hostile, ctx));
    expect(root.querySelectorAll("img")).toHaveLength(0);
  });

  test("a registered slide renders through the asset protocol and keeps its alt text", () => {
    const root = inert(render("## Rates\n\n![Slide 1](slides/slide_01_100203.png)\n\n- a [link](#rates)", ctx));
    const img = root.querySelector("img")!;
    expect(img.getAttribute("src")).toBe(ctx.toUrl("/L/Week 1/slides/slide_01_100203.png"));
    expect(img.getAttribute("alt")).toBe("Slide 1");
    expect(root.querySelector("a")!.getAttribute("href")).toBe("#rates");
    expect(root.querySelector("h2")!.textContent).toBe("Rates");
  });

  test("the document splits at its snapshot markers and keeps their times", () => {
    const doc = "# T\n\n<!-- 10:02:03 -->\n## A\n- a\n\n<!-- 10:07:40 -->\n## B\n";
    expect(chunks(doc)).toEqual([{ time: null, md: "# T\n\n" }, { time: "10:02:03", md: "## A\n- a\n\n" }, { time: "10:07:40", md: "## B\n" }]);
  });

  test("the preview splits into finished blocks and the one still growing", () => {
    expect(previewBlocks("## A\n- one\n- two\n\nSome par")).toEqual(["## A\n", "- one\n- two\n\n", "Some par"]);
  });

  test("paths resolve under the notes folder and URLs do not resolve", () => {
    expect(resolve("/L/Week 1", "slides/a.png")).toBe("/L/Week 1/slides/a.png");
    expect(resolve("/L/Week 1", "../x/../Week 1/slides/a.png")).toBe("/L/Week 1/slides/a.png");
    expect(resolve("/L/Week 1", "slides/slide%2001.png")).toBe("/L/Week 1/slides/slide 01.png");
    for (const u of ["https://a/b.png", "//a/b.png", "data:x", "asset://localhost/x"]) expect(resolve("/L", u)).toBeNull();
  });
});
```

- [ ] **Step 2: Run them to verify they fail**

Run: `npx vitest run src/lib/markdown.test.ts`. Expected: FAIL, module not found.

- [ ] **Step 3: Implement `markdown.ts`.** `marked` with `gfm: true`, synchronous. DOMPurify with `USE_PROFILES: { html: true }`, the forbidden tags and attributes of the test, `ALLOW_DATA_ATTR: false`, `RETURN_DOM_FRAGMENT: true`. Then one walk over the fragment: an `img` keeps its `src` only when `resolve(notesDir, src)` is in `slides`, rewritten to `toUrl(abs)`, and is removed otherwise; an `a` keeps an `href` only when it starts with `#`; every other `src`, `href`, `xlink:href` or `action` is removed. Serialise through a container's `innerHTML`.
- [ ] **Step 4: Run them to verify they pass.** `npx vitest run`. Expected: all passed.
- [ ] **Step 5: Invoke `/frontend-design:frontend-design`** for the notes pane (committed document, snapshot times in the margin, the preview's teal rule and "writing" mark, the empty state). Record the run. No markup before it.
- [ ] **Step 6: Build `Notes.svelte`.** Committed: `chunks(session.document)` plus `session.committed`, each rendered once into `{ key, time, html }` and kept in a `$state.raw` list keyed by revision and position; a new block appends one rendered chunk, a hydrate re-renders the list. Preview: `previewBlocks(session.previewShown)`; finished blocks render once, keyed by index, and fade in; the last block re-renders with each parse. The slide context comes from `session.slides` and `session.folder.notes_dir`, with `toUrl = transport.assetUrl`. Empty state: "Notes appear here after the first snapshot."
- [ ] **Step 7: Set the CSP.** `svelte.config.js` `kit.csp = { mode: "auto", directives: { … } }` and `tauri.conf.json` `security.csp`, both with the directives above. Check the dev header: `curl -sI http://localhost:1420/ | command grep -i content-security`.
- [ ] **Step 8: Check it** in the browser preview (`?fixture=demo`, which includes a slide and a streaming snapshot), light and dark, normal and large type. Screenshots are read, then deleted.
- [ ] **Step 9: Run** `npx vitest run && npx svelte-check --tsconfig ./tsconfig.json 2>&1 | tail -3`. Expected: all passed, 0 errors.
- [ ] **Step 10: Commit**

```bash
git add apps/desktop/src/lib/markdown.ts apps/desktop/src/lib/markdown.test.ts apps/desktop/src/lib/Notes.svelte apps/desktop/src/routes/+page.svelte apps/desktop/svelte.config.js apps/desktop/src-tauri/tauri.conf.json apps/desktop/src/lib/fixture.ts
git commit -m "Add the notes pane with a frozen committed render, a throttled preview, a sanitiser and a strict CSP"
```

---

### Task 8: Control bar, folder picker and key dialog (milestone tasks 5 and 6)

**Files:**
- Create: `apps/desktop/src/lib/{StatusStrip.svelte,CommandLine.svelte,FolderPicker.svelte,KeyDialog.svelte}`
- Modify: `apps/desktop/src-tauri/src/app.rs` (`key_status`, `save_key`, `import_key_from_env`), `apps/desktop/src/routes/+page.svelte`, `apps/desktop/src/lib/session.svelte.ts` (actions), `apps/desktop/src/lib/session.test.ts`

**Interfaces:**
- Consumes: the commands of Task 3; `@tauri-apps/plugin-dialog` `open({ directory: true })`.
- Produces: `Session.stopLabel: "Stop" | "Stop waiting" | null` (derived from the phase: running → "Stop", stopping → "Stop waiting", stopping_now/ended/idle → null), `Session.canSnapshot` (running or stopping with no stop-now), `Session.busyOp` (status.busy is a snapshot or polish), `Session.elapsed` (seconds since `status.started_at`, updated by a one-second `setInterval` started in `init` and cleared in `dispose`).

- [ ] **Step 1: Write the failing test** (appended to `session.test.ts`)

```ts
test("the command line maps to the CLI's operations and the stop button to its levels", async () => {
  const { t, s, ready, frame } = setup();
  await ready;
  t.emitStatus({ type: "status", ...t.state_.status, phase: "running" });
  frame();
  expect(s.stopLabel).toBe("Stop");
  await s.snapshot("");
  await s.snapshot("  focus on momentum  ");
  await s.polish();
  await s.cancel();
  await s.stop();
  expect(t.calls).toEqual([["snapshot", { hint: "" }], ["snapshot", { hint: "focus on momentum" }], ["polish", undefined], ["cancel", undefined], ["stop", undefined]]);
  t.emitStatus({ type: "status", ...t.state_.status, phase: "stopping" });
  frame();
  expect(s.stopLabel).toBe("Stop waiting");
  t.emitStatus({ type: "status", ...t.state_.status, phase: "stopping_now" });
  frame();
  expect(s.stopLabel).toBeNull();
});
```
(A typed `polish` sends `Op::Polish`, as the CLI's `polish ⏎` does: `snapshot(hint)` sends `polish` when the trimmed hint is `polish`, case-insensitive. The test's last lines check that the hint is trimmed.)

- [ ] **Step 2: Run it to verify it fails.** `npx vitest run src/lib/session.test.ts`. Expected: FAIL (`stopLabel` undefined).
- [ ] **Step 3: Implement** the actions and derived fields; `key_status`, `save_key` (trimmed, non-empty; stored with `keychain::set`), `import_key_from_env` (`keychain::env_key()` into the Keychain; the key never reaches the frontend) in `app.rs`.
- [ ] **Step 4: Run it to verify it passes.** `npx vitest run`. Expected: all passed.
- [ ] **Step 5: Invoke `/frontend-design:frontend-design`** for the status strip, the command line, the folder picker and the key dialog. Record the run. No markup before it.
- [ ] **Step 6: Build them.** Status strip: phase in words ("Ready", "Starting", "Listening", "Stopping: finishing the transcript and recovery, then a last snapshot", "Stopping without waiting for recovery", "Stopped"), source with the meter (a bar from −60 to 0 dBFS, teal, signal when the silence warning is on), elapsed, STT state, open transcript gaps (signal when above 0), spend today, and the large-type toggle ("Aa", stored in `localStorage`, wrapped in try/catch). Folder picker: shown when no folder is open, or as the folder name in the strip that reopens the dialog when idle; the source select (inputs, plus "Zoom through LectureLive Loopback" when BlackHole is present; "Install BlackHole 2ch (brew install blackhole-2ch)" otherwise) and Start. Command line: the ◆ prompt and hint field (⏎ snapshot, `polish` ⏎ polish), Snapshot, Cancel (only while a snapshot or polish runs), Polish, Study page, and the Stop button with its label. Notices: the last few, with the CLI's marks. Key dialog: opens when `key_status().stored` is false or from the strip; a password field and "Save to Keychain", and "Use the key in .env" when `env_available`; the field is cleared after saving and the key is never kept in the store.
- [ ] **Step 7: Check it** in the browser preview, light and dark, normal and large type, including the stopping states and the key dialog. Screenshots are read, then deleted.
- [ ] **Step 8: Run** `npx vitest run && npx svelte-check --tsconfig ./tsconfig.json 2>&1 | tail -3 && cargo test -p desktop 2>&1 | command grep -E "^test result|FAILED"`. Expected: all pass.
- [ ] **Step 9: Commit**

```bash
git add apps/desktop/src/lib/StatusStrip.svelte apps/desktop/src/lib/CommandLine.svelte apps/desktop/src/lib/FolderPicker.svelte apps/desktop/src/lib/KeyDialog.svelte apps/desktop/src/lib/session.svelte.ts apps/desktop/src/lib/session.test.ts apps/desktop/src/routes/+page.svelte apps/desktop/src-tauri/src/app.rs apps/desktop/src/lib/fixture.ts
git commit -m "Add the control bar: folder, source and meter, the CLI's command line, the stop levels and the key dialog"
```

---

### Task 9: Spend view (milestone task 7)

**Files:**
- Create: `apps/desktop/src/lib/SpendView.svelte`
- Modify: `apps/desktop/src-tauri/src/app.rs` (`spend_summary`), `apps/desktop/src/lib/{wire.ts,fixture.ts}`, `apps/desktop/src/routes/+page.svelte`

**Interfaces:**
- Consumes (Task 1): `spend::{take_over, read, summary, Summary}`.
- Produces: command `spend_summary() -> Summary` (takes over the CLI ledger first, then reads the app ledger); `wire.ts` `SpendSummary`.

- [ ] **Step 1: Write the failing test** (`app.rs`, test module; the command's body is a plain function `spend_summary_at(app_ledger, cli_ledger) -> Result<Summary>`):

```rust
#[test]
fn the_spend_view_takes_over_the_cli_ledger_first() {
    let dir = tempfile::tempdir().unwrap();
    let (app, cli) = (dir.path().join("app/spend.jsonl"), dir.path().join("spend.jsonl"));
    let at = chrono::NaiveDate::from_ymd_opt(2026, 9, 25).unwrap().and_hms_opt(10, 0, 0).unwrap();
    std::fs::write(&cli, spend::line(at, "ML", "Week 01", spend::SpendKind::Page, 0.25, true, None)).unwrap();
    let s = spend_summary_at(&app, &cli).unwrap();
    assert_eq!((s.calls, s.months.len()), (1, 1));
    assert_eq!(s.recent[0].lecture, "Week 01");
}
```

- [ ] **Step 2: Run it to verify it fails.** `cargo test -p desktop the_spend_view 2>&1 | tail -3`. Expected: compile error.
- [ ] **Step 3: Implement** `spend_summary_at` and the command.
- [ ] **Step 4: Run it to verify it passes.**
- [ ] **Step 5: Invoke `/frontend-design:frontend-design`** for the spend view (a sheet over the panes: all-time total, the last three months with a bar per course, recent lectures with their kinds, and the share estimated from published rates). Record the run. No markup before it.
- [ ] **Step 6: Build it**, opened from the status strip's spend figure; empty state "Nothing spent yet. Every paid request is logged from the next lecture on." Check it in the browser preview (fixture summary), light and dark. Screenshots are read, then deleted.
- [ ] **Step 7: Run** `npx vitest run && npx svelte-check --tsconfig ./tsconfig.json 2>&1 | tail -3 && cargo test -p desktop 2>&1 | command grep -E "^test result|FAILED"`.
- [ ] **Step 8: Commit**

```bash
git add apps/desktop/src/lib/SpendView.svelte apps/desktop/src-tauri/src/app.rs apps/desktop/src/lib/wire.ts apps/desktop/src/lib/fixture.ts apps/desktop/src/routes/+page.svelte
git commit -m "Add the spend view from the ledger: months by course, recent lectures by kind, the estimated share"
```

---

### Task 10: Frame-time measurement (gate: 500-delta/s burst and two-hour fixtures)

**Files:**
- Create: `apps/desktop/src/lib/bench.ts`
- Modify: `apps/desktop/src/lib/fixture.ts` (the two fixtures), `apps/desktop/src/routes/+page.svelte` (bench start-up), `apps/desktop/src-tauri/src/app.rs` (`check_config`, `check_report`, `exit_app`)

**Interfaces:**
- Produces: `fixture.ts`: `burstFixture(): { state: SessionState; run(t: FixtureTransport, done: () => void): void }` (10 s at 500 notes deltas per second, word-sized, a heading every 40 words; an open-utterance update every 500 ms and a closed segment every 5 s), `twoHourFixture()` (hydrates 1,440 segments of 12–14 words and a 6,000-word document in 60 chunks, then plays 30 s of live transcript and one snapshot's deltas at 60 per second, then a committed block).
- Produces: `bench.ts`: `measure(session: Session): { stop(): Report }` (registers an `onFrame` hook that timestamps the frame's apply, `flushSync()` and one `scrollHeight` read of each pane; `Report { name, frames, p50, p95, max, over_16_7 }`).
- Produces: commands `check_config() -> Option<{ mode, dir }>` from `LECTURELIVE_CHECK` / `LECTURELIVE_CHECK_DIR`; `check_report(name, json)` writes `~/Library/Application Support/LectureLive/m4-checks/<name>.json`; `exit_app()`.

- [ ] **Step 1: Write the failing test** (`bench.test.ts`, jsdom: the measurement itself, not the numbers)

```ts
import { expect, test } from "vitest";
import { percentile } from "./bench";

test("percentiles are nearest-rank over the samples", () => {
  const xs = Array.from({ length: 100 }, (_, i) => i + 1);
  expect([percentile(xs, 50), percentile(xs, 95), percentile(xs, 100)]).toEqual([50, 95, 100]);
  expect(percentile([3], 95)).toBe(3);
});
```

- [ ] **Step 2: Run it to verify it fails**, then implement `bench.ts`, the fixtures and the commands; run again. Expected: passes.
- [ ] **Step 3: Measure in WKWebView.**

```bash
cd apps/desktop
LECTURELIVE_CHECK=bench-burst npm run tauri dev    # runs the burst fixture, writes m4-checks/bench-burst.json, exits
LECTURELIVE_CHECK=bench-twohour npm run tauri dev
cat "$HOME/Library/Application Support/LectureLive/m4-checks/bench-"*.json
```
Expected: p95 below 16.7 ms in both. Each run is repeated three times and all three are recorded. If a p95 is above the line, the ledger records it, the cause is found with the Web Inspector's timeline, fixed, and measured again; the gate line holds only on a measured pass.

- [ ] **Step 4: Commit**

```bash
git add apps/desktop/src/lib/bench.ts apps/desktop/src/lib/bench.test.ts apps/desktop/src/lib/fixture.ts apps/desktop/src/routes/+page.svelte apps/desktop/src-tauri/src/app.rs
git commit -m "Measure frame work in the app's webview on a 500-delta/s burst and a two-hour lecture"
```

---

### Task 11: Checks in the running app (gate: rehydration, hint/cancel/polish, sanitiser and CSP)

**Files:**
- Create: `apps/desktop/src/lib/checks.ts`
- Modify: `apps/desktop/src/routes/+page.svelte`, `apps/desktop/src-tauri/src/app.rs` (`hide_window_for`)

**Interfaces:**
- Produces: `checks.ts`: `runCheck(mode: "csp" | "live", session: Session, transport: Transport)`, each writing a report through `check_report` and ending with `exit_app`. Progress across a reload is kept in `sessionStorage`.
- Produces: command `hide_window_for(ms: u64)` (hides the main window, shows it again after `ms`).

- [ ] **Step 1: `csp` check** (`LECTURELIVE_CHECK=csp npm run tauri dev`). In the real webview: records `securitypolicyviolation` events for 3 s while it (a) appends `<img src="http://192.0.2.1/m4.png">`, (b) sets a container's `innerHTML` to `<img src="x" onerror="window.__m4=1">` without the sanitiser, (c) calls `fetch("http://192.0.2.1/m4")`, (d) renders the hostile Markdown of `markdown.test.ts` through `render` into the notes pane and runs the same `inert` walk; then reports the violations, `window.__m4 === undefined`, and the walk's result. Expected: three violations (`img-src`, `script-src-attr` or `script-src`, `connect-src`), no handler ran, the walk found nothing.
- [ ] **Step 2: `live` check** on a synthetic folder, `~/Library/Application Support/LectureLive/m4-live/Weeks/Week 01 — Optimisation` (course "m4-live" from the folder above `Weeks`). The key is in the Keychain (`import_key_from_env` first). While `say -a "BlackHole 2ch"` reads a synthetic lecture text in a background loop, the check: selects the folder, starts with loopback, waits for three segments; takes a hinted snapshot ("focus on the learning rate") and waits for `committed`; starts a snapshot and cancels it after its first delta, and checks the revision is unchanged and the notice says cancelled; takes another snapshot; records a view digest (segment ids and texts in the transcript pane, the notes pane's text, the revision) and reloads the page; after hydration compares: every segment and every committed chunk before the reload is present, and the view equals a fresh `get_session_state`; calls `hide_window_for(4000)` while a snapshot streams, logs whether `visibilitychange` fired, and on return compares the view with a fresh state again; runs polish and waits for `polished` and the re-fetch (revision + 1 and a backup notice); stops, then presses Stop waiting; reports every step with its time and the notices. The run's default output is read before and after (`SwitchAudioSource -c` if installed, else `system_profiler SPAudioDataType`) and must match.
- [ ] **Step 3: Open study page**, once, on the same folder from the app's button after the live check: records the page request's time and cost, and that the page opened in the default browser. (This is the one page request of the milestone; about $0.10–0.30.)
- [ ] **Step 4: Record** each report under `m4-checks/`, the spend of each request from the ledger (`spend.jsonl` lines of course "m4-live"), and the ledger's Rulings for any step that behaved otherwise.
- [ ] **Step 5: Commit**

```bash
git add apps/desktop/src/lib/checks.ts apps/desktop/src/routes/+page.svelte apps/desktop/src-tauri/src/app.rs
git commit -m "Add in-app checks for the CSP, rehydration after reload and hiding, and hint, cancel and polish"
```

---

### Task 12: Spec §3.6 and §9 to what is built

**Files:**
- Modify: `docs/spec.md` §3.6 (transport, messages, sequence and session rules, revision rule, `get_session_state`, the state source while stopping) and §9 (§9.1 control bar with the stop levels and Cancel, §9.2 lifecycle, §9.3 rendering and the CSP as built, §9.4 the design direction).

- [ ] **Step 1:** Rewrite both sections as the current design (no "previously" trails). Everything they state is in the code or the Findings.
- [ ] **Step 2: Commit**

```bash
git add docs/spec.md
git commit -m "Rewrite spec 3.6 and 9 to the adapter and panes as built"
```

---

### Task 13: Review, findings and handover

**Files:**
- Modify: `docs/milestones.md` (M4 section: gate ticks and Findings; Status row)

- [ ] **Step 1:** Run and record: `cargo test -p lecturelive-core`, `cargo test -p desktop`, `npx vitest run`, `npx svelte-check` (pass, fail, ignored counts from their output).
- [ ] **Step 2:** A fresh reviewer on `opus` reviews the whole branch (`git diff main..m4-desktop-panes`), given this plan's Review Focus verbatim and the ledger's Ruling lines. Re-grade its findings by their effect on the person using the app; fix Critical and Important findings test-first.
- [ ] **Step 3:** Findings in the M4 section: resolved versions of new crates and npm packages; the adapter as built; frame-time measurements; sanitiser and CSP evidence; rehydration evidence; the `/frontend-design:frontend-design` runs, one per view; live spend; failed lines with their §14.1 pointer; open threads with owners. Tick the gate lines that held; Status `done` only if every line holds, else `in progress`; link the plan in the Status table.
- [ ] **Step 4: Commit** (the branch stays unmerged and unpushed)

```bash
git add docs/milestones.md
git commit -m "Record M4 findings"
```
