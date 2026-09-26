use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use lecturelive_core::audio::level::{self, SilenceWatch};
use lecturelive_core::audio::permission::{self, MicPermission};
use lecturelive_core::audio::mixed::{MixedSource, MIXED_MODE};
use lecturelive_core::audio::source::{DeviceSource, Fallback, Source};
use lecturelive_core::audio::{input, loopback, recorder, routing};
use lecturelive_core::capture::window;
use lecturelive_core::session::audit;
use lecturelive_core::session::coordinator::{self, Notification, SessionConfig, SttStatus, StopReport};
use lecturelive_core::session::launch::{self, Retention};
use lecturelive_core::notes::chat::{self, ChatClient, ChatConfig};
use lecturelive_core::notes::prompts;
use lecturelive_core::session::files::{course_from_path, LectureFiles};
use lecturelive_core::session::folder::How;
use lecturelive_core::session::lecture::{self, Command as LectureCommand, Event, Lecture, Op, SlideWatch};
use lecturelive_core::session::lock::FolderLock;
use lecturelive_core::session::start;
use lecturelive_core::session::notesfile::Recovered;
use lecturelive_core::session::segments::SegmentSource;
use lecturelive_core::session::spend::{self, Spend};
use lecturelive_core::stt::{probe, rest, stream};

#[derive(Parser)]
#[command(name = "lecturelive")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// List inputs with their CoreAudio UIDs
    Inputs,
    /// List outputs with their CoreAudio UIDs
    Outputs,
    #[command(subcommand)]
    Loopback(Loopback),
    /// Record an input (or Zoom through BlackHole) into a lecture folder until Ctrl-C
    Record {
        #[arg(long, conflicts_with = "device")]
        loopback: bool,
        /// Record Zoom (through LectureLive Loopback) and this input together: its UID or part of its name
        #[arg(long, conflicts_with_all = ["loopback", "device"])]
        mixed: Option<String>,
        /// Input device UID (see `inputs`)
        #[arg(long)]
        device: Option<String>,
        #[arg(long)]
        dir: Option<PathBuf>,
        /// Stop after this many seconds
        #[arg(long)]
        secs: Option<u64>,
        /// Delete closed recordings older than this many days that have no unresolved gap
        #[arg(long)]
        keep_days: Option<u32>,
        /// Stream to Grok speech-to-text and write the transcript (GROK_API_KEY from the repository's .env or the environment)
        #[arg(long)]
        stt: bool,
        /// A term to bias recognition toward (repeatable; up to 100, each at most 50 characters)
        #[arg(long = "keyterm", requires = "stt")]
        keyterms: Vec<String>,
    },
    /// A lecture with live notes, as live_notes.py does it: Enter takes a snapshot, a hint then Enter
    /// adds a focus hint, polish then Enter polishes and typesets the page, Ctrl-C stops (twice: stop
    /// waiting for recovery)
    Lecture(LectureArgs),
    #[command(subcommand)]
    Canary(Canary),
}

#[derive(clap::Args)]
struct LectureArgs {
    /// page: typeset the study page from the notes; spend: what the tool has cost; audit: whether the folder's
    /// audio is whole (exit 1 when anything is unexplained, 2 while something waits). Leave out to record.
    #[arg(value_parser = ["page", "spend", "audit"])]
    command: Option<String>,
    /// The lecture folder (default: the current directory)
    #[arg(long)]
    dir: Option<PathBuf>,
    /// Course name (default: the folder above Weeks/, else LECTURE_COURSE)
    #[arg(long)]
    course: Option<String>,
    /// Record BlackHole: Zoom through "LectureLive Loopback"
    #[arg(long, conflicts_with = "device")]
    loopback: bool,
    /// Record Zoom (through LectureLive Loopback) and this input together: its UID or part of its name
    #[arg(long, conflicts_with_all = ["loopback", "device"])]
    mixed: Option<String>,
    /// Audio input: its UID or part of its name (default: LECTURE_DEVICE)
    #[arg(long)]
    device: Option<String>,
    /// A term to bias recognition toward (repeatable; up to 100, each at most 50 characters)
    #[arg(long = "keyterm")]
    keyterms: Vec<String>,
    /// Notes file (default: lecture_notes_<today>.md)
    #[arg(long)]
    notes: Option<PathBuf>,
    /// Transcript file (default: lecture_transcript_<today>.txt)
    #[arg(long)]
    transcript: Option<PathBuf>,
    /// Where captured slides go (default: slides)
    #[arg(long = "slides-dir")]
    slides_dir: Option<PathBuf>,
    /// Stop after this many seconds
    #[arg(long)]
    secs: Option<u64>,
    /// Delete closed recordings older than this many days that have no unresolved gap
    #[arg(long)]
    keep_days: Option<u32>,
    /// Rebuild a corrupt sidecar from the notes, transcript and slides
    #[arg(long)]
    rebuild: bool,
}

#[derive(Subcommand)]
enum Loopback {
    /// Create or rebuild "LectureLive Loopback" (BlackHole + one physical output)
    Setup {
        #[arg(long)]
        output: Option<String>,
    },
    Status,
    /// Measure BlackHole while Zoom's Test Speaker plays
    Check {
        #[arg(long, default_value_t = 15)]
        secs: u64,
    },
}

#[derive(Subcommand)]
enum Canary {
    /// Loopback routing: status | on | off
    Route { action: String },
    /// List input devices
    Inputs,
    /// Record an input device to a 16 kHz WAV
    Record {
        #[arg(long)]
        device: String,
        #[arg(long, default_value_t = 30)]
        secs: u64,
        #[arg(long, default_value = "recordings")]
        dir: PathBuf,
    },
    /// Repair the header of an interrupted WAV
    Repair { wav: PathBuf },
    /// List windows
    Windows,
    /// Capture a window to PNG
    Capture {
        #[arg(long)]
        id: u32,
        #[arg(long)]
        out: PathBuf,
    },
    /// Stream a 16 kHz mono WAV to the STT websocket and log the protocol
    SttProbe {
        wav: PathBuf,
        #[arg(long)]
        finalize_after_secs: Option<f32>,
        #[arg(long, default_value = r#"{"type":"finalize"}"#)]
        finalize_message: String,
        #[arg(long)]
        log: PathBuf,
        #[arg(long, default_value = "wss://api.x.ai/v1/stt?model=grok-voice-transcribe-2.0&encoding=pcm&sample_rate=16000&interim_results=true&language=en")]
        url: String,
        /// Milliseconds between frames (0 sends as fast as the socket takes them)
        #[arg(long, default_value_t = 100)]
        pace_ms: u64,
    },
    /// Play a tone on one output device (no system output change)
    Tone {
        #[arg(long)]
        output: String,
        #[arg(long, default_value_t = 5.0)]
        secs: f32,
        #[arg(long, default_value_t = 0.1)]
        amp: f32,
    },
}

fn data_dir() -> Result<PathBuf> {
    let dir = dirs::data_dir().context("no Application Support dir")?.join("LectureLive");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Inputs => {
            for i in input::list_inputs()? {
                println!("{:<32} {:<40} {} Hz  {} ch", i.name, i.uid, i.sample_rate, i.channels);
            }
        }
        Cmd::Outputs => {
            for o in loopback::list_outputs()? {
                println!("{:<32} {:<40} {}", o.name, o.uid, if o.is_aggregate { "multi-output" } else { "" });
            }
        }
        Cmd::Loopback(l) => loopback_cmd(l)?,
        Cmd::Record { loopback, mixed, device, dir, secs, keep_days, stt, keyterms } => record(loopback, mixed, device, dir, secs, keep_days, stt, keyterms).await?,
        Cmd::Lecture(args) => lecture_cmd(args).await?,
        Cmd::Canary(c) => canary(c).await?,
    }
    Ok(())
}

