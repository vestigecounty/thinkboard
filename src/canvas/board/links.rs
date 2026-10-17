//! Links: where one runs ([`Board::link_segments`], the one place the three
//! styles are dispatched), where its ends and arrowheads go, how it's drawn and
//! hit, and the ports a new one is dragged from. Everything that has to agree
//! about where a link is lives here.

use super::*;

/// A link's colour when it has none of its own. Opaque: how solid a link is
/// drawn is `config.link_opacity`, for every link alike.
pub(super) const EDGE_COLOR: Color = Color::from_rgb(0.35, 0.62, 1.0);

/// Point size (world px) of a link's caption, and the zoom below which drawing
/// one is pointless — the text is sub-pixel and the board is heading for
/// cluster mode anyway.
pub(super) const EDGE_LABEL_SIZE: f32 = 12.0;

pub(super) const EDGE_LABEL_MIN_ZOOM: f32 = 0.45;

/// Line color for a hovered link — a muted blue.
pub(super) const EDGE_HOVER: Color = Color::from_rgb(0.2625, 0.595, 0.875);

/// Selected link: an electric, highly saturated blue — noticeably more vivid
/// than [`EDGE_HOVER`] (lower red widens the channel spread for more
/// saturation, without adding brightness) so selection reads distinctly.
pub(super) const EDGE_HIGHLIGHT: Color = Color::from_rgb(0.11, 0.66, 1.0);

/// Opaque ring drawn behind a hovered/selected link — a darker shade of the
/// accent, so the vivid line reads on top. Opaque (not translucent) so the
/// color stays true and overlapping strokes don't compound. The selected ring
/// is more saturated than the hover ring, matching the line hierarchy.
pub(super) const EDGE_HOVER_RING: Color = Color::from_rgb(0.114, 0.2625, 0.42);

pub(super) const EDGE_HIGHLIGHT_RING: Color = Color::from_rgb(0.06, 0.32, 0.54);

/// Length of an edge's arrowhead, in world units. Shared by the ordinary and
/// the highlighted edge — only their line weights differ.
pub(super) const ARROW_HEAD: f32 = 8.0;

/// The arrowhead's stroke, in world units. Its round cap reaches half of it
/// past the tip.
pub(super) const ARROW_WIDTH: f32 = 2.0;

/// The clear space between an arrow's tip and the card it points at, in world
/// units. On the card's edge, the tip sat on its border: a link the colour of
/// a card's border lost its tip in it, and links fanning into one card merged
/// into its outline. In world units so it scales with the board — about 2 px
/// at 100%, under a pixel zoomed out, where a fixed gap made links look detached
/// from what they point at.
pub(super) const ARROW_GAP: f32 = 2.0;

/// An arrowhead as drawn: its shape, how far a chevron's wings reach back
/// along the link, and — for a chevron — the width of its stroke, all on
/// screen. A triangle is sized from the same length; see [`Head::reach`].
#[derive(Clone, Copy)]
pub(super) struct Head {
    pub(super) tip: ArrowTip,
    pub(super) length: f32,
    pub(super) stroke: f32,
}

/// A triangle's length against a chevron's, and its half-width against its
/// own length. At a chevron's length, filled, it read as a speck at the end
/// of the line: these are the proportions of the design it was drawn from —
/// about five times the line's width long, two and a half either side of it.
pub(super) const TRIANGLE_LENGTH: f32 = 1.625;

pub(super) const TRIANGLE_SPREAD: f32 = 0.5;

impl Head {
    /// How far the head reaches back from its point along the link.
    pub(super) fn reach(self) -> f32 {
        match self.tip {
            ArrowTip::Chevron => self.length,
            ArrowTip::Triangle => self.length * TRIANGLE_LENGTH,
        }
    }

    /// How far past the point the drawn head reaches: a chevron's round cap,
    /// half its stroke; a triangle comes to the point itself.
    pub(super) fn overhang(self) -> f32 {
        match self.tip {
            ArrowTip::Chevron => self.stroke / 2.0,
            ArrowTip::Triangle => 0.0,
        }
    }

    /// A triangle's base corners, for a head pointing into `side` at `point`.
    pub(super) fn base(self, point: Point, side: &str) -> (Point, Point) {
        let out = outward(side);
        let across = Vector::new(-out.y, out.x) * (self.reach() * TRIANGLE_SPREAD);
        let base = point + out * self.reach();
        (base + across, base - across)
    }
}

/// Where an arrow pointing into `side` of a card at `anchor` puts its point:
/// [`ARROW_GAP`] clear of the card, measured from what's drawn — a chevron's
/// round cap reaches past its point — not from the point.
pub(super) fn arrow_tip(anchor: Point, side: &str, zoom: f32, head: Head) -> Point {
    anchor + outward(side) * (ARROW_GAP * zoom + head.overhang())
}

/// How a link's line is drawn into its arrowheads: how far short of each
/// point it stops, measured straight back from the point, and — for a
/// chevron — the wedge left between that end and the chevron's inner corner,
/// filled in the line's colour.
///
/// **A link is translucent, so wherever two of its strokes overlap the colour
/// is laid down twice.** Run on to the point, the line lay under the
/// chevron's last few pixels and the tip came out denser than the link. Just
/// stopping it sooner traded that for a speck of board showing inside the V,
/// whatever the stop: the V narrows to its corner, a butt end is square.
/// The wedge is exactly that remainder — its sides are the chevron's inner
/// edges and its base the line's end — so the line, the wedge and the chevron
/// tile the tip, each pixel covered once. A chevron's wings sit at 45° to the
/// line, so its inner corner is `stroke / √2` back from the point, and a line
/// `line` wide meets the inner edges `line / 2` further. A triangle is
/// simpler: the line stops at its base, and the two meet edge to edge.
///
/// The line must have a butt cap, and is cut from the routed path
/// ([`Board::link_path_trimmed`]) rather than routed from moved ends. The last
/// stretch into an arrow runs straight along it in every routing style, so the
/// cut lands where the wedge expects it.
pub(super) fn shaft(ends: [Option<(Point, &str, Head)>; 2], line: f32) -> ([f32; 2], Vec<[Point; 3]>) {
    let mut trims = [0.0; 2];
    let mut wedges = Vec::new();
    for (trim, end) in trims.iter_mut().zip(ends) {
        let Some((point, side, head)) = end else {
            continue;
        };
        let out = outward(side);
        *trim = match head.tip {
            ArrowTip::Chevron => {
                let corner = head.stroke / SQRT_2;
                let back = corner + line / 2.0;
                let across = Vector::new(-out.y, out.x) * (line / 2.0);
                let base = point + out * back;
                wedges.push([point + out * corner, base + across, base - across]);
                back
            }
            ArrowTip::Triangle => head.reach(),
        };
    }
    (trims, wedges)
}

/// A point on a cubic piece at parameter `t`.
pub(super) fn cubic_at(piece: &EdgeGeometry, t: f32) -> Point {
    split_cubic(piece, t).0.end
}

