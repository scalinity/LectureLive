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
| M1 | Recording + loopback foundation | done | M0 | Creates the "LectureLive Loopback" device; changes the system output only in the canary-route restore check, which restores it | [m1-recording-loopback](superpowers/plans/2026-09-25-m1-recording-loopback.md) |
| M2 | Streaming + recovery | done | M1 | No | [m2-streaming-recovery](superpowers/plans/2026-09-25-m2-streaming-recovery.md) |
| M3 | Notes/session parity | done | M2 | Migrates lecture folders to the v2 sidecar (one-way for the Python CLI) | [m3-notes-session-parity](superpowers/plans/2026-09-25-m3-notes-session-parity.md) |
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

- [x] 16/44.1/48 kHz inputs framed correctly (unit)
- [x] Interrupted WAV repaired on launch
- [x] `kill -9` during a loopback recording leaves the system output and "LectureLive Loopback" unchanged, and a canary route left behind is undone at the next launch
- [x] Loopback preflight passes with Zoom's Test Speaker played through "LectureLive Loopback"
- [x] Unplugging the wireless receiver produces a marked gap and no automatic source switch

**At the first Zoom lecture after M1** (nothing else waits on it):

- [ ] A 30-minute Zoom recording with no unexplained captured-audio gaps
- [ ] M0's deferred check (the line under "At the first Zoom lecture after M0" above): the packaged `.app` captures the Zoom meeting window with a shared slide, and the image shows the slide

Commands for that lecture (Zoom's Speaker on "LectureLive Loopback", system output on the plain
speakers or headphones):

```bash
L=$HOME/Documents/Tools/LectureLive/target/debug/lecturelive
$L loopback check --secs 15          # while Zoom plays; pass above −60 dBFS
$L record --loopback --secs 1800 --dir "$HOME/Library/Application Support/LectureLive/m1-zoom"
cat "$HOME/Library/Application Support/LectureLive/m1-zoom/.live_notes/lecture_notes_$(date +%Y%m%d).v2.json"   # "gaps": []
APP="$HOME/Documents/Tools/LectureLive/target/release/bundle/macos/LectureLive Canary.app"
open -W -n "$APP" --args --check windows; tail -n 40 "$HOME/Library/Application Support/LectureLive/canary/checks.log"
open -W -n "$APP" --args --check capture <ID of the zoom.us meeting window>
```

**Findings** (acceptance run 2026-09-25; recordings and check files in
`~/Library/Application Support/LectureLive/`, outside the repository):

*Routing decision.* The README arrangement, built by the app: "LectureLive Loopback"
(`com.lecturelive.loopback`), a stacked Multi-Output with BlackHole as clock and one physical
output with drift correction, never the system default (spec §4.3). Chosen over spec §4.3's
default-output aggregate because both carried Zoom in M0 (−41.6 and −40.4 dBFS), and this
one changes nothing system-wide, so a crash leaves nothing to undo and other apps stay out of
the transcript.

*New crates* (`Cargo.lock`): `rtrb 0.4.0`, `uuid 1.26.1` (v4, serde), `objc2-av-foundation 0.3.2`
(no default features; `std`, `AVCaptureDevice`, `AVMediaFormat`); `chrono 0.4.45` gains `serde`.
Workspace `rust-version` is 1.89 (`std::fs::File::try_lock` for the folder lock). cpal 0.18
already installs `kAudioDevicePropertyDeviceIsAlive` and `kAudioDevicePropertyNominalSampleRate`
listeners on every input stream and reports them as `DeviceNotAvailable` and
`StreamInvalidated`; `Device::id()` is the CoreAudio UID. rubato 5's `Fft::output_delay()` is the
exact delay to trim (an impulse at 1 s lands within ±2 samples of 16,000 at 16, 44.1 and 48 kHz).

