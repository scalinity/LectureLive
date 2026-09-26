# M6 Full-Lecture Acceptance Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** LectureLive replaces the Python CLI. Mixed mode (the loopback and a microphone on independent clocks, in one recording) is built and ships once it passes a two-hour drift test. Every row of spec §10's error table is observed, with its evidence in the findings. A two-hour real lecture with injected failures is prepared as one run sheet for the person, with an audit that says whether any audio went missing or was recorded twice. Once that lecture's gate lines hold, `live_notes.py` and `pyproject.toml` are removed and the README switches to the app and the Rust CLI.

**Architecture:** Core gains `audio::mix`, a pure mixer. Each source is resampled to 16 kHz by the FFT resampler that single sources already use, then passes a drift stage (rubato's asynchronous polynomial resampler at a ratio near 1) into a FIFO. On the host clock, the mixer takes exactly the samples that are due, and steers each drift stage to hold its FIFO level. `audio::mixed::MixedSource` runs two device streams through it as one `Source`. The recording lasts while either source is there, and a gap marks where one was missing. The single-input source gains a fallback the person can choose while the device is gone. Core gains `session::audit`, and the CLI gains `lecture audit`: an audit of a folder's recordings against their gaps and session markers. The three network clients accept a connect address (`LECTURELIVE_API_ADDR`), so a local forwarder (`examples/netcut.rs`) can take the network away from LectureLive alone. The desktop adapter adds the microphone fix-it, the fallback offer and the mixed choice, each through `/frontend-design:frontend-design`. `docs/VERIFICATION.html` gains the two-hour lecture as one occasion.

**Tech Stack:** Rust 1.96.1; `cpal 0.18.2`, `rubato 5.0.0` (`Async` polynomial resampler, new use; `Fft` as before), `rtrb 0.4.0`, `reqwest 0.13.5` (`ClientBuilder::resolve`), `tokio-tungstenite 0.30.0` (`client_async_tls_with_config`), `tokio 1.53.1`; `tauri 2.11.6`; `svelte 5.57.1`, `@tauri-apps/api 2.11.1`, `vitest 5.0.2`, `jsdom 30.1.1`. No new crates or packages are expected.

**Spec:** `docs/spec.md` §4.2 (mixed mode), §10 (every row), §11 (drift alignment over simulated clocks, golden tests, the manual full lecture), §12's M6 row (the gate as the spec states it); also §4.1 (sources, fallback), §4.3 step 4 (the silence warning), §5.4 (reconnection), §8 (slide fields, as M5 left them), §13 (the keyring line, rubato), §14.3. Gate: `docs/milestones.md` → M6. Evidence: the M0–M5 Findings and open threads in `docs/milestones.md`, `docs/VERIFICATION.html`, and the plan research below.

**Plan research (2026-09-26, before this plan was written).**

| Question | Evidence | Consequence |
|---|---|---|
| Has the person done the lecture checks V01–V04? | No `m1-zoom/` folder; `m5-fixtures/` holds only `deck-20260926` and `fullscreen-20260926`, no `zoom-live-*`; `m5-checks/record.json` is M5's own run (00:23); the newest canary image, `canary/window_4390.png` (02:02), is "desktop — LectureLive deck" in `checks.log`, M5's synthetic deck. | None was done. M0's and M1's "At the first Zoom lecture" lines stay unticked, and there is no live Zoom recording to measure. The M6 lecture occasion carries V01's 30 minutes, V02's detector recording and V04's full screen inside the two-hour run, and V03 alongside it. |
| How do this Mac's two input clocks drift? | A scratch probe outside the repository counted callback frames against `Instant` for 180 s on both inputs at once, and fitted frames = rate × t: BlackHole 2ch 47,999.998 Hz (−0.0 ppm against the host clock); MacBook Pro Microphone 48,000.092 Hz (+1.9 ppm). Both deliver 512-frame callbacks, at most 11.0 and 11.3 ms apart, and neither deviates more than 0.63 ms from its fit. The DJI receiver (its own crystal) is not connected. | BlackHole is clocked from the host, and the built-in microphone is clocked close to it: together they drift about 14 ms in two hours. The drift test therefore runs on simulated clocks at ±100 ppm (spec §4.2's figure) and ±500 ppm (stress), as §11 names. A live two-hour run of BlackHole and the built-in microphone adds what the simulation cannot, real callbacks and scheduling over two hours, but not real drift. The real independent clock is the DJI receiver, in the lecture. |
| rubato 5's asynchronous resampler | `Async::new_poly(ratio, max_relative, PolynomialDegree, chunk, channels, FixedAsync)` and `Async::new_sinc(…)` (`asynchro.rs:186, 269`); `Adjustable::set_resample_ratio_relative(rel, ramp)` (`asynchro.rs:781`); `Resampler::output_delay()`. Its `Slip` docs steer the ratio with a proportional loop on buffer fill (`slip.rs`, "A clock-drift feedback loop"). A sinc `Async` from 48 kHz to 16 kHz costs about 16,000 × 128 × 2 multiply-adds per second per source, which makes a two-hour simulation tens of billions of operations. | Two stages: the existing `Resampler16k` (FFT, anti-aliased, exact delay trimming, M1) takes each source to 16 kHz at its nominal rate, then `Async` polynomial (septic) at a ratio of 1 ± 0.2% absorbs the drift. Near a ratio of 1, polynomial interpolation of band-limited 16 kHz audio is close to transparent, and it costs 8 taps per sample. `rubato`, `realfft` and `rustfft` are optimised in the dev profile, as M5 did for `image` and `xcap`, so the two-hour simulation runs in `cargo test`. |
| Network loss for LectureLive alone | Turning the Mac's network off also drops Zoom, and the Python CLI (the notes of record during the lecture) uses the same `api.x.ai`. `reqwest 0.13.5` has `ClientBuilder::resolve(domain, addr)` (`async_impl/client.rs:2308`). `tokio-tungstenite 0.30.0` has `client_async_tls_with_config(request, stream, config, connector)` (`tls.rs:189`), which does TLS for the request's host over any stream. Neither `socat` nor `tinyproxy` is installed. STT has a 10 s connect timeout, a 5 s send timeout and a 5 s idle timeout (`stt/stream.rs:59–61, 239`). | With `LECTURELIVE_API_ADDR=127.0.0.1:8443`, the STT websocket, REST recovery and the chat client connect to a local forwarder, and TLS still verifies `api.x.ai`. The forwarder is a core example (`examples/netcut.rs`), a single process. `kill -STOP` of it is a dropped link (connections stay open and nothing passes: the idle timeout finds it within 5 s); `kill -CONT` restores it; killing it refuses connections (backoff). Zoom and the Python CLI keep the real network. |
| Disk full without risk | `hdiutil create -size 8m -fs APFS` and `attach -nobrowse` work as the user; `dd` into it stops at "No space left on device" with about 1.6 MB still reported free (APFS keeps a reserve). | A lecture folder goes on an HFS+ image (its free space falls to zero), which is filled with `dd` mid-lecture. The system volume is never filled. |
| Permission denial | TCC charges Terminal.app for `target/debug/desktop` run from a shell (M5 research). The Python CLI records the microphone from Terminal too. | Denial is only ever the person's toggle, and only of Terminal's grants; the canary's grants are never touched. In the lecture, Screen Recording is denied: the Python CLI does not capture the screen, so its notes go on. Microphone denial is a desk check, never during a lecture. macOS may offer "Quit & Reopen" for Terminal; the answer is always "Later", because a session may be running in Terminal. |
| Receiver removal | The DJI "Wireless Mic Rx" is not connected now (inputs: BlackHole 2ch, MacBook Pro Microphone). M1 recorded its UID and its replug behaviour. | With mixed mode shipping, the lecture runs Zoom and the receiver together; unplugging the receiver shows the "Mixed" half of §10's row. The "Single" half (the fallback offer) is a desk check with the receiver as the only input. |
| The Python CLI for V09 | `live_notes.py page` works in the current folder, refuses a folder with `.live_notes/*.v2.json` (`live_notes.py:820`), and re-renders from `.live_notes/<stem>.page.json` without a request when its key matches (`:737–741`). The `.venv` has Python 3.14. | V09 byte-compares the study page that both tools render from one cache, on a synthetic folder without slides: PIL's and `image`'s JPEG encoders differ, so slide data URIs would differ for no reason. The session runs it once as evidence; V09 at r2 gives the person the same two commands. |
| Golden tests that read `live_notes.py` | Only `notes::prompts::tests::{system_prompts_are_live_notes_py_literals, user_message_fragments_appear_in_live_notes_py}` read it (`prompts.rs:120`). | Frozen into a verbatim excerpt fixture before removal (Task 12). |
| Recovery retries (an M2 thread) | `stt/rest.rs:185–192` retries a transient failure with an exponential pause, capped at `MAX_RETRY_UNITS`, while the session runs. | Left, with that as the reason: retrying without a limit is what lets recovery finish after a long outage, and a second stop ends it. |
| §10's rows today | Tests already cover: the disconnect suite and live reconnects (M2); STT 4xx (`stt_gate::a_refusal_stops_stt…`, a live bad key); notes failures, truncation and cancel (`lecture_gate::failed_empty_and_truncated…`, `…cancelled_snapshot…`); `notes_page::a_page_that_fails_after_its_retry_writes_nothing`; `notesfile::tests::a_crash_at_every_commit_step…`; the Screen Recording denied state (M5); the BlackHole hint (`FolderPicker.svelte:43`); the preflight (`lecturelive loopback check`) and the silence warning (`adapter::tests::ten_silent_seconds…`, M3 live); device gone and rate change (`source::tests`, M1 live receiver); `coordinator::tests::unwritable_recordings_dir_stops_with_the_path`; M0–M2 crashes; `notesfile::tests::an_external_edit_is_kept…`. Missing: a spend-ledger failure test, the microphone fix-it, the single-source fallback offer, the mixed half of "device disappears", a full disk mid-recording, a network loss and a forced restart in one live lecture. | The missing ones are what Tasks 3–10 add. |

**Settled here (the prompt asked for these to be decided, not inherited):**
- *Mixed mode's clock.* Spec §4.2 steers "the secondary source". Built instead: the host clock is the timeline, and both sources are steered to it. A device timeline would need a handoff when that device goes, which is exactly the §10 case "Mixed: surviving source continues". Cpal's capture timestamps and the mixer's ticks are both in host time. A source joins with silence in front of it, so its first sample lands where its capture time belongs, 200 ms behind the host clock. The steering holds its FIFO at the level it settled at over its first 4 s. Spec §4.2 is rewritten to say so.
- *Stalls and bursts.* A FIFO more than 100 ms from its target is not drift (callback blocks and resampler chunks move it by about 600 samples, under 40 ms). When a source runs dry, the silence it leaves is a gap, and the source joins again with silence to realign. When its backlog arrives all at once, the excess is dropped as a gap. Steering alone would take hundreds of seconds to absorb either, with the source misaligned all that time.
- *The meter and the silence warning in mixed mode* follow the loopback: Zoom's sound must not go silent (spec §4.3 step 4), and a room microphone's noise would otherwise hide a misrouted Zoom speaker.
- *The recording in mixed mode* lasts while either source is there. A source that goes leaves a `device_gone` gap in the recording, open-ended at first (durable at once, so a crash while it is gone still explains it) and closed with its end when the source joins again (`SourceEvent::GapEnd`). When both go, the recording ends, as a single source's does.
- *The fallback offer.* The single-input source keeps waiting for the same UID. While it waits, the person may choose another input; the source opens that one as a new recording, and the sidecar records its UID. It never switches by itself (spec §4.1). The CLI names the other inputs in its warning line.
- *Mixed ships* only if the two-hour simulated test passes (both ±100 ppm and the ±500 ppm stress) and the live two-hour run of two real devices shows no gap, no underrun and relative alignment within 10 ms. Otherwise `MIXED_MODE = false`: the CLI refuses `--mixed`, and the app offers no mixed choice.
- *Network loss* is produced by the forwarder (`LECTURELIVE_API_ADDR`). *Disk full* uses an HFS+ disk image. *Permission denial* is Terminal's Screen Recording in the lecture and its microphone at the desk, by the person's toggle only. *Forced restart* is `kill -9` of the app and a relaunch.
- *The audit* (`lecturelive lecture audit --dir …`) is the instrument for the gate's first line. Recordings are checked against their files, and the holes and overlaps between them against gaps and session markers; segments are checked for overlaps; transcript gaps not yet recovered are counted as waiting. It exits 1 on anything unexplained.
- *The error table's evidence* is a table in the findings. Each row has its evidence: a test against fakes, a synthetic lecture run here, or the real lecture (its check ids on `docs/VERIFICATION.html`).
- *Open threads,* each taken test-first in the task whose code it touches, or left with its reason (Task 16 lists them all):
  - *Taken:*
    - the capture-ring tail flake (Task 1);
    - the dropped `SourceEnded` (Task 6);
    - the last snapshot after a failed session (M3 minor, Task 6);
    - the session marker after a torn line (M3 minor, Task 6);
    - `course` from a relative `--dir` (M3 minor, Task 6);
    - `"error": null` read as an error (M3 minor, Task 5);
    - the gap count starting at zero (M4 minor M9, Task 8);
    - the strip still saying "Watching" after the end (M5 minor M1, Task 8);
    - the Screen Recording copy (M5 minor M2, Task 9);
    - unhandled `hydrate()` rejections (M4 minor M11, Task 9);
    - "Stop" offered while starting (M4 minor M6, Task 9);
    - the hash retries' pause (M4 minor M1, Task 8);
    - the golden tests' freeze (Task 12);
    - spec §8's slide fields, §13's keyring line and §14.3 (Task 14).
  - *Decided from measurement:* the REST first-word mishearing (M2), measured on the synthetic network-loss lecture's recovered lines (Task 10). The lead-in remedy is taken only if the measurement shows the error.
  - *Left:* the rest, each with its reason in the findings.
- *Decisions D1 and D2* keep their defaults: Polish only inside a lecture, and no production build. The sitting asks both once.

