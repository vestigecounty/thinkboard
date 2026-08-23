//! The shell shared by the app's two type-to-narrow panels — the command
//! palette and the vault browser: a focused input over a scrollable list of
//! rows, with the same keyboard navigation and the same floating chrome.
//!
//! Only the shell lives here. What fills the rows is the caller's business:
//! the palette filters a static registry of actions, the browser ranks the
//! canvases found in the vault, and neither knows about the other.
//!
//! Rows are a **fixed height**, which is what lets [`navigate`] keep the
//! highlight on screen: iced reports a scroll position but won't tell us where
//! a particular child ended up, so the offset of row *n* has to be arithmetic.
//! The height is applied to the row rather than assumed of it, so the two can't
//! drift apart.

use std::borrow::Cow;
use std::sync::LazyLock;

use iced::advanced::widget::Id;
use iced::advanced::widget::operation::scrollable::AbsoluteOffset;
use iced::widget::{button, column, container, mouse_area, row, scrollable, text, text_input};
use iced::{Alignment, Background, Border, Color, Element, Length, Task};

use super::App;
use crate::Message;

/// Height of one row, enforced on the button below.
const ROW_HEIGHT: f32 = 30.0;
/// Gap between rows, so a row's top is `index * (ROW_HEIGHT + ROW_SPACING)`.
const ROW_SPACING: f32 = 2.0;
/// Inset of a row's text from the highlight's edges. Kept on the row rather
/// than as a pad on the list so the highlight spans the full panel width,
/// squared up with the input above it.
const ROW_PAD_X: f32 = 14.0;
/// Width of the scrollbar iced draws *over* the list rather than beside it, so
/// a scrolling list has to reserve the lane itself — see [`row_padding`].
const SCROLLBAR_WIDTH: f32 = 10.0;
/// Height the list caps at before it scrolls. Below this the list is exactly as
/// tall as its content, so nothing scrolls and the offsets below stay 0 — which
/// is why [`navigate`] can treat the viewport as this tall whenever it matters.
const LIST_HEIGHT: f32 = 360.0;
/// Width of the floating panel.
const PANEL_WIDTH: f32 = 520.0;

/// Rows a Page Up / Page Down moves by — one screenful, less a row of overlap
/// so there's something in common between the before and after.
fn page() -> i32 {
    (LIST_HEIGHT / (ROW_HEIGHT + ROW_SPACING)) as i32 - 1
}

/// A row's padding for a list of `rows` entries: even on both sides, except
/// that a list long enough to scroll widens the right to hold the scrollbar's
/// lane. Otherwise the bar lands on top of the padding it's meant to sit
/// beside, and the right-hand column reads as though it has none.
fn row_padding(rows: usize) -> iced::Padding {
    let content = rows as f32 * ROW_HEIGHT + rows.saturating_sub(1) as f32 * ROW_SPACING;
    let scrollbar = if content > LIST_HEIGHT { SCROLLBAR_WIDTH } else { 0.0 };
    iced::Padding::default().left(ROW_PAD_X).right(ROW_PAD_X + scrollbar)
}

/// Which panel a shared message is about.
///
/// The five differ in what fills them and what choosing a row does; they do
/// *not* differ in typing, arrowing, scrolling or dismissing. Naming the panel
/// is what lets those four be one message each rather than four apiece — and
/// what keeps `App::update` a dispatch table as they multiply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Panel {
    Command,
    Vault,
    Theme,
    Font,
    Model,
}

impl Panel {
    /// The text input's widget id, for focusing it when the panel opens.
    pub(crate) fn input_id(self) -> Id {
        self.ids().0.clone()
    }

    /// The scrollable list's id, for [`navigate`]'s scrolling.
    pub(crate) fn list_id(self) -> &'static Id {
        self.ids().1
    }

    fn ids(self) -> (&'static Id, &'static Id) {
        macro_rules! ids {
            ($name:literal) => {{
                static INPUT: LazyLock<Id> = LazyLock::new(|| Id::new(concat!($name, "-panel")));
                static LIST: LazyLock<Id> = LazyLock::new(|| Id::new(concat!($name, "-panel-list")));
                (&*INPUT, &*LIST)
            }};
        }
        match self {
            Panel::Command => ids!("command"),
            Panel::Vault => ids!("vault"),
            Panel::Theme => ids!("theme"),
            Panel::Font => ids!("font"),
            Panel::Model => ids!("model"),
        }
    }
}

