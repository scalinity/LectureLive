//! The app's one lecture (spec §3.2, §3.6): a folder opened, at most one `lecture::run` on Tauri's
//! runtime, and the commands the frontend calls. Every event goes through the `Pump`.
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use chrono::Local;
use lecturelive_core::audio::permission::{self, MicPermission};
use lecturelive_core::audio::source::DeviceSource;
use lecturelive_core::audio::{input, loopback};
use lecturelive_core::capture::detect::{Region, Thresholds};
use lecturelive_core::capture::select::{Descriptor, Selection, Selections};
use lecturelive_core::capture::window::{self, SystemWindows, WindowInfo, WindowSource};
use lecturelive_core::notes::chat::{ChatClient, ChatConfig};
use lecturelive_core::notes::{page, prompts};
use lecturelive_core::session::coordinator::SessionConfig;
use lecturelive_core::session::files::{course_from_path, LectureFiles};
use lecturelive_core::session::folder::How;
use lecturelive_core::session::launch::{self, Retention};
use lecturelive_core::session::lecture::{self, CaptureSetup, Command, Event, Lecture, Op, SlideWatch};
use lecturelive_core::session::lock::FolderLock;
use lecturelive_core::session::notesfile::Recovered;
use lecturelive_core::session::segments;
use lecturelive_core::session::sidecar::Sidecar;
use lecturelive_core::session::spend::{self, Spend};
use lecturelive_core::session::start;
use lecturelive_core::stt::{rest, stream};
use serde::Serialize;
use serde_json::Value;
use tauri::ipc::Channel;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_global_shortcut::GlobalShortcutExt;
use tokio::sync::{mpsc, oneshot, Mutex};

use crate::adapter::{read_document, Phase, Pump, Sink, Stream};
use crate::keychain;
use crate::wire::{CaptureView, CaptureWord, FolderView, NoticeKind, PreviewShot, SessionState, WindowView};

/// The Python CLI's ledger, taken over by the app's at each start (spec §8).
const CLI_LEDGER: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../spend.jsonl");

type Res<T> = Result<T, String>;

fn text(e: impl std::fmt::Display) -> String {
    e.to_string()
}

fn chain(e: anyhow::Error) -> String {
    format!("{e:#}")
}

fn data_dir() -> Res<PathBuf> {
    let dir = dirs::data_dir().ok_or("no Application Support folder")?.join("LectureLive");
    std::fs::create_dir_all(&dir).map_err(text)?;
    Ok(dir)
}

/// Status over a Tauri event; the two ordered streams over the channels the page attached.
pub struct TauriSink {
    app: AppHandle,
    channels: StdMutex<Option<(Channel<Value>, Channel<Value>)>>,
}

impl Sink for TauriSink {
    fn send(&self, stream: Stream, msg: Value) {
        match stream {
            Stream::Status => {
                let _ = self.app.emit("status", msg);
            }
            Stream::Transcript | Stream::Notes => {
                // Without a page attached the message is dropped: the page's next hydration holds it.
                if let Some((t, n)) = &*self.channels.lock().expect("the channel lock") {
                    let _ = if stream == Stream::Transcript { t.send(msg) } else { n.send(msg) };
                }
            }
        }
    }
}

#[derive(Clone)]
struct OpenFolder {
    files: LectureFiles,
    course: String,
    name: String,
    title: String,
}

struct Running {
    commands: mpsc::UnboundedSender<Command>,
    lecture: Arc<Lecture>,
    stops: u32,
    _lock: FolderLock,
}

pub struct App {
    pump: Arc<Mutex<Pump>>,
    sink: Arc<TauriSink>,
    folder: StdMutex<Option<OpenFolder>>,
    running: StdMutex<Option<Running>>,
}

impl App {
    pub fn new(app: AppHandle) -> Self {
        let sink = Arc::new(TauriSink { app, channels: StdMutex::new(None) });
        Self { pump: Arc::new(Mutex::new(Pump::new(String::new(), sink.clone(), None, false))), sink, folder: StdMutex::new(None), running: StdMutex::new(None) }
    }

    fn folder(&self) -> Option<OpenFolder> {
        self.folder.lock().expect("the folder lock").clone()
    }

    fn commands(&self) -> Option<mpsc::UnboundedSender<Command>> {
        self.running.lock().expect("the running lock").as_ref().map(|r| r.commands.clone())
    }

    fn send(&self, c: Command) -> Res<()> {
        // Core takes no new operation once the lecture is stopping; say so rather than drop it.
        if matches!(c, Command::Op(_)) && self.running.lock().expect("the running lock").as_ref().is_some_and(|r| r.stops > 0) {
            return Err("The lecture is stopping; the last snapshot takes what is left.".into());
        }
        self.commands().ok_or("No lecture is running.")?.send(c).map_err(|_| "The lecture has ended.".to_string())
    }
}

