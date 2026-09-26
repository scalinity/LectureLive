# M5 Slide Automation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Slides reach the notes without the person noticing each change. A core capture worker watches a chosen region of Zoom's window once a second, and a tile detector turns each settled change into a slide. Every slide, whether auto, from the Capture button, from the global shortcut or dragged in, goes through one registration path. The app gains a slides strip with badges and a window and region picker. A window that is covered, minimised or replaced never makes the app capture from another window without asking.

**Architecture:** Core gains a `capture` worker on its own thread, because its native calls and image encoding block. Each second it looks the bound window up by id, captures it with `xcap`, crops the region and shrinks it to 256×144 grayscale, and feeds a pure `Detector` (candidate, confirm, animated-tile mask, expiry). A kept frame becomes a PNG temp file in `slides/`, and `lecture::run` registers it through `session::slides::register`, the one path shared with the screenshot watcher and with dropped files. The window is named by a `Descriptor` (bundle id, title, size) that is saved per course with the region (as fractions of the window) and revalidated when the lecture starts. Once bound, a vanished, replaced or resized window pauses capture and asks: the worker never binds on its own to a window the person did not choose in this session. The desktop adapter adds commands for listing windows, a preview, the selection, manual capture and import, a global shortcut, and a `capture` part of the status. The frontend replaces the plain rail with the slides strip and adds the picker dialog.

**Tech Stack:** Rust 1.96.1; `xcap 0.9.8` (capture), CoreGraphics `CGWindowListCopyWindowInfo` through `core-foundation 0.10.1` (enumeration with the on-screen flag), `objc2-app-kit 0.3.2` (`NSRunningApplication`, bundle id; already in `Cargo.lock` through `xcap`), `image 0.25.10`, `tauri 2.11.6`, `tauri-plugin-global-shortcut 2.3.2` (new); `svelte 5.57.1`, `@tauri-apps/api 2.11.1`, `vitest 5.0.2`, `jsdom 30.1.1`.

**Spec:** `docs/spec.md` §7 (all), §3.2 (workers), §3.5 (`Slide`), §8 (`slides/`, sidecar `slides`), §9.1 (slides strip), §9.3 (asset scope), §9.4 (visual design), §10 row "Microphone or Screen Recording denied", §11 (detector on recorded frames). Gate: `docs/milestones.md` → M5. Evidence: the M0 and M4 Findings in `docs/milestones.md` and the plan research below.

**Plan research (2026-09-25, before this plan was written).**

