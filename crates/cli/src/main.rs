use std::path::PathBuf;
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
use lecturelive_core::session::lock::FolderLock;
use lecturelive_core::session::segments::SegmentSource;
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
    #[command(subcommand)]
    Canary(Canary),
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
                    SttStatus::Refused(m) => eprintln!("transcription refused: {m}. Recording continues without it."),
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
