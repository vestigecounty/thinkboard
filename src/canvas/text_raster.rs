//! CPU rasterization of node text into bitmaps.
//!
//! Continuous zoom cannot use iced's normal text path: glyphs are cached by
//! pixel size, so every animation frame re-rasterizes all visible text.
//! Instead, when a zoom gesture starts, each text node is snapshotted once
//! into an RGBA bitmap (at the current scale, retina density) and the GPU
//! scales the bitmap during the animation — same trick browsers use for
//! pinch zoom. Crisp real text returns when the zoom settles.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use cosmic_text::{
    Align, Attrs, AttrsList, Buffer, BufferLine, Color as CosmicColor, Family, FontSystem,
    Feature, FontFeatures, LineEnding, Metrics, Shaping, Style, SwashCache, Weight, Wrap,
    fontdb,
};
use iced::Color;
use iced::widget::image;

use super::markdown;
use super::md_edit;
use super::syntax::Syntax;
use crate::theme::Theme;

/// Raster density multiplier over the logical size: 2x for retina backing
/// plus a 2x reserve, so a bitmap stays sharp while zooming in up to twice
/// the scale it was captured at.
const RASTER_DENSITY: f32 = 4.0;

/// Ceiling on the raster scale itself — the cost/quality knob.
///
/// Rasterizing is dominated by the glyph size the shaper works at
/// (`font_size * scale`), not by how many pixels the texture ends up with, so
/// bounding the scale is what bounds the cost. Bounding only the *dimensions*
/// (see [`MAX_DIMENSION`]) made resolution depend on a card's size for no good
/// reason, and backwards at that: a 250 px card was left at 8.19x while an
/// 800 px card, holding far more text, was held to 2.56x.
///
/// At scale `k` a card is pixel-exact on a 2x display up to zoom `k / 2`, and
/// keeps its full re-raster reserve up to `k / RASTER_DENSITY`. So 6.0 is sharp
/// through zoom 3 and reserved through 1.5; past that a card softens until the
/// zoom settles, and stays slightly soft at the very top of the zoom range.
const MAX_SCALE: f32 = 6.0;

/// Cap on bitmap dimensions. Unlike [`MAX_SCALE`] this is a hard constraint,
/// not a quality choice: a texture past this fragments across atlas layers and
/// can take iced several frames to allocate, leaving the node blank meanwhile.
/// It still binds before `MAX_SCALE` on large cards, which is unavoidable —
/// the same scale simply needs more texture on a bigger card.
const MAX_DIMENSION: f32 = 2048.0;

/// The tallest a text card auto-grows before its content scrolls instead.
///
/// A card's bitmap is capped at [`MAX_DIMENSION`] on its longest side, so a
/// card taller than that loses resolution in proportion: at 1024 the raster is
/// exactly retina, by 2048 it is 1:1, and past that the bitmap holds *less*
/// detail than the card is — a long note renders visibly soft. Rather than let
/// a card grow until it blurs, growth stops here and the overflow scrolls,
/// which also forces the scrollbar on regardless of the setting (see
/// `Board::scrollable`) so the rest stays reachable.
pub const MAX_AUTO_HEIGHT: f32 = MAX_DIMENSION / 2.0;

/// Floor on bitmap dimensions (longest side). Zoomed all the way out, a node's
/// on-screen size is tiny, so a zoom-proportional scale would bake a ~100 px
/// texture that's a blurry mess the instant you zoom in (before the re-raster
/// lands). Baking at least this resolution keeps the first zoom-in sharp.
const MIN_DIMENSION: f32 = 512.0;

/// Matches `canvas::Text::default()`, which uses `LineHeight::Relative(1.2)`
/// (unlike iced widgets, which default to 1.3).
pub const LINE_HEIGHT: f32 = 1.2;

// --- Card text geometry -----------------------------------------------------
//
// A text card's outer width breaks down, from the edge inward, as: `CARD_H_PAD`
// of card chrome, then `MD_PADDING` of text inset per side, then the text wrap
// column. When a card auto-sizes to its text it adds `NODE_PAD_X` / `NODE_PAD_Y`
// around the measured extent. These live here — the one module both the app
// (which sizes cards) and the board (which draws them) depend on — so the sizing
// and drawing geometry can't drift apart; a mismatch reflows or clips the text.

/// Horizontal padding (logical px per side) inset into the wrap column when
/// rendering Markdown, so left-aligned lines have a left margin. The editor
/// insets the same amount, so entering edit mode doesn't reflow.
pub(crate) const MD_PADDING: f32 = 8.0;

/// Card chrome (world px) between a card's outer width and its text wrap column.
pub(crate) const CARD_H_PAD: f32 = 12.0;

/// Horizontal and vertical padding (world px) added around a node's measured
/// text when it auto-sizes to fit.
pub(crate) const NODE_PAD_X: f32 = 24.0;
pub(crate) const NODE_PAD_Y: f32 = 20.0;

/// Smallest wrap column ever handed to the shaper, so a sliver-width card still
/// lays out a (clipped) line rather than collapsing.
const MIN_TEXT_WRAP: f32 = 20.0;

/// The wrap width to measure or draw a card's Markdown at, for a card of outer
/// `width`. This is the `wrap_width` [`measure_markdown`], [`snapshot_markdown`],
/// and [`with_markdown_layout`] expect — they inset `MD_PADDING` a side of it
/// themselves.
pub fn card_text_wrap(width: f32) -> f32 {
    (width - CARD_H_PAD).max(MIN_TEXT_WRAP)
}

/// The inner column the editor lays a card's text out in, for a card of outer
/// `width`: [`card_text_wrap`] minus the `MD_PADDING` inset each side. The editor
/// measures with [`measure_source`], which — unlike [`measure_markdown`] — does
/// not apply that inset, so it passes this column explicitly. Keeping the two in
/// lockstep is what stops a card reflowing as you enter or leave edit mode.
pub(crate) fn editor_text_wrap(width: f32) -> f32 {
    (card_text_wrap(width) - 2.0 * MD_PADDING).max(MIN_TEXT_WRAP)
}

struct Raster {
    font_system: FontSystem,
    cache: SwashCache,
}

static RASTER: Mutex<Option<Raster>> = Mutex::new(None);

/// Fonts a theme embedded (decoded to sfnt bytes), loaded into the DB on first
/// use so their named families match. Set before `warm_up`.
static EMBEDDED_FONTS: Mutex<Option<Vec<Vec<u8>>>> = Mutex::new(None);

/// Registers decoded theme fonts to load into the font database.
pub fn set_embedded_fonts(fonts: Vec<Vec<u8>>) {
    *EMBEDDED_FONTS.lock().unwrap() = Some(fonts);
    *FAMILIES.lock().unwrap() = None; // the database will gain their families
}

/// Adds theme fonts to the font database *after* it has been built — a theme
/// swapped while the app is running, rather than one read at startup.
///
/// [`set_embedded_fonts`] only works before the first [`with_raster`], since
/// that's when the database is assembled. This takes the other path and loads
/// straight into the live database, then drops the layout and measurement
/// caches: both are keyed by text and size but shaped with whatever faces were
/// resolvable at the time, so entries from before the swap would keep the old
/// typeface on screen.
pub fn add_embedded_fonts(fonts: Vec<Vec<u8>>) {
    if fonts.is_empty() {
        clear_caches(); // the family *names* may still have changed
        return;
    }
    with_raster(|raster| {
        let db = raster.font_system.db_mut();
        for data in fonts {
            db.load_font_data(data);
        }
    });
    *FAMILIES.lock().unwrap() = None; // the database just gained their families
    clear_caches();
}

