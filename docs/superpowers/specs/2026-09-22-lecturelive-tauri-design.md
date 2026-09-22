# LectureLive companion app — proposed architecture

Status: proposal for review, 2026-09-22. Nothing in this document is implemented yet;
the Python CLI in `live_notes.py` is the current tool and the behavioural baseline.

## 1. Context and goals

### 1.1 What exists

`live_notes.py` was written during a live lecture and is in daily use. It captures
the microphone, posts 8–30 s chunks (cut at pauses) to Grok Voice Transcribe's REST
endpoint, appends `[HH:MM:SS] text` lines to a transcript, watches the macOS
screenshot folder for slide captures, and on Enter sends the notes document so far
plus the new transcript and slides to `grok-4.7`, appending only the new notes. It
resumes losslessly after a restart and can rewrite the document into a study document
(`polish`).

### 1.2 Problems observed in real use

| # | Problem | Evidence |
|---|---------|----------|
| P1 | Transcript arrives in 10–30 s blocks; no live feel and the snapshot boundary lags the speaker | REST chunking; the CLI has to force-cut and drain a queue before every snapshot |
| P2 | Audio is captured acoustically (speaker → room → mic) although the lecture is a Zoom stream already inside the machine | accent + room acoustics produced errors like "pre-chain data" for "pretrained data" |
| P3 | Slides are captured only by manual keyboard shortcut | user must notice each slide change while listening |
| P4 | An API failure loses speech; nothing records the audio | retries exist, but there is no ground-truth recording |
| P5 | Runtime coupled to an unrelated project's virtualenv (fixed in this repo by `pyproject.toml`) | original invocation path |
| P6 | Terminal UX: hints typed into the same stream the transcript prints to; notes printed in full to the terminal | |
| P7 | Pipeline is closures over shared lists with three threads; the parts most likely to be subtly wrong (chunk cutting, resume filter, embed guard) are not unit-tested | |

### 1.3 Goals (v1)

- G1 Live transcript with word-level fade-in as words are recognised, tentative words visibly distinct from committed ones.
- G2 Snapshot → notes streamed token-by-token into the document pane; file written only when the response completes.
- G3 Auto-capture of a chosen window (the Zoom window) on slide change; manual capture still available.
- G4 Audio source picker: system audio via BlackHole loopback, any physical input (e.g. a wireless mic receiver), or both mixed; automatic output routing on start/stop.
- G5 Same on-disk format as the CLI (Markdown notes, transcript, `slides/`, `.live_notes/`), same resume rules, so the CLI and app are interchangeable on a folder.
- G6 Always record the session audio to disk so nothing said is ever lost.
- G7 `polish` and hint input as in the CLI.

### 1.4 Non-goals (v1)

Two-channel labelled transcription (lecturer vs. me), in-app editing of the notes,
automatic key-term extraction from slides, a past-lectures browser, cloud sync,
accounts, telemetry, Windows/Linux.

## 2. Decisions already taken

| Decision | Choice | Why |
|----------|--------|-----|
| Pipeline runtime | Rust, inside the Tauri process | No Python at runtime; streaming, audio and window capture are native here; one binary |
| Frontend | Svelte 5 + TypeScript, Tauri 2 | Runes handle token streams without effect hooks; small bundle; official template |
| Speech-to-text | Grok Voice Transcribe 2.0 over WebSocket, `interim_results=true` | Real partial results every ~500 ms; word timestamps; keyterms; ~$0.20/h |
| Notes model | `grok-4.7` via chat completions, `stream: true` | Same key as STT; vision for slides; SSE gives token streaming |
| Loopback | BlackHole 2ch as an ordinary input device | Simplest reliable system-audio capture from Rust (cpal); no ObjC bridging |
| Source of truth | Markdown + transcript + slides on disk, as today | Human-readable, tool-independent, already proven |
| Product shape | Personal single-user tool | No auth, no server, no analytics |

## 3. Architecture (approach B: core library + app + CLI)

### 3.1 Workspace layout

```
LectureLive/
  Cargo.toml                 workspace
  crates/
    core/                    library: the whole pipeline, no UI
      src/audio/             sources, mixer, resampler, framer, recorder, routing (macOS)
      src/stt/               websocket client, transcript state machine
      src/notes/             snapshot builder, notes client (SSE), polish
      src/capture/           window enumeration, capture loop, change detection
      src/session/           lecture folder, files, state, resume
      src/events.rs          Event enum (the only thing the UI depends on)
      src/lib.rs             Session: start/stop/snapshot/polish/capture_now/set_source
    cli/                     headless binary on core (replaces live_notes.py)
  apps/desktop/              Tauri 2 app
    src-tauri/               commands → core::Session; core events → Tauri events
    src/                     Svelte 5
  live_notes.py, pyproject.toml   kept until the CLI reaches parity, then removed
  docs/
```

