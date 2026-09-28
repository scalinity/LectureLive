//! The CLI's capture adapter (M7 plan §E, Task 11): the seam between core's slide capture and the
//! terminal frontends. It owns the course's saved selection — where it lives, how it is loaded
//! before a lecture starts and kept when the watched window moves — the terminal's own words for
//! each capture state, and the rule that decides when Ctrl-S may watch the one window core
//! offers. Core stays the authority (plan §C 1): nothing here finds a window, validates a region
//! or captures a frame; it persists what core relocates and words what core reports.
//!
//! One adapter serves both frontends ([`forward`]): core's `CaptureMoved` is saved for the course
//! before the event continues, so no frontend can say "found again" for a region it has not kept.

use std::path::{Path, PathBuf};

use anyhow::Result;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use lecturelive_core::capture::detect::Thresholds;
use lecturelive_core::capture::select::{Selection, Selections};
use lecturelive_core::capture::window::SystemWindows;
use lecturelive_core::capture::worker::CaptureState;
use lecturelive_core::session::lecture::{CaptureSetup, Event};

/// The saved selections' file, as the desktop names it (spec §7.1): in the app's data folder.
pub(crate) const FILE: &str = "capture.json";

/// What adapting capture needs to know about the lecture (plan Task 11): where the course's
/// selection is saved, the selection as it was loaded before the lecture started — the same
/// state core's worker revalidates — and the capture state as core last emitted it. The reactor
/// alone holds one; nothing rereads the file while the lecture runs, and core's own relocations
/// and this frontend's own kept binds are the only things that update it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Context {
    pub(crate) path: PathBuf,
    pub(crate) course: String,
    pub(crate) saved: Option<Selection>,
    /// The capture state exactly as core emitted it, for the functional decisions (Ctrl-S's
    /// watch-it): never rendered, never cleaned. A candidate's id, bundle id, app, title and
    /// size are its identity when binding (spec §7.1), and display cleaning must never become
    /// persistence identity — so the presentation copy in the view is a different one.
    pub(crate) current: Option<CaptureState>,
}

/// The course's saved selection, loaded off the reactor before the engine starts: a missing file
/// is no selection. A file that cannot be read is no selection too, as the desktop takes it — a
/// preference never stops a lecture from recording — and the reason comes back beside it to be
/// said, never a silent empty state. Nothing is written: [`keep_selection`] still refuses to save
/// over a file it cannot read.
pub(crate) async fn load(path: &Path, course: &str) -> (Option<Selection>, Option<String>) {
    let (path, course) = (path.to_path_buf(), course.to_string());
    match tokio::task::spawn_blocking(move || Selections::load(&path).map(|all| all.get(&course).cloned())).await {
        Ok(Ok(saved)) => (saved, None),
        Ok(Err(e)) => (None, Some(format!("{e:#}"))),
        Err(e) => (None, Some(e.to_string())),
    }
}

/// What both frontends say when the selections file could not be read at the lecture's start.
pub(crate) fn unreadable_words(error: &str) -> (&'static str, &'static str, String) {
    ("warn", "capture", format!("{error}; this lecture starts with no saved window, and the file is left as it is. Screenshots (⌘⇧4) still become slides."))
}

/// Keeps a selection for the course exactly as core produced it — its descriptor, region, parts
/// left out and remembered sizes — preserving every other course in the file.
pub(crate) fn keep_selection(path: &Path, course: &str, sel: &Selection) -> Result<()> {
    let mut all = Selections::load(path)?;
    all.set(course, sel.clone());
    all.save(path)
}

/// What the adapter hands a frontend beside the event itself: whether a relocation it carried
/// was actually kept for the course. The outcome travels in the same ordered stream as the
/// event, so neither frontend infers persistence from wording, and no ordering can diverge.
#[derive(Debug)]
pub(crate) enum CapturePersistence {
    /// The event carried no relocation.
    None,
    /// A relocation kept for the course: this exact selection is on disk.
    MovedSaved(Selection),
    /// A relocation the running session still holds, but the preference could not be written.
    MovedSaveFailed { error: String },
}

