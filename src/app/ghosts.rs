//! Copilot-style suggestion cards.
//!
//! Finishing a freshly created card that hangs off a parent asks the model for
//! sibling ideas; they appear as translucent "ghost" cards the user can accept,
//! drag, or ignore. Ignored ones fade out and auto-dismiss, so a suggestion
//! that missed never has to be dealt with. Engaging any of them (accepting,
//! moving, dismissing one) pins the batch and cancels the countdown.
//!
//! **Shelved.** It was an experiment that never earned its place: nothing in
//! the UI offers it any more — no menu item, setting or palette command — and
//! [`OFFERED`] keeps an old config's `ai_completions: true` from switching it
//! on behind the user's back. The feature itself is kept whole, drawing and
//! all, so bringing it back is the switch and a way to turn it on.

use std::time::{Duration, Instant};

use iced::{Point, Rectangle, Size, Task};

use super::arrange::{node_world_rect, snap_node_geometry};
use super::{App, Message};
use crate::ai;
use crate::canvas::board;
use crate::model::{self, CanvasEdge, CanvasNode};

/// Whether suggestions are offered at all — see the module docs.
const OFFERED: bool = false;

/// How long ignored ghost suggestions linger before auto-dismissing.
const TTL: Duration = Duration::from_secs(10);
/// When (into the TTL) ghosts start fading out to telegraph the dismissal.
const FADE_START: f32 = 3.0;
/// Duration (s) of a ghost card's fade-in when it appears.
pub(super) const FADE_IN: f32 = 0.18;

/// The pending suggestion cards and their clocks.
pub(super) struct Ghosts {
    /// The cards awaiting accept/dismiss (including a "…" placeholder while
    /// suggestions are being fetched).
    pub(super) cards: Vec<board::Ghost>,
    /// When the current batch appeared, for the auto-dismiss timeout (reset by
    /// hover).
    since: Instant,
    /// When the current cards became visible, for the fade-in (not reset by
    /// hover, so hovering doesn't retrigger it).
    appeared: Instant,
    /// Set once the user engages a ghost (moves/accepts/dismisses one), which
    /// cancels the auto-dismiss so they stick around while being used.
    pinned: bool,
    /// Whether the cursor is over a ghost card: pauses the fade/timeout, and
    /// leaving refreshes it to the full TTL.
    pub(super) hovered: bool,
    /// Bumped on each completion request and whenever ghosts are dismissed, so
    /// a suggestion result that arrives after a dismissal (or is superseded) is
    /// ignored instead of popping back up.
    generation: u64,
}

impl Default for Ghosts {
    fn default() -> Self {
        Self {
            cards: Vec::new(),
            since: Instant::now(),
            appeared: Instant::now(),
            pinned: false,
            hovered: false,
            generation: 0,
        }
    }
}

impl Ghosts {
    /// Starts the clocks for a freshly shown batch: un-pinned, fading in.
    fn restart(&mut self) {
        self.since = Instant::now();
        self.appeared = Instant::now();
        self.pinned = false;
    }
}

impl App {
    /// Whether real suggestion ghosts are counting down to auto-dismiss: shown,
    /// not pinned by the user, and not the loading placeholder (which just
    /// waits for its result).
    pub(super) fn ghosts_expiring(&self) -> bool {
        !self.ghosts.cards.is_empty()
            && !self.ghosts.pinned
            && !self.ghosts.hovered
            && !self.ghosts.cards.iter().any(|g| g.placeholder)
    }

    /// Whether a freshly-appeared batch is still fading in.
    pub(super) fn ghosts_fading_in(&self) -> bool {
        !self.ghosts.cards.is_empty() && self.ghosts.appeared.elapsed().as_secs_f32() < FADE_IN
    }

    /// Whether the expiry countdown has run out.
    pub(super) fn ghosts_expired(&self) -> bool {
        self.ghosts_expiring() && self.ghosts.since.elapsed() >= TTL
    }

    /// Fade-in factor (0→1) for freshly-appeared ghost cards.
    pub(super) fn ghost_fade_in(&self) -> f32 {
        (self.ghosts.appeared.elapsed().as_secs_f32() / FADE_IN).clamp(0.0, 1.0)
    }

    /// Opacity for the expiring ghosts: full until [`FADE_START`], then fading
    /// to 0 by the end of [`TTL`] — the visible countdown.
    pub(super) fn ghost_fade(&self) -> f32 {
        if !self.ghosts_expiring() {
            return 1.0;
        }
        let t = self.ghosts.since.elapsed().as_secs_f32();
        let ttl = TTL.as_secs_f32();
        if t <= FADE_START {
            1.0
        } else {
            (1.0 - (t - FADE_START) / (ttl - FADE_START)).clamp(0.0, 1.0)
        }
    }

