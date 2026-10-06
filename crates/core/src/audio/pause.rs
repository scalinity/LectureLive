//! Pausing a lecture's audio (spec §9.6): a source between the real one and the coordinator. While paused it
//! delivers no audio, so nothing is recorded and nothing is streamed to speech-to-text: a break costs no
//! transcription. The recording the pause interrupts ends as any recording does (the open utterance is flushed and
//! the connection closes), and resuming begins the next one, anchored where the real source's clock puts that
//! moment, so what lies between the two recordings is exactly the pause.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use chrono::{DateTime, Local};
use tokio::sync::mpsc::{self, Sender};
use uuid::Uuid;

use super::source::{Source, SourceEvent};
use crate::session::sidecar::{wall_time_at, Gap, GapKind};

/// The switch the lecture turns: the source reads it once per event.
#[derive(Clone, Default)]
pub struct PauseSwitch(Arc<AtomicBool>);

impl PauseSwitch {
    pub fn set(&self, paused: bool) {
        self.0.store(paused, Ordering::SeqCst);
    }

    pub fn is_paused(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// A source that can be paused: `inner` runs as it always does, on its own thread, and its events pass through the
/// gate on the way to the coordinator. The real device keeps running while paused, so the level still moves and
/// resuming needs no new stream.
pub struct Pausable {
    inner: Box<dyn Source>,
    switch: PauseSwitch,
}

impl Pausable {
    pub fn wrap(inner: Box<dyn Source>) -> (Box<dyn Source>, PauseSwitch) {
        let switch = PauseSwitch::default();
        (Box::new(Self { inner, switch: switch.clone() }), switch)
    }
}

impl Source for Pausable {
    fn run(self: Box<Self>, out: Sender<SourceEvent>, stop: Arc<AtomicBool>) {
        let Self { inner, switch } = *self;
        let (tx, mut rx) = mpsc::channel(256);
        let inner_stop = stop.clone();
        let thread = std::thread::spawn(move || inner.run(tx, inner_stop));
        let mut gate = Gate::default();
        'events: while let Some(ev) = rx.blocking_recv() {
            for e in gate.process(ev, switch.is_paused()) {
                if out.blocking_send(e).is_err() {
                    stop.store(true, Ordering::Relaxed); // the session has closed: the real source stops too
                    break 'events;
                }
            }
        }
        drop(rx); // a source blocked on a full channel is released
        let _ = thread.join();
    }
}

/// The real source's current recording, as it numbers it.
struct Src {
    id: Uuid,
    anchor: DateTime<Local>,
    uid: String,
    rate: u32,
    channels: u16,
}

/// The recording the coordinator has, and how it maps onto the source's: the recording after a pause is a new one
/// that starts `base` samples into the source's.
struct Live {
    id: Uuid,
    src_id: Uuid,
    base: u64,
    /// The end of the last frame passed on, in this recording's samples.
    end: u64,
    open: bool,
}

/// A gap the source opened that has not ended (an input of a mixed source away): carried into the next recording
/// when a pause comes between.
struct Away {
    src_id: Uuid,
    start: u64,
    kind: GapKind,
    /// The recording and sample the coordinator was told it began at.
    told: Option<(Uuid, u64)>,
}

#[derive(Default)]
struct Gate {
    src: Option<Src>,
    live: Option<Live>,
    away: Vec<Away>,
}

impl Gate {
    fn process(&mut self, ev: SourceEvent, paused: bool) -> Vec<SourceEvent> {
        let mut out = Vec::new();
        if paused {
            self.close(&mut out);
        }
        match ev {
            SourceEvent::Begin { recording_id, anchor, source_uid, input_rate, channels } => {
                self.away.retain(|a| a.src_id == recording_id);
                self.src = Some(Src { id: recording_id, anchor, uid: source_uid.clone(), rate: input_rate, channels });
                if paused {
                    self.live = None;
                } else {
                    self.live = Some(Live { id: recording_id, src_id: recording_id, base: 0, end: 0, open: true });
                    out.push(SourceEvent::Begin { recording_id, anchor, source_uid, input_rate, channels });
                }
            }
            SourceEvent::Frame(mut f) => {
                let Some(src) = self.src.as_ref().filter(|s| s.id == f.recording_id && !paused) else { return out };
                if !self.live.as_ref().is_some_and(|l| l.open) {
                    // The first frame after a pause: the next recording begins here, where the source's clock puts it.
                    let id = Uuid::new_v4();
                    out.push(SourceEvent::Begin { recording_id: id, anchor: wall_time_at(src.anchor, f.sample_offset), source_uid: src.uid.clone(), input_rate: src.rate, channels: src.channels });
                    for a in self.away.iter_mut().filter(|a| a.src_id == src.id && a.told.is_none()) {
                        out.push(SourceEvent::Gap(Gap::new(id, 0, None, a.kind)));
                        a.told = Some((id, 0));
                    }
                    self.live = Some(Live { id, src_id: src.id, base: f.sample_offset, end: 0, open: true });
                }
                let Some(l) = self.live.as_mut() else { return out };
                f.recording_id = l.id;
                f.sample_offset -= l.base;
                l.end = f.sample_offset + f.valid_samples as u64;
                out.push(SourceEvent::Frame(f));
            }
            SourceEvent::Gap(g) => {
                if paused {
                    if g.end_sample.is_none() {
                        self.away.push(Away { src_id: g.recording_id, start: g.start_sample, kind: g.kind, told: None });
                    }
                    return out;
                }
                let Some((id, base, open)) = self.live.as_ref().filter(|l| l.src_id == g.recording_id).map(|l| (l.id, l.base, l.open)) else {
                    out.push(SourceEvent::Gap(g)); // not a recording this gate knows: as the source said it
                    return out;
                };
                let start = g.start_sample.saturating_sub(base);
                let end = g.end_sample.map(|e| e.saturating_sub(base));
                if end.is_some_and(|e| e <= start) {
                    return out; // it lay wholly before this recording began
                }
                if g.end_sample.is_none() && open {
                    self.away.push(Away { src_id: g.recording_id, start: g.start_sample, kind: g.kind, told: Some((id, start)) });
                }
                out.push(SourceEvent::Gap(Gap { recording_id: id, start_sample: start, end_sample: end, ..g }));
            }
            SourceEvent::GapEnd { recording_id, start_sample, end_sample } => {
                let Some(i) = self.away.iter().position(|a| a.src_id == recording_id && a.start == start_sample) else { return out };
                let a = self.away.remove(i);
                let base = self.live.as_ref().filter(|l| l.src_id == recording_id).map_or(0, |l| l.base);
                if let Some((id, told_start)) = a.told {
                    out.push(SourceEvent::GapEnd { recording_id: id, start_sample: told_start, end_sample: end_sample.saturating_sub(base).max(told_start) });
                }
            }
            SourceEvent::End { recording_id, samples, stream_errors } => {
                if let Some(l) = self.live.as_mut().filter(|l| l.src_id == recording_id && l.open) {
                    l.open = false;
                    out.push(SourceEvent::End { recording_id: l.id, samples: samples.saturating_sub(l.base), stream_errors });
                }
            }
            other => out.push(other), // levels, a device going and coming back, a failure
        }
        out
    }

