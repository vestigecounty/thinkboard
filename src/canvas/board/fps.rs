//! The frame meter (`--fps`): what each frame cost, drawn in a corner.

use super::*;

/// Redraws farther apart than this are treated as separate interaction bursts
/// (sparse hover), not stalls, so their idle gap isn't counted as a slow frame.
pub(super) const CONTINUOUS_GAP_MS: f32 = 100.0;

/// Redraw pacing stats for the `--fps` meter. Measured draw-to-draw, so it
/// reflects the redraw rate the user actually perceives during interaction.
pub(super) struct FrameStats {
    pub(super) last: Option<Instant>,
    pub(super) ema_ms: f32,
    pub(super) worst_ms: f32,
    /// EMA of the CPU time spent building the board geometry (the cached
    /// tessellation closure). Near zero when the cache is reused; rises during
    /// pan/zoom when it regenerates every frame.
    pub(super) build_ms: f32,
    /// EMA of per-frame Markdown-layout shaping time (live text) and the layout
    /// cache hit rate. Shaping time falls to ~0 once the cache warms; hit rate
    /// drops while editing (text changes force re-shaping).
    pub(super) layout_ms: f32,
    pub(super) hit_rate: f32,
    pub(super) prev_hits: u64,
    pub(super) prev_misses: u64,
    pub(super) prev_shape_ns: u64,
}

pub(super) static FRAME_STATS: Mutex<FrameStats> = Mutex::new(FrameStats {
    last: None,
    ema_ms: 8.3,
    worst_ms: 0.0,
    build_ms: 0.0,
    layout_ms: 0.0,
    hit_rate: 1.0,
    prev_hits: 0,
    prev_misses: 0,
    prev_shape_ns: 0,
});

impl Board<'_> {
    /// Uncached, so it re-renders (and samples the clock) every redraw.
    pub(super) fn draw_fps_meter(&self, renderer: &Renderer, bounds: Rectangle) -> Geometry {
        let now = Instant::now();
        let (hits, misses, shape_ns) = crate::text_raster::layout_counters();
        let (ema_ms, worst_ms, build_ms, layout_ms, hit_rate) = {
            let mut stats = FRAME_STATS.lock().unwrap();
            // Measure inter-frame pacing only across back-to-back redraws.
            // Frames close in time are a continuous interaction (active pan,
            // drag, zoom, inertia); a large gap means sparse event-driven
            // redraws (a paused hover) — its "dt" is idle time, not a stall, so
            // reset rather than record it. This catches active panning, which
            // the frame-loop flag alone missed.
            if let Some(last) = stats.last {
                let dt = now.duration_since(last).as_secs_f32() * 1000.0;
                if dt < CONTINUOUS_GAP_MS {
                    stats.ema_ms = stats.ema_ms * 0.9 + dt * 0.1;
                    stats.worst_ms = stats.worst_ms.max(dt);
                } else {
                    // New burst: start `worst` fresh for this interaction.
                    stats.worst_ms = 0.0;
                }
            }
            stats.last = Some(now);

            // Per-frame layout-cache deltas: shaping time this frame, and the
            // fraction of layout lookups that hit the cache.
            let frame_hits = hits - stats.prev_hits;
            let frame_lookups = frame_hits + (misses - stats.prev_misses);
            let frame_ms = (shape_ns - stats.prev_shape_ns) as f32 / 1.0e6;
            (stats.prev_hits, stats.prev_misses, stats.prev_shape_ns) = (hits, misses, shape_ns);
            stats.layout_ms = stats.layout_ms * 0.9 + frame_ms * 0.1;
            if frame_lookups > 0 {
                let rate = frame_hits as f32 / frame_lookups as f32;
                stats.hit_rate = stats.hit_rate * 0.9 + rate * 0.1;
            }

            (
                stats.ema_ms,
                stats.worst_ms,
                stats.build_ms,
                stats.layout_ms,
                stats.hit_rate,
            )
        };

        // Estimated texture footprint of the text/cluster bitmaps (RGBA), the
        // memory the raster scale/cross-fade trades against sharpness.
        let vmem_mb = (self.text_snapshots.values().map(|s| s.bytes).sum::<usize>()
            + self.cluster_snapshots.values().map(|s| s.bytes).sum::<usize>()
            + self
                .staging_snapshots
                .values()
                .map(|s| s.snapshot.bytes)
                .sum::<usize>()) as f32
            / (1024.0 * 1024.0);

        let mut frame = Frame::new(renderer, bounds.size());
        frame.fill_rectangle(
            Point::new(8.0, 8.0),
            Size::new(640.0, 24.0),
            Color::from_rgba(0.0, 0.0, 0.0, 0.7),
        );
        frame.fill_text(canvas::Text {
            content: format!(
                "{:5.1} fps  {:5.2}ms  worst {:5.2}  build {:4.2}  layout {:4.2}ms {:3.0}% hit  vmem {:5.1}MB",
                1000.0 / ema_ms,
                ema_ms,
                worst_ms,
                build_ms,
                layout_ms,
                hit_rate * 100.0,
                vmem_mb,
            ),
            position: Point::new(16.0, 20.0),
            color: Color::from_rgb(0.4, 1.0, 0.5),
            size: 12.0.into(),
            // Monospace so the padded numbers keep fixed columns instead of
            // jittering as digit widths change under the proportional UI font.
            font: iced::Font::MONOSPACE,
            align_y: Vertical::Center,
            shaping: Shaping::Advanced,
            ..canvas::Text::default()
        });
        frame.into_geometry()
    }
}