fn folder_view(f: &OpenFolder) -> FolderView {
    let page = page::page_path(&f.files.notes, &f.name);
    FolderView {
        dir: f.files.dir.to_string_lossy().into_owned(),
        course: f.course.clone(),
        name: f.name.clone(),
        notes_dir: f.files.notes_dir().to_string_lossy().into_owned(),
        page: page.exists().then(|| page.to_string_lossy().into_owned()),
    }
}

#[tauri::command]
pub fn attach(transcript: Channel<Value>, notes: Channel<Value>, app: State<'_, App>) {
    *app.sink.channels.lock().expect("the channel lock") = Some((transcript, notes));
}

/// How long a running lecture has to answer a state read before the file answers instead.
const STATE_WAIT: Duration = Duration::from_secs(2);

/// The sidecar: from the running lecture's one writer, else its file. Once the lecture is stopping it
/// reads no commands (the last snapshot runs), so the file answers at once (spec §3.6).
async fn sidecar(commands: Option<&mpsc::UnboundedSender<Command>>, files: &LectureFiles, stopping: bool) -> Option<Sidecar> {
    if let Some(c) = commands.filter(|_| !stopping) {
        let (tx, rx) = oneshot::channel();
        if c.send(Command::State(tx)).is_ok() {
            if let Ok(Ok(Ok(sc))) = tokio::time::timeout(STATE_WAIT, rx).await {
                return Some(sc);
            }
        }
    }
    Sidecar::load(&files.sidecar()).ok().flatten()
}

/// Spec §3.6: a coherent snapshot. The pump is held throughout, so its sequence is the watermark of
/// everything read; the document is read until its hash is the sidecar's revision.
#[tauri::command]
pub async fn get_session_state(app: State<'_, App>) -> Res<SessionState> {
    let pump = app.pump.lock().await;
    let Some(folder) = app.folder() else { return Ok(pump.state(None, &[], String::new(), None)) };
    let files = &folder.files;
    let commands = app.commands();
    let stopping = matches!(pump.phase(), Phase::Stopping | Phase::StoppingNow | Phase::Ended);
    let mut tries = 0;
    let (sc, document) = loop {
        let sc = sidecar(commands.as_ref(), files, stopping).await;
        let doc = match &sc {
            Some(sc) => read_document(files, &sc.notes),
            None => Some(std::fs::read_to_string(&files.notes).unwrap_or_default()),
        };
        match doc {
            Some(d) => break (sc, d),
            None if tries < 2 => tries += 1,
            // An edit made outside the app since its last session: shown as it is, accepted at the next start.
            None => break (sc, std::fs::read_to_string(&files.notes).unwrap_or_default()),
        }
    };
    let segments = segments::read(&files.segments()).unwrap_or_default();
    Ok(pump.state(sc.as_ref(), &segments, document, Some(files)))
}

#[tauri::command]
pub async fn select_folder(dir: String, app: State<'_, App>, handle: AppHandle) -> Res<FolderView> {
    if app.commands().is_some() {
        return Err("A lecture is running; stop it before opening another folder.".into());
    }
    let dir = PathBuf::from(dir).canonicalize().map_err(text)?;
    let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let course = course_from_path(&dir).or_else(|| std::env::var("LECTURE_COURSE").ok().filter(|c| !c.is_empty())).unwrap_or_else(|| "Lecture".into());
    let today = Local::now().date_naive();
    let files = LectureFiles::standard(&dir, today);
    // Images resolve only inside this lecture's slides/ (spec §9.3).
    handle.asset_protocol_scope().allow_directory(&files.slides, false).map_err(text)?;
    let folder = OpenFolder { title: prompts::title(&course, &name, today), files, course, name };
    let view = folder_view(&folder);
    let capture = ready_view(&selections_path()?, &folder.course);
    *app.folder.lock().expect("the folder lock") = Some(folder);
    let mut pump = app.pump.lock().await;
    *pump = Pump::new(uuid::Uuid::new_v4().to_string(), app.sink.clone(), None, false);
    let v = view.clone();
    pump.set_status(move |s| {
        s.folder = Some(v);
        s.capture = capture;
    });
    Ok(view)
}

#[derive(Serialize)]
pub struct InputView {
    name: String,
    uid: String,
}

#[tauri::command]
pub async fn inputs() -> Res<Vec<InputView>> {
    tauri::async_runtime::spawn_blocking(|| input::list_inputs().map(|v| v.into_iter().map(|i| InputView { name: i.name, uid: i.uid }).collect()).map_err(chain)).await.map_err(text)?
}

