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
    because one node is edited at a time. Also `set_font_features`, which is a
    global for a different reason: OpenType features come from the theme, so
    every run on screen wants the same set. `iced::Font` has nowhere to carry
    them, and `to_attributes` is the one place iced builds a `cosmic_text::Attrs`
    — see **Font features** below.
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
| `picker` | the panel shell shared by the palette, vault browser and the pickers |
| `theme_picker` / `font_picker` / `model_picker` | what fills that shell besides the palette |
| `vault` | finding the vault's boards and switching between them |
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

That thumb is drawn in the **uncached** overlay, and reads the fraction there
rather than taking it from `view`. Both matter: a cursor move doesn't invalidate
the scene cache, and the editor only publishes its scroll while laying out —
after `view` has built the widget but before it draws. Reading it in `view` is
always one update stale.

## Drawing inside a clip

**A mesh filled inside `frame.with_clip` is painted over by the parent's own
fills, whatever the call order.** `paste` appends the child's meshes to the
parent's mesh list, but the parent's fills sit in a buffer stack that isn't
flushed into that list until `into_geometry` — so the card, drawn long before,
lands *after* and covers them. Text escapes it: `text` is a separate list, drawn
above every mesh.

So anything filled for a card is drawn on the parent frame *before* the clip,
clipped by arithmetic rather than by `with_clip` — which is what the node
scrollbar thumb and `Board::draw_search_hits` both do. The failure is silent and
looks like a colour bug: the search highlight's black ink drew on bare card for
four rounds while its yellow fill went straight under the card.

## First run

No configured board means the embedded **welcome board** — a scratch canvas
that never touches disk and comes back each launch. `finish_welcome` completes
it for the machine it's on, which is why the file has a `SUMMON` placeholder in
it rather than instructions.

**The default hotkey is a key most Macs haven't got.** `Pause` is what macOS
reports as F15, which exists on a full-size external keyboard and no laptop —
so on macOS `summon_gesture` (the four-finger swipe) defaults **on** and the
welcome board leads with it, mentioning the key second. Changing the hotkey
default instead would mean picking a chord that might already be claimed;
this way nothing is taken and the overlay is still reachable.

**Obsidian's own vault registry is read** — `core::obsidian`, from
`obsidian.json` beside its settings — and each vault becomes a button on the
welcome board. Someone arriving from Obsidian shouldn't have to find their own
vault in a folder dialog. It is never authoritative: entries that no longer
exist are dropped, an unreadable or absent file is simply no vaults, and the
folder picker beside them still works.

The board says three more things it used to leave out: the command palette
exists (it is now the way to most of the app), Ask AI needs a provider before
its menu item appears, and `.canvas` files round-trip — the trust question for
anyone pointing an unfamiliar app at their notes.

**One word for the thing you work on: "board".** `.canvas` is the file format
and "canvas" stays a hidden search term, so Obsidian's word still finds the
command, but no user-facing string says both.

## The vault

A vault is a **scope, not a mode**. `config.vault` says where to look for
boards; `config.canvas` still names the one that's open. Every pre-existing way
in — the open dialog, drag-and-drop, the CLI path — works on a canvas anywhere,
with or without a vault set. Don't turn "has a vault" into a branch that
disables the rest.

Setting one does change `canvas_file::vault_root`: an explicit vault wins over
the `.obsidian` walk-up and the image-path inference, but only for canvases
*inside* it. A board opened from elsewhere still resolves its own root, so
configuring a vault can't misattribute one that has nothing to do with it.

`thinkboard_core::vault` holds the walk and the ranking (name first, the
vault-relative path at a penalty — the same title-then-keywords shape the
command palette uses). The walk skips dot-directories, so `.obsidian` and
`.trash` never appear as boards, and it doesn't follow a symlinked directory,
which is what stops a link back to an ancestor looping forever.

**Nothing watches the vault.** `sync` deliberately watches one file and states
that a large vault isn't touched; re-walking one on a timer would undo that. The
browser instead shows the last scan immediately and runs a fresh one behind it,
which is also why reopening it is instant.

**The vault also carries the theme.** Obsidian names its active theme in
`.obsidian/appearance.json` (`cssTheme`) and keeps it at
`.obsidian/themes/<name>/theme.css` — a path we can derive rather than ask the
user to hunt for. `Config::adopt_vault_theme` resolves it at startup and
whenever the vault changes, writing the result into `theme_css`.

Two rules make that safe to run unattended. A `theme_css` the user picked *by
hand* is never overwritten — only one that's empty or that we resolved from
this vault ourselves (`vault::is_vault_theme`) is adoptable. And an empty
`cssTheme` is Obsidian's own default, which we have no CSS for, so it clears
`theme_css` back to the built-in palette rather than keeping a stale theme.

