//! Obsidian JSON Canvas document model.
//!
//! Mirrors the Swift `Document`/`Node`/`Edge` structs: tolerant of unknown
//! fields (the sample document carries an extra `ai` field) and of missing
//! geometry, defaulting like the Swift app (150x200).

use serde::{Deserialize, Serialize, Serializer};
use serde_json::{Map, Value};
use std::io;
use std::path::Path;

use crate::color::Rgba;

/// Path to the sample canvas bundled with this crate. The app itself requires
/// a canvas path argument (see `startup::run`), so this is only used as test
/// input (by tests in this crate and the desktop crate's `sync` tests).
pub const DEFAULT_CANVAS: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/testdata/fixtures/sample.canvas"
);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanvasDocument {
    #[serde(default)]
    pub nodes: Vec<CanvasNode>,
    #[serde(default)]
    pub edges: Vec<CanvasEdge>,
    /// Any top-level keys we don't model (preserved on save).
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanvasNode {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default, serialize_with = "serialize_round")]
    pub x: f32,
    #[serde(default, serialize_with = "serialize_round")]
    pub y: f32,
    #[serde(default = "default_width", serialize_with = "serialize_round")]
    pub width: f32,
    #[serde(default = "default_height", serialize_with = "serialize_round")]
    pub height: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// Fields we don't model (e.g. the custom `ai` field) — preserved so
    /// round-tripping never drops another tool's data.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Obsidian stores canvas coordinates as integers; round f32 geometry back
/// so saves match Obsidian's format instead of emitting `-660.0`.
fn serialize_round<S: Serializer>(value: &f32, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_i64(value.round() as i64)
}

fn default_width() -> f32 {
    150.0
}

