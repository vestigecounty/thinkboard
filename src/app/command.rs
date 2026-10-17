//! The command palette (Cmd/Ctrl+K or +P): a fuzzy text search over every
//! action, modeled on the find bar. Actions are modeled as a [`CommandId`]
//! (not a stored `Message`) so dispatch — and the few commands that need a
//! target position — live in one place (`App::run_command`).
//!
//! The panel itself is [`super::picker`], shared with the vault browser; this
//! module supplies the rows.
//!
//! **Settings are searched alongside the commands**, from the settings index
//! (`settings::index`) the form draws its rows from: "mermaid" finds "Link
//! style › Mermaid", "inertia" finds "Pan inertia". A choice is an entry per
//! option, applied as it's picked, the one in force marked; a switch flips;
//! anything else opens the form at its row. They join a query only — the
//! palette with nothing typed stays a list of commands.

use std::borrow::Cow;
use std::sync::LazyLock;

use iced::Element;

use super::App;
use super::picker::{self, Panel, Row, Shell};
use super::settings::{self, Field, Kind};
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
    /// A setting from the index: a switch flips, anything else opens the form
    /// at its row.
    Setting(Field),
    /// One choice of a choice setting, by its place in `App::choices`.
    Choice(Field, u8),
    /// The board-by-keyboard actions (see `app::navigate`), here so they can
    /// be found — and their keys learned from the hint beside them.
    FitBoard,
    EditNote,
    AddLinkedNote,
    /// Colours the selection: an Obsidian preset code, or `None` for none —
    /// the node menu's swatches, for a keyboard.
    Colour(Option<&'static str>),
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
            CommandId::Copy
            | CommandId::DeleteSelected
            | CommandId::Distribute
            | CommandId::EditNote
            | CommandId::AddLinkedNote
            | CommandId::Colour(_)
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
            CommandId::Setting(field) | CommandId::Choice(field, _) => settings_unavailable(app, field),
            _ => None,
        }
    }
}

/// Why a setting's entry can't act now — the form greys or hides its row in
/// the same states.
fn settings_unavailable(app: &App, field: Field) -> Option<&'static str> {
    match field {
        Field::DismissOnBackgroundClick | Field::PersistAcrossSpaces | Field::SummonGesture | Field::OverlayBackdrop
            if !app.overlay =>
        {
            Some("Overlay only")
        }
        Field::DoubleClickAddsNote if app.config.click_dismisses(app.overlay) => {
            Some("Needs click-to-dismiss off")
        }
        Field::AiApiKey | Field::AiNodeColor | Field::AiThinking if !app.config.ai_enabled() => {
            Some("Needs an AI provider — Settings → AI")
        }
        // Only a custom endpoint has a URL to type: a preset fills its own.
        Field::AiUrl if !(app.settings_custom_ai || settings::is_custom(&app.config)) => {
            Some("Pick Custom as the AI provider")
        }
        _ => None,
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
    vec![
        cmd(NewBoard, "New board", "create file", ""),
        cmd(OpenBoard, "Open board", "file canvas load", ""),
        cmd(SwitchCanvas, "Switch board", "vault browse canvas open recent switch", "⌘O"),
        cmd(OpenVault, "Open vault…", "obsidian folder library", ""),
        cmd(ChooseTheme, "Change theme…", "colours colors appearance scheme palette dark", ""),
        cmd(ChooseFont, "Change font…", "typeface family typography text serif mono", ""),
        cmd(ChooseModel, "Change AI model…", "llm gpt claude openrouter fireworks provider", ""),
        cmd(AddNode, "Add node", "new note card", ""),
        cmd(AddLinkedNote, "Add linked note", "new child connected card link", "⇥"),
        cmd(EditNote, "Edit note", "write text change", "↩"),
        cmd(FitBoard, "Fit board to view", "zoom all overview everything", "⌘0"),
        cmd(Colour(Some("1")), "Color: red", "colour color tint card", ""),
        cmd(Colour(Some("2")), "Color: orange", "colour color tint card", ""),
        cmd(Colour(Some("3")), "Color: yellow", "colour color tint card", ""),
        cmd(Colour(Some("4")), "Color: green", "colour color tint card", ""),
        cmd(Colour(Some("5")), "Color: cyan", "colour color tint card blue", ""),
        cmd(Colour(Some("6")), "Color: purple", "colour color tint card violet", ""),
        cmd(Colour(None), "Color: none", "colour color tint card clear default", ""),
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
    ]
});