/// `piece` split at `t` (de Casteljau): the part before and the part after,
/// each the very curve it was a part of.
pub(super) fn split_cubic(piece: &EdgeGeometry, t: f32) -> (EdgeGeometry, EdgeGeometry) {
    let (p0, p1, p2, p3) = (piece.start, piece.control1, piece.control2, piece.end);
    let (p01, p12, p23) = (lerp(p0, p1, t), lerp(p1, p2, t), lerp(p2, p3, t));
    let (p012, p123) = (lerp(p01, p12, t), lerp(p12, p23, t));
    let at = lerp(p012, p123, t);
    (
        EdgeGeometry { start: p0, control1: p01, control2: p012, end: at, ..*piece },
        EdgeGeometry { start: at, control1: p123, control2: p23, end: p3, ..*piece },
    )
}

/// Where along `piece` the point `distance` straight from its end (or, with
/// `from_end` false, its start) lies, as a parameter — by bisection, since a
/// cubic has no closed form for it. Close enough is a hundredth of a pixel.
pub(super) fn cut_at(piece: &EdgeGeometry, distance: f32, from_end: bool) -> f32 {
    let anchor = if from_end { piece.end } else { piece.start };
    // The near and far parameters: the anchor's own, and the other end's.
    let (mut near, mut far) = if from_end { (1.0, 0.0) } else { (0.0, 1.0) };
    if cubic_at(piece, far).distance(anchor) <= distance {
        return far; // the whole piece is inside the cut
    }
    for _ in 0..24 {
        let mid = (near + far) / 2.0;
        if cubic_at(piece, mid).distance(anchor) < distance {
            near = mid;
        } else {
            far = mid;
        }
    }
    (near + far) / 2.0
}

/// Fills the wedges [`shaft`] leaves between a line and its arrowheads.
pub(super) fn fill_wedges(frame: &mut Frame, wedges: &[[Point; 3]], color: Color) {
    for [a, b, c] in wedges {
        frame.fill(
            &Path::new(|builder| {
                builder.move_to(*a);
                builder.line_to(*b);
                builder.line_to(*c);
                builder.close();
            }),
            color,
        );
    }
}

/// The path through a link's cubic pieces, in order.
pub(super) fn path_through(segments: &[EdgeGeometry]) -> Path {
    Path::new(|builder| {
        let Some(first) = segments.first() else {
            return;
        };
        builder.move_to(first.start);
        for piece in segments {
            builder.line_to(piece.start);
            builder.bezier_curve_to(piece.control1, piece.control2, piece.end);
        }
    })
}

/// A link's drawn path, routed the way Mermaid routes one.
///
/// **Waypoints, smoothed by a B-spline** — not a bezier between the two ends.
/// Mermaid's flowcharts come out of dagre (a port of the layered algorithm
/// GraphViz's `dot` implements) as a *polyline*, and are drawn with d3's
/// `curveBasis`, which approximates its interior points rather than passing
/// through them. Corners round themselves, so a waypoint route needs no radius
/// of its own.
///
/// What this changes against [`bezier_path`] is the *ends*. Obsidian reaches
/// out by 0.4x the distance between the ports, so the curve scales with the
/// span — 720 px out on an 1800 px link. This steps a fixed [`EDGE_GUTTER`]
/// whatever the span and runs nearly straight between.
///
/// It does **not** bundle a crowded fan. Siblings share their leading
/// waypoints, which fixes the opening segment's control points but not its
/// endpoint — that already depends on the target — so they part at once, and
/// measurably wider than the bezier does through the first 150 px.
pub(super) fn mermaid_route(g: &EdgeGeometry, gutter: f32) -> Vec<EdgeGeometry> {
    let from = unit(g.control1 - g.start);
    let to = unit(g.control2 - g.end);
    // How far the step out of a port runs. Shared by every link leaving it, so
    // their leading waypoints coincide — capped on the room available, since a
    // step past the target would route the line backwards.
    let waypoints = [g.start, g.start + from * gutter, g.end + to * gutter, g.end];
    let (opening, segments) = basis_segments(&waypoints);
    let mut start = opening.unwrap_or(g.start);
    let mut pieces = Vec::with_capacity(segments.len() + 1);
    // The short straight run d3 opens with, as a degenerate cubic so the whole
    // route is one kind of thing.
    pieces.push(EdgeGeometry { start: g.start, control1: g.start, control2: start, end: start, ..*g });
    for (control1, control2, end) in segments {
        pieces.push(EdgeGeometry { start, control1, control2, end, ..*g });
        start = end;
    }
    pieces
}

/// Stroke width of a link, in world units — the line, its preview and the
/// shimmer that traces it are one line, so they are drawn at one width.
pub(super) const EDGE_LINE_WIDTH: f32 = 2.5;

/// A link as straight runs meeting at square corners, sharing a trunk.
///
/// The third style, and the only one with no curve in it: out of the port along
/// its own normal, a long run, and in along the target's. Because the step out
/// is the port's rather than each link's, the long runs of everything leaving
/// one port land on the same line and draw on top of each other — one stem with
/// square branches, the way a circuit or an org chart is drawn.
///
/// **Both normals matter.** Routed off the source's alone, a link out of a
/// bottom port went down a little, across to its target's column, and then ran
/// the long way *down that column* — a bar flush against the left edge of every
/// node in it. Continuing along the source's normal first and turning in along
/// the target's puts the long run at the port's own coordinate, where it is a
/// trunk, and leaves only a short stub at the far end.
///
/// The corners are deliberately sharp. Rounding them is what the other two
/// routings are for; this is here to be told apart from them at a glance.
pub(super) fn orthogonal_route(g: &EdgeGeometry, gutter: f32) -> Vec<EdgeGeometry> {
    let from = unit(g.control1 - g.start);
    let to = unit(g.control2 - g.end);
    // A step off each end along its own normal, and the turn between them: down
    // (or out) the source's line as far as the target's stub, then across to it.
    let out = g.start + from * gutter;
    let stub = g.end + to * gutter;
    let turn = Point::new(out.x, stub.y);
    runs(&[g.start, out, turn, stub, g.end], g)
}

/// Straight runs through `points`, as cubic pieces.
///
/// Points that add nothing are dropped first — a repeat, or one that lies on
/// the line between its neighbours — so a link with nothing to avoid is one run
/// rather than several laid end to end pretending to bend.
pub(super) fn runs(points: &[Point], g: &EdgeGeometry) -> Vec<EdgeGeometry> {
    let mut corners: Vec<Point> = Vec::with_capacity(points.len());
    for &point in points {
        if corners.last().is_some_and(|last: &Point| last.distance(point) < 0.5) {
            continue;
        }
        // Straight through the previous corner: it isn't one.
        if let [.., before, at] = corners.as_slice() {
            let (a, b) = (*at - *before, point - *at);
            if (a.x * b.y - a.y * b.x).abs() < 0.5 && a.x * b.x + a.y * b.y > 0.0 {
                corners.pop();
            }
        }
        corners.push(point);
    }
    corners
        .windows(2)
        .map(|run| EdgeGeometry {
            start: run[0],
            // Control points on the line: a cubic whose handles lie along it
            // *is* the segment, and keeps every piece the same kind of thing.
            control1: lerp(run[0], run[1], 1.0 / 3.0),
            control2: lerp(run[0], run[1], 2.0 / 3.0),
            end: run[1],
            ..*g
        })
        .collect()
}

