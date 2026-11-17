//! Streaming chat completions against an OpenAI-compatible endpoint (LM Studio,
//! OpenAI, …).
//!
//! Ad-hoc trigger for now: coloring a node yellow asks the endpoint with the
//! chain of upstream cards as the prompt, and streams the answer into a new
//! node branching off it. See `App::trigger_ai`.

use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};

use futures::channel::mpsc::{self, Sender};
use futures::{SinkExt, Stream, StreamExt};

use crate::chain;
use crate::model::{CanvasEdge, CanvasNode, NodeKind};

/// A streamed event from the endpoint.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// A chunk of the current card's text to append.
    Delta(String),
    /// The model asked to start a new sibling card.
    Split,
    /// What the answer used, where the endpoint says: OpenRouter sends it in
    /// the stream's last chunk, unasked, with what it cost.
    Usage(Usage),
    /// The answer finished.
    Done,
    /// The request failed (human-readable reason).
    Error(String),
}

/// What an answer used: tokens each way, and what it cost where the endpoint
/// says (OpenRouter's `usage.cost`, in its credits, which are dollars).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cost: Option<f64>,
}

/// A line of exactly this (per the system prompt) starts a new card.
const SEPARATOR: &str = "\n---\n";

/// A known AI provider: a display name and its OpenAI-style base URL. Selecting
/// one in settings fills the endpoint in (the model is left untouched).
pub struct Preset {
    pub name: &'static str,
    pub url: &'static str,
    pub kind: ProviderKind,
}

/// What sort of provider a preset is, which is how they're listed to choose
/// from: by where a question goes, and what's there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    /// A runtime on this machine: nothing leaves it, and no key.
    Local,
    /// Many makers' models behind one key: routed to whoever serves them
    /// (OpenRouter), or open ones on the host's own hardware (Groq).
    Host,
    /// A model maker's own API, for its own models.
    Maker,
}

/// The providers offered as one-click presets. `Custom` (any other URL) is not
/// listed here — the settings picker adds it.
pub const PRESETS: &[Preset] = &[
    // By kind, as they're listed (local runtimes, no key needed, first) — and
    // within one, the ones someone is likeliest to have a key for first.
    Preset { name: "LM Studio", url: "http://localhost:1234/v1", kind: ProviderKind::Local },
    Preset { name: "Ollama", url: "http://localhost:11434/v1", kind: ProviderKind::Local },
    Preset { name: "llama.cpp", url: "http://localhost:9931/v1", kind: ProviderKind::Local },
    Preset { name: "Jan", url: "http://127.0.0.1:1337/v1", kind: ProviderKind::Local },
    Preset { name: "OpenRouter", url: crate::sign_in::ENDPOINT, kind: ProviderKind::Host },
    Preset { name: "Groq", url: "https://api.groq.com/openai/v1", kind: ProviderKind::Host },
    Preset { name: "Hugging Face", url: "https://router.huggingface.co/v1", kind: ProviderKind::Host },
    Preset { name: "Together AI", url: "https://api.together.ai/v1", kind: ProviderKind::Host },
    Preset { name: "Fireworks AI", url: "https://api.fireworks.ai/inference/v1", kind: ProviderKind::Host },
    Preset { name: "Cerebras", url: "https://api.cerebras.ai/v1", kind: ProviderKind::Host },
    Preset { name: "OpenAI", url: "https://api.openai.com/v1", kind: ProviderKind::Maker },
    Preset { name: "Anthropic", url: "https://api.anthropic.com/v1", kind: ProviderKind::Maker },
    Preset { name: "Google Gemini", url: "https://generativelanguage.googleapis.com/v1beta/openai", kind: ProviderKind::Maker },
    Preset { name: "xAI", url: "https://api.x.ai/v1", kind: ProviderKind::Maker },
    Preset { name: "DeepSeek", url: "https://api.deepseek.com", kind: ProviderKind::Maker },
    Preset { name: "Mistral", url: "https://api.mistral.ai/v1", kind: ProviderKind::Maker },
];

/// A POST to the endpoint with the auth header set when a key is given, plus an
/// app-identifying header some providers (OpenRouter) use for attribution.
/// Endpoints that need neither ignore them.
fn authed_post(url: &str, api_key: &str) -> reqwest::RequestBuilder {
    let mut req = reqwest::Client::new().post(url).header("X-Title", "ThinkBoard");
    let key = api_key.trim();
    if !key.is_empty() {
        req = req.bearer_auth(key);
    }
    req
}

/// A GET against the same endpoint, with the same headers [`authed_post`] sets.
fn authed_get(url: &str, api_key: &str) -> reqwest::RequestBuilder {
    let mut req = reqwest::Client::new().get(url).header("X-Title", "ThinkBoard");
    let key = api_key.trim();
    if !key.is_empty() {
        req = req.bearer_auth(key);
    }
    req
}

/// One model an endpoint offers.
#[derive(Debug, Clone, PartialEq)]
pub struct Model {
    /// What gets sent as `model` — the authoritative name.
    pub id: String,
    /// A human-readable title where the provider gives one ("Anthropic: Claude
    /// Sonnet 4"). OpenRouter does; a plain OpenAI-compatible endpoint doesn't.
    pub name: Option<String>,
    /// Priced at nothing to ask and nothing to answer, and answering in text
    /// alone — where the endpoint says what it charges (OpenRouter does). A
    /// free model is rate-limited, and such offers come and go.
    pub free: bool,
    /// What it charges, where the endpoint says (OpenRouter does; a plain
    /// OpenAI-compatible endpoint doesn't).
    pub price: Option<Price>,
}

/// A model's price, in dollars per token, as `/models` gives it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Price {
    /// Per token sent: the question, and the cards it's asked of.
    pub prompt: f64,
    /// Per token of the answer.
    pub completion: f64,
}

/// The models an endpoint offers, by its OpenAI-compatible `GET /models`.
///
/// **Not per-provider.** OpenRouter and Fireworks were the ones worth browsing —
/// hundreds of models each, with names nobody recalls exactly — but the route is
/// part of the OpenAI API that every preset here implements, local runtimes
/// included, so there is nothing provider-specific to write. Fields beyond `id`
/// are extensions, read when present and skipped when not.
pub async fn models(base_url: &str, api_key: &str) -> Result<Vec<Model>, String> {
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let mut models = Vec::new();
    let mut page = url.clone();
    for _ in 0..MODEL_PAGES {
        let response = authed_get(&page, api_key).send().await.map_err(|e| describe_unreachable(&e, base_url))?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(describe_failure(status, &body, base_url, api_key, Asked::Models));
        }
        let body: serde_json::Value = response.json().await.map_err(|e| e.to_string())?;
        models.extend(parse_models(&body));
        match next_page(&body) {
            Some(after) => page = format!("{url}?limit=1000&after_id={after}"),
            None => break,
        }
    }
    models.sort_by_key(|model| model.id.to_lowercase());
    models.dedup_by(|a, b| a.id == b.id);
    Ok(models)
}

/// Whether an OpenAI-compatible server answers at `base_url`: its model
/// list comes, or is refused for want of a key. Something else on the port
/// answers too, but not like that — and nothing there refuses the
/// connection at once, so on this machine the answer is quick.
pub async fn answers(base_url: &str) -> bool {
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let Ok(client) = reqwest::Client::builder().timeout(std::time::Duration::from_secs(1)).build() else {
        return false;
    };
    client.get(&url).send().await.is_ok_and(|response| {
        response.status().is_success() || matches!(response.status().as_u16(), 401 | 403)
    })
}

/// How many pages of a model list are followed: Anthropic's, the one that
/// comes in pages, is a few dozen models, a thousand to a page after the
/// first. A bound, so a list that never ends can't hold the picker forever.
const MODEL_PAGES: usize = 10;

/// Where the next page of a model list begins, where it says there's one:
/// Anthropic's twenty to a page, unless asked for more, with `has_more` and
/// the last id. Only an id that can go in a query as it is.
fn next_page(body: &serde_json::Value) -> Option<&str> {
    let more = body.get("has_more").and_then(serde_json::Value::as_bool)?;
    let last = body.get("last_id").and_then(serde_json::Value::as_str)?;
    let plain = !last.is_empty() && last.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b));
    (more && plain).then_some(last)
}

/// Reads the model list out of a `/models` response.
///
/// Split from the request so the shapes can be tested without one. Anything
/// without an `id` is skipped rather than shown as a blank row — an entry we
/// can't name is one we can't select either.
fn parse_models(body: &serde_json::Value) -> Vec<Model> {
    // `data` is where the OpenAI shape puts them; a couple of endpoints return
    // the bare array instead.
    let items = body.get("data").or(Some(body)).and_then(|d| d.as_array());
    let mut models: Vec<Model> = items
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let id = item.get("id").and_then(|v| v.as_str())?.trim();
            // Gemini's list names its models as resources ("models/gemini-…"),
            // where a question names them bare, as its own examples do.
            let id = id.strip_prefix("models/").unwrap_or(id);
            (!id.is_empty()).then(|| Model {
                id: id.to_owned(),
                // OpenRouter's `name`; Anthropic's and Together's `display_name`.
                name: ["name", "display_name"]
                    .iter()
                    .find_map(|field| item.get(field).and_then(|v| v.as_str()).filter(|name| !name.trim().is_empty()))
                    .map(str::trim)
                    .filter(|name| !name.is_empty() && *name != id)
                    .map(str::to_owned),
                free: free(item),
                price: price(item, "prompt")
                    .zip(price(item, "completion"))
                    .map(|(prompt, completion)| Price { prompt, completion }),
            })
        })
        .collect();
    // Alphabetical, which for these ids groups a vendor's models together
    // ("anthropic/…", "openai/…") — the order you'd want to scroll.
    models.sort_by_key(|model| model.id.to_lowercase());
    models.dedup_by(|a, b| a.id == b.id);
    models
}

