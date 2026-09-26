//! The M5 gate's window cases (docs/milestones.md): occlusion, minimisation and window replacement
//! never rebind silently; resizes, failures and denial are said; every slide goes through one path. No network.
mod support;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use chrono::Local;
use lecturelive_core::capture::detect::{Region, Thresholds};
use lecturelive_core::capture::select::{Descriptor, Selection, SizedRegion};
use lecturelive_core::capture::worker::{self, CaptureCmd, CaptureEvent, CaptureState, WorkerConfig};
use lecturelive_core::notes::chat::{ChatClient, ChatConfig};
use lecturelive_core::notes::embeds::embeds_in;
use lecturelive_core::session::coordinator::SessionConfig;
use lecturelive_core::session::files::LectureFiles;
use lecturelive_core::session::folder;
use lecturelive_core::session::lecture::{self, CaptureSetup, Command, Event, Lecture, SlideWatch};
use lecturelive_core::session::segments;
use lecturelive_core::session::sidecar::Sidecar;
use lecturelive_core::session::spend::Spend;
use lecturelive_core::stt::rest::{spawn_recovery, RestClient, RestConfig};
use lecturelive_core::stt::stream::{self, SttConfig};
use serde_json::Value;
use support::fake_sse::{self, Reply};
use support::sources::Talking;
use support::windows::{slide, zoom_window, FakeWindows};
use support::{fake_rest, fake_stt};
use tokio::sync::mpsc;

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn selection(fake: &FakeWindows, id: u32) -> Selection {
    let s = fake.0.lock().unwrap();
    let (info, _) = s.windows.iter().find(|(i, _)| i.id == id).unwrap();
    Selection { descriptor: Descriptor::of(info), region: Region { x: 0.05, y: 0.1, w: 0.9, h: 0.85 }, leave_out: vec![], sizes: vec![] }
}

fn start(fake: &FakeWindows, sel: Option<Selection>, dir: &Path) -> (worker::CaptureHandle, mpsc::UnboundedReceiver<CaptureEvent>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let cfg = WorkerConfig { slides: dir.to_path_buf(), interval: ms(20), thresholds: Thresholds::default(), record: None };
    (worker::spawn(Box::new(fake.clone()), sel, cfg, tx), rx)
}

/// Collects events for `ms`: the states in order, and each saved slide's (auto, uncertain).
async fn watch(rx: &mut mpsc::UnboundedReceiver<CaptureEvent>, ms: u64) -> (Vec<CaptureState>, Vec<(bool, bool)>) {
    let (mut states, mut shots) = (Vec::new(), Vec::new());
    let end = tokio::time::Instant::now() + Duration::from_millis(ms);
    while let Ok(Some(e)) = tokio::time::timeout_at(end, rx.recv()).await {
        match e {
            CaptureEvent::State(s) => states.push(s),
            CaptureEvent::Relocated { .. } => {} // said by the app; the slides are what these tests count
            CaptureEvent::Captured(c) => {
                assert!(c.path.exists());
                shots.push((c.auto, c.uncertain));
            }
        }
    }
    (states, shots)
}

/// Collects events until the worker says it moved to another region (up to `ms`), then for the samples
/// that settle the kept frame: the states, the slides, and whether it moved.
async fn until_moved(rx: &mut mpsc::UnboundedReceiver<CaptureEvent>, ms: u64) -> (Vec<CaptureState>, Vec<(bool, bool)>, bool) {
    let (mut states, mut shots, mut moved) = (Vec::new(), Vec::new(), false);
    let mut end = tokio::time::Instant::now() + Duration::from_millis(ms);
    while let Ok(Some(e)) = tokio::time::timeout_at(end, rx.recv()).await {
        match e {
            CaptureEvent::State(s) => states.push(s),
            CaptureEvent::Captured(c) => shots.push((c.auto, c.uncertain)),
            CaptureEvent::Relocated { .. } if !moved => {
                moved = true;
                end = tokio::time::Instant::now() + Duration::from_millis(200); // the settling samples
            }
            CaptureEvent::Relocated { .. } => {}
        }
    }
    (states, shots, moved)
}

