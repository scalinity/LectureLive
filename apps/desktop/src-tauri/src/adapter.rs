//! The desktop adapter (spec §3.6): maps a running lecture's events to the frontend's three streams,
//! stamps each message with the session and one rising sequence, and mirrors what a reload needs.
use std::collections::VecDeque;
use std::path::Path;
use std::sync::Arc;

use chrono::Local;
use lecturelive_core::audio::level::{dbfs, SilenceWatch};
use lecturelive_core::capture::worker::CaptureState;
use lecturelive_core::session::coordinator::{Notification, SttStatus};
use lecturelive_core::session::files::LectureFiles;
use lecturelive_core::session::lecture::Event;
use lecturelive_core::session::notesfile::sha256_hex;
use lecturelive_core::session::segments::{Segment, SegmentSource};
use lecturelive_core::session::sidecar::{NotesState, Sidecar};
use lecturelive_core::session::spend::{money, Spend};
use serde::Serialize;
use serde_json::Value;

use crate::wire::{CaptureView, CaptureWord, Envelope, NoticeKind, NotesMsg, Notice, OpenView, Outcome, PreviewView, SegmentView, SessionState, SlideView, Status, StatusMsg, TranscriptMsg, WindowView};
pub use crate::wire::Phase;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    /// A Tauri event: whole statuses, notices, slides.
    Status,
    /// A Tauri channel: the open utterance and closed segments, in order.
    Transcript,
    /// A Tauri channel: preview deltas and the results that end them, in order.
    Notes,
}

/// Where messages go: Tauri in the app, a list in tests.
pub trait Sink: Send + Sync {
    fn send(&self, stream: Stream, msg: Value);
}

/// Notices a reload shows again.
const NOTICES: usize = 50;

/// What a reloaded frontend needs beyond the files: the live parts of the view.
#[derive(Debug, Clone, Default)]
pub struct Mirror {
    pub status: Status,
    pub open: Option<OpenView>,
    /// The last utterance id handed out.
    utterance: u64,
    pub preview: Option<PreviewView>,
    /// The notes operation the next deltas belong to; every result moves it on.
    pub op: u64,
    pub notices: VecDeque<Notice>,
}

pub struct Pump {
    session: String,
    seq: u64,
    sink: Arc<dyn Sink>,
    spend: Option<Spend>,
    silence: Option<SilenceWatch>,
    mirror: Mirror,
    /// The status as the frontend last received it.
    sent: Option<Status>,
}

fn plural(n: usize, word: &str) -> String {
    format!("{n} {word}{}", if n == 1 { "" } else { "s" })
}

fn stt_words(s: &SttStatus) -> (String, bool) {
    match s {
        SttStatus::Connected => ("transcribing".into(), true),
        SttStatus::Retrying { after, reason } => (format!("reconnecting in {} s ({reason})", after.as_secs()), false),
        SttStatus::Refused(m) => (format!("refused: {m}"), false),
        SttStatus::ServerError(m) => (format!("server: {m}"), false),
        SttStatus::Stopped(m) => (format!("stopped: {m}"), false),
    }
}

impl Pump {
    pub fn new(session: String, sink: Arc<dyn Sink>, spend: Option<Spend>, loopback: bool) -> Self {
        let mirror = Mirror { op: 1, status: Status { stt: "not started".into(), ..Status::default() }, ..Mirror::default() };
        Self { session, seq: 0, sink, spend, silence: loopback.then(|| SilenceWatch::new(-60.0, 10)), mirror, sent: None }
    }

    pub fn phase(&self) -> Phase {
        self.mirror.status.phase
    }

    #[cfg(test)]
    pub fn seq(&self) -> u64 {
        self.seq
    }

    #[cfg(test)]
    pub fn mirror(&self) -> &Mirror {
        &self.mirror
    }

    fn emit<T: Serialize>(&mut self, stream: Stream, msg: T) {
        self.seq += 1;
        let v = serde_json::to_value(Envelope { session: self.session.clone(), seq: self.seq, msg }).expect("wire types serialise");
        self.sink.send(stream, v);
    }

