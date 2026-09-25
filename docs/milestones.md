# LectureLive milestones

Build order for the design in [`spec.md`](spec.md). Each milestone ends in a gate that
is checked on the real machine; a milestone is complete only when every line of its
gate holds. Each milestone gets its own step-by-step plan when it begins, because the
previous milestone's findings (routing behaviour, crate versions, protocol details)
change it. Plans live in `docs/superpowers/plans/`.

## Status

| M | Name | Status | Depends on | Destroys anything | Plan |
|---|------|--------|-----------|-------------------|------|
| M0 | Contract fixtures + packaged native canary | not started | — | Changes the system default output (restored by the canary) | [m0-native-canary](superpowers/plans/2026-09-22-m0-native-canary.md) |
| M1 | Recording + loopback foundation | not started | M0 | Same as M0 | written at M1 start |
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

- [ ] Packaged `.app` obtains microphone permission and records from BlackHole
- [ ] Routing: signal from system audio present on BlackHole while routed; default output restored on stop
- [ ] Zoom's own output (Settings → Audio → Test Speaker) audible and present on BlackHole, with Zoom's speaker setting recorded
- [ ] Route restore offered and working after `kill -9` of the app while routed
- [ ] WAV recorded by the app is playable up to the last one-second checkpoint after `kill -9` (after `canary repair`)
- [ ] Packaged `.app` obtains Screen Recording permission and saves correct images of a known window and a Zoom window
- [ ] STT protocol fixtures recorded for both finalize spellings; accepted spelling, timestamp origin and `speech_final` behaviour written below
- [ ] Crate versions that build together recorded below

**At the first Zoom lecture after M0** (holds before M1's gate is checked; that gate needs a
Zoom lecture anyway):

- [ ] The packaged `.app` captures the Zoom meeting window with a shared slide, and the image shows the slide

**Findings:** _(filled in by M0 task 8)_

## M1 — Recording + loopback foundation

Spec: §3.2–3.4 coordinator and frames, §4.1–4.4.

Tasks:

1. Frame timing: `Frame { recording_id, sample_offset, valid_samples }`, recording anchors persisted in the sidecar (§3.3)
2. Lock-free capture path: callback writes into a preallocated SPSC ring (`rtrb`); overflow counted as a recording gap (§3.4)
3. Device identity by CoreAudio UID; property listeners for disappearance and sample-rate change; stream rebuild with new timing segment (§4.1)
4. Session coordinator skeleton owning recorder and route state; bounded channels to workers (§3.2)
5. Routing: preflight flow, conditional restore, abandoned-route detection on launch (§4.3)
6. Recorder retention rule and header repair on launch (§4.4)
7. CLI `record` command on the coordinator

**Gate:** 16/44.1/48 kHz inputs framed correctly (unit); interrupted WAV repaired on
launch; route restored after `kill -9`; a 30-minute Zoom recording with no unexplained
captured-audio gaps; unplugging the wireless receiver produces a marked gap and no
automatic source switch.

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

Spec: §3.6, §9. Visual design produced with the frontend-design skill at the start of
this milestone.

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

Spec: §7.

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
