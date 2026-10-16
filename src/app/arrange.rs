//! Where nodes go and how big they are: auto-sizing a card to its text,
//! finding a free spot for a new one, and the Mermaid-style layered
//! auto-layout that tidies a whole graph.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use iced::{Point, Rectangle, Size, Task};

use super::{App, FONT_SIZE, Message};
use crate::canvas::text_raster;
use crate::document::layout;
use crate::model::{self, CanvasNode};

/// Smallest height a text node can be resized to when node scrollbars are on
/// (with them off, the floor is the fitted content height instead).
const MIN_SCROLLABLE_NODE_HEIGHT: f32 = 40.0;

/// Clearance kept between a node and its horizontal neighbours as it auto-sizes,
/// matching the breathing room [`App::find_free_placement`] leaves when a node
/// is first placed.
const NEIGHBOR_GAP: f32 = 20.0;

/// Breathing room (world px) a new group frame leaves around its members, and
/// the taller inset at the top so the frame's caption band doesn't sit over the
/// topmost one. Mirrors `board::GROUP_HEADER_HEIGHT` plus a little.
const GROUP_PAD: f32 = 24.0;
const GROUP_HEAD_PAD: f32 = 44.0;

/// An auto-layout animation: each node eases from its old to its new position.
pub(super) struct LayoutAnim {
    pub(super) started: Instant,
    /// `(node_index, from, to)` top-left positions.
    pub(super) moves: Vec<(usize, (f32, f32), (f32, f32))>,
}

/// Whether two cards overlap — by more than touching, so a card placed flush
/// against another isn't counted as covering it.
fn covers(a: Rectangle, b: Rectangle) -> bool {
    a.intersection(&b).is_some_and(|both| both.width > 0.5 && both.height > 0.5)
}

pub(super) fn node_world_rect(node: &CanvasNode) -> Rectangle {
    Rectangle::new(
        Point::new(node.x, node.y),
        Size::new(node.width, node.height),
    )
}

/// The minimum height a text node may be resized to. Normally the fitted content
/// height (so text never clips); with scrollbars on, a small absolute floor —
/// the overflow scrolls instead. `content_height` is the rendered text extent.
pub(super) fn resize_min_height(scrollbars: bool, content_height: f32) -> f32 {
    if scrollbars {
        MIN_SCROLLABLE_NODE_HEIGHT
    } else {
        (content_height + text_raster::NODE_PAD_Y).max(60.0)
    }
}

/// The widest a text node occupying `me` may auto-size to before its box would
/// horizontally overlap a neighbour it shares vertical extent with.
///
/// Vertical overlap is allowed, so only neighbours on the same row constrain the
/// width, and only along the axis the card grows: centred growth (`anchor_left`
/// = false, the off-grid default) is bounded by the nearer of its left/right
/// neighbours; left-anchored growth (`anchor_left` = true, how a snapped node
/// grows from its fixed top-left) only by neighbours to its right. `gap` is the
/// clearance to keep. [`f32::INFINITY`] when nothing on the row blocks it.
fn horizontal_width_cap(
    me: Rectangle,
    others: impl Iterator<Item = Rectangle>,
    anchor_left: bool,
    gap: f32,
) -> f32 {
    let center_x = me.x + me.width / 2.0;
    let (my_left, my_top, my_bottom) = (me.x, me.y, me.y + me.height);
    let mut cap = f32::INFINITY;
    for other in others {
        let (o_left, o_right) = (other.x, other.x + other.width);
        // Off this row (no shared vertical extent) → vertical overlap is fine,
        // so it never bounds the width.
        if my_top >= other.y + other.height || other.y >= my_bottom {
            continue;
        }
        let bound = if anchor_left {
            // Fixed left edge, grows right: only a neighbour at/after our left
            // edge is in the way; one already overlapping on the left holds us.
            if o_left >= my_left {
                o_left - gap - my_left
            } else if o_right > my_left {
                me.width
            } else {
                continue;
            }
        } else if o_right <= center_x {
            // Symmetric about the centre: double the nearer side's half-width.
            2.0 * (center_x - o_right - gap)
        } else if o_left >= center_x {
            2.0 * (o_left - center_x - gap)
        } else {
            // Straddles the centre (already overlapping) — hold the width.
            me.width
        };
        cap = cap.min(bound);
    }
    cap
}

/// Snaps a newly created node's position and size to the grid. A `grid` of
/// `0.0` (snap-to-grid off) leaves it unchanged. Text nodes are further kept
/// aligned as they grow by [`App::autosize_node`]; this handles the initial
/// placement and fixed-size nodes (e.g. pasted images).
pub(super) fn snap_node_geometry(node: &mut CanvasNode, grid: f32) {
    (node.x, node.y) = crate::snap::point(node.x, node.y, grid);
    (node.width, node.height) = crate::snap::size(node.width, node.height, grid);
}

/// Fits an image of `width`x`height` px to a node no larger than `max` on its
/// longest side, preserving aspect ratio and never upscaling (a small image
/// keeps its pixel size).
pub(super) fn fit_image_size(width: usize, height: usize, max: f32) -> (f32, f32) {
    let (w, h) = (width as f32, height as f32);
    let longest = w.max(h);
    let scale = if longest > 0.0 {
        (max / longest).min(1.0)
    } else {
        1.0
    };
    (w * scale, h * scale)
}

