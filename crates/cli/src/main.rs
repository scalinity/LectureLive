use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use lecturelive_core::audio::{input, recorder, routing};
use lecturelive_core::capture::window;
use lecturelive_core::stt::probe;

#[derive(Parser)]
#[command(name = "lecturelive")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    #[command(subcommand)]
    Canary(Canary),
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
    },
}

fn route_state_path() -> Result<PathBuf> {
    let dir = dirs::data_dir().context("no Application Support dir")?.join("LectureLive");
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join("route.json"))
}

#[tokio::main]
async fn main() -> Result<()> {
    let Cmd::Canary(c) = Cli::parse().cmd;
    match c {
        Canary::Route { action } => {
            let p = route_state_path()?;
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
        Canary::SttProbe { wav, finalize_after_secs, finalize_message, log } => {
            dotenvy::dotenv().ok();
            let api_key = std::env::var("GROK_API_KEY").context("GROK_API_KEY not set")?;
            let mut reader = hound::WavReader::open(&wav)?;
            let spec = reader.spec();
            anyhow::ensure!(spec.sample_rate == 16_000 && spec.channels == 1 && spec.bits_per_sample == 16, "need 16 kHz mono PCM16");
            let pcm: Vec<i16> = reader.samples::<i16>().collect::<Result<_, _>>()?;
            let s = probe::probe(probe::ProbeOptions {
                url: "wss://api.x.ai/v1/stt?model=grok-voice-transcribe-2.0&encoding=pcm&sample_rate=16000&interim_results=true&language=en".into(),
                api_key,
                pcm,
                pace: Duration::from_millis(100),
                finalize_after_frames: finalize_after_secs.map(|s| (s * 10.0) as usize),
                finalize_message,
                log_path: log,
            })
            .await?;
            println!("{s:#?}");
        }
    }
    Ok(())
}
