//! The segment log and the transcript file (spec §8): every committed utterance, in commit order.
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::session::sidecar::wall_time_at;
use crate::stt::transcript::Word;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SegmentSource {
    Live,
    Recovered,
    /// A line imported from the Python CLI's transcript: second resolution, no words (spec §8).
    Imported,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Segment {
    /// Position in the log, from 0: the cursor a snapshot takes.
    pub id: u64,
    pub recording_id: Uuid,
    pub start_sample: u64,
    pub end_sample: u64,
    /// Wall time of the first word (of `start_sample` without words): the transcript line's time.
    pub said_at: DateTime<Local>,
    pub start: DateTime<Local>,
    pub end: DateTime<Local>,
    pub text: String,
    pub words: Vec<Word>,
    pub source: SegmentSource,
}

pub struct NewSegment {
    pub recording_id: Uuid,
    pub start_sample: u64,
    pub end_sample: u64,
    pub text: String,
    pub words: Vec<Word>,
    pub source: SegmentSource,
}

pub fn segments_path(dir: &Path, stem: &str) -> PathBuf {
    dir.join(".live_notes").join(format!("{stem}.segments.jsonl"))
}

/// `lecture_notes_YYYYMMDD` → `lecture_transcript_YYYYMMDD.txt`, the Python CLI's name.
pub fn transcript_path(dir: &Path, stem: &str) -> PathBuf {
    dir.join(format!("{}.txt", stem.replacen("lecture_notes", "lecture_transcript", 1)))
}

pub fn transcript_line(s: &Segment) -> String {
    format!("[{}] {}\n", s.said_at.format("%H:%M:%S"), s.text)
}

pub struct SegmentLog {
    log: File,
    transcript: File,
    /// Recording and interval of every logged segment, in log order.
    intervals: Vec<(Uuid, u64, u64)>,
}

impl SegmentLog {
    /// Opens the log and the transcript for appending, creating both. A last line cut short by a crash is removed.
    pub fn open(dir: &Path, stem: &str) -> Result<Self> {
        let path = segments_path(dir, stem);
        std::fs::create_dir_all(path.parent().expect("the log lives in .live_notes"))?;
        let (segments, torn_at) = read_complete(&path)?;
        let log = OpenOptions::new().create(true).append(true).open(&path).with_context(|| format!("open {}", path.display()))?;
        if let Some(len) = torn_at {
            log.set_len(len).with_context(|| format!("trim {}", path.display()))?;
        }
        let tpath = transcript_path(dir, stem);
        let transcript = OpenOptions::new().create(true).append(true).open(&tpath).with_context(|| format!("open {}", tpath.display()))?;
        Ok(Self { log, transcript, intervals: segments.iter().map(|s| (s.recording_id, s.start_sample, s.end_sample)).collect() })
    }

    pub fn len(&self) -> u64 {
        self.intervals.len() as u64
    }

    pub fn is_empty(&self) -> bool {
        self.intervals.is_empty()
    }

    /// Appends the segment to the log and syncs, then its line to the transcript and syncs. `anchor` is its recording's.
    pub fn append(&mut self, s: NewSegment, anchor: DateTime<Local>) -> Result<Segment> {
        let said = s.words.first().map_or(s.start_sample, |w| w.start_sample);
        let seg = Segment {
            id: self.len(),
            recording_id: s.recording_id,
            start_sample: s.start_sample,
            end_sample: s.end_sample,
            said_at: wall_time_at(anchor, said),
            start: wall_time_at(anchor, s.start_sample),
            end: wall_time_at(anchor, s.end_sample),
            text: s.text,
            words: s.words,
            source: s.source,
        };
        let mut line = serde_json::to_vec(&seg)?;
        line.push(b'\n');
        self.log.write_all(&line)?;
        self.log.sync_data()?;
        self.transcript.write_all(transcript_line(&seg).as_bytes())?;
        self.transcript.sync_data()?;
        self.intervals.push((seg.recording_id, seg.start_sample, seg.end_sample));
        Ok(seg)
    }

    pub fn last_end(&self, recording: Uuid) -> Option<u64> {
        self.intervals.iter().filter(|(r, _, _)| *r == recording).map(|(_, _, e)| *e).max()
    }

    /// End of the last segment of `recording` that starts in [from, to): how far recovery of that gap has committed.
    pub fn committed_within(&self, recording: Uuid, from: u64, to: u64) -> Option<u64> {
        self.intervals.iter().filter(|(r, s, _)| *r == recording && (from..to).contains(s)).map(|(_, _, e)| *e).max()
    }
}

/// Every complete segment of the log; a last line without its newline (a crash mid-write) is skipped.
pub fn read(path: &Path) -> Result<Vec<Segment>> {
    Ok(read_complete(path)?.0)
}

/// The complete segments, and the length to trim the file to when its last line is torn.
fn read_complete(path: &Path) -> Result<(Vec<Segment>, Option<u64>)> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((Vec::new(), None)),
        Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
    };
    let keep = bytes.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
    let segments = bytes[..keep]
        .split(|&b| b == b'\n')
        .filter(|l| !l.is_empty())
        .enumerate()
        .map(|(i, l)| serde_json::from_slice(l).with_context(|| format!("corrupt line {} of {}", i + 1, path.display())))
        .collect::<Result<_>>()?;
    Ok((segments, (keep < bytes.len()).then_some(keep as u64)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::io::Write;

    const STEM: &str = "lecture_notes_20260925";

    fn anchor() -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 9, 25, 10, 0, 0).unwrap()
    }

    fn word(text: &str, s: u64, e: u64) -> Word {
        Word { text: text.into(), start_sample: s, end_sample: e }
    }

    fn seg(rec: Uuid, s: u64, e: u64, text: &str, words: Vec<Word>, source: SegmentSource) -> NewSegment {
        NewSegment { recording_id: rec, start_sample: s, end_sample: e, text: text.into(), words, source }
    }

    #[test]
    fn segments_are_durable_lines_in_commit_order_with_cli_transcript_lines() {
        let dir = tempfile::tempdir().unwrap();
        let rec = Uuid::new_v4();
        let mut log = SegmentLog::open(dir.path(), STEM).unwrap();
        let a = log.append(seg(rec, 16, 48_000, "Transfer learning", vec![word("Transfer", 336, 7_744), word("learning", 8_704, 13_216)], SegmentSource::Live), anchor()).unwrap();
        let b = log.append(seg(rec, 48_000, 96_000, "losses", vec![], SegmentSource::Recovered), anchor()).unwrap();
        assert_eq!((a.id, b.id, log.len()), (0, 1, 2));
        assert_eq!(a.said_at, anchor() + chrono::Duration::microseconds(21_000));
        assert_eq!(b.said_at, anchor() + chrono::Duration::seconds(3));
        assert_eq!(read(&segments_path(dir.path(), STEM)).unwrap(), vec![a, b]);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("lecture_transcript_20260925.txt")).unwrap(),
            "[10:00:00] Transfer learning\n[10:00:03] losses\n"
        );
    }

    #[test]
    fn a_line_cut_short_by_a_crash_is_dropped_and_appending_goes_on() {
        let dir = tempfile::tempdir().unwrap();
        let rec = Uuid::new_v4();
        let mut log = SegmentLog::open(dir.path(), STEM).unwrap();
        log.append(seg(rec, 0, 16_000, "one", vec![], SegmentSource::Live), anchor()).unwrap();
        drop(log);
        let path = segments_path(dir.path(), STEM);
        OpenOptions::new().append(true).open(&path).unwrap().write_all(br#"{"id":1,"recording_id":"#).unwrap();
        assert_eq!(read(&path).unwrap().len(), 1, "a torn line is skipped on read");
        let mut log = SegmentLog::open(dir.path(), STEM).unwrap();
        assert_eq!(log.len(), 1);
        log.append(seg(rec, 16_000, 32_000, "two", vec![], SegmentSource::Live), anchor()).unwrap();
        let back = read(&path).unwrap();
        assert_eq!(back.iter().map(|s| (s.id, s.text.as_str())).collect::<Vec<_>>(), vec![(0, "one"), (1, "two")]);
    }

    #[test]
    fn a_corrupt_complete_line_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = segments_path(dir.path(), STEM);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"{not json}\n").unwrap();
        let err = SegmentLog::open(dir.path(), STEM).err().unwrap();
        assert!(format!("{err:#}").contains("corrupt line 1"), "{err:#}");
    }

    #[test]
    fn recovery_bounds_come_from_the_log() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let mut log = SegmentLog::open(dir.path(), STEM).unwrap();
        for (rec, s, e) in [(a, 0, 16_000), (a, 40_000, 56_000), (a, 56_000, 70_000), (b, 0, 8_000)] {
            log.append(seg(rec, s, e, "x", vec![], SegmentSource::Live), anchor()).unwrap();
        }
        assert_eq!(log.last_end(a), Some(70_000));
        assert_eq!(log.last_end(Uuid::new_v4()), None);
        assert_eq!(log.committed_within(a, 40_000, 100_000), Some(70_000));
        assert_eq!(log.committed_within(a, 16_000, 40_000), None);
        assert_eq!(log.committed_within(b, 0, 100_000), Some(8_000));
    }

    #[test]
    fn the_files_take_the_cli_names() {
        let dir = Path::new("/lecture");
        assert_eq!(transcript_path(dir, STEM), Path::new("/lecture/lecture_transcript_20260925.txt"));
        assert_eq!(segments_path(dir, STEM), Path::new("/lecture/.live_notes/lecture_notes_20260925.segments.jsonl"));
    }
}
