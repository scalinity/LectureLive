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
use lecturelive_core::session::coordinator::SessionConfig;
use lecturelive_core::session::files::{course_from_path, LectureFiles};
use lecturelive_core::session::launch::{self, Retention};
use lecturelive_core::session::lecture::{self, Lecture, SlideWatch};
use lecturelive_core::session::lock::FolderLock;
use lecturelive_core::session::spend::{self, Spend};
use lecturelive_core::session::start;
use lecturelive_core::stt::{rest, stream};

use crate::args::LectureArgs;
use crate::data_dir;
use crate::plain;
use crate::stop::StopController;

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

pub(crate) async fn lecture_cmd(a: LectureArgs) -> Result<()> {
    let p = plain::paint();
    let app_ledger = data_dir()?.join("spend.jsonl");
    spend::take_over(&app_ledger, Path::new(CLI_LEDGER))?;
    if a.command.as_deref() == Some("spend") {
        let columns = std::env::var("COLUMNS").ok().and_then(|c| c.parse().ok()).unwrap_or(80);
        print!("{}", spend::render(&spend::read(&app_ledger)?, columns, p, &app_ledger));
        return Ok(());
    }
    let dir = lecture_dir(a.dir)?;
    if a.command.as_deref() == Some("audit") {
        return audit_cmd(&dir);
    }
    let key = env_value("GROK_API_KEY").context("GROK_API_KEY is not set (the environment, a .env here or above, or the repository's .env)")?;
    let course = a.course.or_else(|| course_from_path(&dir)).or_else(|| env_value("LECTURE_COURSE")).unwrap_or_else(|| "Lecture".into());
    let recording = a.command.is_none();
    // Checked before any file is created, so a missing device leaves the folder untouched.
    let input = if recording {
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
        plain::say(&mut std::io::stdout().lock(), p, "page", "page", &format!("distilling {} with {}, a few minutes", files.notes.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), chat::MODEL));
        let printer = tokio::spawn(async move {
            let mut none = None;
            while let Some(e) = ev_rx.recv().await {
                plain::show(&mut std::io::stdout().lock(), p, &e, &mut none);
            }
        });
        let _ = lec.page(&ev_tx).await;
        drop(ev_tx);
        printer.await?;
        return Ok(());
    };

    match permission::microphone() {
        MicPermission::Denied | MicPermission::Restricted => anyhow::bail!("microphone access is denied for this terminal: System Settings → Privacy & Security → Microphone"),
        _ => {}
    }
    let stt_link = stream::spawn(stream::SttConfig::new(key.clone(), a.keyterms.clone()))?;
    let keyterms = a.keyterms.clone();
    let recovery = || -> Result<rest::RecoveryLink> { Ok(rest::spawn_recovery(rest::RestClient::new(rest::RestConfig::new(key.clone(), keyterms.clone()))?)) };
    let _lock = FolderLock::acquire(&dir)?;
    if let Some(restored) = launch::restore_abandoned_route(&data_dir()?.join("route.json"))? {
        println!("  undid a canary route left behind; default output restored: {restored}");
    }
    let mut announce = |stem: &str| plain::say(&mut std::io::stdout().lock(), p, "notes", "recovering", &format!("{stem}'s transcript gaps, before today's session"));
    let ready = start::prepare(&files, &title, a.rebuild, a.keep_days.map_or(Retention::KeepAll, Retention::KeepDays), &recovery, Some(spend.clone()), &mut announce).await?;
    plain::print_prepared(&mut std::io::stdout().lock(), p, &ready, &files, &course, &name, &input_name);

    let session = SessionConfig { dir: dir.clone(), stem: files.stem.clone(), stt: Some(stt_link), recovery: Some(recovery()?), spend: Some(spend.clone()), transcript: Some(files.transcript.clone()) };
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    plain::read_commands(cmd_tx.clone());
    let stop = Arc::new(Mutex::new(StopController::default()));
    plain::stop_on_ctrl_c(cmd_tx.clone(), p, stop.clone());
    if let Some(limit) = a.secs {
        plain::stop_after_secs(limit, cmd_tx.clone(), p, stop);
    }
    drop(cmd_tx);
    let mut watch = (uid == loopback::BLACKHOLE_UID || uid.starts_with("mixed:")).then(|| SilenceWatch::new(-60.0, 10));
    let printer = tokio::spawn(async move {
        while let Some(e) = ev_rx.recv().await {
            plain::show(&mut std::io::stdout().lock(), p, &e, &mut watch);
        }
    });
    let watch_slides = SlideWatch { screenshots: lecture::screenshot_dir(), poll: Duration::from_secs(1) };
    let result = lecture::run(lec, session, source_for(&uid), watch_slides, None, cmd_rx, ev_tx).await;
    printer.await?;
    let report = result?;
    plain::print_end(&mut std::io::stdout().lock(), p, &files, &report, &spend);
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
}
