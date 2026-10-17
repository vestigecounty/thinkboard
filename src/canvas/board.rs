//! The canvas: rendering of cards/images/edges and all mouse interaction.
//!
//! Everything is drawn in *screen space* (points transformed through
//! [`Camera`]) rather than via `frame.scale()`, because scaled canvas text
//! does not scale its wrapping width — screen-space drawing keeps card text
//! wrapping stable across zoom levels.

use std::collections::{HashMap, HashSet};
use std::f32::consts::{PI, SQRT_2};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use iced::advanced::text::{Alignment as TextAlignment, LineHeight, Shaping};
use iced::alignment::Vertical;
use iced::keyboard;
use iced::mouse::{self, Button, ScrollDelta};
use iced::widget::canvas::{
    self, Event, Fill, Frame, Geometry, Path, Stroke, Style, gradient, stroke,
};
use iced::border::Radius;
use iced::widget::image;
use iced::{Color, Point, Rectangle, Renderer, Size, Theme, Vector};

use crate::Message;
use crate::camera::Camera;
use crate::config::ArrowTip;
use super::geometry::{
    EdgeGeometry, bezier_distance, covers_any, cubic_bezier, stacking_planes, within_reach,
};
use super::layered::Stacked;
use crate::document::IntoColor;
use crate::model::{CanvasEdge, CanvasNode, EdgeEnd, NodeKind};
use crate::theme::Theme as ColorTheme;

/// Default outline for uncolored nodes/clusters (not themed).
const CARD_BORDER: Color = Color::from_rgb(0.45, 0.45, 0.45);
/// Default node text color, and the fallback used by the app's menu chrome.
/// Node rendering uses the (possibly themed) color from `Board::theme`.
pub const TEXT_COLOR: Color = Color::from_rgb(0.86, 0.87, 0.87);
/// A link's colour when it has none of its own. Opaque: how solid a link is
/// drawn is `config.link_opacity`, for every link alike.
const EDGE_COLOR: Color = Color::from_rgb(0.35, 0.62, 1.0);

/// Point size (world px) of a link's caption, and the zoom below which drawing
/// one is pointless — the text is sub-pixel and the board is heading for
/// cluster mode anyway.
const EDGE_LABEL_SIZE: f32 = 12.0;
const EDGE_LABEL_MIN_ZOOM: f32 = 0.45;
/// Lavender sheen swept along the chain feeding a pending AI request, echoing
/// the default response-node tint (Obsidian's purple).
const SHIMMER: Color = Color::from_rgba(0.80, 0.74, 0.97, 1.0);
/// Seconds for one shimmer sweep across the chain.
const SHIMMER_SWEEP: f32 = 1.15;
/// Quiet seconds between successive sweeps.
const SHIMMER_GAP: f32 = 1.0;
/// Number of sweeps before the effect settles onto the response node.
const SHIMMER_PULSES: f32 = 2.0;
/// Seconds per breath of the glow that settles on the response node.
const SHIMMER_BREATHE: f32 = 1.7;
/// Size (world px) of a ghost card's top-right "×" dismiss zone.
const GHOST_X: f32 = 24.0;

/// A search hit: highlighter yellow, and black on it. **Deliberately not
/// themed.** A find highlight is a convention rather than a surface — it means
/// the same thing in a browser, an editor and a PDF reader, and it reads at a
/// glance because it's always these colours. Deriving it from a palette makes
/// every theme's hit a slightly different, usually weaker, compromise.
/// The hit being stepped through wears it at full strength; the rest are the
/// same yellow dimmed. One hue at two brightnesses, because two *hues* (yellow
/// against an orange) read as two kinds of highlight rather than one picked out
/// of several, and an outline round the current hit lands unevenly on
/// monospace, where the hit's box and its glyphs rarely line up.
const SEARCH_HIT_CURRENT: Color = Color::from_rgb(1.0, 0.882, 0.302);
/// 55% of it — and that is the floor, held by the same two thresholds
/// `core::theme` uses. Black ink on this is 5.0:1 (body text wants 4.5) and it
/// stands 3.4:1 off a card (a UI mark wants 3.0). At 50% the ink drops to
/// 4.3:1 and the fill to 2.9:1, so a dim hit stops reading as a highlight and
/// starts reading as a smudge on the card.
const SEARCH_HIT: Color = Color::from_rgb(0.55, 0.485, 0.166);
const SEARCH_INK: Color = Color::BLACK;

/// Planes of overlapping cards stacked above the base scene before the rest
/// share the top one — see [`stacking_planes`]. Each costs a renderer
/// layer, but only while some card is that deep in a pile.
const LIFTED_PLANES: usize = 3;

/// The board's cached scene: the base — frames, links, clusters and every card
/// that overlaps nothing beneath it — and a plane per level of a pile of
/// overlapping cards above it.
pub struct SceneCache {
    base: canvas::Cache,
    lifted: [canvas::Cache; LIFTED_PLANES],
    /// How many lifted planes the last rebuild filled.
    lifted_in_use: std::cell::Cell<usize>,
}

impl SceneCache {
    pub fn new() -> Self {
        Self {
            base: canvas::Cache::new(),
            lifted: std::array::from_fn(|_| canvas::Cache::new()),
            lifted_in_use: std::cell::Cell::new(0),
        }
    }

    /// Invalidates every plane together: anything that moves a card can move
    /// it between planes.
    pub fn clear(&self) {
        self.base.clear();
        for plane in &self.lifted {
            plane.clear();
        }
    }
}

/// The command palette's chords, as the empty board's hint prints them —
/// Cmd on macOS and Ctrl elsewhere, as [`chord`] reads them. ⌘⇧P works too,
/// for VS Code's habit, but two is as many as a hint should name.
#[cfg(target_os = "macos")]
const PALETTE_CHORDS: &str = "⌘K or ⌘P";
#[cfg(not(target_os = "macos"))]
const PALETTE_CHORDS: &str = "Ctrl+K or Ctrl+P";
/// What a Cmd/Ctrl chord does. Separated from the event so the table can be
/// read — and tested — without a live board: which chords take Shift, and which
/// need something selected, is the part that is easy to get subtly wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Chord {
    Undo,
    Redo,
    Copy,
    Paste,
    Find,
    Palette,
    SwitchCanvas,
    ZoomIn,
    ZoomOut,
    Fit,
}

/// How far ⌘= and ⌘− zoom, per press.
const ZOOM_KEY_STEP: f32 = 1.25;

/// The chord for a latin `letter`, or `None` if the board doesn't claim it —
/// in which case the key falls through untouched.
fn chord(letter: char, shift: bool, has_selection: bool) -> Option<Chord> {
    Some(match (letter, shift) {
        ('z', false) => Chord::Undo,
        ('z', true) => Chord::Redo,
        // Nothing selected is nothing to copy, and the chord should reach
        // whatever else wants it rather than be swallowed.
        ('c', _) if has_selection => Chord::Copy,
        ('v', _) => Chord::Paste,
        ('f', _) => Chord::Find,
        // The command palette, on every chord someone arrives with: ⌘P from
        // Obsidian, ⌘K from the web's command menus, ⌘⇧P from Sublime and VS
        // Code. Board only, as every chord here is — in a note, ⌘K is the
        // editor's to make a link with, as it is in Obsidian.
        ('p', _) | ('k', false) => Chord::Palette,
        // Switch boards: the vault browser where a vault is configured, the
        // ordinary open dialog where one isn't.
        ('o', false) => Chord::SwitchCanvas,
        // Zoom as a browser does: = (and the + above it), −, and 0 for all of it.
        ('=' | '+', _) => Chord::ZoomIn,
        ('-' | '_', _) => Chord::ZoomOut,
        ('0', _) => Chord::Fit,
        _ => return None,
    })
}

/// Weight of a suggestion card's dashed outline, in world units.
const GHOST_STROKE: f32 = 1.5;
/// Line color for a hovered link — a muted blue.
const EDGE_HOVER: Color = Color::from_rgb(0.2625, 0.595, 0.875);
/// Selected link: an electric, highly saturated blue — noticeably more vivid
/// than [`EDGE_HOVER`] (lower red widens the channel spread for more
/// saturation, without adding brightness) so selection reads distinctly.
const EDGE_HIGHLIGHT: Color = Color::from_rgb(0.11, 0.66, 1.0);
/// Opaque ring drawn behind a hovered/selected link — a darker shade of the
/// accent, so the vivid line reads on top. Opaque (not translucent) so the
/// color stays true and overlapping strokes don't compound. The selected ring
/// is more saturated than the hover ring, matching the line hierarchy.
const EDGE_HOVER_RING: Color = Color::from_rgb(0.114, 0.2625, 0.42);
const EDGE_HIGHLIGHT_RING: Color = Color::from_rgb(0.06, 0.32, 0.54);
const FONT_SIZE: f32 = 17.0;



/// Outline weight of a *selected* card, and the floor it holds at when zoomed
/// out. Shared by every card kind so a selection reads the same wherever it
/// lands; the unselected weights differ by kind and stay where they're used.
const SELECTED_BORDER: f32 = 3.5;
const SELECTED_BORDER_MIN: f32 = 3.0;

/// Length of an edge's arrowhead, in world units. Shared by the ordinary and
/// the highlighted edge — only their line weights differ.
const ARROW_HEAD: f32 = 8.0;
/// The arrowhead's stroke, in world units. Its round cap reaches half of it
/// past the tip.
const ARROW_WIDTH: f32 = 2.0;
/// The clear space between an arrow's tip and the card it points at, in world
/// units. On the card's edge, the tip sat on its border: a link the colour of
/// a card's border lost its tip in it, and links fanning into one card merged
/// into its outline. In world units so it scales with the board — about 2 px
/// at 100%, under a pixel zoomed out, where a fixed gap made links look detached
/// from what they point at.
const ARROW_GAP: f32 = 2.0;

/// An arrowhead as drawn: its shape, how far a chevron's wings reach back
/// along the link, and — for a chevron — the width of its stroke, all on
/// screen. A triangle is sized from the same length; see [`Head::reach`].
#[derive(Clone, Copy)]
struct Head {
    tip: ArrowTip,
    length: f32,
    stroke: f32,
}

/// A triangle's length against a chevron's, and its half-width against its
/// own length. At a chevron's length, filled, it read as a speck at the end
/// of the line: these are the proportions of the design it was drawn from —
/// about five times the line's width long, two and a half either side of it.
const TRIANGLE_LENGTH: f32 = 1.625;
const TRIANGLE_SPREAD: f32 = 0.5;

impl Head {
    /// How far the head reaches back from its point along the link.
    fn reach(self) -> f32 {
        match self.tip {
            ArrowTip::Chevron => self.length,
            ArrowTip::Triangle => self.length * TRIANGLE_LENGTH,
        }
    }

    /// How far past the point the drawn head reaches: a chevron's round cap,
    /// half its stroke; a triangle comes to the point itself.
    fn overhang(self) -> f32 {
        match self.tip {
            ArrowTip::Chevron => self.stroke / 2.0,
            ArrowTip::Triangle => 0.0,
        }
    }

    /// A triangle's base corners, for a head pointing into `side` at `point`.
    fn base(self, point: Point, side: &str) -> (Point, Point) {
        let out = outward(side);
        let across = Vector::new(-out.y, out.x) * (self.reach() * TRIANGLE_SPREAD);
        let base = point + out * self.reach();
        (base + across, base - across)
    }
}

