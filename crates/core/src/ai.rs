//! Streaming chat completions against an OpenAI-compatible endpoint (LM Studio,
//! OpenAI, …).
//!
//! Ad-hoc trigger for now: coloring a node yellow asks the endpoint with the
//! chain of upstream notes as the prompt, and streams the answer into a new
//! node branching off it. See `App::trigger_ai`.

use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};

use futures::channel::mpsc::{self, Sender};
use futures::{SinkExt, Stream, StreamExt};

use crate::model::{CanvasEdge, CanvasNode, NodeKind};

/// A streamed event from the endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A chunk of the current note's text to append.
    Delta(String),
    /// The model asked to start a new sibling note.
    Split,
    /// The answer finished.
    Done,
    /// The request failed (human-readable reason).
    Error(String),
}

/// A line of exactly this (per the system prompt) starts a new note.
const SEPARATOR: &str = "\n---\n";

/// A known AI provider: a display name and its OpenAI-style base URL. Selecting
/// one in settings fills the endpoint in (the model is left untouched).
pub struct Preset {
    pub name: &'static str,
    pub url: &'static str,
}

/// The providers offered as one-click presets. `Custom` (any other URL) is not
/// listed here — the settings picker adds it.
pub const PRESETS: &[Preset] = &[
    // Local runtimes first (no key needed), then the major hosted providers.
    Preset { name: "LM Studio", url: "http://localhost:1234/v1" },
    Preset { name: "Ollama", url: "http://localhost:11434/v1" },
    Preset { name: "OpenAI", url: "https://api.openai.com/v1" },
    Preset { name: "OpenRouter", url: "https://openrouter.ai/api/v1" },
    Preset { name: "Groq", url: "https://api.groq.com/openai/v1" },
    Preset { name: "Together AI", url: "https://api.together.xyz/v1" },
    Preset { name: "Fireworks AI", url: "https://api.fireworks.ai/inference/v1" },
    Preset { name: "Mistral", url: "https://api.mistral.ai/v1" },
    Preset { name: "DeepSeek", url: "https://api.deepseek.com/v1" },
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
    /// Context window in tokens, where stated. Worth showing: it's the thing
    /// most likely to decide between two models with similar names.
    pub context: Option<u64>,
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
    let response = authed_get(&url, api_key).send().await.map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(describe_failure(status, &body));
    }
    let body: serde_json::Value = response.json().await.map_err(|e| e.to_string())?;
    Ok(parse_models(&body))
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
            (!id.is_empty()).then(|| Model {
                id: id.to_owned(),
                name: item
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|name| !name.is_empty() && *name != id)
                    .map(str::to_owned),
                context: ["context_length", "context_window", "max_context_length"]
                    .iter()
                    .find_map(|key| item.get(*key).and_then(serde_json::Value::as_u64)),
            })
        })
        .collect();
    // Alphabetical, which for these ids groups a vendor's models together
    // ("anthropic/…", "openai/…") — the order you'd want to scroll.
    models.sort_by_key(|model| model.id.to_lowercase());
    models.dedup_by(|a, b| a.id == b.id);
    models
}

/// A failed request, in a sentence a user can act on rather than a status code.
fn describe_failure(status: reqwest::StatusCode, body: &str) -> String {
    let detail = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            v.pointer("/error/message")
                .or_else(|| v.pointer("/message"))
                .and_then(|m| m.as_str())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| body.chars().take(200).collect());
    match status {
        reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN => {
            format!("{status}: the API key was rejected ({detail})")
        }
        reqwest::StatusCode::NOT_FOUND => {
            format!("{status}: this endpoint has no model list ({detail})")
        }
        _ => format!("{status}: {detail}"),
    }
}

fn system_prompt(max_notes: usize) -> String {
    format!(
        "You are a helpful assistant inside a mind-mapping canvas. The user's \
         query is formed by a chain of connected notes. Answer concisely. If — \
         and only if — the answer is naturally a list of distinct, standalone \
         items (options, steps, alternatives), split it into separate notes by \
         putting a line containing only `---` between them; make each note \
         self-contained. Prefer a single note for explanations or prose, and \
         never produce more than {max_notes} notes."
    )
}

/// Every note feeding into `target` (all ancestors, following incoming links,
/// not just one chain), topologically ordered so each note precedes the ones it
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

