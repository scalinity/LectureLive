//! Detector fixtures (spec §11): the format a recording writes (`frames/NNNNN.png`, `samples.jsonl`,
//! `states.json`), a synthetic lecture, the gate's measures, and the alignment that annotates a
//! recording of the synthetic deck.
use std::collections::{HashMap, HashSet};
use std::path::Path;

use image::{imageops::FilterType, GrayImage, Luma, Rgba, RgbaImage};
use lecturelive_core::capture::detect::{thumb, Detector, Region, Thresholds, TILES, TILE, W};
use serde::{Deserialize, Serialize};

/// One sample: its time in ms of the recording, and the detector's input (None: the capture failed).
pub struct Sample {
    pub t: u64,
    pub frame: Option<GrayImage>,
}

/// An annotated stable state: what was on screen from `from` to `to` (ms of the recording).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct State {
    pub id: String,
    pub from: u64,
    pub to: u64,
    pub kind: String,
}

#[derive(Serialize, Deserialize)]
struct States {
    states: Vec<State>,
}

/// A recorded fixture's samples.
pub fn load_samples(dir: &Path) -> Vec<Sample> {
    let text = std::fs::read_to_string(dir.join("samples.jsonl")).expect("samples.jsonl");
    let mut frames: HashMap<u64, GrayImage> = HashMap::new();
    let mut out = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let v: serde_json::Value = serde_json::from_str(line).expect("a sample line");
        let t = v["t"].as_u64().expect("t");
        let frame = v["frame"].as_u64().filter(|&n| n > 0).map(|n| {
            frames.entry(n).or_insert_with(|| image::open(dir.join(format!("frames/{n:05}.png"))).expect("a frame").to_luma8()).clone()
        });
        out.push(Sample { t, frame });
    }
    out
}

/// A recorded fixture: its samples and its annotated states.
pub fn load(dir: &Path) -> (Vec<Sample>, Vec<State>) {
    let states: States = serde_json::from_slice(&std::fs::read(dir.join("states.json")).expect("states.json")).expect("states.json parses");
    (load_samples(dir), states.states)
}

pub struct Metrics {
    pub minutes: f64,
    /// States visible for at least 3 s.
    pub counted: usize,
    pub recalled: usize,
    pub missed: Vec<String>,
    /// (sample time, why): a capture of a state already captured, or of no state.
    pub false_captures: Vec<(u64, String)>,
    /// Every capture: (sample time, state id, uncertain).
    pub captures: Vec<(u64, String, bool)>,
}

impl Metrics {
    pub fn recall(&self) -> f64 {
        if self.counted == 0 { 0.0 } else { self.recalled as f64 / self.counted as f64 }
    }

    pub fn false_per_10_min(&self) -> f64 {
        self.false_captures.len() as f64 / (self.minutes / 10.0).max(1e-9)
    }
}

/// Plays the samples through the detector. A capture belongs to the state showing when its frame was
/// taken; the first capture of a state is a hit, any other capture is false.
pub fn evaluate(samples: &[Sample], states: &[State], t: Thresholds) -> Metrics {
    let mut d = Detector::<u64>::new(t);
    let (mut seen, mut false_captures, mut captures) = (HashSet::new(), Vec::new(), Vec::new());
    for s in samples {
        let Some(f) = &s.frame else { continue };
        let Some(dec) = d.observe(s.t, f.clone()) else { continue };
        match states.iter().find(|st| st.from <= s.t && s.t < st.to) {
            Some(st) => {
                captures.push((s.t, st.id.clone(), dec.uncertain));
                if !seen.insert(st.id.clone()) {
                    false_captures.push((s.t, format!("{} again", st.id)));
                }
            }
            None => {
                captures.push((s.t, String::new(), dec.uncertain));
                false_captures.push((s.t, "no state".into()));
            }
        }
    }
    let counted: Vec<&State> = states.iter().filter(|st| st.to - st.from >= 3000).collect();
    let missed: Vec<String> = counted.iter().filter(|st| !seen.contains(&st.id)).map(|st| st.id.clone()).collect();
    let span = samples.last().map_or(0, |s| s.t) - samples.first().map_or(0, |s| s.t);
    Metrics { minutes: span as f64 / 60_000.0, counted: counted.len(), recalled: counted.len() - missed.len(), missed, false_captures, captures }
}

