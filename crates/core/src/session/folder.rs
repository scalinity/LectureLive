//! A lecture folder's initialisation (spec §8): resume, migrate from the Python CLI, rebuild, or
//! start. Runs under the folder lock before a session; the Python CLI's formats are read as it
//! writes them.
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use anyhow::{Context, Result};
use chrono::{DateTime, Local, NaiveDate, NaiveTime, TimeZone};
use regex::Regex;
use serde_json::Value;
use uuid::Uuid;

use crate::audio::source::NoAudio;
use crate::session::coordinator::{self, SessionConfig, StopReport};
use crate::session::files::LectureFiles;
use crate::session::notesfile::{self, Recovered};
use crate::session::segments::{self, Segment, SegmentSource};
use crate::session::sidecar::{Sidecar, SlideEntry};
use crate::session::spend::Spend;
use crate::stt::rest::RecoveryLink;

/// The CLI's slide names: `slide_NN_HHMMSS.png|jpg|jpeg` (live_notes.py `SLIDE_RE`).
static SLIDE_NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^slide_(\d+)_(\d{6})\.(png|jpe?g)$").expect("a valid pattern"));
/// A transcript line (`LINE_RE`).
static LINE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\[(\d\d:\d\d:\d\d)\] (.+)$").expect("a valid pattern"));
/// A snapshot marker in the notes.
static MARKER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^<!-- (\d\d:\d\d:\d\d) -->$").expect("a valid pattern"));

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum How {
    #[default]
    Resumed,
    Created,
    Migrated,
    Rebuilt,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct InitReport {
    pub how: How,
    pub journal: Recovered,
    pub external_edit: bool,
    pub slides_registered: usize,
    pub pending_segments: u64,
    pub pending_slides: usize,
    /// What finishing the Python CLI's interrupted snapshot did, in its own words.
    pub legacy_commit: Option<&'static str>,
    pub corrupt_kept: Option<PathBuf>,
}

/// Spec §8's initialisation table; the caller holds the folder lock.
pub fn open(files: &LectureFiles, title: &str, rebuild: bool) -> Result<(Sidecar, InitReport)> {
    std::fs::create_dir_all(files.state_dir())?;
    std::fs::create_dir_all(&files.slides).with_context(|| format!("create {}", files.slides.display()))?;
    let mut report = InitReport::default();
    let mut sc = match Sidecar::load(&files.sidecar()) {
        Ok(Some(sc)) => sc,
        Ok(None) => fresh(files, title, &mut report)?,
        Err(_) if rebuild => {
            let kept = files.sidecar().with_file_name(format!("{}.v2.json.corrupt-{}", files.stem, Local::now().format("%H%M%S")));
            std::fs::rename(files.sidecar(), &kept).context("set the corrupt sidecar aside")?;
            report.corrupt_kept = Some(kept);
            report.how = How::Rebuilt;
            rebuilt(files)?
        }
        Err(e) => return Err(e.context("run again with --rebuild to rebuild it from the notes, transcript and slides: everything after the last <!-- --> marker becomes pending, and the corrupt file is kept beside it")),
    };
    sc.lecture_date.get_or_insert(files.date);
    report.journal = notesfile::recover(files, &mut sc)?;
    if !files.notes.exists() {
        std::fs::write(&files.notes, format!("{title}\n")).with_context(|| format!("create {}", files.notes.display()))?;
    }
    let edited = notesfile::accept_external_edit(files, &mut sc)?;
    report.external_edit = edited && report.how == How::Resumed;
    for s in slide_files(files)? {
        if !sc.slides.iter().any(|x| x.index == s.index) {
            sc.slides.push(s);
            report.slides_registered += 1;
        }
    }
    sc.slides.sort_by_key(|s| s.index);
    sc.save(&files.sidecar())?;
    report.pending_segments = (segments::read(&files.segments())?.len() as u64).saturating_sub(sc.notes.segment_cursor);
    report.pending_slides = sc.slides.iter().filter(|s| s.index > sc.notes.slide_index).count();
    Ok((sc, report))
}

fn fresh(files: &LectureFiles, title: &str, report: &mut InitReport) -> Result<Sidecar> {
    if files.legacy_state().exists() {
        report.how = How::Migrated;
        return migrate(files, report);
    }
    if files.notes.exists() {
        report.how = How::Rebuilt;
        return rebuilt(files);
    }
    std::fs::write(&files.notes, format!("{title}\n")).with_context(|| format!("create {}", files.notes.display()))?;
    report.how = How::Created;
    import(files, |_| false, 0)
}

struct Line {
    pos: usize,
    clock: String,
    text: String,
}

fn transcript_lines(files: &LectureFiles) -> Result<Vec<Line>> {
    let bytes = match std::fs::read(&files.transcript) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("read {}", files.transcript.display())),
    };
    let mut out = Vec::new();
    let mut pos = 0;
    for raw in bytes.split_inclusive(|&b| b == b'\n') {
        let text = String::from_utf8_lossy(raw);
        if let Some(c) = LINE.captures(text.trim_end_matches('\n')) {
            out.push(Line { pos, clock: c[1].to_string(), text: c[2].to_string() });
        }
        pos += raw.len();
    }
    Ok(out)
}

