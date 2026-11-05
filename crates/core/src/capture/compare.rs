//! How two saved slides relate (spec §7.7): one an earlier state of the other (a build step, a frame of a fade), the
//! same picture, or neither. Measured on the detector's own 256×144 grayscale, so its numbers and `scene`'s apply.
//!
//! Ink is an edge that stands off its 16×16 tile's page level: text, rules, the borders of boxes, never the inside of a
//! flat fill. A slide holds another's ink when, wherever the other has
//! ink, it has ink in the same direction at least as strong: a reveal adds ink on blank page, a fade strengthens ink
//! already there, and neither removes any. Replaced text, a new title or another slide leave ink uncovered.
use image::{DynamicImage, GrayImage};

use crate::capture::detect::{self, Region, H, TILE, TILES, W};
use crate::capture::scene::{self, Features};

/// A pixel further than this from its tile's page level is ink. Zoom's re-encoding moves a pixel by less.
const INK: i16 = 32;
/// Ink is still held when the other picture has it this much fainter: anti-aliasing and re-encoding differ.
const SLACK: i16 = 32;
/// Zoom's control bar, drawn over the bottom of the share while the pointer is over the window: a band of one dark
/// level across the bottom with its icons and labels in it, and a step in level above it (M7.6 class: rows 130–143 of
/// 144 at level 22, over light and dark slides alike).
const BAR_LEVEL: u8 = 40;
const BAR_ROWS: std::ops::RangeInclusive<u32> = 5..=22;
/// PowerPoint's slideshow toolbar, drawn at the bottom-left while the presenter moves the mouse, as fractions of the
/// slide. Neither picture's ink counts there.
const TOOLBAR_W: f32 = 0.17;
const TOOLBAR_H: f32 = 0.06;
/// Ink of one picture the other may lack and still hold it, as a share of the frame: a pointer, a caret. Calibrated on a
/// lecture pruned by hand (M7.6): true build steps lost 0–0.12 %, distinct slides at least 0.67 %; a panel drawn over
/// the end of a title rule, or re-encoding twins, lost 0.30–0.44 % and are left for the person.
pub const LOST: f32 = 0.0025;
/// A difference smaller than this either way is the same picture.
pub const NOISE: f32 = 0.0005;
/// Tile means further apart than this on average are never the same picture, so `relate` is skipped.
const NEAR: f32 = 0.05;
/// Two camera frames of one room share most of their pixels; a cut to another picture does not.
const SAME_VIEW: f32 = 0.3;

/// A saved slide as the comparison sees it.
pub struct Look {
    gray: GrayImage,
    page: [u8; TILES],
    /// Pixels with something sharply different within one pixel.
    edge: Vec<bool>,
    /// Tiles with any edge in them: a flat fill shows nothing, whatever its level.
    busy: [bool; TILES],
    /// The first row of a band that looks like Zoom's control bar. Only a look: a slide's own dark footer can look the
    /// same, so the band is Zoom's only once the lecture shows it over different slides (`same_band`), and comparisons
    /// take it into account only when told so (`relate_with`).
    pub bar: Option<u32>,
    tiles: [f32; TILES],
    /// Pixels of ink, outside the toolbar's corner.
    pub ink: u32,
    pub features: Features,
}

/// What `b` is to `a`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Relation {
    /// Share of the frame where `a` has ink that `b` does not hold.
    pub lost: f32,
    /// Share of the frame where `b` has ink that `a` does not hold.
    pub added: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Order {
    /// `a` is an earlier state of `b`: `b` holds everything `a` shows, and more.
    Earlier,
    Same,
    /// `b` is an earlier state of `a`.
    Later,
    Apart,
}

fn toolbar(x: u32, y: u32) -> bool {
    x < (W as f32 * TOOLBAR_W) as u32 && y >= H - (H as f32 * TOOLBAR_H).ceil() as u32
}

