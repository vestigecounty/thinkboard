//! The in-place note editor: opening and committing an edit session, the
//! text-level undo inside it, and the Markdown marker toggles (Cmd+B/I).
//!
//! The editor is a fixed-size widget clamped to the viewport, so a node hanging
//! off an edge is brought fully into view before editing starts — and editing
//! ends if a pan takes it back out, since the widget can't render off the
//! top/left and its text would detach from the canvas-drawn card.

use std::time::{Duration, Instant};

use iced::widget::text_editor;
use iced::{Point, Task, Vector};

use iced::Rectangle;

use super::arrange::node_world_rect;
use super::{App, CameraFocus, FONT_SIZE, Message};
use crate::canvas::{board, markdown, text_raster};

/// Where and how to lay out the editing overlay. See [`App::editor_box`].
pub(super) struct EditorBox {
    pub(super) origin: Point,
    pub(super) width: f32,
    pub(super) height: f32,
    /// Whether lines are centred, matching how the read view draws them.
    pub(super) centered: bool,
    /// Logical (unzoomed) point size — a body and a caption differ.
    pub(super) font_size: f32,
}

/// Edits within this window coalesce into one undo step.
const EDIT_COALESCE: Duration = Duration::from_millis(600);

/// Narrowest the in-place editor is drawn, in world units. A caret needs
/// somewhere to sit even on an empty card, so this is the floor both the body
/// and the group-caption editors clamp to.
const MIN_EDITOR_WIDTH: f32 = 24.0;

/// Which piece of a node the editor is editing.
///
/// The two share everything — the widget, the text-level undo, the key
/// bindings — and differ only in where the text comes from and goes back to,
/// and in where the box is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EditTarget {
    /// A text node's body, laid out over the whole card.
    Body,
    /// A group frame's caption, laid out along its header strip.
    Label,
}

pub(super) struct Editing {
    pub(super) index: usize,
    pub(super) target: EditTarget,
    pub(super) content: text_editor::Content,
    /// Text-level undo/redo for this session (iced's editor has none). Whole
    /// snapshots, coalesced by a short pause between edits.
    undo: Vec<String>,
    redo: Vec<String>,
    last_snapshot: Instant,
    /// A world-space click position whose caret placement is deferred until the
    /// editor has laid out (its buffer is shaped at `font_size: 1.0` until then,
    /// so a point→caret hit-test is only valid after the first frame). Set when
    /// editing starts from a double-click; consumed by
    /// [`Message::PlaceEditorCaret`].
    pub(super) pending_caret: Option<Point>,
}

impl Editing {
    fn new(index: usize, target: EditTarget, text: &str) -> Self {
        Editing {
            index,
            target,
            content: text_editor::Content::with_text(text),
            undo: Vec::new(),
            redo: Vec::new(),
            last_snapshot: Instant::now(),
            pending_caret: None,
        }
    }

    /// Records the pre-edit text as an undo point. `force` snapshots even mid
    /// burst (for discrete actions like bold); otherwise it coalesces edits
    /// that happen close together.
    fn snapshot(&mut self, force: bool) {
        let now = Instant::now();
        let paused = now.duration_since(self.last_snapshot) > EDIT_COALESCE;
        self.last_snapshot = now;
        if force || paused || self.undo.is_empty() {
            let text = self.content.text();
            if self.undo.last() != Some(&text) {
                self.undo.push(text);
                self.redo.clear();
            }
        }
    }

    fn undo(&mut self) -> Option<String> {
        let previous = self.undo.pop()?;
        self.redo.push(self.content.text());
        self.content = text_editor::Content::with_text(&previous);
        Some(previous)
    }

    fn redo(&mut self) -> Option<String> {
        let next = self.redo.pop()?;
        self.undo.push(self.content.text());
        self.content = text_editor::Content::with_text(&next);
        Some(next)
    }
}

