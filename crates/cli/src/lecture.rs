//! The `lecture` command (M7 plan §E): the one-shots (`page`, `spend`, `audit`), the input and
//! course resolution, and the live lecture's preflight, prepare, session and end.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use lecturelive_core::audio::level::SilenceWatch;
use lecturelive_core::audio::mixed::{MixedSource, MIXED_MODE};
use lecturelive_core::audio::permission::{self, MicPermission};
use lecturelive_core::audio::source::{DeviceSource, Fallback, Source};
use lecturelive_core::audio::{input, loopback};
use lecturelive_core::notes::chat::{self, ChatClient, ChatConfig};
use lecturelive_core::notes::prompts;
use lecturelive_core::session::audit;
use lecturelive_core::session::coordinator::{SessionConfig, StopReport};
use lecturelive_core::session::files::{course_from_path, LectureFiles};
use lecturelive_core::session::launch::{self, Retention};
use lecturelive_core::session::lecture::{self, Lecture, SlideWatch};
use lecturelive_core::session::lock::FolderLock;
use lecturelive_core::session::spend::{self, Spend};
use lecturelive_core::session::start;
use lecturelive_core::stt::{rest, stream};

use crate::args::LectureArgs;
use crate::capture;
use crate::data_dir;
#[cfg(debug_assertions)]
use crate::fixture;
use crate::plain;
use crate::stop::StopController;
use crate::tui;
use crate::tui::state::{Identity, SourceKind};

const REPO_ENV: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../.env");
/// The Python CLI's ledger, taken over by the app's (spec §8).
const CLI_LEDGER: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../spend.jsonl");

/// The environment, a .env here or above, then the repository's .env.
fn env_value(name: &str) -> Option<String> {
    let _ = dotenvy::dotenv();
    std::env::var(name).ok().filter(|v| !v.is_empty()).or_else(|| dotenvy::from_path_iter(REPO_ENV).ok()?.flatten().find(|(k, _)| k == name).map(|(_, v)| v))
}

/// An input by UID, or by part of its name; checked before the folder is touched.
pub(crate) fn resolve_input(loopback: bool, device: Option<String>) -> Result<(String, String)> {
    if loopback {
        return Ok((loopback::BLACKHOLE_UID.to_string(), "BlackHole 2ch".to_string()));
    }
    let want = device.context("give --loopback or --device <UID or part of its name> (or LECTURE_DEVICE in .env); `lecturelive inputs` lists them")?;
    let inputs = input::list_inputs()?;
    inputs
        .iter()
        .find(|i| i.uid == want)
        .or_else(|| inputs.iter().find(|i| i.name.to_lowercase().contains(&want.to_lowercase())))
        .map(|i| (i.uid.clone(), i.name.clone()))
        .with_context(|| format!("No audio input matches {want:?}. Inputs now: {}.", inputs.iter().map(|i| i.name.as_str()).collect::<Vec<_>>().join(", ")))
}

/// `lecture audit`: each day's findings, then the counts. Exits 0 only for a whole folder, 1 when anything is unexplained,
/// 2 when something still waits for repair or recovery (the M6 gate).
fn audit_cmd(dir: &Path) -> Result<()> {
    let days = audit::audit_folder(dir)?;
    anyhow::ensure!(!days.is_empty(), "no lecture state in {} (.live_notes/*.v2.json)", dir.display());
    let (mut unexplained, mut waiting) = (0, 0);
    for (stem, a) in &days {
        println!("{stem}");
        for l in &a.lines {
            println!("  {l}");
        }
        unexplained += a.unexplained;
        waiting += a.waiting;
    }
    println!("{unexplained} unexplained, {waiting} waiting");
    match audit::exit_code(&days) {
        0 => Ok(()),
        code => std::process::exit(code),
    }
}