    /// Sends the status when it differs from what the frontend last received.
    fn flush_status(&mut self) {
        if let Some(spend) = &self.spend {
            self.mirror.status.spend_usd = spend.lecture_total();
        }
        if self.sent.as_ref() != Some(&self.mirror.status) {
            let s = self.mirror.status.clone();
            self.sent = Some(s.clone());
            self.emit(Stream::Status, StatusMsg::Status(s));
        }
    }

    pub fn set_status(&mut self, f: impl FnOnce(&mut Status)) {
        f(&mut self.mirror.status);
        self.flush_status();
    }

    pub fn notice(&mut self, kind: NoticeKind, label: &str, detail: &str) {
        let n = Notice { kind, label: label.into(), detail: detail.into(), at: Local::now().format("%H:%M:%S").to_string() };
        self.mirror.notices.push_back(n.clone());
        while self.mirror.notices.len() > NOTICES {
            self.mirror.notices.pop_front();
        }
        self.emit(Stream::Status, StatusMsg::Notice(n));
    }

    /// A notes result: the deltas of this op end here.
    fn end_op(&mut self) {
        self.mirror.preview = None;
        self.mirror.status.busy = None;
        self.mirror.op += 1;
    }

    fn slide_path(&self, file: &str) -> String {
        self.mirror.status.folder.as_ref().map_or_else(|| file.to_string(), |f| Path::new(&f.dir).join(file).to_string_lossy().into_owned())
    }

    pub fn apply(&mut self, e: Event) {
        match e {
            Event::Session(n) => self.session_note(n),
            Event::Busy(m) => self.mirror.status.busy = Some(m),
            Event::Preview(d) => {
                let op = self.mirror.op;
                self.mirror.preview.get_or_insert_with(|| PreviewView { op, text: String::new() }).text.push_str(&d);
                self.emit(Stream::Notes, NotesMsg::Delta { op, text: d });
            }
            Event::Committed { words, slides, block, usd, confirmed, missing, revision, .. } => {
                let op = self.mirror.op;
                self.end_op();
                self.emit(Stream::Notes, NotesMsg::Committed { op, revision, block });
                let mut detail = format!("{} and {} folded in, {}", plural(words, "word"), plural(slides, "slide"), money(usd));
                if missing > 0 {
                    detail += &format!(" ({} not placed by the model, listed at the end)", plural(missing, "slide"));
                }
                if !confirmed {
                    detail += " (transcription still catching up; the rest goes into the next snapshot)";
                }
                self.notice(NoticeKind::Notes, "Snapshot taken", &detail);
            }
            Event::NothingNew => self.ended(Outcome::NothingNew, NoticeKind::Notes, "Snapshot", "nothing new since the last one".into()),
            Event::SnapshotFailed(m) => self.ended(Outcome::Failed, NoticeKind::Warn, "Snapshot failed", m),
            Event::Cancelled(what) => self.ended(Outcome::Cancelled, NoticeKind::Warn, "Cancelled", format!("{what}; nothing was written, everything is kept for the next snapshot")),
            Event::Polished { backup, usd, revision } => {
                self.mirror.status.busy = None;
                self.emit(Stream::Notes, NotesMsg::Polished { revision });
                let name = backup.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                self.notice(NoticeKind::Done, "Polished", &format!("previous version in .live_notes/{name}, {}", money(usd)));
            }
            Event::PolishStopped(m) => self.failed("Polish stopped", &m),
            Event::PolishFailed(m) => self.failed("Polish failed", &m),
            Event::Page { outcome, usd } => {
                self.mirror.status.busy = None;
                if let Some(f) = self.mirror.status.folder.as_mut() {
                    f.page = Some(outcome.path.to_string_lossy().into_owned());
                }
                let name = outcome.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let mut detail = format!("{name}, {} words of {} allowed, {}", outcome.words, outcome.budget, money(usd));
                if outcome.cached {
                    detail += " (notes unchanged since they were typeset: only the design reapplied, free)";
                }
                if !outcome.missing.is_empty() {
                    detail += &format!(" (missing: {})", outcome.missing.join(", "));
                }
                self.notice(NoticeKind::Page, "Study page", &detail);
            }
            Event::PageFailed(m) => self.failed("Study page failed", &m),
            Event::Slide { index, file, auto, uncertain, shown_at } => {
                let path = self.slide_path(&file);
                let name = Path::new(&file).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let how = match (auto, uncertain) {
                    (true, true) => ", taken while it was still changing: check it",
                    (true, false) => ", taken automatically",
                    _ => "",
                };
                self.emit(Stream::Status, StatusMsg::Slide(SlideView { index, file, path, at: shown_at.format("%H:%M:%S").to_string(), auto, uncertain }));
                self.notice(NoticeKind::Slide, &format!("Slide {index}"), &format!("{name}{how}, into the next snapshot"));
            }
            Event::Capture(s) => self.capture_state(s),
            Event::CaptureMoved { note, .. } => self.notice(NoticeKind::Slide, "Found again", &note),
            Event::Warning(m) => self.notice(NoticeKind::Warn, "Warning", &m),
        }
        self.flush_status();
    }

