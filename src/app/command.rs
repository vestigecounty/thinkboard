//! The command palette (Cmd/Ctrl+Shift+P): a fuzzy text search over every
//! action, modeled on the find bar. Actions are modeled as a [`CommandId`]
//! (not a stored `Message`) so dispatch — and the few commands that need a
//! target position — live in one place (`App::run_command`).

use std::sync::LazyLock;

use iced::advanced::widget::Id;
use iced::widget::{button, column, container, mouse_area, row, scrollable, text, text_input};
use iced::{Alignment, Background, Border, Color, Element, Length};

use super::App;
use super::settings::Field;
use crate::Message;

/// Focus target for the palette's text input.
pub(crate) static COMMAND_INPUT: LazyLock<Id> = LazyLock::new(|| Id::new("command-palette"));

/// Open-palette state: the query and the highlighted row (index into the
/// filtered list).
#[derive(Default)]
pub(crate) struct CommandPalette {
    pub query: String,
    pub selected: usize,
}

/// Every action reachable from the palette. Dispatch is in `App::run_command`;
/// a `Field` toggle flips the corresponding boolean setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CommandId {
    NewBoard,
    OpenBoard,
    FindOnBoard,
    Settings,
    Dismiss,
    Quit,
    Undo,
    Redo,
    Copy,
    Paste,
    DeleteSelected,
    AddNode,
    AskAi,
    Distribute,
    ToggleAutoLayout,
    ToggleSetting(Field),
}

impl CommandId {
    /// Whether the command can run in the current app state (else it's shown
    /// greyed and isn't clickable).
    pub(super) fn is_enabled(self, app: &App) -> bool {
        match self {
            CommandId::AskAi => !app.selected.is_empty() && app.config.ai_enabled(),
            CommandId::Copy | CommandId::DeleteSelected | CommandId::Distribute => {
                !app.selected.is_empty()
            }
            _ => true,
        }
    }
}

pub(crate) struct Command {
    pub id: CommandId,
    pub title: &'static str,
    /// Extra search terms (never shown), so e.g. "arrange" finds "Auto-layout".
    pub keywords: &'static str,
    /// Display-only shortcut hint (right-aligned, greyed).
    pub shortcut: &'static str,
}

const fn cmd(
    id: CommandId,
    title: &'static str,
    keywords: &'static str,
    shortcut: &'static str,
) -> Command {
    Command { id, title, keywords, shortcut }
}

static COMMANDS: LazyLock<Vec<Command>> = LazyLock::new(|| {
    use CommandId::*;
    // `mut` is only exercised by the macOS-only pushes below.
    #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
    let mut list = vec![
        cmd(NewBoard, "New board", "create file", ""),
        cmd(OpenBoard, "Open board", "file canvas load", ""),
        cmd(AddNode, "Add node", "new note card", ""),
        cmd(AskAi, "Ask AI", "generate llm gpt", ""),
        cmd(Distribute, "Auto-layout selection", "distribute arrange tidy", ""),
        cmd(FindOnBoard, "Find on board", "search text", "⌘F"),
        cmd(Copy, "Copy", "clipboard", "⌘C"),
        cmd(Paste, "Paste", "clipboard", "⌘V"),
        cmd(DeleteSelected, "Delete selection", "remove", "⌫"),
        cmd(Undo, "Undo", "", "⌘Z"),
        cmd(Redo, "Redo", "", "⇧⌘Z"),
        cmd(Settings, "Settings", "preferences config options", ""),
        cmd(Dismiss, "Dismiss board", "hide close overlay", ""),
        cmd(Quit, "Quit ThinkBoard", "exit", ""),
        // Setting toggles (flip the current value):
        cmd(ToggleAutoLayout, "Toggle auto-layout on edit", "setting layout", ""),
        cmd(ToggleSetting(Field::CenterNodeText), "Toggle center node text", "setting", ""),
        cmd(ToggleSetting(Field::NodeScrollbars), "Toggle node scrollbars", "setting scroll resize", ""),
        cmd(ToggleSetting(Field::RasterizeText), "Toggle rasterize text", "setting", ""),
        cmd(ToggleSetting(Field::OverlayTransparent), "Toggle transparent overlay", "setting", ""),
        cmd(ToggleSetting(Field::AiCompletions), "Toggle AI completions", "setting", ""),
        cmd(ToggleSetting(Field::AiThinking), "Toggle AI thinking", "setting reasoning", ""),
        cmd(
            ToggleSetting(Field::DismissOnBackgroundClick),
            "Toggle dismiss on background click",
            "setting",
            "",
        ),
    ];
    #[cfg(target_os = "macos")]
    {
        list.push(cmd(
            ToggleSetting(Field::PersistAcrossSpaces),
            "Toggle persist across Spaces",
            "setting macos",
            "",
        ));
        list.push(cmd(
            ToggleSetting(Field::SummonGesture),
            "Toggle four-finger summon gesture",
            "setting macos trackpad",
            "",
        ));
    }
    list
});