/// Every font family the rasterizer can actually draw with, sorted for display.
///
/// Read from the same database the board renders through, so a family offered
/// here is one that will resolve — rather than a list from elsewhere that might
/// name faces this font system can't see. Includes whatever a theme embedded,
/// which is why the picker shows a theme's bundled typeface alongside the
/// installed ones.
///
/// Families whose name starts with `.` are left out: those are the platform's
/// private faces (`.SF NS`, `.LastResort`), which are not meant to be asked for
/// by name and mostly render as fallback if you do.
pub fn families() -> Vec<FontFamily> {
    // Cached because probing every family costs ~8 ms, and the picker is opened
    // on a keystroke. Invalidated only where the font *database* changes — not
    // by `clear_caches`, which the font preview calls on every arrow press.
    if let Some(cached) = FAMILIES.lock().unwrap().as_ref() {
        return cached.clone();
    }
    let families = with_raster(|raster| {
        // One pass over the faces for the names and, per name, the faces
        // carrying it — which give both the weight to ask for and the ids
        // [`renders_own_name`] checks the shaped glyphs against.
        let mut faces: HashMap<String, Vec<Face>> = HashMap::new();
        let db = raster.font_system.db();
        for face in db.faces() {
            if !lays_out_text(db, face.id) {
                continue;
            }
            for (name, _) in &face.families {
                if name.starts_with('.') {
                    continue;
                }
                faces.entry(name.clone()).or_default().push(Face {
                    id: face.id,
                    weight: face.weight.0,
                    upright: face.style == fontdb::Style::Normal,
                });
            }
        }
        let mut names: Vec<String> = faces.keys().cloned().collect();
        names.sort_by_key(|name| name.to_lowercase());
        names.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
        names
            .into_iter()
            .map(|name| {
                let own = faces.get(&name).map(Vec::as_slice).unwrap_or_default();
                let weight = nearest_weight(own, REGULAR);
                let ids: Vec<fontdb::ID> = own.iter().map(|face| face.id).collect();
                FontFamily {
                    legible: renders_own_name(&mut raster.font_system, &name, &ids, weight),
                    weight,
                    name,
                }
            })
            .collect::<Vec<_>>()
    });
    *FAMILIES.lock().unwrap() = Some(families.clone());
    families
}

/// The last [`families`] scan. Costly to produce and stable until a font is
/// loaded, which only a theme swap does.
static FAMILIES: Mutex<Option<Vec<FontFamily>>> = Mutex::new(None);

/// A font family the picker can offer.
#[derive(Clone)]
pub struct FontFamily {
    pub name: String,
    /// The upright weight to ask this family for — see [`family_weights`].
    pub weight: u16,
    /// Whether the family's own name is readable set in it.
    ///
    /// macOS ships dozens of faces with no Latin glyphs at all — Arabic, Thai,
    /// Devanagari, and the whole Noto script set — and their names come out as
    /// tofu boxes. A picker that draws every row in its own face therefore has
    /// half its rows unreadable, naming fonts you then can't identify.
    pub legible: bool,
}

/// What a regular and a bold nominally weigh.
const REGULAR: u16 = 400;
const BOLD: u16 = 700;

/// The weights `family` actually ships, nearest to a regular and a bold —
/// `None` if the font database has no upright face under that name.
///
/// **Asking for a weight a family hasn't got doesn't get you its nearest one,
/// it gets you a different font.** The shaper matches weight exactly and falls
/// back to the system face when nothing matches, and a surprising number of
/// families have no 400 at all: Futura's regular is 500, Arial Black's is 900,
/// Skia reports 5. Left alone, choosing one of those would silently render the
/// board in the system font instead — the same failure the synthesized faces in
/// `synth_weight_faces` exist to prevent for a theme's embedded fonts.
pub fn family_weights(family: &str) -> Option<(u16, u16)> {
    with_raster(|raster| {
        let faces: Vec<Face> = raster
            .font_system
            .db()
            .faces()
            .filter(|face| face.families.iter().any(|(name, _)| name == family))
            .map(|face| Face {
                id: face.id,
                weight: face.weight.0,
                upright: face.style == fontdb::Style::Normal,
            })
            .collect();
        (!faces.is_empty())
            .then(|| (nearest_weight(&faces, REGULAR), nearest_weight(&faces, BOLD)))
    })
}

/// One face of a family: enough to pick a weight and to recognise the face
/// again in a shaped glyph.
struct Face {
    id: fontdb::ID,
    weight: u16,
    upright: bool,
}

/// The weight among `faces` closest to `target`, preferring the heavier of two
/// equally close ones (a bold that lands between two faces should read as bold).
///
/// Upright faces only — except in a family that hasn't got one, which is not the
/// oddity it sounds: Zapfino, Apple Chancery, Brush Script and Snell Roundhand
/// are all script faces macOS marks italic throughout. Weighing those against a
/// family's roman would pick the wrong face; excluding them outright would drop
/// four perfectly pickable fonts off the list.
fn nearest_weight(faces: &[Face], target: u16) -> u16 {
    let upright = faces.iter().any(|face| face.upright);
    faces
        .iter()
        .filter(|face| face.upright || !upright)
        .map(|face| face.weight)
        .min_by_key(|&weight| (weight.abs_diff(target), u16::MAX - weight))
        .unwrap_or(target)
}

/// Whether `name` shapes to real glyphs *from its own faces*.
///
/// Two ways it doesn't. A character the face has no glyph for shapes to
/// `.notdef` — glyph 0, the tofu box. Before that, though, the shaper
/// substitutes another font that does have the character, which comes out
/// legible but in the wrong typeface — a row that quietly shows you a different
/// font than the one it names. Both are caught by the same check, since a
/// substituted glyph carries the *other* face's id.
fn renders_own_name(
    font_system: &mut FontSystem,
    name: &str,
    own: &[fontdb::ID],
    weight: u16,
) -> bool {
    const PROBE_PX: f32 = 16.0;
    let mut buffer = Buffer::new(font_system, Metrics::new(PROBE_PX, PROBE_PX));
    buffer.set_text(
        font_system,
        name,
        // At the family's own weight, or this rejects every family without a
        // 400 face — which is what it's for, not what it's testing.
        &Attrs::new().family(Family::Name(name)).weight(Weight(weight)),
        // `Basic` rather than `Advanced`: this asks which glyphs exist, not how
        // they'd be composed, and it runs once per family in the database.
        Shaping::Basic,
        None,
    );
    buffer.shape_until_scroll(font_system, false);
    buffer.layout_runs().all(|run| {
        run.glyphs
            .iter()
            .all(|glyph| glyph.glyph_id != 0 && own.contains(&glyph.font_id))
    })
}

