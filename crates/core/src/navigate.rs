//! Moving a selection around a board by keyboard: from one card to the next
//! in a direction, as the eye would go.
//!
//! A board has no reading order to tab through — its cards are where someone
//! put them — so the arrow keys go spatially: → from a card is the card beside
//! it on the right. Geometry is bare `(x, y, width, height)` tuples, as the
//! rest of core takes it.

/// Which way an arrow key points.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Toward {
    Left,
    Right,
    Up,
    Down,
}

impl Toward {
    /// One step this way: `(dx, dy)`, a unit along the screen's axes (y down).
    pub fn step(self) -> (f32, f32) {
        match self {
            Toward::Left => (-1.0, 0.0),
            Toward::Right => (1.0, 0.0),
            Toward::Up => (0.0, -1.0),
            Toward::Down => (0.0, 1.0),
        }
    }
}

fn centre((x, y, width, height): (f32, f32, f32, f32)) -> (f32, f32) {
    (x + width / 2.0, y + height / 2.0)
}

/// The card nearest `from` in the direction `toward`, as an index into
/// `cards`, leaving out any `skip` says to (the card itself, or ones already
/// selected).
///
/// Measured between centres. Only a card whose centre lies beyond `from`'s in
/// that direction counts, and a card off to the side counts as twice as far as
/// one straight ahead — so → takes the card beside this one, not one further
/// down the board that happens to be a little nearer.
pub fn nearest(
    from: (f32, f32, f32, f32),
    toward: Toward,
    cards: &[(f32, f32, f32, f32)],
    skip: impl Fn(usize) -> bool,
) -> Option<usize> {
    const OFF_AXIS: f32 = 2.0;
    let (fx, fy) = centre(from);
    let (sx, sy) = toward.step();
    cards
        .iter()
        .enumerate()
        .filter(|(i, _)| !skip(*i))
        .filter_map(|(i, &card)| {
            let (cx, cy) = centre(card);
            let (dx, dy) = (cx - fx, cy - fy);
            // How far along the arrow, and how far off to its side.
            let along = dx * sx + dy * sy;
            let across = (dx * sy - dy * sx).abs();
            (along > 0.5).then_some((i, along + OFF_AXIS * across))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| i)
}

/// The card whose centre is nearest `point` — where an arrow key starts when
/// nothing is selected: the middle of the view.
pub fn nearest_to(point: (f32, f32), cards: &[(f32, f32, f32, f32)]) -> Option<usize> {
    cards
        .iter()
        .enumerate()
        .map(|(i, &card)| {
            let (cx, cy) = centre(card);
            (i, (cx - point.0).hypot(cy - point.1))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| i)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A plan: the trip on the left, two questions to its right, one above
    /// the other, and a note far below the trip.
    const CARDS: [(f32, f32, f32, f32); 4] = [
        (0.0, 100.0, 200.0, 100.0),   // 0: trip
        (400.0, 0.0, 200.0, 60.0),    // 1: eat, up and right
        (400.0, 260.0, 200.0, 60.0),  // 2: sintra, down and right
        (0.0, 600.0, 200.0, 60.0),    // 3: far below the trip
    ];

    fn none(_: usize) -> bool {
        false
    }

    #[test]
    fn an_arrow_goes_to_the_card_that_way() {
        assert_eq!(nearest(CARDS[0], Toward::Down, &CARDS, none), Some(3));
        assert_eq!(nearest(CARDS[1], Toward::Down, &CARDS, none), Some(2));
        assert_eq!(nearest(CARDS[2], Toward::Up, &CARDS, none), Some(1));
        assert_eq!(nearest(CARDS[1], Toward::Left, &CARDS, none), Some(0));
        // Nothing further left of the trip.
        assert_eq!(nearest(CARDS[0], Toward::Left, &CARDS, none), None);
    }

    #[test]
    fn straight_ahead_beats_nearer_but_off_to_the_side() {
        // → from the trip: both questions are to the right; the one closer to
        // level with it wins, though they're nearly the same distance away.
        assert_eq!(nearest(CARDS[0], Toward::Right, &CARDS, none), Some(1));
        // A card just to the right but far down loses to one level and further.
        let cards = [(0.0, 0.0, 100.0, 50.0), (150.0, 300.0, 100.0, 50.0), (400.0, 0.0, 100.0, 50.0)];
        assert_eq!(nearest(cards[0], Toward::Right, &cards, none), Some(2));
    }

    #[test]
    fn skipped_cards_are_passed_over() {
        // Extending a selection: the next card that way that isn't in it.
        assert_eq!(nearest(CARDS[0], Toward::Right, &CARDS, |i| i == 1), Some(2));
    }

    #[test]
    fn with_nothing_selected_it_starts_from_the_middle() {
        assert_eq!(nearest_to((500.0, 290.0), &CARDS), Some(2));
        assert_eq!(nearest_to((0.0, 0.0), &[]), None);
    }
}
