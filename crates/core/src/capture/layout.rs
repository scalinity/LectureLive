//! Where the shared content is in a window (spec §7.1), read from the window alone: Zoom's own screen is one
//! flat dark that runs out to the window's edges (its bars, its margins, the gaps between the speaker tiles),
//! and what is shared is the largest rectangle that is not that. Text inside a slide can be the same dark, but
//! it never reaches the edge of the window through dark, so only the dark that does is Zoom's.
//!
//! This is what keeps the slide region true after Zoom re-lays out its window (full screen on or off, a
//! speaker strip appearing): the region is checked against it as the lecture goes on, so a region found
//! wrongly at a moment when the window was still moving cannot stay wrong.
use image::{imageops::FilterType, DynamicImage, GrayImage, RgbaImage};

use crate::capture::detect::Region;

/// Width the window is read at: wide enough for a slide's edges, small enough to read often.
const WIDTH: u32 = 160;
/// Zoom's own dark is no brighter than this.
const CHROME_MAX: usize = 70;
/// Levels either side of the dark's own level that are still it.
const CHROME_BAND: i32 = 6;
/// The dark must cover at least this share of the window to be Zoom's and not a shadow in a picture.
const CHROME_SHARE: f32 = 0.03;
/// Shared content smaller than this share of the window is a tile or an icon, not the share.
const MIN_AREA: f64 = 0.20;
/// At least this share of a region must be shared content: more than that is Zoom's strip or bars, whose
/// moving pictures would read as slide changes.
const REGION_INSIDE: f64 = 0.80;
/// A region may leave out up to half the content (a camera drawn over it, a control bar), no more: less is a corner.
const CONTENT_COVERED: f64 = 0.50;

/// The largest rectangle in the window that is not Zoom's own dark, as fractions of it; None when the window
/// has no such dark to tell it from (a slide that fills the whole window) or nothing large is left.
pub fn content(window: &RgbaImage) -> Option<Region> {
    let h = ((window.height() as f64 * WIDTH as f64 / window.width().max(1) as f64).round() as u32).max(8);
    // DynamicImage's own methods: compiled, and optimised, inside `image`.
    let g = DynamicImage::ImageLuma8(DynamicImage::ImageRgba8(window.clone()).to_luma8()).resize_exact(WIDTH, h, FilterType::Triangle).into_luma8();
    content_of(&g)
}

pub fn content_of(g: &GrayImage) -> Option<Region> {
    let (w, h) = (g.width() as usize, g.height() as usize);
    let px = g.as_raw();
    let mut hist = [0u32; 256];
    for &v in px {
        hist[v as usize] += 1;
    }
    let near = |l: usize| hist[l.saturating_sub(3)..=(l + 3).min(255)].iter().sum::<u32>();
    let dark = (0..=CHROME_MAX).max_by_key(|&l| near(l))?;
    if (near(dark) as f32) < CHROME_SHARE * px.len() as f32 {
        return None;
    }
    let is_dark = |i: usize| (px[i] as i32 - dark as i32).abs() <= CHROME_BAND;
    // Zoom's dark: what reaches the window's edge through dark.
    let mut chrome = vec![false; w * h];
    let mut todo: Vec<usize> = (0..w).chain((1..h).map(|y| y * w)).chain((1..h).map(|y| y * w + w - 1)).chain((1..w).map(|x| (h - 1) * w + x)).filter(|&i| is_dark(i)).collect();
    for &i in &todo {
        chrome[i] = true;
    }
    while let Some(i) = todo.pop() {
        let (x, y) = (i % w, i / w);
        for j in [(x > 0).then(|| i - 1), (x + 1 < w).then(|| i + 1), (y > 0).then(|| i - w), (y + 1 < h).then(|| i + w)].into_iter().flatten() {
            if !chrome[j] && is_dark(j) {
                chrome[j] = true;
                todo.push(j);
            }
        }
    }
    let shared: Vec<bool> = chrome.iter().map(|c| !c).collect();
    let (x, y, rw, rh) = largest_rect(&shared, w, h)?;
    // A cell in from each side that meets Zoom's dark: the edge's anti-aliasing is neither slide nor dark.
    let (left, top) = (usize::from(x > 0), usize::from(y > 0));
    let (right, bottom) = (usize::from(x + rw < w), usize::from(y + rh < h));
    let (x, y, rw, rh) = (x + left, y + top, rw.saturating_sub(left + right).max(1), rh.saturating_sub(top + bottom).max(1));
    let region = Region { x: x as f64 / w as f64, y: y as f64 / h as f64, w: rw as f64 / w as f64, h: rh as f64 / h as f64 };
    // All of the window is no finding: no dark of Zoom's reached its edge, as when a black slide fills most of it.
    let whole = region.w > 0.97 && region.h > 0.97;
    (region.w * region.h >= MIN_AREA && !whole).then_some(region)
}

