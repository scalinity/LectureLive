//! Mixed mode's source (spec §4.1, §4.2): the loopback and an input, each on its own stream and clock, mixed
//! into one recording that lasts while either is there. A source that goes leaves a gap in the recording, from
//! where its audio ends to where it joins again, and the other carries on; when both have gone, the recording
//! ends, and the next begins when one returns.
use std::any::Any;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use chrono::{Local, TimeZone};
use tokio::sync::mpsc::Sender;
use uuid::Uuid;

use super::capture::{CaptureConsumer, Chunk, StreamFlags};
use super::frame::FrameBuilder;
use super::input::find_input;
use super::mix::{LaneConfig, LaneStats, Mixer, DELAY, RATE};
use super::source::{open_stream, Source, SourceEvent};
use crate::session::sidecar::{Gap, GapKind};

/// Mixed mode ships once it has passed the two-hour drift test (spec §4.2; M6 Findings).
pub const MIXED_MODE: bool = true;
pub const LOOPBACK: usize = 0;
pub const INPUT: usize = 1;
const POLL: Duration = Duration::from_millis(20);
const REAPPEAR_POLL: Duration = Duration::from_millis(500);
/// When a recording begins, how long it waits for both sources' first audio.
const START_WAIT: Duration = Duration::from_secs(2);

pub struct MixedSource {
    pub loopback: String,
    pub input: String,
    pub gains: [f32; 2],
}

impl MixedSource {
    pub fn new(loopback: &str, input: &str) -> Self {
        Self { loopback: loopback.into(), input: input.into(), gains: [1.0, 1.0] }
    }

    /// How the sidecar names the source of its recordings.
    pub fn uid(&self) -> String {
        format!("mixed:{}+{}", self.loopback, self.input)
    }
}

impl Source for MixedSource {
    fn run(self: Box<Self>, out: Sender<SourceEvent>, stop: Arc<AtomicBool>) {
        let uid = self.uid();
        let devices: [Box<dyn LaneDevice>; 2] = [Box::new(CpalLane(self.loopback.clone())), Box::new(CpalLane(self.input.clone()))];
        let mut emit = |e: SourceEvent| out.blocking_send(e).map_err(|_| anyhow!("session closed"));
        if let Err(e) = run_mixed(devices, self.gains, &uid, &mut emit, &stop, None) {
            let _ = out.blocking_send(SourceEvent::Failed(format!("{e:#}")));
        }
    }
}

pub(crate) struct OpenLane {
    pub consumer: CaptureConsumer,
    pub flags: Arc<StreamFlags>,
    pub rate: u32,
    pub channels: u16,
    /// Keeps the stream running; dropping it stops the stream.
    pub stream: Box<dyn Any>,
}

pub(crate) trait LaneDevice {
    fn uid(&self) -> &str;
    /// Opens and starts the device's stream; None while it is not listed.
    fn open(&mut self) -> Result<Option<OpenLane>>;
}

struct CpalLane(String);

impl LaneDevice for CpalLane {
    fn uid(&self) -> &str {
        &self.0
    }
    fn open(&mut self) -> Result<Option<OpenLane>> {
        let Some(d) = find_input(&self.0)? else { return Ok(None) };
        let (stream, consumer, flags, rate, channels) = open_stream(&d)?;
        Ok(Some(OpenLane { consumer, flags, rate, channels, stream: Box::new(stream) }))
    }
}

enum State {
    /// Gone: looked for again at `next`.
    Waiting { next: Instant },
    /// Streaming, not yet in the mix.
    Opened(OpenLane),
    Live(OpenLane),
}

struct Rec {
    id: Uuid,
    t0: Instant,
    frames: FrameBuilder,
    level_at: u64,
}

/// The lanes' state every 10 s, for the live drift measurement: seconds since the start, and each lane's stats.
pub(crate) type StatsLog = Arc<Mutex<Vec<(f64, [Option<LaneStats>; 2])>>>;

fn drain(l: &mut OpenLane, i: usize, mixer: &mut Mixer, rec: Uuid, emit: &mut dyn FnMut(SourceEvent) -> Result<()>) -> Result<()> {
    let mut lost = Vec::new();
    l.consumer.drain(|c| match c {
        Chunk::Audio(a) => mixer.push(i, a),
        Chunk::Silence(n) => {
            lost.push(mixer.push_silence(i, n)?);
            Ok(())
        }
    })?;
    for r in lost {
        emit(SourceEvent::Gap(Gap::new(rec, r.start, Some(r.end), GapKind::CaptureOverflow)))?;
    }
    Ok(())
}