    /// The pause begins: the recording the coordinator has ends where the last frame passed on ended.
    fn close(&mut self, out: &mut Vec<SourceEvent>) {
        if let Some(l) = self.live.as_mut().filter(|l| l.open) {
            l.open = false;
            out.push(SourceEvent::End { recording_id: l.id, samples: l.end, stream_errors: 0 });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::frame::{Frame, FRAME_SAMPLES};
    use chrono::TimeZone;

    fn anchor() -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 10, 6, 12, 0, 0).unwrap()
    }

    fn begin(id: Uuid) -> SourceEvent {
        SourceEvent::Begin { recording_id: id, anchor: anchor(), source_uid: "mixed".into(), input_rate: 48_000, channels: 2 }
    }

    fn frame(id: Uuid, n: u64) -> SourceEvent {
        SourceEvent::Frame(Frame { recording_id: id, sample_offset: n * FRAME_SAMPLES as u64, valid_samples: FRAME_SAMPLES as u32, pcm16: [7; FRAME_SAMPLES] })
    }

    fn feed(g: &mut Gate, events: Vec<(SourceEvent, bool)>) -> Vec<SourceEvent> {
        events.into_iter().flat_map(|(e, paused)| g.process(e, paused)).collect()
    }

    fn offsets(out: &[SourceEvent]) -> Vec<(Uuid, u64)> {
        out.iter().filter_map(|e| if let SourceEvent::Frame(f) = e { Some((f.recording_id, f.sample_offset / FRAME_SAMPLES as u64)) } else { None }).collect()
    }