fn watching(s: &CaptureState) -> bool {
    matches!(s, CaptureState::Watching { .. })
}

#[tokio::test]
async fn a_matching_window_binds_at_start_and_each_settled_slide_is_saved_once() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeWindows::default();
    fake.add(42, "Zoom Meeting", 1600, 900, slide(1));
    let (_h, mut rx) = start(&fake, Some(selection(&fake, 42)), dir.path());
    let (states, shots) = watch(&mut rx, 300).await;
    assert!(states.iter().any(watching), "{states:?}");
    assert_eq!(shots, vec![(true, false)], "the first frame of the session");
    fake.show(42, slide(2));
    let (_, shots) = watch(&mut rx, 300).await;
    assert_eq!(shots, vec![(true, false)], "the build, once");
}

#[tokio::test]
async fn occlusion_changes_nothing_minimising_pauses_and_the_same_window_resumes_without_a_duplicate() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeWindows::default();
    fake.add(42, "Zoom Meeting", 1600, 900, slide(1));
    let (_h, mut rx) = start(&fake, Some(selection(&fake, 42)), dir.path());
    watch(&mut rx, 200).await;
    // Covered by another window: the window's own buffer is unchanged (CGWindowListCreateImage, OptionIncludingWindow).
    let (_, shots) = watch(&mut rx, 200).await;
    assert!(shots.is_empty());
    fake.on_screen(42, false);
    let (states, shots) = watch(&mut rx, 200).await;
    assert!(matches!(states.last(), Some(CaptureState::Paused { reason, .. }) if reason.contains("not on screen")), "{states:?}");
    assert!(shots.is_empty());
    fake.on_screen(42, true);
    let (states, shots) = watch(&mut rx, 200).await;
    assert!(states.last().is_some_and(watching));
    assert!(shots.is_empty(), "the same slide is not captured again after the pause");
}

#[tokio::test]
async fn a_replaced_window_asks_and_nothing_is_captured_from_it_until_the_person_binds_it() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeWindows::default();
    fake.add(42, "Zoom Meeting", 1600, 900, slide(1));
    let sel = selection(&fake, 42);
    let (h, mut rx) = start(&fake, Some(sel.clone()), dir.path());
    watch(&mut rx, 200).await;
    fake.close(42);
    let (states, _) = watch(&mut rx, 150).await;
    assert!(matches!(states.last(), Some(CaptureState::Paused { reason, .. }) if reason.contains("closed")), "{states:?}");
    fake.add(77, "Zoom Meeting", 1600, 900, slide(3));
    let (states, shots) = watch(&mut rx, 300).await;
    let Some(CaptureState::Asking { candidates, .. }) = states.last() else { panic!("{states:?}") };
    assert_eq!(candidates.iter().map(|c| c.id).collect::<Vec<_>>(), vec![77]);
    assert!(shots.is_empty(), "no silent rebinding");
    h.send(CaptureCmd::Bind { window: 77, selection: sel });
    let (states, shots) = watch(&mut rx, 300).await;
    assert!(states.last().is_some_and(watching));
    assert_eq!(shots, vec![(true, false)], "the new window's slide, once it is chosen");
}


/// Watching Zoom in full screen and working in another window: the window is on another desktop, off
/// screen, and still captured (M5, the person's use).
#[tokio::test]
async fn a_window_on_another_desktop_is_still_watched() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeWindows::default();
    fake.add(42, "Zoom Meeting", 1600, 900, slide(1));
    let (_h, mut rx) = start(&fake, Some(selection(&fake, 42)), dir.path());
    watch(&mut rx, 200).await;
    fake.0.lock().unwrap().off_screen_capture = true;
    fake.on_screen(42, false);
    fake.show(42, slide(2));
    let (states, shots) = watch(&mut rx, 300).await;
    assert!(!states.iter().any(|s| matches!(s, CaptureState::Paused { .. })), "{states:?}");
    assert_eq!(shots, vec![(true, false)], "the slide shown while the person works elsewhere");
}