fn tile_of(x: u32, y: u32) -> usize {
    ((y / TILE) * (W / TILE) + x / TILE) as usize
}

impl Look {
    /// A detector-sized grayscale frame.
    pub fn of(gray: GrayImage) -> Self {
        assert_eq!(gray.dimensions(), (W, H), "a look is taken at the detector's size");
        let mut hist = vec![[0u16; 256]; TILES];
        let mut sums = [0u32; TILES];
        for (x, y, p) in gray.enumerate_pixels() {
            hist[tile_of(x, y)][p.0[0] as usize] += 1;
            sums[tile_of(x, y)] += p.0[0] as u32;
        }
        let half = (TILE * TILE / 2) as u16;
        let page: [u8; TILES] = std::array::from_fn(|t| {
            let mut seen = 0u16;
            (0..256).find(|&v| {
                seen += hist[t][v];
                seen > half
            }).unwrap_or(0) as u8
        });
        let tiles = std::array::from_fn(|t| sums[t] as f32 / (TILE * TILE) as f32 / 255.0);
        let busy = std::array::from_fn(|t| {
            let lo = (0..256).find(|&v| hist[t][v] > 0).unwrap_or(0);
            let hi = (0..256).rev().find(|&v| hist[t][v] > 0).unwrap_or(0);
            hi - lo > INK as usize
        });
        let px = gray.as_raw();
        let edge: Vec<bool> = (0..(W * H) as usize)
            .map(|i| {
                let (x, y) = (i as u32 % W, i as u32 / W);
                let (mut lo, mut hi) = (255u8, 0u8);
                for ny in y.saturating_sub(1)..=(y + 1).min(H - 1) {
                    for nx in x.saturating_sub(1)..=(x + 1).min(W - 1) {
                        let v = px[(ny * W + nx) as usize];
                        lo = lo.min(v);
                        hi = hi.max(v);
                    }
                }
                (hi - lo) as i16 > INK
            })
            .collect();
        let bar = zoom_bar(&gray);
        let mut look = Self { gray: gray.clone(), page, edge, busy, bar, tiles, ink: 0, features: scene::features(&gray) };
        let below = bar.unwrap_or(H);
        look.ink = (0..below).flat_map(|y| (0..W).map(move |x| (x, y))).filter(|&(x, y)| !toolbar(x, y) && look.ink_at(x, y) != 0).count() as u32;
        look
    }

    /// A slide image; None for an empty or all-black one, which is never compared.
    pub fn from_image(img: &DynamicImage) -> Option<Self> {
        detect::thumb(&img.to_rgba8(), &Region::WHOLE).map(Self::of)
    }

    /// A slide file; None when it cannot be read as an image.
    pub fn load(path: &std::path::Path) -> Option<Self> {
        Self::from_image(&image::open(path).ok()?)
    }

    /// How far the pixel stands off its tile's page when it is ink, else 0.
    fn ink_at(&self, x: u32, y: u32) -> i16 {
        let i = (y * W + x) as usize;
        let d = self.gray.as_raw()[i] as i16 - self.page[tile_of(x, y)] as i16;
        if d.abs() > INK && self.edge[i] { d } else { 0 }
    }

    /// A camera's picture or a photograph: gradients and grain, no page most of it shares.
    pub fn photographic(&self) -> bool {
        scene::photographic(&self.features)
    }

    /// The detector's 256×144 frame.
    pub fn gray(&self) -> &GrayImage {
        &self.gray
    }
}

