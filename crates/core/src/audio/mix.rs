//! Mixed mode (spec §4.2): the loopback and an input, on independent clocks, become one 16 kHz stream.
//! Each source is downmixed and resampled to 16 kHz at its nominal rate (the FFT resampler single sources
//! use), then passes a drift stage, rubato's asynchronous polynomial resampler at a ratio near 1, into a
//! FIFO. The mixer takes from every FIFO exactly the samples the host clock says are due. A source joins
//! with silence in front of it, so its first sample lands where its capture time belongs, DELAY behind the
//! host clock; its drift stage is then steered to hold its FIFO at the level it settled at, however far
//! its clock drifts.
use std::collections::VecDeque;
use std::ops::Range;

use anyhow::Result;
use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Adjustable, Async, FixedAsync, PolynomialDegree, Resampler};

use super::convert::{downmix, Resampler16k};

pub const RATE: f64 = 16_000.0;
/// How far the mix runs behind the host clock (200 ms): room for a late callback or a slow tick.
pub const DELAY: u64 = 3_200;
/// Seconds after a source joins before its smoothed level becomes its target and steering starts.
const SETTLE: f64 = 4.0;
/// Seconds over which the FIFO level is smoothed before it steers.
const LEVEL_TAU: f64 = 2.0;
/// Steering per sample of level error, and per sample-second of its integral (about 20 s to settle).
const KP: f64 = 3.0e-6;
const KI: f64 = 4.0e-8;
/// The drift stage stays within 0.2% of a ratio of 1: far beyond any crystal.
const MAX_CORRECTION: f64 = 0.002;
/// A level this far from its target (100 ms) is not drift but a stall or a burst: the source realigns. Callback
/// blocks and the FFT resampler's chunks move the level by at most about 600 samples.
const RESYNC: f64 = 1_600.0;
const DRIFT_CHUNK: usize = 160;
const SILENCE_BLOCK: u64 = 4_096;

#[derive(Debug, Clone, Copy)]
pub struct LaneConfig {
    pub rate: u32,
    pub channels: u16,
    pub gain: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LaneStats {
    pub level: usize,
    pub filtered: f64,
    pub target: Option<f64>,
    pub correction_ppm: f64,
    pub underrun: u64,
}

pub struct Pulled {
    pub samples: Vec<f32>,
    /// Stretches of the output that miss a source's audio: it ran dry, or its excess was dropped to realign.
    pub gaps: Vec<(usize, Range<u64>)>,
}

struct Lane {
    channels: usize,
    input_rate: f64,
    gain: f32,
    to16k: Option<Resampler16k>,
    drift: Async<f32>,
    drift_in: Vec<f32>,
    skip: usize,
    fifo: VecDeque<f32>,
    /// Where the source's first kept sample lands, and the input frames it has sent since.
    origin: u64,
    in_frames: u64,
    /// Input frames at the front older than DELAY when the source joined: dropped, so it is not late for good.
    skip_input: u64,
    /// A stretch without audio has just ended.
    resumed: bool,
    age: f64,
    filtered: f64,
    target: Option<f64>,
    integral: f64,
    correction: f64,
    underrun: u64,
    dry_since: Option<u64>,
    draining: bool,
    sq: f64,
    n: u64,
}

impl Lane {
    fn new(cfg: LaneConfig, origin: u64, prefill: usize, skip_input: u64) -> Result<Self> {
        let drift = Async::<f32>::new_poly(1.0, 1.01, PolynomialDegree::Septic, DRIFT_CHUNK, 1, FixedAsync::Input)?;
        let skip = drift.output_delay();
        Ok(Self {
            channels: cfg.channels.max(1) as usize,
            input_rate: cfg.rate as f64,
            gain: cfg.gain,
            to16k: Some(Resampler16k::new(cfg.rate)?),
            drift,
            drift_in: Vec::new(),
            skip,
            fifo: std::iter::repeat_n(0.0, prefill).collect(),
            origin,
            in_frames: 0,
            skip_input,
            resumed: false,
            age: 0.0,
            filtered: 0.0,
            target: None,
            integral: 0.0,
            correction: 0.0,
            underrun: 0,
            dry_since: None,
            draining: false,
            sq: 0.0,
            n: 0,
        })
    }

