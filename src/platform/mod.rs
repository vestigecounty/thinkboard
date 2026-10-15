//! Platform abstraction. Every OS-specific behavior goes through this module,
//! so the rest of the app stays platform-agnostic (no `#[cfg(target_os)]` at
//! call sites). Each backend — [`macos`], [`windows`], [`fallback`] —
//! implements the same small surface; this facade dispatches to the one built
//! for the target and adds portable helpers (`open_url`, `kill_previous_instances`).

#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;
// Everything else (Linux/BSD, for now) gets no desktop-overlay treatment.
#[cfg(not(any(target_os = "macos", windows)))]
mod fallback;

#[cfg(target_os = "macos")]
use macos as backend;
#[cfg(windows)]
use windows as backend;
#[cfg(not(any(target_os = "macos", windows)))]
use fallback as backend;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

/// `CREATE_NO_WINDOW`. Spawning a console program (taskkill, cmd) from our
/// GUI-subsystem process would otherwise flash a console window; this flag
/// suppresses it.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Whether this platform reports workspace/Space changes, so the overlay can
/// auto-dismiss when the user switches desktops. macOS only, for now.
pub const WATCHES_WORKSPACE: bool = backend::WATCHES_WORKSPACE;

/// Whether the overlay window can be transparent (the desktop shows through).
/// macOS composites via Metal; Windows via the DirectComposition swapchain our
/// patched iced_wgpu selects (see `startup::run`); Linux via the desktop
/// compositor over a Vulkan/GL surface. Other targets (uncomposited/headless)
/// render a transparent window black, so the overlay stays opaque there.
#[cfg(any(target_os = "macos", windows, target_os = "linux"))]
pub const OVERLAY_TRANSPARENT: bool = true;
#[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
pub const OVERLAY_TRANSPARENT: bool = false;

/// The system UI font family for live and rasterized text — the fallback
/// family when a theme names none. Its faces come from [`UI_FONT_FILES`].
#[cfg(target_os = "macos")]
pub const UI_FONT_NAME: &str = ".SF NS";
#[cfg(windows)]
pub const UI_FONT_NAME: &str = "Segoe UI";
#[cfg(not(any(target_os = "macos", windows)))]
pub const UI_FONT_NAME: &str = "sans-serif";

/// The system's monospace family, for code — the face [`UI_FONT_FILES`] loads
/// for it. Set as the generic `monospace` in both font systems, whose own
/// defaults name faces that aren't installed (Courier New, Noto Sans Mono) and
/// so fell back to *different* faces: inline code was laid out in one and
/// drawn in another, overlapping the word after it. `None` where we load no
/// mono face of our own.
#[cfg(target_os = "macos")]
pub const UI_MONO_NAME: Option<&str> = Some(".SF NS Mono");
#[cfg(windows)]
pub const UI_MONO_NAME: Option<&str> = Some("Consolas");
#[cfg(not(any(target_os = "macos", windows)))]
pub const UI_MONO_NAME: Option<&str> = None;

/// System font files to register with iced/cosmic-text: an upright face
/// (regular/bold via the weight axis), a real italic face, and a monospace
/// face for code. Missing files are ignored; an empty list means "use whatever
/// fonts the system already exposes" (see the raster path's system-font load).
#[cfg(target_os = "macos")]
pub const UI_FONT_FILES: &[&str] = &[
    "/System/Library/Fonts/SFNS.ttf",
    "/System/Library/Fonts/SFNSItalic.ttf",
    "/System/Library/Fonts/SFNSMono.ttf",
];
#[cfg(windows)]
pub const UI_FONT_FILES: &[&str] = &[
    r"C:\Windows\Fonts\segoeui.ttf",
    r"C:\Windows\Fonts\segoeuii.ttf",
    r"C:\Windows\Fonts\consola.ttf",
];
#[cfg(not(any(target_os = "macos", windows)))]
pub const UI_FONT_FILES: &[&str] = &[];

/// Applies the desktop-overlay treatment to the app window: always-on-top,
/// visible on every workspace, non-activating, sized to the screen. Called
/// after the window is shown. A no-op where it isn't implemented.
pub fn configure_overlay_window() {
    backend::configure_overlay_window();
}

