//! Benchmarks for the CPU work behind a frame.
//!
//! The GPU draw itself can't be measured here — a `canvas::Frame` needs a live
//! wgpu renderer, which a headless bench has no way to build. What *can* be
//! measured is everything that feeds it, and that is where the time actually
//! goes: shaping and measuring Markdown, rasterizing a node's text to a bitmap,
//! fitting cluster labels, and the per-edge curve sampling behind link hover.
//!
//! Several of these sit behind caches, so each is measured **cold and warm**.
//! Warm is the steady state while panning a settled board; cold is what a fresh
//! board, a zoom that leaves the quality band, or a recluster pays. Reporting
//! only the warm number would be reporting the cost of a hash lookup.
//!
//! Run with `cargo bench`, or one group with `cargo bench -- rasterize`.

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use iced::Point;
use serde_json::Map;
use thinkboard::text_raster::{self, card_text_wrap};
use thinkboard::{Theme, cluster};
use thinkboard_core::model::{CanvasEdge, CanvasNode};

/// The board's own body size, so these numbers match what the app pays.
const FONT_SIZE: f32 = 17.0;
const NODE_WIDTH: f32 = 250.0;
const NODE_HEIGHT: f32 = 120.0;

/// Text of the shape a note actually holds. `plain` is the common case; `rich`
/// exercises the styled-span path (headings, bold, links, lists) that costs
/// noticeably more to shape.
const PLAIN: &str = "A short note of the kind most cards hold, long enough to wrap onto \
                     two or three lines at the usual card width.";
const RICH: &str = "## Project ideas\n\nSome **bold** body text, a bit of `code`, and a \
                    [link](https://example.com).\n\n- offline sync\n- full-text search\n\
                    - conflict resolution";

fn node(id: usize, x: f32, y: f32, text: &str) -> CanvasNode {
    CanvasNode {
        id: format!("n{id}"),
        kind: "text".to_owned(),
        text: Some(text.to_owned()),
        file: None,
        x,
        y,
        width: NODE_WIDTH,
        height: NODE_HEIGHT,
        color: None,
        extra: Map::new(),
    }
}

/// Clumps of five, spread far enough apart to cluster separately — the same
/// shape as the core clustering bench, so the two are comparable.
///
/// Every node's text is made distinct. That matters more than it looks: the
/// measurement cache is keyed by the text, so a board of repeated strings lets
/// every cluster's font fitting reuse another's entries and reports a recluster
/// as several times cheaper than it is on a real board.
fn board(count: usize) -> Vec<CanvasNode> {
    (0..count)
        .map(|i| {
            let (clump, within) = (i / 5, i % 5);
            let base = if i % 3 == 0 { RICH } else { PLAIN };
            node(
                i,
                (clump % 6) as f32 * 1200.0 + (within % 3) as f32 * 300.0,
                (clump / 6) as f32 * 1200.0 + (within / 3) as f32 * 200.0,
                &format!("{base}\n\nNote {i}."),
            )
        })
        .collect()
}

/// Measuring rendered Markdown — the auto-size path. This runs on **every
/// keystroke** while typing (via `App::autosize_node`), so the warm number is
/// the one that has to stay small; the cold number is what a card pays the
/// first time it is sized.
fn measure(c: &mut Criterion) {
    let theme = Theme::default();
    let wrap = card_text_wrap(NODE_WIDTH);

    let mut group = c.benchmark_group("measure");
    for (name, text) in [("plain", PLAIN), ("rich", RICH)] {
        group.bench_function(BenchmarkId::new("warm", name), |b| {
            // Prime the cache, then measure repeated hits.
            text_raster::measure_markdown(text, wrap, FONT_SIZE, &theme);
            b.iter(|| {
                text_raster::measure_markdown(black_box(text), wrap, FONT_SIZE, &theme)
            })
        });
        group.bench_function(BenchmarkId::new("cold", name), |b| {
            b.iter_batched(
                text_raster::clear_caches,
                |()| text_raster::measure_markdown(black_box(text), wrap, FONT_SIZE, &theme),
                BatchSize::PerIteration,
            )
        });
    }
    // The editor's own measurement, run on the same keystroke path.
    group.bench_function("source_card_cold", |b| {
        b.iter_batched(
            text_raster::clear_caches,
            |()| text_raster::measure_source_card(black_box(RICH), wrap, FONT_SIZE, &theme),
            BatchSize::PerIteration,
        )
    });
    group.finish();
}