    fn feed16k(&mut self, s: &[f32]) -> Result<()> {
        self.drift_in.extend_from_slice(s);
        while self.drift_in.len() >= self.drift.input_frames_next() {
            let n = self.drift.input_frames_next();
            let out = self.drift.process(&InterleavedSlice::new(&self.drift_in[..n], 1, n)?, None)?.take_data();
            self.drift_in.drain(..n);
            let skip = self.skip.min(out.len());
            self.skip -= skip;
            self.fifo.extend(&out[skip..]);
        }
        Ok(())
    }

    fn push(&mut self, interleaved: &[f32]) -> Result<()> {
        let skip = (self.skip_input as usize).min(interleaved.len() / self.channels);
        self.skip_input -= skip as u64;
        let interleaved = &interleaved[skip * self.channels..];
        self.in_frames += (interleaved.len() / self.channels) as u64;
        let Some(r) = self.to16k.as_mut() else { return Ok(()) };
        let s = r.push(&downmix(interleaved, self.channels))?;
        self.feed16k(&s)
    }

    /// Where input frame `k` lands in the output.
    fn position(&self, k: u64) -> u64 {
        self.origin + (k as f64 * RATE / self.input_rate).round() as u64
    }

    /// The source has gone: the resamplers' tails go into the FIFO, which then plays out.
    fn finish(&mut self) -> Result<()> {
        if let Some(r) = self.to16k.take() {
            let s = r.finish()?;
            self.feed16k(&s)?;
        }
        let rest: Vec<f32> = self.drift_in.drain(..).collect();
        self.fifo.extend(rest); // under 10 ms, at a ratio within 0.2% of 1
        self.draining = true;
        Ok(())
    }

    /// Adds this source's next `out.len()` samples to `out`, which begins at output position `at`. Returns
    /// a stretch it could not fill that has just ended.
    fn take(&mut self, out: &mut [f32], at: u64) -> Option<Range<u64>> {
        let mut ended = None;
        for (k, o) in out.iter_mut().enumerate() {
            match self.fifo.pop_front() {
                Some(s) => {
                    *o += self.gain * s;
                    self.sq += (s as f64) * (s as f64);
                    self.n += 1;
                    if let Some(from) = self.dry_since.take() {
                        ended = Some(from..at + k as u64);
                        self.resumed = true;
                    }
                }
                None if !self.draining => {
                    self.underrun += 1;
                    self.dry_since.get_or_insert(at + k as u64);
                }
                None => {}
            }
        }
        ended
    }

    /// Steers the drift stage toward the target level; a level past RESYNC realigns at once. `at` is the
    /// output position after this pull. Returns a stretch dropped to realign.
    fn steer(&mut self, dt: f64, at: u64) -> Result<Option<Range<u64>>> {
        if self.draining || dt <= 0.0 {
            return Ok(None);
        }
        let level = self.fifo.len() as f64;
        self.filtered = if self.age == 0.0 { level } else { self.filtered + (dt / LEVEL_TAU).min(1.0) * (level - self.filtered) };
        self.age += dt;
        if self.age < SETTLE {
            return Ok(None);
        }
        let target = *self.target.get_or_insert(self.filtered);
        if level > target + RESYNC {
            let excess = (level - target) as usize;
            self.fifo.drain(..excess);
            self.filtered = target;
            self.integral = 0.0;
            return Ok(Some(at..at + excess as u64));
        }
        if std::mem::take(&mut self.resumed) && level < target - RESYNC {
            // Back after running dry with nothing behind it: silence in front puts it where its capture time belongs.
            let short = (target - level) as usize;
            for _ in 0..short {
                self.fifo.push_front(0.0);
            }
            self.filtered = target;
            self.integral = 0.0;
            return Ok(Some(at..at + short as u64));
        }
        let e = self.filtered - target;
        self.integral = (self.integral + e * dt).clamp(-MAX_CORRECTION / KI, MAX_CORRECTION / KI);
        self.correction = (-(KP * e + KI * self.integral)).clamp(-MAX_CORRECTION, MAX_CORRECTION);
        self.drift.set_resample_ratio_relative(1.0 + self.correction, true)?;
        Ok(None)
    }
}

pub struct Mixer {
    lanes: Vec<Option<Lane>>,
    emitted: u64,
    /// Off only in the test that shows the steering is what holds the sources together.
    steering: bool,
}

impl Mixer {
    pub fn new(lanes: usize) -> Self {
        Self { lanes: (0..lanes).map(|_| None).collect(), emitted: 0, steering: true }
    }

