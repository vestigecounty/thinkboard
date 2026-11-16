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

use super::picker::{self, Header, Panel, Row, Shell};
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
    /// Opened from the settings window, and drawn over the form there rather
    /// than over the board — see [`App::open_model_picker_from_settings`].
    pub over_settings: bool,
    /// The model in use when the panel opened, for marking its row.
    before: String,
    /// When it opened, which is when the skeleton's light starts across.
    opened: std::time::Instant,
    /// A line over the list saying what led here, where that wants saying:
    /// a sign-in that worked.
    pub notice: Option<&'static str>,
}

/// Keyboard nav while the picker is open. Enter is the input's `on_submit`.
pub(crate) fn key_event(
    event: iced::Event,
    _status: iced::event::Status,
    _window: iced::window::Id,
) -> Option<Message> {
    picker::panel_key(&event, Panel::Model)
}

/// What a free section's header warns of, at its right. OpenRouter doesn't
/// train on what it's sent, but many of its free models' providers say they
/// log it and may, and the free sections come first, the router to them at
/// the top. Said where the eye is when choosing from one: a line over the
/// whole list stood there whatever was chosen, and as a muted note it read as
/// a hint the list didn't need.
const FREE_WARNING: &str = "May train on prompts";

/// The same, on the settings' Model row while a free model is in use.
pub(super) const FREE_CAVEAT: &str = "What you send a free model may be kept by its provider and used for training.";

/// Which a section of models is, for the colour its header is set in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Cost {
    Free,
    Paid,
}

/// The colour a section of models is told by: the free ones in the green a
/// card can be given, softened toward the theme's muted text so it's a tint
/// rather than a signal, and the paid in the theme's accent — each then taken
/// toward the panel's text until it reads on the panel as a heading has to
/// (3:1), so a light theme gets it darker and a dark one lighter, and neither
/// leaves the colours the board already uses.
fn tint(cost: Cost, theme: &crate::theme::Theme) -> iced::Color {
    let base = match cost {
        Cost::Free => thinkboard_core::model::parse_color("4")
            .map_or(theme.accent, |green| super::chrome::mix(crate::document::IntoColor::into_color(green), theme.text_muted, 0.3)),
        Cost::Paid => theme.accent,
    };
    readable(base, theme)
}

/// A warning's colour: a card's orange, taken toward the text as [`tint`]'s
/// are until it reads on the panel.
pub(super) fn warning_tint(theme: &crate::theme::Theme) -> iced::Color {
    let orange = thinkboard_core::model::parse_color("2").map(crate::document::IntoColor::into_color);
    readable(orange.unwrap_or(theme.accent), theme)
}

