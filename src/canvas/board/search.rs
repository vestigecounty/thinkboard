//! The find bar on the board: the matches highlighted in their cards, the
//! cards without one dimmed, and the ring round the current hit's card.

use super::*;

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
pub(super) const SEARCH_HIT_CURRENT: Color = Color::from_rgb(1.0, 0.882, 0.302);

/// 55% of it — and that is the floor, held by the same two thresholds
/// `core::theme` uses. Black ink on this is 5.0:1 (body text wants 4.5) and it
/// stands 3.4:1 off a card (a UI mark wants 3.0). At 50% the ink drops to
/// 4.3:1 and the fill to 2.9:1, so a dim hit stops reading as a highlight and
/// starts reading as a smudge on the card.
pub(super) const SEARCH_HIT: Color = Color::from_rgb(0.55, 0.485, 0.166);

pub(super) const SEARCH_INK: Color = Color::BLACK;

/// Where a search hit falls inside one laid-out run: its byte range, and the
/// logical offset and width of the glyphs it covers.
///
/// Both passes need it — the fill is drawn from the geometry, and the text is
/// split on the byte range so the glyphs over the fill can change colour — so
/// it's measured once and used twice.
#[derive(Clone, Copy)]
pub(super) struct Hit {
    pub(super) start: usize,
    pub(super) end: usize,
    pub(super) x: f32,
    pub(super) width: f32,
}

/// The active board search, passed to [`Board`] while the find bar is open.
pub struct SearchView<'a> {
    /// The current query (case-insensitive substring, highlighted in card text).
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

impl Board<'_> {
    /// [`search_dim`] for this board's search, if there is one.
    pub(super) fn search_dim(&self, index: usize) -> f32 {
        self.search.as_ref().map_or(1.0, |search| search_dim(search.cards, index))
    }

    /// Rings the card holding the hit being stepped through: two pixels of
    /// accent just outside its edge and a soft halo past that, in screen
    /// pixels whatever the zoom — it points at a card, so it shouldn't grow
    /// thin and vanish as the board zooms out, nor heavy as it zooms in.
    pub(super) fn draw_search_focus(&self, frame: &mut Frame) {
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

    /// Paints a highlight rect behind each occurrence of `query` (case-
    /// insensitive) within a single laid-out run. Occurrences that cross a run
    /// (style/line) boundary aren't caught — acceptable for plain cards.
    /// Where `query` occurs in one laid-out run, case-insensitively.
    ///
    /// Occurrences that straddle a run boundary (a style or line change) aren't
    /// caught — acceptable for plain cards, and the alternative is matching
    /// across text that isn't adjacent on screen.
    pub(super) fn match_hits(&self, run: &crate::text_raster::TextRun, query: &str) -> Vec<Hit> {
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
    pub(super) fn draw_search_hits(
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
            self.config.center_card_text,
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

    /// Paints one hit's background. Opaque, with [`SEARCH_INK`] drawn over it —
    /// which is what lets the fill be as strong as it likes instead of having
    /// to leave the card's own text readable through it.
    pub(super) fn fill_hit(
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
