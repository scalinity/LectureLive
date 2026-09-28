//! Task 13's process measurements (plan §L): the CPU the dev binary itself uses under the PTY
//! harness and the maximum resident set of the stress fixture — measured on the `lecturelive`
//! child the harness spawned, never on the test process, never on `/usr/bin/time`'s wrapper.
//! Every child is the debug build's scripted session in a temporary folder with a scratch `HOME`,
//! as the terminal suite's: no key, no microphone, no network, none of the person's data.
//!
//! All of it is `#[ignore]`d and named `perf` for the canonical
//! `cargo test -p lecturelive-cli perf -- --ignored --nocapture`.

mod support {
    pub mod pty;
}

use std::path::PathBuf;
use std::process::{Command, ExitStatus};
use std::time::Duration;

use support::pty::Pty;

const CTRL_C: &[u8] = b"\x03";
/// Start-up (folder, `prepare`) to the first frame; generous for a loaded machine.
const START: Duration = Duration::from_secs(30);
/// The readiness marker the long fixtures say once their whole backlog has landed (plan Task 13):
/// a single word, because the frame is a diff — changed cells are written word by word, so only a
/// whole word is guaranteed contiguous in the byte stream. The fixture's marker notice is its last
/// event: the word says the segments, the notes document and the slides are all on board.
const LOADED: &str = "quiet";

/// One `lecture --tui` session in a `cols` × `rows` terminal, running fixture `scenario`. When
/// `wrapper` names a program and its arguments (Task 13's RSS measurement), the child is
/// `wrapper… program …` — the TUI still runs inside the PTY, inheriting the wrapper's terminal,
/// which is the point.
struct Lecture {
    pty: Pty,
    /// The lecture folder the child was given, so a test can watch the fixture's own
    /// synchronization markers inside it (Task 13's remediation: the burst's active interval).
    dir: PathBuf,
    /// Where the wrapper, if any, wrote its report.
    report: Option<PathBuf>,
    /// The temporary root holding the scratch `HOME` and the lecture folder, for the drop.
    _tmp: tempfile::TempDir,
}

fn tui(wrapped_in: Option<&[&str]>, scenario: &str, cols: u16, rows: u16) -> Lecture {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir(&home).unwrap();
    let dir = tmp.path().join("Week 01 — Fixture");
    let mut pty = Pty::open(cols, rows);
    let bin = env!("CARGO_BIN_EXE_lecturelive");
    let mut cmd = Command::new(bin);
    let mut report = None;
    if let Some(wrapper) = wrapped_in {
        // e.g. `/usr/bin/time -l -o <file>`: the program comes after the wrapper's own arguments
        let mut w = Command::new(wrapper[0]);
        for a in &wrapper[1..] {
            if *a == "{report}" {
                let path = tmp.path().join("time-report.txt");
                report = Some(path.clone());
                w.arg(&path);
            } else {
                w.arg(a);
            }
        }
        w.arg(bin);
        cmd = w;
    }
    cmd.args(["lecture", "--tui", "--dir", dir.to_str().unwrap(), "--course", "Fixture Course"])
        .env_clear()
        .env("HOME", &home)
        .env("PATH", "/usr/bin:/bin")
        .env("TERM", "xterm-256color")
        .env("LECTURELIVE_CLI_FIXTURE", scenario)
        .current_dir(&home);
    pty.spawn(cmd);
    Lecture { pty, dir, report, _tmp: tmp }
}

impl Lecture {
    /// The fixture's own marker inside the lecture folder it was given, once it exists. The
    /// burst fixture writes `fixture-burst-started` immediately before its 5,000-delta clock
    /// starts and `fixture-burst-finished` immediately after its last delta is sent, so a
    /// harness can measure the active burst itself — never the load-in, never the quiet after.
    fn marker(&self, name: &str) -> Option<std::time::SystemTime> {
        let path = self.dir.join(".live_notes").join(name);
        let meta = std::fs::metadata(path).ok()?;
        meta.modified().ok()
    }
}

/// Ends the session with one Ctrl-C and requires the clean exit 0 of a stopped lecture.
fn stop_cleanly(l: &mut Lecture) -> ExitStatus {
    l.pty.write(CTRL_C);
    let status = l.pty.wait(Duration::from_secs(30));
    assert_eq!(status.code(), Some(0), "exit {status:?}");
    status
}

/// The `lecturelive` process's own CPU share, as macOS `ps` reports it (100 % = one full core).
/// `-p` names the exact child the harness spawned, never a search by name.
fn ps_cpu(pid: u32) -> f64 {
    let out = Command::new("ps").args(["-o", "%cpu=", "-p", &pid.to_string()]).output().expect("ps");
    String::from_utf8_lossy(&out.stdout).trim().parse::<f64>().unwrap_or_else(|e| panic!("ps said {e}: {}", String::from_utf8_lossy(&out.stdout)))
}

fn mean(samples: &[f64]) -> f64 {
    samples.iter().sum::<f64>() / samples.len() as f64
}

