//! The shell shared by the app's five type-to-narrow panels — the command
//! palette, the vault browser and the theme, font and model pickers: a focused
//! input over a scrollable list of rows, with the same keyboard navigation and
//! the same floating chrome.
//!
//! Only the shell lives here. What fills the rows is the caller's business:
//! the palette lists a static registry of actions, the browser ranks the
//! canvases found in the vault, and neither knows about the other.
//!
//! Rows are a **fixed height**, which is what lets [`navigate`] keep the
//! highlight on screen: iced reports a scroll position but won't tell us where
//! a particular child ended up, so the offset of row *n* has to be arithmetic.
//! The height is applied to the row rather than assumed of it, so the two can't
//! drift apart. A section header takes a row's slot for the same reason, and
//! is counted in the arithmetic ([`slot`]) but never highlighted.
//!
//! It wears the find bar's surface — the accent ring, the deeper shadow
//! (`chrome::accent_panel`) — with the input set into its head rather than
//! boxed, and a foot naming the keys.

use std::borrow::Cow;
use std::ops::Range;
use std::sync::LazyLock;

use iced::advanced::widget::Id;
use iced::advanced::widget::operation::scrollable::AbsoluteOffset;
use iced::widget::text::{LineHeight, Span};
use iced::widget::{Space, button, canvas, column, container, mouse_area, rich_text, row, scrollable, span, text, text_input};
use iced::{Alignment, Background, Border, Color, Element, Font, Length, Task};

use super::App;
use crate::Message;

/// Height of one row, enforced on the button below — and of a section header,
/// so that a row's top is `slot * ROW_HEIGHT`. Rows touch: the highlight is
/// a ring, so it needs no gap to stand apart.
const ROW_HEIGHT: f32 = 32.0;
/// Inset of a row's text from the highlight's edges. Kept on the row rather
/// than as a pad on the list so the highlight spans the list's width.
const ROW_PAD_X: f32 = 10.0;
/// The list's inset from the panel's edges, all round.
const LIST_PAD: f32 = 6.0;
/// The scrollbar, which runs in the list's right-hand inset: iced draws it
/// *over* the content, and there it's over nothing. Inside the rows it ran
/// over the highlight's ring, and keeping clear of it took a lane reserved
/// only while the list scrolled.
const SCROLLBAR_WIDTH: f32 = 4.0;
const SCROLLBAR_MARGIN: f32 = 1.0;
/// Height the list caps at before it scrolls: thirteen slots. Below this the
/// list is exactly as tall as its content, so nothing scrolls and the offsets
/// below stay 0 — which is why [`navigate`] can treat the viewport as this
/// tall whenever it matters.
const LIST_HEIGHT: f32 = 13.0 * ROW_HEIGHT;
/// Width of the floating panel, where the window has it.
const PANEL_WIDTH: f32 = 480.0;
/// The head, where the input is, and the foot, which names the keys.
const HEAD_HEIGHT: f32 = 44.0;
const FOOT_HEIGHT: f32 = 32.0;

/// Rows a Page Up / Page Down moves by — one screenful, less a row of overlap
/// so there's something in common between the before and after.
fn page() -> i32 {
    (LIST_HEIGHT / ROW_HEIGHT) as i32 - 1
}

