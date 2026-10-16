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
  a callback). The job still fails on genuine clippy
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

  - `cosmic-text` (0.15.0, the only 0.15 — iced 0.14 pins it) — its ASCII
    shaping fast path shaped a whole word in its first character's style, so
    a style change *inside* a word vanished: `x**bold**y` had no bold, and
    inline code after a space (no line break is allowed before a `.`, so
    ` .canvas` joins the word before it) was laid out in the body face. The
    patch keeps the fast path for words in one style only. Its tests and
    benches aren't vendored, so their targets are cut from its manifest.

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
| `ai` / `ghosts` | streamed answers; Copilot-style suggestion cards (shelved — nothing offers them; `ghosts::OFFERED`) |
| `canvas_file` | opening, autosaving, merging outside changes into the `.canvas` file; vault roots |
| `picker` | the panel shell shared by the palette, vault browser and the pickers |
| `theme_picker` / `font_picker` / `model_picker` | what fills that shell besides the palette |
| `vault` | finding the vault's boards and switching between them |
| `search`, `clipboard`, `command`, `settings`, `startup`, `tray`, `sync` | as named |

Related fields are grouped into sub-structs (`Ai`, `Ghosts`, `Rasters`,
`Clusters`) rather than living loose on `App`. When adding a feature, add its
module and its `impl App` block there — keep `update` a dispatch table.

## Card sizing

Auto-size **grows a card, never narrows it**, and grows its height **downward
from the top edge** — about its centre, every new line pushed the text already
written upward and let a long note climb over the card above. While editing, a
card is sized to the larger of its source and its rendered Markdown, so ending
an edit doesn't make it jump (a heading is typed at body size but rendered
large). A width is something the user can
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

**A point on a card's text is found the way it's drawn.** `board::text_frame`
is where a card's Markdown is laid out — centred in the card, or, on a card that
scrolls, top-aligned and shifted by its scroll — and the drawing, a link's hover
and click, a double-click's caret and the find bar's scroll all go through it.
Link clicks ignored the scroll and hit another line's link. The caret is found
in the *rendered* layout and traced back to the source (`edit::locate`), not by
clicking the editor at the same point: the editor shows the source, with its
markers and body-size headings, so the same point is a different line further
down a note — and on a scrolled card, a paragraph that wasn't on screen.

**A table is laid out as a grid, not as text.** A cosmic-text buffer has one
wrap width and only uniform tab stops, so cells can't line up in it. A card's
text is laid out by `text_raster::lay_out` as blocks stacked a blank line apart
— each stretch between tables in one buffer, each table cell in its own — and
the measuring, the bitmap and the live runs all take their places from it. The
parser marks a cell's spans with where it sits (`markdown::Cell`); the iOS
bindings, which draw spans as text, get them back as rows (`flatten_tables`).
The grid's rules are drawn like the quote bars: before the clip, by arithmetic.

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

## Cards over cards: renderer layers

**Within one renderer layer, iced_wgpu draws every mesh, then every image,
then every text — whatever order they were recorded in.** And the stock canvas
draws all the geometries its program returns into the *same* layer. So no
card's fill could cover another card's bitmap or text: a dragged card showed the
text beneath it interleaved with its own, as did any two cards left
overlapping. The overlay that "lifts" selected cards never lifted anything.

The board is therefore drawn by `canvas::layered`, iced's canvas with one
change: `Stacked::draw_stacked` returns each geometry with whether it starts a
layer of its own. Unselected cards that overlap an earlier card go into
**planes** above the base scene (`geometry::stacking_planes`, a sweep, one cache
per plane in `board::SceneCache`), and the selection overlay takes a layer only
when a selected or suggestion card actually covers another card.

**A layer is not free.** With antialiasing, each layer holding meshes is a
multisampled pass of the whole window plus a blit — about 0.1–0.2 ms on a retina
frame. That's why layers are asked for rather than given: a board without
overlapping cards draws in one layer, exactly as before. Measured with the tour
on 400 cards in release: no difference without overlaps, at most ~0.2 ms with a
plane in use. Don't give every geometry a layer "to be safe".

## First run