#[derive(Serialize)]
pub struct LoopbackView {
    present: bool,
    blackhole_present: bool,
}

#[tauri::command]
pub async fn loopback_status() -> Res<LoopbackView> {
    let s = tauri::async_runtime::spawn_blocking(loopback::status).await.map_err(text)?.map_err(chain)?;
    Ok(LoopbackView { present: s.present, blackhole_present: s.blackhole_present })
}

/// Everything before the session, as the CLI's `lecture` command does it, then `lecture::run` on
/// Tauri's runtime (spec §3.2); returns the new session's id.
#[tauri::command]
pub async fn start_lecture(source: String, app: State<'_, App>, handle: AppHandle) -> Res<String> {
    if app.commands().is_some() {
        return Err("A lecture is already running.".into());
    }
    let folder = app.folder().ok_or("Choose a lecture folder first.")?;
    let key = keychain::get(keychain::SERVICE).map_err(chain)?.ok_or("No API key yet: add it with the key button in the status bar.")?;
    if matches!(permission::microphone(), MicPermission::Denied | MicPermission::Restricted) {
        return Err("Microphone access is denied: System Settings → Privacy & Security → Microphone.".into());
    }
    let (uid, source_name) = if source == "loopback" {
        let s = tauri::async_runtime::spawn_blocking(loopback::status).await.map_err(text)?.map_err(chain)?;
        if !s.blackhole_present {
            return Err("BlackHole 2ch is not installed (brew install blackhole-2ch).".into());
        }
        (loopback::BLACKHOLE_UID.to_string(), "BlackHole 2ch".to_string())
    } else {
        let inputs = tauri::async_runtime::spawn_blocking(input::list_inputs).await.map_err(text)?.map_err(chain)?;
        let i = inputs.into_iter().find(|i| i.uid == source).ok_or("That input is no longer connected.")?;
        (i.uid, i.name)
    };
    let data = data_dir()?;
    let app_ledger = data.join("spend.jsonl");
    spend::take_over(&app_ledger, Path::new(CLI_LEDGER)).map_err(chain)?;
    let files = folder.files.clone();
    let lock = FolderLock::acquire(&files.dir).map_err(chain)?;
    let spend = Spend::open(&app_ledger, &folder.course, &folder.name, files.date).map_err(chain)?;
    let chat = ChatClient::new(ChatConfig::new(key.clone()), Some(spend.clone())).map_err(chain)?;
    let lec = Arc::new(Lecture { files: files.clone(), course: folder.course.clone(), name: folder.name.clone(), title: folder.title.clone(), chat, spend: spend.clone() });
    let session = uuid::Uuid::new_v4().to_string();
    {
        let mut pump = app.pump.lock().await;
        *pump = Pump::new(session.clone(), app.sink.clone(), Some(spend.clone()), uid == loopback::BLACKHOLE_UID);
        let (view, name) = (folder_view(&folder), source_name.clone());
        let capture_view = ready_view(&selections_path()?, &folder.course);
        pump.set_status(move |s| {
            s.phase = Phase::Starting;
            s.folder = Some(view);
            s.source = Some(name);
            s.stt = "connecting".into();
            s.started_at = Some(Local::now().to_rfc3339());
            s.capture = capture_view;
        });
        if let Ok(Some(restored)) = launch::restore_abandoned_route(&data.join("route.json")) {
            pump.notice(NoticeKind::Done, "Route restored", &format!("undid a canary route left behind; default output restored: {restored}"));
        }
    }
    // Other days' recovery can take minutes: each day is announced as it starts.
    let (day_tx, mut day_rx) = mpsc::unbounded_channel::<String>();
    let announcer = {
        let pump = app.pump.clone();
        tauri::async_runtime::spawn(async move {
            while let Some(stem) = day_rx.recv().await {
                pump.lock().await.notice(NoticeKind::Notes, "Recovering", &format!("{stem}'s transcript gaps, before today's session"));
            }
        })
    };
    let recovery = || -> anyhow::Result<rest::RecoveryLink> { Ok(rest::spawn_recovery(rest::RestClient::new(rest::RestConfig::new(key.clone(), vec![]))?)) };
    let mut announce = move |stem: &str| drop(day_tx.send(stem.to_string()));
    let ready = start::prepare(&files, &folder.title, false, Retention::KeepAll, &recovery, Some(spend.clone()), &mut announce).await;
    drop(announce);
    let _ = announcer.await;
    let ready = match ready {
        Ok(r) => r,
        Err(e) => {
            app.pump.lock().await.set_status(|s| s.phase = Phase::Idle);
            return Err(chain(e));
        }
    };
    let stt = stream::spawn(stream::SttConfig::new(key.clone(), vec![])).map_err(chain)?;
    let cfg = SessionConfig { dir: files.dir.clone(), stem: files.stem.clone(), stt: Some(stt), recovery: Some(recovery().map_err(chain)?), spend: Some(spend.clone()), transcript: Some(files.transcript.clone()) };
    {
        let mut pump = app.pump.lock().await;
        for (path, n) in &ready.launch.repaired {
            pump.notice(NoticeKind::Done, "Repaired", &format!("{} ({:.1} s)", path.display(), *n as f64 / 16_000.0));
        }
        for path in &ready.launch.missing {
            pump.notice(NoticeKind::Warn, "Missing", &format!("{} (marked as a gap)", path.display()));
        }
        match ready.init.how {
            How::Created => pump.notice(NoticeKind::Notes, "Notes", "created"),
            How::Migrated => pump.notice(NoticeKind::Notes, "Migrated", "this folder's Python CLI state is now the app's (the Python CLI no longer writes here)"),
            How::Rebuilt => pump.notice(NoticeKind::Warn, "Rebuilt", "the state was rebuilt from the notes: everything after the last <!-- --> marker is pending"),
            How::Resumed => {}
        }
        if let Some(m) = ready.init.legacy_commit {
            pump.notice(NoticeKind::Warn, "Recovered", m);
        }
        match ready.init.journal {
            Recovered::Completed => pump.notice(NoticeKind::Done, "Recovered", "the last snapshot was fully written"),
            Recovered::Truncated => pump.notice(NoticeKind::Warn, "Recovered", "removed a half-written snapshot; its material is queued again"),
            Recovered::NotAppended => pump.notice(NoticeKind::Warn, "Recovered", "an interrupted snapshot never reached the notes; its material is queued again"),
            Recovered::Nothing => {}
        }
        if ready.init.external_edit {
            pump.notice(NoticeKind::Notes, "Notes", "edited outside the app since the last session; kept as they are");
        }
        for (stem, r) in &ready.other_days {
            let waiting = if r.unresolved > 0 { format!(", {} still waiting", r.unresolved) } else { String::new() };
            pump.notice(NoticeKind::Done, "Recovered", &format!("{stem}'s transcript gaps{waiting}"));
        }
        if ready.init.pending_segments > 0 || ready.init.pending_slides > 0 {
            pump.notice(NoticeKind::Notes, "Resumed", &format!("{} lines and {} slides wait for the next snapshot", ready.init.pending_segments, ready.init.pending_slides));
        }
    }
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (ev_tx, mut ev_rx) = mpsc::unbounded_channel::<Event>();
    let pump = app.pump.clone();
    let forward = tauri::async_runtime::spawn(async move {
        while let Some(e) = ev_rx.recv().await {
            pump.lock().await.apply(e);
        }
    });
    let watch = SlideWatch { screenshots: lecture::screenshot_dir(), poll: Duration::from_secs(1) };
    // Spec §7: the course's saved window, revalidated by the worker as the lecture starts.
    let selection = Selections::load(&selections_path()?).ok().and_then(|s| s.get(&folder.course).cloned());
    let capture = CaptureSetup { source: Box::new(SystemWindows), selection, interval: Duration::from_secs(1), thresholds: Thresholds::default(), record: std::env::var_os("LECTURELIVE_RECORD").map(PathBuf::from) };
    let run = lecture::run(lec.clone(), cfg, Box::new(DeviceSource { uid }), watch, Some(capture), cmd_rx, ev_tx);
    *app.running.lock().expect("the running lock") = Some(Running { commands: cmd_tx, lecture: lec, stops: 0, _lock: lock });
    app.pump.lock().await.set_status(|s| s.phase = Phase::Running);
    // ⌘⇧2 captures the slide from anywhere, only while a lecture runs: it takes the keys from every other app.
    if let Err(e) = handle.global_shortcut().register(SHORTCUT) {
        app.pump.lock().await.notice(NoticeKind::Warn, "Shortcut", &format!("⌘⇧2 could not be registered ({e}); use the Capture button"));
    }
    let h = handle.clone();
    tauri::async_runtime::spawn(async move {
        let result = run.await;
        let _ = forward.await;
        let app = h.state::<App>();
        let _ = h.global_shortcut().unregister(SHORTCUT);
        let running = app.running.lock().expect("the running lock").take(); // the folder lock goes with it
        drop(running);
        let mut pump = app.pump.lock().await;
        pump.set_status(|s| {
            s.phase = Phase::Ended;
            s.busy = None;
            s.level_dbfs = None;
        });
        match result {
            Ok(report) => {
                let waiting = if report.unresolved > 0 { format!("; {} transcript gaps still to recover, the next session in this folder does it", report.unresolved) } else { String::new() };
                pump.notice(NoticeKind::Done, "Saved", &format!("the notes and transcript{waiting}"));
            }
            Err(e) => pump.notice(NoticeKind::Warn, "Session failed", &format!("{e:#}")),
        }
    });
    Ok(session)
}

