//! macOS-specific window configuration for the Dashboard-style overlay.
//!
//! iced/winit alone can produce a transparent, undecorated, always-on-top
//! window, but the Dashboard feel needs AppKit — and appearing on top of
//! *fullscreen* apps needs more than flags: a regular activating `NSWindow`
//! is not reliably admitted onto fullscreen Spaces. The production trick
//! (used by tauri-nspanel, Raycast-style launchers, etc.) is to morph the
//! window into a non-activating `NSPanel` at runtime, which can order onto
//! any Space — including fullscreen ones — and take key input without
//! activating the app (so no Space switch happens).

use std::os::raw::{c_char, c_int, c_void};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Mutex, Once, OnceLock};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{ClassType, MainThreadOnly, class, define_class, msg_send};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationOptions, NSApplicationActivationPolicy,
    NSRunningApplication, NSEvent, NSEventMask, NSPanel, NSScreen,
    NSWindow, NSWindowCollectionBehavior, NSWindowStyleMask, NSWorkspace,
    NSWorkspaceActiveSpaceDidChangeNotification,
};
use objc2_foundation::NSNotification;

/// `kCGScreenSaverWindowLevel`: reliably above fullscreen apps.
const OVERLAY_LEVEL: isize = 1000;

/// macOS reports active-Space changes, so the overlay auto-dismisses on switch.
pub const WATCHES_WORKSPACE: bool = true;

/// macOS delivers trackpad pinch (magnify) gestures we capture natively.
pub const SUPPORTS_PINCH: bool = true;

/// macOS can recognize a raw multi-finger trackpad swipe (via a private
/// framework) to summon the overlay. Experimental, opt-in.
pub const SUPPORTS_GESTURES: bool = true;

/// The overlay's see-through switches while it's up (see
/// [`set_overlay_opaque`]).
pub const LIVE_TRANSPARENCY: bool = true;

/// Makes the overlay opaque, or see-through again, by the window's own opaque
/// flag — which is what the window server composites by.
///
/// See-through, the board is blended over everything under it on every frame,
/// and the windows under it count as visible, so their apps keep drawing too:
/// over a busy desktop that's a ProMotion panel dropping to 60 Hz, while the
/// same board over an empty Space, or in a plain window, holds 120. Opaque, it
/// covers what's under it and none of that happens. The Metal layer is
/// see-through-capable either way (iced_wgpu configures it so); the board's
/// clear colour (`startup::style`) is what paints the difference.
pub fn set_overlay_opaque(opaque: bool) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    for window in NSApplication::sharedApplication(mtm).windows() {
        if !is_our_window(&window) {
            continue;
        }
        let background: Retained<AnyObject> = if opaque {
            unsafe { msg_send![class!(NSColor), blackColor] }
        } else {
            unsafe { msg_send![class!(NSColor), clearColor] }
        };
        unsafe {
            let _: () = msg_send![&*window, setOpaque: opaque];
            let _: () = msg_send![&*window, setBackgroundColor: &*background];
        }
    }
}

/// The main screen's top safe-area inset (px) — the MacBook notch / menu-bar
/// height. `safeAreaInsets` is macOS 12+, so guard with `respondsToSelector` to
/// avoid an unrecognized selector on older systems (which have no notch anyway).
/// The app that had the keyboard when the overlay was last summoned, by pid.
/// 0 when we don't know of one.
static PREVIOUS_APP: AtomicI32 = AtomicI32::new(0);

/// Hands the keyboard back to whatever was in front before the overlay took it.
///
/// The counterpart to the `activateIgnoringOtherApps` in
/// [`configure_overlay_window`]: summoning has to take focus explicitly, because
/// a non-activating panel doesn't get it, so dismissing has to give it back just
/// as explicitly. **`deactivate()` on its own does not do it** — AppKit treats it
/// as a request and, with nothing else asking to come forward, simply leaves us
/// active with no window on screen. So the app we took focus *from* is activated
/// by name, and `deactivate` is only the fallback for when it has since quit.
pub fn release_focus() {
    let Some(mtm) = MainThreadMarker::new() else {
        return; // main-thread only; a background caller would be a no-op anyway
    };
    let pid = PREVIOUS_APP.swap(0, Ordering::Relaxed);
    let previous = (pid != 0)
        .then(|| NSRunningApplication::runningApplicationWithProcessIdentifier(pid))
        .flatten();
    if let Some(previous) = previous {
        // Not `ActivateAllWindows`: bring it back as it was, rather than
        // unstacking every window it had.
        if previous.activateWithOptions(NSApplicationActivationOptions::empty()) {
            return;
        }
    }
    NSApplication::sharedApplication(mtm).deactivate();
}

