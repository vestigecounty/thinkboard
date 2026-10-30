//! Back and forward through the boards visited this session — ⌘[ and ⌘] —
//! and the strip that shows them while ⌘ is held.
//!
//! **A board opens where it was left.** Each one left this session keeps its
//! camera, so going back to it — by these keys, the palette or the browser —
//! lands on the part you were working on rather than zoomed out to all of it.
//! Only a board not yet seen this session is fitted to the view.
//!
//! The trail is a browser's history with each board in it once: opening a
//! board drops whatever was ahead of the one on screen and puts it last, so
//! back is always where you came from, while stepping along it changes
//! nothing — and past either end it wraps round to the other. It's for the
//! session, as the palette's recent commands are; the recent boards the
//! palette lists are the ones kept in the config.
//!
//! **And opens as it was left.** The last few boards left keep what was
//! computed from them ([`Warm`]): their clusters and text bitmaps, which a
//! board otherwise computes again on opening — the cluster titles changing
//! and the cards drawn live meanwhile — and their on-screen images, held on
//! the GPU, which otherwise went blank while they were decoded and uploaded
//! again. Kept only while the file is as it was left.
//!
//! **⌘ held brings up the strip; a quick press doesn't.** ⌘[ goes back a
//! board and ⌘] forward, as a browser goes through pages. With ⌘ held
//! [`board::SHOW_SWITCHER_AFTER`] — by itself, or with [ and ] — a strip
//! shows the trail as outlines, older to the left, the way `[` points, and
//! each press moves its highlight. Letting ⌘ go lands there; Esc, or a click
//! off the strip, stays put. A tap, or a few presses in quick succession,
//! switch without it ever showing.
//!
//! ⌘ is held for much else — a chord, a ⌘-click, a ⌘-scroll — and any of
//! those, while it's down, keeps the strip from showing, or puts it away
//! (`board::uses_command`). A chord made after hesitating on ⌘ still does
//! what it does: the strip takes the pointer, but not the keys.
//!
//! The holding and the letting go are seen by the board widget, which times
//! the one (`ShowSwitcher`) and publishes `CommandReleased` on every release
//! — not by a listener of the strip's own. A tap's press and release can
//! arrive in one batch of events, before the subscription its press asked
//! for exists, and the release would be missed with the strip left up.
//! Within one widget they're handled in order.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use iced::widget::{canvas, column, container, image, mouse_area, row, text};
use iced::{Alignment, Border, Color, Element, Fill, Font, Point, Rectangle, Size, Task, Vector};

use super::arrange::node_world_rect;
use super::raster::Clusters;
use super::{App, Message, chrome};
use crate::canvas::{board, text_raster};
use crate::document::IntoColor;
use crate::model::{CanvasEdge, CanvasNode};

/// One board in the strip, its name below it.
const THUMB: Size = Size::new(150.0, 100.0);
const GAP: f32 = 20.0;
/// The strip's inset, all round.
const PAD: f32 = 20.0;
/// Kept clear at the window's sides.
const MARGIN: f32 = 16.0;
/// The most boards shown at once; the rest scroll in as the highlight moves.
const MAX_SHOWN: usize = 5;
/// How many boards left are kept [`Warm`] — enough to go back and forth
/// among the last three without computing any of them again.
const WARM_BOARDS: usize = 2;
/// The most a board left keeps of text bitmaps. A big board fitted to the
/// view bakes every card at its minimum resolution, and 800 cards came to
/// 388 MB; a board over this comes back without its bitmaps, and makes them
/// again as it did before.
const WARM_BITMAPS: usize = 128 * 1024 * 1024;

/// A board as the strip draws it, in world units: its cards and frames as
/// rectangles in their colours, its links as lines between their ports.
/// Taken when the board is left, so the strip never reads a file.
#[derive(Debug, Clone, Default)]
pub(super) struct Outline {
    cards: Vec<Shape>,
    links: Vec<(Point, Point)>,
}

#[derive(Debug, Clone)]
struct Shape {
    rect: Rectangle,
    color: Option<Color>,
    frame: bool,
}

