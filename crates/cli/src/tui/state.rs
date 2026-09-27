//! The session projection (M7 plan §F): the canonical files and the live events reconciled into one
//! view of the lecture — what it is, what has been said, what the notes hold, what needs the person.
//! The reactor alone owns it, it is never persisted, and it is reconstructible presentation state
//! only: the segment log, the sidecar, the notes and the slides stay the lecture's record, and when
//! provisional or live state disagrees with them, canonical wins (plan §C 1, 4, 5).
//!
//! Field groups (plan §F): the identity is hydrated config; the level, the silence, the input-gone
//! mark, STT and capture state are latest-value telemetry, where a lost sample is cosmetic (§C 6);
//! the segments, the notes and the slides reconcile by durable id; the preview is provisional; the
//! activity ring and the notice are display-only.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

use chrono::{DateTime, Local};
use lecturelive_core::audio::level::{dbfs, SilenceWatch};
use lecturelive_core::capture::window::WindowInfo;
use lecturelive_core::capture::worker::CaptureState;
use lecturelive_core::session::coordinator::{Notification, SttStatus};
use lecturelive_core::session::lecture::Event;
use lecturelive_core::session::segments::{Segment, SegmentSource};
use lecturelive_core::session::sidecar::{Gap, SlideEntry};
use lecturelive_core::session::spend::Paint;

use crate::plain::{self, Notice};
use crate::stop::Stage;

use super::hydrate::{Hydration, NotesSnapshot};
use super::markdown::{self, Chunk};

/// The shared wording, unstyled: the TUI renders its own styles, so no styled string may enter the
/// projection (plan §C 12).
const WORDS: Paint = Paint { color: false, truecolor: false };

/// What the lecture records from (plan §F): which silence to watch, and what a gone input means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SourceKind {
    /// One input: when it goes, the person may choose another (spec §4.1).
    Input,
    /// Zoom through BlackHole: ten silent seconds are a warning (spec §4.3).
    Loopback,
    /// Zoom and an input together: the level is the loopback's, and a gone input leaves Zoom going.
    Mixed,
    /// The scripted session of a debug build (plan §H): no signal to watch.
    Fixture,
}

/// What the TUI knows once the lecture is prepared (plan §F's identity fields): the lecture's and
/// the input's names, and the files the panes will name.
pub(crate) struct Identity {
    pub(crate) course: String,
    /// The lecture folder's display name.
    pub(crate) lecture: String,
    /// The input's display label.
    pub(crate) input: String,
    pub(crate) kind: SourceKind,
    pub(crate) notes_file: String,
    pub(crate) transcript_file: String,
}

/// One closed utterance of the transcript (plan §F): the log's id and time, the text, where it came
/// from. The words are dropped — no view needs them.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Line {
    pub(crate) id: u64,
    pub(crate) said_at: DateTime<Local>,
    pub(crate) text: String,
    pub(crate) source: SegmentSource,
}

/// The open utterance (latest-value): what is stable, and what may still change.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct OpenUtterance {
    pub(crate) stable: String,
    pub(crate) tentative: String,
}

/// The notes as the projection holds them (plan §F): the canonical revision and the document it
/// names, together, and the document parsed into chunks once as it changes; the provisional
/// preview beside them — never merged into them.
#[derive(Debug, Default)]
pub(crate) struct Notes {
    pub(crate) revision: u64,
    pub(crate) document: String,
    /// The document split at its markers and parsed (plan §H Notes): rebuilt by a hydration that
    /// moves the document, extended by a commit with its block alone, never by a draw.
    pub(crate) chunks: Vec<Chunk>,
    /// The snapshot being written: the model's deltas in order, never dropped, cleared in the same
    /// update that ends the work (`Committed`, `NothingNew`, `SnapshotFailed`, `Cancelled`,
    /// `Polished`).
    pub(crate) preview: Option<String>,
    /// Which preview this is: a new one begins at the first delta after one ended, so a display of
    /// an earlier preview can never be taken for this one's.
    pub(crate) epoch: u64,
    /// This preview passed the display cap, and the notice saying so has been given.
    capped: bool,
}

/// The most of a preview the notes pane shows (plan §F): a presentation cap, never a change to
/// what the preview holds.
pub(crate) const PREVIEW_CAP: usize = 1 << 20;

impl Notes {
    /// The work the preview belonged to ended: the preview goes in this same update.
    fn end_preview(&mut self) {
        self.preview = None;
    }

    /// The document as canonical state replaced it: parsed again, once.
    fn replace(&mut self, revision: u64, document: String) {
        self.revision = revision;
        if document != self.document {
            self.chunks = markdown::chunks(&document);
            self.document = document;
        }
    }

    /// A committed block appended: only the block is parsed.
    fn append(&mut self, revision: u64, block: &str) {
        let from = self.document.len();
        self.document.push_str(block);
        self.revision = revision;
        markdown::extend(&mut self.chunks, &self.document, from);
    }
}

/// A notes request this TUI submitted (plan §F work lanes): its own, in the order core runs them.
/// The hint line (Task 10) submits them; until it lands only the tests do.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OwnOp {
    Snapshot,
    Polish,
}

/// What the notes lane is doing now, from typed events and this TUI's own submissions alone —
/// never from Busy text (plan §C 13).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Lane {
    Idle,
    /// Own snapshot at the head: `snapshot: writing`.
    Snapshot,
    /// Own polish at the head, its snapshot first: `polish: snapshot first`.
    PolishSnapshotFirst,
    /// Own polish at the head, its snapshot done: `polishing`.
    Polishing,
    /// Stopping, nothing of this TUI's at the head: core's last snapshot is writing.
    LastSnapshot,
}

/// The exact warning core gives when the lecture ends while the study page is still typesetting
/// (core `session/lecture.rs`): the page lane's one terminal condition without a typed event of
/// its own. Recognised by equality for that lane alone; no other warning is ever read.
pub(crate) const PAGE_ABORTED: &str = "the page was still being typeset; `lecture page` finishes it later";

/// The work lanes (plan §F): this TUI's accepted submissions in order, the head in flight; the
/// polish head's phase; core's last snapshot while stopping; the study page typesetting.
/// Presentation state only — the notes themselves always follow canonical events.
#[derive(Debug, Default)]
pub(crate) struct Work {
    fifo: VecDeque<OwnOp>,
    /// The polish at the head has had its snapshot.
    polishing: bool,
    last: bool,
    page: bool,
}

// Submitting, cancelling and hurrying are the hint line's (Task 10); until it lands only the tests
// drive them.
#[cfg_attr(not(test), allow(dead_code))]
impl Work {
    /// An op the frontend has sent: queued behind whatever is in flight, with no limit.
    pub(crate) fn submit(&mut self, op: OwnOp) {
        self.fifo.push_back(op);
    }

    /// Ctrl-X: core drops what is queued and stops what runs; nothing of this TUI's is pending any
    /// more. Whether there was anything to cancel. The page and the last snapshot are not own
    /// requests and are untouched.
    pub(crate) fn cancel(&mut self) -> bool {
        let had = !self.fifo.is_empty();
        self.fifo.clear();
        self.polishing = false;
        had
    }

    /// Stop waiting: core drops the ops queued behind the one in flight, which still finishes.
    pub(crate) fn hurry(&mut self) {
        self.fifo.truncate(1);
    }

    pub(crate) fn mine(&self) -> bool {
        !self.fifo.is_empty()
    }

    pub(crate) fn lane(&self) -> Lane {
        match self.fifo.front() {
            Some(OwnOp::Snapshot) => Lane::Snapshot,
            Some(OwnOp::Polish) if self.polishing => Lane::Polishing,
            Some(OwnOp::Polish) => Lane::PolishSnapshotFirst,
            None if self.last => Lane::LastSnapshot,
            None => Lane::Idle,
        }
    }

    /// Own requests waiting behind the head.
    pub(crate) fn queued(&self) -> usize {
        self.fifo.len().saturating_sub(1)
    }

    /// The study page is typesetting.
    pub(crate) fn page(&self) -> bool {
        self.page
    }

    fn pop(&mut self) {
        self.fifo.pop_front();
        self.polishing = false;
    }

    /// One event's effect on the lanes. A terminal event with nothing of this TUI's to match is
    /// the notice alone; while stopping, a snapshot with nothing of this TUI's at the head is the
    /// last snapshot's.
    fn event(&mut self, e: &Event, stopping: bool) {
        let head = self.fifo.front().copied();
        match (e, head) {
            (Event::Preview(_), None) if stopping => self.last = true,
            (Event::Committed { .. } | Event::NothingNew, Some(OwnOp::Snapshot)) | (Event::SnapshotFailed(_), Some(OwnOp::Snapshot)) => self.pop(),
            // the polish's own snapshot is done; the polish itself goes on
            (Event::Committed { .. } | Event::NothingNew, Some(OwnOp::Polish)) => self.polishing = true,
            (Event::Polished { .. }, Some(OwnOp::Polish)) => {
                self.pop();
                self.page = true;
            }
            (Event::PolishStopped(_) | Event::PolishFailed(_), Some(OwnOp::Polish)) => self.pop(),
            (Event::Cancelled(_), Some(_)) => self.pop(),
            (Event::Committed { .. } | Event::NothingNew | Event::SnapshotFailed(_), None) => self.last = false,
            (Event::Page { .. } | Event::PageFailed(_), _) => self.page = false,
            (Event::Warning(w), _) if w == PAGE_ABORTED => self.page = false,
            _ => {}
        }
    }
}

