//! Absolute-basic reading of node colors from an Obsidian `theme.css`.
//!
//! Only a few chrome colors are read, and only from plain values (`#hex`,
//! `rgb()`, `rgba()`) inside the theme's top-level `.theme-dark { ... }`
//! block. Anything using `var()`, `color-mix()`, `hsl()`, etc. is skipped so
//! the built-in default is kept. The canvas background is not themed (the
//! overlay dims the desktop instead). Font *families* are read by name; fonts
//! the theme *embeds* as base64 WOFF2 data URIs are decoded by
//! [`embedded_fonts`] so they render without being installed system-wide.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use cosmic_text::{Feature, FeatureTag};
use iced::Color;

/// Node/card colors: the built-in palette, optionally overridden by a theme.
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub card_fill: Color,
    pub text: Color,
    pub accent: Color,
    // UI chrome (the settings window, the find bar): read from the standard
    // Obsidian variables so app windows recolor with the theme too.
    /// Primary window/background surface (`--background-primary`).
    pub background: Color,
    /// Secondary text — descriptions, the result count (`--text-muted`).
    pub text_muted: Color,
    /// Hairline borders (`--background-modifier-border`).
    pub border: Color,
    // Markdown element colors.
    pub bold: Color,
    pub italic: Color,
    pub code: Color,
    pub link: Color,
    pub headings: [Color; 6],
    /// Theme font family names (`None` = built-in SF). Interned to `'static` so
    /// `Theme` stays `Copy` — see [`intern`], which is why scrolling the font
    /// picker doesn't leak a string per family it previews.
    pub font_text: Option<&'static str>,
    pub font_mono: Option<&'static str>,
    /// Body / bold weights (`--font-normal`/`--font-bold`), snapped to the
    /// nearest 100. Themes like Primary render Inter at 450/750, not 400/700.
    pub weight_normal: u16,
    pub weight_bold: u16,
    /// OpenType features to shape body and monospace text with.
    ///
    /// A font's stylistic alternates are substitution lookups that lie dormant
    /// unless asked for by tag — Inter carries a single-storey `a` as `cv11`
    /// and never shows it otherwise. Interned to `'static` like the family
    /// names, and for the same reason: `Theme` is `Copy`.
    pub font_features: &'static [Feature],
    pub font_features_mono: &'static [Feature],

    // --- Shape, rather than colour. World units; scaled by zoom at each use.
    /// Corner radius of a card. Every card kind shares it.
    pub card_radius: f32,
    /// Corner radius of the shapes that *contain* cards — a group's frame, a
    /// semantic-zoom cluster — rounded more generously so they read as
    /// containers rather than as oversized cards.
    pub frame_radius: f32,
}

impl Default for Theme {
    fn default() -> Self {
        // Mirrors the built-in board constants. (Default node/cluster outlines
        // use the link color, not a themed border, matching Obsidian.)
        let text = Color::from_rgb(0.86, 0.87, 0.87);
        let accent = Color::from_rgb(0.4, 0.72, 1.0);
        Self {
            card_fill: Color::from_rgb(0.16, 0.16, 0.17),
            text,
            accent,
            background: Color::from_rgb(0.12, 0.12, 0.13),
            text_muted: Color::from_rgb(0.60, 0.60, 0.64),
            border: Color::from_rgb(0.28, 0.28, 0.31),
            bold: text,
            italic: text,
            code: Color::from_rgb8(0xc8, 0x99, 0x6c),
            link: accent,
            headings: [accent; 6],
            font_text: None,
            font_mono: None,
            weight_normal: 400,
            weight_bold: 700,
            font_features: &[],
            font_features_mono: &[],
            card_radius: 8.0,
            frame_radius: 12.0,
        }
    }
}

impl Theme {
    /// The UI font: the theme's text family (`--font-text`) if set, else the
    /// platform default. For app chrome (settings, find bar) so it follows the
    /// theme's typeface.
    pub fn ui_font(&self) -> iced::Font {
        iced::Font {
            family: self
                .font_text
                .map(iced::font::Family::Name)
                .unwrap_or(iced::font::Family::Name(crate::platform::UI_FONT_NAME)),
            ..iced::Font::DEFAULT
        }
    }

    /// A raised surface for floating chrome (context menu, command palette, find
    /// bar): the node `card_fill` nudged toward the text color, so a menu always
    /// reads as elevated above the node cards in any theme — derived from the
    /// theme rather than a hardcoded panel color. (The settings modal keeps
    /// `background`, to meet its themed form without a seam.)
    pub fn elevated(&self) -> Color {
        const LIFT: f32 = 0.08;
        let lift = |base: f32, toward: f32| base + (toward - base) * LIFT;
        Color {
            r: lift(self.card_fill.r, self.text.r),
            g: lift(self.card_fill.g, self.text.g),
            b: lift(self.card_fill.b, self.text.b),
            a: 1.0,
        }
    }