impl Outline {
    pub(super) fn of(nodes: &[CanvasNode], edges: &[CanvasEdge]) -> Self {
        let rects: HashMap<&str, Rectangle> = nodes.iter().map(|node| (node.id.as_str(), node_world_rect(node))).collect();
        let cards = nodes
            .iter()
            .map(|node| Shape {
                rect: node_world_rect(node),
                color: node.palette_color().map(IntoColor::into_color),
                frame: node.is_group(),
            })
            .collect();
        let links = edges
            .iter()
            .filter_map(|edge| {
                let from = port(*rects.get(edge.from_node.as_str())?, &edge.from_side);
                Some((from, port(*rects.get(edge.to_node.as_str())?, &edge.to_side)))
            })
            .collect();
        Outline { cards, links }
    }

    /// What it covers, or `None` for an empty board.
    fn bounds(&self) -> Option<Rectangle> {
        self.cards.iter().map(|shape| shape.rect).reduce(|all, rect| all.union(&rect))
    }
}

/// Where a link leaves `rect` on `side` — the middle of that edge, or of the
/// card for a link that names none.
fn port(rect: Rectangle, side: &str) -> Point {
    let middle = rect.center();
    match side {
        "top" => Point::new(middle.x, rect.y),
        "bottom" => Point::new(middle.x, rect.y + rect.height),
        "left" => Point::new(rect.x, middle.y),
        "right" => Point::new(rect.x + rect.width, middle.y),
        _ => middle,
    }
}

/// What's kept of a board when it's left: where the camera was (its offset
/// from the window's middle, which a resize since doesn't disturb, and its
/// zoom), and the board as it stood.
struct Left {
    camera: (Vector, f32),
    outline: Outline,
}

/// What a board left keeps, for coming back to while its file is unchanged.
pub(super) struct Warm {
    /// The file's modification time when it was left. Changed since, and
    /// none of this describes it any more.
    mtime: Option<SystemTime>,
    clusters: Clusters,
    bitmaps: HashMap<usize, text_raster::Snapshot>,
    /// Its images that were on screen, held on the GPU. Only those: they're
    /// what a return to the same view draws, and already uploaded, so
    /// holding them costs nothing that wasn't spent.
    pins: Vec<image::Allocation>,
}

/// Drops `value` on a thread of its own. A board's bitmaps are a great many
/// allocations, and freeing 800 of them took 10 ms of the frame that
/// switched boards.
fn drop_elsewhere<T: Send + 'static>(value: T) {
    std::thread::spawn(move || drop(value));
}

/// The boards visited this session, and the strip while it's up.
#[derive(Default)]
pub(super) struct Visits {
    /// Each board once, in the order the trail reached it.
    trail: Vec<PathBuf>,
    /// The board on screen, as a place on the trail. Meaningless while the
    /// trail is empty — the welcome board, which isn't on it.
    at: usize,
    left: HashMap<PathBuf, Left>,
    /// The last boards left, latest last, with what they keep.
    warm: Vec<(PathBuf, Warm)>,
    pub(super) strip: Option<Strip>,
}

/// The strip, while ⌘ is held.
pub(super) struct Strip {
    /// The board letting go lands on, as a place on the trail.
    pub(super) highlight: usize,
    /// Whether ⌘ has been held long enough to show it. Until then, a step
    /// taken is only waiting on ⌘'s release.
    shown: bool,
}

impl Visits {
    /// The trail as a launch starts it: the board launched on, unless that's
    /// the welcome board, which is never gone back to.
    pub(super) fn starting_at(board: Option<PathBuf>) -> Self {
        Visits { trail: board.into_iter().collect(), ..Visits::default() }
    }

    /// `path` was opened, rather than stepped to. What was ahead of the board
    /// on screen is dropped, as a browser drops the pages ahead when a link is
    /// followed, and `path` goes last, once.
    pub(super) fn visit(&mut self, path: &Path) {
        if self.trail.get(self.at).is_some_and(|here| here == path) {
            return;
        }
        self.trail.truncate(self.at + 1);
        self.trail.retain(|seen| seen != path);
        self.trail.push(path.to_owned());
        self.at = self.trail.len() - 1;
    }

    /// `index` on the trail was stepped to: the trail stays as it is.
    pub(super) fn step_to(&mut self, index: usize) {
        self.at = index.min(self.trail.len().saturating_sub(1));
    }

    /// The place `delta` steps from the board on screen — round to the other
    /// end past either — if there's another board on the trail at all.
    fn target(&self, delta: i32) -> Option<usize> {
        let len = self.trail.len();
        (len > 1).then(|| wrap(self.at, delta, len))
    }

