# LectureLive companion app — design

Status: design for implementation, 2026-09-22. Nothing here is implemented yet. The
Python CLI in `live_notes.py` is the current tool; its prompts and human-readable file
formats are the baseline, and its checkpoint behaviour is replaced (§8).

## 1. Context and goals

### 1.1 What exists

`live_notes.py` captures the microphone, posts 8–30 s chunks (cut at pauses) to Grok
Voice Transcribe's REST endpoint, appends `[HH:MM:SS] text` lines to a transcript,
watches the macOS screenshot folder for slide captures, and on Enter sends the notes
document (tail-capped at 40,000 characters) plus the new transcript and slides to
`grok-4.7`, appending only the new notes under a `<!-- HH:MM:SS -->` marker. It resumes
after a restart from a `noted_through` timestamp and can rewrite the document into a
study document (`polish`).

### 1.2 Problems observed

| # | Problem | Evidence |
|---|---------|----------|
| P1 | Transcript arrives in 10–30 s blocks; the snapshot boundary lags the speaker | REST chunking; the CLI force-cuts and drains a queue before every snapshot |
| P2 | Audio is captured acoustically although the lecture is a Zoom stream inside the machine | accent + room acoustics produced "pre-chain data" for "pretrained data" |
| P3 | Slides are captured only by manual shortcut | the user must notice each slide change while listening |
| P4 | An API failure loses speech; nothing records the audio | retries exist, no ground-truth recording |
| P5 | Resume is not lossless | checkpoint is the snapshot *request* time, not the committed batch (`live_notes.py` `snapshot`); `>=` on second-resolution times replays same-second lines; notes append and state save are separate writes, so a crash between them duplicates a snapshot; state and polish writes are not atomic; a first run infers "already noted" from the transcript's mere existence |
| P6 | Embed guard checks presence only | not exactly-once, not own-line, not outside code fences |
| P7 | Polish ignores a failed prerequisite snapshot and truncates the original file | `polish()` |
| P8 | Terminal UX: hints typed into the stream the transcript prints to | |

### 1.3 Goals (v1)

- G1 Live transcript with word-level fade-in, tentative words visibly distinct from committed ones.
- G2 Snapshot → notes streamed token-by-token into the document pane; the file changes only by a verified, recoverable commit.
- G3 Auto-capture of a selected region of the Zoom window on slide change, including small builds; manual capture always available.
- G4 System audio via BlackHole loopback with automatic, crash-safe output routing; any physical input; mixed mode opt-in once it passes the drift gate (§4.2).
- G5 Human-readable files compatible with the CLI's (Markdown notes, transcript, `slides/`), with exact resume.
- G6 Captured audio is durable to within the last one-second checkpoint after a crash, and every interval without durable audio or without committed transcript is reported as a gap.
- G7 `polish` and hint input as in the CLI.

### 1.4 Non-goals (v1)

Per-channel labelled transcription, in-app notes editing, key-term extraction from
slides, a past-lectures browser, cloud sync, accounts, telemetry, Windows/Linux.

## 2. Decisions

| Decision | Choice | Why |
|----------|--------|-----|
| Pipeline runtime | Rust, inside the Tauri process, plus a Rust CLI on the same core | No Python at runtime; one implementation of every semantic |
| Frontend | Svelte 5 + TypeScript, Tauri 2 | Runes handle streams without effect hooks; small bundle |
| Speech-to-text | Grok Voice Transcribe 2.0 over WebSocket, `interim_results=true`; REST for gap recovery | Partials every ~500 ms, word timestamps, keyterms; REST re-transcribes recorded intervals |
| Notes model | `grok-4.7`, chat completions, `stream: true` | Same key; vision for slides; SSE token stream |
| Loopback | BlackHole 2ch, conditional on the M0 routing gate | Plain input device from Rust; ScreenCaptureKit or a Core Audio process tap is the fallback (§14) |
| Source of truth | Markdown, transcript and slides on disk; a versioned sidecar records exactly what is committed | Human-readable and tool-independent; the sidecar makes resume exact |
| Target | macOS 13+, Apple Silicon, one user | |
| Credentials | API key in the macOS Keychain for the app; `.env` or environment for the CLI; never in frontend state or lecture files | |

