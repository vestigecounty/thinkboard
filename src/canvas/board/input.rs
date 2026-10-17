//! The board's input: the gesture in progress ([`Kind`]), what the pointer is
//! over, the Cmd/Ctrl chords, and what a drag shows while it lasts — the snap
//! dots. `handle_mouse` is where a press, a move, a release and a wheel turn
//! into messages.

use super::*;

/// What a Cmd/Ctrl chord does. Separated from the event so the table can be
/// read — and tested — without a live board: which chords take Shift, and which
/// need something selected, is the part that is easy to get subtly wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Chord {
    Undo,
    Redo,
    Copy,
    Paste,
    Find,
    Palette,
    SwitchCanvas,
    Settings,
    ZoomIn,
    ZoomOut,
    Fit,
}

/// How far ⌘= and ⌘− zoom, per press.
pub(super) const ZOOM_KEY_STEP: f32 = 1.25;

/// The latin character a key stands for in a chord, whatever the layout: what
/// it types, where that's latin — so on AZERTY ⌘, is the comma key wherever it
/// sits — and otherwise what its place types on a US keyboard.
///
/// That is iced's `to_latin`, whose second half stops at letters and digits.
/// The punctuation it leaves out is here: on a Russian layout the comma's key
/// types "б", and ⌘, went nowhere.
pub(super) fn chord_key(key: &keyboard::Key, physical: keyboard::key::Physical) -> Option<char> {
    use keyboard::key::{Code, Physical};
    key.to_latin(physical).or_else(|| {
        let Physical::Code(code) = physical else {
            return None;
        };
        Some(match code {
            Code::Comma => ',',
            Code::Period => '.',
            Code::Slash => '/',
            Code::Semicolon => ';',
            Code::Quote => '\'',
            Code::BracketLeft => '[',
            Code::BracketRight => ']',
            Code::Backslash => '\\',
            Code::Minus => '-',
            Code::Equal => '=',
            Code::Backquote => '`',
            _ => return None,
        })
    })
}

/// The chord for a latin `letter`, or `None` if the board doesn't claim it —
/// in which case the key falls through untouched.
pub(super) fn chord(letter: char, shift: bool, has_selection: bool) -> Option<Chord> {
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
        // Code. Board only, as every chord here is — while editing, ⌘K is the
        // editor's to make a link with, as it is in Obsidian.
        ('p', _) | ('k', false) => Chord::Palette,
        // Switch boards: the vault browser where a vault is configured, the
        // ordinary open dialog where one isn't.
        ('o', false) => Chord::SwitchCanvas,
        // Settings where every Mac app keeps them, and where VS Code put them
        // for everyone else.
        (',', false) => Chord::Settings,
        // Zoom as a browser does: = (and the + above it), −, and 0 for all of it.
        ('=' | '+', _) => Chord::ZoomIn,
        ('-' | '_', _) => Chord::ZoomOut,
        ('0', _) => Chord::Fit,
        _ => return None,
    })
}

pub(super) const DOUBLE_CLICK_TIME: Duration = Duration::from_millis(400);

pub(super) const DOUBLE_CLICK_DISTANCE: f32 = 6.0;

/// Rendered radius of each snap dot (screen px).
pub(super) const SNAP_DOT_RADIUS: f32 = 1.5;

/// Peak opacity of a snap dot (nearest a dragged node); scaled by the falloff.
pub(super) const SNAP_DOT_ALPHA: f32 = 0.55;

/// How many rings of snap dots surround a node (halo width, in grid steps) —
/// the "around" region. Grid-relative so it's a consistent number of dots at
/// any snap distance; the outermost ring is faintest (falloff).
pub(super) const SNAP_NODE_RINGS: f32 = 3.5;

/// Below this on-screen grid spacing (px) the grid is too dense to be useful,
/// so the dots are hidden (zoomed too far out).
pub(super) const SNAP_DOT_MIN_SPACING: f32 = 6.0;

