//! The one registration path for slides (spec §7.3, §8): auto and manual captures, screenshots and
//! dropped images are all moved into `slides/` under the CLI's name and recorded inside the sidecar's
//! writer, so indexes never collide and no file is registered twice.
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
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
}