/// A registered slide (plan §F): what the slides pane will show.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SlideLine {
    pub(crate) index: u32,
    pub(crate) file: String,
    pub(crate) shown_at: DateTime<Local>,
    pub(crate) auto: bool,
    pub(crate) uncertain: bool,
}

/// One activity record (plan §F): display history only, never persisted and never the record of
/// anything. The wording is the plain CLI's own.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Activity {
    pub(crate) at: DateTime<Local>,
    /// The notice's mark kind, as `say` names it ("notes", "done", "slide", "page", "warn"), or
    /// "dim" for the busy line.
    pub(crate) kind: &'static str,
    pub(crate) label: String,
    pub(crate) detail: String,
}

/// The activity ring: at most [`Ring::CAPACITY`] records, the oldest falling off (plan §F).
#[derive(Debug, Default)]
pub(crate) struct Ring(VecDeque<Activity>);

impl Ring {
    pub(crate) const CAPACITY: usize = 500;

    fn push(&mut self, a: Activity) {
        self.0.push_back(a);
        while self.0.len() > Self::CAPACITY {
            self.0.pop_front();
        }
    }

    /// The ring's readers exist for the tests and the activity overlay (Task 10): the Task-6 view
    /// does not yet read the ring back.
    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.0.len()
    }

    #[cfg(test)]
    pub(crate) fn records(&self) -> &VecDeque<Activity> {
        &self.0
    }
}

/// What reducing an event asked for: a re-read of the canonical files, because a discontinuity the
/// live stream cannot fill appeared — a segment id beyond the next, a notes revision beyond the
/// next, or a polish (plan §F's only mid-session triggers). Nothing cosmetic ever asks.
#[derive(Debug, Default)]
pub(crate) struct Effect {
    pub(crate) hydrate: bool,
}

/// A transcript gap's identity: its recording and first sample, the pair core itself resolves a gap
/// by. Its end may arrive later and its resolution changes; neither changes which gap it is.
type GapKey = (u128, u64);

fn gap_key(g: &Gap) -> GapKey {
    (g.recording_id.as_u128(), g.start_sample)
}

/// The session projection (plan §F). One owner: the reactor. Never shared, never locked.
pub(crate) struct View {
    /// The lecture's identity, cleaned as it entered (plan §C 12).
    pub(crate) identity: Identity,
    /// The stop stage the projection shows; the shared controller owns the truth (plan §G).
    pub(crate) phase: Stage,

    // Latest-value telemetry (plan §C 6): a missed sample is cosmetic, and none of it hydrates.
    /// The last audio level, as it arrived (linear); the header shows its dBFS.
    pub(crate) level: Option<f32>,
    /// Whether the loopback has been silent for ten seconds (loopback and mixed inputs only).
    pub(crate) silence: bool,
    watch: Option<SilenceWatch>,
    /// The single input that is away, while it is gone (mixed leaves it unset: Zoom goes on).
    pub(crate) input_gone: Option<String>,
    /// The transcription connection, as core typed it; never flattened to a string before the view.
    pub(crate) stt: Option<SttStatus>,
    /// The capture worker's state, as core typed it (Task 11 renders it).
    pub(crate) capture: Option<CaptureState>,

    // Canonical, reconciled state (plan §F): durable ids, canonical files.
    /// Transcript gaps waiting for recovery, by identity; [`View::gaps`] is their count.
    waiting: BTreeSet<GapKey>,
    /// Transcript gaps known to be resolved, by identity. A gap only ever moves from waiting to
    /// resolved (core never reopens or removes one), so this set is what keeps a hydration read
    /// before a live `Recovered` from bringing that gap back.
    resolved: BTreeSet<GapKey>,
    /// Closed segments: always the log's contiguous prefix, ids 0, 1, 2, … with no hole.
    /// [`View::pending`] holds every known segment after the first missing id until the hole is
    /// filled, by a hydration or by the missing segment itself.
    pub(crate) closed: Vec<Line>,
    pending: Vec<Line>,
    /// The utterance being said.
    pub(crate) open: Option<OpenUtterance>,
    pub(crate) notes: Notes,
    pub(crate) slides: Vec<SlideLine>,
    /// The work lanes the notes heading shows: this TUI's own requests, the last snapshot, the page.
    pub(crate) work: Work,

    // Sampled and display-only state.
    /// The lecture's spend today, sampled off the reactor at most once a second (plan §B 7). The
    /// shared ledger is the authority; no event's dollars are added to it.
    pub(crate) spend: Option<f64>,
    pub(crate) activity: Ring,
    /// The notice line (plan §H): the input that is gone, while it is gone; else the latest notice.
    pub(crate) notice: Option<Notice>,
    /// The notice line holds a transcription-connection notice (interrupted, server, stopped,
    /// refused): a real `Connected` makes it untrue, so that one clears the line — and only then.
    /// Set from the event's type as the notice is recorded, never read back from its words.
    stt_notice: bool,
}

impl View {
    /// The projection as the initial hydration read it, while the folder lock was still held: the
    /// canonical files, and the start-up records the plain report printed, seeding the activity.
    pub(crate) fn new(identity: Identity, h: Hydration, seed: Vec<Notice>) -> View {
        let kind = identity.kind;
        let mut v = View {
            identity: Identity {
                course: plain::clean(&identity.course),
                lecture: plain::clean(&identity.lecture),
                input: plain::clean(&identity.input),
                notes_file: plain::clean(&identity.notes_file),
                transcript_file: plain::clean(&identity.transcript_file),
                kind,
            },
            phase: Stage::Listening,
            level: None,
            silence: false,
            watch: matches!(kind, SourceKind::Loopback | SourceKind::Mixed).then(|| SilenceWatch::new(-60.0, 10)),
            input_gone: None,
            stt: None,
            capture: None,
            waiting: BTreeSet::new(),
            resolved: BTreeSet::new(),
            closed: Vec::new(),
            pending: Vec::new(),
            open: None,
            notes: Notes::default(),
            slides: Vec::new(),
            work: Work::default(),
            spend: None,
            activity: Ring::default(),
            notice: None,
            stt_notice: false,
        };
        // An empty projection merging its first read: one reconciliation rule for every hydration.
        v.merge(h);
        let at = Local::now();
        for n in seed {
            v.record(n, at);
        }
        v
    }

    /// One live event into the projection. Pure: no I/O, no clocks but `at`. Canonical state moves
    /// by its durable id, latest-value state is replaced, and every externally-sourced string is
    /// cleaned as it enters (plan §C 12).
    pub(crate) fn reduce(&mut self, e: &Event, at: DateTime<Local>) -> Effect {
        let mut effect = Effect::default();
        self.work.event(e, self.phase != Stage::Listening);
        match e {
            Event::Session(n) => self.session(n, at, &mut effect),
            // Busy text is presentation only: shown, never parsed for what is running (plan §C 13).
            Event::Busy(m) => self.activity.push(Activity { at, kind: "dim", label: "…".into(), detail: plain::clean(m) }),
            Event::Preview(d) => self.preview(d, at),
            Event::NothingNew | Event::SnapshotFailed(_) | Event::Cancelled(_) => self.notes.end_preview(),
            Event::Committed { block, revision, .. } => self.committed(block, *revision, &mut effect),
            // The polished document is canonical only in the file; the event cannot rebuild it.
            Event::Polished { .. } => {
                self.notes.end_preview();
                effect.hydrate = true;
            }
            Event::Slide { index, file, auto, uncertain, shown_at } => self.slide(*index, file, *auto, *uncertain, *shown_at),
            Event::Capture(s) => self.capture = Some(capture_cleaned(s)),
            _ => {}
        }
        // The wording both frontends share: the ring always; the notice line when no input is gone.
        if let Some(n) = plain::notice(e, WORDS) {
            self.record(n, at);
            self.stt_notice = self.input_gone.is_none() && matches!(e, Event::Session(Notification::Stt(_)));
        }
        effect
    }

    /// A session notification: the recording's own events (spec §3.6).
    fn session(&mut self, n: &Notification, at: DateTime<Local>, effect: &mut Effect) {
        match n {
            Notification::Segment(s) => self.segment(s, effect),
            Notification::Level(l) => {
                self.level = Some(*l); // latest-value: a missed sample moves nothing but the meter
                if let Some(w) = self.watch.as_mut() {
                    if w.observe(*l) {
                        self.silence = true;
                        self.record(plain::no_signal(), at);
                    } else if dbfs(*l) >= -60.0 {
                        self.silence = false;
                    }
                }
            }
            // Only a transcript gap waits for recovery; an audio gap is explained as it is (spec §5.4),
            // and neither kind is counted as the other.
            Notification::Gap(g) if g.kind.is_transcript() => self.gap(g),
            Notification::Gap(_) => {}
            Notification::DeviceGone { uid } => {
                if self.identity.kind == SourceKind::Input {
                    self.input_gone = Some(plain::clean(uid));
                    // The top-priority notice holds the line until the input returns (plan §H).
                    self.notice = Some(plain::input_gone(&plain::clean(uid)));
                    self.stt_notice = false;
                }
            }
            Notification::DeviceBack { .. } => {
                // The device returned; that says nothing about the signal: only this mark clears,
                // never the silence or the level (only a level above the threshold does).
                if self.input_gone.is_some() {
                    self.input_gone = None;
                    self.notice = None;
                }
            }
            Notification::Stt(s) => {
                // The connection is back: a notice saying it is not no longer holds. The activity
                // keeps the record of the interruption; any other notice stays where it is.
                if matches!(s, SttStatus::Connected) && self.stt_notice {
                    self.notice = None;
                    self.stt_notice = false;
                }
                self.stt = Some(stt_cleaned(s));
            }
            Notification::Open { stable, tentative } => {
                self.open = Some(OpenUtterance { stable: plain::clean(stable), tentative: plain::clean(tentative) });
            }
            // One `Recovered` resolves the gap core says it resolved, by its identity; the transcript
            // is never called whole on the strength of one telemetry event.
            Notification::Recovered(g) => {
                self.waiting.remove(&gap_key(g));
                self.resolved.insert(gap_key(g));
            }
            Notification::Recording { .. } | Notification::Failed(_) | Notification::RecoveryFailed(_) | Notification::SpendFailed(_) | Notification::SourceEnded => {}
        }
    }