/// The size for an aspect-locked resize: the smallest box with `aspect`
/// (width / height) that covers the `drag_w`×`drag_h` rectangle the cursor
/// swept and the `min_w`/`min_h` floors, without ever distorting `aspect`.
/// Keeps image nodes from stretching as a corner is dragged.
pub(super) fn aspect_locked_size(
    aspect: f32,
    drag_w: f32,
    drag_h: f32,
    min_w: f32,
    min_h: f32,
) -> (f32, f32) {
    let aspect = aspect.max(0.01);
    let want_w = drag_w.max(min_w);
    let want_h = drag_h.max(min_h);
    // Widen to whichever axis needs more, then derive the other from `aspect`,
    // so the result covers both wants and both minimums with the aspect intact.
    let width = want_w.max(want_h * aspect);
    (width, width / aspect)
}

/// The smallest translation `(dx, dy)` that moves a block of rects (each grown
/// by `MARGIN`) so none overlaps any `obstacles` rect, searched outward in
/// rings from the block's current position. `(0, 0)` if it already fits, or if
/// nothing clears within the search. Rays start due east and sweep round, so a
/// gap near the natural (to-the-right) position is preferred — keeping a placed
/// block compact rather than flung far in one direction.
fn nearest_free_offset(block: &[Rectangle], obstacles: &[Rectangle]) -> (f32, f32) {
    const MARGIN: f32 = 20.0;
    const STEP: f32 = 48.0;
    const RAYS: usize = 16;
    const RINGS: usize = 32;
    let clears = |dx: f32, dy: f32| {
        block.iter().all(|r| {
            let moved = Rectangle::new(
                Point::new(r.x - MARGIN + dx, r.y - MARGIN + dy),
                Size::new(r.width + 2.0 * MARGIN, r.height + 2.0 * MARGIN),
            );
            !obstacles.iter().any(|other| moved.intersects(other))
        })
    };
    if clears(0.0, 0.0) {
        return (0.0, 0.0);
    }
    for ring in 1..=RINGS {
        let radius = STEP * ring as f32;
        for ray in 0..RAYS {
            let angle = std::f32::consts::TAU * ray as f32 / RAYS as f32;
            let (dx, dy) = (radius * angle.cos(), radius * angle.sin());
            if clears(dx, dy) {
                return (dx, dy);
            }
        }
    }
    (0.0, 0.0)
}

impl App {
    /// The active snap spacing (world px), or `0.0` when snap-to-grid is off —
    /// so snap helpers can be called unconditionally (`0.0` = no-op).
    pub(super) fn snap_grid(&self) -> f32 {
        if self.config.snap_to_grid {
            self.config.snap_grid_size
        } else {
            0.0
        }
    }

    /// Fits the node to its text. While editing (`rendered = false`) it sizes
    /// to whichever is bigger of the raw source, so the editor fits, and the
    /// rendered Markdown it will become; on commit (`rendered = true`) to the
    /// rendered Markdown alone, so the read view fits. Runs on every edit, so
    /// the card resizes live while typing.
    pub(super) fn autosize_node(&mut self, index: usize, rendered: bool) {
        let Some(node) = self.nodes.get(index) else {
            return;
        };
        if !node.is_text() {
            return;
        }

        // Raw text (not trimmed): while editing, trailing newlines must
        // already grow the card. The text is trimmed when editing ends.
        let text = node.text.clone().unwrap_or_default();
        // A snapped node grows rightward from its fixed top-left; off the grid
        // it grows about its centre. That anchor decides which neighbours a
        // widening card can run into.
        let grid = self.snap_grid();
        let snapped = grid > 0.0;

        // Cap the width so the card doesn't widen into a horizontal neighbour —
        // one sharing vertical extent. Vertical overlap is allowed, so a capped
        // card just wraps its text and grows downward instead, mirroring the
        // clearance `find_free_placement` leaves around a node at creation.
        let me = node_world_rect(&self.nodes[index]);
        let others = self
            .nodes
            .iter()
            .enumerate()
            // Frames are not neighbours: a group encloses this card, so it
            // straddles the card's centre and would pin its width forever.
            .filter(|(i, n)| *i != index && !n.is_group())
            .map(|(_, n)| node_world_rect(n));
        let cap = horizontal_width_cap(me, others, snapped, NEIGHBOR_GAP);
        // Only limit growth — never force the card narrower than it already is.
        // Below the cap the node hugs the text; at the cap the display wraps to
        // the width.
        let auto_max = self
            .config
            .max_node_width
            .min(cap.max(me.width))
            .max(self.config.new_node_min_width);
        // A width the card already has outranks the auto-grow ceiling, because
        // the user may have dragged it there. `max_node_width` bounds how wide
        // a card grows *by itself*; it is not a limit on one sized by hand.
        let max_width = auto_max.max(me.width);
        // Both branches measure at the same card wrap column and inset
        // `MD_PADDING` a side, so the card is the same width whether editing or
        // rendered — `measure_source_card` styles bold/italic/code inline (which
        // plain measure under-counts) for the live editor; `measure_markdown`
        // lays out the read view. Sizing the editing card any narrower makes its
        // text wrap while the width chases the single-line extent, then snap on
        // commit.
        let wrap = text_raster::card_text_wrap(max_width);
        let (text_width, text_height) = if rendered {
            text_raster::measure_markdown(&text, wrap, FONT_SIZE, &self.theme)
        } else {
            // And no smaller than the read view it's about to become. A heading
            // is typed at body size but rendered large, so sized to the source
            // alone the card jumped on Esc — 284×61 to 409×93 for a one-line
            // heading over a body line. Sized to both, ending the edit leaves it
            // where it is (bar the trailing blank line the commit trims).
            let (source_width, source_height) =
                text_raster::measure_source_card(&text, wrap, FONT_SIZE, &self.theme);
            let (read_width, read_height) =
                text_raster::measure_markdown(&text, wrap, FONT_SIZE, &self.theme);
            (source_width.max(read_width), source_height.max(read_height))
        };

        // Auto-size grows a card, never narrows it. A width is something the
        // user can set deliberately by dragging, and re-deriving it from the
        // text on the next keystroke would throw that away — which it did.
        let mut width = (text_width + text_raster::NODE_PAD_X)
            .clamp(self.config.new_node_min_width, max_width)
            .max(me.width);
        // Height still follows the content, so text neither clips nor leaves a
        // card standing empty — but it stops at the ceiling, past which the
        // card scrolls instead of growing (see `text_raster::MAX_AUTO_HEIGHT`).
        let mut height = (text_height + text_raster::NODE_PAD_Y).clamp(
            self.config.new_node_min_height,
            text_raster::MAX_AUTO_HEIGHT.max(self.config.new_node_min_height),
        );

        // Snap-to-grid: round the fitted size *up* to whole cells so the text
        // still fits, and grow from a fixed (grid-aligned) top-left rather than
        // recentering — so a grid-aligned node stays aligned as it grows (e.g.
        // a streaming AI note). Off the grid, keep the centered grow.
        if snapped {
            (width, height) = crate::snap::fit_size(width, height, grid);
        }

        // Height grows downward from the top edge, snapped or not, the way a
        // page does. Grown about its centre, every new line pushed the top up
        // half a line — the text already written crept upward as you typed,
        // and a long note climbed over whatever sat above it.
        let node = &mut self.nodes[index];
        if (width - node.width).abs() > 0.5 || (height - node.height).abs() > 0.5 {
            if !snapped {
                node.x -= (width - node.width) / 2.0;
            }
            node.width = width;
            node.height = height;
            self.cache.clear();
        }
    }