/// How far a link steps straight out of its port before turning, in **world**
/// units. Mermaid's is half the rank separation; here there are no ranks, so it
/// is a constant, capped on the room the port's nearest target leaves.
///
/// World units against the zoom, so a board keeps its shape as the camera moves
/// over it. Held in screen units the step shrank in world terms as you zoomed
/// in, walking the trunk toward the node it came out of.
pub(super) const EDGE_GUTTER: f32 = 60.0;

/// The least a trunk may stand off the node it leaves, in world units — enough
/// to read as clear of it.
pub(super) const MIN_TRUNK_GUTTER: f32 = 14.0;

/// Where a port's trunk stands, given the room its nearest target leaves.
///
/// Between two bounds that can conflict: short of the nearest target, so the
/// step never overshoots what it is stepping toward, and clear of the node it
/// leaves. Where there isn't room for both — a target closer than the minimum
/// clearance — it splits the difference, which is the best either can have.
///
/// Nothing ahead is *no cap*, not a cap of nothing. A port whose targets all
/// lie level with it or behind it has nothing to overshoot, so it takes the
/// full step; reading that as zero room was what stood the trunk on the node.
pub(super) fn trunk_gutter(nearest_ahead: f32, zoom: f32) -> f32 {
    let (gutter, minimum) = (EDGE_GUTTER * zoom, MIN_TRUNK_GUTTER * zoom);
    if nearest_ahead <= 0.0 {
        return gutter;
    }
    gutter.min(nearest_ahead * 0.4).max(minimum.min(nearest_ahead * 0.5))
}

/// The spline's cubic segments, and the short straight run d3 opens with.
///
/// Split out from the drawing so the shape can be checked without a canvas.
pub(super) fn basis_segments(points: &[Point]) -> (Option<Point>, Vec<(Point, Point, Point)>) {
    let [first, rest @ ..] = points else {
        return (None, Vec::new());
    };
    let Some(&second) = rest.first() else {
        return (None, Vec::new());
    };
    let between = |a: Point, b: Point, wa: f32, wb: f32| {
        Point::new((a.x * wa + b.x * wb) / (wa + wb), (a.y * wa + b.y * wb) / (wa + wb))
    };
    // d3 opens a sixth of the way into the first span, where the spline begins.
    let opening = between(*first, second, 5.0, 1.0);

    let mut segments = Vec::with_capacity(rest.len());
    let (mut previous, mut current) = (*first, second);
    let push = |previous: Point, current: Point, end: Point| {
        (
            between(previous, current, 2.0, 1.0),
            between(previous, current, 1.0, 2.0),
            end,
        )
    };
    for &next in &rest[1..] {
        let end = Point::new(
            (previous.x + 4.0 * current.x + next.x) / 6.0,
            (previous.y + 4.0 * current.y + next.y) / 6.0,
        );
        segments.push(push(previous, current, end));
        previous = current;
        current = next;
    }
    // …and closes onto the last point, so the arrowhead lands on the node.
    segments.push(push(previous, current, current));
    (Some(opening), segments)
}

pub(super) const RING_RADIUS: f32 = 30.0;

/// Lazily rasterized port dot: a blue disc with a white ring, antialiased.
/// Rendered once at 64px and GPU-scaled to the display size.
pub(super) fn port_dot() -> &'static image::Handle {
    static DOT: OnceLock<image::Handle> = OnceLock::new();
    DOT.get_or_init(|| {
        const SIZE: u32 = 64;
        const CENTER: f32 = 32.0;
        const FILL_RADIUS: f32 = 26.0;

        let blue = [
            (EDGE_COLOR.r * 255.0) as u8,
            (EDGE_COLOR.g * 255.0) as u8,
            (EDGE_COLOR.b * 255.0) as u8,
        ];

        let mut pixels = vec![0u8; (SIZE * SIZE * 4) as usize];
        for y in 0..SIZE {
            for x in 0..SIZE {
                let dx = x as f32 + 0.5 - CENTER;
                let dy = y as f32 + 0.5 - CENTER;
                let d = (dx * dx + dy * dy).sqrt();

                let coverage = (RING_RADIUS + 0.5 - d).clamp(0.0, 1.0);
                if coverage == 0.0 {
                    continue;
                }
                // Blue disc blending into the white ring at its rim.
                let ring = (d - (FILL_RADIUS - 0.5)).clamp(0.0, 1.0);

                let i = ((y * SIZE + x) * 4) as usize;
                for c in 0..3 {
                    let color = blue[c] as f32 + (255.0 - blue[c] as f32) * ring;
                    pixels[i + c] = color as u8;
                }
                pixels[i + 3] = (coverage * 255.0) as u8;
            }
        }

        image::Handle::from_rgba(SIZE, SIZE, pixels)
    })
}

/// The side of `rect` that faces the given point — used to pick which side
/// of a target node an incoming edge should attach to. Distances are
/// normalized by the half-extents so elongated nodes attach naturally.
/// The side opposite `side`, so a link out of a source's port enters the target
/// on the facing side (drag from a `right` port → connect to the target's
/// `left`), which keeps sibling links from piling onto one side.
pub fn opposite_side(side: &str) -> &'static str {
    match side.to_lowercase().as_str() {
        "left" => "right",
        "right" => "left",
        "top" => "bottom",
        "bottom" => "top",
        _ => "left",
    }
}

pub fn facing_side(point: Point, rect: Rectangle) -> &'static str {
    let center = rect.center();
    let nx = (point.x - center.x) / (rect.width / 2.0).max(1.0);
    let ny = (point.y - center.y) / (rect.height / 2.0).max(1.0);
    if nx.abs() > ny.abs() {
        if nx > 0.0 { "right" } else { "left" }
    } else if ny > 0.0 {
        "bottom"
    } else {
        "top"
    }
}

pub fn side_anchor(rect: Rectangle, side: &str) -> Point {
    let center = rect.center();
    match side.to_lowercase().as_str() {
        "left" => Point::new(rect.x, center.y),
        "right" => Point::new(rect.x + rect.width, center.y),
        "top" => Point::new(center.x, rect.y),
        "bottom" => Point::new(center.x, rect.y + rect.height),
        _ => center,
    }
}

pub(super) fn outward(side: &str) -> Vector {
    match side.to_lowercase().as_str() {
        "left" => Vector::new(-1.0, 0.0),
        "right" => Vector::new(1.0, 0.0),
        "top" => Vector::new(0.0, -1.0),
        "bottom" => Vector::new(0.0, 1.0),
        _ => Vector::new(1.0, 0.0),
    }
}

