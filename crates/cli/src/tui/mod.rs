//! The terminal frontend (M7 plan §F, §G): the reactor on the main thread. It owns the terminal and
//! what it shows — the session view, reduced from every lecture event and reconciled with the
//! canonical files — reads the keyboard through Crossterm's `EventStream` alone, and gives Ctrl-C,
//! SIGINT, `--secs` and the audio's own end to the stop controller both frontends share.
//! Every way out goes through [`leave`], which gives the terminal back before anything is printed.

pub(crate) mod hydrate;
mod input;
mod markdown;
mod panes;
pub(crate) mod state;
pub(crate) mod terminal;
mod view;

use std::future::Future;
use std::io::{self, Write};
use std::time::Duration;

use anyhow::Result;
use chrono::Local;
use futures_util::StreamExt;
use lecturelive_core::session::coordinator::{Notification, StopReport};
use lecturelive_core::session::files::LectureFiles;
use lecturelive_core::session::lecture::{Command, Event, Op};
use lecturelive_core::session::spend::Spend;
use ratatui::crossterm::event::{Event as TermEvent, EventStream, KeyEvent, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use lecturelive_core::capture::select::Selection;
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::task::JoinError;
use tokio::time::{interval_at, sleep_until, Instant, MissedTickBehavior};

use crate::capture;
use crate::plain;
use crate::stop::{Origin, Stage, Step, StopController};
use state::{Identity, OwnOp, View};
use terminal::Screen;

/// At most 20 draws a second.
const FRAME: Duration = Duration::from_millis(50);
/// Events applied on one wake before the next draw.
const BATCH: usize = 256;

/// What the lecture hands the TUI once `prepare` has run (plan Task 6): the lecture's identity, its
/// canonical files for the re-reads a discontinuity asks for, the shared spend ledger, and the
/// start-up records the plain report printed, which seed the activity.
pub(crate) struct Session {
    pub(crate) identity: Identity,
    pub(crate) files: LectureFiles,
    pub(crate) spend: Spend,
    pub(crate) seed: Vec<plain::Notice>,
    /// The capture context (plan Task 11): where the course's selection is saved and the
    /// selection as it was loaded before the lecture started. None in a scripted session without
    /// capture. The reactor alone holds it; nothing rereads the file while the lecture runs.
    pub(crate) capture: Option<capture::Context>,
}

/// The view's state and the stop controller: the reactor's decisions, without its I/O. Everything
/// the frame needs besides the session view lives here, owned by the reactor alone: the hint line,
/// the focused pane and zoom, the overlay, and the reader's place in each of the transcript, the
/// notes, the activity and the help — each kept whatever the others do.
struct Ui {
    stop: StopController,
    events: u64,
    /// The reactor's own notice — a refused stop, a refused command, a refused paste, a capture
    /// reply: UI-local, shown above the view's own notices.
    notice: Option<String>,
    /// The reading pane the keys read and a tabbed column shows.
    focus: view::Pane,
    zoom: bool,
    overlay: input::Overlay,
    hint: input::Hint,
    /// The reader's place in the transcript: following, or held at an anchor. Kept whatever the
    /// layout, so a pane that is hidden and shown again is where it was left.
    transcript: panes::Scroll,
    notes: panes::NoteScroll,
    /// The reader's place in the slides: following the newest, or held at a slide (Task 11).
    slides: panes::SlidesScroll,
    activity: panes::ActivityScroll,
    help: usize,
    /// The capture context: the course's selections file and the selection as loaded, for Ctrl-S.
    capture: Option<capture::Context>,
    /// Where the last frame put things: reading moves count rows as the person sees them.
    drawn: view::Drawn,
    /// The preview as last parsed for drawing: at most every [`panes::PREVIEW_EVERY`], never per delta.
    preview: panes::Preview,
    /// Colour and glyphs, the terminal's for the whole session: read once, as the plain CLI reads them.
    theme: view::Theme,
    view: View,
}

/// What a key asks of the reactor: at most one command per key (plan §C 8).
#[derive(Debug, PartialEq)]
enum Act {
    Nothing,
    Redraw,
    Clear,
    Stop(Step),
    /// Enter: one notes operation.
    Op(Op),
    /// Ctrl-X: cancel this TUI's notes requests.
    Cancel,
    /// Ctrl-S while a window is watched: one manual capture. The reply comes back through
    /// [`CaptureReply`]; the slide itself only ever arrives as core's own `Event::Slide`.
    CaptureNow,
    /// Ctrl-S on the one window offered whose region is saved: kept for the course first, then
    /// exactly one `Bind` (plan Task 11) — none if the save fails.
    CaptureWatch { window: u32, selection: Selection },
}

/// What a Ctrl-S comes back as, off the reactor: the oneshot's answer to a capture-now, or the
/// watch-it save's outcome. Nothing here is a slide; slides are core's own events.
#[derive(Debug)]
enum CaptureReply {
    Now(Result<(), String>),
    /// The watch-it save failed: no `Bind` was sent.
    WatchFailed(anyhow::Error),
    /// The watch-it save succeeded and the `Bind` went out; watching arrives as its own event.
    Watched,
}

/// Enter while stopping: nothing is sent, and what was typed stays (plan §H).
pub(crate) const STOPPING: &str = "The lecture is stopping; the last snapshot takes what is left.";
/// Ctrl-X with nothing of this TUI's to cancel.
pub(crate) const NOTHING_TO_CANCEL: &str = "No notes request of yours is running.";
const HINT_FULL: &str = "The hint is at its 8 KiB limit.";
const PASTE_TOO_LONG: &str = "That paste would take the hint past 8 KiB; nothing of it was pasted.";
/// A command the lecture can no longer take: it has ended.
const ENDED: &str = "The lecture has ended; nothing was sent.";

const PANES: [view::Pane; 3] = [view::Pane::Transcript, view::Pane::Notes, view::Pane::Slides];

impl Ui {
    fn new(view: View, theme: view::Theme, capture: Option<capture::Context>) -> Ui {
        Ui {
            stop: StopController::default(),
            events: 0,
            notice: None,
            focus: view::Pane::Transcript,
            zoom: false,
            overlay: input::Overlay::None,
            hint: input::Hint::default(),
            transcript: panes::Scroll::default(),
            notes: panes::NoteScroll::default(),
            slides: panes::SlidesScroll::default(),
            activity: panes::ActivityScroll::default(),
            help: 0,
            capture,
            drawn: view::Drawn::default(),
            preview: panes::Preview::default(),
            theme,
            view,
        }
    }

    /// One stop request through the shared controller; the view's phase follows the stage it
    /// reached. Stop waiting drops core's queued requests, so this TUI's queue keeps only the one
    /// in flight — which still finishes.
    fn stop(&mut self, origin: Origin, now: std::time::Instant) -> Step {
        let step = self.stop.advance(origin, now);
        match step {
            Step::Advance { stage, .. } => {
                self.view.phase = stage;
                self.notice = None;
                if stage == Stage::StopWaiting {
                    self.view.work.hurry();
                }
            }
            Step::Ignored(_) if origin == Origin::Key => self.notice = Some(view::not_yet(self.view.phase).to_string()),
            Step::Ignored(_) | Step::Quit => {}
        }
        step
    }

    /// Every event is drained and reduced into the view; the audio's own end is a stop the controller
    /// must know of. A relocation the adapter has already saved also becomes this TUI's saved
    /// selection, without rereading the file. Returns the stop step, if any, and whether a re-read
    /// of the files was asked for.
    fn event(&mut self, e: &Event, now: std::time::Instant) -> (Option<Step>, bool) {
        self.events += 1;
        if let Event::CaptureMoved { selection, .. } = e {
            if let Some(ctx) = &mut self.capture {
                ctx.saved = Some(selection.clone());
            }
        }
        let effect = self.view.reduce(e, Local::now());
        let step = matches!(e, Event::Session(Notification::SourceEnded)).then(|| self.stop(Origin::SourceEnded, now));
        (step, effect.hydrate)
    }

    /// Captured, so the terminal reports it here instead of scrolling its own viewport. The wheel
    /// reads what the keys read — the open overlay, else the focused pane, [`panes::WHEEL`] rows a
    /// notch; where the pointer is does not matter. Press, release, drag and motion mean nothing.
    fn mouse(&mut self, mouse: MouseEvent) -> Act {
        if self.drawn.small {
            return Act::Nothing;
        }
        match mouse.kind {
            MouseEventKind::ScrollUp => self.read(panes::Move::Up(panes::WHEEL)),
            MouseEventKind::ScrollDown => self.read(panes::Move::Down(panes::WHEEL)),
            _ => Act::Nothing,
        }
    }

    /// A reading move: the open overlay's, else the focused pane's. Only a place moves: nothing is
    /// sent and nothing is read, and a pane or overlay not on screen is left where it is.
    fn read(&mut self, m: panes::Move) -> Act {
        let moved = match self.overlay {
            input::Overlay::Help => {
                let to = match m {
                    panes::Move::Up(k) => self.help.saturating_sub(k),
                    panes::Move::PageUp => self.help.saturating_sub(5),
                    panes::Move::Down(k) => self.help + k,
                    panes::Move::PageDown => self.help + 5,
                    panes::Move::Live => self.help,
                }
                .min(self.drawn.help_max);
                std::mem::replace(&mut self.help, to) != to
            }
            input::Overlay::Activity => {
                let theme = &self.theme;
                let sign = |kind: &str| view::mark(kind, theme);
                self.drawn.activity.is_some_and(|body| self.activity.apply(m, &self.view.activity, body, &sign))
            }
            input::Overlay::None => match self.focus {
                view::Pane::Transcript => match self.drawn.transcript {
                    Some(body) => self.transcript.apply(m, &self.view, body),
                    None if m == panes::Move::Live => self.transcript.apply(m, &self.view, Rect::default()),
                    None => false,
                },
                view::Pane::Notes => match self.drawn.notes {
                    Some(body) => self.notes.apply(m, &self.view, &self.preview, body),
                    None if m == panes::Move::Live => self.notes.apply(m, &self.view, &self.preview, Rect::default()),
                    None => false,
                },
                // the slides list, below the capture block, where the last frame put it
                view::Pane::Slides => match self.drawn.slides {
                    Some(body) => self.slides.apply(m, &self.view, body),
                    None if m == panes::Move::Live => self.slides.apply(m, &self.view, Rect::default()),
                    None => false,
                },
            },
        };
        if moved {
            Act::Redraw
        } else {
            Act::Nothing
        }
    }

    /// A key, by its meaning ([`input::classify`]): Ctrl-C, Ctrl-Z and Ctrl-L always; below the
    /// minimum size nothing else — typing is ignored there; then the overlays, the reading keys,
    /// Tab, Enter and the hint line's editing.
    fn key(&mut self, key: KeyEvent, now: std::time::Instant) -> Act {
        use input::Key;
        match input::classify(&key, self.overlay) {
            Key::Stop => Act::Stop(self.stop(Origin::Key, now)),
            Key::Suspend => self.say(view::SUSPEND),
            Key::Clear => Act::Clear,
            _ if self.drawn.small => Act::Nothing,
            Key::Nothing => Act::Nothing,
            Key::Cancel if self.view.work.mine() => Act::Cancel,
            Key::Cancel => self.say(NOTHING_TO_CANCEL),
            Key::Help => self.toggle(input::Overlay::Help),
            Key::Activity => self.toggle(input::Overlay::Activity),
            Key::Close => {
                self.overlay = input::Overlay::None;
                Act::Redraw
            }
            // behind an overlay, nothing that cannot be seen changes
            Key::Zoom | Key::Focus(_) | Key::Capture if self.overlay != input::Overlay::None => Act::Nothing,
            Key::Zoom => {
                self.zoom = !self.zoom;
                Act::Redraw
            }
            Key::Focus(forward) => {
                let at = PANES.iter().position(|p| *p == self.focus).unwrap_or(0);
                self.focus = PANES[if forward { (at + 1) % 3 } else { (at + 2) % 3 }];
                Act::Redraw
            }
            Key::Capture => self.capture_key(),
            Key::Read(m) => self.read(m),
            Key::Enter => self.enter(now),
            Key::Edit => match self.hint.edit(key) {
                Ok(true) => Act::Redraw,
                Ok(false) => Act::Nothing,
                Err(_) => self.say(HINT_FULL),
            },
        }
    }

    /// What the frame needs besides the session view, from the reactor's own state.
    fn chrome(&self, elapsed: Duration) -> view::Chrome<'_> {
        view::Chrome { elapsed, refused: self.notice.as_deref(), focus: self.focus, zoom: self.zoom, transcript: &self.transcript, notes: &self.notes, slides: &self.slides, preview: &self.preview, hint: &self.hint, overlay: self.overlay, activity: &self.activity, help: self.help, capture: self.capture_action(), theme: &self.theme }
    }

    fn say(&mut self, notice: impl Into<String>) -> Act {
        self.notice = Some(notice.into());
        Act::Redraw
    }

    /// What Ctrl-S does now (plan Task 11): nothing once the lecture is stopping — core drops
    /// capture at the first stop, so no manual capture can happen then either.
    fn capture_action(&self) -> capture::Action {
        if self.view.phase != Stage::Listening {
            return capture::Action::Refused(capture::Refusal::None);
        }
        capture::action(self.view.capture.as_ref(), self.capture.as_ref().and_then(|c| c.saved.as_ref()))
    }

    /// Ctrl-S, by what capture is doing (plan §H): a watched window captures now; the one window
    /// offered through its saved region is watched — saved first, by the send, then bound. Every
    /// other state says why it refuses, and sends nothing.
    fn capture_key(&mut self) -> Act {
        match self.capture_action() {
            capture::Action::Now => Act::CaptureNow,
            capture::Action::Watch { window, selection } => Act::CaptureWatch { window, selection },
            capture::Action::Refused(why) => self.say(self.capture_refusal(why)),
        }
    }

    /// Why Ctrl-S did nothing, in the state's own terms; the Slides pane holds the fuller story.
    fn capture_refusal(&self, why: capture::Refusal) -> String {
        match why {
            capture::Refusal::None => "Nothing is being watched yet.".into(),
            capture::Refusal::Unbound => "No window is chosen for this course yet: choose it once in the LectureLive app. Screenshots (⌘⇧4) still become slides.".into(),
            capture::Refusal::Paused => "Capture is paused; it comes back by itself when the window is back.".into(),
            capture::Refusal::Denied => "Screen Recording is off for this terminal; the Slides pane has the steps to turn it on.".into(),
            capture::Refusal::Failing => "Capture is failing; the Slides pane says why.".into(),
            capture::Refusal::AskingNone => "No window to watch is being offered; the Slides pane says what capture is waiting for.".into(),
            capture::Refusal::AskingMany => "More than one window is offered; choose between them in the LectureLive app.".into(),
            capture::Refusal::AskingSize => "That window size has no saved region; choose where its slide is in the LectureLive app.".into(),
            capture::Refusal::AskingOther => "The window offered is not the one saved; choose in the LectureLive app.".into(),
        }
    }

    /// A Ctrl-S reply landing back on the reactor: a capture-now that failed says why; one that
    /// succeeded says nothing, because the slide itself arrives only as core's own `Event::Slide`.
    fn capture_reply(&mut self, reply: CaptureReply) -> Act {
        match reply {
            CaptureReply::Now(Ok(())) => Act::Nothing,
            CaptureReply::Now(Err(m)) => self.say(format!("The capture did not happen: {}", plain::sentence(&plain::clean(&m)))),
            CaptureReply::Watched => Act::Nothing,
            CaptureReply::WatchFailed(e) => self.say(format!("The window was not watched: saving its region failed ({}).", plain::sentence(&format!("{e:#}")))),
        }
    }

    fn toggle(&mut self, overlay: input::Overlay) -> Act {
        self.overlay = if self.overlay == overlay { input::Overlay::None } else { overlay };
        (self.activity, self.help) = (panes::ActivityScroll::default(), 0);
        Act::Redraw
    }

    /// Enter: the grammar the plain CLI reads (`plain::parse_line`). A held key's repeats are not
    /// taken; while stopping nothing is sent and the hint stays as typed.
    fn enter(&mut self, now: std::time::Instant) -> Act {
        if !self.hint.enter(now) {
            return Act::Nothing;
        }
        if self.view.phase != Stage::Listening {
            return self.say(STOPPING);
        }
        Act::Op(plain::parse_line(self.hint.value()))
    }

    /// A bracketed paste: text into the hint, never a key. Ignored below the minimum size.
    fn paste(&mut self, text: &str) -> Act {
        if self.drawn.small {
            return Act::Nothing;
        }
        match self.hint.paste(text) {
            Ok(true) => Act::Redraw,
            Ok(false) => Act::Nothing,
            Err(_) => self.say(PASTE_TOO_LONG),
        }
    }

    /// Sends the one command a key asked for. Only a command the lecture took joins this TUI's
    /// queue (an op) or empties it (a cancel); then the hint empties. One the lecture could not
    /// take changes nothing and says so. A Ctrl-S goes further off the reactor than a send: the
    /// capture-now oneshot is answered later on a task of its own, and a watch-it is saved for the
    /// course — on a blocking thread — before its single `Bind` goes out, or not at all.
    fn send(&mut self, act: Act, commands: &UnboundedSender<Command>, replies: &tokio::sync::mpsc::Sender<CaptureReply>) {
        match act {
            Act::Op(op) => {
                let own = match op {
                    Op::Snapshot(_) => OwnOp::Snapshot,
                    Op::Polish => OwnOp::Polish,
                };
                if commands.send(Command::Op(op)).is_ok() {
                    self.view.work.submit(own);
                    self.hint.clear();
                    self.notice = None;
                } else {
                    self.notice = Some(ENDED.into());
                }
            }
            Act::Cancel => {
                if commands.send(Command::Cancel).is_ok() {
                    self.view.work.cancel();
                    self.notice = None;
                } else {
                    self.notice = Some(ENDED.into());
                }
            }
            Act::CaptureNow => {
                let (tx, rx) = tokio::sync::oneshot::channel();
                if commands.send(Command::CaptureNow(tx)).is_ok() {
                    self.notice = None;
                    let replies = replies.clone();
                    tokio::spawn(async move {
                        // core replies on its own time; the reactor stays the one place notices land
                        let answer = match rx.await {
                            Ok(r) => r,
                            Err(_) => Err("the capture worker stopped without answering".into()),
                        };
                        let _ = replies.send(CaptureReply::Now(answer)).await;
                    });
                } else {
                    self.notice = Some(ENDED.into());
                }
            }
            Act::CaptureWatch { window, selection } => {
                let (commands, replies, ctx) = (commands.clone(), replies.clone(), self.capture.clone());
                tokio::spawn(async move {
                    // kept for the course first, off the reactor: no Bind goes out until the
                    // selection is on disk (plan Task 11)
                    let saved = match ctx {
                        Some(ctx) => {
                            let sel = selection.clone();
                            tokio::task::spawn_blocking(move || capture::keep_selection(&ctx.path, &ctx.course, &sel)).await
                        }
                        None => Ok(Ok(())),
                    };
                    match saved.unwrap_or_else(|e| Err(anyhow::anyhow!("{e}"))) {
                        Ok(()) => {
                            let _ = commands.send(Command::Bind { window, selection });
                            let _ = replies.send(CaptureReply::Watched).await;
                        }
                        Err(e) => {
                            let _ = replies.send(CaptureReply::WatchFailed(e)).await;
                        }
                    }
                });
            }
            _ => {}
        }
    }
}