*Loopback.* Setup from a speaker default created the device and left the system output
unchanged. A tone through the device read −53.1 dBFS on BlackHole; the same tone straight to the
speakers read −120.0 (negative control). The device adds no loss: a tone read −47.0 dBFS both
straight into BlackHole and through the device. The ~24 dB below nominal was BlackHole's own
output volume (about 60% in Audio MIDI Setup).
Zoom's Test Speaker through the device, first run: pass at −40.5 dBFS, but barely audible and the
volume keys did nothing — the system output was the user-made Multi-Output "Macbook + Notes",
which macOS gives no volume control (`output volume: missing value`), with its speaker member
at about 10%. With the system output on MacBook Pro Speakers and BlackHole's volume at maximum:
pass at −15.7 to −24.7 dBFS, the level following the ringtone regardless of listening volume,
and the volume keys changed Zoom's loudness. Spec §4.3 step 3 now says so.

*Crash and repair.* `record --loopback` killed with `kill -9` 20 s in: system output and the
device unchanged; the next launch repaired the WAV to 19.8 s, marked an `interrupted` gap and
`afplay` played it. A canary route left as the system default was undone at the next launch
(`restored: true`). After review, launch repair and retention cover every sidecar in the folder,
not only the current day's.

*Receiver.* DJI "Wireless Mic Rx", UID `AppleUSBAudioEngine:DJI Technology Co.,
Ltd.:Wireless Mic Rx:XSP12345678B:3` (48 kHz, 2 ch; the UID carries the serial number). Unplugged
20.1 s in: the recording closed, a `device_gone` gap was marked, no other input was opened;
replugged 21 s later: a new recording on the same UID. After review, a device that is listed but
cannot open yet (after a replug or a rate change) is waited for instead of ending the session.

*Soak* (evidence for the deferred 30-minute line, not a substitute): `record --loopback` for
1,800 s with a tone straight into BlackHole: one recording of 1,800.0 s, 0 gaps, 0 stream errors.

*Failed lines:* none. No spec §14.1 fallback is indicated by M1.

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

**Gate** (checked in `cargo test` without network, plus live checks with synthesised speech; plan Task 13):

- [x] Exact outputs on protocol fixtures
- [x] Disconnects of 3, 15, 45 and 300 s with no duplicate or missing committed intervals
- [x] REST recovery exercised
- [x] 4xx stops STT without a reconnect loop while recording continues

**Findings** (acceptance run 2026-09-25; recordings and logs of the live runs in
`~/Library/Application Support/LectureLive/m2-*`, outside the repository):

*Suite.* `cargo test -p lecturelive-core`: 132 passed, 0 failed, 7 ignored (M1's four hardware
tests and three live tests). The live tests, run by hand (`--test stt_live -- --ignored`), pass
against the real endpoint.

*New crates* (`Cargo.lock`): `reqwest 0.13.5` (no default features; `multipart`, `json`,
`rustls-no-provider`), bringing `hyper-rustls 0.27.10` and `rustls-platform-verifier 0.7.1`
(roots from the macOS keychain). reqwest 0.13 renamed its TLS features: `rustls` pulls
`aws-lc-rs`, and beside the `ring` feature that leaves rustls with no default provider. With
`rustls-no-provider`, reqwest panics at `Client::build` unless a provider is installed first,
so `RestClient::new` installs ring (`cargo tree -i aws-lc-rs`: no match). Unchanged:
`tokio-tungstenite 0.30.0` / `tungstenite 0.30.0`, `rustls 0.23.45` (ring), `tokio 1.53.1`,
`serde_json 1.0.151`, `hound 3.5.1`.

*Protocol recorded while planning* (new fixtures `endpoint_pauses`, `finalize_silence`,
`finalize_between_pair`, `silence_after_speech`; spec §5 now states it):
- The server endpoints by itself. A chunk-final comes about 0.5 s after speech stops, then a
  `speech_final` about 0.5 s later, with the same text and words and a duration that runs on
  through the endpoint silence. Utterances do not tile the audio: one closed at 3.08 s, the
  next began at 3.72 s.
- Every finalize is answered with a `speech_final` that ends exactly at the cutoff, within
  40–250 ms. With nothing open it is an empty one of duration 0.
