//! Whether a lecture folder's audio is whole (the M6 gate's first line): every recording holds what the sidecar
//! says, every stretch without audio between recordings is explained by a gap or a new session, no stretch is
//! recorded twice, and no two segments claim the same audio. What is unexplained, and what still waits (a
//! recording not yet repaired, a transcript gap not yet recovered), is counted.
use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, Local, NaiveDate, TimeZone};

use crate::session::files::LectureFiles;
use crate::session::segments;
use crate::session::sidecar::{GapKind, RecState, Sidecar};

/// A hole shorter than this between recordings is the same audio carried on (a rate change's rebuild).
const CONTIGUOUS_S: f64 = 1.0;
/// Two recordings may seem to overlap by this much from their anchors' millisecond clocks.
const OVERLAP_S: f64 = 0.5;
/// A new session's line may come this long before or after the stretch it explains.
const SESSION_SLACK_S: f64 = 2.0;
/// Segments may touch by this much (10 ms) without claiming the same audio.
const TOUCH_SAMPLES: u64 = 160;

#[derive(Debug, Default)]
pub struct Audit {
    /// One line per finding, in time order, in the CLI's words.
    pub lines: Vec<String>,
    pub unexplained: usize,
    pub waiting: usize,
}

impl Audit {
    fn note(&mut self, at: DateTime<Local>, what: String) {
        self.lines.push(format!("{}  {what}", at.format("%H:%M:%S")));
    }
    fn unexplained(&mut self, at: DateTime<Local>, what: String) {
        self.unexplained += 1;
        self.note(at, format!("{what}: unexplained"));
    }
    fn waiting(&mut self, at: DateTime<Local>, what: String) {
        self.waiting += 1;
        self.note(at, what);
    }
}

fn kind(k: GapKind) -> String {
    serde_json::to_value(k).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_else(|| format!("{k:?}"))
}

fn secs(samples: u64) -> f64 {
    samples as f64 / 16_000.0
}

/// Samples in a WAV the recorder wrote: its canonical 44-byte header, then 16-bit mono.
fn wav_samples(path: &Path) -> Option<u64> {
    std::fs::metadata(path).ok().map(|m| m.len().saturating_sub(44) / 2)
}

/// The times of the transcript's session lines (`--- started|resumed HH:MM:SS ---`), on the lecture's date and
/// rolled to the next day once a time runs back past midnight.
fn session_lines(transcript: &Path, date: NaiveDate) -> Vec<DateTime<Local>> {
    let re = regex::Regex::new(r"^--- (?:started|resumed) (\d\d):(\d\d):(\d\d) ---$").expect("the session line");
    let text = std::fs::read_to_string(transcript).unwrap_or_default();
    let (mut day, mut last) = (date, None);
    let mut out = Vec::new();
    for c in text.lines().filter_map(|l| re.captures(l)) {
        let [h, m, s] = [1, 2, 3].map(|i| c[i].parse::<u32>().unwrap_or(0));
        let Some(t) = chrono::NaiveTime::from_hms_opt(h, m, s) else { continue };
        if last.is_some_and(|l| t < l) {
            day = day.succ_opt().unwrap_or(day);
        }
        last = Some(t);
        if let Some(at) = Local.from_local_datetime(&day.and_time(t)).single() {
            out.push(at);
        }
    }
    out
}

