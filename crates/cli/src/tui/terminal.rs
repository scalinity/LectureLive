//! The terminal lease (M7 plan §G): the one path that takes the terminal for the TUI and the one that
//! gives it back, from every way the process can leave, the panic hook included. Its state is two
//! atomics, so any thread restores without a lock of LectureLive's, an await or a destructor: the
//! release build aborts on panic, so no `Drop` can be the guarantee.

use std::io::{self, Stdout, Write};
use std::mem::ManuallyDrop;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Once;

use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::cursor::Show;
use ratatui::crossterm::event::{DisableBracketedPaste, EnableBracketedPaste};
use ratatui::crossterm::execute;
use ratatui::crossterm::style::ResetColor;
use ratatui::crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::layout::Rect;
use ratatui::{Frame, Terminal};

/// The TUI's drawing surface. Its `Drop` never runs: it would write a cursor-show after the lease had given
/// the terminal back, and call `eprintln!`, which panics on a closed terminal, when that write failed.
pub(crate) type Screen = ManuallyDrop<Terminal<CrosstermBackend<Stdout>>>;

// The lease's states, in order. Restoration is claimed from Entering or Active, by one caller only.
const INACTIVE: u8 = 0;
const ENTERING: u8 = 1;
const ACTIVE: u8 = 2;
const RESTORING: u8 = 3;
const RESTORED: u8 = 4;

// The steps of taking the terminal, each recorded once it has succeeded, so restoration undoes only those.
const RAW: u8 = 1 << 0;
const ALTERNATE: u8 = 1 << 1;
const PASTE: u8 = 1 << 2;
/// The drawing surface exists: its draws hide the cursor and set colours.
const SURFACE: u8 = 1 << 3;

struct Lease {
    state: AtomicU8,
    taken: AtomicU8,
}

impl Lease {
    const fn new() -> Lease {
        Lease { state: AtomicU8::new(INACTIVE), taken: AtomicU8::new(0) }
    }

    /// Inactive → Entering: the terminal is taken once per process.
    fn begin(&self) -> bool {
        self.state.compare_exchange(INACTIVE, ENTERING, Ordering::AcqRel, Ordering::Acquire).is_ok()
    }

    fn took(&self, step: u8) {
        self.taken.fetch_or(step, Ordering::AcqRel);
    }

    /// Entering → Active; false when a restoration claimed the lease first.
    fn activate(&self) -> bool {
        self.state.compare_exchange(ENTERING, ACTIVE, Ordering::AcqRel, Ordering::Acquire).is_ok()
    }

    fn is_active(&self) -> bool {
        self.state.load(Ordering::Acquire) == ACTIVE
    }

    /// Entering or Active → Restoring, for exactly one caller: the steps it has to undo. Every other
    /// caller, and any caller before the terminal was taken, gets nothing to do.
    fn claim(&self) -> Option<u8> {
        let mut state = self.state.load(Ordering::Acquire);
        while state == ENTERING || state == ACTIVE {
            match self.state.compare_exchange_weak(state, RESTORING, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => return Some(self.taken.load(Ordering::Acquire)),
                Err(now) => state = now,
            }
        }
        None
    }

    fn release(&self) {
        self.state.store(RESTORED, Ordering::Release);
    }
}

static LEASE: Lease = Lease::new();

/// Takes the terminal: raw mode, the alternate screen, bracketed paste, then the drawing surface, each
/// recorded as it succeeds. When a step fails, the ones already taken are undone before the error returns.
pub(crate) fn enter() -> io::Result<Screen> {
    if !LEASE.begin() {
        return Err(io::Error::other("the terminal was already taken once"));
    }
    match acquire() {
        Ok(screen) if LEASE.activate() => Ok(screen),
        Ok(_) => Err(io::Error::other("the terminal was given back while it was being taken")),
        Err(e) => {
            let _ = restore();
            Err(e)
        }
    }
}

fn acquire() -> io::Result<Screen> {
    fault(RAW)?;
    terminal::enable_raw_mode()?;
    LEASE.took(RAW);
    fault(ALTERNATE)?;
    execute!(io::stdout(), EnterAlternateScreen)?;
    LEASE.took(ALTERNATE);
    fault(PASTE)?;
    execute!(io::stdout(), EnableBracketedPaste)?;
    LEASE.took(PASTE);
    fault(SURFACE)?;
    let screen = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    LEASE.took(SURFACE);
    Ok(ManuallyDrop::new(screen))
}

/// Gives the terminal back, once, from any caller: normal end, error, stage 3, a signal, a lost terminal,
/// a failed draw, or the panic hook. A caller that finds it restoring or restored returns at once.
pub(crate) fn restore() -> io::Result<()> {
    let Some(taken) = LEASE.claim() else {
        return Ok(());
    };
    let result = undo(taken, terminal::disable_raw_mode, || io::stdout().lock());
    LEASE.release();
    result
}

