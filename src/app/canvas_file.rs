//! The `.canvas` file behind the board: opening, creating, autosaving, and
//! reloading it when another process writes it — plus resolving the Obsidian
//! vault root that a canvas's image paths are relative to.

use std::collections::HashMap;
use std::path::{Path as FsPath, PathBuf};

use iced::widget::image;
use iced::{Task, window};

use super::sync::{file_mtime, should_reload};
use super::{App, Message};
use crate::model::{CanvasDocument, CanvasNode, NodeKind};

/// The embedded scratch board shown when no real canvas is configured. A
/// throwaway playground: never written to disk, reset on every launch. Its
/// button nodes (carrying a `tb_action` field) create or open a real board.
const SCRATCH_CANVAS: &str = include_str!("welcome.canvas");

/// Parses the embedded scratch board. It's compiled in and validated by a
/// unit test, so a parse failure is a build-time bug, not a runtime condition.
pub(super) fn scratch_document() -> CanvasDocument {
    serde_json::from_str(SCRATCH_CANVAS).expect("embedded scratch canvas is valid")
}

/// Whether `path` is an Obsidian canvas we can open (case-insensitive
/// extension) — used to filter drag-and-drop.
fn is_canvas_file(path: &FsPath) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("canvas"))
}

/// Maps a `.canvas` file dropped onto the window to an open request. Other
/// files are ignored (we only open canvases).
pub(super) fn drop_event(
    event: iced::Event,
    _status: iced::event::Status,
    _window: window::Id,
) -> Option<Message> {
    let iced::Event::Window(window::Event::FileDropped(path)) = event else {
        return None;
    };
    is_canvas_file(&path).then_some(Message::OpenDropped(path))
}

/// The most a note's body is read into memory. A card shows a few hundred
/// characters at most, so anything past this can't be seen — and a stray
/// multi-megabyte file shouldn't be pulled in just to render one card.
const MAX_NOTE_BYTES: usize = 64 * 1024;

/// Reads a Markdown note, truncated to [`MAX_NOTE_BYTES`] on a char boundary.
/// `None` if it isn't a readable file (missing, a directory, not UTF-8).
fn read_note(path: &FsPath) -> Option<String> {
    let mut text = std::fs::read_to_string(path).ok()?;
    if text.len() > MAX_NOTE_BYTES {
        let end = (0..=MAX_NOTE_BYTES).rev().find(|&i| text.is_char_boundary(i))?;
        text.truncate(end);
    }
    // Obsidian notes often open with a YAML front-matter block, which is
    // metadata rather than content — drop it so the card shows the note.
    Some(strip_front_matter(&text).to_owned())
}

/// The note body with a leading `---`-delimited YAML front-matter block
/// removed. Text that doesn't open with one is returned unchanged.
fn strip_front_matter(text: &str) -> &str {
    let Some(rest) = text.strip_prefix("---\n").or_else(|| text.strip_prefix("---\r\n")) else {
        return text;
    };
    let Some(end) = rest.find("\n---") else {
        return text;
    };
    let after = &rest[end + "\n---".len()..];
    after.trim_start_matches(['\r', '\n'])
}

/// The vault root that a canvas's image `file` paths resolve against. Obsidian
/// records those paths relative to the vault root (which may be several levels
/// above the canvas), so:
///   1. walk up from the canvas to a `.obsidian` marker (authoritative);
///   2. else infer it — walk up to the ancestor where an image `file` actually
///      resolves (handles a vault copied without its `.obsidian` folder);
///   3. else fall back to the canvas's own directory.
pub(super) fn vault_root(canvas: &FsPath, document: &CanvasDocument) -> PathBuf {
    let canvas_dir = canvas.parent().unwrap_or_else(|| FsPath::new("."));

    let mut dir = Some(canvas_dir);
    while let Some(current) = dir {
        if current.join(".obsidian").is_dir() {
            return current.to_path_buf();
        }
        dir = current.parent();
    }

    if let Some(file) = document.nodes.iter().find_map(|node| node.file.as_deref()) {
        let mut dir = Some(canvas_dir);
        while let Some(current) = dir {
            if current.join(file).exists() {
                return current.to_path_buf();
            }
            dir = current.parent();
        }
    }

    canvas_dir.to_path_buf()
}

