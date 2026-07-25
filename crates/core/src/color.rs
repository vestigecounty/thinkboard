//! A minimal, UI-agnostic RGBA color.
//!
//! The core parses and manipulates canvas colors without knowing about any
//! GUI toolkit; each front-end converts [`Rgba`] to its own color type at the
//! render boundary (the desktop app bridges to `iced::Color`, which shares this
//! `{r, g, b, a}` f32 layout). Channels are linear-ish 0..1 floats, matching
//! how iced stores color.

/// An RGBA color with each channel in `0.0..=1.0`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rgba {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Rgba {
    /// Opaque color from 8-bit channels (each mapped `n / 255`).
    pub fn from_rgb8(r: u8, g: u8, b: u8) -> Self {
        Self {
            r: r as f32 / 255.0,
            g: g as f32 / 255.0,
            b: b as f32 / 255.0,
            a: 1.0,
        }
    }

    /// Opaque color from float channels.
    pub fn from_rgb(r: f32, g: f32, b: f32) -> Self {
        Self { r, g, b, a: 1.0 }
    }
}