/// `ps`'s %cpu on macOS is a decaying recent average, so the first sample after a state change
/// lags; the fixed rule for every CPU test below, declared before any was run: wait out the load
/// transition first (the readiness marker plus a settle), then take one sample per second.
fn sample_cpu(pid: u32, seconds: usize) -> Vec<f64> {
    let mut samples = Vec::with_capacity(seconds);
    for k in 0..seconds {
        std::thread::sleep(Duration::from_secs(1));
        let cpu = ps_cpu(pid);
        println!("PERF cpu_sample_{k}={cpu}");
        samples.push(cpu);
    }
    samples
}

/// CPU, quiet (plan §L): the loaded `two-hour` session, its 1,440 segments and 6,671-word notes on
/// screen and nothing further happening, for thirty seconds. The mean must stay under 2 % of a core.
#[test]
#[ignore = "perf: run with `cargo test -p lecturelive-cli perf -- --ignored --nocapture`"]
fn perf_cpu_quiet() {
    // 140 × 40, TERM=xterm-256color — the wide shape every frame measurement uses too
    let mut l = tui(None, "two-hour", 140, 40);
    let loaded = l.pty.wait_for(LOADED, 0, START);
    assert!(l.pty.wait_for("Listening", 0, START) < loaded, "the session was up before its backlog");
    let pid = l.pty.pid().as_raw_nonzero().get() as u32; // the exact lecturelive child, never `pgrep`
    let samples = sample_cpu(pid, 30);
    stop_cleanly(&mut l);
    let mean = mean(&samples);
    println!("PERF quiet_cpu_pct={mean:.2}");
    println!("PERF quiet_cpu_samples_n={} max={:.2}", samples.len(), samples.iter().copied().fold(f64::MIN, f64::max));
    assert!(mean < 2.0, "quiet CPU mean {mean:.2}% is not under 2% of one core");
}

/// CPU, ordinary stream (plan §L): the existing `transcript` fixture — a level every 100 ms, the
/// open utterance moving every 300 ms, a closed segment every second — unchanged, for thirty
/// seconds. The mean must stay under 5 % of a core.
#[test]
#[ignore = "perf: run with `cargo test -p lecturelive-cli perf -- --ignored --nocapture`"]
fn perf_cpu_ordinary() {
    let mut l = tui(None, "transcript", 140, 40);
    l.pty.wait_for("Listening", 0, START);
    let pid = l.pty.pid().as_raw_nonzero().get() as u32;
    let samples = sample_cpu(pid, 30);
    stop_cleanly(&mut l);
    let mean = mean(&samples);
    println!("PERF ordinary_cpu_pct={mean:.2}");
    println!("PERF ordinary_cpu_samples_n={} max={:.2}", samples.len(), samples.iter().copied().fold(f64::MIN, f64::max));
    assert!(mean < 5.0, "ordinary-stream CPU mean {mean:.2}% is not under 5% of one core");
}

/// CPU, burst (plan §L, Task 13's remediation): the canonical burst is exactly 5,000 deltas at
/// 500/s — ten seconds — and is never inflated. Thirty seconds of active sampling come from three
/// independent runs, and every sample is demonstrably inside its own run's active burst: the
/// fixture writes `fixture-burst-started` into the lecture folder immediately before its delta
/// clock starts and `fixture-burst-finished` immediately after its last delta, and this harness
/// reads both markers' own timestamps (the same system clock) to bound every `ps` sample in
/// `[started, finished)`. The earlier harness slept once here *and* again inside its sampler,
/// which pushed the first sample to ≈2 s and could have run past the end of the burst into the
/// quiet after it; the schedule is now owned here alone: sample as soon as the burst starts, then
/// once a second, and never after the finish marker appears. No sample is dropped afterwards.
#[test]
#[ignore = "perf: run with `cargo test -p lecturelive-cli perf -- --ignored --nocapture`"]
fn perf_cpu_burst() {
    const RUNS: usize = 3;
    const WANT: usize = 10; // one per second of a ten-second burst
    let mut all: Vec<f64> = Vec::with_capacity(RUNS * WANT);
    let mut run_means = Vec::with_capacity(RUNS);
    for run in 0..RUNS {
        let mut l = tui(None, "burst", 140, 40);
        l.pty.wait_for("Listening", 0, START); // the session is up; the burst's own clock is not yet
        let pid = l.pty.pid().as_raw_nonzero().get() as u32;

        // the burst's own start, taken from the fixture's marker — never "Listening", never a sleep
        let start = wait_for_marker(&l, "fixture-burst-started", START);
        let mut samples: Vec<(std::time::SystemTime, f64)> = Vec::with_capacity(WANT);
        let mut next = std::time::Instant::now();
        for k in 0..WANT {
            // schedule only, never sleep twice: the sampler's own sleep is the whole delay
            let at = next;
            let left = at.saturating_duration_since(std::time::Instant::now());
            if !left.is_zero() {
                std::thread::sleep(left);
            }
            next += Duration::from_secs(1);
            // the interval is re-checked at the moment of the sample: once the fixture has
            // finished, no further sample is taken, so no quiet sample can enter the mean
            if l.marker("fixture-burst-finished").is_some() {
                break;
            }
            let took = std::time::SystemTime::now();
            if took < start {
                continue; // not yet inside the active interval
            }
            let cpu = ps_cpu(pid);
            println!("PERF burst_run{run}_sample{k}_offset_ms={:.3} cpu_pct={cpu}", took.duration_since(start).unwrap().as_secs_f64() * 1e3);
            samples.push((took, cpu));
        }
        stop_cleanly(&mut l);

        let finished = l.marker("fixture-burst-finished").expect("the burst fixture wrote its finish marker");
        let inside: Vec<f64> = samples.iter().filter(|(at, _)| *at >= start && *at < finished).map(|(_, c)| *c).collect();
        assert!(inside.len() >= 8, "run {run} took only {} samples inside its active burst", inside.len());
        assert_eq!(inside.len(), samples.len(), "every sample was inside the active interval, none after the finish");
        let run_mean = mean(&inside);
        run_means.push(run_mean);
        println!("PERF burst_run{run}_cpu_pct={run_mean:.2}");
        all.extend(inside);
    }
    let combined = mean(&all);
    println!("PERF burst_cpu_pct={combined:.2}");
    println!("PERF burst_cpu_samples_n={}", all.len());
    assert_eq!(all.len(), RUNS * WANT, "three runs of ten active-burst samples each");
    assert!(combined < 20.0, "burst CPU mean {combined:.2}% is not under 20% of one core (runs: {run_means:?})");
}

