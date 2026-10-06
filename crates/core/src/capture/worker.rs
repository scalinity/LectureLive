//! The capture worker (spec §3.2, §7): on its own thread, because its native calls and image work
//! block. Once a second it finds the bound window, captures it, and feeds the slide region to the
//! detector; a kept frame becomes a PNG temp file in `slides/` for the lecture to register. Once bound,
//! it binds no other window by itself, except the saved one back: when the window closes and exactly one
//! window matches the selection as at a lecture's start (an app quit and opened again), that is watched
//! and said. Anything else, several matches, a size not seen before, another app, pauses capture and asks
//! (spec §7.1).
use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc::{self as std_mpsc, RecvTimeoutError};
use std::time::{Duration, Instant};

use chrono::{DateTime, Local};
use image::{imageops::FilterType, GrayImage, RgbaImage};
use serde::Serialize;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;

use crate::capture::detect::{crop, thumb, thumb_for, Detector, Region, Thresholds, SAMPLE_HZ};
use crate::capture::layout;
use crate::capture::locate::locate;
use crate::capture::scene::{classify, features, Gate, Scene};
use crate::capture::select::{revalidate, Revalidation, Selection};
use crate::capture::window::{fit_within, is_blank, CaptureError, WindowInfo, WindowSource, MAX_PX};

/// Failed captures in a row before the strip says so: a minimise animation or one bad frame is not worth a word.
const FAILURES_SHOWN: u32 = 3;
/// Samples between searches for the slide while it is not found: the new layout may not be drawn yet, or a
/// gallery comes before the share.
const SEARCH_AGAIN: u32 = 5 * SAMPLE_HZ;
/// Samples a new window size must hold before the region is looked for at it: full screen animates, and a size
/// is reported before the content is laid out for it.
const NEW_SIZE_HELD: u32 = SAMPLE_HZ;
/// Samples between looks at where the shared content is, to see that the region still looks at it; every sample
/// while it looks wrong, and on the first sample after any region is taken up.
const AUDIT_EVERY: u32 = 5;
/// Slides are 4:3 to 16:9, and a little wider; a rectangle of the window with another shape is a strip or a tile.
const SLIDE_SHAPES: std::ops::RangeInclusive<f64> = 1.1..=2.4;
/// The width of the whole-window copies a recording keeps for re-cropping (never committed).
const RECORD_WINDOW_PX: u32 = 1024;

