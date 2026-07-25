pub mod board;
pub mod camera;
pub mod hue_picker;
pub mod md_edit;
pub mod shadows;
pub mod text_raster;

// `markdown` and `syntax` moved to the UI-agnostic `thinkboard-core` crate;
// re-export them here so `crate::canvas::markdown` / `super::markdown` paths in
// the rendering code keep resolving.
pub use thinkboard_core::{markdown, syntax};
