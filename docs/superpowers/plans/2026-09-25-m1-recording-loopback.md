# M1 Recording + Loopback Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Record any input, or Zoom through BlackHole, as crash-safe 16 kHz WAV with exact sample timing, from a session coordinator and a CLI `record` command. Every interval without durable audio is recorded as a gap. The system output is never changed.

**Architecture:** The cpal callback copies into a preallocated `rtrb` ring and publishes overflows as positioned drop events. A source worker thread turns ring contents into timed `Frame`s: downmix, a resampler trimmed for delay and flushed at the end, and 100 ms framing. It also rebuilds the stream when the device disappears or changes rate. A Tokio coordinator task owns the recorder thread and the v2 sidecar (recordings, anchors, gaps), and the CLI drives it. Loopback uses an app-owned Multi-Output device ("LectureLive Loopback": BlackHole as clock plus one physical output) that Zoom's Speaker is set to. It is never made the system default.

**Tech Stack:** Rust 1.96.1, `cpal 0.18.2`, `rubato 5.0.0`, `hound 3.5.1`, `rtrb 0.4.0`, `uuid 1.26.1`, `chrono 0.4.45` (serde), `objc2-av-foundation 0.3.2`, `coreaudio-sys 0.2.18`, `core-foundation 0.10.1`, `tokio 1.53.1`, `clap 4.6.7`.

**Spec:** `docs/spec.md` §3.2–3.4, §4.1–4.4, §10. Gate: `docs/milestones.md` → M1. Evidence: M0 Findings in `docs/milestones.md`.

**Routing decision (taken before this plan was written):** the README arrangement, built by the app. The app owns a Multi-Output device with a fixed UID. BlackHole is its clock; the chosen headphones or speakers are the second member, with drift correction on them. The device is never made the system default, and Zoom's Speaker is set to it once by name. §4.3's preflight stays as a measurement: play Zoom's Test Speaker and require a signal on BlackHole. It is the only guard against Zoom's setting drifting to a physical device, which M0 found. Task 6 rewrites spec §4.3, §4.1's loopback line, and the lines of §1.3, §10 and §12 that described the default-output design.

## Global Constraints

- macOS 13+, Apple Silicon; single user; no telemetry, accounts or servers.
- Toolchain pinned to `rustc 1.96.1` (`rust-toolchain.toml`). Workspace `rust-version` goes from 1.87 to 1.89 in Task 3, because `std::fs::File::try_lock` (stable since 1.89) is the folder lock.
- Any task that creates or changes UI (desktop app views, `notes_template.html`, any page a person looks at) invokes `/frontend-design:frontend-design` before any markup is written (spec §9.4). M1 has no UI tasks.
- Do not touch `apps/desktop` or `target/release/bundle`. No `npm` or `tauri` build. The packaged "LectureLive Canary.app" holds this Mac's Microphone and Screen Recording grants, which are tied to its ad-hoc signature. The canary app is a workspace member, so these core APIs keep their signatures: `routing::{enable_loopback, disable_loopback, route_status}`, `input::{list_inputs, record_for}`, `InputInfo.name`, `window::{list_windows, capture_window}`. Never run `cargo clean`.
- Builds are authorised: `cargo build`, `test`, `run`, `tree`, `add`.
- Any check that changes the default output ends with the default output it began with. If it doesn't, run `cargo run -p lecturelive-cli -- canary route off`. If that fails, name the device to pick in System Settings → Sound → Output (normally "Macbook + Notes", UID `~:AMS2_StackedOutput:1`).
- Recordings live under `~/Library/Application Support/LectureLive/`. The repository is public: commit no audio except synthesised `say` output. Stage files by name, never `git add -A`.
- Commit messages: imperative, neutral, what and why. No co-author trailers, no names.
- Do not edit files with Python scripts. In the Bash tool `ls` is `eza`: use `command ls` or explicit paths in scripts.
- Crate APIs below are written against the versions named. Where an API differs, adapt the implementation and keep the task's tests unchanged: the tests are the contract. The only exception is a test that encodes an unverified fact about an external system which live evidence contradicts. Change such a test only with the evidence recorded as a Ruling in the ledger (`.superpowers/sdd/2026-09-25-m1-recording-loopback/progress.md`).
- When a check fails, record the observation and the spec §14.1 fallback it points to. Do not build the fallback.
- STT fixtures in `crates/core/tests/fixtures/stt/` belong to M2; do not change them.

## Review Focus

1. **The device disappears mid-recording and comes back** (wireless receiver unplugged and replugged). Expected: the recording closes, a `DeviceGone` gap is marked, and no other input is opened. When the same UID returns, a new recording starts. Pinned by `coordinator::tests::device_gone_and_back_gives_two_recordings_and_a_gap` (Task 5) and the sitting (Task 8).
2. **The recordings folder cannot be written** (disk full, permissions). Expected: the session stops cleanly and the error names the path (spec §10). Pinned by `coordinator::tests::unwritable_recordings_dir_stops_with_the_path` (Task 5).
3. **The sidecar is corrupt at launch.** Expected: launch stops with an explicit error, and the file is left byte-for-byte as it was. It is never overwritten from an empty default. Pinned by `launch::tests::corrupt_sidecar_is_left_untouched` (Task 3).
4. **A crash within the first quarter-second of a recording.** Expected: the WAV header is already on disk, so repair yields a valid empty file rather than an unrepairable 0-byte one. Pinned by `recorder::tests::header_is_on_disk_before_any_audio` (Task 3).
5. **The default output is itself a Multi-Output device** (this Mac: "Macbook + Notes"). Expected: loopback setup refuses to put an aggregate inside its device and asks for `--output <UID>`. Pinned by `loopback::tests::refuses_an_aggregate_default_as_the_physical_output` (Task 6).

---

### Task 1: Frame timing and recording anchors (milestone task 1)

**Files:**
- Create: `crates/core/src/audio/frame.rs`, `crates/core/src/session/mod.rs`, `crates/core/src/session/sidecar.rs`
- Modify: `crates/core/src/audio/convert.rs`, `crates/core/src/audio/mod.rs`, `crates/core/src/audio/input.rs:103-112`, `crates/core/src/lib.rs`, `crates/core/Cargo.toml` (already has `rtrb`, `uuid` v4+serde, `chrono` serde from plan research)

**Interfaces:**
- Produces:
  - `audio::convert::to_i16(f32) -> i16`
  - `Resampler16k::{new(u32) -> Result<Self>, push(&mut self, &[f32]) -> Result<Vec<f32>>, finish(self) -> Result<Vec<f32>>}`. Output is trimmed for the resampler delay, so output sample *k* is input time *k*/16000 s. `finish` flushes the tail so the total output equals the input duration at 16 kHz, rounded to nearest.
  - `audio::frame::{Frame, FrameBuilder, FRAME_SAMPLES}`
    - `Frame { recording_id: Uuid, sample_offset: u64, valid_samples: u32, pcm16: [i16; 1600] }`
    - `Frame::pcm(&self) -> &[i16]`
    - `FrameBuilder::{new(Uuid), push(&mut self, &[f32]) -> Vec<Frame>, finish(self) -> Option<Frame>}`
  - `session::sidecar::{Sidecar, RecordingEntry, RecState, Gap, GapKind, sidecar_path, SIDECAR_VERSION}`
    - `Sidecar::{load(&Path) -> Result<Option<Sidecar>>, save(&self, &Path) -> Result<()>, recording_mut(&mut self, Uuid) -> Option<&mut RecordingEntry>, wall_time(&self, Uuid, u64) -> Option<DateTime<Local>>}`
    - `sidecar_path(dir, stem) -> PathBuf` = `dir/.live_notes/<stem>.v2.json`

- [ ] **Step 1: Write the failing tests**

Append to the test module in `crates/core/src/audio/convert.rs`:

```rust
    fn impulse_peak(rate: u32) -> usize {
        let mut r = Resampler16k::new(rate).unwrap();
        let mut input = vec![0.0f32; rate as usize * 2];
        input[rate as usize] = 1.0; // exactly 1.000 s
        let mut out = Vec::new();
        for chunk in input.chunks(480) {
            out.extend(r.push(chunk).unwrap());
        }
        out.extend(r.finish().unwrap());
        out.iter().enumerate().max_by(|a, b| a.1.abs().total_cmp(&b.1.abs())).unwrap().0
    }

    #[test]
    fn resampled_time_matches_input_time() {
        for rate in [16_000, 44_100, 48_000] {
            let peak = impulse_peak(rate) as i64;
            assert!((peak - 16_000).abs() <= 2, "{rate} Hz: impulse at 1 s came out at sample {peak}");
        }
    }

    #[test]
    fn finish_makes_output_length_equal_input_duration() {
        for (rate, n) in [(16_000u32, 40_480usize), (44_100, 111_573), (48_000, 121_440)] {
            let mut r = Resampler16k::new(rate).unwrap();
            let mut total = 0;
            for chunk in vec![0.1f32; n].chunks(333) {
                total += r.push(chunk).unwrap().len();
            }
            total += r.finish().unwrap().len();
            let expected = (n as u64 * 16_000 + rate as u64 / 2) / rate as u64;
            assert_eq!(total as u64, expected, "{rate} Hz, {n} input samples");
        }
    }
```

Create `crates/core/src/audio/frame.rs` with only the test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::convert::{downmix, Resampler16k};

    fn tone(rate: u32, channels: usize, secs: f64) -> Vec<f32> {
        let n = (rate as f64 * secs).round() as usize;
        (0..n)
            .flat_map(|i| std::iter::repeat((i as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin() * 0.5).take(channels))
            .collect()
    }

    fn frames_for(rate: u32, channels: usize, secs: f64) -> (Uuid, Vec<Frame>) {
        let id = Uuid::new_v4();
        let mut resampler = Resampler16k::new(rate).unwrap();
        let mut builder = FrameBuilder::new(id);
        let mut frames = Vec::new();
        for chunk in tone(rate, channels, secs).chunks(512 * channels) {
            frames.extend(builder.push(&resampler.push(&downmix(chunk, channels)).unwrap()));
        }
        frames.extend(builder.push(&resampler.finish().unwrap()));
        frames.extend(builder.finish());
        (id, frames)
    }

    #[test]
    fn common_rates_frame_contiguously_and_exactly() {
        for rate in [16_000, 44_100, 48_000] {
            for channels in [1, 2] {
                let (id, frames) = frames_for(rate, channels, 2.5);
                assert_eq!(frames.len(), 25, "{rate} Hz × {channels} ch");
                for (i, f) in frames.iter().enumerate() {
                    assert_eq!(f.recording_id, id);
                    assert_eq!(f.sample_offset, i as u64 * FRAME_SAMPLES as u64);
                    assert_eq!(f.valid_samples as usize, FRAME_SAMPLES);
                }
            }
        }
    }

    #[test]
    fn last_partial_frame_is_marked_and_zero_padded() {
        for rate in [16_000, 44_100, 48_000] {
            let (_, frames) = frames_for(rate, 1, 2.53); // 40_480 samples = 25 frames + 480
            let last = frames.last().unwrap();
            assert_eq!(frames.len(), 26, "{rate} Hz");
            assert_eq!(last.sample_offset, 40_000);
            assert_eq!(last.valid_samples, 480);
            assert!(last.pcm16[480..].iter().all(|&s| s == 0));
            assert_eq!(last.pcm().len(), 480);
        }
    }

    #[test]
    fn nothing_pushed_gives_no_final_frame() {
        assert!(FrameBuilder::new(Uuid::new_v4()).finish().is_none());
    }
}
```

Create `crates/core/src/session/sidecar.rs` with only the test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn entry(id: Uuid) -> RecordingEntry {
        RecordingEntry {
            id,
            file: "recordings/session_20260925_100000.wav".into(),
            anchor: Local.with_ymd_and_hms(2026, 9, 25, 10, 0, 0).unwrap(),
            source_uid: "BlackHole2ch_UID".into(),
            input_rate: 48_000,
            samples: None,
            state: RecState::Open,
        }
    }

    #[test]
    fn round_trips_and_lives_under_live_notes() {
        let dir = tempfile::tempdir().unwrap();
        let path = sidecar_path(dir.path(), "lecture_notes_20260925");
        assert_eq!(path, dir.path().join(".live_notes/lecture_notes_20260925.v2.json"));
        assert_eq!(Sidecar::load(&path).unwrap(), None);
        let id = Uuid::new_v4();
        let mut s = Sidecar::default();
        s.recordings.push(entry(id));
        s.gaps.push(Gap { recording_id: id, start_sample: 16_000, end_sample: Some(17_600), kind: GapKind::CaptureOverflow, resolved: false });
        s.save(&path).unwrap();
        let back = Sidecar::load(&path).unwrap().unwrap();
        assert_eq!(back, s);
        assert_eq!(back.version, SIDECAR_VERSION);
    }

    #[test]
    fn sample_offsets_map_to_wall_time_from_the_anchor() {
        let id = Uuid::new_v4();
        let mut s = Sidecar::default();
        s.recordings.push(entry(id));
        let anchor = s.recordings[0].anchor;
        assert_eq!(s.wall_time(id, 16_000).unwrap(), anchor + chrono::Duration::seconds(1));
        assert_eq!(s.wall_time(id, 8).unwrap(), anchor + chrono::Duration::microseconds(500));
        assert_eq!(s.wall_time(Uuid::new_v4(), 0), None);
    }

    #[test]
    fn corrupt_sidecar_is_an_error_not_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = sidecar_path(dir.path(), "x");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"{not json").unwrap();
        assert!(Sidecar::load(&path).is_err());
    }
}
```

Register the modules. `crates/core/src/audio/mod.rs` gets `pub mod frame;`. `crates/core/src/lib.rs` gets `pub mod session;`. `crates/core/src/session/mod.rs`:

```rust
pub mod sidecar;
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p lecturelive-core frame sidecar resampled finish_makes`
Expected: compile errors: `FrameBuilder`, `Frame`, `Sidecar`, `Resampler16k::finish` not found.

- [ ] **Step 3: Implement**

In `crates/core/src/audio/convert.rs`, replace `Resampler16k` and the `Framer::push` conversion with:

```rust
pub fn to_i16(s: f32) -> i16 {
    (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16
}

/// Mono resampler to 16 kHz whose output sample k is input time k / 16000 s:
/// the resampler's delay is trimmed and `finish` flushes the tail.
pub struct Resampler16k {
    inner: Option<Fft<f32>>,
    pending: Vec<f32>,
    input_rate: u64,
    in_total: u64,
    out_total: u64,
    skip: usize,
}

impl Resampler16k {
    pub fn new(input_rate: u32) -> Result<Self> {
        let inner = if input_rate as usize == TARGET_RATE {
            None
        } else {
            Some(Fft::<f32>::new(input_rate as usize, TARGET_RATE, CHUNK, 1, FixedSync::Input)?)
        };
        let skip = inner.as_ref().map_or(0, |r| r.output_delay());
        Ok(Self { inner, pending: Vec::new(), input_rate: input_rate as u64, in_total: 0, out_total: 0, skip })
    }

    fn expected_output(&self) -> u64 {
        (self.in_total * TARGET_RATE as u64 + self.input_rate / 2) / self.input_rate
    }

    fn run(&mut self) -> Result<Vec<f32>> {
        let Some(r) = self.inner.as_mut() else {
            let out = std::mem::take(&mut self.pending);
            self.out_total += out.len() as u64;
            return Ok(out);
        };
        let mut out = Vec::new();
        while self.pending.len() >= r.input_frames_next() {
            let n = r.input_frames_next();
            let chunk: Vec<f32> = self.pending.drain(..n).collect();
            out.extend(r.process(&InterleavedSlice::new(&chunk, 1, n)?, None)?.take_data());
        }
        let skip = self.skip.min(out.len());
        self.skip -= skip;
        out.drain(..skip);
        self.out_total += out.len() as u64;
        Ok(out)
    }

    pub fn push(&mut self, mono: &[f32]) -> Result<Vec<f32>> {
        self.in_total += mono.len() as u64;
        self.pending.extend_from_slice(mono);
        self.run()
    }

    /// Drains the resampler so the total output equals the input duration at 16 kHz (spec §4.2).
    pub fn finish(mut self) -> Result<Vec<f32>> {
        let expected = self.expected_output();
        let mut out = Vec::new();
        while self.inner.is_some() && self.out_total < expected {
            let n = self.inner.as_ref().map_or(0, |r| r.input_frames_next());
            let len = self.pending.len().max(n);
            self.pending.resize(len, 0.0);
            out.extend(self.run()?);
        }
        let excess = self.out_total.saturating_sub(expected) as usize;
        out.truncate(out.len().saturating_sub(excess));
        Ok(out)
    }
}
```

