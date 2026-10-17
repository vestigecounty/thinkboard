//! What the settings form looks like: a tab bar, then the tab's rows.
//!
//! **A row is a label on the left and its control on the right**, a hairline
//! above it, and nothing else until it's focused — by pressing it, or by using
//! its control — when it says what it does. Every row used to carry its
//! description, and the form ran to nine screens. Anything that *can't* wait
//! for focus is a note, always shown: which theme is in use, why a switch is
//! greyed, what a provider needs, how much a performance setting matters.
//!
//! **A changed setting is marked where it is**, by a dot beside its label and,
//! where picking the default isn't already one of the choices on the row, a
//! link that puts it back. The tabs carry no count of their own: a number
//! beside "General" read as a notification rather than as "two of these
//! differ".
//!
//! Colours come from the theme, by role, so the form reads as part of
//! whichever one is in use — the controls are styled here rather than left to
//! iced's palette, which has one colour where the form needs a field's well, a
//! button's face and the hairlines between rows.

use std::borrow::Cow;
use std::ops::RangeInclusive;
use std::path::Path;

use iced::widget::canvas::{self, Frame, Geometry, Path as Pen, Stroke};
use iced::widget::{
    Column, Row, Space, Text, button, column, container, mouse_area, pick_list, responsive, row,
    scrollable, slider, stack, text, text_input, toggler,
};
use iced::{Alignment, Border, Color, Element, Fill, Font, Padding, Point, Rectangle, Renderer, Shrink, Size, Theme};

use super::{FILE_ONLY, Field, Msg, PROVIDER_OFF, Tab, current_preset, hand_picked_css, is_local};
use crate::app::chrome::mix;
use crate::app::{App, Message};
use crate::config::{ArrowTip, Config, LinkStyle, OverlayBackdrop};
use crate::platform::BackdropPicture;
use crate::document::IntoColor;
use crate::model;

/// Below this width a row's control goes under its label rather than beside
/// it: beside, a field and its Browse… button left the label two words a line.
const STACK_BELOW: f32 = 440.0;
/// How wide the window's form grows before it centres instead.
const CONTENT_WIDTH: f32 = 760.0;
/// The form's scrolling body, so a setting opened from the palette can be
/// scrolled to.
pub(in crate::app) static SCROLL: std::sync::LazyLock<iced::advanced::widget::Id> =
    std::sync::LazyLock::new(|| iced::advanced::widget::Id::new("settings-form"));

/// The shortcut that lists a vault's boards, as this platform writes it.
const OPEN_KEYS: &str = if cfg!(target_os = "macos") { "⌘O" } else { "Ctrl+O" };
/// How tall every control is — field, button, list, keycap — so that two
/// side by side are the same height, and a row is as tall as its tallest.
const CONTROL: f32 = 30.0;
/// The width of a button beside a field (Browse…, Choose…, Paste, Record…).
/// One width for all, or each field started wherever its button's word let it.
const BUTTON: f32 = 84.0;

/// The form's type scale. Every size sits on a whole-pixel line: at
/// fractional heights (12.5 px × 1.3) each line below one landed between
/// pixels, and the same label drew heavier in one row than in the next.
#[derive(Clone, Copy)]
enum Type {
    /// The modal's title.
    Title,
    /// A section's name.
    Head,
    /// A row's label; a tab.
    Label,
    /// Inside a control: a field's text, a button's word, a readout.
    Control,
    /// Everything secondary: descriptions, notes, captions, units.
    Small,
    /// A chip.
    Tiny,
}

impl Type {
    /// Its size and the line it sits on, in pixels.
    fn metrics(self) -> (f32, f32) {
        match self {
            Type::Title => (17.0, 22.0),
            Type::Head => (15.0, 20.0),
            Type::Label => (14.0, 18.0),
            Type::Control => (13.0, 18.0),
            Type::Small => (12.0, 16.0),
            Type::Tiny => (11.0, 16.0),
        }
    }

    fn line(self) -> text::LineHeight {
        text::LineHeight::Absolute(self.metrics().1.into())
    }
}

/// Words in the form's type scale.
fn say<'a>(words: impl text::IntoFragment<'a>, kind: Type, font: Font, color: Color) -> Text<'a, Theme, Renderer> {
    let (size, _) = kind.metrics();
    text(words).size(size).line_height(kind.line()).font(font).color(color)
}

/// The form in the board's modal: its title and close button, the tabs, and
/// the tab's rows at their natural height — the panel caps that, and scrolls.
pub(in crate::app) fn modal(app: &App, width: f32) -> Element<'_, Message> {
    let look = Look::of(app, width);
    let close = button(text("×").size(20).line_height(text::LineHeight::Absolute(22.0.into())).font(look.font).color(look.muted))
        .padding([0, 6])
        .on_press(Message::CloseSettingsOverlay)
        .style(bare(look));
    let title = row![say("Settings", Type::Title, look.strong, look.text).width(Fill), close]
        .align_y(Alignment::Center);
    let head = column![title, tabs(app, look)].spacing(6).padding(pad(16.0, 18.0, 0.0, 24.0));
    column![head, hairline(look), scrollable(body(app, look)).id(SCROLL.clone()).height(Shrink)].into()
}

/// The form in a window of its own, which has a title already: the tabs, and
/// the rows filling the window, centred once it's wider than they need.
pub(in crate::app) fn window(app: &App) -> Element<'_, Message> {
    responsive(move |size| {
        let look = Look::of(app, size.width.min(CONTENT_WIDTH));
        // The tabs start where the labels do, centred or not.
        let head = container(
            container(tabs(app, look)).width(Fill).max_width(CONTENT_WIDTH).padding(pad(10.0, 24.0, 0.0, 24.0)),
        )
        .center_x(Fill);
        let rows = container(container(body(app, look)).max_width(CONTENT_WIDTH)).center_x(Fill);
        let sheet = column![head, hairline(look), scrollable(rows).id(SCROLL.clone()).height(Fill)];
        container(sheet)
            .width(Fill)
            .height(Fill)
            .style(move |_theme| container::Style {
                background: Some(look.panel.into()),
                text_color: Some(look.text),
                ..container::Style::default()
            })
            .into()
    })
    .into()
}

/// The colours and measures the form is drawn with, taken from the theme.
#[derive(Clone, Copy)]
struct Look {
    font: Font,
    strong: Font,
    mono: Font,
    text: Color,
    muted: Color,
    /// Quieter than muted: a section's aside, a tick on a slider.
    faint: Color,
    accent: Color,
    /// Text on an accent fill.
    on_accent: Color,
    /// The hairlines between rows and around fields.
    line: Color,
    /// A button's or a list's face: a step up from the panel.
    control: Color,
    /// A switch that's off, the unlit part of a slider: a step further up.
    track: Color,
    /// A field's well: a step down from the panel.
    well: Color,
    panel: Color,
    /// What a restart and the performance bars are marked in.
    amber: Color,
    /// Label above control, for a narrow form.
    stacked: bool,
    /// How wide a path or a text field is.
    field: f32,
}

impl Look {
    fn of(app: &App, width: f32) -> Look {
        let theme = &app.theme;
        let dark = luminance(theme.background) < 0.5;
        // The weights the theme's family actually ships: the shaper matches
        // weight exactly, and a family with no 600 drew a semibold tab in some
        // other face altogether, smaller than the tabs beside it.
        let font = Font { weight: crate::theme::iced_weight(theme.weight_normal), ..theme.ui_font() };
        let stacked = width < STACK_BELOW;
        Look {
            font,
            strong: Font { weight: crate::theme::iced_weight(theme.weight_bold), ..font },
            mono: Font::MONOSPACE,
            text: theme.text,
            muted: theme.text_muted,
            faint: mix(theme.text_muted, theme.background, 0.3),
            accent: theme.accent,
            on_accent: if luminance(theme.accent) > 0.45 { Color::from_rgb8(0x0d, 0x14, 0x22) } else { Color::WHITE },
            line: Color { a: 0.1, ..theme.text },
            control: mix(theme.background, theme.text, 0.08),
            track: mix(theme.background, theme.text, 0.14),
            well: if dark { mix(theme.background, Color::BLACK, 0.3) } else { mix(theme.background, Color::WHITE, 0.7) },
            panel: theme.background,
            amber: if dark { Color::from_rgb8(0xe2, 0xc2, 0x7e) } else { Color::from_rgb8(0x9a, 0x6b, 0x10) },
            stacked,
            // Stacked, a field has the row to itself but for its button.
            field: if stacked { (width - 160.0).clamp(80.0, 260.0) } else { 170.0 },
        }
    }
}

