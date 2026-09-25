# LectureLive milestones

Build order for the design in [`spec.md`](spec.md). Each milestone ends in a gate that
is checked on the real machine; a milestone is complete only when every line of its
gate holds. Each milestone gets its own step-by-step plan when it begins, because the
previous milestone's findings (routing behaviour, crate versions, protocol details)
change it. Plans live in `docs/superpowers/plans/`.

Any task that creates or changes UI — the desktop app's views, the study page template
(`notes_template.html`), or any other page a person looks at — invokes
`/frontend-design:frontend-design` before any markup is written (spec §9.4). Every plan and
kickoff prompt carries this rule.

## Status

| M | Name | Status | Depends on | Destroys anything | Plan |
|---|------|--------|-----------|-------------------|------|
| M0 | Contract fixtures + packaged native canary | done | — | Changes the system default output (restored by the canary) | [m0-native-canary](superpowers/plans/2026-09-22-m0-native-canary.md) |
| M1 | Recording + loopback foundation | in progress | M0 | Creates the "LectureLive Loopback" device; changes the system output only in the canary-route restore check, which restores it | [m1-recording-loopback](superpowers/plans/2026-09-25-m1-recording-loopback.md) |
| M2 | Streaming + recovery | not started | M1 | No | written at M2 start |
| M3 | Notes/session parity | not started | M2 | Migrates lecture folders to the v2 sidecar (one-way for the Python CLI) | written at M3 start |
| M4 | Desktop app: transcript + notes panes | not started | M3 | No | written at M4 start |
| M5 | Slide automation | not started | M4 | No | written at M5 start |
| M6 | Full-lecture acceptance; mixed mode; Python retired | not started | M5 | Removes `live_notes.py` | written at M6 start |

The Python CLI (`live_notes.py`) stays the in-class tool until M6 passes.

## M0 — Contract fixtures + packaged native canary

**Why first:** everything later assumes that a packaged app on this Mac can get
microphone and Screen Recording permission, route Zoom audio into BlackHole and back,
write a recording that survives a crash, and capture a window, and that the STT
protocol behaves as §5 assumes. If any of that fails, the design changes (spec §14.1)
before any UI exists.

Spec: §4.1 permissions, §4.3 routing, §4.4 recording, §5.1 protocol fixture, §7.1
window capture, §13 versions.

Tasks (detailed in the M0 plan):

1. Workspace + `core::audio::recorder` (checkpointed WAV, header repair)
2. `core::audio::routing` (persisted route state, restore rule, CoreAudio multi-output)
3. `core::audio::input` (device listing, downmix, resample to 16 kHz, 100 ms framer, timed recording)
4. `core::capture::window` (enumerate, capture, blank-frame rejection, ≤1600 px)
5. `core::stt::probe` (websocket probe that logs every server message; fake-server test)
6. `cli` canary subcommands
7. Packaged Tauri canary app (permissions, entitlements, buttons for each check)
8. Acceptance run on the real machine; findings recorded below

**Gate** (checked without a live lecture: the session runs every check it can alone, and
a person gives the two permission grants and one Zoom click in a single sitting of about
three minutes, with no meeting; plan Task 8):

- [x] Packaged `.app` obtains microphone permission and records from BlackHole
- [x] Routing: signal from system audio present on BlackHole while routed; default output restored on stop
- [x] Zoom's own output (Settings → Audio → Test Speaker) audible and present on BlackHole, with Zoom's speaker setting recorded
- [x] Route restore offered and working after `kill -9` of the app while routed
- [x] WAV recorded by the app is playable up to the last one-second checkpoint after `kill -9` (after `canary repair`)
- [x] Packaged `.app` obtains Screen Recording permission and saves correct images of a known window and a Zoom window
- [x] STT protocol fixtures recorded for both finalize spellings; accepted spelling, timestamp origin and `speech_final` behaviour written below
- [x] Crate versions that build together recorded below

**At the first Zoom lecture after M0** (holds before M1's gate is checked; that gate needs a
Zoom lecture anyway):

- [ ] The packaged `.app` captures the Zoom meeting window with a shared slide, and the image shows the slide

**Findings** (acceptance run 2026-09-24; evidence in `~/Library/Application Support/LectureLive/canary/checks.log`
and the plan ledger; recordings and images stay in that folder, outside the repository):

