//! The settings form — five tabs by topic over [`Config`], opened in the board
//! (palette → Settings) or in a window of its own (the tray). This module owns
//! the message/field vocabulary and the `App` side of applying an edit: each
//! one is written to the config, clamped, persisted, and anything it affects
//! live is refreshed (no Save button). What it looks like is [`form`].
//!
//! **Tabs by topic, not by importance.** The form used to be one long column,
//! its tuning knobs behind an "Advanced" disclosure that forgot it was open.
//! Now each tab leads with what's changed most, AI has a tab of its own, and
//! the knobs that are only ever tuned live in the file alone ([`FILE_ONLY`]) —
//! counted at the foot of Advanced, with its path and a Reload.
//!
//! Text fields are backed by `App::settings_drafts` (populated from the config
//! when the form opens) so what you type is what's shown — parsing a number
//! back into the field on every keystroke would eat a trailing "." or "0".

mod form;

use std::path::{Path, PathBuf};

use iced::Task;
use iced::widget::image;

use crate::config::{ArrowTip, Config, LinkStyle, OverlayBackdrop};
use crate::platform::BackdropPicture;

use super::{App, Message};

pub(super) use form::{modal, window};

/// Names the single [`Config`] field an edit targets — and the row that shows
/// it, which is how a row knows it's focused, changed, or resettable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Field {
    // General
    SummonHotkey,
    // macOS-only experimental trackpad summon gesture; plumbing stays
    // cross-platform so a hand-set value is honored if copied between machines.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    SummonGesture,
    Vault,
    DismissOnBackgroundClick,
    DoubleClickAddsNote,
    // Only surfaced in the UI on macOS (its Spaces are the only workspaces we
    // watch), but the apply/config plumbing stays cross-platform.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    PersistAcrossSpaces,
    OverlayDim,
    Windowed,
    // Appearance
    Theme,
    Font,
    OverlayBackdrop,
    OverlayImage,
    // macOS-only menu-bar icon appearance; plumbing stays cross-platform so a
    // hand-set value is honored if a config is copied between machines.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    TrayIconMonochrome,
    LinkStyle,
    ArrowTip,
    CenterNodeText,
    NodeScrollbars,
    ColoredCardOpacity,
    MaxNodeWidth,
    // Canvas
    SnapToGrid,
    SnapGridSize,
    AutoLayout,
    AutoLayoutDirection,
    ZoomSpeed,
    PanInertia,
    // AI. The provider has no field of its own — it's read off `ai_url` — but
    // it's a row, with its own focus and its own way back to Off.
    AiProvider,
    AiUrl,
    AiModel,
    AiApiKey,
    AiNodeColor,
    AiThinking,
    // Advanced
    RasterizeText,
    ClusterZoom,
    AnimationScale,
    ThemeCss,
    Canvas,
}

/// The form's tabs, by topic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Tab {
    #[default]
    General,
    Appearance,
    Canvas,
    Ai,
    Advanced,
}

impl Tab {
    /// In the order they're shown.
    pub const ALL: [Tab; 5] = [Tab::General, Tab::Appearance, Tab::Canvas, Tab::Ai, Tab::Advanced];

    pub fn label(self) -> &'static str {
        match self {
            Tab::General => "General",
            Tab::Appearance => "Appearance",
            Tab::Canvas => "Canvas",
            Tab::Ai => "AI",
            Tab::Advanced => "Advanced",
        }
    }
}

/// The settings only the file sets, by their keys in it. Each was a row once,
/// and none is something anyone changes twice: their defaults are tuned, and
/// any sane value works. Counted at the foot of Advanced, so the form says
/// they exist rather than leaving them to be found.
pub(crate) const FILE_ONLY: [&str; 16] = [
    "min_zoom",
    "max_zoom",
    "zoom_smoothing",
    "wheel_zoom_step",
    "pinch_zoom_step",
    "auto_zoom_trigger",
    "auto_zoom_level",
    "new_node_min_width",
    "new_node_min_height",
    "layout_rank_gap",
    "layout_node_gap",
    "autosave_debounce_ms",
    "max_ai_nodes",
    "ai_trigger_color",
    "link_opacity",
    "overlay_blur",
];

/// A settings-form edit. `Slide`/`Edit` change the value live; `Commit` (slider
/// release, text submit) is when it's persisted to disk.
#[derive(Debug, Clone)]
pub(crate) enum Msg {
    Toggle(Field, bool),
    Slide(Field, f32),
    Edit(Field, String),
    Pick(Field, String),
    Commit,
    Browse(Field),
    /// Paste the clipboard's text into `field` (the API-key "Paste" button).
    Paste(Field),
    /// Apply a provider preset by name, filling the endpoint URL + model.
    PickPreset(String),
    /// Choose the model from the endpoint's own list (the model row's button).
    ChooseModel,
    /// Choose the theme or the font in its own picker, over the form.
    ChooseTheme,
    ChooseFont,
    /// One of the overlay background's tiles.
    Backdrop(OverlayBackdrop),
    /// Show another tab.
    Tab(Tab),
    /// Focus a row: it shows what it does.
    Focus(Field),
    /// Put a setting back the way a fresh config has it.
    Reset(Field),
    /// The config file's path, to the clipboard.
    CopyConfigPath,
    /// Read the config file again, for edits made to it by hand.
    ReloadConfig,
}

impl Msg {
    /// The row an edit is about. Using a control focuses its row, which is
    /// what shows its description: a press on a control is the control's,
    /// and never reaches the row around it.
    fn field(&self) -> Option<Field> {
        match self {
            Msg::Toggle(field, _)
            | Msg::Slide(field, _)
            | Msg::Edit(field, _)
            | Msg::Pick(field, _)
            | Msg::Browse(field)
            | Msg::Paste(field)
            | Msg::Focus(field)
            | Msg::Reset(field) => Some(*field),
            Msg::PickPreset(_) => Some(Field::AiProvider),
            Msg::ChooseModel => Some(Field::AiModel),
            Msg::ChooseTheme => Some(Field::Theme),
            Msg::ChooseFont => Some(Field::Font),
            Msg::Backdrop(_) => Some(Field::OverlayBackdrop),
            Msg::Commit | Msg::Tab(_) | Msg::CopyConfigPath | Msg::ReloadConfig => None,
        }
    }
}

/// A picture tile's picture, once the form has asked for it.
#[derive(Clone, Debug)]
pub(crate) enum Preview {
    /// Being prepared.
    Pending,
    Ready(image::Handle),
    /// Couldn't be read.
    Missing,
}

/// What kind of control a setting is — which is what its palette entries do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// On or off: one entry, which flips it.
    Switch,
    /// One of a few: an entry per choice, `Setting › Choice`, applied as it's
    /// picked.
    Choice,
    /// Its own picker — the theme, the font, the model — opened by a command of
    /// its own, so not indexed again.
    Picker,
    /// Set in the form itself — a number, a path, a key: one entry, which
    /// opens the form at its row.
    Row,
}