## 3. Architecture

### 3.1 Workspace layout

```
LectureLive/
  Cargo.toml                 workspace, pinned toolchain (rust-toolchain.toml)
  crates/
    core/                    library: the whole pipeline, no UI
      src/audio/             sources, conversion, alignment, mixer, framer, recorder, routing
      src/stt/               websocket client, transcript state machine, REST recovery
      src/notes/             batch builder, prompts, SSE client, embed validation, polish
      src/capture/           window enumeration, capture worker, change detector
      src/session/           coordinator, lecture folder, sidecar, commit journal, lock
      src/events.rs          notifications to adapters
    cli/                     headless binary on core (replaces live_notes.py at M6)
  apps/desktop/
    src-tauri/               commands → coordinator handle; notifications → events/channels
    src/                     Svelte 5
  live_notes.py, pyproject.toml   retired at the final acceptance gate (M6)
  docs/
```

Dependency direction: `desktop` → `core` ← `cli`. `core` never depends on Tauri.

### 3.2 Ownership: the session coordinator

One coordinator task owns all correctness-bearing state: pending segment and slide
identities, every file mutation, snapshot/polish serialisation, and session state.
Workers (audio, recorder, STT, notes, capture) own their native objects and talk to the
coordinator through bounded Tokio channels with request/reply handles. No shared
`Session` mutex is held across network calls. Notifications to the UI describe
decisions the coordinator has already made; nothing durable depends on the UI.

Tauri holds only a thread-safe coordinator handle. Native objects that are not `Send`
stay on the worker that created them; main-thread-only APIs are dispatched explicitly.
Image encoding and blocking file I/O run on blocking threads. The desktop adapter uses
Tauri's runtime; it does not create a second Tokio runtime.

### 3.3 Time

Every audio frame carries `{ recording_id, sample_offset, valid_samples }`. Each
recording persists its anchor: the wall-clock time (full date, ms) of sample 0. STT
connections carry an epoch and the sample offset at which they began, so server word
times map to recording sample offsets and from there to wall time. Displayed
`[HH:MM:SS]` is derived from this mapping, never from when a response arrived. Gaps
and recovered material are expressed as sample intervals of a recording.

### 3.4 Data flow

```
source ─► format/channel conversion ─► resample + clock alignment ─┐
source ─► format/channel conversion ─► resample + clock alignment ─┴► gain mix ─► PCM16 framer (100 ms)
      framer ─┬─► Recorder (bounded queue, never waits on network)
              └─► STT writer (bounded queue) ─► SttStream ─► Transcript state machine ─► coordinator
capture worker ─► detector ─► tmp file ─► rename into slides/ ─► coordinator (registers slide)
snapshot/hint ─► coordinator: STT cutoff + finalize ─► immutable batch ─► NotesClient (SSE preview)
            ─► validate ─► commit journal ─► append ─► checkpoint ─► NotesCommitted notification
```

The audio callback only copies into preallocated bounded storage; conversion,
resampling, disk and network happen on other threads. Queue overflow on either
consumer produces a recorded gap for that consumer and a notification, never a silent
drop.

### 3.5 Interfaces (sketch)

```rust
struct Frame { recording_id: Uuid, sample_offset: u64, valid_samples: u32, pcm16: [i16; 1600] }
enum SourceSpec { Input(DeviceUid), Loopback, Mixed { input: DeviceUid } }

enum SttMessage {
  Created,
  Partial { epoch, text, words, is_final, speech_final, start, duration },
  Done { epoch, .. }, Error { epoch, kind: SttErrorKind, message },
}
struct Transcript;   // closed utterances + open stable chunks + tentative tail (§5.2)

struct Segment { id: SegmentId, recording_id, start_sample, end_sample, text, words }
struct Slide   { index: u32, first_observed: DateTime, confirmed: DateTime, path, auto: bool, uncertain: bool }
struct Batch   { op_id, segments: Range<u64> /* segment-log positions */, slides: RangeInclusive<u32>, hint }

fn list_windows() -> Result<Vec<WindowInfo>, CaptureError>;   // errors distinct from "none"
fn list_inputs()  -> Result<Vec<InputInfo>, AudioError>;
```

