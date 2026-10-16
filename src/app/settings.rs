//! The settings window — a grouped, scrollable form over [`Config`], opened from
//! the tray. This module owns the view, the message/field vocabulary, and the
//! `App` side of applying an edit: each one is written to the config, clamped,
//! persisted, and anything it affects live is refreshed (no Save button).
//!
//! Text fields are backed by `App::settings_drafts` (populated from the config
//! when the window opens) so what you type is what's shown — parsing a number
//! back into the field on every keystroke would eat a trailing "." or "0".

use iced::widget::{
    Column, Space, button, column, container, pick_list, responsive, row, rule, scrollable, slider,
    text, text_input, toggler,
};
use iced::{Alignment, Color, Element, Fill, Font, Padding, Task};

use crate::config::{Config, LinkStyle, OverlayBackdrop};

use super::{App, Message};

/// Names the single [`Config`] field an edit targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Field {
    Canvas,
    Vault,
    Theme,
    Font,
    SummonHotkey,
    DismissOnBackgroundClick,
    DoubleClickAddsNote,
    // Only surfaced in the UI on macOS (its Spaces are the only workspaces we
    // watch), but the apply/config plumbing stays cross-platform.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    PersistAcrossSpaces,
    // macOS-only experimental trackpad summon gesture; plumbing stays
    // cross-platform so a hand-set value is honored if copied between machines.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    SummonGesture,
    OverlayBackdrop,
    OverlayImage,
    Windowed,
    // macOS-only menu-bar icon appearance; plumbing stays cross-platform so a
    // hand-set value is honored if a config is copied between machines.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    TrayIconMonochrome,
    MinZoom,
    MaxZoom,
    ZoomSmoothing,
    WheelZoomStep,
    PinchZoomStep,
    PanInertia,
    AnimationScale,
    AutosaveDebounceMs,
    ClusterZoom,
    AutoZoomTrigger,
    AutoZoomLevel,
    ThemeCss,
    OverlayDim,
    ColoredCardOpacity,
    CenterNodeText,
    RasterizeText,
    NodeScrollbars,
    LinkStyle,
    NewNodeMinWidth,
    NewNodeMinHeight,
    MaxNodeWidth,
    LayoutRankGap,
    LayoutNodeGap,
    AutoLayout,
    AutoLayoutDirection,
    AiUrl,
    AiModel,
    AiApiKey,
    MaxAiNodes,
    AiTriggerColor,
    AiNodeColor,
    AiThinking,
}

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
    /// Expand/collapse the "Advanced" section (a UI-only toggle, not a config
    /// edit).
    ToggleAdvanced,
}

/// A normalized value handed to [`apply`] — the form's editors are lowered to one
/// of these before touching the config.
pub(crate) enum Value {
    Bool(bool),
    Num(f32),
    Text(String),
}

// Type scale for the form (a step up from iced's 16px default).
const HEADER_SIZE: f32 = 20.0;
const LABEL_SIZE: f32 = 17.0;
const EDITOR_SIZE: f32 = 16.0;
const DESC_SIZE: f32 = 14.0;
/// Width the content column caps at, then centers within a wider window.
const CONTENT_WIDTH: f32 = 760.0;
const INPUT_WIDTH: f32 = 240.0;

/// The settings form, laid out for the room it's given.
///
/// Its fields were a fixed 240 px, so in a narrow window the Browse… buttons
/// beside them went off the panel's edge and couldn't be clicked. Now the
/// fields take what's left beside their label, and where even that isn't
/// enough each label goes above its control.
pub(super) fn view(app: &App) -> Element<'_, Message> {
    responsive(move |size| form(app, Fit::for_width(size.width))).into()
}

/// How the form's rows fit the width they have.
#[derive(Clone, Copy)]
struct Fit {
    /// Label above the control rather than beside it.
    stacked: bool,
    /// How wide a text field is.
    input: f32,
}

impl Fit {
    /// For a form `width` wide. Beside its label, a field shares the row
    /// with the label and a button; stacked, only with the button.
    fn for_width(width: f32) -> Fit {
        /// The content's padding a side, as `form` sets it.
        const PAD: f32 = 26.0;
        /// Room for the label, the gaps and a Browse… button.
        const BESIDE: f32 = 250.0;
        const BUTTON: f32 = 110.0;
        let content = width.min(CONTENT_WIDTH) - 2.0 * PAD;
        let stacked = content - BESIDE < 140.0;
        let input = if stacked { content - BUTTON } else { content - BESIDE };
        Fit { stacked, input: input.clamp(80.0, INPUT_WIDTH) }
    }
}