Dependency direction: `desktop` → `core` ← `cli`. `core` never depends on Tauri.

### 3.2 Core modules and interfaces

```rust
// audio
trait AudioSource { fn start(&mut self, sink: FrameSink) -> Result<()>; fn stop(&mut self); }
struct Frame { pcm16: Vec<i16>, at: Instant }          // 100 ms, 16 kHz mono
enum SourceSpec { Input(DeviceId), Loopback, Mixed { input: DeviceId } }
struct Recorder;                                       // WAV writer, 16 kHz mono
mod routing { fn enable_loopback_output() -> Result<Restore>; }   // macOS only

// stt
struct SttStream;   // connects wss://api.x.ai/v1/stt, sends frames, yields SttMessage
enum SttMessage { Created, Partial { text, words, is_final, speech_final }, Done { .. }, Error { .. } }
struct Transcript; // state machine: committed utterances + tentative tail; emits Event::Transcript*

// notes
struct Snapshot { segments: Vec<(Time, String)>, slides: Vec<Slide>, hint: Option<String> }
fn build_prompt(doc: &str, snapshot: &Snapshot) -> ChatRequest;    // same prompt text as the CLI
struct NotesClient; // streams SSE deltas, returns the final text; embed guard applied by caller

// capture
fn list_windows() -> Vec<WindowInfo>;
struct CaptureLoop; // samples a window every 1.5 s, emits Event::SlideCaptured on change
fn changed(prev: &Thumb, next: &Thumb) -> bool;                    // pure, unit-tested

// session
struct LectureFolder { notes, transcript, slides_dir, state, recording }
struct State { noted_through: Time, source: SourceSpec, window: Option<WindowId>, keyterms: Vec<String> }
fn resume(folder) -> (Vec<Segment>, Vec<Slide>)                   // same rules as the CLI
```

Every module is usable and testable without the others: the transcript state machine
takes `SttMessage`s, change detection takes two thumbnails, `build_prompt` takes
strings, `resume` takes a folder.

### 3.3 Data flow

```
AudioSource(s) ─► Mixer ─► Resampler(→16 kHz) ─► Framer(100 ms) ─┬─► SttStream ─► Transcript ─► transcript file
                                                                  └─► Recorder (session WAV)
CaptureLoop ─► changed()? ─► PNG ≤1600 px in slides/ ─► Event::SlideCaptured ─► pending slides
Enter/hint ─► Session::snapshot(): finalize STT tail ─► collect pending ─► build_prompt ─► NotesClient (SSE)
           ─► Event::NotesToken… ─► on completion: embed guard ─► append to notes file ─► state.noted_through
```

### 3.4 Events (core → UI)

```rust
enum Event {
  AudioLevel { rms: f32 },
  SttState { connected: bool, reconnecting: bool },
  TranscriptPartial { text: String, words: Vec<Word> },        // tentative tail, replaces previous
  TranscriptFinal { at: Time, text: String, words: Vec<Word> },// committed utterance, appended
  SlideCaptured { index: u32, at: Time, path: PathBuf, auto: bool },
  NotesStarted { words: usize, slides: usize },
  NotesToken { delta: String },
  NotesDone { block: String }, NotesFailed { reason: String },
  PolishDone { backup: PathBuf },
  Error { scope: &'static str, message: String },
}
```

Rates: partials ≤ 2/s, notes tokens 20–50/s, audio level 10/s. Tauri's event channel
handles this comfortably; tokens may be coalesced per animation frame in the UI.

## 4. Audio

### 4.1 Sources

- **Input device**: any cpal input (built-in mic, a wireless receiver). Device sample
  rate and channel count are whatever the device offers; converted downstream.
- **Loopback**: the "BlackHole 2ch" input device. Zoom's output reaches it through a
  macOS Multi-Output Device (speakers + BlackHole), so the user still hears the audio.
- **Mixed**: input + loopback summed sample-wise and clamped, after resampling both to
  16 kHz mono. Simple and adequate for "lecturer via Zoom, my questions via my mic".
  Per-channel transcription (`multichannel=true`, `channels=2`) is the v2 upgrade path:
  the API supports it, the framer would interleave instead of sum.

### 4.2 Resampling and framing

`rubato` (sinc, fixed ratio) from device rate to 16 kHz; stereo devices are averaged
to mono before resampling. The framer emits exactly 1600 samples (100 ms) per frame.
Frames go to both the STT stream and the recorder.

### 4.3 Output routing (macOS)

On start with a loopback source: find or create the Multi-Output Device via CoreAudio
(`AudioHardwareCreateAggregateDevice` with the current default output + BlackHole),
remember the current default output, set the aggregate as default output. On stop,
restore. If BlackHole is not installed, the source picker shows the install hint
(`brew install blackhole-2ch`) and the loopback options are disabled. Known trade-off:
macOS disables the volume keys while an aggregate device is the output; volume is set
in Zoom instead.