    /// A source joins. `age` is how long ago its oldest waiting sample was captured (its device latency, the
    /// time since its last callback and the audio waiting in its ring), and `due` is the output position the host
    /// clock has reached now, which runs ahead of what the last pull took: that sample lands where a sample
    /// captured then belongs, DELAY behind the host clock. Returns that position.
    pub fn join(&mut self, i: usize, cfg: LaneConfig, age: f64, due: u64) -> Result<u64> {
        // A source still playing out what it delivered before it left (a moment ago: a rate change) keeps that audio in
        // front of its new audio, so none of it goes missing without a gap.
        let carried: VecDeque<f32> = self.lanes[i].take().filter(|l| l.draining).map(|l| l.fifo).unwrap_or_default();
        let start = self.emitted + carried.len() as u64;
        let wanted = due as f64 + DELAY as f64 - age * RATE;
        let origin = wanted.round().max(start as f64) as u64;
        // Audio too old to be placed after what is already out or queued (a ring that filled while the recording waited
        // to begin) is dropped before the recording, not played late for good.
        let skip = ((start as f64 - wanted).max(0.0) / RATE * cfg.rate as f64).round() as u64;
        let mut lane = Lane::new(cfg, origin, (origin - start) as usize, skip)?;
        for s in carried.into_iter().rev() {
            lane.fifo.push_front(s);
        }
        self.lanes[i] = Some(lane);
        Ok(origin)
    }

    pub fn push(&mut self, i: usize, interleaved: &[f32]) -> Result<()> {
        match self.lanes[i].as_mut().filter(|l| !l.draining) {
            Some(l) => l.push(interleaved),
            None => Ok(()),
        }
    }

    /// Input frames lost to overflow: silence stands in for them, so later audio keeps its place. Returns where
    /// they land.
    pub fn push_silence(&mut self, i: usize, frames: u64) -> Result<Range<u64>> {
        let Some(l) = self.lanes[i].as_mut().filter(|l| !l.draining) else { return Ok(0..0) };
        let lost = l.position(l.in_frames)..l.position(l.in_frames + frames);
        let mut left = frames;
        while left > 0 {
            let n = left.min(SILENCE_BLOCK);
            l.push(&vec![0.0; n as usize * l.channels])?;
            left -= n;
        }
        Ok(lost)
    }

    /// The source has gone: what it delivered still plays out. Returns the output position where its audio ends.
    pub fn leave(&mut self, i: usize) -> Result<u64> {
        let Some(l) = self.lanes[i].as_mut() else { return Ok(self.emitted) };
        l.finish()?;
        Ok(self.emitted + l.fifo.len() as u64)
    }

    pub fn is_joined(&self, i: usize) -> bool {
        self.lanes[i].as_ref().is_some_and(|l| !l.draining)
    }

    /// Whether any source is in the mix, joined or still playing out.
    pub fn any(&self) -> bool {
        self.lanes.iter().any(Option::is_some)
    }