fn luminance(color: Color) -> f32 {
    0.2126 * color.r + 0.7152 * color.g + 0.0722 * color.b
}

fn pad(top: f32, right: f32, bottom: f32, left: f32) -> Padding {
    Padding { top, right, bottom, left }
}

fn fill(color: Color) -> container::Style {
    container::Style { background: Some(color.into()), ..container::Style::default() }
}

fn hairline<'a>(look: Look) -> Element<'a, Message> {
    container(Space::new().height(1)).width(Fill).style(move |_theme| fill(look.line)).into()
}

/// The tab bar. The current tab is in the text colour and underlined in the
/// accent; the rest are muted.
fn tabs(app: &App, look: Look) -> Element<'_, Message> {
    // Narrow, a size down and closer together, so the five still fit on one
    // line; wrapped, "Advanced" sat alone under the rest like a section.
    let mut bar = Row::new().spacing(if look.stacked { 10 } else { 20 });
    for tab in Tab::ALL {
        let on = app.settings_tab == tab;
        let label = say(
            tab.label(),
            if look.stacked { Type::Control } else { Type::Label },
            if on { look.strong } else { look.font },
            if on { look.text } else { look.muted },
        );
        let underline = container(Space::new().height(2)).width(Fill).style(move |_theme| container::Style {
            background: on.then(|| look.accent.into()),
            ..container::Style::default()
        });
        let face = column![container(label).padding([8, 0]), underline].width(Shrink);
        bar = bar.push(button(face).padding(0).style(bare(look)).on_press(Message::Settings(Msg::Tab(tab))));
    }
    bar.wrap().vertical_spacing(0).into()
}

/// The current tab's sections.
fn body(app: &App, look: Look) -> Element<'_, Message> {
    let sections = match app.settings_tab {
        Tab::General => general(app, look),
        Tab::Appearance => appearance(app, look),
        Tab::Canvas => canvas_tab(app, look),
        Tab::Ai => ai(app, look),
        Tab::Advanced => advanced(app, look),
    };
    Column::with_children(sections).padding(pad(4.0, 8.0, 22.0, 8.0)).into()
}

/// A titled group of rows, with an aside on the right where it has one.
fn section<'a>(look: Look, title: &'static str, aside: &'static str, rows: Vec<Element<'a, Message>>) -> Element<'a, Message> {
    let head = row![
        say(title, Type::Head, look.strong, look.text),
        Space::new().width(Fill),
        say(aside, Type::Small, look.font, look.faint),
    ]
    .align_y(Alignment::Center)
    .padding(pad(16.0, 16.0, 6.0, 16.0));
    Column::with_children(std::iter::once(head.into()).chain(rows)).into()
}

// --- The tabs ---

fn general(app: &App, look: Look) -> Vec<Element<'_, Message>> {
    let c = &app.config;
    let overlay = app.overlay;
    // Greyed in a window, which a click never dismisses and no Space switch
    // hides — on there, they read as settings in force.
    let overlay_only = |setting: Setting<'static>| {
        if overlay { setting } else { setting.enabled(false).note(quiet(look, "Overlay only.")) }
    };

    #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
    let mut summon = vec![
        setting(Field::SummonHotkey)
            .control(hotkey(app, look))
            .desc("Shows and hides the board from any app.")
            .build(app, look),
    ];
    // Raw-trackpad gesture, macOS-only (uses a private framework).
    #[cfg(target_os = "macos")]
    summon.push(
        overlay_only(
            setting(Field::SummonGesture)
                .chip(Chip::Mac)
                .control(switch(look, Field::SummonGesture, c.summon_gesture, overlay))
                .desc("Works on a laptop without a Pause key."),
        )
        .build(app, look),
    );

    let vault = if c.vault.trim().is_empty() { "None".to_owned() } else { shown_path(&c.vault, 26) };
    let boards = vec![
        setting(Field::Vault)
            .control(path_box(look, vault, Field::Vault))
            .desc(format!("{OPEN_KEYS} lists every board in it, and the board follows its Obsidian theme."))
            .build(app, look),
    ];

    // Greyed while a click dismisses, which is when it couldn't work: the
    // board is gone before the second click lands.
    let dismisses = c.click_dismisses(overlay);
    let mut double = setting(Field::DoubleClickAddsCard)
        .control(switch(look, Field::DoubleClickAddsCard, c.double_click_adds_card, !dismisses))
        .desc("Double-clicking empty space adds a card there, as Obsidian's canvas does.");
    if dismisses {
        let off = Message::Settings(Msg::Toggle(Field::DismissOnBackgroundClick, false));
        double = double
            .enabled(false)
            .note(row![quiet(look, "Needs click-to-dismiss off."), link(look, "Turn it off", off)].align_y(Alignment::Center));
    }
    let default_dim = Config::default().overlay_dim;
    #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
    let mut behind = vec![
        overlay_only(
            setting(Field::DismissOnBackgroundClick)
                .control(switch(look, Field::DismissOnBackgroundClick, c.dismiss_on_background_click, overlay))
                .desc("A click that lands on no card puts the board away."),
        )
        .build(app, look),
        double.build(app, look),
    ];
    // Only macOS watches Space/workspace changes (platform::WATCHES_WORKSPACE);
    // elsewhere the toggle is a no-op, so hide the row. The config field stays
    // cross-platform so a hand-set value copied between machines is honored.
    #[cfg(target_os = "macos")]
    behind.push(
        overlay_only(
            setting(Field::PersistAcrossSpaces)
                .chip(Chip::Mac)
                .control(switch(look, Field::PersistAcrossSpaces, c.persist_across_spaces, overlay))
                .desc("Otherwise switching Spaces dismisses it."),
        )
        .build(app, look),
    );
    behind.push(
        setting(Field::OverlayDim)
            .control(dial(look, Field::OverlayDim, c.overlay_dim, 0.0..=1.0, default_dim, percent(c.overlay_dim)))
            .desc(format!("How much the desktop darkens behind the board. Default {}.", percent(default_dim)))
            .build(app, look),
    );
    behind.push(
        setting(Field::Windowed)
            .chip(Chip::Restart)
            .control(switch(look, Field::Windowed, c.windowed, true))
            .desc("No overlay, hotkey or tray. Greys the overlay rows above.")
            .build(app, look),
    );

    vec![section(look, "Summon", "", summon), section(look, "Boards", "", boards), section(look, "Overlay", "", behind)]
}

fn appearance(app: &App, look: Look) -> Vec<Element<'_, Message>> {
    let c = &app.config;
    let font = if c.font.trim().is_empty() { "Theme default".to_owned() } else { c.font.trim().to_owned() };

    // The cost of see-through, said only while it's what you have: under
    // any other choice it was a note about something you'd already avoided.
    let mut tiles = column![backdrop_tiles(app, look)].spacing(9);
    if OverlayBackdrop::of(c) == OverlayBackdrop::Transparent {
        tiles = tiles.push(aside(look, "Transparent uses a bit more of the GPU than the other options."));
    }
    let mut backdrop = setting(Field::OverlayBackdrop)
        .wide(tiles)
        .desc("What's behind the board in overlay mode.");
    // Where the window's see-through flag can't be switched live.
    if !crate::platform::LIVE_TRANSPARENCY {
        backdrop = backdrop.chip(Chip::Restart);
    }
    #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
    let mut looks = vec![
        setting(Field::Theme)
            // A theme by its accent, the colour that most tells one from another.
            .control(chooser(look, Some(app.theme.accent), theme_name(c), Message::Settings(Msg::ChooseTheme)))
            .note(theme_chain(c, look))
            .desc("A built-in theme, one of your own, or your vault's Obsidian theme.")
            .build(app, look),
        setting(Field::Font)
            .control(chooser(look, None, font, Message::Settings(Msg::ChooseFont)))
            .desc("Body text only. Code keeps the theme's monospace.")
            .build(app, look),
        backdrop.build(app, look),
    ];
    // Template-image menu-bar icon, macOS-only (the template concept is AppKit's).
    #[cfg(target_os = "macos")]
    looks.push(
        setting(Field::TrayIconMonochrome)
            .chip(Chip::Mac)
            .control(switch(look, Field::TrayIconMonochrome, c.tray_icon_monochrome, true))
            .desc("Follows the menu bar's light or dark appearance.")
            .build(app, look),
    );

    let default_opacity = Config::default().colored_card_opacity;
    let cards = vec![
        setting(Field::LinkStyle)
            .control(link_styles(app, look))
            .desc("Obsidian curves with the distance spanned; Mermaid routes; Square turns at right angles.")
            .build(app, look),
        setting(Field::ArrowTip)
            .control(arrow_tips(app, look))
            .desc("The shape at a link's arrowed end. Which ends have one is the link's own, set from its right-click menu.")
            .build(app, look),
        setting(Field::CenterCardText)
            .control(switch(look, Field::CenterCardText, c.center_card_text, true))
            .desc("Headings, code and lists stay left-aligned.")
            .build(app, look),
        setting(Field::CardScrollbars)
            .control(switch(look, Field::CardScrollbars, c.card_scrollbars, true))
            .desc("Resize a card below its text; the rest scrolls.")
            .build(app, look),
        setting(Field::ColoredCardOpacity)
            .control(dial(
                look,
                Field::ColoredCardOpacity,
                c.colored_card_opacity,
                0.2..=1.0,
                default_opacity,
                percent(c.colored_card_opacity),
            ))
            .desc("Below 100%, what is underneath shows through.")
            .build(app, look),
        setting(Field::MaxCardWidth)
            .control(number(look, Field::MaxCardWidth, app.draft(Field::MaxCardWidth), "px", true))
            .desc("How wide a card gets by itself as you type.")
            .build(app, look),
    ];

    vec![section(look, "Look", "", looks), section(look, "Cards & links", "", cards)]
}

