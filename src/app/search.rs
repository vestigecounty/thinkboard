//! In-board text find (Cmd/Ctrl+F): the match set, the current hit, and the
//! camera fly-to that brings it into view. The bar itself is drawn in
//! [`super::view`].

use iced::{Task, Vector, window};

use super::{App, CameraFocus, Message};
use crate::model::CanvasNode;

/// An in-board text find. `matches` is every occurrence across the text nodes,
/// in board order; `current` indexes it.
pub(super) struct Search {
    pub(super) query: String,
    pub(super) matches: Vec<Match>,
    pub(super) current: usize,
}

/// One search hit: a character range within a node's text.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Match {
    pub(super) node: usize,
    pub(super) start: usize,
    pub(super) len: usize,
}

/// Case-insensitive substring search over the text nodes, in board order. Ranges
/// are character offsets/lengths (for highlighting). Empty query → no matches.
pub(super) fn find_matches(nodes: &[CanvasNode], query: &str) -> Vec<Match> {
    // Fold to a single lowercase char per source char, keeping a 1:1 mapping so
    // the offsets stay valid indices into the original text.
    let fold = |c: char| c.to_lowercase().next().unwrap_or(c);
    let needle: Vec<char> = query.chars().map(fold).collect();
    if needle.is_empty() {
        return Vec::new();
    }

    let mut matches = Vec::new();
    for (node, canvas_node) in nodes.iter().enumerate() {
        if canvas_node.kind != "text" {
            continue;
        }
        let Some(text) = canvas_node.text.as_deref() else {
            continue;
        };
        let hay: Vec<char> = text.chars().map(fold).collect();
        let mut start = 0;
        while start + needle.len() <= hay.len() {
            if hay[start..start + needle.len()] == needle[..] {
                matches.push(Match { node, start, len: needle.len() });
                start += needle.len(); // non-overlapping
            } else {
                start += 1;
            }
        }
    }
    matches
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

#[cfg(test)]
mod tests {
    use super::*;

    fn node(text: &str) -> CanvasNode {
        CanvasNode {
            id: "n".to_owned(),
            kind: "text".to_owned(),
            text: Some(text.to_owned()),
            file: None,
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 100.0,
            color: None,
            extra: Default::default(),
        }
    }

    #[test]
    fn an_empty_query_matches_nothing() {
        assert!(find_matches(&[node("anything")], "").is_empty());
    }

    #[test]
    fn matching_ignores_case() {
        let matches = find_matches(&[node("Hello World")], "hello");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].start, 0);
        assert_eq!(matches[0].len, 5);
    }

    #[test]
    fn repeats_are_matched_without_overlapping() {
        // "aaaa" holds two non-overlapping "aa", not three overlapping ones.
        let matches = find_matches(&[node("aaaa")], "aa");
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].start, 0);
        assert_eq!(matches[1].start, 2);
    }

    #[test]
    fn offsets_are_char_indices_not_bytes() {
        // The highlight indexes chars, so a multi-byte prefix must not shift it.
        let matches = find_matches(&[node("héllo needle")], "needle");
        assert_eq!(matches[0].start, 6);
    }

    #[test]
    fn image_nodes_are_skipped() {
        let mut image = node("needle");
        image.kind = "file".to_owned();
        assert!(find_matches(&[image], "needle").is_empty());
    }
}
