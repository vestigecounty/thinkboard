//! ThinkBoard core — the UI-agnostic half of the app.
//!
//! Everything here is pure document/text logic with no dependency on iced,
//! wgpu, winit, or any GUI toolkit, so it can back both the desktop app (the
//! root `thinkboard` crate) and a future mobile front-end. New code that
//! doesn't need the UI belongs here.

pub mod history;
pub mod markdown;
pub mod syntax;
