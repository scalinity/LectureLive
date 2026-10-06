use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Local, NaiveDate};
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
    /// The live transcript's open intervals (spec §5.2, §8): each recording whose live transcript is
    /// not finished, from the sample its current connection streams from (0 until one does). It is
    /// committed only as far as the segment log shows; a crash turns the rest into a gap. After a
    /// rate change the next recording opens while the last is still being flushed, hence a list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub open_utterances: Vec<OpenUtterance>,
    /// The day the lecture's files were created; it does not change across midnight (spec §8).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lecture_date: Option<NaiveDate>,
    /// What the notes document holds (spec §6.2, §8).
    #[serde(default, skip_serializing_if = "NotesState::is_empty")]
    pub notes: NotesState,
    /// Registered slides, in registration order (spec §7.3, §8).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub slides: Vec<SlideEntry>,
    /// The stretches the person paused the lecture (spec §9.6): nothing was recorded in them, so the hole between
    /// the recordings either side is theirs, and the audit counts it as explained.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pauses: Vec<PauseSpan>,
}

impl Default for Sidecar {
    fn default() -> Self {
        Self { version: SIDECAR_VERSION, recordings: Vec::new(), gaps: Vec::new(), open_utterances: Vec::new(), lecture_date: None, notes: NotesState::default(), slides: Vec::new(), pauses: Vec::new() }
    }
}

/// A pause, from when it began to when it ended; `to` is None while it lasts, or when a crash ended the session in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PauseSpan {
    pub from: DateTime<Local>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<DateTime<Local>>,
}

/// The notes document's committed state: its revision and fingerprint, and the cursors of what it holds.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotesState {
    /// Advances with every commit, polish and accepted external edit.
    pub revision: u64,
    /// Length and SHA-256 (hex) of the notes file at this revision.
    pub len: u64,
    pub sha256: String,
    /// Segment-log positions below this are in the notes.
    pub segment_cursor: u64,
    /// Slides with an index up to this are in the notes.
    pub slide_index: u32,
}

impl NotesState {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlideEntry {
    pub index: u32,
    /// Relative to the lecture folder (absolute when the slides folder is elsewhere).
    pub file: String,
    /// When it was first on screen; for an imported image, its file time.
    pub shown_at: DateTime<Local>,
    /// Captured by the change detector rather than by the person (spec §3.5, §7.2).
    #[serde(default, skip_serializing_if = "is_false")]
    pub auto: bool,
    /// Kept after 10 s of change without settling: it may show a transition (spec §7.2).
    #[serde(default, skip_serializing_if = "is_false")]
    pub uncertain: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenUtterance {
    pub recording_id: Uuid,
    pub from_sample: u64,
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

/// An interval of a recording without durable audio (audio kinds) or without a committed
/// transcript (`stt_*` kinds). `end_sample: None` runs from `start_sample` to the next recording's
/// anchor, or to the end of the session.
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
    /// No live STT connection carried this audio: connecting, disconnected, or it did not finish in time.
    SttOffline,
    /// Frames were dropped on the way to the STT writer.
    SttOverflow,
    /// STT was refused (4xx) and stopped for the session.
    SttRefused,
    /// The session stopped (a crash, or the STT worker ended) before this audio's transcript was committed.
    SttInterrupted,
}

impl GapKind {
    /// Audio exists for the interval but its transcript does not: recovery can fill it (spec §5.4).
    pub fn is_transcript(self) -> bool {
        matches!(self, Self::SttOffline | Self::SttOverflow | Self::SttRefused | Self::SttInterrupted)
    }
}

impl Gap {
    /// A gap is resolved when nothing more can be done for it: a transcript gap once recovery has
    /// committed its interval, an audio gap at once, since it has no audio to recover from.
    pub fn new(recording_id: Uuid, start_sample: u64, end_sample: Option<u64>, kind: GapKind) -> Self {
        Self { recording_id, start_sample, end_sample, kind, resolved: !kind.is_transcript() }
    }
}

/// Wall time of a recording's sample: its anchor plus sample / 16 kHz (spec §3.3).
pub fn wall_time_at(anchor: DateTime<Local>, sample: u64) -> DateTime<Local> {
    anchor + chrono::Duration::microseconds((sample * 1_000_000 / SAMPLE_RATE as u64) as i64)
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

    /// Marks the recording's live transcript open from `from_sample`, or moves its mark there.
    pub fn open_transcript(&mut self, recording_id: Uuid, from_sample: u64) {
        match self.open_utterances.iter_mut().find(|o| o.recording_id == recording_id) {
            Some(o) => o.from_sample = from_sample,
            None => self.open_utterances.push(OpenUtterance { recording_id, from_sample }),
        }
    }

    /// The recording's live transcript is finished: drops its mark and returns it.
    pub fn close_transcript(&mut self, recording_id: Uuid) -> Option<OpenUtterance> {
        let i = self.open_utterances.iter().position(|o| o.recording_id == recording_id)?;
        Some(self.open_utterances.remove(i))
    }

    pub fn wall_time(&self, id: Uuid, sample: u64) -> Option<DateTime<Local>> {
        self.recordings.iter().find(|r| r.id == id).map(|r| wall_time_at(r.anchor, sample))
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

    #[test]
    fn audio_gaps_are_resolved_when_recorded_and_transcript_gaps_wait_for_recovery() {
        let id = Uuid::new_v4();
        for kind in [GapKind::CaptureOverflow, GapKind::RecorderOverflow, GapKind::DeviceGone, GapKind::RateChange, GapKind::Interrupted] {
            assert!(!kind.is_transcript());
            assert!(Gap::new(id, 0, None, kind).resolved, "{kind:?} has no audio to recover");
        }
        for kind in [GapKind::SttOffline, GapKind::SttOverflow, GapKind::SttRefused, GapKind::SttInterrupted] {
            assert!(kind.is_transcript());
            assert!(!Gap::new(id, 0, Some(16_000), kind).resolved, "{kind:?} waits for recovery");
        }
    }

    #[test]
    fn transcript_gaps_and_the_open_utterances_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = sidecar_path(dir.path(), "x");
        let (id, next) = (Uuid::new_v4(), Uuid::new_v4());
        let mut s = Sidecar::default();
        s.recordings.push(entry(id));
        s.gaps.push(Gap::new(id, 48_000, Some(96_000), GapKind::SttOffline));
        s.open_transcript(id, 0);
        s.open_transcript(next, 0);
        s.open_transcript(id, 96_000); // a new connection moves the mark
        assert_eq!(s.open_utterances, vec![OpenUtterance { recording_id: id, from_sample: 96_000 }, OpenUtterance { recording_id: next, from_sample: 0 }]);
        s.save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"stt_offline\"") && text.contains("\"open_utterances\""), "{text}");
        assert_eq!(Sidecar::load(&path).unwrap().unwrap(), s);

