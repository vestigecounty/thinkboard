//! Draw text.
pub mod cache;
pub mod editor;
pub mod paragraph;

pub use cache::Cache;
pub use editor::Editor;
pub use paragraph::Paragraph;

pub use cosmic_text;

use crate::core::alignment;
use crate::core::font::{self, Font};
use crate::core::text::{Alignment, Shaping, Wrapping};
use crate::core::{Color, Pixels, Point, Rectangle, Size, Transformation};

use std::borrow::Cow;
use std::collections::HashSet;
use std::sync::atomic;
use std::sync::{Arc, OnceLock, RwLock, Weak};

/// A text primitive.
#[derive(Debug, Clone, PartialEq)]
pub enum Text {
    /// A paragraph.
    #[allow(missing_docs)]
    Paragraph {
        paragraph: paragraph::Weak,
        position: Point,
        color: Color,
        clip_bounds: Rectangle,
        transformation: Transformation,
    },
    /// An editor.
    #[allow(missing_docs)]
    Editor {
        editor: editor::Weak,
        position: Point,
        color: Color,
        clip_bounds: Rectangle,
        transformation: Transformation,
    },
    /// Some cached text.
    Cached {
        /// The contents of the text.
        content: String,
        /// The bounds of the text.
        bounds: Rectangle,
        /// The color of the text.
        color: Color,
        /// The size of the text in logical pixels.
        size: Pixels,
        /// The line height of the text.
        line_height: Pixels,
        /// The font of the text.
        font: Font,
        /// The horizontal alignment of the text.
        align_x: Alignment,
        /// The vertical alignment of the text.
        align_y: alignment::Vertical,
        /// The shaping strategy of the text.
        shaping: Shaping,
        /// The clip bounds of the text.
        clip_bounds: Rectangle,
    },
    /// Some raw text.
    #[allow(missing_docs)]
    Raw {
        raw: Raw,
        transformation: Transformation,
    },
}

impl Text {
    /// Returns the visible bounds of the [`Text`].
    pub fn visible_bounds(&self) -> Option<Rectangle> {
        match self {
            Text::Paragraph {
                position,
                paragraph,
                clip_bounds,
                transformation,
                ..
            } => Rectangle::new(*position, paragraph.min_bounds)
                .intersection(clip_bounds)
                .map(|bounds| bounds * *transformation),
            Text::Editor {
                editor,
                position,
                clip_bounds,
                transformation,
                ..
            } => Rectangle::new(*position, editor.bounds)
                .intersection(clip_bounds)
                .map(|bounds| bounds * *transformation),
            Text::Cached {
                bounds,
                clip_bounds,
                ..
            } => bounds.intersection(clip_bounds),
            Text::Raw { raw, .. } => Some(raw.clip_bounds),
        }
    }
}

/// The regular variant of the [Fira Sans] font.
///
/// It is loaded as part of the default fonts when the `fira-sans`
/// feature is enabled.
///
/// [Fira Sans]: https://mozilla.github.io/Fira/
#[cfg(feature = "fira-sans")]
pub const FIRA_SANS_REGULAR: &[u8] =
    include_bytes!("../fonts/FiraSans-Regular.ttf").as_slice();

/// Returns the global [`FontSystem`].
pub fn font_system() -> &'static RwLock<FontSystem> {
    static FONT_SYSTEM: OnceLock<RwLock<FontSystem>> = OnceLock::new();

    FONT_SYSTEM.get_or_init(|| {
        RwLock::new(FontSystem {
            raw: cosmic_text::FontSystem::new_with_fonts([
                cosmic_text::fontdb::Source::Binary(Arc::new(
                    include_bytes!("../fonts/Iced-Icons.ttf").as_slice(),
                )),
                #[cfg(feature = "fira-sans")]
                cosmic_text::fontdb::Source::Binary(Arc::new(
                    include_bytes!("../fonts/FiraSans-Regular.ttf").as_slice(),
                )),
            ]),
            loaded_fonts: HashSet::new(),
            version: Version::default(),
        })
    })
}

/// A set of system fonts.
pub struct FontSystem {
    raw: cosmic_text::FontSystem,
    loaded_fonts: HashSet<usize>,
    version: Version,
}

impl FontSystem {
    /// Returns the raw [`cosmic_text::FontSystem`].
    pub fn raw(&mut self) -> &mut cosmic_text::FontSystem {
        &mut self.raw
    }

    /// Loads a font from its bytes.
    pub fn load_font(&mut self, bytes: Cow<'static, [u8]>) {
        if let Cow::Borrowed(bytes) = bytes {
            let address = bytes.as_ptr() as usize;

            if !self.loaded_fonts.insert(address) {
                return;
            }
        }

        let _ = self.raw.db_mut().load_font_source(
            cosmic_text::fontdb::Source::Binary(Arc::new(bytes.into_owned())),
        );

        self.version = Version(self.version.0 + 1);
    }