/// Where Zoom's control bar starts, if it is drawn: from the bottom up, rows of one dark level (each mostly that level,
/// its icons and labels the rest), between 5 and 22 rows of them, some with marks, over a slide of another level. A dark
/// slide's own bottom is its background's level, however much footer text it holds; a light one is not dark. The rows
/// the bar's edge blends into are the bar's too.
fn zoom_bar(g: &GrayImage) -> Option<u32> {
    let row = |y: u32| {
        let mut r: Vec<u8> = (0..W).map(|x| g.get_pixel(x, y).0[0]).collect();
        r.sort_unstable();
        let level = r[W as usize / 2];
        (level, r.iter().filter(|v| v.abs_diff(level) <= 6).count() as f32 / W as f32)
    };
    let (level, _) = row(H - 1);
    if level > BAR_LEVEL {
        return None;
    }
    let mut top = H - 1;
    while top > 0 {
        let (l, even) = row(top - 1);
        if l.abs_diff(level) > 4 || even < 0.7 {
            break;
        }
        top -= 1;
    }
    if !BAR_ROWS.contains(&(H - top)) || !(top..H).any(|y| row(y).1 < 0.95) || top < 8 {
        return None;
    }
    let mut above: Vec<u8> = (top - 8..top - 2).map(|y| row(y).0).collect();
    above.sort_unstable();
    (above[above.len() / 2].abs_diff(level) > 8).then_some(top - 2)
}

/// Pixels where `a` has ink that `b` does not hold. `b` holds it where it has an edge at that pixel too and either the
/// same level there, or, as a fade does, ink of its own (against its own page) in the same direction and at least as strong.
/// Showing something matters: a flat dark frame has dark text's level everywhere, and white text on a dark divider is
/// "lighter" than its page as every pixel of a white slide is; neither shows the text.
fn uncovered(a: &Look, a_bar: Option<u32>, b: &Look, b_bar: Option<u32>) -> u32 {
    let mut n = 0;
    each_uncovered(a, a_bar, b, b_bar, |_| n += 1);
    n
}

/// `a_bar` and `b_bar` are where Zoom's bar starts in each picture, when the lecture shows it to be the bar.
fn each_uncovered(a: &Look, a_bar: Option<u32>, b: &Look, b_bar: Option<u32>, mut hit: impl FnMut(usize)) {
    let (pa, pb) = (a.gray.as_raw(), b.gray.as_raw());
    // `a`'s own bar is not the slide, so its icons are never ink to hold. `b`'s bar holds nothing: what `a` shows under
    // it, `b` hides.
    let below = a_bar.unwrap_or(H);
    for y in 0..below {
        for x in 0..W {
            if toolbar(x, y) {
                continue;
            }
            let (i, t) = ((y * W + x) as usize, tile_of(x, y));
            let ia = a.ink_at(x, y);
            if ia == 0 {
                continue;
            }
            if b_bar.is_some_and(|top| y >= top) {
                hit(i);
                continue;
            }
            // Holding needs an edge right there: a fill at the ink's own level, a box laid over strokes, shows none of it.
            if !b.edge[i] {
                hit(i);
                continue;
            }
            if b.busy[t] && (pb[i] as i16 - pa[i] as i16).abs() <= SLACK {
                continue;
            }
            let (ib, own) = (pb[i] as i16 - a.page[t] as i16, pb[i] as i16 - b.page[t] as i16);
            let stronger = ib.signum() == ia.signum() && ib.abs() >= ia.abs() - SLACK;
            if !(stronger && own.signum() == ia.signum() && own.abs() > INK) {
                hit(i);
            }
        }
    }
}

/// Where `a`'s ink is not held by `b` and `b`'s not by `a`, as a map of 8×8 cells ('L' lost, 'A' added, '*' both): for
/// calibrating by eye.
pub fn map(a: &Look, b: &Look) -> String {
    let (lost, added) = (marks(a, b), marks(b, a));
    let mut out = String::new();
    for cy in 0..H / 8 {
        for cx in 0..W / 8 {
            let cell = |m: &Vec<bool>| (0..8).flat_map(|dy| (0..8).map(move |dx| ((cy * 8 + dy) * W + cx * 8 + dx) as usize)).filter(|&i| m[i]).count();
            out.push(match (cell(&lost) > 1, cell(&added) > 1) {
                (true, true) => '*',
                (true, false) => 'L',
                (false, true) => 'A',
                _ => '.',
            });
        }
        out.push('\n');
    }
    out
}

