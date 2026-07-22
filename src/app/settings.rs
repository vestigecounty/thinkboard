//! The settings window — a grouped, scrollable form over [`Config`], opened from
//! the tray. This module owns the view and the message/field vocabulary; the
//! edits are applied and persisted in `App::update` (live, no Save button).
//!
//! Text fields are backed by `App::settings_drafts` (populated from the config
//! when the window opens) so what you type is what's shown — parsing a number
//! back into the field on every keystroke would eat a trailing "." or "0".

use iced::widget::{
    Column, Space, button, column, container, pick_list, row, rule, scrollable, slider, text,
    text_input, toggler,
};
use iced::{Alignment, Color, Element, Fill, Font, Padding};

use crate::config::Config;

use super::{App, Message};

/// Names the single [`Config`] field an edit targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Field {
    Canvas,
    SummonHotkey,
    DismissOnBackgroundClick,
    // Only surfaced in the UI on macOS (its Spaces are the only workspaces we
    // watch), but the apply/config plumbing stays cross-platform.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    PersistAcrossSpaces,
    // macOS-only experimental trackpad summon gesture; plumbing stays
    // cross-platform so a hand-set value is honored if copied between machines.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    SummonGesture,
    OverlayTransparent,
    Windowed,
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
    CenterNodeText,
    RasterizeText,
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
    AiCompletions,
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

#[allow(clippy::vec_init_then_push)] // sequential rows + group headers read clearest this way
pub(super) fn view(app: &App) -> Element<'_, Message> {
    let c = &app.config;
    // Theme font + muted color for the chrome; the widget colors come from the
    // window's custom theme (startup::settings_theme). Thin forwarding closures
    // so the rows below stay uncluttered.
    let font = app.theme.ui_font();
    let muted = app.theme.text_muted;
    let header = |title| header_row(title, font);
    let setting = |label, editor, desc| setting_row(label, editor, desc, font, muted);

    let mut items: Vec<Element<Message>> = Vec::new();

    // --- Basics: the handful of things people actually change. Canvas first. ---

    items.push(header("General"));
    items.push(setting(
        "Canvas",
        path_line(Field::Canvas, app.draft(Field::Canvas)),
        "The board opened on launch. Restart to load a different one.",
    ));
    items.push(hotkey_row(app, font, muted));
    items.push(setting(
        "Dismiss on background click",
        toggle(Field::DismissOnBackgroundClick, c.dismiss_on_background_click),
        "Clicking empty space on the overlay dismisses the board.",
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
        toggle(Field::PersistAcrossSpaces, c.persist_across_spaces),
        "Keep the board up when you switch macOS Spaces (else it auto-dismisses).",
    ));
    // Experimental raw-trackpad gesture, macOS-only (uses a private framework).
    #[cfg(target_os = "macos")]
    items.push(setting(
        "Four-finger swipe to summon",
        toggle(Field::SummonGesture, c.summon_gesture),
        "Experimental: swipe four fingers down on the trackpad to show/hide the board.",
    ));

    items.push(header("Appearance"));
    items.push(setting(
        "Theme CSS",
        path_line(Field::ThemeCss, app.draft(Field::ThemeCss)),
        "An Obsidian theme.css to read node colors and fonts from. Restart to apply.",
    ));
    items.push(setting(
        "Center node text",
        toggle(Field::CenterNodeText, c.center_node_text),
        "Center plain text; richer formatting (headings, code, lists) stays left-aligned.",
    ));
    items.push(setting(
        "Overlay dim",
        feel_slider(Field::OverlayDim, c.overlay_dim, 1.0),
        "How much the overlay dims the desktop (0 none, 1 solid black).",
    ));

    items.push(header("AI"));
    items.push(setting(
        "Provider preset",
        preset_choices(current_preset(&c.ai_url)),
        "Fills the endpoint URL for a known provider. Pick Custom to enter your own.",
    ));
    items.push(setting(
        "API URL",
        line(Field::AiUrl, app.draft(Field::AiUrl), "http://localhost:1234/v1"),
        "OpenAI-compatible base URL (e.g. LM Studio); /chat/completions is appended.",
    ));
    items.push(setting(
        "Model",
        line(Field::AiModel, app.draft(Field::AiModel), ""),
        "Model name sent to the endpoint.",
    ));
    items.push(setting(
        "API key",
        key_line(Field::AiApiKey, app.draft(Field::AiApiKey)),
        "Sent as a Bearer token. Required for hosted providers like OpenRouter; leave blank for local LM Studio.",
    ));

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
            "Transparent overlay",
            toggle(Field::OverlayTransparent, c.overlay_transparent),
            "Desktop shows through the board. Turn off if it renders black. Restart to apply.",
        ));
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
            "AI completions",
            toggle(Field::AiCompletions, c.ai_completions),
            "After adding a connected node, offer a few \"ghost\" sibling suggestions.",
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

