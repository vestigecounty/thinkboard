//! ThinkBoard's own theme format.
//!
//! A theme is **data, not drawing** — which is why it lives here rather than
//! beside the renderer. Each front-end maps the resolved [`Rgba`] values onto
//! its own colour type at the boundary, exactly as it already does for canvas
//! colours.
//!
//! # A theme is a patch
//!
//! Every section of a theme file is optional, and what it doesn't say is left
//! to the app's built-in default. That isn't laziness — almost nothing that
//! produces a theme knows every value. A base16 scheme is sixteen colours and
//! no fonts; an Obsidian stylesheet is semantic roles and no palette; a
//! hand-written tweak is usually one line. Requiring completeness would mean
//! every importer inventing values it has no opinion about.
//!
//! # Two ways to say the same thing
//!
//! [`Palette`] is the sixteen-colour scheme the popular themes are actually
//! published as — Solarized, Nord, Dracula, Gruvbox and several hundred others
//! all ship in this shape. [`derive`] maps it onto the roles the app draws
//! with, once, so adopting a scheme is sixteen values and nothing else.
//!
//! [`Colors`] names those roles directly, and overrides whatever the palette
//! derived. An Obsidian import writes only this half, since CSS gives roles
//! rather than a palette.

use serde::{Deserialize, Serialize};

use crate::color::Rgba;

/// Whether a theme is meant for a dark or a light board. Themes are grouped by
/// it in the picker, and it's what a front-end follows the system appearance
/// with.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    #[default]
    Dark,
    Light,
}

/// A theme as written on disk.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct ThemeFile {
    /// Format version, so a later shape can be told from this one rather than
    /// silently half-parsed.
    pub format: u32,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    /// Where it came from, for a re-import to find its way back.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub appearance: Appearance,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub palette: Option<Palette>,
    pub colors: Colors,
    pub metrics: Metrics,
    pub fonts: Fonts,
}

impl Default for ThemeFile {
    fn default() -> Self {
        Self {
            format: FORMAT,
            name: String::new(),
            author: None,
            license: None,
            source: None,
            appearance: Appearance::default(),
            palette: None,
            colors: Colors::default(),
            metrics: Metrics::default(),
            fonts: Fonts::default(),
        }
    }
}

/// The format version this build writes and understands.
pub const FORMAT: u32 = 1;

/// A base16 palette: sixteen colours in the slots the scheme ecosystem agreed
/// on. Stored as hex strings so a theme file stays something a person can read
/// and edit.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Palette {
    pub base00: String,
    pub base01: String,
    pub base02: String,
    pub base03: String,
    pub base04: String,
    pub base05: String,
    pub base06: String,
    pub base07: String,
    pub base08: String,
    pub base09: String,
    #[serde(rename = "base0A")]
    pub base0a: String,
    #[serde(rename = "base0B")]
    pub base0b: String,
    #[serde(rename = "base0C")]
    pub base0c: String,
    #[serde(rename = "base0D")]
    pub base0d: String,
    #[serde(rename = "base0E")]
    pub base0e: String,
    #[serde(rename = "base0F")]
    pub base0f: String,
}

impl Palette {
    /// The sixteen slots in order, parsed. A colour that doesn't parse is
    /// `None` and simply doesn't override anything.
    fn slots(&self) -> [Option<Rgba>; 16] {
        let p = |s: &String| crate::model::parse_color(s);
        [
            p(&self.base00),
            p(&self.base01),
            p(&self.base02),
            p(&self.base03),
            p(&self.base04),
            p(&self.base05),
            p(&self.base06),
            p(&self.base07),
            p(&self.base08),
            p(&self.base09),
            p(&self.base0a),
            p(&self.base0b),
            p(&self.base0c),
            p(&self.base0d),
            p(&self.base0e),
            p(&self.base0f),
        ]
    }
}

/// The roles the app draws with. Every field optional: absent means "whatever
/// the layer below decided".
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Colors {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub card_fill: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_muted: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub border: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bold: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub italic: Option<String>,
    /// Six heading colours, h1 first.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headings: Option<Vec<String>>,
}

/// Shape, rather than colour. World units, as the board measures in.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Metrics {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub card_radius: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame_radius: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub border_width: Option<f32>,
    /// Drop-shadow strength, 0 to 1.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shadow: Option<f32>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Fonts {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mono: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weight_normal: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weight_bold: Option<u16>,
}

