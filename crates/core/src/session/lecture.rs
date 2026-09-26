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

use crate::audio::source::Source;
use crate::notes::chat::{self, ChatClient, ChatRequest, Content, Image};
use crate::notes::embeds::{clean_output, repair};
use crate::notes::page::{self, PageOutcome};
use crate::notes::timeline::{embed_line, timeline, Batch};
use crate::notes::{context, polish, prompts};
use crate::session::coordinator::{self, Notification, SessionConfig, StopReport, Store};
use crate::session::files::LectureFiles;

use crate::session::notesfile::{self, sha256_hex};
use crate::session::segments;
use crate::session::sidecar::{Sidecar, SlideEntry};
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
    /// A consistent copy of the sidecar: from the session's one writer while it runs, from the file
    /// once the lecture is stopping (spec §3.6, §8).
    State(oneshot::Sender<Result<Sidecar, String>>),
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
    Warning(String),
}

#[derive(Debug, Clone)]
pub struct SlideWatch {
    /// Where macOS saves screenshots; those taken after the lecture started are taken in as slides.
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
        let answer = tokio::select! {
            a = self.chat.complete(&req, &mut on_delta) => a.map_err(|e| e.to_string())?,
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
        let mut no_delta = |_: &str| {};
        let answer = tokio::select! {
            a = self.chat.complete(&request, &mut no_delta) => a,
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

    /// Spec §6.4: the study page from the notes as they are, without polishing again.
    pub async fn page(&self, events: &UnboundedSender<Event>) -> Result<PageOutcome, String> {
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

/// Runs operations one at a time; each is tagged with the cancel generation it was asked at.
async fn notes_worker(lec: Arc<Lecture>, store: Store, mut ops: UnboundedReceiver<(Op, u64)>, events: UnboundedSender<Event>, pages: Arc<Mutex<Vec<JoinHandle<()>>>>, hurry: Arc<AtomicBool>, generation: watch::Receiver<u64>) {
    while let Some((op, asked_at)) = ops.recv().await {
        if hurry.load(Ordering::Relaxed) {
            continue; // a second stop: what was still queued is dropped; the last snapshot takes its material
        }
        if asked_at < *generation.borrow() {
            continue; // cancelled while it waited
        }
        let mut cancel = Cancel { rx: generation.clone(), at: asked_at };
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
    }
}

fn listing(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir).map(|e| e.flatten().map(|e| e.path()).collect()).unwrap_or_default()
}

/// The CLI's `slide_watcher`: images dropped into `slides/`, and screenshots taken after the start,
/// once their size has stopped changing.
async fn slide_watcher(lec: Arc<Lecture>, store: Store, watch: SlideWatch, mut stop: tokio::sync::watch::Receiver<bool>, events: UnboundedSender<Event>) {
    let started = SystemTime::now();
    let mut known: HashSet<PathBuf> = listing(&lec.files.slides).into_iter().collect();
    let mut sizes: HashMap<PathBuf, u64> = HashMap::new();
    loop {
        let mut candidates = listing(&lec.files.slides);
        if let Some(dir) = &watch.screenshots {
            candidates.extend(listing(dir).into_iter().filter(|p| {
                p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("Screen")) && std::fs::metadata(p).and_then(|m| m.modified()).is_ok_and(|t| t >= started)
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
pub async fn run(lec: Arc<Lecture>, cfg: SessionConfig, source: Box<dyn Source>, watch: SlideWatch, mut commands: UnboundedReceiver<Command>, events: UnboundedSender<Event>) -> Result<StopReport> {
    let (handle, mut notes, store) = coordinator::spawn_with_store(cfg, source);
    let pages = Arc::new(Mutex::new(Vec::new()));
    let (op_tx, op_rx) = mpsc::unbounded_channel();
    let hurry = Arc::new(AtomicBool::new(false));
    let (generation, generation_rx) = watch::channel(0u64);
    let worker = tokio::spawn(notes_worker(lec.clone(), store.clone(), op_rx, events.clone(), pages.clone(), hurry.clone(), generation_rx));
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let watcher = tokio::spawn(slide_watcher(lec.clone(), store.clone(), watch, stop_rx, events.clone()));
    // Serves state reads until the lecture begins stopping: the session ends only once every store is dropped.
    let mut state_store = Some(store);
    let mut op_tx = Some(op_tx);
    let (mut stops, mut commands_open) = (0, true);
    let stopping = AtomicBool::new(false);
    let begin_stop = |op_tx: &mut Option<UnboundedSender<(Op, u64)>>, state_store: &mut Option<Store>| {
        *state_store = None;
        if !stopping.swap(true, Ordering::Relaxed) {
            let _ = stop_tx.send(true);
            *op_tx = None; // queued operations still run; nothing new is taken
        }
    };
    loop {
        tokio::select! {
            n = notes.recv() => match n {
                Some(Notification::SourceEnded) => {
                    begin_stop(&mut op_tx, &mut state_store);
                    let _ = events.send(Event::Session(Notification::SourceEnded));
                }
                Some(n) => { let _ = events.send(Event::Session(n)); }
                None => break,
            },
            c = commands.recv(), if commands_open => match c {
                Some(Command::Op(op)) => if let Some(tx) = &op_tx { let _ = tx.send((op, *generation.borrow())); },
                Some(Command::Cancel) => generation.send_modify(|g| *g += 1),
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
                    begin_stop(&mut op_tx, &mut state_store);
                    if stops >= 2 {
                        hurry.store(true, Ordering::Relaxed);
                    }
                }
                None => commands_open = false, // input closed: the lecture runs until it is stopped or its audio ends
            },
        }
    }
    begin_stop(&mut op_tx, &mut state_store);
    let _ = watcher.await;
    let _ = worker.await;
    let report = handle.finish().await?;
    for p in pages.lock().expect("the page list").drain(..) {
        if !p.is_finished() {
            p.abort();
            let _ = events.send(Event::Warning("the page was still being typeset; `lecture page` finishes it later".into()));
        }
    }
    // The last snapshot, after recovery has drained (spec §5.4), on the sidecar the session left.
    let sc: Sidecar = Sidecar::load(&lec.files.sidecar())?.context("the session left no sidecar")?;
    if let Err(e) = lec.snapshot(&Store::offline(sc, lec.files.sidecar()), "", &events).await {
        let _ = events.send(Event::SnapshotFailed(format!("{e}; everything is kept for the next one")));
    }
    Ok(report)
}

/// The sidecar as its file holds it: the one writer saves it atomically before every answer (spec §8).
fn read_sidecar(files: &LectureFiles) -> Result<Sidecar, String> {
    Sidecar::load(&files.sidecar()).map_err(|e| format!("{e:#}"))?.ok_or_else(|| "the lecture has no sidecar yet".to_string())
}

fn slide_event(s: &SlideEntry) -> Event {
    Event::Slide { index: s.index, file: s.file.clone(), auto: s.auto, uncertain: s.uncertain, shown_at: s.shown_at }
}
