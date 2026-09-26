//! Hydration (M7 plan §F): reading the canonical files back into an immutable result the reactor
//! merges. Read-only by construction — it never writes a file, repairs anything or sends a command.
//! The first read happens while the folder lock is still held and the terminal still cooked; a
//! folder whose notes and sidecar cannot be read as one revision is an ordinary error there, not a
//! reason to take the terminal over. Mid-session, only a discontinuity triggers a read (a segment
//! id beyond the next, a notes revision beyond the next, a polish), and a stale result never rolls
//! the view back — that rule is the projection's ([`crate::tui::state::View::merge`]).

use std::time::Duration;

use anyhow::{Context, Result};
use lecturelive_core::session::files::LectureFiles;
use lecturelive_core::session::notesfile::sha256_hex;
use lecturelive_core::session::segments::{self, Segment};
use lecturelive_core::session::sidecar::{Gap, Sidecar, SlideEntry};

/// The initial attempt and two retries, 50 ms apart (plan §B 31, the desktop's `read_document` rule):
/// long enough for a commit to finish writing between the two reads, never a blocking wait.
const RETRIES: u32 = 2;
const BETWEEN: Duration = Duration::from_millis(50);

/// The notes as one hydration found them: a document and the revision it exactly is, or a pair that
/// could not be read together — a commit was writing between the reads, or the notes were changed by
/// hand. An incoherent pair is never guessed at.
#[derive(Debug)]
pub(crate) enum NotesSnapshot {
    /// The document whose length and SHA-256 are the revision the sidecar names.
    At { revision: u64, document: String },
    Incoherent,
}

/// One coherent read of the canonical files (plan §F): everything the projection rebuilds from.
#[derive(Debug)]
pub(crate) struct Hydration {
    /// The segment log, in commit order: the transcript's source of truth.
    pub(crate) segments: Vec<Segment>,
    /// The notes document and the revision it is.
    pub(crate) notes: NotesSnapshot,
    /// The registered slides, in registration order.
    pub(crate) slides: Vec<SlideEntry>,
    /// The sidecar's transcript gaps, resolved or not: the projection reconciles its waiting set by
    /// each gap's identity, so a recovery whose notification was missed still clears (plan §F).
    /// Audio gaps are not here: nothing recovers them.
    pub(crate) gaps: Vec<Gap>,
}

impl Hydration {
    /// This hydration if its notes pair cohered: what the first read needs, because no projection
    /// exists yet whose state could be kept instead.
    pub(crate) fn coherent(self) -> Option<Hydration> {
        matches!(self.notes, NotesSnapshot::At { .. }).then_some(self)
    }

    /// No canonical state at all: a folder before anything was recorded.
    #[cfg(test)]
    pub(crate) fn empty() -> Hydration {
        Hydration { segments: Vec::new(), notes: NotesSnapshot::At { revision: 0, document: String::new() }, slides: Vec::new(), gaps: Vec::new() }
    }
}

/// Reads the canonical files — the sidecar, the segment log, and the notes checked against the
/// sidecar's fingerprint — retrying while a commit may be writing between the two reads. The file
/// work runs on a blocking thread; the reactor only ever sees the result.
pub(crate) async fn read(files: &LectureFiles) -> Result<Hydration> {
    let mut tries = 0;
    loop {
        let f = files.clone();
        let h = tokio::task::spawn_blocking(move || read_once(&f)).await??;
        if matches!(h.notes, NotesSnapshot::At { .. }) || tries >= RETRIES {
            return Ok(h);
        }
        tries += 1;
        tokio::time::sleep(BETWEEN).await;
    }
}

