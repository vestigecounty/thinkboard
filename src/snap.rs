//! Grid snapping math: rounding world coordinates and node sizes to a grid.
//!
//! Pure functions shared by the board (drag/resize snapping) and the app
//! (new/AI node geometry). A non-positive `grid` disables snapping — every
//! function returns its input unchanged — so callers can pass `0.0` to mean
//! "snapping off" and keep the call sites branch-free.

use iced::Point;

/// Rounds a single coordinate to the nearest multiple of `grid`.
pub fn coord(value: f32, grid: f32) -> f32 {
    if grid <= 0.0 {
        value
    } else {
        (value / grid).round() * grid
    }
}

/// Rounds a point to the nearest grid lattice point.
pub fn point(p: Point, grid: f32) -> Point {
    Point::new(coord(p.x, grid), coord(p.y, grid))
}

/// Rounds a size to the nearest grid multiple, never below a single cell (so a
/// node can't collapse). Used for fixed-size nodes (e.g. pasted images).
pub fn size(width: f32, height: f32, grid: f32) -> (f32, f32) {
    (cells(width, grid, f32::round), cells(height, grid, f32::round))
}

/// Rounds a size *up* to the next whole grid multiple (min one cell), so
/// content that measured to `width`×`height` still fits after snapping. Used
/// when a text node auto-sizes to its content.
pub fn fit_size(width: f32, height: f32, grid: f32) -> (f32, f32) {
    (cells(width, grid, f32::ceil), cells(height, grid, f32::ceil))
}

/// `value` in grid cells, mapped through `whole` (round/ceil), clamped to at
/// least one cell, back to world px. A non-positive grid returns `value`.
fn cells(value: f32, grid: f32, whole: fn(f32) -> f32) -> f32 {
    if grid <= 0.0 {
        value
    } else {
        whole(value / grid).max(1.0) * grid
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coord_rounds_to_the_nearest_multiple() {
        assert_eq!(coord(23.0, 20.0), 20.0);
        assert_eq!(coord(31.0, 20.0), 40.0);
        assert_eq!(coord(40.0, 20.0), 40.0); // exact multiple unchanged
        // Negatives round to the nearest multiple too.
        assert_eq!(coord(-11.0, 20.0), -20.0);
        assert_eq!(coord(-9.0, 20.0), 0.0);
    }

    #[test]
    fn a_non_positive_grid_is_a_no_op() {
        assert_eq!(coord(23.4, 0.0), 23.4);
        assert_eq!(coord(23.4, -5.0), 23.4);
        assert_eq!(point(Point::new(1.3, -7.7), 0.0), Point::new(1.3, -7.7));
        assert_eq!(size(13.0, 47.0, 0.0), (13.0, 47.0));
        assert_eq!(fit_size(13.0, 47.0, 0.0), (13.0, 47.0));
    }

    #[test]
    fn point_snaps_both_axes() {
        assert_eq!(point(Point::new(23.0, 31.0), 20.0), Point::new(20.0, 40.0));
        assert_eq!(point(Point::new(-11.0, 9.0), 20.0), Point::new(-20.0, 0.0));
    }

    #[test]
    fn size_rounds_to_nearest_and_never_below_a_cell() {
        assert_eq!(size(50.0, 70.0, 20.0), (60.0, 80.0)); // 2.5->3, 3.5->4
        assert_eq!(size(60.0, 40.0, 20.0), (60.0, 40.0)); // exact
        // Tiny sizes clamp up to one full cell rather than collapsing to 0.
        assert_eq!(size(5.0, 1.0, 20.0), (20.0, 20.0));
    }

    #[test]
    fn fit_size_rounds_up_so_content_still_fits() {
        assert_eq!(fit_size(41.0, 60.0, 20.0), (60.0, 60.0)); // 41->60, 60 exact
        assert_eq!(fit_size(60.0, 60.0, 20.0), (60.0, 60.0));
        assert_eq!(fit_size(1.0, 21.0, 20.0), (20.0, 40.0));
    }
}