/// Where an arrow pointing into `side` of a card at `anchor` puts its point:
/// [`ARROW_GAP`] clear of the card, measured from what's drawn — a chevron's
/// round cap reaches past its point — not from the point.
fn arrow_tip(anchor: Point, side: &str, zoom: f32, head: Head) -> Point {
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
fn shaft(ends: [Option<(Point, &str, Head)>; 2], line: f32) -> ([f32; 2], Vec<[Point; 3]>) {
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
fn cubic_at(piece: &EdgeGeometry, t: f32) -> Point {
    split_cubic(piece, t).0.end
}

/// `piece` split at `t` (de Casteljau): the part before and the part after,
/// each the very curve it was a part of.
fn split_cubic(piece: &EdgeGeometry, t: f32) -> (EdgeGeometry, EdgeGeometry) {
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
fn cut_at(piece: &EdgeGeometry, distance: f32, from_end: bool) -> f32 {
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
fn fill_wedges(frame: &mut Frame, wedges: &[[Point; 3]], color: Color) {
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

/// The smallest a drawn glyph is allowed to get. Below this, text on a card
/// zoomed out is a smudge rather than something you can read the shape of.
const MIN_TEXT_PX: f32 = 6.0;

/// How far a cluster drag moves this frame: from the anchor's current position
/// to where the cursor wants it, snapped.
///
/// **This is why cluster snapping exists at all.** Every member ends up at
/// `target + (member - anchor)`, so with `target` on the grid a member stays on
/// it exactly when its offset from the anchor is a whole number of cells —
/// automatic if they were all aligned to begin with. Dragging used to apply the
/// raw cursor delta, which left an aligned cluster sitting at whatever fraction
/// of a cell the mouse stopped at.
///
/// It has to be the *absolute* target that's snapped, not the per-frame step.
/// A step is a few pixels: rounding it floors to zero, so the cluster never
/// moves, or jumps a whole cell — and the rounding accumulates into a drift
/// away from the cursor.
fn cluster_delta(want: Point, anchor: Point, grid: f32) -> Vector {
    let (x, y) = crate::snap::point(want.x, want.y, grid);
    Point::new(x, y) - anchor
}

/// The member node a cluster drag is measured against.
///
/// Snapping has to move the *nodes* onto the grid, not the cluster's box: the
/// box is derived from its members plus padding and is recomputed the moment
/// the drag ends, so aligning it would leave the stored geometry at an
/// arbitrary offset — and would knock nodes that were already on the grid off
/// it. Measuring against one member keeps every member's alignment.
///
/// Topmost then leftmost, so the choice is deterministic and the reference
/// can't change part-way through a drag.
fn cluster_anchor(
    clusters: &[crate::cluster::Cluster],
    nodes: &[CanvasNode],
    cluster: usize,
) -> Option<usize> {
    clusters
        .get(cluster)?
        .members
        .iter()
        .copied()
        .filter(|&i| i < nodes.len())
        .min_by(|&a, &b| {
            let (a, b) = (&nodes[a], &nodes[b]);
            (a.y, a.x)
                .partial_cmp(&(b.y, b.x))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
}

/// Where a search hit falls inside one laid-out run: its byte range, and the
/// logical offset and width of the glyphs it covers.
///
/// Both passes need it — the fill is drawn from the geometry, and the text is
/// split on the byte range so the glyphs over the fill can change colour — so
/// it's measured once and used twice.
#[derive(Clone, Copy)]
struct Hit {
    start: usize,
    end: usize,
    x: f32,
    width: f32,
}

/// A rounded-rect path at a screen rect. The radius comes from the theme —
/// `Board::card_radius` for a card, `frame_radius` for the shapes that contain
/// cards — so shape is themeable alongside colour.
fn rounded_path(rect: Rectangle, radius: f32) -> Path {
    Path::rounded_rectangle(rect.position(), rect.size(), radius.into())
}

/// Outline weight for a selected card. See [`SELECTED_BORDER`].
fn selected_border(zoom: f32) -> f32 {
    (SELECTED_BORDER * zoom).max(SELECTED_BORDER_MIN)
}

/// `color` at `alpha` of its own opacity — the fade every card layer applies
/// as it crossfades in, spelled once.
fn fade(color: Color, alpha: f32) -> Color {
    Color { a: color.a * alpha, ..color }
}

const DOUBLE_CLICK_TIME: Duration = Duration::from_millis(400);
const DOUBLE_CLICK_DISTANCE: f32 = 6.0;

/// Rendered radius of each snap dot (screen px).
const SNAP_DOT_RADIUS: f32 = 1.5;
/// Peak opacity of a snap dot (nearest a dragged node); scaled by the falloff.
const SNAP_DOT_ALPHA: f32 = 0.55;
/// How many rings of snap dots surround a node (halo width, in grid steps) —
/// the "around" region. Grid-relative so it's a consistent number of dots at
/// any snap distance; the outermost ring is faintest (falloff).
const SNAP_NODE_RINGS: f32 = 3.5;
/// Below this on-screen grid spacing (px) the grid is too dense to be useful,
/// so the dots are hidden (zoomed too far out).
const SNAP_DOT_MIN_SPACING: f32 = 6.0;

/// Cluster summaries become the interactive view below `config.cluster_zoom`.
/// Rendering crossfades across [`CLUSTER_FADE_NONE`, `CLUSTER_FADE_FULL`]
/// around it. (Wheel/pinch zoom sensitivity are in config too.)
/// At/below this zoom clusters are fully opaque; at/above `CLUSTER_FADE_NONE`
/// they are gone — between, clusters and nodes crossfade.
const CLUSTER_FADE_FULL: f32 = 0.35;
const CLUSTER_FADE_NONE: f32 = 0.45;

/// Rendering weight of clusters (1) vs nodes (0) at the given zoom.
pub fn cluster_opacity(zoom: f32) -> f32 {
    let t = ((CLUSTER_FADE_NONE - zoom) / (CLUSTER_FADE_NONE - CLUSTER_FADE_FULL)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t) // smoothstep
}

/// The active board search, passed to [`Board`] while the find bar is open.
pub struct SearchView<'a> {
    /// The current query (case-insensitive substring, highlighted in note text).
    pub query: &'a str,
    /// Which hit is the one being stepped through: the node it's in, and how
    /// many of that node's hits come before it.
    ///
    /// An ordinal rather than an offset because the two searches count in
    /// different text — `find_matches` scans the node's raw Markdown, while the
    /// highlight scans the laid-out runs, which have the markers stripped. The
    /// *positions* don't correspond; the reading order does.
    pub current_hit: Option<(usize, usize)>,
    /// The cards holding a match, sorted. While there are any, every other
    /// card dims — see [`search_dim`].
    pub cards: &'a [usize],
}

/// How strongly a card is drawn while the find bar has matches: in full if it
/// holds one, faint if it doesn't, so the matches are what the eye lands on
/// and the board around them still reads. With no matches nothing dims —
/// there'd be nothing for it to point at.
pub fn search_dim(cards: &[usize], index: usize) -> f32 {
    /// Far enough back that a match stands out from across the board.
    const DIMMED: f32 = 0.32;
    if cards.is_empty() || cards.binary_search(&index).is_ok() { 1.0 } else { DIMMED }
}

pub struct Board<'a> {
    pub nodes: &'a [CanvasNode],
    /// Node id → index, built once per frame so edge endpoint lookups are
    /// O(1) rather than a linear scan per edge (draw + hit-testing).
    pub node_index: HashMap<&'a str, usize>,
    /// Link indices grouped by the port they leave, built once per frame beside
    /// [`Board::node_index`] — what a shared trunk needs to know, since where it
    /// stands belongs to the port and not to any one link.
    pub ports: HashMap<(&'a str, &'a str), Vec<usize>>,
    pub edges: &'a [CanvasEdge],
    pub images: &'a HashMap<String, image::Handle>,
    /// Body text of the file nodes that embed a Markdown note, keyed by node id.
    /// Absent when the note couldn't be read; the card then shows just its title.
    pub notes: &'a HashMap<String, String>,
    pub clusters: &'a [crate::cluster::Cluster],
    pub cluster_links: &'a [crate::cluster::ClusterLink],
    pub cluster_snapshots: &'a HashMap<usize, crate::text_raster::Snapshot>,
    pub config: &'a crate::config::Config,
    /// Node/card colors, optionally themed from an Obsidian `theme.css`.
    pub theme: &'a ColorTheme,
    pub camera: Camera,
    /// Top safe-area inset (px) — the macOS notch / menu-bar height, so the
    /// top wayfinding button isn't clipped. 0 off macOS or where there's none.
    pub top_safe_inset: f32,
    pub cache: &'a SceneCache,
    pub editing: Option<usize>,
    pub overlay: bool,
    /// An in-board modal (the settings panel) is up: board keyboard shortcuts
    /// are suppressed so the modal owns input.
    pub modal: bool,
    /// A right-click menu is open: Esc closes it before it does anything else.
    pub context_menu: bool,
    pub show_fps: bool,
    /// Spawn animation start times of freshly created nodes.
    pub spawning: &'a HashMap<usize, Instant>,
    /// Pre-rasterized text bitmaps, the primary way node text is drawn;
    /// GPU-scaled so zoom never re-rasterizes glyphs.
    pub text_snapshots: &'a HashMap<usize, crate::text_raster::Snapshot>,
    /// Replacement bitmaps still warming their GPU upload; drawn invisibly
    /// so the texture is resident before the visible swap.
    pub staging_snapshots: &'a HashMap<usize, crate::StagedSnapshot>,
    /// Vertical scroll offset (world px) per node id, for nodes shrunk below
    /// their content with `node_scrollbars` on. Clamped to the overflow on use.
    pub node_scroll: &'a HashMap<String, f32>,
    /// Overlay entrance/exit progress: 0 = hidden, 1 = fully shown.
    pub reveal: f32,
    /// Currently selected nodes (highlighted border; deletable).
    pub selected: &'a [usize],
    /// Currently selected edge (highlighted; deletable).
    pub selected_edge: Option<usize>,
    /// Edge under the cursor (highlighted). App state so the cached edge
    /// layer can render it behind the nodes.
    pub hovered_edge: Option<usize>,
    /// The in-board find, while its bar is open: the query to highlight in note
    /// text and the node holding the current match (emphasized).
    pub search: Option<SearchView<'a>>,
    /// Pending AI requests, each shimmered independently (its own clock) so
    /// concurrent generations don't share a sweep phase.
    pub ai_flows: &'a [AiFlow],
    /// Copilot-style suggestion cards awaiting accept/dismiss.
    pub ghosts: &'a [Ghost],
    /// Opacity of the (non-placeholder) ghosts, fading toward 0 as they near
    /// their auto-dismiss.
    pub ghost_fade: f32,
    /// Fade-in factor (0→1) applied to all ghost cards as they appear.
    pub ghost_fade_in: f32,
    /// Whether a ghost was under the cursor last frame (to emit hover changes).
    pub ghost_hovered: bool,
}

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

/// Bezier geometry of an edge in screen space.
/// Redraw pacing stats for the `--fps` meter. Measured draw-to-draw, so it
/// reflects the redraw rate the user actually perceives during interaction.
/// Redraws farther apart than this are treated as separate interaction bursts
/// (sparse hover), not stalls, so their idle gap isn't counted as a slow frame.
const CONTINUOUS_GAP_MS: f32 = 100.0;

struct FrameStats {
    last: Option<Instant>,
    ema_ms: f32,
    worst_ms: f32,
    /// EMA of the CPU time spent building the board geometry (the cached
    /// tessellation closure). Near zero when the cache is reused; rises during
    /// pan/zoom when it regenerates every frame.
    build_ms: f32,
    /// EMA of per-frame Markdown-layout shaping time (live text) and the layout
    /// cache hit rate. Shaping time falls to ~0 once the cache warms; hit rate
    /// drops while editing (text changes force re-shaping).
    layout_ms: f32,
    hit_rate: f32,
    prev_hits: u64,
    prev_misses: u64,
    prev_shape_ns: u64,
}

static FRAME_STATS: Mutex<FrameStats> = Mutex::new(FrameStats {
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

#[derive(Default)]
pub struct Interaction {
    kind: Kind,
    modifiers: keyboard::Modifiers,
    last_click: Option<(Instant, Point)>,
    /// Left press that landed on empty space; a release nearby dismisses
    /// the overlay.
    background_press: Option<Point>,
    /// Side of the node the cursor is hovering near; shows a port dot.
    hover: Option<(usize, &'static str)>,
    /// Snap animation of the connection preview toward a target node.
    snap: Option<SnapAnim>,
    /// Active rubber-band (marquee) selection dragged from empty space.
    marquee: Option<Marquee>,
    /// Link URL under the current press on an already-selected node; opened on
    /// release if the press didn't turn into a drag. Held here (not on `Kind`,
    /// which is `Copy`) to keep the owned `String` off the interaction enum.
    link: Option<String>,
    /// `tb_action` of a button node under the current press (e.g. the scratch
    /// board's "Create a board"); fired on release. Like `link`, an owned
    /// `String` kept off the `Copy` `Kind`.
    action: Option<String>,
    reported_viewport: Option<Size>,
}

/// A rubber-band selection in progress.
struct Marquee {
    /// Press and current cursor, in screen coordinates.
    start: Point,
    current: Point,
    /// Selection to union the marquee with (non-empty for an additive
    /// Cmd-drag), so it extends rather than replaces.
    base: Vec<usize>,
}

/// Seconds for the preview to snap onto / release from a target node.
const SNAP_DURATION: f32 = 0.25;

/// Leeway around a node (screen px) within which a dragged connection
/// snaps to it.
const SNAP_MARGIN: f32 = 24.0;

#[derive(Clone, Copy)]
struct SnapAnim {
    target: usize,
    side: &'static str,
    /// Strength when the direction last changed; strength grows/shrinks
    /// linearly from here, clamped to [0, 1].
    strength_at_change: f32,
    changed: Instant,
    /// +1 while snapping onto the target, -1 while releasing.
    direction: f32,
}

impl SnapAnim {
    fn strength(&self, now: Instant) -> f32 {
        let elapsed = now.duration_since(self.changed).as_secs_f32();
        (self.strength_at_change + self.direction * elapsed / SNAP_DURATION).clamp(0.0, 1.0)
    }

    fn settled(&self, now: Instant) -> bool {
        let strength = self.strength(now);
        (self.direction > 0.0 && strength >= 1.0) || (self.direction < 0.0 && strength <= 0.0)
    }
}

#[derive(Default, Clone, Copy)]
enum Kind {
    #[default]
    Idle,
    Panning {
        last: Point,
        /// Press position, to tell a right-click (context menu) from a
        /// right-drag (pan) on release.
        start: Point,
    },
    Dragging {
        index: usize,
        grab: Vector,
        /// Press position (screen), to tell a link-click from a drag.
        press: Point,
        /// Set once the cursor moves past the click tolerance — then it's a
        /// node drag, not a link click.
        moved: bool,
    },
    /// Dragging a ghost suggestion card. A press that doesn't move accepts it;
    /// a drag repositions it (without accepting).
    DraggingGhost {
        index: usize,
        /// World offset from the card's top-left to the cursor.
        grab: Vector,
        /// Press position (screen), to tell a click from a drag.
        press: Point,
        moved: bool,
    },
    /// Dragging a new connection out of a node's side port.
    Connecting {
        from: usize,
        side: &'static str,
        cursor: Point,
    },
    /// Dragging a node corner; the opposite corner stays anchored.
    Resizing {
        index: usize,
        corner: Corner,
        /// Opposite corner, world coordinates.
        anchor: Point,
    },
    /// Dragging a cluster card (cluster mode); moves all member nodes.
    DraggingCluster {
        cluster: usize,
        /// The member the drag is measured against, so it can snap to the grid
        /// the way a node drag does. See [`cluster_anchor`].
        anchor: usize,
        /// Cursor offset from the anchor when the drag began, world units.
        grab: Vector,
    },
    /// Dragging a node's scrollbar thumb (node scrollbars on, content overflows).
    ScrollingNode {
        index: usize,
        /// Scroll offset (world px) at press.
        start_scroll: f32,
        /// Press cursor y (screen), against which drag delta is measured.
        press_y: f32,
        /// World scroll per screen px dragged (`overflow / track`).
        scale: f32,
        /// World overflow, to clamp the offset.
        overflow: f32,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl Corner {
    fn interaction(self) -> mouse::Interaction {
        match self {
            // "/" diagonal vs "\" diagonal.
            Corner::TopRight | Corner::BottomLeft => mouse::Interaction::ResizingDiagonallyUp,
            Corner::TopLeft | Corner::BottomRight => mouse::Interaction::ResizingDiagonallyDown,
        }
    }
}

/// Grab distance (screen px) around a node corner for resizing.
const CORNER_GRAB: f32 = 18.0;

/// Width (screen px, pre-zoom) of a node's scrollbar thumb.
const NODE_SCROLLBAR_WIDTH: f32 = 6.0;
/// Total vertical padding (world px) around a text node's text, split top/bottom
/// — the shared `NODE_PAD_Y`, so a fitted node has no overflow and a scrolled one
/// keeps padding at both ends.
const NODE_TEXT_PAD_Y: f32 = crate::text_raster::NODE_PAD_Y;
/// Horizontal inset (world px, per side) of a reference card's title and body,
/// matching where the Markdown renderer puts a text node's first column.
const NODE_TEXT_PAD_X: f32 = crate::text_raster::NODE_PAD_X / 2.0 + crate::text_raster::MD_PADDING;

/// Point size (world px) of a group frame's caption, and its inset from the
/// frame's top-left corner.
pub(crate) const GROUP_LABEL_SIZE: f32 = 15.0;
pub(crate) const GROUP_LABEL_PAD: f32 = 10.0;
/// Height (world px) of a frame's grabbable header band — the caption plus its
/// padding above and below.
pub(crate) const GROUP_HEADER_HEIGHT: f32 = GROUP_LABEL_SIZE + GROUP_LABEL_PAD * 2.0;

/// Point size (world px) of the file-name header on a note/attachment card.
/// Smaller than the body so it reads as a label naming the file rather than as
/// the note's own first line.
const REFERENCE_TITLE_SIZE: f32 = FONT_SIZE - 3.0;

/// The scrollbar thumb rect (screen) within a node `rect`, plus the world
/// overflow, or `None` when the content fits within `node_height`. Sizes/scroll
/// are world px; `zoom` scales the thumb width. Pure, so it's unit-tested.
/// The thumb for a card being edited, placed from how far the editor is
/// scrolled through its content (`fraction`, `0..=1`) rather than from a world
/// scroll offset.
///
/// Goes through [`scrollbar_thumb`] on a scroll derived from the same overflow
/// the read path uses, so the thumb is the same size and travels the same track
/// whether or not the card is being edited — entering an edit shouldn't make it
/// jump.
fn editing_thumb(
    rect: Rectangle,
    node_height: f32,
    content_height: f32,
    fraction: f32,
    zoom: f32,
) -> Option<Rectangle> {
    let overflow = (content_height - node_height).max(0.0);
    let scroll = fraction.clamp(0.0, 1.0) * overflow;
    scrollbar_thumb(rect, node_height, content_height, scroll, zoom).map(|(thumb, _)| thumb)
}

/// Whether a card scrolls its overflow instead of showing all of it.
///
/// Either the setting is on, or the card has grown to the auto-size ceiling —
/// where it stops growing, so without a scrollbar the rest of the note would
/// simply be unreachable. Public, with [`scroll_offset`] and [`text_frame`],
/// because the app maps points to text too (a double-click's caret, a find
/// hit), and a map that disagrees with what's drawn lands on the wrong line.
pub fn scrolls(config: &crate::config::Config, node: &CanvasNode) -> bool {
    node.is_text() && (config.node_scrollbars || node.height >= crate::text_raster::MAX_AUTO_HEIGHT - 0.5)
}

/// Scrollable content height (world px) of a text node: the rendered text
/// extent (`measure_markdown` insets to the render column) plus the node's
/// vertical padding, split top/bottom. So a node at its fitted (autosized)
/// height has no overflow, and a scrolled one keeps `NODE_TEXT_PAD_Y / 2`
/// padding at each end rather than butting the text against the card edge.
pub fn content_height(text: &str, width: f32, theme: &ColorTheme) -> f32 {
    let wrap = crate::text_raster::card_text_wrap(width);
    crate::text_raster::measure_markdown(text.trim(), wrap, FONT_SIZE, theme).1 + NODE_TEXT_PAD_Y
}

/// A card's scroll offset (world px), clamped to its overflow; 0 for a card
/// that doesn't scroll.
pub fn scroll_offset(
    config: &crate::config::Config,
    theme: &ColorTheme,
    node_scroll: &HashMap<String, f32>,
    node: &CanvasNode,
) -> f32 {
    if !scrolls(config, node) {
        return 0.0;
    }
    let content = content_height(node.text.as_deref().unwrap_or(""), node.width, theme);
    let raw = node_scroll.get(&node.id).copied().unwrap_or(0.0);
    raw.clamp(0.0, (content - node.height).max(0.0))
}

/// The rectangle a card's Markdown is laid out against, in whatever space
/// `card` is in at `zoom` (world at 1). The card itself, which centres the text
/// in it; or for a card that scrolls by `scroll`, a zero-height line half the
/// padding below its top and shifted up by the scroll — zero height turns the
/// layout's centring off, and the same padding lands at the bottom when it's
/// scrolled to the end. Everything that puts a point on a card's text goes
/// through this, as the drawing does.
pub fn text_frame(card: Rectangle, scroll: Option<f32>, zoom: f32) -> Rectangle {
    match scroll {
        Some(scroll) => Rectangle {
            y: card.y + (NODE_TEXT_PAD_Y / 2.0 - scroll) * zoom,
            height: 0.0,
            ..card
        },
        None => card,
    }
}

/// Where `query` occurs in a run's `text`, as byte ranges, case-folded and not
/// overlapping — how the find bar's hits are counted on a card, in reading
/// order. Public because the app counts them the same way, to scroll a card to
/// the one that's current.
pub fn hit_ranges(text: &str, query: &str) -> Vec<(usize, usize)> {
    let fold = |c: char| c.to_lowercase().next().unwrap_or(c);
    let hay: Vec<char> = text.chars().map(fold).collect();
    let needle: Vec<char> = query.chars().map(fold).collect();
    if needle.is_empty() || hay.len() < needle.len() {
        return Vec::new();
    }
    // Char index → byte offset, so we can slice the original run text.
    let mut byte: Vec<usize> = text.char_indices().map(|(b, _)| b).collect();
    byte.push(text.len());

    let mut hits = Vec::new();
    let mut cs = 0;
    while cs + needle.len() <= hay.len() {
        if hay[cs..cs + needle.len()] == needle[..] {
            let ce = cs + needle.len();
            hits.push((byte[cs], byte[ce]));
            cs = ce;
        } else {
            cs += 1;
        }
    }
    hits
}

fn scrollbar_thumb(
    rect: Rectangle,
    node_height: f32,
    content_height: f32,
    scroll: f32,
    zoom: f32,
) -> Option<(Rectangle, f32)> {
    let overflow = content_height - node_height;
    if overflow <= 0.5 {
        return None;
    }
    let scroll = scroll.clamp(0.0, overflow);
    // Thumb height ∝ visible fraction; floored so a tiny node still has a grabbable bar.
    let thumb_frac = (node_height / content_height).clamp(0.12, 1.0);
    let thumb_h = rect.height * thumb_frac;
    let track = (rect.height - thumb_h).max(0.0);
    let thumb_y = rect.y + (scroll / overflow) * track;
    let w = (NODE_SCROLLBAR_WIDTH * zoom).clamp(3.0, 10.0);
    let thumb = Rectangle {
        x: rect.x + rect.width - w - 2.0,
        y: thumb_y,
        width: w,
        height: thumb_h,
    };
    Some((thumb, overflow))
}

const SIDES: [&str; 4] = ["left", "right", "top", "bottom"];

/// How close (screen px) the cursor must be to a node for side ports to
/// appear.
const HOVER_MARGIN: f32 = 14.0;

impl canvas::Program<Message> for Board<'_> {
    type State = Interaction;

    fn update(
        &self,
        state: &mut Interaction,
        event: &Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<Message>> {
        // The editing overlay is positioned in view(), which needs to know
        // the canvas size; report it whenever it changes.
        if state.reported_viewport != Some(bounds.size()) {
            state.reported_viewport = Some(bounds.size());
            return Some(canvas::Action::publish(Message::ViewportResized(
                bounds.size(),
            )));
        }

        // While an in-board modal (settings, command palette) is up it owns all
        // input: board shortcuts (Esc, Cmd+Z, …) mustn't fire underneath it, and
        // scroll/zoom gestures its layer doesn't swallow (e.g. wheel over the
        // canvas) mustn't pan the board below. Modifier changes are the one
        // exception — we keep tracking them so the chord that opened the modal
        // (e.g. Cmd+Shift+P) doesn't leave Command "stuck" once released, which
        // would make a later two-finger scroll zoom instead of pan.
        if self.modal
            && !matches!(event, Event::Keyboard(keyboard::Event::ModifiersChanged(_)))
        {
            return None;
        }

        match event {
            Event::Keyboard(keyboard::Event::ModifiersChanged(modifiers)) => {
                state.modifiers = *modifiers;
                None
            }
            Event::Keyboard(keyboard::Event::KeyPressed {
                key: keyboard::Key::Named(keyboard::key::Named::Escape),
                ..
            }) => {
                // A menu is the nearest thing to dismiss. Without this Esc fell
                // through to the cases below and, in the overlay, put the whole
                // board away with the menu still on it.
                if self.context_menu {
                    Some(canvas::Action::publish(Message::CloseContextMenu).and_capture())
                } else if self.editing.is_some() {
                    Some(canvas::Action::publish(Message::FinishEditing))
                } else if self.search.is_some() {
                    // The find bar's own subscription closes it on Esc — don't
                    // also dismiss the overlay.
                    None
                } else if !self.ghosts.is_empty() {
                    Some(canvas::Action::publish(Message::DismissGhosts).and_capture())
                } else if self.overlay {
                    Some(canvas::Action::publish(Message::Exit))
                } else {
                    None
                }
            }
            // Delete/Backspace removes the selected node — but only when not
            // editing, so the keys still edit text in the editor.
            Event::Keyboard(keyboard::Event::KeyPressed {
                key:
                    keyboard::Key::Named(
                        keyboard::key::Named::Delete | keyboard::key::Named::Backspace,
                    ),
                ..
            }) if self.editing.is_none()
                && (!self.selected.is_empty() || self.selected_edge.is_some()) =>
            {
                Some(canvas::Action::publish(Message::DeleteSelected).and_capture())
            }
            // The board by keyboard (see `app::navigate`) — not while editing,
            // with a menu up, or with the find bar taking the keys. The
            // arrows move the selection, or with ⌥ the cards; Enter edits;
            // Tab adds a linked note.
            Event::Keyboard(keyboard::Event::KeyPressed {
                key: keyboard::Key::Named(named),
                modifiers,
                ..
            }) if self.editing.is_none() && !self.context_menu && self.search.is_none() && !modifiers.command() => {
                use keyboard::key::Named;
                use thinkboard_core::navigate::Toward;
                let toward = match named {
                    Named::ArrowLeft => Some(Toward::Left),
                    Named::ArrowRight => Some(Toward::Right),
                    Named::ArrowUp => Some(Toward::Up),
                    Named::ArrowDown => Some(Toward::Down),
                    _ => None,
                };
                let message = match (named, toward) {
                    (_, Some(toward)) if modifiers.alt() => Message::NudgeSelection(toward),
                    (_, Some(toward)) => Message::SelectToward { toward, extend: modifiers.shift() },
                    (Named::Enter, _) if !self.selected.is_empty() => Message::EditSelected,
                    (Named::Tab, _) if !self.selected.is_empty() && !modifiers.shift() => Message::AddLinkedNote,
                    _ => return None,
                };
                Some(canvas::Action::publish(message).and_capture())
            }
            // The Cmd/Ctrl chords, none of which fire during an edit session —
            // the editor gets those keys for its own use. Matched on the
            // *latin* key for the physical position (the trick the editor and
            // iced's copy/paste use) so they fire whatever the keyboard layout.
            Event::Keyboard(keyboard::Event::KeyPressed {
                key,
                physical_key,
                modifiers,
                ..
            }) if self.editing.is_none() && modifiers.command() => {
                let letter = key.to_latin(*physical_key)?;
                let message = match chord(letter, modifiers.shift(), !self.selected.is_empty())? {
                    Chord::Undo => Message::Undo,
                    Chord::Redo => Message::Redo,
                    Chord::Copy => Message::CopyNodes,
                    Chord::Find => Message::OpenSearch,
                    Chord::Palette => Message::OpenCommandPalette,
                    Chord::SwitchCanvas => Message::SwitchCanvas,
                    Chord::ZoomIn | Chord::ZoomOut => Message::ZoomBy {
                        factor: if letter == '=' || letter == '+' { ZOOM_KEY_STEP } else { 1.0 / ZOOM_KEY_STEP },
                        cursor: Point::new(bounds.width / 2.0, bounds.height / 2.0),
                    },
                    Chord::Fit => Message::FitBoard,
                    Chord::Paste => Message::Paste {
                        // At the cursor, or the middle of the view when it is
                        // off-canvas.
                        world: cursor
                            .position()
                            .map(|p| self.camera.to_world(p - Vector::new(bounds.x, bounds.y)))
                            .unwrap_or_else(|| {
                                self.camera.to_world(Point::new(
                                    bounds.width / 2.0,
                                    bounds.height / 2.0,
                                ))
                            }),
                    },
                };
                Some(canvas::Action::publish(message).and_capture())
            }
            Event::Mouse(mouse_event) => {
                let position = cursor
                    .position()
                    .map(|p| p - Vector::new(bounds.x, bounds.y));
                self.handle_mouse(state, *mouse_event, position)
            }
            // Keep frames coming while the snap animation is mid-flight,
            // even with the mouse holding still.
            Event::Window(iced::window::Event::RedrawRequested(_)) => {
                let snap = state.snap?;
                let now = Instant::now();
                if snap.settled(now) {
                    if snap.direction < 0.0 {
                        state.snap = None;
                    }
                    None
                } else {
                    Some(canvas::Action::request_redraw())
                }
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        state: &Interaction,
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        // Drawn through `Layered`, which reads the layering too; a plain canvas
        // would get the same geometries in one layer.
        self.draw_stacked(state, renderer, theme, bounds, cursor)
            .into_iter()
            .map(|(geometry, _)| geometry)
            .collect()
    }

    fn mouse_interaction(
        &self,
        state: &Interaction,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        match state.kind {
            Kind::Panning { .. }
            | Kind::Dragging { .. }
            | Kind::DraggingGhost { .. }
            | Kind::DraggingCluster { .. } => mouse::Interaction::Grabbing,
            Kind::Connecting { .. } => mouse::Interaction::Pointer,
            Kind::Resizing { corner, .. } => corner.interaction(),
            Kind::ScrollingNode { .. } => mouse::Interaction::Grabbing,
            Kind::Idle => {
                let Some(position) = cursor.position_in(bounds) else {
                    return mouse::Interaction::default();
                };
                if self.show_clusters() {
                    if self.cluster_at(self.camera.to_world(position)).is_some() {
                        mouse::Interaction::Grab
                    } else {
                        mouse::Interaction::default()
                    }
                } else if let Some((_, corner)) = self.corner_at(position) {
                    corner.interaction()
                } else if self.node_thumb_at(position).is_some() {
                    mouse::Interaction::Grab
                } else if self.port_at(position).is_some() {
                    mouse::Interaction::Pointer
                } else if self
                    .hit_test(self.camera.to_world(position))
                    .filter(|index| self.selected.contains(index))
                    .and_then(|index| self.link_at(index, position))
                    .is_some()
                {
                    // Over a link on a selected node: a click opens it.
                    mouse::Interaction::Pointer
                } else if self.hit_test(self.camera.to_world(position)).is_some() {
                    mouse::Interaction::Grab
                } else if self.edge_at(position).is_some() {
                    mouse::Interaction::Pointer
                } else {
                    mouse::Interaction::default()
                }
            }
        }
    }
}

/// Which nodes a [`Board::draw_nodes`] pass renders. The selected node(s) are
/// lifted out of the cached scene into the uncached overlay so they layer above
/// everything else — a single geometry can't put a node's card mesh above
/// another node's image, since images always composite over meshes.
#[derive(Clone, Copy)]
enum NodePass<'p> {
    /// Every node (used in cluster mode, where there's no overlay to lift into).
    All,
    /// The unselected cards in one plane of a pile (see [`Board::stacking`]):
    /// plane 0 is the cached base scene, the rest are stacked above it.
    Plane(usize, &'p [usize]),
    /// Only the selected node(s) — drawn into the overlay, on top.
    SelectedOnly,
}

impl Stacked<Message> for Board<'_> {
    fn draw_stacked(
        &self,
        state: &Interaction,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<(Geometry, bool)> {
        // The backdrop (dim or dark fill) and the node shadows live in
        // stack layers below this canvas; only edges and nodes are drawn
        // here.
        let build_start = self.show_fps.then(Instant::now);
        // Crossfade node and cluster layers through the threshold; the
        // more-opaque one is drawn on top so end states are clean.
        let weight = self.cluster_weight();
        let draw_nodes = weight < 1.0;
        let draw_clusters = weight > 0.0;
        // Cards are lifted into planes only while they're drawn alone: through
        // a crossfade the base scene has to own the order between cards and
        // clusters, and in cluster mode there are no cards.
        let lift = draw_nodes && !draw_clusters && !self.show_clusters();
        // Worked out once per rebuild, by whichever plane's cache asks first —
        // and not at all on a frame every plane's cache answers itself.
        let planes = std::cell::OnceCell::new();
        let planes = || planes.get_or_init(|| self.stacking(lift));
        let geometry = self.cache.base.draw(renderer, bounds.size(), |frame| {
            self.cache.lifted_in_use.set(planes().1);

            let node_layer = |frame: &mut Frame| {
                // Frames first: a group is a backdrop its members sit on.
                self.draw_groups(frame);
                self.draw_edges(frame);
                // In cluster mode there's no overlay, so draw every node here;
                // otherwise hold the selected node(s) back for the overlay, and
                // any card over another back for a plane above.
                let pass = if self.show_clusters() {
                    NodePass::All
                } else {
                    NodePass::Plane(0, &planes().0)
                };
                self.draw_nodes(frame, pass);
            };
            let cluster_layer = |frame: &mut Frame| {
                self.draw_cluster_links(frame);
                self.draw_clusters(frame);
            };

            if weight >= 0.5 {
                if draw_nodes {
                    node_layer(frame);
                }
                if draw_clusters {
                    cluster_layer(frame);
                }
            } else {
                if draw_clusters {
                    cluster_layer(frame);
                }
                if draw_nodes {
                    node_layer(frame);
                }
            }
        });

        if let Some(start) = build_start {
            let ms = start.elapsed().as_secs_f32() * 1000.0;
            let mut stats = FRAME_STATS.lock().unwrap();
            // Only average frames that actually rebuilt (cache miss); a cache
            // hit returns in ~0 ms and would otherwise mask the real cost.
            if ms > 0.05 {
                stats.build_ms = stats.build_ms * 0.9 + ms * 0.1;
            }
        }

        let mut geometries = vec![(geometry, false)];
        // Each in a layer of its own (`Layered`), so a card here covers the text
        // of one beneath it, as a card in the same layer can't — which is what
        // a plane exists for, so every one asks.
        if lift {
            for plane in 1..=self.cache.lifted_in_use.get() {
                let cards = self.cache.lifted[plane - 1].draw(renderer, bounds.size(), |frame| {
                    self.draw_nodes(frame, NodePass::Plane(plane, &planes().0));
                });
                geometries.push((cards, true));
            }
        }

        // Uncached overlay: edge highlights (on top of nodes), hover port
        // dots, and the connection-drag preview. Not in cluster mode.
        if !self.show_clusters() {
            let mut overlay = Frame::new(renderer, bounds.size());

            // Lift the selected node(s) above the cached scene so the node
            // being moved layers over any node it overlaps (its card included).
            self.draw_nodes(&mut overlay, NodePass::SelectedOnly);

            self.draw_search_focus(&mut overlay);

            if let Some(edge) = self.selected_edge {
                // A thicker ring than hover makes selection unmistakable.
                self.draw_edge_outline(&mut overlay, edge, EDGE_HIGHLIGHT_RING, 1.5);
                self.draw_edge_line(&mut overlay, edge, EDGE_HIGHLIGHT);
            }
            if let Some(edge) = self.hovered_edge.filter(|e| self.selected_edge != Some(*e)) {
                self.draw_edge_outline(&mut overlay, edge, EDGE_HOVER_RING, 1.0);
                self.draw_edge_line(&mut overlay, edge, EDGE_HOVER);
            }

            // The scrollbar of the card being edited. Uncached for two
            // reasons: a cursor move doesn't invalidate the scene cache, and
            // the scroll it reads is only settled once the editor has laid out
            // — which happens after `view` built this widget, but before it
            // draws.
            if let Some(index) = self.editing {
                self.draw_editing_thumb(&mut overlay, index);
            }

            // Ghost suggestions (uncached so they can fade toward dismissal).
            self.draw_ghosts(&mut overlay);
            // Shimmer sweeping across the notes feeding each pending AI request.
            for flow in self.ai_flows {
                self.draw_flow_shimmer(&mut overlay, flow);
            }

            if let Some(marquee) = &state.marquee {
                let rect = rect_from_points(marquee.start, marquee.current);
                overlay.fill_rectangle(
                    rect.position(),
                    rect.size(),
                    Color { a: 0.12, ..self.theme.accent },
                );
                overlay.stroke_rectangle(
                    rect.position(),
                    rect.size(),
                    Stroke::default()
                        .with_color(Color { a: 0.9, ..self.theme.accent })
                        .with_width(1.0),
                );
            }

            match state.kind {
                Kind::Connecting { from, side, cursor } => {
                    self.draw_connection_preview(&mut overlay, from, side, cursor, state.snap);
                }
                Kind::Idle => {
                    if let Some((index, side)) = state.hover {
                        if self.editing != Some(index) {
                            self.draw_port(&mut overlay, index, side);
                        }
                    }
                }
                _ => {}
            }

            // Snap-grid dots around the node(s) being dragged. Drawn last so
            // they sit atop the overlay.
            if self.config.snap_to_grid
                && let Kind::Dragging { index, .. } = state.kind
            {
                self.draw_snap_dots(&mut overlay, index);
            }

            // A layer of its own only when it has to cover a card: almost every
            // frame it holds a port dot or a highlight, which sit fine in the
            // layer below, and a layer is a pass of the whole window.
            geometries.push((overlay.into_geometry(), self.overlay_covers_cards()));
        }

        // Wayfinding button when the viewport has drifted off all content. Its
        // own geometry (not the overlay above, which is suppressed in cluster
        // mode) so it shows regardless of zoom.
        if let Some(target) = self.wayfinding_target() {
            let mut wf = Frame::new(renderer, bounds.size());
            self.draw_wayfinding(&mut wf, target);
            geometries.push((wf.into_geometry(), false));
        }

        // Its counterpart for a board with nothing to find: how to start one.
        // Gone with the first note, and while a menu is up — the menu is the
        // answer it gives.
        if self.nodes.is_empty() && self.ghosts.is_empty() && !self.context_menu {
            let mut hint = Frame::new(renderer, bounds.size());
            self.draw_empty_hint(&mut hint, bounds.size());
            geometries.push((hint.into_geometry(), false));
        }

        if self.show_fps {
            geometries.push((self.draw_fps_meter(renderer, bounds), false));
        }

        geometries
    }
}

impl Board<'_> {
    fn handle_mouse(
        &self,
        state: &mut Interaction,
        event: mouse::Event,
        position: Option<Point>,
    ) -> Option<canvas::Action<Message>> {
        match event {
            mouse::Event::ButtonPressed(Button::Left) => {
                let position = position?;

                // The wayfinding button (only shown when no node is on-screen):
                // fly the camera to the nearest node.
                if let Some(target) = self.wayfinding_target()
                    && self.wayfinding_button(target).0.contains(position)
                {
                    return Some(
                        canvas::Action::publish(Message::FocusWorld(target)).and_capture(),
                    );
                }

                // In cluster mode, a press on a cluster card drags it;
                // otherwise it acts as a background click (dismiss).
                if self.show_clusters() {
                    let world = self.camera.to_world(position);
                    if let Some((cluster, anchor)) = self
                        .cluster_at(world)
                        .and_then(|c| Some((c, cluster_anchor(self.clusters, self.nodes, c)?)))
                    {
                        let at = &self.nodes[anchor];
                        state.kind = Kind::DraggingCluster {
                            cluster,
                            anchor,
                            grab: world - Point::new(at.x, at.y),
                        };
                        return Some(
                            canvas::Action::publish(Message::CancelInertia).and_capture(),
                        );
                    }
                    state.background_press = Some(position);
                    return Some(canvas::Action::publish(Message::CancelInertia));
                }

                let hit = self.hit_test(self.camera.to_world(position));

                if let Some(index) = self.editing {
                    if hit != Some(index) {
                        return Some(
                            canvas::Action::publish(Message::StopEditing).and_capture(),
                        );
                    }
                    return None; // Let clicks over the edited node reach the editor.
                }

                // A button node (e.g. the scratch board's "Create a board")
                // acts as a button: it neither selects, drags, nor edits — the
                // press is captured and its action fires on release.
                if let Some(index) = hit
                    && let Some(action) = self.node_action(index)
                {
                    state.kind = Kind::Idle;
                    state.action = Some(action);
                    state.hover = None;
                    return Some(canvas::Action::request_redraw().and_capture());
                }

                // A press on a corner starts a resize.
                if let Some((index, corner)) = self.corner_at(position) {
                    let rect = self.node_rect(&self.nodes[index]);
                    let anchor = match corner {
                        Corner::TopLeft => Point::new(rect.x + rect.width, rect.y + rect.height),
                        Corner::TopRight => Point::new(rect.x, rect.y + rect.height),
                        Corner::BottomLeft => Point::new(rect.x + rect.width, rect.y),
                        Corner::BottomRight => Point::new(rect.x, rect.y),
                    };
                    state.kind = Kind::Resizing {
                        index,
                        corner,
                        anchor,
                    };
                    state.hover = None;
                    return Some(canvas::Action::publish(Message::CancelInertia).and_capture());
                }

                // A press on a scrollbar thumb scrolls the node (checked after
                // corners so the resize handles win at the very edges).
                if let Some((index, overflow, scale)) = self.node_thumb_at(position) {
                    let start_scroll = self.node_scroll_offset(&self.nodes[index]);
                    state.kind = Kind::ScrollingNode {
                        index,
                        start_scroll,
                        press_y: position.y,
                        scale,
                        overflow,
                    };
                    state.hover = None;
                    return Some(canvas::Action::publish(Message::CancelInertia).and_capture());
                }

                // A press on a side port starts a connection drag.
                if let Some((from, side)) = self.port_at(position) {
                    state.kind = Kind::Connecting {
                        from,
                        side,
                        cursor: position,
                    };
                    state.hover = None;
                    return Some(canvas::Action::publish(Message::CancelInertia).and_capture());
                }

                let now = Instant::now();
                let is_double_click = state
                    .last_click
                    .is_some_and(|(at, p)| {
                        now.duration_since(at) < DOUBLE_CLICK_TIME
                            && p.distance(position) < DOUBLE_CLICK_DISTANCE
                    });
                state.last_click = Some((now, position));

                // Suggestion cards: the "×" dismisses; otherwise a press starts
                // a potential drag (a click accepts on release, a drag moves).
                let ghost_world = self.camera.to_world(position);
                if let Some(gi) = self.ghost_dismiss_at(ghost_world) {
                    state.kind = Kind::Idle;
                    return Some(canvas::Action::publish(Message::DismissGhost(gi)).and_capture());
                }
                if let Some(gi) = self.ghost_at(ghost_world) {
                    state.kind = Kind::DraggingGhost {
                        index: gi,
                        grab: ghost_world - self.ghosts[gi].rect.position(),
                        press: position,
                        moved: false,
                    };
                    return Some(canvas::Action::publish(Message::CancelInertia).and_capture());
                }

                match hit {
                    Some(index)
                        if is_double_click
                            && (self.nodes[index].is_text() || self.nodes[index].is_group()) =>
                    {
                        state.kind = Kind::Idle;
                        // Pass the click's world position so the caret lands under
                        // the cursor, not at the start of the note.
                        Some(
                            canvas::Action::publish(Message::StartEditing {
                                index,
                                at: Some(self.camera.to_world(position)),
                            })
                            .and_capture(),
                        )
                    }
                    Some(index) if state.modifiers.command() => {
                        // Cmd-click toggles this node in the selection without
                        // starting a drag.
                        state.kind = Kind::Idle;
                        Some(canvas::Action::publish(Message::ToggleNode(index)).and_capture())
                    }
                    Some(index) => {
                        let node = &self.nodes[index];
                        let world = self.camera.to_world(position);
                        // A link on an already-selected node opens on release
                        // (drag still moves the node); the first click just
                        // selects, so links don't fire until the node has focus.
                        // A link *node* is the same gesture, but its whole card
                        // is the link.
                        state.link = self.selected.contains(&index).then(|| {
                            match node.node_kind() {
                                NodeKind::Link => node.url().map(str::to_owned),
                                _ => self.link_at(index, position),
                            }
                        }).flatten();
                        state.kind = Kind::Dragging {
                            index,
                            grab: world - Point::new(node.x, node.y),
                            press: position,
                            moved: false,
                        };
                        // Dragging a node that's already in the selection keeps
                        // the whole group (for a group-drag); otherwise select
                        // just this node. Either way, cancel any coast.
                        let message = if self.selected.contains(&index) {
                            Message::CancelInertia
                        } else {
                            Message::SelectNode(Some(index))
                        };
                        Some(canvas::Action::publish(message).and_capture())
                    }
                    None => {
                        // No node: an edge under the cursor gets selected;
                        // empty space starts a rubber-band selection (and, on a
                        // release without drag, deselects/dismisses).
                        if let Some(edge) = self.edge_at(position) {
                            Some(
                                canvas::Action::publish(Message::SelectEdge(Some(edge)))
                                    .and_capture(),
                            )
                        } else if is_double_click
                            && self.config.double_click_adds_note(self.overlay)
                        {
                            // The first click of the pair started a marquee and
                            // released it unmoved; this one makes the note.
                            state.kind = Kind::Idle;
                            let world = self.camera.to_world(position);
                            Some(canvas::Action::publish(Message::CreateNode { world }).and_capture())
                        } else {
                            let base = if state.modifiers.command() {
                                self.selected.to_vec()
                            } else {
                                Vec::new()
                            };
                            state.marquee = Some(Marquee {
                                start: position,
                                current: position,
                                base,
                            });
                            state.background_press = Some(position);
                            Some(canvas::Action::publish(Message::CancelInertia))
                        }
                    }
                }
            }
            mouse::Event::ButtonReleased(Button::Left) => {
                // A finished marquee leaves its selection in place; the click
                // vs. drag decision below (via background_press) still runs.
                state.marquee = None;
                // Releasing over a pressed button node fires its action.
                if let Some(action) = state.action.take() {
                    state.kind = Kind::Idle;
                    return Some(
                        canvas::Action::publish(Message::CanvasAction(action)).and_capture(),
                    );
                }
                // A ghost released without moving is a click → accept it (but a
                // "…" placeholder isn't acceptable); a dragged ghost just stays
                // where it was moved to.
                if let Kind::DraggingGhost { index, moved, .. } = state.kind {
                    state.kind = Kind::Idle;
                    let placeholder = self.ghosts.get(index).is_some_and(|g| g.placeholder);
                    return Some(if moved || placeholder {
                        canvas::Action::request_redraw().and_capture()
                    } else {
                        canvas::Action::publish(Message::AcceptGhost(index)).and_capture()
                    });
                }
                if matches!(state.kind, Kind::DraggingCluster { .. }) {
                    state.kind = Kind::Idle;
                    return Some(
                        canvas::Action::publish(Message::ClusterDragEnd).and_capture(),
                    );
                }
                if let Kind::Connecting { from, side, cursor } = state.kind {
                    state.kind = Kind::Idle;
                    state.snap = None;
                    let message = match self.connect_target(from, cursor) {
                        Some((to, to_side)) => Some(Message::AddEdge {
                            from,
                            side,
                            to,
                            to_side,
                        }),
                        // Back on the source node: cancel instead of
                        // creating a node inside it.
                        None if self.hit_test(self.camera.to_world(cursor)) == Some(from) => None,
                        None => Some(Message::CreateConnectedNode {
                            from,
                            side,
                            world: self.camera.to_world(cursor),
                        }),
                    };
                    return Some(match message {
                        Some(message) => canvas::Action::publish(message).and_capture(),
                        None => canvas::Action::request_redraw().and_capture(),
                    });
                }

                if let Kind::Resizing { .. } = state.kind {
                    state.kind = Kind::Idle;
                    return Some(canvas::Action::publish(Message::ResizeEnd).and_capture());
                }
                if let Kind::ScrollingNode { .. } = state.kind {
                    state.kind = Kind::Idle;
                    return Some(canvas::Action::request_redraw().and_capture());
                }

                let background_press = state.background_press.take();
                if let Kind::Dragging { index, moved, .. } = state.kind {
                    state.kind = Kind::Idle;
                    // A pending link released without a drag opens it; otherwise
                    // this was a node drag (or a plain click) — just end it.
                    let open = state.link.take().filter(|_| !moved);
                    Some(match open {
                        Some(url) => canvas::Action::publish(Message::OpenUrl(url)).and_capture(),
                        None => canvas::Action::publish(Message::NodeDragEnd { index, moved })
                            .and_capture(),
                    })
                } else if let (Some(start), Some(position)) = (background_press, position) {
                    if start.distance(position) >= 4.0 {
                        // A drag, not a click.
                        None
                    } else if self.search.is_some() {
                        // A click on the board puts the find bar away first,
                        // as Esc does — the board itself only on the next.
                        Some(canvas::Action::publish(Message::CloseSearch).and_capture())
                    } else if !self.selected.is_empty() || self.selected_edge.is_some() {
                        // A click on empty space first clears the selection;
                        // it does not dismiss the board.
                        Some(canvas::Action::publish(Message::SelectNode(None)).and_capture())
                    } else if self.config.click_dismisses(self.overlay) {
                        // Nothing selected: dismiss, like Dashboard.
                        Some(canvas::Action::publish(Message::Exit).and_capture())
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
            mouse::Event::ButtonPressed(Button::Right) => {
                let position = position?;
                state.kind = Kind::Panning {
                    last: position,
                    start: position,
                };
                Some(canvas::Action::publish(Message::CancelInertia).and_capture())
            }
            mouse::Event::ButtonReleased(Button::Right) => {
                if let Kind::Panning { start, .. } = state.kind {
                    state.kind = Kind::Idle;
                    // A right-click that didn't drag opens the context menu;
                    // a drag was a pan and coasts on release.
                    if let Some(position) = position {
                        if start.distance(position) < 5.0 {
                            // A right-click on a node or a link offers Delete;
                            // on empty canvas it offers Add node / Paste.
                            let world = self.camera.to_world(position);
                            let message = if let Some(node) = self.hit_test(world) {
                                Message::OpenNodeMenu {
                                    screen: position,
                                    node,
                                }
                            } else if let Some(edge) = self.edge_at(position) {
                                Message::OpenLinkMenu {
                                    screen: position,
                                    edge,
                                }
                            } else {
                                Message::OpenContextMenu {
                                    screen: position,
                                    world,
                                }
                            };
                            return Some(canvas::Action::publish(message).and_capture());
                        }
                    }
                    Some(canvas::Action::publish(Message::EndPan).and_capture())
                } else {
                    None
                }
            }
            mouse::Event::CursorMoved { .. } => {
                let position = position?;
                // A rubber-band drag updates the selection live from the nodes
                // its rectangle covers (unioned with the additive base).
                if let Some(marquee) = &mut state.marquee {
                    marquee.current = position;
                    let rect = rect_from_points(marquee.start, position);
                    let mut selection = marquee.base.clone();
                    for index in self.nodes_in_rect(rect) {
                        if !selection.contains(&index) {
                            selection.push(index);
                        }
                    }
                    return Some(canvas::Action::publish(Message::SelectNodes(selection)));
                }
                let link_pending = state.link.is_some();
                match &mut state.kind {
                    Kind::Panning { last, .. } => {
                        let delta = position - *last;
                        *last = position;
                        Some(canvas::Action::publish(Message::Pan { delta, track: true }))
                    }
                    Kind::Dragging { index, grab, press, moved } => {
                        // A press over a link stays a click until the cursor
                        // leaves the tolerance; after that it's a node drag.
                        if link_pending && !*moved && position.distance(*press) < DOUBLE_CLICK_DISTANCE {
                            None
                        } else {
                            *moved = true;
                            let world = self.camera.to_world(position);
                            Some(canvas::Action::publish(Message::MoveNode {
                                index: *index,
                                position: self.snap_world(world - *grab),
                            }))
                        }
                    }
                    Kind::DraggingGhost { index, grab, press, moved } => {
                        // Stay a "click" (accept) until the cursor leaves a small
                        // tolerance; after that it's a drag (reposition).
                        if !*moved && position.distance(*press) < DOUBLE_CLICK_DISTANCE {
                            None
                        } else {
                            *moved = true;
                            let world = self.camera.to_world(position);
                            Some(canvas::Action::publish(Message::MoveGhost {
                                index: *index,
                                position: world - *grab,
                            }))
                        }
                    }
                    Kind::Resizing { index, anchor, .. } => {
                        Some(canvas::Action::publish(Message::ResizeNode {
                            index: *index,
                            anchor: *anchor,
                            cursor: self.snap_world(self.camera.to_world(position)),
                        })
                        .and_capture())
                    }
                    Kind::ScrollingNode { index, start_scroll, press_y, scale, overflow } => {
                        let offset =
                            (*start_scroll + (position.y - *press_y) * *scale).clamp(0.0, *overflow);
                        let id = self.nodes[*index].id.clone();
                        Some(
                            canvas::Action::publish(Message::SetNodeScroll { id, offset })
                                .and_capture(),
                        )
                    }
                    Kind::DraggingCluster { cluster, anchor, grab } => {
                        // Measured against the anchor's *absolute* position and
                        // snapped there, exactly as a node drag is — the running
                        // delta this used to accumulate could never snap, since
                        // rounding each frame's step either sticks or drifts.
                        let want = self.camera.to_world(position) - *grab;
                        let at = self.nodes.get(*anchor)?;
                        let delta =
                            cluster_delta(want, Point::new(at.x, at.y), self.snap_grid());
                        // Within one grid cell the target doesn't move; sending
                        // a zero delta would mark the document dirty for nothing.
                        (delta != Vector::ZERO).then(|| {
                            canvas::Action::publish(Message::MoveCluster {
                                index: *cluster,
                                delta,
                            })
                            .and_capture()
                        })
                    }
                    Kind::Connecting { from, cursor, .. } => {
                        let from = *from;
                        *cursor = position;

                        // Snap-in / release transitions toward the node
                        // (or its specific dot) near the cursor.
                        let now = Instant::now();
                        let target = self.connect_target(from, position);
                        state.snap = match (target, state.snap) {
                            (Some((target, to_side)), Some(snap))
                                if snap.target == target && snap.side == to_side =>
                            {
                                if snap.direction < 0.0 {
                                    // Back into the zone: resume from the
                                    // current strength, no restart pop.
                                    Some(SnapAnim {
                                        strength_at_change: snap.strength(now),
                                        changed: now,
                                        direction: 1.0,
                                        ..snap
                                    })
                                } else {
                                    Some(snap)
                                }
                            }
                            (Some((target, to_side)), _) => Some(SnapAnim {
                                target,
                                side: to_side,
                                strength_at_change: 0.0,
                                changed: now,
                                direction: 1.0,
                            }),
                            (None, Some(snap)) if snap.direction > 0.0 => Some(SnapAnim {
                                strength_at_change: snap.strength(now),
                                changed: now,
                                direction: -1.0,
                                ..snap
                            }),
                            (None, other) => other,
                        };

                        Some(canvas::Action::request_redraw().and_capture())
                    }
                    Kind::Idle => {
                        let over_ghost = !self.show_clusters()
                            && self.ghost_at(self.camera.to_world(position)).is_some();
                        let (hover, edge) = if self.show_clusters() || over_ghost {
                            (None, None)
                        } else {
                            let hover = self.hover_side(position);
                            // Edges are only hovered when not over a node.
                            let edge = if hover.is_none()
                                && self.hit_test(self.camera.to_world(position)).is_none()
                            {
                                self.edge_at(position)
                            } else {
                                None
                            };
                            (hover, edge)
                        };
                        let node_changed = hover != state.hover;
                        state.hover = hover;
                        // Ghost hover refreshes their timeout; edge hover is app
                        // state; node port hover is board state.
                        if over_ghost != self.ghost_hovered {
                            Some(canvas::Action::publish(Message::HoverGhost(over_ghost)))
                        } else if edge != self.hovered_edge {
                            Some(canvas::Action::publish(Message::HoverEdge(edge)))
                        } else if node_changed {
                            Some(canvas::Action::request_redraw())
                        } else {
                            None
                        }
                    }
                }
            }
            mouse::Event::WheelScrolled { delta } => {
                let position = position?;
                match delta {
                    // Mouse wheel (line-based): zoom toward the cursor.
                    ScrollDelta::Lines { y, .. } => {
                        Some(canvas::Action::publish(Message::ZoomBy {
                            factor: 1.0 + y * self.config.wheel_zoom_step * self.config.zoom_speed,
                            cursor: position,
                        })
                        .and_capture())
                    }
                    // Trackpad (precise pixels): pan, or zoom with Cmd/Ctrl
                    // held (substitute for pinch, which iced does not expose).
                    ScrollDelta::Pixels { x, y } => {
                        let message = if state.modifiers.command() || state.modifiers.control() {
                            Message::ZoomBy {
                                factor: (1.0 + y * self.config.pinch_zoom_step * self.config.zoom_speed).max(0.5),
                                cursor: position,
                            }
                        } else {
                            Message::Pan {
                                delta: Vector::new(x, y),
                                track: false,
                            }
                        };
                        Some(canvas::Action::publish(message).and_capture())
                    }
                }
            }
            _ => None,
        }
    }

    /// Uncached, so it re-renders (and samples the clock) every redraw.
    fn draw_fps_meter(&self, renderer: &Renderer, bounds: Rectangle) -> Geometry {
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

    /// Topmost node containing the given world point.
    ///
    /// A group is a frame drawn *around* other nodes, so only its header strip
    /// answers: a click anywhere in its interior has to reach the node it
    /// encloses (or the empty canvas), not pick up the frame.
    fn hit_test(&self, world: Point) -> Option<usize> {
        self.nodes.iter().rposition(|node| {
            if node.is_group() {
                group_header(self.node_rect(node), 1.0).contains(world)
            } else {
                self.node_rect(node).contains(world)
            }
        })
    }

    /// The active snap spacing (world px), or `0.0` when snap-to-grid is off.
    /// A card's corner radius on screen: the theme's, at the current zoom.
    fn card_radius(&self) -> f32 {
        self.theme.card_radius * self.camera.zoom
    }

    /// The same for a group frame or a cluster box.
    fn frame_radius(&self) -> f32 {
        self.theme.frame_radius * self.camera.zoom
    }

    fn snap_grid(&self) -> f32 {
        if self.config.snap_to_grid {
            self.config.snap_grid_size
        } else {
            0.0
        }
    }

    /// Snaps a world point to the grid when snap-to-grid is on; otherwise
    /// unchanged. Applied to node moves and resizes.
    fn snap_world(&self, p: Point) -> Point {
        let (x, y) = crate::snap::point(p.x, p.y, self.snap_grid());
        Point::new(x, y)
    }

    /// The `tb_action` of a button node (currently only in the embedded scratch
    /// board), if node `index` carries one. Such nodes behave as buttons rather
    /// than editable notes.
    fn node_action(&self, index: usize) -> Option<String> {
        self.nodes
            .get(index)?
            .extra
            .get("tb_action")?
            .as_str()
            .map(str::to_owned)
    }

    /// The URL of a link under the screen-space `point` within text node
    /// `index`, if any. Reuses the cached Markdown layout, so hover and click
    /// checks are cheap.
    fn link_at(&self, index: usize, point: Point) -> Option<String> {
        let node = self.nodes.get(index)?;
        if !node.is_text() || self.editing == Some(index) {
            return None;
        }
        let rect = self.node_screen_rect(index, node);
        if !rect.contains(point) {
            return None;
        }
        let text = node.text.as_deref()?;
        let zoom = self.camera.zoom;
        let wrap = crate::text_raster::card_text_wrap(node.width);
        // Where the text is drawn — on a scrolled card, not where it would be
        // unscrolled, which put the hover and the click on another line's link.
        let frame = self.node_text_frame(node, rect);
        crate::text_raster::with_markdown_layout(
            text,
            frame.width / zoom,
            frame.height / zoom,
            wrap,
            FONT_SIZE,
            *self.theme,
            self.config.center_node_text,
            |runs| {
                for run in runs {
                    let Some(url) = &run.link else { continue };
                    let origin = Point::new(frame.x + run.x * zoom, frame.y + run.y * zoom);
                    let size = Size::new(
                        run.width * zoom,
                        run.size * crate::text_raster::LINE_HEIGHT * zoom,
                    );
                    if Rectangle::new(origin, size).contains(point) {
                        return Some(url.clone());
                    }
                }
                None
            },
        )
    }

    /// Indices of nodes whose on-screen rectangle intersects `rect` (screen
    /// coordinates); used by the rubber-band selection.
    fn nodes_in_rect(&self, rect: Rectangle) -> Vec<usize> {
        self.nodes
            .iter()
            .enumerate()
            // Frames are excluded: a marquee over a group's contents means the
            // nodes inside it, never the group itself.
            .filter(|(_, node)| !node.is_group())
            .filter(|(index, node)| rect.intersects(&self.node_screen_rect(*index, node)))
            .map(|(index, _)| index)
            .collect()
    }

    /// When the viewport shows no node at all — panned off into empty space —
    /// the world center of the node nearest the viewport center. This is the
    /// target for the wayfinding button; `None` when any node is on-screen or
    /// there are none. Independent of zoom (a node counts as "on-screen" by its
    /// position, so clustered-but-in-view content still suppresses it).
    fn wayfinding_target(&self) -> Option<Point> {
        if self.nodes.is_empty() {
            return None;
        }
        let viewport = Rectangle::new(Point::ORIGIN, self.camera.viewport);
        let any_visible = self
            .nodes
            .iter()
            .enumerate()
            .any(|(i, n)| viewport.intersects(&self.node_screen_rect(i, n)));
        if any_visible {
            return None;
        }
        let target = self.camera.to_world(Point::ORIGIN + self.camera.center());
        self.nodes
            .iter()
            .map(|n| Point::new(n.x + n.width / 2.0, n.y + n.height / 2.0))
            .min_by(|a, b| {
                a.distance(target)
                    .partial_cmp(&b.distance(target))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    }

    /// The wayfinding button's screen rect and the cardinal direction its
    /// chevrons point (unit vector, one of ±x / ±y): a full-edge bar pinned to
    /// whichever screen edge is closest to `target_world`. The top edge starts
    /// below the safe-area inset so the macOS notch doesn't clip it.
    fn wayfinding_button(&self, target_world: Point) -> (Rectangle, Vector) {
        let origin = self.camera.to_world(Point::ORIGIN + self.camera.center());
        let (dx, dy) = (target_world.x - origin.x, target_world.y - origin.y);
        let dir = if dx.abs() >= dy.abs() {
            Vector::new(dx.signum(), 0.0)
        } else {
            Vector::new(0.0, dy.signum())
        };
        let (w, h) = (self.camera.viewport.width, self.camera.viewport.height);
        let top = self.top_safe_inset;
        const THICK: f32 = 40.0;
        let rect = if dir.x > 0.0 {
            Rectangle::new(Point::new(w - THICK, top), Size::new(THICK, h - top))
        } else if dir.x < 0.0 {
            Rectangle::new(Point::new(0.0, top), Size::new(THICK, h - top))
        } else if dir.y > 0.0 {
            Rectangle::new(Point::new(0.0, h - THICK), Size::new(w, THICK))
        } else {
            Rectangle::new(Point::new(0.0, top), Size::new(w, THICK))
        };
        (rect, dir)
    }

    /// What a new, empty board says in its middle: how to put the first note on
    /// it, and where every other action lives. A blank board said nothing at
    /// all, and the right-click that starts one is the gesture nobody tries
    /// first. Muted, and drawn on the canvas rather than as a widget above it,
    /// so it can never catch the click it describes.
    fn draw_empty_hint(&self, frame: &mut Frame, size: Size) {
        let a = self.reveal;
        let center = Point::new(size.width / 2.0, size.height / 2.0);
        // Whichever gesture works here: double-click only where it's been
        // turned on and a click doesn't dismiss the board instead.
        let gesture = if self.config.double_click_adds_note(self.overlay) {
            "Double-click"
        } else {
            "Right-click"
        };
        frame.fill_text(canvas::Text {
            content: format!("{gesture} to add a note"),
            position: Point::new(center.x, center.y - 12.0),
            color: Color { a, ..self.theme.text_muted },
            size: 17.0.into(),
            font: self.theme.ui_font(),
            align_x: TextAlignment::Center,
            align_y: Vertical::Center,
            ..canvas::Text::default()
        });
        frame.fill_text(canvas::Text {
            content: format!("{PALETTE_CHORDS} to show commands"),
            position: Point::new(center.x, center.y + 14.0),
            color: Color { a: 0.7 * a, ..self.theme.text_muted },
            size: 13.0.into(),
            font: self.theme.ui_font(),
            align_x: TextAlignment::Center,
            align_y: Vertical::Center,
            ..canvas::Text::default()
        });
    }

    /// The wayfinding button: a soft, theme-colored edge bar (node card color,
    /// node corner radius on the inner corners, no border) with a double chevron
    /// (`<<` / `>>` / `^^` / `vv`) pointing toward the nearest node.
    fn draw_wayfinding(&self, frame: &mut Frame, target_world: Point) {
        let (rect, dir) = self.wayfinding_button(target_world);
        let a = self.reveal;
        let card = self.theme.card_fill;
        let center = rect.center();
        let perp = Vector::new(-dir.y, dir.x);

        // Soft gradient across the bar in the theme's card color: transparent at
        // the inner edge, solid at the screen edge it hugs.
        let half = (rect.width * dir.x.abs() + rect.height * dir.y.abs()) / 2.0;
        let fill = gradient::Linear::new(center - dir * half, center + dir * half)
            .add_stop(0.0, Color { a: 0.0, ..card })
            .add_stop(1.0, Color { a: 0.92 * a, ..card });

        // Node corner radius (12px), rounded only on the two inner corners so the
        // bar sits flush against its screen edge.
        const R: f32 = 12.0;
        let radius = if dir.x > 0.0 {
            Radius { top_left: R, bottom_left: R, top_right: 0.0, bottom_right: 0.0 }
        } else if dir.x < 0.0 {
            Radius { top_right: R, bottom_right: R, top_left: 0.0, bottom_left: 0.0 }
        } else if dir.y > 0.0 {
            Radius { top_left: R, top_right: R, bottom_left: 0.0, bottom_right: 0.0 }
        } else {
            Radius { bottom_left: R, bottom_right: R, top_left: 0.0, top_right: 0.0 }
        };
        frame.fill(
            &Path::rounded_rectangle(rect.position(), rect.size(), radius),
            fill,
        );

        // Two chevrons in the theme text color, toward the solid (edge) side.
        let base = center + dir * (half * 0.3);
        let chevron = |frame: &mut Frame, c: Point| {
            let path = Path::new(|p| {
                p.move_to(c - dir * 3.0 + perp * 7.0);
                p.line_to(c + dir * 5.0);
                p.line_to(c - dir * 3.0 - perp * 7.0);
            });
            frame.stroke(
                &path,
                Stroke::default()
                    .with_color(Color { a: 0.9 * a, ..self.theme.text })
                    .with_width(2.2)
                    .with_line_cap(stroke::LineCap::Round)
                    .with_line_join(stroke::LineJoin::Round),
            );
        };
        chevron(frame, base - dir * 4.5);
        chevron(frame, base + dir * 4.5);
    }

    /// Topmost cluster whose box contains the given world point.
    fn cluster_at(&self, world: Point) -> Option<usize> {
        self.clusters
            .iter()
            .rposition(|cluster| cluster.bounds.contains(world))
    }

    fn port_radius(&self) -> f32 {
        (4.0 * self.camera.zoom).clamp(3.5, 9.0)
    }

    /// The node corner under the cursor, if any.
    fn corner_at(&self, position: Point) -> Option<(usize, Corner)> {
        for (index, node) in self.nodes.iter().enumerate().rev() {
            if self.editing == Some(index) {
                continue;
            }
            let rect = self.node_screen_rect(index, node);
            let corners = [
                (Corner::TopLeft, Point::new(rect.x, rect.y)),
                (Corner::TopRight, Point::new(rect.x + rect.width, rect.y)),
                (Corner::BottomLeft, Point::new(rect.x, rect.y + rect.height)),
                (
                    Corner::BottomRight,
                    Point::new(rect.x + rect.width, rect.y + rect.height),
                ),
            ];
            for (corner, point) in corners {
                if point.distance(position) < CORNER_GRAB {
                    return Some((index, corner));
                }
            }
        }
        None
    }

    /// The side port under the cursor, if any (with some grab slack).
    fn port_at(&self, position: Point) -> Option<(usize, &'static str)> {
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
        // otherwise dragging a note near the edge of its group would grab the
        // group instead.
        self.frame_last(|index, node| port(index, node))
    }

    /// Runs `f` over every node, cards before frames, returning the first hit.
    /// Frames sit behind everything they enclose, so they must lose every
    /// contest for the cursor to whatever is drawn on top of them.
    fn frame_last<R>(&self, f: impl Fn(usize, &CanvasNode) -> Option<R>) -> Option<R> {
        let cards = self.nodes.iter().enumerate().rev().filter(|(_, n)| !n.is_group());
        let frames = self.nodes.iter().enumerate().rev().filter(|(_, n)| n.is_group());
        cards.chain(frames).find_map(|(index, node)| f(index, node))
    }

    /// Where a dragged connection would attach for the given cursor
    /// position: within a node, the side of the quadrant the cursor is in
    /// (the rect split into four triangles by its diagonals); within the
    /// surrounding [`SNAP_MARGIN`], the nearest side dot.
    fn connect_target(&self, from: usize, cursor: Point) -> Option<(usize, &'static str)> {
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

    /// Which node side the cursor hovers near: inside the node (or just
    /// outside it), attributed to the nearest edge.
    fn hover_side(&self, position: Point) -> Option<(usize, &'static str)> {
        // Cards before frames, for the reason `port_at` gives.
        let candidates: Vec<(usize, &CanvasNode)> = {
            let cards = self.nodes.iter().enumerate().rev().filter(|(_, n)| !n.is_group());
            let frames = self.nodes.iter().enumerate().rev().filter(|(_, n)| n.is_group());
            cards.chain(frames).collect()
        };
        for (index, node) in candidates {
            let rect = self.node_screen_rect(index, node);
            let inflated = Rectangle::new(
                rect.position() - Vector::new(HOVER_MARGIN, HOVER_MARGIN),
                Size::new(
                    rect.width + HOVER_MARGIN * 2.0,
                    rect.height + HOVER_MARGIN * 2.0,
                ),
            );
            if !inflated.contains(position) {
                continue;
            }
            let side = [
                ("left", (position.x - rect.x).abs()),
                ("right", (position.x - (rect.x + rect.width)).abs()),
                ("top", (position.y - rect.y).abs()),
                ("bottom", (position.y - (rect.y + rect.height)).abs()),
            ]
            .into_iter()
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(side, _)| side)?;
            return Some((index, side));
        }
        None
    }

    /// [`search_dim`] for this board's search, if there is one.
    fn search_dim(&self, index: usize) -> f32 {
        self.search.as_ref().map_or(1.0, |search| search_dim(search.cards, index))
    }

    /// Rings the card holding the hit being stepped through: two pixels of
    /// accent just outside its edge and a soft halo past that, in screen
    /// pixels whatever the zoom — it points at a card, so it shouldn't grow
    /// thin and vanish as the board zooms out, nor heavy as it zooms in.
    fn draw_search_focus(&self, frame: &mut Frame) {
        /// The halo's reach past the card, and its strength.
        const HALO: f32 = 7.0;
        const HALO_ALPHA: f32 = 0.18;
        let Some(index) = self.search.as_ref().and_then(|search| search.current_hit).map(|(node, _)| node) else {
            return;
        };
        let Some(node) = self.nodes.get(index) else {
            return;
        };
        let alpha = self.node_layer_alpha() * self.spawn_factors(index).1;
        if alpha <= 0.0 {
            return;
        }
        let rect = self.node_screen_rect(index, node);
        let radius = if node.is_group() { self.frame_radius() } else { self.card_radius() };
        let around = |by: f32| {
            let outer = Rectangle::new(
                Point::new(rect.x - by, rect.y - by),
                Size::new(rect.width + 2.0 * by, rect.height + 2.0 * by),
            );
            rounded_path(outer, radius + by)
        };
        let accent = self.theme.accent;
        // The halo as one stroke centred between the ring and its reach.
        let halo = (HALO - 2.0) / 2.0;
        frame.stroke(
            &around(2.0 + halo),
            Stroke::default().with_color(Color { a: HALO_ALPHA * alpha, ..accent }).with_width(HALO - 2.0),
        );
        frame.stroke(&around(1.0), Stroke::default().with_color(Color { a: alpha, ..accent }).with_width(2.0));
    }

    /// Spawn animation factors for a node: (scale, alpha).
    fn spawn_factors(&self, index: usize) -> (f32, f32) {
        match self.spawning.get(&index) {
            Some(started) => {
                let t = (started.elapsed().as_secs_f32()
                    / self.config.anim_secs(crate::SPAWN_DURATION.as_secs_f32()))
                .clamp(0.0, 1.0);
                (
                    0.2 + 0.8 * crate::camera::ease_out_back(t),
                    crate::camera::ease_out_cubic(t),
                )
            }
            None => (1.0, 1.0),
        }
    }

    /// The node's screen rect, scaled about its center while spawning.
    fn node_screen_rect(&self, index: usize, node: &CanvasNode) -> Rectangle {
        let rect = self.camera.rect_to_screen(self.node_rect(node));
        let (scale, _) = self.spawn_factors(index);
        if (scale - 1.0).abs() < f32::EPSILON {
            return rect;
        }
        let center = rect.center();
        Rectangle::new(
            Point::new(
                center.x - rect.width * scale / 2.0,
                center.y - rect.height * scale / 2.0,
            ),
            Size::new(rect.width * scale, rect.height * scale),
        )
    }

    fn node_rect(&self, node: &CanvasNode) -> Rectangle {
        Rectangle::new(
            Point::new(node.x, node.y),
            Size::new(node.width, node.height),
        )
    }

    fn scrollable(&self, node: &CanvasNode) -> bool {
        scrolls(self.config, node)
    }

    fn node_content_height(&self, text: &str, width: f32) -> f32 {
        content_height(text, width, self.theme)
    }

    /// The clamped scroll offset (world px) for a node, 0 when it fits.
    fn node_scroll_offset(&self, node: &CanvasNode) -> f32 {
        scroll_offset(self.config, self.theme, self.node_scroll, node)
    }

    /// The rectangle (screen) a card's Markdown is laid out against — see
    /// [`text_frame`].
    fn node_text_frame(&self, node: &CanvasNode, rect: Rectangle) -> Rectangle {
        let scroll = self.scrollable(node).then(|| self.node_scroll_offset(node));
        text_frame(rect, scroll, self.camera.zoom)
    }

    /// If `position` is on a scrollable node's scrollbar thumb, returns
    /// `(index, world overflow, world-scroll per screen-px)` for the drag. Only
    /// the node under the cursor is checked (the thumb sits inside its bounds).
    fn node_thumb_at(&self, position: Point) -> Option<(usize, f32, f32)> {
        let index = self.hit_test(self.camera.to_world(position))?;
        if self.editing == Some(index) {
            return None;
        }
        let node = self.nodes.get(index)?;
        if !self.scrollable(node) {
            return None;
        }
        let content = self.node_content_height(node.text.as_deref().unwrap_or(""), node.width);
        let scroll = self.node_scroll_offset(node);
        let rect = self.node_screen_rect(index, node);
        let (thumb, overflow) =
            scrollbar_thumb(rect, node.height, content, scroll, self.camera.zoom)?;
        // Horizontal slack so the thin thumb is easy to grab.
        let grab = Rectangle { x: thumb.x - 5.0, width: thumb.width + 10.0, ..thumb };
        grab.contains(position).then(|| {
            let track = (rect.height - thumb.height).max(1.0);
            (index, overflow, overflow / track)
        })
    }

    /// Content opacity during the overlay entrance/exit.
    fn scene_alpha(&self) -> f32 {
        if self.overlay {
            crate::camera::ease_out_cubic(self.reveal).clamp(0.0, 1.0)
        } else {
            1.0
        }
    }

    /// Whether zoomed out far enough that clusters are the interactive view
    /// (input handoff; distinct from the rendering crossfade).
    pub fn show_clusters(&self) -> bool {
        !self.clusters.is_empty() && self.camera.zoom < self.config.cluster_zoom
    }

    /// Crossfade weight of the cluster layer (0 when no clusters exist).
    fn cluster_weight(&self) -> f32 {
        if self.clusters.is_empty() {
            0.0
        } else {
            cluster_opacity(self.camera.zoom)
        }
    }

    fn node_layer_alpha(&self) -> f32 {
        self.scene_alpha() * (1.0 - self.cluster_weight())
    }

    fn cluster_layer_alpha(&self) -> f32 {
        self.scene_alpha() * self.cluster_weight()
    }

    /// Draws each cluster as one large summary card: the main note's text
    /// at its fitted size, or the main image scaled to the cluster box.
    fn draw_clusters(&self, frame: &mut Frame) {
        let alpha = self.cluster_layer_alpha();
        let zoom = self.camera.zoom;

        for (index, cluster) in self.clusters.iter().enumerate() {
            let rect = self.camera.rect_to_screen(cluster.bounds);
            let card = rounded_path(rect, self.frame_radius());

            let fill = self.theme.card_fill;
            frame.fill(&card, Color { a: fill.a * alpha, ..fill });

            if let Some(id) = &cluster.image_id {
                if let Some(handle) = self.images.get(id) {
                    let mut image = canvas::Image::new(handle.clone());
                    image.border_radius = self.frame_radius().into();
                    image.opacity = alpha;
                    frame.with_clip(rect, |clipped| {
                        clipped.draw_image(rect, image);
                    });
                }
            } else if !cluster.text.is_empty() {
                // Same text path as node labels: a bitmap snapshot, or live
                // vector runs when rasterizing is off.
                let wrap = (cluster.bounds.width - crate::cluster::PADDING * 2.0).max(20.0);
                if let Some(snapshot) =
                    self.cluster_snapshots.get(&index).filter(|_| self.config.rasterize_text)
                {
                    let mut image = canvas::Image::new(snapshot.handle.clone());
                    image.opacity = alpha;
                    frame.draw_image(rect, image);
                } else {
                    // No bitmap — because rasterizing is off, or because one is
                    // being rebuilt after a theme change. Either way draw the
                    // text live, as a node does in the same situation; a label
                    // in the wrong font for a moment beats a blank card.
                    frame.with_clip(rect, |clipped| {
                        self.draw_markdown_live(clipped, rect, &cluster.text, cluster.font_size, wrap, alpha, None);
                    });
                }
            }

            frame.stroke(
                &card,
                Stroke::default()
                    .with_color(Color { a: CARD_BORDER.a * alpha, ..CARD_BORDER })
                    .with_width((0.75 * zoom).max(0.5)),
            );
        }
    }

    // MARK: Drawing

    /// The plane each card is drawn in, by node index, and the deepest one
    /// used. Without `lift`, or for a selected card or a frame, it's 0 — the
    /// selected are lifted into the overlay anyway, and a frame is a backdrop,
    /// enclosing cards rather than covering them.
    fn stacking(&self, lift: bool) -> (Vec<usize>, usize) {
        let mut planes = vec![0; self.nodes.len()];
        if !lift {
            return (planes, 0);
        }
        let cards: Vec<usize> = (0..self.nodes.len())
            .filter(|&i| !self.nodes[i].is_group() && !self.selected.contains(&i))
            .collect();
        let rects: Vec<Rectangle> = cards.iter().map(|&i| self.node_rect(&self.nodes[i])).collect();
        let mut deepest = 0;
        for (&index, plane) in cards.iter().zip(stacking_planes(&rects, LIFTED_PLANES)) {
            planes[index] = plane;
            deepest = deepest.max(plane);
        }
        (planes, deepest)
    }

    /// Whether the overlay holds something that covers a card in the scene
    /// beneath it — a selected card or a suggestion card over another card —
    /// and so needs a layer of its own to be drawn over it.
    fn overlay_covers_cards(&self) -> bool {
        let (mut over, mut under) = (Vec::new(), Vec::new());
        for (index, node) in self.nodes.iter().enumerate().filter(|(_, node)| !node.is_group()) {
            let rect = self.node_rect(node);
            if self.selected.contains(&index) { over.push(rect) } else { under.push(rect) }
        }
        over.extend(self.ghosts.iter().map(|ghost| ghost.rect));
        covers_any(&over, &under)
    }

    fn draw_nodes(&self, frame: &mut Frame, pass: NodePass) {
        let zoom = self.camera.zoom;

        let scene_alpha = self.node_layer_alpha();

        for (index, node) in self.nodes.iter().enumerate() {
            let selected = self.selected.contains(&index);
            let skip = match pass {
                NodePass::All => false,
                NodePass::Plane(plane, planes) => selected || planes.get(index) != Some(&plane),
                NodePass::SelectedOnly => !selected,
            };
            // Groups are frames, drawn in their own pass behind everything.
            if skip || node.is_group() {
                continue;
            }
            let rect = self.node_screen_rect(index, node);
            let alpha = scene_alpha * self.spawn_factors(index).1 * self.search_dim(index);

            if node.node_kind() == NodeKind::Image {
                let radius = self.card_radius();

                if let Some(handle) = self.images.get(&node.id) {
                    // The image shader masks corners with a rounded-box SDF
                    // applied to the image's *clip bounds* — which the frame
                    // normally sets to the whole canvas. Drawing inside
                    // with_clip(rect) makes clip bounds == image rect, so the
                    // radius actually rounds the visible corners.
                    let mut image = canvas::Image::new(handle.clone());
                    image.border_radius = self.frame_radius().into();
                    image.opacity = alpha;
                    frame.with_clip(rect, |clipped| {
                        clipped.draw_image(rect, image);
                    });
                }

                // Images always render above stroked geometry within a wgpu
                // layer, so a stroke on the image edge is partially covered
                // and shimmers during zoom. Stroke it slightly outside the
                // image instead (concentric: radius grows by the inflate):
                // only the inner quarter overlaps, the visible ring never
                // fights the image for subpixels.
                let (border_color, border_width) = match (selected, node.palette_color().map(IntoColor::into_color)) {
                    (true, Some(color)) => (color, selected_border(zoom)),
                    (true, None) => (self.theme.accent, selected_border(zoom)),
                    (false, Some(color)) => (color, (1.2 * zoom).max(1.0)),
                    (false, None) => (CARD_BORDER, (1.0 * zoom).max(1.0)),
                };
                let inflate = border_width * 0.25;
                let border = Path::rounded_rectangle(
                    rect.position() - Vector::new(inflate, inflate),
                    Size::new(
                        rect.width + inflate * 2.0,
                        rect.height + inflate * 2.0,
                    ),
                    (radius + inflate).into(),
                );
                frame.stroke(
                    &border,
                    Stroke::default()
                        .with_color(fade(border_color, alpha))
                        .with_width(border_width),
                );
                continue;
            }

            let card = rounded_path(rect, self.card_radius());
            let accent = node.palette_color().map(IntoColor::into_color);

            let fill = match accent {
                Some(color) => Color {
                    a: self.config.colored_card_opacity,
                    ..blend(self.theme.card_fill, color, 0.15)
                },
                None => self.theme.card_fill,
            };
            frame.fill(&card, Color { a: fill.a * alpha, ..fill });

            // A note, attachment or link card shows content that isn't in the
            // document — the note's body from the vault, or the file name / URL
            // — so it draws its own way and skips the raster path entirely.
            if node.node_kind() != NodeKind::Text {
                self.draw_reference_card(frame, node, rect, alpha);
                self.stroke_card(frame, &card, selected, accent, alpha);
                continue;
            }

            if self.editing != Some(index) {
                let text = node.text.as_deref().unwrap_or("(no content)");
                let wrap = crate::text_raster::card_text_wrap(node.width);
                let highlight = self.search.as_ref().map(|s| {
                    let current = s.current_hit.and_then(|(node, ordinal)| {
                        (node == index).then_some(ordinal)
                    });
                    (s.query, current)
                });

                // A scrollable node (scrollbars on, content overflows) draws the
                // live text scrolled + clipped, with a draggable thumb — the raster
                // snapshot only holds the visible top, so it can't scroll.
                let scrollbar = self.scrollable(node).then(|| {
                    let content = self.node_content_height(text, node.width);
                    let scroll = self.node_scroll_offset(node);
                    scrollbar_thumb(rect, node.height, content, scroll, zoom).map(|(t, _)| (t, scroll))
                }).flatten();

                if let Some((thumb, scroll)) = scrollbar {
                    // Top-aligned and shifted by the scroll (see `text_frame`).
                    let scrolled = text_frame(rect, Some(scroll), zoom);
                    // Before the clip: a clipped frame's fills end up beneath
                    // the card's own. See `draw_search_hits`.
                    self.draw_search_hits(frame, scrolled, text, FONT_SIZE, wrap, alpha, highlight);
                    self.draw_quote_bars(frame, scrolled, rect, text, FONT_SIZE, wrap, alpha);
                    self.draw_table_rules(frame, scrolled, rect, text, FONT_SIZE, wrap, alpha);
                    frame.with_clip(rect, |clipped| {
                        self.draw_markdown_live(clipped, scrolled, text, FONT_SIZE, wrap, alpha, highlight);
                    });
                    let color = Color { a: 0.55 * alpha, ..self.theme.text_muted };
                    frame.fill(
                        &Path::rounded_rectangle(thumb.position(), thumb.size(), (thumb.width / 2.0).into()),
                        color,
                    );
                } else {
                    let rasterize = self.config.rasterize_text;
                    let active = rasterize.then(|| self.text_snapshots.get(&index)).flatten();
                    let staged = rasterize.then(|| self.staging_snapshots.get(&index)).flatten();
                    if let Some(snapshot) = active {
                        // Current bitmap; a re-rasterized one cross-fades in over it.
                        let mut image = canvas::Image::new(snapshot.handle.clone());
                        image.opacity = alpha;
                        frame.draw_image(rect, image);
                        if let Some(staged) = staged {
                            let mut fresh = canvas::Image::new(staged.snapshot.handle.clone());
                            fresh.opacity = alpha * staged.opacity;
                            frame.draw_image(rect, fresh);
                        }
                    } else {
                        // No bitmap yet: draw live Markdown, warming any pending
                        // texture invisibly underneath so its promotion doesn't flash.
                        if let Some(staged) = staged {
                            let mut warming = canvas::Image::new(staged.snapshot.handle.clone());
                            warming.opacity = 0.0;
                            frame.draw_image(rect, warming);
                        }
                        // Before the clip: a clipped frame's fills end up
                        // beneath the card's own. See `draw_search_hits`.
                        self.draw_search_hits(frame, rect, text, FONT_SIZE, wrap, alpha, highlight);
                        self.draw_quote_bars(frame, rect, rect, text, FONT_SIZE, wrap, alpha);
                        self.draw_table_rules(frame, rect, rect, text, FONT_SIZE, wrap, alpha);
                        // Clip to the card so text that overflows a manually-undersized
                        // node doesn't spill past its border (the raster path is already
                        // node-sized, so it clips on its own).
                        frame.with_clip(rect, |clipped| {
                            self.draw_markdown_live(clipped, rect, text, FONT_SIZE, wrap, alpha, highlight);
                        });
                    }
                }
            }

            self.stroke_card(frame, &card, selected, accent, alpha);
        }
    }

    /// Draws the group frames: a rounded outline with a barely-there wash, and
    /// the group's label sitting on the line at the top-left — a backdrop the
    /// member nodes are then drawn on top of.
    ///
    /// Unlike a card this is deliberately almost transparent: a group encloses
    /// other nodes, so anything more would tint everything inside it.
    fn draw_groups(&self, frame: &mut Frame) {
        let zoom = self.camera.zoom;
        let alpha = self.node_layer_alpha();
        if alpha <= 0.0 {
            return;
        }

        for (index, node) in self.nodes.iter().enumerate().filter(|(_, n)| n.is_group()) {
            let alpha = alpha * self.search_dim(index);
            let rect = self.node_screen_rect(index, node);
            let selected = self.selected.contains(&index);
            let accent = node
                .palette_color()
                .map(IntoColor::into_color)
                .unwrap_or(self.theme.text_muted);

            let frame_path = rounded_path(rect, self.frame_radius());
            frame.fill(&frame_path, Color { a: 0.05 * alpha, ..accent });
            frame.stroke(
                &frame_path,
                Stroke::default()
                    .with_color(Color {
                        a: if selected { 0.9 * alpha } else { 0.45 * alpha },
                        ..accent
                    })
                    .with_width(if selected { (2.5 * zoom).max(2.0) } else { (1.5 * zoom).max(1.0) }),
            );

            // While its caption is being edited the editor draws the text.
            let Some(label) = node
                .label()
                .filter(|l| !l.trim().is_empty())
                .filter(|_| self.editing != Some(index))
            else {
                continue;
            };
            // The caption sits inside the top edge rather than above it, so it
            // stays on screen when the frame's top is scrolled off.
            let size = (GROUP_LABEL_SIZE * zoom).max(9.0);
            frame.fill_text(canvas::Text {
                content: label.to_owned(),
                position: Point::new(
                    rect.x + GROUP_LABEL_PAD * zoom,
                    rect.y + GROUP_LABEL_PAD * zoom,
                ),
                color: Color { a: 0.85 * alpha, ..accent },
                size: size.into(),
                font: self.theme.ui_font(),
                ..canvas::Text::default()
            });
        }
    }

    /// Draws a card whose content lives outside the document: a Markdown note
    /// or other file from the vault, or a URL bookmark. Each gets a title line
    /// and, where there is one, a body — all drawn live, since none of it is in
    /// `node.text` for the rasterizer to have picked up.
    /// Alignment differs by what the card *is*, deliberately: a note or an
    /// attachment is a document, so it reads from the top-left under a header;
    /// a link is a single label, so it sits centred in its card the way a text
    /// note's own content does.
    fn draw_reference_card(
        &self,
        frame: &mut Frame,
        node: &CanvasNode,
        rect: Rectangle,
        alpha: f32,
    ) {
        frame.with_clip(rect, |clipped| match node.node_kind() {
            NodeKind::Link => self.draw_link_card(clipped, node, rect, alpha),
            _ => self.draw_note_card(clipped, node, rect, alpha),
        });
    }

    /// A vault file embedded in the board: its name as a muted header over a
    /// hairline rule, then the note's Markdown body (or, for a file that isn't
    /// a note, what kind of file it is).
    fn draw_note_card(&self, frame: &mut Frame, node: &CanvasNode, rect: Rectangle, alpha: f32) {
        let zoom = self.camera.zoom;
        let pad = NODE_TEXT_PAD_X * zoom;
        let attachment = node.node_kind() == NodeKind::Attachment;
        let note = self.notes.get(&node.id).map(String::as_str);
        // An attachment is named in full: without its extension nothing said
        // what kind of file the card stood for.
        let name = if attachment { file_name(node) } else { node.file_title() };
        // A note whose first heading is its own name already says it: the
        // header repeated it, one line above the same words set large.
        let header = name.filter(|name| !note.is_some_and(|text| first_heading_is(text, name)));

        let mut body_y = rect.y + NODE_TEXT_PAD_Y * zoom / 2.0;
        if let Some(name) = header {
            // The header is deliberately smaller and muted: it names the file,
            // while the body below is the content. Sized like the title, it
            // read as a stray first line of the note.
            let title_size = (REFERENCE_TITLE_SIZE * zoom).max(MIN_TEXT_PX);
            frame.fill_text(canvas::Text {
                content: name,
                position: Point::new(rect.x + pad, body_y),
                color: Color { a: 0.9 * alpha, ..self.theme.text_muted },
                size: title_size.into(),
                font: self.theme.ui_font(),
                ..canvas::Text::default()
            });

            // A rule under the header separates it from the body, the way
            // Obsidian's embedded-note header does.
            let rule_y = body_y + title_size * crate::text_raster::LINE_HEIGHT;
            frame.fill(
                &Path::rectangle(
                    Point::new(rect.x + pad, rule_y),
                    Size::new((rect.width - pad * 2.0).max(0.0), (1.0 * zoom).max(0.5)),
                ),
                Color { a: 0.6 * alpha, ..self.theme.border },
            );
            body_y = rule_y + NODE_TEXT_PAD_Y * zoom / 2.0;
        }

        match note {
            Some(text) if !text.trim().is_empty() => {
                // Top-aligned (height 0 zeroes the layout's vertical centering)
                // and started below the rule.
                let origin = Rectangle {
                    x: rect.x,
                    y: body_y,
                    width: rect.width,
                    height: 0.0,
                };
                let wrap = crate::text_raster::card_text_wrap(node.width);
                self.draw_markdown_live(frame, origin, text, FONT_SIZE, wrap, alpha, None);
            }
            _ => {
                // Nothing to show: say what the file is, or why there's no
                // note, rather than leave a blank card. "(not previewable)"
                // here read like an error for a file that was simply a PDF.
                let reason = if attachment {
                    file_kind(node)
                } else {
                    "(note not found)".to_owned()
                };
                frame.fill_text(canvas::Text {
                    content: reason,
                    position: Point::new(rect.x + pad, body_y),
                    color: Color { a: 0.7 * alpha, ..self.theme.text_muted },
                    size: ((FONT_SIZE - 2.0) * zoom).max(MIN_TEXT_PX).into(),
                    font: self.theme.ui_font(),
                    ..canvas::Text::default()
                });
            }
        }
    }

    /// A URL bookmark: the host as the headline with the full URL muted beneath
    /// it, centred in the card. There's no page title to show without fetching
    /// the page, and the host is the part that identifies it at a glance.
    fn draw_link_card(&self, frame: &mut Frame, node: &CanvasNode, rect: Rectangle, alpha: f32) {
        let zoom = self.camera.zoom;
        let url = node.url().unwrap_or("(no URL)");
        let host = url_host(url);

        let host_size = (FONT_SIZE * zoom).max(MIN_TEXT_PX);
        // A whole pixel under the floor: the URL is the secondary line and is
        // allowed to fade out ahead of the host it sits below.
        let url_size = ((FONT_SIZE - 4.0) * zoom).max(MIN_TEXT_PX - 1.0);
        let line = crate::text_raster::LINE_HEIGHT;
        // Both lines as one block, centred on the card.
        let show_url = url != host;
        let block = host_size * line + if show_url { url_size * line } else { 0.0 };
        let top = rect.y + (rect.height - block).max(0.0) / 2.0;
        let center_x = rect.x + rect.width / 2.0;

        frame.fill_text(canvas::Text {
            content: host.to_owned(),
            position: Point::new(center_x, top),
            color: Color { a: alpha, ..self.theme.text },
            size: host_size.into(),
            font: self.theme.ui_font(),
            align_x: TextAlignment::Center,
            ..canvas::Text::default()
        });
        if show_url {
            frame.fill_text(canvas::Text {
                content: url.to_owned(),
                position: Point::new(center_x, top + host_size * line),
                color: Color { a: 0.75 * alpha, ..self.theme.text_muted },
                size: url_size.into(),
                font: self.theme.ui_font(),
                align_x: TextAlignment::Center,
                ..canvas::Text::default()
            });
        }
    }

    /// The rounded border around a card, in the node's own color when it has
    /// one. Shared by every card kind.
    fn stroke_card(
        &self,
        frame: &mut Frame,
        card: &Path,
        selected: bool,
        accent: Option<Color>,
        alpha: f32,
    ) {
        let zoom = self.camera.zoom;
        let (border_color, border_width) = match (selected, accent) {
            // A selected colored node keeps its own color (just thicker)
            // rather than turning blue, so the chosen color stays visible.
            (true, Some(color)) => (color, selected_border(zoom)),
            (true, None) => (self.theme.accent, selected_border(zoom)),
            (false, Some(color)) => (color, 1.5 * zoom),
            (false, None) => (CARD_BORDER, (0.75 * zoom).max(0.5)),
        };
        frame.stroke(
            card,
            Stroke::default()
                .with_color(fade(border_color, alpha))
                .with_width(border_width),
        );
    }

    /// Draws the scrollbar thumb for the card being edited.
    ///
    /// The read path derives the thumb from `node_scroll`, which the editor
    /// doesn't use — it scrolls itself to keep the caret in view. So the
    /// position comes from the editor instead (`editor_scroll`, a fraction),
    /// mapped onto the same overflow the read path measures so the thumb is the
    /// same size and travel in both modes.
    fn draw_editing_thumb(&self, frame: &mut Frame, index: usize) {
        let Some(node) = self.nodes.get(index).filter(|n| self.scrollable(n)) else {
            return;
        };
        // Read here rather than taking it as a field: the editor publishes its
        // scroll while laying out, which happens after `view` has built this
        // widget. A value captured there is always one update stale — the thumb
        // would sit still on a page-down and only catch up on the next key.
        let fraction = iced_graphics::text::editor::editor_scroll_fraction();
        let rect = self.node_screen_rect(index, node);
        let alpha = self.node_layer_alpha();
        let text = node.text.as_deref().unwrap_or("");
        let content = self.node_content_height(text, node.width);
        let Some(thumb) = editing_thumb(rect, node.height, content, fraction, self.camera.zoom)
        else {
            return;
        };
        frame.fill(
            &Path::rounded_rectangle(thumb.position(), thumb.size(), (thumb.width / 2.0).into()),
            Color { a: 0.55 * alpha, ..self.theme.text_muted },
        );
    }

    /// Draws Markdown text live (not from a bitmap) into a card's screen
    /// `rect`: one `fill_text` per styled run, laid out to match the rasterized
    /// path. Shared by node cards and cluster labels. `font_size`/`wrap_width`
    /// are logical (unzoomed), matching the arguments the raster path passes to
    /// `snapshot_markdown`.
    #[allow(clippy::too_many_arguments)] // mirrors with_markdown_layout's params
    fn draw_markdown_live(
        &self,
        frame: &mut Frame,
        rect: Rectangle,
        text: &str,
        font_size: f32,
        wrap_width: f32,
        alpha: f32,
        // `Some((query, current))` highlights search matches; `current` is the
        // ordinal of the hit being stepped through, when it's in this node.
        highlight: Option<(&str, Option<usize>)>,
    ) {
        let zoom = self.camera.zoom;
        // The cached layout is in logical (unzoomed) coordinates; scale each run
        // to screen at draw time so the cache is zoom-invariant.
        crate::text_raster::with_markdown_layout(
            text,
            rect.width / zoom,
            rect.height / zoom,
            wrap_width,
            font_size,
            *self.theme,
            self.config.center_node_text,
            |runs| {
                // Where the search hits fall, so the glyphs over one can be
                // inked to read against it. The fills themselves are painted by
                // `draw_search_hits`, into the *parent* frame — see there.
                let query = highlight.map(|(q, _)| q).filter(|q| !q.is_empty());
                let hits: Vec<Vec<Hit>> = match query {
                    Some(query) => runs.iter().map(|r| self.match_hits(r, query)).collect(),
                    None => Vec::new(),
                };

                for (index, run) in runs.iter().enumerate() {
                    let mut font = crate::UI_FONT;
                    // Match the raster path: body/bold use the theme's declared
                    // weights (a face is synthesized at each in `startup::run`).
                    font.weight = crate::theme::iced_weight(if run.bold {
                        self.theme.weight_bold
                    } else {
                        self.theme.weight_normal
                    });
                    if run.italic {
                        font.style = iced::font::Style::Italic;
                    }
                    font.family = if run.mono {
                        self.theme
                            .font_mono
                            .map(iced::font::Family::Name)
                            .unwrap_or(iced::font::Family::Monospace)
                    } else {
                        self.theme
                            .font_text
                            .map(iced::font::Family::Name)
                            .unwrap_or(iced::font::Family::Name(crate::platform::UI_FONT_NAME))
                    };
                    let mut piece = |text: &str, x: f32, color: Color| {
                        if text.is_empty() {
                            return;
                        }
                        frame.fill_text(canvas::Text {
                            content: text.to_owned(),
                            position: Point::new(
                                rect.x + (run.x + x) * zoom,
                                rect.y + run.y * zoom,
                            ),
                            color: fade(color, alpha),
                            size: (run.size * zoom).into(),
                            line_height: LineHeight::Relative(crate::text_raster::LINE_HEIGHT),
                            font,
                            align_x: TextAlignment::Left,
                            align_y: Vertical::Top,
                            shaping: Shaping::Advanced,
                            ..canvas::Text::default()
                        });
                    };

                    // Split the run on its hits, so the glyphs sitting on a
                    // fill can be inked to read against it. With no query
                    // that's one piece, which is the whole run.
                    let ink = SEARCH_INK;
                    let mut at = 0usize;
                    let mut x = 0.0;
                    for hit in hits.get(index).into_iter().flatten() {
                        piece(&run.text[at..hit.start], x, run.color);
                        piece(&run.text[hit.start..hit.end], hit.x, ink);
                        at = hit.end;
                        x = hit.x + hit.width;
                    }
                    piece(&run.text[at..], x, run.color);
                }
            },
        );
    }

    /// Paints a highlight rect behind each occurrence of `query` (case-
    /// insensitive) within a single laid-out run. Occurrences that cross a run
    /// (style/line) boundary aren't caught — acceptable for plain notes.
    /// Where `query` occurs in one laid-out run, case-insensitively.
    ///
    /// Occurrences that straddle a run boundary (a style or line change) aren't
    /// caught — acceptable for plain notes, and the alternative is matching
    /// across text that isn't adjacent on screen.
    fn match_hits(&self, run: &crate::text_raster::TextRun, query: &str) -> Vec<Hit> {
        // Measured as the run is set — face, weight, slant, features — or a
        // hit's box sits against different letterforms than the ones under it.
        let width = |text: &str| crate::text_raster::run_advance(text, run, self.theme);
        hit_ranges(&run.text, query)
            .into_iter()
            .map(|(start, end)| Hit {
                start,
                end,
                x: width(&run.text[..start]),
                width: width(&run.text[start..end]),
            })
            .collect()
    }

    /// Paints the search hits behind a card's text.
    ///
    /// **Deliberately not part of `draw_markdown_live`,** which runs inside a
    /// `with_clip`. A clipped frame's meshes are pasted into the parent's mesh
    /// list at `paste`, but the parent's own fills sit in a buffer stack that
    /// isn't flushed into that list until `into_geometry` — so *everything* the
    /// parent filled, the card included, is appended afterwards and paints over
    /// anything a clipped child drew. Text escapes it by living in a separate
    /// list drawn above all meshes, which is why the ink showed and the
    /// highlight didn't. The board's scrollbar thumb is filled outside its clip
    /// for the same reason.
    #[allow(clippy::too_many_arguments)] // mirrors `draw_markdown_live`'s
    fn draw_search_hits(
        &self,
        frame: &mut Frame,
        rect: Rectangle,
        text: &str,
        font_size: f32,
        wrap_width: f32,
        alpha: f32,
        highlight: Option<(&str, Option<usize>)>,
    ) {
        let Some((query, current)) = highlight.filter(|(q, _)| !q.is_empty()) else {
            return;
        };
        let zoom = self.camera.zoom;
        crate::text_raster::with_markdown_layout(
            text,
            rect.width / zoom,
            rect.height / zoom,
            wrap_width,
            font_size,
            *self.theme,
            self.config.center_node_text,
            |runs| {
                // Counted across the node's runs, in reading order, so the
                // ordinal means the same thing on both sides.
                let mut ordinal = 0;
                for run in runs {
                    for hit in self.match_hits(run, query) {
                        self.fill_hit(frame, rect, run, &hit, current == Some(ordinal), alpha);
                        ordinal += 1;
                    }
                }
            },
        );
    }

    /// Draws the bar beside each line of a quote, for the live path — the
    /// bitmap path paints its own (`text_raster::snapshot_markdown`).
    ///
    /// Before the clip, like the search hits and for the same reason, so it's
    /// clipped to `card` by arithmetic instead: a scrolled card lays its text
    /// out above and below itself.
    #[allow(clippy::too_many_arguments)] // mirrors `draw_markdown_live`'s
    fn draw_quote_bars(
        &self,
        frame: &mut Frame,
        rect: Rectangle,
        card: Rectangle,
        text: &str,
        font_size: f32,
        wrap_width: f32,
        alpha: f32,
    ) {
        use crate::text_raster::{LINE_HEIGHT, MD_PADDING, QUOTE_BAR_INSET, QUOTE_BAR_WIDTH};
        if !text.contains('>') {
            return; // no quote, no layout to fetch
        }
        let zoom = self.camera.zoom;
        let color = crate::text_raster::quote_bar_color(self.theme);
        crate::text_raster::with_markdown_layout(
            text,
            rect.width / zoom,
            rect.height / zoom,
            wrap_width,
            font_size,
            *self.theme,
            self.config.center_node_text,
            |runs| {
                // The text column's left edge, as the layout placed it.
                let column = (rect.width / zoom - (wrap_width - 2.0 * MD_PADDING).max(1.0)) / 2.0;
                for run in runs.iter().filter(|run| run.quote) {
                    let bar = Rectangle::new(
                        Point::new(rect.x + (column - QUOTE_BAR_INSET) * zoom, rect.y + run.y * zoom),
                        Size::new(QUOTE_BAR_WIDTH * zoom, run.size * LINE_HEIGHT * zoom),
                    );
                    if let Some(bar) = bar.intersection(&card) {
                        frame.fill_rectangle(bar.position(), bar.size(), Color { a: color.a * alpha, ..color });
                    }
                }
            },
        );
    }

    /// Draws the rules of a card's tables, for the live path — the bitmap path
    /// paints its own (`text_raster::snapshot_markdown`). Before the clip and
    /// clipped by arithmetic, like the quote bars and for the same reason; and
    /// never thinner than a pixel on screen, or a table zoomed out loses its
    /// grid to antialiasing.
    #[allow(clippy::too_many_arguments)] // mirrors `draw_quote_bars`'
    fn draw_table_rules(
        &self,
        frame: &mut Frame,
        rect: Rectangle,
        card: Rectangle,
        text: &str,
        font_size: f32,
        wrap_width: f32,
        alpha: f32,
    ) {
        if !text.contains('|') {
            return; // no table, no layout to fetch
        }
        let zoom = self.camera.zoom;
        let color = crate::text_raster::table_rule_color(self.theme);
        crate::text_raster::with_markdown_rules(
            text,
            rect.width / zoom,
            rect.height / zoom,
            wrap_width,
            font_size,
            *self.theme,
            self.config.center_node_text,
            |rules| {
                for rule in rules {
                    let line = Rectangle::new(
                        Point::new(rect.x + rule.x * zoom, rect.y + rule.y * zoom),
                        Size::new((rule.width * zoom).max(1.0), (rule.height * zoom).max(1.0)),
                    );
                    if let Some(line) = line.intersection(&card) {
                        frame.fill_rectangle(line.position(), line.size(), Color { a: color.a * alpha, ..color });
                    }
                }
            },
        );
    }

    /// Paints one hit's background. Opaque, with [`SEARCH_INK`] drawn over it —
    /// which is what lets the fill be as strong as it likes instead of having
    /// to leave the card's own text readable through it.
    fn fill_hit(
        &self,
        frame: &mut Frame,
        rect: Rectangle,
        run: &crate::text_raster::TextRun,
        hit: &Hit,
        current: bool,
        alpha: f32,
    ) {
        let zoom = self.camera.zoom;
        let fill = if current { SEARCH_HIT_CURRENT } else { SEARCH_HIT };
        frame.fill_rectangle(
            Point::new(rect.x + (run.x + hit.x) * zoom, rect.y + run.y * zoom),
            Size::new(
                hit.width * zoom,
                run.size * crate::text_raster::LINE_HEIGHT * zoom,
            ),
            fade(fill, alpha),
        );
    }

    /// Screen-space bezier geometry of an edge, or `None` if an endpoint
    /// node is missing.
    fn edge_geometry(&self, edge: &CanvasEdge) -> Option<EdgeGeometry> {
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
    fn edge_at(&self, point: Point) -> Option<usize> {
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

    fn draw_edges(&self, frame: &mut Frame) {
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
    fn edge_color(&self, edge: &CanvasEdge) -> Color {
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
    fn draw_edge_label(&self, frame: &mut Frame, index: usize, edge: &CanvasEdge) {
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

    fn draw_edge_outline(&self, frame: &mut Frame, index: usize, color: Color, thickness: f32) {
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
    fn draw_edge_line(&self, frame: &mut Frame, index: usize, color: Color) {
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

    /// The suggestion card under `world`, if any (including the placeholder,
    /// which can be dragged but not accepted).
    fn ghost_at(&self, world: Point) -> Option<usize> {
        self.ghosts.iter().position(|ghost| ghost.rect.contains(world))
    }

    /// The ghost whose "×" (top-right corner) is under `world`, if any —
    /// including the "…" placeholder, whose "×" cancels the request.
    fn ghost_dismiss_at(&self, world: Point) -> Option<usize> {
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
    fn ghost_link(&self, ghost: &Ghost) -> Option<EdgeGeometry> {
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
    fn draw_ghosts(&self, frame: &mut Frame) {
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
    fn draw_flow_shimmer(&self, frame: &mut Frame, flow: &AiFlow) {
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

    /// A port dot at the center of the given node side. Drawn through the
    /// image pipeline (not as a mesh): images render above meshes within a
    /// canvas layer, so a mesh dot would disappear under image nodes.
    /// Draws the snap grid as dots around the node(s) being dragged (never over
    /// a node's body), each fading with distance from a dragged node so its
    /// whole perimeter lights up regardless of size. `dragging` is the grabbed
    /// node; a multi-selection lights every selected node (they move together).
    /// Called only while dragging with snap-to-grid on, out of cluster mode.
    fn draw_snap_dots(&self, frame: &mut Frame, dragging: usize) {
        let grid = self.config.snap_grid_size;
        let zoom = self.camera.zoom;
        // Nothing to snap to, or too zoomed out for the grid to read.
        if grid <= 0.0 || grid * zoom < SNAP_DOT_MIN_SPACING {
            return;
        }
        let margin = grid * SNAP_NODE_RINGS;

        let rects: Vec<Rectangle> = self.nodes.iter().map(|n| self.node_rect(n)).collect();
        // Light sources: the whole selection when dragging one of a
        // multi-selection (they move together), else just the dragged node.
        let dragged_rects: Vec<Rectangle> =
            if self.selected.contains(&dragging) && self.selected.len() > 1 {
                self.selected
                    .iter()
                    .filter_map(|&j| rects.get(j).copied())
                    .collect()
            } else {
                rects.get(dragging).copied().into_iter().collect()
            };
        if dragged_rects.is_empty() {
            return;
        }

        let smooth = |t: f32| t * t * (3.0 - 2.0 * t);

        // Lattice points are drawn once even where two halos overlap, so shared
        // dots keep a single, consistent opacity.
        let mut drawn: HashSet<(i32, i32)> = HashSet::new();
        for rect in &rects {
            let halo = expand_rect(*rect, margin);
            // Only nodes whose halo the drag glow can reach.
            if !dragged_rects
                .iter()
                .any(|dr| expand_rect(*dr, margin).intersects(&halo))
            {
                continue;
            }
            let gx0 = (halo.x / grid).ceil() as i32;
            let gx1 = ((halo.x + halo.width) / grid).floor() as i32;
            let gy0 = (halo.y / grid).ceil() as i32;
            let gy1 = ((halo.y + halo.height) / grid).floor() as i32;
            for gx in gx0..=gx1 {
                for gy in gy0..=gy1 {
                    if !drawn.insert((gx, gy)) {
                        continue;
                    }
                    let world = Point::new(gx as f32 * grid, gy as f32 * grid);
                    // Only around nodes, never over one's body.
                    if rects.iter().any(|r| strictly_inside(*r, world)) {
                        continue;
                    }
                    // Brightness fades with distance from the nearest dragged node.
                    let mut light = 0.0_f32;
                    for dr in &dragged_rects {
                        let dn = rect_distance(*dr, world);
                        if dn < margin {
                            light = light.max(smooth(1.0 - dn / margin));
                        }
                    }
                    let alpha = light * SNAP_DOT_ALPHA * self.reveal;
                    if alpha <= 0.01 {
                        continue;
                    }
                    frame.fill(
                        &Path::circle(self.camera.to_screen(world), SNAP_DOT_RADIUS),
                        Color { a: alpha, ..self.theme.text },
                    );
                }
            }
        }
    }

    fn draw_port(&self, frame: &mut Frame, index: usize, side: &str) {
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
    fn draw_connection_preview(
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
    fn draw_arrowhead(
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
    fn head(&self, length: f32, stroke: f32) -> Head {
        Head { tip: ArrowTip::of(self.config), length, stroke }
    }

    /// `color` as solid as links are drawn (`config.link_opacity`).
    fn link_ink(&self, color: Color) -> Color {
        Color { a: color.a * self.config.link_opacity, ..color }
    }

    /// Arrows between connected clusters, drawn behind the cluster cards.
    /// Sized larger than note edges since clusters are large.
    fn draw_cluster_links(&self, frame: &mut Frame) {
        let alpha = self.cluster_layer_alpha();
        let zoom = self.camera.zoom;
        let stroke_width = (9.0 * zoom).max(1.5);
        let head = (26.0 * zoom).max(6.0);

        for link in self.cluster_links {
            let (Some(from), Some(to)) =
                (self.clusters.get(link.from), self.clusters.get(link.to))
            else {
                continue;
            };
            let from_rect = self.camera.rect_to_screen(from.bounds);
            let to_rect = self.camera.rect_to_screen(to.bounds);

            let from_side = facing_side(to_rect.center(), from_rect);
            let to_side = facing_side(from_rect.center(), to_rect);
            let start = side_anchor(from_rect, from_side);
            let cluster_head = self.head(head, stroke_width * 0.8);
            let end = arrow_tip(side_anchor(to_rect, to_side), to_side, zoom, cluster_head);

            let reach = start.distance(end) * 0.4;
            let control1 = start + outward(from_side) * reach;
            let control2 = end + outward(to_side) * reach;

            let g = EdgeGeometry { start, control1, control2, end, alpha: 1.0 };
            let (trims, wedges) = shaft([None, Some((end, to_side, cluster_head))], stroke_width);
            let ink = self.link_ink(EDGE_COLOR);
            let color = Color { a: ink.a * alpha, ..ink };
            fill_wedges(frame, &wedges, color);
            frame.stroke(
                &self.link_path_trimmed(None, &g, trims),
                Stroke {
                    // Butt, to meet the wedge into its arrowhead — see `shaft`.
                    line_cap: stroke::LineCap::Butt,
                    line_join: stroke::LineJoin::Round,
                    ..Stroke::default()
                        .with_color(color)
                        .with_width(stroke_width)
                },
            );
            self.draw_arrowhead(frame, end, to_side, alpha, cluster_head, ink, false);
        }
    }
}

/// The last component of a file node's path, extension included.
fn file_name(node: &CanvasNode) -> Option<String> {
    let file = node.file.as_deref()?;
    file.rsplit(['/', '\\']).next().filter(|name| !name.is_empty()).map(str::to_owned)
}

/// What kind of file an attachment is, from its extension: "PDF file".
fn file_kind(node: &CanvasNode) -> String {
    file_name(node)
        .and_then(|name| name.rsplit_once('.').map(|(_, ext)| ext.to_uppercase()))
        .filter(|ext| !ext.is_empty())
        .map_or_else(|| "File".to_owned(), |ext| format!("{ext} file"))
}

/// Whether `text`'s first heading, before anything else, is `name`.
fn first_heading_is(text: &str, name: &str) -> bool {
    text.lines()
        .find(|line| !line.trim().is_empty())
        .and_then(|line| line.trim_start().strip_prefix("# "))
        .is_some_and(|heading| heading.trim().eq_ignore_ascii_case(name.trim()))
}

/// The cubic bezier path of an edge (screen space).
/// The axis-aligned rectangle spanning two corner points.
/// The header strip along a group frame's top edge — the caption's band, and
/// the only part of a frame the mouse can grab. `scale` is the zoom for a
/// screen-space rect, or 1.0 for a world-space one.
/// The host part of a URL, for a link card's headline: the scheme, any `www.`
/// prefix, and everything from the path on are dropped. Falls back to the whole
/// string when there's nothing recognisable to trim (so a malformed URL still
/// shows something).
fn url_host(url: &str) -> &str {
    let after_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    let host = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(after_scheme);
    let host = host.strip_prefix("www.").unwrap_or(host);
    if host.is_empty() { url } else { host }
}

pub(crate) fn group_header(rect: Rectangle, scale: f32) -> Rectangle {
    Rectangle {
        height: (GROUP_HEADER_HEIGHT * scale).min(rect.height),
        ..rect
    }
}

fn rect_from_points(a: Point, b: Point) -> Rectangle {
    Rectangle::new(
        Point::new(a.x.min(b.x), a.y.min(b.y)),
        Size::new((a.x - b.x).abs(), (a.y - b.y).abs()),
    )
}

/// Distance from `p` to the nearest point of `rect` (0 when inside).
fn rect_distance(rect: Rectangle, p: Point) -> f32 {
    let dx = (rect.x - p.x).max(p.x - (rect.x + rect.width)).max(0.0);
    let dy = (rect.y - p.y).max(p.y - (rect.y + rect.height)).max(0.0);
    (dx * dx + dy * dy).sqrt()
}

/// `rect` grown by `m` on every side.
fn expand_rect(rect: Rectangle, m: f32) -> Rectangle {
    Rectangle::new(
        Point::new(rect.x - m, rect.y - m),
        Size::new(rect.width + 2.0 * m, rect.height + 2.0 * m),
    )
}

/// Whether `p` is strictly inside `rect` (boundary excluded), so snap dots that
/// land exactly on a node's edge/corner still show but its interior stays clear.
fn strictly_inside(rect: Rectangle, p: Point) -> bool {
    p.x > rect.x && p.x < rect.x + rect.width && p.y > rect.y && p.y < rect.y + rect.height
}

impl Board<'_> {
    /// A link as the cubic pieces it is drawn from, under the configured
    /// routing.
    ///
    /// **Everything a link is goes through here** — the line, its hover and
    /// selection rings, the drag preview, ghost links, the AI shimmer, and the
    /// hit test and caption that have to agree with what was drawn. One routing
    /// to switch, and nothing left behind on the other.
    fn link_segments(&self, edge: Option<&CanvasEdge>, g: &EdgeGeometry) -> Vec<EdgeGeometry> {
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
    fn trunk(&self, edge: Option<&CanvasEdge>, g: &EdgeGeometry) -> f32 {
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
    fn link_path(&self, edge: Option<&CanvasEdge>, g: &EdgeGeometry) -> Path {
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
    fn link_path_trimmed(&self, edge: Option<&CanvasEdge>, g: &EdgeGeometry, trims: [f32; 2]) -> Path {
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
}

/// The path through a link's cubic pieces, in order.
fn path_through(segments: &[EdgeGeometry]) -> Path {
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

impl Board<'_> {
    /// How far `point` is from the link as drawn, and the box to reject on
    /// before measuring.
    fn link_distance(
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
    fn link_midpoint(&self, edge: &CanvasEdge, g: &EdgeGeometry) -> Point {
        let segments = self.link_segments(Some(edge), g);
        match segments.len() {
            0 => g.start,
            n => cubic_bezier(&segments[n / 2], 0.5),
        }
    }
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
fn mermaid_route(g: &EdgeGeometry, gutter: f32) -> Vec<EdgeGeometry> {
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
const EDGE_LINE_WIDTH: f32 = 2.5;

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
fn orthogonal_route(g: &EdgeGeometry, gutter: f32) -> Vec<EdgeGeometry> {
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
fn runs(points: &[Point], g: &EdgeGeometry) -> Vec<EdgeGeometry> {
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
const EDGE_GUTTER: f32 = 60.0;

/// The least a trunk may stand off the node it leaves, in world units — enough
/// to read as clear of it.
const MIN_TRUNK_GUTTER: f32 = 14.0;

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
fn trunk_gutter(nearest_ahead: f32, zoom: f32) -> f32 {
    let (gutter, minimum) = (EDGE_GUTTER * zoom, MIN_TRUNK_GUTTER * zoom);
    if nearest_ahead <= 0.0 {
        return gutter;
    }
    gutter.min(nearest_ahead * 0.4).max(minimum.min(nearest_ahead * 0.5))
}

/// The spline's cubic segments, and the short straight run d3 opens with.
///
/// Split out from the drawing so the shape can be checked without a canvas.
fn basis_segments(points: &[Point]) -> (Option<Point>, Vec<(Point, Point, Point)>) {
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

const RING_RADIUS: f32 = 30.0;

/// Lazily rasterized port dot: a blue disc with a white ring, antialiased.
/// Rendered once at 64px and GPU-scaled to the display size.
fn port_dot() -> &'static image::Handle {
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

fn outward(side: &str) -> Vector {
    match side.to_lowercase().as_str() {
        "left" => Vector::new(-1.0, 0.0),
        "right" => Vector::new(1.0, 0.0),
        "top" => Vector::new(0.0, -1.0),
        "bottom" => Vector::new(0.0, 1.0),
        _ => Vector::new(1.0, 0.0),
    }
}

/// A vector of length one, or zero where there is no direction to take.
fn unit(v: Vector) -> Vector {
    let len = (v.x * v.x + v.y * v.y).sqrt();
    if len > f32::EPSILON { v * (1.0 / len) } else { Vector::new(0.0, 0.0) }
}

fn lerp(a: Point, b: Point, t: f32) -> Point {
    Point::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t)
}

fn blend(base: Color, tint: Color, amount: f32) -> Color {
    Color {
        r: base.r + (tint.r - base.r) * amount,
        g: base.g + (tint.g - base.g) * amount,
        b: base.b + (tint.b - base.b) * amount,
        a: 1.0,
    }
}

#[cfg(test)]
mod node_kind_tests {
    use super::{GROUP_HEADER_HEIGHT, group_header, url_host};
    use iced::{Point, Rectangle, Size};

    #[test]
    fn a_link_headline_is_its_host() {
        assert_eq!(url_host("https://obsidian.md"), "obsidian.md");
        assert_eq!(url_host("https://www.example.com/a/b?c=1#d"), "example.com");
        assert_eq!(url_host("http://localhost:1234/v1"), "localhost:1234");
        // Nothing recognisable to trim: show it as-is rather than blank.
        assert_eq!(url_host("obsidian.md"), "obsidian.md");
        assert_eq!(url_host("not a url"), "not a url");
        assert_eq!(url_host("https://"), "https://");
    }

    #[test]
    fn a_frames_header_is_a_band_across_its_top() {
        let frame = Rectangle::new(Point::new(10.0, 20.0), Size::new(400.0, 300.0));
        let header = group_header(frame, 1.0);
        assert_eq!(header.position(), frame.position());
        assert_eq!(header.width, frame.width);
        assert_eq!(header.height, GROUP_HEADER_HEIGHT);
        // The interior is deliberately outside it, so clicks reach the nodes.
        assert!(header.contains(Point::new(200.0, 25.0)));
        assert!(!header.contains(Point::new(200.0, 200.0)));
    }

    #[test]
    fn a_frame_shorter_than_its_header_is_all_header() {
        // A frame dragged down to nothing still has to be grabbable.
        let tiny = Rectangle::new(Point::new(0.0, 0.0), Size::new(400.0, 10.0));
        assert_eq!(group_header(tiny, 1.0).height, 10.0);
    }
}

#[cfg(test)]
mod reference_card_tests {
    use super::{file_kind, first_heading_is};
    use thinkboard_core::fixtures::node;

    #[test]
    fn a_note_headed_with_its_own_name_needs_no_header() {
        assert!(first_heading_is("# Meeting notes\n\nbody", "Meeting notes"));
        assert!(first_heading_is("\n\n#  meeting NOTES  \n", "Meeting notes"), "blank lines, case, spacing");
        assert!(!first_heading_is("Intro first\n# Meeting notes", "Meeting notes"), "only the first line counts");
        assert!(!first_heading_is("## Meeting notes", "Meeting notes"), "only a title-level heading");
        assert!(!first_heading_is("# Agenda", "Meeting notes"));
    }

    #[test]
    fn an_attachment_says_what_kind_of_file_it_is() {
        let file = |path: &str| thinkboard_core::model::CanvasNode { file: Some(path.to_owned()), ..node("f") };
        assert_eq!(file_kind(&file("docs/Quarterly report.pdf")), "PDF file");
        assert_eq!(file_kind(&file("song.mp3")), "MP3 file");
        assert_eq!(file_kind(&file("README")), "File");
    }
}

#[cfg(test)]
mod snap_tests {
    use super::{rect_distance, strictly_inside};
    use iced::{Point, Rectangle, Size};

    #[test]
    fn strictly_inside_excludes_the_boundary() {
        let rect = Rectangle::new(Point::new(0.0, 0.0), Size::new(10.0, 10.0));
        assert!(strictly_inside(rect, Point::new(5.0, 5.0))); // interior
        // Edges and corners are not "inside", so edge snap dots still draw.
        assert!(!strictly_inside(rect, Point::new(0.0, 5.0))); // left edge
        assert!(!strictly_inside(rect, Point::new(10.0, 10.0))); // corner
        assert!(!strictly_inside(rect, Point::new(12.0, 5.0))); // outside
    }

    #[test]
    fn rect_distance_is_zero_inside_and_grows_outside() {
        let rect = Rectangle::new(Point::new(0.0, 0.0), Size::new(10.0, 10.0));
        assert_eq!(rect_distance(rect, Point::new(5.0, 5.0)), 0.0); // inside
        assert_eq!(rect_distance(rect, Point::new(13.0, 5.0)), 3.0); // 3 past the right edge
        // Diagonally off a corner: (3,4) past the top-left -> 5.
        assert_eq!(rect_distance(rect, Point::new(-3.0, -4.0)), 5.0);
    }
}

#[cfg(test)]
mod editing_thumb_tests {
    use super::{editing_thumb, scrollbar_thumb};
    use iced::{Point, Rectangle, Size};

    /// A card 200 wide and 300 tall on screen, holding 900 of content.
    fn card() -> Rectangle {
        Rectangle::new(Point::new(100.0, 50.0), Size::new(200.0, 300.0))
    }
    const NODE_H: f32 = 300.0;
    const CONTENT: f32 = 900.0;

    #[test]
    fn the_thumb_travels_the_track_as_the_editor_scrolls() {
        let top = editing_thumb(card(), NODE_H, CONTENT, 0.0, 1.0).expect("a thumb");
        let middle = editing_thumb(card(), NODE_H, CONTENT, 0.5, 1.0).expect("a thumb");
        let bottom = editing_thumb(card(), NODE_H, CONTENT, 1.0, 1.0).expect("a thumb");

        assert_eq!(top.y, card().y, "unscrolled, it sits at the top");
        assert!(middle.y > top.y && bottom.y > middle.y, "and moves down");
        assert!(
            (bottom.y + bottom.height - (card().y + card().height)).abs() < 0.5,
            "fully scrolled, it reaches the bottom"
        );
    }

    #[test]
    fn a_fraction_outside_the_range_is_clamped() {
        let bottom = editing_thumb(card(), NODE_H, CONTENT, 1.0, 1.0).expect("a thumb");
        let over = editing_thumb(card(), NODE_H, CONTENT, 4.0, 1.0).expect("a thumb");
        let under = editing_thumb(card(), NODE_H, CONTENT, -2.0, 1.0).expect("a thumb");
        assert_eq!(over.y, bottom.y);
        assert_eq!(under.y, card().y);
    }

    #[test]
    fn content_that_fits_gets_no_thumb() {
        assert!(editing_thumb(card(), NODE_H, 200.0, 0.5, 1.0).is_none());
    }

    #[test]
    fn editing_does_not_move_the_thumb_from_where_reading_left_it() {
        // Entering an edit must not make the thumb jump: the same position
        // through the card's own scroll offset and through the editor's
        // fraction has to land in the same place.
        let overflow = CONTENT - NODE_H;
        for fraction in [0.0, 0.25, 0.5, 1.0] {
            let reading = scrollbar_thumb(card(), NODE_H, CONTENT, fraction * overflow, 1.0)
                .expect("a thumb")
                .0;
            let editing = editing_thumb(card(), NODE_H, CONTENT, fraction, 1.0).expect("a thumb");
            assert_eq!(reading, editing, "at {fraction}");
        }
    }
}

#[cfg(test)]
mod scrollbar_tests {
    use super::scrollbar_thumb;
    use iced::{Point, Rectangle, Size};

    /// A node's screen rect: 200 wide, `h` tall, at (100, 50).
    fn rect(h: f32) -> Rectangle {
        Rectangle::new(Point::new(100.0, 50.0), Size::new(200.0, h))
    }

    #[test]
    fn no_thumb_when_content_fits() {
        assert!(scrollbar_thumb(rect(100.0), 100.0, 90.0, 0.0, 1.0).is_none());
        assert!(scrollbar_thumb(rect(100.0), 100.0, 100.0, 0.0, 1.0).is_none());
    }

    #[test]
    fn thumb_reports_overflow_and_sits_inside_the_node_on_its_right_edge() {
        let r = rect(100.0); // node 100 tall, content 300 -> overflow 200
        let (thumb, overflow) = scrollbar_thumb(r, 100.0, 300.0, 0.0, 1.0).unwrap();
        assert!((overflow - 200.0).abs() < 0.01);
        // Thumb height ∝ visible fraction (100/300 of a 100px node ≈ 33px).
        assert!((thumb.height - 100.0 / 3.0).abs() < 0.5, "thumb h {}", thumb.height);
        // At scroll 0 the thumb is flush with the top and within the node.
        assert!((thumb.y - r.y).abs() < 0.01);
        assert!(thumb.y + thumb.height <= r.y + r.height + 0.01);
        // On the right edge.
        assert!(thumb.x + thumb.width <= r.x + r.width + 0.01);
        assert!(thumb.x > r.x + r.width - 20.0);
    }

    #[test]
    fn thumb_travels_with_scroll_and_clamps_at_the_ends() {
        let r = rect(100.0);
        let (top, overflow) = scrollbar_thumb(r, 100.0, 300.0, 0.0, 1.0).unwrap();
        // Full scroll: the thumb bottom meets the node bottom.
        let (bottom, _) = scrollbar_thumb(r, 100.0, 300.0, overflow, 1.0).unwrap();
        assert!((bottom.y + bottom.height - (r.y + r.height)).abs() < 0.5);
        // Over-scroll clamps to the same bottom position.
        let (clamped, _) = scrollbar_thumb(r, 100.0, 300.0, overflow + 500.0, 1.0).unwrap();
        assert!((clamped.y - bottom.y).abs() < 0.01);
        // Mid-scroll sits strictly between the two ends.
        let (mid, _) = scrollbar_thumb(r, 100.0, 300.0, overflow / 2.0, 1.0).unwrap();
        assert!(mid.y > top.y && mid.y < bottom.y);
    }
}

#[cfg(test)]
mod cluster_drag_tests {
    use super::cluster_anchor;
    use crate::cluster::Cluster;
    use crate::model::CanvasNode;
    use iced::{Rectangle, Size, Point};

    fn node(x: f32, y: f32) -> CanvasNode {
        CanvasNode {
            id: format!("n{x}x{y}"),
            kind: "text".to_owned(),
            text: Some("x".to_owned()),
            file: None,
            x,
            y,
            width: 100.0,
            height: 60.0,
            color: None,
            extra: Default::default(),
        }
    }

    fn cluster(members: Vec<usize>) -> Cluster {
        Cluster {
            bounds: Rectangle::new(Point::ORIGIN, Size::new(10.0, 10.0)),
            text: String::new(),
            font_size: 10.0,
            image_id: None,
            members,
        }
    }

    #[test]
    fn the_anchor_is_the_topmost_then_leftmost_member() {
        // Deliberately not in member order: the reference has to come from the
        // geometry, or it would change when a recluster reorders the list.
        let nodes = vec![node(300.0, 40.0), node(120.0, 40.0), node(200.0, 10.0)];
        let clusters = vec![cluster(vec![0, 1, 2])];
        assert_eq!(cluster_anchor(&clusters, &nodes, 0), Some(2));

        // Level with each other: the leftmost breaks the tie.
        let nodes = vec![node(300.0, 40.0), node(120.0, 40.0)];
        let clusters = vec![cluster(vec![0, 1])];
        assert_eq!(cluster_anchor(&clusters, &nodes, 0), Some(1));
    }

    #[test]
    fn an_anchor_is_only_offered_when_there_is_one_to_drag_by() {
        let nodes = vec![node(0.0, 0.0)];
        // A cluster with no members, and a member index left over from a
        // document that has since shrunk — neither should be dragged by.
        assert_eq!(cluster_anchor(&[cluster(Vec::new())], &nodes, 0), None);
        assert_eq!(cluster_anchor(&[cluster(vec![7])], &nodes, 0), None);
        // And a cluster index that doesn't exist.
        assert_eq!(cluster_anchor(&[], &nodes, 0), None);
    }
}

#[cfg(test)]
mod cluster_snap_tests {
    use super::cluster_delta;
    use iced::{Point, Vector};

    /// Where a member ends up: the anchor's new home plus its own offset.
    fn member_after(member: Point, delta: Vector) -> Point {
        Point::new(member.x + delta.x, member.y + delta.y)
    }

    fn on_grid(p: Point, grid: f32) -> bool {
        (p.x / grid).fract().abs() < 1e-4 && (p.y / grid).fract().abs() < 1e-4
    }

    #[test]
    fn dragging_a_cluster_leaves_its_members_on_the_grid() {
        // The reason cluster snapping exists: a cluster that was aligned must
        // still be aligned after being moved, however far the cursor wandered.
        const GRID: f32 = 20.0;
        let anchor = Point::new(40.0, 20.0);
        let members = [anchor, Point::new(260.0, 20.0), Point::new(40.0, 180.0)];
        assert!(members.iter().all(|&m| on_grid(m, GRID)));

        for (dx, dy) in [(3.0, 1.0), (11.0, -7.0), (-58.0, 132.0), (0.5, 0.5)] {
            let want = Point::new(anchor.x + dx, anchor.y + dy);
            let delta = cluster_delta(want, anchor, GRID);
            for &m in &members {
                let moved = member_after(m, delta);
                assert!(on_grid(moved, GRID), "{m:?} + {delta:?} = {moved:?} is off the grid");
            }
        }
    }

    #[test]
    fn snapping_off_follows_the_cursor_exactly() {
        // Grid 0 means the feature is off; the cluster must track the cursor
        // rather than quantising to anything.
        let anchor = Point::new(37.0, 23.0);
        let want = Point::new(101.5, 64.25);
        let delta = cluster_delta(want, anchor, 0.0);
        assert_eq!(delta, Vector::new(64.5, 41.25));
    }

    #[test]
    fn a_drag_inside_one_cell_asks_for_no_move() {
        // Which is what lets the caller skip publishing, so nudging the cursor
        // within a cell doesn't mark the document dirty.
        let anchor = Point::new(40.0, 20.0);
        let delta = cluster_delta(Point::new(44.0, 26.0), anchor, 20.0);
        assert_eq!(delta, Vector::ZERO);
    }

}

#[cfg(test)]
mod chord_tests {
    use super::{Chord, chord};

    #[test]
    fn the_chord_table_is_shift_sensitive_where_it_should_be() {
        // Undo/redo are the same letter, told apart by Shift.
        assert_eq!(chord('z', false, false), Some(Chord::Undo));
        assert_eq!(chord('z', true, false), Some(Chord::Redo));

        // The palette on Cmd+P and Cmd+K, and on Cmd+Shift+P as before; only
        // Cmd+Shift+K is left alone.
        assert_eq!(chord('p', false, false), Some(Chord::Palette));
        assert_eq!(chord('k', false, false), Some(Chord::Palette));
        assert_eq!(chord('p', true, false), Some(Chord::Palette));
        assert_eq!(chord('k', true, false), None);

        // Cmd+O switches boards, and Cmd+Shift+O is left alone.
        assert_eq!(chord('o', false, false), Some(Chord::SwitchCanvas));
        assert_eq!(chord('o', true, false), None);

        // The rest don't care either way, and shouldn't start caring quietly.
        for shift in [false, true] {
            assert_eq!(chord('v', shift, false), Some(Chord::Paste));
            assert_eq!(chord('f', shift, false), Some(Chord::Find));
        }
    }

    #[test]
    fn copy_needs_a_selection_and_otherwise_lets_the_key_through() {
        assert_eq!(chord('c', false, true), Some(Chord::Copy));
        assert_eq!(
            chord('c', false, false),
            None,
            "with nothing selected the chord must fall through, not be swallowed",
        );
    }

    #[test]
    fn a_letter_the_board_does_not_claim_is_left_alone() {
        for letter in ['a', 'x', 's', 'q', 'w'] {
            assert_eq!(chord(letter, false, true), None, "Cmd+{letter} is not ours");
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

#[cfg(test)]
mod search_dim_tests {
    use super::search_dim;

    #[test]
    fn only_cards_without_a_match_dim_and_only_while_there_are_matches() {
        assert_eq!(search_dim(&[], 3), 1.0, "nothing found, nothing to point at");
        assert_eq!(search_dim(&[1, 4], 4), 1.0);
        assert!(search_dim(&[1, 4], 3) < 1.0);
    }
}
