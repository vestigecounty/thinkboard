//! No-op platform backend for targets without a desktop-overlay implementation
//! (Linux/BSD, for now). The app runs as an ordinary window; the summon hotkey
//! and canvas still work — only the always-on-top, all-workspaces overlay
//! treatment is skipped.

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

/// No raw trackpad-gesture recognition on this platform.
pub const SUPPORTS_GESTURES: bool = false;

/// No gesture handler to register (see [`SUPPORTS_GESTURES`]).
pub fn set_gesture_handler(_handler: impl Fn() + Send + Sync + 'static) {}

/// No gesture tap to toggle (see [`SUPPORTS_GESTURES`]).
pub fn set_summon_gesture_enabled(_enable: bool) {}

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