### 3.6 Notifications and UI transport

Low-rate status goes over Tauri events: `SessionState`, `AudioLevel` (10/s),
`SttState`, `SlideRegistered`, `NotesStarted`, `NotesCommitted`, `NotesFailed`,
`PolishCommitted`, `Gap`, `Error`. High-rate ordered streams go over a Tauri 2
`Channel` per stream: transcript updates and notes deltas. Every notification carries
`session_id`, a sequence number and, where relevant, `op_id` and document revision.

- Transcript updates: `OpenUtterance { utterance_id, stable: Vec<Word>, tentative: Vec<Word> }` and `UtteranceClosed { utterance_id, segment_id }`. Words carry explicit IDs (§9.3).
- `NotesCommitted { op_id, revision, block }` carries the canonical verified block that replaces the preview.
- `PolishCommitted { revision }` tells the UI to re-fetch the document.
- `get_session_state()` returns a coherent snapshot (document revision, recent transcript, open utterance, slides, pending counts, status) so a reloaded frontend reconstructs itself without replaying history.

## 4. Audio

### 4.1 Sources and permissions

- **Input**: any CoreAudio input, persisted by device UID. The stream opens at a configuration the device supports (negotiated through cpal), not at a forced 16 kHz.
- **Loopback**: the "BlackHole 2ch" input.
- **Mixed** (opt-in after the drift gate): input + loopback. Documented setup is headphones, so the room mic does not re-capture lecture audio already present in loopback. Per-source gain before summing; clamping is only a final safety limit.

States per source: permission denied, device unavailable, stream started, signal
detected. The packaged app declares `NSMicrophoneUsageDescription` and the
`com.apple.security.device.audio-input` entitlement under the hardened runtime;
permission behaviour for BlackHole is verified on the target machine at M0. Opening a
settings pane is never treated as a grant.

Property listeners watch device disappearance, nominal sample-rate changes and output
changes; an affected stream is rebuilt and starts a new timing segment. When a device
disappears: in mixed mode the surviving source continues and the missing interval is a
gap; otherwise that source stops and the UI offers a fallback — never an automatic
switch to the built-in mic.

### 4.2 Conversion, alignment, mixing

Each source is converted to mono f32, resampled to 16 kHz with `rubato`'s asynchronous
resampler, then framed. For one source a fixed ratio suffices. For mixed mode the two
sources' FIFOs are aligned by timestamps and the resampling ratio of the secondary
source is adjusted slowly to hold the FIFO level (independent clocks drift: 100 ppm
over two hours is 0.72 s). Mixed mode stays disabled until it passes a two-hour
alignment test. On stop, the resampler tail and the final partial frame are drained.

### 4.3 Output routing (macOS)

Loopback needs Zoom's output to reach both the speakers/headphones and BlackHole:

1. Before changing anything, persist the current default output UID in app state.
2. Find the app-owned Multi-Output aggregate by its owner UID, or create it with `AudioHardwareCreateAggregateDevice` as a stacked (mirroring) device: subdevices = current output UID + BlackHole UID, clock source = the physical output, drift correction on BlackHole.
3. Set it as default output. Preflight: ask the user to play Zoom audio and confirm signal on the BlackHole input while they can hear it. If Zoom is pinned to a specific speaker rather than "Same as System", the preflight fails and says so.
4. On stop, restore the saved output only if the default output is still the app-owned aggregate; a later user change is respected.
5. On launch, an abandoned app-owned route (saved state present, aggregate still default) triggers a restore offer. A pre-existing user aggregate is never modified or deleted.

Known trade-off: a Multi-Output device has no global volume key control; volume is set
per device or in Zoom.

