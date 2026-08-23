//! Choosing the AI model, from the list the endpoint itself publishes.
//!
//! Shares the command palette's panel ([`super::picker`]). A model name is
//! exact, unguessable and provider-specific — `anthropic/claude-sonnet-4`,
//! `accounts/fireworks/models/llama-v3p1-70b-instruct` — so typing one from
//! memory is the kind of thing that fails silently at the next question asked.
//!
//! Unlike the other panels this one **fetches**, which gives it two states they
//! don't have: in flight, and failed. Both are shown in the list's own empty
//! row rather than as a dialog, so the panel opens instantly either way and the
//! answer arrives underneath the query you were already typing.

use iced::{Element, Task};

use super::picker::{self, Panel, Row, Shell};
use super::{App, Message};
use thinkboard_core::ai::Model;

/// The open picker: the query and highlight, and what the endpoint said.
pub(crate) struct ModelPicker {
    pub state: picker::State,
    /// The endpoint the list belongs to, so a fetch that lands after the URL
    /// changed is discarded instead of offering another provider's models.
    pub url: String,
    /// Why there are no rows, when there are none. `None` while a fetch is in
    /// flight — which is different from an empty list, and reads differently.
    pub error: Option<String>,
    pub loading: bool,
    /// The model in use when the panel opened, for marking its row.
    before: String,
}

/// Keyboard nav while the picker is open. Enter is the input's `on_submit`.
pub(crate) fn key_event(
    event: iced::Event,
    _status: iced::event::Status,
    _window: iced::window::Id,
) -> Option<Message> {
    picker::panel_key(&event, Panel::Model)
}

/// A context window as it reads in a list: `200k`, `1M`.
fn context(tokens: u64) -> String {
    match tokens {
        0..=9_999 => format!("{tokens}"),
        10_000..=999_999 => format!("{}k", tokens / 1000),
        _ => format!("{}M", tokens / 1_000_000),
    }
}

impl App {
    pub(super) fn open_model_picker(&mut self) -> Task<Message> {
        let opened = self.open_panel(Panel::Model);

        let url = self.config.ai_url.trim().to_owned();
        // Whatever the last fetch for this endpoint found, shown at once — the
        // same reason the vault browser keeps its last scan. A different
        // endpoint starts empty rather than showing the old provider's models.
        let cached = self.ai.models.as_ref().is_some_and(|(cached, _)| *cached == url);
        self.model_picker = Some(ModelPicker {
            state: picker::State::default(),
            url: url.clone(),
            error: None,
            loading: !cached,
            before: self.config.ai_model.trim().to_owned(),
        });

        if url.is_empty() {
            if let Some(picker) = &mut self.model_picker {
                picker.loading = false;
                picker.error = Some("No API URL is set — choose a provider in settings".into());
            }
            return opened;
        }
        // Refetched even when cached: a list is cheap, and a stale one is how
        // you end up picking a model the provider has since retired.
        let key = self.config.ai_key().to_owned();
        let fetch = Task::perform(
            async move {
                let models = thinkboard_core::ai::models(&url, &key).await;
                (url, models)
            },
            |(url, models)| Message::ModelsFetched(url, models.map_err(|e| e.to_string())),
        );
        Task::batch([opened, fetch])
    }

    /// A finished fetch. Kept against the endpoint it was for, so switching
    /// providers mid-flight can't leave one list labelled as another's.
    pub(super) fn models_fetched(&mut self, url: String, models: Result<Vec<Model>, String>) {
        match models {
            Ok(models) => {
                self.ai.models = Some((url.clone(), models));
                if let Some(picker) = self.model_picker.as_mut().filter(|p| p.url == url) {
                    picker.loading = false;
                    picker.error = None;
                    picker.state.reset();
                }
            }
            Err(error) => {
                if let Some(picker) = self.model_picker.as_mut().filter(|p| p.url == url) {
                    picker.loading = false;
                    picker.error = Some(error);
                }
            }
        }
    }

    /// The models fetched for the open picker's endpoint.
    fn models(&self) -> &[Model] {
        let Some(picker) = &self.model_picker else {
            return &[];
        };
        self.ai
            .models
            .as_ref()
            .filter(|(url, _)| *url == picker.url)
            .map_or(&[], |(_, models)| models.as_slice())
    }

    /// The models matching what's been typed, best first.
    pub(super) fn model_matches(&self, query: &str) -> Vec<usize> {
        // The id first — it's what gets sent, and what a half-remembered name is
        // usually a fragment of; the display name is the hidden term behind it.
        thinkboard_core::fuzzy::rank(self.models(), |model| {
            thinkboard_core::fuzzy::score_alias(query, &model.id, model.name.as_deref())
        })
    }

    /// The id the highlight is on.
    pub(super) fn highlighted_model(&self) -> Option<String> {
        let picker = self.model_picker.as_ref()?;
        let index = *self.model_matches(&picker.state.query).get(picker.state.selected)?;
        self.models().get(index).map(|model| model.id.clone())
    }

    /// Keeps the highlighted model (Enter).
    pub(super) fn commit_model(&mut self) -> Task<Message> {
        let Some(id) = self.highlighted_model() else {
            return Task::none();
        };
        self.model_picker = None;
        self.config.ai_model = id;
        self.persist_config();
        self.settings_drafts
            .insert(super::settings::Field::AiModel, self.config.ai_model.clone());
        Task::none()
    }

    /// A clicked row: move the highlight onto it, then keep it.
    pub(super) fn pick_model(&mut self, id: &str) -> Task<Message> {
        self.model_picker = None;
        self.config.ai_model = id.to_owned();
        self.persist_config();
        self.settings_drafts
            .insert(super::settings::Field::AiModel, self.config.ai_model.clone());
        Task::none()
    }

    pub(super) fn model_picker_view<'a>(&'a self, picker: &'a ModelPicker) -> Element<'a, Message> {
        let models = self.models();
        let rows: Vec<Row<'a>> = self
            .model_matches(&picker.state.query)
            .into_iter()
            .filter_map(|index| models.get(index))
            .map(|model| Row {
                // The id, not the display name: it's what gets sent, and seeing
                // it is how you know the right one was chosen.
                title: model.id.as_str().into(),
                detail: match (model.id == picker.before, model.context) {
                    (true, _) => "current".into(),
                    // Otherwise the context window, which is usually what
                    // decides between two models with near-identical names.
                    (false, Some(tokens)) => context(tokens).into(),
                    (false, None) => "".into(),
                },
                enabled: true,
                font: None,
                press: Message::PickModel(model.id.clone()),
            })
            .collect();

        picker::view(
            self,
            &picker.state,
            Shell {
                panel: Panel::Model,
                placeholder: "Model…",
                on_submit: Message::CommitModel,
                // The three reasons a list can be empty read differently, and
                // only one of them is the user's to fix.
                empty: match (&picker.error, picker.loading) {
                    (Some(error), _) => format!("Couldn't list models — {error}").into(),
                    (None, true) => "Asking the endpoint…".into(),
                    (None, false) => "No matching model".into(),
                },
            },
            rows,
        )
    }
}
