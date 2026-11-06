//! M8: real worker lifecycle with local scripted windows; no recording or network.
mod support;
use std::time::Duration;
use chrono::Local;
use lecturelive_core::capture::{detect::{Region, Thresholds}, select::{Descriptor, Selection, zoom_presentation}, worker::{self, CaptureCmd, CaptureEvent, CaptureState, WorkerConfig}};
use support::windows::{FakeWindows, zoom_window, notice};
use tokio::sync::mpsc;

fn picture(n: u32) -> image::RgbaImage { zoom_window(800, 600, (40, 100, 640, 360), n) }
fn chosen(fake: &FakeWindows) -> Selection {
    Selection { descriptor: Descriptor::of(&fake.0.lock().unwrap().windows[0].0), region: Region { x: 0.05, y: 1.0/6.0, w: 0.8, h: 0.6 }, leave_out: vec![], sizes: vec![] }
}
fn start(fake: &FakeWindows, sel: Option<Selection>, dir: &std::path::Path) -> (worker::CaptureHandle, mpsc::UnboundedReceiver<CaptureEvent>) {
    let (tx, rx) = mpsc::unbounded_channel();
    (worker::spawn(Box::new(fake.clone()), sel, WorkerConfig { slides: dir.into(), interval: Duration::from_millis(20), thresholds: Thresholds::default(), record: None }, tx), rx)
}
async fn until(rx: &mut mpsc::UnboundedReceiver<CaptureEvent>, f: impl Fn(&CaptureEvent) -> bool) -> CaptureEvent {
    let end = tokio::time::Instant::now() + Duration::from_secs(8);
    loop {
        let e = tokio::time::timeout_at(end, rx.recv()).await.expect("worker event deadline").expect("worker alive");
        if f(&e) { return e; }
    }
}
async fn quiet(rx: &mut mpsc::UnboundedReceiver<CaptureEvent>) {
    let end = tokio::time::Instant::now() + Duration::from_millis(220);
    while let Ok(Some(e)) = tokio::time::timeout_at(end, rx.recv()).await { assert!(!matches!(e, CaptureEvent::Captured(_)), "{e:?}"); }
}

#[test]
fn automatic_identity_never_accepts_home_chat_settings_or_name_spoofing() {
    let fake = FakeWindows::default(); fake.add(1, "Zoom Meeting", 800, 600, picture(1));
    let mut w = fake.0.lock().unwrap().windows[0].0.clone();
    assert!(zoom_presentation(&w, false));
    for title in ["Zoom Workplace", "Chat", "Settings", "Meeting Settings", "", "Zoom Meeting - Settings"] {
        w.title = title.into(); assert!(!zoom_presentation(&w, false), "{title}");
    }
    assert!(!zoom_presentation(&w, true));
    w.title.clear(); assert!(zoom_presentation(&w, true));
    w.title = "Zoom Meeting".into(); w.bundle_id = None; assert!(!zoom_presentation(&w, true));
    w.bundle_id = Some("test.spoof".into()); assert!(!zoom_presentation(&w, true));
}

#[tokio::test]
async fn discovers_first_slide_and_publishes_a_verified_selection_without_manual_setup() {
    let d = tempfile::tempdir().unwrap(); let f = FakeWindows::default();
    f.add(1, "Zoom Workplace", 800, 600, picture(1));
    let (_h, mut rx) = start(&f, None, d.path()); quiet(&mut rx).await;
    assert_eq!(f.0.lock().unwrap().captures, 0, "home pixels never inspected");
    f.add(2, "Zoom Meeting", 800, 600, picture(1));
    let CaptureEvent::Relocated { selection, .. } = until(&mut rx, |e| matches!(e, CaptureEvent::Relocated { .. })).await else { unreachable!() };
    assert_eq!(selection.descriptor.title, "Zoom Meeting"); assert!(selection.region.w < 0.95);
    until(&mut rx, |e| matches!(e, CaptureEvent::Mode(m) if m.selected)).await;
    let CaptureEvent::Captured(c) = until(&mut rx, |e| matches!(e, CaptureEvent::Captured(_))).await else { unreachable!() };
    assert!(c.auto); assert!(c.path.exists()); quiet(&mut rx).await;
}