    /// The size a new card takes to show `text` whole: as wide as it needs up
    /// to `max_node_width`, as tall as its rendered Markdown up to the
    /// auto-size ceiling — what [`App::autosize_node`] grows a card to, for a
    /// card that isn't anywhere yet. A paste is sized with it before it's
    /// placed, so the free spot found is free for the card it'll be.
    pub(super) fn text_card_size(&self, text: &str) -> (f32, f32) {
        let (min_width, min_height) = (self.config.new_node_min_width, self.config.new_node_min_height);
        let max_width = self.config.max_node_width.max(min_width);
        let wrap = text_raster::card_text_wrap(max_width);
        let (text_width, text_height) = text_raster::measure_markdown(text, wrap, FONT_SIZE, &self.theme);
        let width = (text_width + text_raster::NODE_PAD_X).clamp(min_width, max_width);
        let height = (text_height + text_raster::NODE_PAD_Y)
            .clamp(min_height, text_raster::MAX_AUTO_HEIGHT.max(min_height));
        let grid = self.snap_grid();
        if grid > 0.0 { crate::snap::fit_size(width, height, grid) } else { (width, height) }
    }

    /// The size a text node takes when a resize drag sweeps `drag_w`×`drag_h`,
    /// floored so its text never clips (unless node scrollbars are on, where
    /// the overflow scrolls instead).
    pub(super) fn resized_text_size(&self, node: &CanvasNode, drag_w: f32, drag_h: f32) -> (f32, f32) {
        let width = drag_w.max(150.0);
        // Height floor: normally the content fitted at this width, so text never
        // overflows. Measure the *rendered* Markdown (`measure_markdown`) so the
        // floor matches what draws — it insets to the render column and accounts
        // for heading sizes and bold widening, which plain `measure`
        // under-counts, leaving the card a line short (text then clips). With
        // node scrollbars on, drop the content floor so the node can shrink past
        // its text and the overflow scrolls.
        let text = node.text.clone().unwrap_or_default();
        let (_, text_height) = text_raster::measure_markdown(
            text.trim(),
            text_raster::card_text_wrap(width),
            FONT_SIZE,
            &self.theme,
        );
        // A card taller than the ceiling scrolls whatever the setting says, so
        // it can also be dragged shorter than its text — otherwise a long note
        // could never be resized down at all.
        let scrolls = self.config.node_scrollbars
            || text_height + text_raster::NODE_PAD_Y > text_raster::MAX_AUTO_HEIGHT;
        let min_height = resize_min_height(scrolls, text_height);
        let height = drag_h.max(min_height).max(MIN_SCROLLABLE_NODE_HEIGHT);
        (width, height)
    }

    /// Finds a center near `requested` where a `width`x`height` node (plus
    /// breathing room) won't overlap existing nodes, searching outward in
    /// rings. Falls back to `requested` if the area is too crowded.
    pub(super) fn find_free_placement(&self, requested: Point, width: f32, height: f32) -> Point {
        self.find_free_placement_avoiding(requested, width, height, &[])
    }