/// Whether a `/models` entry says it costs nothing, prompt and completion
/// both, and answers in text alone: a model that makes music can be listed at
/// nothing too, and isn't one to ask a question of. Prices are decimal
/// strings, per token; a model without them isn't free, only unpriced.
fn free(item: &serde_json::Value) -> bool {
    let zero = |side: &str| price(item, side).is_some_and(|price| price == 0.0);
    let text_out = item
        .pointer("/architecture/output_modalities")
        .and_then(serde_json::Value::as_array)
        .is_none_or(|out| out.iter().all(|modality| modality == "text"));
    zero("prompt") && zero("completion") && text_out
}

/// A `/models` entry's price per token for `side` (`prompt` or
/// `completion`): a decimal string, or a number from an endpoint that sends
/// one. Negative is OpenRouter's "varies", for a router — not a price.
fn price(item: &serde_json::Value, side: &str) -> Option<f64> {
    item.pointer(&format!("/pricing/{side}"))
        .and_then(|price| price.as_str().and_then(|s| s.parse::<f64>().ok()).or_else(|| price.as_f64()))
        .filter(|price| *price >= 0.0)
}

/// What a request was for, which decides what a refusal means.
#[derive(Clone, Copy)]
enum Asked<'a> {
    /// `GET /models`, where a 404 is a local runtime without the route — the
    /// ordinary answer, not a fault.
    Models,
    /// A chat completion naming `model`, where a refusal that mentions a model
    /// is almost always that one not being served.
    Chat { model: &'a str },
}

/// A refused request, in a sentence a user can act on rather than a status
/// code: what went wrong, then where to fix it.
///
/// One wording whichever request it was. The model list and the question used
/// to describe the same 401 two ways — one "the API key was rejected", the
/// other "endpoint returned 401 Unauthorized" — and only one said what to do.
fn describe_failure(status: reqwest::StatusCode, body: &str, base_url: &str, api_key: &str, asked: Asked) -> String {
    let detail = error_detail(body);
    let provider = provider_name(base_url).unwrap_or("The provider");
    let unserved = match asked {
        Asked::Chat { model } if matches!(status.as_u16(), 400 | 404 | 422) && names_a_model(&detail) => Some(model),
        _ => None,
    };
    if let Some(model) = unserved {
        return format!("{provider} doesn't serve “{model}” — choose another model in Settings → AI.");
    }
    // OpenRouter's privacy settings can rule out every provider of a model —
    // a free one's may all train on what they're sent — and it says so with a
    // 404, which read as nothing answering at the URL. Its words name the
    // setting and link to it.
    if status.is_client_error() && detail.to_lowercase().contains("data policy") {
        let of = match asked {
            Asked::Chat { model } => format!(" of “{model}”"),
            Asked::Models => String::new(),
        };
        return format!(
            "Your privacy settings on {provider} rule out every provider{of} — change them there, or choose \
             another model in Settings → AI. ({detail})"
        );
    }
    // Gemini refuses a key it doesn't know with a 400 that says so, not a 401.
    let about_the_key = status.as_u16() == 400 && detail.to_lowercase().contains("api key");
    match status.as_u16() {
        // Sent with no key at all, a refusal is a request for one, not a
        // verdict on it.
        401 | 403 if api_key.trim().is_empty() => format!("{provider} wants an API key — paste one in Settings → AI."),
        400 if about_the_key && api_key.trim().is_empty() => {
            format!("{provider} wants an API key — paste one in Settings → AI.")
        }
        401 | 403 => "The API key was rejected — check it in Settings → AI.".to_owned(),
        400 if about_the_key => "The API key was rejected — check it in Settings → AI.".to_owned(),
        404 if matches!(asked, Asked::Models) => {
            "This endpoint doesn't list its models — type the model's name in Settings → AI.".to_owned()
        }
        404 => format!("Nothing answers at {} — check the API URL in Settings → AI.", base_url.trim()),
        // Credits, or a balance below nothing — which stops even the free
        // models.
        402 => format!("{provider} wants credits for this — add some on its site, then ask again. ({detail})"),
        // A free model's limits are per minute and per day, and the day's
        // is the one a wait of a moment won't lift.
        429 if matches!(asked, Asked::Chat { model } if free_by_name(model)) => format!(
            "That's {provider}'s limit for its free models for now — try later, or choose another model in \
             Settings → AI. Credits on the account raise it. ({detail})"
        ),
        429 => format!("{provider} is limiting requests — wait a moment, then ask again. ({detail})"),
        500..=599 => format!("{provider} couldn't answer ({status}): {detail}"),
        _ => format!("{provider} refused the request ({status}): {detail}"),
    }
}

/// OpenRouter's router to its free models: whichever is up at the time, so
/// one that's been withdrawn or is down is passed over.
pub const FREE_ROUTER: &str = "openrouter/free";

/// Whether a model's name says it's a free one: OpenRouter's `:free`
/// variants, and its router to them. A refusal carries the name, not the
/// price — nor does a config, until the endpoint's list has been fetched.
pub fn free_by_name(model: &str) -> bool {
    model.ends_with(":free") || model == FREE_ROUTER
}

/// A model as it's named in a line of its own: the endpoint's title for it
/// less the vendor before it ("Anthropic: Claude Sonnet 4.5" is "Claude
/// Sonnet 4.5"), or else its id's last part. Less whether it's free as well,
/// which whoever says it says their own way, and Ollama's `:latest`, which
/// is what no tag means anyway.
pub fn short_name(id: &str, name: Option<&str>) -> String {
    // The router's id says nothing but "free"; this is what it lists it as.
    let name = name.or((id.trim() == FREE_ROUTER).then_some("Free Models Router"));
    if let Some(name) = name.map(str::trim).filter(|name| !name.is_empty()) {
        let name = name.split_once(": ").map_or(name, |(_, rest)| rest);
        return name.trim_end_matches(" (free)").to_owned();
    }
    let tail = id.trim().rsplit('/').next().unwrap_or_default();
    tail.trim_end_matches(":free").trim_end_matches(":latest").to_owned()
}

/// Whether a refusal is about the model named — wording varies by runtime:
/// OpenAI's "does not exist", Ollama's "not found, try pulling it first",
/// OpenRouter's "is not a valid model ID".
fn names_a_model(detail: &str) -> bool {
    let detail = detail.to_lowercase();
    detail.contains("model")
        && ["not found", "does not exist", "doesn't exist", "not a valid", "invalid model", "unknown model", "no such model"]
            .iter()
            .any(|phrase| detail.contains(phrase))
}

/// A request that got no answer at all, as what to check.
fn describe_unreachable(error: &reqwest::Error, base_url: &str) -> String {
    if error.is_builder() {
        return format!("“{}” isn't a web address — it should start with http:// or https://.", base_url.trim());
    }
    if error.is_connect() {
        return unreachable(base_url);
    }
    error.to_string()
}

/// Nothing listening at `base_url`. Local, that's a runtime not started yet,
/// which is the thing to ask about — reqwest's own "error sending request for
/// url" says only that something, somewhere, failed.
fn unreachable(base_url: &str) -> String {
    let Ok(url) = reqwest::Url::parse(base_url.trim()) else {
        return format!("Couldn't reach {}.", base_url.trim());
    };
    let host = url.host_str().unwrap_or_default();
    let at = match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    };
    if matches!(host, "localhost" | "127.0.0.1" | "[::1]" | "0.0.0.0") {
        match provider_name(base_url) {
            // Its server listened on 8080 until October 2026, and still does
            // wherever it's told to: older builds, the Docker examples.
            Some(name @ "llama.cpp") => {
                format!("Couldn't reach {name} at {at} — is its server running? If it's on 8080, choose Custom in Settings → AI.")
            }
            Some(name) => format!("Couldn't reach {name} at {at} — is it running?"),
            None => format!("Nothing is answering at {at} — is the server running?"),
        }
    } else {
        format!("Couldn't reach {at} — check the connection, and the API URL in Settings → AI.")
    }
}

/// The preset `base_url` belongs to, by name: "LM Studio" means more in a
/// sentence than its port does.
fn provider_name(base_url: &str) -> Option<&'static str> {
    let url = base_url.trim().trim_end_matches('/');
    PRESETS.iter().find(|preset| preset.url.trim_end_matches('/') == url).map(|preset| preset.name)
}

/// Where `base_url` sends a question, as a person would name it: its preset,
/// or else the host it's at, with the port where one's given.
pub fn destination(base_url: &str) -> String {
    if let Some(name) = provider_name(base_url) {
        return name.to_owned();
    }
    let rest = base_url.trim().split_once("://").map_or(base_url.trim(), |(_, rest)| rest);
    rest.split('/').next().unwrap_or_default().to_owned()
}

fn system_prompt(max_nodes: usize) -> String {
    format!(
        "You are a helpful assistant inside a mind-mapping canvas. The user's \
         query is formed by a chain of connected notes. Answer concisely. If — \
         and only if — the answer is naturally a list of distinct, standalone \
         items (options, steps, alternatives), split it into separate notes by \
         putting a line containing only `---` between them; make each note \
         self-contained. Prefer a single note for explanations or prose, and \
         never produce more than {max_nodes} notes."
    )
}