    /// Clears any pending ghost suggestions and invalidates an in-flight
    /// completion request so its result won't pop back up after dismissal.
    pub(super) fn dismiss_ghosts(&mut self) {
        if !self.ghosts.cards.is_empty() {
            self.ghosts.cards.clear();
            self.ghosts.hovered = false;
            self.ghosts.generation = self.ghosts.generation.wrapping_add(1);
            self.cache.clear();
        }
    }

    /// Discards one card. Dismissing the "…" placeholder also cancels its
    /// in-flight request, so the result won't arrive as ghosts later.
    pub(super) fn dismiss_ghost(&mut self, index: usize) {
        if index >= self.ghosts.cards.len() {
            return;
        }
        if self.ghosts.cards[index].placeholder {
            self.ghosts.generation = self.ghosts.generation.wrapping_add(1);
        }
        self.ghosts.cards.remove(index);
        self.ghosts.pinned = true;
        self.cache.clear();
    }

    /// Repositions a card without accepting it; arranging them cancels the
    /// timeout.
    pub(super) fn move_ghost(&mut self, index: usize, position: Point) {
        if let Some(ghost) = self.ghosts.cards.get_mut(index) {
            ghost.rect.x = position.x;
            ghost.rect.y = position.y;
            self.ghosts.pinned = true;
            self.cache.clear();
        }
    }

    /// The cursor moved on/off the cards. Pause the countdown while hovered,
    /// and refresh to the full TTL on enter/leave so a fading card comes back
    /// to full and restarts.
    pub(super) fn hover_ghosts(&mut self, over: bool) {
        self.ghosts.hovered = over;
        self.ghosts.since = Instant::now();
    }

    /// Whether a card just finished being created should get suggestions: the
    /// feature is offered and on, an endpoint is configured, and the card has a
    /// parent.
    pub(super) fn completion_anchor(&self, index: usize) -> Option<String> {
        if !OFFERED || !self.config.ai_completions || !self.config.ai_enabled() {
            return None;
        }
        if self.fresh_node != Some(index) {
            return None;
        }
        let id = self.nodes.get(index)?.id.clone();
        self.edges.iter().any(|e| e.to_node == id).then_some(id)
    }

    /// Bumps the generation and shows the loading placeholder, returning the
    /// task that fetches the suggestions.
    pub(super) fn begin_completions(&mut self, anchor: String) -> Task<Message> {
        self.ghosts.generation = self.ghosts.generation.wrapping_add(1);
        let generation = self.ghosts.generation;
        self.spawn_placeholder_ghost(&anchor);
        self.request_completions(anchor, generation)
    }

    /// Requests Copilot-style sibling suggestions for `anchor` (a node with a
    /// parent) in the background; results arrive as `Message::Suggestions`
    /// tagged with `generation` so a stale (dismissed/superseded) result is
    /// dropped.
    fn request_completions(&self, anchor: String, generation: u64) -> Task<Message> {
        let Some(parent) = self
            .edges
            .iter()
            .find(|e| e.to_node == anchor)
            .map(|e| e.from_node.clone())
        else {
            return Task::none();
        };
        let context = ai::upstream_prompt(&self.nodes, &self.edges, &anchor);
        if context.trim().is_empty() {
            return Task::none();
        }
        // The parent's other children, so suggestions stay on theme and don't
        // repeat cards that already exist.
        let existing: Vec<String> = self
            .edges
            .iter()
            .filter(|e| e.from_node == parent && e.to_node != anchor)
            .filter_map(|e| self.nodes.iter().find(|n| n.id == e.to_node))
            .filter_map(|n| n.text.as_deref())
            .map(|t| t.trim().to_owned())
            .filter(|t| !t.is_empty())
            .collect();
        let (url, model, key) = (
            self.config.ai_url.clone(),
            self.config.ai_model.clone(),
            self.config.ai_key().to_owned(),
        );
        let thinking = self.config.ai_thinking;
        Task::perform(
            async move {
                ai::suggest(url, model, key, context, existing, 3, thinking).await.unwrap_or_default()
            },
            move |texts| Message::Suggestions {
                generation,
                parent: parent.clone(),
                anchor: anchor.clone(),
                texts,
            },
        )
    }