/// One choice of a [`Kind::Choice`] setting, as the palette offers it.
pub(crate) struct Choice {
    pub label: String,
    /// What choosing it sends.
    pub msg: Msg,
    /// Whether it's the one in force.
    pub current: bool,
    /// Terms it's found by but doesn't show.
    pub keywords: &'static str,
}

/// The settings index: every row the form draws, tab by tab, in the order it
/// draws them. **The form takes its labels from here too** ([`Field::label`]),
/// and the palette searches it — one list, so the two can't drift apart, as
/// the palette's own "Toggle center node text" had from the form's "Center
/// plain text".
pub(crate) fn index() -> Vec<Field> {
    use Field::*;
    let mut fields = vec![SummonHotkey];
    if cfg!(target_os = "macos") {
        fields.push(SummonGesture);
    }
    fields.extend([Vault, DismissOnBackgroundClick, DoubleClickAddsNote]);
    if cfg!(target_os = "macos") {
        fields.push(PersistAcrossSpaces);
    }
    fields.extend([OverlayDim, Windowed, Theme, Font, OverlayBackdrop]);
    if cfg!(target_os = "macos") {
        fields.push(TrayIconMonochrome);
    }
    fields.extend([
        LinkStyle, ArrowTip, CenterNodeText, NodeScrollbars, ColoredCardOpacity, MaxNodeWidth,
        SnapToGrid, AutoLayout, AutoLayoutDirection, ZoomSpeed, PanInertia,
        AiProvider, AiUrl, AiModel, AiApiKey, AiNodeColor, AiThinking,
        RasterizeText, ClusterZoom, AnimationScale, ThemeCss, Canvas,
    ]);
    fields
}

impl Field {
    /// What the form's row says.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Field::SummonHotkey => "Summon hotkey",
            Field::SummonGesture => "Four-finger swipe down",
            Field::Vault => "Obsidian vault",
            Field::DismissOnBackgroundClick => "Click empty space to dismiss",
            Field::DoubleClickAddsNote => "Double-click to add a note",
            Field::PersistAcrossSpaces => "Stay up across Spaces",
            Field::OverlayDim => "Dim behind board",
            Field::Windowed => "Run as a normal window",
            Field::Theme => "Theme",
            Field::Font => "Font",
            Field::OverlayBackdrop => "Overlay background",
            Field::OverlayImage => "Overlay image",
            Field::TrayIconMonochrome => "Monochrome menu bar icon",
            Field::LinkStyle => "Link style",
            Field::ArrowTip => "Arrow tips",
            Field::CenterNodeText => "Center plain text",
            Field::NodeScrollbars => "Card scrollbars",
            Field::ColoredCardOpacity => "Colored card opacity",
            Field::MaxNodeWidth => "Card grows to",
            Field::SnapToGrid => "Snap to grid",
            Field::SnapGridSize => "Grid spacing",
            Field::AutoLayout => "Auto-layout when adding a linked card",
            Field::AutoLayoutDirection => "Layout direction",
            Field::ZoomSpeed => "Zoom speed",
            Field::PanInertia => "Pan inertia",
            Field::AiProvider => "Provider",
            Field::AiUrl => "API URL",
            Field::AiModel => "Model",
            Field::AiApiKey => "API key",
            Field::AiNodeColor => "Answer card color",
            Field::AiThinking => "Let the model think first",
            Field::RasterizeText => "Rasterize text",
            Field::ClusterZoom => "Cluster below zoom",
            Field::AnimationScale => "Animation speed",
            Field::ThemeCss => "Theme CSS",
            Field::Canvas => "Launch board",
        }
    }

    /// What the palette calls it: the label, where the label leans on the tab
    /// it sits in — "Provider" is plain under AI and nothing at all in a list
    /// of commands.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Field::AiProvider => "AI provider",
            Field::AiModel => "AI model",
            _ => self.label(),
        }
    }

    /// The tab its row is on.
    pub(crate) fn tab(self) -> Tab {
        use Field::*;
        match self {
            SummonHotkey | SummonGesture | Vault | DismissOnBackgroundClick | DoubleClickAddsNote
            | PersistAcrossSpaces | OverlayDim | Windowed => Tab::General,
            Theme | Font | OverlayBackdrop | OverlayImage | TrayIconMonochrome | LinkStyle | ArrowTip
            | CenterNodeText | NodeScrollbars | ColoredCardOpacity | MaxNodeWidth => Tab::Appearance,
            SnapToGrid | SnapGridSize | AutoLayout | AutoLayoutDirection | ZoomSpeed | PanInertia => Tab::Canvas,
            AiProvider | AiUrl | AiModel | AiApiKey | AiNodeColor | AiThinking => Tab::Ai,
            RasterizeText | ClusterZoom | AnimationScale | ThemeCss | Canvas => Tab::Advanced,
        }
    }

    pub(crate) fn kind(self) -> Kind {
        use Field::*;
        match self {
            SummonGesture | DismissOnBackgroundClick | DoubleClickAddsNote | PersistAcrossSpaces | Windowed
            | TrayIconMonochrome | CenterNodeText | NodeScrollbars | SnapToGrid | AutoLayout | AiThinking
            | RasterizeText => Kind::Switch,
            OverlayBackdrop | LinkStyle | ArrowTip | AutoLayoutDirection | AiProvider | AiNodeColor => Kind::Choice,
            Theme | Font | AiModel => Kind::Picker,
            SummonHotkey | Vault | OverlayDim | OverlayImage | ColoredCardOpacity | MaxNodeWidth | SnapGridSize
            | ZoomSpeed | PanInertia | AiUrl | AiApiKey | ClusterZoom | AnimationScale | ThemeCss | Canvas => Kind::Row,
        }
    }

    /// Terms it's found by in the palette without showing them — the words
    /// someone might reach for that aren't the label's.
    pub(crate) fn keywords(self) -> &'static str {
        match self {
            Field::SummonHotkey => "shortcut key keyboard",
            Field::SummonGesture => "macos trackpad swipe gesture summon",
            Field::Vault => "obsidian folder",
            Field::DismissOnBackgroundClick => "dismiss hide close click",
            Field::DoubleClickAddsNote => "create new card obsidian",
            Field::PersistAcrossSpaces => "macos desktop workspace persist",
            Field::OverlayDim => "darken dim opacity",
            Field::Windowed => "window mode overlay",
            Field::OverlayBackdrop => "transparent see-through wallpaper blur opaque background backdrop",
            Field::TrayIconMonochrome => "macos tray menu bar icon template",
            Field::LinkStyle => "links edges curve routing",
            Field::ArrowTip => "arrowhead arrow head link edge",
            Field::CenterNodeText => "align centre text",
            Field::NodeScrollbars => "scroll resize overflow",
            Field::ColoredCardOpacity => "colour color transparency",
            Field::MaxNodeWidth => "card width size",
            Field::SnapToGrid => "grid align",
            Field::AutoLayout => "layout arrange",
            Field::AutoLayoutDirection => "layout direction flow",
            Field::ZoomSpeed => "zoom wheel pinch sensitivity",
            Field::PanInertia => "pan scroll coast momentum",
            Field::AiProvider => "ai llm endpoint",
            Field::AiUrl => "ai endpoint url",
            Field::AiApiKey => "ai key token",
            Field::AiNodeColor => "ai answer colour color",
            Field::AiThinking => "ai reasoning",
            Field::RasterizeText => "bitmap performance gpu",
            Field::ClusterZoom => "cluster semantic zoom",
            Field::AnimationScale => "animation motion",
            Field::ThemeCss => "obsidian css stylesheet",
            Field::Canvas => "launch board startup open",
            _ => "",
        }
    }
}

