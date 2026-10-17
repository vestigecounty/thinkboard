//! The canvas: rendering of cards/images/edges and all mouse interaction.
//!
//! Everything is drawn in *screen space* (points transformed through
//! [`Camera`]) rather than via `frame.scale()`, because scaled canvas text
//! does not scale its wrapping width — screen-space drawing keeps card text
//! wrapping stable across zoom levels.
//!
//! The board is one widget, split by what it draws and handles: `input` (the
//! gesture in progress, hit tests and chords), `links`, `cards`, `search`,
//! `clusters`, `offscreen` (the markers and the empty-board hint), `ai` and
//! `fps`. This module holds the widget itself — [`Board`], its scene cache and
//! the two entry points iced calls, `update` and `draw_stacked` — and the
//! helpers they share.

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
use super::markers;
use crate::document::IntoColor;
use crate::model::{CanvasEdge, CanvasNode, EdgeEnd, NodeKind};
use crate::theme::Theme as ColorTheme;

mod ai;
mod cards;
mod clusters;
mod fps;
mod input;
mod links;
mod offscreen;
mod search;

use cards::*;
use clusters::*;
use fps::*;
use input::*;
use links::*;
use search::*;
// What the rest of the app names through `board::`, where it was before
// the board was split into these modules.
pub use ai::{AiFlow, Ghost};
pub use cards::{content_height, scroll_offset, scrolls, text_frame};
pub(crate) use cards::{GROUP_LABEL_PAD, GROUP_LABEL_SIZE, group_header};
pub use clusters::cluster_opacity;
pub use links::opposite_side;
pub use search::{SearchView, hit_ranges, search_dim};

/// Default outline for uncolored nodes/clusters (not themed).
const CARD_BORDER: Color = Color::from_rgb(0.45, 0.45, 0.45);
/// Default node text color, and the fallback used by the app's menu chrome.
/// Node rendering uses the (possibly themed) color from `Board::theme`.
pub const TEXT_COLOR: Color = Color::from_rgb(0.86, 0.87, 0.87);
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

const FONT_SIZE: f32 = 17.0;



/// A rounded-rect path at a screen rect. The radius comes from the theme —
/// `Board::card_radius` for a card, `frame_radius` for the shapes that contain
/// cards — so shape is themeable alongside colour.
fn rounded_path(rect: Rectangle, radius: f32) -> Path {
    Path::rounded_rectangle(rect.position(), rect.size(), radius.into())
}

/// `color` at `alpha` of its own opacity — the fade every card layer applies
/// as it crossfades in, spelled once.
fn fade(color: Color, alpha: f32) -> Color {
    Color { a: color.a * alpha, ..color }
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
    /// Top safe-area inset (px) — the macOS notch / menu-bar height, so a
    /// marker on the top edge isn't hidden. 0 off macOS or where there's none.
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
    /// their content with `card_scrollbars` on. Clamped to the overflow on use.
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
    /// The in-board find, while its bar is open: the query to highlight in card
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
            // Tab adds a linked card.
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
                    (Named::Tab, _) if !self.selected.is_empty() && !modifiers.shift() => Message::AddLinkedNode,
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
                let letter = chord_key(key, *physical_key)?;
                let message = match chord(letter, modifiers.shift(), !self.selected.is_empty())? {
                    Chord::Undo => Message::Undo,
                    Chord::Redo => Message::Redo,
                    Chord::Copy => Message::CopyNodes,
                    Chord::Find => Message::OpenSearch,
                    Chord::Palette => Message::OpenCommandPalette,
                    Chord::SwitchCanvas => Message::SwitchCanvas,
                    Chord::Settings => Message::OpenSettingsOverlay,
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
                if self.marker_at(position, state.marker).is_some() {
                    mouse::Interaction::Pointer
                } else if self.show_clusters() {
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

impl Stacked<Message> for Board<'_> {
    fn draw_stacked(
        &self,
        state: &Interaction,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        cursor: mouse::Cursor,
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
            // Shimmer sweeping across the cards feeding each pending AI request.
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

        // Where the cards are, when none of them is on screen. Its own
        // geometry (not the overlay above, which is suppressed in cluster
        // mode) so it shows at any zoom.
        if let Some(offscreen) = self.offscreen() {
            // Hovered is what's under the cursor *now*: the camera can fly
            // off with the cursor holding still, and the hover it left
            // behind belongs to a dot somewhere else.
            let hovered = cursor
                .position_in(bounds)
                .and_then(|position| self.marker_at(position, state.marker))
                .map(|(group, _)| group);
            let mut frame = Frame::new(renderer, bounds.size());
            let mut chip = Frame::new(renderer, bounds.size());
            self.draw_markers(&mut frame, &mut chip, &offscreen, hovered);
            geometries.push((frame.into_geometry(), false));
            // A layer of its own: canvas text is drawn above every fill in its
            // layer, so a chip sharing one with the nearest group's name
            // couldn't cover it, and the two names were drawn through each
            // other. Paid only while a marker is hovered — on a board with
            // nothing on screen.
            if hovered.is_some() {
                geometries.push((chip.into_geometry(), true));
            }
        }

        // Its counterpart for a board with nothing to find: how to start one.
        // Gone with the first card, and while a menu is up — the menu is the
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
    /// Content opacity during the overlay entrance/exit.
    fn scene_alpha(&self) -> f32 {
        if self.overlay {
            crate::camera::ease_out_cubic(self.reveal).clamp(0.0, 1.0)
        } else {
            1.0
        }
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
