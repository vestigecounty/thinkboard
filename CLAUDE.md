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
  - `iced_graphics` — center-aligned text editing (`set_editor_centered`), which
    the editor overlay needs to match read-mode alignment.
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

## Obsidian compatibility

`.canvas` files are Obsidian's, and round-tripping must not damage them:

- `CanvasNode`/`CanvasEdge` carry `#[serde(flatten)] extra`, so fields another
  tool wrote survive a load/save cycle. Same for top-level keys (`doc_extra`).
- `to_canvas_json` mimics Obsidian's own formatting: tab-indented top level,
  each node/edge as one compact line, integer coordinates.
- Node `type` is a `String`, not an enum. Only `"text"` and `"file"` are
  rendered today — `"group"` and `"link"` nodes load and round-trip but draw as
  ordinary cards, and a `file` node pointing at a `.md` note renders blank
  (every `file` node is loaded as an image, with no extension check). These are
  known gaps, not invariants to preserve.

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