/// One core event as a frontend receives it: the event, and what the adapter did about any
/// relocation in it. Both frontends share this single stream.
#[derive(Debug)]
pub(crate) struct Forwarded {
    pub(crate) event: Event,
    pub(crate) capture_persistence: CapturePersistence,
}

/// The one line both frontends say when a relocation could not be kept (plan Task 11): the
/// session found the region again — that truth stands — while the saved selection does not have
/// it, and the failure is the last word.
pub(crate) fn unsaved_words(error: &str) -> (&'static str, &'static str, String) {
    ("warn", "found again", format!("Capture found the slide again, but the new region could not be saved: {error}"))
}

/// The capture event adapter (plan Task 11): the one place core's capture events pass through on
/// their way to whichever frontend is running. A relocation is saved for the course — off this
/// task, on a blocking thread — before the event continues, so a frontend that says "found again"
/// is always behind a region it has already kept; whether the save happened arrives beside the
/// event, so a failure is never dressed as a success. Without a context (a scripted session
/// without capture) it only forwards, and it ends when core's channel closes, so the engine's
/// future resolves only after every final event has been handed on.
pub(crate) async fn forward(ctx: Option<Context>, mut from: UnboundedReceiver<Event>, to: UnboundedSender<Forwarded>) {
    while let Some(e) = from.recv().await {
        let mut persistence = CapturePersistence::None;
        if let (Some(ctx), Event::CaptureMoved { selection, .. }) = (&ctx, &e) {
            let kept = tokio::task::spawn_blocking({
                let (path, course, sel) = (ctx.path.clone(), ctx.course.clone(), selection.clone());
                move || keep_selection(&path, &course, &sel)
            })
            .await;
            persistence = match kept.unwrap_or_else(|e| Err(anyhow::anyhow!("{e}"))) {
                Ok(()) => CapturePersistence::MovedSaved(selection.clone()),
                Err(err) => CapturePersistence::MovedSaveFailed { error: format!("{err:#}") },
            };
        }
        if to.send(Forwarded { event: e, capture_persistence: persistence }).is_err() {
            return; // the frontend is gone: nothing is kept for a reader that is not there
        }
    }
}

/// The production setup, as the desktop builds it (plan Task 11): this Mac's windows, the course's
/// saved selection, once a second, default thresholds, and the detector-input recording the
/// environment names.
pub(crate) fn setup(selection: Option<Selection>, record: Option<PathBuf>) -> CaptureSetup {
    CaptureSetup { source: Box::new(SystemWindows), selection, interval: std::time::Duration::from_secs(1), thresholds: Thresholds::default(), record }
}

/// `LECTURELIVE_RECORD` exactly as the desktop reads it (spec §11): the variable's value verbatim
/// as a path, present or absent — nothing more is read into it.
pub(crate) fn record_path(var: Option<std::ffi::OsString>) -> Option<PathBuf> {
    var.map(PathBuf::from)
}

// ---------------------------------------------------------------------------------------------
// Words.

/// Whose screen recording permission is missing, from the terminal that is running the lecture:
/// named only when it is certainly one of the two (plan §H) — no fuzzy guessing at brands.
pub(crate) fn host_from(term_program: Option<&str>) -> &'static str {
    match term_program {
        Some("Apple_Terminal") => "Terminal",
        Some(p) if p.starts_with("iTerm") => "iTerm",
        _ => "this terminal app",
    }
}

/// The host this session runs in, read once like the theme is.
pub(crate) fn host() -> &'static str {
    host_from(std::env::var("TERM_PROGRAM").ok().as_deref())
}

/// What the shared wording needs to know: the course an unbound state is named by, and the host
/// whose Screen Recording permission may be missing.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Words {
    pub(crate) course: String,
    pub(crate) host: &'static str,
}

