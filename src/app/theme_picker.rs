//! Choosing a theme, with the board under it changing as you scroll.
//!
//! Shares the command palette's panel ([`super::picker`]). What's particular
//! here is the **preview**: moving the highlight applies the theme
//! immediately, so a theme is chosen by seeing it rather than by reading its
//! name, and dismissing puts back the one you had.
//!
//! Preview has to be cheap enough to run on every keypress, and a full theme
//! change isn't — it drops every bitmap and reclusters, which is tens of
//! milliseconds on a large board. The trick is that dropping the bitmaps is the
//! *cheap* half: a card with no bitmap draws its Markdown live, in whatever
//! colours the theme now says. So a preview recolours and invalidates, and the
//! rasterizing is left until a theme is actually chosen.

use std::sync::LazyLock;

use iced::advanced::widget::Id;
use iced::{Element, Task};

use super::picker::{self, Row, Shell};
use super::{App, Message};
use crate::theme::Theme;
use thinkboard_core::theme::ThemeFile;

pub(crate) static THEME_INPUT: LazyLock<Id> = LazyLock::new(|| Id::new("theme-picker"));
pub(crate) static THEME_LIST: LazyLock<Id> = LazyLock::new(|| Id::new("theme-picker-list"));

/// The open picker: the query and highlight, the themes as of opening, and
/// what to put back if it's dismissed.
pub(crate) struct ThemePicker {
    pub state: picker::State,
    /// Read once when the panel opens, with a nameless entry first for
    /// [`no_theme_label`]. Re-reading the directory on every keystroke would put
    /// disk access behind the arrow keys, and a theme appearing mid-scroll would
    /// move the highlight under the user.
    themes: Vec<ThemeFile>,
    /// What naming no theme gives you: the Obsidian stylesheet if one is
    /// configured, else the built-in palette. Resolved when the panel opens, so
    /// the first row can preview like any other.
    unnamed: Theme,
    /// The theme in use before the panel opened, restored on Esc.
    before: Theme,
    /// Its name, for restoring the config and for marking the row.
    before_name: String,
}

/// Keyboard nav while the picker is open. Enter is the input's `on_submit`.
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
        return Some(Message::CloseThemePicker);
    }
    picker::nav_key(&event).map(Message::ThemePickerNav)
}

impl App {
    pub(super) fn open_theme_picker(&mut self) -> Task<Message> {
        self.search = None; // mutually exclusive with the other panels
        self.command_palette = None;
        self.vault_browser = None;

        // The way back to the stylesheet. A named theme wins over
        // `config.theme_css`, so without this row a vault's Obsidian theme
        // becomes unreachable the moment you try one of ours on.
        let mut themes = vec![ThemeFile { name: String::new(), ..ThemeFile::default() }];
        themes.extend(super::theme_store::all());
        // Open on the theme in use, so the first arrow press is a step from
        // where you are rather than a jump to the top of the list.
        let before_name = self.config.theme.trim().to_owned();
        let selected = themes.iter().position(|t| t.name == before_name).unwrap_or(0);

        let mut unnamed = self.config.clone();
        unnamed.theme.clear();
        let (mut unnamed, _) = super::theme_store::resolve_base(&unnamed);
        unnamed.use_font(&self.config.font);

        self.theme_picker = Some(ThemePicker {
            state: picker::State { selected, ..picker::State::default() },
            themes,
            unnamed,
            before: self.theme,
            before_name,
        });
        let focus = iced::advanced::widget::operate(
            iced::advanced::widget::operation::focusable::focus(THEME_INPUT.clone()),
        );
        // Scroll the list to wherever the current theme sits.
        Task::batch([focus, self.preview_highlighted()])
    }

    /// What a row says. The nameless first entry is named for what choosing it
    /// *does*, which depends on whether there's a stylesheet behind it: with one
    /// configured — which is what a vault sets up — clearing the name hands the
    /// board back to Obsidian's own theme.
    fn theme_label<'a>(&'a self, theme: &'a ThemeFile) -> &'a str {
        if !theme.name.is_empty() {
            return &theme.name;
        }
        match self.config.theme_css.as_deref().map(str::trim) {
            Some(css) if !css.is_empty() => "Obsidian theme",
            _ => "Built-in default",
        }
    }

    /// The themes matching what's been typed, best first.
    pub(super) fn theme_matches(&self, query: &str) -> Vec<usize> {
        let Some(picker) = &self.theme_picker else {
            return Vec::new();
        };
        let mut scored: Vec<(usize, i32)> = picker
            .themes
            .iter()
            .enumerate()
            .filter_map(|(i, theme)| {
                // Name first, then the author at a small penalty — the same
                // title-then-keywords shape the command palette uses.
                // The author is matched but never shown — the same hidden search
                // terms the command palette gives its commands, so "schoonover"
                // finds Solarized without the name being cluttered by it.
                let score = thinkboard_core::fuzzy::score(query, self.theme_label(theme))
                    .or_else(|| {
                        thinkboard_core::fuzzy::score(query, theme.author.as_deref()?)
                            .map(|s| s - 4)
                    })?;
                Some((i, score))
            })
            .collect();
        scored.sort_by_key(|&(_, score)| std::cmp::Reverse(score));
        scored.into_iter().map(|(i, _)| i).collect()
    }

