//! ThinkBoard — an iced port of the SwiftUI ThinkBoard canvas app.
//!
//! Loads an Obsidian `.canvas` document and supports pan with inertia,
//! smooth cursor-centered zoom, in-place editing, drag-out node creation,
//! semantic-zoom clusters, and autosave/reload. See `app` for the runtime.
//!
//! This is a library so the binary is a one-liner *and* so `benches/` can reach
//! the CPU-heavy paths that feed rendering — text measurement, rasterization
//! and clustering — which is otherwise impossible: benchmarks are external
//! crates and can only see a library's public API. Only those paths are
//! exported; everything else stays crate-private.

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
pub(crate) use canvas::camera;
pub(crate) use document::model;
pub(crate) use thinkboard_core::{ai, snap};
pub(crate) use app::{Message, SPAWN_DURATION, StagedSnapshot, UI_FONT};

// The benchable surface. These are also the flat aliases the leaf modules use
// (`crate::text_raster`, `crate::cluster`), so exporting them costs nothing.
pub use canvas::{geometry, text_raster};
pub use document::cluster;
pub use theme::Theme;

/// Runs the app: opens the overlay window and enters iced's event loop.
pub fn run() -> iced::Result {
    app::run()
}