/// One read of every canonical file. The notes are taken only when they are the revision the
/// sidecar names (spec §3.6, as the desktop's `read_document` does): without the check, a commit
/// landing between the two reads would pair a document with the wrong revision.
fn read_once(files: &LectureFiles) -> Result<Hydration> {
    let sc = Sidecar::load(&files.sidecar())?;
    let segments = segments::read(&files.segments())?;
    let notes = match &sc {
        // No sidecar is no state: whatever the notes hold is revision 0's document, shown as it is. A
        // missing notes file is an empty document; one that exists but cannot be read is an error,
        // never an empty document.
        None => match std::fs::read(&files.notes) {
            Ok(bytes) => NotesSnapshot::At { revision: 0, document: String::from_utf8_lossy(&bytes).into_owned() },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => NotesSnapshot::At { revision: 0, document: String::new() },
            Err(e) => return Err(e).with_context(|| format!("read {}", files.notes.display())),
        },
        Some(sc) => match std::fs::read(&files.notes) {
            Ok(bytes) if bytes.len() as u64 == sc.notes.len && sha256_hex(&bytes) == sc.notes.sha256 => {
                NotesSnapshot::At { revision: sc.notes.revision, document: String::from_utf8_lossy(&bytes).into_owned() }
            }
            Ok(_) => NotesSnapshot::Incoherent,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => NotesSnapshot::Incoherent,
            Err(e) => return Err(e).with_context(|| format!("read {}", files.notes.display())),
        },
    };
    let gaps = sc.as_ref().map_or_else(Vec::new, |s| s.gaps.iter().filter(|g| g.kind.is_transcript()).cloned().collect());
    let slides = sc.map_or_else(Vec::new, |mut s| std::mem::take(&mut s.slides));
    Ok(Hydration { segments, notes, slides, gaps })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDate, TimeZone};
    use lecturelive_core::session::segments::{NewSegment, SegmentLog, SegmentSource};
    use lecturelive_core::session::sidecar::{Gap, GapKind, SlideEntry as Entry};

    fn anchor() -> chrono::DateTime<chrono::Local> {
        chrono::Local.with_ymd_and_hms(2026, 9, 26, 10, 0, 0).unwrap()
    }

    fn folder() -> (tempfile::TempDir, LectureFiles) {
        let dir = tempfile::tempdir().unwrap();
        let files = LectureFiles::standard(dir.path(), NaiveDate::from_ymd_opt(2026, 9, 26).unwrap());
        std::fs::create_dir_all(files.state_dir()).unwrap();
        (dir, files)
    }

    /// The notes at exactly the sidecar's fingerprint, and the sidecar saved: the state a coherent
    /// commit leaves behind.
    fn notes_at(files: &LectureFiles, sc: &mut Sidecar, text: &str) {
        std::fs::write(&files.notes, text).unwrap();
        sc.notes.len = text.len() as u64;
        sc.notes.sha256 = sha256_hex(text.as_bytes());
    }

    fn line(log: &mut SegmentLog, id: u64, text: &str) {
        log.append(NewSegment { recording_id: Default::default(), start_sample: id * 16_000, end_sample: (id + 1) * 16_000, text: text.into(), words: Vec::new(), source: SegmentSource::Live }, anchor()).unwrap();
    }

    /// Plan §J's hydration tests 1–5 in one folder: a log, slides, one transcript gap waiting (a
    /// resolved transcript gap and an audio gap do not wait), and notes at the sidecar's revision.
    #[tokio::test]
    async fn coherent_initial_hydration() {
        let (dir, files) = folder();
        let mut log = SegmentLog::open(dir.path(), &files.stem).unwrap();
        for id in 0..3u64 {
            line(&mut log, id, &format!("line {id}"));
        }
        drop(log);
        let mut sc = Sidecar::default();
        sc.gaps.push(Gap::new(Default::default(), 0, Some(16_000), GapKind::SttOffline));
        sc.gaps.push(Gap::new(Default::default(), 32_000, Some(48_000), GapKind::RecorderOverflow));
        let mut resolved = Gap::new(Default::default(), 64_000, Some(80_000), GapKind::SttInterrupted);
        resolved.resolved = true;
        sc.gaps.push(resolved);
        sc.slides.push(Entry { index: 1, file: "slides/slide_01_100512.png".into(), shown_at: anchor(), auto: true, uncertain: false });
        sc.notes.revision = 2;
        notes_at(&files, &mut sc, "# Title\n\n<!-- 10:05:00 -->\n## A\n");
        sc.save(&files.sidecar()).unwrap();

        let h = read(&files).await.unwrap().coherent().expect("the folder is coherent");
        assert_eq!(h.segments.iter().map(|s| (s.id, s.text.as_str())).collect::<Vec<_>>(), vec![(0, "line 0"), (1, "line 1"), (2, "line 2")], "the log is the transcript");
        assert_eq!(h.gaps.iter().map(|g| (g.start_sample, g.resolved)).collect::<Vec<_>>(), vec![(0, false), (64_000, true)], "the transcript gaps, waiting and resolved; the audio gap is not one");
        assert_eq!(h.slides.iter().map(|s| (s.index, s.file.as_str(), s.auto)).collect::<Vec<_>>(), vec![(1, "slides/slide_01_100512.png", true)], "the registered slides");
        match h.notes {
            NotesSnapshot::At { revision, document } => {
                assert_eq!(revision, 2);
                assert!(document.starts_with("# Title") && document.ends_with("## A\n"), "{document}");
            }
            NotesSnapshot::Incoherent => panic!("the pair was written coherently"),
        }
    }

    /// A folder with no state at all (before `prepare` would have made one): empty, and coherent.
    #[tokio::test]
    async fn a_folder_with_no_state_hydrates_empty() {
        let (_dir, files) = folder();
        let h = read(&files).await.unwrap().coherent().unwrap();
        assert!(h.segments.is_empty() && h.slides.is_empty() && h.gaps.is_empty());
        assert!(matches!(h.notes, NotesSnapshot::At { revision: 0, ref document } if document.is_empty()));
    }

    /// Plan §J's hydration test 9, the file's half: after a jump, the re-read brings the revision
    /// the notes file actually is — a polish's whole replaced document, not a delta.
    #[tokio::test]
    async fn a_revision_jump_reloads_the_canonical_document() {
        let (_dir, files) = folder();
        let mut sc = Sidecar::default();
        sc.notes.revision = 5;
        notes_at(&files, &mut sc, "# polished\n\n- the whole document\n");
        sc.save(&files.sidecar()).unwrap();
        let h = read(&files).await.unwrap();
        assert!(matches!(h.notes, NotesSnapshot::At { revision: 5, ref document } if document.starts_with("# polished")));
    }

    /// Plan §B 31: the notes are read only as the sidecar's revision. A change between the two reads
    /// is retried twice at 50 ms; a pair that still does not cohere is reported as incoherent —
    /// never guessed at — while the rest of the folder still hydrates, and an initial hydration
    /// refuses it rather than inventing canonical state.
    #[tokio::test]
    async fn an_incoherent_notes_pair_is_retried_then_reported() {
        let (dir, files) = folder();
        let mut log = SegmentLog::open(dir.path(), &files.stem).unwrap();
        line(&mut log, 0, "one");
        drop(log);
        let mut sc = Sidecar::default();
        sc.notes.revision = 2;
        notes_at(&files, &mut sc, "# as it was\n");
        sc.save(&files.sidecar()).unwrap();
        // the notes grew after the sidecar named them, as a commit between the two reads would leave them
        std::fs::write(&files.notes, "# as it was\n\n<!-- 10:05:00 -->\n## more\n").unwrap();
        let started = std::time::Instant::now();
        let h = read(&files).await.unwrap();
        assert!(started.elapsed() >= BETWEEN + BETWEEN, "the initial attempt and two retries, 50 ms apart");
        assert!(matches!(h.notes, NotesSnapshot::Incoherent));
        assert_eq!(h.segments.len(), 1, "the log still hydrates");
        assert!(h.coherent().is_none(), "no revision is invented for a first read");
    }

    /// A sidecar naming notes that do not exist is incoherent too: nothing is shown as canonical
    /// that cannot be read.
    #[tokio::test]
    async fn a_sidecar_without_its_notes_is_incoherent() {
        let (_dir, files) = folder();
        let mut sc = Sidecar::default();
        sc.notes.revision = 1;
        sc.notes.len = 4;
        sc.notes.sha256 = sha256_hex(b"# T\n");
        sc.save(&files.sidecar()).unwrap();
        assert!(matches!(read(&files).await.unwrap().notes, NotesSnapshot::Incoherent));
    }

    /// Canonical files that exist but cannot be read are an error, never an empty lecture: notes
    /// without a sidecar that cannot be read, a corrupt sidecar, a corrupt segment log. Notes that
    /// simply are not there yet are an empty document.
    #[tokio::test]
    async fn an_unreadable_canonical_file_is_an_error_not_empty_state() {
        use std::os::unix::fs::PermissionsExt;
        let (_dir, files) = folder();
        std::fs::write(&files.notes, "# private\n").unwrap();
        std::fs::set_permissions(&files.notes, std::fs::Permissions::from_mode(0o000)).unwrap();
        let unreadable = std::fs::read(&files.notes).is_err(); // a superuser reads it anyway
        let r = read(&files).await;
        std::fs::set_permissions(&files.notes, std::fs::Permissions::from_mode(0o644)).unwrap();
        if unreadable {
            assert!(r.is_err(), "an unreadable notes file is not an empty document");
        }
        std::fs::remove_file(&files.notes).unwrap();
        assert!(matches!(read(&files).await.unwrap().notes, NotesSnapshot::At { revision: 0, ref document } if document.is_empty()), "absent notes are an empty document");
        std::fs::write(files.sidecar(), "{ not json").unwrap();
        assert!(read(&files).await.is_err(), "a corrupt sidecar is an error");
        std::fs::remove_file(files.sidecar()).unwrap();
        std::fs::write(files.segments(), "not json\n").unwrap();
        assert!(read(&files).await.is_err(), "a corrupt segment log is an error");
    }
}
