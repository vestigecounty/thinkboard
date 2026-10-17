//! Ask-AI: streaming an answer into fresh cards branching off the asked one.
//!
//! The chain of cards feeding the clicked card becomes the prompt, upstream
//! image cards ride along as vision inputs, and the reply streams into a
//! response card (a `---` in the answer rolls over to a new sibling). Response
//! nodes stay *transient* — excluded from undo snapshots — until their stream
//! finishes, so an unrelated action never captures a half-written card.

use std::collections::{HashMap, HashSet};
use std::path::Path as FsPath;
use std::time::{Duration, Instant};

use iced::{Point, Task};

use super::{App, Message};
use crate::ai;
use crate::canvas::board;
use crate::model::{self, CanvasEdge, CanvasNode};

use super::arrange::{node_world_rect, snap_node_geometry};

/// Cadence of the pending-AI dot animation.
pub(super) const DOTS_INTERVAL: Duration = Duration::from_millis(400);
/// Minimum gap between auto-relayouts while a streaming answer grows.
const RELAYOUT_THROTTLE: Duration = Duration::from_millis(350);
/// Longest side (px) an image is downscaled to before being sent to the model.
/// Vision encoders (e.g. Gemma's SigLIP at 896²) downsize internally, so larger
/// only bloats the request.
const IMAGE_MAX_PX: u32 = 1024;

/// A live AI answer stream, keyed by its first card's id: the source it
/// branches from, the card currently receiving text (a `---` in the answer
/// rolls this over to a new sibling), how many cards it has spawned, and a
/// handle to abort the in-flight request (e.g. when a response node is deleted).
pub(super) struct AiStream {
    source: String,
    current: String,
    count: usize,
    handle: iced::task::Handle,
    /// Every node this generation has created (first card + any split
    /// siblings). They are transient — excluded from undo snapshots — until the
    /// stream finishes, so an unrelated action's snapshot doesn't capture a
    /// half-generated "…" node.
    pub(super) nodes: Vec<String>,
}

/// Everything about in-flight AI generations.
#[derive(Default)]
pub(super) struct Ai {
    /// Ids of response nodes still awaiting their first token; they show the
    /// animated dots until the answer starts streaming.
    pub(super) pending: HashSet<String>,
    /// Current dot count (1–3) for the pending animation.
    dots: usize,
    /// Last time a streaming answer triggered an auto-relayout (throttle).
    last_relayout: Option<Instant>,
    /// Live answer streams, keyed by first-card id (the `Message::Ai` tag).
    pub(super) streams: HashMap<String, AiStream>,
    /// Pending requests' input chains, each shimmered on its own clock (see
    /// [`board::AiFlow`]) so concurrent generations animate independently.
    pub(super) flows: Vec<board::AiFlow>,
    /// What the last `GET /models` found, against the endpoint it asked. Kept
    /// so reopening the model picker is instant, and refetched behind that —
    /// the vault browser's bargain, for the same reason: the panel has to be up
    /// on the keystroke that asked for it.
    ///
    /// One endpoint's worth. You configure a provider and stay on it, so a map
    /// would only ever hold the one entry and outlive the setting that made it
    /// relevant.
    pub(super) models: Option<(String, Vec<thinkboard_core::ai::Model>)>,
}

/// A configured tint (an Obsidian preset "1"–"6") as a node color value, or
/// `None` for "0"/blank — meaning "don't colorize". Used for the AI trigger and
/// response-node tints.
pub(super) fn config_color(code: &str) -> Option<String> {
    (code != "0" && !code.is_empty()).then(|| code.to_owned())
}

/// Loads the image at `path`, downscales it so its longest side is at most
/// [`IMAGE_MAX_PX`], and returns it as a base64 `data:` URL for the model.
/// `None` if the file can't be read or decoded (e.g. an unsupported format).
fn encode_image_data_url(path: &FsPath) -> Option<String> {
    use base64::Engine as _;
    let png = downscale_to_png(::image::open(path).ok()?)?;
    Some(format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&png)
    ))
}

