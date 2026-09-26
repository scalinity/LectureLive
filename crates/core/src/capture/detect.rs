//! Slide change detection (spec §7.2): each sample is cropped to the slide region and shrunk to 256×144
//! grayscale; its 16×16 tiles are compared with the last kept frame. A change becomes a candidate, a
//! candidate that holds still for a sample becomes a slide, tiles that keep moving are masked, and a
//! candidate that never settles is kept after 10 samples, flagged uncertain.
use image::{imageops::FilterType, DynamicImage, GrayImage, RgbaImage};
use serde::{Deserialize, Serialize};

pub const W: u32 = 256;
pub const H: u32 = 144;
pub const TILE: u32 = 16;
pub const TILES: usize = ((W / TILE) * (H / TILE)) as usize;

/// The slide's rectangle as fractions of the window, so it holds at any capture scale (spec §7.1).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Region {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Region {
    pub const WHOLE: Region = Region { x: 0.0, y: 0.0, w: 1.0, h: 1.0 };

    /// Inside the window and with an area.
    pub fn is_valid(&self) -> bool {
        [self.x, self.y, self.w, self.h].iter().all(|v| v.is_finite()) && self.x >= 0.0 && self.y >= 0.0 && self.w > 0.0 && self.h > 0.0 && self.x + self.w <= 1.0 + 1e-9 && self.y + self.h <= 1.0 + 1e-9
    }

    /// The rectangle in pixels of an image this size, clamped inside it; None when it has no area.
    pub fn pixels(&self, width: u32, height: u32) -> Option<(u32, u32, u32, u32)> {
        let px = |v: f64, n: u32| (v.clamp(0.0, 1.0) * n as f64).round() as u32;
        let (x0, y0) = (px(self.x, width), px(self.y, height));
        let (x1, y1) = (px(self.x + self.w, width), px(self.y + self.h, height));
        (x1 > x0 && y1 > y0).then(|| (x0, y0, x1 - x0, y1 - y0))
    }
}

// The pixel work goes through `DynamicImage`'s own methods: they are compiled inside `image`, which the
// dev profile optimises, where the generic `imageops` functions would be compiled here, unoptimised.

/// The region at full resolution: what a slide saves.
pub fn crop(img: &RgbaImage, region: &Region) -> Option<DynamicImage> {
    let (x, y, w, h) = region.pixels(img.width(), img.height())?;
    Some(DynamicImage::ImageRgba8(img.clone()).crop_imm(x, y, w, h))
}

/// The detector's input: the region, 256×144, grayscale. None for an invalid frame (zero size, or all
/// black), which is a failed capture rather than a slide.
pub fn thumb(img: &RgbaImage, region: &Region) -> Option<GrayImage> {
    let gray = DynamicImage::ImageLuma8(crop(img, region)?.to_luma8());
    let small = gray.resize_exact(W, H, FilterType::Triangle).into_luma8();
    (!small.as_raw().iter().all(|&v| v < 8)).then_some(small)
}

/// Spec §7.2's values, calibrated on recorded Zoom frames (M5 findings).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Thresholds {
    /// A tile changed when its mean absolute difference from the kept frame exceeds this (0–1).
    pub change: f32,
    /// A tile holds still when it differs from the last sample by less than this.
    pub settle: f32,
    /// A tile moving on this many samples in a row is masked until the next kept slide.
    pub animated: u32,
    /// A candidate unsettled for this many samples is kept anyway, flagged uncertain.
    pub expire: u32,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self { change: 0.08, settle: 0.03, animated: 3, expire: 10 }
    }
}

/// A frame to keep as a slide: first on screen at `shown_at`.
#[derive(Debug, Clone, PartialEq)]
pub struct Decision<T> {
    pub shown_at: T,
    pub uncertain: bool,
}

struct Candidate<T> {
    frame: GrayImage,
    since: T,
    age: u32,
}

/// Mean absolute difference of each 16×16 tile, 0–1.
fn tile_diffs(a: &GrayImage, b: &GrayImage) -> [f32; TILES] {
    let (a, b) = (a.as_raw(), b.as_raw());
    let cols = (W / TILE) as usize;
    let mut sums = [0u32; TILES];
    for y in 0..H as usize {
        let row = y * W as usize;
        let tile_row = (y / TILE as usize) * cols;
        for x in 0..W as usize {
            sums[tile_row + x / TILE as usize] += a[row + x].abs_diff(b[row + x]) as u32;
        }
    }
    sums.map(|s| s as f32 / (TILE * TILE) as f32 / 255.0)
}

