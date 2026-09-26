//! The terminal frontend (M7 plan §F, §G): the reactor on the main thread. It owns the terminal and what
//! it shows, drains every lecture event, reads the keyboard through Crossterm's `EventStream` alone, and
//! gives Ctrl-C, SIGINT, `--secs` and the audio's own end to the stop controller both frontends share.
//! Every way out goes through [`leave`], which gives the terminal back before anything is printed.

pub(crate) mod terminal;
mod view;

use std::future::Future;
use std::io::{self, Write};
use std::time::Duration;

use anyhow::Result;
use futures_util::StreamExt;
use lecturelive_core::session::coordinator::{Notification, StopReport};
use lecturelive_core::session::lecture::{Command, Event};
use ratatui::crossterm::event::{Event as TermEvent, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::task::JoinError;
use tokio::time::{interval_at, sleep_until, Instant, MissedTickBehavior};

use crate::stop::{Origin, Stage, Step, StopController};
use terminal::Screen;

/// At most 20 draws a second.
const FRAME: Duration = Duration::from_millis(50);
/// Events applied on one wake before the next draw.
const BATCH: usize = 256;

/// The view's state and the stop controller: the reactor's decisions, without its I/O.
struct Ui {
    stop: StopController,
    stage: Stage,
    events: u64,
    notice: Option<&'static str>,
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
    fn new() -> Ui {
        Ui { stop: StopController::default(), stage: Stage::Listening, events: 0, notice: None }
    }

    /// One stop request through the shared controller; the view follows the stage it reached.
    fn stop(&mut self, origin: Origin, now: std::time::Instant) -> Step {
        let step = self.stop.advance(origin, now);
        match step {
            Step::Advance { stage, .. } => {
                self.stage = stage;
                self.notice = None;
            }
            Step::Ignored(_) if origin == Origin::Key => self.notice = Some(view::not_yet(self.stage)),
            Step::Ignored(_) | Step::Quit => {}
        }
        step
    }

    /// Every event is drained and counted. The audio's own end is a stop the controller must know of.
    fn event(&mut self, e: &Event, now: std::time::Instant) -> Option<Step> {
        self.events += 1;
        matches!(e, Event::Session(Notification::SourceEnded)).then(|| self.stop(Origin::SourceEnded, now))
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

    fn status(&self, elapsed: Duration) -> view::Status {
        view::Status { stage: self.stage, elapsed, events: self.events, notice: self.notice }
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

/// Runs the lecture in the terminal: signals, the panic hook, the terminal, then the lecture and the
/// keyboard, in that order, and the reactor until one of them ends it. The terminal is taken only here,
/// after the folder is prepared; a failure to take it is an ordinary error, with nothing started.
pub(crate) async fn run(engine: impl Future<Output = Result<StopReport>> + Send + 'static, commands: UnboundedSender<Command>, events: UnboundedReceiver<Event>, secs: Option<u64>) -> Result<StopReport> {
    let interrupt = signal(SignalKind::interrupt())?;
    let terminate = signal(SignalKind::terminate())?;
    let hangup = signal(SignalKind::hangup())?;
    terminal::install_panic_hook();
    let screen = terminal::enter().map_err(|e| anyhow::anyhow!("the terminal could not be taken over ({e})"))?;
    let started = Instant::now();
    let lecture = tokio::spawn(engine);
    let keys = EventStream::new();
    let exit = react(Io { screen, keys, lecture, events, commands, interrupt, terminate, hangup }, started, secs).await;
    leave(exit)
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
}

/// The loop. It draws only here, and only when something changed, at most every [`FRAME`]; returning
/// is the fence after which it never draws again.
async fn react(mut io: Io, started: Instant, secs: Option<u64>) -> Exit {
    let mut ui = Ui::new();
    let mut timer = secs.map(|s| started + Duration::from_secs(s));
    let mut clock = interval_at(started + Duration::from_secs(1), Duration::from_secs(1));
    clock.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let (mut dirty, mut drawn_at) = (true, started - FRAME);
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
                    ui.event(&e, std::time::Instant::now());
                }
                return Exit::Ended(ended);
            }
            Some(e) = io.events.recv() => {
                let mut next = Some(e);
                for _ in 0..BATCH {
                    let Some(e) = next.take() else { break };
                    if let Some(step) = ui.event(&e, std::time::Instant::now()) {
                        send_stops(&io.commands, step);
                    }
                    next = io.events.try_recv().ok();
                }
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
            let status = ui.status(started.elapsed());
            if let Err(e) = terminal::draw(&mut io.screen, |f| view::render(f, &status)) {
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
    use std::time::Instant;

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn raw_ctrl_c_is_a_key_stop_through_the_shared_controller() {
        let (mut ui, t0) = (Ui::new(), Instant::now());
        assert_eq!(ui.key(ctrl('c'), t0), Act::Stop(Step::Advance { stage: Stage::Stopping, stops_to_send: 1 }));
        assert_eq!(ui.stage, Stage::Stopping);
        // The Key origin's quiet and dwell hold: a repeat 30 ms later is not taken, and says why.
        assert!(matches!(ui.key(ctrl('c'), t0 + ms(30)), Act::Stop(Step::Ignored(_))));
        assert_eq!(ui.notice, Some(view::not_yet(Stage::Stopping)));
        assert_eq!(ui.key(ctrl('c'), t0 + ms(2500)), Act::Stop(Step::Advance { stage: Stage::StopWaiting, stops_to_send: 1 }));
        assert_eq!((ui.stage, ui.notice), (Stage::StopWaiting, None));
        assert_eq!(ui.key(ctrl('c'), t0 + ms(5000)), Act::Stop(Step::Quit));
    }

    #[test]
    fn source_ended_is_a_stop_the_controller_counts() {
        let (mut ui, t0) = (Ui::new(), Instant::now());
        assert_eq!(ui.event(&Event::Session(Notification::Level(0.1)), t0), None);
        assert_eq!(ui.event(&Event::Session(Notification::SourceEnded), t0), Some(Step::Advance { stage: Stage::Stopping, stops_to_send: 0 }));
        assert_eq!(ui.stage, Stage::Stopping, "the audio ended by itself");
        assert_eq!(ui.events, 2, "every event is counted");
        // Stop waiting after it sends two: core had counted none.
        assert_eq!(ui.key(ctrl('c'), t0 + ms(2500)), Act::Stop(Step::Advance { stage: Stage::StopWaiting, stops_to_send: 2 }));
    }

    #[test]
    fn source_ended_after_a_stop_changes_nothing() {
        let (mut ui, t0) = (Ui::new(), Instant::now());
        ui.key(ctrl('c'), t0);
        assert!(matches!(ui.event(&Event::Session(Notification::SourceEnded), t0 + ms(200)), Some(Step::Ignored(_))));
        assert_eq!((ui.stage, ui.notice), (Stage::Stopping, None), "no notice: nobody pressed anything");
    }

    #[test]
    fn external_sigint_is_not_debounced() {
        let (mut ui, t0) = (Ui::new(), Instant::now());
        assert_eq!(ui.stop(Origin::Signal, t0), Step::Advance { stage: Stage::Stopping, stops_to_send: 1 });
        assert_eq!(ui.stop(Origin::Signal, t0), Step::Advance { stage: Stage::StopWaiting, stops_to_send: 1 });
        assert_eq!(ui.stop(Origin::Signal, t0), Step::Quit);
    }

    #[test]
    fn ctrl_z_only_says_why_it_does_nothing_and_ctrl_l_clears() {
        let (mut ui, t0) = (Ui::new(), Instant::now());
        assert_eq!(ui.key(ctrl('z'), t0), Act::Redraw);
        assert_eq!((ui.stage, ui.notice), (Stage::Listening, Some(view::SUSPEND)));
        assert_eq!(ui.key(ctrl('l'), t0), Act::Clear);
        assert_eq!(ui.stage, Stage::Listening);
    }

    #[test]
    fn plain_keys_and_releases_do_nothing() {
        let (mut ui, t0) = (Ui::new(), Instant::now());
        assert_eq!(ui.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE), t0), Act::Nothing);
        let mut release = ctrl('c');
        release.kind = KeyEventKind::Release;
        assert_eq!(ui.key(release, t0), Act::Nothing);
        assert_eq!(ui.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), t0), Act::Nothing);
        assert_eq!(ui.stage, Stage::Listening);
    }
}