fn canvas_tab(app: &App, look: Look) -> Vec<Element<'_, Message>> {
    let c = &app.config;
    // The spacing waits on the switch beside it, as it means nothing until
    // something snaps to it.
    let grid = row![
        number(look, Field::SnapGridSize, app.draft(Field::SnapGridSize), "px", c.snap_to_grid),
        switch(look, Field::SnapToGrid, c.snap_to_grid, true),
    ]
    .spacing(10)
    .align_y(Alignment::Center);
    let layout = vec![
        setting(Field::SnapToGrid)
            .control(grid)
            .desc("Also on the canvas right-click menu.")
            .build(app, look),
        setting(Field::AutoLayout)
            .control(switch(look, Field::AutoLayout, c.auto_layout, true))
            .desc("Lays the linked cards out again each time one is added.")
            .build(app, look),
        setting(Field::AutoLayoutDirection)
            .control(segments(
                look,
                Field::AutoLayoutDirection,
                [("LR", "Left to right"), ("TD", "Top down")],
                &c.auto_layout_direction,
            ))
            .desc("Which way auto-layout runs.")
            .build(app, look),
    ];

    let fresh = Config::default();
    let navigation = vec![
        setting(Field::ZoomSpeed)
            .control(dial(look, Field::ZoomSpeed, c.zoom_speed, 0.2..=1.8, fresh.zoom_speed, format!("{:.1}×", c.zoom_speed)))
            .desc("Mouse wheel and trackpad pinch together.")
            .build(app, look),
        setting(Field::PanInertia)
            .control(dial(look, Field::PanInertia, c.pan_inertia, 0.0..=0.999, fresh.pan_inertia, format!("{:.2}", c.pan_inertia)))
            .desc("Coasting after a flick; 0 turns it off.")
            .build(app, look),
    ];

    vec![section(look, "Layout & grid", "", layout), section(look, "Navigation", "", navigation)]
}

fn ai(app: &App, look: Look) -> Vec<Element<'_, Message>> {
    let c = &app.config;
    // The endpoint's fields describe a provider, so they wait for one. Shown
    // under "Off" they were three boxes to fill in for nothing — one already
    // holding a model name — with no sign that none of them did anything.
    let custom = app.settings_custom_ai && !c.ai_enabled();
    let provided = c.ai_enabled() || custom;
    let preset = if custom { "Custom".to_owned() } else { current_preset(&c.ai_url) };

    let mut ask = vec![
        setting(Field::AiProvider)
            .control(providers(look, preset.clone()))
            .note(quiet(look, provider_words(&preset, &c.ai_url)))
            .build(app, look),
    ];
    if provided {
        if preset == "Custom" {
            ask.push(
                setting(Field::AiUrl)
                    .control(wide_line(look, Field::AiUrl, app.draft(Field::AiUrl), "http://localhost:1234/v1"))
                    .desc("An OpenAI-compatible base URL; /chat/completions is appended.")
                    .build(app, look),
            );
        }
        let model = row![
            line(look, Field::AiModel, app.draft(Field::AiModel), ""),
            act(look, "Choose…", Message::Settings(Msg::ChooseModel), accent(look)),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        ask.push(
            setting(Field::AiModel)
                .control(model)
                .desc("Lists what the endpoint serves, so you needn't type a name exactly.")
                .build(app, look),
        );
        // A runtime on this machine asks for no key — greyed, not locked, as
        // a local proxy can still want one.
        let local = is_local(&c.ai_url);
        ask.push(
            setting(Field::AiApiKey)
                .control(key(look, app.draft(Field::AiApiKey), local))
                .enabled(!local)
                .desc("Sent as a Bearer token. Hosted providers need one; it's kept per provider.")
                .build(app, look),
        );
    }

    let mut sections = vec![section(look, "Ask AI", "", ask)];
    if provided {
        sections.push(section(
            look,
            "Answers",
            "",
            vec![
                setting(Field::AiCardColor)
                    .control(swatches(app, look))
                    .desc("The color a new answer card takes. The struck-through one leaves it uncolored.")
                    .build(app, look),
                setting(Field::AiThinking)
                    .control(switch(look, Field::AiThinking, c.ai_thinking, true))
                    .desc("Slower and more tokens; rarely worth it for short answers.")
                    .build(app, look),
            ],
        ));
    }
    sections
}

fn advanced(app: &App, look: Look) -> Vec<Element<'_, Message>> {
    let c = &app.config;
    let backdrops: Vec<String> = OverlayBackdrop::offered().iter().map(|b| b.label().to_owned()).collect();
    let backdrop = pick_list(backdrops, Some(OverlayBackdrop::of(c).label().to_owned()), |label| {
        Message::Settings(Msg::Pick(Field::OverlayBackdrop, label))
    })
    .text_size(13)
    .text_line_height(Type::Control.line())
    .width(160)
    .padding([6, 10])
    .style(list(look));

    let performance = vec![
        setting(Field::RasterizeText)
            .control(switch(look, Field::RasterizeText, c.rasterize_text, true))
            .note(impact(look, 3, "Largest effect · trades memory for drawing time"))
            .desc("Draws card text once into bitmaps. Smoother, about 50 MB more memory on a large board.")
            .build(app, look),
        setting(Field::OverlayBackdrop)
            .control(backdrop)
            .note(impact(look, 2, "Medium · Transparent uses more GPU"))
            .desc("What's behind the board in overlay mode.")
            .build(app, look),
        setting(Field::ClusterZoom)
            .control(number(look, Field::ClusterZoom, app.draft(Field::ClusterZoom), "×", true))
            .note(impact(look, 1, "Small · large boards only"))
            .desc("Zoomed out past this, nearby cards merge into clusters. 0 never clusters.")
            .build(app, look),
        setting(Field::AnimationScale)
            .control(dial(
                look,
                Field::AnimationScale,
                c.animation_scale,
                0.0..=3.0,
                Config::default().animation_scale,
                format!("{:.1}×", c.animation_scale),
            ))
            .note(impact(look, 1, "Feel only · fewer frames, same cost"))
            .desc("Lower is faster; 0 is instant.")
            .build(app, look),
    ];

    let launch = Path::new(c.canvas.trim())
        .file_name()
        .map_or_else(|| "Welcome board".to_owned(), |name| name.to_string_lossy().into_owned());
    let files = vec![
        setting(Field::ThemeCss)
            .control(path_box(look, css_shown(c), Field::ThemeCss))
            .desc("Picked automatically from the vault. A file chosen here is kept.")
            .build(app, look),
        setting(Field::Canvas)
            .control(path_box(look, launch, Field::Canvas))
            .desc("Follows the last board you opened. Choosing one here opens it.")
            .build(app, look),
    ];

    vec![
        section(look, "Performance", "Ordered by impact", performance),
        section(look, "Files", "", files),
        file_only(look),
    ]
}