/// Toggles a Markdown wrapper (`marker`, e.g. `**` or `*`) around the editor's
/// selection: adds it when absent, removes it when the selection is already
/// wrapped (markers inside *or* immediately outside the selection). Leading and
/// trailing whitespace in the selection stays outside the markers, so
/// selecting `word1< word2>` bolds `word2`, not ` word2`. With no selection,
/// inserts an empty pair with the caret between them. Returns whether the text
/// changed. Line endings are assumed to be `\n` (as in canvas node text).
fn toggle_marker(content: &mut text_editor::Content, marker: &str) -> bool {
    let full = content.text();
    let cursor = content.cursor();
    let Some(anchor) = cursor.selection else {
        // No selection: insert an empty pair and put the caret inside it.
        content.perform(text_editor::Action::Edit(text_editor::Edit::Paste(
            std::sync::Arc::new(format!("{marker}{marker}")),
        )));
        for _ in 0..marker.chars().count() {
            content.perform(text_editor::Action::Move(text_editor::Motion::Left));
        }
        return true;
    };

    let a = offset_of(&full, anchor);
    let b = offset_of(&full, cursor.position);
    let (start, end) = (a.min(b), a.max(b));

    let Some((new_full, sel_start, sel_end)) = toggle_wrap(&full, start, end, marker) else {
        return false; // nothing, or only whitespace, selected
    };

    // Re-select the affected core so a repeated shortcut toggles it back.
    *content = text_editor::Content::with_text(&new_full);
    content.move_to(text_editor::Cursor {
        position: position_at(&new_full, sel_end),
        selection: Some(position_at(&new_full, sel_start)),
    });
    true
}

/// What Return does with the caret at byte `column` of `line`.
#[derive(Debug, PartialEq, Eq)]
enum ReturnKey {
    /// An ordinary line break.
    Plain,
    /// Break the line, and start the new one with this.
    Continue(String),
    /// The item was empty: the line becomes this — the item stepped out a
    /// level, or nothing — and no break is made.
    End(String),
}

/// Return the way Obsidian's editor does it: in a list item or a quote, the new
/// line starts with the same marker — the next number for an ordered list, an
/// unticked box for a task — so a list is written by typing and pressing
/// Return. Return on an *empty* item is how you stop: a nested one steps out a
/// level, a top-level one loses its marker. Anywhere else, and with the caret
/// still inside the marker, it's a plain line break.
fn on_return(line: &str, column: usize) -> ReturnKey {
    let Some(prefix) = MarkerPrefix::parse(line) else {
        return ReturnKey::Plain;
    };
    if column < prefix.len {
        return ReturnKey::Plain;
    }
    if !line[prefix.len..].trim().is_empty() {
        return ReturnKey::Continue(prefix.next());
    }
    ReturnKey::End(prefix.ended())
}

/// The leading run of a line that Return carries over: indentation, any quote
/// markers, then a list marker (with a task box, if it has one).
struct MarkerPrefix<'a> {
    /// Whitespace before everything else.
    indent: &'a str,
    /// `>` markers, as written (`"> "`, `"> > "`); empty outside a quote.
    quote: &'a str,
    /// Whitespace between the quote and the list marker.
    inner: &'a str,
    list: Option<ListMarker>,
    /// Bytes of `line` the prefix covers.
    len: usize,
}

#[derive(Clone, Copy)]
enum ListMarker {
    Bullet { mark: char, task: bool },
    Ordered { number: u64, delim: char },
}

impl<'a> MarkerPrefix<'a> {
    fn parse(line: &'a str) -> Option<Self> {
        let blank = |c: char| c == ' ' || c == '\t';
        let indent_len = line.len() - line.trim_start_matches(blank).len();
        let mut at = indent_len;
        while line[at..].starts_with('>') {
            at += 1;
            at += line[at..].len() - line[at..].trim_start_matches(blank).len();
        }
        let quote_end = at;
        at += line[at..].len() - line[at..].trim_start_matches(blank).len();
        let inner_end = at;

        let rest = &line[at..];
        let spaced = |after: usize| rest[after..].starts_with([' ', '\t']);
        let list = if let Some(mark) = rest.chars().next().filter(|c| matches!(c, '-' | '*' | '+'))
            && spaced(1)
        {
            at += 2;
            let task = ["[ ] ", "[x] ", "[X] "].iter().any(|box_| line[at..].starts_with(box_));
            if task {
                at += 4;
            }
            Some(ListMarker::Bullet { mark, task })
        } else {
            let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
            let delim = rest[digits..].chars().next();
            match delim {
                Some(delim @ ('.' | ')')) if (1..=9).contains(&digits) && spaced(digits + 1) => {
                    at += digits + 2;
                    Some(ListMarker::Ordered { number: rest[..digits].parse().ok()?, delim })
                }
                _ => None,
            }
        };
        // Neither a quote nor a list: an ordinary line.
        if list.is_none() && quote_end == indent_len {
            return None;
        }
        Some(Self {
            indent: &line[..indent_len],
            quote: &line[indent_len..quote_end],
            inner: &line[quote_end..inner_end],
            list,
            len: if list.is_some() { at } else { quote_end },
        })
    }