/// Builds the text prompt for `target`: the text of its ancestor notes joined
/// by blank lines (see [`upstream_nodes`] for the order). Image (file) notes
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
/// about a whole group rather than a chain of notes.
///
/// A group has no incoming links to walk — it is a region, not a step in a
/// chain — so the prompt is built from its contents instead, preserving the two
/// hierarchies the user actually sees: links between the notes inside it (a
/// note's targets are nested under it) and frames within frames (a nested
/// group's contents are nested under its caption). Each note is emitted as a
/// bullet, its own line breaks indented to stay under it.
pub fn group_prompt(nodes: &[CanvasNode], edges: &[CanvasEdge], frame: usize) -> String {
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
    write_container(nodes, edges, frame, &members, &member_set, 0, &mut emitted, &mut out);
    out.trim_end().to_owned()
}

/// Emits the members whose innermost enclosing frame (within the set) is
/// `container`, following links so a note's targets nest under it.
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
) {
    let direct: Vec<usize> = members
        .iter()
        .copied()
        // A node inside a nested frame belongs to that frame, not to this one.
        .filter(|&m| innermost_frame(nodes, m, member_set).unwrap_or(container) == container)
        .collect();

    // Roots first: a note with no link from a sibling in the same container
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
        write_member(nodes, edges, m, members, member_set, depth, emitted, out);
    }
    for &m in &direct {
        write_member(nodes, edges, m, members, member_set, depth, emitted, out);
    }
}

