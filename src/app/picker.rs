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
use std::time::{Duration, Instant};

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
/// A notice's band: a line of 13 px text and its padding.
const NOTICE_HEIGHT: f32 = 33.0;
/// What a panel keeps off the window's edges.
const MARGIN: f32 = 16.0;

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
    Provider,
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

    /// The id of the control in the settings form that opens the panel in
    /// place — the Provider row's, the Model row's — for [`find_anchor`].
    pub(crate) fn anchor_id(self) -> &'static Id {
        static PROVIDER: LazyLock<Id> = LazyLock::new(|| Id::new("provider-anchor"));
        static MODEL: LazyLock<Id> = LazyLock::new(|| Id::new("model-anchor"));
        match self {
            Panel::Provider => &PROVIDER,
            _ => &MODEL,
        }
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
            Panel::Provider => ids!("provider"),
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
    /// The detail's colour, where it isn't the muted one: a model's "free".
    pub detail_tint: Option<Color>,
    /// A gauge after the detail: a mark drawn four times, how many of them
    /// are lit — the rightmost — and their colour; the rest faint. All four
    /// drawn, so it's the same extent on every row: a model's price, `$` to
    /// `$$$$`.
    pub gauge: Option<(&'static str, usize, Color)>,
    /// A shortcut, a cap per key, after the detail. Empty for none.
    pub keys: Vec<&'static str>,
    /// A disabled row is greyed and can't be pressed (but still takes a slot,
    /// so the list doesn't reshuffle as the app's state changes under it).
    pub enabled: bool,
    /// Draws the title in a face of the row's own choosing rather than the
    /// panel's. Only the font picker uses it, where a list of family names in
    /// one uniform face would tell you nothing about any of them.
    pub font: Option<iced::Font>,
    /// A mark before the title, saying what kind of row it is where the list
    /// mixes them: a board among the palette's commands.
    pub glyph: Option<super::chrome::Glyph>,
    pub press: Message,
}

/// How many marks a [`Row::gauge`] has.
pub(super) const GAUGE_STEPS: usize = 4;

/// A section header: the row it heads, what it says, and the colour it's set
/// in where it isn't the muted one.
pub(super) struct Header<'a> {
    /// The row it heads.
    pub heads: usize,
    pub label: Cow<'a, str>,
    /// Its colour, where it isn't the muted one.
    pub tint: Option<Color>,
    /// What to beware of in the section, at the header's right: a few words
    /// after a warning mark, both in the colour given.
    pub warning: Option<(Cow<'a, str>, Color)>,
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
    /// Since when it has been waiting on rows being fetched, with none to show
    /// (the model picker's): the list is drawn as rows still to come, at its
    /// full height, and `empty` says what's being waited on in the head —
    /// rather than a one-row panel that grows when they come.
    pub loading: Option<Instant>,
    /// A line over the list, in its colour: what led to the panel, where that
    /// wants saying (a sign-in that worked, over the models it opened).
    pub notice: Option<(Cow<'a, str>, Color)>,
    /// Section headers. The palette's and the model picker's, and only with
    /// nothing typed: a query ranks across them.
    pub headers: Vec<Header<'a>>,
    /// Under the control that opened it, rather than high over everything.
    pub anchor: Option<Anchor>,
}

/// Where a panel opened from the settings form is put: under the control
/// that opened it, in the window the form is in. The form stays up beneath
/// it; drawn high over the form, the panel was a second thing open, away
/// from where it was asked for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Anchor {
    /// The control, in the window's coordinates.
    pub at: iced::Rectangle,
    /// The window, which the panel is kept inside.
    pub window: iced::Size,
}

/// Where a panel is, of the places it can be.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Place {
    /// High over whatever's under it: the board, or a form whose control
    /// couldn't be found.
    Top,
    /// Under a control whose place is being asked for: not drawn until the
    /// answer comes, or it would show up somewhere else first.
    Finding,
    Under(Anchor),
}

/// The id of the container a settings form is drawn in, filling its window:
/// the board's modal, and the settings window. What an [`Anchor`] keeps a
/// panel inside.
pub(crate) static FORM_HOST: LazyLock<Id> = LazyLock::new(|| Id::new("settings-form-host"));

