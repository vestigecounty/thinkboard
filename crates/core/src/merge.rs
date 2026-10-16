//! Three-way merge of a canvas, for when another program writes the file a
//! board has open.
//!
//! The board used to settle that by last write wins: an outside change was
//! reloaded only while nothing here was unsaved, and otherwise the next save
//! wrote over it. So a card added in Obsidian while one was being edited here
//! — or in the moment between an edit and its autosave — was gone from the
//! file without a trace. Cards carry stable ids, which makes a merge reliable
//! rather than a guess: each card and link is compared with the copy both
//! sides started from (the file as last read or written), and whichever side
//! changed it is the one that's kept.
//!
//! Where both changed the same card, it's merged field by field, so a card
//! moved there and edited here keeps both; where both changed the same
//! *field*, ours wins — the person in front of this board is the one who'd
//! notice. A deletion doesn't take a card the other side changed: that's the
//! one conflict where either answer loses something, and a card that comes
//! back can be deleted again.
//!
//! Everything is compared as it would be written, through the same
//! serializer: geometry is stored rounded, so a card dragged to x = 100.4 here
//! and read back as 100 hasn't been moved by anyone.

use std::collections::{HashMap, HashSet};

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use crate::model::{CanvasDocument, CanvasEdge, CanvasNode};