impl App {
    /// A choice setting's choices, as the palette offers them: what each is
    /// called, what picking it sends, and which is in force — from the very
    /// lists the form draws its tiles and lists from.
    pub(super) fn choices(&self, field: Field) -> Vec<Choice> {
        let c = &self.config;
        let choice = |label: &str, msg: Msg, current: bool, keywords: &'static str| Choice {
            label: label.to_owned(),
            msg,
            current,
            keywords,
        };
        match field {
            Field::LinkStyle => LinkStyle::ALL
                .into_iter()
                .map(|style| {
                    let keywords = match style {
                        LinkStyle::Obsidian => "curve bezier",
                        LinkStyle::Mermaid => "routed flowchart",
                        LinkStyle::Square => "orthogonal right angle",
                    };
                    let pick = Msg::Pick(field, style.label().to_owned());
                    choice(style.label(), pick, LinkStyle::of(c) == style, keywords)
                })
                .collect(),
            Field::ArrowTip => ArrowTip::ALL
                .into_iter()
                .map(|tip| {
                    let keywords = match tip {
                        ArrowTip::Chevron => "open",
                        ArrowTip::Triangle => "filled",
                    };
                    choice(tip.label(), Msg::Pick(field, tip.label().to_owned()), ArrowTip::of(c) == tip, keywords)
                })
                .collect(),
            Field::OverlayBackdrop => OverlayBackdrop::offered()
                .into_iter()
                .map(|backdrop| choice(backdrop.label(), Msg::Backdrop(backdrop), OverlayBackdrop::of(c) == backdrop, ""))
                .collect(),
            Field::AutoLayoutDirection => [("LR", "Left to right", "horizontal"), ("TD", "Top down", "vertical")]
                .into_iter()
                .map(|(value, label, keywords)| {
                    choice(label, Msg::Pick(field, value.to_owned()), c.auto_layout_direction == value, keywords)
                })
                .collect(),
            Field::AiProvider => {
                let custom = self.settings_custom_ai && !c.ai_enabled();
                let current = if custom { "Custom".to_owned() } else { current_preset(&c.ai_url) };
                std::iter::once(PROVIDER_OFF)
                    .chain(crate::ai::PRESETS.iter().map(|preset| preset.name))
                    .chain(std::iter::once("Custom"))
                    .map(|name| choice(name, Msg::PickPreset(name.to_owned()), current == name, ""))
                    .collect()
            }
            Field::AiNodeColor => [
                ("0", "None"),
                ("1", "Red"),
                ("2", "Orange"),
                ("3", "Yellow"),
                ("4", "Green"),
                ("5", "Cyan"),
                ("6", "Purple"),
            ]
            .into_iter()
            .map(|(code, label)| choice(label, Msg::Pick(field, code.to_owned()), c.ai_node_color == code, ""))
            .collect(),
            _ => Vec::new(),
        }
    }
}

/// A normalized value handed to [`apply`] — the form's editors are lowered to one
/// of these before touching the config.
pub(crate) enum Value {
    Bool(bool),
    Num(f32),
    Text(String),
}

impl Field {
    /// Whether the config holds something a fresh one doesn't — the dot beside
    /// a row. What the app fills in by itself isn't a change: the open board,
    /// a vault's own theme, the model and key that come with a provider.
    pub(crate) fn modified(self, c: &Config) -> bool {
        let d = Config::default();
        let moved = |a: f32, b: f32| (a - b).abs() > 0.001;
        match self {
            Field::SummonHotkey => c.summon_hotkey.trim() != d.summon_hotkey,
            Field::SummonGesture => c.summon_gesture != d.summon_gesture,
            Field::Vault => !c.vault.trim().is_empty(),
            Field::DismissOnBackgroundClick => c.dismiss_on_background_click != d.dismiss_on_background_click,
            Field::DoubleClickAddsNote => c.double_click_adds_note != d.double_click_adds_note,
            Field::PersistAcrossSpaces => c.persist_across_spaces != d.persist_across_spaces,
            Field::OverlayDim => moved(c.overlay_dim, d.overlay_dim),
            Field::Windowed => c.windowed != d.windowed,
            Field::Theme => !c.theme.trim().is_empty(),
            Field::Font => !c.font.trim().is_empty(),
            Field::OverlayBackdrop => OverlayBackdrop::of(c) != OverlayBackdrop::of(&d),
            Field::TrayIconMonochrome => c.tray_icon_monochrome != d.tray_icon_monochrome,
            Field::LinkStyle => LinkStyle::of(c) != LinkStyle::of(&d),
            Field::ArrowTip => ArrowTip::of(c) != ArrowTip::of(&d),
            Field::CenterNodeText => c.center_node_text != d.center_node_text,
            Field::NodeScrollbars => c.node_scrollbars != d.node_scrollbars,
            Field::ColoredCardOpacity => moved(c.colored_card_opacity, d.colored_card_opacity),
            Field::MaxNodeWidth => moved(c.max_node_width, d.max_node_width),
            Field::SnapToGrid => c.snap_to_grid != d.snap_to_grid,
            Field::SnapGridSize => moved(c.snap_grid_size, d.snap_grid_size),
            Field::AutoLayout => c.auto_layout != d.auto_layout,
            Field::AutoLayoutDirection => c.auto_layout_direction != d.auto_layout_direction,
            Field::ZoomSpeed => moved(c.zoom_speed, d.zoom_speed),
            Field::PanInertia => moved(c.pan_inertia, d.pan_inertia),
            Field::AiProvider => c.ai_enabled(),
            Field::AiNodeColor => c.ai_node_color != d.ai_node_color,
            Field::AiThinking => c.ai_thinking != d.ai_thinking,
            Field::RasterizeText => c.rasterize_text != d.rasterize_text,
            Field::ClusterZoom => moved(c.cluster_zoom, d.cluster_zoom),
            Field::AnimationScale => moved(c.animation_scale, d.animation_scale),
            Field::ThemeCss => hand_picked_css(c),
            Field::OverlayImage | Field::AiUrl | Field::AiModel | Field::AiApiKey | Field::Canvas => false,
        }
    }