/// Final review, I1: a build that lands as the window changes size (Zoom going full screen as a share starts)
/// is a new slide, not the kept one found again.
#[tokio::test]
async fn a_build_that_lands_as_the_window_changes_size_is_taken() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeWindows::default();
    let (windowed, full) = (zoom_window(800, 450, (80, 60, 560, 315), 1), zoom_window(1280, 800, (160, 120, 960, 540), 2));
    fake.add(42, "Zoom Meeting", 800, 450, windowed);
    let mut sel = selection(&fake, 42);
    sel.region = Region { x: 80.0 / 800.0, y: 60.0 / 450.0, w: 560.0 / 800.0, h: 315.0 / 450.0 };
    let (_h, mut rx) = start(&fake, Some(sel), dir.path());
    let (_, shots) = watch(&mut rx, 200).await;
    assert_eq!(shots.len(), 1);
    fake.resize(42, 1280, 800);
    fake.show(42, full);
    let (_, shots, moved) = until_moved(&mut rx, 10_000).await;
    assert!(moved, "the slide was found again in full screen");
    let (_, more) = watch(&mut rx, 300).await;
    assert_eq!([shots, more].concat(), vec![(true, false)], "the build, taken once");
}

/// Final review, I1: back at a size whose region is known, a slide that changed meanwhile is taken.
#[tokio::test]
async fn a_slide_that_changes_as_the_window_returns_to_a_known_size_is_taken() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeWindows::default();
    let (windowed, full) = (zoom_window(800, 450, (80, 60, 560, 315), 1), zoom_window(1280, 800, (160, 120, 960, 540), 3));
    fake.add(42, "Zoom Meeting", 800, 450, windowed);
    let mut sel = selection(&fake, 42);
    sel.region = Region { x: 80.0 / 800.0, y: 60.0 / 450.0, w: 560.0 / 800.0, h: 315.0 / 450.0 };
    sel.sizes = vec![SizedRegion { width: 1280, height: 800, region: Region { x: 160.0 / 1280.0, y: 120.0 / 800.0, w: 960.0 / 1280.0, h: 540.0 / 800.0 } }];
    let (_h, mut rx) = start(&fake, Some(sel), dir.path());
    let (_, shots) = watch(&mut rx, 200).await;
    assert_eq!(shots.len(), 1);
    fake.resize(42, 1280, 800);
    fake.show(42, full);
    let (_, shots, moved) = until_moved(&mut rx, 10_000).await;
    assert!(moved, "the region known for full screen");
    let (_, more) = watch(&mut rx, 300).await;
    assert_eq!([shots, more].concat(), vec![(true, false)], "the new slide, taken once");
}

/// Final review, I3: a search that failed because the new layout had no slide yet (a gallery first) is tried
/// again, and the slide is found once it appears.
#[tokio::test]
async fn a_failed_search_is_tried_again_and_finds_the_slide_when_it_appears() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeWindows::default();
    let gallery = image::RgbaImage::from_fn(1280, 800, |x, y| if (x / 320 + y / 400) % 2 == 0 { image::Rgba([60, 70, 80, 255]) } else { image::Rgba([28, 28, 30, 255]) });
    let (windowed, full) = (zoom_window(800, 450, (80, 60, 560, 315), 1), zoom_window(1280, 800, (160, 120, 960, 540), 1));
    fake.add(42, "Zoom Meeting", 800, 450, windowed);
    let mut sel = selection(&fake, 42);
    sel.region = Region { x: 80.0 / 800.0, y: 60.0 / 450.0, w: 560.0 / 800.0, h: 315.0 / 450.0 };
    let (_h, mut rx) = start(&fake, Some(sel), dir.path());
    watch(&mut rx, 200).await;
    fake.resize(42, 1280, 800);
    fake.show(42, gallery);
    let end = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut asked = false;
    while !asked && tokio::time::Instant::now() < end {
        asked = watch(&mut rx, 100).await.0.iter().any(|s| matches!(s, CaptureState::Asking { .. }));
    }
    assert!(asked, "no slide in the gallery: asks");
    fake.show(42, full);
    let (states, shots, moved) = until_moved(&mut rx, 10_000).await;
    assert!(moved, "found once the slide is shared again");
    assert!(shots.is_empty(), "the same slide: {shots:?}");
    assert!(states.last().is_some_and(watching), "{states:?}");
}