    /// The prefix the next item starts with.
    fn next(&self) -> String {
        let (inner, list) = match self.list {
            Some(list) => (self.inner, list.written(true)),
            None => ("", String::new()),
        };
        format!("{}{}{inner}{list}", self.indent, self.quote)
    }

    /// What an empty item's line becomes when Return ends it: the same item a
    /// level out if it was nested, else the line without its list marker —
    /// which, inside a quote, leaves the quote, and outside one leaves nothing.
    fn ended(&self) -> String {
        let Some(list) = self.list else {
            return String::new();
        };
        // What nests a list: indentation before a quote's markers outside a
        // quote, and after them inside one.
        if self.quote.is_empty() {
            if self.indent.is_empty() {
                return String::new();
            }
            return format!("{}{}", outdent(self.indent), list.written(false));
        }
        if self.inner.is_empty() {
            return format!("{}{}", self.indent, self.quote);
        }
        format!("{}{}{}{}", self.indent, self.quote, outdent(self.inner), list.written(false))
    }
}

impl ListMarker {
    /// The marker as it starts an item — the following number when `advance`.
    /// A task's box is always unticked: a new item, or one that stepped out
    /// empty, has nothing done yet.
    fn written(self, advance: bool) -> String {
        match self {
            ListMarker::Bullet { mark, task } => format!("{mark} {}", if task { "[ ] " } else { "" }),
            ListMarker::Ordered { number, delim } => {
                format!("{}{delim} ", if advance { number + 1 } else { number })
            }
        }
    }
}

/// `indent` one level shallower: a tab if it ends in one, else back to the
/// previous multiple of four spaces.
fn outdent(indent: &str) -> &str {
    if let Some(rest) = indent.strip_suffix('\t') {
        return rest;
    }
    let cut = match indent.len() % 4 {
        0 => 4,
        partial => partial,
    };
    &indent[..indent.len().saturating_sub(cut)]
}

/// Pure core of [`toggle_marker`]: given the full text and a selection byte
/// range `[start, end)`, returns the new text and the byte range to re-select,
/// or `None` when the selection has no non-whitespace core to wrap.
fn toggle_wrap(full: &str, start: usize, end: usize, marker: &str) -> Option<(String, usize, usize)> {
    // Shrink the selection to its non-whitespace core, leaving surrounding
    // spaces outside the markers.
    let selected = &full[start..end];
    let core_start = start + (selected.len() - selected.trim_start().len());
    let core_end = end - (selected.len() - selected.trim_end().len());
    if core_start >= core_end {
        return None;
    }
    let core = &full[core_start..core_end];

    let (cut_start, cut_end, replacement) = if wrapped_by(core, marker) {
        // Markers sit inside the selection — strip them.
        let inner = &core[marker.len()..core.len() - marker.len()];
        (core_start, core_end, inner.to_owned())
    } else if ends_with_marker(&full[..core_start], marker)
        && starts_with_marker(&full[core_end..], marker)
    {
        // Markers sit just outside the selection (e.g. a double-clicked word) —
        // remove them.
        (
            core_start - marker.len(),
            core_end + marker.len(),
            core.to_owned(),
        )
    } else {
        (core_start, core_end, format!("{marker}{core}{marker}"))
    };

    let mut new_full = String::with_capacity(full.len() + 2 * marker.len());
    new_full.push_str(&full[..cut_start]);
    new_full.push_str(&replacement);
    new_full.push_str(&full[cut_end..]);
    Some((new_full, cut_start, cut_start + replacement.len()))
}

/// Byte offset in `text` of a `(line, column)` position (`\n`-separated lines,
/// `column` a byte index within its line).
fn offset_of(text: &str, pos: text_editor::Position) -> usize {
    let mut offset = 0;
    for (i, line) in text.split('\n').enumerate() {
        if i == pos.line {
            return offset + pos.column.min(line.len());
        }
        offset += line.len() + 1; // + '\n'
    }
    text.len()
}

/// Inverse of [`offset_of`]: the `(line, column)` position of a byte offset.
fn position_at(text: &str, offset: usize) -> text_editor::Position {
    let prefix = &text[..offset.min(text.len())];
    text_editor::Position {
        line: prefix.matches('\n').count(),
        column: prefix.len() - prefix.rfind('\n').map_or(0, |p| p + 1),
    }
}

