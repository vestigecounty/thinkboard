//! The `.canvas` file behind the board: opening, creating, autosaving, and
//! taking in what another process writes to it — plus resolving the Obsidian
//! vault root that a canvas's image paths are relative to.

use std::collections::{HashMap, HashSet};
use std::path::{Path as FsPath, PathBuf};

use iced::widget::image;
use iced::{Task, window};
use thinkboard_core::merge;

use super::history::{committed_snapshot, text_bitmap_reusable};
use super::sync::{self, OnDisk, changed_since, file_mtime};
use super::{App, Message};
use crate::model::{CanvasEdge, CanvasDocument, CanvasNode, NodeKind};

/// The embedded scratch board shown when no real canvas is configured. A
/// throwaway playground: never written to disk, reset on every launch. Its
/// button nodes (carrying a `tb_action` field) create or open a real board.
const SCRATCH_CANVAS: &str = include_str!("welcome.canvas");

/// Parses the embedded scratch board and finishes it for this machine and
/// `theme`. It's compiled in and validated by a unit test, so a parse failure
/// is a build-time bug, not a runtime condition.
pub(super) fn scratch_document(theme: &crate::theme::Theme) -> CanvasDocument {
    let mut document: CanvasDocument =
        serde_json::from_str(SCRATCH_CANVAS).expect("embedded scratch canvas is valid");
    finish_welcome(&mut document, &thinkboard_core::obsidian::vaults());
    fit_welcome(&mut document, theme);
    document
}

/// Grows each welcome card tall enough for its text in `theme`.
///
/// The file sizes its cards for one font and one wording, and neither holds:
/// `finish_welcome` writes the summon tip per platform — on macOS two
/// paragraphs into a card sized for one, cut off mid-sentence on the first
/// screen anyone sees — and a theme with a larger face overflows the rest.
/// Height only, and downward: widths and top edges are the layout, and a card
/// the file already gave room keeps it.
fn fit_welcome(document: &mut CanvasDocument, theme: &crate::theme::Theme) {
    use crate::text_raster;
    for node in document.nodes.iter_mut().filter(|node| node.is_text()) {
        let Some(text) = node.text.as_deref() else {
            continue;
        };
        let wrap = text_raster::card_text_wrap(node.width);
        let (_, height) = text_raster::measure_markdown(text, wrap, super::FONT_SIZE, theme);
        node.height = node.height.max((height + text_raster::NODE_PAD_Y).ceil());
    }
}

/// Fills in the parts of the welcome board that depend on where it's running:
/// how to summon the overlay here, and which Obsidian vaults are on this
/// machine.
///
/// Takes the vaults rather than reading them, so the shaping can be tested
/// without an Obsidian install.
fn finish_welcome(document: &mut CanvasDocument, vaults: &[thinkboard_core::obsidian::Vault]) {
    for node in &mut document.nodes {
        if node.id == "tip_summon" {
            node.text = Some(summon_tip().to_owned());
        }
    }

    // A button per vault Obsidian knows about, beside the "Use an Obsidian
    // vault…" picker. The whole point of reading the registry: someone arriving
    // from Obsidian shouldn't have to find their own vault in a folder dialog.
    // Capped, because the row is a row and a long tail of old vaults is not
    // what anyone came for.
    const SHOWN: usize = 3;
    /// Narrower than the button they hang from, so three fit in the column
    /// left of the tips.
    const WIDTH: f32 = 240.0;
    const GAP: f32 = 30.0;
    let anchor = document
        .nodes
        .iter()
        .find(|node| node.id == "action_vault")
        .map(|node| (node.x, node.y, node.height, node.color.clone()));
    let Some((x, y, height, color)) = anchor else {
        return;
    };
    for (i, vault) in vaults.iter().take(SHOWN).enumerate() {
        let id = format!("action_vault_{i}");
        let mut node = CanvasNode {
            // Above the button and fanning away from the board, not beside it
            // or below it: side by side the link to the second vault runs
            // straight through the first, and the row reads as a chain of one
            // vault leading to the next rather than three offers from one.
            x: x - i as f32 * (WIDTH + GAP),
            y: y - height - 60.0,
            width: WIDTH,
            height,
            id: id.clone(),
            kind: "text".to_owned(),
            text: Some(format!("Open vault **{}**", vault.name)),
            file: None,
            // The gold of the button they belong to, so the group reads as one.
            color: color.clone(),
            extra: Default::default(),
        };
        let _ = node.extra.insert(
            "tb_action".to_owned(),
            serde_json::Value::String(format!("use_vault:{}", vault.path.display())),
        );
        document.nodes.push(node);
        // Each hangs off the button itself — every link leaving the same top
        // port, so they fan out instead of threading through one another.
        document.edges.push(CanvasEdge {
            id: format!("e_{id}"),
            from_node: "action_vault".to_owned(),
            from_side: "top".to_owned(),
            to_node: id,
            to_side: "bottom".to_owned(),
            extra: Default::default(),
        });
    }
    // Nothing found: the button beside them is the only way in, and its label
    // shouldn't promise a list that isn't there.
    if vaults.is_empty() {
        for node in &mut document.nodes {
            if node.id == "action_vault" {
                node.text = Some("Choose an Obsidian vault…".to_owned());
            }
        }
    }
}

