# ThinkBoard — working notes

A Rust/iced desktop overlay for editing Obsidian `.canvas` files, summoned over
the desktop with a global hotkey. There's also an early SwiftUI iOS front-end
driving the same core through UniFFI.

## Commands

```sh
cargo build --locked --workspace --all-targets
cargo clippy --locked --workspace --all-targets
cargo test --locked --workspace
cargo run                       # overlay mode; Pause (default) summons/dismisses
cargo run -- --windowed --config /tmp/x.json   # a normal window + a throwaway config
```

**`--workspace` is required, not cosmetic.** The root `Cargo.toml` is both a
`[package]` and the `[workspace]` root, so a bare `cargo test` covers only the
`thinkboard` package and silently skips `thinkboard-core` and `thinkboard-ffi`.
CI passes it; so should you.

`--config <path>` points the app at a throwaway config instead of the user's
real one — use it for any run that might write settings.

## Conventions that look like mistakes but aren't

- **Do not run `cargo fmt`.** The code is hand-formatted: single-line struct
  literals in tables (e.g. `ai::PRESETS`), deliberate line breaks that group
  related arguments. `cargo fmt --check` reports diffs against most files by
  design. Match the surrounding style instead.
- **Clippy runs without `-D warnings`.** A handful of style lints are
  intentional (index loops in union-find, `&PathBuf` in a signature that mirrors
  a callback, items after a test module). The job still fails on genuine clippy
  *errors*. Don't "fix" the standing warnings; don't add new ones either.
- **Comments carry the reasoning.** Most non-obvious lines have a comment
  explaining *why*, often referencing a platform bug or a rendering artifact.
  Preserve them verbatim when moving code, and write new ones in the same voice.
- **`vendor/` holds patched iced crates**, wired up by `[patch.crates-io]`:
  - `iced_graphics` — center-aligned text editing (`set_editor_centered`), and
    `editor_scroll_fraction`, which publishes how far the editor has scrolled so
    the app can draw a scrollbar for it. Both are globals, which is sound only
    because one node is edited at a time.
  - `iced_wgpu` — honors wgpu backend env vars so
    `WGPU_DX12_PRESENTATION_SYSTEM=visual` can pick the DirectComposition
    swapchain (transparent overlay on Windows).

  They're checked in, so a clone needs no extra setup. Re-vendoring means
  re-applying these patches.

## Layout

```
src/            the desktop app (iced, wgpu, winit)
  app/          state, the update loop, and one module per feature (below)
  canvas/       the board canvas widget, text rasterizer, camera, shaders
  document/     desktop-side document glue (cluster cards, colour bridging)
  platform/     per-OS window/gesture/tray shims
crates/core/    thinkboard-core — UI-agnostic document logic
crates/ffi/     thinkboard-ffi — UniFFI bindings over the core, for iOS
apps/ios/       SwiftUI front-end (XcodeGen; project file is not checked in)
```

**`crates/core` must stay free of iced/wgpu/winit and any GUI toolkit.** It's
what the iOS app links. The convention when a computation needs geometry: core
takes and returns bare tuples (`(f32, f32)`, `(x, y, w, h)`), and the desktop
wraps them in `iced::Point`/`Rectangle` at the call site — see `core::snap`,
`core::layout`, and the `core::cluster` / `document::cluster` pair, where the
pure grouping lives in core and only the font-fitting (which needs the text
rasterizer) stays desktop-side.

`src/main.rs` re-exports flat aliases (`crate::model`, `crate::text_raster`,
`crate::ai`, …) so leaf modules keep short paths.

### `src/app/`

`mod.rs` is the spine: the `App` state, the `Message` enum, and an
`update`/`subscription` loop that mostly delegates. Each feature owns its
`impl App` block in its own module:

| module | owns |
| --- | --- |
| `arrange` | node auto-sizing, free-spot placement, layered auto-layout |
| `edit` | the in-place editor, its text-level undo, Markdown marker toggles |
| `view` | everything drawn — board scene, menus, find bar, settings modal |
| `history` | document undo/redo snapshots and the deletions that record them |
| `raster` | text bitmaps and semantic-zoom clusters (both computed off-thread) |
| `ai` / `ghosts` | streamed answers; Copilot-style suggestion cards |
| `canvas_file` | opening, autosaving, reloading the `.canvas` file; vault roots |
| `search`, `clipboard`, `command`, `settings`, `startup`, `tray`, `sync` | as named |

Related fields are grouped into sub-structs (`Ai`, `Ghosts`, `Rasters`,
`Clusters`) rather than living loose on `App`. When adding a feature, add its
module and its `impl App` block there — keep `update` a dispatch table.

## Card sizing

Auto-size **grows a card, never narrows it**. A width is something the user can
drag to deliberately, and re-deriving it from the text on the next keystroke
threw that away. `max_node_width` bounds how wide a card grows *by itself*; it
is not a limit on one sized by hand. Height still tracks content, so text
neither clips nor leaves a card standing empty.