#[allow(clippy::vec_init_then_push)] // sequential rows + group headers read clearest this way
fn form(app: &App, fit: Fit) -> Element<'_, Message> {
    let c = &app.config;
    // Theme font + muted color for the chrome; the widget colors come from the
    // window's custom theme (startup::settings_theme). Thin forwarding closures
    // so the rows below stay uncluttered.
    let font = app.theme.ui_font();
    let muted = app.theme.text_muted;
    let header = |title| header_row(title, font);
    let setting = |label, editor, desc| setting_row(label, editor, desc, font, muted, fit.stacked);

    let mut items: Vec<Element<Message>> = Vec::new();

    // --- Basics: the handful of things people actually change. Canvas first. ---

    items.push(header("General"));
    items.push(setting(
        "Board",
        path_line(Field::Canvas, app.draft(Field::Canvas), fit.input),
        "The .canvas file opened on launch. Restart to load a different one.",
    ));
    items.push(setting(
        "Vault",
        path_line(Field::Vault, app.draft(Field::Vault), fit.input),
        "Your Obsidian vault folder. Cmd/Ctrl+O then lists every board in it, and the board follows the vault's Obsidian theme. Leave blank to open boards by dialog only.",
    ));
    items.push(hotkey_row(app, font, muted, fit.stacked));
    items.push(setting(
        "Dismiss on background click",
        // Greyed in a window, which a click never dismisses — on there, it
        // read as a setting in force.
        toggle_when(Field::DismissOnBackgroundClick, c.dismiss_on_background_click, app.overlay),
        "Clicking empty space on the overlay dismisses the board. Overlay only.",
    ));
    // Greyed while a click dismisses, which is when it couldn't work: the
    // board is gone before the second click lands.
    items.push(setting(
        "Double-click to add a note",
        toggle_when(
            Field::DoubleClickAddsNote,
            c.double_click_adds_note,
            !c.click_dismisses(app.overlay),
        ),
        "Double-clicking empty space adds a note there. Needs \"Dismiss on background click\" off, or windowed mode.",
    ));
    items.push(setting(
        "Windowed mode",
        toggle(Field::Windowed, c.windowed),
        "Run in an ordinary window instead of the desktop overlay (no summon hotkey or tray). Restart to apply.",
    ));
    // Only macOS watches Space/workspace changes (platform::WATCHES_WORKSPACE);
    // elsewhere the toggle is a no-op, so hide the row. The config field stays
    // cross-platform so a hand-set value copied between machines is honored.
    #[cfg(target_os = "macos")]
    items.push(setting(
        "Persist across Spaces",
        toggle_when(Field::PersistAcrossSpaces, c.persist_across_spaces, app.overlay),
        "Keep the board up when you switch macOS Spaces (else it auto-dismisses). Overlay only.",
    ));
    // Raw-trackpad gesture, macOS-only (uses a private framework).
    #[cfg(target_os = "macos")]
    items.push(setting(
        "Four-finger swipe to summon",
        toggle_when(Field::SummonGesture, c.summon_gesture, app.overlay),
        "Swipe four fingers down on the trackpad to show or hide the board. On by default on a Mac, where the Pause hotkey below needs a keyboard most Macs haven't got. Overlay only.",
    ));

    items.push(header("Appearance"));
    items.push(setting(
        "Theme",
        line(Field::Theme, app.draft(Field::Theme), "ThinkBoard Dark", fit.input),
        "A built-in scheme, or one of your own in the themes folder beside this config. Cmd/Ctrl+Shift+P → \"Change theme\" shows them with a preview. Blank uses the default.",
    ));
    items.push(setting(
        "Font",
        line(Field::Font, app.draft(Field::Font), "Theme default", fit.input),
        "The body typeface, by family name — any font installed on your system. Cmd/Ctrl+Shift+P → \"Change font\" lists them, each set in its own face. Blank uses the theme's.",
    ));
    items.push(setting(
        "Theme CSS",
        path_line(Field::ThemeCss, app.draft(Field::ThemeCss), fit.input),
        "An Obsidian theme.css to read node colors and fonts from. Set a vault above and this follows whichever theme Obsidian is using; pick a file here to override that.",
    ));
    items.push(setting(
        "Center node text",
        toggle(Field::CenterNodeText, c.center_node_text),
        "Center plain text; richer formatting (headings, code, lists) stays left-aligned.",
    ));
    items.push(setting(
        "Link style",
        choices(
            Field::LinkStyle,
            LinkStyle::ALL.iter().map(|s| s.label().to_owned()).collect(),
            LinkStyle::of(c).label().to_owned(),
        ),
        "How links are drawn. Obsidian curves them, with the bulge growing over the distance spanned. Mermaid routes them — a fixed step out of the port, then a near-straight run. Square drops the curve entirely: straight runs meeting at right angles, links off one port sharing the trunk they branch from.",
    ));
    items.push(setting(
        "Node scrollbars",
        toggle(Field::NodeScrollbars, c.node_scrollbars),
        "Let a node be resized smaller than its text; the overflow scrolls via a drag bar (editing re-grows it).",
    ));
    items.push(setting(
        "Overlay background",
        choices(
            Field::OverlayBackdrop,
            OverlayBackdrop::offered().iter().map(|b| b.label().to_owned()).collect(),
            OverlayBackdrop::of(c).label().to_owned(),
        ),
        OVERLAY_BACKGROUND,
    ));
    // The picture, once there's a choice to be made about it.
    if OverlayBackdrop::of(c) == OverlayBackdrop::Image {
        items.push(setting(
            "Overlay image",
            path_line(Field::OverlayImage, app.draft(Field::OverlayImage), fit.input),
            "The picture behind the board — any image file. Right-click an image card and choose \"Use as overlay background\" to use that one.",
        ));
    }
    items.push(setting(
        "Overlay dim",
        feel_slider(Field::OverlayDim, c.overlay_dim, 1.0),
        "How much the overlay dims the desktop (0 none, 1 solid black).",
    ));
    items.push(setting(
        "Colored card opacity",
        feel_slider(Field::ColoredCardOpacity, c.colored_card_opacity, 1.0),
        "How solid a colored card is. Below 1, what's under it shows through — the board, or in the overlay the desktop.",
    ));
    // Template-image menu-bar icon, macOS-only (the template concept is AppKit's).
    #[cfg(target_os = "macos")]
    items.push(setting(
        "Monochrome menu bar icon",
        toggle(Field::TrayIconMonochrome, c.tray_icon_monochrome),
        "Draw the menu bar icon as a monochrome template that matches the menu bar (light/dark).",
    ));

    items.push(header("AI"));
    // The endpoint's fields describe a provider, so they wait for one. Shown
    // under "Off" they were three boxes to fill in for nothing — one already
    // holding a model name — with no sign that none of them did anything.
    let custom = app.settings_custom_ai && !c.ai_enabled();
    let provider = c.ai_enabled() || custom;
    items.push(setting(
        "Provider preset",
        preset_choices(if custom { "Custom".to_owned() } else { current_preset(&c.ai_url) }),
        if provider {
            "Fills the endpoint URL for a known provider. Pick Custom to enter your own."
        } else {
            "Off: Ask AI stays hidden. Pick a provider to turn them on — a local one like LM Studio needs no key."
        },
    ));
    if provider {
        items.push(setting(
            "API URL",
            line(Field::AiUrl, app.draft(Field::AiUrl), "http://localhost:1234/v1", fit.input),
            "OpenAI-compatible base URL (e.g. LM Studio); /chat/completions is appended.",
        ));
        items.push(setting(
            "Model",
            model_line(app.draft(Field::AiModel), fit.input),
            "The model the endpoint answers with. Choose lists the ones it serves, so you needn't type a name exactly.",
        ));
        items.push(setting(
            "API key",
            key_line(Field::AiApiKey, app.draft(Field::AiApiKey), fit.input),
            "Sent as a Bearer token. Required for hosted providers like OpenRouter; leave blank for local LM Studio.",
        ));
    }

    items.push(header("Animation"));
    items.push(setting(
        "Animation speed",
        feel_slider(Field::AnimationScale, c.animation_scale, 3.0),
        "Scales all animation durations. 1.0 is the tuned default; lower is faster; 0 is instant.",
    ));

    // --- Advanced: tuning knobs, revealed by the disclosure header. ---

    items.push(advanced_header(app.settings_advanced_open, font));
    if app.settings_advanced_open {
        items.push(header("Rendering"));
        items.push(setting(
            "Rasterize text",
            toggle(Field::RasterizeText, c.rasterize_text),
            "Pre-rasterize node text into bitmaps (crisp, more GPU) vs. live vector text.",
        ));

        items.push(header("Zoom & Pan"));
        items.push(setting(
            "Minimum zoom",
            number(Field::MinZoom, app.draft(Field::MinZoom)),
            "Smallest zoom the camera allows.",
        ));
        items.push(setting(
            "Maximum zoom",
            number(Field::MaxZoom, app.draft(Field::MaxZoom)),
            "Largest zoom the camera allows.",
        ));
        items.push(setting(
            "Zoom smoothing",
            number(Field::ZoomSmoothing, app.draft(Field::ZoomSmoothing)),
            "Zoom smoothing time in seconds; smaller is snappier.",
        ));
        items.push(setting(
            "Wheel zoom step",
            number(Field::WheelZoomStep, app.draft(Field::WheelZoomStep)),
            "Mouse-wheel zoom sensitivity per wheel line.",
        ));
        items.push(setting(
            "Pinch zoom step",
            number(Field::PinchZoomStep, app.draft(Field::PinchZoomStep)),
            "Trackpad pinch zoom sensitivity.",
        ));
        items.push(setting(
            "Pan inertia",
            feel_slider(Field::PanInertia, c.pan_inertia, 0.999),
            "Pan coasting after a flick: higher coasts longer, 0 disables it.",
        ));

        items.push(header("Clustering & auto-zoom"));
        items.push(setting(
            "Cluster zoom",
            number(Field::ClusterZoom, app.draft(Field::ClusterZoom)),
            "Zoom below which nearby notes collapse into semantic-zoom clusters.",
        ));
        items.push(setting(
            "Auto-zoom trigger",
            number(Field::AutoZoomTrigger, app.draft(Field::AutoZoomTrigger)),
            "Adding a node while zoomed out below this flies the camera in.",
        ));
        items.push(setting(
            "Auto-zoom level",
            number(Field::AutoZoomLevel, app.draft(Field::AutoZoomLevel)),
            "Zoom the camera flies to when auto-zoom triggers.",
        ));

        items.push(header("Nodes & Layout"));
        items.push(setting(
            "New node min width",
            number(Field::NewNodeMinWidth, app.draft(Field::NewNodeMinWidth)),
            "Smallest width a freshly created / auto-sized text node grows within.",
        ));
        items.push(setting(
            "New node min height",
            number(Field::NewNodeMinHeight, app.draft(Field::NewNodeMinHeight)),
            "Smallest height a freshly created text node grows within.",
        ));
        items.push(setting(
            "Max node width",
            number(Field::MaxNodeWidth, app.draft(Field::MaxNodeWidth)),
            "Largest width an auto-sized text node grows to.",
        ));
        items.push(setting(
            "Layout rank gap",
            number(Field::LayoutRankGap, app.draft(Field::LayoutRankGap)),
            "Gap between layers in the Auto-layout command (world px).",
        ));
        items.push(setting(
            "Layout node gap",
            number(Field::LayoutNodeGap, app.draft(Field::LayoutNodeGap)),
            "Gap between siblings in the Auto-layout command (world px).",
        ));
        items.push(setting(
            "Auto-layout on add",
            toggle(Field::AutoLayout, c.auto_layout),
            "Adding a connected node re-lays-out its graph automatically.",
        ));
        items.push(setting(
            "Auto-layout direction",
            choices(
                Field::AutoLayoutDirection,
                vec!["LR".to_owned(), "TD".to_owned()],
                c.auto_layout_direction.clone(),
            ),
            "Flow direction of the auto-layout: LR (left-to-right) or TD (top-down).",
        ));

        items.push(header("Behavior"));
        items.push(setting(
            "Autosave delay (ms)",
            number(Field::AutosaveDebounceMs, app.draft(Field::AutosaveDebounceMs)),
            "Idle time after your last edit before the board autosaves.",
        ));

        items.push(header("AI (advanced)"));
        items.push(setting(
            "Max AI nodes",
            number(Field::MaxAiNodes, app.draft(Field::MaxAiNodes)),
            "Cap on notes a single AI answer may split into.",
        ));
        items.push(setting(
            "Trigger color",
            choices(Field::AiTriggerColor, color_options(), c.ai_trigger_color.clone()),
            "Obsidian color a node is tinted while it triggers an AI request (0 = none).",
        ));
        items.push(setting(
            "Response color",
            choices(Field::AiNodeColor, color_options(), c.ai_node_color.clone()),
            "Obsidian color given to AI-generated response nodes (0 = none).",
        ));
        items.push(setting(
            "AI thinking",
            toggle(Field::AiThinking, c.ai_thinking),
            "Let a reasoning model think before answering (wasteful for short node generation).",
        ));
    }

    let content = Column::with_children(items)
        .spacing(18)
        .padding(Padding::from([16, 26]))
        .max_width(CONTENT_WIDTH);

    // Cap the content width and center it once the window is wider than that.
    let centered = container(content).center_x(Fill);

    container(scrollable(centered).width(Fill))
        .width(Fill)
        .height(Fill)
        .style(|theme: &iced::Theme| container::Style {
            background: Some(theme.palette().background.into()),
            text_color: Some(theme.palette().text),
            ..container::Style::default()
        })
        .into()
}