fn loopback_cmd(l: Loopback) -> Result<()> {
    match l {
        Loopback::Setup { output } => {
            let (action, physical) = loopback::setup(output.as_deref())?;
            println!("{}: {action:?} with BlackHole 2ch (clock) + {physical}", loopback::LOOPBACK_NAME);
            println!("Zoom → Settings → Audio → Speaker: choose \"{}\". Keep the system output on a device without BlackHole.", loopback::LOOPBACK_NAME);
        }
        Loopback::Status => println!("{:#?}", loopback::status()?),
        Loopback::Check { secs } => {
            println!("Play Zoom's Test Speaker (Zoom → Settings → Audio) now; measuring BlackHole for {secs} s…");
            let dir = data_dir()?.join("checks");
            let mut loudest = 0f32;
            let r = input::record_for("BlackHole 2ch", Duration::from_secs(secs), &dir, |l| {
                println!("  {:>6.1} dBFS", level::dbfs(l));
                loudest = loudest.max(l);
            })?;
            let db = level::dbfs(loudest);
            anyhow::ensure!(db > -60.0, "no signal on BlackHole (loudest {db:.1} dBFS): Zoom's Speaker must be \"{}\"", loopback::LOOPBACK_NAME);
            println!("pass: loudest {db:.1} dBFS ({})", r.path.display());
        }
    }
    Ok(())
}

fn secs(samples: u64) -> f64 {
    samples as f64 / 16_000.0
}

async fn record(use_loopback: bool, mixed: Option<String>, device: Option<String>, dir: Option<PathBuf>, secs_limit: Option<u64>, keep_days: Option<u32>, stt: bool, keyterms: Vec<String>) -> Result<()> {
    match permission::microphone() {
        MicPermission::Denied | MicPermission::Restricted => {
            anyhow::bail!("microphone access is denied for this terminal: System Settings → Privacy & Security → Microphone")
        }
        p => println!("microphone permission: {p:?}"),
    }
    // A missing key or a bad keyterm stops the command before the folder is touched.
    let (stt_link, recovery) = if stt {
        dotenvy::dotenv().ok();
        let key = std::env::var("GROK_API_KEY").context("GROK_API_KEY is not set (the repository's .env, or the environment)")?;
        let link = stream::spawn(stream::SttConfig::new(key.clone(), keyterms.clone()))?;
        let client = rest::RestClient::new(rest::RestConfig::new(key, keyterms))?;
        (Some(link), Some(rest::spawn_recovery(client)))
    } else {
        (None, None)
    };
    let dir = match dir {
        Some(d) => d,
        None => data_dir()?.join("record"),
    };
    std::fs::create_dir_all(&dir)?;
    let _lock = FolderLock::acquire(&dir)?;
    if let Some(restored) = launch::restore_abandoned_route(&data_dir()?.join("route.json"))? {
        println!("undid a canary route left behind; default output restored: {restored}");
    }
    let stem = format!("lecture_notes_{}", chrono::Local::now().format("%Y%m%d"));
    let retention = keep_days.map_or(Retention::KeepAll, Retention::KeepDays);
    let report = launch::recover(&dir, retention, chrono::Local::now())?;
    for (p, n) in &report.repaired {
        println!("repaired {} ({:.1} s)", p.display(), *n as f64 / 16_000.0);
    }
    for p in &report.missing {
        println!("missing {} (marked as a gap)", p.display());
    }
    for p in &report.pruned {
        println!("deleted {} (retention)", p.display());
    }
    for g in &report.untranscribed {
        println!(
            "transcript to recover: {:.1}–{:.1} s of recording {} (recovered by the next --stt session of that day)",
            secs(g.start_sample),
            g.end_sample.map_or(0.0, secs),
            g.recording_id
        );
    }
    let uid = if let Some(m) = &mixed {
        resolve_mixed(m)?.0
    } else if use_loopback {
        let s = loopback::status()?;
        anyhow::ensure!(s.blackhole_present, "BlackHole 2ch is not installed (brew install blackhole-2ch)");
        if !s.present {
            println!("warning: {} is not set up (`lecturelive loopback setup`); recording BlackHole anyway", loopback::LOOPBACK_NAME);
        }
        loopback::BLACKHOLE_UID.to_string()
    } else {
        device.context("give --loopback or --device <UID> (`lecturelive inputs` lists them)")?
    };

    let (handle, mut notes) = coordinator::spawn(SessionConfig { dir, stem, stt: stt_link, recovery, ..Default::default() }, source_for(&uid));
    let mut watch = (use_loopback || mixed.is_some()).then(|| SilenceWatch::new(-60.0, 10));
    let timer = async {
        match secs_limit {
            Some(s) => tokio::time::sleep(Duration::from_secs(s)).await,
            None => std::future::pending().await,
        }
    };
    tokio::pin!(timer);
    let mut stopping = false;
    let mut seconds = 0u64;
    loop {
        tokio::select! {
            n = notes.recv() => match n {
                None => break,
                Some(Notification::Recording { path }) => println!("recording to {}", path.display()),
                Some(Notification::Level(l)) => {
                    seconds += 1;
                    if seconds % 10 == 0 {
                        println!("{:>6} s  {:>6.1} dBFS", seconds, level::dbfs(l));
                    }
                    if watch.as_mut().is_some_and(|w| w.observe(l)) {
                        println!("warning: 10 s of silence on BlackHole — is Zoom's Speaker \"{}\"?", loopback::LOOPBACK_NAME);
                    }
                }
                Some(Notification::Gap(g)) => println!("gap: {:?} from sample {} to {:?} of {}", g.kind, g.start_sample, g.end_sample, g.recording_id),
                Some(Notification::DeviceGone { uid }) => println!(
                    "input {uid} disappeared at {}; waiting for it to return, and nothing switches by itself. {}",
                    chrono::Local::now().format("%H:%M:%S"),
                    other_inputs(&uid)
                ),
                Some(Notification::DeviceBack { uid }) => println!("input {uid} is back at {}; recording continues in a new file", chrono::Local::now().format("%H:%M:%S")),
                Some(Notification::Failed(msg)) => eprintln!("session failed: {msg}"),
                Some(Notification::Stt(s)) => match s {
                    SttStatus::Connected => println!("transcribing"),
                    SttStatus::Retrying { after, reason } => println!("transcription interrupted ({reason}); reconnecting in {} s", after.as_secs()),
                    SttStatus::Refused(m) => eprintln!("transcription refused: {}. Recording continues without it.", sentence(&m)),
                    SttStatus::ServerError(m) => eprintln!("transcription server: {m}"),
                    SttStatus::Stopped(m) => eprintln!("transcription stopped: {m}"),
                },
                Some(Notification::Open { .. }) => {} // the terminal shows closed utterances only
                Some(Notification::Segment(s)) => {
                    let tag = if s.source == SegmentSource::Recovered { "   (recovered)" } else { "" };
                    println!("[{}] {}{tag}", s.said_at.format("%H:%M:%S"), s.text);
                }
                Some(Notification::Recovered(g)) => println!(
                    "recovered the transcript of {:.1}–{:.1} s of recording {}",
                    secs(g.start_sample),
                    g.end_sample.map_or(0.0, secs),
                    g.recording_id
                ),
                Some(Notification::RecoveryFailed(m)) => eprintln!("recovery: {m}"),
                Some(Notification::SpendFailed(m)) => eprintln!("warning: {m}"),
                Some(Notification::SourceEnded) => {}
            },
            _ = tokio::signal::ctrl_c(), if !stopping => { stopping = true; handle.request_stop(); }
            _ = &mut timer, if !stopping => { stopping = true; handle.request_stop(); }
        }
    }
    let report = handle.finish().await?;
    for (p, n) in &report.recordings {
        println!("{} — {:.1} s", p.display(), *n as f64 / 16_000.0);
    }
    println!("gaps in the sidecar: {}, stream errors: {}", report.gaps, report.stream_errors);
    if stt {
        println!("segments: {}, transcript gaps still to recover: {}", report.segments, report.unresolved);
    }
    Ok(())
}

