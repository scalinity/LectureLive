//! Windows to capture (spec §7.1): enumeration through CoreGraphics, which also finds a window that is
//! off screen, and capture through `xcap`. A missing permission is an error of its own, never "no windows".
use std::path::Path;

use anyhow::{bail, Context, Result};
use core_foundation::array::{CFArray, CFArrayRef};
use core_foundation::base::{CFType, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use image::{imageops::FilterType, RgbaImage};
use serde::{Deserialize, Serialize};

pub const MAX_PX: u32 = 1600;

#[repr(C)]
#[derive(Default)]
struct CGRect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
    fn CGWindowListCopyWindowInfo(option: u32, relative_to_window: u32) -> CFArrayRef;
    fn CGRectMakeWithDictionaryRepresentation(dict: CFDictionaryRef, rect: *mut CGRect) -> bool;
}

/// `kCGWindowListOptionAll | kCGWindowListExcludeDesktopElements`: on screen or not.
const ALL_WINDOWS: u32 = 16;
/// Windows smaller than this are palettes, menus and status items, not something to watch.
const MIN_SIDE: u32 = 100;

/// A window as the picker lists it and the descriptor names it; sizes in points.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowInfo {
    pub id: u32,
    pub app: String,
    /// None for a program outside an app bundle.
    pub bundle_id: Option<String>,
    pub title: String,
    pub width: u32,
    pub height: u32,
    /// False when it is minimised or on another desktop.
    pub on_screen: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CaptureError {
    /// Screen Recording is not granted: other apps' windows would silently be missing.
    Denied,
    Failed(String),
}

impl std::fmt::Display for CaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Denied => write!(f, "Screen Recording is off for LectureLive: System Settings → Privacy & Security → Screen & System Audio Recording"),
            Self::Failed(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for CaptureError {}

/// Where the capture worker finds and captures windows: the system, or a fake in tests.
pub trait WindowSource: Send {
    fn windows(&mut self) -> Result<Vec<WindowInfo>, CaptureError>;
    fn capture(&mut self, id: u32) -> Result<RgbaImage, CaptureError>;
}

/// This Mac's windows.
pub struct SystemWindows;

fn number(d: &CFDictionary<CFString, CFType>, key: &'static str) -> Option<i64> {
    d.find(CFString::from_static_string(key)).and_then(|v| v.downcast::<CFNumber>()).and_then(|n| n.to_i64())
}

fn string(d: &CFDictionary<CFString, CFType>, key: &'static str) -> Option<String> {
    d.find(CFString::from_static_string(key)).and_then(|v| v.downcast::<CFString>()).map(|s| s.to_string())
}

fn bundle_id(pid: i32) -> Option<String> {
    objc2::rc::autoreleasepool(|_| objc2_app_kit::NSRunningApplication::runningApplicationWithProcessIdentifier(pid)?.bundleIdentifier().map(|s| s.to_string()))
}

impl WindowSource for SystemWindows {
    fn windows(&mut self) -> Result<Vec<WindowInfo>, CaptureError> {
        ensure_screen_access().map_err(|_| CaptureError::Denied)?;
        let raw = unsafe { CGWindowListCopyWindowInfo(ALL_WINDOWS, 0) };
        if raw.is_null() {
            return Err(CaptureError::Failed("the window list is unavailable".into()));
        }
        let list: CFArray<CFDictionary<CFString, CFType>> = unsafe { CFArray::wrap_under_create_rule(raw) };
        let mut out = Vec::new();
        for d in list.iter() {
            if number(&d, "kCGWindowLayer") != Some(0) {
                continue;
            }
            let (Some(id), Some(pid)) = (number(&d, "kCGWindowNumber"), number(&d, "kCGWindowOwnerPID")) else { continue };
            let mut rect = CGRect::default();
            let bounds = d.find(CFString::from_static_string("kCGWindowBounds")).and_then(|v| v.downcast::<CFDictionary>());
            if !bounds.is_some_and(|b| unsafe { CGRectMakeWithDictionaryRepresentation(b.as_concrete_TypeRef(), &mut rect) }) {
                continue;
            }
            let (width, height) = (rect.w.round() as u32, rect.h.round() as u32);
            if width < MIN_SIDE || height < MIN_SIDE {
                continue;
            }
            let on_screen = d.find(CFString::from_static_string("kCGWindowIsOnscreen")).and_then(|v| v.downcast::<CFBoolean>()).is_some_and(bool::from);
            out.push(WindowInfo { id: id as u32, app: string(&d, "kCGWindowOwnerName").unwrap_or_default(), bundle_id: bundle_id(pid as i32), title: string(&d, "kCGWindowName").unwrap_or_default(), width, height, on_screen });
        }
        Ok(out)
    }

    fn capture(&mut self, id: u32) -> Result<RgbaImage, CaptureError> {
        ensure_screen_access().map_err(|_| CaptureError::Denied)?;
        let window = xcap::Window::all().map_err(|e| CaptureError::Failed(format!("list windows: {e}")))?.into_iter().find(|w| w.id().is_ok_and(|i| i == id)).ok_or_else(|| CaptureError::Failed("the window is not on screen".into()))?;
        window.capture_image().map_err(|e| CaptureError::Failed(format!("capture: {e}")))
    }
}

/// xcap only preflights: without the grant it silently drops other apps' windows, so a
/// missing permission would read as "no windows". Requesting registers the app in
/// System Settings and shows the prompt once.
fn ensure_screen_access() -> Result<()> {
    if unsafe { CGPreflightScreenCaptureAccess() } {
        return Ok(());
    }
    unsafe { CGRequestScreenCaptureAccess() };
    bail!("Screen Recording not granted (System Settings → Privacy & Security → Screen & System Audio Recording)")
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

/// The canary's listing (M0): every normal window, on screen or not.
pub fn list_windows() -> Result<Vec<WindowInfo>> {
    Ok(SystemWindows.windows()?)
}

pub fn capture_window(id: u32, out: &Path) -> Result<(u32, u32)> {
    ensure_screen_access()?;
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

    /// Needs Screen Recording (inherited from Terminal.app when run from its shell):
    /// cargo test -p lecturelive-core system_windows -- --ignored
    #[test]
    #[ignore]
    fn system_windows_lists_this_session_s_terminal_with_its_bundle_id() {
        let ws = SystemWindows.windows().unwrap();
        assert!(ws.iter().any(|w| w.bundle_id.as_deref() == Some("com.apple.Terminal")), "{:?}", ws.iter().map(|w| (&w.app, &w.bundle_id)).collect::<Vec<_>>());
    }
}
