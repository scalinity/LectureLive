# M0 Native Canary Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Prove on the real Mac, from a packaged app, that audio permission, BlackHole routing with restore, crash-safe recording, window capture and the STT websocket protocol all work as `docs/spec.md` assumes — before any real UI is built.

**Architecture:** A Cargo workspace with `crates/core` (library holding each capability behind a small function), `crates/cli` (canary subcommands for fast checks from the terminal) and a minimal Tauri 2 + Svelte 5 app (`apps/desktop`) that calls the same core functions, so permissions are exercised as a signed `.app` rather than inherited from Terminal.

**Tech Stack:** Rust (stable, MSRV 1.87), `hound`, `cpal`, `rubato`, `coreaudio-sys`, `core-foundation`, `xcap`, `image`, `tokio`, `tokio-tungstenite`, `clap`; Tauri 2, Svelte 5, TypeScript.

**Spec:** `docs/spec.md` (§4.1, §4.3, §4.4, §5.1, §7.1, §13). Gate: `docs/milestones.md` → M0.

## Global Constraints

- macOS 13+, Apple Silicon; single user; no telemetry, accounts or servers.
- Rust toolchain pinned in `rust-toolchain.toml` to the exact version recorded in Task 1; workspace `rust-version = "1.87"`.
- Tauri 2; Svelte 5 + TypeScript; no React; no `useEffect`-style effect hooks.
- The API key is read in Rust only (env `GROK_API_KEY` or `.env`); never sent to the frontend.
- Build/compile/test commands (`cargo build`, `cargo test`, `cargo tauri build`, `npm run build`) run only after the user has authorised builds for this execution; ask once at the start.
- Do not edit files with Python scripts.
- Commit messages: imperative, neutral, describe what and why; no co-author trailers; no names.
- STT fixtures contain only synthesised speech (macOS `say`), never lecture audio, because the repository is public.
- Crate APIs below are written against the versions named in each task. Where `cargo add` resolves a newer major version whose API differs, adapt the implementation; the tests in each task are the contract and must pass unchanged.

---

### Task 1: Workspace and crash-safe recorder

**Files:**
- Create: `Cargo.toml`, `rust-toolchain.toml`, `crates/core/Cargo.toml`, `crates/core/src/lib.rs`, `crates/core/src/audio/mod.rs`, `crates/core/src/audio/recorder.rs`
- Modify: `.gitignore`

**Interfaces:**
- Produces: `lecturelive_core::audio::recorder::{Recorder, repair_header, SAMPLE_RATE}`; `Recorder::create(dir: &Path, stem: &str) -> Result<Recorder>`, `Recorder::path(&self) -> &Path`, `Recorder::write(&mut self, pcm: &[i16]) -> Result<()>`, `Recorder::checkpoint(&mut self) -> Result<()>`, `Recorder::finalize(self) -> Result<PathBuf>`, `repair_header(path: &Path) -> Result<u32>` (returns sample count).

- [ ] **Step 1: Create the workspace**

`rustc --version` → write that exact version (e.g. `1.9x.y`) into `rust-toolchain.toml`:

```toml
[toolchain]
channel = "<exact version from rustc --version>"
```

`Cargo.toml`:

```toml
[workspace]
resolver = "2"
members = ["crates/core"]

[workspace.package]
edition = "2021"
rust-version = "1.87"
```

`crates/core/Cargo.toml`:

```toml
[package]
name = "lecturelive-core"
version = "0.1.0"
edition.workspace = true
rust-version.workspace = true

[lib]
name = "lecturelive_core"

[dependencies]
anyhow = "1"
hound = "3.5"

[dev-dependencies]
tempfile = "3"
```

`crates/core/src/lib.rs`:

```rust
pub mod audio;
```

`crates/core/src/audio/mod.rs`:

```rust
pub mod recorder;
```

Append to `.gitignore`:

```
target/
node_modules/
recordings/
```

- [ ] **Step 2: Write the failing tests**

`crates/core/src/audio/recorder.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn frames(n: usize) -> Vec<i16> {
        (0..n).map(|i| ((i % 100) as i16) * 100).collect()
    }

    #[test]
    fn header_is_canonical_44_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let mut r = Recorder::create(dir.path(), "session").unwrap();
        r.write(&frames(1600)).unwrap();
        let path = r.finalize().unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[36..40], b"data");
        assert_eq!(bytes.len(), 44 + 1600 * 2);
    }

    #[test]
    fn same_stem_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let a = Recorder::create(dir.path(), "session").unwrap();
        let b = Recorder::create(dir.path(), "session").unwrap();
        assert_ne!(a.path(), b.path());
    }

    #[test]
    fn checkpointed_audio_survives_a_crash() {
        let dir = tempfile::tempdir().unwrap();
        let mut r = Recorder::create(dir.path(), "session").unwrap();
        let path = r.path().to_path_buf();
        for _ in 0..25 {
            r.write(&frames(1600)).unwrap(); // 25 × 100 ms = 2.5 s, checkpoints at 1 s and 2 s
        }
        std::mem::forget(r); // simulate a crash: no finalize, no drop

        let readable = hound::WavReader::open(&path).unwrap().len();
        assert!(readable >= 32_000, "readable samples {readable}");

        let repaired = repair_header(&path).unwrap();
        assert!(repaired >= 32_000);
        assert_eq!(hound::WavReader::open(&path).unwrap().len(), repaired);
    }

    #[test]
    fn repair_drops_a_trailing_odd_byte() {
        let dir = tempfile::tempdir().unwrap();
        let mut r = Recorder::create(dir.path(), "session").unwrap();
        r.write(&frames(1600)).unwrap();
        let path = r.finalize().unwrap();
        use std::io::Write;
        std::fs::OpenOptions::new().append(true).open(&path).unwrap().write_all(&[7]).unwrap();
        assert_eq!(repair_header(&path).unwrap(), 1600);
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 44 + 3200);
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p lecturelive-core recorder`
Expected: compile errors — `Recorder`, `repair_header` not found.

- [ ] **Step 4: Implement**

Above the test module in `recorder.rs`:

```rust
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use anyhow::{ensure, Context, Result};
use hound::{SampleFormat, WavSpec, WavWriter};

pub const SAMPLE_RATE: u32 = 16_000;
const CHECKPOINT_SAMPLES: u32 = SAMPLE_RATE;
const HEADER_LEN: u64 = 44;

fn spec() -> WavSpec {
    WavSpec { channels: 1, sample_rate: SAMPLE_RATE, bits_per_sample: 16, sample_format: SampleFormat::Int }
}

pub struct Recorder {
    writer: WavWriter<BufWriter<File>>,
    sync_handle: File,
    path: PathBuf,
    since_checkpoint: u32,
}

impl Recorder {
    pub fn create(dir: &Path, stem: &str) -> Result<Self> {
        std::fs::create_dir_all(dir)?;
        let mut n = 0;
        let (file, path) = loop {
            let name = if n == 0 { format!("{stem}.wav") } else { format!("{stem}_{n}.wav") };
            let path = dir.join(name);
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(f) => break (f, path),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => n += 1,
                Err(e) => return Err(e).with_context(|| format!("create {}", path.display())),
            }
        };
        let sync_handle = file.try_clone()?;
        let writer = WavWriter::new(BufWriter::new(file), spec())?;
        Ok(Self { writer, sync_handle, path, since_checkpoint: 0 })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn write(&mut self, pcm: &[i16]) -> Result<()> {
        for &s in pcm {
            self.writer.write_sample(s)?;
        }
        self.since_checkpoint += pcm.len() as u32;
        if self.since_checkpoint >= CHECKPOINT_SAMPLES {
            self.checkpoint()?;
        }
        Ok(())
    }

    /// Flushes buffered samples, rewrites the header sizes, and syncs to disk.
    pub fn checkpoint(&mut self) -> Result<()> {
        self.writer.flush()?;
        self.sync_handle.sync_data()?;
        self.since_checkpoint = 0;
        Ok(())
    }

    pub fn finalize(self) -> Result<PathBuf> {
        let path = self.path.clone();
        self.writer.finalize()?;
        self.sync_handle.sync_all()?;
        Ok(path)
    }
}

/// Rewrites the RIFF and data sizes from the file length; returns the sample count.
pub fn repair_header(path: &Path) -> Result<u32> {
    let mut f = OpenOptions::new().read(true).write(true).open(path)?;
    let len = f.metadata()?.len();
    ensure!(len >= HEADER_LEN, "{} is shorter than a WAV header", path.display());
    let data_len = ((len - HEADER_LEN) / 2 * 2) as u32;
    f.set_len(HEADER_LEN + data_len as u64)?;
    f.seek(SeekFrom::Start(4))?;
    f.write_all(&(36 + data_len).to_le_bytes())?;
    f.seek(SeekFrom::Start(40))?;
    f.write_all(&data_len.to_le_bytes())?;
    f.sync_all()?;
    Ok(data_len / 2)
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p lecturelive-core recorder`
Expected: 4 passed.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock rust-toolchain.toml .gitignore crates/core
git commit -m "Add the Rust workspace and a checkpointed WAV recorder