/// The first stop finishes the transcript and recovery, then takes the last snapshot; the second stops
/// waiting (spec §9.1). Returns which stop this was.
#[tauri::command]
pub async fn stop(app: State<'_, App>) -> Res<u32> {
    let level = {
        let mut running = app.running.lock().expect("the running lock");
        let r = running.as_mut().ok_or("No lecture is running.")?;
        r.stops += 1;
        let _ = r.commands.send(Command::Stop);
        r.stops
    };
    app.pump.lock().await.set_status(|s| s.phase = if level == 1 { Phase::Stopping } else { Phase::StoppingNow });
    Ok(level)
}

#[tauri::command]
pub fn snapshot(hint: String, app: State<'_, App>) -> Res<()> {
    app.send(Command::Op(Op::Snapshot(hint)))
}

#[tauri::command]
pub fn polish(app: State<'_, App>) -> Res<()> {
    app.send(Command::Op(Op::Polish))
}

#[tauri::command]
pub fn cancel(app: State<'_, App>) -> Res<()> {
    app.send(Command::Cancel)
}

/// The global shortcut for Capture (spec §7.3): away from Zoom's and macOS's own combinations.
pub const SHORTCUT: &str = "CommandOrControl+Shift+Digit2";

/// Saved windows and regions, by course (spec §7.1).
fn selections_path() -> Res<PathBuf> {
    Ok(data_dir()?.join("capture.json"))
}