- While audio flows the server sends an interim every second. In silence it also sends an
  empty chunk-final every two seconds, which closes nothing (checked over 12 s). This is what
  makes the 5 s idle watchdog sound.
- Refusals come at the websocket upgrade: a bad key is 400 with a JSON body, an unknown model
  404, a malformed parameter 400 with plain text. Every refusal body is sent chunked.
- Unpaced audio is accepted (10.5 s returned the same finals within 0.7 s). `endpointing=400`
  is accepted but changes nothing measurable. Keyterms apply on the websocket (live
  "cross-entropy" where M0 heard "cross entropy"), but REST was not seen to apply them.
- REST answers `{text, language, duration, words}`, with word times from the clip's start.
  Silence has no `words` key, and there is no cost field.

*What the fake server encodes* (`crates/core/tests/support/`):
- Replay releases each recorded server message once the client has sent as many frames and
  text messages as it had when that message arrived.
- Synthetic speech puts k + 1 in every sample of frame k and places words at fixed positions.
  The server closes an utterance 0.2 s after its last word, as a chunk-final plus an extended
  `speech_final`. It sends an interim every tenth frame and answers a finalize at the position
  it has heard (empty when nothing is open). On `audio.done` it flushes, then sends
  `transcript.done` with empty text and the total duration. Bare text gets serde's error.
- Injected faults: a connection that vanishes without a close frame, or one that stays open
  and goes silent; refusals by HTTP status (with the real body) or by TCP close; a drop on
  finalize; silence after `audio.done`.
- The fake REST endpoint answers each clip with the words that start in it.

*Disconnect suite* (`stt_gate`). Each run is a whole session: a paced source, the recorder, the
real STT worker and the real recovery worker. The outage is measured in audio, dropped at 30 s
mid-utterance. Five consecutive runs were green, 10.4 s per suite run.

| Outage | Refused with | Result |
|---|---|---|
| 3 s | TCP close | Every word committed once. The 5 s hold streams the outage late; recovery covers the unclosed utterance |
| 15 s | HTTP 503 | Every word committed once. The gap spans the outage less the 5 s hold, and is recovered |
| 45 s | TCP close | As 15 s |
| 300 s | HTTP 503 | As 15 s; about 295 s recovered in pieces of at most 30 s |

In every run, no segment's interval overlaps another's, every transcript gap is resolved, and
the transcript has one line per segment.

*REST recovery evidence.*
- Fake endpoint: the pieces tile the gap, recovery resumes after the pieces already logged and
  waits for the recorder, and a refusal ends recovery for the session.
- Gate: every disconnect run recovered through REST.
- Live: a 5.615 s interval came back with 13 word times.
- Live crash: `kill -9` 11 s into a continuous sentence left the recording's open-utterance mark
  in the sidecar. The next `--stt` launch repaired the WAV to 13.3 s, turned the mark into an
  `stt_interrupted` gap `[0, 212096)`, and recovered it as one line marked `(recovered)`.

*"Resolved", per gap kind.* `capture_overflow`, `recorder_overflow`, `device_gone`,
`rate_change` and `interrupted` are resolved when recorded: no audio exists behind them to
recover. `stt_offline`, `stt_overflow`, `stt_refused` and `stt_interrupted` are resolved when
recovery has committed their whole interval. Retention therefore keeps a recording exactly
while it holds a transcript gap not yet recovered. `launch::tests::open_recording_is_repaired_and_its_tail_marked_interrupted`
now expects `resolved: true`.

*Live runs.* Synthesised speech went into BlackHole with `say -a`, which changes no output: the
default output was `BuiltInSpeakerDevice` before and after.
- Four sentences became four lines, one per sentence, each timed by its first word; no gaps;
  a 30.0 s WAV.
- The crash and recovery described above.
- A bad key: one refusal, no reconnect lines, recording continued (4.95 s), recovery was
  refused once, and the `stt_refused` gap was left unresolved.