/// The foot of Advanced: how many settings only the file holds, where it is,
/// and a way to take in an edit made to it by hand.
fn file_only<'a>(look: Look) -> Element<'a, Message> {
    let path = crate::config::path(crate::app::config_path_arg().as_deref());
    let name = path
        .as_ref()
        .and_then(|path| path.file_name())
        .map_or_else(|| "config.json".to_owned(), |name| name.to_string_lossy().into_owned());
    let shown = path.map_or_else(|| "No config file here".to_owned(), |path| shown_path(&path.to_string_lossy(), 44));
    let words = column![
        say(format!("{} more in {name}", FILE_ONLY.len()), Type::Label, look.font, look.text),
        say(shown, Type::Small, look.mono, look.muted).wrapping(text::Wrapping::None),
    ]
    .spacing(4)
    .width(Fill);
    let buttons = row![
        button(say("Copy path", Type::Control, look.font, look.text))
            .padding([6, 11])
            .style(plain(look))
            .on_press(Message::Settings(Msg::CopyConfigPath)),
        button(say("Reload", Type::Control, look.font, look.text))
            .padding([6, 11])
            .style(plain(look))
            .on_press(Message::Settings(Msg::ReloadConfig)),
    ]
    .spacing(8);
    let inside: Element<'a, Message> = if look.stacked {
        column![words, buttons].spacing(10).into()
    } else {
        row![words, buttons].spacing(12).align_y(Alignment::Center).into()
    };
    container(container(inside).padding([13, 14]).width(Fill).style(move |_theme| container::Style {
        border: Border { color: Color { a: 0.16, ..look.text }, width: 1.0, radius: 10.0.into() },
        ..container::Style::default()
    }))
    .padding(pad(20.0, 16.0, 0.0, 16.0))
    .into()
}

// --- A row ---

/// What a row says about itself besides its label.
#[derive(Clone, Copy)]
enum Chip {
    /// Only on this platform.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Mac,
    /// Takes effect on the next launch.
    Restart,
}

impl Chip {
    fn view<'a>(self, look: Look) -> Element<'a, Message> {
        let (word, color, edge) = match self {
            Chip::Mac => ("macOS", look.muted, Color { a: 0.16, ..look.text }),
            Chip::Restart => ("Restart to apply", look.amber, Color { a: 0.45, ..look.amber }),
        };
        container(say(word, Type::Tiny, look.font, color))
            .padding([0, 5])
            .style(move |_theme| container::Style {
                border: Border { color: edge, width: 1.0, radius: 4.0.into() },
                ..container::Style::default()
            })
            .into()
    }
}

/// A row, assembled: see the module's notes for what each part is for.
struct Setting<'a> {
    field: Field,
    label: &'static str,
    chips: Vec<Chip>,
    control: Option<Element<'a, Message>>,
    notes: Vec<Element<'a, Message>>,
    wide: Option<Element<'a, Message>>,
    desc: Cow<'static, str>,
    enabled: bool,
}

/// A row for `field`, labelled as the settings index labels it — the form and
/// the palette say the same thing.
fn setting<'a>(field: Field) -> Setting<'a> {
    Setting {
        field,
        label: field.label(),
        chips: Vec::new(),
        control: None,
        notes: Vec::new(),
        wide: None,
        desc: Cow::Borrowed(""),
        enabled: true,
    }
}

impl<'a> Setting<'a> {
    fn chip(mut self, chip: Chip) -> Self {
        self.chips.push(chip);
        self
    }

    fn control(mut self, control: impl Into<Element<'a, Message>>) -> Self {
        self.control = Some(control.into());
        self
    }

    /// A line under the label that's always shown.
    fn note(mut self, note: impl Into<Element<'a, Message>>) -> Self {
        self.notes.push(note.into());
        self
    }

    /// Something that takes the row's whole width, under it.
    fn wide(mut self, wide: impl Into<Element<'a, Message>>) -> Self {
        self.wide = Some(wide.into());
        self
    }

    /// What the row does, shown while it's focused.
    fn desc(mut self, desc: impl Into<Cow<'static, str>>) -> Self {
        self.desc = desc.into();
        self
    }

    /// Greys the label. The control is greyed by whoever built it, since only
    /// it knows whether it can still be used.
    fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    fn build(self, app: &App, look: Look) -> Element<'a, Message> {
        let Setting { field, label, chips, control, notes, wide, desc, enabled } = self;
        let focused = app.settings_focus == Some(field);
        let changed = field.modified(&app.config);

        let mut title = Row::new()
            .spacing(7)
            .align_y(Alignment::Center)
            .push(say(label, Type::Label, look.font, if enabled { look.text } else { look.muted }));
        for chip in chips {
            title = title.push(chip.view(look));
        }
        // The changed mark sits in a gutter of its own, so labels line up
        // whether or not their setting has one — and in the label's line,
        // centred on it: pinned to the row's top, it stood above the words.
        let mark: Element<'a, Message> = if changed {
            container(Space::new().width(6).height(6))
                .style(move |_theme| container::Style {
                    background: Some(look.accent.into()),
                    border: Border { radius: 3.0.into(), ..Border::default() },
                    ..container::Style::default()
                })
                .into()
        } else {
            Space::new().width(6).height(6).into()
        };
        let title = row![container(mark).width(GUTTER), title].align_y(Alignment::Center);
        let mut lines = Column::new().spacing(3).push(title);
        for note in notes {
            lines = lines.push(indent(note));
        }
        if focused && !desc.is_empty() {
            lines = lines.push(indent(say(desc, Type::Small, look.font, look.muted)));
        }

        let mut controls = Row::new().spacing(8).align_y(Alignment::Center);
        if let Some(word) = field.reset_label().filter(|_| changed) {
            controls = controls.push(link(look, word, Message::Settings(Msg::Reset(field))));
        }
        if let Some(control) = control {
            controls = controls.push(control);
        }
        let main: Element<'a, Message> = if look.stacked {
            column![lines, indent(controls)].spacing(8).into()
        } else {
            // The strut makes every one-line row a control's height, whatever
            // its control: a switch's row was 10 px shorter than a button's.
            row![lines.width(Fill), Space::new().width(16).height(CONTROL), controls].align_y(Alignment::Center).into()
        };
        let main: Element<'a, Message> = match wide {
            Some(wide) => column![main, indent(wide)].spacing(10).into(),
            None => main,
        };

        let framed = container(main)
            .width(Fill)
            .padding(pad(10.0, 16.0, 10.0, 4.0))
            .style(move |_theme| {
                if focused {
                    container::Style {
                        background: Some(Color { a: 0.07, ..look.accent }.into()),
                        border: Border { color: Color { a: 0.55, ..look.accent }, width: 1.5, radius: 8.0.into() },
                        ..container::Style::default()
                    }
                } else {
                    container::Style::default()
                }
            });
        column![
            container(hairline(look)).padding(pad(0.0, 16.0, 0.0, 16.0)),
            mouse_area(framed).on_press(Message::Settings(Msg::Focus(field))),
        ]
        .into()
    }
}

/// The width of the gutter a changed setting's mark sits in.
const GUTTER: f32 = 12.0;

/// Past the gutter, in line with the label above.
fn indent<'a>(content: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    container(content).padding(pad(0.0, 0.0, 0.0, GUTTER)).into()
}

// --- Controls ---

fn switch<'a>(look: Look, field: Field, on: bool, enabled: bool) -> Element<'a, Message> {
    toggler(on)
        .on_toggle_maybe(enabled.then_some(move |b| Message::Settings(Msg::Toggle(field, b))))
        .size(20)
        .style(move |_theme, status| {
            let (on, live) = match status {
                toggler::Status::Active { is_toggled } | toggler::Status::Hovered { is_toggled } => (is_toggled, true),
                toggler::Status::Disabled { is_toggled } => (is_toggled, false),
            };
            let fade = |color: Color| if live { color } else { Color { a: color.a * 0.45, ..color } };
            toggler::Style {
                background: fade(if on { look.accent } else { look.track }).into(),
                background_border_width: 0.0,
                background_border_color: Color::TRANSPARENT,
                foreground: fade(if on { Color::WHITE } else { mix(look.muted, look.text, 0.4) }).into(),
                foreground_border_width: 0.0,
                foreground_border_color: Color::TRANSPARENT,
                text_color: None,
                border_radius: None,
                padding_ratio: 0.1,
            }
        })
        .into()
}