/// Keeps the window and region the person chose for a course.
fn save_selection(path: &Path, course: &str, w: &WindowInfo, region: Region) -> anyhow::Result<Selection> {
    anyhow::ensure!(region.is_valid(), "The region must lie inside the window.");
    let sel = Selection { descriptor: Descriptor::of(w), region };
    let mut all = Selections::load(path)?;
    all.set(course, sel.clone());
    all.save(path)?;
    Ok(sel)
}

/// The person's "Watch it" on an ask: this window, through the region already saved for the course.
fn watch_saved(path: &Path, course: &str, w: &WindowInfo) -> anyhow::Result<Selection> {
    let region = Selections::load(path)?.get(course).map(|s| s.region).ok_or_else(|| anyhow::anyhow!("No window is chosen for {course} yet: choose one."))?;
    save_selection(path, course, w, region)
}

/// The region saved for a course, so the picker can draw it on a new window's still.
fn saved_region(path: &Path, course: &str) -> Option<Region> {
    Selections::load(path).ok()?.get(course).map(|s| s.region)
}

#[tauri::command]
pub fn capture_saved_region(app: State<'_, App>) -> Res<Option<Region>> {
    let folder = app.folder().ok_or("Choose a lecture folder first.")?;
    Ok(saved_region(&selections_path()?, &folder.course))
}

/// Before a lecture: the course's saved window, or none yet.
fn ready_view(path: &Path, course: &str) -> CaptureView {
    match Selections::load(path).ok().and_then(|s| s.get(course).cloned()) {
        Some(sel) => CaptureView { state: CaptureWord::Ready, window: Some(sel.descriptor.label()), ..CaptureView::default() },
        None => CaptureView::default(),
    }
}

/// The windows the picker offers; missing Screen Recording is an error, never an empty list (spec §7.1).
#[tauri::command]
pub async fn capture_windows() -> Res<Vec<WindowView>> {
    tauri::async_runtime::spawn_blocking(|| SystemWindows.windows().map(|ws| ws.iter().map(WindowView::from).collect()).map_err(|e| e.to_string())).await.map_err(text)?
}

/// A still of one window for the picker, fitted to 1600 px, readable only through the asset protocol.
#[tauri::command]
pub async fn capture_preview(id: u32, handle: AppHandle) -> Res<PreviewShot> {
    let dir = data_dir()?.join("picker");
    let (path, (width, height)) = tauri::async_runtime::spawn_blocking(move || -> Res<(PathBuf, (u32, u32))> {
        std::fs::create_dir_all(&dir).map_err(text)?;
        for e in std::fs::read_dir(&dir).map_err(text)?.flatten() {
            let _ = std::fs::remove_file(e.path()); // only the latest still is kept
        }
        let path = dir.join(format!("window-{id}-{}.png", Local::now().timestamp_millis()));
        let size = window::capture_window(id, &path).map_err(chain)?;
        Ok((path, size))
    })
    .await
    .map_err(text)??;
    handle.asset_protocol_scope().allow_file(&path).map_err(text)?;
    Ok(PreviewShot { path: path.to_string_lossy().into_owned(), width, height })
}

