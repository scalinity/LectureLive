//! Capture files can be retired once the finished HTML holds their images. The small manifest
//! points into the page's existing slide gallery; no second copy of the images is stored.
use std::collections::{BTreeMap, HashMap};
use std::path::{Component, Path, PathBuf};
use std::sync::LazyLock;

use anyhow::{bail, Context, Result};
use base64::Engine;
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::fsutil::write_atomic;
use crate::notes::chat::Image;
use crate::session::notesfile::sha256_hex;

static SCRIPTS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?s)<script\b[^>]*\bid=["']([^"']+)["'][^>]*>(.*?)</script\s*>"#).unwrap());

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Capture {
    pub slide: String,
    pub sha256: String,
    pub image_sha256: String,
}
pub type Manifest = BTreeMap<String, Capture>;

pub fn script(html: &str, id: &str) -> Option<String> {
    SCRIPTS.captures_iter(html).find(|c| &c[1] == id).map(|c| c[2].to_string())
}

/// Only passive PNG/JPEG payloads from the saved gallery are image sources.
pub fn image(uri: &str) -> Option<Image> {
    let (mime, data) = uri.strip_prefix("data:")?.split_once(";base64,")?;
    let mime = match mime { "image/jpeg" => "image/jpeg", "image/png" => "image/png", _ => return None };
    let bytes = base64::engine::general_purpose::STANDARD.decode(data).ok()?;
    let valid = if mime == "image/jpeg" { bytes.starts_with(&[0xff, 0xd8]) && bytes.ends_with(&[0xff, 0xd9]) } else { bytes.starts_with(b"\x89PNG\r\n\x1a\n") };
    valid.then(|| Image { mime, base64: data.into() })
}

#[derive(Clone)]
pub struct Saved {
    pub page: PathBuf,
    pub capture: Capture,
    pub uri: String,
}

#[derive(Default)]
pub struct Archive(pub HashMap<PathBuf, Saved>);

impl Archive {
    pub fn load(dir: &Path) -> Self {
        let mut out = Self::default();
        let Ok(entries) = std::fs::read_dir(dir) else { return out };
        for entry in entries.flatten() {
            let page = entry.path();
            if page.extension().and_then(|s| s.to_str()) != Some("html") { continue; }
            let Ok(html) = std::fs::read_to_string(&page) else { continue };
            let Ok((manifest, slides)) = parts(&html) else { continue };
            for (file, capture) in manifest {
                if let Some(uri) = slides.get(&capture.slide).filter(|u| image(u).is_some() && sha256_hex(u.as_bytes()) == capture.image_sha256) {
                    out.0.insert(dir.join(file), Saved { page: page.clone(), capture, uri: uri.clone() });
                }
            }
        }
        out
    }

    pub fn uri(&self, path: &Path) -> Option<&str> { self.0.get(path).map(|s| s.uri.as_str()) }
    pub fn highest_index(&self) -> u32 { self.0.keys().filter_map(|p| p.file_stem()?.to_str()?.strip_prefix("slide_")?.split('_').next()?.parse::<u32>().ok()).max().unwrap_or(0) }
}

fn parts(html: &str) -> Result<(Manifest, BTreeMap<String, String>)> {
    let manifest = serde_json::from_str(&script(html, "capture-files").context("The page has no capture manifest")?)?;
    let mut slides: BTreeMap<String, String> = serde_json::from_str(&script(html, "slides").context("The page has no embedded slide gallery")?)?;
    let removed: BTreeMap<String, String> = script(html, "removed-captures").map(|s| serde_json::from_str(&s)).transpose()?.unwrap_or_default();
    slides.extend(removed);
    Ok((manifest, slides))
}

pub fn manifest_json(manifest: &Manifest) -> Result<String> { Ok(serde_json::to_string(manifest)?.replace("</", "<\\/")) }

pub fn with_manifest(html: &str, manifest: &Manifest) -> Result<String> {
    with_script(html, "capture-files", &manifest_json(manifest)?)
}

fn with_script(html: &str, id: &str, json: &str) -> Result<String> {
    let tag = format!("<script type=\"application/json\" id=\"{id}\">{json}</script>");
    if let Some(old) = SCRIPTS.captures_iter(html).find(|c| &c[1] == id) {
        return Ok(html.replacen(&old[0], &tag, 1));
    }
    let Some(at) = html.rfind("</body>") else { bail!("The study page has no closing body; it was left intact") };
    Ok(format!("{}\n{tag}\n{}", &html[..at], &html[at..]))
}

pub fn with_removed(html: &str, images: &BTreeMap<String, String>) -> Result<String> {
    with_script(html, "removed-captures", &serde_json::to_string(images)?.replace("</", "<\\/"))
}