    /// Applies a resolved theme file over this one.
    ///
    /// Only what the file actually said: a theme is a patch, so anything it
    /// left out keeps whatever this theme already had — which for a fresh load
    /// is the built-in default.
    pub fn apply_resolved(&mut self, resolved: &thinkboard_core::theme::Resolved) {
        use crate::document::IntoColor;
        let set = |slot: &mut Color, value: Option<thinkboard_core::color::Rgba>| {
            if let Some(value) = value {
                *slot = value.into_color();
            }
        };
        set(&mut self.background, resolved.background);
        set(&mut self.card_fill, resolved.card_fill);
        set(&mut self.text, resolved.text);
        set(&mut self.text_muted, resolved.text_muted);
        set(&mut self.border, resolved.border);
        set(&mut self.accent, resolved.accent);
        set(&mut self.link, resolved.link);
        set(&mut self.code, resolved.code);
        set(&mut self.bold, resolved.bold);
        set(&mut self.italic, resolved.italic);
        if let Some(headings) = resolved.headings {
            for (slot, value) in self.headings.iter_mut().zip(headings) {
                *slot = value.into_color();
            }
        }

        let m = &resolved.metrics;
        // Clamped, not trusted: a theme is a file anyone can hand-edit, and a
        // negative or enormous radius would corrupt every path on the board.
        if let Some(radius) = m.card_radius {
            self.card_radius = radius.clamp(0.0, 64.0);
        }
        if let Some(radius) = m.frame_radius {
            self.frame_radius = radius.clamp(0.0, 64.0);
        }

        let f = &resolved.fonts;
        if let Some(name) = &f.text {
            self.font_text = Some(intern(name));
        }
        if let Some(name) = &f.mono {
            self.font_mono = Some(intern(name));
        }
        if let Some(weight) = f.weight_normal {
            self.weight_normal = snap_weight(weight);
        }
        if let Some(weight) = f.weight_bold {
            self.weight_bold = snap_weight(weight);
        }
        // Written in the CSS syntax the fonts' own documentation uses, and
        // parsed by the same reader as a stylesheet's.
        if let Some(features) = &f.features {
            self.font_features = intern_features(parse_features(features));
        }
        if let Some(features) = &f.features_mono {
            self.font_features_mono = intern_features(parse_features(features));
        }
    }

    /// Overrides the body family with the user's own pick (`config.font`).
    ///
    /// Blank is "whatever the theme said", so the two compose rather than one
    /// disabling the other: a theme supplies a font, and naming one here
    /// overrules that part of it and leaves its colours alone. Applied last,
    /// wherever a theme is resolved — see `theme_store::resolve_base`, whose
    /// callers all pair with this.
    pub fn use_font(&mut self, family: &str) {
        let family = family.trim();
        if family.is_empty() {
            return;
        }
        self.font_text = Some(intern(family));
        // And the weights it actually ships. The shaper matches weight exactly,
        // so asking a family that has no 400 for a 400 doesn't get its regular —
        // it gets the system font, and the chosen typeface never appears. See
        // `text_raster::family_weights`. A family the database doesn't know
        // (one a theme is embedding in this same breath) keeps the theme's.
        if let Some((normal, bold)) = crate::text_raster::family_weights(family) {
            self.weight_normal = normal;
            self.weight_bold = bold;
        }
    }

    /// Reads overrides from an Obsidian `theme.css`, keeping the default for
    /// any color that's missing or not a plain value.
    pub fn from_css(css: &str) -> Self {
        let mut theme = Theme::default();
        theme.apply(&dark_variables(css));
        // Font families are often declared on `body`/`:root`, not inside
        // `.theme-dark`, so scan the whole sheet for them. Themes usually declare
        // the applied font only as `--font-text-theme` (which Obsidian falls back
        // to by default), so honor that suggestion too.
        theme.font_text = css_font(css, &["--font-text", "--font-text-theme"]);
        theme.font_mono = css_font(css, &["--font-monospace", "--font-monospace-theme"]);
        // Custom body/bold weights, snapped to the nearest standard 100 (iced's
        // font weight is an enum, and cosmic-text needs a face at exactly the
        // requested weight — so both the request and the synthesized face use
        // the snapped value).
        if let Some(w) = css_number(css, "--font-normal") {
            theme.weight_normal = snap_weight(w);
        }
        if let Some(w) = css_number(css, "--font-bold") {
            theme.weight_bold = snap_weight(w);
        }
        // The typography variables a theme applies with `font-feature-settings`.
        // Not an Obsidian-standard name — the app's own variable is honoured
        // first, then the one Primary (and the themes that follow it) uses.
        theme.font_features = css_features(css, &["--font-text-feature", "--text-font-feature"]);
        theme.font_features_mono = css_features(
            css,
            &["--font-monospace-feature", "--monospace-font-feature"],
        );
        theme
    }

    /// Overrides colors from a theme's variables, resolving one-hop `var()`
    /// chains (so `--bold-color: var(--color-d-red-30): hsla(...)` works) but
    /// still skipping `color-mix()` / unresolved values.
    fn apply(&mut self, vars: &HashMap<String, String>) {
        let get = |name: &str| {
            vars.get(name)
                .and_then(|v| resolve(v, vars, 8))
                .and_then(|v| parse_color(&v))
                .map(opaque)
        };

        if let Some(c) = get("--background-secondary") {
            self.card_fill = c;
        }
        if let Some(c) = get("--text-normal") {
            self.text = c;
        }
        if let Some(c) = get("--interactive-accent") {
            self.accent = c;
        }
        if let Some(c) = get("--background-primary") {
            self.background = c;
        }
        if let Some(c) = get("--text-muted") {
            self.text_muted = c;
        }
        if let Some(c) = get("--background-modifier-border") {
            self.border = c;
        }
        // Markdown element colors default to sensible bases when a theme
        // doesn't set them (or uses var()/color-mix()).
        self.bold = get("--bold-color").unwrap_or(self.text);
        self.italic = get("--italic-color").unwrap_or(self.text);
        if let Some(c) = get("--code-normal") {
            self.code = c;
        }
        self.link = get("--link-color")
            .or_else(|| get("--text-accent"))
            .unwrap_or(self.accent);
        for level in 1..=6usize {
            // Modern (`--h1-color`) then legacy (`--text-title-h1`) names.
            self.headings[level - 1] = get(&format!("--h{level}-color"))
                .or_else(|| get(&format!("--text-title-h{level}")))
                .unwrap_or(self.accent);
        }
    }
}

