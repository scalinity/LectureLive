//! The source worker: owns the cpal stream on its own thread, turns ring contents into
//! timed frames, and rebuilds the stream on disappearance or rate change (spec §4.1).
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Local, TimeZone};
use cpal::traits::{DeviceTrait, StreamTrait};
use tokio::sync::mpsc::Sender;
use uuid::Uuid;

use super::capture::{ring, CaptureConsumer, Chunk, StreamFlags, RING_SECONDS};
use super::frame::Frame;
use super::input::find_input;
use super::pipeline::Pipeline;
use crate::session::sidecar::{Gap, GapKind};

const POLL: Duration = Duration::from_millis(20);
const REAPPEAR_POLL: Duration = Duration::from_millis(500);

#[derive(Debug)]
pub enum SourceEvent {
    Begin { recording_id: Uuid, anchor: DateTime<Local>, source_uid: String, input_rate: u32, channels: u16 },
    Frame(Frame),
    Gap(Gap),
    End { recording_id: Uuid, samples: u64, stream_errors: u64 },
    Level(f32),
    DeviceGone { uid: String },
    DeviceBack { uid: String },
    Failed(String),
}

pub trait Source: Send + 'static {
    /// Runs on a dedicated thread until `stop` is set or the source cannot continue.
    fn run(self: Box<Self>, out: Sender<SourceEvent>, stop: Arc<AtomicBool>);
}

pub struct DeviceSource {
    pub uid: String,
}

impl Source for DeviceSource {
    fn run(self: Box<Self>, out: Sender<SourceEvent>, stop: Arc<AtomicBool>) {
        if let Err(e) = run_device(&self.uid, &out, &stop) {
            let _ = out.blocking_send(SourceEvent::Failed(format!("{e:#}")));
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum SegmentEnd {
    Stopped,
    Gone,
    Invalidated,
}

fn send(out: &Sender<SourceEvent>, e: SourceEvent) -> Result<()> {
    out.blocking_send(e).map_err(|_| anyhow!("session closed"))
}

enum Outcome {
    Ended(SegmentEnd),
    OpenFailed(anyhow::Error),
}

fn run_device(uid: &str, out: &Sender<SourceEvent>, stop: &AtomicBool) -> Result<()> {
    supervise(uid, stop, REAPPEAR_POLL, || find_input(uid), |d| run_segment(d, uid, out, stop), |e| send(out, e))
}

/// Keeps one device recording across disappearance and rate changes. A device that cannot
/// be found or opened at the start fails the session; later it is waited for, and only the
/// same UID is ever opened again: never a switch to another source (spec §4.1).
fn supervise<D>(
    uid: &str,
    stop: &AtomicBool,
    poll: Duration,
    mut find: impl FnMut() -> Result<Option<D>>,
    mut segment: impl FnMut(&D) -> Result<Outcome>,
    mut event: impl FnMut(SourceEvent) -> Result<()>,
) -> Result<()> {
    let mut device = find()?.with_context(|| format!("input {uid} not found; `lecturelive inputs` lists them"))?;
    let mut started = false;
    loop {
        match segment(&device)? {
            Outcome::Ended(SegmentEnd::Stopped) => return Ok(()),
            Outcome::Ended(SegmentEnd::Invalidated) => {
                started = true; // same device, new configuration: rebuild at once
                continue;
            }
            Outcome::Ended(SegmentEnd::Gone) => started = true,
            Outcome::OpenFailed(e) if !started => return Err(e),
            Outcome::OpenFailed(_) => {} // listed but not usable yet: wait as if gone
        }
        event(SourceEvent::DeviceGone { uid: uid.into() })?;
        device = loop {
            if stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            std::thread::sleep(poll);
            // A lookup error while the device is away means "not yet", not the end of the session.
            if let Ok(Some(d)) = find() {
                break d;
            }
        };
        event(SourceEvent::DeviceBack { uid: uid.into() })?;
    }
}

/// One recording: the frames of one stream from its first sample to its end.
struct Segment<'a> {
    out: &'a Sender<SourceEvent>,
    recording_id: Uuid,

    samples: u64,
    level_sq: f64,
    level_n: u64,
}

impl Segment<'_> {
    fn emit(&mut self, f: Frame) -> Result<()> {
        self.samples = f.sample_offset + f.valid_samples as u64;
        for &s in f.pcm() {
            self.level_sq += (s as f64 / i16::MAX as f64).powi(2);
        }
        self.level_n += f.valid_samples as u64;
        if self.level_n >= 16_000 {
            let _ = self.out.try_send(SourceEvent::Level((self.level_sq / self.level_n as f64).sqrt() as f32));
            (self.level_sq, self.level_n) = (0.0, 0);
        }
        send(self.out, SourceEvent::Frame(f))
    }

    fn pump(&mut self, pipeline: &mut Pipeline, consumer: &mut CaptureConsumer) -> Result<()> {
        consumer.drain(|chunk| match chunk {
            Chunk::Audio(a) => pipeline.push(a)?.into_iter().try_for_each(|f| self.emit(f)),
            Chunk::Silence(n) => {
                let (lost, frames) = pipeline.push_silence(n)?;
                let gap = Gap { recording_id: self.recording_id, start_sample: lost.start, end_sample: Some(lost.end), kind: GapKind::CaptureOverflow, resolved: false };
                send(self.out, SourceEvent::Gap(gap))?;
                frames.into_iter().try_for_each(|f| self.emit(f))
            }
        })
    }
}

/// Opens and starts the device's input stream at its own configuration.
fn open_stream(device: &cpal::Device) -> Result<(cpal::Stream, CaptureConsumer, Arc<StreamFlags>, u32, u16)> {
    let config = device.default_input_config()?;
    let rate = config.sample_rate();
    let channels = config.channels();
    let (mut producer, consumer, flags) = ring(channels, rate, RING_SECONDS);
    let err_flags = flags.clone();
    let on_error = move |e: cpal::Error| err_flags.on_error(e.kind());
    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => {
            device.build_input_stream(config.config(), move |d: &[f32], _: &cpal::InputCallbackInfo| producer.push(d), on_error, None)?
        }
        cpal::SampleFormat::I16 => {
            let mut scratch: Vec<f32> = Vec::with_capacity(16_384 * channels as usize);
            device.build_input_stream(
                config.config(),
                move |d: &[i16], _: &cpal::InputCallbackInfo| {
                    scratch.clear(); // within capacity: no allocation in the callback
                    scratch.extend(d.iter().map(|&s| s as f32 / i16::MAX as f32));
                    producer.push(&scratch);
                },
                on_error,
                None,
            )?
        }
        other => bail!("unsupported sample format {other:?}"),
    };
    stream.play()?;
    Ok((stream, consumer, flags, rate, channels))
}