async fn canary(c: Canary) -> Result<()> {
    match c {
        Canary::Route { action } => {
            let p = data_dir()?.join("route.json");
            match action.as_str() {
                "on" => println!("routed; saved previous output {:?}", routing::enable_loopback(&p)?.previous_output_uid),
                "off" => println!("restored default output: {}", routing::disable_loopback(&p)?),
                _ => println!("{:#?}", routing::route_status(&p)?),
            }
        }
        Canary::Inputs => {
            for i in input::list_inputs()? {
                println!("{:<40} {} Hz  {} ch", i.name, i.sample_rate, i.channels);
            }
        }
        Canary::Record { device, secs, dir } => {
            let r = input::record_for(&device, Duration::from_secs(secs), &dir, |level| {
                println!("level {:>5.1} dBFS", 20.0 * level.max(1e-6).log10())
            })?;
            println!("{} — {:.1} s, dropped callbacks: {}", r.path.display(), r.samples as f64 / 16_000.0, r.dropped_callbacks);
        }
        Canary::Repair { wav } => println!("{} samples", recorder::repair_header(&wav)?),
        Canary::Windows => {
            for w in window::list_windows()? {
                println!("{:>8}  {:<20} {:<50} {}x{}", w.id, w.app, w.title, w.width, w.height);
            }
        }
        Canary::Capture { id, out } => {
            let (w, h) = window::capture_window(id, &out)?;
            println!("{} {w}x{h}", out.display());
        }
        Canary::SttProbe { wav, finalize_after_secs, finalize_message, log, url, pace_ms } => {
            dotenvy::dotenv().ok();
            let api_key = std::env::var("GROK_API_KEY").context("GROK_API_KEY not set")?;
            let mut reader = hound::WavReader::open(&wav)?;
            let spec = reader.spec();
            anyhow::ensure!(spec.sample_rate == 16_000 && spec.channels == 1 && spec.bits_per_sample == 16, "need 16 kHz mono PCM16");
            let pcm: Vec<i16> = reader.samples::<i16>().collect::<Result<_, _>>()?;
            let s = probe::probe(probe::ProbeOptions {
                url,
                api_key,
                pcm,
                pace: Duration::from_millis(pace_ms),
                finalize_after_frames: finalize_after_secs.map(|s| (s * 10.0) as usize),
                finalize_message,
                log_path: log,
            })
            .await?;
            println!("{s:#?}");
        }
        Canary::Tone { output, secs, amp } => lecturelive_core::audio::tone::play_tone(&output, secs, amp)?,
    }
    Ok(())
}

const REPO_ENV: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../.env");
/// The Python CLI's ledger, taken over by the app's (spec §8).
const CLI_LEDGER: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../spend.jsonl");

/// The environment, a .env here or above, then the repository's .env.
fn env_value(name: &str) -> Option<String> {
    let _ = dotenvy::dotenv();
    std::env::var(name).ok().filter(|v| !v.is_empty()).or_else(|| dotenvy::from_path_iter(REPO_ENV).ok()?.flatten().find(|(k, _)| k == name).map(|(_, v)| v))
}

/// A message's own final period dropped before the sentence that follows it.
fn sentence(m: &str) -> &str {
    m.trim_end().trim_end_matches('.')
}

fn paint() -> spend::Paint {
    use std::io::IsTerminal;
    spend::Paint { color: std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none(), truecolor: matches!(std::env::var("COLORTERM").as_deref(), Ok("truecolor" | "24bit")) }
}

/// The CLI's `say`: one event line, its mark, what it is, what happened.
fn say(out: &mut impl Write, p: spend::Paint, kind: &str, label: &str, detail: &str) {
    let (mark, colour) = match kind {
        "slide" => ("▣", "teal"),
        "notes" => ("◆", "teal"),
        "page" => ("✦", "teal"),
        "done" => ("✓", "teal"),
        _ => ("▲", "red"),
    };
    let _ = writeln!(out, "{}", format!("  {} {}  {detail}", p.paint(mark, &[colour]), p.paint(label, &["bold"])).trim_end());
}

fn plural(n: usize, word: &str) -> String {
    format!("{n} {word}{}", if n == 1 { "" } else { "s" })
}

