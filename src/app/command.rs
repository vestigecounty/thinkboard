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
//!
//! **With nothing typed it's a menu; with something typed, a search.** Empty,
//! it lists what can run now in sections ([`Section`]) — the selection's own
//! commands first when there is one, then the last few run, then the rest by
//! topic — and leaves out what can't run, which a menu would only grey. Typed
//! into, the sections fall away for one ranked list, and what can't run comes
//! back, last, saying why: searched for by name, it should say what it needs.

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
    EditNode,
    AddLinkedNode,
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
            CommandId::AskAi if app.selected.is_empty() => Some("Select a card to ask about"),
            CommandId::Copy
            | CommandId::DeleteSelected
            | CommandId::Distribute
            | CommandId::EditNode
            | CommandId::AddLinkedNode
            | CommandId::Colour(_)
                if app.selected.is_empty() =>
            {
                Some("Select a card first")
            }
            CommandId::GroupSelection if !app.groupable_selection() => {
                Some("Select two or more cards")
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
        Field::DoubleClickAddsCard if app.config.click_dismisses(app.overlay) => {
            Some("Needs click-to-dismiss off")
        }
        Field::AiApiKey | Field::AiCardColor | Field::AiThinking if !app.config.ai_enabled() => {
            Some("Needs an AI provider — Settings → AI")
        }
        // Only a custom endpoint has a URL to type: a preset fills its own.
        Field::AiUrl if !(app.settings_custom_ai || settings::is_custom(&app.config)) => {
            Some("Pick Custom as the AI provider")
        }
        _ => None,
    }
}

/// Where a command is listed while nothing is typed, in this order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Section {
    /// What acts on the selected cards — first, when anything is selected.
    Selection,
    Board,
    Edit,
    Appearance,
    Ai,
    App,
    /// Found by typing only. The seven colours: listed, they were half the
    /// selection's section, for something the card's own menu does in a click.
    Unlisted,
}

impl Section {
    /// The listed ones, in the order they're listed (the selection's comes
    /// first, and is placed by hand).
    const TOPICS: [Section; 5] = [Section::Board, Section::Edit, Section::Appearance, Section::Ai, Section::App];

    fn label(self) -> &'static str {
        match self {
            Section::Selection => "Selection",
            Section::Board => "Board",
            Section::Edit => "Edit",
            Section::Appearance => "Appearance",
            Section::Ai => "AI",
            Section::App => "App",
            Section::Unlisted => "",
        }
    }
}

/// A command's shortcut, written once and drawn as this platform spells it.
/// The chords are Cmd on macOS and Ctrl elsewhere (`modifiers.command()`), and
/// a hint that said ⌘ on Windows named a key that isn't there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Keys {
    command: bool,
    shift: bool,
    key: Key,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Key {
    Letter(&'static str),
    Tab,
    Enter,
    Delete,
}

impl Keys {
    const fn command(letter: &'static str) -> Option<Keys> {
        Some(Keys { command: true, shift: false, key: Key::Letter(letter) })
    }

    const fn command_shift(letter: &'static str) -> Option<Keys> {
        Some(Keys { command: true, shift: true, key: Key::Letter(letter) })
    }

    const fn bare(key: Key) -> Option<Keys> {
        Some(Keys { command: false, shift: false, key })
    }

    /// The caps to draw, in the platform's own order — ⇧⌘Z on a Mac, Ctrl
    /// Shift Z elsewhere — and in its own words for keys it labels in words.
    pub(crate) fn caps(self) -> Vec<&'static str> {
        let mac = cfg!(target_os = "macos");
        let mut caps = Vec::new();
        if mac {
            caps.extend(self.shift.then_some("⇧"));
            caps.extend(self.command.then_some("⌘"));
        } else {
            caps.extend(self.command.then_some("Ctrl"));
            caps.extend(self.shift.then_some("Shift"));
        }
        caps.push(match self.key {
            Key::Letter(letter) => letter,
            Key::Tab => if mac { "⇥" } else { "Tab" },
            Key::Enter => picker::ENTER,
            // The board takes Delete and Backspace alike; each platform's
            // name for the key you'd reach for.
            Key::Delete => if mac { "⌫" } else { "Del" },
        });
        caps
    }
}

pub(crate) struct Command {
    pub id: CommandId,
    pub title: &'static str,
    /// Extra search terms (never shown), so e.g. "arrange" finds "Auto-layout".
    pub keywords: &'static str,
    /// The keys that do it on the board, drawn as caps beside the title.
    pub shortcut: Option<Keys>,
    pub section: Section,
}

const fn cmd(
    id: CommandId,
    title: &'static str,
    keywords: &'static str,
    shortcut: Option<Keys>,
    section: Section,
) -> Command {
    Command { id, title, keywords, shortcut, section }
}