### 4.4 Recording

The 16 kHz mono stream is written continuously to
`<folder>/recordings/session_YYYYMMDD_HHMMSS.wav` (≈115 MB/h). This is the recovery
path for any STT gap: the CLI's REST transcription can be re-run over any range. A
setting controls whether recordings are kept after a successful session.

## 5. Speech-to-text streaming

### 5.1 Connection

`wss://api.x.ai/v1/stt?model=grok-voice-transcribe-2.0&encoding=pcm&sample_rate=16000&interim_results=true&language=en&endpointing=400&keyterm=…`
with `Authorization: Bearer`. Wait for `transcript.created`, then send one binary frame
per 100 ms. `filler_words` stays false.

### 5.2 Transcript state machine

Messages carry `is_final` and `speech_final`:

- `is_final=false`: replace the tentative tail (UI shows it dimmed).
- `is_final=true, speech_final=false`: lock the chunk; words move to the committed
  region but the utterance is still open.
- `speech_final=true`: utterance closed; append `[HH:MM:SS] text` to the transcript
  file with the utterance's first word start time, push the segment to pending.

The UI never re-renders committed words; only the tail is replaced. Word fade-in keys
on word identity (utterance id + index) so re-renders of the tail do not restart
animations for words that survived.

### 5.3 Snapshot flush

`Session::snapshot()` sends the text message `finalize`, waits for the resulting
`speech_final` (bounded, 3 s), then collects pending. This replaces the CLI's
force-cut-and-drain with one protocol message.

### 5.4 Reconnection

The server closes the socket after `transcript.done` (only sent after `audio.done`)
or on error. On any unexpected close: reconnect with backoff (1, 2, 4 s…), replay a
bounded ring buffer of the last 30 s of frames, and continue. If the outage exceeds
the ring, the gap is marked in the transcript (`--- gap HH:MM:SS–HH:MM:SS ---`) and is
recoverable from the recording. Keyterms and session settings are re-applied on
reconnect.

## 6. Notes

### 6.1 Semantics (unchanged from the CLI)

Whole-document context (tail-capped at 40 k chars), append-only, invisible
`<!-- HH:MM -->` marker per snapshot, chronological timeline interleaving segments and
`>>> Slide N shown` markers, mandatory verbatim embed lines verified after the response
(missing ones appended), hint support, `polish` with backup. The prompt text moves from
`live_notes.py` to `core/src/notes/prompts.rs` unchanged.

### 6.2 Streaming

Chat completions with `stream: true`; SSE deltas are forwarded as `NotesToken` events
and accumulated. The file is appended only on completion, so a failed or cancelled
stream leaves the document untouched and the material pending, exactly as today.

## 7. Slide capture

### 7.1 Window selection

`xcap` enumerates windows (app name, title, id). The user picks the Zoom window once;
the choice is saved in the folder state and re-validated on start. A display region is
the fallback for full-screen setups where the window is not enumerable.

### 7.2 Auto mode

Every 1.5 s: capture the window, downscale to a 64×36 grayscale thumbnail. A frame is
a *candidate* when its mean absolute pixel difference from the last **kept** thumbnail
exceeds 0.06 (normalised 0–1). A candidate is **kept** when the next sample is within
0.02 of it (the slide has settled; animations and cursor movement are ignored).
Kept frames are saved full-size (≤1600 px, PNG) as `slides/slide_NN_HHMMSS.png` with
the capture time, and become pending for the next snapshot. Thresholds are constants
in `capture::detect` with unit tests on synthetic frames; the UI exposes a sensitivity
slider mapped onto the candidate threshold.

### 7.3 Manual mode

Capture button and a global shortcut capture the selected window immediately.
Drag-and-drop of image files onto the slides strip registers them like the CLI does.
The Desktop screenshot watcher is not carried over; the app's own capture replaces it.

### 7.4 Permissions

Screen Recording (TCC). The app shows a clear state and a button that opens the
System Settings pane; capture controls stay disabled until granted.

## 8. Files, state, resume

Layout is the CLI's plus `recordings/`:

```
<lecture folder>/
  lecture_notes_YYYYMMDD.md
  lecture_transcript_YYYYMMDD.txt
  slides/slide_NN_HHMMSS.png
  recordings/session_YYYYMMDD_HHMMSS.wav
  .live_notes/<notes stem>.json      { noted_through, source, window, keyterms }
  .live_notes/<notes stem>_HHMMSS.md polish backups
```

Resume rule: transcript lines and slide files with time ≥ `noted_through` are
pending. First run on a folder that already has files sets `noted_through` to now.
The CLI and the app can be used on the same folder on the same day.

## 9. UI (Svelte 5, Tauri 2)