#[derive(Default)]
pub struct Interaction {
    pub(super) kind: Kind,
    pub(super) modifiers: keyboard::Modifiers,
    pub(super) last_click: Option<(Instant, Point)>,
    /// Left press that landed on empty space; a release nearby dismisses
    /// the overlay.
    pub(super) background_press: Option<Point>,
    /// Side of the node the cursor is hovering near; shows a port dot.
    pub(super) hover: Option<(usize, &'static str)>,
    /// Snap animation of the connection preview toward a target node.
    pub(super) snap: Option<SnapAnim>,
    /// Active rubber-band (marquee) selection dragged from empty space.
    pub(super) marquee: Option<Marquee>,
    /// Link URL under the current press on an already-selected node; opened on
    /// release if the press didn't turn into a drag. Held here (not on `Kind`,
    /// which is `Copy`) to keep the owned `String` off the interaction enum.
    pub(super) link: Option<String>,
    /// `tb_action` of a button node under the current press (e.g. the scratch
    /// board's "Create a board"); fired on release. Like `link`, an owned
    /// `String` kept off the `Copy` `Kind`.
    pub(super) action: Option<String>,
    pub(super) reported_viewport: Option<Size>,
    /// The off-screen group whose marker the cursor is over, by index — what
    /// shows its name; see [`Board::offscreen`].
    pub(super) marker: Option<usize>,
}

/// A rubber-band selection in progress.
pub(super) struct Marquee {
    /// Press and current cursor, in screen coordinates.
    pub(super) start: Point,
    pub(super) current: Point,
    /// Selection to union the marquee with (non-empty for an additive
    /// Cmd-drag), so it extends rather than replaces.
    pub(super) base: Vec<usize>,
}

/// Seconds for the preview to snap onto / release from a target node.
pub(super) const SNAP_DURATION: f32 = 0.25;

/// Leeway around a node (screen px) within which a dragged connection
/// snaps to it.
pub(super) const SNAP_MARGIN: f32 = 24.0;

#[derive(Clone, Copy)]
pub(super) struct SnapAnim {
    pub(super) target: usize,
    pub(super) side: &'static str,
    /// Strength when the direction last changed; strength grows/shrinks
    /// linearly from here, clamped to [0, 1].
    pub(super) strength_at_change: f32,
    pub(super) changed: Instant,
    /// +1 while snapping onto the target, -1 while releasing.
    pub(super) direction: f32,
}

impl SnapAnim {
    pub(super) fn strength(&self, now: Instant) -> f32 {
        let elapsed = now.duration_since(self.changed).as_secs_f32();
        (self.strength_at_change + self.direction * elapsed / SNAP_DURATION).clamp(0.0, 1.0)
    }

    pub(super) fn settled(&self, now: Instant) -> bool {
        let strength = self.strength(now);
        (self.direction > 0.0 && strength >= 1.0) || (self.direction < 0.0 && strength <= 0.0)
    }
}

#[derive(Default, Clone, Copy)]
pub(super) enum Kind {
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
pub(super) enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl Corner {
    pub(super) fn interaction(self) -> mouse::Interaction {
        match self {
            // "/" diagonal vs "\" diagonal.
            Corner::TopRight | Corner::BottomLeft => mouse::Interaction::ResizingDiagonallyUp,
            Corner::TopLeft | Corner::BottomRight => mouse::Interaction::ResizingDiagonallyDown,
        }
    }
}

/// Grab distance (screen px) around a node corner for resizing.
pub(super) const CORNER_GRAB: f32 = 18.0;

pub(super) const SIDES: [&str; 4] = ["left", "right", "top", "bottom"];

/// How close (screen px) the cursor must be to a node for side ports to
/// appear.
pub(super) const HOVER_MARGIN: f32 = 14.0;

pub(super) fn rect_from_points(a: Point, b: Point) -> Rectangle {
    Rectangle::new(
        Point::new(a.x.min(b.x), a.y.min(b.y)),
        Size::new((a.x - b.x).abs(), (a.y - b.y).abs()),
    )
}

/// Distance from `p` to the nearest point of `rect` (0 when inside).
pub(super) fn rect_distance(rect: Rectangle, p: Point) -> f32 {
    let dx = (rect.x - p.x).max(p.x - (rect.x + rect.width)).max(0.0);
    let dy = (rect.y - p.y).max(p.y - (rect.y + rect.height)).max(0.0);
    (dx * dx + dy * dy).sqrt()
}

/// `rect` grown by `m` on every side.
pub(super) fn expand_rect(rect: Rectangle, m: f32) -> Rectangle {
    Rectangle::new(
        Point::new(rect.x - m, rect.y - m),
        Size::new(rect.width + 2.0 * m, rect.height + 2.0 * m),
    )
}

/// Whether `p` is strictly inside `rect` (boundary excluded), so snap dots that
/// land exactly on a node's edge/corner still show but its interior stays clear.
pub(super) fn strictly_inside(rect: Rectangle, p: Point) -> bool {
    p.x > rect.x && p.x < rect.x + rect.width && p.y > rect.y && p.y < rect.y + rect.height
}

impl Board<'_> {
    pub(super) fn handle_mouse(
        &self,
        state: &mut Interaction,
        event: mouse::Event,
        position: Option<Point>,
    ) -> Option<canvas::Action<Message>> {
        match event {
            mouse::Event::ButtonPressed(Button::Left) => self.press(state, position),
            mouse::Event::ButtonReleased(Button::Left) => self.release(state, position),
            mouse::Event::ButtonPressed(Button::Right) => {
                let position = position?;
                state.kind = Kind::Panning {
                    last: position,
                    start: position,
                };
                Some(canvas::Action::publish(Message::CancelInertia).and_capture())
            }
            mouse::Event::ButtonReleased(Button::Right) => self.right_release(state, position),
            mouse::Event::CursorMoved { .. } => self.cursor_moved(state, position),
            mouse::Event::WheelScrolled { delta } => self.wheel(state, delta, position),
            _ => None,
        }
    }