    /// Returns the current [`Version`] of the [`FontSystem`].
    ///
    /// Loading a font will increase the version of a [`FontSystem`].
    pub fn version(&self) -> Version {
        self.version
    }
}

/// A version number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Version(u32);

/// A weak reference to a [`cosmic_text::Buffer`] that can be drawn.
#[derive(Debug, Clone)]
pub struct Raw {
    /// A weak reference to a [`cosmic_text::Buffer`].
    pub buffer: Weak<cosmic_text::Buffer>,
    /// The position of the text.
    pub position: Point,
    /// The color of the text.
    pub color: Color,
    /// The clip bounds of the text.
    pub clip_bounds: Rectangle,
}

impl PartialEq for Raw {
    fn eq(&self, _other: &Self) -> bool {
        // TODO: There is no proper way to compare raw buffers
        // For now, no two instances of `Raw` text will be equal.
        // This should be fine, but could trigger unnecessary redraws
        // in the future.
        false
    }
}

/// Measures the dimensions of the given [`cosmic_text::Buffer`].
pub fn measure(buffer: &cosmic_text::Buffer) -> (Size, bool) {
    let (width, height, has_rtl) = buffer.layout_runs().fold(
        (0.0, 0.0, false),
        |(width, height, has_rtl), run| {
            (
                run.line_w.max(width),
                height + run.line_height,
                has_rtl || run.rtl,
            )
        },
    );

    (Size::new(width, height), has_rtl)
}

/// Aligns the given [`cosmic_text::Buffer`] with the given [`Alignment`]
/// and returns its minimum [`Size`].
pub fn align(
    buffer: &mut cosmic_text::Buffer,
    font_system: &mut cosmic_text::FontSystem,
    alignment: Alignment,
) -> Size {
    let (min_bounds, has_rtl) = measure(buffer);
    let mut needs_relayout = has_rtl;

    if let Some(align) = to_align(alignment) {
        let has_multiple_lines = buffer.lines.len() > 1
            || buffer.lines.first().is_some_and(|line| {
                line.layout_opt().is_some_and(|layout| layout.len() > 1)
            });

        if has_multiple_lines {
            for line in &mut buffer.lines {
                let _ = line.set_align(Some(align));
            }

            needs_relayout = true;
        } else if let Some(line) = buffer.lines.first_mut() {
            needs_relayout = line.set_align(None);
        }
    }

    // TODO: Avoid relayout with some changes to `cosmic-text` (?)
    if needs_relayout {
        log::trace!("Relayouting paragraph...");

        buffer.set_size(
            font_system,
            Some(min_bounds.width),
            Some(min_bounds.height),
        );
    }

    min_bounds
}

/// ThinkBoard patch: the OpenType features every shaped run asks for.
///
/// A font's stylistic alternates — Inter's single-storey `a` (`cv11`), its
/// disambiguation set (`ss02`), a theme's chosen ligatures — are substitution
/// lookups that lie dormant unless the shaper requests them by tag. On the web
/// that request is `font-feature-settings`; iced's [`Font`] has no equivalent,
/// so an Obsidian theme's typography arrives here and is silently ignored.
///
/// A global, and sound for the same reason the editor's alignment is one: the
/// features come from the theme, so every run on screen wants the same set.
/// Text shaped before a theme changes keeps the old features until the caches
/// are dropped, which is what `Renderer::clear_caches` is called for.
static FONT_FEATURES: RwLock<Vec<cosmic_text::Feature>> = RwLock::new(Vec::new());

/// ThinkBoard patch: sets the OpenType features applied to all shaped text.
///
/// Each entry is a four-byte tag and a value — `1` to enable, `0` to disable,
/// or an index for a feature that takes one. Pass an empty slice for the
/// font's own defaults.
pub fn set_font_features(features: &[cosmic_text::Feature]) {
    let mut slot = FONT_FEATURES.write().expect("font features");
    if slot[..] == *features {
        return;
    }
    slot.clear();
    slot.extend_from_slice(features);
    let _ = FONT_FEATURES_GENERATION.fetch_add(1, atomic::Ordering::Relaxed);
}

/// ThinkBoard patch: bumped whenever [`set_font_features`] changes the set.
///
/// The features are a global rather than part of a [`Font`], so they can't be
/// part of a text cache key either — [`cache::Cache`] watches this instead and
/// drops everything it shaped under a previous set.
static FONT_FEATURES_GENERATION: atomic::AtomicU64 = atomic::AtomicU64::new(0);

/// ThinkBoard patch: which set of features is in force, as a counter.
pub fn font_features_generation() -> u64 {
    FONT_FEATURES_GENERATION.load(atomic::Ordering::Relaxed)
}