impl Board<'_> {
    pub(super) fn port_radius(&self) -> f32 {
        (4.0 * self.camera.zoom).clamp(3.5, 9.0)
    }

    /// The side port under the cursor, if any (with some grab slack).
    pub(super) fn port_at(&self, position: Point) -> Option<(usize, &'static str)> {
        let grab = self.port_radius() + 10.0;
        let port = |index: usize, node: &CanvasNode| {
            let rect = self.node_screen_rect(index, node);
            SIDES
                .into_iter()
                .find(|side| side_anchor(rect, side).distance(position) < grab)
                .map(|side| (index, side))
        };
        // Cards first, frames second. A frame's border runs *behind* its
        // members, so a card's port has to win wherever the two overlap —
        // otherwise dragging a card near the edge of its group would grab the
        // group instead.
        self.frame_last(|index, node| port(index, node))
    }

    /// Where a dragged connection would attach for the given cursor
    /// position: within a node, the side of the quadrant the cursor is in
    /// (the rect split into four triangles by its diagonals); within the
    /// surrounding [`SNAP_MARGIN`], the nearest side dot.
    pub(super) fn connect_target(&self, from: usize, cursor: Point) -> Option<(usize, &'static str)> {
        let candidates: Vec<(usize, &CanvasNode)> = {
            let cards = self.nodes.iter().enumerate().rev().filter(|(_, n)| !n.is_group());
            let frames = self.nodes.iter().enumerate().rev().filter(|(_, n)| n.is_group());
            cards.chain(frames).collect()
        };
        for (index, node) in candidates {
            if index == from {
                continue;
            }
            let rect = self.node_screen_rect(index, node);

            if rect.contains(cursor) {
                return Some((index, facing_side(cursor, rect)));
            }

            let inflated = Rectangle::new(
                rect.position() - Vector::new(SNAP_MARGIN, SNAP_MARGIN),
                Size::new(
                    rect.width + SNAP_MARGIN * 2.0,
                    rect.height + SNAP_MARGIN * 2.0,
                ),
            );
            if inflated.contains(cursor) {
                let side = SIDES
                    .into_iter()
                    .min_by(|a, b| {
                        side_anchor(rect, a)
                            .distance(cursor)
                            .total_cmp(&side_anchor(rect, b).distance(cursor))
                    })
                    .unwrap();
                return Some((index, side));
            }
        }
        None
    }

    /// Screen-space bezier geometry of an edge, or `None` if an endpoint
    /// node is missing.
    pub(super) fn edge_geometry(&self, edge: &CanvasEdge) -> Option<EdgeGeometry> {
        let from_index = *self.node_index.get(edge.from_node.as_str())?;
        let to_index = *self.node_index.get(edge.to_node.as_str())?;

        // A link to a match stays in full, so a match is seen with what it
        // hangs from and what hangs from it; a link between two dimmed cards
        // dims with them.
        let alpha = self.node_layer_alpha()
            * self.spawn_factors(from_index).1.min(self.spawn_factors(to_index).1)
            * self.search_dim(from_index).max(self.search_dim(to_index));
        let mut start = side_anchor(
            self.node_screen_rect(from_index, &self.nodes[from_index]),
            &edge.from_side,
        );
        let mut end = side_anchor(
            self.node_screen_rect(to_index, &self.nodes[to_index]),
            &edge.to_side,
        );
        // An end with an arrow stops short of its card; one without stays on
        // its port, where a drag starts and the hover dot sits. Here, before
        // routing, so the line, its rings, the hit test and the caption all
        // take the same ends.
        let zoom = self.camera.zoom;
        let head = self.head(ARROW_HEAD * zoom, ARROW_WIDTH * zoom);
        if edge.to_end() == EdgeEnd::Arrow {
            end = arrow_tip(end, &edge.to_side, zoom, head);
        }
        if edge.from_end() == EdgeEnd::Arrow {
            start = arrow_tip(start, &edge.from_side, zoom, head);
        }
        // Control points extend outward from each side by 0.4 x distance
        // (Swift: `calculateBezierControlPoints`).
        let reach = start.distance(end) * 0.4;
        let control1 = start + outward(&edge.from_side) * reach;
        let control2 = end + outward(&edge.to_side) * reach;
        Some(EdgeGeometry {
            start,
            control1,
            control2,
            end,
            alpha,
        })
    }

    /// The edge under the given screen point (nearest within a slop radius),
    /// found by sampling each bezier and measuring distance.
    pub(super) fn edge_at(&self, point: Point) -> Option<usize> {
        let threshold = (2.5 * self.camera.zoom).max(6.0);
        let mut best: Option<(usize, f32)> = None;
        for (index, edge) in self.edges.iter().enumerate() {
            let Some(geometry) = self.edge_geometry(edge) else {
                continue;
            };
            // Rejects on the hull box before sampling; on a board with many
            // links almost every edge stops there.
            let Some(distance) = self.link_distance(point, edge, &geometry, threshold) else {
                continue;
            };
            if distance < threshold && best.is_none_or(|(_, best)| distance < best) {
                best = Some((index, distance));
            }
        }
        best.map(|(index, _)| index)
    }

    pub(super) fn draw_edges(&self, frame: &mut Frame) {
        for (index, edge) in self.edges.iter().enumerate() {
            self.draw_edge_line(frame, index, self.edge_color(edge));
        }
        // Labels last, so a caption is never crossed by a later edge's line.
        for (index, edge) in self.edges.iter().enumerate() {
            self.draw_edge_label(frame, index, edge);
        }
    }

    /// A link's own colour, falling back to the board's default. The hover and
    /// selection passes override this with their highlight colours, so a
    /// coloured link still shows the same feedback as any other.
    pub(super) fn edge_color(&self, edge: &CanvasEdge) -> Color {
        self.link_ink(edge.palette_color().map(IntoColor::into_color).unwrap_or(EDGE_COLOR))
    }

    /// The opaque outline (ring) of a highlighted edge: drawn wider than, and
    /// behind, the link's own line so a border of `color` (a darker shade of
    /// the accent) shows around the vivid line. Opaque, so the color stays
    /// true and the overlapping line/arrowhead strokes don't compound alpha.
    /// `thickness` scales the ring width (hover uses 1.0; selection a heavier
    /// ring to stand apart).
    /// Draws a link's caption at the midpoint of its curve, on a small filled
    /// chip so the text stays readable where it crosses the line.
    ///
    /// Sized from the character count rather than measured: the chip only has to
    /// cover the glyphs behind it, and running the shaper per edge per frame to
    /// get that exactly right would cost far more than it's worth.
    pub(super) fn draw_edge_label(&self, frame: &mut Frame, index: usize, edge: &CanvasEdge) {
        let Some(label) = edge.label() else {
            return;
        };
        // Below this the caption is unreadable anyway, and a board zoomed out
        // to its clusters shouldn't pay to draw one per link.
        let zoom = self.camera.zoom;
        if zoom < EDGE_LABEL_MIN_ZOOM || self.show_clusters() {
            return;
        }
        let Some(g) = self.edges.get(index).and_then(|e| self.edge_geometry(e)) else {
            return;
        };

        let size = EDGE_LABEL_SIZE * zoom;
        let center = self.link_midpoint(edge, &g);
        // ~0.55em per character is a reasonable average for the UI font; the
        // chip is padded either way, so a little slack is harmless.
        let width = label.chars().count() as f32 * size * 0.55 + size;
        let height = size * crate::text_raster::LINE_HEIGHT + size * 0.35;
        let chip = Rectangle::new(
            Point::new(center.x - width / 2.0, center.y - height / 2.0),
            Size::new(width, height),
        );
        frame.fill(
            &Path::rounded_rectangle(chip.position(), chip.size(), (height / 2.0).into()),
            Color { a: 0.92 * g.alpha, ..self.theme.background },
        );
        frame.fill_text(canvas::Text {
            content: label.to_owned(),
            position: center,
            color: Color { a: g.alpha, ..self.theme.text_muted },
            size: size.into(),
            font: self.theme.ui_font(),
            align_x: TextAlignment::Center,
            align_y: Vertical::Center,
            ..canvas::Text::default()
        });
    }

    pub(super) fn draw_edge_outline(&self, frame: &mut Frame, index: usize, color: Color, thickness: f32) {
        let Some(edge) = self.edges.get(index) else {
            return;
        };
        let Some(g) = self.edge_geometry(edge) else {
            return;
        };
        let zoom = self.camera.zoom;
        frame.stroke(
            &self.link_path(Some(edge), &g),
            Stroke {
                // Butt (not round) so the line stops at the tip rather than
                // poking a half-width past the arrowhead.
                line_cap: stroke::LineCap::Butt,
                line_join: stroke::LineJoin::Round,
                ..Stroke::default()
                    .with_color(fade(color, g.alpha))
                    .with_width(5.5 * zoom * thickness)
            },
        );
        // Ringed where the link has a head, and only there.
        let head = self.head(ARROW_HEAD * zoom, 4.4 * zoom * thickness);
        if edge.to_end() == EdgeEnd::Arrow {
            self.draw_arrowhead(frame, g.end, &edge.to_side, g.alpha, head, color, true);
        }
        if edge.from_end() == EdgeEnd::Arrow {
            self.draw_arrowhead(frame, g.start, &edge.from_side, g.alpha, head, color, true);
        }
    }

    /// The brighter line + arrowhead of a highlighted edge.
    pub(super) fn draw_edge_line(&self, frame: &mut Frame, index: usize, color: Color) {
        let Some(edge) = self.edges.get(index) else {
            return;
        };
        let Some(g) = self.edge_geometry(edge) else {
            return;
        };
        let zoom = self.camera.zoom;
        let head = self.head(ARROW_HEAD * zoom, ARROW_WIDTH * zoom);
        let arrows = [
            (edge.from_end() == EdgeEnd::Arrow).then_some((g.start, edge.from_side.as_str(), head)),
            (edge.to_end() == EdgeEnd::Arrow).then_some((g.end, edge.to_side.as_str(), head)),
        ];
        let (trims, wedges) = shaft(arrows, EDGE_LINE_WIDTH * zoom);
        frame.stroke(
            &self.link_path_trimmed(Some(edge), &g, trims),
            Stroke {
                // Butt (not round), so it meets the wedge into its arrowhead
                // edge to edge — see `shaft`.
                line_cap: stroke::LineCap::Butt,
                line_join: stroke::LineJoin::Round,
                ..Stroke::default()
                    .with_color(fade(color, g.alpha))
                    .with_width(EDGE_LINE_WIDTH * zoom)
            },
        );
        fill_wedges(frame, &wedges, fade(color, g.alpha));
        // Obsidian records an arrow at the target and none at the source
        // unless the document says otherwise — an undirected or bidirectional
        // link is drawn as authored.
        if edge.to_end() == EdgeEnd::Arrow {
            self.draw_arrowhead(frame, g.end, &edge.to_side, g.alpha, head, color, false);
        }
        if edge.from_end() == EdgeEnd::Arrow {
            self.draw_arrowhead(frame, g.start, &edge.from_side, g.alpha, head, color, false);
        }
    }

    pub(super) fn draw_port(&self, frame: &mut Frame, index: usize, side: &str) {
        // Hover state can outlive a node (e.g. deleted by keyboard without
        // moving the mouse); ignore a stale index until the next move.
        let Some(node) = self.nodes.get(index) else {
            return;
        };
        let anchor = side_anchor(self.node_screen_rect(index, node), side);
        let extent = (self.port_radius() + 2.0) * (32.0 / RING_RADIUS);
        frame.draw_image(
            Rectangle::new(
                anchor - Vector::new(extent, extent),
                Size::new(extent * 2.0, extent * 2.0),
            ),
            port_dot(),
        );
    }

    /// Live preview while dragging a connection out of a port: a bezier
    /// from the source anchor to the cursor. When a target node is hovered
    /// the endpoint eases onto its facing side (and back off it when the
    /// cursor leaves the snap zone), driven by [`SnapAnim`].
    pub(super) fn draw_connection_preview(
        &self,
        frame: &mut Frame,
        from: usize,
        side: &str,
        cursor: Point,
        snap: Option<SnapAnim>,
    ) {
        let source = &self.nodes[from];
        let start = side_anchor(self.node_screen_rect(from, source), side);

        let now = Instant::now();
        let snapped = snap
            .filter(|snap| snap.target < self.nodes.len())
            .map(|snap| {
                let rect = self.node_screen_rect(snap.target, &self.nodes[snap.target]);
                let zoom = self.camera.zoom;
                (
                    // Where the link it becomes will end.
                    arrow_tip(
                        side_anchor(rect, snap.side),
                        snap.side,
                        zoom,
                        self.head(ARROW_HEAD * zoom, ARROW_WIDTH * zoom),
                    ),
                    snap.side,
                    crate::camera::ease_out_cubic(snap.strength(now)),
                )
            })
            .filter(|(_, _, strength)| *strength > 0.0);

        let (end, end_side, strength) = match snapped {
            Some((anchor, to_side, strength)) => (
                lerp(cursor, anchor, strength),
                Some(to_side),
                strength,
            ),
            None => (cursor, None, 0.0),
        };

        let reach = start.distance(end) * 0.4;
        let control1 = start + outward(side) * reach;
        let control2 = match end_side {
            Some(to_side) => lerp(cursor, end + outward(to_side) * reach, strength),
            // Free drag: approach as the created edge will — into the node's
            // opposite side — so the curve doesn't snap orientation on release.
            None => end + outward(opposite_side(side)) * reach,
        };

        // Routed as the link it will become, so releasing the drag doesn't
        // change the shape under the cursor.
        let g = EdgeGeometry { start, control1, control2, end, alpha: 1.0 };
        let zoom = self.camera.zoom;
        let head = self.head(ARROW_HEAD * zoom, ARROW_WIDTH * zoom);
        let arrow = end_side.filter(|_| strength > 0.0).map(|side| (end, side, head));
        let (trims, wedges) = shaft([None, arrow], EDGE_LINE_WIDTH * zoom);
        let ink = self.link_ink(EDGE_COLOR);
        // Part of the line, so in its colour — it's the arrowhead that fades in.
        fill_wedges(frame, &wedges, ink);
        frame.stroke(
            &self.link_path_trimmed(None, &g, trims),
            Stroke {
                // Butt, to meet the wedge into its arrowhead — see `shaft`.
                line_cap: stroke::LineCap::Butt,
                line_join: stroke::LineJoin::Round,
                ..Stroke::default()
                    .with_color(ink)
                    .with_width(EDGE_LINE_WIDTH * self.camera.zoom)
            },
        );
        if let (Some(to_side), true) = (end_side, strength > 0.0) {
            self.draw_arrowhead(frame, end, to_side, strength, head, ink, false);
        }

        self.draw_port(frame, from, side);
    }

    /// The head of an arrow pointing into `side` at `point`: the configured
    /// shape, in `color` faded by `alpha`. As a `ring` — the opaque outline
    /// behind a hovered or selected link — a chevron is drawn as before but
    /// wider, and a triangle grows by its stroke on every side.
    pub(super) fn draw_arrowhead(
        &self,
        frame: &mut Frame,
        point: Point,
        side: &str,
        alpha: f32,
        head: Head,
        color: Color,
        ring: bool,
    ) {
        let color = Color { a: color.a * alpha, ..color };
        match head.tip {
            ArrowTip::Chevron => {
                let direction = match side.to_lowercase().as_str() {
                    "left" => 0.0,
                    "right" => PI,
                    "top" => PI * 0.5,
                    "bottom" => PI * 1.5,
                    _ => 0.0,
                };
                let wing = |angle: f32| {
                    Point::new(point.x + head.length * angle.cos(), point.y + head.length * angle.sin())
                };
                let path = Path::new(|builder| {
                    builder.move_to(wing(direction + PI * 0.75));
                    builder.line_to(point);
                    builder.line_to(wing(direction - PI * 0.75));
                });
                frame.stroke(
                    &path,
                    Stroke {
                        line_cap: stroke::LineCap::Round,
                        line_join: stroke::LineJoin::Round,
                        ..Stroke::default().with_color(color).with_width(head.stroke)
                    },
                );
            }
            ArrowTip::Triangle => {
                let (left, right) = head.base(point, side);
                let path = Path::new(|builder| {
                    builder.move_to(point);
                    builder.line_to(left);
                    builder.line_to(right);
                    builder.close();
                });
                frame.fill(&path, color);
                if ring {
                    frame.stroke(
                        &path,
                        Stroke {
                            line_join: stroke::LineJoin::Round,
                            ..Stroke::default().with_color(color).with_width(head.stroke)
                        },
                    );
                }
            }
        }
    }

    /// An arrowhead of the configured shape, `length` long and — a chevron —
    /// `stroke` wide.
    pub(super) fn head(&self, length: f32, stroke: f32) -> Head {
        Head { tip: ArrowTip::of(self.config), length, stroke }
    }

    /// `color` as solid as links are drawn (`config.link_opacity`).
    pub(super) fn link_ink(&self, color: Color) -> Color {
        Color { a: color.a * self.config.link_opacity, ..color }
    }

    /// A link as the cubic pieces it is drawn from, under the configured
    /// routing.
    ///
    /// **Everything a link is goes through here** — the line, its hover and
    /// selection rings, the drag preview, ghost links, the AI shimmer, and the
    /// hit test and caption that have to agree with what was drawn. One routing
    /// to switch, and nothing left behind on the other.
    pub(super) fn link_segments(&self, edge: Option<&CanvasEdge>, g: &EdgeGeometry) -> Vec<EdgeGeometry> {
        match crate::config::LinkStyle::of(self.config) {
            crate::config::LinkStyle::Square => orthogonal_route(g, self.trunk(edge, g)),
            crate::config::LinkStyle::Mermaid => mermaid_route(g, self.trunk(edge, g)),
            crate::config::LinkStyle::Obsidian => vec![*g],
        }
    }

    /// How far out of its port a link steps before turning.
    ///
    /// **Taken from the port, not the link.** Capped per link it is capped by
    /// whichever target is nearest, so siblings at different indents step out by
    /// different amounts and their trunks stand a few pixels apart — four links
    /// on three lines, never merging into one stem. Measured across the whole
    /// port they land on the same line and draw as one.
    ///
    /// A link with no siblings to consult — a preview, a ghost — falls back to
    /// its own span, which is the same answer for a port of one.
    pub(super) fn trunk(&self, edge: Option<&CanvasEdge>, g: &EdgeGeometry) -> f32 {
        let axis = unit(g.control1 - g.start);
        let reach = |anchor: Point| (anchor.x - g.start.x) * axis.x + (anchor.y - g.start.y) * axis.y;
        let nearest = edge
            .and_then(|edge| self.ports.get(&(edge.from_node.as_str(), edge.from_side.as_str())))
            .map(|siblings| {
                siblings.iter().fold(f32::MAX, |lo, &index| {
                    let Some(sibling) = self.edges.get(index) else {
                        return lo;
                    };
                    let Some(&to) = self.node_index.get(sibling.to_node.as_str()) else {
                        return lo;
                    };
                    let anchor = side_anchor(
                        self.node_screen_rect(to, &self.nodes[to]),
                        &sibling.to_side,
                    );
                    let ahead = reach(anchor);
                    // **Only what lies ahead may shorten the step.** A target
                    // level with the port is no distance ahead at all and used
                    // to collapse the trunk onto the node's own edge, taking
                    // every sibling's with it; one *behind* the port can't be
                    // reached by stepping forward, and read through an `abs`
                    // it set a step from a distance in the wrong direction.
                    if ahead > 0.0 { lo.min(ahead) } else { lo }
                })
            })
            .filter(|nearest| nearest.is_finite())
            .unwrap_or_else(|| reach(g.end).max(0.0));
        trunk_gutter(nearest, self.camera.zoom)
    }

    /// A link's drawn path.
    pub(super) fn link_path(&self, edge: Option<&CanvasEdge>, g: &EdgeGeometry) -> Path {
        path_through(&self.link_segments(edge, g))
    }

    /// A link's drawn path with `trims[0]` cut from its start and `trims[1]`
    /// from its end — what [`shaft`] leaves of a line ending in arrowheads.
    ///
    /// **Cut from the routed path, not routed from moved ends.** Sliding the
    /// ends back along the link and routing from there reshaped the whole
    /// curve — the control points pull on all of it — so the line no longer
    /// lay on its own hover ring, which is drawn from the true ends. Cut, it's
    /// the very curve the ring, the hit test and the caption follow, only
    /// shorter.
    pub(super) fn link_path_trimmed(&self, edge: Option<&CanvasEdge>, g: &EdgeGeometry, trims: [f32; 2]) -> Path {
        let mut segments = self.link_segments(edge, g);
        if trims[1] > 0.0
            && let Some(last) = segments.last_mut()
        {
            *last = split_cubic(last, cut_at(last, trims[1], true)).0;
        }
        if trims[0] > 0.0
            && let Some(first) = segments.first_mut()
        {
            *first = split_cubic(first, cut_at(first, trims[0], false)).1;
        }
        path_through(&segments)
    }

    /// How far `point` is from the link as drawn, and the box to reject on
    /// before measuring.
    pub(super) fn link_distance(
        &self,
        point: Point,
        edge: &CanvasEdge,
        g: &EdgeGeometry,
        threshold: f32,
    ) -> Option<f32> {
        let segments = self.link_segments(Some(edge), g);
        if !segments.iter().any(|piece| within_reach(point, piece, threshold)) {
            return None;
        }
        segments
            .iter()
            .map(|piece| bezier_distance(point, piece))
            .fold(None, |best: Option<f32>, d| Some(best.map_or(d, |b| b.min(d))))
    }

    /// The middle of the link as drawn, where its caption sits.
    pub(super) fn link_midpoint(&self, edge: &CanvasEdge, g: &EdgeGeometry) -> Point {
        let segments = self.link_segments(Some(edge), g);
        match segments.len() {
            0 => g.start,
            n => cubic_bezier(&segments[n / 2], 0.5),
        }
    }
}