/// The largest rectangle of true cells: (x, y, width, height).
fn largest_rect(mask: &[bool], w: usize, h: usize) -> Option<(usize, usize, usize, usize)> {
    let mut heights = vec![0usize; w];
    let mut best = (0usize, (0usize, 0usize, 0usize, 0usize));
    for y in 0..h {
        for x in 0..w {
            heights[x] = if mask[y * w + x] { heights[x] + 1 } else { 0 };
        }
        let mut stack: Vec<usize> = Vec::new();
        for x in 0..=w {
            let here = if x == w { 0 } else { heights[x] };
            while let Some(&top) = stack.last() {
                if heights[top] <= here {
                    break;
                }
                stack.pop();
                let left = stack.last().map_or(0, |&l| l + 1);
                let (width, height) = (x - left, heights[top]);
                if width * height > best.0 {
                    best = (width * height, (left, y + 1 - height, width, height));
                }
            }
            stack.push(x);
        }
    }
    (best.0 > 0).then_some(best.1)
}

fn overlap(a: &Region, b: &Region) -> f64 {
    let (w, h) = ((a.x + a.w).min(b.x + b.w) - a.x.max(b.x), (a.y + a.h).min(b.y + b.h) - a.y.max(b.y));
    if w > 0.0 && h > 0.0 { w * h } else { 0.0 }
}

