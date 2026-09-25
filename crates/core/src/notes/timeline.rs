//! What a snapshot sends (spec §6.1): the batch after the committed cursors, and its timeline of
//! speech and slide markers in the Python CLI's line format.
use std::ops::Range;
use std::path::{Component, Path};

use anyhow::Result;
use chrono::{DateTime, Local};

use crate::audio::recorder::SAMPLE_RATE;
use crate::session::segments::{self, Segment};
use crate::session::sidecar::{wall_time_at, Sidecar, SlideEntry};

pub fn hms(t: DateTime<Local>) -> String {
    t.format("%H:%M:%S").to_string()
}

/// Python's `os.path.relpath(path, start)` for absolute paths.
pub fn relpath(path: &Path, start: &Path) -> String {
    let p: Vec<Component> = path.components().collect();
    let s: Vec<Component> = start.components().collect();
    let common = p.iter().zip(&s).take_while(|(a, b)| a == b).count();
    let mut parts: Vec<String> = vec!["..".to_string(); s.len() - common];
    parts.extend(p[common..].iter().map(|c| c.as_os_str().to_string_lossy().into_owned()));
    if parts.is_empty() {
        ".".into()
    } else {
        parts.join("/")
    }
}

/// `![Slide N](path)`, the path relative to the notes file's folder, as the Python CLI writes it.
pub fn embed_line(slide: &SlideEntry, lecture_dir: &Path, notes_dir: &Path) -> String {
    format!("![Slide {}]({})", slide.index, relpath(&lecture_dir.join(&slide.file), notes_dir))
}

#[derive(Debug, Clone, PartialEq)]
pub struct Batch {
    /// Segment-log positions [from, to): the cursor moves to `to` when the batch commits.
    pub positions: Range<u64>,
    pub segments: Vec<Segment>,
    /// Registered slides after the committed index, by index.
    pub slides: Vec<SlideEntry>,
    pub hint: String,
}

impl Batch {
    /// The material after the sidecar's cursors, through log position `upto` (a cutoff's count, spec §5.3).
    pub fn take(log: &Path, sc: &Sidecar, upto: u64, hint: &str) -> Result<Self> {
        let from = sc.notes.segment_cursor;
        let upto = upto.max(from);
        let segments: Vec<Segment> = segments::read(log)?.into_iter().filter(|s| (from..upto).contains(&s.id)).collect();
        let mut slides: Vec<SlideEntry> = sc.slides.iter().filter(|s| s.index > sc.notes.slide_index).cloned().collect();
        slides.sort_by_key(|s| s.index);
        Ok(Self { positions: from..upto, segments, slides, hint: hint.to_string() })
    }

    pub fn is_empty(&self) -> bool {
        self.segments.is_empty() && self.slides.is_empty()
    }

    /// The slide cursor after this batch commits.
    pub fn slide_to(&self, current: u32) -> u32 {
        self.slides.iter().map(|s| s.index).max().unwrap_or(current).max(current)
    }

    /// Spoken words in the batch (the snapshot's report).
    pub fn words(&self) -> usize {
        self.segments.iter().map(|s| s.text.split_whitespace().count()).sum()
    }
}

struct Line {
    at: DateTime<Local>,
    slide: bool,
    order: (u64, usize),
    text: String,
}

/// The batch's timeline (spec §6.1): speech and slide markers by full time, a slide first at equal
/// times. A streamed segment that spans a slide's first-shown time is split there by its word times,
/// for the prompt only.
pub fn timeline(segments: &[Segment], slides: &[SlideEntry], lecture_dir: &Path, notes_dir: &Path) -> String {
    let mut lines: Vec<Line> = slides
        .iter()
        .map(|s| Line { at: s.shown_at, slide: true, order: (s.index as u64, 0), text: format!("[{}] >>> Slide {} shown (embed: {})", hms(s.shown_at), s.index, embed_line(s, lecture_dir, notes_dir)) })
        .collect();
    for seg in segments {
        for (k, (at, text)) in split_at_slides(seg, slides).into_iter().enumerate() {
            lines.push(Line { at, slide: false, order: (seg.id, k), text: format!("[{}] {text}", hms(at)) });
        }
    }
    lines.sort_by(|a, b| a.at.cmp(&b.at).then(b.slide.cmp(&a.slide)).then(a.order.cmp(&b.order)));
    lines.into_iter().map(|l| l.text).collect::<Vec<_>>().join("\n")
}