#[cfg(test)]
mod basis_tests {
    use super::basis_segments;
    use iced::Point;

    #[test]
    fn the_curve_begins_and_ends_on_its_outer_points() {
        // The arrowhead lands on the node and the line leaves the port; the
        // interior waypoints are the only ones the spline merely approaches.
        let points = [
            Point::new(0.0, 0.0),
            Point::new(60.0, 0.0),
            Point::new(140.0, 200.0),
            Point::new(200.0, 200.0),
        ];
        let (opening, segments) = basis_segments(&points);
        let opening = opening.expect("an opening run");
        assert!(opening.x < 20.0 && opening.y.abs() < 0.01, "starts along the first span");
        let last = segments.last().expect("segments").2;
        assert_eq!((last.x, last.y), (200.0, 200.0), "and ends on the final point");
    }

    #[test]
    fn a_corner_is_rounded_rather_than_reached() {
        // The property the route depends on: a basis spline is pulled toward an
        // interior waypoint without arriving at it, so an orthogonal route needs
        // no corner radius of its own.
        let corner = Point::new(100.0, 0.0);
        let points = [Point::new(0.0, 0.0), corner, Point::new(100.0, 100.0)];
        let (_, segments) = basis_segments(&points);
        let reached = segments
            .iter()
            .map(|&(_, _, end)| end.x)
            .fold(f32::MIN, f32::max);
        assert!(reached <= corner.x, "the spline never reaches the corner");
        assert!(
            segments.iter().any(|&(_, _, end)| end.x < corner.x && end.y > 0.0),
            "it turns before it, which is what rounds it",
        );
    }