/// Returns the attributes of the given [`Font`].
pub fn to_attributes(font: Font) -> cosmic_text::Attrs<'static> {
    let attrs = cosmic_text::Attrs::new()
        .family(to_family(font.family))
        .weight(to_weight(font.weight))
        .stretch(to_stretch(font.stretch))
        .style(to_style(font.style));

    // ThinkBoard patch: apply the features set above. Read on every shaped run,
    // so the lock is taken often but held for a clone of a handful of tags.
    let features = FONT_FEATURES.read().expect("font features");
    if features.is_empty() {
        return attrs;
    }
    cosmic_text::Attrs {
        font_features: cosmic_text::FontFeatures { features: features.clone() },
        ..attrs
    }
}

fn to_family(family: font::Family) -> cosmic_text::Family<'static> {
    match family {
        font::Family::Name(name) => cosmic_text::Family::Name(name),
        font::Family::SansSerif => cosmic_text::Family::SansSerif,
        font::Family::Serif => cosmic_text::Family::Serif,
        font::Family::Cursive => cosmic_text::Family::Cursive,
        font::Family::Fantasy => cosmic_text::Family::Fantasy,
        font::Family::Monospace => cosmic_text::Family::Monospace,
    }
}

fn to_weight(weight: font::Weight) -> cosmic_text::Weight {
    match weight {
        font::Weight::Thin => cosmic_text::Weight::THIN,
        font::Weight::ExtraLight => cosmic_text::Weight::EXTRA_LIGHT,
        font::Weight::Light => cosmic_text::Weight::LIGHT,
        font::Weight::Normal => cosmic_text::Weight::NORMAL,
        font::Weight::Medium => cosmic_text::Weight::MEDIUM,
        font::Weight::Semibold => cosmic_text::Weight::SEMIBOLD,
        font::Weight::Bold => cosmic_text::Weight::BOLD,
        font::Weight::ExtraBold => cosmic_text::Weight::EXTRA_BOLD,
        font::Weight::Black => cosmic_text::Weight::BLACK,
    }
}

fn to_stretch(stretch: font::Stretch) -> cosmic_text::Stretch {
    match stretch {
        font::Stretch::UltraCondensed => cosmic_text::Stretch::UltraCondensed,
        font::Stretch::ExtraCondensed => cosmic_text::Stretch::ExtraCondensed,
        font::Stretch::Condensed => cosmic_text::Stretch::Condensed,
        font::Stretch::SemiCondensed => cosmic_text::Stretch::SemiCondensed,
        font::Stretch::Normal => cosmic_text::Stretch::Normal,
        font::Stretch::SemiExpanded => cosmic_text::Stretch::SemiExpanded,
        font::Stretch::Expanded => cosmic_text::Stretch::Expanded,
        font::Stretch::ExtraExpanded => cosmic_text::Stretch::ExtraExpanded,
        font::Stretch::UltraExpanded => cosmic_text::Stretch::UltraExpanded,
    }
}

fn to_style(style: font::Style) -> cosmic_text::Style {
    match style {
        font::Style::Normal => cosmic_text::Style::Normal,
        font::Style::Italic => cosmic_text::Style::Italic,
        font::Style::Oblique => cosmic_text::Style::Oblique,
    }
}

fn to_align(alignment: Alignment) -> Option<cosmic_text::Align> {
    match alignment {
        Alignment::Default => None,
        Alignment::Left => Some(cosmic_text::Align::Left),
        Alignment::Center => Some(cosmic_text::Align::Center),
        Alignment::Right => Some(cosmic_text::Align::Right),
        Alignment::Justified => Some(cosmic_text::Align::Justified),
    }
}

/// Converts some [`Shaping`] strategy to a [`cosmic_text::Shaping`] strategy.
pub fn to_shaping(shaping: Shaping, text: &str) -> cosmic_text::Shaping {
    match shaping {
        Shaping::Auto => {
            if text.is_ascii() {
                cosmic_text::Shaping::Basic
            } else {
                cosmic_text::Shaping::Advanced
            }
        }
        Shaping::Basic => cosmic_text::Shaping::Basic,
        Shaping::Advanced => cosmic_text::Shaping::Advanced,
    }
}

/// Converts some [`Wrapping`] strategy to a [`cosmic_text::Wrap`] strategy.
pub fn to_wrap(wrapping: Wrapping) -> cosmic_text::Wrap {
    match wrapping {
        Wrapping::None => cosmic_text::Wrap::None,
        Wrapping::Word => cosmic_text::Wrap::Word,
        Wrapping::Glyph => cosmic_text::Wrap::Glyph,
        Wrapping::WordOrGlyph => cosmic_text::Wrap::WordOrGlyph,
    }
}

/// Converts some [`Color`] to a [`cosmic_text::Color`].
pub fn to_color(color: Color) -> cosmic_text::Color {
    let [r, g, b, a] = color.into_rgba8();

    cosmic_text::Color::rgba(r, g, b, a)
}

/// A text renderer coupled to `iced_graphics`.
pub trait Renderer {
    /// Draws the given [`Raw`] text.
    fn fill_raw(&mut self, raw: Raw);
}
