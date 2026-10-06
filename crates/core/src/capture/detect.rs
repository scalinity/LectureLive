//! Slide change detection (spec §7.2): each sample is cropped to the slide region and shrunk to 256×144
//! grayscale; its 16×16 tiles are compared with the last kept frame. A change becomes a candidate, a
//! candidate that holds still for a sample becomes a slide, tiles that keep moving are masked, and a
//! candidate that never settles is kept after 10 samples, flagged uncertain.
use image::{imageops::FilterType, DynamicImage, GrayImage, RgbaImage};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

pub const W: u32 = 256;
pub const H: u32 = 144;
pub const TILE: u32 = 16;
pub const TILES: usize = ((W / TILE) * (H / TILE)) as usize;
/// A settled frame equal to one of this many kept frames before the current one is the same slide again.
pub const RECENT_KEPT: usize = 3;
/// ...when that frame was on screen within this many samples (about a second each) of now.
pub const RECENT_SAMPLES: u64 = 60;
/// Samples a lecture takes each second. A slide held for 1–2 s was seen twice only half the time at one a
/// second, which is how a lecturer paging through a deck lost slides (M7.1 class); at three it is seen twice
/// from about 0.7 s.
pub const SAMPLE_HZ: u32 = 3;
pub const SAMPLE_INTERVAL: std::time::Duration = std::time::Duration::from_millis(1000 / SAMPLE_HZ as u64);

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

/// Blanks the parts left out (fractions of the region, so they follow the slide when the window's layout
/// changes) in a detector input: a speaker's camera drawn over the slide, which no rectangle can leave out
/// (spec §7.1).
pub fn leave_out(t: &mut GrayImage, parts: &[Region]) {
    // One pixel wider on each side: shrinking the capture blurs a part's edge into its neighbours.
    let span = |at: f64, size: f64, n: u32| {
        let a = at.clamp(0.0, 1.0) * n as f64;
        let b = (at + size).clamp(0.0, 1.0) * n as f64;
        ((a.floor() as u32).saturating_sub(1), (b.ceil() as u32 + 1).min(n))
    };
    for p in parts {
        let (x0, x1) = span(p.x, p.w, W);
        let (y0, y1) = span(p.y, p.h, H);
        for y in y0..y1 {
            for x in x0..x1 {
                t.put_pixel(x, y, image::Luma([0]));
            }
        }
    }
}

/// The detector's input with the parts left out blanked.
pub fn thumb_for(img: &RgbaImage, region: &Region, parts: &[Region]) -> Option<GrayImage> {
    let mut t = thumb(img, region)?;
    leave_out(&mut t, parts);
    Some(t)
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
    /// A candidate is kept once this many samples in a row have shown it still: the picture has finished
    /// sharpening (Zoom sends a share coarse first), so what is saved is the finished slide.
    pub hold: u32,
    /// A settled frame equal to a recently kept one is the same slide again within this many samples.
    pub recent: u64,
}

impl Default for Thresholds {
    /// The values calibrated at one sample a second (M5).
    fn default() -> Self {
        Self { change: 0.05, settle: 0.03, animated: 3, expire: 10, hold: 1, recent: RECENT_SAMPLES }
    }
}

impl Thresholds {
    /// The calibrated values for `hz` samples a second: what counts in seconds stays the same, so each count of
    /// samples grows with the rate, and a slide is kept after about two thirds of a second on screen.
    pub fn at(hz: u32) -> Self {
        let hz = hz.max(1);
        let one = Self::default();
        Self { animated: one.animated * hz, expire: one.expire * hz, hold: (2 * hz / 3).max(1), recent: one.recent * hz as u64, ..one }
    }

    /// What a lecture runs with.
    pub fn live() -> Self {
        Self::at(SAMPLE_HZ)
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
    /// Samples in a row that showed it still.
    still: u32,
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
    /// Samples seen, and the kept frames before the current one with the sample each was replaced at.
    samples: u64,
    recent: VecDeque<(GrayImage, u64)>,
}

impl<T: Clone> Detector<T> {
    pub fn new(t: Thresholds) -> Self {
        Self { t, reference: None, prev: None, candidate: None, moving: [0; TILES], masked: [false; TILES], samples: 0, recent: VecDeque::new() }
    }

    /// Whether `frame` is one of the last few kept frames again, shown within the last minute or so: a
    /// window's control bar fading in and out of the region, or the lecturer stepping back a slide and forward.
    fn seen_recently(&self, frame: &GrayImage) -> bool {
        self.recent.iter().any(|(old, at)| self.samples - at <= self.t.recent && {
            let d = tile_diffs(frame, old);
            (0..TILES).all(|i| self.masked[i] || d[i] <= self.t.change)
        })
    }

    /// The last kept frame: what the slide on screen should look like.
    pub fn kept(&self) -> Option<&GrayImage> {
        self.reference.as_ref()
    }

