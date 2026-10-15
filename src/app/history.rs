//! Document undo/redo, and the deletions that record it.
//!
//! History is snapshot-based: an action records the whole node/edge set before
//! it mutates anything (the document is small enough that this beats tracking
//! per-field diffs). Drags deliberately record nothing — they would push a step
//! per mouse-move — and in-flight AI generations are excluded until they
//! finish, so an unrelated action's snapshot never captures a half-written note.

use std::collections::{HashMap, HashSet};

use iced::Task;

use super::{App, Message};
use crate::canvas::text_raster;
use crate::model::{self, CanvasEdge, CanvasNode};

/// Cap on undo history depth.
pub(super) const MAX_UNDO: usize = 100;

/// A document state for undo/redo.
pub(super) struct DocSnapshot {
    pub(super) nodes: Vec<CanvasNode>,
    pub(super) edges: Vec<CanvasEdge>,
}

/// Whether `edge` touches a transient node (so it's transient too).
pub(super) fn is_transient_edge(edge: &CanvasEdge, transient: &HashSet<String>) -> bool {
    transient.contains(&edge.from_node) || transient.contains(&edge.to_node)
}

/// A document snapshot with the transient AI nodes (and the edges touching
/// them) left out — in-flight generations don't belong in history.
fn committed_snapshot(
    nodes: &[CanvasNode],
    edges: &[CanvasEdge],
    transient: &HashSet<String>,
) -> DocSnapshot {
    DocSnapshot {
        nodes: nodes.iter().filter(|n| !transient.contains(&n.id)).cloned().collect(),
        edges: edges.iter().filter(|e| !is_transient_edge(e, transient)).cloned().collect(),
    }
}

/// Whether a text bitmap rendered for `(old_text, old_size)` still depicts a
/// node now showing `(new_text, new_size)`. A text node's bitmap is a function
/// of its text and size only — zoom and theme are global, and the node's color
/// tint is drawn separately from the bitmap — so unchanged text and size means
/// the already-uploaded texture can be reused across an undo/redo instead of
/// re-rasterizing (which blanks the node for a frame while the new texture
/// uploads). Size is compared with the same 0.5px slack as `autosize_node`.
fn text_bitmap_reusable(
    old_text: &Option<String>,
    old_size: (f32, f32),
    new_text: &Option<String>,
    new_size: (f32, f32),
) -> bool {
    old_text == new_text
        && (old_size.0 - new_size.0).abs() < 0.5
        && (old_size.1 - new_size.1).abs() < 0.5
}

/// Drops `removed` from an index-keyed map and shifts higher keys down by
/// one, matching a `Vec::remove(removed)` on the nodes.
pub(super) fn shift_indices<V>(map: &mut HashMap<usize, V>, removed: usize) {
    *map = map
        .drain()
        .filter(|(k, _)| *k != removed)
        .map(|(k, v)| (if k > removed { k - 1 } else { k }, v))
        .collect();
}

/// Re-keys an index-keyed map after the nodes were reordered, where
/// `new_of_old[i]` is where the node at `i` went.
pub(super) fn remap_indices<V>(map: &mut HashMap<usize, V>, new_of_old: &[usize]) {
    *map = map.drain().filter_map(|(k, v)| Some((*new_of_old.get(k)?, v))).collect();
}

impl App {
    /// Node ids belonging to in-flight AI generations, which are transient and
    /// excluded from undo history until they finish.
    fn transient_node_ids(&self) -> HashSet<String> {
        self.ai
            .streams
            .values()
            .flat_map(|stream| stream.nodes.iter().cloned())
            .collect()
    }

    pub(super) fn snapshot(&self) -> DocSnapshot {
        committed_snapshot(&self.nodes, &self.edges, &self.transient_node_ids())
    }

    /// Records the current document as an undo point and clears the redo
    /// history (a new action diverges from any undone future).
    pub(super) fn push_undo(&mut self) {
        self.history.record(self.snapshot());
    }

