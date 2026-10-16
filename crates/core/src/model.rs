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
    /// round-tripping never drops another tool's data. A group's `label` and a
    /// link's `url` live here too: the app reads them (see [`CanvasNode::label`]
    /// and [`CanvasNode::url`]) but never writes them, and `extra` already
    /// round-trips them untouched.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// What a node actually *is*, resolved from its `type` and (for file nodes) the
/// extension of the file it points at.
///
/// The canvas format stores `type` as one of `"text"`, `"file"`, `"link"` or
/// `"group"`, which leaves the interesting distinction — a `file` node embedding
/// a Markdown note versus one embedding an image — implicit in the path. Callers
/// should match on this rather than comparing [`CanvasNode::kind`] to strings,
/// so that distinction lives in exactly one place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    /// A note typed on the canvas itself; the only editable kind.
    Text,
    /// A file node embedding an image.
    Image,
    /// A file node embedding a Markdown note from the vault.
    Note,
    /// A file node embedding something we can't render (a PDF, a video, …);
    /// drawn as a card naming the file.
    Attachment,
    /// A URL bookmark card.
    Link,
    /// A labelled frame drawn behind the nodes it encloses.
    Group,
}

/// File extensions Obsidian renders as images in a canvas.
const IMAGE_EXTENSIONS: [&str; 8] = ["png", "jpg", "jpeg", "gif", "bmp", "svg", "webp", "avif"];

impl CanvasNode {
    /// What this node is (see [`NodeKind`]). An unrecognized `type` is treated
    /// as [`NodeKind::Text`], which is what the format's own default is and what
    /// every earlier version of this app assumed.
    pub fn node_kind(&self) -> NodeKind {
        match self.kind.as_str() {
            "group" => NodeKind::Group,
            "link" => NodeKind::Link,
            "file" => match self.file_extension().as_deref() {
                Some("md") | Some("markdown") => NodeKind::Note,
                Some(ext) if IMAGE_EXTENSIONS.contains(&ext) => NodeKind::Image,
                // A file node with no extension at all is most likely an
                // Obsidian note (its wiki-links often omit the `.md`).
                None => NodeKind::Note,
                Some(_) => NodeKind::Attachment,
            },
            _ => NodeKind::Text,
        }
    }

    /// The lowercased extension of this node's `file`, if it has one.
    fn file_extension(&self) -> Option<String> {
        let file = self.file.as_deref()?;
        let name = file.rsplit(['/', '\\']).next()?;
        let (_, ext) = name.rsplit_once('.')?;
        (!ext.is_empty()).then(|| ext.to_ascii_lowercase())
    }

    /// The display name of the file a file node points at — its base name with
    /// any extension dropped, which for a note is the Obsidian note title.
    pub fn file_title(&self) -> Option<String> {
        let file = self.file.as_deref()?;
        let name = file.rsplit(['/', '\\']).next()?;
        Some(match name.rsplit_once('.') {
            Some((stem, _)) if !stem.is_empty() => stem.to_owned(),
            _ => name.to_owned(),
        })
    }

    /// Whether this node holds text the user types on the canvas — the only
    /// kind that is editable, auto-sized, searched and rasterized.
    pub fn is_text(&self) -> bool {
        self.node_kind() == NodeKind::Text
    }

    /// Whether this node is a frame rather than a card. Groups are geometry the
    /// user arranges *around* other nodes, so they take no part in clustering,
    /// hit-testing (beyond their header), linking or auto-layout.
    pub fn is_group(&self) -> bool {
        self.node_kind() == NodeKind::Group
    }

    /// A group node's caption, drawn above its frame.
    pub fn label(&self) -> Option<&str> {
        self.extra.get("label").and_then(Value::as_str)
    }

    /// A link node's target URL.
    pub fn url(&self) -> Option<&str> {
        self.extra.get("url").and_then(Value::as_str)
    }