/// Parses a CSS `font-feature-settings` value — `"calt" 1,"cv11" 1,"case" 0` —
/// into the tags a shaper takes.
///
/// A bare tag means on, as it does in CSS, and `on`/`off` are accepted beside
/// the numeric form. A value that isn't four bytes isn't a feature tag, so it's
/// dropped rather than padded into some other feature.
fn parse_features(value: &str) -> Vec<Feature> {
    let mut features = Vec::new();
    for entry in value.split(',') {
        let entry = entry.trim();
        // Quoted, per the CSS grammar — which also delimits the tag from its
        // value without having to guess where one ends.
        let Some(rest) = entry.strip_prefix(['"', '\'']) else {
            continue;
        };
        let Some(close) = rest.find(['"', '\'']) else {
            continue;
        };
        let Ok(tag) = <[u8; 4]>::try_from(&rest.as_bytes()[..close]) else {
            continue;
        };
        let value = match rest[close + 1..].trim() {
            "" | "on" => 1,
            "off" => 0,
            number => match number.parse() {
                Ok(value) => value,
                Err(_) => continue,
            },
        };
        features.push(Feature { tag: FeatureTag::new(&tag), value });
    }
    features
}

/// The font features declared for any of `keys`, as a CSS custom property.
///
/// **Only the variables, never a bare `font-feature-settings` rule.** A theme
/// applies that rule under whatever selector it likes — Primary sets `"case"`
/// on one — and hoisting a scoped rule to the whole board would turn on
/// features somebody asked for in one place. The variables are the theme's
/// typography-wide statement, which is what this is.
fn css_features(css: &str, keys: &[&str]) -> &'static [Feature] {
    keys.iter()
        .find_map(|key| {
            css_declaration(css, key, |value| {
                // `var(--x)` is an indirection to another variable, which we
                // look up directly — following it would be a second parser.
                if value.starts_with("var(") {
                    return None;
                }
                let features = parse_features(value);
                (!features.is_empty()).then_some(features)
            })
        })
        .map(intern_features)
        .unwrap_or(&[])
}

/// The first concrete font family declared for any of `keys` anywhere in the
/// sheet (not just `.theme-dark`), leaked to `'static`. Generic families
/// (`sans-serif`, …) and unresolved `var()` values are skipped.
fn css_font(css: &str, keys: &[&str]) -> Option<&'static str> {
    keys.iter().find_map(|key| {
        css_declaration(css, key, |value| first_family(value).map(|f| intern(&f)))
    })
}

/// Hands `read` the value of each `<key>: …` declaration in the sheet, until it
/// returns something.
///
/// Every declaration, not just the first: a stylesheet may set the same
/// property several times, and only some of those values are ones we can use —
/// a `var()` indirection or a family list we can't resolve has to fall through
/// to the next rather than end the search.
///
/// The colon must follow the key immediately, so `--font-text` doesn't match
/// inside `--font-text-theme`.
fn css_declaration<'a, T>(
    css: &'a str,
    key: &str,
    mut read: impl FnMut(&'a str) -> Option<T>,
) -> Option<T> {
    let mut search = css;
    while let Some(idx) = search.find(key) {
        let after = &search[idx + key.len()..];
        search = after;
        let Some(value) = after.trim_start().strip_prefix(':') else {
            continue;
        };
        let end = value.find([';', '}']).unwrap_or(value.len());
        if let Some(found) = read(value[..end].trim()) {
            return Some(found);
        }
    }
    None
}

fn first_family(value: &str) -> Option<String> {
    let first = value
        .split(',')
        .next()?
        .trim()
        .trim_matches(|c| c == '"' || c == '\'')
        .trim();
    const GENERIC: [&str; 11] = [
        "sans-serif",
        "serif",
        "monospace",
        "system-ui",
        "ui-monospace",
        "ui-sans-serif",
        "cursive",
        "fantasy",
        "-apple-system",
        "BlinkMacSystemFont",
        "inherit",
    ];
    if first.is_empty() || first.starts_with("var(") || GENERIC.contains(&first) {
        return None;
    }
    Some(first.to_owned())
}

/// The first plain integer value declared for `--name` anywhere in the sheet
/// (e.g. `--font-normal: 450`). `var()`/`calc()` values are skipped.
fn css_number(css: &str, key: &str) -> Option<u16> {
    let mut search = css;
    while let Some(idx) = search.find(key) {
        let after = &search[idx + key.len()..];
        search = after;
        let Some(value) = after.trim_start().strip_prefix(':') else {
            continue;
        };
        let end = value.find([';', '}']).unwrap_or(value.len());
        if let Ok(n) = value[..end].trim().parse::<u16>() {
            return Some(n);
        }
    }
    None
}

/// Maps a numeric font weight to iced's `Weight` enum, the only weight the live
/// text path can request — which is why [`snap_weight`] exists at all: the
/// rasterizer instances a variable font's `wght` axis at any value, and iced
/// can only name nine of them, so both paths are held to the nine.
pub fn iced_weight(weight: u16) -> iced::font::Weight {
    use iced::font::Weight;
    match weight {
        0..=150 => Weight::Thin,
        151..=250 => Weight::ExtraLight,
        251..=350 => Weight::Light,
        351..=450 => Weight::Normal,
        451..=550 => Weight::Medium,
        551..=650 => Weight::Semibold,
        651..=750 => Weight::Bold,
        751..=850 => Weight::ExtraBold,
        _ => Weight::Black,
    }
}