/// PNG bytes of `img`, first shrunk so its longest side is at most
/// [`IMAGE_MAX_PX`] (aspect preserved; never upscaled).
fn downscale_to_png(img: ::image::DynamicImage) -> Option<Vec<u8>> {
    let img = if img.width().max(img.height()) > IMAGE_MAX_PX {
        img.resize(
            IMAGE_MAX_PX,
            IMAGE_MAX_PX,
            ::image::imageops::FilterType::Triangle,
        )
    } else {
        img
    };
    let mut png = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut png), ::image::ImageFormat::Png)
        .ok()?;
    Some(png)
}

/// Ids of AI streams to cancel when the nodes in `removed` are deleted: a
/// stream is cancelled if its anchor (first) card or the card it is currently
/// writing to has been removed. `streams` yields `(stream_id, current_id)`.
fn streams_to_cancel<'a>(
    streams: impl Iterator<Item = (&'a str, &'a str)>,
    removed: &HashSet<String>,
) -> Vec<String> {
    streams
        .filter(|(id, current)| removed.contains(*id) || removed.contains(*current))
        .map(|(id, _)| id.to_owned())
        .collect()
}

impl App {
    /// Asks the AI about every selected node, tinting each the trigger color.
    /// One undo point covers the whole batch.
    pub(super) fn ask_ai(&mut self) -> Task<Message> {
        if !self.config.ai_enabled() {
            return Task::none();
        }
        let targets: Vec<usize> = self
            .selected
            .iter()
            .copied()
            .filter(|&i| i < self.nodes.len())
            .collect();
        self.close_context_menu();
        self.selected.clear();
        if targets.is_empty() {
            return Task::none();
        }
        // One undo point per generation, captured before the trigger tint and
        // the response nodes — recorded even when re-asking an already-tinted
        // node, so each generation undoes on its own instead of being lumped in
        // with a previous one.
        self.push_undo();
        // Tint the clicked node(s) the trigger color (unless "0"), the same
        // feedback the color trigger gives.
        let trigger = config_color(&self.config.ai_trigger_color);
        for &i in &targets {
            self.nodes[i].color = trigger.clone();
        }
        self.mark_dirty();
        let tasks: Vec<Task<Message>> = targets.iter().map(|&i| self.trigger_ai(i)).collect();
        self.cache.clear();
        Task::batch(tasks)
    }

    /// Asks the AI with the chain of cards feeding `source`, spawning a response
    /// node branching to its right and streaming the answer into it. A `---` in
    /// the answer starts a new sibling card (up to `config.max_ai_cards`).
    fn trigger_ai(&mut self, source: usize) -> Task<Message> {
        if !self.config.ai_enabled() {
            return Task::none();
        }
        let Some(source_node) = self.nodes.get(source) else {
            return Task::none();
        };
        let source_id = source_node.id.clone();
        let is_group = source_node.is_group();

        // A group is a region, not a step in a chain: it has no incoming links
        // to walk, so its contents become the prompt instead — flattened into an
        // outline that keeps the link and nesting hierarchy the user sees.
        let (prompt, image_files, inputs) = if is_group {
            let prompt = ai::group_prompt(&self.nodes, &self.edges, source);
            let files = ai::group_images(&self.nodes, source);
            // The shimmer sweeps what was actually read: everything inside.
            let inputs: HashSet<String> = self
                .enclosed_by(source)
                .into_iter()
                .map(|i| self.nodes[i].id.clone())
                .collect();
            (prompt, files, inputs)
        } else {
            let prompt = ai::upstream_prompt(&self.nodes, &self.edges, &source_id);
            let files = ai::upstream_images(&self.nodes, &self.edges, &source_id);
            // The ancestor cards, which include the clicked node.
            let inputs: HashSet<String> = ai::upstream_nodes(&self.nodes, &self.edges, &source_id)
                .iter()
                .map(|node| node.id.clone())
                .collect();
            (prompt, files, inputs)
        };

        // Attach any image cards among the inputs as vision inputs (downscaled).
        let images: Vec<String> = image_files
            .iter()
            .filter_map(|file| encode_image_data_url(&self.base_path.as_path().join(file)))
            .collect();
        if prompt.trim().is_empty() && images.is_empty() {
            return Task::none();
        }

        // The shimmer sweeps the inputs once, ending on the clicked node — the
        // response edge is deliberately left out so the sweep settles on the
        // input, not the (dotted) answer node.
        let ancestors = inputs;
        let flow: Vec<String> = self
            .edges
            .iter()
            .filter(|e| ancestors.contains(&e.from_node) && ancestors.contains(&e.to_node))
            .map(|e| e.id.clone())
            .collect();

        // The first response card starts as animated dots.
        let Some(node_id) = self.spawn_ai_node(&source_id, ".") else {
            return Task::none();
        };
        self.ai.pending.insert(node_id.clone());
        self.ai.flows.push(board::AiFlow {
            nodes: ancestors,
            edges: flow.into_iter().collect(),
            target: node_id.clone(),
            since: Instant::now(),
        });

        let (url, model, key, max_nodes) = (
            self.config.ai_url.clone(),
            self.config.ai_model.clone(),
            self.config.ai_key().to_owned(),
            self.config.max_ai_cards,
        );
        let thinking = self.config.ai_thinking;
        let stream_id = node_id.clone();
        // Abortable so deleting the response node cancels the request.
        let (task, handle) = Task::run(
            ai::stream(url, model, key, prompt, images, max_nodes, thinking),
            move |event| Message::Ai(stream_id.clone(), event),
        )
        .abortable();
        self.ai.streams.insert(
            node_id.clone(),
            AiStream {
                source: source_id,
                current: node_id.clone(),
                count: 1,
                handle,
                nodes: vec![node_id],
            },
        );
        task
    }