/// Sends the `Stop`s a step asks for; a lecture that has already ended needs none.
fn send_stops(commands: &UnboundedSender<Command>, step: Step) {
    if let Step::Advance { stops_to_send, .. } = step {
        for _ in 0..stops_to_send {
            let _ = commands.send(Command::Stop);
        }
    }
}

/// Why the reactor gave the terminal up.
enum Exit {
    /// The lecture ended: its report, its error, or its panic.
    Ended(Result<Result<StopReport>, JoinError>),
    /// Stage 3: quit at once.
    Quit,
    Terminated,
    HungUp,
    /// The keyboard stream failed or ended: the terminal is gone.
    Lost(io::Error),
    DrawFailed(io::Error),
}

/// Runs the lecture in the terminal. The canonical files are read back first, while the folder lock
/// is still held and the terminal still cooked, so a folder whose notes and state cannot be read as
/// one revision is an ordinary error with nothing started and nothing taken over. Then the signals,
/// the panic hook, the terminal, the lecture and the keyboard, in that order, and the reactor until
/// one of them ends it.
pub(crate) async fn run(session: Session, engine: impl Future<Output = Result<StopReport>> + Send + 'static, commands: UnboundedSender<Command>, events: UnboundedReceiver<Event>, secs: Option<u64>) -> Result<StopReport> {
    let hydration = hydrate::read(&session.files).await?;
    let hydration = hydration.coherent().ok_or_else(|| anyhow::anyhow!("the notes and their state in {} could not be read as one revision; the lecture was not started", session.files.dir.display()))?;
    let interrupt = signal(SignalKind::interrupt())?;
    let terminate = signal(SignalKind::terminate())?;
    let hangup = signal(SignalKind::hangup())?;
    terminal::install_panic_hook();
    let screen = terminal::enter().map_err(|e| anyhow::anyhow!("the terminal could not be taken over ({e})"))?;
    let started = Instant::now();
    let view = state::View::new(session.identity, hydration, session.seed);
    let lecture = tokio::spawn(engine);
    let keys = EventStream::new();
    // The spend sample (plan §B 7) and the hydration results (plan §F) both arrive from background
    // work; the reactor alone applies them. So do Ctrl-S's answers (plan Task 11): the oneshot's
    // reply and the watch-it save's outcome, from tasks of their own.
    let (spend_tx, spend_rx) = tokio::sync::mpsc::channel(1);
    tokio::spawn(sample_spend(session.spend.clone(), spend_tx));
    let (hydrate_tx, hydrate_rx) = tokio::sync::mpsc::channel::<anyhow::Result<hydrate::Hydration>>(1);
    let (capture_tx, capture_rx) = tokio::sync::mpsc::channel::<CaptureReply>(4);
    let exit = react(Io { screen, keys, lecture, events, commands, interrupt, terminate, hangup, files: session.files, hydrate_tx, hydrate_rx, spend_rx, capture_tx, capture_rx }, view, session.capture, started, secs).await;
    leave(exit)
}

