//! The canvas: rendering of cards/images/edges and all mouse interaction.
//!
//! Everything is drawn in *screen space* (points transformed through
//! [`Camera`]) rather than via `frame.scale()`, because scaled canvas text
//! does not scale its wrapping width — screen-space drawing keeps card text
//! wrapping stable across zoom levels.

use std::collections::{HashMap, HashSet};
use std::f32::consts::PI;
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
use crate::document::IntoColor;
use crate::model::{CanvasEdge, CanvasNode, NodeKind};
use crate::theme::Theme as ColorTheme;

pub const BACKGROUND: Color = Color::from_rgb(0.12, 0.12, 0.13);
/// Default outline for uncolored nodes/clusters (not themed).
const CARD_BORDER: Color = Color::from_rgb(0.45, 0.45, 0.45);
/// Default node text color, and the fallback used by the app's menu chrome.
/// Node rendering uses the (possibly themed) color from `Board::theme`.
pub const TEXT_COLOR: Color = Color::from_rgb(0.86, 0.87, 0.87);
const EDGE_COLOR: Color = Color::from_rgba(0.35, 0.62, 1.0, 0.75);
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
    /// The node holding the current match, drawn with a stronger highlight.
    pub current_node: Option<usize>,
}

pub struct Board<'a> {
    pub nodes: &'a [CanvasNode],
    /// Node id → index, built once per frame so edge endpoint lookups are
    /// O(1) rather than a linear scan per edge (draw + hit-testing).
    pub node_index: HashMap<&'a str, usize>,
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
    pub cache: &'a canvas::Cache,
    pub editing: Option<usize>,
    pub overlay: bool,
    /// An in-board modal (the settings panel) is up: board keyboard shortcuts
    /// are suppressed so the modal owns input.
    pub modal: bool,
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
struct EdgeGeometry {
    start: Point,
    control1: Point,
    control2: Point,
    end: Point,
    alpha: f32,
}

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
        /// Previous cursor position, screen coordinates.
        last: Point,
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
const GROUP_LABEL_SIZE: f32 = 15.0;
const GROUP_LABEL_PAD: f32 = 10.0;
/// Height (world px) of a frame's grabbable header band — the caption plus its
/// padding above and below.
const GROUP_HEADER_HEIGHT: f32 = GROUP_LABEL_SIZE + GROUP_LABEL_PAD * 2.0;

/// Point size (world px) of the file-name header on a note/attachment card.
/// Smaller than the body so it reads as a label naming the file rather than as
/// the note's own first line.
const REFERENCE_TITLE_SIZE: f32 = FONT_SIZE - 3.0;

