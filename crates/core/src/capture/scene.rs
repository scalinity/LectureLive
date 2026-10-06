//! What a detector frame shows (spec §7.2): a shared slide, or something that must not become one. Zoom
//! draws the speaker's camera, a participant's name on a dark tile and "has started screen sharing" in the
//! same place before and between shares, and a detector that only asks whether pixels changed keeps each of
//! them as a slide (M7.1 class: 17 of the first 21 captures). The measures are taken on the 256×144
//! grayscale the detector already has, so they cost nothing extra and a recording replays to the same answer.
use std::collections::VecDeque;

use image::GrayImage;

/// Levels either side of the commonest level that still count as the page's own colour.
const PAGE_BAND: usize = 4;
/// A camera's compression grain changes the frame by at least this much (mean, 0–1) from one sample to the
/// next; a page held on screen changes it by less. Measured on a recorded class: camera 0.0003–0.29, median
/// 0.003; shared slides median 0.0001.
const MOTION: f32 = 0.0004;
/// Samples averaged for the motion; fewer than this are taken as moving, so a camera is never taken for a page
/// while it is still being looked at for the first time.
const LOOKS: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Features {
    /// Mean level, 0–255.
    pub mean: f32,
    /// Share of pixels within `PAGE_BAND` levels of the commonest level: the page of a slide, the tile of a
    /// placeholder; a photograph has no single level that most of it shares.
    pub dominant: f32,
    /// Share of pixels where nothing within one pixel differs by more than 2 levels.
    pub flat: f32,
    /// Share with a gentle change nearby (3–36 levels): a photograph's gradients and grain.
    pub soft: f32,
    /// Share with a sharp change nearby (more than 36 levels): text, rules and the edges of diagrams.
    pub hard: f32,
}

/// The measures of one frame; all zero for a frame with no interior.
pub fn features(g: &GrayImage) -> Features {
    let (w, h) = (g.width() as usize, g.height() as usize);
    let px = g.as_raw();
    let mut hist = [0u32; 256];
    let mut sum = 0u64;
    for &v in px {
        hist[v as usize] += 1;
        sum += v as u64;
    }
    let total = px.len().max(1) as f32;
    let dominant = (0usize..256).map(|l| hist[l.saturating_sub(PAGE_BAND)..=(l + PAGE_BAND).min(255)].iter().sum::<u32>()).max().unwrap_or(0) as f32 / total;
    let (mut flat, mut soft, mut hard, mut n) = (0u32, 0u32, 0u32, 0u32);
    for y in 1..h.saturating_sub(1) {
        for x in 1..w.saturating_sub(1) {
            let (mut lo, mut hi) = (255u8, 0u8);
            for row in y - 1..=y + 1 {
                for &v in &px[row * w + x - 1..=row * w + x + 1] {
                    lo = lo.min(v);
                    hi = hi.max(v);
                }
            }
            match hi - lo {
                0..=2 => flat += 1,
                3..=36 => soft += 1,
                _ => hard += 1,
            }
            n += 1;
        }
    }
    let n = n.max(1) as f32;
    Features { mean: sum as f32 / total, dominant, flat: flat as f32 / n, soft: soft as f32 / n, hard: hard as f32 / n }
}

/// What the frame is, as far as taking it as a slide goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scene {
    /// A page of something shared: take it when it changes.
    Slide,
    /// A camera or a gallery of them: moving pictures with no page.
    Camera,
    /// A dark tile with a name or a notice on it: Zoom's own screens, never a slide.
    Placeholder,
}

impl Scene {
    /// What the strip says while it waits.
    pub fn waiting_for(self) -> Option<&'static str> {
        match self {
            Scene::Slide => None,
            Scene::Camera => Some("a camera is showing"),
            Scene::Placeholder => Some("Zoom is showing a name or a notice"),
        }
    }
}

/// `moving`: the frame is not the same as a moment ago. A photograph that holds still is a slide with a
/// photograph on it; the same pixels changing every second are a camera.
pub fn classify(f: &Features, moving: bool) -> Scene {
    // Zoom's own tiles: a name or a notice on a dark page, with next to no sharp edges. Recorded: a name tile
    // (mean 34, dominant 0.84, hard 0.04) and the screen-sharing notice (15, 0.86, 0.02); the darkest real
    // slide had a mean of 170.
    if f.mean < 70.0 && f.dominant >= 0.75 && f.hard < 0.05 {
        Scene::Placeholder
    // A photograph's gradients and grain, and no single level most of it shares. Recorded: the lectern camera
    // (soft 0.41–0.48, dominant 0.15–0.17); no slide had soft above 0.15 or dominant below 0.48.
    } else if f.soft >= 0.25 && f.dominant < 0.40 && moving {
        Scene::Camera
    } else {
        Scene::Slide
    }
}

/// Looks at each sample in turn: the measures, and whether the picture is moving.
#[derive(Default)]
pub struct Gate {
    before: Option<GrayImage>,
    deltas: VecDeque<f32>,
}