/// Whether `region` is looking at `content`. A region that leaves out a control bar or a camera is inside it and
/// covers most of it; one that looks at a corner of the slide, or takes in Zoom's strip or bars, does not.
pub fn agrees(region: &Region, content: &Region) -> bool {
    let both = overlap(region, content);
    both >= REGION_INSIDE * region.w * region.h && both >= CONTENT_COVERED * content.w * content.h
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Luma, Rgba};

    /// A Zoom window: dark all over, a row of speaker tiles (video, with dark gaps between), and the slide (a
    /// light page with black text, the same dark as Zoom's own) in `at` (x, y, w, h in pixels).
    fn window(w: u32, h: u32, strip: bool, at: (u32, u32, u32, u32)) -> RgbaImage {
        let mut img = RgbaImage::from_pixel(w, h, Rgba([28, 28, 30, 255]));
        if strip {
            for t in 0..6 {
                for y in 8..h / 8 {
                    for x in w / 12 + t * (w / 7)..w / 12 + t * (w / 7) + w / 9 {
                        img.put_pixel(x, y, Rgba([90 + (x % 17) as u8, 110, 130 + (y % 11) as u8, 255]));
                    }
                }
            }
        }
        let (sx, sy, sw, sh) = at;
        for y in sy..(sy + sh).min(h) {
            for x in sx..(sx + sw).min(w) {
                img.put_pixel(x, y, Rgba([246, 246, 246, 255]));
            }
        }
        for l in 0..5 {
            for y in sy + sh / 8 + l * sh / 8..sy + sh / 8 + l * sh / 8 + sh / 20 {
                for x in sx + sw / 12..sx + sw / 12 + sw * (4 + l % 3) / 10 {
                    img.put_pixel(x, y, Rgba([28, 28, 30, 255])); // text as dark as Zoom's own
                }
            }
        }
        img
    }

    fn near(got: Region, want: (f64, f64, f64, f64)) {
        for (g, w) in [(got.x, want.0), (got.y, want.1), (got.w, want.2), (got.h, want.3)] {
            assert!((g - w).abs() < 0.03, "got {got:?}, want {want:?}");
        }
    }

    #[test]
    fn the_slide_is_the_content_with_or_without_the_speaker_strip_and_its_black_text_is_not_zooms_dark() {
        near(content(&window(1168, 733, true, (76, 160, 1016, 573))).unwrap(), (76.0 / 1168.0, 160.0 / 733.0, 1016.0 / 1168.0, 573.0 / 733.0));
        // Full screen: no strip, the slide fills the width between two bars.
        near(content(&window(1168, 729, false, (0, 50, 1168, 640))).unwrap(), (0.0, 50.0 / 729.0, 1.0, 640.0 / 729.0));
    }

    #[test]
    fn a_dark_slide_that_differs_from_zooms_dark_is_content_and_one_that_fills_the_window_has_nothing_to_tell_it_from() {
        let black = |slide: (u32, u32, u32, u32)| {
            let mut img = window(1000, 600, false, slide);
            for y in slide.1..slide.1 + slide.3 {
                for x in slide.0..slide.0 + slide.2 {
                    img.put_pixel(x, y, Rgba([0, 0, 0, 255]));
                }
            }
            img
        };
        near(content(&black((250, 150, 500, 300))).unwrap(), (0.25, 0.25, 0.5, 0.5));
        // Most of the window is black: that, not the margin, is the commonest dark, and no dark reaches the edge.
        assert_eq!(content(&black((100, 60, 800, 450))), None, "no finding, so no opinion");
        let all_white = RgbaImage::from_pixel(800, 450, Rgba([250, 250, 250, 255]));
        assert_eq!(content(&all_white), None, "no dark of Zoom's to tell it from");
    }

    #[test]
    fn a_region_agrees_with_the_content_it_looks_at_and_not_with_a_corner_of_it_or_with_the_bars() {
        let content = Region { x: 0.0, y: 0.07, w: 1.0, h: 0.88 };
        assert!(agrees(&Region { x: 0.003, y: 0.05, w: 0.995, h: 0.896 }, &content), "a little loose");
        assert!(agrees(&Region { x: 0.0, y: 0.07, w: 1.0, h: 0.78 }, &content), "the control bar left out");
        // The corner a mistaken search chose in a real class: 30% by 27% at the bottom right.
        assert!(!agrees(&Region { x: 0.698, y: 0.717, w: 0.302, h: 0.271 }, &content));
        assert!(!agrees(&Region { x: 0.065, y: 0.218, w: 0.87, h: 0.782 }, &Region { x: 0.0, y: 0.8, w: 1.0, h: 0.2 }), "elsewhere");
        // The full-screen region used back in the window, where it takes in the speaker strip above the slide.
        let windowed = Region { x: 0.062, y: 0.21, w: 0.87, h: 0.79 };
        assert!(!agrees(&Region { x: 0.0, y: 0.07, w: 1.0, h: 0.93 }, &windowed));
        assert!(agrees(&Region { x: 0.0648, y: 0.2184, w: 0.8707, h: 0.7816 }, &windowed), "the person's own region");
    }

    #[test]
    fn the_largest_rectangle_is_found_among_notches() {
        let mut g = GrayImage::from_pixel(40, 30, Luma([30]));
        for y in 5..25 {
            for x in 4..36 {
                g.put_pixel(x, y, Luma([200]));
            }
        }
        for y in 5..9 {
            for x in 4..10 {
                g.put_pixel(x, y, Luma([30])); // a notch the border dark reaches
            }
        }
        let shared: Vec<bool> = g.as_raw().iter().map(|&v| v > 100).collect();
        let (x, y, w, h) = largest_rect(&shared, 40, 30).unwrap();
        assert!(w * h >= 32 * 16, "{x},{y} {w}×{h}");
    }
}