    /// What the link that puts a changed setting back says. `None` where the
    /// row already shows every choice, so picking the default *is* the way back
    /// — a switch, the tiles, the swatches.
    pub(crate) fn reset_label(self) -> Option<&'static str> {
        match self {
            Field::Vault => Some("Clear"),
            Field::AiProvider => Some("Turn off"),
            Field::Theme
            | Field::Font
            | Field::ThemeCss
            | Field::OverlayDim
            | Field::ColoredCardOpacity
            | Field::MaxNodeWidth
            | Field::ZoomSpeed
            | Field::PanInertia
            | Field::ClusterZoom
            | Field::AnimationScale => Some("Reset"),
            _ => None,
        }
    }
}

/// Whether `theme_css` is a stylesheet someone chose, rather than the one the
/// vault's Obsidian theme put there — which is the app's doing, not theirs.
pub(crate) fn hand_picked_css(c: &Config) -> bool {
    c.theme_css
        .as_deref()
        .filter(|css| !css.trim().is_empty())
        .is_some_and(|css| !thinkboard_core::vault::is_vault_theme(Path::new(css), Path::new(c.vault.trim())))
}

/// Whether a field is edited as a number (so text input is parsed as `f32`).
pub(crate) fn is_number(field: Field) -> bool {
    matches!(field, Field::MaxNodeWidth | Field::SnapGridSize | Field::ClusterZoom)
}

/// The config's value for a text or number field, as its editor shows it.
pub(crate) fn config_text(config: &Config, field: Field) -> String {
    match field {
        Field::AiUrl => config.ai_url.clone(),
        Field::AiModel => config.ai_model.clone(),
        Field::AiApiKey => config.ai_key().to_owned(),
        Field::MaxNodeWidth => config.max_node_width.to_string(),
        Field::SnapGridSize => config.snap_grid_size.to_string(),
        Field::ClusterZoom => config.cluster_zoom.to_string(),
        _ => String::new(),
    }
}

/// Writes a normalized [`Value`] into the named config field. Out-of-range
/// numbers are left to `Config::sanitize` (called by the caller).
pub(crate) fn apply(config: &mut Config, field: Field, value: Value) {
    match (field, value) {
        (Field::Canvas, Value::Text(s)) => config.canvas = s,
        (Field::Vault, Value::Text(s)) => config.vault = s,
        (Field::Theme, Value::Text(s)) => config.theme = s,
        (Field::Font, Value::Text(s)) => config.font = s,
        (Field::SummonHotkey, Value::Text(s)) => config.summon_hotkey = s,
        (Field::ThemeCss, Value::Text(s)) => {
            config.theme_css = if s.trim().is_empty() { None } else { Some(s) };
        }
        (Field::AiUrl, Value::Text(s)) => config.ai_url = s,
        (Field::AiModel, Value::Text(s)) => config.ai_model = s,
        (Field::AiApiKey, Value::Text(s)) => config.set_ai_key(&s),
        (Field::AutoLayoutDirection, Value::Text(s)) => config.auto_layout_direction = s,
        (Field::AiNodeColor, Value::Text(s)) => config.ai_node_color = s,

        (Field::DismissOnBackgroundClick, Value::Bool(b)) => config.dismiss_on_background_click = b,
        (Field::DoubleClickAddsNote, Value::Bool(b)) => config.double_click_adds_note = b,
        (Field::PersistAcrossSpaces, Value::Bool(b)) => config.persist_across_spaces = b,
        (Field::SummonGesture, Value::Bool(b)) => config.summon_gesture = b,
        (Field::OverlayImage, Value::Text(s)) => config.overlay_image = s.trim().to_owned(),
        (Field::OverlayBackdrop, Value::Text(label)) => {
            if let Some(backdrop) = OverlayBackdrop::offered().into_iter().find(|b| b.label() == label) {
                backdrop.set(config);
            }
        }
        (Field::Windowed, Value::Bool(b)) => config.windowed = b,
        (Field::TrayIconMonochrome, Value::Bool(b)) => config.tray_icon_monochrome = b,
        (Field::CenterNodeText, Value::Bool(b)) => config.center_node_text = b,
        (Field::RasterizeText, Value::Bool(b)) => config.rasterize_text = b,
        (Field::NodeScrollbars, Value::Bool(b)) => config.node_scrollbars = b,
        // Stored by name, and matched back from the label the form shows.
        (Field::LinkStyle, Value::Text(label)) => {
            config.link_style = LinkStyle::ALL
                .into_iter()
                .find(|style| style.label() == label)
                .unwrap_or_default()
                .as_str()
                .to_owned();
        }
        (Field::ArrowTip, Value::Text(label)) => {
            config.arrow_tip = ArrowTip::ALL
                .into_iter()
                .find(|tip| tip.label() == label)
                .unwrap_or_default()
                .as_str()
                .to_owned();
        }
        (Field::SnapToGrid, Value::Bool(b)) => config.snap_to_grid = b,
        (Field::AutoLayout, Value::Bool(b)) => config.auto_layout = b,
        (Field::AiThinking, Value::Bool(b)) => config.ai_thinking = b,

        (Field::OverlayDim, Value::Num(v)) => config.overlay_dim = v,
        (Field::ColoredCardOpacity, Value::Num(v)) => config.colored_card_opacity = v,
        (Field::MaxNodeWidth, Value::Num(v)) => config.max_node_width = v,
        (Field::SnapGridSize, Value::Num(v)) => config.snap_grid_size = v,
        (Field::ZoomSpeed, Value::Num(v)) => config.zoom_speed = v,
        (Field::PanInertia, Value::Num(v)) => config.pan_inertia = v,
        (Field::ClusterZoom, Value::Num(v)) => config.cluster_zoom = v,
        (Field::AnimationScale, Value::Num(v)) => config.animation_scale = v,

        // Type/field mismatches (shouldn't happen — editors and fields agree).
        _ => {}
    }
}

/// Whether the configured endpoint can list its models yet: a key is stored
/// for it, or it's on this machine, where a runtime asks for none. A hosted
/// provider picked before its key is pasted would only answer the list with
/// a refusal.
fn ready_to_list(config: &Config) -> bool {
    !config.ai_key().is_empty() || is_local(&config.ai_url)
}

/// Whether `url` points at this machine.
fn is_local(url: &str) -> bool {
    let rest = url.trim().split_once("://").map_or(url.trim(), |(_, rest)| rest);
    let host = rest.split(['/', ':']).next().unwrap_or_default();
    matches!(host, "localhost" | "127.0.0.1" | "0.0.0.0") || rest.starts_with("[::1]")
}