/// A slider with a readout, and a tick where the default is — so a value
/// moved off it can be put back by eye as well as by Reset.
fn dial<'a>(
    look: Look,
    field: Field,
    value: f32,
    range: RangeInclusive<f32>,
    default: f32,
    readout: String,
) -> Element<'a, Message> {
    const WIDTH: f32 = 130.0;
    const KNOB: f32 = 7.0;
    let (start, end) = (*range.start(), *range.end());
    let along = ((default - start) / (end - start)).clamp(0.0, 1.0);
    // Where iced puts the knob's centre for that value: it travels the rail
    // less its own width.
    let x = (WIDTH - 2.0 * KNOB) * along + KNOB - 1.0;
    let tick = container(container(Space::new().width(2).height(12)).style(move |_theme| fill(look.faint)))
        .padding(pad(2.0, 0.0, 0.0, x))
        .width(WIDTH)
        .height(16);
    let knob = slider(range, value, move |v| Message::Settings(Msg::Slide(field, v)))
        .on_release(Message::Settings(Msg::Commit))
        .step(0.01_f32)
        .width(WIDTH)
        .height(16)
        .style(move |_theme, _status| slider::Style {
            rail: slider::Rail {
                backgrounds: (look.accent.into(), look.track.into()),
                width: 4.0,
                border: Border { radius: 2.0.into(), ..Border::default() },
            },
            handle: slider::Handle {
                shape: slider::HandleShape::Circle { radius: KNOB },
                background: Color::WHITE.into(),
                border_width: 1.0,
                border_color: Color { a: 0.25, ..Color::BLACK },
            },
        });
    row![
        stack![tick, knob],
        say(readout, Type::Control, look.font, look.text).width(46).align_x(iced::alignment::Horizontal::Right),
    ]
    .spacing(8)
    .align_y(Alignment::Center)
    .into()
}

fn percent(value: f32) -> String {
    format!("{:.0}%", value * 100.0)
}

fn well(look: Look) -> impl Fn(&Theme, text_input::Status) -> text_input::Style {
    move |_theme, status| text_input::Style {
        background: look.well.into(),
        border: Border {
            color: if matches!(status, text_input::Status::Focused { .. }) { look.accent } else { look.line },
            width: 1.0,
            radius: 6.0.into(),
        },
        icon: look.muted,
        placeholder: look.faint,
        value: if matches!(status, text_input::Status::Disabled) { look.muted } else { look.text },
        selection: Color { a: 0.35, ..look.accent },
    }
}

/// A number, right-aligned as figures are, with its unit after it.
fn number<'a>(look: Look, field: Field, draft: &'a str, unit: &'static str, enabled: bool) -> Element<'a, Message> {
    let input = text_input("", draft)
        .on_input_maybe(enabled.then_some(move |s| Message::Settings(Msg::Edit(field, s))))
        .on_submit(Message::Settings(Msg::Commit))
        .size(13)
        .line_height(Type::Control.line())
        .font(look.font)
        .padding([6, 9])
        .width(64)
        .align_x(iced::alignment::Horizontal::Right)
        .style(well(look));
    row![input, say(unit, Type::Small, look.font, look.muted).width(18)]
        .spacing(6)
        .align_y(Alignment::Center)
        .into()
}

/// A line of text to type into.
fn line<'a>(look: Look, field: Field, draft: &'a str, placeholder: &'a str) -> Element<'a, Message> {
    text_input(placeholder, draft)
        .on_input(move |s| Message::Settings(Msg::Edit(field, s)))
        .on_submit(Message::Settings(Msg::Commit))
        .size(13)
        .line_height(Type::Control.line())
        .font(look.font)
        .padding([6, 9])
        .width(look.field)
        .style(well(look))
        .into()
}

/// A line with no button beside it, as wide as a field and its button, so
/// it starts where the fields above and below it do.
fn wide_line<'a>(look: Look, field: Field, draft: &'a str, placeholder: &'a str) -> Element<'a, Message> {
    let width = if look.stacked { look.field } else { look.field + 8.0 + BUTTON };
    text_input(placeholder, draft)
        .on_input(move |s| Message::Settings(Msg::Edit(field, s)))
        .on_submit(Message::Settings(Msg::Commit))
        .size(13)
        .line_height(Type::Control.line())
        .font(look.font)
        .padding([6, 9])
        .width(width)
        .style(well(look))
        .into()
}

/// The API key: masked, with a Paste beside it, since a key is copied from
/// the provider's site rather than typed.
fn key<'a>(look: Look, draft: &'a str, local: bool) -> Element<'a, Message> {
    let placeholder = if local { "Not needed locally" } else { "" };
    row![
        text_input(placeholder, draft)
            .secure(true)
            .on_input(|s| Message::Settings(Msg::Edit(Field::AiApiKey, s)))
            .on_submit(Message::Settings(Msg::Commit))
            .size(13)
            .line_height(Type::Control.line())
            .font(look.font)
            .padding([6, 9])
            .width(look.field)
            .style(well(look)),
        act(look, "Paste", Message::Settings(Msg::Paste(Field::AiApiKey)), plain(look)),
    ]
    .spacing(8)
    .align_y(Alignment::Center)
    .into()
}

/// A path as a well showing where it points, and a Browse… beside it. Not
/// typed into: a path is chosen, and the file is there for anyone who'd
/// rather type.
fn path_box<'a>(look: Look, shown: String, field: Field) -> Element<'a, Message> {
    let well = container(say(shown, Type::Control, look.font, look.text).wrapping(text::Wrapping::None))
        .padding([6, 9])
        .width(look.field)
        .clip(true)
        .style(move |_theme| container::Style {
            background: Some(look.well.into()),
            border: Border { color: look.line, width: 1.0, radius: 6.0.into() },
            ..container::Style::default()
        });
    row![
        well,
        act(look, "Browse…", Message::Settings(Msg::Browse(field)), plain(look)),
    ]
    .spacing(8)
    .align_y(Alignment::Center)
    .into()
}

/// What a picker has chosen, as a button that opens it.
fn chooser<'a>(look: Look, swatch: Option<Color>, value: String, press: Message) -> Element<'a, Message> {
    let mut face = Row::new().spacing(7).align_y(Alignment::Center);
    if let Some(color) = swatch {
        // Round: a square in a button's face read as an unticked checkbox.
        face = face.push(container(Space::new().width(12).height(12)).style(move |_theme| container::Style {
            background: Some(color.into()),
            border: Border { color: Color { a: 0.35, ..look.text }, width: 1.0, radius: 6.0.into() },
            ..container::Style::default()
        }));
    }
    face = face
        .push(say(value, Type::Control, look.font, look.text).wrapping(text::Wrapping::None))
        .push(Space::new().width(Fill))
        .push(say("Change…", Type::Small, look.font, look.muted));
    button(face).width(if look.stacked { look.field + 80.0 } else { 196.0 }).padding([6, 10]).style(plain(look)).on_press(press).into()
}

fn hotkey<'a>(app: &App, look: Look) -> Element<'a, Message> {
    if app.hotkey_recording {
        // As tall as the keycap it stands in for, so the row doesn't move.
        return container(say("Press a shortcut…  (Esc to cancel)", Type::Control, look.font, look.muted))
            .height(CONTROL)
            .center_y(CONTROL)
            .into();
    }
    let current = app.config.summon_hotkey.trim();
    let shown = if current.is_empty() { "None".to_owned() } else { current.to_owned() };
    let cap = container(say(shown, Type::Small, look.mono, look.text))
        .padding([0, 8])
        .height(CONTROL)
        .center_y(CONTROL)
        .style(move |_theme| container::Style {
            border: Border { color: Color { a: 0.2, ..look.text }, width: 1.0, radius: 5.0.into() },
            ..container::Style::default()
        });
    row![
        cap,
        act(look, "Record…", Message::RecordHotkey, plain(look)),
    ]
    .spacing(8)
    .align_y(Alignment::Center)
    .into()
}

/// Two or three choices side by side, the chosen one raised.
fn segments<'a>(look: Look, field: Field, options: [(&'static str, &'static str); 2], chosen: &str) -> Element<'a, Message> {
    let mut bar = Row::new();
    for (value, label) in options {
        let on = chosen == value;
        bar = bar.push(
            button(say(label, Type::Control, look.font, if on { look.text } else { look.muted }))
                .padding([4, 10])
                .style(move |_theme, _status| button::Style {
                    background: on.then(|| mix(look.control, look.text, 0.06).into()),
                    border: Border { radius: 5.0.into(), ..Border::default() },
                    ..button::Style::default()
                })
                .on_press(Message::Settings(Msg::Pick(field, value.to_owned()))),
        );
    }
    container(bar)
        .padding(2)
        .style(move |_theme| container::Style {
            background: Some(look.well.into()),
            border: Border { color: look.line, width: 1.0, radius: 7.0.into() },
            ..container::Style::default()
        })
        .into()
}