// --- Row + editor builders ---

/// A group header: a bold title over a hairline rule.
fn header_row(title: &'static str, font: Font) -> Element<'static, Message> {
    column![
        text(title).size(HEADER_SIZE).font(Font {
            weight: iced::font::Weight::Bold,
            ..font
        }),
        rule::horizontal(1),
    ]
    .spacing(6)
    .into()
}

/// The clickable "Advanced" disclosure: a header-styled row with a ▸/▾ marker
/// that expands or collapses the advanced groups below it.
fn advanced_header(open: bool, font: Font) -> Element<'static, Message> {
    let marker = if open { "▾" } else { "▸" };
    let label = text(format!("{marker}  Advanced")).size(HEADER_SIZE).font(Font {
        weight: iced::font::Weight::Bold,
        ..font
    });
    column![
        button(label)
            .padding(0)
            .on_press(Message::Settings(Msg::ToggleAdvanced))
            .style(|theme: &iced::Theme, _status| button::Style {
                background: None,
                text_color: theme.palette().text,
                ..button::Style::default()
            }),
        rule::horizontal(1),
    ]
    .spacing(6)
    .into()
}

/// What the overlay-background row says. The live desktop is the Dashboard
/// look, and it costs: the board is blended over every window under it, every
/// frame, which on a ProMotion display over a busy desktop drops it to 60 Hz.
const OVERLAY_BACKGROUND: &str = if crate::platform::SUPPORTS_BACKDROP_PICTURE {
    "What's behind the board. Transparent shows the desktop live, and can stutter over busy windows; the others cover them. The wallpaper is read when the app starts. Overlay only."
} else {
    "What's behind the board. Theme colour if transparent renders black, or stutters over other windows. Restart to apply. Overlay only."
};