/// Whether a face declares horizontal metrics, i.e. can say how wide its
/// glyphs are.
///
/// macOS ships one bitmap-only font — `GB18030 Bitmap` — whose `hhea` puts the
/// count at zero, and swash reads an advance from it as `(count - 1) * 4`: an
/// overflow panic in a debug build, a nonsense offset in a release one. The
/// panic happens inside the raster mutex, which poisons it and takes *all* text
/// down with it.
///
/// A font that can't say how wide its glyphs are can't lay out text at all, so
/// it's dropped from the list rather than handled downstream — and dropped
/// before anything shapes with it, since the probe in [`renders_own_name`]
/// would be the first thing to fall over.
fn lays_out_text(db: &fontdb::Database, id: fontdb::ID) -> bool {
    db.with_face_data(id, |data, face_index| {
        // In a collection, each font has its own table directory, and the
        // offsets in it are from the start of the *file* either way.
        let header = if data.get(..4) == Some(&b"ttcf"[..]) {
            let at = 12 + face_index as usize * 4;
            match data.get(at..at + 4).and_then(|b| b.try_into().ok()) {
                Some(b) => u32::from_be_bytes(b) as usize,
                None => return false,
            }
        } else {
            0
        };
        let Some(tables) = data
            .get(header + 4..header + 6)
            .map(|b| u16::from_be_bytes([b[0], b[1]]) as usize)
        else {
            return false;
        };
        (0..tables).any(|i| {
            let at = header + 12 + i * 16;
            let Some(record) = data.get(at..at + 16) else {
                return false;
            };
            if &record[0..4] != b"hhea" {
                return false;
            }
            let Ok(offset) = record[8..12].try_into().map(u32::from_be_bytes) else {
                return false;
            };
            // `numberOfHMetrics` is the last of hhea's 36 bytes.
            let at = offset as usize + 34;
            data.get(at..at + 2).is_some_and(|b| u16::from_be_bytes([b[0], b[1]]) > 0)
        })
    })
    .unwrap_or(false)
}

fn with_raster<R>(f: impl FnOnce(&mut Raster) -> R) -> R {
    // Recovered rather than propagated: what's behind this lock is caches and a
    // font database, so a thread that panicked mid-shape leaves nothing an
    // onlooker can act on wrongly — whereas taking the poison would turn one bad
    // font into an app with no text at all, permanently.
    let mut guard = RASTER.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let raster = guard.get_or_insert_with(|| {
        let mut db = cosmic_text::fontdb::Database::new();
        // The system UI faces: an upright face (regular/bold via the weight
        // axis), a real italic face (without it, italic mis-matches whatever
        // italic font is loaded), and a monospace face for code. Missing files
        // are ignored.
        for path in crate::platform::UI_FONT_FILES {
            let _ = db.load_font_file(path);
        }
        // Always pull in installed system fonts: they're the raster db's only
        // source of glyph fallback, so scripts the UI font lacks (CJK, emoji,
        // symbols) resolve to an installed face instead of rendering as tofu.
        // A theme's named font (e.g. "Inter") also matches from here.
        db.load_system_fonts();
        // Fonts the theme embedded (decoded elsewhere); their name tables
        // register the families the theme's `--font-*` variables reference.
        if let Some(fonts) = EMBEDDED_FONTS.lock().unwrap().take() {
            for data in fonts {
                db.load_font_data(data);
            }
        }
        Raster {
            font_system: FontSystem::new_with_locale_and_db("en-US".to_owned(), db),
            cache: SwashCache::new(),
        }
    });
    f(raster)
}

/// Loads the font database up-front so the first zoom gesture doesn't pay
/// for it.
pub fn warm_up() {
    with_raster(|_| ());
}

/// A text node rendered to a bitmap.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub handle: image::Handle,
    /// The zoom the bitmap was requested for; when the settled camera zoom
    /// drifts too far from this, the node should re-raster.
    pub zoom: f32,
    /// The raster scale actually used (after capping). If a re-raster would
    /// land on the same scale, the snapshot is still fresh.
    pub scale: f32,
    /// GPU texture footprint in bytes (pixel_width × pixel_height × 4), for the
    /// FPS meter's VMEM readout.
    pub bytes: usize,
}

/// The raster scale (texture px per logical px) used for a node at the
/// given zoom, after the dimension cap.
pub fn scale_for(zoom: f32, width: f32, height: f32) -> f32 {
    let longest = width.max(height);
    (zoom * RASTER_DENSITY)
        // What the resolution is actually worth (see MAX_SCALE) …
        .min(MAX_SCALE)
        // … then the hard texture bound, which still binds first on big cards.
        .min(MAX_DIMENSION / longest)
        // The floor wins over both: a tiny on-screen node still bakes enough
        // resolution to survive the first zoom-in.
        .max(MIN_DIMENSION / longest)
}

/// Measures `text` (screen px) as the node editor lays it out: wrapped at
/// `wrap_width * scale`, font `font_size * scale`, matching line height.
/// `family` is the editor's font (theme body font, else SF) so the measured
/// width matches what's drawn. Returns (max line width, total height).
pub fn measure(
    text: &str,
    wrap_width: f32,
    font_size: f32,
    scale: f32,
    family: Option<&str>,
    features: &[Feature],
) -> (f32, f32) {
    let font_px = font_size * scale;
    let line_px = font_px * LINE_HEIGHT;

    // Trailing newlines produce empty buffer lines that may not emit
    // layout runs; floor the height by the logical line count so a card
    // grows the moment Enter is pressed.
    let logical_lines = text.lines().count() + usize::from(text.ends_with('\n'));

    if text.trim().is_empty() {
        return (0.0, logical_lines.max(1) as f32 * line_px);
    }

    let family = family.map(Family::Name).unwrap_or(Family::Name(crate::platform::UI_FONT_NAME));
    with_raster(|raster| {
        let font_system = &mut raster.font_system;
        let mut buffer = Buffer::new(font_system, Metrics::new(font_px, line_px));
        buffer.set_size(font_system, Some(wrap_width * scale), None);
        buffer.set_text(
            font_system,
            text,
            &with_features(Attrs::new().family(family), features),
            Shaping::Advanced,
            None,
        );
        buffer.shape_until_scroll(font_system, false);

        let mut width: f32 = 0.0;
        let mut lines = 0usize;
        for run in buffer.layout_runs() {
            width = width.max(run.line_w);
            lines += 1;
        }
        (width, lines.max(logical_lines).max(1) as f32 * line_px)
    })
}

/// Like [`measure`], but for the *source* as the live editor styles it: the
/// inner content of bold/italic/code spans is shaped with the matching weight/
/// slant/family (see [`md_edit::emphasis_spans`]). Plain [`measure`] shapes
/// everything in the base weight, so bold content — which is wider — can wrap to
/// more lines than it predicts, leaving the fixed-height editor a line short.
/// Falls back to the same result as [`measure`] for text with no emphasis.
pub fn measure_source(
    text: &str,
    wrap_width: f32,
    font_size: f32,
    scale: f32,
    theme: &Theme,
) -> (f32, f32) {
    let font_px = font_size * scale;
    let line_px = font_px * LINE_HEIGHT;

    let logical_lines = text.lines().count() + usize::from(text.ends_with('\n'));
    if text.trim().is_empty() {
        return (0.0, logical_lines.max(1) as f32 * line_px);
    }

    let text_family = theme
        .font_text
        .map(Family::Name)
        .unwrap_or(Family::Name(crate::platform::UI_FONT_NAME));
    let mono_family = theme.font_mono.map(Family::Name).unwrap_or(Family::Monospace);

    with_raster(|raster| {
        let font_system = &mut raster.font_system;
        let mut buffer = Buffer::new(font_system, Metrics::new(font_px, line_px));
        buffer.set_size(font_system, Some(wrap_width * scale), None);
        buffer.set_wrap(font_system, Wrap::WordOrGlyph);

        let base = Attrs::new().family(text_family);
        buffer.lines.clear();
        let mut spans = Vec::new();
        for line in text.split('\n') {
            let mut attrs_list = AttrsList::new(&base);
            spans.clear();
            md_edit::emphasis_spans(line, &mut spans);
            for (range, emphasis) in &spans {
                let attrs = match emphasis {
                    md_edit::Emphasis::Bold => {
                        Attrs::new().family(text_family).weight(Weight::BOLD)
                    }
                    md_edit::Emphasis::Italic => {
                        Attrs::new().family(text_family).style(Style::Italic)
                    }
                    md_edit::Emphasis::Mono => Attrs::new().family(mono_family),
                };
                attrs_list.add_span(range.clone(), &attrs);
            }
            buffer
                .lines
                .push(BufferLine::new(line, LineEnding::Lf, attrs_list, Shaping::Advanced));
        }
        buffer.shape_until_scroll(font_system, false);

        let mut width: f32 = 0.0;
        let mut lines = 0usize;
        for run in buffer.layout_runs() {
            width = width.max(run.line_w);
            lines += 1;
        }
        (width, lines.max(logical_lines).max(1) as f32 * line_px)
    })
}