#[tokio::test]
async fn ambiguity_waits_then_recovers_without_a_choice_when_one_presentation_closes() {
    let d = tempfile::tempdir().unwrap(); let f = FakeWindows::default();
    f.add(1, "Zoom Meeting", 800, 600, picture(1)); f.add(2, "Screen Sharing", 800, 600, picture(2));
    let (_h, mut rx) = start(&f, None, d.path());
    until(&mut rx, |e| matches!(e, CaptureEvent::State(CaptureState::Asking { reason, .. }) if reason.contains("More than one"))).await;
    quiet(&mut rx).await; f.close(1);
    until(&mut rx, |e| matches!(e, CaptureEvent::Captured(_))).await;
    quiet(&mut rx).await;
}

#[tokio::test]
async fn titleless_fullscreen_replacement_verifies_the_kept_slide_without_duplicate() {
    let d = tempfile::tempdir().unwrap(); let f = FakeWindows::default(); f.add(1, "Zoom Meeting", 800, 600, picture(1));
    let (_h, mut rx) = start(&f, Some(chosen(&f)), d.path());
    until(&mut rx, |e| matches!(e, CaptureEvent::Captured(_))).await;
    f.close(1); f.add(2, "", 1000, 700, zoom_window(1000, 700, (50, 110, 800, 450), 1));
    let CaptureEvent::Relocated { selection, .. } = until(&mut rx, |e| matches!(e, CaptureEvent::Relocated { selection, .. } if selection.descriptor.width == 1000)).await else { unreachable!() };
    assert_eq!(selection.descriptor.title, "Zoom Meeting");
    assert!(selection.sizes.iter().any(|s| s.width == 800 && s.height == 600));
    quiet(&mut rx).await;
    f.show(2, zoom_window(1000, 700, (50, 110, 800, 450), 3));
    until(&mut rx, |e| matches!(e, CaptureEvent::Captured(_))).await; quiet(&mut rx).await;
}

#[tokio::test]
async fn reused_window_id_cannot_capture_another_app_or_zoom_home() {
    let d = tempfile::tempdir().unwrap(); let f = FakeWindows::default(); f.add(1, "Zoom Meeting", 800, 600, picture(1));
    let (_h, mut rx) = start(&f, Some(chosen(&f)), d.path()); until(&mut rx, |e| matches!(e, CaptureEvent::Captured(_))).await;
    { let mut s = f.0.lock().unwrap(); s.windows[0].0.bundle_id = Some("test.other".into()); s.windows[0].1 = picture(3); }
    quiet(&mut rx).await;
    { let mut s = f.0.lock().unwrap(); s.windows[0].0.bundle_id = Some("us.zoom.xos".into()); s.windows[0].0.title = "Zoom Workplace".into(); }
    quiet(&mut rx).await;
}

#[tokio::test]
async fn screenshot_and_lecture_pause_stop_discovery_and_resume_uses_current_timestamp() {
    let d = tempfile::tempdir().unwrap(); let f = FakeWindows::default(); let (h, mut rx) = start(&f, None, d.path());
    h.send(CaptureCmd::SetAutoCapture(false)); until(&mut rx, |e| matches!(e, CaptureEvent::Mode(m) if !m.automatic)).await;
    f.add(1, "Zoom Meeting", 800, 600, picture(1)); quiet(&mut rx).await; assert_eq!(f.0.lock().unwrap().captures, 0);
    h.send(CaptureCmd::Hold(true)); h.send(CaptureCmd::SetAutoCapture(true));
    until(&mut rx, |e| matches!(e, CaptureEvent::Mode(m) if m.automatic)).await; quiet(&mut rx).await;
    assert_eq!(f.0.lock().unwrap().captures, 0);
    let resumed = Local::now(); h.send(CaptureCmd::Hold(false));
    let CaptureEvent::Captured(c) = until(&mut rx, |e| matches!(e, CaptureEvent::Captured(_))).await else { unreachable!() };
    assert!(c.shown_at >= resumed);
}

