//! Semantic-zoom clustering (preprocessing).
//!
//! When zoomed out far enough that individual notes are unreadable, a group
//! of nearby notes is replaced by one large summary card showing the
//! cluster's main note, at a font size fitted to the cluster's bounding box
//! so it stays comfortable to read. Clustering is by spatial proximity
//! (single-linkage: notes within [`PROXIMITY`] of each other join, plus any
//! note falling within a cluster's bounding box), so a nearby note joins the
//! cluster whether or not it is connected by an edge. Computed once up front
//! — nothing is recomputed while zooming.

use std::collections::{HashMap, HashSet};

use iced::{Point, Rectangle, Size};

use crate::model::{CanvasEdge, CanvasNode};
use crate::text_raster;
use crate::theme::Theme;

/// Expansion (world px) of a cluster card beyond its members' extent.
const MARGIN: f32 = 24.0;
/// Text inset (world px, per side) within the cluster card.
pub const PADDING: f32 = 34.0;
/// Gap (world px) below which two notes are grouped into one cluster.
const PROXIMITY: f32 = 120.0;

#[derive(Debug, Clone)]
pub struct Clustering {
    pub clusters: Vec<Cluster>,
    /// Directed links between clusters that an edge crosses.
    pub links: Vec<ClusterLink>,
}