/// A settings row: `label ........ editor`, with a secondary description below
/// in the theme's muted color.
fn setting_row<'a>(
    label: &'static str,
    editor: Element<'a, Message>,
    desc: &'static str,
    font: Font,
    muted: Color,
) -> Element<'a, Message> {
    column![
        row![
            text(label).size(LABEL_SIZE).font(font),
            Space::new().width(Fill),
            editor
        ]
        .align_y(Alignment::Center)
        .spacing(12),
        text(desc).size(DESC_SIZE).color(muted).font(font),
    ]
    .spacing(5)
    .into()
}

fn toggle<'a>(field: Field, on: bool) -> Element<'a, Message> {
    toggler(on)
        .on_toggle(move |b| Message::Settings(Msg::Toggle(field, b)))
        .size(20)
        .into()
}

/// A freeform text field.
fn line<'a>(field: Field, value: &'a str, placeholder: &'a str) -> Element<'a, Message> {
    text_input(placeholder, value)
        .on_input(move |s| Message::Settings(Msg::Edit(field, s)))
        .on_submit(Message::Settings(Msg::Commit))
        .size(EDITOR_SIZE)
        .width(INPUT_WIDTH)
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
fn path_line<'a>(field: Field, value: &'a str) -> Element<'a, Message> {
    row![
        text_input("", value)
            .on_input(move |s| Message::Settings(Msg::Edit(field, s)))
            .on_submit(Message::Settings(Msg::Commit))
            .size(EDITOR_SIZE)
            .width(INPUT_WIDTH),
        button(text("Browse…").size(EDITOR_SIZE))
            .padding([5, 12])
            .on_press(Message::Settings(Msg::Browse(field))),
    ]
    .spacing(8)
    .align_y(Alignment::Center)
    .into()
}

/// A masked (bulleted) text field with a Paste button, for the API key.
fn key_line<'a>(field: Field, value: &'a str) -> Element<'a, Message> {
    row![
        text_input("", value)
            .secure(true)
            .on_input(move |s| Message::Settings(Msg::Edit(field, s)))
            .on_submit(Message::Settings(Msg::Commit))
            .size(EDITOR_SIZE)
            .width(INPUT_WIDTH),
        button(text("Paste").size(EDITOR_SIZE))
            .padding([5, 12])
            .on_press(Message::Settings(Msg::Paste(field))),
    ]
    .spacing(8)
    .align_y(Alignment::Center)
    .into()
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
fn hotkey_row(app: &App, font: Font, muted: Color) -> Element<'_, Message> {
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
        Field::SummonHotkey => config.summon_hotkey.clone(),
        Field::ThemeCss => config.theme_css.clone().unwrap_or_default(),
        Field::AiUrl => config.ai_url.clone(),
        Field::AiModel => config.ai_model.clone(),
        Field::AiApiKey => config.ai_api_key.clone(),
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
        (Field::SummonHotkey, Value::Text(s)) => config.summon_hotkey = s,
        (Field::ThemeCss, Value::Text(s)) => {
            config.theme_css = if s.trim().is_empty() { None } else { Some(s) };
        }
        (Field::AiUrl, Value::Text(s)) => config.ai_url = s,
        (Field::AiModel, Value::Text(s)) => config.ai_model = s,
        (Field::AiApiKey, Value::Text(s)) => config.ai_api_key = s,
        (Field::AutoLayoutDirection, Value::Text(s)) => config.auto_layout_direction = s,
        (Field::AiTriggerColor, Value::Text(s)) => config.ai_trigger_color = s,
        (Field::AiNodeColor, Value::Text(s)) => config.ai_node_color = s,

        (Field::DismissOnBackgroundClick, Value::Bool(b)) => config.dismiss_on_background_click = b,
        (Field::PersistAcrossSpaces, Value::Bool(b)) => config.persist_across_spaces = b,
        (Field::SummonGesture, Value::Bool(b)) => config.summon_gesture = b,
        (Field::OverlayTransparent, Value::Bool(b)) => config.overlay_transparent = b,
        (Field::Windowed, Value::Bool(b)) => config.windowed = b,
        (Field::CenterNodeText, Value::Bool(b)) => config.center_node_text = b,
        (Field::RasterizeText, Value::Bool(b)) => config.rasterize_text = b,
        (Field::AutoLayout, Value::Bool(b)) => config.auto_layout = b,
        (Field::AiCompletions, Value::Bool(b)) => config.ai_completions = b,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn apply_writes_each_value_kind() {
        let mut c = Config::default();

        apply(&mut c, Field::OverlayTransparent, Value::Bool(false));
        assert!(!c.overlay_transparent);

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
        assert!(!is_number(Field::OverlayTransparent));
    }
}