    /// The capture worker's state into the status; a changed state is also a notice.
    fn capture_state(&mut self, s: CaptureState) {
        let (label, detail) = s.words();
        let (state, window, why, candidates) = match &s {
            CaptureState::Unbound => (CaptureWord::Unbound, None, None, vec![]),
            CaptureState::Watching { window } => (CaptureWord::Watching, Some(window.clone()), None, vec![]),
            CaptureState::Paused { window, reason } => (CaptureWord::Paused, Some(window.clone()), Some(reason.clone()), vec![]),
            CaptureState::Asking { window, reason, candidates } => (CaptureWord::Asking, Some(window.clone()), Some(reason.clone()), candidates.iter().map(WindowView::from).collect()),
            CaptureState::Denied => (CaptureWord::Denied, None, Some(detail.clone()), vec![]),
            CaptureState::Failing { window, reason } => (CaptureWord::Failing, Some(window.clone()), Some(reason.clone()), vec![]),
        };
        let c = &mut self.mirror.status.capture;
        let captured = c.captured || state == CaptureWord::Watching; // watching follows a successful capture
        let view = CaptureView { state, window, detail: why, candidates, captured };
        if *c != view {
            *c = view;
            self.notice(if state == CaptureWord::Watching { NoticeKind::Slide } else { NoticeKind::Warn }, label, &detail);
        }
    }

    fn ended(&mut self, outcome: Outcome, kind: NoticeKind, label: &str, message: String) {
        let op = self.mirror.op;
        self.end_op();
        self.emit(Stream::Notes, NotesMsg::Ended { op, outcome, message: message.clone() });
        self.notice(kind, label, &message);
    }

    fn failed(&mut self, label: &str, m: &str) {
        self.mirror.status.busy = None;
        self.notice(NoticeKind::Warn, label, m);
    }

