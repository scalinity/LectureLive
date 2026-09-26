//! The terminal frontend (M7 plan §F, §G): the reactor on the main thread. It owns the terminal and
//! what it shows — the session view, reduced from every lecture event and reconciled with the
//! canonical files — reads the keyboard through Crossterm's `EventStream` alone, and gives Ctrl-C,
//! SIGINT, `--secs` and the audio's own end to the stop controller both frontends share.
//! Every way out goes through [`leave`], which gives the terminal back before anything is printed.

pub(crate) mod hydrate;
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
use lecturelive_core::session::lecture::{Command, Event};
use lecturelive_core::session::spend::Spend;
use ratatui::crossterm::event::{Event as TermEvent, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::task::JoinError;
use tokio::time::{interval_at, sleep_until, Instant, MissedTickBehavior};

use crate::plain;
use crate::stop::{Origin, Step, StopController};
use state::{Identity, View};
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
}

/// The view's state and the stop controller: the reactor's decisions, without its I/O.
struct Ui {
    stop: StopController,
    events: u64,
    /// A stop the controller refused (a held key): UI-local, shown above the view's own notices.
    notice: Option<&'static str>,
    /// The reading pane the frame shows in a tabbed column: the transcript until Task 10's keys move it.
    focus: view::Pane,
    /// The reader's place in the transcript: following, or held at an anchor. Kept whatever the
    /// layout, so a pane that is hidden and shown again is where it was left.
    transcript: panes::Scroll,
    /// The transcript's body as last drawn; none while it is not on screen, when reading keys
    /// leave its place alone.
    reading: Option<Rect>,
    view: View,
}

/// What a key asks of the reactor.
#[derive(Debug, PartialEq)]
enum Act {
    Nothing,
    Redraw,
    Clear,
    Stop(Step),
}

impl Ui {
    fn new(view: View) -> Ui {
        Ui { stop: StopController::default(), events: 0, notice: None, focus: view::Pane::Transcript, transcript: panes::Scroll::default(), reading: None, view }
    }

    /// One stop request through the shared controller; the view's phase follows the stage it reached.
    fn stop(&mut self, origin: Origin, now: std::time::Instant) -> Step {
        let step = self.stop.advance(origin, now);
        match step {
            Step::Advance { stage, .. } => {
                self.view.phase = stage;
                self.notice = None;
            }
            Step::Ignored(_) if origin == Origin::Key => self.notice = Some(view::not_yet(self.view.phase)),
            Step::Ignored(_) | Step::Quit => {}
        }
        step
    }

    /// Every event is drained and reduced into the view; the audio's own end is a stop the controller
    /// must know of. Returns the stop step, if any, and whether a re-read of the files was asked for.
    fn event(&mut self, e: &Event, now: std::time::Instant) -> (Option<Step>, bool) {
        self.events += 1;
        let effect = self.view.reduce(e, Local::now());
        let step = matches!(e, Event::Session(Notification::SourceEnded)).then(|| self.stop(Origin::SourceEnded, now));
        (step, effect.hydrate)
    }

    /// Captured, so the terminal reports it here instead of scrolling its own viewport. The wheel
    /// reads the transcript, [`panes::WHEEL`] rows a notch, as the arrow keys do; press, release,
    /// drag and motion are consumed and mean nothing.
    fn mouse(&mut self, mouse: MouseEvent) -> Act {
        match mouse.kind {
            MouseEventKind::ScrollUp => self.read(panes::Move::Up(panes::WHEEL)),
            MouseEventKind::ScrollDown => self.read(panes::Move::Down(panes::WHEEL)),
            _ => Act::Nothing,
        }
    }

    /// A reading move in the focused pane, the transcript while it is the only one that scrolls.
    /// Only the reader's place moves: nothing is sent and nothing is read.
    fn read(&mut self, m: panes::Move) -> Act {
        if self.focus != view::Pane::Transcript {
            return Act::Nothing;
        }
        let moved = match self.reading {
            Some(body) => self.transcript.apply(m, &self.view, body),
            None if m == panes::Move::Live => self.transcript.apply(m, &self.view, Rect::default()),
            None => false,
        };
        if moved {
            Act::Redraw
        } else {
            Act::Nothing
        }
    }

