//! Pure geometry for the curved links between nodes.
//!
//! Split out from the board widget because it is the hot path behind link
//! hover: [`bezier_distance`] samples every edge's curve on every mouse move,
//! so its cost scales with the edge count and is worth being able to measure
//! (see `benches/render.rs`) and test on its own.

use iced::Point;

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

#[cfg(test)]
mod tests {
    use super::*;

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