/// Every card feeding into `target` (all ancestors, following incoming links,
/// not just one chain), topologically ordered so each card precedes the ones it
/// feeds, with `target` last. `A -> B -> C` (target C) is `[A, B, C]`;
/// `A, B, C -> D` is all four.
pub fn upstream_nodes<'a>(
    nodes: &'a [CanvasNode],
    edges: &'a [CanvasEdge],
    target_id: &str,
) -> Vec<&'a CanvasNode> {
    let Some(target) = nodes.iter().find(|n| n.id == target_id) else {
        return Vec::new();
    };
    let mut order: Vec<&str> = Vec::new();
    let mut visited: HashSet<&str> = HashSet::new();
    collect_ancestors(edges, target.id.as_str(), &mut visited, &mut order);
    order
        .iter()
        .filter_map(|id| nodes.iter().find(|n| &n.id == id))
        .collect()
}

/// Builds the text prompt for `target`: the text of its ancestor cards joined
/// by blank lines (see [`upstream_nodes`] for the order). Image (file) cards
/// contribute no text; see [`upstream_images`].
///
/// A *group* among the ancestors contributes its whole contents, flattened by
/// [`group_prompt`]. A frame holds no text of its own, so without this a link
/// from a group into a question would look meaningful on the board and feed the
/// model nothing — which is the point of being able to draw that link at all:
/// it's how a question gets asked *about* a group.
pub fn upstream_prompt(nodes: &[CanvasNode], edges: &[CanvasEdge], target_id: &str) -> String {
    upstream_nodes(nodes, edges, target_id)
        .iter()
        .filter_map(|node| {
            if node.is_group() {
                let index = nodes.iter().position(|n| n.id == node.id)?;
                let outline = group_prompt(nodes, edges, index);
                (!outline.trim().is_empty()).then_some(outline)
            } else {
                node.text
                    .as_deref()
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                    .map(str::to_owned)
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Flattens everything a group frame encloses into one outline, for asking
/// about a whole group rather than a chain of cards.
///
/// A group has no incoming links to walk — it is a region, not a step in a
/// chain — so the prompt is built from its contents instead, preserving the two
/// hierarchies the user actually sees: links between the cards inside it (a
/// card's targets are nested under it) and frames within frames (a nested
/// group's contents are nested under its caption). Each card is emitted as a
/// bullet, its own line breaks indented to stay under it.
pub fn group_prompt(nodes: &[CanvasNode], edges: &[CanvasEdge], frame: usize) -> String {
    group_outline(nodes, edges, frame, Outline::Prompt)
}

/// Who an outline is for.
///
/// Ask AI's prompt is kept as it is, deliberately: its answer is split into
/// cards at `---`, and a prompt of another shape can change the shape of what
/// comes back — a model shown a list tends to answer with one. A copy is read
/// by a person or an agent, so it says what a link or file card holds and
/// keeps the labels on links ([`chain::text`]); that hasn't been tried as a
/// prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outline {
    Prompt,
    Copy,
}

/// [`group_prompt`], or the same outline as a copy writes it.
pub(crate) fn group_outline(nodes: &[CanvasNode], edges: &[CanvasEdge], frame: usize, kind: Outline) -> String {
    let members = crate::model::enclosed_by(nodes, frame);
    if members.is_empty() {
        return String::new();
    }
    let member_set: HashSet<usize> = members.iter().copied().collect();

    let mut out = String::new();
    if let Some(label) = nodes[frame].label().map(str::trim).filter(|l| !l.is_empty()) {
        out.push_str(label);
        out.push('\n');
    }
    let mut emitted = HashSet::new();
    write_container(nodes, edges, frame, &members, &member_set, 0, &mut emitted, &mut out, kind);
    out.trim_end().to_owned()
}

/// Emits the members whose innermost enclosing frame (within the set) is
/// `container`, following links so a card's targets nest under it.
#[allow(clippy::too_many_arguments)] // one recursive walk; splitting it would only move the args
fn write_container(
    nodes: &[CanvasNode],
    edges: &[CanvasEdge],
    container: usize,
    members: &[usize],
    member_set: &HashSet<usize>,
    depth: usize,
    emitted: &mut HashSet<usize>,
    out: &mut String,
    kind: Outline,
) {
    let direct: Vec<usize> = members
        .iter()
        .copied()
        // A node inside a nested frame belongs to that frame, not to this one.
        .filter(|&m| innermost_frame(nodes, m, member_set).unwrap_or(container) == container)
        .collect();

    // Roots first: a card with no link from a sibling in the same container
    // starts a branch. Everything else is reached by following links, and
    // anything left over (a cycle) is emitted afterwards so nothing is lost.
    let has_sibling_parent = |m: usize| {
        edges.iter().any(|e| {
            e.to_node == nodes[m].id
                && direct
                    .iter()
                    .any(|&o| o != m && nodes[o].id == e.from_node)
        })
    };
    for &m in direct.iter().filter(|&&m| !has_sibling_parent(m)) {
        write_member(nodes, edges, m, None, members, member_set, depth, emitted, out, kind);
    }
    for &m in &direct {
        write_member(nodes, edges, m, None, members, member_set, depth, emitted, out, kind);
    }
}

/// Emits one member and, beneath it, whatever it links to or encloses. In a
/// copy, `via` — the label of the link it was reached by — leads it, as it
/// leads a card in a chain.
#[allow(clippy::too_many_arguments)] // as above
fn write_member(
    nodes: &[CanvasNode],
    edges: &[CanvasEdge],
    member: usize,
    via: Option<&str>,
    members: &[usize],
    member_set: &HashSet<usize>,
    depth: usize,
    emitted: &mut HashSet<usize>,
    out: &mut String,
    kind: Outline,
) {
    if !emitted.insert(member) {
        return;
    }
    let node = &nodes[member];
    let indent = "  ".repeat(depth);
    let lead = |body: &str| match via.map(str::trim).filter(|_| kind == Outline::Copy) {
        Some(label) => format!("→ {label}: {body}"),
        None => body.to_owned(),
    };

    if node.is_group() {
        let label = node.label().map(str::trim).filter(|l| !l.is_empty());
        let unnamed = match kind {
            Outline::Prompt => "(unnamed group)",
            Outline::Copy => crate::model::UNTITLED_GROUP,
        };
        chain::list_item(out, &indent, &lead(label.unwrap_or(unnamed)));
        write_container(nodes, edges, member, members, member_set, depth + 1, emitted, out, kind);
        return;
    }

    let text = match kind {
        Outline::Prompt => node.text.as_deref().map(str::trim).filter(|t| !t.is_empty()).map(str::to_owned),
        Outline::Copy => chain::card_text(node),
    };
    // Keep a multi-line card under its own bullet rather than letting later
    // lines read as separate items.
    if let Some(text) = text {
        chain::list_item(out, &indent, &lead(&text));
    }

    // Notes this one links to, nested beneath it.
    for edge in edges.iter().filter(|e| e.from_node == node.id) {
        let Some(&target) = members
            .iter()
            .find(|&&m| nodes[m].id == edge.to_node && member_set.contains(&m))
        else {
            continue;
        };
        write_member(nodes, edges, target, edge.label(), members, member_set, depth + 1, emitted, out, kind);
    }
}

/// The smallest frame in `set` that encloses `member`, or `None` when it sits
/// directly in the outer frame. Smallest, so a node inside nested frames is
/// attributed to the innermost one rather than to every frame around it.
fn innermost_frame(nodes: &[CanvasNode], member: usize, set: &HashSet<usize>) -> Option<usize> {
    set.iter()
        .copied()
        .filter(|&f| f != member && nodes[f].is_group())
        .filter(|&f| crate::model::enclosed_by(nodes, f).contains(&member))
        .min_by(|&a, &b| {
            let area = |i: usize| nodes[i].width * nodes[i].height;
            area(a).total_cmp(&area(b))
        })
}

/// The `file` paths of the image cards a group frame encloses, to attach as
/// vision inputs alongside [`group_prompt`].
pub fn group_images(nodes: &[CanvasNode], frame: usize) -> Vec<String> {
    crate::model::enclosed_by(nodes, frame)
        .into_iter()
        .filter(|&i| nodes[i].node_kind() == NodeKind::Image)
        .filter_map(|i| nodes[i].file.clone())
        .collect()
}

/// The `file` paths of image (file) cards among `target`'s ancestors, in the
/// same order, to attach as vision inputs alongside the text prompt.
pub fn upstream_images(nodes: &[CanvasNode], edges: &[CanvasEdge], target_id: &str) -> Vec<String> {
    upstream_nodes(nodes, edges, target_id)
        .iter()
        .flat_map(|node| {
            // A group ancestor brings the images it encloses, matching how
            // `upstream_prompt` brings its text.
            if node.is_group() {
                let index = nodes.iter().position(|n| n.id == node.id);
                return index.map(|i| group_images(nodes, i)).unwrap_or_default();
            }
            // Images only: a `file` node can also embed a Markdown note or a
            // PDF, neither of which a vision encoder can take.
            match node.node_kind() {
                NodeKind::Image => node.file.clone().into_iter().collect(),
                _ => Vec::new(),
            }
        })
        .collect()
}

/// Post-order DFS over incoming links from `node`, pushing each card after all
/// of its ancestors so `order` is a valid topological sort (ancestors first,
/// `node` last). Marking visited on entry keeps it terminating on cycles.
fn collect_ancestors<'a>(
    edges: &'a [CanvasEdge],
    node: &'a str,
    visited: &mut HashSet<&'a str>,
    order: &mut Vec<&'a str>,
) {
    if !visited.insert(node) {
        return;
    }
    for edge in edges.iter().filter(|e| e.to_node == node) {
        collect_ancestors(edges, edge.from_node.as_str(), visited, order);
    }
    order.push(node);
}

/// Copilot-style completion: asks (non-streaming) for up to `count` brief
/// sibling cards that could branch alongside the card chain in `context`.
/// `existing` lists the sibling cards already present so suggestions stay on
/// topic without repeating them. Returns the cleaned suggestions.
pub async fn suggest(
    base_url: String,
    model: String,
    api_key: String,
    context: String,
    existing: Vec<String>,
    count: usize,
    thinking: bool,
) -> Result<Vec<String>, String> {
    let system = format!(
        "You are a mind-mapping copilot. Given a note and its context, suggest {count} \
         brief, distinct sibling notes that could branch from the same parent alongside \
         it — short phrases, not sentences, no explanations. They must differ from each \
         other and from any notes already present. Output only the notes, one per line, \
         with no numbering, bullets, or extra text."
    );
    let mut user = context;
    if !existing.is_empty() {
        user.push_str("\n\nNotes already present (stay on theme but do not repeat these):");
        for note in &existing {
            user.push_str("\n- ");
            user.push_str(note);
        }
    }
    let mut body = serde_json::json!({
        "model": model,
        "stream": false,
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": user },
        ],
    });
    apply_thinking(&mut body, &base_url, thinking);

    let response = post_with_reasoning_retry(&base_url, &api_key, &mut body).await?;
    let value: serde_json::Value = response.json().await.map_err(|e| e.to_string())?;
    let content = value
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or_default();
    Ok(content
        .lines()
        .filter_map(clean_suggestion)
        .take(count)
        .collect())
}

/// Trims a suggestion line and strips a leading list marker (`- `, `* `, `1. `),
/// or `None` if it's blank.
fn clean_suggestion(line: &str) -> Option<String> {
    let line = line.trim();
    let line = line
        .strip_prefix("- ")
        .or_else(|| line.strip_prefix("* "))
        .or_else(|| line.trim_start_matches(|c: char| c.is_ascii_digit()).strip_prefix(". "))
        .unwrap_or(line)
        .trim();
    (!line.is_empty()).then(|| line.to_owned())
}

/// Whether `url` points at OpenRouter, which takes a typed `reasoning` object
/// to control reasoning (a string value 400s its API).
fn is_openrouter(url: &str) -> bool {
    url.contains("openrouter.ai")
}

/// Models that rejected a disabled-reasoning request as mandatory (e.g. Gemini
/// Flash on OpenRouter). Learned at runtime by [`post_with_reasoning_retry`] so
/// we stop trying to disable reasoning for them this session. In-memory only —
/// a fresh launch re-learns on the first request (one retry), which is cheap for
/// a resident app.
static REASONING_REQUIRED: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

fn reasoning_required(model: &str) -> bool {
    REASONING_REQUIRED.lock().unwrap().contains(model)
}

fn mark_reasoning_required(model: &str) {
    REASONING_REQUIRED.lock().unwrap().insert(model.to_owned());
}

/// The OpenRouter `reasoning` object for the lowest thinking level: a minimal
/// budget, excluded from the response so it doesn't leak into generated cards.
fn lowest_reasoning() -> serde_json::Value {
    serde_json::json!({ "effort": "low", "exclude": true })
}

/// When `thinking` is false, disables reasoning where we safely can. There is no
/// standard switch, so this is best-effort and must never break a request:
///
/// - OpenRouter: a typed `reasoning` object disables it — worth doing since
///   reasoning tokens cost noticeably more on a paid, hosted endpoint. For
///   models we've learned mandate reasoning, request the lowest effort instead
///   (a disable would 400).
/// - Everything else (local runtimes, strict hosted APIs like OpenAI/Groq):
///   left at the model's default. Local runtimes are cheap, and strict APIs
///   400 on unknown params with no uniform disable switch.
fn apply_thinking(body: &mut serde_json::Value, url: &str, thinking: bool) {
    if thinking || !is_openrouter(url) {
        return;
    }
    let model = body.get("model").and_then(|m| m.as_str()).unwrap_or_default();
    body["reasoning"] = if reasoning_required(model) {
        lowest_reasoning()
    } else {
        serde_json::json!({ "enabled": false })
    };
}

/// Whether a failed response says the endpoint requires reasoning we tried to
/// disable (Gemini Flash on OpenRouter: "Reasoning is mandatory for this
/// endpoint and cannot be disabled.").
fn is_reasoning_mandatory(body: &str) -> bool {
    let msg = error_detail(body).to_lowercase();
    msg.contains("reasoning")
        && (msg.contains("mandatory")
            || msg.contains("cannot be disabled")
            || msg.contains("can't be disabled")
            || msg.contains("required")
            || msg.contains("must be enabled"))
}

/// POSTs `body` and returns the successful response. If a disabled-reasoning
/// request is rejected because the model mandates reasoning, learns that for the
/// session, drops to the lowest thinking effort, and retries once.
async fn post_with_reasoning_retry(
    base_url: &str,
    api_key: &str,
    body: &mut serde_json::Value,
) -> Result<reqwest::Response, String> {
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let response = authed_post(&url, api_key)
        .json(&*body)
        .send()
        .await
        .map_err(|e| describe_unreachable(&e, base_url))?;
    if response.status().is_success() {
        return Ok(response);
    }
    let status = response.status();
    let text = response.text().await.unwrap_or_default();

    let attempted_disable = body.get("reasoning") == Some(&serde_json::json!({ "enabled": false }));
    if attempted_disable && is_reasoning_mandatory(&text) {
        if let Some(model) = body.get("model").and_then(|m| m.as_str()) {
            mark_reasoning_required(model);
        }
        body["reasoning"] = lowest_reasoning();
        let retry = authed_post(&url, api_key)
            .json(&*body)
            .send()
            .await
            .map_err(|e| describe_unreachable(&e, base_url))?;
        if retry.status().is_success() {
            return Ok(retry);
        }
        let status = retry.status();
        let text = retry.text().await.unwrap_or_default();
        return Err(describe_failure(status, &text, base_url, api_key, asked(body)));
    }
    Err(describe_failure(status, &text, base_url, api_key, asked(body)))
}

/// A chat request's [`Asked`], naming the model it asked for.
fn asked(body: &serde_json::Value) -> Asked<'_> {
    Asked::Chat { model: body.get("model").and_then(|m| m.as_str()).unwrap_or_default() }
}