    /// Takes the samples due up to `due` (output samples since the mix began) from every source, sums them with
    /// their gains and limits only the sum; then steers every source.
    pub fn pull(&mut self, due: u64) -> Pulled {
        let n = due.saturating_sub(self.emitted) as usize;
        let mut samples = vec![0.0f32; n];
        let mut gaps = Vec::new();
        let at = self.emitted;
        for (i, l) in self.lanes.iter_mut().enumerate() {
            if let Some(g) = l.as_mut().and_then(|l| l.take(&mut samples, at)) {
                gaps.push((i, g));
            }
        }
        for s in &mut samples {
            *s = s.clamp(-1.0, 1.0);
        }
        self.emitted += n as u64;
        let dt = n as f64 / RATE;
        for (i, slot) in self.lanes.iter_mut().enumerate() {
            let Some(l) = slot.as_mut() else { continue };
            if self.steering {
                if let Ok(Some(g)) = l.steer(dt, self.emitted) {
                    gaps.push((i, g));
                }
            }
            if l.draining && l.fifo.is_empty() {
                *slot = None;
            }
        }
        Pulled { samples, gaps }
    }

    /// Everything still waiting, for the end of a recording. A source that ran dry and has nothing more to play ends the
    /// recording without its audio from where it ran dry: that stretch is a gap too.
    pub fn flush(&mut self) -> Pulled {
        let longest = self.lanes.iter().flatten().map(|l| l.fifo.len()).max().unwrap_or(0) as u64;
        let still_dry: Vec<(usize, u64)> = self.lanes.iter().enumerate().filter_map(|(i, l)| l.as_ref().filter(|l| l.fifo.is_empty()).and_then(|l| l.dry_since).map(|d| (i, d))).collect();
        let steering = std::mem::replace(&mut self.steering, false);
        for l in self.lanes.iter_mut().flatten() {
            l.draining = true; // nothing more comes: running out from here is not a gap
        }
        let mut out = self.pull(self.emitted + longest);
        self.steering = steering;
        out.gaps.extend(still_dry.into_iter().map(|(i, from)| (i, from..self.emitted)));
        out
    }

    pub fn emitted(&self) -> u64 {
        self.emitted
    }

    pub fn stats(&self, i: usize) -> Option<LaneStats> {
        self.lanes[i].as_ref().map(|l| LaneStats { level: l.fifo.len(), filtered: l.filtered, target: l.target, correction_ppm: l.correction * 1e6, underrun: l.underrun })
    }