/// Snaps a font weight to the nearest standard 100 in `[100, 900]`.
fn snap_weight(w: u16) -> u16 {
    (((u32::from(w) + 50) / 100) * 100).clamp(100, 900) as u16
}

/// Decodes fonts the theme CSS embeds as base64 data URIs into sfnt
/// (TrueType/OpenType) bytes, ready for the font database. The kind is detected
/// by magic bytes, not the data URI's MIME type (themes label WOFF2 as
/// `application/octet-stream`): WOFF2 is decompressed, bare sfnt is used as-is,
/// and WOFF1 or non-font data is skipped.
pub fn embedded_fonts(css: &str) -> Vec<Vec<u8>> {
    use base64::Engine;

    const NEEDLE: &str = ";base64,";
    let mut fonts = Vec::new();
    let mut rest = css;
    while let Some(pos) = rest.find(NEEDLE) {
        let after = &rest[pos + NEEDLE.len()..];
        // The base64 payload runs to the closing quote/paren of the `url(...)`.
        let end = after.find([')', '"', '\'']).unwrap_or(after.len());
        // Strip any whitespace a pretty-printed CSS may have wrapped it with.
        let payload: String = after[..end].split_whitespace().collect();
        rest = &after[end..];

        let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(&payload) else {
            continue;
        };
        if bytes.starts_with(b"wOF2") {
            if let Ok(sfnt) = wuff::decompress_woff2(&bytes) {
                fonts.push(sfnt);
            }
        } else if is_sfnt(&bytes) {
            fonts.push(bytes);
        }
    }
    fonts
}

/// Whether `bytes` begins with a bare TrueType/OpenType (sfnt) signature.
fn is_sfnt(bytes: &[u8]) -> bool {
    bytes.starts_with(b"OTTO") // OpenType with CFF outlines
        || bytes.starts_with(b"true") // TrueType (Apple)
        || bytes.starts_with(b"ttcf") // TrueType collection
        || bytes.starts_with(&[0x00, 0x01, 0x00, 0x00]) // TrueType
}

/// Copies of the embedded variable fonts stamped at each of `weights`, so a face
/// exists at *exactly* that weight in each family (a font is only copied for a
/// weight its `wght` axis actually reaches). The default weight 400 is skipped —
/// the original face already covers it.
///
/// cosmic-text only draws a requested family for a given weight when the family
/// has a face registered at exactly that weight (see its `default_font_match_key`
/// / `font_weight_diff == 0`). A variable font is registered once, at its default
/// weight (400), so a bold (or 450, 750, …) request finds no match in the family
/// and silently falls back to a *system* font — often a monospace one, so themed
/// text renders as "bold monospace". Stamping a copy's OS/2 weight class to the
/// requested weight gives the family that face; cosmic-text then renders it
/// through the font's `wght` variation axis, so it is genuine rather than faux.
pub fn synth_weight_faces(fonts: &[Vec<u8>], weights: &[u16]) -> Vec<Vec<u8>> {
    let mut wanted: Vec<u16> = weights.iter().copied().filter(|&w| w != 400).collect();
    wanted.sort_unstable();
    wanted.dedup();
    fonts
        .iter()
        .flat_map(|font| {
            wanted
                .iter()
                .filter(|&&w| wght_axis_reaches(font, w))
                .filter_map(|&w| stamp_weight_class(font, w))
        })
        .collect()
}

/// `(offset, length)` of the sfnt table tagged `tag`, if present. Assumes a
/// single-font sfnt (not a `ttcf` collection).
fn sfnt_table(font: &[u8], tag: &[u8; 4]) -> Option<(usize, usize)> {
    let num_tables = u16::from_be_bytes([*font.get(4)?, *font.get(5)?]) as usize;
    (0..num_tables).find_map(|i| {
        let rec = 12 + i * 16;
        let record = font.get(rec..rec + 16)?;
        (&record[0..4] == tag).then(|| {
            let off = u32::from_be_bytes(record[8..12].try_into().unwrap()) as usize;
            let len = u32::from_be_bytes(record[12..16].try_into().unwrap()) as usize;
            (off, len)
        })
    })
}

/// Whether the font has an `fvar` `wght` variation axis whose range covers
/// `weight`, i.e. it can be instanced to that weight.
fn wght_axis_reaches(font: &[u8], weight: u16) -> bool {
    let target = f32::from(weight);
    let read_fixed = |b: &[u8]| i32::from_be_bytes(b.try_into().unwrap()) as f32 / 65536.0;
    let Some((off, len)) = sfnt_table(font, b"fvar") else {
        return false;
    };
    let Some(fvar) = font.get(off..off + len) else {
        return false;
    };
    // fvar header: majorVersion, minorVersion, axesArrayOffset, reserved,
    // axisCount, axisSize (all u16), then axisCount VariationAxisRecords.
    let Some(axes_off) = fvar.get(4..6).map(|b| u16::from_be_bytes([b[0], b[1]]) as usize) else {
        return false;
    };
    let axis_count = fvar.get(8..10).map_or(0, |b| u16::from_be_bytes([b[0], b[1]]) as usize);
    let axis_size = fvar.get(10..12).map_or(0, |b| u16::from_be_bytes([b[0], b[1]]) as usize);
    (0..axis_count).any(|i| {
        // VariationAxisRecord: axisTag(4), min(Fixed), default(Fixed), max(Fixed), ...
        let Some(rec) = fvar.get(axes_off + i * axis_size..axes_off + i * axis_size + 16) else {
            return false;
        };
        &rec[0..4] == b"wght"
            && read_fixed(&rec[4..8]) <= target
            && read_fixed(&rec[12..16]) >= target
    })
}