/// Whether `core` both starts and ends with `marker` (so it can be unwrapped).
fn wrapped_by(core: &str, marker: &str) -> bool {
    core.len() >= 2 * marker.len()
        && starts_with_marker(core, marker)
        && ends_with_marker(core, marker)
}

/// `s` starts with exactly `marker` — not a longer run of `*` (so `*` doesn't
/// match one star of a `**` bold marker).
fn starts_with_marker(s: &str, marker: &str) -> bool {
    s.starts_with(marker) && !s[marker.len()..].starts_with('*')
}

/// The value an edit session is changing, for the before/after comparison that
/// decides whether it earns an undo step.
fn edited_value(node: &crate::model::CanvasNode, target: EditTarget) -> Option<String> {
    match target {
        EditTarget::Body => node.text.clone(),
        EditTarget::Label => node.label().map(str::to_owned),
    }
}

/// `s` ends with exactly `marker`, not part of a longer run of `*`.
fn ends_with_marker(s: &str, marker: &str) -> bool {
    s.ends_with(marker) && !s[..s.len() - marker.len()].ends_with('*')
}

impl App {
    /// Opens the in-place text editor for a node and focuses it.
    pub(super) fn begin_editing(&mut self, index: usize, caret_at: Option<Point>) -> Task<Message> {
        let Some(node) = self.nodes.get(index) else {
            return Task::none();
        };
        let (target, seed) = if node.is_text() {
            (EditTarget::Body, node.text.as_deref().unwrap_or("").trim().to_owned())
        } else if node.is_group() {
            (EditTarget::Label, node.label().unwrap_or("").to_owned())
        } else {
            return Task::none();
        };
        // Capture a pre-edit snapshot to make the edit undoable — but not for
        // a brand-new node (its creation isn't an undoable edit).
        self.edit_undo = (self.fresh_node != Some(index)).then(|| self.snapshot());
        self.rasters.invalidate(index);
        let mut editing = Editing::new(index, target, &seed);
        // Defer caret placement to the first frame (see `pending_caret`); a
        // `window::frames()` subscription then fires `PlaceEditorCaret`.
        editing.pending_caret = caret_at;
        self.editing = Some(editing);
        self.cache.clear();
        // With scrollbars on, editing a node re-grows it to fit (its shrink was
        // temporary): drop its scroll and autosize before anything else, so the
        // editor lays out over the full content rather than a scrolled window.
        if target == EditTarget::Body && self.config.node_scrollbars {
            if let Some(id) = self.nodes.get(index).map(|n| n.id.clone()) {
                self.node_scroll.remove(&id);
            }
            self.autosize_node(index, false);
        }
        // The edit overlay is a fixed-size widget clamped to the viewport, so a
        // node hanging off an edge (or larger than the screen) would rewrap/clip.
        // Bring it fully into view first so the editor lays out like read mode.
        self.ensure_node_visible_for_edit(index);
        iced::advanced::widget::operate(iced::advanced::widget::operation::focusable::focus_next())
    }

