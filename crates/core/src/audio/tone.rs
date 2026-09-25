use std::time::Duration;

use anyhow::{ensure, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

/// Plays a 440 Hz tone on one output device by UID, without touching the system output.
/// Checks use it in place of Zoom: into "LectureLive Loopback" it takes Zoom's path, and into
/// BlackHole's output it feeds BlackHole silently.
pub fn play_tone(output_uid: &str, secs: f32, amplitude: f32) -> Result<()> {
    let device = cpal::default_host()
        .output_devices()?
        .find(|d| d.id().map(|i| i.id() == output_uid).unwrap_or(false))
        .with_context(|| format!("output {output_uid} not found"))?;
    let config = device.default_output_config()?;
    ensure!(config.sample_format() == cpal::SampleFormat::F32, "output {output_uid} is not f32");
    let rate = config.sample_rate() as f32;
    let channels = config.channels() as usize;
    let mut n = 0u64;
    let stream = device.build_output_stream(
        config.config(),
        move |out: &mut [f32], _: &cpal::OutputCallbackInfo| {
            for frame in out.chunks_mut(channels) {
                frame.fill((n as f32 * 440.0 * std::f32::consts::TAU / rate).sin() * amplitude);
                n += 1;
            }
        },
        |e| eprintln!("[tone] {e}"),
        None,
    )?;
    stream.play()?;
    std::thread::sleep(Duration::from_secs_f32(secs));
    Ok(())
}
