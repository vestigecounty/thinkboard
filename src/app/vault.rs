//! The vault browser: a quick switcher over every `.canvas` file in the
//! configured Obsidian vault, sharing the command palette's panel
//! ([`super::picker`]) and its keyboard handling.
//!
//! A vault is a **scope, not a mode**. It says where to look for boards and
//! which folder a canvas's `file` paths resolve against — it doesn't replace
//! opening a canvas from anywhere else, which still works by dialog or by
//! dropping one on the window.
//!
//! The walk itself is off the UI thread: a vault can hold thousands of files,
//! and the panel has to appear on the keystroke that asked for it. So the last
//! scan is kept on the `App` and shown immediately, with a fresh one running
//! behind it that swaps in when it lands. Nothing watches the vault — the
//! external-change watch is deliberately one file (see `sync`), and walking a
//! whole vault on a timer would undo that.

use std::path::PathBuf;
use std::sync::LazyLock;

use iced::advanced::widget::Id;
use iced::{Element, Task};

use super::picker::{self, Row, Shell};
use super::{App, Message};
use thinkboard_core::vault;

/// Focus target for the browser's text input.
pub(crate) static VAULT_INPUT: LazyLock<Id> = LazyLock::new(|| Id::new("vault-browser"));
/// The scrollable canvas list, for keyboard scrolling.
pub(crate) static VAULT_LIST: LazyLock<Id> = LazyLock::new(|| Id::new("vault-browser-list"));

/// Keyboard nav while the browser is open. Enter is the input's `on_submit`.
pub(crate) fn key_event(
    event: iced::Event,
    _status: iced::event::Status,
    _window: iced::window::Id,
) -> Option<Message> {
    use iced::keyboard::key::{Key, Named};
    if let iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
        key: Key::Named(Named::Escape),
        ..
    }) = event
    {
        return Some(Message::CloseVaultBrowser);
    }
    picker::nav_key(&event).map(Message::VaultBrowserNav)
}

impl App {
    /// Whether a vault has been configured — what "Switch canvas" and Cmd+O
    /// gate on, since without one there's nothing to list.
    pub(super) fn has_vault(&self) -> bool {
        !self.config.vault.trim().is_empty()
    }

    /// The configured vault root, if there is one.
    pub(super) fn vault_path(&self) -> Option<PathBuf> {
        self.has_vault().then(|| PathBuf::from(self.config.vault.trim()))
    }

    /// Cmd/Ctrl+O and the "Switch canvas" command: the browser when there's a
    /// vault to browse, the ordinary open dialog when there isn't — so the
    /// single-canvas flow is untouched for anyone who never sets one.
    pub(super) fn switch_canvas(&mut self) -> Task<Message> {
        if self.has_vault() {
            self.open_vault_browser()
        } else {
            Task::done(Message::CanvasAction("open_board".to_owned()))
        }
    }

    /// Opens the browser over whatever the last scan found, and starts a fresh
    /// one. Showing the cached list first is what makes reopening instant; the
    /// rescan is what keeps it honest when boards were added or renamed.
    pub(super) fn open_vault_browser(&mut self) -> Task<Message> {
        let Some(root) = self.vault_path() else {
            return Task::none();
        };
        self.search = None; // mutually exclusive with the other panels
        self.command_palette = None;
        self.vault_browser = Some(picker::State::default());

        let focus = iced::advanced::widget::operate(
            iced::advanced::widget::operation::focusable::focus(VAULT_INPUT.clone()),
        );
        Task::batch([focus, self.rescan_vault(root)])
    }

    /// Walks the vault off the UI thread, resolving to [`Message::VaultScanned`]
    /// — the same shape as the rasterizer's and the reclusterer's background
    /// work, which is CPU-bound in a `Task::perform` too.
    fn rescan_vault(&mut self, root: PathBuf) -> Task<Message> {
        self.vault_scanning = true;
        Task::perform(
            async move { vault::scan(&root) },
            |scan| Message::VaultScanned(scan.entries, scan.truncated),
        )
    }

    /// Adopts a finished scan. The highlight is reset rather than carried over:
    /// it indexed the old list, and the row under it may now be a different
    /// board entirely.
    pub(super) fn vault_scanned(&mut self, entries: Vec<vault::CanvasEntry>, truncated: bool) {
        if truncated {
            eprintln!(
                "vault: stopped after {} canvases — the list is partial (is {} really the vault?)",
                vault::MAX_ENTRIES,
                self.config.vault
            );
        }
        self.vault_canvases = entries;
        self.vault_scanning = false;
        if let Some(state) = &mut self.vault_browser {
            state.reset();
        }
    }

    /// The canvases matching what's been typed, best first.
    pub(super) fn vault_matches(&self, query: &str) -> Vec<usize> {
        vault::rank(&self.vault_canvases, query)
    }