    #[test]
    fn unpaused_events_pass_through_unchanged() {
        let a = Uuid::new_v4();
        let mut g = Gate::default();
        let out = feed(&mut g, vec![(begin(a), false), (frame(a, 0), false), (SourceEvent::Level(0.5), false), (frame(a, 1), false), (SourceEvent::End { recording_id: a, samples: 3200, stream_errors: 1 }, false)]);
        assert_eq!(out.len(), 5);
        assert_eq!(offsets(&out), vec![(a, 0), (a, 1)]);
        assert!(matches!(&out[4], SourceEvent::End { recording_id, samples: 3200, stream_errors: 1 } if *recording_id == a));
    }

    #[test]
    fn a_pause_ends_the_recording_and_drops_audio_and_a_resume_begins_the_next_where_the_clock_says() {
        let a = Uuid::new_v4();
        let mut g = Gate::default();
        let out = feed(&mut g, vec![(begin(a), false), (frame(a, 0), false), (frame(a, 1), false), (frame(a, 2), true), (SourceEvent::Level(0.1), true), (frame(a, 3), true), (frame(a, 4), false), (frame(a, 5), false)]);
        let kinds: Vec<&str> = out.iter().map(|e| match e { SourceEvent::Begin { .. } => "begin", SourceEvent::Frame(_) => "frame", SourceEvent::End { .. } => "end", SourceEvent::Level(_) => "level", _ => "other" }).collect();
        assert_eq!(kinds, ["begin", "frame", "frame", "end", "level", "begin", "frame", "frame"], "{out:?}");
        let SourceEvent::End { recording_id, samples, .. } = &out[3] else { panic!() };
        assert_eq!((*recording_id, *samples), (a, 2 * FRAME_SAMPLES as u64), "the first recording ends after the last frame it was given");
        let SourceEvent::Begin { recording_id: b, anchor: at, source_uid, input_rate, channels } = &out[5] else { panic!() };
        assert_ne!(*b, a);
        assert_eq!(*at, wall_time_at(anchor(), 4 * FRAME_SAMPLES as u64), "anchored at the source's clock for the first frame after the pause");
        assert_eq!((source_uid.as_str(), *input_rate, *channels), ("mixed", 48_000, 2));
        assert_eq!(offsets(&out), vec![(a, 0), (a, 1), (*b, 0), (*b, 1)], "the next recording counts from zero");
    }

    #[test]
    fn a_pause_before_the_source_begins_holds_the_begin_until_the_resume() {
        let a = Uuid::new_v4();
        let mut g = Gate::default();
        let held = feed(&mut g, vec![(begin(a), true), (frame(a, 0), true)]);
        assert!(held.is_empty());
        let out = feed(&mut g, vec![(frame(a, 1), false)]);
        assert!(matches!(out[0], SourceEvent::Begin { .. }));
        assert_eq!(offsets(&out), vec![(match &out[0] { SourceEvent::Begin { recording_id, .. } => *recording_id, _ => unreachable!() }, 0)]);
    }