/// Writes `document` to `path` only when there are unsaved changes — the app
/// calls this to flush a pending autosave before quitting, so edits made
/// inside the autosave debounce window aren't dropped on exit. Returns whether
/// it wrote.
fn save_if_dirty(dirty: bool, document: &CanvasDocument, path: &FsPath) -> bool {
    if !dirty {
        return false;
    }
    if let Err(error) = document.save(path) {
        eprintln!("save on quit failed: {error}");
    }
    true
}

/// The `file` value stored for an image written beside the canvas: its path
/// relative to `base` (the vault root, how Obsidian records file nodes),
/// falling back to the bare file name when the image lies outside the base.
pub(super) fn vault_relative(path: &FsPath, base: &FsPath) -> String {
    path.strip_prefix(base)
        .map(|rel| rel.to_string_lossy().into_owned())
        .unwrap_or_else(|_| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default()
        })
}

impl App {
    /// The in-memory document as it would be written to disk.
    pub(super) fn current_document(&self) -> CanvasDocument {
        CanvasDocument {
            nodes: self.nodes.clone(),
            edges: self.edges.clone(),
            extra: self.doc_extra.clone(),
        }
    }

    /// Writes the current document to disk, preserving unknown fields, and
    /// adopts the resulting mtime so the reload poll ignores our own write.
    pub(super) fn save_document(&mut self) {
        if self.scratch {
            return; // the scratch board is never written to disk
        }
        match self.current_document().save(&self.canvas_path) {
            Ok(()) => {
                self.doc_dirty = false;
                self.file_mtime = file_mtime(&self.canvas_path);
            }
            Err(error) => eprintln!("autosave failed: {error}"),
        }
    }

    /// Flushes a pending autosave (if any) and quits. Used on every exit path
    /// so edits made within the autosave debounce window are never lost.
    pub(super) fn save_and_exit(&mut self) -> Task<Message> {
        if !self.scratch
            && save_if_dirty(self.doc_dirty, &self.current_document(), &self.canvas_path)
        {
            self.doc_dirty = false;
            self.file_mtime = file_mtime(&self.canvas_path);
        }
        // macOS: our NSPanel class-morph leaves winit's KVO teardown
        // inconsistent, which throws NSRangeException on a clean exit. The save
        // is already flushed, so exit the process directly and skip the
        // crashing teardown. Other platforms tear down cleanly via iced.
        #[cfg(target_os = "macos")]
        {
            std::process::exit(0)
        }
        #[cfg(not(target_os = "macos"))]
        {
            iced::exit()
        }
    }

    /// Image handles for the file nodes that embed an *image*, resolved against
    /// the vault root. Rebuilt on every document swap, since the handles are
    /// keyed by node id.
    pub(super) fn load_images(&self, nodes: &[CanvasNode]) -> HashMap<String, image::Handle> {
        nodes
            .iter()
            .filter(|node| node.node_kind() == NodeKind::Image)
            .filter_map(|node| {
                let file = node.file.as_ref()?;
                Some((
                    node.id.clone(),
                    image::Handle::from_path(self.base_path.as_path().join(file)),
                ))
            })
            .collect()
    }

    /// The body text of every file node that embeds a Markdown note, read from
    /// the vault and keyed by node id. A note that can't be read is absent, and
    /// the board falls back to drawing just its title.
    pub(super) fn load_notes(&self, nodes: &[CanvasNode]) -> HashMap<String, String> {
        nodes
            .iter()
            .filter(|node| node.node_kind() == NodeKind::Note)
            .filter_map(|node| {
                let file = node.file.as_ref()?;
                let path = self.base_path.as_path().join(file);
                // Obsidian's links often omit the `.md`; try it before giving up.
                let text = read_note(&path)
                    .or_else(|| read_note(&path.with_extension("md")))?;
                Some((node.id.clone(), text))
            })
            .collect()
    }