// The synthetic lecture: a Zoom-like window whose slide region shows a deck with builds, dissolves,
// animations and brief states, sampled once a second with Zoom's sharpening and a little noise.

const WIN: (u32, u32) = (640, 400);
/// Where the slide sits in the window; outside it, Zoom's chrome and a video tile that never settles.
const REGION: Region = Region { x: 0.0625, y: 0.1, w: 0.875, h: 0.8 };

#[derive(Clone, Copy)]
enum Motion {
    Still,
    /// An animated chart in place, repeating every 15 s: it never returns to a kept frame within the expiry.
    Loop,
    /// A text caret blinking.
    Caret,
}

struct Step {
    slide: u32,
    build: u32,
    ms: u64,
    dissolve: bool,
    motion: Motion,
}

/// About 20 minutes: 24 slides of one to three builds; durations cycle 6–60 s; four brief states.
fn steps() -> Vec<Step> {
    let durs = [42_000u64, 18_000, 9_000, 27_000, 6_000, 55_000, 12_000, 33_000, 7_500, 21_000];
    let mut out = Vec::new();
    let mut k = 0;
    for slide in 1..=24u32 {
        let builds = [1u32, 2, 3, 2, 1, 3][slide as usize % 6];
        for build in 0..=builds {
            let motion = match slide {
                7 | 16 => Motion::Loop,
                11 => Motion::Caret,
                _ => Motion::Still,
            };
            let mut ms = durs[k % durs.len()];
            if motion as u8 != Motion::Still as u8 {
                ms = ms.max(20_000);
            }
            if slide % 6 == 3 && build == 1 {
                ms = 1_500; // a build clicked through
            }
            out.push(Step { slide, build, ms, dissolve: build == 0 && slide % 2 == 0, motion });
            k += 1;
        }
    }
    out
}

fn fill(img: &mut RgbaImage, x: u32, y: u32, w: u32, h: u32, v: u8) {
    for yy in y..(y + h).min(img.height()) {
        for xx in x..(x + w).min(img.width()) {
            img.put_pixel(xx, yy, Rgba([v, v, v, 255]));
        }
    }
}

/// The slide as the window shows it; `phase` animates loops and carets.
fn render(slide: u32, build: u32, motion: Motion, phase: u64) -> RgbaImage {
    let mut img = RgbaImage::from_pixel(WIN.0, WIN.1, Rgba([28, 28, 30, 255]));
    let (sx, sy, sw, sh) = REGION.pixels(WIN.0, WIN.1).unwrap();
    fill(&mut img, sx, sy, sw, sh, 250);
    fill(&mut img, sx + 24, sy + 18, 180 + (slide * 37) % 180, 16, 30); // the title
    for i in 0..=build {
        let y = sy + 60 + i * 52;
        match (slide + i) % 4 {
            0 => fill(&mut img, sx + 40, y, 260 + (slide * 13 + i * 29) % 180, 10, 50), // a bullet line
            1 => {
                for c in 0..3 {
                    fill(&mut img, sx + 40 + c * 130, y, 120, 2, 60); // a table row's rule and cells
                    fill(&mut img, sx + 50 + c * 130, y + 8, 70 + (slide * 7 + c * 11) % 40, 9, 60);
                }
            }
            2 => {
                fill(&mut img, sx + 40, y, 300, 3, 40); // a boxed equation
                fill(&mut img, sx + 40, y + 30, 300, 3, 40);
                fill(&mut img, sx + 40, y, 3, 33, 40);
                fill(&mut img, sx + 337, y, 3, 33, 40);
                fill(&mut img, sx + 70, y + 12, 200, 9, 60);
            }
            _ => {
                for s in 0..160u32 {
                    fill(&mut img, sx + 60 + s * 2, y + 30 - s / 6, 4, 4, 70); // an arrow across a diagram
                }
                fill(&mut img, sx + 40, y + 36, 120, 5, 90); // underline of its label
            }
        }
    }
    match motion {
        Motion::Loop => {
            for j in 0..6u32 {
                let h = 12 + ((phase as u32 % 15) * 7 + j * 11) % 40;
                fill(&mut img, sx + sw - 150 + j * 20, sy + sh - 30 - h, 12, h, 45);
            }
        }
        Motion::Caret if phase % 2 == 0 => fill(&mut img, sx + 310, sy + 60, 3, 14, 20),
        _ => {}
    }
    img
}

