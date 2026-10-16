//! In-board text find (Cmd/Ctrl+F): the match set, the current hit, and the
//! camera fly-to that brings it into view. The bar itself is drawn in
//! [`super::view`].

use iced::{Task, Vector, window};

use super::arrange::node_world_rect;
use super::{App, CameraFocus, FONT_SIZE, Message};
use crate::canvas::{board, text_raster};

pub(super) use thinkboard_core::search::{Match, find_matches};

/// An in-board text find. `matches` is every occurrence across the text nodes,
/// in board order; `current` indexes it.
pub(super) struct Search {
    pub(super) query: String,
    pub(super) matches: Vec<Match>,
    /// The cards holding a match, sorted: what the board keeps at full
    /// strength while the rest dim. Kept with `matches` rather than derived
    /// from them each frame — a short query can match thousands of times.
    pub(super) cards: Vec<usize>,
    pub(super) current: usize,
}

impl Search {
    pub(super) fn new(query: String, matches: Vec<Match>) -> Search {
        let cards = cards_of(&matches);
        Search { query, matches, cards, current: 0 }
    }

    /// Takes a fresh set of matches, keeping the current one where it can.
    pub(super) fn set_matches(&mut self, matches: Vec<Match>) {
        self.cards = cards_of(&matches);
        self.current = self.current.min(matches.len().saturating_sub(1));
        self.matches = matches;
    }
}

fn cards_of(matches: &[Match]) -> Vec<usize> {
    let mut cards: Vec<usize> = matches.iter().map(|m| m.node).collect();
    cards.sort_unstable();
    cards.dedup();
    cards
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
        self.search.as_ref().is_some_and(|search| search.cards.binary_search(&index).is_ok())
    }

    /// Flies the camera to the current search match: its card, centred on the
    /// line the hit is on. Unlike `focus_on_node`, it centers even when already
    /// zoomed in (only zooming in if the view is currently too far out to
    /// read).
    pub(super) fn focus_current_match(&mut self) -> Task<Message> {
        let Some((index, ordinal)) = self.search.as_ref().and_then(|s| {
            let current = s.matches.get(s.current)?;
            // Counted in the card's reading order, as the board counts them.
            let ordinal = s
                .matches
                .iter()
                .filter(|m| m.node == current.node)
                .position(|m| m.start == current.start)?;
            Some((current.node, ordinal))
        }) else {
            return Task::none();
        };
        let line = self.scroll_to_hit(index, ordinal);
        if let Some(node) = self.nodes.get(index) {
            let y = line.unwrap_or(node.y + node.height / 2.0);
            let center = Vector::new(node.x + node.width / 2.0, y);
            let zoom = self.camera.zoom.max(self.config.auto_zoom_level);
            self.camera_focus = Some(CameraFocus {
                target_zoom: zoom,
                target_offset: center * -zoom,
                last_tick: std::time::Instant::now(),
            });
        }
        Task::none()
    }

    /// Scrolls a card that scrolls so its `ordinal`-th hit sits a third of
    /// the way down it, and returns the world y of the hit's line as it now
    /// is. Centring on the card wasn't enough: a hit in a long note's overflow
    /// was counted, highlighted, and out of sight below the card's edge.
    fn scroll_to_hit(&mut self, index: usize, ordinal: usize) -> Option<f32> {
        let query = self.search.as_ref()?.query.clone();
        let node = self.nodes.get(index)?;
        let text = node.text.as_deref()?;
        let scrolls = board::scrolls(&self.config, node);
        // Laid out as it's drawn, from the top: how far down is the hit?
        let frame = board::text_frame(node_world_rect(node), scrolls.then_some(0.0), 1.0);
        let wrap = text_raster::card_text_wrap(node.width);
        let (top, height) = text_raster::with_markdown_layout(
            text,
            frame.width,
            frame.height,
            wrap,
            FONT_SIZE,
            self.theme,
            self.config.center_node_text,
            |runs| {
                runs.iter()
                    .flat_map(|run| {
                        let line = (run.y, run.size * text_raster::LINE_HEIGHT);
                        board::hit_ranges(&run.text, &query).into_iter().map(move |_| line)
                    })
                    .nth(ordinal)
            },
        )?;
        let mut y = frame.y + top;
        if scrolls {
            let overflow = (board::content_height(text, node.width, &self.theme) - node.height).max(0.0);
            let scroll = (y - node.y - node.height / 3.0).clamp(0.0, overflow);
            let id = node.id.clone();
            self.node_scroll.insert(id, scroll);
            self.cache.clear();
            y -= scroll;
        }
        Some(y + height / 2.0)
    }
}

#[cfg(test)]
mod tests {
    use super::{Match, Search};

    fn hit(node: usize) -> Match {
        Match { node, start: 0, len: 1 }
    }

    #[test]
    fn the_matched_cards_are_kept_once_each_in_order() {
        let mut search = Search::new("a".to_owned(), vec![hit(3), hit(1), hit(3)]);
        assert_eq!(search.cards, vec![1, 3]);
        search.current = 2;
        // Fewer matches than the current one: it stays on the last.
        search.set_matches(vec![hit(5)]);
        assert_eq!(search.cards, vec![5]);
        assert_eq!(search.current, 0);
    }
}
