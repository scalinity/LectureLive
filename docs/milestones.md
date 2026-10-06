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
| M4 | Desktop app: transcript + notes panes | done | M3 | No | [m4-desktop-panes](superpowers/plans/2026-09-25-m4-desktop-panes.md) |
| M5 | Slide automation | done | M4 | No | [m5-slide-automation](superpowers/plans/2026-09-25-m5-slide-automation.md) |
| M6 | Full-lecture acceptance; mixed mode; Python retired | implementation accepted as M7's baseline; the two-hour real lecture (gate lines 1–2) is deferred to a class | M5 | Removes `live_notes.py` | [m6-full-lecture](superpowers/plans/2026-09-26-m6-full-lecture.md) |
| M7 | Ratatui CLI | done | M6 (implementation baseline) | No | [m7-ratatui-cli](superpowers/plans/2026-09-26-m7-ratatui-cli.md) |
| M7.1 | A live lecture's findings: pause, quit, a restarted Zoom window, repeated slides | built and tested; live checks V17–V20 wait for the person | M7 | No | — |
| M8 | Automatic Zoom capture | not yet planned; depends on M7's TUI and capture architecture | M7 | No | — |

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

**Gate** (checked in `cargo test` and `vitest` against fakes and fixtures, in the running dev app's WKWebView, and live on synthetic lectures; plan Tasks 10, 11 and 13):

- [x] Reload and hidden-window rehydration restore the full view
- [x] Hint, cancel and polish behave as in the CLI
- [x] 500-delta/s burst and two-hour transcript fixtures keep p95 frame work under 16.7 ms
- [x] Rendered Markdown cannot run script or load remote content

**Findings** (acceptance run 2026-09-25; reports in `~/Library/Application Support/LectureLive/m4-checks/`, synthetic lectures in `~/Library/Application Support/LectureLive/m4-live/`, outside the repository):

*Suites.* `cargo test -p lecturelive-core`: 233 passed, 0 failed, 9 ignored (M3's 229 plus four: the cancel-and-state gate test, `course_from_path`, `prepare`, `spend::summary`). `cargo test -p desktop`: 9 passed, 0 failed, 1 ignored (the Keychain round trip, run by hand against the login Keychain: passed, no prompt). `npx vitest run`: 21 passed in 5 files. `npx svelte-check`: 0 errors, 0 warnings. The `coordinator` test that flaked about once in ten at M3 did not fail in any M4 run.

*New crates and packages.*
- Rust: `tauri-plugin-dialog 2.7.3` (which brings `tauri-plugin-fs 2.5.2`, `tauri-plugin 2.6.3` and `rfd 0.16.0`); `tauri 2.11.6` gains `protocol-asset` (`http-range 0.1.5`). The Keychain is reached through `security-framework 3.7.0`, already compiled into the build through `rustls-platform-verifier`. `keyring` (spec §13) is at a new major line, 4.2.0, built on `keyring-core` and per-platform store crates, so it was not added. The desktop crate gains `dotenvy`, `anyhow`, `uuid` and `tokio` (`sync`, `time`), all already in `Cargo.lock`.
- Frontend: `marked 18.0.14`, `dompurify 3.4.15` (pinned exactly, as spec §13 names it; 3.4.16 is out), `@tauri-apps/plugin-dialog 2.7.3`; dev `vitest 5.0.2`, `jsdom 30.1.1`. Unchanged: `svelte 5.57.1`, `@sveltejs/kit 2.70.3`, `vite 8.3.1`, `@tauri-apps/api 2.11.1`.

*The adapter as built* (spec §3.6 now states it).
- One lecture per window. At most one `lecture::run` runs, on Tauri's runtime; a folder can be open without one.
- A pure `Pump` maps each lecture event to one message on one of three streams:
  - the `status` event: the whole status, sent whenever it changes; notices; slides;
  - a transcript `Channel`: `open`, `closed`, `segment`;
  - a notes `Channel`: `delta`, `committed`, `ended`, `polished`. Deltas and their result share an `op` and the channel, so their order holds.
- Every message carries the session id and one `seq` counter, assigned under the pump's lock.
- Core added:
  - `Command::Cancel`: generation-tagged; it stops the request in flight and drops the queue; only the request is raced, never the commit;
  - `Command::State`: served through the session's store until stopping, then from the file;
  - revisions on `Committed` and `Polished`, and `Event::Cancelled`;
  - `session::start::prepare`, the CLI's start-up, now shared.
- `get_session_state` holds the pump's lock, so its `seq` is exact. It reads:
  - the sidecar: through `Command::State` with a 2 s limit, or from the file once stopping (a finding of the final review);
  - the segment log;
  - the notes, until their SHA-256 is the sidecar's.
- The frontend drops messages from another session, at or below the watermark, or repeated on their stream. A status message from a new session makes it rehydrate, and so do a revision jump, `polished`, or a hole in segment ids.
- Stop has three levels in the UI: Stop, then Stop waiting, then quitting the app. Cancel covers snapshot and polish, not the page. Once stopping, the command line takes nothing new, and the backend says so (spec §9.1).

*Frame work* (plan Task 10). Measured per animation frame as apply + `flushSync` + the panes' layout reads, in the dev app's WKWebView (AppleWebKit/605.1.15, 60 Hz), driven through the real store by fixtures. Reports: `m4-checks/bench-*-run{1,2,3}.json`.

| Fixture | p95 (ms), runs 1/2/3 | Max (ms) | Frames | Over 16.7 ms |
|---|---|---|---|---|
| 5,000 notes deltas at 500/s over 10 s, speech every 500 ms | 6 / 4 / 4 | 9 / 6 / 10 | 600 each | 0 |
| 1,440 segments and a 6,671-word document hydrated, then 30 s live (speech, 900 deltas at 60/s, one commit) | 4 / 2 / 2 | 11 / 4 / 6 | 913–928 | 0 |

- Hydrating the two-hour state took 39–40 ms.
- WebKit coarsens `performance.now()` to 1 ms, so the WKWebView figures have 1 ms resolution.
- For comparison, Chromium 153 at 120 Hz gave p95 4.7 ms (burst) and 2.2 ms (two-hour).
- Defining the measurement exposed a real ordering bug: the pin-to-bottom hooks ran before Svelte flushed the frame. The drain now flushes first.

*Sanitiser and CSP.*
- **Unit** (`markdown.test.ts`, jsdom), with 14 hostile inputs: script, handler, `javascript:`, remote and `data:` images, iframe/object/embed, SVG and MathML, a stylesheet and `@import`, a style attribute, meta refresh and `<base>`, a form with `formaction`, video and `poster`, `srcset`, a path climbing out of `slides/`, and `ping` and `ontoggle`. The render was inert: no remote `src` or `href`, and no image. Only a registered slide renders, through `asset://`.
- **The URL pass is load-bearing.** With it removed, the test failed on `IMG src https://evil.example/a.png`: DOMPurify alone keeps remote images.
- **In the running app** (WKWebView, `m4-checks/csp.json`):
  - violations: `img-src` on `http://192.0.2.1/m4.png`, `connect-src` on `http://192.0.2.1/m4`, and `script-src-attr` on an inline handler written without the sanitiser. The handler did not run.
  - The hostile Markdown rendered with no problems and no images.
  - The registered slide loaded through the asset protocol; `outside.png`, one folder up, was refused.
- **In development the CSP comes from SvelteKit's `kit.csp`.** On macOS, Tauri loads the dev URL directly and never applies its own `csp` there. `curl -sI localhost:1420` showed `script-src 'self' 'nonce-…'` with no `unsafe-inline`.
- **A built app's CSP is not checked:** no build was authorised. That is an open thread.

*Rehydration and the CLI's operations* (plan Task 11, `m4-checks/live-run{1..5}.json`). These are live runs of the dev app on synthetic folders, with speech from `say -a "BlackHole 2ch"` and a PNG dropped into `slides/` mid-lecture.

Run 4 (Week 03) passed every step, and so did run 5 (Week 04), after the review fixes:
1. key from `.env` into the Keychain;
2. start;
3. three segments;
4. hinted snapshot (revision 2);
5. cancel after the first delta: revision unchanged, "Cancelled: the snapshot; nothing was written…";
6. snapshot after the cancel (revision 3);
7. **reload**: 8 (run 5: 16) segments before, after, in the state and in the pane; the revision equal;
8. **hidden window**: `visibilitychange` fired on hide and show (4 s apart), and afterwards the pane equalled a fresh state (15 = 15, run 5: 23 = 23 segments; the revision equal);
9. polish: revision + 2 (its snapshot, then the replace), backup `.live_notes/…_2040NN.md`;
10. Stop, then Stop waiting (`stopping_now`);
11. ended with a last snapshot.

The sidecar ended with cursor 30 of 30 segments, and the dropped slide was embedded exactly once. The default output was "MacBook Pro Speakers" before and after every run.

Runs 1–3 are evidence too:
- Run 1 exposed a check that waited on material the model had already written.
- Run 2 caught a race in the check.
- Run 3 was invalid: a previous `say` loop was still playing into BlackHole.

In the gate test (`lecture_gate::a_cancelled_snapshot_writes_nothing_drops_the_queue_and_its_material_goes_again`), Cancel stops a stalled request, the queued polish never sends, the notes are unchanged, and the next snapshot sends the cancelled material. The Ops are the CLI's (`Snapshot(hint)`, `Polish`, `Stop`); `polish ⏎` in the command line is the CLI's `polish ⏎`.

*The study page from the app.* Typeset in 213 s, $0.104546 billed (`m4-checks/page.json`). A check never opens the default browser (the page loads KaTeX and fonts from the web), so the open itself is left for a person to see.

*`/frontend-design:frontend-design` runs*, recorded in the plan and the ledger:
- **Milestone:** the study page's palette, Atkinson Hyperlegible Next falling back to the system face, 18 px at a 1.2 ratio, flat ruled columns, the command line as the one bold element. Pass 2 dropped a monospace face for data labels and card panes.
- **Transcript pane and layout (Task 6):** a hanging time gutter; the live utterance one step larger on a teal rule; no pane headings. Pass 2 dropped a pulsing live dot.
- **Notes pane (Task 7):** snapshot times in the same gutter; the preview on the teal rule, marked "writing"; slides fill the measure.
- **Control bar (Task 8):** the phase as one word with a static recording dot; the command line in the CLI's grammar and marks; the native `<dialog>` key.
- **Spend view (Task 9):** a sheet with the CLI's layout, dates in the gutter. Pass 2 dropped a big-total hero.

Each view was checked in Chromium through Playwright on the dev server, in light and dark, with large type and at 960 px. The checks found and fixed a strip that wrapped, a layout-driven unpin, and the notes pane not following the preview. The screenshots were deleted. The app itself (WKWebView) has not yet been looked at by a person.

*Live spend:* $0.5429 of the $2 cap, all in `spend.jsonl` under course "m4-live":
- notes $0.2900 (billed), polish $0.1013 (billed), page $0.1045 (billed), transcribe $0.0471 (computed);
- two of the notes requests ($0.009 and $0.012) were billed empty answers, described below.

*Settled while planning* (the plan's header has the reasons):
- one lecture per window;
- the three streams and one `seq`;
- the revision rule and hash-matched document;
- Cancel as a new command;
- the stop levels;
- state from the file once stopping;
- start-up shared through `prepare`;
- the Keychain through `security-framework`;
- fonts local-or-system;
- frame work measured in WKWebView.

*M3 deferred minors taken:* other days' recovery now announces each day before it runs (CLI and app); polish's ledger warning reaches the notices. Not taken, because their code was not touched: the page's ledger warning, the lone `-` after an inline embed. The coordinator flake did not reappear.

*Rulings during execution* (the ledger has all of them with their costs):
- The cancel gate test waits for the stalled request to reach the fake server.
- The pin hooks run after `flushSync`.
- A status message from a new session makes the store rehydrate.
- The store buffers everything until its first hydration.
- `screenshot_dir` moved into core.
- `dompurify` pinned exactly.
- A check never opens the default browser.
- The checks wait for status-driven state rather than reading it right after a command.

*Review* (a fresh reviewer on the most capable model, over the whole branch): no Critical, three Important, eleven Minor.

The three Important findings, and one Minor re-graded to Important for its effect, were fixed test-first:
1. **State reads hung through the final snapshot.** The lecture no longer reads its commands then, and the pump stayed locked for minutes. Once stopping, the file answers, and a running lecture has 2 s.
2. **A committed revision the state already held did not end its preview,** so the last block showed twice.
3. **A closed segment the state already held left the live line showing** (re-graded from Minor).
4. **Snapshot, a hint and Polish were offered while stopping, and core dropped them.** Now they are only offered while running, and the backend refuses with "The lecture is stopping; the last snapshot takes what is left."

The ten remaining Minors are deferred; their owners are listed under open threads.

*Failed lines:* none. No spec §14.1 fallback applies (§14.1 covers loopback only).

*Observations.*
- `grok-4.7` answered two snapshots with empty content, both billed, when the new material repeated what the notes already held: the synthetic speech looped one passage. The material stayed pending, as spec §6.2 requires.
- Stopping aborts the study page that polish starts, as in the CLI.
- After a relink of the debug binary, one start took 92 s. That is consistent with a Keychain access prompt for the new signature being answered on screen; the executor did not observe it.

*Open threads.*
- **Owner M5:**
  - The slides strip replaces the plain list in the right-hand rail. It carries the rail's "Screenshots you take during the lecture become slides" state and takes its thumbnails through the same asset scope.
  - Deferred review minor M8: a new slide re-renders the whole committed document, because `slides.size` is in every chunk's key. The strip's work touches that code.
- **Owner M6:**
  - The built app's CSP: SvelteKit's meta tag together with Tauri's header (unverified, since no build was authorised).
  - Spec §13 names `keyring`, but the app uses `security-framework`.
  - Whether a real lecture draws empty snapshot answers.
  - Keychain prompts after each dev rebuild.
  - Polish after a lecture has ended: a decision, since the CLI also polishes only inside a lecture.
  - A dropped `SourceEnded` (predates M4).
  - Deferred review minors:
    - M1: the hash retries have no pause;
    - M3: the page shares the busy line with snapshot and polish;
    - M4: a late `SourceEnded` re-shows "Stop waiting";
    - M5: a start that fails late stays in "starting";
    - M6: "Stop" is offered while starting;
    - M7: a hydration begun before hiding is reused on return;
    - M9: the gap count starts at zero each session;
    - M10: the CLI's start-up line order;
    - M11: unhandled `hydrate()` rejections.
- **Needing the person, one sitting of a few minutes:**
  - look at the app in the dev build, light and dark and with large type;
  - click Study page once, to see it open in the browser;
  - answer "Always Allow" if the Keychain asks.

  No gate line depends on this.

## M5 — Slide automation

Spec: §7. The region picker and the slides strip are UI: both invoke
`/frontend-design:frontend-design` before any markup is written.

Tasks:

1. Window descriptor revalidation and region picker (§7.1)
2. Tile detector with candidate/confirm/animated-mask/expiry (§7.2), calibrated on recorded Zoom frames
3. Shared registration path for auto, manual, shortcut and drag-and-drop (§7.3)
4. Slides strip with badges; notes embeds

**Gate** (checked in `cargo test` against fakes and recorded fixtures, and live in the dev app on windows it opens itself; plan Tasks 5, 10, 12 and the full-screen check):

- [x] On recorded Zoom fixtures, ≥95% recall of annotated stable states visible ≥3 s and ≤1 false capture per 10 minutes
- [x] Occlusion, minimisation and window replacement handled without silent rebinding

**Findings** (acceptance run 2026-09-26; reports in `~/Library/Application Support/LectureLive/m5-checks/`, synthetic lectures in `m5-capture/`, `m5-deck/` and `m5-fullscreen/` beside it, outside the repository):

*Suites.* `cargo test -p lecturelive-core`: 277 passed, 0 failed, 19 ignored (M4's 233 plus the registration, detector, selection, locate and worker tests and both gates; the ignored ones add the live window listing and capture and the fixture tools). `cargo test -p desktop`: 16 passed, 0 failed, 1 ignored (the Keychain round trip). `npx vitest run`: 26 passed in 6 files. `npx svelte-check`: 0 errors, 0 warnings. Two failures under load, never alone: the M1 test `audio::capture::tests::concurrent_overflow_keeps_every_position_exact`, four times in the milestone's full runs (see open threads); and the STT gate's three disconnect tests once, while the dev app was shutting down beside the run (M5 changed no STT or audio code; the gate passed three times alone).

*New crates and packages.*
- Rust: `tauri-plugin-global-shortcut 2.3.2` (with `global-hotkey 0.8.0`), for ⌘⇧2; `objc2 0.6.4` and `objc2-app-kit 0.3.2` (`NSRunningApplication`, `libc`) as direct dependencies at the versions `xcap` already locked, for bundle ids. `image` and `xcap` are optimised in the dev profile: a debug build spent about 0.8 s of every second shrinking a capture, optimised about 21 ms.
- Frontend: none.

*Capture as built* (spec §7 now states it).
- A capture worker runs on its own thread beside the session. It enumerates windows through CoreGraphics on screen or not, and captures the chosen window by its id, so a covered window, or one full screen on another desktop, is still captured.
- A selection is saved per course in `capture.json`: a descriptor (bundle id, title, size), the region as fractions of the window, parts left out as fractions of the region, and the region at each other size the window has had. Older files load unchanged.
- The person chooses a window and drags its slide region over a still taken by id, so a window full screen on another desktop has one too, and drags over any part to leave out, such as a speaker's camera over the slide. Choosing the same window again keeps the parts it has.
- At start exactly one window matching the descriptor binds; anything else asks in the strip. Nothing is watched in a window's place without the person: "Watch it" is offered only for a window the selection matches, and refuses a size with no region; a closed window pauses and offers its replacement; a window that cannot be captured pauses after three failed samples and resumes by itself.
- A change of the window's size by any amount is a re-layout: the region found or chosen at exactly that size is used; otherwise the last kept slide is searched for in the new layout (`capture::locate`) and its place saved as the region for that size; otherwise, within 2% of a known size, that size's region is used; otherwise the strip asks, and the search runs again every 5 samples. A frame through the new region is kept silently only when it shows the kept slide (no tile past twice the change threshold), so a switch never takes the same slide twice and a slide that changed as the window did is taken.
- The detector is spec §7.2's: tiles against the kept frame, a candidate confirmed when nothing unmasked moves, animated tiles masked, an unsettled change kept as uncertain after 10 samples. Parts left out are blanked before the downscale, one pixel wider for its blur.
- Every slide, auto, the Capture button, ⌘⇧2, a dropped image or a ⌘⇧4 screenshot, goes through one registration path, which records auto and uncertain and never registers a file twice.

*Fixtures and the detector's numbers*, at the calibrated thresholds (change 0.05, settle 0.03, animated 3, expiry 10):

| Fixture | States (visible ≥3 s) | Recalled | False captures |
|---|---|---|---|
| Synthetic lecture (generated in the test) | 68 | 68 (100%) | 1 in 28.0 min (0.36 per 10 min) |
| `deck-1`: the synthetic deck played in the app's window, recorded by the live worker (committed, 241 frames) | 53 | 51 (96.2%; two underline builds missed) | 0 in 26.0 min |
| A recorded Zoom lecture played in Chrome, the speaker's camera left out (local only) | 43 | 43 (100%) | 0 in 36.7 min |
| The same, with the camera inside the region as first drawn | 43 | 43 (100%) | 13 in 36.7 min (3.54 per 10 min); at 0.08, 11, every one the camera |

- **Calibration.** At spec 7.2's first value, 0.08, the deck recorded from a window recalled 41 of 53: one line of real text changes its tiles by about 0.06. Across a grid of change, settle and animation values, 0.05 is the highest change threshold at which the deck keeps 95%; 0.06 gives 92.5%. No threshold made the camera-inside region pass (best 1.91 per 10 min, at a cost in recall), so a selection can now leave out a part of the region.
- **The Zoom evidence is a cloud recording's playback**, not a live meeting (no meeting could be hosted): 39.6 minutes recorded record-only (nothing transcribed, registered or sent), played at 2x from 6:43 (confirmed by the lecture's burned-in clock), the player's setup trimmed, and annotated by still stretches with pointer moves merged, checked by eye on contact sheets. Replaying it through the detector at the thresholds it ran with reproduces its 71 live decisions exactly. Its frames stay off the repository and were deleted once measured.
- Rejected with evidence: counting moves over the last N samples (N 4 to 8) cut the camera's false captures to 1.36 per 10 min but recall to 84.8%.

*Live checks in the dev app*, on windows the app opens itself:
- **Capture check** (`m5-checks/capture.json`, run 2, all 15 steps): the first frame; a build; covered by the app's own window, nothing new for 3 s and a change behind the cover captured; minimised, paused with nothing for 3 s while the deck changed, then exactly one slide on return; replaced, the strip asked with the new window as its candidate and nothing was captured for 3 s, then "Watch it" and a slide from the new window; Capture, a manual slide; a dropped image, a manual slide at its file time; the last snapshot embedded each of the seven slides once. Run 1 found that a closed window's id stays listed off screen, so replacement was never offered; fixed test-first.
- **Full-screen check** (`m5-checks/fullscreen.json`, six runs): the deck went full screen on its own desktop, the main window came back in front, and slides changed unseen. Every cut was captured within about 2 s. Run 1 found a size change of 1.25% (1166 × 720 to 1168 × 729) inside the 2% that names the same window, taking the same slide twice, and a one-second pause while full screen animated; run 2 found a re-layout before the first capture swallowing the first slide; all three were fixed test-first, and run 3 was clean. After the review, run 4 was void (macOS did not enter full screen; the step now fails unless the deck is listed off screen); run 5 found that realigned fine text still differed by up to 0.063 in four tiles, taking the slide again on entering full screen (fixed test-first on its frames); run 6 passed all nine steps: a part left out saved and read back, the picker's still of the unseen deck, three cuts unseen, a new slide taken as the window left full screen, and five decisions for exactly the five slides shown.
- A dissolve into an animated slide never painted while the deck was unseen: a web view pauses transitions and animation frames while its window is not visible. Capture reads what the window's app has drawn, so the spec says capture is live for as long as the app keeps drawing.

*Screen Recording for the dev binary.* The preflight probe was true from this shell (Terminal.app is the responsible process); `./target/debug/desktop --check windows` listed other apps' windows with their titles; every live check above captured by window id without a prompt.

*`/frontend-design:frontend-design` runs*, recorded in the plan and the ledger:
- **Milestone:** the strip as a record in time; the teal rule only while watching; badge words in the gutter; a picker where the still dominates. Pass 2 dropped pill badges, a traffic-light dot, a camera icon, middle-dot meta strings and corner handles.
- **Slides strip (Task 8):** time and badge in a hanging gutter, badge words in the transcript's italic mark, flat thumbnails with a 1 px rule, newest last and followed. Pass 2 dropped a "zoom.us, region" line.
- **Window and region picker (Task 9):** a native dialog, list beside the still; the region is the one teal outline with the outside dimmed; "Use the whole window" for a way without a pointer. Pass 2 dropped numbered steps, check-mark icons, corner handles and a dimensions tooltip.
- **Synthetic deck (Task 10):** an ordinary lecture deck on purpose, since realism is its job: white 16:9 slides, system sans, a university blue for titles. Pass 2 replaced a middle-dot footer with a comma.
- **Parts left out (final review):** a part is a hole in what is watched, so it is dimmed as outside the region is, and the teal outline stays the only mark of what is watched; a toggle, one drag per part, and a × on each. Pass 2 rejected a drawing toolbar and red "excluded" boxes.
- **Title bar (at the person's request):** the status strip is the window's top edge, with the traffic lights on its centre line and anything but a button dragging the window. Pass 2 rejected a separate transparent drag bar (it brings the title bar back) and the app's name in the strip (new copy).

Each view was checked in Chromium through Playwright on the dev server, in light and dark, with large type and at its narrow widths, and the strip and title bar in the app's own window too; the checks' fixes are in the ledger. Screenshots were deleted.

*Live spend:* $0.0727 of the $2 cap, all in `spend.jsonl` under course "m5-capture": notes $0.0714 (billed), transcribe $0.0013 (computed). The record-only and full-screen checks sent nothing.

*Rulings during execution* (the ledger has all of them with their costs):
- The detector's settle check covers every unmasked tile, not only the tiles changed against the kept frame: an in-place animated chart gave two false confirms with the narrower reading.
- The Zoom evidence is a cloud recording's playback, recorded record-only; the committed fixture is the synthetic deck recorded from a window.
- The change threshold is 0.05 (above).
- A selection can leave out parts of its region.
- Capture stays on the chosen window through full screen and other desktops, and finds the slide again after a resize (at the person's request, mid-run).
- A check run reads the API key from `.env`, since the dev binary's Keychain read prompts after each relink.
- `capture_watch` and `capture_saved_region` were added for "Watch it" and the picker's saved region.
- `.gitignore`'s root-level `slides/` rule is anchored, so the fixture folder is tracked.
- At a re-layout, "shows the kept slide" allows twice the change threshold: realignment moves fine text by up to 0.063, a new slide by 0.3 and more; a one-line build landing in the same second as a switch can be taken for the kept slide.
- Gate tests that resize the fake window draw its new image first: drawing in a test build outlasts the samples after which the worker looks.

*Review* (a fresh reviewer on the most capable model, over the whole branch at f20dc37): one Critical, four Important, seven Minor and four test gaps. The five Critical and Important findings were fixed test-first, and the test gaps are covered by the new tests:
1. **Parts left out could not be set from the app, and choosing again wiped them** (Critical). The Zoom fixture met the gate only with the camera left out. The picker now draws them, and a re-choose keeps them.
2. **A slide that changed as the window changed size was settled into the kept frame and lost**, which with Zoom going full screen as a share starts is the first slide of each share. Now only a frame showing the kept slide is kept silently.
3. **"Watch it" on a resize question bound the unchecked old region and saved it for the new size.** A resize question offers no window, and "Watch it" refuses a size with no region.
4. **While Zoom was full screen on another desktop, the picker could not show a still** (xcap lists only windows on screen), and a failed search was never retried. The still is taken by id, and the search runs again every 5 samples.
5. **At start, "Watch it" was offered for Zoom's home window**, and clicking it replaced the course's selection. A question offers only windows the selection matches.

*M4 threads taken:* the slides strip replaces the rail's plain list, with its empty state and the same asset scope; M4 minor M8 is fixed, so a new slide re-renders only the notes chunks that hold an image (Task 7).

*Failed lines:* none. No spec §14.1 fallback applies (§14.1 covers loopback only).

*Open threads.*
- **Owner M6:**
  - Whether Zoom's own meeting window keeps drawing while it is full screen on another desktop and the person works elsewhere: capture by id is shown live on a window the app owns, but a web view pauses animations there, and Zoom could not be measured without a meeting. Check it in the first real lecture.
  - The detector's numbers on a live Zoom meeting window rather than a recording's playback.
  - `audio::capture::tests::concurrent_overflow_keeps_every_position_exact` fails under parallel load (three times in this milestone's full runs): a producer that stops while the drop-marker ring is full leaves the tail drop unpublished. M1 code; the gate is zero unexplained missing audio intervals.
  - Spec §8's sidecar slide fields and §14.3 still describe slides as before M5; they were outside what M5 could change.
  - Deferred review minors: M1 the strip still says "Watching" after the lecture ends; M2 the Screen Recording hint says to choose again, where macOS needs the app reopened; M3 a closed window whose id lingers is described as minimised, a replacement opening off screen is never offered, and the offer waits three failed samples; M4 a search trusts any kept frame, possibly the lecture's first frame of speaker video; M5 below 1100 px the strip that holds the questions folds away; M6 ⌘⇧2 during a resize question captures through the old region; M7 the list of sizes grows without limit.
  - Deferred from the title bar: large type at the 960 px minimum clips the strip's clock (68 px of the overflow predates the lights' inset); the traffic lights are placed once at build, centred for normal type, and sit about 7 pt high in large type.
- **Needing the person, one sitting of a few minutes:**
  - drag the window by the status strip, and double-click it to zoom;
  - in a real lecture, choose Zoom's window, leave out the speaker's camera, and try full screen with another app in front.

  No gate line depends on this.

## M6 — Full-lecture acceptance; mixed mode; Python retired

Spec: §4.2, §10, §11.

Tasks:

1. Mixed mode: timestamp-aligned FIFOs with adaptive resampling; two-hour drift test
2. Error table (§10) verified item by item
3. Two-hour real lecture with forced restart, receiver removal, permission denial, disk-full and network loss
4. Remove `live_notes.py` and `pyproject.toml` (`notes_template.html` stays; core embeds it); README switches to the app and Rust CLI

**Gate** (checked in `cargo test` against fakes and simulated clocks, live on synthetic lectures this Mac plays to itself, and in a real two-hour lecture run from `docs/VERIFICATION.html`; plan Tasks 2, 3, 10, 11 and 13):

- [ ] Zero unexplained missing or duplicate audio intervals over the two-hour run. **Not run:** the real lecture waits for a class ("Your two-hour lecture with LectureLive" on `docs/VERIFICATION.html`). Its instrument, `lecturelive lecture audit`, is built, and every synthetic lecture below audits at 0 unexplained, 0 waiting.
- [ ] Every §10 row observed. **Held in tests and synthetic lectures** (the table below); **its live half, in the real lecture, not run** (V10–V14; V15 and V16 at the desk).
- [x] Mixed mode passes the drift test or ships disabled. It passes, and ships enabled.

**Findings** (acceptance run 2026-09-26; reports in `$HOME/Library/Application Support/LectureLive/m6-checks/`, synthetic lectures in `m6-faults/`, `m6-disk/`, `m6-app/` to `m6-app5/`, `m6-app-key/` and `m6-v09/` beside it, outside the repository):

*Suites.* `cargo test -p lecturelive-core --no-fail-fast`: 319 passed, 0 failed, 20 ignored (M5's 277 plus the mixer, mixed source, fallback, audit, network address, disk and ledger tests; the ignored ones add the live mixed run). `cargo test -p desktop`: 23 passed, 0 failed, 1 ignored (the Keychain round trip). `cargo test -p lecturelive-cli`: 1 passed. `npx vitest run`: 32 passed in 6 files. `npx svelte-check`: 0 errors, 0 warnings. Under load, never alone: `stt_gate`'s refusal, 15 s and 45 s disconnect tests failed twice while the two-hour live mixed run and a full suite ran together (an extra recorder-overflow gap, shifted gap starts); alone they passed 8/8 three times each, and M5 recorded the same three once under load. The final run above had nothing beside it.

*New crates and packages:* none. `rubato 5.0.0` (already a dependency) gains its asynchronous resampler, and `lecturelive-core`, `rubato`, `realfft 3.5.0` and `rustfft 6.4.1` are optimised in the dev profile: `rubato`'s generic resamplers compile inside core, and the two-hour simulation took 330 s unoptimised, 60 s at opt-level 1 and 11 s at 3.

*Mixed mode as built* (spec §4.2 now states it):
- The host clock is the timeline, and both sources are steered to it; a device's clock would need a handoff when that device goes, which is §10's "surviving source continues". Each source is resampled to 16 kHz at its nominal rate, then passes a drift stage (`rubato`'s asynchronous septic polynomial, ratio within 0.2% of 1) into a FIFO; the mixer takes exactly what the host clock says is due, 200 ms behind it.
- A source joins with silence in front of it, so its oldest waiting sample lands at its capture time. Its FIFO's level over the first 4 s becomes the target, and a proportional-integral controller steers the ratio to it. More than 100 ms from the target is not drift: a source that ran dry is realigned behind silence, a backlog is dropped to the target, and both are gaps for that source.
- The meter and the silence warning follow the loopback. The recording lasts while either source is there; a source that goes leaves a `device_gone` gap, written open at once and closed when it returns (`SourceEvent::GapEnd`).
- The CLI takes `--mixed <input>` on `record` and `lecture`; the app offers "Zoom and <input> together" in its "Listen to" choice, with a hint to wear headphones.

*The drift test's numbers:*

| Run | Worst offset after settling | Underruns, gaps |
|---|---|---|
| Simulated, 2 h, 48 kHz at +100 ppm (`mix::tests::two_sources_on_drifting_clocks_stay_aligned_for_two_hours`) | 22 samples (1.4 ms); first click +1 sample | 0, 0 |
| Simulated, 2 h, 44.1 kHz at −100 ppm (same test) | 35 samples (2.2 ms); first click −10 | 0, 0 |
| Simulated, 30 min, ±500 ppm (`mix::tests::five_hundred_ppm_either_way_is_held_within_20_ms`) | 136 and 103 samples (8.5 and 6.4 ms) | 0, 0 |
| The same unsteered, for comparison | the fast source 0.90 s late; the slow one ran dry | — |
| Live, 60 s, BlackHole and the built-in microphone | relative 17.5 samples (1.09 ms) | 0, 0 |
| Live, 2 h, the same (`mixed::tests::blackhole_and_the_built_in_microphone_stay_aligned`, `m6-checks/mix-live-7200.txt`) | relative 39.0 samples (2.43 ms); each lane within 21.5 and 18.7 samples of its target | 0, 0 |

- In the two-hour live run the corrections ranged −65 to +55 ppm around means of −0.5 and −3.9 ppm: the steering answers callback timing, not drift. A probe before the plan measured BlackHole at −0.0 ppm and the built-in microphone at +1.9 ppm against the host clock, so the live run tests two hours of real callbacks and scheduling, and the simulation tests drift. The DJI receiver, a truly independent clock, is first measured in the real lecture.
- **Mixed mode ships enabled** (`MIXED_MODE = true`): both simulated tests pass, and the live run had no gap, no underrun, and 2.43 ms against the 10 ms allowed. The live run's binary predates the review's fix pass; that pass changed only a source rejoining while it still drains and a source still dry at the recording's end, neither of which occurs in a clean run.

*Spec §10, row by row.* Evidence is a test against fakes, a synthetic lecture run here, or the real lecture's check id on `docs/VERIFICATION.html`. Network loss comes from a local forwarder (`crates/core/examples/netcut.rs`, `LECTURELIVE_API_ADDR`) that LectureLive alone goes through, so Zoom and the Python CLI keep the network; the disk fills on a 40 MB HFS+ image; a crash is `kill -9`.

| Row | Tests | Synthetic lectures | Real lecture |
|---|---|---|---|
| Network down / websocket error | `stt_gate::a_{3,15,45}_s_disconnect_commits_every_interval_once` | CLI, `m6-faults`: link dropped 60 s ("no message from the server for 5s; reconnecting in 1 s", backoff 2/4/8/16 s), gap 174.4–250.9 s recovered; connections refused 30 s, gap 354.3–386.3 s recovered. App, `m6-app` and `m6-app5`: the strip said "reconnecting", the gap recovered 2.5 s after the link returned | V10 |
| STT 4xx | `stt_gate::a_refusal_stops_stt_without_a_reconnect_loop_while_recording_continues` | App, `m6-app-key`: "refused: 400 Bad Request: Incorrect API key provided…" at once, no reconnect loop, the recording kept, the gap left for the next session | — |
| Notes call fails, truncates or is cancelled | `lecture_gate::failed_empty_and_truncated_answers_leave_the_notes_untouched_and_the_batch_pending`, `lecture_gate::a_cancelled_snapshot_writes_nothing_drops_the_queue_and_its_material_goes_again`, `lecture_gate::a_last_snapshot_that_fails_is_in_the_stop_report` | `m6-app`: one last snapshot did not commit; its 72 lines stayed pending, and the next session in the folder took them all | — |
| Study page part fails after its retry | `notes_page::a_page_that_fails_after_its_retry_writes_nothing` | — | — |
| Crash mid-commit | `notesfile::tests::a_crash_at_every_commit_step_recovers_to_the_block_exactly_once` | `m6-faults`: `kill -9` while a 977-word snapshot streamed: nothing written, 55 lines pending, "resumed with 55 lines" | V14 |
| Microphone or Screen Recording denied | session store "the microphone's state is read for the fix-it"; the fix-it and the Screen Recording strip checked at 1168 px on the fixture | — | V12 (Screen Recording); V16 at the desk (microphone) |
| BlackHole missing | the hint (`FolderPicker.svelte`) and the CLI's refusal "BlackHole 2ch is not installed (brew install blackhole-2ch)"; unchanged since M1 | — | — |
| Loopback preflight fails | `adapter::tests::ten_silent_seconds_on_loopback_raise_the_warning_once`, `adapter::tests::mixed_mode_watches_the_loopback_for_silence`, `mixed::tests::the_level_follows_the_loopback_even_when_the_input_is_loud`; `lecturelive loopback check` (M1) | — | — |
| Device disappears | Mixed: `mixed::tests::a_mixed_recording_carries_both_and_goes_on_when_one_leaves`, `…when_both_leave_the_recording_ends_and_the_next_begins_when_one_returns`, `adapter::tests::in_mixed_mode_a_gone_input_is_a_notice_not_an_offer`. Single: `source::tests::while_a_gone_device_is_waited_for_the_person_may_choose_another_input`, `…a_fallback_that_cannot_be_found_keeps_the_wait_for_the_original`, `…a_fallback_that_cannot_be_opened_keeps_the_wait_for_the_original`, `adapter::tests::a_single_input_that_goes_is_offered_a_fallback_and_one_that_returns_clears_it`, and the store's offer tests; M1's live receiver unplug and replug | — | V11 (mixed); V15 at the desk (single) |
| Sample-rate change | `source::tests::a_failed_rebuild_after_a_rate_change_waits_instead_of_ending_the_session`; mixed: `mix::tests::a_source_that_rejoins_at_once_keeps_what_it_delivered` | — | — |
| Disk write error | `coordinator::tests::unwritable_recordings_dir_stops_with_the_path`, `coordinator::tests::a_sidecar_that_cannot_be_saved_stops_the_session_with_its_path`, `segments::tests::a_write_that_fails_names_its_file`, `lecture_gate::a_failed_session_still_takes_its_last_snapshot` | CLI, `m6-disk`: "session failed  recording to /Volumes/LLDisk/…/session_20260926_050808.wav failed: No space left on device (os error 28)", the last snapshot attempted and kept pending, exit 1 within 8 s; the relaunch repaired the recording (65.7 s) and recovered its gap | V13 |
| Spend ledger write fails | `notes_chat::a_ledger_that_cannot_be_written_is_a_warning_and_the_answer_is_kept`, `coordinator::tests::a_ledger_that_cannot_be_written_is_a_warning_and_the_session_goes_on` | — | — |
| App crash | `recorder::tests::checkpointed_audio_survives_a_crash`, `coordinator::tests::a_crash_before_stt_ever_connected_leaves_the_audio_as_a_gap`, `coordinator::tests::a_crash_while_two_recordings_await_their_transcript_leaves_a_gap_for_each` | CLI, `m6-faults` (repaired at 539.8 s), and app, `m6-app` (at 299.0 s) and `m6-app4`: repaired, resumed, the unclosed utterance recovered as a gap, the system output `BuiltInSpeakerDevice` before and after | V14 |
| External edit of notes | `notesfile::tests::an_external_edit_is_kept_and_the_block_appended_after_it` | — | — |

- The audit (`lecturelive lecture audit --dir …`) checks each recording against its file, the holes and overlaps between recordings against gaps and session markers, and segments for overlaps; transcript gaps not yet recovered count as waiting. It exits 0 when the folder is whole, 1 on anything unexplained, 2 while anything waits. `m6-faults`, `m6-disk`, `m6-app` and `m6-app5`: 0 unexplained, 0 waiting, each crash's hole (1.5 to 5.5 s) explained as interrupted. Read-only on earlier folders: M1's receiver unplug (a 21.0 s hole explained by `device_gone`), M2's crash (16.8 s explained by `interrupted`), and M3's and M4's lectures whole.
- One app run (`m6-app`, after its relaunch) ended its stop in 0.8 s without its last snapshot committing, and the check then kept only the latest notice, so the reason was lost. Nothing was lost with it (above). Four runs did not reproduce it: the app direct, through the forwarder, crash and relaunch, and the whole timeline again with every notice recorded (`m6-app5`: "Snapshot taken: 1350 words", every line taken). The likeliest cause is one failed notes request through the forwarder. The faults check now records every notice, and the end notice now says when the last snapshot failed (review, below).
- **REST's first word** (an M2 thread), over `m6-faults`' seven recovered clips: the first word right 7 times of 7 (Dropout, Second-order, Gradients, Remember, Schedules, Noise, Newton). The mishearings seen were mid-clip ("Hash and vector" for Hessian vector). The lead-in remedy is not taken. The synthetic speech pauses 1.2 s between sentences, so its clip edges fall in silence; real speech pauses less, and the real lecture's recovered lines are the better sample.

*The lecture checks found done from files:* none (no `m1-zoom/`, no `zoom-live-*` fixture, the newest canary image M5's own deck). *Lines ticked:* none. M0's and M1's "at the first Zoom lecture" lines (V03, V01) and M5's live Zoom checks (V02, V04) are inside the two-hour lecture's run sheet.

*`docs/VERIFICATION.html`:* one run sheet, "Your two-hour lecture with LectureLive", replaces the earlier Zoom-lecture occasion: setup (the forwarder, the disk image, the app in mixed mode with the receiver, the Python CLI in its own folder), then in class order V02 and V01 (the first 30 minutes clean), V10 network loss (the forwarder stopped 60 s, later killed 30 s), V11 the receiver unplugged 30 s, V12 Screen Recording off for Terminal for a minute, V04 Zoom full screen, V13 disk full, V14 `kill -9` and a relaunch, V03 the canary's capture, and after class the audit and the Study page. V01, V02, V04, V06 and V09 are revised to r2; V15 (the fallback offer, with the receiver as the only input) and V16 (the microphone fix-it) are desk checks.

*V09, run here:* synthetic notes (671 words, no slides) typeset by `lecturelive lecture page` in 5 min 36 s ($0.20); the Python CLI, in a copy with the same course and folder names and the page removed, reapplied the cached design without a request ("notes unchanged since they were typeset"), and `cmp` found the two study pages byte-identical (38,740 bytes). The golden tests that read `live_notes.py` read a verbatim excerpt of it now (`crates/core/tests/fixtures/prompts/live_notes_excerpt.txt`, from `live_notes.py` at 6f11f84).

*`/frontend-design:frontend-design` runs*, recorded in the plan and the ledger:
- **Milestone:** what the app says when something stands between the person and a recording: what stopped, that nothing switched behind their back, and the one click that fixes it. Pass 2 turned a full-width red banner for the fallback into the notice line, a microphone modal into the command bar's error line, a separate mix toggle into one option per pairing, a filled offer button into an outline one, and a spliced em-dash sentence into two.
- **Microphone fix-it, fallback offer, mixed choice, strip copy (Task 9):** built from the milestone run; pass 2 kept it (the offer's bold device name is the notice line's own label convention). The 1168 px checks moved the headphones hint below the row, capped the select in large type, and narrowed the large-type column minimums to 48rem, where the strip ran off the window.
- **The lecture run sheet (Task 13):** the page's own run-sheet treatment, times in the gutter as time into the class, one item per injected failure. Pass 2 dropped a two-hour timeline graphic (the gutter times are the timeline) and severity badges.

The app's views were checked in Chromium through Playwright on the dev server at 1168 px, in light and dark and with large type; the checks page at 1168 px in light and 390 px in dark. Screenshots were deleted.

*Live spend:* $0.5378, all in `spend.jsonl` under courses `m6-*`: the synthetic lectures' notes and transcription $0.3367 (`m6-faults` $0.1134, `m6-disk` $0.0292, `m6-app` to `m6-app5` $0.1941), and V09's page $0.2011. The live STT tests through the forwarder write no ledger lines: about 14 s of speech, computed at $0.0008.

*Rulings during execution* (the ledger has all of them with their costs):
- A joining source is placed against the host clock's current position, not the mixer's last pull, which lags it by up to a tick (the first click was 345 samples early).
- The drift stage is `rubato`'s polynomial resampler after the existing FFT resampler, and core is optimised in the dev profile (above).
- `SourceEnded` waits at most 1 s for room, since a session whose owner holds the receiver without reading would otherwise wait out the whole timeout.
- `lecture_dir` makes the folder absolute without touching the filesystem, because the folder is created only after the input is found.
- The network-address tests use port-less URLs, since `reqwest` prefers a URL's explicit port to the override's.
- `coordinator::tests::a_stuck_stt_worker_loses_frames_not_control_messages` has a paced source, as M3 named, with its assertions unchanged.
- The app's preview has fixture-only states (`?look=`) for the new views, which the real transport never reads.
- Mixed mode ships enabled (above).
- The reviewer's suggestion to retry a failed last snapshot once is not built: §6.2 and §10 keep a failed batch pending, the end notice says so, and the next session takes it.
- The plan ran in the main checkout, because its tools read the git-ignored `.env` at the repository root.

*Review* (a fresh reviewer on the most capable model, over `394bd13..0eab1da`): 0 Critical, 5 Important, 12 Minor. Re-graded by effect, since the gate is zero unexplained missing audio: four Important and two Minor entered the fix pass, and each was fixed test-first:
1. **A fallback that could not be opened sent the wait back to nothing.** The source now keeps waiting for the person's own input (`source::tests::a_fallback_that_cannot_be_opened_keeps_the_wait_for_the_original`).
2. **A segment-log or transcript write that failed said "Bad file descriptor" without its file.** Every such write, and the session line's, names its path (`segments::tests::a_write_that_fails_names_its_file`).
3. **`lecture audit` exited 0 while transcript gaps still waited.** It exits 0 only for a whole folder, 1 on anything unexplained, 2 while anything waits (`audit::tests::the_exit_status_is_0_only_for_a_whole_folder`).
4. **The end notice said "Saved" after the last snapshot failed.** The stop report carries the last snapshot's result, and the app and the CLI both warn that the notes miss the end until the next session in the folder adds it (`lecture_gate::a_last_snapshot_that_fails_is_in_the_stop_report`, `app::tests::the_end_notice_says_when_the_last_snapshot_failed`).
5. **In mixed mode a source rejoining at once (a rate change) threw away the audio it had delivered**, unmarked (Minor, re-graded). What it delivered now plays in front of its new audio (`mix::tests::a_source_that_rejoins_at_once_keeps_what_it_delivered`).
6. **A source still silent when a mixed recording ended left that stretch unmarked** (Minor, re-graded). It is a gap (`mix::tests::a_source_still_dry_when_the_recording_ends_is_a_gap`).

The fifth Important finding, that `MIXED_MODE` was set before the live run ended, was sequencing: nothing merged before the live run's numbers.

*Open threads, each taken or left.* No milestone follows M6, so "left" means left for good unless the person asks.
- **Taken:**
  - the capture ring's unpublished tail drop (the `concurrent_overflow` flake; 50 runs under a parallel full suite, 50 passed);
  - the dropped `SourceEnded`, and M4 minor M4 (a late one re-showing "Stop waiting");
  - M3's minors: the last snapshot after a failed session, the session marker glued onto a torn line, `course` from a relative `--dir`, and `"error": null` read as an error;
  - M4's minors M1 (a pause between hash retries), M6 ("Stop" while starting), M9 (the gap count from the sidecar at start) and M11 (unhandled `hydrate()` rejections);
  - M5's minors M1 (the strip back to its word before a lecture at the end) and M2 (the Screen Recording copy says to reopen the app);
  - the golden tests frozen; spec §8's slide fields, §13's keyring and `rubato` lines and §14.3, as built;
  - a failed transcript write leaving the log and the transcript apart (M2): M3 puts back a line lost between the two writes, and the write error now names its file;
  - the Python CLI byte comparison (M3's optional check): V09, above.
- **Decided from measurement:** REST's first word (above): the lead-in remedy is not taken.
- **Waiting for the real lecture** (the run sheet holds each): Zoom's window drawing while full screen elsewhere (V04); the detector on a live Zoom meeting (V02); whether a real lecture draws empty snapshot answers; §14.2's page latency on a real lecture (the synthetic page took 5 min 36 s for 671 words); the receiver's clock in mixed mode (V11 and the audit).
- **Answered at the sitting, and done** (below): D1, Polish after a lecture has ended, built; D2, the built app's security policy (M4's thread), checked.
- **Left for good:**
  - M2's minors: any REST 4xx ends recovery for the session, and the reconnect backoff resets on every handshake (neither met in M6's runs; a refused piece stays a gap for the next session); recovery retries without limit while the session runs (that is what lets recovery finish after a long outage, and a second stop ends it).
  - M3's minors: a relaunch after midnight starts the next day's files, and migration and rebuild date slide files and markers after midnight on the start day (a lecture's files split at midnight, and other days' gaps are recovered at launch); the page retries a billed `length` or empty answer once (one page's cost at most); `lecture page` runs without journal recovery (the next session recovers the journal, and the page can be made again).
  - A Python CLI already running in a folder when the Rust CLI starts there: the run sheet gives the Python CLI its own folder, and the Python CLI goes once the lecture holds.
  - Keychain prompts after each dev rebuild: a dev binary's signature changes at each link (V08 answers it once per build).
  - M4's minors M3 (the page shares the busy line with snapshot and polish), M5 (a start that fails late stays in "starting"), M7 (a hydration begun before hiding reused on return) and M10 (the CLI's start-up line order): not met in M6's runs, and none loses material.
  - M5's minors M3 to M7 and the title bar's large-type crowding: each concerns capture's questions or narrow windows, the display is 1168 pt wide, and the real lecture's V02 and V04 would show any that matter.
  - This review's minors: M1 a resync after a dry spell adds a spurious, explained overflow gap; M3 overflow gap positions ignore drift correction; M5 mixed-mode copy (DeviceBack says "in a new file", notices show device ids, the CLI's mixed line suggests `--device`); M6 the offer names the original device after a chosen fallback goes; M7 a failed "Record from it" shows nothing while the offer is up, and the list is not refreshed while waiting; M8 a fallback chosen as the device returns can switch at the next disappearance; M9 tests built with `::new` configs inherit `LECTURELIVE_API_ADDR` from the shell; M10 `--mixed`'s name match can pick BlackHole itself and skips the loopback set-up warning; M11 the CLI test changes the process's directory; M12 mixed mode's end count of stream errors is always 0.

*The sitting* (2026-09-26; yes to both):
- **D1, Polish after a lecture has ended, built.** After a lecture the app's bar has Polish beside Study page, one at a time. `Lecture::polish_after` opens the folder as a lecture opens it, so a commit a crash interrupted is undone and an edit made after class is accepted before anything reads the notes; then it polishes and typesets the page, as during a lecture. A folder with no sidecar is refused, because opening it would migrate the Python CLI's state one way. Tests: `lecture_gate::a_polish_after_the_lecture_repairs_the_folder_first_then_polishes_and_typesets` (it failed first against a version that skipped opening the folder, which sent a torn block to the polish), `lecture_gate::a_polish_after_the_lecture_leaves_a_python_cli_folder_alone`, and the store's "after the lecture, Polish is offered on the folder's notes while nothing else runs". The CLI still polishes only inside a lecture. `/frontend-design:frontend-design`: an outline Polish before Study page, in the lecture's order and with its name; pass 2 rejected a sparkle icon, a filled button, a cost dialog and a longer label. At 1168 px the two buttons are grouped at the row's end, and in large type the source select narrows to 12rem so Start stays on the row (checked in light and dark, normal and large type).
- **D2, the built app's security policy, checked** (M4's thread). One `npm run tauri build -- --no-bundle`, into a scratch target directory (1 min 46 s), because the bundle's path is the canary's (`target/release/bundle/macos/LectureLive Canary.app`, whose grants V03 needs); the canary's CDHash (231421f3…) and binary hash were the same before and after. The built binary ran `LECTURELIVE_CHECK=csp` on a copy of M4's test folder (`m6-csp/`, report `m6-checks/csp.json`): violations `img-src`, `connect-src` and `script-src-attr`, each once per policy (SvelteKit's tag and Tauri's header); the inline handler did not run; the remote fetch was refused; the hostile Markdown rendered inert, with no image; the registered slide loaded through the asset protocol, and `outside.png` was refused. A first run (`csp-run1.json`) opened today's files in a folder dated the day before, so no slide was registered; the copy was renamed to today's files and run again. Tauri adds a nonce to `style-src`, which turns off `'unsafe-inline'` there, but Svelte applies `style:` through the CSSOM (`style.cssText`, `setProperty`), which the policy does not govern, and the markup has no static style attributes. The build was deleted after the check.
- Suites after the sitting: core 321 passed, 0 failed, 20 ignored; desktop 23/0/1; CLI 1/0; vitest 33 in 6 files; svelte-check 0 errors, 0 warnings. One core run while Spotlight indexed the build failed `stt_gate`'s refusal, 15 s and 45 s tests again; alone they passed 8/8 three times.

*Failed lines:* none. No spec §14.1 fallback applies.

*Next:* after the real lecture, a follow-up session reads its evidence (the audit's output, V01–V04 and V10–V14), ticks the gate lines that hold, and only then runs the plan's Task 17: `live_notes.py` and `pyproject.toml` are removed and the README moves to the app and the Rust CLI. Until then `live_notes.py` is the in-class tool, and it stays on `main`.

*Baseline for M7* (2026-09-26): M6 implementation is accepted as the development baseline for M7. Remaining real-class verification is deferred and does not block M7 development. This is a scheduling decision, not a result: gate lines 1 and 2 stay unticked until the real lecture is run from `docs/VERIFICATION.html`, now against the newer build, and Task 17 still waits for them.

## M7 — Ratatui CLI

Spec: §3.1, §3.6, §9 (a new §9.5, the terminal UI, is written as built). Plan: [m7-ratatui-cli](superpowers/plans/2026-09-26-m7-ratatui-cli.md). Ratatui becomes the CLI's first-class interactive frontend for the live `lecture` command, beside the unchanged desktop app. `lecturelive-core` stays the only authority for recording, transcription, recovery, snapshots, notes, slides and durability, and M7 changes no core or desktop source (core gains tests only). The plain CLI stays for non-terminal, scripted and diagnostic use. Every task that draws a view invokes `tui-design:tui-design`, then `/frontend-design:frontend-design`, before any drawing code.

Tasks (detailed in the M7 plan):

0. Record the plan and the M6 ruling
1. Characterize and split the plain CLI (behaviour-preserving, goldens first)
2. One stop controller for both frontends
3. Pin the frontend boundary in core (tests only)
4. Mode selection, the debug-only fixture session, and the piped-output check
5. Terminal lease and the minimal TUI (Ratatui 0.30.2, Crossterm 0.29 land)
6. Session view: projection, hydration, cleaning, live header
7. Responsive frame
8. Transcript pane
9. Notes pane
10. Hint input, notes operations, cancel, help
11. Capture on by default in the CLI (the course's saved selection)
12. Failure UX, activity, cleaning in plain
13. Performance and long sessions
14. Live synthetic lecture through the TUI; spec as built
15. Fresh final review and fixes
16. Switch the TTY default (last behaviour change), findings, close-out

**Status: done** (2026-09-28). 25 commits over `fcf1d4e`. The last behaviour change is `fee4a34`, "Open the terminal UI by default for live lectures on a terminal"; the findings commit follows it and changes no behaviour.

**Gate** (checked in `cargo test` against fakes, in a PTY, and live on a synthetic lecture; the plan's §L has each line's evidence):

- [x] No regression of M0–M6: the core, desktop, vitest and svelte-check suites as at M6; no change to `crates/core/src` or `apps/desktop` — `cargo test -p lecturelive-core --no-fail-fast` green (no known M6 load flake recurred, so no 8× rerun was needed); `cargo test -p desktop` 23 passed / 0 failed / 1 ignored; `npm test` 33/33; `npm run check` 0 errors 0 warnings; `git diff fcf1d4e..HEAD -- crates/core/src apps/desktop` empty, the only core change being `crates/core/tests/lecture_gate.rs` (+144)
- [x] The M6 real-class procedure stays available and untouched, and M6 gate lines 1–2 stay unticked — `git diff --exit-code fcf1d4e -- docs/VERIFICATION.html` exits 0; M6's two-hour-real-lecture line and its every-§10-row line remain unticked and were **not** run
- [x] Plain CLI retained: its grammar, its one-shot outputs (`spend`, `audit`, `page`, `record`, `loopback`, `canary`, `inputs`, `outputs`) and its exit codes are unchanged, and its live behaviour is unchanged except the changes this milestone planned and made deliberately — the stop-message fix (Task 2) and the `Gap`/`DeviceGone`/`Recovered` wording and terminal cleaning on a TTY (Task 12, plan §C 11); a pipe keeps its untrusted bytes unchanged by design, and LectureLive itself **adds no** terminal control sequence to non-TTY output — Task 1's help and one-shot goldens green; `the_scripted_lecture_runs_plain_through_a_pipe` green **with no frontend flag** (auto mode + piped streams = plain, and zero `0x1b` bytes for that fixture, which carries no hostile source payload); `tui_is_refused_when_stdin_is_a_pipe` still refuses with exit 1 and an untouched folder
- [x] Terminal restored on normal exit, handled error, SIGTERM, SIGHUP, the emergency stop and a panic-abort child — the PTY lifecycle tests (1, 2, 3, both 4s, 5, 8, the draw failure and the session failure), each asserting the termios before and after and the restoration byte order; the automatic-fallback path restores the same way before it prints its one line
- [x] Staged stop semantics kept (a held Ctrl-C only stops); every accepted key press sends at most one command — `held_ctrl_c_cannot_escalate` and PTY 3; the `ops` scenario's own command log
- [x] An unread or slow frontend holds up neither the lecture nor core; durable state is reconciled by segment id and notes revision; latest-value telemetry may be coalesced — `lecture_gate frontend_boundary` 3/3; `slow_terminal_holds_nothing_up`; `a_lost_segment_comes_back_from_the_log`, `segment_hole_requests_rehydration`, `revision_jump_requests_reload`, `missed_level_samples_change_only_the_meter`
- [x] The transcript follows and scrolls; the notes preview is visibly provisional and the committed block wins; the layout works wide, at laptop width, at half screen, narrow and at the minimum — Tasks 8/9/10's tests and the goldens at 140×40, 110×32, 72×45, 80×24, 60×16 and 40×8; the person's own sizes were not added, because the optional sitting did not run
- [x] Capture state is truthful and never silently rebinds; warnings and recovery stay inspectable; untrusted text cannot emit terminal controls — the `capture` PTY scenario and its command log; Task 12's `failures` walk with the activity overlay; `display_text_cannot_emit_terminal_controls`
- [x] The fixture session exists only in debug builds — a build without debug assertions has 0 fixture symbols and no scenario string, refuses `LECTURELIVE_CLI_FIXTURE` with exit 1, creates no folder and leaves the scratch `HOME` untouched (the §J boundary test, re-run on the final tree); every fixture reference is under `cfg(debug_assertions)`
- [x] One Crossterm 0.29.x and one Ratatui 0.30.2 in the CLI's graph, no other terminal backend, and every new duplicate crate classified — `crossterm 0.29.0` once (the CLI, `ratatui-crossterm`, `tui-input`); `ratatui 0.30.2` once; `ratatui-core` / `ratatui-crossterm` / `ratatui-widgets` once each; `tui-input 0.15.4` with `default-features = false, features = ["crossterm"]`; `pulldown-cmark 0.13.4` `default-features = false`; one `unicode-width 0.2.2`, shared by the CLI, `ratatui-core`, `ratatui-widgets` and `tui-input`; no Termion, Termwiz or Termina anywhere in the graph; `cargo tree -d` matches Task 15's classification with no new entry — `hashbrown 0.16.1/0.17.1` (inside `ratatui-core`), `itertools 0.13/0.14` (ratatui vs a bindgen build-dep), `either` and `bitflags` at one version each on host and target: all build-time or internal, **none material**
- [x] TestBackend, PTY and performance suites pass; a synthetic live lecture through the TUI audits whole — the CLI suite 274 + help 2 + perf 1 + pipe 3 + PTY 20, 0 failed; **five consecutive** PTY runs all 20/20; the performance suite re-measured once on the final tree, every §L target met with no idle rerun needed (draws 11.20/s mean, quiet 1.00/s, two-hour p95 0.133 ms, burst p95 6.361 ms, key-to-draw p95 47.762 ms, event-to-draw p95 50.814 ms, CPU 0.06 / 0.48 / 8.59 %, max RSS 33.16 MiB); the live synthetic lecture is Task 14's accepted evidence, not repeated
- [x] A fresh final review leaves no Critical or Important finding open, and the terminal-default switch is the last behaviour change — Task 15: **PASS**, 0 Critical and 0 unresolved Important after `c215f2b`, `8a895ec` and `5c90ed8`; `fee4a34` is M7's last behaviour-changing commit

**Left open, deliberately:**

- [ ] The real owned-window Screen Recording gate (Task 11) is still **pending**. It needs the person's own Terminal against a real Zoom window with Screen Recording granted; no scripted session can stand in for it. No line above depends on it, and M7 does not claim it.
- The Task 16 personal Terminal sitting was not run, so no goldens exist at the person's own `tput cols`/`tput lines` and no sizes are invented for them. No gate line depends on the sitting.

**M7 findings**

Three Important, all raised by the Task 15 final review and each fixed test-first in its own commit:

| ID | Finding | Fix |
|---|---|---|
| I1 | A held Ctrl-S wrote a durable slide per key repeat. Nothing pushes Crossterm's keyboard-enhancement flags, so terminals report every repeat of a held key as `Press` and the `Press`-only guard never fired; core saves a new slide for each `CaptureNow`. | `c215f2b` — a Ctrl-S that would act now takes the stop controller's held-key rule (≥300 ms quiet, ≥2 s since the last accepted one); another is refused, with a reason |
| I2 | An unreadable `capture.json` stopped the live lecture from starting, in both frontends. | `8a895ec` — an unreadable selections file at lecture start is no selection, said in one warning line (plain, and the TUI's activity); the file is left as it is, and `keep_selection` still refuses to overwrite it |
| I3 | The PTY harness failed about 10 % of parallel runs on a kernel-private race in `openpt` (XNU `EREDRIVEOPEN`, −6), before any child existed. | `5c90ed8` — the harness retries `openpt` only on raw errno −6, 5 attempts 50 ms apart; every other error, and everything after allocation, fails as before |

The 30 Minors are the Task 15 review's, with their owners. **Seven were Task 16's and are now closed**, all as documentation or gate corrections with no behaviour change:

| ID | Finding | Closed by |
|---|---|---|
| O1 | The gate line claiming the plain CLI's output was unchanged "except the planned stop-message fix" was not literally true: Task 12 deliberately changed the `Gap`/`DeviceGone`/`Recovered` wording and added terminal cleaning | The M7 gate line rewritten to the truth **before** being ticked |
| O3 | The gate line said non-terminal output "carries no terminal control sequences", which is not the rule — a pipe carries untrusted source bytes unchanged by design | The same gate line, now "**adds no**"; spec §9.5.10 already stated the rule correctly |
| D1 | Spec §9.5.2 said a rule sits above the notice in every shape; only Wide and Normal draw one | §9.5.2 now names those two shapes, and Stacked's heading-as-rule |
| D2 | Spec §9.5.7 said the keys row is generated from the key table; `footer()` and `help_lines()` are separately maintained lists | §9.5.7 now says so |
| D3 | Spec §9.5.9 said a red meter for silence and a red `▲` on the slides tab; silence is red words in the meter's place, and the tab's `▲` takes the tab's own style | §9.5.9 now describes both as built (`view.rs:796-801`, `view.rs:861`) |
| D4 | Spec §3.6 said a duplicate slide index is ignored; the live `Slide` upserts by index | §3.6 now separates slides from segment ids and notes revisions (`state.rs:681-691`) |
| D5 | Spec §9.5.8 read as though the 300 ms/2 s debounce governed every stop; it is the key origin's alone | §9.5.8 now names the signal, `--secs` and audio-end origins as not debounced (`stop.rs:76-96`) |
| X3 | `origin/m7-ratatui-cli` exists although `CLAUDE.md` says milestone branches never go to the remote | **Left as it is — the person's decision.** Task 16 pushed `main` only, did not push the milestone branch, and did not delete the remote one; it is reported in the handover |

The other 23 Minors stay where Task 15 put them and are **not** fixed by this milestone: R1–R5 and F1, F2, F3, F5, D6, D7, P2 (future cleanup); R6 (M8 / core session); R7, F4, F6, P1, X1, X2 (leave-for-good). Two are worth naming. F2: the TUI's control-free render assertion is guaranteed by Ratatui's own filter, so it cannot detect a removed `clean`. X2: `tui/mod.rs`'s `mod task13` reaches `crate::fixture` under `cfg(test)` only, so the tests do not compile without debug assertions.

**Evidence carried from earlier tasks**

- Default mode switch: `fee4a34`.
- Performance (Task 13, accepted): draws ≤12/s, mean 11.20; preview presentation ≤10 Hz; quiet 1.00 draw/s; two-hour frame p95 0.161 ms; burst frame p95 6.484 ms; key-to-draw p95 17.393 ms; event-to-draw p95 51.017 ms; CPU 0.05 / 0.48 / 10.62 %; max RSS 33.27 MiB. The final tree re-measured within the same targets.
- Live synthetic lecture (Task 14, `ad22037`): real BlackHole, real STT, real TUI, 411 s from 23:36:20 to 23:43:27. A real dropped slide was captured, and an ordinary snapshot, a hinted snapshot, the polish path and both staged stops were exercised. Audit 0 unexplained / 0 waiting; `segment_cursor` 11 against 11 contiguous ids; no journal left behind; the slide embedded exactly once; the default output byte-identical before and after; **$0.086238** spent, within the $0.50 ceiling; exit 0 with the terminal restored. Not repeated, and no paid request was made again.
- PTY allocation adjudication: the −6 failure is a kernel-private XNU tty-allocation race, not a test defect; the bounded retry is harness-only and fired 0 times across the final five runs.

**Next:** M8 — Automatic Zoom capture, which depends on the M7 TUI/capture architecture.

## M7.1 — A live lecture's findings: pause, quit, a restarted Zoom window, repeated slides

Spec: §7.1, §7.2, §8, §9.6, §10. A real class on 6 October 2026 ran the app beside the Python tool through a quiz taken in a proctoring browser, which quit Zoom. Four things were found; each is fixed test-first, and the live checks are V17–V20 in `docs/VERIFICATION.html`.

**Findings** (evidence from the session's own files; the app was recording a test folder on a scratch disk).

| # | Finding | Evidence | Fix |
|---|---------|----------|-----|
| F1 | A restarted Zoom window was never watched again | The detector's sample log ends at 12:36:37 with `the window server returned no image`; Zoom came back at 12:48:34 and nothing was sampled for the next 17 minutes: capture asks and waits for a click when a window closes (§7.1), and no one was there | `capture::worker`: when the bound window is gone and exactly one window matches the selection as at a lecture's start, it is watched through the same region and the strip says so (`follow`). Several matches, or a size not seen before, still ask. The same applies to a saved window that is not open at the start. M5's gate line "window replacement handled without silent rebinding" is kept for any other window |
| F2 | The same slide was registered many times | 24 slides in 5 minutes and 40 in 13, 7 of the first 11 distinct by bytes; the saved region ran to the bottom of the Zoom window, so Zoom's control bar fading in and out was a lasting change each time. Every registered slide is sent as a full-detail image and must be embedded exactly once, so duplicates cost tokens and repeat in the notes | `capture::detect`: a settled frame equal to one of the last 3 kept frames within 60 samples is the same slide again and is not registered (`RECENT_KEPT`, `RECENT_SAMPLES`). Test: the control-bar flip-flop registered 9 slides before and 2 after |
| F3 | No way to stop spending during a break | Audio streams to speech-to-text for as long as the lecture runs, silence included; slides pile up for the next snapshot | `Command::Pause`/`Resume`, spec §9.6 (see below) |
| F4 | Nothing handled an app quit | `lib.rs` had no run-event handling and nothing listened for SIGTERM, SIGHUP or SIGINT: a quit would end the process with the recording unfinalized (repaired at the next start, the open utterance and the tail lost). The CLI's terminal UI exited with the lecture still running | `Command::Quit`, spec §9.6: files saved, no last snapshot, no wait for recovery, the request in flight cancelled. Desktop: one shared 8 s deadline across Cmd-Q, window close and signals; CLI: SIGTERM and SIGHUP in both frontends |

**Pause as built.** `audio::pause` is a source between the real one and the coordinator, so the coordinator is unchanged: while paused it delivers no audio, ends the recording as any recording ends (the utterance is flushed, the connection closes) and, on resume, begins the next recording anchored by the source's own clock. The sidecar's `pauses` records each pause and the audit counts the hole between the recordings as explained by it (an open pause, left by a crash, explains only the hole it began in). Automatic slide capture is held (`CaptureCmd::Hold`); slides taken by hand, snapshots and polish still work. Controls: the desktop's Pause/Resume and `pause`/`resume` on its command line, the CLI's typed `pause`/`resume` (plain and terminal UI), and `^P` in the terminal UI.

**Tests.** Core: 13 new unit tests (`audio::pause` 9, detector 2, audit 2, one of them the open-pause case) and 5 new gate tests, with 3 changed: capture 2 new (two matches still ask; a held worker) and 3 changed to the new rule (a restarted window is followed, also when its old window lingers off screen, and a saved window that was not open at the start is followed when it opens); lecture 3 new (a paused lecture sends no frame to STT and its resume is a second recording on a new connection; a stop while paused closes the pause; a quit is prompt, takes no last snapshot and leaves the recordings finalized). Desktop: 31 Rust (7 new: two for the pump, one for what a stopping lecture refuses, four for the quit deadline) and 36 frontend (3 new), `svelte-check` 0 errors. CLI: 285 unit tests plus the pipe and PTY suites, including SIGTERM and SIGHUP. Failing as before this milestone: three `stt_gate` reconnect-timing tests, and one PTY test whose needle breaks against a seed notice containing today's date.

**Not verified by a machine** (V17–V20): the visual look of the desktop's paused state in light, dark and large type; a real Cmd-Q, an Apple-Event quit and a signal saving a real lecture; a real Zoom restart followed in a live meeting; the control bar no longer registering repeats on a live meeting.

**Open threads.**
- A snapshot attaches every pending slide as a full-detail image, with no cap. The 200,000-token budget shrinks the document, not the images: about 100 slides use it all, and past about 270 the request is too large to send and every snapshot after it would fail, since the batch never shrinks. F2 and the pause make it unlikely; a cap that carries the remainder to the next snapshot needs a decision on how the notes' timeline is split, so it is not part of this milestone.
- The spend ledger counts streamed audio by seconds sent, so a pause's saving is visible there; the study of how much a break would have cost without it is not done.
- Python's `lecture` has none of this: it records through a break and takes no part in the quit handling.

**The verification harness** (`tools/verify/`, with its own README). `run.sh` runs `docs/VERIFICATION.html` unattended wherever a machine can: the CLI and the app against real transcription of synthetic speech played into BlackHole, a forwarder of its own that takes the network from the lecture alone, a 40 MB scratch disk image, a virtual input in place of the receiver (`virtual_input.swift`), kills and quits by signal, Apple Event, Cmd-Q and window close (also on a packaged app it builds), the in-app capture check, photographs of the app, and the audit as the judge. It signals only processes it started, never speaks into BlackHole while the Python tool runs, stops at a deadline and at a spending cap, and proves its forwarder works before any network check relies on it.

*Results of its first day* (6 October 2026, four runs merged by `merge.sh`; $0.31 spent): V01 (a 31-minute recording of silent BlackHole, one recording, no gap, audit 0 unexplained and 0 waiting, with the other stages speaking over it and builds running), V10, V11, V13, V14, V15, V17, V19 and V20 pass, each for the CLI and, where it applies, in the app (V10 and V14 in the app: the link held and refused, SIGKILL mid-lecture, repaired at the next start, every word in the transcript). V18 passes for the CLI (SIGTERM, SIGHUP, and SIGTERM while paused), the dev app (Cmd-Q, window close, SIGTERM, SIGHUP, and while paused) and a packaged app built from the tree (an Apple Event addressed by app name, which is how a proctoring tool quits apps, Cmd-Q, window close, SIGTERM, and an Apple Event while paused): every one ended in under three seconds with every recording finalized and no last snapshot spent. Workspace tests: 684 pass, and the only failures are the four named under Tests above, which fail alike without this milestone's changes.

*Found by the harness, and fixed here:* a restarted window was not followed in the in-app check until the check's own slide for the pause was chosen well (a one-line build is at the detector's known blind spot); the harness's own bugs (a marker word the transcription service spells two ways, a forwarder port that a second run could not bind, so a network check could pass without cutting the link, a signal guard that refused the packaged app, a finalized-recording precondition that a lecture paused before its first audio could not meet) were fixed and the stages re-run.

*Found by the harness, and left:*
- **The paused strip, from the photographs** (light and dark, normal and large type; the paused state is unmistakable in all, three columns stay on screen in large type, nothing overlaps the traffic lights): while paused the strip shows two truncated fragments (`not…` for the phrase "nothing is recorded…", and the course name cut to `m…`); in large type the paused clock is clipped (`0:00:31 pau`); and the footer says the same thing twice (the persistent "Paused. Nothing is recorded…" line and the notice under it). All cosmetic; the view change goes through a design pass.
- **V05:** the drag and the double-click zoom are observed; a second double-click does not restore the earlier position (the window is already at its standard size), so "another restores it" is not shown.
- **A System Events `quit` addressed to a process** is ignored even by a packaged app, and the bare dev binary has no application name to address an Apple Event to; the by-name Apple Event, which is what a proctoring tool sends, works on the packaged app.
- **Not automated, and why:** V02 and V04 need a real Zoom meeting that shares slides (see class mode below); V08 needs the login password; V12 and V16 would take Screen Recording and the microphone from Terminal, which also serves the Python tool.

*Afterwards, the same day.* The three paused-strip defects above are fixed (the strip drops its "nothing is recorded…" phrase while paused, the clock no longer carries a redundant "paused" label, and the footer's latest-notice line skips the Pause notice that the persistent line repeats), photographed again in both appearances and both type sizes, and the frontend tests and type-check pass. **V07 passes:** a study page typeset for about two cents from 54 words of notes, the app's own page path re-using it at once, the default browser (Chrome) coming to the front with the page; a check run opens the browser only when `LECTURELIVE_CHECK_OPEN` is set. **V12 and V16 on the packaged app's own permission identity** (so Terminal's, and the Python tool's, are untouched) are experimental stages (`gui_perms_*`, opt-in): the prompt appears with the app's own usage text and a script can press its buttons, but after a scripted "Don't Allow" the app still reads `undetermined`, so a scripted refusal is not a person's; they stay manual items.

**Class mode** (`tools/verify/class.sh`, rehearsed against a stand-in window). One headless CLI lecture on BlackHole in a scratch folder, watching the Zoom meeting window through the course's own region with the control bar left out, ending by a quit (no snapshot, no slide sent). `finish` writes the V01 audit for a real class and the slides the detector took, with its recording, to read against the lecture (V02); `v04` is the optional five minutes of Zoom full screen with another app in front. It records only; the person's Zoom view, the Python tool and the audio are untouched.

**The real class, run through class mode** (6 October 2026, from 16:11). Class mode watched the meeting beside the Python tool and its recording of the detector's input is the evidence below. `crates/core/examples/replay.rs` plays such a recording back through the real worker, `scene_stats.rs` prints the scene measures of its frames and `layout_probe.rs` the content rectangle found in its window copies, so each fix below is measured on the class itself. The recording is local and is never committed.

*What the old code made of the class:* 71 captures by 17:30. Seventeen were taken before the share began, 27 were whole slides, and 27 were broken: 26 corner crops 706 px wide against 1600, kept from 16:30:15 to the last look, and one 202 px shot of the window after it was shrunk. The new code, replayed over the same class from the person's own saved region (4121 samples, one a second, to 17:20): 46 slides, none before the share, none cropped (39 of 892 × 503 in the windowed layout and 7 of 1024 × 594 in full screen, the recording's own scale).

| # | Finding | Evidence | Fix |
|---|---------|----------|-----|
| F5 | Zoom's own screens were kept as slides | 17 of the first 21 captures were taken before the share: the lectern camera (its compression grain changes the frame every second, so a presenter sitting still settled and was kept), a participant's name on a dark tile, and "has started screen sharing" | `capture::scene` (§7.2): the frame is measured; a camera or a dark notice takes nothing and the strip says what is showing. Measured on the class: 127 of 128 pre-share frames read as the camera, the other and the notice as notices, and all 737 frames of the share as pages. Replay of the first five minutes: 17 captures before, 0 after, and the first capture is the first page. Test: a shimmering camera, then a notice, then a page, through the worker |
| F6 | A region found wrongly while Zoom went full screen stayed wrong for the rest of the class | At 16:30:16 the worker searched for the kept slide in a frame that was still moving and chose a corner of it, 30% × 27% of the window (the page number); the size then did not change again, so nothing searched again, and the region was saved to `capture.json`. Every page after it was a corner crop, and pages 15–18, shown while Zoom was full screen, were never seen, since inside the corner only the page number changed | `capture::layout` (§7.1): the shared content is the largest rectangle that is not Zoom's own dark; the region is checked against it on the first sample after any region is taken up, then every 5 samples, and a region looking elsewhere is moved, with nothing taken through it meanwhile. The search for the kept slide starts at half the window's width and must beat a plain patch of its own colour, and failing it the content's edges give the region (it asked before). On real frames the rectangle is within 1% of the person's own region. Replay from the saved crop: moved at 16:30:33, no crop kept, a whole slide at each page change. Test: a region at a corner of the slide is moved before anything is taken |
| F7 | Leaving full screen kept the full-screen region | On this Mac full screen is 1168 × 729 and the window 1168 × 733, within the 2% that names the same window, so the region for 733 was answered with the current one, which took in the speaker strip | `Selection::at_size`: a size seen exactly comes first. Test: two sizes 4 px apart keep their regions through full screen and back |
| F8 | Slides shown for a second or two could be missed | At one sample a second a page held for 1–2 s is seen twice about half the time; the deck's page 3 was never captured (the recording shows page 2 and a second later page 4) | Three samples a second with the calibrated counts scaled, and a slide kept after two further samples show it still (`Thresholds::at`, `SAMPLE_HZ`): a page held for two thirds of a second is kept, as the sharpened picture. Test: five pages of a second each are all kept; sampled once a second they were not. The replay cannot show this, since the recording was made at one a second |

*The harness:* the meeting-window match read `canary windows`' columns wrongly, so the arm watcher waited 25 minutes and class mode was started by hand; V04's restore looked for the window by name, which is empty while Zoom is full screen, so Zoom was left full screen; its counter used `strptime("%z")`, which rejects `-04:00`, and recorded "0 slides" for three; and it judged by count, which would have passed crops. The rehearsal had stood in a window finder, so none of it was reached. Fixed, with `class.sh selftest` (48 checks; with the old window match put back it fails, wanting the meeting window's id and size and getting nothing), V04 reading each slide's size (whole or cropped) and restoring Zoom by its state, and `LECTURELIVE_BIN` to run class mode on a build made beside a running class.

*Tests:* core 260 unit (new: scene 4, layout 4, detector 2, selection 1) and every gate suite pass, `capture_gate` 24 (2 new); CLI 285 unit pass and the desktop crate compiles; the one PTY test that fails (`the_failures_scenario_walks_the_table_and_keeps_it_inspectable`, waiting for an audio-gap notice) was failing before this work.

*Not yet run on a live meeting:* the new build (the class above was recorded by the old one); full screen on and off, V04, and a deck paged through quickly. Dark decks are the open question for the scene measures: a dark slide with next to no sharp edges reads as a notice, and the manual capture covers it.

**Next:** run class mode on the new build at the next class (`LECTURELIVE_BIN`), then V04 again; V08 is the person's, with the login password. M8 (Automatic Zoom capture) is unchanged.