/// Waits for the fixture's marker and returns the instant the fixture recorded for it (the
/// file's own modification time, on the same system clock as this process), so offsets are
/// measured from the burst itself and not from when this harness happened to notice.
fn wait_for_marker(l: &Lecture, name: &str, within: Duration) -> std::time::SystemTime {
    let deadline = std::time::Instant::now() + within;
    loop {
        if let Some(at) = l.marker(name) {
            return at;
        }
        assert!(std::time::Instant::now() < deadline, "{name} never appeared within {within:?}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Maximum RSS, stress fixture (plan §L): `/usr/bin/time -l` around the `lecturelive` child while
/// it runs inside the PTY — loading is part of the measurement, intentionally. The wrapper's
/// report goes to its own file (`-o`): the TUI keeps the terminal, and the rusage lines cannot be
/// lost among the frame bytes. The stress state (10,000 segments, 100,000 notes words, 200
/// slides) must be fully landed and drawn before the stop, so the process has held everything it
/// ever will. macOS reports the maximum resident set in bytes; the gate is under 100 MiB.
#[test]
#[ignore = "perf: run with `cargo test -p lecturelive-cli perf -- --ignored --nocapture`"]
fn perf_stress_max_rss() {
        let wrapped = ["/usr/bin/time", "-l", "-o", "{report}"];
    let mut l = tui(Some(&wrapped), "stress", 140, 40);
    // the whole retained state landed: the marker notice is the fixture's last event, so the
    // 10,000 segments, the 100,000-word document and the 200 slides are all drawn under it
    l.pty.wait_for(LOADED, 0, START);
    // at least one frame after the complete state: the 1 Hz clock draws again within the second
    std::thread::sleep(Duration::from_secs(2));
    stop_cleanly(&mut l);
    let report = l.report.as_ref().expect("the wrapper wrote its report");
    let out = std::fs::read_to_string(report).unwrap_or_else(|e| panic!("reading {report:?}: {e}"));
    let bytes = max_rss_bytes(&out).unwrap_or_else(|| panic!("no maximum resident set in {out:?}"));
    let mib = bytes as f64 / 1_048_576.0;
    println!("PERF stress_max_rss_bytes={bytes}");
    println!("PERF stress_max_rss_mib={mib:.2}");
    assert!(mib < 100.0, "stress maximum RSS {mib:.2} MiB is not under 100 MiB");
}

/// macOS `/usr/bin/time -l` prints `  <bytes>  maximum resident set size` — the number before the
/// label, in bytes.
fn max_rss_bytes(out: &str) -> Option<u64> {
    let at = out.find("maximum resident set size")?;
    let before = out[..at].trim_end();
    let start = before.rfind(char::is_whitespace)? + 1;
    before[start..].parse().ok()
}

/// Nothing here measures the test process or infers a pid by name: every sample names the exact
/// child `Pty::spawn` created (checked once here so each test above can rely on it).
#[test]
fn the_harness_names_the_exact_child() {
    let mut l = tui(None, "quiet", 100, 30);
    l.pty.wait_for("Listening", 0, START);
    let pid = l.pty.pid().as_raw_nonzero().get() as u32;
    let out = Command::new("ps").args(["-o", "comm=", "-p", &pid.to_string()]).output().unwrap();
    let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert!(name.ends_with("lecturelive"), "pid {pid} is {name:?}, not the lecturelive child");
    stop_cleanly(&mut l);
}