pub struct WorkerConfig {
    pub slides: PathBuf,
    pub interval: Duration,
    pub thresholds: Thresholds,
    /// Writes each sample's detector input here: a fixture of exactly what the detector saw (spec §11).
    pub record: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CaptureState {
    /// No window has been chosen for this course.
    Unbound,
    Watching { window: String },
    Paused { window: String, reason: String },
    /// The saved window is not there as it was: the person chooses.
    Asking { window: String, reason: String, candidates: Vec<WindowInfo> },
    /// Screen Recording is not granted.
    Denied,
    Failing { window: String, reason: String },
}

impl CaptureState {
    /// The state as an event line: what it is, and what happened.
    pub fn words(&self) -> (&'static str, String) {
        match self {
            Self::Unbound => ("No window", "choose the window with the slides in the slides strip".into()),
            Self::Watching { window } => ("Watching", window.clone()),
            Self::Paused { window, reason } => ("Paused", format!("{window}: {reason}")),
            Self::Asking { window, reason, .. } => ("Asking", format!("{reason}; choose in the slides strip which window to watch for {window}")),
            Self::Denied => ("Screen Recording", CaptureError::Denied.to_string()),
            Self::Failing { window, reason } => ("Capture failing", format!("{window}: {reason}")),
        }
    }
}

/// A slide saved to a temp file in `slides/`, waiting to be registered.
#[derive(Debug)]
pub struct Captured {
    pub path: PathBuf,
    pub shown_at: DateTime<Local>,
    pub auto: bool,
    pub uncertain: bool,
}

#[derive(Debug)]
pub enum CaptureEvent {
    State(CaptureState),
    /// The same window's slide is watched through another region (its size changed): to save, and to say.
    Relocated { selection: Selection, note: String },
    Captured(Captured),
}

pub enum CaptureCmd {
    /// Capture the bound region now, as a manual slide (the button, the shortcut).
    Now(oneshot::Sender<Result<(), String>>),
    /// The person chose this window and region.
    Bind { window: u32, selection: Selection },
    /// The lecture is paused (spec §9.6): nothing is sampled until it is not. A manual capture still works.
    Hold(bool),
    Stop,
}

/// Commands to the worker; dropping it stops the worker.
pub struct CaptureHandle {
    tx: std_mpsc::Sender<CaptureCmd>,
}

impl CaptureHandle {
    pub fn send(&self, c: CaptureCmd) {
        let _ = self.tx.send(c);
    }
}

pub fn spawn(source: Box<dyn WindowSource>, selection: Option<Selection>, cfg: WorkerConfig, events: UnboundedSender<CaptureEvent>) -> CaptureHandle {
    let (tx, rx) = std_mpsc::channel();
    std::thread::Builder::new()
        .name("capture".into())
        .spawn(move || {
            let recorder = cfg.record.as_ref().and_then(|d| Recorder::new(d.clone()).ok());
            let mut w = Worker { detector: Detector::new(cfg.thresholds), gate: Gate::new(), source, cfg, events, bound: None, waiting: None, sent: None, failures: 0, tried: None, new_size: None, settling: 0, held: false, since_audit: AUDIT_EVERY, suspect: 0, recorder };
            w.start(selection);
            w.run(rx);
        })
        .expect("start the capture thread");
    CaptureHandle { tx }
}

struct Worker {
    source: Box<dyn WindowSource>,
    cfg: WorkerConfig,
    events: UnboundedSender<CaptureEvent>,
    /// The window watched and how.
    bound: Option<(u32, Selection)>,
    /// A saved selection whose window was not found as it was: asking.
    waiting: Option<Selection>,
    detector: Detector<DateTime<Local>>,
    /// What each sample shows: a slide to watch for changes, or a camera or notice to take nothing from.
    gate: Gate,
    sent: Option<CaptureState>,
    failures: u32,
    /// The window size the last search for the slide was made at, and the samples since.
    tried: Option<((u32, u32), u32)>,
    /// A size the window reported that differs from the region's, and for how many samples in a row.
    new_size: Option<((u32, u32), u32)>,
    /// Samples after a new region that settle the kept frame while they still show it.
    settling: u32,
    /// The lecture is paused: nothing is sampled.
    held: bool,
    /// Samples since the region was last checked against the shared content, and the checks in a row that found
    /// the region looking somewhere else. A region under suspicion takes nothing until it is checked again.
    since_audit: u32,
    suspect: u32,
    recorder: Option<Recorder>,
}

impl Worker {
    fn run(&mut self, rx: std_mpsc::Receiver<CaptureCmd>) {
        let mut next = Instant::now();
        loop {
            match rx.recv_timeout(next.saturating_duration_since(Instant::now())) {
                Ok(CaptureCmd::Now(reply)) => {
                    let _ = reply.send(self.capture_now());
                }
                Ok(CaptureCmd::Bind { window, selection }) => {
                    self.bind(window, selection);
                    next = Instant::now();
                }
                Ok(CaptureCmd::Hold(held)) => {
                    self.held = held;
                    self.failures = 0;
                    next = Instant::now();
                }
                Ok(CaptureCmd::Stop) | Err(RecvTimeoutError::Disconnected) => return,
                Err(RecvTimeoutError::Timeout) => {
                    self.tick();
                    next += self.cfg.interval;
                    if next < Instant::now() {
                        next = Instant::now() + self.cfg.interval; // a slow tick: keep the cadence, do not catch up
                    }
                }
            }
        }
    }

    /// Spec §7.1: the saved selection is revalidated when the lecture starts; only an exact match binds.
    fn start(&mut self, selection: Option<Selection>) {
        let Some(sel) = selection else { return self.set(CaptureState::Unbound) };
        match self.source.windows() {
            Ok(ws) => match revalidate(&sel, &ws) {
                Revalidation::Match(w) => self.bound = Some((w.id, sel.at_size(w.width, w.height).unwrap_or(sel))),
                Revalidation::Ask { reason, candidates } => {
                    let window = sel.descriptor.label();
                    self.waiting = Some(sel);
                    self.set(CaptureState::Asking { window, reason, candidates });
                }
            },
            Err(e) => {
                let window = sel.descriptor.label();
                self.waiting = Some(sel);
                self.failed(&window, e);
            }
        }
    }

