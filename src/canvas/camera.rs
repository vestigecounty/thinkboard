//! Pan/zoom camera and pan inertia, ported from the Swift `ZoomManager`.
//!
//! Screen mapping: `screen = view_center + offset + world * zoom`.
//! All drawing happens in screen space (points are transformed manually),
//! which keeps text wrapping correct at every zoom level.

use iced::{Point, Rectangle, Size, Vector};
use std::time::Instant;

/// Default zoom bounds (overridable via config).
pub const MIN_ZOOM: f32 = 0.1;
pub const MAX_ZOOM: f32 = 6.0;

/// Below this speed (px/s) inertia stops (Swift: `minimumVelocity`).
const MIN_INERTIA_SPEED: f32 = 1.0;

#[derive(Debug, Clone, Copy)]
pub struct Camera {
    pub offset: Vector,
    pub zoom: f32,
    pub viewport: Size,
    /// Zoom clamp bounds (set from config).
    pub min_zoom: f32,
    pub max_zoom: f32,
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            offset: Vector::ZERO,
            zoom: 1.0,
            viewport: Size::new(1400.0, 900.0),
            min_zoom: MIN_ZOOM,
            max_zoom: MAX_ZOOM,
        }
    }
}

impl Camera {
    /// Screen offset of the viewport center (half its size).
    pub fn center(&self) -> Vector {
        Vector::new(self.viewport.width / 2.0, self.viewport.height / 2.0)
    }

    pub fn to_screen(self, world: Point) -> Point {
        Point::ORIGIN + self.center() + self.offset + (world - Point::ORIGIN) * self.zoom
    }

    pub fn to_world(self, screen: Point) -> Point {
        Point::ORIGIN + ((screen - Point::ORIGIN) - self.center() - self.offset) * (1.0 / self.zoom)
    }

    pub fn rect_to_screen(&self, world: Rectangle) -> Rectangle {
        Rectangle::new(
            self.to_screen(world.position()),
            Size::new(world.width * self.zoom, world.height * self.zoom),
        )
    }

    /// Zooms to `new_zoom` keeping the world point under `cursor` (screen
    /// coordinates) fixed — port of `ZoomManager.calculateZoom`.
    pub fn zoom_around(&mut self, cursor: Point, new_zoom: f32) {
        let new_zoom = new_zoom.clamp(self.min_zoom, self.max_zoom);
        if new_zoom == self.zoom {
            return;
        }
        let anchor = self.to_world(cursor);
        self.zoom = new_zoom;
        self.offset =
            (cursor - Point::ORIGIN) - self.center() - (anchor - Point::ORIGIN) * self.zoom;
    }

    /// Centers the bounding box of the content in the viewport, zooming out
    /// if it does not fit.
    pub fn fit(&mut self, content: Rectangle) {
        let fit_zoom = (self.viewport.width / content.width)
            .min(self.viewport.height / content.height)
            * 0.9;
        self.zoom = fit_zoom.min(1.0).clamp(self.min_zoom, self.max_zoom);
        let content_center = Vector::new(
            content.x + content.width / 2.0,
            content.y + content.height / 2.0,
        );
        self.offset = content_center * -self.zoom;
    }
}

/// Coasting state after a pan drag is released.
#[derive(Debug, Clone, Copy)]
pub struct Inertia {
    pub velocity: Vector,
    pub last_tick: Instant,
    /// Per-120 Hz-tick velocity retention; 0 disables coasting.
    friction: f32,
}

impl Inertia {
    /// Starts coasting, unless `friction` is off or the fling is too slow.
    pub fn start(velocity: Vector, friction: f32) -> Option<Self> {
        (friction > 0.0 && speed(velocity) > MIN_INERTIA_SPEED).then(|| Self {
            velocity,
            last_tick: Instant::now(),
            friction,
        })
    }

    /// Advances one frame; returns the pan delta to apply, or `None` when
    /// the coast has finished. Frame-rate independent: decay matches the
    /// Swift 120 Hz timer for any actual frame duration.
    pub fn tick(&mut self, now: Instant) -> Option<Vector> {
        let dt = now.duration_since(self.last_tick).as_secs_f32().min(0.1);
        self.last_tick = now;

        let delta = self.velocity * dt;
        self.velocity *= self.friction.powf(dt * 120.0);

        (speed(self.velocity) > MIN_INERTIA_SPEED).then_some(delta)
    }
}

fn speed(v: Vector) -> f32 {
    (v.x * v.x + v.y * v.y).sqrt()
}

pub fn ease_out_cubic(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(3)
}

/// Ease-out with a small overshoot past 1.0 before settling.
pub fn ease_out_back(t: f32) -> f32 {
    const C1: f32 = 1.2;
    const C3: f32 = C1 + 1.0;
    1.0 + C3 * (t - 1.0).powi(3) + C1 * (t - 1.0).powi(2)
}