/// A settings row: `label ........ editor`, with a secondary description below
/// in the theme's muted color.
fn setting_row<'a>(
    label: &'static str,
    editor: Element<'a, Message>,
    desc: &'static str,
    font: Font,
    muted: Color,
    stacked: bool,
) -> Element<'a, Message> {
    let label = text(label).size(LABEL_SIZE).font(font);
    let top: Element<'a, Message> = if stacked {
        column![label, editor].spacing(6).into()
    } else {
        row![label, Space::new().width(Fill), editor].align_y(Alignment::Center).spacing(12).into()
    };
    column![top, text(desc).size(DESC_SIZE).color(muted).font(font)].spacing(5).into()
}

fn toggle<'a>(field: Field, on: bool) -> Element<'a, Message> {
    toggle_when(field, on, true)
}

/// A switch that only flips while `enabled` — one setting that depends on
/// another shows its value greyed rather than vanishing, so the dependency
/// can be read off the form.
fn toggle_when<'a>(field: Field, on: bool, enabled: bool) -> Element<'a, Message> {
    toggler(on)
        .on_toggle_maybe(enabled.then_some(move |b| Message::Settings(Msg::Toggle(field, b))))
        .size(20)
        .into()
}

/// A freeform text field.
fn line<'a>(field: Field, value: &'a str, placeholder: &'a str, width: f32) -> Element<'a, Message> {
    text_input(placeholder, value)
        .on_input(move |s| Message::Settings(Msg::Edit(field, s)))
        .on_submit(Message::Settings(Msg::Commit))
        .size(EDITOR_SIZE)
        .width(width)
        .into()
}

