use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use lecturelive_core::audio::level::{self, SilenceWatch};
use lecturelive_core::audio::permission::{self, MicPermission};
use lecturelive_core::audio::source::DeviceSource;
use lecturelive_core::audio::{input, loopback, recorder, routing};
use lecturelive_core::capture::window;
use lecturelive_core::session::coordinator::{self, Notification, SessionConfig, SttStatus};
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
    /// page: typeset the study page from the notes; spend: what the tool has cost. Leave out to record.
    #[arg(value_parser = ["page", "spend"])]
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
        Cmd::Record { loopback, device, dir, secs, keep_days, stt, keyterms } => record(loopback, device, dir, secs, keep_days, stt, keyterms).await?,
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

async fn record(use_loopback: bool, device: Option<String>, dir: Option<PathBuf>, secs_limit: Option<u64>, keep_days: Option<u32>, stt: bool, keyterms: Vec<String>) -> Result<()> {
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
    let uid = if use_loopback {
        let s = loopback::status()?;
        anyhow::ensure!(s.blackhole_present, "BlackHole 2ch is not installed (brew install blackhole-2ch)");
        if !s.present {
            println!("warning: {} is not set up (`lecturelive loopback setup`); recording BlackHole anyway", loopback::LOOPBACK_NAME);
        }
        loopback::BLACKHOLE_UID.to_string()
    } else {
        device.context("give --loopback or --device <UID> (`lecturelive inputs` lists them)")?
    };

    let (handle, mut notes) = coordinator::spawn(SessionConfig { dir, stem, stt: stt_link, recovery, ..Default::default() }, Box::new(DeviceSource { uid }));
    let mut watch = use_loopback.then(|| SilenceWatch::new(-60.0, 10));
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
                    "input {uid} disappeared at {}; waiting for it to return (no other input is used). Ctrl-C stops.",
                    chrono::Local::now().format("%H:%M:%S")
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
fn say(p: spend::Paint, kind: &str, label: &str, detail: &str) {
    let (mark, colour) = match kind {
        "slide" => ("▣", "teal"),
        "notes" => ("◆", "teal"),
        "page" => ("✦", "teal"),
        "done" => ("✓", "teal"),
        _ => ("▲", "red"),
    };
    println!("{}", format!("  {} {}  {detail}", p.paint(mark, &[colour]), p.paint(label, &["bold"])).trim_end());
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

/// Prints the lecture's events in the CLI's lines; the loopback silence warning as `record` gives it.
fn show(p: spend::Paint, e: &Event, watch: &mut Option<SilenceWatch>) {
    match e {
        Event::Session(n) => match n {
            Notification::Segment(s) => {
                let tag = if s.source == SegmentSource::Recovered { p.paint("  (recovered)", &["dim"]) } else { String::new() };
                println!("  {}  {}{tag}", p.paint(&s.said_at.format("%H:%M:%S").to_string(), &["dim"]), s.text);
            }
            Notification::Level(l) => {
                if watch.as_mut().is_some_and(|w| w.observe(*l)) {
                    say(p, "warn", "no signal", &format!("10 s of silence on BlackHole: is Zoom's Speaker \"{}\"?", loopback::LOOPBACK_NAME));
                }
            }
            Notification::Recording { path } => println!("{}", p.paint(&format!("  recording to {}", path.display()), &["dim"])),
            Notification::Gap(g) => say(p, "warn", "gap", &format!("{:?} from sample {} to {:?} of {}", g.kind, g.start_sample, g.end_sample, g.recording_id)),
            Notification::DeviceGone { uid } => say(p, "warn", "input gone", &format!("{uid}; waiting for it to return (no other input is used). Ctrl-C stops.")),
            Notification::DeviceBack { uid } => say(p, "done", "input back", &format!("{uid}; recording continues in a new file")),
            Notification::Failed(m) => say(p, "warn", "session failed", m),
            Notification::Stt(s) => match s {
                SttStatus::Connected => println!("{}", p.paint("  transcribing", &["dim"])),
                SttStatus::Retrying { after, reason } => say(p, "warn", "transcription interrupted", &format!("{reason}; reconnecting in {} s", after.as_secs())),
                SttStatus::Refused(m) => say(p, "warn", "transcription refused", &format!("{}. Recording continues without it.", sentence(m))),
                SttStatus::ServerError(m) => say(p, "warn", "transcription server", m),
                SttStatus::Stopped(m) => say(p, "warn", "transcription stopped", m),
            },
            Notification::Open { .. } | Notification::SourceEnded => {}
            Notification::Recovered(g) => say(p, "done", "recovered", &format!("the transcript of {:.1}–{:.1} s of recording {}", secs(g.start_sample), g.end_sample.map_or(0.0, secs), g.recording_id)),
            Notification::RecoveryFailed(m) => say(p, "warn", "recovery", m),
            Notification::SpendFailed(m) => say(p, "warn", "spend", m),
        },
        Event::Busy(m) => println!("{}", p.paint(&format!("  … {m}"), &["dim"])),
        Event::Preview(_) => {} // the committed block is printed instead, as the CLI does
        Event::NothingNew => say(p, "notes", "snapshot", "nothing new since the last one"),
        Event::Committed { words, slides, block, usd, confirmed, missing, .. } => {
            for line in block.trim().lines().filter(|l| !l.starts_with("<!-- ")) {
                println!("  {} {}", p.paint("│", &["teal"]), p.paint(line, &["dim"]));
            }
            let mut detail = format!("{} and {} folded in  {}", plural(*words, "word"), plural(*slides, "slide"), p.paint(&spend::money(*usd), &["dim"]));
            if *missing > 0 {
                detail += &format!("  ({} not placed by the model, listed at the end)", plural(*missing, "slide"));
            }
            if !confirmed {
                detail += "  (transcription still catching up; the rest goes into the next snapshot)";
            }
            say(p, "notes", "notes", &detail);
        }
        Event::SnapshotFailed(m) => say(p, "warn", "snapshot failed", m),
        Event::Polished { backup, usd, .. } => {
            let name = backup.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            say(p, "done", "polished", &format!("previous version in .live_notes/{name}  {}", p.paint(&spend::money(*usd), &["dim"])));
        }
        Event::PolishStopped(m) => say(p, "warn", "polish stopped", m),
        Event::PolishFailed(m) => say(p, "warn", "polish failed", m),
        Event::Cancelled(what) => say(p, "warn", "cancelled", &format!("{what}; nothing was written, everything is kept for the next snapshot")),
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
            say(p, "done", "page", &detail);
        }
        Event::PageFailed(m) => say(p, "warn", "page failed", m),
        Event::Slide { index, file, auto, uncertain, .. } => {
            let how = match (auto, uncertain) {
                (true, true) => " (auto, still changing)",
                (true, false) => " (auto)",
                _ => "",
            };
            say(p, "slide", &format!("slide {index}{how}"), &format!("{}, into the next snapshot", Path::new(file).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()))
        }
        Event::Warning(m) => say(p, "warn", "warning", m),
    }
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
    let dir = match a.dir {
        Some(d) => d,
        None => std::env::current_dir()?,
    };
    let key = env_value("GROK_API_KEY").context("GROK_API_KEY is not set (the environment, a .env here or above, or the repository's .env)")?;
    let course = a.course.or_else(|| course_from_path(&dir)).or_else(|| env_value("LECTURE_COURSE")).unwrap_or_else(|| "Lecture".into());
    let recording = a.command.is_none();
    // Checked before any file is created, so a missing device leaves the folder untouched.
    let input = if recording { Some(resolve_input(a.loopback, a.device.or_else(|| env_value("LECTURE_DEVICE")))?) } else { None };
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
        say(p, "page", "page", &format!("distilling {} with {}, a few minutes", files.notes.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), chat::MODEL));
        let printer = tokio::spawn(async move {
            let mut none = None;
            while let Some(e) = ev_rx.recv().await {
                show(p, &e, &mut none);
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
    let mut announce = |stem: &str| say(p, "notes", "recovering", &format!("{stem}'s transcript gaps, before today's session"));
    let ready = start::prepare(&files, &title, a.rebuild, a.keep_days.map_or(Retention::KeepAll, Retention::KeepDays), &recovery, Some(spend.clone()), &mut announce).await?;
    for (path, n) in &ready.launch.repaired {
        say(p, "done", "repaired", &format!("{} ({:.1} s)", path.display(), secs(*n)));
    }
    for path in &ready.launch.missing {
        say(p, "warn", "missing", &format!("{} (marked as a gap)", path.display()));
    }
    for path in &ready.launch.pruned {
        say(p, "done", "deleted", &format!("{} (retention)", path.display()));
    }
    let init = ready.init;
    match init.how {
        How::Created => say(p, "notes", "notes", &format!("{} created", files.notes.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())),
        How::Migrated => say(p, "notes", "migrated", "this folder's Python CLI state is now the app's (the Python CLI no longer writes here)"),
        How::Rebuilt => say(p, "warn", "rebuilt", "the state was rebuilt from the notes: everything after the last <!-- --> marker is pending"),
        How::Resumed => {}
    }
    if let Some(m) = init.legacy_commit {
        say(p, "warn", "recovered", m);
    }
    if let Some(kept) = &init.corrupt_kept {
        say(p, "warn", "rebuilt", &format!("the corrupt state is kept as {}", kept.display()));
    }
    match init.journal {
        Recovered::Completed => say(p, "done", "recovered", "the last snapshot was fully written"),
        Recovered::Truncated => say(p, "warn", "recovered", "removed a half-written snapshot; its material is queued again"),
        Recovered::NotAppended => say(p, "warn", "recovered", "an interrupted snapshot never reached the notes; its material is queued again"),
        Recovered::Nothing => {}
    }
    if init.external_edit {
        say(p, "notes", "notes", "edited outside the app since the last session; kept as they are");
    }
    for (stem, r) in &ready.other_days {
        say(p, "done", "recovered", &format!("{stem}'s transcript gaps{}", if r.unresolved > 0 { format!(", {} still waiting", r.unresolved) } else { String::new() }));
    }

    println!();
    println!("  {}  {}  {name}", p.paint(&course, &["bold"]), p.paint("›", &["dim"]));
    let waiting = if init.pending_segments > 0 || init.pending_slides > 0 {
        format!(", resumed with {} and {} for the next snapshot", plural(init.pending_segments as usize, "line"), plural(init.pending_slides, "slide"))
    } else {
        String::new()
    };
    println!("{}", p.paint(&format!("  listening on {input_name}{waiting}"), &["dim"]));
    println!("{}", p.paint("  ⏎ snapshot   a hint ⏎   polish ⏎   ^C stop", &["dim"]));
    println!();

    let session = SessionConfig { dir: dir.clone(), stem: files.stem.clone(), stt: Some(stt_link), recovery: Some(recovery()?), spend: Some(spend.clone()), transcript: Some(files.transcript.clone()) };
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    let stdin_tx = cmd_tx.clone();
    std::thread::spawn(move || {
        for line in std::io::stdin().lines().map_while(Result::ok) {
            let text = line.trim().to_string();
            let op = if text.eq_ignore_ascii_case("polish") { Op::Polish } else { Op::Snapshot(text) };
            if stdin_tx.send(LectureCommand::Op(op)).is_err() {
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
                1 => say(p, "notes", "stopping", "finishing the transcript and recovery, then a last snapshot (Ctrl-C again stops waiting for recovery)"),
                2 => say(p, "warn", "stopping", "no longer waiting for recovery or queued requests; its gaps wait for the next session (Ctrl-C again quits at once)"),
                _ => {
                    // The recording is durable to its last second and gaps and journals are on disk: the next
                    // session in this folder repairs, recovers and notes what is left.
                    say(p, "warn", "quit", "stopped at once; the next session in this folder picks up what was left");
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
    let mut watch = (uid == loopback::BLACKHOLE_UID).then(|| SilenceWatch::new(-60.0, 10));
    let printer = tokio::spawn(async move {
        while let Some(e) = ev_rx.recv().await {
            show(p, &e, &mut watch);
        }
    });
    let watch_slides = SlideWatch { screenshots: lecture::screenshot_dir(), poll: Duration::from_secs(1) };
    let result = lecture::run(lec, session, Box::new(DeviceSource { uid }), watch_slides, cmd_rx, ev_tx).await;
    printer.await?;
    let report = result?;
    let file_name = |f: &Path| f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    println!();
    println!("  {} {}  {}  {}", p.paint("✓", &["teal"]), p.paint("saved", &["bold"]), file_name(&files.notes), file_name(&files.transcript));
    if report.unresolved > 0 {
        println!("{}", p.paint(&format!("    {} still to recover; the next session in this folder does it", plural(report.unresolved, "transcript gap")), &["dim"]));
    }
    println!("{}", p.paint(&format!("    {} spent on this lecture today; `lecture spend` has the rest", spend::money(spend.lecture_total())), &["dim"]));
    println!();
    Ok(())
}