/// Runs one recording on `device`. A stream that cannot be opened is an outcome, not an
/// error: after a replug CoreAudio can list a device a moment before it is usable.
fn run_segment(device: &cpal::Device, uid: &str, out: &Sender<SourceEvent>, stop: &AtomicBool) -> Result<Outcome> {
    let (stream, mut consumer, flags, rate, channels) = match open_stream(device) {
        Ok(opened) => opened,
        Err(e) => return Ok(Outcome::OpenFailed(e)),
    };
    let recording_id = Uuid::new_v4();
    let mut pipeline = Pipeline::new(recording_id, rate, channels)?;
    let mut seg = Segment { out, recording_id, samples: 0, level_sq: 0.0, level_n: 0 };
    let begin = |ms: u64| -> Result<()> {
        let anchor = Local.timestamp_millis_opt(ms as i64).single().context("anchor time")?;
        send(out, SourceEvent::Begin { recording_id, anchor, source_uid: uid.into(), input_rate: rate, channels })
    };
    let mut begun = false;
    let end = loop {
        if !begun {
            if let Some(ms) = flags.anchor_ms() {
                begin(ms)?;
                begun = true;
            }
        }
        if begun {
            seg.pump(&mut pipeline, &mut consumer)?;
        }
        if stop.load(Ordering::Relaxed) {
            break SegmentEnd::Stopped;
        }
        if flags.gone.load(Ordering::Acquire) {
            break SegmentEnd::Gone;
        }
        if flags.invalidated.load(Ordering::Acquire) {
            break SegmentEnd::Invalidated;
        }
        std::thread::sleep(POLL);
    };
    drop(stream);
    if !begun {
        match flags.anchor_ms() {
            Some(ms) => begin(ms)?,
            None => return Ok(Outcome::Ended(end)), // no audio ever arrived: nothing to record
        }
    }
    seg.pump(&mut pipeline, &mut consumer)?;
    for f in pipeline.finish()? {
        seg.emit(f)?;
    }
    send(out, SourceEvent::End { recording_id, samples: seg.samples, stream_errors: flags.errors.load(Ordering::Relaxed) })?;
    let kind = match end {
        SegmentEnd::Stopped => return Ok(Outcome::Ended(end)),
        SegmentEnd::Gone => GapKind::DeviceGone,
        SegmentEnd::Invalidated => GapKind::RateChange,
    };
    send(out, SourceEvent::Gap(Gap { recording_id, start_sample: seg.samples, end_sample: None, kind, resolved: false }))?;
    Ok(Outcome::Ended(end))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::routing::BLACKHOLE_UID;
    use std::collections::VecDeque;

    /// Drives `supervise` with scripted lookups and segment outcomes; returns the events it
    /// sent and the devices it opened. Lookup script entries: Some(Ok(dev)), Some(Err) or None.
    fn drive(lookups: Vec<Result<Option<u32>>>, outcomes: Vec<Result<Outcome>>) -> (Result<()>, Vec<String>, Vec<u32>) {
        let mut lookups: VecDeque<_> = lookups.into();
        let mut outcomes: VecDeque<_> = outcomes.into();
        let (mut events, mut opened) = (Vec::new(), Vec::new());
        let stop = AtomicBool::new(false);
        let r = supervise(
            "Receiver_UID",
            &stop,
            Duration::ZERO,
            || lookups.pop_front().unwrap_or(Ok(None)),
            |d: &u32| {
                opened.push(*d);
                outcomes.pop_front().expect("no more scripted segments")
            },
            |e| {
                events.push(match e {
                    SourceEvent::DeviceGone { .. } => "gone".to_string(),
                    SourceEvent::DeviceBack { .. } => "back".to_string(),
                    other => format!("{other:?}"),
                });
                Ok(())
            },
        );
        (r, events, opened)
    }

    fn open_failed() -> Result<Outcome> {
        Ok(Outcome::OpenFailed(anyhow!("device not ready")))
    }

    #[test]
    fn a_device_that_is_not_usable_yet_after_replug_is_waited_for_again() {
        let (r, events, opened) = drive(
            vec![Ok(Some(7)), Ok(None), Ok(Some(7)), Err(anyhow!("HAL busy")), Ok(Some(7))],
            vec![Ok(Outcome::Ended(SegmentEnd::Gone)), open_failed(), Ok(Outcome::Ended(SegmentEnd::Stopped))],
        );
        r.unwrap();
        assert_eq!(events, ["gone", "back", "gone", "back"]);
        assert_eq!(opened, [7, 7, 7], "only the same device is ever opened");
    }

    #[test]
    fn a_failed_rebuild_after_a_rate_change_waits_instead_of_ending_the_session() {
        let (r, events, _) = drive(
            vec![Ok(Some(7)), Ok(Some(7))],
            vec![Ok(Outcome::Ended(SegmentEnd::Invalidated)), open_failed(), Ok(Outcome::Ended(SegmentEnd::Stopped))],
        );
        r.unwrap();
        assert_eq!(events, ["gone", "back"]);
    }

    #[test]
    fn a_device_that_cannot_open_at_the_start_fails_the_session() {
        let (r, events, _) = drive(vec![Ok(Some(7))], vec![open_failed()]);
        assert!(format!("{:#}", r.unwrap_err()).contains("device not ready"));
        assert!(events.is_empty());
    }

    #[test]
    fn a_missing_device_at_the_start_fails_the_session() {
        let (r, _, opened) = drive(vec![Ok(None)], vec![]);
        assert!(format!("{:#}", r.unwrap_err()).contains("not found"));
        assert!(opened.is_empty());
    }

    /// Needs BlackHole 2ch and microphone permission for the process running the test:
    /// cargo test -p lecturelive-core source -- --ignored --nocapture
    #[test]
    #[ignore]
    fn blackhole_records_contiguous_frames_by_uid() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(1024);
        let stop = Arc::new(AtomicBool::new(false));
        let s = stop.clone();
        let t = std::thread::spawn(move || Box::new(DeviceSource { uid: BLACKHOLE_UID.into() }).run(tx, s));
        let mut events = vec![rx.blocking_recv().expect("the source sends Begin once audio arrives")];
        std::thread::sleep(Duration::from_secs(2)); // two seconds of audio, timed from the first sample
        stop.store(true, Ordering::Relaxed);
        t.join().unwrap();
        while let Ok(e) = rx.try_recv() {
            events.push(e);
        }
        let SourceEvent::Begin { recording_id, input_rate, .. } = &events[0] else { panic!("first event must be Begin: {:?}", events.first()) };
        let mut next = 0;
        for e in &events {
            if let SourceEvent::Frame(f) = e {
                assert_eq!(f.recording_id, *recording_id);
                assert_eq!(f.sample_offset, next);
                next += 1600;
            }
        }
        let Some(SourceEvent::End { samples, stream_errors, .. }) = events.last() else { panic!("last event must be End") };
        println!("{input_rate} Hz, {samples} samples, {stream_errors} stream errors");
        assert!((28_800..=36_800).contains(samples), "about 2 s: {samples}");
    }
}