    fn session_note(&mut self, n: Notification) {
        match n {
            Notification::Open { stable, tentative } => {
                let utterance = match &self.mirror.open {
                    Some(o) => o.utterance,
                    None => {
                        self.mirror.utterance += 1;
                        self.mirror.utterance
                    }
                };
                self.mirror.open = Some(OpenView { utterance, stable: stable.clone(), tentative: tentative.clone() });
                self.emit(Stream::Transcript, TranscriptMsg::Open { utterance, stable, tentative });
            }
            Notification::Segment(s) if s.source == SegmentSource::Live => {
                let utterance = match self.mirror.open.take() {
                    Some(o) => o.utterance,
                    None => {
                        self.mirror.utterance += 1;
                        self.mirror.utterance
                    }
                };
                self.emit(Stream::Transcript, TranscriptMsg::Closed { utterance, segment: SegmentView::from(&s) });
            }
            Notification::Segment(s) => self.emit(Stream::Transcript, TranscriptMsg::Segment { segment: SegmentView::from(&s) }),
            Notification::Level(l) => {
                self.mirror.status.level_dbfs = Some(dbfs(l).round());
                if let Some(w) = self.silence.as_mut() {
                    if w.observe(l) {
                        self.mirror.status.silence = true;
                        self.notice(NoticeKind::Warn, "No signal", "10 s of silence on BlackHole: is Zoom's Speaker \"LectureLive Loopback\"?");
                    } else if dbfs(l) >= -60.0 {
                        self.mirror.status.silence = false;
                    }
                }
            }
            Notification::Stt(s) => {
                let (words, ok) = stt_words(&s);
                self.mirror.status.stt = words;
                self.mirror.status.stt_ok = ok;
            }
            Notification::Gap(g) => {
                if g.kind.is_transcript() && !g.resolved {
                    self.mirror.status.gaps += 1;
                }
                self.notice(NoticeKind::Warn, "Gap", &format!("{:?} from {:.1} s of recording {}", g.kind, g.start_sample as f64 / 16_000.0, g.recording_id));
            }
            Notification::Recovered(g) => {
                self.mirror.status.gaps = self.mirror.status.gaps.saturating_sub(1);
                self.notice(NoticeKind::Done, "Recovered", &format!("the transcript of {:.1}–{:.1} s of recording {}", g.start_sample as f64 / 16_000.0, g.end_sample.map_or(0.0, |e| e as f64 / 16_000.0), g.recording_id));
            }
            Notification::DeviceGone { uid } => self.notice(NoticeKind::Warn, "Input gone", &format!("{uid}; waiting for it to return (no other input is used)")),
            Notification::DeviceBack { uid } => self.notice(NoticeKind::Done, "Input back", &format!("{uid}; recording continues in a new file")),
            Notification::Failed(m) => self.notice(NoticeKind::Warn, "Session failed", &m),
            Notification::RecoveryFailed(m) => self.notice(NoticeKind::Warn, "Recovery", &m),
            Notification::SpendFailed(m) => self.notice(NoticeKind::Warn, "Spend", &m),
            Notification::SourceEnded => self.mirror.status.phase = Phase::Stopping,
            Notification::Recording { .. } => {}
        }
    }

    /// The whole view at this pump's sequence: the caller holds the pump while it reads the files, so
    /// nothing is sent between the reads and the watermark.
    pub fn state(&self, sc: Option<&Sidecar>, segments: &[Segment], document: String, files: Option<&LectureFiles>) -> SessionState {
        let (revision, pending_segments, pending_slides, slides) = match sc {
            Some(sc) => (
                sc.notes.revision,
                (segments.len() as u64).saturating_sub(sc.notes.segment_cursor),
                sc.slides.iter().filter(|s| s.index > sc.notes.slide_index).count(),
                sc.slides.iter().map(|s| SlideView { index: s.index, file: s.file.clone(), path: files.map_or_else(|| s.file.clone(), |f| f.dir.join(&s.file).to_string_lossy().into_owned()), at: s.shown_at.format("%H:%M:%S").to_string(), auto: s.auto, uncertain: s.uncertain }).collect(),
            ),
            None => (0, 0, 0, Vec::new()),
        };
        SessionState {
            session: self.session.clone(),
            seq: self.seq,
            status: self.mirror.status.clone(),
            notices: self.mirror.notices.iter().cloned().collect(),
            segments: segments.iter().map(SegmentView::from).collect(),
            open: self.mirror.open.clone(),
            revision,
            document,
            preview: self.mirror.preview.clone(),
            op: self.mirror.op,
            slides,
            pending_segments,
            pending_slides,
        }
    }
}

