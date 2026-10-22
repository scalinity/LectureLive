//! A solo right Option release, observed without consuming or changing keyboard input.
//! The callback only queues work. The capture worker owns this listener and its lifetime.
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::Duration;

use core_foundation::base::TCFType;
use core_foundation::mach_port::{CFMachPort, CFMachPortInvalidate, CFMachPortRef};
use core_foundation::runloop::{kCFRunLoopDefaultMode, CFRunLoop};

use super::worker::{CaptureCmd, ManualShortcut};

const KEY_DOWN: u32 = 10;
const KEY_UP: u32 = 11;
const FLAGS_CHANGED: u32 = 12;
const MOUSE_ACTIONS: [u32; 10] = [1, 2, 3, 4, 6, 7, 22, 25, 26, 27]; // buttons, drags, scroll
const RIGHT_OPTION: i64 = 0x3d; // HIToolbox/Events.h: kVK_RightOption
const RIGHT_ALT: u64 = 0x40; // IOKit/hidsystem/IOLLEvent.h: NX_DEVICERALTKEYMASK
// Left Option, Shift, Control, Command and Fn. Caps Lock may remain on during a solo tap.
const OTHER_MODIFIERS: u64 = 0x20 | 0x20000 | 0x40000 | 0x100000 | 0x800000;

#[derive(Default)]
struct Gesture {
    right_down: bool,
    armed: bool,
    keys: u128,
    buttons: u32,
}

impl Gesture {
    fn observe(&mut self, kind: u32, key: i64, flags: u64) -> bool {
        if MOUSE_ACTIONS.contains(&kind) {
            self.armed = false;
            if (0..32).contains(&key) {
                match kind {
                    1 | 3 | 25 => self.buttons |= 1u32 << key,
                    2 | 4 | 26 => self.buttons &= !(1u32 << key),
                    _ => {}
                }
            }
        } else if kind == KEY_DOWN {
            if (0..128).contains(&key) { self.keys |= 1u128 << key; }
            self.armed = false;
        } else if kind == KEY_UP {
            if (0..128).contains(&key) { self.keys &= !(1u128 << key); }
        } else if kind == FLAGS_CHANGED {
            let down = flags & RIGHT_ALT != 0;
            if key != RIGHT_OPTION || flags & OTHER_MODIFIERS != 0 { self.armed = false; }
            if key == RIGHT_OPTION {
                if down && !self.right_down {
                    self.armed = flags & OTHER_MODIFIERS == 0 && self.keys == 0 && self.buttons == 0;
                } else if !down && self.right_down {
                    let capture = self.armed && flags & OTHER_MODIFIERS == 0;
                    self.armed = false;
                    self.right_down = false;
                    return capture;
                }
            }
            self.right_down = down;
        }
        false
    }
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGPreflightListenEventAccess() -> bool;
    fn CGRequestListenEventAccess() -> bool;
    fn CGEventTapCreate(tap: u32, place: u32, options: u32, mask: u64, callback: unsafe extern "C" fn(*mut c_void, u32, *mut c_void, *mut c_void) -> *mut c_void, info: *mut c_void) -> CFMachPortRef;
    fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
    fn CGEventGetIntegerValueField(event: *mut c_void, field: u32) -> i64;
    fn CGEventGetFlags(event: *mut c_void) -> u64;
    fn CGEventSourceFlagsState(state: i32) -> u64;
    fn CGEventSourceKeyState(state: i32, key: u16) -> bool;
    fn CGEventSourceButtonState(state: i32, button: u32) -> bool;
}

struct Context {
    gesture: Gesture,
    tx: mpsc::Sender<CaptureCmd>,
    generation: u64,
    tap: CFMachPortRef,
}

unsafe extern "C" fn callback(_: *mut c_void, kind: u32, event: *mut c_void, info: *mut c_void) -> *mut c_void {
    // The boxed context outlives the tap and is accessed only on this thread's run loop.
    let ctx = unsafe { &mut *info.cast::<Context>() };
    if kind == u32::MAX || kind == u32::MAX - 1 {
        ctx.gesture = Gesture::default();
        unsafe { CGEventTapEnable(ctx.tap, true) };
    } else if !event.is_null() {
        let field = if matches!(kind, 1 | 2 | 3 | 4 | 25 | 26) { 3 } else { 9 };
        let (key, flags) = unsafe { (CGEventGetIntegerValueField(event, field), CGEventGetFlags(event)) };
        if ctx.gesture.observe(kind, key, flags) {
            let _ = ctx.tx.send(CaptureCmd::Shortcut(ctx.generation));
        }
    }
    event // listen-only: never suppress, replace or post an event
}

