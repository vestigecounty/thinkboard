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

use super::chrome::Glyph;
use super::picker::{self, Header, Panel, Row, Shell};
use super::{App, Message};
use thinkboard_core::ai::Model;

/// What Ask AI says beside it ([`App::ask_ai_label`]).
#[derive(Debug, PartialEq)]
pub(super) struct AskAiLabel {
    /// The model a question goes to, or where it goes where none's named.
    pub what: String,
    /// Whether it's a free model, whose provider may train on what it's sent.
    pub free: bool,
}

impl AskAiLabel {
    /// What follows the model: whether it's a free one, where its name
    /// doesn't say so already (OpenRouter's router to them) — kept whole
    /// when the row is too short for the lot.
    pub(super) fn after(&self) -> &'static str {
        if self.free && !self.what.to_lowercase().contains("free") { " · free" } else { "" }
    }

    /// The words, in full.
    pub(super) fn words(&self) -> String {
        format!("{}{}", self.what, self.after())
    }
}

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
    /// Opened from the settings form, and drawn over it, where its Model
    /// row is — see [`App::open_model_picker_from_settings`].
    pub over_settings: bool,
    /// Where it's drawn: under the Model row, from the form.
    pub place: picker::Place,
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

/// A price as a row says it: dollars per million tokens, sent and answered —
/// `$3 / $15`, which is how providers quote them. Always that unit, so the
/// Paid header says it once rather than every row ([`PAID`]).
fn per_million(price: thinkboard_core::ai::Price) -> String {
    format!("{} / {}", dollars(price.prompt * 1e6), dollars(price.completion * 1e6))
}

/// How dear a model is at a glance, one to four, by the mean of its prices
/// in and out per million tokens: under $1, $5 and $20, and above — about a
/// small open model, a fast hosted one, a frontier one, and the largest.
/// Drawn as `$$$$` with that many lit in gold, beside the price itself.
fn rating(price: thinkboard_core::ai::Price) -> usize {
    match (price.prompt + price.completion) / 2.0 * 1e6 {
        mean if mean < 1.0 => 1,
        mean if mean < 5.0 => 2,
        mean if mean < 20.0 => 3,
        _ => 4,
    }
}

/// The rating's gold: a card's yellow, taken toward the text until it reads.
fn gold(theme: &crate::theme::Theme) -> iced::Color {
    let yellow = thinkboard_core::model::parse_color("3").map(crate::document::IntoColor::into_color);
    readable(yellow.unwrap_or(theme.accent), theme)
}

/// The priced section's header, with the unit its rows' prices are in. The
/// sections stay through a search, so a priced row is never without it.
const PAID: &str = "Paid · in / out, per million tokens";

