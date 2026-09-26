//! The terminal frontend (M7 plan §F, §G): the reactor on the main thread. It owns the terminal and
//! what it shows — the session view, reduced from every lecture event and reconciled with the
//! canonical files — reads the keyboard through Crossterm's `EventStream` alone, and gives Ctrl-C,
//! SIGINT, `--secs` and the audio's own end to the stop controller both frontends share.
//! Every way out goes through [`leave`], which gives the terminal back before anything is printed.

pub(crate) mod hydrate;
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
use ratatui::crossterm::event::{Event as TermEvent, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
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
        Ui { stop: StopController::default(), events: 0, notice: None, view }
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

    /// In raw mode Ctrl-C, Ctrl-Z and Ctrl-L are keys, not signals. Everything else waits for later tasks.
    fn key(&mut self, key: KeyEvent, now: std::time::Instant) -> Act {
        if key.kind == KeyEventKind::Release || !key.modifiers.contains(KeyModifiers::CONTROL) {
            return Act::Nothing;
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
            let elapsed = started.elapsed();
            if let Err(e) = terminal::draw(&mut io.screen, |f| view::render(f, &ui.view, elapsed, ui.notice)) {
                return Exit::DrawFailed(e);
            }
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
        let held = (ui.view.closed.clone(), ui.view.gaps, ui.view.notes.revision, ui.view.stt.clone(), ui.view.open.clone());
        assert_eq!(ui.stop(Origin::Key, t0 + ms(2500)), Step::Advance { stage: Stage::StopWaiting, stops_to_send: 1 });
        assert_eq!(ui.view.phase, Stage::StopWaiting, "the projection reflects the stage and nothing more");
        assert_eq!((ui.view.closed.clone(), ui.view.gaps, ui.view.notes.revision, ui.view.stt.clone(), ui.view.open.clone()), held);
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