fn default_height() -> f32 {
    200.0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanvasEdge {
    pub id: String,
    #[serde(rename = "fromNode")]
    pub from_node: String,
    #[serde(rename = "fromSide", default = "default_from_side")]
    pub from_side: String,
    #[serde(rename = "toNode")]
    pub to_node: String,
    #[serde(rename = "toSide", default = "default_to_side")]
    pub to_side: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

fn default_from_side() -> String {
    "right".to_owned()
}

fn default_to_side() -> String {
    "left".to_owned()
}

/// Removes the edge at `index`, returning whether the list changed. An
/// out-of-range index — e.g. a selection left dangling after the edge list
/// shrank — is a no-op rather than a panic.
pub fn remove_edge(edges: &mut Vec<CanvasEdge>, index: usize) -> bool {
    if index >= edges.len() {
        return false;
    }
    edges.remove(index);
    true
}

impl CanvasDocument {
    pub fn load(path: &Path) -> Result<Self, String> {
        let json = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        Self::from_json(&json)
    }

    /// Parses a canvas document from JSON text — the in-memory counterpart of
    /// [`load`] (which reads a file). Tolerant of unknown fields, so a document
    /// authored by another tool round-trips through [`to_canvas_json`] intact.
    pub fn from_json(json: &str) -> Result<Self, String> {
        serde_json::from_str(json).map_err(|e| format!("cannot parse canvas: {e}"))
    }

    /// Serializes in Obsidian's canvas style: tab-indented top-level object
    /// and arrays, but each node/edge as a compact single-line object,
    /// integer coordinates, and no trailing newline.
    pub fn to_canvas_json(&self) -> Result<String, String> {
        let mut buffer = Vec::new();
        let mut serializer =
            serde_json::Serializer::with_formatter(&mut buffer, CanvasFormatter::default());
        self.serialize(&mut serializer)
            .map_err(|e| format!("cannot serialize canvas: {e}"))?;
        String::from_utf8(buffer).map_err(|e| e.to_string())
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        std::fs::write(path, self.to_canvas_json()?)
            .map_err(|e| format!("cannot write {}: {e}", path.display()))
    }
}

/// Default size of a node created through the add-node helpers (Obsidian's
/// text-node default), and the gap left between a new node and its neighbours.
const NEW_NODE_W: f32 = 250.0;
const NEW_NODE_H: f32 = 60.0;
const NEW_NODE_GAP: f32 = 40.0;

/// Whether two axis-aligned rectangles overlap. Each is `(x, y, w, h)` with
/// `(x, y)` the top-left corner.
fn rects_overlap(a: (f32, f32, f32, f32), b: (f32, f32, f32, f32)) -> bool {
    let (ax, ay, aw, ah) = a;
    let (bx, by, bw, bh) = b;
    ax < bx + bw && bx < ax + aw && ay < by + bh && by < ay + ah
}

/// Editing operations. Kept in the core so every front-end mutates a canvas the
/// same way; ids are generated with [`random_id`] in Obsidian's style.
impl CanvasDocument {
    /// Adds a text node at `(x, y)` with the given size, returning its new id.
    pub fn add_text_node(&mut self, text: String, x: f32, y: f32, width: f32, height: f32) -> String {
        let id = random_id();
        self.nodes.push(CanvasNode {
            id: id.clone(),
            kind: "text".to_owned(),
            text: Some(text),
            file: None,
            x,
            y,
            width,
            height,
            color: None,
            extra: Map::new(),
        });
        id
    }

    /// Sets an existing node's text. Returns whether a node with `id` was found.
    pub fn set_node_text(&mut self, id: &str, text: String) -> bool {
        match self.nodes.iter_mut().find(|n| n.id == id) {
            Some(node) => {
                node.text = Some(text);
                true
            }
            None => false,
        }
    }

    /// Sets or clears a node's color — an Obsidian preset code (`"1"`–`"6"`) or a
    /// hex string; `None` clears it. Returns whether a node with `id` was found.
    pub fn set_node_color(&mut self, id: &str, color: Option<String>) -> bool {
        match self.nodes.iter_mut().find(|n| n.id == id) {
            Some(node) => {
                node.color = color;
                true
            }
            None => false,
        }
    }

    /// Moves a node's top-left corner. Returns whether `id` matched.
    pub fn move_node(&mut self, id: &str, x: f32, y: f32) -> bool {
        match self.nodes.iter_mut().find(|n| n.id == id) {
            Some(node) => {
                node.x = x;
                node.y = y;
                true
            }
            None => false,
        }
    }

    /// Removes the node with `id` and every edge touching it. Returns whether
    /// the node existed.
    pub fn remove_node(&mut self, id: &str) -> bool {
        let before = self.nodes.len();
        self.nodes.retain(|n| n.id != id);
        if self.nodes.len() == before {
            return false;
        }
        self.edges.retain(|e| e.from_node != id && e.to_node != id);
        true
    }

    /// Adds an edge between two nodes' sides (`"top"`/`"right"`/`"bottom"`/
    /// `"left"`), returning its new id.
    pub fn add_edge(
        &mut self,
        from_node: String,
        from_side: String,
        to_node: String,
        to_side: String,
    ) -> String {
        let id = random_id();
        self.edges.push(CanvasEdge {
            id: id.clone(),
            from_node,
            from_side,
            to_node,
            to_side,
            extra: Map::new(),
        });
        id
    }

    /// Removes the edge with `id`. Returns whether it existed.
    pub fn remove_edge_by_id(&mut self, id: &str) -> bool {
        let before = self.edges.len();
        self.edges.retain(|e| e.id != id);
        self.edges.len() != before
    }

    /// Adds a child of `parent_id`: a text node placed just past the parent
    /// along the flow (to its right), linked `parent -> child`. Returns the new
    /// node's id, or `None` if `parent_id` doesn't exist.
    pub fn add_child(&mut self, parent_id: &str, text: String) -> Option<String> {
        let parent = self.nodes.iter().find(|n| n.id == parent_id)?;
        let (px, py, pw, ph) = (parent.x, parent.y, parent.width, parent.height);
        let (x, y) = self.free_placement(px + pw + NEW_NODE_GAP, py + ph / 2.0 - NEW_NODE_H / 2.0);
        let id = self.add_text_node(text, x, y, NEW_NODE_W, NEW_NODE_H);
        self.add_edge(parent_id.to_owned(), "right".to_owned(), id.clone(), "left".to_owned());
        Some(id)
    }

    /// Adds a sibling of `node_id`: a node under the same parent, placed just
    /// below it. A root has no parent to share, so this starts a fresh cluster
    /// instead. Returns the new id, or `None` if `node_id` doesn't exist.
    pub fn add_sibling(&mut self, node_id: &str, text: String) -> Option<String> {
        let node = self.nodes.iter().find(|n| n.id == node_id)?;
        let (nx, ny, nh) = (node.x, node.y, node.height);
        match self
            .edges
            .iter()
            .find(|e| e.to_node == node_id)
            .map(|e| e.from_node.clone())
        {
            Some(parent_id) => {
                let (x, y) = self.free_placement(nx, ny + nh + NEW_NODE_GAP);
                let id = self.add_text_node(text, x, y, NEW_NODE_W, NEW_NODE_H);
                self.add_edge(parent_id, "right".to_owned(), id.clone(), "left".to_owned());
                Some(id)
            }
            None => Some(self.add_root(text)),
        }
    }

    /// Adds a standalone node (no edges) as a new cluster, in empty space below
    /// everything already on the canvas. Returns the new node's id.
    pub fn add_root(&mut self, text: String) -> String {
        let (x, y) = self.new_cluster_origin();
        self.add_text_node(text, x, y, NEW_NODE_W, NEW_NODE_H)
    }

    /// Top-left for a new `NEW_NODE_W`×`NEW_NODE_H` node near `(req_x, req_y)`
    /// that clears existing nodes, spiralling outward if the spot is taken.
    /// Mirrors the desktop's placement search.
    fn free_placement(&self, req_x: f32, req_y: f32) -> (f32, f32) {
        const MARGIN: f32 = 20.0;
        let fits = |x: f32, y: f32| {
            !self.nodes.iter().any(|n| {
                rects_overlap(
                    (
                        x - MARGIN,
                        y - MARGIN,
                        NEW_NODE_W + 2.0 * MARGIN,
                        NEW_NODE_H + 2.0 * MARGIN,
                    ),
                    (n.x, n.y, n.width, n.height),
                )
            })
        };
        if fits(req_x, req_y) {
            return (req_x, req_y);
        }
        let step = NEW_NODE_H + MARGIN;
        for ring in 1..=8 {
            let radius = step * ring as f32;
            for ray in 0..12 {
                let angle = std::f32::consts::TAU * ray as f32 / 12.0;
                let (x, y) = (req_x + radius * angle.cos(), req_y + radius * angle.sin());
                if fits(x, y) {
                    return (x, y);
                }
            }
        }
        (req_x, req_y)
    }

    /// The top-left corner for a fresh cluster: below the current bounding box,
    /// or the origin when the canvas is empty.
    fn new_cluster_origin(&self) -> (f32, f32) {
        if self.nodes.is_empty() {
            return (0.0, 0.0);
        }
        let min_x = self.nodes.iter().map(|n| n.x).fold(f32::INFINITY, f32::min);
        let max_y = self
            .nodes
            .iter()
            .map(|n| n.y + n.height)
            .fold(f32::NEG_INFINITY, f32::max);
        (min_x, max_y + NEW_NODE_GAP + 40.0)
    }
}

/// `serde_json` formatter matching Obsidian: containers up to depth 1 (the
/// root object and the `nodes`/`edges` arrays) are tab-pretty; deeper ones
/// (the node/edge objects) are compact. Colons and commas carry no spaces.
#[derive(Default)]
struct CanvasFormatter {
    stack: Vec<Frame>,
}

struct Frame {
    pretty: bool,
    has_content: bool,
}

impl CanvasFormatter {
    fn push(&mut self) -> &'static [u8] {
        let pretty = self.stack.len() < 2;
        self.stack.push(Frame {
            pretty,
            has_content: false,
        });
        b""
    }

    fn indent<W: ?Sized + io::Write>(writer: &mut W, tabs: usize) -> io::Result<()> {
        for _ in 0..tabs {
            writer.write_all(b"\t")?;
        }
        Ok(())
    }

    fn close<W: ?Sized + io::Write>(&mut self, writer: &mut W, open: u8) -> io::Result<()> {
        let frame = self.stack.pop().expect("balanced container");
        if frame.pretty && frame.has_content {
            writer.write_all(b"\n")?;
            Self::indent(writer, self.stack.len())?;
        }
        writer.write_all(&[open])
    }

    fn separator<W: ?Sized + io::Write>(&mut self, writer: &mut W, first: bool) -> io::Result<()> {
        let depth = self.stack.len();
        let frame = self.stack.last_mut().expect("inside a container");
        frame.has_content = true;
        let pretty = frame.pretty;
        if !first {
            writer.write_all(b",")?;
        }
        if pretty {
            writer.write_all(b"\n")?;
            Self::indent(writer, depth)?;
        }
        Ok(())
    }
}

impl serde_json::ser::Formatter for CanvasFormatter {
    fn begin_array<W: ?Sized + io::Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.push();
        writer.write_all(b"[")
    }
    fn end_array<W: ?Sized + io::Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.close(writer, b']')
    }
    fn begin_array_value<W: ?Sized + io::Write>(
        &mut self,
        writer: &mut W,
        first: bool,
    ) -> io::Result<()> {
        self.separator(writer, first)
    }

    fn begin_object<W: ?Sized + io::Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.push();
        writer.write_all(b"{")
    }
    fn end_object<W: ?Sized + io::Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.close(writer, b'}')
    }
    fn begin_object_key<W: ?Sized + io::Write>(
        &mut self,
        writer: &mut W,
        first: bool,
    ) -> io::Result<()> {
        self.separator(writer, first)
    }
    fn begin_object_value<W: ?Sized + io::Write>(&mut self, writer: &mut W) -> io::Result<()> {
        writer.write_all(b":")
    }
}

