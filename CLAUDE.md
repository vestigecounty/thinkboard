# ThinkBoard — working notes

A Rust/iced desktop overlay for editing Obsidian `.canvas` files, summoned over
the desktop with a global hotkey. There's also an early SwiftUI iOS front-end
driving the same core through UniFFI.

## Commands

```sh
cargo build --locked --workspace --all-targets
cargo clippy --locked --workspace --all-targets -- -D warnings
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
- **Clippy runs with `-D warnings`**, on every platform CI builds: a new
  warning fails the job. The few lints that are intentional — index loops in
  union-find, `&PathBuf` in a signature that mirrors a callback, a long
  parameter list that is one call's inputs — are `#[allow]`ed on the item, with
  the reason beside it. Do the same for a new one, or fix it; don't loosen the
  flag.
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
    — see **Font features** below. And `editor_lines`, the editor's last
    layout in visual lines (held, shown, scrolled past), which the scroll
    fraction is worked out from: the fraction is 0 for content that fits
    however it's scrolled, so the tour's check that a card keeps up with its
    editor (`TOUR=growing_lines`) reads these instead.
  - `iced_wgpu` — honors wgpu backend env vars so
    `WGPU_DX12_PRESENTATION_SYSTEM=visual` can pick the DirectComposition
    swapchain (transparent overlay on Windows). And it asks the app how much
    to antialias once the adapter is known (`set_msaa_choice`), giving it the
    most samples the GPU can draw up to that — iced's builder can only say
    yes (4×) or no, before there's an adapter. The app's answer is
    `config.antialiasing`, file-only: `auto` is 4×, but none on OpenGL,
    where VMware's SVGA3D driver, in a Linux VM, drew the board's cards over
    the settings panel and only the last of several canvases on a page.
    OpenGL is what's left where there's no Vulkan — a VM, or a GPU old
    enough to spare the samples — so its edges are rougher rather than
    wrong. `off`, or `2x` to `16x`, overrides it.

  - `cosmic-text` (0.15.0, the only 0.15 — iced 0.14 pins it) — its ASCII
    shaping fast path shaped a whole word in its first character's style, so
    a style change *inside* a word vanished: `x**bold**y` had no bold, and
    inline code after a space (no line break is allowed before a `.`, so
    ` .canvas` joins the word before it) was laid out in the body face. The
    patch keeps the fast path for words in one style only. And a line all
    right to left was laid out from its box's right edge, which for iced's
    canvas text, unbounded, is at infinity: Hebrew or Arabic drawn live
    overflowed a debug build and drew nothing in a release one. An unbounded
    width is aligned against the widest line instead. Its tests and
    benches aren't vendored, so their targets are cut from its manifest.

  They're checked in, so a clone needs no extra setup. Re-vendoring means
  re-applying these patches.

## Layout