/// Measures the source for auto-sizing, taking the card wrap width (as
/// [`measure_markdown`] does) rather than the inner text column: insets
/// `MD_PADDING` a side to lay the source out where the editor will, then adds it
/// back, so the result is directly comparable to [`measure_markdown`]. Sizing a
/// card to this while editing makes it exactly the width the rendered card will
/// be, so the text doesn't reflow when the edit commits.
pub fn measure_source_card(text: &str, wrap_width: f32, font_size: f32, theme: &Theme) -> (f32, f32) {
    let inner = (wrap_width - 2.0 * MD_PADDING).max(1.0);
    let (width, height) = measure_source(text, inner, font_size, 1.0, theme);
    (width + 2.0 * MD_PADDING, height)
}

/// Measures the *rendered* Markdown (max line width, total height) at logical
/// scale, so a card can auto-fit the read view rather than the raw source.
/// Colors don't affect layout, but the theme font family does, so the same
/// theme used to render is passed in.
pub fn measure_markdown(text: &str, wrap_width: f32, font_size: f32, theme: &Theme) -> (f32, f32) {
    let key = (text.trim().to_owned(), bucket(wrap_width), bucket(font_size));
    {
        let mut guard = MEASURE_CACHE.lock().unwrap();
        if let Some(hit) = guard.get_or_insert_with(HashMap::new).get(&key) {
            return *hit;
        }
    }
    let result = measure_markdown_uncached(text, wrap_width, font_size, theme);
    let mut guard = MEASURE_CACHE.lock().unwrap();
    let cache = guard.get_or_insert_with(HashMap::new);
    if cache.len() >= MEASURE_CACHE_CAP {
        cache.clear();
    }
    cache.insert(key, result);
    result
}

fn measure_markdown_uncached(text: &str, wrap_width: f32, font_size: f32, theme: &Theme) -> (f32, f32) {
    let spans = markdown::parse(text.trim());
    let line_px = font_size * LINE_HEIGHT;
    if spans.is_empty() {
        return (0.0, line_px);
    }
    let wrap_px = (wrap_width - 2.0 * MD_PADDING).max(1.0);

    with_raster(|raster| {
        let font_system = &mut raster.font_system;
        let mut buffer = Buffer::new(font_system, Metrics::new(font_size, line_px));
        buffer.set_size(font_system, Some(wrap_px), None);
        let rich: Vec<(&str, Attrs)> = spans
            .iter()
            .map(|span| (span.text.as_str(), span_attrs(span, font_size, theme)))
            .collect();
        let default = base_attrs(theme);
        buffer.set_rich_text(font_system, rich, &default, Shaping::Advanced, Some(Align::Left));
        buffer.shape_until_scroll(font_system, false);

        let mut width = 0.0f32;
        let mut height = 0.0f32;
        for run in buffer.layout_runs() {
            width = width.max(run.line_w);
            height = height.max(run.line_top + run.line_height);
        }
        // Re-add the padding trimmed from the wrap so the card encloses it.
        (width + 2.0 * MD_PADDING, height)
    })
}

/// Renders `text` as Markdown (read mode): inline bold/italic/code, links in
/// the accent color, and heading sizes. Centered within the wrapped column,
/// matching the plain-text look. Falls back to plain text for content with no
/// Markdown.
pub fn snapshot_markdown(
    text: &str,
    width: f32,
    height: f32,
    wrap_width: f32,
    font_size: f32,
    theme: Theme,
    center: bool,
    zoom: f32,
) -> Option<Snapshot> {
    let spans = markdown::parse(text.trim());
    if spans.is_empty() {
        return None;
    }
    // Center only simple (plain/bold/italic) content; richer formatting reads
    // better left-aligned.
    let centered = center && markdown::is_simple(text);

    let k = scale_for(zoom, width, height);
    let pixel_width = (width * k).ceil().max(1.0) as u32;
    let pixel_height = (height * k).ceil().max(1.0) as u32;
    // Extra horizontal padding so left-aligned lines (lists, code) aren't
    // flush against the card edge; the centered offset applies it to both
    // sides.
    let wrap_px = ((wrap_width - 2.0 * MD_PADDING) * k).max(1.0);
    let font_px = font_size * k;
    let line_px = font_px * LINE_HEIGHT;

    with_raster(|raster| {
        let font_system = &mut raster.font_system;

        let mut buffer = Buffer::new(font_system, Metrics::new(font_px, line_px));
        buffer.set_size(font_system, Some(wrap_px), None);

        let rich: Vec<(&str, Attrs)> = spans
            .iter()
            .map(|span| (span.text.as_str(), span_attrs(span, font_px, &theme)))
            .collect();
        let default = base_attrs(&theme);
        let base = if centered { Align::Center } else { Align::Left };
        buffer.set_rich_text(font_system, rich, &default, Shaping::Advanced, Some(base));
        // When centering, still left-align list items and code blocks (they
        // read poorly centered).
        if centered {
            for (line, align) in buffer.lines.iter_mut().zip(line_alignments(&spans)) {
                line.set_align(Some(align));
            }
        }
        buffer.shape_until_scroll(font_system, false);

        // Headings vary line height, so measure the real laid-out extent.
        let text_height = buffer
            .layout_runs()
            .map(|run| run.line_top + run.line_height)
            .fold(0.0, f32::max);
        // Both center the wrap column in the card (left content is left-aligned
        // within it), so the left inset matches plain/centered nodes.
        let offset_x = (pixel_width as f32 - wrap_px) / 2.0;
        let offset_y = ((pixel_height as f32 - text_height) / 2.0).max(0.0);

        let pixels = blit_buffer(
            raster,
            &buffer,
            pixel_width,
            pixel_height,
            offset_x as i32,
            offset_y as i32,
            to_cosmic(theme.text),
        );

        Some(Snapshot {
            handle: image::Handle::from_rgba(pixel_width, pixel_height, pixels),
            zoom,
            scale: k,
            bytes: (pixel_width as usize) * (pixel_height as usize) * 4,
        })
    })
}

fn to_cosmic(color: Color) -> CosmicColor {
    let [r, g, b] = [color.r, color.g, color.b].map(|c| (c * 255.0) as u8);
    CosmicColor::rgba(r, g, b, 255)
}

