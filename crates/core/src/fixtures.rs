//! Nodes and edges for tests, so a test says what it is *about*.
//!
//! Six modules had grown their own builders, filling the same ten fields with
//! slightly different defaults — and a test that spells out a colour and an
//! `extra` map to check a layout is mostly noise. Both of these are bases to
//! amend with struct-update syntax: `CanvasNode { width: 400.0, ..node("a") }`
//! says the width is the point and the rest isn't.

use serde_json::Map;

use crate::model::{CanvasEdge, CanvasNode};

/// An unremarkable text node at the origin: 100×40, no colour, no extras.
pub fn node(id: &str) -> CanvasNode {
    CanvasNode {
        id: id.to_owned(),
        kind: "text".to_owned(),
        text: Some(format!("node {id}")),
        file: None,
        x: 0.0,
        y: 0.0,
        width: 100.0,
        height: 40.0,
        color: None,
        extra: Map::new(),
    }
}

/// A left-to-right link between two nodes, with the id `from->to`.
pub fn edge(from: &str, to: &str) -> CanvasEdge {
    CanvasEdge {
        id: format!("{from}->{to}"),
        from_node: from.to_owned(),
        from_side: "right".to_owned(),
        to_node: to.to_owned(),
        to_side: "left".to_owned(),
        extra: Map::new(),
    }
}
