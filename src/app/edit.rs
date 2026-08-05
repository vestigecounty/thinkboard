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

use super::arrange::node_world_rect;
use super::{App, CameraFocus, FONT_SIZE, Message};
use crate::canvas::{markdown, text_raster};

/// Edits within this window coalesce into one undo step.
const EDIT_COALESCE: Duration = Duration::from_millis(600);

pub(super) struct Editing {
    pub(super) index: usize,
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
    fn new(index: usize, text: &str) -> Self {
        Editing {
            index,
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
        if !node.is_text() {
            return Task::none();
        }
        // Capture a pre-edit snapshot to make the edit undoable — but not for
        // a brand-new node (its creation isn't an undoable edit).
        self.edit_undo = (self.fresh_node != Some(index)).then(|| self.snapshot());
        self.rasters.invalidate(index);
        let text = node.text.as_deref().unwrap_or("").trim().to_owned();
        let mut editing = Editing::new(index, &text);
        // Defer caret placement to the first frame (see `pending_caret`); a
        // `window::frames()` subscription then fires `PlaceEditorCaret`.
        editing.pending_caret = caret_at;
        self.editing = Some(editing);
        self.cache.clear();
        // With scrollbars on, editing a node re-grows it to fit (its shrink was
        // temporary): drop its scroll and autosize before anything else, so the
        // editor lays out over the full content rather than a scrolled window.
        if self.config.node_scrollbars {
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
        // Id of the node being edited, before any removal below.
        let edited_id = self
            .editing
            .as_ref()
            .and_then(|editing| self.nodes.get(editing.index))
            .map(|node| node.id.clone());
        // A freshly-added connected node worth asking Copilot about.
        let mut completion_anchor: Option<String> = None;
        // A drag-created node left without text disappears again.
        if let Some(editing) = &self.editing {
            let index = editing.index;
            let empty = self
                .nodes
                .get(index)
                .is_some_and(|node| node.text.as_deref().unwrap_or("").trim().is_empty());
            if self.fresh_node == Some(index) && empty {
                // Fresh nodes are always last, so removal does not shift the
                // indices keying the snapshot maps.
                let id = self.nodes.remove(index).id;
                self.edges
                    .retain(|edge| edge.from_node != id && edge.to_node != id);
                self.rasters.invalidate(index);
                self.spawning.remove(&index);
            } else {
                // Editing done: drop the whitespace that only mattered while
                // typing, and settle the size.
                if let Some(node) = self.nodes.get_mut(index)
                    && let Some(text) = &node.text
                {
                    let trimmed = text.trim();
                    if trimmed != text {
                        node.text = Some(trimmed.to_owned());
                    }
                }
                // Editing done: fit the card to the rendered Markdown.
                self.autosize_node(index, true);
                // A fresh node with a parent → ask for sibling ideas.
                completion_anchor = self.completion_anchor(index);
            }
        }
        // Commit the pre-edit snapshot as an undo point only if the edited
        // node's text actually changed.
        if let (Some(snapshot), Some(id)) = (self.edit_undo.take(), edited_id) {
            let old = snapshot
                .nodes
                .iter()
                .find(|n| n.id == id)
                .and_then(|n| n.text.clone());
            let new = self.nodes.iter().find(|n| n.id == id).and_then(|n| n.text.clone());
            if old != new {
                self.history.record(snapshot);
                // The card resized; re-lay-out its graph if enabled (covered by
                // the undo snapshot just recorded).
                if let Some(index) = self.nodes.iter().position(|n| n.id == id) {
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

    /// Writes the editor's text back onto its node and refits the card.
    fn commit_editor_text(&mut self, edited: Option<(usize, String)>) {
        let Some((index, text)) = edited else {
            return;
        };
        if let Some(node) = self.nodes.get_mut(index) {
            node.text = Some(text);
        }
        self.autosize_node(index, false);
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
        let Some(node) = self.nodes.get(index) else {
            return false;
        };
        let rect = self.camera.rect_to_screen(node_world_rect(node));
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
        let Some(node) = self.nodes.get(index) else {
            return;
        };
        let vp = self.camera.viewport;
        let center = Vector::new(node.x + node.width / 2.0, node.y + node.height / 2.0);
        // Largest zoom at which the node fits with margin; never zoom in past the
        // current level — only pull back when it's too big to fit.
        let fit = ((vp.width - 2.0 * MARGIN) / node.width)
            .min((vp.height - 2.0 * MARGIN) / node.height);
        let target_zoom =
            self.camera.zoom.min(fit).clamp(self.config.min_zoom, self.config.max_zoom);
        self.camera_focus = Some(CameraFocus {
            target_zoom,
            target_offset: center * -target_zoom,
            last_tick: Instant::now(),
        });
    }

    /// The editing overlay's box for `index`: its screen-space top-left, width
    /// and height. `view` lays the editor out from this, and deferred caret
    /// placement maps a click against its origin — so both stay in sync.
    pub(super) fn editor_box(&self, index: usize) -> Option<(Point, f32, f32, bool)> {
        let node = self.nodes.get(index)?;
        let editing = self.editing.as_ref()?;
        let rect = self.camera.rect_to_screen(node_world_rect(node));
        let zoom = self.camera.zoom;
        let wrap = text_raster::card_text_wrap(node.width);
        // Measure/size the text at the editor's inner column (inset a side like
        // read mode), not the full card wrap, or a wrapping note is a line short
        // and the box mis-centers.
        let text_wrap = text_raster::editor_text_wrap(node.width);
        let text = editing.content.text();
        let (text_width, text_height) =
            text_raster::measure_source(&text, text_wrap, FONT_SIZE, zoom, &self.theme);
        let caret_slack = 4.0;
        let height = text_height + 2.0;
        // Match read mode's alignment while editing: a centered block that hugs
        // the text for simple content, or the full (left-aligned) wrap column
        // when the read view left-aligns it.
        let centered = self.config.center_node_text && markdown::is_simple(&text);
        let width = if centered {
            (text_width + caret_slack).clamp(24.0 * zoom, wrap * zoom + caret_slack)
        } else {
            (text_wrap * zoom).min(rect.width)
        };
        let left = (rect.x + (rect.width - width) / 2.0).max(0.0);
        let top = (rect.y + (rect.height - height) / 2.0).max(0.0);
        Some((Point::new(left, top), width, height, centered))
    }

    /// Screen-space top-left of the editing overlay's text for `index` — the
    /// origin a click maps against for deferred caret placement.
    fn editor_text_origin(&self, index: usize) -> Option<Point> {
        self.editor_box(index).map(|(origin, ..)| origin)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