/// The lecture folder as an absolute path, `.` components dropped, without touching the filesystem: the course is
/// read from the folder's real names (so `--dir .` works), and a folder is still created only once the input is known.
fn lecture_dir(dir: Option<PathBuf>) -> Result<PathBuf> {
    let cwd = std::env::current_dir()?;
    let d = dir.map_or_else(|| cwd.clone(), |d| cwd.join(d));
    Ok(d.components().filter(|c| !matches!(c, std::path::Component::CurDir)).collect())
}

/// `--mixed <input>`: Zoom through BlackHole and that input together, once mixed mode has passed its drift test.
/// Returns the source as `mixed:<input UID>` and its name.
pub(crate) fn resolve_mixed(want: &str) -> Result<(String, String)> {
    anyhow::ensure!(MIXED_MODE, "mixed mode is disabled: it did not pass its drift test (docs/milestones.md, M6)");
    anyhow::ensure!(loopback::status()?.blackhole_present, "BlackHole 2ch is not installed (brew install blackhole-2ch)");
    let (uid, name) = resolve_input(false, Some(want.to_string()))?;
    Ok((format!("mixed:{uid}"), format!("Zoom and {name}")))
}

/// The source a UID names: `mixed:<input>` is Zoom through BlackHole with that input (spec §4.2).
pub(crate) fn source_for(uid: &str) -> Box<dyn Source> {
    match uid.strip_prefix("mixed:") {
        Some(input) => Box::new(MixedSource::new(loopback::BLACKHOLE_UID, input)),
        None => Box::new(DeviceSource { uid: uid.to_string(), fallback: Fallback::default() }),
    }
}

/// The frontend a live lecture shows itself in (plan §H).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Plain,
    Tui,
}

/// Which of the standard streams are terminals.
#[derive(Clone, Copy, Debug)]
struct Ttys {
    stdin: bool,
    stdout: bool,
    #[allow(dead_code)] // read by the automatic choice once the TUI becomes the default (plan Task 16)
    stderr: bool,
}

impl Ttys {
    fn now() -> Self {
        use std::io::IsTerminal;
        Ttys { stdin: std::io::stdin().is_terminal(), stdout: std::io::stdout().is_terminal(), stderr: std::io::stderr().is_terminal() }
    }
}

/// The frontend from the flags and the terminal (plan §H). Without a flag it is plain until the TUI is the default.
/// `--tui` also needs stdin to be a terminal: the stdin grammar, and crossterm cannot read `/dev/tty` on macOS.
fn choose_mode(tui: bool, plain: bool, ttys: Ttys, term: Option<&str>) -> Result<Mode> {
    if plain || !tui {
        return Ok(Mode::Plain);
    }
    anyhow::ensure!(ttys.stdin && ttys.stdout, "--tui needs a terminal on stdin and stdout");
    match term {
        None => anyhow::bail!("--tui needs a terminal that can draw: TERM is not set"),
        Some("dumb") => anyhow::bail!("--tui needs a terminal that can draw: TERM is dumb"),
        Some(_) => Ok(Mode::Tui),
    }
}

/// Names the scripted session that debug builds run in place of a recording (plan §H).
const FIXTURE_ENV: &str = "LECTURELIVE_CLI_FIXTURE";

/// A live session not yet started, for either frontend to await or spawn.
type Engine = std::pin::Pin<Box<dyn std::future::Future<Output = Result<StopReport>> + Send>>;

