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
    LineEnding, Metrics, Shaping, Style, SwashCache, Weight, Wrap,
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

/// Cap on bitmap dimensions. Kept within a single atlas layer: larger
/// textures fragment across layers and can take iced several frames to
/// allocate, leaving the node blank meanwhile. Costs some sharpness on
/// large nodes at extreme zoom.
const MAX_DIMENSION: f32 = 2048.0;

/// Floor on bitmap dimensions (longest side). Zoomed all the way out, a node's
/// on-screen size is tiny, so a zoom-proportional scale would bake a ~100 px
/// texture that's a blurry mess the instant you zoom in (before the re-raster
/// lands). Baking at least this resolution keeps the first zoom-in sharp.
const MIN_DIMENSION: f32 = 512.0;

/// Matches `canvas::Text::default()`, which uses `LineHeight::Relative(1.2)`
/// (unlike iced widgets, which default to 1.3).
pub const LINE_HEIGHT: f32 = 1.2;

/// Horizontal padding (logical px per side) inset into the wrap column when
/// rendering Markdown, so left-aligned lines have a left margin.
const MD_PADDING: f32 = 8.0;

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
}

fn with_raster<R>(f: impl FnOnce(&mut Raster) -> R) -> R {
    let mut guard = RASTER.lock().unwrap();
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
    // Texture longest side kept within [MIN_DIMENSION, MAX_DIMENSION].
    (zoom * RASTER_DENSITY)
        .min(MAX_DIMENSION / longest)
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
            &Attrs::new().family(family),
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

/// Measures the *rendered* Markdown (max line width, total height) at logical
/// scale, so a card can auto-fit the read view rather than the raw source.
/// Colors don't affect layout, but the theme font family does, so the same
/// theme used to render is passed in.
pub fn measure_markdown(text: &str, wrap_width: f32, font_size: f32, theme: &Theme) -> (f32, f32) {
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
    Attrs::new().family(theme.font_text.map(Family::Name).unwrap_or(Family::Name(crate::platform::UI_FONT_NAME)))
}

/// cosmic-text attributes for one Markdown span, colored from the theme.
fn span_attrs(span: &markdown::Span, body_px: f32, theme: &Theme) -> Attrs<'static> {
    let mut attrs = Attrs::new().family(if span.code {
        theme.font_mono.map(Family::Name).unwrap_or(Family::Monospace)
    } else {
        theme.font_text.map(Family::Name).unwrap_or(Family::Name(crate::platform::UI_FONT_NAME))
    });
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
        let (plain_w, _) = measure(text, 100_000.0, 17.0, 1.0, theme.font_text);
        let (source_w, _) = measure_source(text, 100_000.0, 17.0, 1.0, &theme);

        // A font with no wider bold face (some minimal CI fonts, or faux-bold
        // that leaves advances unchanged) can't exhibit the widening this guards.
        if source_w <= plain_w {
            return;
        }

        // Between the two single-line widths, plain still fits on one line but the
        // wider bold source must wrap — so the source measures taller.
        let wrap = (plain_w + source_w) / 2.0;
        let (_, plain_h) = measure(text, wrap, 17.0, 1.0, theme.font_text);
        let (_, source_h) = measure_source(text, wrap, 17.0, 1.0, &theme);
        assert!(
            source_h > plain_h,
            "bold source should wrap taller than plain at width {wrap}: {source_h} vs {plain_h}"
        );
    }

    #[test]
    fn measure_source_matches_measure_for_plain_text() {
        warm_up();
        let theme = Theme::default();
        let text = "Just a plain label with no markup at all in it here";
        assert_eq!(
            measure_source(text, 200.0, 17.0, 1.0, &theme),
            measure(text, 200.0, 17.0, 1.0, theme.font_text),
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
        let (w, _) = measure(text, 100_000.0, 17.0, 1.0, theme.font_text);
        // Fits at the literal width `w + MD_PADDING`, but wraps once inset
        // (`w + MD_PADDING - 2*MD_PADDING = w - MD_PADDING < w`).
        let arg = w + MD_PADDING;
        let (_, plain_h) = measure(text, arg, 17.0, 1.0, theme.font_text);
        let (_, md_h) = measure_markdown(text, arg, 17.0, &theme);
        assert!(
            md_h > plain_h,
            "measure_markdown must count the inset (rendered) wrap: {md_h} vs {plain_h}",
        );
    }
}
