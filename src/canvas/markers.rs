//! Where the board's cards are, when none of them is on screen: a dot on the
//! window's edge toward each group of them — the groups semantic zoom draws —
//! the nearer ones larger and more solid, and the nearest named.
//!
//! It replaces a single bar along whichever edge faced the nearest card, which
//! said *that way* and nothing else: not what was there, nor that anything
//! was anywhere else.
//!
//! Only the placing lives here, in screen space and apart from the drawing, so
//! it can be tested: which edge a group's dot lands on, how big and how solid
//! it is, which are dropped for sitting on a nearer one, and where a label
//! goes so that it stays in the window.

use iced::{Point, Rectangle, Size};

/// How far in from the window's edge a dot's centre sits.
pub const INSET: f32 = 16.0;
/// The nearest group's dot, and the ring of accent around it.
pub const NEAREST_RADIUS: f32 = 6.0;
pub const HALO: f32 = 4.0;
/// Between a dot's ring and its label.
pub const LABEL_GAP: f32 = 8.0;
/// A label's distance from the window's edge, at the closest it's let come.
const LABEL_MARGIN: f32 = 8.0;
/// A dot this close to a nearer group's is dropped: two click targets' width
/// (the board's is 12 px about a dot), so no two overlap — and a run of dots
/// any closer read as a dotted line rather than as groups.
const MIN_GAP: f32 = 24.0;
/// At most this many. Past it the edge is a dotted line that says nothing,
/// and the nearest — the ones worth going to — are already among them.
const MOST: usize = 16;
/// How long a name gets before it's cut short.
const TITLE_CHARS: usize = 40;

/// The window edge a dot sits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Top,
    Right,
    Bottom,
    Left,
}

/// One group's dot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Marker {
    /// Which group it stands for, by its index in what was placed.
    pub group: usize,
    pub at: Point,
    pub side: Side,
    pub radius: f32,
    /// How solid, before the board's own fade.
    pub alpha: f32,
    /// The nearest group: the largest dot, ringed, and named.
    pub nearest: bool,
}

/// A dot on the edges of `view` (screen px) for each of `groups`: a screen
/// point the group lies toward, and how far it is (in any unit, as long as
/// it's the same for all — only the ratios count). Nearest first.
pub fn place(view: Rectangle, groups: &[(Point, f32)]) -> Vec<Marker> {
    let inner = Rectangle::new(
        Point::new(view.x + INSET, view.y + INSET),
        Size::new((view.width - 2.0 * INSET).max(0.0), (view.height - 2.0 * INSET).max(0.0)),
    );
    let center = view.center();
    let mut order: Vec<usize> = (0..groups.len()).collect();
    order.sort_by(|&a, &b| groups[a].1.total_cmp(&groups[b].1));
    let Some(&first) = order.first() else {
        return Vec::new();
    };
    let near = groups[first].1.max(f32::EPSILON);

    let mut placed: Vec<Marker> = Vec::new();
    for group in order {
        if placed.len() == MOST {
            break;
        }
        let (toward, distance) = groups[group];
        let Some((at, side)) = edge_point(inner, center, toward) else {
            continue;
        };
        if placed.iter().any(|marker| marker.at.distance(at) < MIN_GAP) {
            continue;
        }
        let nearest = placed.is_empty();
        // 1 for as near as the nearest, toward 0 for ever farther.
        let closeness = (near / distance.max(near)).clamp(0.0, 1.0);
        let (radius, alpha) = if nearest { (NEAREST_RADIUS, 1.0) } else { (3.5 + closeness, 0.3 + 0.35 * closeness) };
        placed.push(Marker { group, at, side, radius, alpha, nearest });
    }
    placed
}

/// Where the line from `center` toward `toward` crosses the edge of `inner`,
/// and which edge it crosses. `None` when there's no line: `toward` is the
/// centre itself.
fn edge_point(inner: Rectangle, center: Point, toward: Point) -> Option<(Point, Side)> {
    let (dx, dy) = (toward.x - center.x, toward.y - center.y);
    // How far along the line each edge it heads for is; the nearest is crossed
    // first. Worked per edge rather than from the half-size, because the top
    // is set lower than the rest to clear the notch, so `center` isn't the
    // middle of `inner`.
    let mut crossings = Vec::with_capacity(2);
    if dx > 0.0 {
        crossings.push(((inner.x + inner.width - center.x) / dx, Side::Right));
    } else if dx < 0.0 {
        crossings.push(((inner.x - center.x) / dx, Side::Left));
    }
    if dy > 0.0 {
        crossings.push(((inner.y + inner.height - center.y) / dy, Side::Bottom));
    } else if dy < 0.0 {
        crossings.push(((inner.y - center.y) / dy, Side::Top));
    }
    let (along, side) = crossings.into_iter().min_by(|a, b| a.0.total_cmp(&b.0))?;
    let along = along.max(0.0);
    Some((Point::new(center.x + dx * along, center.y + dy * along), side))
}