/// Live evidence (M5 full-screen check): while full screen animates, the window is off screen and one capture
/// comes back empty; like any one bad frame, that is not worth a word.
#[tokio::test]
async fn one_failed_capture_while_moving_to_another_desktop_is_not_a_pause() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeWindows::default();
    fake.add(42, "Zoom Meeting", 1600, 900, slide(1));
    let (_h, mut rx) = start(&fake, Some(selection(&fake, 42)), dir.path());
    watch(&mut rx, 200).await;
    {
        let mut s = fake.0.lock().unwrap();
        s.off_screen_capture = true;
        s.fail_next = 1;
        s.windows[0].0.on_screen = false;
    }
    fake.show(42, slide(2));
    let (states, shots) = watch(&mut rx, 300).await;
    assert!(!states.iter().any(|s| matches!(s, CaptureState::Paused { .. })), "{states:?}");
    assert_eq!(shots, vec![(true, false)], "the slide shown once the move is over");
}

/// Entering full screen re-lays out Zoom's window: the last kept slide is found again, watching goes on
/// without a question or a duplicate, and leaving full screen goes back to the region for that size.
#[tokio::test]
async fn entering_full_screen_finds_the_slide_again_and_leaving_it_needs_no_search() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeWindows::default();
    let windowed = |lines| zoom_window(800, 450, (80, 60, 560, 315), lines);
    let full = |lines| zoom_window(1280, 800, (160, 120, 960, 540), lines);
    // Drawn before each resize: drawing in a test build outlasts the samples after which the worker looks.
    let (full_1, windowed_2) = (full(1), windowed(2));
    fake.add(42, "Zoom Meeting", 800, 450, windowed(1));
    let mut sel = selection(&fake, 42);
    sel.region = Region { x: 80.0 / 800.0, y: 60.0 / 450.0, w: 560.0 / 800.0, h: 315.0 / 450.0 };
    let (_h, mut rx) = start(&fake, Some(sel), dir.path());
    let (_, shots) = watch(&mut rx, 200).await;
    assert_eq!(shots.len(), 1);
    fake.resize(42, 1280, 800);
    fake.show(42, full_1);
    let (states, shots, moved) = until_moved(&mut rx, 10_000).await;
    assert!(moved, "the slide was found again in full screen");
    assert!(!states.iter().any(|s| matches!(s, CaptureState::Asking { .. } | CaptureState::Paused { .. })), "{states:?}");
    assert!(shots.is_empty(), "the same slide is not taken again: {shots:?}");
    fake.show(42, full(2));
    let (_, shots) = watch(&mut rx, 300).await;
    assert_eq!(shots, vec![(true, false)], "a build in full screen, through the region found");
    fake.resize(42, 800, 450);
    fake.show(42, windowed_2);
    let (states, shots, moved) = until_moved(&mut rx, 10_000).await;
    assert!(moved, "back to the region for the windowed size");
    assert!(!states.iter().any(|s| matches!(s, CaptureState::Asking { .. })), "{states:?}");
    assert!(shots.is_empty(), "back in the window, the same slide: {shots:?}");
}