/// A card or a link: something with an id that a merge can match up.
trait Item: Clone + Serialize + DeserializeOwned {
    fn id(&self) -> &str;
    /// Fields that only mean something together, merged as one: a card moved
    /// there and resized here keeps both, but not x from one side and y from
    /// the other, which is a place neither side put it.
    const UNITS: &'static [&'static [&'static str]];
}

impl Item for CanvasNode {
    fn id(&self) -> &str {
        &self.id
    }
    const UNITS: &'static [&'static [&'static str]] = &[&["x", "y"], &["width", "height"]];
}

impl Item for CanvasEdge {
    fn id(&self) -> &str {
        &self.id
    }
    // A link's end is a node *and* a side of it.
    const UNITS: &'static [&'static [&'static str]] = &[&["fromNode", "fromSide"], &["toNode", "toSide"]];
}

/// Merges `ours` (what the board holds) and `theirs` (what's in the file now)
/// against `base`, the copy both started from.
///
/// `held` names cards that count as changed here whatever they hold — the one
/// being edited, which a deletion there mustn't pull out from under the
/// editor before its first keystroke has changed anything.
pub fn merge(
    base: &CanvasDocument,
    ours: &CanvasDocument,
    theirs: &CanvasDocument,
    held: &HashSet<&str>,
) -> CanvasDocument {
    // The common cases, and exact: nothing changed on one side means the
    // other side is the answer, down to the order of its cards.
    if same(theirs, base) {
        return ours.clone();
    }
    if same(ours, base) && held.is_empty() {
        return theirs.clone();
    }
    let nodes = merge_items(&base.nodes, &ours.nodes, &theirs.nodes, held);
    let mut edges = merge_items(&base.edges, &ours.edges, &theirs.edges, &HashSet::new());
    // A link to a card that's gone — deleted on one side while linked to on
    // the other — is a link to nothing.
    {
        let ids: HashSet<&str> = nodes.iter().map(|node| node.id.as_str()).collect();
        edges.retain(|edge| ids.contains(edge.from_node.as_str()) && ids.contains(edge.to_node.as_str()));
    }
    let extra = merge_fields(&base.extra, &ours.extra, &theirs.extra, &[]);
    CanvasDocument { nodes, edges, extra }
}

/// Whether two documents would be written to the same file contents, give or
/// take the order of keys within a card.
pub fn same(a: &CanvasDocument, b: &CanvasDocument) -> bool {
    a.nodes.len() == b.nodes.len()
        && a.edges.len() == b.edges.len()
        && a.extra == b.extra
        && a.nodes.iter().zip(&b.nodes).all(|(a, b)| fields(a) == fields(b))
        && a.edges.iter().zip(&b.edges).all(|(a, b)| fields(a) == fields(b))
}

/// An item as it's written to the file.
fn fields<T: Serialize>(item: &T) -> Map<String, Value> {
    match serde_json::to_value(item) {
        Ok(Value::Object(map)) => map,
        _ => Map::new(),
    }
}

fn merge_items<T: Item>(base: &[T], ours: &[T], theirs: &[T], held: &HashSet<&str>) -> Vec<T> {
    fn by_id<T: Item>(items: &[T]) -> HashMap<&str, &T> {
        items.iter().map(|item| (item.id(), item)).collect()
    }
    let (b, o, t) = (by_id(base), by_id(ours), by_id(theirs));
    let changed = |from: &T, to: &T| fields(from) != fields(to);

    let mut merged: HashMap<&str, T> = HashMap::new();
    for &id in o.keys().chain(t.keys()) {
        if merged.contains_key(id) {
            continue;
        }
        let item = match (b.get(id).copied(), o.get(id).copied(), t.get(id).copied()) {
            (base, Some(ours), Some(theirs)) => Some(merge_item(base, ours, theirs)),
            // Deleted on one side: gone, unless the other side changed it.
            (Some(base), Some(ours), None) => (held.contains(id) || changed(base, ours)).then(|| ours.clone()),
            (Some(base), None, Some(theirs)) => changed(base, theirs).then(|| theirs.clone()),
            // Added on one side.
            (None, Some(item), None) | (None, None, Some(item)) => Some(item.clone()),
            _ => None,
        };
        if let Some(item) = item {
            merged.insert(id, item);
        }
    }
    order(base, ours, theirs)
        .into_iter()
        .filter_map(|id| merged.remove(id))
        .collect()
}

/// One item both sides still have. `base` is absent when both added it, which
/// only an id collision does — every field then counts as changed on both.
fn merge_item<T: Item>(base: Option<&T>, ours: &T, theirs: &T) -> T {
    let (o, t) = (fields(ours), fields(theirs));
    let b = base.map(fields).unwrap_or_default();
    if o == t || t == b {
        return ours.clone();
    }
    if o == b {
        return theirs.clone();
    }
    let merged = merge_fields(&b, &o, &t, T::UNITS);
    serde_json::from_value(Value::Object(merged)).unwrap_or_else(|_| ours.clone())
}

/// Field by field (or unit by unit): unchanged here takes theirs, changed here
/// keeps ours — whether or not they changed it too.
fn merge_fields(
    base: &Map<String, Value>,
    ours: &Map<String, Value>,
    theirs: &Map<String, Value>,
    units: &[&[&str]],
) -> Map<String, Value> {
    let mut merged = Map::new();
    let mut done: HashSet<&str> = HashSet::new();
    // Ours' keys first, so the merged card is written in the order ours was.
    for key in ours.keys().chain(theirs.keys()).chain(base.keys()) {
        if done.contains(key.as_str()) {
            continue;
        }
        let unit: Vec<&str> = units
            .iter()
            .find(|unit| unit.contains(&key.as_str()))
            .map_or_else(|| vec![key.as_str()], |unit| unit.to_vec());
        let values = |map: &Map<String, Value>| unit.iter().map(|k| map.get(*k).cloned()).collect::<Vec<_>>();
        let from = if values(ours) == values(base) { theirs } else { ours };
        for &k in &unit {
            done.insert(k);
            if let Some(value) = from.get(k) {
                merged.insert(k.to_owned(), value.clone());
            }
        }
    }
    merged
}

/// The order the merged items go in, which is the order they're drawn in.
///
/// The file's, unless only this side reordered them — a card raised here by
/// dropping it on another. Whatever the chosen side lacks (cards added on the
/// other, or kept from it against a deletion) goes after, which is on top:
/// it's the newest thing on the board.
fn order<'a, T: Item>(base: &'a [T], ours: &'a [T], theirs: &'a [T]) -> Vec<&'a str> {
    let reordered = |side: &[T]| {
        let (in_base, in_side): (HashSet<&str>, HashSet<&str>) =
            (base.iter().map(T::id).collect(), side.iter().map(T::id).collect());
        let was = base.iter().map(T::id).filter(|id| in_side.contains(id));
        let is = side.iter().map(T::id).filter(|id| in_base.contains(id));
        !was.eq(is)
    };
    let (first, then) = if reordered(ours) && !reordered(theirs) { (ours, theirs) } else { (theirs, ours) };
    let mut seen = HashSet::new();
    first.iter().chain(then).map(T::id).filter(|&id| seen.insert(id)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{edge, node};

    fn doc(nodes: Vec<CanvasNode>, edges: Vec<CanvasEdge>) -> CanvasDocument {
        CanvasDocument { nodes, edges, extra: Map::new() }
    }

    fn texts(document: &CanvasDocument) -> Vec<&str> {
        document.nodes.iter().map(|n| n.text.as_deref().unwrap_or_default()).collect()
    }

    fn text(id: &str, text: &str) -> CanvasNode {
        CanvasNode { text: Some(text.to_owned()), ..node(id) }
    }

    fn none() -> HashSet<&'static str> {
        HashSet::new()
    }