    /// A closed segment by its log id (plan §F): an id the projection already holds is ignored, the
    /// next id appends, and anything beyond that asks for the files while the arrival waits for
    /// them — no text is ever guessed into a hole.
    fn segment(&mut self, s: &Segment, effect: &mut Effect) {
        let next = self.next_id();
        if s.id < next {
            return; // a duplicate or an old id: the log's id is the identity, and it is held
        }
        let line = segment_line(s);
        if s.id == next {
            self.append(line);
            // the hole, if there was one, is filled: what waited after it continues the prefix
            while self.pending.first().is_some_and(|p| p.id == self.next_id()) {
                self.closed.push(self.pending.remove(0));
            }
        } else {
            effect.hydrate = true; // canonical segments may have been missed
            match self.pending.iter().position(|p| p.id >= line.id) {
                Some(i) if self.pending[i].id == line.id => self.pending[i] = line,
                Some(i) => self.pending.insert(i, line),
                None => self.pending.push(line),
            }
        }
    }

    /// The id that continues the closed prefix: closed ids are exactly 0 up to it.
    fn next_id(&self) -> u64 {
        self.closed.len() as u64
    }

    /// Transcript gaps waiting for recovery: the header's count.
    pub(crate) fn gaps(&self) -> usize {
        self.waiting.len()
    }

    /// A transcript gap seen, live or in a read: it waits unless it is resolved, and once resolved —
    /// here or earlier — it never waits again, because core never reopens a gap. Seeing it twice
    /// changes nothing.
    fn gap(&mut self, g: &Gap) {
        let key = gap_key(g);
        if g.resolved || self.resolved.contains(&key) {
            self.waiting.remove(&key);
            self.resolved.insert(key);
        } else {
            self.waiting.insert(key);
        }
    }

    /// Appends a closed segment; a live one closes the open utterance it finalises — a recovered
    /// segment is history, and never closes what is being said now (plan §F).
    fn append(&mut self, line: Line) {
        if line.source == SegmentSource::Live {
            self.open = None;
        }
        self.closed.push(line);
    }

    /// A commit by its revision (plan §F): `held + 1` appends its block, `≤ held` changes nothing
    /// (the document already holds it — though the work still ended), and a jump leaves the document
    /// alone and asks for the files, because no skipped revision is invented.
    fn committed(&mut self, block: &str, revision: u64, effect: &mut Effect) {
        // The parsed block replaces the preview in this one update: no frame holds both.
        self.notes.end_preview();
        match revision.cmp(&(self.notes.revision + 1)) {
            Ordering::Equal => self.notes.append(revision, &plain::clean(block)),
            Ordering::Less => {}
            Ordering::Greater => effect.hydrate = true,
        }
    }

    /// A preview delta: appended in order and never dropped. The first delta after a preview ended
    /// begins a new one. Past the display cap the preview still grows here; the pane shows its
    /// first [`PREVIEW_CAP`] bytes, and the notice says so once.
    fn preview(&mut self, delta: &str, at: DateTime<Local>) {
        let n = &mut self.notes;
        if n.preview.is_none() {
            n.epoch += 1;
            n.capped = false;
        }
        let text = n.preview.get_or_insert_with(String::new);
        text.push_str(&plain::clean(delta));
        if text.len() > PREVIEW_CAP && !n.capped {
            n.capped = true;
            self.record(Notice { kind: "warn", label: "preview".into(), detail: "the snapshot being written is longer than 1 MiB; only its first 1 MiB is shown until it is committed".into() }, at);
        }
    }

    /// A slide registration upserts by index (plan §F): a duplicate index refreshes what is shown.
    fn slide(&mut self, index: u32, file: &str, auto: bool, uncertain: bool, shown_at: DateTime<Local>) {
        let line = SlideLine { index, file: plain::clean(file), shown_at, auto, uncertain };
        match self.slides.iter_mut().find(|s| s.index == index) {
            Some(s) => *s = line,
            None => match self.slides.iter().position(|s| s.index > index) {
                Some(i) => self.slides.insert(i, line),
                None => self.slides.push(line),
            },
        }
    }

    /// A hydration result (plan §F): canonical state is adopted, and a result that read earlier than
    /// what the projection already holds loses nothing. Segments reconcile by id; the notes move
    /// only to a revision at or past the one held; slides are only added, never taken away or
    /// rolled back; gaps move only forward, from waiting to resolved.
    pub(crate) fn merge(&mut self, h: Hydration) {
        // Segments: every id known — the log as read (canonical wins for an id both hold), the
        // closed prefix, and what waits beyond a hole — then split again at the first missing id.
        // A stale read that stops short of a hole cannot promote what lies past it.
        let mut known: BTreeMap<u64, Line> = self.closed.drain(..).chain(self.pending.drain(..)).map(|l| (l.id, l)).collect();
        known.extend(h.segments.iter().map(|s| (s.id, segment_line(s))));
        for (id, line) in known {
            if self.pending.is_empty() && id == self.next_id() {
                self.closed.push(line);
            } else {
                self.pending.push(line);
            }
        }

        // Gaps: each canonical transcript gap by identity. A read's resolution clears a gap whose
        // `Recovered` was missed; a read older than a live `Recovered` cannot bring it back; a live
        // gap the read predates is not in it, and stays.
        for g in &h.gaps {
            self.gap(g);
        }

        // Notes: a coherent pair at or past the held revision (an equal revision is the same
        // document by its fingerprint). A stale pair keeps what is held; an incoherent one keeps it
        // too — the next discontinuity re-reads, nothing retries on its own.
        // The read document enters the view cleaned, as every committed block does (plan §C 12),
        // and is parsed once here.
        if let NotesSnapshot::At { revision, document } = h.notes {
            if revision >= self.notes.revision {
                self.notes.replace(revision, plain::clean(&document));
            }
        }

        // Slides: fill the indices the projection does not hold; live registrations keep their state.
        for s in h.slides {
            if !self.slides.iter().any(|x| x.index == s.index) {
                let line = slide_line(&s);
                match self.slides.iter().position(|x| x.index > line.index) {
                    Some(i) => self.slides.insert(i, line),
                    None => self.slides.push(line),
                }
            }
        }
    }

    /// A background read that failed: the projection keeps what it holds, and the failure is said.
    /// Nothing retries on its own; the next discontinuity reads again.
    pub(crate) fn read_failed(&mut self, e: &anyhow::Error, at: DateTime<Local>) {
        self.record(Notice { kind: "warn", label: "re-read".into(), detail: format!("the lecture's files could not be read again ({e:#}); what is shown still holds") }, at);
    }

    /// One notice into the ring and the notice line: the input that is gone holds the line while it
    /// is gone (plan §H's first priority); otherwise the latest notice is the line.
    fn record(&mut self, n: Notice, at: DateTime<Local>) {
        let n = Notice { kind: n.kind, label: plain::clean(&n.label), detail: plain::clean(&n.detail) };
        self.activity.push(Activity { at, kind: n.kind, label: n.label.clone(), detail: n.detail.clone() });
        if self.input_gone.is_none() {
            self.notice = Some(n);
            self.stt_notice = false;
        }
    }
}

/// A log segment as the projection keeps it: cleaned, the words dropped.
fn segment_line(s: &Segment) -> Line {
    Line { id: s.id, said_at: s.said_at, text: plain::clean(&s.text), source: s.source }
}

/// A registered slide as the projection keeps it: cleaned.
fn slide_line(s: &SlideEntry) -> SlideLine {
    SlideLine { index: s.index, file: plain::clean(&s.file), shown_at: s.shown_at, auto: s.auto, uncertain: s.uncertain }
}

/// A typed connection state with its externally-sourced strings cleaned, the shape kept: later
/// views tell a refusal from a reconnect without parsing anything.
fn stt_cleaned(s: &SttStatus) -> SttStatus {
    match s {
        SttStatus::Connected => SttStatus::Connected,
        SttStatus::Retrying { after, reason } => SttStatus::Retrying { after: *after, reason: plain::clean(reason) },
        SttStatus::Refused(m) => SttStatus::Refused(plain::clean(m)),
        SttStatus::ServerError(m) => SttStatus::ServerError(plain::clean(m)),
        SttStatus::Stopped(m) => SttStatus::Stopped(plain::clean(m)),
    }
}