/// Samples the lecture's spend today off the reactor (plan §B 7): `lecture_total` locks the shared
/// ledger, so it never runs on the draw path. At most once a second, one call in flight; the
/// projection keeps the last sample, and nothing is added to it here — the shared ledger is the
/// authority, and no event's dollars are folded in.
async fn sample_spend(spend: Spend, to: tokio::sync::mpsc::Sender<f64>) {
    let mut clock = tokio::time::interval(Duration::from_secs(1));
    clock.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        clock.tick().await;
        let s = spend.clone();
        let total = match tokio::task::spawn_blocking(move || s.lecture_total()).await {
            Ok(total) => total,
            Err(_) => return,
        };
        if to.send(total).await.is_err() {
            return;
        }
    }
}

struct Io {
    screen: Screen,
    keys: EventStream,
    lecture: tokio::task::JoinHandle<Result<StopReport>>,
    events: UnboundedReceiver<Event>,
    commands: UnboundedSender<Command>,
    interrupt: tokio::signal::unix::Signal,
    terminate: tokio::signal::unix::Signal,
    hangup: tokio::signal::unix::Signal,
    /// The canonical files, for the re-reads a discontinuity asks for.
    files: LectureFiles,
    hydrate_tx: tokio::sync::mpsc::Sender<anyhow::Result<hydrate::Hydration>>,
    hydrate_rx: tokio::sync::mpsc::Receiver<anyhow::Result<hydrate::Hydration>>,
    spend_rx: tokio::sync::mpsc::Receiver<f64>,
    /// Ctrl-S's answers, from the tasks that awaited them.
    capture_tx: tokio::sync::mpsc::Sender<CaptureReply>,
    capture_rx: tokio::sync::mpsc::Receiver<CaptureReply>,
}

/// One hydration at a time (plan §F): a trigger while one runs sets `again`, and the next read
/// starts only when this one's result has been merged. The file work runs on a blocking thread, so
/// no read ever happens in the reactor.
fn want_hydration(io: &Io, hydrating: &mut bool, again: &mut bool) {
    if *hydrating {
        *again = true;
        return;
    }
    *hydrating = true;
    let (files, to) = (io.files.clone(), io.hydrate_tx.clone());
    tokio::spawn(async move {
        let read = hydrate::read(&files).await;
        let _ = to.send(read).await;
    });
}

/// The loop. It draws only here, and only when something changed, at most every [`FRAME`]; returning
/// is the fence after which it never draws again.
async fn react(mut io: Io, view: View, capture: Option<capture::Context>, started: Instant, secs: Option<u64>) -> Exit {
    // Colour and glyphs are the terminal's for the whole session: read once, as the plain CLI reads them.
    let mut ui = Ui::new(view, view::Theme::detect(), capture);
    let mut timer = secs.map(|s| started + Duration::from_secs(s));
    let mut clock = interval_at(started + Duration::from_secs(1), Duration::from_secs(1));
    clock.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let (mut dirty, mut drawn_at) = (true, started - FRAME);
    let (mut hydrating, mut again) = (false, false);
    // When the preview, throttled to ten parses a second, is next due for another look.
    let mut preview_due: Option<Instant> = None;
    loop {
        tokio::select! {
            biased;
            _ = io.terminate.recv() => return Exit::Terminated,
            _ = io.hangup.recv() => return Exit::HungUp,
            // `kill -INT`: in raw mode the keyboard's Ctrl-C arrives as a key instead.
            _ = io.interrupt.recv() => match ui.stop(Origin::Signal, std::time::Instant::now()) {
                Step::Quit => return Exit::Quit,
                step => { send_stops(&io.commands, step); dirty = true; }
            },
            input = io.keys.next() => match input {
                Some(Ok(TermEvent::Key(key))) => match ui.key(key, std::time::Instant::now()) {
                    Act::Stop(Step::Quit) => return Exit::Quit,
                    Act::Stop(step) => { send_stops(&io.commands, step); dirty = true; }
                    Act::Clear => {
                        if let Err(e) = terminal::clear(&mut io.screen) {
                            return Exit::DrawFailed(e);
                        }
                        dirty = true;
                    }
                    Act::Redraw => dirty = true,
                    Act::Nothing => {}
                    act @ (Act::Op(_) | Act::Cancel | Act::CaptureNow | Act::CaptureWatch { .. }) => {
                        ui.send(act, &io.commands, &io.capture_tx);
                        dirty = true;
                    }
                },
                // The next draw lays out `frame.area()` at the new size; nothing else keeps one.
                Some(Ok(TermEvent::Resize(..))) => dirty = true,
                // The lease captures the mouse so scrolls and drags reach the TUI instead of moving
                // the viewport; every one is consumed here. The wheel reads the focused pane or the overlay.
                Some(Ok(TermEvent::Mouse(mouse))) => match ui.mouse(mouse) {
                    Act::Nothing => {}
                    Act::Redraw => dirty = true,
                    act => unreachable!("the mouse only reads: {act:?}"),
                },
                // A paste is text, never keys: it goes into the hint and never submits.
                Some(Ok(TermEvent::Paste(text))) => if ui.paste(&text) == Act::Redraw { dirty = true },
                Some(Ok(_)) => {}
                Some(Err(e)) => return Exit::Lost(e),
                None => return Exit::Lost(io::Error::other("the keyboard stream ended")),
            },
            ended = &mut io.lecture => {
                while let Ok(e) = io.events.try_recv() {
                    let _ = ui.event(&e, std::time::Instant::now());
                }
                return Exit::Ended(ended);
            }
            Some(e) = io.events.recv() => {
                let mut want = false;
                let mut next = Some(e);
                for _ in 0..BATCH {
                    let Some(e) = next.take() else { break };
                    let (step, hydrate) = ui.event(&e, std::time::Instant::now());
                    if let Some(step) = step {
                        send_stops(&io.commands, step);
                    }
                    want |= hydrate;
                    next = io.events.try_recv().ok();
                }
                if want {
                    want_hydration(&io, &mut hydrating, &mut again);
                }
                dirty = true;
            }
            Some(read) = io.hydrate_rx.recv() => {
                hydrating = false;
                match read {
                    Ok(h) => ui.view.merge(h),
                    Err(e) => ui.view.read_failed(&e, Local::now()),
                }
                if again {
                    again = false;
                    want_hydration(&io, &mut hydrating, &mut again);
                }
                dirty = true;
            }
            Some(usd) = io.spend_rx.recv() => {
                ui.view.spend = Some(usd);
                dirty = true;
            }
            Some(reply) = io.capture_rx.recv() => {
                if ui.capture_reply(reply) == Act::Redraw {
                    dirty = true;
                }
            }
            _ = sleep_until(timer.unwrap_or(started)), if timer.is_some() => {
                timer = None;
                send_stops(&io.commands, ui.stop(Origin::Timer, std::time::Instant::now()));
                dirty = true;
            }
            _ = clock.tick() => dirty = true,
            _ = sleep_until(preview_due.unwrap_or(started)), if preview_due.is_some() => {
                preview_due = None;
                dirty = true;
            }
            _ = sleep_until(drawn_at + FRAME), if dirty => {}
        }
        if dirty && Instant::now() >= drawn_at + FRAME {
            // An ended preview is dropped here, before the frame that shows its commit.
            preview_due = ui.preview.refresh(&ui.view.notes, std::time::Instant::now()).map(Instant::from_std);
            let chrome = ui.chrome(started.elapsed());
            let mut drawn = ui.drawn;
            if let Err(e) = terminal::draw(&mut io.screen, |f| drawn = view::render(f, &ui.view, &chrome)) {
                return Exit::DrawFailed(e);
            }
            ui.drawn = drawn;
            (dirty, drawn_at) = (false, Instant::now());
        }
    }
}

