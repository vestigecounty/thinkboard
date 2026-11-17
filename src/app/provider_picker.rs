//! Choosing the AI provider, from the settings form's Provider row.
//!
//! Shares the command palette's panel ([`super::picker`]), opened in place
//! under the Provider row, with the form still there beneath it — or, from
//! the palette's "Change AI provider…", over the board. It was a dropdown, which
//! at sixteen presets was a long list in no order anyone could see; here
//! they're in sections by what sort of provider each is
//! ([`thinkboard_core::ai::ProviderKind`]), and typing narrows them.
//! Choosing one is the form's own `PickPreset`, so what follows a choice —
//! the URL filled in, the model picker opened — is the same either way.
//!
//! **The runtimes on this machine are the ones running.** Each listens on a
//! port of its own, so the picker asks each as it opens
//! ([`thinkboard_core::ai::answers`]), and lists those that answer — and the
//! one in use, whether it does or not, saying which. The last answer is kept,
//! so it opens on that while it asks again, as the vault browser does.

use iced::{Element, Task};

use super::picker::{self, Header, Panel, Row, Shell};
use super::settings::{Msg, PROVIDER_OFF};
use super::{App, Message};
use thinkboard_core::ai::{PRESETS, ProviderKind};

/// The row after the presets: any other OpenAI-compatible URL.
const CUSTOM: &str = "Custom";

/// The open picker.
pub(crate) struct ProviderPicker {
    pub state: picker::State,
    /// Opened from the settings form's Provider row, and drawn over the form
    /// there; else from the palette, over the board.
    pub over_settings: bool,
    pub place: picker::Place,
}

/// What stands in the section of runtimes on this machine when none
/// answered: gone altogether, it would leave no sign there are any to run.
const NONE_RUNNING: &str = "None running";

/// Keyboard nav while the picker is open. Enter is the input's `on_submit`.
pub(crate) fn key_event(
    event: iced::Event,
    _status: iced::event::Status,
    _window: iced::window::Id,
) -> Option<Message> {
    picker::panel_key(&event, Panel::Provider)
}

/// What a section says over its sort of provider. Words, not a mark: a
/// provider is named, never drawn.
fn heading(kind: Option<ProviderKind>) -> &'static str {
    match kind {
        Some(ProviderKind::Local) => "On this machine",
        Some(ProviderKind::Host) => "Many models, one key",
        Some(ProviderKind::Maker) => "Model makers",
        None => "Any other endpoint",
    }
}

/// The sort of provider a row is, for its section — and, searched, as a
/// term it's found by too ("local"). None for Off and Custom.
fn kind(name: &str) -> Option<ProviderKind> {
    if name == NONE_RUNNING {
        return Some(ProviderKind::Local);
    }
    PRESETS.iter().find(|preset| preset.name == name).map(|preset| preset.kind)
}

/// The runtimes on this machine, by their presets.
fn local() -> impl Iterator<Item = &'static thinkboard_core::ai::Preset> {
    PRESETS.iter().filter(|preset| preset.kind == ProviderKind::Local)
}

impl App {
    /// The picker, from the palette: over the board, as the others open. It
    /// opens on the provider in use, and asks which runtimes here are running.
    pub(super) fn open_provider_picker(&mut self) -> Task<Message> {
        let opened = self.open_panel(Panel::Provider);
        let current = self.provider_in_use();
        let selected = self.provider_listing("").0.iter().position(|name| *name == current).unwrap_or(0);
        let state = picker::State { selected, ..picker::State::default() };
        self.provider_picker = Some(ProviderPicker { state, over_settings: false, place: picker::Place::Top });
        Task::batch([opened, ask_whats_running()])
    }

    /// The picker from the form's Provider row: in place, under it.
    pub(super) fn open_provider_picker_over_settings(&mut self) -> Task<Message> {
        let opened = self.open_provider_picker();
        if let Some(picker) = &mut self.provider_picker {
            picker.over_settings = true;
            picker.place = picker::Place::Finding;
        }
        Task::batch([opened, picker::find_anchor(Panel::Provider).map(|at| Message::Placed(Panel::Provider, at))])
    }

    /// The runtimes here that answered, in: the highlight stays on the row
    /// it was on as rows come and go above it.
    pub(super) fn running_here(&mut self, running: Vec<&'static str>) -> Task<Message> {
        let highlighted = self
            .provider_picker
            .as_ref()
            .and_then(|picker| self.provider_listing(&picker.state.query).0.get(picker.state.selected).copied());
        self.ai.running_here = Some(running);
        let Some(query) = self.provider_picker.as_ref().map(|picker| picker.state.query.clone()) else {
            return Task::none();
        };
        let listed = self.provider_listing(&query).0;
        let headers = self.panel_headers(Panel::Provider);
        let Some(picker) = &mut self.provider_picker else {
            return Task::none();
        };
        let state = &mut picker.state;
        state.selected = highlighted.and_then(|name| listed.iter().position(|row| *row == name)).unwrap_or(0);
        picker::navigate(state, picker::Nav::Step(0), listed.len(), &headers, Panel::Provider.list_id())
    }

