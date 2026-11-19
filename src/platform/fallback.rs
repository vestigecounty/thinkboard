//! No-op platform backend for targets without a desktop-overlay implementation
//! (Linux/BSD, for now). The app runs as an ordinary window; the summon hotkey
//! (under X11; Wayland asks the desktop for it, `app::summon`) and canvas still
//! work — only the always-on-top, all-workspaces overlay treatment is skipped.

/// No workspace-change notifications, so the overlay never auto-dismisses.
pub const WATCHES_WORKSPACE: bool = false;

/// Nothing to do: the window stays whatever iced/winit created.
pub fn configure_overlay_window() {}

/// No observer to install; the handler is dropped.
pub fn set_workspace_change_handler(_handler: impl Fn() + Send + Sync + 'static) {}

/// No native pinch gestures on this platform.
pub const SUPPORTS_PINCH: bool = false;

/// No pinch monitor to install (see [`SUPPORTS_PINCH`]).
pub fn set_magnify_handler(_handler: impl Fn(f32, f32, f32) + Send + Sync + 'static) {}

/// Nothing says where a scroll begins here; a pause ends one.
pub fn watch_scrolls() {}

/// Nothing counted (see [`watch_scrolls`]).
pub fn scrolls_begun() -> Option<u64> {
    None
}

/// No raw trackpad-gesture recognition on this platform.
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
// under the board (`PICTURE_IN_WINDOW`): Wayland gives an app no way to order
// one of its windows under another. Prepared and decoded as the other
// platforms' are (`platform::picture`) — JPEG and PNG, which wallpapers here
// mostly are.

use super::BackdropPicture;
use super::picture;

pub const SUPPORTS_BACKDROP_PICTURE: bool = true;

/// Drawn by the app, under the board.
pub const PICTURE_IN_WINDOW: bool = true;

/// Where Linux's pictures come from: the desktop's wallpaper as each desktop
/// keeps it (`platform::wallpaper`), decoded here.
const SOURCE: picture::Source = picture::Source { wallpaper: wallpaper_file, screen: no_screen, make: picture::decoded };

pub fn has_wallpaper() -> bool {
    super::wallpaper::find().is_some()
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
    super::wallpaper::find().map(|path| path.to_string_lossy().into_owned())
}

/// Nothing to ask for the screen's size before there's a window on it; the
/// preparation takes a 4K one.
fn no_screen() -> Option<u32> {
    None
}

