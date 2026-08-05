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
pub fn upstream_prompt(nodes: &[CanvasNode], edges: &[CanvasEdge], target_id: &str) -> String {
    upstream_nodes(nodes, edges, target_id)
        .iter()
        .filter_map(|node| node.text.as_deref())
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// The `file` paths of image (file) notes among `target`'s ancestors, in the
/// same order, to attach as vision inputs alongside the text prompt.
pub fn upstream_images(nodes: &[CanvasNode], edges: &[CanvasEdge], target_id: &str) -> Vec<String> {
    upstream_nodes(nodes, edges, target_id)
        .iter()
        // Images only: a `file` node can also embed a Markdown note or a PDF,
        // neither of which a vision encoder can take.
        .filter(|node| node.node_kind() == NodeKind::Image)
        .filter_map(|node| node.file.clone())
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

    fn node(id: &str, text: &str) -> CanvasNode {
        CanvasNode {
            id: id.to_owned(),
            kind: "text".to_owned(),
            text: Some(text.to_owned()),
            file: None,
            x: 0.0,
            y: 0.0,
            width: 10.0,
            height: 10.0,
            color: None,
            extra: Default::default(),
        }
    }

    fn edge(from: &str, to: &str) -> CanvasEdge {
        CanvasEdge {
            id: format!("{from}{to}"),
            from_node: from.to_owned(),
            from_side: "right".to_owned(),
            to_node: to.to_owned(),
            to_side: "left".to_owned(),
            extra: Default::default(),
        }
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
}