    /// Commits the open edit: trims the text, settles the card size, records an
    /// undo step only if the text actually changed, and takes back a
    /// drag-created node that was left empty. May kick off AI suggestions for a
    /// freshly created note that hangs off a parent.
    pub(super) fn stop_editing(&mut self) -> Task<Message> {
        let Some(target) = self.editing.as_ref().map(|e| e.target) else {
            return Task::none();
        };
        // Id of the node being edited, before any removal below.
        let edited_id = self
            .editing
            .as_ref()
            .and_then(|editing| self.nodes.get(editing.index))
            .map(|node| node.id.clone());
        // A freshly-added connected node worth asking Copilot about.
        let mut completion_anchor: Option<String> = None;

        if let Some(editing) = &self.editing {
            let index = editing.index;
            match target {
                EditTarget::Body => {
                    // A drag-created node left without text disappears again.
                    let empty = self.nodes.get(index).is_some_and(|node| {
                        node.text.as_deref().unwrap_or("").trim().is_empty()
                    });
                    if self.fresh_node == Some(index) && empty {
                        // Fresh nodes are always last, so removal does not shift
                        // the indices keying the snapshot maps.
                        let id = self.nodes.remove(index).id;
                        self.edges
                            .retain(|edge| edge.from_node != id && edge.to_node != id);
                        self.rasters.invalidate(index);
                        self.spawning.remove(&index);
                    } else {
                        // Editing done: drop the whitespace that only mattered
                        // while typing, and settle the size.
                        if let Some(node) = self.nodes.get_mut(index)
                            && let Some(text) = &node.text
                        {
                            let trimmed = text.trim();
                            if trimmed != text {
                                node.text = Some(trimmed.to_owned());
                            }
                        }
                        self.autosize_node(index, true);
                        // A fresh node with a parent → ask for sibling ideas.
                        completion_anchor = self.completion_anchor(index);
                    }
                }
                EditTarget::Label => {
                    // A caption left blank clears the label rather than deleting
                    // the frame — an unnamed group is a perfectly good group.
                    let caption = editing.content.text();
                    if let Some(node) = self.nodes.get_mut(index) {
                        node.set_label(&caption);
                    }
                }
            }
        }

        // Commit the pre-edit snapshot as an undo point only if the edited
        // value actually changed.
        if let (Some(snapshot), Some(id)) = (self.edit_undo.take(), edited_id) {
            let before = snapshot
                .nodes
                .iter()
                .find(|n| n.id == id)
                .and_then(|n| edited_value(n, target));
            let after = self
                .nodes
                .iter()
                .find(|n| n.id == id)
                .and_then(|n| edited_value(n, target));
            if before != after {
                self.history.record(snapshot);
                // The card resized; re-lay-out its graph if enabled (covered by
                // the undo snapshot just recorded). A caption changes no
                // geometry, so it never triggers one.
                if target == EditTarget::Body
                    && let Some(index) = self.nodes.iter().position(|n| n.id == id)
                {
                    self.auto_relayout(index, false);
                }
            }
        }
        self.fresh_node = None;
        self.editing = None;
        self.mark_dirty();
        self.cache.clear();
        let mut tasks = vec![self.refresh_snapshots(), self.schedule_recluster()];
        if let Some(anchor) = completion_anchor {
            tasks.push(self.begin_completions(anchor));
        }
        Task::batch(tasks)
    }

    /// Applies one editor action, mirroring the result back onto the node.
    pub(super) fn editor_action(&mut self, action: text_editor::Action) {
        let edited = if let Some(editing) = &mut self.editing {
            let is_edit = action.is_edit();
            if is_edit {
                editing.snapshot(false);
            }
            editing.content.perform(action);
            is_edit.then(|| (editing.index, editing.content.text()))
        } else {
            None
        };
        self.commit_editor_text(edited);
    }

    /// Return in the editor — see [`on_return`]. A caption is one line and a
    /// selection is being replaced, so both get the plain break.
    pub(super) fn editor_return(&mut self) {
        let plain = text_editor::Action::Edit(text_editor::Edit::Enter);
        let Some(editing) = &mut self.editing else {
            return;
        };
        let cursor = editing.content.cursor();
        if cursor.selection.is_some() || editing.target == EditTarget::Label {
            return self.editor_action(plain);
        }
        let row = cursor.position.line;
        let line = editing.content.line(row).map(|line| line.text.into_owned()).unwrap_or_default();
        let edit = match on_return(&line, cursor.position.column) {
            ReturnKey::Plain => return self.editor_action(plain),
            ReturnKey::Continue(prefix) => text_editor::Edit::Paste(std::sync::Arc::new(format!("\n{prefix}"))),
            ReturnKey::End(line_now) => {
                // Select the whole line, then write what it becomes over it.
                editing.content.move_to(text_editor::Cursor {
                    position: text_editor::Position { line: row, column: line.len() },
                    selection: Some(text_editor::Position { line: row, column: 0 }),
                });
                if line_now.is_empty() {
                    text_editor::Edit::Delete
                } else {
                    text_editor::Edit::Paste(std::sync::Arc::new(line_now))
                }
            }
        };
        self.editor_action(text_editor::Action::Edit(edit));
    }

    /// Wraps the editor selection in a Markdown marker (`**` bold, `*` italic).
    pub(super) fn wrap_selection(&mut self, marker: &str) {
        let edited = if let Some(editing) = &mut self.editing {
            editing.snapshot(true);
            toggle_marker(&mut editing.content, marker)
                .then(|| (editing.index, editing.content.text()))
        } else {
            None
        };
        self.commit_editor_text(edited);
    }

