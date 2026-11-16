//! Mermaid-style layered ("Distribute") auto-layout for a set of nodes.
//!
//! A dagre-lite layered layout, top to bottom: nodes are assigned to ranks by
//! longest path along the edges among them, ordered within each rank in the
//! selection's order, and given fresh coordinates. Only edges whose endpoints
//! are both in the set count; isolated nodes fall on rank 0. The result is
//! recentered on the selection's original centroid, so it lands roughly where
//! it was rather than jumping to the origin.
//!
//! [`framed_layout`] is the same over groups taken whole: a frame and what it
//! encloses are laid out as one node, and move together.

use std::collections::{HashMap, HashSet};

use crate::model::{self, CanvasEdge, CanvasNode};

/// Flow direction of the layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Top to bottom: ranks stack vertically, nodes spread horizontally.
    Down,
    /// Left to right: ranks stack horizontally, nodes spread vertically.
    Right,
}

/// New top-left positions `(node_index, x, y)` for each node in `selected`,
/// laid out as a layered graph flowing in `dir`. `selected` indexes into
/// `nodes`.
pub fn layered_layout(
    nodes: &[CanvasNode],
    edges: &[CanvasEdge],
    selected: &[usize],
    dir: Direction,
    rank_gap: f32,
    node_gap: f32,
) -> Vec<(usize, f32, f32)> {
    if selected.is_empty() {
        return Vec::new();
    }

    // Local ids 0..k for the selected nodes, and a node-id -> local map so
    // edges can be matched.
    let id_to_local: HashMap<&str, usize> = selected
        .iter()
        .enumerate()
        .map(|(local, &i)| (nodes[i].id.as_str(), local))
        .collect();

    let mut edge_list: Vec<(usize, usize)> = Vec::new();
    for edge in edges {
        if let (Some(&u), Some(&v)) = (
            id_to_local.get(edge.from_node.as_str()),
            id_to_local.get(edge.to_node.as_str()),
        ) && u != v
        {
            edge_list.push((u, v));
        }
    }
    place(nodes, selected, &edge_list, dir, rank_gap, node_gap)
}

