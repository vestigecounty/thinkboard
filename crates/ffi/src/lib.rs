//! uniffi bindings exposing [`thinkboard_core`] to a Swift/iOS front-end.
//!
//! This crate is the *only* place that knows about the FFI. It keeps the core
//! crate binding-free by wrapping it: read-only [`Node`]/[`Edge`] records that
//! the UI renders, and an opaque, mutable [`Document`] that owns the full
//! parsed model so serialization stays lossless.
//!
//! First slice: parse a `.canvas`, read its nodes/edges, serialize it back.
//! Layout and the AI copilot come next.

use std::sync::{Arc, Mutex};

use thinkboard_core::model::{self, CanvasDocument};
use thinkboard_core::{layout, markdown, syntax};

uniffi::setup_scaffolding!();

/// The core version string — a trivial end-to-end smoke test that the Rust
/// library links and calls from Swift.
#[uniffi::export]
pub fn core_version() -> String {
    env!("CARGO_PKG_VERSION").to_owned()
}

/// A canvas node, flattened to a plain record the UI can render. Unmodelled
/// fields are not exposed here but are preserved inside [`Document`].
#[derive(uniffi::Record, Clone)]
pub struct Node {
    pub id: String,
    /// `"text"` or `"file"` (Obsidian's node kinds).
    pub kind: String,
    pub text: Option<String>,
    pub file: Option<String>,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    /// Obsidian color: a preset code (`"1"`–`"6"`) or `#hex`, if any.
    pub color: Option<String>,
}

/// A directed link between two nodes' sides.
#[derive(uniffi::Record, Clone)]
pub struct Edge {
    pub id: String,
    pub from_node: String,
    pub from_side: String,
    pub to_node: String,
    pub to_side: String,
}

/// Failure reading or writing a canvas document.
#[derive(uniffi::Error, Debug)]
pub enum CanvasError {
    Parse { message: String },
    Serialize { message: String },
}

impl std::fmt::Display for CanvasError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CanvasError::Parse { message } => write!(f, "parse error: {message}"),
            CanvasError::Serialize { message } => write!(f, "serialize error: {message}"),
        }
    }
}

impl std::error::Error for CanvasError {}

/// An opaque, mutable canvas document. It owns the full parsed model —
/// including fields ThinkBoard doesn't model — so [`Document::to_json`]
/// round-trips losslessly. The `Mutex` makes it `Send + Sync` for the Swift
/// side, which holds it behind an `Arc`.
#[derive(uniffi::Object)]
pub struct Document {
    inner: Mutex<CanvasDocument>,
}

#[uniffi::export]
impl Document {
    /// Parse a `.canvas` document from JSON text.
    #[uniffi::constructor]
    pub fn parse(json: String) -> Result<Arc<Self>, CanvasError> {
        let doc =
            CanvasDocument::from_json(&json).map_err(|message| CanvasError::Parse { message })?;
        Ok(Arc::new(Self { inner: Mutex::new(doc) }))
    }

    /// The document's nodes, in file order.
    pub fn nodes(&self) -> Vec<Node> {
        self.inner.lock().unwrap().nodes.iter().map(Node::from).collect()
    }

    /// The document's edges, in file order.
    pub fn edges(&self) -> Vec<Edge> {
        self.inner.lock().unwrap().edges.iter().map(Edge::from).collect()
    }

    /// Serialize back to Obsidian canvas JSON, preserving unmodelled fields.
    pub fn to_json(&self) -> Result<String, CanvasError> {
        self.inner
            .lock()
            .unwrap()
            .to_canvas_json()
            .map_err(|message| CanvasError::Serialize { message })
    }

    /// Lay out every node as a layered graph flowing in `direction`, returning
    /// each node's computed top-left position keyed by node id. `rankGap` is
    /// the spacing between ranks (layout depth); `nodeGap` between siblings
    /// within a rank. Non-mutating: the caller decides whether to apply the
    /// result.
    pub fn layout(
        &self,
        direction: LayoutDirection,
        rank_gap: f32,
        node_gap: f32,
    ) -> Vec<LayoutPosition> {
        let doc = self.inner.lock().unwrap();
        let selected: Vec<usize> = (0..doc.nodes.len()).collect();
        layout::layered_layout(&doc.nodes, &doc.edges, &selected, direction.into(), rank_gap, node_gap)
            .into_iter()
            .map(|(i, x, y)| LayoutPosition { id: doc.nodes[i].id.clone(), x, y })
            .collect()
    }

    // --- Mutation (see the matching methods on core's `CanvasDocument`) ------

    /// Add a text node at `(x, y)` with the given size; returns its new id.
    pub fn add_text_node(&self, text: String, x: f32, y: f32, width: f32, height: f32) -> String {
        self.inner.lock().unwrap().add_text_node(text, x, y, width, height)
    }

    /// Set a node's text; returns whether `id` matched.
    pub fn set_node_text(&self, id: String, text: String) -> bool {
        self.inner.lock().unwrap().set_node_text(&id, text)
    }

