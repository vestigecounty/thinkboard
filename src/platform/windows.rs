//! Windows backend for the desktop-overlay treatment.
//!
//! iced/winit produces the transparent, undecorated, always-on-top window; the
//! extra Windows-specific step is to make it *non-activating* (so summoning the
//! overlay never steals focus from the app underneath) and to keep it pinned
//! topmost over the primary monitor. This is the Win32 analogue of the macOS
//! NSPanel morph in [`super::macos`].
//!
//! Verification status: written against `windows` 0.58 but not yet built on a
//! Windows host from this repo (the dev machine is macOS-only). Expect to
//! iterate on API details — a few windows-rs signatures (`Result`-returning
//! calls, `HWND` vs `Option<HWND>` params) shift between crate versions. It is
//! `#[cfg(windows)]`, so it never touches the macOS build.
//!
//! Not yet implemented (best-effort / future work):
//! - Covering true exclusive-fullscreen apps (topmost only reliably sits over
//!   borderless-fullscreen windows).
//! - Pinning to every virtual desktop (needs the undocumented
//!   `IVirtualDesktopManagerInternal` COM interface); [`WATCHES_WORKSPACE`] is
//!   false so the overlay simply doesn't auto-dismiss on desktop switches.

use windows::Win32::Foundation::{BOOL, FALSE, HWND, LPARAM, TRUE};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{
    EnumThreadWindows, GWL_EXSTYLE, GW_OWNER, GetSystemMetrics, GetWindow, GetWindowLongPtrW,
    HWND_TOPMOST, IsWindowVisible, SM_CXSCREEN, SM_CYSCREEN, SPI_GETDESKWALLPAPER,
    SWP_NOACTIVATE, SWP_SHOWWINDOW, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SetWindowLongPtrW,
    SetWindowPos, SystemParametersInfoW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
};

/// Windows has no macOS-style Space-change notification we track, so the
/// overlay doesn't auto-dismiss on a virtual-desktop switch.
pub const WATCHES_WORKSPACE: bool = false;

