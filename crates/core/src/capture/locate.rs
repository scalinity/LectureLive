//! Finding the slide again after Zoom re-lays out its window (full screen on, full screen off): the last
//! kept slide is searched for in the new capture, at every size with the slide's own shape.
use image::{imageops::FilterType, DynamicImage, GrayImage, RgbaImage};

use crate::capture::detect::Region;

/// Above this mean absolute difference (0–1) the best place is not the slide.
const FOUND: f64 = 0.06;
/// A slide with less contrast than this (standard deviation, 0–255) would match any plain area.
const CONTRAST: f64 = 12.0;
/// Widths of the window searched first, then around the first answer.
const COARSE: u32 = 192;
const FINE: u32 = 384;

fn gray_at(img: &RgbaImage, width: u32) -> GrayImage {
    let h = (img.height() as f64 * width as f64 / img.width() as f64).round().max(1.0) as u32;
    DynamicImage::ImageLuma8(DynamicImage::ImageRgba8(img.clone()).to_luma8()).resize_exact(width, h, FilterType::Triangle).into_luma8()
}

/// The kept slide at `w`×`h`, and which of its pixels lie in a part left out (not compared).
fn template(kept: &GrayImage, w: u32, h: u32, parts: &[Region]) -> (GrayImage, Vec<bool>) {
    let t = DynamicImage::ImageLuma8(kept.clone()).resize_exact(w, h, FilterType::Triangle).into_luma8();
    let skip = (0..w * h)
        .map(|i| {
            let (fx, fy) = ((i % w) as f64 / w as f64, (i / w) as f64 / h as f64);
            parts.iter().any(|p| fx >= p.x && fx < p.x + p.w && fy >= p.y && fy < p.y + p.h)
        })
        .collect();
    (t, skip)
}

/// Mean absolute difference of the template placed at (`x`, `y`), over every `step`th pixel, 0–1.
fn mad(g: &GrayImage, t: &GrayImage, skip: &[bool], x: u32, y: u32, step: u32) -> f64 {
    let (tw, gw) = (t.width() as usize, g.width() as usize);
    let (gr, tr) = (g.as_raw(), t.as_raw());
    let (mut sum, mut n) = (0u64, 0u64);
    for ty in (0..t.height() as usize).step_by(step as usize) {
        let grow = (y as usize + ty) * gw + x as usize;
        for tx in (0..tw).step_by(step as usize) {
            let i = ty * tw + tx;
            if !skip[i] {
                sum += gr[grow + tx].abs_diff(tr[i]) as u64;
                n += 1;
            }
        }
    }
    if n == 0 { 1.0 } else { sum as f64 / n as f64 / 255.0 }
}

