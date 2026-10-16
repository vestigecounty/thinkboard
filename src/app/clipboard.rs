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
use iced::{Point, Rectangle, Task};
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

/// An image pasted onto a board with no folder to put it in — the welcome
/// board, which is never saved — held in memory for as long as the board is.
/// It used to be written into the app's working directory: the source folder
/// when run from a checkout, `/` in an installed app, where it failed.
pub(super) struct HeldImage {
    width: usize,
    height: usize,
    rgba: Vec<u8>,
    pub(super) handle: image::Handle,
}

/// Writes copied nodes to the clipboard, wrapped under [`CANVAS_CLIP_KEY`].
pub fn write_nodes(clip: &NodeClip) {
    system::set_text(encode_nodes(clip));
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
                match self.keep_pasted_image(width, height, rgba) {
                    Ok((file, handle, id)) => {
                        // Fit the image to a comfortable size, keeping its aspect.
                        let (w, h) =
                            arrange::fit_image_size(width, height, self.config.max_node_width);
                        let center = self.find_free_placement(world, w, h);
                        self.images.insert(id.clone(), handle);
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
                // Sized to the text, as one typed would be — guessed from its
                // line count, a pasted paragraph is one line, and clipped to
                // the three that fit.
                let (width, height) = self.text_card_size(&text);
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
        let rect = arrange::node_world_rect(&node);
        self.nodes.push(node);
        let index = self.nodes.len() - 1;
        self.spawning.insert(index, Instant::now());
        self.mark_dirty();
        self.cache.clear();
        if !self.focus_on_node(index) {
            self.reveal(rect);
        }
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
        let mut bounds: Option<Rectangle> = None;
        for mut node in clip.nodes {
            let new_id = model::random_id();
            id_map.insert(node.id.clone(), new_id.clone());
            node.id = new_id.clone();
            node.x += dx;
            node.y += dy;
            arrange::snap_node_geometry(&mut node, self.snap_grid());
            bounds = Some(match bounds {
                Some(bounds) => arrange::node_world_rect(&node).union(&bounds),
                None => arrange::node_world_rect(&node),
            });
            // File nodes reference the same image on disk; load its handle.
            if let Some(file) = node.file.clone() {
                // Unless it was only ever in memory, copied off the welcome
                // board: a board with a folder writes it there, as a paste
                // onto it would have.
                let handle = match self.held_images.get(&file) {
                    // Copied within the welcome board: the same picture, still held.
                    Some(held) if self.scratch => held.handle.clone(),
                    Some(held) => {
                        let (width, height, rgba) = (held.width, held.height, held.rgba.clone());
                        match self.keep_pasted_image(width, height, rgba) {
                            Ok((file, handle, _)) => {
                                node.file = Some(file);
                                handle
                            }
                            Err(error) => {
                                eprintln!("paste: could not save image ({error})");
                                image::Handle::from_path(self.base_path.as_path().join(&file))
                            }
                        }
                    }
                    None => image::Handle::from_path(self.base_path.as_path().join(&file)),
                };
                self.images.insert(new_id, handle);
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
        if let Some(bounds) = bounds {
            self.reveal(bounds);
        }
        Task::batch([self.refresh_snapshots(), self.schedule_recluster()])
    }

    /// Keeps a pasted image: as a PNG in the canvas file's directory, or — on
    /// the welcome board, which has none — in memory (see [`HeldImage`]).
    /// Returns the node's `file` value (relative to the vault base, as
    /// Obsidian stores it), the handle to draw it with, and a fresh node id
    /// reused as the file stem.
    fn keep_pasted_image(
        &mut self,
        width: usize,
        height: usize,
        rgba: Vec<u8>,
    ) -> Result<(String, image::Handle, String), String> {
        let id = model::random_id();
        let name = format!("pasted-{id}.png");
        if self.scratch {
            let handle = image::Handle::from_rgba(width as u32, height as u32, rgba.clone());
            self.held_images.insert(name.clone(), HeldImage { width, height, rgba, handle: handle.clone() });
            return Ok((name, handle, id));
        }
        let dir = self
            .canvas_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let path = dir.join(&name);
        // `::image` (the crate) — `image` alone resolves to `iced::widget::image`.
        ::image::save_buffer(
            &path,
            &rgba,
            width as u32,
            height as u32,
            ::image::ExtendedColorType::Rgba8,
        )
        .map_err(|e| e.to_string())?;

        let file = super::canvas_file::vault_relative(&path, self.base_path.as_path());
        Ok((file, image::Handle::from_path(path), id))
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

/// Puts `text` on the clipboard.
pub fn write_text(text: String) {
    system::set_text(text);
}

/// The clipboard's text, if any. Cheap (unlike an image read), so it's fine
/// to call on menu open to look for a pasteable color value.
pub fn read_text() -> Option<String> {
    system::text()
}

/// Reads the clipboard, or `None` if it's empty, unreadable, or holds nothing
/// we can turn into a node.
pub fn read() -> Option<Clip> {
    if let Some(image) = system::image()
        && let Clip::Image { width, height, .. } = &image
        && *width > 0
        && *height > 0
    {
        return Some(image);
    }
    system::text().filter(|text| !text.trim().is_empty()).map(Clip::Text)
}

/// The platform's clipboard.
#[cfg(not(test))]
mod system {
    use super::Clip;

    pub fn set_text(text: String) {
        if let Ok(mut clipboard) = arboard::Clipboard::new() {
            let _ = clipboard.set_text(text);
        }
    }

    pub fn text() -> Option<String> {
        arboard::Clipboard::new().ok()?.get_text().ok()
    }

    pub fn image() -> Option<Clip> {
        let image = arboard::Clipboard::new().ok()?.get_image().ok()?;
        Some(Clip::Image { width: image.width, height: image.height, rgba: image.bytes.into_owned() })
    }
}

/// Under test, a clipboard this process holds instead of the platform's: a
/// test that copied would overwrite whatever the developer last copied, and
/// one that pasted would paste it. The guard the config has against writing
/// the real one (`config::writable`), for the other thing a test could reach
/// out and change. The tour fills it to stand in for another app.
#[cfg(test)]
pub(super) mod system {
    use std::sync::{Mutex, MutexGuard, PoisonError};

    use super::Clip;

    static HELD: Mutex<Option<Clip>> = Mutex::new(None);

    /// What the stand-in holds. Shared by the whole test binary, so a test
    /// that relies on its contents sets them first.
    pub fn held() -> MutexGuard<'static, Option<Clip>> {
        HELD.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn set_text(text: String) {
        *held() = Some(Clip::Text(text));
    }

    pub fn text() -> Option<String> {
        match &*held() {
            Some(Clip::Text(text)) => Some(text.clone()),
            _ => None,
        }
    }

    pub fn image() -> Option<Clip> {
        match &*held() {
            Some(Clip::Image { width, height, rgba }) => {
                Some(Clip::Image { width: *width, height: *height, rgba: rgba.clone() })
            }
            _ => None,
        }
    }
}