and in `Framer::push` use `self.buf.extend(mono16k.iter().map(|&s| to_i16(s)));`.

In `crates/core/src/audio/input.rs`, drain the resampler before the framer's tail. Replace the three lines from `let tail = framer.finish();`:

```rust
    for frame in framer.push(&resampler.finish()?) {
        recorder.write(&frame)?;
        samples += frame.len() as u64;
    }
    let tail = framer.finish();
```

Above the test module in `crates/core/src/audio/frame.rs`:

```rust
use uuid::Uuid;

use super::convert::to_i16;
pub use super::convert::FRAME_SAMPLES;

/// 100 ms of 16 kHz mono PCM16 at a known position in a recording (spec §3.3).
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub recording_id: Uuid,
    pub sample_offset: u64,
    pub valid_samples: u32,
    pub pcm16: [i16; FRAME_SAMPLES],
}

impl Frame {
    pub fn pcm(&self) -> &[i16] {
        &self.pcm16[..self.valid_samples as usize]
    }
}

pub struct FrameBuilder {
    recording_id: Uuid,
    next_offset: u64,
    buf: Vec<i16>,
}

impl FrameBuilder {
    pub fn new(recording_id: Uuid) -> Self {
        Self { recording_id, next_offset: 0, buf: Vec::with_capacity(FRAME_SAMPLES * 2) }
    }

    pub fn push(&mut self, mono16k: &[f32]) -> Vec<Frame> {
        self.buf.extend(mono16k.iter().map(|&s| to_i16(s)));
        let mut out = Vec::new();
        while self.buf.len() >= FRAME_SAMPLES {
            let mut pcm16 = [0; FRAME_SAMPLES];
            pcm16.copy_from_slice(&self.buf[..FRAME_SAMPLES]);
            self.buf.drain(..FRAME_SAMPLES);
            out.push(self.frame(pcm16, FRAME_SAMPLES));
        }
        out
    }

    /// The last partial frame, zero-padded, with `valid_samples` saying how much is audio.
    pub fn finish(mut self) -> Option<Frame> {
        if self.buf.is_empty() {
            return None;
        }
        let mut pcm16 = [0; FRAME_SAMPLES];
        let n = self.buf.len();
        pcm16[..n].copy_from_slice(&self.buf);
        Some(self.frame(pcm16, n))
    }

    fn frame(&mut self, pcm16: [i16; FRAME_SAMPLES], valid: usize) -> Frame {
        let f = Frame { recording_id: self.recording_id, sample_offset: self.next_offset, valid_samples: valid as u32, pcm16 };
        self.next_offset += FRAME_SAMPLES as u64;
        f
    }
}
```

Above the test module in `crates/core/src/session/sidecar.rs`:

```rust
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::audio::recorder::SAMPLE_RATE;
use crate::fsutil::write_atomic;

pub const SIDECAR_VERSION: u32 = 2;

/// The lecture folder's record of what exists on disk (spec §8). M1 fills recordings and
/// gaps; later milestones add their fields with `#[serde(default)]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sidecar {
    pub version: u32,
    #[serde(default)]
    pub recordings: Vec<RecordingEntry>,
    #[serde(default)]
    pub gaps: Vec<Gap>,
}

