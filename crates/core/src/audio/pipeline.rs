use std::ops::Range;

use anyhow::Result;
use uuid::Uuid;

use super::convert::{downmix, Resampler16k};
use super::frame::{Frame, FrameBuilder};

const SILENCE_BLOCK: u64 = 4096;

/// One source's conversion chain: downmix → 16 kHz → 100 ms frames (spec §3.4, §4.2).
pub struct Pipeline {
    channels: usize,
    input_rate: u64,
    in_frames: u64,
    resampler: Resampler16k,
    frames: FrameBuilder,
}

impl Pipeline {
    pub fn new(recording_id: Uuid, input_rate: u32, channels: u16) -> Result<Self> {
        Ok(Self {
            channels: channels.max(1) as usize,
            input_rate: input_rate as u64,
            in_frames: 0,
            resampler: Resampler16k::new(input_rate)?,
            frames: FrameBuilder::new(recording_id),
        })
    }

    pub fn push(&mut self, interleaved: &[f32]) -> Result<Vec<Frame>> {
        self.in_frames += (interleaved.len() / self.channels) as u64;
        Ok(self.frames.push(&self.resampler.push(&downmix(interleaved, self.channels))?))
    }

    /// Stands in silence for lost input, so later audio keeps its true position.
    /// Returns the lost interval in 16 kHz samples of this recording.
    pub fn push_silence(&mut self, input_frames: u64) -> Result<(Range<u64>, Vec<Frame>)> {
        let lost = self.to_output(self.in_frames)..self.to_output(self.in_frames + input_frames);
        let mut frames = Vec::new();
        let mut left = input_frames;
        while left > 0 {
            let n = left.min(SILENCE_BLOCK);
            self.in_frames += n;
            frames.extend(self.frames.push(&self.resampler.push(&vec![0.0; n as usize])?));
            left -= n;
        }
        Ok((lost, frames))
    }

    fn to_output(&self, input_frames: u64) -> u64 {
        (input_frames * 16_000 + self.input_rate / 2) / self.input_rate
    }

    pub fn finish(self) -> Result<Vec<Frame>> {
        let Pipeline { resampler, mut frames, .. } = self;
        let mut out = frames.push(&resampler.finish()?);
        out.extend(frames.finish());
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_is_placed_at_its_16k_position() {
        let mut p = Pipeline::new(Uuid::new_v4(), 48_000, 2).unwrap();
        let mut frames = p.push(&vec![0.5; 48_000 * 2]).unwrap(); // 1 s stereo
        let (lost, more) = p.push_silence(24_000).unwrap(); // 0.5 s lost
        assert_eq!(lost, 16_000..24_000);
        frames.extend(more);
        frames.extend(p.push(&vec![0.5; 48_000 * 2]).unwrap());
        frames.extend(p.finish().unwrap());
        let total: u64 = frames.iter().map(|f| f.valid_samples as u64).sum();
        assert_eq!(total, 40_000); // 2.5 s
        let at = |s: u64| {
            let f = frames.iter().find(|f| f.sample_offset <= s && s < f.sample_offset + 1600).unwrap();
            f.pcm16[(s - f.sample_offset) as usize]
        };
        assert_eq!(at(20_000), 0);
        assert!(at(8_000) > 10_000);
        assert!(at(32_000) > 10_000);
    }
}
