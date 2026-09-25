use uuid::Uuid;

use super::convert::to_i16;
pub use super::convert::FRAME_SAMPLES;

/// 100 ms of 16 kHz mono PCM16 at a known position in a recording (spec §3.3).
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub recording_id: Uuid,
    pub sample_offset: u64,
    pub valid_samples: u32,
    pub pcm16: [i16; FRAME_SAMPLES],
}

impl Frame {
    pub fn pcm(&self) -> &[i16] {
        &self.pcm16[..self.valid_samples as usize]
    }
}

pub struct FrameBuilder {
    recording_id: Uuid,
    next_offset: u64,
    buf: Vec<i16>,
}

impl FrameBuilder {
    pub fn new(recording_id: Uuid) -> Self {
        Self { recording_id, next_offset: 0, buf: Vec::with_capacity(FRAME_SAMPLES * 2) }
    }

    pub fn push(&mut self, mono16k: &[f32]) -> Vec<Frame> {
        self.buf.extend(mono16k.iter().map(|&s| to_i16(s)));
        let mut out = Vec::new();
        while self.buf.len() >= FRAME_SAMPLES {
            let mut pcm16 = [0; FRAME_SAMPLES];
            pcm16.copy_from_slice(&self.buf[..FRAME_SAMPLES]);
            self.buf.drain(..FRAME_SAMPLES);
            out.push(self.frame(pcm16, FRAME_SAMPLES));
        }
        out
    }

    /// The last partial frame, zero-padded, with `valid_samples` saying how much is audio.
    pub fn finish(mut self) -> Option<Frame> {
        if self.buf.is_empty() {
            return None;
        }
        let mut pcm16 = [0; FRAME_SAMPLES];
        let n = self.buf.len();
        pcm16[..n].copy_from_slice(&self.buf);
        Some(self.frame(pcm16, n))
    }

    fn frame(&mut self, pcm16: [i16; FRAME_SAMPLES], valid: usize) -> Frame {
        let f = Frame { recording_id: self.recording_id, sample_offset: self.next_offset, valid_samples: valid as u32, pcm16 };
        self.next_offset += FRAME_SAMPLES as u64;
        f
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::convert::{downmix, Resampler16k};

    fn tone(rate: u32, channels: usize, secs: f64) -> Vec<f32> {
        let n = (rate as f64 * secs).round() as usize;
        (0..n)
            .flat_map(|i| std::iter::repeat((i as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin() * 0.5).take(channels))
            .collect()
    }

    fn frames_for(rate: u32, channels: usize, secs: f64) -> (Uuid, Vec<Frame>) {
        let id = Uuid::new_v4();
        let mut resampler = Resampler16k::new(rate).unwrap();
        let mut builder = FrameBuilder::new(id);
        let mut frames = Vec::new();
        for chunk in tone(rate, channels, secs).chunks(512 * channels) {
            frames.extend(builder.push(&resampler.push(&downmix(chunk, channels)).unwrap()));
        }
        frames.extend(builder.push(&resampler.finish().unwrap()));
        frames.extend(builder.finish());
        (id, frames)
    }

    #[test]
    fn common_rates_frame_contiguously_and_exactly() {
        for rate in [16_000, 44_100, 48_000] {
            for channels in [1, 2] {
                let (id, frames) = frames_for(rate, channels, 2.5);
                assert_eq!(frames.len(), 25, "{rate} Hz × {channels} ch");
                for (i, f) in frames.iter().enumerate() {
                    assert_eq!(f.recording_id, id);
                    assert_eq!(f.sample_offset, i as u64 * FRAME_SAMPLES as u64);
                    assert_eq!(f.valid_samples as usize, FRAME_SAMPLES);
                }
            }
        }
    }

    #[test]
    fn last_partial_frame_is_marked_and_zero_padded() {
        for rate in [16_000, 44_100, 48_000] {
            let (_, frames) = frames_for(rate, 1, 2.53); // 40_480 samples = 25 frames + 480
            let last = frames.last().unwrap();
            assert_eq!(frames.len(), 26, "{rate} Hz");
            assert_eq!(last.sample_offset, 40_000);
            assert_eq!(last.valid_samples, 480);
            assert!(last.pcm16[480..].iter().all(|&s| s == 0));
            assert_eq!(last.pcm().len(), 480);
        }
    }

    #[test]
    fn nothing_pushed_gives_no_final_frame() {
        assert!(FrameBuilder::new(Uuid::new_v4()).finish().is_none());
    }
}