No configured board means the embedded **welcome board** — a scratch canvas
that never touches disk and comes back each launch. An image pasted onto it is
held in memory as the board is (`clipboard::HeldImage`), and written out only
if it's copied onto a board with a folder. `finish_welcome` completes
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

The platform's own faces need the same help. SF Pro and SF Mono are variable
fonts registered once at their default weight (SF Mono's is 295), so
`register_ui_families` registers them again at each weight the app asks for
(the same bytes, a second face record), and makes SF Mono the generic
`monospace` in **both** font systems — whose own defaults name faces that
aren't installed, and fell back to different ones, so inline code was laid out
in one face and drawn in another.

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

**The model is chosen where the provider is.** A new config names a local
runtime's model, which no hosted provider serves, so picking a provider and
asking failed on the very first question. The settings form's model row has a
"Choose…" button, and picking a provider that can answer opens the picker by
itself — one on this machine, or a hosted one whose key is already stored; for
a hosted one without, pasting the key does. Opened from the form, the picker
is drawn **over the form** (`ModelPicker::over_settings`), in whichever place
the form is: the board's panel layer sits under the in-board modal, and the
settings window doesn't have one. It closes with the form — left open behind
it, it would hold the board modal with nothing visible — and while it's open,
Esc is the picker's alone.

It's the one panel that **fetches**, so it has two states the others don't —
in flight and failed — and both are shown in the list's own empty row, so the
panel is up on the keystroke that asked for it either way. A finished fetch
carries the URL it was for and is dropped if that no longer matches, or
switching provider mid-flight would offer one provider's models under another's
name. The last list is kept (one endpoint's worth) so reopening is instant, and
refetched behind it — the vault browser's bargain.

Failures are phrased as something to act on rather than a status code — what
went wrong, then where to fix it — and **in one place**, `ai::describe_failure`,
for the model list and a question alike. They used to describe the same 401 two
ways, and only one of them said what to do. What was asked decides what a
refusal means: a 404 from `/models` is a local runtime without the route (the
ordinary answer, not a fault), while a question refused with a message about a
model is that model not being served. A 401 sent with no key asks for one rather
than calling it rejected, and nothing listening on localhost asks whether the
runtime is running, by its preset's name where it has one.

A failed answer stays on the board as a note, which is deliberate: it's where
the question was asked, and deleting it is the editor's ordinary delete. A
failure partway through is added under what had arrived, not written over it.

## Link routing

Three styles, named by `config.link_style` and dispatched in exactly one place,
`Board::link_segments`:

- **`obsidian`** (default) — one bezier whose control points reach out 0.4x the
  distance between the ports, so the shape scales with its span: a link across
  1800 px bulges 720 px before it turns. A board is an Obsidian document and
  should look like one until someone asks otherwise.
- **`mermaid`** — a polyline smoothed by the basis spline d3 draws Mermaid's
  flowcharts with. A basis spline *approximates* its interior waypoints instead
  of passing through them, so the corners of a route round themselves and no
  radius has to be chosen anywhere.
- **`square`** — straight runs meeting at right angles, no curve in it at all.

`link_segments` returns cubic **pieces** rather than a path, because seven
things have to agree about where a link is: the line, its hover and selection
rings, the drag preview, ghost links, the AI shimmer, the hit test, and the
caption's midpoint. The last two measured against the old bezier whatever was
drawn, and the preview and shimmer built their own — all four disagreed with the
routing under them until they were brought across. Anything new that draws a
link goes through here.

**The trunk belongs to the port, not the link** (`Board::trunk`, off the
per-frame `Board::ports` map). Capped per link it is capped by that link's own
span, so siblings at different indents step out by different amounts and stand
on trunks a few pixels apart — four links on three lines, never merging into the
stem the square style is for. Measured across the whole port they land on one.

Two things none of this does, both measured, so neither is retried on the
strength of how it sounds. Mermaid's routing does **not** thin out a crowded
fan: siblings share their leading waypoints, but that fixes the opening
segment's control points and not its endpoint — across a 400 px gap it is
*looser* than the bezier through the first 150 px, 72 px apart at 80 px out
against 39. And Mermaid does **not** avoid nodes: dagre has no collision test in
it anywhere, and gets away with that because it owns the positions and reserves
gaps with dummy nodes. Ours come from the file.

## Obsidian compatibility## Obsidian compatibility

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
- Fields Obsidian treats as optional stay in `extra` rather than becoming
  struct fields: a link node's `url`, a group's `label`, and a link's `label`,
  `color`, `fromEnd` and `toEnd`. Accessors read them; where the app edits
  them (a frame's caption, the link menu) a setter writes them back into
  `extra` — a blank label as *absent*, and a link's ends only where they
  differ from what absence means — so an untouched file round-trips byte for
  byte and an edited one looks the way Obsidian would have written it.
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

## When the file changes elsewhere

Obsidian, a sync client or a text editor can write the board's file while it's
open, so an outside change is **merged, not reloaded**: `thinkboard_core::merge`
compares each card and link, by id, with the copy both sides started from —
`App::disk` (`sync::OnDisk`), the file as last read or written here. Whichever
side changed a card is the one kept; where both did, field by field, and where
both changed the same field, ours. Position and size are each one field (x
from one side and y from the other is a place neither chose), and a deletion
doesn't take a card the other side changed. It used to be last write wins: an
outside change was reloaded only when nothing here was unsaved, so a card
added in Obsidian while one was edited here was written over by the edit.

- **Compared as written.** Everything goes through the serializer, which rounds
  geometry — a card dragged to x = 100.4 and read back as 100 wasn't moved.
- **Our own writes come back as events**, and are told apart by the
  modification time they left (`disk.mtime`). The watcher can trail a write by a
  second, so `SaveTick` checks the time too and takes a change in *before*
  saving, rather than write over it unseen.
- **The card being edited is held**: it counts as changed here, so a deletion
  there can't pull it from under the editor. If it was changed there before
  anything was typed, the editor follows (`follow_edited_card`).
- **An outside change is an undo step**, as in an editor whose file changes on
  disk; the history used to be dropped. An edit, gesture or new note under way
  has taken its undo point already, so the change is recorded from that point
  and the point is rebased past it — undoing the edit then keeps the change.
- **A rename is followed** (`sync::find_renamed`: the same inode, or the same
  modification time, in the same folder), or the next save recreated the old
  name beside the new one. A file that's gone otherwise stops the autosave and
  says so on the board, with "Save it again" — never quietly recreated.
- **Saves are whole**: written beside the file and renamed over it
  (`model::write_whole`), so nothing reads one half-written. A file that stays
  unparseable for `UNREADABLE_SETTLED` was finished that way, and is saved over
  rather than waited on forever.

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
- Stacking overlapping cards (`stacking` group): 16 µs to place 800 cards in
  planes, paid once per scene rebuild; 14 µs per frame for the overlay's
  "does the selection cover a card" check while something is selected.

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

## Looking at the UI: the tour

`src/app/tour.rs` drives the real `App` with synthetic mouse and keyboard
events and renders each step offscreen to a PNG — no window, no display, no
Screen Recording permission. It's how to *see* a change without asking for a
screenshot.

```sh
cargo test --locked -p thinkboard tour -- --ignored --nocapture        # every scene
TOUR=build_a_board TOUR_SCALE=2 cargo test --locked -p thinkboard tour -- --ignored --nocapture
```

Shots land in `target/tour/<scene>/` (or `$TOUR_OUT`). A scene is a plain
function — `click`, `drag`, `type_text`, `chord`, `shot` — added to `SCENES`.
Prefer gestures to `send(Message::…)`: a message skips the widget that would
have published it, which is exactly the part a new user meets first.
`TOUR_THEME` / `TOUR_THEME_CSS` render it in a named theme or an Obsidian
stylesheet, to see the app as a particular user has it.

For anything that changes frame to frame — editing above all — use a
**strip** rather than shots: `strip` fixes a region, `frame` adds the very next
frame after the last event (no extra redraw, so a value that lands a frame late
shows as late), and `save` tiles them into one numbered sheet with a `.txt`
legend of card size, editor box, line count and caret per frame. Read the
legend first; it says which of the three fell behind.

Every event settles, so a click that starts something long — an answer
streaming in — has finished it by the time the click returns. `start` clicks
and returns with it still going, and `film_while` films until a condition
says it's over. `click_label` finds a widget by its text, through iced's
`Operation::text`, rather than by offsets into a menu that moves whenever it
gains an item.

The AI scenes ask a **`Provider`**: an OpenAI-compatible endpoint on a loopback
port, answering from a script, so the real client is what runs — the streamed
events, the `---` that splits an answer, the sentence a refused request turns
into. Nothing leaves the machine and no key is needed. It refuses a model it
doesn't list with a 404, as OpenAI does, which is how `ai_errors` shows what a
new config's model name does on a hosted provider.

Two scenes open a board from a **real file** in a temp folder (`file_launch`),
which brings what the scratch board skips: autosave, which `timers` polls as the
subscription does, and the file watcher, which `watch` runs as a task — the
OS's own change events, not a `FileChanged` sent by hand. And under test the
**clipboard is a stand-in** (`clipboard::system`), as the config's default
location is refused: a scene that copies would otherwise overwrite whatever the
developer had copied. The tour fills it to play another app.

Seven things it has to do that a runtime does for free, each learned from a
shot that lied:

- **Deliver a redraw to the tree that draws.** An iced 0.14 button keeps its
  status on the widget, set from `RedrawRequested`; a freshly built tree drawn
  without one paints every button as `Disabled`.
- **Give big bitmaps a second frame.** iced_wgpu uploads an image over 2 MB on
  a worker thread and leaves it out of the frame that asked for it — a large
  card's text, blank. `shot` draws twice, `UPLOAD_GRACE` apart.
- **Feed the subscription listeners.** Panel keys (palette Esc, arrows) arrive
  through `event::listen_with`, not the widget tree; `App::event_listeners` is
  the one list both `subscription` and the tour read.
- **Draw after every event.** Drawing isn't only output: the editor
  highlights in its draw. Typing a note with no frames between keystrokes
  left headings unstyled — a real bug (`MdHighlighter::change_line` kept the
  latest change, not the earliest) that a window, drawing between keys, shows
  far more rarely.
- **Present every animation frame.** An image is uploaded when a frame is
  prepared, which offscreen only a read-back does. Ticking a bitmap's warm-up
  without presenting never started its upload, and the card it was warming
  for drew blank.
- **Enter a Tokio runtime.** reqwest registers with Tokio's reactor wherever
  its future is polled, and panics with none entered; `pump` enters iced's
  own executor around every poll.
- **Not wait out a countdown.** `settle` waits on `App::transitioning` —
  `animating` less the ghosts' ten-second countdown, which run to its end
  removes the very cards a shot was settling to show.

## Platform notes

- **macOS** — the overlay is an `NSPanel` via a class-morph on winit's window;
  that leaves winit's KVO teardown inconsistent, so `save_and_exit` flushes the
  autosave and calls `std::process::exit(0)` rather than crashing in teardown.
  Pinch-zoom and the four-finger summon gesture are native shims.
  **A see-through overlay is composited over every window under it, every
  frame**, and those windows count as visible, so their apps keep drawing: on a
  ProMotion display over a busy desktop it alternates 60 and 120 Hz, while the
  same board over an empty Space, opaque, or in a window holds 120. A frame
  costs ~5 ms either way, and a `CADisplayLink` frame clock changed nothing —
  it isn't ours to optimize. So the overlay's background is a choice
  (`config::OverlayBackdrop`): transparent; the theme's colour, which is the
  window's opaque flag (`platform::set_overlay_opaque`, switched live — the
  window is always made see-through-capable); or a **picture** — the desktop's
  wallpaper, or an image of your own (from Settings, or an image card's "Use
  as overlay background") — in an opaque panel of our own under the board's,
  covering everything else. The wallpaper is any app's to read
  (`desktopImageURLForScreen:`); the windows on screen would take the Screen
  Recording permission. Every picture is prepared **once, in the background,
  as soon as it's chosen**, and the wallpaper is read once per launch, from the
  Space the app started on: following each Space's own meant reading (and on a
  new Space, blurring) at the moment of summoning, and the lag that added was
  worse than a stale picture. macOS's live behind-window blur was tried too:
  no faster than the desktop, a second slower to summon, and greyed colours.
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