/// A theme with its two colour layers flattened: the palette's derivation, then
/// the explicit roles on top. Fields still optional — what nothing supplied is
/// left for the front-end's own default.
#[derive(Debug, Clone, Default)]
pub struct Resolved {
    pub background: Option<Rgba>,
    pub card_fill: Option<Rgba>,
    pub text: Option<Rgba>,
    pub text_muted: Option<Rgba>,
    pub border: Option<Rgba>,
    pub accent: Option<Rgba>,
    pub link: Option<Rgba>,
    pub code: Option<Rgba>,
    pub bold: Option<Rgba>,
    pub italic: Option<Rgba>,
    pub headings: Option<[Rgba; 6]>,
    pub metrics: Metrics,
    pub fonts: Fonts,
}

impl ThemeFile {
    /// Parses a theme file. A format from the future is refused rather than
    /// read as though it were this one.
    pub fn parse(json: &str) -> Result<Self, String> {
        let file: ThemeFile = serde_json::from_str(json).map_err(|e| e.to_string())?;
        if file.format > FORMAT {
            return Err(format!(
                "theme \"{}\" is format {} and this build understands {FORMAT}",
                file.name, file.format
            ));
        }
        Ok(file)
    }

    /// Flattens the palette and the explicit roles into one set of values.
    pub fn resolve(&self) -> Resolved {
        let mut out = match &self.palette {
            Some(palette) => derive(palette),
            None => Resolved::default(),
        };

        let c = &self.colors;
        let take = |slot: &mut Option<Rgba>, value: &Option<String>| {
            if let Some(parsed) = value.as_deref().and_then(crate::model::parse_color) {
                *slot = Some(parsed);
            }
        };
        take(&mut out.background, &c.background);
        take(&mut out.card_fill, &c.card_fill);
        take(&mut out.text, &c.text);
        take(&mut out.text_muted, &c.text_muted);
        take(&mut out.border, &c.border);
        take(&mut out.accent, &c.accent);
        take(&mut out.link, &c.link);
        take(&mut out.code, &c.code);
        take(&mut out.bold, &c.bold);
        take(&mut out.italic, &c.italic);

        if let Some(list) = &c.headings {
            // All six or none: a partial list would leave some levels from the
            // palette and some from the file, which is nobody's intent.
            let parsed: Vec<Rgba> = list
                .iter()
                .filter_map(|h| crate::model::parse_color(h))
                .collect();
            if parsed.len() == 6 {
                out.headings = Some([
                    parsed[0], parsed[1], parsed[2], parsed[3], parsed[4], parsed[5],
                ]);
            }
        }

        out.metrics = self.metrics.clone();
        out.fonts = self.fonts.clone();
        out
    }
}