#[tokio::test]
async fn denial_and_blank_discovery_retry_then_camera_to_share_recovers() {
    let d = tempfile::tempdir().unwrap(); let f = FakeWindows::default(); f.0.lock().unwrap().denied = true;
    let (_h, mut rx) = start(&f, None, d.path());
    until(&mut rx, |e| matches!(e, CaptureEvent::State(CaptureState::Denied))).await;
    f.add(1, "Zoom Meeting", 800, 600, notice(800, 600)); f.0.lock().unwrap().denied = false;
    quiet(&mut rx).await; f.0.lock().unwrap().blank_next = 3; f.show(1, picture(1));
    until(&mut rx, |e| matches!(e, CaptureEvent::Captured(_))).await; quiet(&mut rx).await;
}

#[tokio::test]
async fn blank_title_without_a_kept_slide_is_not_automatic_identity() {
    let d = tempfile::tempdir().unwrap(); let f = FakeWindows::default(); f.add(1, "Zoom Meeting", 800, 600, picture(1));
    let sel = chosen(&f); f.close(1); f.add(2, "", 800, 600, picture(1));
    let (_h, mut rx) = start(&f, Some(sel), d.path()); quiet(&mut rx).await;
}

#[tokio::test]
async fn a_separate_share_replaces_a_notice_and_a_still_photograph_is_not_lost() {
    let d = tempfile::tempdir().unwrap(); let f = FakeWindows::default(); f.add(1, "Zoom Meeting", 800, 600, notice(800, 600));
    let (_h, mut rx) = start(&f, Some(chosen(&f)), d.path()); quiet(&mut rx).await;
    f.add(2, "Screen Sharing", 800, 600, picture(1));
    until(&mut rx, |e| matches!(e, CaptureEvent::Relocated { selection, .. } if selection.descriptor.title == "Screen Sharing")).await;
    until(&mut rx, |e| matches!(e, CaptureEvent::Captured(_))).await;
    f.show(2, support::windows::camera(800, 600));
    until(&mut rx, |e| matches!(e, CaptureEvent::Captured(_))).await;
    quiet(&mut rx).await;
}

#[tokio::test]
async fn persisted_titleless_selection_cannot_trust_an_unnamed_window_next_session() {
    let d = tempfile::tempdir().unwrap(); let f = FakeWindows::default(); f.add(1, "", 800, 600, picture(1));
    let (_h, mut rx) = start(&f, Some(chosen(&f)), d.path()); quiet(&mut rx).await;
}

#[tokio::test]
async fn replacement_never_guesses_the_position_of_an_excluded_camera() {
    let d = tempfile::tempdir().unwrap(); let f = FakeWindows::default(); f.add(1, "Zoom Meeting", 800, 600, picture(1));
    let mut sel = chosen(&f); sel.leave_out = vec![Region { x: 0.8, y: 0.8, w: 0.2, h: 0.2 }];
    let (_h, mut rx) = start(&f, Some(sel), d.path());
    until(&mut rx, |e| matches!(e, CaptureEvent::Captured(_))).await;
    f.close(1); f.add(2, "Screen Sharing", 800, 600, notice(800, 600));
    quiet(&mut rx).await;
    f.show(2, picture(1));
    let CaptureEvent::Relocated { selection, .. } = until(&mut rx, |e| matches!(e, CaptureEvent::Relocated { .. })).await else { unreachable!() };
    assert_eq!(selection.leave_out, vec![Region { x: 0.8, y: 0.8, w: 0.2, h: 0.2 }]);
    quiet(&mut rx).await;
}