/// A human-readable reason from a failed response body: the OpenAI-style
/// `error.message` (what OpenRouter returns), else the raw body trimmed to a
/// sane length, else a placeholder.
fn error_detail(body: &str) -> String {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(body) {
        let err = value.get("error");
        if let Some(msg) = err
            .and_then(|e| e.get("message").and_then(|m| m.as_str()).or_else(|| e.as_str()))
            .or_else(|| value.get("message").and_then(|m| m.as_str()))
        {
            return msg.to_owned();
        }
    }
    let trimmed = body.trim();
    if trimmed.is_empty() {
        "no response body".to_owned()
    } else {
        trimmed.chars().take(300).collect()
    }
}

/// The user message `content`: a plain string when there are no images (the
/// simplest form every endpoint accepts, and what non-vision use keeps), or the
/// OpenAI multi-part array — a text part then one `image_url` part per image —
/// when there are.
fn user_content(prompt: &str, images: &[String]) -> serde_json::Value {
    if images.is_empty() {
        return serde_json::Value::String(prompt.to_owned());
    }
    let mut parts = Vec::with_capacity(images.len() + 1);
    if !prompt.trim().is_empty() {
        parts.push(serde_json::json!({ "type": "text", "text": prompt }));
    }
    for url in images {
        parts.push(serde_json::json!({ "type": "image_url", "image_url": { "url": url } }));
    }
    serde_json::Value::Array(parts)
}

/// Streams a chat completion for `prompt` from `base_url` (its OpenAI-style
/// base, e.g. `http://localhost:1234/v1`), yielding text deltas and `Split`s.
/// `images` are base64 `data:` URLs attached as vision inputs. `max_nodes`
/// guides the model on how many cards it may produce.
pub fn stream(
    base_url: String,
    model: String,
    api_key: String,
    prompt: String,
    images: Vec<String>,
    max_nodes: usize,
    thinking: bool,
) -> impl Stream<Item = Event> {
    event_stream(64, move |mut output| async move {
        if let Err(error) =
            run(&base_url, &model, &api_key, &prompt, &images, max_nodes, thinking, &mut output).await
        {
            let _ = output.send(Event::Error(error)).await;
        }
    })
}

/// An [`Event`] stream fed by an async producer, mirroring
/// `iced::stream::channel` (which the desktop UI used before this moved to the
/// core crate): a bounded mpsc channel whose receiver *is* the stream, with the
/// producer future driven to completion alongside it but yielding nothing.
fn event_stream<F>(
    buffer: usize,
    producer: impl FnOnce(Sender<Event>) -> F,
) -> impl Stream<Item = Event>
where
    F: std::future::Future<Output = ()>,
{
    let (sender, receiver) = mpsc::channel(buffer);
    let runner =
        futures::stream::once(producer(sender)).filter_map(|_| std::future::ready(None::<Event>));
    futures::stream::select(receiver, runner)
}