    /// `path` is being left: what to come back to.
    pub(super) fn remember(&mut self, path: PathBuf, camera: (Vector, f32), outline: Outline) {
        self.left.insert(path, Left { camera, outline });
    }

    /// Where the camera was when `path` was last left, if it was this session.
    pub(super) fn camera(&self, path: &Path) -> Option<(Vector, f32)> {
        self.left.get(path).map(|left| left.camera)
    }

    /// The board on screen was renamed: it keeps its place and what's kept of
    /// it under the new name.
    pub(super) fn rename(&mut self, from: &Path, to: &Path) {
        for seen in &mut self.trail {
            if seen == from {
                *seen = to.to_owned();
            }
        }
        if let Some(left) = self.left.remove(from) {
            self.left.insert(to.to_owned(), left);
        }
        for (seen, _) in &mut self.warm {
            if seen == from {
                *seen = to.to_owned();
            }
        }
    }

    /// Keeps what `path` had, as it's left; the board left longest ago is
    /// let go of past [`WARM_BOARDS`].
    fn keep_warm(&mut self, path: PathBuf, warm: Warm) {
        self.warm.retain(|(seen, _)| *seen != path);
        self.warm.push((path, warm));
        if self.warm.len() > WARM_BOARDS {
            drop_elsewhere(self.warm.remove(0));
        }
    }

    /// What `path` kept, if it's still the file it was: `mtime` is the file's
    /// modification time now.
    fn take_warm(&mut self, path: &Path, mtime: Option<SystemTime>) -> Option<Warm> {
        let at = self.warm.iter().position(|(seen, _)| seen == path)?;
        let (_, warm) = self.warm.remove(at);
        if warm.mtime == mtime && mtime.is_some() {
            Some(warm)
        } else {
            drop_elsewhere(warm);
            None
        }
    }

    /// An image of `board`'s, held on the GPU — if the board is still kept.
    pub(super) fn pin(&mut self, board: &Path, allocation: Option<image::Allocation>) {
        let kept = self.warm.iter_mut().find(|(seen, _)| seen == board);
        if let (Some((_, warm)), Some(allocation)) = (kept, allocation) {
            warm.pins.push(allocation);
        }
    }

    /// Lets go of everything kept: what it was computed with — the theme, the
    /// font — has changed.
    pub(super) fn forget_warm(&mut self) {
        if !self.warm.is_empty() {
            drop_elsewhere(std::mem::take(&mut self.warm));
        }
    }

    /// Drops the boards `keep` refuses — but never the one on screen.
    fn retain(&mut self, keep: impl Fn(&Path) -> bool) {
        let here = self.trail.get(self.at).cloned();
        self.trail.retain(|seen| Some(seen) == here.as_ref() || keep(seen));
        self.at = here.and_then(|here| self.trail.iter().position(|seen| *seen == here)).unwrap_or(0);
    }
}

/// `from` moved `delta` places round a ring of `len`.
fn wrap(from: usize, delta: i32, len: usize) -> usize {
    (from as i64 + delta as i64).rem_euclid(len as i64) as usize
}

impl App {
    /// ⌘[ or ⌘]: the strip's highlight moves; or, with no strip up, one is
    /// raised a step from this board, round to the other end past either —
    /// nothing, where this is the only board. It shows when the board says
    /// ⌘ has been held long enough ([`App::show_switcher`]).
    pub(super) fn step_boards(&mut self, delta: i32) {
        let visits = &mut self.visits;
        if let Some(strip) = &mut visits.strip {
            strip.highlight = wrap(strip.highlight, delta, visits.trail.len().max(1));
            return;
        }
        // One deleted or moved since would only fail to open.
        visits.retain(Path::is_file);
        if let Some(highlight) = visits.target(delta) {
            visits.strip = Some(Strip { highlight, shown: false });
        }
    }

    /// ⌘ has been held long enough: the strip shows — where [ and ] have
    /// moved it meanwhile, or else on this board, for them to move it from.
    /// Nothing, where there's no other board to go to.
    pub(super) fn show_switcher(&mut self) {
        let visits = &mut self.visits;
        if visits.strip.is_none() {
            visits.retain(Path::is_file);
            if visits.trail.len() < 2 {
                return;
            }
            visits.strip = Some(Strip { highlight: visits.at, shown: false });
        }
        if let Some(strip) = &mut visits.strip {
            strip.shown = true;
        }
    }