    /// Like [`App::find_free_placement`], but also steers clear of the extra
    /// rects `avoid` (e.g. sibling ghost cards being laid out together).
    pub(super) fn find_free_placement_avoiding(
        &self,
        requested: Point,
        width: f32,
        height: f32,
        avoid: &[Rectangle],
    ) -> Point {
        const MARGIN: f32 = 20.0;
        const RAYS: usize = 12;
        const RINGS: usize = 8;

        let fits = |center: Point| {
            let rect = Rectangle::new(
                Point::new(
                    center.x - width / 2.0 - MARGIN,
                    center.y - height / 2.0 - MARGIN,
                ),
                Size::new(width + 2.0 * MARGIN, height + 2.0 * MARGIN),
            );
            !self
                .nodes
                .iter()
                // Frames aren't obstacles — a group is drawn *around* nodes, so
                // treating it as occupied space would push every new node out
                // of it.
                .filter(|node| !node.is_group())
                .any(|node| rect.intersects(&node_world_rect(node)))
                && !avoid.iter().any(|other| rect.intersects(other))
        };

        if fits(requested) {
            return requested;
        }
        let step = height + MARGIN;
        for ring in 1..=RINGS {
            let radius = step * ring as f32;
            for ray in 0..RAYS {
                let angle = std::f32::consts::TAU * ray as f32 / RAYS as f32;
                let candidate = Point::new(
                    requested.x + radius * angle.cos(),
                    requested.y + radius * angle.sin(),
                );
                if fits(candidate) {
                    return candidate;
                }
            }
        }
        requested
    }

    /// Creates an empty text node placed near a world point (avoiding
    /// overlap), animates it in, and queues its editor to open. Returns the
    /// new node's index.
    pub(super) fn spawn_text_node(&mut self, world: Point) -> usize {
        // Undoing the note takes it back whole, so the point is before it was
        // made — and before whatever made it moved anything (a connected
        // note's link and the relayout after it).
        self.fresh_undo = Some(self.snapshot());
        let width = self.config.new_node_min_width;
        let height = self.config.new_node_min_height;
        let center = self.find_free_placement(world, width, height);
        let mut node = CanvasNode {
            id: model::random_id(),
            kind: "text".to_owned(),
            text: Some(String::new()),
            file: None,
            x: center.x - width / 2.0,
            y: center.y - height / 2.0,
            width,
            height,
            color: None,
            extra: Default::default(),
        };
        snap_node_geometry(&mut node, self.snap_grid());
        self.nodes.push(node);
        let index = self.nodes.len() - 1;
        self.spawning.insert(index, Instant::now());
        self.pending_edit = Some(index);
        self.fresh_node = Some(index);
        self.mark_dirty();
        self.cache.clear();
        index
    }

    /// The selected nodes a frame would actually enclose. Frames are left out:
    /// a group around one node is a box around a box, and one around another
    /// frame is best done by selecting its contents.
    fn groupable_members(&self) -> Vec<usize> {
        self.selected
            .iter()
            .copied()
            .filter(|&i| self.nodes.get(i).is_some_and(|n| !n.is_group()))
            .collect()
    }

    /// Brings what was just dropped to the top of the stacking order, if it
    /// landed on a card that would otherwise cover it.
    ///
    /// Stacking is the order of the node list, as in Obsidian. While dragged a
    /// card is drawn above everything (the selection is), and once deselected
    /// it went back to its place in the list — under any later card it had
    /// been dropped on. Now the dropped cards move to the end of the list
    /// instead. Only when they cover such a card, though: an order changed for
    /// nothing is a file changed for nothing. Frames keep their place — they
    /// draw behind everything whatever their order.
    pub(super) fn raise_dropped(&mut self, index: usize) {
        let card = |i: usize| self.nodes.get(i).is_some_and(|n| !n.is_group());
        let mut dropped: Vec<usize> = self.drag_riders(index).into_iter().filter(|&i| card(i)).collect();
        dropped.sort_unstable();
        let covered_by_a_later_card = dropped.iter().any(|&d| {
            let rect = node_world_rect(&self.nodes[d]);
            (d + 1..self.nodes.len())
                .filter(|&o| card(o) && !dropped.contains(&o))
                .any(|o| covers(rect, node_world_rect(&self.nodes[o])))
        });
        if covered_by_a_later_card {
            let order: Vec<usize> = (0..self.nodes.len())
                .filter(|i| !dropped.contains(i))
                .chain(dropped.iter().copied())
                .collect();
            self.reorder_nodes(&order);
        }
    }