    /// Applies a text-level undo (`redo = false`) or redo within the current
    /// edit session.
    pub(super) fn apply_edit_history(&mut self, redo: bool) {
        let edited = self.editing.as_mut().and_then(|editing| {
            let text = if redo { editing.redo() } else { editing.undo() }?;
            Some((editing.index, text))
        });
        if edited.is_some() {
            self.cache.clear();
        }
        self.commit_editor_text(edited);
    }

    /// Writes the editor's text back onto its node, refitting the card when it
    /// is the body that changed. A caption doesn't resize its frame — the frame
    /// is geometry the user placed.
    fn commit_editor_text(&mut self, edited: Option<(usize, String)>) {
        let Some((index, text)) = edited else {
            return;
        };
        let target = self.editing.as_ref().map(|e| e.target).unwrap_or(EditTarget::Body);
        match target {
            EditTarget::Body => {
                if let Some(node) = self.nodes.get_mut(index) {
                    node.text = Some(text);
                }
                self.autosize_node(index, false);
            }
            EditTarget::Label => {
                if let Some(node) = self.nodes.get_mut(index) {
                    node.set_label(&text);
                }
            }
        }
        self.mark_dirty();
    }

    /// Places the caret deferred from a double-click, once the editor has laid
    /// out. Waits for any fly-to-fit (started in `begin_editing`) to settle, so
    /// the editor is unclamped and its geometry final before we hit-test —
    /// `pending_caret` stays set, so the frames subscription fires again next
    /// frame.
    pub(super) fn place_editor_caret(&mut self) {
        if self.camera_focus.is_some() {
            return;
        }
        let Some((index, world)) = self
            .editing
            .as_ref()
            .and_then(|e| Some((e.index, e.pending_caret?)))
        else {
            return;
        };
        let local = self.editor_text_origin(index).map(|origin| {
            let screen = self.camera.to_screen(world);
            Point::new(screen.x - origin.x, screen.y - origin.y)
        });
        if let Some(editing) = self.editing.as_mut() {
            editing.pending_caret = None;
            if let Some(local) = local {
                editing.content.perform(text_editor::Action::Click(local));
            }
        }
        self.cache.clear();
    }

    /// Whether node `index` sits fully inside the viewport with a small margin —
    /// the condition under which the edit overlay (a viewport-clamped widget)
    /// lays out cleanly. Drives fit-on-edit and exit-editing-on-pan-out.
    fn node_fully_visible(&self, index: usize) -> bool {
        const MARGIN: f32 = 12.0;
        let Some(world) = self.edit_rect(index) else {
            return false;
        };
        let rect = self.camera.rect_to_screen(world);
        let vp = self.camera.viewport;
        rect.x >= MARGIN
            && rect.y >= MARGIN
            && rect.x + rect.width <= vp.width - MARGIN
            && rect.y + rect.height <= vp.height - MARGIN
    }

    /// If editing and a pan/zoom has moved the node out of view, commit and leave
    /// editing: the edit overlay can't render off the top/left (widget clamp), so
    /// its text would otherwise detach from the canvas-drawn card.
    pub(super) fn exit_edit_if_offscreen(&mut self) -> Task<Message> {
        match self.editing.as_ref().map(|e| e.index) {
            Some(index) if !self.node_fully_visible(index) => Task::done(Message::StopEditing),
            _ => Task::none(),
        }
    }

    /// Flies the camera to center the node being edited when it doesn't fully fit
    /// in the viewport (zooming out to fit if it's larger than the screen); a
    /// no-op when it already fits, so on-screen edits don't move the view.
    fn ensure_node_visible_for_edit(&mut self, index: usize) {
        const MARGIN: f32 = 12.0;
        if self.node_fully_visible(index) {
            return;
        }
        let Some(world) = self.edit_rect(index) else {
            return;
        };
        let vp = self.camera.viewport;
        let center = Vector::new(world.center_x(), world.center_y());
        // Largest zoom at which it fits with margin; never zoom in past the
        // current level — only pull back when it's too big to fit.
        let fit = ((vp.width - 2.0 * MARGIN) / world.width)
            .min((vp.height - 2.0 * MARGIN) / world.height);
        let target_zoom =
            self.camera.zoom.min(fit).clamp(self.config.min_zoom, self.config.max_zoom);
        self.camera_focus = Some(CameraFocus {
            target_zoom,
            target_offset: center * -target_zoom,
            last_tick: Instant::now(),
        });
    }