/// Where a label of `size` goes for `marker`: inward of its dot, centred on
/// it along the edge, and moved along as far as it takes to stay in `view`.
pub fn label_box(view: Rectangle, marker: &Marker, size: Size) -> Rectangle {
    let reach = marker.radius + HALO + LABEL_GAP;
    let at = marker.at;
    let (x, y) = match marker.side {
        Side::Top => (at.x - size.width / 2.0, at.y + reach),
        Side::Bottom => (at.x - size.width / 2.0, at.y - reach - size.height),
        Side::Left => (at.x + reach, at.y - size.height / 2.0),
        Side::Right => (at.x - reach - size.width, at.y - size.height / 2.0),
    };
    let clamp = |value: f32, low: f32, high: f32| if high < low { low } else { value.clamp(low, high) };
    Rectangle::new(
        Point::new(
            clamp(x, view.x + LABEL_MARGIN, view.x + view.width - LABEL_MARGIN - size.width),
            clamp(y, view.y + LABEL_MARGIN, view.y + view.height - LABEL_MARGIN - size.height),
        ),
        size,
    )
}

/// What a group is called on its marker: its main card's first line as it
/// reads — the Markdown's markers gone, the list bullet too — cut short.
pub fn title(markdown: &str) -> String {
    let mut line = String::new();
    'spans: for span in thinkboard_core::markdown::parse(markdown) {
        for ch in span.text.chars() {
            if ch == '\n' {
                if line.trim().is_empty() {
                    line.clear();
                    continue;
                }
                break 'spans;
            }
            line.push(ch);
        }
    }
    let line = line.trim();
    let line = ["• ", "□ ", "☑ "].iter().find_map(|bullet| line.strip_prefix(bullet)).unwrap_or(line).trim();
    if line.chars().count() <= TITLE_CHARS {
        return line.to_owned();
    }
    let cut: String = line.chars().take(TITLE_CHARS - 1).collect();
    format!("{}…", cut.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view() -> Rectangle {
        Rectangle::new(Point::ORIGIN, Size::new(800.0, 600.0))
    }

    #[test]
    fn a_dot_sits_on_the_edge_its_group_lies_beyond() {
        let markers = place(view(), &[(Point::new(5000.0, 300.0), 1.0), (Point::new(400.0, -900.0), 2.0)]);
        assert_eq!(markers[0].side, Side::Right);
        assert_eq!(markers[0].at, Point::new(800.0 - INSET, 300.0));
        assert_eq!(markers[1].side, Side::Top);
        assert_eq!(markers[1].at, Point::new(400.0, INSET));
        // A diagonal crosses whichever edge it reaches first.
        let corner = place(view(), &[(Point::new(5000.0, 2000.0), 1.0)]);
        assert_eq!(corner[0].side, Side::Right);
        assert!(corner[0].at.y > 300.0 && corner[0].at.y < 600.0 - INSET);
        let corner = place(view(), &[(Point::new(2000.0, 5000.0), 1.0)]);
        assert_eq!(corner[0].side, Side::Bottom);
    }

    #[test]
    fn the_nearest_is_the_largest_and_the_rest_fade_with_distance() {
        let markers = place(
            view(),
            &[(Point::new(-900.0, 300.0), 3.0), (Point::new(400.0, 2000.0), 1.0), (Point::new(400.0, -2000.0), 2.0)],
        );
        // Nearest first, whatever order they were given in.
        let groups: Vec<usize> = markers.iter().map(|marker| marker.group).collect();
        assert_eq!(groups, [1, 2, 0]);
        assert!(markers[0].nearest && !markers[1].nearest);
        assert_eq!(markers[0].radius, NEAREST_RADIUS);
        assert!(markers[1].radius > markers[2].radius);
        assert!(markers[1].alpha > markers[2].alpha);
        assert!(markers[1].radius < NEAREST_RADIUS, "nothing else as large as the nearest");
    }

    #[test]
    fn a_farther_group_behind_a_nearer_one_gets_no_dot_of_its_own() {
        // Both straight to the right: one dot, the nearer's.
        let markers = place(view(), &[(Point::new(3000.0, 300.0), 2.0), (Point::new(1500.0, 302.0), 1.0)]);
        assert_eq!(markers.len(), 1);
        assert_eq!(markers[0].group, 1);
    }

    #[test]
    fn a_label_stays_in_the_window() {
        // A dot near the bottom-left corner: its label goes above it, and is
        // moved right rather than cut off by the window's left edge.
        let marker = Marker { group: 0, at: Point::new(20.0, 584.0), side: Side::Bottom, radius: 6.0, alpha: 1.0, nearest: true };
        let label = label_box(view(), &marker, Size::new(120.0, 16.0));
        assert!(label.y + label.height < marker.at.y - marker.radius);
        assert_eq!(label.x, LABEL_MARGIN);
    }

    #[test]
    fn a_title_is_the_first_line_as_it_reads() {
        assert_eq!(title("# Marketing **plan**\nthe rest"), "Marketing plan");
        assert_eq!(title("\n\n- [ ] Book flights"), "Book flights");
        assert_eq!(title("See [the docs](https://example.com)"), "See the docs");
        let long = title(&"word ".repeat(20));
        assert!(long.ends_with('…') && long.chars().count() <= TITLE_CHARS);
    }
}