    /// Creates a tinted response node branching off `source_id` (an edge from
    /// its right port), spawning-animated, with `initial` text. Returns the new
    /// node's id, or `None` if the source has vanished.
    /// Where a fresh answer node goes for the question at `source`: beside it,
    /// nudged clear of anything already there.
    ///
    /// A frame is kept clear too, which no other placement does. Frames are
    /// deliberately not obstacles — a new node is usually *meant* to be able to
    /// land inside a group — but an answer about a frame is the one case where
    /// that is wrong: inside, it becomes one of the frame's members and is fed
    /// back into the next question asked about it.
    pub(super) fn ai_answer_placement(&self, source: usize, width: f32, height: f32) -> Point {
        let Some(node) = self.nodes.get(source) else {
            return Point::ORIGIN;
        };
        let rect = node_world_rect(node);
        let avoid: Vec<iced::Rectangle> = if node.is_group() { vec![rect] } else { Vec::new() };
        let requested = Point::new(
            rect.x + rect.width + width / 2.0 + 60.0,
            rect.center_y(),
        );
        self.find_free_placement_avoiding(requested, width, height, &avoid)
    }

    fn spawn_ai_node(&mut self, source_id: &str, initial: &str) -> Option<String> {
        let source = self.nodes.iter().position(|n| n.id == source_id)?;
        let (width, height) = (self.config.new_card_min_width, self.config.new_card_min_height);
        let center = self.ai_answer_placement(source, width, height);
        let node_id = model::random_id();
        let mut node = CanvasNode {
            id: node_id.clone(),
            kind: "text".to_owned(),
            text: Some(initial.to_owned()),
            file: None,
            x: center.x - width / 2.0,
            y: center.y - height / 2.0,
            width,
            height,
            color: config_color(&self.config.ai_card_color),
            extra: Default::default(),
        };
        snap_node_geometry(&mut node, self.snap_grid());
        self.nodes.push(node);
        let new_index = self.nodes.len() - 1;
        self.spawning.insert(new_index, Instant::now());

        let to_side = board::opposite_side("right");
        self.edges.push(CanvasEdge {
            id: model::random_id(),
            from_node: source_id.to_owned(),
            from_side: "right".to_owned(),
            to_node: node_id.clone(),
            to_side: to_side.to_owned(),
            extra: Default::default(),
        });
        self.mark_dirty();
        self.cache.clear();
        Some(node_id)
    }