/// Random 16-hex-digit id in the style Obsidian uses for nodes/edges.
pub fn random_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{:016x}", nanos ^ count.rotate_left(48))
}

impl CanvasNode {
    /// Canvas color for this node, if any.
    pub fn palette_color(&self) -> Option<Rgba> {
        self.color.as_deref().and_then(parse_color)
    }
}

/// Parses a canvas color value: an Obsidian preset code (`"1"`–`"6"`) or a
/// hex string (`"#rgb"` / `"#rrggbb"`). Returns `None` for anything else.
pub fn parse_color(value: &str) -> Option<Rgba> {
    let hex = match value {
        "1" => "fb464c", // red
        "2" => "e9973f", // orange
        "3" => "e0de71", // yellow
        "4" => "44cf6e", // green
        "5" => "53dfdd", // cyan
        "6" => "a882ff", // purple
        other => other.strip_prefix('#')?,
    };
    match hex.len() {
        6 => Some(Rgba::from_rgb8(
            u8::from_str_radix(&hex[0..2], 16).ok()?,
            u8::from_str_radix(&hex[2..4], 16).ok()?,
            u8::from_str_radix(&hex[4..6], 16).ok()?,
        )),
        // #rgb shorthand: each nibble is doubled (f -> ff).
        3 => {
            let nibble = |s: &str| u8::from_str_radix(s, 16).ok().map(|v| v * 17);
            Some(Rgba::from_rgb8(
                nibble(&hex[0..1])?,
                nibble(&hex[1..2])?,
                nibble(&hex[2..3])?,
            ))
        }
        _ => None,
    }
}