impl Default for Sidecar {
    fn default() -> Self {
        Self { version: SIDECAR_VERSION, recordings: Vec::new(), gaps: Vec::new() }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordingEntry {
    pub id: Uuid,
    /// Relative to the lecture folder.
    pub file: String,
    /// Wall-clock time of sample 0 (spec §3.3).
    pub anchor: DateTime<Local>,
    pub source_uid: String,
    pub input_rate: u32,
    pub samples: Option<u64>,
    pub state: RecState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecState {
    Open,
    Finalized,
    Repaired,
    Missing,
    Deleted,
}

/// An interval of a recording without durable captured audio. `end_sample: None` runs
/// from `start_sample` to the next recording's anchor, or to the end of the session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Gap {
    pub recording_id: Uuid,
    pub start_sample: u64,
    pub end_sample: Option<u64>,
    pub kind: GapKind,
    #[serde(default)]
    pub resolved: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GapKind {
    CaptureOverflow,
    RecorderOverflow,
    DeviceGone,
    RateChange,
    Interrupted,
}

pub fn sidecar_path(dir: &Path, stem: &str) -> PathBuf {
    dir.join(".live_notes").join(format!("{stem}.v2.json"))
}

impl Sidecar {
    pub fn load(path: &Path) -> Result<Option<Self>> {
        match std::fs::read(path) {
            Ok(b) => Ok(Some(serde_json::from_slice(&b).with_context(|| format!("corrupt sidecar {}", path.display()))?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        write_atomic(path, &serde_json::to_vec_pretty(self)?).with_context(|| format!("write {}", path.display()))
    }

    pub fn recording_mut(&mut self, id: Uuid) -> Option<&mut RecordingEntry> {
        self.recordings.iter_mut().find(|r| r.id == id)
    }

    pub fn wall_time(&self, id: Uuid, sample: u64) -> Option<DateTime<Local>> {
        let r = self.recordings.iter().find(|r| r.id == id)?;
        Some(r.anchor + chrono::Duration::microseconds((sample * 1_000_000 / SAMPLE_RATE as u64) as i64))
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p lecturelive-core`
Expected: every test passes, including M0's (`resamples_common_rates_to_16k` still holds with the delay trimmed; the ignored hardware tests stay ignored). If `resampled_time_matches_input_time` fails by a constant, the delay reported by `output_delay()` differs from what rubato 5's `Fft` actually applies. Measure it from the test output and trim that amount. The test does not change.

- [ ] **Step 5: Commit**

```bash
git add crates/core/Cargo.toml Cargo.lock crates/core/src/lib.rs crates/core/src/audio/mod.rs crates/core/src/audio/convert.rs crates/core/src/audio/input.rs crates/core/src/audio/frame.rs crates/core/src/session/mod.rs crates/core/src/session/sidecar.rs
git commit -m "Add timed 100 ms frames, an exact 16 kHz resampler and the v2 sidecar

Frames carry recording id, sample offset and valid length. The resampler
trims its delay and flushes its tail, so output sample k is input time
k/16000 s at 16, 44.1 and 48 kHz. The sidecar persists each recording's
wall-clock anchor and the gaps in it."
```

---

### Task 2: Lock-free capture path (milestone task 2)

**Files:**
- Create: `crates/core/src/audio/capture.rs`, `crates/core/src/audio/pipeline.rs`
- Modify: `crates/core/src/audio/mod.rs`

**Interfaces:**
- Consumes: `Resampler16k`, `FrameBuilder`, `Frame`, `downmix` (Task 1).
- Produces:
  - `audio::capture::{ring, CaptureProducer, CaptureConsumer, Chunk, Dropped, StreamFlags, RING_SECONDS}`
    - `ring(channels: u16, rate: u32, seconds: u32) -> (CaptureProducer, CaptureConsumer, Arc<StreamFlags>)`
    - `CaptureProducer::push(&mut self, &[f32])`: never blocks; allocates nothing
    - `CaptureConsumer::drain(&mut self, impl FnMut(Chunk<'_>) -> Result<()>) -> Result<()>`
    - `Chunk::{Audio(&[f32]), Silence(u64 /* input frames */)}`
    - `StreamFlags::{anchor_ms(&self) -> Option<u64>, on_error(&self, cpal::ErrorKind)}` plus pub fields `gone: AtomicBool`, `invalidated: AtomicBool`, `errors: AtomicU64`
  - `audio::pipeline::Pipeline`
    - `new(Uuid, input_rate: u32, channels: u16) -> Result<Self>`
    - `push(&mut self, &[f32]) -> Result<Vec<Frame>>`
    - `push_silence(&mut self, input_frames: u64) -> Result<(Range<u64>, Vec<Frame>)>`
    - `finish(self) -> Result<Vec<Frame>>`

- [ ] **Step 1: Write the failing tests**

`crates/core/src/audio/capture.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn collect(c: &mut CaptureConsumer) -> Vec<String> {
        let mut seen = Vec::new();
        c.drain(|chunk| {
            seen.push(match chunk {
                Chunk::Audio(a) => format!("audio {}x{}", a.len(), a.first().copied().unwrap_or(0.0)),
                Chunk::Silence(n) => format!("silence {n}"),
            });
            Ok(())
        })
        .unwrap();
        seen
    }

    fn small_ring(channels: u16, frames: usize) -> (CaptureProducer, CaptureConsumer, Arc<StreamFlags>) {
        ring_with_capacity(channels, 16_000, frames)
    }

    #[test]
    fn audio_passes_through_in_order() {
        let (mut p, mut c, flags) = small_ring(1, 64);
        p.push(&[1.0; 10]);
        p.push(&[2.0; 5]);
        assert_eq!(collect(&mut c), ["audio 15x1"]);
        assert!(flags.anchor_ms().is_some());
    }

    #[test]
    fn overflow_becomes_silence_at_the_exact_position() {
        let (mut p, mut c, _) = small_ring(1, 8);
        p.push(&[1.0; 6]);
        p.push(&[2.0; 6]); // does not fit: dropped at frame 6
        p.push(&[2.5; 6]); // still no room: the same drop grows to 12 frames
        assert_eq!(collect(&mut c), ["audio 6x1", "silence 12"]);
        p.push(&[3.0; 4]);
        assert_eq!(collect(&mut c), ["audio 4x3"]);
    }

    #[test]
    fn stereo_positions_count_frames_not_samples() {
        let (mut p, mut c, _) = small_ring(2, 4);
        p.push(&[1.0; 8]); // 4 frames
        p.push(&[2.0; 4]); // 2 frames dropped
        assert_eq!(collect(&mut c), ["audio 8x1", "silence 2"]);
    }

    #[test]
    fn stream_errors_set_flags() {
        let (_, _, flags) = small_ring(1, 8);
        flags.on_error(cpal::ErrorKind::Xrun);
        flags.on_error(cpal::ErrorKind::DeviceNotAvailable);
        flags.on_error(cpal::ErrorKind::StreamInvalidated);
        assert_eq!(flags.errors.load(Ordering::Relaxed), 1);
        assert!(flags.gone.load(Ordering::Relaxed));
        assert!(flags.invalidated.load(Ordering::Relaxed));
    }

    /// Producer and consumer on two threads with a tiny ring: every audio sample must arrive
    /// at its own position, and every dropped frame must come back as silence.
    #[test]
    fn concurrent_overflow_keeps_every_position_exact() {
        const TOTAL: u64 = 400_000;
        let (mut p, mut c, _) = small_ring(1, 1024);
        let producer = std::thread::spawn(move || {
            let mut pos = 0u64;
            let mut seed = 12345u32;
            let mut block = Vec::with_capacity(600);
            while pos < TOTAL {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                let n = (1 + (seed >> 16) % 600) as u64;
                let n = n.min(TOTAL - pos);
                block.clear();
                block.extend((pos..pos + n).map(|i| (i % (1 << 23)) as f32));
                p.push(&block);
                pos += n;
            }
        });
        let (mut pos, mut silent) = (0u64, 0u64);
        let mut check = |c: &mut CaptureConsumer| {
            c.drain(|chunk| {
                match chunk {
                    Chunk::Audio(a) => {
                        for (i, &v) in a.iter().enumerate() {
                            assert_eq!(v, ((pos + i as u64) % (1 << 23)) as f32, "sample at {}", pos + i as u64);
                        }
                        pos += a.len() as u64;
                    }
                    Chunk::Silence(n) => {
                        pos += n;
                        silent += n;
                    }
                }
                Ok(())
            })
            .unwrap();
        };
        while !producer.is_finished() {
            check(&mut c);
            std::thread::sleep(std::time::Duration::from_micros(300));
        }
        producer.join().unwrap();
        check(&mut c);
        assert_eq!(pos, TOTAL);
        assert!(silent > 0, "the test must force at least one overflow");
    }
}
```

`crates/core/src/audio/pipeline.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_is_placed_at_its_16k_position() {
        let mut p = Pipeline::new(Uuid::new_v4(), 48_000, 2).unwrap();
        let mut frames = p.push(&vec![0.5; 48_000 * 2]).unwrap(); // 1 s stereo
        let (lost, more) = p.push_silence(24_000).unwrap(); // 0.5 s lost
        assert_eq!(lost, 16_000..24_000);
        frames.extend(more);
        frames.extend(p.push(&vec![0.5; 48_000 * 2]).unwrap());
        frames.extend(p.finish().unwrap());
        let total: u64 = frames.iter().map(|f| f.valid_samples as u64).sum();
        assert_eq!(total, 40_000); // 2.5 s
        let at = |s: u64| {
            let f = frames.iter().find(|f| f.sample_offset <= s && s < f.sample_offset + 1600).unwrap();
            f.pcm16[(s - f.sample_offset) as usize]
        };
        assert_eq!(at(20_000), 0);
        assert!(at(8_000) > 10_000);
        assert!(at(32_000) > 10_000);
    }
}
```

Register in `crates/core/src/audio/mod.rs`: `pub mod capture;` and `pub mod pipeline;`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p lecturelive-core capture pipeline`
Expected: compile errors: `ring_with_capacity`, `CaptureConsumer`, `Pipeline` not found.

- [ ] **Step 3: Implement**

Above the test module in `capture.rs`:

```rust
//! The only code that runs in the audio callback: a copy into a preallocated ring, or a
//! count of what did not fit (spec §3.4). Everything else happens on the source thread.
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use rtrb::{Consumer, Producer, RingBuffer};

pub const RING_SECONDS: u32 = 4;
const DROP_EVENTS: usize = 256;

/// `frames` input frames that did not fit, starting at input frame `at_frame`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dropped {
    pub at_frame: u64,
    pub frames: u64,
}

#[derive(Default)]
pub struct StreamFlags {
    pub gone: AtomicBool,
    pub invalidated: AtomicBool,
    pub errors: AtomicU64,
    first_ms: AtomicU64,
}

impl StreamFlags {
    /// Wall-clock milliseconds of the first captured sample, once audio has arrived.
    pub fn anchor_ms(&self) -> Option<u64> {
        match self.first_ms.load(Ordering::Acquire) {
            0 => None,
            ms => Some(ms),
        }
    }

    /// cpal reports disconnection and sample-rate change through its own property
    /// listeners (`kAudioDevicePropertyDeviceIsAlive`, `kAudioDevicePropertyNominalSampleRate`).
    pub fn on_error(&self, kind: cpal::ErrorKind) {
        match kind {
            cpal::ErrorKind::DeviceNotAvailable => self.gone.store(true, Ordering::Release),
            cpal::ErrorKind::StreamInvalidated => self.invalidated.store(true, Ordering::Release),
            _ => {
                self.errors.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

pub struct CaptureProducer {
    data: Producer<f32>,
    drops: Producer<Dropped>,
    channels: u64,
    rate: u64,
    pos: u64,
    pending: Option<Dropped>,
    flags: Arc<StreamFlags>,
}

impl CaptureProducer {
    /// Called from the audio callback: copies or counts; never blocks or allocates.
    /// A drop is published before any later audio, so the consumer can place it exactly.
    pub fn push(&mut self, interleaved: &[f32]) {
        let frames = interleaved.len() as u64 / self.channels;
        if self.pos == 0 && self.flags.first_ms.load(Ordering::Relaxed) == 0 {
            let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
            let first = now.saturating_sub(frames * 1000 / self.rate).max(1);
            self.flags.first_ms.store(first, Ordering::Release);
        }
        if self.pending.is_none() && self.data.push_entire_slice(interleaved).is_ok() {
            self.pos += frames;
            return;
        }
        match &mut self.pending {
            Some(d) => d.frames += frames,
            None => self.pending = Some(Dropped { at_frame: self.pos, frames }),
        }
        self.pos += frames;
        if let Some(d) = self.pending {
            if self.drops.push(d).is_ok() {
                self.pending = None;
            }
        }
    }
}

pub enum Chunk<'a> {
    Audio(&'a [f32]),
    /// Input frames lost to overflow, to be replaced by silence.
    Silence(u64),
}

pub struct CaptureConsumer {
    data: Consumer<f32>,
    drops: Consumer<Dropped>,
    channels: usize,
    pos: u64,
    scratch: Vec<f32>,
}

impl CaptureConsumer {
    /// Delivers everything captured so far, in input order, with drops in place.
    pub fn drain(&mut self, mut f: impl FnMut(Chunk<'_>) -> Result<()>) -> Result<()> {
        loop {
            // Read the audio length before looking for drops: a drop published after this
            // read lies beyond the audio counted here, because it was published before any
            // audio that follows it.
            let available = (self.data.slots() / self.channels) as u64;
            let limit = match self.drops.peek() {
                Ok(d) if d.at_frame == self.pos => {
                    let d = *d;
                    let _ = self.drops.pop();
                    f(Chunk::Silence(d.frames))?;
                    self.pos += d.frames;
                    continue;
                }
                Ok(d) => available.min(d.at_frame - self.pos),
                Err(_) => available,
            };
            if limit == 0 {
                return Ok(());
            }
            let chunk = self.data.read_chunk(limit as usize * self.channels)?;
            let (a, b) = chunk.as_slices();
            self.scratch.clear();
            self.scratch.extend_from_slice(a);
            self.scratch.extend_from_slice(b);
            chunk.commit_all();
            f(Chunk::Audio(&self.scratch))?;
            self.pos += limit;
        }
    }
}

pub fn ring(channels: u16, rate: u32, seconds: u32) -> (CaptureProducer, CaptureConsumer, Arc<StreamFlags>) {
    ring_with_capacity(channels, rate, (rate * seconds) as usize)
}

fn ring_with_capacity(channels: u16, rate: u32, frames: usize) -> (CaptureProducer, CaptureConsumer, Arc<StreamFlags>) {
    let channels = channels.max(1);
    let (data_p, data_c) = RingBuffer::new(frames * channels as usize);
    let (drops_p, drops_c) = RingBuffer::new(DROP_EVENTS);
    let flags = Arc::new(StreamFlags::default());
    let producer = CaptureProducer {
        data: data_p,
        drops: drops_p,
        channels: channels as u64,
        rate: rate as u64,
        pos: 0,
        pending: None,
        flags: flags.clone(),
    };
    let consumer = CaptureConsumer { data: data_c, drops: drops_c, channels: channels as usize, pos: 0, scratch: Vec::with_capacity(frames * channels as usize) };
    (producer, consumer, flags)
}
```

Above the test module in `pipeline.rs`:

```rust
use std::ops::Range;

use anyhow::Result;
use uuid::Uuid;

use super::convert::{downmix, Resampler16k};
use super::frame::{Frame, FrameBuilder};

const SILENCE_BLOCK: u64 = 4096;

/// One source's conversion chain: downmix → 16 kHz → 100 ms frames (spec §3.4, §4.2).
pub struct Pipeline {
    channels: usize,
    input_rate: u64,
    in_frames: u64,
    resampler: Resampler16k,
    frames: FrameBuilder,
}

impl Pipeline {
    pub fn new(recording_id: Uuid, input_rate: u32, channels: u16) -> Result<Self> {
        Ok(Self {
            channels: channels.max(1) as usize,
            input_rate: input_rate as u64,
            in_frames: 0,
            resampler: Resampler16k::new(input_rate)?,
            frames: FrameBuilder::new(recording_id),
        })
    }

    pub fn push(&mut self, interleaved: &[f32]) -> Result<Vec<Frame>> {
        self.in_frames += (interleaved.len() / self.channels) as u64;
        Ok(self.frames.push(&self.resampler.push(&downmix(interleaved, self.channels))?))
    }

    /// Stands in silence for lost input, so later audio keeps its true position.
    /// Returns the lost interval in 16 kHz samples of this recording.
    pub fn push_silence(&mut self, input_frames: u64) -> Result<(Range<u64>, Vec<Frame>)> {
        let lost = self.to_output(self.in_frames)..self.to_output(self.in_frames + input_frames);
        let mut frames = Vec::new();
        let mut left = input_frames;
        while left > 0 {
            let n = left.min(SILENCE_BLOCK);
            self.in_frames += n;
            frames.extend(self.frames.push(&self.resampler.push(&vec![0.0; n as usize])?));
            left -= n;
        }
        Ok((lost, frames))
    }

    fn to_output(&self, input_frames: u64) -> u64 {
        (input_frames * 16_000 + self.input_rate / 2) / self.input_rate
    }

    pub fn finish(self) -> Result<Vec<Frame>> {
        let Pipeline { resampler, mut frames, .. } = self;
        let mut out = frames.push(&resampler.finish()?);
        out.extend(frames.finish());
        Ok(out)
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p lecturelive-core`
Expected: all pass. Run `concurrent_overflow_keeps_every_position_exact` five times (`cargo test -p lecturelive-core concurrent -- --test-threads=1` in a loop); it must pass every time.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/audio/mod.rs crates/core/src/audio/capture.rs crates/core/src/audio/pipeline.rs
git commit -m "Capture through a preallocated ring with positioned drop events

The callback copies into an rtrb ring or counts what did not fit. Each
drop is published before later audio, so the consumer replaces it with
silence at its exact position and sample offsets stay linear in time.
The pipeline reports the lost interval in 16 kHz samples."
```

---

### Task 3: Recorder header, positioned writes, launch repair, retention, folder lock (milestone task 6)

**Files:**
- Create: `crates/core/src/session/launch.rs`, `crates/core/src/session/lock.rs`
- Modify: `crates/core/src/audio/recorder.rs`, `crates/core/src/session/mod.rs`, `Cargo.toml` (`rust-version = "1.89"`)

**Interfaces:**
- Consumes: `Frame` (Task 1); `Sidecar`, `RecState`, `Gap`, `GapKind`, `sidecar_path` (Task 1).
- Produces:
  - `Recorder::create` writes and syncs the header before returning
  - `Recorder::write_frame(&mut self, &Frame) -> Result<()>`: pads silence up to `sample_offset`; rejects overlap
  - `Recorder::samples(&self) -> u64`
  - `session::launch::{recover, prunable, Retention, LaunchReport}`
    - `recover(dir: &Path, stem: &str, retention: Retention, now: DateTime<Local>) -> Result<LaunchReport>`
    - `Retention::{KeepAll, KeepDays(u32)}`
    - `LaunchReport { repaired: Vec<(PathBuf, u64)>, missing: Vec<PathBuf>, pruned: Vec<PathBuf> }`
  - `session::lock::FolderLock::acquire(dir: &Path) -> Result<FolderLock>`: held until dropped

- [ ] **Step 1: Write the failing tests**

Append to the test module in `crates/core/src/audio/recorder.rs`:

```rust
    fn frame(offset: u64, value: i16) -> crate::audio::frame::Frame {
        crate::audio::frame::Frame { recording_id: uuid::Uuid::nil(), sample_offset: offset, valid_samples: 1600, pcm16: [value; 1600] }
    }

    #[test]
    fn header_is_on_disk_before_any_audio() {
        let dir = tempfile::tempdir().unwrap();
        let r = Recorder::create(dir.path(), "session").unwrap();
        let path = r.path().to_path_buf();
        std::mem::forget(r); // crash straight after creation
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 44);
        assert_eq!(repair_header(&path).unwrap(), 0);
        assert_eq!(hound::WavReader::open(&path).unwrap().len(), 0);
    }

    #[test]
    fn write_frame_fills_skipped_offsets_with_silence() {
        let dir = tempfile::tempdir().unwrap();
        let mut r = Recorder::create(dir.path(), "session").unwrap();
        r.write_frame(&frame(0, 100)).unwrap();
        r.write_frame(&frame(3200, 300)).unwrap(); // 1600..3200 never arrived
        assert_eq!(r.samples(), 4800);
        let path = r.finalize().unwrap();
        let s: Vec<i16> = hound::WavReader::open(&path).unwrap().samples::<i16>().map(|x| x.unwrap()).collect();
        assert_eq!(s.len(), 4800);
        assert_eq!((s[0], s[1600], s[3199], s[3200]), (100, 0, 0, 300));
    }

    #[test]
    fn write_frame_rejects_overlap() {
        let dir = tempfile::tempdir().unwrap();
        let mut r = Recorder::create(dir.path(), "session").unwrap();
        r.write_frame(&frame(0, 1)).unwrap();
        assert!(r.write_frame(&frame(800, 1)).is_err());
    }
```

`crates/core/src/session/launch.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::recorder::Recorder;
    use crate::session::sidecar::{Gap, GapKind, RecordingEntry, Sidecar};
    use chrono::TimeZone;

    const STEM: &str = "lecture_notes_20260925";

    fn now() -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 9, 25, 12, 0, 0).unwrap()
    }

    fn entry(id: Uuid, file: &str, days_old: i64, state: RecState) -> RecordingEntry {
        RecordingEntry {
            id,
            file: file.into(),
            anchor: now() - chrono::Duration::days(days_old),
            source_uid: "BlackHole2ch_UID".into(),
            input_rate: 48_000,
            samples: Some(16_000),
            state,
        }
    }

    /// A recording killed mid-write: 2.5 s written, header last updated at the 2 s checkpoint.
    fn crashed_recording(dir: &Path) -> String {
        let mut r = Recorder::create(&dir.join("recordings"), "session_20260925_100000").unwrap();
        for _ in 0..25 {
            r.write(&[500; 1600]).unwrap();
        }
        let name = r.path().file_name().unwrap().to_string_lossy().to_string();
        std::mem::forget(r);
        format!("recordings/{name}")
    }

    #[test]
    fn open_recording_is_repaired_and_its_tail_marked_interrupted() {
        let dir = tempfile::tempdir().unwrap();
        let file = crashed_recording(dir.path());
        let id = Uuid::new_v4();
        let mut sc = Sidecar::default();
        sc.recordings.push(RecordingEntry { samples: None, ..entry(id, &file, 0, RecState::Open) });
        let path = sidecar_path(dir.path(), STEM);
        sc.save(&path).unwrap();

        let report = recover(dir.path(), STEM, Retention::KeepAll, now()).unwrap();
        assert_eq!(report.repaired.len(), 1);
        assert!(report.repaired[0].1 >= 32_000);

        let sc = Sidecar::load(&path).unwrap().unwrap();
        let r = &sc.recordings[0];
        assert_eq!(r.state, RecState::Repaired);
        assert_eq!(r.samples, Some(report.repaired[0].1));
        assert_eq!(sc.gaps, vec![Gap { recording_id: id, start_sample: report.repaired[0].1, end_sample: None, kind: GapKind::Interrupted, resolved: false }]);
        assert_eq!(hound::WavReader::open(dir.path().join(&file)).unwrap().len() as u64, report.repaired[0].1);

        let again = recover(dir.path(), STEM, Retention::KeepAll, now()).unwrap();
        assert!(again.repaired.is_empty(), "a second launch repairs nothing");
    }

    #[test]
    fn missing_open_recording_is_marked_missing() {
        let dir = tempfile::tempdir().unwrap();
        let id = Uuid::new_v4();
        let mut sc = Sidecar::default();
        sc.recordings.push(entry(id, "recordings/gone.wav", 0, RecState::Open));
        sc.save(&sidecar_path(dir.path(), STEM)).unwrap();
        let report = recover(dir.path(), STEM, Retention::KeepAll, now()).unwrap();
        assert_eq!(report.missing.len(), 1);
        let sc = Sidecar::load(&sidecar_path(dir.path(), STEM)).unwrap().unwrap();
        assert_eq!(sc.recordings[0].state, RecState::Missing);
        assert_eq!(sc.gaps[0].kind, GapKind::Interrupted);
    }

    #[test]
    fn corrupt_sidecar_is_left_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = sidecar_path(dir.path(), STEM);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"{\"version\":2,\"recordings\":[").unwrap();
        let err = recover(dir.path(), STEM, Retention::KeepAll, now()).unwrap_err();
        assert!(format!("{err:#}").contains("corrupt sidecar"), "{err:#}");
        assert_eq!(std::fs::read(&path).unwrap(), b"{\"version\":2,\"recordings\":[");
    }

    #[test]
    fn no_sidecar_is_a_fresh_folder() {
        let dir = tempfile::tempdir().unwrap();
        let report = recover(dir.path(), STEM, Retention::KeepDays(1), now()).unwrap();
        assert!(report.repaired.is_empty() && report.pruned.is_empty() && report.missing.is_empty());
        assert!(!sidecar_path(dir.path(), STEM).exists());
    }

    #[test]
    fn retention_keeps_recordings_with_unresolved_gaps() {
        let (old_clean, old_gap, old_resolved, recent, open) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let mut sc = Sidecar::default();
        sc.recordings.push(entry(old_clean, "a.wav", 30, RecState::Finalized));
        sc.recordings.push(entry(old_gap, "b.wav", 30, RecState::Repaired));
        sc.recordings.push(entry(old_resolved, "c.wav", 30, RecState::Finalized));
        sc.recordings.push(entry(recent, "d.wav", 1, RecState::Finalized));
        sc.recordings.push(entry(open, "e.wav", 30, RecState::Open));
        let gap = |id, resolved| Gap { recording_id: id, start_sample: 0, end_sample: Some(10), kind: GapKind::CaptureOverflow, resolved };
        sc.gaps.push(gap(old_gap, false));
        sc.gaps.push(gap(old_resolved, true));
        assert_eq!(prunable(&sc, Retention::KeepDays(14), now()), vec![old_clean, old_resolved]);
        assert!(prunable(&sc, Retention::KeepAll, now()).is_empty());
    }

    #[test]
    fn pruned_recordings_are_deleted_and_kept_in_the_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("recordings")).unwrap();
        std::fs::write(dir.path().join("recordings/old.wav"), b"x").unwrap();
        let id = Uuid::new_v4();
        let mut sc = Sidecar::default();
        sc.recordings.push(entry(id, "recordings/old.wav", 30, RecState::Finalized));
        sc.save(&sidecar_path(dir.path(), STEM)).unwrap();
        let report = recover(dir.path(), STEM, Retention::KeepDays(14), now()).unwrap();
        assert_eq!(report.pruned, vec![dir.path().join("recordings/old.wav")]);
        assert!(!dir.path().join("recordings/old.wav").exists());
        let sc = Sidecar::load(&sidecar_path(dir.path(), STEM)).unwrap().unwrap();
        assert_eq!(sc.recordings[0].state, RecState::Deleted);
    }
}
```

`crates/core/src/session/lock.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_has_one_session_at_a_time() {
        let dir = tempfile::tempdir().unwrap();
        let first = FolderLock::acquire(dir.path()).unwrap();
        let err = FolderLock::acquire(dir.path()).err().unwrap();
        assert!(format!("{err:#}").contains("another LectureLive session"), "{err:#}");
        drop(first);
        FolderLock::acquire(dir.path()).unwrap();
    }
}
```

`crates/core/src/session/mod.rs`:

```rust
pub mod launch;
pub mod lock;
pub mod sidecar;
```

Root `Cargo.toml`: `rust-version = "1.89"`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p lecturelive-core recorder launch lock`
Expected: compile errors: `write_frame`, `samples`, `recover`, `FolderLock` not found; `header_is_on_disk_before_any_audio` would fail on a 0-byte file.

- [ ] **Step 3: Implement**

In `crates/core/src/audio/recorder.rs`, add a `samples: u64` field (initialised to 0). At the end of `create`, write the header to disk before returning:

```rust
        let sync_handle = file.try_clone()?;
        let writer = WavWriter::new(BufWriter::new(file), spec())?;
        let mut r = Self { writer, sync_handle, path, since_checkpoint: 0, samples: 0 };
        r.checkpoint()?; // the header reaches disk before any audio, so a crash leaves a repairable file
        Ok(r)
```

In `write`, after the loop add `self.samples += pcm.len() as u64;`. Add:

```rust
    pub fn samples(&self) -> u64 {
        self.samples
    }

    /// Writes a frame at its own offset; missing offsets before it become silence, so the
    /// file's timeline stays linear in wall-clock time (the gap itself is recorded by the caller).
    pub fn write_frame(&mut self, frame: &Frame) -> Result<()> {
        ensure!(
            frame.sample_offset >= self.samples,
            "frame at sample {} overlaps audio already written up to {}",
            frame.sample_offset,
            self.samples
        );
        const SILENCE: [i16; FRAME_SAMPLES] = [0; FRAME_SAMPLES];
        let mut pad = frame.sample_offset - self.samples;
        while pad > 0 {
            let n = pad.min(FRAME_SAMPLES as u64) as usize;
            self.write(&SILENCE[..n])?;
            pad -= n as u64;
        }
        self.write(frame.pcm())
    }
```

with `use crate::audio::frame::{Frame, FRAME_SAMPLES};` at the top.

`crates/core/src/session/lock.rs`, above the tests:

```rust
use std::fs::{File, OpenOptions, TryLockError};
use std::path::Path;

use anyhow::{bail, Context, Result};

/// Exclusive advisory lock on a lecture folder (`.live_notes/lock`, spec §8); released on drop.
pub struct FolderLock {
    _file: File,
}

impl FolderLock {
    pub fn acquire(dir: &Path) -> Result<Self> {
        let path = dir.join(".live_notes").join("lock");
        std::fs::create_dir_all(path.parent().unwrap())?;
        let file = OpenOptions::new().create(true).truncate(false).write(true).open(&path).with_context(|| format!("open {}", path.display()))?;
        match file.try_lock() {
            Ok(()) => Ok(Self { _file: file }),
            Err(TryLockError::WouldBlock) => bail!("another LectureLive session is using {}", dir.display()),
            Err(TryLockError::Error(e)) => Err(e).with_context(|| format!("lock {}", path.display())),
        }
    }
}
```

`crates/core/src/session/launch.rs`, above the tests:

```rust
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Local};
use uuid::Uuid;

use crate::audio::recorder::repair_header;
use crate::session::sidecar::{sidecar_path, Gap, GapKind, RecState, Sidecar};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retention {
    KeepAll,
    KeepDays(u32),
}

#[derive(Debug, Default)]
pub struct LaunchReport {
    pub repaired: Vec<(PathBuf, u64)>,
    pub missing: Vec<PathBuf>,
    pub pruned: Vec<PathBuf>,
}

/// Runs before a session starts in `dir` (caller holds the folder lock): repairs recordings a
/// crash left open, marks their lost tails as gaps, then applies retention (spec §4.4).
pub fn recover(dir: &Path, stem: &str, retention: Retention, now: DateTime<Local>) -> Result<LaunchReport> {
    let path = sidecar_path(dir, stem);
    let Some(mut sc) = Sidecar::load(&path)? else { return Ok(LaunchReport::default()) };
    let mut report = LaunchReport::default();
    let mut gaps = Vec::new();
    for r in sc.recordings.iter_mut().filter(|r| r.state == RecState::Open) {
        let wav = dir.join(&r.file);
        let start_sample = if wav.exists() {
            let samples = repair_header(&wav).with_context(|| format!("repair {}", wav.display()))? as u64;
            r.samples = Some(samples);
            r.state = RecState::Repaired;
            report.repaired.push((wav, samples));
            samples
        } else {
            r.state = RecState::Missing;
            report.missing.push(wav);
            0
        };
        gaps.push(Gap { recording_id: r.id, start_sample, end_sample: None, kind: GapKind::Interrupted, resolved: false });
    }
    sc.gaps.extend(gaps);
    for id in prunable(&sc, retention, now) {
        let r = sc.recording_mut(id).expect("prunable ids come from the sidecar");
        let wav = dir.join(&r.file);
        match std::fs::remove_file(&wav) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("delete {}", wav.display())),
        }
        r.state = RecState::Deleted;
        report.pruned.push(wav);
    }
    if !(report.repaired.is_empty() && report.missing.is_empty() && report.pruned.is_empty()) {
        sc.save(&path)?;
    }
    Ok(report)
}

/// Closed recordings older than the retention window with no unresolved gap (spec §4.4).
pub fn prunable(sc: &Sidecar, retention: Retention, now: DateTime<Local>) -> Vec<Uuid> {
    let Retention::KeepDays(days) = retention else { return Vec::new() };
    let cutoff = now - chrono::Duration::days(days as i64);
    sc.recordings
        .iter()
        .filter(|r| matches!(r.state, RecState::Finalized | RecState::Repaired))
        .filter(|r| r.anchor < cutoff)
        .filter(|r| !sc.gaps.iter().any(|g| g.recording_id == r.id && !g.resolved))
        .map(|r| r.id)
        .collect()
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p lecturelive-core`
Expected: all pass, including M0's recorder tests. `checkpointed_audio_survives_a_crash` still holds, since checkpointing at create adds one early flush.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/core/src/audio/recorder.rs crates/core/src/session/mod.rs crates/core/src/session/launch.rs crates/core/src/session/lock.rs
git commit -m "Repair interrupted recordings at launch and write frames at their offsets

The WAV header now reaches disk at creation, so a crash in the first
quarter-second leaves a repairable file. Frames are written at their
sample offsets with silence for anything missing. Launch repairs open
recordings, marks their lost tails as gaps and prunes only closed
recordings with no unresolved gap. A folder lock keeps two sessions
from repairing each other's live recording."
```

---

### Task 4: Device identity, stream rebuild, microphone permission (milestone task 3)

**Files:**
- Create: `crates/core/src/audio/source.rs`, `crates/core/src/audio/permission.rs`
- Modify: `crates/core/src/audio/input.rs`, `crates/core/src/audio/mod.rs`, `crates/core/Cargo.toml`

**Interfaces:**
- Consumes: `capture::{ring, Chunk, StreamFlags, RING_SECONDS}`, `Pipeline` (Task 2); `Gap`, `GapKind` (Task 1).
- Produces:
  - `InputInfo` gains `uid: String` (the CoreAudio device UID, from cpal's `Device::id()`)
  - `input::find_input(uid: &str) -> Result<Option<cpal::Device>>`
  - `audio::source::{Source, SourceEvent, DeviceSource}`
    - `trait Source: Send + 'static { fn run(self: Box<Self>, out: tokio::sync::mpsc::Sender<SourceEvent>, stop: Arc<AtomicBool>); }`
    - `SourceEvent` variants:
      - `Begin { recording_id: Uuid, anchor: DateTime<Local>, source_uid: String, input_rate: u32, channels: u16 }`
      - `Frame(Frame)`, `Gap(Gap)`, `Level(f32)`
      - `End { recording_id: Uuid, samples: u64, stream_errors: u64 }`
      - `DeviceGone { uid: String }`, `DeviceBack { uid: String }`, `Failed(String)`
    - `DeviceSource { uid: String }`
  - `audio::permission::{microphone, MicPermission}`: `MicPermission::{NotDetermined, Restricted, Denied, Granted}`

Event order per recording: `Begin`, then `Frame`s and overflow `Gap`s, then `End`. A recording that ends because its device went away or changed rate is followed by a `Gap` with `end_sample: None` and kind `DeviceGone` or `RateChange`. A disappearance also sends `DeviceGone`, and later `DeviceBack` when the same UID returns. No other device is ever opened (spec §4.1).

- [ ] **Step 1: Add the dependency**

```bash
cargo add -p lecturelive-core objc2-av-foundation --no-default-features --features std,AVCaptureDevice,AVMediaFormat
```

Record the resolved version in the ledger.

- [ ] **Step 2: Write the failing tests**

`crates/core/src/audio/permission.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// Reports the permission of whatever process runs the tests; the value is not asserted.
    #[test]
    fn microphone_status_is_readable() {
        let p = microphone();
        println!("microphone permission: {p:?}");
    }
}
```

`crates/core/src/audio/source.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::routing::BLACKHOLE_UID;

    /// Needs BlackHole 2ch and microphone permission for the process running the test:
    /// cargo test -p lecturelive-core source -- --ignored --nocapture
    #[test]
    #[ignore]
    fn blackhole_records_contiguous_frames_by_uid() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(1024);
        let stop = Arc::new(AtomicBool::new(false));
        let s = stop.clone();
        let t = std::thread::spawn(move || Box::new(DeviceSource { uid: BLACKHOLE_UID.into() }).run(tx, s));
        std::thread::sleep(Duration::from_secs(2));
        stop.store(true, Ordering::Relaxed);
        t.join().unwrap();
        let mut events = Vec::new();
        while let Ok(e) = rx.try_recv() {
            events.push(e);
        }
        let SourceEvent::Begin { recording_id, input_rate, .. } = &events[0] else { panic!("first event must be Begin: {:?}", events.first()) };
        let mut next = 0;
        for e in &events {
            if let SourceEvent::Frame(f) = e {
                assert_eq!(f.recording_id, *recording_id);
                assert_eq!(f.sample_offset, next);
                next += 1600;
            }
        }
        let Some(SourceEvent::End { samples, stream_errors, .. }) = events.last() else { panic!("last event must be End") };
        println!("{input_rate} Hz, {samples} samples, {stream_errors} stream errors");
        assert!((28_800..=36_800).contains(samples), "about 2 s: {samples}");
    }
}
```

Register in `crates/core/src/audio/mod.rs`: `pub mod permission;` and `pub mod source;`.

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p lecturelive-core permission source`
Expected: compile errors: `microphone`, `DeviceSource` not found.

- [ ] **Step 4: Implement**

`permission.rs`, above the tests:

```rust
use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaTypeAudio};

/// Microphone (TCC) status of the responsible process. A CLI started from a terminal gets
/// the terminal's grant. Denial must not read as silence (M0 finding).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MicPermission {
    NotDetermined,
    Restricted,
    Denied,
    Granted,
}

pub fn microphone() -> MicPermission {
    let Some(audio) = (unsafe { AVMediaTypeAudio }) else { return MicPermission::NotDetermined };
    match unsafe { AVCaptureDevice::authorizationStatusForMediaType(audio) } {
        s if s == AVAuthorizationStatus::Authorized => MicPermission::Granted,
        s if s == AVAuthorizationStatus::Denied => MicPermission::Denied,
        s if s == AVAuthorizationStatus::Restricted => MicPermission::Restricted,
        _ => MicPermission::NotDetermined,
    }
}
```

In `input.rs`, add the UID to `InputInfo` and a lookup by UID:

```rust
pub struct InputInfo {
    pub name: String,
    pub uid: String,
    pub sample_rate: u32,
    pub channels: u16,
}
```

In `list_inputs`, push `InputInfo { name: d.description()?.name().to_string(), uid: d.id()?.id().to_string(), sample_rate: cfg.sample_rate(), channels: cfg.channels() }`. Add:

```rust
/// Inputs are identified by CoreAudio device UID (spec §4.1); cpal's device id is that UID.
pub fn find_input(uid: &str) -> Result<Option<cpal::Device>> {
    Ok(cpal::default_host().input_devices()?.find(|d| d.id().map(|i| i.id() == uid).unwrap_or(false)))
}
```

`source.rs`, above the tests:

```rust
//! The source worker: owns the cpal stream on its own thread, turns ring contents into
//! timed frames, and rebuilds the stream on disappearance or rate change (spec §4.1).
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Local, TimeZone};
use cpal::traits::{DeviceTrait, StreamTrait};
use tokio::sync::mpsc::Sender;
use uuid::Uuid;

use super::capture::{ring, Chunk, RING_SECONDS};
use super::frame::Frame;
use super::input::find_input;
use super::pipeline::Pipeline;
use crate::session::sidecar::{Gap, GapKind};

const POLL: Duration = Duration::from_millis(20);
const REAPPEAR_POLL: Duration = Duration::from_millis(500);

#[derive(Debug)]
pub enum SourceEvent {
    Begin { recording_id: Uuid, anchor: DateTime<Local>, source_uid: String, input_rate: u32, channels: u16 },
    Frame(Frame),
    Gap(Gap),
    End { recording_id: Uuid, samples: u64, stream_errors: u64 },
    Level(f32),
    DeviceGone { uid: String },
    DeviceBack { uid: String },
    Failed(String),
}

pub trait Source: Send + 'static {
    /// Runs on a dedicated thread until `stop` is set or the source cannot continue.
    fn run(self: Box<Self>, out: Sender<SourceEvent>, stop: Arc<AtomicBool>);
}

pub struct DeviceSource {
    pub uid: String,
}

impl Source for DeviceSource {
    fn run(self: Box<Self>, out: Sender<SourceEvent>, stop: Arc<AtomicBool>) {
        if let Err(e) = run_device(&self.uid, &out, &stop) {
            let _ = out.blocking_send(SourceEvent::Failed(format!("{e:#}")));
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum SegmentEnd {
    Stopped,
    Gone,
    Invalidated,
}

fn send(out: &Sender<SourceEvent>, e: SourceEvent) -> Result<()> {
    out.blocking_send(e).map_err(|_| anyhow::anyhow!("session closed"))
}

fn run_device(uid: &str, out: &Sender<SourceEvent>, stop: &AtomicBool) -> Result<()> {
    let mut device = find_input(uid)?.with_context(|| format!("input {uid} not found; `lecturelive inputs` lists them"))?;
    loop {
        match run_segment(&device, uid, out, stop)? {
            SegmentEnd::Stopped => return Ok(()),
            SegmentEnd::Invalidated => {} // same device, new configuration: rebuild at once
            SegmentEnd::Gone => {
                send(out, SourceEvent::DeviceGone { uid: uid.into() })?;
                // Wait for this device only: never switch to another source (spec §4.1).
                device = loop {
                    if stop.load(Ordering::Relaxed) {
                        return Ok(());
                    }
                    std::thread::sleep(REAPPEAR_POLL);
                    if let Some(d) = find_input(uid)? {
                        break d;
                    }
                };
                send(out, SourceEvent::DeviceBack { uid: uid.into() })?;
            }
        }
    }
}

fn run_segment(device: &cpal::Device, uid: &str, out: &Sender<SourceEvent>, stop: &AtomicBool) -> Result<SegmentEnd> {
    let config = device.default_input_config()?;
    let rate = config.sample_rate();
    let channels = config.channels();
    let (mut producer, mut consumer, flags) = ring(channels, rate, RING_SECONDS);
    let err_flags = flags.clone();
    let on_error = move |e: cpal::Error| err_flags.on_error(e.kind());
    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => {
            device.build_input_stream(config.config(), move |d: &[f32], _: &cpal::InputCallbackInfo| producer.push(d), on_error, None)?
        }
        cpal::SampleFormat::I16 => {
            let mut scratch: Vec<f32> = Vec::with_capacity(16_384 * channels as usize);
            device.build_input_stream(
                config.config(),
                move |d: &[i16], _: &cpal::InputCallbackInfo| {
                    scratch.clear(); // within capacity: no allocation in the callback
                    scratch.extend(d.iter().map(|&s| s as f32 / i16::MAX as f32));
                    producer.push(&scratch);
                },
                on_error,
                None,
            )?
        }
        other => bail!("unsupported sample format {other:?}"),
    };
    stream.play()?;

    let recording_id = Uuid::new_v4();
    let mut pipeline = Pipeline::new(recording_id, rate, channels)?;
    let mut samples = 0u64;
    let (mut level_sq, mut level_n) = (0f64, 0u64);
    let mut emit = |f: Frame, samples: &mut u64| -> Result<()> {
        *samples = f.sample_offset + f.valid_samples as u64;
        for &s in f.pcm() {
            level_sq += (s as f64 / i16::MAX as f64).powi(2);
        }
        level_n += f.valid_samples as u64;
        if level_n >= 16_000 {
            let _ = out.try_send(SourceEvent::Level((level_sq / level_n as f64).sqrt() as f32));
            (level_sq, level_n) = (0.0, 0);
        }
        send(out, SourceEvent::Frame(f))
    };
    let mut pump = |pipeline: &mut Pipeline, samples: &mut u64| -> Result<()> {
        consumer.drain(|chunk| match chunk {
            Chunk::Audio(a) => pipeline.push(a)?.into_iter().try_for_each(|f| emit(f, samples)),
            Chunk::Silence(n) => {
                let (lost, frames) = pipeline.push_silence(n)?;
                send(out, SourceEvent::Gap(Gap { recording_id, start_sample: lost.start, end_sample: Some(lost.end), kind: GapKind::CaptureOverflow, resolved: false }))?;
                frames.into_iter().try_for_each(|f| emit(f, samples))
            }
        })
    };

    let mut begun = false;
    let mut begin = |ms: u64| -> Result<()> {
        let anchor = Local.timestamp_millis_opt(ms as i64).single().context("anchor time")?;
        send(out, SourceEvent::Begin { recording_id, anchor, source_uid: uid.into(), input_rate: rate, channels })
    };
    let end = loop {
        if !begun {
            if let Some(ms) = flags.anchor_ms() {
                begin(ms)?;
                begun = true;
            }
        }
        if begun {
            pump(&mut pipeline, &mut samples)?;
        }
        if stop.load(Ordering::Relaxed) {
            break SegmentEnd::Stopped;
        }
        if flags.gone.load(Ordering::Acquire) {
            break SegmentEnd::Gone;
        }
        if flags.invalidated.load(Ordering::Acquire) {
            break SegmentEnd::Invalidated;
        }
        std::thread::sleep(POLL);
    };
    drop(stream);
    if !begun {
        match flags.anchor_ms() {
            Some(ms) => begin(ms)?,
            None => return Ok(end), // no audio ever arrived: nothing to record
        }
    }
    pump(&mut pipeline, &mut samples)?;
    for f in pipeline.finish()? {
        emit(f, &mut samples)?;
    }
    send(out, SourceEvent::End { recording_id, samples, stream_errors: flags.errors.load(Ordering::Relaxed) })?;
    let kind = match end {
        SegmentEnd::Stopped => return Ok(end),
        SegmentEnd::Gone => GapKind::DeviceGone,
        SegmentEnd::Invalidated => GapKind::RateChange,
    };
    send(out, SourceEvent::Gap(Gap { recording_id, start_sample: samples, end_sample: None, kind, resolved: false }))?;
    Ok(end)
}
```

The closures `emit`, `pump` and `begin` each capture different state. If the borrow checker rejects one of them sharing `out` or `pipeline`, make it a small `fn` that takes the state as arguments. The behaviour is the contract, not the closure shape.

- [ ] **Step 5: Run the tests**

Run: `cargo test -p lecturelive-core` → all non-ignored tests pass.
Run: `cargo test -p lecturelive-core source -- --ignored --nocapture` → `blackhole_records_contiguous_frames_by_uid` passes (BlackHole is installed, and the terminal has microphone permission). Note the printed rate and error count in the ledger.

- [ ] **Step 6: Commit**

```bash
git add Cargo.lock crates/core/Cargo.toml crates/core/src/audio/mod.rs crates/core/src/audio/input.rs crates/core/src/audio/permission.rs crates/core/src/audio/source.rs
git commit -m "Record an input by CoreAudio UID and rebuild its stream on device changes

The source worker opens the device by UID, turns the capture ring into
timed frames and reports stream errors. When the device disappears it
closes the recording, marks a gap and waits for the same UID; it never
opens another input. A sample-rate change starts a new recording at
once. Microphone permission is read so a denial is not taken for
silence."
```

---

### Task 5: Session coordinator (milestone task 4)

**Files:**
- Create: `crates/core/src/session/coordinator.rs`
- Modify: `crates/core/src/session/mod.rs`

**Interfaces:**
- Consumes: `Source`, `SourceEvent` (Task 4); `Recorder::{create, write_frame, finalize}` (Task 3); `Sidecar`, `RecordingEntry`, `RecState`, `Gap`, `GapKind`, `sidecar_path` (Task 1).
- Produces: `session::coordinator::{spawn, SessionConfig, SessionHandle, Notification, StopReport}`
  - `spawn(cfg: SessionConfig, source: Box<dyn Source>) -> (SessionHandle, mpsc::Receiver<Notification>)`
  - `SessionConfig { dir: PathBuf, stem: String }`
  - `SessionHandle::{request_stop(&self), finish(self) -> Result<StopReport>}`
  - `Notification` variants:
    - `Recording { path: PathBuf }`, `Level(f32)`, `Gap(Gap)`
    - `DeviceGone { uid: String }`, `DeviceBack { uid: String }`
    - `Failed(String)`
  - `StopReport { recordings: Vec<(PathBuf, u64)>, gaps: usize, stream_errors: u64 }`

Ownership (spec §3.2): the coordinator task alone mutates the sidecar and decides what the recorder does. The recorder runs on its own thread behind a bounded queue (`FRAME_QUEUE` = 50 frames = 5 s). Frames are forwarded with `try_send`. A full queue becomes a `RecorderOverflow` gap, and the recorder pads those offsets with silence, so the coordinator never waits on the disk. The session ends when the source thread exits: after a stop request, when the source fails, or when a scripted test source runs out. Frames pass through the coordinator, which keeps a recording's begin, frames and end in one order at 10 frames/s. M1's loopback design gives the coordinator no route state to own: the system output is never changed (Task 6).

- [ ] **Step 1: Write the failing tests**

`crates/core/src/session/coordinator.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::frame::Frame;
    use chrono::TimeZone;

    struct Script(Vec<SourceEvent>);
    impl Source for Script {
        fn run(self: Box<Self>, out: mpsc::Sender<SourceEvent>, _stop: Arc<AtomicBool>) {
            for e in self.0 {
                out.blocking_send(e).unwrap();
            }
        }
    }

    /// Sends frames until stopped, like a live device.
    struct Endless;
    impl Source for Endless {
        fn run(self: Box<Self>, out: mpsc::Sender<SourceEvent>, stop: Arc<AtomicBool>) {
            let id = Uuid::new_v4();
            out.blocking_send(begin(id, 0)).unwrap();
            let mut n = 0;
            while !stop.load(Ordering::Relaxed) {
                out.blocking_send(frame(id, n)).unwrap();
                n += 1600;
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            out.blocking_send(SourceEvent::End { recording_id: id, samples: n, stream_errors: 0 }).unwrap();
        }
    }

    fn begin(id: Uuid, secs: u32) -> SourceEvent {
        SourceEvent::Begin {
            recording_id: id,
            anchor: Local.with_ymd_and_hms(2026, 9, 25, 10, 0, secs).unwrap(),
            source_uid: "Receiver_UID".into(),
            input_rate: 48_000,
            channels: 1,
        }
    }

    fn frame(id: Uuid, offset: u64) -> SourceEvent {
        SourceEvent::Frame(Frame { recording_id: id, sample_offset: offset, valid_samples: 1600, pcm16: [1000; 1600] })
    }

    fn cfg(dir: &Path) -> SessionConfig {
        SessionConfig { dir: dir.to_path_buf(), stem: "lecture_notes_20260925".into() }
    }

    async fn run(dir: &Path, source: impl Source) -> (Result<StopReport>, Vec<Notification>) {
        let (handle, mut notes) = spawn(cfg(dir), Box::new(source));
        let result = handle.finish().await;
        let mut seen = Vec::new();
        while let Ok(n) = notes.try_recv() {
            seen.push(n);
        }
        (result, seen)
    }

    fn samples(path: &Path) -> Vec<i16> {
        hound::WavReader::open(path).unwrap().samples::<i16>().map(|s| s.unwrap()).collect()
    }

    #[tokio::test]
    async fn device_gone_and_back_gives_two_recordings_and_a_gap() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let overflow = Gap { recording_id: a, start_sample: 3200, end_sample: Some(4800), kind: GapKind::CaptureOverflow, resolved: false };
        let gone = Gap { recording_id: a, start_sample: 6400, end_sample: None, kind: GapKind::DeviceGone, resolved: false };
        let script = Script(vec![
            begin(a, 0),
            frame(a, 0),
            frame(a, 1600),
            SourceEvent::Gap(overflow.clone()),
            frame(a, 4800),
            SourceEvent::End { recording_id: a, samples: 6400, stream_errors: 2 },
            SourceEvent::Gap(gone.clone()),
            SourceEvent::DeviceGone { uid: "Receiver_UID".into() },
            SourceEvent::DeviceBack { uid: "Receiver_UID".into() },
            begin(b, 9),
            frame(b, 0),
            SourceEvent::End { recording_id: b, samples: 1600, stream_errors: 0 },
        ]);
        let (report, notes) = run(dir.path(), script).await;
        let report = report.unwrap();

        assert_eq!(report.recordings.len(), 2);
        assert_eq!(report.stream_errors, 2);
        let first = samples(&report.recordings[0].0);
        assert_eq!(first.len(), 6400);
        assert!(first[3200..4800].iter().all(|&s| s == 0), "the lost interval is silence");
        assert!(first[..3200].iter().chain(&first[4800..]).all(|&s| s == 1000));
        assert_eq!(samples(&report.recordings[1].0).len(), 1600);

        let sc = Sidecar::load(&sidecar_path(dir.path(), "lecture_notes_20260925")).unwrap().unwrap();
        assert_eq!(sc.recordings.len(), 2);
        assert!(sc.recordings.iter().all(|r| r.state == RecState::Finalized && r.source_uid == "Receiver_UID"));
        assert_eq!(sc.recordings[0].samples, Some(6400));
        assert_eq!(sc.recordings[1].anchor, Local.with_ymd_and_hms(2026, 9, 25, 10, 0, 9).unwrap());
        assert_eq!(sc.gaps, vec![overflow, gone]);
        assert!(notes.iter().any(|n| matches!(n, Notification::DeviceGone { .. })));
        assert!(notes.iter().any(|n| matches!(n, Notification::DeviceBack { .. })));
    }

    #[tokio::test]
    async fn stop_request_ends_the_session_with_a_finalized_recording() {
        let dir = tempfile::tempdir().unwrap();
        let (handle, _notes) = spawn(cfg(dir.path()), Box::new(Endless));
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
        handle.request_stop();
        let report = handle.finish().await.unwrap();
        assert_eq!(report.recordings.len(), 1);
        let (path, n) = &report.recordings[0];
        assert!(*n > 0 && n % 1600 == 0);
        assert_eq!(samples(path).len() as u64, *n);
    }

    #[tokio::test]
    async fn unwritable_recordings_dir_stops_with_the_path() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let rec = dir.path().join("recordings");
        std::fs::create_dir_all(&rec).unwrap();
        std::fs::set_permissions(&rec, std::fs::Permissions::from_mode(0o555)).unwrap();
        let id = Uuid::new_v4();
        let (result, notes) = run(dir.path(), Script(vec![begin(id, 0), frame(id, 0)])).await;
        std::fs::set_permissions(&rec, std::fs::Permissions::from_mode(0o755)).unwrap();
        let err = format!("{:#}", result.unwrap_err());
        assert!(err.contains("recordings"), "{err}");
        assert!(notes.iter().any(|n| matches!(n, Notification::Failed(_))));
    }

    #[tokio::test]
    async fn a_source_that_never_begins_leaves_no_recording() {
        let dir = tempfile::tempdir().unwrap();
        let (report, _) = run(dir.path(), Script(vec![])).await;
        assert!(report.unwrap().recordings.is_empty());
        assert!(!sidecar_path(dir.path(), "lecture_notes_20260925").exists());
    }

    #[tokio::test]
    async fn source_failure_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let (result, _) = run(dir.path(), Script(vec![SourceEvent::Failed("input X not found".into())])).await;
        assert!(format!("{:#}", result.unwrap_err()).contains("input X not found"));
    }
}
```

`crates/core/src/session/mod.rs` gets `pub mod coordinator;`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p lecturelive-core coordinator`
Expected: compile errors: `spawn`, `SessionConfig`, `Notification` not found.

- [ ] **Step 3: Implement**

Above the tests in `coordinator.rs`:

```rust
//! The session coordinator (spec §3.2): one task owns the sidecar and the recorder's
//! instructions; the source and the recorder are workers behind bounded channels.
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use chrono::Local;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::audio::frame::Frame;
use crate::audio::recorder::Recorder;
use crate::audio::source::{Source, SourceEvent};
use crate::session::sidecar::{sidecar_path, Gap, GapKind, RecState, RecordingEntry, Sidecar};

/// Frames the recorder may fall behind before frames become a recorder gap (5 s).
const FRAME_QUEUE: usize = 50;

pub struct SessionConfig {
    pub dir: PathBuf,
    pub stem: String,
}

#[derive(Debug)]
pub enum Notification {
    Recording { path: PathBuf },
    Level(f32),
    Gap(Gap),
    DeviceGone { uid: String },
    DeviceBack { uid: String },
    Failed(String),
}

#[derive(Debug, Default)]
pub struct StopReport {
    pub recordings: Vec<(PathBuf, u64)>,
    pub gaps: usize,
    pub stream_errors: u64,
}

pub struct SessionHandle {
    stop: mpsc::Sender<()>,
    task: JoinHandle<Result<StopReport>>,
}

impl SessionHandle {
    pub fn request_stop(&self) {
        let _ = self.stop.try_send(());
    }

    /// Waits for the session to end (after `request_stop`, or when the source ends).
    pub async fn finish(self) -> Result<StopReport> {
        self.task.await?
    }
}

pub fn spawn(cfg: SessionConfig, source: Box<dyn Source>) -> (SessionHandle, mpsc::Receiver<Notification>) {
    let (stop_tx, stop_rx) = mpsc::channel(1);
    let (notify_tx, notify_rx) = mpsc::channel(256);
    let task = tokio::spawn(run(cfg, source, stop_rx, notify_tx));
    (SessionHandle { stop: stop_tx, task }, notify_rx)
}

enum RecMsg {
    Open { recording_id: Uuid, recorder: Recorder },
    Frame(Frame),
    Finish { recording_id: Uuid },
}

enum RecorderEvent {
    Finished { recording_id: Uuid, path: PathBuf, samples: u64 },
    Failed { path: PathBuf, error: String },
}

/// Recorder thread: writes and finalizes; never touches the network or the sidecar.
fn recorder_worker(mut rx: mpsc::Receiver<RecMsg>, events: mpsc::Sender<RecorderEvent>) {
    let mut current: Option<(Uuid, Recorder)> = None;
    let finish = |id: Uuid, r: Recorder, events: &mpsc::Sender<RecorderEvent>| {
        let path = r.path().to_path_buf();
        let samples = r.samples();
        let e = match r.finalize() {
            Ok(path) => RecorderEvent::Finished { recording_id: id, path, samples },
            Err(e) => RecorderEvent::Failed { path, error: format!("{e:#}") },
        };
        let _ = events.blocking_send(e);
    };
    while let Some(msg) = rx.blocking_recv() {
        match msg {
            RecMsg::Open { recording_id, recorder } => current = Some((recording_id, recorder)),
            RecMsg::Frame(f) => {
                if let Some((_, r)) = current.as_mut().filter(|(id, _)| *id == f.recording_id) {
                    if let Err(e) = r.write_frame(&f) {
                        let _ = events.blocking_send(RecorderEvent::Failed { path: r.path().to_path_buf(), error: format!("{e:#}") });
                        current = None;
                    }
                }
            }
            RecMsg::Finish { recording_id } => {
                if let Some((id, r)) = current.take() {
                    if id == recording_id {
                        finish(id, r, &events);
                    } else {
                        current = Some((id, r));
                    }
                }
            }
        }
    }
    if let Some((id, r)) = current.take() {
        finish(id, r, &events);
    }
}

struct Coordinator {
    dir: PathBuf,
    sidecar_path: PathBuf,
    sidecar: Sidecar,
    dirty: bool,
    rec_tx: Option<mpsc::Sender<RecMsg>>, // None once the session is closing
    notify: mpsc::Sender<Notification>,
    overflow: Option<usize>, // index in sidecar.gaps of the recorder-overflow gap still growing
    report: StopReport,
}

impl Coordinator {
    async fn save(&mut self) -> Result<()> {
        let (sc, path) = (self.sidecar.clone(), self.sidecar_path.clone());
        tokio::task::spawn_blocking(move || sc.save(&path)).await??;
        self.dirty = false;
        Ok(())
    }

    fn notify(&self, n: Notification) {
        let _ = self.notify.try_send(n); // notifications describe decisions already made; none is durable
    }

    fn recorder(&self) -> Result<&mpsc::Sender<RecMsg>> {
        self.rec_tx.as_ref().ok_or_else(|| anyhow!("recorder closed"))
    }

    async fn on_source(&mut self, ev: SourceEvent) -> Result<()> {
        match ev {
            SourceEvent::Begin { recording_id, anchor, source_uid, input_rate, .. } => {
                let dir = self.dir.join("recordings");
                let stem = format!("session_{}", anchor.format("%Y%m%d_%H%M%S"));
                let d = dir.clone();
                let recorder = tokio::task::spawn_blocking(move || Recorder::create(&d, &stem))
                    .await?
                    .with_context(|| format!("create a recording in {}", dir.display()))?;
                let path = recorder.path().to_path_buf();
                let file = path.strip_prefix(&self.dir).unwrap_or(&path).to_string_lossy().into_owned();
                self.sidecar.recordings.push(RecordingEntry { id: recording_id, file, anchor, source_uid, input_rate, samples: None, state: RecState::Open });
                self.save().await?;
                self.recorder()?.send(RecMsg::Open { recording_id, recorder }).await.map_err(|_| anyhow!("recorder stopped"))?;
                self.notify(Notification::Recording { path });
            }
            SourceEvent::Frame(f) => {
                let (id, start) = (f.recording_id, f.sample_offset);
                match self.recorder()?.try_send(RecMsg::Frame(f)) {
                    Ok(()) => {
                        if self.overflow.take().is_some() {
                            self.save().await?;
                        }
                    }
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        let end = start + crate::audio::frame::FRAME_SAMPLES as u64;
                        match self.overflow {
                            Some(i) => self.sidecar.gaps[i].end_sample = Some(end),
                            None => {
                                let gap = Gap { recording_id: id, start_sample: start, end_sample: Some(end), kind: GapKind::RecorderOverflow, resolved: false };
                                self.sidecar.gaps.push(gap.clone());
                                self.overflow = Some(self.sidecar.gaps.len() - 1);
                                self.save().await?;
                                self.notify(Notification::Gap(gap));
                            }
                        }
                        self.dirty = true;
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => {} // the recorder's own failure event ends the session
                }
            }
            SourceEvent::Gap(g) => {
                self.sidecar.gaps.push(g.clone());
                self.save().await?;
                self.notify(Notification::Gap(g));
            }
            SourceEvent::End { recording_id, stream_errors, .. } => {
                self.report.stream_errors += stream_errors;
                self.overflow = None;
                self.recorder()?.send(RecMsg::Finish { recording_id }).await.map_err(|_| anyhow!("recorder stopped"))?;
            }
            SourceEvent::Level(l) => self.notify(Notification::Level(l)),
            SourceEvent::DeviceGone { uid } => self.notify(Notification::DeviceGone { uid }),
            SourceEvent::DeviceBack { uid } => self.notify(Notification::DeviceBack { uid }),
            SourceEvent::Failed(msg) => return Err(anyhow!(msg)),
        }
        Ok(())
    }

    async fn on_recorder(&mut self, ev: RecorderEvent) -> Result<()> {
        match ev {
            RecorderEvent::Finished { recording_id, path, samples } => {
                if let Some(r) = self.sidecar.recording_mut(recording_id) {
                    r.samples = Some(samples);
                    r.state = RecState::Finalized;
                }
                self.save().await?;
                self.report.recordings.push((path, samples));
                Ok(())
            }
            RecorderEvent::Failed { path, error } => Err(anyhow!("recording to {} failed: {error}", path.display())),
        }
    }
}

async fn run(cfg: SessionConfig, source: Box<dyn Source>, mut stop_rx: mpsc::Receiver<()>, notify: mpsc::Sender<Notification>) -> Result<StopReport> {
    let sidecar_path = sidecar_path(&cfg.dir, &cfg.stem);
    let sidecar = Sidecar::load(&sidecar_path)?.unwrap_or_default();
    let stop = Arc::new(AtomicBool::new(false));
    let (src_tx, mut src_rx) = mpsc::channel(256);
    let source_thread = {
        let stop = stop.clone();
        std::thread::spawn(move || source.run(src_tx, stop))
    };
    let (rec_tx, rec_rx) = mpsc::channel(FRAME_QUEUE);
    let (rev_tx, mut rev_rx) = mpsc::channel(16);
    let recorder_thread = std::thread::spawn(move || recorder_worker(rec_rx, rev_tx));

    let mut c = Coordinator { dir: cfg.dir, sidecar_path, sidecar, dirty: false, rec_tx: Some(rec_tx), notify, overflow: None, report: StopReport::default() };
    let mut failure: Option<anyhow::Error> = None;
    let mut fail = |e: anyhow::Error, failure: &mut Option<anyhow::Error>| {
        stop.store(true, Ordering::Relaxed);
        failure.get_or_insert(e);
    };
    loop {
        tokio::select! {
            Some(()) = stop_rx.recv() => stop.store(true, Ordering::Relaxed),
            ev = src_rx.recv() => match ev {
                Some(ev) => if failure.is_none() {
                    if let Err(e) = c.on_source(ev).await { fail(e, &mut failure) }
                },
                None => break, // the source thread has exited
            },
            Some(ev) = rev_rx.recv() => if let Err(e) = c.on_recorder(ev).await { fail(e, &mut failure) },
        }
    }
    // Closing the queue makes the recorder finalize whatever is still open, then exit.
    c.rec_tx = None;
    while let Some(ev) = rev_rx.recv().await {
        if let Err(e) = c.on_recorder(ev).await {
            failure.get_or_insert(e);
        }
    }
    tokio::task::spawn_blocking(move || {
        let _ = source_thread.join();
        let _ = recorder_thread.join();
    })
    .await?;
    if c.dirty || !c.sidecar.recordings.is_empty() {
        c.save().await?;
    }
    c.report.gaps = c.sidecar.gaps.len();
    match failure {
        Some(e) => {
            c.notify(Notification::Failed(format!("{e:#}")));
            Err(e)
        }
        None => Ok(c.report),
    }
}
```

The order at the end of `run` is the contract: close the recorder queue, drain recorder events (each open recording reports `Finished`), join both threads, then save.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p lecturelive-core coordinator` then `cargo test -p lecturelive-core`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/session/mod.rs crates/core/src/session/coordinator.rs
git commit -m "Add the session coordinator that owns the sidecar and the recorder

One task records each recording's anchor and state and every gap, and
tells the recorder thread what to open, write and finalize through a
bounded queue. A full queue becomes a recorder gap rather than a wait.
A disk error or a failed source ends the session with the path or
reason, and nothing durable depends on notifications."
```

---

### Task 6: App-owned loopback device, preflight, silence warning (milestone task 5)

**Files:**
- Create: `crates/core/src/audio/loopback.rs`, `crates/core/src/audio/level.rs`, `crates/core/src/audio/tone.rs`
- Modify: `crates/core/src/audio/coreaudio.rs`, `crates/core/src/audio/routing.rs:65`, `crates/core/src/audio/mod.rs`, `crates/core/src/session/launch.rs`, `docs/spec.md` (§1.3 G4, §4.1, §4.3, §10, §12), `docs/milestones.md` (M1 section and Status row)

**Interfaces:**
- Consumes: `coreaudio::{default_output_device, device_uid, device_for_uid, create_multi_output, destroy_aggregate}`, `routing::{route_status, disable_loopback, BLACKHOLE_UID}` (M0).
- Produces:
  - `coreaudio::{is_aggregate(AudioObjectID) -> Result<bool>, aggregate_members(AudioObjectID) -> Result<Vec<String>>}`
  - `audio::loopback::{LOOPBACK_UID, LOOPBACK_NAME, DeviceAction, plan_device, choose_physical, setup, status, list_outputs, LoopbackStatus, OutputInfo}`
    - `plan_device(existing_members: Option<&[String]>, physical_uid: &str) -> DeviceAction`
    - `choose_physical(requested: Option<&str>, default_uid: &str, default_is_aggregate: bool) -> Result<String>`
    - `setup(output: Option<&str>) -> Result<(DeviceAction, String)>`
    - `status() -> Result<LoopbackStatus>`
    - `list_outputs() -> Result<Vec<OutputInfo>>`
    - `LoopbackStatus { present: bool, members: Vec<String>, blackhole_present: bool, default_output_uid: String }`
    - `OutputInfo { name: String, uid: String, is_aggregate: bool }`
  - `audio::level::{SilenceWatch, dbfs}`
    - `SilenceWatch::{new(threshold_dbfs: f32, secs: u32), observe(&mut self, level: f32) -> bool}`
    - `dbfs(level: f32) -> f32`
  - `audio::tone::play_tone(output_uid: &str, secs: f32, amplitude: f32) -> Result<()>`: for checks without Zoom
  - `session::launch::restore_abandoned_route(state_path: &Path) -> Result<Option<bool>>`

- [ ] **Step 1: Write the failing tests**

`crates/core/src/audio/loopback.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn members(a: &str, b: &str) -> Vec<String> {
        vec![a.to_string(), b.to_string()]
    }

    #[test]
    fn creates_keeps_or_rebuilds_by_members() {
        assert_eq!(plan_device(None, "BuiltInSpeakerDevice"), DeviceAction::Create);
        assert_eq!(plan_device(Some(&members(BLACKHOLE_UID, "BuiltInSpeakerDevice")), "BuiltInSpeakerDevice"), DeviceAction::Keep);
        assert_eq!(plan_device(Some(&members("BuiltInSpeakerDevice", BLACKHOLE_UID)), "BuiltInSpeakerDevice"), DeviceAction::Keep);
        assert_eq!(plan_device(Some(&members(BLACKHOLE_UID, "AirPods_UID")), "BuiltInSpeakerDevice"), DeviceAction::Rebuild);
        assert_eq!(plan_device(Some(&[BLACKHOLE_UID.to_string()]), "BuiltInSpeakerDevice"), DeviceAction::Rebuild);
    }

    #[test]
    fn a_physical_default_is_used_when_nothing_is_named() {
        assert_eq!(choose_physical(None, "BuiltInSpeakerDevice", false).unwrap(), "BuiltInSpeakerDevice");
        assert_eq!(choose_physical(Some("AirPods_UID"), "BuiltInSpeakerDevice", false).unwrap(), "AirPods_UID");
    }

    #[test]
    fn refuses_an_aggregate_default_as_the_physical_output() {
        let err = choose_physical(None, "~:AMS2_StackedOutput:1", true).unwrap_err();
        assert!(format!("{err}").contains("--output"), "{err}");
        assert_eq!(choose_physical(Some("BuiltInSpeakerDevice"), "~:AMS2_StackedOutput:1", true).unwrap(), "BuiltInSpeakerDevice");
    }

    #[test]
    fn refuses_blackhole_or_itself_as_the_physical_output() {
        assert!(choose_physical(Some(BLACKHOLE_UID), "BuiltInSpeakerDevice", false).is_err());
        assert!(choose_physical(Some(LOOPBACK_UID), "BuiltInSpeakerDevice", false).is_err());
        assert!(choose_physical(None, LOOPBACK_UID, true).is_err());
    }

    fn loudest_on_blackhole_while(play: impl FnOnce() + Send + 'static) -> f32 {
        let dir = tempfile::tempdir().unwrap();
        let player = std::thread::spawn(play);
        let mut loudest = 0f32;
        crate::audio::input::record_for("BlackHole 2ch", std::time::Duration::from_secs(3), dir.path(), |l| loudest = loudest.max(l)).unwrap();
        player.join().unwrap();
        crate::audio::level::dbfs(loudest)
    }

    /// Creates or keeps the real device and plays tones; the system output must not change.
    /// cargo test -p lecturelive-core loopback -- --ignored --nocapture
    #[test]
    #[ignore]
    fn device_carries_its_input_to_blackhole_without_touching_the_default_output() {
        let before = coreaudio::device_uid(coreaudio::default_output_device().unwrap()).unwrap();
        let (action, physical) = setup(Some("BuiltInSpeakerDevice")).unwrap();
        println!("setup: {action:?} with {physical}");
        let s = status().unwrap();
        assert!(s.present && s.members.contains(&BLACKHOLE_UID.to_string()) && s.members.contains(&physical));
        assert_eq!(s.default_output_uid, before, "setup must not change the system output");
        let via_device = loudest_on_blackhole_while(|| tone::play_tone(LOOPBACK_UID, 2.0, 0.05).unwrap());
        let direct = loudest_on_blackhole_while(|| tone::play_tone("BuiltInSpeakerDevice", 2.0, 0.05).unwrap());
        println!("BlackHole loudest: via {LOOPBACK_NAME} {via_device:.1} dBFS, straight to the speakers {direct:.1} dBFS");
        assert!(via_device > -40.0);
        assert!(direct < -90.0);
        assert_eq!(coreaudio::device_uid(coreaudio::default_output_device().unwrap()).unwrap(), before);
        assert_eq!(setup(Some("BuiltInSpeakerDevice")).unwrap().0, DeviceAction::Keep);
    }
}
```

`crates/core/src/audio/level.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn lin(db: f32) -> f32 {
        10f32.powf(db / 20.0)
    }

    #[test]
    fn warns_once_after_the_silent_run_and_again_after_signal_returns() {
        let mut w = SilenceWatch::new(-60.0, 3);
        assert!(!w.observe(lin(-80.0)));
        assert!(!w.observe(lin(-80.0)));
        assert!(w.observe(lin(-80.0)));
        assert!(!w.observe(0.0), "one warning per silent stretch");
        assert!(!w.observe(lin(-30.0)));
        assert!(!w.observe(lin(-90.0)));
        assert!(!w.observe(lin(-90.0)));
        assert!(w.observe(lin(-90.0)));
    }

    #[test]
    fn dbfs_of_silence_is_floored() {
        assert_eq!(dbfs(0.0), -120.0);
        assert!((dbfs(1.0)).abs() < 1e-6);
    }
}
```

Append to the tests in `crates/core/src/session/launch.rs`:

```rust
    #[test]
    fn no_saved_route_means_nothing_to_restore() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(restore_abandoned_route(&dir.path().join("route.json")).unwrap(), None);
    }
```

Register in `crates/core/src/audio/mod.rs`: `pub mod level;`, `pub mod loopback;`, `pub mod tone;`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p lecturelive-core loopback level no_saved_route`
Expected: compile errors: `plan_device`, `SilenceWatch`, `restore_abandoned_route` not found.

- [ ] **Step 3: Implement**

Append to `crates/core/src/audio/coreaudio.rs` (add `use core_foundation::array::CFArrayRef;` to the imports):

```rust
pub fn is_aggregate(id: AudioObjectID) -> Result<bool> {
    let a = addr(kAudioDevicePropertyTransportType);
    let mut t: u32 = 0;
    let mut size = size_of::<u32>() as u32;
    check(
        unsafe { AudioObjectGetPropertyData(id, &a, 0, null(), &mut size, &mut t as *mut _ as *mut c_void) },
        "get transport type",
    )?;
    Ok(t == kAudioDeviceTransportTypeAggregate)
}

/// UIDs of an aggregate's subdevices, in order.
pub fn aggregate_members(id: AudioObjectID) -> Result<Vec<String>> {
    let a = addr(kAudioAggregateDevicePropertyFullSubDeviceList);
    let mut list: CFArrayRef = std::ptr::null();
    let mut size = size_of::<CFArrayRef>() as u32;
    check(
        unsafe { AudioObjectGetPropertyData(id, &a, 0, null(), &mut size, &mut list as *mut _ as *mut c_void) },
        "get subdevice list",
    )?;
    let arr: CFArray<CFString> = unsafe { CFArray::wrap_under_create_rule(list) };
    Ok(arr.iter().map(|s| s.to_string()).collect())
}
```

`crates/core/src/audio/level.rs`, above the tests:

```rust
pub fn dbfs(level: f32) -> f32 {
    if level <= 1e-6 {
        -120.0
    } else {
        20.0 * level.log10()
    }
}

/// Warns once when `secs` consecutive one-second levels stay below the threshold: the sign
/// that Zoom's Speaker is no longer the loopback device (spec §4.3).
pub struct SilenceWatch {
    threshold_dbfs: f32,
    needed: u32,
    run: u32,
}

impl SilenceWatch {
    pub fn new(threshold_dbfs: f32, secs: u32) -> Self {
        Self { threshold_dbfs, needed: secs, run: 0 }
    }

    pub fn observe(&mut self, level: f32) -> bool {
        if dbfs(level) >= self.threshold_dbfs {
            self.run = 0;
            return false;
        }
        self.run += 1;
        self.run == self.needed
    }
}
```

`crates/core/src/audio/tone.rs`:

```rust
use std::time::Duration;

use anyhow::{ensure, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

/// Plays a 440 Hz tone on one output device by UID, without touching the system output.
/// Checks use it in place of Zoom: into "LectureLive Loopback" it takes Zoom's path, and into
/// BlackHole's output it feeds BlackHole silently.
pub fn play_tone(output_uid: &str, secs: f32, amplitude: f32) -> Result<()> {
    let device = cpal::default_host()
        .output_devices()?
        .find(|d| d.id().map(|i| i.id() == output_uid).unwrap_or(false))
        .with_context(|| format!("output {output_uid} not found"))?;
    let config = device.default_output_config()?;
    ensure!(config.sample_format() == cpal::SampleFormat::F32, "output {output_uid} is not f32");
    let rate = config.sample_rate() as f32;
    let channels = config.channels() as usize;
    let mut n = 0u64;
    let stream = device.build_output_stream(
        config.config(),
        move |out: &mut [f32], _: &cpal::OutputCallbackInfo| {
            for frame in out.chunks_mut(channels) {
                frame.fill((n as f32 * 440.0 * std::f32::consts::TAU / rate).sin() * amplitude);
                n += 1;
            }
        },
        |e| eprintln!("[tone] {e}"),
        None,
    )?;
    stream.play()?;
    std::thread::sleep(Duration::from_secs_f32(secs));
    Ok(())
}
```

`crates/core/src/audio/loopback.rs`, above the tests:

```rust
//! The app-owned Multi-Output device that Zoom's Speaker is set to (spec §4.3). It is
//! never made the system default, so a crash leaves nothing to restore.
use anyhow::{bail, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait};

use super::coreaudio;
pub use super::routing::BLACKHOLE_UID;

pub const LOOPBACK_UID: &str = "com.lecturelive.loopback";
pub const LOOPBACK_NAME: &str = "LectureLive Loopback";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceAction {
    Keep,
    Rebuild,
    Create,
}

#[derive(Debug)]
pub struct LoopbackStatus {
    pub present: bool,
    pub members: Vec<String>,
    pub blackhole_present: bool,
    pub default_output_uid: String,
}

pub struct OutputInfo {
    pub name: String,
    pub uid: String,
    pub is_aggregate: bool,
}

pub fn plan_device(existing_members: Option<&[String]>, physical_uid: &str) -> DeviceAction {
    match existing_members {
        None => DeviceAction::Create,
        Some(m) if m.len() == 2 && m.iter().any(|u| u == BLACKHOLE_UID) && m.iter().any(|u| u == physical_uid) => DeviceAction::Keep,
        Some(_) => DeviceAction::Rebuild,
    }
}

pub fn choose_physical(requested: Option<&str>, default_uid: &str, default_is_aggregate: bool) -> Result<String> {
    let uid = match requested {
        Some(u) => u,
        None if default_is_aggregate || default_uid == LOOPBACK_UID => bail!(
            "the system output {default_uid} is itself a multi-output device; name the headphones or speakers with --output <UID> (`lecturelive outputs` lists them)"
        ),
        None => default_uid,
    };
    if uid == BLACKHOLE_UID || uid == LOOPBACK_UID {
        bail!("{uid} cannot be the physical output of {LOOPBACK_NAME}");
    }
    Ok(uid.to_string())
}

/// Creates the device, or rebuilds it under the same UID and name when the physical output
/// changed. Run on request only, never during a recording: a rebuild under a running meeting
/// would move Zoom to another speaker.
pub fn setup(output: Option<&str>) -> Result<(DeviceAction, String)> {
    if coreaudio::device_for_uid(BLACKHOLE_UID)?.is_none() {
        bail!("BlackHole 2ch is not installed (brew install blackhole-2ch)");
    }
    let default_id = coreaudio::default_output_device()?;
    let physical = choose_physical(output, &coreaudio::device_uid(default_id)?, coreaudio::is_aggregate(default_id)?)?;
    let physical_id = coreaudio::device_for_uid(&physical)?.with_context(|| format!("output {physical} not found"))?;
    if coreaudio::is_aggregate(physical_id)? {
        bail!("{physical} is itself a multi-output device; name the headphones or speakers");
    }
    let existing = coreaudio::device_for_uid(LOOPBACK_UID)?;
    let members = existing.map(coreaudio::aggregate_members).transpose()?;
    let action = plan_device(members.as_deref(), &physical);
    if action == DeviceAction::Rebuild {
        coreaudio::destroy_aggregate(existing.expect("rebuild implies an existing device"))?;
    }
    if action != DeviceAction::Keep {
        // BlackHole is the clock: it never disconnects, so headphones leaving cannot take the clock away.
        coreaudio::create_multi_output(LOOPBACK_NAME, LOOPBACK_UID, BLACKHOLE_UID, &[BLACKHOLE_UID, &physical])?;
    }
    Ok((action, physical))
}

pub fn status() -> Result<LoopbackStatus> {
    let existing = coreaudio::device_for_uid(LOOPBACK_UID)?;
    Ok(LoopbackStatus {
        present: existing.is_some(),
        members: existing.map(coreaudio::aggregate_members).transpose()?.unwrap_or_default(),
        blackhole_present: coreaudio::device_for_uid(BLACKHOLE_UID)?.is_some(),
        default_output_uid: coreaudio::device_uid(coreaudio::default_output_device()?)?,
    })
}

pub fn list_outputs() -> Result<Vec<OutputInfo>> {
    let mut out = Vec::new();
    for d in cpal::default_host().output_devices()? {
        let uid = d.id()?.id().to_string();
        let is_aggregate = match coreaudio::device_for_uid(&uid)? {
            Some(id) => coreaudio::is_aggregate(id)?,
            None => false,
        };
        out.push(OutputInfo { name: d.description()?.name().to_string(), uid, is_aggregate });
    }
    Ok(out)
}
```

In `crates/core/src/audio/routing.rs`, the M0 canary's restore names what to do when the saved output is gone:

```rust
        let id = coreaudio::device_for_uid(&prev)?.with_context(|| {
            format!("previous output {prev} is gone: choose an output in System Settings → Sound → Output, then run `route off` again")
        })?;
```

Append to `crates/core/src/session/launch.rs` (above the tests):

```rust
/// The M0 canary can still make its own aggregate the system default. At launch, a route
/// it left behind is undone (spec §4.3). Returns None when no route was saved, otherwise
/// whether the default output was put back.
pub fn restore_abandoned_route(state_path: &Path) -> Result<Option<bool>> {
    if crate::audio::routing::load_state(state_path)?.is_none() {
        return Ok(None);
    }
    Ok(Some(crate::audio::routing::disable_loopback(state_path)?))
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p lecturelive-core` → all non-ignored tests pass.
Note the current default output first: `cargo run -q -p lecturelive-cli -- canary route status`. Then run: `cargo test -p lecturelive-core loopback -- --ignored --nocapture` → passes and prints both levels. Afterwards `canary route status` must show the same `default_output_uid`. If `direct` fails because something else was playing into "Macbook + Notes" at the time, rerun it once in silence. Record the levels in the ledger.

- [ ] **Step 5: Rewrite the spec to the chosen design**

In `docs/spec.md`, replace G4 in §1.3:

```markdown
- G4 System audio via BlackHole loopback through an app-owned Multi-Output device that only Zoom plays into; any physical input; mixed mode opt-in once it passes the drift gate (§4.2).
```

In §4.1, replace the Loopback bullet:

```markdown
- **Loopback**: the "BlackHole 2ch" input, fed by the app-owned "LectureLive Loopback" device that Zoom's Speaker is set to (§4.3).
```

Replace the whole of §4.3 (heading through the "Known trade-off" paragraph) with:

```markdown
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
   output stays on a device that does not include BlackHole (the headphones or speakers), so
   only Zoom reaches the transcript. Volume is set on the headphones or in Zoom; Zoom's
   speaker slider also lowers the level that is transcribed.
4. Preflight: play Zoom's Test Speaker and require a signal on the BlackHole input (loudest
   second above −60 dBFS). During a loopback recording, ten consecutive seconds below
   −60 dBFS raise a warning that names Zoom's Speaker setting. These are the only guards
   against Zoom's Speaker drifting to a physical device, which captures nothing.
5. A crash leaves nothing to undo: the device persists and the system output was never
   changed. The M0 canary can still make its own aggregate the system default; at launch, a
   route it left behind (saved state present) is undone, restoring the saved output only if
   the canary's aggregate is still the default. A user-made aggregate is never modified or
   deleted.
```

In §10, replace two rows:

```markdown
| Loopback preflight fails | Reason shown: Zoom's Speaker is not "LectureLive Loopback"; ten silent seconds during a loopback recording raise the same warning |
```

```markdown
| App crash | Recording valid to last checkpoint and repaired at launch; system output untouched; journal recovery; unclosed utterance becomes a gap |
```

In §12, the M1 row's gate: `16/44.1/48 kHz inputs; interrupted WAV repaired; a crash leaves the system output untouched; loopback preflight through the app-owned device; no unexplained captured-audio gaps over 30 min`.

- [ ] **Step 6: Update the M1 section of `docs/milestones.md`**

Status row: `| M1 | Recording + loopback foundation | in progress | M0 | Creates the "LectureLive Loopback" device; changes the system output only in the canary-route restore check, which restores it | [m1-recording-loopback](superpowers/plans/2026-09-25-m1-recording-loopback.md) |`

Task 5 line: `5. Loopback device: app-owned Multi-Output with BlackHole as clock, preflight and silence warning, abandoned canary route undone at launch (§4.3)`

Replace the **Gate** paragraph with a checklist:

```markdown
**Gate** (checked without a live lecture where the mechanism allows; plan Task 8):

- [ ] 16/44.1/48 kHz inputs framed correctly (unit)
- [ ] Interrupted WAV repaired on launch
- [ ] `kill -9` during a loopback recording leaves the system output and "LectureLive Loopback" unchanged, and a canary route left behind is undone at the next launch
- [ ] Loopback preflight passes with Zoom's Test Speaker played through "LectureLive Loopback"
- [ ] Unplugging the wireless receiver produces a marked gap and no automatic source switch

**At the first Zoom lecture after M1** (nothing else waits on it):

- [ ] A 30-minute Zoom recording with no unexplained captured-audio gaps
- [ ] M0's deferred check: the packaged `.app` captures the Zoom meeting window with a shared slide, and the image shows the slide
```

Remove the M0 section's "At the first Zoom lecture after M0" line and its item, since the new M1 line above carries it.

- [ ] **Step 7: Commit**

```bash
git add crates/core/src/audio/mod.rs crates/core/src/audio/coreaudio.rs crates/core/src/audio/routing.rs crates/core/src/audio/loopback.rs crates/core/src/audio/level.rs crates/core/src/audio/tone.rs crates/core/src/session/launch.rs docs/spec.md docs/milestones.md
git commit -m "Build loopback on an app-owned Multi-Output device Zoom is pinned to

LectureLive Loopback mirrors to BlackHole, which is its clock, and one
physical output, and is never made the system default, so a crash has
nothing to undo and other apps stay out of the transcript. Setup refuses
to nest a multi-output default and rebuilds only on request. A silence
watch covers Zoom's Speaker drifting away from the device. Spec §4.3 and
the M1 gate now describe this design."
```

---

### Task 7: CLI `record`, `inputs`, `outputs`, `loopback` (milestone task 7)

**Files:**
- Modify: `crates/cli/src/main.rs`, `crates/cli/Cargo.toml`

**Interfaces:**
- Consumes: everything above.
- Produces the commands used by Task 8:
  - `lecturelive inputs`: name, UID, rate, channels
  - `lecturelive outputs`: name, UID, whether it is a multi-output device
  - `lecturelive loopback setup [--output UID]`, `lecturelive loopback status`, `lecturelive loopback check [--secs 15]`
  - `lecturelive record (--loopback | --device UID) [--dir DIR] [--secs N] [--keep-days N]`. Default `--dir` is `~/Library/Application Support/LectureLive/record`; the stem is `lecture_notes_<YYYYMMDD>`.
  - `lecturelive canary tone --output UID [--secs 5] [--amp 0.1]`; all M0 `canary` commands are unchanged.

- [ ] **Step 1: Add features**

```bash
cargo add -p lecturelive-cli tokio --features rt-multi-thread,macros,signal,time
cargo add -p lecturelive-cli chrono
```

- [ ] **Step 2: Implement**

In `crates/cli/src/main.rs`, the `Cmd` enum gains the new commands next to `Canary`. The `Canary` enum gains `Tone`:

```rust
#[derive(Subcommand)]
enum Cmd {
    /// List inputs with their CoreAudio UIDs
    Inputs,
    /// List outputs with their CoreAudio UIDs
    Outputs,
    #[command(subcommand)]
    Loopback(Loopback),
    /// Record an input (or Zoom through BlackHole) into a lecture folder until Ctrl-C
    Record {
        #[arg(long, conflicts_with = "device")]
        loopback: bool,
        /// Input device UID (see `inputs`)
        #[arg(long)]
        device: Option<String>,
        #[arg(long)]
        dir: Option<PathBuf>,
        /// Stop after this many seconds
        #[arg(long)]
        secs: Option<u64>,
        /// Delete closed recordings older than this many days that have no unresolved gap
        #[arg(long)]
        keep_days: Option<u32>,
    },
    #[command(subcommand)]
    Canary(Canary),
}

#[derive(Subcommand)]
enum Loopback {
    /// Create or rebuild "LectureLive Loopback" (BlackHole + one physical output)
    Setup {
        #[arg(long)]
        output: Option<String>,
    },
    Status,
    /// Measure BlackHole while Zoom's Test Speaker plays
    Check {
        #[arg(long, default_value_t = 15)]
        secs: u64,
    },
}
```

and in `Canary`:

```rust
    /// Play a tone on one output device (no system output change)
    Tone {
        #[arg(long)]
        output: String,
        #[arg(long, default_value_t = 5.0)]
        secs: f32,
        #[arg(long, default_value_t = 0.1)]
        amp: f32,
    },
```

with the arm `Canary::Tone { output, secs, amp } => lecturelive_core::audio::tone::play_tone(&output, secs, amp)?,`. Move the existing canary `match c { … }` into `async fn canary(c: Canary) -> Result<()>`, unchanged. `main` becomes:

```rust
#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Inputs => {
            for i in input::list_inputs()? {
                println!("{:<32} {:<40} {} Hz  {} ch", i.name, i.uid, i.sample_rate, i.channels);
            }
        }
        Cmd::Outputs => {
            for o in loopback::list_outputs()? {
                println!("{:<32} {:<40} {}", o.name, o.uid, if o.is_aggregate { "multi-output" } else { "" });
            }
        }
        Cmd::Loopback(l) => loopback_cmd(l)?,
        Cmd::Record { loopback, device, dir, secs, keep_days } => record(loopback, device, dir, secs, keep_days).await?,
        Cmd::Canary(c) => canary(c).await?,
    }
    Ok(())
}

fn data_dir() -> Result<PathBuf> {
    let dir = dirs::data_dir().context("no Application Support dir")?.join("LectureLive");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn loopback_cmd(l: Loopback) -> Result<()> {
    match l {
        Loopback::Setup { output } => {
            let (action, physical) = loopback::setup(output.as_deref())?;
            println!("{}: {action:?} with BlackHole 2ch (clock) + {physical}", loopback::LOOPBACK_NAME);
            println!("Zoom → Settings → Audio → Speaker: choose \"{}\". Keep the system output on a device without BlackHole.", loopback::LOOPBACK_NAME);
        }
        Loopback::Status => println!("{:#?}", loopback::status()?),
        Loopback::Check { secs } => {
            println!("Play Zoom's Test Speaker (Zoom → Settings → Audio) now; measuring BlackHole for {secs} s…");
            let dir = data_dir()?.join("checks");
            let mut loudest = 0f32;
            let r = input::record_for("BlackHole 2ch", Duration::from_secs(secs), &dir, |l| {
                println!("  {:>6.1} dBFS", level::dbfs(l));
                loudest = loudest.max(l);
            })?;
            let db = level::dbfs(loudest);
            anyhow::ensure!(
                db > -60.0,
                "no signal on BlackHole (loudest {db:.1} dBFS): Zoom's Speaker must be \"{}\"",
                loopback::LOOPBACK_NAME
            );
            println!("pass: loudest {db:.1} dBFS ({})", r.path.display());
        }
    }
    Ok(())
}

async fn record(use_loopback: bool, device: Option<String>, dir: Option<PathBuf>, secs: Option<u64>, keep_days: Option<u32>) -> Result<()> {
    match permission::microphone() {
        MicPermission::Denied | MicPermission::Restricted => {
            anyhow::bail!("microphone access is denied for this terminal: System Settings → Privacy & Security → Microphone")
        }
        p => println!("microphone permission: {p:?}"),
    }
    let dir = match dir {
        Some(d) => d,
        None => data_dir()?.join("record"),
    };
    std::fs::create_dir_all(&dir)?;
    let _lock = FolderLock::acquire(&dir)?;
    if let Some(restored) = launch::restore_abandoned_route(&data_dir()?.join("route.json"))? {
        println!("undid a canary route left behind; default output restored: {restored}");
    }
    let stem = format!("lecture_notes_{}", chrono::Local::now().format("%Y%m%d"));
    let retention = keep_days.map_or(Retention::KeepAll, Retention::KeepDays);
    let report = launch::recover(&dir, &stem, retention, chrono::Local::now())?;
    for (p, n) in &report.repaired {
        println!("repaired {} ({:.1} s)", p.display(), *n as f64 / 16_000.0);
    }
    for p in &report.missing {
        println!("missing {} (marked as a gap)", p.display());
    }
    for p in &report.pruned {
        println!("deleted {} (retention)", p.display());
    }
    let uid = if use_loopback {
        let s = loopback::status()?;
        anyhow::ensure!(s.blackhole_present, "BlackHole 2ch is not installed (brew install blackhole-2ch)");
        if !s.present {
            println!("warning: {} is not set up (`lecturelive loopback setup`); recording BlackHole anyway", loopback::LOOPBACK_NAME);
        }
        loopback::BLACKHOLE_UID.to_string()
    } else {
        device.context("give --loopback or --device <UID> (`lecturelive inputs` lists them)")?
    };

    let (handle, mut notes) = coordinator::spawn(SessionConfig { dir, stem }, Box::new(DeviceSource { uid }));
    let mut watch = use_loopback.then(|| SilenceWatch::new(-60.0, 10));
    let timer = async {
        match secs {
            Some(s) => tokio::time::sleep(Duration::from_secs(s)).await,
            None => std::future::pending().await,
        }
    };
    tokio::pin!(timer);
    let mut stopping = false;
    let mut seconds = 0u64;
    loop {
        tokio::select! {
            n = notes.recv() => match n {
                None => break,
                Some(Notification::Recording { path }) => println!("recording to {}", path.display()),
                Some(Notification::Level(l)) => {
                    seconds += 1;
                    if seconds % 10 == 0 {
                        println!("{:>6} s  {:>6.1} dBFS", seconds, level::dbfs(l));
                    }
                    if watch.as_mut().is_some_and(|w| w.observe(l)) {
                        println!("warning: 10 s of silence on BlackHole — is Zoom's Speaker \"{}\"?", loopback::LOOPBACK_NAME);
                    }
                }
                Some(Notification::Gap(g)) => println!("gap: {:?} from sample {} to {:?} of {}", g.kind, g.start_sample, g.end_sample, g.recording_id),
                Some(Notification::DeviceGone { uid }) => println!(
                    "input {uid} disappeared at {}; waiting for it to return (no other input is used). Ctrl-C stops.",
                    chrono::Local::now().format("%H:%M:%S")
                ),
                Some(Notification::DeviceBack { uid }) => println!("input {uid} is back at {}; recording continues in a new file", chrono::Local::now().format("%H:%M:%S")),
                Some(Notification::Failed(msg)) => eprintln!("session failed: {msg}"),
            },
            _ = tokio::signal::ctrl_c(), if !stopping => { stopping = true; handle.request_stop(); }
            _ = &mut timer, if !stopping => { stopping = true; handle.request_stop(); }
        }
    }
    let report = handle.finish().await?;
    for (p, n) in &report.recordings {
        println!("{} — {:.1} s", p.display(), *n as f64 / 16_000.0);
    }
    println!("gaps in the sidecar: {}, stream errors: {}", report.gaps, report.stream_errors);
    Ok(())
}
```

Imports at the top of `main.rs`:

```rust
use lecturelive_core::audio::level::{self, SilenceWatch};
use lecturelive_core::audio::permission::{self, MicPermission};
use lecturelive_core::audio::source::DeviceSource;
use lecturelive_core::audio::{input, loopback, recorder, routing};
use lecturelive_core::session::coordinator::{self, Notification, SessionConfig};
use lecturelive_core::session::launch::{self, Retention};
use lecturelive_core::session::lock::FolderLock;
```

`route_state_path()` in the canary code becomes `data_dir()?.join("route.json")`, the same file.

`notes.recv()` returns `None` only after the coordinator task has ended, so the loop cannot spin once a stop has been requested.

- [ ] **Step 3: Smoke-test the commands**

```bash
cargo run -q -p lecturelive-cli -- inputs                 # BlackHole 2ch with BlackHole2ch_UID; MacBook Pro Microphone with its UID
cargo run -q -p lecturelive-cli -- outputs                # Macbook + Notes marked multi-output
cargo run -q -p lecturelive-cli -- loopback status
cargo run -q -p lecturelive-cli -- canary tone --output BlackHole2ch_UID --secs 8 --amp 0.1 &
cargo run -q -p lecturelive-cli -- record --loopback --secs 5 --dir "$HOME/Library/Application Support/LectureLive/m1-smoke"
```

Expected: `record` prints the microphone permission, `recording to …/recordings/session_….wav`, then one recording of about 5 s, `gaps in the sidecar: 0`. `cat "$HOME/Library/Application Support/LectureLive/m1-smoke/.live_notes/lecture_notes_$(date +%Y%m%d).v2.json"` shows one `finalized` recording with an anchor.

- [ ] **Step 4: Run all tests**

Run: `cargo test -p lecturelive-core` and `cargo build -p lecturelive-cli`
Expected: all pass. The desktop crate is not built.

- [ ] **Step 5: Commit**

```bash
git add crates/cli/Cargo.toml Cargo.lock crates/cli/src/main.rs
git commit -m "Add the record, inputs, outputs and loopback commands

record repairs the folder at launch, undoes any canary route left
behind, and records an input by UID or Zoom through BlackHole on the
coordinator until Ctrl-C or --secs. It reports gaps, device changes, a
denied microphone and ten silent seconds on loopback. loopback check
measures BlackHole while Zoom's Test Speaker plays."
```

---

### Task 8: Acceptance run and findings

**Files:**
- Modify: `docs/milestones.md` (M1 gate checkboxes, Findings, Status)

Nothing here waits for a live lecture. The session first runs every check it can alone (Steps 1–4). A person then gives one sitting of a few minutes (Step 5): Zoom's Test Speaker through the new device, the wireless receiver unplugged and replugged, and any permission prompt the CLI raises. The 30-minute Zoom recording and M0's slide capture go to the "At the first Zoom lecture after M1" line.

Define in every shell, from the repository root:

```bash
LL() { cargo run -q -p lecturelive-cli -- "$@"; }
AS="$HOME/Library/Application Support/LectureLive"
SC() { cat "$1/.live_notes/lecture_notes_$(date +%Y%m%d).v2.json"; }
```

- [ ] **Step 1: Unit gate and hardware tests**

```bash
cargo test -p lecturelive-core 2>&1 | tail -5                   # pass/fail/ignored counts
cargo test -p lecturelive-core frame -- --nocapture             # 16/44.1/48 kHz framing (gate line 1)
LL canary route status                                          # note default_output_uid
cargo test -p lecturelive-core source loopback -- --ignored --nocapture
LL canary route status                                          # default_output_uid unchanged
```

- [ ] **Step 2: Crash and launch repair** (gate lines 2 and 3; no person needed)

```bash
D="$AS/m1-crash"; rm -rf "$D"
BEFORE=$(LL canary route status | grep default_output_uid)
LL canary tone --output BlackHole2ch_UID --secs 60 --amp 0.1 &
LL record --loopback --dir "$D" --secs 120 & REC=$!
sleep 20; pkill -9 -f "target/debug/lecturelive record"; wait $REC 2>/dev/null
LL canary route status | grep default_output_uid                # same as $BEFORE
LL loopback status                                              # present: true, members unchanged
SC "$D"                                                         # one recording, state "open"
LL record --loopback --dir "$D" --secs 3                        # "repaired …/session_….wav (≈19–20 s)"
SC "$D"                                                         # first recording "repaired", an "interrupted" gap; second "finalized"
afinfo "$D"/recordings/session_*.wav | grep duration            # first ≥ seconds recorded before the kill − 1
afplay -t 2 "$(find "$D/recordings" -name 'session_*.wav' | sort | head -1)"   # exits 0
```

The kill must hit the recording process itself. If `pkill` pattern matches nothing, find the pid with `pgrep -fl lecturelive`.

- [ ] **Step 3: A canary route left behind** (gate line 3, second half)

```bash
LL canary route status                  # note default_output_uid
LL canary route on                      # the canary's aggregate is now the system default: the state a crash leaves
LL record --loopback --dir "$AS/m1-route" --secs 2   # "undid a canary route left behind; default output restored: true"
LL canary route status                  # default_output_uid as noted, saved: None
```

If `canary route on` fails because the default is a multi-output device, record the error, set the output back by name if it changed, and report this half as "not run" with the reason.

- [ ] **Step 4: Soak** (not a gate line; evidence for the deferred 30-minute line)

```bash
D="$AS/m1-soak"; rm -rf "$D"
LL canary tone --output BlackHole2ch_UID --secs 1820 --amp 0.1 &
LL record --loopback --dir "$D" --secs 1800 > "$AS/m1-soak.log" 2>&1
tail -3 "$AS/m1-soak.log"                                       # ≈1800 s, gaps 0, stream errors 0
SC "$D" | grep -c '"kind"'                                      # 0
```

This runs silently: the tone goes straight to BlackHole's output. Run it in the background while the other steps proceed. It shows the capture and recording path running 30 minutes without gaps. It does not replace the Zoom line.

- [ ] **Step 5: The sitting** (a person at the Mac, a few minutes)

Ask for the sitting once, listing every action up front, and run each command when the person says they're ready:

1. `LL loopback setup` (or `--output <UID of the headphones or speakers in use>`; `LL outputs` lists them). The person opens Zoom → Settings → Audio and sets **Speaker** to "LectureLive Loopback". Run `LL loopback check --secs 15`, and the person clicks **Test Speaker** at once. Expected: `pass: loudest … dBFS`, and the person hears the ringtone. Then the person chooses which Speaker setting Zoom keeps. The Python CLI records BlackHole either way. Tell them: with the system output on "Macbook + Notes", every app's sound also reaches BlackHole, and the system output has to be the plain speakers or headphones for only Zoom to be transcribed.
2. The person plugs in the wireless receiver. Run `LL inputs` and note its UID. Run `LL record --device <UID> --dir "$AS/m1-receiver" --secs 60`. About 15 s in, the person unplugs the receiver; about 30 s in, they plug it back in. Expected: `input … disappeared`, then `input … is back` and a second `recording to …`. `SC "$AS/m1-receiver"` shows two recordings, both with the receiver's `source_uid`, and a `device_gone` gap. No other input appears anywhere.
3. If macOS asks for microphone access for the terminal at any point, the person clicks Allow.

Record what the person said about the ringtone, and the Speaker setting Zoom was left on.

- [ ] **Step 6: Record versions**

`cargo tree -p lecturelive-core --depth 1 | grep -E "rtrb|uuid|objc2-av-foundation|chrono"` → into Findings.

- [ ] **Step 7: Update `docs/milestones.md`**

Tick each gate line that held. For any that failed, write what was observed and the spec §14.1 fallback it points to. Write **Findings** under the M1 section: new crate versions, the routing decision and its evidence (Step 1 levels, Step 5 check), crash and repair figures, the soak result, the receiver observation. Set M1's Status to `done` only if every gate line not under "At the first Zoom lecture after M1" holds; otherwise `in progress`.

- [ ] **Step 8: Commit**

```bash
git add docs/milestones.md
git commit -m "Record M1 findings

Findings cover the loopback device check, crash repair, the canary route
undo, the receiver unplug, a 30-minute soak and the crate versions added
in M1."
```