*Environment.* macOS 27.0 (26A428), Apple Silicon; Xcode 27.0 beta (27A5218g); `rustc 1.96.1`
(pinned in `rust-toolchain.toml`); BlackHole 2ch installed (UID `BlackHole2ch_UID`, 48 kHz, 2 ch).

*Crate versions that build together* (`Cargo.lock`): `cpal 0.18.2`, `rubato 5.0.0`, `hound 3.5.1`,
`xcap 0.9.8`, `image 0.25.10`, `tokio 1.53.1`, `tokio-tungstenite 0.30.0`, `rustls 0.23.45` (feature
`ring`), `coreaudio-sys 0.2.18`, `core-foundation 0.10.1`, `serde_json 1.0.151`, `chrono 0.4.45`,
`anyhow 1.0.104`, `clap 4.6.7`, `dirs 7.0.0`, `dotenvy 0.15.7`, `tauri 2.11.6`, `tauri-build 2.6.3`.
Frontend: `svelte 5.57.1`, `@tauri-apps/api 2.11.1`, `@tauri-apps/cli 2.11.5`, `@sveltejs/kit 2.70.3`
(the Tauri template's page is `src/routes/+page.svelte`), `vite 8.3.1`. Differences from the plan's
lines: `rubato` is a new major line (`FftFixedIn` is gone; `Fft` with `FixedSync::Input` and
`audioadapter` buffers replaces it); `cpal 0.18` names devices through `description()?.name()` and
`SampleRate` is a plain `u32`; `tokio-tungstenite 0.30` brings `rustls` with no crypto provider, so
`ring` is enabled explicitly (without it every `wss://` connect panics). Build: with `lto = true`,
`strip = true` on host-side proc-macro dylibs makes rustc fail to load them (E0463,
`ctor_proc_macro`) on this toolchain, so `[profile.release.build-override] strip = false`.

*STT protocol* (fixtures `crates/core/tests/fixtures/stt/finalize_{json,text}.jsonl`, synthesised
`speech.wav`, 8.6 s):
- Accepted finalize spelling: `{"type":"finalize"}`. It produced `is_final: true, speech_final: true`
  244 ms after it was sent. The bare text `finalize` is answered with
  `{"type":"error","message":"Invalid message: expected ident at line 1 column 2"}`; the
  connection stays open and nothing is finalized.
- End of audio must also be JSON: `{"type":"audio.done"}`. The bare text `audio.done` is answered
  with an `error` (`expected value at line 1 column 1`), the connection stays open and
  `transcript.done` never comes.
- Timestamp origin: `start`, `duration` and word `start`/`end` are seconds of audio from the first
  frame sent on that connection, not wall-clock time (the segment after a finalize at 3.0 s of
  audio starts at `start: 3.0`; the last word ends at 8.374 of 8.615 s). A new connection starts
  again at 0.
- `speech_final` behaviour: every final arrives as a pair at the same instant: an event with
  `is_final: true, speech_final: false`, then an identical one (same text and words) with
  `speech_final: true`. The second marks the utterance boundary and adds no text. Interim events
  (`is_final: false`) arrive about once a second with `words: []`; words come only on finals.
  Without a finalize, 8.6 s of continuous speech produced no chunk-final before `audio.done`.
- `transcript.done` does not repeat the text: `{"text":"","words":[],"duration":8.615}`. It comes
  after the flushed final pair. The client owns the whole transcript.

*Routing and permissions.*
- The default output on this Mac is the user-made Multi-Output "Macbook + Notes"
  (`~:AMS2_StackedOutput:1`), which already contains BlackHole. Routed from there, §4.3's aggregate
  nests one aggregate inside another, and a BlackHole signal cannot show that the route did
  anything. The routing checks were therefore run from "MacBook Pro Speakers"
  (`BuiltInSpeakerDevice`) as the default, and "Macbook + Notes" was restored afterwards.
- `route on/off` from the CLI and from the packaged app: `abandoned: true` while routed,
  `restored: true`, and the noted default came back each time. Negative control with speech
  playing on the speakers and the route off: BlackHole at −120 dBFS. With the route on: −38.1 dBFS,
  20.0 s, 0 dropped callbacks.
- `AudioHardwareDestroyAggregateDevice` returns before the device disappears (its UID resolved for
  about 13 ms more); `destroy_aggregate` waits for it.
- Microphone: the packaged, ad-hoc-signed app (`adhoc,runtime`, entitlement
  `com.apple.security.device.audio-input`) raised the prompt; after Allow it records BlackHole.
- Screen Recording: `xcap` only preflights the permission, and without it silently leaves out other
  apps' windows. The app was absent from Screen & System Audio Recording until it called
  `CGRequestScreenCaptureAccess`; the person then allowed it, and listing returned other apps'
  windows with titles. Without the grant, `windows` now fails with a distinct error instead of an
  empty list.
- Window capture (`CGWindowListCreateImage` through `xcap`) still works on macOS 27: TextEdit
  README.md 1312×844, text correct; Zoom 1600×865, showing Zoom's window (login screen, Zoom
  7.1.5). The Zoom image has a blank strip about 180 px wide on its right edge where Zoom's side
  panel is clipped, probably the window running past the screen edge. Not diagnosed; relevant to
  §7.1 region selection at M5.