#[tokio::test]
async fn capture_time_denial_is_reported_for_discovery_and_offscreen_bound_windows() {
    for selected in [false, true] {
        let d = tempfile::tempdir().unwrap(); let f = FakeWindows::default(); f.add(1, "Zoom Meeting", 800, 600, picture(1));
        let sel = selected.then(|| chosen(&f));
        f.0.lock().unwrap().deny_capture = true; f.on_screen(1, false);
        let (_h, mut rx) = start(&f, sel, d.path());
        until(&mut rx, |e| matches!(e, CaptureEvent::State(CaptureState::Denied))).await;
        quiet(&mut rx).await;
        f.0.lock().unwrap().deny_capture = false; f.on_screen(1, true);
        until(&mut rx, |e| matches!(e, CaptureEvent::Captured(_))).await;
    }
}

#[tokio::test]
async fn discovery_reports_repeated_capture_failure_and_recovers() {
    let d = tempfile::tempdir().unwrap(); let f = FakeWindows::default(); f.add(1, "Zoom Meeting", 800, 600, picture(1));
    f.0.lock().unwrap().fail_next = 6;
    let (_h, mut rx) = start(&f, None, d.path());
    until(&mut rx, |e| matches!(e, CaptureEvent::State(CaptureState::Failing { reason, .. }) if reason.contains("window server said no"))).await;
    until(&mut rx, |e| matches!(e, CaptureEvent::Captured(_))).await;
}

#[tokio::test]
async fn background_discovery_does_not_flap_a_healthy_bound_status() {
    let d = tempfile::tempdir().unwrap(); let f = FakeWindows::default(); f.add(1, "Zoom Meeting", 800, 600, notice(800, 600));
    let sel = chosen(&f); f.add(2, "Screen Sharing", 800, 600, notice(800, 600));
    let (_h, mut rx) = start(&f, Some(sel), d.path());
    until(&mut rx, |e| matches!(e, CaptureEvent::State(CaptureState::Watching { .. }))).await;
    let end = tokio::time::Instant::now() + Duration::from_millis(350);
    while let Ok(Some(e)) = tokio::time::timeout_at(end, rx.recv()).await {
        assert!(!matches!(e, CaptureEvent::State(CaptureState::Asking { .. })), "{e:?}");
    }
}

#[tokio::test]
async fn fullscreen_keeps_course_crops_and_manual_capture_uses_the_live_title() {
    let d = tempfile::tempdir().unwrap(); let f = FakeWindows::default(); f.add(1, "Zoom Meeting", 800, 600, picture(1));
    let mut sel = chosen(&f); sel.leave_out = vec![Region { x: 0.8, y: 0.8, w: 0.2, h: 0.2 }];
    let (h, mut rx) = start(&f, Some(sel), d.path());
    until(&mut rx, |e| matches!(e, CaptureEvent::Captured(_))).await;
    f.close(1); f.add(2, "", 1000, 700, zoom_window(1000, 700, (50, 110, 800, 450), 1));
    let CaptureEvent::Relocated { selection, .. } = until(&mut rx, |e| matches!(e, CaptureEvent::Relocated { .. })).await else { unreachable!() };
    assert_eq!(selection.descriptor.title, "Zoom Meeting");
    assert_eq!(selection.leave_out.len(), 1);
    let (tx, reply) = tokio::sync::oneshot::channel(); h.send(CaptureCmd::Now(tx));
    assert!(reply.await.unwrap().is_ok(), "manual capture follows the verified runtime title");
    drop(h);
    let d2 = tempfile::tempdir().unwrap(); let f2 = FakeWindows::default(); f2.add(3, "Zoom Meeting", 800, 600, picture(2));
    let (_h2, mut rx2) = start(&f2, Some(selection), d2.path());
    until(&mut rx2, |e| matches!(e, CaptureEvent::Captured(_))).await;
}

