//! Semantic zoom: the cluster cards drawn in place of the cards when zoomed
//! out, the links between them, and the cross-fade between the two scenes.

use super::*;

/// How far a cluster drag moves this frame: from the anchor's current position
/// to where the cursor wants it, snapped.
///
/// **This is why cluster snapping exists at all.** Every member ends up at
/// `target + (member - anchor)`, so with `target` on the grid a member stays on
/// it exactly when its offset from the anchor is a whole number of cells —
/// automatic if they were all aligned to begin with. Dragging used to apply the
/// raw cursor delta, which left an aligned cluster sitting at whatever fraction
/// of a cell the mouse stopped at.
///
/// It has to be the *absolute* target that's snapped, not the per-frame step.
/// A step is a few pixels: rounding it floors to zero, so the cluster never
/// moves, or jumps a whole cell — and the rounding accumulates into a drift
/// away from the cursor.
pub(super) fn cluster_delta(want: Point, anchor: Point, grid: f32) -> Vector {
    let (x, y) = crate::snap::point(want.x, want.y, grid);
    Point::new(x, y) - anchor
}

/// The member node a cluster drag is measured against.
///
/// Snapping has to move the *nodes* onto the grid, not the cluster's box: the
/// box is derived from its members plus padding and is recomputed the moment
/// the drag ends, so aligning it would leave the stored geometry at an
/// arbitrary offset — and would knock nodes that were already on the grid off
/// it. Measuring against one member keeps every member's alignment.
///
/// Topmost then leftmost, so the choice is deterministic and the reference
/// can't change part-way through a drag.
pub(super) fn cluster_anchor(
    clusters: &[crate::cluster::Cluster],
    nodes: &[CanvasNode],
    cluster: usize,
) -> Option<usize> {
    clusters
        .get(cluster)?
        .members
        .iter()
        .copied()
        .filter(|&i| i < nodes.len())
        .min_by(|&a, &b| {
            let (a, b) = (&nodes[a], &nodes[b]);
            (a.y, a.x)
                .partial_cmp(&(b.y, b.x))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
}

/// Cluster summaries become the interactive view below `config.cluster_zoom`.
/// Rendering crossfades across [`CLUSTER_FADE_NONE`, `CLUSTER_FADE_FULL`]
/// around it. (Wheel/pinch zoom sensitivity are in config too.)
/// At/below this zoom clusters are fully opaque; at/above `CLUSTER_FADE_NONE`
/// they are gone — between, clusters and nodes crossfade.
pub(super) const CLUSTER_FADE_FULL: f32 = 0.35;

pub(super) const CLUSTER_FADE_NONE: f32 = 0.45;

/// Rendering weight of clusters (1) vs nodes (0) at the given zoom.
pub fn cluster_opacity(zoom: f32) -> f32 {
    let t = ((CLUSTER_FADE_NONE - zoom) / (CLUSTER_FADE_NONE - CLUSTER_FADE_FULL)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t) // smoothstep
}

impl Board<'_> {
    /// Topmost cluster whose box contains the given world point.
    pub(super) fn cluster_at(&self, world: Point) -> Option<usize> {
        self.clusters
            .iter()
            .rposition(|cluster| cluster.bounds.contains(world))
    }

    /// Whether zoomed out far enough that clusters are the interactive view
    /// (input handoff; distinct from the rendering crossfade).
    pub fn show_clusters(&self) -> bool {
        !self.clusters.is_empty() && self.camera.zoom < self.config.cluster_zoom
    }

    /// Crossfade weight of the cluster layer (0 when no clusters exist).
    pub(super) fn cluster_weight(&self) -> f32 {
        if self.clusters.is_empty() {
            0.0
        } else {
            cluster_opacity(self.camera.zoom)
        }
    }

    pub(super) fn node_layer_alpha(&self) -> f32 {
        self.scene_alpha() * (1.0 - self.cluster_weight())
    }

    pub(super) fn cluster_layer_alpha(&self) -> f32 {
        self.scene_alpha() * self.cluster_weight()
    }

    /// Draws each cluster as one large summary card: the main card's text
    /// at its fitted size, or the main image scaled to the cluster box.
    pub(super) fn draw_clusters(&self, frame: &mut Frame) {
        let alpha = self.cluster_layer_alpha();
        let zoom = self.camera.zoom;

        for (index, cluster) in self.clusters.iter().enumerate() {
            let rect = self.camera.rect_to_screen(cluster.bounds);
            let card = rounded_path(rect, self.frame_radius());

            let fill = self.theme.card_fill;
            frame.fill(&card, Color { a: fill.a * alpha, ..fill });

            if let Some(id) = &cluster.image_id {
                if let Some(handle) = self.images.get(id) {
                    let mut image = canvas::Image::new(handle.clone());
                    image.border_radius = self.frame_radius().into();
                    image.opacity = alpha;
                    frame.with_clip(rect, |clipped| {
                        clipped.draw_image(rect, image);
                    });
                }
            } else if !cluster.text.is_empty() {
                // Same text path as node labels: a bitmap snapshot, or live
                // vector runs when rasterizing is off.
                let wrap = (cluster.bounds.width - crate::cluster::PADDING * 2.0).max(20.0);
                if let Some(snapshot) =
                    self.cluster_snapshots.get(&index).filter(|_| self.config.rasterize_text)
                {
                    let mut image = canvas::Image::new(snapshot.handle.clone());
                    image.opacity = alpha;
                    frame.draw_image(rect, image);
                } else {
                    // No bitmap — because rasterizing is off, or because one is
                    // being rebuilt after a theme change. Either way draw the
                    // text live, as a node does in the same situation; a label
                    // in the wrong font for a moment beats a blank card.
                    frame.with_clip(rect, |clipped| {
                        self.draw_markdown_live(clipped, rect, &cluster.text, cluster.font_size, wrap, alpha, None);
                    });
                }
            }

            frame.stroke(
                &card,
                Stroke::default()
                    .with_color(Color { a: CARD_BORDER.a * alpha, ..CARD_BORDER })
                    .with_width((0.75 * zoom).max(0.5)),
            );
        }
    }

    /// Arrows between connected clusters, drawn behind the cluster cards.
    /// Sized larger than ordinary links since clusters are large.
    pub(super) fn draw_cluster_links(&self, frame: &mut Frame) {
        let alpha = self.cluster_layer_alpha();
        let zoom = self.camera.zoom;
        let stroke_width = (9.0 * zoom).max(1.5);
        let head = (26.0 * zoom).max(6.0);

        for link in self.cluster_links {
            let (Some(from), Some(to)) =
                (self.clusters.get(link.from), self.clusters.get(link.to))
            else {
                continue;
            };
            let from_rect = self.camera.rect_to_screen(from.bounds);
            let to_rect = self.camera.rect_to_screen(to.bounds);

            let from_side = facing_side(to_rect.center(), from_rect);
            let to_side = facing_side(from_rect.center(), to_rect);
            let start = side_anchor(from_rect, from_side);
            let cluster_head = self.head(head, stroke_width * 0.8);
            let end = arrow_tip(side_anchor(to_rect, to_side), to_side, zoom, cluster_head);

            let reach = start.distance(end) * 0.4;
            let control1 = start + outward(from_side) * reach;
            let control2 = end + outward(to_side) * reach;

            let g = EdgeGeometry { start, control1, control2, end, alpha: 1.0 };
            let (trims, wedges) = shaft([None, Some((end, to_side, cluster_head))], stroke_width);
            let ink = self.link_ink(EDGE_COLOR);
            let color = Color { a: ink.a * alpha, ..ink };
            fill_wedges(frame, &wedges, color);
            frame.stroke(
                &self.link_path_trimmed(None, &g, trims),
                Stroke {
                    // Butt, to meet the wedge into its arrowhead — see `shaft`.
                    line_cap: stroke::LineCap::Butt,
                    line_join: stroke::LineJoin::Round,
                    ..Stroke::default()
                        .with_color(color)
                        .with_width(stroke_width)
                },
            );
            self.draw_arrowhead(frame, end, to_side, alpha, cluster_head, ink, false);
        }
    }
}

#[cfg(test)]
mod cluster_drag_tests {
    use super::cluster_anchor;
    use crate::cluster::Cluster;
    use crate::model::CanvasNode;
    use iced::{Rectangle, Size, Point};

    fn node(x: f32, y: f32) -> CanvasNode {
        CanvasNode {
            id: format!("n{x}x{y}"),
            kind: "text".to_owned(),
            text: Some("x".to_owned()),
            file: None,
            x,
            y,
            width: 100.0,
            height: 60.0,
            color: None,
            extra: Default::default(),
        }
    }

    fn cluster(members: Vec<usize>) -> Cluster {
        Cluster {
            bounds: Rectangle::new(Point::ORIGIN, Size::new(10.0, 10.0)),
            text: String::new(),
            font_size: 10.0,
            image_id: None,
            members,
        }
    }

    #[test]
    fn the_anchor_is_the_topmost_then_leftmost_member() {
        // Deliberately not in member order: the reference has to come from the
        // geometry, or it would change when a recluster reorders the list.
        let nodes = vec![node(300.0, 40.0), node(120.0, 40.0), node(200.0, 10.0)];
        let clusters = vec![cluster(vec![0, 1, 2])];
        assert_eq!(cluster_anchor(&clusters, &nodes, 0), Some(2));

        // Level with each other: the leftmost breaks the tie.
        let nodes = vec![node(300.0, 40.0), node(120.0, 40.0)];
        let clusters = vec![cluster(vec![0, 1])];
        assert_eq!(cluster_anchor(&clusters, &nodes, 0), Some(1));
    }

    #[test]
    fn an_anchor_is_only_offered_when_there_is_one_to_drag_by() {
        let nodes = vec![node(0.0, 0.0)];
        // A cluster with no members, and a member index left over from a
        // document that has since shrunk — neither should be dragged by.
        assert_eq!(cluster_anchor(&[cluster(Vec::new())], &nodes, 0), None);
        assert_eq!(cluster_anchor(&[cluster(vec![7])], &nodes, 0), None);
        // And a cluster index that doesn't exist.
        assert_eq!(cluster_anchor(&[], &nodes, 0), None);
    }
}

#[cfg(test)]
mod cluster_snap_tests {
    use super::cluster_delta;
    use iced::{Point, Vector};

    /// Where a member ends up: the anchor's new home plus its own offset.
    fn member_after(member: Point, delta: Vector) -> Point {
        Point::new(member.x + delta.x, member.y + delta.y)
    }

    fn on_grid(p: Point, grid: f32) -> bool {
        (p.x / grid).fract().abs() < 1e-4 && (p.y / grid).fract().abs() < 1e-4
    }

    #[test]
    fn dragging_a_cluster_leaves_its_members_on_the_grid() {
        // The reason cluster snapping exists: a cluster that was aligned must
        // still be aligned after being moved, however far the cursor wandered.
        const GRID: f32 = 20.0;
        let anchor = Point::new(40.0, 20.0);
        let members = [anchor, Point::new(260.0, 20.0), Point::new(40.0, 180.0)];
        assert!(members.iter().all(|&m| on_grid(m, GRID)));

        for (dx, dy) in [(3.0, 1.0), (11.0, -7.0), (-58.0, 132.0), (0.5, 0.5)] {
            let want = Point::new(anchor.x + dx, anchor.y + dy);
            let delta = cluster_delta(want, anchor, GRID);
            for &m in &members {
                let moved = member_after(m, delta);
                assert!(on_grid(moved, GRID), "{m:?} + {delta:?} = {moved:?} is off the grid");
            }
        }
    }

    #[test]
    fn snapping_off_follows_the_cursor_exactly() {
        // Grid 0 means the feature is off; the cluster must track the cursor
        // rather than quantising to anything.
        let anchor = Point::new(37.0, 23.0);
        let want = Point::new(101.5, 64.25);
        let delta = cluster_delta(want, anchor, 0.0);
        assert_eq!(delta, Vector::new(64.5, 41.25));
    }

    #[test]
    fn a_drag_inside_one_cell_asks_for_no_move() {
        // Which is what lets the caller skip publishing, so nudging the cursor
        // within a cell doesn't mark the document dirty.
        let anchor = Point::new(40.0, 20.0);
        let delta = cluster_delta(Point::new(44.0, 26.0), anchor, 20.0);
        assert_eq!(delta, Vector::ZERO);
    }

}
