//! The one registration path for slides (spec §7.3, §8): auto and manual captures, screenshots and
//! dropped images are all moved into `slides/` under the CLI's name and recorded inside the sidecar's
//! writer, so indexes never collide and no file is registered twice.
use std::path::{Path, PathBuf};
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Local};

use crate::session::coordinator::Store;
use crate::session::files::LectureFiles;
use crate::session::folder::{next_slide_index, relative};
use crate::session::sidecar::SlideEntry;

const IMAGE_EXTENSIONS: [&str; 3] = ["png", "jpg", "jpeg"];
const SLIDE_MAX_PX: u32 = 1600;

/// How a slide came to be: when it was first on screen, and whether the detector took it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SlideMeta {
    pub shown_at: DateTime<Local>,
    pub auto: bool,
    pub uncertain: bool,
}

impl SlideMeta {
    /// Taken by the person: the button, the shortcut, a screenshot or a dropped image.
    pub fn manual(shown_at: DateTime<Local>) -> Self {
        Self { shown_at, auto: false, uncertain: false }
    }
}

pub fn is_image(p: &Path) -> bool {
    p.extension().and_then(|e| e.to_str()).is_some_and(|e| IMAGE_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

/// A file's modification time: an imported image's capture time (spec §8).
pub fn file_time(p: &Path) -> Result<DateTime<Local>> {
    Ok(std::fs::metadata(p).and_then(|m| m.modified()).with_context(|| format!("read the time of {}", p.display()))?.into())
}

/// Moves a file, copying when it crosses file systems.
fn move_file(from: &Path, to: &Path) -> Result<()> {
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    std::fs::copy(from, to).with_context(|| format!("copy {} to {}", from.display(), to.display()))?;
    std::fs::remove_file(from).with_context(|| format!("remove {}", from.display()))
}

/// The CLI's `shrink_if_large`: `sips -Z` scales up as well as down, so it runs only when there is something to shrink.
fn shrink_if_large(path: &Path) {
    let Ok(out) = std::process::Command::new("sips").args(["-g", "pixelWidth", "-g", "pixelHeight"]).arg(path).output() else { return };
    let dims: Vec<u32> = String::from_utf8_lossy(&out.stdout).lines().filter(|l| l.contains("pixel")).filter_map(|l| l.split(':').nth(1)?.trim().parse().ok()).collect();
    if dims.iter().any(|&d| d > SLIDE_MAX_PX) {
        let _ = std::process::Command::new("sips").args(["-Z", &SLIDE_MAX_PX.to_string()]).arg(path).output();
    }
}

/// Registers `path` (a file in `slides/`, a temp file or a screenshot) as the next slide: moved into
/// `slides/` as `slide_NN_HHMMSS.<ext>`, shrunk to 1600 px, and recorded, all inside the sidecar's
/// writer. None when the file is already registered.
pub async fn register(files: &LectureFiles, store: &Store, path: &Path, meta: SlideMeta) -> Result<Option<SlideEntry>> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = Path::new(name.trim_end_matches(".tmp")).extension().and_then(|e| e.to_str()).unwrap_or("png").to_ascii_lowercase();
    let (files, p) = (files.clone(), path.to_path_buf());
    store
        .update(move |sc| {
            if sc.slides.iter().any(|s| files.dir.join(&s.file) == p) {
                return Ok(None);
            }
            let index = next_slide_index(&files, sc)?;
            let dest = files.slides.join(format!("slide_{index:02}_{}.{ext}", meta.shown_at.format("%H%M%S")));
            if p != dest {
                move_file(&p, &dest)?;
            }
            shrink_if_large(&dest);
            let entry = SlideEntry { index, file: relative(&files, &dest), shown_at: meta.shown_at, auto: meta.auto, uncertain: meta.uncertain };
            sc.slides.push(entry.clone());
            Ok(Some(entry))
        })
        .await
}

/// Where a removed slide's file goes inside `slides/`: out of the folder's slide files, so opening the lecture
/// again does not register it back, and kept for undo.
pub const REMOVED_DIR: &str = "removed";

/// Takes a slide that no snapshot has used out of the lecture (spec §7.5), inside the sidecar's writer: its record
/// goes and its file moves to `slides/removed/`. A slide the notes already hold stays, because the notes embed
/// its file. `snapshotting` is set while a snapshot or polish runs: its batch may already hold the slide, and the
/// check is made here, where nothing else writes, so a batch taken a moment earlier is seen too.
pub async fn remove(files: &LectureFiles, store: &Store, index: u32, snapshotting: Arc<AtomicBool>) -> Result<SlideEntry> {
    let files = files.clone();
    store
        .update(move |sc| {
            let Some(at) = sc.slides.iter().position(|s| s.index == index) else { bail!("Slide {index} is not in this lecture") };
            if index <= sc.notes.slide_index {
                bail!("Slide {index} is already in the notes");
            }
            if snapshotting.load(Ordering::SeqCst) {
                bail!("A snapshot is running and may be using slide {index}: remove it once the snapshot is done");
            }
            let from = files.dir.join(&sc.slides[at].file);
            if let Some(name) = from.file_name().filter(|_| from.exists()) {
                // The record's path is whatever the sidecar says, and only a file inside the slides folder is the app's
                // to move: compared as real paths, so a `..`, an absolute path or a link pointing out is refused.
                let inside = from.canonicalize().ok().zip(files.slides.canonicalize().ok()).is_some_and(|(file, folder)| file.starts_with(folder));
                if !inside {
                    bail!("Slide {index}'s file is not in the slides folder, so it is left where it is");
                }
                let dir = files.slides.join(REMOVED_DIR);
                std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
                if dir.canonicalize()?.parent() != Some(files.slides.canonicalize()?.as_path()) {
                    bail!("The removed folder is not inside the slides folder");
                }
                let dest = dir.join(name);
                if dest.exists() { bail!("Slide {index}'s removed file already exists, so neither image was overwritten"); }
                move_file(&from, &dest)?;
                sc.removed_slides.push(sc.slides[at].clone());
            }
            Ok(sc.slides.remove(at))
        })
        .await
}

/// Restores the most recent removal inside the same writer as capture and snapshots. If a snapshot
/// has passed its old number, give it a new pending number so the next snapshot can include it.
pub async fn undo_remove(files: &LectureFiles, store: &Store, snapshotting: Arc<AtomicBool>) -> Result<SlideEntry> {
    let files = files.clone();
    store.update(move |sc| {
        let mut slide = sc.removed_slides.last().cloned().context("There is no removed slide to restore")?;
        if snapshotting.load(Ordering::SeqCst) { bail!("A snapshot is running: undo the removal once it is done"); }
        let original = files.dir.join(&slide.file);
        let name = original.file_name().context("The removed slide has no filename")?;
        let folder = files.slides.canonicalize()?;
        if original.parent().and_then(|p| p.canonicalize().ok()).as_ref() != Some(&folder) {
            bail!("The removed slide's file is not in the slides folder");
        }
        let from = files.slides.join(REMOVED_DIR).join(name);
        let archived = if !from.exists() { crate::notes::archive::Archive::load(files.notes_dir()).uri(&from).and_then(crate::notes::archive::image) } else { None };
        let removed = files.slides.join(REMOVED_DIR).canonicalize();
        let unsafe_file = std::fs::symlink_metadata(&from).is_ok_and(|m| !m.file_type().is_file());
        if unsafe_file || removed.as_ref().is_ok_and(|p| p.parent() != Some(folder.as_path())) || (archived.is_none() && !from.is_file()) {
            bail!("The removed slide's file is missing or is not a regular image in slides/removed");
        }
        if slide.index <= sc.notes.slide_index || sc.slides.iter().any(|s| s.index == slide.index) {
            slide.index = next_slide_index(&files, sc)?;
            let ext = original.extension().and_then(|s| s.to_str()).unwrap_or("png");
            slide.file = relative(&files, &files.slides.join(format!("slide_{:02}_{}.{ext}", slide.index, slide.shown_at.format("%H%M%S"))));
        }
        if let Some(image) = &archived {
            let ext = if image.mime == "image/jpeg" { "jpg" } else { "png" };
            slide.file = Path::new(&slide.file).with_extension(ext).to_string_lossy().into_owned();
        }
        let dest = files.dir.join(&slide.file);
        if dest.exists() { bail!("The restored slide's destination already exists, so neither image was overwritten"); }
        if let Some(image) = archived {
            use base64::Engine;
            let bytes = base64::engine::general_purpose::STANDARD.decode(&image.base64)?;
            let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&dest)?;
            if let Err(error) = file.write_all(&bytes).and_then(|_| file.sync_all()) {
                let _ = std::fs::remove_file(&dest);
                return Err(error.into());
            }
        } else { move_file(&from, &dest)?; }
        sc.removed_slides.pop();
        sc.slides.push(slide.clone());
        sc.slides.sort_by_key(|s| s.index);
        Ok(slide)
    }).await
}

