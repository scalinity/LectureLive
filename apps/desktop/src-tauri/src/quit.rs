//! Quitting with a lecture running (spec §9.6). The app is quit by the person, or by something that quits apps (a
//! proctoring browser, the system); either way a running lecture must be saved first: the recording finalized, the
//! open utterance flushed, the sidecar written, so the next start repairs nothing and loses no tail.
//!
//! How a quit reaches Tauri 2.11 on macOS (read from tao 0.35.3 and tauri-runtime-wry 2.11.4):
//! - Cmd-Q, and an Apple-Event quit, are `terminate:` (the default menu's Quit item, and AppKit's handler for the
//!   event). tao implements `applicationWillTerminate:` and not `applicationShouldTerminate:`, so this arrives as
//!   `RunEvent::Exit` only, after which the process ends: it cannot be cancelled, only delayed by blocking there.
//! - The last window closing, and `AppHandle::exit`, arrive as `RunEvent::ExitRequested`, which can be prevented.
//! - SIGTERM, SIGHUP and SIGINT are handled by nothing in Tauri, tao or wry: the process would end at once.
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// How long a quit waits for the lecture to save before it goes ahead anyway: the stop itself is quick (a quit takes
/// no last snapshot and does not wait for recovery), so this is a bound, not an expectation.
pub const QUIT_WAIT: Duration = Duration::from_secs(8);

/// How often the wait looks.
pub const POLL: Duration = Duration::from_millis(50);

/// When a running lecture has to be saved by, for every route a quit can take. The first quit sets it; a quit that
/// arrives by another route while the lecture is being saved (an Apple Event, then a SIGTERM) waits for the same
/// moment, so none ends the process mid-save and none waits twice.
#[derive(Default)]
pub struct Quit(OnceLock<Instant>);

impl Quit {
    /// The moment to wait until, or None when there is nothing to wait for: no lecture runs, or the time is up.
    pub fn deadline(&self, lecture_running: bool, limit: Duration) -> Option<Instant> {
        lecture_running.then(|| *self.0.get_or_init(|| Instant::now() + limit)).filter(|d| *d > Instant::now())
    }
}

/// Waits, looking every `poll`, until `done` says so or `limit` has passed; whether it was done.
pub async fn wait_until(done: impl Fn() -> bool, limit: Duration, poll: Duration) -> bool {
    let end = tokio::time::Instant::now() + limit;
    while !done() {
        if tokio::time::Instant::now() >= end {
            return false;
        }
        tokio::time::sleep(poll).await;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[test]
    fn no_lecture_means_no_wait_and_the_first_quit_sets_a_deadline_every_later_one_shares() {
        let q = Quit::default();
        assert_eq!(q.deadline(false, Duration::from_secs(8)), None, "nothing runs: exit at once");
        assert_eq!(q.deadline(false, Duration::from_secs(8)), None, "and that sets no deadline");
        let first = q.deadline(true, Duration::from_secs(8)).expect("a lecture runs: wait for it");
        std::thread::sleep(Duration::from_millis(20));
        let second = q.deadline(true, Duration::from_secs(8)).expect("still running, still inside the deadline");
        assert_eq!(first, second, "a second quit, by another route, waits for the same moment: it does not wait afresh or not at all");
    }

    /// An Apple-Event quit followed by a SIGTERM must not let the second end the process mid-save, nor loop forever
    /// once the time is up.
    #[test]
    fn after_the_deadline_there_is_nothing_left_to_wait_for() {
        let q = Quit::default();
        assert!(q.deadline(true, Duration::from_millis(10)).is_some());
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(q.deadline(true, Duration::from_millis(10)), None, "time is up: the quit goes ahead, however it arrives");
    }

    #[tokio::test]
    async fn the_wait_ends_as_soon_as_the_lecture_has() {
        let looks = Arc::new(AtomicUsize::new(0));
        let l = looks.clone();
        let done = wait_until(move || l.fetch_add(1, Ordering::SeqCst) >= 3, Duration::from_secs(5), Duration::from_millis(2)).await;
        assert!(done);
        assert_eq!(looks.load(Ordering::SeqCst), 4, "it looked four times and went on, not for the whole limit");
    }

    #[tokio::test]
    async fn the_wait_is_bounded_when_the_lecture_does_not_end() {
        let started = std::time::Instant::now();
        assert!(!wait_until(|| false, Duration::from_millis(60), Duration::from_millis(5)).await);
        assert!(started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());
    }

    #[tokio::test]
    async fn a_lecture_already_over_needs_no_wait() {
        assert!(wait_until(|| true, Duration::from_secs(5), Duration::from_secs(5)).await);
    }
}
