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
const MOUSE_ON: &str = "\x1b[?1000h\x1b[?1002h\x1b[?1006h";
const MOUSE_OFF: &str = "\x1b[?1006l\x1b[?1002l\x1b[?1000l";
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
    /// The lecture folder, in the temporary directory.
    dir: PathBuf,
    /// The terminal's modes before the child ran.
    before: Termios,
    _tmp: tempfile::TempDir,
}

/// `lecture` with `frontend` before the folder arguments, in a `cols` × `rows` terminal, as the
/// fixture `scenario`. `tui` passes `--tui`, so the terminal frontend is asked for explicitly;
/// `auto` passes nothing, and the command decides for itself which frontend the terminal allows.
fn lecture(frontend: &[&str], scenario: &str, extra: &[&str], cols: u16, rows: u16) -> Lecture {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir(&home).unwrap();
    let dir = tmp.path().join("Week 01 — Fixture");
    let mut pty = Pty::open(cols, rows);
    let before = pty.termios();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_lecturelive"));
    cmd.args(["lecture"])
        .args(frontend)
        .args(["--dir", dir.to_str().unwrap(), "--course", "Fixture Course"])
        .args(extra)
        .env_clear()
        .env("HOME", &home)
        .env("PATH", "/usr/bin:/bin")
        .env("TERM", "xterm-256color")
        .env("LECTURELIVE_CLI_FIXTURE", scenario)
        .current_dir(&home);
    pty.spawn(cmd);
    Lecture { pty, home, dir, before, _tmp: tmp }
}

/// `lecture --tui` on a fresh folder: the terminal frontend was asked for by name.
fn tui(scenario: &str, extra: &[&str], cols: u16, rows: u16) -> Lecture {
    lecture(&["--tui"], scenario, extra, cols, rows)
}

/// `lecture` with no frontend flag on a fresh folder: the same terminal, the same folder, the same
/// scripted session, and the process left to choose.
fn auto(scenario: &str, extra: &[&str], cols: u16, rows: u16) -> Lecture {
    lecture(&[], scenario, extra, cols, rows)
}

