//! Process-level checks of the plain frontend in a pipe (M7 plan §J, Task 4): the scripted session
//! (`LECTURELIVE_CLI_FIXTURE`, debug builds only) runs the real folder, `prepare` and plain adapter, and
//! nothing a pipe receives carries a terminal control sequence. Every child runs with `HOME` set to an
//! empty scratch directory, so the person's application data cannot be touched and must stay untouched.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

const FIXTURE: &str = "LECTURELIVE_CLI_FIXTURE";

fn lecturelive(bin: &str, home: &Path, args: &[&str], env: &[(&str, &str)], stdin: &[u8]) -> Output {
    let mut child = Command::new(bin)
        .args(args)
        .env("HOME", home)
        .env_remove(FIXTURE)
        .envs(env.iter().copied())
        .current_dir(home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin).unwrap(); // then closed: EOF
    child.wait_with_output().unwrap()
}

fn no_escapes(out: &Output) {
    assert!(!out.stdout.contains(&0x1b), "stdout carries an escape: {}", String::from_utf8_lossy(&out.stdout));
    assert!(!out.stderr.contains(&0x1b), "stderr carries an escape: {}", String::from_utf8_lossy(&out.stderr));
}

fn empty(dir: &Path) -> bool {
    std::fs::read_dir(dir).unwrap().next().is_none()
}

/// The default frontend in a pipe is plain; the grammar reaches the session; EOF is not a stop; only `--secs` ends it.
#[test]
fn the_scripted_lecture_runs_plain_through_a_pipe() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir(&home).unwrap();
    let dir = tmp.path().join("Week 01 — Fixture");
    let started = Instant::now();
    let out = lecturelive(env!("CARGO_BIN_EXE_lecturelive"), &home, &["lecture", "--dir", dir.to_str().unwrap(), "--course", "Fixture Course", "--secs", "1"], &[(FIXTURE, "quiet")], b"\nnotes\n");
    let elapsed = started.elapsed();
    let stdout = String::from_utf8(out.stdout.clone()).unwrap();

    assert!(out.status.success(), "exit {:?}\nstdout:\n{stdout}\nstderr:\n{}", out.status.code(), String::from_utf8_lossy(&out.stderr));
    assert!(out.stderr.is_empty(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    no_escapes(&out);
    assert!(stdout.contains("  transcribing\n"), "{stdout}");
    let plain = stdout.find("  │ ## Scripted snapshot\n  │ - no focus hint\n").unwrap_or_else(|| panic!("Enter took no snapshot:\n{stdout}"));
    let hinted = stdout.find("  │ ## Scripted snapshot\n  │ - focus: notes\n").unwrap_or_else(|| panic!("`notes` took no hinted snapshot:\n{stdout}"));
    assert!(plain < hinted, "in stdin's order:\n{stdout}");
    assert_eq!(stdout.matches(" slides folded in ").count(), 2, "one commit per line, none for EOF:\n{stdout}");
    assert!(stdout.contains("  ✓ saved  lecture_notes_"), "{stdout}");
    assert!(elapsed >= Duration::from_secs(1), "EOF ended the lecture before --secs did: {elapsed:?}");

    let names: Vec<String> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    assert!(names.iter().any(|n| n.starts_with("lecture_notes_")) && names.iter().any(|n| n == ".live_notes"), "the real folder was prepared: {names:?}");
    assert!(!names.iter().any(|n| n == "fixture-spend.jsonl"), "the scripted session spends nothing");
    assert!(empty(&home), "application data was touched: {:?}", std::fs::read_dir(&home).unwrap().flatten().map(|e| e.path()).collect::<Vec<_>>());
}

#[test]
fn tui_is_refused_when_stdin_is_a_pipe() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("never");
    let out = lecturelive(env!("CARGO_BIN_EXE_lecturelive"), tmp.path(), &["lecture", "--tui", "--dir", dir.to_str().unwrap()], &[], b"");
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("--tui needs a terminal on stdin and stdout"), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(out.stdout.is_empty());
    no_escapes(&out);
    assert!(!dir.exists());
    assert!(empty(tmp.path()), "refused before anything was touched");
}

/// Clap refuses the combinations before the command runs: its usage error, exit 2.
#[test]
fn frontend_flags_conflict_in_the_process() {
    let tmp = tempfile::tempdir().unwrap();
    for args in [&["lecture", "--tui", "--plain"][..], &["lecture", "--no-tui", "--tui"], &["lecture", "page", "--tui"], &["lecture", "spend", "--plain"], &["lecture", "audit", "--no-tui"]] {
        let out = lecturelive(env!("CARGO_BIN_EXE_lecturelive"), tmp.path(), args, &[], b"");
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(String::from_utf8_lossy(&out.stderr).contains("cannot be used with"), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        assert!(out.stdout.is_empty());
        no_escapes(&out);
    }
    assert!(empty(tmp.path()));
}

/// Plan §C 15: a build without debug assertions has no scripted session and refuses the variable before touching
/// anything. Run by hand against such a build (never `target/release`):
/// `cargo build -p lecturelive-cli --config 'profile.dev.debug-assertions=false' --target-dir "$TMPDIR/m7-noassert"`, then
/// `LECTURELIVE_NOASSERT_BIN="$TMPDIR/m7-noassert/debug/lecturelive" cargo test -p lecturelive-cli --test pipe fixture_refused -- --ignored`.
#[test]
#[ignore = "needs a build without debug assertions, named by LECTURELIVE_NOASSERT_BIN"]
fn fixture_refused_by_a_build_without_debug_assertions() {
    let bin = std::env::var("LECTURELIVE_NOASSERT_BIN").expect("LECTURELIVE_NOASSERT_BIN names the binary built without debug assertions");
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("never");
    let out = lecturelive(&bin, tmp.path(), &["lecture", "--dir", dir.to_str().unwrap()], &[(FIXTURE, "quiet")], b"");
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(String::from_utf8_lossy(&out.stderr), "Error: LECTURELIVE_CLI_FIXTURE is honoured only by debug builds; unset it to record a lecture.\n");
    assert!(out.stdout.is_empty(), "no lecture ran: {}", String::from_utf8_lossy(&out.stdout));
    assert!(!dir.exists(), "the folder was created");
    assert!(empty(tmp.path()), "application data was touched");
}