fn marks(a: &Look, b: &Look) -> Vec<bool> {
    let mut m = vec![false; (W * H) as usize];
    each_uncovered(a, None, b, None, |i| m[i] = true);
    m
}

/// What `b` is to `a`, every pixel being the slide's.
pub fn relate(a: &Look, b: &Look) -> Relation {
    relate_with(a, None, b, None)
}

/// What `b` is to `a`, given where Zoom's bar is drawn over each, when the lecture shows it to be the bar.
pub fn relate_with(a: &Look, a_bar: Option<u32>, b: &Look, b_bar: Option<u32>) -> Relation {
    let px = (W * H) as f32;
    Relation { lost: uncovered(a, a_bar, b, b_bar) as f32 / px, added: uncovered(b, b_bar, a, a_bar) as f32 / px }
}

/// Whether two pictures carry the same bottom band, the way Zoom draws one bar over whatever is shared: it starts at the
/// same row in both and differs in at most 3 % of its pixels (a participant count, a chat badge).
pub fn same_band(a: &Look, b: &Look) -> bool {
    let (Some(ta), Some(tb)) = (a.bar, b.bar) else { return false };
    if ta.abs_diff(tb) > 1 {
        return false;
    }
    let from = (ta.max(tb) * W) as usize;
    let band = &a.gray.as_raw()[from..];
    let differ = band.iter().zip(&b.gray.as_raw()[from..]).filter(|(x, y)| x.abs_diff(**y) as i16 > INK).count();
    differ as f32 <= 0.03 * band.len() as f32
}

/// Whether two pictures show the same thing above their bands: at most 1 % of those pixels differ.
pub fn same_above(a: &Look, b: &Look) -> bool {
    let to = (a.bar.into_iter().chain(b.bar).min().unwrap_or(H) * W) as usize;
    let differ = a.gray.as_raw()[..to].iter().zip(&b.gray.as_raw()[..to]).filter(|(x, y)| x.abs_diff(**y) as i16 > INK).count();
    differ as f32 <= 0.01 * to.max(1) as f32
}

/// Whether the two can be the same picture at all: a cheap look at the tile means before `relate`.
pub fn near(a: &Look, b: &Look) -> bool {
    near_with(a, None, b, None)
}

/// `near`, leaving out the tiles that reach into Zoom's bar in either picture.
pub fn near_with(a: &Look, a_bar: Option<u32>, b: &Look, b_bar: Option<u32>) -> bool {
    let below = a_bar.into_iter().chain(b_bar).min().unwrap_or(H);
    let rows = (below / TILE) as usize * (W / TILE) as usize;
    rows > 0 && a.tiles[..rows].iter().zip(&b.tiles[..rows]).map(|(x, y)| (x - y).abs()).sum::<f32>() / rows as f32 <= NEAR
}

pub fn order(r: &Relation) -> Order {
    match (r.lost <= LOST, r.added <= LOST) {
        (false, false) => Order::Apart,
        (true, false) => Order::Earlier,
        (false, true) => Order::Later,
        (true, true) if r.added - r.lost > NOISE => Order::Earlier,
        (true, true) if r.lost - r.added > NOISE => Order::Later,
        (true, true) => Order::Same,
    }
}

/// Share of the frame whose level differs by more than ink does.
pub fn changed(a: &Look, b: &Look) -> f32 {
    a.gray.as_raw().iter().zip(b.gray.as_raw()).filter(|(x, y)| x.abs_diff(**y) as i16 > INK).count() as f32 / (W * H) as f32
}

/// Two frames of the same view: most pixels alike, as a fixed camera's are while someone moves in front of it.
pub fn same_view(a: &Look, b: &Look) -> bool {
    changed(a, b) < SAME_VIEW
}