    /// A left press: the start of whatever the gesture turns out to be — a drag,
    /// a connection, a resize, a marquee, a pan in cluster mode — or a click on a
    /// marker, a link, a button card.
    fn press(
        &self,
        state: &mut Interaction,
        position: Option<Point>,
    ) -> Option<canvas::Action<Message>> {
        let position = position?;

        // A marker (only shown when no card is on screen): fly the
        // camera to its group.
        if let Some((_, target)) = self.marker_at(position, state.marker) {
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
                // the cursor, not at the start of the card.
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
                    && self.config.double_click_adds_card(self.overlay)
                {
                    // The first click of the pair started a marquee and
                    // released it unmoved; this one makes the card.
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

    /// A left release: the end of the gesture the press began, and the click a
    /// press that never became a drag turns out to have been.
    fn release(
        &self,
        state: &mut Interaction,
        position: Option<Point>,
    ) -> Option<canvas::Action<Message>> {
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

    /// A right release: the end of a pan, or, if it never moved, the menu for what
    /// was under it.
    fn right_release(
        &self,
        state: &mut Interaction,
        position: Option<Point>,
    ) -> Option<canvas::Action<Message>> {
        if let Kind::Panning { start, .. } = state.kind {
            state.kind = Kind::Idle;
            // A right-click that didn't drag opens the context menu;
            // a drag was a pan and coasts on release.
            if let Some(position) = position
                && start.distance(position) < 5.0
            {
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
            Some(canvas::Action::publish(Message::EndPan).and_capture())
        } else {
            None
        }
    }

    /// A move: the gesture in progress follows it, and with none, whatever the
    /// pointer is over — a port, a link, a marker — is what lights up.
    fn cursor_moved(
        &self,
        state: &mut Interaction,
        position: Option<Point>,
    ) -> Option<canvas::Action<Message>> {
        // Off the board, no marker is under it any more.
        let Some(position) = position else {
            return state.marker.take().map(|_| canvas::Action::request_redraw());
        };
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
                // A marker's name shows while it's hovered: board
                // state, drawn only by this board.
                let marker = self.marker_at(position, state.marker).map(|(group, _)| group);
                if marker != state.marker {
                    state.marker = marker;
                    return Some(canvas::Action::request_redraw());
                }
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

    /// A wheel or a trackpad: a mouse wheel zooms toward the cursor; a trackpad
    /// pans, or zooms with Cmd/Ctrl held.
    fn wheel(
        &self,
        state: &mut Interaction,
        delta: ScrollDelta,
        position: Option<Point>,
    ) -> Option<canvas::Action<Message>> {
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

    /// Topmost node containing the given world point.
    ///
    /// A group is a frame drawn *around* other nodes, so only its header strip
    /// answers: a click anywhere in its interior has to reach the node it
    /// encloses (or the empty canvas), not pick up the frame.
    pub(super) fn hit_test(&self, world: Point) -> Option<usize> {
        self.nodes.iter().rposition(|node| {
            if node.is_group() {
                group_header(self.node_rect(node), 1.0).contains(world)
            } else {
                self.node_rect(node).contains(world)
            }
        })
    }

    pub(super) fn snap_grid(&self) -> f32 {
        if self.config.snap_to_grid {
            self.config.snap_grid_size
        } else {
            0.0
        }
    }

    /// Snaps a world point to the grid when snap-to-grid is on; otherwise
    /// unchanged. Applied to node moves and resizes.
    pub(super) fn snap_world(&self, p: Point) -> Point {
        let (x, y) = crate::snap::point(p.x, p.y, self.snap_grid());
        Point::new(x, y)
    }

    /// The `tb_action` of a button node (currently only in the embedded scratch
    /// board), if node `index` carries one. Such nodes behave as buttons rather
    /// than editable cards.
    pub(super) fn node_action(&self, index: usize) -> Option<String> {
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
    pub(super) fn link_at(&self, index: usize, point: Point) -> Option<String> {
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
            self.config.center_card_text,
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
    pub(super) fn nodes_in_rect(&self, rect: Rectangle) -> Vec<usize> {
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

    /// The node corner under the cursor, if any.
    pub(super) fn corner_at(&self, position: Point) -> Option<(usize, Corner)> {
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

    /// Runs `f` over every node, cards before frames, returning the first hit.
    /// Frames sit behind everything they enclose, so they must lose every
    /// contest for the cursor to whatever is drawn on top of them.
    pub(super) fn frame_last<R>(&self, f: impl Fn(usize, &CanvasNode) -> Option<R>) -> Option<R> {
        let cards = self.nodes.iter().enumerate().rev().filter(|(_, n)| !n.is_group());
        let frames = self.nodes.iter().enumerate().rev().filter(|(_, n)| n.is_group());
        cards.chain(frames).find_map(|(index, node)| f(index, node))
    }

    /// Which node side the cursor hovers near: inside the node (or just
    /// outside it), attributed to the nearest edge.
    pub(super) fn hover_side(&self, position: Point) -> Option<(usize, &'static str)> {
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

    /// If `position` is on a scrollable node's scrollbar thumb, returns
    /// `(index, world overflow, world-scroll per screen-px)` for the drag. Only
    /// the node under the cursor is checked (the thumb sits inside its bounds).
    pub(super) fn node_thumb_at(&self, position: Point) -> Option<(usize, f32, f32)> {
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

    /// A port dot at the center of the given node side. Drawn through the
    /// image pipeline (not as a mesh): images render above meshes within a
    /// canvas layer, so a mesh dot would disappear under image nodes.
    /// Draws the snap grid as dots around the node(s) being dragged (never over
    /// a node's body), each fading with distance from a dragged node so its
    /// whole perimeter lights up regardless of size. `dragging` is the grabbed
    /// node; a multi-selection lights every selected node (they move together).
    /// Called only while dragging with snap-to-grid on, out of cluster mode.
    pub(super) fn draw_snap_dots(&self, frame: &mut Frame, dragging: usize) {
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
mod chord_tests {
    use super::{Chord, chord, chord_key};

    #[test]
    fn a_chords_key_is_read_the_same_on_any_layout() {
        use iced::keyboard::Key;
        use iced::keyboard::key::{Code, Physical};
        let read = |typed: &str, place| chord_key(&Key::Character(typed.into()), Physical::Code(place));
        // A latin layout: what the key types, wherever it is — AZERTY's comma
        // sits where a US keyboard has M.
        assert_eq!(read(",", Code::Comma), Some(','));
        assert_eq!(read(",", Code::KeyM), Some(','));
        // A Cyrillic one: what its place types on a US keyboard — the letters
        // by iced, the punctuation by us.
        assert_eq!(read("ф", Code::KeyA), Some('a'));
        assert_eq!(read("б", Code::Comma), Some(','));
        assert_eq!(read("ю", Code::Period), Some('.'));
    }

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

        // Cmd+, opens settings; Cmd+Shift+, (Cmd+<) is someone else's.
        assert_eq!(chord(',', false, false), Some(Chord::Settings));
        assert_eq!(chord(',', true, false), None);

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