pub fn top_safe_inset() -> f32 {
    use objc2::runtime::NSObjectProtocol;
    let Some(mtm) = MainThreadMarker::new() else {
        return 0.0;
    };
    let Some(screen) = NSScreen::mainScreen(mtm) else {
        return 0.0;
    };
    if !screen.respondsToSelector(objc2::sel!(safeAreaInsets)) {
        return 0.0;
    }
    screen.safeAreaInsets().top as f32
}

/// Callback invoked when the active macOS Space changes; set by the app.
type SpaceHandler = Box<dyn Fn() + Send + Sync>;
static SPACE_HANDLER: OnceLock<SpaceHandler> = OnceLock::new();
static SPACE_OBSERVER: Once = Once::new();

/// Callback for a trackpad pinch: `(magnification_delta, x, y)` where the point
/// is the event's `locationInWindow` (bottom-left origin; the app flips y).
type MagnifyHandler = Box<dyn Fn(f32, f32, f32) + Send + Sync>;
static MAGNIFY_HANDLER: OnceLock<MagnifyHandler> = OnceLock::new();
static MAGNIFY_MONITOR: Once = Once::new();

/// Registers the app's handler for trackpad pinch gestures. The local
/// event monitor is installed lazily by [`start_magnify_monitor`].
pub fn set_magnify_handler(handler: impl Fn(f32, f32, f32) + Send + Sync + 'static) {
    let _ = MAGNIFY_HANDLER.set(Box::new(handler));
}

/// Installs a local `NSEvent` monitor for magnify (pinch) gestures once, on the
/// main thread. The block forwards each event's incremental magnification and
/// cursor position to the app, then returns the event unchanged so normal
/// dispatch continues. Monitor + block are leaked to live for the app.
fn start_magnify_monitor() {
    MAGNIFY_MONITOR.call_once(|| {
        let block = RcBlock::new(|event: NonNull<NSEvent>| -> *mut NSEvent {
            if let Some(handler) = MAGNIFY_HANDLER.get() {
                let ev = unsafe { event.as_ref() };
                let mag = ev.magnification() as f32;
                let loc = ev.locationInWindow();
                handler(mag, loc.x as f32, loc.y as f32);
            }
            event.as_ptr()
        });
        let monitor = unsafe {
            NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::Magnify, &block)
        };
        std::mem::forget(monitor);
        std::mem::forget(block);
    });
}

// ---------------------------------------------------------------------------
// Four-finger-swipe-down summon gesture (experimental)
//
// Reads raw trackpad contacts through the private MultitouchSupport framework —
// the only way to see finger positions while the app is unfocused (a local
// NSEvent monitor reports zero touches). The framework is loaded with `dlopen`
// so a missing or renamed symbol degrades to a no-op rather than a launch
// failure, and it's only tapped while the user has opted in.
// ---------------------------------------------------------------------------

/// A trackpad contact point, normalized to `0..1` (x: left→right, y: bottom→top).
#[repr(C)]
#[derive(Clone, Copy)]
struct MtPoint {
    x: f32,
    y: f32,
}

/// Position + velocity readout for a contact (private-framework `MTReadout`).
#[repr(C)]
#[derive(Clone, Copy)]
#[allow(dead_code)] // `vel` is part of the ABI layout but unused.
struct MtReadout {
    pos: MtPoint,
    vel: MtPoint,
}

/// One finger in a contact frame (private-framework `MTTouch`). The full,
/// reverse-engineered layout is declared so the callback's array stride is
/// correct; we only read `normalized.pos`.
#[repr(C)]
#[derive(Clone, Copy)]
#[allow(dead_code)] // Most fields exist only to fix the struct size/stride.
struct MtTouch {
    frame: c_int,
    timestamp: f64,
    identifier: c_int,
    state: c_int,
    finger_id: c_int,
    hand_id: c_int,
    normalized: MtReadout,
    size: f32,
    field9: c_int,
    angle: f32,
    major_axis: f32,
    minor_axis: f32,
    absolute: MtReadout,
    field14: c_int,
    field15: c_int,
    z_density: f32,
}

type MtDeviceRef = *mut c_void;
type MtContactCallback =
    extern "C" fn(MtDeviceRef, *mut MtTouch, c_int, f64, c_int) -> c_int;

/// The private-framework entry points we resolve at runtime.
struct MtApi {
    create_list: unsafe extern "C" fn() -> *const c_void, // -> CFArrayRef of devices
    register: unsafe extern "C" fn(MtDeviceRef, MtContactCallback),
    start: unsafe extern "C" fn(MtDeviceRef, c_int) -> c_int,
    stop: unsafe extern "C" fn(MtDeviceRef),
}

