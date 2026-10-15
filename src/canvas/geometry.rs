//! Pure geometry for the board: the curved links between nodes, and how
//! overlapping cards stack.
//!
//! Split out from the board widget because both run per frame: link hover
//! ([`bezier_distance`] samples every edge's curve on every mouse move) and
//! [`stacking_planes`] (every rebuild of the scene), so their cost is worth
//! being able to measure (see `benches/render.rs`) and test on their own.

use iced::{Point, Rectangle};

/// A link's cubic Bézier, in screen space.
#[derive(Debug, Clone, Copy)]
pub struct EdgeGeometry {
    pub start: Point,
    pub control1: Point,
    pub control2: Point,
    pub end: Point,
    /// Carried along for the draw pass (fade-in, hover emphasis); it takes no
    /// part in the geometry itself.
    pub alpha: f32,
}

/// The point at `t` (0..=1) along the curve.
pub fn cubic_bezier(g: &EdgeGeometry, t: f32) -> Point {
    let u = 1.0 - t;
    let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
    Point::new(
        a * g.start.x + b * g.control1.x + c * g.control2.x + d * g.end.x,
        a * g.start.y + b * g.control1.y + c * g.control2.y + d * g.end.y,
    )
}

/// Distance from `p` to the segment `a`–`b`.
pub fn point_segment_distance(p: Point, a: Point, b: Point) -> f32 {
    let (abx, aby) = (b.x - a.x, b.y - a.y);
    let len2 = abx * abx + aby * aby;
    let t = if len2 > 0.0 {
        (((p.x - a.x) * abx + (p.y - a.y) * aby) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let (cx, cy) = (a.x + t * abx, a.y + t * aby);
    ((p.x - cx).powi(2) + (p.y - cy).powi(2)).sqrt()
}

/// How many straight segments the curve is approximated by when measuring
/// distance to it. The cost of a hover test is linear in this.
const SAMPLES: usize = 24;

/// Approximate distance from `point` to the curve, by sampling it into
/// [`SAMPLES`] segments and taking the nearest. Run per edge on every mouse
/// move, so it is deliberately allocation-free.
pub fn bezier_distance(point: Point, g: &EdgeGeometry) -> f32 {
    let mut previous = g.start;
    let mut min = f32::MAX;
    for i in 1..=SAMPLES {
        let current = cubic_bezier(g, i as f32 / SAMPLES as f32);
        min = min.min(point_segment_distance(point, previous, current));
        previous = current;
    }
    min
}

/// Whether `point` is close enough to the curve to be worth [`bezier_distance`].
///
/// A cubic Bézier is contained by the convex hull of its four control points,
/// so the box around them bounds the curve. Testing that box first turns a hover
/// test on a board full of links from "sample every edge" into "sample the
/// couple of edges actually near the cursor" — the rest are rejected on four
/// comparisons.
///
/// Conservative by construction: it can admit a curve that turns out to be
/// farther than `threshold` (the hull is larger than the curve), never reject
/// one that is within it. So it changes only the cost of a hover test, not
/// its result.
pub fn within_reach(point: Point, g: &EdgeGeometry, threshold: f32) -> bool {
    let min_x = g.start.x.min(g.end.x).min(g.control1.x).min(g.control2.x);
    let max_x = g.start.x.max(g.end.x).max(g.control1.x).max(g.control2.x);
    let min_y = g.start.y.min(g.end.y).min(g.control1.y).min(g.control2.y);
    let max_y = g.start.y.max(g.end.y).max(g.control1.y).max(g.control2.y);
    point.x >= min_x - threshold
        && point.x <= max_x + threshold
        && point.y >= min_y - threshold
        && point.y <= max_y + threshold
}

/// Which plane each card is drawn in, for `rects` in drawing order (later is
/// on top): 0 for a card that overlaps nothing drawn before it, else one above
/// the highest card under it — never past `top`.
///
/// A plane is a renderer layer, and only a later layer can put a card's fill
/// over another card's text (see `canvas::layered`). Most boards have no
/// overlaps and so one plane; a pile of cards needs as many as it is deep, and
/// past `top` the rest share the top one, where they bleed into each other as
/// every card used to.
pub fn stacking_planes(rects: &[Rectangle], top: usize) -> Vec<usize> {
    let mut under: Vec<Vec<usize>> = vec![Vec::new(); rects.len()];
    for (earlier, later) in overlapping_pairs(rects) {
        under[later].push(earlier);
    }
    let mut planes = vec![0; rects.len()];
    for i in 0..rects.len() {
        planes[i] = under[i].iter().map(|&j| planes[j] + 1).max().unwrap_or(0).min(top);
    }
    planes
}

/// Whether any of `over` overlaps any of `under`.
pub fn covers_any(over: &[Rectangle], under: &[Rectangle]) -> bool {
    if over.is_empty() || under.is_empty() {
        return false;
    }
    let all: Vec<Rectangle> = under.iter().chain(over).copied().collect();
    let is_over = |i: usize| i >= under.len();
    overlapping_pairs(&all).into_iter().any(|(a, b)| is_over(a) != is_over(b))
}

/// Every pair of `rects` that overlap, as `(earlier, later)` indices.
///
/// A sweep over left edges, so a board of cards side by side costs a sort and
/// a scan, not a comparison of every pair. Touching edges don't count: a card
/// placed flush against another isn't covering it.
fn overlapping_pairs(rects: &[Rectangle]) -> Vec<(usize, usize)> {
    const TOUCH: f32 = 0.5;
    let mut by_left: Vec<usize> = (0..rects.len()).collect();
    by_left.sort_by(|&a, &b| rects[a].x.total_cmp(&rects[b].x));
    let mut pairs = Vec::new();
    let mut open: Vec<usize> = Vec::new();
    for &i in &by_left {
        let r = rects[i];
        open.retain(|&j| rects[j].x + rects[j].width > r.x + TOUCH);
        for &j in &open {
            let o = rects[j];
            if o.y < r.y + r.height - TOUCH && r.y < o.y + o.height - TOUCH {
                pairs.push((i.min(j), i.max(j)));
            }
        }
        open.push(i);
    }
    pairs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(x: f32, y: f32) -> Rectangle {
        Rectangle::new(Point::new(x, y), iced::Size::new(100.0, 50.0))
    }

    #[test]
    fn cards_apart_share_one_plane() {
        let rects = [card(0.0, 0.0), card(200.0, 0.0), card(0.0, 100.0)];
        assert_eq!(stacking_planes(&rects, 3), vec![0, 0, 0]);
    }

    #[test]
    fn a_card_over_another_goes_one_plane_up() {
        // The second overlaps the first; the third overlaps only the second.
        let rects = [card(0.0, 0.0), card(50.0, 20.0), card(140.0, 40.0)];
        assert_eq!(stacking_planes(&rects, 3), vec![0, 1, 2]);
    }

    #[test]
    fn drawing_order_decides_who_is_on_top_not_position() {
        // The same overlap, listed the other way round: the later one lifts.
        let rects = [card(50.0, 20.0), card(0.0, 0.0)];
        assert_eq!(stacking_planes(&rects, 3), vec![0, 1]);
    }

    #[test]
    fn touching_is_not_overlapping() {
        let rects = [card(0.0, 0.0), card(100.0, 0.0), card(0.0, 50.0)];
        assert_eq!(stacking_planes(&rects, 3), vec![0, 0, 0]);
    }

    #[test]
    fn covering_is_between_the_two_sets_only() {
        let (a, b, far) = (card(0.0, 0.0), card(50.0, 20.0), card(500.0, 0.0));
        assert!(covers_any(&[b], &[a]));
        assert!(!covers_any(&[far], &[a, b]), "a and b overlap, but both are under");
        assert!(!covers_any(&[], &[a]));
    }

    #[test]
    fn a_deep_pile_shares_the_top_plane() {
        let rects: Vec<Rectangle> = (0..6).map(|i| card(i as f32 * 10.0, 0.0)).collect();
        assert_eq!(stacking_planes(&rects, 2), vec![0, 1, 2, 2, 2, 2]);
    }

    /// A curve from (0,0) to (100,0) bowing downward.
    fn curve() -> EdgeGeometry {
        EdgeGeometry {
            start: Point::new(0.0, 0.0),
            control1: Point::new(30.0, 40.0),
            control2: Point::new(70.0, 40.0),
            end: Point::new(100.0, 0.0),
            alpha: 1.0,
        }
    }

    #[test]
    fn the_curve_runs_between_its_endpoints() {
        let g = curve();
        assert_eq!(cubic_bezier(&g, 0.0), g.start);
        assert_eq!(cubic_bezier(&g, 1.0), g.end);
        // Both controls pull downward, so the midpoint bows that way.
        assert!(cubic_bezier(&g, 0.5).y > 0.0);
    }

    #[test]
    fn distance_is_zero_on_the_curve_and_grows_away_from_it() {
        let g = curve();
        // Sampled, so a point *on* the curve is within the sampling error.
        assert!(bezier_distance(cubic_bezier(&g, 0.5), &g) < 0.5);
        assert!(bezier_distance(g.start, &g) < 0.001);
        // Well off to the side.
        assert!(bezier_distance(Point::new(50.0, -100.0), &g) > 90.0);
    }

    #[test]
    fn the_reach_test_never_rejects_a_curve_within_the_threshold() {
        // The contract that makes it safe as a pre-filter: it may admit a
        // curve that is farther than the threshold, but never rejects one
        // that is closer. Swept over a grid spanning the curve and well past
        // it, on both sides of the threshold.
        let g = curve();
        let threshold = 6.0;
        for x in (-200..=300).step_by(7) {
            for y in (-200..=300).step_by(7) {
                let p = Point::new(x as f32, y as f32);
                if bezier_distance(p, &g) <= threshold {
                    assert!(
                        within_reach(p, &g, threshold),
                        "rejected {p:?}, which is within {threshold} of the curve"
                    );
                }
            }
        }
    }

    #[test]
    fn the_reach_test_rejects_what_is_plainly_far_away() {
        let g = curve(); // spans x 0..100, y 0..~30
        assert!(!within_reach(Point::new(1000.0, 0.0), &g, 6.0));
        assert!(!within_reach(Point::new(50.0, -500.0), &g, 6.0));
        // Just outside the hull box but inside the threshold: still admitted.
        assert!(within_reach(Point::new(-3.0, 0.0), &g, 6.0));
    }

    #[test]
    fn segment_distance_clamps_to_the_ends() {
        let (a, b) = (Point::new(0.0, 0.0), Point::new(10.0, 0.0));
        assert_eq!(point_segment_distance(Point::new(5.0, 3.0), a, b), 3.0);
        // Past an end: the distance is to the endpoint, not the infinite line.
        assert_eq!(point_segment_distance(Point::new(-4.0, 0.0), a, b), 4.0);
        assert_eq!(point_segment_distance(Point::new(14.0, 0.0), a, b), 4.0);
        // A degenerate segment is just its own point.
        assert_eq!(point_segment_distance(Point::new(3.0, 4.0), a, a), 5.0);
    }
}