/// The "no endpoint" provider — AI disabled. Represented by an empty `ai_url`.
pub(super) const PROVIDER_OFF: &str = "Off";

/// Whether the AI endpoint is one of the user's own: set, and no preset's.
pub(crate) fn is_custom(config: &Config) -> bool {
    config.ai_enabled() && current_preset(&config.ai_url) == "Custom"
}

/// The preset name for `url`: "Off" when empty, else the matching provider
/// (ignoring a trailing slash), else "Custom".
fn current_preset(url: &str) -> String {
    let url = url.trim().trim_end_matches('/');
    if url.is_empty() {
        return PROVIDER_OFF.to_owned();
    }
    crate::ai::PRESETS
        .iter()
        .find(|p| p.url.trim_end_matches('/') == url)
        .map_or_else(|| "Custom".to_owned(), |p| p.name.to_owned())
}

impl App {
    /// The raw text of a settings text/number field (seeded from the config when
    /// the form opens; reflects exactly what the user has typed since).
    pub(super) fn draft(&self, field: Field) -> &str {
        self.settings_drafts
            .get(&field)
            .map(String::as_str)
            .unwrap_or("")
    }

    /// Seeds `settings_drafts` from the current config for every text-editable
    /// field, so the form opens showing the live values.
    pub(super) fn populate_settings_drafts(&mut self) {
        use Field::*;
        self.settings_drafts.clear();
        for field in [AiUrl, AiModel, AiApiKey, MaxNodeWidth, SnapGridSize, ClusterZoom] {
            self.settings_drafts.insert(field, config_text(&self.config, field));
        }
    }

    pub(super) fn persist_config(&self) {
        self.config.save(super::config_path_arg().as_deref());
    }

    /// Applies one settings edit: mutates the config, clamps it, persists (except
    /// mid-drag/mid-type, which persist on `Commit`), and refreshes anything the
    /// change affects live.
    pub(super) fn apply_setting(&mut self, msg: Msg) -> Task<Message> {
        if let Some(field) = msg.field() {
            self.settings_focus = Some(field);
        }
        match msg {
            Msg::Toggle(field, b) => {
                apply(&mut self.config, field, Value::Bool(b));
                self.config.sanitize();
                self.persist_config();
                self.after_setting_change(field)
            }
            // The background's list (Advanced) chooses as its tiles do.
            Msg::Pick(Field::OverlayBackdrop, label) => {
                match OverlayBackdrop::offered().into_iter().find(|backdrop| backdrop.label() == label) {
                    Some(backdrop) => self.apply_setting(Msg::Backdrop(backdrop)),
                    None => Task::none(),
                }
            }
            Msg::Pick(field, choice) => {
                apply(&mut self.config, field, Value::Text(choice));
                self.config.sanitize();
                self.persist_config();
                self.after_setting_change(field)
            }
            Msg::Slide(field, v) => {
                apply(&mut self.config, field, Value::Num(v));
                self.config.sanitize();
                self.after_setting_change(field)
            }
            Msg::Edit(field, s) => {
                if is_number(field) {
                    if let Ok(v) = s.trim().parse::<f32>() {
                        apply(&mut self.config, field, Value::Num(v));
                        self.config.sanitize();
                    }
                } else {
                    apply(&mut self.config, field, Value::Text(s.clone()));
                    self.config.sanitize();
                }
                self.settings_drafts.insert(field, s);
                self.after_setting_change(field)
            }
            Msg::Commit => {
                self.persist_config();
                Task::none()
            }
            Msg::Browse(field) => self.browse_for(field),
            Msg::Paste(field) => {
                if let Some(text) = super::clipboard::read_text() {
                    let text = text.trim().to_owned();
                    apply(&mut self.config, field, Value::Text(text.clone()));
                    self.config.sanitize();
                    self.settings_drafts.insert(field, text);
                    self.persist_config();
                    let changed = self.after_setting_change(field);
                    // A hosted provider's key is what its model list waited on.
                    if field == Field::AiApiKey && self.config.ai_enabled() && ready_to_list(&self.config) {
                        return Task::batch([changed, self.open_model_picker_over_settings()]);
                    }
                    return changed;
                }
                Task::none()
            }
            Msg::PickPreset(name) => {
                // "Off" clears the endpoint (AI disabled); a known provider fills
                // it; "Custom" keeps whatever the user typed — and, with nothing
                // typed yet, has to be remembered here, or the form would read
                // "Off" and hide the URL field it was picked to reveal.
                self.settings_custom_ai = name == "Custom";
                if name == PROVIDER_OFF {
                    self.config.ai_url = String::new();
                } else if let Some(preset) = crate::ai::PRESETS.iter().find(|p| p.name == name) {
                    self.config.ai_url = preset.url.to_owned();
                } else {
                    return Task::none();
                }
                self.config.sanitize();
                self.persist_config();
                // Reflect the change in the URL field, and swap the key field
                // to the one belonging to the provider now selected.
                self.settings_drafts
                    .insert(Field::AiUrl, self.config.ai_url.clone());
                self.settings_drafts
                    .insert(Field::AiApiKey, self.config.ai_key().to_owned());
                // The model next, while the provider is fresh in mind — a new
                // config's names a local runtime's, which the next one is
                // unlikely to serve. Not for a hosted one with no key yet: its
                // list would only be a refusal, so the key's paste opens it.
                if self.config.ai_enabled() && ready_to_list(&self.config) {
                    return self.open_model_picker_over_settings();
                }
                Task::none()
            }
            Msg::ChooseModel => self.open_model_picker_over_settings(),
            Msg::ChooseTheme => self.open_theme_picker_over_settings(),
            Msg::ChooseFont => self.open_font_picker_over_settings(),
            Msg::Backdrop(backdrop) => {
                // The image tile is also where the picture is chosen: the first
                // time it's picked, or when it's picked again to change it. And
                // then the picture is the choice — it's taken when one is
                // (`take_browsed`). Taken first, a cancelled dialog left the
                // board on the theme's colour, which is what an image with no
                // picture shows, rather than on what it had before.
                let again = OverlayBackdrop::of(&self.config) == backdrop;
                if backdrop == OverlayBackdrop::Image && (again || self.config.overlay_image.trim().is_empty()) {
                    return self.browse_for(Field::OverlayImage);
                }
                backdrop.set(&mut self.config);
                self.persist_config();
                self.after_setting_change(Field::OverlayBackdrop)
            }
            Msg::Tab(tab) => {
                // What was typed on the last tab is kept, as leaving the form
                // would keep it.
                self.persist_config();
                self.settings_tab = tab;
                self.settings_focus = None;
                Task::none()
            }
            Msg::Focus(_) => Task::none(),
            Msg::Reset(field) => self.reset_setting(field),
            Msg::CopyConfigPath => {
                if let Some(path) = crate::config::path(super::config_path_arg().as_deref()) {
                    super::clipboard::write_text(path.to_string_lossy().into_owned());
                }
                Task::none()
            }
            Msg::ReloadConfig => self.reload_config(),
        }
    }