fn from_cosmic(color: CosmicColor) -> Color {
    Color::from_rgb8(color.r(), color.g(), color.b())
}

/// A positioned, styled run of Markdown text for the live (non-rasterized)
/// render path. Coordinates and `size` are in *logical* (unzoomed) px relative
/// to the card's top-left; the caller scales them by the current zoom. Laying
/// out at logical scale (rather than screen scale) makes the layout cache
/// zoom-invariant — a hit every frame during pan *and* zoom. Mirrors
/// [`snapshot_markdown`]'s layout so both paths render the same wrapping,
/// alignment, sizes, and colors.
#[derive(Debug, Clone)]
pub struct TextRun {
    pub text: String,
    /// Top-left of the run within the card (logical px).
    pub x: f32,
    pub y: f32,
    /// Advance width of the run (logical px), for link hit-testing.
    pub width: f32,
    pub size: f32,
    pub color: Color,
    pub bold: bool,
    pub italic: bool,
    pub mono: bool,
    /// The link destination when this run is part of a link, else `None`.
    pub link: Option<String>,
}

#[derive(PartialEq, Eq, Hash)]
struct LayoutKey {
    text: String,
    width: i32,
    height: i32,
    wrap: i32,
    font_size: i32,
    center: bool,
}

/// Bucket a dimension to 1/4-px so values that only differ by float rounding
/// (e.g. a screen size divided back by zoom) map to the same cache entry.
fn bucket(value: f32) -> i32 {
    (value * 4.0).round() as i32
}

static LAYOUT_CACHE: Mutex<Option<HashMap<LayoutKey, Vec<TextRun>>>> = Mutex::new(None);
const LAYOUT_CACHE_CAP: usize = 1024;

/// Memoizes [`measure_markdown`] by `(trimmed text, wrap, font size)` → `(w, h)`
/// — it shapes with cosmic-text, so calling it per node per frame (node
/// scrollbars) would stutter. Theme is treated as stable within a session, like
/// `LAYOUT_CACHE`.
type MeasureCache = HashMap<(String, i32, i32), (f32, f32)>;
static MEASURE_CACHE: Mutex<Option<MeasureCache>> = Mutex::new(None);

/// Empties the layout and measurement caches.
///
/// Both are self-capping, so the app only calls this when the *fonts* change
/// under them ([`add_embedded_fonts`]): the keys carry text and size but not
/// the typeface those were shaped with, so a swapped theme has to invalidate
/// them wholesale or old entries keep drawing the old face.
///
/// It also exists for benchmarks, which measure the *cold* cost of a text path
/// — what a board pays the first time it lays out, and what dominates a
/// recluster (font fitting binary-searches ~22 distinct sizes per cluster, none
/// of which can hit a cache entry left by another). Measuring only the warm
/// path would report the cost of a hash lookup and call it text layout.
pub fn clear_caches() {
    *LAYOUT_CACHE.lock().unwrap() = None;
    *MEASURE_CACHE.lock().unwrap() = None;
}
const MEASURE_CACHE_CAP: usize = 2048;

static LAYOUT_HITS: AtomicU64 = AtomicU64::new(0);
static LAYOUT_MISSES: AtomicU64 = AtomicU64::new(0);
static LAYOUT_SHAPE_NS: AtomicU64 = AtomicU64::new(0);

/// Cumulative `(hits, misses, shape_nanos)` for the layout cache — the meter
/// diffs these per frame.
pub fn layout_counters() -> (u64, u64, u64) {
    (
        LAYOUT_HITS.load(Ordering::Relaxed),
        LAYOUT_MISSES.load(Ordering::Relaxed),
        LAYOUT_SHAPE_NS.load(Ordering::Relaxed),
    )
}

/// Lays `text` out as Markdown (matching [`snapshot_markdown`]) and calls `f`
/// with the positioned styled runs, caching the layout so repeat frames skip
/// the cosmic-text shaping. `f` runs while the cache is held, avoiding a clone
/// of the runs. Dimensions are logical (unzoomed); see [`TextRun`].
// Mirrors `snapshot_markdown`'s parameters by design.
#[allow(clippy::too_many_arguments)]
pub fn with_markdown_layout<R>(
    text: &str,
    width: f32,
    height: f32,
    wrap_width: f32,
    font_size: f32,
    theme: Theme,
    center: bool,
    f: impl FnOnce(&[TextRun]) -> R,
) -> R {
    let key = LayoutKey {
        text: text.to_owned(),
        width: bucket(width),
        height: bucket(height),
        wrap: bucket(wrap_width),
        font_size: bucket(font_size),
        center,
    };

    // Fast path: a cache hit draws straight from the stored layout.
    {
        let mut guard = LAYOUT_CACHE.lock().unwrap();
        if let Some(runs) = guard.get_or_insert_with(HashMap::new).get(&key) {
            LAYOUT_HITS.fetch_add(1, Ordering::Relaxed);
            return f(runs);
        }
    }

    // Miss: shape once (locks the font system), then store and draw.
    LAYOUT_MISSES.fetch_add(1, Ordering::Relaxed);
    let started = Instant::now();
    let runs = shape_markdown_logical(text, width, height, wrap_width, font_size, theme, center);
    LAYOUT_SHAPE_NS.fetch_add(started.elapsed().as_nanos() as u64, Ordering::Relaxed);

    let mut guard = LAYOUT_CACHE.lock().unwrap();
    let cache = guard.get_or_insert_with(HashMap::new);
    if cache.len() >= LAYOUT_CACHE_CAP {
        cache.clear();
    }
    f(cache.entry(key).or_insert(runs))
}

/// Shapes the Markdown at logical scale (1 px per logical unit) into styled
/// runs. Line breaks are scale-invariant, so drawing scales these by zoom.
fn shape_markdown_logical(
    text: &str,
    width: f32,
    height: f32,
    wrap_width: f32,
    font_size: f32,
    theme: Theme,
    center: bool,
) -> Vec<TextRun> {
    let spans = markdown::parse(text.trim());
    if spans.is_empty() {
        return Vec::new();
    }
    // Center only simple (plain/bold/italic) content; see `snapshot_markdown`.
    let centered = center && markdown::is_simple(text);
    let wrap_px = (wrap_width - 2.0 * MD_PADDING).max(1.0);
    let line_px = font_size * LINE_HEIGHT;

    with_raster(|raster| {
        let font_system = &mut raster.font_system;
        let mut buffer = Buffer::new(font_system, Metrics::new(font_size, line_px));
        buffer.set_size(font_system, Some(wrap_px), None);

        // Tag each span with its index so laid-out glyphs can be traced back to
        // their style (bold/italic/mono).
        let rich: Vec<(&str, Attrs)> = spans
            .iter()
            .enumerate()
            .map(|(i, span)| (span.text.as_str(), span_attrs(span, font_size, &theme).metadata(i)))
            .collect();
        let default = base_attrs(&theme);
        let base = if centered { Align::Center } else { Align::Left };
        buffer.set_rich_text(font_system, rich, &default, Shaping::Advanced, Some(base));
        if centered {
            for (line, align) in buffer.lines.iter_mut().zip(line_alignments(&spans)) {
                line.set_align(Some(align));
            }
        }
        buffer.shape_until_scroll(font_system, false);

        let text_height = buffer
            .layout_runs()
            .map(|run| run.line_top + run.line_height)
            .fold(0.0, f32::max);
        // Column centered in the card (matches `snapshot_markdown`).
        let offset_x = (width - wrap_px) / 2.0;
        let offset_y = ((height - text_height) / 2.0).max(0.0);

        let mut runs = Vec::new();
        for run in buffer.layout_runs() {
            let glyphs = run.glyphs;
            let mut i = 0;
            // Merge adjacent glyphs sharing a span (same style) into one run.
            while i < glyphs.len() {
                let meta = glyphs[i].metadata;
                let mut j = i + 1;
                while j < glyphs.len() && glyphs[j].metadata == meta {
                    j += 1;
                }
                let (first, last) = (&glyphs[i], &glyphs[j - 1]);
                if let Some(slice) = run.text.get(first.start..last.end).filter(|s| !s.is_empty()) {
                    let span = spans.get(meta);
                    runs.push(TextRun {
                        text: slice.to_owned(),
                        x: offset_x + first.x,
                        y: offset_y + run.line_top,
                        width: (last.x + last.w) - first.x,
                        size: first.font_size,
                        color: first.color_opt.map(from_cosmic).unwrap_or(theme.text),
                        bold: span.is_some_and(|s| s.bold || s.heading > 0),
                        italic: span.is_some_and(|s| s.italic),
                        mono: span.is_some_and(|s| s.code),
                        link: span.and_then(|s| s.url.clone()),
                    });
                }
                i = j;
            }
        }
        runs
    })
}