    /// Replaces the document with a snapshot (used by undo/redo), rebuilding
    /// images and transient caches since node indices change.
    fn restore(&mut self, snapshot: DocSnapshot) -> Task<Message> {
        self.editing = None;
        self.layout_anim = None;
        // In-flight AI nodes aren't in snapshots; carry the live ones across so
        // undo/redo of an unrelated action doesn't drop a streaming node.
        let transient = self.transient_node_ids();
        let mut nodes = snapshot.nodes;
        nodes.extend(
            self.nodes
                .iter()
                .filter(|n| transient.contains(&n.id))
                .cloned(),
        );
        let mut edges = snapshot.edges;
        edges.extend(self.edges.iter().filter(|e| is_transient_edge(e, &transient)).cloned());
        // Text bitmaps we can carry across the restore, keyed by node id with
        // the text + size they depict. Nodes an undo/redo doesn't touch keep
        // their already-uploaded texture instead of re-rasterizing and blanking
        // for a frame (see text_bitmap_reusable). Captured before self.nodes is
        // rebuilt, since the raster map is keyed by the old indices.
        let carried: HashMap<String, (text_raster::Snapshot, Option<String>, f32, f32)> = self
            .nodes
            .iter()
            .enumerate()
            .filter_map(|(index, node)| {
                let snapshot = self.rasters.text.get(&index)?;
                Some((
                    node.id.clone(),
                    (snapshot.clone(), node.text.clone(), node.width, node.height),
                ))
            })
            .collect();

        self.nodes = nodes;
        self.edges = edges;
        self.images = self.load_images(&self.nodes);
        self.notes = self.load_notes(&self.nodes);
        // Re-key the carried bitmaps onto the restored nodes, keeping one only
        // where the node's text and size still match what it depicts.
        self.rasters.text = self
            .nodes
            .iter()
            .enumerate()
            .filter_map(|(index, node)| {
                let (snapshot, text, width, height) = carried.get(&node.id)?;
                text_bitmap_reusable(text, (*width, *height), &node.text, (node.width, node.height))
                    .then(|| (index, snapshot.clone()))
            })
            .collect();
        self.rasters.staging.clear();
        self.spawning.clear();
        self.pending_edit = None;
        self.fresh_node = None;
        self.selected.clear();
        self.selected_edge = None;
        self.hovered_edge = None;
        self.mark_dirty();
        self.cache.clear();
        Task::batch([self.refresh_snapshots(), self.schedule_recluster()])
    }

    pub(super) fn undo(&mut self) -> Task<Message> {
        if !self.history.can_undo() {
            return Task::none();
        }
        let previous = self.history.undo(self.snapshot()).expect("can_undo");
        self.restore(previous)
    }

    pub(super) fn redo(&mut self) -> Task<Message> {
        if !self.history.can_redo() {
            return Task::none();
        }
        let next = self.history.redo(self.snapshot()).expect("can_redo");
        self.restore(next)
    }

    /// Deletes the selected edge (undoable).
    fn delete_selected_edge(&mut self) -> Task<Message> {
        let Some(index) = self.selected_edge.take() else {
            return Task::none();
        };
        self.hovered_edge = None;
        if self.editing.is_some() {
            return Task::none();
        }
        // Snapshot first, but only commit it to the undo history if a link is
        // actually removed — a stale index (list shrank under the selection)
        // must not leave a phantom undo entry.
        let snapshot = self.snapshot();
        // Remember an endpoint so its (now possibly smaller) graph can settle.
        let endpoint = self.edges.get(index).map(|e| e.from_node.clone());
        if !model::remove_edge(&mut self.edges, index) {
            return Task::none();
        }
        self.history.record(snapshot);
        // Removing a link may split a graph; re-lay-out if enabled (covered by
        // the undo snapshot just recorded).
        if let Some(id) = endpoint
            && let Some(i) = self.nodes.iter().position(|n| n.id == id)
        {
            self.auto_relayout(i, false);
        }
        self.mark_dirty();
        self.cache.clear();
        self.schedule_recluster()
    }