impl Lecture {
    /// The first frame is up: the terminal is taken and the scripted session runs.
    fn listening(&mut self) -> usize {
        let enter = self.pty.wait_for(ENTER_ALTERNATE, 0, START);
        let paste = self.pty.wait_for(PASTE_ON, enter, SOON);
        let mouse = self.pty.wait_for(MOUSE_ON, paste, SOON);
        let listening = self.pty.wait_for("Listening", mouse, SOON);
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

    /// The terminal is given back, in order, after byte `from`: mouse capture off, bracketed paste off,
    /// the alternate screen left, the cursor shown; the modes are the ones from before; `HOME` is untouched.
    /// Returns the index just past the restoration, where the ordinary screen's output begins.
    fn restored(&self, from: usize) -> usize {
        let mouse = self.at(MOUSE_OFF, from);
        let paste = self.at(PASTE_OFF, mouse);
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
/// macOS can refuse a `/dev/ptmx` open with XNU's kernel-private `EREDRIVEOPEN` (−6) when its own
/// retries of the tty allocation race run out, as parallel PTY tests make happen. Only that error,
/// and only the allocation, is tried again, a bounded number of times; any other error fails at once.
#[test]
fn only_the_kernels_pty_allocation_race_is_tried_again() {
    use rustix::io::Errno;
    let raced = || Errno::from_raw_os_error(-6);
    let mut calls = 0;
    assert_eq!(support::pty::allocate(|| { calls += 1; if calls < 3 { Err(raced()) } else { Ok(7) } }), Ok(7), "a cleared race allocates");
    assert_eq!(calls, 3);
    calls = 0;
    assert_eq!(support::pty::allocate(|| { calls += 1; Err::<(), _>(Errno::NOENT) }), Err(Errno::NOENT));
    assert_eq!(calls, 1, "any other error is not tried again");
    calls = 0;
    assert_eq!(support::pty::allocate(|| { calls += 1; Err::<(), _>(raced()) }), Err(raced()), "a race that never clears still fails");
    assert!((2..=10).contains(&calls), "bounded: {calls} attempts");
}

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

/// PTY 1: the lecture ends by itself (`--secs`). Raw mode, the alternate screen, bracketed paste and
/// mouse capture are taken, then given back, and the end summary is printed after the alternate screen is left.
#[test]
fn a_lecture_that_ends_gives_the_terminal_back_before_its_summary() {
    let mut l = tui("quiet", &["--secs", "2"], 100, 30);
    let prepared = l.pty.wait_for("listening on the scripted session", 0, START);
    let listening = l.listening();
    assert!(prepared < l.at(ENTER_ALTERNATE, 0), "the start-up lines print before the terminal is taken");
    let status = l.pty.wait(SOON);
    assert_eq!(code(status, &l), Some(0));
    // The smaller capture mode: no any-event tracking (passive motion would only wake the reactor)
    // and no alternate scroll (the wheel must not masquerade as cursor keys).
    for never in ["\x1b[?1003", "\x1b[?1007", "\x1b[?1015"] {
        assert!(find(l.out(), never.as_bytes(), 0).is_none(), "{never:?} was written:\n{}", visible(l.out()));
    }
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
    // The repeats were read and refused, with the reason on screen (Task 6's notice line shares the
    // row with the start-up records, so the wait is on a run that is written whole).
    l.pty.wait_for("Ctrl-C again to stop waiting", stopping, SOON);
    // Past the dwell since the first byte: had a repeat counted, stage 2 would be showing by now.
    std::thread::sleep(STOP_DWELL);
    assert!(find(l.out(), b"Stop waiting", listening).is_none(), "a held key escalated:\n{}", visible(l.out()));
    assert!(find(l.out(), LEAVE_ALTERNATE.as_bytes(), listening).is_none(), "the TUI is still up");
    l.pty.signal(Signal::TERM);
    let status = l.pty.wait(SOON);
    assert_eq!(code(status, &l), Some(143), "alive until the test ended it");
    l.restored(stopping);
}

/// PTY 4: SIGTERM (`kill`, an app that quits other apps) tells the lecture to quit and waits for it, gives the
/// terminal back, says the recording is saved, and exits 143. The scripted session logs what it was sent: a quit, not
/// a stop, and the last snapshot, which writes for over a second, never happens (spec §9.6).
#[test]
fn sigterm_quits_the_lecture_then_restores_and_exits_143() {
    let mut l = tui("ops", &[], 100, 30);
    let listening = l.listening();
    l.pty.signal(Signal::TERM);
    let status = l.pty.wait(SOON);
    assert_eq!(code(status, &l), Some(143));
    assert_eq!(commands(&l), ["quit"], "the lecture was told to quit, once");
    let after = l.restored(listening);
    l.at("Terminated; the recording and transcript are saved", after);
    assert!(find(l.out(), b"Stopping", listening).is_none(), "a quit is not the stop");
    assert!(find(l.out(), b"the last of the lecture", listening).is_none(), "and takes no last snapshot");
}

/// PTY 4: SIGHUP (the terminal closed) does the same: the lecture is told to quit and waited for, restoration is
/// attempted (the terminal may be gone), and the status is 129.
#[test]
fn sighup_quits_the_lecture_then_restores_and_exits_129() {
    let mut l = tui("ops", &[], 100, 30);
    let listening = l.listening();
    l.pty.signal(Signal::HUP);
    let status = l.pty.wait(SOON);
    assert_eq!(code(status, &l), Some(129));
    assert_eq!(commands(&l), ["quit"], "the lecture was told to quit, once");
    l.restored(listening);
    assert!(find(l.out(), b"Stopping", listening).is_none(), "a quit is not the stop");
    assert!(find(l.out(), b"the last of the lecture", listening).is_none(), "and takes no last snapshot");
}

/// A quit while the lecture is already stopping: the person pressed Ctrl-C and the stop is writing its last
/// snapshot when the signal comes. The quit cuts it short (core's does too), instead of waiting out the drain.
#[test]
fn sigterm_while_stopping_ends_the_stop_at_once() {
    let mut l = tui("slow-stop", &[], 100, 30);
    let listening = l.listening();
    l.pty.write(CTRL_C);
    let stopping = l.pty.wait_for("Stopping", listening, SOON);
    l.pty.signal(Signal::TERM);
    // the scripted last snapshot sleeps a minute; the grace is eight seconds; a quit ends it at once
    let started = std::time::Instant::now();
    let status = l.pty.wait(SOON);
    assert_eq!(code(status, &l), Some(143));
    assert!(started.elapsed() < Duration::from_secs(5), "the quit did not wait out the stop: {:?}", started.elapsed());
    let after = l.restored(stopping);
    l.at("Terminated; the recording and transcript are saved", after);
}

/// Pause (spec §9.6) in one real TUI: `pause` and Enter sends one pause; Ctrl-P, a real 0x10 byte, resumes it, and
/// Ctrl-P again, past the held-key dwell, pauses again; a typed `resume` ends it; a half-typed hint is untouched by the
/// key. The fixture's own log is the count. The header's words are the view tests' (the screen is drawn as diffs, and
/// "Paused" over "Listening" shares a letter, so it is not written whole); here it is proved that the commands arrive
/// once each, the TUI goes on drawing and typing through the events they cause, and the terminal is given back.
#[test]
fn pause_is_typed_or_a_key_and_each_is_one_command() {
    let mut l = tui("ops", &[], 110, 32);
    let listening = l.listening();
    l.pty.write(b"pause\r");
    assert_eq!(wait_commands(&l, 1), ["pause"]);
    std::thread::sleep(ENTER_GAP); // the view follows core's event, which the key then reads: let it arrive
    l.pty.write(b"focus on"); // half a hint, then the key
    l.pty.write(b"\x10"); // Ctrl-P
    assert_eq!(wait_commands(&l, 2)[1], "resume");
    std::thread::sleep(STOP_DWELL); // past a held key's dwell: a new press
    l.pty.write(b"\x10");
    assert_eq!(wait_commands(&l, 3)[2], "pause");
    // a held key's repeats: nothing more, whatever the dwell
    for _ in 0..10 {
        l.pty.write(b"\x10");
        std::thread::sleep(Duration::from_millis(30));
    }
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(commands(&l).len(), 3, "a held Ctrl-P is one press: {:?}", commands(&l));
    // a typed resume: Ctrl-U clears the half-typed hint the key left alone, then the command
    std::thread::sleep(ENTER_GAP);
    l.pty.write(b"\x15resume\r");
    assert_eq!(wait_commands(&l, 4)[3], "resume");
    l.pty.write(CTRL_C);
    let status = l.pty.wait(Duration::from_secs(20));
    assert_eq!(code(status, &l), Some(0));
    assert_eq!(commands(&l), ["pause", "resume", "pause", "resume", "stop"]);
    let after = l.restored(listening);
    l.at("saved", after);
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

/// Task 6 (plan §J's "a fixture PTY run shows STT words and the gap count updating"): the session
/// view is live — the connection's words, a gap and its recovery reaching the notice line, the spend
/// sample — while events keep draining and the background reads and samples change no terminal
/// behaviour: the lifecycle ends exactly as Task 5's.
#[test]
fn the_session_view_shows_the_scripted_lecture_live() {
    let mut l = tui("quiet", &["--secs", "8"], 110, 30);
    let listening = l.listening();
    let stt = l.pty.wait_for("transcribing", listening, SOON);
    // An empty day's ledger sums to -0.0; the header says $0.00 (the plain end summary's golden
    // keeps its own bytes).
    let spend = l.pty.wait_for("$0.00 today", stt, SOON);
    let gap = l.pty.wait_for("gap", spend, SOON); // t ≈ 2 s: a transcript gap opens, then recovery resolves it
    let recovered = l.pty.wait_for("recovered", gap, SOON);
    l.pty.wait_for("reconnect", recovered, SOON); // t ≈ 4 s: the connection dips and comes back
    let status = l.pty.wait(SOON);
    assert_eq!(code(status, &l), Some(0));
    let after = l.restored(listening);
    l.at("saved", after);
}

/// PTY 8: taking the terminal fails at its last step, after raw mode, the alternate screen, bracketed
/// paste and mouse capture: each is given back and `--tui` exits 1 with the reason, with nothing drawn
/// and nothing started.
#[test]
fn a_failure_while_taking_the_terminal_gives_back_what_was_taken() {
    let mut l = tui("init-fail", &[], 100, 30);
    let status = l.pty.wait(START);
    assert_eq!(code(status, &l), Some(1));
    let enter = l.at(ENTER_ALTERNATE, 0);
    let paste = l.at(PASTE_ON, enter);
    let mouse = l.at(MOUSE_ON, paste);
    let leave = l.at(LEAVE_ALTERNATE, l.at(PASTE_OFF, l.at(MOUSE_OFF, mouse)));
    assert!(find(l.out(), b"\x1b[?25", 0).is_none(), "no drawing surface, so no cursor was hidden and none is shown");
    assert!(find(l.out(), b"Listening", 0).is_none(), "nothing was drawn");
    same_modes(&l.before, &l.pty.termios());
    assert!(untouched(&l.home));
    l.at("Error: the terminal could not be taken over (a failure injected by the debug fixture)", leave);
}

/// Taking the terminal fails at mouse capture, after raw mode, the alternate screen and bracketed
/// paste: those are given back, no mouse sequence is written either way, and `--tui` exits 1 with the
/// reason, with nothing drawn and nothing started.
#[test]
fn a_failure_at_mouse_capture_restores_without_touching_the_mouse() {
    let mut l = tui("init-fail-mouse", &[], 100, 30);
    let status = l.pty.wait(START);
    assert_eq!(code(status, &l), Some(1));
    let enter = l.at(ENTER_ALTERNATE, 0);
    let paste = l.at(PASTE_ON, enter);
    for never in ["\x1b[?1000", "\x1b[?1002", "\x1b[?1006"] {
        assert!(find(l.out(), never.as_bytes(), 0).is_none(), "{never:?} was written:\n{}", visible(l.out()));
    }
    let leave = l.at(LEAVE_ALTERNATE, l.at(PASTE_OFF, paste));
    assert!(find(l.out(), b"\x1b[?25", 0).is_none(), "no drawing surface, so no cursor was hidden and none is shown");
    assert!(find(l.out(), b"Listening", 0).is_none(), "nothing was drawn");
    same_modes(&l.before, &l.pty.termios());
    assert!(untouched(&l.home));
    l.at("Error: the terminal could not be taken over (a failure injected by the debug fixture)", leave);
}

/// PTY 8: taking the terminal fails right after raw mode: raw mode is given back and nothing is written
/// for the steps never taken, so no alternate-screen, paste or mouse sequence reaches the terminal.
#[test]
fn a_failure_after_raw_mode_writes_nothing_it_did_not_take() {
    let mut l = tui("init-fail-raw", &[], 100, 30);
    let status = l.pty.wait(START);
    assert_eq!(code(status, &l), Some(1));
    for never in ["\x1b[?1049", "\x1b[?2004", "\x1b[?1000", "\x1b[?1002", "\x1b[?1006", "\x1b[?25"] {
        assert!(find(l.out(), never.as_bytes(), 0).is_none(), "{never:?} was written:\n{}", visible(l.out()));
    }
    same_modes(&l.before, &l.pty.termios());
    assert!(untouched(&l.home));
    l.at("Error: the terminal could not be taken over (a failure injected by the debug fixture)", 0);
}

/// Plan §H as built: with **no** frontend flag, a live lecture on a real terminal opens the terminal
/// UI by itself. The scripted session, the folder and the lifecycle are the ones PTY 1 already
/// proves; what is new here is only that nothing asked for the TUI and it was opened anyway, and
/// that the terminal comes back and the lecture ends as any other.
#[test]
fn a_live_lecture_opens_the_terminal_ui_with_no_flag() {
    let mut l = auto("quiet", &["--secs", "2"], 100, 30);
    let prepared = l.pty.wait_for("listening on the scripted session", 0, START);
    let listening = l.listening();
    assert!(prepared < l.at(ENTER_ALTERNATE, 0), "the start-up lines print before the terminal is taken");
    assert!(find(l.out(), b"  transcribing", 0).is_none(), "the plain adapter's own line is not what a TUI session prints");
    let status = l.pty.wait(SOON);
    assert_eq!(code(status, &l), Some(0));
    let after = l.restored(listening);
    l.at("saved", after);
    l.at("spent on this lecture today", after);
}

/// Plan §H as built, and plan §G's takeover row: a terminal frontend chosen **by itself** that
/// cannot take the terminal is not a failure. What was taken is given back, one line says so, and
/// the same lecture — same folder, same `prepare`, same one engine — continues through the plain
/// adapter to the same clean end. Nothing is prepared twice and no second session is started.
#[test]
fn an_automatic_terminal_that_cannot_be_taken_over_continues_in_plain() {
    const FALLBACK: &str = "The terminal could not be taken over (a failure injected by the debug fixture); continuing in plain mode.";
    /// Plain's end summary, which only a lecture that actually ended prints.
    const END_SUMMARY: &str = "spent on this lecture today";
    let mut l = auto("init-fail", &["--secs", "3"], 100, 30);
    let status = l.pty.wait(START);
    // The takeover really was attempted: raw mode, the alternate screen, bracketed paste and mouse
    // capture were each taken, and the failure came at the drawing surface.
    let enter = l.at(ENTER_ALTERNATE, 0);
    let paste = l.at(PASTE_ON, enter);
    let mouse = l.at(MOUSE_ON, paste);
    // All of it is given back, in order, before anything is printed.
    let leave = l.at(LEAVE_ALTERNATE, l.at(PASTE_OFF, l.at(MOUSE_OFF, mouse)));
    same_modes(&l.before, &l.pty.termios());
    assert!(find(l.out(), b"\x1b[?25", 0).is_none(), "no drawing surface, so no cursor was hidden and none is shown");
    // The one line, then the very same lecture through the plain adapter.
    let said = l.at(FALLBACK, leave);
    assert_eq!(l.out()[said..].windows(FALLBACK.len()).filter(|w| *w == FALLBACK.as_bytes()).count(), 1, "the fallback is said once");
    assert!(find(l.out(), b"Listening", 0).is_none(), "nothing was ever drawn");
    l.pty.wait_for("transcribing", said, SOON);
    // `prepare` printed its line once, and the one lecture ends once: neither ran twice.
    assert_eq!(l.out().windows("listening on the scripted session".len()).filter(|w| *w == b"listening on the scripted session").count(), 1, "prepared twice");
    assert_eq!(code(status, &l), Some(0), "a plain lecture's own exit code");
    // Restoration is already behind the fallback line above, so the plain end summary is what
    // follows on the ordinary screen, exactly as it does for any plain lecture.
    l.at("saved", said);
    l.at("spent on this lecture today", said);
    // The end summary is printed once, by one plain lecture.
    assert_eq!(l.out().windows(END_SUMMARY.len()).filter(|w| *w == END_SUMMARY.as_bytes()).count(), 1, "the session ended once");
    assert!(untouched(&l.home));
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

/// Plan Task 12's session-failure row, with the audit's hostile payload in the engine's own
/// error: the in-TUI `session failed` notice is rendered by the (cleaning) projection; the
/// terminal is restored; only then does the ordinary `Error: …` appear — carrying the failure's
/// visible words and none of the payload's sequences (the restoration's own escapes are
/// legitimate, so the assertions look only at the post-restoration error region) — and the
/// process exits 1 with the person's data untouched.
#[test]
fn a_session_failure_restores_then_errors_cleanly() {
    let mut l = tui("session-fail", &[], 100, 30);
    let listening = l.listening();
    l.pty.wait_for("session failed", listening, SOON);
    let status = l.pty.wait(SOON);
    assert_eq!(code(status, &l), Some(1));
    let after = l.restored(listening);
    let error = l.at("Error: disk full in the scripted session", after);
    for gone in ["\x1b[31m", "\x1b[2J", "\x1b]0;", "\x1b]52;", "\x1b]8;", "\x07", "\x7f"] {
        assert!(find(l.out(), gone.as_bytes(), error).is_none(), "the payload's {gone:?} survived into the final error:\n{}", visible(l.out()));
    }
    for kept in ["csi", "link", "bel", "rewritten", "del"] {
        assert!(find(l.out(), kept.as_bytes(), error).is_some(), "{kept:?} left the final error:\n{}", visible(l.out()));
    }
    assert!(untouched(&l.home), "the person's data folder was never touched");
}

/// Plan Task 12's `failures` scenario: the non-fatal walk through the failure table, in order,
/// while the lecture stays alive — each failure's own words reach the notice line, the activity
/// overlay keeps them all after the line has moved on, and the session still ends cleanly with
/// nothing of the person's data touched.
#[test]
fn the_failures_scenario_walks_the_table_and_keeps_it_inspectable() {
    let mut l = tui("failures", &["--secs", "20"], 110, 32);
    let listening = l.listening();
    // the walk, one failure every 2 s: gap → recovery failed → spend → reconnect → snapshot → page
    let mut at = l.pty.wait_for("2.0–3.0", listening, Duration::from_secs(20));
    at = l.pty.wait_for("backlog", at, SOON);
    at = l.pty.wait_for("spend", at, SOON);
    at = l.pty.wait_for("reconnecting", at, SOON);
    at = l.pty.wait_for("snapshot failed", at, SOON);
    l.pty.wait_for("page failed", at, SOON);
    // Ctrl-O: the activity keeps the walk after the line has moved on — its first frame draws
    // the whole overlay, so its rows arrive whole (which words survive the cell diff whole
    // depends on the transcript beneath; these three did, and the ring's full inventory is the
    // unit suite's to pin)
    l.pty.write(b"\x0f");
    let overlay = l.pty.wait_for("Activity", listening, SOON);
    for still_there in ["backlog", "ledger", "interrupted"] {
        assert!(find(l.out(), still_there.as_bytes(), overlay).is_some(), "{still_there:?} left the activity:\n{}", visible(l.out()));
    }
    // the lecture was alive throughout (only --secs ended it), and ends as any clean session
    let status = l.pty.wait(Duration::from_secs(20));
    assert_eq!(code(status, &l), Some(0));
    let after = l.restored(listening);
    l.at("saved", after);
    assert!(untouched(&l.home), "the person's data folder was never touched");
}

/// The `ops` fixture's command log: every command the TUI sent, one line each, in order.
fn commands(l: &Lecture) -> Vec<String> {
    std::fs::read_to_string(l.dir.join("fixture-commands.log")).unwrap_or_default().lines().map(str::to_string).collect()
}

/// Waits until the log holds `n` commands, then returns them.
fn wait_commands(l: &Lecture, n: usize) -> Vec<String> {
    let deadline = std::time::Instant::now() + SOON;
    loop {
        let got = commands(l);
        if got.len() >= n || std::time::Instant::now() >= deadline {
            assert_eq!(got.len(), n, "commands so far: {got:?}\n{}", visible(l.out()));
            return got;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// More than the 300 ms Enter debounce: the next Enter is a new press, not a held key's repeat.
const ENTER_GAP: Duration = Duration::from_millis(400);

/// Plan Task 10 (PTY scenario 7): in one real TUI, typed keys and a real bracketed paste. An empty
/// Enter sends exactly one snapshot; a typed hint and Enter one hinted snapshot; `polish` and Enter
/// one polish; a paste holding "polish", a newline and a Ctrl-C byte sends nothing — no op, no stop —
/// until a real Enter; Ctrl-X during a stalled request sends exactly one cancel. The fixture's own
/// log is the count, not the screen. The terminal is given back as ever.
#[test]
fn the_hint_line_sends_exactly_what_was_asked() {
    let mut l = tui("ops", &[], 110, 32);
    let listening = l.listening();
    l.pty.write(b"\r");
    assert_eq!(wait_commands(&l, 1), ["snapshot\t"]);
    std::thread::sleep(ENTER_GAP);
    l.pty.write("focus on treatment 漢字".as_bytes());
    l.pty.write(b"\r");
    assert_eq!(wait_commands(&l, 2)[1], "snapshot\tfocus on treatment 漢字");
    std::thread::sleep(ENTER_GAP);
    l.pty.write(b"polish\r");
    assert_eq!(wait_commands(&l, 3)[2], "polish");
    // a real bracketed paste: text, never keys
    std::thread::sleep(ENTER_GAP);
    l.pty.write(b"\x1b[200~polish\r\n\x03and more\x1b[201~");
    std::thread::sleep(Duration::from_millis(700));
    assert_eq!(commands(&l).len(), 3, "the paste sent nothing");
    assert!(find(l.out(), b"Stopping", listening).is_none(), "the pasted Ctrl-C byte is no stop");
    l.pty.write(b"\r");
    assert_eq!(wait_commands(&l, 4)[3], "snapshot\tpolish and more", "a real Enter sends the pasted text, as one line");
    // a request that writes until it is cancelled, queued behind the others
    std::thread::sleep(ENTER_GAP);
    l.pty.write(b"stall\r");
    assert_eq!(wait_commands(&l, 5)[4], "snapshot\tstall");
    let stalled = l.pty.wait_for("Stalled", listening, Duration::from_secs(20));
    l.pty.write(b"\x18"); // Ctrl-X
    assert_eq!(wait_commands(&l, 6)[5], "cancel");
    l.pty.wait_for("cancelled", stalled, SOON);
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(commands(&l).len(), 6, "one Ctrl-X, one cancel");
    // one Ctrl-C: the stop, then the drain and the last snapshot, and the terminal back
    l.pty.write(CTRL_C);
    let status = l.pty.wait(Duration::from_secs(20));
    assert_eq!(code(status, &l), Some(0));
    assert_eq!(commands(&l), ["snapshot\t", "snapshot\tfocus on treatment 漢字", "polish", "snapshot\tpolish and more", "snapshot\tstall", "cancel", "stop"]);
    let after = l.restored(listening);
    l.at("saved", after);
}

/// Plan Task 11: the `capture` fixture, whose command log is the count. Ctrl-S while watching
/// sends exactly one `capture-now` and the slide itself arrives as its own event; the safe
/// asking (the one window offered, at a size the saved selection remembers) sends exactly one
/// `bind` for that window and region; the unsafe asking (a size with no region) sends nothing;
/// Ctrl-S once stopping sends nothing. The scripted relocation is kept in the lecture folder's
/// own selections file, never the person's data folder (HOME stays empty). The waits are on
/// whole written words: the frame is a diff, so a phrase split across changed cells never
/// appears contiguously in the byte stream.
#[test]
fn the_capture_scenario_ctrl_s_sends_exactly_one_command_each() {
    // wide, so the slides column — and every capture state in it — is on screen throughout
    let mut l = tui("capture", &[], 140, 40);
    let listening = l.listening();
    // unbound first, then watching
    l.pty.wait_for("Screenshots", listening, SOON);
    let watching = l.pty.wait_for("capture now", listening, SOON);
    l.pty.write(b"\x13"); // Ctrl-S
    assert_eq!(wait_commands(&l, 1), ["capture-now"]);
    // the slide itself arrives as its own canonical event — never fabricated from the reply
    let captured = l.pty.wait_for("captured-", watching, SOON);
    // the safe asking: one offered window at 1280 × 720, a size the saved selection remembers —
    // its Ctrl-S row is on screen exactly while the offering is safe
    let watch_it = l.pty.wait_for("watch it", captured, Duration::from_secs(20));
    l.pty.write(b"\x13"); // Ctrl-S: watch it
    assert_eq!(wait_commands(&l, 2)[1], "bind\t42\tus.zoom.xos|zoom.us|Zoom Meeting|1280×720|(0.05,0.12,0.9,0.7)");
    let watching_again = l.pty.wait_for("capture now", watch_it, SOON);
    // the unsafe asking: a size with no saved region — no bind, a notice instead
    let unsafe_asking = l.pty.wait_for("999", watching_again, Duration::from_secs(20));
    l.pty.write(b"\x13");
    l.pty.wait_for("saved", unsafe_asking, SOON);
    assert_eq!(commands(&l).len(), 2);
    // the relocation (and the watching after it) recovers the pane; its save — the adapter's,
    // before any notice — is in the lecture folder's own file, never the person's data folder
    let found = l.pty.wait_for("Watching", unsafe_asking, Duration::from_secs(20));
    let kept = std::fs::read_to_string(l.dir.join(".live_notes/fixture-capture.json")).unwrap_or_default();
    assert!(kept.contains("\"Fixture Course\"") && kept.contains("\"width\": 1280"), "{kept}");
    // one Ctrl-C stops; Ctrl-S while stopping sends no capture command
    l.pty.write(CTRL_C);
    let stopped = l.pty.wait_for("Stopping", found, SOON);
    l.pty.write(b"\x13");
    let status = l.pty.wait(Duration::from_secs(20));
    assert_eq!(code(status, &l), Some(0));
    assert_eq!(commands(&l), ["capture-now", "bind\t42\tus.zoom.xos|zoom.us|Zoom Meeting|1280×720|(0.05,0.12,0.9,0.7)", "stop"], "no capture command from an unsafe asking or a stopping lecture");
    let after = l.restored(stopped);
    l.at("saved", after);
    assert!(untouched(&l.home), "the person's data folder was never reached");
}