/// A copy of `font` with its OS/2 `usWeightClass` set to `weight`, leaving every
/// other table (and the variation axes) intact. `None` if it has no OS/2 table.
/// ttf-parser (via fontdb) ignores sfnt checksums, so they need not be fixed.
fn stamp_weight_class(font: &[u8], weight: u16) -> Option<Vec<u8>> {
    // OS/2: version(u16), xAvgCharWidth(i16), usWeightClass(u16) at offset 4.
    let (off, _) = sfnt_table(font, b"OS/2")?;
    let pos = off + 4;
    let mut out = font.to_vec();
    out.get_mut(pos..pos + 2)?.copy_from_slice(&weight.to_be_bytes());
    Some(out)
}

/// Remaps the theme's requested font names to the *internal* family names of
/// its embedded fonts. A theme references its `@font-face` alias (e.g. "Inter")
/// while the embedded file's name table says "Inter Variable", so the requested
/// name wouldn't resolve in the font database; substitute the real name.
pub fn resolve_embedded_names(theme: &mut Theme, fonts: &[Vec<u8>]) {
    let internal: Vec<String> = fonts
        .iter()
        .filter_map(|data| {
            let mut db = cosmic_text::fontdb::Database::new();
            db.load_font_data(data.clone());
            db.faces()
                .next()
                .and_then(|face| face.families.first().map(|(name, _)| name.clone()))
        })
        .collect();
    theme.font_text = remap_font(theme.font_text, &internal);
    theme.font_mono = remap_font(theme.font_mono, &internal);
}

/// Keeps `name` if a loaded face matches it, else the first internal name that
/// starts with it (e.g. "Inter" → "Inter Variable"), else the original.
fn remap_font(name: Option<&'static str>, internal: &[String]) -> Option<&'static str> {
    let name = name?;
    if internal.iter().any(|i| i == name) {
        return Some(name);
    }
    match internal.iter().find(|i| i.starts_with(name)) {
        Some(found) => Some(intern(found)),
        None => Some(name),
    }
}

/// Interns a set of font features to `'static`, so [`Theme`] can stay `Copy`.
///
/// Bounded by the distinct sets in play rather than by how often a theme is
/// applied — which matters because the theme picker re-resolves on every arrow
/// press.
fn intern_features(features: Vec<Feature>) -> &'static [Feature] {
    static SETS: Mutex<Option<HashSet<&'static [Feature]>>> = Mutex::new(None);
    if features.is_empty() {
        return &[];
    }
    let mut guard = SETS.lock().unwrap();
    let sets = guard.get_or_insert_with(HashSet::new);
    match sets.get(features.as_slice()) {
        Some(found) => found,
        None => {
            let leaked: &'static [Feature] = Box::leak(features.into_boxed_slice());
            let _ = sets.insert(leaked);
            leaked
        }
    }
}

/// Interns a font family name to `'static`.
///
/// [`Theme`] is `Copy`, so the names it holds can't be owned `String`s, and a
/// family name has to outlive every card it's drawn on. Leaking each one was
/// fine while a theme loaded once at startup — the font picker previews a
/// family per arrow press, and interning is what keeps that bounded by the
/// number of distinct families rather than by how long you scroll.
pub fn intern(name: &str) -> &'static str {
    static NAMES: Mutex<Option<HashSet<&'static str>>> = Mutex::new(None);
    let mut guard = NAMES.lock().unwrap();
    let names = guard.get_or_insert_with(HashSet::new);
    match names.get(name) {
        Some(found) => found,
        None => {
            let leaked: &'static str = Box::leak(name.to_owned().into_boxed_str());
            names.insert(leaked);
            leaked
        }
    }
}

fn opaque(c: Color) -> Color {
    Color { a: 1.0, ..c }
}

/// `--name: value` declarations from the theme's top-level `.theme-dark`
/// block. Works on minified CSS.
fn dark_variables(css: &str) -> HashMap<String, String> {
    let mut vars = HashMap::new();
    let Some(block) = dark_block(css) else {
        return vars;
    };
    for decl in block.split(';') {
        if let Some((name, value)) = decl.split_once(':') {
            let name = name.trim();
            if name.starts_with("--") {
                vars.insert(name.to_owned(), value.trim().to_owned());
            }
        }
    }
    vars
}

/// The contents of the first `.theme-dark { ... }` block (matched braces),
/// skipping descendant selectors like `.theme-dark .notice { ... }`.
fn dark_block(css: &str) -> Option<&str> {
    let mut search = css;
    loop {
        let idx = search.find(".theme-dark")?;
        let after = &search[idx + ".theme-dark".len()..];
        if let Some(rest) = after.trim_start().strip_prefix('{') {
            let mut depth = 1;
            for (i, ch) in rest.char_indices() {
                match ch {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(&rest[..i]);
                        }
                    }
                    _ => {}
                }
            }
            return None;
        }
        search = after;
    }
}

/// Resolves `var(--x)` / `var(--x, fallback)` references against `vars`,
/// returning the final concrete value. Depth-capped against cycles. Non-`var`
/// values pass through unchanged (including `color-mix()`, which we can't
/// evaluate and later parse as `None`).
fn resolve(value: &str, vars: &HashMap<String, String>, depth: u8) -> Option<String> {
    let value = value.trim();
    if depth == 0 {
        return None;
    }
    let Some(inner) = value.strip_prefix("var(").and_then(|r| r.strip_suffix(')')) else {
        return Some(value.to_owned());
    };
    let (name, fallback) = match inner.split_once(',') {
        Some((name, fallback)) => (name.trim(), Some(fallback.trim())),
        None => (inner.trim(), None),
    };
    if let Some(referenced) = vars.get(name) {
        return resolve(referenced, vars, depth - 1);
    }
    fallback.and_then(|f| resolve(f, vars, depth - 1))
}