    /// Set or clear a node's color (an Obsidian preset code or hex string; pass
    /// `None` to clear it). Returns whether `id` matched.
    pub fn set_node_color(&self, id: String, color: Option<String>) -> bool {
        self.inner.lock().unwrap().set_node_color(&id, color)
    }

    /// Move a node's top-left corner; returns whether `id` matched.
    pub fn move_node(&self, id: String, x: f32, y: f32) -> bool {
        self.inner.lock().unwrap().move_node(&id, x, y)
    }

    /// Remove a node and every edge touching it; returns whether it existed.
    pub fn remove_node(&self, id: String) -> bool {
        self.inner.lock().unwrap().remove_node(&id)
    }

    /// Add an edge between two nodes' sides; returns its new id.
    pub fn add_edge(
        &self,
        from_node: String,
        from_side: String,
        to_node: String,
        to_side: String,
    ) -> String {
        self.inner.lock().unwrap().add_edge(from_node, from_side, to_node, to_side)
    }

    /// Remove the edge with `id`; returns whether it existed.
    pub fn remove_edge(&self, id: String) -> bool {
        self.inner.lock().unwrap().remove_edge_by_id(&id)
    }

    /// Add a child of `parent_id` (placed and linked from the parent); returns
    /// the new node's id, or `None` if the parent doesn't exist.
    pub fn add_child(&self, parent_id: String, text: String) -> Option<String> {
        self.inner.lock().unwrap().add_child(&parent_id, text)
    }

    /// Add a sibling of `node_id` (same parent, or a new cluster for a root);
    /// returns the new node's id, or `None` if `node_id` doesn't exist.
    pub fn add_sibling(&self, node_id: String, text: String) -> Option<String> {
        self.inner.lock().unwrap().add_sibling(&node_id, text)
    }

    /// Add a standalone node as a new cluster in empty space; returns its id.
    pub fn add_root(&self, text: String) -> String {
        self.inner.lock().unwrap().add_root(text)
    }

    /// Lay out every node (as [`Document::layout`]) and write the new positions
    /// back into the document, so a following [`Document::to_json`] persists it.
    pub fn apply_layout(&self, direction: LayoutDirection, rank_gap: f32, node_gap: f32) {
        let mut doc = self.inner.lock().unwrap();
        let selected: Vec<usize> = (0..doc.nodes.len()).collect();
        let positions = layout::layered_layout(
            &doc.nodes,
            &doc.edges,
            &selected,
            direction.into(),
            rank_gap,
            node_gap,
        );
        for (i, x, y) in positions {
            doc.nodes[i].x = x;
            doc.nodes[i].y = y;
        }
    }
}

impl From<&model::CanvasNode> for Node {
    fn from(n: &model::CanvasNode) -> Self {
        Node {
            id: n.id.clone(),
            kind: n.kind.clone(),
            text: n.text.clone(),
            file: n.file.clone(),
            x: n.x,
            y: n.y,
            width: n.width,
            height: n.height,
            color: n.color.clone(),
        }
    }
}

impl From<&model::CanvasEdge> for Edge {
    fn from(e: &model::CanvasEdge) -> Self {
        Edge {
            id: e.id.clone(),
            from_node: e.from_node.clone(),
            from_side: e.from_side.clone(),
            to_node: e.to_node.clone(),
            to_side: e.to_side.clone(),
        }
    }
}

// --- Auto-layout -----------------------------------------------------------

/// Flow direction for [`Document::layout`].
#[derive(uniffi::Enum)]
pub enum LayoutDirection {
    /// Top to bottom: ranks stack vertically, siblings spread horizontally.
    Down,
    /// Left to right: ranks stack horizontally, siblings spread vertically.
    Right,
}

impl From<LayoutDirection> for layout::Direction {
    fn from(d: LayoutDirection) -> Self {
        match d {
            LayoutDirection::Down => layout::Direction::Down,
            LayoutDirection::Right => layout::Direction::Right,
        }
    }
}

/// A node's computed top-left position, from [`Document::layout`].
#[derive(uniffi::Record)]
pub struct LayoutPosition {
    pub id: String,
    pub x: f32,
    pub y: f32,
}

// --- Color -----------------------------------------------------------------