```
src/            the desktop app (iced, wgpu, winit)
  app/          state, the update loop, and one module per feature (below)
  canvas/       the board canvas widget, text rasterizer, camera, shaders
    board/      the board split by concern: input, links, cards, search,
                clusters, offscreen markers, AI marks, the frame meter
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
| `theme_picker` / `font_picker` / `model_picker` / `provider_picker` | what fills that shell besides the palette |
| `vault` | finding the vault's boards and switching between them |
| `switcher` | back and forward through the boards visited (⌘[ ⌘]), and where each was left |
| `summon` | the summon key under Wayland (the desktop's portal, or COSMIC's own shortcuts), and a second launch handing over to the copy running |
| `search`, `clipboard`, `command`, `settings`, `startup`, `tray`, `sync`, `updates` | as named |

Related fields are grouped into sub-structs (`Ai`, `Ghosts`, `Rasters`,
`Clusters`) rather than living loose on `App`. When adding a feature, add its
module and its `impl App` block there — keep `update` a dispatch table.

## Card sizing

Auto-size **grows a card, never narrows it**, and grows its height **downward
from the top edge** — about its centre, every new line pushed the text already
written upward and let a long card climb over the card above. While editing, a
card is sized to the larger of its source and its rendered Markdown, so ending
an edit doesn't make it jump (a heading is typed at body size but rendered
large). A width is something the user can
drag to deliberately, and re-deriving it from the text on the next keystroke
threw that away. `max_card_width` bounds how wide a card grows *by itself*; it
is not a limit on one sized by hand. Height still tracks content, so text
neither clips nor leaves a card standing empty.

Height stops at `text_raster::MAX_AUTO_HEIGHT` (`MAX_DIMENSION / 2`). A card's
bitmap is capped at 2048 px on its longest side, so a taller card loses
resolution in proportion — retina at 1024, 1:1 at 2048, and below 1:1 past that,
where the bitmap holds less detail than the card is. Rather than let a long card
grow until it visibly blurs, growth stops and the overflow scrolls;
`Board::scrollable` forces the scrollbar on for such a card whatever
`card_scrollbars` says, or the rest of the card would be unreachable.

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
and click, a task box's click, a double-click's caret and the find bar's scroll
all go through it. A box ticks on the first click, selected card or not, and
`markdown::toggle_task` finds its `[ ]` with the parser that drew it, counting
boxes in the trimmed text as the drawing does.
Link clicks ignored the scroll and hit another line's link. The caret is found
in the *rendered* layout and traced back to the source (`edit::locate`), not by
clicking the editor at the same point: the editor shows the source, with its
markers and body-size headings, so the same point is a different line further
down a card — and on a scrolled card, a paragraph that wasn't on screen.

**A text bitmap knows what it's of** (`Snapshot::depicts`: the text, hashed,
and the size it was laid out at). Batches run in the background, and a card can
be rewritten or resized while one does — and by code that forgets to invalidate
it: an answer's cards are widened to one width at its end, and a bitmap made
at a `---` was drawn stretched over the wider card until something repainted
it. So a stale one is turned away as it lands, a refresh lets go of one whose
card was resized (size only — refresh runs on every pan event), and drawing
never puts one on a card of another size.

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
it for the machine it's on, which is why the file has `SUMMON` placeholders in
it rather than instructions.

It's built from ordinary cards and links, laid out as a map: the welcome card,
its buttons below, the Obsidian branch to the left (the `.canvas` note, the
vault buttons), and the tips down the right in the order they're first needed —
edit, connect, summon, palette, settings, right-click, AI. A tip is one idea:
its aside (the hotkey, the AI provider) is a card of its own beside it. A
button is a card with a `tb_action`; it ends in → and shows a pointer, and the
one to do first (`tb_primary`) is ringed in its colour.

**The default hotkey is a key most Macs haven't got.** `Pause` is what macOS
reports as F15, which exists on a full-size external keyboard and no laptop —
so on macOS `summon_gesture` (the four-finger swipe) defaults **on** and the
welcome board leads with it, the key in a card beside it. Changing the hotkey
default instead would mean picking a chord that might already be claimed;
this way nothing is taken and the overlay is still reachable.

**Obsidian's own vault registry is read** — `core::obsidian`, from
`obsidian.json` beside its settings — and each vault becomes a button on the
welcome board. Someone arriving from Obsidian shouldn't have to find their own
vault in a folder dialog. It is never authoritative: entries that no longer
exist are dropped, an unreadable or absent file is simply no vaults, and the
folder picker beside them still works. On Linux there are three places to
look — the XDG config directory, and the ones Flatpak (its most common
install there) and Snap give Obsidian instead — and a vault in more than one
is offered once.

The board says three more things it used to leave out: the command palette
exists (it is now the way to most of the app), Ask AI needs a provider before
its menu item appears, and `.canvas` files round-trip — the trust question for
anyone pointing an unfamiliar app at their notes.

**One word for the thing you work on: "board".** `.canvas` is the file format
and "canvas" stays a hidden search term, so Obsidian's word still finds the
command, but no user-facing string says both.

**And one for what's on it: "card".** Every kind — text, image, link, an
embedded note — is a card to the person using it, as it is in Obsidian's own
canvas. The code says **node**, because the file format does (`nodes`,
`CanvasNode`), as it says `canvas` where the UI says board. **"Note" means a
Markdown file from the vault** (`NodeKind::Note`), in code and on screen —
never a card in general; it was both, and "Add note" beside "Add node" beside
"Colored card opacity" was three words for one thing. Config keys are read
and edited by hand, so they say card too (`max_card_width`); comments and test
names describing what's on screen may as well. "node" and "note" stay hidden
palette terms, so either still finds "Add card".

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

`picker` is the shell all six panels draw: input, rows, keyboard nav. Rows are
a **fixed height applied to the row**, not assumed of it — iced reports a scroll
position but won't say where a given child landed, so keeping the highlight on
screen is arithmetic over that constant, and the two must not drift. A section
header takes a row's slot for the same reason: it counts in the arithmetic
(`picker::slot`) but never in the selection, so the highlight, Enter and the
drawing all index the same list of rows. It wears the find bar's surface
(`chrome::accent_panel`, shared rather than copied), bolds the letters a query
matched (`fuzzy::matched`, the same walk `score` makes), and names its keys in
a foot. The scrollbar runs in the list's right inset, where iced's floating bar
is over nothing — inside the rows it crossed the highlight's ring.

**The palette is a menu until you type, then a search** (`command::listing`).
Empty, it lists what can run *now*, in sections — the selection's commands
first when there is one, then the last few run this session, then by topic —
and leaves out what can't run rather than grey it. Typed into, one ranked list,
settings included, with what can't run moved last and saying what it needs:
searched for by name, a command should explain itself. The seven colours are
search-only; listed, they were half the selection's section. A shortcut is
written once (`command::Keys`) and drawn as the platform spells it — ⇧⌘Z on a
Mac, Ctrl Shift Z elsewhere — and only shortcuts that exist are drawn.

**Boards are found in the palette too.** Empty, the boards opened last come
before the commands run last — never the open one, so ⌘K ↵ is back to the board
before. They're `config.recent_boards`, kept across launches as `canvas` is
(recorded in `open_canvas`, at launch, and on a followed rename), unlike the
session's recent commands. A search ranks the recent and the vault's boards by
the browser's rule (`vault::score`) into a section of their own, at most five,
which leads only when it holds the better match: a tie goes to the commands, so
a command typed by name still runs. With no board matching, it's the search it
always was, with no sections.

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

**While the find bar has matches, the rest of the board dims.** Every card
without a match fades (`board::search_dim`), with its shadow and with any link
that doesn't touch a match, and the card holding the current hit is ringed in
the accent, at a fixed screen width so zooming neither loses nor thickens it.
That finds the *card*; the yellow finds the *word* in it, and stayed yellow
when the redesign proposed an accent wash: a wash faint enough to leave the
card's text legible (32% accent, text 5.5:1) stands 1.9:1 off the card, and
one strong enough to read as a mark (55%, 3.1:1) takes the text down to 3.5:1.
Nothing dims when nothing matches — there'd be nothing to point at. A click
on the board closes the bar, as Esc does, before it does anything else — on
empty space the hit's card is selected, as with Esc; on a card or a link, what
was clicked is — and a drag or a pan leaves it up. The
matched cards are kept with the matches (`Search::cards`), not found from them
each frame: a short query can match thousands of times.

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
own theme unreachable. The settings form's Theme row opens the same picker
(over the form) and its Reset clears the name the same way.

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
extensions: OpenRouter gives a display name and a price, a plain endpoint
gives neither, and both are read only when present. Not the context window,
which it gives too: a question from the board is a few hundred tokens, far
inside any of them, and the number took the price's room in a row. Three
shapes are read as they come rather than per provider: a title under
`display_name` (Anthropic's, Together's), a list in pages that says there's
more (`has_more` and `last_id`: Anthropic's, twenty to a page), and an id
named as a resource (`models/…`: Gemini's, asked for bare).

**The presets** (`ai::PRESETS`) are listed by what sort of provider each is
(`ai::ProviderKind`): on this machine, many models behind one key (a
router like OpenRouter and a host like Groq are the same service to whoever
asks), and the model makers' own — and within a sort, the ones someone's
likeliest to have a key for first. They're chosen in a picker of their own
(`provider_picker`), a section to each sort: as a dropdown, sixteen were a
long list in no order anyone could see, and iced's has no headers to give
it one. From the Provider row it opens in place, under the row; from the
palette ("Change AI provider…", listed even with no provider, since it's
the way to one) it opens over the board, and what the choice still wants
— a key, Custom's URL — opens the form at that row. Choosing is the form's
own `PickPreset`, so the model picker follows as it did. **Of the runtimes
on this machine, it lists the ones running**: each has a port of its own,
so the picker asks each as it opens (`ai::answers`: a model list, or a
refusal for want of a key — not just anything on the port) and lists those
that answer, plus the one in use, marked "not running" if it isn't. On
localhost a closed port refuses at once, so the asking takes milliseconds;
the last answer is kept to open on meanwhile, and the highlight stays on
its row as rows come and go above it. With none running the section says
so, naming them, rather than go. Under test nothing is asked: what the
developer has running would decide the shot, so tests say (`RunningHere`).
Each is an OpenAI-compatible base URL, checked against the provider's own
docs in November 2026: Anthropic's is its compatibility layer, which it
doesn't call long-term; DeepSeek's documented one has no `/v1`; llama.cpp's server moved
from 8080 to 9931 in October 2026, and the sentence for one that can't be
reached says what to do about an older one. Left to Custom: providers whose
URL is per account (Azure, Bedrock, DashScope), Perplexity (its chat
completions ended in September 2026), and runtimes that share 8080 (LocalAI,
llamafile) or don't stream (GPT4All).

**The model is chosen where the provider is.** A new config names a local
runtime's model, which no hosted provider serves, so picking a provider and
asking failed on the very first question. The settings form's model row has a
"Choose…" button, and picking a provider that can answer opens the picker by
itself — one on this machine, or a hosted one whose key is already stored; for
a hosted one without, pasting the key does. Opened from the form, **it opens
in place, under the Model row, with the form still there beneath it**
(`open_model_picker_from_settings`) — in the board's modal as in the
settings window. High over the form it was a panel over a panel, away from
where it was asked for, so the form used to close for it, and a model
chosen left you on the board rather than back at the form. It closes with
the form, and has Esc to itself while open. "Sign in…" closes the form —
the browser has it from there — and the models come up over the board
after, as they do when the provider was picked from the palette.

**A panel opened from a form control is put under it** (`picker::Anchor`):
right-aligned to the control, moved left and up as far as keeps it in the
window, over the control where there's no room below. iced doesn't say
where a widget landed, so the control is a named container and
`picker::find_anchor` asks for its bounds — less the form's scroll, which
a widget's reported bounds leave out — with those of the form's host
(`FORM_HOST`, the modal's backdrop or the settings window), which is the
window to keep inside. The panel isn't drawn till the answer comes
(`Place::Finding`), or it would show up somewhere else first, so its input
is focused again then; a control that can't be found puts it at the top.
The Theme and Font rows' pickers still open at the top, over the form.

It's the one panel that **fetches**, so it has two states the others don't —
in flight and failed — and the panel is up on the keystroke that asked for it
either way. A failure is said in the list's own empty row. In flight, with
nothing kept from before, the list is drawn as rows still to come, at its full
height, and the head says what it's waiting on (`picker::Shell::loading`): a
one-row panel high over the form, growing when the list came, read as broken.
A band of light crosses the rows meanwhile — one band across the whole list,
which is why it's a canvas rather than containers, each of whose gradients
would be its own — on frames asked for only while it's up, and held still
when animations are off. A finished fetch
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
runtime is running, by its preset's name where it has one. A 404 to a
question means nothing's there only when it comes without words of the
server's own (`error_message`): OpenRouter refuses a request no provider of
the model can serve with a 404 and a message, which read as "Nothing
answers at" the very URL that had just answered.

**A model that reads text only is asked again without the images**
(`ai::run`, on `refuses_images`): the image cards in a chain are mostly its
context, and an answer without them beats a refusal. The answer's first card
says so ahead of its first words (`Event::ImagesLeftOut`) — in the card, not
in a label that fades, since an answer about a picture it never saw reads as
nonsense for as long as it's on the board. Read from the refusal, not from
`/models`' input modalities, so it works where the list hasn't been fetched,
or doesn't say; the refusal costs nothing.

A failed answer stays on the board as a card, which is deliberate: it's where
the question was asked, and deleting it is the editor's ordinary delete. A
failure partway through is added under what had arrived, not written over it.

**Free models are listed first**, with nothing typed: OpenRouter's router to
them (`ai::FREE_ROUTER`), which routes to an available one, then the models
themselves, "Free · rate-limited" — none is put forward as one to rely on —
then "Paid". Headers say what's so, and nothing more. Sections appear only where
the endpoint prices its models, which is where any can be free, so the rest are
the priced ones. The free sections and a free row's detail are tinted in a
card's green, softened toward the muted text; "Paid" in the theme's accent;
each taken toward the text until it reads on the panel at 3:1
(`model_picker::tint`), so no theme gets a colour of ours it can't read. Free is read live from `/models`, never a list of
names: nothing charged to ask or to answer, and text the only output
(OpenRouter lists music models at nothing too). Typed into, the sections
stay, each ranked within, and the one holding the best match leads, so Enter
takes it as it would from one ranked list; a tie goes to the paid, since a
model searched for by name is more likely meant than its free variant.
Whether there are sections is the whole list's to say, so a search down to
one model still says which it is. A refusal from one is
worded for it in `ai::describe_failure`: a 429 is the free models' limit,
which a moment's wait may not lift, and a 402 wants credits, which a balance
below nothing does even for a free model.

**A free model isn't private, and the app says so where it matters.**
OpenRouter doesn't train on what it's sent, but many of its free models'
providers say they log it and may, and those come first in the picker. So
each free section's header warns at its right ("May train on prompts", a
warning mark, a card's orange taken toward the text until it reads) — where
the eye is when choosing from it. Two things were tried and dropped: a muted
line over the list read as a hint the list didn't need, and a warning band
there stood over the list whatever was chosen. The settings' Model row says
it in full while a free model is in use, and Ask AI, in the node menu and
the palette, says beside it what a question goes to (`App::ask_ai_label`):
the model by the name the list gives it, less its vendor, or its id's last
part before one's fetched (`ai::short_name`), "· free" after a free model —
or with no model named, the preset or host (`ai::destination`), whose
runtime answers with what it has loaded. A long name is what's cut
(`chrome::fit_line`), never "free". The provider beside the model made a
busy line, and **no provider's logo is drawn**: nearly every brand's
guidelines want permission for one and forbid recolouring it, and Groq's
forbid one in an app's interface.
Whether the model in use is free is read from the list kept for the
endpoint, or from its name before one's been fetched (`ai::free_by_name`).
OpenRouter's privacy settings can rule out every provider of a model, which
it answers with a 404 naming its "data policy" — worded as that, not as
nothing answering.

**The picker opens on the model in use**, as the theme picker does, scrolled
to it: under the free sections it was out of sight. A refetch landing keeps
the highlight on its model rather than sending it back to the top.

**What a model costs is said where it's read.** The picker covers the
Model row's note, so a priced row says its price ("$3 / $15", from `/models`' per-token `pricing`) — the
current one too, as the one to compare against. Its unit, in and out per
million tokens, is named once, in the Paid header: the sections are there
wherever prices are, through a search too. Beside it, `$$$$`, always all
four, the rightmost lit in gold by the mean of the two prices — under $1,
$5, $20, and above (`picker::Row::gauge`) — so the extent is the same on
every row and the big jumps show at a glance. After its name, a picture
marks a model that reads images, where `/models` says (OpenRouter's input
modalities, Mistral's vision capability) — only there: a row without it
keeps no room for it, and where the list doesn't say, nothing's marked,
which isn't "text only". Marking the text-only ones instead would draw a
negative. The settings' Model row says the price again, and "text only"
for a model known to read no images, for whoever opens them later — no more:
the answer that went without its images says why — from the list kept for
the endpoint, which the
form fetches as it opens on OpenRouter — the one provider whose list has
prices — where none is kept, and not again after a failure that launch
(`fetch_models_for_form`). And what an answer actually cost is said under its
last card once it's done (`board::Spent`), then fades: OpenRouter sends the
usage and its cost in the stream's last chunk, unasked (`ai::Event::Usage`).
Elsewhere it's the tokens where they're given, "free" for a free model, and
nothing from a runtime on this machine, which gives no cost. The board asks
for its own frames for it, one at the end of the hold and then each while it
fades, so a label held still costs none. Prices exist only where `/models`
lists them, which today means OpenRouter; a provider that prices in another
shape would be code of its own.

**OpenRouter can be signed into for a key** (`app::sign_in`, the protocol in
`core::sign_in`) — not as a recommendation, but because it's the one provider
that makes a key for an app it's signed into; the rest hand keys out only on
their own sites. So the way to it is OpenRouter's key row, where there's no
key yet ("Sign in…" beside Paste); the Provider note while AI is off says
only that some hosted providers have free models. It's OAuth with PKCE: the browser signs in, a code comes back, and
the code is traded for a key labelled ThinkBoard in the person's account,
kept with OpenRouter's like a pasted one. ThinkBoard comes back in front of
the browser — the settings window, or the board's, the overlay as its summon
brings it (a focus request alone left the macOS panel behind the browser) —
and asks for attention, all a window can do under Wayland. The model picker
opens with a line over the list saying the sign-in worked (`Shell::notice`):
the picker coming up by itself didn't say so, and a note under the key row
was under the picker. The
code comes back through **the site's relay page** (`signin/` on the site,
`demo/public/signin/`): sent straight to `localhost`, the consent page would
name the app `localhost:<port>`. The relay forwards it to `127.0.0.1` on the
port the app waits on, which it reads from `state` — and nowhere else. The
code is worth nothing without the verifier only that waiting sign-in holds,
so passing through a page is safe; one request is answered, the one with this
sign-in's `state`, and after ten minutes, a code's life, it gives up.

## The settings form

Five tabs by topic — General, Appearance, Canvas, AI, Advanced — in
`app::settings` (the vocabulary and applying an edit) and `settings::form`
(drawing it), hosted twice: `settings::modal` in the board, from the palette,
and `settings::window` in the tray's window. One long column with an
"Advanced" disclosure ran to nine screens and forgot it had been opened; the
tab is now kept for the session.

- **A row says what it does only while it's focused** — pressed, or its
  control used (`Msg::field`), since a press on a control never reaches the
  row around it. Anything that can't wait for focus is a *note*, always shown:
  which source the theme comes from, why a switch is greyed (and a link that
  ungreys it), what a provider needs, how much a performance setting matters.
- **A changed setting is marked on its row** (`Field::modified` against a
  fresh `Config`), with a Reset where picking the default isn't already one of
  the row's choices. What the app fills in itself isn't a change: the open
  board, the vault's own stylesheet, the model and key a provider brings.
  **The tabs carry no count** — tried, and read as notifications.
- **Dependent rows grey rather than vanish**, so the dependency can be read
  off the form; the AI rows past Provider are the exception, as before.
- **`FILE_ONLY` names the settings that only the file holds** — tuning
  nobody changes twice. The foot of Advanced counts them, shows the path, and
  offers **Reload**: the file is read at launch and written whole, so a hand
  edit made while the app ran was lost to the next change made in the form.
  Reload keeps the open board, and `Config::load` won't read the default
  location under test, as `save` won't write it.
- **Zoom speed is one multiplier** (`config.zoom_speed`) over the wheel,
  ⌘-scroll and the native pinch — whose step was a constant no setting
  reached. The two steps it multiplies stayed, file-only.
- **Colours are roles from the theme** (`form::Look`): a field's well a step
  below the panel, a button's face a step above, hairlines in the text colour
  at low alpha. iced's palette has one colour where the form needs those.
- **The settings index is one list** (`settings::index`, with each field's
  `label`, `tab` and `kind`): the form takes its row labels from it and the
  command palette searches it. A choice is an entry per option ("Link style ›
  Mermaid"), applied as it's picked, the one in force marked "current"; a
  switch flips and says its state; anything else opens the form at its row
  (`App::open_settings_at`). Settings join a *search* only — the bare palette
  stays a list of commands. Written out by hand, the palette's toggles had
  drifted from the form's labels.
- The modal hangs from a fixed top rather than centring: each tab is its own
  height, and centred, the tab bar moved under the pointer from tab to tab.
- **One control height, one type scale.** Fields, buttons, lists and keycaps
  are all `form::CONTROL` tall, a row is at least that, and every size sits on
  a whole-pixel line (`form::Type`). Its weights are the theme's
  `weight_normal`/`weight_bold`, never a named one: a family with no 600 drew a
  semibold tab in another face altogether.
- **The overlay background tiles are previews.** The wallpaper and your own
  picture are prepared as the overlay would show them
  (`platform::preview_backdrop_picture`, sharing the background's own
  preparation), once per launch, from when the form opens; a sample picture
  stands in until one's ready, or when none is chosen.

The tour's `settings_window` renders the window host instead of the board's;
the `settings` scene visits every tab in both.

## When nothing is on screen

**Panned off every card, the window's edge points back** (`canvas::markers`):
a dot toward each group of cards — the groups semantic zoom draws, so the two
agree on what a group is — larger and more solid the nearer it is, the nearest
ringed and named in plain text, any other named in a chip while it's hovered.
The chip takes a renderer layer of its own (only while one is up), because text
is drawn above every fill in its layer and the two names showed through each
other; hiding the nearest's name instead made it blink with every dot the
cursor crossed. A click flies there (`FocusWorld`) at the zoom you're at. It replaced a bar along
one edge that said only *that way* — not what was there, nor that anything was
anywhere else.

Dots closer than two click targets are dropped for the nearer group's, and
there are at most sixteen: past that the edge is a dotted line. Which marker is
hovered is checked against the cursor when it's drawn, not taken from the last
mouse move — a fly-to moves the camera under a cursor that's holding still, and
the hover it left behind belongs to a dot somewhere else.

## The board by keyboard

Without the mouse, a card can be reached, edited, linked, moved, coloured and
put away (`app::navigate`). Nothing used to select one, so everything that acts
on a selection was out of a keyboard's reach. **The arrows go spatially**
(`thinkboard_core::navigate::nearest`): a board has no reading order to tab
through, so → is the card beside this one, a card off to the side counting
twice its distance. Shift+arrow adds to the selection, ⌥+arrow nudges (a run of
nudges is one undo step), Enter edits with the caret at the end, Tab adds a
card linked to the right, ⌘= ⌘− ⌘0 zoom and fit, ⌘, opens settings. Ending
an edit from the keyboard (`FinishEditing`: Esc, ⌘Enter) leaves the card
selected, as does closing the find bar on a hit; undo keeps the selection by id.

**Esc still dismisses the overlay** once nothing is open over it — the user's
call. Deselecting first would make putting the board away a two-key job.

## Copying cards out

**Copied cards are text to another app and cards to this one**
(`app::clipboard`). ⌘C writes them in the chain format
(`thinkboard_core::chain::text`) as the clipboard's plain text — what a
terminal or a coding agent pastes — and rendered as HTML, which Obsidian turns
back into Markdown. The cards themselves ride in that HTML as a comment, which
every app that pastes HTML leaves out and ThinkBoard reads back first. They
used to be the plain text, so another app pasted JSON with positions in it. A
comment ends at `--`, which the JSON only holds inside a string, so it's
written `-\u002d` there.

**The text is written for a person or an agent to read**: one card is its own
text; several are a list in link order, each card's own lines indented under
its item — joined by blank lines they ran together — and a link's label leads
the card it points to ("→ ruled out: …"). A web link card is its URL and a
file card its path. A group is its outline, and is copied with its cards.

**Ask AI keeps its own prompt** (`ai::upstream_prompt`, `ai::group_prompt`),
though it reads the same cards. Its answer is split into cards at `---`, and
a prompt of another shape can change the shape of what comes back — a model
shown a list tends to answer with one. The old prompt is the one tried against
real models; the copy's format hasn't been, and the tour's provider answers
from a script whatever it's sent. A group's outline is one walk written for
either reader (`ai::Outline`), and the prompt's is pinned by its tests.

**A chain is a card and every card leading into it**, what Ask AI reads.
⌥-click selects one (Alt does nothing else on a card), as does the palette's
"Select chain" where a Linux window manager takes ⌥-click. ⌥⌘C (Ctrl+Alt+C
elsewhere) copies the chain of each selected card without selecting it, so
the shimmer sweeps it once — an `AiFlow` with no answer card — or nothing
would show what went. On Windows Ctrl+Alt is AltGr, so AltGr+C on the board
copies a chain.

**Mermaid is another way to write the same copy**, from the palette only. It's
plain text alone, fenced: an app that pastes HTML takes the HTML over the text,
and would lose the flowchart. A card's label is its Markdown read as the board
draws it — Mermaid shows the markers as written — escaped in Mermaid's entity
codes, and a group is a subgraph.

**A Mermaid flowchart pasted is the cards it draws** (`thinkboard_core::mermaid`)
— an AI tool writes one readily, and a card holding its source was all a paste
gave. It's read in full or not at all: a line it can't read leaves the paste
as text, rather than cards with that line quietly gone. It's laid out in the
chart's own direction, each subgraph's inside first and its frame drawn round
it, since a group on a board is only where its cards are; a node is in the
subgraph whose body names it, as Mermaid has it. "Paste as text" (palette
only) pastes the source regardless, or the text of copied cards. The copy's
flowchart reads back as the same cards, less what Mermaid doesn't carry:
Markdown formatting, and a note or image card, which comes back as its path.

The clipboard stand-in tests copy and paste through is shared by the whole
binary, and tests run side by side, so one that copies or pastes holds
`clipboard::system::exclusive()`.

## Going back to a board

**A board opens where it was left** (`app::switcher`). Each board left this
session keeps its camera, so going back to it — by ⌘[ ⌘], the palette or the
browser — lands on the part being worked on; zoomed out to all of it, that
part had to be found again every time. Only a board not yet seen this session
is fitted. Session-only, like the palette's recent commands.

**⌘[ and ⌘] go back and forward** along a browser-style trail of the boards
visited, each on it once, wrapping round past either end: opening a board
drops what was ahead and puts it last, stepping moves along it and changes
nothing. A tap, or a few quick presses, switch with nothing on screen; with ⌘
held `board::SHOW_SWITCHER_AFTER` (300 ms, from ⌘ going down — by itself or
with [ and ]), a strip shows the trail as outlines (taken
when each board was left, so nothing is read from disk), older to the left as
`[` points, and letting ⌘ go lands on the highlight. Esc stays put.

**⌘ held for anything else isn't a look at the boards** (`board::uses_command`):
a chord, a ⌘-click or a ⌘-scroll while it's down keeps the strip from showing,
or puts it away, and ⌘ pressed during a drag, an edit or a panel doesn't start
the clock. The strip takes the pointer but **not the keys** (`Board::switching`,
not `modal`), so a chord made after hesitating on ⌘ still does what it does.
The hold is timed, and the release seen, by the **board widget**
(`ShowSwitcher` from a `request_redraw_at`, `CommandReleased` on every ⌘
release), not by a listener of the strip's: a tap's press and release, or ⌘
and a chord's key, can arrive in one batch, before the subscription the press
asked for exists. The tour waits the time out (`wait`) before its shot.

**The last two boards left are kept warm** (`switcher::Warm`): their clusters
and text bitmaps — moved out rather than freed, which took 10 ms off the
switching frame — and their on-screen images, pinned on the GPU with
`image::allocate`. Coming back to one whose file is unchanged recomputes
nothing; otherwise it's worked out afresh, its clusters synchronously when
the view opens on them (as at launch) rather than drawing every card live
until a background pass lands. A board's bitmaps are kept only up to
`WARM_BITMAPS` (128 MB): 800 cards fitted to the view bake at the 512 px floor
and came to 388 MB. `drop_text_bitmaps` lets go of all of it.

**Background work belongs to one board.** `SnapshotsReady` and `ClustersReady`
carry the `board_epoch` they were started for and are dropped after a switch:
they used to land on the next board — another board's cluster titles over its
cards, bitmaps keyed by index on cards they weren't made from — and their
`pending` flags held back the new board's own work until they had.

**A cluster title is installed once it's on the GPU** (`install_titles`, then
`ClusterTitleUploaded`), drawn live until then. Titles are big enough to
upload off-frame, so installed at once they drew blank after every recluster
and switch. The allocation is let go once installed — pinned, every title's
texture would be held while zoomed in on the cards. The tour serves
`image::allocate` through its headless renderer, and `shot` takes in an
upload's answer between its two renders, as a window's redraw would.

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

**An arrow stops short of its card** (`board::links::ARROW_GAP`, 2 world
units, measured from the tip's round cap): on the card's edge the tip sat on
its border and was lost in a border of its own colour. In world units so it
scales with zoom rather than leaving zoomed-out links looking detached. The
gap is taken in `edge_geometry`, *before* routing, so all seven consumers get
the same ends; an end without an arrow stays on its port. The drag preview,
once it snaps, and the cluster links take the same tip.

**A link's line stops where the chevron's inner edges meet it** (`shaft`),
with the small wedge between its butt end and the chevron's inner corner
filled in its colour. A link is translucent, so run on to the point it lay
under the chevron and the tip drew denser than the link; just stopping short
left a speck of board inside the V. Line, wedge and chevron tile the tip,
each pixel covered once. Drawing only — the geometry still ends at the point,
and the line is *cut* from the routed path (`link_path_trimmed`), never routed
from ends slid back: that reshaped the whole curve, and the line left its own
hover ring.

**The tip's shape is the app's, which ends have one is the file's.** `fromEnd`
and `toEnd` are per link, in the board, as Obsidian stores them; the shape —
`config.arrow_tip`, a chevron or a filled triangle — has nowhere to go in that
format, so it's a preference, chosen beside the link style. A triangle's line
stops at its base, so it tiles the tip as the chevron's wedge does. How solid
every link is, coloured or not, is `config.link_opacity` (file-only); the
hover and selection highlights stay opaque.

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
  only by their header strip, and are excluded from clustering, from a
  marquee that doesn't cover them whole, ports, shadows — and, importantly,
  from every *obstacle* set
  (placement, auto-size neighbours, AI sibling stacking). A frame encloses
  other nodes, so counting it as occupied space pins the cards inside it.
  Dragging one carries whatever it encloses (`App::drag_riders`), which is also
  where multi-selection drags are resolved — as a set, so nothing that
  qualifies twice moves at double speed. Moving a stretch of board takes its
  groups: a marquee drawn round a whole group selects the group with its
  cards (`Board::nodes_in_rect`; one inside it still means its cards), and a
  cluster dragged at semantic zoom carries each group whose cards are all its
  own (`App::move_cluster`) — decided as the drag starts, or it picked up an
  empty group it was dragged across. Both used to leave the frames where they
  were. Auto-layout takes a group whole the
  same way (`layout::framed_layout`): a selected card stands for its group,
  which is laid out as one node with every link of what it encloses, and
  carries its inside as arranged — laid out card by card, a marquee's
  selection of a group's cards left the frame behind. Its own cards selected
  alone are laid out inside it, and the frame grows to hold them.
  `App::group_selection` creates one around two or more selected non-frame
  nodes (command palette, or the node context menu); `groupable_selection` is
  the single predicate all three call sites gate on. A new frame is labelled
  "Untitled group" (`model::UNTITLED_GROUP`), as Obsidian names one, and
  drops straight into naming it with that selected; double-clicking a frame's
  header reopens the caption. A caption cleared goes back to "Untitled group"
  rather than deleting the frame — unlike a drag-created card, which *is*
  taken back when left empty. A frame another tool left unnamed stays unnamed
  unless it's given a name: a double-click and an Esc isn't a change to the
  file.
- **The editor edits two things.** `EditTarget::Body` is a text node's text over
  the whole card; `EditTarget::Label` is a frame's caption along its header
  strip. They share the widget, the text-level undo and the key bindings —
  except that a caption is one line, as in Obsidian: Return ends it, and a
  line break pasted into it is a space (`model::one_line`, which `set_label`
  applies too). They differ in where the text comes from, where it goes back
  to, and which rectangle counts as "on screen" (`App::edit_rect`) — a frame
  is routinely bigger than the viewport, so fitting all of it to edit a
  caption would zoom the board out for nothing.
- **Ask AI works on a frame**, which has no incoming links to walk. Its contents
  are flattened into an outline instead (`ai::group_prompt`), preserving both
  hierarchies the user sees: links nest a card's targets under it, and a nested
  frame's contents nest under its caption. A node inside nested frames is
  attributed to the innermost one, so nothing is listed twice.
- **Group membership is positional, never stored.** The format has no children
  array and no parent pointer — only geometry — so `enclosed_by` recomputes it
  from centres whenever it matters. Dragging a node out of a frame is therefore
  just moving it, with no state to keep in sync, and adding a card to a group
  is just dropping it inside the bounds. Never invent a membership field: it
  would be ours alone, and Obsidian would ignore it.
- **A frame can be linked like any node**, so a port-drag off a group makes a
  connected card — that's how a question gets asked *about* a group. Its ports
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
  left-align per `center_card_text`. Keep new kinds consistent with whichever
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
  disk; the history used to be dropped. An edit, gesture or new card under way
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

`resize` changes the window's size, as dragging its corner does — the
`small_windows` scene walks the panels and menus down to 360 px wide.

**`growing_lines` is a check, run only when named** (`TOUR=growing_lines`,
two minutes): every keystroke of a set of texts — prose, lists, headings,
tables, CJK, emoji, right to left — typed a character at a time, in a few
zooms, snapped, and three keys to a frame, with each checked by the
editor's own layout for a card that hasn't grown to what it shows. It fails
on one, and writes what it caught to `report.txt`. A card running off the
window's edge is left out: it's edited where it's on screen, and scrolls
there.

**The tour doubles as a check that a refactor changed nothing**: render every
scene before and after, and compare the files. Two things to know first. Run
it twice before, because a few shots differ between runs of the same code —
streamed answers, zoom animations, the overlay's fade, a marquee by a few
pixels. And compare runs made the same way: a scene run alone (`TOUR=…`)
renders text a hair differently from the same scene in a full run, whose
earlier scenes have warmed the font and shaping caches.

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

**`click` holds perfectly still, and a hand doesn't.** A press on a card is a
drag from its first move, except over a link or a task's box, which allow a
few pixels — so a scene checking what a click on a card does uses
`hand_click`, which drifts a pixel between press and release. The task boxes
passed with `click` and never ticked in the app.

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
  highlights in its draw. Typing a card with no frames between keystrokes
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

**A second launch hands over to the copy running** (`summon::hand_over`),
over a Unix socket in the runtime directory — on Windows, in the user's temp
directory, through `uds_windows`, which gives Windows 10's own Unix sockets
std's API, so it's one protocol on every platform: it does what the summon key
does there, or shows the board and opens the one it was given, and exits. It used to kill every other copy, which lost an edit made inside the
autosave's debounce, restarted the app from its own launcher, and let a
throwaway `--config` run kill the everyday copy. So:

- **One copy per config file.** The socket's name carries a hash of the config
  path (`socket_name`, FNV-1a — std's hasher isn't stable across builds, and
  every build has to find every other), so a `--config` run starts alongside.
- **Another build, or the other mode, takes over**: the copy running is asked
  to quit, saves, and is killed by pid only if it hasn't gone in 5 s. A build
  is the version and the executable's path and mtime, read at launch, so a
  rebuild counts; the mode is the effective `windowed`, so a launch after
  changing "Run as a normal window" applies it. The same build launched again
  only toggles — `cargo run` without a change in between hides the board.
- **Nothing answering, on the default config**, is an older build or a hung
  one, and is killed as before.
- **The macOS app opened again isn't launched again**: LaunchServices puts
  the copy running in front and asks it to reopen, an Apple Event
  (`platform::listen_for_reopen`), taken as a second launch of the same
  build (`summon::reopens`). It arrives with the app in front already, so
  the summon hands the keyboard back on dismissal to the last other app
  that was.

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
  worse than a stale picture. How blurred is `config.overlay_blur` (file-only),
  a multiple of each picture's own — the wallpaper heavily, yours lightly —
  and the blur is also what lets a picture be prepared small: the less of it,
  the larger it's decoded (`preparation`), up to the screen at 0. Sharp costs
  about 0.7 s and a 12 MB PNG on a 3456 px screen, against 0.26 s for the
  default wallpaper: once, in the background, but it's why sharp isn't the
  default. macOS's live behind-window blur was tried too: no faster than the
  desktop, a second slower to summon, and greyed colours.
- **Linux** — Wayland can't hide a window in place, so hiding *closes* the
  window and summoning opens a fresh one. The overlay is borderless
  fullscreen, except on COSMIC, whose compositor draws a fullscreen window
  opaque: there it's maximized, stopping at the panel, so it can be
  see-through. **Nor can an app hear a key that
  isn't its own there**: `global-hotkey` grabs through X11, and XWayland hears
  a key only while one of its windows has focus, which ours never is. So under
  Wayland the summon key is asked of the desktop, through the GlobalShortcuts
  portal (`app::summon`; KDE Plasma, GNOME 48+, Hyprland), which confirms it in
  a dialog and keeps it in its own keyboard settings — the settings row shows
  the key the desktop names and says where to change it, and
  `config.summon_hotkey` is only what we suggest. Where there's no portal (sway,
  an older GNOME, COSMIC), a key bound to `thinkboard` in the desktop's own
  settings does it, since a second launch hands over (below); the launcher
  runs it too. On COSMIC the app adds that shortcut itself, once
  (`summon::cosmic`), to the file where COSMIC keeps every custom shortcut —
  and one that doesn't parse loses the user all of them. So it's read with
  the parser COSMIC uses, each shortcut kept as written, and what would be
  written is read back first: every shortcut that was there, unchanged, and
  ours, or nothing is written. The portal files the shortcut under the
  app id, `com.vestigecounty.ThinkBoard`, which is why the `.desktop` file and
  the window's `app_id` are named for it. Tray uses `ksni` (pure-Rust D-Bus, no
  GTK) so the zig cross-build works; the dialogs use the xdg-portal backend for
  the same reason. **What's copied stays with the app that copied it** — under
  X11, and under Wayland, which arboard reaches through XWayland — and arboard
  hands it over only while a `Clipboard` lives, so one is held for the app's
  life (`clipboard::system`). Opened and dropped per copy, a copy pasted as
  nothing wherever no clipboard manager took it. **A picture behind the
  overlay is drawn in the board's own window**, under the board
  (`platform::PICTURE_IN_WINDOW`), which is then opaque: Wayland gives an
  app no way to order one of its windows under another, as macOS's picture
  window is. It's prepared as macOS's are (`platform::picture`, shared), the
  board drawing the same prepared picture as the form's tile. The wallpaper
  is found per desktop (`platform::wallpaper`: GNOME's settings, KDE's
  appletsrc, Xfce's xfconf, COSMIC's own config) and offered only where it
  is, and is a JPEG or PNG — a slideshow or a JPEG XL isn't offered rather
  than shown black. **Inside a Flatpak** (`core::sandbox`), `$XDG_CONFIG_HOME`
  is the app's own folder, so what's read of the host's — Obsidian's vault
  list, COSMIC's shortcuts, a desktop's wallpaper settings — is read from
  `~/.config` (`host_config_home`), which the sandbox's home access reaches;
  each launch gets a runtime directory of its own, so the hand-over socket
  goes in the `app/<id>` one they share; COSMIC's shortcut runs `flatpak run`
  with the app id, the sandbox's binary being nowhere the host can run it;
  and the tray item skips owning a D-Bus name, which the sandbox won't let it.
  A default wallpaper under the host's `/usr/share` isn't in the sandbox, and
  isn't offered.
- **Windows** — release builds use the GUI subsystem (no console); the
  `no-console` feature forces that in any profile. A picture behind the
  overlay is drawn in the board's own window, as on Linux
  (`platform::PICTURE_IN_WINDOW`): one window to keep in order, and an
  opaque one, which doesn't need the see-through swapchain. The wallpaper is
  the file `SPI_GETDESKWALLPAPER` names, or — where that's gone or isn't a
  JPEG or PNG — the copy Windows keeps of the one it shows,
  `TranscodedWallpaper`, a JPEG with no extension, which is why the shared
  decoder reads a file's format from its content. **Every scroll arrives in
  lines** there, a precision touchpad's included — and X11's smooth
  scrolling in fractions of one — where the board pans on pixels and zooms
  on lines, so a two-finger swipe zoomed. A wheel's notch is a whole line,
  so off macOS a fraction of one is read as a trackpad and pans
  (`input::trackpad_lines`). A free-spinning wheel scrolls in fractions too,
  hence `config.pan_on_fractional_scroll` (file-only) to turn it off. Not on
  macOS, whose wheel lines are accelerated fractions and whose trackpads say
  pixels.

## Releasing

`git tag vX.Y.Z && git push origin vX.Y.Z` runs `.github/workflows/release.yml`,
which builds a universal macOS `.app`, a Windows zip, and for Linux, x86_64 and
aarch64 each, a tarball, a `.deb`, an `.rpm` and a Flatpak bundle. Push the one
tag, not `--tags`: older tags exist only locally. Tag releases for shipped
user-facing features — not for internal refactors. Add the release to
`<releases>` in `packaging/linux/…metainfo.xml` as it's tagged: software
centres show it.

**A running app says when a newer release is out** (`app::updates`, the
check in `core::update`): GitHub's latest release for the repo, asked at
launch and once a day — the app runs resident for days — its tag compared
with the build's version, a release marked `-dev` coming before its own. A
newer one is said in the board's bottom-right corner, small and quiet, with
its page to open and a × that holds for that release (`dismissed_update`);
the missing file's notice goes first. Only said: nothing downloads or
replaces itself. Settings' General tab has the switch, on by default, and
the version, always; the tray menu has the version, and `thinkboard
--version` says it. Installed by Homebrew (a Cellar in the executable's
path), the notice names `brew upgrade` instead; inside a Flatpak nothing's
asked, Flathub keeping it up to date. Under test nothing's asked — what's
out would decide the shot — and the tour says what is (`updates` scene).

**Linux packages.** The Linux builds run on Ubuntu 22.04 runners, Arm's on
GitHub's own, so the binary asks for glibc 2.35 and runs on the distros since.
The `.deb` and `.rpm` (cargo-deb, cargo-generate-rpm) carry the tarball's
files where a system install puts them, from `[package.metadata.deb]` and
`[package.metadata.generate-rpm]` in Cargo.toml: no repository, so they don't
update themselves. **The AUR's `thinkboard-bin`** is `packaging/aur` with the
version and the tarballs' checksums filled in, pushed with the key of the AUR
account that maintains it (the `AUR_SSH_KEY` secret); without it the job says
so and leaves the AUR be. **The Flatpak** (`packaging/flatpak`) builds from
source offline, as Flathub does, on the freedesktop runtime; its crate list
(`cargo-sources.json`) is generated from Cargo.lock as it runs, by
flatpak-builder-tools' generator pinned to a commit, and never committed, so
it can't go stale beside the lock. Flathub's copy of the manifest names a tag
instead of the checkout, and keeps a crate list beside it. A Flatpak that
fails to build doesn't hold the release back. The launcher and the metainfo
are validated on every push (`linux-packaging` in ci.yml).

After publishing, it points the **Homebrew tap** (`vestigecounty/homebrew-tap`,
checked out at `homebrew-tap/`, excluded like `demo/`) at the new Mac zip:
`packaging/homebrew/thinkboard.rb` with the version and checksum filled in,
pushed with a deploy key (the `HOMEBREW_TAP_KEY` secret). Change the formula
there; the tap's copy is overwritten every release.

**The `.app` is signed ad hoc** — sealed, but by nobody — so a download still
meets Gatekeeper's first-launch warning; only a paid Developer ID and
notarization remove it. A Homebrew install doesn't meet it, because Homebrew
doesn't quarantine what a *formula* installs. That's why the tap has a formula
rather than a cask: a cask is quarantined like any download, and Homebrew has
dropped the flag that skipped it. Testing the formula locally, set
`HOMEBREW_NO_AUTOREMOVE=1`: `brew uninstall` otherwise removes whatever
dependencies it decides are unneeded, the user's included.