    /// Routes one streamed event to the generation tagged `stream_id`.
    pub(super) fn ai_event(&mut self, stream_id: String, event: ai::Event) -> Task<Message> {
        match event {
            ai::Event::Delta(text) => self.ai_delta(&stream_id, text),
            ai::Event::Split => self.ai_split(&stream_id),
            ai::Event::Done => self.ai_done(&stream_id),
            ai::Event::Error(error) => self.ai_error(&stream_id, error),
        }
    }

    /// A chunk of answer text: appended to the card currently receiving it.
    fn ai_delta(&mut self, stream_id: &str, text: String) -> Task<Message> {
        let Some(target) = self.ai.streams.get(stream_id).map(|s| s.current.clone()) else {
            return Task::none();
        };
        let Some(index) = self.nodes.iter().position(|n| n.id == target) else {
            return Task::none();
        };
        // The first chunk replaces the animated dots; append after.
        if self.end_ai_pending(&target) {
            self.nodes[index].text = Some(text);
        } else {
            let current = self.nodes[index].text.take().unwrap_or_default();
            self.nodes[index].text = Some(current + &text);
        }
        self.autosize_node(index, true);
        // The text/size just changed; drop the stale bitmap so it draws live
        // until the fresh raster catches up.
        self.invalidate_snapshot(index);
        // Follow the growing card with a throttled relayout so filling cards
        // don't overlap — always, regardless of the auto-layout setting. Tidies
        // only this generation's cards, never the user's surrounding nodes.
        let due = self
            .ai
            .last_relayout
            .is_none_or(|last| last.elapsed() >= RELAYOUT_THROTTLE);
        if due {
            self.ai.last_relayout = Some(Instant::now());
            if let Some((source, ids)) = self
                .ai
                .streams
                .get(stream_id)
                .map(|s| (s.source.clone(), s.nodes.clone()))
            {
                self.relayout_ai_siblings(&source, &ids);
            }
        }
        self.mark_dirty();
        self.cache.clear();
        // Draw live while streaming (each Delta redraws); the final raster is
        // taken once at Done, like a resize.
        Task::none()
    }

    /// A `---` in the answer: start a new sibling card off the same source,
    /// unless the card cap is reached — then keep appending to the current one.
    fn ai_split(&mut self, stream_id: &str) -> Task<Message> {
        let Some(stream) = self.ai.streams.get(stream_id) else {
            return Task::none();
        };
        if stream.count >= self.config.max_ai_cards {
            return Task::none();
        }
        let source = stream.source.clone();
        if let Some(new_id) = self.spawn_ai_node(&source, "")
            && let Some(stream) = self.ai.streams.get_mut(stream_id)
        {
            stream.current = new_id.clone();
            stream.count += 1;
            stream.nodes.push(new_id);
        }
        self.refresh_snapshots()
    }

    fn ai_done(&mut self, stream_id: &str) -> Task<Message> {
        let Some(stream) = self.ai.streams.remove(stream_id) else {
            return Task::none();
        };
        // A still-pending first card means no text ever arrived.
        if self.end_ai_pending(stream_id)
            && let Some(index) = self.nodes.iter().position(|n| n.id == stream.current)
        {
            self.nodes[index].text = Some("(no response)".to_owned());
            self.autosize_node(index, true);
            self.invalidate_snapshot(index);
        }
        // Settle the layout at the final size, tidying only this generation's
        // cards (the source and other nodes stay put).
        self.relayout_ai_siblings(&stream.source, &stream.nodes);
        Task::batch([self.refresh_snapshots(), self.schedule_recluster()])
    }

    fn ai_error(&mut self, stream_id: &str, error: String) -> Task<Message> {
        // Still pending, the card holds only the dots; otherwise it holds the
        // answer as far as it got, which the failure is added to rather than
        // written over.
        let pending = self.end_ai_pending(stream_id);
        if let Some(stream) = self.ai.streams.remove(stream_id)
            && let Some(index) = self.nodes.iter().position(|n| n.id == stream.current)
        {
            let written = self.nodes[index].text.take().filter(|_| !pending).unwrap_or_default();
            let gap = if written.trim().is_empty() { "" } else { "\n\n" };
            self.nodes[index].text = Some(format!("{}{gap}⚠ {error}", written.trim_end()));
            self.autosize_node(index, true);
            self.invalidate_snapshot(index);
        }
        self.mark_dirty();
        self.cache.clear();
        self.refresh_snapshots()
    }

