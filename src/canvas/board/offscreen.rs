//! What the board says when there's nothing on it to see: a marker toward each
//! group of cards when none is on screen (placed by `canvas::markers`), and on
//! an empty board, how to start one.

use super::*;

/// The command palette's chords, as the empty board's hint prints them —
/// Cmd on macOS and Ctrl elsewhere, as [`chord`] reads them. ⌘⇧P works too,
/// for VS Code's habit, but two is as many as a hint should name.
#[cfg(target_os = "macos")]
pub(super) const PALETTE_CHORDS: &str = "⌘K or ⌘P";

#[cfg(not(target_os = "macos"))]
pub(super) const PALETTE_CHORDS: &str = "Ctrl+K or Ctrl+P";

/// An off-screen group of cards, as its marker needs it.
pub(super) struct Offscreen<'a> {
    /// In world space: the camera flies to its middle.
    pub(super) bounds: Rectangle,
    /// Its main card's text, as Markdown — what names it.
    pub(super) text: std::borrow::Cow<'a, str>,
    /// The image card that stands for it, when one does.
    pub(super) image: Option<&'a str>,
    pub(super) cards: usize,
}

/// What a marker says, laid out.
pub(super) struct MarkerLabel {
    pub(super) rect: Rectangle,
    pub(super) title: String,
    pub(super) title_width: f32,
    /// "5 cards", in the chip a hovered marker shows; `None` for the plain
    /// name the nearest one shows.
    pub(super) count: Option<String>,
}

/// A marker's name, and the count beside it in its chip.
pub(super) const MARKER_TITLE_SIZE: f32 = 12.0;

pub(super) const MARKER_COUNT_SIZE: f32 = 11.5;

/// The chip a hovered marker's name sits in: the find bar's surface, small.
pub(super) const CHIP_HEIGHT: f32 = 26.0;

pub(super) const CHIP_PAD_X: f32 = 9.0;

pub(super) const CHIP_GAP: f32 = 6.0;

pub(super) const CHIP_RADIUS: f32 = 7.0;

/// How far `point` is from `rect`: 0 inside it.
pub(super) fn distance_to(rect: Rectangle, point: Point) -> f32 {
    let dx = (rect.x - point.x).max(point.x - (rect.x + rect.width)).max(0.0);
    let dy = (rect.y - point.y).max(point.y - (rect.y + rect.height)).max(0.0);
    dx.hypot(dy)
}

