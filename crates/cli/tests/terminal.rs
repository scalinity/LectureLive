//! The terminal's lifecycle in a real pseudo-terminal (M7 plan §J PTY, Task 5): what only a terminal can
//! prove. Every child is the debug build's scripted session (`LECTURELIVE_CLI_FIXTURE`) in a temporary
//! folder, with `HOME` set to an empty scratch directory and nothing else in its environment: no key, no
//! microphone, no audio, and the person's application data is neither read nor written.
//!
//! The screen is drawn as diffs, so the tests look only for what is written whole: the mode sequences, a
//! single word, or text styled as one run (the phase word). Nothing here parses the layout.

mod support {
    pub mod pty;
}

use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::time::Duration;

use rustix::process::Signal;
use rustix::termios::{LocalModes, SpecialCodeIndex, Termios};
use support::pty::{find, visible, Pty};

const ENTER_ALTERNATE: &str = "\x1b[?1049h";
const LEAVE_ALTERNATE: &str = "\x1b[?1049l";
const PASTE_ON: &str = "\x1b[?2004h";
const PASTE_OFF: &str = "\x1b[?2004l";
const SHOW_CURSOR: &str = "\x1b[?25h";
const CTRL_C: &[u8] = b"\x03";

/// Start-up (folder, `prepare`) to the first frame; generous for a loaded machine.
const START: Duration = Duration::from_secs(30);
const SOON: Duration = Duration::from_secs(10);
/// Task 2's stop controller takes a Ctrl-C key as the next stage only 2 s after the stage before and
/// 300 ms after the last Ctrl-C: this wait is that dwell, deliberately, plus a margin.
const STOP_DWELL: Duration = Duration::from_millis(2300);

struct Lecture {
    pty: Pty,
    home: PathBuf,
    /// The terminal's modes before the child ran.
    before: Termios,
    _tmp: tempfile::TempDir,
}

/// `lecture --tui` on a fresh folder, in a `cols` × `rows` terminal, as the fixture `scenario`.
fn tui(scenario: &str, extra: &[&str], cols: u16, rows: u16) -> Lecture {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir(&home).unwrap();
    let dir = tmp.path().join("Week 01 — Fixture");
    let mut pty = Pty::open(cols, rows);
    let before = pty.termios();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_lecturelive"));
    cmd.args(["lecture", "--tui", "--dir", dir.to_str().unwrap(), "--course", "Fixture Course"])
        .args(extra)
        .env_clear()
        .env("HOME", &home)
        .env("PATH", "/usr/bin:/bin")
        .env("TERM", "xterm-256color")
        .env("LECTURELIVE_CLI_FIXTURE", scenario)
        .current_dir(&home);
    pty.spawn(cmd);
    Lecture { pty, home, before, _tmp: tmp }
}

impl Lecture {
    /// The first frame is up: the terminal is taken and the scripted session runs.
    fn listening(&mut self) -> usize {
        let enter = self.pty.wait_for(ENTER_ALTERNATE, 0, START);
        let paste = self.pty.wait_for(PASTE_ON, enter, SOON);
        let listening = self.pty.wait_for("Listening", paste, SOON);
        let during = self.pty.termios();
        assert!(!during.local_modes.intersects(LocalModes::ICANON | LocalModes::ECHO | LocalModes::ISIG), "raw mode while the TUI runs: {:?}", during.local_modes);
        listening
    }

    fn out(&self) -> &[u8] {
        &self.pty.out
    }

    fn at(&self, needle: &str, from: usize) -> usize {
        find(self.out(), needle.as_bytes(), from).unwrap_or_else(|| panic!("{needle:?} was not written after byte {from}:\n{}", visible(self.out())))
    }

    /// The terminal is given back, in order, after byte `from`: bracketed paste off, the alternate screen
    /// left, the cursor shown; the modes are the ones from before; `HOME` is untouched. Returns the index
    /// just past the restoration, where the ordinary screen's output begins.
    fn restored(&self, from: usize) -> usize {
        let paste = self.at(PASTE_OFF, from);
        let leave = self.at(LEAVE_ALTERNATE, paste);
        let show = self.at(SHOW_CURSOR, leave);
        same_modes(&self.before, &self.pty.termios());
        assert!(untouched(&self.home), "application data was touched: {:?}", std::fs::read_dir(&self.home).unwrap().flatten().map(|e| e.path()).collect::<Vec<_>>());
        show + SHOW_CURSOR.len()
    }
}

fn untouched(home: &Path) -> bool {
    std::fs::read_dir(home).unwrap().next().is_none()
}

