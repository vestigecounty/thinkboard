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

/// Text of the shape a card actually holds. `plain` is the common case; `rich`
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
/// Card sizes to sweep. Cost turns on the raster scale, which is derived from
/// the node's own dimensions — so a size sweep is the point, not a detail: the
/// same zoom can land these on very different scales.
const CARD_SIZES: [(&str, f32, f32); 3] = [
    ("small", 250.0, 120.0),
    ("medium", 400.0, 300.0),
    ("large", 800.0, 600.0),
];

/// Zooms to sweep, spanning the configured range (`min_zoom` 0.1 …
/// `max_zoom` 6.0) rather than just the comfortable middle.
const ZOOMS: [f32; 4] = [0.5, 1.0, 2.0, 4.0];

fn rasterize(c: &mut Criterion) {
    let theme = Theme::default();

    let mut group = c.benchmark_group("rasterize");
    for (size_name, width, height) in CARD_SIZES {
        let wrap = card_text_wrap(width);
        for zoom in ZOOMS {
            // The id must stay stable across a change to the scale policy —
            // putting the resolution in it would break baseline comparison at
            // exactly the moment the resolution is what changed. The scale each
            // case resolves to is printed by the `raster_scale` group instead.
            let id = BenchmarkId::new(size_name, format!("{zoom}x"));
            group.bench_function(id, |b| {
                b.iter(|| {
                    text_raster::snapshot_markdown(
                        black_box(RICH),
                        width,
                        height,
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
    group.finish();
}

/// The raster scale itself, independent of any rasterization: what resolution
/// each card size gets at each zoom. Not a timing so much as a table — the
/// assertions live in `text_raster`'s own tests; this makes the numbers visible
/// alongside the cost they produce.
fn raster_scale(c: &mut Criterion) {
    let mut group = c.benchmark_group("raster_scale");
    for (size_name, width, height) in CARD_SIZES {
        group.bench_function(BenchmarkId::from_parameter(size_name), |b| {
            b.iter(|| {
                ZOOMS.map(|zoom| text_raster::scale_for(black_box(zoom), width, height))
            })
        });
    }
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
    use thinkboard::geometry::{EdgeGeometry, bezier_distance, within_reach};

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

    // What `Board::edge_at` actually does per mouse move: reject on the hull
    // box, then sample only what survives.
    let threshold = 6.0_f32;
    let scan = |slice: &[EdgeGeometry], point| {
        slice
            .iter()
            .filter(|g| within_reach(point, g, threshold))
            .map(|g| bezier_distance(point, g))
            .fold(f32::MAX, f32::min)
    };

    let mut group = c.benchmark_group("link_hover");
    group.bench_function("single_curve", |b| {
        b.iter(|| bezier_distance(black_box(cursor), black_box(&curves[0])))
    });
    for edges in [50, 200, 800] {
        let slice = &curves[..edges];
        group.throughput(Throughput::Elements(edges as u64));
        group.bench_with_input(BenchmarkId::new("scan", edges), &edges, |b, _| {
            b.iter(|| scan(slice, black_box(cursor)))
        });
    }
    // The cursor sitting *on* a link, so the broad phase can't reject
    // everything: the realistic worst case rather than the best.
    group.bench_function("scan_on_a_link/800", |b| {
        let on_link = curves[400].start;
        b.iter(|| scan(&curves, black_box(on_link)))
    });
    group.finish();
}

/// Stacking overlapping cards into planes: once per rebuild of the scene for
/// [`stacking_planes`], and every frame for [`covers_any`] while something is
/// selected. Both sweep left edges rather than comparing every pair, so a board
/// laid out side by side — the common one — should cost a sort.
fn stacking(c: &mut Criterion) {
    use iced::{Rectangle, Size};
    use thinkboard::geometry::{covers_any, stacking_planes};

    // A grid of cards, every tenth pushed onto its neighbour: a few real
    // overlaps among many that aren't.
    let cards: Vec<Rectangle> = (0..800)
        .map(|i| {
            let (col, row) = ((i % 40) as f32, (i / 40) as f32);
            let nudge = if i % 10 == 0 { (160.0, 40.0) } else { (0.0, 0.0) };
            Rectangle::new(Point::new(col * 280.0 + nudge.0, row * 140.0 + nudge.1), Size::new(250.0, 90.0))
        })
        .collect();

    let mut group = c.benchmark_group("stacking");
    for count in [100, 800] {
        let slice = &cards[..count];
        group.throughput(Throughput::Elements(count as u64));
        group.bench_with_input(BenchmarkId::new("planes", count), &count, |b, _| {
            b.iter(|| stacking_planes(black_box(slice), 3))
        });
        // One card selected: the overlay asking whether it covers any other.
        let (selected, rest) = slice.split_at(1);
        group.bench_with_input(BenchmarkId::new("covers_any", count), &count, |b, _| {
            b.iter(|| covers_any(black_box(selected), black_box(rest)))
        });
    }
    group.finish();
}

/// What the main thread actually pays when background text work is running.
///
/// Measurement and rasterization share one global mutex (`with_raster`), so a
/// keystroke's auto-size can queue behind background work. The question this
/// answers is *how long* — the lock is taken per call, not held across a batch,
/// so the exposure should be one in-flight operation rather than the whole
/// recluster or raster sweep. If that is right, the contended number sits about
/// one rasterization above the quiet one, and splitting the mutex would buy
/// very little.
fn contention(c: &mut Criterion) {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let theme = Theme::default();
    let wrap = card_text_wrap(NODE_WIDTH);

    let mut group = c.benchmark_group("contention");
    group.sample_size(30);

    // A keystroke's auto-size with nothing else running.
    group.bench_function("autosize_quiet", |b| {
        b.iter_batched(
            text_raster::clear_caches,
            |()| text_raster::measure_source_card(black_box(RICH), wrap, FONT_SIZE, &theme),
            BatchSize::PerIteration,
        )
    });

    // The same, while a background thread rasterizes cards back to back —
    // what a zoom-settle or a recluster does.
    let stop = Arc::new(AtomicBool::new(false));
    let worker = {
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            let theme = Theme::default();
            let wrap = card_text_wrap(NODE_WIDTH);
            let mut n = 0usize;
            while !stop.load(Ordering::Relaxed) {
                // Vary the text so it never short-circuits on a cache hit.
                let text = format!("{RICH}\n\nbackground {n}");
                text_raster::snapshot_markdown(
                    &text, NODE_WIDTH, NODE_HEIGHT, wrap, FONT_SIZE, theme, false, 1.0,
                );
                n += 1;
            }
        })
    };
    group.bench_function("autosize_under_load", |b| {
        b.iter_batched(
            text_raster::clear_caches,
            |()| text_raster::measure_source_card(black_box(RICH), wrap, FONT_SIZE, &theme),
            BatchSize::PerIteration,
        )
    });
    stop.store(true, Ordering::Relaxed);
    worker.join().expect("background rasterizer");
    group.finish();
}

/// The three candidates that were still guesses: a per-frame index rebuild, the
/// allocation inside every measurement-cache lookup, and search-as-you-type.
/// Measured so they can be ranked against the paths above rather than assumed.
fn per_frame_overheads(c: &mut Criterion) {
    use std::collections::HashMap;

    let theme = Theme::default();
    let wrap = card_text_wrap(NODE_WIDTH);

    let mut group = c.benchmark_group("per_frame");
    for size in [100, 800] {
        let nodes = board(size);

        // `view()` rebuilds this id -> index map on every frame.
        group.throughput(Throughput::Elements(size as u64));
        group.bench_with_input(BenchmarkId::new("node_index_rebuild", size), &size, |b, _| {
            b.iter(|| {
                black_box(&nodes)
                    .iter()
                    .enumerate()
                    .map(|(index, node)| (node.id.as_str(), index))
                    .collect::<HashMap<&str, usize>>()
            })
        });

    }
    // A warm measurement is dominated by building its cache key, which clones
    // the node's text on every call — including the hits.
    group.bench_function("measure_cache_key", |b| {
        text_raster::measure_markdown(RICH, wrap, FONT_SIZE, &theme);
        b.iter(|| text_raster::measure_markdown(black_box(RICH), wrap, FONT_SIZE, &theme))
    });
    group.bench_function("measure_cache_key_alloc_only", |b| {
        b.iter(|| black_box(RICH).trim().to_owned())
    });
    group.finish();
}

criterion_group!(
    benches,
    measure,
    rasterize,
    raster_scale,
    live_layout,
    recluster,
    link_hover,
    stacking,
    contention,
    per_frame_overheads,
);
criterion_main!(benches);