pub fn audit_day(files: &LectureFiles) -> Result<Audit> {
    let sc = Sidecar::load(&files.sidecar())?.with_context(|| format!("no sidecar at {}", files.sidecar().display()))?;
    let segs = if files.segments().exists() { segments::read(&files.segments())? } else { Vec::new() };
    let sessions = session_lines(&files.transcript, files.date);
    let mut a = Audit::default();
    let mut recs: Vec<_> = sc.recordings.iter().collect();
    recs.sort_by_key(|r| r.anchor);
    let mut prev: Option<(DateTime<Local>, &crate::session::sidecar::RecordingEntry)> = None;
    for r in recs {
        let path = files.dir.join(&r.file);
        let samples = match r.state {
            RecState::Open => {
                a.waiting(r.anchor, format!("{} waits for repair (a session in this folder repairs it)", r.file));
                wav_samples(&path).unwrap_or(0)
            }
            RecState::Missing => {
                a.note(r.anchor, format!("{} is missing (marked as a gap when it was found gone)", r.file));
                r.samples.unwrap_or(0)
            }
            RecState::Deleted => {
                a.note(r.anchor, format!("{} was deleted by retention (kept in the sidecar)", r.file));
                r.samples.unwrap_or(0)
            }
            RecState::Finalized | RecState::Repaired => {
                let want = r.samples.unwrap_or(0);
                match wav_samples(&path) {
                    Some(n) if n == want => a.note(r.anchor, format!("{} {:.1} s", r.file, secs(want))),
                    Some(n) => a.unexplained(r.anchor, format!("{} holds {:.3} s, the sidecar says {:.3} s", r.file, secs(n), secs(want))),
                    None => a.unexplained(r.anchor, format!("{} is not on disk", r.file)),
                }
                want
            }
        };
        let start = r.anchor;
        let end = crate::session::sidecar::wall_time_at(start, samples);
        if let Some((prev_end, p)) = prev {
            let hole = (start - prev_end).num_milliseconds() as f64 / 1000.0;
            if hole < -OVERLAP_S {
                a.unexplained(start, format!("{} begins {:.1} s before {} ends", r.file, -hole, p.file));
            } else if hole > CONTIGUOUS_S {
                let terminal = sc.gaps.iter().find(|g| g.recording_id == p.id && g.end_sample.is_none());
                let slack = chrono::Duration::milliseconds((SESSION_SLACK_S * 1000.0) as i64);
                let session = sessions.iter().any(|&s| s >= prev_end - slack && s <= start + slack);
                // A pause covers the hole when it began before the next recording did and had not ended before the last
                // stopped. One a crash left open covers only the hole it began in, not every later one.
                let paused = sc.pauses.iter().any(|p| p.from <= start + slack && p.to.map_or(p.from >= prev_end - slack, |t| t >= prev_end - slack));
                match (terminal, paused, session) {
                    (Some(g), _, _) => a.note(prev_end, format!("hole of {hole:.1} s after {}: {} (explained)", p.file, kind(g.kind))),
                    (None, true, _) => a.note(prev_end, format!("hole of {hole:.1} s after {}: a pause (explained)", p.file)),
                    (None, false, true) => a.note(prev_end, format!("hole of {hole:.1} s after {}: a stop and a new session (explained)", p.file)),
                    (None, false, false) => a.unexplained(prev_end, format!("hole of {hole:.1} s after {}", p.file)),
                }
            }
        }
        for g in sc.gaps.iter().filter(|g| g.recording_id == r.id) {
            let span = match g.end_sample {
                Some(e) => format!("{:.1}–{:.1} s", secs(g.start_sample), secs(e)),
                None => format!("{:.1} s to the end", secs(g.start_sample)),
            };
            let at = crate::session::sidecar::wall_time_at(r.anchor, g.start_sample);
            if g.kind.is_transcript() && !g.resolved {
                a.waiting(at, format!("gap {} {span} of {}: waits for recovery", kind(g.kind), r.file));
            } else if g.kind.is_transcript() {
                a.note(at, format!("gap {} {span} of {}: recovered", kind(g.kind), r.file));
            } else {
                a.note(at, format!("gap {} {span} of {} (explained)", kind(g.kind), r.file));
            }
        }
        prev = Some((end, r));
    }
    let mut by_recording: BTreeMap<uuid::Uuid, Vec<&segments::Segment>> = BTreeMap::new();
    for s in &segs {
        by_recording.entry(s.recording_id).or_default().push(s);
    }
    for list in by_recording.values_mut() {
        list.sort_by_key(|s| s.start_sample);
        for w in list.windows(2) {
            if w[1].start_sample + TOUCH_SAMPLES < w[0].end_sample {
                a.unexplained(w[1].start, format!("segments {} and {} claim the same {:.2} s", w[0].id, w[1].id, secs(w[0].end_sample - w[1].start_sample)));
            }
        }
    }
    a.lines.sort();
    Ok(a)
}

/// The audit's exit status: 0 only for a whole folder; 1 when anything is unexplained; 2 when nothing is unexplained but
/// something still waits (a recording not yet repaired, a transcript gap not yet recovered), which it cannot vouch for.
pub fn exit_code(days: &[(String, Audit)]) -> i32 {
    if days.iter().any(|(_, a)| a.unexplained > 0) {
        1
    } else if days.iter().any(|(_, a)| a.waiting > 0) {
        2
    } else {
        0
    }
}