*Rulings during execution:*
- The live endpoint's refusal body is chunked, and tungstenite hands the raw tail over (the
  live test showed `400 Bad Request: 8a`). `refusal_message` now reads through the framing.
- The endpoint-fixture gate test failed one run in two with single-frame recorder-overflow
  gaps while passing alone every time: eight parallel sessions syncing every 10 frames at 1 ms
  per frame. The fixture source now paces 3 ms per frame, with its assertions unchanged.
- In `record`, the `secs` parameter became `secs_limit`, because the new `secs()` helper would
  have been shadowed.

*Review* (a fresh reviewer on the most capable model, over the whole branch): no Critical, one
Important, eight Minor.
- **The Important finding.** A crash in a recording whose STT never connected (a refused key,
  or offline from the start) left no open-utterance mark, so its audio got no transcript gap
  and retention could later delete it. Fixed test-first with one mark per recording
  (`open_utterances`), set when a recording begins with transcription expected and moved by
  each connection. A single mark would not do: after a rate change the next recording begins
  while the last is still being flushed, and the earlier recording's tail would be lost.
- **One Minor checked live first.** Its trigger was that the endpoint might refuse very short
  clips; exact 10 ms and 50 ms clips are accepted with empty text, so it stays Minor.

*Open threads.*
- Deferred review minors:
  - A cutoff taken between a recording's end and its final flush answers `confirmed`, while the
    last sentence still arrives into the next batch. Owner: M3, snapshot semantics, which also
    decides whether a snapshot waits for pending recovery.
  - Any REST 4xx ends recovery for the session; only a key or authorisation refusal should.
  - The reconnect backoff resets on every handshake.
  - Recovery retries a failing piece without limit while the session runs.
  - A failed transcript write can leave the segment log and the transcript diverging.
  - A second Ctrl-C does not cut short a stop that is draining recovery.
  - The refusal line prints a double period.

  Owner of these seven: M3's `lecture` command and M6's error table (§10), whichever reaches
  them first.
- Sidecars written before M2 (only the M1 test folders) keep `resolved: false` on audio gaps,
  so retention never prunes those recordings.
- REST can mishear a word at the very start of a clip: "losses" came back as "This", and
  "Backpropagation" as "That propagation". Gaps start where an utterance closed, so the first
  word of the recovered interval sits on the clip's edge. M6 measures this over a real
  lecture; starting each request a moment before the gap and keeping only the words inside it
  is the likely remedy.
- A transcript gap left in another day's sidecar is recovered only by a session with that
  day's stem. Owner: M3, folder initialisation.
- The transcript file does not yet have the Python CLI's `--- started/resumed HH:MM:SS ---`
  lines. Owner: M3, golden file formats.
- Speech-to-text spend is not yet written to the spend ledger ($0.20 per hour streamed and
  $0.10 per hour recovered, both computed). Owner: M3 task 10.
- A crash between the segment log's sync and the transcript's sync leaves a logged segment
  without its transcript line (the safer order: never a line whose segment is lost). Owner:
  M3, rebuild from files.

*Failed lines:* none. No spec §14.1 fallback applies (§14.1 covers loopback only).

## M3 — Notes/session parity

Spec: §6, §8.

Tasks:

1. Prompts carried over from `live_notes.py` with golden tests
2. Batch builder and timeline (tie rule, slide-boundary splitting) (§6.1)
3. Context budget with a local outline of the omitted prefix (§6.1)
4. SSE client with strict success criteria (§6.2)
5. Embed validation and repair (§6.2)
6. Commit journal and launch recovery with fault injection at every step (§6.2)
7. Polish with abort, backup, atomic replace (§6.3)
8. Sidecar v2, folder lock, initialisation table, legacy migration; Python CLI exits on v2 folders (§8)
9. CLI `lecture` command: full session (record, stream, snapshot, polish) headless, with `page` and `spend` as in the Python CLI
10. Spend ledger: one line per paid request, billed or computed, CLI format; takes over the CLI's ledger (§8)
11. Study page: one distillation request over the whole notes and slides within a word budget, revision on overshoot, section re-cut, fragment stripping, page cache keyed to prompt and notes, single-pass template fill with `notes_template.html` embedded (§6.4)