/// Removed images stay out of the gallery, but their smaller HTML copy keeps deletion undo usable.
pub fn removed_captures(dir: &Path, page: &Path, archived: &Archive) -> (Manifest, BTreeMap<String, String>) {
    let mut manifest = Manifest::new();
    let mut images = BTreeMap::new();
    for (file, saved) in &archived.0 {
        if saved.page == page && saved.capture.slide.starts_with("removed:") {
            if let Ok(file) = file.strip_prefix(dir) {
                manifest.insert(file.to_string_lossy().into_owned(), saved.capture.clone());
                images.insert(saved.capture.slide.clone(), saved.uri.clone());
            }
        }
    }
    if let Ok(files) = std::fs::read_dir(dir.join("slides/removed")) {
        for entry in files.flatten() {
            let path = entry.path();
            let Ok(relative) = path.strip_prefix(dir) else { continue };
            let file = relative.to_string_lossy().into_owned();
            if capture_path(dir, &file).is_none() { continue; }
            let Ok(bytes) = std::fs::read(&path) else { continue };
            let hash = sha256_hex(&bytes);
            let Some(uri) = crate::notes::page::slide_uri(&path).filter(|u| image(u).is_some()) else { continue };
            if std::fs::read(&path).ok().is_none_or(|b| sha256_hex(&b) != hash) { continue; }
            let key = format!("removed:{file}");
            manifest.insert(file, Capture { slide: key.clone(), sha256: hash, image_sha256: sha256_hex(uri.as_bytes()) });
            images.insert(key, uri);
        }
    }
    (manifest, images)
}

fn capture_path(dir: &Path, file: &str) -> Option<PathBuf> {
    let relative = Path::new(file);
    let components: Vec<_> = relative.components().collect();
    if !matches!(components.len(), 2 | 3) || components[0] != Component::Normal("slides".as_ref()) || components.iter().any(|c| !matches!(c, Component::Normal(_))) || (components.len() == 3 && components[1] != Component::Normal("removed".as_ref())) { return None; }
    let path = dir.join(relative);
    if !matches!(path.extension().and_then(|s| s.to_str()), Some("png" | "jpg" | "jpeg")) { return None; }
    if !std::fs::symlink_metadata(&path).ok()?.file_type().is_file() { return None; }
    let expected = if components.len() == 3 { "slides/removed" } else { "slides" };
    if path.canonicalize().ok()?.parent()? != dir.canonicalize().ok()?.join(expected) { return None; }
    Some(path)
}

#[derive(Debug, Default, Serialize)]
pub struct Cleanup {
    pub files: usize,
    pub bytes: u64,
    pub kept: Vec<String>,
}

/// The final HTML is durable before this runs. A changed capture or an incomplete gallery stays.
pub fn clear(page: &Path) -> Result<Cleanup> {
    let html = std::fs::read_to_string(page)?;
    let (manifest, slides) = parts(&html)?;
    let dir = page.parent().context("The study page has no folder")?;
    let mut out = Cleanup::default();
    for (file, capture) in manifest {
        if !dir.join(&file).exists() { continue; }
        let Some(path) = capture_path(dir, &file) else { out.kept.push(file); continue };
        let Ok(bytes) = std::fs::read(&path) else { out.kept.push(file); continue };
        if sha256_hex(&bytes) != capture.sha256 || !slides.get(&capture.slide).is_some_and(|u| image(u).is_some() && sha256_hex(u.as_bytes()) == capture.image_sha256) {
            out.kept.push(file);
            continue;
        }
        if std::fs::remove_file(&path).is_err() { out.kept.push(file); continue; }
        out.files += 1;
        out.bytes += bytes.len() as u64;
    }
    Ok(out)
}