| Question | Evidence | Consequence |
|---|---|---|
| Drag-and-drop in `tauri 2.11.6` / `@tauri-apps/api 2.11.1` | `tauri-runtime-2.11.3/src/window.rs:97` `DragDropEvent { Enter { paths, position }, Over { position }, Drop { paths, position }, Leave }`. JS: `getCurrentWebview().onDragDropEvent(cb)` (`webview.d.ts:413`), payload `{type: "enter" \| "over" \| "drop" \| "leave", paths, position}`, built on `tauri://drag-enter/over/drop/leave`. `dragDropEnabled` defaults to true (`tauri-utils-2.9.3/src/config.rs:2301`), so HTML5 `drop` events carry no file paths; the native event does. Listening needs `core:event:allow-listen`, which `core:default` grants. | The frontend listens with `onDragDropEvent`, shows a drop hint on `enter`, and on `drop` calls `import_slides(paths)`. The in-app check calls the same command with a synthetic image (an OS drag cannot be scripted); the real drag is a step of the one sitting. |
| Global shortcut for tauri 2.11 | `tauri-plugin-global-shortcut` 2.3.2 (3.0 is an alpha); depends on `tauri 2.10+` and `global-hotkey 0.8`. Rust: `Builder::new().with_handler(Fn(&AppHandle, &Shortcut, ShortcutEvent)).build()`, then `app.global_shortcut().register(s)` / `unregister(s)`; `ShortcutEvent.state` is `ShortcutState::Pressed \| Released`. npm `@tauri-apps/plugin-global-shortcut` 2.3.2 needs `@tauri-apps/api ^2.11.0`. | Registered from Rust only while a lecture runs (it takes the key combination from every other app while registered), so the npm package and a capability are not needed. The combination is ⌘⇧2: macOS uses ⌘⇧3/4/5/6 for screenshots and Zoom uses ⌘⇧A/V/S/T/R/C/P/N/H/M/F. When registering fails (another app holds it), a warning notice names the Capture button. |
| Screen Recording for `target/debug/desktop` launched from this shell | A preflight-only probe (`CGPreflightScreenCaptureAccess`, no request) returned `true`. `./target/debug/desktop --check windows` then listed other apps' windows with their titles (Terminal, ChatGPT, Chrome, Zoom's login window). The process chain is `zsh → claude → zsh → login → Terminal.app`, so TCC charges Terminal.app, which holds the grant. | Dev runs launched from this shell list and capture Zoom without their own grant. The permission path (`list_windows` erring distinctly, the strip's fix-it line) is tested against a fake source; the packaged canary keeps M0's evidence for a real grant. |
| `xcap 0.9.8` cost at 1 s cadence | A Preview window showing a synthetic 1920×1080 slide, captured natively at 2264×1380, 20 samples at 1 s. **Optimised:** list+find p50 1.3 ms, `capture_image` p50 16.1 / p95 19.8 / max 28.1 ms, grayscale+256×144 5.0 ms, fit to 1600 px + PNG 26.8 ms. **Debug profile (opt-level 0):** `capture_image` 133 ms, grayscale+256×144 661 ms, fit+PNG 1954 ms. | About 21 ms of work per second optimised, but the debug build cannot keep a 1 s cadence. The workspace optimises `image` and `xcap` in the dev profile (`[profile.dev.package.*] opt-level = 3`), and the detector's own loops stay on 256×144 pixels. |
| What xcap enumerates, and how it captures | `ImplWindow::all()` uses `CGWindowListCopyWindowInfo(OptionOnScreenOnly \| ExcludeDesktopElements)`: a minimised window, or one on another desktop, is absent. `capture_image` is `CGWindowListCreateImage(bounds, OptionIncludingWindow, id, Default)`: the window's own buffer, so other windows on top of it do not appear in the image. | Enumeration goes through `CGWindowListCopyWindowInfo(OptionAll \| ExcludeDesktopElements)` directly, so a window that is off screen is found with `kCGWindowIsOnscreen = false` rather than read as closed. Capture stays on `xcap`. Occlusion is expected to leave captures unchanged; the live check shows it. |
| Bundle id | xcap exposes only the owner name and pid. `objc2-app-kit 0.3.2` has `NSRunningApplication::runningApplicationWithProcessIdentifier(pid)` and `bundleIdentifier()`; it is already compiled into the build through `xcap`. | The descriptor names the app by bundle id when it has one (Zoom: `us.zoom.xos`); a binary outside a bundle (the dev app) falls back to the owner name. |
| A Zoom window showing a shared slide with one person | Zoom never shows a sharer their own share, so a meeting window with a shared slide needs a second participant. | The one sitting joins the person's own meeting a second time from Zoom's web client in Chrome, shares a Chrome tab with the synthetic deck (`localhost:1420/deck`), and the Zoom app's meeting window shows it as a lecture would. |
| AppleScript | `osascript -e 'tell application "Preview" …'` from this shell raised "“Terminal” wants access to control “Preview”" and hung; the dialog stays until answered. | No check scripts another app. Windows a check needs are its own (a second Tauri window). Answering that dialog ("Don't Allow") is the first item of the one sitting. |

**Settled here (the prompt asked for these to be decided, not inherited):**
- *Where capture runs.* A core capture worker (`capture::worker`) on its own thread beside the session: it owns the native calls and the image work (spec §3.2), and it sends temp files and states to an async task in `lecture::run`, which registers each file through the one path. The worker runs only while a lecture runs. The picker's list and preview run on blocking threads from the adapter at any time.
- *One registration path.* `session::slides::register(files, store, path, meta)` moves the file into `slides/` under the CLI's name inside the sidecar's writer. It is idempotent by file: a file already in the sidecar is not registered again, so the screenshot watcher, which lists `slides/`, never takes a slide another path already registered. `import` copies dropped files to a temp name first, so the person's originals stay where they were.
- *The descriptor and region* are saved per course in `$HOME/Library/Application Support/LectureLive/capture.json` (`{course: {descriptor, region}}`): one course's Zoom layout is the same every week. The region is fractions of the window (x, y, w, h), so it holds at any capture scale. When the lecture starts, an exact match of bundle id, title and size (within 2%) binds. Anything else asks, in the strip, with the candidates.
- *Once bound, nothing binds without the person.* A closed window pauses capture. A new window matching the descriptor makes the strip ask ("Watch it"), with the new window's size. A window that is off screen (minimised, or on another desktop) pauses and resumes by itself on the same id. A resize asks, because Zoom re-lays out the slide and the region may no longer frame it. Three failed captures in a row say so. None of these rebinds.
- *The badge data.* `SlideEntry` gains `auto: bool` and `uncertain: bool`, both `#[serde(default, skip_serializing_if = "is_false")]`. Older sidecars load as manual and certain (they hold screenshots), and a sidecar re-saved without these slides is byte-identical. `shown_at` stays the first-observed time.
- *Manual capture* (the button and ⌘⇧2) captures the bound region now, registers it as manual, and makes it the detector's kept frame, so auto capture does not take the same slide again. The screenshot folder (⌘⇧4) and dropped images are manual too.
- *The fixture format.* `crates/core/tests/fixtures/slides/<name>/`: `frames/NNNNN.png`, the distinct 256×144 grayscale detector inputs (exact duplicates are stored once); `samples.jsonl`, one line per sample (`{"t": ms, "frame": n}` or `{"t": ms, "error": "…"}`); `states.json`, the annotated stable states (`{"id", "from", "to", "kind"}` in ms of the recording). The live worker writes the first two when `WorkerConfig.record` names a folder, so a fixture holds exactly what the live detector saw. A replay of it must reproduce the live decisions.
- *Annotation and calibration.* The synthetic deck (`apps/desktop/src/lib/deck.json`, played by the route `/deck`) is a fixed schedule. Its states are aligned to a recording by the one offset that best matches the recording's large frame changes to the schedule's slide boundaries (an ignored test writes `states.json`). A state counts for recall when it is visible for at least 3 s. A capture is false when it lands on a state already captured or on no state. Thresholds start at spec §7.2's values (0.08, 0.03, 3 samples, 10 s). They are changed only if the recorded fixture fails at those values, and then only to values that also pass the synthetic lecture. Spec §7.2 and §14.3 record the result.

**Visual design** (the milestone-level `/frontend-design:frontend-design` run, 2026-09-25, before any M5 view is built; each view's task invokes it again):
- *Subject and job.* The strip is a record in time beside the notes. At arm's length it answers whether the slide on Zoom is being watched, what was captured last and when, and whether something needs the person. The picker binds "that window, that rectangle" once per course, and later asks when the window no longer matches.
- *Tokens.* M4's, unchanged: paper, ink, graphite, rule, plate, signal, teal, light and dark after the system; Atkinson Hyperlegible Next else the system face; 18 px at 1.2; tabular numbers in the text face; radius 6 px on controls only.
- *Strip.* 18 rem wide from 1280 px, 16 rem from 1100 px, folded away below. A binding line sits on top. While the window is watched, it carries the teal rule (live) and reads "Watching Zoom Meeting", with the app and region size in graphite below and "Capture" beside the quiet hint "⌘⇧2". A pause is a graphite sentence. An ask is a plain question in ink with one filled button ("Watch it") and "Choose…". A failure or missing permission is signal-red words with its fix ("Open Settings"). Below it, each slide is a row with its time in the hanging gutter (tabular, graphite) and its badge word under the time ("auto", "manual", "unsettled" for uncertain, whose tooltip says it was still changing after 10 s). The thumbnail is flat with a 1 px rule and fills the rest of the width. The newest slide is at the end, and the strip follows it while the reader is at the end, as the panes do. A new thumbnail fades in once (the arrival carries meaning), and `prefers-reduced-motion` turns that off. While files are dragged over the window, the list shows a dashed teal outline and "Drop images to add them as slides" ("Start the lecture to add slides" when none runs). The empty state keeps "Screenshots you take during the lecture become slides." and adds "Zoom's slides are added as they change." once a window is bound.
- *Picker.* A native `<dialog>`, `min(64rem, 100vw − 2rem)`. On the left is a flat list of windows: app in ink, title and size in graphite, rules between rows, the chosen row on a teal left rule. On the right, the preview dominates. The person drags over it to draw the region, and dragging again redraws it. Outside the region is dimmed with the scrim, and the region is the one teal outline. Under it: "Drag over the slide; leave out Zoom's controls and the video tiles." and "Region 1180 × 664 of 1600 × 900" in tabular numbers. The actions are "Cancel" and "Watch this region" (primary). When the saved window no longer matches, the first line says why ("Zoom Meeting is 1280 × 800 now; you chose it at 1600 × 900 for Machine Learning."), and the saved region is drawn on the new preview so that one click accepts it.
- *Reviewed against the brief.* Pill badges became words in the gutter. A traffic-light status dot became the teal rule the app already uses for "live". The camera icon on Capture became plain text with the shortcut beside it. "zoom.us · 1600×900" meta strings became separate lines. Figma-style corner handles became redraw-by-drag with a size readout.

```
┌ status strip ─────────────────────────────────────────────────────────────────────────────┐
│ transcript │ notes                                  ▎Watching Zoom Meeting                │
│            │                                        ▎zoom.us, region 1180 × 664           │
│            │                                        ▎[Capture]  ⌘⇧2         Choose…       │
│            │                                        ──────────────────────────────────────│
│            │                                         10:02:51 ┌───────────────────────┐   │
│            │                                         auto     │ thumbnail             │   │
│            │                                                  └───────────────────────┘   │
│            │                                         10:05:12 ┌───────────────────────┐   │
│            │                                         unsettled│                       │   │
└ command line ─────────────────────────────────────────────────────────────────────────────┘
```

**Execution:** executing-plans, inline, without approval stops. The human steps are one sitting (Task 11), scheduled after everything that can finish without the person. A fresh reviewer on the most capable model (`opus`) reviews the whole branch before the findings commit (Task 14). Ledger: `.superpowers/sdd/2026-09-25-m5-slide-automation/progress.md` (git-excluded through `.git/info/exclude`).

**The one sitting (Task 11), listed up front so it can be planned for; about five minutes of the person's time, then about 25 unattended minutes:**
1. Answer "Don't Allow" on "“Terminal” wants access to control “Preview”" (left by the plan research).
2. Zoom: sign in if needed; New Meeting with camera and microphone off.
3. Chrome: open `http://localhost:1420/deck`; open the meeting's invite link in another tab, choose "Join from your browser" with camera and microphone off, then Share → Chrome tab → the deck tab.
4. In the LectureLive dev window (already open on the synthetic folder `m5-live`): Choose… in the slides strip, choose Zoom's meeting window, drag over the shared slide, Watch this region.
5. Say "ready"; when told, click Start on the deck tab and leave Zoom's window where it is for about 25 minutes (the machine can be left).
6. During those minutes, once: press ⌘⇧2, and drag the file `$HOME/Library/Application Support/LectureLive/m5-live/drop-me.png` from Finder onto the LectureLive window.
7. Optional, for M4's open item: look at the app in light and dark and with large type.

## Global Constraints

- macOS 13+, Apple Silicon; one user; no telemetry, accounts or servers.
- Toolchain pinned to `rustc 1.96.1` (`rust-toolchain.toml`); workspace `rust-version` 1.89.
- Builds authorised: `cargo build`, `test`, `run`, `check`, `tree`, `add` (debug profile). Frontend, in `apps/desktop`: `npm install` (and `npm install <package>` for `@tauri-apps/plugin-global-shortcut` and test tooling), `npm run dev`, `npm run tauri dev`, `npm test` / `npx vitest run`, `npx svelte-check`. Not authorised without asking: `npm run build`, `npm run tauri build`, any packaging, signing or notarising, anything that writes under `target/release`, and `cargo clean`.
- The packaged "LectureLive Canary.app" in `target/release/bundle` holds this Mac's Microphone and Screen Recording grants, tied to its ad-hoc signature. It is never rebuilt, re-signed, moved or cleaned without asking. `tauri.conf.json` changes only in `app` and `security`; `identifier`, `productName` and `bundle` stay as they are; the canary's `--check` mode stays in the app's source. Dev runs build `target/debug/desktop`.
- **Every task that creates or changes a view invokes `/frontend-design:frontend-design` before any markup is written** (spec §9.4): the slides strip and its badges, the window and region picker, the ask and permission lines, the synthetic deck page. Work within the design direction above (M4's palette light and dark, Atkinson Hyperlegible Next else the system face, 18 px base at ratio 1.2, flat ruled columns, a hanging "when" gutter, a teal rule for what is live, no decorative motion). A view built before the skill ran is rebuilt after it. Each run is recorded in the ledger and in the Findings.
- Svelte 5 runes only: `$state`, `$state.raw`, `$derived`, event-driven updates. No `$effect` or any other effect hook; DOM wiring that needs an element uses an action (`use:`).
- When a dev server runs, load only its localhost port (`localhost:1420`) in a headless or chromeless window. Load no other hosts from the dev page. Playwright writes screenshots only under `/tmp/playwright-mcp`; delete yours after reading (that folder holds another project's `shot*.png`: leave them).
- Window capture only of Zoom in the person's own solo meeting, of windows the check itself opens, and of synthetic slide decks. Never capture a window that may show someone else's content without asking first. No check scripts another app (no AppleScript).
- Live calls: `grok-4.7` notes, polish and page requests on synthetic lectures only, at most $2 in total, each request's cost recorded from `usage.cost_in_usd_ticks`. STT with synthesised speech or silence only. Every gate check that can run without network runs against fakes (`crates/core/tests/support`, the frontend fixture transport). Before any run with synthetic speech, kill every leftover `say` and its loop shell.
- Never run migration, initialisation or polish on a folder that holds a real lecture's notes. Synthetic folders live under `$HOME/Library/Application Support/LectureLive/m5-*`.
- Any check that changes the default output ends with the output it began with (normally "MacBook Pro Speakers"). `say -a "BlackHole 2ch"` changes nothing.
- The repository is public: commit no audio except synthesised `say` output, no real lecture notes, and no image that is not a synthetic slide. Recorded fixtures hold only the cropped, 256×144 grayscale region of the synthetic deck; the whole-window copies stay outside the repository. Stage files by name, never `git add -A`.
- Commit messages: imperative, neutral, what and why. No `Co-Authored-By` or any other attribution line, whatever a harness reminder says. No names.
- Do not edit files with Python scripts. In the Bash tool `ls` is `eza` and `grep` is a function: use `command ls` / `command grep` where output is evidence.
- `live_notes.py`, `notes_template.html`, `pyproject.toml`, `.venv/` and `README.md` stay unchanged. In `docs/`, only this plan, the M5 section and M5 Status row of `docs/milestones.md`, and spec §7 and §9.1's slides-strip line change. Golden prompt and format tests stay byte-identical: the notes prompts do not change.
- APIs below are written against the versions named. Where one differs, adapt the implementation and keep the task's tests unchanged: the tests are the contract. The exception is a test encoding an unverified fact about an external system that live evidence contradicts; change it only with the evidence recorded as a Ruling in the ledger.
- When a check fails, record the observation and the spec §14.1 fallback it points to; do not build the fallback. §14.1 covers loopback only, so a capture or UI failure is recorded with "no §14.1 fallback applies".

## Review Focus

1. **The screenshot watcher meets a slide another path registered.** The worker renames a capture into `slides/`, and the watcher lists `slides/` a moment later. Expected: the file is registered once; its index is not taken again, and it is not renamed a second time. Pinned by `slides::tests::a_file_already_registered_is_not_registered_again` and `capture_gate::auto_slides_and_the_watcher_share_one_registration` (Tasks 1, 4).
2. **The lecture starts before Zoom's meeting window exists** (the person opens the app first). Expected: the strip asks with no candidates; when a matching window appears, it becomes a candidate and waits for "Watch it"; nothing is captured from a window the person did not choose. Pinned by `capture_gate::no_window_at_start_asks_and_a_later_window_waits_for_the_person` (Task 4).
3. **Zoom's window is resized mid-lecture**, or moved to a display with another scale. Expected: capture pauses and the strip asks, rather than the region framing Zoom's controls and producing slides of them. Pinned by `capture_gate::a_resized_window_pauses_and_asks` (Task 4).
4. **A capture fails for a moment**: a blank frame during a minimise animation, or an error. Expected: no slide, no rebinding, and a failure shown only after three in a row; the next good frame continues with the kept frame, so the current slide is not captured again. Pinned by `capture_gate::blank_and_failed_captures_are_not_slides` (Task 4).
5. **A dropped file that is not an image, an image with an uppercase extension, or a file dropped twice.** Expected: the non-image is refused by name in a notice; `.JPG` registers as `.jpg`; the original stays where the person had it; a second drop of the same file is a second slide, because the person asked twice. Pinned by `slides::tests::import_copies_images_and_refuses_the_rest` (Task 1).

## File Structure

| File | Responsibility | Task |
|---|---|---|
| `Cargo.toml` (root) | Dev-profile optimisation of `image` and `xcap` | 2 |
| `crates/core/src/session/sidecar.rs` | `SlideEntry.auto`, `SlideEntry.uncertain` | 1 |
| `crates/core/src/session/slides.rs` | The one registration path: `register`, `import`, `SlideMeta`, image helpers | 1 |
| `crates/core/src/session/lecture.rs` | Watcher on `register`; `Event::Slide` badges; `Event::Capture`; `Command::{CaptureNow, Bind, Import}`; capture wiring in `run` | 1, 4 |
| `crates/core/src/capture/detect.rs` | `Region`, `thumb`, `Detector`, `Thresholds`, `Decision` | 2 |
| `crates/core/src/capture/window.rs` | `WindowInfo`, `CaptureError`, `WindowSource`, `SystemWindows` (CG enumeration, bundle id, xcap capture) | 3 |
| `crates/core/src/capture/select.rs` | `Descriptor`, `Selection`, `revalidate`, `Selections` (per course, `capture.json`) | 3 |
| `crates/core/src/capture/worker.rs` | The capture worker: ticks, binding states, manual capture, saving, recording | 4 |
| `crates/core/tests/support/windows.rs` | `FakeWindows`: a scripted window source | 4 |
| `crates/core/tests/capture_gate.rs` | Occlusion, minimisation, replacement, resize, failures, manual, import, notes embeds | 4 |
| `crates/core/tests/support/frames.rs` | Fixture format, synthetic lecture, evaluation, annotation | 5 |
| `crates/core/tests/slides_gate.rs` | Detector gate on the synthetic lecture and on recorded Zoom fixtures | 5, 12 |
| `crates/core/tests/fixtures/slides/<name>/` | Recorded Zoom fixture | 12 |
| `crates/cli/src/main.rs` | New `run` argument; badges in the slide line | 1, 4 |
| `apps/desktop/src-tauri/{Cargo.toml,src/lib.rs}` | Global-shortcut plugin; command registration | 6 |
| `apps/desktop/src-tauri/src/{app.rs,adapter.rs,wire.rs}` | Capture commands, selection per course, status, shortcut, import, checks | 6, 10 |
| `apps/desktop/src/lib/{wire.ts,transport.ts,fixture.ts,session.svelte.ts}` | Capture status, slide badges, drops, commands | 7 |
| `apps/desktop/src/lib/{markdown.ts,Notes.svelte}` | Chunk keys that change only for chunks with an image (M4 minor M8) | 7 |
| `apps/desktop/src/lib/SlidesStrip.svelte` | Slides strip | 8 |
| `apps/desktop/src/lib/WindowPicker.svelte` | Window and region picker; the ask | 9 |
| `apps/desktop/src/routes/+page.svelte` | Strip and picker in the layout; check modes | 8–10 |
| `apps/desktop/src/lib/deck.json`, `apps/desktop/src/routes/deck/+page.svelte` | Synthetic deck: schedule and page | 10 |
| `apps/desktop/src/lib/checks.ts` | `captureCheck`, `zoomCheck` | 10 |
| `docs/spec.md` §7, §9.1 strip line | As built | 13 |
| `docs/milestones.md` M5 section + Status row | Gate, Findings, Status | 14 |

---

### Task 1: Slide badges and the one registration path (milestone task 3, first half)

**Files:**
- Create: `crates/core/src/session/slides.rs`
- Modify: `crates/core/src/session/{mod.rs,sidecar.rs,lecture.rs,folder.rs}`, `crates/core/src/notes/timeline.rs` (test constructor), `crates/core/tests/lecture_gate.rs` (constructor), `crates/cli/src/main.rs`

**Interfaces:**
- Produces (`session::sidecar`): `SlideEntry { index: u32, file: String, shown_at: DateTime<Local>, auto: bool, uncertain: bool }`, the two new fields `#[serde(default, skip_serializing_if = "is_false")]`.
- Produces (`session::slides`):
  - `pub struct SlideMeta { pub shown_at: DateTime<Local>, pub auto: bool, pub uncertain: bool }`, `SlideMeta::manual(shown_at)`;
  - `pub fn is_image(p: &Path) -> bool` (png, jpg, jpeg, any case);
  - `pub fn file_time(p: &Path) -> Result<DateTime<Local>>`;
  - `pub async fn register(files: &LectureFiles, store: &Store, path: &Path, meta: SlideMeta) -> Result<Option<SlideEntry>>`: moves `path` into `slides/` as `slide_NN_HHMMSS.<ext>` (ext lower-cased; a `.tmp` suffix is dropped), shrinks it to 1600 px when larger, and records it, all inside `Store::update`; `None` when the file is already registered;
  - `pub async fn import(files: &LectureFiles, store: &Store, paths: &[PathBuf]) -> Vec<(PathBuf, Result<SlideEntry, String>)>`: copies each image to `slides/.import-<uuid>.<ext>.tmp`, then `register` with its original file time.
- Produces (`session::lecture`): `Event::Slide { index: u32, file: String, auto: bool, uncertain: bool, shown_at: DateTime<Local> }`.

- [ ] **Step 1: Write the failing tests** in `crates/core/src/session/slides.rs` (`#[cfg(test)] mod tests`) and `sidecar.rs`:

```rust
// sidecar.rs tests
#[test]
fn slide_badges_default_to_manual_and_certain_and_are_not_written_when_unset() {
    let at = Local.with_ymd_and_hms(2026, 9, 25, 10, 5, 12).unwrap();
    let old = r#"{"version":2,"recordings":[],"gaps":[],"slides":[{"index":1,"file":"slides/slide_01_100512.png","shown_at":"2026-09-25T10:05:12+02:00"}]}"#;
    let sc: Sidecar = serde_json::from_str(old).unwrap();
    assert!(!sc.slides[0].auto && !sc.slides[0].uncertain, "an M4 sidecar's slides are screenshots");
    let manual = SlideEntry { index: 1, file: "slides/a.png".into(), shown_at: at, auto: false, uncertain: false };
    assert!(!serde_json::to_string(&manual).unwrap().contains("auto"));
    let auto = SlideEntry { auto: true, uncertain: true, ..manual };
    let back: SlideEntry = serde_json::from_str(&serde_json::to_string(&auto).unwrap()).unwrap();
    assert_eq!(back, auto);
}
```

```rust
// slides.rs tests
use super::*;
use crate::session::coordinator::Store;
use crate::session::files::LectureFiles;
use crate::session::sidecar::Sidecar;
use chrono::TimeZone;

fn setup() -> (tempfile::TempDir, LectureFiles, Store) {
    let dir = tempfile::tempdir().unwrap();
    let files = LectureFiles::standard(dir.path(), chrono::NaiveDate::from_ymd_opt(2026, 9, 25).unwrap());
    std::fs::create_dir_all(&files.slides).unwrap();
    let store = Store::offline(Sidecar::default(), files.sidecar());
    (dir, files, store)
}

fn png(path: &Path) {
    image::RgbImage::from_pixel(64, 36, image::Rgb([200, 200, 200])).save_with_format(path, image::ImageFormat::Png).unwrap();
}

#[tokio::test]
async fn register_moves_names_and_records_with_the_badges() {
    let (_d, files, store) = setup();
    let tmp = files.slides.join(".capture-1.png.tmp");
    png(&tmp);
    let at = Local.with_ymd_and_hms(2026, 9, 25, 10, 2, 51).unwrap();
    let s = register(&files, &store, &tmp, SlideMeta { shown_at: at, auto: true, uncertain: true }).await.unwrap().unwrap();
    assert_eq!((s.index, s.file.as_str(), s.auto, s.uncertain), (1, "slides/slide_01_100251.png", true, true));
    assert!(!tmp.exists() && files.slides.join("slide_01_100251.png").exists());
    assert_eq!(store.read().await.unwrap().slides, vec![s]);
}

#[tokio::test]
async fn a_file_already_registered_is_not_registered_again() {
    let (_d, files, store) = setup();
    let tmp = files.slides.join(".capture-1.png.tmp");
    png(&tmp);
    let s = register(&files, &store, &tmp, SlideMeta::manual(Local::now())).await.unwrap().unwrap();
    let again = register(&files, &store, &files.dir.join(&s.file), SlideMeta::manual(Local::now())).await.unwrap();
    assert_eq!(again, None, "the watcher listing slides/ finds the worker's file already registered");
    assert_eq!(store.read().await.unwrap().slides.len(), 1);
    assert!(files.dir.join(&s.file).exists(), "not renamed a second time");
}

#[tokio::test]
async fn paths_registering_at_once_never_share_an_index() {
    let (_d, files, store) = setup();
    let mut tasks = Vec::new();
    for i in 0..8 {
        let (files, store) = (files.clone(), store.clone());
        let p = files.slides.join(format!(".capture-{i}.png.tmp"));
        png(&p);
        tasks.push(tokio::spawn(async move { register(&files, &store, &p, SlideMeta::manual(Local::now())).await.unwrap().unwrap().index }));
    }
    let mut got: Vec<u32> = futures_util::future::join_all(tasks).await.into_iter().map(Result::unwrap).collect();
    got.sort();
    assert_eq!(got, (1..=8).collect::<Vec<_>>());
}

#[tokio::test]
async fn import_copies_images_and_refuses_the_rest() {
    let (d, files, store) = setup();
    let outside = d.path().join("Desktop");
    std::fs::create_dir_all(&outside).unwrap();
    let (a, b, text) = (outside.join("Board.JPG"), outside.join("chart.png"), outside.join("notes.txt"));
    image::RgbImage::from_pixel(64, 36, image::Rgb([10, 10, 10])).save_with_format(&a, image::ImageFormat::Jpeg).unwrap();
    png(&b);
    std::fs::write(&text, "not an image").unwrap();
    let out = import(&files, &store, &[a.clone(), text.clone(), b.clone(), b.clone()]).await;
    let ok: Vec<_> = out.iter().filter_map(|(_, r)| r.as_ref().ok()).collect();
    assert_eq!(ok.len(), 3, "{out:?}");
    assert!(ok[0].file.ends_with(".jpg"), "{}", ok[0].file);
    assert!(ok.iter().all(|s| !s.auto && !s.uncertain));
    assert!(out[1].1.as_ref().unwrap_err().contains("notes.txt"));
    assert!(a.exists() && b.exists(), "the originals stay where they were");
    assert_eq!(ok[1].index + 1, ok[2].index, "a second drop of the same file is a second slide");
    assert!(std::fs::read_dir(&files.slides).unwrap().flatten().all(|e| !e.file_name().to_string_lossy().ends_with(".tmp")));
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p lecturelive-core slides:: sidecar::tests::slide_badges 2>&1 | tail -15`
Expected: FAIL to compile (`slides` module, `SlideMeta`, `SlideEntry.auto` do not exist).

- [ ] **Step 3: Implement.** `sidecar.rs`: add the fields, and `fn is_false(b: &bool) -> bool { !*b }`. Create `slides.rs` with `is_image`, `file_time`, `move_file` and `shrink_if_large` moved from `lecture.rs`, and:

```rust
pub async fn register(files: &LectureFiles, store: &Store, path: &Path, meta: SlideMeta) -> Result<Option<SlideEntry>> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = Path::new(name.trim_end_matches(".tmp")).extension().and_then(|e| e.to_str()).unwrap_or("png").to_ascii_lowercase();
    let (files, p) = (files.clone(), path.to_path_buf());
    store
        .update(move |sc| {
            if sc.slides.iter().any(|s| files.dir.join(&s.file) == p) {
                return Ok(None);
            }
            let index = next_slide_index(&files, sc)?;
            let dest = files.slides.join(format!("slide_{index:02}_{}.{ext}", meta.shown_at.format("%H%M%S")));
            if p != dest {
                move_file(&p, &dest)?;
            }
            shrink_if_large(&dest);
            let entry = SlideEntry { index, file: relative(&files, &dest), shown_at: meta.shown_at, auto: meta.auto, uncertain: meta.uncertain };
            sc.slides.push(entry.clone());
            Ok(Some(entry))
        })
        .await
}
```

`import` checks `is_image` (error `"{name} is not a PNG or JPEG image"`), reads `file_time` of the original, copies to `slides/.import-<uuid>.<ext>.tmp`, then registers; a failed copy removes the temp file. `folder.rs` `slide_files` builds entries with `auto: false, uncertain: false`. In `lecture.rs`, `slide_watcher` calls `slides::register(…, SlideMeta::manual(file_time(&p)?))` and skips `None`; `Event::Slide` carries `auto`, `uncertain` and `shown_at`. The CLI's slide line appends " (auto)" or " (auto, still changing)".

- [ ] **Step 4: Run the tests and the whole core suite**

Run: `cargo test -p lecturelive-core 2>&1 | command grep -E "^test result|FAILED|panicked" | head -20`
Expected: every `test result: ok`, including `lecture_gate` (the watcher's slide is still embedded once).

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/session/slides.rs crates/core/src/session/mod.rs crates/core/src/session/sidecar.rs crates/core/src/session/lecture.rs crates/core/src/session/folder.rs crates/core/src/notes/timeline.rs crates/core/tests/lecture_gate.rs crates/cli/src/main.rs
git commit -m "Register every slide through one path that records auto and uncertain, and never registers a file twice"
```

---

### Task 2: The change detector (milestone task 2, detector)

**Files:**
- Create: `crates/core/src/capture/detect.rs`
- Modify: `crates/core/src/capture/mod.rs`, `Cargo.toml` (root: dev-profile optimisation)

**Interfaces:**
- Produces:
  - `pub const W: u32 = 256; pub const H: u32 = 144; pub const TILE: u32 = 16; pub const TILES: usize = 144;`
  - `#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)] pub struct Region { pub x: f64, pub y: f64, pub w: f64, pub h: f64 }` (fractions of the window), `Region::WHOLE`, `Region::is_valid(&self) -> bool`, `Region::pixels(&self, width: u32, height: u32) -> Option<(u32, u32, u32, u32)>`;
  - `pub fn thumb(img: &RgbaImage, region: &Region) -> Option<GrayImage>` (None for zero size or an all-black crop);
  - `pub fn crop(img: &RgbaImage, region: &Region) -> Option<RgbaImage>` (full resolution, for saving);
  - `#[derive(Debug, Clone, Copy, PartialEq)] pub struct Thresholds { pub change: f32, pub settle: f32, pub animated: u32, pub expire: u32 }`, `Default` = `{0.08, 0.03, 3, 10}`;
  - `#[derive(Debug, Clone, PartialEq)] pub struct Decision<T> { pub shown_at: T, pub uncertain: bool }`;
  - `pub struct Detector<T>` with `new(Thresholds)`, `observe(&mut self, at: T, frame: GrayImage) -> Option<Decision<T>>`, `keep(&mut self, frame: GrayImage)`, `masked(&self) -> usize`.

- [ ] **Step 1: Write the failing tests** in `detect.rs`. Frames are built directly at 256×144:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use image::Luma;

    fn page() -> GrayImage { GrayImage::from_pixel(W, H, Luma([245])) }
    /// A dark bar over tiles: a bullet line, a build.
    fn with_bar(mut f: GrayImage, x0: u32, y0: u32, w: u32, h: u32, v: u8) -> GrayImage {
        for y in y0..y0 + h { for x in x0..x0 + w { f.put_pixel(x, y, Luma([v])); } }
        f
    }
    fn run(d: &mut Detector<u32>, frames: &[GrayImage]) -> Vec<(u32, Decision<u32>)> {
        frames.iter().enumerate().filter_map(|(i, f)| d.observe(i as u32, f.clone()).map(|x| (i as u32, x))).collect()
    }

    #[test]
    fn the_first_frame_is_kept_and_a_settled_build_is_confirmed_on_the_next_sample() {
        let a = page();
        let b = with_bar(a.clone(), 16, 32, 120, 6, 40); // one bullet line
        let got = run(&mut Detector::new(Thresholds::default()), &[a.clone(), a.clone(), b.clone(), b.clone(), b.clone()]);
        assert_eq!(got, vec![(0, Decision { shown_at: 0, uncertain: false }), (3, Decision { shown_at: 2, uncertain: false })]);
    }

    #[test]
    fn a_change_that_reverts_before_it_settles_is_not_a_slide() {
        let a = page();
        let flash = with_bar(a.clone(), 0, 0, W, H, 20);
        let got = run(&mut Detector::new(Thresholds::default()), &[a.clone(), flash, a.clone(), a.clone()]);
        assert_eq!(got.len(), 1, "only the first frame: {got:?}");
    }

    #[test]
    fn a_looping_animation_is_masked_and_neither_triggers_nor_blocks() {
        let a = page();
        let spin = |k: u32| with_bar(page(), 200 + (k % 3) * 16, 100, 16, 16, 30); // moves every sample
        let mut frames = vec![a.clone()];
        for k in 0..8 { frames.push(spin(k)); }
        let mut d = Detector::new(Thresholds::default());
        assert_eq!(run(&mut d, &frames).len(), 1, "the animation alone captures nothing");
        assert!(d.masked() > 0);
        // A build elsewhere while it loops is still confirmed.
        let build = |k: u32| with_bar(spin(k), 16, 32, 120, 6, 40);
        let got: Vec<_> = (8..11).filter_map(|k| d.observe(k, build(k))).collect();
        assert_eq!(got, vec![Decision { shown_at: 8, uncertain: false }]);
        assert_eq!(d.masked(), 0, "masks last until the next confirmed slide");
    }

    #[test]
    fn a_candidate_that_never_settles_is_kept_uncertain_after_the_expiry() {
        let a = page();
        // A large area changing every sample, faster than the mask can settle it: a video.
        let video = |k: u32| with_bar(page(), 0, 0, W, H, (40 + (k * 37) % 150) as u8);
        let mut frames = vec![a];
        for k in 0..12 { frames.push(video(k)); }
        let t = Thresholds { animated: 100, ..Thresholds::default() }; // no masking, to reach the expiry
        let got = run(&mut Detector::new(t), &frames);
        assert_eq!(got.last().unwrap(), &(11, Decision { shown_at: 1, uncertain: true }), "{got:?}");
    }

    #[test]
    fn a_small_change_below_the_threshold_is_ignored_and_a_kept_frame_moves_the_reference() {
        let a = page();
        let cursor = with_bar(a.clone(), 100, 60, 2, 3, 20); // a pointer
        let mut d = Detector::new(Thresholds::default());
        assert_eq!(run(&mut d, &[a.clone(), cursor.clone(), cursor.clone()]).len(), 1);
        let b = with_bar(a.clone(), 16, 32, 120, 6, 40);
        d.keep(b.clone()); // a manual capture of b
        assert_eq!(d.observe(3, b.clone()), None);
        assert_eq!(d.observe(4, b), None, "b is the kept frame: not captured again");
    }

    #[test]
    fn thumb_crops_the_region_and_rejects_black_and_empty_regions() {
        let mut img = RgbaImage::from_pixel(1000, 600, image::Rgba([250, 250, 250, 255]));
        for y in 0..600 { for x in 0..200 { img.put_pixel(x, y, image::Rgba([0, 0, 0, 255])); } } // a black sidebar
        let t = thumb(&img, &Region { x: 0.2, y: 0.0, w: 0.8, h: 1.0 }).unwrap();
        assert_eq!(t.dimensions(), (W, H));
        assert!(t.pixels().all(|p| p.0[0] > 200), "the sidebar is outside the region");
        assert!(thumb(&img, &Region { x: 0.0, y: 0.0, w: 0.2, h: 1.0 }).is_none(), "all black");
        assert!(thumb(&img, &Region { x: 0.5, y: 0.5, w: 0.0, h: 0.5 }).is_none(), "zero size");
        assert_eq!(Region { x: 0.25, y: 0.5, w: 0.5, h: 0.5 }.pixels(1000, 600), Some((250, 300, 500, 300)));
        assert_eq!(Region { x: 0.9, y: 0.9, w: 0.5, h: 0.5 }.pixels(1000, 600), Some((900, 540, 100, 60)), "clamped inside");
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p lecturelive-core capture::detect 2>&1 | tail -5`
Expected: FAIL to compile (`detect` module missing).

- [ ] **Step 3: Implement** `detect.rs` as follows (spec §7.2):
  - `tile_diffs(a, b) -> [f32; TILES]` is the mean absolute difference per 16×16 tile, over the raw buffers, divided by 255.
  - `observe`:
    - With no reference yet, the frame becomes the reference and the previous frame, and the call returns `Decision { shown_at: at, uncertain: false }`.
    - Otherwise it computes `d_ref` and `d_prev`. A tile with `d_ref > change && d_prev > settle` increments its unsettled count, and at `animated` the tile is masked; any other tile resets its count to 0. `changed` is the unmasked tiles with `d_ref > change`.
    - Without a candidate: a non-empty `changed` starts one `{frame, since: at, age: 0}`.
    - With a candidate:
      - an empty `changed` drops it;
      - otherwise, if every unmasked tile changed now or in the candidate has `diff(frame, candidate) < settle`, it is confirmed. The frame becomes the reference, masks and counts clear, and the call returns `Decision { shown_at: since, uncertain: false }`;
      - otherwise `age += 1` and the candidate's frame becomes this frame. At `age >= expire`, it is kept as `Decision { shown_at: since, uncertain: true }`, with the same reset.
    - `prev = frame` in every case.
  - `keep(frame)` sets the reference and the previous frame, clears masks and counts, and drops the candidate.
  - `thumb`: `crop` (the region in pixels, via `imageops::crop_imm`) → `to_luma8` → `resize(W, H, Triangle)`; `None` when every pixel is below 8.
  - Root `Cargo.toml`:

```toml
# The capture worker shrinks a window capture every second (spec §7.2). Unoptimised, `image` and
# `xcap` take about 0.8 s of each second in a debug build; optimised, about 21 ms (M5 plan research).
[profile.dev.package.image]
opt-level = 3

[profile.dev.package.xcap]
opt-level = 3
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p lecturelive-core capture:: 2>&1 | command grep -E "^test |test result"`
Expected: the six detector tests and the three M0 window tests pass (one ignored).

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/core/src/capture/mod.rs crates/core/src/capture/detect.rs
git commit -m "Add the slide change detector: tiles against the kept frame, confirm on settling, mask animations, keep an unsettled change after 10 s"
```

---

### Task 3: Windows, descriptors and revalidation (milestone task 1, core)

**Files:**
- Create: `crates/core/src/capture/select.rs`
- Modify: `crates/core/src/capture/{mod.rs,window.rs}`, `crates/core/Cargo.toml` (`objc2-app-kit`, `objc2-foundation` as direct dependencies at the versions in `Cargo.lock`), `apps/desktop/src-tauri/src/canary.rs`, `crates/cli/src/main.rs` (the canary's use of `WindowInfo`)

**Interfaces:**
- Produces (`capture::window`):
  - `#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)] pub struct WindowInfo { pub id: u32, pub app: String, pub bundle_id: Option<String>, pub title: String, pub width: u32, pub height: u32, pub on_screen: bool }` (width and height in points);
  - `#[derive(Debug, Clone, PartialEq)] pub enum CaptureError { Denied, Failed(String) }`, with `Display` (Denied: "Screen Recording is off for LectureLive: System Settings → Privacy & Security → Screen & System Audio Recording");
  - `pub trait WindowSource: Send { fn windows(&mut self) -> Result<Vec<WindowInfo>, CaptureError>; fn capture(&mut self, id: u32) -> Result<RgbaImage, CaptureError>; }`;
  - `pub struct SystemWindows;` implementing it (normal-layer windows of other processes and this one, at least 100×100 points, on screen or not);
  - `list_windows()` and `capture_window()` keep their M0 behaviour for the canary, built on `SystemWindows`.
- Produces (`capture::select`):
  - `#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)] pub struct Descriptor { pub bundle_id: Option<String>, pub app: String, pub title: String, pub width: u32, pub height: u32 }` with `of(&WindowInfo)`, `same_app`, `same_size` (within 2% each way), `matches` (same app, same title, same size), `label() -> String` (the title, else the app);
  - `#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)] pub struct Selection { pub descriptor: Descriptor, pub region: Region }`;
  - `#[derive(Debug, Clone, PartialEq)] pub enum Revalidation { Match(WindowInfo), Ask { reason: String, candidates: Vec<WindowInfo> } }`, `pub fn revalidate(sel: &Selection, windows: &[WindowInfo]) -> Revalidation`;
  - `#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)] pub struct Selections(pub BTreeMap<String, Selection>)` with `load(path) -> Result<Self>` (missing file: empty), `save(&self, path) -> Result<()>` (atomic), `get(&self, course) -> Option<&Selection>`, `set(&mut self, course, Selection)`.

- [ ] **Step 1: Write the failing tests** in `select.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn win(id: u32, bundle: &str, title: &str, w: u32, h: u32) -> WindowInfo {
        WindowInfo { id, app: "zoom.us".into(), bundle_id: Some(bundle.into()), title: title.into(), width: w, height: h, on_screen: true }
    }
    fn saved() -> Selection {
        Selection { descriptor: Descriptor::of(&win(1, "us.zoom.xos", "Zoom Meeting", 1600, 900)), region: Region { x: 0.1, y: 0.1, w: 0.8, h: 0.8 } }
    }

    #[test]
    fn exactly_one_matching_window_binds() {
        let ws = [win(7, "us.zoom.xos", "Zoom Workplace", 900, 600), win(42, "us.zoom.xos", "Zoom Meeting", 1610, 895)];
        assert_eq!(revalidate(&saved(), &ws), Revalidation::Match(ws[1].clone()), "within 2% is the same size");
    }

    #[test]
    fn a_mismatch_asks_with_the_app_s_windows_and_says_why() {
        let resized = [win(42, "us.zoom.xos", "Zoom Meeting", 1280, 800)];
        let Revalidation::Ask { reason, candidates } = revalidate(&saved(), &resized) else { panic!() };
        assert!(reason.contains("1280 × 800") && reason.contains("1600 × 900"), "{reason}");
        assert_eq!(candidates, resized.to_vec());

        let other_title = [win(7, "us.zoom.xos", "Zoom Workplace", 900, 600)];
        let Revalidation::Ask { reason, candidates } = revalidate(&saved(), &other_title) else { panic!() };
        assert!(reason.contains("Zoom Meeting"), "{reason}");
        assert_eq!(candidates.len(), 1);

        let Revalidation::Ask { reason, candidates } = revalidate(&saved(), &[win(9, "com.google.Chrome", "Slides", 1600, 900)]) else { panic!() };
        assert!(reason.contains("zoom.us") && candidates.is_empty(), "{reason}");

        let two = [win(42, "us.zoom.xos", "Zoom Meeting", 1600, 900), win(43, "us.zoom.xos", "Zoom Meeting", 1600, 900)];
        let Revalidation::Ask { reason, candidates } = revalidate(&saved(), &two) else { panic!() };
        assert!(reason.contains("2 "), "{reason}");
        assert_eq!(candidates.len(), 2);
    }

    #[test]
    fn a_window_outside_a_bundle_is_named_by_its_app() {
        let dev = WindowInfo { bundle_id: None, app: "desktop".into(), ..win(5, "", "LectureLive deck", 1280, 720) };
        let sel = Selection { descriptor: Descriptor::of(&dev), region: Region::WHOLE };
        assert_eq!(revalidate(&sel, &[dev.clone()]), Revalidation::Match(dev));
    }

    #[test]
    fn selections_are_kept_per_course_and_a_missing_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("capture.json");
        assert_eq!(Selections::load(&path).unwrap(), Selections::default());
        let mut s = Selections::default();
        s.set("Machine Learning", saved());
        s.save(&path).unwrap();
        let back = Selections::load(&path).unwrap();
        assert_eq!(back.get("Machine Learning"), Some(&saved()));
        assert_eq!(back.get("Statistics"), None);
    }
}
```

and in `window.rs`, an ignored live test:

```rust
/// Needs Screen Recording (inherited from Terminal.app when run from its shell):
/// cargo test -p lecturelive-core system_windows -- --ignored
#[test]
#[ignore]
fn system_windows_lists_this_session_s_terminal_with_its_bundle_id() {
    let ws = SystemWindows.windows().unwrap();
    assert!(ws.iter().any(|w| w.bundle_id.as_deref() == Some("com.apple.Terminal")), "{:?}", ws.iter().map(|w| (&w.app, &w.bundle_id)).collect::<Vec<_>>());
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p lecturelive-core capture::select 2>&1 | tail -5`
Expected: FAIL to compile.

- [ ] **Step 3: Implement.**
  - `SystemWindows::windows`:
    - `ensure_screen_access()` first, mapped to `CaptureError::Denied`.
    - Enumerate with `CGWindowListCopyWindowInfo(kCGWindowListOptionAll | kCGWindowListExcludeDesktopElements, 0)` (declared in the existing `extern "C"` block). Read the dictionaries with `core-foundation`: `kCGWindowNumber`, `kCGWindowOwnerName`, `kCGWindowOwnerPID`, `kCGWindowName` (absent: empty), `kCGWindowLayer` (keep 0), `kCGWindowBounds` (`Width`, `Height`), and `kCGWindowIsOnscreen` (absent: false).
    - The bundle id is looked up once per pid through `NSRunningApplication::runningApplicationWithProcessIdentifier(pid).bundleIdentifier()`.
  - `capture(id)`: `xcap::Window::all()` and find by id. Not found: `Failed("the window is not on screen")`. Capture error: `Failed(e)`.
  - `revalidate`, in order:
    - Exact matches: one gives `Match`; more give `Ask { "{n} windows match {label}; choose one", them }`.
    - Otherwise, `same_app` windows:
      - none: `Ask { "No {app} window is open", [] }`;
      - one with the same title but another size: `Ask { "{title} is {w} × {h} now; it was {W} × {H}", same_app }`;
      - otherwise `Ask { "{app} has no “{title}” window", same_app }`.
  - `Selections` saves through `fsutil::write_atomic` as pretty JSON.

- [ ] **Step 4: Run the tests, and the ignored live one**

Run: `cargo test -p lecturelive-core capture:: 2>&1 | command grep -E "test result"; cargo test -p lecturelive-core system_windows -- --ignored 2>&1 | command grep -E "test result"`
Expected: all pass; the live test lists Terminal with `com.apple.Terminal`.

- [ ] **Step 5: Commit**

```bash
git add crates/core/Cargo.toml Cargo.lock crates/core/src/capture/mod.rs crates/core/src/capture/window.rs crates/core/src/capture/select.rs apps/desktop/src-tauri/src/canary.rs crates/cli/src/main.rs
git commit -m "Describe windows by bundle id, title and size, find them on screen or off, and revalidate a saved selection by asking on any mismatch"
```

---

### Task 4: The capture worker and its place in the lecture (milestone tasks 1–3)

**Files:**
- Create: `crates/core/src/capture/worker.rs`, `crates/core/tests/support/windows.rs`, `crates/core/tests/capture_gate.rs`
- Modify: `crates/core/src/capture/mod.rs`, `crates/core/src/session/lecture.rs`, `crates/core/tests/support/mod.rs`, `crates/core/tests/lecture_gate.rs` (the new `run` argument), `crates/cli/src/main.rs` (`capture: None`)

**Interfaces:**
- Consumes: Task 1's `slides::{register, import, SlideMeta}`; Task 2's `Detector`, `thumb`, `crop`, `Thresholds`; Task 3's `WindowSource`, `WindowInfo`, `CaptureError`, `Selection`, `revalidate`, `Descriptor`.
- Produces (`capture::worker`):

```rust
pub struct WorkerConfig { pub slides: PathBuf, pub interval: Duration, pub thresholds: Thresholds, /** writes each sample's detector input here (fixtures, spec §11) */ pub record: Option<PathBuf> }
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CaptureState {
    Unbound,
    Watching { window: String },
    Paused { window: String, reason: String },
    Asking { window: String, reason: String, candidates: Vec<WindowInfo> },
    Denied,
    Failing { window: String, reason: String },
}
#[derive(Debug)]
pub struct Captured { pub path: PathBuf, pub shown_at: DateTime<Local>, pub auto: bool, pub uncertain: bool }
#[derive(Debug)]
pub enum CaptureEvent { State(CaptureState), Captured(Captured) }
pub enum CaptureCmd { Now(tokio::sync::oneshot::Sender<Result<(), String>>), Bind { window: u32, selection: Selection }, Stop }
pub struct CaptureHandle { /* std::sync::mpsc::Sender<CaptureCmd> */ }
impl CaptureHandle { pub fn send(&self, c: CaptureCmd); }
pub fn spawn(source: Box<dyn WindowSource>, selection: Option<Selection>, cfg: WorkerConfig, events: tokio::sync::mpsc::UnboundedSender<CaptureEvent>) -> CaptureHandle;
```

- Produces (`session::lecture`):
  - `pub struct CaptureSetup { pub source: Box<dyn WindowSource>, pub selection: Option<Selection>, pub interval: Duration, pub thresholds: Thresholds, pub record: Option<PathBuf> }`;
  - `run(lec, cfg, source, watch, capture: Option<CaptureSetup>, commands, events)`;
  - `Command::CaptureNow(oneshot::Sender<Result<(), String>>)`, `Command::Bind { window: u32, selection: Selection }`, `Command::Import(Vec<PathBuf>)`;
  - `Event::Capture(CaptureState)`; a failed import is `Event::Warning`.
- Worker rules (the Settled section above):
  - **Start.** With a selection: `Match` binds, `Ask` becomes `Asking` (still asking on later ticks, with fresh candidates). Without one: `Unbound`.
  - **Each tick while bound:**
    - the id is gone: `Paused "its window closed"`, or `Asking "a new “{title}” window opened"` with the windows matching the descriptor;
    - off screen: `Paused "not on screen (minimised or on another desktop)"`;
    - size changed beyond 2%: `Asking "it is {w} × {h} now; check the region"` with that window;
    - otherwise capture. An error, a blank window or a black region counts a failure (`Failing` from the third in a row). A good frame resets the failures, sets `Watching`, and feeds the detector; a decision saves the region crop, fitted to 1600 px, as `slides/.capture-<n>.png.tmp`, and sends `Captured`.
  - **`Bind`** binds that window with that selection. The detector is reset (so its first frame is kept) only when the region changed.
  - **`Now`** captures the bound region, `keep`s its thumb, and saves it as manual. The reply says why it could not ("No window is being watched: choose one in the slides strip.", "{window} is not on screen", the capture error).
  - A state is sent only when it differs from the last one sent.

- [ ] **Step 1: Write the fake and the failing tests.** `support/windows.rs`:

```rust
//! A scripted window source: windows appear, move off screen, vanish and change content under test control.
use std::sync::{Arc, Mutex};
use image::{Rgba, RgbaImage};
use lecturelive_core::capture::window::{CaptureError, WindowInfo, WindowSource};

#[derive(Default)]
pub struct Screen { pub windows: Vec<(WindowInfo, RgbaImage)>, pub denied: bool, pub fail_next: u32, pub blank_next: u32, pub captures: u32 }

#[derive(Clone, Default)]
pub struct FakeWindows(pub Arc<Mutex<Screen>>);

impl FakeWindows {
    pub fn add(&self, id: u32, title: &str, w: u32, h: u32, content: RgbaImage) {
        let info = WindowInfo { id, app: "zoom.us".into(), bundle_id: Some("us.zoom.xos".into()), title: title.into(), width: w, height: h, on_screen: true };
        self.0.lock().unwrap().windows.push((info, content));
    }
    pub fn show(&self, id: u32, content: RgbaImage) { self.with(id, |_, c| *c = content); }
    pub fn on_screen(&self, id: u32, on: bool) { self.with(id, |i, _| i.on_screen = on); }
    pub fn resize(&self, id: u32, w: u32, h: u32) { self.with(id, |i, _| (i.width, i.height) = (w, h)); }
    pub fn close(&self, id: u32) { self.0.lock().unwrap().windows.retain(|(i, _)| i.id != id); }
    fn with(&self, id: u32, f: impl FnOnce(&mut WindowInfo, &mut RgbaImage)) {
        let mut s = self.0.lock().unwrap();
        let (i, c) = s.windows.iter_mut().find(|(i, _)| i.id == id).expect("a window");
        f(i, c);
    }
}

impl WindowSource for FakeWindows {
    fn windows(&mut self) -> Result<Vec<WindowInfo>, CaptureError> {
        let s = self.0.lock().unwrap();
        if s.denied { return Err(CaptureError::Denied); }
        Ok(s.windows.iter().map(|(i, _)| i.clone()).collect())
    }
    fn capture(&mut self, id: u32) -> Result<RgbaImage, CaptureError> {
        let mut s = self.0.lock().unwrap();
        s.captures += 1;
        if s.fail_next > 0 { s.fail_next -= 1; return Err(CaptureError::Failed("the window server said no".into())); }
        let (i, c) = s.windows.iter().find(|(i, _)| i.id == id).ok_or(CaptureError::Failed("gone".into()))?;
        if !i.on_screen { return Err(CaptureError::Failed("not on screen".into())); }
        if s.blank_next > 0 { let (w, h) = c.dimensions(); s.blank_next -= 1; return Ok(RgbaImage::from_pixel(w, h, Rgba([0, 0, 0, 255]))); }
        Ok(c.clone())
    }
}

/// A slide as a window shows it: a white page with `lines` dark bullet bars.
pub fn slide(lines: u32) -> RgbaImage {
    let mut img = RgbaImage::from_pixel(640, 360, Rgba([250, 250, 250, 255]));
    for l in 0..lines { for y in 60 + l * 40..60 + l * 40 + 14 { for x in 60..520 { img.put_pixel(x, y, Rgba([30, 30, 40, 255])); } } }
    img
}
```

`capture_gate.rs` (worker-level tests run the worker at a 20 ms interval; `events` helpers wait with a 5 s limit):

```rust
//! The M5 gate's window cases (docs/milestones.md): occlusion, minimisation and window replacement
//! never rebind silently; resizes, failures and denial are said; every slide goes through one path. No network.
mod support;

use std::time::Duration;
use lecturelive_core::capture::detect::{Region, Thresholds};
use lecturelive_core::capture::select::{Descriptor, Selection};
use lecturelive_core::capture::worker::{self, CaptureCmd, CaptureEvent, CaptureState, WorkerConfig};
use support::windows::{slide, FakeWindows};
use tokio::sync::mpsc;

fn selection(fake: &FakeWindows, id: u32) -> Selection {
    let s = fake.0.lock().unwrap();
    let (info, _) = s.windows.iter().find(|(i, _)| i.id == id).unwrap();
    Selection { descriptor: Descriptor::of(info), region: Region { x: 0.05, y: 0.1, w: 0.9, h: 0.85 } }
}

fn start(fake: &FakeWindows, sel: Option<Selection>, dir: &std::path::Path) -> (worker::CaptureHandle, mpsc::UnboundedReceiver<CaptureEvent>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let cfg = WorkerConfig { slides: dir.to_path_buf(), interval: Duration::from_millis(20), thresholds: Thresholds::default(), record: None };
    (worker::spawn(Box::new(fake.clone()), sel, cfg, tx), rx)
}

/// Collects events for `ms`: the states in order and how many slides were saved.
async fn watch(rx: &mut mpsc::UnboundedReceiver<CaptureEvent>, ms: u64) -> (Vec<CaptureState>, Vec<(bool, bool)>) {
    let (mut states, mut shots) = (Vec::new(), Vec::new());
    let end = tokio::time::Instant::now() + Duration::from_millis(ms);
    while let Ok(Some(e)) = tokio::time::timeout_at(end, rx.recv()).await {
        match e {
            CaptureEvent::State(s) => states.push(s),
            CaptureEvent::Captured(c) => { assert!(c.path.exists()); shots.push((c.auto, c.uncertain)); }
        }
    }
    (states, shots)
}

fn watching(s: &CaptureState) -> bool { matches!(s, CaptureState::Watching { .. }) }

#[tokio::test]
async fn a_matching_window_binds_at_start_and_each_settled_slide_is_saved_once() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeWindows::default();
    fake.add(42, "Zoom Meeting", 1600, 900, slide(1));
    let (_h, mut rx) = start(&fake, Some(selection(&fake, 42)), dir.path());
    let (states, shots) = watch(&mut rx, 300).await;
    assert!(states.iter().any(watching), "{states:?}");
    assert_eq!(shots, vec![(true, false)], "the first frame of the session");
    fake.show(42, slide(2));
    let (_, shots) = watch(&mut rx, 300).await;
    assert_eq!(shots, vec![(true, false)], "the build, once");
}

#[tokio::test]
async fn occlusion_changes_nothing_minimising_pauses_and_the_same_window_resumes_without_a_duplicate() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeWindows::default();
    fake.add(42, "Zoom Meeting", 1600, 900, slide(1));
    let (_h, mut rx) = start(&fake, Some(selection(&fake, 42)), dir.path());
    watch(&mut rx, 200).await;
    // Covered by another window: the window's own buffer is unchanged (CGWindowListCreateImage, OptionIncludingWindow).
    let (_, shots) = watch(&mut rx, 200).await;
    assert!(shots.is_empty());
    fake.on_screen(42, false);
    let (states, shots) = watch(&mut rx, 200).await;
    assert!(matches!(states.last(), Some(CaptureState::Paused { reason, .. }) if reason.contains("not on screen")), "{states:?}");
    assert!(shots.is_empty());
    fake.on_screen(42, true);
    let (states, shots) = watch(&mut rx, 200).await;
    assert!(states.last().is_some_and(watching));
    assert!(shots.is_empty(), "the same slide is not captured again after the pause");
}

#[tokio::test]
async fn a_replaced_window_asks_and_nothing_is_captured_from_it_until_the_person_binds_it() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeWindows::default();
    fake.add(42, "Zoom Meeting", 1600, 900, slide(1));
    let sel = selection(&fake, 42);
    let (h, mut rx) = start(&fake, Some(sel.clone()), dir.path());
    watch(&mut rx, 200).await;
    fake.close(42);
    let (states, _) = watch(&mut rx, 150).await;
    assert!(matches!(states.last(), Some(CaptureState::Paused { reason, .. }) if reason.contains("closed")), "{states:?}");
    fake.add(77, "Zoom Meeting", 1600, 900, slide(3));
    let (states, shots) = watch(&mut rx, 300).await;
    let Some(CaptureState::Asking { candidates, .. }) = states.last() else { panic!("{states:?}") };
    assert_eq!(candidates.iter().map(|c| c.id).collect::<Vec<_>>(), vec![77]);
    assert!(shots.is_empty(), "no silent rebinding");
    h.send(CaptureCmd::Bind { window: 77, selection: sel });
    let (states, shots) = watch(&mut rx, 300).await;
    assert!(states.last().is_some_and(watching));
    assert_eq!(shots, vec![(true, false)], "the new window's slide, once it is chosen");
}

#[tokio::test]
async fn no_window_at_start_asks_and_a_later_window_waits_for_the_person() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeWindows::default();
    fake.add(42, "Zoom Meeting", 1600, 900, slide(1));
    let sel = selection(&fake, 42);
    fake.close(42);
    let (_h, mut rx) = start(&fake, Some(sel), dir.path());
    let (states, _) = watch(&mut rx, 150).await;
    assert!(matches!(states.last(), Some(CaptureState::Asking { candidates, .. }) if candidates.is_empty()), "{states:?}");
    fake.add(50, "Zoom Meeting", 1600, 900, slide(1));
    let (states, shots) = watch(&mut rx, 300).await;
    assert!(matches!(states.last(), Some(CaptureState::Asking { candidates, .. }) if candidates.len() == 1), "{states:?}");
    assert!(shots.is_empty());
}

#[tokio::test]
async fn a_resized_window_pauses_and_asks() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeWindows::default();
    fake.add(42, "Zoom Meeting", 1600, 900, slide(1));
    let (_h, mut rx) = start(&fake, Some(selection(&fake, 42)), dir.path());
    watch(&mut rx, 200).await;
    fake.resize(42, 1280, 800);
    fake.show(42, slide(4));
    let (states, shots) = watch(&mut rx, 300).await;
    assert!(matches!(states.last(), Some(CaptureState::Asking { reason, .. }) if reason.contains("1280 × 800")), "{states:?}");
    assert!(shots.is_empty(), "Zoom's re-laid-out window is not captured through the old region");
}

#[tokio::test]
async fn blank_and_failed_captures_are_not_slides() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeWindows::default();
    fake.add(42, "Zoom Meeting", 1600, 900, slide(1));
    let (_h, mut rx) = start(&fake, Some(selection(&fake, 42)), dir.path());
    watch(&mut rx, 200).await;
    fake.0.lock().unwrap().blank_next = 2;
    let (states, shots) = watch(&mut rx, 200).await;
    assert!(shots.is_empty() && !states.iter().any(|s| matches!(s, CaptureState::Failing { .. })), "two blanks are not yet a failure: {states:?}");
    fake.0.lock().unwrap().fail_next = 5;
    let (states, shots) = watch(&mut rx, 300).await;
    assert!(states.iter().any(|s| matches!(s, CaptureState::Failing { .. })), "{states:?}");
    assert!(states.last().is_some_and(watching), "and it recovers");
    assert!(shots.is_empty(), "the slide on screen is not captured again");
}

#[tokio::test]
async fn denial_is_said_and_manual_capture_says_why_it_cannot() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeWindows::default();
    fake.add(42, "Zoom Meeting", 1600, 900, slide(1));
    let sel = selection(&fake, 42);
    fake.0.lock().unwrap().denied = true;
    let (h, mut rx) = start(&fake, Some(sel), dir.path());
    let (states, _) = watch(&mut rx, 150).await;
    assert_eq!(states.last(), Some(&CaptureState::Denied));
    let (tx, reply) = tokio::sync::oneshot::channel();
    h.send(CaptureCmd::Now(tx));
    assert!(reply.await.unwrap().unwrap_err().contains("No window is being watched"));
}

#[tokio::test]
async fn manual_capture_keeps_the_frame_so_auto_does_not_take_it_again() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeWindows::default();
    fake.add(42, "Zoom Meeting", 1600, 900, slide(1));
    let (h, mut rx) = start(&fake, Some(selection(&fake, 42)), dir.path());
    watch(&mut rx, 200).await;
    fake.show(42, slide(2));
    let (tx, reply) = tokio::sync::oneshot::channel();
    h.send(CaptureCmd::Now(tx));
    reply.await.unwrap().unwrap();
    let (_, shots) = watch(&mut rx, 300).await;
    assert_eq!(shots, vec![(false, false)], "the manual slide, and no auto copy of it");
}
```

and one lecture-level test in `capture_gate.rs`: `auto_slides_and_the_watcher_share_one_registration`. It runs `lecture::run` with the fake STT and SSE of `lecture_gate` (copy its `respond`, `chat` and `lecture_for` helpers into this file), a `CaptureSetup { source: FakeWindows, selection: Some(…), interval: 20 ms, … }`, and `SlideWatch { screenshots: None, poll: 20 ms }`. It then does the following:
1. Waits for `Event::Slide { index: 1, auto: true, .. }`.
2. Shows `slide(2)` and waits for slide 2, auto.
3. Sends `Command::CaptureNow` and waits for slide 3, `auto: false`.
4. Sends `Command::Import(vec![a PNG outside the folder])` and waits for slide 4.
5. Waits 200 ms more (the watcher has polled `slides/` several times).
6. Stops and awaits the report.

It asserts:
- the sidecar holds exactly 4 slides, with indexes 1–4 and `auto` `[true, true, false, false]`;
- `slides/` holds exactly 4 files and no `.tmp`;
- the notes embed each `![Slide N]` exactly once (the last snapshot takes them all);
- the lecture ended within 10 s of the stop.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p lecturelive-core --test capture_gate 2>&1 | tail -5`
Expected: FAIL to compile (`worker` module missing).

- [ ] **Step 3: Implement.**
  - **`worker.rs`.** A `std::thread` named `capture` whose loop does:

```rust
loop {
    let wait = next.saturating_duration_since(Instant::now());
    match rx.recv_timeout(wait) {
        Ok(CaptureCmd::Now(reply)) => { let _ = reply.send(w.capture_now()); }
        Ok(CaptureCmd::Bind { window, selection }) => { w.bind(window, selection); next = Instant::now(); }
        Ok(CaptureCmd::Stop) | Err(RecvTimeoutError::Disconnected) => return,
        Err(RecvTimeoutError::Timeout) => { w.tick(); next += w.cfg.interval; if next < Instant::now() { next = Instant::now() + w.cfg.interval; } }
    }
}
```

  - **Recording.** With `record: Some(dir)`, each tick writes the thumb to `dir/frames/NNNNN.png` when it differs from the last one written, and appends `{"t": ms since spawn, "frame": n}` (or `"error"`) to `dir/samples.jsonl`. It also writes the whole window as grayscale 1024 px wide to `dir/window/NNNNN.png` on the same dedupe, for re-cropping outside the repository.
  - **`lecture.rs`.** `run` takes `capture: Option<CaptureSetup>`. With one, it spawns the worker (slides dir `lec.files.slides`) and a `capture_task`:
    - `Captured(c)` → `slides::register(&files, &store, &c.path, SlideMeta { shown_at: c.shown_at, auto: c.auto, uncertain: c.uncertain })`, then `Event::Slide` (or `Event::Warning` on failure);
    - `State(s)` → `Event::Capture(s)`.
  - **Commands in the loop:**
    - `CaptureNow(reply)`: forwarded to the worker, or answered "No window is being watched: choose one in the slides strip." when there is none;
    - `Bind`: forwarded;
    - `Import(paths)`: spawns `slides::import` on the state store, with one `Event::Slide` per success and one `Event::Warning` per refusal. Once stopping, it answers `Event::Warning("The lecture is stopping; images dropped now are not added.")`.
  - `begin_stop` sends `CaptureCmd::Stop`, and the loop awaits `capture_task` beside the watcher, so the store clone is dropped before the session ends.

- [ ] **Step 4: Run the gate tests and the whole core suite, three times** (the tests use real time)

Run: `for i in 1 2 3; do cargo test -p lecturelive-core 2>&1 | command grep -E "^test result|FAILED|panicked"; done`
Expected: every run all `ok`.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/capture/mod.rs crates/core/src/capture/worker.rs crates/core/src/session/lecture.rs crates/core/tests/support/mod.rs crates/core/tests/support/windows.rs crates/core/tests/capture_gate.rs crates/core/tests/lecture_gate.rs crates/cli/src/main.rs
git commit -m "Add the capture worker beside the session: bind on a start match, pause or ask when the window goes, is replaced or resized, and register every capture through the one path"
```

---

### Task 5: The detector gate on a synthetic lecture, and the fixture tools (milestone task 2, calibration method)

**Files:**
- Create: `crates/core/tests/support/frames.rs`, `crates/core/tests/slides_gate.rs`
- Modify: `crates/core/tests/support/mod.rs`

**Interfaces:**
- Produces (`support::frames`):
  - `pub struct Sample { pub t: u64, pub frame: Option<GrayImage> }`, `pub struct State { pub id: String, pub from: u64, pub to: u64, pub kind: String }`;
  - `pub fn load(dir: &Path) -> (Vec<Sample>, Vec<State>)` (the fixture format of the Settled section);
  - `pub struct Metrics { pub minutes: f64, pub counted: usize, pub recalled: usize, pub missed: Vec<String>, pub false_captures: Vec<(u64, String)> }` with `recall()` and `false_per_10_min()`;
  - `pub fn evaluate(samples: &[Sample], states: &[State], t: Thresholds) -> Metrics`. A decision at sample time `t` belongs to the state showing at `t`. A state counts when `to − from ≥ 3000`. A capture is false when it lands on a state already captured or on no state;
  - `pub fn synthetic_lecture() -> (Vec<Sample>, Vec<State>)`: 20 minutes at 1 s, generated at 1280×720 through `thumb`, with ±3 grey-level noise. It holds slide changes (cuts and 0.6 s dissolves) and builds of one bullet line, an underline and a table row. It also holds a first sample blurred after each change (Zoom sharpening), a looping animation over 60 s on two slides with a build during it, a caret blinking on one slide, states of 1–2 s that are not counted, and static slides of 60–120 s;
  - `pub fn align(samples: &[Sample], schedule: &[State]) -> i64`: the offset in ms that best matches large frame changes (over 20% of tiles) to the schedule's slide boundaries.
- Produces (`slides_gate.rs`):
  - `synthetic_lecture_meets_the_gate`;
  - `#[ignore] annotate`, which reads `M5_RECORDING` (a recorded folder) and `M5_SCHEDULE` (`apps/desktop/src/lib/deck.json`), aligns, and writes `states.json` into the recording.

- [ ] **Step 1: Write the failing test**

```rust
//! The M5 detector gate (docs/milestones.md): ≥95% recall of stable states visible ≥3 s and ≤1 false
//! capture per 10 minutes, on a synthetic lecture here and on recorded Zoom fixtures (Task 12).
mod support;

use lecturelive_core::capture::detect::Thresholds;
use support::frames::{evaluate, synthetic_lecture};

#[test]
fn synthetic_lecture_meets_the_gate() {
    let (samples, states) = synthetic_lecture();
    let m = evaluate(&samples, &states, Thresholds::default());
    println!("synthetic: {} of {} states recalled ({:.1}%), {} false in {:.1} min; missed {:?}; false {:?}", m.recalled, m.counted, m.recall() * 100.0, m.false_captures.len(), m.minutes, m.missed, m.false_captures);
    assert!(m.counted >= 40, "enough states to measure: {}", m.counted);
    assert!(m.recall() >= 0.95, "recall {:.3}", m.recall());
    assert!(m.false_per_10_min() <= 1.0, "false {:?}", m.false_captures);
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p lecturelive-core --test slides_gate 2>&1 | tail -5`
Expected: FAIL to compile (`support::frames` missing).

- [ ] **Step 3: Implement `frames.rs`**, with a deterministic generator (an LCG seeded per frame, no `rand`). If the synthetic lecture misses a state at the spec's thresholds, look at the state first. A build smaller than one line of text, over a single tile, is below what spec §7.2 promises; resize that build in the generator and record the finding. Do not tune the thresholds to the synthetic data: calibration is on recorded Zoom frames (Task 12).

- [ ] **Step 4: Run it**

Run: `cargo test -p lecturelive-core --test slides_gate -- --nocapture 2>&1 | command grep -E "synthetic:|test result"`
Expected: PASS, with the numbers printed (recorded in the ledger).

- [ ] **Step 5: Commit**

```bash
git add crates/core/tests/support/mod.rs crates/core/tests/support/frames.rs crates/core/tests/slides_gate.rs
git commit -m "Measure the detector's recall and false captures on a synthetic lecture, and add the fixture format and alignment for recorded ones"
```

---

### Task 6: The desktop adapter for capture (milestone tasks 1, 3)

**Files:**
- Modify: `apps/desktop/src-tauri/{Cargo.toml,src/lib.rs,src/app.rs,src/adapter.rs,src/wire.rs}`

**Interfaces:**
- Consumes: Tasks 1–4.
- Produces (`wire.rs`):
  - `SlideView { index, file, path, at: String /* HH:MM:SS */, auto: bool, uncertain: bool }`;
  - `WindowView { id, app, title, width, height, on_screen }`;
  - `CaptureView { state: CaptureWord, window: Option<String>, detail: Option<String>, candidates: Vec<WindowView>, captured: bool }`, where `CaptureWord` is `unbound | ready | watching | paused | asking | denied | failing` (snake case);
  - `Status.capture: CaptureView` (default `unbound`);
  - `PreviewShot { path: String, width: u32, height: u32 }`.
- Produces (commands):
  - `capture_windows() -> Vec<WindowView>`: the Denied error text is distinct from an empty list;
  - `capture_preview(id: u32) -> PreviewShot`: the window fitted to 1600 px, written to `data_dir/picker/window-<id>.png` (other picker files removed), allowed in the asset scope as that one file;
  - `capture_select(id: u32, region: Region)`: saves `Selection` for the open folder's course in `data_dir/capture.json`; binds the running lecture through `Command::Bind`, else status `ready`;
  - `capture_now()`: `Command::CaptureNow`, awaiting the reply;
  - `import_slides(paths: Vec<String>)`: `Command::Import`, or "Start the lecture to add slides." when none runs;
  - `open_screen_settings()`: `open x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture`.
- Produces (behaviour):
  - `select_folder` sets `status.capture` to `ready` (with the saved window's label) or `unbound`.
  - `start_lecture` passes `CaptureSetup { source: SystemWindows, selection: the course's, interval: 1 s, thresholds: default, record: LECTURELIVE_RECORD as a path when set }`. It registers the global shortcut `CommandOrControl+Shift+Digit2`; a failure gives a warning notice. The shortcut is unregistered when the lecture ends.
  - `Pump` maps `Event::Capture(s)` to `status.capture`, with a notice (▣ slide) on each change of kind: "Watching …", "Paused: …", "Asking: …", "Screen Recording is off …", "Capture failing: …". `captured` becomes true at the first `Event::Slide` with `auto` or at the first successful manual capture of the session.
  - `Pump.state` builds `SlideView` with the badges; the slide notice reads "Slide N (auto)", "Slide N (auto, still changing)" or "Slide N".

- [ ] **Step 1: Write the failing tests** in `adapter.rs` and `app.rs`:

```rust
// adapter.rs tests
#[test]
fn capture_states_reach_the_status_with_a_notice_and_slides_carry_their_badges() {
    let (mut p, sink) = pump();
    p.apply(Event::Capture(CaptureState::Watching { window: "Zoom Meeting".into() }));
    p.apply(Event::Capture(CaptureState::Watching { window: "Zoom Meeting".into() }));
    let at = Local.with_ymd_and_hms(2026, 9, 25, 10, 2, 51).unwrap();
    p.apply(Event::Slide { index: 1, file: "slides/slide_01_100251.png".into(), auto: true, uncertain: true, shown_at: at });
    p.apply(Event::Capture(CaptureState::Asking { window: "Zoom Meeting".into(), reason: "a new “Zoom Meeting” window opened".into(), candidates: vec![] }));
    let msgs = sink.on(Stream::Status);
    let notices: Vec<&str> = msgs.iter().filter(|m| m["type"] == "notice").map(|m| m["label"].as_str().unwrap()).collect();
    assert_eq!(notices, ["Watching", "Slide 1", "Asking"], "an unchanged state is not announced twice");
    let slide = msgs.iter().find(|m| m["type"] == "slide").unwrap();
    assert_eq!((slide["auto"].as_bool(), slide["uncertain"].as_bool(), slide["at"].as_str()), (Some(true), Some(true), Some("10:02:51")));
    let status = msgs.iter().rev().find(|m| m["type"] == "status").unwrap();
    assert_eq!((status["capture"]["state"].as_str(), status["capture"]["captured"].as_bool()), (Some("asking"), Some(true)));
}
```

```rust
// app.rs tests
#[test]
fn a_selection_is_saved_per_course_and_the_status_names_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("capture.json");
    let w = WindowInfo { id: 42, app: "zoom.us".into(), bundle_id: Some("us.zoom.xos".into()), title: "Zoom Meeting".into(), width: 1600, height: 900, on_screen: true };
    save_selection(&path, "Machine Learning", &w, Region { x: 0.1, y: 0.1, w: 0.8, h: 0.8 }).unwrap();
    assert_eq!(ready_view(&path, "Machine Learning").state, CaptureWord::Ready);
    assert_eq!(ready_view(&path, "Machine Learning").window.as_deref(), Some("Zoom Meeting"));
    assert_eq!(ready_view(&path, "Statistics").state, CaptureWord::Unbound);
    assert!(save_selection(&path, "Machine Learning", &w, Region { x: 0.5, y: 0.5, w: 0.9, h: 0.9 }).is_err(), "a region outside the window is refused");
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p desktop 2>&1 | tail -5`
Expected: FAIL to compile.

- [ ] **Step 3: Implement.** Add the plugin: `cargo add tauri-plugin-global-shortcut@2.3.2 -p desktop` (outside the sandbox, if the index is blocked). In `lib.rs`, `.plugin(tauri_plugin_global_shortcut::Builder::new().with_handler(|app, _, e| if e.state == ShortcutState::Pressed { app::shortcut(app) }).build())` and register the new commands. `app::shortcut` sends `CaptureNow` and turns an error reply into a warning notice. `save_selection(path, course, &WindowInfo, Region)` and `ready_view(path, course)` are the testable halves of `capture_select` and `select_folder`.

- [ ] **Step 4: Run**

Run: `cargo test -p desktop 2>&1 | command grep -E "test result|FAILED"; cargo build -p desktop 2>&1 | tail -2`
Expected: all pass; the debug binary builds.

- [ ] **Step 5: Commit**

```bash
git add apps/desktop/src-tauri/Cargo.toml Cargo.lock apps/desktop/src-tauri/src/lib.rs apps/desktop/src-tauri/src/app.rs apps/desktop/src-tauri/src/adapter.rs apps/desktop/src-tauri/src/wire.rs
git commit -m "Give the app the capture commands, a selection per course, the capture status, a global shortcut and dropped images"
```

---

### Task 7: The frontend store for slides and capture; chunk keys (milestone tasks 3, 4; M4 minor M8)

**Files:**
- Modify: `apps/desktop/src/lib/{wire.ts,transport.ts,fixture.ts,session.svelte.ts,markdown.ts,Notes.svelte,session.test.ts,markdown.test.ts}`

**Interfaces:**
- Produces (`wire.ts`): the Task 6 types mirrored exactly.
- Produces (`transport.ts`): `listenDrops(cb: (e: { type: "enter" | "over" | "drop" | "leave"; paths: string[] }) => void): Promise<() => void>` (Tauri: `getCurrentWebview().onDragDropEvent`; fixture: `emitDrop`).
- Produces (`session.svelte.ts`):
  - `dragging = $state(false)`;
  - `get capture(): CaptureView`;
  - `get canCapture(): boolean` (running, and `capture.captured` and `capture.state === "watching"`);
  - `captureNow()`, `importSlides(paths)`, `captureWindows(): Promise<WindowView[]>`, `capturePreview(id): Promise<PreviewShot | undefined>`, `captureSelect(id, region)`, `openScreenSettings()`;
  - a drop calls `importSlides(paths)`.
- Produces (`markdown.ts`): `hasImage(md: string): boolean` and `chunkKey(base: string, part: string, md: string, slides: number): string`. Only a chunk with an image carries the slide count.

- [ ] **Step 1: Write the failing tests**

```ts
// markdown.test.ts
it("a new slide changes the key of chunks with an image only (M4 minor M8)", () => {
  const text = "## Momentum\n- keeps rolling\n";
  const img = "## Chart\n![Slide 1](slides/slide_01_100251.png)\n";
  expect(chunkKey("r2", "d1", text, 1)).toBe(chunkKey("r2", "d1", text, 2));
  expect(chunkKey("r2", "d2", img, 1)).not.toBe(chunkKey("r2", "d2", img, 2));
  expect(hasImage('<img src="slides/a.png">')).toBe(true);
});
```

```ts
// session.test.ts
it("slides keep their badges, capture status arrives whole, and a drop imports its paths", async () => {
  const t = new FixtureTransport(emptyState("s1"));
  const s = new Session();
  await s.init(t, { schedule: (f) => queueMicrotask(() => f(0)) });
  t.emitStatus({ type: "slide", index: 1, file: "slides/slide_01_100251.png", path: "/l/slides/slide_01_100251.png", at: "10:02:51", auto: true, uncertain: false });
  t.emitStatus({ type: "status", ...emptyState("s1").status, phase: "running", capture: { state: "watching", window: "Zoom Meeting", detail: null, candidates: [], captured: true } });
  await tick();
  expect(s.slides[0].auto).toBe(true);
  expect(s.canCapture).toBe(true);
  t.emitDrop({ type: "enter", paths: ["/Desktop/board.png"] });
  expect(s.dragging).toBe(true);
  t.emitDrop({ type: "drop", paths: ["/Desktop/board.png"] });
  await tick();
  expect(s.dragging).toBe(false);
  expect(t.calls.at(-1)).toEqual(["import_slides", { paths: ["/Desktop/board.png"] }]);
});
```

- [ ] **Step 2: Run to verify they fail**

Run: `cd apps/desktop && npx vitest run 2>&1 | tail -8`
Expected: FAIL (`chunkKey`, `emitDrop`, `canCapture` missing).

- [ ] **Step 3: Implement.** `Notes.svelte` builds its keys with `chunkKey`; the preview's finished blocks use it too. `fixture.ts`: `idleStatus()` gains `capture`; the demo gains three slides (auto, manual, uncertain), a watching capture status and `capture_windows` / `capture_preview` answers (the preview through `/demo-slide.svg`); `FixtureTransport.emitDrop`.

- [ ] **Step 4: Run**

Run: `cd apps/desktop && npx vitest run 2>&1 | tail -4 && npx svelte-check --tsconfig ./tsconfig.json 2>&1 | tail -2`
Expected: all pass; 0 errors, 0 warnings.

- [ ] **Step 5: Commit**

```bash
git add apps/desktop/src/lib/wire.ts apps/desktop/src/lib/transport.ts apps/desktop/src/lib/fixture.ts apps/desktop/src/lib/session.svelte.ts apps/desktop/src/lib/markdown.ts apps/desktop/src/lib/Notes.svelte apps/desktop/src/lib/session.test.ts apps/desktop/src/lib/markdown.test.ts
git commit -m "Carry slide badges, the capture status and dropped files through the store, and re-render only notes chunks that hold an image when a slide arrives"
```

---

### Task 8: The slides strip (milestone task 4)

**Files:**
- Create: `apps/desktop/src/lib/SlidesStrip.svelte`
- Modify: `apps/desktop/src/routes/+page.svelte` (the aside becomes the strip; column widths)

**Interfaces:**
- Consumes (Task 7): `session.slides`, `session.capture`, `session.canCapture`, `session.dragging`, `session.captureNow()`, `session.openScreenSettings()`, `session.onFrame`.
- Produces: `<SlidesStrip toUrl onChoose />`, where `onChoose()` opens the picker (Task 9).

- [ ] **Step 1: Invoke `/frontend-design:frontend-design`** for the strip, with the milestone design direction above as the brief's context. Record the run (its plan, what the review against the brief changed) in the ledger. No markup before it.
- [ ] **Step 2: Build the strip.**
  - The binding line holds one of: watching (teal rule, Capture with the ⌘⇧2 hint, Choose…), ready, unbound, paused, asking ("Watch it" binds the one candidate through `captureSelect` with the saved region; "Choose…" opens the picker), denied (Open Settings) or failing.
  - Each slide row: its time and badge in the gutter, the thumbnail through `toUrl(path)` with `alt="Slide N, taken automatically at HH:MM:SS"` (or "by you"), and `loading="lazy"`.
  - It follows the newest slide while the reader is at the end; the drop hint shows while dragging; the empty state is as designed.
  - The page: `grid-template-columns: minmax(20rem, 34fr) minmax(28rem, 56fr) 18rem`, 16 rem below 1280 px, the strip folded below 1100 px.
- [ ] **Step 3: Check it** in the browser preview: `npm run dev`, then `http://localhost:1420/` (the fixture demo) in the Playwright browser. Check light and dark (`emulateMedia`), normal and large type, at 1440 and 1180 px, and with the fixture driven through `globalThis.__fixture` into every binding state and into a drag. Screenshots are read, then deleted.
- [ ] **Step 4: Run** `npx vitest run && npx svelte-check --tsconfig ./tsconfig.json 2>&1 | tail -2`. Expected: all pass, 0 errors.
- [ ] **Step 5: Commit**

```bash
git add apps/desktop/src/lib/SlidesStrip.svelte apps/desktop/src/routes/+page.svelte apps/desktop/src/lib/fixture.ts
git commit -m "Add the slides strip: thumbnails with time and auto, manual or unsettled; the watched window and its state; capture and dropped images"
```

---

### Task 9: The window and region picker (milestone task 1, UI)

**Files:**
- Create: `apps/desktop/src/lib/WindowPicker.svelte`
- Modify: `apps/desktop/src/routes/+page.svelte`

**Interfaces:**
- Consumes (Task 7): `session.captureWindows()`, `session.capturePreview(id)`, `session.captureSelect(id, region)`, `session.capture` (the reason and candidates when asking), `session.error`.
- Produces: `<WindowPicker bind:this />` with `show(reason?: string)`; an action `region(node, onRegion)` that turns pointer drags over the preview into a `Region` in fractions (pointer capture; a click without a drag keeps the last region).

- [ ] **Step 1: Invoke `/frontend-design:frontend-design`** for the picker, the ask and the permission line, with the milestone design direction above as context. Record the run in the ledger. No markup before it.
- [ ] **Step 2: Build it.**
  - The list: Zoom's windows first, then the rest by app; windows off screen marked "not on screen".
  - Choosing a row loads its preview. The saved region, or the whole window, is drawn on it, and a drag redraws it. "Watch this region" calls `captureSelect` and closes the dialog.
  - Errors (Denied) show the backend's words with "Open Settings".
  - The reason line comes from `session.capture.detail` when the dialog opens from an ask.
- [ ] **Step 3: Check it** in the browser preview with the fixture's windows and preview: drawing a region by pointer drag (Playwright `mouse.down/move/up`), light and dark, large type, at 960 px wide. Screenshots are read, then deleted.
- [ ] **Step 4: Run** `npx vitest run && npx svelte-check --tsconfig ./tsconfig.json 2>&1 | tail -2`. Expected: all pass.
- [ ] **Step 5: Commit**

```bash
git add apps/desktop/src/lib/WindowPicker.svelte apps/desktop/src/routes/+page.svelte apps/desktop/src/lib/fixture.ts
git commit -m "Add the window and region picker, which also answers a changed window by asking"
```

---

### Task 10: The synthetic deck and the in-app capture check (gate: occlusion, minimisation and replacement live)

**Files:**
- Create: `apps/desktop/src/lib/deck.json`, `apps/desktop/src/routes/deck/+page.svelte`
- Modify: `apps/desktop/src/lib/checks.ts`, `apps/desktop/src/routes/+page.svelte`, `apps/desktop/src-tauri/src/{app.rs,lib.rs}` (`check_deck`, report folder `m5-checks/`)

**Interfaces:**
- Produces (`deck.json`): `{ "states": [{ "id": "s01", "slide": 1, "build": 0, "ms": 45000, "kind": "slide" | "build" | "animation" | "brief", "transition": "cut" | "dissolve" }] }`, about 20 minutes in total, with at least 45 states visible for 3 s or more.
- Produces (`/deck`): the deck rendered at the window's size. Without a query, it waits on a title slide with a Start button and then plays the schedule. With `?check`, it shows only what `window.__deck.show(i)` asks for. No remote loads.
- Produces (command): `check_deck(action: String) -> Result<Value, String>`, one of:
  - `open`: a second window `deck`, "LectureLive deck", 1280×720, at `/deck?check`; returns its window id from `SystemWindows`;
  - `show:N`: `eval("window.__deck.show(N)")`;
  - `cover` / `uncover`: moves the main window over the deck, or back;
  - `minimize` / `unminimize`;
  - `replace`: closes the deck and opens it again; returns the new id.
- Produces (`checks.ts`): `captureCheck(session, t, dir)` and `zoomCheck(session, t, dir)`, each reporting to `m5-checks/<name>.json`.

- [ ] **Step 1: Invoke `/frontend-design:frontend-design`** for the deck page. Its brief is to look like an ordinary university lecture deck (white slides, a title, bullets, a diagram, a table, an equation), because its job is to be realistic input for the detector; it is not an app view. Record the run in the ledger. No markup before it.
- [ ] **Step 2: Build `deck.json` and `/deck`.**
  - The builds are one bullet line, an underline, a table row, a boxed equation and an arrow on a diagram.
  - Two slides carry a looping 2 s animation, one with a build during it; one slide carries a blinking caret.
  - Dissolves take 0.6 s; there are states of 1–2 s (`brief`) and static slides of 60–120 s.
- [ ] **Step 3: `captureCheck`.** Folder `$HOME/Library/Application Support/LectureLive/m5-capture/Weeks/Week 01 — Capture`, course "m5-capture". With BlackHole silent (no `say`), the check does, in order:
  1. Selects the folder, runs `check_deck open` and `capture_select(id, {x:0.02, y:0.06, w:0.96, h:0.92})`, then starts the lecture on loopback.
  2. Waits for `watching` and slide 1 (auto).
  3. `show:1`, then waits for slide 2 (auto).
  4. `cover`; `show:2` while covered, then waits for slide 3 (captured through the cover); `uncover`.
  5. `minimize`, waits for `paused`, runs `show:3` while minimised, and checks that 3 s pass with no slide; `unminimize`, then waits for `watching` and exactly one new slide (state 3), within 5 s.
  6. `replace`, waits for `asking` with the new id as the candidate, and checks that 3 s pass with no slide; `capture_select(new id, same region)`, then waits for `watching`.
  7. `show:4`, then waits for an auto slide.
  8. `capture_now`, then waits for a manual slide.
  9. `import_slides([a synthetic PNG in the check folder])`, then waits for a manual slide.
  10. Stops and waits for `ended`.

  It reports every step with its time, the slides with their badges, and the notes' embed count (each slide exactly once). The default output is read before and after, and the report says so.
- [ ] **Step 4: Run it**: `npm run dev` in `apps/desktop`, then `LECTURELIVE_CHECK=capture LECTURELIVE_CHECK_DIR="…/Week 01 — Capture" ./target/debug/desktop`. Record the report and the cost of the last snapshot from `spend.jsonl` (course "m5-capture").
- [ ] **Step 5: `zoomCheck`** (for Task 11), on `…/m5-live/Weeks/Week 01 — Zoom`, course "m5-live". The check does, in order:
  1. Selects the folder and waits (up to 15 minutes) for `status.capture.state` to be `ready`, which happens once the person has chosen Zoom's window.
  2. Starts the lecture on loopback with `LECTURELIVE_RECORD=$HOME/Library/Application Support/LectureLive/m5-fixtures/zoom-<date>` set by the launcher.
  3. Records the time of `watching`, then runs for 26 minutes. It notes whether a manual slide (⌘⇧2) and a dropped image arrived.
  4. Stops, waits for `ended`, and reports the slides with their badges and times.
- [ ] **Step 6: Commit**

```bash
git add apps/desktop/src/lib/deck.json apps/desktop/src/routes/deck/+page.svelte apps/desktop/src/lib/checks.ts apps/desktop/src/routes/+page.svelte apps/desktop/src-tauri/src/app.rs apps/desktop/src-tauri/src/lib.rs
git commit -m "Add a synthetic deck and an in-app capture check through cover, minimise, replacement, manual capture and a dropped image"
```

---

### Task 11: The one sitting (human steps; gate evidence on Zoom)

- [ ] **Step 1:** Prepare everything first:
  - the dev server;
  - the dev app on `zoomCheck`;
  - the synthetic folder and `drop-me.png` (a synthetic slide);
  - no `say` running;
  - the default output read.
- [ ] **Step 2:** Ask the person for the sitting listed in the header, all steps at once.
- [ ] **Step 3:** When they say "ready":
  - confirm `status.capture` is `watching`, and that the recording folder has frames;
  - look at one recorded `window/` frame (their solo meeting with the synthetic deck) to confirm the region frames the shared slide and holds no name or face; delete any copy made to look at it;
  - then tell them to click Start on the deck.
- [ ] **Step 4:** After the check ends:
  - record the report (`m5-checks/zoom.json`), whether ⌘⇧2 and the drop arrived, and the spend;
  - end nothing in Zoom (the person ends their meeting when they like);
  - read the default output again.

---

### Task 12: The recorded Zoom fixture and the gate (milestone task 2, calibration; gate line 1)

**Files:**
- Create: `crates/core/tests/fixtures/slides/zoom-1/{frames/,samples.jsonl,states.json}`
- Modify: `crates/core/tests/slides_gate.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn recorded_zoom_fixtures_meet_the_gate() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/slides");
    let dirs: Vec<_> = std::fs::read_dir(&root).map(|d| d.flatten().map(|e| e.path()).filter(|p| p.join("states.json").exists()).collect()).unwrap_or_default();
    assert!(!dirs.is_empty(), "no recorded Zoom fixture under {}", root.display());
    for dir in dirs {
        let (samples, states) = support::frames::load(&dir);
        let m = evaluate(&samples, &states, Thresholds::default());
        println!("{}: {} of {} states recalled ({:.1}%), {} false in {:.1} min ({:.2} per 10 min); missed {:?}; false {:?}", dir.display(), m.recalled, m.counted, m.recall() * 100.0, m.false_captures.len(), m.minutes, m.false_per_10_min(), m.missed, m.false_captures);
        assert!(m.recall() >= 0.95 && m.false_per_10_min() <= 1.0);
    }
}
```

- [ ] **Step 2: Run to verify it fails** (no fixture yet): `cargo test -p lecturelive-core --test slides_gate recorded 2>&1 | tail -4`.
- [ ] **Step 3: Annotate and copy the fixture.**
  - Annotate: `M5_RECORDING=… M5_SCHEDULE=apps/desktop/src/lib/deck.json cargo test -p lecturelive-core --test slides_gate annotate -- --ignored --nocapture`. It prints the offset and each slide boundary's residual; the alignment is accepted when the median residual is under 1 s.
  - Copy `frames/`, `samples.jsonl` and `states.json` (not `window/`) into `crates/core/tests/fixtures/slides/zoom-1/`, and check the size (`du -sh`, expected under 5 MB).
  - Look at a contact sheet of every 20th distinct frame (an ignored helper writes it outside the repository), and delete it after.
  - Confirm the replay reproduces the live decisions: `evaluate`'s captures equal the auto slides of `m5-checks/zoom.json` in time.
- [ ] **Step 4: Run the gate.** If it passes at the spec's thresholds, record the numbers. If it fails, calibrate on this fixture:
  - a grid of `change ∈ {0.05, 0.06, 0.07, 0.08, 0.10}` × `settle ∈ {0.02, 0.03, 0.04, 0.05}`;
  - pick the pair with the fewest false captures among those at 95% recall or more, preferring the spec's values when tied;
  - check it still passes `synthetic_lecture_meets_the_gate`;
  - change `Thresholds::default()` and record the grid in the ledger. The same threshold then applies to both tests.
- [ ] **Step 5: Commit**

```bash
git add crates/core/tests/slides_gate.rs crates/core/tests/fixtures/slides/zoom-1 crates/core/src/capture/detect.rs
git commit -m "Record a Zoom fixture of the synthetic deck and hold the detector gate on it"
```

---

### Task 13: Spec §7 and the strip line of §9.1 to what is built

**Files:**
- Modify: `docs/spec.md` §7 (window, descriptor and region per course, revalidation and the asking rule, detection with the thresholds as calibrated, the registration path, permissions) and §9.1's slides line.

- [ ] **Step 1:** Rewrite both as the current design (no "previously" trails). Everything they state is in the code or the Findings. Update §14.3 only with the calibration result (M5 owns spec §7; §14.3 is its open question).
- [ ] **Step 2: Commit**

```bash
git add docs/spec.md
git commit -m "Rewrite spec 7 and the slides strip line to the capture worker, detector and strip as built"
```

---

### Task 14: Review, findings and handover

**Files:**
- Modify: `docs/milestones.md` (M5 section: gate ticks and Findings; Status row)

- [ ] **Step 1:** Run and record `cargo test -p lecturelive-core`, `cargo test -p desktop`, `npx vitest run` and `npx svelte-check`, with pass, fail and ignored counts from their output.
- [ ] **Step 2:** A fresh reviewer on `opus` reviews the whole branch (`git diff main..m5-slide-automation`), given this plan's Review Focus verbatim and the ledger's Ruling lines. Re-grade its findings by their effect on the person using the app; fix Critical and Important findings test-first.
- [ ] **Step 3:** Write Findings in the M5 section:
  - resolved versions of new crates and npm packages;
  - the capture worker and registration path as built;
  - the fixture set and the detector's recall and false-capture numbers;
  - the Screen Recording evidence for the dev binary;
  - the `/frontend-design:frontend-design` runs, one per view;
  - live spend;
  - failed lines with their §14.1 pointer;
  - open threads with owners.

  Tick the gate lines that held. Set Status to `done` only if every line holds, else `in progress`. Link the plan in the Status table.
- [ ] **Step 4: Commit.** Then follow the repository's CLAUDE.md: scan what the push publishes, fast-forward `main`, push, delete the branch.

```bash
git add docs/milestones.md
git commit -m "Record M5 findings"
```
