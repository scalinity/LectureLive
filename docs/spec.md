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
- G4 System audio via BlackHole loopback through an app-owned Multi-Output device that only Zoom plays into; any physical input; mixed mode opt-in once it passes the drift gate (§4.2).
- G5 Human-readable files compatible with the CLI's (Markdown notes, transcript, `slides/`), with exact resume.
- G6 Captured audio is durable to within the last one-second checkpoint after a crash, and every interval without durable audio or without committed transcript is reported as a gap.
- G7 `polish` and hint input as in the CLI.
- G8 A study page per lecture: polish also typesets the notes as one interactive HTML page, each slide redrawn as formulas, tables and diagrams and switchable back to its screenshot.

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
| Study page | One fixed template (`notes_template.html`) filled with model-typeset fragments in a small component vocabulary; parts cached against the notes | The design lives in one file while content varies per lecture; a template change reaches every lecture from its cached parts, at no cost |

## 3. Architecture

### 3.1 Workspace layout

```
LectureLive/
  Cargo.toml                 workspace, pinned toolchain (rust-toolchain.toml)
  crates/
    core/                    library: the whole pipeline, no UI
      src/audio/             sources, conversion, alignment, mixer, framer, recorder, routing
      src/stt/               websocket client, transcript state machine, REST recovery
      src/notes/             batch builder, prompts, SSE client, embed validation, polish, study page
      src/capture/           window enumeration, capture worker, change detector
      src/session/           coordinator, lecture folder, sidecar, commit journal, spend ledger, lock
      src/events.rs          notifications to adapters
    cli/                     headless binary on core, the `lecture` command (replaces live_notes.py at M6)
  apps/desktop/
    src-tauri/               commands → coordinator handle; notifications → events/channels
    src/                     Svelte 5
  notes_template.html        the study page design, read by live_notes.py and embedded in core at build time
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

The desktop adapter runs one lecture per window: at most one `session::lecture::run`, spawned on
Tauri's runtime, drives the session; a lecture folder can be open without one, and its notes and
transcript are then shown from the files. A `Pump` turns each lecture event into one message on one
of three streams:

- **status**, a Tauri event: the whole status (phase, folder, source, input level once a second, STT
  state, the notes operation running, open transcript gaps, start time, this lecture's spend today,
  the loopback silence warning), sent whenever any part of it changes; notices, the CLI's event lines
  in its words and marks; registered slides.
- **transcript**, a Tauri `Channel`: `open {utterance, stable, tentative}` for what is being said;
  `closed {utterance, segment}` when a live segment closes it; `segment {segment}` for a recovered one,
  which closes nothing. A segment carries its segment-log id and its time.
- **notes**, a Tauri `Channel`: `delta {op, text}`; `committed {op, revision, block}`, the verified
  block that replaces the preview; `ended {op, outcome, message}` for nothing new, a failure or a
  cancel; `polished {revision}`, after which the document is read again. Deltas and the result that
  ends them share an `op` and a channel, so their order holds.

Every message carries `session`, an id per opened folder or started lecture, and `seq`, one counter
across the three streams, assigned under the pump's lock. The lecture's `Committed` and `Polished`
events carry the notes revision their commit produced.

`get_session_state()` holds the pump's lock while it reads, so its `seq` is the watermark of
everything it returns: the sidecar (from the session's one writer while it runs, through
`Command::State`; from its file, which that writer saves atomically, once the lecture is stopping or
when none runs); the whole segment log; the notes file, read until its length and SHA-256 are the
sidecar's revision; and the pump's mirror of the live view: the open utterance, the preview so far,
the last 50 notices, the status. A reloaded frontend rebuilds itself from it without replaying
history.

The frontend drops a message whose session is not its own (a status message from a new session
makes it read the state again), whose `seq` is at or below the watermark, or at or below the last on
its stream. It appends a committed block only at the next revision and ignores one it already holds;
a revision jump, `polished`, or a hole in segment ids (the coordinator's notification channel can drop
under load) makes it read the state again.

Commands: `attach(transcript, notes)` on every page load, replacing the previous page's channels;
`get_session_state`; `select_folder`; `inputs`; `loopback_status`; `start_lecture(source)`; `stop`,
which answers the stop level; `snapshot(hint)`; `polish`; `cancel`; `open_page`; `spend_summary`;
`key_status`, `save_key` and `import_key_from_env`, none of which returns the key.

## 4. Audio

### 4.1 Sources and permissions

- **Input**: any CoreAudio input, persisted by device UID. The stream opens at a configuration the device supports (negotiated through cpal), not at a forced 16 kHz.
- **Loopback**: the "BlackHole 2ch" input, fed by the app-owned "LectureLive Loopback" device that Zoom's Speaker is set to (§4.3).
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

### 4.3 Loopback device (macOS)

Zoom's output has to reach both the headphones or speakers and BlackHole. The app owns one
Multi-Output device for this, Zoom's Speaker is set to it by name, and the system output is
never changed:

1. The device is a stacked (mirroring) aggregate with a fixed UID (`com.lecturelive.loopback`)
   and the name "LectureLive Loopback". Its members are BlackHole 2ch, which is the clock
   (it never disconnects, so taking off a pair of headphones cannot remove the clock), and one
   physical output, with drift correction on the physical output.
2. Setup creates the device, or rebuilds it under the same UID and name when the physical
   output changes, so Zoom's choice stays valid. The physical output is the current system
   output unless that is itself a multi-output device, in which case it is named explicitly.
   Setup runs on request only, never during a recording: rebuilding the device under a
   running meeting would move Zoom to another speaker.
3. In Zoom → Settings → Audio, Speaker is set to "LectureLive Loopback" once. The system
   output is the plain physical device (the headphones or speakers), never a multi-output
   device: macOS gives a multi-output device no volume control, and one that includes
   BlackHole sends every app's sound into the transcript. Because the system output is the
   same physical device that plays Zoom inside "LectureLive Loopback", the volume keys set
   Zoom's listening volume without changing what is transcribed. BlackHole's own output
   volume (Audio MIDI Setup) stays at maximum, because it sets the transcribed level; Zoom's
   speaker slider lowers that level too.
4. Preflight: play Zoom's Test Speaker and require a signal on the BlackHole input (loudest
   second above −60 dBFS). During a loopback recording, ten consecutive seconds below
   −60 dBFS raise a warning that names Zoom's Speaker setting. These are the only guards
   against Zoom's Speaker drifting to a physical device, which captures nothing.
5. A crash leaves nothing to undo: the device persists and the system output was never
   changed. The M0 canary can still make its own aggregate the system default; at launch, a
   route it left behind (saved state present) is undone, restoring the saved output only if
   the canary's aggregate is still the default. A user-made aggregate is never modified or
   deleted.

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

`wss://api.x.ai/v1/stt?model=grok-voice-transcribe-2.0&encoding=pcm&sample_rate=16000&interim_results=true&language=en`,
plus `&keyterm=…` once per keyterm, with `Authorization: Bearer`. Keyterms are validated (at most 100, each
1–50 characters) and URL-encoded one by one. The server endpoints by itself about 0.5 s after speech stops,
so no endpointing parameter is sent (`endpointing=400` is accepted and changes nothing measurable).