/// Where `panel`'s control is, and the window around it. Always answers:
/// None where it isn't on screen, so a panel waiting on it can go to the top.
pub(crate) fn find_anchor(panel: Panel) -> Task<Option<Anchor>> {
    iced::advanced::widget::operate(FindAnchor {
        target: panel.anchor_id(),
        pending: iced::Vector::ZERO,
        scrolled: iced::Vector::ZERO,
        entering_host: None,
        host: None,
        found: None,
    })
}

/// Walks the widget tree for a control and the form host around it. A
/// widget reports where it was laid out, not where it's drawn: inside a
/// scrollable, that's off by how far it's scrolled — the form is one — so
/// each scrollable's offset is taken off what's inside it.
struct FindAnchor {
    target: &'static Id,
    /// A scrollable's offset, for the walk into its content that follows.
    pending: iced::Vector,
    /// Every offset of the scrollables the walk is in.
    scrolled: iced::Vector,
    /// The host just met, for the walk into its content that follows.
    entering_host: Option<iced::Rectangle>,
    /// The host the walk is in.
    host: Option<iced::Rectangle>,
    found: Option<Anchor>,
}

impl iced::advanced::widget::Operation<Option<Anchor>> for FindAnchor {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn iced::advanced::widget::Operation<Option<Anchor>>)) {
        let (scrolled, host) = (self.scrolled, self.host);
        self.scrolled += std::mem::take(&mut self.pending);
        self.host = self.entering_host.take().or(self.host);
        operate(self);
        (self.scrolled, self.host) = (scrolled, host);
    }

    fn scrollable(
        &mut self,
        _id: Option<&Id>,
        _bounds: iced::Rectangle,
        _content_bounds: iced::Rectangle,
        translation: iced::Vector,
        _state: &mut dyn iced::advanced::widget::operation::Scrollable,
    ) {
        self.pending = translation;
    }

    fn container(&mut self, id: Option<&Id>, bounds: iced::Rectangle) {
        self.pending = iced::Vector::ZERO;
        if id == Some(&*FORM_HOST) {
            self.entering_host = Some(bounds);
        } else if id == Some(self.target) {
            let at = iced::Rectangle::new(bounds.position() - self.scrolled, bounds.size());
            if let Some(host) = self.host {
                self.found = Some(Anchor { at, window: host.size() });
            }
        }
    }

    fn finish(&self) -> iced::advanced::widget::operation::Outcome<Option<Anchor>> {
        iced::advanced::widget::operation::Outcome::Some(self.found)
    }
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
    let waiting = shell.loading.filter(|_| rows.is_empty());
    // How tall the list is, as laid out below: what keeps a panel put under
    // a control inside its window.
    let list_height = match rows.len() {
        _ if waiting.is_some() => LIST_HEIGHT,
        0 => ROW_HEIGHT,
        n => ((n + shell.headers.len()) as f32 * ROW_HEIGHT).min(LIST_HEIGHT),
    } + 2.0 * LIST_PAD;
    // How many a query left, as the find bar counts; nothing with none typed,
    // where the whole list is the answer. Waiting, what on.
    let count = match rows.len() {
        _ if waiting.is_some() => shell.empty.clone().into_owned(),
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

    let body = if let Some(since) = waiting {
        // A sweep takes as long as the app's animations say, and a scale of
        // nothing — animations off — holds it still.
        let scale = app.config.animation_scale;
        let sweep = (scale > 0.0).then(|| SKELETON_SWEEP.mul_f32(scale));
        let skeleton = Skeleton { since, sweep, ink: ct.text };
        Element::from(
            container(canvas(skeleton).width(Length::Fill).height(LIST_HEIGHT))
                .padding(iced::Padding { top: LIST_PAD, right: LIST_PAD, bottom: LIST_PAD, left: LIST_PAD }),
        )
    } else if rows.is_empty() {
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
            while let Some(Header { label, tint, warning, .. }) = headers.next_if(|header| header.heads == i) {
                list = list.push(header(label, warning, strong, tint.unwrap_or(muted), padding));
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
    let mut parts = column![head, rule()];
    let mut height = HEAD_HEIGHT + 1.0 + list_height + 1.0 + FOOT_HEIGHT;
    if let Some((words, color)) = shell.notice {
        height += NOTICE_HEIGHT;
        // Under the field, where the eye is when the panel comes up: a band
        // of its colour, faint, and the words in it.
        parts = parts.push(
            container(text(words).size(13).font(font).color(color))
                .width(Length::Fill)
                .padding([8.0, 14.0])
                .style(move |_theme| container::Style {
                    background: Some(Color { a: 0.10, ..color }.into()),
                    ..container::Style::default()
                }),
        );
    }
    // Under its control, right-aligned to it — the form's controls are at
    // its right — and kept inside the window: moved left as far as it
    // must, and up, over the control, where there's no room below.
    let under = shell.anchor.map(|Anchor { at, window }| {
        let width = PANEL_WIDTH.min(window.width - 2.0 * MARGIN).max(0.0);
        let left = (at.x + at.width - width).min(window.width - MARGIN - width).max(MARGIN);
        let top = (at.y + at.height + 6.0).min(window.height - MARGIN - height).max(MARGIN);
        (width, iced::Padding { top, left, ..iced::Padding::ZERO })
    });
    let panel = container(parts.push(body).push(rule()).push(foot))
        // As wide as the design asks, or the window less a margin each side.
        .width(under.map_or(Length::Fill, |(width, _)| Length::Fixed(width)))
        .max_width(PANEL_WIDTH)
        .style(move |_theme| chrome::accent_panel(&ct));

    // A click on the panel is swallowed; a click anywhere else dismisses.
    let panel = mouse_area(panel).on_press(Message::Noop);

    let backdrop = container(panel).width(Length::Fill).height(Length::Fill).align_y(iced::alignment::Vertical::Top);
    let backdrop = match under {
        Some((_, at)) => backdrop.align_x(iced::alignment::Horizontal::Left).padding(at),
        None => backdrop
            .align_x(iced::alignment::Horizontal::Center)
            // Clear the macOS notch, like the find bar, and keep off the
            // window's sides when it's narrower than the panel.
            .padding(iced::Padding { top: 64.0 + app.top_safe_inset, right: MARGIN, bottom: 0.0, left: MARGIN }),
    };

    mouse_area(backdrop).on_press(Message::ClosePanel(which)).into()
}

/// How long the skeleton's light takes to cross it, at an animation scale of 1.
const SKELETON_SWEEP: Duration = Duration::from_millis(1400);

/// A list's worth of rows still to come: each a bar where its title will be
/// and a short one for its detail, in the text colour barely there — the
/// lengths varied, as titles are, so it reads as a list and not as stripes —
/// and a band of light sweeping across them. One band, in the list's own
/// coordinates, so it crosses the bars in turn rather than each on its own:
/// why it's a canvas, where a container's gradient is its own.
struct Skeleton {
    since: Instant,
    /// A sweep's length; none holds the light still, which is to say off.
    sweep: Option<Duration>,
    ink: Color,
}

impl canvas::Program<Message> for Skeleton {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &iced::Renderer,
        _theme: &iced::Theme,
        bounds: iced::Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        use canvas::{Fill, Path, Style, gradient};
        use iced::{Point, Size};
        const TITLES: [f32; 5] = [0.7, 0.45, 0.58, 0.38, 0.5];
        const BAR: f32 = 9.0;
        const DETAIL: f32 = 28.0;
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let dim = Color { a: 0.07, ..self.ink };
        let fill = match self.sweep {
            Some(sweep) => {
                let through = (self.since.elapsed().as_secs_f32() / sweep.as_secs_f32()).fract();
                // From wholly off the left edge to wholly off the right, and
                // leaning, so each row below is lit a moment after the one above.
                let half = 90.0;
                let x = -half + through * (bounds.width + 2.0 * half);
                let light = gradient::Linear::new(Point::new(x - half, 0.0), Point::new(x + half, half * 0.4))
                    .add_stop(0.0, dim)
                    .add_stop(0.5, Color { a: 0.15, ..self.ink })
                    .add_stop(1.0, dim);
                Fill { style: Style::Gradient(light.into()), ..Fill::default() }
            }
            None => Fill::from(dim),
        };
        let titles = bounds.width - 2.0 * ROW_PAD_X - DETAIL - 10.0;
        for row in 0..(LIST_HEIGHT / ROW_HEIGHT) as usize {
            let y = row as f32 * ROW_HEIGHT + (ROW_HEIGHT - BAR) / 2.0;
            let title = titles * TITLES[row % TITLES.len()];
            for (x, width) in [(ROW_PAD_X, title), (bounds.width - ROW_PAD_X - DETAIL, DETAIL)] {
                let bar = Path::rounded_rectangle(Point::new(x, y), Size::new(width, BAR), (BAR / 2.0).into());
                frame.fill(&bar, fill);
            }
        }
        vec![frame.into_geometry()]
    }
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
fn header<'a>(
    label: Cow<'a, str>,
    warning: Option<(Cow<'a, str>, Color)>,
    strong: Font,
    color: Color,
    padding: iced::Padding,
) -> Element<'a, Message> {
    let caps = |words: Cow<'a, str>, color| {
        text(words.to_uppercase()).size(11).font(strong).color(color).wrapping(text::Wrapping::None)
    };
    let mut line = row![caps(label, color)].spacing(6).align_y(Alignment::Center);
    if let Some((words, tint)) = warning {
        line = line
            .push(Space::new().width(Length::Fill))
            .push(canvas(super::chrome::Icon { glyph: super::chrome::Glyph::Warning, color: tint }).width(14).height(14))
            .push(caps(words, tint));
    }
    container(line)
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
    let mut line = row![]
        // A row is a fixed height, and `layout::padded` seats a button's
        // content against its padding rather than centring it — so the line
        // fills that height and centres itself within it.
        .height(Length::Fill)
        .spacing(10)
        .align_y(Alignment::Center);
    if let Some(glyph) = entry.glyph {
        line = line.push(canvas(super::chrome::Icon { glyph, color: ct.text_muted }).width(14).height(14));
    }
    line = line.push(title);
    // Only when there is one: an empty child would still take the row's
    // spacing, nudging every row without one off-centre.
    if !entry.detail.is_empty() {
        let color = entry.detail_tint.unwrap_or(ct.text_muted);
        line = line.push(text(entry.detail).size(12).font(font).color(color).wrapping(text::Wrapping::None));
    }
    if let Some((mark, lit, tint)) = entry.gauge {
        let faint = Color { a: 0.3, ..ct.text_muted };
        let marks: Vec<Span<'a, (), Font>> = (0..GAUGE_STEPS)
            .map(|step| span(mark).font(font).color(if step + lit >= GAUGE_STEPS { tint } else { faint }))
            .collect();
        line = line.push(rich_text(marks).size(12).wrapping(text::Wrapping::None));
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

    #[test]
    fn a_control_is_found_where_its_scrolled_form_shows_it() {
        use iced::advanced::widget::Operation;
        use iced::advanced::widget::operation::{Outcome, Scrollable};
        use iced::{Point, Rectangle, Size, Vector};
        struct Still;
        impl Scrollable for Still {
            fn snap_to(&mut self, _offset: iced::advanced::widget::operation::scrollable::RelativeOffset<Option<f32>>) {}
            fn scroll_to(&mut self, _offset: AbsoluteOffset<Option<f32>>) {}
            fn scroll_by(&mut self, _offset: AbsoluteOffset, _bounds: Rectangle, _content_bounds: Rectangle) {}
        }
        let window = Rectangle::new(Point::ORIGIN, Size::new(900.0, 700.0));
        let control = Rectangle::new(Point::new(600.0, 400.0), Size::new(196.0, 32.0));
        let find = |scrolled: f32, host: bool| {
            let mut operation = FindAnchor {
                target: Panel::Provider.anchor_id(),
                pending: Vector::ZERO,
                scrolled: Vector::ZERO,
                entering_host: None,
                host: None,
                found: None,
            };
            // A host, a scrollable form in it, the control inside that, and
            // after the form, another scrollable that isn't scrolled into.
            operation.container(host.then_some(&*FORM_HOST), window);
            operation.traverse(&mut |operation| {
                operation.scrollable(None, window, window, Vector::new(0.0, scrolled), &mut Still);
                operation.traverse(&mut |operation| {
                    operation.container(Some(Panel::Provider.anchor_id()), control);
                });
                operation.scrollable(None, window, window, Vector::new(0.0, 999.0), &mut Still);
            });
            match operation.finish() {
                Outcome::Some(found) => found,
                _ => panic!("always answers"),
            }
        };
        let at = |y: f32| Some(Anchor { at: Rectangle::new(Point::new(600.0, y), control.size()), window: window.size() });
        assert_eq!(find(0.0, true), at(400.0));
        assert_eq!(find(150.0, true), at(250.0), "less how far the form is scrolled");
        assert_eq!(find(0.0, false), None, "not in a form's host: nowhere to keep it");
    }

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