    /// The world-space rectangle an edit session is anchored to: the whole card
    /// for a body, the header strip for a caption. Everything that asks "is the
    /// thing being edited on screen?" means this, not the node — a frame is
    /// routinely larger than the viewport, and fitting all of it to edit its
    /// caption would zoom the board out for no reason.
    fn edit_rect(&self, index: usize) -> Option<Rectangle> {
        let node = self.nodes.get(index)?;
        let rect = node_world_rect(node);
        match self.editing.as_ref()?.target {
            EditTarget::Body => Some(rect),
            EditTarget::Label => Some(board::group_header(rect, 1.0)),
        }
    }

    /// The editing overlay's box: where to put it, how big, and how to lay the
    /// text out inside. `view` builds the editor from this and deferred caret
    /// placement maps a click against its origin — so both stay in sync.
    pub(super) fn editor_box(&self, index: usize) -> Option<EditorBox> {
        let node = self.nodes.get(index)?;
        let editing = self.editing.as_ref()?;
        let rect = self.camera.rect_to_screen(node_world_rect(node));
        let zoom = self.camera.zoom;

        if editing.target == EditTarget::Label {
            // Along the header strip, laid out where `draw_groups` puts the
            // caption so entering edit mode doesn't shift the text.
            let pad = board::GROUP_LABEL_PAD * zoom;
            let font_size = board::GROUP_LABEL_SIZE;
            let height = font_size * zoom * text_raster::LINE_HEIGHT + 2.0;
            return Some(EditorBox {
                origin: Point::new(rect.x + pad, rect.y + pad),
                width: (rect.width - pad * 2.0).max(MIN_EDITOR_WIDTH * zoom),
                height,
                centered: false,
                font_size,
            });
        }

        let wrap = text_raster::card_text_wrap(node.width);
        // Measure/size the text at the editor's inner column (inset a side like
        // read mode), not the full card wrap, or a wrapping note is a line short
        // and the box mis-centers.
        let text_wrap = text_raster::editor_text_wrap(node.width);
        let text = editing.content.text();
        let (text_width, text_height) =
            text_raster::measure_source(&text, text_wrap, FONT_SIZE, zoom, &self.theme);
        let caret_slack = 4.0;
        // Bound the box to the card, so a note longer than its card scrolls
        // *within* it instead of spilling over the board above and below.
        //
        // The scrolling is iced's: given a fixed height smaller than the
        // content it keeps the caret inside the visible line range, which is
        // exactly what following the caret by keyboard needs. Sized to the
        // content — as this was — there is nothing to scroll and the overflow
        // simply draws outside the card.
        let inner = (rect.height - text_raster::NODE_PAD_Y * zoom)
            .max(FONT_SIZE * zoom * text_raster::LINE_HEIGHT);
        let height = (text_height + 2.0).min(inner);
        // Match read mode's alignment while editing: a centered block that hugs
        // the text for simple content, or the full (left-aligned) wrap column
        // when the read view left-aligns it.
        let centered = self.config.center_node_text && markdown::is_simple(&text);
        let width = if centered {
            (text_width + caret_slack).clamp(MIN_EDITOR_WIDTH * zoom, wrap * zoom + caret_slack)
        } else {
            (text_wrap * zoom).min(rect.width)
        };
        Some(EditorBox {
            origin: Point::new(
                (rect.x + (rect.width - width) / 2.0).max(0.0),
                (rect.y + (rect.height - height) / 2.0).max(0.0),
            ),
            width,
            height,
            centered,
            font_size: FONT_SIZE,
        })
    }