/// Parses a CSS-style color from free text (e.g. clipboard contents): a hex
/// string (`#rgb`/`#rrggbb`) or `rgb(r, g, b)` / `rgba(r, g, b, a)`. Unlike
/// [`parse_color`], bare preset digits are not accepted (a stray "3" on the
/// clipboard shouldn't read as a color).
pub fn parse_css_color(value: &str) -> Option<Rgba> {
    let value = value.trim();
    if value.starts_with('#') {
        return parse_color(value);
    }
    let inner = value
        .strip_prefix("rgba(")
        .or_else(|| value.strip_prefix("rgb("))?
        .strip_suffix(')')?;
    let mut parts = inner.split(',');
    let mut channel = || -> Option<u8> {
        let raw: f32 = parts.next()?.trim().parse().ok()?;
        Some(raw.clamp(0.0, 255.0).round() as u8)
    };
    let (r, g, b) = (channel()?, channel()?, channel()?);
    Some(Rgba::from_rgb8(r, g, b))
}

/// HSV (all channels in [0, 1]) to an opaque RGB color. Used to build the hue
/// picker and rainbow wheel swatch.
pub fn hsv(h: f32, s: f32, v: f32) -> Rgba {
    let h6 = h.rem_euclid(1.0) * 6.0;
    let sector = h6.floor() as i32;
    let f = h6 - sector as f32;
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - s * f), v * (1.0 - s * (1.0 - f)));
    let (r, g, b) = match sector.rem_euclid(6) {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    Rgba::from_rgb(r, g, b)
}