    /// Opens a native dialog to create a new board (a Save dialog, `create`) or
    /// open an existing one, then resolves to `Message::BoardChosen`. Mirrors
    /// `browse_for`'s per-OS handling (blocking on mac/win, async portal on
    /// Linux, whose zbus backend needs iced's tokio runtime).
    pub(super) fn choose_board(&mut self, create: bool) -> Task<Message> {
        #[cfg(not(target_os = "linux"))]
        {
            let dialog = rfd::FileDialog::new().add_filter("Obsidian canvas", &["canvas"]);
            let path = if create {
                dialog.set_file_name("board.canvas").save_file()
            } else {
                dialog.pick_file()
            };
            Task::done(Message::BoardChosen { path, create })
        }
        #[cfg(target_os = "linux")]
        {
            Task::perform(
                async move {
                    let dialog =
                        rfd::AsyncFileDialog::new().add_filter("Obsidian canvas", &["canvas"]);
                    let handle = if create {
                        dialog.set_file_name("board.canvas").save_file().await
                    } else {
                        dialog.pick_file().await
                    };
                    handle.map(|handle| handle.path().to_path_buf())
                },
                move |path| Message::BoardChosen { path, create },
            )
        }
    }

    /// Creates an empty canvas at `path` (when `create`), then opens it as the
    /// live board. A failed create is reported and aborts the open.
    pub(super) fn open_board(&mut self, path: PathBuf, create: bool) -> Task<Message> {
        if create {
            let empty = CanvasDocument {
                nodes: Vec::new(),
                edges: Vec::new(),
                extra: serde_json::Map::new(),
            };
            if let Err(error) = empty.save(&path) {
                eprintln!("could not create {}: {error}", path.display());
                return Task::none();
            }
        }
        self.open_canvas(path)
    }

    /// Switches the live document to the canvas at `path`: leaves the scratch
    /// board (or a previous file) behind, reframes the camera, and remembers
    /// the choice so the next launch opens it directly. Generalizes
    /// `reload_document` (which reloads the *same* path) to a different file.
    pub(super) fn open_canvas(&mut self, path: PathBuf) -> Task<Message> {
        // Flush pending edits to the board we're leaving before switching, so a
        // drag-drop / open while mid-edit doesn't drop them. No-op for the
        // scratch board (which is never saved) and when there's nothing dirty.
        if self.doc_dirty {
            self.save_document();
        }

        let document = match CanvasDocument::load(&path) {
            Ok(document) => document,
            Err(error) => {
                eprintln!("could not open {}: {error}", path.display());
                return Task::none();
            }
        };

        self.canvas_path = path;
        self.base_path = vault_root(&self.canvas_path, &document);
        self.file_mtime = file_mtime(&self.canvas_path);
        self.scratch = false;

        self.adopt_document(document);
        // A different board entirely: also drop the editor and the history,
        // and reframe the camera on the new content.
        self.editing = None;
        self.history.clear();
        self.edit_undo = None;
        self.doc_dirty = false;
        self.interacted = false;
        self.camera.fit(self.content_bounds());

        // Remember the working board so the next launch skips the scratch board.
        self.config.canvas = self.canvas_path.to_string_lossy().into_owned();
        self.persist_config();

        Task::batch([self.refresh_snapshots(), self.schedule_recluster()])
    }

