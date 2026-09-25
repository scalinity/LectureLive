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