    /// ⌘ was let go: to the highlighted board, unless it's this one.
    pub(super) fn land_switch(&mut self) -> Task<Message> {
        match self.visits.strip.take() {
            Some(strip) if strip.highlight != self.visits.at => self.go_to_visited(strip.highlight),
            _ => Task::none(),
        }
    }

    /// The board at `index` on the trail, stepped to — clicked in the strip,
    /// or where letting ⌘ go left the highlight.
    pub(super) fn go_to_visited(&mut self, index: usize) -> Task<Message> {
        self.visits.strip = None;
        match self.visits.trail.get(index).cloned() {
            Some(path) => self.switch_board(path, Some(index)),
            None => Task::none(),
        }
    }

    /// The board on screen is being left: what was computed from it is kept
    /// for coming back to (see [`Warm`]), rather than freed — which also
    /// takes freeing it off the frame that switches — and its images on
    /// screen are asked to stay on the GPU.
    pub(super) fn put_away(&mut self) -> Task<Message> {
        let bitmaps = self.rasters.take();
        let bitmaps = if bitmaps.values().map(|bitmap| bitmap.bytes).sum::<usize>() <= WARM_BITMAPS {
            bitmaps
        } else {
            drop_elsewhere(bitmaps);
            HashMap::new()
        };
        let clusters = std::mem::take(&mut self.clusters);
        let board = self.canvas_path.clone();
        let warm = Warm { mtime: self.disk.mtime, clusters, bitmaps, pins: Vec::new() };
        self.visits.keep_warm(board.clone(), warm);
        let on_screen = self.nodes.iter().filter(|node| self.node_visible(node));
        let pins: Vec<Task<Message>> = on_screen
            .filter_map(|node| self.images.get(&node.id))
            .map(|handle| {
                let board = board.clone();
                image::allocate(handle.clone()).map(move |allocation| Message::ImagePinned(board.clone(), allocation.ok()))
            })
            .collect();
        Task::batch(pins)
    }

    /// The board just opened gets what it kept, if it was left unchanged —
    /// or else works out its clusters and bitmaps afresh: its clusters there
    /// and then when the view opens on them, as at launch, rather than draw
    /// every card until a background pass brings them.
    pub(super) fn bring_out(&mut self) -> Task<Message> {
        let warm = self.visits.take_warm(&self.canvas_path, self.disk.mtime);
        let Some(Warm { clusters, bitmaps, pins, .. }) = warm else {
            let clusters = if board::cluster_opacity(self.camera.zoom) > 0.0 {
                self.clusters_now()
            } else {
                self.clusters = Clusters::default();
                self.schedule_recluster()
            };
            return Task::batch([clusters, self.refresh_snapshots()]);
        };
        // Drawn in the frame that shows them, they stay as long as they're
        // drawn; held past it, they'd hold GPU memory nothing is using.
        drop(pins);
        let (clusters, titles) = clusters.into_titles();
        self.clusters = clusters;
        self.rasters.restore(bitmaps);
        Task::batch([self.install_titles(titles), self.refresh_snapshots()])
    }

    /// The strip over the board, once it has been held long enough to show.
    pub(super) fn strip_view(&self) -> Option<Element<'_, Message>> {
        let strip = self.visits.strip.as_ref().filter(|strip| strip.shown)?;
        let ct = self.theme;
        let font = Font { weight: crate::theme::iced_weight(ct.weight_normal), ..ct.ui_font() };
        let faint = chrome::mix(ct.text_muted, ct.elevated(), 0.35);
        let ink = Ink {
            card: ct.text_muted,
            frame: ct.text,
            link: ct.text_muted,
        };

        // As many as fit beside each other, the highlight kept among them.
        let trail = &self.visits.trail;
        let room = self.camera.viewport.width - 2.0 * (MARGIN + PAD) + GAP;
        let fits = ((room / (THUMB.width + GAP)).floor() as usize).max(1);
        let shown = trail.len().min(fits).min(MAX_SHOWN);
        let first = strip.highlight.saturating_sub(shown / 2).min(trail.len() - shown);

