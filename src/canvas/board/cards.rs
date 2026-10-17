//! Cards: their geometry on screen, and drawing them — text cards with live
//! Markdown, file, link and image cards, frames and their headers, and the
//! scrollbar of a card whose text overflows it.

use super::*;

/// Outline weight of a *selected* card, and the floor it holds at when zoomed
/// out. Shared by every card kind so a selection reads the same wherever it
/// lands; the unselected weights differ by kind and stay where they're used.
pub(super) const SELECTED_BORDER: f32 = 3.5;

pub(super) const SELECTED_BORDER_MIN: f32 = 3.0;

/// The smallest a drawn glyph is allowed to get. Below this, text on a card
/// zoomed out is a smudge rather than something you can read the shape of.
pub(super) const MIN_TEXT_PX: f32 = 6.0;

/// Outline weight for a selected card. See [`SELECTED_BORDER`].
pub(super) fn selected_border(zoom: f32) -> f32 {
    (SELECTED_BORDER * zoom).max(SELECTED_BORDER_MIN)
}

/// Width (screen px, pre-zoom) of a node's scrollbar thumb.
pub(super) const NODE_SCROLLBAR_WIDTH: f32 = 6.0;

/// Total vertical padding (world px) around a text node's text, split top/bottom
/// — the shared `NODE_PAD_Y`, so a fitted node has no overflow and a scrolled one
/// keeps padding at both ends.
pub(super) const NODE_TEXT_PAD_Y: f32 = crate::text_raster::NODE_PAD_Y;

