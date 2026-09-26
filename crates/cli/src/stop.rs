//! One stop controller for both frontends (M7 plan §G): the stage the lecture's stopping has reached
//! and how many `Stop`s core still needs, counted the same whether a key, a signal, the `--secs`
//! timer or the audio's own end began it. Pure: the caller prints, sends and exits.

use std::time::{Duration, Instant};

/// A held key's repeats keep arriving: a Key must follow this much quiet since the last Ctrl-C key.
const QUIET: Duration = Duration::from_millis(300);
/// macOS waits up to 1.8 s before a held key repeats: a Key must also come this long after the stage before.
const DWELL: Duration = Duration::from_secs(2);

/// Where a stop request came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)] // Key and SourceEnded arrive with the TUI (Task 5); plain feeds Signal and Timer
pub(crate) enum Origin {
    /// Ctrl-C read as a key in raw mode.
    Key,
    /// SIGINT: plain's Ctrl-C, or `kill -INT`.
    Signal,
    /// The `--secs` limit passed.
    Timer,
    /// The audio ended by itself: core is already draining, without a `Stop`.
    SourceEnded,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stage {
    Listening,
    /// Stage 1: finishing the transcript and recovery, then a last snapshot.
    Stopping,
    /// Stage 2: core has counted two `Stop`s, so it no longer waits for recovery or queued requests.
    StopWaiting,
}

/// Why a request changed nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)] // the TUI (Task 5) shows the reason; plain ignores it
pub(crate) enum Reason {
    /// A Key within the quiet interval of the last Ctrl-C key: a held key repeating.
    Held,
    /// A Key sooner than the dwell after the stage before.
    TooSoon,
    /// The timer or the audio's end once the lecture is already stopping: neither escalates.
    AlreadyStopping,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// The stage reached, and how many `Command::Stop`s the caller sends for it.
    Advance { stage: Stage, stops_to_send: u8 },
    Ignored(Reason),
    /// Stage 3: nothing is sent; the caller ends the process (exit 130).
    Quit,
}

#[derive(Debug)]
pub(crate) struct StopController {
    stage: Stage,
    /// `Stop`s already handed to the caller for core: core counts two to stop waiting.
    core_stops_sent: u8,
    /// When the current stage was entered.
    stage_at: Option<Instant>,
    /// The last Ctrl-C key event, accepted or not: every repeat of a held key moves it.
    key_at: Option<Instant>,
}

impl Default for StopController {
    fn default() -> Self {
        StopController { stage: Stage::Listening, core_stops_sent: 0, stage_at: None, key_at: None }
    }
}

impl StopController {
    /// One stop request at `now`.
    pub(crate) fn advance(&mut self, origin: Origin, now: Instant) -> Step {
        if origin == Origin::Key {
            let quiet = self.key_at.is_none_or(|k| now.saturating_duration_since(k) >= QUIET);
            self.key_at = Some(now);
            if self.stage != Stage::Listening {
                if !quiet {
                    return Step::Ignored(Reason::Held);
                }
                if self.stage_at.is_some_and(|s| now.saturating_duration_since(s) < DWELL) {
                    return Step::Ignored(Reason::TooSoon);
                }
            }
        }
        match (self.stage, origin) {
            (Stage::Listening, _) => {
                let stops_to_send = if origin == Origin::SourceEnded { 0 } else { 1 };
                self.enter(Stage::Stopping, stops_to_send, now)
            }
            (_, Origin::Timer | Origin::SourceEnded) => Step::Ignored(Reason::AlreadyStopping),
            (Stage::Stopping, _) => self.enter(Stage::StopWaiting, 2 - self.core_stops_sent, now),
            (Stage::StopWaiting, _) => Step::Quit,
        }
    }