/// One row of the palette: a command, or a setting from the index.
pub(crate) struct Entry {
    pub id: CommandId,
    pub title: Cow<'static, str>,
    /// Terms it's found by but doesn't show.
    pub keywords: Cow<'static, str>,
    /// What the right of the row says: a command's shortcut; a switch's state;
    /// "current" on the choice in force; the tab a row opens on.
    pub detail: Cow<'static, str>,
}

/// The commands, as palette rows.
fn command_entries() -> impl Iterator<Item = Entry> {
    COMMANDS.iter().map(|command| Entry {
        id: command.id,
        title: command.title.into(),
        keywords: command.keywords.into(),
        detail: command.shortcut.into(),
    })
}

/// The settings index, as palette rows.
fn setting_entries(app: &App) -> Vec<Entry> {
    let mut entries = Vec::new();
    for field in settings::index() {
        let tab = field.tab().label();
        let keywords = format!("setting settings {tab} {}", field.keywords());
        match field.kind() {
            Kind::Switch => entries.push(Entry {
                id: CommandId::Setting(field),
                title: field.name().into(),
                keywords: keywords.into(),
                detail: if app.setting_bool(field) { "On" } else { "Off" }.into(),
            }),
            Kind::Choice => {
                for (at, choice) in app.choices(field).into_iter().enumerate() {
                    entries.push(Entry {
                        id: CommandId::Choice(field, at as u8),
                        title: format!("{} › {}", field.name(), choice.label).into(),
                        keywords: format!("{keywords} {}", choice.keywords).into(),
                        detail: if choice.current { "current" } else { "" }.into(),
                    });
                }
            }
            // Each has its own command ("Change theme…"), which opens it.
            Kind::Picker => {}
            Kind::Row => entries.push(Entry {
                id: CommandId::Setting(field),
                title: field.name().into(),
                keywords: keywords.into(),
                detail: tab.into(),
            }),
        }
    }
    entries
}

/// `entries` matching `query`, best first. Title first, keywords behind it —
/// the shape every panel here ranks by — and a stable sort, so the order
/// they're given in breaks ties.
fn rank(entries: Vec<Entry>, query: &str) -> Vec<Entry> {
    let order = thinkboard_core::fuzzy::rank(&entries, |entry| {
        thinkboard_core::fuzzy::score_alias(query, &entry.title, Some(&entry.keywords))
    });
    let mut entries: Vec<Option<Entry>> = entries.into_iter().map(Some).collect();
    order.into_iter().filter_map(|at| entries.get_mut(at).and_then(Option::take)).collect()
}

/// The palette's rows for `query`, best first. With nothing typed, the
/// commands in registry order: the settings join a search, not the list
/// someone browses to see what the app does.
pub(crate) fn filtered(app: &App, query: &str) -> Vec<Entry> {
    let mut entries: Vec<Entry> = command_entries().collect();
    if !query.trim().is_empty() {
        entries.extend(setting_entries(app));
    }
    rank(entries, query)
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
    let rows = filtered(app, &state.query)
        .into_iter()
        .map(|entry| {
            let unavailable = entry.id.unavailable(app);
            Row {
                title: entry.title,
                // The reason replaces the shortcut: a chord for something that
                // can't run is the one thing on the row with no use.
                detail: unavailable.map_or(entry.detail, Cow::Borrowed),
                enabled: unavailable.is_none(),
                font: None,
                press: Message::RunCommand(entry.id),
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

    fn commands(query: &str) -> Vec<CommandId> {
        rank(command_entries().collect(), query).into_iter().map(|entry| entry.id).collect()
    }

    #[test]
    fn filtered_ranks_the_obvious_match_first() {
        assert_eq!(commands("settings").first(), Some(&CommandId::Settings));
        // Empty query lists everything in registry order.
        assert_eq!(commands("").len(), COMMANDS.len());
        // A keyword-only term still finds its command.
        assert!(commands("arrange").contains(&CommandId::Distribute));
    }

    #[test]
    fn the_vault_commands_are_reachable_by_their_keywords() {
        assert!(commands("vault").contains(&CommandId::OpenVault));
        // "browse" is a keyword of Switch canvas, not of its title.
        assert!(commands("browse").contains(&CommandId::SwitchCanvas));
    }
}
