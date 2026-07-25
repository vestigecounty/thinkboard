//! ThinkBoard — an iced port of the SwiftUI ThinkBoard canvas app.
//!
//! Loads an Obsidian `.canvas` document and supports pan with inertia,
//! smooth cursor-centered zoom, in-place editing, drag-out node creation,
//! semantic-zoom clusters, and autosave/reload. See `app` for the runtime.

// On Windows, a release build is a GUI app (no console window), while debug
// builds keep the console so stderr (eprintln diagnostics, panics) stays
// visible. The `no-console` feature forces the GUI subsystem in any profile —
// e.g. `--features no-console` for a clean, console-free debug build to test.
#![cfg_attr(
    all(windows, any(not(debug_assertions), feature = "no-console")),
    windows_subsystem = "windows"
)]

mod app;
mod canvas;
mod config;
mod document;
mod platform;
mod theme;

// Flat aliases so leaf modules keep resolving the `crate::camera`-style
// paths they use, plus the app items they reference. `ai` and `snap` now
// live in `thinkboard-core`; re-export them so `crate::ai` / `crate::snap`
// still resolve.
pub(crate) use canvas::{camera, text_raster};
pub(crate) use document::{cluster, model};
pub(crate) use thinkboard_core::{ai, snap};
pub(crate) use app::{Message, SPAWN_DURATION, StagedSnapshot, UI_FONT};

fn main() -> iced::Result {
    // Route iced/wgpu's `log` output to stderr. Defaults to warnings; set e.g.
    // `RUST_LOG=iced_wgpu=info` to see the selected adapter, backend, and
    // surface alpha mode (useful for diagnosing overlay transparency).
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    app::run()
}
