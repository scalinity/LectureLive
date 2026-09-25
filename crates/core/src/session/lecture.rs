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
use tokio::task::JoinHandle;

use crate::audio::source::Source;
use crate::notes::chat::{self, ChatClient, ChatRequest, Content, Image};
use crate::notes::embeds::{clean_output, repair};
use crate::notes::page::{self, PageOutcome};
use crate::notes::timeline::{embed_line, timeline, Batch};
use crate::notes::{context, polish, prompts};
use crate::session::coordinator::{self, Notification, SessionConfig, StopReport, Store};
use crate::session::files::LectureFiles;
use crate::session::folder::{next_slide_index, relative};
use crate::session::notesfile::{self, sha256_hex};
use crate::session::segments;
use crate::session::sidecar::{Sidecar, SlideEntry};
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

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Op(Op),
    /// The first stops the lecture; a second stops waiting for recovery.
    Stop,
}

#[derive(Debug)]
pub enum Event {
    Session(Notification),
    Busy(String),
    Preview(String),
    NothingNew,
    Committed { words: usize, slides: usize, block: String, usd: f64, confirmed: bool, removed: usize, missing: usize },
    SnapshotFailed(String),
    Polished { backup: PathBuf, usd: f64 },
    PolishStopped(String),
    PolishFailed(String),
    Page { outcome: PageOutcome, usd: f64 },
    PageFailed(String),
    Slide { index: u32, file: String },
    Warning(String),
}

#[derive(Debug, Clone)]
pub struct SlideWatch {
    /// Where macOS saves screenshots; those taken after the lecture started are taken in as slides.
    pub screenshots: Option<PathBuf>,
    pub poll: Duration,
}

const NOTES_TIMEOUT: Duration = Duration::from_secs(600);
const IMAGE_EXTENSIONS: [&str; 3] = ["png", "jpg", "jpeg"];
const SLIDE_MAX_PX: u32 = 1600;

impl Lecture {
    /// Spec §6.1–6.2: cutoff, batch after the cursors, one streamed request, repair, journaled commit.
    pub async fn snapshot(&self, store: &Store, hint: &str, events: &UnboundedSender<Event>) -> Result<(), String> {
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
        let answer = self.chat.complete(&req, &mut |d: &str| drop(preview.send(Event::Preview(d.to_string())))).await.map_err(|e| e.to_string())?;
        if let Some(w) = answer.warning {
            let _ = events.send(Event::Warning(w));
        }
        let repaired = repair(&clean_output(&answer.text, true), &embeds);
        let block = notesfile::block(at, &repaired.text);
        let (files, b, to) = (self.files.clone(), block.clone(), batch.positions.end);
        store.update(move |sc| notesfile::commit(&files, sc, &b, to, slide_to)).await.map_err(text)?;
        let _ = events.send(Event::Committed { words: batch.words(), slides: batch.slides.len(), block, usd: answer.usd.unwrap_or(0.0), confirmed: cut.is_none_or(|c| c.confirmed), removed: repaired.removed, missing: repaired.missing.len() });
        Ok(())
    }