impl Words {
    pub(crate) fn new(course: &str) -> Words {
        Words { course: course.to_string(), host: host() }
    }
}

/// A capture state in the terminal's own words (plan §H), shared by plain and the TUI: which mark
/// kind, what it is, what happened. Core's `words()` speaks of the desktop's slides strip; the
/// terminal has no strip, and first-time choosing is the app's (M8 plans the rest).
pub(crate) fn state_words(s: &CaptureState, w: &Words) -> (&'static str, &'static str, String) {
    match s {
        CaptureState::Unbound => ("slide", "no window", format!("No Zoom window chosen for {} yet: choose it once in the LectureLive app. Screenshots (⌘⇧4) still become slides.", w.course)),
        CaptureState::Watching { window } => ("slide", "watching", window.clone()),
        CaptureState::Paused { window, reason } => ("warn", "paused", format!("{window}: {reason}")),
        CaptureState::Asking { window, reason, .. } => ("warn", "asking", format!("{reason}; choose or update “{window}” in the LectureLive app")),
        CaptureState::Denied => ("warn", "screen recording", format!("Screen Recording is off for {}: System Settings → Privacy & Security → Screen & System Audio Recording, then quit and reopen {}.", w.host, w.host)),
        CaptureState::Failing { window, reason } => ("warn", "capture failing", format!("{window}: {reason}")),
    }
}

/// A relocation as both frontends say it (plan §H): the same words in plain lines, the activity
/// ring and the notice line, always after the save has been kept.
pub(crate) fn moved_words(note: &str) -> (&'static str, &'static str, String) {
    ("slide", "found again", note.to_string())
}

/// Whether a capture state needs the person: healthy watching does not; everything else does —
/// a window to choose, permission to grant, a pause to outlast or a failure to explain. Typed,
/// never parsed back out of the words above.
pub(crate) fn attention(s: &CaptureState) -> bool {
    !matches!(s, CaptureState::Watching { .. })
}

// ---------------------------------------------------------------------------------------------
// Ctrl-S.

/// What Ctrl-S does in a capture state (plan §H): capture the watched window now, watch the one
/// window offered through the region already saved for its size, or nothing — with the reason, so
/// the keys can say why. Binding is never silent: only a single candidate that is the saved
/// window, at a size the selection remembers a region for, can be watched (spec §7.1).
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Action {
    Now,
    Watch { window: u32, selection: Selection },
    Refused(Refusal),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// No capture state has arrived at all.
    None,
    Unbound,
    Paused,
    Denied,
    Failing,
    /// Asking with no window offered.
    AskingNone,
    /// Asking with more than one window offered.
    AskingMany,
    /// Asking with a window whose size has no saved region.
    AskingSize,
    /// Asking with a window that is not the one saved.
    AskingOther,
}

pub(crate) fn action(state: Option<&CaptureState>, saved: Option<&Selection>) -> Action {
    match state {
        None => Action::Refused(Refusal::None),
        Some(CaptureState::Watching { .. }) => Action::Now,
        Some(CaptureState::Unbound) => Action::Refused(Refusal::Unbound),
        Some(CaptureState::Paused { .. }) => Action::Refused(Refusal::Paused),
        Some(CaptureState::Denied) => Action::Refused(Refusal::Denied),
        Some(CaptureState::Failing { .. }) => Action::Refused(Refusal::Failing),
        Some(CaptureState::Asking { candidates, .. }) => match candidates.as_slice() {
            [] => Action::Refused(Refusal::AskingNone),
            [only] => match watch_saved(saved, only) {
                Some(selection) => Action::Watch { window: only.id, selection },
                None if saved.is_none_or(|s| !s.descriptor.same_app(only) || s.descriptor.title != only.title) => Action::Refused(Refusal::AskingOther),
                None => Action::Refused(Refusal::AskingSize),
            },
            _ => Action::Refused(Refusal::AskingMany),
        },
    }
}