/// The notes file, only when it is the revision the sidecar records (spec §3.6): a commit between the
/// two reads would otherwise pair a document with the wrong revision.
pub fn read_document(files: &LectureFiles, notes: &NotesState) -> Option<String> {
    let bytes = std::fs::read(&files.notes).ok()?;
    (bytes.len() as u64 == notes.len && sha256_hex(&bytes) == notes.sha256).then(|| String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Local, TimeZone};
    use lecturelive_core::session::coordinator::{Notification, SttStatus};
    use lecturelive_core::session::segments::{Segment, SegmentSource};
    use std::sync::Mutex;

    #[derive(Default)]
    struct Recorded(Mutex<Vec<(Stream, Value)>>);
    impl Sink for Recorded {
        fn send(&self, stream: Stream, msg: Value) {
            self.0.lock().unwrap().push((stream, msg));
        }
    }
    impl Recorded {
        fn take(&self) -> Vec<(Stream, Value)> {
            std::mem::take(&mut *self.0.lock().unwrap())
        }
        fn on(&self, s: Stream) -> Vec<Value> {
            self.take().into_iter().filter(|(k, _)| *k == s).map(|(_, v)| v).collect()
        }
    }

    fn pump() -> (Pump, Arc<Recorded>) {
        let sink = Arc::new(Recorded::default());
        (Pump::new("s1".into(), sink.clone(), None, false), sink)
    }

    fn seg(id: u64, text: &str, source: SegmentSource) -> Segment {
        let at = Local.with_ymd_and_hms(2026, 9, 25, 10, 2, 3).unwrap();
        Segment { id, recording_id: uuid::Uuid::nil(), start_sample: 0, end_sample: 16_000, said_at: at, start: at, end: at, text: text.into(), words: vec![], source }
    }

    fn committed(revision: u64) -> Event {
        Event::Committed { words: 12, slides: 1, block: "\n<!-- 10:02:03 -->\n## A\n".into(), usd: 0.02, confirmed: true, removed: 0, missing: 0, revision }
    }

    #[test]
    fn every_message_carries_the_session_and_one_rising_sequence() {
        let (mut p, sink) = pump();
        p.apply(Event::Session(Notification::Open { stable: "the".into(), tentative: "rate".into() }));
        p.apply(Event::Preview("## A".into()));
        p.apply(Event::Session(Notification::Segment(seg(0, "the rate", SegmentSource::Live))));
        p.apply(committed(2));
        p.apply(Event::Slide { index: 1, file: "slides/slide_01_100203.png".into(), auto: false, uncertain: false, shown_at: Local::now() });
        let all = sink.take();
        assert!(all.len() >= 5);
        assert!(all.iter().all(|(_, m)| m["session"] == "s1"));
        let seqs: Vec<u64> = all.iter().map(|(_, m)| m["seq"].as_u64().unwrap()).collect();
        assert!(seqs.windows(2).all(|w| w[1] == w[0] + 1) && seqs[0] == 1, "{seqs:?}");
        assert_eq!(p.seq(), *seqs.last().unwrap());
    }

    #[test]
    fn a_live_segment_closes_the_open_utterance_and_a_recovered_one_does_not() {
        let (mut p, sink) = pump();
        p.apply(Event::Session(Notification::Open { stable: "the".into(), tentative: "rate".into() }));
        let open = sink.on(Stream::Transcript);
        assert_eq!((open[0]["type"].as_str(), open[0]["utterance"].as_u64(), open[0]["tentative"].as_str()), (Some("open"), Some(1), Some("rate")));
        p.apply(Event::Session(Notification::Segment(seg(0, "recovered words", SegmentSource::Recovered))));
        let r = sink.on(Stream::Transcript);
        assert_eq!((r[0]["type"].as_str(), r[0]["segment"]["recovered"].as_bool(), r[0]["segment"]["at"].as_str()), (Some("segment"), Some(true), Some("10:02:03")));
        assert!(p.mirror().open.is_some(), "recovery does not close what is being said");
        p.apply(Event::Session(Notification::Segment(seg(1, "the rate", SegmentSource::Live))));
        let c = sink.on(Stream::Transcript);
        assert_eq!((c[0]["type"].as_str(), c[0]["utterance"].as_u64(), c[0]["segment"]["id"].as_u64()), (Some("closed"), Some(1), Some(1)));
        assert!(p.mirror().open.is_none());
        p.apply(Event::Session(Notification::Open { stable: String::new(), tentative: "next".into() }));
        assert_eq!(sink.on(Stream::Transcript)[0]["utterance"].as_u64(), Some(2));
    }

    #[test]
    fn deltas_share_an_op_with_the_result_that_ends_them() {
        let (mut p, sink) = pump();
        p.apply(Event::Preview("## A".into()));
        p.apply(Event::Preview("\n- b".into()));
        p.apply(committed(4));
        p.apply(Event::Preview("## C".into()));
        p.apply(Event::SnapshotFailed("stream broke".into()));
        p.apply(Event::NothingNew);
        p.apply(Event::Cancelled("the snapshot".into()));
        p.apply(Event::Polished { backup: "/l/.live_notes/b.md".into(), usd: 0.01, revision: 5 });
        let n = sink.on(Stream::Notes);
        let brief: Vec<(String, u64)> = n.iter().map(|m| (m["type"].as_str().unwrap().to_string(), m["op"].as_u64().unwrap_or(0))).collect();
        assert_eq!(brief, [("delta", 1), ("delta", 1), ("committed", 1), ("delta", 2), ("ended", 2), ("ended", 3), ("ended", 4), ("polished", 0)].map(|(t, o)| (t.to_string(), o)));
        assert_eq!(n[2]["revision"].as_u64(), Some(4));
        assert_eq!([n[4]["outcome"].as_str(), n[5]["outcome"].as_str(), n[6]["outcome"].as_str()], [Some("failed"), Some("nothing_new"), Some("cancelled")]);
        assert_eq!(n[7]["revision"].as_u64(), Some(5));
    }

    #[test]
    fn the_mirror_holds_what_a_reload_needs() {
        let (mut p, _sink) = pump();
        p.apply(Event::Busy("snapshot, 12 words to grok-4.7".into()));
        p.apply(Event::Session(Notification::Open { stable: "the".into(), tentative: "rate".into() }));
        p.apply(Event::Preview("## A".into()));
        p.apply(Event::Preview("\n- b".into()));
        p.apply(Event::Warning("slide 2 is no longer on disk".into()));
        let s = p.state(None, &[seg(0, "said", SegmentSource::Live)], "# T\n".into(), None);
        assert_eq!(s.seq, p.seq());
        assert_eq!(s.preview.as_ref().map(|v| (v.op, v.text.as_str())), Some((1, "## A\n- b")));
        assert_eq!(s.open.as_ref().map(|o| (o.utterance, o.tentative.as_str())), Some((1, "rate")));
        assert_eq!(s.status.busy.as_deref(), Some("snapshot, 12 words to grok-4.7"));
        assert_eq!(s.notices.last().map(|n| n.detail.as_str()), Some("slide 2 is no longer on disk"));
        assert_eq!((s.segments.len(), s.document.as_str(), s.op), (1, "# T\n", 1));
        p.apply(committed(1));
        let s = p.state(None, &[], String::new(), None);
        assert!(s.preview.is_none() && s.status.busy.is_none(), "the block replaced the preview");
    }

    #[test]
    fn status_changes_are_sent_whole_and_stop_levels_are_phases() {
        let (mut p, sink) = pump();
        p.set_status(|s| s.phase = Phase::Running);
        p.apply(Event::Session(Notification::Stt(SttStatus::Retrying { after: std::time::Duration::from_secs(4), reason: "closed".into() })));
        let st = sink.on(Stream::Status);
        let last = st.iter().rev().find(|m| m["type"] == "status").unwrap();
        assert_eq!((last["phase"].as_str(), last["stt_ok"].as_bool()), (Some("running"), Some(false)));
        assert!(last["stt"].as_str().unwrap().contains("4 s"));
        p.apply(Event::Session(Notification::Level(0.5)));
        p.apply(Event::Session(Notification::Level(0.5)));
        assert_eq!(sink.on(Stream::Status).iter().filter(|m| m["type"] == "status").count(), 1, "an unchanged status is not sent again");
        p.set_status(|s| s.phase = Phase::Stopping);
        p.set_status(|s| s.phase = Phase::StoppingNow);
        let phases: Vec<String> = sink.on(Stream::Status).iter().map(|m| m["phase"].as_str().unwrap().to_string()).collect();
        assert_eq!(phases, ["stopping", "stopping_now"]);
    }

    #[test]
    fn ten_silent_seconds_on_loopback_raise_the_warning_once() {
        let sink = Arc::new(Recorded::default());
        let mut p = Pump::new("s1".into(), sink.clone(), None, true);
        for _ in 0..12 {
            p.apply(Event::Session(Notification::Level(0.0)));
        }
        let msgs = sink.take();
        assert_eq!(msgs.iter().filter(|(_, m)| m["type"] == "notice" && m["kind"] == "warn").count(), 1);
        assert_eq!(p.mirror().status.silence, true);
        p.apply(Event::Session(Notification::Level(0.3)));
        assert_eq!(p.mirror().status.silence, false);
    }

    #[test]
    fn a_document_is_read_only_at_its_sidecar_revision() {
        let dir = tempfile::tempdir().unwrap();
        let files = LectureFiles::standard(dir.path(), chrono::NaiveDate::from_ymd_opt(2026, 9, 25).unwrap());
        std::fs::write(&files.notes, "# T\n").unwrap();
        let mut notes = lecturelive_core::session::sidecar::NotesState::default();
        notes.len = 4;
        notes.sha256 = lecturelive_core::session::notesfile::sha256_hex(b"# T\n");
        assert_eq!(read_document(&files, &notes).as_deref(), Some("# T\n"));
        std::fs::write(&files.notes, "# T\n\n<!-- 10:00:00 -->\n## A\n").unwrap(); // a commit landed after the sidecar was read
        assert_eq!(read_document(&files, &notes), None);
    }

    #[test]
    fn capture_states_reach_the_status_with_a_notice_and_slides_carry_their_badges() {
        use lecturelive_core::capture::worker::CaptureState;
        let (mut p, sink) = pump();
        p.apply(Event::Capture(CaptureState::Watching { window: "Zoom Meeting".into() }));
        p.apply(Event::Capture(CaptureState::Watching { window: "Zoom Meeting".into() }));
        let at = Local.with_ymd_and_hms(2026, 9, 25, 10, 2, 51).unwrap();
        p.apply(Event::Slide { index: 1, file: "slides/slide_01_100251.png".into(), auto: true, uncertain: true, shown_at: at });
        p.apply(Event::Capture(CaptureState::Asking { window: "Zoom Meeting".into(), reason: "a new “Zoom Meeting” window opened".into(), candidates: vec![] }));
        let msgs = sink.on(Stream::Status);
        let notices: Vec<&str> = msgs.iter().filter(|m| m["type"] == "notice").map(|m| m["label"].as_str().unwrap()).collect();
        assert_eq!(notices, ["Watching", "Slide 1", "Asking"], "an unchanged state is not announced twice");
        let slide = msgs.iter().find(|m| m["type"] == "slide").unwrap();
        assert_eq!((slide["auto"].as_bool(), slide["uncertain"].as_bool(), slide["at"].as_str()), (Some(true), Some(true), Some("10:02:51")));
        let status = msgs.iter().rev().find(|m| m["type"] == "status").unwrap();
        assert_eq!((status["capture"]["state"].as_str(), status["capture"]["captured"].as_bool()), (Some("asking"), Some(true)));
    }

    #[test]
    fn a_slide_found_again_after_a_resize_is_said() {
        use lecturelive_core::capture::detect::Region;
        use lecturelive_core::capture::select::{Descriptor, Selection};
        let (mut p, sink) = pump();
        let d = Descriptor { bundle_id: None, app: "zoom.us".into(), title: "Zoom Meeting".into(), width: 1920, height: 1200 };
        let selection = Selection { descriptor: d, region: Region::WHOLE, leave_out: vec![], sizes: vec![] };
        p.apply(Event::CaptureMoved { selection, note: "Zoom Meeting is 1920 × 1200 now; the slide was found again there".into() });
        let n: Vec<Value> = sink.on(Stream::Status).into_iter().filter(|m| m["type"] == "notice").collect();
        assert_eq!((n[0]["label"].as_str(), n[0]["kind"].as_str()), (Some("Found again"), Some("slide")));
        assert!(n[0]["detail"].as_str().unwrap().contains("1920 × 1200"));
    }
}