#[tokio::test]
async fn leaving_titleless_fullscreen_on_a_new_slide_preserves_the_course_sizes() {
    let d = tempfile::tempdir().unwrap(); let f = FakeWindows::default(); f.add(1, "Zoom Meeting", 800, 600, picture(1));
    let (_h, mut rx) = start(&f, Some(chosen(&f)), d.path());
    until(&mut rx, |e| matches!(e, CaptureEvent::Captured(_))).await;
    f.close(1); f.add(2, "", 1000, 700, zoom_window(1000, 700, (50, 110, 800, 450), 1));
    until(&mut rx, |e| matches!(e, CaptureEvent::Relocated { .. })).await;
    f.close(2); f.add(3, "Zoom Meeting", 900, 650, zoom_window(900, 650, (40, 100, 720, 405), 4));
    let CaptureEvent::Relocated { selection, .. } = until(&mut rx, |e| matches!(e, CaptureEvent::Relocated { selection, .. } if selection.descriptor.width == 900)).await else { unreachable!() };
    assert_eq!(selection.descriptor.title, "Zoom Meeting");
    assert!(selection.sizes.iter().any(|s| s.width == 800));
    assert!(selection.sizes.iter().any(|s| s.width == 1000));
}

#[tokio::test]
async fn a_differently_named_share_gets_its_own_saved_identity_even_for_the_same_slide() {
    let d = tempfile::tempdir().unwrap(); let f = FakeWindows::default(); f.add(1, "Zoom Meeting", 800, 600, picture(1));
    let (_h, mut rx) = start(&f, Some(chosen(&f)), d.path());
    until(&mut rx, |e| matches!(e, CaptureEvent::Captured(_))).await;
    f.close(1); f.add(2, "Screen Sharing", 800, 600, picture(1));
    let CaptureEvent::Relocated { selection, .. } = until(&mut rx, |e| matches!(e, CaptureEvent::Relocated { .. })).await else { unreachable!() };
    assert_eq!(selection.descriptor.title, "Screen Sharing");
    assert!(selection.sizes.is_empty());
}

#[tokio::test]
async fn resize_ticks_between_discovery_observations_do_not_count_as_confirmation() {
    use lecturelive_core::capture::window::{CaptureError, WindowInfo, WindowSource};
    struct Script { fake: FakeWindows, calls: u32 }
    impl WindowSource for Script {
        fn windows(&mut self) -> Result<Vec<WindowInfo>, CaptureError> {
            self.calls += 1;
            if self.calls > 6 { return Err(CaptureError::Failed("script finished".into())); }
            let mut windows = self.fake.windows()?;
            // Start is call 1. Discovery runs on ticks 1, 3 and 5, never consecutively.
            if self.calls == 3 || self.calls == 5 { windows[0].width += 1; }
            Ok(windows)
        }
        fn capture(&mut self, id: u32) -> Result<image::RgbaImage, CaptureError> { self.fake.capture(id) }
    }
    let d = tempfile::tempdir().unwrap(); let f = FakeWindows::default(); f.add(1, "Zoom Meeting", 800, 600, notice(800, 600));
    let sel = chosen(&f); f.add(2, "Screen Sharing", 800, 600, picture(1));
    let (tx, mut rx) = mpsc::unbounded_channel();
    let _h = worker::spawn(Box::new(Script { fake: f, calls: 0 }), Some(sel), WorkerConfig { slides: d.path().into(), interval: Duration::from_millis(20), thresholds: Thresholds::default(), record: None }, tx);
    until(&mut rx, |e| {
        assert!(!matches!(e, CaptureEvent::Relocated { .. } | CaptureEvent::Captured(_)), "non-adjacent confirmation: {e:?}");
        matches!(e, CaptureEvent::State(CaptureState::Failing { reason, .. }) if reason.contains("script finished"))
    }).await;
}