The recorder flushes and syncs once per second, so a crash loses at most the
last second of audio. repair_header restores a WAV whose header was not
finalized."
```

---

### Task 2: Output routing with persisted restore state

**Files:**
- Create: `crates/core/src/fsutil.rs`, `crates/core/src/audio/routing.rs`, `crates/core/src/audio/coreaudio.rs`
- Modify: `crates/core/src/lib.rs`, `crates/core/src/audio/mod.rs`, `crates/core/Cargo.toml`

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces:
  - `fsutil::write_atomic(path: &Path, bytes: &[u8]) -> Result<()>`
  - `routing::{RouteState, AGGREGATE_UID, BLACKHOLE_UID, restore_target, load_state, enable_loopback, disable_loopback, route_status, RouteStatus}`
  - `RouteState { previous_output_uid: String, aggregate_uid: String }`
  - `restore_target(state: &RouteState, current_default_uid: &str) -> Option<String>`
  - `enable_loopback(state_path: &Path) -> Result<RouteState>`
  - `disable_loopback(state_path: &Path) -> Result<bool>` (true when the default output was restored)
  - `route_status(state_path: &Path) -> Result<RouteStatus>`, `RouteStatus { default_output_uid: String, saved: Option<RouteState>, abandoned: bool, blackhole_present: bool }`
  - `coreaudio::{default_output_device, device_uid, device_for_uid, set_default_output, create_multi_output, destroy_aggregate}`

- [ ] **Step 1: Add dependencies**

In `crates/core/Cargo.toml` `[dependencies]`:

```toml
serde = { version = "1", features = ["derive"] }
serde_json = "1"
coreaudio-sys = "0.2"
core-foundation = "0.10"
```

`lib.rs`:

```rust
pub mod audio;
pub mod fsutil;
```

`audio/mod.rs`:

```rust
pub mod coreaudio;
pub mod recorder;
pub mod routing;
```

- [ ] **Step 2: Write the failing tests**

`crates/core/src/fsutil.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_atomic_replaces_and_leaves_no_temp() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("state.json");
        write_atomic(&p, b"one").unwrap();
        write_atomic(&p, b"two").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"two");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
```

`crates/core/src/audio/routing.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> RouteState {
        RouteState { previous_output_uid: "BuiltInSpeakerDevice".into(), aggregate_uid: AGGREGATE_UID.into() }
    }

    #[test]
    fn restores_when_our_aggregate_is_still_default() {
        assert_eq!(restore_target(&state(), AGGREGATE_UID), Some("BuiltInSpeakerDevice".to_string()));
    }

    #[test]
    fn respects_a_later_user_change() {
        assert_eq!(restore_target(&state(), "AirPodsUID"), None);
    }

    #[test]
    fn state_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("route.json");
        save_state(&p, &state()).unwrap();
        assert_eq!(load_state(&p).unwrap(), Some(state()));
        std::fs::remove_file(&p).unwrap();
        assert_eq!(load_state(&p).unwrap(), None);
    }

    /// Changes the real default output; run by hand: cargo test -p lecturelive-core routing -- --ignored
    #[test]
    #[ignore]
    fn enable_then_disable_restores_the_original_output() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("route.json");
        let before = coreaudio::device_uid(coreaudio::default_output_device().unwrap()).unwrap();
        enable_loopback(&p).unwrap();
        assert_eq!(coreaudio::device_uid(coreaudio::default_output_device().unwrap()).unwrap(), AGGREGATE_UID);
        assert!(disable_loopback(&p).unwrap());
        assert_eq!(coreaudio::device_uid(coreaudio::default_output_device().unwrap()).unwrap(), before);
        assert!(coreaudio::device_for_uid(AGGREGATE_UID).unwrap().is_none());
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p lecturelive-core -- fsutil routing`
Expected: compile errors — items not found.

- [ ] **Step 4: Implement `fsutil.rs`** (above its tests)

```rust
use std::fs::File;
use std::io::Write;
use std::path::Path;

use anyhow::Result;

pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut f = File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}
```

- [ ] **Step 5: Implement `coreaudio.rs`**

```rust
//! Thin CoreAudio wrappers for default-output routing (macOS only).
use std::ffi::c_void;
use std::mem::size_of;
use std::ptr::null;

use anyhow::{bail, Result};
use core_foundation::array::CFArray;
use core_foundation::base::{CFType, TCFType};
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::{CFString, CFStringRef};
use coreaudio_sys::*;

fn addr(selector: AudioObjectPropertySelector) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    }
}

fn check(status: OSStatus, what: &str) -> Result<()> {
    if status != 0 {
        bail!("{what} failed: OSStatus {status}");
    }
    Ok(())
}

pub fn default_output_device() -> Result<AudioObjectID> {
    let a = addr(kAudioHardwarePropertyDefaultOutputDevice);
    let mut id: AudioObjectID = 0;
    let mut size = size_of::<AudioObjectID>() as u32;
    check(
        unsafe { AudioObjectGetPropertyData(kAudioObjectSystemObject, &a, 0, null(), &mut size, &mut id as *mut _ as *mut c_void) },
        "get default output",
    )?;
    Ok(id)
}

pub fn set_default_output(id: AudioObjectID) -> Result<()> {
    let a = addr(kAudioHardwarePropertyDefaultOutputDevice);
    check(
        unsafe {
            AudioObjectSetPropertyData(kAudioObjectSystemObject, &a, 0, null(), size_of::<AudioObjectID>() as u32, &id as *const _ as *const c_void)
        },
        "set default output",
    )
}

pub fn device_uid(id: AudioObjectID) -> Result<String> {
    let a = addr(kAudioDevicePropertyDeviceUID);
    let mut uid: CFStringRef = std::ptr::null();
    let mut size = size_of::<CFStringRef>() as u32;
    check(
        unsafe { AudioObjectGetPropertyData(id, &a, 0, null(), &mut size, &mut uid as *mut _ as *mut c_void) },
        "get device uid",
    )?;
    Ok(unsafe { CFString::wrap_under_create_rule(uid) }.to_string())
}

pub fn device_for_uid(uid: &str) -> Result<Option<AudioObjectID>> {
    let a = addr(kAudioHardwarePropertyTranslateUIDToDevice);
    let cf_uid = CFString::new(uid);
    let qualifier = cf_uid.as_concrete_TypeRef();
    let mut id: AudioObjectID = kAudioObjectUnknown;
    let mut size = size_of::<AudioObjectID>() as u32;
    check(
        unsafe {
            AudioObjectGetPropertyData(
                kAudioObjectSystemObject,
                &a,
                size_of::<CFStringRef>() as u32,
                &qualifier as *const _ as *const c_void,
                &mut size,
                &mut id as *mut _ as *mut c_void,
            )
        },
        "translate uid",
    )?;
    Ok((id != kAudioObjectUnknown).then_some(id))
}

/// Public stacked (multi-output) aggregate: every subdevice plays the same audio.
pub fn create_multi_output(name: &str, uid: &str, clock_uid: &str, sub_uids: &[&str]) -> Result<AudioObjectID> {
    let subs: Vec<CFDictionary<CFString, CFType>> = sub_uids
        .iter()
        .map(|u| {
            CFDictionary::from_CFType_pairs(&[
                (CFString::new("uid"), CFString::new(u).as_CFType()),
                (CFString::new("drift"), CFNumber::from(i32::from(*u != clock_uid)).as_CFType()),
            ])
        })
        .collect();
    let desc = CFDictionary::from_CFType_pairs(&[
        (CFString::new("name"), CFString::new(name).as_CFType()),
        (CFString::new("uid"), CFString::new(uid).as_CFType()),
        (CFString::new("subdevices"), CFArray::from_CFTypes(&subs).as_CFType()),
        (CFString::new("master"), CFString::new(clock_uid).as_CFType()),
        (CFString::new("stacked"), CFNumber::from(1).as_CFType()),
        (CFString::new("private"), CFNumber::from(0).as_CFType()),
    ]);
    let mut id: AudioObjectID = 0;
    check(
        unsafe { AudioHardwareCreateAggregateDevice(desc.as_concrete_TypeRef() as _, &mut id) },
        "create aggregate",
    )?;
    Ok(id)
}