### 4.4 Recording

The 16 kHz mono PCM16 stream is written to
`recordings/session_YYYYMMDD_HHMMSS.wav` (created exclusively; a collision gets a
suffix) with `hound`: 32,000 B/s, 115.2 MB/h. The recorder calls `flush()` (header
update) and `fsync` once per second, and `finalize()` on stop with the error surfaced.
After a crash the header is repaired from the file length on next launch. A recording
is kept while any gap overlapping it is unresolved; otherwise retention follows a
setting.

## 5. Speech-to-text streaming

### 5.1 Connection

`wss://api.x.ai/v1/stt?model=grok-voice-transcribe-2.0&encoding=pcm&sample_rate=16000&interim_results=true&language=en&endpointing=400&keyterm=…`
with `Authorization: Bearer`. Keyterms are validated (≤100, ≤50 chars) and URL-encoded
individually. Wait for `transcript.created`, then one binary little-endian PCM16 frame
per 100 ms through a single ordered writer. The exact spelling of the finalize control
message (`{"type":"finalize"}` / bare text) and the server's timestamp origin are fixed
by a recorded protocol fixture at M0.

### 5.2 Transcript state machine

State: closed utterances + open utterance (stable chunks + tentative tail).

- `is_final=false`: replaces the tentative tail only. An empty hypothesis never erases stable content.
- `is_final=true, speech_final=false`: locks the chunk's range into the open utterance's stable part.
- `speech_final=true`: the message's text **replaces the whole open utterance** (it is the stitched utterance, not a continuation), produces exactly one segment, appended once to the segment log and the transcript file. Repeated finals for a closed utterance are ignored.

UI stability is not disk durability: the open utterance's sample interval is persisted
in the sidecar, so after a crash an unclosed utterance becomes a gap recoverable from
the recording.

### 5.3 Snapshot cutoff

`snapshot` records an audio cutoff (sample offset), and the STT writer sends all frames
through the cutoff, then the finalize message, then continues with live audio. The
coordinator waits (3 s) for the utterance-final whose epoch and interval cover the
cutoff; no open utterance means nothing to wait for. On timeout, the unconfirmed
interval stays pending and the UI reports "snapshot of confirmed material; transcription
still catching up". Recording never pauses and no lock is held while waiting.

### 5.4 Reconnection and recovery

On an unexpected close: reconnect live-first with backoff (1, 2, 4 … 30 s) and a new
epoch anchored at the current sample offset. The gap is the interval from the last
durably closed utterance's end to the new epoch's origin (including handshake time and
any unclosed utterance). Gaps are recovered by the REST endpoint over the recorded
interval, owned by the recovery path alone, so live and recovery commits are disjoint.
Recovered segments enter the segment log when they arrive and are eligible for the next
snapshot even though their speech times precede earlier snapshots. Auth and parameter
errors (4xx) stop STT without a reconnect loop; recording continues. On stop: drain the
framer, send `audio.done`, process remaining finals, ignore `transcript.done`'s full
text for commit purposes, finalize the recording, take the last snapshot.

## 6. Notes

### 6.1 Batches and prompts

A snapshot takes an immutable batch: segment-log positions after the committed cursor,
slides after the committed slide index, and the hint. The prompt text is
`live_notes.py`'s `notes_system` / `polish_system`, carried over unchanged and covered
by golden tests.

Timeline: segments and slide markers ordered by full timestamp with a deterministic tie
rule (slide before speech at equal time). For streamed sessions, a segment spanning a
slide's first-observed time is split at that point using word timings for the prompt
only; legacy imported lines are marked approximate.

Context: the whole document is sent while the request fits a configured token budget
(document + batch text + images + output allowance). Above the budget, the prompt
carries a derived outline/summary of the omitted prefix plus the recent part verbatim;
the summary is cached keyed to the document revision and invalidated by polish or any
external edit. Text is sliced by characters, never bytes.

### 6.2 Streaming, validation, commit