    /// Sets (or clears) a group's caption.
    ///
    /// Blank is stored as *absent* rather than as an empty string: the format
    /// leaves `label` optional, and a node carrying `"label": ""` would round-
    /// trip that emptiness back into the vault as though it were meaningful.
    pub fn set_label(&mut self, label: &str) {
        let trimmed = label.trim();
        if trimmed.is_empty() {
            self.extra.remove("label");
        } else {
            self.extra
                .insert("label".to_owned(), Value::String(trimmed.to_owned()));
        }
    }
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

/// How a link terminates at one of its ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeEnd {
    /// The line simply stops — an undirected link.
    None,
    Arrow,
}

impl CanvasEdge {
    /// The link's caption, drawn along the curve.
    ///
    /// Like a group's label and a link node's URL, this lives in `extra`, which
    /// round-trips it untouched; [`CanvasEdge::set_label`] writes it there.
    pub fn label(&self) -> Option<&str> {
        self.extra
            .get("label")
            .and_then(Value::as_str)
            .filter(|label| !label.trim().is_empty())
    }

    /// The link's own color, if the document gives it one. Same value space as
    /// a node's: an Obsidian preset code or a hex string.
    pub fn palette_color(&self) -> Option<Rgba> {
        self.extra
            .get("color")
            .and_then(Value::as_str)
            .and_then(parse_color)
    }

    /// How the link terminates at its source. Obsidian omits `fromEnd` for the
    /// common case, which means a bare end.
    pub fn from_end(&self) -> EdgeEnd {
        edge_end(self.extra.get("fromEnd"), EdgeEnd::None)
    }

    /// How the link terminates at its target — an arrowhead unless the document
    /// says otherwise.
    pub fn to_end(&self) -> EdgeEnd {
        edge_end(self.extra.get("toEnd"), EdgeEnd::Arrow)
    }

    /// Sets (or clears) the link's caption — blank stored as absent, for the
    /// reason a group's is (see [`CanvasNode::set_label`]).
    pub fn set_label(&mut self, label: &str) {
        let trimmed = label.trim();
        if trimmed.is_empty() {
            self.extra.remove("label");
        } else {
            self.extra.insert("label".to_owned(), Value::String(trimmed.to_owned()));
        }
    }

    /// Sets (or clears) the link's color: an Obsidian preset code or a hex
    /// string, as a node's is.
    pub fn set_color(&mut self, color: Option<&str>) {
        match color {
            Some(color) => self.extra.insert("color".to_owned(), Value::String(color.to_owned())),
            None => self.extra.remove("color"),
        };
    }

    /// Sets how the link ends at each node. A field is written only where it
    /// differs from what its absence means — a bare start, an arrow at the
    /// target — which is how Obsidian writes them, so a plain link stays a
    /// plain line in the file.
    pub fn set_ends(&mut self, from: EdgeEnd, to: EdgeEnd) {
        let mut put = |key: &str, end: EdgeEnd, absent: EdgeEnd| {
            if end == absent {
                self.extra.remove(key);
            } else {
                let value = if end == EdgeEnd::Arrow { "arrow" } else { "none" };
                self.extra.insert(key.to_owned(), Value::String(value.to_owned()));
            }
        };
        put("fromEnd", from, EdgeEnd::None);
        put("toEnd", to, EdgeEnd::Arrow);
    }
}

/// Reads an `arrow`/`none` end marker, falling back to `default` when the field
/// is absent or holds something unrecognized.
fn edge_end(value: Option<&Value>, default: EdgeEnd) -> EdgeEnd {
    match value.and_then(Value::as_str) {
        Some("arrow") => EdgeEnd::Arrow,
        Some("none") => EdgeEnd::None,
        _ => default,
    }
}