**Visual design** (the milestone-level `/frontend-design:frontend-design` run, 2026-09-26, before any M6 view is built; each view's task invokes it again):
- *Subject and job.* These views are what the app says when something stands between the person and a recording. At arm's length, mid-lecture, the person must see what stopped, that nothing was switched behind their back, and the one click that fixes it.
- *Tokens.* M4's, unchanged (`apps/desktop/src/lib/theme.css`): paper `#f4f6f8`, ink `#14213d`, graphite `#5a6478`, rule `#d6dce4`, plate `#e7ecf1`, signal `#b3261e` / `#f3dedc`, teal `#1d6b72` / `#d9eaeb`, with their dark values; Atkinson Hyperlegible Next, else the system face; 18 px at 1.2; radius 6 px on controls only.
- *Microphone fix-it* (before a lecture). It uses the command bar's existing error line, the place a person already reads for what went wrong: "▲ Microphone access is off, so nothing can be recorded. Turn it on in System Settings, then press Refresh." Beside it sits one outline button, "Open Settings", the word M5's strip uses for its Screen Recording fix-it. Start is disabled, with the reason as its title. The folder button, the source choice, Refresh and Study page stay usable.
- *Fallback offer* (during a lecture from one input). It takes the notice line's place above the prompt and stays until the device returns or the person chooses. The ▲ is in signal; the sentence is in ink, because it is a question, not an alarm: "**Wireless Mic Rx** is unplugged. LectureLive waits for it and records nothing meanwhile." Then "Record from [select]" and one outline button, "Record from it". The notice that follows says "Recording from MacBook Pro Microphone", keeping the button's verb. With no other input: "No other input is connected." It is an outline button because the command line stays the one bold element.
- *Mixed choice.* The same "Listen to" select, with one option per input after the single sources: "Zoom and MacBook Pro Microphone together". While one is chosen, a graphite hint in the bar reads "Wear headphones, so the microphone does not hear Zoom as well." (spec §4.1's documented setup). There are no gain controls.
- *Lecture occasion* (`docs/VERIFICATION.html`). The page's own run-sheet treatment for a class: the teal rule through the rows, with times in the gutter as time into the class ("0:40 in"). Each injected failure is its own item, in class order. Setup and the audit are steps.
- *Reviewed against the brief.*
  - A full-width red banner for the fallback became the notice line, because the lecture's other parts go on and the state can be recovered.
  - A modal for the microphone became the error line, because a modal would block "the rest works".
  - A separate mix toggle beside a second select became one option per pairing.
  - A filled button in the offer became an outline one, beside the filled Snapshot.
  - A two-hour timeline graphic on the checks page was dropped, because the gutter times are the timeline.
  - Coloured severity badges per failure were dropped.
  - "Waiting for it — or record from" (a spaced em dash splicing two thoughts) became two sentences.

```
Before a lecture, microphone denied:
┌ command bar ───────────────────────────────────────────────────────────────────────────────────┐
│ ▲ Microphone access is off, so nothing can be recorded. Turn it on in System Settings, then    │
│   press Refresh.  [Open Settings]                                                              │
│ [Change folder]  Listen to [Zoom through LectureLive Loopback ▾]  Refresh      [Start](off)     │
└────────────────────────────────────────────────────────────────────────────────────────────────┘
During a lecture, the receiver gone:
┌ command bar ───────────────────────────────────────────────────────────────────────────────────┐
│ ▲ Wireless Mic Rx is unplugged. LectureLive waits for it and records nothing meanwhile.        │
│   Record from [MacBook Pro Microphone ▾] [Record from it]                                      │
│ ◆ [a hint, or ⏎ for a snapshot                         ] [Snapshot] [Polish] [Study page] [Stop]│
└────────────────────────────────────────────────────────────────────────────────────────────────┘
```

**Execution:** executing-plans, inline, without approval stops. The human steps are one sitting (Task 15), scheduled after everything that can finish without the person. The two-hour real lecture needs a lecture and cannot run on demand: Task 13 writes it onto `docs/VERIFICATION.html`, and a follow-up session reads its evidence. Task 17 (removing the Python CLI) runs only once that lecture's gate lines hold. A fresh reviewer on the most capable model (`opus`) reviews the whole branch before the findings commit (Task 16). Ledger: `.superpowers/sdd/2026-09-26-m6-full-lecture/progress.md` (git-excluded through `.git/info/exclude`).

**The one sitting (Task 15), listed up front; about two minutes of the person's time:**
1. D1: may Polish run after a lecture has ended? (Default: no, as today.)
2. D2: may M6 build the production app once, so the built app's CSP is checked? (Default: no.)

**The two-hour lecture (Task 13 writes it; the person runs it at a real Zoom lecture, a follow-up session reads it):** set up before class (forwarder, disk image, the app in mixed mode with the receiver, the Python CLI in its own folder), then in class order: V01's first 30 minutes clean; V10 network loss (forwarder stopped 60 s, later killed 30 s); V11 receiver unplugged 30 s; V12 Screen Recording turned off for Terminal for a minute; V13 disk full; V14 `kill -9` and a relaunch; V03 and V04 alongside; after class `lecture audit` and the Study page.

## Global Constraints

- macOS 13+, Apple Silicon; one user; no telemetry, accounts or servers.
- Toolchain pinned to `rustc 1.96.1` (`rust-toolchain.toml`); workspace `rust-version` 1.89.
- Builds authorised: `cargo build`, `test`, `run`, `check`, `tree`, `add` (debug profile). Frontend, in `apps/desktop`: `npm install`, `npm run dev`, `npm run tauri dev`, `npx vitest run`, `npx svelte-check`. Not authorised: `npm run build`, `npm run tauri build`, any packaging, signing or notarising, anything that writes under `target/release`, `cargo clean`, `cargo build --release` of the desktop crate. Only D2 answered yes can change this.
- The packaged "LectureLive Canary.app" in `target/release/bundle` holds this Mac's Microphone and Screen Recording grants, tied to its ad-hoc signature. It is never rebuilt, re-signed, moved or cleaned, and no `tccutil reset` touches its grants. `tauri.conf.json` changes only in `app` and `security`; `identifier`, `productName` and `bundle` stay as they are. Dev runs build `target/debug/desktop`.
- **Every task that creates or changes a view invokes `/frontend-design:frontend-design` before any markup is written** (spec §9.4): the microphone fix-it, the fallback offer, the mixed choice, the Screen Recording copy, the lecture occasion on `docs/VERIFICATION.html`. Work within the design direction above: M4's palette light and dark, Atkinson Hyperlegible Next else the system face, 18 px base at ratio 1.2, flat ruled columns, a hanging "when" gutter, a teal rule for what is live, no decorative motion. **Check every view at 1168 px wide in normal and large type, light and dark**: that is the person's display, and at M5 a layout that passed at 1280 clipped there. A view built before the skill ran is rebuilt after it. Each run is recorded in the ledger and the Findings.
- Svelte 5 runes only: `$state`, `$state.raw`, `$derived`, event-driven updates. No `$effect` or any other effect hook; DOM wiring that needs an element uses an action (`use:`).
- When a dev server runs, load only its localhost port (`localhost:1420`) in a headless or chromeless window. Playwright writes screenshots only under `/tmp/playwright-mcp`; delete yours after reading and leave another project's files there.
- Checks run the debug app from the shell: `cd apps/desktop && npm run dev` (port 1420), then `LECTURELIVE_CHECK=<mode> LECTURELIVE_CHECK_DIR=<folder> ./target/debug/desktop`. A check run reads `GROK_API_KEY` from `.env`, because the debug binary's Keychain read prompts after each relink. Reports go to `$HOME/Library/Application Support/LectureLive/m6-checks/<name>.json`.
- Live calls on synthetic lectures only: at most $2 in total, each request's cost recorded from `usage.cost_in_usd_ticks`. STT with synthesised speech only (`say -a "BlackHole 2ch"`, non-repeating text with `[[slnc 1200]]` pauses). Before any speech run, kill every leftover `say` and its loop shell. The real lecture's spend is the person's normal use: report it, do not cap it.
- Never run migration, initialisation, recovery or polish on a folder that holds a real lecture's notes, except the real lecture's own run through the app. Synthetic folders live under `$HOME/Library/Application Support/LectureLive/m6-*`.
- Any check that changes the default output ends with the output it began with (normally "MacBook Pro Speakers"). `say -a` changes nothing.
- Never capture a window that may show someone else's content, except Zoom's in a lecture the person runs. Room audio from the built-in microphone is never written to disk or sent: the live mixed run counts it in memory only.
- The repository is public. Commit no audio except synthesised `say` output, no real lecture notes or transcript, no image that is not synthetic. Write paths in docs as `$HOME/…`. Stage files by name, never `git add -A`.
- Commit messages: imperative, neutral, what and why. No `Co-Authored-By` or any other attribution line, whatever a harness reminder says. No names.
- Do not edit files with Python scripts. In the Bash tool `ls` is `eza` and `grep` is a function: use `command ls` / `command grep` where output is evidence. macOS has no `timeout` command. `cargo test` stops at the first failing test binary: run with `--no-fail-fast` to see every binary.
- Test fakes: draw a fake window's new image before resizing it (M5).
- `live_notes.py`, `pyproject.toml`, `README.md` and the golden tests that read `live_notes.py` change only in Tasks 12 and 17. `notes_template.html` stays unchanged. In `docs/`, only this plan, the M6 section and M6 Status row of `docs/milestones.md`, spec §4.2, §8's slide fields, §10, §11, §13's keyring and rubato lines and §14.3, and `docs/VERIFICATION.html` change.
- APIs below are written against the versions named. Where one differs, adapt the implementation and keep the task's tests unchanged: the tests are the contract. The exception is a test encoding an unverified fact about an external system that live evidence contradicts; change it only with the evidence recorded as a Ruling in the ledger.
- When a check fails, record the observation and the spec §14.1 fallback it points to; do not build the fallback. §14.1 covers loopback only.

## Review Focus

1. **In mixed mode, a source stalls for seconds and then delivers its backlog at once** (a USB receiver re-pairing, a driver hiccup). Expected: the stall is a gap for that source, the recording goes on with the other source, and within a second of the backlog the source is aligned again rather than half a second late for minutes. Pinned by `mix::tests::a_source_that_stalls_and_then_delivers_its_backlog_joins_again_where_it_belongs` (Task 2).
2. **In mixed mode, Zoom falls silent while the room microphone hears sound** (Zoom's speaker drifted to a physical device). Expected: the ten-second silence warning still fires, because the level it watches is the loopback's. Pinned by `mixed::tests::the_level_follows_the_loopback_even_when_the_input_is_loud` (Task 3).
3. **The fallback the person chose is gone by the time it is clicked**, or cannot be opened. Expected: the source keeps waiting for the original device, the offer stays, and it is never stuck on neither. Pinned by `source::tests::a_fallback_that_cannot_be_found_keeps_the_wait_for_the_original` (Task 4).
4. **The disk fills while the sidecar or the segment log is written, not the WAV.** Expected: the session stops cleanly with that file's path in the message, and the last snapshot is still attempted. Pinned by `coordinator::tests::a_sidecar_that_cannot_be_saved_stops_the_session_with_its_path` and `lecture_gate::a_failed_session_still_takes_its_last_snapshot` (Task 6).
5. **The audit meets a folder whose last recording was never repaired** (a crash, and no session since). Expected: it says the recording waits for repair and exits nonzero; it never reports the folder whole. Pinned by `audit::tests::an_unrepaired_recording_is_waiting_not_whole` (Task 7).

## File Structure

| File | Responsibility | Task |
|---|---|---|
| `crates/core/src/audio/capture.rs` | The ring publishes a pending drop when its producer ends; the capture latency and last-callback time | 1, 3 |
| `Cargo.toml` (root) | Dev-profile optimisation of `rubato`, `realfft`, `rustfft` | 2 |
| `crates/core/src/audio/mix.rs` | The mixer: lanes, drift stages, FIFOs, steering, resync, gaps | 2 |
| `crates/core/src/audio/mixed.rs` | `MixedSource`, `MIXED_MODE`, the two-lane run loop, the live drift test | 3 |
| `crates/core/src/audio/source.rs` | `open_stream` shared; `SourceEvent::GapEnd`; fallback in `supervise`, `DeviceSource.fallback`, `Fallback` | 3, 4 |
| `crates/core/src/session/coordinator.rs` | `GapEnd`; `SourceEnded` always delivered | 3, 6 |
| `crates/core/src/net.rs` | `API_ADDR_VAR`, `api_addr()` | 5 |
| `crates/core/src/stt/stream.rs`, `stt/rest.rs`, `notes/chat.rs` | `connect_to`; chat's `"error": null` | 5 |
| `crates/core/examples/netcut.rs` | The forwarder a check stops and starts | 5 |
| `crates/core/src/session/lecture.rs` | The last snapshot after a failed session | 6 |
| `crates/core/src/session/segments.rs` | The session marker on a line of its own | 6 |
| `crates/core/src/session/audit.rs` | The audit | 7 |
| `crates/cli/src/main.rs` | `--mixed`; the fallback line; `course` after the folder is absolute; `lecture audit` | 3, 4, 6, 7 |
| `apps/desktop/src-tauri/src/{app.rs,adapter.rs,wire.rs,lib.rs}` | Microphone state and settings, `use_input`, mixed start, `SourceKind`, `input_gone`, gaps from the sidecar, capture after the end, hash-retry pause, check minutes, `m6-checks` | 8 |
| `apps/desktop/src/lib/{wire.ts,transport.ts,fixture.ts,session.svelte.ts,session.test.ts}` | The store for the new states and commands; hydrate rejections; Stop while starting | 9 |
| `apps/desktop/src/lib/{FolderPicker.svelte,CommandLine.svelte,SlidesStrip.svelte}` | Microphone fix-it, mixed choice, fallback offer, Screen Recording copy | 9 |
| `apps/desktop/src/lib/checks.ts`, `apps/desktop/src/routes/+page.svelte` | `faultsCheck` | 9 |
| `crates/core/src/notes/prompts.rs`, `crates/core/tests/fixtures/prompts/live_notes_excerpt.txt` | Goldens frozen | 12 |
| `docs/VERIFICATION.html` | The lecture occasion; V01–V04, V06, V09 revised; new desk checks | 13 |
| `docs/spec.md` §4.2, §8, §10, §11, §13, §14.3 | As built | 14 |
| `docs/milestones.md` M6 section + Status row | Gate, Findings, Status | 16 |
| `live_notes.py`, `pyproject.toml`, `README.md` | Removed; README on the app and the Rust CLI (only after the lecture) | 17 |

---

### Task 1: The capture ring places a drop still pending when its stream ends (M5 thread: the `concurrent_overflow` flake)

**Files:**
- Modify: `crates/core/src/audio/capture.rs`

**Interfaces:**
- Produces: `impl Drop for CaptureProducer` (records the end position and marks the stream ended); `CaptureConsumer::drain` delivers `Chunk::Silence(end − pos)` once the stream has ended and nothing else waits; `CaptureConsumer::available(&self) -> u64` (input frames waiting). `StreamFlags` gains private `ended: AtomicBool`, `end_frame: AtomicU64`.

The mechanism, from M5: when the drop-event ring (256 slots) is full, the producer holds the drop in `pending` and pushes no audio after it. A producer that stops at that moment takes the pending drop with it. The consumer never learns of that stretch, and the recording ends early without a mark. That is the flake in the test, and a lost drop marker in a recording whose consumer is starved at its end.

- [ ] **Step 1: Write the failing test** in `capture.rs` `mod tests`:

```rust
    /// M5 open thread: a drop still pending when the stream ends (its event ring was full) reaches the
    /// consumer as silence at its exact place, so the recording's last stretch is never lost unmarked.
    #[test]
    fn a_drop_still_pending_when_the_stream_ends_reaches_the_consumer() {
        let (mut p, mut c, _) = small_ring(1, 8);
        p.push(&[1.0; 8]); // the ring is full
        for _ in 0..DROP_EVENTS + 1 {
            p.push(&[2.0; 1]); // one event per overflow; the last finds the event ring full
        }
        drop(p); // the stream ends with that drop still pending
        assert_eq!(collect(&mut c), ["audio 8x1".to_string(), format!("silence {DROP_EVENTS}"), "silence 1".to_string()]);
        assert_eq!(c.available(), 0);
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p lecturelive-core audio::capture 2>&1 | tail -15`
Expected: FAIL to compile (`available` does not exist), then, once it exists, FAIL with `left: ["audio 8x1", "silence 256"]`.

- [ ] **Step 3: Implement.** In `StreamFlags` add `ended: AtomicBool, end_frame: AtomicU64` (both default). Give `CaptureConsumer` a `flags: Arc<StreamFlags>` (set in `ring_with_capacity`). Add:

```rust
impl Drop for CaptureProducer {
    /// A drop still pending (its event ring was full) is published through the end position, so the
    /// consumer places it: a stream's last stretch is never lost without a mark.
    fn drop(&mut self) {
        self.flags.end_frame.store(self.pos, Ordering::Relaxed);
        self.flags.ended.store(true, Ordering::Release);
    }
}
```

In `drain`, read `let ended = self.flags.ended.load(Ordering::Acquire);` at the top of each loop pass, before `available`. Every push happened before the end was marked, so once it is seen, `available` counts all the audio. Replace `if limit == 0 { return Ok(()); }` with:

```rust
            if limit == 0 {
                let end = self.flags.end_frame.load(Ordering::Relaxed);
                if ended && end > self.pos && self.drops.is_empty() {
                    f(Chunk::Silence(end - self.pos))?;
                    self.pos = end;
                }
                return Ok(());
            }
```

and add `pub fn available(&self) -> u64 { (self.data.slots() / self.channels) as u64 }`.

- [ ] **Step 4: Run the capture tests, then the flake's test 50 times under load**

Run: `cargo test -p lecturelive-core audio::capture 2>&1 | tail -5`, then `for i in $(seq 50); do cargo test -q -p lecturelive-core concurrent_overflow 2>&1 | command grep -E "test result|FAILED" ; done | sort | uniq -c` while `cargo test -p lecturelive-core --no-fail-fast -q` runs beside it.
Expected: all pass; 50 × `test result: ok`.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/audio/capture.rs
git commit -m "Place a capture drop still pending when the stream ends, so a recording's last stretch is never lost without a mark"
```

---

### Task 2: The mixer and the two-hour drift test over simulated clocks (milestone task 1, core)

**Files:**
- Create: `crates/core/src/audio/mix.rs`
- Modify: `crates/core/src/audio/mod.rs` (`pub mod mix;`), `Cargo.toml` (root)

**Interfaces:**
- Consumes: `audio::convert::{downmix, Resampler16k}` (M1).
- Produces (`audio::mix`):
  - `pub const RATE: f64 = 16_000.0; pub const DELAY: u64 = 3_200;`
  - `#[derive(Debug, Clone, Copy)] pub struct LaneConfig { pub rate: u32, pub channels: u16, pub gain: f32 }`
  - `#[derive(Debug, Clone, Copy, PartialEq)] pub struct LaneStats { pub level: usize, pub filtered: f64, pub target: Option<f64>, pub correction_ppm: f64, pub underrun: u64 }`
  - `pub struct Pulled { pub samples: Vec<f32>, pub gaps: Vec<(usize, Range<u64>)> }`
  - `pub struct Mixer`, with:
    - `new(lanes: usize)`;
    - `join(&mut self, i, cfg, age_secs: f64) -> Result<u64>`: where the source's oldest waiting sample lands;
    - `push(&mut self, i, interleaved: &[f32]) -> Result<()>`;
    - `push_silence(&mut self, i, input_frames: u64) -> Result<Range<u64>>`;
    - `leave(&mut self, i) -> Result<u64>`: where its audio ends;
    - `is_joined(&self, i) -> bool`;
    - `any(&self) -> bool`;
    - `pull(&mut self, due: u64) -> Pulled`;
    - `flush(&mut self) -> Vec<f32>`;
    - `emitted(&self) -> u64`;
    - `stats(&self, i) -> Option<LaneStats>`;
    - `level(&mut self, i) -> Option<f32>`: the RMS of what the source contributed since the last call, before its gain.

- [ ] **Step 1: Write the failing tests** at the end of the new `mix.rs` (the module's body is written in Step 3):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// A device on its own clock: `ppm` fast or slow against the host clock, started at host time `start`,
    /// delivering whole blocks `latency` seconds after capture, with a unit impulse at each click (host
    /// seconds). A stall from `a` to `b` delivers nothing; with `backlog` the device kept capturing and hands
    /// the stretch over at `b`, without it what fell in the stall was never captured.
    struct Device { rate: u32, ppm: f64, start: f64, block: u64, latency: f64, sent: u64, clicks: Vec<f64>, next_click: usize, stall: Option<(f64, f64, bool)> }

    impl Device {
        fn new(rate: u32, ppm: f64, start: f64, block: u64, latency: f64, clicks: Vec<f64>) -> Self {
            Self { rate, ppm, start, block, latency, sent: 0, clicks, next_click: 0, stall: None }
        }
        fn true_rate(&self) -> f64 {
            self.rate as f64 * (1.0 + self.ppm * 1e-6)
        }
        fn delivered_by(&self, t: f64) -> u64 {
            let captured = ((t - self.latency - self.start).max(0.0) * self.true_rate()) as u64;
            captured / self.block * self.block
        }
        /// Seconds since the oldest frame not yet taken was captured.
        fn age(&self, t: f64) -> f64 {
            t - (self.start + self.sent as f64 / self.true_rate())
        }
        fn take(&mut self, t: f64) -> Vec<f32> {
            if let Some((a, b, backlog)) = self.stall {
                if t >= a && t < b {
                    return Vec::new();
                }
                if t >= b && !backlog && self.sent < self.delivered_by(b) - self.block {
                    self.sent = self.delivered_by(b); // never captured
                    while self.clicks.get(self.next_click).is_some_and(|&c| ((c - self.start) * self.true_rate()) as u64 <= self.sent) {
                        self.next_click += 1;
                    }
                }
            }
            let to = self.delivered_by(t);
            let mut v = vec![0.0f32; (to - self.sent) as usize];
            while let Some(&c) = self.clicks.get(self.next_click) {
                let n = ((c - self.start) * self.true_rate()).round() as u64;
                if n >= to {
                    break;
                }
                if n >= self.sent {
                    v[(n - self.sent) as usize] = 1.0;
                }
                self.next_click += 1;
            }
            self.sent = to;
            v
        }
    }

    /// Output positions of the impulses: local maxima above 0.1.
    #[derive(Default)]
    struct Peaks { pos: u64, prev: f32, rising: bool, found: Vec<u64> }

    impl Peaks {
        fn scan(&mut self, s: &[f32]) {
            for &x in s {
                if x > 0.1 && x >= self.prev {
                    self.rising = true;
                } else if self.rising && x < self.prev {
                    self.found.push(self.pos - 1);
                    self.rising = false;
                }
                self.prev = x;
                self.pos += 1;
            }
        }
    }

    struct Run { peaks: Vec<u64>, t0: f64, underrun: [u64; 2], gaps: Vec<(usize, Range<u64>)>, emitted: u64, due: u64 }

    /// The host clock ticks every 15–25 ms; at each tick every device hands over what it has delivered and
    /// the mixer takes what is due.
    fn simulate(devices: &mut [Device; 2], secs: f64, steering: bool) -> Run {
        let mut m = Mixer::new(2);
        m.steering = steering;
        let (mut seed, mut t, mut t0, mut due) = (7u32, 0.0f64, None::<f64>, 0u64);
        let (mut peaks, mut gaps) = (Peaks::default(), Vec::new());
        while t < secs {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            t += 0.015 + ((seed >> 16) % 10) as f64 / 1000.0;
            for (i, d) in devices.iter_mut().enumerate() {
                let age = d.age(t);
                let data = d.take(t);
                if data.is_empty() {
                    continue;
                }
                if !m.is_joined(i) {
                    t0.get_or_insert(t);
                    m.join(i, LaneConfig { rate: d.rate, channels: 1, gain: 1.0 }, age).unwrap();
                }
                m.push(i, &data).unwrap();
            }
            if let Some(t0) = t0 {
                due = ((t - t0) * RATE) as u64;
                let p = m.pull(due);
                gaps.extend(p.gaps);
                peaks.scan(&p.samples);
            }
        }
        Run { peaks: peaks.found, t0: t0.unwrap(), underrun: [0, 1].map(|i| m.stats(i).map_or(0, |s| s.underrun)), gaps, emitted: m.emitted(), due }
    }

    fn clicks(first: f64, until: f64) -> Vec<f64> {
        (0..).map(|k| first + 10.0 * k as f64).take_while(|&h| h < until).collect()
    }

    /// Each click's error in output samples: where it landed, less where its capture time belongs (DELAY
    /// behind the host clock). Clicks of the two devices are 5 s apart, so a window of 2 s tells them apart.
    fn errors(run: &Run, clicks: &[f64]) -> Vec<(f64, f64)> {
        clicks
            .iter()
            .filter_map(|&h| {
                let want = (h - run.t0) * RATE + DELAY as f64;
                run.peaks.iter().map(|&p| p as f64 - want).find(|e| e.abs() < 2.0 * RATE).map(|e| (h, e))
            })
            .collect()
    }

    fn worst_after(e: &[(f64, f64)], after: f64) -> f64 {
        e.iter().filter(|(h, _)| *h > after).map(|(_, x)| x.abs()).fold(0.0, f64::max)
    }

    /// The gate (spec §4.2, §11): two hours, 48 kHz at +100 ppm beside 44.1 kHz at −100 ppm, with different
    /// latencies and start times. Without steering they part by 0.72 s.
    #[test]
    fn two_sources_on_drifting_clocks_stay_aligned_for_two_hours() {
        const SECS: f64 = 7_200.0;
        let (a, b) = (clicks(1.0, SECS - 2.0), clicks(6.0, SECS - 2.0));
        let mut devices = [Device::new(48_000, 100.0, 0.0, 512, 0.005, a.clone()), Device::new(44_100, -100.0, 0.007, 441, 0.012, b.clone())];
        let run = simulate(&mut devices, SECS, true);
        assert_eq!(run.emitted, run.due, "the mix emits exactly what the host clock says is due");
        assert_eq!(run.underrun, [0, 0], "no source ran dry");
        assert!(run.gaps.is_empty(), "{:?}", &run.gaps[..run.gaps.len().min(5)]);
        for (name, cl) in [("48 kHz, +100 ppm", &a), ("44.1 kHz, −100 ppm", &b)] {
            let e = errors(&run, cl);
            assert_eq!(e.len(), cl.len(), "{name}: every click found");
            let worst = worst_after(&e, 60.0);
            println!("{name}: first click {:+.0} samples, worst after 60 s {worst:.0} samples ({:.1} ms)", e[0].1, worst / 16.0);
            assert!(e[0].1.abs() <= 16.0, "{name}: the first click lands where its capture time belongs: {:+.0}", e[0].1);
            assert!(worst <= 160.0, "{name}: {worst} samples off after 60 s");
        }
    }

    #[test]
    fn five_hundred_ppm_either_way_is_held_within_20_ms() {
        const SECS: f64 = 1_800.0;
        let (a, b) = (clicks(1.0, SECS - 2.0), clicks(6.0, SECS - 2.0));
        let mut devices = [Device::new(48_000, 500.0, 0.0, 480, 0.004, a.clone()), Device::new(48_000, -500.0, 0.003, 512, 0.010, b.clone())];
        let run = simulate(&mut devices, SECS, true);
        assert_eq!(run.underrun, [0, 0]);
        for cl in [&a, &b] {
            let worst = worst_after(&errors(&run, cl), 60.0);
            println!("±500 ppm: worst after 60 s {worst:.0} samples");
            assert!(worst <= 320.0, "{worst}");
        }
    }

    /// The negative control: the same drift without steering parts the sources, so the steering is what holds them.
    #[test]
    fn without_steering_the_same_drift_parts_the_sources() {
        const SECS: f64 = 1_800.0;
        let (a, b) = (clicks(1.0, SECS - 2.0), clicks(6.0, SECS - 2.0));
        let mut devices = [Device::new(48_000, 500.0, 0.0, 480, 0.004, a.clone()), Device::new(48_000, -500.0, 0.003, 512, 0.010, b.clone())];
        let run = simulate(&mut devices, SECS, false);
        let fast = worst_after(&errors(&run, &a), 60.0);
        println!("unsteered: the fast source {fast:.0} samples late, the slow one ran dry for {} samples", run.underrun[1]);
        assert!(fast > 8_000.0, "{fast}");
        assert!(run.underrun[1] > 0);
    }

    /// Review Focus 1.
    #[test]
    fn a_source_that_stalls_and_then_delivers_its_backlog_joins_again_where_it_belongs() {
        const SECS: f64 = 120.0;
        let (a, b) = (clicks(1.0, SECS - 2.0), clicks(6.0, SECS - 2.0));
        let mut stalled = Device::new(48_000, 0.0, 0.0, 480, 0.01, b.clone());
        stalled.stall = Some((30.0, 32.5, true));
        let mut devices = [Device::new(48_000, 0.0, 0.0, 512, 0.005, a.clone()), stalled];
        let run = simulate(&mut devices, SECS, true);
        let stall_at = ((30.0 - run.t0) * RATE) as u64;
        assert!(run.gaps.iter().any(|(i, g)| *i == 1 && g.start >= stall_at && g.start <= stall_at + 2 * DELAY), "the stall is a gap for that source: {:?}", run.gaps);
        let after: Vec<f64> = errors(&run, &b).into_iter().filter(|(h, _)| *h > 34.0).map(|(_, x)| x.abs()).collect();
        assert!(!after.is_empty() && after.iter().all(|&x| x <= 160.0), "{after:?}");
        assert!(errors(&run, &a).iter().all(|(_, x)| x.abs() <= 160.0), "the other source never moved");
    }

    #[test]
    fn a_source_that_stops_capturing_for_a_while_joins_again_where_it_belongs() {
        const SECS: f64 = 120.0;
        let (a, b) = (clicks(1.0, SECS - 2.0), clicks(6.0, SECS - 2.0));
        let mut stalled = Device::new(48_000, 0.0, 0.0, 480, 0.01, b.clone());
        stalled.stall = Some((30.0, 32.5, false));
        let mut devices = [Device::new(48_000, 0.0, 0.0, 512, 0.005, a.clone()), stalled];
        let run = simulate(&mut devices, SECS, true);
        assert!(run.gaps.iter().any(|(i, _)| *i == 1));
        let after: Vec<f64> = errors(&run, &b).into_iter().filter(|(h, _)| *h > 34.0).map(|(_, x)| x.abs()).collect();
        assert!(!after.is_empty() && after.iter().all(|&x| x <= 160.0), "{after:?}");
    }

    #[test]
    fn each_source_s_gain_applies_before_the_sum_and_only_the_sum_is_clamped() {
        let mut m = Mixer::new(2);
        let age = DELAY as f64 / RATE; // nothing in front of either
        m.join(0, LaneConfig { rate: 16_000, channels: 1, gain: 0.5 }, age).unwrap();
        m.join(1, LaneConfig { rate: 16_000, channels: 2, gain: 1.0 }, age).unwrap();
        m.push(0, &vec![0.8; 16_000]).unwrap();
        m.push(1, &vec![0.3; 32_000]).unwrap(); // 16,000 stereo frames of 0.3
        let out = m.pull(8_000).samples;
        assert!((out[4_000] - 0.7).abs() < 1e-3, "0.5 × 0.8 + 0.3: {}", out[4_000]);
        let mut loud = Mixer::new(2);
        loud.join(0, LaneConfig { rate: 16_000, channels: 1, gain: 1.0 }, age).unwrap();
        loud.join(1, LaneConfig { rate: 16_000, channels: 1, gain: 1.0 }, age).unwrap();
        loud.push(0, &vec![0.8; 16_000]).unwrap();
        loud.push(1, &vec![0.6; 16_000]).unwrap();
        let out = loud.pull(8_000).samples;
        assert_eq!(out[4_000], 1.0, "only the sum is limited");
        assert_eq!(loud.level(0).map(|l| (l * 100.0).round()), Some(80.0), "a source's level is its own, before the gain");
    }

    #[test]
    fn input_lost_to_overflow_is_silence_where_it_would_have_played() {
        let mut m = Mixer::new(1);
        let at = m.join(0, LaneConfig { rate: 48_000, channels: 1, gain: 1.0 }, DELAY as f64 / RATE).unwrap();
        assert_eq!(at, 0);
        m.push(0, &vec![0.5; 48_000]).unwrap(); // 1 s
        let lost = m.push_silence(0, 24_000).unwrap(); // 0.5 s lost
        m.push(0, &vec![0.5; 48_000]).unwrap();
        assert!((lost.start as i64 - 16_000).abs() <= 16 && (lost.end as i64 - 24_000).abs() <= 16, "{lost:?}");
        let out = m.pull(36_000).samples;
        assert!(out[20_000].abs() < 0.01 && out[8_000] > 0.45 && out[32_000] > 0.45);
    }

    #[test]
    fn a_source_that_leaves_plays_out_what_it_delivered_and_joins_again_behind_silence() {
        let mut m = Mixer::new(2);
        let age = DELAY as f64 / RATE;
        m.join(0, LaneConfig { rate: 16_000, channels: 1, gain: 1.0 }, age).unwrap();
        m.join(1, LaneConfig { rate: 16_000, channels: 1, gain: 1.0 }, age).unwrap();
        m.push(0, &vec![0.25; 64_000]).unwrap();
        m.push(1, &vec![0.5; 16_000]).unwrap();
        m.pull(8_000);
        let ends = m.leave(1).unwrap();
        assert!((ends as i64 - 16_000).abs() <= 16, "its audio ends where its last sample lands: {ends}");
        let out = m.pull(24_000).samples; // positions 8,000..24,000
        assert!((out[4_000] - 0.75).abs() < 0.01 && (out[12_000] - 0.25).abs() < 0.01, "then only the other");
        assert!(!m.is_joined(1) && m.any());
        let back = m.join(1, LaneConfig { rate: 16_000, channels: 1, gain: 1.0 }, 0.0).unwrap();
        assert_eq!(back, 24_000 + DELAY, "a sample captured now lands DELAY behind the host clock");
        m.push(1, &vec![0.5; 16_000]).unwrap();
        let out = m.pull(40_000).samples; // 24,000..40,000
        assert!((out[1_000] - 0.25).abs() < 0.01 && (out[(DELAY as usize) + 1_000] - 0.75).abs() < 0.01);
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p lecturelive-core audio::mix 2>&1 | tail -15`
Expected: FAIL to compile (`Mixer`, `LaneConfig` do not exist).

- [ ] **Step 3: Implement.** Root `Cargo.toml`, below the `xcap` override:

```toml
# Mixed mode resamples two sources and steers a drift stage per source (spec §4.2); its two-hour test over
# simulated clocks runs in `cargo test` only with these optimised (M6 plan research).
[profile.dev.package.rubato]
opt-level = 3

[profile.dev.package.realfft]
opt-level = 3

[profile.dev.package.rustfft]
opt-level = 3
```

`crates/core/src/audio/mix.rs` (above the tests):

```rust
//! Mixed mode (spec §4.2): the loopback and an input, on independent clocks, become one 16 kHz stream.
//! Each source is downmixed and resampled to 16 kHz at its nominal rate (the FFT resampler single sources
//! use), then passes a drift stage, rubato's asynchronous polynomial resampler at a ratio near 1, into a
//! FIFO. The mixer takes from every FIFO exactly the samples the host clock says are due. A source joins
//! with silence in front of it, so its first sample lands where its capture time belongs, DELAY behind the
//! host clock; its drift stage is then steered to hold its FIFO at the level it settled at, however far
//! its clock drifts.
use std::collections::VecDeque;
use std::ops::Range;

use anyhow::Result;
use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Adjustable, Async, FixedAsync, PolynomialDegree, Resampler};

use super::convert::{downmix, Resampler16k};

pub const RATE: f64 = 16_000.0;
/// How far the mix runs behind the host clock (200 ms): room for a late callback or a slow tick.
pub const DELAY: u64 = 3_200;
/// Seconds after a source joins before its smoothed level becomes its target and steering starts.
const SETTLE: f64 = 4.0;
/// Seconds over which the FIFO level is smoothed before it steers.
const LEVEL_TAU: f64 = 2.0;
/// Steering per sample of level error, and per sample-second of its integral (about 20 s to settle).
const KP: f64 = 3.0e-6;
const KI: f64 = 4.0e-8;
/// The drift stage stays within 0.2% of a ratio of 1: far beyond any crystal.
const MAX_CORRECTION: f64 = 0.002;
/// A level this far from its target (100 ms) is not drift but a stall or a burst: the source realigns. Callback
/// blocks and the FFT resampler's chunks move the level by at most about 600 samples.
const RESYNC: f64 = 1_600.0;
const DRIFT_CHUNK: usize = 160;
const SILENCE_BLOCK: u64 = 4_096;

#[derive(Debug, Clone, Copy)]
pub struct LaneConfig {
    pub rate: u32,
    pub channels: u16,
    pub gain: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LaneStats {
    pub level: usize,
    pub filtered: f64,
    pub target: Option<f64>,
    pub correction_ppm: f64,
    pub underrun: u64,
}

pub struct Pulled {
    pub samples: Vec<f32>,
    /// Stretches of the output that miss a source's audio: it ran dry, or its excess was dropped to realign.
    pub gaps: Vec<(usize, Range<u64>)>,
}

struct Lane {
    channels: usize,
    input_rate: f64,
    gain: f32,
    to16k: Option<Resampler16k>,
    drift: Async<f32>,
    drift_in: Vec<f32>,
    skip: usize,
    fifo: VecDeque<f32>,
    /// Where the source's first kept sample lands, and the input frames it has sent since.
    origin: u64,
    in_frames: u64,
    /// Input frames at the front older than DELAY when the source joined: dropped, so it is not late for good.
    skip_input: u64,
    /// A stretch without audio has just ended.
    resumed: bool,
    age: f64,
    filtered: f64,
    target: Option<f64>,
    integral: f64,
    correction: f64,
    underrun: u64,
    dry_since: Option<u64>,
    draining: bool,
    sq: f64,
    n: u64,
}

impl Lane {
    fn new(cfg: LaneConfig, origin: u64, prefill: usize, skip_input: u64) -> Result<Self> {
        let drift = Async::<f32>::new_poly(1.0, 1.01, PolynomialDegree::Septic, DRIFT_CHUNK, 1, FixedAsync::Input)?;
        let skip = drift.output_delay();
        Ok(Self {
            channels: cfg.channels.max(1) as usize,
            input_rate: cfg.rate as f64,
            gain: cfg.gain,
            to16k: Some(Resampler16k::new(cfg.rate)?),
            drift,
            drift_in: Vec::new(),
            skip,
            fifo: std::iter::repeat_n(0.0, prefill).collect(),
            origin,
            in_frames: 0,
            skip_input,
            resumed: false,
            age: 0.0,
            filtered: 0.0,
            target: None,
            integral: 0.0,
            correction: 0.0,
            underrun: 0,
            dry_since: None,
            draining: false,
            sq: 0.0,
            n: 0,
        })
    }

    fn feed16k(&mut self, s: &[f32]) -> Result<()> {
        self.drift_in.extend_from_slice(s);
        while self.drift_in.len() >= self.drift.input_frames_next() {
            let n = self.drift.input_frames_next();
            let out = self.drift.process(&InterleavedSlice::new(&self.drift_in[..n], 1, n)?, None)?.take_data();
            self.drift_in.drain(..n);
            let skip = self.skip.min(out.len());
            self.skip -= skip;
            self.fifo.extend(&out[skip..]);
        }
        Ok(())
    }

    fn push(&mut self, interleaved: &[f32]) -> Result<()> {
        let skip = (self.skip_input as usize).min(interleaved.len() / self.channels);
        self.skip_input -= skip as u64;
        let interleaved = &interleaved[skip * self.channels..];
        self.in_frames += (interleaved.len() / self.channels) as u64;
        let Some(r) = self.to16k.as_mut() else { return Ok(()) };
        let s = r.push(&downmix(interleaved, self.channels))?;
        self.feed16k(&s)
    }

    /// Where input frame `k` lands in the output.
    fn position(&self, k: u64) -> u64 {
        self.origin + (k as f64 * RATE / self.input_rate).round() as u64
    }

    /// The source has gone: the resamplers' tails go into the FIFO, which then plays out.
    fn finish(&mut self) -> Result<()> {
        if let Some(r) = self.to16k.take() {
            let s = r.finish()?;
            self.feed16k(&s)?;
        }
        let rest: Vec<f32> = self.drift_in.drain(..).collect();
        self.fifo.extend(rest); // under 10 ms, at a ratio within 0.2% of 1
        self.draining = true;
        Ok(())
    }

    /// Adds this source's next `out.len()` samples to `out`, which begins at output position `at`. Returns
    /// a stretch it could not fill that has just ended.
    fn take(&mut self, out: &mut [f32], at: u64) -> Option<Range<u64>> {
        let mut ended = None;
        for (k, o) in out.iter_mut().enumerate() {
            match self.fifo.pop_front() {
                Some(s) => {
                    *o += self.gain * s;
                    self.sq += (s as f64) * (s as f64);
                    self.n += 1;
                    if let Some(from) = self.dry_since.take() {
                        ended = Some(from..at + k as u64);
                        self.resumed = true;
                    }
                }
                None if !self.draining => {
                    self.underrun += 1;
                    self.dry_since.get_or_insert(at + k as u64);
                }
                None => {}
            }
        }
        ended
    }

    /// Steers the drift stage toward the target level; a level past RESYNC realigns at once. `at` is the
    /// output position after this pull. Returns a stretch dropped to realign.
    fn steer(&mut self, dt: f64, at: u64) -> Result<Option<Range<u64>>> {
        if self.draining || dt <= 0.0 {
            return Ok(None);
        }
        let level = self.fifo.len() as f64;
        self.filtered = if self.age == 0.0 { level } else { self.filtered + (dt / LEVEL_TAU).min(1.0) * (level - self.filtered) };
        self.age += dt;
        if self.age < SETTLE {
            return Ok(None);
        }
        let target = *self.target.get_or_insert(self.filtered);
        if level > target + RESYNC {
            let excess = (level - target) as usize;
            self.fifo.drain(..excess);
            self.filtered = target;
            self.integral = 0.0;
            return Ok(Some(at..at + excess as u64));
        }
        if std::mem::take(&mut self.resumed) && level < target - RESYNC {
            // Back after running dry with nothing behind it: silence in front puts it where its capture time belongs.
            let short = (target - level) as usize;
            for _ in 0..short {
                self.fifo.push_front(0.0);
            }
            self.filtered = target;
            self.integral = 0.0;
            return Ok(Some(at..at + short as u64));
        }
        let e = self.filtered - target;
        self.integral = (self.integral + e * dt).clamp(-MAX_CORRECTION / KI, MAX_CORRECTION / KI);
        self.correction = (-(KP * e + KI * self.integral)).clamp(-MAX_CORRECTION, MAX_CORRECTION);
        self.drift.set_resample_ratio_relative(1.0 + self.correction, true)?;
        Ok(None)
    }
}

pub struct Mixer {
    lanes: Vec<Option<Lane>>,
    emitted: u64,
    /// Off only in the test that shows the steering is what holds the sources together.
    steering: bool,
}

impl Mixer {
    pub fn new(lanes: usize) -> Self {
        Self { lanes: (0..lanes).map(|_| None).collect(), emitted: 0, steering: true }
    }

    /// A source joins. `age` is how long ago its oldest waiting sample was captured (its device latency, the
    /// time since its last callback and the audio waiting in its ring): that sample lands where a sample
    /// captured then belongs, DELAY behind the host clock. Returns that position.
    pub fn join(&mut self, i: usize, cfg: LaneConfig, age: f64) -> Result<u64> {
        let prefill = (DELAY as f64 - age * RATE).round().max(0.0) as u64;
        // Audio older than DELAY (a ring that filled while the recording waited to begin) cannot be placed in time:
        // it is dropped before the recording, not played late for good.
        let skip = ((age - DELAY as f64 / RATE).max(0.0) * cfg.rate as f64).round() as u64;
        let origin = self.emitted + prefill;
        self.lanes[i] = Some(Lane::new(cfg, origin, prefill as usize, skip)?);
        Ok(origin)
    }

    pub fn push(&mut self, i: usize, interleaved: &[f32]) -> Result<()> {
        match self.lanes[i].as_mut().filter(|l| !l.draining) {
            Some(l) => l.push(interleaved),
            None => Ok(()),
        }
    }

    /// Input frames lost to overflow: silence stands in for them, so later audio keeps its place. Returns where
    /// they land.
    pub fn push_silence(&mut self, i: usize, frames: u64) -> Result<Range<u64>> {
        let Some(l) = self.lanes[i].as_mut().filter(|l| !l.draining) else { return Ok(0..0) };
        let lost = l.position(l.in_frames)..l.position(l.in_frames + frames);
        let mut left = frames;
        while left > 0 {
            let n = left.min(SILENCE_BLOCK);
            l.push(&vec![0.0; n as usize * l.channels])?;
            left -= n;
        }
        Ok(lost)
    }

    /// The source has gone: what it delivered still plays out. Returns the output position where its audio ends.
    pub fn leave(&mut self, i: usize) -> Result<u64> {
        let Some(l) = self.lanes[i].as_mut() else { return Ok(self.emitted) };
        l.finish()?;
        Ok(self.emitted + l.fifo.len() as u64)
    }

    pub fn is_joined(&self, i: usize) -> bool {
        self.lanes[i].as_ref().is_some_and(|l| !l.draining)
    }

    /// Whether any source is in the mix, joined or still playing out.
    pub fn any(&self) -> bool {
        self.lanes.iter().any(Option::is_some)
    }

    /// Takes the samples due up to `due` (output samples since the mix began) from every source, sums them with
    /// their gains and limits only the sum; then steers every source.
    pub fn pull(&mut self, due: u64) -> Pulled {
        let n = due.saturating_sub(self.emitted) as usize;
        let mut samples = vec![0.0f32; n];
        let mut gaps = Vec::new();
        let at = self.emitted;
        for (i, l) in self.lanes.iter_mut().enumerate() {
            if let Some(g) = l.as_mut().and_then(|l| l.take(&mut samples, at)) {
                gaps.push((i, g));
            }
        }
        for s in &mut samples {
            *s = s.clamp(-1.0, 1.0);
        }
        self.emitted += n as u64;
        let dt = n as f64 / RATE;
        for (i, slot) in self.lanes.iter_mut().enumerate() {
            let Some(l) = slot.as_mut() else { continue };
            if self.steering {
                if let Ok(Some(g)) = l.steer(dt, self.emitted) {
                    gaps.push((i, g));
                }
            }
            if l.draining && l.fifo.is_empty() {
                *slot = None;
            }
        }
        Pulled { samples, gaps }
    }

    /// Everything still waiting, for the end of a recording.
    pub fn flush(&mut self) -> Vec<f32> {
        let longest = self.lanes.iter().flatten().map(|l| l.fifo.len()).max().unwrap_or(0) as u64;
        let steering = std::mem::replace(&mut self.steering, false);
        for l in self.lanes.iter_mut().flatten() {
            l.draining = true; // nothing more comes: running out is not a gap
        }
        let out = self.pull(self.emitted + longest).samples;
        self.steering = steering;
        out
    }

    pub fn emitted(&self) -> u64 {
        self.emitted
    }

    pub fn stats(&self, i: usize) -> Option<LaneStats> {
        self.lanes[i].as_ref().map(|l| LaneStats { level: l.fifo.len(), filtered: l.filtered, target: l.target, correction_ppm: l.correction * 1e6, underrun: l.underrun })
    }

    pub fn level(&mut self, i: usize) -> Option<f32> {
        let l = self.lanes[i].as_mut().filter(|l| l.n > 0)?;
        let rms = (l.sq / l.n as f64).sqrt() as f32;
        (l.sq, l.n) = (0.0, 0);
        Some(rms)
    }
}
```

- [ ] **Step 4: Run the mixer's tests and read the numbers**

Run: `cargo test -p lecturelive-core audio::mix -- --nocapture 2>&1 | command grep -E "kHz|ppm|unsteered|test result|panicked|FAILED"`
Expected: 8 passed, and the printed numbers are recorded in the ledger (first-click error, worst error after 60 s per source, the negative control). Tune only `KP`, `KI`, `LEVEL_TAU` and `SETTLE` if a bound fails, recording each change as a Ruling with its numbers. **Runtime ruling, decided now:** if the two-hour test takes over 90 s, it runs with 16 kHz devices (the drift stage alone, the same ±100 ppm and latencies), and the ±500 ppm test keeps 48 and 44.1 kHz. The Findings record which.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/core/src/audio/mix.rs crates/core/src/audio/mod.rs
git commit -m "Mix two sources on independent clocks by steering a drift stage per source against the host clock, with a two-hour test over simulated clocks"
```

---

### Task 3: `MixedSource`, the coordinator's `GapEnd`, the CLI's `--mixed`, and the live two-device run (milestone task 1, devices)

**Files:**
- Create: `crates/core/src/audio/mixed.rs`
- Modify: `crates/core/src/audio/{mod.rs,capture.rs,source.rs}`, `crates/core/src/session/coordinator.rs`, `crates/cli/src/main.rs`

**Interfaces:**
- Consumes: `mix::{Mixer, LaneConfig, LaneStats, DELAY, RATE}` (Task 2); `CaptureConsumer::available` (Task 1).
- Produces:
  - `audio::capture`: `StreamFlags::note_callback(&self, latency: Option<Duration>)` (called from each callback), and `StreamFlags::age_of_oldest(&self, waiting: u64, rate: u32) -> f64`, which is the seconds since now minus the last callback, plus the latency, plus waiting / rate.
  - `audio::source`:
    - `pub(crate) fn open_stream(device: &cpal::Device) -> Result<(cpal::Stream, CaptureConsumer, Arc<StreamFlags>, u32, u16)>` (was private);
    - `SourceEvent::GapEnd { recording_id: Uuid, start_sample: u64, end_sample: u64 }`.
  - `audio::mixed`:
    - `pub const MIXED_MODE: bool`, `pub const LOOPBACK: usize = 0`, `pub const INPUT: usize = 1`;
    - `pub struct MixedSource { pub loopback: String, pub input: String, pub gains: [f32; 2] }`, `MixedSource::new(loopback: &str, input: &str)`, `MixedSource::uid(&self) -> String` (`"mixed:<loopback>+<input>"`);
    - `impl Source for MixedSource`.
  - Coordinator: a `GapEnd` sets the end of the open gap with that recording and start, and saves.
  - CLI: `record --mixed <input>` and `lecture --mixed <input>` (each conflicts with `--loopback` and `--device`; the input is a UID or part of a name, as `--device`).

- [ ] **Step 1: Write the failing tests.** In `coordinator.rs` `mod tests`:

```rust
    #[tokio::test]
    async fn a_gap_left_open_is_closed_by_its_end() {
        let dir = tempfile::tempdir().unwrap();
        let a = Uuid::new_v4();
        let script = Script(vec![
            begin(a, 0),
            frame(a, 0),
            SourceEvent::Gap(Gap::new(a, 1600, None, GapKind::DeviceGone)),
            frame(a, 1600),
            SourceEvent::GapEnd { recording_id: a, start_sample: 1600, end_sample: 4800 },
            frame(a, 3200),
            SourceEvent::End { recording_id: a, samples: 4800, stream_errors: 0 },
        ]);
        run(dir.path(), script).await.0.unwrap();
        let sc = Sidecar::load(&sidecar_path(dir.path(), STEM)).unwrap().unwrap();
        assert_eq!(sc.gaps, vec![Gap::new(a, 1600, Some(4800), GapKind::DeviceGone)]);
    }
```

In the new `mixed.rs`, `#[cfg(test)] mod tests` with scripted devices:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::capture::ring;
    use std::sync::atomic::AtomicBool;
    use std::thread::JoinHandle;

    #[derive(Default)]
    struct Ctl {
        listed: AtomicBool,
        unplug: AtomicBool,
    }

    /// A device on its own thread, producing `value` at `rate` in 10 ms blocks until it is unplugged.
    struct Fake {
        uid: String,
        rate: u32,
        value: f32,
        ctl: Arc<Ctl>,
    }

    struct FakeStream {
        stop: Arc<AtomicBool>,
        thread: Option<JoinHandle<()>>,
    }

    impl Drop for FakeStream {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
        }
    }

    impl LaneDevice for Fake {
        fn uid(&self) -> &str {
            &self.uid
        }
        fn open(&mut self) -> Result<Option<OpenLane>> {
            if !self.ctl.listed.load(Ordering::Relaxed) {
                return Ok(None);
            }
            self.ctl.unplug.store(false, Ordering::Relaxed);
            let (mut p, consumer, flags) = ring(1, self.rate, 4);
            let stop = Arc::new(AtomicBool::new(false));
            let (s, f, ctl, rate, value) = (stop.clone(), flags.clone(), self.ctl.clone(), self.rate, self.value);
            let thread = std::thread::spawn(move || {
                let (block, start, mut sent) = ((rate / 100) as u64, Instant::now(), 0u64);
                while !s.load(Ordering::Relaxed) {
                    if ctl.unplug.load(Ordering::Relaxed) {
                        f.on_error(cpal::ErrorKind::DeviceNotAvailable);
                        return;
                    }
                    while sent + block <= (start.elapsed().as_secs_f64() * rate as f64) as u64 {
                        f.note_callback(Some(Duration::from_millis(3)));
                        p.push(&vec![value; block as usize]);
                        sent += block;
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
            });
            Ok(Some(OpenLane { consumer, flags, rate: self.rate, channels: 1, stream: Box::new(FakeStream { stop, thread: Some(thread) }) }))
        }
    }

    fn fake(uid: &str, rate: u32, value: f32) -> (Fake, Arc<Ctl>) {
        let ctl = Arc::new(Ctl::default());
        ctl.listed.store(true, Ordering::Relaxed);
        (Fake { uid: uid.into(), rate, value, ctl: ctl.clone() }, ctl)
    }

    /// Runs the loop on its own thread while `script` plays out on this one; returns every event.
    fn run(a: Fake, b: Fake, script: impl FnOnce(&AtomicBool)) -> (Result<()>, Vec<SourceEvent>) {
        let stop = Arc::new(AtomicBool::new(false));
        let s = stop.clone();
        let t = std::thread::spawn(move || {
            let mut events = Vec::new();
            let r = run_mixed([Box::new(a), Box::new(b)], [1.0, 1.0], "mixed:a+b", &mut |e| {
                events.push(e);
                Ok(())
            }, &s, None);
            (r, events)
        });
        script(&stop);
        stop.store(true, Ordering::Relaxed);
        t.join().unwrap()
    }

    fn sleep(ms: u64) {
        std::thread::sleep(Duration::from_millis(ms));
    }

    /// The mean of the frames' samples between two output positions.
    fn mean(events: &[SourceEvent], from: u64, to: u64) -> f64 {
        let v: Vec<f64> = events
            .iter()
            .filter_map(|e| if let SourceEvent::Frame(f) = e { Some(f) } else { None })
            .flat_map(|f| f.pcm().iter().enumerate().map(move |(k, &s)| (f.sample_offset + k as u64, s)))
            .filter(|(p, _)| (from..to).contains(p))
            .map(|(_, s)| s as f64 / i16::MAX as f64)
            .collect();
        v.iter().sum::<f64>() / v.len().max(1) as f64
    }

    #[test]
    fn a_mixed_recording_carries_both_and_goes_on_when_one_leaves() {
        let (a, _) = fake("BlackHole", 48_000, 0.25);
        let (b, ctl) = fake("Receiver", 44_100, 0.5);
        let (r, events) = run(a, b, |_| {
            sleep(1_500);
            ctl.listed.store(false, Ordering::Relaxed);
            ctl.unplug.store(true, Ordering::Relaxed);
            sleep(1_500);
            ctl.listed.store(true, Ordering::Relaxed);
            sleep(1_500);
        });
        r.unwrap();
        let begins = events.iter().filter(|e| matches!(e, SourceEvent::Begin { .. })).count();
        assert_eq!(begins, 1, "one recording throughout");
        let mut next = 0;
        for e in &events {
            if let SourceEvent::Frame(f) = e {
                assert_eq!(f.sample_offset, next, "frames are contiguous");
                next += 1600;
            }
        }
        let gone = events.iter().find_map(|e| if let SourceEvent::Gap(g) = e { (g.kind == GapKind::DeviceGone).then_some(g.start_sample) } else { None }).expect("a gap where the receiver went");
        let back = events.iter().find_map(|e| if let SourceEvent::GapEnd { start_sample, end_sample, .. } = e { (*start_sample == gone).then_some(*end_sample) } else { None }).expect("its end where it joined again");
        assert!(events.iter().any(|e| matches!(e, SourceEvent::DeviceGone { uid } if uid == "Receiver")));
        assert!(events.iter().any(|e| matches!(e, SourceEvent::DeviceBack { uid } if uid == "Receiver")));
        assert!((mean(&events, 8_000, 12_000) - 0.75).abs() < 0.03, "both: {}", mean(&events, 8_000, 12_000));
        assert!((mean(&events, gone + 4_000, back - 1_000) - 0.25).abs() < 0.03, "the loopback alone while the receiver was away");
        assert!((mean(&events, back + 8_000, back + 12_000) - 0.75).abs() < 0.03, "both again");
        let Some(SourceEvent::End { samples, .. }) = events.iter().rev().find(|e| matches!(e, SourceEvent::End { .. })) else { panic!("no End") };
        assert!((4.3..=5.2).contains(&(*samples as f64 / 16_000.0)), "{samples}");
    }

    #[test]
    fn when_both_leave_the_recording_ends_and_the_next_begins_when_one_returns() {
        let (a, ca) = fake("BlackHole", 48_000, 0.25);
        let (b, cb) = fake("Receiver", 48_000, 0.5);
        let (r, events) = run(a, b, |_| {
            sleep(1_000);
            for c in [&ca, &cb] {
                c.listed.store(false, Ordering::Relaxed);
                c.unplug.store(true, Ordering::Relaxed);
            }
            sleep(1_200);
            ca.listed.store(true, Ordering::Relaxed);
            sleep(1_500);
        });
        r.unwrap();
        let kinds: Vec<&str> = events.iter().filter_map(|e| match e { SourceEvent::Begin { .. } => Some("begin"), SourceEvent::End { .. } => Some("end"), _ => None }).collect();
        assert_eq!(kinds, ["begin", "end", "begin", "end"]);
    }

    #[test]
    fn a_source_missing_at_the_start_fails_the_session() {
        let (a, _) = fake("BlackHole", 48_000, 0.25);
        let (b, cb) = fake("Receiver", 48_000, 0.5);
        cb.listed.store(false, Ordering::Relaxed);
        let (r, _) = run(a, b, |_| sleep(100));
        assert!(format!("{:#}", r.unwrap_err()).contains("Receiver not found"));
    }

    /// Review Focus 2: in mixed mode the meter and the silence warning follow Zoom's sound (spec §4.3 step 4).
    #[test]
    fn the_level_follows_the_loopback_even_when_the_input_is_loud() {
        let (a, _) = fake("BlackHole", 48_000, 0.0);
        let (b, _) = fake("Receiver", 48_000, 0.5);
        let (r, events) = run(a, b, |_| sleep(3_500));
        r.unwrap();
        let levels: Vec<f32> = events.iter().filter_map(|e| if let SourceEvent::Level(l) = e { Some(*l) } else { None }).collect();
        assert!(levels.len() >= 2, "{levels:?}");
        assert!(levels.iter().all(|&l| l < 0.001), "the loopback is silent whatever the input hears: {levels:?}");
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p lecturelive-core mixed:: gap_left_open 2>&1 | tail -15`
Expected: FAIL to compile (`mixed` module, `SourceEvent::GapEnd`, `note_callback` do not exist).

- [ ] **Step 3: Implement.**

`capture.rs`: add `latency_us: AtomicU64` and `last_callback_ns: AtomicU64` to `StreamFlags`, and a process-wide clock:

```rust
/// Nanoseconds on the host's monotonic clock since this process first asked: what callback times are compared in.
pub fn mono_ns() -> u64 {
    static EPOCH: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    EPOCH.get_or_init(std::time::Instant::now).elapsed().as_nanos() as u64
}

impl StreamFlags {
    /// Called from each audio callback: when it came, and (once) how long after capture.
    pub fn note_callback(&self, latency: Option<std::time::Duration>) {
        self.last_callback_ns.store(mono_ns(), Ordering::Relaxed);
        if let Some(l) = latency {
            let _ = self.latency_us.compare_exchange(0, (l.as_micros() as u64).max(1), Ordering::Relaxed, Ordering::Relaxed);
        }
    }

    /// Seconds since the oldest of `waiting` frames at `rate` was captured.
    pub fn age_of_oldest(&self, waiting: u64, rate: u32) -> f64 {
        let since = mono_ns().saturating_sub(self.last_callback_ns.load(Ordering::Relaxed)) as f64 / 1e9;
        since + self.latency_us.load(Ordering::Relaxed) as f64 / 1e6 + waiting as f64 / rate as f64
    }
}
```

`source.rs`: `open_stream` becomes `pub(crate)`. Each callback closure calls `flags.note_callback(info.timestamp().callback.duration_since(&info.timestamp().capture))` before `producer.push(…)`: clone the `Arc` into the closures as `cb_flags`, and name the `InputCallbackInfo` argument `info`. Add `GapEnd { recording_id: Uuid, start_sample: u64, end_sample: u64 }` to `SourceEvent`.

`coordinator.rs` `on_source`:

```rust
            SourceEvent::GapEnd { recording_id, start_sample, end_sample } => {
                let open = self.sidecar.gaps.iter_mut().find(|g| g.recording_id == recording_id && g.start_sample == start_sample && g.end_sample.is_none());
                if let Some(g) = open {
                    g.end_sample = Some(end_sample);
                    self.save().await?;
                }
            }
```

`crates/core/src/audio/mixed.rs` (above the tests):

```rust
//! Mixed mode's source (spec §4.1, §4.2): the loopback and an input, each on its own stream and clock, mixed
//! into one recording that lasts while either is there. A source that goes leaves a gap in the recording, from
//! where its audio ends to where it joins again, and the other carries on; when both have gone, the recording
//! ends, and the next begins when one returns.
use std::any::Any;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use chrono::{Local, TimeZone};
use tokio::sync::mpsc::Sender;
use uuid::Uuid;

use super::capture::{CaptureConsumer, Chunk, StreamFlags};
use super::frame::FrameBuilder;
use super::input::find_input;
use super::mix::{LaneConfig, LaneStats, Mixer, DELAY, RATE};
use super::source::{open_stream, Source, SourceEvent};
use crate::session::sidecar::{Gap, GapKind};

/// Mixed mode ships once it has passed the two-hour drift test (spec §4.2; M6 Findings).
pub const MIXED_MODE: bool = true;
pub const LOOPBACK: usize = 0;
pub const INPUT: usize = 1;
const POLL: Duration = Duration::from_millis(20);
const REAPPEAR_POLL: Duration = Duration::from_millis(500);
/// When a recording begins, how long it waits for both sources' first audio.
const START_WAIT: Duration = Duration::from_secs(2);

pub struct MixedSource {
    pub loopback: String,
    pub input: String,
    pub gains: [f32; 2],
}

impl MixedSource {
    pub fn new(loopback: &str, input: &str) -> Self {
        Self { loopback: loopback.into(), input: input.into(), gains: [1.0, 1.0] }
    }

    /// How the sidecar names the source of its recordings.
    pub fn uid(&self) -> String {
        format!("mixed:{}+{}", self.loopback, self.input)
    }
}

impl Source for MixedSource {
    fn run(self: Box<Self>, out: Sender<SourceEvent>, stop: Arc<AtomicBool>) {
        let uid = self.uid();
        let devices: [Box<dyn LaneDevice>; 2] = [Box::new(CpalLane(self.loopback.clone())), Box::new(CpalLane(self.input.clone()))];
        let mut emit = |e: SourceEvent| out.blocking_send(e).map_err(|_| anyhow!("session closed"));
        if let Err(e) = run_mixed(devices, self.gains, &uid, &mut emit, &stop, None) {
            let _ = out.blocking_send(SourceEvent::Failed(format!("{e:#}")));
        }
    }
}

pub(crate) struct OpenLane {
    pub consumer: CaptureConsumer,
    pub flags: Arc<StreamFlags>,
    pub rate: u32,
    pub channels: u16,
    /// Keeps the stream running; dropping it stops the stream.
    pub stream: Box<dyn Any>,
}

pub(crate) trait LaneDevice {
    fn uid(&self) -> &str;
    /// Opens and starts the device's stream; None while it is not listed.
    fn open(&mut self) -> Result<Option<OpenLane>>;
}

struct CpalLane(String);

impl LaneDevice for CpalLane {
    fn uid(&self) -> &str {
        &self.0
    }
    fn open(&mut self) -> Result<Option<OpenLane>> {
        let Some(d) = find_input(&self.0)? else { return Ok(None) };
        let (stream, consumer, flags, rate, channels) = open_stream(&d)?;
        Ok(Some(OpenLane { consumer, flags, rate, channels, stream: Box::new(stream) }))
    }
}

enum State {
    /// Gone: looked for again at `next`.
    Waiting { next: Instant },
    /// Streaming, not yet in the mix.
    Opened(OpenLane),
    Live(OpenLane),
}

struct Rec {
    id: Uuid,
    t0: Instant,
    frames: FrameBuilder,
    level_at: u64,
}

/// The lanes' state every 10 s, for the live drift measurement: seconds since the start, and each lane's stats.
pub(crate) type StatsLog = Arc<Mutex<Vec<(f64, [Option<LaneStats>; 2])>>>;

fn drain(l: &mut OpenLane, i: usize, mixer: &mut Mixer, rec: Uuid, emit: &mut dyn FnMut(SourceEvent) -> Result<()>) -> Result<()> {
    let mut lost = Vec::new();
    l.consumer.drain(|c| match c {
        Chunk::Audio(a) => mixer.push(i, a),
        Chunk::Silence(n) => {
            lost.push(mixer.push_silence(i, n)?);
            Ok(())
        }
    })?;
    for r in lost {
        emit(SourceEvent::Gap(Gap::new(rec, r.start, Some(r.end), GapKind::CaptureOverflow)))?;
    }
    Ok(())
}

fn end_recording(r: Rec, mixer: &mut Mixer, emit: &mut dyn FnMut(SourceEvent) -> Result<()>) -> Result<()> {
    let Rec { id, mut frames, .. } = r;
    for f in frames.push(&mixer.flush()) {
        emit(SourceEvent::Frame(f))?;
    }
    if let Some(f) = frames.finish() {
        emit(SourceEvent::Frame(f))?;
    }
    emit(SourceEvent::End { recording_id: id, samples: mixer.emitted(), stream_errors: 0 })
}

/// The loop, on the source's thread: open both, mix on the host clock, follow each source away and back.
pub(crate) fn run_mixed(mut devices: [Box<dyn LaneDevice>; 2], gains: [f32; 2], uid: &str, emit: &mut dyn FnMut(SourceEvent) -> Result<()>, stop: &AtomicBool, stats: Option<StatsLog>) -> Result<()> {
    let mut state = Vec::new();
    for d in devices.iter_mut() {
        let l = d.open()?.with_context(|| format!("input {} not found; `lecturelive inputs` lists them", d.uid()))?;
        state.push(State::Opened(l));
    }
    let (started, mut first) = (Instant::now(), true);
    let mut mixer = Mixer::new(2);
    let mut open_gap: [Option<u64>; 2] = [None, None];
    let mut was_gone = [false, false];
    let mut rec: Option<Rec> = None;
    let mut stats_at = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        for (i, s) in state.iter_mut().enumerate() {
            if let State::Waiting { next } = s {
                if Instant::now() >= *next {
                    match devices[i].open() {
                        Ok(Some(l)) => *s = State::Opened(l),
                        _ => *next = Instant::now() + REAPPEAR_POLL, // "not yet", as a single source waits
                    }
                }
            }
        }
        if rec.is_none() {
            let heard: Vec<bool> = state.iter().map(|s| matches!(s, State::Opened(l) if l.flags.anchor_ms().is_some())).collect();
            let unheard = state.iter().zip(&heard).any(|(s, h)| matches!(s, State::Opened(_)) && !h);
            if heard.iter().any(|&h| h) && !(first && unheard && started.elapsed() < START_WAIT) {
                first = false;
                let id = Uuid::new_v4();
                let ms = Local::now().timestamp_millis() - (DELAY as f64 / RATE * 1000.0) as i64;
                let anchor = Local.timestamp_millis_opt(ms).single().context("anchor time")?;
                emit(SourceEvent::Begin { recording_id: id, anchor, source_uid: uid.into(), input_rate: RATE as u32, channels: 1 })?;
                rec = Some(Rec { id, t0: Instant::now(), frames: FrameBuilder::new(id), level_at: 0 });
                mixer = Mixer::new(2);
                for i in 0..2 {
                    if !heard[i] {
                        open_gap[i] = Some(0); // not there yet: missing from the recording's first sample
                        emit(SourceEvent::Gap(Gap::new(id, 0, None, GapKind::DeviceGone)))?;
                    }
                }
            }
        }
        if let Some(r) = &rec {
            for i in 0..2 {
                let heard = matches!(&state[i], State::Opened(l) if l.flags.anchor_ms().is_some());
                if !heard {
                    continue;
                }
                let State::Opened(mut l) = std::mem::replace(&mut state[i], State::Waiting { next: Instant::now() }) else { unreachable!() };
                let age = l.flags.age_of_oldest(l.consumer.available(), l.rate);
                let at = mixer.join(i, LaneConfig { rate: l.rate, channels: l.channels, gain: gains[i] }, age)?;
                if let Some(start) = open_gap[i].take() {
                    emit(SourceEvent::GapEnd { recording_id: r.id, start_sample: start, end_sample: at })?;
                }
                if std::mem::take(&mut was_gone[i]) {
                    emit(SourceEvent::DeviceBack { uid: devices[i].uid().into() })?;
                }
                drain(&mut l, i, &mut mixer, r.id, emit)?;
                state[i] = State::Live(l);
            }
            for i in 0..2 {
                let State::Live(l) = &mut state[i] else { continue };
                drain(l, i, &mut mixer, r.id, emit)?;
                let (gone, invalid) = (l.flags.gone.load(Ordering::Acquire), l.flags.invalidated.load(Ordering::Acquire));
                if !gone && !invalid {
                    continue;
                }
                let next = Instant::now() + if invalid { Duration::ZERO } else { REAPPEAR_POLL };
                let State::Live(mut l) = std::mem::replace(&mut state[i], State::Waiting { next }) else { unreachable!() };
                drop(std::mem::replace(&mut l.stream, Box::new(())));
                drain(&mut l, i, &mut mixer, r.id, emit)?;
                let at = mixer.leave(i)?;
                open_gap[i] = Some(at);
                emit(SourceEvent::Gap(Gap::new(r.id, at, None, if invalid { GapKind::RateChange } else { GapKind::DeviceGone })))?;
                if gone {
                    was_gone[i] = true;
                    emit(SourceEvent::DeviceGone { uid: devices[i].uid().into() })?;
                }
            }
        }
        if let Some(r) = rec.as_mut() {
            let pulled = mixer.pull((r.t0.elapsed().as_secs_f64() * RATE) as u64);
            for (_, g) in pulled.gaps {
                emit(SourceEvent::Gap(Gap::new(r.id, g.start, Some(g.end), GapKind::CaptureOverflow)))?;
            }
            for f in r.frames.push(&pulled.samples) {
                emit(SourceEvent::Frame(f))?;
            }
            if mixer.emitted() >= r.level_at + RATE as u64 {
                r.level_at = mixer.emitted();
                // The meter and the silence warning follow the loopback: in mixed mode it is Zoom's sound that
                // must not go silent (spec §4.3 step 4); a room microphone's noise would hide it.
                if let Some(l) = mixer.level(LOOPBACK).or_else(|| mixer.level(INPUT)) {
                    emit(SourceEvent::Level(l))?;
                }
            }
            if !mixer.any() {
                // Every source has gone: the recording ends here; each one's open gap runs to its end.
                end_recording(rec.take().expect("a recording"), &mut mixer, emit)?;
                open_gap = [None, None];
            }
        }
        if let Some(log) = &stats {
            if stats_at.elapsed() >= Duration::from_secs(10) {
                stats_at = Instant::now();
                log.lock().expect("the stats lock").push((started.elapsed().as_secs_f64(), [mixer.stats(0), mixer.stats(1)]));
            }
        }
        std::thread::sleep(POLL);
    }
    // Stop: the streams end, what they captured is drained, and the mix flushed.
    for s in state.iter_mut() {
        if let State::Live(l) = s {
            drop(std::mem::replace(&mut l.stream, Box::new(())));
        }
    }
    if let Some(r) = rec.take() {
        for (i, s) in state.iter_mut().enumerate() {
            if let State::Live(l) = s {
                drain(l, i, &mut mixer, r.id, emit)?;
            }
        }
        end_recording(r, &mut mixer, emit)?;
    }
    Ok(())
}
```

`audio/mod.rs`: `pub mod mixed;`. CLI: add `mixed: Option<String>` to `Record` and `LectureArgs` (`#[arg(long, conflicts_with_all = ["loopback", "device"])]`, doc "Record Zoom (through LectureLive Loopback) and this input together: its UID or part of its name"). When it is given: `anyhow::ensure!(MIXED_MODE, "mixed mode is disabled: it did not pass its drift test (docs/milestones.md, M6)")`, resolve the input as `--device` does, and pass `Box::new(MixedSource::new(loopback::BLACKHOLE_UID, &uid))` as the source. The loopback silence warning is on whenever BlackHole is one of the sources.

- [ ] **Step 4: Run the tests and the whole core suite**

Run: `cargo test -p lecturelive-core --no-fail-fast 2>&1 | command grep -E "^test result|FAILED|panicked" | head -30`
Expected: every `test result: ok`.

- [ ] **Step 5: Add the live run as an ignored test** in `mixed.rs` `mod tests`:

```rust
    /// The live drift measurement (M6 plan, Task 11), on this Mac's two real inputs. The microphone's audio is
    /// counted in memory only: nothing is written or sent. LECTURELIVE_MIX_SECS sets the length (default 60):
    /// LECTURELIVE_MIX_SECS=7200 cargo test -p lecturelive-core blackhole_and_the_built_in -- --ignored --nocapture
    #[test]
    #[ignore]
    fn blackhole_and_the_built_in_microphone_stay_aligned() {
        let secs: u64 = std::env::var("LECTURELIVE_MIX_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(60);
        let stop = Arc::new(AtomicBool::new(false));
        let log: StatsLog = Default::default();
        let s = stop.clone();
        let timer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(secs));
            s.store(true, Ordering::Relaxed);
        });
        let (mut next, mut begins, mut gaps) = (0u64, 0, Vec::new());
        let devices: [Box<dyn LaneDevice>; 2] = [Box::new(CpalLane(crate::audio::routing::BLACKHOLE_UID.into())), Box::new(CpalLane("BuiltInMicrophoneDevice".into()))];
        run_mixed(devices, [1.0, 0.0], "mixed:live", &mut |e| {
            match e {
                SourceEvent::Begin { .. } => begins += 1,
                SourceEvent::Frame(f) => {
                    assert_eq!(f.sample_offset, next, "contiguous");
                    next += 1600;
                }
                SourceEvent::Gap(g) => gaps.push(g),
                _ => {}
            }
            Ok(())
        }, &stop, Some(log.clone()))
        .unwrap();
        timer.join().unwrap();
        let log = log.lock().unwrap();
        let off = |s: &LaneStats| s.filtered - s.target.unwrap_or(s.filtered);
        let mut worst: f64 = 0.0;
        for (t, s) in log.iter() {
            let [Some(a), Some(b)] = s else { continue };
            println!("{t:>6.0} s  loopback level {:>5} ({:+6.1}, {:+7.1} ppm)  microphone level {:>5} ({:+6.1}, {:+7.1} ppm)", a.level, off(a), a.correction_ppm, b.level, off(b), b.correction_ppm);
            if *t > 30.0 {
                worst = worst.max((off(a) - off(b)).abs());
            }
        }
        let last = log.last().expect("stats");
        println!("{} s recorded; worst relative offset after 30 s {worst:.1} samples ({:.2} ms); underruns {:?}", next / 16_000, worst / 16.0, last.1.map(|s| s.map(|s| s.underrun)));
        assert_eq!(begins, 1);
        assert!(gaps.is_empty(), "{gaps:?}");
        assert!(last.1.iter().all(|s| s.is_some_and(|s| s.underrun == 0)));
        assert!(worst <= 160.0, "{worst}");
    }
```

Run once for 60 s: `LECTURELIVE_MIX_SECS=60 cargo test -p lecturelive-core blackhole_and_the_built_in -- --ignored --nocapture 2>&1 | tail -12`
Expected: PASS, with six stats lines. (The two-hour run is Task 11.)

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/audio/mixed.rs crates/core/src/audio/mod.rs crates/core/src/audio/capture.rs crates/core/src/audio/source.rs crates/core/src/session/coordinator.rs crates/cli/src/main.rs
git commit -m "Record Zoom and an input together as one mixed source that lasts while either is there, with a gap where one was missing"
```

---

### Task 4: The fallback a person may choose while a single input is gone (spec §4.1, §10 "Device disappears: Single")

**Files:**
- Modify: `crates/core/src/audio/source.rs`, `crates/cli/src/main.rs`

**Interfaces:**
- Produces (`audio::source`):
  - `#[derive(Clone, Default)] pub struct Fallback(Arc<Mutex<Option<String>>>)` with `pub fn offer(&self, uid: &str)` and `pub fn take(&self) -> Option<String>`;
  - `pub struct DeviceSource { pub uid: String, pub fallback: Fallback }`;
  - `DeviceBack { uid }` carries the UID now recording.
- Consumed by: Task 8 (`use_input`).

- [ ] **Step 1: Write the failing tests** in `source.rs` `mod tests`. Rewrite `drive` to look up by UID: `lookups` become `Vec<(&'static str, Result<Option<u32>>)>`, popped only when the UID matches. A new `fallback` script sets the offer after the n-th wait. Then:

```rust
    #[test]
    fn while_a_gone_device_is_waited_for_the_person_may_choose_another_input() {
        let fallback = Fallback::default();
        let f = fallback.clone();
        let mut polls = 0;
        let (r, events, opened) = drive_with(
            &fallback,
            |uid| match uid {
                "Receiver_UID" => {
                    polls += 1;
                    if polls == 3 {
                        f.offer("BuiltInMicrophoneDevice"); // chosen while waiting
                    }
                    Ok(if polls == 1 { Some(7) } else { None })
                }
                "BuiltInMicrophoneDevice" => Ok(Some(9)),
                other => panic!("looked for {other}"),
            },
            vec![Ok(Outcome::Ended(SegmentEnd::Gone)), Ok(Outcome::Ended(SegmentEnd::Stopped))],
        );
        r.unwrap();
        assert_eq!(events, ["gone", "back BuiltInMicrophoneDevice"]);
        assert_eq!(opened, [(7, "Receiver_UID".to_string()), (9, "BuiltInMicrophoneDevice".to_string())], "the chosen input records under its own UID");
    }

    /// Review Focus 3.
    #[test]
    fn a_fallback_that_cannot_be_found_keeps_the_wait_for_the_original() {
        let fallback = Fallback::default();
        let f = fallback.clone();
        let mut polls = 0;
        let (r, events, opened) = drive_with(
            &fallback,
            |uid| match uid {
                "Receiver_UID" => {
                    polls += 1;
                    if polls == 2 {
                        f.offer("Unplugged_UID");
                    }
                    Ok(if polls == 1 || polls == 4 { Some(7) } else { None })
                }
                "Unplugged_UID" => Ok(None),
                other => panic!("looked for {other}"),
            },
            vec![Ok(Outcome::Ended(SegmentEnd::Gone)), Ok(Outcome::Ended(SegmentEnd::Stopped))],
        );
        r.unwrap();
        assert_eq!(events, ["gone", "back Receiver_UID"]);
        assert_eq!(opened.iter().map(|o| o.0).collect::<Vec<_>>(), [7, 7], "the original, when it returned");
    }
```

`drive_with(fallback, find, outcomes) -> (Result<()>, Vec<String>, Vec<(u32, String)>)` records `DeviceBack` as `"back {uid}"`, and each opened device with the UID it was opened as. The existing four `drive` tests keep their assertions: `drive` wraps `drive_with` with a lookup that ignores the UID.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p lecturelive-core audio::source 2>&1 | tail -15`
Expected: FAIL to compile (`Fallback`, `drive_with`).

- [ ] **Step 3: Implement.** `supervise` takes `fallback: &Fallback`, `find: impl FnMut(&str) -> Result<Option<D>>` and `segment: impl FnMut(&D, &str) -> Result<Outcome>`, and keeps `let mut current = uid.to_string();`. The waiting loop becomes:

```rust
        device = loop {
            if stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            std::thread::sleep(poll);
            // Another input only when the person chose it (spec §4.1: never a switch by itself). One that cannot
            // be found is dropped, and the wait for the original goes on.
            if let Some(other) = fallback.take() {
                if let Ok(Some(d)) = find(&other) {
                    current = other;
                    break d;
                }
            }
            if let Ok(Some(d)) = find(&current) {
                break d;
            }
        };
        event(SourceEvent::DeviceBack { uid: current.clone() })?;
```

`DeviceSource::run` passes `&self.fallback`, `|u| find_input(u)` and `|d, u| run_segment(d, u, out, stop)`. `Fallback` is:

```rust
/// An input the person chose while theirs is gone (spec §4.1, §10): read once by the waiting source.
#[derive(Clone, Default)]
pub struct Fallback(Arc<std::sync::Mutex<Option<String>>>);

impl Fallback {
    pub fn offer(&self, uid: &str) {
        *self.0.lock().expect("the fallback lock") = Some(uid.to_string());
    }
    pub fn take(&self) -> Option<String> {
        self.0.lock().expect("the fallback lock").take()
    }
}
```

Construct `DeviceSource { uid, fallback: Fallback::default() }` wherever it is built (CLI, app, the ignored hardware test). The CLI's warning on `DeviceGone` names the other inputs: `▲ Input gone {uid}; waiting for it to return. To record from another input, stop (Ctrl-C) and start again with --device <UID>: {names and UIDs from list_inputs, comma-separated}`.

- [ ] **Step 4: Run the source tests and the core suite**

Run: `cargo test -p lecturelive-core --no-fail-fast 2>&1 | command grep -E "^test result|FAILED|panicked" | head -30`
Expected: every `test result: ok`.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/audio/source.rs crates/cli/src/main.rs apps/desktop/src-tauri/src/app.rs
git commit -m "Let the person choose another input while theirs is gone, never switching by itself, and name the other inputs in the CLI's warning"
```

---

### Task 5: Network loss for LectureLive alone; chat's `"error": null` (M3 minor)

**Files:**
- Create: `crates/core/src/net.rs`, `crates/core/examples/netcut.rs`
- Modify: `crates/core/src/lib.rs`, `crates/core/src/stt/stream.rs`, `crates/core/src/stt/rest.rs`, `crates/core/src/notes/chat.rs`, `crates/core/tests/{stt_stream.rs,stt_rest.rs,notes_chat.rs}`

**Interfaces:**
- Produces:
  - `net::API_ADDR_VAR: &str = "LECTURELIVE_API_ADDR"`, `net::api_addr() -> Option<SocketAddr>`;
  - `SttConfig.connect_to`, `RestConfig.connect_to` and `ChatConfig.connect_to`, each `Option<SocketAddr>`: connect there for the URL's host, with TLS still for that host. Their `new` constructors read `net::api_addr()`.

- [ ] **Step 1: Write the failing tests.** `stt_stream.rs`:

```rust
#[tokio::test]
async fn a_connect_address_carries_the_stream_while_the_url_keeps_its_host() {
    let fake = fake_stt::start(Config::default()).await;
    let addr: std::net::SocketAddr = fake.url.trim_start_matches("ws://").split('/').next().unwrap().parse().unwrap();
    let mut c = cfg(&fake);
    c.url = fake.url.replace(&addr.to_string(), "lecturelive.invalid:9");
    c.connect_to = Some(addr);
    let mut link = spawn(c).unwrap();
    let id = Uuid::new_v4();
    link.input.send(SttInput::Begin { recording_id: id }).await.unwrap();
    link.input.send(SttInput::Frame(speech_frame(id, 0))).await.unwrap();
    let connected = tokio::time::timeout(ms(5_000), async {
        while let Some(e) = link.events.recv().await {
            if matches!(e, SttEvent::Connected { .. }) {
                return true;
            }
        }
        false
    })
    .await;
    assert_eq!(connected, Ok(true), "lecturelive.invalid never resolves: only the address can have carried it");
}
```

`stt_rest.rs`:

```rust
#[tokio::test]
async fn a_connect_address_carries_recovery_requests() {
    let fake = fake_rest::start(None).await;
    let addr: std::net::SocketAddr = fake.url.trim_start_matches("http://").split('/').next().unwrap().parse().unwrap();
    let mut c = cfg(&fake.url.replace(&addr.to_string(), "lecturelive.invalid:9"));
    c.connect_to = Some(addr);
    let t = RestClient::new(c).unwrap().transcribe(&synthetic(0..20)).await.unwrap();
    assert!(!t.words.is_empty());
}
```

`notes_chat.rs`: the same shape with `fake_sse::start(|_| fake_sse::answer("ok", 1))`, `ChatConfig { url: …invalid…, connect_to: Some(addr), ..ChatConfig::new("k".into()) }`, asserting the answer's text is `"ok"`. Also:

```rust
#[tokio::test]
async fn an_error_key_that_is_null_is_not_an_error() {
    let body = b"data: {\"choices\":[{\"delta\":{\"content\":\"fine\"},\"finish_reason\":null}],\"error\":null}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: {\"choices\":[],\"usage\":{\"cost_in_usd_ticks\":1000}}\n\ndata: [DONE]\n\n".to_vec();
    let fake = fake_sse::start(move |_| fake_sse::Reply::Stream(vec![body.clone()])).await;
    let answer = client(&fake.url).complete(&request(), &mut |_| {}).await.unwrap();
    assert_eq!(answer.text, "fine");
}
```

(`client` and `request` are `notes_chat.rs`'s existing helpers. `Reply::Stream` is the fake's existing variant for raw pieces; adapt to its real name.)

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p lecturelive-core --test stt_stream --test stt_rest --test notes_chat connect_address error_key 2>&1 | tail -15`
Expected: FAIL to compile (`connect_to` does not exist); the null-error test fails with an error answer.

- [ ] **Step 3: Implement.**
  - `net.rs` holds a one-line doc ("Where LectureLive's requests to api.x.ai connect, when a check puts a forwarder in between (M6 plan, the error table)"), `API_ADDR_VAR`, and `api_addr()`: `std::env::var(API_ADDR_VAR).ok()?.parse().ok()`. `lib.rs` gains `pub mod net;`.
  - `stream.rs`: `open(url, key, connect_to)`. With `Some(addr)`: `let tcp = tokio::net::TcpStream::connect(addr).await.map_err(|e| ConnectError::Transient(e.to_string()))?;` then `tokio_tungstenite::client_async_tls_with_config(req, tcp, None, None).await`, matched exactly as `connect_async` is.
  - `rest.rs` and `chat.rs`: `let mut b = reqwest::Client::builder()…; if let (Some(a), Some(host)) = (cfg.connect_to, reqwest::Url::parse(&cfg.url).ok().and_then(|u| u.host_str().map(str::to_string))) { b = b.resolve(&host, a); }`.
  - `chat.rs` line 107: `if let Some(e) = v.get("error").filter(|e| !e.is_null()) {`.
  - `examples/netcut.rs`:

```rust
//! Stands between LectureLive and api.x.ai, so a check can take the network away from LectureLive alone
//! (M6 plan, the error table): `kill -STOP <pid>` holds every connection open with nothing passing, as a
//! dropped link does; `kill -CONT <pid>` lets traffic pass again; killing it refuses new connections. Zoom and
//! any other program keep the real network. LectureLive connects through it with
//! LECTURELIVE_API_ADDR=127.0.0.1:8443, and still checks api.x.ai's certificate.
//!   cargo run -p lecturelive-core --example netcut -- 127.0.0.1:8443 api.x.ai:443
#[tokio::main]
async fn main() -> std::io::Result<()> {
    let mut args = std::env::args().skip(1);
    let listen = args.next().unwrap_or_else(|| "127.0.0.1:8443".into());
    let upstream = args.next().unwrap_or_else(|| "api.x.ai:443".into());
    let listener = tokio::net::TcpListener::bind(&listen).await?;
    println!("netcut {} -> {upstream}, pid {}", listener.local_addr()?, std::process::id());
    loop {
        let (mut client, _) = listener.accept().await?;
        let up = upstream.clone();
        tokio::spawn(async move {
            if let Ok(mut server) = tokio::net::TcpStream::connect(&up).await {
                let _ = tokio::io::copy_bidirectional(&mut client, &mut server).await;
            }
        });
    }
}
```

- [ ] **Step 4: Run the tests, then a live forwarder check**

Run: `cargo test -p lecturelive-core --no-fail-fast 2>&1 | command grep -E "^test result|FAILED|panicked" | head -30`
Expected: every `test result: ok`.

Live: start the forwarder with `cargo run -q -p lecturelive-core --example netcut -- 127.0.0.1:8443 api.x.ai:443 &`, then `LECTURELIVE_API_ADDR=127.0.0.1:8443 cargo test -p lecturelive-core --test stt_live -- --ignored 2>&1 | tail -5`, then stop the forwarder.
Expected: the live STT tests pass through the forwarder; their cost is recorded in the ledger (a few cents at most).

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/net.rs crates/core/src/lib.rs crates/core/examples/netcut.rs crates/core/src/stt/stream.rs crates/core/src/stt/rest.rs crates/core/src/notes/chat.rs crates/core/tests/stt_stream.rs crates/core/tests/stt_rest.rs crates/core/tests/notes_chat.rs
git commit -m "Let LectureLive's requests connect through a forwarder a check can stop, so network loss can be LectureLive's alone; read a null error key as no error"
```

---

### Task 6: The error table in the session: disk and ledger failures, the last snapshot after a failure, `SourceEnded`, the marker, the CLI's course

**Files:**
- Modify: `crates/core/src/session/{coordinator.rs,lecture.rs,segments.rs}`, `crates/core/src/notes/chat.rs` (test), `crates/core/tests/lecture_gate.rs`, `crates/cli/src/main.rs`

**Interfaces:**
- Produces: `lecture::run` takes the last snapshot even when the session ended in failure, then returns that failure. `Notification::SourceEnded` is always delivered. `segments::session_marker` starts its line on a line of its own. CLI: `fn lecture_dir(dir: Option<PathBuf>) -> Result<PathBuf>` makes the folder absolute before `course` is derived.

- [ ] **Step 1: Write the failing tests.** `lecture_gate.rs`, beside the existing lecture helpers (a source that speaks, then fails):

```rust
/// §10 "Disk write error: session stops cleanly" (M3 minor): a session that fails still takes its last snapshot.
#[tokio::test]
async fn a_failed_session_still_takes_its_last_snapshot() {
    let (dir, files, stt, sse) = setup_lecture().await; // the gate's usual fake STT, fake SSE and folder
    let source = support::sources::Speech { then_fail: Some("disk full".into()), ..support::sources::Speech::words(40) };
    let (report, events) = run_lecture(&files, &stt, &sse, Box::new(source), vec![]).await;
    assert!(format!("{:#}", report.unwrap_err()).contains("disk full"));
    assert!(events.iter().any(|e| matches!(e, Event::Committed { .. })), "the last snapshot committed what was logged");
    let sc = Sidecar::load(&files.sidecar()).unwrap().unwrap();
    assert_eq!(sc.notes.segment_cursor, segments::read(&files.segments()).unwrap().len() as u64);
    drop(dir);
}
```

(`setup_lecture` and `run_lecture` are the gate file's existing setup, factored out if they are inline. `Speech::then_fail` adds `SourceEvent::Failed(msg)` after its last frame; add it to `support/sources.rs` if it does not exist.)

`coordinator.rs` `mod tests`:

```rust
    /// Review Focus 4.
    #[tokio::test]
    async fn a_sidecar_that_cannot_be_saved_stops_the_session_with_its_path() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join(".live_notes");
        std::fs::create_dir_all(&state).unwrap();
        let a = Uuid::new_v4();
        let script = Script(vec![begin(a, 0), frame(a, 0)]);
        let (handle, mut notes) = spawn(cfg(dir.path()), Box::new(script));
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o500)).unwrap(); // as a full disk refuses
        let result = handle.finish().await;
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700)).unwrap();
        let msg = format!("{:#}", result.unwrap_err());
        assert!(msg.contains(".live_notes"), "the path is surfaced: {msg}");
        let mut seen = Vec::new();
        while let Ok(n) = notes.try_recv() {
            seen.push(n);
        }
        assert!(seen.iter().any(|n| matches!(n, Notification::Failed(m) if m.contains(".live_notes"))));
    }

    /// M4 open thread: SourceEnded is the lecture's cue to stop taking operations, so a full notification
    /// channel must not drop it.
    #[tokio::test]
    async fn source_ended_is_delivered_even_when_notifications_back_up() {
        let dir = tempfile::tempdir().unwrap();
        let a = Uuid::new_v4();
        let mut events = vec![begin(a, 0)];
        events.extend((0..400).map(|k| SourceEvent::Level(k as f32 / 400.0))); // more than the channel holds
        events.push(frame(a, 0));
        events.push(SourceEvent::End { recording_id: a, samples: 1600, stream_errors: 0 });
        let (handle, mut notes) = spawn(cfg(dir.path()), Box::new(Script(events)));
        let reader = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await; // the lecture is busy, then reads
            let mut got = false;
            while let Some(n) = notes.recv().await {
                got |= matches!(n, Notification::SourceEnded);
            }
            got
        });
        handle.finish().await.unwrap();
        assert!(reader.await.unwrap());
    }

    /// §10 "Spend ledger write fails": a warning; recording and transcription go on.
    #[tokio::test]
    async fn a_ledger_that_cannot_be_written_is_a_warning_and_the_session_goes_on() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let ledger = dir.path().join("spend.jsonl");
        let spend = Spend::open(&ledger, "C", "W", chrono::NaiveDate::from_ymd_opt(2026, 9, 26).unwrap()).unwrap();
        std::fs::write(&ledger, "").unwrap();
        std::fs::set_permissions(&ledger, std::fs::Permissions::from_mode(0o400)).unwrap();
        let (report, notes) = run_with(SessionConfig { spend: Some(spend), ..live_cfg(dir.path()).await }, speech_source()).await;
        assert!(report.unwrap().segments > 0, "transcription went on");
        assert!(notes.iter().any(|n| matches!(n, Notification::SpendFailed(_))));
    }
```

(`run_with`, `live_cfg` and `speech_source` are the helpers of `streamed_and_recovered_audio_are_written_to_the_ledger`; reuse or factor them out.) `chat.rs` `mod tests` gets the chat half of the same row: a `ChatClient` whose ledger is read-only answers with its text, and `warning` is `Some(…"spend ledger"…)`.

`segments.rs` `mod tests`:

```rust
    /// M3 minor: a session line never glues onto a line a crash cut short.
    #[test]
    fn a_session_marker_after_a_torn_line_starts_a_line_of_its_own() {
        let dir = tempfile::tempdir().unwrap();
        let t = dir.path().join("t.txt");
        std::fs::write(&t, "--- started 10:00:00 ---\n--- resu").unwrap();
        session_marker(&t, anchor() + chrono::Duration::minutes(3)).unwrap();
        assert_eq!(std::fs::read_to_string(&t).unwrap(), "--- started 10:00:00 ---\n--- resu\n--- resumed 10:03:00 ---\n");
    }
```

CLI `#[cfg(test)] mod tests`:

```rust
    /// M3 minor: the course comes from the folder's absolute path, so `--dir .` names the course above Weeks/.
    #[test]
    fn a_relative_folder_is_made_absolute_before_the_course_is_read() {
        let root = tempfile::tempdir().unwrap();
        let week = root.path().join("Machine Learning/Weeks/Week 01");
        std::fs::create_dir_all(&week).unwrap();
        let here = std::env::current_dir().unwrap();
        std::env::set_current_dir(&week).unwrap();
        let dir = lecture_dir(Some(PathBuf::from("."))).unwrap();
        std::env::set_current_dir(here).unwrap();
        assert_eq!(course_from_path(&dir).as_deref(), Some("Machine Learning"));
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p lecturelive-core --no-fail-fast failed_session sidecar_that source_ended ledger_that torn_line 2>&1 | tail -20; cargo test -p lecturelive-cli relative_folder 2>&1 | tail -5`
Expected: FAIL. The snapshot is skipped, `SourceEnded` is dropped, the marker glues onto the torn line, and `lecture_dir` does not exist. The sidecar-path test may already pass (`Sidecar::save` names its path); if so, it pins the behaviour.

- [ ] **Step 3: Implement.**
  - `lecture::run`: `let result = handle.finish().await;`. The page tasks are handled as now. Then take the last snapshot when a sidecar exists (the load's error, if any, joins the warning), and finally `result`, not `?` before the snapshot.
  - `coordinator::run`: `let _ = c.notify.send(Notification::SourceEnded).await;` instead of `c.notify(…)`. It is the lecture's cue, and the lecture drains its channel.
  - `session_marker`: before writing, `if std::fs::read(transcript).is_ok_and(|b| b.last().is_some_and(|&c| c != b'\n')) { f.write_all(b"\n")?; }`.
  - CLI: `fn lecture_dir(dir: Option<PathBuf>) -> Result<PathBuf> { let d = dir.unwrap_or(std::env::current_dir()?); Ok(std::fs::canonicalize(&d).with_context(|| format!("no folder {}", d.display()))?) }`, used by `lecture` and `record` before `course_from_path`.

- [ ] **Step 4: Run the tests and the suites**

Run: `cargo test -p lecturelive-core --no-fail-fast 2>&1 | command grep -E "^test result|FAILED|panicked" | head -30; cargo test -p lecturelive-cli 2>&1 | tail -3`
Expected: every `test result: ok`.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/session/coordinator.rs crates/core/src/session/lecture.rs crates/core/src/session/segments.rs crates/core/src/notes/chat.rs crates/core/tests/lecture_gate.rs crates/core/tests/support/sources.rs crates/cli/src/main.rs
git commit -m "Take the last snapshot after a failed session, always deliver SourceEnded, keep session lines on their own line, and read the course from the absolute folder"
```

---

### Task 7: The audit of a lecture folder's audio (gate line 1's instrument)

**Files:**
- Create: `crates/core/src/session/audit.rs`
- Modify: `crates/core/src/session/mod.rs`, `crates/cli/src/main.rs`

**Interfaces:**
- Produces (`session::audit`):
  - `#[derive(Debug, Default)] pub struct Audit { pub lines: Vec<String>, pub unexplained: usize, pub waiting: usize }`
  - `pub fn audit_day(files: &LectureFiles) -> Result<Audit>`
  - `pub fn audit_folder(dir: &Path) -> Result<Vec<(String, Audit)>>`: one per sidecar in the folder, by stem.
- CLI: `lecturelive lecture audit [--dir …]` prints each day's lines and a summary, `N unexplained, M waiting`. It exits 1 when anything is unexplained.

The rules, in time order per day:
1. **Recordings.** A finalized recording's WAV holds exactly the samples the sidecar says, `(file length − 44) / 2`; otherwise it is unexplained. An open recording waits for repair. A missing or pruned one is listed with its state.
2. **Between consecutive recordings,** end = anchor + samples / 16 kHz, then:
   - a hole under 1.0 s is contiguous;
   - a longer hole is explained by the earlier recording's open-ended gap (`device_gone`, `rate_change`, `interrupted`), or by a session line in the transcript (`--- started|resumed HH:MM:SS ---`) that falls between the earlier end less 2 s and the later anchor plus 2 s (a new session);
   - any other hole is unexplained;
   - an overlap over 0.5 s (the later recording begins before the earlier ends) is unexplained.
3. **Inside recordings,** gaps are listed by kind (explained).
4. **Segments,** per recording and sorted by start: one that begins more than 10 ms before the previous one ends is an unexplained duplicate.
5. **Transcript gaps** not yet resolved are waiting.

- [ ] **Step 1: Write the failing tests** in `audit.rs` `mod tests`, building folders with `Sidecar`, `Recorder` (real WAVs of known length), `segments::write_all` and a transcript file:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::frame::Frame;
    use crate::audio::recorder::Recorder;
    use crate::session::segments::{self, Segment, SegmentSource};
    use crate::session::sidecar::{Gap, GapKind, RecState, RecordingEntry, Sidecar};
    use chrono::{Local, TimeZone};
    use uuid::Uuid;

    struct Folder {
        _dir: tempfile::TempDir,
        files: LectureFiles,
        sc: Sidecar,
    }

    fn folder() -> Folder {
        let dir = tempfile::tempdir().unwrap();
        let files = LectureFiles::standard(dir.path(), chrono::NaiveDate::from_ymd_opt(2026, 9, 26).unwrap());
        std::fs::create_dir_all(files.state_dir()).unwrap();
        std::fs::write(&files.transcript, "--- started 10:00:00 ---\n").unwrap();
        Folder { _dir: dir, files, sc: Sidecar::default() }
    }

    impl Folder {
        /// A finalized recording of `secs` seconds starting at 10:mm:ss.
        fn recording(&mut self, at: (u32, u32), secs: u64) -> Uuid {
            let id = Uuid::new_v4();
            let stem = format!("session_20260926_10{:02}{:02}", at.0, at.1);
            let mut r = Recorder::create(&self.files.dir.join("recordings"), &stem).unwrap();
            for k in 0..secs * 10 {
                r.write_frame(&Frame { recording_id: id, sample_offset: k * 1600, valid_samples: 1600, pcm16: [0; 1600] }).unwrap();
            }
            let path = r.finalize().unwrap();
            let file = path.strip_prefix(&self.files.dir).unwrap().to_string_lossy().into_owned();
            let anchor = Local.with_ymd_and_hms(2026, 9, 26, 10, at.0, at.1).unwrap();
            self.sc.recordings.push(RecordingEntry { id, file, anchor, source_uid: "BlackHole2ch_UID".into(), input_rate: 48_000, samples: Some(secs * 16_000), state: RecState::Finalized });
            id
        }
        fn run(&self) -> Audit {
            self.sc.save(&self.files.sidecar()).unwrap();
            audit_day(&self.files).unwrap()
        }
    }

    #[test]
    fn contiguous_recordings_and_a_marked_absence_are_whole() {
        let mut f = folder();
        let a = f.recording((0, 0), 60);
        f.sc.gaps.push(Gap::new(a, 60 * 16_000, None, GapKind::DeviceGone));
        f.recording((1, 30), 60); // 30 s later: the receiver was away
        let audit = f.run();
        assert_eq!((audit.unexplained, audit.waiting), (0, 0), "{:#?}", audit.lines);
    }

    #[test]
    fn a_hole_without_a_gap_or_a_new_session_is_unexplained() {
        let mut f = folder();
        f.recording((0, 0), 60);
        f.recording((1, 30), 60);
        let audit = f.run();
        assert_eq!(audit.unexplained, 1, "{:#?}", audit.lines);
        assert!(audit.lines.iter().any(|l| l.contains("30.0 s") && l.contains("unexplained")));
    }

    #[test]
    fn a_hole_with_a_new_session_line_is_a_stop_and_a_start() {
        let mut f = folder();
        f.recording((0, 0), 60);
        std::fs::write(&f.files.transcript, "--- started 10:00:00 ---\n--- resumed 10:01:25 ---\n").unwrap();
        f.recording((1, 30), 60);
        assert_eq!(f.run().unexplained, 0);
    }

    #[test]
    fn overlapping_recordings_are_unexplained() {
        let mut f = folder();
        f.recording((0, 0), 60);
        f.recording((0, 50), 60);
        assert_eq!(f.run().unexplained, 1);
    }

    #[test]
    fn a_wav_shorter_than_its_sidecar_says_is_unexplained() {
        let mut f = folder();
        f.recording((0, 0), 10);
        f.sc.recordings[0].samples = Some(11 * 16_000);
        assert_eq!(f.run().unexplained, 1);
    }

    /// Review Focus 5.
    #[test]
    fn an_unrepaired_recording_is_waiting_not_whole() {
        let mut f = folder();
        f.recording((0, 0), 10);
        f.sc.recordings[0].state = RecState::Open;
        f.sc.recordings[0].samples = None;
        let audit = f.run();
        assert_eq!(audit.waiting, 1, "{:#?}", audit.lines);
        assert!(audit.lines.iter().any(|l| l.contains("waits for repair")));
    }

    #[test]
    fn overlapping_segments_are_an_unexplained_duplicate() {
        let mut f = folder();
        let a = f.recording((0, 0), 10);
        let anchor = f.sc.recordings[0].anchor;
        let seg = |id: u64, s: u64, e: u64| Segment::test(id, a, s, e, anchor, SegmentSource::Live);
        segments::write_all(&f.files.segments(), &[seg(0, 0, 32_000), seg(1, 16_000, 48_000)]).unwrap();
        assert_eq!(f.run().unexplained, 1);
    }

    #[test]
    fn an_unrecovered_transcript_gap_is_waiting() {
        let mut f = folder();
        let a = f.recording((0, 0), 10);
        f.sc.gaps.push(Gap::new(a, 0, Some(16_000), GapKind::SttOffline));
        let audit = f.run();
        assert_eq!((audit.unexplained, audit.waiting), (0, 1));
    }
}
```

(`Segment::test(id, recording, start, end, anchor, source)` is a `#[cfg(test)]` constructor. Add it to `segments.rs` if the tests there build segments otherwise.)

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p lecturelive-core session::audit 2>&1 | tail -10`
Expected: FAIL to compile (`audit` does not exist).

- [ ] **Step 3: Implement** `audit.rs` by the rules above, with a module doc. Read the sidecar with `Sidecar::load`, the segments with `segments::read`, and the session lines from the transcript with `regex::Regex::new(r"^--- (started|resumed) (\d\d):(\d\d):(\d\d) ---$")`. They are dated on the lecture date and roll to the next day once a time runs back past midnight. Each finding is one line in the CLI's words: `10:01:00  hole of 30.0 s after recordings/session_20260926_100000.wav: unexplained`; `10:00:00  recordings/session_… waits for repair (a session in this folder repairs it)`; `10:00:30  gap device_gone from 60.0 s (explained)`; `—  2 transcript gaps wait for recovery`. The CLI's `lecture audit` prints each day under its stem, then `N unexplained, M waiting`, and `std::process::exit(1)` when N > 0.

- [ ] **Step 4: Run the tests; audit the M5 and M4 synthetic folders as a smoke test**

Run: `cargo test -p lecturelive-core session::audit 2>&1 | tail -3; ./target/debug/lecturelive lecture audit --dir "$HOME/Library/Application Support/LectureLive/m4-live/Weeks/Week 04 — Live" ; echo "exit $?"` (find the real week folder with `command ls`).
Expected: 8 passed; the smoke test prints its lines and exits 0 or explains each finding. Record its output in the ledger.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/session/audit.rs crates/core/src/session/mod.rs crates/core/src/session/segments.rs crates/cli/src/main.rs
git commit -m "Audit a lecture folder's audio: recordings against their files, holes and overlaps against gaps and sessions, and duplicate segments"
```

---

### Task 8: The desktop adapter: microphone, fallback, mixed start, gaps and capture after the end

**Files:**
- Modify: `apps/desktop/src-tauri/src/{app.rs,adapter.rs,wire.rs,lib.rs}`

**Interfaces:**
- Consumes: `Fallback` (Task 4), `MixedSource`, `MIXED_MODE` (Task 3).
- Produces:
  - Commands:
    - `microphone() -> Res<String>` (`"granted" | "denied" | "restricted" | "undetermined"`);
    - `open_microphone_settings() -> Res<()>`;
    - `use_input(uid: String) -> Res<()>`, refused unless the running single input is gone and the UID is listed;
    - `start_lecture(source)` also takes `"mixed:<uid>"`.
  - `LoopbackView.mixed: bool` (true when `MIXED_MODE` and BlackHole is present).
  - `Status.input_gone: Option<String>` (the gone UID, single input only).
  - `pub enum SourceKind { Input, Loopback, Mixed }`; `Pump::new(session, sink, spend, kind)`.
  - `Pump::ended(&mut self)`: the end's status, with capture back to its before-lecture word.
  - `Pump::set_open_gaps(&mut self, n: usize)`.
  - `CheckConfig.minutes: Option<u64>` from `LECTURELIVE_CHECK_MINUTES`.
  - Reports go to `m6-checks/`.

- [ ] **Step 1: Write the failing tests** in `adapter.rs` `mod tests` (the pump) and `app.rs` `mod tests`:

```rust
    #[test]
    fn a_single_input_that_goes_is_offered_a_fallback_and_one_that_returns_clears_it() {
        let (mut p, _) = pump_with(SourceKind::Input);
        p.apply(Event::Session(Notification::DeviceGone { uid: "Receiver_UID".into() }));
        assert_eq!(p.status().input_gone.as_deref(), Some("Receiver_UID"));
        p.apply(Event::Session(Notification::DeviceBack { uid: "BuiltInMicrophoneDevice".into() }));
        assert_eq!(p.status().input_gone, None);
        assert!(p.notices().last().unwrap().detail.contains("BuiltInMicrophoneDevice"), "Recording from the input now used");
    }

    #[test]
    fn in_mixed_mode_a_gone_input_is_a_notice_not_an_offer() {
        let (mut p, _) = pump_with(SourceKind::Mixed);
        p.apply(Event::Session(Notification::DeviceGone { uid: "Receiver_UID".into() }));
        assert_eq!(p.status().input_gone, None);
        assert!(p.notices().last().unwrap().detail.contains("Zoom goes on"));
    }

    #[test]
    fn mixed_mode_watches_the_loopback_for_silence() {
        let (mut p, _) = pump_with(SourceKind::Mixed);
        for _ in 0..10 {
            p.apply(Event::Session(Notification::Level(0.0)));
        }
        assert!(p.status().silence);
    }

    /// M4 minor M9: gaps a crash left are counted from the start, and recovery counts them down.
    #[test]
    fn open_gaps_from_earlier_sessions_are_counted_from_the_start() {
        let (mut p, _) = pump_with(SourceKind::Loopback);
        p.set_open_gaps(2);
        p.apply(Event::Session(Notification::Recovered(Gap::new(uuid::Uuid::new_v4(), 0, Some(1), GapKind::SttInterrupted))));
        assert_eq!(p.status().gaps, 1);
    }

    /// M5 minor M1: once the lecture has ended the strip no longer says Watching.
    #[test]
    fn the_end_puts_capture_back_to_its_word_before_a_lecture() {
        let (mut p, _) = pump_with(SourceKind::Loopback);
        p.apply(Event::Capture(CaptureState::Watching { window: "Zoom Meeting".into(), region: (1180, 664) }));
        p.ended();
        assert_eq!(p.status().phase, Phase::Ended);
        assert_ne!(p.status().capture.state, CaptureWord::Watching);
    }
```

(`pump_with(kind)` builds a `Pump` with a recording sink, as the file's other tests do; `status()` and `notices()` read the mirror. Adapt `CaptureState::Watching`'s fields to their real shape.)

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p desktop 2>&1 | tail -15`
Expected: FAIL to compile (`SourceKind`, `input_gone`, `set_open_gaps`, `ended`).

- [ ] **Step 3: Implement.**
  - `Pump::new` takes `SourceKind`. The silence watch applies to `Loopback | Mixed`.
  - On `DeviceGone`: for `Input`, set `input_gone` and the notice "{uid}; waiting for it to return. Choose another input below to record from it."; for `Mixed`, the notice "{uid}; Zoom goes on, and the gap is marked".
  - On `DeviceBack`: clear `input_gone`; the notice is "Input back" when the UID is the one that went, else "Recording from {uid}".
  - `ended()` sets the phase, busy, level and capture, keeping the watched window's label with the word `Ready`. `start_lecture`'s end handler calls it.
  - `start_lecture`: after `prepare`, `set_open_gaps` from the sidecar's unresolved transcript gaps. It parses `mixed:<uid>` (refused with "Mixed mode is off in this build." unless `MIXED_MODE`), names the source "Zoom + {name}", and keeps the `Fallback` of a single input in `Running`.
  - `get_session_state`: `tokio::time::sleep(Duration::from_millis(50)).await` between hash retries (M4 minor M1).
  - `open_microphone_settings` opens `x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone`.
  - `use_input` checks `input_gone` and `list_inputs`, then `fallback.offer(uid)`.
  - `check_report` writes to `m6-checks`; `check_config` reads `LECTURELIVE_CHECK_MINUTES`.
  - Register the new commands in `lib.rs`.

- [ ] **Step 4: Run the desktop tests**

Run: `cargo test -p desktop 2>&1 | command grep -E "test result|FAILED|panicked"`
Expected: `test result: ok`, 21 or more passed, 1 ignored.

- [ ] **Step 5: Commit**

```bash
git add apps/desktop/src-tauri/src/app.rs apps/desktop/src-tauri/src/adapter.rs apps/desktop/src-tauri/src/wire.rs apps/desktop/src-tauri/src/lib.rs
git commit -m "Offer a fallback when a single input goes, start mixed lectures, report the microphone's state, and count earlier gaps from the start"
```

---

### Task 9: The views: microphone fix-it, fallback offer, mixed choice; the store; the faults check

**Files:**
- Modify: `apps/desktop/src/lib/{wire.ts,transport.ts,fixture.ts,session.svelte.ts,session.test.ts,FolderPicker.svelte,CommandLine.svelte,SlidesStrip.svelte,checks.ts}`, `apps/desktop/src/routes/+page.svelte`

**Interfaces:**
- Consumes: Task 8's commands and status.
- Produces (`session.svelte.ts`): `session.microphone(): Promise<string>`, `session.openMicrophoneSettings()`, `session.useInput(uid)`, `session.fallbacks: InputView[]` (read when `input_gone` first appears in a status message); `stopLabel` is `null` while starting; every `hydrate()` rejection becomes `session.error`.

- [ ] **Step 1: Invoke `/frontend-design:frontend-design`** for the three views and the Screen Recording copy, within the Visual design above. Record the run (pass 1, pass 2, what changed) in the ledger before any markup.

- [ ] **Step 2: Write the failing store tests** in `session.test.ts` (fixture transport):

```ts
it("offers the other inputs when the single input goes, and records from the one chosen", async () => {
  const { t, session } = await started({ source: "BuiltInMicrophoneDevice" });
  t.answers.inputs = [{ name: "MacBook Pro Microphone", uid: "BuiltInMicrophoneDevice" }, { name: "Wireless Mic Rx", uid: "Rx" }];
  t.emitStatus({ ...t.status, input_gone: "Rx" });
  await tick();
  expect(session.fallbacks.map((i) => i.uid)).toEqual(["BuiltInMicrophoneDevice"]);
  await session.useInput("BuiltInMicrophoneDevice");
  expect(t.calls.at(-1)).toEqual(["use_input", { uid: "BuiltInMicrophoneDevice" }]);
});

it("does not offer Stop while the lecture is starting", async () => {
  const { t, session } = await started({});
  t.emitStatus({ ...t.status, phase: "starting" });
  await tick();
  expect(session.stopLabel).toBeNull();
});

it("shows a failed hydration as the error instead of leaving it unhandled", async () => {
  const t = fixtureTransport();
  t.fail.get_session_state = "the sidecar could not be read";
  const session = await attachedSession(t);
  expect(session.error).toContain("the sidecar could not be read");
});
```

(`started`, `attachedSession`, `emitStatus`, `answers`, `calls` and `fail` are the fixture's existing helpers, or small additions to it.)

- [ ] **Step 3: Run to verify they fail**

Run: `cd apps/desktop && npx vitest run 2>&1 | tail -15`
Expected: the three new tests FAIL.

- [ ] **Step 4: Implement the store, then the views.**
  - Store:
    - status messages with a new `input_gone` read `inputs()` into `fallbacks`, less the gone one and BlackHole; BlackHole is offered as "Zoom through LectureLive Loopback" when present;
    - `stopLabel` is `"Stop"` only while running;
    - every `hydrate()` call ends in `.catch((e) => (this.error = String(e)))`.
  - `FolderPicker.svelte`:
    - `refresh()` also reads `session.microphone()`;
    - when it is `denied` or `restricted`, a `<p class="line error" role="alert">` reads "▲ Microphone access is off, so nothing can be recorded. Turn it on in System Settings, then press Refresh." with `<button class="outline" onclick={() => session.openMicrophoneSettings()}>Open Settings</button>`;
    - Start is disabled, with `title="Microphone access is off"`;
    - when `loop.mixed`, the select adds `<option value="mixed:{i.uid}">Zoom and {i.name} together</option>` per input after the single sources;
    - while a mixed source is chosen, `<span class="hint">Wear headphones, so the microphone does not hear Zoom as well.</span>`.
  - `CommandLine.svelte`: while running with `session.status.input_gone`, the notice line's place holds `<div class="offer" role="alert">`. It contains the ▲ mark in signal and "<strong>{session.status.source}</strong> is unplugged. LectureLive waits for it and records nothing meanwhile.", then either "Record from `<select>` `<button class="outline">Record from it</button>`" or "No other input is connected.". It wraps to two lines, never clips, and the prompt row stays below it.
  - `SlidesStrip.svelte`: the denied note becomes "Allow LectureLive there, then quit and reopen LectureLive." (M5 minor M2: macOS applies a new Screen Recording grant only to a reopened app).

- [ ] **Step 5: Add `faultsCheck`** to `checks.ts` and its mode `faults` to `+page.svelte`:
  - it selects `cfg.dir`, starts `"loopback"`, and every 500 ms records a row whenever the phase, `stt`, `gaps`, `input_gone` or the latest notice changes, as `{at, phase, stt, gaps, notice}`;
  - every 5 s it writes the rows to `check_report` as `faults-<launch time>`;
  - after `cfg.minutes` (default 10) it stops, waits for `ended`, writes the report, and quits;
  - a relaunch in the same folder starts a new report.

- [ ] **Step 6: Run the frontend tests and the type check**

Run: `cd apps/desktop && npx vitest run 2>&1 | tail -5 && npx svelte-check 2>&1 | tail -3`
Expected: all pass; 0 errors, 0 warnings.

- [ ] **Step 7: Check every changed view at 1168 px wide, in normal and large type, light and dark.** Start the dev server. In Playwright (Chromium, the fixture transport at `localhost:1420`), resize to 1168 × 730 and look at:
  - the before-lecture bar with the microphone denied, with a mixed source chosen, and with neither;
  - the command bar with the offer, with no other input, and in large type (the offer wraps; nothing clips, including the status strip's clock and the traffic-light inset);
  - the strip's denied state.

  Take each in light and dark. Fix what clips. Delete every screenshot after reading it. Record the checks in the ledger.

- [ ] **Step 8: Commit**

```bash
git add apps/desktop/src/lib/wire.ts apps/desktop/src/lib/transport.ts apps/desktop/src/lib/fixture.ts apps/desktop/src/lib/session.svelte.ts apps/desktop/src/lib/session.test.ts apps/desktop/src/lib/FolderPicker.svelte apps/desktop/src/lib/CommandLine.svelte apps/desktop/src/lib/SlidesStrip.svelte apps/desktop/src/lib/checks.ts apps/desktop/src/routes/+page.svelte
git commit -m "Show the microphone fix-it, the fallback offer and the mixed choice; offer Stop only once running; surface failed hydrations"
```

---

### Task 10: §10 live on synthetic lectures: network loss, forced restart, disk full, a refused key; the REST first-word measurement

**Files:** none in the repository (evidence in the ledger and `$HOME/Library/Application Support/LectureLive/m6-*`).

- [ ] **Step 1: Prepare** the folders:
  - `m6-faults/Weeks/Week 01 — Faults` (the CLI run);
  - `m6-app/Weeks/Week 01 — App` (the app run);
  - `m6-disk` (the disk image's mount point comes from `hdiutil`).

  Write a speech script: 150 numbered, non-repeating sentences on one lecture topic, each followed by `[[slnc 1200]]`, as `$HOME/Library/Application Support/LectureLive/m6-faults/speech.txt`. Note the default output (`./target/debug/lecturelive outputs`).
- [ ] **Step 2: Network loss and a forced restart through the CLI** (about 15 minutes). Kill any `say` first. Then:
  1. start the forwarder;
  2. `LECTURELIVE_API_ADDR=127.0.0.1:8443 ./target/debug/lecturelive lecture --loopback --dir "$F"`, with its stdin from a FIFO the script writes Enter into;
  3. `say -a "BlackHole 2ch" -f speech.txt` in a loop shell.

  Timeline:
  - 1:30 snapshot;
  - 3:00 `kill -STOP` the forwarder for 60 s, then `kill -CONT`;
  - 6:00 kill the forwarder for 30 s, then restart it;
  - 8:00 snapshot;
  - 9:00 `kill -9` the CLI, then start it again the same way;
  - 11:00 snapshot;
  - 12:30 Ctrl-C (SIGINT) to stop.

  Expected, from its output and files:
  - "reconnecting" lines;
  - `stt_offline` gaps recovered (`(recovered)` lines);
  - the relaunch prints "Repaired" and an `interrupted` gap, and recovers an `stt_interrupted` gap;
  - the last snapshot is taken;
  - `lecture audit` exits 0 with 0 waiting;
  - the default output is unchanged.
- [ ] **Step 3: Measure the REST first word** from Step 2's folder. For each `recovered` segment, compare its first word with the speech script's sentence that contains the segment's text; count the first-word mismatches against all recovered segments. Record the numbers. If any mismatch is at the clip's edge, take the lead-in remedy test-first in `stt/rest.rs`: start each request 1.0 s before its piece, keep only words starting at or after the piece less 0.1 s, and add a fake-REST test that garbles a clip's first word when it starts within 0.3 s of the clip's start. Then repeat Step 2 once. Otherwise record the thread as left, with the numbers.
- [ ] **Step 4: Disk full** (about 5 minutes). Run:
  1. `hdiutil create -size 40m -fs HFS+ -volname LLDisk -ov "$HOME/Library/Application Support/LectureLive/m6-disk/disk.dmg"`;
  2. `hdiutil attach -nobrowse …`;
  3. create `/Volumes/LLDisk/Weeks/Week 01 — Disk`;
  4. start the CLI lecture there (loopback, the forwarder not needed), with speech;
  5. at 1:00, `dd if=/dev/zero of=/Volumes/LLDisk/filler bs=1m` until it stops.

  Expected: the session stops with a message naming a path on `/Volumes/LLDisk`, and the last snapshot is attempted. Then `rm` the filler and start the CLI again. Expected: the WAV is repaired, the session resumes, and `lecture audit` explains the stop. Detach the image and delete it.
- [ ] **Step 5: The same faults in the app** (about 12 minutes): the dev server, then `LECTURELIVE_CHECK=faults LECTURELIVE_CHECK_DIR="$A" LECTURELIVE_CHECK_MINUTES=10 LECTURELIVE_API_ADDR=127.0.0.1:8443 ./target/debug/desktop`, with speech. Timeline:
  - 2:00 `kill -STOP` the forwarder for 45 s, then `kill -CONT`;
  - 5:00 `kill -9` the app, then relaunch with `LECTURELIVE_CHECK_MINUTES=4`.

  Expected, in `m6-checks/faults-*.json`:
  - `stt` goes to "reconnecting…" and back to connected;
  - the gaps rise, then return to 0;
  - the second launch's first notices are "Repaired" and "Recovered", and its gap count starts at the crash's gaps (M4 minor M9).

  `lecture audit` exits 0 afterwards.
- [ ] **Step 6: A refused key in the app.** `GROK_API_KEY=xai-bad LECTURELIVE_CHECK=faults LECTURELIVE_CHECK_MINUTES=1 …`, in a fresh synthetic folder. Expected: `stt` reads "refused: …", recording continues, and the report shows one refusal and no reconnect loop.
- [ ] **Step 7: Record** each run's evidence in the ledger: commands, times, report paths, the audit output, and costs from `spend.jsonl` under courses `m6-faults`, `m6-app` and `m6-disk`. Confirm the default output was restored. Kill every leftover `say`, loop shell and forwarder.

---

### Task 11: Mixed mode: the live two-hour two-device run, and the decision

- [ ] **Step 1: Start the two-hour run in the background,** right after Task 3 (it needs no attention, and other work goes on beside it):

`LECTURELIVE_MIX_SECS=7200 cargo test -p lecturelive-core blackhole_and_the_built_in -- --ignored --nocapture > "$HOME/Library/Application Support/LectureLive/m6-checks/mix-live-7200.txt" 2>&1`

- [ ] **Step 2: When it ends, read it:** the stats lines, the worst relative offset after 30 s, the corrections in ppm, and the underruns. Record the numbers in the ledger.
- [ ] **Step 3: Decide.** Mixed mode ships (`MIXED_MODE = true`) if all three hold:
  - `two_sources_on_drifting_clocks_stay_aligned_for_two_hours` passes;
  - `five_hundred_ppm_either_way_is_held_within_20_ms` passes;
  - the live run passes (no gap, no underrun, worst relative offset ≤ 10 ms).

  Otherwise set `MIXED_MODE = false`, and confirm that `lecturelive lecture --mixed …` refuses and that the app's select has no mixed options. Record the decision and its numbers as a Ruling. Commit only if the constant changed (`git add crates/core/src/audio/mixed.rs`, message "Ship mixed mode disabled: it did not pass the live two-hour drift run").

---

### Task 12: Freeze the golden tests; prepare and run V09 (before the Python CLI goes)

**Files:**
- Create: `crates/core/tests/fixtures/prompts/live_notes_excerpt.txt`
- Modify: `crates/core/src/notes/prompts.rs`

- [ ] **Step 1: Write the excerpt.** Copy verbatim from `live_notes.py`, with `sed -n 'a,bp'` for each range:
  - the `def notes_system`, `def polish_system`, `def page_system` and `def revise_system` blocks;
  - every line containing a fragment that `user_message_fragments_appear_in_live_notes_py` lists;
  - the `title = f"…"` line.

  Keep them in the source's order, and put a first line saying where they came from (`# Verbatim from live_notes.py at <commit>, frozen before its removal (M6).`).
- [ ] **Step 2: Write the failing check** in `prompts.rs` tests:

```rust
    /// The excerpt is live_notes.py verbatim, block by block, while live_notes.py exists (removed with it in M6 Task 17).
    #[test]
    fn the_frozen_excerpt_is_live_notes_py_verbatim() {
        let py = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../live_notes.py")).expect("live_notes.py");
        for block in excerpt().split("\n\n").skip(1) {
            assert!(py.contains(block), "not in live_notes.py:\n{block}");
        }
    }
```

Change `live_notes_source()` to `excerpt()`, reading the fixture (`concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/prompts/live_notes_excerpt.txt")`). The two golden tests are otherwise unchanged.

- [ ] **Step 3: Run the prompt tests**

Run: `cargo test -p lecturelive-core notes::prompts 2>&1 | tail -5`
Expected: 4 passed (the two goldens now read the excerpt; the hand goldens; the verbatim check).

- [ ] **Step 4: Commit**

```bash
git add crates/core/tests/fixtures/prompts/live_notes_excerpt.txt crates/core/src/notes/prompts.rs
git commit -m "Freeze the prompt goldens into a verbatim excerpt of live_notes.py, so they outlive its removal"
```

- [ ] **Step 5: Prepare and run V09.**
  1. In `$HOME/Library/Application Support/LectureLive/m6-v09/Weeks/Week 01 — Compare`, write a synthetic notes file of about 700 words, with no slides.
  2. Run `./target/debug/lecturelive lecture page --dir <it>` once. That is one live page request (about $0.10), and it writes the cache and the page.
  3. Copy the folder to `…/m6-v09/Weeks/Week 01 — Compare (Python)`, delete the copy's `.live_notes/*.v2.json` and its HTML page, then `cd` into the copy and run `"$HOME/Documents/Tools/LectureLive/.venv/bin/python" "$HOME/Documents/Tools/LectureLive/live_notes.py" page`. Expected: "notes unchanged since they were typeset… (free)", and a new HTML page.
  4. `cmp` the two pages.

  Record the result, byte-identical or the first differing offset with its context, in the ledger. Task 13 writes the same commands into V09.

---

### Task 13: `docs/VERIFICATION.html`: the two-hour lecture occasion and the revised items

**Files:**
- Modify: `docs/VERIFICATION.html`

- [ ] **Step 1: Invoke `/frontend-design:frontend-design`** for the new occasion, within the page's own tokens and its run-sheet treatment (the Visual design above). Record the run in the ledger.
- [ ] **Step 2: Write the occasion** in place of "Coming with M6": `<section class="occasion lecture" id="m6-lecture">` with the heading "Your two-hour lecture with LectureLive". Its `why` says it closes M6's gate, that the Python tool keeps taking the notes of record in its own folder, and that no injected failure touches it. Rows, in class order:
  - **Before class** (step, about 10 minutes):
    - plug in the DJI receiver;
    - Terminal 1: `npm run dev`;
    - Terminal 2: the forwarder (`cargo run -q -p lecturelive-core --example netcut -- 127.0.0.1:8443 api.x.ai:443`, noting its pid);
    - Terminal 3: the disk image (`hdiutil create -size 2g -fs HFS+ -volname LLLecture …` and `attach`), a lecture folder on it (`/Volumes/LLLecture/<Course>/Weeks/<Week NN — Title>`);
    - Terminal 4: the app, `LECTURELIVE_API_ADDR=127.0.0.1:8443 LECTURELIVE_RECORD="$HOME/Library/Application Support/LectureLive/m6-fixtures/zoom-live-$(date +%Y%m%d)" ./target/debug/desktop`;
    - the Python tool in its own usual folder;
    - in the app: Choose lecture folder (the one on the image), "Zoom and Wireless Mic Rx together" (or Zoom alone if mixed shipped disabled), choose Zoom's window with the camera left out (V02's steps), Start;
    - answer "Always Allow" if the Keychain asks.
  - **V01** (r2), "0:00–0:30": nothing injected; the first 30 minutes are V01's recording, read by the audit.
  - **V10**, "0:35 in", network loss for LectureLive alone: `kill -STOP <pid>`, wait 60 s, `kill -CONT <pid>`; at "0:45 in", kill the forwarder, wait 30 s, start it again. Expected:
    - the strip shows "reconnecting", then connected;
    - gaps rise and return to 0;
    - Zoom and the Python tool are unaffected.
  - **V11**, "0:55 in", receiver removal: unplug the receiver for 30 s, then plug it back. Expected: "Zoom goes on, and the gap is marked"; the transcript continues from Zoom; "Input back". (Without mixed: skip, and do V15 at the desk.)
  - **V12**, "1:05 in", permission denial: System Settings → Privacy & Security → Screen & System Audio Recording → turn Terminal off for about a minute, then on again; answer "Later", never "Quit & Reopen". Expected: the strip says Screen Recording is off, with Open Settings; the transcript and notes go on; afterwards the strip says whether capture resumed or needs the app reopened (either is recorded).
  - **V04** (r2), "1:10 in": Zoom full screen, another app in front for five minutes.
  - **V13**, "1:20 in", disk full: `dd if=/dev/zero of=/Volumes/LLLecture/filler bs=1m`. Expected: the session stops, naming a path on `/Volumes/LLLecture`, and the last snapshot is attempted. Then `rm /Volumes/LLLecture/filler`, and Start again in the same folder.
  - **V14**, "1:35 in", forced restart: `kill -9 $(pgrep -f target/debug/desktop)`, relaunch the app (same command), Start in the same folder. Expected: notices "Repaired" and "Recovered"; the gap count shows the crash's gaps and falls to 0.
  - **V03** (r1, unchanged), "any time a slide is shared".
  - **After class** (step):
    - Stop, then Study page;
    - `./target/debug/lecturelive lecture audit --dir "/Volumes/LLLecture/…"` (expected: `0 unexplained, 0 waiting`);
    - note the Study page's typesetting time (§14.2);
    - read `m6-checks`'s files and the notices for empty snapshot answers;
    - copy the lecture folder to the usual lecture place if wanted, then detach the image;
    - keep `m6-fixtures/zoom-live-<date>` for the follow-up session.
- [ ] **Step 3: Revise the other items** by the page's rules (bump `data-rev` when the instructions change; never renumber or reuse an id):
  - V01, V02 and V04 at r2, pointing to the M6 occasion;
  - the "At your next Zoom lecture" occasion's `why` says it is folded into the M6 lecture;
  - V06 at r2 (the new views: the microphone line, the offer, the mixed choice);
  - V09 at r2, with Task 12's two commands and the expected `cmp` result;
  - new desk items: **V15** (the fallback offer with the receiver as the only input: unplug it, choose MacBook Pro Microphone, "Record from it") and **V16** (the microphone fix-it: turn Terminal's microphone off, "Later", open the app, see the line and Open Settings, turn it on, Refresh).

  D1 and D2 keep their ids, with the sitting's answers written in their text if given. Update "Last updated".
- [ ] **Step 4: Check the page** at 1168 px and at 390 px, light and dark, in Playwright, loading it by its `file://` path (a local file, no host). Delete the screenshots.
- [ ] **Step 5: Commit**

```bash
git add docs/VERIFICATION.html
git commit -m "Add the two-hour lecture with its injected failures to the checks page, and revise the items it folds in"
```

---

### Task 14: The spec, as built

**Files:**
- Modify: `docs/spec.md` §4.2, §8 (the sidecar's `slides` fields), §10, §11, §13, §14.3

- [ ] **Step 1: Write each section** as the current design, with no history trails:
  - §4.2: two stages, the host clock as the timeline, joining behind silence by capture time, steering with its limits, resync, the recording's life and gaps in mixed mode, the meter following the loopback, and "Mixed mode ships once it has passed the two-hour drift test" with the numbers' home (M6 Findings).
  - §8: `slides [{index, file, shown_at, auto, uncertain}]`.
  - §10: the "Device disappears" row names the fallback the person chooses; the "Microphone or Screen Recording denied" row names the fix-its; a "Disk write error" row "Session stops cleanly with the path; the last snapshot is attempted".
  - §11: the audit and the fault runs under Manual.
  - §13: the Keychain through `security-framework` (not `keyring`), and rubato's `Async` polynomial resampler for the drift stage.
  - §14.3: the detector thresholds as calibrated at M5 (change 0.05, settle 0.03, animated 3, expiry 10), with the live Zoom measurement left to the lecture.
- [ ] **Step 2: Commit**

```bash
git add docs/spec.md
git commit -m "Bring the spec to mixed mode, the error table and the slide fields as built"
```

---

### Task 15: The one sitting

- [ ] **Step 1:** When every task above except the review is done, ask the person the two decisions, with their defaults, in one question set (D1: Polish after the end; D2: one production build for the CSP). Record the answers in the ledger and on `docs/VERIFICATION.html` (D1, D2 text). Act on a changed answer only while this session runs:
  - D1 yes: allow Polish when no lecture runs, test-first, as its own small task;
  - D2 yes: one `npm run tauri build`, then run the built app's CSP check. Never re-sign or move the canary.

---

### Task 16: Review, findings and handover

**Files:**
- Modify: `docs/milestones.md` (M6 section and Status row), `docs/VERIFICATION.html` (Last updated)

- [ ] **Step 1: Run the suites and record their counts:** `cargo test -p lecturelive-core --no-fail-fast`, `cargo test -p desktop`, `cd apps/desktop && npx vitest run`, `npx svelte-check`, `cargo test -p lecturelive-cli`. Paste any failure verbatim, and name a flake only after it has passed alone.
- [ ] **Step 2: Final review.** A fresh reviewer on `opus`, over `main..m6-full-lecture`, given this plan's Review Focus verbatim and the ledger's Ruling lines. Re-grade its findings by their effect on the person. Fix Critical and Important findings test-first in one pass.
- [ ] **Step 3: Findings** in the M6 section:
  - the resolved versions of any new crates and packages;
  - mixed mode as built, with the drift test's numbers, and whether it ships enabled;
  - the §10 table, with each row's evidence (a test name, a synthetic run's report path, or a VERIFICATION id);
  - the lecture checks found done from files (none), and the lines ticked (none);
  - every M6 open thread, taken or left with its reason ("left for good": no milestone follows);
  - the `/frontend-design:frontend-design` runs, one per view;
  - live spend;
  - rulings;
  - the review;
  - failed lines with their §14.1 pointer.

  Each gate line is held, failed, not run or deferred, with evidence. Status: "done" only if every gate line holds; else "in progress", with the plan linked in the Status row.
- [ ] **Step 4: Commit the findings;** then the repository's CLAUDE.md end-of-milestone steps:
  1. scan every added line of `origin/main..m6-full-lecture` (skip lock files) for credentials and personal identifiers, classifying each hit;
  2. redact a real value only after asking;
  3. `git checkout main && git merge --ff-only m6-full-lecture`;
  4. `git push origin main`;
  5. `git branch -d m6-full-lecture`.

  With the lecture not yet held, `live_notes.py` stays on `main`.

---

### Task 17: Retire the Python CLI (milestone task 4; only after the two-hour lecture's gate lines hold)

Run by the follow-up session. **Precondition:** the M6 lecture occasion's evidence is recorded, and gate lines 1 and 2 hold ("zero unexplained missing or duplicate audio intervals", "every §10 row observed"). Without that, stop: the Python CLI is the person's in-class tool until the Rust app has passed a real lecture.

**Files:**
- Delete: `live_notes.py`, `pyproject.toml`
- Modify: `crates/core/src/notes/prompts.rs` (delete `the_frozen_excerpt_is_live_notes_py_verbatim`), `README.md`

- [ ] **Step 1:** `git rm live_notes.py pyproject.toml`. Delete the verbatim test, whose subject is gone; the excerpt fixture stays. `.venv/` is not tracked; leave it on disk for the person.
- [ ] **Step 2:** Rewrite `README.md` around the app and the Rust CLI:
  - what LectureLive does;
  - setup (BlackHole, "LectureLive Loopback", Zoom's speaker, the Keychain key);
  - the app's run (`npm run dev` plus `./target/debug/desktop` until a packaged app exists);
  - the CLI's `lecture`, `lecture page`, `lecture spend` and `lecture audit`;
  - where files go (spec §8).

  Keep the page's copy plain, and write paths as `$HOME/…`.
- [ ] **Step 3:** Run the suites: `cargo test -p lecturelive-core --no-fail-fast`, `cargo test -p desktop`, `npx vitest run`.
- [ ] **Step 4: Commit**

```bash
git add README.md crates/core/src/notes/prompts.rs
git commit -m "Retire the Python CLI now that the app has passed a real two-hour lecture; the README describes the app and the Rust CLI"
```

Then set the M6 gate line "`live_notes.py` removed" and the Status row, and follow the CLAUDE.md merge steps.