/// Every command, in the order each section lists them — which is also the
/// order a query's ties fall in.
static COMMANDS: LazyLock<Vec<Command>> = LazyLock::new(|| {
    use CommandId::*;
    use Section::*;
    vec![
        cmd(EditNode, "Edit card", "write text change note node", Keys::bare(Key::Enter), Selection),
        cmd(AddLinkedNode, "Add linked card", "new child connected note node link", Keys::bare(Key::Tab), Selection),
        cmd(AskAi, "Ask AI", "generate llm gpt", None, Selection),
        cmd(Distribute, "Auto-layout selection", "distribute arrange tidy", None, Selection),
        cmd(GroupSelection, "Group selection", "frame box enclose", None, Selection),
        cmd(Copy, "Copy", "clipboard", Keys::command("C"), Selection),
        cmd(DeleteSelected, "Delete selection", "remove", Keys::bare(Key::Delete), Selection),
        cmd(AddNode, "Add card", "new note node", None, Board),
        cmd(FitBoard, "Fit board to view", "zoom all overview everything", Keys::command("0"), Board),
        cmd(SwitchCanvas, "Switch board…", "vault browse canvas open recent switch", Keys::command("O"), Board),
        cmd(NewBoard, "New board", "create file", None, Board),
        cmd(OpenBoard, "Open board from file…", "file canvas load", None, Board),
        cmd(OpenVault, "Open vault…", "obsidian folder library", None, Board),
        cmd(Undo, "Undo", "", Keys::command("Z"), Edit),
        cmd(Redo, "Redo", "", Keys::command_shift("Z"), Edit),
        cmd(Paste, "Paste", "clipboard", Keys::command("V"), Edit),
        cmd(FindOnBoard, "Find on board", "search text", Keys::command("F"), Edit),
        cmd(ChooseTheme, "Change theme…", "colours colors appearance scheme palette dark", None, Appearance),
        cmd(ChooseFont, "Change font…", "typeface family typography text serif mono", None, Appearance),
        cmd(ChooseModel, "Change AI model…", "llm gpt claude openrouter fireworks provider", None, Ai),
        cmd(Settings, "Settings…", "preferences config options", Keys::command(","), App),
        cmd(Dismiss, "Dismiss board", "hide close overlay", None, App),
        cmd(Quit, "Quit ThinkBoard", "exit", None, App),
        cmd(Colour(Some("1")), "Color: red", "colour color tint card", None, Unlisted),
        cmd(Colour(Some("2")), "Color: orange", "colour color tint card", None, Unlisted),
        cmd(Colour(Some("3")), "Color: yellow", "colour color tint card", None, Unlisted),
        cmd(Colour(Some("4")), "Color: green", "colour color tint card", None, Unlisted),
        cmd(Colour(Some("5")), "Color: cyan", "colour color tint card blue", None, Unlisted),
        cmd(Colour(Some("6")), "Color: purple", "colour color tint card violet", None, Unlisted),
        cmd(Colour(None), "Color: none", "colour color tint card clear default", None, Unlisted),
    ]
});

/// How many of the commands run last are listed first.
pub(crate) const RECENT: usize = 3;

/// One row of the palette: a command, or a setting from the index.
pub(crate) struct Entry {
    pub id: CommandId,
    pub title: Cow<'static, str>,
    /// Terms it's found by but doesn't show.
    pub keywords: Cow<'static, str>,
    /// What the right of the row says: a switch's state; "current" on the
    /// choice in force; the tab a row opens on.
    pub detail: Cow<'static, str>,
    /// A command's shortcut, as caps.
    pub keys: Vec<&'static str>,
}

impl Command {
    fn entry(&self) -> Entry {
        Entry {
            id: self.id,
            title: self.title.into(),
            keywords: self.keywords.into(),
            detail: "".into(),
            keys: self.shortcut.map(Keys::caps).unwrap_or_default(),
        }
    }
}

