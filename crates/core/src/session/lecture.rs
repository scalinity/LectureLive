//! A whole lecture (spec §3.2, §5.4, §6, §8): the session, a notes worker taking snapshots and polishing
//! one at a time, a slide watcher, and the stop sequence that ends with a last snapshot. The CLI's
//! `lecture` command and the app drive it through `Command`s and read its `Event`s.
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use chrono::{DateTime, Local};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::sync::{oneshot, watch};
use tokio::task::JoinHandle;

use crate::audio::pause::Pausable;
use crate::audio::source::Source;
use crate::capture::detect::Thresholds;
use crate::capture::select::Selection;
use crate::capture::window::WindowSource;
use crate::capture::worker::{self, CaptureCmd, CaptureEvent, CaptureHandle, CaptureMode, CaptureState, WorkerConfig};
use crate::notes::chat::{self, ChatClient, ChatRequest, Content, Image};
use crate::notes::embeds::{clean_output, repair};
use crate::notes::page::{self, PageOutcome};
use crate::notes::timeline::{embed_line, timeline, Batch};
use crate::notes::{context, polish, prompts};
use crate::session::coordinator::{self, Notification, SessionConfig, StopReport, Store};
use crate::session::files::LectureFiles;
use crate::session::folder;

use crate::session::notesfile::{self, sha256_hex};
use crate::session::segments;
use crate::session::sidecar::{PauseSpan, Sidecar, SlideEntry};
use crate::session::slides::{self, SlideMeta};
use crate::session::spend::{Spend, SpendKind};

pub struct Lecture {
    pub files: LectureFiles,
    pub course: String,
    /// The lecture folder's name: the ledger's lecture and the page's title.
    pub name: String,
    /// The notes' title line.
    pub title: String,
    pub chat: ChatClient,
    pub spend: Spend,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    /// Enter, or a hint and Enter.
    Snapshot(String),
    Polish,
}

#[derive(Debug)]
pub enum Command {
    Op(Op),
    /// Stops the notes request in flight and drops the operations queued behind it (spec §6.2):
    /// nothing is written and the batch stays pending.
    Cancel,
    /// The first stops the lecture; a second stops waiting for recovery.
    Stop,
    /// Records and transcribes nothing until `Resume` (spec §9.6), so a break costs no transcription; automatic slide
    /// capture waits too. Notes, snapshots, and slides taken by hand carry on. Pausing a paused lecture does nothing.
    Pause,
    Resume,
    /// Stops the lecture because the app is being quit: everything is saved as a stop saves it, but there is no last
    /// snapshot, no wait for recovery and no operation left running, since a request taking minutes, and costing
    /// money, must not hold up a quit. What the last snapshot would have taken waits for the next session in the
    /// folder (spec §8, §9.6).
    Quit,
    /// A consistent copy of the sidecar: from the session's one writer while it runs, from the file
    /// once the lecture is stopping (spec §3.6, §8).
    State(oneshot::Sender<Result<Sidecar, String>>),
    /// Capture the watched region now, as a manual slide (spec §7.3): the button and the shortcut.
    CaptureNow(oneshot::Sender<Result<(), String>>),
    CaptureManual,
    /// Hold automatic screenshots without pausing audio or transcription.
    SetAutoCapture(bool),
    /// Watch this window through this region: the person's answer to the picker or to an ask.
    Bind { window: u32, selection: Selection },
    /// Images dropped on the app (spec §7.3).
    Import(Vec<PathBuf>),
    /// Remove a slide no snapshot has used (spec §7.5). `reply` answers the caller that waits for the result (the
    /// desktop's command); without one, a refusal is told as a `Warning`, which is how a typed `remove` hears it.
    RemoveSlide { index: u32, reply: Option<oneshot::Sender<Result<(), String>>> },
}

/// Slide capture for a lecture (spec §7): where windows come from, and the course's saved selection.
pub struct CaptureSetup {
    pub source: Box<dyn WindowSource>,
    pub selection: Option<Selection>,
    pub interval: Duration,
    pub thresholds: Thresholds,
    /// Records the detector's input as a fixture (spec §11).
    pub record: Option<PathBuf>,
}