/// Where row `index` sits in the list, counting the section headers above it
/// — each takes a row's slot. `headers` holds, for each, the row it heads.
fn slot(index: usize, headers: &[usize]) -> usize {
    index + headers.iter().filter(|&&heads| heads <= index).count()
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

/// Applies `nav` to the highlight over a list of `len` rows, under `headers`
/// (the rows each section header heads), and scrolls the list — by the least
/// that brings the new row fully into view, so the list holds still until the
/// highlight would leave it.
pub(crate) fn navigate(state: &mut State, nav: Nav, len: usize, headers: &[usize], list: &Id) -> Task<Message> {
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
    scroll_into_view(state, headers, list)
}

/// Scrolls the list so the highlighted row is fully visible, and records where
/// that left it. No task when it's already in view.
fn scroll_into_view(state: &mut State, headers: &[usize], list: &Id) -> Task<Message> {
    let at = slot(state.selected, headers);
    // A row that opens a section brings its header into view with it: scrolled
    // to alone, the first row of a section sat at the top under no heading.
    let first = if headers.contains(&state.selected) { at - 1 } else { at };
    let top = first as f32 * ROW_HEIGHT;
    let bottom = (at + 1) as f32 * ROW_HEIGHT;
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
    /// Where the query matched the title (`fuzzy::matched`), set in the
    /// theme's bold — what a row was found by, at a glance. Empty for none.
    pub matched: Vec<Range<usize>>,
    /// The muted right-hand column — a setting's state in the palette, the
    /// enclosing folder in the vault browser, why a command can't run. Empty
    /// for none.
    pub detail: Cow<'a, str>,
    /// A shortcut, a cap per key, after the detail. Empty for none.
    pub keys: Vec<&'static str>,
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
    /// What the head opens with: the palette's prompt, the others' magnifier.
    pub glyph: super::chrome::Glyph,
    /// Enter — run/open whatever is highlighted.
    pub on_submit: Message,
    /// What Enter does, as the foot names it: "Run", "Open", "Choose".
    pub verb: &'a str,
    /// Shown in place of the rows when there are none.
    pub empty: Cow<'a, str>,
    /// Section headers, each with the row it heads. Only the palette has any,
    /// and only with nothing typed: a query ranks across them.
    pub headers: Vec<(usize, Cow<'a, str>)>,
}

/// How this platform spells the keys a hint names: Return is ↵ on a Mac and
/// Enter elsewhere, as the keys themselves are labelled.
pub(super) const ENTER: &str = if cfg!(target_os = "macos") { "↵" } else { "Enter" };

/// The panel: input, rows, and the backdrop that dismisses it.
pub(super) fn view<'a>(
    app: &'a App,
    state: &'a State,
    shell: Shell<'a>,
    rows: Vec<Row<'a>>,
) -> Element<'a, Message> {
    use super::chrome;
    // A Copy of the theme for the style closures.
    let ct = app.theme;
    let surface = ct.elevated();
    // The theme's own weights: the shaper matches weight exactly, and a family
    // without the one asked for draws in another face entirely.
    let font = Font { weight: crate::theme::iced_weight(ct.weight_normal), ..ct.ui_font() };
    let strong = Font { weight: crate::theme::iced_weight(ct.weight_bold), ..font };
    let (text_c, muted, accent) = (ct.text, ct.text_muted, ct.accent);
    // A step below muted, for what only labels the chrome: the placeholder, a
    // disabled row, the foot's words beside its keys.
    let faint = chrome::mix(muted, surface, 0.35);

    let which = shell.panel;
    let input = text_input(shell.placeholder, &state.query)
        .id(which.input_id())
        .on_input(move |query| Message::PanelInput(which, query))
        .on_submit(shell.on_submit)
        .size(14)
        .font(font)
        .padding(0)
        .width(Length::Fill)
        .style(move |_theme, _status| text_input::Style {
            // No box of its own: the head is the field, as in the find bar.
            background: Background::Color(Color::TRANSPARENT),
            border: Border::default(),
            icon: muted,
            placeholder: faint,
            value: text_c,
            selection: Color { a: 0.30, ..accent },
        });
    // How many a query left, as the find bar counts; nothing with none typed,
    // where the whole list is the answer.
    let count = match rows.len() {
        _ if state.query.trim().is_empty() => String::new(),
        0 => String::new(),
        1 => "1 result".to_owned(),
        n => format!("{n} results"),
    };
    let head = container(
        row![
            canvas(chrome::Icon { glyph: shell.glyph, color: muted }).width(14).height(14),
            input,
            text(count).size(12).font(font).color(muted).wrapping(text::Wrapping::None),
        ]
        .spacing(10)
        .align_y(Alignment::Center),
    )
    .padding([0.0, 14.0])
    .center_y(Length::Fixed(HEAD_HEIGHT));

    let body = if rows.is_empty() {
        // Sits at a row's height so an empty list doesn't collapse the panel.
        Element::from(
            container(text(shell.empty).size(13).font(font).color(muted))
                .padding([LIST_PAD, LIST_PAD + ROW_PAD_X])
                .center_y(Length::Fixed(ROW_HEIGHT + 2.0 * LIST_PAD)),
        )
    } else {
        let padding = iced::Padding::default().left(ROW_PAD_X).right(ROW_PAD_X);
        let mut headers = shell.headers.into_iter().peekable();
        // Its right inset inside the scrollable, so that's where the bar runs.
        let mut list = column![].padding(iced::Padding::default().right(LIST_PAD));
        for (i, entry) in rows.into_iter().enumerate() {
            while let Some((_, label)) = headers.next_if(|(heads, _)| *heads == i) {
                list = list.push(header(label, strong, muted, padding));
            }
            list = list.push(list_row(entry, i == state.selected, Look { ct, font, strong, faint }, padding));
        }
        Element::from(
            container(
                scrollable(list)
                    .id(which.list_id().clone())
                    .direction(scrollable::Direction::Vertical(
                        scrollable::Scrollbar::new()
                            .width(SCROLLBAR_WIDTH)
                            .scroller_width(SCROLLBAR_WIDTH)
                            .margin(SCROLLBAR_MARGIN),
                    ))
                    .style(move |_theme, status| scrollbar(&ct, status))
                    .on_scroll(move |viewport| {
                        Message::PanelScrolled(which, viewport.absolute_offset().y)
                    }),
            )
            .max_height(LIST_HEIGHT + 2.0 * LIST_PAD)
            .padding(iced::Padding { top: LIST_PAD, right: 0.0, bottom: LIST_PAD, left: LIST_PAD }),
        )
    };

    // What the keys do, said once at the foot rather than learned by trying.
    let key = move |keys: &'a str, does: &'a str| {
        row![
            text(keys).size(key_size(keys)).font(font).color(muted),
            text(does).size(11).font(font).color(faint),
        ]
        .spacing(5)
        .align_y(Alignment::Center)
    };
    let foot = container(
        row![key("↑↓", "Navigate"), key(ENTER, shell.verb), key("esc", "Close")].spacing(14).align_y(Alignment::Center),
    )
        .padding([0.0, 14.0])
        .center_y(Length::Fixed(FOOT_HEIGHT));

    let rule = || container(Space::new().width(Length::Fill).height(1)).style(move |_theme| chrome::hairline(&ct));
    let panel = container(column![head, rule(), body, rule(), foot])
        // As wide as the design asks, or the window less a margin each side.
        .width(Length::Fill)
        .max_width(PANEL_WIDTH)
        .style(move |_theme| chrome::accent_panel(&ct));

    // A click on the panel is swallowed; a click anywhere else dismisses.
    let panel = mouse_area(panel).on_press(Message::Noop);

    let backdrop = container(panel)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(iced::alignment::Horizontal::Center)
        .align_y(iced::alignment::Vertical::Top)
        // Clear the macOS notch, like the find bar, and keep off the window's
        // sides when it's narrower than the panel.
        .padding(iced::Padding { top: 64.0 + app.top_safe_inset, right: 16.0, bottom: 0.0, left: 16.0 });

    mouse_area(backdrop).on_press(Message::ClosePanel(which)).into()
}