A refusal comes at the websocket upgrade as HTTP 4xx: an unknown key is 400 with a JSON body
(`{"code":…,"error":"Incorrect API key provided…"}`), an unknown model 404, a malformed parameter 400 with a
plain-text body. After `transcript.created`, audio goes as binary little-endian PCM16 frames of 100 ms, in
order, through a single writer. The server takes audio faster than real time: 10.5 s sent unpaced returned
the same finals within 0.7 s. Control messages are JSON text, `{"type":"finalize"}` and
`{"type":"audio.done"}`; bare text is answered with an `error` message and otherwise ignored, and the
connection stays open.

Times in server messages (`start`, `duration`, word `start`/`end`) are seconds of audio, to the
millisecond, from the first frame sent on that connection. Each connection is therefore an epoch anchored
at (recording, sample of its first frame), and a new recording always gets a new connection. While audio
flows the server sends a message at least every two seconds, speech or not: through 12 s of silence, an
empty interim every second and an empty chunk-final every two. Five seconds without one means the
connection is dead even if it has not closed.

### 5.2 Transcript state machine

Messages are `transcript.partial` with `is_final` and `speech_final`. Interims (`is_final: false`) arrive
about once a second with `words: []`; their text is the whole open utterance so far and `start` is its
start. Finals carry words. At a natural endpoint a chunk-final (`is_final: true, speech_final: false`)
comes about 0.5 s after speech stops and a `speech_final` about 0.5 s later, with the same text and words
and a duration that runs on through the endpoint silence; after a finalize or `audio.done` both arrive at
once. In silence the server sends empty interims and empty chunk-finals, which close nothing.
`transcript.done` has empty text and the total duration: the client owns the whole transcript.

State: closed utterances + open utterance (stable chunks + tentative tail).

- `is_final=false`: replaces the tentative tail only (less the stable text it repeats). An empty hypothesis never erases stable content.
- `is_final=true`: its range `[start, start+duration)` replaces the stable chunks it overlaps; an empty final adds nothing.
- `speech_final=true`: closes the open utterance. Its text is the stable part after that replacement, which with the recorded protocol is the `speech_final`'s own text. The utterance is its `[start, start+duration)` of the recording and produces exactly one segment, appended once to the segment log and the transcript file (an empty one produces none). A final ending at or before the last close is a repeat and is ignored.

The transcript is settled through the end of the last close. UI stability is not disk durability: the
sidecar records the open interval of every recording whose live transcript is not finished, from its
first sample until a connection streams it and then from that connection's origin (the next recording
can open while the last is still being flushed). After a crash, each one's audio after its last logged
segment becomes a gap recoverable from the recording.

### 5.3 Snapshot cutoff

`snapshot` records an audio cutoff (the end of the last frame forwarded to the STT writer), and the writer
sends all frames through the cutoff, then `{"type":"finalize"}`, then continues with live audio. The server
answers every finalize with a `speech_final` that ends exactly at the cutoff, within 40–250 ms: the open
utterance, or with nothing open an empty one of duration 0. The coordinator waits (3 s) for the close that
settles the transcript through the cutoff; when it is already settled it does not ask. On timeout, or when
the cutoff's audio is not on the live connection (connecting, disconnected), the unconfirmed interval stays
pending and the UI reports "snapshot of confirmed material; transcription still catching up". Recording
never pauses and no lock is held while waiting.

### 5.4 Reconnection and recovery

