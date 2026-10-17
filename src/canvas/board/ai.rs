//! The AI's marks on the board: the shimmer along the cards an answer is being
//! asked from, and the suggestion cards (shelved — see `app::ghosts`).

use super::*;

/// Lavender sheen swept along the chain feeding a pending AI request, echoing
/// the default response-node tint (Obsidian's purple).
pub(super) const SHIMMER: Color = Color::from_rgba(0.80, 0.74, 0.97, 1.0);

/// Seconds for one shimmer sweep across the chain.
pub(super) const SHIMMER_SWEEP: f32 = 1.15;

/// Quiet seconds between successive sweeps.
pub(super) const SHIMMER_GAP: f32 = 1.0;

/// Number of sweeps before the effect settles onto the response node.
pub(super) const SHIMMER_PULSES: f32 = 2.0;

/// Seconds per breath of the glow that settles on the response node.
pub(super) const SHIMMER_BREATHE: f32 = 1.7;

/// Size (world px) of a ghost card's top-right "×" dismiss zone.
pub(super) const GHOST_X: f32 = 24.0;

/// Weight of a suggestion card's dashed outline, in world units.
pub(super) const GHOST_STROKE: f32 = 1.5;

/// A Copilot-style suggested node: shown as a faint dashed card that becomes a
/// real node (branching off `parent`) when clicked.
pub struct Ghost {
    pub text: String,
    /// World-space placement.
    pub rect: Rectangle,
    /// Id of the node the accepted suggestion should branch from.
    pub parent: String,
    /// A "…" loading placeholder shown while suggestions are fetched — drawn
    /// like a ghost but not clickable/acceptable.
    pub placeholder: bool,
}

/// One pending AI request's input chain, animated by the shimmer: a sweep
/// across `nodes`/`edges` a couple of times, then a breathing glow settling on
/// `target` (the "…" response node). `since` is this flow's own clock.
pub struct AiFlow {
    pub nodes: HashSet<String>,
    pub edges: HashSet<String>,
    pub target: String,
    pub since: Instant,
}

