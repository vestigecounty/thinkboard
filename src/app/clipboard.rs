//! System clipboard reads for paste (text, images and files).
//!
//! iced's own clipboard only handles text, so images use `arboard`, which
//! wraps the platform clipboard (NSPasteboard on macOS). Reads are one-shot:
//! a fresh `Clipboard` is opened per paste rather than held, since the app
//! only touches it on an explicit paste.
//!
//! **Copied cards are two things at once.** Another app gets them as text it
//! can read: the chain format (`thinkboard_core::chain`) as plain text, for a
//! terminal or an agent, and rendered as HTML, for an app that pastes HTML —
//! Obsidian turns it back into Markdown. ThinkBoard gets the cards themselves,
//! hidden in that HTML as a comment. They used to be the plain text, so what
//! another app pasted was JSON with positions in it.

use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::Instant;

use iced::widget::image;
use iced::{Point, Rectangle, Task};
use serde::{Deserialize, Serialize};

use thinkboard_core::{chain, mermaid};

use super::{App, Message, arrange};
use crate::canvas::board;
use crate::model::{self, CanvasEdge, CanvasNode, NodeKind};

/// Private marker key that wraps copied canvas nodes on the clipboard, so a
/// paste can tell "these are ThinkBoard nodes" from arbitrary copied text.
const CANVAS_CLIP_KEY: &str = "thinkboard/canvas";

/// The size of a card for a note pasted from a file manager: Obsidian's own
/// for a file card. A note is read like a document, from its top, so it isn't
/// sized to its text as a text card is.
const NOTE_SIZE: (f32, f32) = (400.0, 400.0);

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

/// Writes copied cards: `text` for other apps, as plain text and as HTML,
/// and the cards themselves for ThinkBoard, in the HTML ([`hide_in_html`]).
pub fn write_cards(clip: &NodeClip, text: &str) {
    let html = format!("{}{}", chain::html(text), hide_in_html(&encode_nodes(clip)));
    system::set_html(html, text.to_owned());
}

/// Reads copied cards, or `None` if the clipboard doesn't hold ours (so a
/// normal text/image paste can take over). From the HTML, or from the plain
/// text where a build before the HTML put them.
pub fn read_nodes() -> Option<NodeClip> {
    system::html()
        .and_then(|html| decode_nodes(found_in_html(&html)?))
        .or_else(|| decode_nodes(&read_text()?))
}

/// `json` as an HTML comment, which every app that pastes HTML leaves out.
/// A comment ends at the first `--`, which JSON can only hold inside a string,
/// where `\u002d` reads back as the same hyphen.
fn hide_in_html(json: &str) -> String {
    format!("<!--{}-->", json.replace("--", "-\\u002d"))
}