/// Registers a handler fired when the active workspace/Space changes. Only
/// installs a native observer where [`WATCHES_WORKSPACE`] is true.
pub fn set_workspace_change_handler(handler: impl Fn() + Send + Sync + 'static) {
    backend::set_workspace_change_handler(handler);
}

/// Whether this platform delivers trackpad pinch (magnify) gestures we capture
/// natively (iced doesn't expose them). Only macOS, for now.
pub const SUPPORTS_PINCH: bool = backend::SUPPORTS_PINCH;

/// Registers a handler for trackpad pinch gestures: `(magnification_delta, x,
/// y)`, the point being the event's window-space location (bottom-left origin).
/// A no-op where [`SUPPORTS_PINCH`] is false.
pub fn set_magnify_handler(handler: impl Fn(f32, f32, f32) + Send + Sync + 'static) {
    backend::set_magnify_handler(handler);
}

/// Whether this platform can recognize a raw multi-finger trackpad swipe to
/// summon the overlay (a private-framework tap). Only macOS, and experimental.
pub const SUPPORTS_GESTURES: bool = backend::SUPPORTS_GESTURES;

/// Registers the handler fired when the summon gesture is recognized. Pair with
/// [`set_summon_gesture_enabled`] to actually start listening. A no-op where
/// [`SUPPORTS_GESTURES`] is false.
pub fn set_gesture_handler(handler: impl Fn() + Send + Sync + 'static) {
    backend::set_gesture_handler(handler);
}

/// Starts (or stops) listening for the summon gesture. Must be called on the
/// main thread. A no-op where [`SUPPORTS_GESTURES`] is false.
pub fn set_summon_gesture_enabled(enable: bool) {
    backend::set_summon_gesture_enabled(enable);
}

/// Stops this app being the active one, so the keyboard goes back to whatever
/// was in front before the overlay was summoned.
///
/// Hiding the window isn't enough on macOS: summoning calls
/// `activateIgnoringOtherApps`, because ordering a non-activating panel forward
/// leaves focus behind — and nothing undoes that when the panel goes away. The
/// board vanishes, the app stays active, and it takes a click on another window
/// to type anywhere again. A no-op elsewhere, where hiding a window hands focus
/// on by itself.
pub fn release_focus() {
    backend::release_focus();
}

/// The top safe-area inset in points: the macOS notch / menu-bar height, so
/// top-anchored overlay UI can avoid being clipped. 0 on platforms without one.
pub fn top_safe_inset() -> f32 {
    backend::top_safe_inset()
}

/// Terminates any other running instance so a freshly built binary can take
/// over — the app is resident (hotkey-summoned), so iterating would otherwise
/// pile up copies fighting over the global hotkey.
pub fn kill_previous_instances() {
    let this_pid = std::process::id().to_string();

    #[cfg(unix)]
    {
        let Ok(output) = std::process::Command::new("pgrep")
            .args(["-x", "thinkboard"])
            .output()
        else {
            return;
        };
        for pid in String::from_utf8_lossy(&output.stdout).split_whitespace() {
            if pid != this_pid {
                let _ = std::process::Command::new("kill").arg(pid).status();
            }
        }
    }

    #[cfg(windows)]
    {
        // Kill every other thinkboard.exe (all but this PID) so the new build
        // wins the hotkey. `/F` forces, the filter excludes ourselves.
        let _ = std::process::Command::new("taskkill")
            .args([
                "/F",
                "/IM",
                "thinkboard.exe",
                "/FI",
                &format!("PID ne {this_pid}"),
            ])
            .creation_flags(CREATE_NO_WINDOW)
            .status();
    }
}

/// Opens `url` in the user's default browser. Only `http(s)`/`mailto` schemes
/// are launched — a guard so a note's link can't fire an arbitrary URL handler
/// (`file:`, `javascript:`, custom app schemes, …).
pub fn open_url(url: &str) {
    let allowed = url.starts_with("http://")
        || url.starts_with("https://")
        || url.starts_with("mailto:");
    if !allowed {
        return;
    }
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(url).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    #[cfg(windows)]
    let _ = std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
}
