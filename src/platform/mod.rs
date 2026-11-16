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
#[cfg(not(windows))]
mod picture;
#[cfg(any(not(any(target_os = "macos", windows)), test))]
mod wallpaper;

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

/// Whether the overlay's see-through can be switched while it's up. On macOS
/// it's the window's opaque flag, which AppKit lets us flip at any time. Where
/// it's decided by how the window and its swapchain were made — Windows'
/// DirectComposition swapchain, Linux's compositor surface — a switch waits
/// for a restart.
pub const LIVE_TRANSPARENCY: bool = backend::LIVE_TRANSPARENCY;

/// Makes the overlay opaque, or see-through again. A no-op where
/// [`LIVE_TRANSPARENCY`] is false. Main thread only.
pub fn set_overlay_opaque(opaque: bool) {
    backend::set_overlay_opaque(opaque);
}

/// Whether the desktop is COSMIC, which names itself among
/// `XDG_CURRENT_DESKTOP`'s names (`COSMIC`, or `pop:COSMIC` on Pop!_OS).
#[cfg(not(any(target_os = "macos", windows)))]
pub fn on_cosmic() -> bool {
    std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|desktops| names_cosmic(&desktops))
}

#[cfg(any(not(any(target_os = "macos", windows)), test))]
fn names_cosmic(desktops: &str) -> bool {
    desktops.split(':').any(|name| name.eq_ignore_ascii_case("cosmic"))
}

/// Whether this platform can put a picture under the overlay
/// (`config::OverlayBackdrop::Wallpaper` / `Image`): in a window of its own
/// under the board's on macOS, drawn in the board's own on Linux
/// ([`PICTURE_IN_WINDOW`]). Not on Windows yet.
pub const SUPPORTS_BACKDROP_PICTURE: bool = backend::SUPPORTS_BACKDROP_PICTURE;

/// Whether the app draws the picture itself, in the board's own window and
/// under the board — where it can't have a window of its own put under the
/// overlay's, as on Linux, where Wayland gives an app no way to order its
/// windows. The board's window is then opaque under a picture, where on
/// macOS it's see-through over the picture's window. The board draws the
/// picture the platform prepared, through [`preview_backdrop_picture`].
pub const PICTURE_IN_WINDOW: bool = backend::PICTURE_IN_WINDOW;

/// Whether the desktop's wallpaper can be found here, so offered as a
/// background. Looked up once a launch.
pub fn has_wallpaper() -> bool {
    backend::has_wallpaper()
}

/// What the picture under the overlay is of.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BackdropPicture {
    /// The desktop's wallpaper — one per launch, from the Space the app
    /// started on.
    Wallpaper,
    /// An image file of your own.
    File(std::path::PathBuf),
}

/// Chooses the picture under the overlay, blurred by `blur` (a multiple of
/// its own blur, `Config::overlay_blur`), and starts preparing it in the
/// background, once per picture and blur, so the summon that shows it doesn't
/// wait. Main thread only; a no-op where [`SUPPORTS_BACKDROP_PICTURE`] is
/// false.
pub fn set_backdrop_picture(picture: BackdropPicture, blur: f32) {
    backend::set_backdrop_picture(picture, blur);
}

/// Readies `picture` as the overlay would show it at `blur` and hands it to
/// `ready` as PNG once it's done, from a background thread, or `None` if it
/// couldn't be read. The settings form previews with it, so a tile shows what
/// choosing it gives, and choosing it afterwards costs nothing more. Main
/// thread only; `ready(None)` at once where [`SUPPORTS_BACKDROP_PICTURE`] is
/// false.
pub fn preview_backdrop_picture(
    picture: BackdropPicture,
    blur: f32,
    ready: impl FnOnce(Option<Vec<u8>>) + Send + 'static,
) {
    backend::preview_backdrop_picture(picture, blur, ready);
}

/// Shows the chosen picture under the overlay at `alpha` — faded with the
/// board as it comes and goes — or puts it away at 0. Main thread only; a
/// no-op where the app draws the picture itself ([`PICTURE_IN_WINDOW`]).
pub fn show_backdrop_picture(alpha: f32) {
    backend::show_backdrop_picture(alpha);
}

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
/// pile up copies fighting over the global hotkey. Unsaved edits go with it, so
/// on macOS and Linux it's the last resort, for a copy that didn't answer a
/// launch (`summon::hand_over`); Windows has no socket yet and does it always.
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
/// are launched — a guard so a card's link can't fire an arbitrary URL handler
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

#[cfg(test)]
mod tests {
    use super::names_cosmic;

    #[test]
    fn cosmic_is_found_among_the_desktops_named_whatever_its_case() {
        assert!(names_cosmic("COSMIC"));
        assert!(names_cosmic("pop:COSMIC"));
        assert!(!names_cosmic("GNOME"));
        assert!(!names_cosmic("ubuntu:GNOME"));
        assert!(!names_cosmic(""));
    }
}