/// The modes a shell depends on, and the characters it reads specially, are as they were.
fn same_modes(before: &Termios, after: &Termios) {
    assert_eq!(before.local_modes, after.local_modes);
    assert_eq!(before.input_modes, after.input_modes);
    assert_eq!(before.output_modes, after.output_modes);
    assert_eq!(before.control_modes, after.control_modes);
    assert_eq!(before.input_speed(), after.input_speed());
    assert_eq!(before.output_speed(), after.output_speed());
    for i in [SpecialCodeIndex::VINTR, SpecialCodeIndex::VEOF, SpecialCodeIndex::VERASE, SpecialCodeIndex::VKILL, SpecialCodeIndex::VSUSP, SpecialCodeIndex::VMIN, SpecialCodeIndex::VTIME] {
        assert_eq!(before.special_codes[i], after.special_codes[i], "{i:?}");
    }
    assert!(after.local_modes.contains(LocalModes::ICANON | LocalModes::ECHO | LocalModes::ISIG), "{:?}", after.local_modes);
}

fn code(status: ExitStatus, l: &Lecture) -> Option<i32> {
    eprintln!("exit {status:?}\n{}", visible(l.out()));
    status.code()
}

/// The harness itself (plan Task 5 "tests first"): `stty -a` in the child sees the size the test set and a
/// terminal in its usual modes, and a mode the child changes is read back after it has ended.
#[test]
fn the_harness_gives_the_child_a_terminal() {
    let mut pty = Pty::open(93, 31);
    let before = pty.termios();
    assert!(before.local_modes.contains(LocalModes::ICANON | LocalModes::ECHO | LocalModes::ISIG), "{:?}", before.local_modes);
    let mut sh = Command::new("/bin/sh");
    sh.args(["-c", "/bin/stty -a; /bin/stty raw -echo; echo done"]).env_clear();
    pty.spawn(sh);
    assert!(pty.wait(SOON).success());
    let out = String::from_utf8_lossy(&pty.out).into_owned();
    assert!(out.contains("31 rows; 93 columns"), "{out}");
    assert!(out.contains("done"), "{out}");
    let after = pty.termios();
    assert!(!after.local_modes.contains(LocalModes::ICANON), "the child's raw mode is read back after it ended: {:?}", after.local_modes);
}

/// PTY 1: the lecture ends by itself (`--secs`). Raw mode, the alternate screen and bracketed paste are
/// taken, then given back, and the end summary is printed after the alternate screen is left.
#[test]
fn a_lecture_that_ends_gives_the_terminal_back_before_its_summary() {
    let mut l = tui("quiet", &["--secs", "2"], 100, 30);
    let prepared = l.pty.wait_for("listening on the scripted session", 0, START);
    let listening = l.listening();
    assert!(prepared < l.at(ENTER_ALTERNATE, 0), "the start-up lines print before the terminal is taken");
    let status = l.pty.wait(SOON);
    assert_eq!(code(status, &l), Some(0));
    let after = l.restored(listening);
    l.at("saved", after);
    l.at("spent on this lecture today", after);
}

/// PTY 2: Ctrl-C typed as a byte (raw mode: a key, not SIGINT) takes the stop through its three stages,
/// each after the dwell; the third gives the terminal back, then says so, and exits 130.
#[test]
fn three_ctrl_c_keys_stop_then_stop_waiting_then_quit_after_restoring() {
    let mut l = tui("slow-stop", &[], 100, 30);
    let listening = l.listening();
    l.pty.write(CTRL_C);
    let stopping = l.pty.wait_for("Stopping", listening, SOON);
    std::thread::sleep(STOP_DWELL);
    l.pty.write(CTRL_C);
    let waiting = l.pty.wait_for("Stop waiting", stopping, SOON);
    std::thread::sleep(STOP_DWELL);
    l.pty.write(CTRL_C);
    let status = l.pty.wait(SOON);
    assert_eq!(code(status, &l), Some(130));
    let after = l.restored(waiting);
    l.at("Stopped at once; the next session in this folder picks up what was left.", after);
}

/// PTY 3: a held Ctrl-C (ten bytes 30 ms apart, as a key repeat sends them) stops the lecture and nothing
/// more; the lecture goes on stopping until the test ends it.
#[test]
fn a_held_ctrl_c_only_stops() {
    let mut l = tui("slow-stop", &[], 100, 30);
    let listening = l.listening();
    for _ in 0..10 {
        l.pty.write(CTRL_C);
        std::thread::sleep(Duration::from_millis(30)); // a key repeat's pace
    }
    let stopping = l.pty.wait_for("Stopping", listening, SOON);
    // The repeats were read and refused, with the reason on screen.
    l.pty.wait_for("moment,", stopping, SOON);
    // Past the dwell since the first byte: had a repeat counted, stage 2 would be showing by now.
    std::thread::sleep(STOP_DWELL);
    assert!(find(l.out(), b"Stop waiting", listening).is_none(), "a held key escalated:\n{}", visible(l.out()));
    assert!(find(l.out(), LEAVE_ALTERNATE.as_bytes(), listening).is_none(), "the TUI is still up");
    l.pty.signal(Signal::TERM);
    let status = l.pty.wait(SOON);
    assert_eq!(code(status, &l), Some(143), "alive until the test ended it");
    l.restored(stopping);
}