/// What every row of one panel is drawn with.
#[derive(Clone, Copy)]
struct Look {
    ct: crate::theme::Theme,
    font: Font,
    strong: Font,
    faint: Color,
}

/// The size a key's label is drawn at. The UI font draws ↵ ⇥ ⌫ well under the
/// height of its letters, so at a letter's size they were specks in their caps.
fn key_size(key: &str) -> f32 {
    if matches!(key, "↵" | "⇥" | "⌫") { 14.0 } else { 11.0 }
}

/// A thin bar with no rail, darkening as it's used: the list is short enough
/// that the bar only says *there's more*, and shouldn't outweigh the rows.
fn scrollbar(ct: &crate::theme::Theme, status: scrollable::Status) -> scrollable::Style {
    let active = matches!(status, scrollable::Status::Hovered { .. } | scrollable::Status::Dragged { .. });
    let rail = scrollable::Rail {
        background: None,
        border: Border::default(),
        scroller: scrollable::Scroller {
            background: Background::Color(Color { a: if active { 0.32 } else { 0.18 }, ..ct.text }),
            border: Border { radius: (SCROLLBAR_WIDTH / 2.0).into(), ..Border::default() },
        },
    };
    let mut style = scrollable::default(&iced::Theme::Dark, status);
    style.vertical_rail = rail;
    style.horizontal_rail = rail;
    style
}

