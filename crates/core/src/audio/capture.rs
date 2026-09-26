//! The only code that runs in the audio callback: a copy into a preallocated ring, or a
//! count of what did not fit (spec §3.4). Everything else happens on the source thread.
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use rtrb::{Consumer, Producer, RingBuffer};

pub const RING_SECONDS: u32 = 4;
const DROP_EVENTS: usize = 256;

/// `frames` input frames that did not fit, starting at input frame `at_frame`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dropped {
    pub at_frame: u64,
    pub frames: u64,
}

#[derive(Default)]
pub struct StreamFlags {
    pub gone: AtomicBool,
    pub invalidated: AtomicBool,
    pub errors: AtomicU64,
    first_ms: AtomicU64,
    ended: AtomicBool,
    end_frame: AtomicU64,
}

impl StreamFlags {
    /// Wall-clock milliseconds of the first captured sample, once audio has arrived.
    pub fn anchor_ms(&self) -> Option<u64> {
        match self.first_ms.load(Ordering::Acquire) {
            0 => None,
            ms => Some(ms),
        }
    }

    /// cpal reports disconnection and sample-rate change through its own property
    /// listeners (`kAudioDevicePropertyDeviceIsAlive`, `kAudioDevicePropertyNominalSampleRate`).
    pub fn on_error(&self, kind: cpal::ErrorKind) {
        match kind {
            cpal::ErrorKind::DeviceNotAvailable => self.gone.store(true, Ordering::Release),
            cpal::ErrorKind::StreamInvalidated => self.invalidated.store(true, Ordering::Release),
            _ => {
                self.errors.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

pub struct CaptureProducer {
    data: Producer<f32>,
    drops: Producer<Dropped>,
    channels: u64,
    rate: u64,
    pos: u64,
    pending: Option<Dropped>,
    flags: Arc<StreamFlags>,
}

impl CaptureProducer {
    /// Called from the audio callback: copies or counts; never blocks or allocates.
    /// A drop is published before any later audio, so the consumer can place it exactly.
    pub fn push(&mut self, interleaved: &[f32]) {
        let frames = interleaved.len() as u64 / self.channels;
        if self.pos == 0 && self.flags.first_ms.load(Ordering::Relaxed) == 0 {
            let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
            let first = now.saturating_sub(frames * 1000 / self.rate).max(1);
            self.flags.first_ms.store(first, Ordering::Release);
        }
        if self.pending.is_none() && self.data.push_entire_slice(interleaved).is_ok() {
            self.pos += frames;
            return;
        }
        match &mut self.pending {
            Some(d) => d.frames += frames,
            None => self.pending = Some(Dropped { at_frame: self.pos, frames }),
        }
        self.pos += frames;
        if let Some(d) = self.pending {
            if self.drops.push(d).is_ok() {
                self.pending = None;
            }
        }
    }
}

impl Drop for CaptureProducer {
    /// A drop still pending (its event ring was full) is published through the end position, so the
    /// consumer places it: a stream's last stretch is never lost without a mark.
    fn drop(&mut self) {
        self.flags.end_frame.store(self.pos, Ordering::Relaxed);
        self.flags.ended.store(true, Ordering::Release);
    }
}

pub enum Chunk<'a> {
    Audio(&'a [f32]),
    /// Input frames lost to overflow, to be replaced by silence.
    Silence(u64),
}

pub struct CaptureConsumer {
    data: Consumer<f32>,
    drops: Consumer<Dropped>,
    channels: usize,
    pos: u64,
    scratch: Vec<f32>,
    flags: Arc<StreamFlags>,
}

impl CaptureConsumer {
    /// Input frames waiting to be drained.
    pub fn available(&self) -> u64 {
        (self.data.slots() / self.channels) as u64
    }

    /// Delivers everything captured so far, in input order, with drops in place.
    pub fn drain(&mut self, mut f: impl FnMut(Chunk<'_>) -> Result<()>) -> Result<()> {
        loop {
            // Whether the stream has ended, read first: every push came before the end was marked, so
            // once it is seen the audio length below counts all of it.
            let ended = self.flags.ended.load(Ordering::Acquire);
            // Read the audio length before looking for drops: a drop published after this
            // read lies beyond the audio counted here, because it was published before any
            // audio that follows it.
            let available = (self.data.slots() / self.channels) as u64;
            let limit = match self.drops.peek() {
                Ok(d) if d.at_frame == self.pos => {
                    let mut d = *d;
                    let _ = self.drops.pop();
                    // Back-to-back overflows arrive as adjacent events: report one stretch.
                    while let Ok(next) = self.drops.peek() {
                        if next.at_frame != d.at_frame + d.frames {
                            break;
                        }
                        d.frames += next.frames;
                        let _ = self.drops.pop();
                    }
                    f(Chunk::Silence(d.frames))?;
                    self.pos += d.frames;
                    continue;
                }
                Ok(d) => available.min(d.at_frame - self.pos),
                Err(_) => available,
            };
            if limit == 0 {
                // The stream has ended and everything published is delivered: what is left of its
                // count is the drop it could not publish.
                let end = self.flags.end_frame.load(Ordering::Relaxed);
                if ended && end > self.pos && self.drops.is_empty() {
                    f(Chunk::Silence(end - self.pos))?;
                    self.pos = end;
                }
                return Ok(());
            }
            let chunk = self.data.read_chunk(limit as usize * self.channels)?;
            let (a, b) = chunk.as_slices();
            self.scratch.clear();
            self.scratch.extend_from_slice(a);
            self.scratch.extend_from_slice(b);
            chunk.commit_all();
            f(Chunk::Audio(&self.scratch))?;
            self.pos += limit;
        }
    }
}

pub fn ring(channels: u16, rate: u32, seconds: u32) -> (CaptureProducer, CaptureConsumer, Arc<StreamFlags>) {
    ring_with_capacity(channels, rate, (rate * seconds) as usize)
}

fn ring_with_capacity(channels: u16, rate: u32, frames: usize) -> (CaptureProducer, CaptureConsumer, Arc<StreamFlags>) {
    let channels = channels.max(1);
    let (data_p, data_c) = RingBuffer::new(frames * channels as usize);
    let (drops_p, drops_c) = RingBuffer::new(DROP_EVENTS);
    let flags = Arc::new(StreamFlags::default());
    let producer = CaptureProducer {
        data: data_p,
        drops: drops_p,
        channels: channels as u64,
        rate: rate as u64,
        pos: 0,
        pending: None,
        flags: flags.clone(),
    };
    let consumer = CaptureConsumer { data: data_c, drops: drops_c, channels: channels as usize, pos: 0, scratch: Vec::with_capacity(frames * channels as usize), flags: flags.clone() };
    (producer, consumer, flags)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(c: &mut CaptureConsumer) -> Vec<String> {
        let mut seen = Vec::new();
        c.drain(|chunk| {
            seen.push(match chunk {
                Chunk::Audio(a) => format!("audio {}x{}", a.len(), a.first().copied().unwrap_or(0.0)),
                Chunk::Silence(n) => format!("silence {n}"),
            });
            Ok(())
        })
        .unwrap();
        seen
    }

    fn small_ring(channels: u16, frames: usize) -> (CaptureProducer, CaptureConsumer, Arc<StreamFlags>) {
        ring_with_capacity(channels, 16_000, frames)
    }

    #[test]
    fn audio_passes_through_in_order() {
        let (mut p, mut c, flags) = small_ring(1, 64);
        p.push(&[1.0; 10]);
        p.push(&[2.0; 5]);
        assert_eq!(collect(&mut c), ["audio 15x1"]);
        assert!(flags.anchor_ms().is_some());
    }

    #[test]
    fn overflow_becomes_silence_at_the_exact_position() {
        let (mut p, mut c, _) = small_ring(1, 8);
        p.push(&[1.0; 6]);
        p.push(&[2.0; 6]); // does not fit: dropped at frame 6
        p.push(&[2.5; 6]); // still no room: the same drop grows to 12 frames
        assert_eq!(collect(&mut c), ["audio 6x1", "silence 12"]);
        p.push(&[3.0; 4]);
        assert_eq!(collect(&mut c), ["audio 4x3"]);
    }

    #[test]
    fn stereo_positions_count_frames_not_samples() {
        let (mut p, mut c, _) = small_ring(2, 4);
        p.push(&[1.0; 8]); // 4 frames
        p.push(&[2.0; 4]); // 2 frames dropped
        assert_eq!(collect(&mut c), ["audio 8x1", "silence 2"]);
    }

    /// M5 open thread: a drop still pending when the stream ends (its event ring was full) reaches the
    /// consumer as silence at its exact place, so the recording's last stretch is never lost unmarked.
    #[test]
    fn a_drop_still_pending_when_the_stream_ends_reaches_the_consumer() {
        let (mut p, mut c, _) = small_ring(1, 8);
        p.push(&[1.0; 8]); // the ring is full
        for _ in 0..DROP_EVENTS + 1 {
            p.push(&[2.0; 1]); // one event per overflow; the last finds the event ring full
        }
        drop(p); // the stream ends with that drop still pending
        assert_eq!(collect(&mut c), ["audio 8x1".to_string(), format!("silence {DROP_EVENTS}"), "silence 1".to_string()]);
        assert_eq!(c.available(), 0);
    }

    #[test]
    fn stream_errors_set_flags() {
        let (_, _, flags) = small_ring(1, 8);
        flags.on_error(cpal::ErrorKind::Xrun);
        flags.on_error(cpal::ErrorKind::DeviceNotAvailable);
        flags.on_error(cpal::ErrorKind::StreamInvalidated);
        assert_eq!(flags.errors.load(Ordering::Relaxed), 1);
        assert!(flags.gone.load(Ordering::Relaxed));
        assert!(flags.invalidated.load(Ordering::Relaxed));
    }

    /// Producer and consumer on two threads with a tiny ring: every audio sample must arrive
    /// at its own position, and every dropped frame must come back as silence.
    #[test]
    fn concurrent_overflow_keeps_every_position_exact() {
        const TOTAL: u64 = 400_000;
        let (mut p, mut c, _) = small_ring(1, 1024);
        let producer = std::thread::spawn(move || {
            let mut pos = 0u64;
            let mut seed = 12345u32;
            let mut block = Vec::with_capacity(600);
            while pos < TOTAL {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                let n = (1 + (seed >> 16) % 600) as u64;
                let n = n.min(TOTAL - pos);
                block.clear();
                block.extend((pos..pos + n).map(|i| (i % (1 << 23)) as f32));
                p.push(&block);
                pos += n;
            }
        });
        let (mut pos, mut silent) = (0u64, 0u64);
        let mut check = |c: &mut CaptureConsumer| {
            c.drain(|chunk| {
                match chunk {
                    Chunk::Audio(a) => {
                        for (i, &v) in a.iter().enumerate() {
                            assert_eq!(v, ((pos + i as u64) % (1 << 23)) as f32, "sample at {}", pos + i as u64);
                        }
                        pos += a.len() as u64;
                    }
                    Chunk::Silence(n) => {
                        pos += n;
                        silent += n;
                    }
                }
                Ok(())
            })
            .unwrap();
        };
        while !producer.is_finished() {
            check(&mut c);
            std::thread::sleep(std::time::Duration::from_micros(300));
        }
        producer.join().unwrap();
        check(&mut c);
        assert_eq!(pos, TOTAL);
        assert!(silent > 0, "the test must force at least one overflow");
    }
}
