//! Choosing the body font, with the board under it changing as you scroll.
//!
//! Shares the command palette's panel ([`super::picker`]) and the theme
//! picker's preview trick: moving the highlight applies the font immediately,
//! and dismissing puts back the one you had. Each row is drawn *in the family
//! it names*, which a list of a few hundred is unusable without.
//!
//! The font is a config value rather than part of the theme, so it survives
//! changing themes — see [`crate::config::Config::font`]. That's also why the
//! first row exists: it clears the override and hands the choice back to
//! whatever theme is in use.

use iced::{Element, Task};

use super::picker::{self, Panel, Row, Shell};
use super::{App, Message};
use crate::canvas::text_raster::{self, FontFamily};
use crate::theme::Theme;

/// The first row: no override, i.e. a blank `config.font`.
const THEME_DEFAULT: &str = "Theme default";

/// The open picker: the query and highlight, the families as of opening, and
/// what to put back if it's dismissed.
pub(crate) struct FontPicker {
    pub state: picker::State,
    /// Every family, a nameless one first for [`THEME_DEFAULT`]. Read once when
    /// the panel opens: walking the font database on each keystroke would put it
    /// behind the arrow keys, and a family appearing mid-scroll would move the
    /// highlight under the user.
    entries: Vec<FontFamily>,
    /// The theme *before* its font was overridden, which is what lets the first
    /// row put the theme's own face back — `self.theme` can't say what that was,
    /// since the override has already replaced it.
    base: Theme,
    /// The theme in use before the panel opened, restored on Esc.
    before: Theme,
    /// The configured font then, for restoring and for marking the row.
    before_font: String,
}

/// Keyboard nav while the picker is open. Enter is the input's `on_submit`.
pub(crate) fn key_event(
    event: iced::Event,
    _status: iced::event::Status,
    _window: iced::window::Id,
) -> Option<Message> {
    picker::panel_key(&event, Panel::Font)
}

/// What a row says: the family, or the standing-in name for the empty entry.
fn label(entry: &str) -> &str {
    if entry.is_empty() { THEME_DEFAULT } else { entry }
}

impl App {
    pub(super) fn open_font_picker(&mut self) -> Task<Message> {
        let opened = self.open_panel(Panel::Font);

        // The empty name is the way back to the theme's own font; it has no
        // face of its own, so it's drawn in the panel's like any other row that
        // can't set itself.
        let mut entries = vec![FontFamily { name: String::new(), weight: 400, legible: false }];
        entries.extend(text_raster::families());
        // Open on the font in use, so the first arrow press is a step from
        // where you are rather than a jump to the top of the list.
        let before_font = self.config.font.trim().to_owned();
        let selected = entries.iter().position(|e| e.name == before_font).unwrap_or(0);
        let (base, _) = super::theme_store::resolve_base(&self.config);

        self.font_picker = Some(FontPicker {
            state: picker::State { selected, ..picker::State::default() },
            entries,
            base,
            before: self.theme,
            before_font,
        });
        Task::batch([opened, self.preview_highlighted_font()])
    }

    /// The families matching what's been typed, best first.
    pub(super) fn font_matches(&self, query: &str) -> Vec<usize> {
        let Some(picker) = &self.font_picker else {
            return Vec::new();
        };
        thinkboard_core::fuzzy::rank(&picker.entries, |entry| {
            thinkboard_core::fuzzy::score(query, label(&entry.name))
        })
    }

    /// The family the highlight is on — `""` for the theme's own.
    pub(super) fn highlighted_font(&self) -> Option<String> {
        let picker = self.font_picker.as_ref()?;
        let index = *self.font_matches(&picker.state.query).get(picker.state.selected)?;
        picker.entries.get(index).map(|entry| entry.name.clone())
    }

    /// Shows the highlighted font on the board, without rebuilding anything.
    ///
    /// Cheap for the same reason the theme preview is: dropping the bitmaps
    /// makes every card fall back to drawing its Markdown live, in whatever
    /// face the theme now names. Unlike a colour change this also has to drop
    /// the *shaping* caches — they're keyed by text and size but not by the
    /// face those were measured with, so a stale entry would keep the old
    /// typeface's metrics under the new one's glyphs.
    pub(super) fn preview_highlighted_font(&mut self) -> Task<Message> {
        let Some(entry) = self.highlighted_font() else {
            return Task::none();
        };
        let mut theme = self.font_picker.as_ref().map(|p| p.base).unwrap_or_default();
        theme.use_font(&entry);
        self.install_theme(theme);
        text_raster::clear_caches();
        self.drop_text_bitmaps();
        Task::none()
    }

    /// Keeps the previewed font: writes it to the config and rebuilds the
    /// bitmaps the preview was doing without.
    pub(super) fn commit_font(&mut self) -> Task<Message> {
        let Some(name) = self.highlighted_font() else {
            return Task::none();
        };
        self.font_picker = None;
        self.config.font = name;
        self.persist_config();
        self.settings_drafts
            .insert(super::settings::Field::Font, self.config.font.clone());
        // Re-resolves from the config, which is where the override now lives —
        // and re-rasterizes, since the cards are drawing live after the preview.
        self.apply_theme_css()
    }

    /// A clicked row: move the highlight onto it, then keep it.
    pub(super) fn pick_font(&mut self, name: &str) -> Task<Message> {
        if let Some(picker) = &mut self.font_picker {
            picker.state.query.clear();
            picker.state.selected =
                picker.entries.iter().position(|e| e.name == name).unwrap_or(0);
        }
        self.commit_font()
    }

    /// Dismisses the picker and puts back the font that was in use.
    pub(super) fn cancel_font_picker(&mut self) -> Task<Message> {
        let Some(picker) = self.font_picker.take() else {
            return Task::none();
        };
        self.config.font = picker.before_font;
        self.install_theme(picker.before);
        // The preview left the board drawing live text, shaped in a face that
        // is no longer the one in use.
        text_raster::clear_caches();
        self.drop_text_bitmaps();
        Task::batch([self.refresh_snapshots(), self.schedule_recluster()])
    }

    pub(super) fn font_picker_view<'a>(&'a self, picker: &'a FontPicker) -> Element<'a, Message> {
        let rows: Vec<Row<'a>> = self
            .font_matches(&picker.state.query)
            .into_iter()
            .filter_map(|index| picker.entries.get(index))
            .map(|entry| Row {
                title: label(&entry.name).into(),
                detail: if entry.name == picker.before_font { "current".into() } else { "".into() },
                enabled: true,
                // The point of the list: each name set in its own face — except
                // where that face can't spell it, which would put an unreadable
                // row in front of a font you then can't identify.
                font: entry.legible.then(|| iced::Font {
                    family: iced::font::Family::Name(crate::theme::intern(&entry.name)),
                    // iced matches weight exactly too, so a family whose regular
                    // isn't a 400 needs asking for by the weight it has.
                    weight: crate::theme::iced_weight(entry.weight),
                    ..iced::Font::DEFAULT
                }),
                press: Message::PickFont(entry.name.clone()),
            })
            .collect();

        picker::view(
            self,
            &picker.state,
            Shell {
                panel: Panel::Font,
                placeholder: "Font…",
                on_submit: Message::CommitFont,
                empty: "No matching font".into(),
            },
            rows,
        )
    }
}