/// A section header, in a row's slot: its label sits at the slot's foot, so
/// the space above it is what parts it from the section before.
fn header<'a>(label: Cow<'a, str>, strong: Font, muted: Color, padding: iced::Padding) -> Element<'a, Message> {
    container(text(label.to_uppercase()).size(11).font(strong).color(muted).wrapping(text::Wrapping::None))
        .width(Length::Fill)
        .height(Length::Fixed(ROW_HEIGHT))
        .padding(padding.bottom(5.0))
        .align_y(Alignment::End)
        .into()
}

/// One row: the title (its matched letters bold), then the detail, then the
/// keycaps.
fn list_row<'a>(entry: Row<'a>, selected: bool, look: Look, padding: iced::Padding) -> Element<'a, Message> {
    let Look { ct, font, strong, faint } = look;
    let (color, bright) = if entry.enabled { (ct.text, ct.text) } else { (faint, ct.text_muted) };
    // A row in a face of its own (the font picker's) keeps it for every
    // letter: bold in a family that ships none would be drawn in another.
    let (plain, bold) = match entry.font {
        Some(own) => (own, own),
        None => (font, strong),
    };
    // One line, whatever the panel's width: a row is a fixed height, and a
    // title wrapped onto a second line drew it over the next row. A long one
    // is cut where the detail begins instead.
    let title = rich_text(title_spans(entry.title, &entry.matched, plain, bold, color, bright))
        .size(13.5)
        .line_height(LineHeight::Absolute(18.0.into()))
        .wrapping(text::Wrapping::None)
        .width(Length::Fill);
    let mut line = row![title]
        // A row is a fixed height, and `layout::padded` seats a button's
        // content against its padding rather than centring it — so the line
        // fills that height and centres itself within it.
        .height(Length::Fill)
        .spacing(10)
        .align_y(Alignment::Center);
    // Only when there is one: an empty child would still take the row's
    // spacing, nudging every row without one off-centre.
    if !entry.detail.is_empty() {
        line = line.push(text(entry.detail).size(12).font(font).color(ct.text_muted).wrapping(text::Wrapping::None));
    }
    if !entry.keys.is_empty() {
        let caps = entry.keys.into_iter().map(|key| {
            // A lone glyph sits in a square cap; a word gets the room it needs.
            let width = if key.chars().count() == 1 { Length::Fixed(20.0) } else { Length::Shrink };
            container(text(key).size(key_size(key)).font(font).color(ct.text_muted).line_height(LineHeight::Absolute(14.0.into())))
                .width(width)
                .height(Length::Fixed(20.0))
                .padding([0.0, 5.0])
                .align_x(Alignment::Center)
                .align_y(Alignment::Center)
                .style(move |_theme| super::chrome::keycap(&ct))
                .into()
        });
        line = line.push(row(caps).spacing(3));
    }
    button(line)
        .width(Length::Fill)
        .height(Length::Fixed(ROW_HEIGHT))
        .padding(padding)
        .clip(true)
        .on_press_maybe(entry.enabled.then_some(entry.press))
        .style(move |_theme, status| super::chrome::list_row(&ct, status, selected))
        .into()
}