/// Per-line alignment for the rendered Markdown: list items and code-block
/// lines left-align (centered lists/code read poorly); headings and paragraphs
/// stay centered. One entry per `\n`-delimited line, matching the buffer's
/// lines.
fn line_alignments(spans: &[markdown::Span]) -> Vec<Align> {
    let mut aligns = Vec::new();
    let mut has_content = false;
    let mut all_code = true;
    let mut bullet = false;
    let mut line_start = true;

    let finish = |aligns: &mut Vec<Align>, has_content: bool, all_code: bool, bullet: bool| {
        let left = bullet || (has_content && all_code);
        aligns.push(if left { Align::Left } else { Align::Center });
    };

    for span in spans {
        for (i, part) in span.text.split('\n').enumerate() {
            if i > 0 {
                finish(&mut aligns, has_content, all_code, bullet);
                (has_content, all_code, bullet, line_start) = (false, true, false, true);
            }
            if !part.is_empty() {
                if line_start && part.trim_start().starts_with('\u{2022}') {
                    bullet = true;
                }
                has_content = true;
                all_code &= span.code;
                line_start = false;
            }
        }
    }
    finish(&mut aligns, has_content, all_code, bullet);
    aligns
}

/// Enlargement of a heading level over body text.
fn heading_scale(level: u8) -> f32 {
    match level {
        1 => 1.6,
        2 => 1.4,
        3 => 1.25,
        4 => 1.1,
        _ => 1.0,
    }
}

/// Fixed (theme-independent) code palette, tuned for dark cards. Kept separate
/// from the Markdown element colors so highlighting stays legible regardless of
/// the Obsidian theme. `Plain` returns `None` so it falls back to the theme's
/// inline-code color.
fn syntax_color(kind: Syntax) -> Option<Color> {
    let (r, g, b) = match kind {
        Syntax::Plain => return None,
        Syntax::Keyword => (0xc6, 0x78, 0xdd),
        Syntax::Type => (0xe5, 0xc0, 0x7b),
        Syntax::String => (0x98, 0xc3, 0x79),
        Syntax::Number => (0xd1, 0x9a, 0x66),
        Syntax::Comment => (0x7f, 0x84, 0x8e),
        Syntax::Function => (0x61, 0xaf, 0xef),
    };
    Some(Color::from_rgb8(r, g, b))
}

/// Base attributes for a rich-text buffer: the theme's body font (fallback SF).
fn base_attrs(theme: &Theme) -> Attrs<'static> {
    with_features(
        Attrs::new().family(theme.font_text.map(Family::Name).unwrap_or(Family::Name(crate::platform::UI_FONT_NAME))),
        theme.font_features,
    )
}

/// Asks the shaper for `features` on top of `attrs`.
///
/// A font's stylistic alternates are substitution lookups that do nothing until
/// requested by tag — Inter carries a single-storey `a` as `cv11` and shows the
/// double-storey one until asked. Themes declare the set they want in CSS; this
/// is where it reaches the shaper.
fn with_features<'a>(attrs: Attrs<'a>, features: &[Feature]) -> Attrs<'a> {
    if features.is_empty() {
        return attrs;
    }
    attrs.font_features(FontFeatures { features: features.to_vec() })
}

/// cosmic-text attributes for one Markdown span, colored from the theme.
fn span_attrs(span: &markdown::Span, body_px: f32, theme: &Theme) -> Attrs<'static> {
    let mut attrs = Attrs::new().family(if span.code {
        theme.font_mono.map(Family::Name).unwrap_or(Family::Monospace)
    } else {
        theme.font_text.map(Family::Name).unwrap_or(Family::Name(crate::platform::UI_FONT_NAME))
    });
    // Code has its own set: a monospace face's slashed zero and disambiguated
    // letterforms are chosen for code, and are wrong for prose (and vice versa).
    attrs = with_features(
        attrs,
        if span.code { theme.font_features_mono } else { theme.font_features },
    );
    // Body/bold use the theme's declared weights (snapped to the nearest 100),
    // which is where the synthesized faces live — so 450/750 themes render at
    // 500/800 rather than falling back to a system face.
    attrs = attrs.weight(Weight(if span.bold || span.heading > 0 {
        theme.weight_bold
    } else {
        theme.weight_normal
    }));
    if span.italic {
        attrs = attrs.style(Style::Italic);
    }
    let size = body_px * heading_scale(span.heading);
    attrs = attrs.metrics(Metrics::new(size, size * LINE_HEIGHT));

    // Color by element, most specific first (headings/links/code override the
    // body). Bold/italic default to the body color, so only stand out if the
    // theme sets them.
    let color = if span.heading > 0 {
        Some(theme.headings[(span.heading as usize - 1).min(5)])
    } else if span.link {
        Some(theme.link)
    } else if span.code {
        // Highlighted tokens use the fixed code palette; plain code keeps the
        // theme's inline-code color.
        Some(syntax_color(span.syntax).unwrap_or(theme.code))
    } else if span.bold {
        Some(theme.bold)
    } else if span.italic {
        Some(theme.italic)
    } else {
        None
    };
    if let Some(color) = color {
        attrs = attrs.color(to_cosmic(color));
    }
    attrs
}

