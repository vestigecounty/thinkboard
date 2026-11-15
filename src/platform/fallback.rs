//! No-op platform backend for targets without a desktop-overlay implementation
//! (Linux/BSD, for now). The app runs as an ordinary window; the summon hotkey
//! and canvas still work — only the always-on-top, all-workspaces overlay
//! treatment is skipped.
//
// On Linux the consts that describe the desktop (`WATCHES_WORKSPACE`,
// `LIVE_TRANSPARENCY`, the `SUPPORTS_*` family) are decided by the facade
// (false for every Linux desktop), so they are not re-exported here; only the
// no-op functions the facade dispatches to live on.

/// Nothing to do: the window stays whatever iced/winit created.
pub fn configure_overlay_window() {}

/// No observer to install; the handler is dropped.
pub fn set_workspace_change_handler(_handler: impl Fn() + Send + Sync + 'static) {}

/// No pinch monitor to install.
pub fn set_magnify_handler(_handler: impl Fn(f32, f32, f32) + Send + Sync + 'static) {}

/// No gesture handler to register.
pub fn set_gesture_handler(_handler: impl Fn() + Send + Sync + 'static) {}

/// No gesture tap to toggle (see [`SUPPORTS_GESTURES`]).
pub fn set_summon_gesture_enabled(_enable: bool) {}

/// No safe-area inset to account for on this platform.
/// Hiding the window already hands focus to whatever is behind it here.
pub fn release_focus() {}

pub fn top_safe_inset() -> f32 {
    0.0
}

/// Nothing to switch: the overlay's transparency is fixed when its window is
/// made, so switching it waits for a restart (the facade's Linux
/// `LIVE_TRANSPARENCY` is decided per-desktop, not by this backend).
pub fn set_overlay_opaque(_opaque: bool) {}

/// Nothing to choose — no window of our own under the overlay here.
pub fn set_backdrop_picture(_picture: super::BackdropPicture, _blur: f32) {}

/// Nothing to preview (see [`set_backdrop_picture`]).
pub fn preview_backdrop_picture(
    _picture: super::BackdropPicture,
    _blur: f32,
    ready: impl FnOnce(Option<Vec<u8>>) + Send + 'static,
) {
    ready(None);
}

/// Nothing to show (see [`set_backdrop_picture`]).
pub fn show_backdrop_picture(_alpha: f32) {}