/// PTY 4: SIGTERM gives the terminal back, says so, and exits 143; no graceful stop is attempted.
#[test]
fn sigterm_restores_then_exits_143() {
    let mut l = tui("quiet", &[], 100, 30);
    let listening = l.listening();
    l.pty.signal(Signal::TERM);
    let status = l.pty.wait(SOON);
    assert_eq!(code(status, &l), Some(143));
    let after = l.restored(listening);
    l.at("Terminated;", after);
    assert!(find(l.out(), b"Stopping", listening).is_none(), "SIGTERM is not a stop");
}

/// PTY 4: SIGHUP attempts restoration (the terminal may be gone) and exits 129.
#[test]
fn sighup_restores_then_exits_129() {
    let mut l = tui("quiet", &[], 100, 30);
    let listening = l.listening();
    l.pty.signal(Signal::HUP);
    let status = l.pty.wait(SOON);
    assert_eq!(code(status, &l), Some(129));
    l.restored(listening);
    assert!(find(l.out(), b"Stopping", listening).is_none(), "SIGHUP is not a stop");
}

/// PTY 5: a panic that cannot unwind (the release build's `panic = "abort"`, reproduced in the dev build by
/// an `extern "C"` boundary in the scripted session): the hook gives the terminal back, then the panic is
/// reported, then the process aborts.
#[test]
fn a_panic_that_aborts_gives_the_terminal_back_first() {
    let mut l = tui("panic", &[], 100, 30);
    let listening = l.listening();
    let status = l.pty.wait(SOON);
    eprintln!("exit {status:?}\n{}", visible(l.out()));
    assert!(status.code().is_none() && status.signal().is_some(), "the process aborted: {status:?}");
    let after = l.restored(listening);
    l.at("the scripted panic", after);
}

/// PTY 6: resizing from wide to tiny and back redraws at each size (the phase word is written again after
/// each full repaint) and the lecture ends normally.
#[test]
fn resizing_redraws_and_the_lecture_goes_on() {
    let mut l = tui("quiet", &["--secs", "5"], 140, 40);
    let mut at = l.listening();
    for (cols, rows) in [(80, 24), (40, 8), (110, 32)] {
        l.pty.resize(cols, rows);
        at = l.pty.wait_for("Listening", at + 1, SOON);
    }
    let status = l.pty.wait(SOON);
    assert_eq!(code(status, &l), Some(0));
    let after = l.restored(at);
    l.at("saved", after);
}

/// PTY 8: taking the terminal fails at its last step, after raw mode, the alternate screen and bracketed
/// paste: each is given back and `--tui` exits 1 with the reason, with nothing drawn and nothing started.
#[test]
fn a_failure_while_taking_the_terminal_gives_back_what_was_taken() {
    let mut l = tui("init-fail", &[], 100, 30);
    let status = l.pty.wait(START);
    assert_eq!(code(status, &l), Some(1));
    let enter = l.at(ENTER_ALTERNATE, 0);
    let paste = l.at(PASTE_ON, enter);
    let leave = l.at(LEAVE_ALTERNATE, l.at(PASTE_OFF, paste));
    assert!(find(l.out(), b"\x1b[?25", 0).is_none(), "no drawing surface, so no cursor was hidden and none is shown");
    assert!(find(l.out(), b"Listening", 0).is_none(), "nothing was drawn");
    same_modes(&l.before, &l.pty.termios());
    assert!(untouched(&l.home));
    l.at("Error: the terminal could not be taken over (a failure injected by the debug fixture)", leave);
}

/// PTY 8: taking the terminal fails right after raw mode: raw mode is given back and nothing is written
/// for the steps never taken, so no alternate-screen or paste sequence reaches the terminal.
#[test]
fn a_failure_after_raw_mode_writes_nothing_it_did_not_take() {
    let mut l = tui("init-fail-raw", &[], 100, 30);
    let status = l.pty.wait(START);
    assert_eq!(code(status, &l), Some(1));
    for never in ["\x1b[?1049", "\x1b[?2004", "\x1b[?25"] {
        assert!(find(l.out(), never.as_bytes(), 0).is_none(), "{never:?} was written:\n{}", visible(l.out()));
    }
    same_modes(&l.before, &l.pty.termios());
    assert!(untouched(&l.home));
    l.at("Error: the terminal could not be taken over (a failure injected by the debug fixture)", 0);
}

/// A draw that fails gives the terminal back before the error is printed, and exits 1.
#[test]
fn a_failed_draw_gives_the_terminal_back_before_the_error() {
    let mut l = tui("draw-fail", &[], 100, 30);
    let listening = l.listening();
    let status = l.pty.wait(SOON);
    assert_eq!(code(status, &l), Some(1));
    let after = l.restored(listening);
    l.at("Error: the terminal stopped accepting output: a failure injected by the debug fixture; the next session in this folder picks up what was left", after);
}