/// The answer colour, as the colours themselves: none, then Obsidian's six.
fn swatches<'a>(app: &App, look: Look) -> Element<'a, Message> {
    let chosen = app.config.ai_card_color.clone();
    // "No colour" is a card's own fill, as the card menu's first swatch is —
    // struck through, the sign for "none" an editor's colour well uses, or it
    // read as one more dark colour rather than as the way to have none.
    let plain_card = app.theme.card_fill;
    let mut bar = Row::new().spacing(4).align_y(Alignment::Center);
    for code in ["0", "1", "2", "3", "4", "5", "6"] {
        let color = if code == "0" {
            plain_card
        } else {
            model::parse_color(code).map(IntoColor::into_color).unwrap_or(plain_card)
        };
        let on = chosen == code;
        let face: Element<'a, Message> = if code == "0" {
            canvas::Canvas::new(Strike).width(18).height(18).into()
        } else {
            Space::new().width(18).height(18).into()
        };
        let disc = container(face).style(move |_theme| container::Style {
            background: Some(color.into()),
            border: Border { color: Color { a: 0.2, ..look.text }, width: if on { 0.0 } else { 1.0 }, radius: 9.0.into() },
            ..container::Style::default()
        });
        let ring = container(disc).padding(2).style(move |_theme| container::Style {
            border: Border { color: if on { look.text } else { Color::TRANSPARENT }, width: 1.5, radius: 12.0.into() },
            ..container::Style::default()
        });
        bar = bar.push(
            button(ring)
                .padding(0)
                .style(bare(look))
                .on_press(Message::Settings(Msg::Pick(Field::AiCardColor, code.to_owned()))),
        );
    }
    bar.into()
}

/// The stroke across the "no colour" swatch.
struct Strike;

impl canvas::Program<Message> for Strike {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        // Corner to corner of the disc, stopping inside its edge.
        let stroke = Pen::line(Point::new(4.0, 14.0), Point::new(14.0, 4.0));
        frame.stroke(&stroke, Stroke::default().with_color(crate::app::chrome::DANGER).with_width(1.5));
        vec![frame.into_geometry()]
    }
}

/// The three link styles, each drawn rather than named.
fn link_styles<'a>(app: &App, look: Look) -> Element<'a, Message> {
    let current = LinkStyle::of(&app.config);
    let mut bar = Row::new().spacing(6);
    for style in LinkStyle::ALL {
        let on = style == current;
        let sketch = container(canvas::Canvas::new(Sketch { style, on, look }).width(64).height(34)).style(move |_theme| {
            container::Style {
                background: Some(if on { Color { a: 0.1, ..look.accent } } else { look.well }.into()),
                border: Border {
                    color: if on { look.accent } else { look.line },
                    width: if on { 1.5 } else { 1.0 },
                    radius: 6.0.into(),
                },
                ..container::Style::default()
            }
        });
        let caption = say(style.label(), Type::Small, look.font, if on { look.text } else { look.muted });
        bar = bar.push(
            button(column![sketch, caption].spacing(4).align_x(Alignment::Center))
                .padding(0)
                .style(bare(look))
                .on_press(Message::Settings(Msg::Pick(Field::LinkStyle, style.label().to_owned()))),
        );
    }
    bar.into()
}

/// The two arrow tips, each drawn rather than named — beside the link
/// styles, and the same size, since the two choices are read together.
fn arrow_tips<'a>(app: &App, look: Look) -> Element<'a, Message> {
    let current = ArrowTip::of(&app.config);
    let mut bar = Row::new().spacing(6);
    for tip in ArrowTip::ALL {
        let on = tip == current;
        let sketch = container(canvas::Canvas::new(TipSketch { tip, on, look }).width(64).height(34)).style(move |_theme| {
            container::Style {
                background: Some(if on { Color { a: 0.1, ..look.accent } } else { look.well }.into()),
                border: Border {
                    color: if on { look.accent } else { look.line },
                    width: if on { 1.5 } else { 1.0 },
                    radius: 6.0.into(),
                },
                ..container::Style::default()
            }
        });
        let caption = say(tip.label(), Type::Small, look.font, if on { look.text } else { look.muted });
        bar = bar.push(
            button(column![sketch, caption].spacing(4).align_x(Alignment::Center))
                .padding(0)
                .style(bare(look))
                .on_press(Message::Settings(Msg::Pick(Field::ArrowTip, tip.label().to_owned()))),
        );
    }
    bar.into()
}

/// An arrow tip in miniature: a link running into the edge of a card.
struct TipSketch {
    tip: ArrowTip,
    on: bool,
    look: Look,
}

impl canvas::Program<Message> for TipSketch {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<Geometry> {
        let look = self.look;
        let mut frame = Frame::new(renderer, bounds.size());
        let card = if self.on { look.faint } else { mix(look.faint, look.panel, 0.35) };
        let ink = if self.on { look.text } else { look.muted };
        // The card's edge, and the point stopping just short of it.
        frame.fill(&Pen::rectangle(Point::new(52.0, 7.0), Size::new(8.0, 20.0)), card);
        let point = Point::new(49.0, 17.0);
        const HEAD: f32 = 8.0;
        match self.tip {
            ArrowTip::Chevron => {
                frame.stroke(&Pen::line(Point::new(8.0, 17.0), point), Stroke::default().with_color(ink).with_width(1.6));
                let wings = Pen::new(|b| {
                    b.move_to(Point::new(point.x - HEAD * 0.707, point.y - HEAD * 0.707));
                    b.line_to(point);
                    b.line_to(Point::new(point.x - HEAD * 0.707, point.y + HEAD * 0.707));
                });
                frame.stroke(&wings, Stroke::default().with_color(ink).with_width(1.6));
            }
            // In the board's proportions: longer than a chevron, half as
            // wide either side as it is long.
            ArrowTip::Triangle => {
                let length = HEAD * 1.625;
                frame.stroke(
                    &Pen::line(Point::new(8.0, 17.0), Point::new(point.x - length, 17.0)),
                    Stroke::default().with_color(ink).with_width(1.6),
                );
                let head = Pen::new(|b| {
                    b.move_to(point);
                    b.line_to(Point::new(point.x - length, point.y - length * 0.5));
                    b.line_to(Point::new(point.x - length, point.y + length * 0.5));
                    b.close();
                });
                frame.fill(&head, ink);
            }
        }
        vec![frame.into_geometry()]
    }
}

/// A link style in miniature: two ports and the line between them, as
/// `Board::link_segments` would route it.
struct Sketch {
    style: LinkStyle,
    on: bool,
    look: Look,
}

impl canvas::Program<Message> for Sketch {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<Geometry> {
        let look = self.look;
        let mut frame = Frame::new(renderer, bounds.size());
        let port = if self.on { look.faint } else { mix(look.faint, look.panel, 0.35) };
        for at in [Point::new(2.0, 21.0), Point::new(54.0, 5.0)] {
            frame.fill(&Pen::rectangle(at, Size::new(8.0, 8.0)), port);
        }
        let p = Point::new;
        let route = Pen::new(|b| {
            b.move_to(p(10.0, 25.0));
            match self.style {
                LinkStyle::Obsidian => b.bezier_curve_to(p(34.0, 25.0), p(30.0, 9.0), p(54.0, 9.0)),
                LinkStyle::Mermaid => {
                    b.line_to(p(18.0, 25.0));
                    b.quadratic_curve_to(p(22.0, 25.0), p(26.0, 20.0));
                    b.line_to(p(38.0, 14.0));
                    b.quadratic_curve_to(p(42.0, 9.0), p(46.0, 9.0));
                    b.line_to(p(54.0, 9.0));
                }
                LinkStyle::Square => {
                    b.line_to(p(32.0, 25.0));
                    b.line_to(p(32.0, 9.0));
                    b.line_to(p(54.0, 9.0));
                }
            }
        });
        let ink = if self.on { look.text } else { look.muted };
        frame.stroke(&route, Stroke::default().with_color(ink).with_width(1.6));
        vec![frame.into_geometry()]
    }
}