It resolves in **two places that must agree**: `startup::run`, before the iced
builder registers the theme's embedded fonts, and `App::new`, which is where
the result is persisted (the startup pass must not write the user's config).
Both call the same function on a freshly loaded config, so they can't diverge.

Applying a theme is live (`App::apply_theme_css`) — colours and font names take
effect at once, and every bitmap is dropped because the old colours are baked
into it. Embedded WOFF2 faces have to reach *two* font systems: the
rasterizer's database via `text_raster::add_embedded_fonts`, and iced's via
`font::load`. `set_embedded_fonts` only works before the database is built, so
a runtime swap must use the other entry point — which also clears the layout
and measurement caches, since their keys carry text and size but not the face
those were shaped with.

`picker` is the shell all five panels draw: input, rows, keyboard nav. Rows are
a **fixed height applied to the row**, not assumed of it — iced reports a scroll
position but won't say where a given child landed, so keeping the highlight on
screen is arithmetic over that constant, and the two must not drift.

**`picker::Panel` names which panel a message is about.** Typing, arrowing,
scrolling and dismissing are the same four interactions everywhere, so they are
one message each (`PanelInput`, `PanelNav`, `PanelScrolled`, `ClosePanel`)
rather than four per panel, resolved through four small matches on `App`
(`panel_state`, `panel_len`, `panel_preview`, `close_panel`). Written out per
panel they were 155 lines of `update` and climbing with each new one. A panel
still needs its own `key_event`, because a subscription handler is a bare `fn`
that can't capture which panel it is — but that function's whole body is now
`picker::panel_key(&event, Panel::X)`.

**Every opener goes through `App::open_panel`,** which dismisses the rest and
focuses the input. Each used to name the panels it displaced, and those lists
drifted: the font and model pickers could be open at once, with the font preview
left applied over a config that disagreed. `dismiss_panels` *cancels* the two
pickers that preview rather than dropping them, since a preview is only ever
undone by cancelling — and it deliberately leaves the find bar alone, so that
re-summoning the find bar doesn't empty the query it already holds; `open_panel`
closes that separately. The test walks every ordered pair rather than the ones
someone thought to name.

**Ranking is one shape, in `core::fuzzy`.** `score_alias` scores what a row
*says* and falls back to terms it carries but doesn't show — the palette's
keywords, a theme's author, a model's display name, a board's vault-relative
path — at `ALIAS_PENALTY`, so a row found by something invisible never outranks
one found by what's printed on it. `rank` is the enumerate-score-sort around it,
and is a **stable** sort, which is what lets each caller's own order stand in as
the tiebreak: registry order in the palette, modified time in the vault.

## Themes

A theme is **data, not drawing**, so the format lives in `thinkboard_core::theme`
and the desktop maps its `Rgba` onto `iced::Color` at the boundary — the same
split as canvas colours. `src/themes/*.json` are compiled in; a file of the same
name in `<config>/themes/` shadows one.

**A theme is a patch.** Every section is optional and layers over the built-in
default. That's forced by what produces themes: a base16 scheme is sixteen
colours and no fonts, an Obsidian stylesheet is roles and no palette, a
hand-edit is one line. Requiring completeness would make every importer invent
values it has no opinion about.

**The palette layer is why bundling is cheap.** Solarized, Nord, Dracula and
several hundred others are published as sixteen-colour base16 schemes, so
`theme::derive` maps those slots onto roles *once* and adopting a scheme is
sixteen values. `colors` then overrides whatever the palette derived.

**The mapping is contrast-aware, and has to be** — the conventional slots aren't
dependable across ports, and the test in `theme_store` caught all three of
these:

- `base03` is comment-dim by design: 1.4–2.9 against a card. Never use it for
  muted text.
- `base04` is the convention instead, but several ports put a *surface* colour
  there — Catppuccin's lands at 2.6. Where it fails, muted text steps back from
  the body text until it reads.
- Inline code is orange (`base09`) by convention, but Solarized's is 2.8. Where
  it fails, the best of the warm-to-cool accents is used.

Thresholds differ by role: 4.5 for body text, 3.0 for accents and links (what
WCAG asks of large text and UI components). One threshold for everything would
reject Solarized, whose low contrast is the point of it.

Author and licence live in each theme file, which is where attribution belongs
— the picker shows names only, and marks the one in use. The author is still a
hidden search term, like the command palette's keywords.

**The search highlight is deliberately not themed.** Highlighter yellow, orange
for the current hit, black text — the convention a browser, an editor and a PDF
reader all share, which is why it reads at a glance. Deriving it from a palette
was tried and every theme got its own slightly weaker compromise.

**A hit is only ever painted by the live text path**, so a matched node must
not hold a bitmap: `refresh_snapshots` won't start one, and `snapshots_ready`
drops one that was already in flight when the search began — without that second
half the batch lands after the invalidation and quietly puts the highlight out.

It's opaque, and `draw_markdown_live` splits each run on its hits so the glyphs
over a fill are inked black rather than left in the card's own colour. That
matters: a *translucent* wash can't work at all here, because a hit has to read
against the card and leave the text on it legible, and for light text on a dark
card those pull apart with no overlap — the wash needs luminance at most 0.12 to
hold the text at 4.5:1 and at least 0.17 to reach 3:1 against the card. The old
translucent marker sat in that gap at 2.4:1, with the current hit's text down at
3.4:1. Black on yellow is 16:1, and the fill 9–13:1 against any card.

**Preview is cheap because it drops bitmaps rather than rebuilding them.** A
card with no bitmap draws its Markdown live, in whatever colours the theme now
says — so the picker recolours and invalidates on each keypress and leaves the
rasterizing until a theme is actually chosen. Clusters fall back the same way.

Precedence is `config.theme` (by name) → `config.theme_css` (a stylesheet, which
is what a vault's Obsidian theme feeds) → the built-in default. That order lives
in **`theme_store::resolve_base`, and only there** — it used to be written out
three times (the startup font pass, `App::new`, the live swap), and those have to
agree or a theme looks one way when you pick it and another after a relaunch.

Because a *named* theme wins, there has to be a way to un-name one: the theme
picker's first row clears `config.theme`, handing the board back to the vault's
Obsidian stylesheet — or to the built-in palette when there isn't one, which is
what the row says it will do. Without it, trying one of ours on made a vault's
own theme unreachable. The settings field writes the same value, so
`Field::Theme` re-resolves live exactly as the picker does.

**The font is config, not theme.** `config.font` names a body family and
`Theme::use_font` layers it over whatever the theme asked for, as the last step
of every resolution — so a font is a preference you keep while trying themes on,
rather than something each theme takes back. It overrides only the body family;
the monospace face still comes from the theme, since code wants a font picked
for code. Blank means the theme's own, which is what the picker's first row
sets — without it a font could be chosen but never unchosen.

`font_picker` previews like the theme picker, and additionally drops
`text_raster`'s shaping caches: they're keyed by text and size but *not* by the
face those were shaped with, so a stale entry keeps the old typeface's metrics
under the new one's glyphs. Its rows are each drawn in the family they name
(`picker::Row::font`) — a few hundred family names in one uniform face tell you
nothing about any of them.

**Naming a family is not enough to get it.** The shaper matches weight
*exactly* and falls back to the system face when nothing matches, and plenty of
families have no 400 at all — Futura's regular is a 500, Arial Black's a 900,
Skia reports 5. So `use_font` also snaps `weight_normal`/`weight_bold` to what
the family ships (`text_raster::family_weights`), and the picker's rows ask iced
for the same weight. Without that, choosing one of those silently renders the
board in the system font. It's the same failure `synth_weight_faces` exists to
prevent for a theme's embedded fonts.

That is also why `use_font` reads the font database, so **`set_embedded_fonts`
has to be called before it** in `App::new` — that entry point only takes effect
while the database is still unbuilt.

### Font features

A font's stylistic alternates are substitution lookups that **do nothing until
the shaper asks for them by tag**. Inter carries a single-storey `a` as `cv11`
and shows the double-storey one until told otherwise; that's what a theme means
by `font-feature-settings`, and ignoring it renders the theme's typeface with
the wrong letterforms.

`Theme::font_features` / `font_features_mono` carry the set, interned to
`'static` so `Theme` stays `Copy`. Prose and code get **different** sets: a
monospace face's slashed zero is chosen for code and wrong in a sentence.

They have to reach *two* shapers, and a mismatch is visible — the rasterized
bitmap and the live text would disagree on the same card:

- ours, in `text_raster`'s `base_attrs`/`span_attrs`, and in `measure`, which
  the search-hit boxes are laid out with (measure the hit under different
  letterforms and the highlight lands beside the word);
- iced's, via the vendored `text::set_font_features`. Every path that changes
  the theme goes through `App::install_theme`, which is the only place that
  global is written after startup.

**Read from the CSS variables only**, never a bare `font-feature-settings`
rule: a theme applies that rule under whatever selector it likes — Primary sets
`"case"` on one — and hoisting a scoped rule to the whole board turns on
features somebody asked for in one corner. Our own theme format takes the same
string (`fonts.features`), parsed by the same reader.

Features are part of no cache key — not iced's `text::cache::Key`, not
`text_raster`'s layout or measurement caches. iced's cache watches
`font_features_generation` and drops everything when it changes, because `trim`
alone keeps exactly the text that is still on screen. Ours are dropped by
whoever changes the theme.

Two things the family list must keep doing. It drops faces whose `hhea`
declares no horizontal metrics: macOS's `GB18030 Bitmap` is such a font, and
swash reads an advance from it as `(count - 1) * 4` — an overflow panic in a
debug build, inside the raster mutex, which poisons it and takes all text down
(hence `with_raster` recovers from poisoning rather than propagating it). And it
marks families that can't spell their own name (`FontFamily::legible`) so those
rows fall back to the panel font: about 200 of the 455 here are Arabic, Hebrew,
Devanagari, Noto script or symbol faces with no Latin glyphs, and a picker that
sets every row in its own face leaves half the list as tofu.

Both pickers must be **cancelled rather than dropped** (`App::dismiss_panels`).
A preview is only undone by cancelling, so a picker discarded when another panel
opens leaves what it was previewing on the board with nothing in the config to
match it.

## The model picker

A model name is exact and unguessable — `anthropic/claude-sonnet-4`,
`accounts/fireworks/models/llama-v3p1-70b-instruct` — and typing one from memory
fails at the *next question asked*, not when you type it. So the list comes from
the endpoint: `GET {ai_url}/models`, which is part of the OpenAI API every
preset implements, local runtimes included. **There is nothing provider-specific
here** — OpenRouter and Fireworks were the ones worth browsing (421 models on
OpenRouter), but neither needed its own code path. Fields past `id` are
extensions: OpenRouter gives a display name and a context window, a plain
endpoint gives neither, and both are read only when present.

It's the one panel that **fetches**, so it has two states the others don't —
in flight and failed — and both are shown in the list's own empty row, so the
panel is up on the keystroke that asked for it either way. A finished fetch
carries the URL it was for and is dropped if that no longer matches, or
switching provider mid-flight would offer one provider's models under another's
name. The last list is kept (one endpoint's worth) so reopening is instant, and
refetched behind it — the vault browser's bargain.

Failures are phrased as something to act on rather than a status code: a 401 is
"the API key was rejected", a 404 is "this endpoint has no model list", which is
the ordinary answer from a local runtime rather than a fault.

## Link routing

Two routings, chosen by `config.mermaid_links` (off by default — a board is an
Obsidian document and should look like one).

Obsidian's is a single bezier whose control points reach out **0.4x the distance
between the ports**, so the shape scales with the span: a link across 1800 px
bulges 720 px before it turns. Mermaid's is a *polyline* — dagre emits waypoints
and d3's `curveBasis` smooths them — which steps a fixed distance out of the
port whatever the span. A basis spline approximates its interior points rather
than passing through them, so the corners of a waypoint route round themselves
and no radius has to be chosen.

**`Board::link_segments` is the one place that chooses**, and it returns cubic
pieces rather than a path, because six things have to agree about where a link
is: the line, its hover and selection rings, the drag preview, ghost links, the
AI shimmer, the hit test, and the caption's midpoint. Anything drawing its own
bezier would quietly disagree with the routing under it — which the preview and
the shimmer both did until they were brought across.

**Mermaid's routing does not thin out a crowded fan**, whatever it looks like it
should do. Siblings share their leading waypoints, but that fixes only the
opening segment's control points, not its endpoint — measured on five children
across a 400 px gap it is *looser* than the bezier through the first 150 px
(72 px apart at 80 px out, against 39). Nor does Mermaid itself avoid nodes:
dagre has no collision test anywhere in it, and gets away with that because it
*owns* the positions and reserves gaps with dummy nodes. Our positions come from
the file. Don't reach for either as a fix for crowding.

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

## Testing conventions worth knowing

`App::drop_text_bitmaps` is what "the text is stale" means: the bitmaps, the
cluster snapshots, the scene cache, **and** `text_raster`'s shaping caches. That
last one is the one that gets forgotten — it is keyed by text and size but not
by the face or features those were measured with, so leaving it is how a new
theme gets laid out to the old one's metrics.

Test nodes and edges come from `thinkboard_core::fixtures`, shared with this
crate's tests through core's `fixtures` feature (a dev-dependency on the same
path crate, so it is never on in a shipping build). They are bases to amend with
struct-update syntax — `CanvasNode { width: 400.0, ..node("a") }` — so a test
says what it is *about* rather than spelling out ten fields.

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
