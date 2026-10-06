//! Replays a verification recording (LECTURELIVE_RECORD, spec §11) through the real capture worker: each
//! sample's whole-window copy is what the window server returns for that second, so the worker's decisions
//! (slides kept, regions found again) can be read against a class that already happened. The recording is
//! local evidence and is never committed.
//!
//!   cargo run -p lecturelive-core --example replay -- --rec <dir> [--selection capture.json] [--course m71-class]
//!       [--from 16:29:50] [--to 16:36:00] [--out <dir>] [--base-width 1168]
//!
//! Window sizes are rebuilt from each copy's shape at `--base-width`, since a recording keeps no sizes.
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Local};
use image::RgbaImage;
use lecturelive_core::capture::detect::Thresholds;
use lecturelive_core::capture::select::Selections;
use lecturelive_core::capture::window::{CaptureError, WindowInfo, WindowSource};
use lecturelive_core::capture::worker::{self, CaptureCmd, CaptureEvent, WorkerConfig};

struct Sample {
    at: String,
    window: u32,
}

struct Replay {
    dir: PathBuf,
    samples: Arc<Vec<Sample>>,
    /// The sample the worker is on, how many times a tick has asked, and when each was served.
    cur: Arc<AtomicUsize>,
    served: Arc<Mutex<Vec<DateTime<Local>>>>,
    done: Arc<AtomicBool>,
    base_width: u32,
    cache: Option<(u32, RgbaImage)>,
}

impl Replay {
    fn path(&self, window: u32) -> PathBuf {
        self.dir.join(format!("window/{window:05}.png"))
    }
}

impl WindowSource for Replay {
    /// One call per tick: the next sample.
    fn windows(&mut self) -> Result<Vec<WindowInfo>, CaptureError> {
        let i = self.cur.load(Ordering::SeqCst);
        let next = (i + 1).min(self.samples.len() - 1);
        if i + 1 >= self.samples.len() {
            self.done.store(true, Ordering::SeqCst);
        }
        self.cur.store(next, Ordering::SeqCst);
        self.served.lock().unwrap().push(Local::now());
        let (w, h) = image::image_dimensions(self.path(self.samples[next].window)).map_err(|e| CaptureError::Failed(e.to_string()))?;
        let height = (self.base_width as f64 * h as f64 / w as f64).round() as u32;
        Ok(vec![WindowInfo { id: 938, app: "Zoom".into(), bundle_id: Some("us.zoom.xos".into()), title: "Zoom Meeting".into(), width: self.base_width, height, on_screen: true }])
    }

    fn capture(&mut self, _id: u32) -> Result<RgbaImage, CaptureError> {
        let window = self.samples[self.cur.load(Ordering::SeqCst)].window;
        if self.cache.as_ref().map(|(w, _)| *w) != Some(window) {
            let img = image::open(self.path(window)).map_err(|e| CaptureError::Failed(e.to_string()))?.to_rgba8();
            self.cache = Some((window, img));
        }
        Ok(self.cache.as_ref().unwrap().1.clone())
    }
}

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rec = PathBuf::from(arg(&args, "--rec").expect("--rec <recording dir>"));
    let home = std::env::var("HOME").unwrap_or_default();
    let selection_file = arg(&args, "--selection").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(format!("{home}/Library/Application Support/LectureLive/capture.json")));
    let course = arg(&args, "--course").unwrap_or_else(|| "m71-class".into());
    let (from, to) = (arg(&args, "--from").unwrap_or_default(), arg(&args, "--to").unwrap_or_else(|| "99".into()));
    let base_width: u32 = arg(&args, "--base-width").and_then(|v| v.parse().ok()).unwrap_or(1168);
    let out = PathBuf::from(arg(&args, "--out").unwrap_or_else(|| "/tmp/lecturelive-replay".into()));
    std::fs::create_dir_all(&out).expect("create --out");

    let text = std::fs::read_to_string(rec.join("samples.jsonl")).expect("samples.jsonl");
    let samples: Vec<Sample> = text
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter_map(|v| Some(Sample { at: v.get("at")?.as_str()?.to_string(), window: v.get("window")?.as_u64()? as u32 }))
        .filter(|s| s.window > 0 && s.at[11..19] >= *from.as_str() && s.at[11..19] <= *to.as_str())
        .collect();
    assert!(samples.len() > 2, "no samples between {from} and {to}");
    let selection = Selections::load(&selection_file).expect("capture.json").get(&course).cloned().expect("the course's selection in capture.json");
    println!("replaying {} samples, {} to {}; region {:?}, leave out {:?}", samples.len(), &samples[0].at[11..19], &samples[samples.len() - 1].at[11..19], selection.region, selection.leave_out);

    let samples = Arc::new(samples);
    let (cur, served, done) = (Arc::new(AtomicUsize::new(0)), Arc::new(Mutex::new(Vec::new())), Arc::new(AtomicBool::new(false)));
    let source = Replay { dir: rec, samples: samples.clone(), cur: cur.clone(), served: served.clone(), done: done.clone(), base_width, cache: None };
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let cfg = WorkerConfig { slides: out.clone(), interval: Duration::from_millis(30), thresholds: Thresholds::default(), record: None };
    let handle = worker::spawn(Box::new(source), Some(selection), cfg, tx);

    let (mut slides, mut moves, mut idle) = (0, 0, 0);
    let at = |i: usize| samples[i.min(samples.len() - 1)].at[11..19].to_string();
    loop {
        let mut quiet = true;
        while let Ok(e) = rx.try_recv() {
            quiet = false;
            let now = cur.load(Ordering::SeqCst);
            match e {
                CaptureEvent::State(s) => println!("{}  state    {:?}", at(now), s),
                CaptureEvent::Relocated { selection, note } => {
                    moves += 1;
                    let r = selection.region;
                    println!("{}  region   {note}  -> x {:.3} y {:.3} w {:.3} h {:.3}", at(now), r.x, r.y, r.w, r.h);
                }
                CaptureEvent::Captured(c) => {
                    slides += 1;
                    let seen = served.lock().unwrap().partition_point(|t| *t <= c.shown_at).saturating_sub(1);
                    let (w, h) = image::ImageReader::open(&c.path).ok().and_then(|r| r.with_guessed_format().ok()).and_then(|r| r.into_dimensions().ok()).unwrap_or((0, 0));
                    let name = out.join(format!("slide_{slides:02}_{}.png", at(seen).replace(':', "")));
                    let _ = std::fs::rename(&c.path, &name);
                    println!("{}  SLIDE {slides:02} first on screen {}  {}x{}  auto={} uncertain={}", at(now), at(seen), w, h, c.auto, c.uncertain);
                }
            }
        }
        idle = if quiet { idle + 1 } else { 0 };
        if done.load(Ordering::SeqCst) && idle > 60 {
            break;
        }
        std::thread::sleep(Duration::from_millis(15));
    }
    handle.send(CaptureCmd::Stop);
    println!("done: {slides} slides, {moves} region changes; slides saved in {}", out.display());
}