    /// The name of the theme the highlight is on, without holding a borrow on
    /// the picker — which is what lets the caller go on to mutate it.
    pub(super) fn highlighted_theme_name(&self) -> Option<String> {
        self.highlighted_theme().map(|t| t.name.clone())
    }

    /// The theme the highlight is on.
    fn highlighted_theme(&self) -> Option<&ThemeFile> {
        let picker = self.theme_picker.as_ref()?;
        let index = *self.theme_matches(&picker.state.query).get(picker.state.selected)?;
        picker.themes.get(index)
    }

    /// Shows the highlighted theme on the board, without rebuilding anything.
    ///
    /// Colours land at once; the bitmaps they'd be baked into are simply
    /// dropped, which makes every card fall back to drawing its Markdown live.
    /// That's what keeps this fast enough to run on each arrow press.
    pub(super) fn preview_highlighted(&mut self) -> Task<Message> {
        let Some(file) = self.highlighted_theme() else {
            return Task::none();
        };
        let theme = if file.name.is_empty() {
            self.theme_picker.as_ref().map(|p| p.unnamed).unwrap_or_default()
        } else {
            let mut theme = Theme::default();
            theme.apply_resolved(&file.resolve());
            theme.use_font(&self.config.font);
            theme
        };
        self.install_theme(theme);
        // A theme carries typography as well as colour — a family, its weights,
        // its OpenType features — and the shaping caches are keyed by none of
        // those, so a stale entry would measure the previous theme's text.
        crate::canvas::text_raster::clear_caches();
        self.rasters.clear();
        self.clusters.snapshots.clear();
        self.cache.clear();
        Task::none()
    }

    /// Keeps the previewed theme: writes it to the config and rebuilds the
    /// bitmaps the preview was doing without.
    pub(super) fn commit_theme(&mut self) -> Task<Message> {
        let Some(name) = self.highlighted_theme_name() else {
            return Task::none();
        };
        self.theme_picker = None;
        self.config.theme = name;
        self.persist_config();
        self.settings_drafts
            .insert(super::settings::Field::Theme, self.config.theme.clone());
        self.apply_theme_css()
    }

    /// A clicked row: move the highlight onto it, then keep it.
    pub(super) fn pick_theme(&mut self, name: &str) -> Task<Message> {
        if let Some(picker) = &mut self.theme_picker {
            picker.state.query.clear();
            picker.state.selected =
                picker.themes.iter().position(|t| t.name == name).unwrap_or(0);
        }
        self.commit_theme()
    }

    /// Dismisses the picker and puts back the theme that was in use.
    pub(super) fn cancel_theme_picker(&mut self) -> Task<Message> {
        let Some(picker) = self.theme_picker.take() else {
            return Task::none();
        };
        self.config.theme = picker.before_name;
        self.install_theme(picker.before);
        // The preview left the board drawing live text, measured under the
        // theme it was previewing.
        crate::canvas::text_raster::clear_caches();
        self.rasters.clear();
        self.clusters.snapshots.clear();
        self.cache.clear();
        Task::batch([self.refresh_snapshots(), self.schedule_recluster()])
    }

    pub(super) fn theme_picker_view<'a>(&'a self, picker: &'a ThemePicker) -> Element<'a, Message> {
        let rows: Vec<Row<'a>> = self
            .theme_matches(&picker.state.query)
            .into_iter()
            .filter_map(|index| picker.themes.get(index))
            .map(|theme| Row {
                title: self.theme_label(theme).into(),
                // Which one you're on, the way the vault browser marks the open
                // canvas. *Not* the author: attribution belongs in the file, and
                // no editor puts it in front of you every time you pick a theme.
                detail: if theme.name == picker.before_name {
                    "current".into()
                } else {
                    "".into()
                },
                enabled: true,
                font: None,
                press: Message::PickTheme(theme.name.clone()),
            })
            .collect();

        picker::view(
            self,
            &picker.state,
            Shell {
                placeholder: "Theme…",
                input: THEME_INPUT.clone(),
                list: THEME_LIST.clone(),
                on_input: Message::ThemePickerInput,
                on_submit: Message::CommitTheme,
                on_scroll: Message::ThemePickerScrolled,
                on_close: Message::CloseThemePicker,
                empty: "No matching theme".into(),
            },
            rows,
        )
    }
}