    fn bind(&mut self, window: u32, selection: Selection) {
        let seen = |s: &Selection| (s.region, s.leave_out.clone());
        let before = self.bound.as_ref().map(|(_, s)| seen(s)).or(self.waiting.as_ref().map(seen));
        if before != Some(seen(&selection)) {
            self.detector = Detector::new(self.cfg.thresholds); // another region or parts left out: its first frame is kept
        }
        self.bound = Some((window, selection));
        self.waiting = None;
        self.tried = None;
        self.failures = 0;
        (self.since_audit, self.suspect) = (AUDIT_EVERY, 0);
    }

    fn set(&mut self, s: CaptureState) {
        if self.sent.as_ref() != Some(&s) {
            self.sent = Some(s.clone());
            let _ = self.events.send(CaptureEvent::State(s));
        }
    }

    fn failed(&mut self, window: &str, e: CaptureError) {
        if let Some(r) = self.recorder.as_mut() {
            r.error(&e.to_string());
        }
        match e {
            CaptureError::Denied => self.set(CaptureState::Denied),
            CaptureError::Failed(reason) => {
                self.failures += 1;
                if self.failures >= FAILURES_SHOWN {
                    self.set(CaptureState::Failing { window: window.to_string(), reason });
                }
            }
        }
    }

    /// While asking: the candidates as they are now. Exactly one window matching the selection is the saved window
    /// back, as at a lecture's start: it is watched, with no one to choose.
    fn wait(&mut self) {
        let Some(sel) = self.waiting.clone() else { return };
        let window = sel.descriptor.label();
        match self.source.windows() {
            Ok(ws) => match revalidate(&sel, &ws) {
                Revalidation::Match(w) => self.follow(&w, &sel),
                Revalidation::Ask { reason, candidates } => self.set(CaptureState::Asking { window, reason, candidates }),
            },
            Err(e) => self.failed(&window, e),
        }
    }

    /// The window this selection watched is gone and exactly one window matches it as one does at a lecture's
    /// start (spec §7.1): the app was quit and opened again, as a proctoring browser does. It is watched as before,
    /// through the same region, and the app is told so it can say it. Which slide it shows is the detector's to
    /// judge against the last kept one, so the slide that was up is not taken again.
    fn follow(&mut self, window: &WindowInfo, sel: &Selection) {
        let Some(at) = sel.at_size(window.width, window.height) else { return };
        let label = sel.descriptor.label();
        self.bound = Some((window.id, at.clone()));
        self.waiting = None;
        self.tried = None;
        self.new_size = None;
        self.failures = 0;
        (self.since_audit, self.suspect) = (AUDIT_EVERY, 0);
        let _ = self.events.send(CaptureEvent::Relocated { selection: at, note: format!("{label} opened again; watching it as before") });
    }