fn split_at_slides(seg: &Segment, slides: &[SlideEntry]) -> Vec<(DateTime<Local>, String)> {
    let whole = vec![(seg.said_at, seg.text.clone())];
    if seg.words.len() < 2 {
        return whole;
    }
    // The recording's anchor, exactly: `start` is anchor + start_sample in whole microseconds (§3.3).
    let anchor = seg.start - chrono::Duration::microseconds((seg.start_sample * 1_000_000 / SAMPLE_RATE as u64) as i64);
    let times: Vec<DateTime<Local>> = seg.words.iter().map(|w| wall_time_at(anchor, w.start_sample)).collect();
    let mut cuts: Vec<usize> = slides.iter().filter_map(|s| (1..times.len()).find(|&i| times[i - 1] < s.shown_at && s.shown_at <= times[i])).collect();
    if cuts.is_empty() {
        return whole;
    }
    cuts.sort_unstable();
    cuts.dedup();
    let tokens: Vec<&str> = seg.text.split_whitespace().collect();
    let words: Vec<&str> = if tokens.len() == seg.words.len() { tokens } else { seg.words.iter().map(|w| w.text.as_str()).collect() };
    let mut bounds = vec![0];
    bounds.extend(cuts);
    bounds.push(words.len());
    bounds.windows(2).map(|b| (times[b[0]], words[b[0]..b[1]].join(" "))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::segments::{NewSegment, SegmentLog, SegmentSource};
    use crate::session::sidecar::wall_time_at;
    use crate::stt::transcript::Word;
    use chrono::{Duration, TimeZone};
    use std::path::Path;
    use uuid::Uuid;

    fn anchor() -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 9, 25, 10, 0, 0).unwrap()
    }

    /// A segment of a recording anchored at 10:00:00, its words starting at the given seconds.
    fn seg(id: u64, text: &str, words: &[(&str, u64)], source: SegmentSource) -> Segment {
        let words: Vec<Word> = words.iter().map(|&(t, s)| Word { text: t.into(), start_sample: s * 16_000, end_sample: s * 16_000 + 8_000 }).collect();
        let (start, end) = (words.first().map_or(0, |w| w.start_sample), words.last().map_or(16_000, |w| w.end_sample));
        Segment { id, recording_id: Uuid::nil(), start_sample: start, end_sample: end, said_at: wall_time_at(anchor(), start), start: wall_time_at(anchor(), start), end: wall_time_at(anchor(), end), text: text.into(), words, source }
    }

    /// An imported line: no words, said at `at`.
    fn line(id: u64, text: &str, at: DateTime<Local>) -> Segment {
        Segment { id, recording_id: Uuid::nil(), start_sample: 0, end_sample: 0, said_at: at, start: at, end: at, text: text.into(), words: vec![], source: SegmentSource::Imported }
    }

    fn slide(index: u32, at: DateTime<Local>) -> SlideEntry {
        SlideEntry { index, file: format!("slides/slide_{index:02}_{}.png", at.format("%H%M%S")), shown_at: at }
    }

    const DIR: &str = "/lecture";

    fn tl(segments: &[Segment], slides: &[SlideEntry]) -> String {
        timeline(segments, slides, Path::new(DIR), Path::new(DIR))
    }

    #[test]
    fn speech_and_slides_are_ordered_by_full_time_in_the_clis_format() {
        let segments = [
            seg(0, "Gradient descent.", &[("Gradient", 1), ("descent", 1)], SegmentSource::Live),
            seg(1, "Momentum.", &[("Momentum", 5)], SegmentSource::Live),
            seg(2, "Recovered words.", &[("Recovered", 3), ("words", 3)], SegmentSource::Recovered), // logged last, said earlier
        ];
        assert_eq!(
            tl(&segments, &[slide(1, anchor() + Duration::seconds(4))]),
            "[10:00:01] Gradient descent.\n[10:00:03] Recovered words.\n[10:00:04] >>> Slide 1 shown (embed: ![Slide 1](slides/slide_01_100004.png))\n[10:00:05] Momentum."
        );
    }

    #[test]
    fn a_slide_comes_before_speech_at_the_same_time() {
        let at = anchor() + Duration::seconds(4);
        assert_eq!(tl(&[line(0, "Said then.", at)], &[slide(2, at)]), "[10:00:04] >>> Slide 2 shown (embed: ![Slide 2](slides/slide_02_100004.png))\n[10:00:04] Said then.");
    }

    #[test]
    fn a_segment_spanning_a_slide_is_split_there_by_its_word_times() {
        let s = seg(0, "One two three four.", &[("One", 0), ("two", 1), ("three", 2), ("four", 3)], SegmentSource::Live);
        let shown = anchor() + Duration::milliseconds(2_500);
        assert_eq!(tl(&[s], &[slide(1, shown)]), "[10:00:00] One two three\n[10:00:02] >>> Slide 1 shown (embed: ![Slide 1](slides/slide_01_100002.png))\n[10:00:03] four.");
    }

    #[test]
    fn a_split_uses_the_word_texts_when_the_text_does_not_align_with_them() {
        let s = seg(0, "One-two three", &[("One", 0), ("two", 1), ("three", 2)], SegmentSource::Live);
        assert_eq!(tl(&[s], &[slide(1, anchor() + Duration::milliseconds(1_500))]).lines().collect::<Vec<_>>()[0], "[10:00:00] One two");
    }

    #[test]
    fn midnight_orders_by_full_time_not_by_the_clock_string() {
        let day2 = Local.with_ymd_and_hms(2026, 9, 26, 0, 0, 3).unwrap();
        let segments = [line(0, "Before midnight.", Local.with_ymd_and_hms(2026, 9, 25, 23, 59, 58).unwrap()), line(1, "After it.", day2 + Duration::seconds(2))];
        let out = tl(&segments, &[slide(4, day2)]);
        assert_eq!(out, "[23:59:58] Before midnight.\n[00:00:03] >>> Slide 4 shown (embed: ![Slide 4](slides/slide_04_000003.png))\n[00:00:05] After it.");
    }

    #[test]
    fn embeds_are_relative_to_the_notes_folder_as_the_cli_writes_them() {
        let s = slide(3, anchor());
        assert_eq!(embed_line(&s, Path::new("/l"), Path::new("/l")), "![Slide 3](slides/slide_03_100000.png)");
        assert_eq!(embed_line(&s, Path::new("/l"), Path::new("/l/notes")), "![Slide 3](../slides/slide_03_100000.png)");
        let elsewhere = SlideEntry { file: "/shots/a.png".into(), ..s };
        assert_eq!(embed_line(&elsewhere, Path::new("/l"), Path::new("/l")), "![Slide 3](../shots/a.png)");
    }

    #[test]
    fn a_batch_is_the_log_after_the_cursor_through_the_cutoff_and_the_slides_after_the_index() {
        let dir = tempfile::tempdir().unwrap();
        let mut log = SegmentLog::open(dir.path(), "lecture_notes_20260925").unwrap();
        for k in 0..5u64 {
            log.append(NewSegment { recording_id: Uuid::nil(), start_sample: k * 16_000, end_sample: k * 16_000 + 8_000, text: format!("line {k} words"), words: vec![], source: SegmentSource::Live }, anchor()).unwrap();
        }
        let mut sc = Sidecar::default();
        sc.notes.segment_cursor = 2;
        sc.notes.slide_index = 1;
        sc.slides = (1..=3).map(|i| slide(i, anchor())).collect();
        let path = crate::session::segments::segments_path(dir.path(), "lecture_notes_20260925");
        let b = Batch::take(&path, &sc, 4, "momentum").unwrap();
        assert_eq!(b.positions, 2..4);
        assert_eq!(b.segments.iter().map(|s| s.id).collect::<Vec<_>>(), vec![2, 3]);
        assert_eq!(b.slides.iter().map(|s| s.index).collect::<Vec<_>>(), vec![2, 3]);
        assert_eq!((b.slide_to(1), b.words(), b.hint.as_str()), (3, 6, "momentum"));
        sc.notes.segment_cursor = 4;
        sc.notes.slide_index = 3;
        let empty = Batch::take(&path, &sc, 4, "").unwrap();
        assert!(empty.is_empty());
        assert_eq!(empty.slide_to(3), 3);
    }
}