/// The commands, as palette rows.
fn command_entries() -> impl Iterator<Item = Entry> {
    COMMANDS.iter().map(Command::entry)
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
                keys: Vec::new(),
            }),
            Kind::Choice => {
                for (at, choice) in app.choices(field).into_iter().enumerate() {
                    entries.push(Entry {
                        id: CommandId::Choice(field, at as u8),
                        title: format!("{} › {}", field.name(), choice.label).into(),
                        keywords: format!("{keywords} {}", choice.keywords).into(),
                        detail: if choice.current { "current" } else { "" }.into(),
                        keys: Vec::new(),
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
                keys: Vec::new(),
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

/// What the palette lists for a query: its rows, in order, and the section
/// headers among them, each with the row it heads. Navigating, Enter and the
/// drawing all read this one list, so what's highlighted is what runs.
#[derive(Default)]
pub(crate) struct Listing {
    pub entries: Vec<Entry>,
    pub headers: Vec<(usize, &'static str)>,
}

impl Listing {
    /// A section, if it has anything in it: an empty heading says nothing.
    fn push(&mut self, label: &'static str, entries: Vec<Entry>) {
        if !entries.is_empty() {
            self.headers.push((self.entries.len(), label));
            self.entries.extend(entries);
        }
    }

    /// Whether `id` is listed already: no row appears twice.
    fn has(&self, id: CommandId) -> bool {
        self.entries.iter().any(|entry| entry.id == id)
    }
}

/// The palette's rows for `query`: a menu with nothing typed, a search with
/// something (see the module's notes).
pub(crate) fn listing(app: &App, query: &str) -> Listing {
    if query.trim().is_empty() { browse(app) } else { search(app, query) }
}

/// The rows alone, in the order they're listed.
pub(crate) fn filtered(app: &App, query: &str) -> Vec<Entry> {
    listing(app, query).entries
}

/// Nothing typed: what can run now, in sections. The settings join a search,
/// not the list someone browses to see what the app does — except one run
/// lately, which comes back under Recent.
fn browse(app: &App) -> Listing {
    let mut listing = Listing::default();
    let runnable = |command: &&Command| command.id.is_enabled(app);
    let of = |wanted: Section| COMMANDS.iter().filter(move |command| command.section == wanted);

    if !app.selected.is_empty() {
        let entries = of(Section::Selection).filter(runnable).map(Command::entry).collect();
        listing.push(Section::Selection.label(), entries);
    }
    let recent = app
        .recent_commands
        .iter()
        .filter(|&&id| !listing.has(id) && id.is_enabled(app))
        .filter_map(|&id| entry_for(app, id))
        .take(RECENT)
        .collect();
    listing.push("Recent", recent);
    for topic in Section::TOPICS {
        let entries = of(topic).filter(runnable).filter(|command| !listing.has(command.id)).map(Command::entry).collect();
        listing.push(topic.label(), entries);
    }
    listing
}

/// Something typed: commands and settings ranked together, with what can't
/// run now after what can — each keeping its rank — so it's found, and says
/// why, but isn't the first thing Enter would do.
fn search(app: &App, query: &str) -> Listing {
    let mut entries: Vec<Entry> = command_entries().collect();
    entries.extend(setting_entries(app));
    let (ready, waiting): (Vec<Entry>, Vec<Entry>) = rank(entries, query).into_iter().partition(|entry| entry.id.is_enabled(app));
    Listing { entries: ready.into_iter().chain(waiting).collect(), headers: Vec::new() }
}

/// The row a command or setting is listed as — for Recent, which holds ids.
fn entry_for(app: &App, id: CommandId) -> Option<Entry> {
    match id {
        CommandId::Setting(_) | CommandId::Choice(..) => setting_entries(app).into_iter().find(|entry| entry.id == id),
        _ => COMMANDS.iter().find(|command| command.id == id).map(Command::entry),
    }
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

/// The palette panel: a focused input over the listing. Rows are greyed and
/// un-clickable when their command isn't applicable.
pub(super) fn view<'a>(app: &'a App, state: &'a picker::State) -> Element<'a, Message> {
    let Listing { entries, headers } = listing(app, &state.query);
    let rows = entries
        .into_iter()
        .map(|entry| {
            let unavailable = entry.id.unavailable(app);
            Row {
                matched: thinkboard_core::fuzzy::matched(&state.query, &entry.title),
                title: entry.title,
                // The reason replaces the shortcut: a chord for something that
                // can't run is the one thing on the row with no use.
                detail: unavailable.map_or(entry.detail, Cow::Borrowed),
                keys: if unavailable.is_some() { Vec::new() } else { entry.keys },
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
            glyph: super::chrome::Glyph::Prompt,
            on_submit: Message::RunSelectedCommand,
            verb: "Run",
            empty: "No matching command".into(),
            headers: headers.into_iter().map(|(heads, label)| (heads, label.into())).collect(),
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

    #[test]
    fn a_shortcut_is_spelled_the_platforms_way() {
        let redo = COMMANDS.iter().find(|command| command.id == CommandId::Redo).and_then(|command| command.shortcut);
        let caps = redo.expect("redo has keys").caps();
        // Each in its own order, too: Apple's modifiers lead with Shift.
        if cfg!(target_os = "macos") {
            assert_eq!(caps, ["⇧", "⌘", "Z"]);
        } else {
            assert_eq!(caps, ["Ctrl", "Shift", "Z"]);
        }
    }

    #[test]
    fn the_old_words_for_a_card_still_find_it() {
        // A card was a node in one menu and a note in the next; both are
        // hidden terms now, as "canvas" is for board.
        for word in ["node", "note"] {
            for id in [CommandId::AddNode, CommandId::AddLinkedNode, CommandId::EditNode] {
                assert!(commands(word).contains(&id), "{word} finds {id:?}");
            }
        }
    }
}