/// Live evidence (M5 full-screen check): on a display barely larger than the window, full screen took it from
/// 1166 × 720 to 1168 × 729, within the 2% that names the same window, yet the slide moved in it.
#[tokio::test]
async fn a_resize_within_the_same_size_that_moves_the_slide_takes_it_once() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeWindows::default();
    // The slide clears the participant video in the corner: a video drawn over the slide that moves against it
    // at a switch is a change, as it would be at any other time (a camera over the slide is left out).
    let windowed = |lines| zoom_window(1166, 720, (60, 40, 880, 495), lines);
    let full = |lines| zoom_window(1168, 729, (64, 48, 880, 495), lines);
    let full_1 = full(1); // drawn before the resize, as above
    fake.add(42, "Zoom Meeting", 1166, 720, windowed(1));
    let mut sel = selection(&fake, 42);
    sel.region = Region { x: 60.0 / 1166.0, y: 40.0 / 720.0, w: 880.0 / 1166.0, h: 495.0 / 720.0 };
    let (_h, mut rx) = start(&fake, Some(sel), dir.path());
    let (_, shots) = watch(&mut rx, 200).await;
    assert_eq!(shots.len(), 1);
    fake.resize(42, 1168, 729);
    fake.show(42, full_1);
    let (states, shots, moved) = until_moved(&mut rx, 3_000).await;
    assert!(shots.is_empty(), "the same slide is not taken again: {shots:?}");
    assert!(moved, "the new size is watched through the region it already had");
    assert!(!states.iter().any(|s| matches!(s, CaptureState::Asking { .. } | CaptureState::Paused { .. })), "{states:?}");
    fake.show(42, full(2));
    let (_, shots) = watch(&mut rx, 300).await;
    assert_eq!(shots, vec![(true, false)], "a build at the new size");
}

/// Live evidence (M5 full-screen check, second run): a new window was still settling its size (1166 × 718, then
/// 1168 × 720) as watching began; the re-layout came before the first slide, which is still taken, once.
#[tokio::test]
async fn a_size_change_before_the_first_slide_still_takes_it() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeWindows::default();
    let (before, after) = (zoom_window(1166, 718, (60, 40, 1040, 585), 1), zoom_window(1168, 720, (61, 41, 1040, 585), 1));
    fake.add(42, "Zoom Meeting", 1166, 718, before);
    let mut sel = selection(&fake, 42);
    sel.region = Region { x: 60.0 / 1166.0, y: 40.0 / 718.0, w: 1040.0 / 1166.0, h: 585.0 / 718.0 };
    fake.0.lock().unwrap().fail_next = 10; // nothing to capture yet, so no first slide before the size settles
    let (_h, mut rx) = start(&fake, Some(sel), dir.path());
    let (_, early) = watch(&mut rx, 60).await; // bound, with nothing captured yet
    fake.resize(42, 1168, 720);
    fake.show(42, after);
    let (_, mut shots) = watch(&mut rx, 800).await;
    shots.splice(0..0, early);
    assert_eq!(shots, vec![(true, false)], "the first slide, once");
}

/// A window a few pixels off its saved size is the same window at start: its first slide is taken, with no
/// relocation, since only a change of size while watching moves the slide.
#[tokio::test]
async fn a_window_near_its_saved_size_at_start_takes_its_first_slide() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeWindows::default();
    fake.add(42, "Zoom Meeting", 1168, 729, zoom_window(1168, 729, (64, 48, 1040, 585), 1));
    let mut sel = selection(&fake, 42);
    sel.descriptor.width = 1166;
    sel.descriptor.height = 720;
    sel.region = Region { x: 64.0 / 1168.0, y: 48.0 / 729.0, w: 1040.0 / 1168.0, h: 585.0 / 729.0 };
    let (_h, mut rx) = start(&fake, Some(sel), dir.path());
    let (states, shots, moved) = until_moved(&mut rx, 500).await;
    assert_eq!(shots, vec![(true, false)], "the first slide");
    assert!(!moved && states.iter().any(watching), "{states:?}");
}

/// Live evidence (M5 capture check): a closed window's id can stay listed, off screen, while its app runs.
#[tokio::test]
async fn a_replacement_whose_old_window_lingers_off_screen_still_asks() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeWindows::default();
    fake.add(42, "Zoom Meeting", 1600, 900, slide(1));
    let (_h, mut rx) = start(&fake, Some(selection(&fake, 42)), dir.path());
    watch(&mut rx, 200).await;
    fake.on_screen(42, false);
    fake.add(77, "Zoom Meeting", 1600, 900, slide(3));
    let (states, shots) = watch(&mut rx, 300).await;
    let Some(CaptureState::Asking { candidates, .. }) = states.last() else { panic!("{states:?}") };
    assert_eq!(candidates.iter().map(|c| c.id).collect::<Vec<_>>(), vec![77]);
    assert!(shots.is_empty(), "no silent rebinding");
}