### 9.1 Layout

Three panes and a control bar:

- **Transcript** (left): utterances as paragraphs with times; committed words at full
  opacity, the tentative tail at reduced opacity; each newly committed word fades in
  (≈180 ms). Auto-scroll pins to bottom until the user scrolls up; a "jump to live"
  affordance returns.
- **Notes** (centre, widest): the Markdown document rendered; during a snapshot the
  new block streams in at the bottom with the same fade-in; slide embeds render inline
  from `slides/`.
- **Slides** (right strip): thumbnails in capture order with time, an auto/manual
  badge, and the window picker at the top.
- **Control bar**: source picker with level meter, Start/Stop, hint field + Snapshot,
  Polish, capture Auto/Manual toggle and Capture-now, status (STT connection, capture
  state, elapsed, session cost estimate).

### 9.2 State

One `session.svelte.ts` store built on runes, fed by Tauri event listeners registered
once at module load. Components read `$derived` values; no React-style effect hooks
anywhere. Token events are coalesced per animation frame before touching the store.

### 9.3 Visual design

Produced at build time with the frontend-design skill against these requirements:
readable at a glance from arm's length during a lecture, light and dark themes, a
large-type toggle, and no decorative motion beyond the fade-ins that carry meaning
(new words, new tokens).

## 10. Errors and robustness

| Failure | Behaviour |
|---------|-----------|
| Network down / websocket error | Reconnect with backoff; audio buffered 30 s then gap-marked; recording continues; status shows reconnecting |
| STT 4xx (auth, params) | Stop STT, surface the message, keep recording |
| Notes call fails or is cancelled | Nothing written; material stays pending; toast with reason |
| Screen Recording not granted | Capture disabled with a fix-it button; everything else works |
| BlackHole missing | Loopback options disabled with install hint; input devices still work |
| Input device disappears (receiver unplugged) | Source falls back to the built-in mic with a warning; recording continues |
| Disk write error | Stop the session cleanly, surface the path |
| App crash | Transcript, slides and recording are on disk; resume picks up at the last snapshot |

## 11. Testing

- **Unit (core)**: framer sizes across device rates; mixer clamping; transcript state
  machine driven by recorded `SttMessage` logs; `changed()` on synthetic thumbnails
  (identical, cursor-only, slide change, transition mid-animation); `resume()` on
  fixture folders; embed guard; prompt builder golden tests against the CLI's output.
- **Integration**: a fake STT websocket server replaying a captured session; a fake
  SSE endpoint; both run in `cargo test` without network.
- **Manual checklist**: real Zoom, each audio source, permission flows, a full lecture
  with restart mid-way.

## 12. Milestones

| M | Deliverable | Done when |
|---|-------------|-----------|
| M1 | `core::audio` + `core::stt` + `cli` streaming transcript to a file | A 10-minute Zoom recording produces a transcript equal in content to the Python CLI's, with partials visible in the terminal |
| M2 | `core::notes` + `core::session` parity | The CLI snapshot/polish/resume behaviour is reproduced on the same folder format; Python CLI removed |
| M3 | Tauri shell + transcript pane | Live fade-in transcript in the window; start/stop; source picker for input devices |
| M4 | Notes pane streaming | Snapshot with hint streams tokens; file written on completion; polish |
| M5 | Capture | Window picker, auto change detection, manual capture, slides strip, embeds in notes |
| M6 | Loopback + routing + recording | BlackHole source, automatic output routing and restore, session WAV, mixed source |
| M7 | Hardening | Reconnect/replay, gap marking, permission states, error table verified |

## 13. Crates (proposed)

`tauri 2`, `tokio`, `tokio-tungstenite` (rustls), `reqwest` (stream feature),
`serde`/`serde_json`, `cpal`, `rubato`, `hound`, `xcap`, `image`, `coreaudio-sys`
(default output device, aggregate device), `anyhow`/`thiserror`, `tracing`.
Frontend: `@tauri-apps/api`, `svelte 5`, `marked` for Markdown.

## 14. Open questions for review

1. Mixed source as a sum vs. `multichannel=2` from the start — is the per-channel UI worth the extra complexity in v1?
2. BlackHole (driver install, output-device switching, volume-key caveat) vs. ScreenCaptureKit audio capture (no driver, needs ObjC bridging from Rust). Is the bridging cost justified now?
3. Change-detection thresholds and cadence (§7.2): any known-better approach for slide decks with builds/animations?
4. Whole-document context per snapshot: keep, or switch to a rolling summary once the doc exceeds the cap?
5. Should the REST transcription path be kept in core as the recovery tool over recordings, or left to the Python CLI?
6. Event granularity for fade-in: per token from SSE vs. per word after whitespace — any UI-performance reason to prefer one?
7. Anything in §3 that will fight Tauri 2's threading or event model in practice.