impl Board<'_> {
    /// When no card is on screen — panned off into empty space — the groups
    /// of cards off it, and a marker on the window's edge toward each
    /// (`canvas::markers`). `None` while any card is in view, and on a board
    /// with none. A card counts as in view by its position whatever the zoom,
    /// so a board zoomed out to its clusters isn't given markers for them.
    pub(super) fn offscreen(&self) -> Option<(Vec<Offscreen<'_>>, Vec<markers::Marker>)> {
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
        // The groups semantic zoom draws. Until the first clustering lands —
        // and on a board of frames alone, which never cluster — each card is
        // a group of one.
        let groups: Vec<Offscreen<'_>> = if self.clusters.is_empty() {
            self.nodes
                .iter()
                .map(|node| Offscreen {
                    bounds: Rectangle::new(Point::new(node.x, node.y), Size::new(node.width, node.height)),
                    text: match node.label() {
                        Some(label) if node.is_group() => label.into(),
                        _ => crate::document::cluster::summary_text(node).into(),
                    },
                    image: None,
                    cards: 1,
                })
                .collect()
        } else {
            self.clusters
                .iter()
                .map(|cluster| Offscreen {
                    bounds: cluster.bounds,
                    text: cluster.text.as_str().into(),
                    image: cluster.image_id.as_deref(),
                    cards: cluster.members.len(),
                })
                .collect()
        };
        let eye = self.camera.to_world(Point::ORIGIN + self.camera.center());
        let toward: Vec<(Point, f32)> = groups
            .iter()
            .map(|group| (self.camera.to_screen(group.bounds.center()), distance_to(group.bounds, eye)))
            .collect();
        let placed = markers::place(self.marker_view(), &toward);
        Some((groups, placed))
    }

    /// Where the markers are on screen, nearest first — for the tour, which
    /// points at one as a hand would.
    #[cfg(test)]
    pub(crate) fn marker_points(&self) -> Vec<Point> {
        self.offscreen().map(|(_, placed)| placed.iter().map(|marker| marker.at).collect()).unwrap_or_default()
    }

    /// The window as the markers see it: below the notch, which would hide a
    /// dot on the top edge.
    pub(super) fn marker_view(&self) -> Rectangle {
        let top = self.top_safe_inset;
        Rectangle::new(
            Point::new(0.0, top),
            Size::new(self.camera.viewport.width, (self.camera.viewport.height - top).max(0.0)),
        )
    }

    /// The marker at `position`, as its group's index and the world point to
    /// fly to: its dot, generously, or the label it's showing — the nearest
    /// group's name, and the chip of the one `hovered`.
    pub(super) fn marker_at(&self, position: Point, hovered: Option<usize>) -> Option<(usize, Point)> {
        let (groups, placed) = self.offscreen()?;
        let hit = placed.iter().find(|marker| {
            // A dot is small; the target around it isn't.
            marker.at.distance(position) <= (marker.radius + markers::HALO).max(12.0)
                || self
                    .marker_label(marker, &groups[marker.group], hovered)
                    .is_some_and(|label| label.rect.contains(position))
        })?;
        Some((hit.group, groups[hit.group].bounds.center()))
    }

    /// What a marker says, and where: while it's the one `hovered`, a chip
    /// with its group's name and how many cards — the find bar's surface,
    /// small; otherwise, for the nearest group, its name in plain text; and
    /// nothing for the rest. The nearest's name stays while another is
    /// hovered: hidden then, it blinked on and off with every dot the cursor
    /// crossed on its way along the edge.
    pub(super) fn marker_label(&self, marker: &markers::Marker, group: &Offscreen<'_>, hovered: Option<usize>) -> Option<MarkerLabel> {
        let chip = hovered == Some(marker.group);
        if !chip && !marker.nearest {
            return None;
        }
        let title = match (group.image, markers::title(&group.text)) {
            (_, title) if !title.is_empty() => title,
            // A group summed up by an image is named by its file.
            (Some(id), _) => self
                .nodes
                .iter()
                .find(|node| node.id == id)
                .and_then(|node| node.file_title())
                .unwrap_or_else(|| "Image".to_owned()),
            (None, _) => "Untitled".to_owned(),
        };
        let width = |text: &str, size: f32| {
            crate::text_raster::measure(text, 100_000.0, size, 1.0, self.theme.font_text, self.theme.font_features).0
        };
        let title_width = width(&title, MARKER_TITLE_SIZE);
        let (size, count) = if chip {
            let count = format!("{} card{}", group.cards, if group.cards == 1 { "" } else { "s" });
            let count_width = width(&count, MARKER_COUNT_SIZE);
            let chip = CHIP_PAD_X + title_width + CHIP_GAP + count_width + CHIP_PAD_X;
            (Size::new(chip, CHIP_HEIGHT), Some(count))
        } else {
            (Size::new(title_width, MARKER_TITLE_SIZE * crate::text_raster::LINE_HEIGHT), None)
        };
        Some(MarkerLabel { rect: markers::label_box(self.marker_view(), marker, size), title, title_width, count })
    }

    /// What a new, empty board says in its middle: how to put the first card on
    /// it, and where every other action lives. A blank board said nothing at
    /// all, and the right-click that starts one is the gesture nobody tries
    /// first. Muted, and drawn on the canvas rather than as a widget above it,
    /// so it can never catch the click it describes.
    pub(super) fn draw_empty_hint(&self, frame: &mut Frame, size: Size) {
        let a = self.reveal;
        let center = Point::new(size.width / 2.0, size.height / 2.0);
        // Whichever gesture works here: double-click only where it's been
        // turned on and a click doesn't dismiss the board instead.
        let gesture = if self.config.double_click_adds_card(self.overlay) {
            "Double-click"
        } else {
            "Right-click"
        };
        frame.fill_text(canvas::Text {
            content: format!("{gesture} to add a card"),
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

    /// The markers: a dot per off-screen group in the accent, the nearest
    /// ringed and named, and in `chips`, drawn over them, the one `hovered`
    /// named in a chip.
    pub(super) fn draw_markers(
        &self,
        frame: &mut Frame,
        chips: &mut Frame,
        (groups, placed): &(Vec<Offscreen<'_>>, Vec<markers::Marker>),
        hovered: Option<usize>,
    ) {
        let a = self.reveal;
        let accent = self.theme.accent;
        // Farthest first, so a nearer dot — and the nearest's ring — is drawn
        // over any it touches.
        for marker in placed.iter().rev() {
            if marker.nearest {
                frame.fill(&Path::circle(marker.at, marker.radius + markers::HALO), Color { a: 0.18 * a, ..accent });
            }
            let alpha = if hovered == Some(marker.group) { 1.0 } else { marker.alpha };
            frame.fill(&Path::circle(marker.at, marker.radius), Color { a: alpha * a, ..accent });
        }
        for marker in placed {
            let Some(label) = self.marker_label(marker, &groups[marker.group], hovered) else {
                continue;
            };
            let text = |frame: &mut Frame, content: String, x: f32, size: f32, color: Color| {
                frame.fill_text(canvas::Text {
                    content,
                    position: Point::new(x, label.rect.center_y()),
                    color,
                    size: size.into(),
                    font: self.theme.ui_font(),
                    align_x: TextAlignment::Left,
                    align_y: Vertical::Center,
                    ..canvas::Text::default()
                });
            };
            let Some(count) = label.count else {
                // Plain, a step back from the cards' own text: it labels the
                // edge, not the board.
                text(frame, label.title, label.rect.x, MARKER_TITLE_SIZE, Color { a: 0.85 * a, ..self.theme.text });
                continue;
            };
            let frame = &mut *chips;
            let rect = label.rect;
            let radius: Radius = CHIP_RADIUS.into();
            // A soft shadow in two steps — a canvas fill has none of its own.
            for (spread, alpha) in [(4.0, 0.12), (2.0, 0.22)] {
                frame.fill(
                    &Path::rounded_rectangle(
                        Point::new(rect.x - spread, rect.y - spread + 4.0),
                        Size::new(rect.width + 2.0 * spread, rect.height + 2.0 * spread),
                        (CHIP_RADIUS + spread).into(),
                    ),
                    Color { a: alpha * a, ..Color::BLACK },
                );
            }
            let chip = Path::rounded_rectangle(rect.position(), rect.size(), radius);
            frame.fill(&chip, Color { a, ..self.theme.elevated() });
            frame.stroke(&chip, Stroke::default().with_color(Color { a: 0.10 * a, ..self.theme.text }).with_width(1.0));
            let x = rect.x + CHIP_PAD_X;
            text(frame, label.title, x, MARKER_TITLE_SIZE, Color { a, ..self.theme.text });
            text(frame, count, x + label.title_width + CHIP_GAP, MARKER_COUNT_SIZE, Color { a, ..self.theme.text_muted });
        }
    }
}