SSE parsing handles split UTF-8, partial and multiple events per read, and
non-content deltas; only content deltas reach the preview. A response is successful
only with a normal finish reason and non-empty output. Validation, after stripping
code fences and any `#` title: each expected embed line appears exactly once, on its
own line, outside code fences; duplicates are removed; embeds for unregistered paths
are rejected; missing embeds are appended under `### Slides not placed` without an
invented description.

Commit (the coordinator, one at a time):

1. Write the journal entry atomically (tmp + rename + fsync): `op_id`, batch ranges, notes length and SHA-256 before, block length and SHA-256.
2. Append `\n<!-- HH:MM:SS -->\n{block}\n` to the notes file and fsync.
3. Advance cursors and clear the journal atomically.

Recovery on launch with a journal entry present: notes length equals "before" → not
appended, material stays pending; length equals "before + block" and the tail hash
matches → complete, advance cursors; anything in between with a matching "before"
prefix → truncate to "before", material stays pending. Any other state stops with an
explicit recovery prompt; nothing is truncated without a verified prefix.

Failure, truncation or cancellation leaves the document untouched and the batch
pending.

### 6.3 Polish

Runs only after a successful flush-and-commit of all pending material; if that fails,
polish aborts. The previous document is copied to a uniquely named backup and fsynced,
the result is validated (every embed exactly once), written to a temp file and
atomically renamed over the notes file, and the revision advances.

## 7. Slide capture

### 7.1 Window and region

`xcap` enumerates windows; enumeration and capture errors are distinct from "no
windows". The saved selection is a descriptor (app bundle id, title pattern, size) that
is revalidated on start; a mismatch asks the user rather than rebinding silently. The
user drags a slide region inside the window (excluding Zoom controls and participant
video); the region is stored relative to the window. Invalid frames (all-black,
zero-size) are capture failures, not slides.

### 7.2 Change detection

Every 1.0 s: capture, crop to the region, downscale to 256×144 grayscale, divide into
16×16 tiles (144 tiles). A tile changes when its mean absolute difference from the
last kept frame exceeds 0.08.

- **Candidate**: at least one changed tile. Its image and first-observed time are kept.
- **Confirm**: on the next sample, the changed tiles differ from the candidate by < 0.03 → the candidate becomes a slide (captured full-size ≤1600 px, PNG).
- **Animated tiles**: a tile that changes on 3 consecutive samples without settling is masked until the next confirmed slide, so a looping animation neither blocks nor triggers capture.
- **Expiry**: a candidate that has not settled within 10 s is saved flagged `uncertain`.
- The first valid frame of a session is captured unconditionally.

Thresholds are initial values, calibrated on recorded Zoom lectures with annotated
small builds. Content shown for less than one sample interval can be missed; manual
capture covers it.

### 7.3 Manual and imported

Capture button, global shortcut, and drag-and-drop of image files all go through the
same registration path and index allocator as auto capture. Files are written to a temp
name and renamed into `slides/` before registration.

### 7.4 Permissions

Screen Recording (TCC), verified in the packaged app. Capture controls stay disabled
until a real capture succeeds.

## 8. Files, state, resume

```
<lecture folder>/
  lecture_notes_YYYYMMDD.md           append-only during class; <!-- HH:MM:SS --> markers
  lecture_transcript_YYYYMMDD.txt     [HH:MM:SS] text, append-only, in commit order
  slides/slide_NN_HHMMSS.png|jpg
  recordings/session_YYYYMMDD_HHMMSS.wav
  .live_notes/<stem>.v2.json          sidecar (version, lecture date, recordings + anchors,
                                      committed segment cursor, committed slide index,
                                      open-utterance interval, gaps, route state)
  .live_notes/<stem>.segments.jsonl   segment log: id, recording, samples, wall times, text, words
  .live_notes/<stem>.journal.json     pending commit, present only mid-commit
  .live_notes/<stem>_HHMMSS.md        polish backups
  .live_notes/lock                    exclusive advisory lock (GUI and CLI)
```