Frames reach the writer through a bounded queue that keeps room for control messages: a full queue drops
frames and never waits, and the writer, seeing the hole in sample offsets, ends the epoch. On an unexpected
close, a send that fails or stalls (5 s), five silent seconds, or dropped frames, the writer reconnects
live-first with backoff (1, 2, 4 … 30 s). While connecting it holds the newest 5 s of frames and sends them
first, so a short outage costs only its unclosed utterance; the new epoch begins at the oldest held frame.
The gap is the interval from the last close to the new epoch's origin (including handshake time and any
unclosed utterance), written to the sidecar together with the new origin. Gap kinds name the cause:
`stt_offline`, `stt_overflow`, `stt_refused`, `stt_interrupted` (the session stopped first).

Gaps are recovered by the REST endpoint over the recorded interval, owned by the recovery path alone, so
live and recovery commits are disjoint: `POST https://api.x.ai/v1/stt`, multipart `file` (a WAV of the
interval), `language=en`, `format=true`, `keyterm` per keyterm (not observed to take effect). The answer is
`{text, language, duration, words:[{text,start,end}]}` with times from the clip's start (silence: empty
text, no `words`). The interval goes in pieces of at most 30 s, each cut at the quietest 100 ms of its last
10 s; each piece with speech is one segment, and the pieces tile the gap. Recovery resumes after the pieces
already in the segment log, so a crash mid-recovery writes no line twice. Recovered segments enter the
segment log when they arrive and are eligible for the next snapshot even though their speech times precede
earlier snapshots.

A gap is resolved when nothing more can be done for its interval: a transcript gap once recovery has
committed all of it, a gap with no audio behind it (capture or recorder overflow, device gone, rate change,
interrupted) when it is recorded. A recording is therefore kept (§4.4) exactly while it holds a transcript
not yet recovered. Auth and parameter errors (4xx) stop STT without a reconnect loop, and stop recovery for
the session; recording continues and their gaps wait for a later session. On stop: drain the framer, send
`audio.done`, process remaining finals until `transcript.done` (5 s), ignore `transcript.done`'s text for
commit purposes, finalize the recording, recover what is pending, take the last snapshot.

## 6. Notes

### 6.1 Batches and prompts

A snapshot takes an immutable batch: segment-log positions from the committed cursor up to the
count its cutoff settled at (§5.3), registered slides after the committed slide index, and the
hint. It does not wait for recovery still pending: recovered segments enter the log when they
arrive and join the next snapshot, and the last snapshot of a session runs after it has drained
recovery (§5.4). Between recordings a cutoff is confirmed only when no recording's live
transcript is still being flushed; otherwise the snapshot takes what is logged and reports
"transcription still catching up". The prompts are `live_notes.py`'s `notes_system` /
`polish_system` and its user messages, carried over unchanged; golden tests compare the system
prompts with its f-strings evaluated from its source.

Timeline: segments and slide markers ordered by full timestamp with a deterministic tie
rule (slide before speech at equal time), in the CLI's line format: `[HH:MM:SS] text` and
`[HH:MM:SS] >>> Slide N shown (embed: ![Slide N](path))`. A streamed segment spanning a slide's
first-shown time is split at that point using word timings, for the prompt only. Lines imported
from the Python CLI are segments of source `imported`: second resolution, no words, never split.

Context: the whole document is sent while the request fits the budget of 200,000 tokens,
`grok-4.7`'s long-context threshold (its window is 500,000; above the threshold every token of
the request is billed at twice the rate). The estimate counts text at three characters per
token, 1,800 tokens per slide and 16,000 for the output. Above the budget, the prompt carries the
omitted prefix's own `#` headings, then the most recent part verbatim from a line start. The
outline comes from the document itself: no summary request, nothing to cache. Text is cut at
line starts, never inside a character.

### 6.2 Streaming, validation, commit

Every chat request streams (`stream: true`) and asks for `stream_options: {"include_usage": true}`;
without it a stream reports no cost. The server sends one `data: {json}` event per chunk:
reasoning deltas (`delta.reasoning_content`) first, then content deltas (`delta.content`), then a
chunk with `finish_reason`, then one chunk with `choices: []` and `usage` (with
`cost_in_usd_ticks`), then `data: [DONE]`. SSE parsing handles split UTF-8 (a line is decoded
only once its newline has arrived), partial and multiple events per read, comments and CRLF;
only content deltas reach the preview. A stream silent for 180 s is broken. A response is
successful only with `finish_reason: "stop"` and non-empty output: a `length` or filtered
finish, an error event, an empty answer or a stream that ends before its finish all fail. A 4xx
before the stream is a refusal (the key or a parameter), reported with the server's message.

Validation, after stripping a wrapping code fence and any `#` title: each expected embed line
appears exactly once, on its own line, outside code fences. A later own-line copy is removed;
inline copies are removed, since an image in the middle of a bullet is not placed; embeds of
slides not in the batch are removed; what fences hold is code and stays as written. Expected
embeds still absent are appended under `### Slides not placed`, each on its own line, without an
invented description.