    #[test]
    fn siblings_sharing_their_first_waypoints_share_their_first_segment() {
        // Why a fan needs no spine of its own: links out of one port share the
        // port and the step out of it, so they are one line until they part.
        let port = Point::new(0.0, 0.0);
        let step = Point::new(60.0, 0.0);
        let (open_a, a) = basis_segments(&[port, step, Point::new(200.0, 300.0)]);
        let (open_b, b) = basis_segments(&[port, step, Point::new(200.0, -300.0)]);
        assert_eq!(open_a, open_b);
        assert_eq!(a[0].0, b[0].0, "same first control point");
        assert_eq!(a[0].1, b[0].1, "and the same second");
    }

    #[test]
    fn degenerate_routes_do_not_panic() {
        assert!(basis_segments(&[]).1.is_empty());
        assert!(basis_segments(&[Point::new(1.0, 2.0)]).1.is_empty());
        assert_eq!(basis_segments(&[Point::new(0.0, 0.0), Point::new(10.0, 10.0)]).1.len(), 1);
    }
}

#[cfg(test)]
mod route_tests {
    use super::{EdgeGeometry, mermaid_route};

    /// The step out of a port, which a board measures from the port's own
    /// links. Fixed here so the route is the only thing under test.
    const GUTTER: f32 = 60.0;
    use iced::Point;