    /// Whether `frame`, through a region found again after the window changed size, shows the kept slide. The
    /// search realigns to a fraction of a pixel, which still moves fine text by up to about 0.063 in a tile
    /// (M5 full-screen check), so twice the change threshold applies: a new slide moves tiles by far more, and
    /// only a one-line build landing in the same second as the switch is taken for the kept slide.
    pub fn shows_kept(&self, frame: &GrayImage) -> bool {
        self.reference.as_ref().is_some_and(|r| {
            let d = tile_diffs(frame, r);
            (0..TILES).all(|i| self.masked[i] || d[i] <= 2.0 * self.t.change)
        })
    }

    /// Tiles masked as animated.
    pub fn masked(&self) -> usize {
        self.masked.iter().filter(|&&m| m).count()
    }

    /// Makes `frame` the kept frame: a slide taken by hand, or one this detector decided on.
    pub fn keep(&mut self, frame: GrayImage) {
        if let Some(old) = self.reference.replace(frame.clone()) {
            self.recent.push_back((old, self.samples));
            if self.recent.len() > RECENT_KEPT {
                self.recent.pop_front();
            }
        }
        self.prev = Some(frame);
        self.candidate = None;
        self.moving = [0; TILES];
        self.masked = [false; TILES];
    }

    pub fn observe(&mut self, at: T, frame: GrayImage) -> Option<Decision<T>> {
        self.samples += 1;
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
                self.candidate = Some(Candidate { frame, since: at, age: 0, still: 0 });
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
            c.still += 1;
            if c.still < self.t.hold {
                return None;
            }
            let since = c.since.clone();
            let again = self.seen_recently(&frame);
            self.keep(frame);
            return (!again).then_some(Decision { shown_at: since, uncertain: false });
        }
        c.age += 1;
        c.still = 0;
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

    /// Calibration (M5, the deck recorded from a window): one line of real text is about 3 px tall at 256×144
    /// and changes its tiles by about 0.06–0.075, below spec 7.2's first 0.08; a pointer (0.02) stays below.
    #[test]
    fn a_one_line_text_build_is_caught_at_the_calibrated_threshold() {
        let a = page();
        // Text strokes, not a solid bar: a line 3 px tall with every other column inked.
        let mut line = a.clone();
        for x in (16..160).step_by(2) {
            for y in 40..43 {
                line.put_pixel(x, y, Luma([40]));
            }
        }
        let got = run(&mut Detector::new(Thresholds::default()), &[a.clone(), line.clone(), line]);
        assert_eq!(got.len(), 2, "the first frame and the text line: {got:?}");
    }

    #[test]
    fn a_change_that_reverts_before_it_settles_is_not_a_slide() {
        let a = page();
        let flash = with_bar(a.clone(), 0, 0, W, H, 20);
        let got = run(&mut Detector::new(Thresholds::default()), &[a.clone(), flash, a.clone(), a.clone()]);
        assert_eq!(got.len(), 1, "only the first frame: {got:?}");
    }

    /// Live evidence (M6, a Zoom lecture): the window's control bar fades in and out inside the region, and each
    /// stable state of the same slide was kept as a new slide, 24 in five minutes. A frame that matches one of the
    /// last few kept frames is the same slide again: it becomes the reference and is not registered.
    #[test]
    fn a_control_bar_that_comes_and_goes_registers_the_slide_once_per_look_not_every_time() {
        let a = page();
        let bar = with_bar(a.clone(), 0, 128, W, 16, 15); // the toolbar: a dark strip along the bottom
        let (a, bar) = (|| a.clone(), || bar.clone());
        let mut frames = vec![a(), a()];
        for _ in 0..4 {
            frames.extend([bar(), bar(), bar(), a(), a(), a()]);
        }
        let got = run(&mut Detector::new(Thresholds::default()), &frames);
        assert_eq!(got.iter().map(|(i, _)| *i).collect::<Vec<_>>(), vec![0, 3], "the first frame and the bar's first look: {got:?}");
    }