    /// Advances the "…" animation on every card still awaiting its first token.
    pub(super) fn tick_ai_dots(&mut self) {
        if self.ai.pending.is_empty() {
            return;
        }
        self.ai.dots = self.ai.dots % 3 + 1;
        let dots = ".".repeat(self.ai.dots);
        let pending: Vec<usize> = self
            .ai
            .pending
            .iter()
            .filter_map(|id| self.nodes.iter().position(|n| &n.id == id))
            .collect();
        for index in pending {
            self.nodes[index].text = Some(dots.clone());
            // Drawn live: without invalidating, the dots freeze at the first
            // rendered frame's bitmap.
            self.invalidate_snapshot(index);
        }
        self.cache.clear();
    }

    /// Marks the AI node `id` no longer pending (its first token arrived, or it
    /// finished/failed/was deleted). Once nothing is pending, the input-flow
    /// animation ends. Returns whether `id` was pending.
    fn end_ai_pending(&mut self, id: &str) -> bool {
        let was_pending = self.ai.pending.remove(id);
        // Drop this request's shimmer once its answer starts (or it ends).
        self.ai.flows.retain(|flow| flow.target != id);
        was_pending
    }

    /// Aborts and forgets any AI stream whose response node is among `removed`,
    /// so deleting a response node (even mid-generation) cancels its request
    /// instead of leaving it running and writing into a node that's gone.
    pub(super) fn cancel_ai_streams(&mut self, removed: &HashSet<String>) {
        let cancel = streams_to_cancel(
            self.ai.streams.iter().map(|(id, s)| (id.as_str(), s.current.as_str())),
            removed,
        );
        for id in cancel {
            if let Some(stream) = self.ai.streams.remove(&id) {
                stream.handle.abort();
            }
            self.end_ai_pending(&id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn removed(ids: &[&str]) -> HashSet<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn deleting_the_streaming_card_cancels_its_stream() {
        // Stream "a" is currently writing into split sibling "a2"; deleting "a2"
        // cancels it. Stream "b" is untouched.
        let streams = [("a", "a2"), ("b", "b")];
        assert_eq!(
            streams_to_cancel(streams.into_iter(), &removed(&["a2"])),
            vec!["a".to_owned()]
        );
    }

    #[test]
    fn deleting_the_anchor_card_cancels_its_stream() {
        let streams = [("a", "a3")];
        assert_eq!(
            streams_to_cancel(streams.into_iter(), &removed(&["a"])),
            vec!["a".to_owned()]
        );
    }

    #[test]
    fn deleting_an_unrelated_card_cancels_nothing() {
        let streams = [("a", "a2"), ("b", "b")];
        assert!(streams_to_cancel(streams.into_iter(), &removed(&["zzz"])).is_empty());
    }

    #[test]
    fn config_color_treats_zero_and_blank_as_no_tint() {
        assert_eq!(config_color("0"), None);
        assert_eq!(config_color(""), None);
        assert_eq!(config_color("3"), Some("3".to_owned()));
        assert_eq!(config_color("6"), Some("6".to_owned()));
    }

    #[test]
    fn downscale_caps_the_longest_side_preserving_aspect() {
        let big = ::image::DynamicImage::new_rgb8(2000, 1000);
        let png = downscale_to_png(big).unwrap();
        let decoded = ::image::load_from_memory(&png).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (IMAGE_MAX_PX, IMAGE_MAX_PX / 2));
    }

    #[test]
    fn downscale_never_upscales_a_small_image() {
        let small = ::image::DynamicImage::new_rgb8(300, 200);
        let png = downscale_to_png(small).unwrap();
        let decoded = ::image::load_from_memory(&png).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (300, 200));
    }
}