/// Undoes the steps in `taken`, each attempted whatever the others did; the first error is returned.
///
/// Raw mode goes first, as Ratatui 0.30.2's own `try_restore` does ("it has more side effects than leaving
/// the alternate screen"). It is a `tcsetattr`, which never waits on output, so the shell gets its echo,
/// line editing and Ctrl-C back even when a write below blocks on a stuck terminal, and anything printed
/// afterwards (the panic report) has its line endings again. Only then is the stdout lock taken, so the
/// escape sequences never land inside a frame another thread is writing (see [`draw`]).
fn undo<W: Write>(taken: u8, raw_off: impl FnOnce() -> io::Result<()>, out: impl FnOnce() -> W) -> io::Result<()> {
    let mut first = Ok(());
    let mut note = |r: io::Result<()>| {
        if first.is_ok() {
            first = r;
        }
    };
    if taken & RAW != 0 {
        note(raw_off());
    }
    if taken & (ALTERNATE | PASTE | SURFACE) != 0 {
        let mut out = out();
        if taken & PASTE != 0 {
            note(execute!(out, DisableBracketedPaste));
        }
        if taken & ALTERNATE != 0 {
            note(execute!(out, LeaveAlternateScreen));
        }
        if taken & SURFACE != 0 {
            note(execute!(out, Show));
            note(execute!(out, ResetColor));
        }
    }
    first
}

/// Installs, once and before the terminal is taken, the hook that gives the terminal back before the
/// previous hook reports the panic. It runs on the panicking thread, before the release build's abort.
pub(crate) fn install_panic_hook() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = restore();
            previous(info);
        }));
    });
}

/// Draws a frame unless the terminal is being given back. The check and the frame's writes happen under
/// the stdout lock that restoration also takes, so no byte of a frame lands after restoration began.
pub(crate) fn draw(screen: &mut Screen, render: impl FnOnce(&mut Frame)) -> io::Result<()> {
    let _out = io::stdout().lock();
    if !LEASE.is_active() {
        return Ok(());
    }
    draw_fault()?;
    screen.draw(render)?;
    Ok(())
}

/// Ctrl-L: clears the screen and forgets the last frame, so the next draw repaints every cell. Not
/// `Terminal::clear`, which asks the terminal where the cursor is and so reads beside `EventStream`.
pub(crate) fn clear(screen: &mut Screen) -> io::Result<()> {
    let _out = io::stdout().lock();
    if !LEASE.is_active() {
        return Ok(());
    }
    let size = screen.size()?;
    screen.resize(Rect::new(0, 0, size.width, size.height))
}

/// Debug builds only: a failure injected for the PTY tests (plan §J 8), chosen by the debug fixture's
/// scenario. A build without debug assertions has neither the faults nor the means to set them.
#[cfg(debug_assertions)]
#[derive(Clone, Copy, Debug)]
pub(crate) enum Fault {
    /// Entering the alternate screen fails: only raw mode has been taken.
    AlternateScreen,
    /// Making the drawing surface fails: raw mode, the alternate screen and bracketed paste have been taken.
    Surface,
    /// The second draw fails.
    SecondDraw,
}

#[cfg(debug_assertions)]
static FAIL_STEP: AtomicU8 = AtomicU8::new(0);
#[cfg(debug_assertions)]
static DRAWS_BEFORE_FAILURE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

#[cfg(debug_assertions)]
pub(crate) fn inject(fault: Fault) {
    match fault {
        Fault::AlternateScreen => FAIL_STEP.store(ALTERNATE, Ordering::Relaxed),
        Fault::Surface => FAIL_STEP.store(SURFACE, Ordering::Relaxed),
        Fault::SecondDraw => DRAWS_BEFORE_FAILURE.store(2, Ordering::Relaxed),
    }
}

fn fault(step: u8) -> io::Result<()> {
    #[cfg(debug_assertions)]
    if FAIL_STEP.load(Ordering::Relaxed) == step {
        return Err(io::Error::other("a failure injected by the debug fixture"));
    }
    let _ = step;
    Ok(())
}

fn draw_fault() -> io::Result<()> {
    #[cfg(debug_assertions)]
    if DRAWS_BEFORE_FAILURE.load(Ordering::Relaxed) > 0 && DRAWS_BEFORE_FAILURE.fetch_sub(1, Ordering::Relaxed) == 1 {
        return Err(io::Error::other("a failure injected by the debug fixture"));
    }
    Ok(())
}