    /// Puts `field` back the way a fresh config has it.
    fn reset_setting(&mut self, field: Field) -> Task<Message> {
        let fresh = Config::default();
        let c = &mut self.config;
        match field {
            Field::Vault => c.vault.clear(),
            Field::Theme => c.theme.clear(),
            Field::Font => c.font.clear(),
            // Back to what the vault's Obsidian theme says, if there's one —
            // which is what an unpicked stylesheet would have been.
            Field::ThemeCss => {
                c.theme_css = None;
                c.adopt_vault_theme();
            }
            Field::AiProvider => {
                self.settings_custom_ai = false;
                c.ai_url.clear();
            }
            Field::OverlayDim => c.overlay_dim = fresh.overlay_dim,
            Field::ColoredCardOpacity => c.colored_card_opacity = fresh.colored_card_opacity,
            Field::MaxNodeWidth => c.max_node_width = fresh.max_node_width,
            Field::ZoomSpeed => c.zoom_speed = fresh.zoom_speed,
            Field::PanInertia => c.pan_inertia = fresh.pan_inertia,
            Field::ClusterZoom => c.cluster_zoom = fresh.cluster_zoom,
            Field::AnimationScale => c.animation_scale = fresh.animation_scale,
            _ => return Task::none(),
        }
        self.config.sanitize();
        self.persist_config();
        self.populate_settings_drafts();
        // The provider is the URL, as far as anything downstream knows.
        let field = if field == Field::AiProvider { Field::AiUrl } else { field };
        self.after_setting_change(field)
    }

    /// Reads the config file again and applies what it says, for the settings
    /// [`FILE_ONLY`] leaves to it. The file is read at launch and written whole,
    /// so an edit made by hand while the app runs was otherwise lost to the next
    /// change made here.
    ///
    /// The open board stays open whatever the file names — Reload is for
    /// settings, not for switching boards.
    pub(super) fn reload_config(&mut self) -> Task<Message> {
        let mut fresh = Config::load(super::config_path_arg().as_deref());
        fresh.canvas = self.config.canvas.clone();
        // As at launch: the vault's Obsidian theme is resolved, not read.
        fresh.adopt_vault_theme();
        let before = std::mem::replace(&mut self.config, fresh);
        self.populate_settings_drafts();
        if before.summon_hotkey != self.config.summon_hotkey {
            self.reregister_hotkey();
        }
        if crate::platform::SUPPORTS_GESTURES {
            crate::platform::set_summon_gesture_enabled(self.overlay && self.config.summon_gesture);
        }
        super::tray::set_monochrome(self.config.tray_icon_monochrome);
        if self.overlay {
            self.apply_backdrop();
        }
        let mut tasks = Vec::new();
        // The tiles were prepared at the old blur; Reload is on the form, so
        // they're on screen.
        if before.overlay_blur != self.config.overlay_blur {
            self.backdrop_previews.clear();
            tasks.push(self.preview_backdrops());
        }
        self.cache.clear();
        let c = &self.config;
        let looks = before.theme != c.theme || before.theme_css != c.theme_css || before.font != c.font;
        let text = before.rasterize_text != c.rasterize_text || before.center_node_text != c.center_node_text;
        if looks {
            tasks.push(self.apply_theme_css());
        }
        if text {
            tasks.push(self.refresh_snapshots());
            tasks.push(self.schedule_recluster());
        }
        Task::batch(tasks)
    }

    /// Live side effects of a settings change (over and above the config write).
    fn after_setting_change(&mut self, field: Field) -> Task<Message> {
        // Where the overlay can change in place, it does; elsewhere the row
        // says it waits for a restart.
        if matches!(field, Field::OverlayBackdrop | Field::OverlayImage) && self.overlay {
            self.apply_backdrop();
        }
        // A new picture of your own, for its tile.
        let previews = if field == Field::OverlayImage { self.preview_backdrops() } else { Task::none() };
        self.cache.clear();
        // Keys are per endpoint, so changing the endpoint changes which one the
        // field is showing.
        if field == Field::AiUrl {
            self.settings_drafts
                .insert(Field::AiApiKey, self.config.ai_key().to_owned());
        }
        let effect = match field {
            // These change how node *and* cluster text is drawn — re-rasterize
            // both. Cluster snapshots aren't produced while rasterize is off, so
            // turning it on must regenerate them or clusters render empty.
            Field::RasterizeText | Field::CenterNodeText => {
                Task::batch([self.refresh_snapshots(), self.schedule_recluster()])
            }
            // A theme now swaps in place rather than waiting for a restart.
            // Naming a theme, clearing the name to fall back to the
            // stylesheet, pointing at a different stylesheet, choosing a font:
            // all of them re-resolve, and all of them do it without a restart.
            Field::Theme | Field::ThemeCss | Field::Font => self.apply_theme_css(),
            // Pointing at a different vault means a different Obsidian theme.
            Field::Vault => self.adopt_vault_theme(),
            // Arm or disarm the trackpad tap immediately (we're on the main
            // thread here). The subscription registers the handler separately.
            Field::SummonGesture => {
                if crate::platform::SUPPORTS_GESTURES {
                    let armed = self.overlay && self.config.summon_gesture;
                    crate::platform::set_summon_gesture_enabled(armed);
                }
                Task::none()
            }
            // Retint the live menu-bar icon (macOS template mode); a no-op
            // elsewhere and before the tray exists. We're on the main thread,
            // which the tray requires.
            Field::TrayIconMonochrome => {
                super::tray::set_monochrome(self.config.tray_icon_monochrome);
                Task::none()
            }
            _ => Task::none(),
        };
        Task::batch([previews, effect])
    }

    /// Asks for a path for `field` with a file dialog — clearing the overlay
    /// out of its way first, when it's up.
    ///
    /// **The overlay floats above every window, a file dialog included.**
    /// Opened from the in-board form, the dialog came up behind the board,
    /// where nothing could reach it. So, as the open-board dialog does, the
    /// overlay steps aside, the dialog opens once it's gone, and the answer —
    /// a path or a cancel — brings it back (`settings_browsed`), the form
    /// still open on it. Only if it was this that hid it: from the settings
    /// window, the overlay is usually away already, and must stay so.
    fn browse_for(&mut self, field: Field) -> Task<Message> {
        if self.overlay && !matches!(self.phase, super::Phase::Hidden) {
            self.settings_dialog_hid_overlay = true;
            return self.hide_window().chain(Task::done(Message::SettingsBrowseDialog(field)));
        }
        self.browse_dialog(field)
    }