    /// Spec §6.3: a snapshot first, and nothing if it fails; true when the notes were polished.
    pub async fn polish(&self, store: &Store, events: &UnboundedSender<Event>) -> bool {
        if let Err(e) = self.snapshot(store, "", events).await {
            let _ = events.send(Event::PolishStopped(format!("the snapshot before it failed ({e}); the notes are unchanged")));
            return false;
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
        let answer = match self.chat.complete(&polish::request(&self.course, &self.title, &doc, &transcript), &mut |_| {}).await {
            Ok(a) => a,
            Err(e) => {
                let _ = events.send(Event::PolishFailed(format!("{e}; the notes are unchanged")));
                return false;
            }
        };
        let text = polish::validate(&answer.text, &doc).text;
        let (files, based_on) = (self.files.clone(), sha256_hex(doc.as_bytes()));
        match store.update(move |sc| notesfile::replace(&files, sc, &text, &based_on, Local::now())).await {
            Ok(backup) => {
                let _ = events.send(Event::Polished { backup, usd: answer.usd.unwrap_or(0.0) });
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

async fn notes_worker(lec: Arc<Lecture>, store: Store, mut ops: UnboundedReceiver<Op>, events: UnboundedSender<Event>, pages: Arc<Mutex<Vec<JoinHandle<()>>>>, hurry: Arc<AtomicBool>) {
    while let Some(op) = ops.recv().await {
        if hurry.load(Ordering::Relaxed) {
            continue; // a second stop: what was still queued is dropped; the last snapshot takes its material
        }
        match op {
            Op::Snapshot(hint) => {
                if let Err(e) = lec.snapshot(&store, &hint, &events).await {
                    let _ = events.send(Event::SnapshotFailed(format!("{e}; everything is kept for the next one")));
                }
            }
            Op::Polish => {
                if lec.polish(&store, &events).await {
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

fn is_image(p: &Path) -> bool {
    p.extension().and_then(|e| e.to_str()).is_some_and(|e| IMAGE_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

fn listing(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir).map(|e| e.flatten().map(|e| e.path()).collect()).unwrap_or_default()
}

/// Moves a file, copying when it crosses file systems.
fn move_file(from: &Path, to: &Path) -> Result<()> {
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    std::fs::copy(from, to).with_context(|| format!("copy {} to {}", from.display(), to.display()))?;
    std::fs::remove_file(from).with_context(|| format!("remove {}", from.display()))
}

/// The CLI's `shrink_if_large`: `sips -Z` scales up as well as down, so it runs only when there is something to shrink.
fn shrink_if_large(path: &Path) {
    let Ok(out) = std::process::Command::new("sips").args(["-g", "pixelWidth", "-g", "pixelHeight"]).arg(path).output() else { return };
    let dims: Vec<u32> = String::from_utf8_lossy(&out.stdout).lines().filter(|l| l.contains("pixel")).filter_map(|l| l.split(':').nth(1)?.trim().parse().ok()).collect();
    if dims.iter().any(|&d| d > SLIDE_MAX_PX) {
        let _ = std::process::Command::new("sips").args(["-Z", &SLIDE_MAX_PX.to_string()]).arg(path).output();
    }
}

/// Registers an image as the next slide (spec §7.3, §8): renamed into `slides/` as the CLI names it,
/// timed by its file time, shrunk to 1600 px, inside the sidecar's writer so indexes never collide.
async fn register(files: &LectureFiles, store: &Store, p: &Path) -> Result<SlideEntry> {
    let shown_at: DateTime<Local> = std::fs::metadata(p)?.modified()?.into();
    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("png").to_ascii_lowercase();
    let (files, p) = (files.clone(), p.to_path_buf());
    store
        .update(move |sc| {
            let index = next_slide_index(&files, sc)?;
            let dest = files.slides.join(format!("slide_{index:02}_{}.{ext}", shown_at.format("%H%M%S")));
            if p != dest {
                move_file(&p, &dest)?;
            }
            shrink_if_large(&dest);
            let entry = SlideEntry { index, file: relative(&files, &dest), shown_at };
            sc.slides.push(entry.clone());
            Ok(entry)
        })
        .await
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
            if known.contains(&p) || !is_image(&p) {
                continue;
            }
            let Ok(size) = std::fs::metadata(&p).map(|m| m.len()) else { continue };
            if sizes.insert(p.clone(), size) != Some(size) {
                continue; // still being written
            }
            known.insert(p.clone());
            match register(&lec.files, &store, &p).await {
                Ok(s) => {
                    known.insert(lec.files.dir.join(&s.file));
                    let _ = events.send(Event::Slide { index: s.index, file: s.file });
                }
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
    let worker = tokio::spawn(notes_worker(lec.clone(), store.clone(), op_rx, events.clone(), pages.clone(), hurry.clone()));
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let watcher = tokio::spawn(slide_watcher(lec.clone(), store.clone(), watch, stop_rx, events.clone()));
    drop(store);
    let mut op_tx = Some(op_tx);
    let (mut stops, mut commands_open) = (0, true);
    let stopping = AtomicBool::new(false);
    let begin_stop = |op_tx: &mut Option<UnboundedSender<Op>>| {
        if !stopping.swap(true, Ordering::Relaxed) {
            let _ = stop_tx.send(true);
            *op_tx = None; // queued operations still run; nothing new is taken
        }
    };
    loop {
        tokio::select! {
            n = notes.recv() => match n {
                Some(Notification::SourceEnded) => {
                    begin_stop(&mut op_tx);
                    let _ = events.send(Event::Session(Notification::SourceEnded));
                }
                Some(n) => { let _ = events.send(Event::Session(n)); }
                None => break,
            },
            c = commands.recv(), if commands_open => match c {
                Some(Command::Op(op)) => if let Some(tx) = &op_tx { let _ = tx.send(op); },
                Some(Command::Stop) => {
                    stops += 1;
                    handle.request_stop();
                    begin_stop(&mut op_tx);
                    if stops >= 2 {
                        hurry.store(true, Ordering::Relaxed);
                    }
                }
                None => commands_open = false, // input closed: the lecture runs until it is stopped or its audio ends
            },
        }
    }
    begin_stop(&mut op_tx);
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
