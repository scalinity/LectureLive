use std::path::Path;

use anyhow::{bail, Context, Result};
use image::{imageops::FilterType, RgbaImage};

pub const MAX_PX: u32 = 1600;

pub struct WindowInfo {
    pub id: u32,
    pub app: String,
    pub title: String,
    pub width: u32,
    pub height: u32,
}

pub fn fit_within(w: u32, h: u32, max: u32) -> (u32, u32) {
    let longest = w.max(h);
    if longest <= max {
        return (w, h);
    }
    let scale = max as f64 / longest as f64;
    ((w as f64 * scale).round() as u32, (h as f64 * scale).round() as u32)
}

/// A frame with no pixel differing from the first is treated as a failed capture.
pub fn is_blank(img: &RgbaImage) -> bool {
    let mut px = img.pixels();
    let Some(first) = px.next() else { return true };
    px.all(|p| p == first)
}

pub fn list_windows() -> Result<Vec<WindowInfo>> {
    let mut out = Vec::new();
    for w in xcap::Window::all().context("enumerate windows (Screen Recording permission?)")? {
        out.push(WindowInfo {
            id: w.id()?,
            app: w.app_name()?,
            title: w.title()?,
            width: w.width()?,
            height: w.height()?,
        });
    }
    Ok(out)
}

pub fn capture_window(id: u32, out: &Path) -> Result<(u32, u32)> {
    let window = xcap::Window::all()?
        .into_iter()
        .find(|w| w.id().map(|i| i == id).unwrap_or(false))
        .with_context(|| format!("window {id} not found"))?;
    let img = window.capture_image().context("capture window")?;
    if is_blank(&img) {
        bail!("capture of window {id} is blank (permission missing or window hidden)");
    }
    let (w, h) = fit_within(img.width(), img.height(), MAX_PX);
    let img = if (w, h) == (img.width(), img.height()) { img } else { image::imageops::resize(&img, w, h, FilterType::Lanczos3) };
    let tmp = out.with_extension("png.tmp");
    img.save_with_format(&tmp, image::ImageFormat::Png)?;
    std::fs::rename(&tmp, out)?;
    Ok((w, h))
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    #[test]
    fn fit_within_keeps_aspect_and_never_upscales() {
        assert_eq!(fit_within(3200, 1800, 1600), (1600, 900));
        assert_eq!(fit_within(1800, 3200, 1600), (900, 1600));
        assert_eq!(fit_within(1280, 720, 1600), (1280, 720));
    }

    #[test]
    fn blank_detection() {
        assert!(is_blank(&RgbaImage::from_pixel(64, 36, Rgba([0, 0, 0, 255]))));
        assert!(is_blank(&RgbaImage::new(0, 0)));
        let mut slide = RgbaImage::from_pixel(64, 36, Rgba([255, 255, 255, 255]));
        slide.put_pixel(10, 10, Rgba([0, 0, 0, 255]));
        assert!(!is_blank(&slide));
    }

    /// Needs Screen Recording permission: cargo test -p lecturelive-core window -- --ignored
    #[test]
    #[ignore]
    fn lists_at_least_one_window() {
        assert!(!list_windows().unwrap().is_empty());
    }
}