/// Live evidence (M5, a recorded Zoom lecture): all false captures were the speaker's camera drawn over
/// the slide. A part left out never triggers a slide, and the rest of the slide still does.
#[tokio::test]
async fn a_camera_left_out_takes_no_slides_and_a_build_still_does() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeWindows::default();
    let with_camera = |lines: u32, cam: u8| {
        let mut img = slide(lines);
        for y in 36..120 {
            for x in 500..608 {
                img.put_pixel(x, y, image::Rgba([cam, cam, cam, 255]));
            }
        }
        img
    };
    fake.add(42, "Zoom Meeting", 1600, 900, with_camera(1, 40));
    let mut sel = selection(&fake, 42);
    sel.leave_out = vec![Region { x: 0.8, y: 0.0, w: 0.2, h: 0.3 }];
    let (_h, mut rx) = start(&fake, Some(sel), dir.path());
    let (_, shots) = watch(&mut rx, 200).await;
    assert_eq!(shots.len(), 1, "the first frame");
    for k in 0..6u8 {
        fake.show(42, with_camera(1, 40 + k * 30)); // the lecturer moves, and stays still a moment
        let (_, shots) = watch(&mut rx, 60).await;
        assert!(shots.is_empty(), "the camera is left out: {shots:?}");
    }
    fake.show(42, with_camera(2, 90));
    let (_, shots) = watch(&mut rx, 300).await;
    assert_eq!(shots, vec![(true, false)], "the build");
}

#[tokio::test]
async fn no_window_at_start_asks_and_a_later_window_waits_for_the_person() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeWindows::default();
    fake.add(42, "Zoom Meeting", 1600, 900, slide(1));
    let sel = selection(&fake, 42);
    fake.close(42);
    let (_h, mut rx) = start(&fake, Some(sel), dir.path());
    let (states, _) = watch(&mut rx, 150).await;
    assert!(matches!(states.last(), Some(CaptureState::Asking { candidates, .. }) if candidates.is_empty()), "{states:?}");
    fake.add(50, "Zoom Meeting", 1600, 900, slide(1));
    let (states, shots) = watch(&mut rx, 300).await;
    assert!(matches!(states.last(), Some(CaptureState::Asking { candidates, .. }) if candidates.len() == 1), "{states:?}");
    assert!(shots.is_empty());
}

#[tokio::test]
async fn a_resized_window_pauses_and_asks() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeWindows::default();
    fake.add(42, "Zoom Meeting", 1600, 900, slide(1));
    let (_h, mut rx) = start(&fake, Some(selection(&fake, 42)), dir.path());
    watch(&mut rx, 200).await;
    // Zoom's gallery view: no slide anywhere, so the region cannot be found again. Drawn before the resize, since
    // drawing it in a test build outlasts the two samples after which the worker searches.
    let gallery = image::RgbaImage::from_fn(1280, 800, |x, y| if (x / 320 + y / 400) % 2 == 0 { image::Rgba([60, 70, 80, 255]) } else { image::Rgba([28, 28, 30, 255]) });
    fake.resize(42, 1280, 800);
    fake.show(42, gallery);
    // One search for the slide at the new size first: its length depends on the machine's load.
    let (mut states, mut shots) = (Vec::new(), Vec::new());
    let end = tokio::time::Instant::now() + Duration::from_secs(10);
    while !states.iter().any(|s| matches!(s, CaptureState::Asking { .. })) && tokio::time::Instant::now() < end {
        let (s, c) = watch(&mut rx, 200).await;
        states.extend(s);
        shots.extend(c);
    }
    // Final review, I2: no window to "Watch it" through a region nobody has checked at this size.
    assert!(matches!(states.last(), Some(CaptureState::Asking { reason, candidates, .. }) if reason.contains("1280 × 800") && candidates.is_empty()), "{states:?}");
    assert!(shots.is_empty(), "Zoom's re-laid-out window is not captured through the old region");
}