/// The overlay background's choices as tiles, each showing what it gives:
/// the wallpaper and your own picture as they'd be shown, blurred as they'd
/// be — a sample picture until one's ready — the theme's colour, and the
/// checkerboard an editor shows for see-through.
fn backdrop_tiles<'a>(app: &App, look: Look) -> Element<'a, Message> {
    const HEIGHT: f32 = 52.0;
    let chosen = OverlayBackdrop::of(&app.config);
    let board = app.theme.background;
    let mut tiles = Row::new().spacing(8);
    for backdrop in OverlayBackdrop::offered() {
        let on = backdrop == chosen;
        let picture = match backdrop {
            OverlayBackdrop::Wallpaper => app.backdrop_preview(&BackdropPicture::Wallpaper),
            OverlayBackdrop::Image => {
                let path = std::path::PathBuf::from(app.config.overlay_image.trim());
                app.backdrop_preview(&BackdropPicture::File(path))
            }
            OverlayBackdrop::Transparent | OverlayBackdrop::Theme => None,
        };
        let shown: Element<'a, Message> = match picture {
            Some(handle) => iced::widget::image(handle.clone())
                .width(Fill)
                .height(HEIGHT)
                .content_fit(iced::ContentFit::Cover)
                .border_radius(5.0)
                .into(),
            None => canvas::Canvas::new(Preview { backdrop, look, board }).width(Fill).height(HEIGHT).into(),
        };
        // The same inset for every tile, the border drawn within it: inset
        // by its border, the chosen one stood 2 px taller than its row.
        let edge = if on { 2.0 } else { 1.0 };
        let framed = container(shown).padding(2).width(Fill).style(move |_theme| container::Style {
            border: Border { color: if on { look.accent } else { look.line }, width: edge, radius: 7.0.into() },
            ..container::Style::default()
        });
        let (title, note) = tile_words(backdrop, &app.config);
        let face = column![
            framed,
            say(title, Type::Control, look.font, look.text),
            say(note, Type::Small, look.font, look.muted).wrapping(text::Wrapping::None),
        ]
        .spacing(3);
        tiles = tiles.push(
            button(face)
                .width(Fill)
                .padding(0)
                .style(bare(look))
                .on_press(Message::Settings(Msg::Backdrop(backdrop))),
        );
    }
    tiles.into()
}

fn tile_words(backdrop: OverlayBackdrop, config: &Config) -> (&'static str, String) {
    match backdrop {
        OverlayBackdrop::Transparent => ("Transparent", "Live desktop".to_owned()),
        OverlayBackdrop::Theme => ("Theme color", "Opaque".to_owned()),
        OverlayBackdrop::Wallpaper => ("Wallpaper", "Blurred · macOS".to_owned()),
        // Named once there's one, so the tile says which picture it is.
        OverlayBackdrop::Image => {
            let name = Path::new(config.overlay_image.trim())
                .file_name()
                .map(|name| clip(&name.to_string_lossy(), 14))
                .unwrap_or_else(|| "Choose…".to_owned());
            ("Image", format!("{name} · macOS"))
        }
    }
}

/// A backdrop tile with no picture to show: the checkerboard an editor shows
/// for see-through, the theme's colour filling the tile, and — for a picture
/// not ready, or not chosen yet — a sample one, a hillside at dusk, so the
/// tile still reads as "a picture" rather than as a pattern.
struct Preview {
    backdrop: OverlayBackdrop,
    look: Look,
    board: Color,
}

impl canvas::Program<Message> for Preview {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<Geometry> {
        let look = self.look;
        let size = bounds.size();
        let mut frame = Frame::new(renderer, size);
        match self.backdrop {
            OverlayBackdrop::Transparent => {
                const SQUARE: f32 = 6.0;
                frame.fill_rectangle(Point::ORIGIN, size, look.well);
                let mut y = 0.0;
                let mut odd = false;
                while y < size.height {
                    let mut x = if odd { SQUARE } else { 0.0 };
                    while x < size.width {
                        frame.fill_rectangle(Point::new(x, y), Size::new(SQUARE, SQUARE), look.control);
                        x += 2.0 * SQUARE;
                    }
                    y += SQUARE;
                    odd = !odd;
                }
            }
            OverlayBackdrop::Theme => frame.fill_rectangle(Point::ORIGIN, size, self.board),
            OverlayBackdrop::Wallpaper | OverlayBackdrop::Image => sample_picture(&mut frame, size),
        }
        vec![frame.into_geometry()]
    }
}

/// A hillside at dusk: a sky going from blue to amber, a low sun, and two
/// ridges in front of it.
fn sample_picture(frame: &mut Frame, size: Size) {
    let rgb = |hex: u32| Color::from_rgb8((hex >> 16) as u8, (hex >> 8) as u8, hex as u8);
    let (w, h) = (size.width, size.height);
    let sky = canvas::gradient::Linear::new(Point::ORIGIN, Point::new(0.0, h))
        .add_stop(0.0, rgb(0x4a6fa5))
        .add_stop(0.65, rgb(0xd99a6c))
        .add_stop(1.0, rgb(0xf2c48d));
    frame.fill_rectangle(Point::ORIGIN, size, sky);
    frame.fill(&Pen::circle(Point::new(w * 0.68, h * 0.62), h * 0.16), rgb(0xfde2b0));
    let ridge = |frame: &mut Frame, base: f32, peak: (f32, f32), color: Color| {
        let path = Pen::new(|b| {
            b.move_to(Point::new(0.0, h * base));
            b.quadratic_curve_to(Point::new(w * peak.0, h * peak.1), Point::new(w, h * (base + 0.05)));
            b.line_to(Point::new(w, h));
            b.line_to(Point::new(0.0, h));
            b.close();
        });
        frame.fill(&path, color);
    };
    ridge(frame, 0.7, (0.3, 0.35), rgb(0x6b6f8f));
    ridge(frame, 0.85, (0.75, 0.55), rgb(0x3b3f57));
}

fn providers<'a>(look: Look, chosen: String) -> Element<'a, Message> {
    let options: Vec<String> = std::iter::once(PROVIDER_OFF.to_owned())
        .chain(crate::ai::PRESETS.iter().map(|p| p.name.to_owned()))
        .chain(std::iter::once("Custom".to_owned()))
        .collect();
    pick_list(options, Some(chosen), |name| Message::Settings(Msg::PickPreset(name)))
        .text_size(13)
        .text_line_height(Type::Control.line())
        .width(160)
        .padding([6, 10])
        .style(list(look))
        .into()
}

/// What a provider is, under its name: whether it wants a key, and where it is.
fn provider_words(preset: &str, url: &str) -> String {
    let place = url.trim().split_once("://").map_or(url.trim(), |(_, rest)| rest).trim_end_matches('/');
    match preset {
        PROVIDER_OFF => "Off: Ask AI stays hidden. A local provider like LM Studio needs no key.".to_owned(),
        "Custom" => "Any OpenAI-compatible endpoint.".to_owned(),
        _ if is_local(url) => format!("Local, no key needed · {place}"),
        _ => format!("Hosted, needs an API key · {place}"),
    }
}

/// How much a performance setting matters, in three bars and a few words.
fn impact<'a>(look: Look, level: usize, words: &'static str) -> Element<'a, Message> {
    let mut bars = Row::new().spacing(2).align_y(Alignment::End);
    for n in 1..=3 {
        let lit = n <= level;
        bars = bars.push(container(Space::new().width(4).height(10)).style(move |_theme| container::Style {
            background: Some(if lit { look.amber } else { look.track }.into()),
            border: Border { radius: 1.0.into(), ..Border::default() },
            ..container::Style::default()
        }));
    }
    row![bars, say(words, Type::Small, look.font, look.muted)]
        .spacing(6)
        .align_y(Alignment::Center)
        .into()
}