/// Indices of the nodes a group frame encloses — those whose centre falls
/// inside it, the frame itself excluded.
///
/// Centres rather than full containment, so a node poking over the frame's edge
/// still belongs to it, which is how it reads on screen. Nested frames come back
/// as members in their own right; a caller that cares about the nesting
/// re-derives it (see [`crate::ai::group_prompt`]).
pub fn enclosed_by(nodes: &[CanvasNode], frame: usize) -> Vec<usize> {
    let Some(bounds) = nodes.get(frame) else {
        return Vec::new();
    };
    let (left, top) = (bounds.x, bounds.y);
    let (right, bottom) = (bounds.x + bounds.width, bounds.y + bounds.height);
    nodes
        .iter()
        .enumerate()
        .filter(|&(i, node)| {
            if i == frame {
                return false;
            }
            let (cx, cy) = (node.x + node.width / 2.0, node.y + node.height / 2.0);
            cx >= left && cx <= right && cy >= top && cy <= bottom
        })
        .map(|(i, _)| i)
        .collect()
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
        write_whole(path, self.to_canvas_json()?.as_bytes())
            .map_err(|e| format!("cannot write {}: {e}", path.display()))
    }
}

/// Writes `bytes` to `path` in one step as far as a reader can tell: into a
/// file beside it, then renamed over it.
///
/// A plain write truncates the file and then fills it, and Obsidian watches
/// the vault and reads a changed canvas straight away — so it could read one
/// half-written, as could a sync client. Through a symlink it writes the file
/// the link names, since a rename over the link would replace the link with a
/// copy; and it keeps the file's permissions. Where the rename is refused — on
/// Windows, a file another program holds open — it falls back to writing in
/// place, which is what it always did.
fn write_whole(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let target = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let (Some(dir), Some(name)) = (target.parent(), target.file_name()) else {
        return std::fs::write(path, bytes);
    };
    // A dot-file, so neither Obsidian nor the vault browser lists it in the
    // moment it exists.
    let temp = dir.join(format!(".{}.{}.tmp", name.to_string_lossy(), random_id()));
    let staged = std::fs::write(&temp, bytes).and_then(|()| {
        if let Ok(metadata) = std::fs::metadata(&target) {
            std::fs::set_permissions(&temp, metadata.permissions())?;
        }
        std::fs::rename(&temp, &target)
    });
    if staged.is_err() {
        let _ = std::fs::remove_file(&temp);
        return std::fs::write(&target, bytes);
    }
    Ok(())
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

    fn node(json: serde_json::Value) -> CanvasNode {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn a_file_node_is_an_image_or_a_note_by_its_extension() {
        use serde_json::json;
        let of = |file: &str| node(json!({"id": "n", "type": "file", "file": file})).node_kind();

        assert_eq!(of("assets/sketch.png"), NodeKind::Image);
        assert_eq!(of("assets/photo.JPEG"), NodeKind::Image); // case-insensitive
        assert_eq!(of("Notes/Ideas.md"), NodeKind::Note);
        assert_eq!(of("Notes/Ideas.markdown"), NodeKind::Note);
        // Obsidian's links often omit the extension; those are notes.
        assert_eq!(of("Notes/Ideas"), NodeKind::Note);
        // Anything else is an attachment we name but can't render.
        assert_eq!(of("refs/paper.pdf"), NodeKind::Attachment);
    }

    #[test]
    fn the_other_types_resolve_from_the_type_field() {
        use serde_json::json;
        assert_eq!(node(json!({"id": "n", "type": "text"})).node_kind(), NodeKind::Text);
        assert_eq!(node(json!({"id": "n", "type": "group"})).node_kind(), NodeKind::Group);
        assert_eq!(node(json!({"id": "n", "type": "link"})).node_kind(), NodeKind::Link);
        // An unknown type falls back to a plain card rather than vanishing.
        assert_eq!(node(json!({"id": "n", "type": "wat"})).node_kind(), NodeKind::Text);
    }

    #[test]
    fn a_file_title_is_the_base_name_without_its_extension() {
        use serde_json::json;
        let title = |file: &str| node(json!({"id": "n", "type": "file", "file": file})).file_title();
        assert_eq!(title("Notes/Deep/Ideas.md").as_deref(), Some("Ideas"));
        assert_eq!(title("Ideas").as_deref(), Some("Ideas"));
        // A dotfile keeps its name rather than becoming empty.
        assert_eq!(title(".hidden").as_deref(), Some(".hidden"));
        assert_eq!(node(json!({"id": "n", "type": "text"})).file_title(), None);
    }

    #[test]
    fn a_groups_label_and_a_links_url_are_read_from_the_document() {
        use serde_json::json;
        let group = node(json!({"id": "g", "type": "group", "label": "Ideas"}));
        assert_eq!(group.label(), Some("Ideas"));
        assert!(group.is_group());

        let link = node(json!({"id": "l", "type": "link", "url": "https://example.com"}));
        assert_eq!(link.url(), Some("https://example.com"));

        // Absent on the kinds that don't carry them.
        assert_eq!(node(json!({"id": "n", "type": "text"})).label(), None);
    }

    fn edge_json(json: serde_json::Value) -> CanvasEdge {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn a_blank_label_is_stored_as_absent() {
        use serde_json::json;
        let mut group = node(json!({"id": "g", "type": "group", "label": "Ideas"}));
        assert_eq!(group.label(), Some("Ideas"));

        group.set_label("  Planning  ");
        assert_eq!(group.label(), Some("Planning"), "stored trimmed");

        // Clearing removes the key rather than writing "", so the saved canvas
        // doesn't carry an empty caption back into the vault.
        group.set_label("   ");
        assert_eq!(group.label(), None);
        assert!(!group.extra.contains_key("label"));
    }

    #[test]
    fn an_edges_ends_default_the_way_obsidian_omits_them() {
        use serde_json::json;
        let bare = edge_json(json!({"id": "e", "fromNode": "a", "toNode": "b"}));
        // The common case is written with neither field: a plain arrow.
        assert_eq!(bare.from_end(), EdgeEnd::None);
        assert_eq!(bare.to_end(), EdgeEnd::Arrow);

        let two_way = edge_json(json!({
            "id": "e", "fromNode": "a", "toNode": "b",
            "fromEnd": "arrow", "toEnd": "arrow",
        }));
        assert_eq!(two_way.from_end(), EdgeEnd::Arrow);
        assert_eq!(two_way.to_end(), EdgeEnd::Arrow);

        let undirected = edge_json(json!({
            "id": "e", "fromNode": "a", "toNode": "b", "toEnd": "none",
        }));
        assert_eq!(undirected.to_end(), EdgeEnd::None);

        // Junk falls back to the default rather than losing the arrow.
        let junk = edge_json(json!({"id": "e", "fromNode": "a", "toNode": "b", "toEnd": "wat"}));
        assert_eq!(junk.to_end(), EdgeEnd::Arrow);
    }

    #[test]
    fn setting_an_edges_ends_writes_only_what_differs_from_absent() {
        use serde_json::json;
        let mut edge = edge_json(json!({"id": "e", "fromNode": "a", "toNode": "b"}));

        edge.set_ends(EdgeEnd::Arrow, EdgeEnd::Arrow);
        assert_eq!(edge.extra.get("fromEnd"), Some(&json!("arrow")));
        assert!(!edge.extra.contains_key("toEnd"), "an arrow at the target is the default");

        edge.set_ends(EdgeEnd::None, EdgeEnd::None);
        assert!(!edge.extra.contains_key("fromEnd"));
        assert_eq!(edge.extra.get("toEnd"), Some(&json!("none")));

        // Back to a plain link: back to a bare line in the file.
        edge.set_ends(EdgeEnd::None, EdgeEnd::Arrow);
        assert!(!edge.extra.contains_key("fromEnd") && !edge.extra.contains_key("toEnd"));
    }

    #[test]
    fn an_edges_label_and_color_set_and_clear() {
        use serde_json::json;
        let mut edge = edge_json(json!({"id": "e", "fromNode": "a", "toNode": "b"}));
        edge.set_label("  flies to  ");
        edge.set_color(Some("4"));
        assert_eq!(edge.label(), Some("flies to"));
        assert_eq!(edge.extra.get("color"), Some(&json!("4")));

        edge.set_label("   ");
        edge.set_color(None);
        assert!(!edge.extra.contains_key("label"), "blank is absent, not empty");
        assert!(!edge.extra.contains_key("color"));
    }

    #[test]
    fn an_edges_label_and_color_are_read_from_the_document() {
        use serde_json::json;
        let e = edge_json(json!({
            "id": "e", "fromNode": "a", "toNode": "b",
            "label": "depends on", "color": "4",
        }));
        assert_eq!(e.label(), Some("depends on"));
        assert!(e.palette_color().is_some());

        let bare = edge_json(json!({"id": "e", "fromNode": "a", "toNode": "b"}));
        assert_eq!(bare.label(), None);
        assert_eq!(bare.palette_color(), None);

        // A blank label is the same as none — it must not reserve a chip.
        let blank = edge_json(json!({"id": "e", "fromNode": "a", "toNode": "b", "label": "  "}));
        assert_eq!(blank.label(), None);
    }

    #[test]
    fn an_edges_own_fields_round_trip() {
        let source = r#"{"nodes":[],"edges":[{"id":"e","fromNode":"a","fromSide":"right","toNode":"b","toSide":"left","label":"depends on","color":"4","toEnd":"none"}]}"#;
        let doc: CanvasDocument = serde_json::from_str(source).unwrap();
        let json = doc.to_canvas_json().unwrap();
        assert!(json.contains(r#""label":"depends on""#));
        assert!(json.contains(r#""color":"4""#));
        assert!(json.contains(r#""toEnd":"none""#));
    }

    #[test]
    fn a_group_and_a_link_round_trip_their_own_fields() {
        // `label`/`url` live in `extra`, so a document Obsidian wrote comes back
        // out byte-for-byte even though the app never writes them itself.
        let source = r#"{"nodes":[{"id":"g","type":"group","x":0,"y":0,"width":400,"height":300,"label":"Ideas"},{"id":"l","type":"link","x":0,"y":0,"width":400,"height":300,"url":"https://example.com"}],"edges":[]}"#;
        let doc: CanvasDocument = serde_json::from_str(source).unwrap();
        let json = doc.to_canvas_json().unwrap();
        assert!(json.contains(r#""label":"Ideas""#));
        assert!(json.contains(r#""url":"https://example.com""#));
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

    #[test]
    fn a_save_replaces_the_file_whole_and_leaves_nothing_beside_it() {
        let dir = std::env::temp_dir().join(format!("tb-save-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("board.canvas");
        let document = |text: &str| CanvasDocument {
            nodes: vec![CanvasNode { text: Some(text.to_owned()), ..crate::fixtures::node("a") }],
            edges: Vec::new(),
            extra: Map::new(),
        };
        document("first").save(&path).unwrap();
        document("second").save(&path).unwrap();
        assert_eq!(CanvasDocument::load(&path).unwrap().nodes[0].text.as_deref(), Some("second"));
        let names: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(names, ["board.canvas"], "the staging file is renamed into place, not left");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // Its permissions are the file's, not a fresh file's.
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            document("third").save(&path).unwrap();
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);

            // Through a link, the file it names is what's written; the link stays one.
            let link = dir.join("linked.canvas");
            std::os::unix::fs::symlink(&path, &link).unwrap();
            document("fourth").save(&link).unwrap();
            assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
            assert_eq!(CanvasDocument::load(&path).unwrap().nodes[0].text.as_deref(), Some("fourth"));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