**Gate** (checked in `cargo test` against fakes, plus live checks on synthetic lectures; plan Task 15):

- [x] Golden prompts and file formats (including the spend ledger) match `live_notes.py`
- [x] Empty and truncated SSE leave the document untouched
- [x] Fault injection between every commit step recovers correctly
- [x] A legacy folder migrates (and the Python CLI refuses a v2 folder)
- [x] The Rust CLI runs a full lecture headless
- [x] A fixture lecture distils to a complete study page within its budget, and an unchanged one re-renders from its cached parts without requests

**Findings** (acceptance run 2026-09-25; live runs in `~/Library/Application Support/LectureLive/m3-*`, outside the repository):

*Suite.* `cargo test -p lecturelive-core`: 229 passed, 0 failed, 9 ignored (M1's four hardware tests, M2's three live tests, M3's two live tests). The gate files are `lecture_gate` (5 tests, five consecutive runs green, about 2.2 s), `notes_chat` (10), `notes_page` (2), and the `notesfile`, `folder`, `spend`, `timeline`, `embeds`, `page`, `prompts` and `pyjson` unit tests.

*New crates* (`Cargo.lock`): `regex 1.13.1`, `sha2 0.11.0`, `base64 0.23.1`; `reqwest 0.13.5` gains `stream`; `serde_json 1.0.151` gains `preserve_order`, so Python-format JSON keeps insertion order. Map equality stays order-insensitive, and every M0–M2 test passed unchanged with it.

*SSE protocol recorded while planning* (`crates/core/tests/fixtures/notes/*.sse`; spec §6.2 now states it; about $0.005):
- A streamed chat response reports its cost only when asked with `stream_options: {"include_usage": true}`, and then only once: a chunk with `choices: []` and `usage.cost_in_usd_ticks` after the finish chunk, then `data: [DONE]`. Without the option, no cost appears at all.
- Reasoning deltas (`delta.reasoning_content`) come before content deltas; the first carries `role`.
- A `max_tokens` cut ends with `finish_reason: "length"` and is still billed.
- Refusals are HTTP 400 before any stream, with the STT endpoint's `{"code","error"}` body.
- `grok-4.7` reports a 500,000-token context and a 200,000-token `long_context_threshold`, above which every token is billed at twice the rate. The §6.1 budget is that threshold, so §14.2 did not have to be measured to set it.

*What the fake SSE endpoint encodes* (`crates/core/tests/support/fake_sse.rs`): replays of the recorded streams in pieces of any size (7 bytes up to 1 KB); synthetic answers in api.x.ai's order (reasoning, word-sized content deltas, finish, usage chunk, `[DONE]`); a stall after the headers; refusals by status with the real body. It records every request body.

*Golden comparisons.*
- **Prompts.** The four system prompts are compared with `live_notes.py`'s f-strings evaluated from its source by the test, for two courses (one non-ASCII) and several budgets and slide counts. The user messages have hand goldens, and every fixed fragment of them is checked to appear in the source.
- **Spend ledger.** Byte-identical to the CLI's real ledger line (read with `od -c`): Python `json.dumps` defaults, `—` for the em dash, `repr` floats, `usd` rounded to 6 decimals and `audio_s` to 1. Live check: the CLI ledger's one line, imported into the app ledger by take-over, compares byte-identical with `cmp`. The spend view is golden-compared with `show_spend`'s layout.
- **Other formats.** Transcript lines `[HH:MM:SS] text` and `--- started|resumed HH:MM:SS ---`, the notes block `\n<!-- HH:MM:SS -->\n…\n`, the title line, and the timeline lines (`>>> Slide N shown (embed: …)`) are compared with the formats `live_notes.py` writes in code. The study page cache is the CLI's own file (`{"source", "fills", "words", "budget"}`, keyed to the SHA-256 of prompt and notes), so either tool re-renders the other's cache.
- **Not done.** No fresh Python CLI run was made to byte-compare notes and transcript output on the same fixtures. Those formats are compared with the code that writes them.

*Fault injection per commit step* (`notesfile::tests::a_crash_at_every_commit_step_recovers_to_the_block_exactly_once`). Each case crashes at one point, reloads the sidecar from disk, recovers, and checks that the block lands exactly once:

| Crash point | Recovery | Outcome |
|---|---|---|
| Journal's temp file only | Nothing | Material pending, then committed once |
| After the journal | Not appended | Committed once on retry |
| Mid-append (20 bytes written) | Truncated | Committed once on retry |
| After the append | Completed | Cursors advanced |
| After the cursors | Completed | Idempotent, no second advance |
| After the clear | Nothing | — |

Recovery stops and changes nothing in three further cases: the notes no longer begin with the recorded "before"; they grew past the block; or (after review) the tail is text typed by hand rather than the block's start. After review, a commit that finds an earlier commit's journal in the same session repairs it first, and refuses a stale batch when that earlier block had completed.

*Legacy migration* (`folder::tests`, `lecture_gate::a_legacy_folder_migrates…`), on synthetic folders in the Python CLI's exact formats. No real lecture folder was touched.
- `transcript_offset` and the older `noted_through` checkpoints both migrate; lines before the checkpoint count as noted.
- The CLI's half-written snapshot (a `commit` entry) is finished or undone as `recover_commit` does, in its own words.
- Imported lines roll to the next day past midnight.
- Notes without state, and a corrupt sidecar with `--rebuild`, are rebuilt from the last `<!-- -->` marker; the corrupt file is kept beside.
- The next snapshot sends exactly the pending legacy lines.
- `live_notes.py page` in a folder holding `.live_notes/*.v2.json` prints "This folder is kept by the LectureLive app now (.live_notes/*.v2.json). Run `lecturelive lecture` here instead." and exits 1. In a plain folder it behaves as before and touches nothing.

*The headless lecture.*
- **Fakes** (`lecture_gate::a_whole_lecture_runs_headless_from_first_word_to_study_page`): speech into the fake STT, a slide dropped into `slides/`, a snapshot, a hinted snapshot, polish and page, then stop. The notes are polished with the embed exactly once, the backup holds the raw snapshots, the last snapshot takes every logged segment, no journal is left, the ledger has transcribe, notes, polish and page lines, and the hint reached the request.
- **Live**, `lecturelive lecture --loopback --secs 600`, with `say -a "BlackHole 2ch"` into a synthetic folder and scripted input:
  - The notes were created and the dropped slide was registered as `slide_01_185408.png` and placed.
  - Snapshots cost $0.02 (41 words, 1 slide), $0.01 (hinted, 34 words) and $0.01 (the one polish takes first, 16 words); the polish cost $0.01, with its backup written.
  - The page was `Optimisation.html`, 530 of 600 words, $0.09. The last snapshot found nothing new, and the command exited 0.
  - The sidecar ended with cursor 6 for 6 segments, slide index 1, 0 gaps and no journal. The default output was "MacBook Pro Speakers" before and after.
  - The ten-second silence warning fired after the scripted speech ended, as it should.

*Study page.*
- **Fake:** the 2,006-word fixture lecture (budget 600) drew a first draft half again over budget, then one revision without images, ending within budget and complete. It re-rendered from the cache with no request, and after a template change still with no request; changed notes typeset again.
- **Live:** 570 of 600 words, all parts present, one request with no revision, $0.2727. The re-render was cached and the ledger unchanged.
- The live page request took about 530 s at medium effort for 2,000 words of notes and three slides.
- The template is embedded unchanged. No visual change was made, so `/frontend-design:frontend-design` was not needed.

*Live spend:* about $0.44 of the $2 cap. Research about $0.005; the live snapshot check $0.003432; the live page $0.2727; the headless lecture $0.162046 (notes $0.028986, polish $0.011396 and page $0.088336, all billed; transcribe $0.033328 computed for 599.9 s).

*Settled while planning* (the plan's header has the reasons):
- A snapshot does not wait for pending recovery.
- A cutoff while a recording's transcript is still flushing is not confirmed (M2's deferred minor).
- The prefix summary is an outline derived locally, with no request and no cache.
- The ledger lives in the app's data directory with incremental take-over.
- Streamed speech-to-text is one computed line per recording; recovery is one per REST piece.
- Notes without state are rebuilt from the last marker.
- Other days' transcript gaps are recovered at launch.
- A transcript line lost between the two syncs is put back.

*M2 deferred minors taken:*
- The cutoff between a recording's end and its flush.
- A second Ctrl-C stops a draining stop, in the coordinator and the CLI.
- The refusal's double period, in `record` and `lecture`.

Not taken, because their code was not touched: any REST 4xx ends recovery; the backoff resets on each handshake; recovery retries a failing piece without limit.

*Rulings during execution:*
- `cargo test` takes one filter before `--`.
- The Write tool decodes `\uXXXX` escapes, so files containing them are written with quoted heredocs.
- A raw string in the plan's chat test needed `r###`.
- The page fixture is named `fixture_lecture.md`, because `lecture_notes_*.md` is git-ignored to keep real notes out of this public repository.
- The fixture has 2,006 words (budget 600).
- The fake answers in 1 KB pieces.
- The plan's store test waited for samples the recorder had not yet synced; it now waits for the sidecar to list the recording.
- **Durable store updates.** The plan's `Store::update` answered before the coordinator had saved. It now answers only after the save, caught by its own test.

*Review* (a fresh reviewer on the most capable model, over the whole branch): no Critical, four Important, twelve Minor. All four Important were fixed test-first:
1. Journal recovery could delete notes typed by hand after a crash. The journal now keeps the block, and a tail is truncated only when it is the block's start. Spec §6.2 was amended.
2. A commit that failed mid-session left a fragment the next commit built on. A commit now repairs an existing journal first.
3. There was no way out while stopping, and a second stop sent while the audio was ending was swallowed. The coordinator now counts stops in both phases, a second stop also skips queued operations, and a third Ctrl-C quits at once.
4. A deleted slide failed every later snapshot. It is now skipped with a warning, and the cursor moves past it.

*Failed lines:* none. No spec §14.1 fallback applies (§14.1 covers loopback only).

*Open threads.*
- **Owner M4:**
  - Deferred review minors: other days' gap recovery runs before today's recording without a progress line; ledger warnings from polish and page are not shown; removing an inline embed from `- ![Slide N](…)` leaves a lone `-`.
  - `coordinator::tests::a_stuck_stt_worker_loses_frames_not_control_messages` flakes about 1 run in 10 under parallel load. It was reproduced at the M2 tip, so it predates M3. Pacing its source, as M2 did for the fixture source, would fix it with its assertions unchanged.
- **Owner M6:**
  - Deferred review minors: relaunching after midnight starts the next day's files; migration and rebuild date slide files and markers after midnight on the start day; `course` is derived from a relative `--dir` before it is made absolute; the session marker can glue onto a torn transcript line; the last snapshot is skipped when the session ends in failure; the page retries a billed `length` or empty answer once; `lecture page` runs without journal recovery; an `"error": null` key reads as an error.
  - A Python CLI already running in a folder when the Rust CLI starts there would share it, because the Python CLI takes no lock. The v2 check covers only a Python CLI started later.
  - The M2 minors listed above as not taken.
  - §14.2's latency (a page took about nine minutes for 2,000 words) is measured over a real lecture.
  - Before `live_notes.py` is removed, its golden tests (which read its source) are frozen into fixtures.
- **Optional, needing the person:** a Python CLI run on a synthetic folder, to byte-compare its notes and transcript output with the Rust CLI's (no step of M3 depends on it).

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