/// How two pictures compare when either is a photograph. A photograph's detail is gradient and grain rather than ink,
/// so by ink it would read as nearly blank and an earlier state of anything: it is only ever the same picture as one
/// that matches it pixel for pixel.
pub fn order_photographs(a: &Look, b: &Look) -> Order {
    if changed(a, b) <= LOST { Order::Same } else { Order::Apart }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use image::Luma;

    pub(crate) const PAGE: u8 = 246;

    /// A light slide: a title of strokes (another `title` is another title) and `lines` bullet bars, revealed one by
    /// one as a build does.
    pub(crate) fn slide(title: u32, lines: usize) -> GrayImage {
        let mut g = GrayImage::from_pixel(W, H, Luma([PAGE]));
        for x in (20..170).filter(|x| (x + title) % 6 < 3) {
            bar(&mut g, 14, x, 1, 8, 30);
        }
        for (k, w) in [190, 170, 200, 140, 180, 120].into_iter().take(lines).enumerate() {
            bar(&mut g, 44 + 16 * k as u32, 26, w, 5, 50);
        }
        g
    }

    pub(crate) fn bar(g: &mut GrayImage, y0: u32, x0: u32, w: u32, h: u32, level: u8) {
        for y in y0..(y0 + h).min(H) {
            for x in x0..(x0 + w).min(W) {
                g.put_pixel(x, y, Luma([level]));
            }
        }
    }

    /// `to` part way faded in over `from`.
    fn blend(from: &GrayImage, to: &GrayImage, share: f32) -> GrayImage {
        GrayImage::from_fn(W, H, |x, y| {
            let (a, b) = (from.get_pixel(x, y).0[0] as f32, to.get_pixel(x, y).0[0] as f32);
            Luma([(a + (b - a) * share).round() as u8])
        })
    }

    fn rel(a: &GrayImage, b: &GrayImage) -> Order {
        order(&relate(&Look::of(a.clone()), &Look::of(b.clone())))
    }

    /// `rel`, with each picture's bar taken to be Zoom's, as the lecture would show it.
    fn rel_barred(a: &GrayImage, b: &GrayImage) -> Order {
        let (la, lb) = (Look::of(a.clone()), Look::of(b.clone()));
        order(&relate_with(&la, la.bar, &lb, lb.bar))
    }

    #[test]
    fn each_step_of_a_build_is_an_earlier_state_of_the_next_and_of_the_last() {
        let steps: Vec<_> = (1..=4).map(|n| slide(0, n)).collect();
        for w in steps.windows(2) {
            assert_eq!(rel(&w[0], &w[1]), Order::Earlier);
            assert_eq!(rel(&w[1], &w[0]), Order::Later);
        }
        assert_eq!(rel(&steps[0], &steps[3]), Order::Earlier, "the first step against the complete slide");
    }

    #[test]
    fn a_frame_of_a_fade_is_an_earlier_state_and_so_is_a_faint_one_out_of_order() {
        let (before, done) = (slide(0, 2), slide(0, 4));
        for share in [0.3, 0.6] {
            assert_eq!(rel(&blend(&before, &done, share), &done), Order::Earlier, "{share}");
        }
        assert_eq!(rel(&blend(&before, &done, 0.9), &done), Order::Same, "nine tenths of the way is the slide itself");
        // 101 / 102 / 103: the table alone, then a faint fade of the whole slide, then all of it.
        let faint = blend(&GrayImage::from_pixel(W, H, Luma([PAGE])), &done, 0.15);
        assert_eq!(rel(&faint, &done), Order::Earlier);
        assert_eq!(rel(&before, &done), Order::Earlier);
    }

    #[test]
    fn the_same_slide_again_is_the_same_even_with_the_pointer_moved() {
        let s = slide(0, 3);
        assert_eq!(rel(&s, &s.clone()), Order::Same);
        let (mut a, mut b) = (s.clone(), s.clone());
        bar(&mut a, 100, 200, 2, 4, 0);
        bar(&mut b, 60, 230, 2, 4, 0);
        assert_eq!(rel(&a, &b), Order::Same);
    }

    #[test]
    fn a_figure_filled_in_is_more_not_the_same() {
        let s = slide(0, 3);
        let mut more = s.clone();
        bar(&mut more, 120, 120, 12, 6, 20); // a number written into a table cell
        assert_eq!(rel(&s, &more), Order::Earlier);
    }

    #[test]
    fn the_slideshow_toolbar_is_ignored_both_ways() {
        let s = slide(0, 3);
        let mut with = s.clone();
        for k in 0..5 {
            bar(&mut with, H - 6, 3 + 7 * k, 5, 5, 90);
        }
        assert_eq!(rel(&with, &s), Order::Same);
        assert_eq!(rel(&s, &with), Order::Same);
    }

    /// Zoom's control bar over the bottom of `g`: its dark level with icons and labels in it.
    pub(crate) fn with_bar(g: &GrayImage) -> GrayImage {
        let mut g = g.clone();
        bar(&mut g, 130, 0, W, 14, 22);
        for x in [8, 60, 110, 140, 170, 200] {
            bar(&mut g, 133, x, 6, 4, 200);
        }
        g
    }

    #[test]
    fn zooms_bar_over_the_footer_is_the_same_slide_and_is_found_on_light_and_dark_slides() {
        let mut s = slide(0, 3);
        bar(&mut s, 136, 20, 90, 3, 90); // the slide's own footer, under where the bar comes
        let barred = with_bar(&s);
        assert!(Look::of(barred.clone()).bar.is_some() && Look::of(s.clone()).bar.is_none());
        assert_eq!(rel_barred(&barred, &s), Order::Earlier, "the clean capture holds the barred one, footer and all");
        let blank_bottom = slide(0, 3);
        assert_eq!(rel_barred(&blank_bottom, &with_bar(&blank_bottom)), Order::Same, "with nothing under it, the bar hides nothing");
        assert_eq!(rel(&s, &barred), Order::Apart, "taken as slide content, the band covers the footer and adds its own");
        let dark = GrayImage::from_fn(W, H, |x, y| Luma([if (40..48).contains(&y) && x > 20 && x < 200 { 230 } else { 34 }]));
        assert!(Look::of(with_bar(&dark)).bar.is_some(), "over a dark slide too");
        let black = GrayImage::from_fn(W, H, |x, y| Luma([if (40..48).contains(&y) && x > 20 && x < 200 { 230 } else if y >= 130 && x % 40 < 5 { 200 } else { 22 }]));
        assert!(Look::of(black).bar.is_none(), "a dark slide's own bottom is its background");
        let footed = GrayImage::from_fn(W, H, |x, y| Luma([if (100..124).contains(&y) && x % 9 < 4 { 200 } else if (131..134).contains(&y) && (20..100).contains(&x) && x % 3 == 0 { 180 } else { 34 }]));
        assert!(Look::of(footed).bar.is_none(), "nor is its footer");
    }

    #[test]
    fn what_zooms_bar_hides_in_the_later_capture_is_not_held_by_it() {
        // The finished slide came with the bar over a row only the earlier capture shows.
        let mut earlier = slide(0, 2);
        bar(&mut earlier, 134, 30, 160, 5, 40);
        let mut later = earlier.clone();
        bar(&mut later, 76, 26, 200, 5, 50);
        assert_eq!(rel_barred(&earlier, &with_bar(&later)), Order::Apart);
        assert_eq!(rel(&earlier, &later), Order::Earlier, "without the bar it is an earlier state");
    }

    #[test]
    fn another_slide_on_the_same_template_is_apart() {
        let a = slide(0, 3);
        let mut b = slide(0, 0);
        for (k, w) in [120, 210, 90].into_iter().enumerate() {
            bar(&mut b, 50 + 20 * k as u32, 40, w, 6, 50);
        }
        assert_eq!(rel(&a, &b), Order::Apart);
        assert_eq!(rel(&slide(0, 3), &slide(3, 3)), Order::Apart, "a different title");
    }

    #[test]
    fn a_light_slide_does_not_hold_white_text_on_a_dark_divider_nor_a_dark_one_dark_text() {
        let mut divider = GrayImage::from_pixel(W, H, Luma([40]));
        bar(&mut divider, 60, 20, 150, 10, 240);
        assert_eq!(rel(&divider, &slide(0, 3)), Order::Apart);
        // A flat dark frame shows nothing: at most an earlier state of the slide, never one that holds it.
        assert_eq!(rel(&slide(0, 3), &GrayImage::from_pixel(W, H, Luma([40]))), Order::Later);
    }

    #[test]
    fn text_hidden_under_a_box_is_lost() {
        let s = slide(0, 3);
        let mut boxed = s.clone();
        bar(&mut boxed, 40, 16, 224, 60, 40); // a dark panel laid over the bullets
        assert_eq!(rel(&s, &boxed), Order::Apart);
    }

    #[test]
    fn thin_strokes_painted_over_at_their_own_level_are_lost() {
        // F001: a narrow block at the strokes' level leaves the tile busy but shows none of them.
        let mut a = slide(0, 1);
        for k in 0..17 {
            bar(&mut a, 100, 60 + 6 * k, 1, 6, 30);
        }
        let mut b = slide(0, 1);
        bar(&mut b, 99, 58, 104, 8, 30);
        assert_eq!(rel(&a, &b), Order::Apart);
    }

    #[test]
    fn a_dark_box_with_text_revealed_inside_it_is_an_earlier_state() {
        let mut box_only = slide(0, 1);
        bar(&mut box_only, 80, 20, 200, 50, 40);
        let mut filled = box_only.clone();
        bar(&mut filled, 90, 30, 150, 5, 235);
        bar(&mut filled, 105, 30, 120, 5, 235);
        assert_eq!(rel(&box_only, &filled), Order::Earlier);
    }

    #[test]
    fn camera_frames_share_a_view_and_a_cut_does_not() {
        // Shading with a fixed grain, and someone standing in a different place.
        let room = |shift: u32| GrayImage::from_fn(W, H, |x, y| Luma([(60 + x * 90 / W + y * 70 / H + (x * 7 + y * 13) % 11 + if (100..130).contains(&x) && y > 40 { shift } else { 0 }).min(255) as u8]));
        let (a, b) = (Look::of(room(0)), Look::of(room(40)));
        assert!(a.photographic() && b.photographic());
        assert!(same_view(&a, &b));
        assert!(!same_view(&a, &Look::of(slide(0, 3))));
        assert!(!Look::of(slide(0, 3)).photographic());
    }

    #[test]
    fn photographs_are_the_same_only_when_they_match() {
        let photo = |k: u32| Look::of(GrayImage::from_fn(W, H, |x, y| Luma([(60 + x * 90 / W + y * 70 / H + (x * 7 + y * k) % 11) as u8])));
        assert_eq!(order_photographs(&photo(13), &photo(13)), Order::Same);
        let other = Look::of(GrayImage::from_fn(W, H, |x, y| Luma([(200 - x * 90 / W - y * 50 / H + (x * 5 + y * 3) % 11) as u8])));
        assert_eq!(order_photographs(&photo(13), &other), Order::Apart);
    }

    #[test]
    fn near_screens_out_different_pictures_cheaply() {
        let s = Look::of(slide(0, 3));
        assert!(near(&s, &Look::of(slide(0, 3))));
        assert!(!near(&s, &Look::of(GrayImage::from_pixel(W, H, Luma([20])))));
    }
}