    /// Opens a file dialog for a path setting. macOS/Windows use the blocking
    /// dialog on the (main) update thread; Linux runs the async portal dialog on
    /// iced's runtime (its zbus backend needs one — iced is built with tokio).
    pub(super) fn browse_dialog(&mut self, field: Field) -> Task<Message> {
        // The vault is a folder; every other browsable setting names a file.
        let folder = field == Field::Vault;
        #[cfg(not(target_os = "linux"))]
        {
            let dialog = rfd::FileDialog::new();
            let picked = if folder { dialog.pick_folder() } else { dialog.pick_file() };
            Task::done(Message::SettingsBrowsed(field, picked))
        }
        #[cfg(target_os = "linux")]
        {
            Task::perform(
                async move {
                    let dialog = rfd::AsyncFileDialog::new();
                    let handle = if folder {
                        dialog.pick_folder().await
                    } else {
                        dialog.pick_file().await
                    };
                    handle.map(|handle| handle.path().to_path_buf())
                },
                move |picked| Message::SettingsBrowsed(field, picked),
            )
        }
    }

    /// The current value of a boolean setting, so the palette can flip it.
    pub(super) fn setting_bool(&self, field: Field) -> bool {
        let c = &self.config;
        match field {
            Field::DismissOnBackgroundClick => c.dismiss_on_background_click,
            Field::DoubleClickAddsNote => c.double_click_adds_note,
            Field::PersistAcrossSpaces => c.persist_across_spaces,
            Field::SummonGesture => c.summon_gesture,
            Field::CenterNodeText => c.center_node_text,
            Field::TrayIconMonochrome => c.tray_icon_monochrome,
            Field::RasterizeText => c.rasterize_text,
            Field::NodeScrollbars => c.node_scrollbars,
            Field::AiThinking => c.ai_thinking,
            Field::Windowed => c.windowed,
            Field::SnapToGrid => c.snap_to_grid,
            Field::AutoLayout => c.auto_layout,
            _ => false,
        }
    }

    /// Applies a resolved "Browse…" dialog result to its field, and brings
    /// back the overlay if it was hidden for the dialog (see `browse_for`).
    pub(super) fn settings_browsed(&mut self, field: Field, path: Option<std::path::PathBuf>) -> Task<Message> {
        let task = self.take_browsed(field, path);
        if std::mem::take(&mut self.settings_dialog_hid_overlay) {
            return self.resummon_after_dialog(task);
        }
        task
    }

    fn take_browsed(&mut self, field: Field, path: Option<std::path::PathBuf>) -> Task<Message> {
        let Some(path) = path else {
            return Task::none();
        };
        // The launch board is the open one — every way of opening a board
        // rewrites it — so choosing one opens it, rather than leaving the
        // board on screen and the row disagreeing until the next launch.
        if field == Field::Canvas {
            return self.open_canvas(path);
        }
        let text = path.to_string_lossy().into_owned();
        apply(&mut self.config, field, Value::Text(text));
        // A picture chosen is the image background chosen — see `Msg::Backdrop`.
        if field == Field::OverlayImage {
            OverlayBackdrop::Image.set(&mut self.config);
        }
        self.config.sanitize();
        self.persist_config();
        // As a typed path is, once committed: a stylesheet chosen here used
        // to wait for a restart to be applied.
        self.after_setting_change(field)
    }

    /// The in-board form on `field`'s tab, its row focused and scrolled into
    /// view — a setting found in the palette that's set in the form. Rows are
    /// a known order but not a known height, so the scroll is the row's place
    /// in its tab as a fraction: near enough to bring it on screen.
    pub(super) fn open_settings_at(&mut self, field: Field) -> Task<Message> {
        use iced::advanced::widget::operation::scrollable::{RelativeOffset, snap_to};
        self.search = None;
        self.command_palette = None;
        self.settings_overlay = true;
        self.settings_tab = field.tab();
        let opened = self.open_settings_form();
        self.settings_focus = Some(field);
        self.cache.clear();
        let rows: Vec<Field> = index().into_iter().filter(|row| row.tab() == field.tab()).collect();
        let at = rows.iter().position(|row| *row == field).unwrap_or(0);
        let fraction = if rows.len() > 1 { at as f32 / (rows.len() - 1) as f32 } else { 0.0 };
        let scroll = iced::advanced::widget::operate(snap_to(
            form::SCROLL.clone(),
            RelativeOffset { x: None, y: Some(fraction) },
        ));
        Task::batch([opened, scroll])
    }

    /// Opens the form: drafts from the config, nothing focused, on the tab it
    /// was left on — the old form's Advanced forgot it had been opened.
    pub(super) fn open_settings_form(&mut self) -> Task<Message> {
        self.populate_settings_drafts();
        self.settings_focus = None;
        self.preview_backdrops()
    }

    /// Starts the overlay background tiles' pictures, each once per launch:
    /// the wallpaper, and the image of your own if there's one. They're
    /// prepared as the overlay would show them, so a tile is a preview, and a
    /// picture chosen from it is ready by the time it's shown.
    pub(super) fn preview_backdrops(&mut self) -> Task<Message> {
        if !crate::platform::SUPPORTS_BACKDROP_PICTURE {
            return Task::none();
        }
        let mut wanted = vec![BackdropPicture::Wallpaper];
        let image = PathBuf::from(self.config.overlay_image.trim());
        if image.is_file() {
            wanted.push(BackdropPicture::File(image));
        }
        let blur = self.config.overlay_blur;
        let mut tasks = Vec::new();
        for picture in wanted {
            if self.backdrop_previews.iter().any(|(asked, _)| *asked == picture) {
                continue;
            }
            self.backdrop_previews.push((picture.clone(), Preview::Pending));
            let (sender, receiver) = iced::futures::channel::oneshot::channel();
            crate::platform::preview_backdrop_picture(picture.clone(), blur, move |png| {
                let _ = sender.send(png);
            });
            tasks.push(Task::perform(receiver, move |png| Message::BackdropPreview(picture, blur, png.ok().flatten())));
        }
        Task::batch(tasks)
    }

    /// A tile's picture, ready — at the blur it was asked for at, or it's
    /// dropped: a Reload that changed the blur asked again, and the tile waits
    /// for that one instead.
    pub(super) fn backdrop_previewed(&mut self, picture: BackdropPicture, blur: f32, png: Option<Vec<u8>>) {
        if blur != self.config.overlay_blur {
            return;
        }
        if let Some((_, preview)) = self.backdrop_previews.iter_mut().find(|(asked, _)| *asked == picture) {
            // Made once and kept: a handle made from bytes is a new image to
            // the renderer each time, uploaded again on every frame.
            *preview = png.map_or(Preview::Missing, |png| Preview::Ready(image::Handle::from_bytes(png)));
        }
        self.cache.clear();
    }