/// A numeric text field (parsed on the way into the config).
fn number<'a>(field: Field, value: &'a str) -> Element<'a, Message> {
    text_input("", value)
        .on_input(move |s| Message::Settings(Msg::Edit(field, s)))
        .on_submit(Message::Settings(Msg::Commit))
        .size(EDITOR_SIZE)
        .width(100)
        .into()
}

/// A 0..=`max` slider with a live value readout.
fn feel_slider<'a>(field: Field, value: f32, max: f32) -> Element<'a, Message> {
    row![
        slider(0.0..=max, value, move |v| Message::Settings(Msg::Slide(field, v)))
            .on_release(Message::Settings(Msg::Commit))
            .step(0.01_f32)
            .width(170),
        text(format!("{value:.2}")).size(EDITOR_SIZE).width(38),
    ]
    .spacing(10)
    .align_y(Alignment::Center)
    .into()
}

fn choices<'a>(field: Field, options: Vec<String>, selected: String) -> Element<'a, Message> {
    pick_list(options, Some(selected), move |s| {
        Message::Settings(Msg::Pick(field, s))
    })
    .text_size(EDITOR_SIZE)
    .into()
}

/// A text field with a Browse… button that opens a native file dialog.
fn path_line<'a>(field: Field, value: &'a str, width: f32) -> Element<'a, Message> {
    row![
        text_input("", value)
            .on_input(move |s| Message::Settings(Msg::Edit(field, s)))
            .on_submit(Message::Settings(Msg::Commit))
            .size(EDITOR_SIZE)
            .width(width),
        button(text("Browse…").size(EDITOR_SIZE))
            .padding([5, 12])
            .on_press(Message::Settings(Msg::Browse(field))),
    ]
    .spacing(8)
    .align_y(Alignment::Center)
    .into()
}