Height stops at `text_raster::MAX_AUTO_HEIGHT` (`MAX_DIMENSION / 2`). A card's
bitmap is capped at 2048 px on its longest side, so a taller card loses
resolution in proportion — retina at 1024, 1:1 at 2048, and below 1:1 past that,
where the bitmap holds less detail than the card is. Rather than let a long note
grow until it visibly blurs, growth stops and the overflow scrolls;
`Board::scrollable` forces the scrollbar on for such a card whatever
`node_scrollbars` says, or the rest of the note would be unreachable.

The editing overlay is bounded to its card for the same reason: sized to its
content it spilled over the board above and below. Bounded, iced scrolls it
internally and keeps the caret in the visible line range. The thumb follows it: iced's
`Content` exposes `cursor()` and `line_count()` but nothing about the scroll
offset, so the vendored `iced_graphics` editor publishes it as a fraction
(`editor_scroll_fraction`, measured in *visual* lines so wrapped paragraphs
count honestly), and `editing_thumb` maps that onto the same overflow the read
path uses — so the thumb doesn't jump when an edit starts.

## Obsidian compatibility

`.canvas` files are Obsidian's, and round-tripping must not damage them:

- `CanvasNode`/`CanvasEdge` carry `#[serde(flatten)] extra`, so fields another
  tool wrote survive a load/save cycle. Same for top-level keys (`doc_extra`).
- `to_canvas_json` mimics Obsidian's own formatting: tab-indented top level,
  each node/edge as one compact line, integer coordinates.
- Edges carry `label`, `color`, `fromEnd` and `toEnd`, all read through
  accessors off `extra` like the node fields. `toEnd` defaults to an arrow and
  `fromEnd` to none — that's how Obsidian omits them for the common case, so an
  absent field is meaningful, not missing data.