    fn enter(&mut self, stage: Stage, stops_to_send: u8, now: Instant) -> Step {
        self.stage = stage;
        self.core_stops_sent += stops_to_send;
        self.stage_at = Some(now);
        Step::Advance { stage, stops_to_send }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn advance(stage: Stage, stops_to_send: u8) -> Step {
        Step::Advance { stage, stops_to_send }
    }

    #[test]
    fn first_key_stops_at_once() {
        let mut stop = StopController::default();
        assert_eq!(stop.advance(Origin::Key, Instant::now()), advance(Stage::Stopping, 1));
        assert_eq!(stop.core_stops_sent, 1);
    }

    /// macOS repeats a held key after 225–1800 ms, then about every 33 ms; every repeat is a Key.
    #[test]
    fn held_ctrl_c_cannot_escalate() {
        for delay in [225, 500, 1800] {
            let t0 = Instant::now();
            let mut stop = StopController::default();
            assert_eq!(stop.advance(Origin::Key, t0), advance(Stage::Stopping, 1));
            let mut at = t0 + ms(delay);
            while at < t0 + Duration::from_secs(20) {
                assert!(matches!(stop.advance(Origin::Key, at), Step::Ignored(_)), "a repeat {:?} after the press escalated (delay {delay} ms)", at - t0);
                assert_eq!(stop.key_at, Some(at), "every repeat refreshes the quiet interval");
                at += ms(33);
            }
            assert_eq!((stop.stage, stop.core_stops_sent), (Stage::Stopping, 1));
        }
    }

    #[test]
    fn second_stage_needs_quiet_and_dwell() {
        let t0 = Instant::now();
        let mut stop = StopController::default();
        assert_eq!(stop.advance(Origin::Key, t0), advance(Stage::Stopping, 1));
        // Not quiet (100 ms after the last key), and too soon.
        assert_eq!(stop.advance(Origin::Key, t0 + ms(100)), Step::Ignored(Reason::Held));
        // Quiet (900 ms), but 1 s after stage 1.
        assert_eq!(stop.advance(Origin::Key, t0 + ms(1000)), Step::Ignored(Reason::TooSoon));
        // Past the dwell, but 200 ms after the last key.
        assert_eq!(stop.advance(Origin::Key, t0 + ms(1900)), Step::Ignored(Reason::TooSoon));
        assert_eq!(stop.advance(Origin::Key, t0 + ms(2100)), Step::Ignored(Reason::Held));
        // Both: 400 ms quiet and 2.5 s after stage 1.
        assert_eq!(stop.advance(Origin::Key, t0 + ms(2500)), advance(Stage::StopWaiting, 1));
        // Stage 3 measures its dwell from stage 2.
        assert_eq!(stop.advance(Origin::Key, t0 + ms(4000)), Step::Ignored(Reason::TooSoon));
        assert_eq!(stop.advance(Origin::Key, t0 + ms(4600)), Step::Quit);
    }

    /// Plan §B (a): the timer's `Stop` is stage 1, so the next Ctrl-C is stage 2 and sends one more.
    #[test]
    fn secs_then_ctrl_c_advances_shared_stop() {
        let t0 = Instant::now();
        let mut stop = StopController::default();
        assert_eq!(stop.advance(Origin::Timer, t0), advance(Stage::Stopping, 1));
        assert_eq!(stop.advance(Origin::Signal, t0 + ms(10)), advance(Stage::StopWaiting, 1));
        assert_eq!(stop.core_stops_sent, 2);
        assert_eq!(stop.advance(Origin::Signal, t0 + ms(20)), Step::Quit);
    }

    /// The timer only ever begins the stop: after a Ctrl-C it sends nothing more.
    #[test]
    fn ctrl_c_then_secs_sends_no_second_stop() {
        let t0 = Instant::now();
        let mut stop = StopController::default();
        assert_eq!(stop.advance(Origin::Signal, t0), advance(Stage::Stopping, 1));
        assert_eq!(stop.advance(Origin::Timer, t0 + ms(10)), Step::Ignored(Reason::AlreadyStopping));
        assert_eq!((stop.stage, stop.core_stops_sent), (Stage::Stopping, 1));
    }

    /// Core does not count `SourceEnded` as a stop (`lecture.rs:451`, `coordinator.rs:681`), so stopping
    /// the wait after it takes two.
    #[test]
    fn source_ended_then_stop_waiting_sends_two_stops() {
        let t0 = Instant::now();
        let mut stop = StopController::default();
        assert_eq!(stop.advance(Origin::SourceEnded, t0), advance(Stage::Stopping, 0));
        assert_eq!(stop.advance(Origin::Timer, t0 + ms(10)), Step::Ignored(Reason::AlreadyStopping));
        assert_eq!(stop.advance(Origin::Signal, t0 + ms(20)), advance(Stage::StopWaiting, 2));
        assert_eq!(stop.core_stops_sent, 2);

        // A Key after it keeps its dwell, measured from the audio's end.
        let mut stop = StopController::default();
        assert_eq!(stop.advance(Origin::SourceEnded, t0), advance(Stage::Stopping, 0));
        assert_eq!(stop.advance(Origin::Key, t0 + ms(500)), Step::Ignored(Reason::TooSoon));
        assert_eq!(stop.advance(Origin::Key, t0 + ms(2100)), advance(Stage::StopWaiting, 2));
    }

    #[test]
    fn signal_origin_is_not_debounced() {
        let t0 = Instant::now();
        let mut stop = StopController::default();
        assert_eq!(stop.advance(Origin::Signal, t0), advance(Stage::Stopping, 1));
        assert_eq!(stop.advance(Origin::Signal, t0), advance(Stage::StopWaiting, 1));
        assert_eq!(stop.advance(Origin::Signal, t0), Step::Quit);
    }

    #[test]
    fn third_stage_quits() {
        let t0 = Instant::now();
        let mut stop = StopController::default();
        stop.advance(Origin::Key, t0);
        stop.advance(Origin::Key, t0 + ms(2500));
        assert_eq!(stop.advance(Origin::Key, t0 + ms(5000)), Step::Quit);
        assert_eq!(stop.core_stops_sent, 2, "stage 3 sends nothing");
        assert_eq!(stop.advance(Origin::Signal, t0 + ms(5010)), Step::Quit);
    }
}