    /// In raw mode Ctrl-C, Ctrl-Z and Ctrl-L are keys, not signals; the arrows, the page keys and Esc
    /// read the transcript. Everything else, printable keys included, waits for later tasks.
    fn key(&mut self, key: KeyEvent, now: std::time::Instant) -> Act {
        if key.kind == KeyEventKind::Release {
            return Act::Nothing;
        }
        if !key.modifiers.contains(KeyModifiers::CONTROL) {
            return match key.code {
                KeyCode::Up => self.read(panes::Move::Up(1)),
                KeyCode::Down => self.read(panes::Move::Down(1)),
                KeyCode::PageUp => self.read(panes::Move::PageUp),
                KeyCode::PageDown => self.read(panes::Move::PageDown),
                KeyCode::Esc => self.read(panes::Move::Live),
                _ => Act::Nothing,
            };
        }
        match key.code {
            KeyCode::Char('c') => Act::Stop(self.stop(Origin::Key, now)),
            KeyCode::Char('z') => {
                self.notice = Some(view::SUSPEND);
                Act::Redraw
            }
            KeyCode::Char('l') => Act::Clear,
            _ => Act::Nothing,
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
    // work; the reactor alone applies them.
    let (spend_tx, spend_rx) = tokio::sync::mpsc::channel(1);
    tokio::spawn(sample_spend(session.spend.clone(), spend_tx));
    let (hydrate_tx, hydrate_rx) = tokio::sync::mpsc::channel::<anyhow::Result<hydrate::Hydration>>(1);
    let exit = react(Io { screen, keys, lecture, events, commands, interrupt, terminate, hangup, files: session.files, hydrate_tx, hydrate_rx, spend_rx }, view, started, secs).await;
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
async fn react(mut io: Io, view: View, started: Instant, secs: Option<u64>) -> Exit {
    let mut ui = Ui::new(view);
    // Colour and glyphs are the terminal's for the whole session: read once, as the plain CLI reads them.
    let theme = view::Theme::detect();
    let mut timer = secs.map(|s| started + Duration::from_secs(s));
    let mut clock = interval_at(started + Duration::from_secs(1), Duration::from_secs(1));
    clock.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let (mut dirty, mut drawn_at) = (true, started - FRAME);
    let (mut hydrating, mut again) = (false, false);
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
                },
                // The next draw lays out `frame.area()` at the new size; nothing else keeps one.
                Some(Ok(TermEvent::Resize(..))) => dirty = true,
                // The lease captures the mouse so scrolls and drags reach the TUI instead of moving
                // the viewport; every one is consumed here. The wheel reads the transcript.
                Some(Ok(TermEvent::Mouse(mouse))) => match ui.mouse(mouse) {
                    Act::Nothing => {}
                    Act::Redraw => dirty = true,
                    Act::Clear | Act::Stop(_) => unreachable!("the mouse only reads"),
                },
                // A paste is text, never keys; Task 10 inserts it into the hint.
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
            _ = sleep_until(timer.unwrap_or(started)), if timer.is_some() => {
                timer = None;
                send_stops(&io.commands, ui.stop(Origin::Timer, std::time::Instant::now()));
                dirty = true;
            }
            _ = clock.tick() => dirty = true,
            _ = sleep_until(drawn_at + FRAME), if dirty => {}
        }
        if dirty && Instant::now() >= drawn_at + FRAME {
            let chrome = view::Chrome { elapsed: started.elapsed(), refused: ui.notice, focus: ui.focus, transcript: &ui.transcript, theme: &theme };
            let mut reading = ui.reading;
            if let Err(e) = terminal::draw(&mut io.screen, |f| reading = view::render(f, &ui.view, &chrome)) {
                return Exit::DrawFailed(e);
            }
            ui.reading = reading;
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
    use crate::stop::Stage;
    use crate::tui::hydrate::Hydration;
    use crate::tui::state::{Identity, SourceKind};
    use std::time::Instant;

    fn identity() -> Identity {
        Identity { course: "Machine Learning".into(), lecture: "Week 03 — Optimisation".into(), input: "BlackHole 2ch".into(), kind: SourceKind::Loopback, notes_file: "lecture_notes_20260926.md".into(), transcript_file: "lecture_transcript_20260926.txt".into() }
    }

    fn ui() -> Ui {
        Ui::new(state::View::new(identity(), Hydration::empty(), Vec::new()))
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
        assert_eq!(ui.notice, Some(view::not_yet(Stage::Stopping)));
        assert_eq!(ui.key(ctrl('c'), t0 + ms(2500)), Act::Stop(Step::Advance { stage: Stage::StopWaiting, stops_to_send: 1 }));
        assert_eq!((ui.view.phase, ui.notice), (Stage::StopWaiting, None));
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
        assert_eq!((ui.view.phase, ui.notice), (Stage::Listening, Some(view::SUSPEND)));
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
        let theme = view::Theme::new(lecturelive_core::session::spend::Paint { color: true, truecolor: true }, true);
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        let chrome = view::Chrome { elapsed: Duration::from_secs(60), refused: None, focus: ui.focus, transcript: &ui.transcript, theme: &theme };
        let mut reading = None;
        terminal.draw(|f| reading = view::render(f, &ui.view, &chrome)).unwrap();
        ui.reading = reading;
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
        for c in ['k', 'j', ' ', 'q'] {
            assert_eq!(ui.key(key(KeyCode::Char(c)), Instant::now()), Act::Nothing, "{c:?}");
        }
        assert_eq!(ui.key(key(KeyCode::Down), Instant::now()), Act::Redraw);
        assert!(ui.transcript.following(), "down to the end");
        ui.key(key(KeyCode::PageUp), Instant::now());
        assert_eq!(ui.key(key(KeyCode::Esc), Instant::now()), Act::Redraw);
        assert!(ui.transcript.following());
        assert_eq!(frame(&mut ui, 80, 25), live, "the live bottom again");
        // too small: nothing on screen, nothing moves
        frame(&mut ui, 40, 8);
        assert_eq!(ui.reading, None);
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
    fn plain_keys_and_releases_do_nothing() {
        let (mut ui, t0) = (ui(), Instant::now());
        assert_eq!(ui.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE), t0), Act::Nothing);
        let mut release = ctrl('c');
        release.kind = KeyEventKind::Release;
        assert_eq!(ui.key(release, t0), Act::Nothing);
        assert_eq!(ui.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), t0), Act::Nothing);
        assert_eq!(ui.view.phase, Stage::Listening);
    }
}