/// A local time on a day; in a daylight-saving gap, the hour after it.
fn local(date: NaiveDate, t: NaiveTime) -> DateTime<Local> {
    let naive = date.and_time(t);
    Local.from_local_datetime(&naive).earliest().unwrap_or_else(|| Local.from_local_datetime(&(naive + chrono::Duration::hours(1))).earliest().expect("an hour past a gap exists"))
}

/// The CLI's transcript lines as segments: `imported`, no words, at the lecture day's clock time,
/// a day later once the clock runs back past midnight.
fn imported(lines: &[Line], date: NaiveDate) -> Vec<Segment> {
    let (mut day, mut prev) = (date, None::<NaiveTime>);
    lines
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let t = NaiveTime::parse_from_str(&l.clock, "%H:%M:%S").expect("LINE matched HH:MM:SS");
            if prev.is_some_and(|p| p - t > chrono::Duration::hours(12)) {
                day = day.succ_opt().expect("a next day");
            }
            prev = Some(t);
            let at = local(day, t);
            Segment { id: i as u64, recording_id: Uuid::nil(), start_sample: 0, end_sample: 0, said_at: at, start: at, end: at, text: l.text.clone(), words: vec![], source: SegmentSource::Imported }
        })
        .collect()
}

/// A path as the sidecar keeps it: relative to the lecture folder when inside it.
pub fn relative(files: &LectureFiles, path: &Path) -> String {
    path.strip_prefix(&files.dir).unwrap_or(path).to_string_lossy().into_owned()
}

/// Slide files already named as the CLI names them, by index; each is dated the lecture day at the time in its name.
pub fn slide_files(files: &LectureFiles) -> Result<Vec<SlideEntry>> {
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(&files.slides) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e).with_context(|| format!("read {}", files.slides.display())),
    };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(c) = SLIDE_NAME.captures(&name) else { continue };
        let (Ok(index), Ok(t)) = (c[1].parse::<u32>(), NaiveTime::parse_from_str(&c[2], "%H%M%S")) else { continue };
        out.push(SlideEntry { index, file: relative(files, &e.path()), shown_at: local(files.date, t) });
    }
    out.sort_by_key(|s| s.index);
    Ok(out)
}

/// The next slide index: one past the highest registered or on disk (spec §8).
pub fn next_slide_index(files: &LectureFiles, sc: &Sidecar) -> Result<u32> {
    let on_disk = slide_files(files)?.iter().map(|s| s.index).max().unwrap_or(0);
    Ok(sc.slides.iter().map(|s| s.index).max().unwrap_or(0).max(on_disk) + 1)
}