#[derive(Debug, Clone)]
pub struct ClusterLink {
    pub from: usize,
    pub to: usize,
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

/// Groups nearby notes (single-linkage by [`PROXIMITY`]) into clusters,
/// each summarized by its main note — the text root with the most edges,
/// else the highest-degree member.
pub fn compute(nodes: &[CanvasNode], edges: &[CanvasEdge], theme: &Theme) -> Clustering {
    let n = nodes.len();
    if n == 0 {
        return Clustering {
            clusters: Vec::new(),
            links: Vec::new(),
        };
    }

    // Edges only inform which note is a cluster's main one; grouping is
    // purely spatial.
    let index_of: HashMap<&str, usize> = nodes
        .iter()
        .enumerate()
        .map(|(i, node)| (node.id.as_str(), i))
        .collect();

    // Union any two notes whose rectangles are within PROXIMITY, using a
    // uniform grid (cell = PROXIMITY) so each note is compared only against
    // ones in its own and adjacent cells — not the whole canvas.
    let mut parent: Vec<usize> = (0..n).collect();
    let cell_of = |v: f32| (v / PROXIMITY).floor() as i64;
    let cells_of = |node: &CanvasNode| {
        (
            cell_of(node.x),
            cell_of(node.y),
            cell_of(node.x + node.width),
            cell_of(node.y + node.height),
        )
    };

    let mut grid: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
    for i in 0..n {
        let (cx0, cy0, cx1, cy1) = cells_of(&nodes[i]);
        for cx in cx0..=cx1 {
            for cy in cy0..=cy1 {
                grid.entry((cx, cy)).or_default().push(i);
            }
        }
    }

    for i in 0..n {
        let (cx0, cy0, cx1, cy1) = cells_of(&nodes[i]);
        for cx in (cx0 - 1)..=(cx1 + 1) {
            for cy in (cy0 - 1)..=(cy1 + 1) {
                let Some(bucket) = grid.get(&(cx, cy)) else {
                    continue;
                };
                for &j in bucket {
                    if j <= i {
                        continue;
                    }
                    if find(&mut parent, i) != find(&mut parent, j)
                        && rect_gap(&nodes[i], &nodes[j]) < PROXIMITY
                    {
                        let (ri, rj) = (find(&mut parent, i), find(&mut parent, j));
                        parent[ri] = rj;
                    }
                }
            }
        }
    }

    // Containment: absorb any note that falls within a cluster's bounding
    // box (e.g. one sitting in an interior gap wider than PROXIMITY).
    // Absorbing grows the box, which can pull in more — so repeat until
    // stable; component count only shrinks, so this terminates.
    loop {
        let mut bounds: HashMap<usize, [f32; 4]> = HashMap::new();
        for i in 0..n {
            let r = find(&mut parent, i);
            let b = bounds
                .entry(r)
                .or_insert([f32::MAX, f32::MAX, f32::MIN, f32::MIN]);
            b[0] = b[0].min(nodes[i].x);
            b[1] = b[1].min(nodes[i].y);
            b[2] = b[2].max(nodes[i].x + nodes[i].width);
            b[3] = b[3].max(nodes[i].y + nodes[i].height);
        }

        let mut changed = false;
        for i in 0..n {
            let node = &nodes[i];
            for (&root, b) in &bounds {
                if find(&mut parent, i) == find(&mut parent, root) {
                    continue;
                }
                let overlaps = node.x < b[2]
                    && node.x + node.width > b[0]
                    && node.y < b[3]
                    && node.y + node.height > b[1];
                if overlaps {
                    let (ri, rr) = (find(&mut parent, i), find(&mut parent, root));
                    parent[ri] = rr;
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }

    // Assign each note a cluster index (grouped by union-find root).
    let mut root_to_cluster: HashMap<usize, usize> = HashMap::new();
    let mut members_by_cluster: Vec<Vec<usize>> = Vec::new();
    let mut node_cluster = vec![0usize; n];
    for i in 0..n {
        let root = find(&mut parent, i);
        let ci = *root_to_cluster.entry(root).or_insert_with(|| {
            members_by_cluster.push(Vec::new());
            members_by_cluster.len() - 1
        });
        members_by_cluster[ci].push(i);
        node_cluster[i] = ci;
    }

    // Edge counts restricted to within-cluster edges, so a link arriving
    // from another cluster does not disqualify a cluster's own head node.
    let mut incoming = vec![0u32; n];
    let mut outgoing = vec![0u32; n];
    for edge in edges {
        if let (Some(&a), Some(&b)) = (
            index_of.get(edge.from_node.as_str()),
            index_of.get(edge.to_node.as_str()),
        ) {
            if node_cluster[a] == node_cluster[b] {
                outgoing[a] += 1;
                incoming[b] += 1;
            }
        }
    }

    let clusters = members_by_cluster
        .iter()
        .map(|members| {
            let main = select_main(members, nodes, &incoming, &outgoing);
            let bounds = union_bounds(members, nodes);
            let node = &nodes[main];

            if node.kind == "file" {
                Cluster {
                    bounds,
                    text: String::new(),
                    font_size: 0.0,
                    image_id: Some(node.id.clone()),
                    members: members.clone(),
                }
            } else {
                let text = node.text.as_deref().unwrap_or("").trim().to_owned();
                let font_size = fit_font_size(&text, bounds, theme);
                Cluster {
                    bounds,
                    text,
                    font_size,
                    image_id: None,
                    members: members.clone(),
                }
            }
        })
        .collect();

    // An edge whose endpoints lie in different clusters becomes a directed
    // cluster link (deduplicated per ordered cluster pair).
    let mut seen: HashSet<(usize, usize)> = HashSet::new();
    let mut links = Vec::new();
    for edge in edges {
        if let (Some(&a), Some(&b)) = (
            index_of.get(edge.from_node.as_str()),
            index_of.get(edge.to_node.as_str()),
        ) {
            let (ca, cb) = (node_cluster[a], node_cluster[b]);
            if ca != cb && seen.insert((ca, cb)) {
                links.push(ClusterLink { from: ca, to: cb });
            }
        }
    }

    Clustering { clusters, links }
}

/// Nearest gap between two node rectangles (0 if they overlap).
fn rect_gap(a: &CanvasNode, b: &CanvasNode) -> f32 {
    let dx = (b.x - (a.x + a.width)).max(a.x - (b.x + b.width)).max(0.0);
    let dy = (b.y - (a.y + a.height)).max(a.y - (b.y + b.height)).max(0.0);
    (dx * dx + dy * dy).sqrt()
}

fn find(parent: &mut [usize], mut x: usize) -> usize {
    while parent[x] != x {
        parent[x] = parent[parent[x]];
        x = parent[x];
    }
    x
}

/// The cluster's main note: a text root (no incoming edges) with the most
/// outgoing edges; failing that the highest-degree text node; failing that
/// the first member.
fn select_main(
    members: &[usize],
    nodes: &[CanvasNode],
    incoming: &[u32],
    outgoing: &[u32],
) -> usize {
    let text: Vec<usize> = members
        .iter()
        .copied()
        .filter(|&i| nodes[i].kind == "text")
        .collect();
    let pool = if text.is_empty() { members } else { &text };

    let roots: Vec<usize> = pool.iter().copied().filter(|&i| incoming[i] == 0).collect();
    let candidates = if roots.is_empty() { pool } else { &roots };

    candidates
        .iter()
        .copied()
        .max_by_key(|&i| outgoing[i] + incoming[i])
        .unwrap_or(members[0])
}

fn union_bounds(members: &[usize], nodes: &[CanvasNode]) -> Rectangle {
    let mut min = Point::new(f32::MAX, f32::MAX);
    let mut max = Point::new(f32::MIN, f32::MIN);
    for &i in members {
        let node = &nodes[i];
        min.x = min.x.min(node.x);
        min.y = min.y.min(node.y);
        max.x = max.x.max(node.x + node.width);
        max.y = max.y.max(node.y + node.height);
    }
    Rectangle::new(
        Point::new(min.x - MARGIN, min.y - MARGIN),
        Size::new(max.x - min.x + MARGIN * 2.0, max.y - min.y + MARGIN * 2.0),
    )
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

    /// The cluster that contains the given node index.
    fn cluster_with(clustering: &Clustering, node: usize) -> &Cluster {
        clustering
            .clusters
            .iter()
            .find(|cluster| cluster.members.contains(&node))
            .expect("node should belong to a cluster")
    }

    #[test]
    fn empty_document_has_no_clusters() {
        let clustering = compute(&[], &[], &Theme::default());
        assert!(clustering.clusters.is_empty());
        assert!(clustering.links.is_empty());
    }

    #[test]
    fn distant_nodes_form_separate_clusters() {
        let nodes = vec![
            text_node("a", 0.0, 0.0, 100.0, 100.0),
            text_node("b", 1000.0, 0.0, 100.0, 100.0),
        ];
        assert_eq!(compute(&nodes, &[], &Theme::default()).clusters.len(), 2);
    }

    #[test]
    fn nearby_nodes_merge_by_proximity() {
        // Gap of 50 (< PROXIMITY) so the two notes join.
        let nodes = vec![
            text_node("a", 0.0, 0.0, 100.0, 100.0),
            text_node("b", 150.0, 0.0, 100.0, 100.0),
        ];
        assert_eq!(compute(&nodes, &[], &Theme::default()).clusters.len(), 1);
    }

    #[test]
    fn containment_absorbs_interior_node() {
        // An L-shaped chain whose bounding box has a hollow interior, plus a
        // stray node inside it but > PROXIMITY from every member. Proximity
        // alone leaves it out; containment pulls it in.
        let nodes = vec![
            text_node("a", 0.0, 0.0, 100.0, 100.0),
            text_node("b", 150.0, 0.0, 100.0, 100.0),
            text_node("c", 300.0, 0.0, 100.0, 100.0),
            text_node("d", 300.0, 150.0, 100.0, 100.0),
            text_node("e", 300.0, 300.0, 100.0, 100.0),
            text_node("stray", 30.0, 300.0, 100.0, 100.0),
        ];
        let clustering = compute(&nodes, &[], &Theme::default());
        assert_eq!(clustering.clusters.len(), 1);
        assert_eq!(clustering.clusters[0].members.len(), 6);
    }

    #[test]
    fn main_note_is_the_text_root() {
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
    fn external_edge_does_not_steal_the_head() {
        // x (its own cluster) points at a, the head of a separate a->b->c
        // cluster. Root detection is intra-cluster, so a stays the head:
        // with global edge counts, the a/b degree tie would pick b instead.
        let nodes = vec![
            text_node("a", 0.0, 0.0, 100.0, 100.0),
            text_node("b", 150.0, 0.0, 100.0, 100.0),
            text_node("c", 300.0, 0.0, 100.0, 100.0),
            text_node("x", 1000.0, 1000.0, 100.0, 100.0),
        ];
        let edges = vec![edge("a", "b"), edge("b", "c"), edge("x", "a")];
        let clustering = compute(&nodes, &edges, &Theme::default());

        assert_eq!(clustering.clusters.len(), 2);
        assert_eq!(cluster_with(&clustering, 0).text, "node a");
    }

    #[test]
    fn cross_cluster_edge_becomes_one_link() {
        let nodes = vec![
            text_node("a", 0.0, 0.0, 100.0, 100.0),
            text_node("b", 150.0, 0.0, 100.0, 100.0),
            text_node("x", 1000.0, 1000.0, 100.0, 100.0),
        ];
        // Two edges from x into the a-b cluster collapse to a single link.
        let edges = vec![edge("a", "b"), edge("x", "a"), edge("x", "b")];
        let clustering = compute(&nodes, &edges, &Theme::default());

        assert_eq!(clustering.links.len(), 1);
        let link = &clustering.links[0];
        assert!(clustering.clusters[link.from].members.contains(&2)); // x
        assert!(clustering.clusters[link.to].members.contains(&0)); // a
    }

    #[test]
    fn intra_cluster_edges_produce_no_links() {
        let nodes = vec![
            text_node("a", 0.0, 0.0, 100.0, 100.0),
            text_node("b", 150.0, 0.0, 100.0, 100.0),
        ];
        let clustering = compute(&nodes, &[edge("a", "b")], &Theme::default());
        assert!(clustering.links.is_empty());
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
}
