//! The command palette (Cmd/Ctrl+Shift+P): a fuzzy text search over every
//! action, modeled on the find bar. Actions are modeled as a [`CommandId`]
//! (not a stored `Message`) so dispatch — and the few commands that need a
//! target position — live in one place (`App::run_command`).
//!
//! The panel itself is [`super::picker`], shared with the vault browser; this
//! module supplies the rows.

use std::sync::LazyLock;

use iced::Element;

use super::App;
use super::picker::{self, Panel, Row, Shell};
use super::settings::Field;
use crate::Message;

/// Every action reachable from the palette. Dispatch is in `App::run_command`;
/// a `Field` toggle flips the corresponding boolean setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CommandId {
    NewBoard,
    OpenBoard,
    OpenVault,
    ChooseTheme,
    ChooseFont,
    ChooseModel,
    SwitchCanvas,
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
    GroupSelection,
    ToggleAutoLayout,
    ToggleSetting(Field),
}

impl CommandId {
    /// Whether the command can run in the current app state (else it's shown
    /// greyed and isn't clickable).
    pub(super) fn is_enabled(self, app: &App) -> bool {
        self.unavailable(app).is_none()
    }

    /// Why the command can't run right now, phrased as what would let it — or
    /// `None` when it can.
    ///
    /// The palette prints this where an enabled row prints its shortcut. A row
    /// that is merely grey says *no* and nothing else: "Ask AI" sat dimmed
    /// with no hint that it was waiting on a provider, and the provider is the
    /// part nobody guesses.
    pub(super) fn unavailable(self, app: &App) -> Option<&'static str> {
        // Nothing to ask without an endpoint — "API URL" in settings is what
        // turns these on.
        const NO_PROVIDER: &str = "Needs an AI provider — Settings → AI";
        match self {
            CommandId::AskAi if !app.config.ai_enabled() => Some(NO_PROVIDER),
            CommandId::ChooseModel if !app.config.ai_enabled() => Some(NO_PROVIDER),
            CommandId::AskAi if app.selected.is_empty() => Some("Select a note to ask about"),
            CommandId::Copy | CommandId::DeleteSelected | CommandId::Distribute
                if app.selected.is_empty() =>
            {
                Some("Select a note first")
            }
            CommandId::GroupSelection if !app.groupable_selection() => {
                Some("Select two or more notes")
            }
            // Nothing to browse until a vault has been picked — "Open vault…"
            // sits right above it.
            CommandId::SwitchCanvas if !app.has_vault() => Some("Open a vault first"),
            _ => None,
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
        cmd(SwitchCanvas, "Switch board", "vault browse canvas open recent switch", "⌘O"),
        cmd(OpenVault, "Open vault…", "obsidian folder library", ""),
        cmd(ChooseTheme, "Change theme…", "colours colors appearance scheme palette dark", ""),
        cmd(ChooseFont, "Change font…", "typeface family typography text serif mono", ""),
        cmd(ChooseModel, "Change AI model…", "llm gpt claude openrouter fireworks provider", ""),
        cmd(AddNode, "Add node", "new note card", ""),
        cmd(AskAi, "Ask AI", "generate llm gpt", ""),
        cmd(Distribute, "Auto-layout selection", "distribute arrange tidy", ""),
        cmd(GroupSelection, "Group selection", "frame box enclose", ""),
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
        list.push(cmd(
            ToggleSetting(Field::TrayIconMonochrome),
            "Toggle monochrome menu bar icon",
            "setting macos tray menu bar icon",
            "",
        ));
    }
    list
});

/// Commands matching `query`, best first (empty query = registry order).
pub(crate) fn filtered(query: &str) -> Vec<&'static Command> {
    // Title first, keywords behind it — the shape every panel here ranks by.
    thinkboard_core::fuzzy::rank(&COMMANDS, |c| {
        thinkboard_core::fuzzy::score_alias(query, c.title, Some(c.keywords))
    })
    .into_iter()
    .filter_map(|i| COMMANDS.get(i))
    .collect()
}

/// Keyboard nav while the palette is open (subscribed like `search_key_event`).
/// Enter is handled by the input's `on_submit`, so it's absent here.
pub(crate) fn key_event(
    event: iced::Event,
    _status: iced::event::Status,
    _window: iced::window::Id,
) -> Option<Message> {
    picker::panel_key(&event, Panel::Command)
}

/// The palette panel: a focused input over a scrollable, fuzzy-filtered command
/// list. Rows are greyed and un-clickable when their command isn't applicable.
pub(super) fn view<'a>(app: &'a App, state: &'a picker::State) -> Element<'a, Message> {
    let rows = filtered(&state.query)
        .into_iter()
        .map(|command| {
            let unavailable = command.id.unavailable(app);
            Row {
                title: command.title.into(),
                // The reason replaces the shortcut: a chord for something that
                // can't run is the one thing on the row with no use.
                detail: unavailable.unwrap_or(command.shortcut).into(),
                enabled: unavailable.is_none(),
                font: None,
                press: Message::RunCommand(command.id),
            }
        })
        .collect();

    picker::view(
        app,
        state,
        Shell {
            panel: Panel::Command,
            placeholder: "Run a command…",
            on_submit: Message::RunSelectedCommand,
            empty: "No matching command".into(),
        },
        rows,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filtered_ranks_the_obvious_match_first() {
        let hits = filtered("settings");
        assert_eq!(hits.first().map(|c| c.id), Some(CommandId::Settings));
        // Empty query lists everything in registry order.
        assert_eq!(filtered("").len(), COMMANDS.len());
        // A keyword-only term still finds its command.
        assert!(filtered("arrange").iter().any(|c| c.id == CommandId::Distribute));
    }

    #[test]
    fn the_vault_commands_are_reachable_by_their_keywords() {
        assert!(filtered("vault").iter().any(|c| c.id == CommandId::OpenVault));
        // "browse" is a keyword of Switch canvas, not of its title.
        assert!(filtered("browse").iter().any(|c| c.id == CommandId::SwitchCanvas));
    }
}