    pub fn level(&mut self, i: usize) -> Option<f32> {
        let l = self.lanes[i].as_mut().filter(|l| l.n > 0)?;
        let rms = (l.sq / l.n as f64).sqrt() as f32;
        (l.sq, l.n) = (0.0, 0);
        Some(rms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A device on its own clock: `ppm` fast or slow against the host clock, started at host time `start`,
    /// delivering whole blocks `latency` seconds after capture, with a unit impulse at each click (host
    /// seconds). A stall from `a` to `b` delivers nothing; with `backlog` the device kept capturing and hands
    /// the stretch over at `b`, without it what fell in the stall was never captured.
    struct Device { rate: u32, ppm: f64, start: f64, block: u64, latency: f64, sent: u64, clicks: Vec<f64>, next_click: usize, stall: Option<(f64, f64, bool)> }

    impl Device {
        fn new(rate: u32, ppm: f64, start: f64, block: u64, latency: f64, clicks: Vec<f64>) -> Self {
            Self { rate, ppm, start, block, latency, sent: 0, clicks, next_click: 0, stall: None }
        }
        fn true_rate(&self) -> f64 {
            self.rate as f64 * (1.0 + self.ppm * 1e-6)
        }
        fn delivered_by(&self, t: f64) -> u64 {
            let captured = ((t - self.latency - self.start).max(0.0) * self.true_rate()) as u64;
            captured / self.block * self.block
        }
        /// Seconds since the oldest frame not yet taken was captured.
        fn age(&self, t: f64) -> f64 {
            t - (self.start + self.sent as f64 / self.true_rate())
        }
        fn take(&mut self, t: f64) -> Vec<f32> {
            if let Some((a, b, backlog)) = self.stall {
                if t >= a && t < b {
                    return Vec::new();
                }
                if t >= b && !backlog && self.sent < self.delivered_by(b) - self.block {
                    self.sent = self.delivered_by(b); // never captured
                    while self.clicks.get(self.next_click).is_some_and(|&c| ((c - self.start) * self.true_rate()) as u64 <= self.sent) {
                        self.next_click += 1;
                    }
                }
            }
            let to = self.delivered_by(t);
            let mut v = vec![0.0f32; (to - self.sent) as usize];
            while let Some(&c) = self.clicks.get(self.next_click) {
                let n = ((c - self.start) * self.true_rate()).round() as u64;
                if n >= to {
                    break;
                }
                if n >= self.sent {
                    v[(n - self.sent) as usize] = 1.0;
                }
                self.next_click += 1;
            }
            self.sent = to;
            v
        }
    }

    /// Output positions of the impulses: local maxima above 0.1.
    #[derive(Default)]
    struct Peaks { pos: u64, prev: f32, rising: bool, found: Vec<u64> }

    impl Peaks {
        fn scan(&mut self, s: &[f32]) {
            for &x in s {
                if x > 0.1 && x >= self.prev {
                    self.rising = true;
                } else if self.rising && x < self.prev {
                    self.found.push(self.pos - 1);
                    self.rising = false;
                }
                self.prev = x;
                self.pos += 1;
            }
        }
    }

    struct Run { peaks: Vec<u64>, t0: f64, underrun: [u64; 2], gaps: Vec<(usize, Range<u64>)>, emitted: u64, due: u64 }

    /// The host clock ticks every 15–25 ms; at each tick every device hands over what it has delivered and
    /// the mixer takes what is due.
    fn simulate(devices: &mut [Device; 2], secs: f64, steering: bool) -> Run {
        let mut m = Mixer::new(2);
        m.steering = steering;
        let (mut seed, mut t, mut t0, mut due) = (7u32, 0.0f64, None::<f64>, 0u64);
        let (mut peaks, mut gaps) = (Peaks::default(), Vec::new());
        while t < secs {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            t += 0.015 + ((seed >> 16) % 10) as f64 / 1000.0;
            for (i, d) in devices.iter_mut().enumerate() {
                let age = d.age(t);
                let data = d.take(t);
                if data.is_empty() {
                    continue;
                }
                if !m.is_joined(i) {
                    let start = *t0.get_or_insert(t);
                    m.join(i, LaneConfig { rate: d.rate, channels: 1, gain: 1.0 }, age, ((t - start) * RATE) as u64).unwrap();
                }
                m.push(i, &data).unwrap();
            }
            if let Some(t0) = t0 {
                due = ((t - t0) * RATE) as u64;
                let p = m.pull(due);
                gaps.extend(p.gaps);
                peaks.scan(&p.samples);
            }
        }
        Run { peaks: peaks.found, t0: t0.unwrap(), underrun: [0, 1].map(|i| m.stats(i).map_or(0, |s| s.underrun)), gaps, emitted: m.emitted(), due }
    }

    fn clicks(first: f64, until: f64) -> Vec<f64> {
        (0..).map(|k| first + 10.0 * k as f64).take_while(|&h| h < until).collect()
    }

    /// Each click's error in output samples: where it landed, less where its capture time belongs (DELAY
    /// behind the host clock). Clicks of the two devices are 5 s apart, so a window of 2 s tells them apart.
    fn errors(run: &Run, clicks: &[f64]) -> Vec<(f64, f64)> {
        clicks
            .iter()
            .filter_map(|&h| {
                let want = (h - run.t0) * RATE + DELAY as f64;
                run.peaks.iter().map(|&p| p as f64 - want).find(|e| e.abs() < 2.0 * RATE).map(|e| (h, e))
            })
            .collect()
    }

    fn worst_after(e: &[(f64, f64)], after: f64) -> f64 {
        e.iter().filter(|(h, _)| *h > after).map(|(_, x)| x.abs()).fold(0.0, f64::max)
    }

    /// The gate (spec §4.2, §11): two hours, 48 kHz at +100 ppm beside 44.1 kHz at −100 ppm, with different
    /// latencies and start times. Without steering they part by 0.72 s.
    #[test]
    fn two_sources_on_drifting_clocks_stay_aligned_for_two_hours() {
        const SECS: f64 = 7_200.0;
        let (a, b) = (clicks(1.0, SECS - 2.0), clicks(6.0, SECS - 2.0));
        let mut devices = [Device::new(48_000, 100.0, 0.0, 512, 0.005, a.clone()), Device::new(44_100, -100.0, 0.007, 441, 0.012, b.clone())];
        let run = simulate(&mut devices, SECS, true);
        assert_eq!(run.emitted, run.due, "the mix emits exactly what the host clock says is due");
        assert_eq!(run.underrun, [0, 0], "no source ran dry");
        assert!(run.gaps.is_empty(), "{:?}", &run.gaps[..run.gaps.len().min(5)]);
        for (name, cl) in [("48 kHz, +100 ppm", &a), ("44.1 kHz, −100 ppm", &b)] {
            let e = errors(&run, cl);
            assert_eq!(e.len(), cl.len(), "{name}: every click found");
            let worst = worst_after(&e, 60.0);
            println!("{name}: first click {:+.0} samples, worst after 60 s {worst:.0} samples ({:.1} ms)", e[0].1, worst / 16.0);
            assert!(e[0].1.abs() <= 16.0, "{name}: the first click lands where its capture time belongs: {:+.0}", e[0].1);
            assert!(worst <= 160.0, "{name}: {worst} samples off after 60 s");
        }
    }

    #[test]
    fn five_hundred_ppm_either_way_is_held_within_20_ms() {
        const SECS: f64 = 1_800.0;
        let (a, b) = (clicks(1.0, SECS - 2.0), clicks(6.0, SECS - 2.0));
        let mut devices = [Device::new(48_000, 500.0, 0.0, 480, 0.004, a.clone()), Device::new(48_000, -500.0, 0.003, 512, 0.010, b.clone())];
        let run = simulate(&mut devices, SECS, true);
        assert_eq!(run.underrun, [0, 0]);
        for cl in [&a, &b] {
            let worst = worst_after(&errors(&run, cl), 60.0);
            println!("±500 ppm: worst after 60 s {worst:.0} samples");
            assert!(worst <= 320.0, "{worst}");
        }
    }

    /// The negative control: the same drift without steering parts the sources, so the steering is what holds them.
    #[test]
    fn without_steering_the_same_drift_parts_the_sources() {
        const SECS: f64 = 1_800.0;
        let (a, b) = (clicks(1.0, SECS - 2.0), clicks(6.0, SECS - 2.0));
        let mut devices = [Device::new(48_000, 500.0, 0.0, 480, 0.004, a.clone()), Device::new(48_000, -500.0, 0.003, 512, 0.010, b.clone())];
        let run = simulate(&mut devices, SECS, false);
        let fast = worst_after(&errors(&run, &a), 60.0);
        println!("unsteered: the fast source {fast:.0} samples late, the slow one ran dry for {} samples", run.underrun[1]);
        assert!(fast > 8_000.0, "{fast}");
        assert!(run.underrun[1] > 0);
    }

    /// Review Focus 1.
    #[test]
    fn a_source_that_stalls_and_then_delivers_its_backlog_joins_again_where_it_belongs() {
        const SECS: f64 = 120.0;
        let (a, b) = (clicks(1.0, SECS - 2.0), clicks(6.0, SECS - 2.0));
        let mut stalled = Device::new(48_000, 0.0, 0.0, 480, 0.01, b.clone());
        stalled.stall = Some((30.0, 32.5, true));
        let mut devices = [Device::new(48_000, 0.0, 0.0, 512, 0.005, a.clone()), stalled];
        let run = simulate(&mut devices, SECS, true);
        let stall_at = ((30.0 - run.t0) * RATE) as u64;
        assert!(run.gaps.iter().any(|(i, g)| *i == 1 && g.start >= stall_at && g.start <= stall_at + 2 * DELAY), "the stall is a gap for that source: {:?}", run.gaps);
        let after: Vec<f64> = errors(&run, &b).into_iter().filter(|(h, _)| *h > 34.0).map(|(_, x)| x.abs()).collect();
        assert!(!after.is_empty() && after.iter().all(|&x| x <= 160.0), "{after:?}");
        assert!(errors(&run, &a).iter().all(|(_, x)| x.abs() <= 160.0), "the other source never moved");
    }

    #[test]
    fn a_source_that_stops_capturing_for_a_while_joins_again_where_it_belongs() {
        const SECS: f64 = 120.0;
        let (a, b) = (clicks(1.0, SECS - 2.0), clicks(6.0, SECS - 2.0));
        let mut stalled = Device::new(48_000, 0.0, 0.0, 480, 0.01, b.clone());
        stalled.stall = Some((30.0, 32.5, false));
        let mut devices = [Device::new(48_000, 0.0, 0.0, 512, 0.005, a.clone()), stalled];
        let run = simulate(&mut devices, SECS, true);
        assert!(run.gaps.iter().any(|(i, _)| *i == 1));
        let after: Vec<f64> = errors(&run, &b).into_iter().filter(|(h, _)| *h > 34.0).map(|(_, x)| x.abs()).collect();
        assert!(!after.is_empty() && after.iter().all(|&x| x <= 160.0), "{after:?}");
    }

    #[test]
    fn each_source_s_gain_applies_before_the_sum_and_only_the_sum_is_clamped() {
        let mut m = Mixer::new(2);
        let age = DELAY as f64 / RATE; // nothing in front of either
        m.join(0, LaneConfig { rate: 16_000, channels: 1, gain: 0.5 }, age, m.emitted()).unwrap();
        m.join(1, LaneConfig { rate: 16_000, channels: 2, gain: 1.0 }, age, m.emitted()).unwrap();
        m.push(0, &vec![0.8; 16_000]).unwrap();
        m.push(1, &vec![0.3; 32_000]).unwrap(); // 16,000 stereo frames of 0.3
        let out = m.pull(8_000).samples;
        assert!((out[4_000] - 0.7).abs() < 1e-3, "0.5 × 0.8 + 0.3: {}", out[4_000]);
        let mut loud = Mixer::new(2);
        loud.join(0, LaneConfig { rate: 16_000, channels: 1, gain: 1.0 }, age, loud.emitted()).unwrap();
        loud.join(1, LaneConfig { rate: 16_000, channels: 1, gain: 1.0 }, age, loud.emitted()).unwrap();
        loud.push(0, &vec![0.8; 16_000]).unwrap();
        loud.push(1, &vec![0.6; 16_000]).unwrap();
        let out = loud.pull(8_000).samples;
        assert_eq!(out[4_000], 1.0, "only the sum is limited");
        assert_eq!(loud.level(0).map(|l| (l * 100.0).round()), Some(80.0), "a source's level is its own, before the gain");
    }

    #[test]
    fn input_lost_to_overflow_is_silence_where_it_would_have_played() {
        let mut m = Mixer::new(1);
        let at = m.join(0, LaneConfig { rate: 48_000, channels: 1, gain: 1.0 }, DELAY as f64 / RATE, m.emitted()).unwrap();
        assert_eq!(at, 0);
        m.push(0, &vec![0.5; 48_000]).unwrap(); // 1 s
        let lost = m.push_silence(0, 24_000).unwrap(); // 0.5 s lost
        m.push(0, &vec![0.5; 48_000]).unwrap();
        assert!((lost.start as i64 - 16_000).abs() <= 16 && (lost.end as i64 - 24_000).abs() <= 16, "{lost:?}");
        let out = m.pull(36_000).samples;
        assert!(out[20_000].abs() < 0.01 && out[8_000] > 0.45 && out[32_000] > 0.45);
    }

    /// Final review, M2: a source that leaves and joins again at once (a rate change) keeps what it had delivered in front
    /// of its new audio, so none of it goes missing without a gap.
    /// Final review, M4: a source that stops delivering without going away, and is still silent when the recording ends,
    /// has that stretch marked as a gap.
    #[test]
    fn a_source_still_dry_when_the_recording_ends_is_a_gap() {
        let mut m = Mixer::new(2);
        let age = DELAY as f64 / RATE;
        m.join(0, LaneConfig { rate: 16_000, channels: 1, gain: 1.0 }, age, m.emitted()).unwrap();
        m.join(1, LaneConfig { rate: 16_000, channels: 1, gain: 1.0 }, age, m.emitted()).unwrap();
        m.push(0, &vec![0.25; 64_000]).unwrap();
        m.push(1, &vec![0.5; 16_000]).unwrap();
        assert!(m.pull(32_000).gaps.is_empty(), "the stretch is still open");
        let tail = m.flush();
        assert!(tail.gaps.iter().any(|(i, g)| *i == 1 && (g.start as i64 - 16_000).abs() <= 16 && g.end >= 32_000), "{:?}", tail.gaps);
        assert!(tail.samples.len() >= 30_000, "the other source's audio still flushes");
    }

    #[test]
    fn a_source_that_rejoins_at_once_keeps_what_it_delivered() {
        let mut m = Mixer::new(2);
        let age = DELAY as f64 / RATE;
        m.join(0, LaneConfig { rate: 16_000, channels: 1, gain: 1.0 }, age, m.emitted()).unwrap();
        m.join(1, LaneConfig { rate: 16_000, channels: 1, gain: 1.0 }, age, m.emitted()).unwrap();
        m.push(0, &vec![0.25; 64_000]).unwrap();
        m.push(1, &vec![0.5; 16_000]).unwrap();
        m.pull(8_000);
        let ends = m.leave(1).unwrap(); // about 8,000 samples still to play
        let back = m.join(1, LaneConfig { rate: 16_000, channels: 1, gain: 1.0 }, 0.0, m.emitted()).unwrap();
        assert!(back >= ends, "its new audio comes after what it had delivered: {back} before {ends}");
        m.push(1, &vec![0.125; 16_000]).unwrap();
        let out = m.pull(16_000).samples; // positions 8,000..16,000
        assert!((out[4_000] - 0.75).abs() < 0.01, "what it had delivered still plays: {}", out[4_000]);
    }

    #[test]
    fn a_source_that_leaves_plays_out_what_it_delivered_and_joins_again_behind_silence() {
        let mut m = Mixer::new(2);
        let age = DELAY as f64 / RATE;
        m.join(0, LaneConfig { rate: 16_000, channels: 1, gain: 1.0 }, age, m.emitted()).unwrap();
        m.join(1, LaneConfig { rate: 16_000, channels: 1, gain: 1.0 }, age, m.emitted()).unwrap();
        m.push(0, &vec![0.25; 64_000]).unwrap();
        m.push(1, &vec![0.5; 16_000]).unwrap();
        m.pull(8_000);
        let ends = m.leave(1).unwrap();
        assert!((ends as i64 - 16_000).abs() <= 16, "its audio ends where its last sample lands: {ends}");
        let out = m.pull(24_000).samples; // positions 8,000..24,000
        assert!((out[4_000] - 0.75).abs() < 0.01 && (out[12_000] - 0.25).abs() < 0.01, "then only the other");
        assert!(!m.is_joined(1) && m.any());
        let back = m.join(1, LaneConfig { rate: 16_000, channels: 1, gain: 1.0 }, 0.0, m.emitted()).unwrap();
        assert_eq!(back, 24_000 + DELAY, "a sample captured now lands DELAY behind the host clock");
        m.push(1, &vec![0.5; 16_000]).unwrap();
        let out = m.pull(40_000).samples; // 24,000..40,000
        assert!((out[1_000] - 0.25).abs() < 0.01 && (out[(DELAY as usize) + 1_000] - 0.75).abs() < 0.01);
    }
}
