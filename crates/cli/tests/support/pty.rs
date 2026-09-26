//! A small pseudo-terminal for the terminal tests (M7 plan §J), on rustix: the child gets the slave as
//! its controlling terminal; the test reads everything it writes from the master, types raw bytes,
//! resizes the window, sends signals and reads the terminal's modes. Nothing here emulates a screen:
//! tests look for byte sequences in the order they were written.

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{BorrowedFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use rustix::fs::{Mode, OFlags};
use rustix::process::{Pid, Signal};
use rustix::pty::OpenptFlags;
use rustix::termios::{Termios, Winsize};

pub struct Pty {
    master: File,
    /// Held open by the test so the terminal's modes can be read before and after the child.
    slave: Option<OwnedFd>,
    chunks: Receiver<Vec<u8>>,
    /// Everything the child has written so far, in order.
    pub out: Vec<u8>,
    child: Option<Child>,
}

impl Pty {
    /// A new terminal of `cols` × `rows`, with nothing attached yet.
    pub fn open(cols: u16, rows: u16) -> Pty {
        let master = rustix::pty::openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).expect("openpt");
        rustix::pty::grantpt(&master).expect("grantpt");
        rustix::pty::unlockpt(&master).expect("unlockpt");
        let name = rustix::pty::ptsname(&master, Vec::new()).expect("ptsname");
        let slave = rustix::fs::open(name.as_c_str(), OFlags::RDWR | OFlags::NOCTTY, Mode::empty()).expect("open the slave");
        let master = File::from(master);
        let mut reader = master.try_clone().expect("clone the master");
        let (tx, chunks) = mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = [0; 16 * 1024];
            // Ends when the last slave descriptor closes (EOF or EIO, as the platform reports it).
            while let Ok(n @ 1..) = reader.read(&mut buf) {
                if tx.send(buf[..n].to_vec()).is_err() {
                    return;
                }
            }
        });
        let pty = Pty { master, slave: Some(slave), chunks, out: Vec::new(), child: None };
        pty.resize(cols, rows);
        pty
    }

    /// Runs `cmd` on the slave as a new session whose controlling terminal it is, as a login shell's
    /// job would have it: the terminal's signals and window-size changes reach it.
    pub fn spawn(&mut self, mut cmd: Command) {
        let slave = self.slave.as_ref().expect("the slave is open");
        let fd = || Stdio::from(slave.try_clone().expect("clone the slave"));
        cmd.stdin(fd()).stdout(fd()).stderr(fd());
        // SAFETY: only async-signal-safe system calls between fork and exec.
        unsafe {
            cmd.pre_exec(|| {
                rustix::process::setsid()?;
                rustix::process::ioctl_tiocsctty(BorrowedFd::borrow_raw(0))?;
                Ok(())
            });
        }
        self.child = Some(cmd.spawn().expect("spawn the child"));
    }

    pub fn pid(&self) -> Pid {
        Pid::from_child(self.child.as_ref().expect("a child"))
    }

    /// Types `bytes` into the terminal, exactly as they are.
    pub fn write(&mut self, bytes: &[u8]) {
        self.master.write_all(bytes).expect("write to the terminal");
    }

    /// Changes the window size; the kernel tells the terminal's foreground process (SIGWINCH).
    pub fn resize(&self, cols: u16, rows: u16) {
        rustix::termios::tcsetwinsize(&self.master, Winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 }).expect("set the window size");
    }

    /// The terminal's modes, read through the master: they are the slave's own, and stay readable after
    /// the child's session has ended.
    pub fn termios(&self) -> Termios {
        rustix::termios::tcgetattr(&self.master).expect("read the terminal's modes")
    }

    pub fn signal(&self, sig: Signal) {
        rustix::process::kill_process(self.pid(), sig).expect("signal the child");
    }

    /// Takes whatever the child has written since the last call, waiting at most `within` for more.
    fn pull(&mut self, within: Duration) -> bool {
        match self.chunks.recv_timeout(within) {
            Ok(chunk) => {
                self.out.extend_from_slice(&chunk);
                while let Ok(more) = self.chunks.try_recv() {
                    self.out.extend_from_slice(&more);
                }
                true
            }
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => false,
        }
    }

    /// The index of `needle` at or after `from`, waiting until `within` has passed.
    pub fn wait_for(&mut self, needle: &str, from: usize, within: Duration) -> usize {
        let deadline = Instant::now() + within;
        loop {
            if let Some(i) = find(&self.out, needle.as_bytes(), from) {
                return i;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() || (!self.pull(left.min(Duration::from_millis(50))) && self.exited()) {
                if let Some(i) = find(&self.out, needle.as_bytes(), from) {
                    return i;
                }
                panic!("{needle:?} did not appear after byte {from} within {within:?}; the child wrote:\n{}", visible(&self.out));
            }
        }
    }

    fn exited(&mut self) -> bool {
        self.child.as_mut().is_none_or(|c| c.try_wait().ok().flatten().is_some())
    }

    /// Waits for the child to end, at most `within`, then reads what it wrote to the last byte: the
    /// test's own slave descriptor closes, so the master reaches its end once the output is drained.
    pub fn wait(&mut self, within: Duration) -> ExitStatus {
        let deadline = Instant::now() + within;
        let status = loop {
            if let Some(status) = self.child.as_mut().expect("a child").try_wait().expect("wait for the child") {
                break status;
            }
            if Instant::now() >= deadline {
                panic!("the child was still running after {within:?}; it wrote:\n{}", visible(&self.out));
            }
            self.pull(Duration::from_millis(20));
        };
        self.child = None;
        self.slave = None;
        let drained = Instant::now() + Duration::from_secs(5);
        while Instant::now() < drained {
            match self.chunks.recv_timeout(drained.saturating_duration_since(Instant::now())) {
                Ok(chunk) => self.out.extend_from_slice(&chunk),
                Err(_) => break,
            }
        }
        status
    }
}

impl Drop for Pty {
    /// A test that fails leaves no child behind.
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

pub fn find(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    haystack.get(from..)?.windows(needle.len()).position(|w| w == needle).map(|i| i + from)
}

/// The output with escapes and control bytes made visible, for failure messages.
pub fn visible(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).chars().flat_map(|c| if c == '\n' { vec!['\n'] } else if c.is_control() { format!("\\x{:02x}", c as u32).chars().collect() } else { vec![c] }).collect()
}