impl Board<'_> {
    /// The suggestion card under `world`, if any (including the placeholder,
    /// which can be dragged but not accepted).
    pub(super) fn ghost_at(&self, world: Point) -> Option<usize> {
        self.ghosts.iter().position(|ghost| ghost.rect.contains(world))
    }

    /// The ghost whose "×" (top-right corner) is under `world`, if any —
    /// including the "…" placeholder, whose "×" cancels the request.
    pub(super) fn ghost_dismiss_at(&self, world: Point) -> Option<usize> {
        self.ghosts.iter().position(|ghost| {
            let zone = Rectangle::new(
                Point::new(ghost.rect.x + ghost.rect.width - GHOST_X, ghost.rect.y),
                Size::new(GHOST_X, GHOST_X),
            );
            zone.contains(world)
        })
    }

    /// Screen-space bezier from a ghost's parent to the ghost, mirroring an
    /// edge's geometry so the suggestion reads as connected.
    pub(super) fn ghost_link(&self, ghost: &Ghost) -> Option<EdgeGeometry> {
        let parent = *self.node_index.get(ghost.parent.as_str())?;
        let parent_rect = self.node_screen_rect(parent, &self.nodes[parent]);
        let ghost_rect = self.camera.rect_to_screen(ghost.rect);
        let start = side_anchor(parent_rect, "right");
        let to_side = opposite_side("right");
        let end = side_anchor(ghost_rect, to_side);
        let reach = start.distance(end) * 0.4;
        Some(EdgeGeometry {
            start,
            control1: start + outward("right") * reach,
            control2: end + outward(to_side) * reach,
            end,
            alpha: 1.0,
        })
    }

    /// Draws the Copilot suggestion cards: faint, dashed-outline versions of a
    /// node (with a dashed link to their parent) so they read as "not yet real".
    pub(super) fn draw_ghosts(&self, frame: &mut Frame) {
        let zoom = self.camera.zoom;
        let dash = [5.0 * zoom, 4.0 * zoom];
        for ghost in self.ghosts {
            // Every card fades in on appear; real suggestions also fade out as
            // they near auto-dismissal (the placeholder stays solid meanwhile).
            let a = self.ghost_fade_in * if ghost.placeholder { 1.0 } else { self.ghost_fade };
            if a <= 0.0 {
                continue;
            }
            let accent = |alpha: f32| Color { a: alpha * a, ..self.theme.accent };
            // The dashed outline a suggestion is drawn with — the same stroke
            // for its link and for its card, differing only in how strongly it
            // reads.
            let dashed = |alpha: f32| Stroke {
                line_dash: canvas::LineDash {
                    segments: &dash,
                    offset: 0,
                },
                ..Stroke::default()
                    .with_color(accent(alpha))
                    .with_width(GHOST_STROKE * zoom)
            };
            if let Some(g) = self.ghost_link(ghost) {
                frame.stroke(&self.link_path(None, &g), dashed(0.4));
            }
            let rect = self.camera.rect_to_screen(ghost.rect);
            let card = rounded_path(rect, self.card_radius());
            frame.fill(&card, accent(0.05));
            frame.stroke(&card, dashed(0.55));
            let wrap = crate::text_raster::card_text_wrap(ghost.rect.width);
            self.draw_markdown_live(frame, rect, &ghost.text, FONT_SIZE, wrap, 0.5 * a, None);

            // A small "×" in the top-right corner dismisses the card (on the
            // placeholder it cancels the request).
            let d = GHOST_X * zoom;
            let cx = rect.x + rect.width - d / 2.0;
            let cy = rect.y + d / 2.0;
            let s = 3.5 * zoom;
            let cross = Path::new(|p| {
                p.move_to(Point::new(cx - s, cy - s));
                p.line_to(Point::new(cx + s, cy + s));
                p.move_to(Point::new(cx + s, cy - s));
                p.line_to(Point::new(cx - s, cy + s));
            });
            frame.stroke(&cross, Stroke::default().with_color(accent(0.6)).with_width(1.3 * zoom));
        }
    }

    /// A skeleton-loader-style shimmer: a soft lavender band sweeps a couple of
    /// times (in screen space, along a gentle rightward diagonal) across the
    /// nodes and edges feeding a pending AI request, washing over the cards and
    /// lighting the links as it passes. When the sweeps finish it settles into a
    /// gentle breathing glow on the "…" response node until the answer arrives.
    pub(super) fn draw_flow_shimmer(&self, frame: &mut Frame, flow: &AiFlow) {
        if flow.nodes.is_empty() {
            return;
        }
        let time = flow.since.elapsed().as_secs_f32();
        let zoom = self.camera.zoom;
        // Each pulse is a sweep followed by a quiet gap; the last pulse leads
        // straight into the settle (no trailing gap).
        let cycle = SHIMMER_SWEEP + SHIMMER_GAP;
        let settle_start = (SHIMMER_PULSES - 1.0) * cycle + SHIMMER_SWEEP;

        // After the sweeps, the "…" response node keeps a breathing glow.
        if time >= settle_start {
            let phase = (time - settle_start) * std::f32::consts::TAU / SHIMMER_BREATHE;
            let alpha = 0.2 + 0.16 * phase.cos();
            if let Some(i) = self.node_index.get(flow.target.as_str()) {
                let r = self.node_screen_rect(*i, &self.nodes[*i]);
                frame.fill(
                    &rounded_path(r, self.card_radius()),
                    Color { a: alpha, ..SHIMMER },
                );
            }
            return;
        }

        // Between pulses: a quiet gap with nothing sweeping.
        let in_cycle = time % cycle;
        if in_cycle >= SHIMMER_SWEEP {
            return;
        }

        // Sweep phase: collect the shimmering shapes and their projection extent.
        let dir = Vector::new(0.94, 0.34); // ~20° below horizontal
        let proj = |p: Point| p.x * dir.x + p.y * dir.y;
        let mut cards: Vec<Rectangle> = Vec::new();
        let mut lines: Vec<EdgeGeometry> = Vec::new();
        let (mut lo, mut hi) = (f32::MAX, f32::MIN);
        let mut span = |p: Point| {
            lo = lo.min(proj(p));
            hi = hi.max(proj(p));
        };
        for (i, node) in self.nodes.iter().enumerate() {
            if flow.nodes.contains(node.id.as_str()) {
                let r = self.node_screen_rect(i, node);
                span(r.position());
                span(Point::new(r.x + r.width, r.y + r.height));
                span(Point::new(r.x + r.width, r.y));
                span(Point::new(r.x, r.y + r.height));
                cards.push(r);
            }
        }
        for edge in self.edges.iter() {
            if flow.edges.contains(edge.id.as_str())
                && let Some(g) = self.edge_geometry(edge)
            {
                for p in [g.start, g.control1, g.control2, g.end] {
                    span(p);
                }
                lines.push(g);
            }
        }
        if lo > hi {
            return;
        }

        // The band: a bright middle stop with transparent ends, sweeping fully
        // across (entering and exiting) once per pass, wrapping between passes.
        let half = ((hi - lo) * 0.22).max(60.0 * zoom);
        let travel = (hi - lo) + half * 2.0;
        let center = lo - half + (in_cycle / SHIMMER_SWEEP) * travel;
        let at = |q: f32| Point::new(dir.x * q, dir.y * q);
        let band = |peak: f32| {
            gradient::Linear::new(at(center - half), at(center + half))
                .add_stop(0.0, Color { a: 0.0, ..SHIMMER })
                .add_stop(0.5, Color { a: peak, ..SHIMMER })
                .add_stop(1.0, Color { a: 0.0, ..SHIMMER })
        };

        let card_wash = band(0.3);
        for r in cards {
            frame.fill(
                &rounded_path(r, self.card_radius()),
                Fill {
                    style: Style::Gradient(card_wash.into()),
                    ..Fill::default()
                },
            );
        }
        let line_glow = band(0.85);
        for g in &lines {
            frame.stroke(
                &self.link_path(None, g),
                Stroke {
                    style: Style::Gradient(line_glow.into()),
                    line_cap: stroke::LineCap::Round,
                    line_join: stroke::LineJoin::Round,
                    ..Stroke::default().with_width(3.5 * zoom)
                },
            );
        }
    }
}