/// The person chose a window and region: saved for the course, and watched at once if a lecture runs.
#[tauri::command]
pub async fn capture_select(id: u32, region: Region, app: State<'_, App>) -> Res<()> {
    let folder = app.folder().ok_or("Choose a lecture folder first.")?;
    let windows = tauri::async_runtime::spawn_blocking(|| SystemWindows.windows()).await.map_err(text)?.map_err(|e| e.to_string())?;
    let w = windows.into_iter().find(|w| w.id == id).ok_or("That window is no longer open.")?;
    let path = selections_path()?;
    let selection = save_selection(&path, &folder.course, &w, region).map_err(chain)?;
    match app.commands() {
        Some(c) => c.send(Command::Bind { window: id, selection }).map_err(|_| "The lecture has ended.".to_string()),
        None => {
            let view = ready_view(&path, &folder.course);
            app.pump.lock().await.set_status(move |s| s.capture = view);
            Ok(())
        }
    }
}

/// "Watch it": the window the strip asks about, through the course's saved region.
#[tauri::command]
pub async fn capture_watch(id: u32, app: State<'_, App>) -> Res<()> {
    let folder = app.folder().ok_or("Choose a lecture folder first.")?;
    let windows = tauri::async_runtime::spawn_blocking(|| SystemWindows.windows()).await.map_err(text)?.map_err(|e| e.to_string())?;
    let w = windows.into_iter().find(|w| w.id == id).ok_or("That window is no longer open.")?;
    let selection = watch_saved(&selections_path()?, &folder.course, &w).map_err(chain)?;
    app.commands().ok_or("No lecture is running.")?.send(Command::Bind { window: id, selection }).map_err(|_| "The lecture has ended.".to_string())
}

/// The Capture button: the watched region now, as a manual slide.
#[tauri::command]
pub async fn capture_now(app: State<'_, App>) -> Res<()> {
    let (tx, rx) = oneshot::channel();
    app.send(Command::CaptureNow(tx))?;
    rx.await.map_err(|_| "The lecture has ended.".to_string())?
}

/// ⌘⇧2: as the Capture button; a refusal becomes a notice, since no button was pressed to show it.
pub fn shortcut(handle: &AppHandle) {
    let handle = handle.clone();
    tauri::async_runtime::spawn(async move {
        let app = handle.state::<App>();
        let (tx, rx) = oneshot::channel();
        let result = match app.send(Command::CaptureNow(tx)) {
            Ok(()) => rx.await.unwrap_or_else(|_| Err("The lecture has ended.".into())),
            Err(e) => Err(e),
        };
        if let Err(m) = result {
            app.pump.lock().await.notice(NoticeKind::Warn, "Capture", &m);
        }
    });
}

/// Images dropped on the window (spec §7.3).
#[tauri::command]
pub fn import_slides(paths: Vec<String>, app: State<'_, App>) -> Res<()> {
    if app.commands().is_none() {
        return Err("Start the lecture to add slides.".into());
    }
    app.send(Command::Import(paths.into_iter().map(PathBuf::from).collect()))
}

/// The fix-it for a missing Screen Recording grant (spec §10).
#[tauri::command]
pub fn open_screen_settings() -> Res<()> {
    std::process::Command::new("open").arg("x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture").status().map_err(text)?;
    Ok(())
}