/// Formats a color as `#rrggbb`, the canonical form stored for picked and
/// clipboard colors.
pub fn color_to_hex(color: Rgba) -> String {
    let byte = |c: f32| (c.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!(
        "#{:02x}{:02x}{:02x}",
        byte(color.r),
        byte(color.g),
        byte(color.b)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A frozen copy of the sample, compiled in so tests don't depend on the
    /// app's live (user-editable) canvas.
    const FIXTURE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/testdata/fixtures/sample.canvas"
    ));

    #[test]
    fn parses_sample_canvas() {
        let doc: CanvasDocument = serde_json::from_str(FIXTURE).unwrap();
        assert_eq!(doc.nodes.len(), 8);
        assert_eq!(doc.edges.len(), 6);
        assert_eq!(doc.nodes.iter().filter(|n| n.kind == "file").count(), 2);
        // Node with the extra `ai` field parses fine.
        assert!(doc.nodes.iter().any(|n| n.id == "ai_summary"));
    }

    #[test]
    fn round_trips_preserving_unknown_fields() {
        let doc: CanvasDocument = serde_json::from_str(FIXTURE).unwrap();
        let json = doc.to_canvas_json().unwrap();

        // Custom fields survive the round-trip.
        assert!(json.contains(r#""ai":"done""#));
        // Coordinates stay integers, not `-660.0`.
        assert!(json.contains(r#""x":-660"#));
        assert!(!json.contains(".0,"));
        // Obsidian structure: tab-pretty top level, compact node objects.
        assert!(json.starts_with("{\n\t\"nodes\":[\n\t\t{\"id\":"));

        // Reparsing yields the same document.
        let reparsed: CanvasDocument = serde_json::from_str(&json).unwrap();
        assert_eq!(reparsed.nodes.len(), doc.nodes.len());
        assert_eq!(reparsed.edges.len(), doc.edges.len());
        let node = reparsed
            .nodes
            .iter()
            .find(|n| n.id == "ai_summary")
            .unwrap();
        assert_eq!(node.extra.get("ai").and_then(Value::as_str), Some("done"));
    }

    #[test]
    fn round_trip_is_lossless() {
        // The whole document survives load → save with no data loss: every
        // node/edge field, color, coordinate, and unmodelled field (top-level
        // and per-node) is byte-for-byte reproduced except for JSON key order.
        // (`#[serde(flatten)]` writes unknown fields last, so a value like `ai`
        // moves to the end of its object; key order is not semantically
        // significant, so a Value comparison ignores it.)
        let doc: CanvasDocument = serde_json::from_str(FIXTURE).unwrap();
        let original: Value = serde_json::from_str(FIXTURE).unwrap();
        let reserialized: Value = serde_json::from_str(&doc.to_canvas_json().unwrap()).unwrap();
        assert_eq!(original, reserialized);
    }

    fn edge(id: &str) -> CanvasEdge {
        CanvasEdge {
            id: id.to_owned(),
            from_node: "a".to_owned(),
            from_side: "right".to_owned(),
            to_node: "b".to_owned(),
            to_side: "left".to_owned(),
            extra: Map::new(),
        }
    }

    #[test]
    fn remove_edge_drops_the_indexed_link_and_keeps_the_rest() {
        let mut edges = vec![edge("e0"), edge("e1"), edge("e2")];

        assert!(remove_edge(&mut edges, 1));

        assert_eq!(
            edges.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            ["e0", "e2"]
        );
    }

    #[test]
    fn remove_edge_ignores_an_out_of_range_index() {
        let mut edges = vec![edge("e0"), edge("e1")];

        // A stale selection index (list has shrunk) must not panic or change.
        assert!(!remove_edge(&mut edges, 9));
        assert_eq!(edges.len(), 2);
    }

    #[test]
    fn remove_edge_from_an_empty_list_is_a_noop() {
        let mut edges: Vec<CanvasEdge> = Vec::new();
        assert!(!remove_edge(&mut edges, 0));
        assert!(edges.is_empty());
    }

    #[test]
    fn add_edits_and_moves_nodes() {
        let mut doc = CanvasDocument::from_json(r#"{"nodes":[],"edges":[]}"#).unwrap();
        let a = doc.add_text_node("hello".to_owned(), 10.0, 20.0, 200.0, 100.0);
        assert_eq!(doc.nodes.len(), 1);
        assert_eq!(doc.nodes[0].text.as_deref(), Some("hello"));

        assert!(doc.set_node_text(&a, "world".to_owned()));
        assert_eq!(doc.nodes[0].text.as_deref(), Some("world"));
        assert!(doc.move_node(&a, 5.0, 6.0));
        assert_eq!((doc.nodes[0].x, doc.nodes[0].y), (5.0, 6.0));

        // Unknown ids are no-ops, not panics.
        assert!(!doc.set_node_text("nope", "x".to_owned()));
        assert!(!doc.move_node("nope", 0.0, 0.0));
    }

    #[test]
    fn removing_a_node_drops_its_edges() {
        let mut doc = CanvasDocument::from_json(r#"{"nodes":[],"edges":[]}"#).unwrap();
        let a = doc.add_text_node("a".to_owned(), 0.0, 0.0, 10.0, 10.0);
        let b = doc.add_text_node("b".to_owned(), 0.0, 0.0, 10.0, 10.0);
        doc.add_edge(a.clone(), "right".to_owned(), b, "left".to_owned());
        assert_eq!(doc.edges.len(), 1);

        assert!(doc.remove_node(&a));
        assert_eq!(doc.nodes.len(), 1);
        assert!(doc.edges.is_empty(), "the edge touching the removed node is gone");
        assert!(!doc.remove_node("nope"));
    }

    #[test]
    fn adds_and_removes_edges_by_id() {
        let mut doc = CanvasDocument::from_json(r#"{"nodes":[],"edges":[]}"#).unwrap();
        let a = doc.add_text_node("a".to_owned(), 0.0, 0.0, 10.0, 10.0);
        let b = doc.add_text_node("b".to_owned(), 0.0, 0.0, 10.0, 10.0);
        let e = doc.add_edge(a, "bottom".to_owned(), b, "top".to_owned());
        assert_eq!(doc.edges.len(), 1);

        assert!(doc.remove_edge_by_id(&e));
        assert!(doc.edges.is_empty());
        assert!(!doc.remove_edge_by_id(&e));
    }

    #[test]
    fn add_child_links_the_new_node_to_its_parent() {
        let mut doc = CanvasDocument::from_json(r#"{"nodes":[],"edges":[]}"#).unwrap();
        let parent = doc.add_text_node("parent".to_owned(), 0.0, 0.0, 250.0, 60.0);

        let child = doc.add_child(&parent, String::new()).unwrap();
        assert_eq!(doc.nodes.len(), 2);
        assert_eq!(doc.edges.len(), 1);
        assert_eq!(doc.edges[0].from_node, parent);
        assert_eq!(doc.edges[0].to_node, child);
        // Placed clear of the parent (to its right).
        let c = doc.nodes.iter().find(|n| n.id == child).unwrap();
        assert!(c.x >= 250.0, "child sits past the parent's right edge");

        assert!(doc.add_child("nope", String::new()).is_none());
    }

    #[test]
    fn add_sibling_shares_the_parent_or_starts_a_cluster() {
        let mut doc = CanvasDocument::from_json(r#"{"nodes":[],"edges":[]}"#).unwrap();
        let parent = doc.add_text_node("parent".to_owned(), 0.0, 0.0, 250.0, 60.0);
        let child = doc.add_child(&parent, String::new()).unwrap();

        // A sibling of the child hangs off the same parent.
        let sib = doc.add_sibling(&child, String::new()).unwrap();
        assert_eq!(doc.edges.iter().filter(|e| e.to_node == sib).count(), 1);
        assert!(doc.edges.iter().any(|e| e.from_node == parent && e.to_node == sib));

        // A sibling of a root (no parent) is a fresh, unconnected cluster.
        let cluster = doc.add_sibling(&parent, String::new()).unwrap();
        assert!(doc.edges.iter().all(|e| e.to_node != cluster));
    }

    #[test]
    fn add_root_places_a_disconnected_node_clear_of_others() {
        let mut doc = CanvasDocument::from_json(r#"{"nodes":[],"edges":[]}"#).unwrap();
        doc.add_text_node("a".to_owned(), 0.0, 0.0, 250.0, 60.0);

        let root = doc.add_root(String::new());
        assert!(doc.edges.is_empty(), "a new cluster has no edges");
        let r = doc.nodes.iter().find(|n| n.id == root).unwrap();
        // Below the existing node's bounding box, so they don't overlap.
        assert!(r.y >= 60.0);
    }

    #[test]
    fn free_placement_avoids_overlap() {
        let mut doc = CanvasDocument::from_json(r#"{"nodes":[],"edges":[]}"#).unwrap();
        let p = doc.add_text_node("p".to_owned(), 0.0, 0.0, 250.0, 60.0);
        // Two children at the same requested spot must not land on each other.
        let a = doc.add_child(&p, String::new()).unwrap();
        let b = doc.add_child(&p, String::new()).unwrap();
        let ra = doc.nodes.iter().find(|n| n.id == a).unwrap();
        let rb = doc.nodes.iter().find(|n| n.id == b).unwrap();
        assert!(
            !rects_overlap(
                (ra.x, ra.y, ra.width, ra.height),
                (rb.x, rb.y, rb.width, rb.height)
            ),
            "sibling children should not overlap"
        );
    }

    #[test]
    fn parse_color_reads_obsidian_preset_codes() {
        assert_eq!(parse_color("1"), Some(Rgba::from_rgb8(0xfb, 0x46, 0x4c)));
        assert_eq!(parse_color("4"), Some(Rgba::from_rgb8(0x44, 0xcf, 0x6e)));
    }

    #[test]
    fn parse_color_reads_full_hex() {
        assert_eq!(parse_color("#4a90e2"), Some(Rgba::from_rgb8(74, 144, 226)));
    }

    #[test]
    fn parse_color_expands_shorthand_hex() {
        // #f0a -> #ff00aa
        assert_eq!(
            parse_color("#f0a"),
            Some(Rgba::from_rgb8(0xff, 0x00, 0xaa))
        );
    }

    #[test]
    fn parse_color_rejects_garbage() {
        assert_eq!(parse_color("nope"), None);
        assert_eq!(parse_color("#12"), None);
        assert_eq!(parse_color("#zzzzzz"), None);
    }

    #[test]
    fn parse_css_color_reads_hex_and_rgb() {
        let expected = Rgba::from_rgb8(74, 144, 226);
        assert_eq!(parse_css_color("#4a90e2"), Some(expected));
        assert_eq!(parse_css_color("rgb(74, 144, 226)"), Some(expected));
        assert_eq!(parse_css_color("  rgba(74,144,226,0.5) "), Some(expected));
    }

    #[test]
    fn parse_css_color_rejects_non_colors_and_bare_digits() {
        // A stray preset digit on the clipboard is not a color.
        assert_eq!(parse_css_color("3"), None);
        assert_eq!(parse_css_color("hello"), None);
        assert_eq!(parse_css_color("rgb(1,2)"), None);
    }

    #[test]
    fn color_to_hex_round_trips() {
        assert_eq!(color_to_hex(Rgba::from_rgb8(74, 144, 226)), "#4a90e2");
        assert_eq!(color_to_hex(Rgba::from_rgb8(0xff, 0x00, 0xaa)), "#ff00aa");
    }

    #[test]
    fn hsv_hue_vertices_are_pure_colors() {
        // Full-saturation, full-value sweep hits the primaries/secondaries.
        // (Compared via hex so the tiny float error at the vertices rounds
        // away.)
        assert_eq!(color_to_hex(hsv(0.0, 1.0, 1.0)), "#ff0000"); // red
        assert_eq!(color_to_hex(hsv(1.0 / 6.0, 1.0, 1.0)), "#ffff00"); // yellow
        assert_eq!(color_to_hex(hsv(1.0 / 3.0, 1.0, 1.0)), "#00ff00"); // green
        assert_eq!(color_to_hex(hsv(0.5, 1.0, 1.0)), "#00ffff"); // cyan
        assert_eq!(color_to_hex(hsv(2.0 / 3.0, 1.0, 1.0)), "#0000ff"); // blue
        assert_eq!(color_to_hex(hsv(5.0 / 6.0, 1.0, 1.0)), "#ff00ff"); // magenta
    }

    #[test]
    fn hsv_zero_saturation_is_gray() {
        assert_eq!(color_to_hex(hsv(0.37, 0.0, 0.5)), "#808080");
    }

    #[test]
    fn hsv_wraps_hue_past_one() {
        // Hue 1.0 wraps back to red.
        assert_eq!(color_to_hex(hsv(1.0, 1.0, 1.0)), "#ff0000");
    }
}