pub(crate) async fn lecture_cmd(a: LectureArgs) -> Result<()> {
    // A build without debug assertions has no scripted session: refused before anything is read or created.
    #[cfg(not(debug_assertions))]
    anyhow::ensure!(a.command.is_some() || std::env::var_os(FIXTURE_ENV).is_none(), "{FIXTURE_ENV} is honoured only by debug builds; unset it to record a lecture.");
    let mode = choose_mode(a.tui, a.plain, Ttys::now(), std::env::var("TERM").ok().as_deref())?;
    // Debug builds only: the scenario that stands in for the recording. It needs no key, microphone, input or
    // network, and leaves the person's ledger and route alone; the folder, `prepare` and the frontend are real.
    #[cfg(debug_assertions)]
    let fixture = match std::env::var(FIXTURE_ENV) {
        Ok(name) if a.command.is_none() => Some(fixture::Scenario::parse(&name)?),
        _ => None,
    };
    #[cfg(not(debug_assertions))]
    let fixture: Option<()> = None;
    // The plain display's context, read once (plan Task 12): how lines are painted, and whether
    // stdout is a terminal — an independent fact that decides whether untrusted text is cleaned.
    let out = plain::Display::stdout();
    let p = out.paint;
    let app_ledger = if fixture.is_some() {
        // A name in the scripted lecture's own folder, never written: the scripted session spends nothing.
        lecture_dir(a.dir.clone())?.join("fixture-spend.jsonl")
    } else {
        let ledger = data_dir()?.join("spend.jsonl");
        spend::take_over(&ledger, Path::new(CLI_LEDGER))?;
        ledger
    };
    if a.command.as_deref() == Some("spend") {
        let columns = std::env::var("COLUMNS").ok().and_then(|c| c.parse().ok()).unwrap_or(80);
        print!("{}", spend::render(&spend::read(&app_ledger)?, columns, p, &app_ledger));
        return Ok(());
    }
    let dir = lecture_dir(a.dir)?;
    if a.command.as_deref() == Some("audit") {
        return audit_cmd(&dir);
    }
    let key = match fixture {
        Some(_) => String::new(), // nothing is sent
        None => env_value("GROK_API_KEY").context("GROK_API_KEY is not set (the environment, a .env here or above, or the repository's .env)")?,
    };
    let course = a.course.or_else(|| course_from_path(&dir)).or_else(|| env_value("LECTURE_COURSE")).unwrap_or_else(|| "Lecture".into());
    let recording = a.command.is_none();
    // Checked before any file is created, so a missing device leaves the folder untouched.
    let input = if fixture.is_some() {
        Some(("fixture".to_string(), "the scripted session".to_string()))
    } else if recording {
        Some(match &a.mixed {
            Some(m) => resolve_mixed(m)?,
            None => resolve_input(a.loopback, a.device.or_else(|| env_value("LECTURE_DEVICE")))?,
        })
    } else {
        None
    };
    std::fs::create_dir_all(&dir)?;
    let dir = dir.canonicalize()?;
    let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let today = chrono::Local::now().date_naive();
    let resolve = |f: Option<PathBuf>| f.map(|f| if f.is_absolute() { f } else { dir.join(f) });
    let files = LectureFiles::custom(&dir, today, resolve(a.notes), resolve(a.transcript), resolve(a.slides_dir));
    let spend = Spend::open(&app_ledger, &course, &name, today)?;
    let chat = ChatClient::new(ChatConfig::new(key.clone()), Some(spend.clone()))?;
    let title = prompts::title(&course, &name, today);
    let lec = Arc::new(Lecture { files: files.clone(), course: course.clone(), name: name.clone(), title: title.clone(), chat, spend: spend.clone() });
    let (ev_tx, mut ev_rx) = tokio::sync::mpsc::unbounded_channel();

    let Some((uid, input_name)) = input else {
        // `lecture page`: the page from the notes as they are, without recording or polishing.
        anyhow::ensure!(files.notes.exists(), "No notes to typeset: {} is not in this folder.", files.notes.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
        let _lock = FolderLock::acquire(&dir)?;
        plain::say(&mut std::io::stdout().lock(), out, "page", "page", &format!("distilling {} with {}, a few minutes", files.notes.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), chat::MODEL));
        let printer = tokio::spawn({
            let words = capture::Words::new(&course);
            async move {
                let mut none = None;
                while let Some(e) = ev_rx.recv().await {
                    plain::show(&mut std::io::stdout().lock(), out, &e, &mut none, &words, false);
                }
            }
        });
        let _ = lec.page(&ev_tx).await;
        drop(ev_tx);
        printer.await?;
        return Ok(());
    };

    let stt_link = match fixture {
        Some(_) => None,
        None => {
            match permission::microphone() {
                MicPermission::Denied | MicPermission::Restricted => anyhow::bail!("microphone access is denied for this terminal: System Settings → Privacy & Security → Microphone"),
                _ => {}
            }
            Some(stream::spawn(stream::SttConfig::new(key.clone(), a.keyterms.clone()))?)
        }
    };
    let keyterms = a.keyterms.clone();
    let recovery = || -> Result<rest::RecoveryLink> {
        anyhow::ensure!(fixture.is_none(), "a scripted session recovers nothing");
        Ok(rest::spawn_recovery(rest::RestClient::new(rest::RestConfig::new(key.clone(), keyterms.clone()))?))
    };
    let _lock = FolderLock::acquire(&dir)?;
    if fixture.is_none() {
        if let Some(restored) = launch::restore_abandoned_route(&data_dir()?.join("route.json"))? {
            println!("  undid a canary route left behind; default output restored: {restored}");
        }
    }
    let mut announce = |stem: &str| plain::say(&mut std::io::stdout().lock(), out, "notes", "recovering", &format!("{stem}'s transcript gaps, before today's session"));
    let ready = start::prepare(&files, &title, a.rebuild, a.keep_days.map_or(Retention::KeepAll, Retention::KeepDays), &recovery, Some(spend.clone()), &mut announce).await?;
    plain::print_prepared(&mut std::io::stdout().lock(), out, &ready, &files, &course, &name, &input_name);

    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    // Capture on by default (plan Task 11): the course's saved window is loaded before the engine
    // starts, off the reactor, and the same saved state seeds the worker and the TUI's Ctrl-S. A
    // scripted session never reaches the window server or the person's data folder: its selections
    // live in the lecture folder, and only the `capture` scenario has any.
    let capture = if fixture.is_some() {
        #[cfg(debug_assertions)]
        {
            (fixture == Some(fixture::Scenario::Capture)).then(|| capture::Context {
                path: files.state_dir().join(fixture::CAPTURE_JSON),
                course: course.clone(),
                saved: Some(fixture::saved_selection()),
                current: None, // core's first raw state arrives with the lecture's events
            })
        }
        #[cfg(not(debug_assertions))]
        { None }
    } else {
        let path = data_dir()?.join(capture::FILE);
        Some(capture::Context { path: path.clone(), course: course.clone(), saved: capture::load(&path, &course).await?, current: None })
    };
    // The session, run by the frontend: `lecture::run`, or in debug builds the scripted stand-in.
    // Nothing runs until it is awaited or spawned. Both run behind the capture adapter, so Plain
    // and the TUI see the same already-adapted events — each with whether any relocation it
    // carried was actually persisted — and the engine's future resolves only after every final
    // event has been handed on.
    let engine_uid = uid.clone();
    let engine = |cmd_rx, ev_tx| -> Result<(Engine, tokio::sync::mpsc::UnboundedReceiver<capture::Forwarded>)> {
        let (fe_tx, fe_rx) = tokio::sync::mpsc::unbounded_channel();
        let forward = tokio::spawn(capture::forward(capture.clone(), ev_rx, fe_tx));
        Ok(match fixture {
            #[cfg(debug_assertions)]
            Some(scenario) => {
                let files = files.clone();
                (
                    Box::pin(async move {
                        let r = fixture::run(scenario, &files, cmd_rx, ev_tx).await;
                        let _ = forward.await;
                        r
                    }),
                    fe_rx,
                )
            }
            _ => {
                let session = SessionConfig { dir: dir.clone(), stem: files.stem.clone(), stt: stt_link, recovery: Some(recovery()?), spend: Some(spend.clone()), transcript: Some(files.transcript.clone()) };
                let watch_slides = SlideWatch { screenshots: lecture::screenshot_dir(), poll: Duration::from_secs(1) };
                // Spec §7, as the desktop builds it: the course's saved selection, revalidated by
                // the worker as the lecture starts; LECTURELIVE_RECORD verbatim when it is set.
                let setup = capture.as_ref().map(|c| capture::setup(c.saved.clone(), capture::record_path(std::env::var_os("LECTURELIVE_RECORD"))));
                (
                    Box::pin(async move {
                        let r = lecture::run(lec, session, source_for(&engine_uid), watch_slides, setup, cmd_rx, ev_tx).await;
                        let _ = forward.await;
                        r
                    }),
                    fe_rx,
                )
            }
        })
    };
    if mode == Mode::Tui {
        // Debug builds only: the terminal faults the PTY tests inject, named by the scenario.
        #[cfg(debug_assertions)]
        match fixture {
            Some(fixture::Scenario::InitFailRaw) => tui::terminal::inject(tui::terminal::Fault::AlternateScreen),
            Some(fixture::Scenario::InitFailMouse) => tui::terminal::inject(tui::terminal::Fault::Mouse),
            Some(fixture::Scenario::InitFail) => tui::terminal::inject(tui::terminal::Fault::Surface),
            Some(fixture::Scenario::DrawFail) => tui::terminal::inject(tui::terminal::Fault::SecondDraw),
            _ => {}
        }
        // What the session view opens with (plan Task 6): the lecture's names, the input it records
        // from, and the start-up records the plain report printed.
        let kind = if fixture.is_some() {
            SourceKind::Fixture
        } else if uid.starts_with("mixed:") {
            SourceKind::Mixed
        } else if uid == loopback::BLACKHOLE_UID {
            SourceKind::Loopback
        } else {
            SourceKind::Input
        };
        let file_name = |p: &Path| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let session = tui::Session {
            identity: Identity {
                course: course.clone(),
                lecture: name.clone(),
                input: input_name.clone(),
                kind,
                notes_file: file_name(&files.notes),
                transcript_file: file_name(&files.transcript),
            },
            files: files.clone(),
            spend: spend.clone(),
            seed: plain::startup_records(&ready, &files),
            capture: capture.clone(),
        };
        // The terminal is given back before this returns, so the summary prints on the ordinary screen.
        let (engine, events) = engine(cmd_rx, ev_tx)?;
        let report = tui::run(session, engine, cmd_tx, events, a.secs).await?;
        plain::print_end(&mut std::io::stdout().lock(), out, &files, &report, &spend);
        return Ok(());
    }
    plain::read_commands(cmd_tx.clone());
    let stop = Arc::new(Mutex::new(StopController::default()));
    plain::stop_on_ctrl_c(cmd_tx.clone(), out, stop.clone());
    if let Some(limit) = a.secs {
        plain::stop_after_secs(limit, cmd_tx.clone(), out, stop);
    }
    drop(cmd_tx);
    let mixed = uid.starts_with("mixed:");
    let mut watch = (uid == loopback::BLACKHOLE_UID || mixed).then(|| SilenceWatch::new(-60.0, 10));
    let (engine, mut events) = engine(cmd_rx, ev_tx)?;
    let words = capture::Words::new(&course);
    let printer = tokio::spawn(async move {
        while let Some(f) = events.recv().await {
            match &f.capture_persistence {
                // a relocation the session found but could not keep: one line, both truths, the
                // failure last — never a bare "found again" that implies it was saved
                capture::CapturePersistence::MovedSaveFailed { error } => {
                    let (kind, label, detail) = capture::unsaved_words(error);
                    plain::say(&mut std::io::stdout().lock(), out, kind, label, &detail);
                }
                _ => plain::show(&mut std::io::stdout().lock(), out, &f.event, &mut watch, &words, mixed),
            }
        }
    });
    let result = engine.await;
    printer.await?;
    let report = result?;
    plain::print_end(&mut std::io::stdout().lock(), out, &files, &report, &spend);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M3 minor: the course comes from the folder's absolute path, so `--dir .` names the course above Weeks/.
    #[test]
    fn a_relative_folder_is_made_absolute_before_the_course_is_read() {
        let root = std::env::temp_dir().join(format!("lecturelive-cli-{}", std::process::id()));
        let week = root.join("Machine Learning/Weeks/Week 01");
        std::fs::create_dir_all(&week).unwrap();
        let here = std::env::current_dir().unwrap();
        std::env::set_current_dir(&week).unwrap();
        let dir = lecture_dir(Some(PathBuf::from(".")));
        std::env::set_current_dir(here).unwrap();
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(course_from_path(&dir.unwrap()).as_deref(), Some("Machine Learning"));
    }

    const GOOD: Option<&str> = Some("xterm-256color");

    fn ttys(stdin: bool, stdout: bool, stderr: bool) -> Ttys {
        Ttys { stdin, stdout, stderr }
    }

    /// Plan §H, until the TUI becomes the default (Task 16): no flag is plain whatever the terminal, `--plain` is plain,
    /// and `--tui` is the TUI exactly when stdin and stdout are terminals and TERM can draw. Stderr never decides it.
    #[test]
    fn choose_mode_table() {
        for (tui, plain) in [(false, false), (false, true), (true, false)] {
            for stdin in [true, false] {
                for stdout in [true, false] {
                    for stderr in [true, false] {
                        for term in [GOOD, Some("dumb"), None] {
                            let got = choose_mode(tui, plain, ttys(stdin, stdout, stderr), term).map_err(|e| e.to_string());
                            let want = if !tui { Ok(Mode::Plain) } else if !(stdin && stdout) { Err("--tui needs a terminal on stdin and stdout".to_string()) } else if term == GOOD { Ok(Mode::Tui) } else { Err(format!("--tui needs a terminal that can draw: TERM is {}", if term.is_none() { "not set" } else { "dumb" })) };
                            assert_eq!(got, want, "tui {tui}, plain {plain}, stdin {stdin}, stdout {stdout}, stderr {stderr}, TERM {term:?}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn choose_mode_pins_the_cases_that_matter() {
        let all = ttys(true, true, true);
        assert_eq!(choose_mode(false, false, all, GOOD).unwrap(), Mode::Plain, "the default stays plain on a full terminal");
        assert_eq!(choose_mode(false, true, all, GOOD).unwrap(), Mode::Plain);
        assert_eq!(choose_mode(true, false, all, GOOD).unwrap(), Mode::Tui);
        assert_eq!(choose_mode(true, false, ttys(true, true, false), GOOD).unwrap(), Mode::Tui, "stderr redirected does not refuse --tui");
        assert_eq!(choose_mode(true, false, ttys(false, true, true), GOOD).unwrap_err().to_string(), "--tui needs a terminal on stdin and stdout");
        assert_eq!(choose_mode(true, false, ttys(true, false, true), GOOD).unwrap_err().to_string(), "--tui needs a terminal on stdin and stdout");
        assert_eq!(choose_mode(true, false, all, Some("dumb")).unwrap_err().to_string(), "--tui needs a terminal that can draw: TERM is dumb");
        assert_eq!(choose_mode(true, false, all, None).unwrap_err().to_string(), "--tui needs a terminal that can draw: TERM is not set");
    }

    /// The flags conflict with each other and with the one-shots, as clap parses them.
    #[test]
    fn frontend_flags_conflict_with_each_other_and_the_one_shots() {
        use clap::Parser;
        let parse = |args: &[&str]| crate::args::Cli::try_parse_from(["lecturelive", "lecture"].iter().chain(args)).map(|_| ()).map_err(|e| e.kind());
        assert_eq!(parse(&["--tui", "--plain"]), Err(clap::error::ErrorKind::ArgumentConflict));
        assert_eq!(parse(&["--tui", "--no-tui"]), Err(clap::error::ErrorKind::ArgumentConflict));
        for one_shot in ["page", "spend", "audit"] {
            for flag in ["--tui", "--plain", "--no-tui"] {
                assert_eq!(parse(&[one_shot, flag]), Err(clap::error::ErrorKind::ArgumentConflict), "{one_shot} {flag}");
            }
        }
        for ok in [&["--tui"][..], &["--plain"], &["--no-tui"], &[], &["spend"]] {
            assert_eq!(parse(ok), Ok(()), "{ok:?}");
        }
    }
}