    /// What a picture tile shows, when it's ready.
    pub(super) fn backdrop_preview(&self, picture: &BackdropPicture) -> Option<&image::Handle> {
        self.backdrop_previews.iter().find(|(asked, _)| asked == picture).and_then(|(_, preview)| match preview {
            Preview::Ready(handle) => Some(handle),
            Preview::Pending | Preview::Missing => None,
        })
    }

    /// Closes the settings window's state (its drafts and focus), persisting
    /// whatever was typed.
    pub(super) fn close_settings_window(&mut self, persist: bool) -> Task<Message> {
        if persist {
            self.persist_config();
        }
        self.settings_window = None;
        self.settings_drafts.clear();
        self.settings_focus = None;
        self.close_settings_pickers()
    }

    /// Closes a picker opened over the settings form, with the form: left
    /// open, it would hold the board modal behind a panel nobody can see. The
    /// two that preview are cancelled, as every other dismissal of them is.
    pub(super) fn close_settings_pickers(&mut self) -> Task<Message> {
        if self.model_picker.as_ref().is_some_and(|picker| picker.over_settings) {
            self.model_picker = None;
        }
        let mut tasks = Vec::new();
        if self.theme_picker.as_ref().is_some_and(|picker| picker.over_settings) {
            tasks.push(self.cancel_theme_picker());
        }
        if self.font_picker.as_ref().is_some_and(|picker| picker.over_settings) {
            tasks.push(self.cancel_font_picker());
        }
        Task::batch(tasks)
    }

    /// Whichever picker the form opened, drawn over it — in the board's modal
    /// or in the settings window, wherever the form is.
    pub(super) fn settings_picker(&self) -> Option<iced::Element<'_, Message>> {
        if let Some(picker) = self.model_picker.as_ref().filter(|picker| picker.over_settings) {
            return Some(self.model_picker_view(picker));
        }
        if let Some(picker) = self.theme_picker.as_ref().filter(|picker| picker.over_settings) {
            return Some(self.theme_picker_view(picker));
        }
        if let Some(picker) = self.font_picker.as_ref().filter(|picker| picker.over_settings) {
            return Some(self.font_picker_view(picker));
        }
        None
    }

    /// Whether a picker is open: its Esc and Tab are its own, not the form's.
    pub(super) fn picker_open(&self) -> bool {
        self.model_picker.is_some() || self.theme_picker.is_some() || self.font_picker.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn apply_writes_each_value_kind() {
        let mut c = Config::default();

        apply(&mut c, Field::OverlayBackdrop, Value::Text("Theme color".to_owned()));
        assert_eq!(OverlayBackdrop::of(&c), OverlayBackdrop::Theme);
        assert!(!c.overlay_transparent, "kept in step, for an older build");

        apply(&mut c, Field::OverlayDim, Value::Num(0.25));
        assert_eq!(c.overlay_dim, 0.25);

        apply(&mut c, Field::AiModel, Value::Text("gpt-x".to_owned()));
        assert_eq!(c.ai_model, "gpt-x");

        apply(&mut c, Field::ZoomSpeed, Value::Num(1.5));
        assert_eq!(c.zoom_speed, 1.5);

        apply(&mut c, Field::SnapToGrid, Value::Bool(true));
        assert!(c.snap_to_grid);

        // theme_css: a blank string clears it to None.
        apply(&mut c, Field::ThemeCss, Value::Text("  ".to_owned()));
        assert_eq!(c.theme_css, None);
    }

    #[test]
    fn number_fields_round_trip_through_text() {
        // config_text formats the value; parsing that text back and applying it
        // must restore the same value (the drafts-backed number editor path).
        let c = Config::default();
        for field in [Field::MaxNodeWidth, Field::SnapGridSize, Field::ClusterZoom] {
            assert!(is_number(field));
            let text = config_text(&c, field);
            let value: f32 = text.parse().expect("numeric field formats as a number");
            let mut edited = Config::default();
            apply(&mut edited, field, Value::Num(value));
            assert_eq!(config_text(&edited, field), text, "round-trip for {field:?}");
        }
        assert!(!is_number(Field::AiModel));
    }

    #[test]
    fn current_preset_matches_known_urls_else_custom() {
        // An empty URL is the "Off" provider (AI disabled).
        assert_eq!(current_preset(""), PROVIDER_OFF);
        assert_eq!(current_preset("   "), PROVIDER_OFF);
        assert_eq!(current_preset("http://localhost:1234/v1"), "LM Studio");
        assert_eq!(current_preset("https://openrouter.ai/api/v1"), "OpenRouter");
        // A trailing slash still matches.
        assert_eq!(current_preset("https://openrouter.ai/api/v1/"), "OpenRouter");
        // Anything else is Custom.
        assert_eq!(current_preset("https://api.example.com/v1"), "Custom");
    }

    #[test]
    fn a_fresh_config_has_nothing_changed() {
        // Every row starts without its dot, or the form would open claiming
        // changes nobody made — the platform-dependent default included.
        let fresh = Config::default();
        for field in [
            Field::SummonHotkey, Field::SummonGesture, Field::Vault, Field::DismissOnBackgroundClick,
            Field::DoubleClickAddsNote, Field::PersistAcrossSpaces, Field::OverlayDim, Field::Windowed,
            Field::Theme, Field::Font, Field::OverlayBackdrop, Field::TrayIconMonochrome, Field::LinkStyle,
            Field::ArrowTip,
            Field::CenterNodeText, Field::NodeScrollbars, Field::ColoredCardOpacity, Field::MaxNodeWidth,
            Field::SnapToGrid, Field::SnapGridSize, Field::AutoLayout, Field::AutoLayoutDirection,
            Field::ZoomSpeed, Field::PanInertia, Field::AiProvider, Field::AiNodeColor, Field::AiThinking,
            Field::RasterizeText, Field::ClusterZoom, Field::AnimationScale, Field::ThemeCss,
        ] {
            assert!(!field.modified(&fresh), "{field:?} reads as changed on a fresh config");
        }
    }

    #[test]
    fn a_vaults_own_stylesheet_is_not_a_change() {
        let mut c = Config { vault: "/notes".to_owned(), ..Config::default() };
        c.theme_css = Some("/notes/.obsidian/themes/Minimal/theme.css".to_owned());
        assert!(!Field::ThemeCss.modified(&c), "the app put it there");
        c.theme_css = Some("/elsewhere/theme.css".to_owned());
        assert!(Field::ThemeCss.modified(&c), "chosen by hand");
    }

    #[test]
    fn every_file_only_key_is_in_the_file() {
        // The count at the foot of Advanced names keys; one renamed in the
        // config would leave it counting something that isn't there.
        let written = serde_json::to_value(Config::default()).expect("config serializes");
        for key in FILE_ONLY {
            assert!(written.get(key).is_some(), "{key} isn't a config key");
        }
    }
}