/// No workspace observer to install (see [`WATCHES_WORKSPACE`]).
pub fn set_workspace_change_handler(_handler: impl Fn() + Send + Sync + 'static) {}

/// Trackpad pinch isn't wired natively on Windows (use Ctrl+scroll to zoom).
pub const SUPPORTS_PINCH: bool = false;

/// No pinch monitor to install (see [`SUPPORTS_PINCH`]).
pub fn set_magnify_handler(_handler: impl Fn(f32, f32, f32) + Send + Sync + 'static) {}

/// Raw trackpad-gesture recognition isn't wired natively on Windows.
pub const SUPPORTS_GESTURES: bool = false;

/// No gesture handler to register (see [`SUPPORTS_GESTURES`]).
pub fn set_gesture_handler(_handler: impl Fn() + Send + Sync + 'static) {}

/// No gesture tap to toggle (see [`SUPPORTS_GESTURES`]).
pub fn set_summon_gesture_enabled(_enable: bool) {}

/// Opening the app again is a second launch here, which hands over.
pub const HEARS_REOPEN: bool = false;

/// No reopen to hear (see [`HEARS_REOPEN`]).
pub fn set_reopen_handler(_handler: impl Fn() + Send + Sync + 'static) {}

/// No reopen to hear (see [`HEARS_REOPEN`]).
pub fn listen_for_reopen() {}

/// Makes the app window a non-activating, taskbar-hidden, topmost overlay
/// covering the primary monitor.
pub fn configure_overlay_window() {
    let Some(hwnd) = app_window() else {
        return;
    };

    unsafe {
        // Non-activating (never steals focus), hidden from the taskbar and
        // Alt-Tab, always topmost.
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE)
            | WS_EX_TOPMOST.0 as isize
            | WS_EX_NOACTIVATE.0 as isize
            | WS_EX_TOOLWINDOW.0 as isize;
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex);

        // Cover the primary monitor and float above everything without taking
        // focus (mirrors the macOS `setFrame` + screen-saver level).
        let (w, h) = (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN));
        let _ = SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            0,
            0,
            w,
            h,
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
    }
}

/// The app's top-level window, found on the current (UI) thread — winit runs
/// its event loop on this thread and owns exactly one visible top-level window.
fn app_window() -> Option<HWND> {
    unsafe extern "system" fn find(hwnd: HWND, lparam: LPARAM) -> BOOL {
        // A visible, unowned (top-level) window is ours; stop at the first.
        let owned = unsafe { GetWindow(hwnd, GW_OWNER) }
            .map(|owner| !owner.is_invalid())
            .unwrap_or(false);
        if unsafe { IsWindowVisible(hwnd) }.as_bool() && !owned {
            unsafe { *(lparam.0 as *mut HWND) = hwnd };
            return FALSE; // found it — stop enumerating
        }
        TRUE
    }

    let mut found = HWND::default();
    unsafe {
        let _ = EnumThreadWindows(
            GetCurrentThreadId(),
            Some(find),
            LPARAM(&mut found as *mut HWND as isize),
        );
    }
    (!found.is_invalid()).then_some(found)
}

/// No safe-area inset to account for on this platform.
/// Hiding the window already hands focus to whatever is behind it here.
pub fn release_focus() {}

pub fn top_safe_inset() -> f32 {
    0.0
}

/// The overlay's transparency is fixed when its window is made, so switching
/// it waits for a restart.
pub const LIVE_TRANSPARENCY: bool = false;

/// Nothing to switch (see [`LIVE_TRANSPARENCY`]).
pub fn set_overlay_opaque(_opaque: bool) {}

// A picture under the overlay, drawn by the app in the board's own window,
// under the board, as on Linux (`PICTURE_IN_WINDOW`): one window rather than
// two to keep in order, and under a picture the board's window is opaque, so
// it doesn't need the see-through swapchain. Prepared and decoded as the other
// platforms' are (`platform::picture`).

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use super::BackdropPicture;
use super::picture;

pub const SUPPORTS_BACKDROP_PICTURE: bool = true;

/// Drawn by the app, under the board.
pub const PICTURE_IN_WINDOW: bool = true;

/// Where Windows' pictures come from: the desktop's wallpaper, as Windows
/// names it, at the primary screen's size.
const SOURCE: picture::Source =
    picture::Source { wallpaper: wallpaper_file, screen: primary_screen_pixels, make: picture::decoded };

pub fn has_wallpaper() -> bool {
    wallpaper().is_some()
}

pub fn set_backdrop_picture(picture: BackdropPicture, blur: f32) {
    picture::choose(picture, blur, &SOURCE);
}

pub fn preview_backdrop_picture(
    picture: BackdropPicture,
    blur: f32,
    ready: impl FnOnce(Option<Vec<u8>>) + Send + 'static,
) {
    picture::preview(picture, blur, &SOURCE, ready);
}

/// The app draws the picture itself (see [`PICTURE_IN_WINDOW`]).
pub fn show_backdrop_picture(_alpha: f32) {}

fn wallpaper_file() -> Option<String> {
    wallpaper().map(|path| path.to_string_lossy().into_owned())
}

/// The desktop's wallpaper, looked up once a launch: the picture Windows was
/// given — or, where that's gone or isn't a JPEG or PNG, the copy Windows
/// keeps of the one it shows, which is also a slideshow's current one. None
/// for a solid colour, which names no picture.
fn wallpaper() -> Option<&'static Path> {
    static FOUND: OnceLock<Option<PathBuf>> = OnceLock::new();
    FOUND
        .get_or_init(|| {
            let mut name = [0u16; 1024];
            let asked = unsafe {
                SystemParametersInfoW(
                    SPI_GETDESKWALLPAPER,
                    name.len() as u32,
                    Some(name.as_mut_ptr().cast()),
                    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
                )
            };
            asked.ok()?;
            let length = name.iter().position(|&c| c == 0).unwrap_or(name.len());
            let given = PathBuf::from(String::from_utf16_lossy(&name[..length]));
            if given.as_os_str().is_empty() {
                return None;
            }
            let readable = given
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| ["png", "jpg", "jpeg", "jfif"].contains(&extension.to_ascii_lowercase().as_str()));
            if readable && given.is_file() {
                return Some(given);
            }
            let kept = PathBuf::from(std::env::var_os("APPDATA")?).join(r"Microsoft\Windows\Themes\TranscodedWallpaper");
            kept.is_file().then_some(kept)
        })
        .as_deref()
}

/// The primary screen's longest side, in pixels.
fn primary_screen_pixels() -> Option<u32> {
    let (width, height) = unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
    u32::try_from(width.max(height)).ok().filter(|&pixels| pixels > 0)
}
