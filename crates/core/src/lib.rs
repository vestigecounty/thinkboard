//! ThinkBoard core — the UI-agnostic half of the app.
//!
//! Everything here is pure document/text logic with no dependency on iced,
//! wgpu, winit, or any GUI toolkit, so it can back both the desktop app (the
//! root `thinkboard` crate) and a future mobile front-end. New code that
//! doesn't need the UI belongs here.

// Test helpers, shared with the desktop crate's tests through the `fixtures`
// feature — off in any real build, so the iOS app links none of it.
#[cfg(any(test, feature = "fixtures"))]
pub mod fixtures;
pub mod ai;
pub mod cluster;
pub mod color;
pub mod fuzzy;
pub mod history;
pub mod layout;
pub mod markdown;
pub mod model;
pub mod obsidian;
pub mod search;
pub mod snap;
pub mod syntax;
pub mod theme;
pub mod vault;