    fn tick(&mut self) {
        if self.held {
            if let Some((_, sel)) = &self.bound {
                let window = sel.descriptor.label();
                self.set(CaptureState::Paused { window, reason: "the lecture is paused; capture goes on when it resumes".into() });
            }
            return;
        }
        let Some((id, sel)) = self.bound.clone() else { return self.wait() };
        let window = sel.descriptor.label();
        let windows = match self.source.windows() {
            Ok(ws) => ws,
            Err(e) => return self.failed(&window, e),
        };
        let Some(w) = windows.iter().find(|w| w.id == id).cloned() else {
            let others: Vec<WindowInfo> = windows.into_iter().filter(|w| sel.matches(w)).collect();
            if let [only] = others.as_slice() {
                return self.follow(only, &sel);
            }
            return self.set(if others.is_empty() {
                CaptureState::Paused { window, reason: "the window closed; a new one is offered here when it opens".into() }
            } else {
                CaptureState::Asking { reason: format!("a new “{window}” window opened"), window, candidates: others }
            });
        };
        // Any change of size re-lays out the window, even one within the 2% that names the same window: full
        // screen on a display barely larger than the window moves the slide by a few pixels.
        let sel = if (sel.descriptor.width, sel.descriptor.height) == (w.width, w.height) {
            self.new_size = None;
            sel
        } else {
            // Full screen animates, and a size can be reported before the content is laid out for it: act
            // once the new size has held for a sample.
            let seen = match self.new_size {
                Some((size, n)) if size == (w.width, w.height) => n + 1,
                _ => 1,
            };
            self.new_size = Some(((w.width, w.height), seen));
            if seen < NEW_SIZE_HELD {
                return;
            }
            match self.resized(id, &sel, &w) {
                Some(s) => s,
                None => return,
            }
        };
        // Captured by its id whether on screen or not: covered, or full screen on another desktop while the
        // person works elsewhere. Only a window that cannot be captured waits.
        let img = match self.frame(id, &sel) {
            Ok((img, t)) => {
                // Found wrongly while the window was still moving, a region would stay wrong: so it is checked
                // against where the shared content is, and moved when it is not looking at it.
                if let Some(better) = self.audit(&img, &sel.region) {
                    let found = sel.with_size(w.width, w.height, better);
                    self.rebind(id, found, format!("{window} is {} × {}; the slide was not where the region looked, and is watched where its edges are", w.width, w.height));
                    return;
                }
                if self.suspect > 0 {
                    return; // looking somewhere else than the slide, perhaps: nothing is taken until it is checked again
                }
                let scene = self.gate.look(&t);
                if let Some(r) = self.recorder.as_mut() {
                    r.sample(&t, &img, scene);
                }
                self.failures = 0;
                // Zoom's camera, a name tile and "has started screen sharing" are not slides: the strip says what
                // is showing, and nothing is taken from it. A manual capture is the person's call and still works.
                self.set(CaptureState::Watching { window: scene.waiting_for().map_or_else(|| window.clone(), |why| format!("{window}, waiting for a slide ({why})")) });
                if scene != Scene::Slide {
                    None
                } else if self.settling > 0 && self.detector.shows_kept(&t) {
                    // Just after a new region: a frame still showing the kept slide settles into it.
                    self.settling -= 1;
                    self.detector.keep(t);
                    None
                } else {
                    self.settling = 0;
                    self.detector.observe(Local::now(), t).map(|d| (img, d))
                }
            }
            Err(e) if !w.on_screen => {
                // A closed window's id can stay listed off screen while its app runs, so another window
                // matching on screen is a replacement to ask about, not a reason to wait.
                let others: Vec<WindowInfo> = windows.iter().filter(|o| o.id != id && o.on_screen && sel.matches(o)).cloned().collect();
                if let Some(r) = self.recorder.as_mut() {
                    r.error(&e.to_string());
                }
                // Moving to or from full screen, a window is off screen and uncapturable for a moment.
                self.failures += 1;
                if self.failures < FAILURES_SHOWN {
                    return;
                }
                if let [only] = others.as_slice() {
                    return self.follow(only, &sel);
                }
                return self.set(if others.is_empty() {
                    CaptureState::Paused { window, reason: "not on screen and cannot be captured (minimised?); capture resumes when it is back".into() }
                } else {
                    CaptureState::Asking { reason: format!("a new “{window}” window opened"), window, candidates: others }
                });
            }
            Err(e) => return self.failed(&window, e),
        };
        if let Some((img, d)) = img {
            if let Err(e) = self.save(&img, &sel.region, d.shown_at, true, d.uncertain) {
                self.set(CaptureState::Failing { window, reason: e });
            }
        }
    }

    /// Where the shared content is in this capture, when that is a slide: a rectangle of the window of a slide's
    /// shape that looks like a slide and not a camera, a speaker strip or a notice.
    fn edges(img: &RgbaImage) -> Option<Region> {
        let rect = layout::content(img)?;
        let aspect = rect.w * img.width() as f64 / (rect.h * img.height() as f64);
        let slide = thumb(img, &rect).is_some_and(|t| classify(&features(&t), true) == Scene::Slide);
        (SLIDE_SHAPES.contains(&aspect) && slide).then_some(rect)
    }

