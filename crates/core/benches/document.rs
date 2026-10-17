//! Benchmarks for the UI-agnostic document algorithms.
//!
//! These are the ones whose cost scales with board size, so each is measured at
//! a few node counts rather than one: the interesting number is not "how long
//! for my board" but "what happens when the board is ten times bigger". A
//! linear-looking line here is the pass; a curve is the finding.
//!
//! Run with `cargo bench -p thinkboard-core`, or one group with
//! `cargo bench -p thinkboard-core -- clustering`.

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use serde_json::Map;
use thinkboard_core::model::{CanvasDocument, CanvasEdge, CanvasNode};
use thinkboard_core::{cluster, layout, markdown, model, search, syntax};

/// Board sizes to measure at. 800 is well past a comfortable hand-made board;
/// it's here to expose super-linear behaviour, not because anyone has one.
const SIZES: [usize; 4] = [25, 100, 300, 800];

/// A node with a realistic amount of text.
fn node(id: usize, x: f32, y: f32) -> CanvasNode {
    CanvasNode {
        id: format!("n{id}"),
        kind: "text".to_owned(),
        text: Some(format!(
            "## Note {id}\n\nA line of **body** text with a [link](https://example.com) \
             and enough words to wrap across a couple of lines."
        )),
        file: None,
        x,
        y,
        width: 250.0,
        height: 120.0,
        color: None,
        extra: Map::new(),
    }
}

fn edge(from: usize, to: usize) -> CanvasEdge {
    CanvasEdge {
        id: format!("e{from}-{to}"),
        from_node: format!("n{from}"),
        from_side: "right".to_owned(),
        to_node: format!("n{to}"),
        to_side: "left".to_owned(),
        extra: Map::new(),
    }
}

/// A board laid out as a loose grid of clumps: nodes within a clump are close
/// enough to cluster, clumps are far enough apart not to. That gives the
/// clustering pass a realistic mix rather than one giant blob or `n` singletons.
fn board(count: usize) -> (Vec<CanvasNode>, Vec<CanvasEdge>) {
    const PER_CLUMP: usize = 5;
    let mut nodes = Vec::with_capacity(count);
    let mut edges = Vec::new();
    for i in 0..count {
        let clump = i / PER_CLUMP;
        let within = i % PER_CLUMP;
        // Clumps on a 6-wide grid, 1200 apart; members 300 apart inside one.
        let x = (clump % 6) as f32 * 1200.0 + (within % 3) as f32 * 300.0;
        let y = (clump / 6) as f32 * 1200.0 + (within / 3) as f32 * 200.0;
        nodes.push(node(i, x, y));
        // Chain each clump together, so edges are intra-cluster like a real map.
        if within > 0 {
            edges.push(edge(i - 1, i));
        }
    }
    (nodes, edges)
}