/// Gives the terminal back, then says why the lecture left it, in the ordinary screen.
fn leave(exit: Exit) -> Result<StopReport> {
    let _ = terminal::restore();
    // After restoration the terminal may be gone: nothing here may panic on a failed write.
    let say = |mut w: Box<dyn Write>, line: &str| {
        let _ = writeln!(w, "{line}");
        let _ = w.flush();
    };
    match exit {
        Exit::Ended(Ok(result)) => result,
        Exit::Ended(Err(e)) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
        Exit::Ended(Err(e)) => Err(anyhow::anyhow!("the lecture ended abnormally: {e}")),
        Exit::Quit => {
            say(Box::new(io::stdout()), "Stopped at once; the next session in this folder picks up what was left.");
            std::process::exit(130)
        }
        Exit::Terminated => {
            say(Box::new(io::stderr()), "Terminated; the next session in this folder picks up what was left.");
            std::process::exit(143)
        }
        Exit::HungUp => std::process::exit(129),
        Exit::Lost(e) => {
            say(Box::new(io::stderr()), &format!("The terminal was lost ({e}); the next session in this folder picks up what was left."));
            std::process::exit(129)
        }
        Exit::DrawFailed(e) => Err(anyhow::anyhow!("the terminal stopped accepting output: {e}; the next session in this folder picks up what was left")),
    }
}