/// A masked (bulleted) text field with a Paste button, for the API key.
fn key_line<'a>(field: Field, value: &'a str, width: f32) -> Element<'a, Message> {
    row![
        text_input("", value)
            .secure(true)
            .on_input(move |s| Message::Settings(Msg::Edit(field, s)))
            .on_submit(Message::Settings(Msg::Commit))
            .size(EDITOR_SIZE)
            .width(width),
        button(text("Paste").size(EDITOR_SIZE))
            .padding([5, 12])
            .on_press(Message::Settings(Msg::Paste(field))),
    ]
    .spacing(8)
    .align_y(Alignment::Center)
    .into()
}

/// The model field, with a button that lists what the endpoint serves.
fn model_line(value: &str, width: f32) -> Element<'_, Message> {
    row![
        text_input("", value)
            .on_input(|s| Message::Settings(Msg::Edit(Field::AiModel, s)))
            .on_submit(Message::Settings(Msg::Commit))
            .size(EDITOR_SIZE)
            .width(width),
        button(text("Choose…").size(EDITOR_SIZE))
            .padding([5, 12])
            .on_press(Message::Settings(Msg::ChooseModel)),
    ]
    .spacing(8)
    .align_y(Alignment::Center)
    .into()
}

/// Whether the configured endpoint can list its models yet: a key is stored
/// for it, or it's on this machine, where a runtime asks for none. A hosted
/// provider picked before its key is pasted would only answer the list with
/// a refusal.
fn ready_to_list(config: &crate::config::Config) -> bool {
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

/// The provider-preset picker: "Off" (AI disabled), the known providers, then
/// "Custom" (any URL not matching one). `selected` is the name for the current
/// URL.
fn preset_choices(selected: String) -> Element<'static, Message> {
    let options: Vec<String> = std::iter::once(PROVIDER_OFF.to_owned())
        .chain(crate::ai::PRESETS.iter().map(|p| p.name.to_owned()))
        .chain(std::iter::once("Custom".to_owned()))
        .collect();
    pick_list(options, Some(selected), |s| Message::Settings(Msg::PickPreset(s)))
        .text_size(EDITOR_SIZE)
        .into()
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

/// The summon-hotkey row: shows the current combo (or "Press a shortcut…" while
/// recording) and a Record button that captures the next key combination.
fn hotkey_row(app: &App, font: Font, muted: Color, stacked: bool) -> Element<'_, Message> {
    let editor: Element<Message> = if app.hotkey_recording {
        row![
            text("Press a shortcut…  (Esc to cancel)")
                .size(EDITOR_SIZE)
                .font(font)
                .color(muted),
        ]
        .into()
    } else {
        let current = app.config.summon_hotkey.trim();
        let shown = if current.is_empty() {
            "None".to_owned()
        } else {
            current.to_owned()
        };
        row![
            text(shown).size(EDITOR_SIZE).font(font),
            button(text("Set shortcut…").size(EDITOR_SIZE).font(font))
                .padding([5, 12])
                .on_press(Message::RecordHotkey),
        ]
        .spacing(12)
        .align_y(Alignment::Center)
        .into()
    };

    setting_row(
        "Summon hotkey",
        editor,
        "Global summon/dismiss key. Click Set shortcut and press the combination.",
        font,
        muted,
        stacked,
    )
}

/// The Obsidian color-code choices for AI tints: "0" (none) through "6".
fn color_options() -> Vec<String> {
    (0..=6).map(|n| n.to_string()).collect()
}

// --- Applying edits to the config ---

/// Whether a field is edited as a number (so text input is parsed as `f32`).
pub(crate) fn is_number(field: Field) -> bool {
    matches!(
        field,
        Field::MinZoom
            | Field::MaxZoom
            | Field::ZoomSmoothing
            | Field::WheelZoomStep
            | Field::PinchZoomStep
            | Field::AutosaveDebounceMs
            | Field::ClusterZoom
            | Field::AutoZoomTrigger
            | Field::AutoZoomLevel
            | Field::NewNodeMinWidth
            | Field::NewNodeMinHeight
            | Field::MaxNodeWidth
            | Field::LayoutRankGap
            | Field::LayoutNodeGap
            | Field::MaxAiNodes
    )
}