    #[test]
    fn a_frame_matching_an_old_kept_frame_registers_again_once_it_is_no_longer_recent() {
        let a = page();
        let b = with_bar(a.clone(), 16, 32, 120, 6, 40);
        let c = with_bar(a.clone(), 16, 64, 120, 6, 40);
        let (d, e) = (with_bar(a.clone(), 16, 96, 120, 6, 40), with_bar(a.clone(), 16, 112, 120, 6, 40));
        let mut frames = vec![a.clone()];
        for f in [&b, &c, &d, &e] {
            frames.extend([f.clone(), f.clone()]);
        }
        // Three slides ago is still recent: going back to b within the window is the same slide.
        let mut at = Detector::new(Thresholds::default());
        let near = run(&mut at, &[frames.clone(), vec![b.clone(), b.clone()]].concat());
        assert_eq!(near.len(), 5, "a, b, c, d, e, and no sixth for b again: {near:?}");
        // Past the window it is a new look.
        let mut far = Detector::new(Thresholds::default());
        let mut late = frames.clone();
        late.extend(std::iter::repeat(e.clone()).take(RECENT_SAMPLES as usize));
        late.extend([b.clone(), b]);
        assert_eq!(run(&mut far, &late).len(), 6, "b again after the window is registered");
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

    /// Live evidence (M5, a recorded Zoom lecture): the speaker's camera is drawn over the slide, so no
    /// rectangle leaves it out; a part left out never changes the detector's input.
    #[test]
    fn a_part_left_out_never_triggers_and_the_rest_still_does() {
        let region = Region { x: 0.1, y: 0.2, w: 0.5, h: 0.5 };
        // The camera: the region's top-right fifth, in the region's own fractions.
        let camera = Region { x: 0.8, y: 0.0, w: 0.2, h: 0.2 };
        let window = |cam: u8, lines: u32| {
            let mut img = RgbaImage::from_pixel(1000, 600, image::Rgba([245, 245, 245, 255]));
            for y in 120..180 {
                for x in 500..600 {
                    img.put_pixel(x, y, image::Rgba([cam, cam, cam, 255]));
                }
            }
            for l in 0..lines {
                for y in 200 + l * 40..212 + l * 40 {
                    for x in 130..420 {
                        img.put_pixel(x, y, image::Rgba([30, 30, 30, 255]));
                    }
                }
            }
            img
        };
        let seen = |img: &RgbaImage| thumb_for(img, &region, &[camera]).unwrap();
        assert_eq!(seen(&window(40, 1)), seen(&window(200, 1)), "the camera's part is the same whatever it shows");
        let mut d = Detector::new(Thresholds::default());
        let frames: Vec<GrayImage> = [(40, 1), (200, 1), (200, 1), (90, 1), (90, 1), (90, 2), (90, 2)].iter().map(|&(c, l)| seen(&window(c, l))).collect();
        let got = run(&mut d, &frames);
        assert_eq!(got.iter().map(|(i, _)| *i).collect::<Vec<_>>(), vec![0, 6], "the first frame and the build, not the camera: {got:?}");
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

    /// Live evidence (M7.1 class): a lecturer paging through a deck lost slides at one sample a second, since a page
    /// held for a second or two is seen twice only half the time. The live rate is the calibrated one scaled to
    /// three samples a second, and keeps each page held for about a second.
    #[test]
    fn the_live_rate_is_the_calibrated_one_scaled_and_keeps_every_page_held_for_about_a_second() {
        assert_eq!(Thresholds::at(1), Thresholds::default());
        let t = Thresholds::at(SAMPLE_HZ);
        assert_eq!((t.animated, t.expire, t.hold, t.recent), (9, 30, 2, 180));
        assert_eq!(SAMPLE_INTERVAL.as_millis(), 333);
        let pages: Vec<GrayImage> = (0..5).map(|k| with_bar(page(), 16, 20 + k * 24, 120, 6, 40)).collect();
        // Each page on screen for three samples: one second.
        let frames: Vec<GrayImage> = pages.iter().flat_map(|p| [p.clone(), p.clone(), p.clone()]).collect();
        let got = run(&mut Detector::new(t), &frames);
        assert_eq!(got.iter().map(|(i, _)| *i).collect::<Vec<_>>(), vec![0, 5, 8, 11, 14], "the first frame, then each page when it has held: {got:?}");
        // The same five pages, one second each, sampled once a second: only the first is kept.
        assert_eq!(run(&mut Detector::new(Thresholds::default()), &pages).len(), 1);
    }

    /// A slide still sharpening (Zoom sends a share coarse first) is not kept until it has held for `hold` samples,
    /// so what is saved is the finished picture, whatever the rate.
    #[test]
    fn a_change_is_kept_only_after_hold_samples_in_a_row_that_show_it_still() {
        let a = page();
        let b = with_bar(a.clone(), 16, 32, 120, 6, 40);
        let t = Thresholds { hold: 3, ..Thresholds::default() };
        let got = run(&mut Detector::new(t), &[a.clone(), b.clone(), b.clone(), b.clone(), b.clone()]);
        assert_eq!(got.iter().map(|(i, _)| *i).collect::<Vec<_>>(), vec![0, 4], "b is first seen at 1 and kept at its third look after: {got:?}");
        // A sample that differs puts the count back to nothing.
        let c = with_bar(a.clone(), 16, 80, 120, 6, 40);
        let got = run(&mut Detector::new(t), &[a.clone(), b.clone(), b.clone(), c.clone(), c.clone(), c.clone(), c.clone()]);
        assert_eq!(got.iter().map(|(i, _)| *i).collect::<Vec<_>>(), vec![0, 6], "{got:?}");
    }
}