// CoreFoundation array accessors, the dynamic loader, and the libc fd calls we
// use to mute the framework's banner (all in libSystem, which is always linked,
// so no `#[link]` attribute is needed).
unsafe extern "C" {
    fn CFArrayGetCount(array: *const c_void) -> isize;
    fn CFArrayGetValueAtIndex(array: *const c_void, index: isize) -> *const c_void;
    fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dup(fd: c_int) -> c_int;
    fn dup2(src: c_int, dst: c_int) -> c_int;
    fn close(fd: c_int) -> c_int;
    fn fflush(stream: *mut c_void) -> c_int;
}

/// Runs `f` with `stdout`/`stderr` redirected to `/dev/null`, then restores
/// them. MultitouchSupport prints an unconditional device banner
/// (`*** Recognized (0x..) family*** (N cols X M rows)`) from inside the closed
/// framework when a device is created/started; this swallows it. The redirect
/// window is a few synchronous FFI calls on the main thread, so the only cost
/// is that anything another thread prints in that instant is dropped too.
fn muting_std_output<R>(f: impl FnOnce() -> R) -> R {
    use std::os::fd::AsRawFd;
    let Ok(devnull) = std::fs::OpenOptions::new().write(true).open("/dev/null") else {
        return f(); // can't open the sink — just let the banner through
    };
    unsafe {
        let (saved_out, saved_err) = (dup(1), dup(2));
        dup2(devnull.as_raw_fd(), 1);
        dup2(devnull.as_raw_fd(), 2);
        let result = f();
        fflush(std::ptr::null_mut()); // flush the banner while it still goes to /dev/null
        if saved_out >= 0 {
            dup2(saved_out, 1);
            close(saved_out);
        }
        if saved_err >= 0 {
            dup2(saved_err, 2);
            close(saved_err);
        }
        result
    }
}

/// The app's summon handler, fired when the swipe is recognized.
type GestureHandler = Box<dyn Fn() + Send + Sync>;
static GESTURE_HANDLER: OnceLock<GestureHandler> = OnceLock::new();
/// Resolved framework API, or `None` if it couldn't be loaded (once).
static MT_API: OnceLock<Option<MtApi>> = OnceLock::new();
/// Started device handles (as `usize`, since raw pointers aren't `Send`), kept
/// so the tap can be stopped again.
static GESTURE_DEVICES: Mutex<Vec<usize>> = Mutex::new(Vec::new());
/// Recognizer state across contact frames.
static GESTURE_STATE: Mutex<GestureState> = Mutex::new(GestureState::IDLE);

#[derive(Clone, Copy)]
struct GestureState {
    /// A four-finger contact is currently in progress.
    active: bool,
    /// Centroid y when the four-finger contact began.
    start_y: f32,
    /// Already fired for this contact (until fingers lift/change count).
    fired: bool,
}

impl GestureState {
    const IDLE: Self = Self {
        active: false,
        start_y: 0.0,
        fired: false,
    };
}

/// Minimum downward centroid travel (normalized pad units) to fire the swipe.
const GESTURE_THRESHOLD: f32 = 0.2;

/// Registers the app's handler for the summon gesture. The tap itself is
/// started/stopped separately via [`set_summon_gesture_enabled`].
pub fn set_gesture_handler(handler: impl Fn() + Send + Sync + 'static) {
    let _ = GESTURE_HANDLER.set(Box::new(handler));
}

/// Resolves the MultitouchSupport entry points once. Returns `None` (cached) if
/// the framework or any symbol is unavailable.
fn mt_api() -> Option<&'static MtApi> {
    MT_API
        .get_or_init(|| unsafe {
            let handle = dlopen(
                c"/System/Library/PrivateFrameworks/MultitouchSupport.framework/MultitouchSupport"
                    .as_ptr(),
                1, // RTLD_LAZY
            );
            if handle.is_null() {
                return None;
            }
            let create_list = dlsym(handle, c"MTDeviceCreateList".as_ptr());
            let register = dlsym(handle, c"MTRegisterContactFrameCallback".as_ptr());
            let start = dlsym(handle, c"MTDeviceStart".as_ptr());
            let stop = dlsym(handle, c"MTDeviceStop".as_ptr());
            if create_list.is_null() || register.is_null() || start.is_null() || stop.is_null() {
                return None;
            }
            Some(MtApi {
                create_list: std::mem::transmute::<
                    *mut c_void,
                    unsafe extern "C" fn() -> *const c_void,
                >(create_list),
                register: std::mem::transmute::<
                    *mut c_void,
                    unsafe extern "C" fn(MtDeviceRef, MtContactCallback),
                >(register),
                start: std::mem::transmute::<
                    *mut c_void,
                    unsafe extern "C" fn(MtDeviceRef, c_int) -> c_int,
                >(start),
                stop: std::mem::transmute::<*mut c_void, unsafe extern "C" fn(MtDeviceRef)>(stop),
            })
        })
        .as_ref()
}

