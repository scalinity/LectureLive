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
use futures_util::{Stream, StreamExt};
use lecturelive_core::session::coordinator::{Notification, StopReport};
use lecturelive_core::session::files::LectureFiles;
use lecturelive_core::session::lecture::{Command, Event, Op};
use lecturelive_core::session::spend::Spend;
use ratatui::crossterm::event::{Event as TermEvent, EventStream, KeyEvent, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::Frame;
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

/// What the reactor draws on (plan Task 13's testability seam, and nothing more): one frame at a
/// time, and Ctrl-L's full repaint. Production's is [`Screen`], through the lease's own
/// `terminal::draw`/`terminal::clear` exactly as before; the perf and slow-terminal tests bring
/// a `Terminal<TestBackend>` of their own, which can time a draw or hold one inside its flush.
/// A private trait with static dispatch only — production monomorphizes to today's calls.
trait Surface {
    fn draw<F>(&mut self, render: F) -> io::Result<()>
    where
        F: FnOnce(&mut Frame);
    fn clear(&mut self) -> io::Result<()>;
}

impl Surface for Screen {
    fn draw<F>(&mut self, render: F) -> io::Result<()>
    where
        F: FnOnce(&mut Frame),
    {
        terminal::draw(self, render)
    }

    fn clear(&mut self) -> io::Result<()> {
        terminal::clear(self)
    }
}

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
/// watch-it save's outcome. `Watched` carries the selection because it is now genuinely on disk —
/// the reactor's saved truth moves to it without rereading the file — and says whether the `Bind`
/// went out; a worker `CaptureState::Watching` event remains the only authority for live
/// watching either way. Nothing here is a slide; slides are core's own events.
#[derive(Debug)]
enum CaptureReply {
    Now(Result<(), String>),
    /// The watch-it save failed: nothing was bound, nothing was persisted.
    WatchFailed(anyhow::Error),
    /// The watch-it selection is persisted. `bound` is false when the `Bind` could not be sent —
    /// the lecture had ended — so the selection stays saved for the next session without the
    /// current worker ever being claimed to watch it.
    Watched { selection: Selection, bound: bool },
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
    /// must know of. The functional capture state is taken first, exactly as core emitted it and
    /// before the event is cleaned into the view's presentation copy (plan Task 11): a candidate's
    /// id, bundle id, app, title and size are binding identity, and display-cleaned text must never
    /// become it. The reactor feeds [`Self::forwarded`]; the tests drive this seam.
    fn event(&mut self, e: &Event, now: std::time::Instant) -> (Option<Step>, bool) {
        self.events += 1;
        if let Event::Capture(s) = e {
            if let Some(ctx) = &mut self.capture {
                ctx.current = Some(s.clone()); // raw, functional, never rendered
            }
        }
        let effect = self.view.reduce(e, Local::now());
        let step = matches!(e, Event::Session(Notification::SourceEnded)).then(|| self.stop(Origin::SourceEnded, now));
        (step, effect.hydrate)
    }

    /// One adapted event as the reactor receives it: a relocation moves the saved selection only
    /// when the save genuinely happened — the outcome rides with the event, so nothing is inferred
    /// from wording — and a relocation that could not be kept is said as the failure it is while
    /// the session's own found-again truth stays in the ring.
    fn forwarded(&mut self, f: &capture::Forwarded, now: std::time::Instant) -> (Option<Step>, bool) {
        match &f.capture_persistence {
            capture::CapturePersistence::None => {}
            capture::CapturePersistence::MovedSaved(selection) => {
                if let Some(ctx) = &mut self.capture {
                    ctx.saved = Some(selection.clone()); // this exact selection is on disk
                }
            }
            // a relocation that could not be kept: `saved` must not move (said below)
            capture::CapturePersistence::MovedSaveFailed { .. } => {}
        }
        let (step, hydrate) = self.event(&f.event, now);
        if let capture::CapturePersistence::MovedSaveFailed { error } = &f.capture_persistence {
            self.view.relocation_unsaved(error, Local::now());
        }
        (step, hydrate)
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
    /// capture at the first stop, so no manual capture can happen then either. The state it
    /// reads is the functional one, exactly as core emitted it; the view's cleaned copy is for
    /// drawing only.
    fn capture_action(&self) -> capture::Action {
        if self.view.phase != Stage::Listening {
            return capture::Action::Refused(capture::Refusal::None);
        }
        match &self.capture {
            Some(ctx) => capture::action(ctx.current.as_ref(), ctx.saved.as_ref()),
            None => capture::action(None, None),
        }
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
    /// A watch-it that saved moves the saved truth to exactly what is on disk; one whose `Bind`
    /// could not be sent says the lecture has ended without ever claiming the worker watches it.
    fn capture_reply(&mut self, reply: CaptureReply) -> Act {
        match reply {
            CaptureReply::Now(Ok(())) => Act::Nothing,
            CaptureReply::Now(Err(m)) => self.say(format!("The capture did not happen: {}", plain::sentence(&plain::clean(&m)))),
            CaptureReply::WatchFailed(e) => self.say(format!("The window was not watched: saving its region failed ({}).", plain::sentence(&plain::clean(&format!("{e:#}"))))),
            CaptureReply::Watched { selection, bound } => {
                if let Some(ctx) = &mut self.capture {
                    ctx.saved = Some(selection); // persisted is persisted, bound or not
                }
                if bound {
                    Act::Nothing // watching arrives as the worker's own event
                } else {
                    self.say(ENDED)
                }
            }
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
                            let path = ctx.path.clone();
                            let course = ctx.course.clone();
                            tokio::task::spawn_blocking(move || capture::keep_selection(&path, &course, &sel)).await
                        }
                        None => Ok(Ok(())),
                    };
                    match saved.unwrap_or_else(|e| Err(anyhow::anyhow!("{e}"))) {
                        Ok(()) => {
                            // persisted is persisted: the saved truth moves even if the Bind below
                            // cannot be sent, and `bound` says which it was — never Watched blindly
                            let bound = commands.send(Command::Bind { window, selection: selection.clone() }).is_ok();
                            let _ = replies.send(CaptureReply::Watched { selection, bound }).await;
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
pub(crate) async fn run(session: Session, engine: impl Future<Output = Result<StopReport>> + Send + 'static, commands: UnboundedSender<Command>, events: UnboundedReceiver<capture::Forwarded>, secs: Option<u64>) -> Result<StopReport> {
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
    let (exit, _) = react(Io { screen, keys, lecture, events, commands, interrupt, terminate, hangup, files: session.files, hydrate_tx, hydrate_rx, spend_rx, capture_tx, capture_rx }, view, session.capture, started, secs).await;
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

/// The reactor's inputs and I/O, generic only where Task 13's tests need it (the keyboard stream
/// and the drawing surface); everything else is exactly production's. `K` is the event stream
/// crossterm reads in production and a channel the tests feed; `S` is [`Screen`] in production
/// and the tests' own backend.
struct Io<K, S> {
    screen: S,
    keys: K,
    lecture: tokio::task::JoinHandle<Result<StopReport>>,
    /// The lecture's events as the capture adapter handed them on: each with whether any
    /// relocation it carried was actually persisted.
    events: UnboundedReceiver<capture::Forwarded>,
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
fn want_hydration<K, S>(io: &Io<K, S>, hydrating: &mut bool, again: &mut bool) {
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
/// is the fence after which it never draws again. The final [`Ui`] comes back with the exit — the
/// seam Task 13's tests inspect; production drops it.
async fn react<K, S>(mut io: Io<K, S>, view: View, capture: Option<capture::Context>, started: Instant, secs: Option<u64>) -> (Exit, Ui)
where
    K: Stream<Item = io::Result<TermEvent>> + Unpin,
    S: Surface,
{
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
            _ = io.terminate.recv() => return (Exit::Terminated, ui),
            _ = io.hangup.recv() => return (Exit::HungUp, ui),
            // `kill -INT`: in raw mode the keyboard's Ctrl-C arrives as a key instead.
            _ = io.interrupt.recv() => match ui.stop(Origin::Signal, std::time::Instant::now()) {
                Step::Quit => return (Exit::Quit, ui),
                step => { send_stops(&io.commands, step); dirty = true; }
            },
            input = io.keys.next() => match input {
                Some(Ok(TermEvent::Key(key))) => match ui.key(key, std::time::Instant::now()) {
                    Act::Stop(Step::Quit) => return (Exit::Quit, ui),
                    Act::Stop(step) => { send_stops(&io.commands, step); dirty = true; }
                    Act::Clear => {
                        if let Err(e) = io.screen.clear() {
                            return (Exit::DrawFailed(e), ui);
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
                Some(Err(e)) => return (Exit::Lost(e), ui),
                None => return (Exit::Lost(io::Error::other("the keyboard stream ended")), ui),
            },
            ended = &mut io.lecture => {
                while let Ok(f) = io.events.try_recv() {
                    let _ = ui.forwarded(&f, std::time::Instant::now());
                }
                return (Exit::Ended(ended), ui);
            }
            Some(f) = io.events.recv() => {
                let mut want = false;
                let mut next = Some(f);
                let mut taken = 0usize;
                // Up to BATCH events on this wake, and never one more taken than is processed:
                // an event taken past the batch's end would leave the channel with it — dropped —
                // and durable events are never dropped (plan §C 6). Task 13's slow-terminal test
                // proved the loss (one event per full batch) and pins it.
                while taken < BATCH {
                    let Some(f) = next.take() else { break };
                    taken += 1;
                    let (step, hydrate) = ui.forwarded(&f, std::time::Instant::now());
                    if let Some(step) = step {
                        send_stops(&io.commands, step);
                    }
                    want |= hydrate;
                    if taken < BATCH {
                        next = io.events.try_recv().ok();
                    }
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
                // The spend is a 1 Hz quantity shown beside the 1 Hz clock: it updates the
                // projection without demanding its own frame, because the clock's next tick
                // draws it within the same one-second freshness budget (and under any load the
                // next dirty draw shows it at once). Two independent 1 Hz wakeups otherwise drew
                // twice a second in a quiet session (Task 13's measurement).
                ui.view.spend = Some(usd);
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
            // The cap is measured draw-start to draw-start, so a frame's own duration never
            // stretches the period past FRAME (and the worst key-to-draw past 50 ms): starts are
            // ≥ 50 ms apart, which is exactly ≤ 20 draws in any half-open second (Task 13).
            let started_draw = Instant::now();
            if let Err(e) = io.screen.draw(|f| drawn = view::render(f, &ui.view, &chrome)) {
                return (Exit::DrawFailed(e), ui);
            }
            ui.drawn = drawn;
            (dirty, drawn_at) = (false, started_draw);
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
        ui.capture = Some(crate::capture::Context { path: dir.path().join("capture.json"), course: identity().course, saved: Some(saved_selection()), current: None });
        ui
    }

    /// A relocation as the adapter hands it on: saved for the course, or not.
    fn moved(selection: Selection, saved: bool) -> capture::Forwarded {
        let persistence = if saved { capture::CapturePersistence::MovedSaved(selection.clone()) } else { capture::CapturePersistence::MovedSaveFailed { error: "read capture.json: not json".into() } };
        capture::Forwarded { event: Event::CaptureMoved { selection, note: "the slide moved with the window; watching it at the new size".into() }, capture_persistence: persistence }
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
    }

    /// The watch-it that saved also moves the reactor's saved truth to exactly what is on disk
    /// (no reread), and the next functional decision uses it.
    #[tokio::test]
    async fn watch_it_success_updates_saved_context() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let (replies, mut reply_rx) = tokio::sync::mpsc::channel(4);
        let dir = tempfile::tempdir().unwrap();
        let mut ui = capture_ui(&dir);
        frame(&mut ui, 110, 32);
        ui.event(&asking(vec![candidate(42, 1280, 720)]), Instant::now());
        let act = ui.key(ctrl('s'), Instant::now());
        let Act::CaptureWatch { selection, .. } = &act else { panic!("{act:?}") };
        let bound = selection.clone();
        ui.send(act, &tx, &replies);
        let sent = tokio::time::timeout(Duration::from_secs(2), rx.recv()).await.unwrap().unwrap();
        assert!(matches!(sent, Command::Bind { window: 42, .. }), "{sent:?}");
        let replied = tokio::time::timeout(Duration::from_secs(2), reply_rx.recv()).await.unwrap().unwrap();
        assert_eq!(ui.capture_reply(replied), Act::Nothing);
        assert_eq!(ui.capture.as_ref().unwrap().saved, Some(bound.clone()), "the saved truth is exactly what was persisted");
        assert_eq!(Selections::load(&dir.path().join("capture.json")).unwrap().get("Machine Learning"), Some(&bound), "and exactly what is on disk");
        // the next functional decision reads that in-memory truth, not the file: remove the file,
        // and a safe offering at the remembered size still resolves
        std::fs::remove_file(dir.path().join("capture.json")).unwrap();
        ui.event(&asking(vec![candidate(42, 1600, 900)]), Instant::now());
        let watch = ui.capture_action();
        let capture::Action::Watch { selection, .. } = watch else { panic!("{watch:?}") };
        assert_eq!(selection.descriptor.title, "Zoom Meeting");
        assert!(selection.sizes.iter().any(|s| s.width == 1280 && s.height == 720), "the bound selection's remembered size, not the pre-bind one");
    }

    /// A Bind that could not be sent (the lecture had ended) is reported truthfully: the
    /// selection is still persisted and becomes the saved truth, but the worker is never claimed
    /// to be watching it — the notice is the normal ended one.
    #[tokio::test]
    async fn a_watch_it_with_a_dead_command_channel_is_truthful() {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        drop(rx); // the lecture is gone
        let (replies, mut reply_rx) = tokio::sync::mpsc::channel(4);
        let dir = tempfile::tempdir().unwrap();
        let mut ui = capture_ui(&dir);
        frame(&mut ui, 110, 32);
        ui.event(&asking(vec![candidate(42, 1280, 720)]), Instant::now());
        let act = ui.key(ctrl('s'), Instant::now());
        let Act::CaptureWatch { selection, .. } = &act else { panic!("{act:?}") };
        let bound = selection.clone();
        ui.send(act, &tx, &replies);
        let replied = tokio::time::timeout(Duration::from_secs(2), reply_rx.recv()).await.unwrap().unwrap();
        match &replied {
            CaptureReply::Watched { selection, bound: false } => assert_eq!(selection, &bound),
            other => panic!("{other:?}"),
        }
        assert_eq!(ui.capture_reply(replied), Act::Redraw);
        assert_eq!(ui.notice.as_deref(), Some(ENDED));
        assert_eq!(ui.capture.as_ref().unwrap().saved, Some(bound.clone()), "persisted is persisted, bind or not");
        assert_eq!(Selections::load(&dir.path().join("capture.json")).unwrap().get("Machine Learning"), Some(&bound));
        assert!(matches!(ui.view.capture, Some(CaptureState::Asking { .. })), "no watching is claimed: the worker's own word has not arrived");
    }

    /// A relocation the adapter kept: by the time the frontend handles it, the selection is on
    /// disk (that is the adapter's guarantee, pinned in capture.rs), and the reactor's saved
    /// truth moves to it — no reread — while another course stays untouched.
    #[test]
    fn capture_moved_success_updates_saved_context() {
        let dir = tempfile::tempdir().unwrap();
        let moved_sel = saved_selection().with_size(1920, 1200, Region { x: 0.02, y: 0.03, w: 0.95, h: 0.9 });
        // the adapter has already saved B and left the other course alone
        let mut all = Selections::default();
        let statistics = Selection { descriptor: Descriptor { bundle_id: None, app: "TextEdit".into(), title: "Deck".into(), width: 1000, height: 700 }, region: Region::WHOLE, leave_out: vec![], sizes: vec![] };
        all.set("Statistics", statistics.clone());
        all.set("Machine Learning", moved_sel.clone());
        all.save(&dir.path().join("capture.json")).unwrap();
        let mut ui = capture_ui(&dir);
        ui.forwarded(&moved(moved_sel.clone(), true), Instant::now());
        assert_eq!(ui.capture.as_ref().unwrap().saved, Some(moved_sel.clone()), "the persisted selection, without rereading the file");
        let after = Selections::load(&dir.path().join("capture.json")).unwrap();
        assert_eq!(after.get("Machine Learning"), Some(&moved_sel));
        assert_eq!(after.get("Statistics"), Some(&statistics), "another course is never replaced");
        assert_eq!(ui.view.notice.as_ref().map(|n| n.label.as_str()), Some("found again"), "kept is said as kept");
    }

    /// A relocation whose preference could not be written: the worker's relocation is still true
    /// for the running session, but the saved truth does not move, the disk does not gain what
    /// was never written, and the visible notice is the failure — not a found-again that reads
    /// as kept.
    #[test]
    fn capture_moved_save_failure_does_not_update_saved_context() {
        let dir = tempfile::tempdir().unwrap();
        let mut all = Selections::default();
        let statistics = Selection { descriptor: Descriptor { bundle_id: None, app: "TextEdit".into(), title: "Deck".into(), width: 1000, height: 700 }, region: Region::WHOLE, leave_out: vec![], sizes: vec![] };
        all.set("Statistics", statistics.clone());
        all.set("Machine Learning", saved_selection());
        all.save(&dir.path().join("capture.json")).unwrap();
        let mut ui = capture_ui(&dir);
        ui.forwarded(&moved(saved_selection().with_size(999, 600, Region::WHOLE), false), Instant::now());
        // the saved truth is still the loaded selection A
        assert_eq!(ui.capture.as_ref().unwrap().saved, Some(saved_selection()), "a selection that was never persisted is not saved");
        let after = Selections::load(&dir.path().join("capture.json")).unwrap();
        assert_eq!(after.get("Machine Learning"), Some(&saved_selection()), "the disk does not contain B");
        assert_eq!(after.get("Statistics"), Some(&statistics));
        // the visible notice is the persistence failure, in both truths
        let n = ui.view.notice.as_ref().expect("a notice");
        assert_eq!((n.kind, n.label.as_str()), ("warn", "found again"));
        assert_eq!(n.detail, "Capture found the slide again, but the new region could not be saved: read capture.json: not json");
        // and the ring keeps both truths: the found-again record and the failure beside it
        let details: Vec<&str> = ui.view.activity.records().iter().map(|a| a.detail.as_str()).collect();
        assert!(details.iter().any(|d| d.starts_with("the slide moved")), "the session's found-again truth stays: {details:?}");
        assert!(details.iter().any(|d| d.starts_with("Capture found the slide again, but")), "{details:?}");
    }

    /// Ctrl-S's watch-it reads the raw functional identity, not the display copy: a candidate
    /// whose app and title contain what cleaning strips still binds against the same raw saved
    /// identity, while the view holds only cleaned words for drawing. One Bind follows, off the
    /// reactor's thread, once the save has happened.
    #[tokio::test]
    async fn raw_identity_survives_sanitization_for_watch_it() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let (replies, _) = tokio::sync::mpsc::channel(1);
        let dir = tempfile::tempdir().unwrap();
        let mut ui = capture_ui(&dir);
        frame(&mut ui, 110, 32);
        // one raw identity, hostile to displays, equal on both the saved and the offered side
        let hostile = "Zoom\x1b]0;owned\x07Meeting";
        let mut saved = saved_selection();
        saved.descriptor.bundle_id = None;
        saved.descriptor.app = hostile.into();
        saved.descriptor.title = hostile.into();
        ui.capture.as_mut().unwrap().saved = Some(saved.clone());
        let raw = WindowInfo { id: 42, app: hostile.into(), bundle_id: None, title: hostile.into(), width: 1280, height: 720, on_screen: true };
        ui.event(&Event::Capture(CaptureState::Asking { window: hostile.into(), reason: "it is not where it was".into(), candidates: vec![raw] }), Instant::now());
        // the view cleans for display; the functional state stays exactly as core emitted it
        let Some(CaptureState::Asking { candidates, .. }) = &ui.view.capture else { panic!("{:?}", ui.view.capture) };
        assert_eq!(candidates[0].title, "ZoomMeeting", "the presentation copy is cleaned");
        assert_eq!(candidates[0].title, plain::clean(hostile));
        let Some(CaptureState::Asking { candidates, .. }) = &ui.capture.as_ref().unwrap().current else { panic!("raw state") };
        assert_eq!(candidates[0].title, hostile, "the functional copy is raw");
        // and the decision binds: raw equals raw, whatever display would make of it
        let Act::CaptureWatch { window, selection } = ui.key(ctrl('s'), Instant::now()) else { panic!("{:?}", ui.notice) };
        assert_eq!(window, 42);
        assert_eq!(selection.descriptor.title, hostile, "the bind's identity is the raw one");
        ui.send(Act::CaptureWatch { window, selection }, &tx, &replies);
        let sent = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Ok(c) = rx.try_recv() {
                    return c;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the Bind went out");
        assert!(matches!(sent, Command::Bind { window: 42, .. }));
        assert!(rx.try_recv().is_err(), "one key, one Bind");
    }

    /// Two windows whose app/title clean to the same words are not the same window: the
    /// presentation may safely clean both, the functional decision refuses, and no Bind goes out.
    #[test]
    fn display_equivalent_but_different_windows_do_not_bind() {
        let (_tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Command>();
        let dir = tempfile::tempdir().unwrap();
        let mut ui = capture_ui(&dir);
        frame(&mut ui, 110, 32);
        for (a, b) in [
            ("Zoom\x1b]0;a\x07Meeting", "Zoom\x1b]0;b\x07Meeting"), // both clean to “ZoomMeeting”
            ("Zoom\rMeeting", "Zoom\nMeeting"),                     // both clean to “Zoom\nMeeting”
        ] {
            assert_eq!(plain::clean(a), plain::clean(b), "the pair really is display-equivalent");
            let mut saved = saved_selection();
            saved.descriptor.bundle_id = None;
            saved.descriptor.app = "zoom.us".into();
            saved.descriptor.title = a.into();
            ui.capture.as_mut().unwrap().saved = Some(saved.clone());
            let raw = WindowInfo { id: 9, app: "zoom.us".into(), bundle_id: None, title: b.into(), width: 1280, height: 720, on_screen: true };
            ui.event(&Event::Capture(CaptureState::Asking { window: a.into(), reason: "it is not where it was".into(), candidates: vec![raw] }), Instant::now());
            // both copies hold a title; they are merely cleaned differently
            let (Some(view), Some(raw_state)) = (&ui.view.capture, &ui.capture.as_ref().unwrap().current) else { panic!() };
            let (CaptureState::Asking { candidates: shown, .. }, CaptureState::Asking { candidates: functional, .. }) = (view, raw_state) else { panic!() };
            assert_eq!(plain::clean(&functional[0].title), shown[0].title, "the display copy is the cleaned functional one");
            // the functional decision does not treat them as the same saved window
            assert_eq!(ui.key(ctrl('s'), Instant::now()), Act::Redraw, "refused");
            assert_eq!(ui.notice.as_deref(), Some("The window offered is not the one saved; choose in the LectureLive app."));
        }
        assert!(rx.try_recv().is_err(), "no Bind was ever sent");
    }

    /// One physical Ctrl-S — a press, its repeats, its release — is one command: one CaptureNow
    /// while watching, one Bind on a safe offering, and nothing at all from the repeats.
    #[tokio::test]
    async fn held_ctrl_s_press_sequence_sends_one_command_each() {
        let press = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL);
        let mut repeat = press;
        repeat.kind = KeyEventKind::Repeat;
        let mut release = press;
        release.kind = KeyEventKind::Release;
        // watching: one CaptureNow
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let (replies, _) = tokio::sync::mpsc::channel(1);
        let dir = tempfile::tempdir().unwrap();
        let mut ui = capture_ui(&dir);
        frame(&mut ui, 110, 32);
        ui.event(&Event::Capture(CaptureState::Watching { window: "Zoom Meeting".into() }), Instant::now());
        assert_eq!(ui.key(press, Instant::now()), Act::CaptureNow);
        ui.send(Act::CaptureNow, &tx, &replies);
        for _ in 0..6 {
            assert_eq!(ui.key(repeat, Instant::now()), Act::Nothing, "a repeat is not a new press");
        }
        assert_eq!(ui.key(release, Instant::now()), Act::Nothing);
        assert!(matches!(rx.recv().await.unwrap(), Command::CaptureNow(_)));
        assert!(rx.try_recv().is_err(), "one press sequence, one capture");
        // a safe offering: one Bind, and the repeats never even reach the decision
        let mut ui = capture_ui(&dir);
        frame(&mut ui, 110, 32);
        ui.event(&asking(vec![candidate(42, 1280, 720)]), Instant::now());
        assert!(matches!(ui.key(press, Instant::now()), Act::CaptureWatch { .. }));
        assert_eq!(ui.key(repeat, Instant::now()), Act::Nothing);
        assert_eq!(ui.key(repeat, Instant::now()), Act::Nothing);
        let (replies, mut reply_rx) = tokio::sync::mpsc::channel(4);
        let act = ui.key(press, Instant::now()); // a second, real press: a second deliberate action
        assert!(matches!(act, Act::CaptureWatch { .. }));
        ui.send(act, &tx, &replies);
        let sent = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Ok(c) = rx.try_recv() {
                    return c;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the Bind went out");
        assert!(matches!(sent, Command::Bind { .. }));
        assert!(rx.try_recv().is_err(), "one press sequence, one Bind");
        let _ = reply_rx.recv().await; // the reply is drained elsewhere; nothing retries on its own
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

/// Task 13's harness and measurements (plan §J "Performance"): the reactor itself, driven through
/// the same seam over a synthetic keyboard stream and a test backend it draws on, so what is
/// measured is the loop production runs — its batches, its throttle, its 1 Hz wakeups — not a
/// model of it. The percentile method is nearest-rank everywhere. Only the structural proofs run
/// with the normal suite; every timing gate is `#[ignore]`d and named `perf` for the canonical
/// `cargo test -p lecturelive-cli perf -- --ignored --nocapture`.
#[cfg(test)]
mod task13 {
    use super::*;
    use crate::capture::{CapturePersistence, Forwarded};
    use crate::fixture;
    use crate::tui::hydrate::Hydration;
    use crate::tui::state::{Identity, SourceKind};
    use lecturelive_core::session::segments::{Segment, SegmentSource};
    use ratatui::backend::{Backend, ClearType, TestBackend};
    use ratatui::buffer::Cell;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::layout::{Position, Rect, Size};
    use ratatui::backend::WindowSize;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Condvar, Mutex};
    use tokio::sync::mpsc::UnboundedSender;
    use tokio::sync::oneshot;

    // ---- the p95 method (one helper, used by every gate) --------------------------------------

    /// Task 13's percentile method: nearest rank — sorted ascending, index `ceil(q·N) − 1`. No
    /// interpolation, no statistics dependency; the same helper serves every percentile below.
    fn percentile(mut samples: Vec<Duration>, q: f64) -> Duration {
        assert!(!samples.is_empty(), "a percentile of nothing");
        samples.sort();
        samples[((samples.len() as f64 * q).ceil() as usize).saturating_sub(1)]
    }

    /// Prints one run's N/p50/p95/max and returns the worst p95 of the runs — the gate, never the
    /// best. Nothing prints inside a timed loop; the summary comes after the measurement.
    fn summarize(metric: &str, runs: &[Vec<Duration>]) -> Duration {
        let ms = |d: Duration| d.as_secs_f64() * 1e3;
        let mut worst = Duration::ZERO;
        for (k, run) in runs.iter().enumerate() {
            let max = run.iter().copied().max().unwrap();
            println!("PERF {metric}_run{k}_n={} p50_ms={:.3} p95_ms={:.3} max_ms={:.3}", run.len(), ms(percentile(run.clone(), 0.50)), ms(percentile(run.clone(), 0.95)), ms(max));
            worst = worst.max(percentile(run.clone(), 0.95));
        }
        println!("PERF {metric}_p95_ms={:.3}", ms(worst));
        worst
    }
    /// Half-open one-second windows `[t0 + k·s, t0 + (k+1)·s)`: the most starts any window holds,
    /// and the mean rate over the span.
    fn per_second(starts: &[std::time::Instant], t0: std::time::Instant, span: Duration) -> (usize, f64) {
        let mut windows = vec![0usize; span.as_secs() as usize];
        for s in starts {
            let k = (*s - t0).as_secs() as usize;
            if let Some(w) = windows.get_mut(k) {
                *w += 1;
            }
        }
        (windows.iter().copied().max().unwrap_or(0), starts.len() as f64 / span.as_secs_f64())
    }

    /// Task 13 pins the frame cap structurally: the reactor never draws more often than every
    /// 50 ms, which is the ≤20 draws a second target.
    #[test]
    fn frame_cap_is_50_ms() {
        assert_eq!(FRAME, Duration::from_millis(50));
    }

    // ---- the surfaces --------------------------------------------------------------------------

    /// Draw starts and durations as the surface itself saw them: the probes Task 13 measures with.
    type Probe = Arc<Mutex<Vec<(std::time::Instant, Duration)>>>;

    /// The reactor's drawing surface for measurement: `Terminal<TestBackend>` behind the seam,
    /// timing each draw where it happens and never writing to a real terminal.
    struct ProbeSurface {
        terminal: ratatui::Terminal<TestBackend>,
        probe: Probe,
    }

    fn infallible(e: core::convert::Infallible) -> io::Error {
        match e {}
    }

    impl Surface for ProbeSurface {
        fn draw<F>(&mut self, render: F) -> io::Result<()>
        where
            F: FnOnce(&mut Frame),
        {
            let start = std::time::Instant::now();
            self.terminal.draw(render).map_err(infallible)?;
            let took = start.elapsed();
            self.probe.lock().unwrap().push((start, took));
            Ok(())
        }

        fn clear(&mut self) -> io::Result<()> {
            let size = self.terminal.size().map_err(infallible)?;
            self.terminal.resize(Rect::new(0, 0, size.width, size.height)).map_err(infallible)
        }
    }

    /// The gate a slow backend holds its draw in: entry is published, and the draw blocks **until
    /// the test releases it** — the five-second bound below is only a hang guard for a broken
    /// test, never the proof. Because the release is the test's own and happens only after every
    /// send has returned, the producer provably finished while the draw still held the reactor,
    /// and the test observes entry and completion from this state rather than from elapsed time:
    /// no sleep, and no wall-clock budget, is part of this proof.
    #[derive(Default)]
    struct FlushGate {
        entered: AtomicBool,
        done: AtomicBool,
        release: Mutex<bool>,
        wake: Condvar,
    }

    impl FlushGate {
        fn enter_and_block(&self) {
            self.entered.store(true, Ordering::Release);
            let mut release = self.release.lock().unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while !*release {
                let left = deadline.saturating_duration_since(std::time::Instant::now());
                if left.is_zero() {
                    break;
                }
                let (next, timed_out) = self.wake.wait_timeout(release, left).unwrap();
                release = next;
                if timed_out.timed_out() {
                    break;
                }
            }
            self.done.store(true, Ordering::Release);
        }

        fn release(&self) {
            *self.release.lock().unwrap() = true;
            self.wake.notify_all();
        }

        fn entered(&self) -> bool {
            self.entered.load(Ordering::Acquire)
        }

        fn blocked(&self) -> bool {
            self.entered() && !self.done.load(Ordering::Acquire)
        }
    }

    /// `TestBackend` behind a flush that blocks (plan Task 13): the reactor is held inside its
    /// frame exactly as a slow terminal holds it. Every method but `draw` is the inner backend's.
    struct SlowBackend {
        inner: TestBackend,
        gate: Arc<FlushGate>,
    }

    impl Backend for SlowBackend {
        type Error = io::Error;

        fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
        where
            I: Iterator<Item = (u16, u16, &'a Cell)>,
        {
            // a slow terminal's flush is a blocking wait inside the reactor's poll: announced to
            // the runtime (`block_in_place`), so the worker is replaced while the flush holds it —
            // the reactor is genuinely blocked, and the runtime around it stays alive
            tokio::task::block_in_place(|| self.gate.enter_and_block());
            self.inner.draw(content).map_err(infallible)
        }

        fn hide_cursor(&mut self) -> io::Result<()> {
            self.inner.hide_cursor().map_err(infallible)
        }

        fn show_cursor(&mut self) -> io::Result<()> {
            self.inner.show_cursor().map_err(infallible)
        }

        fn get_cursor_position(&mut self) -> io::Result<Position> {
            self.inner.get_cursor_position().map_err(infallible)
        }

        fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
            self.inner.set_cursor_position(position).map_err(infallible)
        }

        fn clear(&mut self) -> io::Result<()> {
            self.inner.clear().map_err(infallible)
        }

        fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
            self.inner.clear_region(clear_type).map_err(infallible)
        }

        fn size(&self) -> io::Result<Size> {
            self.inner.size().map_err(infallible)
        }

        fn window_size(&mut self) -> io::Result<WindowSize> {
            self.inner.window_size().map_err(infallible)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.inner.flush().map_err(infallible)
        }
    }

    // ---- a reactor on the test's own surface and keyboard stream --------------------------------

    type Keys = Pin<Box<dyn Stream<Item = io::Result<TermEvent>> + Send>>;

    /// The keyboard stream as a channel the test types into: the reactor reads it exactly as it
    /// reads crossterm's `EventStream`.
    fn keys_from_channel() -> (tokio::sync::mpsc::UnboundedSender<io::Result<TermEvent>>, Keys) {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        (tx, Box::pin(futures_util::stream::poll_fn(move |cx| rx.poll_recv(cx))))
    }

    fn identity() -> Identity {
        Identity { course: "Machine Learning".into(), lecture: "Week 03 — Optimisation".into(), input: "BlackHole 2ch".into(), kind: SourceKind::Loopback, notes_file: "lecture_notes_20260926.md".into(), transcript_file: "lecture_transcript_20260926.txt".into() }
    }

    /// The view the exact scripted payload leaves (Task 13 measures the fixture's own state, so
    /// the tests and the scenarios cannot drift): reduce the events, nothing more.
    fn view_of(events: &[Event]) -> View {
        let mut v = View::new(identity(), Hydration::empty(), Vec::new());
        for e in events {
            v.reduce(e, Local::now());
        }
        v
    }

    /// A reactor running on the test's runtime: the same loop, channels, deadlines and 1 Hz clock
    /// production has, over `screen` and a keyboard channel the test feeds. `engine_of` builds the
    /// lecture the reactor awaits — a script that ends on demand, or the fixture itself.
    struct Reactor {
        keys: tokio::sync::mpsc::UnboundedSender<io::Result<TermEvent>>,
        events: UnboundedSender<Forwarded>,
        commands: UnboundedSender<Command>,
        end: Option<oneshot::Sender<()>>,
        lecture: tokio::task::JoinHandle<(Exit, Ui)>,
        probe: Probe,
        _dir: tempfile::TempDir,
    }

    fn reactor_on<S, F>(screen: S, probe: Probe, view: View, spend: bool, engine_of: F) -> Reactor
    where
        S: Surface + Send + 'static,
        F: FnOnce(UnboundedReceiver<Command>, UnboundedSender<Forwarded>) -> (tokio::task::JoinHandle<Result<StopReport>>, Option<oneshot::Sender<()>>),
    {
        let (keys, key_stream) = keys_from_channel();
        let (events, events_rx) = tokio::sync::mpsc::unbounded_channel();
        let (commands, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
        let dir = tempfile::tempdir().unwrap();
        let files = LectureFiles::standard(dir.path(), chrono::NaiveDate::from_ymd_opt(2026, 9, 26).unwrap());
        std::fs::create_dir_all(files.state_dir()).unwrap();
        let (hydrate_tx, hydrate_rx) = tokio::sync::mpsc::channel(1);
        let (capture_tx, capture_rx) = tokio::sync::mpsc::channel(4);
        let (spend_tx, spend_rx) = tokio::sync::mpsc::channel::<f64>(1);
        if spend {
            // production's 1 Hz sample, wakeups and all: the ledger's own cost is a spawn_blocking
            // on another thread either way, so the reactor sees only the channel
            tokio::spawn(async move {
                let mut clock = tokio::time::interval(Duration::from_secs(1));
                clock.set_missed_tick_behavior(MissedTickBehavior::Skip);
                loop {
                    clock.tick().await;
                    if spend_tx.send(0.0).await.is_err() {
                        return;
                    }
                }
            });
        } else {
            drop(spend_tx);
        }
        let (lecture, end) = engine_of(cmd_rx, events.clone());
        let io = Io {
            screen,
            keys: key_stream,
            lecture,
            events: events_rx,
            commands: commands.clone(),
            interrupt: signal(SignalKind::interrupt()).unwrap(),
            terminate: signal(SignalKind::terminate()).unwrap(),
            hangup: signal(SignalKind::hangup()).unwrap(),
            files,
            hydrate_tx,
            hydrate_rx,
            spend_rx,
            capture_tx,
            capture_rx,
        };
        let lecture = tokio::spawn(react(io, view, None, Instant::now(), None));
        Reactor { keys, events, commands, end, lecture, probe, _dir: dir }
    }

    /// The measured screen (140 × 40, the wide shape) over a lecture that ends when the test says.
    fn reactor(width: u16, height: u16, view: View, spend: bool) -> Reactor {
        let probe: Probe = Arc::new(Mutex::new(Vec::new()));
        reactor_on(
            ProbeSurface { terminal: ratatui::Terminal::new(TestBackend::new(width, height)).unwrap(), probe: probe.clone() },
            probe,
            view,
            spend,
            |_commands, _events| {
                let (end_tx, end_rx) = oneshot::channel();
                let engine = tokio::spawn(async move {
                    let _ = end_rx.await;
                    Ok::<StopReport, anyhow::Error>(StopReport::default())
                });
                (engine, Some(end_tx))
            },
        )
    }

    /// Sets the slow-terminal proof's screen into a reactor of its own: see `slow_terminal_holds_nothing_up`.

    impl Reactor {
        async fn finish(mut self) -> (Exit, Ui) {
            if let Some(end) = self.end.take() {
                let _ = end.send(());
            }
            self.lecture.await.unwrap()
        }

        fn draws(&self) -> Vec<(std::time::Instant, Duration)> {
            self.probe.lock().unwrap().clone()
        }

        /// Waits for the next draw-start after `before` entries existed, or panics on the deadline.
        /// The poll is fine-grained on purpose: on a current-thread runtime the test's own sleep
        /// cadence is what advances the timer wheel between wakes, so a coarse poll would make
        /// the reactor's 50 ms deadline itself fire late — a harness artifact, not the reactor's.
        async fn next_draw(&self, before: usize, within: Duration) -> std::time::Instant {
            let deadline = std::time::Instant::now() + within;
            loop {
                let draws = self.draws();
                if draws.len() > before {
                    return draws[draws.len() - 1].0;
                }
                assert!(std::time::Instant::now() < deadline, "no draw came within {within:?}");
                tokio::time::sleep(Duration::from_micros(200)).await;
            }
        }

        /// The first draw that both began after `before` entries existed **and** started no
        /// earlier than `floor`: a key can never be measured against a frame that was already
        /// painting when the key was sent, so a latency is never measured to a negative or to a
        /// frame the key did not cause.
        async fn next_draw_after(&self, before: usize, floor: std::time::Instant, within: Duration) -> std::time::Instant {
            let deadline = std::time::Instant::now() + within;
            loop {
                let draws = self.draws();
                if let Some(at) = draws[before..].iter().map(|(at, _)| *at).find(|at| *at >= floor) {
                    return at;
                }
                assert!(std::time::Instant::now() < deadline, "no draw came within {within:?}");
                tokio::time::sleep(Duration::from_micros(200)).await;
            }
        }
    }

    // ---- the slow terminal (a correctness proof, not a timing benchmark) ------------------------

    /// Plan Task 13: a terminal whose draw blocks holds nothing up. The test backend's `draw`
    /// waits on [`FlushGate`] and is released only by the test itself, so the reactor is held
    /// inside its frame for exactly as long as the test needs — not for any fixed interval.
    /// While the reactor is blocked there, exactly 3,000 events — a four-event cycle 750 times:
    /// Segment, Preview, Committed, Level — all enter the unbounded event channel production
    /// uses, and every send returns before the test releases the draw: that ordering, observed
    /// through the gate's own state, is the proof, never a timing budget or a sleep. Afterwards
    /// the view holds all of it: every segment once and in order, every commit revision, the
    /// preview the last commit ended, and the last level's value.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn slow_terminal_holds_nothing_up() {
        let gate = Arc::new(FlushGate::default());
        let screen = ProbelessBlocking { terminal: ratatui::Terminal::new(SlowBackend { inner: TestBackend::new(140, 40), gate: gate.clone() }).unwrap() };
        let r = reactor_on(screen, Arc::new(Mutex::new(Vec::new())), View::new(identity(), Hydration::empty(), Vec::new()), false, |_commands, _events| {
            let (end_tx, end_rx) = oneshot::channel();
            let engine = tokio::spawn(async move {
                let _ = end_rx.await;
                Ok::<StopReport, anyhow::Error>(StopReport::default())
            });
            (engine, Some(end_tx))
        });
        // the reactor is blocked inside its first frame's flush — known, not assumed
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !gate.entered() {
            assert!(std::time::Instant::now() < deadline, "the reactor never drew");
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        // the four-event cycle, 750 times, all through the production channel; the last level is
        // uniquely identifiable and the last commit ends the preview it follows
        let at = Local::now();
        let seg = |k: u64| Event::Session(Notification::Segment(Segment { id: k, recording_id: Default::default(), start_sample: k * 16_000, end_sample: (k + 1) * 16_000, said_at: at, start: at, end: at, text: format!("Segment {k} of the slow-terminal proof."), words: Vec::new(), source: SegmentSource::Live }));
        let commit = |k: u64| Event::Committed { words: 8, slides: 0, block: format!("\n<!-- 10:00:00 -->\n## Block {k}\n\n- committed revision {k} landed.\n"), usd: 0.0, confirmed: true, removed: 0, missing: 0, revision: k };
        for k in 0..750u64 {
            let rev = k + 1;
            let level = if k == 749 { -41.5 } else { 0.001 * k as f32 };
            for e in [seg(k), Event::Preview(format!("slow-terminal delta {k} of the proof, ")), commit(rev), Event::Session(Notification::Level(level))] {
                r.events.send(Forwarded { event: e, capture_persistence: CapturePersistence::None }).unwrap();
            }
        }
        // the proof itself: all 3,000 sends returned while the flush still held the reactor
        assert!(gate.blocked(), "every send completed while the terminal flush held the reactor — no sender ever awaited the frontend");
        gate.release();
        // the pile drains, a frame draws it, and then the lecture ends on the test's word
        tokio::time::sleep(Duration::from_millis(400)).await;
        let (exit, ui) = r.finish().await;
        assert!(matches!(exit, Exit::Ended(_)), "the lecture ended as the test ended it");
        assert_eq!(ui.view.closed.iter().map(|l| l.id).collect::<Vec<_>>(), (0..750).collect::<Vec<_>>(), "every segment, once, in order");
        assert_eq!(ui.view.notes.revision, 750, "every commit revision 1..=750 landed");
        assert!(ui.view.notes.document.contains("## Block 1\n") && ui.view.notes.document.contains("## Block 750\n"), "the document holds the first and the last block");
        assert_eq!(ui.view.notes.preview, None, "the final commit ended the preview");
        assert_eq!(ui.view.level, Some(-41.5), "the meter shows the last level sent");
        assert_eq!(ui.events, 3_000, "no durable event was lost");
    }

    /// The slow-terminal proof's surface: `Terminal<SlowBackend>` behind the seam.
    struct ProbelessBlocking {
        terminal: ratatui::Terminal<SlowBackend>,
    }

    impl Surface for ProbelessBlocking {
        fn draw<F>(&mut self, render: F) -> io::Result<()>
        where
            F: FnOnce(&mut Frame),
        {
            self.terminal.draw(render).map(|_| ())
        }

        fn clear(&mut self) -> io::Result<()> {
            let size = self.terminal.size()?;
            self.terminal.resize(Rect::new(0, 0, size.width, size.height))
        }
    }

    // ---- frame-work p95, the two-hour fixture ----------------------------------------------------

    /// One frame's work as the reactor does it on a wake: the preview's throttled refresh, the
    /// chrome, the render, and Ratatui's own draw and buffer diff — never real terminal I/O.
    #[test]
    #[ignore = "perf: run with `cargo test -p lecturelive-cli perf -- --ignored --nocapture`"]
    fn perf_two_hour_frame_work_p95() {
        // the exact two-hour payload is built first; no timing below includes its generation
        let mut ui = Ui::new(view_of(&fixture::two_hour_events(0, 0)), view::Theme::new(lecturelive_core::session::spend::Paint { color: true, truecolor: true }, true), None);
        const WARMUP: usize = 20;
        const MEASURED: usize = 200;
        const RUNS: usize = 3;
        let runs: Vec<Vec<Duration>> = (0..RUNS)
            .map(|_| {
                let mut terminal = ratatui::Terminal::new(TestBackend::new(140, 40)).unwrap();
                let mut samples = Vec::with_capacity(MEASURED);
                for i in 0..WARMUP + MEASURED {
                    // the clock moves, so the frame is never an artificially static no-op
                    let t0 = std::time::Instant::now();
                    ui.preview.refresh(&ui.view.notes, std::time::Instant::now());
                    let chrome = ui.chrome(Duration::from_millis(250 * i as u64));
                    let mut drawn = ui.drawn;
                    terminal.draw(|f| drawn = view::render(f, &ui.view, &chrome)).unwrap();
                    ui.drawn = drawn;
                    if i >= WARMUP {
                        samples.push(t0.elapsed());
                    }
                }
                samples
            })
            .collect();
        let worst = summarize("two_hour_frame", &runs);
        assert!(worst <= Duration::from_millis(8), "two-hour frame work p95 {worst:?} exceeds the 8 ms budget");
        // the final frame is a real frame: the wide layout, both reading panes on screen
        assert!(!ui.drawn.small && ui.drawn.transcript.is_some() && ui.drawn.notes.is_some(), "the frame is semantically valid, not TooSmall");
    }

    // ---- frame-work p95, the burst through the real reactor --------------------------------------

    /// The burst's own active interval, as the fixture recorded it at the source: the instant
    /// immediately before its 5,000-delta clock starts, and the instant immediately after its last
    /// delta has been sent (plan Task 13's remediation). Read from the fixture's test-only
    /// observer — same thread, as the current-thread runtime runs the fixture and this test — so
    /// nothing here is a settling heuristic, a sleep, or a positional guess.
    async fn burst_interval(within: Duration) -> (std::time::Instant, std::time::Instant) {
        let deadline = std::time::Instant::now() + within;
        loop {
            let marks = fixture::BURST_INTERVAL.with(|m| m.borrow().clone());
            if let (Some((true, start)), Some((false, end))) = (marks.first(), marks.iter().find(|(began, _)| !*began)) {
                let (start, end) = (*start, *end);
                assert!(end > start, "the burst finished before it started: {start:?} … {end:?}");
                return (start, end);
            }
            assert!(std::time::Instant::now() < deadline, "the fixture never reported its burst interval");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    /// The burst through the real generic reactor (plan Task 13): the fixture paces its 5,000
    /// deltas at 500/s, the adapter forwards them as production's capture adapter does, and the
    /// surface times each actual draw. The sample is exactly the active burst — every draw that
    /// started at or after the fixture's own start instant and before its own finish instant, and
    /// no other: the session's first paint, the pre-burst load and the quiet frames after it are
    /// all outside by construction, never by position. The preview's presentation parses are
    /// counted where they happen (panes::PRESENTED), over that same interval, and gated at ≤10 a
    /// second in half-open one-second windows.
    #[tokio::test]
    #[ignore = "perf: run with `cargo test -p lecturelive-cli perf -- --ignored --nocapture`"]
    async fn perf_burst_frame_work_p95() {
        let mut runs: Vec<Vec<Duration>> = Vec::new();
        let mut worst_hz_window = 0usize;
        for run in 0..3 {
            // each run measures its own burst: the observer and the parse probe start empty, so a
            // run can never read the previous run's interval
            crate::tui::panes::PRESENTED.with(|p| p.borrow_mut().clear());
            fixture::BURST_INTERVAL.with(|m| m.borrow_mut().clear());
            let probe: Probe = Arc::new(Mutex::new(Vec::new()));
            let r = reactor_on(
                ProbeSurface { terminal: ratatui::Terminal::new(TestBackend::new(140, 40)).unwrap(), probe: probe.clone() },
                probe,
                View::new(identity(), Hydration::empty(), Vec::new()),
                false,
                |commands, forward| {
                    let (ev_tx, mut ev_rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
                    tokio::spawn(async move {
                        // the capture adapter's forwarding, None persistence: a scripted session
                        while let Some(e) = ev_rx.recv().await {
                            if forward.send(Forwarded { event: e, capture_persistence: CapturePersistence::None }).is_err() {
                                return;
                            }
                        }
                    });
                    let dir = tempfile::tempdir().unwrap();
                    let files = LectureFiles::standard(dir.path(), chrono::NaiveDate::from_ymd_opt(2026, 9, 26).unwrap());
                    std::fs::create_dir_all(files.state_dir()).unwrap();
                    // the folder outlives the engine inside its own task, and is cleaned up with it
                    let engine = tokio::spawn(async move {
                        let _keep = &dir;
                        fixture::run(fixture::Scenario::Burst, &files, commands, ev_tx).await
                    });
                    (engine, None)
                },
            );
            let (burst_started, burst_finished) = burst_interval(Duration::from_secs(90)).await;
            let draws = r.draws();
            let _ = r.commands.send(Command::Stop);
            let (exit, ui) = r.finish().await;
            assert!(matches!(exit, Exit::Ended(_)), "the burst ended as the test stopped it");
            assert_eq!(ui.events, (fixture::BURST_DELTAS + fixture::BURST_SPEECH as usize + 4) as u64, "the whole burst: 5,000 deltas, 20 segments, the connection, the commit, its marker and the SourceEnded the drain takes");
            // draw work: exactly the active burst, by the fixture's own instants
            let active: Vec<Duration> = draws.iter().filter(|(at, _)| *at >= burst_started && *at < burst_finished).map(|(_, d)| *d).collect();
            assert!(active.len() >= 50, "run {run} drew only {} frames inside the active burst ({burst_started:?} … {burst_finished:?})", active.len());
            runs.push(active);
            // preview presentation: the same active interval, never the incoming deltas
            let parses: Vec<std::time::Instant> = crate::tui::panes::PRESENTED.with(|p| p.borrow().clone()).into_iter().filter(|at| *at >= burst_started && *at < burst_finished).collect();
            assert!(!parses.is_empty(), "the preview was presented");
            let (window, hz) = per_second(&parses, burst_started, burst_finished - burst_started);
            println!("PERF burst_run{run}_active_draws={} burst_s={:.3}", runs[run].len(), (burst_finished - burst_started).as_secs_f64());
            println!("PERF burst_run{run}_preview_parses={} mean_hz={:.2} max_per_s={window}", parses.len(), hz);
            worst_hz_window = worst_hz_window.max(window);
        }
        let worst = summarize("burst_frame", &runs);
        assert!(worst <= Duration::from_nanos(16_700_000), "burst frame work p95 {worst:?} exceeds the 16.7 ms budget");
        println!("PERF preview_hz={worst_hz_window}");
        assert!(worst_hz_window <= 10, "the preview was presented {worst_hz_window} times in one second (≤10)");
    }

    // ---- key-to-draw latency ----------------------------------------------------------------------

    /// Plan Task 13's corrected key-to-draw measurement: the **whole** 50 ms frame phase, not a
    /// favoured part of it. The earlier harness slept 30–130 ms after each draw, so nearly every
    /// key arrived past the 50 ms throttle deadline and drew at once — which measured the lucky
    /// phases only. Here each sample reads the exact previous draw start from the surface, then
    /// sends its key at a deterministic intended phase from that instant — 1 ms through 49 ms,
    /// cycled over the 100 samples, so the phases are spread across the whole period — and waits
    /// for its own draw. The latency is measured from the actual send, and the achieved phase is
    /// recorded as evidence rather than assumed; the intended schedule is never used to skip a
    /// sample. Keys are visible hint edits (a character, then Backspace): no command, no stop.
    #[tokio::test]
    #[ignore = "perf: run with `cargo test -p lecturelive-cli perf -- --ignored --nocapture`"]
    async fn perf_key_to_draw_p95() {
        const SAMPLES: usize = 100;
        let mut runs: Vec<Vec<Duration>> = Vec::new();
        let mut achieved: Vec<Duration> = Vec::new();
        for _run in 0..3 {
            let r = reactor(140, 40, view_of(&fixture::two_hour_events(0, 0)), false);
            r.next_draw(0, Duration::from_secs(5)).await; // the first frame is up
            let mut samples = Vec::with_capacity(SAMPLES);
            for k in 0..SAMPLES {
                // the prior draw's exact start, as the surface recorded it: the phase reference
                let prior = r.draws().last().expect("a frame is up").0;
                let want = Duration::from_millis(1 + (k % 49) as u64);
                let at = prior + want;
                if at > std::time::Instant::now() {
                    tokio::time::sleep_until(tokio::time::Instant::from_std(at)).await;
                }
                let before = r.draws().len();
                let key = if k % 2 == 0 {
                    KeyEvent::new(KeyCode::Char((b'a' + (k % 26) as u8) as char), KeyModifiers::NONE)
                } else {
                    KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)
                };
                let sent = std::time::Instant::now();
                r.keys.send(Ok(TermEvent::Key(key))).unwrap();
                let drew = r.next_draw_after(before, sent, Duration::from_secs(5)).await;
                achieved.push(sent - prior);
                samples.push(drew - sent);
            }
            runs.push(samples);
            r.finish().await;
        }
        // the phases actually reached, as evidence: their range and spread across the frame period
        let ms = |d: Duration| d.as_secs_f64() * 1e3;
        let (lo, hi) = (achieved.iter().copied().min().unwrap(), achieved.iter().copied().max().unwrap());
        let mut sorted = achieved.clone();
        sorted.sort();
        println!("PERF key_phase_achieved_min_ms={:.3} p50_ms={:.3} max_ms={:.3}", ms(lo), ms(sorted[sorted.len() / 2]), ms(hi));
        let worst = summarize("key_to_draw", &runs);
        assert!(worst <= Duration::from_millis(50), "key-to-draw p95 {worst:?} exceeds the 50 ms budget (phases reached: {lo:?} … {hi:?})");
    }

    /// The draw-start anchored scheduler itself, pinned (plan Task 13's remediation): every
    /// consecutive pair of draw starts is at least [`FRAME`] apart. The ≤ 20 draws a second the
    /// plan caps is therefore the scheduler's own invariant, proved directly, not only through
    /// an aligned one-second window in a timing test.
    #[tokio::test]
    async fn draw_start_spacing_holds_the_frame_cap() {
        let r = reactor(140, 40, view_of(&fixture::two_hour_events(0, 0)), false);
        r.next_draw(0, Duration::from_secs(5)).await;
        // a dozen accepted keys, each demanding its own frame
        for k in 0..12u32 {
            let before = r.draws().len();
            let key = if k % 2 == 0 {
                KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)
            } else {
                KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)
            };
            r.keys.send(Ok(TermEvent::Key(key))).unwrap();
            r.next_draw(before, Duration::from_secs(5)).await;
        }
        let draws = r.draws();
        r.finish().await;
        assert!(draws.len() >= 12, "the keys produced {} frames", draws.len());
        let mut smallest = Duration::MAX;
        for pair in draws.windows(2) {
            let gap = pair[1].0 - pair[0].0;
            assert!(gap >= FRAME, "draws {} ms apart: below the {FRAME:?} frame cap", gap.as_secs_f64() * 1e3);
            smallest = smallest.min(gap);
        }
        println!("PERF draw_start_min_gap_ms={:.3}", smallest.as_secs_f64() * 1e3);
    }

    // ---- event-to-draw latency ---------------------------------------------------------------------

    /// Plan Task 13: end-to-end, from the event's send into the unbounded channel (queue wait
    /// included, never only after `recv`) to the start of the first draw that follows its
    /// processing — one uniquely named marker notice at a time, 100 samples a repetition, three
    /// repetitions, gate on the worst p95.
    #[tokio::test]
    #[ignore = "perf: run with `cargo test -p lecturelive-cli perf -- --ignored --nocapture`"]
    async fn perf_event_to_draw_p95() {
        let mut runs: Vec<Vec<Duration>> = Vec::new();
        for run in 0..3 {
            let r = reactor(140, 40, view_of(&fixture::two_hour_events(0, 0)), false);
            r.next_draw(0, Duration::from_secs(5)).await;
            let mut samples = Vec::with_capacity(100);
            for k in 0..100u32 {
                let before = r.draws().len();
                let sent = std::time::Instant::now();
                r.events.send(Forwarded { event: Event::Warning(format!("marker {k} of run {run}")), capture_persistence: CapturePersistence::None }).unwrap();
                let drew = r.next_draw(before, Duration::from_secs(5)).await;
                samples.push(drew - sent);
            }
            runs.push(samples);
            r.finish().await;
        }
        let worst = summarize("event_to_draw", &runs);
        assert!(worst <= Duration::from_millis(100), "event-to-draw p95 {worst:?} exceeds the 100 ms budget");
    }

    // ---- draw rate under an ordinary stream ---------------------------------------------------------

    /// The draw rate the reactor actually drew (plan Task 13), from the surface's own start
    /// timestamps — never estimated from FRAME — under the ordinary stream the `transcript`
    /// fixture represents: a level every 100 ms, the open utterance moving every 300 ms, a closed
    /// segment every second. Every half-open one-second window must hold ≤20 draws.
    #[tokio::test]
    #[ignore = "perf: run with `cargo test -p lecturelive-cli perf -- --ignored --nocapture`"]
    async fn perf_draw_rate() {
        const SPAN: Duration = Duration::from_secs(10);
        let r = reactor(140, 40, view_of(&fixture::two_hour_events(0, 0)), true);
        let events = r.events.clone();
        tokio::spawn(async move {
            let (mut level, mut open, mut segment) = (
                tokio::time::interval(Duration::from_millis(100)),
                tokio::time::interval(Duration::from_millis(300)),
                tokio::time::interval(Duration::from_secs(1)),
            );
            let mut said = 0u64;
            let start = Local::now();
            loop {
                tokio::select! {
                    _ = level.tick() => { let _ = events.send(Forwarded { event: Event::Session(Notification::Level(0.05)), capture_persistence: CapturePersistence::None }); }
                    _ = open.tick() => { let _ = events.send(Forwarded { event: Event::Session(Notification::Open { stable: format!("the live words {said}"), tentative: " still settling".into() }), capture_persistence: CapturePersistence::None }); }
                    _ = segment.tick() => {
                        let at = start + chrono::Duration::seconds(said as i64);
                        let s = Segment { id: 1_440 + said, recording_id: Default::default(), start_sample: said * 16_000, end_sample: (said + 1) * 16_000, said_at: at, start: at, end: at, text: format!("An ordinary line {said} of the live stream."), words: Vec::new(), source: SegmentSource::Live };
                        said += 1;
                        let _ = events.send(Forwarded { event: Event::Session(Notification::Segment(s)), capture_persistence: CapturePersistence::None });
                    }
                }
            }
        });
        let first = r.next_draw(0, Duration::from_secs(5)).await;
        tokio::time::sleep(SPAN + Duration::from_millis(100)).await;
        let starts: Vec<std::time::Instant> = r.draws().iter().map(|(s, _)| *s).collect();
        r.finish().await;
        let (window, mean) = per_second(&starts, first, SPAN);
        println!("PERF ordinary_draws_per_s={mean:.2}");
        println!("PERF ordinary_draws_max_per_s={window}");
        assert!(window <= 20, "a one-second window held {window} draws (≤20)");
    }

    // ---- the quiet draw rate -------------------------------------------------------------------------

    /// The loaded two-hour session after its backlog (plan Task 13): steady state, the initial
    /// frame and the load excluded by construction (the state is the view the reactor starts
    /// with), the spend sampler's 1 Hz wakeups included — measured over twelve steady seconds so
    /// at least ten full windows count. The target is ≤1 draw a second.
    #[tokio::test]
    #[ignore = "perf: run with `cargo test -p lecturelive-cli perf -- --ignored --nocapture`"]
    async fn perf_quiet_draw_rate() {
        const SETTLE: Duration = Duration::from_secs(2);
        const SPAN: Duration = Duration::from_secs(12);
        let r = reactor(140, 40, view_of(&fixture::two_hour_events(0, 0)), true);
        r.next_draw(0, Duration::from_secs(5)).await;
        tokio::time::sleep(SETTLE).await;
        let t0 = std::time::Instant::now();
        tokio::time::sleep(SPAN).await;
        let draws: Vec<std::time::Instant> = r.draws().iter().map(|(s, _)| *s).filter(|s| *s >= t0).collect();
        r.finish().await;
        let (window, mean) = per_second(&draws, t0, SPAN);
        println!("PERF quiet_draws_per_s={mean:.2}");
        println!("PERF quiet_draws_max_per_s={window}");
        assert!(window <= 1, "a quiet one-second window held {window} draws (≤1)");
    }
}
