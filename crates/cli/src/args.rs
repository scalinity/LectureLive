//! The command line's shape (M7 plan §E): the clap definitions, moved verbatim from main.rs.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "lecturelive")]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) cmd: Cmd,
}

#[derive(Subcommand)]
pub(crate) enum Cmd {
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
pub(crate) struct LectureArgs {
    /// page: typeset the study page from the notes; spend: what the tool has cost; audit: whether the folder's
    /// audio is whole (exit 1 when anything is unexplained, 2 while something waits). Leave out to record.
    #[arg(value_parser = ["page", "spend", "audit"])]
    pub(crate) command: Option<String>,
    /// The lecture folder (default: the current directory)
    #[arg(long)]
    pub(crate) dir: Option<PathBuf>,
    /// Course name (default: the folder above Weeks/, else LECTURE_COURSE)
    #[arg(long)]
    pub(crate) course: Option<String>,
    /// Record BlackHole: Zoom through "LectureLive Loopback"
    #[arg(long, conflicts_with = "device")]
    pub(crate) loopback: bool,
    /// Record Zoom (through LectureLive Loopback) and this input together: its UID or part of its name
    #[arg(long, conflicts_with_all = ["loopback", "device"])]
    pub(crate) mixed: Option<String>,
    /// Audio input: its UID or part of its name (default: LECTURE_DEVICE)
    #[arg(long)]
    pub(crate) device: Option<String>,
    /// A term to bias recognition toward (repeatable; up to 100, each at most 50 characters)
    #[arg(long = "keyterm")]
    pub(crate) keyterms: Vec<String>,
    /// Notes file (default: lecture_notes_<today>.md)
    #[arg(long)]
    pub(crate) notes: Option<PathBuf>,
    /// Transcript file (default: lecture_transcript_<today>.txt)
    #[arg(long)]
    pub(crate) transcript: Option<PathBuf>,
    /// Where captured slides go (default: slides)
    #[arg(long = "slides-dir")]
    pub(crate) slides_dir: Option<PathBuf>,
    /// Stop after this many seconds
    #[arg(long)]
    pub(crate) secs: Option<u64>,
    /// Delete closed recordings older than this many days that have no unresolved gap
    #[arg(long)]
    pub(crate) keep_days: Option<u32>,
    /// Rebuild a corrupt sidecar from the notes, transcript and slides
    #[arg(long)]
    pub(crate) rebuild: bool,
    /// Show the lecture full-screen in the terminal (needs a terminal on stdin and stdout)
    #[arg(long, conflicts_with_all = ["plain", "command"])]
    pub(crate) tui: bool,
    /// Print the lecture line by line, as in a pipe
    #[arg(long, visible_alias = "no-tui", conflicts_with = "command")]
    pub(crate) plain: bool,
}

#[derive(Subcommand)]
pub(crate) enum Loopback {
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
pub(crate) enum Canary {
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
