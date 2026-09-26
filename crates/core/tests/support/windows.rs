//! A scripted window source: windows appear, move off screen, vanish and change content under test control.
use std::sync::{Arc, Mutex};

use image::{Rgba, RgbaImage};
use lecturelive_core::capture::window::{CaptureError, WindowInfo, WindowSource};

#[derive(Default)]
pub struct Screen {
    pub windows: Vec<(WindowInfo, RgbaImage)>,
    pub denied: bool,
    pub fail_next: u32,
    pub blank_next: u32,
    pub captures: u32,
    /// Windows off screen can still be captured: a full-screen window on another desktop.
    pub off_screen_capture: bool,
}

#[derive(Clone, Default)]
pub struct FakeWindows(pub Arc<Mutex<Screen>>);

impl FakeWindows {
    pub fn add(&self, id: u32, title: &str, w: u32, h: u32, content: RgbaImage) {
        let info = WindowInfo { id, app: "zoom.us".into(), bundle_id: Some("us.zoom.xos".into()), title: title.into(), width: w, height: h, on_screen: true };
        self.0.lock().unwrap().windows.push((info, content));
    }

    pub fn show(&self, id: u32, content: RgbaImage) {
        self.with(id, |_, c| *c = content);
    }

    pub fn on_screen(&self, id: u32, on: bool) {
        self.with(id, |i, _| i.on_screen = on);
    }

    pub fn resize(&self, id: u32, w: u32, h: u32) {
        self.with(id, |i, _| (i.width, i.height) = (w, h));
    }

    pub fn close(&self, id: u32) {
        self.0.lock().unwrap().windows.retain(|(i, _)| i.id != id);
    }

    fn with(&self, id: u32, f: impl FnOnce(&mut WindowInfo, &mut RgbaImage)) {
        let mut s = self.0.lock().unwrap();
        let (i, c) = s.windows.iter_mut().find(|(i, _)| i.id == id).expect("a window");
        f(i, c);
    }
}

impl WindowSource for FakeWindows {
    fn windows(&mut self) -> Result<Vec<WindowInfo>, CaptureError> {
        let s = self.0.lock().unwrap();
        if s.denied {
            return Err(CaptureError::Denied);
        }
        Ok(s.windows.iter().map(|(i, _)| i.clone()).collect())
    }

    fn capture(&mut self, id: u32) -> Result<RgbaImage, CaptureError> {
        let mut s = self.0.lock().unwrap();
        s.captures += 1;
        if s.fail_next > 0 {
            s.fail_next -= 1;
            return Err(CaptureError::Failed("the window server said no".into()));
        }
        let blank = s.blank_next > 0;
        if blank {
            s.blank_next -= 1;
        }
        let (i, c) = s.windows.iter().find(|(i, _)| i.id == id).ok_or(CaptureError::Failed("gone".into()))?;
        if !i.on_screen && !s.off_screen_capture {
            return Err(CaptureError::Failed("not on screen".into()));
        }
        if blank {
            let (w, h) = c.dimensions();
            return Ok(RgbaImage::from_pixel(w, h, Rgba([0, 0, 0, 255])));
        }
        Ok(c.clone())
    }
}

/// A slide as a window shows it: a white page with `lines` dark bullet bars.
pub fn slide(lines: u32) -> RgbaImage {
    let mut img = RgbaImage::from_pixel(640, 360, Rgba([250, 250, 250, 255]));
    for l in 0..lines {
        for y in 60 + l * 40..60 + l * 40 + 14 {
            for x in 60..520 {
                img.put_pixel(x, y, Rgba([30, 30, 40, 255]));
            }
        }
    }
    img
}

/// Zoom's meeting window at `w`×`h`: a dark background, a video tile, and the shared slide (`lines` bullet
/// bars, a title and a diagram) scaled into `at` (x, y, width, height in pixels).
pub fn zoom_window(w: u32, h: u32, at: (u32, u32, u32, u32), lines: u32) -> RgbaImage {
    let mut img = RgbaImage::from_pixel(w, h, Rgba([28, 28, 30, 255]));
    let (sx, sy, sw, sh) = at;
    let fill = |img: &mut RgbaImage, x0: f64, y0: f64, x1: f64, y1: f64, v: u8| {
        for y in (sy as f64 + y0 * sh as f64) as u32..(sy as f64 + y1 * sh as f64) as u32 {
            for x in (sx as f64 + x0 * sw as f64) as u32..(sx as f64 + x1 * sw as f64) as u32 {
                img.put_pixel(x, y, Rgba([v, v, v, 255]));
            }
        }
    };
    fill(&mut img, 0.0, 0.0, 1.0, 1.0, 250);
    fill(&mut img, 0.06, 0.07, 0.55, 0.15, 25); // the title
    for l in 0..lines {
        let y = 0.25 + l as f64 * 0.1;
        fill(&mut img, 0.08, y, 0.08 + 0.3 + (l % 3) as f64 * 0.15, y + 0.045, 45);
    }
    fill(&mut img, 0.70, 0.30, 0.92, 0.75, 110); // a diagram
    fill(&mut img, 0.74, 0.40, 0.88, 0.48, 20);
    // A participant's video beside the share, outside it.
    for y in 8..8 + h / 8 {
        for x in w - w / 6 - 8..w - 8 {
            img.put_pixel(x, y, Rgba([90, 120, 150, 255]));
        }
    }
    img
}