    #[test]
    fn a_card_added_there_survives_an_edit_here() {
        // The loss the tour found: a card added in Obsidian while one was
        // being edited here was written over by the edit's save.
        let base = doc(vec![text("a", "plan"), text("b", "eat")], vec![]);
        let ours = doc(vec![text("a", "plan"), text("b", "eat near Alfama")], vec![]);
        let theirs = doc(
            vec![text("a", "plan"), text("b", "eat"), text("c", "tram passes")],
            vec![edge("a", "c")],
        );
        let merged = merge(&base, &ours, &theirs, &none());
        assert_eq!(texts(&merged), ["plan", "eat near Alfama", "tram passes"]);
        assert_eq!(merged.edges.len(), 1);
    }

    #[test]
    fn a_move_there_and_an_edit_here_both_survive() {
        let base = doc(vec![text("a", "plan")], vec![]);
        let ours = doc(vec![text("a", "plan, booked")], vec![]);
        let theirs = doc(vec![CanvasNode { x: 160.0, ..text("a", "plan") }], vec![]);
        let merged = merge(&base, &ours, &theirs, &none());
        assert_eq!(merged.nodes[0].text.as_deref(), Some("plan, booked"));
        assert_eq!(merged.nodes[0].x, 160.0);
    }

    #[test]
    fn ours_wins_where_both_changed_the_same_thing() {
        let base = doc(vec![text("a", "plan")], vec![]);
        let ours = doc(vec![CanvasNode { x: 40.0, y: 10.0, ..text("a", "ours") }], vec![]);
        let theirs = doc(vec![CanvasNode { x: 160.0, ..text("a", "theirs") }], vec![]);
        let merged = merge(&base, &ours, &theirs, &none());
        assert_eq!(merged.nodes[0].text.as_deref(), Some("ours"));
        // A position is one thing: not their x with our y.
        assert_eq!((merged.nodes[0].x, merged.nodes[0].y), (40.0, 10.0));
    }

    #[test]
    fn deletions_on_either_side_are_kept() {
        let base = doc(vec![text("a", "a"), text("b", "b"), text("c", "c")], vec![edge("a", "b"), edge("b", "c")]);
        let ours = doc(vec![text("a", "a"), text("b", "b")], vec![edge("a", "b")]);
        let theirs = doc(vec![text("b", "b"), text("c", "c")], vec![edge("b", "c")]);
        let merged = merge(&base, &ours, &theirs, &none());
        assert_eq!(texts(&merged), ["b"]);
        assert!(merged.edges.is_empty(), "both links lost an end");
    }