/// `title` cut into runs at `matched`, those in `bold` and `bright`. A range
/// that doesn't fall on the title's own boundaries is ignored rather than
/// trusted: it was matched against some other string.
fn title_spans<'a>(
    title: Cow<'a, str>,
    matched: &[Range<usize>],
    plain: Font,
    bold: Font,
    color: Color,
    bright: Color,
) -> Vec<Span<'a, (), Font>> {
    let fits = matched.iter().all(|run| title.get(run.clone()).is_some());
    if matched.is_empty() || !fits {
        return vec![span(title).font(plain).color(color)];
    }
    let mut spans = Vec::new();
    let mut at = 0;
    for run in matched {
        if run.start > at {
            spans.push(span(title[at..run.start].to_owned()).font(plain).color(color));
        }
        spans.push(span(title[run.clone()].to_owned()).font(bold).color(bright));
        at = run.end.max(at);
    }
    if at < title.len() {
        spans.push(span(title[at..].to_owned()).font(plain).color(color));
    }
    spans
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
        let _ = navigate(&mut state, Nav::Step(-1), 5, &[], &list);
        assert_eq!(state.selected, 4);
        let _ = navigate(&mut state, Nav::Step(1), 5, &[], &list);
        assert_eq!(state.selected, 0);
        // A page past the end stops there rather than wrapping round.
        let _ = navigate(&mut state, Nav::Page(page()), 5, &[], &list);
        assert_eq!(state.selected, 4);
        let _ = navigate(&mut state, Nav::First, 5, &[], &list);
        assert_eq!(state.selected, 0);
        let _ = navigate(&mut state, Nav::Last, 5, &[], &list);
        assert_eq!(state.selected, 4);
    }

    #[test]
    fn navigating_an_empty_list_does_nothing() {
        let list = Id::new("test-list");
        let mut state = state_at(0, 0.0);
        let _ = navigate(&mut state, Nav::Step(1), 0, &[], &list);
        assert_eq!(state.selected, 0);
    }

    #[test]
    fn the_list_holds_still_until_the_highlight_would_leave_it() {
        let list = Id::new("test-list");
        let visible = (LIST_HEIGHT / ROW_HEIGHT) as usize;

        // Stepping down through the visible rows doesn't scroll at all.
        let mut state = state_at(0, 0.0);
        for _ in 0..visible - 1 {
            let _ = navigate(&mut state, Nav::Step(1), 100, &[], &list);
            assert_eq!(state.offset, 0.0, "scrolled while row {} was in view", state.selected);
        }
        // The first row past the bottom edge scrolls by just enough to show it.
        let _ = navigate(&mut state, Nav::Step(1), 100, &[], &list);
        let bottom = state.selected as f32 * ROW_HEIGHT + ROW_HEIGHT;
        assert_eq!(state.offset, bottom - LIST_HEIGHT);

        // And going back up stops as soon as the row is in view again, rather
        // than snapping the row to the top.
        let deep = state.offset;
        let _ = navigate(&mut state, Nav::Step(-1), 100, &[], &list);
        assert_eq!(state.offset, deep);
    }

    #[test]
    fn a_wrap_to_the_last_row_scrolls_to_the_end() {
        let list = Id::new("test-list");
        let mut state = state_at(0, 0.0);
        let _ = navigate(&mut state, Nav::Step(-1), 100, &[], &list);
        assert_eq!(state.selected, 99);
        let expected = 99.0 * ROW_HEIGHT + ROW_HEIGHT - LIST_HEIGHT;
        assert_eq!(state.offset, expected);
    }

    #[test]
    fn scrolling_by_hand_is_where_the_next_keypress_starts_from() {
        let list = Id::new("test-list");
        // The user wheeled the list down; the highlight is now above the
        // viewport, so the next Down must bring it back rather than sit still.
        let mut state = state_at(0, 500.0);
        let _ = navigate(&mut state, Nav::Step(1), 100, &[], &list);
        assert_eq!(state.selected, 1);
        assert_eq!(state.offset, 1.0 * ROW_HEIGHT);
    }

    #[test]
    fn a_header_takes_a_slot_and_comes_into_view_with_its_section() {
        let list = Id::new("test-list");
        // Headers over rows 0 and 3: row 3 sits in slot 5, under its header.
        let headers = [0, 3];
        assert_eq!(slot(0, &headers), 1);
        assert_eq!(slot(2, &headers), 3);
        assert_eq!(slot(3, &headers), 5);

        // Scrolled past the second section, arrowing back up to its first row
        // shows the header above it too.
        let mut state = state_at(4, 10.0 * ROW_HEIGHT);
        let _ = navigate(&mut state, Nav::Step(-1), 40, &headers, &list);
        assert_eq!(state.selected, 3);
        assert_eq!(state.offset, 4.0 * ROW_HEIGHT, "the header's slot, not the row's");

        // And back to the very top: the first header too.
        let _ = navigate(&mut state, Nav::First, 40, &headers, &list);
        assert_eq!(state.offset, 0.0);
    }

    #[test]
    fn a_title_is_cut_where_it_matched() {
        let (plain, bold) = (Font::DEFAULT, Font::MONOSPACE);
        let spans = title_spans("Add card".into(), std::slice::from_ref(&(0..3)), plain, bold, Color::WHITE, Color::WHITE);
        let texts: Vec<&str> = spans.iter().map(|span| span.text.as_ref()).collect();
        assert_eq!(texts, ["Add", " card"]);
        assert_eq!(spans[0].font, Some(bold));
        // A range from some other string is no reason to cut this one.
        let spans = title_spans("Café".into(), std::slice::from_ref(&(3..4)), plain, bold, Color::WHITE, Color::WHITE);
        assert_eq!(spans.len(), 1, "3..4 splits the é");
    }
}