/// Spec §9.1: the study page, typeset first when it is missing or stale (from its cache otherwise),
/// then opened in the default browser.
#[tauri::command]
pub async fn open_page(app: State<'_, App>) -> Res<String> {
    let running = app.running.lock().expect("the running lock").as_ref().map(|r| r.lecture.clone());
    let (lec, _lock) = match running {
        Some(l) => (l, None),
        None => {
            let folder = app.folder().ok_or("Choose a lecture folder first.")?;
            if !folder.files.notes.exists() {
                return Err("There are no notes to typeset in this folder yet.".into());
            }
            let key = keychain::get(keychain::SERVICE).map_err(chain)?.ok_or("No API key yet: add it with the key button in the status bar.")?;
            let lock = FolderLock::acquire(&folder.files.dir).map_err(chain)?;
            let spend = Spend::open(&data_dir()?.join("spend.jsonl"), &folder.course, &folder.name, folder.files.date).map_err(chain)?;
            let chat = ChatClient::new(ChatConfig::new(key), Some(spend.clone())).map_err(chain)?;
            (Arc::new(Lecture { files: folder.files.clone(), course: folder.course, name: folder.name, title: folder.title, chat, spend }), Some(lock))
        }
    };
    let (tx, mut rx) = mpsc::unbounded_channel::<Event>();
    let pump = app.pump.clone();
    let forward = tauri::async_runtime::spawn(async move {
        while let Some(e) = rx.recv().await {
            pump.lock().await.apply(e);
        }
    });
    let outcome = lec.page(&tx).await;
    drop(tx);
    let _ = forward.await;
    let outcome = outcome?;
    // A check typesets without opening the default browser: the page itself loads math and fonts from the web.
    if std::env::var_os("LECTURELIVE_CHECK").is_none() {
        std::process::Command::new("open").arg(&outcome.path).status().map_err(text)?;
    }
    Ok(outcome.path.to_string_lossy().into_owned())
}

#[derive(Serialize)]
pub struct KeyStatus {
    stored: bool,
    /// The CLI's key is in the environment or the repository's `.env`, ready to move into the Keychain.
    env_available: bool,
}

/// Whether a key is stored; the key itself never reaches the frontend (spec §2).
#[tauri::command]
pub fn key_status() -> Res<KeyStatus> {
    Ok(KeyStatus { stored: keychain::get(keychain::SERVICE).map_err(chain)?.is_some(), env_available: keychain::env_key().is_some() })
}

#[tauri::command]
pub fn save_key(key: String) -> Res<()> {
    let key = key.trim();
    if key.is_empty() {
        return Err("Paste the API key first.".into());
    }
    keychain::set(keychain::SERVICE, key).map_err(chain)
}

#[tauri::command]
pub fn import_key_from_env() -> Res<()> {
    let key = keychain::env_key().ok_or("GROK_API_KEY is not in the environment or the repository's .env.")?;
    keychain::set(keychain::SERVICE, &key).map_err(chain)
}

/// The spend view's figures (spec §9.1): the CLI's new lines are taken over first, so both tools' spend shows.
fn spend_summary_at(app_ledger: &Path, cli_ledger: &Path) -> anyhow::Result<spend::Summary> {
    spend::take_over(app_ledger, cli_ledger)?;
    Ok(spend::summary(&spend::read(app_ledger)?))
}

#[tauri::command]
pub fn spend_summary() -> Res<spend::Summary> {
    spend_summary_at(&data_dir()?.join("spend.jsonl"), Path::new(CLI_LEDGER)).map_err(chain)
}

#[derive(Serialize)]
pub struct CheckConfig {
    mode: String,
    dir: Option<String>,
}

/// A measurement or check the page runs by itself, from `LECTURELIVE_CHECK` (and `LECTURELIVE_CHECK_DIR`,
/// the synthetic lecture folder for the live check); None in ordinary use.
#[tauri::command]
pub fn check_config() -> Option<CheckConfig> {
    let mode = std::env::var("LECTURELIVE_CHECK").ok().filter(|m| !m.is_empty())?;
    Some(CheckConfig { mode, dir: std::env::var("LECTURELIVE_CHECK_DIR").ok() })
}

/// Writes a check's report to `~/Library/Application Support/LectureLive/m4-checks/<name>.json`.
#[tauri::command]
pub fn check_report(name: String, json: String) -> Res<String> {
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err(format!("not a report name: {name:?}"));
    }
    let dir = data_dir()?.join("m4-checks");
    std::fs::create_dir_all(&dir).map_err(text)?;
    let path = dir.join(format!("{name}.json"));
    std::fs::write(&path, json).map_err(text)?;
    Ok(path.to_string_lossy().into_owned())
}

/// Hides the window for `ms`, then shows it again: the hidden-window rehydration check (spec §9.2).
#[tauri::command]
pub async fn hide_window_for(ms: u64, handle: AppHandle) -> Res<()> {
    let w = handle.get_webview_window("main").ok_or("no main window")?;
    w.hide().map_err(text)?;
    tokio::time::sleep(Duration::from_millis(ms)).await;
    w.show().map_err(text)?;
    w.set_focus().map_err(text)
}