#[allow(clippy::too_many_arguments)]
async fn run(
    base_url: &str,
    model: &str,
    api_key: &str,
    prompt: &str,
    images: &[String],
    max_nodes: usize,
    thinking: bool,
    output: &mut Sender<Event>,
) -> Result<(), String> {
    let mut body = serde_json::json!({
        "model": model,
        "stream": true,
        "messages": [
            { "role": "system", "content": system_prompt(max_nodes) },
            { "role": "user", "content": user_content(prompt, images) },
        ],
    });
    apply_thinking(&mut body, base_url, thinking);

    let response = post_with_reasoning_retry(base_url, api_key, &mut body).await?;

    // Server-sent events: `data: {json}` lines, terminated by `data: [DONE]`.
    // Buffer bytes and only decode whole lines so multi-byte characters split
    // across chunks aren't corrupted. Accumulated answer text is emitted split
    // on `---` separators.
    let mut stream = response.bytes_stream();
    let mut sse: Vec<u8> = Vec::new();
    let mut content = String::new();
    while let Some(chunk) = stream.next().await {
        sse.extend_from_slice(&chunk.map_err(|_| "The connection dropped partway through the answer.".to_owned())?);
        while let Some(newline) = sse.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = sse.drain(..=newline).collect();
            let text = String::from_utf8_lossy(&line);
            let Some(data) = text.trim().strip_prefix("data:") else {
                continue;
            };
            let data = data.trim();
            if data == "[DONE]" {
                emit_segments(&mut content, output, true).await;
                let _ = output.send(Event::Done).await;
                return Ok(());
            }
            if let Some(delta) = parse_delta(data) {
                content.push_str(&delta);
            }
            if let Some(usage) = parse_usage(data) {
                let _ = output.send(Event::Usage(usage)).await;
            }
        }
        emit_segments(&mut content, output, false).await;
    }
    emit_segments(&mut content, output, true).await;
    let _ = output.send(Event::Done).await;
    Ok(())
}

/// Drains `content` into the events ready to emit: each `---` separator line
/// becomes a `Split` (its preceding text a `Delta`), then the remaining text a
/// `Delta`. Unless `is_final`, a trailing fragment that could be the start of a
/// separator is held back for the next chunk, and so is bold that has opened
/// and not yet closed (see [`open_bold_len`]); on the final flush (including
/// when a stream stops mid-sentence) everything left is emitted, so an
/// incomplete `---` ends up as literal text rather than being dropped.
fn drain_segments(content: &mut String, is_final: bool) -> Vec<Event> {
    let mut events = Vec::new();
    while let Some(pos) = content.find(SEPARATOR) {
        let mut before: String = content.drain(..pos + SEPARATOR.len()).collect();
        before.truncate(pos); // drop the separator itself
        if !before.is_empty() {
            events.push(Event::Delta(before));
        }
        events.push(Event::Split);
    }
    let hold = if is_final { 0 } else { separator_prefix_len(content).max(open_bold_len(content)) };
    if content.len() > hold {
        let emit: String = content.drain(..content.len() - hold).collect();
        if !emit.is_empty() {
            events.push(Event::Delta(emit));
        }
    }
    events
}

/// Emits [`drain_segments`] over the channel.
async fn emit_segments(content: &mut String, output: &mut Sender<Event>, is_final: bool) {
    for event in drain_segments(content, is_final) {
        let _ = output.send(event).await;
    }
}

/// Length of the longest suffix of `content` that is a prefix of [`SEPARATOR`],
/// so an in-progress separator isn't emitted as text. Always ASCII → safe cut.
fn separator_prefix_len(content: &str) -> usize {
    (1..=SEPARATOR.len().min(content.len()))
        .rev()
        .find(|&k| content.ends_with(&SEPARATOR[..k]))
        .unwrap_or(0)
}

/// The longest a run of bold is held back waiting for its closing `**`.
const BOLD_HOLD: usize = 160;

/// Length of the end of `content` that is bold still waiting to close — from
/// an opening `**` with no closing one after it, or a lone trailing `*` that
/// may be half of one.
///
/// A card is drawn as it arrives, and `**Taberna da Rua` drawn before its
/// closing pair shows the asterisks, then snaps to bold when they come.
/// Holding the run back until then costs a word or two of delay. An opener is
/// a `**` not followed by a space and a closer one not preceded by one —
/// CommonMark's flanking rules, simplified — so `x ** 2` isn't held at all. A
/// run longer than [`BOLD_HOLD`] is let through, since a `**` that never
/// closes (`**kwargs`, in code) would otherwise hold the answer until it ends.
/// Always ASCII → safe cut.
fn open_bold_len(content: &str) -> usize {
    let bytes = content.as_bytes();
    let mut open = None;
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'*' && bytes[i + 1] == b'*' {
            let after_space = i > 0 && bytes[i - 1].is_ascii_whitespace();
            let before_space = bytes.get(i + 2).is_some_and(u8::is_ascii_whitespace);
            match open {
                Some(_) if !after_space => open = None,
                None if !before_space => open = Some(i),
                _ => {}
            }
            i += 2;
        } else {
            i += 1;
        }
    }
    let held = match open {
        Some(at) => content.len() - at,
        None if i + 1 == bytes.len() && bytes[i] == b'*' => 1,
        None => 0,
    };
    if held > BOLD_HOLD { 0 } else { held }
}

/// The `choices[0].delta.content` string of a streamed chunk, if any.
fn parse_delta(data: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(data).ok()?;
    value
        .get("choices")?
        .get(0)?
        .get("delta")?
        .get("content")?
        .as_str()
        .map(str::to_owned)
}

