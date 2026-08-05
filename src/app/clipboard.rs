//! System clipboard reads for paste (text and images).
//!
//! iced's own clipboard only handles text, so images use `arboard`, which
//! wraps the platform clipboard (NSPasteboard on macOS). Reads are one-shot:
//! a fresh `Clipboard` is opened per paste rather than held, since the app
//! only touches it on an explicit paste.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use iced::widget::image;
use iced::{Point, Task};
use serde::{Deserialize, Serialize};

use super::{App, Message, arrange};
use crate::model::{self, CanvasEdge, CanvasNode};

/// Private marker key that wraps copied canvas nodes on the clipboard, so a
/// paste can tell "these are ThinkBoard nodes" from arbitrary copied text.
const CANVAS_CLIP_KEY: &str = "thinkboard/canvas";

/// Copied nodes plus the edges among them, round-tripped through the system
/// clipboard as JSON (so a copy survives a paste elsewhere, or a restart).
#[derive(Serialize, Deserialize)]
pub struct NodeClip {
    pub nodes: Vec<CanvasNode>,
    pub edges: Vec<CanvasEdge>,
}

/// Writes copied nodes to the clipboard, wrapped under [`CANVAS_CLIP_KEY`].
pub fn write_nodes(clip: &NodeClip) {
    if let Ok(mut clipboard) = arboard::Clipboard::new() {
        let _ = clipboard.set_text(encode_nodes(clip));
    }
}

/// Reads copied canvas nodes, or `None` if the clipboard doesn't hold our
/// marker (so a normal text/image paste can take over).
pub fn read_nodes() -> Option<NodeClip> {
    decode_nodes(&read_text()?)
}

/// Serializes a [`NodeClip`] to its clipboard JSON string.
fn encode_nodes(clip: &NodeClip) -> String {
    let inner = serde_json::to_value(clip).unwrap_or(serde_json::Value::Null);
    let mut wrapper = serde_json::Map::new();
    wrapper.insert(CANVAS_CLIP_KEY.to_owned(), inner);
    serde_json::Value::Object(wrapper).to_string()
}

/// Parses a [`NodeClip`] from clipboard text, or `None` if it isn't our format.
fn decode_nodes(text: &str) -> Option<NodeClip> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    serde_json::from_value(value.get(CANVAS_CLIP_KEY)?.clone()).ok()
}

impl App {
    /// Copies the selected nodes and the edges among them to the clipboard.
    pub(super) fn copy_selected_nodes(&self) {
        let selected: Vec<&CanvasNode> =
            self.selected.iter().filter_map(|&i| self.nodes.get(i)).collect();
        if selected.is_empty() {
            return;
        }
        let ids: HashSet<&str> = selected.iter().map(|n| n.id.as_str()).collect();
        let nodes = selected.into_iter().cloned().collect();
        // Only the edges wholly within the copied set carry over.
        let edges = self
            .edges
            .iter()
            .filter(|e| ids.contains(e.from_node.as_str()) && ids.contains(e.to_node.as_str()))
            .cloned()
            .collect();
        write_nodes(&NodeClip { nodes, edges });
    }

    /// Pastes the clipboard at `world`: previously copied nodes if it holds
    /// them, else its text or image as a single new node. Undoable; a no-op
    /// when the clipboard holds nothing pasteable.
    pub(super) fn paste_at(&mut self, world: Point) -> Task<Message> {
        // Copied nodes take priority over their JSON's textual form.
        if let Some(clip) = read_nodes() {
            return self.paste_nodes(clip, world);
        }
        let node = match read() {
            Some(Clip::Image { width, height, rgba }) => {
                match self.write_pasted_image(width, height, &rgba) {
                    Ok((file, path, id)) => {
                        // Fit the image to a comfortable size, keeping its aspect.
                        let (w, h) =
                            arrange::fit_image_size(width, height, self.config.max_node_width);
                        let center = self.find_free_placement(world, w, h);
                        self.images.insert(id.clone(), image::Handle::from_path(path));
                        CanvasNode {
                            id,
                            kind: "file".to_owned(),
                            text: None,
                            file: Some(file),
                            x: center.x - w / 2.0,
                            y: center.y - h / 2.0,
                            width: w,
                            height: h,
                            color: None,
                            extra: Default::default(),
                        }
                    }
                    Err(error) => {
                        eprintln!("paste: could not save image ({error})");
                        return Task::none();
                    }
                }
            }
            Some(Clip::Text(text)) => {
                let width = self.config.new_node_min_width;
                let lines = text.lines().count().max(1) as f32;
                let height = (lines * 22.0 + 24.0).clamp(self.config.new_node_min_height, 400.0);
                let center = self.find_free_placement(world, width, height);
                CanvasNode {
                    id: model::random_id(),
                    kind: "text".to_owned(),
                    text: Some(text),
                    file: None,
                    x: center.x - width / 2.0,
                    y: center.y - height / 2.0,
                    width,
                    height,
                    color: None,
                    extra: Default::default(),
                }
            }
            None => return Task::none(),
        };

        self.push_undo();
        let mut node = node;
        arrange::snap_node_geometry(&mut node, self.snap_grid());
        self.nodes.push(node);
        let index = self.nodes.len() - 1;
        self.spawning.insert(index, Instant::now());
        self.mark_dirty();
        self.cache.clear();
        self.focus_on_node(index);
        Task::batch([self.refresh_snapshots(), self.schedule_recluster()])
    }