Commit (through the sidecar's one writer, one at a time):

1. Accept any external edit as the new revision, then write the journal atomically
   (`.live_notes/<stem>.journal.json`; tmp + rename + fsync): `op_id`, the segment and slide
   cursors before and after, notes length and SHA-256 before, the block, its length and SHA-256.
   A journal already there (an earlier commit in this session failed) is recovered first; if that
   recovery finds the earlier block complete, this batch is stale and the commit refuses, to be
   taken again from the new cursors.
2. Append `\n<!-- HH:MM:SS -->\n{block}\n` to the notes file and fsync; the time is when the
   snapshot was asked for.
3. Advance the revision, the notes' fingerprint and the cursors in the sidecar, and save it atomically.
4. Delete the journal.

Recovery on launch with a journal present, before any external edit is accepted: if the notes no
longer begin with the recorded "before" (length and hash), stop and change nothing. Otherwise:
nothing after it → not appended, material stays pending; exactly the block (length and hash) →
complete, the cursors advance unless step 3 already did; a shorter tail that is the block's own
start → a torn append, truncated to "before", material stays pending; any other tail (text typed
by hand after the crash, a tail longer than the block, or one of its length with another hash) →
stop and change nothing. Recovery that goes on saves the sidecar and deletes
the journal; a stop leaves the journal for the person to look at. Nothing is truncated without
a verified prefix.

Failure, truncation or cancellation leaves the document untouched and the batch pending.

### 6.3 Polish

Runs only after a successful snapshot of all pending material; if that fails, polish stops
before its own request. The request carries the notes and the whole transcript; the answer
keeps its title, and every embed of the notes appears exactly once (§6.2's rules, with the
notes' own embeds as the expected set). If the notes changed while the request ran, nothing is
written: an edit is never overwritten from memory. Otherwise the previous document is copied to
`.live_notes/<stem>_HHMMSS.md` (`_2`, `_3` … when that name exists) and fsynced, the result is
written to a temp file and atomically renamed over the notes file, and the revision advances.

### 6.4 Study page

A successful polish distils the polished notes into an HTML study page beside them, named
after the lecture: the lecture folder's title without its week prefix (`Week 06 —
Statistical Analysis Methods` gives `Statistical Analysis Methods.html`), with the date
added only when the folder holds more than one day's notes. The same step runs on its own
from the notes (`lecture page`, or the app's Open study page when the page is missing or
stale), without polishing again.

The page is a condensation, not a copy: the notes stay the complete record, and the page
holds what a student needs to revise. One chat request carries the whole notes and every
slide image, at medium reasoning effort (at the default, high, one request over a
two-hour lecture ran past ten minutes), so what earns space is decided across the whole lecture: flagged and
examinable points first, then the formulas and decision rules needed to solve problems,
then at most one worked example per method cut to its essential steps, then terms new in
this lecture. Reviews of earlier weeks, digressions, logistics, repetition and software
walkthroughs are cut. The budget is 30% of the notes' words, between 600 and 2,500, split
about 65% topics, 15% glossary and takeaways, 20% questions. Visible words are counted
from the output (drawings excluded); a draft more than 10% over budget gets one revision
request, without images, that cuts the lowest-yield material, and the final count is
reported with the page. At most 30% of the slides (2–8) are redrawn, only where a figure,
table, diagram or formula block teaches faster than words and only the part that
matters; every slide stays reachable in a collapsed All slides gallery. Prompts are
`live_notes.py`'s `page_system` and `revise_system`, carried over with golden tests.

The output runs keystone (the lecture's governing formula or idea, set large at the top),
a two-to-three-sentence lede, 4–7 topic sections, a glossary of at most 12 terms, at most
5 key takeaways, and 4–5 check-yourself questions with brief worked answers. It is written
in a fixed vocabulary the template styles: TeX math in `\( \)` and `\[ \]`; `formula`
(named, collected into the formula sheet), `flag` (examinable, at most 4), `example` with
numbered `steps`, `map` (situation → method), `takeaways`, `glossary`, and
`figure.redraw[data-slide=N]` holding display math, a table (highlighted groups as cells
`a`/`b`) or an inline SVG drawn only with the template's classes. Sections are re-cut at
every `<h2>`, so the page's structure never depends on the model's own wrappers;
`<script>`, `<style>`, event and style attributes are stripped.

Each request gets one retry (a refusal gets none); if it still fails, no page is written and
the notes are untouched. The typeset page is stored in `.live_notes/<stem>.page.json` keyed by
the SHA-256 of the prompt and the notes: a change to either typesets again, while a change to
the template alone re-renders every lecture without requests. The file is the Python CLI's
own, `{"source": <key>, "fills": {"keystone", "summary", "content", "slides"}, "words", "budget"}`
written as Python's `json.dumps` writes it, so a page either tool typeset re-renders in the
other without a request. Over a 2,000-word synthetic lecture one medium-effort request took
about nine minutes. The page is filled in one
pass (generated text is never read as a placeholder) and written atomically. Slide
screenshots are embedded as JPEG data URIs (quality 80, subscripts still legible), read
fresh from `slides/` at each fill while the cache keeps only their paths: the page is one
self-contained file, because a browser handed only the page may not read the folder
around it.

The template owns all styling and behaviour: a contents rail in lecture order marking
each section's slides and flagged points, a switch on every redraw back to its
screenshot, self-test mode (definitions, takeaways, example steps and formulas hidden
until clicked), glossary definitions on the first use of each term per section, a formula
sheet, questions with folded answers, and the All slides gallery. It works in light and dark, at phone width and
in print. Changes to the template's design go through `/frontend-design:frontend-design` (§9.4). Math (KaTeX) and fonts load from the web with pinned versions and integrity
hashes; offline the page stays readable with math shown as TeX.

## 7. Slide capture

### 7.1 Window and region

A capture worker runs beside the session on its own thread, because its native calls and image
work block (§3.2). Windows are enumerated through CoreGraphics (`CGWindowListCopyWindowInfo`), on
screen or not, and captured by their id (`CGWindowListCreateImage` on the window alone), so a window
that is covered, or full screen on another desktop while the person works elsewhere, is still
captured, for as long as its app keeps drawing it there (a web page pauses its transitions and
animations while unseen). A missing Screen Recording grant is an error of its own, never "no windows". The app is
named by its bundle id, or by its name outside a bundle.

The person chooses a window and drags the slide region over a still of it (taken by id, so a window
on another desktop has one too), leaving out Zoom's controls and the video tiles. A speaker's camera
drawn over the slide, as Zoom's recordings do, is dragged over in the same still as a part left out,
which the detector never reacts to; a part stays on the window where it was drawn when the region
is redrawn, and choosing the same window again keeps the parts it has. The selection, a descriptor
(bundle id, title, size), the region as fractions of the window, the parts left out as fractions of
the region, and the region at each other size the window has had, is saved per course in the app's
data folder (`capture.json`).

When a lecture starts, exactly one window matching the descriptor (at a size it has had, within 2%)
is watched. Anything else asks, in the slides strip. "Watch it" is offered only for a window the
selection matches, whose region at its size someone has chosen or found; any other window is chosen
in the picker, where its region is drawn. Once a window is watched, nothing is watched in its place
without the person:

- it cannot be captured while off screen (minimised) for three samples in a row: capture pauses and
  resumes by itself when it is back. A moment off screen, as full screen animates, is not a pause;
- it closes: capture pauses; a window matching the descriptor that opens, or is already on screen
  while the old one is off screen (a closed window's id can stay listed), is offered with "Watch it";
- it changes size by any amount (full screen on or off, even within the 2% that names the same
  window, moves the slide): once the new size has held for a sample, the region found or chosen at
  exactly that size is used; otherwise the last kept slide is searched for in the new layout and,
  when found closely, its place becomes the region for that size, saved and announced (at a size
  within 2% of one it had, that region is used if the search finds nothing). A frame through the new
  region that still shows the kept slide (no tile past twice the change threshold, since realigning
  fine text moves a tile by up to about 0.06) becomes the kept frame, as do the two samples after
  it, so a switch never takes the same slide twice; a different slide, one that changed as the
  window did, is taken. A one-line build landing in the same second as a switch can be taken for the
  kept slide. A slide that cannot be found asks, and is searched for again every 5 samples, since the
  new layout may not be drawn yet;
- captures fail (an error, a blank window, a black region): nothing is kept, and three in a row are
  shown with the reason.

### 7.2 Change detection

Every 1.0 s: capture, crop to the region, blank the parts left out, downscale to 256×144 grayscale,
divide into 16×16 tiles (144 tiles). A tile has changed when its mean absolute difference from the
last kept frame exceeds 0.05, and it is moving when it differs from the previous sample by more than
0.03.

- **Candidate**: at least one changed tile outside the mask. Its image and first-observed time are kept.
- **Confirm**: on a later sample in which no tile outside the mask moved since the candidate, the
  candidate becomes a slide (the region at full size, at most 1600 px, PNG). A candidate whose
  changed tiles return to the kept frame is dropped.
- **Animated tiles**: a tile moving on 3 consecutive samples is masked until the next kept slide, so
  a looping animation neither blocks nor triggers capture.
- **Expiry**: a candidate that has not settled after 10 samples is kept, flagged `uncertain`.
- The first valid frame of a lecture is kept unconditionally.
- A manual capture becomes the kept frame, so auto capture does not take the same slide again.

The thresholds are calibrated on two recordings of the detector's input made by the worker itself: a
synthetic lecture deck with builds, dissolves, animated charts and a blinking caret, played in a
window and annotated from its schedule, and a recorded Zoom lecture, annotated by its still stretches
and checked by eye. The measures are recall of states visible for at least 3 s and false captures per
10 minutes (M5 Findings). A line of text on a slide changes its tiles by about 0.06, hence 0.05.
Content shown for less than one sample interval can be missed, and marks thinner than about 1% of
the slide's height (an underline) change too little of a tile to count; manual capture covers both.

### 7.3 Manual and imported

The Capture button, the global shortcut ⌘⇧2 (registered only while a lecture runs), images dropped
on the window, and screenshots taken with macOS (⌘⇧4, from its screenshot folder) all go through the
one registration path and index allocator that auto capture uses. A file reaches `slides/` under a
temporary name, and registration renames it to `slide_NN_HHMMSS.png|jpg` inside the sidecar's
writer. A file already registered is never registered again. Dropped images are copied, so the
originals stay where they were, and take their own file time. Each slide records whether it was
auto and whether it was uncertain.

### 7.4 Permissions

Screen Recording (TCC), verified in the packaged app; a binary run from a terminal uses the
terminal's grant. Without it the strip says so and offers System Settings. The Capture button works
once a real capture of the watched window has succeeded.

## 8. Files, state, resume

```
<lecture folder>/
  lecture_notes_YYYYMMDD.md           append-only during class; <!-- HH:MM:SS --> markers
  <lecture title>.html                the study page, named after the lecture (§6.4)
  lecture_transcript_YYYYMMDD.txt     [HH:MM:SS] text, append-only, in commit order;
                                      --- started|resumed HH:MM:SS --- at each session
  slides/slide_NN_HHMMSS.png|jpg
  recordings/session_YYYYMMDD_HHMMSS.wav
  .live_notes/<stem>.v2.json          sidecar: version, lecture date, recordings + anchors, gaps,
                                      open utterances, notes {revision, len, sha256,
                                      segment_cursor, slide_index}, slides [{index, file, shown_at}]
  .live_notes/<stem>.segments.jsonl   segment log: id, recording, samples, wall times, text, words,
                                      source (live | recovered | imported)
  .live_notes/<stem>.journal.json     pending commit, present only mid-commit
  .live_notes/<stem>_HHMMSS.md        polish backups
  .live_notes/<stem>.page.json        typeset study page parts, keyed by prompt and notes (§6.4)
  .live_notes/lock                    exclusive advisory lock (GUI and CLI)
```

- Resume is by cursor: segment-log position and slide index, both in registration order. No timestamp comparison decides what is committed.
- The lecture date is fixed when the folder's files are created and does not change across midnight; full timestamps live in the sidecar and segment log.
- Custom notes/transcript/slides paths, course name, JPEG and PNG slides, numbering from the highest existing index, and file-mtime capture time for imported images are supported as in the CLI. State files are named after the notes file's stem.
- Before append or polish, the notes file's length and hash are checked against the sidecar's revision; an external edit is accepted as the new revision, never overwritten from memory.
- The sidecar has one writer: the session coordinator while a session runs, which runs other parts' updates (commits, polish, slide registration) in turn and answers only after saving; the file itself when none runs.
- Opening the segment log puts back the last segment's transcript line when a crash came between the log's sync and the transcript's (missing, or cut short).
- Before a session starts, every other day's sidecar in the folder that holds unresolved transcript gaps gets a recovery-only session; its segments reach that day's transcript, and that day's notes stay as they are.

Initialisation cases:

| Folder state | Action |
|---|---|
| Empty | Create notes with title, empty sidecar |
| v2 sidecar present | Resume: journal recovery first, then any external edit is accepted, then slide files dropped in while no session ran are registered |
| v2 sidecar corrupt | Stop, naming `--rebuild`. With it, the corrupt file is kept beside it as `<name>.corrupt-HHMMSS` and the state rebuilt: transcript lines and slides after the last `<!-- -->` marker are pending; an existing segment log is kept and its cursor set the same way |
| Legacy `<stem>.json` only | One-time migration. The CLI's half-written snapshot (a `commit` entry) is finished or undone as its `recover_commit` does. Transcript lines become `imported` segments (rolled to the next day once the clock runs back past midnight) and slide files registered slides; lines before `transcript_offset` (for the older `noted_through`, before the first line at or after it) and slides up to `slide_index` count as noted. The legacy file stays; v2 is written last |
| Notes but no state | Rebuilt as for a corrupt sidecar |
| Transcript or slides but no notes | Create notes; everything existing is pending |

Legacy writer: the Python CLI exits in any folder whose `.live_notes/` holds a v2 sidecar, so the
two writers never share a folder; its `spend` still works everywhere.

Spend ledger: every paid request appends one JSON line to `spend.jsonl` in the app's data
directory, fsynced: time, course, lecture folder, kind (`transcribe`, `notes`, `polish`,
`page`), USD, `billed`, and for transcription the audio seconds. The format is the CLI's, byte
for byte: Python's `json.dumps` with its defaults (`", "` and `": "` separators, non-ASCII as
`\uXXXX`, floats as Python's `repr`), keys `at, course, lecture, what, usd, billed[, audio_s]`,
`usd` rounded to 6 decimals and `audio_s` to 1. `billed` is true when the cost is the response's
own `usage.cost_in_usd_ticks` (10^10 ticks per dollar), which a streamed chat response reports
once, in its usage chunk after the finish; false when it is computed from published rates. Speech
to text carries no cost: one `transcribe` line per recording for the audio its connections
carried ($0.20 per hour), and one per recovery request ($0.10 per hour). A response whose usage
chunk arrived is recorded whether or not its text is used (a `length` answer is billed); a request
that reported no cost records nothing, and so does a cancelled stream, whose cost would have come
only at the end. A ledger that cannot be written is a warning (§10). The app takes over the
CLI's ledger at each start: the complete lines it gained since the last take-over are appended,
tracked by `spend.import.json`; a crash between the append and that mark is caught by the app
ledger already ending with exactly those lines. A last line cut short by a crash is skipped on
read, and the next line starts on a line of its own.

## 9. UI (Svelte 5, Tauri 2)

### 9.1 Layout

- **Transcript** (left): closed utterances as plain paragraphs, each with its time in a hanging
  gutter; a recovered one is marked there. The utterance being said sits below them, one step
  larger, on a teal rule: stable words in ink, the tentative tail in graphite. A new word fades in
  (~180 ms); a tentative word turning stable changes colour without remounting. The pane follows the
  newest words while the reader is at its end; scrolling up stops it and shows "Jump to live".
- **Notes** (centre): the committed document, split at its snapshot markers, each part rendered once
  and frozen, with its time in the same gutter. The streaming preview renders below it on the teal
  rule, marked "writing"; each finished block fades in once, and the committed block replaces it.
  The pane follows the newest writing while the reader is at its end.
- **Slides** (right strip): the watched window on top, on a teal rule while it is watched, with
  Capture (⌘⇧2) and Choose… for the window and region picker; a pause, a question or a failure is said
  there in words. Below it, each slide in time order: its time in the gutter with "auto", "manual" or
  "unsettled" under it, and its thumbnail; images dropped on the window join them. Below 1100 px the
  strip folds away.
- **Status strip** (top): the phase in one word (Ready, Starting, Listening, Stopping, Stopped) with
  what it means while it lasts; a static red dot while recording; course › lecture; the source with a
  level meter (−60 to 0 dBFS, once a second, red while ten silent seconds on loopback last); elapsed
  time; STT state; open transcript gaps; this lecture's spend today from the ledger (§8), which opens
  the spend view; a large-type toggle; the API key.
- **Command line** (bottom), in the CLI's grammar: an empty ⏎ takes a snapshot, a hint then ⏎ a hinted
  snapshot, `polish` ⏎ a polish. Snapshot; Cancel, only while a snapshot or polish runs; Polish; Study
  page (typesets first when the page is missing or stale, §6.4, then opens it in the default browser);
  Stop. The latest notice sits above the prompt in the CLI's marks (◆ notes, ▣ slide, ✦ page, ✓ done,
  ▲ warning) and opens the last eight; a command that fails shows the backend's message there. Before
  a lecture the same bar holds the folder picker, the source (the inputs, and Zoom through "LectureLive
  Loopback" when BlackHole is present, else the install hint) and Start.
- **Stop** has the CLI's three levels. Stop finishes the transcript and recovery, then takes the last
  snapshot; while that runs the button becomes Stop waiting, which stops waiting for recovery and drops
  queued requests; quitting the app is the third level, and the next session in the folder repairs,
  recovers and notes what is left.
- **Cancel** stops the notes request in flight and the requests queued behind it: nothing is written,
  the batch stays pending, and the next snapshot sends the same material (§6.2). The study page is not
  cancellable.
- **Spend** (from the strip): a sheet beside the lecture with the CLI's `lecture spend` figures: all
  time, the last three months by course with bars, the eight most recent lectures by kind, and the
  share estimated from published rates rather than billed.
- **API key**: a dialog that stores the key in the Keychain (or moves the CLI's `GROK_API_KEY` there),
  opened at start when none is stored.

### 9.2 State and lifecycle

One `session.svelte.ts` rune store. Initialisation registers the status listener and both channels
before it awaits anything, then calls `get_session_state()` (§3.6); messages that arrive meanwhile wait
and are replayed above its watermark. Messages are applied once per animation frame: the frame applies
its queue, flushes the DOM, then runs the panes' hooks (pinning), so they measure what is on screen.
Unlisten handles are kept and disposed on hot reload. When the window is hidden (`visibilitychange`,
which WKWebView fires when a Tauri window hides and shows), the store stops applying messages and
discards them; on return it reads the state again. Commands go through the store; the key never
reaches it.

### 9.3 Rendering

Words carry explicit IDs: an unchanged prefix keeps its IDs, replacements get new ones, promotion from
tentative to stable does not remount. The preview accumulates deltas as text and is shown at most
every 100 ms, up to its last whitespace, so an unfinished trailing word waits for the next delta; it is
split into top-level Markdown blocks, all but the last finished and parsed once. Committed parts are
parsed once. Closed transcript paragraphs use `content-visibility: auto`, so a two-hour transcript lays
out only what is on screen.

Markdown goes through `marked`, then DOMPurify (no scripts, frames, forms, SVG, MathML, styles, event
handlers or `srcset`), then one pass that decides every URL: an image renders only when its path
resolves under the notes folder to a registered slide, through Tauri's asset protocol; a link keeps
only an in-page anchor; every other URL attribute is removed. The asset protocol's scope is empty until
a lecture folder is opened, which allows that lecture's `slides/`, not recursively.

The CSP is `default-src 'self'; script-src 'self'` with a nonce, `style-src 'self' 'unsafe-inline'`
(Svelte and Vite inject styles), `img-src 'self' asset: http://asset.localhost`, `connect-src 'self'
ipc: http://ipc.localhost` and the dev server's websocket, `font-src 'self'`, and `'none'` for
objects, frames, `base-uri` and form actions. SvelteKit's `kit.csp` sends it (a header in development,
where the webview loads the dev server directly; a meta tag in a built page), and `tauri.conf.json`
gives the custom protocol of a built app the same directives.

Frame work, the time to apply a frame's messages, flush the DOM and read the panes' layout, stays under
16.7 ms at the 95th percentile on a 500-delta/s burst and on a two-hour lecture streaming live,
measured in the app's WKWebView.

### 9.4 Visual design

All UI work invokes `/frontend-design:frontend-design` before any markup is written:
the desktop app (`apps/desktop/src/`), the study page template (`notes_template.html`,
§6.4), and any other page or view a person looks at. That includes changes to an
existing view, not only new ones. The design is readable at arm's length, light and
dark, has a large-type toggle, and has no decorative motion beyond the fade-ins that
carry meaning.

The app takes the study page's own palette (paper, ink, graphite, rule, plate, signal red, teal), in
light and dark after the system, so the app and the page are one family. Type is Atkinson Hyperlegible
Next when installed, else the system face, on an 18 px base with a 1.2 ratio; the large-type toggle
scales it by 125%. Times and money use tabular figures in the same face. Panes are flat columns divided
by rules, with no cards or shadows. A hanging gutter holds when things happened, and a teal rule marks
what is live. The command line is the one bold element.

## 10. Errors and robustness

| Failure | Behaviour |
|---------|-----------|
| Network down / websocket error | Live-first reconnect with backoff; gap recorded; REST recovery when online; recording continues |
| STT 4xx | Stop STT, surface message, keep recording |
| Notes call fails, truncates or is cancelled | Document untouched; batch pending |
| Study page part fails after its retry | No page written; notes untouched; typesetting can run again from the notes without polishing |
| Crash mid-commit | Journal recovery on launch (§6.2) |
| Microphone or Screen Recording denied | Affected feature disabled with fix-it button; the rest works |
| BlackHole missing | Loopback disabled with install hint (`brew install blackhole-2ch`) |
| Loopback preflight fails | Reason shown: Zoom's Speaker is not "LectureLive Loopback"; ten silent seconds during a loopback recording raise the same warning |
| Device disappears | Mixed: surviving source continues, gap marked. Single: source stops, fallback offered |
| Sample-rate change | Stream rebuilt, new timing segment, gap marked |
| Disk write error | Session stops cleanly, path surfaced |
| Spend ledger write fails | Warning in the status; the request's result is kept and recording continues |
| App crash | Recording valid to last checkpoint and repaired at launch; system output untouched; journal recovery; unclosed utterance becomes a gap |
| External edit of notes | Accepted as new revision |

## 11. Testing

- **Unit (core)**: framer across 16/44.1/48 kHz; drift alignment over simulated clocks; mixer gain/headroom; transcript state machine on recorded protocol logs incl. duplicate and late finals, empty hypotheses, continuous speech; finalize cutoff cases (no open utterance, final in flight, timeout, disconnect during flush); detector on recorded Zoom frames with annotated builds and animations; batch/cursor logic; journal recovery with fault injection after every commit step; embed validation; SSE parser on split/partial/truncated streams; character-safe slicing; midnight; external edits; legacy migration; spend ledger (billed and computed costs, a crash-truncated last line); study page (budget from the notes, word count excluding drawings, revision on overshoot, section re-cut, fragment stripping, cache hit and miss on prompt and notes, single-pass template fill).
- **Integration**: fake STT websocket replaying captured sessions with disconnects of 3, 15, 45 and 300 s; fake REST STT; fake SSE endpoint; all in `cargo test` without network.
- **Golden**: prompts and file formats byte-compared with `live_notes.py` output on fixtures.
- **UI**: 500-delta/s burst and a two-hour transcript fixture with p95 frame work < 16.7 ms; reload and hidden-window rehydration.
- **Manual**: packaged app with real Zoom for every source, permission flows, routing restore after kill, a full lecture with forced restart; the study page of that lecture checked in light and dark, at phone width, and against each slide's screenshot.

## 12. Milestones

| M | Deliverable | Gate |
|---|-------------|------|
| M0 | Contract fixtures + packaged native canary | A packaged `.app` gets mic and Screen Recording permission, captures real Zoom audio through BlackHole with routing preflight, writes a playable WAV that survives `kill -9`, captures one correct window image, restores routing; STT protocol fixture recorded (finalize spelling, timestamp origin) |
| M1 | Recording + loopback foundation (core + CLI) | 16/44.1/48 kHz inputs; interrupted WAV repaired; a crash leaves the system output untouched; loopback preflight through the app-owned device; no unexplained captured-audio gaps over 30 min |
| M2 | Streaming + recovery (core + CLI) | Exact outputs on protocol fixtures; disconnect suite with no duplicate or missing committed intervals; REST recovery exercised |
| M3 | Notes/session parity (core + CLI) | Golden prompts/formats; empty/truncated SSE; fault injection at every commit step; legacy migration; Python CLI refuses v2 folders; a fixture lecture distils to a study page within its budget |
| M4 | Desktop app: transcript + notes panes | Hydration after reload, hint/cancel/polish, burst and two-hour fixtures within budget, sanitised rendering with scoped images |
| M5 | Slide automation | Recorded Zoom fixtures: ≥95% recall of annotated stable states visible ≥3 s, ≤1 false capture per 10 min |
| M6 | Full-lecture acceptance; mixed mode; Python retired | Two-hour real lecture with restart, device removal, permission denial, disk and network failure all accounted for; zero unexplained missing or duplicate audio intervals; mixed mode passes the two-hour drift test or stays disabled; `live_notes.py` removed |

## 13. Dependencies

Pinned in `Cargo.lock`; the versions that build together are recorded in
`milestones.md` at M0: `tauri 2`, `tokio 1`, `tokio-tungstenite` (rustls), `reqwest`
(stream), `serde`/`serde_json`, `cpal` (0.18 line), `rubato` (fixed-ratio resampling
from M0; the asynchronous resampler that mixed mode needs is adopted at M6),
`hound 3.5`, `xcap` (0.9 line), `image`,
`coreaudio-sys`, `sha2`, `uuid`, `fs2` (advisory lock), `keyring`, `anyhow`/`thiserror`,
`tracing`. Frontend: `@tauri-apps/api` 2, `svelte` 5, `marked`, `dompurify 3.4.15`.
Study page, loaded by the page rather than built: KaTeX 0.18.9 from jsDelivr with SRI
hashes, Atkinson Hyperlegible Next and Mono from Google Fonts.
Deployment target macOS 13; toolchain pinned in `rust-toolchain.toml`.

## 14. Open questions

1. Loopback fallback if M0 routing fails: ScreenCaptureKit audio (`objc2-screen-capture-kit`, macOS 13+) or a Core Audio process tap (`AudioHardwareCreateProcessTap`, macOS 14.2+), which captures one app's output without a driver or output-device switching. Decide only on M0 evidence.
2. `grok-4.7`'s context window and per-request latency at 50k–150k tokens, to set the §6.1 budget from measurement.
3. Detector thresholds and cadence (§7.2) after calibration on real lectures.