        assert_eq!(s.close_transcript(id), Some(OpenUtterance { recording_id: id, from_sample: 96_000 }));
        assert_eq!(s.close_transcript(id), None);
        s.close_transcript(next);
        s.save(&path).unwrap();
        assert!(!std::fs::read_to_string(&path).unwrap().contains("open_utterances"));
    }

    #[test]
    fn an_m1_sidecar_loads_with_no_open_utterances() {
        let m1 = r#"{"version":2,"recordings":[],"gaps":[{"recording_id":"6f2c1f7e-0d6b-4d0c-9b8e-2a4c1f0e9d31","start_sample":0,"end_sample":null,"kind":"device_gone","resolved":false}]}"#;
        let s: Sidecar = serde_json::from_str(m1).unwrap();
        assert!(s.open_utterances.is_empty());
        assert_eq!(s.gaps[0].kind, GapKind::DeviceGone);
    }

    #[test]
    fn wall_time_is_the_anchor_plus_samples() {
        let anchor = Local.with_ymd_and_hms(2026, 9, 25, 10, 0, 0).unwrap();
        assert_eq!(wall_time_at(anchor, 66_288), anchor + chrono::Duration::milliseconds(4_143));
    }

    #[test]
    fn slide_badges_default_to_manual_and_certain_and_are_not_written_when_unset() {
        let at = Local.with_ymd_and_hms(2026, 9, 25, 10, 5, 12).unwrap();
        let old = r#"{"version":2,"recordings":[],"gaps":[],"slides":[{"index":1,"file":"slides/slide_01_100512.png","shown_at":"2026-09-25T10:05:12+02:00"}]}"#;
        let sc: Sidecar = serde_json::from_str(old).unwrap();
        assert!(!sc.slides[0].auto && !sc.slides[0].uncertain, "an M4 sidecar's slides are screenshots");
        let manual = SlideEntry { index: 1, file: "slides/a.png".into(), shown_at: at, auto: false, uncertain: false };
        assert!(!serde_json::to_string(&manual).unwrap().contains("auto"));
        let auto = SlideEntry { auto: true, uncertain: true, ..manual };
        let back: SlideEntry = serde_json::from_str(&serde_json::to_string(&auto).unwrap()).unwrap();
        assert_eq!(back, auto);
    }

    #[test]
    fn notes_state_and_slides_round_trip_and_an_m2_sidecar_is_written_back_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let path = sidecar_path(dir.path(), "x");
        let mut s = Sidecar { lecture_date: chrono::NaiveDate::from_ymd_opt(2026, 9, 25), ..Sidecar::default() };
        s.notes = NotesState { revision: 3, len: 120, sha256: "ab".repeat(32), segment_cursor: 7, slide_index: 2 };
        s.slides.push(SlideEntry { index: 1, file: "slides/slide_01_100512.png".into(), shown_at: Local.with_ymd_and_hms(2026, 9, 25, 10, 5, 12).unwrap(), auto: false, uncertain: false });
        s.save(&path).unwrap();
        assert_eq!(Sidecar::load(&path).unwrap().unwrap(), s);

        let m2 = r#"{"version":2,"recordings":[],"gaps":[]}"#;
        let old: Sidecar = serde_json::from_str(m2).unwrap();
        assert_eq!((old.lecture_date, old.notes.clone(), old.slides.len()), (None, NotesState::default(), 0));
        assert_eq!(serde_json::to_string(&old).unwrap(), m2, "fields M3 did not set are not written");
    }
}