pub(super) struct Listener {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Listener {
    pub(super) fn start(tx: mpsc::Sender<CaptureCmd>, generation: u64) -> Result<Self, ManualShortcut> {
        if !unsafe { CGPreflightListenEventAccess() } && !unsafe { CGRequestListenEventAccess() } {
            return Err(ManualShortcut::NeedsPermission);
        }
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let (ready_tx, ready_rx) = mpsc::channel();
        let thread = std::thread::Builder::new().name("capture-shortcut".into()).spawn(move || {
            let flags = unsafe { CGEventSourceFlagsState(1) };
            // A key already held when manual mode starts must not become a solo tap.
            let mut gesture = Gesture { right_down: flags & RIGHT_ALT != 0, ..Gesture::default() };
            for key in 0..128u16 {
                if !matches!(key, 54..=63) && unsafe { CGEventSourceKeyState(1, key) } { gesture.keys |= 1u128 << key; }
            }
            for button in 0..32 {
                if unsafe { CGEventSourceButtonState(1, button) } { gesture.buttons |= 1u32 << button; }
            }
            let mut ctx = Box::new(Context { gesture, tx, generation, tap: std::ptr::null_mut() });
            let mask = MOUSE_ACTIONS.iter().fold((1 << KEY_DOWN) | (1 << KEY_UP) | (1 << FLAGS_CHANGED), |mask, kind| mask | (1 << kind));
            // Session tap, tail append, listen-only. Ownership follows the Core Foundation create rule.
            let raw = unsafe { CGEventTapCreate(1, 1, 1, mask, callback, (&mut *ctx as *mut Context).cast()) };
            if raw.is_null() {
                let _ = ready_tx.send(Err("macOS could not start the right Option listener".to_string()));
                return;
            }
            let tap = unsafe { CFMachPort::wrap_under_create_rule(raw) };
            ctx.tap = raw;
            let Ok(source) = tap.create_runloop_source(0) else {
                unsafe { CFMachPortInvalidate(raw) };
                let _ = ready_tx.send(Err("macOS could not start the keyboard run loop".to_string()));
                return;
            };
            let run_loop = CFRunLoop::get_current();
            let mode = unsafe { kCFRunLoopDefaultMode };
            run_loop.add_source(&source, mode);
            let _ = ready_tx.send(Ok(()));
            while !stopping.load(Ordering::Acquire) {
                CFRunLoop::run_in_mode(mode, Duration::from_millis(100), false);
            }
            unsafe { CGEventTapEnable(raw, false); CFMachPortInvalidate(raw); }
            run_loop.remove_source(&source, mode);
            // tap, source and callback context are all released on their owning thread.
        }).map_err(|e| ManualShortcut::Unavailable { reason: e.to_string() })?;
        let listener = Self { stop, thread: Some(thread) };
        match ready_rx.recv_timeout(Duration::from_secs(3)) {
            Ok(Ok(())) => Ok(listener),
            result => {
                let reason = match result { Ok(Err(e)) => e, Err(e) => e.to_string(), _ => unreachable!() };
                drop(listener);
                Err(ManualShortcut::Unavailable { reason })
            }
        }
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() { let _ = thread.join(); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run manually with Input Monitoring already granted. In another app press left Option,
    /// right Option+A, then two solo right Option taps. No input is posted by this test.
    #[test]
    #[ignore = "requires an existing Input Monitoring grant and external keyboard input"]
    fn native_right_option_listener_observes_two_solo_taps() {
        assert!(unsafe { CGPreflightListenEventAccess() }, "grant Input Monitoring before this check");
        let (tx, rx) = mpsc::channel();
        let _listener = Listener::start(tx, 7).unwrap_or_else(|e| panic!("native listener: {e:?}"));
        println!("READY_RIGHT_OPTION: left Option, right Option+A, then two solo right Option taps");
        for _ in 0..2 {
            assert!(matches!(rx.recv_timeout(Duration::from_secs(15)), Ok(CaptureCmd::Shortcut(7))));
        }
        assert!(rx.recv_timeout(Duration::from_millis(500)).is_err(), "one release means one capture request");
    }

    #[test]
    fn only_a_solo_right_option_release_captures_once() {
        let mut g = Gesture::default();
        assert!(!g.observe(FLAGS_CHANGED, RIGHT_OPTION, RIGHT_ALT));
        assert!(!g.observe(FLAGS_CHANGED, RIGHT_OPTION, RIGHT_ALT));
        assert!(g.observe(FLAGS_CHANGED, RIGHT_OPTION, 0));
        assert!(!g.observe(FLAGS_CHANGED, RIGHT_OPTION, 0));
        assert!(!g.observe(FLAGS_CHANGED, 58, 0x20));
        assert!(!g.observe(FLAGS_CHANGED, 58, 0));
        for _ in 0..3 {
            assert!(!g.observe(FLAGS_CHANGED, RIGHT_OPTION, RIGHT_ALT));
            assert!(g.observe(FLAGS_CHANGED, RIGHT_OPTION, 0));
        }
    }

    #[test]
    fn chords_in_either_order_cancel_the_tap() {
        for modifier in [0x20, 0x20000, 0x40000, 0x100000, 0x800000] {
            let mut g = Gesture::default();
            g.observe(FLAGS_CHANGED, RIGHT_OPTION, RIGHT_ALT);
            g.observe(FLAGS_CHANGED, 56, RIGHT_ALT | modifier);
            g.observe(FLAGS_CHANGED, 56, RIGHT_ALT);
            assert!(!g.observe(FLAGS_CHANGED, RIGHT_OPTION, 0));
            g.observe(FLAGS_CHANGED, RIGHT_OPTION, RIGHT_ALT | modifier);
            assert!(!g.observe(FLAGS_CHANGED, RIGHT_OPTION, modifier));
        }
        let mut g = Gesture::default();
        g.observe(FLAGS_CHANGED, RIGHT_OPTION, RIGHT_ALT);
        g.observe(KEY_DOWN, 0, RIGHT_ALT);
        g.observe(KEY_DOWN, 0, RIGHT_ALT); // repeat
        g.observe(KEY_UP, 0, RIGHT_ALT);
        assert!(!g.observe(FLAGS_CHANGED, RIGHT_OPTION, 0));
        g.observe(KEY_DOWN, 0, 0); // normal key already held
        g.observe(FLAGS_CHANGED, RIGHT_OPTION, RIGHT_ALT);
        assert!(!g.observe(FLAGS_CHANGED, RIGHT_OPTION, 0));
    }

    #[test]
    fn mouse_actions_and_a_caps_lock_press_cancel_the_tap() {
        for kind in MOUSE_ACTIONS {
            let mut g = Gesture::default();
            g.observe(FLAGS_CHANGED, RIGHT_OPTION, RIGHT_ALT);
            g.observe(kind, 0, RIGHT_ALT);
            assert!(!g.observe(FLAGS_CHANGED, RIGHT_OPTION, 0));
        }
        let mut g = Gesture::default();
        g.observe(FLAGS_CHANGED, RIGHT_OPTION, RIGHT_ALT);
        g.observe(FLAGS_CHANGED, 57, RIGHT_ALT | 0x10000);
        assert!(!g.observe(FLAGS_CHANGED, RIGHT_OPTION, 0x10000));
        for release_before_option in [false, true] {
            let mut g = Gesture::default();
            g.observe(1, 0, 0); // button was already held before Option
            g.observe(FLAGS_CHANGED, RIGHT_OPTION, RIGHT_ALT);
            if release_before_option { g.observe(2, 0, RIGHT_ALT); }
            assert!(!g.observe(FLAGS_CHANGED, RIGHT_OPTION, 0));
        }
    }

    #[test]
    fn starting_or_resetting_during_a_press_does_not_capture() {
        let mut g = Gesture { right_down: true, ..Gesture::default() };
        assert!(!g.observe(FLAGS_CHANGED, RIGHT_OPTION, 0));
        let mut reset = Gesture::default();
        assert!(!reset.observe(FLAGS_CHANGED, RIGHT_OPTION, 0));
        g.observe(FLAGS_CHANGED, RIGHT_OPTION, RIGHT_ALT | 0x10000); // Caps Lock is allowed
        assert!(g.observe(FLAGS_CHANGED, RIGHT_OPTION, 0x10000));
    }
}
