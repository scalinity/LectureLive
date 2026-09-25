use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, RecvTimeoutError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use super::convert::{downmix, rms, Framer, Resampler16k};
use super::recorder::Recorder;

pub struct InputInfo {
    pub name: String,
    pub sample_rate: u32,
    pub channels: u16,
}

pub struct RecordReport {
    pub path: PathBuf,
    pub samples: u64,
    pub dropped_callbacks: u64,
}

pub fn list_inputs() -> Result<Vec<InputInfo>> {
    let host = cpal::default_host();
    let mut out = Vec::new();
    for d in host.input_devices()? {
        let cfg = d.default_input_config()?;
        out.push(InputInfo { name: d.description()?.name().to_string(), sample_rate: cfg.sample_rate(), channels: cfg.channels() });
    }
    Ok(out)
}

/// Records `device_name` for `duration` into `dir` as 16 kHz mono PCM16.
/// The callback copies into a bounded queue; M1 replaces this with a preallocated ring.
pub fn record_for(device_name: &str, duration: Duration, dir: &Path, mut on_level: impl FnMut(f32)) -> Result<RecordReport> {
    let host = cpal::default_host();
    let device = host
        .input_devices()?
        .find(|d| d.description().map(|n| n.name() == device_name).unwrap_or(false))
        .with_context(|| format!("input device {device_name:?} not found"))?;
    let config = device.default_input_config()?;
    let rate = config.sample_rate();
    let channels = config.channels() as usize;

    let (tx, rx) = sync_channel::<Vec<f32>>(64);
    let dropped = Arc::new(AtomicU64::new(0));
    let dropped_cb = dropped.clone();
    let err_fn = |e| eprintln!("[audio] stream error: {e}");
    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => device.build_input_stream(
            config.config(),
            move |data: &[f32], _| {
                if tx.try_send(data.to_vec()).is_err() {
                    dropped_cb.fetch_add(1, Ordering::Relaxed);
                }
            },
            err_fn,
            None,
        )?,
        cpal::SampleFormat::I16 => device.build_input_stream(
            config.config(),
            move |data: &[i16], _| {
                let v = data.iter().map(|&s| s as f32 / i16::MAX as f32).collect();
                if tx.try_send(v).is_err() {
                    dropped_cb.fetch_add(1, Ordering::Relaxed);
                }
            },
            err_fn,
            None,
        )?,
        other => return Err(anyhow!("unsupported sample format {other:?}")),
    };

    let stem = format!("session_{}", chrono::Local::now().format("%Y%m%d_%H%M%S"));
    let mut recorder = Recorder::create(dir, &stem)?;
    let mut resampler = Resampler16k::new(rate)?;
    let mut framer = Framer::default();
    let mut samples = 0u64;
    let mut level_acc: Vec<i16> = Vec::new();

    stream.play()?;
    let deadline = Instant::now() + duration;
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(block) => {
                for frame in framer.push(&resampler.push(&downmix(&block, channels))?) {
                    recorder.write(&frame)?;
                    samples += frame.len() as u64;
                    level_acc.extend_from_slice(&frame);
                    if level_acc.len() >= 16_000 {
                        on_level(rms(&level_acc));
                        level_acc.clear();
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    drop(stream);
    while let Ok(block) = rx.try_recv() {
        for frame in framer.push(&resampler.push(&downmix(&block, channels))?) {
            recorder.write(&frame)?;
            samples += frame.len() as u64;
        }
    }
    for frame in framer.push(&resampler.finish()?) {
        recorder.write(&frame)?;
        samples += frame.len() as u64;
    }
    let tail = framer.finish();
    recorder.write(&tail)?;
    samples += tail.len() as u64;
    let path = recorder.finalize()?;
    Ok(RecordReport { path, samples, dropped_callbacks: dropped.load(Ordering::Relaxed) })
}