/// Every day's sidecar in the folder, by its stem (`lecture_notes_YYYYMMDD`).
pub fn audit_folder(dir: &Path) -> Result<Vec<(String, Audit)>> {
    let state = dir.join(".live_notes");
    let mut stems: Vec<String> = std::fs::read_dir(&state)
        .with_context(|| format!("no lecture state in {}", state.display()))?
        .flatten()
        .filter_map(|e| e.file_name().to_string_lossy().strip_suffix(".v2.json").map(str::to_string))
        .collect();
    stems.sort();
    let mut out = Vec::new();
    for stem in stems {
        let Some(date) = stem.strip_prefix("lecture_notes_").and_then(|d| NaiveDate::parse_from_str(d, "%Y%m%d").ok()) else { continue };
        out.push((stem, audit_day(&LectureFiles::standard(dir, date))?));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::frame::Frame;
    use crate::audio::recorder::Recorder;
    use crate::session::segments::{self, Segment, SegmentSource};
    use crate::session::sidecar::{wall_time_at, Gap, GapKind, RecState, RecordingEntry, Sidecar};
    use chrono::{DateTime, Local, TimeZone};
    use uuid::Uuid;

    struct Folder {
        _dir: tempfile::TempDir,
        files: LectureFiles,
        sc: Sidecar,
    }

    fn folder() -> Folder {
        let dir = tempfile::tempdir().unwrap();
        let files = LectureFiles::standard(dir.path(), chrono::NaiveDate::from_ymd_opt(2026, 9, 26).unwrap());
        std::fs::create_dir_all(files.state_dir()).unwrap();
        std::fs::write(&files.transcript, "--- started 10:00:00 ---\n").unwrap();
        Folder { _dir: dir, files, sc: Sidecar::default() }
    }

    impl Folder {
        /// A finalized recording of `secs` seconds starting at 10:mm:ss.
        fn recording(&mut self, at: (u32, u32), secs: u64) -> Uuid {
            let id = Uuid::new_v4();
            let stem = format!("session_20260926_10{:02}{:02}", at.0, at.1);
            let mut r = Recorder::create(&self.files.dir.join("recordings"), &stem).unwrap();
            for k in 0..secs * 10 {
                r.write_frame(&Frame { recording_id: id, sample_offset: k * 1600, valid_samples: 1600, pcm16: [0; 1600] }).unwrap();
            }
            let path = r.finalize().unwrap();
            let file = path.strip_prefix(&self.files.dir).unwrap().to_string_lossy().into_owned();
            let anchor = Local.with_ymd_and_hms(2026, 9, 26, 10, at.0, at.1).unwrap();
            self.sc.recordings.push(RecordingEntry { id, file, anchor, source_uid: "BlackHole2ch_UID".into(), input_rate: 48_000, samples: Some(secs * 16_000), state: RecState::Finalized });
            id
        }

        fn run(&self) -> Audit {
            self.sc.save(&self.files.sidecar()).unwrap();
            audit_day(&self.files).unwrap()
        }
    }

    fn seg(id: u64, recording_id: Uuid, s: u64, e: u64, anchor: DateTime<Local>) -> Segment {
        let (start, end) = (wall_time_at(anchor, s), wall_time_at(anchor, e));
        Segment { id, recording_id, start_sample: s, end_sample: e, said_at: start, start, end, text: format!("segment {id}"), words: vec![], source: SegmentSource::Live }
    }

    #[test]
    fn contiguous_recordings_and_a_marked_absence_are_whole() {
        let mut f = folder();
        let a = f.recording((0, 0), 60);
        f.sc.gaps.push(Gap::new(a, 60 * 16_000, None, GapKind::DeviceGone));
        f.recording((1, 30), 60); // 30 s later: the receiver was away
        let audit = f.run();
        assert_eq!((audit.unexplained, audit.waiting), (0, 0), "{:#?}", audit.lines);
        assert!(audit.lines.iter().any(|l| l.contains("device_gone")), "{:#?}", audit.lines);
    }

    #[test]
    fn a_hole_without_a_gap_or_a_new_session_is_unexplained() {
        let mut f = folder();
        f.recording((0, 0), 60);
        f.recording((1, 30), 60);
        let audit = f.run();
        assert_eq!(audit.unexplained, 1, "{:#?}", audit.lines);
        assert!(audit.lines.iter().any(|l| l.contains("30.0 s") && l.contains("unexplained")), "{:#?}", audit.lines);
    }

    #[test]
    fn a_hole_with_a_new_session_line_is_a_stop_and_a_start() {
        let mut f = folder();
        f.recording((0, 0), 60);
        std::fs::write(&f.files.transcript, "--- started 10:00:00 ---\n--- resumed 10:01:25 ---\n").unwrap();
        f.recording((1, 30), 60);
        let audit = f.run();
        assert_eq!(audit.unexplained, 0, "{:#?}", audit.lines);
    }

    /// A pause is the person's own doing: nothing was recorded in it, and the sidecar says when it began and ended.
    #[test]
    fn a_hole_inside_a_recorded_pause_is_explained_and_one_outside_it_is_not() {
        let mut f = folder();
        f.recording((0, 0), 60); // ends 10:01:00
        f.recording((1, 30), 60); // begins 10:01:30
        let at = |m, s| Local.with_ymd_and_hms(2026, 9, 26, 10, m, s).unwrap();
        f.sc.pauses.push(crate::session::sidecar::PauseSpan { from: at(1, 1), to: Some(at(1, 30)) });
        let audit = f.run();
        assert_eq!(audit.unexplained, 0, "{:#?}", audit.lines);
        assert!(audit.lines.iter().any(|l| l.contains("a pause (explained)")), "{:#?}", audit.lines);

        let mut elsewhere = folder();
        elsewhere.recording((0, 0), 60);
        elsewhere.recording((1, 30), 60);
        elsewhere.sc.pauses.push(crate::session::sidecar::PauseSpan { from: at(5, 0), to: Some(at(6, 0)) });
        assert_eq!(elsewhere.run().unexplained, 1, "a pause at another time explains nothing");
    }

    #[test]
    fn a_pause_a_crash_left_open_explains_the_hole_it_began_in_and_no_later_one() {
        let mut f = folder();
        f.recording((0, 0), 60); // ends 10:01:00
        f.recording((1, 30), 60); // begins 10:01:30: the pause's hole
        f.sc.pauses.push(crate::session::sidecar::PauseSpan { from: Local.with_ymd_and_hms(2026, 9, 26, 10, 1, 1).unwrap(), to: None });
        let audit = f.run();
        assert_eq!(audit.unexplained, 0, "{:#?}", audit.lines);
        f.recording((5, 0), 60); // 10:02:30 to 10:05:00 is nobody's pause
        let audit = f.run();
        assert_eq!(audit.unexplained, 1, "an open pause is not a licence for every later hole: {:#?}", audit.lines);
    }

    #[test]
    fn overlapping_recordings_are_unexplained() {
        let mut f = folder();
        f.recording((0, 0), 60);
        f.recording((0, 50), 60);
        assert_eq!(f.run().unexplained, 1);
    }

    #[test]
    fn a_wav_shorter_than_its_sidecar_says_is_unexplained() {
        let mut f = folder();
        f.recording((0, 0), 10);
        f.sc.recordings[0].samples = Some(11 * 16_000);
        let audit = f.run();
        assert_eq!(audit.unexplained, 1, "{:#?}", audit.lines);
    }

    /// Review Focus 5.
    #[test]
    fn an_unrepaired_recording_is_waiting_not_whole() {
        let mut f = folder();
        f.recording((0, 0), 10);
        f.sc.recordings[0].state = RecState::Open;
        f.sc.recordings[0].samples = None;
        let audit = f.run();
        assert_eq!(audit.waiting, 1, "{:#?}", audit.lines);
        assert!(audit.lines.iter().any(|l| l.contains("waits for repair")), "{:#?}", audit.lines);
    }

    #[test]
    fn overlapping_segments_are_an_unexplained_duplicate() {
        let mut f = folder();
        let a = f.recording((0, 0), 10);
        let anchor = f.sc.recordings[0].anchor;
        segments::write_all(&f.files.segments(), &[seg(0, a, 0, 32_000, anchor), seg(1, a, 16_000, 48_000, anchor)]).unwrap();
        let audit = f.run();
        assert_eq!(audit.unexplained, 1, "{:#?}", audit.lines);
    }

    #[test]
    fn an_unrecovered_transcript_gap_is_waiting() {
        let mut f = folder();
        let a = f.recording((0, 0), 10);
        f.sc.gaps.push(Gap::new(a, 0, Some(16_000), GapKind::SttOffline));
        let audit = f.run();
        assert_eq!((audit.unexplained, audit.waiting), (0, 1), "{:#?}", audit.lines);
    }

    /// Final review, I3 (Review Focus 5): only a whole folder exits 0; anything unexplained is 1, anything still waiting
    /// (a recording not repaired, a transcript gap not recovered) is 2, since the audit cannot vouch for it yet.
    #[test]
    fn the_exit_status_is_0_only_for_a_whole_folder() {
        let day = |unexplained, waiting| ("lecture_notes_20260926".to_string(), Audit { lines: vec![], unexplained, waiting });
        assert_eq!(exit_code(&[day(0, 0)]), 0);
        assert_eq!(exit_code(&[day(0, 0), day(0, 1)]), 2, "waiting");
        assert_eq!(exit_code(&[day(1, 3)]), 1, "unexplained comes first");
    }

    #[test]
    fn a_folder_is_audited_day_by_day() {
        let mut f = folder();
        f.recording((0, 0), 10);
        f.sc.save(&f.files.sidecar()).unwrap();
        let days = audit_folder(&f.files.dir).unwrap();
        assert_eq!(days.iter().map(|(s, _)| s.as_str()).collect::<Vec<_>>(), ["lecture_notes_20260926"]);
    }
}