    /// Shows a "…" placeholder ghost below `anchor` while its suggestions are
    /// fetched — the loading cue, in the ghosts' own visual language.
    fn spawn_placeholder_ghost(&mut self, anchor: &str) {
        let Some(node) = self.nodes.iter().find(|n| n.id == anchor) else {
            return;
        };
        let Some(parent) = self
            .edges
            .iter()
            .find(|e| e.to_node == anchor)
            .map(|e| e.from_node.clone())
        else {
            return;
        };
        let rect = node_world_rect(node);
        let (w, h) = (self.config.new_card_min_width, self.config.new_card_min_height);
        let requested = Point::new(rect.center_x(), rect.y + rect.height + 30.0 + h / 2.0);
        let center = self.find_free_placement(requested, w, h);
        self.ghosts.cards.clear();
        self.ghosts.cards.push(board::Ghost {
            text: "…".to_owned(),
            rect: Rectangle::new(Point::new(center.x - w / 2.0, center.y - h / 2.0), Size::new(w, h)),
            parent,
            placeholder: true,
        });
        self.ghosts.restart();
        self.ghosts.hovered = false;
        self.cache.clear();
    }

    /// Lays a batch of arrived suggestions out as cards, anchored to the "…"
    /// placeholder's (possibly dragged) spot. A result from a superseded or
    /// dismissed request is dropped.
    pub(super) fn show_suggestions(
        &mut self,
        generation: u64,
        parent: String,
        anchor: String,
        texts: Vec<String>,
    ) {
        if generation != self.ghosts.generation {
            return;
        }
        let (w, h) = (self.config.new_card_min_width, self.config.new_card_min_height);
        // Anchor the batch to the placeholder's spot, falling back to just
        // below the added node.
        let start = self
            .ghosts
            .cards
            .iter()
            .find(|g| g.placeholder)
            .map(|g| g.rect.position())
            .or_else(|| {
                self.nodes.iter().find(|n| n.id == anchor).map(|n| {
                    let r = node_world_rect(n);
                    Point::new(r.x, r.y + r.height + 30.0)
                })
            });
        let Some(start) = start else {
            self.dismiss_ghosts();
            return;
        };
        self.ghosts.cards.clear();
        // Lay the suggestions out from `start`, each nudged clear of existing
        // nodes and the sibling ghosts already placed.
        let mut placed: Vec<Rectangle> = Vec::new();
        let mut top = start.y;
        for text in texts {
            let requested = Point::new(start.x + w / 2.0, top + h / 2.0);
            let center = self.find_free_placement_avoiding(requested, w, h, &placed);
            let ghost_rect =
                Rectangle::new(Point::new(center.x - w / 2.0, center.y - h / 2.0), Size::new(w, h));
            placed.push(ghost_rect);
            self.ghosts.cards.push(board::Ghost {
                text,
                rect: ghost_rect,
                parent: parent.clone(),
                placeholder: false,
            });
            top = ghost_rect.y + ghost_rect.height + 20.0;
        }
        self.ghosts.restart();
        self.cache.clear();
    }

    /// Turns the card at `index` into a real node, keeping the rest around.
    pub(super) fn accept_ghost_at(&mut self, index: usize) -> Task<Message> {
        if index >= self.ghosts.cards.len() {
            return Task::none();
        }
        self.ghosts.pinned = true; // engaged: keep the rest around
        let ghost = self.ghosts.cards.remove(index);
        self.accept_ghost(ghost)
    }

    /// Turns a ghost suggestion into a real node branching off its parent,
    /// keeping the ghost's placement. Undoable.
    fn accept_ghost(&mut self, ghost: board::Ghost) -> Task<Message> {
        self.push_undo();
        let node_id = model::random_id();
        let mut node = CanvasNode {
            id: node_id.clone(),
            kind: "text".to_owned(),
            text: Some(ghost.text),
            file: None,
            x: ghost.rect.x,
            y: ghost.rect.y,
            width: ghost.rect.width,
            height: ghost.rect.height,
            color: None,
            extra: Default::default(),
        };
        snap_node_geometry(&mut node, self.snap_grid());
        self.nodes.push(node);
        let index = self.nodes.len() - 1;
        self.spawning.insert(index, Instant::now());

        // Only need to know the parent still exists; keep the ghost's placement.
        if self.nodes.iter().any(|n| n.id == ghost.parent) {
            self.edges.push(CanvasEdge {
                id: model::random_id(),
                from_node: ghost.parent,
                from_side: "right".to_owned(),
                to_node: node_id,
                to_side: board::opposite_side("right").to_owned(),
                extra: Default::default(),
            });
        }
        self.autosize_node(index, true);
        self.auto_relayout(index, false);
        self.mark_dirty();
        self.cache.clear();
        Task::batch([self.refresh_snapshots(), self.schedule_recluster()])
    }
}