#[tokio::test]
async fn blank_and_failed_captures_are_not_slides() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeWindows::default();
    fake.add(42, "Zoom Meeting", 1600, 900, slide(1));
    let (_h, mut rx) = start(&fake, Some(selection(&fake, 42)), dir.path());
    watch(&mut rx, 200).await;
    fake.0.lock().unwrap().blank_next = 2;
    let (states, shots) = watch(&mut rx, 200).await;
    assert!(shots.is_empty() && !states.iter().any(|s| matches!(s, CaptureState::Failing { .. })), "two blanks are not yet a failure: {states:?}");
    fake.0.lock().unwrap().fail_next = 5;
    let (states, shots) = watch(&mut rx, 300).await;
    assert!(states.iter().any(|s| matches!(s, CaptureState::Failing { .. })), "{states:?}");
    assert!(states.last().is_some_and(watching), "and it recovers");
    assert!(shots.is_empty(), "the slide on screen is not captured again");
}

#[tokio::test]
async fn denial_is_said_and_manual_capture_says_why_it_cannot() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeWindows::default();
    fake.add(42, "Zoom Meeting", 1600, 900, slide(1));
    let sel = selection(&fake, 42);
    fake.0.lock().unwrap().denied = true;
    let (h, mut rx) = start(&fake, Some(sel), dir.path());
    let (states, _) = watch(&mut rx, 150).await;
    assert_eq!(states.last(), Some(&CaptureState::Denied));
    let (tx, reply) = tokio::sync::oneshot::channel();
    h.send(CaptureCmd::Now(tx));
    assert!(reply.await.unwrap().unwrap_err().contains("No window is being watched"));
}

#[tokio::test]
async fn manual_capture_keeps_the_frame_so_auto_does_not_take_it_again() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeWindows::default();
    fake.add(42, "Zoom Meeting", 1600, 900, slide(1));
    let (h, mut rx) = start(&fake, Some(selection(&fake, 42)), dir.path());
    watch(&mut rx, 200).await;
    fake.show(42, slide(2));
    let (tx, reply) = tokio::sync::oneshot::channel();
    h.send(CaptureCmd::Now(tx));
    reply.await.unwrap().unwrap();
    let (_, shots) = watch(&mut rx, 300).await;
    assert_eq!(shots, vec![(false, false)], "the manual slide, and no auto copy of it");
}

// A whole lecture with capture, through the fakes of the M3 gate.

const TITLE: &str = "# Machine Learning — Week 01 — Optimisation — 2026-09-25";
const NAME: &str = "Week 01 — Optimisation";

fn user_text(body: &Value) -> String {
    let c = &body["messages"][1]["content"];
    c.as_str().map(str::to_string).unwrap_or_else(|| c[0]["text"].as_str().unwrap_or_default().to_string())
}

/// The model, played by the fake: notes that place every embed they are given.
fn respond(body: &Value) -> Reply {
    let embeds: Vec<String> = user_text(body).lines().filter(|l| l.starts_with("![Slide ")).map(str::to_string).collect();
    fake_sse::answer(&format!("## Gradient descent\n- Steps against the gradient.\n{}", embeds.join("\n")), 1_000_000)
}

async fn until(events: &mut mpsc::UnboundedReceiver<Event>, what: &str, want: impl Fn(&Event) -> bool) -> Event {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let e = tokio::time::timeout_at(deadline, events.recv()).await.unwrap_or_else(|_| panic!("no {what} within 30 s")).expect("the lecture is running");
        if let Event::SnapshotFailed(m) = &e {
            panic!("waiting for {what}: {m}");
        }
        if want(&e) {
            return e;
        }
    }
}