pub fn destroy_aggregate(id: AudioObjectID) -> Result<()> {
    check(unsafe { AudioHardwareDestroyAggregateDevice(id) }, "destroy aggregate")
}
```

- [ ] **Step 6: Implement `routing.rs`** (above its tests)

```rust
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use super::coreaudio;
use crate::fsutil::write_atomic;

pub const AGGREGATE_UID: &str = "com.lecturelive.multioutput";
pub const BLACKHOLE_UID: &str = "BlackHole2ch_UID";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RouteState {
    pub previous_output_uid: String,
    pub aggregate_uid: String,
}

#[derive(Debug)]
pub struct RouteStatus {
    pub default_output_uid: String,
    pub saved: Option<RouteState>,
    pub abandoned: bool,
    pub blackhole_present: bool,
}

pub fn restore_target(state: &RouteState, current_default_uid: &str) -> Option<String> {
    (current_default_uid == state.aggregate_uid).then(|| state.previous_output_uid.clone())
}

pub fn load_state(path: &Path) -> Result<Option<RouteState>> {
    match std::fs::read(path) {
        Ok(b) => Ok(Some(serde_json::from_slice(&b).context("route state")?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn save_state(path: &Path, state: &RouteState) -> Result<()> {
    write_atomic(path, &serde_json::to_vec_pretty(state)?)
}

pub fn enable_loopback(state_path: &Path) -> Result<RouteState> {
    let current = coreaudio::device_uid(coreaudio::default_output_device()?)?;
    if current == AGGREGATE_UID {
        bail!("already routed; run disable first");
    }
    if coreaudio::device_for_uid(BLACKHOLE_UID)?.is_none() {
        bail!("BlackHole 2ch is not installed (brew install blackhole-2ch)");
    }
    let state = RouteState { previous_output_uid: current.clone(), aggregate_uid: AGGREGATE_UID.into() };
    save_state(state_path, &state)?; // persisted before anything changes, so a crash can be undone
    if let Some(stale) = coreaudio::device_for_uid(AGGREGATE_UID)? {
        coreaudio::destroy_aggregate(stale)?;
    }
    let agg = coreaudio::create_multi_output("LectureLive Output", AGGREGATE_UID, &current, &[&current, BLACKHOLE_UID])?;
    coreaudio::set_default_output(agg)?;
    Ok(state)
}

pub fn disable_loopback(state_path: &Path) -> Result<bool> {
    let Some(state) = load_state(state_path)? else { return Ok(false) };
    let current = coreaudio::device_uid(coreaudio::default_output_device()?)?;
    let mut restored = false;
    if let Some(prev) = restore_target(&state, &current) {
        let id = coreaudio::device_for_uid(&prev)?.with_context(|| format!("previous output {prev} is gone"))?;
        coreaudio::set_default_output(id)?;
        restored = true;
    }
    if let Some(agg) = coreaudio::device_for_uid(AGGREGATE_UID)? {
        coreaudio::destroy_aggregate(agg)?;
    }
    std::fs::remove_file(state_path)?;
    Ok(restored)
}

pub fn route_status(state_path: &Path) -> Result<RouteStatus> {
    let default_output_uid = coreaudio::device_uid(coreaudio::default_output_device()?)?;
    let saved = load_state(state_path)?;
    let abandoned = saved.as_ref().is_some_and(|s| s.aggregate_uid == default_output_uid);
    let blackhole_present = coreaudio::device_for_uid(BLACKHOLE_UID)?.is_some();
    Ok(RouteStatus { default_output_uid, saved, abandoned, blackhole_present })
}
```

- [ ] **Step 7: Run unit tests**

Run: `cargo test -p lecturelive-core -- fsutil routing`
Expected: 4 passed, 1 ignored.

- [ ] **Step 8: Run the hardware test** (BlackHole installed; the default output switches for about a second, and the test asserts it comes back)

Run: `cargo test -p lecturelive-core routing -- --ignored`
Expected: PASS. Before running it, note the default output's name (`system_profiler SPAudioDataType`, the device marked `Default Output Device: Yes`): if the test fails partway, sound still plays, because the aggregate includes the original output, and that name is what to select in System Settings → Sound → Output. If `BlackHole2ch_UID` is wrong, `route_status` reports `blackhole_present: false`; list device UIDs with Audio MIDI Setup and correct the constant.

- [ ] **Step 9: Commit**

```bash
git add crates/core
git commit -m "Add multi-output routing with a persisted restore rule

The previous default output is saved atomically before the route changes,
and it is restored only while the LectureLive aggregate is still the default,
so a later change by the user is respected. The aggregate has a fixed UID, so
aggregates the user created are never touched."
```

---

### Task 3: Input capture, 16 kHz conversion, timed recording

**Files:**
- Create: `crates/core/src/audio/convert.rs`, `crates/core/src/audio/input.rs`
- Modify: `crates/core/src/audio/mod.rs`, `crates/core/Cargo.toml`

**Interfaces:**
- Consumes: `recorder::{Recorder, SAMPLE_RATE}` (Task 1).
- Produces:
  - `convert::{downmix, Resampler16k, Framer, FRAME_SAMPLES, rms}`: `downmix(interleaved: &[f32], channels: usize) -> Vec<f32>`; `Resampler16k::new(input_rate: u32) -> Result<Self>`, `push(&mut self, mono: &[f32]) -> Result<Vec<f32>>`; `Framer::default()`, `push(&mut self, mono16k: &[f32]) -> Vec<Vec<i16>>` (each exactly `FRAME_SAMPLES` = 1600), `finish(self) -> Vec<i16>` (remainder, < 1600); `rms(pcm: &[i16]) -> f32` (0–1).
  - `input::{InputInfo, list_inputs, RecordReport, record_for}`: `list_inputs() -> Result<Vec<InputInfo>>` with `InputInfo { name: String, sample_rate: u32, channels: u16 }`; `record_for(device_name: &str, duration: Duration, dir: &Path, on_level: impl FnMut(f32)) -> Result<RecordReport>` with `RecordReport { path: PathBuf, samples: u64, dropped_callbacks: u64 }`.

- [ ] **Step 1: Add dependencies**

```toml
cpal = "0.18"
rubato = "0.16"
chrono = "0.4"
```

`audio/mod.rs` adds `pub mod convert;` and `pub mod input;`.

- [ ] **Step 2: Write the failing tests** in `convert.rs`

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downmix_averages_channels() {
        assert_eq!(downmix(&[1.0, 0.0, 0.5, 0.5], 2), vec![0.5, 0.5]);
        assert_eq!(downmix(&[0.25, 0.75], 1), vec![0.25, 0.75]);
    }

    #[test]
    fn framer_emits_exact_frames_and_keeps_remainder() {
        let mut f = Framer::default();
        let frames = f.push(&vec![0.5; 4000]);
        assert_eq!(frames.len(), 2);
        assert!(frames.iter().all(|fr| fr.len() == FRAME_SAMPLES));
        assert_eq!(f.finish().len(), 800);
    }

    #[test]
    fn framer_clamps_to_i16() {
        let mut f = Framer::default();
        let fr = f.push(&vec![2.0; FRAME_SAMPLES]);
        assert_eq!(fr[0][0], i16::MAX);
    }

    fn resampled_len(rate: u32) -> usize {
        let mut r = Resampler16k::new(rate).unwrap();
        let tone: Vec<f32> = (0..rate as usize * 2).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
        let mut out = 0;
        for chunk in tone.chunks(480) {
            out += r.push(chunk).unwrap().len();
        }
        out
    }

    #[test]
    fn resamples_common_rates_to_16k() {
        for rate in [16_000, 44_100, 48_000] {
            let n = resampled_len(rate);
            assert!((30_000..=32_000).contains(&n), "{rate} Hz -> {n} samples for 2 s");
        }
    }

    #[test]
    fn rms_of_silence_and_full_scale() {
        assert_eq!(rms(&[0; 1600]), 0.0);
        assert!((rms(&[i16::MAX; 1600]) - 1.0).abs() < 0.001);
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p lecturelive-core convert`
Expected: compile errors — items not found.

- [ ] **Step 4: Implement `convert.rs`** (above its tests; written for rubato 0.16's `FftFixedIn` — if a newer rubato is pinned, use its synchronous fixed-ratio resampler with the same `push` contract)

```rust
use anyhow::Result;
use rubato::{FftFixedIn, Resampler};

pub const FRAME_SAMPLES: usize = 1600;
const TARGET_RATE: usize = 16_000;
const CHUNK: usize = 1024;

pub fn downmix(interleaved: &[f32], channels: usize) -> Vec<f32> {
    if channels == 1 {
        return interleaved.to_vec();
    }
    interleaved.chunks(channels).map(|c| c.iter().sum::<f32>() / channels as f32).collect()
}

pub struct Resampler16k {
    inner: Option<FftFixedIn<f32>>,
    pending: Vec<f32>,
}

impl Resampler16k {
    pub fn new(input_rate: u32) -> Result<Self> {
        let inner = if input_rate as usize == TARGET_RATE {
            None
        } else {
            Some(FftFixedIn::<f32>::new(input_rate as usize, TARGET_RATE, CHUNK, 2, 1)?)
        };
        Ok(Self { inner, pending: Vec::new() })
    }

    pub fn push(&mut self, mono: &[f32]) -> Result<Vec<f32>> {
        let Some(r) = self.inner.as_mut() else { return Ok(mono.to_vec()) };
        self.pending.extend_from_slice(mono);
        let mut out = Vec::new();
        while self.pending.len() >= r.input_frames_next() {
            let n = r.input_frames_next();
            let chunk: Vec<f32> = self.pending.drain(..n).collect();
            out.extend(r.process(&[chunk], None)?.remove(0));
        }
        Ok(out)
    }
}

#[derive(Default)]
pub struct Framer {
    buf: Vec<i16>,
}

impl Framer {
    pub fn push(&mut self, mono16k: &[f32]) -> Vec<Vec<i16>> {
        self.buf.extend(mono16k.iter().map(|s| (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16));
        let mut frames = Vec::new();
        while self.buf.len() >= FRAME_SAMPLES {
            frames.push(self.buf.drain(..FRAME_SAMPLES).collect());
        }
        frames
    }

    pub fn finish(self) -> Vec<i16> {
        self.buf
    }
}

pub fn rms(pcm: &[i16]) -> f32 {
    if pcm.is_empty() {
        return 0.0;
    }
    let sum: f64 = pcm.iter().map(|&s| (s as f64 / i16::MAX as f64).powi(2)).sum();
    (sum / pcm.len() as f64).sqrt() as f32
}
```

- [ ] **Step 5: Run tests**

Run: `cargo test -p lecturelive-core convert`
Expected: 5 passed.

- [ ] **Step 6: Implement `input.rs`** (hardware-bound; exercised by the CLI in Task 6)

```rust
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, RecvTimeoutError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use super::convert::{downmix, rms, Framer, Resampler16k};
use super::recorder::Recorder;

pub struct InputInfo {
    pub name: String,
    pub sample_rate: u32,
    pub channels: u16,
}

pub struct RecordReport {
    pub path: PathBuf,
    pub samples: u64,
    pub dropped_callbacks: u64,
}

pub fn list_inputs() -> Result<Vec<InputInfo>> {
    let host = cpal::default_host();
    let mut out = Vec::new();
    for d in host.input_devices()? {
        let cfg = d.default_input_config()?;
        out.push(InputInfo { name: d.name()?, sample_rate: cfg.sample_rate().0, channels: cfg.channels() });
    }
    Ok(out)
}

/// Records `device_name` for `duration` into `dir` as 16 kHz mono PCM16.
/// The callback copies into a bounded queue; M1 replaces this with a preallocated ring.
pub fn record_for(device_name: &str, duration: Duration, dir: &Path, mut on_level: impl FnMut(f32)) -> Result<RecordReport> {
    let host = cpal::default_host();
    let device = host
        .input_devices()?
        .find(|d| d.name().map(|n| n == device_name).unwrap_or(false))
        .with_context(|| format!("input device {device_name:?} not found"))?;
    let config = device.default_input_config()?;
    let rate = config.sample_rate().0;
    let channels = config.channels() as usize;

    let (tx, rx) = sync_channel::<Vec<f32>>(64);
    let dropped = Arc::new(AtomicU64::new(0));
    let dropped_cb = dropped.clone();
    let err_fn = |e| eprintln!("[audio] stream error: {e}");
    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &config.clone().into(),
            move |data: &[f32], _| {
                if tx.try_send(data.to_vec()).is_err() {
                    dropped_cb.fetch_add(1, Ordering::Relaxed);
                }
            },
            err_fn,
            None,
        )?,
        cpal::SampleFormat::I16 => device.build_input_stream(
            &config.clone().into(),
            move |data: &[i16], _| {
                let v = data.iter().map(|&s| s as f32 / i16::MAX as f32).collect();
                if tx.try_send(v).is_err() {
                    dropped_cb.fetch_add(1, Ordering::Relaxed);
                }
            },
            err_fn,
            None,
        )?,
        other => return Err(anyhow!("unsupported sample format {other:?}")),
    };

    let stem = format!("session_{}", chrono::Local::now().format("%Y%m%d_%H%M%S"));
    let mut recorder = Recorder::create(dir, &stem)?;
    let mut resampler = Resampler16k::new(rate)?;
    let mut framer = Framer::default();
    let mut samples = 0u64;
    let mut level_acc: Vec<i16> = Vec::new();

    stream.play()?;
    let deadline = Instant::now() + duration;
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(block) => {
                for frame in framer.push(&resampler.push(&downmix(&block, channels))?) {
                    recorder.write(&frame)?;
                    samples += frame.len() as u64;
                    level_acc.extend_from_slice(&frame);
                    if level_acc.len() >= 16_000 {
                        on_level(rms(&level_acc));
                        level_acc.clear();
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    drop(stream);
    while let Ok(block) = rx.try_recv() {
        for frame in framer.push(&resampler.push(&downmix(&block, channels))?) {
            recorder.write(&frame)?;
            samples += frame.len() as u64;
        }
    }
    let tail = framer.finish();
    recorder.write(&tail)?;
    samples += tail.len() as u64;
    let path = recorder.finalize()?;
    Ok(RecordReport { path, samples, dropped_callbacks: dropped.load(Ordering::Relaxed) })
}
```

- [ ] **Step 7: Run all core tests**

Run: `cargo test -p lecturelive-core`
Expected: all non-ignored tests pass.

- [ ] **Step 8: Commit**

```bash
git add crates/core
git commit -m "Add input capture with 16 kHz conversion and timed recording

Device audio is downmixed, resampled to 16 kHz, cut into 100 ms frames and
written through the checkpointed recorder. Callbacks dropped by a full queue
are counted and reported instead of being lost silently."
```

---

### Task 4: Window capture

**Files:**
- Create: `crates/core/src/capture/mod.rs`, `crates/core/src/capture/window.rs`
- Modify: `crates/core/src/lib.rs`, `crates/core/Cargo.toml`

**Interfaces:**
- Produces: `capture::window::{WindowInfo, list_windows, capture_window, is_blank, fit_within}`; `WindowInfo { id: u32, app: String, title: String, width: u32, height: u32 }`; `list_windows() -> Result<Vec<WindowInfo>>`; `capture_window(id: u32, out: &Path) -> Result<(u32, u32)>` (saved size); `is_blank(img: &image::RgbaImage) -> bool`; `fit_within(w: u32, h: u32, max: u32) -> (u32, u32)`.

- [ ] **Step 1: Add dependencies and modules**

```toml
xcap = "0.9"
image = { version = "0.25", default-features = false, features = ["png"] }
```

`lib.rs` adds `pub mod capture;`; `capture/mod.rs` contains `pub mod window;`.

- [ ] **Step 2: Write the failing tests** in `window.rs`

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    #[test]
    fn fit_within_keeps_aspect_and_never_upscales() {
        assert_eq!(fit_within(3200, 1800, 1600), (1600, 900));
        assert_eq!(fit_within(1800, 3200, 1600), (900, 1600));
        assert_eq!(fit_within(1280, 720, 1600), (1280, 720));
    }

    #[test]
    fn blank_detection() {
        assert!(is_blank(&RgbaImage::from_pixel(64, 36, Rgba([0, 0, 0, 255]))));
        assert!(is_blank(&RgbaImage::new(0, 0)));
        let mut slide = RgbaImage::from_pixel(64, 36, Rgba([255, 255, 255, 255]));
        slide.put_pixel(10, 10, Rgba([0, 0, 0, 255]));
        assert!(!is_blank(&slide));
    }

    /// Needs Screen Recording permission: cargo test -p lecturelive-core window -- --ignored
    #[test]
    #[ignore]
    fn lists_at_least_one_window() {
        assert!(!list_windows().unwrap().is_empty());
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p lecturelive-core window`
Expected: compile errors.

- [ ] **Step 4: Implement** (xcap 0.9 methods return `XCapResult`; if the pinned version returns plain values, drop the `?`)

```rust
use std::path::Path;

use anyhow::{bail, Context, Result};
use image::{imageops::FilterType, RgbaImage};

pub const MAX_PX: u32 = 1600;

pub struct WindowInfo {
    pub id: u32,
    pub app: String,
    pub title: String,
    pub width: u32,
    pub height: u32,
}

pub fn fit_within(w: u32, h: u32, max: u32) -> (u32, u32) {
    let longest = w.max(h);
    if longest <= max {
        return (w, h);
    }
    let scale = max as f64 / longest as f64;
    ((w as f64 * scale).round() as u32, (h as f64 * scale).round() as u32)
}

/// A frame with no pixel differing from the first is treated as a failed capture.
pub fn is_blank(img: &RgbaImage) -> bool {
    let mut px = img.pixels();
    let Some(first) = px.next() else { return true };
    px.all(|p| p == first)
}

pub fn list_windows() -> Result<Vec<WindowInfo>> {
    let mut out = Vec::new();
    for w in xcap::Window::all().context("enumerate windows (Screen Recording permission?)")? {
        out.push(WindowInfo {
            id: w.id()?,
            app: w.app_name()?,
            title: w.title()?,
            width: w.width()?,
            height: w.height()?,
        });
    }
    Ok(out)
}

pub fn capture_window(id: u32, out: &Path) -> Result<(u32, u32)> {
    let window = xcap::Window::all()?
        .into_iter()
        .find(|w| w.id().map(|i| i == id).unwrap_or(false))
        .with_context(|| format!("window {id} not found"))?;
    let img = window.capture_image().context("capture window")?;
    if is_blank(&img) {
        bail!("capture of window {id} is blank (permission missing or window hidden)");
    }
    let (w, h) = fit_within(img.width(), img.height(), MAX_PX);
    let img = if (w, h) == (img.width(), img.height()) { img } else { image::imageops::resize(&img, w, h, FilterType::Lanczos3) };
    let tmp = out.with_extension("png.tmp");
    img.save_with_format(&tmp, image::ImageFormat::Png)?;
    std::fs::rename(&tmp, out)?;
    Ok((w, h))
}
```

- [ ] **Step 5: Run tests**

Run: `cargo test -p lecturelive-core window`
Expected: 2 passed, 1 ignored.

- [ ] **Step 6: Commit**

```bash
git add crates/core
git commit -m "Add window enumeration and capture with blank-frame rejection

Captures are scaled to at most 1600 px and written through a temporary file.
A uniform frame is an error, so a missing permission or a hidden window can
never produce a black slide."
```

---

### Task 5: STT websocket probe

**Files:**
- Create: `crates/core/src/stt/mod.rs`, `crates/core/src/stt/probe.rs`, `crates/core/tests/stt_probe.rs`
- Modify: `crates/core/src/lib.rs`, `crates/core/Cargo.toml`

**Interfaces:**
- Produces: `stt::probe::{ProbeOptions, ProbeSummary, probe}`; `ProbeOptions { url: String, api_key: String, pcm: Vec<i16>, pace: Duration, finalize_after_frames: Option<usize>, finalize_message: String, log_path: PathBuf }`; `async fn probe(opts: ProbeOptions) -> Result<ProbeSummary>`; `ProbeSummary { frames_sent: usize, messages: usize, saw_created: bool, saw_done: bool }`. Log format: one JSON object per line `{"t_ms": u64, "dir": "in"|"out", "msg": <server JSON or client text>}`.

- [ ] **Step 1: Add dependencies**

```toml
tokio = { version = "1", features = ["rt-multi-thread", "macros", "time", "net", "sync", "fs", "io-util"] }
tokio-tungstenite = { version = "0.27", features = ["rustls-tls-webpki-roots"] }
futures-util = "0.3"
```

`lib.rs` adds `pub mod stt;`; `stt/mod.rs` contains `pub mod probe;`.

- [ ] **Step 2: Write the failing integration test** `crates/core/tests/stt_probe.rs`

```rust
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use lecturelive_core::stt::probe::{probe, ProbeOptions};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use tokio_tungstenite::tungstenite::Message;

#[tokio::test]
async fn probe_sends_paced_pcm_frames_and_logs_every_message() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut auth = None;
        let mut ws = tokio_tungstenite::accept_hdr_async(tcp, |req: &Request, resp: Response| {
            auth = req.headers().get("authorization").map(|v| v.to_str().unwrap().to_string());
            Ok(resp)
        })
        .await
        .unwrap();
        ws.send(Message::Text(r#"{"type":"transcript.created"}"#.into())).await.unwrap();
        let (mut sizes, mut texts) = (Vec::new(), Vec::new());
        while let Some(Ok(msg)) = ws.next().await {
            match msg {
                Message::Binary(b) => {
                    sizes.push(b.len());
                    if sizes.len() == 2 {
                        ws.send(Message::Text(r#"{"type":"transcript.partial","transcript":"hi","is_final":false,"speech_final":false,"words":[]}"#.into())).await.unwrap();
                    }
                }
                Message::Text(t) => {
                    let t = t.to_string();
                    texts.push(t.clone());
                    if t == "audio.done" {
                        ws.send(Message::Text(r#"{"type":"transcript.done","transcript":"hi","words":[]}"#.into())).await.unwrap();
                        ws.close(None).await.ok();
                        break;
                    }
                }
                _ => {}
            }
        }
        (auth, sizes, texts)
    });

    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("probe.jsonl");
    let summary = probe(ProbeOptions {
        url: format!("ws://{addr}/v1/stt"),
        api_key: "test-key".into(),
        pcm: vec![0i16; 1600 * 3 + 100],
        pace: Duration::ZERO,
        finalize_after_frames: Some(2),
        finalize_message: r#"{"type":"finalize"}"#.into(),
        log_path: log.clone(),
    })
    .await
    .unwrap();

    let (auth, sizes, texts) = server.await.unwrap();
    assert_eq!(auth.as_deref(), Some("Bearer test-key"));
    assert_eq!(sizes, vec![3200, 3200, 3200, 200]); // little-endian PCM16, last frame partial
    assert_eq!(texts, vec![r#"{"type":"finalize"}"#.to_string(), "audio.done".to_string()]);
    assert!(summary.saw_created && summary.saw_done);
    assert_eq!(summary.frames_sent, 4);
    let lines = std::fs::read_to_string(&log).unwrap();
    assert!(lines.lines().count() >= 5, "log:\n{lines}");
    assert!(lines.contains("transcript.partial"));
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p lecturelive-core --test stt_probe`
Expected: compile error — `stt::probe` not found.

- [ ] **Step 4: Implement `probe.rs`**

```rust
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

pub struct ProbeOptions {
    pub url: String,
    pub api_key: String,
    pub pcm: Vec<i16>,
    pub pace: Duration,
    pub finalize_after_frames: Option<usize>,
    pub finalize_message: String,
    pub log_path: PathBuf,
}

#[derive(Debug)]
pub struct ProbeSummary {
    pub frames_sent: usize,
    pub messages: usize,
    pub saw_created: bool,
    pub saw_done: bool,
}

fn frame_bytes(frame: &[i16]) -> Vec<u8> {
    frame.iter().flat_map(|s| s.to_le_bytes()).collect()
}

pub async fn probe(opts: ProbeOptions) -> Result<ProbeSummary> {
    let mut req = opts.url.as_str().into_client_request()?;
    req.headers_mut().insert("Authorization", format!("Bearer {}", opts.api_key).parse()?);
    let (ws, _) = tokio_tungstenite::connect_async(req).await.context("connect")?;
    let (mut tx, mut rx) = ws.split();
    let mut log = tokio::fs::File::create(&opts.log_path).await?;
    let started = Instant::now();
    let mut summary = ProbeSummary { frames_sent: 0, messages: 0, saw_created: false, saw_done: false };

    async fn write_line(log: &mut tokio::fs::File, started: Instant, dir: &str, msg: Value) -> Result<()> {
        let line = json!({ "t_ms": started.elapsed().as_millis() as u64, "dir": dir, "msg": msg });
        log.write_all(format!("{line}\n").as_bytes()).await?;
        Ok(())
    }

    // Wait for transcript.created before sending audio.
    loop {
        let Some(msg) = rx.next().await else { bail!("closed before transcript.created") };
        if let Message::Text(t) = msg? {
            let v: Value = serde_json::from_str(&t).unwrap_or(Value::String(t.to_string()));
            summary.messages += 1;
            let created = v["type"] == "transcript.created";
            write_line(&mut log, started, "in", v).await?;
            if created {
                summary.saw_created = true;
                break;
            }
        }
    }

    let reader = tokio::spawn(async move {
        let mut lines = Vec::new();
        while let Some(Ok(msg)) = rx.next().await {
            if let Message::Text(t) = msg {
                lines.push((Instant::now(), t.to_string()));
            }
        }
        lines
    });

    for (i, frame) in opts.pcm.chunks(1600).enumerate() {
        tx.send(Message::Binary(frame_bytes(frame).into())).await?;
        summary.frames_sent += 1;
        write_line(&mut log, started, "out", json!({ "binary_bytes": frame.len() * 2 })).await?;
        if opts.finalize_after_frames == Some(i + 1) {
            tx.send(Message::Text(opts.finalize_message.clone().into())).await?;
            write_line(&mut log, started, "out", Value::String(opts.finalize_message.clone())).await?;
        }
        if !opts.pace.is_zero() {
            tokio::time::sleep(opts.pace).await;
        }
    }
    tx.send(Message::Text("audio.done".into())).await?;
    write_line(&mut log, started, "out", Value::String("audio.done".into())).await?;

    let received = tokio::time::timeout(Duration::from_secs(30), reader).await.context("waiting for transcript.done")??;
    for (at, t) in received {
        let v: Value = serde_json::from_str(&t).unwrap_or(Value::String(t));
        summary.messages += 1;
        summary.saw_done |= v["type"] == "transcript.done";
        let line = json!({ "t_ms": at.duration_since(started).as_millis() as u64, "dir": "in", "msg": v });
        log.write_all(format!("{line}\n").as_bytes()).await?;
    }
    log.flush().await?;
    Ok(summary)
}
```

- [ ] **Step 5: Run test**

Run: `cargo test -p lecturelive-core --test stt_probe`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/core
git commit -m "Add a websocket probe that records the STT protocol

The probe sends 100 ms little-endian PCM16 frames after transcript.created,
can send a finalize message mid-stream, and logs every message in both
directions with a timestamp. The resulting logs become the fixtures for the
transcript state machine."
```

---

### Task 6: CLI canary commands

**Files:**
- Create: `crates/cli/Cargo.toml`, `crates/cli/src/main.rs`
- Modify: `Cargo.toml` (members)

**Interfaces:**
- Consumes: every core function from Tasks 1–5.
- Produces: binary `lecturelive` with `canary {route status|on|off, inputs, record, repair, windows, capture, stt-probe}`. Route state file: `~/Library/Application Support/LectureLive/route.json`.

- [ ] **Step 1: Create the crate**

Workspace `members = ["crates/core", "crates/cli"]`.

`crates/cli/Cargo.toml`:

```toml
[package]
name = "lecturelive-cli"
version = "0.1.0"
edition.workspace = true
rust-version.workspace = true

[[bin]]
name = "lecturelive"
path = "src/main.rs"

[dependencies]
lecturelive-core = { path = "../core" }
anyhow = "1"
clap = { version = "4", features = ["derive"] }
dotenvy = "0.15"
dirs = "6"
hound = "3.5"
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
```

- [ ] **Step 2: Implement `main.rs`**

```rust
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use lecturelive_core::audio::{input, recorder, routing};
use lecturelive_core::capture::window;
use lecturelive_core::stt::probe;

#[derive(Parser)]
#[command(name = "lecturelive")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    #[command(subcommand)]
    Canary(Canary),
}

#[derive(Subcommand)]
enum Canary {
    /// Loopback routing: status | on | off
    Route { action: String },
    /// List input devices
    Inputs,
    /// Record an input device to a 16 kHz WAV
    Record {
        #[arg(long)]
        device: String,
        #[arg(long, default_value_t = 30)]
        secs: u64,
        #[arg(long, default_value = "recordings")]
        dir: PathBuf,
    },
    /// Repair the header of an interrupted WAV
    Repair { wav: PathBuf },
    /// List windows
    Windows,
    /// Capture a window to PNG
    Capture {
        #[arg(long)]
        id: u32,
        #[arg(long)]
        out: PathBuf,
    },
    /// Stream a 16 kHz mono WAV to the STT websocket and log the protocol
    SttProbe {
        wav: PathBuf,
        #[arg(long)]
        finalize_after_secs: Option<f32>,
        #[arg(long, default_value = r#"{"type":"finalize"}"#)]
        finalize_message: String,
        #[arg(long)]
        log: PathBuf,
    },
}

fn route_state_path() -> Result<PathBuf> {
    let dir = dirs::data_dir().context("no Application Support dir")?.join("LectureLive");
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join("route.json"))
}

#[tokio::main]
async fn main() -> Result<()> {
    let Cmd::Canary(c) = Cli::parse().cmd;
    match c {
        Canary::Route { action } => {
            let p = route_state_path()?;
            match action.as_str() {
                "on" => println!("routed; saved previous output {:?}", routing::enable_loopback(&p)?.previous_output_uid),
                "off" => println!("restored default output: {}", routing::disable_loopback(&p)?),
                _ => println!("{:#?}", routing::route_status(&p)?),
            }
        }
        Canary::Inputs => {
            for i in input::list_inputs()? {
                println!("{:<40} {} Hz  {} ch", i.name, i.sample_rate, i.channels);
            }
        }
        Canary::Record { device, secs, dir } => {
            let r = input::record_for(&device, Duration::from_secs(secs), &dir, |level| {
                println!("level {:>5.1} dBFS", 20.0 * level.max(1e-6).log10())
            })?;
            println!("{} — {:.1} s, dropped callbacks: {}", r.path.display(), r.samples as f64 / 16_000.0, r.dropped_callbacks);
        }
        Canary::Repair { wav } => println!("{} samples", recorder::repair_header(&wav)?),
        Canary::Windows => {
            for w in window::list_windows()? {
                println!("{:>8}  {:<20} {:<50} {}x{}", w.id, w.app, w.title, w.width, w.height);
            }
        }
        Canary::Capture { id, out } => {
            let (w, h) = window::capture_window(id, &out)?;
            println!("{} {w}x{h}", out.display());
        }
        Canary::SttProbe { wav, finalize_after_secs, finalize_message, log } => {
            dotenvy::dotenv().ok();
            let api_key = std::env::var("GROK_API_KEY").context("GROK_API_KEY not set")?;
            let mut reader = hound::WavReader::open(&wav)?;
            let spec = reader.spec();
            anyhow::ensure!(spec.sample_rate == 16_000 && spec.channels == 1 && spec.bits_per_sample == 16, "need 16 kHz mono PCM16");
            let pcm: Vec<i16> = reader.samples::<i16>().collect::<Result<_, _>>()?;
            let s = probe::probe(probe::ProbeOptions {
                url: "wss://api.x.ai/v1/stt?model=grok-voice-transcribe-2.0&encoding=pcm&sample_rate=16000&interim_results=true&language=en".into(),
                api_key,
                pcm,
                pace: Duration::from_millis(100),
                finalize_after_frames: finalize_after_secs.map(|s| (s * 10.0) as usize),
                finalize_message,
                log_path: log,
            })
            .await?;
            println!("{s:#?}");
        }
    }
    Ok(())
}
```

- [ ] **Step 3: Check it builds and the offline commands run**

Run: `cargo run -p lecturelive-cli -- canary inputs` then `cargo run -p lecturelive-cli -- canary route status`
Expected: input list including `BlackHole 2ch`; route status with `blackhole_present: true`, `saved: None`.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml Cargo.lock crates/cli
git commit -m "Add canary CLI commands for each native capability

Routing, recording, WAV repair, window capture and the STT probe can each be
exercised from the terminal before the packaged app exists."
```

---

### Task 7: Packaged Tauri canary app

**Files:**
- Create: `apps/desktop/` (from the Tauri svelte-ts template), `apps/desktop/src-tauri/Info.plist`, `apps/desktop/src-tauri/Entitlements.plist`
- Modify: `apps/desktop/src-tauri/Cargo.toml`, `apps/desktop/src-tauri/src/lib.rs`, `apps/desktop/src-tauri/tauri.conf.json`, `apps/desktop/src/routes/+page.svelte` (or `src/App.svelte`, whichever the template produces), `Cargo.toml` (members)

**Interfaces:**
- Consumes: `routing::{enable_loopback, disable_loopback, route_status}`, `input::{list_inputs, record_for}`, `window::{list_windows, capture_window}`.
- Produces: Tauri commands `route(action: String) -> Result<String, String>`, `inputs() -> Result<Vec<String>, String>`, `record(device: String, secs: u64) -> Result<String, String>`, `windows() -> Result<Vec<(u32, String)>, String>`, `capture(id: u32) -> Result<String, String>`. The same checks run without the UI when the app is launched with `--check <route on|off|status | record DEVICE SECS | windows | capture ID>`: the result is appended to `checks.log` and the app quits, so Task 8 can drive the packaged app from a terminal. Outputs go to `~/Library/Application Support/LectureLive/canary/`.

- [ ] **Step 1: Scaffold**

```bash
cd apps && npm create tauri-app@latest desktop -- --template svelte-ts --manager npm --yes && cd desktop && npm install
```

Add `"apps/desktop/src-tauri"` to workspace members. In `apps/desktop/src-tauri/Cargo.toml` add:

```toml
lecturelive-core = { path = "../../../crates/core" }
dirs = "6"
```

- [ ] **Step 2: Permissions**

`apps/desktop/src-tauri/Info.plist`:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>NSMicrophoneUsageDescription</key>
  <string>LectureLive records lecture audio from the input you choose.</string>
</dict>
</plist>
```

`apps/desktop/src-tauri/Entitlements.plist`:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>com.apple.security.device.audio-input</key>
  <true/>
</dict>
</plist>
```

In `tauri.conf.json` set `productName` to `LectureLive Canary`, `identifier` to `com.lecturelive.canary`, and under `bundle`:

```json
"macOS": {
  "minimumSystemVersion": "13.0",
  "entitlements": "Entitlements.plist",
  "signingIdentity": "-",
  "hardenedRuntime": true
}
```

Ad-hoc signing (`"-"`) means macOS may ask for permissions again after each rebuild; that is expected for the canary.

- [ ] **Step 3: Commands** — replace the template's `lib.rs` body:

```rust
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use lecturelive_core::audio::{input, routing};
use lecturelive_core::capture::window;

fn app_dir(sub: &str) -> Result<PathBuf, String> {
    let d = dirs::data_dir().ok_or("no Application Support dir")?.join("LectureLive").join(sub);
    std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
    Ok(d)
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

fn route_sync(action: &str) -> Result<String, String> {
    let p = app_dir("")?.join("route.json");
    match action {
        "on" => routing::enable_loopback(&p).map(|s| format!("routed; previous output {}", s.previous_output_uid)).map_err(err),
        "off" => routing::disable_loopback(&p).map(|r| format!("restored: {r}")).map_err(err),
        _ => routing::route_status(&p).map(|s| format!("{s:#?}")).map_err(err),
    }
}

fn record_sync(device: &str, secs: u64) -> Result<String, String> {
    let mut loudest = 0f32;
    let r = input::record_for(device, Duration::from_secs(secs), &app_dir("canary")?, |l| loudest = loudest.max(l)).map_err(err)?;
    Ok(format!(
        "{} — {:.1} s, dropped {}, loudest second {:.1} dBFS",
        r.path.display(),
        r.samples as f64 / 16_000.0,
        r.dropped_callbacks,
        20.0 * loudest.max(1e-6).log10()
    ))
}

fn windows_sync() -> Result<Vec<(u32, String)>, String> {
    window::list_windows().map(|v| v.into_iter().map(|w| (w.id, format!("{} — {}", w.app, w.title))).collect()).map_err(err)
}

fn capture_sync(id: u32) -> Result<String, String> {
    let out = app_dir("canary")?.join(format!("window_{id}.png"));
    window::capture_window(id, &out).map(|(w, h)| format!("{} {w}x{h}", out.display())).map_err(err)
}

#[tauri::command]
async fn route(action: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || route_sync(&action)).await.map_err(err)?
}

#[tauri::command]
async fn inputs() -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(|| input::list_inputs().map(|v| v.into_iter().map(|i| i.name).collect()).map_err(err))
        .await
        .map_err(err)?
}

#[tauri::command]
async fn record(device: String, secs: u64) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || record_sync(&device, secs)).await.map_err(err)?
}

#[tauri::command]
async fn windows() -> Result<Vec<(u32, String)>, String> {
    tauri::async_runtime::spawn_blocking(windows_sync).await.map_err(err)?
}

#[tauri::command]
async fn capture(id: u32) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || capture_sync(id)).await.map_err(err)?
}

/// One check without the UI: `--check route on|off|status`, `--check record DEVICE SECS`,
/// `--check windows`, `--check capture ID`.
fn run_check(args: &[String]) -> Result<String, String> {
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["route", action] => route_sync(action),
        ["record", device, secs] => record_sync(device, secs.parse().map_err(err)?),
        ["windows"] => windows_sync().map(|v| v.iter().map(|(id, l)| format!("{id} {l}")).collect::<Vec<_>>().join("\n")),
        ["capture", id] => capture_sync(id.parse().map_err(err)?),
        other => Err(format!("unknown check {other:?}")),
    }
}

fn log_check(args: &[String], result: &Result<String, String>) -> std::io::Result<()> {
    let path = app_dir("canary").map_err(std::io::Error::other)?.join("checks.log");
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    match result {
        Ok(s) => writeln!(f, "{} ok\n{s}\n", args.join(" ")),
        Err(e) => writeln!(f, "{} FAILED: {e}\n", args.join(" ")),
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let check: Vec<String> = std::env::args().skip_while(|a| a.as_str() != "--check").skip(1).collect();
    tauri::Builder::default()
        .setup(move |app| {
            if !check.is_empty() {
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    let _ = log_check(&check, &run_check(&check));
                    handle.exit(0);
                });
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![route, inputs, record, windows, capture])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
```

Remove any template plugins/commands no longer referenced.

Launch checks through `open` (`open -W -n "<path>/LectureLive Canary.app" --args --check …`), never by running `Contents/MacOS/` from a shell: macOS attributes a process started from a terminal to that terminal for Microphone and Screen Recording, so the check would use the terminal's permissions — the very thing this app exists to rule out. `-W` returns when the check has finished and the app has quit; `-n` starts a fresh instance each time.

- [ ] **Step 4: Page** — replace the template's main Svelte page with:

```svelte
<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";

  let log = $state<string[]>([]);
  let devices = $state<string[]>([]);
  let device = $state("BlackHole 2ch");
  let secs = $state(60);
  let wins = $state<[number, string][]>([]);
  let busy = $state(false);

  async function run(label: string, f: () => Promise<unknown>) {
    busy = true;
    try {
      const r = await f();
      log = [`${label}: ${typeof r === "string" ? r : JSON.stringify(r)}`, ...log];
    } catch (e) {
      log = [`${label} FAILED: ${e}`, ...log];
    } finally {
      busy = false;
    }
  }
</script>

<main>
  <h1>LectureLive canary</h1>
  <section>
    <button disabled={busy} onclick={() => run("route status", () => invoke("route", { action: "status" }))}>Route status</button>
    <button disabled={busy} onclick={() => run("route on", () => invoke("route", { action: "on" }))}>Route on</button>
    <button disabled={busy} onclick={() => run("route off", () => invoke("route", { action: "off" }))}>Route off</button>
  </section>
  <section>
    <button disabled={busy} onclick={() => run("inputs", async () => (devices = await invoke<string[]>("inputs")))}>List inputs</button>
    <select bind:value={device}>
      {#each devices as d}<option>{d}</option>{/each}
    </select>
    <input type="number" bind:value={secs} min="5" />
    <button disabled={busy} onclick={() => run("record", () => invoke("record", { device, secs }))}>Record</button>
  </section>
  <section>
    <button disabled={busy} onclick={() => run("windows", async () => (wins = await invoke<[number, string][]>("windows")))}>List windows</button>
    {#each wins as [id, label]}
      <div><button disabled={busy} onclick={() => run(`capture ${id}`, () => invoke("capture", { id }))}>Capture</button> {label}</div>
    {/each}
  </section>
  <pre>{log.join("\n")}</pre>
</main>
```

- [ ] **Step 5: Build the packaged app**

Run: `cd apps/desktop && npm run tauri build -- --bundles app`
Expected: `target/release/bundle/macos/LectureLive Canary.app` at the workspace root. Verify entitlements: `codesign -d --entitlements - "target/release/bundle/macos/LectureLive Canary.app"` lists `com.apple.security.device.audio-input`. Then `open -W -n "$PWD/target/release/bundle/macos/LectureLive Canary.app" --args --check route status` appends a route status to `~/Library/Application Support/LectureLive/canary/checks.log`.

This is the build Task 8 grants permissions to. An ad-hoc-signed rebuild changes the app's code signature and macOS may ask for both permissions again, so any change after the permission sitting means another sitting.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock apps/desktop
git commit -m "Add a packaged canary app that exercises each native capability

The app asks for microphone and Screen Recording permission as a signed
bundle with the audio-input entitlement, so the permissions M0 checks are the
app's own, not inherited from Terminal. Launched with --check, it runs one
check without the UI and logs the result, so the acceptance run can drive the
packaged app from a terminal."
```

---

### Task 8: Acceptance run and findings

**Files:**
- Create: `crates/core/tests/fixtures/stt/finalize_json.jsonl`, `crates/core/tests/fixtures/stt/finalize_text.jsonl`, `crates/core/tests/fixtures/stt/speech.wav`
- Modify: `docs/milestones.md` (M0 gate checkboxes, Findings, Status)

Nothing here waits for a live lecture. The session runs every check it can alone (Steps 1, 2, 4 and 5); a person gives the two permission grants and one Zoom click in a single sitting of about three minutes, with no meeting (Step 3); the one check that needs a Zoom meeting is left in `docs/milestones.md` for the first Zoom lecture after M0. macOS grants Microphone and Screen Recording only through a person's click — no program can write that permission — so the sitting is the part that cannot be automated, and it comes after the last build (Task 7 Step 5).

The packaged app is driven with `--check` (Task 7). Define these in every shell that runs checks, from the repository root:

```bash
APP="$PWD/target/release/bundle/macos/LectureLive Canary.app"
LOG="$HOME/Library/Application Support/LectureLive/canary/checks.log"
check() { open -W -n "$APP" --args --check "$@"; tail -n 25 "$LOG"; }
```

- [ ] **Step 1: STT fixtures from synthesised speech** (no lecture audio in the repository)

```bash
mkdir -p crates/core/tests/fixtures/stt
say -o /tmp/ll_speech.aiff "Transfer learning reuses pretrained weights. The loss is cross entropy over the vocabulary. Gradient descent updates the weights after every batch."
afconvert /tmp/ll_speech.aiff -f WAVE -d LEI16@16000 -c 1 crates/core/tests/fixtures/stt/speech.wav
cargo run -p lecturelive-cli -- canary stt-probe crates/core/tests/fixtures/stt/speech.wav --finalize-after-secs 3 --finalize-message '{"type":"finalize"}' --log crates/core/tests/fixtures/stt/finalize_json.jsonl
cargo run -p lecturelive-cli -- canary stt-probe crates/core/tests/fixtures/stt/speech.wav --finalize-after-secs 3 --finalize-message finalize --log crates/core/tests/fixtures/stt/finalize_text.jsonl
```

Record in Findings: which finalize spelling produced a `speech_final: true` shortly after it (or an `error`); whether word `start` times are relative to connection start; whether `transcript.done` repeats the whole text.

- [ ] **Step 2: Routing from the terminal** (no permission involved)

```bash
cargo run -p lecturelive-cli -- canary route status   # blackhole_present: true; note default_output_uid, the output that must come back
cargo run -p lecturelive-cli -- canary route on
cargo run -p lecturelive-cli -- canary route status   # abandoned: true, the state a crash while routed leaves behind
cargo run -p lecturelive-cli -- canary route off      # restored default output: true
cargo run -p lecturelive-cli -- canary route status   # default_output_uid as noted, saved: None
```

While routed, the aggregate plays through the original output as well as BlackHole, so sound is never lost; if a restore fails, name the noted device for System Settings → Sound → Output.

- [ ] **Step 3: Permission sitting** (a person at the Mac, about three minutes, no Zoom meeting)

Ask for the sitting once, listing all three actions up front, and run each command when the person says they are ready.

1. `check record "BlackHole 2ch" 5` → macOS asks to let LectureLive Canary use the microphone → the person clicks Allow.
2. `check windows` → macOS asks for Screen Recording → the person turns on LectureLive Canary in System Settings → Privacy & Security → Screen & System Audio Recording. Run `check windows` again: the log lists windows with their titles.
3. Zoom's own audio, without a meeting: the person opens Zoom → Settings → Audio and reads out the Speaker setting ("Same as System" or a named device). Run `check route on`; tell the person to click Test Speaker and at once run `check record "BlackHole 2ch" 15`; then `check route off`. The person says whether they heard the ringtone; a loudest second above −60 dBFS in the log (digital silence reads −120) means Zoom's output reached BlackHole.

Record the Zoom speaker setting in Findings. With "Same as System", the signal came through the app's route (spec §4.3). With a named device, it came through that device, and §4.3's route did not carry Zoom at all.

- [ ] **Step 4: Loopback and crash through the packaged app** (no person needed; the synthesised speech plays aloud while routed)

```bash
check route status                                      # note default_output_uid
check route on
(for i in $(seq 12); do afplay crates/core/tests/fixtures/stt/speech.wav; done) &   # ~2 min of speech, ends by itself; run in the background
check record "BlackHole 2ch" 20                         # ~20 s, dropped 0, loudest second above −60 dBFS
open -n "$APP" --args --check record "BlackHole 2ch" 120
```

About 30 s into that recording run `pkill -9 -f "LectureLive Canary"`, noting how long it recorded. Then:

```bash
check route status                                      # abandoned: true
check route off                                         # restored: true
check route status                                      # default_output_uid as noted
WAV=$(ls -t "$HOME/Library/Application Support/LectureLive/canary"/session_*.wav | head -1)
cargo run -p lecturelive-cli -- canary repair "$WAV"
afinfo "$WAV"                                           # duration ≥ seconds recorded before the kill − 1
afplay -t 2 "$WAV"                                      # exits 0
```

- [ ] **Step 5: Window capture through the packaged app** (no person needed)

```bash
open -a TextEdit README.md
open -a zoom.us
check windows                                           # note the ids of the TextEdit README.md window and a zoom.us window
check capture <TextEdit id>
check capture <zoom.us id>
```

Look at each PNG: the TextEdit image shows README.md's text, the Zoom image shows Zoom's window, and neither side exceeds 1600 px. If Zoom opens no window, record that.

- [ ] **Step 6: Record versions**

`cargo tree -p lecturelive-core --depth 1` → copy the resolved versions of `cpal`, `rubato`, `hound`, `xcap`, `tokio-tungstenite`, `coreaudio-sys` and `core-foundation`, and `tauri` from the app crate's `cargo tree --depth 1`, into Findings together with `rustc --version`, `sw_vers -productVersion` and `xcodebuild -version`.

- [ ] **Step 7: Update `docs/milestones.md`**

Tick each gate line that held; for any that failed, write what was observed and which spec §14.1 fallback it points to. Set M0 status to `done` only if every line under **Gate** holds; the line under **At the first Zoom lecture after M0** stays open until that lecture.

Findings also compare spec §4.3 with README.md's "Zoom lectures on headphones" setup, which is how Zoom is recorded in class today. There, Zoom's speaker is pinned by name to a Multi-Output device the user made, with BlackHole as its primary (clock) device because headphones disconnect and take the clock with them, and the system output stays on the headphones so other sounds are not transcribed. §4.3 instead makes an app-owned aggregate the system default, with the physical output as the clock. Record what Step 3 showed and which arrangement the evidence favours; the choice is made when M1's plan is written.

- [ ] **Step 8: Commit**

```bash
git add crates/core/tests/fixtures docs/milestones.md
git commit -m "Record M0 findings and STT protocol fixtures

The fixtures use synthesised speech only. Findings cover the accepted
finalize message, timestamp origin, routing and permission behaviour, crash
recovery, and the crate versions that build together."
```