#[tauri::command]
pub fn exit_app(app: AppHandle) {
    app.exit(0);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Final review, I1: once the lecture is stopping it no longer reads its commands (the last snapshot
    /// runs), so the state comes from the file at once; a lecture that does not answer falls back to it too.
    #[tokio::test]
    async fn a_stopping_or_silent_lecture_s_state_comes_from_the_file_promptly() {
        let dir = tempfile::tempdir().unwrap();
        let files = LectureFiles::standard(dir.path(), chrono::NaiveDate::from_ymd_opt(2026, 9, 25).unwrap());
        std::fs::create_dir_all(files.state_dir()).unwrap();
        let mut on_disk = Sidecar::default();
        on_disk.notes.revision = 7;
        on_disk.save(&files.sidecar()).unwrap();
        let (tx, _unread) = mpsc::unbounded_channel::<Command>();
        let t0 = std::time::Instant::now();
        let sc = tokio::time::timeout(Duration::from_secs(10), sidecar(Some(&tx), &files, true)).await.expect("no wait while stopping");
        assert_eq!(sc.map(|s| s.notes.revision), Some(7));
        assert!(t0.elapsed() < Duration::from_millis(500), "{:?}", t0.elapsed());
        let t0 = std::time::Instant::now();
        let sc = tokio::time::timeout(Duration::from_secs(10), sidecar(Some(&tx), &files, false)).await.expect("a silent lecture falls back to the file");
        assert_eq!(sc.map(|s| s.notes.revision), Some(7));
        assert!(t0.elapsed() < Duration::from_secs(4), "{:?}", t0.elapsed());
    }

    #[test]
    fn the_spend_view_takes_over_the_cli_ledger_first() {
        let dir = tempfile::tempdir().unwrap();
        let (app, cli) = (dir.path().join("app/spend.jsonl"), dir.path().join("spend.jsonl"));
        let at = chrono::NaiveDate::from_ymd_opt(2026, 9, 25).unwrap().and_hms_opt(10, 0, 0).unwrap();
        std::fs::write(&cli, spend::line(at, "ML", "Week 01", spend::SpendKind::Page, 0.25, true, None)).unwrap();
        let s = spend_summary_at(&app, &cli).unwrap();
        assert_eq!((s.calls, s.months.len()), (1, 1));
        assert_eq!(s.recent[0].lecture, "Week 01");
    }

    #[test]
    fn a_selection_is_saved_per_course_and_the_status_names_it() {
        use lecturelive_core::capture::detect::Region;
        use lecturelive_core::capture::window::WindowInfo;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("capture.json");
        let w = WindowInfo { id: 42, app: "zoom.us".into(), bundle_id: Some("us.zoom.xos".into()), title: "Zoom Meeting".into(), width: 1600, height: 900, on_screen: true };
        save_selection(&path, "Machine Learning", &w, Region { x: 0.1, y: 0.1, w: 0.8, h: 0.8 }).unwrap();
        assert_eq!(ready_view(&path, "Machine Learning").state, CaptureWord::Ready);
        assert_eq!(ready_view(&path, "Machine Learning").window.as_deref(), Some("Zoom Meeting"));
        assert_eq!(ready_view(&path, "Statistics").state, CaptureWord::Unbound);
        assert!(save_selection(&path, "Machine Learning", &w, Region { x: 0.5, y: 0.5, w: 0.9, h: 0.9 }).is_err(), "a region outside the window is refused");
    }

    #[test]
    fn watching_a_new_window_keeps_the_course_s_region() {
        use lecturelive_core::capture::detect::Region;
        use lecturelive_core::capture::window::WindowInfo;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("capture.json");
        let old = WindowInfo { id: 42, app: "zoom.us".into(), bundle_id: Some("us.zoom.xos".into()), title: "Zoom Meeting".into(), width: 1600, height: 900, on_screen: true };
        let region = Region { x: 0.1, y: 0.1, w: 0.8, h: 0.8 };
        save_selection(&path, "Machine Learning", &old, region).unwrap();
        let new = WindowInfo { id: 77, width: 1280, height: 800, ..old };
        let sel = watch_saved(&path, "Machine Learning", &new).unwrap();
        assert_eq!((sel.region, sel.descriptor.width, sel.descriptor.height), (region, 1280, 800));
        assert!(watch_saved(&path, "Statistics", &new).is_err(), "a course with no saved window must choose one");
    }

    #[test]
    fn the_picker_reads_the_course_s_saved_region() {
        use lecturelive_core::capture::detect::Region;
        use lecturelive_core::capture::window::WindowInfo;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("capture.json");
        assert_eq!(saved_region(&path, "Machine Learning"), None);
        let w = WindowInfo { id: 42, app: "zoom.us".into(), bundle_id: None, title: "Zoom Meeting".into(), width: 1600, height: 900, on_screen: true };
        let region = Region { x: 0.1, y: 0.2, w: 0.7, h: 0.6 };
        save_selection(&path, "Machine Learning", &w, region).unwrap();
        assert_eq!(saved_region(&path, "Machine Learning"), Some(region));
    }
}