/// Emits one member and, beneath it, whatever it links to or encloses.
#[allow(clippy::too_many_arguments)] // as above
fn write_member(
    nodes: &[CanvasNode],
    edges: &[CanvasEdge],
    member: usize,
    members: &[usize],
    member_set: &HashSet<usize>,
    depth: usize,
    emitted: &mut HashSet<usize>,
    out: &mut String,
) {
    if !emitted.insert(member) {
        return;
    }
    let node = &nodes[member];
    let indent = "  ".repeat(depth);

    if node.is_group() {
        let label = node.label().map(str::trim).filter(|l| !l.is_empty());
        out.push_str(&format!("{indent}- {}\n", label.unwrap_or("(unnamed group)")));
        write_container(nodes, edges, member, members, member_set, depth + 1, emitted, out);
        return;
    }

    let text = node.text.as_deref().unwrap_or("").trim();
    if !text.is_empty() {
        // Keep a multi-line note under its own bullet rather than letting later
        // lines read as separate items.
        let mut lines = text.lines();
        if let Some(first) = lines.next() {
            out.push_str(&format!("{indent}- {first}\n"));
        }
        for line in lines {
            out.push_str(&format!("{indent}  {line}\n"));
        }
    }

    // Notes this one links to, nested beneath it.
    for edge in edges.iter().filter(|e| e.from_node == node.id) {
        let Some(&target) = members
            .iter()
            .find(|&&m| nodes[m].id == edge.to_node && member_set.contains(&m))
        else {
            continue;
        };
        write_member(nodes, edges, target, members, member_set, depth + 1, emitted, out);
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

/// The `file` paths of the image notes a group frame encloses, to attach as
/// vision inputs alongside [`group_prompt`].
pub fn group_images(nodes: &[CanvasNode], frame: usize) -> Vec<String> {
    crate::model::enclosed_by(nodes, frame)
        .into_iter()
        .filter(|&i| nodes[i].node_kind() == NodeKind::Image)
        .filter_map(|i| nodes[i].file.clone())
        .collect()
}

/// The `file` paths of image (file) notes among `target`'s ancestors, in the
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

/// Post-order DFS over incoming links from `node`, pushing each note after all
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
/// sibling notes that could branch alongside the note chain in `context`.
/// `existing` lists the sibling notes already present so suggestions stay on
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
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
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

    let response = post_with_reasoning_retry(&url, &api_key, &mut body).await?;
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
/// budget, excluded from the response so it doesn't leak into generated notes.
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
    url: &str,
    api_key: &str,
    body: &mut serde_json::Value,
) -> Result<reqwest::Response, String> {
    let response = authed_post(url, api_key)
        .json(&*body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
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
        let retry = authed_post(url, api_key)
            .json(&*body)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if retry.status().is_success() {
            return Ok(retry);
        }
        let status = retry.status();
        let text = retry.text().await.unwrap_or_default();
        return Err(format!("endpoint returned {status}: {}", error_detail(&text)));
    }
    Err(format!("endpoint returned {status}: {}", error_detail(&text)))
}

/// A human-readable reason from a failed response body: the OpenAI-style
/// `error.message` (what OpenRouter returns), else the raw body trimmed to a
/// sane length, else a placeholder.
fn error_detail(body: &str) -> String {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(body) {
        let err = value.get("error");
        if let Some(msg) = err
            .and_then(|e| e.get("message").and_then(|m| m.as_str()).or_else(|| e.as_str()))
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
/// `images` are base64 `data:` URLs attached as vision inputs. `max_notes`
/// guides the model on how many notes it may produce.
pub fn stream(
    base_url: String,
    model: String,
    api_key: String,
    prompt: String,
    images: Vec<String>,
    max_notes: usize,
    thinking: bool,
) -> impl Stream<Item = Event> {
    event_stream(64, move |mut output| async move {
        if let Err(error) =
            run(&base_url, &model, &api_key, &prompt, &images, max_notes, thinking, &mut output).await
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
    max_notes: usize,
    thinking: bool,
    output: &mut Sender<Event>,
) -> Result<(), String> {
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let mut body = serde_json::json!({
        "model": model,
        "stream": true,
        "messages": [
            { "role": "system", "content": system_prompt(max_notes) },
            { "role": "user", "content": user_content(prompt, images) },
        ],
    });
    apply_thinking(&mut body, base_url, thinking);

    let response = post_with_reasoning_retry(&url, api_key, &mut body).await?;

    // Server-sent events: `data: {json}` lines, terminated by `data: [DONE]`.
    // Buffer bytes and only decode whole lines so multi-byte characters split
    // across chunks aren't corrupted. Accumulated answer text is emitted split
    // on `---` separators.
    let mut stream = response.bytes_stream();
    let mut sse: Vec<u8> = Vec::new();
    let mut content = String::new();
    while let Some(chunk) = stream.next().await {
        sse.extend_from_slice(&chunk.map_err(|e| e.to_string())?);
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
/// separator is held back for the next chunk; on the final flush (including when
/// a stream stops mid-sentence) everything left is emitted, so an incomplete
/// `---` ends up as literal text rather than being dropped.
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
    let hold = if is_final { 0 } else { separator_prefix_len(content) };
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
    fn a_multi_line_note_stays_under_its_own_bullet() {
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
        // type a question into the note it makes, and ask.
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
        // of a disable (which would 400), excluded so it doesn't leak into notes.
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
        // and no separator text leaks into the notes.
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
        // The separator completed before the stream cut off mid-second-note.
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
        // The file note is an image input, and contributes no text.
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
        // OpenRouter: rich entries, a display name and a context window.
        let openrouter = serde_json::json!({"data": [
            {"id": "anthropic/claude-sonnet-4", "name": "Anthropic: Claude Sonnet 4",
             "context_length": 200000},
            {"id": "openai/gpt-4o", "name": "OpenAI: GPT-4o", "context_length": 128000},
        ]});
        let models = parse_models(&openrouter);
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "anthropic/claude-sonnet-4");
        assert_eq!(models[0].name.as_deref(), Some("Anthropic: Claude Sonnet 4"));
        assert_eq!(models[0].context, Some(200_000));

        // Fireworks and plain OpenAI: an id and nothing else worth showing.
        let plain = serde_json::json!({"data": [
            {"id": "accounts/fireworks/models/llama-v3p1-70b-instruct", "object": "model"},
        ]});
        let models = parse_models(&plain);
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].name, None, "no name is better than a repeated id");
        assert_eq!(models[0].context, None);

        // A bare array, which a couple of endpoints return instead of `data`.
        let bare = serde_json::json!([{"id": "local-model"}]);
        assert_eq!(parse_models(&bare)[0].id, "local-model");
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
    fn a_failure_says_what_to_do_about_it() {
        let unauthorized = describe_failure(
            reqwest::StatusCode::UNAUTHORIZED,
            r#"{"error": {"message": "No auth credentials found"}}"#,
        );
        assert!(unauthorized.contains("API key"), "{unauthorized}");
        assert!(unauthorized.contains("No auth credentials found"), "{unauthorized}");

        // A provider that simply hasn't got the route — the common case for a
        // local runtime — shouldn't read as a broken key.
        let missing = describe_failure(reqwest::StatusCode::NOT_FOUND, "<html>404</html>");
        assert!(missing.contains("no model list"), "{missing}");
    }

}