/// The reactor's decisions, fed synthetic keys and events (plan §J): the terminal is the PTY suite's.
#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};
    use crate::stop::Stage;
    use crate::tui::hydrate::Hydration;
    use crate::tui::state::{Identity, SourceKind};
    use std::time::Instant;

    fn identity() -> Identity {
        Identity { course: "Machine Learning".into(), lecture: "Week 03 — Optimisation".into(), input: "BlackHole 2ch".into(), kind: SourceKind::Loopback, notes_file: "lecture_notes_20260926.md".into(), transcript_file: "lecture_transcript_20260926.txt".into() }
    }

    fn ui() -> Ui {
        Ui::new(state::View::new(identity(), Hydration::empty(), Vec::new()), view::Theme::new(lecturelive_core::session::spend::Paint { color: true, truecolor: true }, true), None)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn raw_ctrl_c_is_a_key_stop_through_the_shared_controller() {
        let (mut ui, t0) = (ui(), Instant::now());
        assert_eq!(ui.key(ctrl('c'), t0), Act::Stop(Step::Advance { stage: Stage::Stopping, stops_to_send: 1 }));
        assert_eq!(ui.view.phase, Stage::Stopping);
        // The Key origin's quiet and dwell hold: a repeat 30 ms later is not taken, and says why.
        assert!(matches!(ui.key(ctrl('c'), t0 + ms(30)), Act::Stop(Step::Ignored(_))));
        assert_eq!(ui.notice.as_deref(), Some(view::not_yet(Stage::Stopping)));
        assert_eq!(ui.key(ctrl('c'), t0 + ms(2500)), Act::Stop(Step::Advance { stage: Stage::StopWaiting, stops_to_send: 1 }));
        assert_eq!((ui.view.phase, ui.notice.as_deref()), (Stage::StopWaiting, None));
        assert_eq!(ui.key(ctrl('c'), t0 + ms(5000)), Act::Stop(Step::Quit));
    }

    /// Plan §J's `first_and_second_stop_phase`: the projection's phase only follows the shared
    /// controller; no lecture state moves with it, and nothing is sent for the third.
    #[test]
    fn first_and_second_stop_phase() {
        let (mut ui, t0) = (ui(), Instant::now());
        assert_eq!(ui.view.phase, Stage::Listening);
        assert_eq!(ui.stop(Origin::Key, t0), Step::Advance { stage: Stage::Stopping, stops_to_send: 1 });
        assert_eq!(ui.view.phase, Stage::Stopping);
        let held = (ui.view.closed.clone(), ui.view.gaps(), ui.view.notes.revision, ui.view.stt.clone(), ui.view.open.clone());
        assert_eq!(ui.stop(Origin::Key, t0 + ms(2500)), Step::Advance { stage: Stage::StopWaiting, stops_to_send: 1 });
        assert_eq!(ui.view.phase, Stage::StopWaiting, "the projection reflects the stage and nothing more");
        assert_eq!((ui.view.closed.clone(), ui.view.gaps(), ui.view.notes.revision, ui.view.stt.clone(), ui.view.open.clone()), held);
        assert_eq!(ui.stop(Origin::Key, t0 + ms(5000)), Step::Quit);
    }

    #[test]
    fn source_ended_is_a_stop_the_controller_counts() {
        let (mut ui, t0) = (ui(), Instant::now());
        assert_eq!(ui.event(&Event::Session(Notification::Level(0.1)), t0), (None, false));
        assert_eq!(ui.event(&Event::Session(Notification::SourceEnded), t0), (Some(Step::Advance { stage: Stage::Stopping, stops_to_send: 0 }), false));
        assert_eq!(ui.view.phase, Stage::Stopping, "the audio ended by itself");
        assert_eq!(ui.events, 2, "every event is counted");
        // Stop waiting after it sends two: core had counted none.
        assert_eq!(ui.key(ctrl('c'), t0 + ms(2500)), Act::Stop(Step::Advance { stage: Stage::StopWaiting, stops_to_send: 2 }));
    }

    #[test]
    fn source_ended_after_a_stop_changes_nothing() {
        let (mut ui, t0) = (ui(), Instant::now());
        ui.key(ctrl('c'), t0);
        let (step, _) = ui.event(&Event::Session(Notification::SourceEnded), t0 + ms(200));
        assert!(matches!(step, Some(Step::Ignored(_))));
        assert_eq!((ui.view.phase, ui.notice), (Stage::Stopping, None), "no notice: nobody pressed anything");
    }

    #[test]
    fn external_sigint_is_not_debounced() {
        let (mut ui, t0) = (ui(), Instant::now());
        assert_eq!(ui.stop(Origin::Signal, t0), Step::Advance { stage: Stage::Stopping, stops_to_send: 1 });
        assert_eq!(ui.stop(Origin::Signal, t0), Step::Advance { stage: Stage::StopWaiting, stops_to_send: 1 });
        assert_eq!(ui.stop(Origin::Signal, t0), Step::Quit);
    }

    #[test]
    fn ctrl_z_only_says_why_it_does_nothing_and_ctrl_l_clears() {
        let (mut ui, t0) = (ui(), Instant::now());
        assert_eq!(ui.key(ctrl('z'), t0), Act::Redraw);
        assert_eq!((ui.view.phase, ui.notice.as_deref()), (Stage::Listening, Some(view::SUSPEND)));
        assert_eq!(ui.key(ctrl('l'), t0), Act::Clear);
        assert_eq!(ui.view.phase, Stage::Listening);
    }

    /// A lecture `n` segments long, each a sentence or two, five seconds apart.
    fn lecture(n: u64) -> Ui {
        use lecturelive_core::session::segments::{Segment, SegmentSource};
        let mut ui = ui();
        let start = chrono::TimeZone::with_ymd_and_hms(&Local, 2026, 9, 26, 9, 0, 0).unwrap();
        for id in 0..n {
            let t = start + chrono::Duration::seconds(id as i64 * 5);
            let text = format!("Segment {id}: the gradient points uphill, so each step goes against it{}", ", scaled by the learning rate and checked against the validation loss".repeat(id as usize % 3));
            let source = if id % 97 == 13 { SegmentSource::Recovered } else { SegmentSource::Live };
            ui.event(&Event::Session(Notification::Segment(Segment { id, recording_id: Default::default(), start_sample: id * 80_000, end_sample: (id + 1) * 80_000, said_at: t, start: t, end: t, text, words: Vec::new(), source })), Instant::now());
        }
        ui
    }

    /// Draws as the reactor does, keeping where the transcript went; returns the frame's rows.
    fn frame(ui: &mut Ui, width: u16, height: u16) -> Vec<String> {
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        ui.preview.refresh(&ui.view.notes, std::time::Instant::now());
        let chrome = ui.chrome(Duration::from_secs(60));
        let mut drawn = ui.drawn;
        terminal.draw(|f| drawn = view::render(f, &ui.view, &chrome)).unwrap();
        ui.drawn = drawn;
        let b = terminal.backend().buffer();
        (0..height).map(|y| (0..width).map(|x| b[(x, y)].symbol()).collect::<String>().trim_end().to_string()).collect()
    }

    fn wheel(kind: MouseEventKind) -> MouseEvent {
        MouseEvent { kind, column: 40, row: 12, modifiers: KeyModifiers::NONE }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// The wheel now reads the transcript — the terminal's viewport never moves, since the mouse is
    /// captured — and everything else the mouse does is consumed and changes nothing. Scrolling
    /// sends nothing and reads nothing: no event counted, no hydration, no stop.
    #[test]
    fn the_wheel_reads_the_transcript_and_the_rest_of_the_mouse_does_nothing() {
        use ratatui::crossterm::event::MouseButton;
        let mut ui = lecture(60);
        let live = frame(&mut ui, 110, 32);
        let held = (ui.view.closed.clone(), ui.view.gaps(), ui.view.notes.revision, ui.view.stt.clone(), ui.view.open.clone(), ui.events);
        for kind in [MouseEventKind::Down(MouseButton::Left), MouseEventKind::Up(MouseButton::Left), MouseEventKind::Drag(MouseButton::Left), MouseEventKind::ScrollLeft, MouseEventKind::ScrollRight, MouseEventKind::Moved] {
            assert_eq!(ui.mouse(wheel(kind)), Act::Nothing, "{kind:?}: consumed, and nothing more");
        }
        assert!(ui.transcript.following());
        assert_eq!(ui.mouse(wheel(MouseEventKind::ScrollDown)), Act::Nothing, "already at the live end");
        assert_eq!(ui.mouse(wheel(MouseEventKind::ScrollUp)), Act::Redraw);
        assert!(!ui.transcript.following(), "up leaves the live end");
        let up = frame(&mut ui, 110, 32);
        let left = |rows: &[String]| rows.iter().map(|r| r.chars().take(44).collect::<String>()).collect::<Vec<_>>();
        assert_eq!(left(&up[4 + panes::WHEEL..28]), left(&live[4..28 - panes::WHEEL]), "one notch is {} rows", panes::WHEEL);
        assert!(up[3].starts_with(" Transcript   Slides               Esc live │ Notes"), "the lane ends at the column's edge: {:?}", up[3]);
        assert_eq!(ui.mouse(wheel(MouseEventKind::ScrollDown)), Act::Redraw);
        assert!(ui.transcript.following(), "down to the end follows again");
        assert_eq!(frame(&mut ui, 110, 32), live);
        assert_eq!((ui.view.closed.clone(), ui.view.gaps(), ui.view.notes.revision, ui.view.stt.clone(), ui.view.open.clone(), ui.events), held, "no state moved, no hydration, no event");
        assert_eq!((ui.view.phase, ui.notice), (Stage::Listening, None), "no stop");
    }

    /// The reading keys: ↑/↓ a row, PgUp/PgDn a page less two rows, Esc back to live. Printable
    /// keys still do nothing, and while the transcript is not on screen its place is left alone.
    #[test]
    fn reading_keys_scroll_the_transcript() {
        let mut ui = lecture(60);
        let live = frame(&mut ui, 80, 25);
        assert_eq!(ui.key(key(KeyCode::Esc), Instant::now()), Act::Nothing, "Esc while live changes nothing");
        assert_eq!(ui.key(key(KeyCode::Up), Instant::now()), Act::Redraw);
        let up = frame(&mut ui, 80, 25);
        assert_eq!(up[4..21], live[3..20], "one row");
        assert_eq!(up[2].trim_end(), format!(" Transcript   Notes   Slides{}Esc live", " ".repeat(79 - 28 - 8)), "narrow: the lane beside the tabs");
        ui.key(key(KeyCode::PageUp), Instant::now());
        ui.key(key(KeyCode::PageDown), Instant::now());
        assert_eq!(frame(&mut ui, 80, 25), up, "a page and back");
        // printable keys are the hint's: they type, and the transcript's place does not move
        for c in ['k', 'j', ' ', 'q'] {
            assert_eq!(ui.key(key(KeyCode::Char(c)), Instant::now()), Act::Redraw, "{c:?}");
        }
        assert_eq!(ui.hint.value(), "kj q");
        assert_eq!(frame(&mut ui, 80, 25)[3..21], up[3..21], "the reading position held while typing");
        assert_eq!(ui.key(key(KeyCode::Down), Instant::now()), Act::Redraw);
        assert!(ui.transcript.following(), "down to the end");
        ui.key(key(KeyCode::PageUp), Instant::now());
        assert_eq!(ui.key(key(KeyCode::Esc), Instant::now()), Act::Redraw);
        assert!(ui.transcript.following());
        assert_eq!(frame(&mut ui, 80, 25)[..23], live[..23], "the live bottom again (the hint still holds what was typed)");
        // too small: nothing on screen, nothing moves
        frame(&mut ui, 40, 8);
        assert!(ui.drawn.small && ui.drawn.transcript.is_none());
        assert_eq!(ui.key(key(KeyCode::Up), Instant::now()), Act::Nothing);
        assert!(ui.transcript.following());
    }

    /// Plan Task 8: a two-hour lecture, 1,440 segments. A frame wraps only what it shows, following
    /// or scrolled; the reader's place survives 140 → 80 → 140 and arrivals below. The frame time
    /// is informational (Task 13 owns the target): `--nocapture` prints it.
    #[test]
    fn a_two_hour_transcript_scrolls_by_what_is_on_screen() {
        let mut ui = lecture(1_440);
        let wrapped = |ui: &mut Ui, w, h| {
            panes::WRAPPED.with(|n| n.set(0));
            let rows = frame(ui, w, h);
            (rows, panes::WRAPPED.with(|n| n.get()))
        };
        let (live, n) = wrapped(&mut ui, 140, 40);
        assert!(live[4..36].iter().any(|r| r.contains("Segment 1439")) && !live[35].is_empty(), "the live end at the bottom: {live:?}");
        assert!(n <= 2 * 30, "following wraps the visible segments (twice at most), not 1,440: {n}");
        // scroll well back: a frame still wraps only what it shows
        for _ in 0..200 {
            ui.key(key(KeyCode::PageUp), Instant::now());
        }
        let (back, n) = wrapped(&mut ui, 140, 40);
        assert!(n <= 30, "scrolled: {n}");
        let anchor = ui.transcript.anchor().unwrap();
        assert!(anchor.id < 1_000, "{anchor:?}");
        let (_, n) = wrapped(&mut ui, 80, 24);
        assert!(n <= 30);
        assert_eq!(ui.transcript.anchor(), Some(anchor), "a resize moves no anchor");
        assert_eq!(frame(&mut ui, 140, 40), back, "140 → 80 → 140: the same rows");
        // arrivals while scrolled
        let mut more = lecture(1_443);
        more.transcript = ui.transcript.clone();
        let rows = frame(&mut more, 140, 40);
        assert_eq!(rows[4..36], back[4..36], "the place held while three more arrived");
        assert!(rows[3].contains("3 new below   Esc live"), "{:?}", rows[3]);
        // informational timing at 140×40
        let (runs, t0) = (200, std::time::Instant::now());
        for _ in 0..runs {
            frame(&mut ui, 140, 40);
        }
        let scrolled = t0.elapsed() / runs;
        ui.key(key(KeyCode::Esc), Instant::now());
        let t0 = std::time::Instant::now();
        for _ in 0..runs {
            frame(&mut ui, 140, 40);
        }
        eprintln!("two-hour transcript, 140×40 full frame on TestBackend (dev profile): following {:?}, scrolled {scrolled:?} a frame", t0.elapsed() / runs);
    }

    #[test]
    fn plain_keys_type_and_releases_do_nothing() {
        let (mut ui, t0) = (ui(), Instant::now());
        assert_eq!(ui.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE), t0), Act::Redraw, "a plain c is text, never a stop");
        let mut release = ctrl('c');
        release.kind = KeyEventKind::Release;
        assert_eq!(ui.key(release, t0), Act::Nothing);
        assert_eq!(ui.view.phase, Stage::Listening);
        assert_eq!(ui.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), t0), Act::Op(Op::Snapshot("c".into())), "Enter takes the hint");
    }

    // ---- the hint line, notes operations, overlays (Task 10) ----------------------------------

    /// A key as the reactor takes it: the Ui decides, and the one command a key may carry is sent.
    fn press(ui: &mut Ui, k: KeyEvent, now: Instant, tx: &UnboundedSender<Command>) -> Act {
        let act = ui.key(k, now);
        if matches!(act, Act::Op(_) | Act::Cancel) {
            let (replies, _) = tokio::sync::mpsc::channel(1);
            ui.send(act, tx, &replies);
            return Act::Redraw;
        }
        act
    }

    fn type_in(ui: &mut Ui, text: &str, now: Instant, tx: &UnboundedSender<Command>) {
        for c in text.chars() {
            press(ui, key(KeyCode::Char(c)), now, tx);
        }
    }

    fn sent(rx: &mut tokio::sync::mpsc::UnboundedReceiver<Command>) -> Vec<String> {
        let mut out = Vec::new();
        while let Ok(c) = rx.try_recv() {
            out.push(match c {
                Command::Op(Op::Snapshot(h)) => format!("snapshot {h:?}"),
                Command::Op(Op::Polish) => "polish".into(),
                Command::Cancel => "cancel".into(),
                Command::Stop => "stop".into(),
                Command::CaptureNow(_) => "capture-now".into(),
                Command::Bind { window, .. } => format!("bind {window}"),
                other => format!("{other:?}"),
            });
        }
        out
    }

    /// A Ui already drawn once at 110×32, as the reactor's first frame does.
    fn drawn_ui() -> Ui {
        let mut ui = lecture(60);
        frame(&mut ui, 110, 32);
        ui
    }

    /// Plan §J `one_press_one_command`: each accepted press sends exactly one command — empty
    /// Enter a snapshot, a hint and Enter a hinted snapshot, `polish` and Enter a polish; a held
    /// Enter's repeats send nothing; a paste sends nothing; while stopping Enter sends nothing.
    #[test]
    fn one_press_one_command() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let (mut ui, t0) = (drawn_ui(), Instant::now());
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        press(&mut ui, key(KeyCode::Enter), at(0), &tx);
        assert_eq!(sent(&mut rx), ["snapshot \"\""]);
        type_in(&mut ui, "focus on treatment", at(10), &tx);
        assert_eq!(sent(&mut rx), Vec::<String>::new(), "typing sends nothing");
        press(&mut ui, key(KeyCode::Enter), at(400), &tx);
        assert_eq!(sent(&mut rx), ["snapshot \"focus on treatment\""]);
        assert_eq!(ui.hint.value(), "", "an accepted hint empties the line");
        type_in(&mut ui, "  Polish ", at(410), &tx);
        press(&mut ui, key(KeyCode::Enter), at(800), &tx);
        assert_eq!(sent(&mut rx), ["polish"], "the plain grammar, trimmed and case-blind");
        // a held Enter: repeats 33 ms apart send nothing more
        for k in 1..30 {
            press(&mut ui, key(KeyCode::Enter), at(800 + 33 * k), &tx);
        }
        assert_eq!(sent(&mut rx), Vec::<String>::new(), "no repeat is taken");
        // a paste, with a newline, a polish and a Ctrl-C byte in it: text only
        assert_eq!(ui.paste("polish\r\n\u{3}and more"), Act::Redraw);
        assert_eq!(sent(&mut rx), Vec::<String>::new());
        assert_eq!(ui.hint.value(), "polish and more");
        assert_eq!(ui.view.phase, Stage::Listening, "a pasted Ctrl-C is no stop");
        press(&mut ui, key(KeyCode::Enter), at(5000), &tx);
        assert_eq!(sent(&mut rx), ["snapshot \"polish and more\""], "only a real Enter sends it");
        // four accepted presses, four ops, in order, in the TUI's own queue
        assert_eq!((ui.view.work.lane(), ui.view.work.queued()), (state::Lane::Snapshot, 3));
        // stopping: Enter sends nothing
        ui.key(ctrl('c'), at(6000));
        assert_eq!(sent(&mut rx), Vec::<String>::new(), "the stop is the reactor's to send");
        type_in(&mut ui, "late", at(6100), &tx);
        press(&mut ui, key(KeyCode::Enter), at(7000), &tx);
        assert_eq!(sent(&mut rx), Vec::<String>::new());
    }

    /// Plan §J: while stopping, Enter is refused with the desktop's words and the text is kept.
    #[test]
    fn ops_refused_while_stopping_keep_text() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let (mut ui, t0) = (drawn_ui(), Instant::now());
        type_in(&mut ui, "keep this hint", t0, &tx);
        assert!(matches!(ui.key(ctrl('c'), t0), Act::Stop(Step::Advance { stage: Stage::Stopping, .. })));
        assert_eq!(press(&mut ui, key(KeyCode::Enter), t0 + ms(500), &tx), Act::Redraw);
        assert!(sent(&mut rx).is_empty());
        assert_eq!(ui.hint.value(), "keep this hint", "the text stays");
        assert_eq!(ui.notice.as_deref(), Some(STOPPING));
        assert!(!ui.view.work.mine(), "nothing joined the queue");
        // the audio ending by itself is stopping too
        let mut ui = drawn_ui();
        ui.event(&Event::Session(Notification::SourceEnded), t0);
        type_in(&mut ui, "x", t0, &tx);
        press(&mut ui, key(KeyCode::Enter), t0, &tx);
        assert!(sent(&mut rx).is_empty() && ui.hint.value() == "x");
    }

    /// Ctrl-X: one Cancel while this TUI has requests, and its queue empties; with none, the
    /// notice and no command. The study page and the last snapshot are not its to cancel.
    #[test]
    fn ctrl_x_cancels_only_this_tuis_requests() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let (mut ui, t0) = (drawn_ui(), Instant::now());
        press(&mut ui, ctrl('x'), t0, &tx);
        assert!(sent(&mut rx).is_empty());
        assert_eq!(ui.notice.as_deref(), Some(NOTHING_TO_CANCEL));
        press(&mut ui, key(KeyCode::Enter), t0, &tx);
        type_in(&mut ui, "polish", t0, &tx);
        press(&mut ui, key(KeyCode::Enter), t0 + ms(400), &tx);
        assert_eq!(sent(&mut rx).len(), 2);
        assert_eq!(ui.view.work.queued(), 1);
        press(&mut ui, ctrl('x'), t0 + ms(500), &tx);
        assert_eq!(sent(&mut rx), ["cancel"], "exactly one");
        assert!(!ui.view.work.mine(), "the queue emptied");
        press(&mut ui, ctrl('x'), t0 + ms(600), &tx);
        assert!(sent(&mut rx).is_empty(), "nothing of this TUI's is left");
        // a typesetting page and the last snapshot are not cancellable
        let mut ui = drawn_ui();
        ui.view.work.submit(OwnOp::Polish);
        ui.event(&Event::NothingNew, t0);
        ui.event(&Event::Polished { backup: "b.md".into(), usd: 0.0, revision: 1 }, t0);
        ui.key(ctrl('c'), t0);
        ui.event(&Event::Preview("the last snapshot ".into()), t0);
        assert_eq!(ui.view.work.lane(), state::Lane::LastSnapshot, "the automatic last snapshot is not taken for the person's own");
        press(&mut ui, ctrl('x'), t0 + ms(100), &tx);
        assert!(sent(&mut rx).is_empty());
        assert!(ui.view.work.page());
    }

    /// Plan §J `cancel_then_commit_accepts_committed_reality`, through the reactor's own path: a
    /// commit that won the race with Ctrl-X still lands in the notes.
    #[test]
    fn cancel_then_commit_accepts_committed_reality() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let (mut ui, t0) = (drawn_ui(), Instant::now());
        press(&mut ui, key(KeyCode::Enter), t0, &tx);
        ui.event(&Event::Preview("## Racing the cancel ".into()), t0);
        press(&mut ui, ctrl('x'), t0 + ms(50), &tx);
        assert_eq!(sent(&mut rx), ["snapshot \"\"", "cancel"]);
        ui.event(&Event::Committed { words: 10, slides: 0, block: "\n<!-- 10:00:00 -->\n## Won the race\n".into(), usd: 0.0, confirmed: true, removed: 0, missing: 0, revision: 1 }, t0);
        assert_eq!(ui.view.notes.revision, 1);
        assert!(ui.view.notes.chunks.iter().flat_map(|c| &c.blocks).any(|b| b.text == "Won the race"));
        assert_eq!(ui.view.notes.preview, None);
    }

    /// Stop waiting: core drops the queued requests behind the one in flight; this TUI's queue does
    /// the same, and does not claim the one in flight was stopped.
    #[test]
    fn second_stop_drops_queued_local_tails() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let (mut ui, t0) = (drawn_ui(), Instant::now());
        for (k, text) in ["", "one", "polish"].iter().enumerate() {
            type_in(&mut ui, text, t0, &tx);
            press(&mut ui, key(KeyCode::Enter), t0 + ms(400 * k as u64), &tx);
        }
        assert_eq!(ui.view.work.queued(), 2);
        ui.key(ctrl('c'), t0 + ms(2000));
        assert_eq!(ui.view.work.queued(), 2, "stage 1: queued requests still run");
        ui.key(ctrl('c'), t0 + ms(4500));
        assert_eq!(ui.view.phase, Stage::StopWaiting);
        assert_eq!((ui.view.work.lane(), ui.view.work.queued()), (state::Lane::Snapshot, 0), "the one in flight is still there");
    }

    /// Tab and Shift-Tab cycle Transcript → Notes → Slides; each pane keeps its place.
    #[test]
    fn tab_cycles_the_reading_panes_and_each_keeps_its_place() {
        let mut ui = lecture(60);
        ui.view.merge(Hydration { notes: crate::tui::hydrate::NotesSnapshot::At { revision: 1, document: format!("# Notes\n\n<!-- 09:00:00 -->\n## A\n{}", "- a bullet that the reader can scroll past\n".repeat(80)) }, ..Hydration::empty() });
        let tab = key(KeyCode::Tab);
        let back = KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT);
        frame(&mut ui, 80, 25);
        ui.key(key(KeyCode::PageUp), Instant::now());
        let transcript = ui.transcript.anchor().unwrap();
        assert_eq!(ui.key(tab, Instant::now()), Act::Redraw);
        assert_eq!(ui.focus, view::Pane::Notes);
        let rows = frame(&mut ui, 80, 25);
        assert!(rows[2].starts_with(" Transcript   Notes   Slides") && rows.iter().any(|r| r.contains("• a bullet")), "narrow shows the notes now: {rows:?}");
        ui.key(key(KeyCode::Up), Instant::now());
        ui.key(key(KeyCode::Up), Instant::now());
        let notes = ui.notes.anchor().unwrap();
        ui.key(tab, Instant::now());
        assert_eq!(ui.focus, view::Pane::Slides);
        let rows = frame(&mut ui, 80, 25);
        assert!(rows[3].contains("No slides yet."), "the real slides pane: {rows:?}");
        // the slides read too: enough arrive to overflow the pane, up leaves the newest, Esc back
        for index in 1..=30u32 {
            ui.event(&Event::Slide { index, file: format!("slides/slide_{index}.png"), auto: index % 3 != 0, uncertain: index % 3 == 2, shown_at: Local::now() }, Instant::now());
        }
        let slides = frame(&mut ui, 80, 25);
        assert!(slides.iter().any(|r| r.contains("Slide 30")), "{slides:?}");
        assert_eq!(ui.key(key(KeyCode::Up), Instant::now()), Act::Redraw, "the slides scroll now (Task 11)");
        assert!(!ui.slides.following());
        assert_eq!(ui.key(key(KeyCode::Down), Instant::now()), Act::Redraw);
        assert!(ui.slides.following(), "down to the end follows again");
        ui.key(key(KeyCode::PageUp), Instant::now());
        assert_eq!(ui.key(key(KeyCode::Esc), Instant::now()), Act::Redraw);
        assert!(ui.slides.following(), "Esc returns the slides to live");
        ui.key(tab, Instant::now());
        assert_eq!(ui.focus, view::Pane::Transcript);
        ui.key(back, Instant::now());
        assert_eq!(ui.focus, view::Pane::Slides, "shift-tab goes back");
        ui.key(back, Instant::now());
        ui.key(back, Instant::now());
        assert_eq!(ui.focus, view::Pane::Transcript);
        frame(&mut ui, 80, 25);
        assert_eq!((ui.transcript.anchor(), ui.notes.anchor()), (Some(transcript), Some(notes)), "both places held across every switch");
        // in normal the notes stay on the right and the left tab follows the focus
        let rows = frame(&mut ui, 110, 32);
        assert!(rows[3].starts_with(" Transcript   Slides"), "{:?}", rows[3]);
    }

    /// The wheel reads the focused pane — or the open overlay — whatever the pointer is over.
    #[test]
    fn the_wheel_follows_the_focus() {
        let mut ui = lecture(60);
        ui.view.merge(Hydration { notes: crate::tui::hydrate::NotesSnapshot::At { revision: 1, document: "- note\n".repeat(90) }, ..Hydration::empty() });
        ui.key(key(KeyCode::Tab), Instant::now());
        frame(&mut ui, 110, 32);
        assert_eq!(ui.mouse(wheel(MouseEventKind::ScrollUp)), Act::Redraw);
        assert!(ui.notes.scrolled() && ui.transcript.following(), "the notes moved, the transcript did not");
        assert_eq!(ui.mouse(wheel(MouseEventKind::ScrollDown)), Act::Redraw);
        assert!(!ui.notes.scrolled());
        // the slides read the wheel too, through their own list (Task 11)
        ui.key(key(KeyCode::Tab), Instant::now());
        assert_eq!(ui.focus, view::Pane::Slides);
        for index in 1..=40u32 {
            ui.event(&Event::Slide { index, file: format!("slides/slide_{index}.png"), auto: true, uncertain: false, shown_at: Local::now() }, Instant::now());
        }
        frame(&mut ui, 110, 32);
        assert!(ui.drawn.slides.is_some(), "the slides list is on screen");
        assert_eq!(ui.mouse(wheel(MouseEventKind::ScrollUp)), Act::Redraw);
        assert!(!ui.slides.following(), "the wheel left the newest slide");
        assert_eq!(ui.mouse(wheel(MouseEventKind::ScrollDown)), Act::Redraw);
        assert!(ui.slides.following(), "down to the end follows again");
    }

    /// Esc closes an overlay first; only then does it take a scrolled pane back to live. Behind
    /// help, no reading key, Tab or zoom changes anything unseen.
    #[test]
    fn overlays_own_their_keys() {
        let mut ui = drawn_ui();
        ui.key(key(KeyCode::PageUp), Instant::now());
        let held = ui.transcript.anchor();
        assert!(held.is_some());
        assert_eq!(ui.key(ctrl('h'), Instant::now()), Act::Redraw);
        assert_eq!(ui.overlay, input::Overlay::Help);
        frame(&mut ui, 110, 32);
        for k in [key(KeyCode::Up), key(KeyCode::Down), key(KeyCode::PageUp), key(KeyCode::Tab), ctrl('t')] {
            ui.key(k, Instant::now());
        }
        assert_eq!((ui.transcript.anchor(), ui.focus, ui.zoom), (held, view::Pane::Transcript, false), "nothing hidden moved");
        assert_eq!(ui.key(key(KeyCode::Esc), Instant::now()), Act::Redraw);
        assert_eq!(ui.overlay, input::Overlay::None, "Esc closed the help");
        assert_eq!(ui.transcript.anchor(), held, "and did not touch the transcript");
        ui.key(key(KeyCode::Esc), Instant::now());
        assert!(ui.transcript.following(), "the next Esc goes back to live");
        // F1 is help too; ^H again closes it; ^O swaps to activity
        ui.key(key(KeyCode::F(1)), Instant::now());
        assert_eq!(ui.overlay, input::Overlay::Help);
        ui.key(ctrl('o'), Instant::now());
        assert_eq!(ui.overlay, input::Overlay::Activity);
        ui.key(ctrl('o'), Instant::now());
        assert_eq!(ui.overlay, input::Overlay::None);
        // app chords still work behind an overlay
        ui.key(ctrl('h'), Instant::now());
        assert_eq!(ui.key(ctrl('l'), Instant::now()), Act::Clear);
        assert!(matches!(ui.key(ctrl('c'), Instant::now()), Act::Stop(_)));
    }

    /// The activity overlay scrolls on its own: the transcript and the notes keep their places,
    /// and a new record while it is held does not move it.
    #[test]
    fn activity_scrolls_on_its_own() {
        let mut ui = drawn_ui();
        for k in 0..80 {
            ui.event(&Event::Warning(format!("warning {k}")), Instant::now());
        }
        ui.key(key(KeyCode::PageUp), Instant::now());
        let transcript = ui.transcript.anchor();
        ui.key(ctrl('o'), Instant::now());
        let open = frame(&mut ui, 110, 32);
        assert!(open[3].starts_with(" Activity") && open[27].contains("warning 79"), "{open:?}");
        assert_eq!(ui.key(key(KeyCode::PageUp), Instant::now()), Act::Redraw);
        assert_eq!(ui.mouse(wheel(MouseEventKind::ScrollUp)), Act::Redraw);
        let held = frame(&mut ui, 110, 32);
        assert!(!held[27].contains("warning 79"));
        ui.event(&Event::Warning("a newer one".into()), Instant::now());
        assert_eq!(frame(&mut ui, 110, 32)[4..28], held[4..28], "a new record does not yank it");
        assert_eq!(ui.transcript.anchor(), transcript, "the transcript is where it was");
        ui.key(key(KeyCode::Esc), Instant::now());
        assert_eq!(ui.transcript.anchor(), transcript);
        ui.key(ctrl('o'), Instant::now());
        assert!(frame(&mut ui, 110, 32)[27].contains("a newer one"), "reopened, it follows the newest");
    }

    /// Ctrl-T: the focused pane fills the body; again, back; Tab while zoomed shows the next pane.
    #[test]
    fn ctrl_t_zooms_the_focused_pane() {
        let mut ui = drawn_ui();
        assert_eq!(ui.key(ctrl('t'), Instant::now()), Act::Redraw);
        assert!(ui.zoom);
        let rows = frame(&mut ui, 110, 32);
        assert!(!rows[5].contains('│') && ui.drawn.transcript.is_some_and(|b| b.width == 108), "{rows:?}");
        ui.key(key(KeyCode::Tab), Instant::now());
        frame(&mut ui, 110, 32);
        assert!(ui.drawn.notes.is_some_and(|b| b.width == 108) && ui.drawn.transcript.is_none());
        ui.key(ctrl('t'), Instant::now());
        frame(&mut ui, 110, 32);
        assert!(!ui.zoom && ui.drawn.transcript.is_some() && ui.drawn.notes.is_some());
    }

    /// Below the minimum size typing is ignored — no invisible hint piles up — while Ctrl-C,
    /// Ctrl-L and the capture key still classify and none of them sends anything.
    #[test]
    fn too_small_ignores_typing() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut ui = lecture(10);
        frame(&mut ui, 40, 8);
        assert!(ui.drawn.small);
        type_in(&mut ui, "invisible", Instant::now(), &tx);
        assert_eq!(ui.paste("pasted"), Act::Nothing);
        for k in [key(KeyCode::Enter), key(KeyCode::Tab), ctrl('x'), ctrl('t'), ctrl('h'), ctrl('s')] {
            assert_eq!(press(&mut ui, k, Instant::now(), &tx), Act::Nothing, "{k:?}");
        }
        assert_eq!(ui.hint.value(), "");
        assert!(sent(&mut rx).is_empty());
        assert_eq!(ui.key(ctrl('l'), Instant::now()), Act::Clear);
        assert!(matches!(ui.key(ctrl('c'), Instant::now()), Act::Stop(Step::Advance { .. })));
    }

    /// Ctrl-S is Task 11's: with nothing watched it says why and sends nothing.
    #[test]
    fn no_ctrl_s_action_yet() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut ui = drawn_ui();
        type_in(&mut ui, "abc", Instant::now(), &tx);
        assert_eq!(press(&mut ui, ctrl('s'), Instant::now(), &tx), Act::Redraw);
        assert_eq!(ui.hint.value(), "abc", "Ctrl-S never edits");
        assert!(sent(&mut rx).is_empty());
        assert_eq!(ui.notice.as_deref(), Some("Nothing is being watched yet."));
    }

    // ---- Ctrl-S and capture (Task 11) ---------------------------------------------------------

    use lecturelive_core::capture::detect::Region;
    use lecturelive_core::capture::select::{Descriptor, Selection, Selections, SizedRegion};
    use lecturelive_core::capture::window::WindowInfo;
    use lecturelive_core::capture::worker::CaptureState;

    /// The course's saved window, as the app leaves it: Zoom's meeting window at 1600 × 900, its
    /// smaller size remembered, a camera patch left out.
    fn saved_selection() -> Selection {
        Selection {
            descriptor: Descriptor { bundle_id: Some("us.zoom.xos".into()), app: "zoom.us".into(), title: "Zoom Meeting".into(), width: 1600, height: 900 },
            region: Region { x: 0.1, y: 0.1, w: 0.8, h: 0.8 },
            leave_out: vec![Region { x: 0.75, y: 0.0, w: 0.25, h: 0.3 }],
            sizes: vec![SizedRegion { width: 1280, height: 720, region: Region { x: 0.05, y: 0.12, w: 0.9, h: 0.7 } }],
        }
    }

    fn candidate(id: u32, w: u32, h: u32) -> WindowInfo {
        WindowInfo { id, app: "zoom.us".into(), bundle_id: Some("us.zoom.xos".into()), title: "Zoom Meeting".into(), width: w, height: h, on_screen: true }
    }

    fn asking(candidates: Vec<WindowInfo>) -> Event {
        Event::Capture(CaptureState::Asking { window: "Zoom Meeting".into(), reason: "Zoom Meeting is 1280 × 720 now; it was 1600 × 900".into(), candidates })
    }

    /// A Ui with a capture context: the course's selections in a temp file, the saved selection
    /// as it was loaded before the lecture started.
    fn capture_ui(dir: &tempfile::TempDir) -> Ui {
        let mut ui = ui();
        ui.capture = Some(crate::capture::Context { path: dir.path().join("capture.json"), course: identity().course, saved: Some(saved_selection()) });
        ui
    }

    /// Ctrl-S while watching: exactly one `CaptureNow`; a successful reply alone fabricates no
    /// slide (core's own `Event::Slide` is the truth), a failed one says why.
    #[tokio::test]
    async fn ctrl_s_watching_sends_exactly_one_capture_now() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let (replies, mut reply_rx) = tokio::sync::mpsc::channel(4);
        let dir = tempfile::tempdir().unwrap();
        let mut ui = capture_ui(&dir);
        frame(&mut ui, 110, 32);
        ui.event(&Event::Capture(CaptureState::Watching { window: "Zoom Meeting".into() }), Instant::now());
        assert_eq!(ui.key(ctrl('s'), Instant::now()), Act::CaptureNow);
        ui.send(Act::CaptureNow, &tx, &replies);
        // core answers the oneshot on its own time; the reactor applies what comes back
        let Command::CaptureNow(answer) = rx.recv().await.unwrap() else { panic!("the one command: {:?}", sent(&mut rx)) };
        assert!(rx.try_recv().is_err(), "one key, one command");
        answer.send(Ok(())).unwrap();
        let replied = tokio::time::timeout(Duration::from_secs(2), reply_rx.recv()).await.unwrap().unwrap();
        assert_eq!(ui.capture_reply(replied), Act::Nothing, "a successful capture-now says nothing");
        assert!(ui.view.slides.is_empty(), "no slide is fabricated from the reply");
        assert_eq!(ui.notice, None);
        // the slide itself arrives as core's own event, and lands
        ui.event(&Event::Slide { index: 4, file: "slides/slide_04.png".into(), auto: false, uncertain: false, shown_at: Local::now() }, Instant::now());
        assert_eq!(ui.view.slides.len(), 1);
        assert_eq!(ui.view.slides[0].index, 4, "the canonical slide, not a fabricated one");
        // a failed capture-now is a notice; a second press is a second command, never a retry
        assert_eq!(ui.key(ctrl('s'), Instant::now()), Act::CaptureNow);
        ui.send(Act::CaptureNow, &tx, &replies);
        let Command::CaptureNow(answer) = rx.recv().await.unwrap() else { panic!() };
        assert!(rx.try_recv().is_err(), "one more key, one more command");
        answer.send(Err("Zoom Meeting could not be captured: the capture was blank".into())).unwrap();
        let replied = tokio::time::timeout(Duration::from_secs(2), reply_rx.recv()).await.unwrap().unwrap();
        assert_eq!(ui.capture_reply(replied), Act::Redraw);
        assert_eq!(ui.notice.as_deref(), Some("The capture did not happen: Zoom Meeting could not be captured: the capture was blank"));
    }

    /// Ctrl-S on the one safe offering: the saved-size selection is prepared, kept for the course
    /// (another course untouched), and exactly one `Bind` follows — with the saved region, parts
    /// left out and remembered sizes intact.
    #[tokio::test]
    async fn ctrl_s_watch_it_saves_then_binds_once() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let (replies, mut reply_rx) = tokio::sync::mpsc::channel(4);
        let dir = tempfile::tempdir().unwrap();
        // another course's selection, which the save must leave exactly as it is
        let other = Selection { descriptor: Descriptor { bundle_id: None, app: "TextEdit".into(), title: "Deck".into(), width: 1000, height: 700 }, region: Region::WHOLE, leave_out: vec![], sizes: vec![] };
        let mut all = Selections::default();
        all.set("Statistics", other.clone());
        all.save(&dir.path().join("capture.json")).unwrap();
        let mut ui = capture_ui(&dir);
        frame(&mut ui, 110, 32);
        ui.event(&asking(vec![candidate(42, 1280, 720)]), Instant::now());
        let Act::CaptureWatch { window, selection } = ui.key(ctrl('s'), Instant::now()) else { panic!("{:?}", ui.notice) };
        assert_eq!(window, 42);
        assert_eq!(selection.region, Region { x: 0.05, y: 0.12, w: 0.9, h: 0.7 }, "the saved region at the offered size");
        assert_eq!(selection.leave_out, saved_selection().leave_out, "the parts left out are kept");
        assert_eq!(selection.sizes, vec![SizedRegion { width: 1600, height: 900, region: Region { x: 0.1, y: 0.1, w: 0.8, h: 0.8 } }], "the size it was at is remembered; the offered one is now live");
        ui.send(Act::CaptureWatch { window, selection }, &tx, &replies);
        // the save runs off the reactor; the Bind follows it
        let bound = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Ok(c) = rx.try_recv() {
                    return c;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await;
        let bound = bound.expect("the Bind went out");
        assert!(matches!(bound, Command::Bind { window: 42, .. }), "{bound:?}");
        assert!(rx.try_recv().is_err(), "one key, one Bind");
        let answer = tokio::time::timeout(Duration::from_secs(2), reply_rx.recv()).await.unwrap().unwrap();
        assert_eq!(ui.capture_reply(answer), Act::Nothing);
        // the course's selection is on disk, the other course untouched
        let after = Selections::load(&dir.path().join("capture.json")).unwrap();
        let kept = after.get("Machine Learning").unwrap();
        assert_eq!((kept.descriptor.width, kept.descriptor.height, kept.region), (1280, 720, Region { x: 0.05, y: 0.12, w: 0.9, h: 0.7 }));
        assert_eq!(after.get("Statistics"), Some(&other));
        // a relocation updates the TUI's own saved selection, without rereading the file
        let moved = saved_selection().with_size(1920, 1200, Region { x: 0.02, y: 0.02, w: 0.96, h: 0.96 });
        ui.event(&Event::CaptureMoved { selection: moved.clone(), note: "found again".into() }, Instant::now());
        assert_eq!(ui.capture.as_ref().unwrap().saved, Some(moved));
    }

    /// Every state that cannot act says why and sends nothing: unbound, paused, denied, failing,
    /// asking with none, two or an unknown size offered, a window that is not the saved one,
    /// nothing watched at all — and stopping, where core has already dropped capture.
    #[test]
    fn unsafe_ctrl_s_sends_nothing_and_says_why() {
        let (_tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let dir = tempfile::tempdir().unwrap();
        let mut ui = capture_ui(&dir);
        frame(&mut ui, 110, 32);
        let notice = |ui: &mut Ui, e: &Event| {
            ui.event(e, Instant::now());
            let act = ui.key(ctrl('s'), Instant::now());
            assert_eq!(act, Act::Redraw, "{e:?}");
            ui.notice.clone()
        };
        assert_eq!(notice(&mut ui, &Event::Capture(CaptureState::Unbound)).as_deref(), Some("No window is chosen for this course yet: choose it once in the LectureLive app. Screenshots (⌘⇧4) still become slides."));
        assert_eq!(notice(&mut ui, &Event::Capture(CaptureState::Paused { window: "Zoom Meeting".into(), reason: "minimised".into() })).as_deref(), Some("Capture is paused; it comes back by itself when the window is back."));
        assert_eq!(notice(&mut ui, &Event::Capture(CaptureState::Denied)).as_deref(), Some("Screen Recording is off for this terminal; the Slides pane has the steps to turn it on."));
        assert_eq!(notice(&mut ui, &Event::Capture(CaptureState::Failing { window: "Zoom Meeting".into(), reason: "blank".into() })).as_deref(), Some("Capture is failing; the Slides pane says why."));
        assert_eq!(notice(&mut ui, &asking(vec![])).as_deref(), Some("No window to watch is being offered; the Slides pane says what capture is waiting for."));
        assert_eq!(notice(&mut ui, &asking(vec![candidate(42, 1280, 720), candidate(43, 1280, 720)])).as_deref(), Some("More than one window is offered; choose between them in the LectureLive app."));
        assert_eq!(notice(&mut ui, &asking(vec![candidate(42, 999, 600)])).as_deref(), Some("That window size has no saved region; choose where its slide is in the LectureLive app."));
        let chrome = WindowInfo { id: 7, app: "Google Chrome".into(), bundle_id: Some("com.google.Chrome".into()), title: "Zoom Meeting".into(), width: 1280, height: 720, on_screen: true };
        assert_eq!(notice(&mut ui, &asking(vec![chrome])).as_deref(), Some("The window offered is not the one saved; choose in the LectureLive app."));
        // behind an overlay, Ctrl-S changes nothing
        ui.key(ctrl('h'), Instant::now());
        assert_eq!(ui.key(ctrl('s'), Instant::now()), Act::Nothing);
        ui.key(key(KeyCode::Esc), Instant::now());
        // stopping: core has dropped capture; no command, and the notice says so
        ui.event(&Event::Capture(CaptureState::Watching { window: "Zoom Meeting".into() }), Instant::now());
        ui.key(ctrl('c'), Instant::now());
        assert_ne!(ui.view.phase, crate::stop::Stage::Listening);
        assert_eq!(ui.key(ctrl('s'), Instant::now()), Act::Redraw);
        assert_eq!(ui.notice.as_deref(), Some("Nothing is being watched yet."), "stopping refuses like an unwatched capture");
        assert!(sent(&mut rx).is_empty(), "not one capture command in any of these states");
    }

    /// A watch-it whose save fails sends no `Bind` and surfaces the failure.
    #[tokio::test]
    async fn a_failed_watch_save_binds_nothing() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let (replies, mut reply_rx) = tokio::sync::mpsc::channel(4);
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("capture.json"), "{ not json").unwrap(); // unreadable: the save fails
        let mut ui = capture_ui(&dir);
        frame(&mut ui, 110, 32);
        ui.event(&asking(vec![candidate(42, 1280, 720)]), Instant::now());
        let act = ui.key(ctrl('s'), Instant::now());
        assert!(matches!(act, Act::CaptureWatch { .. }));
        ui.send(act, &tx, &replies);
        let answer = tokio::time::timeout(Duration::from_secs(2), reply_rx.recv()).await.unwrap().unwrap();
        match answer {
            CaptureReply::WatchFailed(_) => {}
            other => panic!("{other:?}"),
        }
        assert!(sent(&mut rx).is_empty(), "no Bind without a save");
        assert_eq!(ui.capture_reply(CaptureReply::WatchFailed(anyhow::anyhow!("read capture.json: not json"))), Act::Redraw);
        assert!(ui.notice.as_deref().unwrap().contains("The window was not watched: saving its region failed"), "{:?}", ui.notice);
        // the session's own asking state is untouched: the worker is still offering the window
        assert!(matches!(ui.view.capture, Some(CaptureState::Asking { .. })));
    }

    /// A paste past the limit inserts nothing and says so; typing at the limit says so too.
    #[test]
    fn refused_paste_and_full_hint_say_why() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut ui = drawn_ui();
        type_in(&mut ui, "kept", Instant::now(), &tx);
        assert_eq!(ui.paste(&"x".repeat(input::HINT_CAP)), Act::Redraw);
        assert_eq!((ui.hint.value(), ui.notice.as_deref()), ("kept", Some(PASTE_TOO_LONG)));
        ui.paste(&"y".repeat(input::HINT_CAP - 4));
        press(&mut ui, key(KeyCode::Char('z')), Instant::now(), &tx);
        assert_eq!(ui.notice.as_deref(), Some(HINT_FULL));
    }

    /// A command the lecture can no longer take changes nothing: no queue entry, the hint kept.
    #[test]
    fn a_closed_lecture_takes_nothing() {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        drop(rx);
        let mut ui = drawn_ui();
        type_in(&mut ui, "late", Instant::now(), &tx);
        press(&mut ui, key(KeyCode::Enter), Instant::now(), &tx);
        assert_eq!((ui.hint.value(), ui.view.work.mine(), ui.notice.as_deref()), ("late", false, Some(ENDED)));
    }

    /// Plan Task 10: the lanes follow what the TUI actually submits, and typed events alone.
    #[test]
    fn submissions_drive_the_lanes() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let (mut ui, t0) = (drawn_ui(), Instant::now());
        press(&mut ui, key(KeyCode::Enter), t0, &tx);
        assert_eq!(ui.view.work.lane(), state::Lane::Snapshot);
        type_in(&mut ui, "polish", t0, &tx);
        press(&mut ui, key(KeyCode::Enter), t0 + ms(400), &tx);
        ui.event(&Event::Busy("polishing 900 words".into()), t0);
        assert_eq!((ui.view.work.lane(), ui.view.work.queued()), (state::Lane::Snapshot, 1), "busy text moves nothing");
        ui.event(&Event::NothingNew, t0);
        assert_eq!(ui.view.work.lane(), state::Lane::PolishSnapshotFirst);
        ui.event(&Event::Committed { words: 1, slides: 0, block: "\n<!-- 10:00:00 -->\n## x\n".into(), usd: 0.0, confirmed: true, removed: 0, missing: 0, revision: 1 }, t0);
        assert_eq!(ui.view.work.lane(), state::Lane::Polishing);
        ui.event(&Event::Polished { backup: "b.md".into(), usd: 0.0, revision: 2 }, t0);
        assert_eq!((ui.view.work.lane(), ui.view.work.page()), (state::Lane::Idle, true));
        let rows = frame(&mut ui, 140, 40);
        assert!(rows[3].contains("study page: typesetting"), "{:?}", rows[3]);
    }
}
