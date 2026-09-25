//! Recovery of transcript gaps from the recording through the REST endpoint (spec §5.4).
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio::time::Instant;
use uuid::Uuid;

use super::protocol::{is_refusal, refusal_message, to_samples, ServerWord};
use super::transcript::Word;
use crate::audio::frame::FRAME_SAMPLES;
use crate::audio::recorder::{HEADER_LEN, SAMPLE_RATE};

pub const REST_URL: &str = "https://api.x.ai/v1/stt";
/// The longest audio per request: the Python CLI's longest chunk.
const MAX_PIECE: usize = 30 * SAMPLE_RATE as usize;
/// Each piece ends at the quietest 100 ms of its last 10 s.
const CUT_WINDOW: usize = 10 * SAMPLE_RATE as usize;
const MAX_RETRY_UNITS: u32 = 30;

#[derive(Debug, Clone)]
pub struct RestConfig {
    pub url: String,
    pub api_key: String,
    pub keyterms: Vec<String>,
    /// Retry waits are 1, 2, 4 … 30 of these.
    pub retry_unit: Duration,
    pub request_timeout: Duration,
    /// How long a job waits for the recorder to write its interval.
    pub file_wait: Duration,
}

impl RestConfig {
    pub fn new(api_key: String, keyterms: Vec<String>) -> Self {
        Self { url: REST_URL.into(), api_key, keyterms, retry_unit: Duration::from_secs(1), request_timeout: Duration::from_secs(60), file_wait: Duration::from_secs(10) }
    }
}

/// The answer: word times are seconds from the clip's start; silence has no `words`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct RestTranscript {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub words: Vec<ServerWord>,
    #[serde(default)]
    pub duration: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RestError {
    Refused(String),
    Transient(String),
}

impl std::fmt::Display for RestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RestError::Refused(m) => write!(f, "refused: {m}"),
            RestError::Transient(m) => f.write_str(m),
        }
    }
}

pub struct RestClient {
    http: reqwest::Client,
    cfg: RestConfig,
}

impl RestClient {
    pub fn new(cfg: RestConfig) -> Result<Self> {
        // reqwest is built without a TLS provider of its own; rustls uses ring, as the websocket does.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let http = reqwest::Client::builder().timeout(cfg.request_timeout).build().context("build the HTTP client")?;
        Ok(Self { http, cfg })
    }

    pub async fn transcribe(&self, pcm: &[i16]) -> Result<RestTranscript, RestError> {
        let wav = wav_bytes(pcm).map_err(|e| RestError::Transient(format!("encode the WAV: {e}")))?;
        let file = reqwest::multipart::Part::bytes(wav).file_name("recovery.wav").mime_str("audio/wav").map_err(|e| RestError::Transient(e.to_string()))?;
        let mut form = reqwest::multipart::Form::new().part("file", file).text("language", "en").text("format", "true");
        for k in &self.cfg.keyterms {
            form = form.text("keyterm", k.trim().to_string());
        }
        let resp = self.http.post(&self.cfg.url).bearer_auth(&self.cfg.api_key).multipart(form).send().await.map_err(|e| RestError::Transient(e.to_string()))?;
        let status = resp.status();
        let body = resp.text().await.map_err(|e| RestError::Transient(e.to_string()))?;
        if status.is_success() {
            return serde_json::from_str(&body).map_err(|e| RestError::Transient(format!("unreadable answer: {e}")));
        }
        let msg = format!("{status}: {}", refusal_message(&body));
        Err(if is_refusal(status.as_u16()) { RestError::Refused(msg) } else { RestError::Transient(msg) })
    }
}