    /// Deletes the selected nodes and their edges, remapping index-keyed state
    /// so the caches for other nodes survive. Falls through to the selected
    /// link when no node is selected.
    pub(super) fn delete_selected(&mut self) -> Task<Message> {
        if self.selected.is_empty() {
            return self.delete_selected_edge();
        }

        // Live indices to remove, high to low, so each removal leaves the
        // lower indices (and their index-keyed bitmaps) valid.
        let mut indices: Vec<usize> = self
            .selected
            .iter()
            .copied()
            .filter(|&i| i < self.nodes.len())
            .collect();
        indices.sort_unstable();
        indices.dedup();
        if indices.is_empty() || self.editing.is_some() {
            self.selected.clear();
            self.selected_edge = None;
            self.hovered_edge = None;
            return Task::none();
        }

        self.push_undo();
        let removed_ids: HashSet<String> =
            indices.iter().map(|&i| self.nodes[i].id.clone()).collect();
        // Cancel any in-flight AI request whose response node is being deleted.
        self.cancel_ai_streams(&removed_ids);
        for &index in indices.iter().rev() {
            self.nodes.remove(index);
            shift_indices(&mut self.rasters.text, index);
            shift_indices(&mut self.rasters.staging, index);
            shift_indices(&mut self.spawning, index);
        }
        self.edges.retain(|edge| {
            !removed_ids.contains(&edge.from_node) && !removed_ids.contains(&edge.to_node)
        });
        self.pending_edit = None;
        self.fresh_node = None;
        self.selected.clear();
        self.selected_edge = None;
        self.hovered_edge = None;

        self.mark_dirty();
        self.cache.clear();
        Task::batch([self.refresh_snapshots(), self.schedule_recluster()])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use thinkboard_core::fixtures::{self, edge};

    fn node(id: &str, text: &str, width: f32) -> CanvasNode {
        CanvasNode {
            text: Some(text.to_owned()),
            width,
            height: 100.0,
            ..fixtures::node(id)
        }
    }

    #[test]
    fn shift_indices_drops_the_removed_key_and_closes_the_gap() {
        let mut map: HashMap<usize, &str> =
            [(0, "a"), (1, "b"), (2, "c")].into_iter().collect();
        shift_indices(&mut map, 1);
        assert_eq!(map.get(&0), Some(&"a"));
        assert_eq!(map.get(&1), Some(&"c"));
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn a_snapshot_leaves_out_in_flight_ai_nodes_and_their_edges() {
        let nodes = vec![node("a", "kept", 100.0), node("ai", "streaming…", 100.0)];
        let edges = vec![edge("a", "ai"), edge("a", "a")];
        let transient: HashSet<String> = ["ai".to_owned()].into_iter().collect();

        let snapshot = committed_snapshot(&nodes, &edges, &transient);
        assert_eq!(snapshot.nodes.len(), 1);
        assert_eq!(snapshot.nodes[0].id, "a");
        // Only the self-edge survives; the one into the streaming node doesn't.
        assert_eq!(snapshot.edges.len(), 1);
        assert_eq!(snapshot.edges[0].to_node, "a");
    }

    #[test]
    fn an_edge_is_transient_from_either_end() {
        let transient: HashSet<String> = ["ai".to_owned()].into_iter().collect();
        assert!(is_transient_edge(&edge("a", "ai"), &transient));
        assert!(is_transient_edge(&edge("ai", "a"), &transient));
        assert!(!is_transient_edge(&edge("a", "b"), &transient));
    }

    #[test]
    fn a_snapshot_keeps_the_whole_document_when_nothing_is_transient() {
        let nodes = vec![node("a", "x", 100.0), node("b", "x", 100.0)];
        let edges = vec![edge("a", "b")];
        let snapshot = committed_snapshot(&nodes, &edges, &HashSet::new());
        assert_eq!(snapshot.nodes.len(), 2);
        assert_eq!(snapshot.edges.len(), 1);
    }

    #[test]
    fn a_snapshot_drops_every_edge_touching_a_transient_node() {
        // Two concurrent in-flight generations off "a": ai1 and ai2, with an
        // edge between them (a split's chained notes). Every edge touching a
        // transient node — including the one between the two transients — is out.
        let nodes = vec![
            node("a", "x", 100.0),
            node("ai1", "x", 100.0),
            node("ai2", "x", 100.0),
            node("b", "x", 100.0),
        ];
        let edges = vec![
            edge("a", "ai1"),   // committed -> transient: dropped
            edge("ai1", "ai2"), // transient -> transient: dropped
            edge("ai2", "b"),   // transient -> committed: dropped
            edge("a", "b"),     // committed -> committed: kept
        ];
        let transient: HashSet<String> =
            ["ai1".to_owned(), "ai2".to_owned()].into_iter().collect();

        let snapshot = committed_snapshot(&nodes, &edges, &transient);

        assert_eq!(
            snapshot.nodes.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(),
            ["a", "b"]
        );
        assert_eq!(
            snapshot.edges.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            ["a->b"]
        );
    }

    #[test]
    fn a_snapshot_is_an_independent_clone_of_the_live_document() {
        let nodes = vec![node("a", "x", 100.0)];
        let edges = vec![edge("a", "a")];
        let snapshot = committed_snapshot(&nodes, &edges, &HashSet::new());

        // Mutating the live document after snapshotting must not touch the snap.
        let mut nodes = nodes;
        nodes[0].text = Some("changed".to_owned());
        nodes.push(node("b", "x", 100.0));
        assert_eq!(snapshot.nodes.len(), 1);
        assert_eq!(snapshot.nodes[0].text.as_deref(), Some("x"));
    }

    /// End-to-end at the data layer: two Ask-AI generations off one node, each
    /// recording a pre-generation snapshot (as the handler does). Undo must peel
    /// them off one at a time — first gen 2, then gen 1 — and redo must rebuild
    /// them in order. This is the scenario the undo bugs hit.
    #[test]
    fn two_ai_generations_undo_and_redo_one_at_a_time() {
        use thinkboard_core::history::History;

        let ids = |snap: &DocSnapshot| -> Vec<String> {
            snap.nodes.iter().map(|n| n.id.clone()).collect()
        };
        let no_transient = HashSet::new();
        let mut history: History<DocSnapshot> = History::new(MAX_UNDO);

        // Start: just the source node "s".
        let mut nodes = vec![node("s", "x", 100.0)];
        let mut edges: Vec<CanvasEdge> = Vec::new();

        // Generation 1: record the pre-gen state, then add g1 (now committed).
        history.record(committed_snapshot(&nodes, &edges, &no_transient));
        nodes.push(node("g1", "x", 100.0));
        edges.push(edge("s", "g1"));

        // Generation 2: record the pre-gen state ({s, g1}), then add g2.
        history.record(committed_snapshot(&nodes, &edges, &no_transient));
        nodes.push(node("g2", "x", 100.0));
        edges.push(edge("s", "g2"));

        // Undo once → only generation 2 is peeled off.
        let live = committed_snapshot(&nodes, &edges, &no_transient);
        let after_undo1 = history.undo(live).unwrap();
        assert_eq!(ids(&after_undo1), ["s", "g1"]);
        assert_eq!(
            after_undo1.edges.iter().map(|e| e.id.clone()).collect::<Vec<_>>(),
            ["s->g1"]
        );

        // Undo again → generation 1 is peeled off, leaving just the source.
        let after_undo2 = history.undo(after_undo1).unwrap();
        assert_eq!(ids(&after_undo2), ["s"]);
        assert!(after_undo2.edges.is_empty());
        assert!(!history.can_undo());

        // Redo rebuilds gen 1, then gen 2, in order.
        let after_redo1 = history.redo(after_undo2).unwrap();
        assert_eq!(ids(&after_redo1), ["s", "g1"]);
        let after_redo2 = history.redo(after_redo1).unwrap();
        assert_eq!(ids(&after_redo2), ["s", "g1", "g2"]);
        assert!(!history.can_redo());
    }

    #[test]
    fn a_bitmap_survives_only_an_unchanged_text_and_size() {
        let text = Some("hello".to_owned());
        assert!(text_bitmap_reusable(&text, (100.0, 50.0), &text, (100.0, 50.0)));
        // Sub-pixel drift is tolerated, a real resize is not.
        assert!(text_bitmap_reusable(&text, (100.0, 50.0), &text, (100.2, 50.0)));
        assert!(!text_bitmap_reusable(&text, (100.0, 50.0), &text, (140.0, 50.0)));
        // Changed text always re-rasterizes.
        let other = Some("hello!".to_owned());
        assert!(!text_bitmap_reusable(&text, (100.0, 50.0), &other, (100.0, 50.0)));
    }
}