- Crash: a 120 s app recording killed with `kill -9` 31.6 s after its logged start read 31.0 s before
  repair (the last checkpoint) and 31.512 s after `canary repair`; `afplay` plays it.
  `route status` after the kill reported `abandoned: true`; `route off` restored the output. The
  restore offer at launch (§4.3 step 5) is M1 work; M0 shows the detection and the restore.

*Zoom and §4.3 compared with README.md.* Zoom's Speaker was pinned by name to "MacBook Pro
Speakers". Three Test Speaker runs with the default output on the speakers:

| Zoom Speaker | App route | BlackHole | Heard |
|---|---|---|---|
| MacBook Pro Speakers (as found) | on | −120.0 dBFS | not asked |
| Same as System (§4.3) | on | −41.6 dBFS | yes |
| Macbook + Notes (README) | off | −40.4 dBFS | yes |

Both arrangements carry Zoom when Zoom is set up for them, and neither carries it when Zoom is
pinned to a physical device: the setting found would have left the notes without Zoom audio.
§4.3 depends on Zoom following the system output and routes every app's sound into the
transcript. The README arrangement depends on Zoom being pinned to the Multi-Output device,
keeps other sounds out, and needs no change to the system output. Here, though, the system output
was itself "Macbook + Notes", so other sounds were reaching BlackHole anyway. Zoom was left on
"Macbook + Notes". Which arrangement M1 builds is decided when M1's plan is written.

*Failed lines:* none. No spec §14.1 fallback is indicated by M0.

## M1 — Recording + loopback foundation

Spec: §3.2–3.4 coordinator and frames, §4.1–4.4.

Tasks:

1. Frame timing: `Frame { recording_id, sample_offset, valid_samples }`, recording anchors persisted in the sidecar (§3.3)
2. Lock-free capture path: callback writes into a preallocated SPSC ring (`rtrb`); overflow counted as a recording gap (§3.4)
3. Device identity by CoreAudio UID; property listeners for disappearance and sample-rate change; stream rebuild with new timing segment (§4.1)
4. Session coordinator skeleton owning recorder and route state; bounded channels to workers (§3.2)
5. Loopback device: app-owned Multi-Output with BlackHole as clock, preflight and silence warning, abandoned canary route undone at launch (§4.3)
6. Recorder retention rule and header repair on launch (§4.4)
7. CLI `record` command on the coordinator

**Gate** (checked without a live lecture where the mechanism allows; plan Task 8):

- [ ] 16/44.1/48 kHz inputs framed correctly (unit)
- [ ] Interrupted WAV repaired on launch
- [ ] `kill -9` during a loopback recording leaves the system output and "LectureLive Loopback" unchanged, and a canary route left behind is undone at the next launch
- [ ] Loopback preflight passes with Zoom's Test Speaker played through "LectureLive Loopback"
- [ ] Unplugging the wireless receiver produces a marked gap and no automatic source switch

**At the first Zoom lecture after M1** (nothing else waits on it):

- [ ] A 30-minute Zoom recording with no unexplained captured-audio gaps
- [ ] M0's deferred check (the line under "At the first Zoom lecture after M0" above): the packaged `.app` captures the Zoom meeting window with a shared slide, and the image shows the slide

## M2 — Streaming + recovery

Spec: §5.

Tasks:

1. `SttStream` with a single ordered writer, epochs, keyterm validation (§5.1)
2. Transcript state machine: stable chunks, tentative tail, utterance-final replacement, duplicate-final suppression (§5.2)
3. Segment log (`<stem>.segments.jsonl`) and transcript file append (§8)
4. Snapshot cutoff: finalize after the cutoff frame, correlation, timeout leaves material pending (§5.3)
5. Live-first reconnect, gap computation, REST recovery over recorded intervals (§5.4)
6. Stop sequence: drain, `audio.done`, finals, finalize recording (§5.4)
7. Fake STT server replaying M0 fixtures with injected disconnects

**Gate:** exact outputs on protocol fixtures; disconnects of 3, 15, 45 and 300 s with no
duplicate or missing committed intervals; REST recovery exercised; 4xx stops STT without
a reconnect loop while recording continues.

## M3 — Notes/session parity

Spec: §6, §8.

Tasks:

1. Prompts carried over from `live_notes.py` with golden tests
2. Batch builder and timeline (tie rule, slide-boundary splitting) (§6.1)
3. Context budget and revision-keyed prefix summary (§6.1)
4. SSE client with strict success criteria (§6.2)
5. Embed validation and repair (§6.2)
6. Commit journal and launch recovery with fault injection at every step (§6.2)
7. Polish with abort, backup, atomic replace (§6.3)
8. Sidecar v2, folder lock, initialisation table, legacy migration; Python CLI exits on v2 folders (§8)
9. CLI `lecture` command: full session (record, stream, snapshot, polish) headless, with `page` and `spend` as in the Python CLI
10. Spend ledger: one line per paid request, billed or computed, CLI format; takes over the CLI's ledger (§8)
11. Study page: one distillation request over the whole notes and slides within a word budget, revision on overshoot, section re-cut, fragment stripping, page cache keyed to prompt and notes, single-pass template fill with `notes_template.html` embedded (§6.4)

**Gate:** golden prompts and file formats (including the spend ledger) match `live_notes.py`; empty and truncated SSE
leave the document untouched; fault injection between every commit step recovers
correctly; a legacy folder migrates; the Rust CLI runs a full lecture headless; a
fixture lecture distils to a complete study page within its budget, and an unchanged one re-renders from
its cached parts without requests.

## M4 — Desktop app: transcript + notes panes

Spec: §3.6, §9. Visual design produced with `/frontend-design:frontend-design` at the
start of this milestone, before any pane is built; tasks 3–5 and 7 invoke it again for
their own views.

Tasks:

1. Tauri adapter: coordinator handle, events for status, `Channel`s for transcript and notes streams, `get_session_state()`
2. Svelte rune store: awaited listener setup, hydration, sequence checks, bounded preview
3. Transcript pane: word IDs, fade-in, collapse of closed utterances, pin-to-bottom
4. Notes pane: frozen committed render, throttled preview parse, DOMPurify, scoped slide assets, CSP
5. Control bar: source picker with meter and route status, start/stop, hint + snapshot, polish, open study page, status with this lecture's spend
6. Keychain storage of the API key
7. Spend view from the ledger (§9.1)

**Gate:** reload and hidden-window rehydration restore the full view; hint, cancel and
polish behave as in the CLI; 500-delta/s burst and two-hour transcript fixtures keep p95
frame work under 16.7 ms; rendered Markdown cannot run script or load remote content.

## M5 — Slide automation

Spec: §7. The region picker and the slides strip are UI: both invoke
`/frontend-design:frontend-design` before any markup is written.

Tasks:

1. Window descriptor revalidation and region picker (§7.1)
2. Tile detector with candidate/confirm/animated-mask/expiry (§7.2), calibrated on recorded Zoom frames
3. Shared registration path for auto, manual, shortcut and drag-and-drop (§7.3)
4. Slides strip with badges; notes embeds

**Gate:** on recorded Zoom fixtures, ≥95% recall of annotated stable states visible ≥3 s
and ≤1 false capture per 10 minutes; occlusion, minimisation and window replacement
handled without silent rebinding.

## M6 — Full-lecture acceptance; mixed mode; Python retired

Spec: §4.2, §10, §11.

Tasks:

1. Mixed mode: timestamp-aligned FIFOs with adaptive resampling; two-hour drift test
2. Error table (§10) verified item by item
3. Two-hour real lecture with forced restart, receiver removal, permission denial, disk-full and network loss
4. Remove `live_notes.py` and `pyproject.toml` (`notes_template.html` stays; core embeds it); README switches to the app and Rust CLI

**Gate:** zero unexplained missing or duplicate audio intervals over the two-hour run;
every §10 row observed; mixed mode passes the drift test or ships disabled.