/// Composites a shaped buffer onto a fresh transparent RGBA bitmap at the
/// given offset, with straight-alpha src-over. `default` colors glyphs whose
/// span sets no explicit color.
fn blit_buffer(
    raster: &mut Raster,
    buffer: &Buffer,
    pixel_width: u32,
    pixel_height: u32,
    offset_x: i32,
    offset_y: i32,
    default: CosmicColor,
) -> Vec<u8> {
    let mut pixels = vec![0u8; (pixel_width * pixel_height * 4) as usize];
    buffer.draw(
        &mut raster.font_system,
        &mut raster.cache,
        default,
        |x, y, w, h, glyph_color| {
            let alpha = glyph_color.a();
            if alpha == 0 {
                return;
            }
            for dy in 0..h as i32 {
                for dx in 0..w as i32 {
                    let px = x + dx + offset_x;
                    let py = y + dy + offset_y;
                    if px < 0 || py < 0 || px >= pixel_width as i32 || py >= pixel_height as i32 {
                        continue;
                    }
                    let i = ((py as u32 * pixel_width + px as u32) * 4) as usize;
                    let a = alpha as u32;
                    let inv = 255 - a;
                    pixels[i] = ((glyph_color.r() as u32 * a + pixels[i] as u32 * inv) / 255) as u8;
                    pixels[i + 1] =
                        ((glyph_color.g() as u32 * a + pixels[i + 1] as u32 * inv) / 255) as u8;
                    pixels[i + 2] =
                        ((glyph_color.b() as u32 * a + pixels[i + 2] as u32 * inv) / 255) as u8;
                    pixels[i + 3] = (a + pixels[i + 3] as u32 * inv / 255).min(255) as u8;
                }
            }
        },
    );
    pixels
}

#[cfg(test)]
mod scale_tests {
    use super::*;

    /// Card sizes spanning what the board actually holds.
    const SMALL: (f32, f32) = (250.0, 120.0);
    const LARGE: (f32, f32) = (800.0, 600.0);

    #[test]
    fn the_scale_ceiling_bounds_every_card_a_board_can_hold() {
        // The point of capping on scale rather than dimension: however far you
        // zoom, and whatever the card's size, glyphs are never rasterized past
        // MAX_SCALE — which is what bounds the cost.
        //
        // `150.0` is the narrowest a text node can be dragged (see
        // `App::resized_text_size`), so it's the smallest card that can reach
        // this in practice.
        for (w, h) in [SMALL, LARGE, (150.0, 40.0), (2000.0, 1500.0)] {
            for zoom in [1.0, 2.0, 4.0, 6.0, 20.0] {
                let k = scale_for(zoom, w, h);
                assert!(
                    k <= MAX_SCALE + f32::EPSILON,
                    "{w}x{h} @ {zoom}x gave scale {k}, over the {MAX_SCALE} ceiling"
                );
            }
        }
    }

    #[test]
    fn the_minimum_texture_floor_outranks_the_ceiling() {
        // A card smaller than `MIN_DIMENSION / MAX_SCALE` (~85 px) can't reach
        // the floor's texture size without passing the ceiling, and the floor
        // wins — deliberately. It stays cheap regardless, because the cost is
        // glyphs rasterized, and a card that small holds almost no text.
        let k = scale_for(1.0, 60.0, 40.0);
        assert!(k > MAX_SCALE);
        assert_eq!(k, MIN_DIMENSION / 60.0);
    }

    #[test]
    fn the_texture_bound_still_holds() {
        // MAX_SCALE is a quality choice; the dimension cap is a hard limit —
        // past it a texture fragments across atlas layers and the node blanks.
        for (w, h) in [SMALL, LARGE, (2000.0, 1500.0), (4000.0, 400.0)] {
            let k = scale_for(20.0, w, h);
            let longest = w.max(h) * k;
            assert!(
                longest <= MAX_DIMENSION + 0.5,
                "{w}x{h} baked a {longest} px texture, over the {MAX_DIMENSION} limit"
            );
        }
    }

    #[test]
    fn a_tiny_on_screen_node_still_bakes_a_usable_texture() {
        // Zoomed right out, a proportional scale would bake a ~100 px mush that
        // falls apart the moment you zoom in, before the re-raster lands.
        let k = scale_for(0.05, SMALL.0, SMALL.1);
        assert!(SMALL.0.max(SMALL.1) * k >= MIN_DIMENSION - 0.5);
    }

    #[test]
    fn scale_rises_with_zoom_until_it_is_capped() {
        let k = |zoom| scale_for(zoom, SMALL.0, SMALL.1);
        assert!(k(0.5) < k(1.0), "below the cap, resolution follows zoom");
        assert_eq!(k(2.0), MAX_SCALE, "and levels off at the ceiling");
        assert_eq!(k(6.0), MAX_SCALE);
    }

    #[test]
    fn resolution_no_longer_inverts_with_card_size() {
        // The bug this cap fixes: a small card used to get 8.19x while a large
        // one — holding more text — got 2.56x at the same zoom. A large card is
        // still texture-bound below a small one, but never above the ceiling,
        // so the gap is the hard limit rather than an accident of sizing.
        let small = scale_for(4.0, SMALL.0, SMALL.1);
        let large = scale_for(4.0, LARGE.0, LARGE.1);
        assert_eq!(small, MAX_SCALE);
        assert!(large < small, "a big card is still bound by the texture limit");
        assert_eq!(large, MAX_DIMENSION / LARGE.0.max(LARGE.1));
    }
}

#[cfg(test)]
mod measure_tests {
    use super::*;

    #[test]
    fn measure_source_accounts_for_bold_widening() {
        warm_up();
        let theme = Theme::default();
        // The only bold span (`**Ask AI**`) makes the source wider than the same
        // text shaped all-regular. `measure` shapes it regular; `measure_source`
        // must account for the bold width the editor renders — otherwise, at a
        // wrap width between the two, the source wouldn't wrap and the
        // fixed-height editor would scroll.
        let text = "Right-click a note and choose **Ask AI** to grow connected notes from your idea.";

        // Self-calibrate to this machine's font rather than a hard-coded pixel
        // threshold (fonts, and thus wrap points, differ across platforms/CI).
        // Measure each on a single line by allowing effectively unlimited width.
        let (plain_w, _) = measure(text, 100_000.0, 17.0, 1.0, theme.font_text, theme.font_features);
        let (source_w, _) = measure_source(text, 100_000.0, 17.0, 1.0, &theme);

        // A font with no wider bold face (some minimal CI fonts, or faux-bold
        // that leaves advances unchanged) can't exhibit the widening this guards.
        if source_w <= plain_w {
            return;
        }

        // Between the two single-line widths, plain still fits on one line but the
        // wider bold source must wrap — so the source measures taller.
        let wrap = (plain_w + source_w) / 2.0;
        let (_, plain_h) = measure(text, wrap, 17.0, 1.0, theme.font_text, theme.font_features);
        let (_, source_h) = measure_source(text, wrap, 17.0, 1.0, &theme);
        assert!(
            source_h > plain_h,
            "bold source should wrap taller than plain at width {wrap}: {source_h} vs {plain_h}"
        );
    }

    #[test]
    fn editing_and_commit_autosize_to_the_same_width() {
        warm_up();
        let theme = Theme::default();
        // Mirror src/app/mod.rs::autosize_node exactly: node width = measured text
        // width + NODE_PAD_X, both branches measuring at the same card wrap column.
        // The editing branch (measure_source_card) and the commit branch
        // (measure_markdown) both inset MD_PADDING a side and add it back, so a
        // plain-text card is the same width whether you're editing or reading it —
        // its text never reflows on commit.
        const MIN_W: f32 = 250.0; // config.new_node_min_width
        const MAX_W: f32 = 420.0; // config.max_node_width
        let node = |text_w: f32| (text_w + NODE_PAD_X).clamp(MIN_W, MAX_W);

        // Plain text (no markup, so the editor and read view lay out identically)
        // sized to clear the min-width clamp but stay under the cap — the band
        // where both branches are free to hug the text and so must agree.
        let text = "alpha beta gamma delta epsilon zeta eta theta";
        let w_edit = node(measure_source_card(text, card_text_wrap(MAX_W), 17.0, &theme).0);
        let w_commit = node(measure_markdown(text, card_text_wrap(MAX_W), 17.0, &theme).0);

        // Guard: only meaningful when neither side is pinned to a bound on this
        // machine's font (else a difference could be a clamp, not a real gap).
        assert!(
            w_edit > MIN_W && w_edit < MAX_W && w_commit > MIN_W && w_commit < MAX_W,
            "calibration: widths clamped (edit {w_edit:.1}, commit {w_commit:.1}); \
             pick a sample whose single-line width lands mid-band",
        );

        // Sizing the editing card to the *bare* source width (measuring at the
        // card wrap without the MD_PADDING inset dance) regresses this by
        // 2*MD_PADDING: the card would then widen mid-edit and snap on Done.
        assert!(
            (w_edit - w_commit).abs() < 1.0,
            "editing width {w_edit:.1} must equal commit width {w_commit:.1} \
             (off by {:.1})",
            w_commit - w_edit,
        );
    }