fn wav_bytes(pcm: &[i16]) -> hound::Result<Vec<u8>> {
    let spec = hound::WavSpec { channels: 1, sample_rate: SAMPLE_RATE, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut out = std::io::Cursor::new(Vec::with_capacity(44 + pcm.len() * 2));
    let mut w = hound::WavWriter::new(&mut out, spec)?;
    for &s in pcm {
        w.write_sample(s)?;
    }
    w.finalize()?;
    Ok(out.into_inner())
}

/// Splits an interval into requests of at most 30 s, each ending in the quietest 100 ms of its last 10 s.
pub fn pieces(pcm: &[i16]) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut start = 0;
    while pcm.len() - start > MAX_PIECE {
        let quietest = (start + MAX_PIECE - CUT_WINDOW..=start + MAX_PIECE - FRAME_SAMPLES)
            .step_by(FRAME_SAMPLES)
            .min_by_key(|&i| pcm[i..i + FRAME_SAMPLES].iter().map(|&s| (s as i64).pow(2)).sum::<i64>())
            .expect("the window holds whole frames");
        let cut = quietest + FRAME_SAMPLES / 2;
        out.push(start..cut);
        start = cut;
    }
    if start < pcm.len() {
        out.push(start..pcm.len());
    }
    out
}

#[derive(Debug, Clone, PartialEq)]
pub struct RecoverJob {
    pub recording_id: Uuid,
    /// The gap's start as the sidecar records it: names the gap.
    pub gap_start: u64,
    /// Where recovery begins: past the pieces an earlier run already logged.
    pub from: u64,
    pub end: u64,
    pub wav: PathBuf,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RecoverEvent {
    Piece { recording_id: Uuid, gap_start: u64, start_sample: u64, end_sample: u64, text: String, words: Vec<Word> },
    Done { recording_id: Uuid, gap_start: u64 },
    Failed { recording_id: Uuid, gap_start: u64, message: String, refused: bool },
}

pub struct RecoveryLink {
    pub jobs: mpsc::UnboundedSender<RecoverJob>,
    pub events: mpsc::Receiver<RecoverEvent>,
}

/// Starts the session's recovery worker: one job at a time, in order.
pub fn spawn_recovery(client: RestClient) -> RecoveryLink {
    let (jobs, mut rx) = mpsc::unbounded_channel::<RecoverJob>();
    let (tx, events) = mpsc::channel(64);
    tokio::spawn(async move {
        while let Some(job) = rx.recv().await {
            let (recording_id, gap_start) = (job.recording_id, job.gap_start);
            let ev = match recover(&client, &job, &tx, &rx).await {
                Ok(()) => RecoverEvent::Done { recording_id, gap_start },
                Err(RestError::Refused(message)) => {
                    // The key or a parameter is refused: nothing later can succeed this session.
                    let _ = tx.send(RecoverEvent::Failed { recording_id, gap_start, message, refused: true }).await;
                    return;
                }
                Err(RestError::Transient(message)) => RecoverEvent::Failed { recording_id, gap_start, message, refused: false },
            };
            if tx.send(ev).await.is_err() {
                return;
            }
        }
    });
    RecoveryLink { jobs, events }
}

async fn recover(client: &RestClient, job: &RecoverJob, tx: &mpsc::Sender<RecoverEvent>, jobs: &mpsc::UnboundedReceiver<RecoverJob>) -> Result<(), RestError> {
    let pcm = read_interval(&job.wav, job.from, job.end, client.cfg.file_wait).await.map_err(|e| RestError::Transient(format!("{e:#}")))?;
    for piece in pieces(&pcm) {
        let start = job.from + piece.start as u64;
        let mut failures = 0u32;
        let t = loop {
            match client.transcribe(&pcm[piece.clone()]).await {
                Ok(t) => break t,
                // Retried while the session runs; once it is stopping, the gap waits for a later session.
                Err(RestError::Transient(m)) if !jobs.is_closed() => {
                    let units = 1u32.checked_shl(failures).unwrap_or(u32::MAX).min(MAX_RETRY_UNITS);
                    failures = failures.saturating_add(1);
                    if !pause(client.cfg.retry_unit * units, jobs).await {
                        return Err(RestError::Transient(m));
                    }
                }
                Err(e) => return Err(e),
            }
        };
        let words = t.words.iter().map(|w| Word { text: w.text.clone(), start_sample: start + to_samples(w.start), end_sample: start + to_samples(w.end) }).collect();
        let ev = RecoverEvent::Piece { recording_id: job.recording_id, gap_start: job.gap_start, start_sample: start, end_sample: job.from + piece.end as u64, text: t.text.trim().to_string(), words };
        if tx.send(ev).await.is_err() {
            return Err(RestError::Transient("the session has ended".into()));
        }
    }
    Ok(())
}

/// Waits `d`, or less if the session starts stopping; false if it did.
async fn pause(d: Duration, jobs: &mpsc::UnboundedReceiver<RecoverJob>) -> bool {
    let until = Instant::now() + d;
    while Instant::now() < until {
        if jobs.is_closed() {
            return false;
        }
        tokio::time::sleep((until - Instant::now()).min(Duration::from_millis(100))).await;
    }
    !jobs.is_closed()
}

/// Samples [from, end) of a recording, waiting while the recorder has not yet written through `end`.
async fn read_interval(wav: &Path, from: u64, end: u64, wait: Duration) -> Result<Vec<i16>> {
    let give_up = Instant::now() + wait;
    loop {
        let path = wav.to_path_buf();
        if let Some(pcm) = tokio::task::spawn_blocking(move || read_pcm(&path, from, end)).await?? {
            return Ok(pcm);
        }
        anyhow::ensure!(Instant::now() < give_up, "{} holds no audio through sample {end} after {wait:?}", wav.display());
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn read_pcm(path: &Path, from: u64, end: u64) -> Result<Option<Vec<i16>>> {
    let mut f = File::open(path).with_context(|| format!("open {}", path.display()))?;
    if f.metadata()?.len() < HEADER_LEN + end * 2 {
        return Ok(None);
    }
    f.seek(SeekFrom::Start(HEADER_LEN + from * 2))?;
    let mut bytes = vec![0u8; ((end - from) * 2) as usize];
    f.read_exact(&mut bytes)?;
    Ok(Some(bytes.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_intervals_are_cut_in_their_quietest_places() {
        let mut pcm = vec![1000i16; 70 * 16_000];
        let silences = [(384_000, 392_000), (800_000, 808_000)]; // 24.0–24.5 s and 50.0–50.5 s
        for (from, to) in silences {
            pcm[from..to].fill(0);
        }
        let p = pieces(&pcm);
        assert_eq!(p.len(), 3, "{p:?}");
        assert_eq!((p[0].start, p[2].end), (0, pcm.len()));
        assert!(p.windows(2).all(|w| w[0].end == w[1].start));
        assert!(p.iter().all(|r| r.len() <= 30 * 16_000));
        for (cut, (from, to)) in [p[0].end, p[1].end].into_iter().zip(silences) {
            assert!((from..to).contains(&cut), "cut at {cut}, outside {from}..{to}");
        }
    }

    #[test]
    fn short_intervals_are_one_piece_and_empty_ones_none() {
        assert_eq!(pieces(&vec![5; 16_000]), vec![0..16_000]);
        assert_eq!(pieces(&vec![5; 30 * 16_000]), vec![0..480_000]);
        assert!(pieces(&[]).is_empty());
    }

    #[test]
    fn silence_answers_without_words() {
        let t: RestTranscript = serde_json::from_str(r#"{"text":"","language":"","duration":3.1}"#).unwrap();
        assert_eq!(t, RestTranscript { text: String::new(), words: vec![], duration: 3.1 });
    }

    #[test]
    fn a_wav_is_the_recorders_format() {
        let bytes = wav_bytes(&[1, -2, 3]).unwrap();
        assert_eq!(bytes.len(), 44 + 6);
        let r = hound::WavReader::new(std::io::Cursor::new(bytes)).unwrap();
        assert_eq!((r.spec().sample_rate, r.spec().channels, r.spec().bits_per_sample), (16_000, 1, 16));
    }
}