/// `base`, taken toward the text until it reads on the panel at 3:1.
fn readable(base: iced::Color, theme: &crate::theme::Theme) -> iced::Color {
    use thinkboard_core::theme::{ACCENT_CONTRAST, contrast};
    let rgba = |c: iced::Color| thinkboard_core::color::Rgba { r: c.r, g: c.g, b: c.b, a: 1.0 };
    let surface = rgba(theme.elevated());
    (0..=10)
        .map(|step| super::chrome::mix(base, theme.text, step as f32 / 10.0))
        .find(|color| contrast(rgba(*color), surface) >= ACCENT_CONTRAST)
        .unwrap_or(theme.text)
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
            over_settings: false,
            before: self.config.ai_model.trim().to_owned(),
            opened: std::time::Instant::now(),
            notice: None,
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
        // Open on the model in use, where a kept list has it — as the theme
        // picker opens on the theme — or it could be anywhere below the free
        // ones, out of sight.
        let before = self.config.ai_model.trim().to_owned();
        let current = self.highlight_model(&before);
        let fetch = Task::perform(
            async move {
                let models = thinkboard_core::ai::models(&url, &key).await;
                (url, models)
            },
            |(url, models)| Message::ModelsFetched(url, models.map_err(|e| e.to_string())),
        );
        Task::batch([opened, current, fetch])
    }

    /// The picker, from the settings form, where the model is chosen
    /// alongside the provider — from the form's "Choose…", or by itself once
    /// a provider is picked that can answer, or a sign-in has made a key.
    ///
    /// A new config names a local runtime's model, which no hosted provider
    /// serves, so picking one and asking used to fail on the first question.
    /// In the board, the form closes and the picker takes its place: drawn
    /// over the form, it was a panel over a panel, two things open at once
    /// and neither clearly the one to answer. The settings window has no
    /// board to show it over, so there it's drawn over the form.
    pub(super) fn open_model_picker_from_settings(&mut self) -> Task<Message> {
        if self.settings_overlay {
            let closed = self.close_settings_overlay();
            return Task::batch([closed, self.open_model_picker()]);
        }
        let opened = self.open_model_picker();
        if let Some(picker) = &mut self.model_picker {
            picker.over_settings = self.settings_window.is_some();
        }
        opened
    }

    /// A finished fetch. Kept against the endpoint it was for, so switching
    /// providers mid-flight can't leave one list labelled as another's.
    pub(super) fn models_fetched(&mut self, url: String, models: Result<Vec<Model>, String>) -> Task<Message> {
        match models {
            Ok(models) => {
                // The highlight stays on its model as the list under it is
                // replaced — the one in use, unless it's been moved — rather
                // than jumping to the top when a refetch lands.
                let open = self.model_picker.as_ref().is_some_and(|p| p.url == url);
                let keep = self
                    .highlighted_model()
                    .or_else(|| self.model_picker.as_ref().map(|picker| picker.before.clone()));
                self.ai.models = Some((url.clone(), models));
                if let Some(picker) = self.model_picker.as_mut().filter(|p| p.url == url) {
                    picker.loading = false;
                    picker.error = None;
                    picker.state.reset();
                }
                if let Some(id) = keep.filter(|_| open) {
                    return self.highlight_model(&id);
                }
            }
            Err(error) => {
                if let Some(picker) = self.model_picker.as_mut().filter(|p| p.url == url) {
                    picker.loading = false;
                    picker.error = Some(error);
                }
            }
        }
        Task::none()
    }

    /// Moves the open picker's highlight onto `id`, where it's listed, and
    /// scrolls the list to it.
    fn highlight_model(&mut self, id: &str) -> Task<Message> {
        let Some(picker) = &self.model_picker else {
            return Task::none();
        };
        let listed = self.model_matches(&picker.state.query);
        let models = self.models();
        let Some(at) = listed.iter().position(|&index| models.get(index).is_some_and(|model| model.id == id)) else {
            return Task::none();
        };
        let headers = self.panel_headers(Panel::Model);
        let Some(picker) = &mut self.model_picker else {
            return Task::none();
        };
        picker.state.selected = at;
        picker::navigate(&mut picker.state, picker::Nav::Step(0), listed.len(), &headers, Panel::Model.list_id())
    }

    /// The models fetched for the open picker's endpoint.
    pub(super) fn models(&self) -> &[Model] {
        let Some(picker) = &self.model_picker else {
            return &[];
        };
        self.ai
            .models
            .as_ref()
            .filter(|(url, _)| *url == picker.url)
            .map_or(&[], |(_, models)| models.as_slice())
    }

    /// Whether questions go to a free model: as the endpoint's list says, where
    /// one's kept for it, or else by the model's name — the list is fetched
    /// only when the picker opens.
    pub(super) fn asks_a_free_model(&self) -> bool {
        let (url, model) = (self.config.ai_url.trim(), self.config.ai_model.trim());
        self.ai
            .models
            .as_ref()
            .filter(|(kept, _)| kept == url)
            .and_then(|(_, models)| models.iter().find(|listed| listed.id == model))
            .map_or_else(|| thinkboard_core::ai::free_by_name(model), |listed| listed.free)
    }

    /// Where Ask AI sends a question, as the menu and the palette say beside
    /// it: the provider, and whether the model is a free one.
    pub(super) fn ask_ai_destination(&self) -> String {
        let place = thinkboard_core::ai::destination(&self.config.ai_url);
        if self.asks_a_free_model() { format!("{place} · free") } else { place }
    }

    /// The models matching what's been typed, best first.
    /// Since when the open picker has been waiting on a list with nothing to
    /// show meanwhile — which is when its skeleton is up, and wants frames.
    pub(super) fn model_picker_waiting(&self) -> Option<std::time::Instant> {
        let picker = self.model_picker.as_ref()?;
        (picker.loading && picker.error.is_none() && self.models().is_empty()).then_some(picker.opened)
    }

    pub(super) fn model_matches(&self, query: &str) -> Vec<usize> {
        self.model_listing(query).0
    }

    /// The models listed for `query`, in the order they're shown, and the
    /// sections over them. With nothing typed, the free ones come first:
    /// the router to them, which picks whichever is up, then the models
    /// themselves, under a header that says what free means — limited, and
    /// the offers come and go, so none is put forward as one to rely on —
    /// then the paid. Sections only where the endpoint prices its models,
    /// which is where any is free, so the rest are the priced ones.
    ///
    /// Typed into, the sections stay, each ranked within, and the one holding
    /// the best match leads — so the highlight and Enter are on it, as in one
    /// ranked list, and a free model found is still under a header warning
    /// of what its provider may do. A tie goes to the paid: a model searched
    /// for by name is more likely meant than its free variant, rate-limited
    /// and perhaps trained on what it's sent.
    pub(super) fn model_listing(&self, query: &str) -> (Vec<usize>, Vec<(usize, &'static str, Cost)>) {
        // The id first — it's what gets sent, and what a half-remembered name is
        // usually a fragment of; the display name is the hidden term behind it.
        let score = |model: &Model| thinkboard_core::fuzzy::score_alias(query, &model.id, model.name.as_deref());
        let models = self.models();
        let ranked = thinkboard_core::fuzzy::rank(models, score);
        let sections = |rows: &[usize]| {
            let (free, paid): (Vec<usize>, Vec<usize>) = rows.iter().partition(|&&index| models[index].free);
            let (router, free): (Vec<usize>, Vec<usize>) =
                free.into_iter().partition(|&index| models[index].id == thinkboard_core::ai::FREE_ROUTER);
            [
                ("Free · routes to an available model", router, Cost::Free),
                ("Free · rate-limited", free, Cost::Free),
                ("Paid", paid, Cost::Paid),
            ]
        };
        // Whether there are sections is the whole list's to say, so a search
        // down to one model still says which it is.
        let all: Vec<usize> = (0..models.len()).collect();
        if sections(&all).iter().filter(|(_, rows, _)| !rows.is_empty()).count() < 2 {
            return (ranked, Vec::new());
        }
        let mut found: Vec<_> = sections(&ranked).into_iter().filter(|(_, rows, _)| !rows.is_empty()).collect();
        if !query.trim().is_empty() {
            found.sort_by_key(|(_, rows, cost)| (std::cmp::Reverse(score(&models[rows[0]])), *cost == Cost::Free));
        }
        let (mut order, mut headers) = (Vec::new(), Vec::new());
        for (label, rows, cost) in found {
            headers.push((order.len(), label, cost));
            order.extend(rows);
        }
        (order, headers)
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
                matched: thinkboard_core::fuzzy::matched(&picker.state.query, &model.id),
                detail: match (model.id == picker.before, model.context) {
                    (true, _) => "current".into(),
                    // Otherwise the context window, which is usually what
                    // decides between two models with near-identical names —
                    // and whether it's free, which a search shows no section for.
                    (false, Some(tokens)) if model.free => format!("free · {}", context(tokens)).into(),
                    (false, Some(tokens)) => context(tokens).into(),
                    (false, None) if model.free => "free".into(),
                    (false, None) => "".into(),
                },
                detail_tint: (model.free && model.id != picker.before).then(|| tint(Cost::Free, &self.theme)),
                keys: Vec::new(),
                enabled: true,
                font: None,
                glyph: None,
                press: Message::PickModel(model.id.clone()),
            })
            .collect();

        let headers = self.model_listing(&picker.state.query).1;
        picker::view(
            self,
            &picker.state,
            Shell {
                panel: Panel::Model,
                placeholder: "Model…",
                notice: picker.notice.map(|words| (words.into(), tint(Cost::Free, &self.theme))),
                glyph: super::chrome::Glyph::Magnifier,
                on_submit: Message::CommitModel,
                verb: "Choose",
                // A free section says what its provider may do with what's
                // sent, where the eye is when choosing from it — a line over
                // the list stood there whatever was chosen.
                headers: headers
                    .into_iter()
                    .map(|(heads, label, cost)| Header {
                        heads,
                        label: label.into(),
                        tint: Some(tint(cost, &self.theme)),
                        warning: (cost == Cost::Free).then(|| (FREE_WARNING.into(), warning_tint(&self.theme))),
                    })
                    .collect(),
                // Asked, and nothing kept from before: rows still to come, at
                // the list's full height, rather than a one-row panel that grows.
                loading: self.model_picker_waiting(),
                // The three reasons a list can be empty read differently, and
                // only one of them is the user's to fix.
                empty: match (&picker.error, picker.loading) {
                    // Each says what went wrong itself — "Couldn't reach LM
                    // Studio", "wants an API key" — so it stands alone.
                    (Some(error), _) => error.as_str().into(),
                    (None, true) => "Asking the endpoint…".into(),
                    (None, false) => "No matching model".into(),
                },
            },
            rows,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use thinkboard_core::theme::{ACCENT_CONTRAST, contrast};

    fn rgba(c: iced::Color) -> thinkboard_core::color::Rgba {
        thinkboard_core::color::Rgba { r: c.r, g: c.g, b: c.b, a: 1.0 }
    }

    #[test]
    fn a_sections_colour_reads_on_the_panel_in_a_light_theme_and_a_dark() {
        let dark = crate::theme::Theme::default();
        let light = crate::theme::Theme {
            background: iced::Color::from_rgb(0.97, 0.97, 0.96),
            card_fill: iced::Color::WHITE,
            text: iced::Color::from_rgb(0.13, 0.13, 0.15),
            text_muted: iced::Color::from_rgb(0.42, 0.42, 0.45),
            accent: iced::Color::from_rgb(0.35, 0.62, 1.0),
            ..dark
        };
        for theme in [dark, light] {
            for cost in [Cost::Free, Cost::Paid] {
                let ratio = contrast(rgba(tint(cost, &theme)), rgba(theme.elevated()));
                assert!(ratio >= ACCENT_CONTRAST, "{cost:?} at {ratio:.2}");
            }
        }
        // Green stays green: taken toward the text only as far as it must.
        let free = tint(Cost::Free, &crate::theme::Theme::default());
        assert!(free.g > free.r && free.g > free.b, "{free:?}");
    }
}