    #[test]
    fn measure_source_matches_measure_for_plain_text() {
        warm_up();
        let theme = Theme::default();
        let text = "Just a plain label with no markup at all in it here";
        assert_eq!(
            measure_source(text, 200.0, 17.0, 1.0, &theme),
            measure(text, 200.0, 17.0, 1.0, theme.font_text, theme.font_features),
        );
    }

    #[test]
    fn edit_box_height_matches_the_rendered_card() {
        warm_up();
        let theme = Theme::default();
        // The editor measures the source at the card wrap minus the inset each
        // side (the app's `EDITOR_INSET` == `MD_PADDING`); read mode
        // (`measure_markdown`) insets the same amount internally. For plain text
        // the two must lay out to the same height, or the note jumps when you
        // start editing — the bug this guards. Several widths so at least one
        // wraps to multiple lines.
        let text =
            "one two three four five six seven eight nine ten eleven twelve thirteen fourteen";
        for card_wrap in [120.0_f32, 170.0, 260.0] {
            let (_, read_h) = measure_markdown(text, card_wrap, 17.0, &theme);
            let (_, edit_h) = measure_source(text, card_wrap - 2.0 * MD_PADDING, 17.0, 1.0, &theme);
            assert!(
                (read_h - edit_h).abs() < 1.0,
                "edit box height {edit_h} must match the rendered card {read_h} at wrap {card_wrap}",
            );
        }
    }

    #[test]
    fn measure_markdown_counts_the_inset_wrap_column() {
        warm_up();
        let theme = Theme::default();
        // A card's height floor (on resize) must be measured with `measure_markdown`,
        // which insets `MD_PADDING` a side like the render — plain `measure` wraps at
        // the full width and under-counts lines, leaving the card a line short. At a
        // width where the text fits literally but wraps once inset, the rendered
        // measurement must be taller.
        let text = "one two three four five six seven eight nine ten eleven twelve";
        // Single-line width on this machine's font.
        let (w, _) = measure(text, 100_000.0, 17.0, 1.0, theme.font_text, theme.font_features);
        // Fits at the literal width `w + MD_PADDING`, but wraps once inset
        // (`w + MD_PADDING - 2*MD_PADDING = w - MD_PADDING < w`).
        let arg = w + MD_PADDING;
        let (_, plain_h) = measure(text, arg, 17.0, 1.0, theme.font_text, theme.font_features);
        let (_, md_h) = measure_markdown(text, arg, 17.0, &theme);
        assert!(
            md_h > plain_h,
            "measure_markdown must count the inset (rendered) wrap: {md_h} vs {plain_h}",
        );
    }

    /// A synthetic face, for the weight arithmetic.
    fn face(weight: u16, upright: bool) -> Face {
        Face { id: fontdb::ID::dummy(), weight, upright }
    }

    #[test]
    fn a_family_is_asked_for_at_a_weight_it_actually_has() {
        // The whole point: ask Futura for a 400 and you don't get its regular,
        // you get the system font. Its lightest upright is a 500.
        let futura = [face(500, true), face(700, true), face(800, true)];
        assert_eq!(nearest_weight(&futura, REGULAR), 500);
        assert_eq!(nearest_weight(&futura, BOLD), 700);

        // Arial Black is a 900 and nothing else, so that's both.
        let black = [face(900, true)];
        assert_eq!(nearest_weight(&black, REGULAR), 900);
        assert_eq!(nearest_weight(&black, BOLD), 900);

        // Skia reports 5, which is not a standard step and still has to resolve
        // to itself rather than to a 400 that isn't there.
        assert_eq!(nearest_weight(&[face(5, true)], REGULAR), 5);
    }

    #[test]
    fn a_tie_goes_to_the_heavier_face() {
        // 300 and 500 are equally far from a 400; a bold that lands between two
        // faces should read as the bolder of them.
        assert_eq!(nearest_weight(&[face(300, true), face(500, true)], REGULAR), 500);
        assert_eq!(nearest_weight(&[face(600, true), face(800, true)], BOLD), 800);
    }

    #[test]
    fn italic_faces_only_count_where_a_family_has_nothing_else() {
        // Weighing a roman against its own italic would pick the wrong face…
        let both = [face(400, true), face(900, false)];
        assert_eq!(nearest_weight(&both, BOLD), 400, "the upright face, far as it is");

        // …but Zapfino, Apple Chancery, Brush Script and Snell Roundhand are
        // marked italic throughout, and excluding them outright would drop four
        // perfectly pickable fonts off the list.
        assert_eq!(nearest_weight(&[face(500, false)], REGULAR), 500);
    }

    #[test]
    fn every_offered_family_resolves_to_the_weight_it_was_listed_with() {
        // `families` and `family_weights` read the same database by different
        // routes — the picker previews through the first and the board renders
        // through the second, so a disagreement shows up as a font that looks
        // one way in the list and another once chosen.
        for family in families() {
            let weights = family_weights(&family.name);
            assert_eq!(
                weights.map(|(normal, _)| normal),
                Some(family.weight),
                "{} listed at {} but resolved elsewhere",
                family.name,
                family.weight,
            );
        }
    }


    #[test]
    fn the_themes_features_reach_the_shaper() {
        use cosmic_text::FeatureTag;
        const SINGLE_STOREY_A: &[Feature] = &[Feature { tag: FeatureTag::new(b"cv11"), value: 1 }];
        const SLASHED_ZERO: &[Feature] = &[Feature { tag: FeatureTag::new(b"zero"), value: 1 }];

        let mut theme = Theme::default();
        assert!(
            base_attrs(&theme).font_features.features.is_empty(),
            "no theme features means the font's own defaults, not a set of ours",
        );

        theme.font_features = SINGLE_STOREY_A;
        theme.font_features_mono = SLASHED_ZERO;
        assert_eq!(base_attrs(&theme).font_features.features, SINGLE_STOREY_A);

        // Prose and code ask for different sets: a monospace face's slashed
        // zero is chosen for code and wrong in a sentence.
        let span = |code| markdown::Span {
            text: "x".to_owned(),
            bold: false,
            italic: false,
            code,
            heading: 0,
            link: false,
            url: None,
            syntax: Syntax::Plain,
        };
        let (prose, code) = (span(false), span(true));
        assert_eq!(span_attrs(&prose, 17.0, &theme).font_features.features, SINGLE_STOREY_A);
        assert_eq!(span_attrs(&code, 17.0, &theme).font_features.features, SLASHED_ZERO);
    }

}
