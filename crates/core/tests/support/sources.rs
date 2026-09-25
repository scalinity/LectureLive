//! Sources that feed a session like a device would.
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Local, TimeZone};
use lecturelive_core::audio::frame::Frame;
use lecturelive_core::audio::source::{Source, SourceEvent};
use tokio::sync::mpsc::Sender;
use uuid::Uuid;

use super::fake_stt::State;
use super::speech;

pub fn anchor() -> DateTime<Local> {
    Local.with_ymd_and_hms(2026, 9, 25, 10, 0, 0).unwrap()
}

fn begin(id: Uuid) -> SourceEvent {
    SourceEvent::Begin { recording_id: id, anchor: anchor(), source_uid: "Test_UID".into(), input_rate: 16_000, channels: 1 }
}

/// Synthetic speech, paced, telling the fake STT server how far the audio has got.
pub struct Speech {
    pub frames: u64,
    pub pace: Duration,
    pub fake: Arc<State>,
}

impl Source for Speech {
    fn run(self: Box<Self>, out: Sender<SourceEvent>, _stop: Arc<AtomicBool>) {
        let id = Uuid::new_v4();
        out.blocking_send(begin(id)).unwrap();
        for k in 0..self.frames {
            let f = Frame { recording_id: id, sample_offset: k * 1600, valid_samples: 1600, pcm16: speech::frame_pcm(k) };
            out.blocking_send(SourceEvent::Frame(f)).unwrap();
            self.fake.feed.store((k + 1) * 1600, SeqCst);
            std::thread::sleep(self.pace);
        }
        out.blocking_send(SourceEvent::End { recording_id: id, samples: self.frames * 1600, stream_errors: 0 }).unwrap();
    }
}

/// Silence as long as a recorded fixture, started once the fake has accepted the connection, so
/// the stream begins at sample 0; it can pause after `pause_after.0` frames until `pause_after.1` is set.
pub struct Silence {
    pub samples: u64,
    pub fake: Arc<State>,
    pub pause_after: Option<(u64, Arc<AtomicBool>)>,
}

impl Source for Silence {
    fn run(self: Box<Self>, out: Sender<SourceEvent>, _stop: Arc<AtomicBool>) {
        let id = Uuid::new_v4();
        out.blocking_send(begin(id)).unwrap();
        for _ in 0..5_000 {
            if self.fake.accepted.load(SeqCst) > 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        for k in 0..self.samples.div_ceil(1600) {
            if let Some((after, go)) = &self.pause_after {
                while k == *after && !go.load(SeqCst) {
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
            let valid = (self.samples - k * 1600).min(1600) as u32;
            out.blocking_send(SourceEvent::Frame(Frame { recording_id: id, sample_offset: k * 1600, valid_samples: valid, pcm16: [0; 1600] })).unwrap();
            // 33 times real time: the recorder syncs every 10 frames, and the gate runs its sessions in parallel.
            std::thread::sleep(Duration::from_millis(3));
        }
        out.blocking_send(SourceEvent::End { recording_id: id, samples: self.samples, stream_errors: 0 }).unwrap();
    }
}

/// Synthetic speech, paced, until the session stops it: a lecture of no fixed length.
pub struct Talking {
    pub pace: Duration,
    pub fake: Arc<State>,
}

impl Source for Talking {
    fn run(self: Box<Self>, out: Sender<SourceEvent>, stop: Arc<AtomicBool>) {
        let id = Uuid::new_v4();
        out.blocking_send(begin(id)).unwrap();
        let mut k = 0;
        while !stop.load(SeqCst) {
            let f = Frame { recording_id: id, sample_offset: k * 1600, valid_samples: 1600, pcm16: speech::frame_pcm(k) };
            if out.blocking_send(SourceEvent::Frame(f)).is_err() {
                return;
            }
            self.fake.feed.store((k + 1) * 1600, SeqCst);
            k += 1;
            std::thread::sleep(self.pace);
        }
        let _ = out.blocking_send(SourceEvent::End { recording_id: id, samples: k * 1600, stream_errors: 0 });
    }
}