/// Horizontal inset (world px, per side) of a reference card's title and body,
/// matching where the Markdown renderer puts a text node's first column.
pub(super) const NODE_TEXT_PAD_X: f32 = crate::text_raster::NODE_PAD_X / 2.0 + crate::text_raster::MD_PADDING;

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
pub(super) const REFERENCE_TITLE_SIZE: f32 = FONT_SIZE - 3.0;

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
pub(super) fn editing_thumb(
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
/// where it stops growing, so without a scrollbar the rest of the card would
/// simply be unreachable. Public, with [`scroll_offset`] and [`text_frame`],
/// because the app maps points to text too (a double-click's caret, a find
/// hit), and a map that disagrees with what's drawn lands on the wrong line.
pub fn scrolls(config: &crate::config::Config, node: &CanvasNode) -> bool {
    node.is_text() && (config.card_scrollbars || node.height >= crate::text_raster::MAX_AUTO_HEIGHT - 0.5)
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

pub(super) fn scrollbar_thumb(
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

/// Which nodes a [`Board::draw_nodes`] pass renders. The selected node(s) are
/// lifted out of the cached scene into the uncached overlay so they layer above
/// everything else — a single geometry can't put a node's card mesh above
/// another node's image, since images always composite over meshes.
#[derive(Clone, Copy)]
pub(super) enum NodePass<'p> {
    /// Every node (used in cluster mode, where there's no overlay to lift into).
    All,
    /// The unselected cards in one plane of a pile (see [`Board::stacking`]):
    /// plane 0 is the cached base scene, the rest are stacked above it.
    Plane(usize, &'p [usize]),
    /// Only the selected node(s) — drawn into the overlay, on top.
    SelectedOnly,
}

/// The last component of a file node's path, extension included.
pub(super) fn file_name(node: &CanvasNode) -> Option<String> {
    let file = node.file.as_deref()?;
    file.rsplit(['/', '\\']).next().filter(|name| !name.is_empty()).map(str::to_owned)
}

/// What kind of file an attachment is, from its extension: "PDF file".
pub(super) fn file_kind(node: &CanvasNode) -> String {
    file_name(node)
        .and_then(|name| name.rsplit_once('.').map(|(_, ext)| ext.to_uppercase()))
        .filter(|ext| !ext.is_empty())
        .map_or_else(|| "File".to_owned(), |ext| format!("{ext} file"))
}

/// Whether `text`'s first heading, before anything else, is `name`.
pub(super) fn first_heading_is(text: &str, name: &str) -> bool {
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
pub(super) fn url_host(url: &str) -> &str {
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

impl Board<'_> {
    /// The active snap spacing (world px), or `0.0` when snap-to-grid is off.
    /// A card's corner radius on screen: the theme's, at the current zoom.
    pub(super) fn card_radius(&self) -> f32 {
        self.theme.card_radius * self.camera.zoom
    }

    /// The same for a group frame or a cluster box.
    pub(super) fn frame_radius(&self) -> f32 {
        self.theme.frame_radius * self.camera.zoom
    }

    /// Spawn animation factors for a node: (scale, alpha).
    pub(super) fn spawn_factors(&self, index: usize) -> (f32, f32) {
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
    pub(super) fn node_screen_rect(&self, index: usize, node: &CanvasNode) -> Rectangle {
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

    pub(super) fn node_rect(&self, node: &CanvasNode) -> Rectangle {
        Rectangle::new(
            Point::new(node.x, node.y),
            Size::new(node.width, node.height),
        )
    }

    pub(super) fn scrollable(&self, node: &CanvasNode) -> bool {
        scrolls(self.config, node)
    }

    pub(super) fn node_content_height(&self, text: &str, width: f32) -> f32 {
        content_height(text, width, self.theme)
    }

    /// The clamped scroll offset (world px) for a node, 0 when it fits.
    pub(super) fn node_scroll_offset(&self, node: &CanvasNode) -> f32 {
        scroll_offset(self.config, self.theme, self.node_scroll, node)
    }

    /// The rectangle (screen) a card's Markdown is laid out against — see
    /// [`text_frame`].
    pub(super) fn node_text_frame(&self, node: &CanvasNode, rect: Rectangle) -> Rectangle {
        let scroll = self.scrollable(node).then(|| self.node_scroll_offset(node));
        text_frame(rect, scroll, self.camera.zoom)
    }


    /// The plane each card is drawn in, by node index, and the deepest one
    /// used. Without `lift`, or for a selected card or a frame, it's 0 — the
    /// selected are lifted into the overlay anyway, and a frame is a backdrop,
    /// enclosing cards rather than covering them.
    pub(super) fn stacking(&self, lift: bool) -> (Vec<usize>, usize) {
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
    pub(super) fn overlay_covers_cards(&self) -> bool {
        let (mut over, mut under) = (Vec::new(), Vec::new());
        for (index, node) in self.nodes.iter().enumerate().filter(|(_, node)| !node.is_group()) {
            let rect = self.node_rect(node);
            if self.selected.contains(&index) { over.push(rect) } else { under.push(rect) }
        }
        over.extend(self.ghosts.iter().map(|ghost| ghost.rect));
        covers_any(&over, &under)
    }

    pub(super) fn draw_nodes(&self, frame: &mut Frame, pass: NodePass) {
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
            // The welcome board's primary button (`tb_primary`): a soft ring in
            // its own colour, so the first thing to do reads as first. A band
            // outside the card rather than a fill behind it — a coloured card
            // is translucent, and a fill would tint it.
            if node.extra.get("tb_primary").and_then(serde_json::Value::as_bool) == Some(true)
                && let Some(color) = accent
            {
                let spread = 5.0 * zoom;
                frame.stroke(
                    &rounded_path(expand_rect(rect, spread / 2.0), self.card_radius() + spread / 2.0),
                    Stroke::default().with_color(Color { a: 0.14 * alpha, ..color }).with_width(spread),
                );
            }
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
    pub(super) fn draw_groups(&self, frame: &mut Frame) {
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
    pub(super) fn draw_reference_card(
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
    pub(super) fn draw_note_card(&self, frame: &mut Frame, node: &CanvasNode, rect: Rectangle, alpha: f32) {
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
    pub(super) fn draw_link_card(&self, frame: &mut Frame, node: &CanvasNode, rect: Rectangle, alpha: f32) {
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
    pub(super) fn stroke_card(
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
    pub(super) fn draw_editing_thumb(&self, frame: &mut Frame, index: usize) {
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
    pub(super) fn draw_markdown_live(
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
            self.config.center_card_text,
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

    /// Draws the bar beside each line of a quote, for the live path — the
    /// bitmap path paints its own (`text_raster::snapshot_markdown`).
    ///
    /// Before the clip, like the search hits and for the same reason, so it's
    /// clipped to `card` by arithmetic instead: a scrolled card lays its text
    /// out above and below itself.
    #[allow(clippy::too_many_arguments)] // mirrors `draw_markdown_live`'s
    pub(super) fn draw_quote_bars(
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
            self.config.center_card_text,
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
    pub(super) fn draw_table_rules(
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
            self.config.center_card_text,
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