#[tokio::test]
async fn auto_slides_and_the_watcher_share_one_registration() {
    let dir = tempfile::tempdir().unwrap();
    let f = LectureFiles::standard(dir.path(), chrono::NaiveDate::from_ymd_opt(2026, 9, 25).unwrap());
    let ledger = dir.path().join("spend.jsonl");
    folder::open(&f, TITLE, false).unwrap();
    segments::session_marker(&f.transcript, Local::now()).unwrap();
    let stt = fake_stt::start(fake_stt::Config::default()).await;
    let rest = fake_rest::start(None).await;
    let sse = fake_sse::start(respond).await;
    let spend = Spend::open(&ledger, "Machine Learning", NAME, f.date).unwrap();
    let chat = ChatClient::new(ChatConfig { url: sse.url.clone(), idle_timeout: ms(5_000), ..ChatConfig::new("test-key".into()) }, Some(spend.clone())).unwrap();
    let lec = Arc::new(Lecture { files: f.clone(), course: "Machine Learning".into(), name: NAME.into(), title: TITLE.into(), chat, spend: spend.clone() });
    let stt_cfg = SttConfig { url: stt.url.clone(), backoff_unit: ms(1), connect_timeout: ms(2_000), send_timeout: ms(2_000), idle_timeout: ms(2_000), finalize_wait: ms(2_000), done_wait: ms(2_000), ..SttConfig::new("test-key".into(), vec![]) };
    let rest_cfg = RestConfig { url: rest.url.clone(), retry_unit: ms(1), request_timeout: ms(5_000), file_wait: ms(5_000), ..RestConfig::new("test-key".into(), vec![]) };
    let session = SessionConfig { dir: f.dir.clone(), stem: f.stem.clone(), stt: Some(stream::spawn(stt_cfg).unwrap()), recovery: Some(spawn_recovery(RestClient::new(rest_cfg).unwrap())), spend: Some(spend), ..Default::default() };

    let fake = FakeWindows::default();
    fake.add(42, "Zoom Meeting", 1600, 900, slide(1));
    let capture = CaptureSetup { source: Box::new(fake.clone()), selection: Some(selection(&fake, 42)), interval: ms(20), thresholds: Thresholds::default(), record: None };
    let (cmd, cmd_rx) = mpsc::unbounded_channel();
    let (ev_tx, mut ev) = mpsc::unbounded_channel();
    let source = Talking { pace: ms(2), fake: stt.state.clone() };
    let run = tokio::spawn(lecture::run(lec, session, Box::new(source), SlideWatch { screenshots: None, poll: ms(20) }, Some(capture), cmd_rx, ev_tx));

    until(&mut ev, "the first auto slide", |e| matches!(e, Event::Slide { index: 1, auto: true, .. })).await;
    fake.show(42, slide(2));
    until(&mut ev, "the build", |e| matches!(e, Event::Slide { index: 2, auto: true, .. })).await;
    let (tx, reply) = tokio::sync::oneshot::channel();
    cmd.send(Command::CaptureNow(tx)).unwrap();
    reply.await.unwrap().unwrap();
    until(&mut ev, "the manual slide", |e| matches!(e, Event::Slide { index: 3, auto: false, .. })).await;
    let outside = dir.path().join("board.png");
    support::slides::png(&outside);
    cmd.send(Command::Import(vec![outside.clone()])).unwrap();
    until(&mut ev, "the dropped image", |e| matches!(e, Event::Slide { index: 4, auto: false, .. })).await;
    tokio::time::sleep(ms(200)).await; // the watcher lists slides/ several times
    cmd.send(Command::Stop).unwrap();
    tokio::time::timeout(Duration::from_secs(10), run).await.expect("the lecture ends within 10 s of the stop").unwrap().unwrap();

    let sc = Sidecar::load(&f.sidecar()).unwrap().unwrap();
    assert_eq!(sc.slides.iter().map(|s| (s.index, s.auto)).collect::<Vec<_>>(), vec![(1, true), (2, true), (3, false), (4, false)]);
    let names: Vec<String> = std::fs::read_dir(&f.slides).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(names.len(), 4, "{names:?}");
    assert!(outside.exists(), "the dropped original stays");
    let notes = std::fs::read_to_string(&f.notes).unwrap();
    for n in 1..=4 {
        assert_eq!(notes.matches(&format!("![Slide {n}](slides/slide_{n:02}_")).count(), 1, "{notes}");
    }
    assert_eq!(embeds_in(&notes).len(), 4);
}