/// A capture state, its window titles and reasons cleaned as they enter. A candidate window's app
/// and title are what a person reads, so they are cleaned; its id and bundle id are what choosing it
/// sends back to core, so they stay exactly as core gave them.
fn capture_cleaned(s: &CaptureState) -> CaptureState {
    match s {
        CaptureState::Unbound => CaptureState::Unbound,
        CaptureState::Watching { window } => CaptureState::Watching { window: plain::clean(window) },
        CaptureState::Paused { window, reason } => CaptureState::Paused { window: plain::clean(window), reason: plain::clean(reason) },
        CaptureState::Asking { window, reason, candidates } => CaptureState::Asking {
            window: plain::clean(window),
            reason: plain::clean(reason),
            candidates: candidates.iter().map(|w| WindowInfo { app: plain::clean(&w.app), title: plain::clean(&w.title), ..w.clone() }).collect(),
        },
        CaptureState::Denied => CaptureState::Denied,
        CaptureState::Failing { window, reason } => CaptureState::Failing { window: plain::clean(window), reason: plain::clean(reason) },
    }
}

/// What a session view is reduced from, for the tests below and the view's: one identity, over
/// whatever canonical state the folder holds.
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use lecturelive_core::session::files::LectureFiles;
    use lecturelive_core::session::segments::{NewSegment, SegmentLog};
    use lecturelive_core::session::sidecar::{Gap, GapKind};

    use super::super::hydrate;

    fn at() -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 9, 26, 10, 0, 0).unwrap()
    }

    fn identity(kind: SourceKind) -> Identity {
        Identity { course: "Machine Learning".into(), lecture: "Week 03 — Optimisation".into(), input: "BlackHole 2ch".into(), kind, notes_file: "lecture_notes_20260926.md".into(), transcript_file: "lecture_transcript_20260926.txt".into() }
    }

    fn view() -> View {
        View::new(identity(SourceKind::Loopback), Hydration::empty(), Vec::new())
    }

    fn seg(id: u64, text: &str, source: SegmentSource) -> Segment {
        let t = at();
        Segment { id, recording_id: Default::default(), start_sample: id * 16_000, end_sample: (id + 1) * 16_000, said_at: t, start: t, end: t, text: text.into(), words: Vec::new(), source }
    }

    fn segment(id: u64, text: &str, source: SegmentSource) -> Event {
        Event::Session(Notification::Segment(seg(id, text, source)))
    }

    fn committed(revision: u64) -> Event {
        Event::Committed { words: 12, slides: 1, block: format!("\n<!-- 10:00:00 -->\n## Block {revision}\n"), usd: 0.02, confirmed: true, removed: 0, missing: 0, revision }
    }

    fn hydration(notes: NotesSnapshot) -> Hydration {
        Hydration { segments: Vec::new(), notes, slides: Vec::new(), gaps: Vec::new() }
    }

    /// Plan §J: an Open utterance appears provisionally, a newer Open replaces it, and the live
    /// segment that finalises it closes it as it lands.
    #[test]
    fn open_then_final_transcript() {
        let mut v = view();
        assert_eq!(v.open, None);
        v.reduce(&Event::Session(Notification::Open { stable: "the learning".into(), tentative: " rate".into() }), at());
        assert_eq!(v.open.as_ref().map(|o| (o.stable.as_str(), o.tentative.as_str())), Some(("the learning", " rate")));
        v.reduce(&Event::Session(Notification::Open { stable: "the learning rate".into(), tentative: String::new() }), at());
        assert_eq!(v.open.as_ref().map(|o| o.stable.as_str()), Some("the learning rate"), "an Open replaces the one before");
        v.reduce(&segment(0, "the learning rate", SegmentSource::Live), at());
        assert_eq!(v.open, None, "the live segment closed it");
        assert_eq!(v.closed.iter().map(|l| (l.id, l.text.as_str())).collect::<Vec<_>>(), vec![(0, "the learning rate")]);
    }

    /// Plan §J: recovery commits history; it must not close what is being said now.
    #[test]
    fn recovered_segment_does_not_close_live_open() {
        let mut v = view();
        v.reduce(&Event::Session(Notification::Open { stable: "gradient".into(), tentative: " descent".into() }), at());
        v.reduce(&segment(0, "recovered words", SegmentSource::Recovered), at());
        assert!(v.open.is_some(), "a recovered segment is history");
        assert_eq!(v.closed.len(), 1, "it is still appended");
        v.reduce(&segment(1, "gradient descent", SegmentSource::Live), at());
        assert!(v.open.is_none(), "only the live one closes the utterance");
    }

    /// Plan §J: an old or duplicate id is not appended; the log's id is the identity.
    #[test]
    fn duplicate_segment_is_not_appended() {
        let mut v = view();
        v.reduce(&segment(0, "one", SegmentSource::Live), at());
        v.reduce(&segment(1, "two", SegmentSource::Live), at());
        assert!(!v.reduce(&segment(1, "two", SegmentSource::Live), at()).hydrate);
        assert!(!v.reduce(&segment(0, "one", SegmentSource::Live), at()).hydrate);
        assert_eq!(v.closed.iter().map(|l| l.id).collect::<Vec<_>>(), vec![0, 1]);
        assert_eq!(v.closed.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(), vec!["one", "two"], "no reordering either");
    }

    /// Plan §J: held last id N, an event at N + 2 is a hole: hydration is asked for, and nothing is
    /// guessed into the gap.
    #[test]
    fn segment_hole_requests_rehydration() {
        let mut v = view();
        v.reduce(&segment(0, "one", SegmentSource::Live), at());
        v.reduce(&segment(1, "two", SegmentSource::Live), at());
        let effect = v.reduce(&segment(3, "four", SegmentSource::Live), at());
        assert!(effect.hydrate, "id 3 while the next is 2");
        assert_eq!(v.closed.iter().map(|l| l.id).collect::<Vec<_>>(), vec![0, 1], "nothing lands in the hole");
        // and a first id that is not 0 is a hole too
        let mut fresh = view();
        assert!(fresh.reduce(&segment(2, "from nowhere", SegmentSource::Live), at()).hydrate);
    }

    /// Plan §J: the missed ids come from the log. A real segment log in a tempdir holds 0–9; the
    /// events deliver 0–4 and then 7; the view ends with 0–9, once each, and goes on contiguously.
    #[tokio::test]
    async fn a_lost_segment_comes_back_from_the_log() {
        let dir = tempfile::tempdir().unwrap();
        let files = LectureFiles::standard(dir.path(), chrono::NaiveDate::from_ymd_opt(2026, 9, 26).unwrap());
        std::fs::create_dir_all(files.state_dir()).unwrap();
        let mut log = SegmentLog::open(dir.path(), &files.stem).unwrap();
        for id in 0..=4u64 {
            log.append(NewSegment { recording_id: Default::default(), start_sample: id * 16_000, end_sample: (id + 1) * 16_000, text: format!("line {id}"), words: Vec::new(), source: SegmentSource::Live }, at()).unwrap();
        }
        drop(log);
        // the session starts over the log as it is
        let mut v = View::new(identity(SourceKind::Loopback), hydrate::read(&files).await.unwrap(), Vec::new());
        for id in 0..=4u64 {
            assert!(!v.reduce(&segment(id, &format!("line {id}"), SegmentSource::Live), at()).hydrate);
        }
        // a read taken while the log still ended at 4, which will land late
        let stale = hydrate::read(&files).await.unwrap();
        // the log grew to 9 while the session ran; the notifications for 5 and 6 were lost, and 7's
        // arrives now
        let mut log = SegmentLog::open(dir.path(), &files.stem).unwrap();
        for id in 5..=9u64 {
            log.append(NewSegment { recording_id: Default::default(), start_sample: id * 16_000, end_sample: (id + 1) * 16_000, text: format!("line {id}"), words: Vec::new(), source: SegmentSource::Live }, at()).unwrap();
        }
        drop(log);
        assert!(v.reduce(&segment(7, "line 7", SegmentSource::Live), at()).hydrate, "a hole at 5 and 6");
        v.merge(stale);
        assert_eq!(v.closed.iter().map(|l| l.id).collect::<Vec<_>>(), (0..=4).collect::<Vec<_>>(), "the stale read closed nothing past the hole");
        assert_eq!(v.pending.iter().map(|l| l.id).collect::<Vec<_>>(), vec![7]);
        v.merge(hydrate::read(&files).await.unwrap());
        assert!(v.pending.is_empty(), "the fresh read filled the hole and released 7");
        assert_eq!(v.closed.iter().map(|l| l.id).collect::<Vec<_>>(), (0..=9).collect::<Vec<_>>(), "contiguous, from the log");
        assert_eq!(v.closed.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(), (0..=9).map(|id| format!("line {id}")).collect::<Vec<_>>(), "once each");
        // the stream continues from there with no further read
        let mut log = SegmentLog::open(dir.path(), &files.stem).unwrap();
        log.append(NewSegment { recording_id: Default::default(), start_sample: 160_000, end_sample: 176_000, text: "line 10".into(), words: Vec::new(), source: SegmentSource::Live }, at()).unwrap();
        drop(log);
        assert!(!v.reduce(&segment(10, "line 10", SegmentSource::Live), at()).hydrate);
    }

    /// Plan §J: a hydration that started earlier than the live stream must not undo it — segments
    /// the log did not yet hold, a notes revision already applied, a slide already registered.
    #[test]
    fn stale_hydration_never_rolls_back() {
        let mut v = view();
        for id in 0..=12u64 {
            v.reduce(&segment(id, &format!("line {id}"), SegmentSource::Live), at());
        }
        v.reduce(&committed(1), at());
        v.reduce(&committed(2), at());
        v.reduce(&Event::Slide { index: 5, file: "slides/slide_05_100000.png".into(), auto: true, uncertain: false, shown_at: at() }, at());
        let stale = Hydration {
            segments: (0..=10).map(|id| seg(id, &format!("line {id}"), SegmentSource::Live)).collect(),
            notes: NotesSnapshot::At { revision: 1, document: "only the first block\n".into() },
            slides: vec![SlideEntry { index: 2, file: "slides/slide_02_100000.png".into(), shown_at: at(), auto: false, uncertain: false }],
            gaps: Vec::new(),
        };
        v.merge(stale);
        assert_eq!(v.closed.iter().map(|l| l.id).collect::<Vec<_>>(), (0..=12).collect::<Vec<_>>(), "segments 11 and 12 survive the read that stopped at 10");
        assert_eq!(v.notes.revision, 2, "the notes did not move back to 1");
        assert!(v.notes.document.contains("Block 2"), "the document still holds revision 2's block");
        assert!(v.slides.iter().any(|s| s.index == 5), "a live registration is not removed");
        assert!(v.slides.iter().any(|s| s.index == 2), "and the read's older slide was added");
    }

    /// Plan §J: the connection's ups and downs change only its own words.
    #[test]
    fn reconnect_preserves_recording_state() {
        let mut v = view();
        v.reduce(&segment(0, "one", SegmentSource::Live), at());
        v.reduce(&committed(1), at());
        let before = (v.phase, v.closed.len(), v.gaps(), v.notes.revision, v.slides.len(), v.spend);
        v.reduce(&Event::Session(Notification::Stt(SttStatus::Retrying { after: std::time::Duration::from_secs(4), reason: "socket closed".into() })), at());
        assert!(matches!(v.stt, Some(SttStatus::Retrying { .. })));
        v.reduce(&Event::Session(Notification::Stt(SttStatus::Connected)), at());
        assert_eq!(v.stt, Some(SttStatus::Connected));
        assert_eq!((v.phase, v.closed.len(), v.gaps(), v.notes.revision, v.slides.len(), v.spend), before, "recording state untouched");
    }

    /// Seen in Apple Terminal at Task 7: the header said "transcribing" while the notice line still
    /// said "transcription interrupted … reconnecting in 1 s". A real `Connected` clears a notice
    /// the connection itself raised; the activity keeps the interruption, and a notice anything else
    /// raised stays.
    #[test]
    fn connected_stt_clears_the_stale_transcription_notice() {
        let retrying = || Event::Session(Notification::Stt(SttStatus::Retrying { after: std::time::Duration::from_secs(1), reason: "socket closed".into() }));
        let connected = Event::Session(Notification::Stt(SttStatus::Connected));
        let mut v = view();
        v.reduce(&retrying(), at());
        assert_eq!(v.notice.as_ref().map(|n| n.label.as_str()), Some("transcription interrupted"));
        v.reduce(&connected, at());
        assert_eq!(v.notice, None, "the line no longer claims an interruption");
        assert!(v.activity.records().iter().any(|a| a.label == "transcription interrupted" && a.detail == "socket closed; reconnecting in 1 s"), "the history keeps it");
        // every connection notice core can follow with a real Connected
        for s in [SttStatus::ServerError("500".into()), SttStatus::Stopped("the worker stopped".into()), SttStatus::Refused("bad key".into())] {
            v.reduce(&Event::Session(Notification::Stt(s)), at());
            assert!(v.notice.as_ref().is_some_and(|n| n.label.starts_with("transcription")));
            v.reduce(&connected, at());
            assert_eq!(v.notice, None);
        }
        // an interruption followed by an unrelated notice: Connected leaves that one alone
        v.reduce(&retrying(), at());
        v.reduce(&Event::SnapshotFailed("timed out".into()), at());
        v.reduce(&connected, at());
        assert_eq!(v.notice.as_ref().map(|n| n.label.as_str()), Some("snapshot failed"));
        // and a Connected with no connection notice showing changes nothing
        v.reduce(&connected, at());
        assert_eq!(v.notice.as_ref().map(|n| n.label.as_str()), Some("snapshot failed"));
        // a gone input holds the line through a reconnect
        let mut w = View::new(identity(SourceKind::Input), Hydration::empty(), Vec::new());
        w.reduce(&retrying(), at());
        w.reduce(&Event::Session(Notification::DeviceGone { uid: "Receiver_UID".into() }), at());
        w.reduce(&connected, at());
        assert_eq!(w.notice.as_ref().map(|n| n.label.as_str()), Some("input gone"));
    }

    /// Plan §J: a refusal stays a refusal — semantically, not as a string — until a real later event
    /// changes it.
    #[test]
    fn refused_stt_does_not_fake_reconnect() {
        let mut v = view();
        v.reduce(&Event::Session(Notification::Stt(SttStatus::Refused("bad key".into()))), at());
        v.reduce(&Event::Session(Notification::Level(0.1)), at());
        v.reduce(&Event::Session(Notification::Open { stable: "words".into(), tentative: String::new() }), at());
        assert_eq!(v.stt, Some(SttStatus::Refused("bad key".into())), "nothing else connected it");
        v.reduce(&Event::Session(Notification::Stt(SttStatus::Connected)), at());
        assert_eq!(v.stt, Some(SttStatus::Connected), "a real later event does change it");
    }

    /// Plan §J: `Level` is latest-value telemetry (plan §C 6). Widely separated samples move the
    /// meter alone: no hydration, no canonical change.
    #[test]
    fn missed_level_samples_change_only_the_meter() {
        let mut v = view();
        v.reduce(&segment(0, "one", SegmentSource::Live), at());
        v.reduce(&committed(1), at());
        v.reduce(&Event::Session(Notification::Stt(SttStatus::Connected)), at());
        let before = (v.closed.clone(), v.gaps(), v.slides.clone(), v.notes.revision, v.notes.document.clone(), v.stt.clone(), v.phase);
        assert!(!v.reduce(&Event::Session(Notification::Level(0.5)), at()).hydrate);
        assert!(!v.reduce(&Event::Session(Notification::Level(0.000_1)), at()).hydrate, "whatever arrived in between was missed");
        assert_eq!(v.level, Some(0.000_1), "the meter shows the latest sample");
        assert_eq!((v.closed.clone(), v.gaps(), v.slides.clone(), v.notes.revision, v.notes.document.clone(), v.stt.clone(), v.phase), before);
    }

    /// Plan §J: held revision N, a commit at N + 2 asks for the files; the re-read's document lands.
    #[test]
    fn revision_jump_requests_reload() {
        let mut v = view();
        assert!(!v.reduce(&committed(1), at()).hydrate, "held + 1 applies with no read");
        assert_eq!(v.notes.revision, 1);
        assert!(v.reduce(&committed(3), at()).hydrate, "a jump over revision 2");
        assert_eq!(v.notes.revision, 1, "the document did not move on the strength of the event");
        v.merge(hydration(NotesSnapshot::At { revision: 3, document: "# the real notes\n".into() }));
        assert_eq!((v.notes.revision, v.notes.document.as_str()), (3, "# the real notes\n"));
    }

    /// Plan §J: an old or duplicate commit still ends the preview — the work is over — without
    /// duplicating notes content the document already holds.
    #[test]
    fn hydrated_commit_still_ends_preview() {
        let mut v = view();
        v.reduce(&committed(1), at());
        v.reduce(&Event::Preview("## A".into()), at());
        v.reduce(&Event::Preview("\n- b".into()), at());
        assert_eq!(v.notes.preview.as_deref(), Some("## A\n- b"));
        v.reduce(&committed(1), at()); // a duplicate delivery of revision 1
        assert_eq!(v.notes.preview, None, "the preview ended");
        assert_eq!(v.notes.document.matches("Block 1").count(), 1, "and nothing was appended twice");
        v.reduce(&Event::Preview("again".into()), at());
        v.merge(hydration(NotesSnapshot::At { revision: 1, document: v.notes.document.clone() }));
        v.reduce(&committed(1), at()); // what a late read of revision 1 would bring
        assert_eq!(v.notes.preview, None);
        assert_eq!(v.notes.document.matches("Block 1").count(), 1);
    }

    /// Plan §J: a polish always re-reads the files; its event cannot rebuild the document.
    #[test]
    fn polished_requests_hydration_and_ends_the_preview() {
        let mut v = view();
        v.reduce(&Event::Preview("## A".into()), at());
        let effect = v.reduce(&Event::Polished { backup: "l/.live_notes/backup.md".into(), usd: 0.04, revision: 4 }, at());
        assert!(effect.hydrate);
        assert_eq!(v.notes.preview, None);
        assert_eq!(v.notes.revision, 0, "the revision comes only from the file");
    }

    /// Plan §J: a transcript gap raises the count, `Recovered` resolves it, saturating; an audio gap
    /// never counts.
    #[test]
    fn gap_and_recovered_move_the_waiting_count() {
        let mut v = view();
        v.reduce(&Event::Session(Notification::Gap(audio_gap())), at());
        assert_eq!(v.gaps(), 0, "an audio gap is explained as it is");
        v.reduce(&Event::Session(Notification::Gap(transcript_gap())), at());
        assert_eq!(v.gaps(), 1);
        v.reduce(&Event::Session(Notification::Recovered(resolved(transcript_gap()))), at());
        assert_eq!(v.gaps(), 0);
        v.reduce(&Event::Session(Notification::Recovered(resolved(transcript_gap()))), at());
        assert_eq!(v.gaps(), 0, "a duplicate recovery cannot underflow");
        v.reduce(&Event::Session(Notification::Gap(transcript_gap())), at());
        assert_eq!(v.gaps(), 0, "a resolved gap seen again does not wait again");
        // two gaps wait; recovering one leaves exactly the other, by identity, not by count
        let (a, b) = (gap_at(1, 0), gap_at(1, 16_000));
        v.reduce(&Event::Session(Notification::Gap(a.clone())), at());
        v.reduce(&Event::Session(Notification::Gap(b.clone())), at());
        v.reduce(&Event::Session(Notification::Gap(a.clone())), at());
        assert_eq!(v.gaps(), 2, "the same gap twice is one gap");
        v.reduce(&Event::Session(Notification::Recovered(resolved(b))), at());
        assert_eq!(v.waiting.iter().copied().collect::<Vec<_>>(), vec![gap_key(&a)]);
    }

    /// The sidecar says a gap was recovered, but its `Recovered` never reached the view: the next
    /// read clears it. Canonical state wins over a count kept from events alone.
    #[test]
    fn hydration_clears_a_gap_when_recovered_notification_was_missed() {
        let mut v = view();
        v.reduce(&Event::Session(Notification::Gap(gap_at(1, 0))), at());
        v.reduce(&Event::Session(Notification::Gap(gap_at(1, 16_000))), at());
        assert_eq!(v.gaps(), 2);
        // the read: the first gap resolved (its notification lost), the second still waiting, and
        // a third the view never heard of, waiting too
        v.merge(Hydration { gaps: vec![resolved(gap_at(1, 0)), gap_at(1, 16_000), gap_at(2, 0)], ..Hydration::empty() });
        assert_eq!(v.gaps(), 2, "the missed recovery cleared; the unheard gap waits");
        assert!(!v.waiting.contains(&gap_key(&gap_at(1, 0))));
        assert!(v.waiting.contains(&gap_key(&gap_at(2, 0))));
        // a gap the read predates (it arrived live after the read) is not in the read, and stays
        v.reduce(&Event::Session(Notification::Gap(gap_at(3, 0))), at());
        v.merge(Hydration { gaps: vec![resolved(gap_at(1, 0)), gap_at(1, 16_000), gap_at(2, 0)], ..Hydration::empty() });
        assert_eq!(v.gaps(), 3);
        // and a first read that finds gaps opens the view with them
        let opened = View::new(identity(SourceKind::Loopback), Hydration { gaps: vec![gap_at(1, 0), resolved(gap_at(1, 16_000))], ..Hydration::empty() }, Vec::new());
        assert_eq!(opened.gaps(), 1);
    }

    /// A read taken before a live `Recovered` still shows that gap waiting. It must not bring it
    /// back: a gap only moves forward, from waiting to resolved.
    #[test]
    fn stale_hydration_does_not_resurrect_a_live_resolved_gap() {
        let mut v = view();
        v.reduce(&Event::Session(Notification::Gap(gap_at(1, 0))), at());
        let stale = || Hydration { gaps: vec![gap_at(1, 0)], ..Hydration::empty() };
        v.reduce(&Event::Session(Notification::Recovered(resolved(gap_at(1, 0)))), at());
        assert_eq!(v.gaps(), 0);
        v.merge(stale());
        assert_eq!(v.gaps(), 0, "the older read did not roll the recovery back");
        // the same holds for a recovery heard of before its gap ever was
        let mut w = view();
        w.reduce(&Event::Session(Notification::Recovered(resolved(gap_at(1, 0)))), at());
        w.merge(stale());
        w.reduce(&Event::Session(Notification::Gap(gap_at(1, 0))), at());
        assert_eq!(w.gaps(), 0);
    }

    /// The closed transcript is always the log's contiguous prefix: a read that stops short of a
    /// hole cannot move what lies beyond it into the closed prefix, and the missing segment, once
    /// it comes, releases what waited behind it.
    #[test]
    fn stale_hydration_does_not_promote_a_segment_past_a_hole() {
        let ids = |l: &[Line]| l.iter().map(|l| l.id).collect::<Vec<_>>();
        let mut v = view();
        v.reduce(&segment(0, "zero", SegmentSource::Live), at());
        v.reduce(&segment(1, "one", SegmentSource::Live), at());
        assert!(v.reduce(&segment(3, "three", SegmentSource::Live), at()).hydrate);
        assert_eq!((ids(&v.closed), ids(&v.pending)), (vec![0, 1], vec![3]));
        // a read taken before segment 2 reached the log
        v.merge(Hydration { segments: vec![seg(0, "zero", SegmentSource::Live), seg(1, "one", SegmentSource::Live)], ..Hydration::empty() });
        assert_eq!((ids(&v.closed), ids(&v.pending)), (vec![0, 1], vec![3]), "3 still waits behind the hole");
        // segment 2 is not taken for an old one: it fills the hole and releases 3
        assert!(!v.reduce(&segment(2, "two", SegmentSource::Live), at()).hydrate);
        assert_eq!((ids(&v.closed), ids(&v.pending)), (vec![0, 1, 2, 3], vec![]));
        assert_eq!(v.closed.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(), vec!["zero", "one", "two", "three"]);
    }

    /// The merge rule's cases: a read that covers the hole closes everything after it; live
    /// segments held beyond a read continue its prefix when they are contiguous with it; a read
    /// that reaches past a still-open hole leaves what is past the hole waiting.
    #[test]
    fn hydration_partitions_segments_at_the_first_missing_id() {
        let ids = |l: &[Line]| l.iter().map(|l| l.id).collect::<Vec<_>>();
        let log = |r: std::ops::RangeInclusive<u64>| Hydration { segments: r.map(|id| seg(id, &format!("line {id}"), SegmentSource::Live)).collect(), ..Hydration::empty() };
        // the read covers the hole
        let mut v = view();
        for id in [0, 1, 3] {
            v.reduce(&segment(id, &format!("line {id}"), SegmentSource::Live), at());
        }
        v.merge(log(0..=2));
        assert_eq!((ids(&v.closed), ids(&v.pending)), (vec![0, 1, 2, 3], vec![]));
        // live-held 2 and 3 continue a read of 0 and 1
        let mut v = view();
        for id in 0..=3 {
            v.reduce(&segment(id, &format!("line {id}"), SegmentSource::Live), at());
        }
        v.merge(log(0..=1));
        assert_eq!((ids(&v.closed), ids(&v.pending)), (vec![0, 1, 2, 3], vec![]));
        // a first read that is empty while live segments wait past a hole at 0
        let mut v = view();
        v.reduce(&segment(2, "line 2", SegmentSource::Live), at());
        v.merge(log(0..=0));
        assert_eq!((ids(&v.closed), ids(&v.pending)), (vec![0], vec![2]), "1 is still missing");
        // canonical text wins for an id both hold
        v.merge(Hydration { segments: vec![seg(0, "the log's words", SegmentSource::Live)], ..Hydration::empty() });
        assert_eq!(v.closed[0].text, "the log's words");
    }

    /// Capture candidates are shown to the person by app and title: those are cleaned; the window
    /// id and bundle id are what choosing a window sends back to core, and stay exactly as given.
    #[test]
    fn capture_candidates_are_cleaned_but_keep_their_identity() {
        let hostile = WindowInfo { id: 4242, app: "zoom.us\x1b]0;owned\x07".into(), bundle_id: Some("us.zoom.xos".into()), title: "\x1b[2JZoom Meeting\x1b]52;c;cGF5\x07".into(), width: 1280, height: 800, on_screen: true };
        let mut v = view();
        v.reduce(&Event::Capture(CaptureState::Asking { window: "Zoom\x1b[31m".into(), reason: "moved\r\x1b[2K".into(), candidates: vec![hostile.clone()] }), at());
        let Some(CaptureState::Asking { window, reason, candidates }) = &v.capture else { panic!("{:?}", v.capture) };
        assert_eq!((window.as_str(), reason.as_str()), ("Zoom", "moved\n"));
        assert_eq!(candidates.len(), 1);
        let c = &candidates[0];
        assert_eq!((c.app.as_str(), c.title.as_str()), ("zoom.us", "Zoom Meeting"));
        assert_eq!((c.id, c.bundle_id.as_deref(), c.width, c.height, c.on_screen), (4242, Some("us.zoom.xos"), 1280, 800, true), "identity untouched");
        assert_eq!(hostile.title, "\x1b[2JZoom Meeting\x1b]52;c;cGF5\x07", "the event's own data is not mutated");
    }

    /// Plan §J: a single input's gone mark is held until it returns.
    #[test]
    fn device_gone_then_back() {
        let mut v = View::new(identity(SourceKind::Input), Hydration::empty(), Vec::new());
        assert!(!v.reduce(&Event::Session(Notification::DeviceGone { uid: "Receiver_UID".into() }), at()).hydrate, "a gone input never re-reads files");
        assert_eq!(v.input_gone.as_deref(), Some("Receiver_UID"));
        assert!(v.notice.as_ref().is_some_and(|n| n.label == "input gone"), "the top-priority notice holds the line");
        v.reduce(&Event::Session(Notification::DeviceBack { uid: "Receiver_UID".into() }), at());
        assert_eq!(v.input_gone, None);
    }

    /// Plan §J: `DeviceBack` says the device returned, not that signal resumed. The loopback's
    /// silence stays until a level at or above the threshold arrives.
    #[test]
    fn device_return_does_not_imply_signal() {
        let mut v = view(); // the loopback: its level is the signal
        let quiet = 10f32.powf(-80.0 / 20.0);
        for _ in 0..10 {
            v.reduce(&Event::Session(Notification::Level(quiet)), at());
        }
        assert!(v.silence, "ten quiet seconds on the loopback");
        v.reduce(&Event::Session(Notification::DeviceGone { uid: "Receiver_UID".into() }), at());
        v.reduce(&Event::Session(Notification::DeviceBack { uid: "Receiver_UID".into() }), at());
        assert_eq!(v.input_gone, None, "the gone mark did not survive the return");
        assert!(v.silence, "but nothing has been heard yet");
        assert_eq!(v.level, Some(quiet));
        v.reduce(&Event::Session(Notification::Level(0.5)), at());
        assert!(!v.silence, "a real level clears it");
    }

    /// Plan §F: the ring keeps the last [`Ring::CAPACITY`] records; the oldest falls off.
    #[test]
    fn the_activity_ring_keeps_only_the_last_500_records() {
        let mut v = view();
        assert!(v.activity.is_empty());
        for k in 0..502 {
            v.reduce(&Event::Warning(format!("number {k}")), at());
        }
        assert_eq!(v.activity.len(), Ring::CAPACITY);
        assert_eq!(v.activity.records().front().unwrap().detail, "number 2", "the oldest fell off");
        assert_eq!(v.activity.records().back().unwrap().detail, "number 501");
    }

    /// The projection's boundary cleans (plan §C 12): transcript text, open utterances, previews,
    /// notices — no control character enters the view.
    #[test]
    fn untrusted_text_is_cleaned_as_it_enters() {
        let hostile = |s: &str| s.chars().any(|c| c != '\n' && c != '\t' && c.is_control());
        let mut v = view();
        v.reduce(&Event::Session(Notification::Open { stable: "\x1b[31mred".into(), tentative: "\x07".into() }), at());
        let open = v.open.clone().unwrap();
        assert!(!hostile(&open.stable) && !hostile(&open.tentative));
        v.reduce(&segment(0, "\x1b]0;title\x07", SegmentSource::Live), at());
        assert!(!hostile(&v.closed[0].text));
        v.reduce(&Event::Preview("\x1b[58;5;9mpartial".into()), at());
        assert!(!hostile(v.notes.preview.as_deref().unwrap()));
        v.reduce(&Event::SnapshotFailed("failed\x1b[2K".into()), at());
        assert!(!hostile(&v.notice.as_ref().unwrap().detail));
        v.reduce(&Event::Slide { index: 1, file: "slides/s\x1b[31m.png".into(), auto: false, uncertain: false, shown_at: at() }, at());
        assert!(!hostile(&v.slides[0].file));
    }

    /// Busy text goes to the ring as it is, dim, and is never a notice.
    #[test]
    fn busy_text_is_activity_never_a_notice() {
        let mut v = view();
        v.reduce(&Event::Busy("snapshot, 12 words to grok-4.7".into()), at());
        assert_eq!(v.notice, None);
        assert_eq!(v.activity.records().iter().last().map(|a| (a.kind, a.label.as_str(), a.detail.as_str())), Some(("dim", "…", "snapshot, 12 words to grok-4.7")));
    }

    /// The seed records open the activity: what the plain report printed before the terminal was
    /// taken (plan Task 6).
    #[test]
    fn the_seed_opens_the_activity() {
        let seed = vec![plain::Notice { kind: "notes", label: "notes".into(), detail: "lecture_notes_20260926.md created".into() }];
        let v = tested_with(identity(SourceKind::Loopback), seed);
        assert_eq!(v.activity.len(), 1);
        assert_eq!(v.activity.records()[0].detail, "lecture_notes_20260926.md created");
    }

    /// Slide upserts by index: a later registration of the same index refreshes what is shown.
    #[test]
    fn slides_upsert_by_index() {
        let mut v = view();
        v.reduce(&Event::Slide { index: 1, file: "slides/slide_01_100000.png".into(), auto: true, uncertain: true, shown_at: at() }, at());
        v.reduce(&Event::Slide { index: 1, file: "slides/slide_01_100000.png".into(), auto: true, uncertain: false, shown_at: at() }, at());
        assert_eq!(v.slides.len(), 1);
        assert!(!v.slides[0].uncertain, "the same index refreshed, not duplicated");
        v.reduce(&Event::Slide { index: 3, file: "slides/slide_03_100000.png".into(), auto: false, uncertain: false, shown_at: at() }, at());
        assert_eq!(v.slides.iter().map(|s| s.index).collect::<Vec<_>>(), vec![1, 3], "in index order");
    }

    // ---- notes and work lanes (Task 9) ---------------------------------------------------------

    fn preview(d: &str) -> Event {
        Event::Preview(d.into())
    }

    /// Plan §J: the committed block replaces the preview in one update — after it, the preview is
    /// gone and the block is canonical, parsed into its chunk, with the document holding it once.
    #[test]
    fn preview_then_commit_replaces_atomically() {
        let mut v = view();
        v.work.submit(OwnOp::Snapshot);
        v.reduce(&preview("## Provisional heading\n- unconfirmed "), at());
        assert!(v.notes.preview.is_some() && v.notes.chunks.is_empty());
        assert_eq!(v.work.lane(), Lane::Snapshot);
        let epoch = v.notes.epoch;
        v.reduce(&committed(1), at());
        assert_eq!(v.notes.preview, None, "the preview ended in the commit's own update");
        assert_eq!(v.notes.revision, 1);
        assert_eq!(v.notes.chunks.len(), 1);
        assert_eq!((v.notes.chunks[0].time.as_deref(), v.notes.chunks[0].blocks[0].text.as_str()), (Some("10:00:00"), "Block 1"));
        assert_eq!(v.work.lane(), Lane::Idle, "the snapshot is done");
        // the next preview is a new one, never the old one's continuation
        v.reduce(&preview("next"), at());
        assert_eq!((v.notes.preview.as_deref(), v.notes.epoch), (Some("next"), epoch + 1));
    }

    /// A contiguous commit parses its own block, not the document; a hydration that moves the
    /// document parses it once; a draw never does (the pane's tests).
    #[test]
    fn committed_notes_are_parsed_once_per_change() {
        let mut v = view();
        v.merge(hydration(NotesSnapshot::At { revision: 1, document: "# Title\n\n<!-- 09:00:00 -->\n## Long ago\n".repeat(1) + &"- a bullet from earlier\n".repeat(400) }));
        let doc = v.notes.document.len();
        markdown::PARSED.with(|n| n.set(0));
        v.reduce(&committed(2), at());
        let block = format!("\n<!-- 10:00:00 -->\n## Block 2\n").len();
        assert!(markdown::PARSED.with(|n| n.get()) <= block, "only the block: {} of a {doc}-byte document", markdown::PARSED.with(|n| n.get()));
        assert_eq!(v.notes.chunks, markdown::chunks(&v.notes.document), "the same as parsing it all");
        // a hydration of the same revision and document parses nothing
        markdown::PARSED.with(|n| n.set(0));
        v.merge(hydration(NotesSnapshot::At { revision: 2, document: v.notes.document.clone() }));
        assert_eq!(markdown::PARSED.with(|n| n.get()), 0);
        // a polished document replaces it all, parsed once
        v.merge(hydration(NotesSnapshot::At { revision: 3, document: "# Title\n\n<!-- 09:00:00 -->\n## Polished\n".into() }));
        assert_eq!(v.notes.chunks.iter().flat_map(|c| &c.blocks).map(|b| b.text.as_str()).collect::<Vec<_>>(), vec!["Title", "Polished"]);
    }

    /// Plan §J: a failed snapshot ends its preview and leaves the canonical notes as they were.
    #[test]
    fn preview_then_fail_keeps_notes() {
        let mut v = view();
        v.reduce(&committed(1), at());
        let (doc, chunks) = (v.notes.document.clone(), v.notes.chunks.clone());
        v.work.submit(OwnOp::Snapshot);
        v.reduce(&preview("## Never written "), at());
        v.reduce(&Event::SnapshotFailed("timed out; everything is kept for the next one".into()), at());
        assert_eq!(v.notes.preview, None);
        assert_eq!((v.notes.revision, &v.notes.document, &v.notes.chunks), (1, &doc, &chunks));
        assert_eq!(v.work.lane(), Lane::Idle);
        assert_eq!(v.notice.as_ref().map(|n| n.label.as_str()), Some("snapshot failed"));
    }

    /// Plan §J: a cancel ends the preview; nothing was written.
    #[test]
    fn preview_then_cancel_clears_preview() {
        let mut v = view();
        v.work.submit(OwnOp::Snapshot);
        v.work.submit(OwnOp::Snapshot);
        v.reduce(&preview("## Half "), at());
        v.reduce(&Event::Cancelled("the snapshot".into()), at());
        assert_eq!(v.notes.preview, None);
        assert_eq!((v.notes.revision, v.notes.chunks.len()), (0, 0));
        assert_eq!(v.work.lane(), Lane::Snapshot, "the matched head went; the next is at the head");
        assert_eq!(v.notice.as_ref().map(|n| n.label.as_str()), Some("cancelled"));
    }

    /// Plan §J: a commit that won the race with Ctrl-X is still reality. The TUI's own requests
    /// are gone, the commit is unmatched — and the notes take it all the same.
    #[test]
    fn cancel_then_commit_accepts_committed_reality() {
        let mut v = view();
        v.work.submit(OwnOp::Snapshot);
        v.work.submit(OwnOp::Polish);
        v.reduce(&preview("## Racing "), at());
        assert!(v.work.cancel(), "there was something of this TUI's to cancel");
        assert_eq!((v.work.lane(), v.work.queued()), (Lane::Idle, 0));
        assert!(!v.work.cancel(), "and now there is nothing");
        v.reduce(&committed(1), at());
        assert_eq!(v.notes.revision, 1, "canonical wins");
        assert!(v.notes.document.contains("Block 1") && v.notes.chunks.len() == 1);
        assert_eq!(v.notes.preview, None);
        assert_eq!(v.work.lane(), Lane::Idle);
        // the in-flight op's own cancellation, arriving later, is a notice and nothing more
        v.reduce(&Event::Cancelled("the snapshot".into()), at());
        assert_eq!((v.notes.revision, v.work.lane()), (1, Lane::Idle));
    }

    /// Plan §J: a polish goes snapshot-first, then polishing, and its success starts the page,
    /// which only its own results — or core's one fixed warning — end.
    #[test]
    fn polish_lane_and_page_lane() {
        let mut v = view();
        v.work.submit(OwnOp::Polish);
        v.work.submit(OwnOp::Snapshot);
        assert_eq!((v.work.lane(), v.work.queued()), (Lane::PolishSnapshotFirst, 1));
        v.reduce(&preview("## The polish's snapshot "), at());
        assert_eq!(v.work.lane(), Lane::PolishSnapshotFirst);
        v.reduce(&committed(1), at());
        assert_eq!(v.work.lane(), Lane::Polishing, "its snapshot committed; the polish is not done");
        assert_eq!(v.work.queued(), 1);
        assert!(!v.work.page());
        assert!(v.reduce(&Event::Polished { backup: "l/.live_notes/backup.md".into(), usd: 0.04, revision: 2 }, at()).hydrate);
        assert_eq!((v.work.lane(), v.work.queued(), v.work.page()), (Lane::Snapshot, 0, true), "the queued snapshot runs; the page typesets");
        v.reduce(&Event::NothingNew, at());
        assert_eq!(v.work.lane(), Lane::Idle);
        assert!(v.work.page(), "a snapshot's result does not end the page");
        v.reduce(&Event::Warning("slide 3 (slides/x.png) is no longer on disk; left out of the notes".into()), at());
        assert!(v.work.page(), "no other warning is read");
        v.reduce(&Event::Warning(PAGE_ABORTED.into()), at());
        assert!(!v.work.page(), "the lecture ended with the page still typesetting");
        // the typed endings
        for end in [Event::PageFailed("the model refused".into()), Event::Page { outcome: page_outcome(), usd: 0.3 }] {
            let mut v = view();
            v.work.submit(OwnOp::Polish);
            v.reduce(&Event::NothingNew, at());
            assert_eq!(v.work.lane(), Lane::Polishing, "nothing new is still the snapshot done");
            v.reduce(&Event::Polished { backup: "b.md".into(), usd: 0.0, revision: 1 }, at());
            assert!(v.work.page());
            v.reduce(&end, at());
            assert!(!v.work.page());
        }
        // a polish that stops or fails ends without a page
        for end in [Event::PolishStopped("the snapshot before it failed".into()), Event::PolishFailed("timed out".into()), Event::Cancelled("the polish".into())] {
            let mut v = view();
            v.work.submit(OwnOp::Polish);
            v.reduce(&end, at());
            assert_eq!((v.work.lane(), v.work.page()), (Lane::Idle, false));
        }
    }

    fn page_outcome() -> lecturelive_core::notes::page::PageOutcome {
        lecturelive_core::notes::page::PageOutcome { path: "lecture_page.html".into(), words: 900, budget: 1200, cached: false, missing: Vec::new() }
    }

    /// Plan §J: the page's progress arrives as Busy text; it moves neither lane.
    #[test]
    fn page_busy_does_not_touch_notes_lane() {
        let mut v = view();
        v.work.submit(OwnOp::Polish);
        v.reduce(&Event::NothingNew, at());
        v.reduce(&Event::Polished { backup: "b.md".into(), usd: 0.0, revision: 1 }, at());
        v.work.submit(OwnOp::Snapshot);
        for busy in ["typesetting the study page", "snapshot, 40 words to grok", "polishing 900 words", "the snapshot is done", "cancelled"] {
            v.reduce(&Event::Busy(busy.into()), at());
            assert_eq!((v.work.lane(), v.work.queued(), v.work.page()), (Lane::Snapshot, 0, true), "{busy:?}");
        }
    }

    /// Plan §J: stopping, with nothing of this TUI's at the head, a preview is core's last
    /// snapshot; with an own snapshot still at the head, it is that one's.
    #[test]
    fn last_snapshot_attributed_while_stopping() {
        let mut v = view();
        v.reduce(&preview("while listening "), at());
        assert_eq!(v.work.lane(), Lane::Idle, "listening, an unmatched preview is no lane");
        v.reduce(&Event::NothingNew, at());
        v.phase = Stage::Stopping;
        v.work.submit(OwnOp::Snapshot);
        v.reduce(&preview("mine "), at());
        assert_eq!(v.work.lane(), Lane::Snapshot, "the queued own snapshot still runs while stopping");
        v.reduce(&committed(1), at());
        assert_eq!(v.work.lane(), Lane::Idle);
        v.reduce(&preview("## What was left "), at());
        assert_eq!(v.work.lane(), Lane::LastSnapshot);
        assert!(!v.work.mine(), "the last snapshot is not the person's own work");
        v.reduce(&committed(2), at());
        assert_eq!(v.work.lane(), Lane::Idle);
        // and its failure ends it too
        v.reduce(&preview("again "), at());
        v.reduce(&Event::SnapshotFailed("timed out".into()), at());
        assert_eq!(v.work.lane(), Lane::Idle);
    }

    /// Stop waiting: core drops what is queued behind the one in flight, which still finishes.
    #[test]
    fn hurry_keeps_only_what_is_in_flight() {
        let mut v = view();
        for op in [OwnOp::Polish, OwnOp::Snapshot, OwnOp::Snapshot] {
            v.work.submit(op);
        }
        v.work.hurry();
        assert_eq!((v.work.lane(), v.work.queued()), (Lane::PolishSnapshotFirst, 0));
        v.work.hurry();
        assert!(v.work.mine(), "the in-flight polish is not claimed aborted");
    }

    /// The display cap: past 1 MiB the preview keeps growing (nothing is dropped) and the notice
    /// says once that only its first 1 MiB is shown.
    #[test]
    fn a_preview_past_the_cap_says_so_once() {
        let mut v = view();
        let chunk = "word ".repeat(1 << 14);
        for _ in 0..(PREVIEW_CAP / chunk.len() + 2) {
            v.reduce(&preview(&chunk), at());
        }
        assert!(v.notes.preview.as_ref().unwrap().len() > PREVIEW_CAP, "nothing dropped");
        assert_eq!(v.activity.records().iter().filter(|a| a.label == "preview").count(), 1);
        assert_eq!(v.notice.as_ref().map(|n| n.label.as_str()), Some("preview"));
        v.reduce(&Event::NothingNew, at());
        v.reduce(&preview(&chunk.repeat(70)), at());
        assert_eq!(v.activity.records().iter().filter(|a| a.label == "preview").count(), 2, "a new preview can say it again");
    }

    fn tested_with(identity: Identity, seed: Vec<Notice>) -> View {
        View::new(identity, Hydration::empty(), seed)
    }

    /// A transcript gap, waiting for recovery, and an audio gap, explained as it is (spec §5.4).
    fn transcript_gap() -> Gap {
        Gap::new(Default::default(), 32_000, Some(48_000), GapKind::SttOffline)
    }

    /// A transcript gap waiting, in recording `rec`, from `start`. The recording id is parsed into
    /// the field's own type, which the CLI does not otherwise name.
    fn gap_at(rec: u128, start: u64) -> Gap {
        let mut g = Gap::new(Default::default(), start, Some(start + 8_000), GapKind::SttOffline);
        g.recording_id = format!("00000000-0000-0000-0000-{rec:012x}").parse().unwrap();
        g
    }

    fn audio_gap() -> Gap {
        Gap::new(Default::default(), 32_000, Some(48_000), GapKind::RecorderOverflow)
    }

    fn resolved(mut g: Gap) -> Gap {
        g.resolved = true;
        g
    }
}
