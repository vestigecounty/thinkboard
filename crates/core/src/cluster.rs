//! Spatial grouping of canvas notes — the pure half of semantic-zoom clustering.
//!
//! Groups notes by proximity (single-linkage: notes within [`PROXIMITY`] of
//! each other join, plus any note falling within a group's bounding box), picks
//! the note that summarizes each group, and reports which edges cross between
//! groups. Grouping is purely spatial: a nearby note joins whether or not an
//! edge connects it. Edges only decide *which* member is the main one.
//!
//! Rendering the result — fitting a font size to the box, choosing between a
//! text and an image summary — needs a text rasterizer, so it stays in the
//! desktop crate (`document::cluster`), which calls this and wraps the bare
//! bounds in its UI toolkit's rectangle type.

use std::collections::{HashMap, HashSet};

use crate::model::{CanvasEdge, CanvasNode};

/// An axis-aligned world-space box, `(x, y, width, height)`. Bare tuples keep
/// this crate free of any UI toolkit's rectangle type; the desktop wraps them
/// back into `iced::Rectangle` at the call site.
pub type Bounds = (f32, f32, f32, f32);

/// Expansion (world px) of a group's box beyond its members' extent.
pub const MARGIN: f32 = 24.0;
/// Gap (world px) below which two notes are grouped together.
const PROXIMITY: f32 = 120.0;

#[derive(Debug, Clone)]
pub struct Grouping {
    pub groups: Vec<Group>,
    /// Directed links between groups that an edge crosses.
    pub links: Vec<Link>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub from: usize,
    pub to: usize,
}

#[derive(Debug, Clone)]
pub struct Group {
    /// The members' union, expanded by [`MARGIN`].
    pub bounds: Bounds,
    /// Index of the member that summarizes the group (see [`select_main`]).
    pub main: usize,
    /// Member node indices, for dragging the whole group.
    pub members: Vec<usize>,
}

/// Groups nearby notes (single-linkage by [`PROXIMITY`]) and picks each
/// group's main note — the text root with the most edges, else the
/// highest-degree member.
pub fn group(nodes: &[CanvasNode], edges: &[CanvasEdge]) -> Grouping {
    let n = nodes.len();
    if n == 0 {
        return Grouping {
            groups: Vec::new(),
            links: Vec::new(),
        };
    }

    let index_of: HashMap<&str, usize> = nodes
        .iter()
        .enumerate()
        .map(|(i, node)| (node.id.as_str(), i))
        .collect();

    let mut parent: Vec<usize> = (0..n).collect();
    union_by_proximity(nodes, &mut parent);
    absorb_contained(nodes, &mut parent);

    // Assign each note a group index (grouped by union-find root).
    let mut root_to_group: HashMap<usize, usize> = HashMap::new();
    let mut members_by_group: Vec<Vec<usize>> = Vec::new();
    let mut node_group = vec![0usize; n];
    for i in 0..n {
        let root = find(&mut parent, i);
        let gi = *root_to_group.entry(root).or_insert_with(|| {
            members_by_group.push(Vec::new());
            members_by_group.len() - 1
        });
        members_by_group[gi].push(i);
        node_group[i] = gi;
    }

    // Edge counts restricted to within-group edges, so a link arriving from
    // another group does not disqualify a group's own head node.
    let mut incoming = vec![0u32; n];
    let mut outgoing = vec![0u32; n];
    for edge in edges {
        if let (Some(&a), Some(&b)) = (
            index_of.get(edge.from_node.as_str()),
            index_of.get(edge.to_node.as_str()),
        ) && node_group[a] == node_group[b]
        {
            outgoing[a] += 1;
            incoming[b] += 1;
        }
    }

    let groups = members_by_group
        .iter()
        .map(|members| Group {
            bounds: union_bounds(members, nodes),
            main: select_main(members, nodes, &incoming, &outgoing),
            members: members.clone(),
        })
        .collect();

    // An edge whose endpoints lie in different groups becomes a directed link
    // (deduplicated per ordered pair).
    let mut seen: HashSet<(usize, usize)> = HashSet::new();
    let mut links = Vec::new();
    for edge in edges {
        if let (Some(&a), Some(&b)) = (
            index_of.get(edge.from_node.as_str()),
            index_of.get(edge.to_node.as_str()),
        ) {
            let (ga, gb) = (node_group[a], node_group[b]);
            if ga != gb && seen.insert((ga, gb)) {
                links.push(Link { from: ga, to: gb });
            }
        }
    }

    Grouping { groups, links }
}

