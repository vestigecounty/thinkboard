//! In-board text find (Cmd/Ctrl+F): the match set, the current hit, and the
//! camera fly-to that brings it into view. The bar itself is drawn in
//! [`super::view`].

use iced::{Task, Vector, window};

use super::{App, CameraFocus, Message};

pub(super) use thinkboard_core::search::{Match, find_matches};

/// An in-board text find. `matches` is every occurrence across the text nodes,
/// in board order; `current` indexes it.
pub(super) struct Search {
    pub(super) query: String,
    pub(super) matches: Vec<Match>,
    pub(super) current: usize,
}

/// Find-bar navigation keys, subscribed while the search is open: Enter →
/// next, Shift+Enter → previous, Esc → close. (The text input keeps typing; a
/// raw subscription handles these reliably regardless of focus, and doing them
/// all here avoids double-firing with `on_submit`.)
pub(super) fn key_event(
    event: iced::Event,
    _status: iced::event::Status,
    _window: window::Id,
) -> Option<Message> {
    use iced::keyboard::key::{Key, Named};

    let iced::Event::Keyboard(iced::keyboard::Event::KeyPressed { key, modifiers, .. }) = event
    else {
        return None;
    };
    match key {
        Key::Named(Named::Enter) if modifiers.shift() => Some(Message::SearchPrev),
        Key::Named(Named::Enter) => Some(Message::SearchNext),
        Key::Named(Named::Escape) => Some(Message::CloseSearch),
        _ => None,
    }
}

impl App {
    /// Whether a node currently holds a search match (drawn live + highlighted
    /// while the find bar is open, so it isn't rasterized over the highlight).
    pub(super) fn is_search_match(&self, index: usize) -> bool {
        self.search
            .as_ref()
            .is_some_and(|search| search.matches.iter().any(|m| m.node == index))
    }

    /// Flies the camera to center the current search match's node. Unlike
    /// `focus_on_node`, it centers even when already zoomed in (only zooming in
    /// if the view is currently too far out to read).
    pub(super) fn focus_current_match(&mut self) -> Task<Message> {
        let Some(index) = self
            .search
            .as_ref()
            .and_then(|s| s.matches.get(s.current))
            .map(|m| m.node)
        else {
            return Task::none();
        };
        if let Some(node) = self.nodes.get(index) {
            let center = Vector::new(node.x + node.width / 2.0, node.y + node.height / 2.0);
            let zoom = self.camera.zoom.max(self.config.auto_zoom_level);
            self.camera_focus = Some(CameraFocus {
                target_zoom: zoom,
                target_offset: center * -zoom,
                last_tick: std::time::Instant::now(),
            });
        }
        Task::none()
    }
}