    fn link(from: (f32, f32), reach_out: f32, to: (f32, f32), reach_in: f32) -> EdgeGeometry {
        EdgeGeometry {
            start: Point::new(from.0, from.1),
            control1: Point::new(from.0 + reach_out, from.1),
            control2: Point::new(to.0 - reach_in, to.1),
            end: Point::new(to.0, to.1),
            alpha: 1.0,
        }
    }

    #[test]
    fn the_route_starts_and_ends_where_the_link_does() {
        // The pieces are what everything else measures against — the hit test,
        // the caption's midpoint, the arrowhead's anchor — so the route has to
        // begin at the port and land on the node.
        let g = link((0.0, 0.0), 160.0, (400.0, 300.0), 160.0);
        let pieces = mermaid_route(&g, GUTTER);
        assert_eq!(pieces.first().expect("pieces").start, g.start);
        assert_eq!(pieces.last().expect("pieces").end, g.end);
    }

    #[test]
    fn the_pieces_join_end_to_end() {
        // A gap between two of them would show as a break in the line and as a
        // dead spot in the hit test.
        let g = link((0.0, 0.0), 160.0, (400.0, 300.0), 160.0);
        for pair in mermaid_route(&g, GUTTER).windows(2) {
            assert_eq!(pair[0].end, pair[1].start, "the route must be continuous");
        }
    }

    #[test]
    fn the_step_out_of_the_port_does_not_grow_with_the_span() {
        // The difference from Obsidian's curve, whose reach is 0.4x the distance
        // and so bulges further the further it goes.
        let short = mermaid_route(&link((0.0, 0.0), 60.0, (150.0, 0.0), 60.0), GUTTER);
        let long = mermaid_route(&link((0.0, 0.0), 720.0, (1800.0, 0.0), 720.0), GUTTER);
        let step = |pieces: &[EdgeGeometry]| pieces[1].control1.x;
        assert!(
            (step(&short) - step(&long)).abs() < 1.0,
            "a fixed step: {} vs {}",
            step(&short),
            step(&long),
        );
    }

    #[test]
    fn a_short_link_steps_out_less_than_it_spans() {
        // Or the route would turn back on itself between two close nodes.
        let g = link((0.0, 0.0), 20.0, (50.0, 0.0), 20.0);
        for piece in mermaid_route(&g, GUTTER) {
            for p in [piece.start, piece.control1, piece.control2, piece.end] {
                assert!(p.x >= -0.01 && p.x <= 50.01, "{p:?} is outside the span");
            }
        }
    }
}

#[cfg(test)]
mod orthogonal_tests {
    use super::{EdgeGeometry, orthogonal_route};
    use iced::Point;

    /// The step out of a port, which a board measures from the port's own
    /// links. Fixed here so the route is the only thing under test.
    const GUTTER: f32 = 60.0;

    /// A link between two ports, each named by the way it faces.
    fn link(from: (f32, f32), facing: (f32, f32), to: (f32, f32), entering: (f32, f32)) -> EdgeGeometry {
        let (start, end) = (Point::new(from.0, from.1), Point::new(to.0, to.1));
        EdgeGeometry {
            start,
            control1: start + iced::Vector::new(facing.0, facing.1) * 100.0,
            control2: end + iced::Vector::new(entering.0, entering.1) * 100.0,
            end,
            alpha: 1.0,
        }
    }

    const RIGHT: (f32, f32) = (1.0, 0.0);
    const DOWN: (f32, f32) = (0.0, 1.0);
    const LEFT: (f32, f32) = (-1.0, 0.0);