/// Images dropped on the app (spec §7.3): each is copied to a temp name in `slides/`, so the original
/// stays where the person had it, then registered with its own file time.
pub async fn import(files: &LectureFiles, store: &Store, paths: &[PathBuf]) -> Vec<(PathBuf, Result<SlideEntry, String>)> {
    let mut out = Vec::new();
    for p in paths {
        out.push((p.clone(), import_one(files, store, p).await));
    }
    out
}

async fn import_one(files: &LectureFiles, store: &Store, p: &Path) -> Result<SlideEntry, String> {
    let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string());
    if !is_image(p) || !p.is_file() {
        return Err(format!("{name} is not a PNG or JPEG image"));
    }
    let shown_at = file_time(p).map_err(|e| format!("{e:#}"))?;
    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("png").to_ascii_lowercase();
    let tmp = files.slides.join(format!(".import-{}.{ext}.tmp", uuid::Uuid::new_v4()));
    if let Err(e) = std::fs::copy(p, &tmp) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("copy {name}: {e}"));
    }
    match register(files, store, &tmp, SlideMeta::manual(shown_at)).await {
        Ok(Some(s)) => Ok(s),
        Ok(None) => Err(format!("{name} is already a slide")),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(format!("{name}: {e:#}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::coordinator::Store;
    use crate::session::files::LectureFiles;
    use crate::session::sidecar::Sidecar;
    use chrono::{Local, TimeZone};
    use std::path::Path;

    fn setup() -> (tempfile::TempDir, LectureFiles, Store) {
        let dir = tempfile::tempdir().unwrap();
        let files = LectureFiles::standard(dir.path(), chrono::NaiveDate::from_ymd_opt(2026, 9, 25).unwrap());
        std::fs::create_dir_all(&files.slides).unwrap();
        let store = Store::offline(Sidecar::default(), files.sidecar());
        (dir, files, store)
    }

    fn png(path: &Path) {
        image::RgbImage::from_pixel(64, 36, image::Rgb([200, 200, 200])).save_with_format(path, image::ImageFormat::Png).unwrap();
    }

    #[tokio::test]
    async fn register_moves_names_and_records_with_the_badges() {
        let (_d, files, store) = setup();
        let tmp = files.slides.join(".capture-1.png.tmp");
        png(&tmp);
        let at = Local.with_ymd_and_hms(2026, 9, 25, 10, 2, 51).unwrap();
        let s = register(&files, &store, &tmp, SlideMeta { shown_at: at, auto: true, uncertain: true }).await.unwrap().unwrap();
        assert_eq!((s.index, s.file.as_str(), s.auto, s.uncertain), (1, "slides/slide_01_100251.png", true, true));
        assert!(!tmp.exists() && files.slides.join("slide_01_100251.png").exists());
        assert_eq!(store.read().await.unwrap().slides, vec![s]);
    }

    #[tokio::test]
    async fn a_file_already_registered_is_not_registered_again() {
        let (_d, files, store) = setup();
        let tmp = files.slides.join(".capture-1.png.tmp");
        png(&tmp);
        let s = register(&files, &store, &tmp, SlideMeta::manual(Local::now())).await.unwrap().unwrap();
        let again = register(&files, &store, &files.dir.join(&s.file), SlideMeta::manual(Local::now())).await.unwrap();
        assert_eq!(again, None, "the watcher listing slides/ finds the worker's file already registered");
        assert_eq!(store.read().await.unwrap().slides.len(), 1);
        assert!(files.dir.join(&s.file).exists(), "not renamed a second time");
    }

    #[tokio::test]
    async fn paths_registering_at_once_never_share_an_index() {
        let (_d, files, store) = setup();
        let mut tasks = Vec::new();
        for i in 0..8 {
            let (files, store) = (files.clone(), store.clone());
            let p = files.slides.join(format!(".capture-{i}.png.tmp"));
            png(&p);
            tasks.push(tokio::spawn(async move { register(&files, &store, &p, SlideMeta::manual(Local::now())).await.unwrap().unwrap().index }));
        }
        let mut got: Vec<u32> = futures_util::future::join_all(tasks).await.into_iter().map(Result::unwrap).collect();
        got.sort();
        assert_eq!(got, (1..=8).collect::<Vec<_>>());
    }

    #[tokio::test]
    async fn import_copies_images_and_refuses_the_rest() {
        let (d, files, store) = setup();
        let outside = d.path().join("Desktop");
        std::fs::create_dir_all(&outside).unwrap();
        let (a, b, text) = (outside.join("Board.JPG"), outside.join("chart.png"), outside.join("notes.txt"));
        png(&b); // core builds `image` with PNG only; sips makes the JPEG
        assert!(std::process::Command::new("sips").args(["-s", "format", "jpeg"]).arg(&b).arg("--out").arg(&a).output().unwrap().status.success());
        std::fs::write(&text, "not an image").unwrap();
        let out = import(&files, &store, &[a.clone(), text.clone(), b.clone(), b.clone()]).await;
        let ok: Vec<_> = out.iter().filter_map(|(_, r)| r.as_ref().ok()).collect();
        assert_eq!(ok.len(), 3, "{out:?}");
        assert!(ok[0].file.ends_with(".jpg"), "{}", ok[0].file);
        assert!(ok.iter().all(|s| !s.auto && !s.uncertain));
        assert!(out[1].1.as_ref().unwrap_err().contains("notes.txt"));
        assert!(a.exists() && b.exists(), "the originals stay where they were");
        assert_eq!(ok[1].index + 1, ok[2].index, "a second drop of the same file is a second slide");
        assert!(std::fs::read_dir(&files.slides).unwrap().flatten().all(|e| !e.file_name().to_string_lossy().ends_with(".tmp")));
    }

    async fn three(files: &LectureFiles, store: &Store) -> Vec<SlideEntry> {
        let mut out = Vec::new();
        for k in 1..=3u32 {
            let tmp = files.slides.join(format!(".capture-{k}.png.tmp"));
            png(&tmp);
            let at = Local.with_ymd_and_hms(2026, 9, 25, 10, 0, k).unwrap();
            out.push(register(files, store, &tmp, SlideMeta { shown_at: at, auto: true, uncertain: false }).await.unwrap().unwrap());
        }
        out
    }

    fn idle() -> Arc<AtomicBool> {
        Arc::new(AtomicBool::new(false))
    }

    #[tokio::test]
    async fn a_removed_slide_leaves_the_record_and_the_slide_files_but_is_kept() {
        let (_d, files, store) = setup();
        let slides = three(&files, &store).await;
        let gone = remove(&files, &store, 2, idle()).await.unwrap();
        assert_eq!(gone, slides[1]);
        assert_eq!(store.read().await.unwrap().slides, vec![slides[0].clone(), slides[2].clone()]);
        assert!(!files.dir.join(&gone.file).exists(), "out of slides/");
        assert!(files.slides.join(REMOVED_DIR).join("slide_02_100002.png").exists(), "kept where the person can take it back");
        let on_disk: Vec<u32> = crate::session::folder::slide_files(&files).unwrap().iter().map(|s| s.index).collect();
        assert_eq!(on_disk, vec![1, 3], "opening the folder again does not register it back");
    }

    #[tokio::test]
    async fn a_slide_a_snapshot_has_used_stays_because_the_notes_embed_it() {
        let (_d, files, store) = setup();
        let slides = three(&files, &store).await;
        store.update(|sc| Ok(sc.notes.slide_index = 2)).await.unwrap();
        for used in [1, 2] {
            let e = remove(&files, &store, used, idle()).await.unwrap_err();
            assert!(format!("{e}").contains("already in the notes"), "{e}");
        }
        assert!(files.dir.join(&slides[1].file).exists());
        assert_eq!(store.read().await.unwrap().slides.len(), 3);
        remove(&files, &store, 3, idle()).await.expect("the one after the snapshot's cursor is still pending");
    }

    #[tokio::test]
    async fn nothing_is_removed_while_a_snapshot_may_hold_the_slide() {
        let (_d, files, store) = setup();
        let slides = three(&files, &store).await;
        let busy = Arc::new(AtomicBool::new(true));
        let e = remove(&files, &store, 3, busy.clone()).await.unwrap_err();
        assert!(format!("{e}").contains("snapshot is running"), "{e}");
        assert!(files.dir.join(&slides[2].file).exists() && store.read().await.unwrap().slides.len() == 3);
        busy.store(false, Ordering::SeqCst);
        remove(&files, &store, 3, busy).await.unwrap();
    }

    #[tokio::test]
    async fn an_unknown_slide_is_an_error_and_a_slide_whose_file_is_gone_is_still_dropped() {
        let (_d, files, store) = setup();
        let slides = three(&files, &store).await;
        assert!(format!("{}", remove(&files, &store, 9, idle()).await.unwrap_err()).contains("not in this lecture"));
        std::fs::remove_file(files.dir.join(&slides[0].file)).unwrap();
        remove(&files, &store, 1, idle()).await.unwrap();
        assert_eq!(store.read().await.unwrap().slides.len(), 2);
        assert!(!files.slides.join(REMOVED_DIR).exists(), "there was no file to keep");
    }

    #[tokio::test]
    async fn a_record_that_names_a_file_outside_the_slides_folder_moves_nothing() {
        let (_d, files, store) = setup();
        three(&files, &store).await;
        let outside = files.dir.join("not-a-slide.png");
        png(&outside);
        let link = files.slides.join("slide_09_100009.png");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        // The sidecar is a file anyone can edit: records are pointed at a `..` path, an absolute path and a link.
        for (index, file) in [(1u32, "slides/../not-a-slide.png".to_string()), (2, outside.to_string_lossy().into_owned()), (3, "slides/slide_09_100009.png".to_string())] {
            store.update(move |sc| Ok(sc.slides.iter_mut().find(|s| s.index == index).unwrap().file = file)).await.unwrap();
            let e = remove(&files, &store, index, idle()).await.unwrap_err();
            assert!(format!("{e}").contains("not in the slides folder"), "{e}");
        }
        assert!(outside.exists() && link.exists(), "nothing was moved");
        assert!(!files.slides.join(REMOVED_DIR).exists());
        assert_eq!(store.read().await.unwrap().slides.len(), 3, "the records stay");
    }

    #[tokio::test]
    async fn the_next_slide_reserves_a_removed_slides_number_for_undo() {
        let (_d, files, store) = setup();
        three(&files, &store).await;
        remove(&files, &store, 3, idle()).await.unwrap();
        let tmp = files.slides.join(".capture-4.png.tmp");
        png(&tmp);
        let s = register(&files, &store, &tmp, SlideMeta::manual(Local.with_ymd_and_hms(2026, 9, 25, 10, 5, 0).unwrap())).await.unwrap().unwrap();
        assert_eq!(s.index, 4, "undo must not collide with a new capture");
        assert_eq!(undo_remove(&files, &store, idle()).await.unwrap().index, 3);
    }

    #[tokio::test]
    async fn undo_restores_multiple_removals_in_reverse_order_with_metadata_and_bytes() {
        let (_d, files, store) = setup();
        let slides = three(&files, &store).await;
        let bytes = std::fs::read(files.dir.join(&slides[1].file)).unwrap();
        remove(&files, &store, 2, idle()).await.unwrap();
        remove(&files, &store, 1, idle()).await.unwrap();
        assert_eq!(undo_remove(&files, &store, idle()).await.unwrap(), slides[0]);
        assert_eq!(undo_remove(&files, &store, idle()).await.unwrap(), slides[1]);
        assert_eq!(std::fs::read(files.dir.join(&slides[1].file)).unwrap(), bytes);
        assert_eq!(store.read().await.unwrap().slides, slides);
        assert!(store.read().await.unwrap().removed_slides.is_empty());
        assert!(undo_remove(&files, &store, idle()).await.unwrap_err().to_string().contains("no removed slide"));
    }

    #[tokio::test]
    async fn undo_survives_reopening_and_a_later_snapshot_makes_the_restored_image_pending() {
        let (_d, files, store) = setup();
        let slides = three(&files, &store).await;
        remove(&files, &store, 2, idle()).await.unwrap();
        store.update(|sc| Ok(sc.notes.slide_index = 3)).await.unwrap();
        let (sc, _) = crate::session::folder::open(&files, "Lecture", false).unwrap();
        assert_eq!(sc.removed_slides, vec![slides[1].clone()]);
        let reopened = Store::offline(sc, files.sidecar());
        let restored = undo_remove(&files, &reopened, idle()).await.unwrap();
        assert_eq!(restored.index, 4);
        assert_eq!((restored.shown_at, restored.auto, restored.uncertain), (slides[1].shown_at, true, false));
        assert!(restored.file.ends_with("slide_04_100002.png"));
        assert!(files.dir.join(&restored.file).is_file());
        let sc = reopened.read().await.unwrap();
        assert_eq!(sc.notes.slide_index, 3);
        assert!(sc.removed_slides.is_empty());
        assert_eq!(sc.slides.iter().filter(|s| s.index > sc.notes.slide_index).cloned().collect::<Vec<_>>(), vec![restored]);
    }

    #[tokio::test]
    async fn undo_refuses_a_snapshot_or_missing_file_without_losing_history() {
        let (_d, files, store) = setup();
        three(&files, &store).await;
        let gone = remove(&files, &store, 2, idle()).await.unwrap();
        let busy = Arc::new(AtomicBool::new(true));
        assert!(undo_remove(&files, &store, busy).await.unwrap_err().to_string().contains("snapshot is running"));
        std::fs::remove_file(files.slides.join(REMOVED_DIR).join("slide_02_100002.png")).unwrap();
        assert!(undo_remove(&files, &store, idle()).await.is_err());
        assert_eq!(store.read().await.unwrap().removed_slides, vec![gone]);
    }

    #[tokio::test]
    async fn undo_still_works_when_polish_retired_the_removed_capture_file() {
        let (_d, files, store) = setup();
        let slides = three(&files, &store).await;
        remove(&files, &store, 2, idle()).await.unwrap();
        std::fs::write(&files.notes, "# Lecture").unwrap();
        let fills = crate::notes::page::Fills { keystone: "k".into(), summary: "s".into(), content: "c".into(), slides: "{}".into() };
        let page = crate::notes::page::render(&fills, "Test", &files.notes, "Week 01 — Test", crate::notes::page::TEMPLATE).unwrap();
        assert_eq!(crate::notes::archive::clear(&page).unwrap().files, 1);
        assert!(!files.slides.join(REMOVED_DIR).join("slide_02_100002.png").exists());
        let html = std::fs::read_to_string(&page).unwrap();
        assert_eq!(crate::notes::archive::script(&html, "slides").unwrap(), "{}", "a removed image stays out of the gallery");
        let restored = undo_remove(&files, &store, idle()).await.unwrap();
        assert_eq!((restored.index, restored.shown_at, restored.auto), (2, slides[1].shown_at, true));
        assert!(restored.file.ends_with(".jpg"));
        assert_eq!(crate::notes::chat::Image::read(&files.dir.join(restored.file)).unwrap().mime, "image/jpeg");
        assert!(store.read().await.unwrap().removed_slides.is_empty());
    }

    #[tokio::test]
    async fn undo_overwrites_neither_an_existing_destination_nor_a_link_outside_removed() {
        let (_d, files, store) = setup();
        let slides = three(&files, &store).await;
        remove(&files, &store, 2, idle()).await.unwrap();
        let dest = files.dir.join(&slides[1].file);
        std::fs::write(&dest, b"another image").unwrap();
        assert!(undo_remove(&files, &store, idle()).await.unwrap_err().to_string().contains("already exists"));
        assert_eq!(std::fs::read(&dest).unwrap(), b"another image");
        std::fs::remove_file(&dest).unwrap();
        let from = files.slides.join(REMOVED_DIR).join("slide_02_100002.png");
        std::fs::remove_file(&from).unwrap();
        std::os::unix::fs::symlink(files.dir.join(&slides[0].file), &from).unwrap();
        assert!(undo_remove(&files, &store, idle()).await.is_err());
        assert!(files.dir.join(&slides[0].file).exists());
        assert_eq!(store.read().await.unwrap().removed_slides.len(), 1);
    }
}