/// Rasterizing a node's text to an RGBA bitmap — the "draw a node" cost, and
/// the single most expensive thing on the board. Never cached: every visible
/// text node pays this again whenever a settled zoom leaves its quality band.
/// Measured across zoom levels because the bitmap is sized in device pixels, so
/// cost grows with the square of the scale.
fn rasterize(c: &mut Criterion) {
    let theme = Theme::default();
    let wrap = card_text_wrap(NODE_WIDTH);

    let mut group = c.benchmark_group("rasterize");
    for zoom in [0.5_f32, 1.0, 2.0] {
        for (name, text) in [("plain", PLAIN), ("rich", RICH)] {
            let id = BenchmarkId::new(name, format!("{zoom}x"));
            group.bench_function(id, |b| {
                b.iter(|| {
                    text_raster::snapshot_markdown(
                        black_box(text),
                        NODE_WIDTH,
                        NODE_HEIGHT,
                        wrap,
                        FONT_SIZE,
                        theme,
                        false,
                        zoom,
                    )
                })
            });
        }
    }
    // A large card at retina scale: the worst case a single node can present.
    group.bench_function("large_card_2x", |b| {
        b.iter(|| {
            text_raster::snapshot_markdown(
                black_box(RICH),
                800.0,
                600.0,
                card_text_wrap(800.0),
                FONT_SIZE,
                theme,
                false,
                2.0,
            )
        })
    });
    group.finish();
}

/// The live-text path: laying Markdown out into positioned runs, which the
/// board then fills one `fill_text` per run. This is what draws a card while
/// it is being edited, scrolled, or matched by the find bar — i.e. whenever the
/// bitmap can't be used. Cached, so warm is the per-frame steady state.
fn live_layout(c: &mut Criterion) {
    let theme = Theme::default();
    let wrap = card_text_wrap(NODE_WIDTH);

    let mut group = c.benchmark_group("live_layout");
    group.bench_function("warm", |b| {
        text_raster::with_markdown_layout(RICH, NODE_WIDTH, NODE_HEIGHT, wrap, FONT_SIZE, theme, false, |runs| runs.len());
        b.iter(|| {
            text_raster::with_markdown_layout(
                black_box(RICH),
                NODE_WIDTH,
                NODE_HEIGHT,
                wrap,
                FONT_SIZE,
                theme,
                false,
                |runs| runs.len(),
            )
        })
    });
    group.bench_function("cold", |b| {
        b.iter_batched(
            text_raster::clear_caches,
            |()| {
                text_raster::with_markdown_layout(
                    black_box(RICH),
                    NODE_WIDTH,
                    NODE_HEIGHT,
                    wrap,
                    FONT_SIZE,
                    theme,
                    false,
                    |runs| runs.len(),
                )
            },
            BatchSize::PerIteration,
        )
    });
    group.finish();
}

/// A full recluster: spatial grouping plus fitting a font size to every cluster
/// card. The fitting binary-searches ~22 sizes per cluster and each is a
/// distinct cache key, so a cold recluster is dominated by text measurement
/// rather than by the grouping the core bench measures. This is the number to
/// watch — it runs on a background task after every settled edit.
fn recluster(c: &mut Criterion) {
    let theme = Theme::default();

    let mut group = c.benchmark_group("recluster");
    group.sample_size(20);
    for size in [25, 100, 300] {
        let nodes = board(size);
        let edges: Vec<CanvasEdge> = Vec::new();
        group.throughput(Throughput::Elements(size as u64));
        group.bench_with_input(BenchmarkId::new("cold", size), &size, |b, _| {
            b.iter_batched(
                text_raster::clear_caches,
                |()| cluster::compute(black_box(&nodes), black_box(&edges), &theme),
                BatchSize::PerIteration,
            )
        });
        group.bench_with_input(BenchmarkId::new("warm", size), &size, |b, _| {
            cluster::compute(&nodes, &edges, &theme);
            b.iter(|| cluster::compute(black_box(&nodes), black_box(&edges), &theme))
        });
    }
    group.finish();
}

/// Link hover: the board samples every edge's curve to find the one under the
/// cursor, on every mouse move. Cost is linear in the edge count, so this is
/// measured as a whole scan rather than per curve.
fn link_hover(c: &mut Criterion) {
    use thinkboard::geometry::{EdgeGeometry, bezier_distance};

    let curves: Vec<EdgeGeometry> = (0..800)
        .map(|i| {
            let y = i as f32 * 7.0;
            EdgeGeometry {
                start: Point::new(0.0, y),
                control1: Point::new(120.0, y + 40.0),
                control2: Point::new(280.0, y + 40.0),
                end: Point::new(400.0, y),
                alpha: 1.0,
            }
        })
        .collect();
    let cursor = Point::new(200.0, 1400.0);

    let mut group = c.benchmark_group("link_hover");
    group.bench_function("single_curve", |b| {
        b.iter(|| bezier_distance(black_box(cursor), black_box(&curves[0])))
    });
    for edges in [50, 200, 800] {
        let slice = &curves[..edges];
        group.throughput(Throughput::Elements(edges as u64));
        group.bench_with_input(BenchmarkId::new("scan", edges), &edges, |b, _| {
            b.iter(|| {
                slice
                    .iter()
                    .map(|g| bezier_distance(black_box(cursor), g))
                    .fold(f32::MAX, f32::min)
            })
        });
    }
    group.finish();
}

criterion_group!(benches, measure, rasterize, live_layout, recluster, link_hover);
criterion_main!(benches);