/// Maps a base16 palette onto the roles the app draws with.
///
/// One function for the whole ecosystem: the slots have agreed meanings, so
/// this transfers across every scheme rather than being tuned per theme.
///
/// ```text
///   base00 default background   base08 red      variables, errors
///   base01 raised surface       base09 orange   constants, numbers
///   base02 selection            base0A yellow   classes
///   base03 comments (dimmest)   base0B green    strings
///   base04 muted foreground     base0C cyan     escapes, support
///   base05 default foreground   base0D blue     functions, headings
///   base06 bright foreground    base0E purple   keywords
///   base07 brightest background base0F brown    deprecated
/// ```
pub fn derive(palette: &Palette) -> Resolved {
    let s = palette.slots();
    let mut out = Resolved {
        background: s[0x0],
        // A card is a raised surface over the board, which is what base01 is.
        card_fill: s[0x1],
        border: s[0x2],
        // Filled in below: base04 by convention, if it can be read.
        text_muted: s[0x4],
        text: s[0x5],
        accent: s[0xD],
        link: s[0xD],
        // Filled in below: orange by convention, but only if it can be read.
        code: s[0x9],
        // Schemes don't colour emphasis, so leave it to the text colour.
        bold: s[0x5],
        italic: s[0x5],
        headings: None,
        metrics: Metrics::default(),
        fonts: Fonts::default(),
    };

    // Inline code is orange by convention across the ecosystem, and that
    // matches the app's own default — but a scheme tuned for low contrast can
    // put orange somewhere genuinely hard to read. Solarized's is 2.8:1 against
    // its own card colour, which is under even the threshold for large text. So
    // orange if it reads, and otherwise whichever of the warm-to-cool accents
    // reads best. The conventional choice is kept wherever it's legible, and
    // legibility wins where it isn't.
    if let Some(card) = out.card_fill {
        // Muted text is base04 — *not* base03, which is comment-dim by design
        // and unreadable on a card in every scheme (1.4 to 2.9 against base01).
        // But base04 isn't dependable either: several ports put a surface
        // colour there rather than a foreground one, and Catppuccin's lands at
        // 2.6. Where it doesn't read, muted text is the body text moved part of
        // the way toward the card, which is what "muted" means anyway and can't
        // be illegible unless the body text already is.
        if !out
            .text_muted
            .is_some_and(|c| contrast(c, card) >= ACCENT_CONTRAST)
        {
            out.text_muted = out.text.map(|text| {
                // The most muted value that still reads: step back toward the
                // body text until it clears the bar. Body text itself always
                // does, so this always finds something.
                [0.45, 0.35, 0.25, 0.15, 0.0]
                    .into_iter()
                    .map(|toward| mix(text, card, toward))
                    .find(|c| contrast(*c, card) >= ACCENT_CONTRAST)
                    .unwrap_or(text)
            });
        }

        let readable = |c: Option<Rgba>| c.is_some_and(|c| contrast(c, card) >= BODY_CONTRAST);
        if !readable(out.code) {
            out.code = [s[0x9], s[0xA], s[0xB], s[0xC]]
                .into_iter()
                .flatten()
                .max_by(|a, b| {
                    contrast(*a, card)
                        .partial_cmp(&contrast(*b, card))
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .or(out.code);
        }
    }

    // base16 says nothing about a heading scale, so grade one: h1 fully
    // accent, h6 nearly body text. Calmer than rotating hues, and it doesn't
    // assume a scheme's greens and purples look good beside each other.
    if let (Some(accent), Some(text)) = (s[0xD], s[0x5]) {
        let mut headings = [accent; 6];
        for (level, slot) in headings.iter_mut().enumerate() {
            *slot = mix(accent, text, level as f32 / 5.0);
        }
        out.headings = Some(headings);
    }
    out
}

/// `a` moved `t` of the way toward `b`.
fn mix(a: Rgba, b: Rgba, t: f32) -> Rgba {
    let lerp = |x: f32, y: f32| x + (y - x) * t;
    Rgba {
        r: lerp(a.r, b.r),
        g: lerp(a.g, b.g),
        b: lerp(a.b, b.b),
        a: lerp(a.a, b.a),
    }
}

/// Relative luminance, per WCAG.
fn luminance(c: Rgba) -> f32 {
    let channel = |v: f32| {
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(c.r) + 0.7152 * channel(c.g) + 0.0722 * channel(c.b)
}

/// What WCAG asks of body text.
pub const BODY_CONTRAST: f32 = 4.5;
/// What it asks of large text and of interface components — the right bar for
/// an accent, a link or a border, which are read as shapes as much as glyphs.
pub const ACCENT_CONTRAST: f32 = 3.0;

/// Contrast ratio between two colours, 1.0 (identical) to 21.0 (black on
/// white).
pub fn contrast(a: Rgba, b: Rgba) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

impl Resolved {
    /// Roles that would be hard to read on a card, as `(role, ratio, wanted)`.
    ///
    /// Worth checking at import: a palette can be perfectly pleasant in a
    /// terminal and still put text somewhere unreadable on a card, and the
    /// person adopting it has no way to know before they see it.
    ///
    /// The bar differs by role rather than being one number. Body text is held
    /// to [`BODY_CONTRAST`]; an accent, a link or inline code is held to
    /// [`ACCENT_CONTRAST`], which is what WCAG asks of large text and interface
    /// components. Holding a whole scheme to the body threshold would reject
    /// Solarized, whose low contrast is the point of it rather than a defect.
    pub fn contrast_warnings(&self) -> Vec<(&'static str, f32, f32)> {
        let Some(card) = self.card_fill else {
            return Vec::new();
        };
        [
            ("text", self.text, BODY_CONTRAST),
            ("text_muted", self.text_muted, ACCENT_CONTRAST),
            ("link", self.link, ACCENT_CONTRAST),
            ("code", self.code, ACCENT_CONTRAST),
            ("accent", self.accent, ACCENT_CONTRAST),
        ]
        .into_iter()
        .filter_map(|(role, color, wanted)| {
            let ratio = contrast(color?, card);
            (ratio < wanted).then_some((role, ratio, wanted))
        })
        .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Solarized Dark, in base16 order.
    const SOLARIZED: &str = r##"{
      "format": 1, "name": "Solarized Dark", "appearance": "dark",
      "palette": {
        "base00": "#002b36", "base01": "#073642", "base02": "#586e75", "base03": "#657b83",
        "base04": "#839496", "base05": "#93a1a1", "base06": "#eee8d5", "base07": "#fdf6e3",
        "base08": "#dc322f", "base09": "#cb4b16", "base0A": "#b58900", "base0B": "#859900",
        "base0C": "#2aa198", "base0D": "#268bd2", "base0E": "#6c71c4", "base0F": "#d33682"
      }
    }"##;

    #[test]
    fn a_palette_alone_yields_a_whole_theme() {
        let file = ThemeFile::parse(SOLARIZED).expect("parses");
        let theme = file.resolve();
        assert_eq!(theme.background, Some(Rgba::from_rgb8(0x00, 0x2b, 0x36)));
        assert_eq!(theme.card_fill, Some(Rgba::from_rgb8(0x07, 0x36, 0x42)));
        assert_eq!(theme.text, Some(Rgba::from_rgb8(0x93, 0xa1, 0xa1)));
        assert_eq!(theme.accent, Some(Rgba::from_rgb8(0x26, 0x8b, 0xd2)));
        // Sixteen lines in, every role out — which is the point of the palette
        // layer, since that's the shape these schemes are published in.
        assert!(theme.headings.is_some());
        assert!(theme.text_muted.is_some());
        assert!(theme.code.is_some());
    }

    #[test]
    fn muted_text_is_readable_on_a_card() {
        // base03 is the tempting slot ("comments") and is unreadable on base01.
        // This is the check that made the mapping use base04 instead.
        let theme = ThemeFile::parse(SOLARIZED).unwrap().resolve();
        let (muted, card) = (theme.text_muted.unwrap(), theme.card_fill.unwrap());
        let comments = Rgba::from_rgb8(0x65, 0x7b, 0x83); // base03
        assert!(contrast(muted, card) > contrast(comments, card));
        assert!(theme.contrast_warnings().iter().all(|(role, ..)| *role != "text"));
    }

    #[test]
    fn explicit_roles_override_what_the_palette_derived() {
        let json = SOLARIZED.replace(
            r##""appearance": "dark","##,
            r##""appearance": "dark", "colors": { "accent": "#ff0000" },"##,
        );
        let theme = ThemeFile::parse(&json).unwrap().resolve();
        assert_eq!(theme.accent, Some(Rgba::from_rgb8(0xff, 0, 0)));
        // …and only that one: the rest still comes from the palette.
        assert_eq!(theme.link, Some(Rgba::from_rgb8(0x26, 0x8b, 0xd2)));
    }

    #[test]
    fn a_theme_may_say_almost_nothing() {
        // The shape an Obsidian import writes: roles, no palette. Everything it
        // doesn't mention stays None for the front-end's default to fill.
        let file = ThemeFile::parse(
            r##"{ "format": 1, "name": "Just an accent", "colors": { "accent": "#4c8dff" } }"##,
        )
        .unwrap();
        let theme = file.resolve();
        assert_eq!(theme.accent, Some(Rgba::from_rgb8(0x4c, 0x8d, 0xff)));
        assert!(theme.background.is_none());
        assert!(theme.text.is_none());
        assert_eq!(file.appearance, Appearance::Dark);
    }

    #[test]
    fn a_partial_heading_list_is_ignored_rather_than_half_applied() {
        let json = SOLARIZED.replace(
            r##""appearance": "dark","##,
            r##""appearance": "dark", "colors": { "headings": ["#ff0000", "#00ff00"] },"##,
        );
        let theme = ThemeFile::parse(&json).unwrap().resolve();
        let headings = theme.headings.expect("the palette's grade");
        assert_ne!(headings[0], Rgba::from_rgb8(0xff, 0, 0));
    }

    #[test]
    fn a_format_from_the_future_is_refused() {
        let err = ThemeFile::parse(r##"{ "format": 99, "name": "Later" }"##).unwrap_err();
        assert!(err.contains("format 99"), "{err}");
    }

    #[test]
    fn contrast_is_the_wcag_ratio() {
        let black = Rgba::from_rgb8(0, 0, 0);
        let white = Rgba::from_rgb8(255, 255, 255);
        assert!((contrast(black, white) - 21.0).abs() < 0.01);
        assert!((contrast(white, white) - 1.0).abs() < 0.01);
    }
}
