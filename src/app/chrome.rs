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

/// The floating surface the find bar set and the panels follow: the raised
/// card, ringed in the accent and lifted by a deeper shadow than a menu's — it
/// is where the keyboard is, and the ring says so.
pub(super) fn accent_panel(theme: &ColorTheme) -> container::Style {
    container::Style {
        background: Some(theme.elevated().into()),
        border: Border { color: Color { a: 0.45, ..theme.accent }, width: 1.0, radius: PANEL_RADIUS.into() },
        text_color: Some(theme.text),
        shadow: Shadow { color: Color { a: 0.45, ..Color::BLACK }, offset: Vector::new(0.0, 10.0), blur_radius: 28.0 },
        ..container::Style::default()
    }
}

/// A rule inside a panel: the text colour, barely there, so it reads on any
/// surface the theme gives.
pub(super) fn hairline(theme: &ColorTheme) -> container::Style {
    container::Style { background: Some(Color { a: 0.08, ..theme.text }.into()), ..container::Style::default() }
}

/// A row in one of the type-to-narrow panels. The highlight is a wash *and* a
/// ring, so it holds against a hovered row beside it; hover alone is the wash,
/// fainter.
pub(super) fn list_row(theme: &ColorTheme, status: button::Status, selected: bool) -> button::Style {
    use button::Status;
    let hovered = matches!(status, Status::Hovered | Status::Pressed);
    let (background, ring) = if selected {
        (Some(Color { a: 0.16, ..theme.accent }), Color { a: 0.35, ..theme.accent })
    } else {
        (hovered.then_some(Color { a: 0.08, ..theme.accent }), Color::TRANSPARENT)
    };
    button::Style {
        background: background.map(Background::Color),
        border: Border { radius: CONTROL_RADIUS.into(), width: 1.0, color: ring },
        ..button::Style::default()
    }
}

/// A key drawn as a key: one cap per key, so a chord reads as the keys to
/// press rather than as a word.
pub(super) fn keycap(theme: &ColorTheme) -> container::Style {
    container::Style {
        background: Some(Color { a: 0.06, ..theme.text }.into()),
        border: Border { radius: 5.0.into(), width: 1.0, color: Color { a: 0.08, ..theme.text } },
        ..container::Style::default()
    }
}

/// `a` taken `t` of the way to `b`, alpha and all.
pub(super) fn mix(a: Color, b: Color, t: f32) -> Color {
    Color {
        r: a.r + (b.r - a.r) * t,
        g: a.g + (b.g - a.g) * t,
        b: a.b + (b.b - a.b) * t,
        a: a.a + (b.a - a.a) * t,
    }
}

/// The small line drawings that open a floating bar: the find bar's
/// magnifier, and the prompt (`>_`) the command palette opens with.
#[derive(Clone, Copy)]
pub(super) enum Glyph {
    Magnifier,
    Prompt,
}

/// A [`Glyph`], 14 px square, in one colour — a canvas, since the UI font has
/// no such marks and a font that did would be one more to ship.
pub(super) struct Icon {
    pub glyph: Glyph,
    pub color: Color,
}

impl<Message> iced::widget::canvas::Program<Message> for Icon {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &iced::Renderer,
        _theme: &iced::Theme,
        bounds: iced::Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<iced::widget::canvas::Geometry> {
        use iced::Point;
        use iced::widget::canvas::{Frame, LineCap, LineJoin, Path, Stroke};
        let mut frame = Frame::new(renderer, bounds.size());
        let stroke = Stroke::default().with_color(self.color).with_width(1.5);
        match self.glyph {
            Glyph::Magnifier => {
                frame.stroke(&Path::circle(Point::new(6.0, 6.0), 4.5), stroke);
                frame.stroke(&Path::line(Point::new(9.5, 9.5), Point::new(13.0, 13.0)), stroke);
            }
            Glyph::Prompt => {
                let stroke = stroke.with_line_cap(LineCap::Round).with_line_join(LineJoin::Round);
                let chevron = Path::new(|p| {
                    p.move_to(Point::new(3.0, 4.5));
                    p.line_to(Point::new(6.0, 7.0));
                    p.line_to(Point::new(3.0, 9.5));
                });
                frame.stroke(&chevron, stroke);
                frame.stroke(&Path::line(Point::new(7.5, 10.0), Point::new(11.0, 10.0)), stroke);
            }
        }
        vec![frame.into_geometry()]
    }
}

/// A menu/list row: transparent until hovered or selected, then an accent wash.
/// The context menu's items; the panels' rows are [`list_row`]. Disabled rows
/// dim their text.
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