    #[test]
    fn every_run_is_square_to_the_last() {
        // The point of the style: no diagonals anywhere.
        for piece in orthogonal_route(&link((0.0, 0.0), RIGHT, (400.0, 300.0), LEFT), GUTTER) {
            let (dx, dy) = (piece.end.x - piece.start.x, piece.end.y - piece.start.y);
            assert!(dx.abs() < 0.01 || dy.abs() < 0.01, "not level or plumb: {dx} x {dy}");
        }
    }

    #[test]
    fn the_route_joins_up_between_its_ports() {
        let g = link((0.0, 0.0), RIGHT, (400.0, 300.0), LEFT);
        let pieces = orthogonal_route(&g, GUTTER);
        assert_eq!(pieces.first().expect("pieces").start, g.start);
        assert_eq!(pieces.last().expect("pieces").end, g.end);
        for pair in pieces.windows(2) {
            assert_eq!(pair[0].end, pair[1].start, "the route must be continuous");
        }
    }

    #[test]
    fn the_long_run_stands_off_the_source_not_across_its_targets() {
        // The bug: routed off the source's normal alone, a link out of a bottom
        // port went down a little, across to its target's column, then ran the
        // long way *down that column* — flush against every node in it.
        let column = 1113.0;
        let g = link((605.0, 415.0), DOWN, (column, 1053.0), LEFT);
        let longest = orthogonal_route(&g, GUTTER)
            .into_iter()
            .max_by(|a, b| a.start.distance(a.end).total_cmp(&b.start.distance(b.end)))
            .expect("a run");
        assert!((longest.start.x - longest.end.x).abs() < 0.01, "the long run is the vertical");
        assert!(
            (longest.start.x - column).abs() > GUTTER,
            "it must not graze the column it feeds: {} against {column}",
            longest.start.x,
        );
        assert!((longest.start.x - 605.0).abs() < 0.01, "it stands on the port's own line");
    }

    #[test]
    fn siblings_share_the_trunk_they_branch_from() {
        // Why it reads as one stem: the step is the port's, not the link's, so
        // links to targets at different indents turn on the same line.
        let near = orthogonal_route(&link((0.0, 0.0), RIGHT, (200.0, -300.0), LEFT), GUTTER);
        let far = orthogonal_route(&link((0.0, 0.0), RIGHT, (600.0, 300.0), LEFT), GUTTER);
        assert_eq!(near[0].end, far[0].end, "one trunk, whatever the target");
        assert_eq!(near[0].end.x, GUTTER, "standing where the port says");
    }

    #[test]
    fn a_target_dead_ahead_is_one_run_with_no_kink_in_it() {
        // Corners that bend through nothing are dropped, so a link with nothing
        // to avoid is a straight line rather than three runs pretending.
        let pieces = orthogonal_route(&link((0.0, 0.0), RIGHT, (400.0, 0.0), LEFT), GUTTER);
        assert_eq!(pieces.len(), 1);
        assert_eq!(pieces[0].start, Point::new(0.0, 0.0));
        assert_eq!(pieces[0].end, Point::new(400.0, 0.0));
    }

    #[test]
    fn a_link_into_a_top_port_arrives_plumb() {
        // The final stub follows the target's normal, so it enters the way the
        // port faces rather than sideways through the node's edge.
        const UP: (f32, f32) = (0.0, -1.0);
        let g = link((0.0, 0.0), RIGHT, (400.0, 300.0), UP);
        let last = orthogonal_route(&g, GUTTER).pop().expect("a run");
        assert!((last.start.x - last.end.x).abs() < 0.01, "arrives vertically");
        assert!(last.start.y < last.end.y, "and from above");
    }
}

#[cfg(test)]
mod trunk_tests {
    use super::{EDGE_GUTTER, MIN_TRUNK_GUTTER, trunk_gutter};

    #[test]
    fn a_trunk_never_stands_on_the_node_it_leaves() {
        // The bug: a sibling whose target is level with the port is no distance
        // ahead, which collapsed the step to nothing and put the vertical bar
        // flat against the parent's edge — and took every sibling's with it,
        // since the step belongs to the port.
        for ahead in [0.0, 1.0, 5.0, 20.0, 60.0, 400.0] {
            assert!(
                trunk_gutter(ahead, 1.0) > 0.0,
                "{ahead} ahead put the trunk on the node",
            );
        }
        // Nothing ahead is no cap at all, not a cap of nothing: there is
        // nothing to overshoot, so the step is the full one.
        assert_eq!(trunk_gutter(0.0, 1.0), EDGE_GUTTER);
        assert_eq!(trunk_gutter(-200.0, 1.0), EDGE_GUTTER, "nor does anything behind");
        assert!(trunk_gutter(1.0, 1.0) >= MIN_TRUNK_GUTTER.min(0.5));
    }

    #[test]
    fn a_trunk_never_oversteps_the_target_it_heads_for() {
        // The other bound. Where the two can't both be had — a target closer
        // than the minimum clearance — it splits the difference.
        for ahead in [1.0, 4.0, 10.0, 25.0, 300.0] {
            assert!(
                trunk_gutter(ahead, 1.0) < ahead,
                "{ahead} ahead overshot",
            );
        }
    }

    #[test]
    fn the_step_is_the_same_distance_on_the_board_at_any_zoom() {
        // Held in screen units it shrank in world terms as you zoomed in,
        // walking the trunk toward the node it came out of.
        for zoom in [0.25_f32, 1.0, 4.0] {
            let world = trunk_gutter(1000.0 * zoom, zoom) / zoom;
            assert!((world - EDGE_GUTTER).abs() < 0.01, "{zoom}x gave {world}");
        }
    }

    #[test]
    fn a_close_target_still_pulls_the_trunk_in() {
        // It is a cap, not a fixed distance: a near target has to leave room.
        assert!(trunk_gutter(60.0, 1.0) < trunk_gutter(600.0, 1.0));
    }
}

#[cfg(test)]
mod cut_tests {
    use super::{EdgeGeometry, cubic_at, cut_at, split_cubic};
    use iced::Point;

    #[test]
    fn a_cut_line_is_the_same_curve_only_shorter() {
        // A link that leaves rightwards and arrives from the left, as a
        // curve between two ports does.
        let piece = EdgeGeometry {
            start: Point::new(0.0, 0.0),
            control1: Point::new(120.0, 0.0),
            control2: Point::new(80.0, 90.0),
            end: Point::new(200.0, 90.0),
            alpha: 1.0,
        };
        let at = cut_at(&piece, 10.0, true);
        let (kept, _) = split_cubic(&piece, at);
        assert!((kept.end.distance(piece.end) - 10.0).abs() < 0.01, "cut where asked");
        // Every point of what's kept lies on the original: moving the ends
        // instead had reshaped the whole curve off its hover ring.
        for step in 0..=10 {
            let s = step as f32 / 10.0;
            assert!(cubic_at(&kept, s).distance(cubic_at(&piece, s * at)) < 0.01);
        }
        // From the start, the same.
        let (_, rest) = split_cubic(&piece, cut_at(&piece, 10.0, false));
        assert!((rest.start.distance(piece.start) - 10.0).abs() < 0.01);
    }
}