/// The desktop's "watch it" rule (plan Task 11): the one window offered, watched through the
/// region and parts left out already saved for the course at exactly its size — a size nobody
/// has checked the slide at has no region to reuse, so it is the person's to choose. The
/// selection keeps the window's identity as it was saved and every size it remembers.
pub(crate) fn watch_saved(saved: Option<&Selection>, w: &lecturelive_core::capture::window::WindowInfo) -> Option<Selection> {
    let saved = saved?;
    if !(saved.descriptor.same_app(w) && saved.descriptor.title == w.title) {
        return None;
    }
    saved.at_size(w.width, w.height)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lecturelive_core::capture::detect::Region;
    use lecturelive_core::capture::select::{Descriptor, SizedRegion};
    use lecturelive_core::capture::window::WindowInfo;

    fn win(id: u32, bundle: Option<&str>, app: &str, title: &str, w: u32, h: u32) -> WindowInfo {
        WindowInfo { id, app: app.into(), bundle_id: bundle.map(String::from), title: title.into(), width: w, height: h, on_screen: true }
    }

    /// Zoom's meeting window at 1600 × 900, its smaller size remembered, and a camera patch left
    /// out: the state the app leaves in capture.json after a first lecture.
    fn saved() -> Selection {
        Selection {
            descriptor: Descriptor { bundle_id: Some("us.zoom.xos".into()), app: "zoom.us".into(), title: "Zoom Meeting".into(), width: 1600, height: 900 },
            region: Region { x: 0.1, y: 0.1, w: 0.8, h: 0.8 },
            leave_out: vec![Region { x: 0.75, y: 0.0, w: 0.25, h: 0.3 }],
            sizes: vec![SizedRegion { width: 1280, height: 720, region: Region { x: 0.05, y: 0.12, w: 0.9, h: 0.7 } }],
        }
    }

    fn at_1280(id: u32) -> WindowInfo {
        win(id, Some("us.zoom.xos"), "zoom.us", "Zoom Meeting", 1280, 720)
    }

    fn asking(candidates: Vec<WindowInfo>) -> CaptureState {
        CaptureState::Asking { window: "Zoom Meeting".into(), reason: "it is not where it was".into(), candidates }
    }

    fn words() -> Words {
        Words { course: "Machine Learning".into(), host: "Terminal" }
    }

    /// A missing file is no selection and a broken one is an error, never a silent empty state.
    #[test]
    fn a_missing_file_is_no_selection_and_a_broken_one_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE);
        assert_eq!(Selections::load(&path).unwrap(), Selections::default(), "no file: no selections");
        assert_eq!(Selections::load(&path).unwrap().get("Machine Learning"), None);
        std::fs::write(&path, "{ not json").unwrap();
        assert!(Selections::load(&path).is_err(), "a broken file is not silently empty");
    }

    /// Plan §B 8 builds capture as the desktop does (`app.rs:392`, `Selections::load(..).ok()`):
    /// a selections file that cannot be read never stops a lecture from recording. It starts
    /// with no selection, and the file is left exactly as it was.
    #[tokio::test]
    async fn an_unreadable_selections_file_does_not_stop_the_lecture() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE);
        std::fs::write(&path, "{ not json").unwrap();
        let (selection, why) = load(&path, "Machine Learning").await;
        assert_eq!(selection, None, "the lecture starts with no selection");
        assert!(why.as_deref().is_some_and(|w| w.contains(FILE)), "and says why: {why:?}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ not json", "the file is left as it was");
        assert!(keep_selection(&path, "Machine Learning", &saved()).is_err(), "nothing is saved over it");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ not json");
        std::fs::remove_file(&path).unwrap();
        assert_eq!(load(&path, "Machine Learning").await, (None, None), "a missing file is no selection, silently");
    }

    /// Plan §J `capture_moved_is_saved_before_notice`: two courses are saved; a relocation for one
    /// arrives through the adapter; by the time the frontend sees the event, the file already
    /// holds the moved selection for that course and the other course exactly as it was — and the
    /// outcome arrives beside the event, typed, so the frontend never guesses.
    #[tokio::test]
    async fn capture_moved_is_saved_before_notice() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE);
        let mut all = Selections::default();
        let statistics = Selection { descriptor: Descriptor { bundle_id: None, app: "TextEdit".into(), title: "Lecture deck".into(), width: 1000, height: 700 }, region: Region::WHOLE, leave_out: vec![], sizes: vec![] };
        all.set("Machine Learning", saved());
        all.set("Statistics", statistics.clone());
        all.save(&path).unwrap();

        let moved = saved().with_size(1920, 1200, Region { x: 0.02, y: 0.03, w: 0.95, h: 0.9 });
        let ctx = Context { path: path.clone(), course: "Machine Learning".into(), saved: Some(saved()), current: None };
        let (core_tx, core_rx) = tokio::sync::mpsc::unbounded_channel();
        let (fe_tx, mut fe_rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(forward(Some(ctx), core_rx, fe_tx));
        core_tx.send(Event::CaptureMoved { selection: moved.clone(), note: "the slide moved with the window; watching it at the new size".into() }).unwrap();
        match fe_rx.recv().await.unwrap() {
            Forwarded { event: Event::CaptureMoved { note, .. }, capture_persistence: CapturePersistence::MovedSaved(kept) } => {
                assert_eq!(note, "the slide moved with the window; watching it at the new size");
                assert_eq!(kept, moved, "the persisted selection travels with the outcome");
            }
            other => panic!("{other:?}"),
        }
        // The save has already happened: the file, read now, holds the moved selection and the
        // untouched other course.
        let after = Selections::load(&path).unwrap();
        assert_eq!(after.get("Machine Learning"), Some(&moved), "core's own relocated selection, kept as it is");
        assert_eq!(after.get("Statistics"), Some(&statistics), "another course is never replaced by a one-course save");
    }

    /// A relocation that cannot be saved arrives with the failure typed beside it — no synthetic
    /// warning event, nothing for a frontend to infer from wording — and the event still arrives:
    /// the session found the region even though the preference could not be written.
    #[tokio::test]
    async fn a_save_failure_arrives_typed_with_the_event() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE);
        std::fs::write(&path, "{ not json").unwrap(); // unreadable: the save fails
        let ctx = Context { path, course: "Machine Learning".into(), saved: Some(saved()), current: None };
        let (core_tx, core_rx) = tokio::sync::mpsc::unbounded_channel();
        let (fe_tx, mut fe_rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(forward(Some(ctx), core_rx, fe_tx));
        core_tx.send(Event::CaptureMoved { selection: saved(), note: "found at the new size".into() }).unwrap();
        match fe_rx.recv().await.unwrap() {
            Forwarded { event: Event::CaptureMoved { .. }, capture_persistence: CapturePersistence::MovedSaveFailed { error } } => {
                assert!(error.starts_with("read "), "{error}");
            }
            other => panic!("the failure rides with the event: {other:?}"),
        }
        assert!(fe_rx.try_recv().is_err(), "exactly one forwarded thing: no warning event was invented");
        // nothing was written: the file is still the broken one it started as
        assert_eq!(std::fs::read_to_string(dir.path().join(FILE)).unwrap(), "{ not json");
    }

    /// Without a context the adapter only forwards: a scripted session with no capture never
    /// touches a file, and its events arrive in order with nothing beside them, with the channel
    /// closing only after the last.
    #[tokio::test]
    async fn without_a_context_nothing_is_saved_and_events_pass_through() {
        let (core_tx, core_rx) = tokio::sync::mpsc::unbounded_channel();
        let (fe_tx, mut fe_rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(forward(None, core_rx, fe_tx));
        core_tx.send(Event::CaptureMoved { selection: saved(), note: "n".into() }).unwrap();
        core_tx.send(Event::NothingNew).unwrap();
        drop(core_tx);
        match fe_rx.recv().await.unwrap() {
            Forwarded { event: Event::CaptureMoved { .. }, capture_persistence: CapturePersistence::None } => {}
            other => panic!("{other:?}"),
        }
        assert!(matches!(fe_rx.recv().await.unwrap().event, Event::NothingNew));
        assert!(fe_rx.recv().await.is_none(), "the channel closes only after the last event");
    }

    /// Identity is functional, never display: a candidate whose app and title contain what
    /// `plain::clean` would strip still binds against the same raw saved identity, and two raw
    /// identities that would clean to the same words are not the same window (plan Task 11).
    #[test]
    fn action_reads_raw_identity_not_display_identity() {
        // raw app/title, hostile but equal on both sides: binds
        let hostile = "Zoom\x1b]0;owned\x07Meeting";
        let mut raw_saved = saved();
        raw_saved.descriptor.app = hostile.into();
        raw_saved.descriptor.title = hostile.into();
        raw_saved.descriptor.bundle_id = None; // identity falls to the app name, the rawest form
        let raw_candidate = WindowInfo { id: 9, app: hostile.into(), bundle_id: None, title: hostile.into(), width: 1280, height: 720, on_screen: true };
        let asking = CaptureState::Asking { window: hostile.into(), reason: "resized".into(), candidates: vec![raw_candidate] };
        assert!(matches!(action(Some(&asking), Some(&raw_saved)), Action::Watch { .. }), "raw equals raw, whatever display would make of it");
        // display-equivalent but raw-different: never bound
        for (a, b) in [
            ("Zoom\x1b]0;a\x07Meeting", "Zoom\x1b]0;b\x07Meeting"), // both clean to “ZoomMeeting”
            ("Zoom\rMeeting", "Zoom\nMeeting"),                     // both clean to “Zoom\nMeeting”
        ] {
            assert_eq!(crate::plain::clean(a), crate::plain::clean(b), "the pair really is display-equivalent: {a:?} {b:?}");
            let mut saved_a = saved();
            saved_a.descriptor.bundle_id = None;
            saved_a.descriptor.app = "zoom.us".into();
            saved_a.descriptor.title = a.into();
            let candidate_b = WindowInfo { id: 9, app: "zoom.us".into(), bundle_id: None, title: b.into(), width: 1280, height: 720, on_screen: true };
            let asking = CaptureState::Asking { window: a.into(), reason: "resized".into(), candidates: vec![candidate_b] };
            assert_eq!(action(Some(&asking), Some(&saved_a)), Action::Refused(Refusal::AskingOther), "render identity is not bind identity: {a:?} vs {b:?}");
        }
    }

    /// The one shared sentence for a relocation that could not be kept: both truths, failure last.
    #[test]
    fn an_unsaved_relocation_says_both_truths() {
        let (kind, label, detail) = unsaved_words("read capture.json: not json");
        assert_eq!((kind, label), ("warn", "found again"));
        assert_eq!(detail, "Capture found the slide again, but the new region could not be saved: read capture.json: not json");
    }

    /// The host is named only when it is certainly Apple Terminal or iTerm (plan §H).
    #[test]
    fn the_host_is_narrowly_detected() {        assert_eq!(host_from(Some("Apple_Terminal")), "Terminal");
        assert_eq!(host_from(Some("iTerm.app")), "iTerm");
        assert_eq!(host_from(Some("iTerm2")), "iTerm");
        for other in [None, Some(""), Some("vscode"), Some("tmux"), Some("Hyper"), Some("apple_terminal")] {
            assert_eq!(host_from(other), "this terminal app", "{other:?}");
        }
    }

    /// Every variant has terminal words; none speaks of the desktop's slides strip.
    #[test]
    fn every_state_has_terminal_words() {
        let w = words();
        let cases: Vec<(CaptureState, &str, &str)> = vec![
            (CaptureState::Unbound, "slide", "no window"),
            (CaptureState::Watching { window: "Zoom Meeting".into() }, "slide", "watching"),
            (CaptureState::Paused { window: "Zoom Meeting".into(), reason: "the window is minimised".into() }, "warn", "paused"),
            (asking(vec![]), "warn", "asking"),
            (CaptureState::Denied, "warn", "screen recording"),
            (CaptureState::Failing { window: "Zoom Meeting".into(), reason: "3 failed captures in a row".into() }, "warn", "capture failing"),
        ];
        for (state, kind, label) in &cases {
            let (k, l, detail) = state_words(state, &w);
            assert_eq!((k, l), (*kind, *label), "{state:?}");
            assert!(!detail.contains("slides strip"), "{state:?}: {detail}");
            assert!(!detail.is_empty(), "{state:?}");
        }
        let (_, l, d) = state_words(&CaptureState::Unbound, &w);
        assert_eq!(l, "no window");
        assert_eq!(d, "No Zoom window chosen for Machine Learning yet: choose it once in the LectureLive app. Screenshots (⌘⇧4) still become slides.");
        let (_, _, d) = state_words(&CaptureState::Denied, &Words { course: "C".into(), host: "iTerm" });
        assert_eq!(d, "Screen Recording is off for iTerm: System Settings → Privacy & Security → Screen & System Audio Recording, then quit and reopen iTerm.");
        let (_, _, d) = state_words(&asking(vec![]), &w);
        assert_eq!(d, "it is not where it was; choose or update “Zoom Meeting” in the LectureLive app");
        let (k, l, d) = moved_words("the slide moved with the window; watching it at the new size");
        assert_eq!((k, l, d.as_str()), ("slide", "found again", "the slide moved with the window; watching it at the new size"));
    }

    /// Attention is typed: healthy watching is the only state that needs nobody.
    #[test]
    fn only_watching_needs_no_attention() {
        assert!(!attention(&CaptureState::Watching { window: "Zoom Meeting".into() }));
        for s in [CaptureState::Unbound, CaptureState::Denied, asking(vec![]), CaptureState::Paused { window: "Zoom Meeting".into(), reason: "minimised".into() }, CaptureState::Failing { window: "Zoom Meeting".into(), reason: "blank".into() }] {
            assert!(attention(&s), "{s:?}");
        }
    }

    /// Ctrl-S: watching captures now; the one offered candidate that is the saved window at a
    /// size it remembers is watched through exactly that region, with the parts left out and the
    /// other sizes kept; nothing else binds.
    #[test]
    fn ctrl_s_acts_only_on_a_safe_offering() {
        let full = Region { x: 0.05, y: 0.12, w: 0.9, h: 0.7 };
        assert_eq!(action(None, None), Action::Refused(Refusal::None));
        assert_eq!(action(Some(&CaptureState::Watching { window: "Zoom Meeting".into() }), None), Action::Now);
        let watch = action(Some(&asking(vec![at_1280(42)])), Some(&saved()));
        assert_eq!(watch, Action::Watch { window: 42, selection: saved().at_size(1280, 720).unwrap() });
        let Action::Watch { selection, .. } = watch else { panic!("{watch:?}") };
        assert_eq!(selection.region, full, "core's own saved region at that size");
        assert_eq!(selection.leave_out, saved().leave_out, "the parts left out are kept");
        assert_eq!(selection.sizes, vec![SizedRegion { width: 1600, height: 900, region: saved().region }], "the size it was at is remembered; the offered one is now live");
        assert_eq!((selection.descriptor.width, selection.descriptor.height, selection.descriptor.title.as_str()), (1280, 720, "Zoom Meeting"));
        // unsafe: nothing offered, two offered, a size with no region, another app, another title, no saved selection
        assert_eq!(action(Some(&asking(vec![])), Some(&saved())), Action::Refused(Refusal::AskingNone));
        assert_eq!(action(Some(&asking(vec![at_1280(42), at_1280(43)])), Some(&saved())), Action::Refused(Refusal::AskingMany));
        assert_eq!(action(Some(&asking(vec![win(42, Some("us.zoom.xos"), "zoom.us", "Zoom Meeting", 999, 600)])), Some(&saved())), Action::Refused(Refusal::AskingSize));
        assert_eq!(action(Some(&asking(vec![win(7, Some("com.google.Chrome"), "Google Chrome", "Zoom Meeting", 1280, 720)])), Some(&saved())), Action::Refused(Refusal::AskingOther));
        assert_eq!(action(Some(&asking(vec![win(7, Some("us.zoom.xos"), "zoom.us", "Zoom Workplace", 1280, 720)])), Some(&saved())), Action::Refused(Refusal::AskingOther));
        assert_eq!(action(Some(&asking(vec![at_1280(42)])), None), Action::Refused(Refusal::AskingOther), "nothing saved: nothing to watch through");
        for (s, r) in [
            (CaptureState::Unbound, Refusal::Unbound),
            (CaptureState::Paused { window: "Zoom Meeting".into(), reason: "gone".into() }, Refusal::Paused),
            (CaptureState::Denied, Refusal::Denied),
            (CaptureState::Failing { window: "Zoom Meeting".into(), reason: "blank".into() }, Refusal::Failing),
        ] {
            assert_eq!(action(Some(&s), Some(&saved())), Action::Refused(r), "{s:?}");
        }
        // core's identity rule: the bundle id decides when both have one; a selection saved
        // without one is matched by its app's name
        let no_bundle = Selection { descriptor: Descriptor { bundle_id: None, ..saved().descriptor.clone() }, ..saved() };
        assert_eq!(action(Some(&asking(vec![at_1280(42)])), Some(&no_bundle)), Action::Watch { window: 42, selection: no_bundle.at_size(1280, 720).unwrap() });
        // the same window at its saved size, within the 2% core allows, is safe too
        let near = win(42, Some("us.zoom.xos"), "zoom.us", "Zoom Meeting", 1290, 725);
        assert_eq!(action(Some(&asking(vec![near])), Some(&saved())), Action::Watch { window: 42, selection: saved().at_size(1290, 725).unwrap() });
    }

    /// `LECTURELIVE_RECORD` is taken verbatim, present or absent (spec §11).
    #[test]
    fn the_record_path_is_the_variable_verbatim() {
        assert_eq!(record_path(None), None);
        assert_eq!(record_path(Some("dir/with spaces".into())), Some(PathBuf::from("dir/with spaces")));
        let built = setup(Some(saved()), record_path(Some("/tmp/detector".into())));
        assert_eq!(built.record, Some(PathBuf::from("/tmp/detector")));
        assert_eq!(built.selection, Some(saved()));
        assert_eq!(built.interval, std::time::Duration::from_secs(1));
        assert_eq!(built.thresholds, Thresholds::default());
        assert_eq!(setup(None, record_path(None)).record, None);
    }

    /// Only the named course moves: a save preserves every other course in the file.
    #[test]
    fn keeping_one_course_preserves_the_others() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE);
        let mut all = Selections::default();
        all.set("m7-capture", saved());
        let real = Selection { descriptor: Descriptor { bundle_id: Some("us.zoom.xos".into()), app: "zoom.us".into(), title: "Real Meeting".into(), width: 1440, height: 810 }, region: Region::WHOLE, leave_out: vec![], sizes: vec![] };
        all.set("real-course", real.clone());
        all.save(&path).unwrap();
        let moved = saved().with_size(1920, 1200, Region { x: 0.0, y: 0.0, w: 1.0, h: 1.0 });
        keep_selection(&path, "m7-capture", &moved).unwrap();
        let after = Selections::load(&path).unwrap();
        assert_eq!(after.get("m7-capture"), Some(&moved));
        assert_eq!(after.get("real-course"), Some(&real));
    }
}
