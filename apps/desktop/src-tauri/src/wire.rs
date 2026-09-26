//! What the adapter sends the frontend (spec §3.6). `src/lib/wire.ts` mirrors these types exactly.
use lecturelive_core::capture::detect::Region;
use lecturelive_core::capture::window::WindowInfo;
use lecturelive_core::session::segments::{Segment, SegmentSource};
use serde::Serialize;

/// Every message: the session it belongs to and its place in the one sequence across all streams.
#[derive(Debug, Clone, Serialize)]
pub struct Envelope<T> {
    pub session: String,
    pub seq: u64,
    #[serde(flatten)]
    pub msg: T,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SegmentView {
    pub id: u64,
    /// When it was said, `HH:MM:SS`.
    pub at: String,
    pub text: String,
    pub recovered: bool,
}

impl From<&Segment> for SegmentView {
    fn from(s: &Segment) -> Self {
        Self { id: s.id, at: s.said_at.format("%H:%M:%S").to_string(), text: s.text.clone(), recovered: s.source == SegmentSource::Recovered }
    }
}

/// The transcript channel.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TranscriptMsg {
    /// What is being said: its settled words and the tentative tail.
    Open { utterance: u64, stable: String, tentative: String },
    /// The open utterance closed into this segment.
    Closed { utterance: u64, segment: SegmentView },
    /// A segment that did not come from the open utterance (recovered).
    Segment { segment: SegmentView },
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    NothingNew,
    Failed,
    Cancelled,
}

/// The notes channel: deltas and the result that ends them share an `op`, so their order is kept.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NotesMsg {
    Delta { op: u64, text: String },
    /// The canonical block that replaces the preview; `revision` is the notes' revision it produced.
    Committed { op: u64, revision: u64, block: String },
    Ended { op: u64, outcome: Outcome, message: String },
    /// The whole document changed: re-fetch it.
    Polished { revision: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    #[default]
    Idle,
    Starting,
    Running,
    /// Finishing the transcript and recovery, then the last snapshot.
    Stopping,
    /// The second stop: no longer waiting for recovery or queued requests.
    StoppingNow,
    Ended,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FolderView {
    pub dir: String,
    pub course: String,
    pub name: String,
    pub notes_dir: String,
    /// The study page, when one exists.
    pub page: Option<String>,
}

/// The whole status, sent whenever any part of it changes.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct Status {
    pub phase: Phase,
    pub folder: Option<FolderView>,
    pub source: Option<String>,
    pub level_dbfs: Option<f32>,
    pub stt: String,
    pub stt_ok: bool,
    /// The notes operation running, in words.
    pub busy: Option<String>,
    /// Transcript gaps not yet recovered.
    pub gaps: usize,
    /// RFC 3339.
    pub started_at: Option<String>,
    /// This lecture's spend today.
    pub spend_usd: f64,
    /// Ten silent seconds on loopback.
    pub silence: bool,
    /// Slide capture: the watched window and its state (spec §7).
    pub capture: CaptureView,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureWord {
    /// No window chosen for this course.
    #[default]
    Unbound,
    /// A window is chosen; watching starts with the lecture.
    Ready,
    Watching,
    Paused,
    Asking,
    Denied,
    Failing,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WindowView {
    pub id: u32,
    pub app: String,
    pub title: String,
    pub width: u32,
    pub height: u32,
    pub on_screen: bool,
}

impl From<&WindowInfo> for WindowView {
    fn from(w: &WindowInfo) -> Self {
        Self { id: w.id, app: w.app.clone(), title: w.title.clone(), width: w.width, height: w.height, on_screen: w.on_screen }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct CaptureView {
    pub state: CaptureWord,
    /// The watched window, as the person calls it.
    pub window: Option<String>,
    /// Why it is paused, asking or failing.
    pub detail: Option<String>,
    /// The windows the person may mean, while asking.
    pub candidates: Vec<WindowView>,
    /// A real capture has succeeded this lecture: the Capture button works (spec §7.4).
    pub captured: bool,
}

/// The course's saved region and the parts of it left out, for the picker to draw on a new still.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SavedRegion {
    pub region: Region,
    pub leave_out: Vec<Region>,
}

/// A still of a window for the picker, through the asset protocol.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PreviewShot {
    pub path: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeKind {
    Notes,
    Slide,
    Page,
    Done,
    Warn,
}

/// One of the CLI's event lines: its mark, what it is, what happened.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Notice {
    pub kind: NoticeKind,
    pub label: String,
    pub detail: String,
    /// `HH:MM:SS`.
    pub at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SlideView {
    pub index: u32,
    /// Relative to the lecture folder.
    pub file: String,
    pub path: String,
    /// When it was first on screen, `HH:MM:SS`.
    pub at: String,
    /// Taken by the change detector.
    pub auto: bool,
    /// Kept after 10 s of change: it may show a transition.
    pub uncertain: bool,
}

/// The status stream (a Tauri event).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StatusMsg {
    Status(Status),
    Notice(Notice),
    Slide(SlideView),
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OpenView {
    pub utterance: u64,
    pub stable: String,
    pub tentative: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PreviewView {
    pub op: u64,
    pub text: String,
}

/// Everything a reloaded frontend needs, at one sequence number (spec §3.6).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SessionState {
    pub session: String,
    /// Messages at or below this are already in the state.
    pub seq: u64,
    pub status: Status,
    pub notices: Vec<Notice>,
    pub segments: Vec<SegmentView>,
    pub open: Option<OpenView>,
    pub revision: u64,
    pub document: String,
    pub preview: Option<PreviewView>,
    /// The notes operation the next deltas belong to.
    pub op: u64,
    pub slides: Vec<SlideView>,
    pub pending_segments: u64,
    pub pending_slides: usize,
}