fn end_recording(r: Rec, mixer: &mut Mixer, emit: &mut dyn FnMut(SourceEvent) -> Result<()>) -> Result<()> {
    let Rec { id, mut frames, .. } = r;
    for f in frames.push(&mixer.flush()) {
        emit(SourceEvent::Frame(f))?;
    }
    if let Some(f) = frames.finish() {
        emit(SourceEvent::Frame(f))?;
    }
    emit(SourceEvent::End { recording_id: id, samples: mixer.emitted(), stream_errors: 0 })
}

/// The loop, on the source's thread: open both, mix on the host clock, follow each source away and back.
pub(crate) fn run_mixed(mut devices: [Box<dyn LaneDevice>; 2], gains: [f32; 2], uid: &str, emit: &mut dyn FnMut(SourceEvent) -> Result<()>, stop: &AtomicBool, stats: Option<StatsLog>) -> Result<()> {
    let mut state = Vec::new();
    for d in devices.iter_mut() {
        let l = d.open()?.with_context(|| format!("input {} not found; `lecturelive inputs` lists them", d.uid()))?;
        state.push(State::Opened(l));
    }
    let (started, mut first) = (Instant::now(), true);
    let mut mixer = Mixer::new(2);
    let mut open_gap: [Option<u64>; 2] = [None, None];
    let mut was_gone = [false, false];
    let mut rec: Option<Rec> = None;
    let mut stats_at = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        for (i, s) in state.iter_mut().enumerate() {
            if let State::Waiting { next } = s {
                if Instant::now() >= *next {
                    match devices[i].open() {
                        Ok(Some(l)) => *s = State::Opened(l),
                        _ => *next = Instant::now() + REAPPEAR_POLL, // "not yet", as a single source waits
                    }
                }
            }
        }
        if rec.is_none() {
            let heard: Vec<bool> = state.iter().map(|s| matches!(s, State::Opened(l) if l.flags.anchor_ms().is_some())).collect();
            let unheard = state.iter().zip(&heard).any(|(s, h)| matches!(s, State::Opened(_)) && !h);
            if heard.iter().any(|&h| h) && !(first && unheard && started.elapsed() < START_WAIT) {
                first = false;
                let id = Uuid::new_v4();
                let ms = Local::now().timestamp_millis() - (DELAY as f64 / RATE * 1000.0) as i64;
                let anchor = Local.timestamp_millis_opt(ms).single().context("anchor time")?;
                emit(SourceEvent::Begin { recording_id: id, anchor, source_uid: uid.into(), input_rate: RATE as u32, channels: 1 })?;
                rec = Some(Rec { id, t0: Instant::now(), frames: FrameBuilder::new(id), level_at: 0 });
                mixer = Mixer::new(2);
                for i in 0..2 {
                    if !heard[i] {
                        open_gap[i] = Some(0); // not there yet: missing from the recording's first sample
                        emit(SourceEvent::Gap(Gap::new(id, 0, None, GapKind::DeviceGone)))?;
                    }
                }
            }
        }
        if let Some(r) = &rec {
            for i in 0..2 {
                let heard = matches!(&state[i], State::Opened(l) if l.flags.anchor_ms().is_some());
                if !heard {
                    continue;
                }
                let State::Opened(mut l) = std::mem::replace(&mut state[i], State::Waiting { next: Instant::now() }) else { unreachable!() };
                let age = l.flags.age_of_oldest(l.consumer.available(), l.rate);
                let due = (r.t0.elapsed().as_secs_f64() * RATE) as u64;
                let at = mixer.join(i, LaneConfig { rate: l.rate, channels: l.channels, gain: gains[i] }, age, due)?;
                if let Some(start) = open_gap[i].take() {
                    emit(SourceEvent::GapEnd { recording_id: r.id, start_sample: start, end_sample: at })?;
                }
                if std::mem::take(&mut was_gone[i]) {
                    emit(SourceEvent::DeviceBack { uid: devices[i].uid().into() })?;
                }
                drain(&mut l, i, &mut mixer, r.id, emit)?;
                state[i] = State::Live(l);
            }
            for i in 0..2 {
                let State::Live(l) = &mut state[i] else { continue };
                drain(l, i, &mut mixer, r.id, emit)?;
                let (gone, invalid) = (l.flags.gone.load(Ordering::Acquire), l.flags.invalidated.load(Ordering::Acquire));
                if !gone && !invalid {
                    continue;
                }
                let next = Instant::now() + if invalid { Duration::ZERO } else { REAPPEAR_POLL };
                let State::Live(mut l) = std::mem::replace(&mut state[i], State::Waiting { next }) else { unreachable!() };
                drop(std::mem::replace(&mut l.stream, Box::new(())));
                drain(&mut l, i, &mut mixer, r.id, emit)?;
                let at = mixer.leave(i)?;
                open_gap[i] = Some(at);
                emit(SourceEvent::Gap(Gap::new(r.id, at, None, if invalid { GapKind::RateChange } else { GapKind::DeviceGone })))?;
                if gone {
                    was_gone[i] = true;
                    emit(SourceEvent::DeviceGone { uid: devices[i].uid().into() })?;
                }
            }
        }
        if let Some(r) = rec.as_mut() {
            let pulled = mixer.pull((r.t0.elapsed().as_secs_f64() * RATE) as u64);
            for (_, g) in pulled.gaps {
                emit(SourceEvent::Gap(Gap::new(r.id, g.start, Some(g.end), GapKind::CaptureOverflow)))?;
            }
            for f in r.frames.push(&pulled.samples) {
                emit(SourceEvent::Frame(f))?;
            }
            if mixer.emitted() >= r.level_at + RATE as u64 {
                r.level_at = mixer.emitted();
                // The meter and the silence warning follow the loopback: in mixed mode it is Zoom's sound that
                // must not go silent (spec §4.3 step 4); a room microphone's noise would hide it.
                if let Some(l) = mixer.level(LOOPBACK).or_else(|| mixer.level(INPUT)) {
                    emit(SourceEvent::Level(l))?;
                }
            }
            if !mixer.any() {
                // Every source has gone: the recording ends here; each one's open gap runs to its end.
                end_recording(rec.take().expect("a recording"), &mut mixer, emit)?;
                open_gap = [None, None];
            }
        }
        if let Some(log) = &stats {
            if stats_at.elapsed() >= Duration::from_secs(10) {
                stats_at = Instant::now();
                log.lock().expect("the stats lock").push((started.elapsed().as_secs_f64(), [mixer.stats(0), mixer.stats(1)]));
            }
        }
        std::thread::sleep(POLL);
    }
    // Stop: the streams end, what they captured is drained, and the mix flushed.
    for s in state.iter_mut() {
        if let State::Live(l) = s {
            drop(std::mem::replace(&mut l.stream, Box::new(())));
        }
    }
    if let Some(r) = rec.take() {
        for (i, s) in state.iter_mut().enumerate() {
            if let State::Live(l) = s {
                drain(l, i, &mut mixer, r.id, emit)?;
            }
        }
        end_recording(r, &mut mixer, emit)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::capture::ring;
    use std::sync::atomic::AtomicBool;
    use std::thread::JoinHandle;

    #[derive(Default)]
    struct Ctl {
        listed: AtomicBool,
        unplug: AtomicBool,
    }

    /// A device on its own thread, producing `value` at `rate` in 10 ms blocks until it is unplugged.
    struct Fake {
        uid: String,
        rate: u32,
        value: f32,
        ctl: Arc<Ctl>,
    }

    struct FakeStream {
        stop: Arc<AtomicBool>,
        thread: Option<JoinHandle<()>>,
    }

    impl Drop for FakeStream {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
        }
    }

    impl LaneDevice for Fake {
        fn uid(&self) -> &str {
            &self.uid
        }
        fn open(&mut self) -> Result<Option<OpenLane>> {
            if !self.ctl.listed.load(Ordering::Relaxed) {
                return Ok(None);
            }
            self.ctl.unplug.store(false, Ordering::Relaxed);
            let (mut p, consumer, flags) = ring(1, self.rate, 4);
            let stop = Arc::new(AtomicBool::new(false));
            let (s, f, ctl, rate, value) = (stop.clone(), flags.clone(), self.ctl.clone(), self.rate, self.value);
            let thread = std::thread::spawn(move || {
                let (block, start, mut sent) = ((rate / 100) as u64, Instant::now(), 0u64);
                while !s.load(Ordering::Relaxed) {
                    if ctl.unplug.load(Ordering::Relaxed) {
                        f.on_error(cpal::ErrorKind::DeviceNotAvailable);
                        return;
                    }
                    while sent + block <= (start.elapsed().as_secs_f64() * rate as f64) as u64 {
                        f.note_callback(Some(Duration::from_millis(3)));
                        p.push(&vec![value; block as usize]);
                        sent += block;
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
            });
            Ok(Some(OpenLane { consumer, flags, rate: self.rate, channels: 1, stream: Box::new(FakeStream { stop, thread: Some(thread) }) }))
        }
    }

    fn fake(uid: &str, rate: u32, value: f32) -> (Fake, Arc<Ctl>) {
        let ctl = Arc::new(Ctl::default());
        ctl.listed.store(true, Ordering::Relaxed);
        (Fake { uid: uid.into(), rate, value, ctl: ctl.clone() }, ctl)
    }

    /// Runs the loop on its own thread while `script` plays out on this one; returns every event.
    fn run(a: Fake, b: Fake, script: impl FnOnce(&AtomicBool)) -> (Result<()>, Vec<SourceEvent>) {
        let stop = Arc::new(AtomicBool::new(false));
        let s = stop.clone();
        let t = std::thread::spawn(move || {
            let mut events = Vec::new();
            let r = run_mixed([Box::new(a), Box::new(b)], [1.0, 1.0], "mixed:a+b", &mut |e| {
                events.push(e);
                Ok(())
            }, &s, None);
            (r, events)
        });
        script(&stop);
        stop.store(true, Ordering::Relaxed);
        t.join().unwrap()
    }

    fn sleep(ms: u64) {
        std::thread::sleep(Duration::from_millis(ms));
    }

    /// The mean of the frames' samples between two output positions.
    fn mean(events: &[SourceEvent], from: u64, to: u64) -> f64 {
        let v: Vec<f64> = events
            .iter()
            .filter_map(|e| if let SourceEvent::Frame(f) = e { Some(f) } else { None })
            .flat_map(|f| f.pcm().iter().enumerate().map(move |(k, &s)| (f.sample_offset + k as u64, s)))
            .filter(|(p, _)| (from..to).contains(p))
            .map(|(_, s)| s as f64 / i16::MAX as f64)
            .collect();
        v.iter().sum::<f64>() / v.len().max(1) as f64
    }

    #[test]
    fn a_mixed_recording_carries_both_and_goes_on_when_one_leaves() {
        let (a, _) = fake("BlackHole", 48_000, 0.25);
        let (b, ctl) = fake("Receiver", 44_100, 0.5);
        let (r, events) = run(a, b, |_| {
            sleep(1_500);
            ctl.listed.store(false, Ordering::Relaxed);
            ctl.unplug.store(true, Ordering::Relaxed);
            sleep(1_500);
            ctl.listed.store(true, Ordering::Relaxed);
            sleep(1_500);
        });
        r.unwrap();
        let begins = events.iter().filter(|e| matches!(e, SourceEvent::Begin { .. })).count();
        assert_eq!(begins, 1, "one recording throughout");
        let mut next = 0;
        for e in &events {
            if let SourceEvent::Frame(f) = e {
                assert_eq!(f.sample_offset, next, "frames are contiguous");
                next += 1600;
            }
        }
        let gone = events.iter().find_map(|e| if let SourceEvent::Gap(g) = e { (g.kind == GapKind::DeviceGone).then_some(g.start_sample) } else { None }).expect("a gap where the receiver went");
        let back = events.iter().find_map(|e| if let SourceEvent::GapEnd { start_sample, end_sample, .. } = e { (*start_sample == gone).then_some(*end_sample) } else { None }).expect("its end where it joined again");
        assert!(events.iter().any(|e| matches!(e, SourceEvent::DeviceGone { uid } if uid == "Receiver")));
        assert!(events.iter().any(|e| matches!(e, SourceEvent::DeviceBack { uid } if uid == "Receiver")));
        assert!((mean(&events, 8_000, 12_000) - 0.75).abs() < 0.03, "both: {}", mean(&events, 8_000, 12_000));
        assert!((mean(&events, gone + 4_000, back - 1_000) - 0.25).abs() < 0.03, "the loopback alone while the receiver was away");
        assert!((mean(&events, back + 8_000, back + 12_000) - 0.75).abs() < 0.03, "both again");
        let Some(SourceEvent::End { samples, .. }) = events.iter().rev().find(|e| matches!(e, SourceEvent::End { .. })) else { panic!("no End") };
        assert!((4.3..=5.2).contains(&(*samples as f64 / 16_000.0)), "{samples}");
    }

    #[test]
    fn when_both_leave_the_recording_ends_and_the_next_begins_when_one_returns() {
        let (a, ca) = fake("BlackHole", 48_000, 0.25);
        let (b, cb) = fake("Receiver", 48_000, 0.5);
        let (r, events) = run(a, b, |_| {
            sleep(1_000);
            for c in [&ca, &cb] {
                c.listed.store(false, Ordering::Relaxed);
                c.unplug.store(true, Ordering::Relaxed);
            }
            sleep(1_200);
            ca.listed.store(true, Ordering::Relaxed);
            sleep(1_500);
        });
        r.unwrap();
        let kinds: Vec<&str> = events.iter().filter_map(|e| match e { SourceEvent::Begin { .. } => Some("begin"), SourceEvent::End { .. } => Some("end"), _ => None }).collect();
        assert_eq!(kinds, ["begin", "end", "begin", "end"]);
    }

    #[test]
    fn a_source_missing_at_the_start_fails_the_session() {
        let (a, _) = fake("BlackHole", 48_000, 0.25);
        let (b, cb) = fake("Receiver", 48_000, 0.5);
        cb.listed.store(false, Ordering::Relaxed);
        let (r, _) = run(a, b, |_| sleep(100));
        assert!(format!("{:#}", r.unwrap_err()).contains("Receiver not found"));
    }

    /// Review Focus 2: in mixed mode the meter and the silence warning follow Zoom's sound (spec §4.3 step 4).
    #[test]
    fn the_level_follows_the_loopback_even_when_the_input_is_loud() {
        let (a, _) = fake("BlackHole", 48_000, 0.0);
        let (b, _) = fake("Receiver", 48_000, 0.5);
        let (r, events) = run(a, b, |_| sleep(3_500));
        r.unwrap();
        let levels: Vec<f32> = events.iter().filter_map(|e| if let SourceEvent::Level(l) = e { Some(*l) } else { None }).collect();
        assert!(levels.len() >= 2, "{levels:?}");
        assert!(levels.iter().all(|&l| l < 0.001), "the loopback is silent whatever the input hears: {levels:?}");
    }

    /// The live drift measurement (M6 plan, Task 11), on this Mac's two real inputs. The microphone's audio is
    /// counted in memory only: nothing is written or sent. LECTURELIVE_MIX_SECS sets the length (default 60):
    /// LECTURELIVE_MIX_SECS=7200 cargo test -p lecturelive-core blackhole_and_the_built_in -- --ignored --nocapture
    #[test]
    #[ignore]
    fn blackhole_and_the_built_in_microphone_stay_aligned() {
        let secs: u64 = std::env::var("LECTURELIVE_MIX_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(60);
        let stop = Arc::new(AtomicBool::new(false));
        let log: StatsLog = Default::default();
        let s = stop.clone();
        let timer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(secs));
            s.store(true, Ordering::Relaxed);
        });
        let (mut next, mut begins, mut gaps) = (0u64, 0, Vec::new());
        let devices: [Box<dyn LaneDevice>; 2] = [Box::new(CpalLane(crate::audio::routing::BLACKHOLE_UID.into())), Box::new(CpalLane("BuiltInMicrophoneDevice".into()))];
        run_mixed(devices, [1.0, 0.0], "mixed:live", &mut |e| {
            match e {
                SourceEvent::Begin { .. } => begins += 1,
                SourceEvent::Frame(f) => {
                    assert_eq!(f.sample_offset, next, "contiguous");
                    next += 1600;
                }
                SourceEvent::Gap(g) => gaps.push(g),
                _ => {}
            }
            Ok(())
        }, &stop, Some(log.clone()))
        .unwrap();
        timer.join().unwrap();
        let log = log.lock().unwrap();
        let off = |s: &LaneStats| s.filtered - s.target.unwrap_or(s.filtered);
        let mut worst: f64 = 0.0;
        for (t, s) in log.iter() {
            let [Some(a), Some(b)] = s else { continue };
            println!("{t:>6.0} s  loopback level {:>5} ({:+6.1}, {:+7.1} ppm)  microphone level {:>5} ({:+6.1}, {:+7.1} ppm)", a.level, off(a), a.correction_ppm, b.level, off(b), b.correction_ppm);
            if *t > 30.0 {
                worst = worst.max((off(a) - off(b)).abs());
            }
        }
        let last = log.last().expect("stats");
        println!("{} s recorded; worst relative offset after 30 s {worst:.1} samples ({:.2} ms); underruns {:?}", next / 16_000, worst / 16.0, last.1.map(|s| s.map(|s| s.underrun)));
        assert_eq!(begins, 1);
        assert!(gaps.is_empty(), "{gaps:?}");
        assert!(last.1.iter().all(|s| s.is_some_and(|s| s.underrun == 0)));
        assert!(worst <= 160.0, "{worst}");
    }
}