/// Case-insensitive subsequence match with a score (higher = better): contiguous
/// runs and word-start hits are rewarded. `None` if not every query char appears
/// in order. An empty query matches everything with score 0.
pub(crate) fn fuzzy_score(query: &str, text: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    let q: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();
    let t: Vec<char> = text.chars().flat_map(char::to_lowercase).collect();
    let mut qi = 0;
    let mut score = 0;
    let mut prev: Option<usize> = None;
    for (i, &tc) in t.iter().enumerate() {
        if qi < q.len() && tc == q[qi] {
            score += 1;
            if prev == Some(i.wrapping_sub(1)) {
                score += 5; // contiguous run
            }
            if i == 0 || !t[i - 1].is_alphanumeric() {
                score += 8; // start of a word
            }
            prev = Some(i);
            qi += 1;
        }
    }
    (qi == q.len()).then_some(score)
}

/// Commands matching `query`, best first (empty query = registry order).
pub(crate) fn filtered(query: &str) -> Vec<&'static Command> {
    let mut scored: Vec<(&Command, i32)> = COMMANDS
        .iter()
        .filter_map(|c| {
            // Prefer a title match; fall back to keywords at a small penalty.
            let score = fuzzy_score(query, c.title)
                .or_else(|| fuzzy_score(query, c.keywords).map(|s| s - 4))?;
            Some((c, score))
        })
        .collect();
    // Stable sort (highest score first) keeps registry order for equal scores.
    scored.sort_by_key(|&(_, score)| std::cmp::Reverse(score));
    scored.into_iter().map(|(c, _)| c).collect()
}

/// Keyboard nav while the palette is open (subscribed like `search_key_event`).
/// Enter is handled by the input's `on_submit`, so it's absent here.
pub(crate) fn key_event(
    event: iced::Event,
    _status: iced::event::Status,
    _window: iced::window::Id,
) -> Option<Message> {
    use iced::keyboard::key::{Key, Named};
    let iced::Event::Keyboard(iced::keyboard::Event::KeyPressed { key, .. }) = event else {
        return None;
    };
    match key {
        Key::Named(Named::ArrowDown) => Some(Message::CommandPaletteMove(1)),
        Key::Named(Named::ArrowUp) => Some(Message::CommandPaletteMove(-1)),
        Key::Named(Named::Escape) => Some(Message::CloseCommandPalette),
        _ => None,
    }
}

/// The palette panel: a focused input over a scrollable, fuzzy-filtered command
/// list. Rows are greyed and un-clickable when their command isn't applicable.
pub(super) fn view<'a>(app: &'a App, palette: &'a CommandPalette) -> Element<'a, Message> {
    let theme = &app.theme;
    // A Copy of the theme for the shared-chrome style closures.
    let ct = app.theme;
    let font = theme.ui_font();
    let (text_c, muted, accent, input_bg, border_c) =
        (theme.text, theme.text_muted, theme.accent, theme.background, theme.border);

    let input = text_input("Run a command…", &palette.query)
        .id(COMMAND_INPUT.clone())
        .on_input(Message::CommandPaletteInput)
        .on_submit(Message::RunSelectedCommand)
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

    let matches = filtered(&palette.query);
    let mut list = column![].spacing(2);
    for (i, command) in matches.iter().enumerate() {
        let enabled = command.id.is_enabled(app);
        let selected = i == palette.selected;
        let title_color = if enabled { text_c } else { muted };
        let entry = row![
            text(command.title).size(14).font(font).color(title_color).width(Length::Fill),
            text(command.shortcut).size(12).font(font).color(muted),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        let id = command.id;
        list = list.push(
            button(entry)
                .width(Length::Fill)
                .padding([6, 10])
                .on_press_maybe(enabled.then_some(Message::RunCommand(id)))
                .style(move |_theme, status| super::chrome::menu_item(&ct, status, selected)),
        );
    }

    let body = column![
        input,
        // Right padding keeps the rows clear of the vertical scrollbar.
        container(scrollable(container(list).padding(iced::Padding::default().right(10))))
            .max_height(360.0),
    ]
    .spacing(8);

    let panel = container(body)
        .width(Length::Fixed(520.0))
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

    mouse_area(backdrop).on_press(Message::CloseCommandPalette).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_prefers_word_starts_and_contiguity() {
        // Subsequence found → Some; missing char → None.
        assert!(fuzzy_score("nb", "New board").is_some());
        assert!(fuzzy_score("xyz", "New board").is_none());
        // A contiguous, word-start prefix beats a scattered match.
        let prefix = fuzzy_score("new", "New board").unwrap();
        let scattered = fuzzy_score("new", "Rename widget").unwrap_or(i32::MIN);
        assert!(prefix > scattered);
    }

    #[test]
    fn filtered_ranks_the_obvious_match_first() {
        let hits = filtered("settings");
        assert_eq!(hits.first().map(|c| c.id), Some(CommandId::Settings));
        // Empty query lists everything in registry order.
        assert_eq!(filtered("").len(), COMMANDS.len());
        // A keyword-only term still finds its command.
        assert!(filtered("arrange").iter().any(|c| c.id == CommandId::Distribute));
    }
}