/// `T` is when a sample was taken: a wall-clock time in the app, a sample time in tests.
pub struct Detector<T> {
    t: Thresholds,
    reference: Option<GrayImage>,
    prev: Option<GrayImage>,
    candidate: Option<Candidate<T>>,
    moving: [u32; TILES],
    masked: [bool; TILES],
}

impl<T: Clone> Detector<T> {
    pub fn new(t: Thresholds) -> Self {
        Self { t, reference: None, prev: None, candidate: None, moving: [0; TILES], masked: [false; TILES] }
    }

    /// Tiles masked as animated.
    pub fn masked(&self) -> usize {
        self.masked.iter().filter(|&&m| m).count()
    }

    /// Makes `frame` the kept frame: a slide taken by hand, or one this detector decided on.
    pub fn keep(&mut self, frame: GrayImage) {
        self.reference = Some(frame.clone());
        self.prev = Some(frame);
        self.candidate = None;
        self.moving = [0; TILES];
        self.masked = [false; TILES];
    }

    pub fn observe(&mut self, at: T, frame: GrayImage) -> Option<Decision<T>> {
        let (Some(reference), Some(prev)) = (&self.reference, &self.prev) else {
            self.keep(frame);
            return Some(Decision { shown_at: at, uncertain: false }); // the first valid frame of a session
        };
        let d_ref = tile_diffs(&frame, reference);
        let d_prev = tile_diffs(&frame, prev);
        for i in 0..TILES {
            if d_prev[i] > self.t.settle {
                self.moving[i] += 1;
                if self.moving[i] >= self.t.animated {
                    self.masked[i] = true;
                }
            } else {
                self.moving[i] = 0;
            }
        }
        let changed: Vec<usize> = (0..TILES).filter(|&i| !self.masked[i] && d_ref[i] > self.t.change).collect();
        self.prev = Some(frame.clone());
        let Some(c) = self.candidate.as_mut() else {
            if !changed.is_empty() {
                self.candidate = Some(Candidate { frame, since: at, age: 0 });
            }
            return None;
        };
        if changed.is_empty() {
            self.candidate = None; // it reverted, or only animated tiles changed
            return None;
        }
        // Settled: nothing unmasked moved since the candidate. Checking only the tiles that differ from the
        // kept frame would let an animation's quieter tiles pass for a still frame.
        let d_cand = tile_diffs(&frame, &c.frame);
        if (0..TILES).all(|i| self.masked[i] || d_cand[i] < self.t.settle) {
            let since = c.since.clone();
            self.keep(frame);
            return Some(Decision { shown_at: since, uncertain: false });
        }
        c.age += 1;
        c.frame = frame.clone();
        if c.age >= self.t.expire {
            let since = c.since.clone();
            self.keep(frame);
            return Some(Decision { shown_at: since, uncertain: true });
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{GrayImage, Luma, RgbaImage};

    fn page() -> GrayImage {
        GrayImage::from_pixel(W, H, Luma([245]))
    }

    /// A dark bar over tiles: a bullet line, a build.
    fn with_bar(mut f: GrayImage, x0: u32, y0: u32, w: u32, h: u32, v: u8) -> GrayImage {
        for y in y0..y0 + h {
            for x in x0..x0 + w {
                f.put_pixel(x, y, Luma([v]));
            }
        }
        f
    }

    fn run(d: &mut Detector<u32>, frames: &[GrayImage]) -> Vec<(u32, Decision<u32>)> {
        frames.iter().enumerate().filter_map(|(i, f)| d.observe(i as u32, f.clone()).map(|x| (i as u32, x))).collect()
    }

    #[test]
    fn the_first_frame_is_kept_and_a_settled_build_is_confirmed_on_the_next_sample() {
        let a = page();
        let b = with_bar(a.clone(), 16, 32, 120, 6, 40); // one bullet line
        let got = run(&mut Detector::new(Thresholds::default()), &[a.clone(), a.clone(), b.clone(), b.clone(), b.clone()]);
        assert_eq!(got, vec![(0, Decision { shown_at: 0, uncertain: false }), (3, Decision { shown_at: 2, uncertain: false })]);
    }

    #[test]
    fn a_change_that_reverts_before_it_settles_is_not_a_slide() {
        let a = page();
        let flash = with_bar(a.clone(), 0, 0, W, H, 20);
        let got = run(&mut Detector::new(Thresholds::default()), &[a.clone(), flash, a.clone(), a.clone()]);
        assert_eq!(got.len(), 1, "only the first frame: {got:?}");
    }

    #[test]
    fn a_looping_animation_is_masked_and_neither_triggers_nor_blocks() {
        let a = page();
        // A loop sampled once a second alternates between two frames.
        let spin = |k: u32| with_bar(page(), 200 + (k % 2) * 16, 100, 16, 16, 30);
        let mut frames = vec![a.clone()];
        for k in 0..8 {
            frames.push(spin(k));
        }
        let mut d = Detector::new(Thresholds::default());
        assert_eq!(run(&mut d, &frames).len(), 1, "the animation alone captures nothing");
        assert!(d.masked() > 0);
        // A build elsewhere while it loops is still confirmed.
        let build = |k: u32| with_bar(spin(k), 16, 32, 120, 6, 40);
        let got: Vec<_> = (8..11).filter_map(|k| d.observe(k, build(k))).collect();
        assert_eq!(got, vec![Decision { shown_at: 8, uncertain: false }]);
        assert_eq!(d.masked(), 0, "masks last until the next confirmed slide");
    }

    #[test]
    fn a_candidate_that_never_settles_is_kept_uncertain_after_the_expiry() {
        let a = page();
        // A large area changing every sample, faster than the mask can settle it: a video.
        let video = |k: u32| with_bar(page(), 0, 0, W, H, (40 + (k * 37) % 150) as u8);
        let mut frames = vec![a];
        for k in 0..12 {
            frames.push(video(k));
        }
        let t = Thresholds { animated: 100, ..Thresholds::default() }; // no masking, to reach the expiry
        let got = run(&mut Detector::new(t), &frames);
        assert_eq!(got.last().unwrap(), &(11, Decision { shown_at: 1, uncertain: true }), "{got:?}");
    }

    #[test]
    fn a_small_change_below_the_threshold_is_ignored_and_a_kept_frame_moves_the_reference() {
        let a = page();
        let cursor = with_bar(a.clone(), 100, 60, 2, 3, 20); // a pointer
        let mut d = Detector::new(Thresholds::default());
        assert_eq!(run(&mut d, &[a.clone(), cursor.clone(), cursor.clone()]).len(), 1);
        let b = with_bar(a.clone(), 16, 32, 120, 6, 40);
        d.keep(b.clone()); // a manual capture of b
        assert_eq!(d.observe(3, b.clone()), None);
        assert_eq!(d.observe(4, b), None, "b is the kept frame: not captured again");
    }

    #[test]
    fn thumb_crops_the_region_and_rejects_black_and_empty_regions() {
        let mut img = RgbaImage::from_pixel(1000, 600, image::Rgba([250, 250, 250, 255]));
        for y in 0..600 {
            for x in 0..200 {
                img.put_pixel(x, y, image::Rgba([0, 0, 0, 255])); // a black sidebar
            }
        }
        let t = thumb(&img, &Region { x: 0.2, y: 0.0, w: 0.8, h: 1.0 }).unwrap();
        assert_eq!(t.dimensions(), (W, H));
        assert!(t.pixels().all(|p| p.0[0] > 200), "the sidebar is outside the region");
        assert!(thumb(&img, &Region { x: 0.0, y: 0.0, w: 0.2, h: 1.0 }).is_none(), "all black");
        assert!(thumb(&img, &Region { x: 0.5, y: 0.5, w: 0.0, h: 0.5 }).is_none(), "zero size");
        assert_eq!(Region { x: 0.25, y: 0.5, w: 0.5, h: 0.5 }.pixels(1000, 600), Some((250, 300, 500, 300)));
        assert_eq!(Region { x: 0.9, y: 0.9, w: 0.5, h: 0.5 }.pixels(1000, 600), Some((900, 540, 100, 60)), "clamped inside");
    }
}