/// Upgrade an existing page without another model request. Its embedded image must exactly match
/// the same JPEG conversion used by the renderer before the original can be retired.
pub fn adopt(page: &Path, paths: &BTreeMap<String, String>, apply: bool) -> Result<Cleanup> {
    let html = std::fs::read_to_string(page)?;
    let slides: BTreeMap<String, String> = serde_json::from_str(&script(&html, "slides").context("The page has no embedded slide gallery")?)?;
    let dir = page.parent().context("The study page has no folder")?;
    let mut manifest: Manifest = script(&html, "capture-files").map(|s| serde_json::from_str(&s)).transpose()?.unwrap_or_default();
    let mut plan = Cleanup::default();
    for (slide, file) in paths {
        let Some(path) = capture_path(dir, file) else { continue };
        let bytes = std::fs::read(&path)?;
        let hash = sha256_hex(&bytes);
        if !slides.get(slide).is_some_and(|uri| image(uri).is_some() && crate::notes::page::slide_uri(&path).as_ref() == Some(uri)) || sha256_hex(&std::fs::read(&path)?) != hash {
            plan.kept.push(file.clone());
            continue;
        }
        manifest.insert(file.clone(), Capture { slide: slide.clone(), sha256: hash, image_sha256: sha256_hex(slides[slide].as_bytes()) });
        plan.files += 1;
        plan.bytes += bytes.len() as u64;
    }
    let (removed, images) = removed_captures(dir, page, &Archive::load(dir));
    for file in removed.keys() {
        if let Ok(meta) = std::fs::metadata(dir.join(file)) { plan.files += 1; plan.bytes += meta.len(); }
    }
    manifest.extend(removed);
    if apply && !manifest.is_empty() {
        let updated = with_removed(&with_manifest(&html, &manifest)?, &images)?;
        write_atomic(page, updated.as_bytes())?;
        return clear(page);
    }
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notes::page::{self, Fills};
    use crate::session::{files::LectureFiles, folder, sidecar::Sidecar};

    fn setup() -> (tempfile::TempDir, LectureFiles, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let files = LectureFiles::standard(dir.path(), chrono::NaiveDate::from_ymd_opt(2026, 10, 22).unwrap());
        std::fs::create_dir(&files.slides).unwrap();
        let image = files.slides.join("slide_07_100000.png");
        image::RgbImage::from_pixel(32, 18, image::Rgb([70, 90, 110])).save(&image).unwrap();
        std::fs::write(&files.notes, "# Lecture\n![Slide 7](slides/slide_07_100000.png)\n").unwrap();
        (dir, files, image)
    }

    fn render(files: &LectureFiles) -> PathBuf {
        let fills = Fills { keystone: "k".into(), summary: "s".into(), content: "<img src=\"slides/slide_07_100000.png\">".into(), slides: r#"{"7":"slides/slide_07_100000.png"}"#.into() };
        page::render(&fills, "Test course", &files.notes, "Week 01 — Test", page::TEMPLATE).unwrap()
    }

    #[test]
    fn clearing_preserves_notes_and_the_only_image_copy_through_rerender_and_reopen() {
        let (_dir, files, image) = setup();
        let page = render(&files);
        let before = std::fs::read(&files.notes).unwrap();
        let original = Archive::load(files.notes_dir()).uri(&image).unwrap().to_string();
        let pending = files.slides.join("pending.png");
        std::fs::copy(&image, &pending).unwrap();
        assert_eq!(clear(&page).unwrap().files, 1);
        assert!(!image.exists() && pending.exists());
        assert_eq!(std::fs::read(&files.notes).unwrap(), before);
        assert_eq!(clear(&page).unwrap().files, 0, "idempotent");
        assert_eq!(folder::next_slide_index(&files, &Sidecar::default()).unwrap(), 8);
        render(&files);
        assert_eq!(Archive::load(files.notes_dir()).uri(&image), Some(original.as_str()));
        let (sc, _) = folder::open(&files, "# Lecture", false).unwrap();
        assert_eq!(sc.slides[0].index, 7, "an archive also recovers legacy capture records");
    }

    #[test]
    fn changed_files_or_changed_embedded_images_are_never_deleted() {
        let (_dir, files, image) = setup();
        let page = render(&files);
        let original = std::fs::read(&image).unwrap();
        std::fs::write(&image, b"a new capture").unwrap();
        assert_eq!(clear(&page).unwrap().kept.len(), 1);
        assert!(image.exists());
        std::fs::write(&image, original).unwrap();
        let html = std::fs::read_to_string(&page).unwrap();
        let old: BTreeMap<String, String> = serde_json::from_str(&script(&html, "slides").unwrap()).unwrap();
        std::fs::write(&page, html.replace(&old["7"], "data:image/jpeg;base64,/9j/2Q==")).unwrap();
        assert_eq!(clear(&page).unwrap().kept.len(), 1);
        assert!(Archive::load(files.notes_dir()).uri(&image).is_none());
        assert!(image.exists());
    }

    #[test]
    fn retrospective_cleanup_is_read_only_until_applied_and_keeps_the_gallery_unchanged() {
        let (_dir, files, image) = setup();
        let page = render(&files);
        let html = std::fs::read_to_string(&page).unwrap();
        let manifest = SCRIPTS.captures_iter(&html).find(|c| &c[1] == "capture-files").unwrap();
        let legacy = html.replacen(&manifest[0], "", 1);
        let gallery = script(&legacy, "slides").unwrap();
        std::fs::write(&page, &legacy).unwrap();
        let paths = [("7".into(), "slides/slide_07_100000.png".into())].into();
        assert_eq!(adopt(&page, &paths, false).unwrap().files, 1);
        assert!(image.exists());
        assert_eq!(std::fs::read_to_string(&page).unwrap(), legacy);
        assert_eq!(adopt(&page, &paths, true).unwrap().files, 1);
        assert!(!image.exists());
        assert_eq!(script(&std::fs::read_to_string(&page).unwrap(), "slides").unwrap(), gallery);
        assert!(Archive::load(files.notes_dir()).uri(&image).is_some());
    }

    #[test]
    fn a_mismatched_old_page_external_file_or_symlink_stays_intact() {
        let (_dir, files, image) = setup();
        let page = render(&files);
        image::RgbImage::from_pixel(32, 18, image::Rgb([150, 50, 20])).save(&image).unwrap();
        let paths = [("7".into(), "slides/slide_07_100000.png".into())].into();
        assert_eq!(adopt(&page, &paths, true).unwrap().files, 0);
        assert!(image.exists());
        let external = files.dir.join("outside.png");
        std::fs::rename(&image, &external).unwrap();
        std::os::unix::fs::symlink(&external, &image).unwrap();
        assert_eq!(clear(&page).unwrap().files, 0);
        assert!(external.exists() && image.exists());
        assert!(super::image("data:image/svg+xml;base64,PHN2Zz4=").is_none());
    }
}