    /// Whether the region still looks at the slide: every few samples, and every one while it seems not to, the
    /// shared content is measured. Two looks in a row that find the region elsewhere give the region to move to.
    fn audit(&mut self, img: &RgbaImage, region: &Region) -> Option<Region> {
        self.since_audit += 1;
        if self.suspect == 0 && self.since_audit < AUDIT_EVERY {
            return None;
        }
        self.since_audit = 0;
        let Some(rect) = Self::edges(img).filter(|r| !layout::agrees(region, r)) else {
            self.suspect = 0;
            return None;
        };
        self.suspect += 1;
        (self.suspect >= 2).then_some(rect)
    }

    /// The window changed size (full screen on or off): its region at that size, remembered or found again
    /// by looking for the last kept slide; None while the person is asked.
    fn resized(&mut self, id: u32, sel: &Selection, w: &WindowInfo) -> Option<Selection> {
        let window = sel.descriptor.label();
        let size = (w.width, w.height);
        let known = sel.at_size(w.width, w.height);
        // A region found or chosen at exactly this size frames the slide there; one within 2% of it may be a few
        // pixels off, so the slide is searched for first.
        if let Some(known) = known.as_ref().filter(|_| sel.sizes.iter().any(|s| (s.width, s.height) == size)) {
            return Some(self.rebind(id, known.clone(), format!("{window} is {} × {} again; watching its slide there", w.width, w.height)));
        }
        let due = match self.tried {
            Some((at, n)) => at != size || n >= SEARCH_AGAIN,
            None => true,
        };
        if !due {
            self.tried = self.tried.map(|(at, n)| (at, n + 1));
        } else {
            self.tried = Some((size, 0));
            let kept = self.detector.kept().cloned();
            if let Ok(img) = self.source.capture(id) {
                if let Some(kept) = kept {
                    let aspect = (sel.region.w * sel.descriptor.width as f64) / (sel.region.h * sel.descriptor.height as f64);
                    if let Some(region) = locate(&img, &kept, aspect, &sel.leave_out) {
                        let found = sel.with_size(w.width, w.height, region);
                        return Some(self.rebind(id, found, format!("{window} is {} × {} now; the slide was found again there", w.width, w.height)));
                    }
                }
                // The kept slide is not on screen (it changed as the window did, or none was kept yet): the
                // slide's own edges say where it is, whichever slide it is.
                if let Some(region) = Self::edges(&img) {
                    let found = sel.with_size(w.width, w.height, region);
                    return Some(self.rebind(id, found, format!("{window} is {} × {} now; the slide was found by its edges there", w.width, w.height)));
                }
            }
        }
        if let Some(known) = known {
            return Some(self.rebind(id, known, format!("{window} is {} × {} now; watching the same region", w.width, w.height)));
        }
        // No window to "Watch it": the old region is what could not be checked at this size.
        let reason = format!("it is {} × {} now and the slide was not found in it; check the region", w.width, w.height);
        self.set(CaptureState::Asking { window, reason, candidates: vec![] });
        None
    }

    /// The same window through another region, and the app is told (it saves the region and says so). The
    /// slide on screen becomes the kept frame when it is the kept slide; a slide that changed as the window did,
    /// or the first one, is left to the detector, so it is taken.
    fn rebind(&mut self, id: u32, sel: Selection, note: String) -> Selection {
        if let Ok((_, t)) = self.frame(id, &sel) {
            if self.detector.shows_kept(&t) {
                self.detector.keep(t);
                self.settling = 2;
            }
        }
        self.bound = Some((id, sel.clone()));
        self.tried = None;
        (self.since_audit, self.suspect) = (AUDIT_EVERY, 0); // the new region is checked on the next sample
        let _ = self.events.send(CaptureEvent::Relocated { selection: sel.clone(), note });
        sel
    }