/// Per-frame callback from the trackpad. Tracks the four-finger centroid and
/// fires the handler once when it has traveled far enough downward.
extern "C" fn contact_callback(
    _device: MtDeviceRef,
    data: *mut MtTouch,
    n: c_int,
    _timestamp: f64,
    _frame: c_int,
) -> c_int {
    if !data.is_null() && n > 0 {
        let touches = unsafe { std::slice::from_raw_parts(data, n as usize) };
        let mut state = GESTURE_STATE.lock().unwrap();
        if touches.len() == 4 {
            let centroid_y = touches.iter().map(|t| t.normalized.pos.y).sum::<f32>() / 4.0;
            if !state.active {
                *state = GestureState {
                    active: true,
                    start_y: centroid_y,
                    fired: false,
                };
            } else if !state.fired && state.start_y - centroid_y > GESTURE_THRESHOLD {
                // y decreases toward the near edge of the pad, so a positive
                // `start_y - centroid_y` is a downward swipe.
                state.fired = true;
                if let Some(handler) = GESTURE_HANDLER.get() {
                    handler();
                }
            }
        } else {
            *state = GestureState::IDLE;
        }
    }
    0
}

/// Starts or stops the trackpad tap. Must run on the main thread (the callback
/// is scheduled on the calling thread's run loop). A no-op if the framework
/// isn't available.
pub fn set_summon_gesture_enabled(enable: bool) {
    let Some(api) = mt_api() else {
        return;
    };
    let mut devices = GESTURE_DEVICES.lock().unwrap();
    if enable {
        if !devices.is_empty() {
            return; // already running
        }
        // `MTDeviceCreateList`/`MTDeviceStart` print a recognition banner from
        // inside the framework; mute stdout/stderr across them so it doesn't
        // land in the app's console on every launch.
        muting_std_output(|| {
            let list = unsafe { (api.create_list)() };
            if list.is_null() {
                return;
            }
            let count = unsafe { CFArrayGetCount(list) };
            for i in 0..count {
                let device = unsafe { CFArrayGetValueAtIndex(list, i) } as MtDeviceRef;
                if device.is_null() {
                    continue;
                }
                unsafe {
                    (api.register)(device, contact_callback);
                    (api.start)(device, 0);
                }
                devices.push(device as usize);
            }
        });
    } else {
        for &device in devices.iter() {
            unsafe { (api.stop)(device as MtDeviceRef) };
        }
        devices.clear();
        *GESTURE_STATE.lock().unwrap() = GestureState::IDLE;
    }
}

/// Registers the app's handler for active-Space changes. The observer is
/// installed lazily by [`start_space_observer`].
pub fn set_workspace_change_handler(handler: impl Fn() + Send + Sync + 'static) {
    let _ = SPACE_HANDLER.set(Box::new(handler));
}

/// Installs the `NSWorkspace` active-Space-change observer once. The block
/// and observer token are intentionally leaked to live for the app's
/// lifetime.
fn start_space_observer() {
    SPACE_OBSERVER.call_once(|| {
        let center = NSWorkspace::sharedWorkspace().notificationCenter();
        let block = RcBlock::new(|_notification: NonNull<NSNotification>| {
            if let Some(handler) = SPACE_HANDLER.get() {
                handler();
            }
        });
        let observer = unsafe {
            center.addObserverForName_object_queue_usingBlock(
                Some(NSWorkspaceActiveSpaceDidChangeNotification),
                None,
                None,
                &block,
            )
        };
        std::mem::forget(observer);
        std::mem::forget(block);
    });
}

define_class!(
    /// A non-activating panel that can still take key input (for Esc and
    /// the node text editor). Mirrors winit's `WinitWindow` overrides,
    /// which are lost by the class swap.
    #[unsafe(super(NSPanel))]
    #[thread_kind = MainThreadOnly]
    #[name = "ThinkBoardOverlayPanel"]
    struct OverlayPanel;

    impl OverlayPanel {
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key_window(&self) -> bool {
            true
        }

        #[unsafe(method(canBecomeMainWindow))]
        fn can_become_main_window(&self) -> bool {
            false
        }
    }
);