    #[test]
    fn the_sources_own_end_while_paused_is_dropped_and_its_next_begin_starts_a_recording_of_its_own() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let mut g = Gate::default();
        let out = feed(&mut g, vec![(begin(a), false), (frame(a, 0), false), (SourceEvent::End { recording_id: a, samples: 1600, stream_errors: 0 }, true), (begin(b), true), (frame(b, 0), true), (frame(b, 1), false)]);
        let ends = out.iter().filter(|e| matches!(e, SourceEvent::End { .. })).count();
        assert_eq!(ends, 1, "the pause ended the recording once; the source's own end adds nothing: {out:?}");
        let begins: Vec<Uuid> = out.iter().filter_map(|e| if let SourceEvent::Begin { recording_id, .. } = e { Some(*recording_id) } else { None }).collect();
        assert_eq!(begins.len(), 2);
        assert_eq!(offsets(&out), vec![(a, 0), (begins[1], 0)], "frame 1 of the source's second recording is the first of the resumed one");
    }

    #[test]
    fn a_stop_while_paused_adds_no_second_end() {
        let a = Uuid::new_v4();
        let mut g = Gate::default();
        let out = feed(&mut g, vec![(begin(a), false), (frame(a, 0), false), (frame(a, 1), true), (SourceEvent::End { recording_id: a, samples: 3200, stream_errors: 0 }, true)]);
        assert_eq!(out.iter().filter(|e| matches!(e, SourceEvent::End { .. })).count(), 1);
    }

    #[test]
    fn a_device_gone_after_the_end_keeps_its_terminal_gap_as_the_source_said() {
        let a = Uuid::new_v4();
        let gone = Gap { recording_id: a, start_sample: 3200, end_sample: None, kind: GapKind::DeviceGone, resolved: true };
        let mut g = Gate::default();
        let out = feed(&mut g, vec![(begin(a), false), (frame(a, 0), false), (frame(a, 1), false), (SourceEvent::End { recording_id: a, samples: 3200, stream_errors: 0 }, false), (SourceEvent::Gap(gone.clone()), false)]);
        assert!(matches!(out.last(), Some(SourceEvent::Gap(x)) if *x == gone));
    }

    #[test]
    fn a_gap_in_the_recording_after_a_pause_is_counted_from_the_new_recording() {
        let a = Uuid::new_v4();
        let mut g = Gate::default();
        // Paused after 2 frames; resumed at frame 10; an overflow gap over frames 12–13 and an input away from frame 14.
        let overflow = Gap { recording_id: a, start_sample: 12 * 1600, end_sample: Some(14 * 1600), kind: GapKind::CaptureOverflow, resolved: true };
        let away = Gap { recording_id: a, start_sample: 14 * 1600, end_sample: None, kind: GapKind::DeviceGone, resolved: true };
        let out = feed(&mut g, vec![
            (begin(a), false), (frame(a, 0), false), (frame(a, 1), false),
            (frame(a, 2), true),
            (frame(a, 10), false), (SourceEvent::Gap(overflow), false), (SourceEvent::Gap(away), false),
            (SourceEvent::GapEnd { recording_id: a, start_sample: 14 * 1600, end_sample: 16 * 1600 }, false),
        ]);
        let b = out.iter().find_map(|e| if let SourceEvent::Begin { recording_id, .. } = e { Some(*recording_id).filter(|r| *r != a) } else { None }).unwrap();
        let gaps: Vec<(&Gap, Option<u64>)> = out.iter().filter_map(|e| if let SourceEvent::Gap(g) = e { Some((g, None)) } else { None }).collect();
        assert_eq!(gaps[0].0.start_sample, 2 * 1600, "frame 12 is the 3rd frame of the new recording");
        assert_eq!(gaps[0].0.end_sample, Some(4 * 1600));
        assert_eq!(gaps[0].0.recording_id, b);
        assert_eq!((gaps[1].0.start_sample, gaps[1].0.end_sample), (4 * 1600, None));
        let end = out.iter().find_map(|e| if let SourceEvent::GapEnd { recording_id, start_sample, end_sample } = e { Some((*recording_id, *start_sample, *end_sample)) } else { None }).unwrap();
        assert_eq!(end, (b, 4 * 1600, 6 * 1600), "the end is in the new recording's samples, matching the start the coordinator was told");
    }