/// The cards [`hide_in_html`] put in `html`, as JSON. Searched for rather than
/// expected at the end: each platform wraps HTML on the clipboard its own way.
fn found_in_html(html: &str) -> Option<&str> {
    let start = html.find(&format!("<!--{{\"{CANVAS_CLIP_KEY}\""))? + "<!--".len();
    let len = html[start..].find("-->")?;
    Some(&html[start..start + len])
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

/// `name` in `dir`, or, where that's taken, numbered as Obsidian numbers a
/// second copy of an attachment: `photo 1.jpg`, then `photo 2.jpg`.
fn untaken(dir: &Path, name: &OsStr) -> PathBuf {
    let path = dir.join(name);
    if !path.exists() {
        return path;
    }
    let name = Path::new(name);
    let stem = name.file_stem().unwrap_or(name.as_os_str()).to_string_lossy();
    let extension = name.extension().map(|ext| format!(".{}", ext.to_string_lossy())).unwrap_or_default();
    let mut n = 1;
    loop {
        let path = dir.join(format!("{stem} {n}{extension}"));
        if !path.exists() {
            return path;
        }
        n += 1;
    }
}

impl App {
    /// Copies the selected cards — or, with `chain`, the chain leading into
    /// each — with what a group among them encloses: as text other apps read
    /// and cards this one pastes, or as a Mermaid flowchart alone, which an
    /// app that pastes HTML would otherwise take the HTML over.
    ///
    /// A chain copied isn't on screen the way a selection is, so the shimmer
    /// sweeps it once to show what went.
    pub(super) fn copy(&mut self, chain: bool, mermaid: bool) {
        let selected: Vec<usize> = self.selected.iter().copied().filter(|&i| i < self.nodes.len()).collect();
        if selected.is_empty() {
            return;
        }
        let set = if chain { chain::upstream(&self.nodes, &self.edges, &selected) } else { selected };
        let set = chain::with_contents(&self.nodes, &set);
        let ids: HashSet<&str> = set.iter().map(|&i| self.nodes[i].id.as_str()).collect();
        // Only the edges wholly within the copied set carry over.
        let edges: Vec<&CanvasEdge> = self
            .edges
            .iter()
            .filter(|e| ids.contains(e.from_node.as_str()) && ids.contains(e.to_node.as_str()))
            .collect();
        if mermaid {
            write_text(chain::mermaid(&self.nodes, &self.edges, &set, self.config_layout_dir()));
        } else {
            let text = chain::text(&self.nodes, &self.edges, &chain::order(&self.nodes, &self.edges, &set));
            let nodes = set.iter().map(|&i| self.nodes[i].clone()).collect();
            write_cards(&NodeClip { nodes, edges: edges.iter().map(|&e| e.clone()).collect() }, &text);
        }
        if chain {
            self.ai.flows.push(board::AiFlow {
                nodes: set.iter().map(|&i| self.nodes[i].id.clone()).collect(),
                edges: edges.iter().map(|e| e.id.clone()).collect(),
                target: None,
                since: Instant::now(),
            });
        }
    }

    /// Selects the chain of each selected card, or of `of`: it and every card
    /// leading into it.
    pub(super) fn select_chain(&mut self, of: Option<usize>) {
        let targets = match of {
            Some(index) if index < self.nodes.len() => vec![index],
            Some(_) => return,
            None => self.selected.iter().copied().filter(|&i| i < self.nodes.len()).collect(),
        };
        if targets.is_empty() {
            return;
        }
        self.selected = chain::upstream(&self.nodes, &self.edges, &targets);
        self.selected_edge = None;
        self.cache.clear();
    }

    /// Pastes the clipboard at `world`: previously copied nodes if it holds
    /// them, a Mermaid flowchart as the cards it draws, files copied in a file
    /// manager as a card each, else its text or image as a single new node —
    /// or, `as_text`, its text as a card whatever else it is. Undoable; a
    /// no-op when the clipboard holds nothing pasteable.
    pub(super) fn paste_at(&mut self, world: Point, as_text: bool) -> Task<Message> {
        // Copied nodes take priority over their JSON's textual form.
        if !as_text && let Some(clip) = read_nodes() {
            return self.paste_nodes(clip, world);
        }
        let clip = if as_text { read_text().filter(|text| !text.trim().is_empty()).map(Clip::Text) } else { read() };
        if !as_text
            && let Some(Clip::Text(text)) = &clip
            && let Some(mut chart) = mermaid::parse(text)
        {
            self.lay_out_flowchart(&mut chart);
            return self.paste_nodes(NodeClip { nodes: chart.nodes, edges: chart.edges }, world);
        }
        let node = match clip {
            Some(Clip::Files(paths)) => return self.paste_files(&paths, world),
            Some(Clip::Image { width, height, rgba }) => {
                match self.keep_pasted_image(width, height, rgba) {
                    Ok((file, handle, id)) => {
                        // Fit the image to a comfortable size, keeping its aspect.
                        let (w, h) =
                            arrange::fit_image_size(width, height, self.config.max_card_width);
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
        let first = self.nodes.len();
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
            // A file named by its full path was pasted onto the welcome board,
            // which has no folder: a board with one brings it in, as a paste
            // onto it would have.
            if !self.scratch
                && let Some(file) = node.file.as_deref().map(PathBuf::from)
                && file.is_absolute()
                && let Some(brought) = self.bring_file(&file)
            {
                node.file = Some(brought);
            }
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
        // And a note's text, which the board draws from here: a note card
        // copied and pasted said "(note not found)" until the board reloaded.
        let notes = self.load_notes(&self.nodes[first..]);
        self.notes.extend(notes);

        self.selected = selection;
        self.selected_edge = None;
        self.mark_dirty();
        self.cache.clear();
        if let Some(bounds) = bounds {
            self.reveal(bounds);
        }
        Task::batch([self.refresh_snapshots(), self.schedule_recluster()])
    }

    /// Pastes files copied in a file manager, a card each, in a grid near
    /// `world`, left selected: a picture as itself, a note as its text,
    /// anything else as its path. Finder puts each file's icon on the
    /// clipboard beside it, and that icon was what pasted, whatever the file
    /// held.
    fn paste_files(&mut self, paths: &[PathBuf], world: Point) -> Task<Message> {
        const GAP: f32 = 20.0;
        let mut cards: Vec<CanvasNode> = paths.iter().map(|path| self.file_card(path)).collect();
        // As near square as the count allows, so a few dozen pictures don't
        // make one long strip of the board.
        let columns = (cards.len() as f32).sqrt().ceil() as usize;
        let (mut x, mut y, mut row_height) = (0.0, 0.0, 0.0_f32);
        for (i, card) in cards.iter_mut().enumerate() {
            if i > 0 && i % columns == 0 {
                (x, y, row_height) = (0.0, y + row_height + GAP, 0.0);
            }
            (card.x, card.y) = (x, y);
            x += card.width + GAP;
            row_height = row_height.max(card.height);
        }
        self.paste_nodes(NodeClip { nodes: cards, edges: Vec::new() }, world)
    }

    /// A card, at the origin, for the file at `path`: its picture or its
    /// note, where it holds one, or else its path as text.
    fn file_card(&mut self, path: &Path) -> CanvasNode {
        let mut card = CanvasNode {
            id: model::random_id(),
            kind: "file".to_owned(),
            text: None,
            file: Some(path.to_string_lossy().into_owned()),
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 0.0,
            color: None,
            extra: Default::default(),
        };
        // Which files a card draws, and as what, is the file format's to say.
        let size = match card.node_kind() {
            NodeKind::Image => ::image::image_dimensions(path).ok().map(|(width, height)| {
                arrange::fit_image_size(width as usize, height as usize, self.config.max_card_width)
            }),
            // A board's file with no extension is taken for a note, since
            // Obsidian's links leave the `.md` off; one from a file manager is
            // whatever it is — a folder, a `Makefile`.
            NodeKind::Note if path.extension().is_some() && path.is_file() => Some(NOTE_SIZE),
            _ => None,
        };
        if let Some((width, height)) = size
            && let Some(file) = self.bring_file(path)
        {
            card.file = Some(file);
            (card.width, card.height) = (width, height);
            return card;
        }
        // A picture a card can't draw from its file, a TIFF, is kept as a
        // pasted image is.
        if let Some((file, width, height)) = self.keep_decoded(path) {
            card.file = Some(file);
            (card.width, card.height) = arrange::fit_image_size(width, height, self.config.max_card_width);
            return card;
        }
        let text = path.display().to_string();
        (card.width, card.height) = self.text_card_size(&text);
        card.kind = "text".to_owned();
        card.file = None;
        card.text = Some(text);
        card
    }

    /// The `file` value for a card of the file at `path`: the file where it
    /// lies, if that's inside the vault, or else a copy beside the board under
    /// its own name — not re-encoded as a pasted image is, since a photo's
    /// JPEG came out several times its size as a PNG.
    ///
    /// The welcome board has no folder to copy into, and is never saved, so
    /// there a file is named by its full path, and brought in when its card is
    /// pasted onto a board that has one ([`App::paste_nodes`]).
    fn bring_file(&self, path: &Path) -> Option<String> {
        if self.scratch {
            return Some(path.to_string_lossy().into_owned());
        }
        let base = self.base_path.as_path();
        if path.starts_with(base) {
            return Some(super::canvas_file::vault_relative(path, base));
        }
        let copy = untaken(&self.board_folder(), path.file_name()?);
        match std::fs::copy(path, &copy) {
            Ok(_) => Some(super::canvas_file::vault_relative(&copy, base)),
            Err(error) => {
                eprintln!("paste: could not copy {} ({error})", path.display());
                None
            }
        }
    }

    /// The picture in the file at `path`, if the decoder reads one, kept as a
    /// pasted image is: the card's `file` value and the picture's size.
    fn keep_decoded(&mut self, path: &Path) -> Option<(String, usize, usize)> {
        // `::image` (the crate) — `image` alone resolves to `iced::widget::image`.
        let picture = ::image::open(path).ok()?.into_rgba8();
        let (width, height) = (picture.width() as usize, picture.height() as usize);
        match self.keep_pasted_image(width, height, picture.into_raw()) {
            Ok((file, _, _)) => Some((file, width, height)),
            Err(error) => {
                eprintln!("paste: could not save image ({error})");
                None
            }
        }
    }

    /// The folder the board's file is in, where what's pasted onto it is kept.
    fn board_folder(&self) -> PathBuf {
        self.canvas_path.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."))
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
        let path = self.board_folder().join(&name);
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

    #[test]
    fn cards_hidden_in_html_come_back_whatever_their_text_says() {
        // A card's text can hold what would end a comment.
        let mut card = node("a");
        card.text = Some("a --> b <!-- c --- d".to_owned());
        let json = encode_nodes(&NodeClip { nodes: vec![card], edges: Vec::new() });
        let hidden = hide_in_html(&json);
        assert_eq!(hidden.matches("--").count(), 2, "only the comment's own: {hidden}");
        // Wrapped as a platform wraps it, after the readable part.
        let html = format!("<html><body><p>a --&gt; b</p>{hidden}</body></html>");
        let found = decode_nodes(found_in_html(&html).expect("found")).expect("ours");
        assert_eq!(found.nodes[0].text.as_deref(), Some("a --> b <!-- c --- d"));
        assert!(found_in_html("<p>someone else's HTML</p><!-- a comment -->").is_none());
    }
}

/// What the clipboard currently holds, as far as we can paste it. Images take
/// priority over text: a screenshot on the clipboard is offered as an image
/// even though some sources also expose a textual fallback. Files take
/// priority over both: a file copied in Finder comes with its icon as an
/// image and its name as text, and neither is the file.
pub enum Clip {
    Text(String),
    /// Files copied in a file manager, by path.
    Files(Vec<PathBuf>),
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
    if let Some(paths) = system::files().filter(|paths| !paths.is_empty()) {
        return Some(Clip::Files(paths));
    }
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
    use std::path::PathBuf;

    use super::Clip;

    /// Runs `f` with the platform's clipboard: on Linux, one held for the
    /// app's life; elsewhere, one opened for the call.
    ///
    /// Under X11 — and under Wayland, which arboard reaches through XWayland —
    /// what's copied stays with the app that copied it, which hands it over
    /// when it's pasted, and arboard does that only while a `Clipboard` lives.
    /// Dropping the last one gives the data to a clipboard manager, waiting up
    /// to 100 ms for one, and with none, it's gone. Opened and dropped for
    /// each copy, a copy could paste as nothing, here or anywhere, and stalled
    /// a frame on the way. macOS and Windows keep what's copied themselves.
    fn with<T>(f: impl FnOnce(&mut arboard::Clipboard) -> Option<T>) -> Option<T> {
        #[cfg(target_os = "linux")]
        {
            use std::sync::{Mutex, PoisonError};
            static HELD: Mutex<Option<arboard::Clipboard>> = Mutex::new(None);
            let mut held = HELD.lock().unwrap_or_else(PoisonError::into_inner);
            if held.is_none() {
                *held = arboard::Clipboard::new().ok();
            }
            f(held.as_mut()?)
        }
        #[cfg(not(target_os = "linux"))]
        f(&mut arboard::Clipboard::new().ok()?)
    }

    pub fn set_text(text: String) {
        with(|clipboard| clipboard.set_text(text).ok());
    }

    /// `html`, with `text` beside it for the apps that paste plain text.
    pub fn set_html(html: String, text: String) {
        with(|clipboard| clipboard.set_html(html, Some(text)).ok());
    }

    pub fn text() -> Option<String> {
        with(|clipboard| clipboard.get_text().ok())
    }

    pub fn html() -> Option<String> {
        with(|clipboard| clipboard.get().html().ok())
    }

    pub fn image() -> Option<Clip> {
        let image = with(|clipboard| clipboard.get_image().ok())?;
        Some(Clip::Image { width: image.width, height: image.height, rgba: image.bytes.into_owned() })
    }

    /// Files copied in a file manager: `file://` URLs on macOS, a
    /// `text/uri-list` on Linux, a file drop on Windows.
    pub fn files() -> Option<Vec<PathBuf>> {
        with(|clipboard| clipboard.get().file_list().ok())
    }
}

/// Under test, a clipboard this process holds instead of the platform's: a
/// test that copied would overwrite whatever the developer last copied, and
/// one that pasted would paste it. The guard the config has against writing
/// the real one (`config::writable`), for the other thing a test could reach
/// out and change. The tour fills it to stand in for another app.
#[cfg(test)]
pub(super) mod system {
    use std::path::PathBuf;
    use std::sync::{Mutex, MutexGuard, PoisonError};

    use super::Clip;

    static HELD: Mutex<Option<Clip>> = Mutex::new(None);

    /// The HTML last put on, with the text put on beside it.
    static HTML: Mutex<Option<(String, String)>> = Mutex::new(None);

    static EXCLUSIVE: Mutex<()> = Mutex::new(());

    /// The stand-in to one test while the guard lives. Tests run side by side,
    /// and one that copied between another's copy and its paste would hand it
    /// the wrong clip.
    pub fn exclusive() -> MutexGuard<'static, ()> {
        EXCLUSIVE.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// What the stand-in holds. Shared by the whole test binary, so a test
    /// that relies on its contents sets them first.
    pub fn held() -> MutexGuard<'static, Option<Clip>> {
        HELD.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn set_text(text: String) {
        *held() = Some(Clip::Text(text));
    }

    pub fn set_html(html: String, text: String) {
        *held() = Some(Clip::Text(text.clone()));
        *HTML.lock().unwrap_or_else(PoisonError::into_inner) = Some((html, text));
    }

    /// The HTML, while the text put on with it is still what's held: whatever
    /// was put on since — by a test, or the tour playing another app — took
    /// its place, as it would on a real clipboard.
    pub fn html() -> Option<String> {
        let (html, text) = HTML.lock().unwrap_or_else(PoisonError::into_inner).clone()?;
        matches!(&*held(), Some(Clip::Text(held)) if *held == text).then_some(html)
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

    pub fn files() -> Option<Vec<PathBuf>> {
        match &*held() {
            Some(Clip::Files(paths)) => Some(paths.clone()),
            _ => None,
        }
    }
}