/// Spatial clustering — proximity union-find plus the containment pass, which
/// repeats until stable and is the part most likely to bite at scale.
fn clustering(c: &mut Criterion) {
    let mut group = c.benchmark_group("clustering");
    for size in SIZES {
        let (nodes, edges) = board(size);
        group.throughput(Throughput::Elements(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, _| {
            b.iter(|| cluster::group(black_box(&nodes), black_box(&edges)))
        });
    }
    group.finish();
}

/// A pathological shape for the containment pass: one long diagonal chain, so
/// every clump's bounding box overlaps the next and absorption keeps finding
/// more to merge. Worth watching separately from the grid case.
fn clustering_diagonal(c: &mut Criterion) {
    let mut group = c.benchmark_group("clustering_diagonal");
    for size in [25, 100, 300] {
        let nodes: Vec<CanvasNode> = (0..size)
            .map(|i| node(i, i as f32 * 80.0, i as f32 * 80.0))
            .collect();
        group.throughput(Throughput::Elements(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, _| {
            b.iter(|| cluster::group(black_box(&nodes), black_box(&[])))
        });
    }
    group.finish();
}

/// The layered ("Distribute") auto-layout: longest-path ranking plus barycenter
/// crossing reduction. Runs on every auto-layout and on every AI relayout tick.
fn layered_layout(c: &mut Criterion) {
    let mut group = c.benchmark_group("layered_layout");
    for size in SIZES {
        let (nodes, edges) = board(size);
        let selected: Vec<usize> = (0..size).collect();
        group.throughput(Throughput::Elements(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, _| {
            b.iter(|| {
                layout::layered_layout(
                    black_box(&nodes),
                    black_box(&edges),
                    black_box(&selected),
                    layout::Direction::Right,
                    120.0,
                    40.0,
                )
            })
        });
    }
    group.finish();
}

/// Loading and saving a canvas. Both run on the main thread — a load blocks the
/// summon, and a save runs inside the autosave debounce.
fn document_io(c: &mut Criterion) {
    let mut group = c.benchmark_group("document_io");
    for size in SIZES {
        let (nodes, edges) = board(size);
        let document = CanvasDocument {
            nodes,
            edges,
            extra: Map::new(),
        };
        let json = document.to_canvas_json().expect("serializes");
        group.throughput(Throughput::Bytes(json.len() as u64));

        group.bench_with_input(BenchmarkId::new("parse", size), &size, |b, _| {
            b.iter(|| CanvasDocument::from_json(black_box(&json)).expect("parses"))
        });
        group.bench_with_input(BenchmarkId::new("serialize", size), &size, |b, _| {
            b.iter(|| black_box(&document).to_canvas_json().expect("serializes"))
        });
    }
    group.finish();
}

/// Markdown parsing, run for every card that draws live text (and inside every
/// text measurement, so it sits under the auto-size path too).
fn markdown_parse(c: &mut Criterion) {
    let short = "A plain note with no formatting at all.";
    let rich = "# Heading\n\nSome **bold** and *italic* text, `inline code`, and a \
                [link](https://example.com).\n\n- first item\n- second item\n- third item\n\n\
                ```rust\nfn main() { println!(\"hi\"); }\n```\n";

    let mut group = c.benchmark_group("markdown");
    group.bench_function("parse_plain", |b| {
        b.iter(|| markdown::parse(black_box(short)))
    });
    group.bench_function("parse_rich", |b| b.iter(|| markdown::parse(black_box(rich))));
    // `is_simple` decides centred vs left alignment on every draw and every
    // editor layout, so it wants to stay much cheaper than a full parse.
    group.bench_function("is_simple_plain", |b| {
        b.iter(|| markdown::is_simple(black_box(short)))
    });
    group.bench_function("is_simple_rich", |b| {
        b.iter(|| markdown::is_simple(black_box(rich)))
    });
    group.finish();
}

/// Code-block syntax highlighting, run per fenced block on every live draw.
fn syntax_highlight(c: &mut Criterion) {
    let code = "fn main() {\n    let x = 42; // the answer\n    \
                println!(\"{}\", x);\n}\n";
    c.bench_function("syntax_highlight_rust", |b| {
        b.iter(|| syntax::highlight(black_box(code), black_box("rust")))
    });
}

/// Walking a card's ancestors to build an AI prompt. Runs once per Ask-AI, but
/// over the whole edge list per step, so it's worth knowing its shape.
fn upstream(c: &mut Criterion) {
    let mut group = c.benchmark_group("upstream_prompt");
    for size in SIZES {
        let (nodes, edges) = board(size);
        let target = format!("n{}", size - 1);
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, _| {
            b.iter(|| {
                thinkboard_core::ai::upstream_prompt(
                    black_box(&nodes),
                    black_box(&edges),
                    black_box(&target),
                )
            })
        });
    }
    group.finish();
}

/// Search-as-you-type: the whole board is rescanned on every keystroke in the
/// find bar, on the main thread. Measured on a hit-free query (the common case
/// while typing a word out) and an all-ASCII board, which is the fast path.
fn search(c: &mut Criterion) {
    let mut group = c.benchmark_group("find_matches");
    for size in SIZES {
        let (nodes, _) = board(size);
        group.throughput(Throughput::Elements(size as u64));
        group.bench_with_input(BenchmarkId::new("miss", size), &size, |b, _| {
            b.iter(|| search::find_matches(black_box(&nodes), black_box("zzzz")))
        });
        group.bench_with_input(BenchmarkId::new("hit", size), &size, |b, _| {
            b.iter(|| search::find_matches(black_box(&nodes), black_box("note")))
        });
    }
    // Non-ASCII forces the folded-char path for every node.
    let (nodes, _) = board(300);
    group.bench_function("unicode_query/300", |b| {
        b.iter(|| search::find_matches(black_box(&nodes), black_box("nöte")))
    });
    group.finish();
}

/// Id generation, called for every node, edge and paste.
fn ids(c: &mut Criterion) {
    c.bench_function("random_id", |b| b.iter(model::random_id));
}

criterion_group!(
    benches,
    clustering,
    clustering_diagonal,
    layered_layout,
    document_io,
    markdown_parse,
    syntax_highlight,
    upstream,
    search,
    ids,
);
criterion_main!(benches);