/// Transcript lines and slide files as segments and slides. The leading segments for which `noted`
/// holds, and slides up to `slide_index`, count as in the notes. Writes the segment log.
fn import(files: &LectureFiles, noted: impl Fn(&Segment) -> bool, slide_index: u32) -> Result<Sidecar> {
    let segs = imported(&transcript_lines(files)?, files.date);
    segments::write_all(&files.segments(), &segs)?;
    let mut sc = Sidecar { lecture_date: Some(files.date), slides: slide_files(files)?, ..Sidecar::default() };
    sc.notes.segment_cursor = segs.iter().take_while(|s| noted(s)).count() as u64;
    sc.notes.slide_index = slide_index;
    Ok(sc)
}

/// Finishes or undoes the CLI's interrupted snapshot as its `recover_commit` does, in its words.
fn legacy_commit(files: &LectureFiles, state: &mut Value) -> Result<Option<&'static str>> {
    let Some(c) = state.as_object_mut().and_then(|o| o.remove("commit")) else { return Ok(None) };
    let block = c["block"].as_str().unwrap_or_default().as_bytes().to_vec();
    let before = c["before"].as_u64().unwrap_or(0) as usize;
    let data = std::fs::read(&files.notes).with_context(|| format!("read {}", files.notes.display()))?;
    let tail = data.get(before..).unwrap_or(&[]);
    if data.len() >= before && tail == block.as_slice() {
        state["transcript_offset"] = c["transcript_offset"].clone();
        state["slide_index"] = c["slide_index"].clone();
        Ok(Some("the last snapshot was fully written"))
    } else if data.len() > before && block.starts_with(tail) {
        let f = std::fs::OpenOptions::new().write(true).open(&files.notes)?;
        f.set_len(before as u64)?;
        f.sync_all()?;
        Ok(Some("removed a half-written snapshot; its material is queued again"))
    } else if data.len() != before {
        Ok(Some("the notes changed during an interrupted snapshot; left as they are, material queued again"))
    } else {
        Ok(None)
    }
}

fn migrate(files: &LectureFiles, report: &mut InitReport) -> Result<Sidecar> {
    let path = files.legacy_state();
    let mut state: Value = serde_json::from_slice(&std::fs::read(&path)?).with_context(|| format!("read the Python CLI's state {}", path.display()))?;
    report.legacy_commit = legacy_commit(files, &mut state)?;
    let lines = transcript_lines(files)?;
    if let Some(offset) = state["transcript_offset"].as_u64() {
        let n = lines.iter().filter(|l| (l.pos as u64) < offset).count();
        return import(files, |s| (s.id as usize) < n, state["slide_index"].as_u64().unwrap_or(0) as u32);
    }
    if let Some(cut) = state["noted_through"].as_str().map(str::to_string) {
        // The older checkpoint: the first line at or after it starts the pending part (initial_state).
        let n = lines.iter().position(|l| l.clock >= cut).unwrap_or(lines.len());
        let slide_index = slide_files(files)?.iter().filter(|s| s.shown_at.format("%H:%M:%S").to_string() < cut).map(|s| s.index).max().unwrap_or(0);
        return import(files, |s| (s.id as usize) < n, slide_index);
    }
    rebuilt(files)
}

/// Spec §8's rebuild: everything after the last `<!-- HH:MM:SS -->` marker is pending. A segment log
/// already there is kept.
fn rebuilt(files: &LectureFiles) -> Result<Sidecar> {
    let doc = std::fs::read_to_string(&files.notes).unwrap_or_default();
    let upto = MARKER.captures_iter(&doc).last().and_then(|c| NaiveTime::parse_from_str(&c[1], "%H:%M:%S").ok()).map(|t| local(files.date, t));
    let noted = |at: DateTime<Local>| upto.is_some_and(|u| at <= u);
    let slide_index = slide_files(files)?.iter().filter(|s| noted(s.shown_at)).map(|s| s.index).max().unwrap_or(0);
    let logged = segments::read(&files.segments())?;
    if logged.is_empty() {
        return import(files, |s| noted(s.said_at), slide_index);
    }
    let mut sc = Sidecar { lecture_date: Some(files.date), slides: slide_files(files)?, ..Sidecar::default() };
    sc.notes.segment_cursor = logged.iter().take_while(|s| noted(s.said_at)).count() as u64;
    sc.notes.slide_index = slide_index;
    Ok(sc)
}