/// The video tile outside the region changes with every sample.
fn video_tile(img: &mut RgbaImage, phase: u64) {
    fill(img, WIN.0 - 36, 4, 32, 24, (40 + (phase * 53) % 180) as u8);
}

fn blend(a: &RgbaImage, b: &RgbaImage, f: f32) -> RgbaImage {
    RgbaImage::from_fn(a.width(), a.height(), |x, y| {
        let (p, q) = (a.get_pixel(x, y).0, b.get_pixel(x, y).0);
        let m = |i: usize| (p[i] as f32 * (1.0 - f) + q[i] as f32 * f).round() as u8;
        Rgba([m(0), m(1), m(2), 255])
    })
}

/// Zoom's first frame after a change: softer, sharpened a moment later.
fn soften(img: &RgbaImage) -> RgbaImage {
    let small = image::imageops::resize(img, img.width() / 3, img.height() / 3, FilterType::Triangle);
    image::imageops::resize(&small, img.width(), img.height(), FilterType::Triangle)
}

fn noise(frame: &mut GrayImage, seed: u64) {
    let mut s = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
    for p in frame.pixels_mut() {
        s = s.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        let n = ((s >> 33) % 5) as i16 - 2;
        *p = Luma([(p.0[0] as i16 + n).clamp(0, 255) as u8]);
    }
}

/// About half an hour sampled every second, 370 ms after the start so boundaries fall between samples.
pub fn synthetic_lecture() -> &'static (Vec<Sample>, Vec<State>) {
    static LECTURE: std::sync::OnceLock<(Vec<Sample>, Vec<State>)> = std::sync::OnceLock::new();
    LECTURE.get_or_init(generate)
}

fn generate() -> (Vec<Sample>, Vec<State>) {
    let steps = steps();
    let mut states = Vec::new();
    let mut at = 0u64;
    for s in &steps {
        states.push(State { id: format!("s{:02}b{}", s.slide, s.build), from: at, to: at + s.ms, kind: if s.ms < 3000 { "brief".into() } else { "slide".into() } });
        at += s.ms;
    }
    // Each state's animation phases, drawn once: the test profile runs this code unoptimised.
    let mut drawn: HashMap<(usize, u64), RgbaImage> = HashMap::new();
    let cycle = |m: Motion, phase: u64| match m {
        Motion::Still => 0,
        Motion::Loop => phase % 15,
        Motion::Caret => phase % 2,
    };
    let mut draw = |i: usize, phase: u64| drawn.entry((i, cycle(steps[i].motion, phase))).or_insert_with(|| render(steps[i].slide, steps[i].build, steps[i].motion, phase)).clone();
    let mut samples = Vec::new();
    let mut last_change: Option<usize> = None;
    let mut t = 370;
    while t < at {
        let i = states.iter().position(|st| st.from <= t && t < st.to).unwrap();
        let s = &steps[i];
        let phase = t / 1000;
        let mut img = draw(i, phase);
        let since = t - states[i].from;
        if since < 600 && s.dissolve && i > 0 {
            img = blend(&draw(i - 1, phase), &img, since as f32 / 600.0);
        } else if last_change != Some(i) {
            img = soften(&img); // the first sample of a new state
        }
        video_tile(&mut img, phase);
        if since >= 600 || !s.dissolve {
            last_change = Some(i);
        }
        let mut frame = thumb(&img, &REGION).expect("a valid frame");
        noise(&mut frame, t);
        samples.push(Sample { t, frame: Some(frame) });
        t += 1000;
    }
    (samples, states)
}