#[derive(Debug)]
pub enum Event {
    Session(Notification),
    Busy(String),
    Preview(String),
    NothingNew,
    /// `revision` is the notes' revision this commit produced.
    Committed { words: usize, slides: usize, block: String, usd: f64, confirmed: bool, removed: usize, missing: usize, revision: u64 },
    SnapshotFailed(String),
    Polished { backup: PathBuf, usd: f64, revision: u64 },
    PolishStopped(String),
    PolishFailed(String),
    /// What was cancelled: "the snapshot" or "the polish". Nothing was written.
    Cancelled(String),
    Page { outcome: PageOutcome, usd: f64 },
    PageFailed(String),
    Slide { index: u32, file: String, auto: bool, uncertain: bool, shown_at: DateTime<Local> },
    /// A piece of the model's summary of its thinking, from a snapshot or a polish: shown while it works and no word
    /// of its answer has streamed. It is not part of the answer.
    Thinking(String),
    /// The person removed a slide no snapshot had used (spec §7.5).
    SlideRemoved { index: u32 },
    /// A snapshot took the slides through this index into the notes: they can no longer be removed.
    SlidesNoted { through: u32 },
    /// The capture worker's state: watching, paused, asking, denied or failing (spec §7.1, §7.4).
    Capture(CaptureState),
    CaptureMode(CaptureMode),
    /// The watched window changed size and its slide is watched through the region for that size: saved by
    /// the adapter for the course, and said.
    CaptureMoved { selection: Selection, note: String },
    /// The lecture was paused (true) or resumed (false) by the person.
    Paused(bool),
    Warning(String),
}

#[derive(Debug, Clone)]
pub struct SlideWatch {
    /// Where macOS saves screenshots; those taken while the lecture is recording are taken in as slides, and none
    /// taken while it is paused, before it started or after it began stopping.
    pub screenshots: Option<PathBuf>,
    pub poll: Duration,
}

/// A notes operation's cancel signal: `Command::Cancel` moves the generation on, and an operation
/// taken at an earlier generation stops before its commit.
pub struct Cancel {
    rx: watch::Receiver<u64>,
    at: u64,
}

impl Cancel {
    /// A signal that never fires.
    pub fn never() -> Self {
        let (_, rx) = watch::channel(0);
        Self { rx, at: 0 }
    }