/// An input by UID, or by part of its name; checked before the folder is touched.
fn resolve_input(loopback: bool, device: Option<String>) -> Result<(String, String)> {
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

/// The fallback the CLI offers while an input is gone (spec §4.1): the other inputs, to start again with.
fn other_inputs(gone: &str) -> String {
    let others: Vec<String> = input::list_inputs().unwrap_or_default().into_iter().filter(|i| i.uid != gone).map(|i| format!("{} ({})", i.name, i.uid)).collect();
    if others.is_empty() {
        return "No other input is connected; Ctrl-C stops.".into();
    }
    format!("To record from another input, stop (Ctrl-C) and start again with --device <UID>: {}.", others.join(", "))
}

/// `--mixed <input>`: Zoom through BlackHole and that input together, once mixed mode has passed its drift test.
/// Returns the source as `mixed:<input UID>` and its name.
fn resolve_mixed(want: &str) -> Result<(String, String)> {
    anyhow::ensure!(MIXED_MODE, "mixed mode is disabled: it did not pass its drift test (docs/milestones.md, M6)");
    anyhow::ensure!(loopback::status()?.blackhole_present, "BlackHole 2ch is not installed (brew install blackhole-2ch)");
    let (uid, name) = resolve_input(false, Some(want.to_string()))?;
    Ok((format!("mixed:{uid}"), format!("Zoom and {name}")))
}

/// The source a UID names: `mixed:<input>` is Zoom through BlackHole with that input (spec §4.2).
fn source_for(uid: &str) -> Box<dyn Source> {
    match uid.strip_prefix("mixed:") {
        Some(input) => Box::new(MixedSource::new(loopback::BLACKHOLE_UID, input)),
        None => Box::new(DeviceSource { uid: uid.to_string(), fallback: Fallback::default() }),
    }
}

/// Prints the lecture's events in the CLI's lines; the loopback silence warning as `record` gives it.
fn show(out: &mut impl Write, p: spend::Paint, e: &Event, watch: &mut Option<SilenceWatch>) {
    match e {
        Event::Session(n) => match n {
            Notification::Segment(s) => {
                let tag = if s.source == SegmentSource::Recovered { p.paint("  (recovered)", &["dim"]) } else { String::new() };
                let _ = writeln!(out, "  {}  {}{tag}", p.paint(&s.said_at.format("%H:%M:%S").to_string(), &["dim"]), s.text);
            }
            Notification::Level(l) => {
                if watch.as_mut().is_some_and(|w| w.observe(*l)) {
                    say(out, p, "warn", "no signal", &format!("10 s of silence on BlackHole: is Zoom's Speaker \"{}\"?", loopback::LOOPBACK_NAME));
                }
            }
            Notification::Recording { path } => { let _ = writeln!(out, "{}", p.paint(&format!("  recording to {}", path.display()), &["dim"])); }
            Notification::Gap(g) => say(out, p, "warn", "gap", &format!("{:?} from sample {} to {:?} of {}", g.kind, g.start_sample, g.end_sample, g.recording_id)),
            Notification::DeviceGone { uid } => say(out, p, "warn", "input gone", &format!("{uid}; waiting for it to return, and nothing switches by itself. {}", other_inputs(uid))),
            Notification::DeviceBack { uid } => say(out, p, "done", "input back", &format!("{uid}; recording continues in a new file")),
            Notification::Failed(m) => say(out, p, "warn", "session failed", m),
            Notification::Stt(s) => match s {
                SttStatus::Connected => { let _ = writeln!(out, "{}", p.paint("  transcribing", &["dim"])); }
                SttStatus::Retrying { after, reason } => say(out, p, "warn", "transcription interrupted", &format!("{reason}; reconnecting in {} s", after.as_secs())),
                SttStatus::Refused(m) => say(out, p, "warn", "transcription refused", &format!("{}. Recording continues without it.", sentence(m))),
                SttStatus::ServerError(m) => say(out, p, "warn", "transcription server", m),
                SttStatus::Stopped(m) => say(out, p, "warn", "transcription stopped", m),
            },
            Notification::Open { .. } | Notification::SourceEnded => {}
            Notification::Recovered(g) => say(out, p, "done", "recovered", &format!("the transcript of {:.1}–{:.1} s of recording {}", secs(g.start_sample), g.end_sample.map_or(0.0, secs), g.recording_id)),
            Notification::RecoveryFailed(m) => say(out, p, "warn", "recovery", m),
            Notification::SpendFailed(m) => say(out, p, "warn", "spend", m),
        },
        Event::Busy(m) => { let _ = writeln!(out, "{}", p.paint(&format!("  … {m}"), &["dim"])); }
        Event::Preview(_) => {} // the committed block is printed instead, as the CLI does
        Event::NothingNew => say(out, p, "notes", "snapshot", "nothing new since the last one"),
        Event::Committed { words, slides, block, usd, confirmed, missing, .. } => {
            for line in block.trim().lines().filter(|l| !l.starts_with("<!-- ")) {
                let _ = writeln!(out, "  {} {}", p.paint("│", &["teal"]), p.paint(line, &["dim"]));
            }
            let mut detail = format!("{} and {} folded in  {}", plural(*words, "word"), plural(*slides, "slide"), p.paint(&spend::money(*usd), &["dim"]));
            if *missing > 0 {
                detail += &format!("  ({} not placed by the model, listed at the end)", plural(*missing, "slide"));
            }
            if !confirmed {
                detail += "  (transcription still catching up; the rest goes into the next snapshot)";
            }
            say(out, p, "notes", "notes", &detail);
        }
        Event::SnapshotFailed(m) => say(out, p, "warn", "snapshot failed", m),
        Event::Polished { backup, usd, .. } => {
            let name = backup.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            say(out, p, "done", "polished", &format!("previous version in .live_notes/{name}  {}", p.paint(&spend::money(*usd), &["dim"])));
        }
        Event::PolishStopped(m) => say(out, p, "warn", "polish stopped", m),
        Event::PolishFailed(m) => say(out, p, "warn", "polish failed", m),
        Event::Cancelled(what) => say(out, p, "warn", "cancelled", &format!("{what}; nothing was written, everything is kept for the next snapshot")),
        Event::Page { outcome, usd } => {
            let name = outcome.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let length = p.paint(&format!("{} words of {} allowed", outcome.words, outcome.budget), &[if outcome.words > outcome.budget as usize { "red" } else { "dim" }]);
            let mut detail = format!("{name}  {length}  {}", p.paint(&spend::money(*usd), &["dim"]));
            if outcome.cached {
                detail += "  (notes unchanged since they were typeset: only the design reapplied, free)";
            }
            if !outcome.missing.is_empty() {
                detail += &format!("  (missing: {})", outcome.missing.join(", "));
            }
            say(out, p, "done", "page", &detail);
        }
        Event::PageFailed(m) => say(out, p, "warn", "page failed", m),
        Event::Slide { index, file, auto, uncertain, .. } => {
            let how = match (auto, uncertain) {
                (true, true) => " (auto, still changing)",
                (true, false) => " (auto)",
                _ => "",
            };
            say(out, p, "slide", &format!("slide {index}{how}"), &format!("{}, into the next snapshot", Path::new(file).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()))
        }
        Event::Capture(s) => {
            let (label, detail) = s.words();
            say(out, p, "slide", &label.to_lowercase(), &detail)
        }
        Event::CaptureMoved { note, .. } => say(out, p, "slide", "found again", note),
        Event::Warning(m) => say(out, p, "warn", "warning", m),
    }
}

/// The stdin grammar: an empty line takes a snapshot, `polish` polishes, and any other line is the snapshot's hint.
fn parse_line(line: &str) -> Op {
    let text = line.trim().to_string();
    if text.eq_ignore_ascii_case("polish") { Op::Polish } else { Op::Snapshot(text) }
}

/// The start-up report after `prepare`: what launch did to the recordings, what the folder's
/// initialisation found, then the lecture's header lines.
fn print_prepared(out: &mut impl Write, p: spend::Paint, ready: &start::Prepared, files: &LectureFiles, course: &str, name: &str, input_name: &str) {
    for (path, n) in &ready.launch.repaired {
        say(out, p, "done", "repaired", &format!("{} ({:.1} s)", path.display(), secs(*n)));
    }
    for path in &ready.launch.missing {
        say(out, p, "warn", "missing", &format!("{} (marked as a gap)", path.display()));
    }
    for path in &ready.launch.pruned {
        say(out, p, "done", "deleted", &format!("{} (retention)", path.display()));
    }
    let init = &ready.init;
    match init.how {
        How::Created => say(out, p, "notes", "notes", &format!("{} created", files.notes.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())),
        How::Migrated => say(out, p, "notes", "migrated", "this folder's Python CLI state is now the app's (the Python CLI no longer writes here)"),
        How::Rebuilt => say(out, p, "warn", "rebuilt", "the state was rebuilt from the notes: everything after the last <!-- --> marker is pending"),
        How::Resumed => {}
    }
    if let Some(m) = init.legacy_commit {
        say(out, p, "warn", "recovered", m);
    }
    if let Some(kept) = &init.corrupt_kept {
        say(out, p, "warn", "rebuilt", &format!("the corrupt state is kept as {}", kept.display()));
    }
    match init.journal {
        Recovered::Completed => say(out, p, "done", "recovered", "the last snapshot was fully written"),
        Recovered::Truncated => say(out, p, "warn", "recovered", "removed a half-written snapshot; its material is queued again"),
        Recovered::NotAppended => say(out, p, "warn", "recovered", "an interrupted snapshot never reached the notes; its material is queued again"),
        Recovered::Nothing => {}
    }
    if init.external_edit {
        say(out, p, "notes", "notes", "edited outside the app since the last session; kept as they are");
    }
    for (stem, r) in &ready.other_days {
        say(out, p, "done", "recovered", &format!("{stem}'s transcript gaps{}", if r.unresolved > 0 { format!(", {} still waiting", r.unresolved) } else { String::new() }));
    }

    let _ = writeln!(out);
    let _ = writeln!(out, "  {}  {}  {name}", p.paint(course, &["bold"]), p.paint("›", &["dim"]));
    let waiting = if init.pending_segments > 0 || init.pending_slides > 0 {
        format!(", resumed with {} and {} for the next snapshot", plural(init.pending_segments as usize, "line"), plural(init.pending_slides, "slide"))
    } else {
        String::new()
    };
    let _ = writeln!(out, "{}", p.paint(&format!("  listening on {input_name}{waiting}"), &["dim"]));
    let _ = writeln!(out, "{}", p.paint("  ⏎ snapshot   a hint ⏎   polish ⏎   ^C stop", &["dim"]));
    let _ = writeln!(out);
}

/// The end summary: what was saved, what still waits for the next session, and what the lecture cost today.
fn print_end(out: &mut impl Write, p: spend::Paint, files: &LectureFiles, report: &StopReport, spend: &Spend) {
    let file_name = |f: &Path| f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let _ = writeln!(out);
    let _ = writeln!(out, "  {} {}  {}  {}", p.paint("✓", &["teal"]), p.paint("saved", &["bold"]), file_name(&files.notes), file_name(&files.transcript));
    if let Some(e) = &report.last_snapshot {
        say(out, p, "warn", "notes", &format!("the last snapshot failed ({e}); the next session in this folder adds what it missed"));
    }
    if report.unresolved > 0 {
        let _ = writeln!(out, "{}", p.paint(&format!("    {} still to recover; the next session in this folder does it", plural(report.unresolved, "transcript gap")), &["dim"]));
    }
    let _ = writeln!(out, "{}", p.paint(&format!("    {} spent on this lecture today; `lecture spend` has the rest", spend::money(spend.lecture_total())), &["dim"]));
    let _ = writeln!(out);
}

async fn lecture_cmd(a: LectureArgs) -> Result<()> {
    let p = paint();
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
        say(&mut std::io::stdout().lock(), p, "page", "page", &format!("distilling {} with {}, a few minutes", files.notes.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), chat::MODEL));
        let printer = tokio::spawn(async move {
            let mut none = None;
            while let Some(e) = ev_rx.recv().await {
                show(&mut std::io::stdout().lock(), p, &e, &mut none);
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
    let mut announce = |stem: &str| say(&mut std::io::stdout().lock(), p, "notes", "recovering", &format!("{stem}'s transcript gaps, before today's session"));
    let ready = start::prepare(&files, &title, a.rebuild, a.keep_days.map_or(Retention::KeepAll, Retention::KeepDays), &recovery, Some(spend.clone()), &mut announce).await?;
    print_prepared(&mut std::io::stdout().lock(), p, &ready, &files, &course, &name, &input_name);

    let session = SessionConfig { dir: dir.clone(), stem: files.stem.clone(), stt: Some(stt_link), recovery: Some(recovery()?), spend: Some(spend.clone()), transcript: Some(files.transcript.clone()) };
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    let stdin_tx = cmd_tx.clone();
    std::thread::spawn(move || {
        for line in std::io::stdin().lines().map_while(Result::ok) {
            if stdin_tx.send(LectureCommand::Op(parse_line(&line))).is_err() {
                return;
            }
        }
    });
    let stop_tx = cmd_tx.clone();
    tokio::spawn(async move {
        let mut presses = 0;
        while tokio::signal::ctrl_c().await.is_ok() {
            presses += 1;
            match presses {
                1 => say(&mut std::io::stdout().lock(), p, "notes", "stopping", "finishing the transcript and recovery, then a last snapshot (Ctrl-C again stops waiting for recovery)"),
                2 => say(&mut std::io::stdout().lock(), p, "warn", "stopping", "no longer waiting for recovery or queued requests; its gaps wait for the next session (Ctrl-C again quits at once)"),
                _ => {
                    // The recording is durable to its last second and gaps and journals are on disk: the next
                    // session in this folder repairs, recovers and notes what is left.
                    say(&mut std::io::stdout().lock(), p, "warn", "quit", "stopped at once; the next session in this folder picks up what was left");
                    std::process::exit(130);
                }
            }
            if stop_tx.send(LectureCommand::Stop).is_err() {
                return;
            }
        }
    });
    if let Some(limit) = a.secs {
        let timer_tx = cmd_tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(limit)).await;
            let _ = timer_tx.send(LectureCommand::Stop);
        });
    }
    drop(cmd_tx);
    let mut watch = (uid == loopback::BLACKHOLE_UID || uid.starts_with("mixed:")).then(|| SilenceWatch::new(-60.0, 10));
    let printer = tokio::spawn(async move {
        while let Some(e) = ev_rx.recv().await {
            show(&mut std::io::stdout().lock(), p, &e, &mut watch);
        }
    });
    let watch_slides = SlideWatch { screenshots: lecture::screenshot_dir(), poll: Duration::from_secs(1) };
    let result = lecture::run(lec, session, source_for(&uid), watch_slides, None, cmd_rx, ev_tx).await;
    printer.await?;
    let report = result?;
    print_end(&mut std::io::stdout().lock(), p, &files, &report, &spend);
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

/// Task 1's characterization goldens (M7 plan §J): the plain lines the CLI prints today, byte for
/// byte, so the module split changes none of them.
#[cfg(test)]
mod goldens {
    use super::*;
    use lecturelive_core::capture::detect::Region;
    use lecturelive_core::capture::select::{Descriptor, Selection};
    use lecturelive_core::capture::worker::CaptureState;
    use lecturelive_core::notes::page::PageOutcome;
    use lecturelive_core::session::folder::InitReport;
    use lecturelive_core::session::launch::LaunchReport;
    use lecturelive_core::session::segments::{self, Segment};
    use lecturelive_core::session::sidecar::{Gap, Sidecar};
    use lecturelive_core::session::spend::SpendKind;

    const OFF: spend::Paint = spend::Paint { color: false, truecolor: false };
    const ANSI: spend::Paint = spend::Paint { color: true, truecolor: false };
    const TRUE: spend::Paint = spend::Paint { color: true, truecolor: true };

    fn shown(p: spend::Paint, e: &Event, watch: &mut Option<SilenceWatch>) -> String {
        let mut out = Vec::new();
        show(&mut out, p, e, watch);
        String::from_utf8(out).unwrap()
    }

    fn plain(e: &Event) -> String {
        shown(OFF, e, &mut None)
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lecturelive-goldens-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A fixed gap, read the way core reads one, so no test needs the uuid crate.
    fn gap() -> Gap {
        let p = scratch("gap").join("day.v2.json");
        std::fs::write(&p, r#"{"version":2,"gaps":[{"recording_id":"11111111-1111-1111-1111-111111111111","start_sample":3200,"end_sample":16000,"kind":"recorder_overflow","resolved":false}]}"#).unwrap();
        Sidecar::load(&p).unwrap().unwrap().gaps[0].clone()
    }

    /// A fixed segment at a fixed instant, read the way core reads one (`live` or `recovered`).
    fn segment(source: &str) -> Segment {
        let p = scratch("segment").join("day.segments.jsonl");
        let mut line = format!(r#"{{"id":0,"recording_id":"11111111-1111-1111-1111-111111111111","start_sample":1600,"end_sample":8000,"said_at":"2026-09-26T10:42:03+02:00","start":"2026-09-26T10:42:03+02:00","end":"2026-09-26T10:42:08+02:00","text":"gradient descent","words":[],"source":"{source}"}}"#);
        line.push('\n');
        std::fs::write(&p, line).unwrap();
        segments::read(&p).unwrap()[0].clone()
    }

    #[test]
    fn session_notification_lines_colour_off() {
        let seg = segment("live");
        assert_eq!(plain(&Event::Session(Notification::Segment(seg.clone()))), format!("  {}  gradient descent\n", seg.said_at.format("%H:%M:%S")));
        let rec = segment("recovered");
        assert_eq!(plain(&Event::Session(Notification::Segment(rec.clone()))), format!("  {}  gradient descent  (recovered)\n", rec.said_at.format("%H:%M:%S")));
        assert_eq!(plain(&Event::Session(Notification::Recording { path: PathBuf::from("/tmp/lec/recordings/session_1.wav") })), "  recording to /tmp/lec/recordings/session_1.wav\n");
        assert_eq!(plain(&Event::Session(Notification::Gap(gap()))), "  ▲ gap  RecorderOverflow from sample 3200 to Some(16000) of 11111111-1111-1111-1111-111111111111\n");
        assert_eq!(plain(&Event::Session(Notification::DeviceBack { uid: "BlackHole-UID".into() })), "  ✓ input back  BlackHole-UID; recording continues in a new file\n");
        assert_eq!(plain(&Event::Session(Notification::Failed("disk full".into()))), "  ▲ session failed  disk full\n");
        assert_eq!(plain(&Event::Session(Notification::Recovered(gap()))), "  ✓ recovered  the transcript of 0.2–1.0 s of recording 11111111-1111-1111-1111-111111111111\n");
        assert_eq!(plain(&Event::Session(Notification::RecoveryFailed("server busy".into()))), "  ▲ recovery  server busy\n");
        assert_eq!(plain(&Event::Session(Notification::SpendFailed("ledger unwritable".into()))), "  ▲ spend  ledger unwritable\n");
        assert_eq!(plain(&Event::Session(Notification::Open { stable: "gra".into(), tentative: "descent".into() })), "", "the terminal shows closed utterances only");
        assert_eq!(plain(&Event::Session(Notification::SourceEnded)), "");
    }

    #[test]
    fn stt_status_lines_colour_off() {
        assert_eq!(plain(&Event::Session(Notification::Stt(SttStatus::Connected))), "  transcribing\n");
        assert_eq!(plain(&Event::Session(Notification::Stt(SttStatus::Retrying { after: Duration::from_secs(5), reason: "socket closed".into() }))), "  ▲ transcription interrupted  socket closed; reconnecting in 5 s\n");
        assert_eq!(plain(&Event::Session(Notification::Stt(SttStatus::Refused("bad key.".into())))), "  ▲ transcription refused  bad key. Recording continues without it.\n");
        assert_eq!(plain(&Event::Session(Notification::Stt(SttStatus::ServerError("500 again".into())))), "  ▲ transcription server  500 again\n");
        assert_eq!(plain(&Event::Session(Notification::Stt(SttStatus::Stopped("the worker stopped".into())))), "  ▲ transcription stopped  the worker stopped\n");
    }

    #[test]
    fn level_lines_carry_only_the_loopback_silence_warning() {
        let quiet = 10f32.powf(-80.0 / 20.0); // −80 dBFS, below the −60 dBFS threshold
        assert_eq!(plain(&Event::Session(Notification::Level(0.5))), "", "without a watch nothing is printed");
        let mut watch = Some(SilenceWatch::new(-60.0, 10));
        let mut seen = String::new();
        for _ in 0..9 {
            seen += &shown(OFF, &Event::Session(Notification::Level(quiet)), &mut watch);
        }
        assert_eq!(seen, "");
        seen += &shown(OFF, &Event::Session(Notification::Level(quiet)), &mut watch);
        assert_eq!(seen, "  ▲ no signal  10 s of silence on BlackHole: is Zoom's Speaker \"LectureLive Loopback\"?\n");
        seen += &shown(OFF, &Event::Session(Notification::Level(quiet)), &mut watch);
        assert_eq!(seen, "  ▲ no signal  10 s of silence on BlackHole: is Zoom's Speaker \"LectureLive Loopback\"?\n", "one warning per silent stretch");
    }

    /// `DeviceGone`'s offer lists this machine's inputs, so only the prefix and the offer's shape are pinned.
    #[test]
    fn device_gone_keeps_a_stable_prefix_before_the_machine_input_list() {
        let line = plain(&Event::Session(Notification::DeviceGone { uid: "Gone-UID".into() }));
        let prefix = "  ▲ input gone  Gone-UID; waiting for it to return, and nothing switches by itself. ";
        let rest = line.strip_prefix(prefix).unwrap_or_else(|| panic!("the stable prefix was lost: {line:?}"));
        assert!(
            rest == "No other input is connected; Ctrl-C stops."
                || rest.starts_with("To record from another input, stop (Ctrl-C) and start again with --device <UID>: "),
            "the machine's inputs follow the fixed offer: {rest:?}"
        );
    }

    #[test]
    fn busy_preview_and_snapshot_outcomes_colour_off() {
        assert_eq!(plain(&Event::Busy("snapshot, 12 words to grok-4".into())), "  … snapshot, 12 words to grok-4\n");
        assert_eq!(plain(&Event::Preview("partial answer".into())), "", "the committed block is printed instead");
        assert_eq!(plain(&Event::NothingNew), "  ◆ snapshot  nothing new since the last one\n");
        assert_eq!(plain(&Event::SnapshotFailed("the model refused; everything is kept for the next one".into())), "  ▲ snapshot failed  the model refused; everything is kept for the next one\n");
        assert_eq!(plain(&Event::Cancelled("the snapshot".into())), "  ▲ cancelled  the snapshot; nothing was written, everything is kept for the next snapshot\n");
    }

    #[test]
    fn committed_lines_colour_off() {
        let block = "\n<!-- 10:42:03 -->\n## Sampling distributions\n\n- Larger samples reduce standard error.\n";
        let e = Event::Committed { words: 486, slides: 2, block: block.into(), usd: 0.02, confirmed: true, removed: 0, missing: 0, revision: 3 };
        assert_eq!(plain(&e), "  │ ## Sampling distributions\n  │ \n  │ - Larger samples reduce standard error.\n  ◆ notes  486 words and 2 slides folded in  $0.02\n");
        let e = Event::Committed { words: 1, slides: 0, block: "<!-- 10:43:00 -->\n- one line".into(), usd: 0.001, confirmed: false, removed: 0, missing: 2, revision: 4 };
        assert_eq!(plain(&e), "  │ - one line\n  ◆ notes  1 word and 0 slides folded in  <$0.01  (2 slides not placed by the model, listed at the end)  (transcription still catching up; the rest goes into the next snapshot)\n");
    }

    #[test]
    fn polish_outcomes_colour_off() {
        let e = Event::Polished { backup: PathBuf::from("/tmp/lec/.live_notes/lecture_notes_20260926_104152.md"), usd: 0.04, revision: 5 };
        assert_eq!(plain(&e), "  ✓ polished  previous version in .live_notes/lecture_notes_20260926_104152.md  $0.04\n");
        assert_eq!(plain(&Event::PolishStopped("the snapshot before it failed (read notes); the notes are unchanged".into())), "  ▲ polish stopped  the snapshot before it failed (read notes); the notes are unchanged\n");
        assert_eq!(plain(&Event::PolishFailed("read notes: denied; the notes are unchanged".into())), "  ▲ polish failed  read notes: denied; the notes are unchanged\n");
    }

    #[test]
    fn page_outcomes_colour_off() {
        let outcome = PageOutcome { path: PathBuf::from("/tmp/lec/study_page.html"), words: 1200, budget: 1500, cached: false, missing: vec![] };
        assert_eq!(plain(&Event::Page { outcome, usd: 0.03 }), "  ✓ page  study_page.html  1200 words of 1500 allowed  $0.03\n");
        let outcome = PageOutcome { path: PathBuf::from("/tmp/lec/study_page.html"), words: 1600, budget: 1500, cached: true, missing: vec!["slide 3"] };
        assert_eq!(plain(&Event::Page { outcome, usd: 0.0 }), "  ✓ page  study_page.html  1600 words of 1500 allowed  $0.00  (notes unchanged since they were typeset: only the design reapplied, free)  (missing: slide 3)\n");
        assert_eq!(plain(&Event::PageFailed("the model refused. The notes are unchanged; `lecture page` tries again.".into())), "  ▲ page failed  the model refused. The notes are unchanged; `lecture page` tries again.\n");
    }

    #[test]
    fn slide_and_capture_lines_colour_off() {
        let now = chrono::Local::now();
        assert_eq!(plain(&Event::Slide { index: 17, file: "slides/slide_17_103941.png".into(), auto: true, uncertain: false, shown_at: now }), "  ▣ slide 17 (auto)  slide_17_103941.png, into the next snapshot\n");
        assert_eq!(plain(&Event::Slide { index: 18, file: "slides/slide_18_104152.png".into(), auto: true, uncertain: true, shown_at: now }), "  ▣ slide 18 (auto, still changing)  slide_18_104152.png, into the next snapshot\n");
        assert_eq!(plain(&Event::Slide { index: 19, file: "slides/slide_19_104201.png".into(), auto: false, uncertain: false, shown_at: now }), "  ▣ slide 19  slide_19_104201.png, into the next snapshot\n");
        assert_eq!(plain(&Event::Capture(CaptureState::Unbound)), "  ▣ no window  choose the window with the slides in the slides strip\n");
        assert_eq!(plain(&Event::Capture(CaptureState::Watching { window: "Zoom Meeting".into() })), "  ▣ watching  Zoom Meeting\n");
        assert_eq!(plain(&Event::Capture(CaptureState::Paused { window: "Zoom Meeting".into(), reason: "the window is minimised".into() })), "  ▣ paused  Zoom Meeting: the window is minimised\n");
        assert_eq!(plain(&Event::Capture(CaptureState::Asking { window: "Zoom Meeting".into(), reason: "it is not where it was".into(), candidates: vec![] })), "  ▣ asking  it is not where it was; choose in the slides strip which window to watch for Zoom Meeting\n");
        assert_eq!(plain(&Event::Capture(CaptureState::Denied)), "  ▣ screen recording  Screen Recording is off for LectureLive: System Settings → Privacy & Security → Screen & System Audio Recording\n");
        assert_eq!(plain(&Event::Capture(CaptureState::Failing { window: "Zoom Meeting".into(), reason: "3 failed captures in a row".into() })), "  ▣ capture failing  Zoom Meeting: 3 failed captures in a row\n");
        let selection = Selection { descriptor: Descriptor { bundle_id: Some("us.zoom.xos".into()), app: "Zoom".into(), title: "Zoom Meeting".into(), width: 1600, height: 900 }, region: Region::WHOLE, leave_out: vec![], sizes: vec![] };
        assert_eq!(plain(&Event::CaptureMoved { selection, note: "the slide moved with the window; watching it at the new size".into() }), "  ▣ found again  the slide moved with the window; watching it at the new size\n");
        assert_eq!(plain(&Event::Warning("slide 5 (slides/slide_5.png) is no longer on disk; left out of the notes".into())), "  ▲ warning  slide 5 (slides/slide_5.png) is no longer on disk; left out of the notes\n");
    }

    #[test]
    fn say_paints_its_marks_and_labels() {
        assert_eq!(shown(ANSI, &Event::NothingNew, &mut None), "  \u{1b}[36m◆\u{1b}[0m \u{1b}[1msnapshot\u{1b}[0m  nothing new since the last one\n");
        assert_eq!(shown(TRUE, &Event::NothingNew, &mut None), "  \u{1b}[38;2;93;184;192m◆\u{1b}[0m \u{1b}[1msnapshot\u{1b}[0m  nothing new since the last one\n");
        assert_eq!(shown(ANSI, &Event::SnapshotFailed("the model refused".into()), &mut None), "  \u{1b}[31m▲\u{1b}[0m \u{1b}[1msnapshot failed\u{1b}[0m  the model refused\n");
        assert_eq!(shown(TRUE, &Event::SnapshotFailed("the model refused".into()), &mut None), "  \u{1b}[38;2;242;118;107m▲\u{1b}[0m \u{1b}[1msnapshot failed\u{1b}[0m  the model refused\n");
    }

    #[test]
    fn dim_and_teal_run_through_the_block_and_busy_lines() {
        assert_eq!(shown(ANSI, &Event::Busy("polishing 40 words".into()), &mut None), "\u{1b}[2m  … polishing 40 words\u{1b}[0m\n");
        assert_eq!(shown(TRUE, &Event::Busy("polishing 40 words".into()), &mut None), "\u{1b}[2m  … polishing 40 words\u{1b}[0m\n");
        assert_eq!(shown(ANSI, &Event::Session(Notification::Stt(SttStatus::Connected)), &mut None), "\u{1b}[2m  transcribing\u{1b}[0m\n");
        let e = Event::Committed { words: 486, slides: 2, block: "<!-- 10:42:03 -->\n## Sampling".into(), usd: 0.02, confirmed: true, removed: 0, missing: 0, revision: 3 };
        assert_eq!(shown(TRUE, &e, &mut None), "  \u{1b}[38;2;93;184;192m│\u{1b}[0m \u{1b}[2m## Sampling\u{1b}[0m\n  \u{1b}[38;2;93;184;192m◆\u{1b}[0m \u{1b}[1mnotes\u{1b}[0m  486 words and 2 slides folded in  \u{1b}[2m$0.02\u{1b}[0m\n");
    }

    #[test]
    fn parse_line_pins_the_stdin_grammar() {
        assert_eq!(parse_line(""), Op::Snapshot(String::new()));
        assert_eq!(parse_line("polish"), Op::Polish);
        assert_eq!(parse_line("Polish"), Op::Polish);
        assert_eq!(parse_line("  POLISH  "), Op::Polish);
        assert_eq!(parse_line("polish the notes"), Op::Snapshot("polish the notes".into()));
        assert_eq!(parse_line("  a hint about variance  "), Op::Snapshot("a hint about variance".into()));
    }

    fn date() -> chrono::NaiveDate {
        chrono::NaiveDate::from_ymd_opt(2026, 9, 26).unwrap()
    }

    fn files() -> LectureFiles {
        LectureFiles::standard(Path::new("/tmp/lec"), date())
    }

    fn prepared(init: InitReport) -> start::Prepared {
        start::Prepared { launch: LaunchReport::default(), init, other_days: Vec::new() }
    }

    fn rendered(ready: &start::Prepared) -> String {
        let mut out = Vec::new();
        print_prepared(&mut out, OFF, ready, &files(), "Machine Learning", "Week 03 — Optimisation", "BlackHole 2ch");
        String::from_utf8(out).unwrap()
    }

    const HEADER: &str = "\n  Machine Learning  ›  Week 03 — Optimisation\n  listening on BlackHole 2ch\n  ⏎ snapshot   a hint ⏎   polish ⏎   ^C stop\n\n";

    #[test]
    fn print_prepared_for_a_created_folder() {
        assert_eq!(rendered(&prepared(InitReport { how: How::Created, ..Default::default() })), format!("  ◆ notes  lecture_notes_20260926.md created\n{HEADER}"));
    }

    #[test]
    fn print_prepared_for_a_migrated_or_rebuilt_folder() {
        assert_eq!(rendered(&prepared(InitReport { how: How::Migrated, ..Default::default() })), format!("  ◆ migrated  this folder's Python CLI state is now the app's (the Python CLI no longer writes here)\n{HEADER}"));
        assert_eq!(rendered(&prepared(InitReport { how: How::Rebuilt, ..Default::default() })), format!("  ▲ rebuilt  the state was rebuilt from the notes: everything after the last <!-- --> marker is pending\n{HEADER}"));
    }

    #[test]
    fn print_prepared_for_each_journal_recovery() {
        assert_eq!(rendered(&prepared(InitReport { journal: Recovered::Completed, ..Default::default() })), format!("  ✓ recovered  the last snapshot was fully written\n{HEADER}"));
        assert_eq!(rendered(&prepared(InitReport { journal: Recovered::Truncated, ..Default::default() })), format!("  ▲ recovered  removed a half-written snapshot; its material is queued again\n{HEADER}"));
        assert_eq!(rendered(&prepared(InitReport { journal: Recovered::NotAppended, ..Default::default() })), format!("  ▲ recovered  an interrupted snapshot never reached the notes; its material is queued again\n{HEADER}"));
    }

    #[test]
    fn print_prepared_for_kept_corrupt_state_legacy_commit_and_external_edit() {
        let init = InitReport { corrupt_kept: Some(PathBuf::from("/tmp/lec/.live_notes/lecture_notes_20260926.v2.json.corrupt-104152")), ..Default::default() };
        assert_eq!(rendered(&prepared(init)), format!("  ▲ rebuilt  the corrupt state is kept as /tmp/lec/.live_notes/lecture_notes_20260926.v2.json.corrupt-104152\n{HEADER}"));
        let init = InitReport { legacy_commit: Some("finished the Python CLI's interrupted snapshot"), ..Default::default() };
        assert_eq!(rendered(&prepared(init)), format!("  ▲ recovered  finished the Python CLI's interrupted snapshot\n{HEADER}"));
        let init = InitReport { external_edit: true, ..Default::default() };
        assert_eq!(rendered(&prepared(init)), format!("  ◆ notes  edited outside the app since the last session; kept as they are\n{HEADER}"));
    }

    #[test]
    fn print_prepared_for_other_days_and_pending_material() {
        let mut ready = prepared(InitReport::default());
        ready.other_days = vec![
            ("lecture_notes_20260920".into(), StopReport { unresolved: 2, ..Default::default() }),
            ("lecture_notes_20260919".into(), StopReport::default()),
        ];
        assert_eq!(rendered(&ready), format!("  ✓ recovered  lecture_notes_20260920's transcript gaps, 2 still waiting\n  ✓ recovered  lecture_notes_20260919's transcript gaps\n{HEADER}"));
        let init = InitReport { pending_segments: 12, pending_slides: 1, ..Default::default() };
        assert_eq!(rendered(&prepared(init)).lines().nth(2).unwrap(), "  listening on BlackHole 2ch, resumed with 12 lines and 1 slide for the next snapshot");
    }

    #[test]
    fn print_prepared_for_launch_repair_and_retention() {
        let mut ready = prepared(InitReport::default());
        ready.launch = LaunchReport {
            repaired: vec![(PathBuf::from("/tmp/lec/recordings/session_1.wav"), 19200)],
            missing: vec![PathBuf::from("/tmp/lec/recordings/session_2.wav")],
            pruned: vec![PathBuf::from("/tmp/lec/recordings/old.wav")],
            untranscribed: Vec::new(),
        };
        assert_eq!(rendered(&ready), format!("  ✓ repaired  /tmp/lec/recordings/session_1.wav (1.2 s)\n  ▲ missing  /tmp/lec/recordings/session_2.wav (marked as a gap)\n  ✓ deleted  /tmp/lec/recordings/old.wav (retention)\n{HEADER}"));
    }

    #[test]
    fn print_prepared_paints_the_header() {
        let mut out = Vec::new();
        print_prepared(&mut out, TRUE, &prepared(InitReport::default()), &files(), "Machine Learning", "Week 03 — Optimisation", "BlackHole 2ch");
        assert_eq!(String::from_utf8(out).unwrap(), "\n  \u{1b}[1mMachine Learning\u{1b}[0m  \u{1b}[2m›\u{1b}[0m  Week 03 — Optimisation\n\u{1b}[2m  listening on BlackHole 2ch\u{1b}[0m\n\u{1b}[2m  ⏎ snapshot   a hint ⏎   polish ⏎   ^C stop\u{1b}[0m\n\n");
    }

    fn ended(report: StopReport, spend: &Spend) -> String {
        let mut out = Vec::new();
        print_end(&mut out, OFF, &files(), &report, spend);
        String::from_utf8(out).unwrap()
    }

    fn ledger() -> Spend {
        Spend::open(&scratch("ledger").join("spend.jsonl"), "Machine Learning", "Week 03 — Optimisation", date()).unwrap()
    }

    #[test]
    fn print_end_for_a_clean_lecture() {
        // A lecture that spent nothing today sums an empty day, which `money` renders as $-0.00.
        assert_eq!(ended(StopReport::default(), &ledger()), "\n  ✓ saved  lecture_notes_20260926.md  lecture_transcript_20260926.txt\n    $-0.00 spent on this lecture today; `lecture spend` has the rest\n\n");
    }

    #[test]
    fn print_end_with_failures_and_spend() {
        let spend = ledger();
        spend.add(SpendKind::Notes, 0.02, true, None).unwrap();
        let report = StopReport { unresolved: 2, last_snapshot: Some("the model refused".into()), ..Default::default() };
        assert_eq!(ended(report, &spend), "\n  ✓ saved  lecture_notes_20260926.md  lecture_transcript_20260926.txt\n  ▲ notes  the last snapshot failed (the model refused); the next session in this folder adds what it missed\n    2 transcript gaps still to recover; the next session in this folder does it\n    $0.02 spent on this lecture today; `lecture spend` has the rest\n\n");
    }

    #[test]
    fn print_end_paints_the_saved_line() {
        let mut out = Vec::new();
        print_end(&mut out, TRUE, &files(), &StopReport::default(), &ledger());
        assert_eq!(String::from_utf8(out).unwrap(), "\n  \u{1b}[38;2;93;184;192m✓\u{1b}[0m \u{1b}[1msaved\u{1b}[0m  lecture_notes_20260926.md  lecture_transcript_20260926.txt\n\u{1b}[2m    $-0.00 spent on this lecture today; `lecture spend` has the rest\u{1b}[0m\n\n");
    }
}