fn mean_abs_diff(a: &GrayImage, b: &GrayImage) -> f32 {
    a.as_raw().iter().zip(b.as_raw()).map(|(x, y)| x.abs_diff(*y) as u64).sum::<u64>() as f32 / a.as_raw().len().max(1) as f32 / 255.0
}

impl Gate {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn look(&mut self, g: &GrayImage) -> Scene {
        match self.before.as_ref().filter(|b| b.dimensions() == g.dimensions()) {
            Some(b) => {
                self.deltas.push_back(mean_abs_diff(b, g));
                while self.deltas.len() > LOOKS {
                    self.deltas.pop_front();
                }
            }
            None => self.deltas.clear(),
        }
        self.before = Some(g.clone());
        let moving = self.deltas.len() < LOOKS || self.deltas.iter().sum::<f32>() / LOOKS as f32 >= MOTION;
        classify(&features(g), moving)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::detect::{H, W};
    use image::Luma;

    /// A deterministic grain: no random-number crate for a test.
    fn grain(seed: u32) -> impl FnMut() -> i32 {
        let mut s = seed.wrapping_mul(2654435761).wrapping_add(1);
        move || {
            s = s.wrapping_mul(1664525).wrapping_add(1013904223);
            ((s >> 24) as i32 % 21) - 10
        }
    }

    /// A light page with title and bullet bars, as a deck draws it.
    fn page() -> GrayImage {
        let mut g = GrayImage::from_pixel(W, H, Luma([246]));
        for (y0, w) in [(14, 150), (46, 190), (64, 170), (82, 200), (100, 140)] {
            for y in y0..y0 + 6 {
                for x in 20..20 + w {
                    g.put_pixel(x, y, Luma([40]));
                }
            }
        }
        g
    }

    /// A camera frame: smooth shading with grain that differs on every look.
    fn camera(seed: u32) -> GrayImage {
        let mut noise = grain(seed);
        GrayImage::from_fn(W, H, |x, y| {
            let shade = 60 + (x * 90 / W) + (y * 70 / H) + ((x / 24 + y / 18) % 3) * 17;
            Luma([(shade as i32 + noise()).clamp(0, 255) as u8])
        })
    }

    /// A dark tile with some centred text: a participant's name, or Zoom's notice.
    fn tile(rows: u32) -> GrayImage {
        let mut g = GrayImage::from_pixel(W, H, Luma([30]));
        for r in 0..rows {
            for y in 66 + r * 14..72 + r * 14 {
                for x in 80..176 {
                    g.put_pixel(x, y, Luma([240]));
                }
            }
        }
        g
    }

    #[test]
    fn a_page_a_camera_and_a_dark_tile_are_told_apart() {
        assert_eq!(classify(&features(&page()), true), Scene::Slide);
        assert_eq!(classify(&features(&camera(1)), true), Scene::Camera);
        assert_eq!(classify(&features(&tile(1)), true), Scene::Placeholder, "a name");
        assert_eq!(classify(&features(&tile(2)), true), Scene::Placeholder, "a notice of two lines");
    }

    /// A dark deck is still a deck: dense text and diagram edges are sharp, which a placeholder's are not.
    #[test]
    fn a_dark_slide_with_content_is_a_slide() {
        let mut g = GrayImage::from_pixel(W, H, Luma([28]));
        for y in (10..H - 10).step_by(9) {
            for x in (12..W - 12).filter(|x| x % 7 < 5) {
                g.put_pixel(x, y, Luma([235]));
                g.put_pixel(x, y + 1, Luma([235]));
            }
        }
        assert_eq!(classify(&features(&g), true), Scene::Slide);
    }

    /// The camera is moving pictures: it is told by looking at it more than once. A photograph on a slide holds
    /// still, so it is a slide.
    #[test]
    fn a_picture_that_holds_still_is_a_slide_and_one_that_shimmers_is_a_camera() {
        let still = camera(7);
        let mut gate = Gate::new();
        let seen: Vec<Scene> = (0..5).map(|_| gate.look(&still)).collect();
        assert_eq!(seen[..2], [Scene::Camera, Scene::Camera], "not yet known to be still");
        assert_eq!(seen[4], Scene::Slide, "five identical looks: {seen:?}");
        let mut gate = Gate::new();
        let seen: Vec<Scene> = (0..6).map(|k| gate.look(&camera(k))).collect();
        assert!(seen.iter().all(|s| *s == Scene::Camera), "{seen:?}");
    }

    #[test]
    fn a_page_is_a_slide_from_the_first_look_and_the_notice_never_is() {
        let mut gate = Gate::new();
        assert_eq!(gate.look(&page()), Scene::Slide);
        assert_eq!(gate.look(&tile(2)), Scene::Placeholder);
        assert_eq!(gate.look(&page()), Scene::Slide, "back to the page");
        assert_eq!(Scene::Slide.waiting_for(), None);
        assert!(Scene::Camera.waiting_for().is_some() && Scene::Placeholder.waiting_for().is_some());
    }
}