/// The scrollbar thumb rect (screen) within a node `rect`, plus the world
/// overflow, or `None` when the content fits within `node_height`. Sizes/scroll
/// are world px; `zoom` scales the thumb width. Pure, so it's unit-tested.
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
                if self.editing.is_some() {
                    Some(canvas::Action::publish(Message::StopEditing))
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
            // Cmd+Z undo / Cmd+Shift+Z redo, when not editing (the editor
            // gets these keys during a session). Matched on the *latin* key for
            // the physical position (the trick the editor and iced's copy/paste
            // use) so it fires regardless of the active keyboard layout.
            Event::Keyboard(keyboard::Event::KeyPressed {
                key,
                physical_key,
                modifiers,
                ..
            }) if self.editing.is_none()
                && modifiers.command()
                && key.to_latin(*physical_key) == Some('z') =>
            {
                let message = if modifiers.shift() {
                    Message::Redo
                } else {
                    Message::Undo
                };
                Some(canvas::Action::publish(message).and_capture())
            }
            // Cmd+C copies the selected nodes (and edges among them) to the
            // clipboard, when not editing (the editor gets Cmd+C in a session).
            Event::Keyboard(keyboard::Event::KeyPressed {
                key,
                physical_key,
                modifiers,
                ..
            }) if self.editing.is_none()
                && modifiers.command()
                && key.to_latin(*physical_key) == Some('c')
                && !self.selected.is_empty() =>
            {
                Some(canvas::Action::publish(Message::CopyNodes).and_capture())
            }
            // Cmd+V pastes clipboard nodes/text/image as new node(s) at the
            // cursor (or the view center if the cursor is off-canvas), when not
            // editing — the editor gets Cmd+V during a session.
            Event::Keyboard(keyboard::Event::KeyPressed {
                key,
                physical_key,
                modifiers,
                ..
            }) if self.editing.is_none()
                && modifiers.command()
                && key.to_latin(*physical_key) == Some('v') =>
            {
                let world = cursor
                    .position()
                    .map(|p| self.camera.to_world(p - Vector::new(bounds.x, bounds.y)))
                    .unwrap_or_else(|| {
                        self.camera
                            .to_world(Point::new(bounds.width / 2.0, bounds.height / 2.0))
                    });
                Some(canvas::Action::publish(Message::Paste { world }).and_capture())
            }
            // Cmd+F (macOS) / Ctrl+F (Win/Linux) opens the in-board find bar,
            // when not editing (the editor keeps the key for its own use).
            Event::Keyboard(keyboard::Event::KeyPressed {
                key,
                physical_key,
                modifiers,
                ..
            }) if self.editing.is_none()
                && modifiers.command()
                && key.to_latin(*physical_key) == Some('f') =>
            {
                Some(canvas::Action::publish(Message::OpenSearch).and_capture())
            }
            // Cmd/Ctrl+Shift+P: the command palette (Sublime/Zed convention).
            Event::Keyboard(keyboard::Event::KeyPressed {
                key,
                physical_key,
                modifiers,
                ..
            }) if self.editing.is_none()
                && modifiers.command()
                && modifiers.shift()
                && key.to_latin(*physical_key) == Some('p') =>
            {
                Some(canvas::Action::publish(Message::OpenCommandPalette).and_capture())
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
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        // The backdrop (dim or dark fill) and the node shadows live in
        // stack layers below this canvas; only edges and nodes are drawn
        // here.
        let build_start = self.show_fps.then(Instant::now);
        let geometry = self.cache.draw(renderer, bounds.size(), |frame| {
            // Crossfade node and cluster layers through the threshold; the
            // more-opaque one is drawn on top so end states are clean.
            let weight = self.cluster_weight();
            let draw_nodes = weight < 1.0;
            let draw_clusters = weight > 0.0;

            let node_layer = |frame: &mut Frame| {
                // Frames first: a group is a backdrop its members sit on.
                self.draw_groups(frame);
                self.draw_edges(frame);
                // In cluster mode there's no overlay, so draw every node here;
                // otherwise hold the selected node(s) back for the overlay.
                let pass = if self.show_clusters() {
                    NodePass::All
                } else {
                    NodePass::Unselected
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

        let mut geometries = vec![geometry];

        // Uncached overlay: edge highlights (on top of nodes), hover port
        // dots, and the connection-drag preview. Not in cluster mode.
        if !self.show_clusters() {
            let mut overlay = Frame::new(renderer, bounds.size());

            // Lift the selected node(s) above the cached scene so the node
            // being moved layers over any node it overlaps (its card included).
            self.draw_nodes(&mut overlay, NodePass::SelectedOnly);

            if let Some(edge) = self.selected_edge {
                // A thicker ring than hover makes selection unmistakable.
                self.draw_edge_outline(&mut overlay, edge, EDGE_HIGHLIGHT_RING, 1.5);
                self.draw_edge_line(&mut overlay, edge, EDGE_HIGHLIGHT);
            }
            if let Some(edge) = self.hovered_edge.filter(|e| self.selected_edge != Some(*e)) {
                self.draw_edge_outline(&mut overlay, edge, EDGE_HOVER_RING, 1.0);
                self.draw_edge_line(&mut overlay, edge, EDGE_HOVER);
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

            geometries.push(overlay.into_geometry());
        }

        // Wayfinding button when the viewport has drifted off all content. Its
        // own geometry (not the overlay above, which is suppressed in cluster
        // mode) so it shows regardless of zoom.
        if let Some(target) = self.wayfinding_target() {
            let mut wf = Frame::new(renderer, bounds.size());
            self.draw_wayfinding(&mut wf, target);
            geometries.push(wf.into_geometry());
        }

        if self.show_fps {
            geometries.push(self.draw_fps_meter(renderer, bounds));
        }

        geometries
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
enum NodePass {
    /// Every node (used in cluster mode, where there's no overlay to lift into).
    All,
    /// Everything except the selected node(s) — the cached base scene.
    Unselected,
    /// Only the selected node(s) — drawn into the overlay, on top.
    SelectedOnly,
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
                    if let Some(cluster) = self.cluster_at(self.camera.to_world(position)) {
                        state.kind = Kind::DraggingCluster {
                            cluster,
                            last: position,
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
                    Some(index) if is_double_click && self.nodes[index].is_text() => {
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
                if let Kind::Dragging { moved, .. } = state.kind {
                    state.kind = Kind::Idle;
                    // A pending link released without a drag opens it; otherwise
                    // this was a node drag (or a plain click) — just end it.
                    let open = state.link.take().filter(|_| !moved);
                    Some(match open {
                        Some(url) => canvas::Action::publish(Message::OpenUrl(url)).and_capture(),
                        None => canvas::Action::publish(Message::NodeDragEnd).and_capture(),
                    })
                } else if let (Some(start), Some(position)) = (background_press, position) {
                    if start.distance(position) >= 4.0 {
                        // A drag, not a click.
                        None
                    } else if !self.selected.is_empty() || self.selected_edge.is_some() {
                        // A click on empty space first clears the selection;
                        // it does not dismiss the board.
                        Some(canvas::Action::publish(Message::SelectNode(None)).and_capture())
                    } else if self.overlay && self.config.dismiss_on_background_click {
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
                    Kind::DraggingCluster { cluster, last } => {
                        let delta =
                            self.camera.to_world(position) - self.camera.to_world(*last);
                        *last = position;
                        Some(canvas::Action::publish(Message::MoveCluster {
                            index: *cluster,
                            delta,
                        })
                        .and_capture())
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
                            factor: 1.0 + y * self.config.wheel_zoom_step,
                            cursor: position,
                        })
                        .and_capture())
                    }
                    // Trackpad (precise pixels): pan, or zoom with Cmd/Ctrl
                    // held (substitute for pinch, which iced does not expose).
                    ScrollDelta::Pixels { x, y } => {
                        let message = if state.modifiers.command() || state.modifiers.control() {
                            Message::ZoomBy {
                                factor: (1.0 + y * self.config.pinch_zoom_step).max(0.5),
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
        crate::text_raster::with_markdown_layout(
            text,
            rect.width / zoom,
            rect.height / zoom,
            wrap,
            FONT_SIZE,
            *self.theme,
            self.config.center_node_text,
            |runs| {
                for run in runs {
                    let Some(url) = &run.link else { continue };
                    let origin = Point::new(rect.x + run.x * zoom, rect.y + run.y * zoom);
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
        for (index, node) in self.nodes.iter().enumerate().rev() {
            // No ports on a frame: its border runs behind other nodes, and
            // hovering anywhere near it would sprout dots over them.
            if node.is_group() {
                continue;
            }
            let rect = self.node_screen_rect(index, node);
            for side in SIDES {
                if side_anchor(rect, side).distance(position) < grab {
                    return Some((index, side));
                }
            }
        }
        None
    }

    /// Where a dragged connection would attach for the given cursor
    /// position: within a node, the side of the quadrant the cursor is in
    /// (the rect split into four triangles by its diagonals); within the
    /// surrounding [`SNAP_MARGIN`], the nearest side dot.
    fn connect_target(&self, from: usize, cursor: Point) -> Option<(usize, &'static str)> {
        for (index, node) in self.nodes.iter().enumerate().rev() {
            if index == from || node.is_group() {
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
        for (index, node) in self.nodes.iter().enumerate().rev() {
            if node.is_group() {
                continue; // frames have no ports to hover (see `port_at`)
            }
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

    /// Scrollable content height (world px) of a text node: the rendered text
    /// extent (`measure_markdown` insets to the render column) plus the node's
    /// vertical padding, split top/bottom. So a node at its fitted (autosized)
    /// height has no overflow, and a scrolled one keeps `NODE_TEXT_PAD_Y / 2`
    /// padding at each end rather than butting the text against the card edge.
    fn node_content_height(&self, text: &str, width: f32) -> f32 {
        let wrap = crate::text_raster::card_text_wrap(width);
        crate::text_raster::measure_markdown(text.trim(), wrap, FONT_SIZE, self.theme).1
            + NODE_TEXT_PAD_Y
    }

    /// The clamped scroll offset (world px) for a node, 0 when it fits.
    fn node_scroll_offset(&self, node: &CanvasNode) -> f32 {
        if !self.config.node_scrollbars || !node.is_text() {
            return 0.0;
        }
        let content = self.node_content_height(node.text.as_deref().unwrap_or(""), node.width);
        let raw = self.node_scroll.get(&node.id).copied().unwrap_or(0.0);
        raw.clamp(0.0, (content - node.height).max(0.0))
    }

    /// If `position` is on a scrollable node's scrollbar thumb, returns
    /// `(index, world overflow, world-scroll per screen-px)` for the drag. Only
    /// the node under the cursor is checked (the thumb sits inside its bounds).
    fn node_thumb_at(&self, position: Point) -> Option<(usize, f32, f32)> {
        if !self.config.node_scrollbars {
            return None;
        }
        let index = self.hit_test(self.camera.to_world(position))?;
        if self.editing == Some(index) {
            return None;
        }
        let node = self.nodes.get(index)?;
        if !node.is_text() {
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
            let radius = 12.0 * zoom;
            let card = Path::rounded_rectangle(rect.position(), rect.size(), radius.into());

            let fill = self.theme.card_fill;
            frame.fill(&card, Color { a: fill.a * alpha, ..fill });

            if let Some(id) = &cluster.image_id {
                if let Some(handle) = self.images.get(id) {
                    let mut image = canvas::Image::new(handle.clone());
                    image.border_radius = radius.into();
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
                } else if !self.config.rasterize_text {
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

    fn draw_nodes(&self, frame: &mut Frame, pass: NodePass) {
        let zoom = self.camera.zoom;

        let scene_alpha = self.node_layer_alpha();

        for (index, node) in self.nodes.iter().enumerate() {
            let selected = self.selected.contains(&index);
            let skip = match pass {
                NodePass::All => false,
                NodePass::Unselected => selected,
                NodePass::SelectedOnly => !selected,
            };
            // Groups are frames, drawn in their own pass behind everything.
            if skip || node.is_group() {
                continue;
            }
            let rect = self.node_screen_rect(index, node);
            let alpha = scene_alpha * self.spawn_factors(index).1;

            if node.node_kind() == NodeKind::Image {
                let radius = 8.0 * zoom;

                if let Some(handle) = self.images.get(&node.id) {
                    // The image shader masks corners with a rounded-box SDF
                    // applied to the image's *clip bounds* — which the frame
                    // normally sets to the whole canvas. Drawing inside
                    // with_clip(rect) makes clip bounds == image rect, so the
                    // radius actually rounds the visible corners.
                    let mut image = canvas::Image::new(handle.clone());
                    image.border_radius = radius.into();
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
                    (true, Some(color)) => (color, (3.5 * zoom).max(3.0)),
                    (true, None) => (self.theme.accent, (3.5 * zoom).max(3.0)),
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
                        .with_color(Color {
                            a: border_color.a * alpha,
                            ..border_color
                        })
                        .with_width(border_width),
                );
                continue;
            }

            let radius = 8.0 * zoom;

            let card = Path::rounded_rectangle(rect.position(), rect.size(), radius.into());
            let accent = node.palette_color().map(IntoColor::into_color);

            let fill = match accent {
                Some(color) => Color { a: 0.85, ..blend(self.theme.card_fill, color, 0.15) },
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
                let highlight = self.search.as_ref().map(|s| (s.query, s.current_node == Some(index)));

                // A scrollable node (scrollbars on, content overflows) draws the
                // live text scrolled + clipped, with a draggable thumb — the raster
                // snapshot only holds the visible top, so it can't scroll.
                let scrollbar = self.config.node_scrollbars.then(|| {
                    let content = self.node_content_height(text, node.width);
                    let scroll = self.node_scroll_offset(node);
                    scrollbar_thumb(rect, node.height, content, scroll, zoom).map(|(t, _)| (t, scroll))
                }).flatten();

                if let Some((thumb, scroll)) = scrollbar {
                    frame.with_clip(rect, |clipped| {
                        // Top-align (height 0 zeroes the layout's vertical centering)
                        // and start half the padding below the top; the same padding
                        // lands at the bottom when scrolled to the end.
                        let scrolled = Rectangle {
                            x: rect.x,
                            y: rect.y + (NODE_TEXT_PAD_Y / 2.0 - scroll) * zoom,
                            width: rect.width,
                            height: 0.0,
                        };
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
            let rect = self.node_screen_rect(index, node);
            let selected = self.selected.contains(&index);
            let accent = node
                .palette_color()
                .map(IntoColor::into_color)
                .unwrap_or(self.theme.text_muted);

            let radius = 12.0 * zoom;
            let frame_path = Path::rounded_rectangle(rect.position(), rect.size(), radius.into());
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

            let Some(label) = node.label().filter(|l| !l.trim().is_empty()) else {
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
    /// hairline rule, then the note's Markdown body (or why there isn't one).
    fn draw_note_card(&self, frame: &mut Frame, node: &CanvasNode, rect: Rectangle, alpha: f32) {
        let zoom = self.camera.zoom;
        let pad = NODE_TEXT_PAD_X * zoom;
        // The header is deliberately smaller and muted: it names the file,
        // while the body below is the content. Sized like the title, it read as
        // a stray first line of the note.
        let title_size = (REFERENCE_TITLE_SIZE * zoom).max(6.0);
        let title_y = rect.y + NODE_TEXT_PAD_Y * zoom / 2.0;

        frame.fill_text(canvas::Text {
            content: node.file_title().unwrap_or_else(|| "(no file)".to_owned()),
            position: Point::new(rect.x + pad, title_y),
            color: Color { a: 0.9 * alpha, ..self.theme.text_muted },
            size: title_size.into(),
            font: self.theme.ui_font(),
            ..canvas::Text::default()
        });

        // A rule under the header separates it from the body, the way
        // Obsidian's embedded-note header does.
        let rule_y = title_y + title_size * crate::text_raster::LINE_HEIGHT;
        frame.fill(
            &Path::rectangle(
                Point::new(rect.x + pad, rule_y),
                Size::new((rect.width - pad * 2.0).max(0.0), (1.0 * zoom).max(0.5)),
            ),
            Color { a: 0.6 * alpha, ..self.theme.border },
        );

        let body_y = rule_y + NODE_TEXT_PAD_Y * zoom / 2.0;
        match self.notes.get(&node.id).map(String::as_str) {
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
                // Nothing to show: say why rather than leaving a blank card.
                let reason = match node.node_kind() {
                    NodeKind::Attachment => "(not previewable)",
                    _ => "(note not found)",
                };
                frame.fill_text(canvas::Text {
                    content: reason.to_owned(),
                    position: Point::new(rect.x + pad, body_y),
                    color: Color { a: 0.7 * alpha, ..self.theme.text_muted },
                    size: ((FONT_SIZE - 2.0) * zoom).max(6.0).into(),
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

        let host_size = (FONT_SIZE * zoom).max(6.0);
        let url_size = ((FONT_SIZE - 4.0) * zoom).max(5.0);
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
            (true, Some(color)) => (color, (3.5 * zoom).max(3.0)),
            (true, None) => (self.theme.accent, (3.5 * zoom).max(3.0)),
            (false, Some(color)) => (color, 1.5 * zoom),
            (false, None) => (CARD_BORDER, (0.75 * zoom).max(0.5)),
        };
        frame.stroke(
            card,
            Stroke::default()
                .with_color(Color {
                    a: border_color.a * alpha,
                    ..border_color
                })
                .with_width(border_width),
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
        // `Some((query, strong))` highlights search matches; `strong` = the
        // node holding the current match (brighter).
        highlight: Option<(&str, bool)>,
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
                // Search highlight rects, painted behind the text.
                if let Some((query, strong)) = highlight.filter(|(q, _)| !q.is_empty()) {
                    let color = if strong {
                        Color { r: 1.0, g: 0.62, b: 0.11, a: 0.55 * alpha }
                    } else {
                        Color { r: 1.0, g: 0.85, b: 0.30, a: 0.32 * alpha }
                    };
                    for run in runs {
                        self.highlight_matches(frame, rect, run, zoom, query, color);
                    }
                }
                for run in runs {
                    let mut font = crate::UI_FONT;
                    // Match the raster path: body/bold use the theme's declared
                    // weights (a face is synthesized at each in `startup::run`).
                    font.weight = iced_weight(if run.bold {
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
                    frame.fill_text(canvas::Text {
                        content: run.text.clone(),
                        position: Point::new(rect.x + run.x * zoom, rect.y + run.y * zoom),
                        color: Color { a: run.color.a * alpha, ..run.color },
                        size: (run.size * zoom).into(),
                        line_height: LineHeight::Relative(crate::text_raster::LINE_HEIGHT),
                        font,
                        align_x: TextAlignment::Left,
                        align_y: Vertical::Top,
                        shaping: Shaping::Advanced,
                        ..canvas::Text::default()
                    });
                }
            },
        );
    }

    /// Paints a highlight rect behind each occurrence of `query` (case-
    /// insensitive) within a single laid-out run. Occurrences that cross a run
    /// (style/line) boundary aren't caught — acceptable for plain notes.
    fn highlight_matches(
        &self,
        frame: &mut Frame,
        rect: Rectangle,
        run: &crate::text_raster::TextRun,
        zoom: f32,
        query: &str,
        color: Color,
    ) {
        let fold = |c: char| c.to_lowercase().next().unwrap_or(c);
        let hay: Vec<char> = run.text.chars().map(fold).collect();
        let needle: Vec<char> = query.chars().map(fold).collect();
        if needle.is_empty() || hay.len() < needle.len() {
            return;
        }
        // Char index → byte offset, so we can slice the original run text.
        let mut byte: Vec<usize> = run.text.char_indices().map(|(b, _)| b).collect();
        byte.push(run.text.len());

        let family = if run.mono {
            self.theme.font_mono
        } else {
            self.theme.font_text
        };
        let line_h = run.size * crate::text_raster::LINE_HEIGHT * zoom;

        let mut cs = 0;
        while cs + needle.len() <= hay.len() {
            if hay[cs..cs + needle.len()] == needle[..] {
                let ce = cs + needle.len();
                // Widths (logical) of the run prefix and of the match itself.
                let x0 = crate::text_raster::measure(
                    &run.text[..byte[cs]],
                    f32::MAX,
                    run.size,
                    1.0,
                    family,
                )
                .0;
                let w = crate::text_raster::measure(
                    &run.text[byte[cs]..byte[ce]],
                    f32::MAX,
                    run.size,
                    1.0,
                    family,
                )
                .0;
                frame.fill_rectangle(
                    Point::new(rect.x + (run.x + x0) * zoom, rect.y + run.y * zoom),
                    Size::new(w * zoom, line_h),
                    color,
                );
                cs = ce;
            } else {
                cs += 1;
            }
        }
    }

    /// Screen-space bezier geometry of an edge, or `None` if an endpoint
    /// node is missing.
    fn edge_geometry(&self, edge: &CanvasEdge) -> Option<EdgeGeometry> {
        let from_index = *self.node_index.get(edge.from_node.as_str())?;
        let to_index = *self.node_index.get(edge.to_node.as_str())?;

        let alpha = self.node_layer_alpha()
            * self.spawn_factors(from_index).1.min(self.spawn_factors(to_index).1);
        let start = side_anchor(
            self.node_screen_rect(from_index, &self.nodes[from_index]),
            &edge.from_side,
        );
        let end = side_anchor(
            self.node_screen_rect(to_index, &self.nodes[to_index]),
            &edge.to_side,
        );
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
            let distance = bezier_distance(point, &geometry);
            if distance < threshold && best.is_none_or(|(_, best)| distance < best) {
                best = Some((index, distance));
            }
        }
        best.map(|(index, _)| index)
    }

    fn draw_edges(&self, frame: &mut Frame) {
        for (index, _) in self.edges.iter().enumerate() {
            self.draw_edge_line(frame, index, EDGE_COLOR);
        }
    }

    /// The opaque outline (ring) of a highlighted edge: drawn wider than, and
    /// behind, the link's own line so a border of `color` (a darker shade of
    /// the accent) shows around the vivid line. Opaque, so the color stays
    /// true and the overlapping line/arrowhead strokes don't compound alpha.
    /// `thickness` scales the ring width (hover uses 1.0; selection a heavier
    /// ring to stand apart).
    fn draw_edge_outline(&self, frame: &mut Frame, index: usize, color: Color, thickness: f32) {
        let Some(edge) = self.edges.get(index) else {
            return;
        };
        let Some(g) = self.edge_geometry(edge) else {
            return;
        };
        let zoom = self.camera.zoom;
        frame.stroke(
            &bezier_path(&g),
            Stroke {
                // Butt (not round) so the line stops at the tip rather than
                // poking a half-width past the arrowhead.
                line_cap: stroke::LineCap::Butt,
                line_join: stroke::LineJoin::Round,
                ..Stroke::default()
                    .with_color(Color { a: color.a * g.alpha, ..color })
                    .with_width(5.5 * zoom * thickness)
            },
        );
        self.draw_arrowhead(
            frame,
            g.end,
            &edge.to_side,
            g.alpha,
            8.0 * zoom,
            4.4 * zoom * thickness,
            color,
        );
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
        frame.stroke(
            &bezier_path(&g),
            Stroke {
                // Butt (not round) so the line stops at the tip rather than
                // poking a half-width past the arrowhead.
                line_cap: stroke::LineCap::Butt,
                line_join: stroke::LineJoin::Round,
                ..Stroke::default()
                    .with_color(Color { a: color.a * g.alpha, ..color })
                    .with_width(2.5 * zoom)
            },
        );
        self.draw_arrowhead(frame, g.end, &edge.to_side, g.alpha, 8.0 * zoom, 2.0 * zoom, color);
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
            if let Some(g) = self.ghost_link(ghost) {
                frame.stroke(
                    &bezier_path(&g),
                    Stroke {
                        line_dash: canvas::LineDash {
                            segments: &dash,
                            offset: 0,
                        },
                        ..Stroke::default().with_color(accent(0.4)).with_width(1.5 * zoom)
                    },
                );
            }
            let rect = self.camera.rect_to_screen(ghost.rect);
            let card = Path::rounded_rectangle(rect.position(), rect.size(), (8.0 * zoom).into());
            frame.fill(&card, accent(0.05));
            frame.stroke(
                &card,
                Stroke {
                    line_dash: canvas::LineDash {
                        segments: &dash,
                        offset: 0,
                    },
                    ..Stroke::default().with_color(accent(0.55)).with_width(1.5 * zoom)
                },
            );
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
                    &Path::rounded_rectangle(r.position(), r.size(), (8.0 * zoom).into()),
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
                &Path::rounded_rectangle(r.position(), r.size(), (8.0 * zoom).into()),
                Fill {
                    style: Style::Gradient(card_wash.into()),
                    ..Fill::default()
                },
            );
        }
        let line_glow = band(0.85);
        for g in &lines {
            frame.stroke(
                &bezier_path(g),
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
                (
                    side_anchor(rect, snap.side),
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

        let path = Path::new(|builder| {
            builder.move_to(start);
            builder.bezier_curve_to(control1, control2, end);
        });
        frame.stroke(
            &path,
            Stroke {
                line_cap: stroke::LineCap::Round,
                line_join: stroke::LineJoin::Round,
                ..Stroke::default()
                    .with_color(EDGE_COLOR)
                    .with_width(2.5 * self.camera.zoom)
            },
        );
        if let (Some(to_side), true) = (end_side, strength > 0.0) {
            self.draw_arrowhead(
                frame,
                end,
                to_side,
                strength,
                8.0 * self.camera.zoom,
                2.0 * self.camera.zoom,
                EDGE_COLOR,
            );
        }

        self.draw_port(frame, from, side);
    }

    /// Port of the Swift `ArrowHead`: two wings at the tip, oriented by the
    /// side the edge arrives at. `size` and `width` are in screen px.
    fn draw_arrowhead(
        &self,
        frame: &mut Frame,
        tip: Point,
        to_side: &str,
        alpha: f32,
        size: f32,
        width: f32,
        color: Color,
    ) {
        let direction = match to_side.to_lowercase().as_str() {
            "left" => 0.0,
            "right" => PI,
            "top" => PI * 0.5,
            "bottom" => PI * 1.5,
            _ => 0.0,
        };
        let wing = |angle: f32| {
            Point::new(tip.x + size * angle.cos(), tip.y + size * angle.sin())
        };
        let path = Path::new(|builder| {
            builder.move_to(wing(direction + PI * 0.75));
            builder.line_to(tip);
            builder.line_to(wing(direction - PI * 0.75));
        });
        frame.stroke(
            &path,
            Stroke {
                line_cap: stroke::LineCap::Round,
                line_join: stroke::LineJoin::Round,
                ..Stroke::default()
                    .with_color(Color { a: color.a * alpha, ..color })
                    .with_width(width)
            },
        );
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
            let end = side_anchor(to_rect, to_side);

            let reach = start.distance(end) * 0.4;
            let control1 = start + outward(from_side) * reach;
            let control2 = end + outward(to_side) * reach;

            let path = Path::new(|builder| {
                builder.move_to(start);
                builder.bezier_curve_to(control1, control2, end);
            });
            frame.stroke(
                &path,
                Stroke {
                    line_cap: stroke::LineCap::Round,
                    line_join: stroke::LineJoin::Round,
                    ..Stroke::default()
                        .with_color(Color { a: EDGE_COLOR.a * alpha, ..EDGE_COLOR })
                        .with_width(stroke_width)
                },
            );
            self.draw_arrowhead(frame, end, to_side, alpha, head, stroke_width * 0.8, EDGE_COLOR);
        }
    }
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

fn group_header(rect: Rectangle, scale: f32) -> Rectangle {
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

fn bezier_path(g: &EdgeGeometry) -> Path {
    Path::new(|builder| {
        builder.move_to(g.start);
        builder.bezier_curve_to(g.control1, g.control2, g.end);
    })
}

fn cubic_bezier(g: &EdgeGeometry, t: f32) -> Point {
    let u = 1.0 - t;
    let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
    Point::new(
        a * g.start.x + b * g.control1.x + c * g.control2.x + d * g.end.x,
        a * g.start.y + b * g.control1.y + c * g.control2.y + d * g.end.y,
    )
}

/// Minimum distance from a point to an edge's bezier, via polyline sampling.
fn bezier_distance(point: Point, g: &EdgeGeometry) -> f32 {
    const SAMPLES: usize = 24;
    let mut previous = g.start;
    let mut min = f32::MAX;
    for i in 1..=SAMPLES {
        let current = cubic_bezier(g, i as f32 / SAMPLES as f32);
        min = min.min(point_segment_distance(point, previous, current));
        previous = current;
    }
    min
}

fn point_segment_distance(p: Point, a: Point, b: Point) -> f32 {
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

/// Maps a numeric font weight (expected snapped to the nearest 100) to iced's
/// `Weight` enum, the only weight the live text path can request.
fn iced_weight(weight: u16) -> iced::font::Weight {
    use iced::font::Weight;
    match weight {
        0..=150 => Weight::Thin,
        151..=250 => Weight::ExtraLight,
        251..=350 => Weight::Light,
        351..=450 => Weight::Normal,
        451..=550 => Weight::Medium,
        551..=650 => Weight::Semibold,
        651..=750 => Weight::Bold,
        751..=850 => Weight::ExtraBold,
        _ => Weight::Black,
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