/// [`layered_layout`] once its links are known: `edge_list` is between local
/// indices into `selected`, which is not empty.
fn place(
    nodes: &[CanvasNode],
    selected: &[usize],
    edge_list: &[(usize, usize)],
    dir: Direction,
    rank_gap: f32,
    node_gap: f32,
) -> Vec<(usize, f32, f32)> {
    let k = selected.len();

    // Longest-path ranking; the iteration cap tolerates cycles.
    let mut rank = vec![0usize; k];
    for _ in 0..k {
        let mut changed = false;
        for &(u, v) in edge_list {
            if rank[v] < rank[u] + 1 {
                rank[v] = rank[u] + 1;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    // Group into ranks, initially in the selection's order.
    let max_rank = rank.iter().copied().max().unwrap_or(0);
    let mut layers: Vec<Vec<usize>> = vec![Vec::new(); max_rank + 1];
    for local in 0..k {
        layers[rank[local]].push(local);
    }

    // Barycenter crossing reduction: sweep down then up, reordering each rank
    // toward the average position of its neighbors in the adjacent rank.
    let mut preds: Vec<Vec<usize>> = vec![Vec::new(); k];
    let mut succs: Vec<Vec<usize>> = vec![Vec::new(); k];
    for &(u, v) in edge_list {
        succs[u].push(v);
        preds[v].push(u);
    }
    let mut pos = vec![0usize; k];
    for layer in &layers {
        for (i, &local) in layer.iter().enumerate() {
            pos[local] = i;
        }
    }
    for iteration in 0..8 {
        let down = iteration % 2 == 0;
        let ranks: Vec<usize> = if down {
            (1..layers.len()).collect()
        } else {
            (0..layers.len().saturating_sub(1)).rev().collect()
        };
        for r in ranks {
            let neighbors = if down { &preds } else { &succs };
            order_by_barycenter(&mut layers[r], &mut pos, neighbors);
        }
    }

    let width = |local: usize| nodes[selected[local]].width;
    let height = |local: usize| nodes[selected[local]].height;
    let down = dir == Direction::Down;
    // Node extent along the flow (rank) axis and across it.
    let along = |l: usize| if down { height(l) } else { width(l) };
    let across = |l: usize| if down { width(l) } else { height(l) };

    // Center of each rank along the flow axis, stacked by the thickest member.
    let mut rank_along = vec![0.0f32; layers.len()];
    let mut a = 0.0;
    for (r, layer) in layers.iter().enumerate() {
        let thickness = layer.iter().map(|&l| along(l)).fold(0.0, f32::max);
        rank_along[r] = a + thickness / 2.0;
        a += thickness + rank_gap;
    }

    // Positions across each rank, centered on 0.
    let mut center = vec![(0.0f32, 0.0f32); k];
    for (r, layer) in layers.iter().enumerate() {
        let total: f32 = layer.iter().map(|&l| across(l)).sum::<f32>()
            + node_gap * layer.len().saturating_sub(1) as f32;
        let mut c = -total / 2.0;
        for &local in layer {
            let cross_center = c + across(local) / 2.0;
            center[local] = if down {
                (cross_center, rank_along[r])
            } else {
                (rank_along[r], cross_center)
            };
            c += across(local) + node_gap;
        }
    }

    // Shift so the new layout's centroid matches the old one.
    let orig_c = centroid(selected.iter().map(|&i| {
        (
            nodes[i].x + nodes[i].width / 2.0,
            nodes[i].y + nodes[i].height / 2.0,
        )
    }));
    let new_c = centroid(center.iter().copied());
    let (dx, dy) = (orig_c.0 - new_c.0, orig_c.1 - new_c.1);

    (0..k)
        .map(|local| {
            let i = selected[local];
            let (cx, cy) = center[local];
            (
                i,
                cx + dx - nodes[i].width / 2.0,
                cy + dy - nodes[i].height / 2.0,
            )
        })
        .collect()
}

/// What [`framed_layout`] decided: where everything goes, and the frame it
/// happened in.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Framed {
    /// New top-left `(node_index, x, y)` of every node that moves: each unit,
    /// and whatever a frame among them encloses, by that frame's delta.
    pub moves: Vec<(usize, f32, f32)>,
    /// The unit each of `moves` moved as — itself, or the frame that carried
    /// it — in the same order. A link between two nodes of one unit kept its
    /// shape, so it keeps its ports.
    pub units: Vec<usize>,
    /// The frame enclosing the whole selection, which the layout happened
    /// inside. It's the caller's to grow around the result, or what spread
    /// past its edge would leave the group.
    pub container: Option<usize>,
}

/// [`layered_layout`] with each group taken whole, as a drag takes it.
///
/// Membership of a group is positional (see [`model::enclosed_by`]), so laid
/// out card by card a group's cards left its rect and the frame stayed put.
/// Instead the layout happens one level below the selection's *container* —
/// the innermost frame enclosing all of it, or the board — and each selected
/// node stands for the outermost frame below the container that encloses it,
/// or for itself. One of a group's cards is enough to bring the group. A frame
/// is laid out as one node the size of its rect; a link to or from anything it
/// encloses is the frame's, a link inside it drops out, and everything inside
/// moves by the frame's delta, so it stays as it was arranged. Arranging the
/// inside is a second step: select only its cards, and the frame is then the
/// container.
pub fn framed_layout(
    nodes: &[CanvasNode],
    edges: &[CanvasEdge],
    selected: &[usize],
    dir: Direction,
    rank_gap: f32,
    node_gap: f32,
) -> Framed {
    if selected.is_empty() {
        return Framed::default();
    }
    let encloses = |frame: usize, i: usize| frame != i && model::centre_within(&nodes[i], &nodes[frame]);
    let area = |i: usize| nodes[i].width * nodes[i].height;
    let frames: Vec<usize> = (0..nodes.len()).filter(|&i| nodes[i].is_group()).collect();

    let container = frames
        .iter()
        .copied()
        .filter(|&frame| selected.iter().all(|&i| encloses(frame, i)))
        .min_by(|&a, &b| area(a).total_cmp(&area(b)));
    // The frames that can stand for a card: those inside the container.
    let below: Vec<usize> = frames
        .iter()
        .copied()
        .filter(|&frame| container.is_none_or(|c| encloses(c, frame)))
        .collect();
    let unit_of = |i: usize| {
        below
            .iter()
            .copied()
            .filter(|&frame| encloses(frame, i))
            .max_by(|&a, &b| area(a).total_cmp(&area(b)))
            .unwrap_or(i)
    };

    // Units in the selection's order, which is the layout's starting order.
    let mut units: Vec<usize> = Vec::new();
    let mut claimed: HashSet<usize> = HashSet::new();
    for &i in selected {
        let unit = unit_of(i);
        if claimed.insert(unit) {
            units.push(unit);
        }
    }
    // What each carries. Frames that overlap without nesting can both enclose
    // a node; the first to claim it takes it, so nothing moves twice.
    let carried: Vec<Vec<usize>> = units
        .iter()
        .map(|&unit| {
            let mut members = vec![unit];
            if nodes[unit].is_group() {
                members.extend((0..nodes.len()).filter(|&j| encloses(unit, j) && claimed.insert(j)));
            }
            members
        })
        .collect();

    let local_of: HashMap<&str, usize> = carried
        .iter()
        .enumerate()
        .flat_map(|(local, members)| members.iter().map(move |&j| (nodes[j].id.as_str(), local)))
        .collect();
    let edge_list: Vec<(usize, usize)> = edges
        .iter()
        .filter_map(|edge| Some((*local_of.get(edge.from_node.as_str())?, *local_of.get(edge.to_node.as_str())?)))
        .filter(|(u, v)| u != v)
        .collect();

    let mut framed = Framed { container, ..Framed::default() };
    for (local, (unit, x, y)) in place(nodes, &units, &edge_list, dir, rank_gap, node_gap).into_iter().enumerate() {
        let (dx, dy) = (x - nodes[unit].x, y - nodes[unit].y);
        for &j in &carried[local] {
            framed.moves.push((j, nodes[j].x + dx, nodes[j].y + dy));
            framed.units.push(unit);
        }
    }
    framed
}

/// The `(from_side, to_side)` to re-anchor a link to after a relayout, biased
/// to the flow axis: a cross-rank link routes along `dir` (so in a top-down
/// layout every incoming edge enters the top and every outgoing edge leaves
/// the bottom — never sharing a port and looking bidirectional); only nodes on
/// the same rank connect across the flow.
pub fn facing_sides(
    from: (f32, f32),
    to: (f32, f32),
    dir: Direction,
) -> (&'static str, &'static str) {
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    let vertical = if dy >= 0.0 {
        ("bottom", "top")
    } else {
        ("top", "bottom")
    };
    let horizontal = if dx >= 0.0 {
        ("right", "left")
    } else {
        ("left", "right")
    };
    match dir {
        // Different ranks flow along the axis; same-rank links go across it.
        Direction::Down if dy.abs() >= 1.0 => vertical,
        Direction::Down => horizontal,
        Direction::Right if dx.abs() >= 1.0 => horizontal,
        Direction::Right => vertical,
    }
}

/// Top-left targets for `children` (each `(width, height)`) laid out as a
/// single rank of siblings hanging off `source` (its top-left `(x, y)` and
/// `(width, height)`), centered on the source's cross axis.
///
/// For [`Direction::Right`] the children form a vertical column to the
/// source's right; for [`Direction::Down`], a horizontal row below it.
/// `rank_gap` separates the children from the source along the flow axis,
/// `node_gap` separates adjacent siblings. Unlike [`layered_layout`], the
/// source itself is never moved — only positions for the children are returned.
pub fn sibling_stack(
    source: (f32, f32, f32, f32),
    children: &[(f32, f32)],
    dir: Direction,
    rank_gap: f32,
    node_gap: f32,
) -> Vec<(f32, f32)> {
    let (sx, sy, sw, sh) = source;
    let gaps = node_gap * children.len().saturating_sub(1) as f32;
    let mut out = Vec::with_capacity(children.len());
    match dir {
        Direction::Down => {
            let y = sy + sh + rank_gap;
            let total: f32 = children.iter().map(|&(w, _)| w).sum::<f32>() + gaps;
            let mut x = sx + sw / 2.0 - total / 2.0;
            for &(w, _) in children {
                out.push((x, y));
                x += w + node_gap;
            }
        }
        Direction::Right => {
            let x = sx + sw + rank_gap;
            let total: f32 = children.iter().map(|&(_, h)| h).sum::<f32>() + gaps;
            let mut y = sy + sh / 2.0 - total / 2.0;
            for &(_, h) in children {
                out.push((x, y));
                y += h + node_gap;
            }
        }
    }
    out
}

/// Reorders one rank so each node sits near the average position of its
/// `neighbors` in the adjacent rank; nodes with no neighbors keep their spot.
/// Updates `pos` (each node's index within its rank).
///
/// A node with no neighbors there has no average to sort by, and keeps its
/// slot while the others are sorted into the rest, as dagre does. It used to
/// be sorted by its own index in this rank, against the others' averages of
/// indices in the next — two scales in one sort: a day with no plans yet,
/// added last beside three days of three plans each, came second (3 against
/// 1, 4 and 7).
fn order_by_barycenter(layer: &mut [usize], pos: &mut [usize], neighbors: &[Vec<usize>]) {
    let barycenter = |local: usize| {
        let ns = &neighbors[local];
        ns.iter().map(|&n| pos[n] as f32).sum::<f32>() / ns.len() as f32
    };
    // In rank order, so a stable sort leaves ties as they were.
    let mut sorted: Vec<(f32, usize)> = layer
        .iter()
        .filter(|&&local| !neighbors[local].is_empty())
        .map(|&local| (barycenter(local), local))
        .collect();
    sorted.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut sorted = sorted.into_iter().map(|(_, local)| local);
    for slot in layer.iter_mut() {
        if !neighbors[*slot].is_empty() {
            *slot = sorted.next().expect("one sorted node per sortable slot");
        }
    }
    for (i, &local) in layer.iter().enumerate() {
        pos[local] = i;
    }
}

fn centroid(points: impl Iterator<Item = (f32, f32)>) -> (f32, f32) {
    let (mut sx, mut sy, mut n) = (0.0, 0.0, 0.0);
    for (x, y) in points {
        sx += x;
        sy += y;
        n += 1.0;
    }
    if n == 0.0 {
        (0.0, 0.0)
    } else {
        (sx / n, sy / n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures;

    fn node(id: &str, x: f32, y: f32) -> CanvasNode {
        CanvasNode { x, y, ..fixtures::node(id) }
    }

    /// Layout flows downward, so its links leave the bottom and arrive at the top.
    fn edge(from: &str, to: &str) -> CanvasEdge {
        CanvasEdge {
            id: format!("{from}-{to}"),
            from_side: "bottom".to_owned(),
            to_side: "top".to_owned(),
            ..fixtures::edge(from, to)
        }
    }

    fn y_of(out: &[(usize, f32, f32)], index: usize) -> f32 {
        out.iter().find(|(i, _, _)| *i == index).unwrap().2
    }

    #[test]
    fn empty_selection_lays_out_nothing() {
        let nodes = vec![node("a", 0.0, 0.0)];
        assert!(layered_layout(&nodes, &[], &[], Direction::Down, 70.0, 40.0).is_empty());
    }

    #[test]
    fn a_chain_stacks_into_ranks_top_to_bottom() {
        // Scrambled initial positions; layout should order them a, b, c by y.
        let nodes = vec![
            node("a", 0.0, 0.0),
            node("b", 500.0, 500.0),
            node("c", -300.0, 100.0),
        ];
        let edges = vec![edge("a", "b"), edge("b", "c")];
        let out = layered_layout(&nodes, &edges, &[0, 1, 2], Direction::Down, 70.0, 40.0);

        assert!(y_of(&out, 0) < y_of(&out, 1));
        assert!(y_of(&out, 1) < y_of(&out, 2));
    }

    #[test]
    fn a_chain_flows_left_to_right_in_lr() {
        let nodes = vec![node("a", 0.0, 0.0), node("b", 0.0, 0.0), node("c", 0.0, 0.0)];
        let edges = vec![edge("a", "b"), edge("b", "c")];
        let out = layered_layout(&nodes, &edges, &[0, 1, 2], Direction::Right, 70.0, 40.0);

        // Ranks now advance along x, and each single-node rank shares a y.
        assert!(x_of(&out, 0) < x_of(&out, 1));
        assert!(x_of(&out, 1) < x_of(&out, 2));
        assert_eq!(y_of(&out, 0), y_of(&out, 2));
    }

    #[test]
    fn independent_nodes_share_a_rank_side_by_side() {
        let nodes = vec![node("a", 0.0, 0.0), node("b", 0.0, 0.0)];
        let out = layered_layout(&nodes, &[], &[0, 1], Direction::Down, 70.0, 40.0);

        assert_eq!(out[0].2, out[1].2); // same rank -> same y
        assert_ne!(out[0].1, out[1].1); // laid out side by side -> different x
    }

    fn x_of(out: &[(usize, f32, f32)], index: usize) -> f32 {
        out.iter().find(|(i, _, _)| *i == index).unwrap().1
    }

    #[test]
    fn barycenter_reduces_crossings() {
        // Ranks {a,b} over {c,d} with edges a->d and b->c cross under the
        // initial order; the barycenter pass should swap c and d so d (fed by
        // the left node a) ends up left of c.
        let nodes = vec![
            node("a", 0.0, 0.0),
            node("b", 0.0, 0.0),
            node("c", 0.0, 0.0),
            node("d", 0.0, 0.0),
        ];
        let edges = vec![edge("a", "d"), edge("b", "c")];
        let out = layered_layout(&nodes, &edges, &[0, 1, 2, 3], Direction::Down, 70.0, 40.0);

        assert!(x_of(&out, 3) < x_of(&out, 2)); // d left of c
    }

    #[test]
    fn a_card_with_no_children_keeps_its_place_among_siblings_that_have_some() {
        // A trip: three days with three plans each, and a fourth day added
        // last, with none yet. Sorted by its own place among the days (3)
        // against the others' average place among the plans (1, 4, 7), it
        // came second.
        let mut nodes = vec![node("trip", 0.0, 0.0)];
        let mut edges = Vec::new();
        for day in 1..=3 {
            let id = format!("day{day}");
            nodes.push(node(&id, 0.0, 0.0));
            edges.push(edge("trip", &id));
            for plan in 1..=3 {
                let plan = format!("{id}-plan{plan}");
                nodes.push(node(&plan, 0.0, 0.0));
                edges.push(edge(&id, &plan));
            }
        }
        nodes.push(node("day4", 0.0, 0.0));
        edges.push(edge("trip", "day4"));
        let days: Vec<usize> = ["day1", "day2", "day3", "day4"]
            .iter()
            .map(|id| nodes.iter().position(|n| n.id == *id).unwrap())
            .collect();
        let all: Vec<usize> = (0..nodes.len()).collect();

        let out = layered_layout(&nodes, &edges, &all, Direction::Down, 70.0, 40.0);
        let across: Vec<f32> = days.iter().map(|&day| x_of(&out, day)).collect();
        assert!(across.is_sorted(), "days left to right in the order they were added: {across:?}");

        let out = layered_layout(&nodes, &edges, &all, Direction::Right, 70.0, 40.0);
        let across: Vec<f32> = days.iter().map(|&day| y_of(&out, day)).collect();
        assert!(across.is_sorted(), "days top to bottom in the order they were added: {across:?}");
    }

    #[test]
    fn a_diamond_stacks_into_three_ranks() {
        // a -> {b, c} -> d: b and c share the middle rank.
        let nodes = vec![
            node("a", 0.0, 0.0),
            node("b", 0.0, 0.0),
            node("c", 0.0, 0.0),
            node("d", 0.0, 0.0),
        ];
        let edges = vec![edge("a", "b"), edge("a", "c"), edge("b", "d"), edge("c", "d")];
        let out = layered_layout(&nodes, &edges, &[0, 1, 2, 3], Direction::Down, 70.0, 40.0);

        assert!(y_of(&out, 0) < y_of(&out, 1)); // a above b
        assert_eq!(y_of(&out, 1), y_of(&out, 2)); // b and c share a rank
        assert!(y_of(&out, 2) < y_of(&out, 3)); // b/c above d
    }

    #[test]
    fn a_cycle_lays_out_without_hanging() {
        // a -> b -> a: the ranking cap must terminate and still place both.
        let nodes = vec![node("a", 0.0, 0.0), node("b", 0.0, 0.0)];
        let edges = vec![edge("a", "b"), edge("b", "a")];
        let out = layered_layout(&nodes, &edges, &[0, 1], Direction::Down, 70.0, 40.0);

        assert_eq!(out.len(), 2);
    }

    #[test]
    fn facing_sides_are_flow_biased() {
        // Top-down: any cross-rank link flows vertically — even a mostly
        // horizontal one — so incoming enters the top, outgoing leaves the
        // bottom (never the same port).
        assert_eq!(
            facing_sides((0.0, 0.0), (300.0, 80.0), Direction::Down),
            ("bottom", "top")
        );
        // Same rank (no vertical gap) connects across the flow.
        assert_eq!(
            facing_sides((0.0, 0.0), (100.0, 0.0), Direction::Down),
            ("right", "left")
        );
        // Left-to-right flips the axes.
        assert_eq!(
            facing_sides((0.0, 0.0), (80.0, 300.0), Direction::Right),
            ("right", "left")
        );
        assert_eq!(
            facing_sides((0.0, 0.0), (0.0, 100.0), Direction::Right),
            ("bottom", "top")
        );
    }

    #[test]
    fn layout_preserves_the_centroid() {
        let nodes = vec![node("a", 10.0, 10.0), node("b", 200.0, 300.0)];
        let edges = vec![edge("a", "b")];
        let out = layered_layout(&nodes, &edges, &[0, 1], Direction::Down, 70.0, 40.0);

        let orig_cx = ((10.0 + 50.0) + (200.0 + 50.0)) / 2.0;
        let orig_cy = ((10.0 + 20.0) + (300.0 + 20.0)) / 2.0;
        let new_cx = out.iter().map(|(i, x, _)| x + nodes[*i].width / 2.0).sum::<f32>() / 2.0;
        let new_cy = out.iter().map(|(i, _, y)| y + nodes[*i].height / 2.0).sum::<f32>() / 2.0;

        assert!((new_cx - orig_cx).abs() < 0.01);
        assert!((new_cy - orig_cy).abs() < 0.01);
    }

    // Source box: top-left (0, 0), 100 wide, 50 tall → cross-axis center 25.
    const SRC: (f32, f32, f32, f32) = (0.0, 0.0, 100.0, 50.0);

    #[test]
    fn sibling_stack_places_one_child_right_and_centered() {
        let out = sibling_stack(SRC, &[(200.0, 80.0)], Direction::Right, 70.0, 40.0);
        // x = source right (100) + rank_gap (70); y centers the 80-tall child on
        // the source's center_y (25): 25 - 40 = -15.
        assert_eq!(out, vec![(170.0, -15.0)]);
        // The child's own center_y lands exactly on the source's.
        assert!((out[0].1 + 80.0 / 2.0 - 25.0).abs() < 0.01);
    }

    #[test]
    fn sibling_stack_stacks_multiple_centered_on_the_source() {
        let children = [(200.0, 80.0), (200.0, 120.0)];
        let out = sibling_stack(SRC, &children, Direction::Right, 70.0, 40.0);
        // Both in the same column, second below the first by height + node_gap.
        assert_eq!(out[0].0, 170.0);
        assert_eq!(out[1].0, 170.0);
        assert!((out[1].1 - (out[0].1 + 80.0 + 40.0)).abs() < 0.01);
        // The stack's combined span is centered on the source's center_y (25).
        let top = out[0].1;
        let bottom = out[1].1 + 120.0;
        assert!(((top + bottom) / 2.0 - 25.0).abs() < 0.01);
    }

    #[test]
    fn sibling_stack_flows_into_a_row_when_going_down() {
        let children = [(200.0, 80.0), (160.0, 80.0)];
        let out = sibling_stack(SRC, &children, Direction::Down, 70.0, 40.0);
        // Same y (source bottom 50 + rank_gap 70), spread horizontally.
        assert_eq!(out[0].1, 120.0);
        assert_eq!(out[1].1, 120.0);
        assert!((out[1].0 - (out[0].0 + 200.0 + 40.0)).abs() < 0.01);
        // Centered on the source's center_x (50).
        let left = out[0].0;
        let right = out[1].0 + 160.0;
        assert!(((left + right) / 2.0 - 50.0).abs() < 0.01);
    }

    #[test]
    fn sibling_stack_of_no_children_is_empty() {
        assert!(sibling_stack(SRC, &[], Direction::Right, 70.0, 40.0).is_empty());
    }

    // --- Groups taken whole ---

    fn frame(id: &str, x: f32, y: f32, width: f32, height: f32) -> CanvasNode {
        CanvasNode { kind: "group".to_owned(), text: None, x, y, width, height, ..fixtures::node(id) }
    }

    /// A group of three cards linked in a chain, a card linked into it from
    /// the left and one it links out to, both well away from it.
    fn a_group_between_two_cards() -> (Vec<CanvasNode>, Vec<CanvasEdge>) {
        let nodes = vec![
            frame("g", 0.0, 0.0, 300.0, 300.0),
            node("m1", 20.0, 40.0),
            node("m2", 150.0, 120.0),
            node("m3", 20.0, 220.0),
            node("in", -900.0, 500.0),
            node("out", 900.0, -500.0),
        ];
        let edges = vec![edge("in", "m1"), edge("m1", "m2"), edge("m2", "m3"), edge("m3", "out")];
        (nodes, edges)
    }

    /// `nodes` with `framed`'s moves made.
    fn moved(nodes: &[CanvasNode], framed: &Framed) -> Vec<CanvasNode> {
        let mut nodes = nodes.to_vec();
        for &(i, x, y) in &framed.moves {
            (nodes[i].x, nodes[i].y) = (x, y);
        }
        nodes
    }

    /// Asserts each of `members` sits where it did relative to `frame`, and
    /// so still inside it.
    fn kept_inside(before: &[CanvasNode], after: &[CanvasNode], frame: usize, members: &[usize]) {
        for &m in members {
            let offset = |nodes: &[CanvasNode]| (nodes[m].x - nodes[frame].x, nodes[m].y - nodes[frame].y);
            assert_eq!(offset(after), offset(before), "{} kept its place in the group", before[m].id);
            assert!(model::centre_within(&after[m], &after[frame]), "{} is still in the group", before[m].id);
        }
    }

    #[test]
    fn a_groups_cards_are_laid_out_with_it_not_out_of_it() {
        // A marquee over a group selects its cards, never the frame.
        let (nodes, edges) = a_group_between_two_cards();
        let framed = framed_layout(&nodes, &edges, &[1, 2, 3, 4, 5], Direction::Right, 70.0, 40.0);
        let after = moved(&nodes, &framed);

        assert_eq!(framed.container, None);
        kept_inside(&nodes, &after, 0, &[1, 2, 3]);
        // Links to and from its cards are the group's: it's ranked between them.
        assert!(after[4].x + after[4].width < after[0].x, "in, then the group");
        assert!(after[0].x + after[0].width < after[5].x, "the group, then out");
        // And its cards moved as the group, so the links inside keep their ports.
        for (&(i, _, _), &unit) in framed.moves.iter().zip(&framed.units) {
            assert_eq!(unit, if i <= 3 { 0 } else { i });
        }
    }

    #[test]
    fn one_of_a_groups_cards_brings_the_whole_group() {
        let (nodes, edges) = a_group_between_two_cards();
        let framed = framed_layout(&nodes, &edges, &[2, 4], Direction::Right, 70.0, 40.0);
        let after = moved(&nodes, &framed);

        assert!(after[0].x != nodes[0].x || after[0].y != nodes[0].y, "the frame moved");
        kept_inside(&nodes, &after, 0, &[1, 2, 3]);
    }

    #[test]
    fn a_selected_frame_is_laid_out_whole() {
        // A click on its header, then a ⌘-click on each card beside it.
        let (nodes, edges) = a_group_between_two_cards();
        let framed = framed_layout(&nodes, &edges, &[0, 4, 5], Direction::Right, 70.0, 40.0);
        let after = moved(&nodes, &framed);

        kept_inside(&nodes, &after, 0, &[1, 2, 3]);
        assert!(after[4].x + after[4].width < after[0].x);
    }

    #[test]
    fn a_selection_inside_one_group_is_laid_out_inside_it() {
        // The second step: the group's own cards, arranged among themselves.
        let (nodes, edges) = a_group_between_two_cards();
        let framed = framed_layout(&nodes, &edges, &[1, 2, 3], Direction::Down, 70.0, 40.0);
        let after = moved(&nodes, &framed);

        assert_eq!(framed.container, Some(0), "the caller grows it around them");
        assert!(framed.moves.iter().all(|&(i, _, _)| i != 0), "the frame stays put");
        assert_eq!(framed.units, vec![1, 2, 3], "each card on its own");
        assert!(after[1].y < after[2].y && after[2].y < after[3].y, "the chain, top to bottom");
    }

    #[test]
    fn a_nested_group_moves_with_the_group_around_it() {
        let nodes = vec![
            frame("outer", 0.0, 0.0, 600.0, 600.0),
            frame("inner", 20.0, 40.0, 300.0, 300.0),
            node("card", 60.0, 100.0),
            node("other", 400.0, 400.0),
            node("beside", -900.0, 0.0),
        ];
        let edges = vec![edge("beside", "card")];
        let framed = framed_layout(&nodes, &edges, &[2, 4], Direction::Right, 70.0, 40.0);
        let after = moved(&nodes, &framed);

        assert_eq!(framed.container, None);
        kept_inside(&nodes, &after, 0, &[1, 2, 3]);
        assert!(after[4].x + after[4].width < after[0].x, "the outer group is what's ranked");
    }
}