/// Escape dismisses, the arrow keys navigate.
///
/// Each panel still needs its own `key_event` for the subscription — a handler
/// is a bare `fn`, so it can't capture which panel it belongs to — but that is
/// now the only thing those functions say.
pub(crate) fn panel_key(event: &iced::Event, panel: Panel) -> Option<Message> {
    use iced::keyboard::key::{Key, Named};
    if let iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
        key: Key::Named(Named::Escape),
        ..
    }) = event
    {
        return Some(Message::ClosePanel(panel));
    }
    nav_key(event).map(|nav| Message::PanelNav(panel, nav))
}

/// What's open: the query, the highlighted row, and where the list is scrolled.
///
/// The scroll offset is tracked rather than inferred because the user can also
/// move it with the wheel — arrowing after a scroll has to work from where the
/// list actually is, not from where the last keypress left it.
#[derive(Default)]
pub(crate) struct State {
    pub query: String,
    pub selected: usize,
    pub offset: f32,
}

impl State {
    /// Resets to the top, for a fresh query (the old highlight means nothing
    /// once the list under it has changed).
    pub fn reset(&mut self) {
        self.selected = 0;
        self.offset = 0.0;
    }
}

/// A keyboard move within an open panel, resolved by [`navigate`]. Each panel
/// wraps this in its own message so the two stay independent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Nav {
    /// One row up or down — wraps at the ends, so holding Down cycles.
    Step(i32),
    /// A screenful, clamped at the ends (wrapping a page jump would lose
    /// your place rather than take you to the end of the list).
    Page(i32),
    First,
    Last,
}

/// Reads a key press as a navigation, or `None` if it isn't one. Shared by both
/// panels' `key_event` subscriptions, which map the result onto their own
/// messages (a subscription handler is a bare `fn`, so it can't capture them).
pub(crate) fn nav_key(event: &iced::Event) -> Option<Nav> {
    use iced::keyboard::key::{Key, Named};
    let iced::Event::Keyboard(iced::keyboard::Event::KeyPressed { key, .. }) = event else {
        return None;
    };
    match key {
        Key::Named(Named::ArrowDown) => Some(Nav::Step(1)),
        Key::Named(Named::ArrowUp) => Some(Nav::Step(-1)),
        Key::Named(Named::PageDown) => Some(Nav::Page(page())),
        Key::Named(Named::PageUp) => Some(Nav::Page(-page())),
        Key::Named(Named::Home) => Some(Nav::First),
        Key::Named(Named::End) => Some(Nav::Last),
        _ => None,
    }
}

/// Applies `nav` to the highlight over a list of `len` rows and scrolls the
/// list — by the least that brings the new row fully into view, so the list
/// holds still until the highlight would leave it.
pub(crate) fn navigate(state: &mut State, nav: Nav, len: usize, list: &Id) -> Task<Message> {
    if len == 0 {
        return Task::none();
    }
    let last = len as i32 - 1;
    // The list may have shrunk under a stale highlight (the query changed, or
    // a rescan landed), so clamp before moving from it.
    let current = (state.selected as i32).min(last);
    state.selected = match nav {
        Nav::Step(delta) => (current + delta).rem_euclid(len as i32),
        Nav::Page(delta) => (current + delta).clamp(0, last),
        Nav::First => 0,
        Nav::Last => last,
    } as usize;
    scroll_into_view(state, list)
}

/// Scrolls the list so the highlighted row is fully visible, and records where
/// that left it. No task when it's already in view.
fn scroll_into_view(state: &mut State, list: &Id) -> Task<Message> {
    let stride = ROW_HEIGHT + ROW_SPACING;
    let top = state.selected as f32 * stride;
    let bottom = top + ROW_HEIGHT;
    let offset = if top < state.offset {
        top
    } else if bottom > state.offset + LIST_HEIGHT {
        bottom - LIST_HEIGHT
    } else {
        return Task::none();
    }
    .max(0.0);

    state.offset = offset;
    iced::advanced::widget::operate(
        iced::advanced::widget::operation::scrollable::scroll_to(
            list.clone(),
            AbsoluteOffset { x: None, y: Some(offset) },
        ),
    )
}

/// One row: what it says, a muted note on the right, and what pressing it does.
pub(super) struct Row<'a> {
    pub title: Cow<'a, str>,
    /// The muted right-hand column — a shortcut hint in the palette, the
    /// enclosing folder in the vault browser. Empty for none.
    pub detail: Cow<'a, str>,
    /// A disabled row is greyed and can't be pressed (but still takes a slot,
    /// so the list doesn't reshuffle as the app's state changes under it).
    pub enabled: bool,
    /// Draws the title in a face of the row's own choosing rather than the
    /// panel's. Only the font picker uses it, where a list of family names in
    /// one uniform face would tell you nothing about any of them.
    pub font: Option<iced::Font>,
    pub press: Message,
}