/// Which source the theme comes from, of the three in the order they're
/// tried — `theme_store::resolve_base`'s precedence, made visible.
fn theme_chain<'a>(config: &Config, look: Look) -> Element<'a, Message> {
    let named = config.theme.trim();
    let css = config.theme_css.as_deref().map(str::trim).filter(|css| !css.is_empty());
    let steps = [
        (if named.is_empty() { "Named theme".to_owned() } else { format!("Named: {named}") }, !named.is_empty()),
        (css.map_or_else(|| "Obsidian CSS".to_owned(), |css| css_label(css, config)), named.is_empty() && css.is_some()),
        ("Built-in".to_owned(), named.is_empty() && css.is_none()),
    ];
    let mut chain = Row::new()
        .spacing(5)
        .align_y(Alignment::Center)
        .push(say("Uses", Type::Small, look.font, look.faint));
    for (i, (words, active)) in steps.into_iter().enumerate() {
        if i > 0 {
            chain = chain.push(say("›", Type::Small, look.font, look.faint));
        }
        chain = chain.push(if active {
            Element::from(container(say(words, Type::Small, look.font, look.text)).padding([1, 6]).style(
                move |_theme| container::Style {
                    background: Some(look.track.into()),
                    border: Border { radius: 4.0.into(), ..Border::default() },
                    ..container::Style::default()
                },
            ))
        } else {
            say(words, Type::Small, look.font, look.faint).into()
        });
    }
    chain.wrap().into()
}

/// A stylesheet by where it came from: the vault's theme by its name, one
/// picked by hand by its file.
fn css_label(css: &str, config: &Config) -> String {
    if hand_picked_css(config) {
        let file = Path::new(css).file_name().map_or_else(|| css.to_owned(), |f| f.to_string_lossy().into_owned());
        format!("CSS: {file}")
    } else {
        format!("Vault: {}", vault_theme_name(css))
    }
}

/// The name a vault's theme goes by: its folder, `.obsidian/themes/<name>/`.
fn vault_theme_name(css: &str) -> String {
    Path::new(css)
        .parent()
        .and_then(Path::file_name)
        .map_or_else(|| "theme".to_owned(), |name| name.to_string_lossy().into_owned())
}

/// The theme in use, by name — whichever source it came from.
fn theme_name(config: &Config) -> String {
    let named = config.theme.trim();
    if !named.is_empty() {
        return named.to_owned();
    }
    match config.theme_css.as_deref().map(str::trim).filter(|css| !css.is_empty()) {
        Some(css) if hand_picked_css(config) => {
            Path::new(css).file_stem().map_or_else(|| css.to_owned(), |stem| stem.to_string_lossy().into_owned())
        }
        Some(css) => vault_theme_name(css),
        None => "ThinkBoard Dark".to_owned(),
    }
}

/// The Theme CSS row's well: where the stylesheet came from.
fn css_shown(config: &Config) -> String {
    match config.theme_css.as_deref().map(str::trim).filter(|css| !css.is_empty()) {
        Some(css) if hand_picked_css(config) => {
            Path::new(css).file_name().map_or_else(|| css.to_owned(), |f| f.to_string_lossy().into_owned())
        }
        Some(css) => format!("From vault: {}", vault_theme_name(css)),
        None => "None".to_owned(),
    }
}

/// A path as the form shows it: home as `~`, and a long one cut from the
/// left, since the end is the part that tells two apart.
fn shown_path(path: &str, room: usize) -> String {
    let path = path.trim();
    let home = std::env::var("HOME").ok().filter(|home| !home.is_empty() && path.starts_with(home.as_str()));
    let path = match home {
        Some(home) => format!("~{}", &path[home.len()..]),
        None => path.to_owned(),
    };
    let count = path.chars().count();
    if count <= room {
        path
    } else {
        let tail: String = path.chars().skip(count + 1 - room).collect();
        format!("…{tail}")
    }
}

/// `words` cut to `room` characters, with an ellipsis if it was cut.
fn clip(words: &str, room: usize) -> String {
    if words.chars().count() <= room {
        words.to_owned()
    } else {
        let head: String = words.chars().take(room - 1).collect();
        format!("{head}…")
    }
}

/// Small muted text: a note under a label.
fn quiet<'a>(look: Look, words: impl text::IntoFragment<'a>) -> Element<'a, Message> {
    say(words, Type::Small, look.font, look.muted).into()
}

/// A note with an ⓘ before it, for a fact about the choices rather than a
/// warning against one.
fn aside<'a>(look: Look, words: &'static str) -> Element<'a, Message> {
    let mark = container(
        text("i")
            .size(10)
            .line_height(text::LineHeight::Absolute(14.0.into()))
            .font(Font { style: iced::font::Style::Italic, ..look.strong })
            .color(look.muted),
    )
        .width(14)
        .height(14)
        .center_x(14)
        .center_y(14)
        .style(move |_theme| container::Style {
            border: Border { color: Color { a: 0.22, ..look.text }, width: 1.0, radius: 7.0.into() },
            ..container::Style::default()
        });
    row![mark, say(words, Type::Small, look.font, look.muted)]
        .spacing(7)
        .align_y(Alignment::Center)
        .into()
}

/// A button beside a field, its word centred: one width for all of them.
fn act<'a>(
    look: Look,
    word: &'static str,
    press: Message,
    face: impl Fn(&Theme, button::Status) -> button::Style + 'a,
) -> Element<'a, Message> {
    // No colour of its own, so it takes the face's — the accent's text on
    // the accent button.
    let (size, _) = Type::Control.metrics();
    let word = text(word).size(size).line_height(Type::Control.line()).font(look.font);
    button(container(word).center_x(Fill)).width(BUTTON).padding([6, 0]).style(face).on_press(press).into()
}

/// An action as a word in the accent, for a row's Reset and the like.
fn link<'a>(look: Look, word: &'static str, press: Message) -> Element<'a, Message> {
    button(say(word, Type::Small, look.font, look.accent))
        .padding([0, 4])
        .style(bare(look))
        .on_press(press)
        .into()
}

// --- Button and list faces ---

fn plain(look: Look) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_theme, status| button::Style {
        background: Some(
            match status {
                button::Status::Hovered | button::Status::Pressed => mix(look.control, look.text, 0.08),
                _ => look.control,
            }
            .into(),
        ),
        text_color: if matches!(status, button::Status::Disabled) { look.faint } else { look.text },
        border: Border { radius: 6.0.into(), ..Border::default() },
        ..button::Style::default()
    }
}

fn accent(look: Look) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_theme, status| button::Style {
        background: Some(
            match status {
                button::Status::Hovered | button::Status::Pressed => mix(look.accent, Color::WHITE, 0.12),
                _ => look.accent,
            }
            .into(),
        ),
        text_color: look.on_accent,
        border: Border { radius: 6.0.into(), ..Border::default() },
        ..button::Style::default()
    }
}

/// No face at all: for what draws its own (tabs, tiles, swatches, links).
fn bare(look: Look) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_theme, _status| button::Style { background: None, text_color: look.text, ..button::Style::default() }
}

fn list(look: Look) -> impl Fn(&Theme, pick_list::Status) -> pick_list::Style {
    move |_theme, status| pick_list::Style {
        text_color: look.text,
        placeholder_color: look.faint,
        handle_color: look.muted,
        background: match status {
            pick_list::Status::Hovered | pick_list::Status::Opened { .. } => mix(look.control, look.text, 0.08),
            pick_list::Status::Active => look.control,
        }
        .into(),
        border: Border { radius: 6.0.into(), ..Border::default() },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_path_keeps_its_end() {
        assert_eq!(shown_path("/a/b", 10), "/a/b");
        let shown = shown_path("/very/long/path/to/the/Notes", 10);
        assert_eq!(shown.chars().count(), 10);
        assert!(shown.ends_with("/Notes"), "{shown}");
    }

    #[test]
    fn a_stylesheet_is_named_for_where_it_came_from() {
        let vault = Config {
            vault: "/notes".to_owned(),
            theme_css: Some("/notes/.obsidian/themes/Minimal/theme.css".to_owned()),
            ..Config::default()
        };
        assert_eq!(theme_name(&vault), "Minimal");
        assert_eq!(css_shown(&vault), "From vault: Minimal");
        let picked = Config { theme_css: Some("/elsewhere/primary.css".to_owned()), ..vault.clone() };
        assert_eq!(theme_name(&picked), "primary");
        assert_eq!(css_shown(&picked), "primary.css");
        // A named theme wins, as it does when the theme is resolved.
        let named = Config { theme: "Nord".to_owned(), ..vault };
        assert_eq!(theme_name(&named), "Nord");
    }

    #[test]
    fn a_provider_says_whether_it_wants_a_key() {
        assert!(provider_words("LM Studio", "http://localhost:1234/v1").starts_with("Local, no key needed"));
        assert!(provider_words("OpenAI", "https://api.openai.com/v1").contains("needs an API key · api.openai.com/v1"));
        assert!(provider_words(PROVIDER_OFF, "").starts_with("Off"));
    }
}