/// Parses a plain CSS color: `#rgb`/`#rrggbb`/`#rrggbbaa`, `rgb()`/`rgba()`, or
/// `hsl()`/`hsla()`. Returns `None` for `color-mix()`, `var()` chains that
/// didn't resolve, keywords, or `hsl(calc(...))` expressions.
fn parse_color(value: &str) -> Option<Color> {
    let value = value.trim().trim_end_matches("!important").trim();
    if let Some(hex) = value.strip_prefix('#') {
        return parse_hex(hex);
    }
    if let Some(inner) = value
        .strip_prefix("hsla(")
        .or_else(|| value.strip_prefix("hsl("))
        .and_then(|s| s.strip_suffix(')'))
    {
        let mut parts = inner.split(',').map(str::trim);
        let h: f32 = parts.next()?.trim_end_matches("deg").trim().parse().ok()?;
        let s: f32 = parts.next()?.trim_end_matches('%').parse::<f32>().ok()? / 100.0;
        let l: f32 = parts.next()?.trim_end_matches('%').parse::<f32>().ok()? / 100.0;
        let a: f32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(1.0);
        return Some(hsl_to_rgb(h, s, l, a));
    }
    let inner = value
        .strip_prefix("rgba(")
        .or_else(|| value.strip_prefix("rgb("))?
        .strip_suffix(')')?;
    let mut parts = inner.split(',').map(str::trim);
    let mut channel = || parts.next()?.parse::<f32>().ok().map(|v| v / 255.0);
    let (r, g, b) = (channel()?, channel()?, channel()?);
    let a = parts.next().and_then(|s| s.parse::<f32>().ok()).unwrap_or(1.0);
    Some(Color { r, g, b, a })
}

fn hsl_to_rgb(h: f32, s: f32, l: f32, a: f32) -> Color {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = h.rem_euclid(360.0) / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r, g, b) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    Color {
        r: r + m,
        g: g + m,
        b: b + m,
        a,
    }
}