    /// Screen-space top-left of the editing overlay's text for `index` — the
    /// origin a click maps against for deferred caret placement.
    fn editor_text_origin(&self, index: usize) -> Option<Point> {
        self.editor_box(index).map(|b| b.origin)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Return at the end of `line`.
    fn ret(line: &str) -> ReturnKey {
        on_return(line, line.len())
    }

    fn cont(prefix: &str) -> ReturnKey {
        ReturnKey::Continue(prefix.to_owned())
    }

    fn end(line: &str) -> ReturnKey {
        ReturnKey::End(line.to_owned())
    }

    #[test]
    fn return_continues_a_list_with_its_marker() {
        assert_eq!(ret("- passport"), cont("- "));
        assert_eq!(ret("* passport"), cont("* "));
        assert_eq!(ret("+ passport"), cont("+ "));
        assert_eq!(ret("\t- nested"), cont("\t- "));
        assert_eq!(ret("  - nested"), cont("  - "));
    }

    #[test]
    fn an_ordered_list_counts_on_and_a_task_starts_unticked() {
        assert_eq!(ret("1. first"), cont("2. "));
        assert_eq!(ret("9) ninth"), cont("10) "));
        assert_eq!(ret("- [x] done"), cont("- [ ] "));
        assert_eq!(ret("- [ ] todo"), cont("- [ ] "));
    }

    #[test]
    fn a_quote_carries_on_and_so_does_a_list_inside_it() {
        assert_eq!(ret("> said"), cont("> "));
        assert_eq!(ret("> > nested"), cont("> > "));
        assert_eq!(ret("> - item"), cont("> - "));
    }

    #[test]
    fn return_on_an_empty_item_ends_or_outdents_it() {
        // Top level: the marker goes, and with it the list.
        assert_eq!(ret("- "), end(""));
        assert_eq!(ret("1. "), end(""));
        assert_eq!(ret("- [ ] "), end(""));
        assert_eq!(ret("> "), end(""));
        // Nested: one level out, keeping its marker.
        assert_eq!(ret("\t- "), end("- "));
        assert_eq!(ret("\t\t- "), end("\t- "));
        assert_eq!(ret("    - "), end("- "));
        assert_eq!(ret("  2. "), end("2. "));
        // Inside a quote: the list ends, the quote stays.
        assert_eq!(ret("> - "), end("> "));
    }

    #[test]
    fn plain_lines_and_lookalikes_break_plainly() {
        assert_eq!(ret("Packing list"), ReturnKey::Plain);
        assert_eq!(ret("**bold** start"), ReturnKey::Plain);
        assert_eq!(ret("---"), ReturnKey::Plain);
        assert_eq!(ret("-dash"), ReturnKey::Plain);
        assert_eq!(ret("2024. was a year"), cont("2025. "), "a number then `. ` is a list item, as in Markdown");
        assert_eq!(ret("12345678901. too long"), ReturnKey::Plain);
        // The caret inside the marker: a plain break, which moves the item down.
        assert_eq!(on_return("- passport", 1), ReturnKey::Plain);
        // The caret mid-item still continues: the rest moves to the new item.
        assert_eq!(on_return("- pass port", 6), cont("- "));
    }

    /// `<sel>` marks the selection range; returns the new text with the
    /// re-selected range marked the same way, or `"<none>"` for a no-op.
    fn toggle(marked: &str, marker: &str) -> String {
        let start = marked.find('<').unwrap();
        // `>` offset in `marked`, minus 1 for the removed `<` before it.
        let end = marked[start + 1..].find('>').unwrap() + start;
        let full = marked.replacen('<', "", 1).replacen('>', "", 1);
        match toggle_wrap(&full, start, end, marker) {
            Some((new_full, s, e)) => {
                let mut out = new_full;
                out.insert(e, '>');
                out.insert(s, '<');
                out
            }
            None => "<none>".to_owned(),
        }
    }

    #[test]
    fn bold_wraps_the_selection() {
        assert_eq!(toggle("<word>", "**"), "<**word**>");
    }

    #[test]
    fn bold_excludes_leading_and_trailing_whitespace() {
        // Selecting `word1< word2>` bolds `word2`, not ` word2`.
        assert_eq!(toggle("word1< word2>", "**"), "word1 <**word2**>");
        assert_eq!(toggle("<word1 >word2", "**"), "<**word1**> word2");
    }

    #[test]
    fn bold_toggles_off_when_markers_are_inside_the_selection() {
        assert_eq!(toggle("<**word**>", "**"), "<word>");
    }

    #[test]
    fn bold_toggles_off_when_markers_are_outside_the_selection() {
        // A double-clicked word selects the text but not the surrounding `**`.
        assert_eq!(toggle("**<word>**", "**"), "<word>");
    }

    #[test]
    fn italic_does_not_strip_bold_markers() {
        // `*` must not match one star of a `**` pair.
        assert_eq!(toggle("<**word**>", "*"), "<***word***>");
    }

    #[test]
    fn whitespace_only_selection_is_a_noop() {
        assert_eq!(toggle("a<   >b", "**"), "<none>");
    }

    #[test]
    fn offsets_and_positions_are_inverses() {
        let text = "one\ntwo\nthree";
        for offset in [0, 3, 4, 7, 8, 13] {
            let position = position_at(text, offset);
            assert_eq!(offset_of(text, position), offset, "round-trip at {offset}");
        }
    }
}