/// The `usage` a streamed chunk carries, where it carries one: the last before
/// `[DONE]`, from an endpoint that sends it at all. OpenAI, asked for it,
/// sends `null` in every other chunk, which is none.
fn parse_usage(data: &str) -> Option<Usage> {
    if !data.contains("\"usage\"") {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(data).ok()?;
    let usage = value.get("usage").filter(|usage| usage.is_object())?;
    let tokens = |key: &str| usage.get(key).and_then(serde_json::Value::as_u64);
    Some(Usage {
        prompt_tokens: tokens("prompt_tokens")?,
        completion_tokens: tokens("completion_tokens")?,
        cost: usage.get("cost").and_then(serde_json::Value::as_f64),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures;

    fn node(id: &str, text: &str) -> CanvasNode {
        CanvasNode { text: Some(text.to_owned()), width: 10.0, height: 10.0, ..fixtures::node(id) }
    }

    fn edge(from: &str, to: &str) -> CanvasEdge {
        CanvasEdge { id: format!("{from}{to}"), ..fixtures::edge(from, to) }
    }

    /// A node of `kind` at `(x, y)` sized `w`x`h`, with `text` (or a label, for
    /// a group).
    fn gnode(id: &str, kind: &str, x: f32, y: f32, w: f32, h: f32, text: &str) -> CanvasNode {
        let mut node: CanvasNode = serde_json::from_value(serde_json::json!({
            "id": id, "type": kind, "x": x, "y": y, "width": w, "height": h,
        }))
        .unwrap();
        if kind == "group" {
            node.set_label(text);
        } else {
            node.text = Some(text.to_owned());
        }
        node
    }

    fn glink(from: &str, to: &str) -> CanvasEdge {
        serde_json::from_value(serde_json::json!({
            "id": format!("{from}-{to}"), "fromNode": from, "toNode": to,
        }))
        .unwrap()
    }

    #[test]
    fn a_group_prompt_follows_the_links_inside_it() {
        // frame encloses a -> b -> c and a stray d.
        let nodes = vec![
            gnode("frame", "group", 0.0, 0.0, 1000.0, 1000.0, "Planning"),
            gnode("a", "text", 50.0, 50.0, 100.0, 50.0, "root note"),
            gnode("b", "text", 50.0, 200.0, 100.0, 50.0, "child note"),
            gnode("c", "text", 50.0, 350.0, 100.0, 50.0, "grandchild"),
            gnode("d", "text", 500.0, 50.0, 100.0, 50.0, "unlinked"),
        ];
        let edges = vec![glink("a", "b"), glink("b", "c")];

        assert_eq!(
            group_prompt(&nodes, &edges, 0),
            "Planning\n- root note\n  - child note\n    - grandchild\n- unlinked"
        );
    }

    #[test]
    fn a_nested_frame_nests_its_contents_once() {
        // outer encloses inner, which encloses x. x's centre is inside *both*,
        // so the naive walk would list it twice.
        let nodes = vec![
            gnode("outer", "group", 0.0, 0.0, 1000.0, 1000.0, "Outer"),
            gnode("top", "text", 10.0, 10.0, 100.0, 50.0, "top level"),
            gnode("inner", "group", 200.0, 200.0, 400.0, 400.0, "Inner"),
            gnode("x", "text", 250.0, 250.0, 100.0, 50.0, "inside inner"),
        ];
        let out = group_prompt(&nodes, &[], 0);

        assert_eq!(out, "Outer\n- top level\n- Inner\n  - inside inner");
        assert_eq!(out.matches("inside inner").count(), 1, "listed once");
    }

    #[test]
    fn a_multi_line_card_stays_under_its_own_bullet() {
        let nodes = vec![
            gnode("frame", "group", 0.0, 0.0, 500.0, 500.0, ""),
            gnode("a", "text", 50.0, 50.0, 100.0, 50.0, "# Heading\n\nbody line"),
        ];
        // No label on the frame, so the outline starts straight at the bullet.
        assert_eq!(
            group_prompt(&nodes, &[], 0),
            "- # Heading\n  \n  body line"
        );
    }

    #[test]
    fn a_question_linked_from_a_frame_gets_the_frames_contents() {
        // The point of being able to draw that link: drag a port off a group,
        // type a question into the card it makes, and ask.
        let nodes = vec![
            gnode("frame", "group", 0.0, 0.0, 800.0, 600.0, "Planning"),
            gnode("a", "text", 50.0, 50.0, 100.0, 50.0, "first note"),
            gnode("b", "text", 50.0, 200.0, 100.0, 50.0, "second note"),
            // The question sits outside the frame, linked from it.
            gnode("q", "text", 2000.0, 300.0, 100.0, 50.0, "What should we do?"),
        ];
        let edges = vec![glink("frame", "q")];

        let prompt = upstream_prompt(&nodes, &edges, "q");
        assert!(prompt.contains("first note"), "{prompt:?}");
        assert!(prompt.contains("second note"), "{prompt:?}");
        assert!(prompt.contains("Planning"), "the caption leads it: {prompt:?}");
        // The group's contents come first, the question last.
        assert!(
            prompt.find("first note").unwrap() < prompt.find("What should we do?").unwrap(),
            "{prompt:?}"
        );
    }

    #[test]
    fn a_frame_with_no_contents_adds_nothing_to_a_chain() {
        let nodes = vec![
            gnode("frame", "group", 0.0, 0.0, 800.0, 600.0, "Empty"),
            gnode("q", "text", 2000.0, 300.0, 100.0, 50.0, "a question"),
        ];
        let edges = vec![glink("frame", "q")];
        assert_eq!(upstream_prompt(&nodes, &edges, "q"), "a question");
    }

    #[test]
    fn images_inside_a_linked_frame_ride_along() {
        let mut img = gnode("img", "file", 50.0, 50.0, 100.0, 50.0, "");
        img.file = Some("pics/a.png".to_owned());
        let nodes = vec![
            gnode("frame", "group", 0.0, 0.0, 800.0, 600.0, "With a picture"),
            img,
            gnode("q", "text", 2000.0, 300.0, 100.0, 50.0, "what is this?"),
        ];
        let edges = vec![glink("frame", "q")];
        assert_eq!(upstream_images(&nodes, &edges, "q"), vec!["pics/a.png".to_owned()]);
    }

    #[test]
    fn a_copy_of_a_frame_says_more_than_its_prompt() {
        // Ask AI's prompt stays as it's always been; a copy also has a link
        // card's URL and the label on a link.
        let mut link = gnode("docs", "link", 50.0, 200.0, 100.0, 50.0, "");
        link.text = None;
        link.extra.insert("url".to_owned(), serde_json::json!("https://example.com"));
        let nodes = vec![
            gnode("frame", "group", 0.0, 0.0, 800.0, 600.0, "Bug"),
            gnode("a", "text", 50.0, 50.0, 100.0, 50.0, "first note"),
            gnode("b", "text", 300.0, 50.0, 100.0, 50.0, "second note"),
            link,
        ];
        let mut ruled_out = glink("a", "b");
        ruled_out.set_label("ruled out");
        let edges = vec![ruled_out];
        assert_eq!(group_prompt(&nodes, &edges, 0), "Bug\n- first note\n  - second note");
        assert_eq!(
            group_outline(&nodes, &edges, 0, Outline::Copy),
            "Bug\n- first note\n  - → ruled out: second note\n- https://example.com"
        );
    }

    #[test]
    fn an_empty_frame_makes_no_prompt() {
        let nodes = vec![gnode("frame", "group", 0.0, 0.0, 500.0, 500.0, "Empty")];
        assert!(group_prompt(&nodes, &[], 0).is_empty());
    }

    #[test]
    fn a_cycle_inside_a_frame_still_terminates() {
        let nodes = vec![
            gnode("frame", "group", 0.0, 0.0, 900.0, 900.0, "Loop"),
            gnode("a", "text", 50.0, 50.0, 100.0, 50.0, "first"),
            gnode("b", "text", 50.0, 200.0, 100.0, 50.0, "second"),
        ];
        let edges = vec![glink("a", "b"), glink("b", "a")];
        let out = group_prompt(&nodes, &edges, 0);
        // Both appear exactly once despite neither being a root.
        assert_eq!(out.matches("first").count(), 1);
        assert_eq!(out.matches("second").count(), 1);
    }

    #[test]
    fn group_images_picks_up_only_the_images_inside() {
        let mut img = gnode("img", "file", 50.0, 50.0, 100.0, 50.0, "");
        img.file = Some("pics/a.png".to_owned());
        let mut note = gnode("note", "file", 50.0, 200.0, 100.0, 50.0, "");
        note.file = Some("Notes/x.md".to_owned());
        let mut outside = gnode("out", "file", 5000.0, 5000.0, 100.0, 50.0, "");
        outside.file = Some("pics/b.png".to_owned());
        let nodes = vec![
            gnode("frame", "group", 0.0, 0.0, 900.0, 900.0, ""),
            img,
            note,
            outside,
        ];
        assert_eq!(group_images(&nodes, 0), vec!["pics/a.png".to_owned()]);
    }

    #[test]
    fn upstream_prompt_chains_incoming_links_in_order() {
        let nodes = [node("a", "A text"), node("b", "B text"), node("c", "C text")];
        let edges = [edge("a", "b"), edge("b", "c")];
        assert_eq!(
            upstream_prompt(&nodes, &edges, "c"),
            "A text\n\nB text\n\nC text"
        );
    }

    #[test]
    fn apply_thinking_disables_reasoning_only_for_openrouter_when_off() {
        // Thinking on: nothing added, whatever the provider.
        let mut on = serde_json::json!({ "model": "m" });
        apply_thinking(&mut on, "https://openrouter.ai/api/v1", true);
        assert!(on.get("reasoning").is_none());

        // Off, but not OpenRouter (local or strict hosted): left at the model
        // default — no fields, so nothing can 400.
        for url in ["http://localhost:1234/v1", "https://api.openai.com/v1"] {
            let mut body = serde_json::json!({ "model": "m" });
            apply_thinking(&mut body, url, false);
            assert!(body.get("reasoning").is_none(), "{url}");
            assert!(body.get("chat_template_kwargs").is_none(), "{url}");
        }

        // OpenRouter off: a typed object disables reasoning (a string would 400).
        let mut off = serde_json::json!({ "model": "m" });
        apply_thinking(&mut off, "https://openrouter.ai/api/v1", false);
        assert_eq!(off["reasoning"], serde_json::json!({ "enabled": false }));
    }

    #[test]
    fn apply_thinking_uses_lowest_effort_once_a_model_mandates_reasoning() {
        // A distinct model name so this doesn't perturb the global cache other
        // tests rely on.
        let model = "reasoning-mandatory-test-model";

        // Before learning: OpenRouter-off disables reasoning as usual.
        let mut first = serde_json::json!({ "model": model });
        apply_thinking(&mut first, "https://openrouter.ai/api/v1", false);
        assert_eq!(first["reasoning"], serde_json::json!({ "enabled": false }));

        // After learning it mandates reasoning: request the lowest effort instead
        // of a disable (which would 400), excluded so it doesn't leak into cards.
        mark_reasoning_required(model);
        let mut later = serde_json::json!({ "model": model });
        apply_thinking(&mut later, "https://openrouter.ai/api/v1", false);
        assert_eq!(
            later["reasoning"],
            serde_json::json!({ "effort": "low", "exclude": true })
        );
    }

    #[test]
    fn is_reasoning_mandatory_matches_the_openrouter_rejection() {
        let body = r#"{"error":{"message":"Reasoning is mandatory for this endpoint and cannot be disabled.","code":400}}"#;
        assert!(is_reasoning_mandatory(body));
        // Unrelated errors must not trip the reasoning retry — including other
        // reasoning-related 400s that aren't about it being mandatory.
        assert!(!is_reasoning_mandatory(r#"{"error":{"message":"model not found"}}"#));
        assert!(!is_reasoning_mandatory("reasoning: Expected object"));
    }

    #[test]
    fn error_detail_pulls_the_openai_style_message() {
        let body = r#"{"error":{"message":"reasoning: Expected object","code":400}}"#;
        assert_eq!(error_detail(body), "reasoning: Expected object");
        // Non-JSON falls back to the trimmed raw body.
        assert_eq!(error_detail("  Bad Request  "), "Bad Request");
        assert_eq!(error_detail("   "), "no response body");
    }

    #[test]
    fn clean_suggestion_strips_list_markers_and_blanks() {
        assert_eq!(clean_suggestion("- Route options"), Some("Route options".to_owned()));
        assert_eq!(clean_suggestion("* Gear list"), Some("Gear list".to_owned()));
        assert_eq!(clean_suggestion("1. Timeline"), Some("Timeline".to_owned()));
        assert_eq!(clean_suggestion("  Plain  "), Some("Plain".to_owned()));
        assert_eq!(clean_suggestion("   "), None);
    }

    #[test]
    fn separator_prefix_len_holds_partial_separators() {
        assert_eq!(separator_prefix_len("hello\n---"), 4); // could still become a separator
        assert_eq!(separator_prefix_len("hello\n"), 1);
        assert_eq!(separator_prefix_len("a\n-"), 2);
        assert_eq!(separator_prefix_len("plain text"), 0);
    }

    /// Feeds `chunks` through `drain_segments` as a streaming answer, then a
    /// final flush (a stream that just stops — no `[DONE]`). Returns the joined
    /// text of all `Delta`s, the number of `Split`s, and whether `content` was
    /// fully drained.
    fn run_stream(chunks: &[&str]) -> (String, usize, bool) {
        let mut content = String::new();
        let mut text = String::new();
        let mut splits = 0;
        let mut apply = |events: Vec<Event>| {
            for event in events {
                match event {
                    Event::Delta(t) => text.push_str(&t),
                    Event::Split => splits += 1,
                    other => panic!("unexpected event {other:?}"),
                }
            }
        };
        for chunk in chunks {
            content.push_str(chunk);
            apply(drain_segments(&mut content, false));
        }
        apply(drain_segments(&mut content, true)); // stream stopped: flush.
        (text, splits, content.is_empty())
    }

    #[test]
    fn drain_segments_reassembles_a_streamed_answer_across_chunk_splits() {
        // A separator split across two chunks still yields exactly one Split,
        // and no separator text leaks into the cards.
        let (text, splits, drained) = run_stream(&["First note\n-", "--\nSecond note"]);
        assert_eq!(text, "First noteSecond note");
        assert_eq!(splits, 1);
        assert!(drained);
    }

    #[test]
    fn a_stream_that_stops_mid_separator_flushes_the_partial_as_text() {
        // The model emits an incomplete `---` and the stream drops. The partial
        // never became a real split, so it's flushed as literal text, not lost.
        let (text, splits, drained) = run_stream(&["Half an answer\n-", "-"]);
        assert_eq!(text, "Half an answer\n--");
        assert_eq!(splits, 0);
        assert!(drained);
    }

    #[test]
    fn a_stream_that_stops_after_a_completed_separator_still_splits() {
        // The separator completed before the stream cut off mid-second-card.
        let (text, splits, drained) = run_stream(&["A\n---\nB, cut off here"]);
        assert_eq!(text, "AB, cut off here");
        assert_eq!(splits, 1);
        assert!(drained);
    }

    #[test]
    fn drain_segments_holds_a_partial_separator_until_the_final_flush() {
        // Mid-stream, a trailing partial separator is withheld (it might still
        // complete); the final flush releases whatever is left.
        let mut content = "text so far\n--".to_owned();
        assert_eq!(drain_segments(&mut content, false), vec![Event::Delta("text so far".to_owned())]);
        assert_eq!(content, "\n--"); // the maybe-separator is held back
        assert_eq!(drain_segments(&mut content, true), vec![Event::Delta("\n--".to_owned())]);
        assert!(content.is_empty());
    }

    #[test]
    fn bold_is_held_until_it_closes() {
        // Mid-stream, the open run waits; what's before it doesn't.
        let mut content = "Try **Taberna da".to_owned();
        assert_eq!(drain_segments(&mut content, false), vec![Event::Delta("Try ".to_owned())]);
        content.push_str(" Rua** — small");
        assert_eq!(drain_segments(&mut content, false), vec![Event::Delta("**Taberna da Rua** — small".to_owned())]);
        // A lone `*` at the end may be the first half of a pair.
        let mut content = "Try *".to_owned();
        assert_eq!(drain_segments(&mut content, false), vec![Event::Delta("Try ".to_owned())]);
        // However it was split, the answer arrives whole.
        let (text, splits, drained) = run_stream(&["**Cerve", "jaria Ramiro*", "* — seafood"]);
        assert_eq!((text.as_str(), splits, drained), ("**Cervejaria Ramiro** — seafood", 0, true));
    }

    #[test]
    fn what_isnt_bold_is_not_held() {
        assert_eq!(open_bold_len("a **b** c"), 0, "closed");
        assert_eq!(open_bold_len("x ** 2"), 0, "an operator, not an opener");
        assert_eq!(open_bold_len("a **b** c *"), 1, "a lone star could still pair");
        let runaway = format!("def f(**{}", "kwargs, ".repeat(30));
        assert_eq!(open_bold_len(&runaway), 0, "never closing, so let through");
        // Final flush releases a run that never closed.
        let (text, _, drained) = run_stream(&["Half **an answer"]);
        assert_eq!(text, "Half **an answer");
        assert!(drained);
    }

    #[test]
    fn drain_segments_makes_no_events_from_empty_content() {
        let mut content = String::new();
        assert!(drain_segments(&mut content, false).is_empty());
        assert!(drain_segments(&mut content, true).is_empty());
    }

    #[test]
    fn parse_delta_extracts_content_and_tolerates_junk() {
        assert_eq!(
            parse_delta(r#"{"choices":[{"delta":{"content":"hi"}}]}"#),
            Some("hi".to_owned())
        );
        // A role-only / finish chunk carries no content.
        assert_eq!(parse_delta(r#"{"choices":[{"delta":{"role":"assistant"}}]}"#), None);
        assert_eq!(parse_delta(r#"{"choices":[]}"#), None);
        // A truncated SSE line (stream cut mid-JSON) must not panic.
        assert_eq!(parse_delta(r#"{"choices":[{"delta":{"content""#), None);
        assert_eq!(parse_delta(""), None);
    }

    #[test]
    fn parse_usage_reads_openrouters_last_chunk_and_skips_the_rest() {
        // OpenRouter's accounting frame: an empty delta, and the usage.
        let last = r#"{"choices":[{"delta":{"content":""},"finish_reason":"stop"}],"usage":{"prompt_tokens":812,"completion_tokens":403,"total_tokens":1215,"cost":0.00031}}"#;
        assert_eq!(parse_usage(last), Some(Usage { prompt_tokens: 812, completion_tokens: 403, cost: Some(0.00031) }));
        // Tokens without a cost, as OpenAI reports them.
        let tokens = r#"{"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}}"#;
        assert_eq!(parse_usage(tokens).and_then(|usage| usage.cost), None);
        assert!(parse_usage(tokens).is_some());
        // Every other chunk: none, or null.
        assert_eq!(parse_usage(r#"{"choices":[{"delta":{"content":"hi"}}]}"#), None);
        assert_eq!(parse_usage(r#"{"choices":[{"delta":{"content":"hi"}}],"usage":null}"#), None);
    }

    #[test]
    fn upstream_prompt_is_just_the_node_when_unlinked() {
        let nodes = [node("solo", "only me")];
        assert_eq!(upstream_prompt(&nodes, &[], "solo"), "only me");
    }

    #[test]
    fn upstream_prompt_tolerates_cycles() {
        let nodes = [node("a", "A"), node("b", "B")];
        let edges = [edge("a", "b"), edge("b", "a")];
        // Should terminate and include both, not loop forever.
        assert_eq!(upstream_prompt(&nodes, &edges, "b"), "A\n\nB");
    }

    #[test]
    fn upstream_prompt_includes_all_parents() {
        // A, B, C all feed D — every parent is in the prompt, target last.
        let nodes = [
            node("a", "A text"),
            node("b", "B text"),
            node("c", "C text"),
            node("d", "D text"),
        ];
        let edges = [edge("a", "d"), edge("b", "d"), edge("c", "d")];
        assert_eq!(
            upstream_prompt(&nodes, &edges, "d"),
            "A text\n\nB text\n\nC text\n\nD text"
        );
    }

    #[test]
    fn upstream_prompt_orders_a_diamond_topologically() {
        // A -> B, A -> C, B -> D, C -> D: A precedes B and C, all precede D,
        // and the shared ancestor A appears only once.
        let nodes = [node("a", "A"), node("b", "B"), node("c", "C"), node("d", "D")];
        let edges = [edge("a", "b"), edge("a", "c"), edge("b", "d"), edge("c", "d")];
        assert_eq!(upstream_prompt(&nodes, &edges, "d"), "A\n\nB\n\nC\n\nD");
    }

    #[test]
    fn upstream_images_collects_ancestor_file_nodes() {
        let mut img = node("img", "");
        img.kind = "file".to_owned();
        img.text = None;
        img.file = Some("pics/a.png".to_owned());
        let nodes = [img, node("b", "B text"), node("c", "C text")];
        let edges = [edge("img", "b"), edge("b", "c")];
        // The file card is an image input, and contributes no text.
        assert_eq!(upstream_images(&nodes, &edges, "c"), vec!["pics/a.png".to_owned()]);
        assert_eq!(upstream_prompt(&nodes, &edges, "c"), "B text\n\nC text");
    }

    #[test]
    fn user_content_is_a_plain_string_without_images() {
        assert_eq!(user_content("hi", &[]), serde_json::Value::String("hi".to_owned()));
    }

    #[test]
    fn user_content_is_a_multipart_array_with_images() {
        let content = user_content("look", &["data:image/png;base64,AAA".to_owned()]);
        let parts = content.as_array().unwrap();
        assert_eq!(parts[0]["type"], "text");
        assert_eq!(parts[0]["text"], "look");
        assert_eq!(parts[1]["type"], "image_url");
        assert_eq!(parts[1]["image_url"]["url"], "data:image/png;base64,AAA");
    }

    #[test]
    fn model_lists_are_read_from_either_shape() {
        // OpenRouter: rich entries, a display name among them.
        let openrouter = serde_json::json!({"data": [
            {"id": "anthropic/claude-sonnet-4", "name": "Anthropic: Claude Sonnet 4",
             "context_length": 200000},
            {"id": "openai/gpt-4o", "name": "OpenAI: GPT-4o", "context_length": 128000},
        ]});
        let models = parse_models(&openrouter);
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "anthropic/claude-sonnet-4");
        assert_eq!(models[0].name.as_deref(), Some("Anthropic: Claude Sonnet 4"));

        // Fireworks and plain OpenAI: an id and nothing else worth showing.
        let plain = serde_json::json!({"data": [
            {"id": "accounts/fireworks/models/llama-v3p1-70b-instruct", "object": "model"},
        ]});
        let models = parse_models(&plain);
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].name, None, "no name is better than a repeated id");

        // A bare array, which a couple of endpoints return instead of `data`.
        let bare = serde_json::json!([{"id": "local-model"}]);
        assert_eq!(parse_models(&bare)[0].id, "local-model");

        // Anthropic's and Together's title, under another field.
        let anthropic = serde_json::json!({"data": [{"id": "claude-sonnet-4-5", "display_name": "Claude Sonnet 4.5"}]});
        assert_eq!(parse_models(&anthropic)[0].name.as_deref(), Some("Claude Sonnet 4.5"));
        // Gemini's resource names, as a question names them.
        let gemini = serde_json::json!({"data": [{"id": "models/gemini-2.5-flash", "object": "model"}]});
        assert_eq!(parse_models(&gemini)[0].id, "gemini-2.5-flash");
    }

    #[test]
    fn a_model_list_in_pages_is_followed_to_its_end() {
        let page = |more: bool, last: &str| serde_json::json!({"data": [], "has_more": more, "last_id": last});
        assert_eq!(next_page(&page(true, "claude-3-haiku-20240307")), Some("claude-3-haiku-20240307"));
        assert_eq!(next_page(&page(false, "claude-3-haiku-20240307")), None, "the last page");
        assert_eq!(next_page(&serde_json::json!({"data": []})), None, "a list that doesn't page");
        assert_eq!(next_page(&page(true, "a&b=c")), None, "nothing that would need escaping in a query");
    }

    #[test]
    fn a_free_model_costs_nothing_either_way_and_answers_in_text() {
        let body = serde_json::json!({"data": [
            {"id": "google/gemma-4-31b-it:free", "pricing": {"prompt": "0", "completion": "0"},
             "architecture": {"output_modalities": ["text"]}},
            {"id": "openrouter/free", "pricing": {"prompt": "0", "completion": "0"}},
            // Music, at nothing: not something to ask a question of.
            {"id": "google/lyria-3-pro-preview", "pricing": {"prompt": "0", "completion": "0"},
             "architecture": {"output_modalities": ["text", "audio"]}},
            {"id": "openai/gpt-4o", "pricing": {"prompt": "0.0000025", "completion": "0.00001"}},
            {"id": "half/free", "pricing": {"prompt": "0", "completion": "0.000001"}},
            // No price given is unknown, not free.
            {"id": "local-model"},
        ]});
        let models = parse_models(&body);
        let free: Vec<&str> = models.iter().filter(|m| m.free).map(|m| m.id.as_str()).collect();
        assert_eq!(free, ["google/gemma-4-31b-it:free", "openrouter/free"]);

        // The price is kept, per token; none given is none known.
        let price = |id: &str| models.iter().find(|m| m.id == id).and_then(|m| m.price);
        assert_eq!(price("openai/gpt-4o"), Some(Price { prompt: 0.0000025, completion: 0.00001 }));
        assert_eq!(price("local-model"), None);
        // OpenRouter's "varies", for a router, isn't a price.
        let auto = serde_json::json!([{"id": "openrouter/auto", "pricing": {"prompt": "-1", "completion": "-1"}}]);
        assert_eq!(parse_models(&auto)[0].price, None);
    }

    #[test]
    fn unusable_entries_are_dropped_and_the_rest_sorted() {
        let body = serde_json::json!({"data": [
            {"id": "zeta"},
            {"object": "model"},          // no id: not selectable, so not shown
            {"id": "   "},                // nor is a blank one
            {"id": "Alpha"},
            {"id": "zeta"},               // a duplicate is one model, not two
            {"id": "beta", "name": "beta"},
        ]});
        let models = parse_models(&body);
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["Alpha", "beta", "zeta"], "case-insensitive, so vendors group");
        assert_eq!(models[1].name, None, "a name equal to the id says nothing twice");
    }

    #[test]
    fn credits_and_a_free_models_limit_say_so() {
        use reqwest::StatusCode;
        let openrouter = "https://openrouter.ai/api/v1";
        let broke = describe_failure(StatusCode::PAYMENT_REQUIRED, r#"{"error": {"message": "Insufficient credits"}}"#, openrouter, "sk-or", Asked::Chat { model: "openai/gpt-4o" });
        assert!(broke.starts_with("OpenRouter wants credits"), "{broke}");

        let limited = |model| describe_failure(StatusCode::TOO_MANY_REQUESTS, r#"{"error": {"message": "Rate limit exceeded"}}"#, openrouter, "sk-or", Asked::Chat { model });
        for free in ["google/gemma-4-31b-it:free", "openrouter/free"] {
            assert!(limited(free).contains("limit for its free models"), "{}", limited(free));
        }
        assert!(limited("openai/gpt-4o").contains("wait a moment"), "a paid model's limit lifts by itself");

        // Every provider of a free model ruled out by the account's privacy
        // settings: a 404, which isn't nothing answering at the URL.
        let body = r#"{"error": {"message": "No endpoints found matching your data policy (Free model training). Configure: https://openrouter.ai/settings/privacy", "code": 404}}"#;
        let ruled_out = describe_failure(StatusCode::NOT_FOUND, body, openrouter, "sk-or", Asked::Chat { model: "openrouter/free" });
        assert!(ruled_out.starts_with("Your privacy settings on OpenRouter rule out every provider of “openrouter/free”"), "{ruled_out}");
        assert!(ruled_out.contains("https://openrouter.ai/settings/privacy"), "the link to the setting: {ruled_out}");
    }

    #[test]
    fn a_destination_is_named_as_a_person_would() {
        assert_eq!(destination("https://openrouter.ai/api/v1/"), "OpenRouter");
        assert_eq!(destination("http://localhost:1234/v1"), "LM Studio");
        assert_eq!(destination("http://localhost:8080/v1"), "localhost:8080");
        assert_eq!(destination("https://api.example.com/v1"), "api.example.com");
        // llama.cpp's server moved off 8080, where an older one still is.
        assert!(unreachable("http://localhost:9931/v1").contains("If it's on 8080, choose Custom"));
    }

    #[test]
    fn the_presets_are_listed_a_kind_at_a_time() {
        // Each kind in one run, so a section header over its first stands
        // over all of them.
        let mut seen = Vec::new();
        for preset in PRESETS {
            if seen.last() != Some(&preset.kind) {
                assert!(!seen.contains(&preset.kind), "{} out of its kind's run", preset.name);
                seen.push(preset.kind);
            }
        }
        assert_eq!(seen.first(), Some(&ProviderKind::Local), "this machine first");
    }

    #[test]
    fn a_model_is_named_without_its_vendor_or_whether_its_free() {
        assert_eq!(short_name("anthropic/claude-sonnet-4.5", Some("Anthropic: Claude Sonnet 4.5")), "Claude Sonnet 4.5");
        assert_eq!(short_name("google/gemma-3-27b-it:free", Some("Google: Gemma 3 27B (free)")), "Gemma 3 27B");
        assert_eq!(short_name("openrouter/free", Some("Free Models Router")), "Free Models Router");
        assert_eq!(short_name("openrouter/free", None), "Free Models Router", "before it's listed");
        // Before the list is fetched, or where the endpoint gives no title.
        assert_eq!(short_name("meta-llama/llama-3.3-70b-instruct:free", None), "llama-3.3-70b-instruct");
        assert_eq!(short_name("llama3.2:latest", None), "llama3.2");
        assert_eq!(short_name("qwen2.5:7b", Some("")), "qwen2.5:7b");
        assert_eq!(short_name("accounts/fireworks/models/llama-v3p1-70b-instruct", None), "llama-v3p1-70b-instruct");
    }

    #[test]
    fn a_failure_says_what_to_do_about_it() {
        use reqwest::StatusCode;
        let lm_studio = "http://localhost:1234/v1";
        let chat = Asked::Chat { model: "google/gemma-4-26b-a4b-qat" };

        // No key sent: a request for one, from the provider by name.
        let no_key = describe_failure(StatusCode::UNAUTHORIZED, r#"{"error": {"message": "No auth credentials found"}}"#, "https://api.openai.com/v1", "", chat);
        assert_eq!(no_key, "OpenAI wants an API key — paste one in Settings → AI.");
        // A key sent and refused: the same sentence for the list and a question.
        let rejected = |asked| describe_failure(StatusCode::UNAUTHORIZED, "{}", lm_studio, "sk-1", asked);
        assert_eq!(rejected(Asked::Models), rejected(chat));
        assert!(rejected(chat).contains("check it in Settings → AI"));
        // Gemini says it with a 400.
        let gemini = "https://generativelanguage.googleapis.com/v1beta/openai";
        let invalid = r#"[{"error": {"code": 400, "message": "API key not valid. Please pass a valid API key.", "status": "INVALID_ARGUMENT"}}]"#;
        assert_eq!(describe_failure(StatusCode::BAD_REQUEST, invalid, gemini, "AIza-1", Asked::Models), rejected(Asked::Models));
        assert_eq!(describe_failure(StatusCode::BAD_REQUEST, invalid, gemini, "", chat), "Google Gemini wants an API key — paste one in Settings → AI.");

        // A local runtime without the route: ordinary, and nothing to do with a key.
        let missing = describe_failure(StatusCode::NOT_FOUND, "<html>404</html>", lm_studio, "", Asked::Models);
        assert!(missing.contains("doesn't list its models"), "{missing}");

        // A question for a model the provider hasn't got, in three runtimes' words.
        for body in [
            r#"{"error": {"message": "The model `google/gemma-4-26b-a4b-qat` does not exist or you do not have access to it."}}"#,
            r#"{"error": "model \"gemma\" not found, try pulling it first"}"#,
            r#"{"error": {"message": "google/gemma is not a valid model ID"}}"#,
        ] {
            let unserved = describe_failure(StatusCode::NOT_FOUND, body, lm_studio, "", chat);
            assert_eq!(unserved, "LM Studio doesn't serve “google/gemma-4-26b-a4b-qat” — choose another model in Settings → AI.");
        }
        // A 404 that isn't about a model is the URL.
        let elsewhere = describe_failure(StatusCode::NOT_FOUND, "Not Found", "http://example.com/api", "", chat);
        assert!(elsewhere.contains("Nothing answers at http://example.com/api"), "{elsewhere}");
    }

    #[test]
    fn nothing_listening_asks_whether_it_is_running() {
        assert_eq!(unreachable("http://localhost:1234/v1"), "Couldn't reach LM Studio at localhost:1234 — is it running?");
        assert_eq!(unreachable("http://127.0.0.1:8080/v1"), "Nothing is answering at 127.0.0.1:8080 — is the server running?");
        assert!(unreachable("https://api.openai.com/v1").starts_with("Couldn't reach api.openai.com — check the connection"));
    }

}