    /// Puts the nodes in `order` (old indices, each once), re-keying everything
    /// that names a node by its index.
    fn reorder_nodes(&mut self, order: &[usize]) {
        let mut new_of_old = vec![0; order.len()];
        for (new, &old) in order.iter().enumerate() {
            new_of_old[old] = new;
        }
        let mut old_nodes: Vec<Option<CanvasNode>> = std::mem::take(&mut self.nodes).into_iter().map(Some).collect();
        self.nodes = order.iter().filter_map(|&old| old_nodes[old].take()).collect();

        let moved = |i: usize| new_of_old[i];
        super::history::remap_indices(&mut self.rasters.text, &new_of_old);
        super::history::remap_indices(&mut self.rasters.staging, &new_of_old);
        super::history::remap_indices(&mut self.spawning, &new_of_old);
        for selected in &mut self.selected {
            *selected = moved(*selected);
        }
        self.pending_edit = self.pending_edit.map(moved);
        self.fresh_node = self.fresh_node.map(moved);
        if let Some(editing) = &mut self.editing {
            editing.index = moved(editing.index);
        }
        if let Some(search) = &mut self.search {
            for hit in &mut search.matches {
                hit.node = moved(hit.node);
            }
        }
        if let Some(anim) = &mut self.layout_anim {
            for (node, _, _) in &mut anim.moves {
                *node = moved(*node);
            }
        }
        self.mark_dirty();
        self.cache.clear();
    }

    /// Whether the selection is worth grouping — the condition the palette
    /// entry and the context-menu item both gate on.
    pub(super) fn groupable_selection(&self) -> bool {
        self.groupable_members().len() >= 2
    }

    /// Wraps the selected nodes in a group frame and selects it.
    ///
    /// The frame is pushed onto the end of the node list like any other
    /// creation, which leaves every existing index — and the caches keyed by
    /// them — untouched. Drawing order doesn't suffer for it: frames are drawn
    /// in their own pass behind everything (see `Board::draw_groups`), not in
    /// document order.
    pub(super) fn group_selection(&mut self) -> Task<Message> {
        let members = self.groupable_members();
        if members.len() < 2 {
            return Task::none();
        }

        let (mut min_x, mut min_y) = (f32::MAX, f32::MAX);
        let (mut max_x, mut max_y) = (f32::MIN, f32::MIN);
        for &i in &members {
            let node = &self.nodes[i];
            min_x = min_x.min(node.x);
            min_y = min_y.min(node.y);
            max_x = max_x.max(node.x + node.width);
            max_y = max_y.max(node.y + node.height);
        }

        self.push_undo();
        let mut frame = CanvasNode {
            id: model::random_id(),
            kind: "group".to_owned(),
            text: None,
            file: None,
            x: min_x - GROUP_PAD,
            y: min_y - GROUP_HEAD_PAD,
            width: (max_x - min_x) + GROUP_PAD * 2.0,
            height: (max_y - min_y) + GROUP_HEAD_PAD + GROUP_PAD,
            color: None,
            extra: Default::default(),
        };
        // No label: the format leaves it optional, and there's no rename UI yet,
        // so a default caption would be permanent noise. Obsidian can name it.
        snap_node_geometry(&mut frame, self.snap_grid());
        self.nodes.push(frame);
        // Deliberately not spawn-animated. A frame appears *around* content
        // that's already on screen, so scaling it up from nothing reads as the
        // contents having jumped rather than as the frame arriving.
        let index = self.nodes.len() - 1;
        self.selected = vec![index];
        self.selected_edge = None;
        // Marked fresh so the caption edit that follows doesn't record a second
        // undo step on top of the creation. Unlike a text node, a frame left
        // unnamed is *not* taken back — an unnamed group is a fine group.
        self.fresh_node = Some(index);
        self.fresh_undo = None;
        self.mark_dirty();
        self.cache.clear();
        // Drop straight into naming it, the way a drag-created note opens its
        // editor.
        Task::batch([
            self.schedule_recluster(),
            Task::done(Message::StartEditing { index, at: None }),
        ])
    }

    /// Lays out `subset` as a Mermaid-style layered graph flowing in `dir`,
    /// re-anchoring touched links and easing nodes to their targets. Streaming
    /// relayouts pass `record_undo = false` so they don't flood the undo stack.
    pub(super) fn apply_layered_layout(
        &mut self,
        subset: &[usize],
        dir: layout::Direction,
        record_undo: bool,
    ) {
        let moves = layout::layered_layout(
            &self.nodes,
            &self.edges,
            subset,
            dir,
            self.config.layout_rank_gap,
            self.config.layout_node_gap,
        );
        if moves.is_empty() {
            return;
        }
        if record_undo {
            self.push_undo();
        }

        // Post-layout centers for every node (moved -> target, else unchanged).
        let mut centers: Vec<(f32, f32)> = self
            .nodes
            .iter()
            .map(|n| (n.x + n.width / 2.0, n.y + n.height / 2.0))
            .collect();
        let mut moved = HashSet::new();
        for &(i, x, y) in &moves {
            centers[i] = (x + self.nodes[i].width / 2.0, y + self.nodes[i].height / 2.0);
            moved.insert(i);
        }

        // Re-anchor links touching a moved node to the facing ports.
        let id_index: HashMap<String, usize> = self
            .nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (n.id.clone(), i))
            .collect();
        for edge in &mut self.edges {
            let (Some(&from), Some(&to)) =
                (id_index.get(&edge.from_node), id_index.get(&edge.to_node))
            else {
                continue;
            };
            if moved.contains(&from) || moved.contains(&to) {
                let (fs, ts) = layout::facing_sides(centers[from], centers[to], dir);
                edge.from_side = fs.to_owned();
                edge.to_side = ts.to_owned();
            }
        }

