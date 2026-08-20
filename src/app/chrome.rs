//! Shared visual style for the app's floating overlay surfaces — the command
//! palette, the settings modal, the context menu, and the find bar — so they
//! read as one designed system rather than four independently-styled panels:
//! the same panel radius/border/shadow, the same accent-based hover, all themed
//! from the loaded Obsidian [`ColorTheme`] (the overlay window itself stays on
//! `Theme::Dark`, so colors are captured from the theme into `move` closures
//! rather than read from the widget's `&iced::Theme`).

use iced::widget::{button, container};
use iced::{Background, Border, Color, Shadow, Vector};

use crate::theme::Theme as ColorTheme;

/// Corner radius of a floating panel (palette, menu, find bar, settings modal).
pub(super) const PANEL_RADIUS: f32 = 10.0;
/// Corner radius of a control inside a panel (rows, inputs, buttons, chips).
pub(super) const CONTROL_RADIUS: f32 = 6.0;
/// Corner radius of the small square controls — colour swatches, the hue wheel,
/// the settings pills. Tighter than a full control so a 24px square still reads
/// as a square rather than as a lozenge.
pub(super) const SWATCH_RADIUS: f32 = 4.0;
/// Destructive-action tint (Delete), consistent across every menu.
pub(super) const DANGER: Color = Color { r: 1.0, g: 0.44, b: 0.44, a: 1.0 };

/// The soft drop shadow every floating panel casts over the board, so the
/// surfaces lift off the canvas consistently.
fn panel_shadow() -> Shadow {
    Shadow {
        color: Color { r: 0.0, g: 0.0, b: 0.0, a: 0.35 },
        offset: Vector::new(0.0, 4.0),
        blur_radius: 16.0,
    }
}

/// A floating panel surface: a raised card with a themed hairline border and a
/// soft shadow. `surface` is the fill — usually `theme.card_fill`; the settings
/// modal passes `theme.background` so the panel meets its form seamlessly.
pub(super) fn panel(theme: &ColorTheme, surface: Color) -> container::Style {
    container::Style {
        background: Some(surface.into()),
        border: Border { radius: PANEL_RADIUS.into(), width: 1.0, color: theme.border },
        text_color: Some(theme.text),
        shadow: panel_shadow(),
        ..container::Style::default()
    }
}

/// A menu/list row: transparent until hovered or selected, then an accent wash.
/// Shared by the command-palette rows and the context-menu items so both feel
/// identical. Disabled rows dim their text.
pub(super) fn menu_item(theme: &ColorTheme, status: button::Status, selected: bool) -> button::Style {
    use button::Status;
    let hovered = matches!(status, Status::Hovered | Status::Pressed);
    let background = if selected {
        Some(Background::Color(Color { a: 0.22, ..theme.accent }))
    } else if hovered {
        Some(Background::Color(Color { a: 0.12, ..theme.accent }))
    } else {
        None
    };
    button::Style {
        background,
        text_color: match status {
            Status::Disabled => Color { a: 0.35, ..theme.text },
            _ => theme.text,
        },
        border: Border { radius: CONTROL_RADIUS.into(), ..Border::default() },
        ..button::Style::default()
    }
}

/// A destructive menu item (Delete): danger-tinted text with a red wash on hover.
pub(super) fn danger_item(_theme: &ColorTheme, status: button::Status) -> button::Style {
    use button::Status;
    let hovered = matches!(status, Status::Hovered | Status::Pressed);
    button::Style {
        background: hovered.then_some(Background::Color(Color { a: 0.18, ..DANGER })),
        text_color: DANGER,
        border: Border { radius: CONTROL_RADIUS.into(), ..Border::default() },
        ..button::Style::default()
    }
}

/// A thin divider between groups of menu items, tinted from the theme border.
pub(super) fn separator(theme: &ColorTheme) -> container::Style {
    container::Style {
        background: Some(Color { a: 0.6, ..theme.border }.into()),
        ..container::Style::default()
    }
}