    /// One capture and its detector input; a blank window or a black region is a failed capture.
    fn frame(&mut self, id: u32, sel: &Selection) -> Result<(RgbaImage, GrayImage), CaptureError> {
        let img = self.source.capture(id)?;
        if is_blank(&img) {
            return Err(CaptureError::Failed("the capture was blank".into()));
        }
        let t = thumb_for(&img, &sel.region, &sel.leave_out).ok_or_else(|| CaptureError::Failed("the slide region was black".into()))?;
        Ok((img, t))
    }

    fn capture_now(&mut self) -> Result<(), String> {
        let Some((id, sel)) = self.bound.clone() else { return Err("No window is being watched: choose one in the slides strip.".into()) };
        let (img, t) = self.frame(id, &sel).map_err(|e| match e {
            CaptureError::Denied => e.to_string(),
            CaptureError::Failed(m) => format!("{} could not be captured: {m}", sel.descriptor.label()),
        })?;
        self.detector.keep(t); // auto capture does not take this slide again
        self.save(&img, &sel.region, Local::now(), false, false)
    }

    /// The region at full size, at most 1600 px, as a PNG temp file in `slides/` (spec §7.2, §7.3).
    fn save(&mut self, img: &RgbaImage, region: &Region, shown_at: DateTime<Local>, auto: bool, uncertain: bool) -> Result<(), String> {
        let slide = crop(img, region).ok_or("the slide region is empty")?;
        let (w, h) = fit_within(slide.width(), slide.height(), MAX_PX);
        let slide = if (w, h) == (slide.width(), slide.height()) { slide } else { slide.resize_exact(w, h, FilterType::Lanczos3) };
        let path = self.cfg.slides.join(format!(".capture-{}.png.tmp", uuid::Uuid::new_v4()));
        slide.save_with_format(&path, image::ImageFormat::Png).map_err(|e| {
            let _ = std::fs::remove_file(&path);
            format!("could not save the slide in {}: {e}", self.cfg.slides.display())
        })?;
        let _ = self.events.send(CaptureEvent::Captured(Captured { path, shown_at, auto, uncertain }));
        Ok(())
    }
}

/// A recording of the detector's input (spec §11): the distinct 256×144 frames, one line per sample,
/// and whole-window copies for re-cropping.
struct Recorder {
    dir: PathBuf,
    started: Instant,
    samples: std::fs::File,
    last_frame: Option<GrayImage>,
    frames: u64,
    last_window: Option<GrayImage>,
    windows: u64,
}

impl Recorder {
    fn new(dir: PathBuf) -> std::io::Result<Self> {
        std::fs::create_dir_all(dir.join("frames"))?;
        std::fs::create_dir_all(dir.join("window"))?;
        let samples = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("samples.jsonl"))?;
        Ok(Self { dir, started: Instant::now(), samples, last_frame: None, frames: 0, last_window: None, windows: 0 })
    }

    fn line(&mut self, v: serde_json::Value) {
        let _ = writeln!(self.samples, "{v}");
    }

    fn sample(&mut self, t: &GrayImage, img: &RgbaImage, scene: Scene) {
        if self.last_frame.as_ref() != Some(t) {
            self.frames += 1;
            let _ = t.save_with_format(self.dir.join(format!("frames/{:05}.png", self.frames)), image::ImageFormat::Png);
            self.last_frame = Some(t.clone());
        }
        let (w, h) = fit_within(img.width(), img.height(), RECORD_WINDOW_PX);
        let whole = image::DynamicImage::ImageLuma8(image::DynamicImage::ImageRgba8(img.clone()).to_luma8()).resize_exact(w, h, FilterType::Triangle).into_luma8();
        if self.last_window.as_ref() != Some(&whole) {
            self.windows += 1;
            let _ = whole.save_with_format(self.dir.join(format!("window/{:05}.png", self.windows)), image::ImageFormat::Png);
            self.last_window = Some(whole);
        }
        let v = serde_json::json!({"t": self.started.elapsed().as_millis() as u64, "at": Local::now().to_rfc3339(), "frame": self.frames, "window": self.windows, "scene": format!("{scene:?}").to_lowercase()});
        self.line(v);
    }

    fn error(&mut self, e: &str) {
        let v = serde_json::json!({"t": self.started.elapsed().as_millis() as u64, "at": Local::now().to_rfc3339(), "error": e});
        self.line(v);
    }
}