pub fn configure_overlay_window() {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);

    // Accessory (agent) app: no Dock icon, no Space ownership.
    let _ = app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    // `mainScreen` is nil when no window holds keyboard focus — exactly the
    // state right after a Space switch, when we're about to re-summon. Don't
    // bail (that would skip morphing and ordering the window, leaving it
    // hidden while the app still activates); fall back to the first screen and
    // simply skip the reframe if there's genuinely no screen.
    let screen = NSScreen::mainScreen(mtm).or_else(|| NSScreen::screens(mtm).firstObject());

    for window in app.windows() {
        // Only configure our own window: the app also owns system-created
        // auxiliary windows (input-method / ViewBridge panels), and
        // morphing or force-fronting those crashes inside AppKit.
        if !is_our_window(&window) {
            continue;
        }
        morph_into_panel(&window);

        window.setLevel(OVERLAY_LEVEL);
        // `MoveToActiveSpace`: the panel follows to whichever Space is active
        // when we activate the app on summon. `CanJoinAllSpaces` gets stranded on
        // its original Space after another app's fullscreen transition (which
        // spins up a Space of its own) and won't rejoin the active one — so a
        // summon on another desktop shows nothing. `FullScreenAuxiliary` still
        // lets it appear over a fullscreen app's Space.
        window.setCollectionBehavior(
            NSWindowCollectionBehavior::MoveToActiveSpace
                | NSWindowCollectionBehavior::FullScreenAuxiliary
                | NSWindowCollectionBehavior::IgnoresCycle,
        );
        window.setHasShadow(false);
        window.setHidesOnDeactivate(false);
        if let Some(screen) = &screen {
            window.setFrame_display(screen.frame(), true);
        }
        // `orderFrontRegardless` brings the panel forward even before the app is
        // active (we activate just below); paired with `MoveToActiveSpace`, the
        // panel then lands on the Space that's active now.
        window.makeKeyAndOrderFront(None);
        window.orderFrontRegardless();
    }

    // Who had the keyboard before we take it, so dismissing can give it back.
    // `deactivate()` alone is not enough: AppKit treats it as advisory and, with
    // no other app asking to be frontmost, leaves us active with our window
    // gone — which is exactly the symptom, a board that vanishes while the next
    // keystroke still arrives here.
    if let Some(front) = NSWorkspace::sharedWorkspace().frontmostApplication() {
        let pid = front.processIdentifier();
        if pid != std::process::id() as i32 {
            PREVIOUS_APP.store(pid, Ordering::Relaxed);
        }
    }

    // Grab keyboard focus for the just-shown overlay. Ordering a non-activating
    // panel to the front isn't enough — the system leaves focus on the
    // previously-active app — so activate explicitly. The panel's collection
    // behavior keeps it on the current Space (even over a fullscreen app), so
    // this doesn't switch Spaces. `activate()` (the non-deprecated call) yields
    // to an app that hasn't cooperatively deactivated, so it wouldn't reliably
    // take focus when summoned over another app — the older call is deliberate.
    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);

    // Install the Space-change observer and pinch-gesture monitor (once) now
    // that we're on the main thread; their handlers are set by the app.
    start_space_observer();
    start_magnify_monitor();
}

use objc2::MainThreadMarker;

fn is_our_window(window: &Retained<NSWindow>) -> bool {
    // Only the borderless overlay is morphed — never the settings window, a
    // normal titled window. Morphing it would clobber its KVO state (the class
    // swap breaks winit's `effectiveAppearance` observer) and throw an
    // NSRangeException when it's closed.
    if window.styleMask().contains(NSWindowStyleMask::Titled) {
        return false;
    }

    // KVO dynamically subclasses the window (`NSKVONotifying_WinitWindow`),
    // so the class must be matched by ancestry, not by exact name.
    let object: &AnyObject = window.as_ref();
    let mut class = Some(object.class());
    while let Some(current) = class {
        if current == OverlayPanel::class() || current.name().to_str() == Ok("WinitWindow") {
            return true;
        }
        class = current.superclass();
    }
    false
}

/// Swaps the window's class to [`OverlayPanel`] and adds the
/// non-activating panel style. Safe because winit's `WinitWindow` declares
/// no instance variables and only overrides the two `canBecome*` methods,
/// which `OverlayPanel` re-implements.
fn morph_into_panel(window: &Retained<NSWindow>) {
    let object: &AnyObject = window.as_ref();
    if object.class() == OverlayPanel::class() {
        return;
    }

    unsafe {
        AnyObject::set_class(object, OverlayPanel::class());
    }
    window.setStyleMask(window.styleMask() | NSWindowStyleMask::NonactivatingPanel);
}
