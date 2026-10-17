//! Semantic-zoom cluster cards (the render half of clustering).
//!
//! When zoomed out far enough that individual cards are unreadable, a group
//! of nearby cards is replaced by one large summary card showing the group's
//! main card, at a font size fitted to the card's box so it stays comfortable
//! to read. Computed once up front — nothing is recomputed while zooming.
//!
//! The grouping itself (proximity, containment, main-card choice, cross-group
//! links) is UI-agnostic and lives in [`thinkboard_core::cluster`]. This module
//! wraps its bare bounds in `iced::Rectangle` and adds what needs the text
//! rasterizer: fitting a font size to the box, and choosing between a text and
//! an image summary.

use iced::{Point, Rectangle, Size};

use crate::model::{CanvasEdge, CanvasNode, NodeKind};
use crate::text_raster;
use crate::theme::Theme;

pub use thinkboard_core::cluster::Link as ClusterLink;

/// Text inset (world px, per side) within the cluster card.
pub const PADDING: f32 = 34.0;

#[derive(Debug, Clone)]
pub struct Clustering {
    pub clusters: Vec<Cluster>,
    /// Directed links between connected clusters.
    pub links: Vec<ClusterLink>,
}

#[derive(Debug, Clone)]
pub struct Cluster {
    pub bounds: Rectangle,
    /// Main-card text; empty when the main card is an image.
    pub text: String,
    /// Font size in world units, fitted to `bounds`.
    pub font_size: f32,
    /// Node id when the main card is an image, to render it large instead.
    pub image_id: Option<String>,
    /// Member node indices, for dragging the whole cluster.
    pub members: Vec<usize>,
}

/// Groups nearby cards and summarizes each with its main card, rendered either
/// as text fitted to the card or as the main card's image.
pub fn compute(nodes: &[CanvasNode], edges: &[CanvasEdge], theme: &Theme) -> Clustering {
    let grouping = thinkboard_core::cluster::group(nodes, edges);

    let clusters = grouping
        .groups
        .into_iter()
        .map(|group| {
            let (x, y, width, height) = group.bounds;
            let bounds = Rectangle::new(Point::new(x, y), Size::new(width, height));
            let node = &nodes[group.main];

            if node.node_kind() == NodeKind::Image {
                Cluster {
                    bounds,
                    text: String::new(),
                    font_size: 0.0,
                    image_id: Some(node.id.clone()),
                    members: group.members,
                }
            } else {
                // Notes, links and attachments summarize by name rather than by
                // body: at cluster zoom the title is what identifies them, and
                // a note's text isn't in the document at all.
                let text = summary_text(node);
                let font_size = fit_font_size(&text, bounds, theme);
                Cluster {
                    bounds,
                    text,
                    font_size,
                    image_id: None,
                    members: group.members,
                }
            }
        })
        .collect();

    Clustering {
        clusters,
        links: grouping.links,
    }
}

/// The text a cluster card shows for its main card: the card's own text for a
/// text card, and the file title / URL for the kinds whose content isn't in
/// the document.
pub(crate) fn summary_text(node: &CanvasNode) -> String {
    match node.node_kind() {
        NodeKind::Note | NodeKind::Attachment => node.file_title().unwrap_or_default(),
        NodeKind::Link => node.url().unwrap_or_default().to_owned(),
        _ => node.text.as_deref().unwrap_or("").trim().to_owned(),
    }
}

/// The smallest font size the search will settle on.
const MIN_FIT: f32 = 8.0;

/// How precisely the size is resolved.
///
/// Every step costs a full Markdown layout at a size nothing has measured
/// before, so precision *is* the cost: the number of cold measurements is
/// `log2(bracket / this)`. A cluster label is a display size fitted to a box —
/// a pixel either way is invisible — so resolving past 1 px buys nothing and
/// charges ~35 us a step for it.
const FIT_PRECISION: f32 = 1.0;