/// Unions any two notes whose rectangles are within [`PROXIMITY`], using a
/// uniform grid (cell = `PROXIMITY`) so each note is compared only against ones
/// in its own and adjacent cells — not the whole canvas.
fn union_by_proximity(nodes: &[CanvasNode], parent: &mut [usize]) {
    let n = nodes.len();
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
                    if find(parent, i) != find(parent, j)
                        && rect_gap(&nodes[i], &nodes[j]) < PROXIMITY
                    {
                        let (ri, rj) = (find(parent, i), find(parent, j));
                        parent[ri] = rj;
                    }
                }
            }
        }
    }
}

/// Absorbs any note that falls within a group's bounding box (e.g. one sitting
/// in an interior gap wider than [`PROXIMITY`]). Absorbing grows the box, which
/// can pull in more — so repeat until stable; the component count only shrinks,
/// so this terminates.
fn absorb_contained(nodes: &[CanvasNode], parent: &mut [usize]) {
    let n = nodes.len();
    loop {
        let mut bounds: HashMap<usize, [f32; 4]> = HashMap::new();
        for i in 0..n {
            let r = find(parent, i);
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
                if find(parent, i) == find(parent, root) {
                    continue;
                }
                let overlaps = node.x < b[2]
                    && node.x + node.width > b[0]
                    && node.y < b[3]
                    && node.y + node.height > b[1];
                if overlaps {
                    let (ri, rr) = (find(parent, i), find(parent, root));
                    parent[ri] = rr;
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
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

/// The group's main note: a text root (no incoming edges) with the most
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

/// The members' union expanded by [`MARGIN`] on every side.
fn union_bounds(members: &[usize], nodes: &[CanvasNode]) -> Bounds {
    let (mut min_x, mut min_y) = (f32::MAX, f32::MAX);
    let (mut max_x, mut max_y) = (f32::MIN, f32::MIN);
    for &i in members {
        let node = &nodes[i];
        min_x = min_x.min(node.x);
        min_y = min_y.min(node.y);
        max_x = max_x.max(node.x + node.width);
        max_y = max_y.max(node.y + node.height);
    }
    (
        min_x - MARGIN,
        min_y - MARGIN,
        max_x - min_x + MARGIN * 2.0,
        max_y - min_y + MARGIN * 2.0,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Map;

    fn node(id: &str, kind: &str, x: f32, y: f32, w: f32, h: f32) -> CanvasNode {
        CanvasNode {
            id: id.to_owned(),
            kind: kind.to_owned(),
            text: Some(format!("node {id}")),
            file: None,
            x,
            y,
            width: w,
            height: h,
            color: None,
            extra: Map::new(),
        }
    }

    fn text_node(id: &str, x: f32, y: f32, w: f32, h: f32) -> CanvasNode {
        node(id, "text", x, y, w, h)
    }

    fn edge(from: &str, to: &str) -> CanvasEdge {
        CanvasEdge {
            id: format!("{from}->{to}"),
            from_node: from.to_owned(),
            from_side: "right".to_owned(),
            to_node: to.to_owned(),
            to_side: "left".to_owned(),
            extra: Map::new(),
        }
    }

    /// The group that contains the given node index.
    fn group_with(grouping: &Grouping, node: usize) -> &Group {
        grouping
            .groups
            .iter()
            .find(|group| group.members.contains(&node))
            .expect("node should belong to a group")
    }

    #[test]
    fn empty_document_has_no_groups() {
        let grouping = group(&[], &[]);
        assert!(grouping.groups.is_empty());
        assert!(grouping.links.is_empty());
    }

    #[test]
    fn distant_nodes_form_separate_groups() {
        let nodes = vec![
            text_node("a", 0.0, 0.0, 100.0, 100.0),
            text_node("b", 1000.0, 0.0, 100.0, 100.0),
        ];
        assert_eq!(group(&nodes, &[]).groups.len(), 2);
    }

    #[test]
    fn nearby_nodes_merge_by_proximity() {
        // Gap of 50 (< PROXIMITY) so the two notes join.
        let nodes = vec![
            text_node("a", 0.0, 0.0, 100.0, 100.0),
            text_node("b", 150.0, 0.0, 100.0, 100.0),
        ];
        assert_eq!(group(&nodes, &[]).groups.len(), 1);
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
        let grouping = group(&nodes, &[]);
        assert_eq!(grouping.groups.len(), 1);
        assert_eq!(grouping.groups[0].members.len(), 6);
    }

    #[test]
    fn main_note_is_the_text_root() {
        // Chain a -> b -> c in one group; the root `a` names it.
        let nodes = vec![
            text_node("a", 0.0, 0.0, 100.0, 100.0),
            text_node("b", 150.0, 0.0, 100.0, 100.0),
            text_node("c", 300.0, 0.0, 100.0, 100.0),
        ];
        let edges = vec![edge("a", "b"), edge("b", "c")];
        let grouping = group(&nodes, &edges);
        assert_eq!(grouping.groups.len(), 1);
        assert_eq!(grouping.groups[0].main, 0);
    }

    #[test]
    fn external_edge_does_not_steal_the_head() {
        // x (its own group) points at a, the head of a separate a->b->c group.
        // Root detection is intra-group, so a stays the head: with global edge
        // counts, the a/b degree tie would pick b instead.
        let nodes = vec![
            text_node("a", 0.0, 0.0, 100.0, 100.0),
            text_node("b", 150.0, 0.0, 100.0, 100.0),
            text_node("c", 300.0, 0.0, 100.0, 100.0),
            text_node("x", 1000.0, 1000.0, 100.0, 100.0),
        ];
        let edges = vec![edge("a", "b"), edge("b", "c"), edge("x", "a")];
        let grouping = group(&nodes, &edges);

        assert_eq!(grouping.groups.len(), 2);
        assert_eq!(group_with(&grouping, 0).main, 0);
    }

    #[test]
    fn an_image_only_group_still_picks_a_main() {
        let nodes = vec![node("img", "file", 0.0, 0.0, 400.0, 300.0)];
        let grouping = group(&nodes, &[]);
        assert_eq!(grouping.groups.len(), 1);
        assert_eq!(grouping.groups[0].main, 0);
    }

    #[test]
    fn cross_group_edge_becomes_one_link() {
        let nodes = vec![
            text_node("a", 0.0, 0.0, 100.0, 100.0),
            text_node("b", 150.0, 0.0, 100.0, 100.0),
            text_node("x", 1000.0, 1000.0, 100.0, 100.0),
        ];
        // Two edges from x into the a-b group collapse to a single link.
        let edges = vec![edge("a", "b"), edge("x", "a"), edge("x", "b")];
        let grouping = group(&nodes, &edges);

        assert_eq!(grouping.links.len(), 1);
        let link = &grouping.links[0];
        assert!(grouping.groups[link.from].members.contains(&2)); // x
        assert!(grouping.groups[link.to].members.contains(&0)); // a
    }

    #[test]
    fn intra_group_edges_produce_no_links() {
        let nodes = vec![
            text_node("a", 0.0, 0.0, 100.0, 100.0),
            text_node("b", 150.0, 0.0, 100.0, 100.0),
        ];
        let grouping = group(&nodes, &[edge("a", "b")]);
        assert!(grouping.links.is_empty());
    }

    #[test]
    fn bounds_expand_the_members_union_by_the_margin() {
        let nodes = vec![text_node("a", 10.0, 20.0, 100.0, 50.0)];
        let (x, y, w, h) = group(&nodes, &[]).groups[0].bounds;
        assert_eq!((x, y), (10.0 - MARGIN, 20.0 - MARGIN));
        assert_eq!((w, h), (100.0 + MARGIN * 2.0, 50.0 + MARGIN * 2.0));
    }
}