    #[test]
    fn an_input_still_away_at_the_resume_is_away_from_the_start_of_the_next_recording() {
        let a = Uuid::new_v4();
        let away = Gap { recording_id: a, start_sample: 3 * 1600, end_sample: None, kind: GapKind::DeviceGone, resolved: true };
        let mut g = Gate::default();
        let out = feed(&mut g, vec![
            (begin(a), false), (frame(a, 0), false), (frame(a, 1), false),
            (frame(a, 2), true), (SourceEvent::Gap(away), true), (frame(a, 3), true),
            (frame(a, 8), false),
            (SourceEvent::GapEnd { recording_id: a, start_sample: 3 * 1600, end_sample: 10 * 1600 }, false),
        ]);
        let b = out.iter().find_map(|e| if let SourceEvent::Begin { recording_id, .. } = e { Some(*recording_id).filter(|r| *r != a) } else { None }).unwrap();
        let opened: Vec<_> = out.iter().filter_map(|e| if let SourceEvent::Gap(g) = e { Some((g.recording_id, g.start_sample, g.end_sample)) } else { None }).collect();
        assert_eq!(opened, vec![(b, 0, None)]);
        let closed = out.iter().find_map(|e| if let SourceEvent::GapEnd { recording_id, start_sample, end_sample } = e { Some((*recording_id, *start_sample, *end_sample)) } else { None }).unwrap();
        assert_eq!(closed, (b, 0, 2 * 1600));
    }

    /// A device's lecture in three parts, each ended by a `Level` the gate always passes on: the test flips the
    /// switch only after seeing one, so everything before it has been through the gate and the order is exact.
    struct Scripted {
        id: Uuid,
        go: std::sync::mpsc::Receiver<()>,
    }

    impl Source for Scripted {
        fn run(self: Box<Self>, out: Sender<SourceEvent>, _stop: Arc<AtomicBool>) {
            let send = |e| out.blocking_send(e).unwrap();
            send(begin(self.id));
            for part in 0..3u64 {
                for n in part * 10..part * 10 + 10 {
                    send(frame(self.id, n));
                }
                if part < 2 {
                    send(SourceEvent::Level(part as f32 + 1.0));
                    self.go.recv().unwrap();
                }
            }
            send(SourceEvent::End { recording_id: self.id, samples: 30 * FRAME_SAMPLES as u64, stream_errors: 0 });
        }
    }

    #[test]
    fn the_wrapper_runs_a_real_source_through_the_gate() {
        let id = Uuid::new_v4();
        let (go_tx, go_rx) = std::sync::mpsc::channel();
        let switch = PauseSwitch::default();
        let source: Box<dyn Source> = Box::new(Pausable { inner: Box::new(Scripted { id, go: go_rx }), switch: switch.clone() });
        let (tx, mut rx) = mpsc::channel(512);
        let thread = std::thread::spawn(move || source.run(tx, Arc::new(AtomicBool::new(false))));
        let mut out = Vec::new();
        while let Some(e) = rx.blocking_recv() {
            if let SourceEvent::Level(l) = &e {
                switch.set(*l < 1.5); // after the first part: paused; after the second: running again
                go_tx.send(()).unwrap();
            }
            out.push(e);
        }
        thread.join().unwrap();
        let begins = out.iter().filter(|e| matches!(e, SourceEvent::Begin { .. })).count();
        let ends = out.iter().filter(|e| matches!(e, SourceEvent::End { .. })).count();
        assert_eq!((begins, ends), (2, 2), "{out:?}");
        let frames = out.iter().filter(|e| matches!(e, SourceEvent::Frame(_))).count();
        assert_eq!(frames, 20, "the ten frames sent while paused were dropped");
    }
}