    #[test]
    fn a_deletion_does_not_take_a_card_the_other_side_changed() {
        let base = doc(vec![text("a", "a"), text("b", "b")], vec![]);
        let ours = doc(vec![text("a", "a, changed here"), text("b", "b")], vec![]);
        let theirs = doc(vec![text("b", "b, changed there")], vec![]);
        let merged = merge(&base, &ours, &theirs, &none());
        assert_eq!(texts(&merged), ["b, changed there", "a, changed here"]);

        // Nor the one being edited, before anything in it has changed. Kept
        // against the file, it's drawn last, like anything else it lacks.
        let merged = merge(&base, &base, &doc(vec![text("b", "b")], vec![]), &["a"].into());
        assert_eq!(texts(&merged), ["b", "a"]);
    }

    #[test]
    fn subpixel_drift_is_not_a_change() {
        // Dragged to 100.4 here, written as 100, read back as 100: nobody
        // moved it, so their move stands.
        let base = doc(vec![CanvasNode { x: 100.4, ..text("a", "a") }], vec![]);
        let ours = base.clone();
        let theirs = doc(vec![CanvasNode { x: 100.0, y: 80.0, ..text("a", "a") }], vec![]);
        let merged = merge(&base, &ours, &theirs, &none());
        assert_eq!((merged.nodes[0].x, merged.nodes[0].y), (100.0, 80.0));
        assert!(same(&doc(vec![CanvasNode { x: 100.4, ..text("a", "a") }], vec![]), &doc(vec![CanvasNode { x: 100.0, ..text("a", "a") }], vec![])));
    }

    #[test]
    fn fields_another_tool_wrote_merge_like_the_rest() {
        let tagged = |key: &str, value: &str| {
            let mut node = text("a", "a");
            node.extra.insert(key.to_owned(), Value::String(value.to_owned()));
            node
        };
        let base = doc(vec![text("a", "a")], vec![]);
        let ours = doc(vec![CanvasNode { color: Some("4".into()), ..text("a", "a") }], vec![]);
        let theirs = doc(vec![tagged("ai", "done")], vec![]);
        let merged = merge(&base, &ours, &theirs, &none());
        assert_eq!(merged.nodes[0].color.as_deref(), Some("4"));
        assert_eq!(merged.nodes[0].extra.get("ai"), Some(&Value::String("done".into())));
    }

    #[test]
    fn the_files_order_stands_unless_only_ours_changed_it() {
        let base = doc(vec![text("a", "a"), text("b", "b")], vec![]);
        // Raised here, and something else changed there.
        let ours = doc(vec![text("b", "b"), text("a", "a")], vec![]);
        let theirs = doc(vec![text("a", "a"), text("b", "b, there")], vec![]);
        assert_eq!(texts(&merge(&base, &ours, &theirs, &none())), ["b, there", "a"]);
        // Both reordered: the file's order.
        let theirs = doc(vec![text("b", "b, there"), text("a", "a")], vec![]);
        let ours = doc(vec![text("b", "b"), text("a", "a here")], vec![]);
        assert_eq!(texts(&merge(&base, &ours, &theirs, &none())), ["b, there", "a here"]);
    }

    #[test]
    fn what_one_side_left_alone_is_the_other_side_exactly() {
        let base = doc(vec![text("a", "a")], vec![]);
        let ours = doc(vec![CanvasNode { x: 100.4, ..text("a", "a") }, text("b", "new")], vec![edge("a", "b")]);
        let merged = merge(&base, &ours, &base, &none());
        assert_eq!(merged.nodes[0].x, 100.4, "kept exactly, not as written");
        assert!(same(&merged, &ours));
        let theirs = ours.clone();
        assert!(same(&merge(&base, &base, &theirs, &none()), &theirs));
    }
}