/// Transcript gaps other days' sessions left in this folder (spec §5.4): a recovery-only session per
/// such day, before today's starts, announced to `on_start` by its stem. Their segments reach that
/// day's transcript; its notes stay as they are.
pub async fn recover_other_days(dir: &Path, today: &str, recovery: impl Fn() -> Result<RecoveryLink>, spend: Option<Spend>, on_start: &mut (dyn FnMut(&str) + Send)) -> Result<Vec<(String, StopReport)>> {
    let mut sidecars: Vec<PathBuf> = match std::fs::read_dir(dir.join(".live_notes")) {
        Ok(e) => e.flatten().map(|e| e.path()).filter(|p| p.to_string_lossy().ends_with(".v2.json")).collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    sidecars.sort();
    let mut out = Vec::new();
    for path in sidecars {
        let stem = path.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_suffix(".v2.json")).unwrap_or_default().to_string();
        if stem == today {
            continue;
        }
        let Some(sc) = Sidecar::load(&path)? else { continue };
        if !sc.gaps.iter().any(|g| g.kind.is_transcript() && !g.resolved) {
            continue;
        }
        on_start(&stem);
        let cfg = SessionConfig { dir: dir.to_path_buf(), stem: stem.clone(), recovery: Some(recovery()?), spend: spend.clone(), ..Default::default() };
        let (handle, _notes) = coordinator::spawn(cfg, Box::new(NoAudio));
        out.push((stem, handle.finish().await?));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::segments::{self, SegmentSource};
    use chrono::{NaiveDate, TimeZone};
    use std::io::Write;

    const TITLE: &str = "# Machine Learning — Week 01 — 2026-09-25";

    fn files(dir: &Path) -> LectureFiles {
        LectureFiles::standard(dir, NaiveDate::from_ymd_opt(2026, 9, 25).unwrap())
    }

    fn png(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        image::RgbImage::from_pixel(8, 8, image::Rgb([255, 255, 255])).save(path).unwrap();
    }

    /// Notes with one snapshot, a transcript across two runs, two slides: the Python CLI's formats.
    fn legacy(dir: &Path, state: serde_json::Value) -> LectureFiles {
        let f = files(dir);
        std::fs::create_dir_all(f.state_dir()).unwrap();
        std::fs::write(&f.notes, format!("{TITLE}\n\n<!-- 10:00:12 -->\n## Intro\n- one, two\n")).unwrap();
        std::fs::write(&f.transcript, "--- started 10:00:00 ---\n[10:00:05] one\n[10:00:09] two\n--- resumed 10:20:00 ---\n[10:20:03] three\n").unwrap();
        png(&f.slides.join("slide_01_100007.png"));
        png(&f.slides.join("slide_02_102004.png"));
        std::fs::write(f.legacy_state(), state.to_string()).unwrap();
        f
    }

    /// Byte offset of the "--- resumed" line: where the CLI's transcript stood at its snapshot.
    const OFFSET: u64 = ("--- started 10:00:00 ---\n[10:00:05] one\n[10:00:09] two\n".len()) as u64;

    #[test]
    fn an_empty_folder_gets_notes_with_the_title_and_an_empty_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let f = files(dir.path());
        let (sc, report) = open(&f, TITLE, false).unwrap();
        assert_eq!(report.how, How::Created);
        assert_eq!(std::fs::read_to_string(&f.notes).unwrap(), format!("{TITLE}\n"));
        assert_eq!((sc.notes.segment_cursor, sc.notes.slide_index, sc.notes.revision), (0, 0, 1));
        assert_eq!(sc.lecture_date, Some(f.date));
        assert_eq!(Sidecar::load(&f.sidecar()).unwrap().unwrap(), sc);
    }

    #[test]
    fn a_legacy_folder_migrates_with_the_lines_after_its_offset_pending() {
        let dir = tempfile::tempdir().unwrap();
        let f = legacy(dir.path(), serde_json::json!({"transcript_offset": OFFSET, "slide_index": 1}));
        let transcript = std::fs::read(&f.transcript).unwrap();
        let (sc, report) = open(&f, TITLE, false).unwrap();
        assert_eq!(report.how, How::Migrated);
        let segs = segments::read(&f.segments()).unwrap();
        assert_eq!(
            segs.iter().map(|s| (s.id, s.said_at.format("%H:%M:%S").to_string(), s.text.as_str(), s.source)).collect::<Vec<_>>(),
            vec![
                (0, "10:00:05".to_string(), "one", SegmentSource::Imported),
                (1, "10:00:09".to_string(), "two", SegmentSource::Imported),
                (2, "10:20:03".to_string(), "three", SegmentSource::Imported),
            ]
        );
        assert_eq!((sc.notes.segment_cursor, sc.notes.slide_index), (2, 1));
        assert_eq!(sc.slides.iter().map(|s| (s.index, s.file.as_str())).collect::<Vec<_>>(), vec![(1, "slides/slide_01_100007.png"), (2, "slides/slide_02_102004.png")]);
        assert_eq!((report.pending_segments, report.pending_slides), (1, 1));
        assert_eq!(std::fs::read(&f.transcript).unwrap(), transcript, "the transcript is not rewritten");
        assert!(f.legacy_state().exists(), "the CLI's state stays as provenance");
        let (_, again) = open(&f, TITLE, false).unwrap();
        assert_eq!(again.how, How::Resumed, "migration is one-time");
    }

    #[test]
    fn an_older_noted_through_checkpoint_migrates_by_time() {
        let dir = tempfile::tempdir().unwrap();
        let f = legacy(dir.path(), serde_json::json!({"noted_through": "10:00:09"}));
        let (sc, _) = open(&f, TITLE, false).unwrap();
        assert_eq!((sc.notes.segment_cursor, sc.notes.slide_index), (1, 1), "lines from 10:00:09 on and slides from then on are pending");
    }

    #[test]
    fn a_legacy_half_written_snapshot_is_undone_before_migration() {
        let block = "\n<!-- 10:21:00 -->\n## Three\n- three\n";
        let commit = |before: usize| serde_json::json!({"transcript_offset": OFFSET, "slide_index": 1, "commit": {"before": before, "block": block, "transcript_offset": 200, "slide_index": 2}});
        // Torn: part of the block reached the notes.
        let dir = tempfile::tempdir().unwrap();
        let f = legacy(dir.path(), serde_json::json!({}));
        let before = std::fs::read_to_string(&f.notes).unwrap();
        std::fs::write(f.legacy_state(), commit(before.len()).to_string()).unwrap();
        std::fs::write(&f.notes, format!("{before}{}", &block[..12])).unwrap();
        let (sc, report) = open(&f, TITLE, false).unwrap();
        assert_eq!(report.legacy_commit, Some("removed a half-written snapshot; its material is queued again"));
        assert_eq!(std::fs::read_to_string(&f.notes).unwrap(), before);
        assert_eq!((sc.notes.segment_cursor, sc.notes.slide_index), (2, 1));
        // Complete: the whole block is there, so its cursors hold.
        let dir = tempfile::tempdir().unwrap();
        let f = legacy(dir.path(), serde_json::json!({}));
        let before = std::fs::read_to_string(&f.notes).unwrap();
        std::fs::write(f.legacy_state(), commit(before.len()).to_string()).unwrap();
        std::fs::write(&f.notes, format!("{before}{block}")).unwrap();
        let (sc, report) = open(&f, TITLE, false).unwrap();
        assert_eq!(report.legacy_commit, Some("the last snapshot was fully written"));
        assert_eq!((sc.notes.segment_cursor, sc.notes.slide_index), (3, 2), "offset 200 is past every line");
    }

    #[test]
    fn imported_lines_after_midnight_roll_to_the_next_day() {
        let dir = tempfile::tempdir().unwrap();
        let f = files(dir.path());
        std::fs::create_dir_all(f.state_dir()).unwrap();
        std::fs::write(&f.transcript, "[23:59:50] late\n[00:00:10] later\n").unwrap();
        std::fs::write(f.legacy_state(), serde_json::json!({"transcript_offset": 0, "slide_index": 0}).to_string()).unwrap();
        std::fs::write(&f.notes, format!("{TITLE}\n")).unwrap();
        open(&f, TITLE, false).unwrap();
        let segs = segments::read(&f.segments()).unwrap();
        assert_eq!(segs[0].said_at, Local.with_ymd_and_hms(2026, 9, 25, 23, 59, 50).unwrap());
        assert_eq!(segs[1].said_at, Local.with_ymd_and_hms(2026, 9, 26, 0, 0, 10).unwrap());
    }

    #[test]
    fn notes_without_any_state_are_rebuilt_from_the_last_marker() {
        let dir = tempfile::tempdir().unwrap();
        let f = files(dir.path());
        std::fs::write(&f.notes, format!("{TITLE}\n\n<!-- 10:05:00 -->\n- a\n\n<!-- 10:10:00 -->\n- b\n")).unwrap();
        std::fs::write(&f.transcript, "[10:04:00] a\n[10:09:59] b\n[10:10:30] c\n").unwrap();
        png(&f.slides.join("slide_01_100900.png"));
        png(&f.slides.join("slide_02_101100.png"));
        let (sc, report) = open(&f, TITLE, false).unwrap();
        assert_eq!(report.how, How::Rebuilt);
        assert_eq!((sc.notes.segment_cursor, sc.notes.slide_index), (2, 1));
    }

    #[test]
    fn a_corrupt_sidecar_stops_unless_a_rebuild_is_asked_and_is_then_kept() {
        let dir = tempfile::tempdir().unwrap();
        let f = files(dir.path());
        std::fs::create_dir_all(f.state_dir()).unwrap();
        std::fs::write(&f.notes, format!("{TITLE}\n\n<!-- 10:00:03 -->\n- one\n")).unwrap();
        let mut log = crate::session::segments::SegmentLog::open(dir.path(), &f.stem).unwrap();
        for (k, text) in ["one", "two"].iter().enumerate() {
            let s = crate::session::segments::NewSegment { recording_id: uuid::Uuid::nil(), start_sample: k as u64 * 64_000, end_sample: k as u64 * 64_000 + 8_000, text: text.to_string(), words: vec![], source: SegmentSource::Live };
            log.append(s, Local.with_ymd_and_hms(2026, 9, 25, 10, 0, 0).unwrap()).unwrap();
        }
        drop(log);
        std::fs::write(f.sidecar(), b"{\"version\":2,\"recor").unwrap();
        let err = open(&f, TITLE, false).unwrap_err();
        assert!(format!("{err:#}").contains("--rebuild"), "{err:#}");
        assert_eq!(std::fs::read(f.sidecar()).unwrap(), b"{\"version\":2,\"recor");
        let (sc, report) = open(&f, TITLE, true).unwrap();
        assert_eq!(report.how, How::Rebuilt);
        let kept = report.corrupt_kept.unwrap();
        assert_eq!(std::fs::read(&kept).unwrap(), b"{\"version\":2,\"recor");
        assert_eq!(segments::read(&f.segments()).unwrap().len(), 2, "the segment log is kept");
        assert_eq!(sc.notes.segment_cursor, 1, "said at 10:00:00, before the marker; the next at 10:00:04, after it");
    }

    #[test]
    fn a_transcript_or_slides_without_notes_are_all_pending() {
        let dir = tempfile::tempdir().unwrap();
        let f = files(dir.path());
        std::fs::write(&f.transcript, "[10:00:05] one\n").unwrap();
        png(&f.slides.join("slide_03_100007.png"));
        let (sc, report) = open(&f, TITLE, false).unwrap();
        assert_eq!(report.how, How::Created);
        assert_eq!((sc.notes.segment_cursor, sc.notes.slide_index, report.pending_segments, report.pending_slides), (0, 0, 1, 1));
    }

    #[test]
    fn resume_recovers_the_journal_first_then_keeps_an_edit_and_registers_new_slide_files() {
        let dir = tempfile::tempdir().unwrap();
        let f = files(dir.path());
        let (mut sc, _) = open(&f, TITLE, false).unwrap();
        crate::session::notesfile::commit_through(&f, &mut sc, "\n<!-- 10:05:00 -->\n- a\n", 0, 0, crate::session::notesfile::Step::Journal).unwrap();
        std::fs::OpenOptions::new().append(true).open(&f.notes).unwrap().write_all(b"\n<!-- 10:0").unwrap();
        let (_, report) = open(&f, TITLE, false).unwrap();
        assert_eq!((report.how, report.journal, report.external_edit), (How::Resumed, Recovered::Truncated, false));
        std::fs::OpenOptions::new().append(true).open(&f.notes).unwrap().write_all(b"- typed by hand\n").unwrap();
        png(&f.slides.join("slide_01_101500.png"));
        let (sc, report) = open(&f, TITLE, false).unwrap();
        assert!(report.external_edit);
        assert_eq!((report.slides_registered, sc.slides.len(), next_slide_index(&f, &sc).unwrap()), (1, 1, 2));
    }

    #[tokio::test]
    async fn another_days_transcript_gaps_are_recovered_before_today() {
        use crate::session::sidecar::{Gap, GapKind, RecState, RecordingEntry};
        use crate::stt::rest::{RecoverEvent, RecoveryLink};
        let dir = tempfile::tempdir().unwrap();
        let wav = dir.path().join("recordings/r.wav");
        std::fs::create_dir_all(wav.parent().unwrap()).unwrap();
        let spec = hound::WavSpec { channels: 1, sample_rate: 16_000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        let mut w = hound::WavWriter::create(&wav, spec).unwrap();
        for _ in 0..48_000 {
            w.write_sample(100i16).unwrap();
        }
        w.finalize().unwrap();
        let id = uuid::Uuid::new_v4();
        let anchor = Local.with_ymd_and_hms(2026, 9, 24, 10, 0, 0).unwrap();
        let mut yesterday = Sidecar::default();
        yesterday.recordings.push(RecordingEntry { id, file: "recordings/r.wav".into(), anchor, source_uid: "X".into(), input_rate: 48_000, samples: Some(48_000), state: RecState::Finalized });
        yesterday.gaps.push(Gap::new(id, 16_000, Some(48_000), GapKind::SttOffline));
        yesterday.save(&crate::session::sidecar::sidecar_path(dir.path(), "lecture_notes_20260924")).unwrap();
        let recovery = || -> Result<RecoveryLink> {
            let (jobs, mut rx) = tokio::sync::mpsc::unbounded_channel::<crate::stt::rest::RecoverJob>();
            let (tx, events) = tokio::sync::mpsc::channel(8);
            tokio::spawn(async move {
                while let Some(j) = rx.recv().await {
                    let _ = tx.send(RecoverEvent::Piece { recording_id: j.recording_id, gap_start: j.gap_start, start_sample: j.from, end_sample: j.end, text: "recovered a day later".into(), words: vec![] }).await;
                    let _ = tx.send(RecoverEvent::Done { recording_id: j.recording_id, gap_start: j.gap_start }).await;
                }
            });
            Ok(RecoveryLink { jobs, events })
        };
        let mut seen = Vec::new();
        let done = recover_other_days(dir.path(), "lecture_notes_20260925", recovery, None, &mut |s: &str| seen.push(s.to_string())).await.unwrap();
        assert_eq!(seen, vec!["lecture_notes_20260924"], "each day's recovery is announced before it runs");
        assert_eq!(done.iter().map(|(s, r)| (s.as_str(), r.unresolved)).collect::<Vec<_>>(), vec![("lecture_notes_20260924", 0)]);
        assert_eq!(std::fs::read_to_string(dir.path().join("lecture_transcript_20260924.txt")).unwrap(), "[10:00:01] recovered a day later\n");
        assert!(recover_other_days(dir.path(), "lecture_notes_20260925", recovery, None, &mut |_: &str| {}).await.unwrap().is_empty(), "nothing is left to recover");
    }
}