/// How to summon the overlay on this platform, for the welcome board.
///
/// macOS leads with the trackpad gesture because the default hotkey is the
/// Pause key, which macOS reports as F15 — a key a laptop hasn't got. Telling a
/// MacBook user to press it is telling them to do something impossible.
fn summon_tip() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "Swipe **four fingers down** on the trackpad to summon or dismiss the board over \
         whatever you're doing.\n\nThere's a hotkey too — **Pause**, which needs a full-size \
         keyboard. Set your own in Settings, or use the menu-bar icon."
    }
    #[cfg(not(target_os = "macos"))]
    {
        "Press **Pause** anytime to summon or dismiss the board over whatever you're doing. \
         Set your own shortcut in Settings."
    }
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

/// How long a board's file has to have gone unwritten before an unreadable one
/// is taken as finished rather than caught mid-write (see `check_reload`).
const UNREADABLE_SETTLED: std::time::Duration = std::time::Duration::from_secs(2);

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
///   0. take `configured` when the canvas is inside it — the user told us where
///      the vault is, which beats every guess below;
///   1. walk up from the canvas to a `.obsidian` marker (authoritative);
///   2. else infer it — walk up to the ancestor where an image `file` actually
///      resolves (handles a vault copied without its `.obsidian` folder);
///   3. else fall back to the canvas's own directory.
///
/// A canvas *outside* the configured vault falls through to the guesses, so
/// setting a vault never breaks a board opened from somewhere else.
pub(super) fn vault_root(
    canvas: &FsPath,
    document: &CanvasDocument,
    configured: Option<&FsPath>,
) -> PathBuf {
    let canvas_dir = canvas.parent().unwrap_or_else(|| FsPath::new("."));

    if let Some(vault) = configured
        && canvas.starts_with(vault)
    {
        return vault.to_path_buf();
    }

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
    /// adopts what it wrote as what's on disk — so the event our own write
    /// raises is ignored, and the next outside change is merged against it.
    /// Returns whether it wrote.
    pub(super) fn save_document(&mut self) -> bool {
        // The scratch board is never written to disk; a board whose file went
        // is written only when the board is told to.
        if self.scratch || self.disk.missing {
            return false;
        }
        let document = self.current_document();
        match document.save(&self.canvas_path) {
            Ok(()) => {
                self.doc_dirty = false;
                self.disk = OnDisk::new(document, &self.canvas_path);
                true
            }
            Err(error) => {
                eprintln!("autosave failed: {error}");
                false
            }
        }
    }

    /// Saves whatever is pending now, for leaving the board — quitting it, or
    /// switching to another — when there's no later tick to save on. Merged
    /// first with anything written there that hasn't been taken in, as an
    /// autosave is.
    fn flush(&mut self) {
        if self.disk_changed() {
            // Its task only refreshes bitmaps, which a board being left
            // won't draw.
            let _ = self.check_reload();
        }
        if self.doc_dirty {
            self.save_document();
        }
    }

    /// Whether the file has changed since this app last read or wrote it —
    /// written by something else, or gone.
    pub(super) fn disk_changed(&self) -> bool {
        !self.scratch && file_mtime(&self.canvas_path) != self.disk.mtime
    }

    /// Flushes a pending autosave (if any) and quits. Used on every exit path
    /// so edits made within the autosave debounce window are never lost.
    pub(super) fn save_and_exit(&mut self) -> Task<Message> {
        if !self.scratch {
            self.flush();
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
                let handle = match self.held_images.get(file) {
                    Some(held) => held.handle.clone(),
                    None => image::Handle::from_path(self.base_path.as_path().join(file)),
                };
                Some((node.id.clone(), handle))
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
    /// the choice so the next launch opens it directly.
    pub(super) fn open_canvas(&mut self, path: PathBuf) -> Task<Message> {
        // Flush pending edits to the board we're leaving before switching, so a
        // drag-drop / open while mid-edit doesn't drop them. No-op for the
        // scratch board (which is never saved) and when there's nothing dirty.
        if !self.scratch {
            self.flush();
        }

        let document = match CanvasDocument::load(&path) {
            Ok(document) => document,
            Err(error) => {
                eprintln!("could not open {}: {error}", path.display());
                return Task::none();
            }
        };

        self.canvas_path = path;
        self.base_path = vault_root(
            &self.canvas_path,
            &document,
            self.vault_path().as_deref(),
        );
        self.disk = OnDisk::new(document.clone(), &self.canvas_path);
        self.scratch = false;

        self.adopt_document(document);
        // A different board entirely: also drop the editor and the history,
        // and reframe the camera on the new content.
        self.editing = None;
        self.history.clear();
        self.edit_undo = None;
        self.gesture_undo = None;
        self.fresh_undo = None;
        self.doc_dirty = false;
        self.interacted = false;
        self.camera.fit(self.content_bounds());

        // Remember the working board so the next launch skips the scratch board.
        self.config.canvas = self.canvas_path.to_string_lossy().into_owned();
        self.persist_config();

        Task::batch([self.refresh_snapshots(), self.schedule_recluster()])
    }

    /// Takes in a change another program made to the board's file, merged
    /// with whatever here isn't saved yet, card by card (`thinkboard_core::
    /// merge`) — so neither side's work is lost to the other.
    ///
    /// It used to reload only when nothing here was unsaved and no card was
    /// being edited, and let the next save win otherwise: a card added in
    /// Obsidian while one was edited here was simply written over. Our own
    /// writes raise events too, and are told apart by the modification time
    /// they left.
    pub(super) fn check_reload(&mut self) -> Task<Message> {
        if self.scratch {
            return Task::none();
        }
        let mtime = file_mtime(&self.canvas_path);
        if mtime.is_none() {
            return self.follow_file();
        }
        if !self.disk.missing && !changed_since(mtime, self.disk.mtime) {
            return Task::none();
        }
        let theirs = match CanvasDocument::load(&self.canvas_path) {
            Ok(theirs) => theirs,
            Err(error) => {
                // Caught half-written: the rest of the write raises another
                // event. But one that has stayed unreadable a while was
                // finished that way — broken by hand in a text editor, say —
                // and waiting for it to parse would stop the autosave for
                // good. It's written over instead, as every outside change
                // was before there was a merge.
                let settled = mtime.and_then(|m| m.elapsed().ok()).is_some_and(|age| age > UNREADABLE_SETTLED);
                if settled {
                    eprintln!("{} can't be read, and will be saved over: {error}", self.canvas_path.display());
                    self.disk.mtime = mtime;
                }
                return Task::none();
            }
        };
        self.take_in(theirs)
    }

    /// The board's file is gone from where it was: renamed beside it, which is
    /// what renaming a board in Obsidian does, or moved away, or deleted.
    ///
    /// A rename is followed, so the next save goes to the new name — it used to
    /// write the old one again, and the folder held both. Anything else stops
    /// the autosave and says so on the board, where writing the file again is
    /// one click; quietly recreating a file someone just deleted would be
    /// worse than not saving.
    fn follow_file(&mut self) -> Task<Message> {
        let Some(path) = sync::find_renamed(&self.canvas_path, self.disk.id, self.disk.mtime) else {
            self.disk.missing = true;
            return Task::none();
        };
        self.canvas_path = path;
        // The next launch opens it under its new name.
        self.config.canvas = self.canvas_path.to_string_lossy().into_owned();
        self.persist_config();
        // Whatever it holds now, if it changed as it moved.
        self.check_reload()
    }

    /// Writes the board back where its file was, after something else moved
    /// or deleted it, and resumes autosaving there.
    pub(super) fn restore_file(&mut self) {
        if !self.disk.missing {
            return;
        }
        self.disk.missing = false;
        if !self.save_document() {
            // The folder went too, or it can't be written: still not saved.
            self.disk.missing = true;
        }
    }

    /// Merges `theirs` — the file as it is now — into the board.
    fn take_in(&mut self, theirs: CanvasDocument) -> Task<Message> {
        let ours = self.current_document();
        let editing = self.editing.as_ref().and_then(|e| self.nodes.get(e.index)).map(|n| n.id.clone());
        let held: HashSet<&str> = editing.iter().map(String::as_str).collect();
        let merged = merge::merge(&self.disk.document, &ours, &theirs, &held);
        // What the file lacks of the merge, the next save writes.
        let unsaved = !merge::same(&merged, &theirs);
        self.disk = OnDisk::new(theirs, &self.canvas_path);
        if merge::same(&merged, &ours) {
            // Nothing there that isn't here already.
            if !unsaved {
                self.doc_dirty = false;
            } else if !self.doc_dirty {
                self.mark_dirty();
            }
            return Task::none();
        }
        self.record_outside_change(&ours, &merged);
        self.replace_document(merged);
        if unsaved {
            self.mark_dirty();
        } else {
            self.doc_dirty = false;
        }
        Task::batch([self.refresh_snapshots(), self.schedule_recluster()])
    }

    /// Makes an outside change a step undo can take back, as an editor does
    /// when a file it has open changes on disk. The history used to be dropped
    /// instead, so Cmd+Z after any sync — Obsidian touching the file, or a
    /// sync client — did nothing at all.
    ///
    /// A gesture or an edit under way has already taken the point it'll undo
    /// to. The change is recorded from that point, as if it came first, and
    /// the point moves past it — so undoing the edit, once it's done, doesn't
    /// take the change with it, and undoing the change doesn't restore half
    /// an edit.
    fn record_outside_change(&mut self, ours: &CanvasDocument, merged: &CanvasDocument) {
        let transient = self.transient_node_ids();
        let point = [&self.fresh_undo, &self.edit_undo, &self.gesture_undo]
            .into_iter()
            .flatten()
            .next()
            .cloned()
            .unwrap_or_else(|| self.snapshot());
        for pending in [&mut self.fresh_undo, &mut self.edit_undo, &mut self.gesture_undo].into_iter().flatten() {
            let before = CanvasDocument {
                nodes: std::mem::take(&mut pending.nodes),
                edges: std::mem::take(&mut pending.edges),
                extra: ours.extra.clone(),
            };
            // Ours is the base here: what `before` lacks of it is the edit,
            // and what the merge adds to it is theirs.
            let past = merge::merge(ours, &before, merged, &HashSet::new());
            *pending = committed_snapshot(&past.nodes, &past.edges, &transient);
        }
        self.history.record(point);
    }

    /// Swaps in `document`, a merge of this board with its file, keeping what
    /// is keyed to a card by index on the card it was keyed to: its bitmap, its
    /// place in the selection, the editor open on it, the find bar's hits.
    /// Opening another board drops all of that, and reloading this one used
    /// to — which lost the selection and blanked every card for a frame.
    fn replace_document(&mut self, document: CanvasDocument) {
        let new_index: HashMap<&str, usize> =
            document.nodes.iter().enumerate().map(|(i, node)| (node.id.as_str(), i)).collect();
        let moved: Vec<Option<usize>> =
            self.nodes.iter().map(|node| new_index.get(node.id.as_str()).copied()).collect();
        let at = |old: usize| moved.get(old).copied().flatten();
        // A bitmap is kept for a card whose text and size are still the ones
        // it depicts.
        let depicts = |old: usize| {
            let new = at(old)?;
            let (was, is) = (&self.nodes[old], &document.nodes[new]);
            text_bitmap_reusable(&was.text, (was.width, was.height), &is.text, (is.width, is.height))
                .then_some(new)
        };
        self.rasters.text =
            std::mem::take(&mut self.rasters.text).into_iter().filter_map(|(i, v)| Some((depicts(i)?, v))).collect();
        self.rasters.staging =
            std::mem::take(&mut self.rasters.staging).into_iter().filter_map(|(i, v)| Some((depicts(i)?, v))).collect();
        self.spawning = std::mem::take(&mut self.spawning).into_iter().filter_map(|(i, v)| Some((at(i)?, v))).collect();
        self.selected = self.selected.iter().filter_map(|&i| at(i)).collect();
        self.pending_edit = self.pending_edit.and_then(at);
        self.fresh_node = self.fresh_node.and_then(at);
        if self.fresh_node.is_none() {
            self.fresh_undo = None;
        }
        if let Some(anim) = &mut self.layout_anim {
            anim.moves.retain_mut(|(node, ..)| at(*node).map(|new| *node = new).is_some());
        }
        let editing = self.editing.as_ref().map(|editing| at(editing.index));
        let edge_at = |old: Option<usize>| {
            let id = &self.edges.get(old?)?.id;
            document.edges.iter().position(|edge| &edge.id == id)
        };
        self.selected_edge = edge_at(self.selected_edge);
        self.hovered_edge = edge_at(self.hovered_edge);

        self.nodes = document.nodes;
        self.edges = document.edges;
        self.doc_extra = document.extra;
        self.images = self.load_images(&self.nodes);
        self.notes = self.load_notes(&self.nodes);
        match editing {
            Some(Some(index)) => {
                if let Some(editing) = &mut self.editing {
                    editing.index = index;
                }
                self.follow_edited_card();
            }
            // Never the case: the card being edited is held through the merge.
            Some(None) => self.editing = None,
            None => {}
        }
        if let Some(search) = &mut self.search {
            search.set_matches(super::search::find_matches(&self.nodes, &search.query));
            // A hit is painted by the live text path, so a card with one holds
            // no bitmap.
            for hit in &search.matches {
                self.rasters.invalidate(hit.node);
            }
        }
        self.cache.clear();
    }

    /// Installs a freshly loaded document, dropping the per-node caches and
    /// transient state that the old node indices keyed — for opening a
    /// different board.
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
        let document = scratch_document(&crate::theme::Theme::default());
        assert!(!document.nodes.is_empty());
        assert!(!document.edges.is_empty());
    }

    #[test]
    fn no_welcome_card_cuts_its_text_off() {
        // The summon tip is written per platform after the file is read, and
        // on macOS it ran two paragraphs into a card sized for one.
        let theme = crate::theme::Theme::default();
        for node in scratch_document(&theme).nodes.iter().filter(|node| node.is_text()) {
            let text = node.text.as_deref().unwrap_or_default();
            let wrap = crate::text_raster::card_text_wrap(node.width);
            let (_, height) =
                crate::text_raster::measure_markdown(text, wrap, super::super::FONT_SIZE, &theme);
            let needs = height + crate::text_raster::NODE_PAD_Y;
            assert!(needs <= node.height, "{} needs {needs} and has {}", node.id, node.height);
        }
    }

    /// The welcome board as it parses, before it is finished for a machine.
    fn welcome() -> CanvasDocument {
        serde_json::from_str(SCRATCH_CANVAS).expect("the embedded board parses")
    }

    fn actions(document: &CanvasDocument) -> Vec<&str> {
        document
            .nodes
            .iter()
            .filter_map(|node| node.extra.get("tb_action").and_then(|v| v.as_str()))
            .collect()
    }

    fn vault(name: &str) -> thinkboard_core::obsidian::Vault {
        thinkboard_core::obsidian::Vault {
            path: FsPath::new("/vaults").join(name),
            name: name.to_owned(),
            open: false,
            last_opened: 0,
        }
    }

    #[test]
    fn the_scratch_board_has_its_onboarding_buttons() {
        let mut document = welcome();
        finish_welcome(&mut document, &[]);
        assert_eq!(actions(&document), ["new_board", "open_board", "open_vault"]);
        // Every button is a real node the click handler can find by its action.
        for button in document.nodes.iter().filter(|n| n.extra.contains_key("tb_action")) {
            assert_eq!(button.kind, "text");
            assert!(button.text.as_deref().is_some_and(|t| !t.is_empty()));
        }
    }

    #[test]
    fn a_vault_obsidian_knows_about_becomes_a_button() {
        // The point of reading Obsidian's registry: arriving from Obsidian
        // shouldn't mean finding your own vault in a folder dialog.
        let mut document = welcome();
        finish_welcome(&mut document, &[vault("Notes"), vault("Work")]);
        assert_eq!(
            actions(&document),
            ["new_board", "open_board", "open_vault", "use_vault:/vaults/Notes", "use_vault:/vaults/Work"],
        );
        let named = document
            .nodes
            .iter()
            .find(|n| n.text.as_deref().is_some_and(|t| t.contains("Notes")))
            .expect("a button naming the vault");
        assert!(named.text.as_deref().unwrap().contains("Notes"));
    }

    #[test]
    fn the_vault_buttons_are_capped_linked_and_clear_of_the_tips() {
        let many: Vec<_> = ["A", "B", "C", "D", "E"].iter().map(|n| vault(n)).collect();
        let mut document = welcome();
        finish_welcome(&mut document, &many);
        let buttons: Vec<_> = document
            .nodes
            .iter()
            .filter(|n| {
                n.extra.get("tb_action").and_then(|v| v.as_str()).is_some_and(|a| a.starts_with("use_vault:"))
            })
            .collect();
        assert_eq!(buttons.len(), 3, "a long tail of old vaults is not what anyone came for");
        // One row above the button, fanning away from the board: side by side,
        // the link to the second vault runs through the first and the row reads
        // as a chain of one vault leading to the next.
        assert!(buttons.windows(2).all(|w| w[0].y == w[1].y && w[0].x > w[1].x));
        let anchor = document.nodes.iter().find(|n| n.id == "action_vault").expect("the button");
        assert!(buttons.iter().all(|b| b.y + b.height < anchor.y), "above the root button");
        assert!(buttons.iter().all(|b| b.x <= anchor.x), "and away from the rest of the board");

        // Each hangs off the button itself, from the same top port — so they fan
        // out rather than threading through one another.
        for button in &buttons {
            let edge = document
                .edges
                .iter()
                .find(|e| e.to_node == button.id)
                .unwrap_or_else(|| panic!("{} is not connected to anything", button.id));
            assert_eq!(edge.from_node, "action_vault", "parented to the button, not to a sibling");
            assert_eq!((edge.from_side.as_str(), edge.to_side.as_str()), ("top", "bottom"));
        }

        // And they wear its colour, so the group reads as one offer.
        assert!(buttons.iter().all(|b| b.color == anchor.color && b.color.is_some()));

        // Clear of the tips, which run down the right of the board.
        let tips = document.nodes.iter().filter(|n| n.id.starts_with("tip_"));
        for tip in tips {
            for button in &buttons {
                let overlaps = button.x < tip.x + tip.width
                    && tip.x < button.x + button.width
                    && button.y < tip.y + tip.height
                    && tip.y < button.y + button.height;
                assert!(!overlaps, "{} overlaps {}", button.id, tip.id);
            }
        }
    }

    #[test]
    fn the_summon_tip_says_what_works_on_this_platform() {
        let mut document = welcome();
        finish_welcome(&mut document, &[]);
        let tip = document
            .nodes
            .iter()
            .find(|n| n.id == "tip_summon")
            .and_then(|n| n.text.clone())
            .expect("a summon tip");
        assert_ne!(tip, "SUMMON", "the placeholder must be filled in");
        if cfg!(target_os = "macos") {
            // The default hotkey is F15, which a MacBook hasn't got, so the
            // gesture has to lead — telling someone to press Pause is telling
            // them to do something impossible.
            assert!(tip.starts_with("Swipe"), "{tip}");
            let (swipe, pause) = (tip.find("four fingers"), tip.find("Pause"));
            assert!(swipe < pause, "the gesture must come first: {tip}");
        } else {
            assert!(tip.contains("Pause"), "{tip}");
            assert!(!tip.contains("four fingers"), "no trackpad gesture off macOS: {tip}");
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
        assert_eq!(vault_root(&canvas, &document, None), temp);
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn a_configured_vault_beats_the_inferred_base() {
        // Nothing on disk to walk to, so every guess would land on the canvas's
        // own folder — the configured vault is what makes the real base known.
        let canvas = FsPath::new("/vault/Projects/board.canvas");
        let document = CanvasDocument {
            nodes: Vec::new(),
            edges: Vec::new(),
            extra: Default::default(),
        };
        let vault = FsPath::new("/vault");
        assert_eq!(vault_root(canvas, &document, Some(vault)), vault);

        // A board opened from outside the vault is unaffected by it: the vault
        // is where we *look* for boards, not a claim about every board there is.
        let outside = FsPath::new("/elsewhere/board.canvas");
        assert_eq!(
            vault_root(outside, &document, Some(vault)),
            FsPath::new("/elsewhere")
        );
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
}