fn parse_hex(hex: &str) -> Option<Color> {
    let byte = |s: &str| u8::from_str_radix(s, 16).ok().map(|v| v as f32 / 255.0);
    match hex.len() {
        3 => {
            let nibble =
                |c: &str| u8::from_str_radix(c, 16).ok().map(|v| (v * 17) as f32 / 255.0);
            Some(Color {
                r: nibble(&hex[0..1])?,
                g: nibble(&hex[1..2])?,
                b: nibble(&hex[2..3])?,
                a: 1.0,
            })
        }
        6 => Some(Color {
            r: byte(&hex[0..2])?,
            g: byte(&hex[2..4])?,
            b: byte(&hex[4..6])?,
            a: 1.0,
        }),
        8 => Some(Color {
            r: byte(&hex[0..2])?,
            g: byte(&hex[2..4])?,
            b: byte(&hex[4..6])?,
            a: byte(&hex[6..8])?,
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r"
        .theme-dark .notice { color: #123456; }
        .theme-dark {
          --background-secondary: #191621;
          --text-normal: #bebebe;
          --interactive-accent: rgba(14, 210, 247, 0.5);
          --background-modifier-border: rgba(14, 210, 247, 0.05);
          --from-var: var(--color-x);
        }
    ";

    #[test]
    fn reads_plain_colors_from_the_dark_block() {
        let theme = load_from(SAMPLE);
        assert_eq!(theme.card_fill, Color::from_rgb8(0x19, 0x16, 0x21));
        assert_eq!(theme.text, Color::from_rgb8(0xbe, 0xbe, 0xbe));
        // Accent is forced opaque.
        assert_eq!(theme.accent, opaque(parse_color("rgba(14,210,247,0.5)").unwrap()));
    }

    #[test]
    fn var_and_missing_values_keep_defaults() {
        let vars = dark_variables(SAMPLE);
        assert_eq!(parse_color(vars.get("--from-var").unwrap()), None);
        // A theme with only var()/color-mix() leaves every default in place.
        let all_vars = load_from(".theme-dark{--background-secondary:var(--x);--text-normal:color-mix(in hsl,#000,#fff)}");
        assert_eq!(all_vars.card_fill, Theme::default().card_fill);
        assert_eq!(all_vars.text, Theme::default().text);
    }

    #[test]
    fn descendant_selector_is_not_mistaken_for_the_var_block() {
        // `.theme-dark .notice` comes first but isn't the var block.
        let theme = load_from(SAMPLE);
        assert_ne!(theme.card_fill, Color::from_rgb8(0x12, 0x34, 0x56));
    }

    /// Parses a CSS string as if it were a theme file.
    #[test]
    fn reads_markdown_element_colors() {
        let css = ".theme-dark{--bold-color:#ff0000;--h1-color:#00ff00;--text-title-h2:#0000ff}";
        let theme = load_from(css);
        assert_eq!(theme.bold, Color::from_rgb8(0xff, 0x00, 0x00));
        assert_eq!(theme.headings[0], Color::from_rgb8(0x00, 0xff, 0x00)); // --h1-color
        assert_eq!(theme.headings[1], Color::from_rgb8(0x00, 0x00, 0xff)); // legacy --text-title-h2
        // Unset element colors fall back (italic -> text, headings -> accent).
        assert_eq!(theme.italic, theme.text);
        assert_eq!(theme.headings[2], theme.accent);
    }

    #[test]
    fn resolves_var_chains_into_hsl() {
        // Mirrors Primary: a semantic var points at a base color that's hsla.
        let css = ".theme-dark{--bold-color:var(--color-d-red-30);\
                   --color-d-red-30:hsla(4, 88%, 61%, 1)}";
        let theme = load_from(css);
        assert_ne!(theme.bold, Theme::default().bold);
        // hsla(4,…) is red-ish: red channel dominates.
        assert!(theme.bold.r > theme.bold.g && theme.bold.r > theme.bold.b);
    }

    #[test]
    fn color_mix_and_dangling_vars_keep_defaults() {
        let css = ".theme-dark{--text-normal:color-mix(in hsl, #000, #fff);\
                   --interactive-accent:var(--not-defined)}";
        let theme = load_from(css);
        assert_eq!(theme.text, Theme::default().text);
        assert_eq!(theme.accent, Theme::default().accent);
    }

    #[test]
    fn reads_font_families_skipping_generics() {
        // `--font-text` resolves via var() to a stack; the first concrete family
        // wins and generics are skipped. `--font-monospace-theme` is a fallback
        // name when the primary key is unset.
        let css = ".theme-dark{--font-text:var(--font-text-theme);\
                   --font-text-theme:\"Inter\", sans-serif;\
                   --font-monospace-theme:'Cascadia Code', monospace}";
        let theme = Theme::from_css(css);
        assert_eq!(theme.font_text, Some("Inter"));
        assert_eq!(theme.font_mono, Some("Cascadia Code"));
    }

    #[test]
    fn reads_font_declared_outside_the_dark_block() {
        // Themes (e.g. Primary) declare fonts on `body`, not `.theme-dark`.
        let css = "body{--font-text-theme:\"Inter\"}.theme-dark{--text-normal:#fff}";
        assert_eq!(Theme::from_css(css).font_text, Some("Inter"));
    }

    #[test]
    fn purely_generic_font_stacks_keep_the_builtin() {
        let css = "body{--font-text:-apple-system, system-ui, sans-serif}";
        assert_eq!(Theme::from_css(css).font_text, None);
    }

    #[test]
    fn remaps_a_requested_name_to_an_embedded_family() {
        let internal = vec!["Inter Variable".to_owned(), "Cascadia Code".to_owned()];
        assert_eq!(remap_font(Some("Inter"), &internal), Some("Inter Variable"));
        assert_eq!(remap_font(Some("Cascadia Code"), &internal), Some("Cascadia Code"));
        assert_eq!(remap_font(Some("Menlo"), &internal), Some("Menlo"));
    }

    #[test]
    fn embedded_fonts_ignores_css_without_fonts() {
        assert!(embedded_fonts(".theme-dark{--text-normal:#fff}").is_empty());
    }

    #[test]
    fn embedded_fonts_skips_undecodable_payloads() {
        // The needle is present but the payload isn't valid base64 WOFF2, so it
        // is skipped rather than panicking.
        let css = "@font-face{font-family:X;src:url(data:font/woff2;base64,not*base64)}";
        assert!(embedded_fonts(css).is_empty());
    }

    fn load_from(css: &str) -> Theme {
        let mut theme = Theme::default();
        theme.apply(&dark_variables(css));
        theme
    }

    /// A minimal single-font sfnt with an OS/2 table (`usWeightClass` = `weight`)
    /// and, if `variable`, an `fvar` table with a `wght` axis spanning 100–900.
    fn fake_sfnt(weight: u16, variable: bool) -> Vec<u8> {
        let fixed = |v: f32| ((v * 65536.0) as i32).to_be_bytes();
        let mut os2 = vec![0, 0]; // version
        os2.extend_from_slice(&[0, 0]); // xAvgCharWidth
        os2.extend_from_slice(&weight.to_be_bytes()); // usWeightClass

        let mut fvar = Vec::new();
        if variable {
            fvar.extend_from_slice(&1u16.to_be_bytes()); // majorVersion
            fvar.extend_from_slice(&0u16.to_be_bytes()); // minorVersion
            fvar.extend_from_slice(&12u16.to_be_bytes()); // axesArrayOffset
            fvar.extend_from_slice(&0u16.to_be_bytes()); // reserved
            fvar.extend_from_slice(&1u16.to_be_bytes()); // axisCount
            fvar.extend_from_slice(&20u16.to_be_bytes()); // axisSize
            fvar.extend_from_slice(b"wght");
            fvar.extend_from_slice(&fixed(100.0)); // min
            fvar.extend_from_slice(&fixed(400.0)); // default
            fvar.extend_from_slice(&fixed(900.0)); // max
            fvar.extend_from_slice(&0u16.to_be_bytes()); // flags
            fvar.extend_from_slice(&0u16.to_be_bytes()); // axisNameID
        }

        let mut payloads: Vec<(&[u8; 4], &[u8])> = vec![(b"OS/2", &os2)];
        if variable {
            payloads.push((b"fvar", &fvar));
        }
        let num = payloads.len();

        let mut font = Vec::new();
        font.extend_from_slice(&[0x00, 0x01, 0x00, 0x00]); // sfnt version
        font.extend_from_slice(&(num as u16).to_be_bytes()); // numTables
        font.extend_from_slice(&[0, 0, 0, 0, 0, 0]); // search fields (unused here)

        let mut offset = 12 + num * 16; // table data follows the directory
        for (tag, data) in &payloads {
            font.extend_from_slice(*tag);
            font.extend_from_slice(&[0, 0, 0, 0]); // checksum (ignored)
            font.extend_from_slice(&(offset as u32).to_be_bytes());
            font.extend_from_slice(&(data.len() as u32).to_be_bytes());
            offset += data.len();
        }
        for (_, data) in &payloads {
            font.extend_from_slice(data);
        }
        font
    }

    #[test]
    fn finds_the_wght_axis_only_on_variable_fonts() {
        assert!(wght_axis_reaches(&fake_sfnt(400, true), 700));
        assert!(!wght_axis_reaches(&fake_sfnt(400, false), 700));
    }

    #[test]
    fn stamps_the_os2_weight_class() {
        let font = fake_sfnt(400, true);
        let (off, _) = sfnt_table(&font, b"OS/2").unwrap();
        let bold = stamp_weight_class(&font, 700).unwrap();
        // Same length, weight class rewritten, everything else untouched.
        assert_eq!(bold.len(), font.len());
        assert_eq!(u16::from_be_bytes([bold[off + 4], bold[off + 5]]), 700);
        assert_eq!(&bold[..off + 4], &font[..off + 4]);
    }

    #[test]
    fn synth_weight_faces_only_copies_variable_fonts() {
        let fonts = vec![fake_sfnt(400, true), fake_sfnt(400, false)];
        // Two weights, one variable font that reaches both → two stamped faces.
        let faces = synth_weight_faces(&fonts, &[500, 800]);
        assert_eq!(faces.len(), 2);
        let stamped: Vec<u16> = faces
            .iter()
            .map(|f| {
                let (off, _) = sfnt_table(f, b"OS/2").unwrap();
                u16::from_be_bytes([f[off + 4], f[off + 5]])
            })
            .collect();
        assert_eq!(stamped, vec![500, 800]);
    }

    #[test]
    fn synth_weight_faces_skips_the_default_weight() {
        let fonts = vec![fake_sfnt(400, true)];
        // 400 needs no synthetic face; only 700 is stamped.
        let faces = synth_weight_faces(&fonts, &[400, 700]);
        assert_eq!(faces.len(), 1);
        let (off, _) = sfnt_table(&faces[0], b"OS/2").unwrap();
        assert_eq!(u16::from_be_bytes([faces[0][off + 4], faces[0][off + 5]]), 700);
    }

    #[test]
    fn snap_weight_rounds_to_the_nearest_hundred() {
        assert_eq!(snap_weight(450), 500);
        assert_eq!(snap_weight(750), 800);
        assert_eq!(snap_weight(400), 400);
        assert_eq!(snap_weight(0), 100);
        assert_eq!(snap_weight(2000), 900);
    }

    #[test]
    fn css_number_reads_plain_integer_declarations() {
        let css = ":root { --font-normal: 450; --font-bold:750; --x: var(--y); }";
        assert_eq!(css_number(css, "--font-normal"), Some(450));
        assert_eq!(css_number(css, "--font-bold"), Some(750));
        assert_eq!(css_number(css, "--x"), None);
        assert_eq!(css_number(css, "--missing"), None);
    }

    #[test]
    fn font_features_are_read_from_the_theme_s_own_variable() {
        let css = r#"
            body { font-feature-settings: var(--text-font-feature); }
            .theme-dark { --text-font-feature: "calt" 1,"case" 0,"cv11" 1,"ss03" on; }
            .cm-editor { font-feature-settings: "zero" 1; }
        "#;
        let theme = Theme::from_css(css);
        let got: Vec<(String, u32)> = theme
            .font_features
            .iter()
            .map(|f| (String::from_utf8_lossy(f.tag.as_bytes()).into_owned(), f.value))
            .collect();
        assert_eq!(
            got,
            [("calt", 1), ("case", 0), ("cv11", 1), ("ss03", 1)]
                .map(|(tag, value)| (tag.to_owned(), value))
                .to_vec(),
            "`on` means 1, and an explicit 0 has to survive as a 0 — it turns a \
             feature the font enables by default back off",
        );

        // The scoped rule is not the theme's typography: applying one selector's
        // features to the whole board would turn on what someone asked for in
        // one corner of the UI.
        assert!(
            !theme.font_features.iter().any(|f| f.tag.as_bytes() == b"zero"),
            "a bare font-feature-settings rule must not be hoisted",
        );
    }

    #[test]
    fn a_var_indirection_falls_through_to_the_variable_itself() {
        // Primary writes `--interface-font-feature: var(--text-font-feature)`.
        // Following that here would be a second CSS parser; the declaration it
        // points at is in the same sheet, so the scan simply carries on.
        let css = r#"
            .theme-dark {
              --font-text-feature: var(--text-font-feature);
              --text-font-feature: "cv05" 1;
            }
        "#;
        let theme = Theme::from_css(css);
        assert_eq!(theme.font_features.len(), 1);
        assert_eq!(theme.font_features[0].tag.as_bytes(), b"cv05");
    }

    #[test]
    fn a_theme_without_features_asks_for_none() {
        // Not "the defaults we think are nice" — a font's own defaults, which is
        // what an empty set means to the shaper.
        assert!(Theme::from_css(".theme-dark { color: #fff; }").font_features.is_empty());
        assert!(Theme::default().font_features.is_empty());
    }

    #[test]
    fn malformed_feature_entries_are_dropped_not_guessed() {
        // Tags are four bytes. Anything else is not a feature, and padding or
        // truncating one would silently turn on a different feature.
        let parsed = parse_features(r#""toolong" 1,"ab" 1,"cv11" 1,notquoted 1,"cv05" maybe"#);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].tag.as_bytes(), b"cv11");
    }

    #[test]
    fn the_same_set_interns_to_the_same_slice() {
        // The theme picker re-resolves on every arrow press, so this must not
        // leak per keypress.
        let a = intern_features(parse_features(r#""cv11" 1,"calt" 1"#));
        let b = intern_features(parse_features(r#""cv11" 1,"calt" 1"#));
        assert!(std::ptr::eq(a, b));
    }

}