    /// Pastes previously copied nodes: fresh ids, the whole group dropped near
    /// `world` (avoiding overlap), internal edges re-linked, and the new nodes
    /// left selected. Undoable.
    fn paste_nodes(&mut self, clip: NodeClip, world: Point) -> Task<Message> {
        if clip.nodes.is_empty() {
            return Task::none();
        }
        self.push_undo();

        // Shift the whole group so its center lands at the drop point.
        let (mut min_x, mut min_y, mut max_x, mut max_y) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for n in &clip.nodes {
            min_x = min_x.min(n.x);
            min_y = min_y.min(n.y);
            max_x = max_x.max(n.x + n.width);
            max_y = max_y.max(n.y + n.height);
        }
        let (w, h) = (max_x - min_x, max_y - min_y);
        let group_center = Point::new((min_x + max_x) / 2.0, (min_y + max_y) / 2.0);
        let free = self.find_free_placement(world, w.max(1.0), h.max(1.0));
        let (dx, dy) = (free.x - group_center.x, free.y - group_center.y);

        let mut id_map: HashMap<String, String> = HashMap::new();
        let mut selection = Vec::new();
        for mut node in clip.nodes {
            let new_id = model::random_id();
            id_map.insert(node.id.clone(), new_id.clone());
            node.id = new_id.clone();
            node.x += dx;
            node.y += dy;
            arrange::snap_node_geometry(&mut node, self.snap_grid());
            // File nodes reference the same image on disk; load its handle.
            if let Some(file) = node.file.as_deref() {
                self.images.insert(
                    new_id,
                    image::Handle::from_path(self.base_path.as_path().join(file)),
                );
            }
            self.nodes.push(node);
            let index = self.nodes.len() - 1;
            self.spawning.insert(index, Instant::now());
            selection.push(index);
        }
        for mut edge in clip.edges {
            let (Some(from), Some(to)) = (id_map.get(&edge.from_node), id_map.get(&edge.to_node))
            else {
                continue;
            };
            edge.id = model::random_id();
            edge.from_node = from.clone();
            edge.to_node = to.clone();
            self.edges.push(edge);
        }

        self.selected = selection;
        self.selected_edge = None;
        self.mark_dirty();
        self.cache.clear();
        Task::batch([self.refresh_snapshots(), self.schedule_recluster()])
    }

    /// Writes a pasted image as a PNG in the canvas file's directory. Returns
    /// the node's `file` value (relative to the vault base, as Obsidian
    /// stores it), the absolute path (for the in-memory image handle), and a
    /// fresh node id reused as the file stem.
    fn write_pasted_image(
        &self,
        width: usize,
        height: usize,
        rgba: &[u8],
    ) -> Result<(String, PathBuf, String), String> {
        let id = model::random_id();
        let dir = self
            .canvas_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let name = format!("pasted-{id}.png");
        let path = dir.join(&name);
        // `::image` (the crate) — `image` alone resolves to `iced::widget::image`.
        ::image::save_buffer(
            &path,
            rgba,
            width as u32,
            height as u32,
            ::image::ExtendedColorType::Rgba8,
        )
        .map_err(|e| e.to_string())?;

        let file = super::canvas_file::vault_relative(&path, self.base_path.as_path());
        Ok((file, path, id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CanvasEdge, CanvasNode};

    fn node(id: &str) -> CanvasNode {
        CanvasNode {
            id: id.to_owned(),
            kind: "text".to_owned(),
            text: Some("hi".to_owned()),
            file: None,
            x: 1.0,
            y: 2.0,
            width: 250.0,
            height: 60.0,
            color: Some("3".to_owned()),
            extra: Default::default(),
        }
    }

    #[test]
    fn nodes_round_trip_through_the_clipboard_format() {
        let clip = NodeClip {
            nodes: vec![node("a"), node("b")],
            edges: vec![CanvasEdge {
                id: "e".to_owned(),
                from_node: "a".to_owned(),
                from_side: "right".to_owned(),
                to_node: "b".to_owned(),
                to_side: "left".to_owned(),
                extra: Default::default(),
            }],
        };
        let decoded = decode_nodes(&encode_nodes(&clip)).unwrap();
        assert_eq!(decoded.nodes.len(), 2);
        assert_eq!(decoded.nodes[0].id, "a");
        assert_eq!(decoded.nodes[0].color.as_deref(), Some("3"));
        assert_eq!(decoded.edges.len(), 1);
        assert_eq!(decoded.edges[0].from_node, "a");
    }

    #[test]
    fn plain_text_is_not_mistaken_for_copied_nodes() {
        assert!(decode_nodes("just some copied text").is_none());
        // Valid JSON but without our marker key.
        assert!(decode_nodes(r#"{"nodes":[]}"#).is_none());
    }
}

/// What the clipboard currently holds, as far as we can paste it. Images take
/// priority over text: a screenshot on the clipboard is offered as an image
/// even though some sources also expose a textual fallback.
pub enum Clip {
    Text(String),
    /// Raw RGBA8 pixels, row-major, `width * height * 4` bytes.
    Image {
        width: usize,
        height: usize,
        rgba: Vec<u8>,
    },
}

/// The clipboard's text, if any. Cheap (unlike an image read), so it's fine
/// to call on menu open to look for a pasteable color value.
pub fn read_text() -> Option<String> {
    arboard::Clipboard::new().ok()?.get_text().ok()
}

/// Reads the clipboard, or `None` if it's empty, unreadable, or holds nothing
/// we can turn into a node.
pub fn read() -> Option<Clip> {
    let mut clipboard = arboard::Clipboard::new().ok()?;

    if let Ok(image) = clipboard.get_image() {
        if image.width > 0 && image.height > 0 {
            return Some(Clip::Image {
                width: image.width,
                height: image.height,
                rgba: image.bytes.into_owned(),
            });
        }
    }

    match clipboard.get_text() {
        Ok(text) if !text.trim().is_empty() => Some(Clip::Text(text)),
        _ => None,
    }
}