/// Largest font (world units) whose wrapped text fits the padded box —
/// bigger boxes get proportionally bigger, readable text. Measures the
/// *rendered* Markdown with the theme font, matching how the cluster is drawn,
/// so the fitted size never overflows (measuring with a narrower fallback font
/// would let wider theme text spill onto an extra line).
///
/// Font fitting, not the grouping it summarizes, is the bulk of a recluster: a
/// cold measurement costs ~35 µs and a plain binary search over
/// `[MIN_FIT, avail_h]` needs one per halving. So the search is seeded rather
/// than started blind — one probe, then `probe_size * avail_h / probe_height`
/// as an estimate, which is exact whenever the probe already wraps the way the
/// answer does and an over-estimate otherwise (growing the size can only wrap
/// text into *more* lines, never fewer). Bracketing tightly around that, and
/// resolving only to [`FIT_PRECISION`], takes it from ~10 cold measurements to
/// ~6.
///
/// Worth knowing before optimizing further: this is the *cold* path. A repeat
/// recluster whose texts and boxes are unchanged probes the same sizes and hits
/// the measurement cache throughout, which is why a warm recluster is ~75x
/// cheaper. The cold cost is what a board pays on load.
///
/// Both this and the plain search it replaced assume "fits" is monotonic in
/// size. It is, for any box a cluster card realistically takes — but not for a
/// very narrow one, where a long word overflows the width at one size and
/// rewraps at the next, so the predicate flips back and forth. Either search
/// then returns *some* size that fits rather than provably the largest, which
/// is the right failure: the card is legible either way.
fn fit_font_size(text: &str, bounds: Rectangle, theme: &Theme) -> f32 {
    if text.is_empty() {
        return 16.0;
    }
    let avail_w = (bounds.width - PADDING * 2.0).max(20.0);
    let avail_h = (bounds.height - PADDING * 2.0).max(20.0);

    // No bigger than lets the widest word fit the column. Past that the shaper
    // breaks words mid-letter rather than overflow — "Bud / get: ~€9 / 00" in
    // a tall, narrow cluster — and a broken word *fits*, so the height test
    // alone grew the type until it filled the box. Width scales with size, so
    // one measurement gives the cap.
    const AT: f32 = 100.0;
    let column = avail_w - 2.0 * text_raster::MD_PADDING;
    let widest = text_raster::widest_word(text, AT, theme);
    let word_cap = if widest > 0.0 { AT * column / widest } else { f32::MAX };

    let fits = |size: f32| {
        if size > word_cap {
            return false;
        }
        let (w, h) = text_raster::measure_markdown(text, avail_w, size, theme);
        w <= avail_w && h <= avail_h
    };

    // One probe, in the middle of the plausible range, to calibrate.
    let probe = (avail_h / 4.0).clamp(MIN_FIT, avail_h);
    let (_, probe_h) = text_raster::measure_markdown(text, avail_w, probe, theme);
    if probe_h <= 0.0 {
        return probe;
    }

    // Height is linear in size at a fixed line count, so this lands on the
    // answer whenever the probe already wraps the way the answer does, and
    // over-estimates otherwise.
    let estimate = (probe * avail_h / probe_h).clamp(MIN_FIT, avail_h);

    // Bracket it. `hi` must not fit and `lo` must fit; the estimate is an upper
    // bound in theory, but the width constraint and Markdown's block spacing
    // can break that, so widen until the bracket really does straddle.
    // `lo` is clamped to the floor: a box too small for the text at any legible
    // size must settle on `MIN_FIT` and clip, never search below it.
    let mut lo = (estimate * 0.92).max(MIN_FIT);
    let mut hi = (estimate * 1.03).clamp(lo + FIT_PRECISION, avail_h.max(lo + FIT_PRECISION));
    let mut guard = 0;
    while lo > MIN_FIT && !fits(lo) && guard < 8 {
        hi = lo;
        lo = (lo * 0.6).max(MIN_FIT);
        guard += 1;
    }
    while hi < avail_h && fits(hi) && guard < 16 {
        lo = hi;
        hi = (hi * 1.6).min(avail_h);
        guard += 1;
    }
    if !fits(lo) {
        return MIN_FIT; // even the floor overflows; nothing to search
    }

    while hi - lo > FIT_PRECISION {
        let mid = (lo + hi) / 2.0;
        if fits(mid) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    lo
}

#[cfg(test)]
mod tests {
    use super::*;
    use thinkboard_core::fixtures::{self, edge};

    fn text_node(id: &str, x: f32, y: f32, w: f32, h: f32) -> CanvasNode {
        CanvasNode { x, y, width: w, height: h, ..fixtures::node(id) }
    }
    use crate::model::CanvasNode;

    fn file_node(id: &str, x: f32, y: f32, w: f32, h: f32) -> CanvasNode {
        CanvasNode {
            id: id.to_owned(),
            kind: "file".to_owned(),
            text: None,
            file: Some(format!("{id}.png")),
            x,
            y,
            width: w,
            height: h,
            color: None,
            extra: Default::default(),
        }
    }

    #[test]
    fn a_cluster_is_summarized_by_its_main_card() {
        // Chain a -> b -> c in one cluster; the root `a` names it.
        let nodes = vec![
            text_node("a", 0.0, 0.0, 100.0, 100.0),
            text_node("b", 150.0, 0.0, 100.0, 100.0),
            text_node("c", 300.0, 0.0, 100.0, 100.0),
        ];
        let edges = vec![edge("a", "b"), edge("b", "c")];
        let clustering = compute(&nodes, &edges, &Theme::default());
        assert_eq!(clustering.clusters.len(), 1);
        assert_eq!(clustering.clusters[0].text, "node a");
    }

    #[test]
    fn image_cluster_carries_the_image_not_text() {
        let nodes = vec![file_node("img", 0.0, 0.0, 400.0, 300.0)];
        let clustering = compute(&nodes, &[], &Theme::default());
        assert_eq!(clustering.clusters.len(), 1);
        assert_eq!(clustering.clusters[0].image_id.as_deref(), Some("img"));
        assert!(clustering.clusters[0].text.is_empty());
    }

    #[test]
    fn text_cluster_gets_a_positive_font_size() {
        let nodes = vec![text_node("a", 0.0, 0.0, 300.0, 120.0)];
        let clustering = compute(&nodes, &[], &Theme::default());
        assert!(clustering.clusters[0].font_size > 0.0);
    }

    #[test]
    fn a_bigger_card_fits_a_bigger_font() {
        // The font is fitted to the box, so the same text in a larger cluster
        // renders larger — the point of the fit.
        let small = compute(
            &[text_node("a", 0.0, 0.0, 200.0, 100.0)],
            &[],
            &Theme::default(),
        );
        let large = compute(
            &[text_node("a", 0.0, 0.0, 800.0, 400.0)],
            &[],
            &Theme::default(),
        );
        assert!(large.clusters[0].font_size > small.clusters[0].font_size);
    }

    /// The plain binary search the seeded fit replaced: the reference answer.
    /// Under the same rule — no word broken — checked word by word rather than
    /// through the cap, so it doesn't just repeat the arithmetic it checks.
    fn exhaustive_fit(text: &str, bounds: Rectangle, theme: &Theme) -> f32 {
        let avail_w = (bounds.width - PADDING * 2.0).max(20.0);
        let avail_h = (bounds.height - PADDING * 2.0).max(20.0);
        let column = avail_w - 2.0 * text_raster::MD_PADDING;
        let unbroken = |size: f32| text_raster::widest_word(text, size, theme) <= column;
        let mut lo = 8.0;
        let mut hi = avail_h;
        for _ in 0..22 {
            let mid = (lo + hi) / 2.0;
            let (w, h) = text_raster::measure_markdown(text, avail_w, mid, theme);
            if w <= avail_w && h <= avail_h && unbroken(mid) {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        lo
    }

    #[test]
    fn the_seeded_fit_matches_an_exhaustive_search() {
        // Speeding the search up must not change the sizes it lands on.
        let theme = Theme::default();
        let long = "word ".repeat(200);
        let texts = [
            "short",
            "A single line of ordinary prose that will wrap once or twice.",
            "# Heading\n\nBody text under it, with **bold** and a list:\n\n- one\n- two\n- three",
            long.as_str(),
        ];
        // Boxes wide enough for "fits" to be monotonic in size, which is what
        // both searches assume. A cluster card is at least a member node wide,
        // so these are the shapes that actually occur.
        let boxes = [
            (300.0, 200.0),
            (400.0, 300.0),
            (900.0, 400.0),
            (1600.0, 1200.0),
        ];
        for text in texts {
            for (w, h) in boxes {
                let bounds = Rectangle::new(Point::new(0.0, 0.0), Size::new(w, h));
                let seeded = fit_font_size(text, bounds, &theme);
                let reference = exhaustive_fit(text, bounds, &theme);
                assert!(
                    (seeded - reference).abs() <= 1.0,
                    "{w}x{h} {:?}: seeded {seeded}, exhaustive {reference}",
                    &text[..text.len().min(20)]
                );
            }
        }
    }

    #[test]
    fn a_narrow_cluster_never_breaks_a_word() {
        // A tall, narrow card: by height alone the type grew until words split
        // mid-letter — "Bud / get: ~€9 / 00".
        let theme = Theme::default();
        let bounds = Rectangle::new(Point::new(0.0, 0.0), Size::new(330.0, 640.0));
        for text in ["Budget: ~€900", "Retire the old dashboard", "# Trip to Lisbon"] {
            let size = fit_font_size(text, bounds, &theme);
            let column = (bounds.width - PADDING * 2.0) - 2.0 * text_raster::MD_PADDING;
            let widest = text_raster::widest_word(text, size, &theme);
            assert!(widest <= column + 0.5, "{text:?} at {size}: a {widest}px word in a {column}px column");
        }
    }

    #[test]
    fn a_fitted_size_fits_unless_even_the_floor_overflows() {
        // The property that matters: the size handed back renders inside the
        // card. The exception is a box too small for the text at any legible
        // size, where the floor is returned and the draw clips it — long-
        // standing behaviour, not something this search introduced.
        let theme = Theme::default();
        let text = "# Ideas\n\nSome **bold** text and a list:\n\n- one\n- two";
        for (w, h) in [
            (200.0, 120.0),
            (300.0, 200.0),
            (400.0, 300.0),
            (900.0, 400.0),
        ] {
            let bounds = Rectangle::new(Point::new(0.0, 0.0), Size::new(w, h));
            let size = fit_font_size(text, bounds, &theme);
            let avail_w = (w - PADDING * 2.0).max(20.0);
            let avail_h = (h - PADDING * 2.0).max(20.0);
            let (tw, th) = text_raster::measure_markdown(text, avail_w, size, &theme);

            let fits = tw <= avail_w + 0.5 && th <= avail_h + 0.5;
            assert!(
                fits || size == MIN_FIT,
                "{w}x{h}: fitted {size} renders {tw}x{th}, over {avail_w}x{avail_h}, \
                 and isn't the floor"
            );
        }
    }

    #[test]
    fn the_card_box_is_the_members_union_plus_a_margin() {
        // Two cards 50 apart join; the card spans both, expanded by MARGIN.
        let nodes = vec![
            text_node("a", 0.0, 0.0, 100.0, 100.0),
            text_node("b", 150.0, 0.0, 100.0, 100.0),
        ];
        let clustering = compute(&nodes, &[], &Theme::default());
        let margin = thinkboard_core::cluster::MARGIN;
        let bounds = clustering.clusters[0].bounds;
        assert_eq!(bounds.x, -margin);
        assert_eq!(bounds.width, 250.0 + margin * 2.0);
    }
}