// Annotating a recording of the synthetic deck (apps/desktop/src/lib/deck.json).

#[derive(Deserialize)]
struct Deck {
    states: Vec<DeckState>,
}

#[derive(Deserialize)]
struct DeckState {
    id: String,
    ms: u64,
    kind: String,
    #[serde(default)]
    build: u32,
}

/// The deck's schedule as states from its Start, and which of them begin a new slide.
pub fn schedule(path: &Path) -> Vec<(State, bool)> {
    let deck: Deck = serde_json::from_slice(&std::fs::read(path).expect("deck.json")).expect("deck.json parses");
    let mut at = 0;
    deck.states
        .into_iter()
        .map(|s| {
            let st = State { id: s.id, from: at, to: at + s.ms, kind: s.kind };
            at += s.ms;
            (st, s.build == 0)
        })
        .collect()
}

/// Share of tiles that changed by more than 0.08 between two frames.
fn changed_share(a: &GrayImage, b: &GrayImage) -> f64 {
    let (a, b) = (a.as_raw(), b.as_raw());
    let cols = (W / TILE) as usize;
    let mut sums = [0u32; TILES];
    for (i, (x, y)) in a.iter().zip(b).enumerate() {
        let (px, py) = (i % W as usize, i / W as usize);
        sums[(py / TILE as usize) * cols + px / TILE as usize] += x.abs_diff(*y) as u32;
    }
    sums.iter().filter(|&&s| s as f64 / (TILE * TILE) as f64 / 255.0 > 0.08).count() as f64 / TILES as f64
}

/// The offset (ms of the recording) of the deck's Start that best matches its slide boundaries to the
/// recording's large changes, and each boundary's distance from the nearest change at that offset.
pub fn align(samples: &[Sample], plan: &[(State, bool)]) -> (u64, Vec<i64>) {
    let mut changes = Vec::new();
    let mut prev: Option<&GrayImage> = None;
    for s in samples {
        if let Some(f) = &s.frame {
            if prev.is_some_and(|p| changed_share(p, f) > 0.2) {
                changes.push(s.t as i64);
            }
            prev = Some(f);
        }
    }
    let bounds: Vec<i64> = plan.iter().filter(|(_, first)| *first).map(|(s, _)| s.from as i64).collect();
    let end = samples.last().map_or(0, |s| s.t) as i64;
    let near = |b: i64| changes.iter().map(|c| c - b).min_by_key(|d| d.abs()).unwrap_or(i64::MAX / 4);
    let score = |o: i64| bounds.iter().map(|b| near(b + o).abs().min(3000)).sum::<i64>();
    let best = (0..=end.max(0)).step_by(100).min_by_key(|&o| score(o)).unwrap_or(0);
    (best as u64, bounds.iter().map(|b| near(b + best)).collect())
}

/// Writes `states.json`: the waiting title before Start, then the deck's states at the offset.
pub fn write_states(dir: &Path, plan: &[(State, bool)], offset: u64, end: u64) {
    let mut states = vec![State { id: "title".into(), from: 0, to: offset, kind: "slide".into() }];
    for (s, _) in plan {
        states.push(State { id: s.id.clone(), from: s.from + offset, to: (s.to + offset).min(end.max(s.to + offset)), kind: s.kind.clone() });
    }
    if let Some(last) = states.last_mut() {
        last.to = last.to.max(end);
    }
    std::fs::write(dir.join("states.json"), serde_json::to_vec_pretty(&States { states }).unwrap()).unwrap();
}