- Node `type` is a `String` in the file, but never match on it directly: call
  `CanvasNode::node_kind()`, which resolves the four stored types into what a
  node actually *is* — text, image, note, attachment, link or group. The
  image-vs-Markdown distinction for `file` nodes lives only in that one place
  (it depends on the path's extension, not on `type`).
- Only what the app itself *writes* is a struct field. A group's `label` and a
  link's `url` are read through accessors off `extra`, which already
  round-trips them — so the app renders them without ever having to serialize
  them back.
- **Groups are frames, not cards.** They draw behind everything, are grabbable
  only by their header strip, and are excluded from clustering, marquee
  selection, ports, shadows — and, importantly, from every *obstacle* set
  (placement, auto-size neighbours, AI sibling stacking). A frame encloses
  other nodes, so counting it as occupied space pins the cards inside it.
  Dragging one carries whatever it encloses (`App::drag_riders`), which is also
  where multi-selection drags are resolved — as a set, so nothing that
  qualifies twice moves at double speed. `App::group_selection` creates one
  around two or more selected non-frame nodes (command palette, or the node
  context menu); `groupable_selection` is the single predicate all three call
  sites gate on. A new frame carries no `label`, since there's no rename UI —
  name it, which drops straight into naming it; double-clicking a frame's
  header reopens the caption. A blank caption clears the label rather than
  deleting the frame — unlike a drag-created note, which *is* taken back when
  left empty.
- **The editor edits two things.** `EditTarget::Body` is a text node's text over
  the whole card; `EditTarget::Label` is a frame's caption along its header
  strip. They share the widget, the text-level undo and the key bindings, and
  differ in where the text comes from, where it goes back to, and which
  rectangle counts as "on screen" (`App::edit_rect`) — a frame is routinely
  bigger than the viewport, so fitting all of it to edit a caption would zoom
  the board out for nothing.
- **Ask AI works on a frame**, which has no incoming links to walk. Its contents
  are flattened into an outline instead (`ai::group_prompt`), preserving both
  hierarchies the user sees: links nest a note's targets under it, and a nested
  frame's contents nest under its caption. A node inside nested frames is
  attributed to the innermost one, so nothing is listed twice.
- **Group membership is positional, never stored.** The format has no children
  array and no parent pointer — only geometry — so `enclosed_by` recomputes it
  from centres whenever it matters. Dragging a node out of a frame is therefore
  just moving it, with no state to keep in sync, and adding a note to a group
  is just dropping it inside the bounds. Never invent a membership field: it
  would be ours alone, and Obsidian would ignore it.
- **A frame can be linked like any node**, so a port-drag off a group makes a
  connected note — that's how a question gets asked *about* a group. Its ports
  lose every hit test to the cards it encloses (`Board::frame_last`), since a
  frame's border runs behind its members. A group among a chain's ancestors
  contributes its flattened outline to `upstream_prompt`, and its images to
  `upstream_images`; without that the link would look meaningful on the board
  and feed the model nothing.
- The one place a frame *is* an obstacle is `App::ai_answer_placement`. An
  answer about a frame must land beside it, because inside it would become one
  of its members and be fed back into the next question about that frame.
- **Card alignment follows what the card is.** A note or attachment is a
  document: top-left under a muted file-name header and a hairline rule. A link
  is a single label: host and URL centred in the card. Text nodes centre or
  left-align per `center_node_text`. Keep new kinds consistent with whichever
  of the two they resemble.

## Benchmarks

```sh
cargo bench --workspace              # everything
cargo bench -- rasterize             # one group in the desktop benches
cargo bench -p thinkboard-core       # the pure document algorithms
```

`benches/render.rs` covers the CPU work behind a frame — text measurement,
rasterization, live Markdown layout, reclustering, link hover.
`crates/core/benches/document.rs` covers the pure algorithms — clustering,
layered layout, canvas parse/serialize, Markdown, syntax highlighting.

CI compiles both (`--all-targets` includes benches) but never runs them.

Two things to know before trusting a number:

- **Most text paths are cached**, so every one is measured *cold and warm*.
  Warm is the steady state on a settled board; cold is what a fresh board, a
  zoom out of the quality band, or a recluster pays. A warm-only number is
  measuring a hash lookup.
- **Give every generated node distinct text.** The measurement cache is keyed
  by the text, so a board built from one repeated string lets each cluster's
  font fitting reuse another's entries — which made `recluster/cold/300` report
  1 ms instead of its real 23 ms.

The GPU draw itself isn't benchmarkable here: a `canvas::Frame` needs a live
wgpu renderer. Everything feeding it is, and that's where the time goes.

Standing results worth knowing (Apple silicon, release):

- Rasterizing one card is the most expensive thing on the board, and scales
  with *glyph* size, not bitmap area — a 400×300 card at 2× zoom has more
  pixels than at 1× yet costs less, because the texture cap pulled its scale
  down. `MAX_SCALE` in `text_raster` is therefore the cost/quality knob;
  `MAX_DIMENSION` is a hard atlas bound, not a policy. Capping the scale at 6
  cut the worst case (a small card at high zoom) by ~41% and left every other
  size untouched, since they were already texture-bound below it. Dropping it
  to 4 cuts the same case by ~71% but softens cards above zoom 2.
- A cold recluster is dominated by font fitting, not by grouping (~1 µs/node).
  Seeding the search and resolving only to 1 px took it from ~10 cold
  measurements per cluster to ~6 (23 ms → 19.5 ms at 300 nodes). It resists
  further tuning: precision *is* the cost, one full layout per untried size.
  The cold path only matters on board load — a repeat recluster probes the same
  sizes, hits the cache throughout, and is ~75× cheaper.
- Link hover rejects on the curve's hull box before sampling it, which took a
  scan of 800 edges from 64 µs to 2.2 µs per mouse move.

Measured and **not** worth optimizing — don't re-propose without a new number:
clustering itself, layered layout, canvas parse/serialize, Markdown parse,
syntax highlighting, `random_id`, the per-frame `node_index` rebuild (8.5 µs at
800 nodes), the measurement cache key's allocation (15 ns of 54 ns), and
splitting the global `with_raster` mutex — a keystroke's auto-size costs
35.6 µs quiet and 41.8 µs under continuous background rasterization, because
the lock is taken per call rather than held across a batch. The `contention`
bench guards that last claim.

- `find_matches` (now `thinkboard_core::search`) compares ASCII text and query
  as bytes with no intermediate, falling back to a folded `char` buffer reused
  across nodes. 830 µs → 64 µs at 800 nodes. The Unicode path is ~5× the ASCII
  one, which is the right place to pay it.

## Testing

Tests live next to what they test, in `#[cfg(test)] mod tests`. Pure helpers are
tested directly; state transitions go through `src/app/update_tests.rs`, which
builds an `App` via `App::build(Init { .. })` and drives real `Message`s.

`App::new` reads the user's config file and registers an OS-level hotkey, so
tests never call it — `App::build` takes an already-resolved `Init` instead.
Test apps are built as `scratch: true` (suppresses every disk write) and
`windowed: true` (starts in `Phase::Running`, skipping the entrance animation).

The text rasterizer works headless, so tests may measure and auto-size text.

## Platform notes

- **macOS** — the overlay is an `NSPanel` via a class-morph on winit's window;
  that leaves winit's KVO teardown inconsistent, so `save_and_exit` flushes the
  autosave and calls `std::process::exit(0)` rather than crashing in teardown.
  Pinch-zoom and the four-finger summon gesture are native shims.
- **Linux** — Wayland can't hide a window in place, so hiding *closes* the
  window and summoning opens a fresh one. Tray uses `ksni` (pure-Rust D-Bus, no
  GTK) so the zig cross-build works; the dialogs use the xdg-portal backend for
  the same reason.
- **Windows** — release builds use the GUI subsystem (no console); the
  `no-console` feature forces that in any profile.

## Releasing

`git tag vX.Y.Z && git push --tags` runs `.github/workflows/release.yml`, which
builds a universal macOS `.app`, a Windows zip and a Linux tarball. Tag releases
for shipped user-facing features — not for internal refactors.
