//! Every path of a lecture (spec §8), named as the Python CLI names them.
use std::path::{Path, PathBuf};

use chrono::NaiveDate;

use crate::session::segments::segments_path;
use crate::session::sidecar::sidecar_path;

#[derive(Debug, Clone, PartialEq)]
pub struct LectureFiles {
    /// The lecture folder: `.live_notes/` and `recordings/` live here.
    pub dir: PathBuf,
    /// The notes file's stem: every state file is named after it.
    pub stem: String,
    /// The lecture day, fixed when its files were created.
    pub date: NaiveDate,
    pub notes: PathBuf,
    pub transcript: PathBuf,
    pub slides: PathBuf,
}

impl LectureFiles {
    pub fn standard(dir: &Path, date: NaiveDate) -> Self {
        Self::custom(dir, date, None, None, None)
    }

    /// The CLI's `--notes`, `--transcript` and `--slides-dir`, each defaulting to the standard name.
    pub fn custom(dir: &Path, date: NaiveDate, notes: Option<PathBuf>, transcript: Option<PathBuf>, slides: Option<PathBuf>) -> Self {
        let stamp = date.format("%Y%m%d");
        let notes = notes.unwrap_or_else(|| dir.join(format!("lecture_notes_{stamp}.md")));
        let stem = notes.file_stem().map_or_else(|| format!("lecture_notes_{stamp}"), |s| s.to_string_lossy().into_owned());
        Self {
            dir: dir.to_path_buf(),
            stem,
            date,
            notes,
            transcript: transcript.unwrap_or_else(|| dir.join(format!("lecture_transcript_{stamp}.txt"))),
            slides: slides.unwrap_or_else(|| dir.join("slides")),
        }
    }

    pub fn state_dir(&self) -> PathBuf {
        self.dir.join(".live_notes")
    }

    pub fn sidecar(&self) -> PathBuf {
        sidecar_path(&self.dir, &self.stem)
    }

    pub fn segments(&self) -> PathBuf {
        segments_path(&self.dir, &self.stem)
    }

    pub fn journal(&self) -> PathBuf {
        self.state_dir().join(format!("{}.journal.json", self.stem))
    }

    /// The Python CLI's state file.
    pub fn legacy_state(&self) -> PathBuf {
        self.state_dir().join(format!("{}.json", self.stem))
    }

    pub fn page_cache(&self) -> PathBuf {
        self.state_dir().join(format!("{}.page.json", self.stem))
    }

    pub fn notes_dir(&self) -> &Path {
        self.notes.parent().unwrap_or(&self.dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_standard_files_take_the_clis_names() {
        let f = LectureFiles::standard(Path::new("/l"), NaiveDate::from_ymd_opt(2026, 9, 25).unwrap());
        assert_eq!(f.stem, "lecture_notes_20260925");
        assert_eq!((f.notes.as_path(), f.transcript.as_path(), f.slides.as_path()), (Path::new("/l/lecture_notes_20260925.md"), Path::new("/l/lecture_transcript_20260925.txt"), Path::new("/l/slides")));
        assert_eq!(f.sidecar(), Path::new("/l/.live_notes/lecture_notes_20260925.v2.json"));
        assert_eq!(f.journal(), Path::new("/l/.live_notes/lecture_notes_20260925.journal.json"));
        assert_eq!(f.legacy_state(), Path::new("/l/.live_notes/lecture_notes_20260925.json"));
        assert_eq!(f.page_cache(), Path::new("/l/.live_notes/lecture_notes_20260925.page.json"));
        assert_eq!(f.segments(), Path::new("/l/.live_notes/lecture_notes_20260925.segments.jsonl"));
    }

    #[test]
    fn custom_notes_name_the_state_files_as_the_cli_does() {
        let f = LectureFiles::custom(Path::new("/l"), NaiveDate::from_ymd_opt(2026, 9, 25).unwrap(), Some("/l/week1.md".into()), Some("/l/t.txt".into()), Some("/shots".into()));
        assert_eq!(f.stem, "week1");
        assert_eq!(f.sidecar(), Path::new("/l/.live_notes/week1.v2.json"));
        assert_eq!((f.transcript.as_path(), f.slides.as_path()), (Path::new("/l/t.txt"), Path::new("/shots")));
    }
}