/// What makes a shell one panel rather than another.
///
/// Only what actually differs: typing, scrolling and dismissing are the same
/// interactions everywhere, so [`Panel`] supplies those messages and their
/// widget ids rather than each caller repeating them.
pub(super) struct Shell<'a> {
    pub panel: Panel,
    pub placeholder: &'a str,
    /// Enter — run/open whatever is highlighted.
    pub on_submit: Message,
    /// Shown in place of the rows when there are none.
    pub empty: Cow<'a, str>,
}

/// The panel: input, rows, and the backdrop that dismisses it.
pub(super) fn view<'a>(
    app: &'a App,
    state: &'a State,
    shell: Shell<'a>,
    rows: Vec<Row<'a>>,
) -> Element<'a, Message> {
    let theme = &app.theme;
    // A Copy of the theme for the shared-chrome style closures.
    let ct = app.theme;
    let font = theme.ui_font();
    let (text_c, muted, accent, input_bg, border_c) =
        (theme.text, theme.text_muted, theme.accent, theme.background, theme.border);

    let which = shell.panel;
    let input = text_input(shell.placeholder, &state.query)
        .id(which.input_id())
        .on_input(move |query| Message::PanelInput(which, query))
        .on_submit(shell.on_submit)
        .size(15)
        .font(font)
        .width(Length::Fill)
        .style(move |_theme, status| text_input::Style {
            background: Background::Color(input_bg),
            border: Border {
                radius: 6.0.into(),
                width: 1.0,
                color: if matches!(status, text_input::Status::Focused { .. }) {
                    accent
                } else {
                    border_c
                },
            },
            icon: muted,
            placeholder: muted,
            value: text_c,
            selection: Color { a: 0.30, ..accent },
        });

    let body = if rows.is_empty() {
        // Sits at a row's height so an empty list doesn't collapse the panel.
        Element::from(
            container(text(shell.empty).size(14).font(font).color(muted))
                .height(Length::Fixed(ROW_HEIGHT))
                // One line never scrolls, so no lane to reserve.
                .padding(row_padding(1))
                .align_y(Alignment::Center),
        )
    } else {
        let padding = row_padding(rows.len());
        let mut list = column![].spacing(ROW_SPACING);
        for (i, entry) in rows.into_iter().enumerate() {
            let selected = i == state.selected;
            let title_color = if entry.enabled { text_c } else { muted };
            let mut line = row![
                text(entry.title)
                    .size(14)
                    .font(entry.font.unwrap_or(font))
                    .color(title_color)
                    .width(Length::Fill)
            ]
            // A row is a fixed height, and `layout::padded` seats a button's
            // content against its padding rather than centring it — so the line
            // fills that height and centres itself within it.
            .height(Length::Fill)
            .spacing(8)
            .align_y(Alignment::Center);
            // Only when there is one: an empty child would still take the
            // row's spacing, nudging every unshortcutted title off-centre.
            if !entry.detail.is_empty() {
                line = line.push(text(entry.detail).size(12).font(font).color(muted));
            }
            list = list.push(
                button(line)
                    .width(Length::Fill)
                    .height(Length::Fixed(ROW_HEIGHT))
                    .padding(padding)
                    .on_press_maybe(entry.enabled.then_some(entry.press))
                    .style(move |_theme, status| super::chrome::menu_item(&ct, status, selected)),
            );
        }
        Element::from(
            container(
                scrollable(list)
                    .id(which.list_id().clone())
                    .on_scroll(move |viewport| {
                        Message::PanelScrolled(which, viewport.absolute_offset().y)
                    }),
            )
            .max_height(LIST_HEIGHT),
        )
    };

    let panel = container(column![input, body].spacing(8))
        .width(Length::Fixed(PANEL_WIDTH))
        .padding(10)
        .style(move |_theme| super::chrome::panel(&ct, ct.elevated()));

    // A click on the panel is swallowed; a click anywhere else dismisses.
    let panel = mouse_area(panel).on_press(Message::Noop);

    let backdrop = container(panel)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(iced::alignment::Horizontal::Center)
        .align_y(iced::alignment::Vertical::Top)
        // Clear the macOS notch, like the find bar.
        .padding(iced::Padding::default().top(64.0 + app.top_safe_inset));

    mouse_area(backdrop).on_press(Message::ClosePanel(which)).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_at(selected: usize, offset: f32) -> State {
        State { query: String::new(), selected, offset }
    }

    #[test]
    fn arrows_wrap_and_pages_clamp() {
        let list = Id::new("test-list");
        let mut state = state_at(0, 0.0);
        // Up from the first row lands on the last.
        let _ = navigate(&mut state, Nav::Step(-1), 5, &list);
        assert_eq!(state.selected, 4);
        let _ = navigate(&mut state, Nav::Step(1), 5, &list);
        assert_eq!(state.selected, 0);
        // A page past the end stops there rather than wrapping round.
        let _ = navigate(&mut state, Nav::Page(page()), 5, &list);
        assert_eq!(state.selected, 4);
        let _ = navigate(&mut state, Nav::First, 5, &list);
        assert_eq!(state.selected, 0);
        let _ = navigate(&mut state, Nav::Last, 5, &list);
        assert_eq!(state.selected, 4);
    }

    #[test]
    fn a_scrolling_list_reserves_the_scrollbars_lane() {
        // Short enough to fit: even padding, and the highlight reads square.
        let short = row_padding(3);
        assert_eq!(short.left, short.right);
        assert_eq!(short.left, ROW_PAD_X);

        // Long enough to scroll: the bar is drawn over the list, so the right
        // grows by its width and the *visible* gap stays the same on both sides.
        let long = row_padding(100);
        assert_eq!(long.left, ROW_PAD_X);
        assert_eq!(long.right - SCROLLBAR_WIDTH, long.left);

        // Exactly one screenful still fits, so it mustn't reserve the lane.
        let fits = ((LIST_HEIGHT + ROW_SPACING) / (ROW_HEIGHT + ROW_SPACING)) as usize;
        assert_eq!(row_padding(fits).right, ROW_PAD_X);
        assert_eq!(row_padding(fits + 1).right, ROW_PAD_X + SCROLLBAR_WIDTH);
    }

    #[test]
    fn navigating_an_empty_list_does_nothing() {
        let list = Id::new("test-list");
        let mut state = state_at(0, 0.0);
        let _ = navigate(&mut state, Nav::Step(1), 0, &list);
        assert_eq!(state.selected, 0);
    }

    #[test]
    fn the_list_holds_still_until_the_highlight_would_leave_it() {
        let list = Id::new("test-list");
        let stride = ROW_HEIGHT + ROW_SPACING;
        let visible = (LIST_HEIGHT / stride) as usize;

        // Stepping down through the visible rows doesn't scroll at all.
        let mut state = state_at(0, 0.0);
        for _ in 0..visible - 1 {
            let _ = navigate(&mut state, Nav::Step(1), 100, &list);
            assert_eq!(state.offset, 0.0, "scrolled while row {} was in view", state.selected);
        }
        // The first row past the bottom edge scrolls by just enough to show it.
        let _ = navigate(&mut state, Nav::Step(1), 100, &list);
        let bottom = state.selected as f32 * stride + ROW_HEIGHT;
        assert_eq!(state.offset, bottom - LIST_HEIGHT);

        // And going back up stops as soon as the row is in view again, rather
        // than snapping the row to the top.
        let deep = state.offset;
        let _ = navigate(&mut state, Nav::Step(-1), 100, &list);
        assert_eq!(state.offset, deep);
    }

    #[test]
    fn a_wrap_to_the_last_row_scrolls_to_the_end() {
        let list = Id::new("test-list");
        let mut state = state_at(0, 0.0);
        let _ = navigate(&mut state, Nav::Step(-1), 100, &list);
        assert_eq!(state.selected, 99);
        let expected = 99.0 * (ROW_HEIGHT + ROW_SPACING) + ROW_HEIGHT - LIST_HEIGHT;
        assert_eq!(state.offset, expected);
    }

    #[test]
    fn scrolling_by_hand_is_where_the_next_keypress_starts_from() {
        let list = Id::new("test-list");
        // The user wheeled the list down; the highlight is now above the
        // viewport, so the next Down must bring it back rather than sit still.
        let mut state = state_at(0, 500.0);
        let _ = navigate(&mut state, Nav::Step(1), 100, &list);
        assert_eq!(state.selected, 1);
        assert_eq!(state.offset, 1.0 * (ROW_HEIGHT + ROW_SPACING));
    }
}