/// The current config value of a text-editable `field`, formatted for display.
/// Used to seed `settings_drafts` when the window opens.
pub(crate) fn config_text(config: &Config, field: Field) -> String {
    match field {
        Field::Canvas => config.canvas.clone(),
        Field::Vault => config.vault.clone(),
        Field::Theme => config.theme.clone(),
        Field::Font => config.font.clone(),
        Field::SummonHotkey => config.summon_hotkey.clone(),
        Field::ThemeCss => config.theme_css.clone().unwrap_or_default(),
        Field::OverlayImage => config.overlay_image.clone(),
        Field::AiUrl => config.ai_url.clone(),
        Field::AiModel => config.ai_model.clone(),
        Field::AiApiKey => config.ai_key().to_owned(),
        Field::MinZoom => config.min_zoom.to_string(),
        Field::MaxZoom => config.max_zoom.to_string(),
        Field::ZoomSmoothing => config.zoom_smoothing.to_string(),
        Field::WheelZoomStep => config.wheel_zoom_step.to_string(),
        Field::PinchZoomStep => config.pinch_zoom_step.to_string(),
        Field::AutosaveDebounceMs => config.autosave_debounce_ms.to_string(),
        Field::ClusterZoom => config.cluster_zoom.to_string(),
        Field::AutoZoomTrigger => config.auto_zoom_trigger.to_string(),
        Field::AutoZoomLevel => config.auto_zoom_level.to_string(),
        Field::NewNodeMinWidth => config.new_node_min_width.to_string(),
        Field::NewNodeMinHeight => config.new_node_min_height.to_string(),
        Field::MaxNodeWidth => config.max_node_width.to_string(),
        Field::LayoutRankGap => config.layout_rank_gap.to_string(),
        Field::LayoutNodeGap => config.layout_node_gap.to_string(),
        Field::MaxAiNodes => config.max_ai_nodes.to_string(),
        // Non-text fields (toggles, sliders, pick lists) never seed a draft.
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
        (Field::AiTriggerColor, Value::Text(s)) => config.ai_trigger_color = s,
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
        (Field::AutoLayout, Value::Bool(b)) => config.auto_layout = b,
        (Field::AiThinking, Value::Bool(b)) => config.ai_thinking = b,

        (Field::MinZoom, Value::Num(v)) => config.min_zoom = v,
        (Field::MaxZoom, Value::Num(v)) => config.max_zoom = v,
        (Field::ZoomSmoothing, Value::Num(v)) => config.zoom_smoothing = v,
        (Field::WheelZoomStep, Value::Num(v)) => config.wheel_zoom_step = v,
        (Field::PinchZoomStep, Value::Num(v)) => config.pinch_zoom_step = v,
        (Field::PanInertia, Value::Num(v)) => config.pan_inertia = v,
        (Field::AnimationScale, Value::Num(v)) => config.animation_scale = v,
        (Field::AutosaveDebounceMs, Value::Num(v)) => config.autosave_debounce_ms = v.max(0.0) as u64,
        (Field::ClusterZoom, Value::Num(v)) => config.cluster_zoom = v,
        (Field::AutoZoomTrigger, Value::Num(v)) => config.auto_zoom_trigger = v,
        (Field::AutoZoomLevel, Value::Num(v)) => config.auto_zoom_level = v,
        (Field::OverlayDim, Value::Num(v)) => config.overlay_dim = v,
        (Field::ColoredCardOpacity, Value::Num(v)) => config.colored_card_opacity = v,
        (Field::NewNodeMinWidth, Value::Num(v)) => config.new_node_min_width = v,
        (Field::NewNodeMinHeight, Value::Num(v)) => config.new_node_min_height = v,
        (Field::MaxNodeWidth, Value::Num(v)) => config.max_node_width = v,
        (Field::LayoutRankGap, Value::Num(v)) => config.layout_rank_gap = v,
        (Field::LayoutNodeGap, Value::Num(v)) => config.layout_node_gap = v,
        (Field::MaxAiNodes, Value::Num(v)) => config.max_ai_nodes = v.max(0.0) as usize,

        // Type/field mismatches (shouldn't happen — editors and fields agree).
        _ => {}
    }
}