- Resume is by cursor: segment-log position and slide index, both in registration order. No timestamp comparison decides what is committed.
- The lecture date is fixed when the folder's files are created and does not change across midnight; full timestamps live in the sidecar and segment log.
- Custom notes/transcript/slides paths, course name, JPEG and PNG slides, numbering from the highest existing index, and file-mtime capture time for imported images are supported as in the CLI.
- Before append or polish, the notes file's length and hash are checked against the sidecar's revision; an external edit is accepted as the new revision (and invalidates the summary cache), never overwritten from memory.

Initialisation cases:

| Folder state | Action |
|---|---|
| Empty | Create notes with title, empty sidecar |
| v2 sidecar present | Resume; run journal recovery first |
| v2 sidecar corrupt | Stop; offer rebuild from files (all transcript lines and slides after the last `<!-- -->` marker are pending) |
| Legacy `<stem>.json` only | One-time migration: import transcript lines and slides as segments/slides; those after `noted_through` are pending, marked approximate; write v2 |
| Transcript or slides but no notes | Create notes; everything existing is pending |

Legacy writer: from M3 the Python CLI exits when a v2 sidecar exists, so the two
writers never share a folder.

## 9. UI (Svelte 5, Tauri 2)

### 9.1 Layout

- **Transcript** (left): utterances as paragraphs with times; stable words full opacity, tentative tail reduced; newly stable words fade in (~180 ms). Closed utterances collapse to plain paragraph text. Auto-scroll pins to bottom until the user scrolls up; "jump to live" returns.
- **Notes** (centre): committed document rendered once per revision and frozen; the streaming preview renders below it with the same fade-in and is replaced by the committed block.
- **Slides** (right strip): thumbnails with time and auto/manual/uncertain badges; window and region picker on top.
- **Control bar**: source picker with level meter and route status, Start/Stop, hint field + Snapshot, Polish, capture Auto/Manual and Capture-now, status (STT, capture, gaps, elapsed, cost estimate).

### 9.2 State and lifecycle

One `session.svelte.ts` rune store. Initialisation awaits listener and channel
attachment, then calls `get_session_state()`, then allows Start. Unlisten handles are
kept and disposed on hot reload. Out-of-order or stale-session messages are dropped by
sequence number and session id. Preview work is bounded; when the window is hidden,
queued preview updates are discarded and the store rehydrates on return.

### 9.3 Rendering

Words carry explicit IDs: an unchanged prefix keeps its IDs, replacements get new ones,
promotion from tentative to stable does not remount. SSE deltas are assembled into
words for display, keeping the unfinished trailing word across deltas. Markdown of the
preview is parsed at most every 100 ms; committed blocks are parsed once. Output of
`marked` is sanitised with DOMPurify (no scripts, frames, event handlers, remote URLs);
images resolve only for registered slide files through Tauri's asset protocol scoped to
the lecture's `slides/`. CSP forbids remote loads.

### 9.4 Visual design

Produced at build time with the frontend-design skill: readable at arm's length, light
and dark, a large-type toggle, no decorative motion beyond the fade-ins that carry
meaning.

## 10. Errors and robustness

| Failure | Behaviour |
|---------|-----------|
| Network down / websocket error | Live-first reconnect with backoff; gap recorded; REST recovery when online; recording continues |
| STT 4xx | Stop STT, surface message, keep recording |
| Notes call fails, truncates or is cancelled | Document untouched; batch pending |
| Crash mid-commit | Journal recovery on launch (§6.2) |
| Microphone or Screen Recording denied | Affected feature disabled with fix-it button; the rest works |
| BlackHole missing | Loopback disabled with install hint (`brew install blackhole-2ch`) |
| Routing preflight fails | Loopback not started; reason shown (e.g. Zoom pinned to a speaker) |
| Device disappears | Mixed: surviving source continues, gap marked. Single: source stops, fallback offered |
| Sample-rate change | Stream rebuilt, new timing segment, gap marked |
| Disk write error | Session stops cleanly, path surfaced |
| App crash | Recording valid to last checkpoint; route restore offered; journal recovery; unclosed utterance becomes a gap |
| External edit of notes | Accepted as new revision |