    /// The canvas the highlight is on, resolved through the current ranking.
    /// `None` when the browser is closed or nothing matches what's typed.
    pub(super) fn selected_canvas(&self) -> Option<PathBuf> {
        let state = self.vault_browser.as_ref()?;
        let index = *self.vault_matches(&state.query).get(state.selected)?;
        Some(self.vault_canvases.get(index)?.path.clone())
    }

    /// Opens the highlighted canvas (Enter).
    pub(super) fn open_selected_canvas(&mut self) -> Task<Message> {
        match self.selected_canvas() {
            Some(path) => {
                self.vault_browser = None;
                self.open_canvas(path)
            }
            None => Task::none(),
        }
    }

    /// Asks for the vault folder. Like the board dialogs, the always-on-top
    /// overlay is dismissed first (it would cover the native dialog) and the
    /// picker opens once it's hidden.
    pub(super) fn choose_vault(&mut self) -> Task<Message> {
        self.command_palette = None;
        self.vault_browser = None;
        if self.overlay {
            return self.hide_window().chain(Task::done(Message::OpenVaultDialog));
        }
        self.vault_dialog()
    }

    /// The folder picker itself. Mirrors `choose_board`'s per-OS handling:
    /// blocking on mac/Windows, the async portal on Linux (whose zbus backend
    /// needs iced's tokio runtime).
    pub(super) fn vault_dialog(&mut self) -> Task<Message> {
        #[cfg(not(target_os = "linux"))]
        {
            let picked = rfd::FileDialog::new()
                .set_title("Choose your Obsidian vault")
                .pick_folder();
            Task::done(Message::VaultChosen(picked))
        }
        #[cfg(target_os = "linux")]
        {
            Task::perform(
                async {
                    rfd::AsyncFileDialog::new()
                        .set_title("Choose your Obsidian vault")
                        .pick_folder()
                        .await
                        .map(|handle| handle.path().to_path_buf())
                },
                Message::VaultChosen,
            )
        }
    }

    /// A vault folder was picked (or the dialog was cancelled). Storing it
    /// drops the cached canvas list, which belonged to the old vault.
    pub(super) fn vault_chosen(&mut self, path: Option<PathBuf>) -> Task<Message> {
        let Some(path) = path else {
            // Cancelled: re-summon the overlay we hid for the dialog.
            return self.resummon_after_dialog(Task::none());
        };
        self.config.vault = path.to_string_lossy().into_owned();
        self.persist_config();
        self.vault_canvases.clear();
        // An explicit vault outranks the root inferred from the open canvas,
        // so re-resolve it — the board's images may have been unresolvable
        // until now (a vault copied without its `.obsidian` folder).
        self.rebase_on_vault();
        // Summon the board back *before* opening the browser: the browser
        // focuses its input, and on Linux the re-summon builds a whole new
        // window (hiding there means closing), so focusing first would aim at
        // one that's on its way out.
        self.resummon_after_dialog(Task::none())
            .chain(Task::done(Message::OpenVaultBrowser))
    }

    /// Re-resolves the open board's base path now that the vault changed, and
    /// reloads the images and notes that hang off it.
    fn rebase_on_vault(&mut self) {
        if self.scratch {
            return; // the scratch board has no file, so no vault-relative paths
        }
        let document = self.current_document();
        let base = super::canvas_file::vault_root(
            &self.canvas_path,
            &document,
            self.vault_path().as_deref(),
        );
        if base == self.base_path {
            return;
        }
        self.base_path = base;
        // Cloned once to release the borrow on `self` that the two loads hold.
        let nodes = self.nodes.clone();
        self.images = self.load_images(&nodes);
        self.notes = self.load_notes(&nodes);
        self.cache.clear();
    }

    /// The browser panel.
    pub(super) fn vault_browser_view<'a>(&'a self, state: &'a picker::State) -> Element<'a, Message> {
        let rows: Vec<Row<'a>> = self
            .vault_matches(&state.query)
            .into_iter()
            .filter_map(|index| self.vault_canvases.get(index))
            .map(|entry| Row {
                title: entry.name().into(),
                // Where it lives, so two boards with the same name are
                // distinguishable — and the board that's already open says so.
                detail: if entry.path == self.canvas_path {
                    "open".into()
                } else {
                    entry.folder().into()
                },
                enabled: true,
                press: Message::OpenVaultCanvas(entry.path.clone()),
            })
            .collect();

        let empty = if self.vault_scanning {
            "Searching the vault…"
        } else if self.vault_canvases.is_empty() {
            "No canvases in this vault"
        } else {
            "No matching canvas"
        };

        picker::view(
            self,
            state,
            Shell {
                placeholder: "Switch to a canvas…",
                input: VAULT_INPUT.clone(),
                list: VAULT_LIST.clone(),
                on_input: Message::VaultBrowserInput,
                on_submit: Message::OpenSelectedCanvas,
                on_scroll: Message::VaultBrowserScrolled,
                on_close: Message::CloseVaultBrowser,
                empty: empty.into(),
            },
            rows,
        )
    }
}