        let boards = (first..first + shown).map(|index| {
            let path = &trail[index];
            // The board on screen as it is now; the others as they were left.
            let outline = if index == self.visits.at {
                Outline::of(&self.nodes, &self.edges)
            } else {
                self.visits.left.get(path).map(|left| left.outline.clone()).unwrap_or_default()
            };
            let lit = index == strip.highlight;
            let picture = container(canvas(Thumbnail { outline, ink }).width(Fill).height(Fill))
                .width(THUMB.width)
                .height(THUMB.height)
                .style(move |_theme| container::Style {
                    background: Some(ct.background.into()),
                    border: Border {
                        radius: 8.0.into(),
                        width: if lit { 2.0 } else { 1.0 },
                        color: if lit { ct.accent } else { Color { a: 0.08, ..ct.text } },
                    },
                    ..container::Style::default()
                });
            let name = path.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default();
            let name = container(
                text(name)
                    .size(12.5)
                    .font(font)
                    .color(if lit { ct.text } else { ct.text_muted })
                    .wrapping(text::Wrapping::None),
            )
            .width(THUMB.width)
            .center_x(THUMB.width)
            .clip(true);
            mouse_area(column![picture, name].spacing(10).align_x(Alignment::Center))
                .on_press(Message::SwitchToVisited(index))
                .into()
        });

        let key = if cfg!(target_os = "macos") { "⌘" } else { "Ctrl" };
        let hint = text(format!("Hold {key} and press [ or ]. Release to switch."))
            .size(11.5)
            .font(font)
            .color(faint);
        let panel = container(column![row(boards).spacing(GAP), hint].spacing(16).align_x(Alignment::Center))
            .padding(PAD)
            .style(move |_theme| chrome::panel(&ct, ct.elevated()));
        // A click on the strip is its boards'; anywhere else stays put.
        let panel = mouse_area(panel).on_press(Message::Noop);
        let backdrop = container(panel)
            .center(Fill)
            .padding(MARGIN)
            .style(|_theme| container::Style {
                background: Some(Color { a: 0.45, ..Color::BLACK }.into()),
                ..container::Style::default()
            });
        Some(mouse_area(backdrop).on_press(Message::CancelSwitch).into())
    }
}

/// While the strip is up: ⌘ put to any other use puts it away — a chord's
/// key, which the board still acts on, or a press or scroll, which the strip
/// takes. ⌘[ ⌘], Esc and ⌘'s release are the board widget's, in order with
/// the press that raised it — see the module's notes.
pub(crate) fn key_event(
    event: iced::Event,
    _status: iced::event::Status,
    _window: iced::window::Id,
) -> Option<Message> {
    board::uses_command(&event).then_some(Message::CancelSwitch)
}

/// The colours a board is outlined in.
#[derive(Debug, Clone, Copy)]
struct Ink {
    /// An uncoloured card.
    card: Color,
    /// A frame, barely there: it only says where a group is.
    frame: Color,
    link: Color,
}

/// A board drawn small: its frames, links and cards fitted to the box, with
/// no text — at this size it's recognised by its shape.
struct Thumbnail {
    outline: Outline,
    ink: Ink,
}

impl<Message> canvas::Program<Message> for Thumbnail {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &iced::Renderer,
        _theme: &iced::Theme,
        bounds: Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        use canvas::{Frame, Path, Stroke};
        let mut frame = Frame::new(renderer, bounds.size());
        let Some(world) = self.outline.bounds() else {
            return vec![frame.into_geometry()];
        };
        const INSET: f32 = 12.0;
        // No larger than a board's own cards would look beside a full one:
        // a single card fitted to the box would read as a box, not a board.
        const MAX_SCALE: f32 = 0.15;
        let scale = ((bounds.width - 2.0 * INSET) / world.width.max(1.0))
            .min((bounds.height - 2.0 * INSET) / world.height.max(1.0))
            .min(MAX_SCALE);
        let middle = world.center();
        let place = |p: Point| {
            Point::new(bounds.width / 2.0 + (p.x - middle.x) * scale, bounds.height / 2.0 + (p.y - middle.y) * scale)
        };
        let rounded = |rect: Rectangle| {
            let size = Size::new(rect.width * scale, rect.height * scale);
            let radius = (8.0 * scale).max(2.0).min(size.width.min(size.height) / 2.0);
            Path::rounded_rectangle(place(rect.position()), size, radius.into())
        };