/// The lease's state and restoration order, on a lease and writers of the test's own: the test runner's
/// terminal is never touched. The real terminal is the PTY suite's (`tests/terminal.rs`).
#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::sync::{Arc, Barrier};

    const ALL: u8 = RAW | ALTERNATE | PASTE | SURFACE;

    /// What `undo` did, in order: "raw", then "out" when it took the writer, then the bytes written.
    fn undone(taken: u8) -> (Vec<&'static str>, String) {
        let log = RefCell::new(Vec::new());
        let mut bytes = Vec::new();
        undo(
            taken,
            || {
                log.borrow_mut().push("raw");
                Ok(())
            },
            || {
                log.borrow_mut().push("out");
                &mut bytes
            },
        )
        .unwrap();
        (log.into_inner(), String::from_utf8(bytes).unwrap())
    }

    #[test]
    fn restoring_a_terminal_never_taken_does_nothing() {
        let lease = Lease::new();
        assert_eq!(lease.claim(), None);
        assert_eq!(lease.state.load(Ordering::Acquire), INACTIVE, "claiming nothing changes nothing");
        assert!(!lease.is_active());
    }

    #[test]
    fn a_partial_acquisition_undoes_only_its_steps() {
        let lease = Lease::new();
        assert!(lease.begin());
        lease.took(RAW);
        assert_eq!(lease.claim(), Some(RAW), "raw mode taken, the alternate screen failed");
        assert_eq!(undone(RAW), (vec!["raw"], String::new()), "nothing is written: no alternate screen to leave");

        let lease = Lease::new();
        assert!(lease.begin());
        lease.took(RAW);
        lease.took(ALTERNATE);
        lease.took(PASTE);
        assert_eq!(lease.claim(), Some(RAW | ALTERNATE | PASTE), "the drawing surface failed");
        assert_eq!(undone(RAW | ALTERNATE | PASTE), (vec!["raw", "out"], "\x1b[?2004l\x1b[?1049l".to_string()));
    }

    #[test]
    fn restoration_turns_raw_mode_off_first_then_writes_every_step() {
        assert_eq!(undone(ALL), (vec!["raw", "out"], "\x1b[?2004l\x1b[?1049l\x1b[?25h\x1b[0m".to_string()));
    }

    #[test]
    fn active_then_restoring_then_restored_and_a_second_caller_does_nothing() {
        let lease = Lease::new();
        assert!(lease.begin());
        assert!(!lease.begin(), "the terminal is taken once");
        lease.took(ALL);
        assert!(lease.activate());
        assert!(lease.is_active());
        assert_eq!(lease.claim(), Some(ALL));
        assert_eq!(lease.state.load(Ordering::Acquire), RESTORING);
        assert!(!lease.is_active(), "no frame is drawn once restoration began");
        assert_eq!(lease.claim(), None, "a caller during restoration returns at once");
        lease.release();
        assert_eq!(lease.state.load(Ordering::Acquire), RESTORED);
        assert_eq!(lease.claim(), None, "a caller after restoration returns at once");
        assert!(!lease.begin(), "and the terminal is not taken again");
    }

    /// A restoration claimed while the terminal is still being taken (a panic hook, say) wins: the taker
    /// finds it cannot become active.
    #[test]
    fn a_restoration_during_entry_stops_the_entry() {
        let lease = Lease::new();
        assert!(lease.begin());
        lease.took(RAW);
        assert_eq!(lease.claim(), Some(RAW));
        assert!(!lease.activate());
    }

    #[test]
    fn concurrent_callers_cannot_both_restore() {
        for _ in 0..200 {
            let lease = Arc::new(Lease::new());
            assert!(lease.begin());
            lease.took(ALL);
            assert!(lease.activate());
            let start = Arc::new(Barrier::new(8));
            let claims: Vec<_> = (0..8)
                .map(|_| {
                    let (lease, start) = (lease.clone(), start.clone());
                    std::thread::spawn(move || {
                        start.wait();
                        lease.claim()
                    })
                })
                .collect();
            let won: Vec<_> = claims.into_iter().filter_map(|t| t.join().unwrap()).collect();
            assert_eq!(won, vec![ALL], "exactly one caller owns the restoration");
        }
    }

    /// Every step is attempted when the ones before it fail, and the first failure is the one returned.
    #[test]
    fn every_step_is_attempted_after_a_failure() {
        struct Broken(usize);
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                self.0 += 1;
                Err(io::Error::other("gone"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut broken = Broken(0);
        let result = undo(ALL, || Err(io::Error::other("raw")), || &mut broken);
        assert_eq!(result.unwrap_err().to_string(), "raw");
        assert_eq!(broken.0, 4, "paste, alternate screen, cursor and colour each tried");
    }
}
