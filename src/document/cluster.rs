//! Semantic-zoom cluster cards (the render half of clustering).
//!
//! When zoomed out far enough that individual notes are unreadable, a group
//! of nearby notes is replaced by one large summary card showing the group's
//! main note, at a font size fitted to the card's box so it stays comfortable
//! to read. Computed once up front — nothing is recomputed while zooming.
//!
//! The grouping itself (proximity, containment, main-note choice, cross-group
//! links) is UI-agnostic and lives in [`thinkboard_core::cluster`]. This module
//! wraps its bare bounds in `iced::Rectangle` and adds what needs the text
//! rasterizer: fitting a font size to the box, and choosing between a text and
//! an image summary.

use iced::{Point, Rectangle, Size};

use crate::model::{CanvasEdge, CanvasNode};
use crate::text_raster;
use crate::theme::Theme;

pub use thinkboard_core::cluster::Link as ClusterLink;

/// Text inset (world px, per side) within the cluster card.
pub const PADDING: f32 = 34.0;

#[derive(Debug, Clone)]
pub struct Clustering {
    pub clusters: Vec<Cluster>,
    /// Directed links between connected clusters.
    pub links: Vec<ClusterLink>,
}

#[derive(Debug, Clone)]
pub struct Cluster {
    pub bounds: Rectangle,
    /// Main-note text; empty when the main note is an image.
    pub text: String,
    /// Font size in world units, fitted to `bounds`.
    pub font_size: f32,
    /// Node id when the main note is an image, to render it large instead.
    pub image_id: Option<String>,
    /// Member node indices, for dragging the whole cluster.
    pub members: Vec<usize>,
}

/// Groups nearby notes and summarizes each with its main note, rendered either
/// as text fitted to the card or as the main note's image.
pub fn compute(nodes: &[CanvasNode], edges: &[CanvasEdge], theme: &Theme) -> Clustering {
    let grouping = thinkboard_core::cluster::group(nodes, edges);

    let clusters = grouping
        .groups
        .into_iter()
        .map(|group| {
            let (x, y, width, height) = group.bounds;
            let bounds = Rectangle::new(Point::new(x, y), Size::new(width, height));
            let node = &nodes[group.main];

            if node.kind == "file" {
                Cluster {
                    bounds,
                    text: String::new(),
                    font_size: 0.0,
                    image_id: Some(node.id.clone()),
                    members: group.members,
                }
            } else {
                let text = node.text.as_deref().unwrap_or("").trim().to_owned();
                let font_size = fit_font_size(&text, bounds, theme);
                Cluster {
                    bounds,
                    text,
                    font_size,
                    image_id: None,
                    members: group.members,
                }
            }
        })
        .collect();

    Clustering {
        clusters,
        links: grouping.links,
    }
}

/// Largest font (world units) whose wrapped text fits the padded box —
/// bigger boxes get proportionally bigger, readable text. Measures the
/// *rendered* Markdown with the theme font, matching how the cluster is drawn,
/// so the fitted size never overflows (measuring with a narrower fallback font
/// would let wider theme text spill onto an extra line).
fn fit_font_size(text: &str, bounds: Rectangle, theme: &Theme) -> f32 {
    if text.is_empty() {
        return 16.0;
    }
    let avail_w = (bounds.width - PADDING * 2.0).max(20.0);
    let avail_h = (bounds.height - PADDING * 2.0).max(20.0);

    let mut lo = 8.0;
    let mut hi = avail_h;
    for _ in 0..22 {
        let mid = (lo + hi) / 2.0;
        let (w, h) = text_raster::measure_markdown(text, avail_w, mid, theme);
        if w <= avail_w && h <= avail_h {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    lo
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CanvasEdge, CanvasNode};

    fn text_node(id: &str, x: f32, y: f32, w: f32, h: f32) -> CanvasNode {
        CanvasNode {
            id: id.to_owned(),
            kind: "text".to_owned(),
            text: Some(format!("node {id}")),
            file: None,
            x,
            y,
            width: w,
            height: h,
            color: None,
            extra: Default::default(),
        }
    }

    fn file_node(id: &str, x: f32, y: f32, w: f32, h: f32) -> CanvasNode {
        CanvasNode {
            id: id.to_owned(),
            kind: "file".to_owned(),
            text: None,
            file: Some(format!("{id}.png")),
            x,
            y,
            width: w,
            height: h,
            color: None,
            extra: Default::default(),
        }
    }

    fn edge(from: &str, to: &str) -> CanvasEdge {
        CanvasEdge {
            id: format!("{from}->{to}"),
            from_node: from.to_owned(),
            from_side: "right".to_owned(),
            to_node: to.to_owned(),
            to_side: "left".to_owned(),
            extra: Default::default(),
        }
    }

    #[test]
    fn the_card_is_summarized_by_its_main_note() {
        // Chain a -> b -> c in one cluster; the root `a` names it.
        let nodes = vec![
            text_node("a", 0.0, 0.0, 100.0, 100.0),
            text_node("b", 150.0, 0.0, 100.0, 100.0),
            text_node("c", 300.0, 0.0, 100.0, 100.0),
        ];
        let edges = vec![edge("a", "b"), edge("b", "c")];
        let clustering = compute(&nodes, &edges, &Theme::default());
        assert_eq!(clustering.clusters.len(), 1);
        assert_eq!(clustering.clusters[0].text, "node a");
    }

    #[test]
    fn image_cluster_carries_the_image_not_text() {
        let nodes = vec![file_node("img", 0.0, 0.0, 400.0, 300.0)];
        let clustering = compute(&nodes, &[], &Theme::default());
        assert_eq!(clustering.clusters.len(), 1);
        assert_eq!(clustering.clusters[0].image_id.as_deref(), Some("img"));
        assert!(clustering.clusters[0].text.is_empty());
    }

    #[test]
    fn text_cluster_gets_a_positive_font_size() {
        let nodes = vec![text_node("a", 0.0, 0.0, 300.0, 120.0)];
        let clustering = compute(&nodes, &[], &Theme::default());
        assert!(clustering.clusters[0].font_size > 0.0);
    }

    #[test]
    fn a_bigger_card_fits_a_bigger_font() {
        // The font is fitted to the box, so the same text in a larger cluster
        // renders larger — the point of the fit.
        let small = compute(&[text_node("a", 0.0, 0.0, 200.0, 100.0)], &[], &Theme::default());
        let large = compute(&[text_node("a", 0.0, 0.0, 800.0, 400.0)], &[], &Theme::default());
        assert!(large.clusters[0].font_size > small.clusters[0].font_size);
    }

    #[test]
    fn the_card_box_is_the_members_union_plus_a_margin() {
        // Two notes 50 apart join; the card spans both, expanded by MARGIN.
        let nodes = vec![
            text_node("a", 0.0, 0.0, 100.0, 100.0),
            text_node("b", 150.0, 0.0, 100.0, 100.0),
        ];
        let clustering = compute(&nodes, &[], &Theme::default());
        let margin = thinkboard_core::cluster::MARGIN;
        let bounds = clustering.clusters[0].bounds;
        assert_eq!(bounds.x, -margin);
        assert_eq!(bounds.width, 250.0 + margin * 2.0);
    }
}