        let Ink { card, frame: group, link } = self.ink;
        for shape in self.outline.cards.iter().filter(|shape| shape.frame) {
            let path = rounded(shape.rect);
            frame.fill(&path, Color { a: 0.025, ..group });
            frame.stroke(&path, Stroke::default().with_color(Color { a: 0.16, ..group }).with_width(1.0));
        }
        let line = Stroke::default().with_color(link).with_width((1.5 * scale).max(0.75));
        for &(from, to) in &self.outline.links {
            frame.stroke(&Path::line(place(from), place(to)), line);
        }
        for shape in self.outline.cards.iter().filter(|shape| !shape.frame) {
            let path = rounded(shape.rect);
            let color = shape.color.unwrap_or(card);
            frame.fill(&path, Color { a: 0.33, ..color });
            frame.stroke(&path, Stroke::default().with_color(color).with_width(1.0));
        }
        vec![frame.into_geometry()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn trail(visits: &Visits) -> Vec<&str> {
        visits.trail.iter().map(|path| path.to_str().unwrap()).collect()
    }

    #[test]
    fn opening_a_board_drops_what_was_ahead_and_lists_it_once() {
        let mut visits = Visits::starting_at(Some("a".into()));
        for board in ["b", "c"] {
            visits.visit(Path::new(board));
        }
        assert_eq!((trail(&visits), visits.at), (vec!["a", "b", "c"], 2));

        // Back two, then open a board: what was ahead goes, as in a browser.
        visits.step_to(0);
        visits.visit(Path::new("d"));
        assert_eq!((trail(&visits), visits.at), (vec!["a", "d"], 1));

        // A board already on it moves to the end rather than appear twice;
        // opening the one on screen changes nothing.
        visits.visit(Path::new("a"));
        visits.visit(Path::new("a"));
        assert_eq!((trail(&visits), visits.at), (vec!["d", "a"], 1));
    }

    #[test]
    fn a_step_wraps_round_and_needs_another_board() {
        let mut visits = Visits::starting_at(Some("a".into()));
        assert_eq!(visits.target(-1), None, "a board alone has nowhere to go");
        for board in ["b", "c"] {
            visits.visit(Path::new(board));
        }
        assert_eq!(visits.target(-1), Some(1));
        assert_eq!(visits.target(1), Some(0), "past the last, round to the first");
        visits.step_to(0);
        assert_eq!(visits.target(-1), Some(2), "and before the first, the last");
        // The welcome board isn't on the trail at all.
        assert_eq!(Visits::starting_at(None).target(-1), None);
    }

    #[test]
    fn what_a_board_kept_is_only_for_the_file_it_was() {
        let warm = |seconds| Warm {
            mtime: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)),
            clusters: Clusters::default(),
            bitmaps: HashMap::new(),
            pins: Vec::new(),
        };
        let at = |seconds| Some(SystemTime::UNIX_EPOCH + Duration::from_secs(seconds));
        let mut visits = Visits::default();
        for board in ["a", "b", "c"] {
            visits.keep_warm(board.into(), warm(1));
        }
        // Only the last ones left are kept.
        assert!(visits.take_warm(Path::new("a"), at(1)).is_none());
        // Changed since, and it's let go of rather than handed back.
        assert!(visits.take_warm(Path::new("b"), at(2)).is_none());
        assert!(visits.take_warm(Path::new("b"), at(1)).is_none());
        assert!(visits.take_warm(Path::new("c"), at(1)).is_some());
    }

    #[test]
    fn a_board_that_has_gone_is_dropped_but_never_the_one_on_screen() {
        let mut visits = Visits::starting_at(Some("a".into()));
        for board in ["b", "c"] {
            visits.visit(Path::new(board));
        }
        visits.step_to(1);
        visits.retain(|_| false);
        assert_eq!((trail(&visits), visits.at), (vec!["b"], 0));
    }

    #[test]
    fn a_renamed_board_keeps_its_place_and_its_camera() {
        let mut visits = Visits::starting_at(Some("a".into()));
        visits.visit(Path::new("b"));
        visits.remember("a".into(), (Vector::new(10.0, 20.0), 2.0), Outline::default());
        visits.rename(Path::new("a"), Path::new("z"));
        assert_eq!(trail(&visits), ["z", "b"]);
        assert_eq!(visits.camera(Path::new("z")), Some((Vector::new(10.0, 20.0), 2.0)));
    }
}