    /// The provider the form says is in use: a preset, Custom, or Off.
    fn provider_in_use(&self) -> String {
        if self.settings_custom_ai && !self.config.ai_enabled() {
            CUSTOM.to_owned()
        } else {
            super::settings::current_preset(&self.config.ai_url)
        }
    }

    /// Every row, in order: Off, the presets a kind at a time — of the
    /// runtimes here, those that answered and the one in use — then Custom.
    fn provider_names(&self) -> Vec<&'static str> {
        let current = self.provider_in_use();
        let running = self.ai.running_here.as_deref();
        let listed = |name: &str| running.is_some_and(|running| running.contains(&name)) || name == current;
        let mut names = vec![PROVIDER_OFF];
        names.extend(local().map(|preset| preset.name).filter(|name| listed(name)));
        // Once asked, a section that would be empty says so.
        if running.is_some() && !local().any(|preset| listed(preset.name)) {
            names.push(NONE_RUNNING);
        }
        names.extend(PRESETS.iter().filter(|preset| preset.kind != ProviderKind::Local).map(|preset| preset.name));
        names.push(CUSTOM);
        names
    }

    /// The rows for `query`, in the order they're shown, and the sections
    /// over them. With nothing typed, a section to each sort of provider,
    /// Off first under none; typed, one ranked list, a row found by its
    /// sort's heading too, after those found by name.
    pub(super) fn provider_listing(&self, query: &str) -> (Vec<&'static str>, Vec<(usize, &'static str)>) {
        let names = self.provider_names();
        if !query.trim().is_empty() {
            let choices: Vec<&'static str> = names.into_iter().filter(|name| *name != NONE_RUNNING).collect();
            let ranked = thinkboard_core::fuzzy::rank(&choices, |name| {
                let sort = (*name != PROVIDER_OFF).then(|| heading(kind(name)));
                thinkboard_core::fuzzy::score_alias(query, name, sort)
            });
            return (ranked.into_iter().map(|index| choices[index]).collect(), Vec::new());
        }
        let mut headers = Vec::new();
        for (index, name) in names.iter().enumerate().skip(1) {
            if index == 1 || kind(name) != kind(names[index - 1]) {
                headers.push((index, heading(kind(name))));
            }
        }
        (names, headers)
    }

    /// Enter: the highlighted provider, chosen as a click would.
    pub(super) fn commit_provider(&mut self) -> Task<Message> {
        let Some(state) = self.provider_picker.as_ref().map(|picker| &picker.state) else {
            return Task::none();
        };
        let Some(name) = self.provider_listing(&state.query).0.get(state.selected).copied() else {
            return Task::none();
        };
        if name == NONE_RUNNING {
            return Task::none();
        }
        self.apply_setting(Msg::PickPreset(name.to_owned()))
    }

    pub(super) fn provider_picker_view<'a>(&'a self, picker: &'a ProviderPicker) -> Element<'a, Message> {
        let state = &picker.state;
        let current = self.provider_in_use();
        let running = self.ai.running_here.as_deref().unwrap_or_default();
        let (names, headers) = self.provider_listing(&state.query);
        let rows: Vec<Row<'a>> = names
            .into_iter()
            .map(|name| {
                let detail = if name == NONE_RUNNING {
                    local().map(|preset| preset.name).collect::<Vec<_>>().join(" · ")
                } else if name == current && kind(name) == Some(ProviderKind::Local) && !running.contains(&name) {
                    // Listed though it didn't answer: it's the one in use.
                    "current · not running".to_owned()
                } else if name == current {
                    "current".to_owned()
                } else {
                    String::new()
                };
                Row {
                    matched: thinkboard_core::fuzzy::matched(&state.query, name),
                    title: name.into(),
                    detail: detail.into(),
                    detail_tint: None,
                    gauge: None,
                    keys: Vec::new(),
                    enabled: name != NONE_RUNNING,
                    font: None,
                    glyph: None,
                    press: Message::Settings(Msg::PickPreset(name.to_owned())),
                }
            })
            .collect();
        picker::view(
            self,
            state,
            Shell {
                panel: Panel::Provider,
                placeholder: "Provider…",
                glyph: super::chrome::Glyph::Magnifier,
                on_submit: Message::CommitProvider,
                verb: "Choose",
                loading: None,
                notice: None,
                empty: "No matching provider".into(),
                headers: headers
                    .into_iter()
                    .map(|(heads, label)| Header { heads, label: label.into(), tint: None, warning: None })
                    .collect(),
                anchor: match picker.place {
                    picker::Place::Under(anchor) => Some(anchor),
                    _ => None,
                },
            },
            rows,
        )
    }
}

/// Asks every runtime here at once whether it's running. Not under test:
/// what answered on the developer's machine would decide what a test sees,
/// so tests and the tour say what's running themselves (`RunningHere`).
fn ask_whats_running() -> Task<Message> {
    if cfg!(test) {
        return Task::none();
    }
    Task::perform(
        async {
            let asked = local().map(|preset| async move { thinkboard_core::ai::answers(preset.url).await.then_some(preset.name) });
            iced::futures::future::join_all(asked).await.into_iter().flatten().collect()
        },
        Message::RunningHere,
    )
}