/// An amount in dollars as a person writes it: whole dollars bare (`$3`),
/// cents to two places (`$2.50`, `$0.15`), and less than a cent to two
/// significant digits (`$0.0031`) — an answer can cost a fraction of one.
pub(super) fn dollars(amount: f64) -> String {
    let whole = amount.fract() < 0.005 || amount.fract() > 0.995;
    if amount <= 0.0 {
        "$0".to_owned()
    } else if amount >= 100.0 || (amount >= 1.0 && whole) {
        format!("${amount:.0}")
    } else if amount >= 0.01 {
        format!("${amount:.2}")
    } else {
        let places = 1 - amount.log10().floor() as i32;
        format!("${amount:.*}", places.max(0) as usize)
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
            place: picker::Place::Top,
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
        // Open on the model in use, where a kept list has it — as the theme
        // picker opens on the theme — or it could be anywhere below the free
        // ones, out of sight.
        let before = self.config.ai_model.trim().to_owned();
        let current = self.highlight_model(&before);
        // Refetched even when cached: a list is cheap, and a stale one is how
        // you end up picking a model the provider has since retired.
        Task::batch([opened, current, self.fetch_models()])
    }

    /// Asks the endpoint for its models. What comes back is kept for it
    /// (`models_fetched`), whether or not a picker is waiting on it.
    fn fetch_models(&self) -> Task<Message> {
        let url = self.config.ai_url.trim().to_owned();
        let key = self.config.ai_key().to_owned();
        Task::perform(
            async move {
                let models = thinkboard_core::ai::models(&url, &key).await;
                (url, models)
            },
            |(url, models)| Message::ModelsFetched(url, models.map_err(|e| e.to_string())),
        )
    }

    /// The list for the settings form's Model row, which says what the model
    /// costs: fetched as the form opens, on OpenRouter — the one provider
    /// whose list has prices, so the only one worth asking unasked — and only
    /// while nothing's kept for it. Not again after a failure this launch
    /// (no key yet, offline): each opening would ask once more for nothing.
    /// The picker still asks every time it's opened, which is asking.
    pub(super) fn fetch_models_for_form(&self) -> Task<Message> {
        if !self.form_wants_models() {
            return Task::none();
        }
        self.fetch_models()
    }

    /// Whether the form opening now should ask for the list
    /// ([`App::fetch_models_for_form`]).
    pub(super) fn form_wants_models(&self) -> bool {
        let url = self.config.ai_url.trim();
        let kept = self.ai.models.as_ref().is_some_and(|(kept, _)| kept == url);
        let failed = self.ai.models_failed.as_deref() == Some(url);
        super::sign_in::signs_in(url) && !kept && !failed
    }

    /// The picker, from the settings form, where the model is chosen
    /// alongside the provider — from the form's "Choose…", or by itself once
    /// a provider is picked that can answer, or a sign-in has made a key.
    ///
    /// A new config names a local runtime's model, which no hosted provider
    /// serves, so picking one and asking used to fail on the first question.
    /// It opens in place, under the Model row, with the form still there
    /// beneath it — in the board, as in the settings window. High over the
    /// form, it was a panel over a panel, two things open at once and away
    /// from where it was asked for, so the form used to close for it; and
    /// then a model chosen left you on the board, not back at the form. With
    /// no form open — a sign-in that closed it, a provider from the palette
    /// — it's where it always is, over the board.
    pub(super) fn open_model_picker_from_settings(&mut self) -> Task<Message> {
        let opened = self.open_model_picker();
        if !(self.settings_overlay || self.settings_window.is_some()) {
            return opened;
        }
        if let Some(picker) = &mut self.model_picker {
            picker.over_settings = true;
            picker.place = picker::Place::Finding;
        }
        Task::batch([opened, picker::find_anchor(Panel::Model).map(|at| Message::Placed(Panel::Model, at))])
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
                self.ai.models_failed = None;
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
                self.ai.models_failed = Some(url);
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
        self.model_in_use()
            .map_or_else(|| thinkboard_core::ai::free_by_name(self.config.ai_model.trim()), |listed| listed.free)
    }

    /// The model in use, as the list kept for its endpoint has it.
    pub(super) fn model_in_use(&self) -> Option<&Model> {
        let (url, model) = (self.config.ai_url.trim(), self.config.ai_model.trim());
        let (_, models) = self.ai.models.as_ref().filter(|(kept, _)| kept == url)?;
        models.iter().find(|listed| listed.id == model)
    }

    /// Whether the model in use is known to read text only: the list kept
    /// for its endpoint says so. Not where it says nothing.
    pub(super) fn reads_text_only(&self) -> bool {
        self.model_in_use().is_some_and(|model| model.images == Some(false))
    }

    /// What the model in use costs, as the Model row says it, where the list
    /// kept for its endpoint gives a price.
    pub(super) fn price_in_use(&self) -> Option<String> {
        let price = self.model_in_use().filter(|model| !model.free)?.price?;
        Some(format!(
            "{} in, {} out, per million tokens",
            dollars(price.prompt * 1e6),
            dollars(price.completion * 1e6)
        ))
    }

    /// What Ask AI says beside it, in the menu and the palette: the model a
    /// question goes to, by the name the list kept for the endpoint gives
    /// it, less its vendor, or its id's last part before one's fetched — and
    /// whether that's a free one, whose provider may train on what it's
    /// sent. The provider beside it made a busy line, and no brand's mark is
    /// drawn, as nearly every brand asks permission for one. With no model
    /// named, the runtime answers with what it has loaded, and it's named.
    pub(super) fn ask_ai_label(&self) -> AskAiLabel {
        use thinkboard_core::ai;
        let model = self.config.ai_model.trim();
        let name = self.model_in_use().and_then(|listed| listed.name.as_deref());
        let what = if model.is_empty() { ai::destination(&self.config.ai_url) } else { ai::short_name(model, name) };
        AskAiLabel { what, free: self.asks_a_free_model() }
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
    /// which is where any can be free, so the rest are the priced ones.
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
                (PAID, paid, Cost::Paid),
            ]
        };
        // Sections where the endpoint prices its models — the whole list's to
        // say, so a search down to one model still says which it is, and a
        // list with nothing free still has the header its prices' unit is in.
        if !models.iter().any(|model| model.free || model.price.is_some()) {
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

    /// The picker's rows and section headers, as its view draws them — and
    /// as the tour records them.
    pub(super) fn model_rows<'a>(&'a self, picker: &'a ModelPicker) -> (Vec<Row<'a>>, Vec<Header<'a>>) {
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
                // Whether it's the one in use, and what it costs, free or
                // priced, where the endpoint says — the current one too, as
                // the one to compare the rest against. Not the context
                // window: a question from the board is a few hundred tokens,
                // far inside any of them, and it took the price's room.
                detail: [
                    (model.id == picker.before).then(|| "current".to_owned()),
                    if model.free { Some("free".to_owned()) } else { model.price.map(per_million) },
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" · ")
                .into(),
                detail_tint: (model.free && model.id != picker.before).then(|| tint(Cost::Free, &self.theme)),
                gauge: model.price.filter(|_| !model.free).map(|price| ("$", rating(price), gold(&self.theme))),
                // Where the endpoint says the model reads images, after its
                // name; a row without the mark keeps no room for it.
                flag: (model.images == Some(true)).then_some(Glyph::Picture),
                keys: Vec::new(),
                enabled: true,
                font: None,
                glyph: None,
                press: Message::PickModel(model.id.clone()),
            })
            .collect();
        // A free section says what its provider may do with what's sent,
        // where the eye is when choosing from it — a line over the list stood
        // there whatever was chosen.
        let headers = self
            .model_listing(&picker.state.query)
            .1
            .into_iter()
            .map(|(heads, label, cost)| Header {
                heads,
                label: label.into(),
                tint: Some(tint(cost, &self.theme)),
                warning: (cost == Cost::Free).then(|| (FREE_WARNING.into(), warning_tint(&self.theme))),
            })
            .collect();
        (rows, headers)
    }

    pub(super) fn model_picker_view<'a>(&'a self, picker: &'a ModelPicker) -> Element<'a, Message> {
        let (rows, headers) = self.model_rows(picker);
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
                headers,
                // Asked, and nothing kept from before: rows still to come, at
                // the list's full height, rather than a one-row panel that grows.
                loading: self.model_picker_waiting(),
                anchor: match picker.place {
                    picker::Place::Under(anchor) => Some(anchor),
                    _ => None,
                },
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
    fn a_price_reads_as_providers_quote_it() {
        use thinkboard_core::ai::Price;
        // Per token as /models gives it, with its floating-point dust.
        assert_eq!(per_million(Price { prompt: 0.000003, completion: 0.000015 }), "$3 / $15");
        assert_eq!(per_million(Price { prompt: 0.0000025, completion: 0.00001 }), "$2.50 / $10");
        assert_eq!(per_million(Price { prompt: 0.00000015, completion: 0.0000006 }), "$0.15 / $0.60");
        // At a glance: Llama 3.3 70B, Claude Haiku, Sonnet, Opus.
        let per_m = |prompt: f64, completion: f64| Price { prompt: prompt / 1e6, completion: completion / 1e6 };
        assert_eq!(rating(per_m(0.13, 0.39)), 1);
        assert_eq!(rating(per_m(1.0, 5.0)), 2);
        assert_eq!(rating(per_m(3.0, 15.0)), 3);
        assert_eq!(rating(per_m(15.0, 75.0)), 4);
        // What an answer costs: a fraction of a cent, to two figures.
        assert_eq!(dollars(0.00031), "$0.00031");
        assert_eq!(dollars(0.0075), "$0.0075");
        assert_eq!(dollars(0.0), "$0");
        assert_eq!(dollars(150.0), "$150");
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