    async fn cancelled(&mut self) {
        loop {
            if *self.rx.borrow_and_update() > self.at {
                return;
            }
            if self.rx.changed().await.is_err() {
                std::future::pending::<()>().await; // nobody can cancel any more
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum OpError {
    Cancelled,
    Failed(String),
}

impl From<String> for OpError {
    fn from(m: String) -> Self {
        Self::Failed(m)
    }
}

/// Where macOS saves screenshots (`defaults read com.apple.screencapture location`), else the Desktop.
pub fn screenshot_dir() -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    let out = std::process::Command::new("defaults").args(["read", "com.apple.screencapture", "location"]).output().ok();
    let found = out.filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).map(|raw| raw.strip_prefix("~/").map_or_else(|| PathBuf::from(&raw), |r| home.join(r)));
    Some(found.filter(|p| p.is_dir()).unwrap_or_else(|| home.join("Desktop")))
}

const NOTES_TIMEOUT: Duration = Duration::from_secs(600);

impl Lecture {
    /// Spec §6.1–6.2: cutoff, batch after the cursors, one streamed request, repair, journaled commit.
    pub async fn snapshot(&self, store: &Store, hint: &str, events: &UnboundedSender<Event>) -> Result<(), String> {
        self.snapshot_with(store, hint, events, &mut Cancel::never()).await.map_err(|e| match e {
            OpError::Cancelled => "cancelled".to_string(),
            OpError::Failed(m) => m,
        })
    }

    /// A snapshot whose request `cancel` can stop; once the answer is in, the commit is not raced, so
    /// a cancel never leaves a commit half reported.
    pub async fn snapshot_with(&self, store: &Store, hint: &str, events: &UnboundedSender<Event>, cancel: &mut Cancel) -> Result<(), OpError> {
        let text = |e: anyhow::Error| format!("{e:#}");
        let at = Local::now();
        let cut = store.cutoff().await.map_err(text)?;
        let sc = store.read().await.map_err(text)?;
        let log = self.files.segments();
        let upto = match cut {
            Some(c) => c.segments,
            None => segments::read(&log).map_err(text)?.len() as u64,
        };
        let mut batch = Batch::take(&log, &sc, upto, hint).map_err(text)?;
        // A slide whose file was deleted after it was registered is skipped, and the cursor still
        // moves past it: one missing file must not stop the notes for the rest of the lecture.
        let slide_to = batch.slide_to(sc.notes.slide_index);
        let (present, missing): (Vec<_>, Vec<_>) = std::mem::take(&mut batch.slides).into_iter().partition(|s| self.files.dir.join(&s.file).exists());
        batch.slides = present;
        for s in &missing {
            let _ = events.send(Event::Warning(format!("slide {} ({}) is no longer on disk; left out of the notes", s.index, s.file)));
        }
        if batch.is_empty() && !missing.is_empty() {
            store.update(move |sc| {
                sc.notes.slide_index = sc.notes.slide_index.max(slide_to);
                Ok(())
            })
            .await
            .map_err(text)?;
            let _ = events.send(Event::SlidesNoted { through: slide_to });
        }
        if batch.is_empty() {
            let _ = events.send(Event::NothingNew);
            return Ok(());
        }
        let doc = std::fs::read_to_string(&self.files.notes).map_err(|e| format!("read {}: {e}", self.files.notes.display()))?;
        let notes_dir = self.files.notes_dir();
        let embeds: Vec<String> = batch.slides.iter().map(|s| embed_line(s, &self.files.dir, notes_dir)).collect();
        let images = batch.slides.iter().map(|s| Image::read(&self.files.dir.join(&s.file))).collect::<Result<Vec<_>>>().map_err(text)?;
        let timeline = timeline(&batch.segments, &batch.slides, &self.files.dir, notes_dir);
        let system = prompts::notes_system(&self.course);
        let rest = context::text_tokens(&system) + context::text_tokens(&prompts::notes_user("", &timeline, &embeds, hint)) + images.len() * context::IMAGE_TOKENS;
        let doc_context = context::doc_context(&doc, rest, context::BUDGET_TOKENS);
        let req = ChatRequest { what: SpendKind::Notes, system, content: Content::Parts { text: prompts::notes_user(&doc_context, &timeline, &embeds, hint), images }, effort: None, timeout: NOTES_TIMEOUT };
        let _ = events.send(Event::Busy(format!("snapshot, {} words to {}", batch.words(), chat::MODEL)));
        let preview = events.clone();
        let mut on_delta = |d: &str| drop(preview.send(Event::Preview(d.to_string())));
        let thinking = events.clone();
        let mut on_thinking = |d: &str| drop(thinking.send(Event::Thinking(d.to_string())));
        let answer = tokio::select! {
            a = self.chat.complete_with_thinking(&req, &mut on_delta, &mut on_thinking) => a.map_err(|e| e.to_string())?,
            _ = cancel.cancelled() => return Err(OpError::Cancelled),
        };
        if let Some(w) = answer.warning {
            let _ = events.send(Event::Warning(w));
        }
        let repaired = repair(&clean_output(&answer.text, true), &embeds);
        let block = notesfile::block(at, &repaired.text);
        let (files, b, to) = (self.files.clone(), block.clone(), batch.positions.end);
        let revision = store
            .update(move |sc| {
                notesfile::commit(&files, sc, &b, to, slide_to)?;
                Ok(sc.notes.revision)
            })
            .await
            .map_err(text)?;
        let _ = events.send(Event::Committed { words: batch.words(), slides: batch.slides.len(), block, usd: answer.usd.unwrap_or(0.0), confirmed: cut.is_none_or(|c| c.confirmed), removed: repaired.removed, missing: repaired.missing.len(), revision });
        let _ = events.send(Event::SlidesNoted { through: slide_to });
        Ok(())
    }

    /// Spec §6.3: a snapshot first, and nothing if it fails; true when the notes were polished.
    pub async fn polish(&self, store: &Store, events: &UnboundedSender<Event>) -> bool {
        self.polish_with(store, events, &mut Cancel::never()).await
    }

    /// A polish whose snapshot and request `cancel` can stop.
    pub async fn polish_with(&self, store: &Store, events: &UnboundedSender<Event>, cancel: &mut Cancel) -> bool {
        match self.snapshot_with(store, "", events, cancel).await {
            Ok(()) => {}
            Err(OpError::Cancelled) => {
                let _ = events.send(Event::Cancelled("the polish".into()));
                return false;
            }
            Err(OpError::Failed(e)) => {
                let _ = events.send(Event::PolishStopped(format!("the snapshot before it failed ({e}); the notes are unchanged")));
                return false;
            }
        }
        let doc = match std::fs::read_to_string(&self.files.notes) {
            Ok(d) => d,
            Err(e) => {
                let _ = events.send(Event::PolishFailed(format!("read {}: {e}", self.files.notes.display())));
                return false;
            }
        };
        let transcript = std::fs::read_to_string(&self.files.transcript).unwrap_or_default();
        let _ = events.send(Event::Busy(format!("polishing {} words", doc.split_whitespace().count())));
        let request = polish::request(&self.course, &self.title, &doc, &transcript);
        // A polish is replaced as a whole once it is validated, so its words do not stream; its thinking does.
        let mut no_delta = |_: &str| {};
        let thinking = events.clone();
        let mut on_thinking = |d: &str| drop(thinking.send(Event::Thinking(d.to_string())));
        let answer = tokio::select! {
            a = self.chat.complete_with_thinking(&request, &mut no_delta, &mut on_thinking) => a,
            _ = cancel.cancelled() => {
                let _ = events.send(Event::Cancelled("the polish".into()));
                return false;
            }
        };
        let answer = match answer {
            Ok(a) => a,
            Err(e) => {
                let _ = events.send(Event::PolishFailed(format!("{e}; the notes are unchanged")));
                return false;
            }
        };
        if let Some(w) = answer.warning.clone() {
            let _ = events.send(Event::Warning(w)); // the ledger could not be written; the answer is kept
        }
        let text = polish::validate(&answer.text, &doc).text;
        let (files, based_on) = (self.files.clone(), sha256_hex(doc.as_bytes()));
        let replaced = store.update(move |sc| {
            let backup = notesfile::replace(&files, sc, &text, &based_on, Local::now())?;
            Ok((backup, sc.notes.revision))
        });
        match replaced.await {
            Ok((backup, revision)) => {
                let _ = events.send(Event::Polished { backup, usd: answer.usd.unwrap_or(0.0), revision });
                true
            }
            Err(e) => {
                let _ = events.send(Event::PolishFailed(format!("{e:#}")));
                false
            }
        }
    }

    /// Polish after the lecture has ended (decision D1). The folder is opened as a lecture opens it, so a commit a
    /// crash interrupted is finished or undone and an edit made after class is accepted before anything reads the
    /// notes; then the polish, and the page after it as during a lecture. The caller holds the folder lock.
    pub async fn polish_after(&self, events: &UnboundedSender<Event>) -> Result<(), String> {
        // Opening a folder migrates the Python CLI's state one way, and the Python CLI is still the in-class tool.
        if !self.files.sidecar().exists() {
            return Err("Polish works on a lecture LectureLive recorded, and this folder has none yet. It is left as it is, so the Python CLI can still use it.".into());
        }
        let (sc, _) = folder::open(&self.files, &self.title, false).map_err(|e| format!("{e:#}"))?;
        if self.polish(&Store::offline(sc, self.files.sidecar()), events).await {
            let _ = self.page(events).await;
        }
        Ok(())
    }

    /// Spec §6.4: the study page from the notes as they are, without polishing again. One typeset runs at a time: a
    /// polish starts its own page in the background, and a Study page pressed meanwhile used to start a second
    /// full typeset, which cost the same again. It now waits, and finds the first one's cache.
    pub async fn page(&self, events: &UnboundedSender<Event>) -> Result<PageOutcome, String> {
        let _one_at_a_time = PAGE_LOCK.lock().await;
        let before = self.spend.kind_total(SpendKind::Page);
        let progress = events.clone();
        let made = page::make_page(&self.chat, &self.course, &self.files.notes, &self.files.page_cache(), &self.name, page::TEMPLATE, &move |m| drop(progress.send(Event::Busy(m)))).await;
        match made {
            Ok(outcome) => {
                let _ = events.send(Event::Page { outcome: outcome.clone(), usd: self.spend.kind_total(SpendKind::Page) - before });
                Ok(outcome)
            }
            Err(e) => {
                let m = format!("{e:#}. The notes are unchanged; `lecture page` tries again.");
                let _ = events.send(Event::PageFailed(m.clone()));
                Err(m)
            }
        }
    }
}

/// A removal that was refused: told to the caller that waits for an answer, else as a warning in the event stream.
fn refused(reply: Option<oneshot::Sender<Result<(), String>>>, events: &UnboundedSender<Event>, why: String) {
    match reply {
        Some(r) => drop(r.send(Err(why))),
        None => drop(events.send(Event::Warning(why))),
    }
}

/// Held for the whole of a typeset, so that a second request for the same notes waits for the first instead of paying
/// for another (`Lecture::page`). Dropped with the typeset when it is cancelled.
static PAGE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Runs operations one at a time; each is tagged with the cancel generation it was asked at.
async fn notes_worker(lec: Arc<Lecture>, store: Store, mut ops: UnboundedReceiver<(Op, u64)>, events: UnboundedSender<Event>, pages: Arc<Mutex<Vec<JoinHandle<()>>>>, hurry: Arc<AtomicBool>, snapshotting: Arc<AtomicBool>, generation: watch::Receiver<u64>) {
    while let Some((op, asked_at)) = ops.recv().await {
        if hurry.load(Ordering::Relaxed) {
            continue; // a second stop: what was still queued is dropped; the last snapshot takes its material
        }
        if asked_at < *generation.borrow() {
            continue; // cancelled while it waited
        }
        let mut cancel = Cancel { rx: generation.clone(), at: asked_at };
        // Set before the batch is taken and kept until the cursor has moved: a slide removed in between would be
        // named by the notes after its file had gone (`slides::remove` looks at this inside the sidecar's writer).
        snapshotting.store(true, Ordering::SeqCst);
        match op {
            Op::Snapshot(hint) => match lec.snapshot_with(&store, &hint, &events, &mut cancel).await {
                Ok(()) => {}
                Err(OpError::Cancelled) => {
                    let _ = events.send(Event::Cancelled("the snapshot".into()));
                }
                Err(OpError::Failed(e)) => {
                    let _ = events.send(Event::SnapshotFailed(format!("{e}; everything is kept for the next one")));
                }
            },
            Op::Polish => {
                if lec.polish_with(&store, &events, &mut cancel).await {
                    // Typesetting takes minutes and reads only the polished file: snapshots are not held up by it.
                    let (l, ev) = (lec.clone(), events.clone());
                    pages.lock().expect("the page list").push(tokio::spawn(async move {
                        let _ = l.page(&ev).await;
                    }));
                }
            }
        }
        snapshotting.store(false, Ordering::SeqCst);
    }
}

fn listing(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir).map(|e| e.flatten().map(|e| e.path()).collect()).unwrap_or_default()
}

/// Whether the lecture is recording, for what may be taken in from the screenshot folder: a screenshot is a slide only
/// if it was taken while the lecture was recording, so one taken in a pause is never a slide, not even after the resume.
#[derive(Debug, Clone, Copy)]
struct Recording {
    paused: bool,
    /// The start, or the last resume: screenshots older than this were taken before the lecture was recording.
    since: SystemTime,
}

/// The CLI's `slide_watcher`: images dropped into `slides/`, and, while the lecture records, screenshots taken since
/// it started or last resumed, once their size has stopped changing.
async fn slide_watcher(lec: Arc<Lecture>, store: Store, watch: SlideWatch, recording: tokio::sync::watch::Receiver<Recording>, mut stop: tokio::sync::watch::Receiver<bool>, events: UnboundedSender<Event>) {
    let mut known: HashSet<PathBuf> = listing(&lec.files.slides).into_iter().collect();
    let mut sizes: HashMap<PathBuf, u64> = HashMap::new();
    loop {
        let mut candidates = listing(&lec.files.slides);
        let rec = *recording.borrow();
        if let (Some(dir), false) = (&watch.screenshots, rec.paused) {
            candidates.extend(listing(dir).into_iter().filter(|p| {
                p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("Screen")) && std::fs::metadata(p).and_then(|m| m.modified()).is_ok_and(|t| t >= rec.since)
            }));
        }
        for p in candidates {
            if known.contains(&p) || !slides::is_image(&p) {
                continue;
            }
            let Ok(size) = std::fs::metadata(&p).map(|m| m.len()) else { continue };
            if sizes.insert(p.clone(), size) != Some(size) {
                continue; // still being written
            }
            known.insert(p.clone());
            let registered = match slides::file_time(&p) {
                Ok(t) => slides::register(&lec.files, &store, &p, SlideMeta::manual(t)).await,
                Err(e) => Err(e),
            };
            match registered {
                Ok(Some(s)) => {
                    known.insert(lec.files.dir.join(&s.file));
                    let _ = events.send(slide_event(&s));
                }
                Ok(None) => {} // another path registered it
                Err(e) => {
                    let _ = events.send(Event::Warning(format!("slide {}: {e:#}", p.display())));
                }
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(watch.poll) => {}
            _ = stop.changed() => return,
        }
    }
}

/// A whole lecture: runs until stopped (or until its audio ends), then the last snapshot.
pub async fn run(lec: Arc<Lecture>, cfg: SessionConfig, source: Box<dyn Source>, watch: SlideWatch, capture: Option<CaptureSetup>, mut commands: UnboundedReceiver<Command>, events: UnboundedSender<Event>) -> Result<StopReport> {
    let (source, pause) = Pausable::wrap(source);
    let (handle, mut notes, store) = coordinator::spawn_with_store(cfg, source);
    let pages = Arc::new(Mutex::new(Vec::new()));
    let (op_tx, op_rx) = mpsc::unbounded_channel();
    let hurry = Arc::new(AtomicBool::new(false));
    let snapshotting = Arc::new(AtomicBool::new(false));
    let (generation, generation_rx) = watch::channel(0u64);
    let worker = tokio::spawn(notes_worker(lec.clone(), store.clone(), op_rx, events.clone(), pages.clone(), hurry.clone(), snapshotting.clone(), generation_rx));
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let (recording_tx, recording_rx) = watch::channel(Recording { paused: false, since: SystemTime::now() });
    let watcher = tokio::spawn(slide_watcher(lec.clone(), store.clone(), watch, recording_rx, stop_rx, events.clone()));
    let (mut capture, capturer) = match capture {
        Some(c) => {
            let (tx, rx) = mpsc::unbounded_channel();
            let cfg = WorkerConfig { slides: lec.files.slides.clone(), interval: c.interval, thresholds: c.thresholds, record: c.record };
            (Some(worker::spawn(c.source, c.selection, cfg, tx)), Some(tokio::spawn(capture_task(lec.clone(), store.clone(), rx, events.clone()))))
        }
        None => (None, None),
    };
    // The pauses go into the sidecar in order, through one task that holds a store until the lecture stops.
    let (pause_tx, pause_rx) = mpsc::unbounded_channel();
    let pause_task = tokio::spawn(pause_log(store.clone(), pause_rx, events.clone()));
    let mut pause_tx = Some(pause_tx);
    let (mut paused, mut leaving) = (false, false);
    // Serves state reads until the lecture begins stopping: the session ends only once every store is dropped.
    let mut state_store = Some(store);
    let mut op_tx = Some(op_tx);
    let (mut stops, mut commands_open) = (0, true);
    let stopping = AtomicBool::new(false);
    let begin_stop = |op_tx: &mut Option<UnboundedSender<(Op, u64)>>, state_store: &mut Option<Store>, capture: &mut Option<CaptureHandle>, pause_tx: &mut Option<UnboundedSender<PauseLog>>, paused: &mut bool| {
        *state_store = None;
        *capture = None; // the worker stops; what it already saved is still registered
        if let Some(tx) = pause_tx.take() {
            if std::mem::take(paused) {
                let _ = tx.send(PauseLog::End(Local::now())); // the lecture ended in a pause: the pause ends with it
            }
        }
        if !stopping.swap(true, Ordering::Relaxed) {
            let _ = stop_tx.send(true);
            *op_tx = None; // queued operations still run; nothing new is taken
        }
    };
    loop {
        tokio::select! {
            n = notes.recv() => match n {
                Some(Notification::SourceEnded) => {
                    begin_stop(&mut op_tx, &mut state_store, &mut capture, &mut pause_tx, &mut paused);
                    let _ = events.send(Event::Session(Notification::SourceEnded));
                }
                Some(n) => { let _ = events.send(Event::Session(n)); }
                None => break,
            },
            c = commands.recv(), if commands_open => match c {
                Some(Command::Op(op)) => if let Some(tx) = &op_tx { let _ = tx.send((op, *generation.borrow())); },
                Some(Command::Cancel) => {
                    generation.send_modify(|g| *g += 1);
                    // A study page being typeset in the background (a polish starts one) is stopped too: it is not an
                    // operation of the queue, so the generation does not reach it.
                    let mut typesetting = pages.lock().expect("the page list");
                    if typesetting.iter().any(|p| !p.is_finished()) {
                        typesetting.drain(..).for_each(|p| p.abort());
                        let _ = events.send(Event::Cancelled("the study page".into()));
                    }
                }
                Some(Command::State(reply)) => match &state_store {
                    Some(s) => {
                        let s = s.clone();
                        tokio::spawn(async move { let _ = reply.send(s.read().await.map_err(|e| format!("{e:#}"))); });
                    }
                    None => { let _ = reply.send(read_sidecar(&lec.files)); }
                },
                Some(Command::Stop) => {
                    stops += 1;
                    handle.request_stop();
                    begin_stop(&mut op_tx, &mut state_store, &mut capture, &mut pause_tx, &mut paused);
                    if stops >= 2 {
                        hurry.store(true, Ordering::Relaxed);
                    }
                }
                Some(Command::Quit) => {
                    leaving = true;
                    generation.send_modify(|g| *g += 1); // an operation in flight is cancelled: nothing is written, all is kept
                    hurry.store(true, Ordering::Relaxed);
                    handle.request_stop();
                    handle.request_stop(); // a second stop: recovery is not waited for either
                    begin_stop(&mut op_tx, &mut state_store, &mut capture, &mut pause_tx, &mut paused);
                }
                Some(Command::Pause) => {
                    if stopping.load(Ordering::Relaxed) {
                        let _ = events.send(Event::Warning("The lecture is stopping; it cannot be paused.".into()));
                    } else if !paused {
                        paused = true;
                        pause.set(true);
                        recording_tx.send_replace(Recording { paused: true, since: SystemTime::now() });
                        if let Some(c) = &capture { c.send(CaptureCmd::Hold(true)) }
                        if let Some(tx) = &pause_tx { let _ = tx.send(PauseLog::Begin(Local::now())); }
                        let _ = events.send(Event::Paused(true));
                    }
                }
                Some(Command::Resume) => if paused {
                    paused = false;
                    pause.set(false);
                    recording_tx.send_replace(Recording { paused: false, since: SystemTime::now() });
                    if let Some(c) = &capture { c.send(CaptureCmd::Hold(false)) }
                    if let Some(tx) = &pause_tx { let _ = tx.send(PauseLog::End(Local::now())); }
                    let _ = events.send(Event::Paused(false));
                },
                Some(Command::SetAutoCapture(enabled)) => match &capture {
                    Some(c) => c.send(CaptureCmd::SetAutoCapture(enabled)),
                    None => { let _ = events.send(Event::Warning("Capture is unavailable or the lecture is stopping.".into())); }
                },
                Some(Command::CaptureManual) => match &capture {
                    Some(c) => c.send(CaptureCmd::Manual),
                    None => { let _ = events.send(Event::Warning("Capture is unavailable or the lecture is stopping.".into())); }
                },
                Some(Command::CaptureNow(reply)) => match &capture {
                    Some(c) => c.send(CaptureCmd::Now(reply)),
                    None if stopping.load(Ordering::Relaxed) => { let _ = reply.send(Err("The lecture is stopping.".into())); }
                    None => { let _ = reply.send(Err("No window is being watched: choose one in the slides strip.".into())); }
                },
                Some(Command::Bind { window, selection }) => if let Some(c) = &capture { c.send(CaptureCmd::Bind { window, selection }) },
                Some(Command::Import(paths)) => match &state_store {
                    Some(s) => {
                        let (lec, s, events) = (lec.clone(), s.clone(), events.clone());
                        tokio::spawn(async move {
                            for (_, r) in slides::import(&lec.files, &s, &paths).await {
                                let _ = events.send(match r {
                                    Ok(slide) => slide_event(&slide),
                                    Err(m) => Event::Warning(m),
                                });
                            }
                        });
                    }
                    None => { let _ = events.send(Event::Warning("The lecture is stopping; images dropped now are not added.".into())); }
                },
                Some(Command::RemoveSlide { index, reply }) => match &state_store {
                    Some(s) => {
                        let (lec, s, events, snapshotting) = (lec.clone(), s.clone(), events.clone(), snapshotting.clone());
                        tokio::spawn(async move {
                            match slides::remove(&lec.files, &s, index, snapshotting).await {
                                Ok(_) => {
                                    let _ = events.send(Event::SlideRemoved { index });
                                    if let Some(r) = reply {
                                        let _ = r.send(Ok(()));
                                    }
                                }
                                Err(e) => refused(reply, &events, format!("{e:#}")),
                            }
                        });
                    }
                    None => refused(reply, &events, "The lecture is stopping.".into()),
                },
                None => commands_open = false, // input closed: the lecture runs until it is stopped or its audio ends
            },
        }
    }
    begin_stop(&mut op_tx, &mut state_store, &mut capture, &mut pause_tx, &mut paused);
    let _ = watcher.await;
    let _ = pause_task.await;
    if let Some(c) = capturer {
        let _ = c.await;
    }
    let _ = worker.await;
    let result = handle.finish().await;
    for p in pages.lock().expect("the page list").drain(..) {
        if !p.is_finished() {
            p.abort();
            let _ = events.send(Event::Warning("the page was still being typeset; `lecture page` finishes it later".into()));
        }
    }
    // The last snapshot, after recovery has drained (spec §5.4), on the sidecar the session left: also after a
    // session that failed (a full disk), so what was logged reaches the notes before the failure is reported (§10).
    // Its failure goes into the report too, so the end never reads as if the notes were complete.
    // A quit takes none: a request that takes minutes, and costs money, must not hold the app up, and everything it
    // would have taken is pending for the next session in the folder, as after a crash.
    let failed = if leaving {
        None
    } else {
        match Sidecar::load(&lec.files.sidecar()).and_then(|sc| sc.context("the session left no sidecar")) {
            Ok(sc) => lec.snapshot(&Store::offline(sc, lec.files.sidecar()), "", &events).await.err(),
            Err(e) if result.is_err() => Some(format!("{e:#}")),
            Err(e) => return Err(e),
        }
    };
    if let Some(e) = &failed {
        let _ = events.send(Event::SnapshotFailed(format!("{e}; everything is kept for the next one")));
    }
    result.map(|mut report| {
        report.last_snapshot = if leaving { Some("the app was quit first".to_string()) } else { failed };
        report
    })
}

/// A pause beginning or ending, for the sidecar.
enum PauseLog {
    Begin(DateTime<Local>),
    End(DateTime<Local>),
}

/// Writes each pause into the sidecar, in the order they were asked, through the one writer; it ends when the lecture
/// stops and its channel closes.
async fn pause_log(store: Store, mut rx: UnboundedReceiver<PauseLog>, events: UnboundedSender<Event>) {
    while let Some(m) = rx.recv().await {
        let saved = store
            .update(move |sc| {
                match m {
                    PauseLog::Begin(at) => sc.pauses.push(PauseSpan { from: at, to: None }),
                    PauseLog::End(at) => {
                        if let Some(p) = sc.pauses.iter_mut().rev().find(|p| p.to.is_none()) {
                            p.to = Some(at);
                        }
                    }
                }
                Ok(())
            })
            .await;
        if let Err(e) = saved {
            let _ = events.send(Event::Warning(format!("the pause could not be recorded: {e:#}")));
        }
    }
}

/// The sidecar as its file holds it: the one writer saves it atomically before every answer (spec §8).
fn read_sidecar(files: &LectureFiles) -> Result<Sidecar, String> {
    Sidecar::load(&files.sidecar()).map_err(|e| format!("{e:#}"))?.ok_or_else(|| "the lecture has no sidecar yet".to_string())
}

fn slide_event(s: &SlideEntry) -> Event {
    Event::Slide { index: s.index, file: s.file.clone(), auto: s.auto, uncertain: s.uncertain, shown_at: s.shown_at }
}

/// The capture worker's results: each saved slide goes through the one registration path, and each
/// state reaches the adapters. It ends when the worker stops, releasing its store.
async fn capture_task(lec: Arc<Lecture>, store: Store, mut rx: UnboundedReceiver<CaptureEvent>, events: UnboundedSender<Event>) {
    while let Some(e) = rx.recv().await {
        match e {
            CaptureEvent::Mode(mode) => { let _ = events.send(Event::CaptureMode(mode)); }
            CaptureEvent::Warning(reason) => { let _ = events.send(Event::Warning(reason)); }
            CaptureEvent::Relocated { selection, note } => {
                let _ = events.send(Event::CaptureMoved { selection, note });
            }
            CaptureEvent::State(s) => {
                let _ = events.send(Event::Capture(s));
            }
            CaptureEvent::Captured(c) => match slides::register(&lec.files, &store, &c.path, SlideMeta { shown_at: c.shown_at, auto: c.auto, uncertain: c.uncertain }).await {
                Ok(Some(s)) => {
                    let _ = events.send(slide_event(&s));
                }
                Ok(None) => {}
                Err(e) => {
                    let _ = std::fs::remove_file(&c.path);
                    let _ = events.send(Event::Warning(format!("slide capture: {e:#}")));
                }
            },
        }
    }
}
