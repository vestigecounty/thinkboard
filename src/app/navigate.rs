//! The board by keyboard: moving the selection from card to card, editing the
//! selected one, adding a linked card beside it, nudging, and fitting the
//! board to the view.
//!
//! Before this, nothing selected a card without the mouse — so everything
//! that acts on a selection (delete, group, ask, colour, copy) was out of a
//! keyboard's reach, and the palette listed them greyed out with "Select a
//! card first". The arrows go spatially (`thinkboard_core::navigate`): a board
//! has no reading order to tab through.
//!
//! **Esc is not part of it.** It ends an edit and closes what's on top, and
//! otherwise puts the overlay away as it always has; deselecting first would
//! make dismissing the board a two-key job.

use std::time::{Duration, Instant};

use iced::widget::text_editor;
use iced::{Point, Task};
use thinkboard_core::navigate::{self, Toward};

use super::arrange::node_world_rect;
use super::{App, CameraFocus, Message};

/// How far ⌥+arrow moves a card when there's no grid to step by, in world px.
const NUDGE: f32 = 10.0;
/// How long a run of nudges is one undo step: a key held down, or tapped a few
/// times to line something up, is one move.
const NUDGE_RUN: Duration = Duration::from_secs(1);
/// The gap Tab leaves between a card and the card it adds beside it.
const LINKED_GAP: f32 = 80.0;

impl App {
    /// Moves the selection to the nearest card `toward` — or, with `extend`,
    /// adds that card to it. With nothing selected, starts from the card
    /// nearest the middle of the view. Frames are passed over: they're
    /// containers, chosen by their header.
    pub(super) fn select_toward(&mut self, toward: Toward, extend: bool) {
        let rects: Vec<_> = self.nodes.iter().map(|n| (n.x, n.y, n.width, n.height)).collect();
        let frame = |i: usize| self.nodes[i].is_group();
        let next = match self.selected.last().copied().filter(|&i| i < self.nodes.len()) {
            Some(from) => navigate::nearest(rects[from], toward, &rects, |i| {
                i == from || frame(i) || (extend && self.selected.contains(&i))
            }),
            None => {
                let middle = self.camera.to_world(Point::ORIGIN + self.camera.center());
                let cards: Vec<usize> = (0..self.nodes.len()).filter(|&i| !frame(i)).collect();
                let card_rects: Vec<_> = cards.iter().map(|&i| rects[i]).collect();
                navigate::nearest_to((middle.x, middle.y), &card_rects).map(|at| cards[at])
            }
        };
        let Some(next) = next else {
            return;
        };
        if !extend {
            self.selected.clear();
        }
        if !self.selected.contains(&next) {
            self.selected.push(next);
        }
        self.selected_edge = None;
        self.reveal(node_world_rect(&self.nodes[next]));
        self.cache.clear();
    }

    /// Edits the selected card, with the caret at the end — there's no click
    /// to place it, and the end is where you'd go on writing.
    pub(super) fn edit_selected(&mut self) -> Task<Message> {
        let Some(&index) = self.selected.last() else {
            return Task::none();
        };
        let task = self.begin_editing(index, None);
        if let Some(editing) = &mut self.editing {
            editing.content.perform(text_editor::Action::Move(text_editor::Motion::DocumentEnd));
        }
        task
    }

    /// Adds a card linked from the selected card, to its right — where a drag
    /// off its right port would put one — and opens its editor. Tab again
    /// after Esc adds the next, as a mind map does.
    pub(super) fn add_linked_node(&mut self) {
        let Some(&from) = self.selected.last() else {
            return;
        };
        let Some(node) = self.nodes.get(from) else {
            return;
        };
        let world = Point::new(node.x + node.width + LINKED_GAP, node.y + node.height / 2.0);
        self.create_connected_node(from, "right", world);
        let index = self.nodes.len() - 1;
        self.selected = vec![index];
        self.selected_edge = None;
        self.reveal(node_world_rect(&self.nodes[index]));
        self.cache.clear();
    }

    /// Moves the selected cards a step `toward` — a grid cell where snapping
    /// is on, so they stay on it. A run of nudges is one undo step.
    pub(super) fn nudge_selection(&mut self, toward: Toward) {
        let Some(&first) = self.selected.first() else {
            return;
        };
        let continuing = self.last_nudge.is_some_and(|at| at.elapsed() < NUDGE_RUN);
        if !continuing {
            self.push_undo();
        }
        self.last_nudge = Some(Instant::now());
        let grid = self.snap_grid();
        let step = if grid > 0.0 { grid } else { NUDGE };
        let (dx, dy) = toward.step();
        for i in self.drag_riders(first) {
            if let Some(node) = self.nodes.get_mut(i) {
                node.x += dx * step;
                node.y += dy * step;
            }
        }
        self.reveal(node_world_rect(&self.nodes[first]));
        self.mark_dirty();
        self.cache.clear();
    }

    /// Flies the camera to show the whole board, as opening it does.
    pub(super) fn fit_board(&mut self) {
        let mut fitted = self.camera;
        fitted.fit(self.content_bounds());
        self.inertia = None;
        self.camera_focus = Some(CameraFocus {
            target_zoom: fitted.zoom,
            target_offset: fitted.offset,
            last_tick: Instant::now(),
        });
    }

    /// Ends the edit from the keyboard — Esc, or ⌘Enter — leaving the card
    /// selected, so the next key acts on what was just written: Enter to go
    /// back in, Tab to add the next card, an arrow to move on. A click
    /// elsewhere ends it too, and selects whatever it lands on instead.
    pub(super) fn finish_editing(&mut self) -> Task<Message> {
        let id = self
            .editing
            .as_ref()
            .and_then(|editing| self.nodes.get(editing.index))
            .map(|node| node.id.clone());
        let task = self.stop_editing();
        // Gone, if it was a new card left empty.
        if let Some(index) = id.and_then(|id| self.nodes.iter().position(|node| node.id == id)) {
            self.selected = vec![index];
            self.selected_edge = None;
            self.cache.clear();
        }
        task
    }
}
