pub mod cluster;

// `model` (the canvas document) and `layout` moved to the UI-agnostic
// `thinkboard-core` crate; re-export them so `crate::document::model` /
// `crate::model` paths keep resolving.
pub use thinkboard_core::{layout, model};

use iced::Color;
use thinkboard_core::color::Rgba;

/// Bridge the core's UI-agnostic [`Rgba`] to iced's `Color` at the render
/// boundary — the two share the same `{r, g, b, a}` f32 layout. Used where the
/// document color helpers (`model::palette_color`, `hsv`, `parse_color`, …)
/// feed into iced widgets.
pub(crate) trait IntoColor {
    fn into_color(self) -> Color;
}

impl IntoColor for Rgba {
    fn into_color(self) -> Color {
        Color { r: self.r, g: self.g, b: self.b, a: self.a }
    }
}
