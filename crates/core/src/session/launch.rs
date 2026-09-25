use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Local};
use uuid::Uuid;

use crate::audio::recorder::repair_header;
use crate::session::sidecar::{Gap, GapKind, RecState, Sidecar};

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

/// Runs before a session starts in `dir` (caller holds the folder lock): for every sidecar in
/// the folder, whatever day it belongs to, repairs recordings a crash left open, marks their
/// lost tails as gaps, then applies retention (spec §4.4).
pub fn recover(dir: &Path, retention: Retention, now: DateTime<Local>) -> Result<LaunchReport> {
    let mut report = LaunchReport::default();
    let state_dir = dir.join(".live_notes");
    let entries = match std::fs::read_dir(&state_dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(report),
        Err(e) => return Err(e).with_context(|| format!("read {}", state_dir.display())),
    };
    let mut sidecars: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.ends_with(".v2.json")))
        .collect();
    sidecars.sort();
    for path in sidecars {
        recover_one(dir, &path, retention, now, &mut report)?;
    }
    Ok(report)
}

fn recover_one(dir: &Path, path: &Path, retention: Retention, now: DateTime<Local>, report: &mut LaunchReport) -> Result<()> {
    let Some(mut sc) = Sidecar::load(path)? else { return Ok(()) };
    let before = (report.repaired.len(), report.missing.len(), report.pruned.len());
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
    if before != (report.repaired.len(), report.missing.len(), report.pruned.len()) {
        sc.save(path)?;
    }
    Ok(())
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

/// The M0 canary can still make its own aggregate the system default. At launch, a route
/// it left behind is undone (spec §4.3). Returns None when no route was saved, otherwise
/// whether the default output was put back.
pub fn restore_abandoned_route(state_path: &Path) -> Result<Option<bool>> {
    if crate::audio::routing::load_state(state_path)?.is_none() {
        return Ok(None);
    }
    Ok(Some(crate::audio::routing::disable_loopback(state_path)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::recorder::Recorder;
    use crate::session::sidecar::{sidecar_path, Gap, GapKind, RecordingEntry, Sidecar};
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

        let report = recover(dir.path(), Retention::KeepAll, now()).unwrap();
        assert_eq!(report.repaired.len(), 1);
        assert!(report.repaired[0].1 >= 32_000);

        let sc = Sidecar::load(&path).unwrap().unwrap();
        let r = &sc.recordings[0];
        assert_eq!(r.state, RecState::Repaired);
        assert_eq!(r.samples, Some(report.repaired[0].1));
        assert_eq!(sc.gaps, vec![Gap { recording_id: id, start_sample: report.repaired[0].1, end_sample: None, kind: GapKind::Interrupted, resolved: false }]);
        assert_eq!(hound::WavReader::open(dir.path().join(&file)).unwrap().len() as u64, report.repaired[0].1);

        let again = recover(dir.path(), Retention::KeepAll, now()).unwrap();
        assert!(again.repaired.is_empty(), "a second launch repairs nothing");
    }

    #[test]
    fn missing_open_recording_is_marked_missing() {
        let dir = tempfile::tempdir().unwrap();
        let id = Uuid::new_v4();
        let mut sc = Sidecar::default();
        sc.recordings.push(entry(id, "recordings/gone.wav", 0, RecState::Open));
        sc.save(&sidecar_path(dir.path(), STEM)).unwrap();
        let report = recover(dir.path(), Retention::KeepAll, now()).unwrap();
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
        let err = recover(dir.path(), Retention::KeepAll, now()).unwrap_err();
        assert!(format!("{err:#}").contains("corrupt sidecar"), "{err:#}");
        assert_eq!(std::fs::read(&path).unwrap(), b"{\"version\":2,\"recordings\":[");
    }

    #[test]
    fn no_sidecar_is_a_fresh_folder() {
        let dir = tempfile::tempdir().unwrap();
        let report = recover(dir.path(), Retention::KeepDays(1), now()).unwrap();
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

    /// A crash during an evening recording, relaunched the next day: the folder's other
    /// sidecars are repaired and pruned too, not only today's.
    #[test]
    fn an_earlier_days_sidecar_is_repaired_and_pruned_at_launch() {
        let dir = tempfile::tempdir().unwrap();
        let file = crashed_recording(dir.path());
        std::fs::write(dir.path().join("recordings/old.wav"), b"x").unwrap();
        let (open, old) = (Uuid::new_v4(), Uuid::new_v4());
        let mut yesterday = Sidecar::default();
        yesterday.recordings.push(RecordingEntry { samples: None, ..entry(open, &file, 1, RecState::Open) });
        yesterday.save(&sidecar_path(dir.path(), "lecture_notes_20260924")).unwrap();
        let mut last_month = Sidecar::default();
        last_month.recordings.push(entry(old, "recordings/old.wav", 30, RecState::Finalized));
        last_month.save(&sidecar_path(dir.path(), "lecture_notes_20260826")).unwrap();

        let report = recover(dir.path(), Retention::KeepDays(14), now()).unwrap();
        assert_eq!(report.repaired.len(), 1);
        assert_eq!(report.pruned, vec![dir.path().join("recordings/old.wav")]);
        let y = Sidecar::load(&sidecar_path(dir.path(), "lecture_notes_20260924")).unwrap().unwrap();
        assert_eq!(y.recordings[0].state, RecState::Repaired);
        let m = Sidecar::load(&sidecar_path(dir.path(), "lecture_notes_20260826")).unwrap().unwrap();
        assert_eq!(m.recordings[0].state, RecState::Deleted);
    }

    #[test]
    fn no_saved_route_means_nothing_to_restore() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(restore_abandoned_route(&dir.path().join("route.json")).unwrap(), None);
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
        let report = recover(dir.path(), Retention::KeepDays(14), now()).unwrap();
        assert_eq!(report.pruned, vec![dir.path().join("recordings/old.wav")]);
        assert!(!dir.path().join("recordings/old.wav").exists());
        let sc = Sidecar::load(&sidecar_path(dir.path(), STEM)).unwrap().unwrap();
        assert_eq!(sc.recordings[0].state, RecState::Deleted);
    }
}