        // Ease each node from its current spot to the target.
        let anims = moves
            .iter()
            .map(|&(i, x, y)| (i, (self.nodes[i].x, self.nodes[i].y), (x, y)))
            .collect();
        self.layout_anim = Some(LayoutAnim {
            started: Instant::now(),
            moves: anims,
        });
        self.mark_dirty();
        self.cache.clear();
    }

    /// The `auto_layout_direction` config as a layout direction.
    pub(super) fn config_layout_dir(&self) -> layout::Direction {
        if self.config.auto_layout_direction == "TD" {
            layout::Direction::Down
        } else {
            layout::Direction::Right
        }
    }

    /// Node indices reachable from `start` via links (treated undirected).
    fn connected_component(&self, start: usize) -> Vec<usize> {
        let id_index: HashMap<&str, usize> =
            self.nodes.iter().enumerate().map(|(i, n)| (n.id.as_str(), i)).collect();
        let mut visited = HashSet::from([start]);
        let mut stack = vec![start];
        while let Some(i) = stack.pop() {
            let id = self.nodes[i].id.as_str();
            for edge in &self.edges {
                let neighbor = if edge.from_node == id {
                    Some(edge.to_node.as_str())
                } else if edge.to_node == id {
                    Some(edge.from_node.as_str())
                } else {
                    None
                };
                let Some(&ni) = neighbor.and_then(|nid| id_index.get(nid)) else {
                    continue;
                };
                if visited.insert(ni) {
                    stack.push(ni);
                }
            }
        }
        let mut result: Vec<usize> = visited.into_iter().collect();
        result.sort_unstable();
        result
    }

    /// Re-lays out the whole graph `node` belongs to, unconditionally.
    fn relayout_component(&mut self, node: usize, record_undo: bool) {
        let component = self.connected_component(node);
        if component.len() > 1 {
            let dir = self.config_layout_dir();
            self.apply_layered_layout(&component, dir, record_undo);
        }
    }

    /// When auto-layout is enabled, re-lays out the graph `node` belongs to.
    pub(super) fn auto_relayout(&mut self, node: usize, record_undo: bool) {
        if self.config.auto_layout {
            self.relayout_component(node, record_undo);
        }
    }

    /// Tidies just this generation's response nodes (`ai_ids`) into a clean
    /// sibling column (or row) off their source, then translates the whole
    /// stack to the nearest free spot so it never overlaps an existing node.
    /// The source and every other node stay put. The nearest-gap search keeps
    /// the stack compact (nudged just off an obstruction, e.g. below a prior
    /// generation) rather than sliding it far down the flow axis.
    pub(super) fn relayout_ai_siblings(&mut self, source_id: &str, ai_ids: &[String]) {
        let Some(source) = self.nodes.iter().position(|n| n.id == source_id) else {
            return;
        };
        let ai: Vec<usize> = ai_ids
            .iter()
            .filter_map(|id| self.nodes.iter().position(|n| &n.id == id))
            .collect();
        if ai.is_empty() {
            return;
        }
        // One answer, one width. Each note sized to its own text left the
        // column's right edge ragged, and a list read as unrelated cards. They
        // take the widest's width — auto-size only ever grows a card, so this
        // is the one width none of them has to shrink to — and are re-fitted,
        // so a height still follows its text.
        let widest = ai.iter().map(|&i| self.nodes[i].width).fold(0.0, f32::max);
        for &i in &ai {
            if self.nodes[i].width < widest {
                self.nodes[i].width = widest;
                self.autosize_node(i, true);
            }
        }
        let dir = self.config_layout_dir();
        let src = &self.nodes[source];
        let source_box = (src.x, src.y, src.width, src.height);
        let sizes: Vec<(f32, f32)> = ai
            .iter()
            .map(|&i| (self.nodes[i].width, self.nodes[i].height))
            .collect();
        let mut targets = layout::sibling_stack(
            source_box,
            &sizes,
            dir,
            self.config.layout_rank_gap,
            self.config.layout_node_gap,
        );

        // `sibling_stack` anchors the block beside the source, blind to other
        // nodes. Shift the whole block to the nearest free position (searched in
        // rings) so the generated nodes clear every node outside this
        // generation — compactly, in whatever direction has room.
        let block: Vec<Rectangle> = targets
            .iter()
            .zip(&sizes)
            .map(|(&(x, y), &(w, h))| Rectangle::new(Point::new(x, y), Size::new(w, h)))
            .collect();
        let ignore: HashSet<usize> = std::iter::once(source).chain(ai.iter().copied()).collect();
        let mut obstacles: Vec<Rectangle> = self
            .nodes
            .iter()
            .enumerate()
            .filter(|(i, node)| !ignore.contains(i) && !node.is_group())
            .map(|(_, node)| node_world_rect(node))
            .collect();
        // When the question *is* a frame, that frame is an obstacle for its own
        // answers — settling them inside it would make them its members. See
        // `App::ai_answer_placement`.
        if self.nodes[source].is_group() {
            obstacles.push(node_world_rect(&self.nodes[source]));
        }
        let (dx, dy) = nearest_free_offset(&block, &obstacles);
        if dx != 0.0 || dy != 0.0 {
            for target in &mut targets {
                target.0 += dx;
                target.1 += dy;
            }
        }

        // Snap each generated node's resting position to the grid (their sizes
        // are already grid-aligned via autosize, so top-left snapping is enough).
        let grid = self.snap_grid();
        if grid > 0.0 {
            for target in &mut targets {
                (target.0, target.1) = crate::snap::point(target.0, target.1, grid);
            }
        }

        // Face the source→child links to their sides.
        let (from_side, to_side) = match dir {
            layout::Direction::Down => ("bottom", "top"),
            layout::Direction::Right => ("right", "left"),
        };
        let ai_set: HashSet<&str> = ai_ids.iter().map(String::as_str).collect();
        for edge in &mut self.edges {
            if edge.from_node == source_id && ai_set.contains(edge.to_node.as_str()) {
                edge.from_side = from_side.to_owned();
                edge.to_side = to_side.to_owned();
            }
        }

        // Ease only this generation's nodes to their new spots.
        let anims = ai
            .iter()
            .zip(targets)
            .map(|(&i, (x, y))| (i, (self.nodes[i].x, self.nodes[i].y), (x, y)))
            .collect();
        self.layout_anim = Some(LayoutAnim {
            started: Instant::now(),
            moves: anims,
        });
        self.mark_dirty();
        self.cache.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resize_min_height_respects_node_scrollbars() {
        // Scrollbars OFF: the floor is the fitted content (+ vertical padding),
        // never below 60 — so a resize can't shrink a node past its text (it
        // would clip). This is today's autoresize-friendly behavior.
        assert!(resize_min_height(false, 200.0) >= 200.0);
        assert_eq!(resize_min_height(false, 5.0), 60.0); // tiny content → 60 floor
        // Scrollbars ON: a small absolute floor well below the content, so the
        // node CAN be undersized (the overflow scrolls). This is the resolution
        // of the conflict with autoresize — the two behaviors must diverge here.
        assert_eq!(resize_min_height(true, 200.0), MIN_SCROLLABLE_NODE_HEIGHT);
        assert!(resize_min_height(true, 200.0) < resize_min_height(false, 200.0));
    }

    fn rect(x: f32, y: f32, w: f32, h: f32) -> Rectangle {
        Rectangle::new(Point::new(x, y), Size::new(w, h))
    }

    #[test]
    fn width_cap_is_unbounded_without_neighbours() {
        // Nothing to run into → the caller's own min/max bounds apply, unchanged.
        let cap = horizontal_width_cap(
            rect(0.0, 0.0, 250.0, 60.0),
            std::iter::empty(),
            false,
            NEIGHBOR_GAP,
        );
        assert!(cap.is_infinite());
    }

    #[test]
    fn width_cap_allows_vertical_overlap() {
        let me = rect(0.0, 0.0, 250.0, 60.0);
        // Directly below (no shared vertical extent): vertical overlap is fine,
        // so it never bounds the width — the whole point of the feature.
        let below = [rect(0.0, 100.0, 250.0, 60.0)];
        assert!(
            horizontal_width_cap(me, below.into_iter(), false, NEIGHBOR_GAP).is_infinite()
        );
        // Rows that merely touch (my bottom == its top) are still off-row.
        let touching = [rect(300.0, 60.0, 100.0, 60.0)];
        assert!(
            horizontal_width_cap(me, touching.into_iter(), false, NEIGHBOR_GAP).is_infinite()
        );
    }

    #[test]
    fn width_cap_bounds_centred_growth_by_the_nearer_neighbour() {
        let me = rect(0.0, 0.0, 250.0, 60.0); // centre_x = 125
        // Right neighbour at x=300 → 2*(300 - 20 - 125) = 310.
        let right = [rect(300.0, 0.0, 50.0, 60.0)];
        assert_eq!(horizontal_width_cap(me, right.into_iter(), false, NEIGHBOR_GAP), 310.0);
        // Left neighbour ending at x=0 → 2*(125 - 0 - 20) = 210.
        let left = [rect(-80.0, 0.0, 80.0, 60.0)];
        assert_eq!(horizontal_width_cap(me, left.into_iter(), false, NEIGHBOR_GAP), 210.0);
        // With both, the tighter side (left, 210) wins.
        let both = [rect(300.0, 0.0, 50.0, 60.0), rect(-80.0, 0.0, 80.0, 60.0)];
        assert_eq!(horizontal_width_cap(me, both.into_iter(), false, NEIGHBOR_GAP), 210.0);
    }

    #[test]
    fn width_cap_counts_partial_vertical_overlap_as_a_neighbour() {
        // Only rows 40..60 are shared, but that still makes it a horizontal
        // neighbour → bounds the width the same as a full-height one.
        let me = rect(0.0, 0.0, 250.0, 60.0);
        let overlap = [rect(300.0, 40.0, 100.0, 100.0)];
        assert_eq!(horizontal_width_cap(me, overlap.into_iter(), false, NEIGHBOR_GAP), 310.0);
    }

    #[test]
    fn width_cap_left_anchored_ignores_left_neighbours() {
        // A snapped card grows rightward from a fixed left edge (x = 100).
        let me = rect(100.0, 0.0, 250.0, 60.0);
        // A neighbour fully to the left is out of the growth path → unbounded.
        let left = [rect(0.0, 0.0, 50.0, 60.0)];
        assert!(
            horizontal_width_cap(me, left.into_iter(), true, NEIGHBOR_GAP).is_infinite()
        );
        // A right neighbour bounds it: 500 - 20 - 100 = 380.
        let right = [rect(500.0, 0.0, 100.0, 60.0)];
        assert_eq!(horizontal_width_cap(me, right.into_iter(), true, NEIGHBOR_GAP), 380.0);
    }

    #[test]
    fn width_cap_holds_the_current_width_when_already_overlapping() {
        // Centred: a neighbour straddling the centre can't be grown clear of, so
        // the card holds its current width rather than widening further in.
        let me = rect(0.0, 0.0, 250.0, 60.0); // centre_x = 125
        let straddling = [rect(100.0, 0.0, 60.0, 60.0)]; // spans 100..160 over 125
        assert_eq!(horizontal_width_cap(me, straddling.into_iter(), false, NEIGHBOR_GAP), 250.0);
        // Left-anchored: a neighbour overlapping the fixed left edge holds it too.
        let me = rect(100.0, 0.0, 250.0, 60.0);
        let over_left = [rect(50.0, 0.0, 80.0, 60.0)]; // spans 50..130 over left edge 100
        assert_eq!(horizontal_width_cap(me, over_left.into_iter(), true, NEIGHBOR_GAP), 250.0);
    }

    fn tnode(id: &str) -> CanvasNode {
        CanvasNode {
            id: id.to_owned(),
            kind: "text".to_owned(),
            text: Some("x".to_owned()),
            file: None,
            x: 0.0,
            y: 0.0,
            width: 1.0,
            height: 1.0,
            color: None,
            extra: Default::default(),
        }
    }

    #[test]
    fn snap_node_geometry_snaps_position_and_size() {
        // A node created off-grid: every creation site routes through this.
        let mut node = tnode("n");
        node.x = 23.0;
        node.y = -9.0;
        node.width = 247.0;
        node.height = 51.0;
        snap_node_geometry(&mut node, 20.0);
        // Position rounds to the nearest lattice point...
        assert_eq!((node.x, node.y), (20.0, 0.0)); // -9 -> -0
        // ...and size to the nearest whole cell (247->240, 51->60).
        assert_eq!((node.width, node.height), (240.0, 60.0));
    }

    #[test]
    fn snap_node_geometry_is_a_no_op_when_snapping_is_off() {
        let mut node = tnode("n");
        node.x = 23.0;
        node.y = -9.0;
        node.width = 247.0;
        node.height = 51.0;
        snap_node_geometry(&mut node, 0.0);
        assert_eq!((node.x, node.y, node.width, node.height), (23.0, -9.0, 247.0, 51.0));
    }

    #[test]
    fn fit_image_size_scales_down_preserving_aspect() {
        // 2:1 landscape past the limit halves cleanly.
        assert_eq!(fit_image_size(840, 420, 420.0), (420.0, 210.0));
    }

    #[test]
    fn fit_image_size_never_upscales_a_small_image() {
        assert_eq!(fit_image_size(100, 50, 420.0), (100.0, 50.0));
    }

    #[test]
    fn fit_image_size_fits_a_square_to_the_max() {
        assert_eq!(fit_image_size(800, 800, 400.0), (400.0, 400.0));
    }

    #[test]
    fn aspect_locked_size_holds_the_ratio_on_a_horizontal_drag() {
        // 2:1 image, dragged wide but barely tall: height follows the width.
        let (w, h) = aspect_locked_size(2.0, 400.0, 10.0, 80.0, 60.0);
        assert_eq!((w, h), (400.0, 200.0));
    }

    #[test]
    fn aspect_locked_size_holds_the_ratio_on_a_vertical_drag() {
        // Same 2:1 image dragged tall: width grows to keep the ratio.
        let (w, h) = aspect_locked_size(2.0, 10.0, 150.0, 80.0, 60.0);
        assert_eq!((w, h), (300.0, 150.0));
    }

    #[test]
    fn aspect_locked_size_covers_the_drag_rectangle() {
        // The box always contains the swept rectangle, never distorting.
        let (w, h) = aspect_locked_size(2.0, 300.0, 200.0, 80.0, 60.0);
        assert!(w >= 300.0 && h >= 200.0);
        assert!((w / h - 2.0).abs() < 1e-4);
    }

    #[test]
    fn aspect_locked_size_enforces_the_minimum_without_distorting() {
        // A tiny drag snaps up to the floors but keeps the 2:1 ratio.
        let (w, h) = aspect_locked_size(2.0, 5.0, 5.0, 80.0, 60.0);
        assert!(w >= 80.0 && h >= 60.0);
        assert!((w / h - 2.0).abs() < 1e-4);
    }

    #[test]
    fn nearest_free_offset_nudges_a_blocked_block_and_leaves_a_free_one() {
        let block = [Rectangle::new(Point::new(0.0, 0.0), Size::new(80.0, 40.0))];
        // Obstacle sitting exactly on the block → a non-zero nudge that clears.
        let obstacle = [Rectangle::new(Point::new(0.0, 0.0), Size::new(80.0, 40.0))];
        let (dx, dy) = nearest_free_offset(&block, &obstacle);
        assert!(dx != 0.0 || dy != 0.0);
        let moved = Rectangle::new(Point::new(dx, dy), Size::new(80.0, 40.0));
        assert!(!moved.intersects(&obstacle[0]));

        // Nothing in the way → no offset.
        assert_eq!(nearest_free_offset(&block, &[]), (0.0, 0.0));
    }
}