    /// Reloads the file if another process changed it — unless we have
    /// unsaved edits or an open editor, in which case our own state wins
    /// (last-write-wins; our pending save will overwrite). Only touches the
    /// disk (one stat) when we might actually reload.
    pub(super) fn check_reload(&mut self) -> Task<Message> {
        if self.scratch || self.doc_dirty || self.editing.is_some() {
            return Task::none();
        }
        let current = file_mtime(&self.canvas_path);
        if should_reload(false, false, current, self.file_mtime) {
            return self.reload_document();
        }
        Task::none()
    }

    pub(super) fn reload_document(&mut self) -> Task<Message> {
        let Ok(document) = CanvasDocument::load(&self.canvas_path) else {
            return Task::none();
        };
        self.file_mtime = file_mtime(&self.canvas_path);

        self.adopt_document(document);
        // The document changed under us; the old history no longer applies.
        self.history.clear();
        self.edit_undo = None;

        Task::batch([self.refresh_snapshots(), self.schedule_recluster()])
    }

    /// Installs a freshly loaded document, dropping the per-node caches and
    /// transient state that the old node indices keyed. Shared by opening a
    /// different board and reloading the same one after an external write.
    fn adopt_document(&mut self, document: CanvasDocument) {
        self.images = self.load_images(&document.nodes);
        self.notes = self.load_notes(&document.nodes);
        self.nodes = document.nodes;
        self.edges = document.edges;
        self.doc_extra = document.extra;

        self.rasters.clear();
        self.spawning.clear();
        self.pending_edit = None;
        self.fresh_node = None;
        self.selected.clear();
        self.selected_edge = None;
        self.hovered_edge = None;
        self.cache.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::CanvasNode;

    #[test]
    fn the_embedded_scratch_board_parses() {
        // A parse failure here means the checked-in welcome.canvas is broken —
        // caught at test time rather than panicking on launch.
        let document = scratch_document();
        assert!(!document.nodes.is_empty());
        assert!(!document.edges.is_empty());
    }

    #[test]
    fn the_scratch_board_has_create_and_open_buttons() {
        let document = scratch_document();
        let actions: Vec<&str> = document
            .nodes
            .iter()
            .filter_map(|node| node.extra.get("tb_action").and_then(|v| v.as_str()))
            .collect();
        // Exactly the two onboarding buttons, and every button is a real node
        // the click handler can find by its `tb_action`.
        assert_eq!(actions, ["new_board", "open_board"]);
        for action in ["new_board", "open_board"] {
            let button = document
                .nodes
                .iter()
                .find(|n| n.extra.get("tb_action").and_then(|v| v.as_str()) == Some(action))
                .unwrap_or_else(|| panic!("missing {action} button"));
            assert_eq!(button.kind, "text");
            assert!(button.text.as_deref().is_some_and(|t| !t.is_empty()));
        }
    }

    #[test]
    fn only_canvas_files_are_droppable() {
        assert!(is_canvas_file(FsPath::new("/tmp/board.canvas")));
        assert!(is_canvas_file(FsPath::new("/tmp/BOARD.CANVAS")));
        assert!(!is_canvas_file(FsPath::new("/tmp/photo.png")));
        assert!(!is_canvas_file(FsPath::new("/tmp/canvas")));
    }

    #[test]
    fn vault_relative_paths_are_recorded_from_the_base() {
        assert_eq!(
            vault_relative(
                FsPath::new("/vault/Hobbies/pasted-1.png"),
                FsPath::new("/vault")
            ),
            "Hobbies/pasted-1.png"
        );
        // Outside the base, only the file name survives.
        assert_eq!(
            vault_relative(
                FsPath::new("/elsewhere/pasted-2.png"),
                FsPath::new("/vault")
            ),
            "pasted-2.png"
        );
    }

    #[test]
    fn vault_root_infers_the_base_from_image_paths_without_obsidian() {
        // A vault copied without its `.obsidian` folder: the base is the
        // ancestor where the recorded image path actually resolves.
        let temp = std::env::temp_dir().join(format!("tb-vault-{}", std::process::id()));
        let hobbies = temp.join("Hobbies");
        std::fs::create_dir_all(&hobbies).unwrap();
        std::fs::write(hobbies.join("img.png"), b"x").unwrap();

        let document = CanvasDocument {
            nodes: vec![CanvasNode {
                id: "n".to_owned(),
                kind: "file".to_owned(),
                text: None,
                file: Some("Hobbies/img.png".into()),
                x: 0.0,
                y: 0.0,
                width: 10.0,
                height: 10.0,
                color: None,
                extra: Default::default(),
            }],
            edges: Vec::new(),
            extra: Default::default(),
        };

        let canvas = hobbies.join("board.canvas");
        assert_eq!(vault_root(&canvas, &document), temp);
        let _ = std::fs::remove_dir_all(&temp);
    }

    fn file_node(id: &str, file: &str) -> CanvasNode {
        CanvasNode {
            id: id.to_owned(),
            kind: "file".to_owned(),
            text: None,
            file: Some(file.to_owned()),
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 100.0,
            color: None,
            extra: Default::default(),
        }
    }

    #[test]
    fn front_matter_is_dropped_from_a_note_body() {
        let note = "---\ntitle: Ideas\ntags: [a, b]\n---\n\n# Ideas\n\nbody";
        assert_eq!(strip_front_matter(note), "# Ideas\n\nbody");
    }

    #[test]
    fn text_without_front_matter_is_untouched() {
        assert_eq!(strip_front_matter("# Ideas\n\nbody"), "# Ideas\n\nbody");
        // A horizontal rule further down is not front matter.
        assert_eq!(strip_front_matter("intro\n\n---\n\nmore"), "intro\n\n---\n\nmore");
        // An unterminated block is left alone rather than eating the note.
        assert_eq!(strip_front_matter("---\nstill going"), "---\nstill going");
    }

    #[test]
    fn only_image_nodes_get_image_handles() {
        // The bug this guards: every file node used to become an image handle,
        // so a note embedded in a canvas rendered as an empty card.
        let dir = std::env::temp_dir().join(format!("tb-embeds-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Ideas.md"), "---\ntitle: x\n---\nthe body").unwrap();

        let mut app = super::super::update_tests::app_with(
            vec![
                file_node("img", "sketch.png"),
                file_node("note", "Ideas.md"),
                file_node("doc", "paper.pdf"),
            ],
            Vec::new(),
        );
        app.base_path = dir.clone();

        let images = app.load_images(&app.nodes);
        assert_eq!(images.len(), 1, "only the image is an image");
        assert!(images.contains_key("img"));

        let notes = app.load_notes(&app.nodes);
        assert_eq!(notes.get("note").map(String::as_str), Some("the body"));
        assert!(!notes.contains_key("doc"), "a PDF is not a note");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_note_is_simply_absent() {
        let app = super::super::update_tests::app_with(
            vec![file_node("note", "Nowhere.md")],
            Vec::new(),
        );
        assert!(app.load_notes(&app.nodes).is_empty());
    }

    #[test]
    fn save_if_dirty_flushes_only_when_dirty() {
        let dir = std::env::temp_dir().join(format!("tb-quit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("quit.canvas");

        let document = CanvasDocument {
            nodes: vec![CanvasNode {
                id: "n".into(),
                kind: "text".into(),
                text: Some("hello".into()),
                file: None,
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 40.0,
                color: None,
                extra: Default::default(),
            }],
            edges: Vec::new(),
            extra: Default::default(),
        };

        // Clean document: quitting writes nothing.
        assert!(!save_if_dirty(false, &document, &path));
        assert!(!path.exists());

        // Unsaved edits: quitting flushes them, and they round-trip.
        assert!(save_if_dirty(true, &document, &path));
        let reloaded = CanvasDocument::load(&path).unwrap();
        assert_eq!(reloaded.nodes.len(), 1);
        assert_eq!(reloaded.nodes[0].text.as_deref(), Some("hello"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