/// An RGBA color, channels in `0.0..=1.0`, resolved from a canvas color value.
#[derive(uniffi::Record)]
pub struct RgbaColor {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

/// Resolve a node's canvas color value — an Obsidian preset code (`"1"`–`"6"`)
/// or a hex string (`"#rgb"` / `"#rrggbb"`) — to RGBA. `None` for anything
/// unrecognized. Custom `theme.css` colors are not applied.
#[uniffi::export]
pub fn parse_color(value: String) -> Option<RgbaColor> {
    model::parse_color(&value).map(|c| RgbaColor { r: c.r, g: c.g, b: c.b, a: c.a })
}

// --- Markdown rendering-prep -----------------------------------------------

/// Syntax-highlight class of a span inside a fenced code block (`Plain`
/// everywhere else).
#[derive(uniffi::Enum)]
pub enum SyntaxClass {
    Plain,
    Keyword,
    Type,
    String,
    Number,
    Comment,
    Function,
}

impl From<syntax::Syntax> for SyntaxClass {
    fn from(s: syntax::Syntax) -> Self {
        match s {
            syntax::Syntax::Plain => SyntaxClass::Plain,
            syntax::Syntax::Keyword => SyntaxClass::Keyword,
            syntax::Syntax::Type => SyntaxClass::Type,
            syntax::Syntax::String => SyntaxClass::String,
            syntax::Syntax::Number => SyntaxClass::Number,
            syntax::Syntax::Comment => SyntaxClass::Comment,
            syntax::Syntax::Function => SyntaxClass::Function,
        }
    }
}

/// A run of note text sharing one style — the unit a renderer draws. Block
/// breaks arrive as spans of `"\n\n"`; list items get a `"• "` prefix.
#[derive(uniffi::Record)]
pub struct MarkdownSpan {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    /// Monospace (inline code or code block).
    pub code: bool,
    /// 0 = body text; 1–6 = heading level.
    pub heading: u8,
    pub link: bool,
    /// The link's destination, when this span is part of a link.
    pub url: Option<String>,
    pub syntax: SyntaxClass,
}

impl From<markdown::Span> for MarkdownSpan {
    fn from(s: markdown::Span) -> Self {
        MarkdownSpan {
            text: s.text,
            bold: s.bold,
            italic: s.italic,
            code: s.code,
            heading: s.heading,
            link: s.link,
            url: s.url,
            syntax: s.syntax.into(),
        }
    }
}

/// Parse note text into styled spans for rendering. A table arrives as rows
/// of text, its cells separated by `" | "` — the spans carry no grid.
#[uniffi::export]
pub fn parse_markdown(source: String) -> Vec<MarkdownSpan> {
    markdown::flatten_tables(markdown::parse(&source)).into_iter().map(MarkdownSpan::from).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r##"{"nodes":[
        {"id":"a","type":"text","text":"# Title","x":0,"y":0,"width":200,"height":100,"ai":"kept"},
        {"id":"b","type":"text","text":"child","x":0,"y":0,"width":200,"height":100}
    ],"edges":[{"id":"e","fromNode":"a","toNode":"b"}]}"##;

    #[test]
    fn parses_nodes_and_edges() {
        let doc = Document::parse(SAMPLE.to_owned()).unwrap();
        assert_eq!(doc.nodes().len(), 2);
        assert_eq!(doc.edges().len(), 1);
        assert_eq!(doc.nodes()[0].id, "a");
        assert_eq!(doc.edges()[0].from_node, "a");
    }

    #[test]
    fn to_json_preserves_unmodelled_fields() {
        let doc = Document::parse(SAMPLE.to_owned()).unwrap();
        let json = doc.to_json().unwrap();
        // The custom `ai` field survives the wrap → serialize round-trip.
        assert!(json.contains(r#""ai":"kept""#));
        assert_eq!(Document::parse(json).unwrap().nodes().len(), 2);
    }

    #[test]
    fn layout_positions_every_node() {
        let doc = Document::parse(SAMPLE.to_owned()).unwrap();
        let positions = doc.layout(LayoutDirection::Right, 80.0, 24.0);
        let ids: Vec<&str> = positions.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&"a") && ids.contains(&"b"));
    }

    #[test]
    fn parse_color_resolves_presets_and_hex() {
        assert!(parse_color("1".to_owned()).is_some()); // preset
        assert!(parse_color("#4a90e2".to_owned()).is_some()); // hex
        assert!(parse_color("nope".to_owned()).is_none());
    }

    #[test]
    fn parse_markdown_marks_a_heading() {
        let spans = parse_markdown("# Title".to_owned());
        assert!(spans.iter().any(|s| s.heading == 1 && s.text.contains("Title")));
    }

    #[test]
    fn mutates_and_persists_through_the_wrapper() {
        let doc = Document::parse(r##"{"nodes":[],"edges":[]}"##.to_owned()).unwrap();
        let a = doc.add_text_node("a".to_owned(), 0.0, 0.0, 100.0, 40.0);
        let b = doc.add_text_node("b".to_owned(), 0.0, 0.0, 100.0, 40.0);
        doc.add_edge(a.clone(), "right".to_owned(), b, "left".to_owned());
        assert_eq!(doc.nodes().len(), 2);
        assert_eq!(doc.edges().len(), 1);

        doc.apply_layout(LayoutDirection::Right, 80.0, 24.0);
        // Mutations survive serialize → reparse.
        let json = doc.to_json().unwrap();
        assert_eq!(Document::parse(json).unwrap().nodes().len(), 2);

        assert!(doc.remove_node(a));
        assert_eq!(doc.nodes().len(), 1);
        assert!(doc.edges().is_empty());
    }
}