impl App {
    /// The raw text of a settings text/number field (seeded from the config when
    /// the window opens; reflects exactly what the user has typed since).
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
        for field in [
            Canvas, SummonHotkey, ThemeCss, OverlayImage, AiUrl, AiModel, AiApiKey, MinZoom, MaxZoom,
            ZoomSmoothing, WheelZoomStep, PinchZoomStep, AutosaveDebounceMs, ClusterZoom,
            AutoZoomTrigger, AutoZoomLevel, NewNodeMinWidth, NewNodeMinHeight, MaxNodeWidth,
            LayoutRankGap, LayoutNodeGap, MaxAiNodes,
        ] {
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
        match msg {
            Msg::Toggle(field, b) => {
                apply(&mut self.config, field, Value::Bool(b));
                self.config.sanitize();
                self.persist_config();
                self.after_setting_change(field)
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
            Msg::ToggleAdvanced => {
                self.settings_advanced_open = !self.settings_advanced_open;
                Task::none()
            }
        }
    }

    /// Live side effects of a settings change (over and above the config write).
    fn after_setting_change(&mut self, field: Field) -> Task<Message> {
        // Where the overlay can change in place, it does; elsewhere the row
        // says it waits for a restart.
        if matches!(field, Field::OverlayBackdrop | Field::OverlayImage) && self.overlay {
            self.apply_backdrop();
        }
        self.cache.clear();
        // Keys are per endpoint, so changing the endpoint changes which one the
        // field is showing.
        if field == Field::AiUrl {
            self.settings_drafts
                .insert(Field::AiApiKey, self.config.ai_key().to_owned());
        }
        match field {
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
        }
    }

    /// Opens a file dialog for a path setting. macOS/Windows use the blocking
    /// dialog on the (main) update thread; Linux runs the async portal dialog on
    /// iced's runtime (its zbus backend needs one — iced is built with tokio).
    fn browse_for(&mut self, field: Field) -> Task<Message> {
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
            _ => false,
        }
    }

    /// Applies a resolved "Browse…" dialog result to its field.
    pub(super) fn settings_browsed(&mut self, field: Field, path: Option<std::path::PathBuf>) -> Task<Message> {
        let Some(path) = path else {
            return Task::none();
        };
        let text = path.to_string_lossy().into_owned();
        self.settings_drafts.insert(field, text.clone());
        apply(&mut self.config, field, Value::Text(text));
        self.config.sanitize();
        self.persist_config();
        // As a typed path is, once committed: a stylesheet chosen here used
        // to wait for a restart to be applied.
        self.after_setting_change(field)
    }

    /// Closes the settings window's state (its drafts and the Advanced
    /// disclosure), persisting whatever was typed.
    pub(super) fn close_settings_window(&mut self, persist: bool) {
        if persist {
            self.persist_config();
        }
        self.settings_window = None;
        self.settings_drafts.clear();
        self.settings_advanced_open = false;
        self.close_settings_model_picker();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn apply_writes_each_value_kind() {
        let mut c = Config::default();

        apply(&mut c, Field::OverlayBackdrop, Value::Text("Theme colour".to_owned()));
        assert_eq!(OverlayBackdrop::of(&c), OverlayBackdrop::Theme);
        assert!(!c.overlay_transparent, "kept in step, for an older build");

        apply(&mut c, Field::OverlayDim, Value::Num(0.25));
        assert_eq!(c.overlay_dim, 0.25);

        apply(&mut c, Field::AiModel, Value::Text("gpt-x".to_owned()));
        assert_eq!(c.ai_model, "gpt-x");

        // usize/u64 fields coerce from the numeric value.
        apply(&mut c, Field::MaxAiNodes, Value::Num(9.0));
        assert_eq!(c.max_ai_nodes, 9);

        // theme_css: a blank string clears it to None.
        apply(&mut c, Field::ThemeCss, Value::Text("  ".to_owned()));
        assert_eq!(c.theme_css, None);
    }

    #[test]
    fn number_fields_round_trip_through_text() {
        // config_text formats the value; parsing that text back and applying it
        // must restore the same value (the drafts-backed number editor path).
        let c = Config::default();
        for field in [Field::MinZoom, Field::MaxNodeWidth, Field::AutosaveDebounceMs] {
            let text = config_text(&c, field);
            let value: f32 = text.parse().expect("numeric field formats as a number");
            let mut edited = Config::default();
            apply(&mut edited, field, Value::Num(value));
            assert_eq!(config_text(&edited, field), text, "round-trip for {field:?}");
        }
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
    fn is_number_matches_numeric_fields() {
        assert!(is_number(Field::MinZoom));
        assert!(is_number(Field::MaxAiNodes));
        assert!(!is_number(Field::AiModel));
        assert!(!is_number(Field::OverlayBackdrop));
    }
}