## 11. Testing

- **Unit (core)**: framer across 16/44.1/48 kHz; drift alignment over simulated clocks; mixer gain/headroom; transcript state machine on recorded protocol logs incl. duplicate and late finals, empty hypotheses, continuous speech; finalize cutoff cases (no open utterance, final in flight, timeout, disconnect during flush); detector on recorded Zoom frames with annotated builds and animations; batch/cursor logic; journal recovery with fault injection after every commit step; embed validation; SSE parser on split/partial/truncated streams; character-safe slicing; midnight; external edits; legacy migration.
- **Integration**: fake STT websocket replaying captured sessions with disconnects of 3, 15, 45 and 300 s; fake REST STT; fake SSE endpoint; all in `cargo test` without network.
- **Golden**: prompts and file formats byte-compared with `live_notes.py` output on fixtures.
- **UI**: 500-delta/s burst and a two-hour transcript fixture with p95 frame work < 16.7 ms; reload and hidden-window rehydration.
- **Manual**: packaged app with real Zoom for every source, permission flows, routing restore after kill, a full lecture with forced restart.

## 12. Milestones

| M | Deliverable | Gate |
|---|-------------|------|
| M0 | Contract fixtures + packaged native canary | A packaged `.app` gets mic and Screen Recording permission, captures real Zoom audio through BlackHole with routing preflight, writes a playable WAV that survives `kill -9`, captures one correct window image, restores routing; STT protocol fixture recorded (finalize spelling, timestamp origin) |
| M1 | Recording + loopback foundation (core + CLI) | 16/44.1/48 kHz inputs; interrupted WAV repaired; route restore after crash; no unexplained captured-audio gaps over 30 min |
| M2 | Streaming + recovery (core + CLI) | Exact outputs on protocol fixtures; disconnect suite with no duplicate or missing committed intervals; REST recovery exercised |
| M3 | Notes/session parity (core + CLI) | Golden prompts/formats; empty/truncated SSE; fault injection at every commit step; legacy migration; Python CLI refuses v2 folders |
| M4 | Desktop app: transcript + notes panes | Hydration after reload, hint/cancel/polish, burst and two-hour fixtures within budget, sanitised rendering with scoped images |
| M5 | Slide automation | Recorded Zoom fixtures: ≥95% recall of annotated stable states visible ≥3 s, ≤1 false capture per 10 min |
| M6 | Full-lecture acceptance; mixed mode; Python retired | Two-hour real lecture with restart, device removal, permission denial, disk and network failure all accounted for; zero unexplained missing or duplicate audio intervals; mixed mode passes the two-hour drift test or stays disabled; `live_notes.py` removed |

## 13. Dependencies

Candidates, pinned in `Cargo.lock` and verified together at M0: `tauri 2`, `tokio 1`,
`tokio-tungstenite` (rustls), `reqwest` (stream), `serde`/`serde_json`, `cpal 0.18.2`,
`rubato 5.0.0` (needs Rust ≥ 1.87), `hound 3.5.1`, `xcap 0.9.8`, `image`,
`coreaudio-sys`, `sha2`, `uuid`, `fs2` (advisory lock), `keyring`, `anyhow`/`thiserror`,
`tracing`. Frontend: `@tauri-apps/api` 2, `svelte` 5, `marked`, `dompurify 3.4.15`.
Deployment target macOS 13; toolchain pinned in `rust-toolchain.toml`.

## 14. Open questions

1. Loopback fallback if M0 routing fails: ScreenCaptureKit audio (`objc2-screen-capture-kit`, macOS 13+) or a Core Audio process tap (`AudioHardwareCreateProcessTap`, macOS 14.2+), which captures one app's output without a driver or output-device switching. Decide only on M0 evidence.
2. `grok-4.7`'s context window and per-request latency at 50k–150k tokens, to set the §6.1 budget from measurement.
3. Detector thresholds and cadence (§7.2) after calibration on real lectures.