/// Where the kept slide (the detector's last kept frame, `aspect` its width over height in window pixels)
/// now is in `window`, as fractions of it; None unless it is found closely.
pub fn locate(window: &RgbaImage, kept: &GrayImage, aspect: f64, parts: &[Region]) -> Option<Region> {
    let (t0, skip0) = template(kept, kept.width(), kept.height(), parts);
    let seen: Vec<f64> = t0.as_raw().iter().zip(&skip0).filter(|(_, s)| !**s).map(|(v, _)| *v as f64).collect();
    let mean = seen.iter().sum::<f64>() / seen.len().max(1) as f64;
    let sd = (seen.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / seen.len().max(1) as f64).sqrt();
    if sd < CONTRAST || !aspect.is_finite() || aspect <= 0.0 {
        return None;
    }
    let g = gray_at(window, COARSE);
    let mut best: Option<(f64, u32, u32, u32)> = None;
    for w in (COARSE / 4..=COARSE).step_by(2) {
        let h = (w as f64 / aspect).round() as u32;
        if h < 12 || h > g.height() {
            continue;
        }
        let (t, skip) = template(kept, w, h, parts);
        for y in (0..=g.height() - h).step_by(2) {
            for x in (0..=COARSE - w).step_by(2) {
                let d = mad(&g, &t, &skip, x, y, 2);
                if best.is_none_or(|b| d < b.0) {
                    best = Some((d, x, y, w));
                }
            }
        }
    }
    let (_, bx, by, bw) = best?;
    // Near the first answer, at twice the resolution.
    let g2 = gray_at(window, FINE);
    let mut fine: Option<(f64, u32, u32, u32, u32)> = None;
    for w in (bw * 2).saturating_sub(4)..=(bw * 2 + 4).min(FINE) {
        let h = (w as f64 / aspect).round() as u32;
        if h == 0 || h > g2.height() {
            continue;
        }
        let (t, skip) = template(kept, w, h, parts);
        for y in (by * 2).saturating_sub(4)..=(by * 2 + 4).min(g2.height() - h) {
            for x in (bx * 2).saturating_sub(4)..=(bx * 2 + 4).min(FINE - w) {
                let d = mad(&g2, &t, &skip, x, y, 1);
                if fine.is_none_or(|f| d < f.0) {
                    fine = Some((d, x, y, w, h));
                }
            }
        }
    }
    let (d, x, y, w, h) = fine?;
    let (fw, fh) = (FINE as f64, g2.height() as f64);
    (d < FOUND).then(|| Region { x: x as f64 / fw, y: y as f64 / fh, w: w as f64 / fw, h: h as f64 / fh })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::detect::{thumb, Region};
    use image::{Rgba, RgbaImage};

    /// A dark window with a slide (title, bullets, a diagram) drawn into `at` (x, y, w, h in pixels).
    fn window(w: u32, h: u32, at: (u32, u32, u32, u32)) -> RgbaImage {
        let mut img = RgbaImage::from_pixel(w, h, Rgba([28, 28, 30, 255]));
        let (sx, sy, sw, sh) = at;
        let mut fill = |x0: f64, y0: f64, x1: f64, y1: f64, v: u8| {
            for y in (sy as f64 + y0 * sh as f64) as u32..(sy as f64 + y1 * sh as f64) as u32 {
                for x in (sx as f64 + x0 * sw as f64) as u32..(sx as f64 + x1 * sw as f64) as u32 {
                    img.put_pixel(x, y, Rgba([v, v, v, 255]));
                }
            }
        };
        fill(0.0, 0.0, 1.0, 1.0, 250);
        fill(0.06, 0.07, 0.55, 0.15, 25);
        for l in 0..4 {
            let y = 0.25 + l as f64 * 0.1;
            fill(0.08, y, 0.38 + l as f64 * 0.1, y + 0.045, 45);
        }
        fill(0.70, 0.30, 0.92, 0.75, 110);
        fill(0.74, 0.40, 0.88, 0.48, 20);
        img
    }

    #[test]
    fn the_slide_is_found_where_full_screen_put_it() {
        let before = window(1600, 900, (200, 100, 1200, 675));
        let region = Region { x: 200.0 / 1600.0, y: 100.0 / 900.0, w: 1200.0 / 1600.0, h: 675.0 / 900.0 };
        let kept = thumb(&before, &region).unwrap();
        let after = window(1920, 1200, (240, 240, 1440, 810));
        let found = locate(&after, &kept, 1200.0 / 675.0, &[]).expect("found");
        let want = Region { x: 240.0 / 1920.0, y: 240.0 / 1200.0, w: 1440.0 / 1920.0, h: 810.0 / 1200.0 };
        for (got, want) in [(found.x, want.x), (found.y, want.y), (found.w, want.w), (found.h, want.h)] {
            assert!((got - want).abs() < 0.01, "found {found:?}, want {want:?}");
        }
    }

    #[test]
    fn nothing_is_found_when_the_slide_is_not_there() {
        let before = window(1600, 900, (200, 100, 1200, 675));
        let region = Region { x: 200.0 / 1600.0, y: 100.0 / 900.0, w: 1200.0 / 1600.0, h: 675.0 / 900.0 };
        let kept = thumb(&before, &region).unwrap();
        let gallery = RgbaImage::from_fn(1920, 1200, |x, y| if (x / 480 + y / 400) % 2 == 0 { Rgba([60, 70, 80, 255]) } else { Rgba([28, 28, 30, 255]) });
        assert_eq!(locate(&gallery, &kept, 1200.0 / 675.0, &[]), None);
    }
}
