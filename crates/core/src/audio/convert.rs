use anyhow::Result;
use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Resampler};

pub const FRAME_SAMPLES: usize = 1600;
const TARGET_RATE: usize = 16_000;
const CHUNK: usize = 1024;

pub fn downmix(interleaved: &[f32], channels: usize) -> Vec<f32> {
    if channels == 1 {
        return interleaved.to_vec();
    }
    interleaved.chunks(channels).map(|c| c.iter().sum::<f32>() / channels as f32).collect()
}

pub fn to_i16(s: f32) -> i16 {
    (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16
}

/// Mono resampler to 16 kHz whose output sample k is input time k / 16000 s:
/// the resampler's delay is trimmed and `finish` flushes the tail.
pub struct Resampler16k {
    inner: Option<Fft<f32>>,
    pending: Vec<f32>,
    input_rate: u64,
    in_total: u64,
    out_total: u64,
    skip: usize,
}

impl Resampler16k {
    pub fn new(input_rate: u32) -> Result<Self> {
        let inner = if input_rate as usize == TARGET_RATE {
            None
        } else {
            Some(Fft::<f32>::new(input_rate as usize, TARGET_RATE, CHUNK, 1, FixedSync::Input)?)
        };
        let skip = inner.as_ref().map_or(0, |r| r.output_delay());
        Ok(Self { inner, pending: Vec::new(), input_rate: input_rate as u64, in_total: 0, out_total: 0, skip })
    }

    fn expected_output(&self) -> u64 {
        (self.in_total * TARGET_RATE as u64 + self.input_rate / 2) / self.input_rate
    }

    fn run(&mut self) -> Result<Vec<f32>> {
        let Some(r) = self.inner.as_mut() else {
            let out = std::mem::take(&mut self.pending);
            self.out_total += out.len() as u64;
            return Ok(out);
        };
        let mut out = Vec::new();
        while self.pending.len() >= r.input_frames_next() {
            let n = r.input_frames_next();
            let chunk: Vec<f32> = self.pending.drain(..n).collect();
            out.extend(r.process(&InterleavedSlice::new(&chunk, 1, n)?, None)?.take_data());
        }
        let skip = self.skip.min(out.len());
        self.skip -= skip;
        out.drain(..skip);
        self.out_total += out.len() as u64;
        Ok(out)
    }

    pub fn push(&mut self, mono: &[f32]) -> Result<Vec<f32>> {
        self.in_total += mono.len() as u64;
        self.pending.extend_from_slice(mono);
        self.run()
    }

    /// Drains the resampler so the total output equals the input duration at 16 kHz (spec §4.2).
    pub fn finish(mut self) -> Result<Vec<f32>> {
        let expected = self.expected_output();
        let mut out = Vec::new();
        while self.inner.is_some() && self.out_total < expected {
            let n = self.inner.as_ref().map_or(0, |r| r.input_frames_next());
            let len = self.pending.len().max(n);
            self.pending.resize(len, 0.0);
            out.extend(self.run()?);
        }
        let excess = self.out_total.saturating_sub(expected) as usize;
        out.truncate(out.len().saturating_sub(excess));
        Ok(out)
    }
}

#[derive(Default)]
pub struct Framer {
    buf: Vec<i16>,
}

impl Framer {
    pub fn push(&mut self, mono16k: &[f32]) -> Vec<Vec<i16>> {
        self.buf.extend(mono16k.iter().map(|&s| to_i16(s)));
        let mut frames = Vec::new();
        while self.buf.len() >= FRAME_SAMPLES {
            frames.push(self.buf.drain(..FRAME_SAMPLES).collect());
        }
        frames
    }

    pub fn finish(self) -> Vec<i16> {
        self.buf
    }
}

pub fn rms(pcm: &[i16]) -> f32 {
    if pcm.is_empty() {
        return 0.0;
    }
    let sum: f64 = pcm.iter().map(|&s| (s as f64 / i16::MAX as f64).powi(2)).sum();
    (sum / pcm.len() as f64).sqrt() as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downmix_averages_channels() {
        assert_eq!(downmix(&[1.0, 0.0, 0.5, 0.5], 2), vec![0.5, 0.5]);
        assert_eq!(downmix(&[0.25, 0.75], 1), vec![0.25, 0.75]);
    }

    #[test]
    fn framer_emits_exact_frames_and_keeps_remainder() {
        let mut f = Framer::default();
        let frames = f.push(&vec![0.5; 4000]);
        assert_eq!(frames.len(), 2);
        assert!(frames.iter().all(|fr| fr.len() == FRAME_SAMPLES));
        assert_eq!(f.finish().len(), 800);
    }

    #[test]
    fn framer_clamps_to_i16() {
        let mut f = Framer::default();
        let fr = f.push(&vec![2.0; FRAME_SAMPLES]);
        assert_eq!(fr[0][0], i16::MAX);
    }

    fn resampled_len(rate: u32) -> usize {
        let mut r = Resampler16k::new(rate).unwrap();
        let tone: Vec<f32> = (0..rate as usize * 2).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
        let mut out = 0;
        for chunk in tone.chunks(480) {
            out += r.push(chunk).unwrap().len();
        }
        out
    }

    #[test]
    fn resamples_common_rates_to_16k() {
        for rate in [16_000, 44_100, 48_000] {
            let n = resampled_len(rate);
            assert!((30_000..=32_000).contains(&n), "{rate} Hz -> {n} samples for 2 s");
        }
    }

    fn impulse_peak(rate: u32) -> usize {
        let mut r = Resampler16k::new(rate).unwrap();
        let mut input = vec![0.0f32; rate as usize * 2];
        input[rate as usize] = 1.0; // exactly 1.000 s
        let mut out = Vec::new();
        for chunk in input.chunks(480) {
            out.extend(r.push(chunk).unwrap());
        }
        out.extend(r.finish().unwrap());
        out.iter().enumerate().max_by(|a, b| a.1.abs().total_cmp(&b.1.abs())).unwrap().0
    }

    #[test]
    fn resampled_time_matches_input_time() {
        for rate in [16_000, 44_100, 48_000] {
            let peak = impulse_peak(rate) as i64;
            assert!((peak - 16_000).abs() <= 2, "{rate} Hz: impulse at 1 s came out at sample {peak}");
        }
    }

    #[test]
    fn finish_makes_output_length_equal_input_duration() {
        for (rate, n) in [(16_000u32, 40_480usize), (44_100, 111_573), (48_000, 121_440)] {
            let mut r = Resampler16k::new(rate).unwrap();
            let mut total = 0;
            for chunk in vec![0.1f32; n].chunks(333) {
                total += r.push(chunk).unwrap().len();
            }
            total += r.finish().unwrap().len();
            let expected = (n as u64 * 16_000 + rate as u64 / 2) / rate as u64;
            assert_eq!(total as u64, expected, "{rate} Hz, {n} input samples");
        }
    }

    #[test]
    fn rms_of_silence_and_full_scale() {
        assert_eq!(rms(&[0; 1600]), 0.0);
        assert!((rms(&[i16::MAX; 1600]) - 1.0).abs() < 0.001);
    }
}
