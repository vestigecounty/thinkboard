//! Application state, update/view logic, and lifecycle.

use std::collections::{HashMap, HashSet};
use std::path::{Path as FsPath, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use iced::widget::{
    Column, Row, Space, button, canvas as iced_canvas, column, container, image, mouse_area, row,
    shader, stack, text, text_editor, text_input,
};
use iced::window;
use iced::{
    Element, Fill, Font, Length, Padding, Point, Rectangle, Size, Subscription, Task, Theme, Vector,
};


use std::sync::LazyLock;

use global_hotkey::GlobalHotKeyManager;
use global_hotkey::hotkey::HotKey;

use crate::ai;
use crate::canvas::board::{self, Board};
use crate::canvas::camera::{self, Camera, Inertia};
use crate::canvas::hue_picker::HuePicker;
use crate::canvas::{markdown, md_edit, shadows, text_raster};
use crate::config;
use crate::document::cluster;
use crate::document::layout;
use crate::theme::Theme as ColorTheme;
use crate::document::model::{self, CanvasDocument, CanvasEdge, CanvasNode};
use crate::platform;

mod chrome;
mod clipboard;
mod command;
mod settings;
mod startup;
mod sync;
#[cfg(any(target_os = "macos", windows, target_os = "linux"))]
mod tray;

use clipboard::Clip;
use thinkboard_core::history::History;

pub(crate) use startup::run;
use startup::{config_path_arg, hotkey_toggles, is_windowed};
use sync::{file_mtime, should_reload, should_save, watch_file};

const FONT_SIZE: f32 = 17.0;
/// Horizontal inset (per side) of the editing overlay's text within the card,
/// matching read mode's `MD_PADDING` so entering edit mode doesn't reflow.
const EDITOR_INSET: f32 = 8.0;

/// The find bar's text input, so opening the bar can focus it.
static SEARCH_INPUT: LazyLock<iced::advanced::widget::Id> =
    LazyLock::new(|| iced::advanced::widget::Id::new("board-search"));

/// Duration (seconds) of the hue picker's open (unroll) transition.
const PICKER_REVEAL: f32 = 0.18;

/// Duration (seconds) of the auto-layout ease-to-position animation.
const LAYOUT_ANIM: f32 = 0.4;

/// Node color swatches offered in the context menu: Obsidian's canonical
/// canvas presets `"1"`–`"6"` (red, orange, yellow, green, cyan, purple),
/// stored and rendered through `model::parse_color` so they round-trip
/// natively and stay consistent with the display swatch.
const NODE_PALETTE: [&str; 6] = ["1", "2", "3", "4", "5", "6"];

/// The embedded scratch board shown when no real canvas is configured. A
/// throwaway playground: never written to disk, reset on every launch. Its
/// button nodes (carrying a `tb_action` field) create or open a real board.
const SCRATCH_CANVAS: &str = include_str!("welcome.canvas");

/// A configured tint (an Obsidian preset "1"–"6") as a node color value, or
/// `None` for "0"/blank — meaning "don't colorize". Used for the AI trigger and
/// response-node tints.
fn config_color(code: &str) -> Option<String> {
    (code != "0" && !code.is_empty()).then(|| code.to_owned())
}

/// Transparent, text-only styling for chrome buttons (the find bar's ‹ › ×),
/// using theme colors: `base` normally, `accent` (with a faint wash) on hover.
fn chrome_button_style(
    status: iced::widget::button::Status,
    base: iced::Color,
    accent: iced::Color,
) -> iced::widget::button::Style {
    use iced::widget::button::Status;
    iced::widget::button::Style {
        background: matches!(status, Status::Hovered | Status::Pressed)
            .then(|| iced::Background::Color(iced::Color { a: 0.14, ..accent })),
        text_color: match status {
            Status::Disabled => iced::Color { a: 0.35, ..base },
            Status::Hovered | Status::Pressed => accent,
            Status::Active => base,
        },
        border: iced::Border {
            radius: 6.0.into(),
            ..Default::default()
        },
        ..iced::widget::button::Style::default()
    }
}

/// Builds a `"Ctrl+Shift+K"`-style hotkey string from a captured key + modifiers,
/// in the token format the registrar's parser accepts (it uppercases and takes
/// `"K"`, `"F12"`, `"Space"`, `"Super"`, …). Returns `None` for a bare modifier
/// or an unidentified key — meaning "no complete combination yet".
fn hotkey_string(
    key: &iced::keyboard::Key,
    physical: iced::keyboard::key::Physical,
    modifiers: iced::keyboard::Modifiers,
) -> Option<String> {
    use iced::keyboard::key::{Key, Named};

    let token = match key {
        Key::Named(
            Named::Shift
            | Named::Control
            | Named::Alt
            | Named::Super
            | Named::Meta
            | Named::Hyper,
        ) => return None,
        Key::Named(named) => format!("{named:?}"),
        // Record the latin letter for the physical position (the same trick the
        // in-app shortcuts use), so a letter recorded on a non-QWERTY layout
        // still maps to the physical Code the global hotkey actually fires on.
        // Fall back to the layout character for keys with no latin mapping.
        Key::Character(c) => key
            .to_latin(physical)
            .map_or_else(|| c.to_uppercase(), |latin| latin.to_uppercase().to_string()),
        Key::Unidentified => return None,
    };

    let mut parts: Vec<String> = Vec::new();
    if modifiers.control() {
        parts.push("Ctrl".to_owned());
    }
    if modifiers.alt() {
        parts.push("Alt".to_owned());
    }
    if modifiers.shift() {
        parts.push("Shift".to_owned());
    }
    if modifiers.logo() {
        parts.push("Super".to_owned());
    }
    parts.push(token);
    Some(parts.join("+"))
}

/// Find-bar navigation keys, subscribed while the search is open: Enter →
/// next, Shift+Enter → previous, Esc → close. (The text input keeps typing; a
/// raw subscription handles these reliably regardless of focus, and doing them
/// all here avoids double-firing with `on_submit`.)
fn search_key_event(
    event: iced::Event,
    _status: iced::event::Status,
    _window: window::Id,
) -> Option<Message> {
    use iced::keyboard::key::{Key, Named};

    let iced::Event::Keyboard(iced::keyboard::Event::KeyPressed { key, modifiers, .. }) = event
    else {
        return None;
    };
    match key {
        Key::Named(Named::Enter) if modifiers.shift() => Some(Message::SearchPrev),
        Key::Named(Named::Enter) => Some(Message::SearchNext),
        Key::Named(Named::Escape) => Some(Message::CloseSearch),
        _ => None,
    }
}

/// Esc closes the in-board settings modal (its form widgets get everything else).
fn settings_key_event(
    event: iced::Event,
    _status: iced::event::Status,
    _window: window::Id,
) -> Option<Message> {
    use iced::keyboard::key::{Key, Named};
    matches!(
        event,
        iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
            key: Key::Named(Named::Escape),
            ..
        })
    )
    .then_some(Message::CloseSettingsOverlay)
}

/// Filters raw events while recording a summon hotkey: forwards each key press so
/// `App::capture_hotkey` can turn one into the shortcut. Subscribed only while
/// recording (see `subscription`).
/// Whether `path` is an Obsidian canvas we can open (case-insensitive
/// extension) — used to filter drag-and-drop.
fn is_canvas_file(path: &FsPath) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("canvas"))
}

/// Maps a `.canvas` file dropped onto the window to an open request. Other
/// files are ignored (we only open canvases).
fn file_drop_event(
    event: iced::Event,
    _status: iced::event::Status,
    _window: window::Id,
) -> Option<Message> {
    let iced::Event::Window(window::Event::FileDropped(path)) = event else {
        return None;
    };
    is_canvas_file(&path).then_some(Message::OpenDropped(path))
}

fn capture_hotkey_event(
    event: iced::Event,
    _status: iced::event::Status,
    _window: window::Id,
) -> Option<Message> {
    if let iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
        key,
        physical_key,
        modifiers,
        ..
    }) = event
    {
        Some(Message::HotkeyCaptured(key, physical_key, modifiers))
    } else {
        None
    }
}

/// The system UI font (SF Pro on macOS, Segoe UI on Windows), matching what
/// the OS renders with. Its faces are loaded from disk at startup
/// ([`platform::UI_FONT_FILES`]); iced's own default resolves the generic
/// sans-serif family to a different font entirely.
pub const UI_FONT: Font = Font::with_name(platform::UI_FONT_NAME);

/// Velocity samples older than this are stale — releasing the button after
/// holding still should not coast.
const VELOCITY_FRESHNESS: Duration = Duration::from_millis(120);

/// How long a freshly rasterized text bitmap is drawn invisibly (warming
/// the GPU upload) before replacing the old one. iced uploads images larger
/// than 2 MB on a worker thread and draws nothing until that finishes, so
/// swapping handles immediately would blank the node for a few frames.
const SNAPSHOT_WARM_UP: Duration = Duration::from_millis(300);

/// After warm-up, a re-rasterized snapshot cross-fades in over this window
/// (drawn on top of the outgoing one) before being promoted, so a sharper
/// re-raster eases in instead of popping.
const SNAPSHOT_FADE: Duration = Duration::from_millis(140);

/// Dashboard-style overlay entrance/exit animation lengths.
const ENTER_DURATION: f32 = 0.45;
const EXIT_DURATION: f32 = 0.35;

/// Zoom-in + fade of a freshly created node.
pub const SPAWN_DURATION: Duration = Duration::from_millis(280);

/// Autosave poll interval while there are pending changes (the debounce
/// itself is `config.autosave_debounce_ms`).
const SAVE_POLL: Duration = Duration::from_millis(250);
/// Cadence of the pending-AI dot animation.
const AI_DOTS_INTERVAL: Duration = Duration::from_millis(400);
/// Minimum gap between auto-relayouts while a streaming answer grows.
const AI_RELAYOUT_THROTTLE: Duration = Duration::from_millis(350);
/// Longest side (px) an image is downscaled to before being sent to the model.
/// Vision encoders (e.g. Gemma's SigLIP at 896²) downsize internally, so larger
/// only bloats the request.
const AI_IMAGE_MAX_PX: u32 = 1024;
/// How long ignored ghost suggestions linger before auto-dismissing.
const GHOST_TTL: Duration = Duration::from_secs(10);

/// After summoning the overlay, ignore Space-change notifications for this long.
/// Activating/ordering the panel onto the current Space itself emits an
/// active-Space-change, which would otherwise trip the auto-dismiss and hide the
/// board we just brought up. A real user switch away happens well after this.
const SPACE_DISMISS_COOLDOWN: Duration = Duration::from_millis(1200);
/// When (into the TTL) ghosts start fading out to telegraph the dismissal.
const GHOST_FADE_START: f32 = 3.0;
/// Duration (s) of a ghost card's fade-in when it appears.
const GHOST_FADE_IN: f32 = 0.18;
/// Multiplier on a trackpad pinch's incremental magnification (1.0 ≈ the OS's
/// native feel); the per-event zoom factor is `1 + magnification * this`.
const PINCH_SPEED: f32 = 1.0;

/// Ease time constant of the auto-zoom fly-to (its trigger/target zoom are in
/// config).
const FOCUS_TAU: f32 = 0.12;

/// Total padding (both sides together) around a text node's text block. The
/// node's min/max size bounds are in config.
const NODE_PADDING_X: f32 = 24.0;
const NODE_PADDING_Y: f32 = 20.0;

/// Smallest height a text node can be resized to when node scrollbars are on
/// (with them off, the floor is the fitted content height instead).
const MIN_SCROLLABLE_NODE_HEIGHT: f32 = 40.0;

/// The minimum height a text node may be resized to. Normally the fitted content
/// height (so text never clips); with scrollbars on, a small absolute floor —
/// the overflow scrolls instead. `content_height` is the rendered text extent.
fn resize_min_height(scrollbars: bool, content_height: f32) -> f32 {
    if scrollbars {
        MIN_SCROLLABLE_NODE_HEIGHT
    } else {
        (content_height + NODE_PADDING_Y).max(60.0)
    }
}

/// How much larger the whole scene starts during the entrance (and grows
/// to during the exit): the classic Dashboard full-scene zoom.
const SCENE_ZOOM: f32 = 0.25;











/// Chromeless editor: the rounded card drawn by the canvas shows through.
fn editor_style(_theme: &Theme, _status: text_editor::Status) -> text_editor::Style {
    text_editor::Style {
        background: iced::Color::TRANSPARENT.into(),
        border: iced::Border::default(),
        placeholder: iced::Color::from_rgba(1.0, 1.0, 1.0, 0.4),
        value: board::TEXT_COLOR,
        selection: iced::Color::from_rgba(0.35, 0.62, 1.0, 0.4),
    }
}

/// A color swatch in the node menu: a rounded, filled square whose border
/// brightens on hover.
fn swatch_style(color: iced::Color, status: button::Status) -> button::Style {
    let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
    button::Style {
        background: Some(color.into()),
        border: iced::Border {
            radius: 4.0.into(),
            width: if hovered { 2.0 } else { 1.0 },
            color: if hovered {
                iced::Color::WHITE
            } else {
                iced::Color::from_rgba(1.0, 1.0, 1.0, 0.35)
            },
        },
        ..button::Style::default()
    }
}

/// The hue-picker (wheel) swatch: same rounded shape and hover border as a
/// color swatch, but filled with a smooth rainbow.
fn wheel_style(status: button::Status) -> button::Style {
    let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
    let mut rainbow = iced::gradient::Linear::new(iced::Radians(std::f32::consts::FRAC_PI_2));
    for i in 0..=6 {
        rainbow = rainbow.add_stop(i as f32 / 6.0, model::hsv(i as f32 / 6.0, 1.0, 1.0));
    }
    button::Style {
        background: Some(iced::Background::Gradient(rainbow.into())),
        border: iced::Border {
            radius: 4.0.into(),
            width: if hovered { 2.0 } else { 1.0 },
            color: if hovered {
                iced::Color::WHITE
            } else {
                iced::Color::from_rgba(1.0, 1.0, 1.0, 0.35)
            },
        },
        ..button::Style::default()
    }
}

/// A small square toggle chip (the auto-layout direction arrows): a subtle
/// filled button whose fill and border brighten on hover.
fn chip_style(_theme: &Theme, status: button::Status) -> button::Style {
    let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
    button::Style {
        background: Some(
            iced::Color::from_rgba(1.0, 1.0, 1.0, if hovered { 0.16 } else { 0.06 }).into(),
        ),
        text_color: board::TEXT_COLOR,
        border: iced::Border {
            radius: 5.0.into(),
            width: 1.0,
            color: iced::Color::from_rgba(1.0, 1.0, 1.0, if hovered { 0.5 } else { 0.2 }),
        },
        ..button::Style::default()
    }
}

/// Rasterizes each text cluster's label into a bitmap (same Markdown path as
/// node labels). Cluster boxes are large and the font is fixed, so one raster
/// at the reference zoom stays crisp for the whole cluster-visible range.
/// Returns nothing in live-text mode, where labels are drawn from Markdown.
fn generate_cluster_snapshots(
    clusters: &[cluster::Cluster],
    reference_zoom: f32,
    theme: ColorTheme,
    center: bool,
    rasterize: bool,
) -> HashMap<usize, text_raster::Snapshot> {
    if !rasterize {
        return HashMap::new();
    }
    clusters
        .iter()
        .enumerate()
        .filter(|(_, c)| c.image_id.is_none() && !c.text.is_empty())
        .filter_map(|(index, c)| {
            let snapshot = text_raster::snapshot_markdown(
                &c.text,
                c.bounds.width,
                c.bounds.height,
                (c.bounds.width - cluster::PADDING * 2.0).max(20.0),
                c.font_size,
                theme,
                center,
                reference_zoom,
            )?;
            Some((index, snapshot))
        })
        .collect()
}

/// Toggles a Markdown wrapper (`marker`, e.g. `**` or `*`) around the editor's
/// selection: adds it when absent, removes it when the selection is already
/// wrapped (markers inside *or* immediately outside the selection). Leading and
/// trailing whitespace in the selection stays outside the markers, so
/// selecting `word1< word2>` bolds `word2`, not ` word2`. With no selection,
/// inserts an empty pair with the caret between them. Returns whether the text
/// changed. Line endings are assumed to be `\n` (as in canvas node text).
fn toggle_marker(content: &mut text_editor::Content, marker: &str) -> bool {
    let full = content.text();
    let cursor = content.cursor();
    let Some(anchor) = cursor.selection else {
        // No selection: insert an empty pair and put the caret inside it.
        content.perform(text_editor::Action::Edit(text_editor::Edit::Paste(
            std::sync::Arc::new(format!("{marker}{marker}")),
        )));
        for _ in 0..marker.chars().count() {
            content.perform(text_editor::Action::Move(text_editor::Motion::Left));
        }
        return true;
    };

    let a = offset_of(&full, anchor);
    let b = offset_of(&full, cursor.position);
    let (start, end) = (a.min(b), a.max(b));

    let Some((new_full, sel_start, sel_end)) = toggle_wrap(&full, start, end, marker) else {
        return false; // nothing, or only whitespace, selected
    };

    // Re-select the affected core so a repeated shortcut toggles it back.
    *content = text_editor::Content::with_text(&new_full);
    content.move_to(text_editor::Cursor {
        position: position_at(&new_full, sel_end),
        selection: Some(position_at(&new_full, sel_start)),
    });
    true
}

/// Pure core of [`toggle_marker`]: given the full text and a selection byte
/// range `[start, end)`, returns the new text and the byte range to re-select,
/// or `None` when the selection has no non-whitespace core to wrap.
fn toggle_wrap(
    full: &str,
    start: usize,
    end: usize,
    marker: &str,
) -> Option<(String, usize, usize)> {
    // Shrink the selection to its non-whitespace core, leaving surrounding
    // spaces outside the markers.
    let selected = &full[start..end];
    let core_start = start + (selected.len() - selected.trim_start().len());
    let core_end = end - (selected.len() - selected.trim_end().len());
    if core_start >= core_end {
        return None;
    }
    let core = &full[core_start..core_end];

    let (cut_start, cut_end, replacement) = if wrapped_by(core, marker) {
        // Markers sit inside the selection — strip them.
        let inner = &core[marker.len()..core.len() - marker.len()];
        (core_start, core_end, inner.to_owned())
    } else if ends_with_marker(&full[..core_start], marker)
        && starts_with_marker(&full[core_end..], marker)
    {
        // Markers sit just outside the selection (e.g. a double-clicked word) —
        // remove them.
        (
            core_start - marker.len(),
            core_end + marker.len(),
            core.to_owned(),
        )
    } else {
        (core_start, core_end, format!("{marker}{core}{marker}"))
    };

    let mut new_full = String::with_capacity(full.len() + 2 * marker.len());
    new_full.push_str(&full[..cut_start]);
    new_full.push_str(&replacement);
    new_full.push_str(&full[cut_end..]);
    Some((new_full, cut_start, cut_start + replacement.len()))
}

/// Byte offset in `text` of a `(line, column)` position (`\n`-separated lines,
/// `column` a byte index within its line).
fn offset_of(text: &str, pos: text_editor::Position) -> usize {
    let mut offset = 0;
    for (i, line) in text.split('\n').enumerate() {
        if i == pos.line {
            return offset + pos.column.min(line.len());
        }
        offset += line.len() + 1; // + '\n'
    }
    text.len()
}

/// Inverse of [`offset_of`]: the `(line, column)` position of a byte offset.
fn position_at(text: &str, offset: usize) -> text_editor::Position {
    let prefix = &text[..offset.min(text.len())];
    text_editor::Position {
        line: prefix.matches('\n').count(),
        column: prefix.len() - prefix.rfind('\n').map_or(0, |p| p + 1),
    }
}

/// Whether `core` both starts and ends with `marker` (so it can be unwrapped).
fn wrapped_by(core: &str, marker: &str) -> bool {
    core.len() >= 2 * marker.len()
        && starts_with_marker(core, marker)
        && ends_with_marker(core, marker)
}

/// `s` starts with exactly `marker` — not a longer run of `*` (so `*` doesn't
/// match one star of a `**` bold marker).
fn starts_with_marker(s: &str, marker: &str) -> bool {
    s.starts_with(marker) && !s[marker.len()..].starts_with('*')
}

/// `s` ends with exactly `marker`, not part of a longer run of `*`.
fn ends_with_marker(s: &str, marker: &str) -> bool {
    s.ends_with(marker) && !s[..s.len() - marker.len()].ends_with('*')
}

fn node_world_rect(node: &CanvasNode) -> Rectangle {
    Rectangle::new(
        Point::new(node.x, node.y),
        Size::new(node.width, node.height),
    )
}

/// Snaps a newly created node's position and size to the grid. A `grid` of
/// `0.0` (snap-to-grid off) leaves it unchanged. Text nodes are further kept
/// aligned as they grow by [`App::autosize_node`]; this handles the initial
/// placement and fixed-size nodes (e.g. pasted images).
fn snap_node_geometry(node: &mut CanvasNode, grid: f32) {
    let p = crate::snap::point(Point::new(node.x, node.y), grid);
    node.x = p.x;
    node.y = p.y;
    (node.width, node.height) = crate::snap::size(node.width, node.height, grid);
}

/// Whether a text bitmap rendered for `(old_text, old_size)` still depicts a
/// node now showing `(new_text, new_size)`. A text node's bitmap is a function
/// of its text and size only — zoom and theme are global, and the node's color
/// tint is drawn separately from the bitmap — so unchanged text and size means
/// the already-uploaded texture can be reused across an undo/redo instead of
/// re-rasterizing (which blanks the node for a frame while the new texture
/// uploads). Size is compared with the same 0.5px slack as `autosize_node`.
fn text_bitmap_reusable(
    old_text: &Option<String>,
    old_size: (f32, f32),
    new_text: &Option<String>,
    new_size: (f32, f32),
) -> bool {
    old_text == new_text
        && (old_size.0 - new_size.0).abs() < 0.5
        && (old_size.1 - new_size.1).abs() < 0.5
}

/// The smallest translation `(dx, dy)` that moves a block of rects (each grown
/// by `MARGIN`) so none overlaps any `obstacles` rect, searched outward in
/// rings from the block's current position. `(0, 0)` if it already fits, or if
/// nothing clears within the search. Rays start due east and sweep round, so a
/// gap near the natural (to-the-right) position is preferred — keeping a placed
/// block compact rather than flung far in one direction.
fn nearest_free_offset(block: &[Rectangle], obstacles: &[Rectangle]) -> (f32, f32) {
    const MARGIN: f32 = 20.0;
    const STEP: f32 = 48.0;
    const RAYS: usize = 16;
    const RINGS: usize = 32;
    let clears = |dx: f32, dy: f32| {
        block.iter().all(|r| {
            let moved = Rectangle::new(
                Point::new(r.x - MARGIN + dx, r.y - MARGIN + dy),
                Size::new(r.width + 2.0 * MARGIN, r.height + 2.0 * MARGIN),
            );
            !obstacles.iter().any(|other| moved.intersects(other))
        })
    };
    if clears(0.0, 0.0) {
        return (0.0, 0.0);
    }
    for ring in 1..=RINGS {
        let radius = STEP * ring as f32;
        for ray in 0..RAYS {
            let angle = std::f32::consts::TAU * ray as f32 / RAYS as f32;
            let (dx, dy) = (radius * angle.cos(), radius * angle.sin());
            if clears(dx, dy) {
                return (dx, dy);
            }
        }
    }
    (0.0, 0.0)
}

/// Drops `removed` from an index-keyed map and shifts higher keys down by
/// one, matching a `Vec::remove(removed)` on the nodes.
fn shift_indices<V>(map: &mut HashMap<usize, V>, removed: usize) {
    *map = map
        .drain()
        .filter(|(k, _)| *k != removed)
        .map(|(k, v)| (if k > removed { k - 1 } else { k }, v))
        .collect();
}

/// Loads the image at `path`, downscales it so its longest side is at most
/// [`AI_IMAGE_MAX_PX`], and returns it as a base64 `data:` URL for the model.
/// `None` if the file can't be read or decoded (e.g. an unsupported format).
fn encode_image_data_url(path: &FsPath) -> Option<String> {
    use base64::Engine as _;
    let png = downscale_to_png(::image::open(path).ok()?)?;
    Some(format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&png)
    ))
}

/// PNG bytes of `img`, first shrunk so its longest side is at most
/// [`AI_IMAGE_MAX_PX`] (aspect preserved; never upscaled).
fn downscale_to_png(img: ::image::DynamicImage) -> Option<Vec<u8>> {
    let img = if img.width().max(img.height()) > AI_IMAGE_MAX_PX {
        img.resize(
            AI_IMAGE_MAX_PX,
            AI_IMAGE_MAX_PX,
            ::image::imageops::FilterType::Triangle,
        )
    } else {
        img
    };
    let mut png = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut png), ::image::ImageFormat::Png)
        .ok()?;
    Some(png)
}

/// Whether `edge` touches a transient node (so it's transient too).
fn is_transient_edge(edge: &CanvasEdge, transient: &HashSet<String>) -> bool {
    transient.contains(&edge.from_node) || transient.contains(&edge.to_node)
}

/// A document snapshot for undo/redo with the transient AI nodes (and the edges
/// touching them) left out — in-flight generations don't belong in history.
fn committed_snapshot(
    nodes: &[CanvasNode],
    edges: &[CanvasEdge],
    transient: &HashSet<String>,
) -> DocSnapshot {
    DocSnapshot {
        nodes: nodes.iter().filter(|n| !transient.contains(&n.id)).cloned().collect(),
        edges: edges.iter().filter(|e| !is_transient_edge(e, transient)).cloned().collect(),
    }
}

/// Ids of AI streams to cancel when the nodes in `removed` are deleted: a
/// stream is cancelled if its anchor (first) note or the note it is currently
/// writing to has been removed. `streams` yields `(stream_id, current_id)`.
fn ai_streams_to_cancel<'a>(
    streams: impl Iterator<Item = (&'a str, &'a str)>,
    removed: &HashSet<String>,
) -> Vec<String> {
    streams
        .filter(|(id, current)| removed.contains(*id) || removed.contains(*current))
        .map(|(id, _)| id.to_owned())
        .collect()
}

/// Bridges native workspace/Space-change notifications into `SpaceChanged`
/// messages: registers the platform handler to feed a channel this forwards.
/// Only subscribed where [`platform::WATCHES_WORKSPACE`] is true.
fn space_change_stream() -> iced::futures::stream::BoxStream<'static, Message> {
    use iced::futures::StreamExt;
    iced::stream::channel(
        4,
        |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
            use iced::futures::SinkExt;
            let (sender, mut receiver) = iced::futures::channel::mpsc::unbounded();
            platform::set_workspace_change_handler(move || {
                let _ = sender.unbounded_send(());
            });
            while receiver.next().await.is_some() {
                let _ = output.send(Message::SpaceChanged).await;
            }
        },
    )
    .boxed()
}

/// Forwards native trackpad pinch gestures (macOS) into the update loop as
/// `PinchZoom` messages. A no-op stream where the platform doesn't report them.
fn magnify_stream() -> iced::futures::stream::BoxStream<'static, Message> {
    use iced::futures::StreamExt;
    iced::stream::channel(
        32,
        |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
            use iced::futures::SinkExt;
            let (sender, mut receiver) = iced::futures::channel::mpsc::unbounded();
            platform::set_magnify_handler(move |magnification, x, y| {
                let _ = sender.unbounded_send((magnification, x, y));
            });
            while let Some((magnification, x, y)) = receiver.next().await {
                let _ = output
                    .send(Message::PinchZoom { magnification, x, y })
                    .await;
            }
        },
    )
    .boxed()
}

/// Forwards the native summon gesture (macOS four-finger swipe) into the update
/// loop as `ToggleOverlay`. Registers the handler; the tap is started/stopped
/// separately via `platform::set_summon_gesture_enabled` per the config.
fn gesture_stream() -> iced::futures::stream::BoxStream<'static, Message> {
    use iced::futures::StreamExt;
    iced::stream::channel(
        4,
        |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
            use iced::futures::SinkExt;
            let (sender, mut receiver) = iced::futures::channel::mpsc::unbounded();
            platform::set_gesture_handler(move || {
                let _ = sender.unbounded_send(());
            });
            while receiver.next().await.is_some() {
                let _ = output.send(Message::ToggleOverlay).await;
            }
        },
    )
    .boxed()
}



#[derive(Debug, Clone)]
pub(crate) enum Message {
    ViewportResized(Size),
    Pan { delta: Vector, track: bool },
    EndPan,
    CancelInertia,
    ZoomBy { factor: f32, cursor: Point },
    /// Smooth-scroll the camera to center a world point (the wayfinding arrow).
    FocusWorld(Point),
    /// A trackpad pinch step (macOS native gesture): `magnification` is the
    /// incremental delta; `(x, y)` is the event's window-space location
    /// (bottom-left origin — the handler flips y into canvas space).
    PinchZoom { magnification: f32, x: f32, y: f32 },
    MoveNode { index: usize, position: Point },
    MoveCluster { index: usize, delta: Vector },
    ClusterDragEnd,
    ResizeNode { index: usize, anchor: Point, cursor: Point },
    ResizeEnd,
    /// Set a node's vertical scroll offset (world px from the top), from dragging
    /// its scrollbar. Clamped to the content overflow on use.
    SetNodeScroll { id: String, offset: f32 },
    AddEdge { from: usize, side: &'static str, to: usize, to_side: &'static str },
    CreateConnectedNode { from: usize, side: &'static str, world: Point },
    OpenContextMenu { screen: Point, world: Point },
    OpenNodeMenu { screen: Point, node: usize },
    OpenLinkMenu { screen: Point, edge: usize },
    CloseContextMenu,
    CreateNode { world: Point },
    /// Copy the selected nodes (and the edges among them) to the clipboard.
    CopyNodes,
    /// Paste clipboard contents (copied nodes, else text or image) at `world`.
    Paste { world: Point },
    /// Sets the selected nodes' color (a canvas value like "1" or "#4a90e2");
    /// `None` clears it back to default.
    SetNodeColor(Option<String>),
    /// Asks the AI about the selected node(s) (tinting them the trigger color).
    AskAi,
    /// A streamed AI event for the response node with the given id.
    Ai(String, ai::Event),
    /// Advances the pending-AI dot animation.
    AiTick,
    /// Auto-layout the selected nodes (Mermaid-style layered graph) flowing in
    /// the given direction.
    AutoLayout(layout::Direction),
    /// Toggles the "auto-layout on add" setting (persisted to config).
    ToggleAutoLayout,
    /// Toggles snap-to-grid (persisted to config).
    ToggleSnap,
    /// Toggles the AI completions ("ghost" suggestions) setting (persisted).
    ToggleAiCompletions,
    /// Suggestion texts arrived for the node `anchor`; show them as ghosts
    /// branching off `parent`. `generation` is discarded if it's no longer current
    /// (the request was dismissed or superseded).
    Suggestions {
        generation: u64,
        parent: String,
        anchor: String,
        texts: Vec<String>,
    },
    /// Accept the ghost suggestion at this index into a real node.
    AcceptGhost(usize),
    /// Reposition a ghost card (drag) without accepting it. `position` is the
    /// new world-space top-left.
    MoveGhost { index: usize, position: Point },
    /// Discard the ghost suggestion at this index (its "×").
    DismissGhost(usize),
    /// The cursor moved on/off the ghost cards; pauses/refreshes their timeout.
    HoverGhost(bool),
    /// Discard all pending ghost suggestions.
    DismissGhosts,
    /// Expands the node menu into the hue picker.
    ExpandColorPicker,
    SelectNode(Option<usize>),
    /// Toggle a node's membership in the selection (Cmd-click).
    ToggleNode(usize),
    /// Replace the selection with these nodes (marquee result).
    SelectNodes(Vec<usize>),
    SelectEdge(Option<usize>),
    HoverEdge(Option<usize>),
    DeleteSelected,
    /// Begin editing a node's text. `at` is the world-space click position when
    /// started from a double-click, so the caret lands under the cursor.
    StartEditing { index: usize, at: Option<Point> },
    /// Place the deferred caret once the editor has laid out (see [`Editing::pending_caret`]).
    PlaceEditorCaret,
    StopEditing,
    EditorAction(text_editor::Action),
    /// Wrap the editor selection in a Markdown marker (`**` bold, `*` italic).
    WrapSelection(&'static str),
    /// Text-level undo/redo within the current edit session.
    UndoEdit,
    RedoEdit,
    Tick(Instant),
    SaveTick,
    FileChanged,
    SnapshotsReady(Vec<(usize, text_raster::Snapshot)>),
    ClustersReady(cluster::Clustering, HashMap<usize, text_raster::Snapshot>),
    NodeDragEnd,
    /// Open a link (from a note's text) in the default browser.
    OpenUrl(String),
    Undo,
    Redo,
    ToggleOverlay,
    WindowShown,
    SpaceChanged,
    /// Open (or focus) the in-board find bar (Cmd/Ctrl+F).
    OpenSearch,
    /// The find query changed (search-as-you-type).
    SearchInput(String),
    /// Cycle to the next / previous match (Enter / Shift+Enter, ‹ ›).
    SearchNext,
    SearchPrev,
    /// Close the find bar (Esc / ×).
    CloseSearch,
    /// Command palette (Cmd/Ctrl+Shift+P): open, close, filter, move the
    /// selection, run a clicked command, or run the highlighted one (Enter).
    OpenCommandPalette,
    CloseCommandPalette,
    CommandPaletteInput(String),
    CommandPaletteMove(i32),
    RunCommand(command::CommandId),
    RunSelectedCommand,
    /// Settings as an in-board modal (from the command palette): open / close.
    OpenSettingsOverlay,
    CloseSettingsOverlay,
    /// Swallow a click (e.g. on the settings modal panel) so it doesn't fall
    /// through to the backdrop.
    Noop,
    /// Open the settings window (from the tray), or focus it if already open.
    OpenSettings,
    /// A settings-form edit (see `settings::Msg`); applied and persisted.
    Settings(settings::Msg),
    /// A "Browse…" file dialog resolved (a path, or `None` if cancelled).
    SettingsBrowsed(settings::Field, Option<PathBuf>),
    /// A create/open-board action was invoked — from a scratch-board button
    /// node (its `tb_action`) or the tray menu. Values: "new_board" /
    /// "open_board".
    CanvasAction(String),
    /// The overlay has hidden; now open the deferred create/open-board dialog.
    OpenBoardDialog,
    /// A create/open board dialog resolved. `create` writes a fresh empty
    /// canvas at the path first; either way the board is opened.
    BoardChosen { path: Option<PathBuf>, create: bool },
    /// A `.canvas` file was dropped onto the window — open it.
    OpenDropped(PathBuf),
    /// Start capturing a key combination for the summon hotkey.
    RecordHotkey,
    /// A key was pressed while capturing the summon hotkey.
    HotkeyCaptured(
        iced::keyboard::Key,
        iced::keyboard::key::Physical,
        iced::keyboard::Modifiers,
    ),
    /// The OS asked to close a window (close button). Routed by id: the settings
    /// window just closes; the main window quits.
    WindowCloseRequested(window::Id),
    /// A window finished closing; clears our settings-window handle.
    WindowClosed(window::Id),
    Exit,
    /// The OS asked to close the window (close button / Cmd+Q). Flush the
    /// autosave and quit — bypasses the dismiss animation.
    Quit,
}

struct Editing {
    index: usize,
    content: text_editor::Content,
    /// Text-level undo/redo for this session (iced's editor has none). Whole
    /// snapshots, coalesced by a short pause between edits.
    undo: Vec<String>,
    redo: Vec<String>,
    last_snapshot: Instant,
    /// A world-space click position whose caret placement is deferred until the
    /// editor has laid out (its buffer is shaped at `font_size: 1.0` until then,
    /// so a point→caret hit-test is only valid after the first frame). Set when
    /// editing starts from a double-click; consumed by [`Message::PlaceEditorCaret`].
    pending_caret: Option<Point>,
}

/// Edits within this window coalesce into one undo step.
const EDIT_COALESCE: Duration = Duration::from_millis(600);

impl Editing {
    fn new(index: usize, text: &str) -> Self {
        Editing {
            index,
            content: text_editor::Content::with_text(text),
            undo: Vec::new(),
            redo: Vec::new(),
            last_snapshot: Instant::now(),
            pending_caret: None,
        }
    }

    /// Records the pre-edit text as an undo point. `force` snapshots even mid
    /// burst (for discrete actions like bold); otherwise it coalesces edits
    /// that happen close together.
    fn snapshot(&mut self, force: bool) {
        let now = Instant::now();
        let paused = now.duration_since(self.last_snapshot) > EDIT_COALESCE;
        self.last_snapshot = now;
        if force || paused || self.undo.is_empty() {
            let text = self.content.text();
            if self.undo.last() != Some(&text) {
                self.undo.push(text);
                self.redo.clear();
            }
        }
    }

    fn undo(&mut self) -> Option<String> {
        let previous = self.undo.pop()?;
        self.redo.push(self.content.text());
        self.content = text_editor::Content::with_text(&previous);
        Some(previous)
    }

    fn redo(&mut self) -> Option<String> {
        let next = self.redo.pop()?;
        self.undo.push(self.content.text());
        self.content = text_editor::Content::with_text(&next);
        Some(next)
    }
}

/// Smoothly approaches `target` zoom, keeping `cursor` anchored.
struct ZoomAnimation {
    target: f32,
    cursor: Point,
    last_tick: Instant,
}

struct App {
    nodes: Vec<CanvasNode>,
    edges: Vec<CanvasEdge>,
    images: HashMap<String, image::Handle>,
    /// User configuration loaded at startup.
    config: config::Config,
    /// Node/card colors, optionally read from an Obsidian theme.
    theme: ColorTheme,
    /// The `.canvas` file we load from and autosave to.
    canvas_path: PathBuf,
    /// The embedded scratch board is active: a throwaway playground that's
    /// never saved, so all disk writes and the external-change watch are
    /// suppressed. Cleared once a real board is created or opened.
    scratch: bool,
    /// Vault root that image `file` paths resolve against (Obsidian records them
    /// vault-relative). Derived from `canvas_path` at load.
    base_path: PathBuf,
    /// Top-level canvas keys we don't model, preserved on save.
    doc_extra: serde_json::Map<String, serde_json::Value>,
    /// Unsaved local changes are pending.
    doc_dirty: bool,
    /// Time of the last change, for the save debounce.
    last_edit: Instant,
    /// Last file mtime we know about (set on load/save/reload); external
    /// writes are detected by comparison, our own writes adopt the new one.
    file_mtime: Option<SystemTime>,
    /// Semantic-zoom cluster summaries; recomputed in the background after
    /// geometry/edge changes.
    clusters: Vec<cluster::Cluster>,
    /// Arrows between connected clusters, shown in cluster mode.
    cluster_links: Vec<cluster::ClusterLink>,
    /// Cluster label bitmaps, keyed by cluster index — same rendering path
    /// as node labels. Rasterized once at the reference zoom.
    cluster_snapshots: HashMap<usize, text_raster::Snapshot>,
    recluster_pending: bool,
    /// A change arrived while a recluster was in flight; recompute again.
    recluster_dirty: bool,
    camera: Camera,
    /// Top safe-area inset (px), refreshed on resize; passed to the board so the
    /// top wayfinding button clears the macOS notch.
    top_safe_inset: f32,
    cache: iced_canvas::Cache,
    inertia: Option<Inertia>,
    zoom_animation: Option<ZoomAnimation>,
    pan_velocity: Vector,
    last_pan: Option<Instant>,
    editing: Option<Editing>,
    /// Refit content to the viewport on resize until the user interacts.
    interacted: bool,
    overlay: bool,
    overlay_configured: bool,
    /// When the overlay was last (re)configured on summon. Used to ignore the
    /// Space-change our own summon emits (see [`SPACE_DISMISS_COOLDOWN`]).
    overlay_shown_at: Option<Instant>,
    /// The main overlay window. Tracked explicitly (not via `window::latest`,
    /// which returns the settings window when it's open) so hide/summon always
    /// target the board. Set by the boot task; `None` on Linux while hidden
    /// (there the overlay is closed, not hidden).
    main_window: Option<window::Id>,
    /// The settings window, while it's open (a separate decorated window opened
    /// from the tray). `None` when closed.
    settings_window: Option<window::Id>,
    /// Raw text of the settings window's text/number fields, so typing shows
    /// exactly what was typed. Seeded from the config when the window opens.
    settings_drafts: HashMap<settings::Field, String>,
    /// Whether the settings window's collapsible "Advanced" section is expanded.
    /// Collapsed by default so the essentials (theme, canvas, hotkey) stay short.
    settings_advanced_open: bool,
    /// The in-board find (Cmd/Ctrl+F), while its bar is open. `None` = closed.
    search: Option<Search>,
    /// The command palette (Cmd/Ctrl+Shift+P), when open. Mutually exclusive
    /// with the find bar.
    command_palette: Option<command::CommandPalette>,
    /// The settings form rendered as an in-board modal (opened from the command
    /// palette), instead of the separate settings window (opened from the tray).
    settings_overlay: bool,
    /// A create/open-board dialog waiting for the overlay to finish hiding
    /// (the always-on-top window would otherwise cover the native dialog).
    /// `Some(true)` = create, `Some(false)` = open.
    pending_board_create: Option<bool>,
    /// While true, the next key combination pressed is captured as the summon
    /// hotkey (the settings "Record" button).
    hotkey_recording: bool,
    /// The global-hotkey registration, kept so the summon shortcut can be
    /// re-registered live when changed in settings (no restart). `None` in
    /// windowed mode or where global hotkeys aren't available (e.g. Wayland).
    hotkey_manager: Option<GlobalHotKeyManager>,
    /// The hotkey currently registered with `hotkey_manager`, to unregister on
    /// change.
    registered_hotkey: Option<HotKey>,
    show_fps: bool,
    /// Bitmap renditions of text nodes — the only way text is drawn on the
    /// board. GPU-scaled during zoom; re-rasterized only when the settled
    /// zoom drifts outside the bitmap's quality band or the text changes,
    /// so gestures never touch the glyph rasterizer.
    text_snapshots: HashMap<usize, text_raster::Snapshot>,
    /// Freshly rasterized bitmaps being upload-warmed before they replace
    /// the entries in `text_snapshots`.
    staging_snapshots: HashMap<usize, StagedSnapshot>,
    /// Vertical scroll offset (world px from the top) per node id, for nodes
    /// resized below their content while `node_scrollbars` is on. Clamped to the
    /// content overflow on use; stale entries clamp to 0 harmlessly.
    node_scroll: HashMap<String, f32>,
    /// A background rasterization is in flight; acts as a debounce.
    raster_pending: bool,
    /// Ids of AI response nodes still awaiting their first token; they show the
    /// animated dots until the answer starts streaming.
    ai_pending: HashSet<String>,
    /// Current dot count (1–3) for the pending-AI animation.
    ai_dots: usize,
    /// Last time a streaming answer triggered an auto-relayout (throttle).
    ai_last_relayout: Instant,
    /// Live AI answer streams, keyed by first-note id (the `Message::Ai` tag).
    ai_streams: HashMap<String, AiStream>,
    /// Pending AI requests' input chains, each shimmered on its own clock (see
    /// `board::AiFlow`) so concurrent generations animate independently.
    ai_flows: Vec<board::AiFlow>,
    /// Copilot-style suggestion cards awaiting accept/dismiss (including a "…"
    /// placeholder while suggestions are being fetched).
    ghosts: Vec<board::Ghost>,
    /// When the current ghost batch appeared, for the auto-dismiss timeout
    /// (reset by hover).
    ghosts_since: Instant,
    /// When the current cards became visible, for the fade-in (not reset by
    /// hover, so hovering doesn't retrigger it).
    ghosts_appeared: Instant,
    /// Set once the user engages a ghost (moves/accepts/dismisses one), which
    /// cancels the auto-dismiss so they stick around while being used.
    ghosts_pinned: bool,
    /// Whether the cursor is over a ghost card: pauses the fade/timeout, and
    /// leaving refreshes it to the full TTL.
    ghost_hovered: bool,
    /// Bumped on each completion request and whenever ghosts are dismissed, so
    /// a suggestion result that arrives after a dismissal (or is superseded) is
    /// ignored instead of popping back up.
    completion_gen: u64,
    /// Spawn animation start times of freshly created nodes; when one
    /// finishes, `pending_edit` may open its editor.
    spawning: HashMap<usize, Instant>,
    pending_edit: Option<usize>,
    /// A just-created node that is removed again if its editing session
    /// ends with no text.
    fresh_node: Option<usize>,
    /// Open right-click context menu: its screen position and the world
    /// point that was clicked.
    context_menu: Option<ContextMenu>,
    /// Smooth camera fly-to (e.g. focusing a newly added node).
    camera_focus: Option<CameraFocus>,
    /// Selected nodes (highlighted, deletable with Delete/Backspace). Built
    /// by clicking (single), Cmd-clicking (toggle), or a marquee drag.
    selected: Vec<usize>,
    /// Selected edge/link (highlighted, deletable). Mutually exclusive with
    /// `selected`.
    selected_edge: Option<usize>,
    /// Edge under the cursor (highlighted). App state so the cached edge
    /// layer can draw it behind the nodes.
    hovered_edge: Option<usize>,
    /// Extra color swatch shown in the open node menu: the clipboard's color
    /// if it holds one, else the last custom-picked color. Computed on menu
    /// open so `view` does no clipboard I/O.
    extra_swatch: Option<String>,
    /// The most recent custom (non-preset) color applied, offered again as the
    /// extra swatch when the clipboard has no color.
    last_picked_color: Option<String>,
    /// Whether the node menu is showing the expanded hue picker.
    color_picker_open: bool,
    /// Start of the hue picker's open transition, while it's playing.
    picker_anim: Option<Instant>,
    /// In-progress auto-layout animation easing nodes to new positions.
    layout_anim: Option<LayoutAnim>,
    /// Undo/redo history of document snapshots. Deletes and completed edits
    /// record an entry; drags do not.
    history: History<DocSnapshot>,
    /// Pre-edit snapshot captured while editing an existing node, committed
    /// to the undo stack only if the text actually changed.
    edit_undo: Option<DocSnapshot>,
    phase: Phase,
    /// 0 = hidden, 1 = fully shown; the board eases node positions, node
    /// alpha, and the dim from this.
    reveal: f32,
}

#[derive(Clone, Copy)]
struct ContextMenu {
    screen: Point,
    kind: ContextMenuKind,
}

#[derive(Clone, Copy)]
enum ContextMenuKind {
    /// Empty canvas: offers "Add node" at the clicked world point.
    Canvas { world: Point },
    /// A node: offers "Delete" for the selected node.
    Node,
    /// A link: offers "Delete" for that edge.
    Link,
}

#[derive(Clone, Copy)]
struct CameraFocus {
    target_zoom: f32,
    target_offset: Vector,
    last_tick: Instant,
}

/// An in-board text find (Cmd/Ctrl+F). `matches` is every occurrence across the
/// text nodes, in board order; `current` indexes it.
struct Search {
    query: String,
    matches: Vec<Match>,
    current: usize,
}

/// One search hit: a character range within a node's text.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Match {
    node: usize,
    start: usize,
    len: usize,
}

/// Case-insensitive substring search over the text nodes, in board order. Ranges
/// are character offsets/lengths (for highlighting). Empty query → no matches.
fn find_matches(nodes: &[CanvasNode], query: &str) -> Vec<Match> {
    // Fold to a single lowercase char per source char, keeping a 1:1 mapping so
    // the offsets stay valid indices into the original text.
    let fold = |c: char| c.to_lowercase().next().unwrap_or(c);
    let needle: Vec<char> = query.chars().map(fold).collect();
    if needle.is_empty() {
        return Vec::new();
    }

    let mut matches = Vec::new();
    for (node, canvas_node) in nodes.iter().enumerate() {
        if canvas_node.kind != "text" {
            continue;
        }
        let Some(text) = canvas_node.text.as_deref() else {
            continue;
        };
        let hay: Vec<char> = text.chars().map(fold).collect();
        let mut start = 0;
        while start + needle.len() <= hay.len() {
            if hay[start..start + needle.len()] == needle[..] {
                matches.push(Match { node, start, len: needle.len() });
                start += needle.len(); // non-overlapping
            } else {
                start += 1;
            }
        }
    }
    matches
}

/// An auto-layout animation: each node eases from its old to its new position.
struct LayoutAnim {
    started: Instant,
    /// `(node_index, from, to)` top-left positions.
    moves: Vec<(usize, (f32, f32), (f32, f32))>,
}

/// A live AI answer stream, keyed by its first note's id: the source it
/// branches from, the note currently receiving text (a `---` in the answer
/// rolls this over to a new sibling), how many notes it has spawned, and a
/// handle to abort the in-flight request (e.g. when a response node is deleted).
struct AiStream {
    source: String,
    current: String,
    count: usize,
    handle: iced::task::Handle,
    /// Every node this generation has created (first note + any split
    /// siblings). They are transient — excluded from undo snapshots — until the
    /// stream finishes, so an unrelated action's snapshot doesn't capture a
    /// half-generated "…" node.
    nodes: Vec<String>,
}

/// A document state for undo/redo. The whole node/edge set is small enough
/// that snapshotting is simpler and safer than tracking per-field diffs.
struct DocSnapshot {
    nodes: Vec<CanvasNode>,
    edges: Vec<CanvasEdge>,
}

/// Cap on undo history depth.
const MAX_UNDO: usize = 100;

pub struct StagedSnapshot {
    pub snapshot: text_raster::Snapshot,
    since: Instant,
    /// Cross-fade opacity: 0 while the texture warms, then ramps to 1 over the
    /// fade window, at which point the snapshot is promoted.
    pub opacity: f32,
}

/// Overlay lifecycle: the scene zooms in while the dim eases up, and the
/// exact inverse plays before the window hides — like the old macOS
/// Dashboard. The app stays resident; F15 summons it back from `Hidden`.
enum Phase {
    Entering { started: Instant },
    Running,
    Exiting { started: Instant },
    /// Resident-and-hidden, re-summoned by the hotkey (macOS/Windows) or the
    /// tray's Show/Hide (all platforms).
    Hidden,
}

impl App {
    fn new(boot: startup::Boot) -> (Self, Task<Message>) {
        let config = config::Config::load(config_path_arg().as_deref());
        let windowed = is_windowed(&config);
        // Register the global summon hotkey, keeping the manager so it can be
        // re-registered when the setting changes (rather than on restart). Skipped
        // entirely in windowed mode.
        let (hotkey_manager, registered_hotkey) =
            startup::register_summon_hotkey(&config.summon_hotkey, windowed);
        // Read the theme CSS once for both its colors/font names and any fonts
        // it embeds as base64 WOFF2.
        let (mut theme, embedded_fonts) = match config.theme_css.as_deref() {
            Some(path) => match std::fs::read_to_string(path) {
                Ok(css) => (ColorTheme::from_css(&css), crate::theme::embedded_fonts(&css)),
                Err(_) => {
                    eprintln!("theme: could not read {path}");
                    (ColorTheme::default(), Vec::new())
                }
            },
            None => (ColorTheme::default(), Vec::new()),
        };
        // Point the theme's font names at the embedded files' real family names.
        crate::theme::resolve_embedded_names(&mut theme, &embedded_fonts);

        // A real canvas loads from (and autosaves to) its file; the scratch
        // board is parsed from the embedded string and never touches disk.
        let (document, canvas_path, base_path, file_mtime, scratch) = match boot {
            startup::Boot::Canvas(path) => {
                let document =
                    CanvasDocument::load(&path).unwrap_or_else(|error| panic!("{error}"));
                let base_path = vault_root(&path, &document);
                let file_mtime = file_mtime(&path);
                (document, path, base_path, file_mtime, false)
            }
            startup::Boot::Scratch => {
                let document = scratch_document();
                let base_path = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                (document, PathBuf::new(), base_path, None, true)
            }
        };

        // Register the theme's embedded fonts in the raster font DB (for layout
        // + rasterized draw), with a synthesized face at each themed weight for
        // variable fonts so body/bold spans match the theme font rather than a
        // system one (see synth_weight_faces). iced's own font system (the live
        // draw path) is loaded separately, in `startup::run` via the builder —
        // see there for why not `iced::font::load`.
        let mut embedded_fonts = embedded_fonts;
        embedded_fonts.extend(crate::theme::synth_weight_faces(
            &embedded_fonts,
            &[theme.weight_normal, theme.weight_bold],
        ));
        if !embedded_fonts.is_empty() {
            text_raster::set_embedded_fonts(embedded_fonts);
        }
        text_raster::warm_up();

        let images = document
            .nodes
            .iter()
            .filter_map(|node| {
                let file = node.file.as_ref()?;
                let path = base_path.join(file);
                if !path.exists() {
                    eprintln!(
                        "warning: image not found: {} (vault root: {})",
                        path.display(),
                        base_path.display()
                    );
                }
                Some((node.id.clone(), image::Handle::from_path(path)))
            })
            .collect();

        let clustering = cluster::compute(&document.nodes, &document.edges, &theme);
        let cluster_snapshots = generate_cluster_snapshots(
            &clustering.clusters,
            config.cluster_zoom,
            theme,
            config.center_node_text,
            config.rasterize_text,
        );
        // Captured before `config` is moved into the struct below.
        let (min_zoom, max_zoom) = (config.min_zoom, config.max_zoom);

        let app = Self {
            nodes: document.nodes,
            edges: document.edges,
            images,
            config,
            theme,
            canvas_path,
            scratch,
            base_path,
            doc_extra: document.extra,
            doc_dirty: false,
            last_edit: Instant::now(),
            file_mtime,
            clusters: clustering.clusters,
            cluster_links: clustering.links,
            cluster_snapshots,
            recluster_pending: false,
            recluster_dirty: false,
            camera: Camera {
                min_zoom,
                max_zoom,
                ..Camera::default()
            },
            cache: iced_canvas::Cache::new(),
            inertia: None,
            zoom_animation: None,
            pan_velocity: Vector::new(0.0, 0.0),
            last_pan: None,
            editing: None,
            interacted: false,
            overlay: !windowed,
            overlay_configured: false,
            overlay_shown_at: None,
            top_safe_inset: 0.0,
            main_window: None,
            settings_window: None,
            settings_drafts: HashMap::new(),
            settings_advanced_open: false,
            search: None,
            command_palette: None,
            settings_overlay: false,
            pending_board_create: None,
            hotkey_recording: false,
            hotkey_manager,
            registered_hotkey,
            show_fps: std::env::args().any(|arg| arg == "--fps"),
            text_snapshots: HashMap::new(),
            staging_snapshots: HashMap::new(),
            node_scroll: HashMap::new(),
            raster_pending: false,
            ai_pending: HashSet::new(),
            ai_dots: 1,
            ai_last_relayout: Instant::now(),
            ai_streams: HashMap::new(),
            ai_flows: Vec::new(),
            ghosts: Vec::new(),
            ghosts_since: Instant::now(),
            ghosts_appeared: Instant::now(),
            ghosts_pinned: false,
            ghost_hovered: false,
            completion_gen: 0,
            spawning: HashMap::new(),
            pending_edit: None,
            fresh_node: None,
            context_menu: None,
            camera_focus: None,
            selected: Vec::new(),
            selected_edge: None,
            hovered_edge: None,
            extra_swatch: None,
            last_picked_color: None,
            color_picker_open: false,
            picker_anim: None,
            layout_anim: None,
            history: History::new(MAX_UNDO),
            edit_undo: None,
            phase: if windowed {
                Phase::Running
            } else {
                Phase::Entering {
                    started: Instant::now(),
                }
            },
            reveal: if windowed { 1.0 } else { 0.0 },
        };
        (app, Task::none())
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::ViewportResized(size) => {
                self.camera.viewport = size;
                self.top_safe_inset = platform::top_safe_inset();
                if self.overlay && !self.overlay_configured {
                    self.overlay_configured = true;
                    platform::configure_overlay_window();
                    // Create the tray icon now that the app is running (macOS
                    // needs the run loop live). Leaked so it lives for the
                    // process; the OS removes it on exit.
                    #[cfg(any(target_os = "macos", windows))]
                    if let Some(tray_icon) = tray::create() {
                        std::mem::forget(tray_icon);
                    }
                    // Restart the entrance clock now that the window is
                    // actually on screen.
                    self.phase = Phase::Entering {
                        started: Instant::now(),
                    };
                    // Start listening for the summon gesture if opted in (the
                    // tap must be armed on the main thread, which we're on).
                    if platform::SUPPORTS_GESTURES {
                        platform::set_summon_gesture_enabled(self.config.summon_gesture);
                    }
                }
                // The overlay resizes itself to the screen right after
                // opening; keep refitting until the user takes over.
                if !self.interacted {
                    self.camera.fit(self.content_bounds());
                }
                self.cache.clear();
                return self.refresh_snapshots();
            }
            Message::Pan { delta, track } => {
                self.interacted = true;
                self.inertia = None;
                self.camera_focus = None;
                self.camera.offset = self.camera.offset + delta;
                if track {
                    let now = Instant::now();
                    if let Some(last) = self.last_pan {
                        let dt = now.duration_since(last).as_secs_f32().max(0.001);
                        // Blend with previous samples so one slow event just
                        // before release doesn't kill the glide.
                        let sample = delta * (1.0 / dt);
                        self.pan_velocity = self.pan_velocity * 0.5 + sample * 0.5;
                    }
                    self.last_pan = Some(now);
                }
                self.cache.clear();
                // Panning can reveal nodes with stale bitmaps.
                return Task::batch([self.refresh_snapshots(), self.exit_edit_if_offscreen()]);
            }
            Message::EndPan => {
                let fresh = self
                    .last_pan
                    .is_some_and(|last| last.elapsed() < VELOCITY_FRESHNESS);
                if fresh {
                    self.inertia = Inertia::start(self.pan_velocity, self.config.pan_inertia);
                }
                self.last_pan = None;
                self.pan_velocity = Vector::new(0.0, 0.0);
            }
            Message::CancelInertia => {
                self.inertia = None;
            }
            Message::FocusWorld(world) => self.focus_on_world(world),
            Message::ZoomBy { factor, cursor } => {
                self.zoom_by(factor, cursor);
                return self.exit_edit_if_offscreen();
            }
            Message::PinchZoom { magnification, x, y } => {
                // The gesture reports a bottom-left-origin window point; flip it
                // into iced's top-left canvas space to zoom around the pinch.
                let cursor = Point::new(x, (self.camera.viewport.height - y).max(0.0));
                let factor = (1.0 + magnification * PINCH_SPEED).max(0.1);
                self.zoom_by(factor, cursor);
                return self.exit_edit_if_offscreen();
            }
            Message::MoveNode { index, position } => {
                self.interacted = true;
                if let Some(node) = self.nodes.get(index) {
                    // Dragging a node in a multi-selection moves the whole
                    // group by the same delta; otherwise just the one node.
                    let delta = position - Point::new(node.x, node.y);
                    if self.selected.contains(&index) && self.selected.len() > 1 {
                        for &i in &self.selected {
                            if let Some(node) = self.nodes.get_mut(i) {
                                node.x += delta.x;
                                node.y += delta.y;
                            }
                        }
                    } else if let Some(node) = self.nodes.get_mut(index) {
                        node.x = position.x;
                        node.y = position.y;
                    }
                }
                self.mark_dirty();
                self.cache.clear();
            }
            Message::MoveCluster { index, delta } => {
                self.interacted = true;
                if let Some(cluster) = self.clusters.get(index) {
                    // Move every member node and the card itself; the drag
                    // ends with a recluster to resettle bounds.
                    for &m in &cluster.members.clone() {
                        if let Some(node) = self.nodes.get_mut(m) {
                            node.x += delta.x;
                            node.y += delta.y;
                        }
                    }
                    if let Some(cluster) = self.clusters.get_mut(index) {
                        cluster.bounds.x += delta.x;
                        cluster.bounds.y += delta.y;
                    }
                    self.mark_dirty();
                    self.cache.clear();
                }
            }
            Message::ClusterDragEnd => {
                return self.schedule_recluster();
            }
            Message::ResizeNode { index, anchor, cursor } => {
                self.interacted = true;
                let Some(node) = self.nodes.get(index) else {
                    return Task::none();
                };
                let is_text = node.kind == "text";
                let (drag_w, drag_h) = ((cursor.x - anchor.x).abs(), (cursor.y - anchor.y).abs());

                let (width, height) = if is_text {
                    let width = drag_w.max(150.0);
                    // Height floor: normally the content fitted at this width, so
                    // text never overflows. Measure the *rendered* Markdown
                    // (`measure_markdown`) so the floor matches what draws — it
                    // insets to the render column and accounts for heading sizes
                    // and bold widening, which plain `measure` under-counts,
                    // leaving the card a line short (text then clips). With node
                    // scrollbars on, drop the content floor so the node can shrink
                    // past its text and the overflow scrolls.
                    let text = node.text.clone().unwrap_or_default();
                    let (_, text_height) =
                        text_raster::measure_markdown(text.trim(), width - 12.0, FONT_SIZE, &self.theme);
                    let min_height = resize_min_height(self.config.node_scrollbars, text_height);
                    let height = drag_h.max(min_height).max(MIN_SCROLLABLE_NODE_HEIGHT);
                    (width, height)
                } else {
                    // Image nodes keep their aspect ratio so the picture never
                    // stretches. They're created aspect-correct (fit_image_size),
                    // so preserving the current ratio holds it true.
                    let aspect = node.width / node.height;
                    aspect_locked_size(aspect, drag_w, drag_h, 80.0, 60.0)
                };

                let node = &mut self.nodes[index];
                node.x = if cursor.x >= anchor.x {
                    anchor.x
                } else {
                    anchor.x - width
                };
                node.y = if cursor.y >= anchor.y {
                    anchor.y
                } else {
                    anchor.y - height
                };
                node.width = width;
                node.height = height;

                // The stale bitmap would stretch; draw live text until the
                // resize ends and a fresh raster arrives.
                self.text_snapshots.remove(&index);
                self.staging_snapshots.remove(&index);
                self.mark_dirty();
                self.cache.clear();
            }
            Message::ResizeEnd => {
                return Task::batch([self.refresh_snapshots(), self.schedule_recluster()]);
            }
            Message::SetNodeScroll { id, offset } => {
                // Stored raw; clamped to the content overflow where it's read
                // (the board), so a later text/size change can't leave it stuck.
                self.node_scroll.insert(id, offset.max(0.0));
                self.cache.clear();
            }
            Message::AddEdge { from, side, to, to_side } => {
                self.interacted = true;
                let endpoints = self
                    .nodes
                    .get(from)
                    .map(|s| s.id.clone())
                    .zip(self.nodes.get(to).map(|t| t.id.clone()));
                if let Some((from_id, to_id)) = endpoints {
                    self.push_undo();
                    self.edges.push(CanvasEdge {
                        id: model::random_id(),
                        from_node: from_id,
                        from_side: side.to_owned(),
                        to_node: to_id,
                        to_side: to_side.to_owned(),
                        extra: Default::default(),
                    });
                    // Connecting two nodes merges their graphs; re-lay-out if
                    // enabled (covered by the undo snapshot just recorded).
                    self.auto_relayout(to, false);
                    self.cache.clear();
                    self.mark_dirty();
                    return self.schedule_recluster();
                }
            }
            Message::CreateConnectedNode { from, side, world } => {
                self.interacted = true;
                self.dismiss_ghosts();
                let Some(source) = self.nodes.get(from) else {
                    return Task::none();
                };
                let source_id = source.id.clone();

                // Place the node so its connecting side sits at the release
                // point (matching the drag preview) and the body extends away
                // from the source — right of a left link, under a top link, etc.
                let to_side = board::opposite_side(side);
                let (w, h) = (self.config.new_node_min_width, self.config.new_node_min_height);
                let center = match to_side {
                    "left" => Point::new(world.x + w / 2.0, world.y),
                    "right" => Point::new(world.x - w / 2.0, world.y),
                    "top" => Point::new(world.x, world.y + h / 2.0),
                    "bottom" => Point::new(world.x, world.y - h / 2.0),
                    _ => world,
                };
                let index = self.spawn_text_node(center);
                self.edges.push(CanvasEdge {
                    id: model::random_id(),
                    from_node: source_id,
                    from_side: side.to_owned(),
                    to_node: self.nodes[index].id.clone(),
                    to_side: to_side.to_owned(),
                    extra: Default::default(),
                });
                self.auto_relayout(index, true);
            }
            Message::OpenContextMenu { screen, world } => {
                self.context_menu = Some(ContextMenu {
                    screen,
                    kind: ContextMenuKind::Canvas { world },
                });
            }
            Message::OpenNodeMenu { screen, node } => {
                // Right-clicking a node selects it (highlight on) and offers
                // colors + Delete.
                self.inertia = None;
                // Keep an existing multi-selection when right-clicking one of
                // its members; otherwise select just this node.
                if !self.selected.contains(&node) {
                    self.selected = vec![node];
                }
                self.selected_edge = None;
                self.color_picker_open = false;
                self.picker_anim = None;
                // Extra swatch: a color on the clipboard, else the last custom
                // color picked. Text-only clipboard read (cheap).
                self.extra_swatch = clipboard::read_text()
                    .and_then(|text| model::parse_css_color(text.trim()).map(model::color_to_hex))
                    .or_else(|| self.last_picked_color.clone());
                self.context_menu = Some(ContextMenu {
                    screen,
                    kind: ContextMenuKind::Node,
                });
                self.cache.clear();
            }
            Message::OpenLinkMenu { screen, edge } => {
                // Right-clicking a link selects it (highlight on) and offers
                // Delete.
                self.selected.clear();
                self.selected_edge = Some(edge);
                self.context_menu = Some(ContextMenu {
                    screen,
                    kind: ContextMenuKind::Link,
                });
                self.cache.clear();
            }
            Message::CloseContextMenu => {
                // Dismissing a node/link menu also clears its selection.
                match self.context_menu.map(|menu| menu.kind) {
                    Some(ContextMenuKind::Node) => self.selected.clear(),
                    Some(ContextMenuKind::Link) => {
                        self.selected_edge = None;
                        self.hovered_edge = None;
                    }
                    _ => {}
                }
                self.context_menu = None;
                self.color_picker_open = false;
                self.picker_anim = None;
                self.cache.clear();
            }
            Message::CreateNode { world } => {
                self.interacted = true;
                self.context_menu = None;
                self.dismiss_ghosts();
                let index = self.spawn_text_node(world);
                self.focus_on_node(index);
            }
            Message::CopyNodes => {
                self.copy_selected_nodes();
            }
            Message::Paste { world } => {
                self.interacted = true;
                self.context_menu = None;
                return self.paste_at(world);
            }
            Message::SetNodeColor(color) => {
                // Applies to every selected node (undoable); `None` clears the
                // color back to default. Closes the menu so the pick reads as
                // confirmed — one undo entry.
                let targets: Vec<usize> = self
                    .selected
                    .iter()
                    .copied()
                    .filter(|&i| i < self.nodes.len())
                    .collect();
                let changed = targets.iter().any(|&i| self.nodes[i].color != color);
                if changed {
                    self.push_undo();
                    for &i in &targets {
                        self.nodes[i].color = color.clone();
                    }
                    self.mark_dirty();
                }
                // Remember custom (non-preset) colors for the extra swatch.
                if let Some(c) = color.filter(|c| !NODE_PALETTE.contains(&c.as_str())) {
                    self.last_picked_color = Some(c);
                }
                self.context_menu = None;
                self.color_picker_open = false;
                self.picker_anim = None;
                self.selected.clear();
                self.cache.clear();
            }
            Message::AskAi => {
                if !self.config.ai_enabled() {
                    return Task::none();
                }
                let targets: Vec<usize> = self
                    .selected
                    .iter()
                    .copied()
                    .filter(|&i| i < self.nodes.len())
                    .collect();
                self.context_menu = None;
                self.color_picker_open = false;
                self.picker_anim = None;
                self.selected.clear();
                if targets.is_empty() {
                    return Task::none();
                }
                // One undo point per generation, captured before the trigger
                // tint and the response nodes — recorded even when re-asking an
                // already-tinted node, so each generation undoes on its own
                // instead of being lumped in with a previous one.
                self.push_undo();
                // Tint the clicked node(s) the trigger color (unless "0"), the
                // same feedback the color trigger gives.
                let trigger = config_color(&self.config.ai_trigger_color);
                for &i in &targets {
                    self.nodes[i].color = trigger.clone();
                }
                self.mark_dirty();
                let ai_tasks: Vec<Task<Message>> =
                    targets.iter().map(|&i| self.trigger_ai(i)).collect();
                self.cache.clear();
                return Task::batch(ai_tasks);
            }
            Message::Ai(stream_id, event) => {
                match event {
                    ai::Event::Delta(text) => {
                        // Route the chunk to the note currently receiving text.
                        let Some(target) = self.ai_streams.get(&stream_id).map(|s| s.current.clone())
                        else {
                            return Task::none();
                        };
                        let Some(index) = self.nodes.iter().position(|n| n.id == target) else {
                            return Task::none();
                        };
                        // The first chunk replaces the animated dots; append after.
                        if self.end_ai_pending(&target) {
                            self.nodes[index].text = Some(text);
                        } else {
                            let current = self.nodes[index].text.take().unwrap_or_default();
                            self.nodes[index].text = Some(current + &text);
                        }
                        self.autosize_node(index, true);
                        // The text/size just changed; drop the stale bitmap so it
                        // draws live until the fresh raster catches up.
                        self.invalidate_snapshot(index);
                        // Follow the growing card with a throttled relayout so
                        // filling cards don't overlap — always, regardless of
                        // the auto-layout setting. Tidies only this generation's
                        // cards, never the user's surrounding nodes.
                        if self.ai_last_relayout.elapsed() >= AI_RELAYOUT_THROTTLE {
                            self.ai_last_relayout = Instant::now();
                            if let Some((source, ids)) = self
                                .ai_streams
                                .get(&stream_id)
                                .map(|s| (s.source.clone(), s.nodes.clone()))
                            {
                                self.relayout_ai_siblings(&source, &ids);
                            }
                        }
                        self.mark_dirty();
                        self.cache.clear();
                        // Draw live while streaming (each Delta redraws); the
                        // final raster is taken once at Done, like a resize.
                        return Task::none();
                    }
                    ai::Event::Split => {
                        // Start a new sibling note off the same source, unless the
                        // note cap is reached — then keep appending to the current.
                        let Some(stream) = self.ai_streams.get(&stream_id) else {
                            return Task::none();
                        };
                        if stream.count >= self.config.max_ai_nodes {
                            return Task::none();
                        }
                        let source = stream.source.clone();
                        if let Some(new_id) = self.spawn_ai_node(&source, "")
                            && let Some(stream) = self.ai_streams.get_mut(&stream_id)
                        {
                            stream.current = new_id.clone();
                            stream.count += 1;
                            stream.nodes.push(new_id);
                        }
                        return self.refresh_snapshots();
                    }
                    ai::Event::Done => {
                        let Some(stream) = self.ai_streams.remove(&stream_id) else {
                            return Task::none();
                        };
                        // A still-pending first note means no text ever arrived.
                        if self.end_ai_pending(&stream_id)
                            && let Some(index) =
                                self.nodes.iter().position(|n| n.id == stream.current)
                        {
                            self.nodes[index].text = Some("(no response)".to_owned());
                            self.autosize_node(index, true);
                            self.invalidate_snapshot(index);
                        }
                        // Settle the layout at the final size, tidying only this
                        // generation's cards (the source and other nodes stay put).
                        self.relayout_ai_siblings(&stream.source, &stream.nodes);
                        return Task::batch([self.refresh_snapshots(), self.schedule_recluster()]);
                    }
                    ai::Event::Error(error) => {
                        self.end_ai_pending(&stream_id);
                        if let Some(stream) = self.ai_streams.remove(&stream_id)
                            && let Some(index) = self.nodes.iter().position(|n| n.id == stream.current)
                        {
                            self.nodes[index].text = Some(format!("⚠ {error}"));
                            self.autosize_node(index, true);
                            self.invalidate_snapshot(index);
                        }
                        self.mark_dirty();
                        self.cache.clear();
                        return self.refresh_snapshots();
                    }
                }
            }
            Message::AiTick => {
                if self.ai_pending.is_empty() {
                    return Task::none();
                }
                self.ai_dots = self.ai_dots % 3 + 1;
                let dots = ".".repeat(self.ai_dots);
                let pending: Vec<usize> = self
                    .ai_pending
                    .iter()
                    .filter_map(|id| self.nodes.iter().position(|n| &n.id == id))
                    .collect();
                for index in pending {
                    self.nodes[index].text = Some(dots.clone());
                    // Drawn live: without invalidating, the dots freeze at the
                    // first rendered frame's bitmap.
                    self.invalidate_snapshot(index);
                }
                self.cache.clear();
                return Task::none();
            }
            Message::ExpandColorPicker => {
                self.color_picker_open = true;
                self.picker_anim = Some(Instant::now());
            }
            Message::AutoLayout(dir) => {
                self.context_menu = None;
                self.color_picker_open = false;
                self.picker_anim = None;
                if self.selected.len() > 1 {
                    let subset = self.selected.clone();
                    self.apply_layered_layout(&subset, dir, true);
                }
            }
            Message::ToggleAutoLayout => {
                // Leaves the menu open so the checkbox state visibly updates.
                self.config.auto_layout = !self.config.auto_layout;
                self.config.save(config_path_arg().as_deref());
            }
            Message::ToggleSnap => {
                // Leaves the menu open so the checkbox state visibly updates.
                self.config.snap_to_grid = !self.config.snap_to_grid;
                self.config.save(config_path_arg().as_deref());
                self.cache.clear();
            }
            Message::ToggleAiCompletions => {
                self.config.ai_completions = !self.config.ai_completions;
                self.config.save(config_path_arg().as_deref());
                if !self.config.ai_completions {
                    self.dismiss_ghosts();
                }
            }
            Message::Suggestions {
                generation, parent, anchor, texts } => {
                // Drop a result the user already dismissed or superseded.
                if generation != self.completion_gen {
                    return Task::none();
                }
                let (w, h) = (self.config.new_node_min_width, self.config.new_node_min_height);
                // Anchor the batch to the "…" placeholder's (possibly dragged)
                // spot, falling back to just below the added node.
                let start = self
                    .ghosts
                    .iter()
                    .find(|g| g.placeholder)
                    .map(|g| g.rect.position())
                    .or_else(|| {
                        self.nodes.iter().find(|n| n.id == anchor).map(|n| {
                            let r = node_world_rect(n);
                            Point::new(r.x, r.y + r.height + 30.0)
                        })
                    });
                let Some(start) = start else {
                    self.dismiss_ghosts();
                    return Task::none();
                };
                self.ghosts.clear();
                // Lay the suggestions out from `start`, each nudged clear of
                // existing nodes and the sibling ghosts already placed.
                let mut placed: Vec<Rectangle> = Vec::new();
                let mut top = start.y;
                for text in texts {
                    let requested = Point::new(start.x + w / 2.0, top + h / 2.0);
                    let center = self.find_free_placement_avoiding(requested, w, h, &placed);
                    let ghost_rect =
                        Rectangle::new(Point::new(center.x - w / 2.0, center.y - h / 2.0), Size::new(w, h));
                    placed.push(ghost_rect);
                    self.ghosts.push(board::Ghost {
                        text,
                        rect: ghost_rect,
                        parent: parent.clone(),
                        placeholder: false,
                    });
                    top = ghost_rect.y + ghost_rect.height + 20.0;
                }
                // Fresh batch: start the clocks, un-pinned, and fade in.
                self.ghosts_since = Instant::now();
                self.ghosts_appeared = Instant::now();
                self.ghosts_pinned = false;
                self.cache.clear();
            }
            Message::AcceptGhost(index) => {
                if index >= self.ghosts.len() {
                    return Task::none();
                }
                self.ghosts_pinned = true; // engaged: keep the rest around
                let ghost = self.ghosts.remove(index);
                return self.accept_ghost(ghost);
            }
            Message::MoveGhost { index, position } => {
                if let Some(ghost) = self.ghosts.get_mut(index) {
                    ghost.rect.x = position.x;
                    ghost.rect.y = position.y;
                    self.ghosts_pinned = true; // arranging them cancels the timeout
                    self.cache.clear();
                }
            }
            Message::DismissGhost(index) => {
                if index < self.ghosts.len() {
                    // Dismissing the "…" placeholder cancels its in-flight
                    // request so the result won't arrive as ghosts later.
                    if self.ghosts[index].placeholder {
                        self.completion_gen = self.completion_gen.wrapping_add(1);
                    }
                    self.ghosts.remove(index);
                    self.ghosts_pinned = true;
                    self.cache.clear();
                }
            }
            Message::HoverGhost(over) => {
                // Pause the countdown while hovered; refresh to the full TTL on
                // enter/leave so a fading card comes back to full and restarts.
                self.ghost_hovered = over;
                self.ghosts_since = Instant::now();
            }
            Message::DismissGhosts => {
                self.dismiss_ghosts();
            }
            Message::SelectNode(index) => {
                // Grabbing a node also stops any coast.
                self.inertia = None;
                let new: Vec<usize> = index.into_iter().collect();
                if self.selected != new || self.selected_edge.is_some() {
                    self.selected = new;
                    self.selected_edge = None; // node and edge selection are exclusive
                    self.cache.clear();
                }
            }
            Message::ToggleNode(index) => {
                self.inertia = None;
                if let Some(pos) = self.selected.iter().position(|&i| i == index) {
                    self.selected.remove(pos);
                } else {
                    self.selected.push(index);
                }
                self.selected_edge = None;
                self.cache.clear();
            }
            Message::SelectNodes(indices) => {
                self.inertia = None;
                if self.selected != indices || self.selected_edge.is_some() {
                    self.selected = indices;
                    self.selected_edge = None;
                    self.cache.clear();
                }
            }
            Message::SelectEdge(index) => {
                self.inertia = None;
                if self.selected_edge != index || !self.selected.is_empty() {
                    self.selected_edge = index;
                    self.selected.clear(); // node and edge selection are exclusive
                    self.cache.clear();
                }
            }
            Message::HoverEdge(index) => {
                if self.hovered_edge != index {
                    self.hovered_edge = index;
                    self.cache.clear();
                }
            }
            Message::DeleteSelected => {
                self.context_menu = None;
                return self.delete_selected();
            }
            Message::StartEditing { index, at } => {
                self.dismiss_ghosts();
                return self.begin_editing(index, at);
            }
            Message::PlaceEditorCaret => {
                // Wait for any fly-to-fit (started in `begin_editing`) to settle,
                // so the editor is unclamped and its geometry final before we
                // hit-test — `pending_caret` stays set, so the frames subscription
                // fires again next frame.
                if self.camera_focus.is_none()
                    && let Some((index, world)) =
                        self.editing.as_ref().and_then(|e| Some((e.index, e.pending_caret?)))
                {
                    let local = self.editor_text_origin(index).map(|origin| {
                        let screen = self.camera.to_screen(world);
                        Point::new(screen.x - origin.x, screen.y - origin.y)
                    });
                    if let Some(editing) = self.editing.as_mut() {
                        editing.pending_caret = None;
                        if let Some(local) = local {
                            editing.content.perform(text_editor::Action::Click(local));
                        }
                    }
                    self.cache.clear();
                }
            }
            Message::StopEditing => {
                // Id of the node being edited, before any removal below.
                let edited_id = self
                    .editing
                    .as_ref()
                    .and_then(|editing| self.nodes.get(editing.index))
                    .map(|node| node.id.clone());
                // A freshly-added connected node worth asking Copilot about.
                let mut completion_anchor: Option<String> = None;
                // A drag-created node left without text disappears again.
                if let Some(editing) = &self.editing {
                    let index = editing.index;
                    let empty = self
                        .nodes
                        .get(index)
                        .is_some_and(|node| {
                            node.text.as_deref().unwrap_or("").trim().is_empty()
                        });
                    if self.fresh_node == Some(index) && empty {
                        // Fresh nodes are always last, so removal does not
                        // shift the indices keying the snapshot maps.
                        let id = self.nodes.remove(index).id;
                        self.edges
                            .retain(|edge| edge.from_node != id && edge.to_node != id);
                        self.text_snapshots.remove(&index);
                        self.staging_snapshots.remove(&index);
                        self.spawning.remove(&index);
                    } else {
                        // Editing done: drop the whitespace that only
                        // mattered while typing, and settle the size.
                        if let Some(node) = self.nodes.get_mut(index) {
                            if let Some(text) = &node.text {
                                let trimmed = text.trim();
                                if trimmed != text {
                                    node.text = Some(trimmed.to_owned());
                                }
                            }
                        }
                        // Editing done: fit the card to the rendered Markdown.
                        self.autosize_node(index, true);
                        // A fresh node with a parent → ask for sibling ideas.
                        if self.config.ai_completions
                            && self.config.ai_enabled()
                            && self.fresh_node == Some(index)
                        {
                            let id = self.nodes[index].id.clone();
                            if self.edges.iter().any(|e| e.to_node == id) {
                                completion_anchor = Some(id);
                            }
                        }
                    }
                }
                // Commit the pre-edit snapshot as an undo point only if the
                // edited node's text actually changed.
                if let (Some(snapshot), Some(id)) = (self.edit_undo.take(), edited_id) {
                    let old = snapshot
                        .nodes
                        .iter()
                        .find(|n| n.id == id)
                        .and_then(|n| n.text.clone());
                    let new = self.nodes.iter().find(|n| n.id == id).and_then(|n| n.text.clone());
                    if old != new {
                        self.history.record(snapshot);
                        // The card resized; re-lay-out its graph if enabled
                        // (covered by the undo snapshot just recorded).
                        if let Some(index) = self.nodes.iter().position(|n| n.id == id) {
                            self.auto_relayout(index, false);
                        }
                    }
                }
                self.fresh_node = None;
                self.editing = None;
                self.mark_dirty();
                self.cache.clear();
                let mut tasks = vec![self.refresh_snapshots(), self.schedule_recluster()];
                if let Some(anchor) = completion_anchor {
                    self.completion_gen = self.completion_gen.wrapping_add(1);
                    let generation = self.completion_gen;
                    self.spawn_placeholder_ghost(&anchor);
                    tasks.push(self.request_completions(anchor, generation));
                }
                return Task::batch(tasks);
            }
            Message::EditorAction(action) => {
                let edited = if let Some(editing) = &mut self.editing {
                    let is_edit = action.is_edit();
                    if is_edit {
                        editing.snapshot(false);
                    }
                    editing.content.perform(action);
                    is_edit.then(|| (editing.index, editing.content.text()))
                } else {
                    None
                };
                if let Some((index, text)) = edited {
                    if let Some(node) = self.nodes.get_mut(index) {
                        node.text = Some(text);
                    }
                    self.autosize_node(index, false);
                    self.mark_dirty();
                }
            }
            Message::WrapSelection(marker) => {
                let edited = if let Some(editing) = &mut self.editing {
                    editing.snapshot(true);
                    if toggle_marker(&mut editing.content, marker) {
                        Some((editing.index, editing.content.text()))
                    } else {
                        None
                    }
                } else {
                    None
                };
                if let Some((index, text)) = edited {
                    if let Some(node) = self.nodes.get_mut(index) {
                        node.text = Some(text);
                    }
                    self.autosize_node(index, false);
                    self.mark_dirty();
                }
            }
            Message::UndoEdit => self.apply_edit_history(false),
            Message::RedoEdit => self.apply_edit_history(true),
            Message::Tick(now) => {
                let mut follow_up = Task::none();

                // Expire ignored ghost suggestions once fully faded.
                if self.ghosts_expiring() && self.ghosts_since.elapsed() >= GHOST_TTL {
                    self.dismiss_ghosts();
                }

                // Stop driving frames once the hue picker has finished opening;
                // `view` reads the elapsed time to draw the unroll.
                if let Some(start) = self.picker_anim {
                    if now.duration_since(start).as_secs_f32() >= self.config.anim_secs(PICKER_REVEAL)
                    {
                        self.picker_anim = None;
                    }
                }
                match self.phase {
                    Phase::Entering { started } => {
                        let t = now.duration_since(started).as_secs_f32()
                            / self.config.anim_secs(ENTER_DURATION);
                        if t >= 1.0 {
                            self.reveal = 1.0;
                            self.phase = Phase::Running;
                        } else {
                            self.reveal = t;
                        }
                        self.cache.clear();
                    }
                    Phase::Exiting { started } => {
                        let t = now.duration_since(started).as_secs_f32()
                            / self.config.anim_secs(EXIT_DURATION);
                        if t >= 1.0 {
                            return if self.overlay {
                                self.hide_window()
                            } else {
                                self.save_and_exit()
                            };
                        }
                        self.reveal = 1.0 - t;
                        self.cache.clear();
                    }
                    Phase::Running | Phase::Hidden => {}
                }

                // Finished spawn animations; open the editor of the node
                // that was just dragged into existence.
                if !self.spawning.is_empty() {
                    let done: Vec<usize> = self
                        .spawning
                        .iter()
                        .filter(|(_, started)| {
                            now.duration_since(**started).as_secs_f32()
                                >= self.config.anim_secs(SPAWN_DURATION.as_secs_f32())
                        })
                        .map(|(index, _)| *index)
                        .collect();
                    for index in done {
                        self.spawning.remove(&index);
                        if self.pending_edit == Some(index) {
                            self.pending_edit = None;
                            follow_up = self.begin_editing(index, None);
                        }
                    }
                    self.cache.clear();
                }

                // Warm each pending texture invisibly, then ramp its cross-fade
                // opacity; once fully faded in, promote it to the active bitmap.
                let mut promote = Vec::new();
                let mut fading = false;
                for (index, staged) in self.staging_snapshots.iter_mut() {
                    let elapsed = now.duration_since(staged.since);
                    let opacity = if elapsed <= SNAPSHOT_WARM_UP {
                        0.0
                    } else {
                        ((elapsed - SNAPSHOT_WARM_UP).as_secs_f32() / SNAPSHOT_FADE.as_secs_f32())
                            .min(1.0)
                    };
                    if opacity != staged.opacity {
                        fading = true;
                    }
                    staged.opacity = opacity;
                    if opacity >= 1.0 {
                        promote.push(*index);
                    }
                }
                for index in &promote {
                    if let Some(staged) = self.staging_snapshots.remove(index) {
                        self.text_snapshots.insert(*index, staged.snapshot);
                    }
                }
                if fading || !promote.is_empty() {
                    self.cache.clear();
                }
                if let Some(inertia) = &mut self.inertia {
                    match inertia.tick(now) {
                        Some(delta) => self.camera.offset = self.camera.offset + delta,
                        None => self.inertia = None,
                    }
                    self.cache.clear();
                }
                if let Some(focus) = &mut self.camera_focus {
                    let dt = now.duration_since(focus.last_tick).as_secs_f32().min(0.1);
                    focus.last_tick = now;
                    let t = 1.0 - (-dt / FOCUS_TAU).exp();

                    let dz = focus.target_zoom - self.camera.zoom;
                    let dx = focus.target_offset.x - self.camera.offset.x;
                    let dy = focus.target_offset.y - self.camera.offset.y;
                    if dz.abs() < 0.001 && dx.abs() < 0.5 && dy.abs() < 0.5 {
                        self.camera.zoom = focus.target_zoom;
                        self.camera.offset = focus.target_offset;
                        self.camera_focus = None;
                    } else {
                        self.camera.zoom += dz * t;
                        self.camera.offset.x += dx * t;
                        self.camera.offset.y += dy * t;
                    }
                    self.cache.clear();
                }
                if let Some(animation) = &mut self.zoom_animation {
                    let dt = now
                        .duration_since(animation.last_tick)
                        .as_secs_f32()
                        .min(0.1);
                    animation.last_tick = now;

                    let progress = 1.0 - (-dt / self.config.zoom_smoothing).exp();
                    let zoom = self.camera.zoom + (animation.target - self.camera.zoom) * progress;

                    if (animation.target - zoom).abs() < 0.001 {
                        self.camera.zoom_around(animation.cursor, animation.target);
                        self.zoom_animation = None;
                        // Settled: re-raster any bitmaps that drifted out of
                        // their quality band.
                        follow_up = self.refresh_snapshots();
                    } else {
                        self.camera.zoom_around(animation.cursor, zoom);
                    }
                    self.cache.clear();
                }
                // Auto-layout: ease each node from its old to its new position.
                let layout_done = if let Some(anim) = &self.layout_anim {
                    let t = (now.duration_since(anim.started).as_secs_f32()
                        / self.config.anim_secs(LAYOUT_ANIM))
                    .clamp(0.0, 1.0);
                    let e = camera::ease_out_cubic(t);
                    for &(index, from, to) in &anim.moves {
                        if let Some(node) = self.nodes.get_mut(index) {
                            node.x = from.0 + (to.0 - from.0) * e;
                            node.y = from.1 + (to.1 - from.1) * e;
                        }
                    }
                    // Keep resetting the save debounce so only the final
                    // positions are written.
                    self.mark_dirty();
                    self.cache.clear();
                    t >= 1.0
                } else {
                    false
                };
                if layout_done {
                    self.layout_anim = None;
                    follow_up =
                        Task::batch([follow_up, self.refresh_snapshots(), self.schedule_recluster()]);
                }
                return follow_up;
            }
            Message::SnapshotsReady(snapshots) => {
                self.raster_pending = false;
                let editing = self.editing.as_ref().map(|editing| editing.index);
                let now = Instant::now();
                for (index, snapshot) in snapshots {
                    if editing == Some(index) {
                        continue;
                    }
                    if self.text_snapshots.contains_key(&index) {
                        // Warm the upload behind the old bitmap, then cross-fade.
                        self.staging_snapshots.insert(
                            index,
                            StagedSnapshot {
                                snapshot,
                                since: now,
                                opacity: 0.0,
                            },
                        );
                    } else {
                        // Nothing on screen to preserve; show it right away.
                        self.text_snapshots.insert(index, snapshot);
                    }
                }
                self.cache.clear();
                // The camera may have moved again while rasterizing.
                return self.refresh_snapshots();
            }
            Message::ClustersReady(clustering, snapshots) => {
                self.clusters = clustering.clusters;
                self.cluster_links = clustering.links;
                self.cluster_snapshots = snapshots;
                self.recluster_pending = false;
                self.cache.clear();
                // Changes arrived while recomputing; do it once more.
                if self.recluster_dirty {
                    return self.schedule_recluster();
                }
            }
            Message::NodeDragEnd => {
                return self.schedule_recluster();
            }
            Message::OpenUrl(url) => {
                crate::platform::open_url(&url);
            }
            Message::Undo => {
                return self.undo();
            }
            Message::Redo => {
                return self.redo();
            }
            Message::SaveTick => {
                // Debounce: only write once changes have paused.
                if should_save(
                    self.doc_dirty,
                    self.last_edit.elapsed(),
                    self.config.autosave_debounce(),
                ) {
                    self.save_document();
                }
            }
            Message::FileChanged => {
                // The OS told us the file changed; decide whether to reload.
                return self.check_reload();
            }
            Message::Exit => {
                if !self.overlay {
                    return self.save_and_exit();
                }
                match self.phase {
                    Phase::Running | Phase::Entering { .. } => {
                        self.editing = None;
                        self.phase = Phase::Exiting {
                            started: Instant::now(),
                        };
                        self.cache.clear();
                    }
                    // Second Esc during the exit animation: hide right away.
                    Phase::Exiting { .. } => return self.hide_window(),
                    Phase::Hidden => {}
                }
            }
            Message::Quit => return self.save_and_exit(),
            // The global hotkey is an OS-level registration that keeps firing
            // even while we're recording its replacement — ignore the toggle so
            // pressing the current hotkey can be re-bound instead of summoning.
            Message::ToggleOverlay if self.hotkey_recording => {}
            Message::ToggleOverlay => match self.phase {
                Phase::Hidden => {
                    self.phase = Phase::Entering {
                        started: Instant::now(),
                    };
                    self.reveal = 0.0;
                    self.cache.clear();
                    // Linux reopens a fresh window (it was closed to hide); its
                    // ViewportResized drives the rest. Elsewhere the resident
                    // main window is simply un-hidden.
                    #[cfg(target_os = "linux")]
                    let task = {
                        let (id, open) = window::open(self.overlay_settings());
                        self.main_window = Some(id);
                        open.discard().chain(Task::done(Message::WindowShown))
                    };
                    #[cfg(not(target_os = "linux"))]
                    let task = match self.main_window {
                        Some(id) => window::set_mode(id, window::Mode::Windowed)
                            .chain(Task::done(Message::WindowShown)),
                        None => Task::none(),
                    };
                    return task;
                }
                Phase::Running | Phase::Entering { .. } => {
                    self.editing = None;
                    self.phase = Phase::Exiting {
                        started: Instant::now(),
                    };
                    self.cache.clear();
                }
                Phase::Exiting { .. } => {}
            },
            Message::WindowShown => {
                // Re-raise and re-frame the overlay window (also re-adapts
                // to display changes while hidden).
                platform::configure_overlay_window();
                self.overlay_shown_at = Some(Instant::now());
            }
            Message::SpaceChanged => {
                // The active macOS Space changed. Auto-dismiss (the
                // subscription only fires this when persist is off). Hide
                // instantly — the exit animation would play on a Space the
                // user is no longer looking at.
                //
                // But ordering/activating the panel onto the current Space at
                // summon *itself* emits an active-Space-change; acting on that
                // would hide the board we just brought up (and, on a non-launch
                // desktop, it fires repeatedly). So ignore Space-changes for a
                // short cooldown after a summon — a genuine switch away lands
                // well after it.
                let summon_churn = self
                    .overlay_shown_at
                    .is_some_and(|t| t.elapsed() < SPACE_DISMISS_COOLDOWN);
                let dismiss = self.overlay
                    && !summon_churn
                    && matches!(self.phase, Phase::Running | Phase::Entering { .. });
                if dismiss {
                    return self.hide_window();
                }
            }
            Message::OpenSearch => {
                self.command_palette = None; // mutually exclusive
                if self.search.is_none() {
                    self.search = Some(Search {
                        query: String::new(),
                        matches: Vec::new(),
                        current: 0,
                    });
                }
                return iced::advanced::widget::operate(
                    iced::advanced::widget::operation::focusable::focus(SEARCH_INPUT.clone()),
                );
            }
            Message::OpenCommandPalette => {
                self.search = None; // mutually exclusive
                self.command_palette = Some(command::CommandPalette::default());
                return iced::advanced::widget::operate(
                    iced::advanced::widget::operation::focusable::focus(
                        command::COMMAND_INPUT.clone(),
                    ),
                );
            }
            Message::CloseCommandPalette => self.command_palette = None,
            Message::CommandPaletteInput(query) => {
                if let Some(palette) = &mut self.command_palette {
                    palette.query = query;
                    palette.selected = 0;
                }
            }
            Message::CommandPaletteMove(delta) => {
                if let Some(palette) = &mut self.command_palette {
                    let len = command::filtered(&palette.query).len();
                    if len > 0 {
                        palette.selected =
                            (palette.selected as i32 + delta).rem_euclid(len as i32) as usize;
                    }
                }
            }
            Message::RunCommand(id) => return self.run_command(id),
            Message::RunSelectedCommand => {
                let id = self.command_palette.as_ref().and_then(|p| {
                    command::filtered(&p.query).get(p.selected).map(|c| c.id)
                });
                if let Some(id) = id
                    && id.is_enabled(self)
                {
                    return self.run_command(id);
                }
            }
            Message::OpenSettingsOverlay => {
                self.search = None;
                self.command_palette = None;
                self.populate_settings_drafts();
                self.settings_overlay = true;
                self.cache.clear();
            }
            Message::CloseSettingsOverlay => {
                self.settings_overlay = false;
                self.cache.clear();
            }
            Message::Noop => {}
            Message::SearchInput(query) => {
                // Nodes that were or are now matched must re-render: newly matched
                // ones draw live (highlighted), newly unmatched ones re-rasterize.
                let mut affected: HashSet<usize> = self
                    .search
                    .as_ref()
                    .map(|s| s.matches.iter().map(|m| m.node).collect())
                    .unwrap_or_default();
                let matches = find_matches(&self.nodes, &query);
                affected.extend(matches.iter().map(|m| m.node));
                for index in affected {
                    self.invalidate_snapshot(index);
                }
                self.search = Some(Search { query, matches, current: 0 });
                self.cache.clear();
                let focus = self.focus_current_match();
                return Task::batch([self.refresh_snapshots(), focus]);
            }
            Message::SearchNext | Message::SearchPrev => {
                if let Some(search) = &mut self.search
                    && !search.matches.is_empty()
                {
                    let n = search.matches.len();
                    search.current = if matches!(message, Message::SearchNext) {
                        (search.current + 1) % n
                    } else {
                        (search.current + n - 1) % n
                    };
                }
                self.cache.clear();
                return self.focus_current_match();
            }
            Message::CloseSearch => {
                if let Some(search) = self.search.take() {
                    for index in search
                        .matches
                        .iter()
                        .map(|m| m.node)
                        .collect::<HashSet<_>>()
                    {
                        self.invalidate_snapshot(index);
                    }
                }
                self.cache.clear();
                return self.refresh_snapshots();
            }
            Message::OpenSettings => {
                if let Some(id) = self.settings_window {
                    return window::gain_focus(id);
                }
                self.populate_settings_drafts();
                let (id, open) = window::open(startup::settings_window_settings());
                self.settings_window = Some(id);
                return open.discard();
            }
            Message::Settings(msg) => return self.apply_setting(msg),
            Message::SettingsBrowsed(field, path) => {
                if let Some(path) = path {
                    let text = path.to_string_lossy().into_owned();
                    self.settings_drafts.insert(field, text.clone());
                    settings::apply(&mut self.config, field, settings::Value::Text(text));
                    self.config.sanitize();
                    self.persist_config();
                }
            }
            Message::CanvasAction(action) => {
                let create = match action.as_str() {
                    "new_board" => true,
                    "open_board" => false,
                    _ => return Task::none(),
                };
                // The overlay is always-on-top and would cover the native file
                // dialog, so dismiss it first and open the dialog once it's
                // hidden. In windowed mode there's nothing to hide.
                if self.overlay {
                    self.pending_board_create = Some(create);
                    return self.hide_window().chain(Task::done(Message::OpenBoardDialog));
                }
                return self.choose_board(create);
            }
            Message::OpenBoardDialog => {
                if let Some(create) = self.pending_board_create.take() {
                    return self.choose_board(create);
                }
            }
            Message::BoardChosen { path, create } => {
                let mut task = Task::none();
                if let Some(path) = path {
                    task = self.open_board(path, create);
                }
                // Re-summon the overlay we hid for the dialog: shows the newly
                // opened board, or the scratch board again if cancelled.
                if self.overlay && matches!(self.phase, Phase::Hidden) {
                    task = task.chain(Task::done(Message::ToggleOverlay));
                }
                return task;
            }
            Message::OpenDropped(path) => return self.open_board(path, false),
            Message::RecordHotkey => {
                self.hotkey_recording = true;
            }
            Message::HotkeyCaptured(key, physical, modifiers) => {
                self.capture_hotkey(key, physical, modifiers);
            }
            Message::WindowCloseRequested(id) => {
                if self.settings_window == Some(id) {
                    self.persist_config();
                    self.settings_window = None;
                    self.settings_drafts.clear();
                    self.settings_advanced_open = false;
                    return window::close(id);
                }
                // The main window's close button: quit (flushing the autosave).
                return self.save_and_exit();
            }
            Message::WindowClosed(id) => {
                if self.settings_window == Some(id) {
                    self.settings_window = None;
                    self.settings_drafts.clear();
                    self.settings_advanced_open = false;
                }
            }
        }

        Task::none()
    }

    /// Whether a continuous redraw loop is running (`window::frames()`): camera
    /// motion or an in-flight animation. Frame-pacing stats are only meaningful
    /// here — outside it, redraws are sparse and event-driven (hover), so their
    /// inter-frame gaps are idle time, not render cost.
    fn animating(&self) -> bool {
        self.inertia.is_some()
            || self.zoom_animation.is_some()
            || self.camera_focus.is_some()
            || !self.staging_snapshots.is_empty()
            || !self.spawning.is_empty()
            || self.picker_anim.is_some()
            || self.layout_anim.is_some()
            || !self.ai_flows.is_empty()
            || self.ghosts_expiring()
            || (!self.ghosts.is_empty() && self.ghosts_appeared.elapsed().as_secs_f32() < GHOST_FADE_IN)
            || matches!(self.phase, Phase::Entering { .. } | Phase::Exiting { .. })
    }

    fn subscription(&self) -> Subscription<Message> {
        let animating = self.animating();

        let mut subscriptions = Vec::new();
        if animating {
            subscriptions.push(window::frames().map(Message::Tick));
        }
        // The enter/exit reveal must finish even if the window isn't compositing
        // yet: a re-summon can race a Space switch so `window::frames()` (a
        // display-link source) never ticks, freezing the board fully transparent
        // while the app is already frontmost. A wall-clock timer guarantees the
        // reveal advances and the phase leaves `Entering`/`Exiting` regardless.
        if matches!(self.phase, Phase::Entering { .. } | Phase::Exiting { .. }) {
            subscriptions.push(iced::time::every(Duration::from_millis(16)).map(Message::Tick));
        }
        // A double-click's caret placement waits for the editor's first frame
        // (its buffer isn't shaped until then); one frame tick delivers it.
        if self.editing.as_ref().is_some_and(|e| e.pending_caret.is_some()) {
            subscriptions.push(window::frames().map(|_| Message::PlaceEditorCaret));
        }
        // While recording a summon hotkey, capture the next key combination.
        if self.hotkey_recording {
            subscriptions.push(iced::event::listen_with(capture_hotkey_event));
        }
        // While the find bar is open, Enter/Shift+Enter/Esc drive it.
        if self.search.is_some() {
            subscriptions.push(iced::event::listen_with(search_key_event));
        }
        // While the command palette is open, arrows/Esc drive it.
        if self.command_palette.is_some() {
            subscriptions.push(iced::event::listen_with(command::key_event));
        }
        // Esc closes the in-board settings modal.
        if self.settings_overlay {
            subscriptions.push(iced::event::listen_with(settings_key_event));
        }
        // Drag a `.canvas` onto the window to open it.
        subscriptions.push(iced::event::listen_with(file_drop_event));
        if self.overlay {
            subscriptions.push(Subscription::run(hotkey_toggles));
            // Tray menu clicks (Show/Hide, Quit) where a tray exists.
            #[cfg(any(target_os = "macos", windows, target_os = "linux"))]
            subscriptions.push(Subscription::run(tray::events));
        }
        // Auto-dismiss when the active workspace/Space changes, unless
        // configured to persist. Only where the platform reports it
        // (event-driven, no polling).
        if platform::WATCHES_WORKSPACE
            && self.overlay
            && !self.config.persist_across_spaces
        {
            subscriptions.push(Subscription::run(space_change_stream));
        }
        // Native trackpad pinch-to-zoom (macOS), which iced doesn't surface.
        if platform::SUPPORTS_PINCH {
            subscriptions.push(Subscription::run(magnify_stream));
        }
        // Native summon gesture (macOS four-finger swipe), when enabled. The
        // subscription just registers the handler; the trackpad tap is toggled
        // on/off by `set_summon_gesture_enabled` (startup + settings change).
        if platform::SUPPORTS_GESTURES && self.config.summon_gesture {
            subscriptions.push(Subscription::run(gesture_stream));
        }
        // Autosave debounce ticks only while there are unsaved changes (never
        // for the scratch board, which is never saved).
        if self.doc_dirty && !self.scratch {
            subscriptions.push(iced::time::every(SAVE_POLL).map(|_| Message::SaveTick));
        }
        // Animate the "…" dots while an AI request awaits its first token.
        if !self.ai_pending.is_empty() {
            subscriptions.push(iced::time::every(AI_DOTS_INTERVAL).map(|_| Message::AiTick));
        }
        // Event-driven external-change watch (no polling of the file). The
        // scratch board has no backing file, so nothing to watch.
        if !self.scratch {
            subscriptions.push(Subscription::run_with(self.canvas_path.clone(), watch_file));
        }
        // Close button / Cmd+Q: flush the autosave before quitting (the window
        // is created with exit_on_close_request off so we control this).
        subscriptions.push(window::close_requests().map(Message::WindowCloseRequested));
        // Clears our settings-window handle once it's gone (also covers the OS
        // closing it out from under us).
        subscriptions.push(window::close_events().map(Message::WindowClosed));
        Subscription::batch(subscriptions)
    }

    /// Fits a text node to its content (with padding, within the size
    /// bounds), growing and shrinking around its center. Runs on every
    /// edit, so the card resizes live while typing.
    /// Fits the node to its text. While editing (`rendered = false`) it sizes
    /// to the raw source so the editor fits; on commit (`rendered = true`) it
    /// sizes to the rendered Markdown so the read view fits.
    fn autosize_node(&mut self, index: usize, rendered: bool) {
        let Some(node) = self.nodes.get(index) else {
            return;
        };
        if node.kind != "text" {
            return;
        }

        // Raw text (not trimmed): while editing, trailing newlines must
        // already grow the card. The text is trimmed when editing ends.
        let text = node.text.clone().unwrap_or_default();
        // Measure against the widest allowed wrap; below the cap the node
        // hugs the text, at the cap the display wraps at the same width.
        let max_width = self.config.max_node_width;
        let (text_width, text_height) = if rendered {
            text_raster::measure_markdown(&text, max_width - 12.0, FONT_SIZE, &self.theme)
        } else {
            // The editor styles bold/italic/code inline, so measure the source
            // the same way — plain measure under-counts wrapped lines for bold
            // content and the card (and its editor) end up a line short.
            text_raster::measure_source(&text, max_width - 12.0, FONT_SIZE, 1.0, &self.theme)
        };

        let mut width =
            (text_width + NODE_PADDING_X).clamp(self.config.new_node_min_width, max_width);
        let mut height = (text_height + NODE_PADDING_Y).max(self.config.new_node_min_height);

        // Snap-to-grid: round the fitted size *up* to whole cells so the text
        // still fits, and grow from a fixed (grid-aligned) top-left rather than
        // recentering — so a grid-aligned node stays aligned as it grows (e.g.
        // a streaming AI note). Off the grid, keep the centered grow.
        let grid = self.snap_grid();
        let snapped = grid > 0.0;
        if snapped {
            (width, height) = crate::snap::fit_size(width, height, grid);
        }

        let node = &mut self.nodes[index];
        if (width - node.width).abs() > 0.5 || (height - node.height).abs() > 0.5 {
            if !snapped {
                node.x -= (width - node.width) / 2.0;
                node.y -= (height - node.height) / 2.0;
            }
            node.width = width;
            node.height = height;
            self.cache.clear();
        }
    }

    /// The active snap spacing (world px), or `0.0` when snap-to-grid is off —
    /// so snap helpers can be called unconditionally (`0.0` = no-op).
    fn snap_grid(&self) -> f32 {
        if self.config.snap_to_grid {
            self.config.snap_grid_size
        } else {
            0.0
        }
    }

    /// Applies a text-level undo (`redo = false`) or redo within the current
    /// edit session.
    fn apply_edit_history(&mut self, redo: bool) {
        let edited = self.editing.as_mut().and_then(|editing| {
            let text = if redo { editing.redo() } else { editing.undo() }?;
            Some((editing.index, text))
        });
        if let Some((index, text)) = edited {
            if let Some(node) = self.nodes.get_mut(index) {
                node.text = Some(text);
            }
            self.autosize_node(index, false);
            self.mark_dirty();
            self.cache.clear();
        }
    }

    fn mark_dirty(&mut self) {
        self.doc_dirty = true;
        self.last_edit = Instant::now();
    }

    /// Zooms by `factor` around `cursor` (canvas space), accumulating on the
    /// eased zoom target so rapid steps (wheel notches, pinch events) compound.
    /// Shared by mouse-wheel zoom and trackpad pinch.
    fn zoom_by(&mut self, factor: f32, cursor: Point) {
        self.interacted = true;
        self.inertia = None;
        self.camera_focus = None;
        let clamp = |z: f32| z.clamp(self.camera.min_zoom, self.camera.max_zoom);
        match &mut self.zoom_animation {
            // Retarget an in-flight ease but keep `last_tick`, so the per-frame
            // step still measures a full frame. A rapid gesture (pinch fires
            // ~60-120×/s) would otherwise reset the clock every event, leaving
            // each frame `dt ≈ 0` and the ease starved (choppy).
            Some(animation) => {
                animation.target = clamp(animation.target * factor);
                animation.cursor = cursor;
            }
            None => {
                self.zoom_animation = Some(ZoomAnimation {
                    target: clamp(self.camera.zoom * factor),
                    cursor,
                    last_tick: Instant::now(),
                });
            }
        }
    }

    /// Writes the current document to disk, preserving unknown fields, and
    /// adopts the resulting mtime so the reload poll ignores our own write.
    /// The in-memory document as it would be written to disk.
    fn current_document(&self) -> CanvasDocument {
        CanvasDocument {
            nodes: self.nodes.clone(),
            edges: self.edges.clone(),
            extra: self.doc_extra.clone(),
        }
    }

    fn save_document(&mut self) {
        if self.scratch {
            return; // the scratch board is never written to disk
        }
        match self.current_document().save(&self.canvas_path) {
            Ok(()) => {
                self.doc_dirty = false;
                self.file_mtime = file_mtime(&self.canvas_path);
            }
            Err(error) => eprintln!("autosave failed: {error}"),
        }
    }

    /// Flushes a pending autosave (if any) and quits. Used on every exit path
    /// so edits made within the autosave debounce window are never lost.
    fn save_and_exit(&mut self) -> Task<Message> {
        if !self.scratch
            && save_if_dirty(self.doc_dirty, &self.current_document(), &self.canvas_path)
        {
            self.doc_dirty = false;
            self.file_mtime = file_mtime(&self.canvas_path);
        }
        // macOS: our NSPanel class-morph leaves winit's KVO teardown
        // inconsistent, which throws NSRangeException on a clean exit. The save
        // is already flushed, so exit the process directly and skip the
        // crashing teardown. Other platforms tear down cleanly via iced.
        #[cfg(target_os = "macos")]
        {
            std::process::exit(0)
        }
        #[cfg(not(target_os = "macos"))]
        {
            iced::exit()
        }
    }

    // --- Settings window ---

    /// The raw text of a settings text/number field (seeded from the config when
    /// the window opens; reflects exactly what the user has typed since).
    fn draft(&self, field: settings::Field) -> &str {
        self.settings_drafts
            .get(&field)
            .map(String::as_str)
            .unwrap_or("")
    }

    /// Seeds `settings_drafts` from the current config for every text-editable
    /// field, so the form opens showing the live values.
    fn populate_settings_drafts(&mut self) {
        use settings::Field::*;
        self.settings_drafts.clear();
        for field in [
            Canvas, SummonHotkey, ThemeCss, AiUrl, AiModel, AiApiKey, MinZoom, MaxZoom,
            ZoomSmoothing, WheelZoomStep, PinchZoomStep, AutosaveDebounceMs, ClusterZoom,
            AutoZoomTrigger, AutoZoomLevel, NewNodeMinWidth, NewNodeMinHeight, MaxNodeWidth,
            LayoutRankGap, LayoutNodeGap, MaxAiNodes,
        ] {
            self.settings_drafts
                .insert(field, settings::config_text(&self.config, field));
        }
    }

    /// Applies one settings edit: mutates the config, clamps it, persists (except
    /// mid-drag/mid-type, which persist on `Commit`), and refreshes anything the
    /// change affects live.
    fn apply_setting(&mut self, msg: settings::Msg) -> Task<Message> {
        use settings::{Msg, Value};
        match msg {
            Msg::Toggle(field, b) => {
                settings::apply(&mut self.config, field, Value::Bool(b));
                self.config.sanitize();
                self.persist_config();
                self.after_setting_change(field)
            }
            Msg::Pick(field, choice) => {
                settings::apply(&mut self.config, field, Value::Text(choice));
                self.config.sanitize();
                self.persist_config();
                self.after_setting_change(field)
            }
            Msg::Slide(field, v) => {
                settings::apply(&mut self.config, field, Value::Num(v));
                self.config.sanitize();
                self.after_setting_change(field)
            }
            Msg::Edit(field, s) => {
                if settings::is_number(field) {
                    if let Ok(v) = s.trim().parse::<f32>() {
                        settings::apply(&mut self.config, field, Value::Num(v));
                        self.config.sanitize();
                    }
                } else {
                    settings::apply(&mut self.config, field, Value::Text(s.clone()));
                    self.config.sanitize();
                }
                self.settings_drafts.insert(field, s);
                self.after_setting_change(field)
            }
            Msg::Commit => {
                self.persist_config();
                Task::none()
            }
            Msg::Browse(field) => self.browse_for(field),
            Msg::Paste(field) => {
                if let Some(text) = clipboard::read_text() {
                    let text = text.trim().to_owned();
                    settings::apply(&mut self.config, field, Value::Text(text.clone()));
                    self.config.sanitize();
                    self.settings_drafts.insert(field, text);
                    self.persist_config();
                    return self.after_setting_change(field);
                }
                Task::none()
            }
            Msg::PickPreset(name) => {
                // "Off" clears the endpoint (AI disabled); a known provider fills
                // it; "Custom" keeps whatever the user typed.
                if name == settings::PROVIDER_OFF {
                    self.config.ai_url = String::new();
                } else if let Some(preset) = ai::PRESETS.iter().find(|p| p.name == name) {
                    self.config.ai_url = preset.url.to_owned();
                } else {
                    return Task::none();
                }
                self.config.sanitize();
                self.persist_config();
                // Reflect the change in the URL text field.
                self.settings_drafts
                    .insert(settings::Field::AiUrl, self.config.ai_url.clone());
                Task::none()
            }
            Msg::ToggleAdvanced => {
                self.settings_advanced_open = !self.settings_advanced_open;
                Task::none()
            }
        }
    }

    /// Live side effects of a settings change (over and above the config write).
    fn after_setting_change(&mut self, field: settings::Field) -> Task<Message> {
        self.cache.clear();
        match field {
            // These change how node *and* cluster text is drawn — re-rasterize
            // both. Cluster snapshots aren't produced while rasterize is off, so
            // turning it on must regenerate them or clusters render empty.
            settings::Field::RasterizeText | settings::Field::CenterNodeText => {
                Task::batch([self.refresh_snapshots(), self.schedule_recluster()])
            }
            // Arm or disarm the trackpad tap immediately (we're on the main
            // thread here). The subscription registers the handler separately.
            settings::Field::SummonGesture => {
                if platform::SUPPORTS_GESTURES {
                    platform::set_summon_gesture_enabled(self.config.summon_gesture);
                }
                Task::none()
            }
            _ => Task::none(),
        }
    }

    fn persist_config(&self) {
        self.config.save(config_path_arg().as_deref());
    }

    /// Opens a file dialog for a path setting. macOS/Windows use the blocking
    /// dialog on the (main) update thread; Linux runs the async portal dialog on
    /// iced's runtime (its zbus backend needs one — iced is built with tokio).
    fn browse_for(&mut self, field: settings::Field) -> Task<Message> {
        #[cfg(not(target_os = "linux"))]
        {
            let picked = rfd::FileDialog::new().pick_file();
            Task::done(Message::SettingsBrowsed(field, picked))
        }
        #[cfg(target_os = "linux")]
        {
            Task::perform(
                async {
                    rfd::AsyncFileDialog::new()
                        .pick_file()
                        .await
                        .map(|handle| handle.path().to_path_buf())
                },
                move |picked| Message::SettingsBrowsed(field, picked),
            )
        }
    }

    /// Opens a native dialog to create a new board (a Save dialog, `create`) or
    /// open an existing one, then resolves to `Message::BoardChosen`. Mirrors
    /// `browse_for`'s per-OS handling (blocking on mac/win, async portal on
    /// Linux, whose zbus backend needs iced's tokio runtime).
    fn choose_board(&mut self, create: bool) -> Task<Message> {
        #[cfg(not(target_os = "linux"))]
        {
            let dialog = rfd::FileDialog::new().add_filter("Obsidian canvas", &["canvas"]);
            let path = if create {
                dialog.set_file_name("board.canvas").save_file()
            } else {
                dialog.pick_file()
            };
            Task::done(Message::BoardChosen { path, create })
        }
        #[cfg(target_os = "linux")]
        {
            Task::perform(
                async move {
                    let dialog =
                        rfd::AsyncFileDialog::new().add_filter("Obsidian canvas", &["canvas"]);
                    let handle = if create {
                        dialog.set_file_name("board.canvas").save_file().await
                    } else {
                        dialog.pick_file().await
                    };
                    handle.map(|handle| handle.path().to_path_buf())
                },
                move |path| Message::BoardChosen { path, create },
            )
        }
    }

    /// Creates an empty canvas at `path` (when `create`), then opens it as the
    /// live board. A failed create is reported and aborts the open.
    fn open_board(&mut self, path: PathBuf, create: bool) -> Task<Message> {
        if create {
            let empty = CanvasDocument {
                nodes: Vec::new(),
                edges: Vec::new(),
                extra: serde_json::Map::new(),
            };
            if let Err(error) = empty.save(&path) {
                eprintln!("could not create {}: {error}", path.display());
                return Task::none();
            }
        }
        self.open_canvas(path)
    }

    /// Switches the live document to the canvas at `path`: leaves the scratch
    /// board (or a previous file) behind, reframes the camera, and remembers
    /// the choice so the next launch opens it directly. Generalizes
    /// `reload_document` (which reloads the *same* path) to a different file.
    fn open_canvas(&mut self, path: PathBuf) -> Task<Message> {
        // Flush pending edits to the board we're leaving before switching, so a
        // drag-drop / open while mid-edit doesn't drop them. No-op for the
        // scratch board (which is never saved) and when there's nothing dirty.
        if self.doc_dirty {
            self.save_document();
        }

        let document = match CanvasDocument::load(&path) {
            Ok(document) => document,
            Err(error) => {
                eprintln!("could not open {}: {error}", path.display());
                return Task::none();
            }
        };

        self.canvas_path = path;
        self.base_path = vault_root(&self.canvas_path, &document);
        self.file_mtime = file_mtime(&self.canvas_path);
        self.scratch = false;

        self.images = document
            .nodes
            .iter()
            .filter_map(|node| {
                let file = node.file.as_ref()?;
                Some((
                    node.id.clone(),
                    image::Handle::from_path(self.base_path.as_path().join(file)),
                ))
            })
            .collect();
        self.nodes = document.nodes;
        self.edges = document.edges;
        self.doc_extra = document.extra;

        // A fresh document: drop per-node bitmaps and all transient/edit state
        // (mirrors `reload_document`), then reframe the new content.
        self.text_snapshots.clear();
        self.staging_snapshots.clear();
        self.spawning.clear();
        self.pending_edit = None;
        self.fresh_node = None;
        self.editing = None;
        self.selected.clear();
        self.selected_edge = None;
        self.hovered_edge = None;
        self.history.clear();
        self.edit_undo = None;
        self.doc_dirty = false;
        self.interacted = false;
        self.camera.fit(self.content_bounds());
        self.cache.clear();

        // Remember the working board so the next launch skips the scratch board.
        self.config.canvas = self.canvas_path.to_string_lossy().into_owned();
        self.persist_config();

        Task::batch([self.refresh_snapshots(), self.schedule_recluster()])
    }

    /// Turns a captured key + modifiers into a summon-hotkey string, validates it
    /// with the real parser, and sets it if valid. Escape cancels; a bare
    /// modifier keeps waiting for the actual key.
    fn capture_hotkey(
        &mut self,
        key: iced::keyboard::Key,
        physical: iced::keyboard::key::Physical,
        modifiers: iced::keyboard::Modifiers,
    ) {
        use iced::keyboard::key::{Key, Named};

        // Escape cancels; a bare modifier (or nothing yet) keeps waiting.
        if matches!(&key, Key::Named(Named::Escape)) {
            self.hotkey_recording = false;
            return;
        }
        let Some(candidate) = hotkey_string(&key, physical, modifiers) else {
            return;
        };

        // Only accept combinations the registrar can actually parse.
        if startup::parse_hotkey(&candidate).is_some() {
            self.config.summon_hotkey = candidate.clone();
            self.persist_config();
            self.settings_drafts
                .insert(settings::Field::SummonHotkey, candidate);
            self.hotkey_recording = false;
            self.reregister_hotkey();
        }
    }

    /// Re-registers the summon hotkey with the OS after it changes in settings,
    /// so it takes effect without a restart.
    fn reregister_hotkey(&mut self) {
        let Some(manager) = &self.hotkey_manager else {
            return;
        };
        if let Some(old) = self.registered_hotkey.take() {
            let _ = manager.unregister(old);
        }
        if let Some(hotkey) = startup::summon_hotkey(&self.config.summon_hotkey) {
            match manager.register(hotkey) {
                Ok(()) => self.registered_hotkey = Some(hotkey),
                Err(error) => eprintln!("could not register the summon hotkey: {error}"),
            }
        }
    }

    /// Reloads the file if another process changed it — unless we have
    /// unsaved edits or an open editor, in which case our own state wins
    /// (last-write-wins; our pending save will overwrite). Only touches the
    /// disk (one stat) when we might actually reload.
    fn check_reload(&mut self) -> Task<Message> {
        if self.scratch || self.doc_dirty || self.editing.is_some() {
            return Task::none();
        }
        let current = file_mtime(&self.canvas_path);
        if should_reload(false, false, current, self.file_mtime) {
            return self.reload_document();
        }
        Task::none()
    }

    fn reload_document(&mut self) -> Task<Message> {
        let Ok(document) = CanvasDocument::load(&self.canvas_path) else {
            return Task::none();
        };
        self.file_mtime = file_mtime(&self.canvas_path);

        self.images = document
            .nodes
            .iter()
            .filter_map(|node| {
                let file = node.file.as_ref()?;
                Some((
                    node.id.clone(),
                    image::Handle::from_path(self.base_path.as_path().join(file)),
                ))
            })
            .collect();
        self.nodes = document.nodes;
        self.edges = document.edges;
        self.doc_extra = document.extra;

        // Node indices changed; drop per-node bitmaps and let them refresh.
        self.text_snapshots.clear();
        self.staging_snapshots.clear();
        self.spawning.clear();
        self.pending_edit = None;
        self.fresh_node = None;
        self.selected.clear();
        self.selected_edge = None;
        self.hovered_edge = None;
        // The document changed under us; the old history no longer applies.
        self.history.clear();
        self.edit_undo = None;
        self.cache.clear();

        Task::batch([self.refresh_snapshots(), self.schedule_recluster()])
    }

    /// Recomputes clusters off the main thread. Coalesces bursts: if one is
    /// already running, just marks the result stale so it reruns once on
    /// completion. Called only after settled geometry/edge changes, never
    /// during a drag or on pan/zoom.
    fn schedule_recluster(&mut self) -> Task<Message> {
        if self.recluster_pending {
            self.recluster_dirty = true;
            return Task::none();
        }
        self.recluster_pending = true;
        self.recluster_dirty = false;
        let nodes = self.nodes.clone();
        let edges = self.edges.clone();
        let cluster_zoom = self.config.cluster_zoom;
        let theme = self.theme;
        let center = self.config.center_node_text;
        let rasterize = self.config.rasterize_text;
        Task::perform(
            async move {
                let clustering = cluster::compute(&nodes, &edges, &theme);
                let snapshots = generate_cluster_snapshots(
                    &clustering.clusters,
                    cluster_zoom,
                    theme,
                    center,
                    rasterize,
                );
                (clustering, snapshots)
            },
            |(clustering, snapshots)| Message::ClustersReady(clustering, snapshots),
        )
    }

    /// Opens the in-place text editor for a node and focuses it.
    /// Node ids belonging to in-flight AI generations, which are transient and
    /// excluded from undo history until they finish.
    fn transient_node_ids(&self) -> HashSet<String> {
        self.ai_streams
            .values()
            .flat_map(|stream| stream.nodes.iter().cloned())
            .collect()
    }

    fn snapshot(&self) -> DocSnapshot {
        committed_snapshot(&self.nodes, &self.edges, &self.transient_node_ids())
    }

    /// Records the current document as an undo point and clears the redo
    /// history (a new action diverges from any undone future).
    fn push_undo(&mut self) {
        self.history.record(self.snapshot());
    }

    /// Replaces the document with a snapshot (used by undo/redo), rebuilding
    /// images and transient caches since node indices change.
    fn restore(&mut self, snapshot: DocSnapshot) -> Task<Message> {
        self.editing = None;
        self.layout_anim = None;
        // In-flight AI nodes aren't in snapshots; carry the live ones across so
        // undo/redo of an unrelated action doesn't drop a streaming node.
        let transient = self.transient_node_ids();
        let mut nodes = snapshot.nodes;
        nodes.extend(
            self.nodes
                .iter()
                .filter(|n| transient.contains(&n.id))
                .cloned(),
        );
        let mut edges = snapshot.edges;
        edges.extend(self.edges.iter().filter(|e| is_transient_edge(e, &transient)).cloned());
        // Text bitmaps we can carry across the restore, keyed by node id with
        // the text + size they depict. Nodes an undo/redo doesn't touch keep
        // their already-uploaded texture instead of re-rasterizing and blanking
        // for a frame (see text_bitmap_reusable). Captured before self.nodes is
        // rebuilt, since text_snapshots is keyed by the old indices.
        let carried: HashMap<String, (text_raster::Snapshot, Option<String>, f32, f32)> = self
            .nodes
            .iter()
            .enumerate()
            .filter_map(|(index, node)| {
                let snapshot = self.text_snapshots.get(&index)?;
                Some((
                    node.id.clone(),
                    (snapshot.clone(), node.text.clone(), node.width, node.height),
                ))
            })
            .collect();

        self.nodes = nodes;
        self.edges = edges;
        self.images = self
            .nodes
            .iter()
            .filter_map(|node| {
                let file = node.file.as_ref()?;
                Some((
                    node.id.clone(),
                    image::Handle::from_path(self.base_path.as_path().join(file)),
                ))
            })
            .collect();
        // Re-key the carried bitmaps onto the restored nodes, keeping one only
        // where the node's text and size still match what it depicts.
        self.text_snapshots = self
            .nodes
            .iter()
            .enumerate()
            .filter_map(|(index, node)| {
                let (snapshot, text, width, height) = carried.get(&node.id)?;
                text_bitmap_reusable(text, (*width, *height), &node.text, (node.width, node.height))
                    .then(|| (index, snapshot.clone()))
            })
            .collect();
        self.staging_snapshots.clear();
        self.spawning.clear();
        self.pending_edit = None;
        self.fresh_node = None;
        self.selected.clear();
        self.selected_edge = None;
        self.hovered_edge = None;
        self.mark_dirty();
        self.cache.clear();
        Task::batch([self.refresh_snapshots(), self.schedule_recluster()])
    }

    fn undo(&mut self) -> Task<Message> {
        if !self.history.can_undo() {
            return Task::none();
        }
        let previous = self.history.undo(self.snapshot()).expect("can_undo");
        self.restore(previous)
    }

    fn redo(&mut self) -> Task<Message> {
        if !self.history.can_redo() {
            return Task::none();
        }
        let next = self.history.redo(self.snapshot()).expect("can_redo");
        self.restore(next)
    }

    /// Deletes the selected node and its edges, remapping index-keyed state
    /// so the caches for other nodes survive.
    /// Deletes the selected edge (undoable).
    fn delete_selected_edge(&mut self) -> Task<Message> {
        let Some(index) = self.selected_edge.take() else {
            return Task::none();
        };
        self.hovered_edge = None;
        if self.editing.is_some() {
            return Task::none();
        }
        // Snapshot first, but only commit it to the undo history if a link is
        // actually removed — a stale index (list shrank under the selection)
        // must not leave a phantom undo entry.
        let snapshot = self.snapshot();
        // Remember an endpoint so its (now possibly smaller) graph can settle.
        let endpoint = self.edges.get(index).map(|e| e.from_node.clone());
        if !model::remove_edge(&mut self.edges, index) {
            return Task::none();
        }
        self.history.record(snapshot);
        // Removing a link may split a graph; re-lay-out if enabled (covered by
        // the undo snapshot just recorded).
        if let Some(id) = endpoint
            && let Some(i) = self.nodes.iter().position(|n| n.id == id)
        {
            self.auto_relayout(i, false);
        }
        self.mark_dirty();
        self.cache.clear();
        self.schedule_recluster()
    }

    fn delete_selected(&mut self) -> Task<Message> {
        if self.selected.is_empty() {
            return self.delete_selected_edge();
        }

        // Live indices to remove, high to low, so each removal leaves the
        // lower indices (and their index-keyed bitmaps) valid.
        let mut indices: Vec<usize> = self
            .selected
            .iter()
            .copied()
            .filter(|&i| i < self.nodes.len())
            .collect();
        indices.sort_unstable();
        indices.dedup();
        if indices.is_empty() || self.editing.is_some() {
            self.selected.clear();
            self.selected_edge = None;
            self.hovered_edge = None;
            return Task::none();
        }

        self.push_undo();
        let removed_ids: std::collections::HashSet<String> =
            indices.iter().map(|&i| self.nodes[i].id.clone()).collect();
        // Cancel any in-flight AI request whose response node is being deleted.
        self.cancel_ai_streams(&removed_ids);
        for &index in indices.iter().rev() {
            self.nodes.remove(index);
            shift_indices(&mut self.text_snapshots, index);
            shift_indices(&mut self.staging_snapshots, index);
            shift_indices(&mut self.spawning, index);
        }
        self.edges.retain(|edge| {
            !removed_ids.contains(&edge.from_node) && !removed_ids.contains(&edge.to_node)
        });
        self.pending_edit = None;
        self.fresh_node = None;
        self.selected.clear();
        self.selected_edge = None;
        self.hovered_edge = None;

        self.mark_dirty();
        self.cache.clear();
        Task::batch([self.refresh_snapshots(), self.schedule_recluster()])
    }

    /// Smooth camera fly-to that centers `world` at the *current* zoom (a pure
    /// scroll, no zoom change) — the wayfinding arrow's action.
    fn focus_on_world(&mut self, world: Point) {
        self.inertia = None;
        self.camera_focus = Some(CameraFocus {
            target_zoom: self.camera.zoom,
            target_offset: Vector::new(world.x, world.y) * -self.camera.zoom,
            last_tick: Instant::now(),
        });
    }

    /// Runs a command-palette entry: closes the palette, then dispatches the
    /// action as its `Message`. Position-needing commands (paste, add node)
    /// target the viewport center.
    fn run_command(&mut self, id: command::CommandId) -> Task<Message> {
        use command::CommandId;
        self.command_palette = None;
        let center = self.camera.to_world(Point::ORIGIN + self.camera.center());
        let message = match id {
            CommandId::NewBoard => Message::CanvasAction("new_board".to_owned()),
            CommandId::OpenBoard => Message::CanvasAction("open_board".to_owned()),
            CommandId::FindOnBoard => Message::OpenSearch,
            // From the palette, settings open in-board (the tray still uses the
            // separate window, which works while the board is hidden).
            CommandId::Settings => Message::OpenSettingsOverlay,
            CommandId::Dismiss => Message::ToggleOverlay,
            CommandId::Quit => Message::Quit,
            CommandId::Undo => Message::Undo,
            CommandId::Redo => Message::Redo,
            CommandId::Copy => Message::CopyNodes,
            CommandId::Paste => Message::Paste { world: center },
            CommandId::DeleteSelected => Message::DeleteSelected,
            CommandId::AddNode => Message::CreateNode { world: center },
            CommandId::AskAi => Message::AskAi,
            CommandId::Distribute => Message::AutoLayout(self.config_layout_dir()),
            CommandId::ToggleAutoLayout => Message::ToggleAutoLayout,
            CommandId::ToggleSetting(field) => {
                Message::Settings(settings::Msg::Toggle(field, !self.setting_bool(field)))
            }
        };
        Task::done(message)
    }

    /// The current value of a boolean setting, so the palette can flip it.
    fn setting_bool(&self, field: settings::Field) -> bool {
        use settings::Field;
        let c = &self.config;
        match field {
            Field::DismissOnBackgroundClick => c.dismiss_on_background_click,
            Field::PersistAcrossSpaces => c.persist_across_spaces,
            Field::SummonGesture => c.summon_gesture,
            Field::CenterNodeText => c.center_node_text,
            Field::OverlayTransparent => c.overlay_transparent,
            Field::RasterizeText => c.rasterize_text,
            Field::NodeScrollbars => c.node_scrollbars,
            Field::AiCompletions => c.ai_completions,
            Field::AiThinking => c.ai_thinking,
            _ => false,
        }
    }

    /// Starts a smooth camera fly-to that centers the node at a comfortable
    /// editing zoom — but only when currently zoomed out, so adding a node
    /// at node level doesn't move the view.
    fn focus_on_node(&mut self, index: usize) {
        if self.camera.zoom >= self.config.auto_zoom_trigger {
            return;
        }
        let Some(node) = self.nodes.get(index) else {
            return;
        };
        let center = Vector::new(node.x + node.width / 2.0, node.y + node.height / 2.0);
        let level = self.config.auto_zoom_level;
        self.camera_focus = Some(CameraFocus {
            target_zoom: level,
            target_offset: center * -level,
            last_tick: Instant::now(),
        });
    }

    /// Whether a node currently holds a search match (drawn live + highlighted
    /// while the find bar is open, so it isn't rasterized over the highlight).
    fn is_search_match(&self, index: usize) -> bool {
        self.search
            .as_ref()
            .is_some_and(|search| search.matches.iter().any(|m| m.node == index))
    }

    /// Flies the camera to center the current search match's node. Unlike
    /// `focus_on_node`, it centers even when already zoomed in (only zooming in
    /// if the view is currently too far out to read).
    fn focus_current_match(&mut self) -> Task<Message> {
        let Some(index) = self
            .search
            .as_ref()
            .and_then(|s| s.matches.get(s.current))
            .map(|m| m.node)
        else {
            return Task::none();
        };
        if let Some(node) = self.nodes.get(index) {
            let center = Vector::new(node.x + node.width / 2.0, node.y + node.height / 2.0);
            let zoom = self.camera.zoom.max(self.config.auto_zoom_level);
            self.camera_focus = Some(CameraFocus {
                target_zoom: zoom,
                target_offset: center * -zoom,
                last_tick: Instant::now(),
            });
        }
        Task::none()
    }

    /// Finds a center near `requested` where a `width`x`height` node (plus
    /// breathing room) won't overlap existing nodes, searching outward in
    /// rings. Falls back to `requested` if the area is too crowded.
    fn find_free_placement(&self, requested: Point, width: f32, height: f32) -> Point {
        self.find_free_placement_avoiding(requested, width, height, &[])
    }

    /// Like [`find_free_placement`], but also steers clear of the extra rects
    /// `avoid` (e.g. sibling ghost cards being laid out together).
    fn find_free_placement_avoiding(
        &self,
        requested: Point,
        width: f32,
        height: f32,
        avoid: &[Rectangle],
    ) -> Point {
        const MARGIN: f32 = 20.0;
        const RAYS: usize = 12;
        const RINGS: usize = 8;

        let fits = |center: Point| {
            let rect = Rectangle::new(
                Point::new(
                    center.x - width / 2.0 - MARGIN,
                    center.y - height / 2.0 - MARGIN,
                ),
                Size::new(width + 2.0 * MARGIN, height + 2.0 * MARGIN),
            );
            !self
                .nodes
                .iter()
                .any(|node| rect.intersects(&node_world_rect(node)))
                && !avoid.iter().any(|other| rect.intersects(other))
        };

        if fits(requested) {
            return requested;
        }
        let step = height + MARGIN;
        for ring in 1..=RINGS {
            let radius = step * ring as f32;
            for ray in 0..RAYS {
                let angle = std::f32::consts::TAU * ray as f32 / RAYS as f32;
                let candidate = Point::new(
                    requested.x + radius * angle.cos(),
                    requested.y + radius * angle.sin(),
                );
                if fits(candidate) {
                    return candidate;
                }
            }
        }
        requested
    }

    /// Creates an empty text node placed near a world point (avoiding
    /// overlap), animates it in, and queues its editor to open. Returns the
    /// new node's index.
    fn spawn_text_node(&mut self, world: Point) -> usize {
        let width = self.config.new_node_min_width;
        let height = self.config.new_node_min_height;
        let center = self.find_free_placement(world, width, height);
        let mut node = CanvasNode {
            id: model::random_id(),
            kind: "text".to_owned(),
            text: Some(String::new()),
            file: None,
            x: center.x - width / 2.0,
            y: center.y - height / 2.0,
            width,
            height,
            color: None,
            extra: Default::default(),
        };
        snap_node_geometry(&mut node, self.snap_grid());
        self.nodes.push(node);
        let index = self.nodes.len() - 1;
        self.spawning.insert(index, Instant::now());
        self.pending_edit = Some(index);
        self.fresh_node = Some(index);
        self.mark_dirty();
        self.cache.clear();
        index
    }

    /// Lays out `subset` as a Mermaid-style layered graph flowing in `dir`,
    /// re-anchoring touched links and easing nodes to their targets. Streaming
    /// relayouts pass `record_undo = false` so they don't flood the undo stack.
    fn apply_layered_layout(&mut self, subset: &[usize], dir: layout::Direction, record_undo: bool) {
        let moves = layout::layered_layout(
            &self.nodes,
            &self.edges,
            subset,
            dir,
            self.config.layout_rank_gap,
            self.config.layout_node_gap,
        );
        if moves.is_empty() {
            return;
        }
        if record_undo {
            self.push_undo();
        }

        // Post-layout centers for every node (moved -> target, else unchanged).
        let mut centers: Vec<(f32, f32)> = self
            .nodes
            .iter()
            .map(|n| (n.x + n.width / 2.0, n.y + n.height / 2.0))
            .collect();
        let mut moved = HashSet::new();
        for &(i, x, y) in &moves {
            centers[i] = (x + self.nodes[i].width / 2.0, y + self.nodes[i].height / 2.0);
            moved.insert(i);
        }

        // Re-anchor links touching a moved node to the facing ports.
        let id_index: HashMap<String, usize> = self
            .nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (n.id.clone(), i))
            .collect();
        for edge in &mut self.edges {
            let (Some(&from), Some(&to)) =
                (id_index.get(&edge.from_node), id_index.get(&edge.to_node))
            else {
                continue;
            };
            if moved.contains(&from) || moved.contains(&to) {
                let (fs, ts) = layout::facing_sides(centers[from], centers[to], dir);
                edge.from_side = fs.to_owned();
                edge.to_side = ts.to_owned();
            }
        }

        // Ease each node from its current spot to the target.
        let anims = moves
            .iter()
            .map(|&(i, x, y)| (i, (self.nodes[i].x, self.nodes[i].y), (x, y)))
            .collect();
        self.layout_anim = Some(LayoutAnim {
            started: Instant::now(),
            moves: anims,
        });
        self.mark_dirty();
        self.cache.clear();
    }

    /// The `auto_layout_direction` config as a layout direction.
    fn config_layout_dir(&self) -> layout::Direction {
        if self.config.auto_layout_direction == "TD" {
            layout::Direction::Down
        } else {
            layout::Direction::Right
        }
    }

    /// Node indices reachable from `start` via links (treated undirected).
    fn connected_component(&self, start: usize) -> Vec<usize> {
        let id_index: HashMap<&str, usize> =
            self.nodes.iter().enumerate().map(|(i, n)| (n.id.as_str(), i)).collect();
        let mut visited = HashSet::from([start]);
        let mut stack = vec![start];
        while let Some(i) = stack.pop() {
            let id = self.nodes[i].id.as_str();
            for edge in &self.edges {
                let neighbor = if edge.from_node == id {
                    Some(edge.to_node.as_str())
                } else if edge.to_node == id {
                    Some(edge.from_node.as_str())
                } else {
                    None
                };
                let Some(&ni) = neighbor.and_then(|nid| id_index.get(nid)) else {
                    continue;
                };
                if visited.insert(ni) {
                    stack.push(ni);
                }
            }
        }
        let mut result: Vec<usize> = visited.into_iter().collect();
        result.sort_unstable();
        result
    }

    /// Re-lays out the whole graph `node` belongs to, unconditionally.
    fn relayout_component(&mut self, node: usize, record_undo: bool) {
        let component = self.connected_component(node);
        if component.len() > 1 {
            let dir = self.config_layout_dir();
            self.apply_layered_layout(&component, dir, record_undo);
        }
    }

    /// When auto-layout is enabled, re-lays out the graph `node` belongs to.
    fn auto_relayout(&mut self, node: usize, record_undo: bool) {
        if self.config.auto_layout {
            self.relayout_component(node, record_undo);
        }
    }

    /// Tidies just this generation's response nodes (`ai_ids`) into a clean
    /// sibling column (or row) off their source, then translates the whole
    /// stack to the nearest free spot so it never overlaps an existing node.
    /// The source and every other node stay put. The nearest-gap search keeps
    /// the stack compact (nudged just off an obstruction, e.g. below a prior
    /// generation) rather than sliding it far down the flow axis.
    fn relayout_ai_siblings(&mut self, source_id: &str, ai_ids: &[String]) {
        let Some(source) = self.nodes.iter().position(|n| n.id == source_id) else {
            return;
        };
        let ai: Vec<usize> = ai_ids
            .iter()
            .filter_map(|id| self.nodes.iter().position(|n| &n.id == id))
            .collect();
        if ai.is_empty() {
            return;
        }
        let dir = self.config_layout_dir();
        let src = &self.nodes[source];
        let source_box = (src.x, src.y, src.width, src.height);
        let sizes: Vec<(f32, f32)> = ai
            .iter()
            .map(|&i| (self.nodes[i].width, self.nodes[i].height))
            .collect();
        let mut targets = layout::sibling_stack(
            source_box,
            &sizes,
            dir,
            self.config.layout_rank_gap,
            self.config.layout_node_gap,
        );

        // `sibling_stack` anchors the block beside the source, blind to other
        // nodes. Shift the whole block to the nearest free position (searched in
        // rings) so the generated nodes clear every node outside this
        // generation — compactly, in whatever direction has room.
        let block: Vec<Rectangle> = targets
            .iter()
            .zip(&sizes)
            .map(|(&(x, y), &(w, h))| Rectangle::new(Point::new(x, y), Size::new(w, h)))
            .collect();
        let ignore: HashSet<usize> = std::iter::once(source).chain(ai.iter().copied()).collect();
        let obstacles: Vec<Rectangle> = self
            .nodes
            .iter()
            .enumerate()
            .filter(|(i, _)| !ignore.contains(i))
            .map(|(_, node)| node_world_rect(node))
            .collect();
        let (dx, dy) = nearest_free_offset(&block, &obstacles);
        if dx != 0.0 || dy != 0.0 {
            for target in &mut targets {
                target.0 += dx;
                target.1 += dy;
            }
        }

        // Snap each generated node's resting position to the grid (their sizes
        // are already grid-aligned via autosize, so top-left snapping is enough).
        let grid = self.snap_grid();
        if grid > 0.0 {
            for target in &mut targets {
                let p = crate::snap::point(Point::new(target.0, target.1), grid);
                (target.0, target.1) = (p.x, p.y);
            }
        }

        // Face the source→child links to their sides.
        let (from_side, to_side) = match dir {
            layout::Direction::Down => ("bottom", "top"),
            layout::Direction::Right => ("right", "left"),
        };
        let ai_set: HashSet<&str> = ai_ids.iter().map(String::as_str).collect();
        for edge in &mut self.edges {
            if edge.from_node == source_id && ai_set.contains(edge.to_node.as_str()) {
                edge.from_side = from_side.to_owned();
                edge.to_side = to_side.to_owned();
            }
        }

        // Ease only this generation's nodes to their new spots.
        let anims = ai
            .iter()
            .zip(targets)
            .map(|(&i, (x, y))| (i, (self.nodes[i].x, self.nodes[i].y), (x, y)))
            .collect();
        self.layout_anim = Some(LayoutAnim {
            started: Instant::now(),
            moves: anims,
        });
        self.mark_dirty();
        self.cache.clear();
    }

    /// Creates a lavender response node branching off `source_id` (an edge from
    /// its right port), spawning-animated, with `initial` text. Returns the new
    /// node's id, or `None` if the source has vanished.
    fn spawn_ai_node(&mut self, source_id: &str, initial: &str) -> Option<String> {
        let source_rect = node_world_rect(self.nodes.iter().find(|n| n.id == source_id)?);
        let (width, height) = (self.config.new_node_min_width, self.config.new_node_min_height);
        let requested = Point::new(
            source_rect.x + source_rect.width + width / 2.0 + 60.0,
            source_rect.center_y(),
        );
        let center = self.find_free_placement(requested, width, height);
        let node_id = model::random_id();
        let mut node = CanvasNode {
            id: node_id.clone(),
            kind: "text".to_owned(),
            text: Some(initial.to_owned()),
            file: None,
            x: center.x - width / 2.0,
            y: center.y - height / 2.0,
            width,
            height,
            color: config_color(&self.config.ai_node_color),
            extra: Default::default(),
        };
        snap_node_geometry(&mut node, self.snap_grid());
        self.nodes.push(node);
        let new_index = self.nodes.len() - 1;
        self.spawning.insert(new_index, Instant::now());

        let to_side = board::opposite_side("right");
        self.edges.push(CanvasEdge {
            id: model::random_id(),
            from_node: source_id.to_owned(),
            from_side: "right".to_owned(),
            to_node: node_id.clone(),
            to_side: to_side.to_owned(),
            extra: Default::default(),
        });
        self.mark_dirty();
        self.cache.clear();
        Some(node_id)
    }

    /// Requests Copilot-style sibling suggestions for `anchor` (a node with a
    /// parent) in the background; results arrive as `Message::Suggestions`
    /// tagged with `generation` so a stale (dismissed/superseded) result is dropped.
    fn request_completions(&self, anchor: String, generation: u64) -> Task<Message> {
        let Some(parent) = self
            .edges
            .iter()
            .find(|e| e.to_node == anchor)
            .map(|e| e.from_node.clone())
        else {
            return Task::none();
        };
        let context = ai::upstream_prompt(&self.nodes, &self.edges, &anchor);
        if context.trim().is_empty() {
            return Task::none();
        }
        // The parent's other children, so suggestions stay on theme and don't
        // repeat notes that already exist.
        let existing: Vec<String> = self
            .edges
            .iter()
            .filter(|e| e.from_node == parent && e.to_node != anchor)
            .filter_map(|e| self.nodes.iter().find(|n| n.id == e.to_node))
            .filter_map(|n| n.text.as_deref())
            .map(|t| t.trim().to_owned())
            .filter(|t| !t.is_empty())
            .collect();
        let (url, model, key) = (
            self.config.ai_url.clone(),
            self.config.ai_model.clone(),
            self.config.ai_api_key.clone(),
        );
        let thinking = self.config.ai_thinking;
        Task::perform(
            async move {
                ai::suggest(url, model, key, context, existing, 3, thinking).await.unwrap_or_default()
            },
            move |texts| Message::Suggestions {
                generation,
                parent: parent.clone(),
                anchor: anchor.clone(),
                texts,
            },
        )
    }

    /// Whether real suggestion ghosts are counting down to auto-dismiss: shown,
    /// not pinned by the user, and not the loading placeholder (which just
    /// waits for its result).
    fn ghosts_expiring(&self) -> bool {
        !self.ghosts.is_empty()
            && !self.ghosts_pinned
            && !self.ghost_hovered
            && !self.ghosts.iter().any(|g| g.placeholder)
    }

    /// Fade-in factor (0→1) for freshly-appeared ghost cards.
    fn ghost_fade_in(&self) -> f32 {
        (self.ghosts_appeared.elapsed().as_secs_f32() / GHOST_FADE_IN).clamp(0.0, 1.0)
    }

    /// Opacity for the expiring ghosts: full until [`GHOST_FADE_START`], then
    /// fading to 0 by the end of [`GHOST_TTL`] — the visible countdown.
    fn ghost_fade(&self) -> f32 {
        if !self.ghosts_expiring() {
            return 1.0;
        }
        let t = self.ghosts_since.elapsed().as_secs_f32();
        let ttl = GHOST_TTL.as_secs_f32();
        if t <= GHOST_FADE_START {
            1.0
        } else {
            (1.0 - (t - GHOST_FADE_START) / (ttl - GHOST_FADE_START)).clamp(0.0, 1.0)
        }
    }

    /// Clears any pending ghost suggestions and invalidates an in-flight
    /// completion request so its result won't pop back up after dismissal.
    fn dismiss_ghosts(&mut self) {
        if !self.ghosts.is_empty() {
            self.ghosts.clear();
            self.ghost_hovered = false;
            self.completion_gen = self.completion_gen.wrapping_add(1);
            self.cache.clear();
        }
    }

    /// Shows a "…" placeholder ghost below `anchor` while its suggestions are
    /// fetched — the loading cue, in the ghosts' own visual language.
    fn spawn_placeholder_ghost(&mut self, anchor: &str) {
        let Some(node) = self.nodes.iter().find(|n| n.id == anchor) else {
            return;
        };
        let Some(parent) = self
            .edges
            .iter()
            .find(|e| e.to_node == anchor)
            .map(|e| e.from_node.clone())
        else {
            return;
        };
        let rect = node_world_rect(node);
        let (w, h) = (self.config.new_node_min_width, self.config.new_node_min_height);
        let requested = Point::new(rect.center_x(), rect.y + rect.height + 30.0 + h / 2.0);
        let center = self.find_free_placement(requested, w, h);
        self.ghosts.clear();
        self.ghosts.push(board::Ghost {
            text: "…".to_owned(),
            rect: Rectangle::new(Point::new(center.x - w / 2.0, center.y - h / 2.0), Size::new(w, h)),
            parent,
            placeholder: true,
        });
        self.ghosts_since = Instant::now();
        self.ghosts_appeared = Instant::now();
        self.ghosts_pinned = false;
        self.ghost_hovered = false;
        self.cache.clear();
    }

    /// Turns a ghost suggestion into a real node branching off its parent,
    /// keeping the ghost's placement. Undoable.
    fn accept_ghost(&mut self, ghost: board::Ghost) -> Task<Message> {
        self.push_undo();
        let node_id = model::random_id();
        let mut node = CanvasNode {
            id: node_id.clone(),
            kind: "text".to_owned(),
            text: Some(ghost.text),
            file: None,
            x: ghost.rect.x,
            y: ghost.rect.y,
            width: ghost.rect.width,
            height: ghost.rect.height,
            color: None,
            extra: Default::default(),
        };
        snap_node_geometry(&mut node, self.snap_grid());
        self.nodes.push(node);
        let index = self.nodes.len() - 1;
        self.spawning.insert(index, Instant::now());

        // Only need to know the parent still exists; keep the ghost's placement.
        if self.nodes.iter().any(|n| n.id == ghost.parent) {
            self.edges.push(CanvasEdge {
                id: model::random_id(),
                from_node: ghost.parent,
                from_side: "right".to_owned(),
                to_node: node_id,
                to_side: board::opposite_side("right").to_owned(),
                extra: Default::default(),
            });
        }
        self.autosize_node(index, true);
        self.auto_relayout(index, false);
        self.mark_dirty();
        self.cache.clear();
        Task::batch([self.refresh_snapshots(), self.schedule_recluster()])
    }

    /// Asks the AI with the chain of notes feeding `source`, spawning a response
    /// node branching to its right and streaming the answer into it. A `---` in
    /// the answer starts a new sibling note (up to `config.max_ai_nodes`).
    fn trigger_ai(&mut self, source: usize) -> Task<Message> {
        if !self.config.ai_enabled() {
            return Task::none();
        }
        let Some(source_node) = self.nodes.get(source) else {
            return Task::none();
        };
        let source_id = source_node.id.clone();
        let prompt = ai::upstream_prompt(&self.nodes, &self.edges, &source_id);
        // Attach any upstream image notes as vision inputs (downscaled).
        let images: Vec<String> = ai::upstream_images(&self.nodes, &self.edges, &source_id)
            .iter()
            .filter_map(|file| encode_image_data_url(&self.base_path.as_path().join(file)))
            .collect();
        if prompt.trim().is_empty() && images.is_empty() {
            return Task::none();
        }

        // The chain feeding the request: the ancestor notes (which include the
        // clicked node) and the edges among them. The shimmer sweeps this once,
        // ending on the clicked node — the response edge is deliberately left
        // out so the sweep settles on the input, not the (dotted) answer node.
        let ancestors: HashSet<String> = ai::upstream_nodes(&self.nodes, &self.edges, &source_id)
            .iter()
            .map(|node| node.id.clone())
            .collect();
        let flow: Vec<String> = self
            .edges
            .iter()
            .filter(|e| ancestors.contains(&e.from_node) && ancestors.contains(&e.to_node))
            .map(|e| e.id.clone())
            .collect();

        // The first response note starts as animated dots.
        let Some(node_id) = self.spawn_ai_node(&source_id, ".") else {
            return Task::none();
        };
        self.ai_pending.insert(node_id.clone());
        self.ai_flows.push(board::AiFlow {
            nodes: ancestors,
            edges: flow.into_iter().collect(),
            target: node_id.clone(),
            since: Instant::now(),
        });

        let (url, model, key, max_nodes) = (
            self.config.ai_url.clone(),
            self.config.ai_model.clone(),
            self.config.ai_api_key.clone(),
            self.config.max_ai_nodes,
        );
        let thinking = self.config.ai_thinking;
        let stream_id = node_id.clone();
        // Abortable so deleting the response node cancels the request.
        let (task, handle) = Task::run(
            ai::stream(url, model, key, prompt, images, max_nodes, thinking),
            move |event| Message::Ai(stream_id.clone(), event),
        )
        .abortable();
        self.ai_streams.insert(
            node_id.clone(),
            AiStream {
                source: source_id,
                current: node_id.clone(),
                count: 1,
                handle,
                nodes: vec![node_id],
            },
        );
        task
    }

    /// Marks the AI node `id` no longer pending (its first token arrived, or it
    /// finished/failed/was deleted). Once nothing is pending, the input-flow
    /// animation ends. Returns whether `id` was pending.
    fn end_ai_pending(&mut self, id: &str) -> bool {
        let was_pending = self.ai_pending.remove(id);
        // Drop this request's shimmer once its answer starts (or it ends).
        self.ai_flows.retain(|flow| flow.target != id);
        was_pending
    }

    /// Aborts and forgets any AI stream whose response node is among `removed`,
    /// so deleting a response node (even mid-generation) cancels its request
    /// instead of leaving it running and writing into a node that's gone.
    fn cancel_ai_streams(&mut self, removed: &HashSet<String>) {
        let cancel = ai_streams_to_cancel(
            self.ai_streams.iter().map(|(id, s)| (id.as_str(), s.current.as_str())),
            removed,
        );
        for id in cancel {
            if let Some(stream) = self.ai_streams.remove(&id) {
                stream.handle.abort();
            }
            self.end_ai_pending(&id);
        }
    }

    /// Pastes the clipboard (text or image) as a new node near `world`.
    /// Undoable; a no-op when the clipboard holds nothing pasteable.
    /// Copies the selected nodes and the edges among them to the clipboard.
    fn copy_selected_nodes(&self) {
        let selected: Vec<&CanvasNode> =
            self.selected.iter().filter_map(|&i| self.nodes.get(i)).collect();
        if selected.is_empty() {
            return;
        }
        let ids: HashSet<&str> = selected.iter().map(|n| n.id.as_str()).collect();
        let nodes = selected.into_iter().cloned().collect();
        // Only the edges wholly within the copied set carry over.
        let edges = self
            .edges
            .iter()
            .filter(|e| ids.contains(e.from_node.as_str()) && ids.contains(e.to_node.as_str()))
            .cloned()
            .collect();
        clipboard::write_nodes(&clipboard::NodeClip { nodes, edges });
    }

    /// Pastes previously copied nodes: fresh ids, the whole group dropped near
    /// `world` (avoiding overlap), internal edges re-linked, and the new nodes
    /// left selected. Undoable.
    fn paste_nodes(&mut self, clip: clipboard::NodeClip, world: Point) -> Task<Message> {
        if clip.nodes.is_empty() {
            return Task::none();
        }
        self.push_undo();

        // Shift the whole group so its center lands at the drop point.
        let (mut min_x, mut min_y, mut max_x, mut max_y) =
            (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for n in &clip.nodes {
            min_x = min_x.min(n.x);
            min_y = min_y.min(n.y);
            max_x = max_x.max(n.x + n.width);
            max_y = max_y.max(n.y + n.height);
        }
        let (w, h) = (max_x - min_x, max_y - min_y);
        let group_center = Point::new((min_x + max_x) / 2.0, (min_y + max_y) / 2.0);
        let free = self.find_free_placement(world, w.max(1.0), h.max(1.0));
        let (dx, dy) = (free.x - group_center.x, free.y - group_center.y);

        let mut id_map: HashMap<String, String> = HashMap::new();
        let mut selection = Vec::new();
        for mut node in clip.nodes {
            let new_id = model::random_id();
            id_map.insert(node.id.clone(), new_id.clone());
            node.id = new_id.clone();
            node.x += dx;
            node.y += dy;
            snap_node_geometry(&mut node, self.snap_grid());
            // File nodes reference the same image on disk; load its handle.
            if let Some(file) = node.file.as_deref() {
                self.images
                    .insert(new_id, image::Handle::from_path(self.base_path.as_path().join(file)));
            }
            self.nodes.push(node);
            let index = self.nodes.len() - 1;
            self.spawning.insert(index, Instant::now());
            selection.push(index);
        }
        for mut edge in clip.edges {
            let (Some(from), Some(to)) =
                (id_map.get(&edge.from_node), id_map.get(&edge.to_node))
            else {
                continue;
            };
            edge.id = model::random_id();
            edge.from_node = from.clone();
            edge.to_node = to.clone();
            self.edges.push(edge);
        }

        self.selected = selection;
        self.selected_edge = None;
        self.mark_dirty();
        self.cache.clear();
        Task::batch([self.refresh_snapshots(), self.schedule_recluster()])
    }

    fn paste_at(&mut self, world: Point) -> Task<Message> {
        // Copied nodes take priority over their JSON's textual form.
        if let Some(clip) = clipboard::read_nodes() {
            return self.paste_nodes(clip, world);
        }
        let node = match clipboard::read() {
            Some(Clip::Image {
                width,
                height,
                rgba,
            }) => match self.write_pasted_image(width, height, &rgba) {
                Ok((file, path, id)) => {
                    // Fit the image to a comfortable size, keeping its aspect.
                    let (w, h) = fit_image_size(width, height, self.config.max_node_width);
                    let center = self.find_free_placement(world, w, h);
                    self.images.insert(id.clone(), image::Handle::from_path(path));
                    CanvasNode {
                        id,
                        kind: "file".to_owned(),
                        text: None,
                        file: Some(file),
                        x: center.x - w / 2.0,
                        y: center.y - h / 2.0,
                        width: w,
                        height: h,
                        color: None,
                        extra: Default::default(),
                    }
                }
                Err(error) => {
                    eprintln!("paste: could not save image ({error})");
                    return Task::none();
                }
            },
            Some(Clip::Text(text)) => {
                let width = self.config.new_node_min_width;
                let lines = text.lines().count().max(1) as f32;
                let height =
                    (lines * 22.0 + 24.0).clamp(self.config.new_node_min_height, 400.0);
                let center = self.find_free_placement(world, width, height);
                CanvasNode {
                    id: model::random_id(),
                    kind: "text".to_owned(),
                    text: Some(text),
                    file: None,
                    x: center.x - width / 2.0,
                    y: center.y - height / 2.0,
                    width,
                    height,
                    color: None,
                    extra: Default::default(),
                }
            }
            None => return Task::none(),
        };

        self.push_undo();
        let mut node = node;
        snap_node_geometry(&mut node, self.snap_grid());
        self.nodes.push(node);
        let index = self.nodes.len() - 1;
        self.spawning.insert(index, Instant::now());
        self.mark_dirty();
        self.cache.clear();
        self.focus_on_node(index);
        Task::batch([self.refresh_snapshots(), self.schedule_recluster()])
    }

    /// Writes a pasted image as a PNG in the canvas file's directory. Returns
    /// the node's `file` value (relative to the vault base, as Obsidian
    /// stores it), the absolute path (for the in-memory image handle), and a
    /// fresh node id reused as the file stem.
    fn write_pasted_image(
        &self,
        width: usize,
        height: usize,
        rgba: &[u8],
    ) -> Result<(String, PathBuf, String), String> {
        let id = model::random_id();
        let dir = self
            .canvas_path
            .parent()
            .map(FsPath::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let name = format!("pasted-{id}.png");
        let path = dir.join(&name);
        // `::image` (the crate) — `image` alone resolves to `iced::widget::image`.
        ::image::save_buffer(
            &path,
            rgba,
            width as u32,
            height as u32,
            ::image::ExtendedColorType::Rgba8,
        )
        .map_err(|e| e.to_string())?;

        let file = vault_relative(&path, self.base_path.as_path());
        Ok((file, path, id))
    }

    fn begin_editing(&mut self, index: usize, caret_at: Option<Point>) -> Task<Message> {
        let Some(node) = self.nodes.get(index) else {
            return Task::none();
        };
        if node.kind != "text" {
            return Task::none();
        }
        // Capture a pre-edit snapshot to make the edit undoable — but not for
        // a brand-new node (its creation isn't an undoable edit).
        self.edit_undo = (self.fresh_node != Some(index)).then(|| self.snapshot());
        self.text_snapshots.remove(&index);
        self.staging_snapshots.remove(&index);
        let text = node.text.as_deref().unwrap_or("").trim().to_owned();
        let mut editing = Editing::new(index, &text);
        // Defer caret placement to the first frame (see `pending_caret`); a
        // `window::frames()` subscription then fires `PlaceEditorCaret`.
        editing.pending_caret = caret_at;
        self.editing = Some(editing);
        self.cache.clear();
        // With scrollbars on, editing a node re-grows it to fit (its shrink was
        // temporary): drop its scroll and autosize before anything else, so the
        // editor lays out over the full content rather than a scrolled window.
        if self.config.node_scrollbars {
            if let Some(id) = self.nodes.get(index).map(|n| n.id.clone()) {
                self.node_scroll.remove(&id);
            }
            self.autosize_node(index, false);
        }
        // The edit overlay is a fixed-size widget clamped to the viewport, so a
        // node hanging off an edge (or larger than the screen) would rewrap/clip.
        // Bring it fully into view first so the editor lays out like read mode.
        self.ensure_node_visible_for_edit(index);
        iced::advanced::widget::operate(
            iced::advanced::widget::operation::focusable::focus_next(),
        )
    }

    /// Whether node `index` sits fully inside the viewport with a small margin —
    /// the condition under which the edit overlay (a viewport-clamped widget)
    /// lays out cleanly. Drives fit-on-edit and exit-editing-on-pan-out.
    fn node_fully_visible(&self, index: usize) -> bool {
        const MARGIN: f32 = 12.0;
        let Some(node) = self.nodes.get(index) else {
            return false;
        };
        let rect = self.camera.rect_to_screen(node_world_rect(node));
        let vp = self.camera.viewport;
        rect.x >= MARGIN
            && rect.y >= MARGIN
            && rect.x + rect.width <= vp.width - MARGIN
            && rect.y + rect.height <= vp.height - MARGIN
    }

    /// If editing and a pan/zoom has moved the node out of view, commit and leave
    /// editing: the edit overlay can't render off the top/left (widget clamp), so
    /// its text would otherwise detach from the canvas-drawn card.
    fn exit_edit_if_offscreen(&mut self) -> Task<Message> {
        match self.editing.as_ref().map(|e| e.index) {
            Some(index) if !self.node_fully_visible(index) => Task::done(Message::StopEditing),
            _ => Task::none(),
        }
    }

    /// Flies the camera to center the node being edited when it doesn't fully fit
    /// in the viewport (zooming out to fit if it's larger than the screen); a
    /// no-op when it already fits, so on-screen edits don't move the view.
    fn ensure_node_visible_for_edit(&mut self, index: usize) {
        const MARGIN: f32 = 12.0;
        if self.node_fully_visible(index) {
            return;
        }
        let Some(node) = self.nodes.get(index) else {
            return;
        };
        let vp = self.camera.viewport;
        let center = Vector::new(node.x + node.width / 2.0, node.y + node.height / 2.0);
        // Largest zoom at which the node fits with margin; never zoom in past the
        // current level — only pull back when it's too big to fit.
        let fit = ((vp.width - 2.0 * MARGIN) / node.width)
            .min((vp.height - 2.0 * MARGIN) / node.height);
        let target_zoom =
            self.camera.zoom.min(fit).clamp(self.config.min_zoom, self.config.max_zoom);
        self.camera_focus = Some(CameraFocus {
            target_zoom,
            target_offset: center * -target_zoom,
            last_tick: Instant::now(),
        });
    }

    /// Screen-space top-left of the editing overlay's text for `index` — the
    /// origin a click maps against for deferred caret placement. Mirrors the
    /// editor geometry in `view` (keep the two in sync).
    fn editor_text_origin(&self, index: usize) -> Option<Point> {
        let node = self.nodes.get(index)?;
        let editing = self.editing.as_ref()?;
        let rect = self.camera.rect_to_screen(node_world_rect(node));
        let zoom = self.camera.zoom;
        let wrap = (node.width - 12.0).max(20.0);
        // The editor renders inset a `MD_PADDING`-equivalent each side (like read
        // mode), so measure and size the text at that inner column, not the full
        // card wrap — otherwise a wrapping note measures a line short.
        let text_wrap = (wrap - 2.0 * EDITOR_INSET).max(20.0);
        let text = editing.content.text();
        let (text_width, text_height) =
            text_raster::measure_source(&text, text_wrap, FONT_SIZE, zoom, &self.theme);
        let caret_slack = 4.0;
        let editor_height = text_height + 2.0;
        let centered = self.config.center_node_text && markdown::is_simple(&text);
        let editor_width = if centered {
            (text_width + caret_slack).clamp(24.0 * zoom, wrap * zoom + caret_slack)
        } else {
            (text_wrap * zoom).min(rect.width)
        };
        let editor_left = (rect.x + (rect.width - editor_width) / 2.0).max(0.0);
        let editor_top = (rect.y + (rect.height - editor_height) / 2.0).max(0.0);
        Some(Point::new(editor_left, editor_top))
    }

    /// Hides the overlay window, keeping the app resident for the next
    /// summon.
    fn hide_window(&mut self) -> Task<Message> {
        self.phase = Phase::Hidden;
        self.reveal = 0.0;
        self.cache.clear();
        let Some(id) = self.main_window else {
            return Task::none();
        };
        // macOS/Windows keep the window resident-but-hidden, re-summoned by the
        // hotkey or the tray. Wayland can't hide a window in place, so Linux
        // *closes* it (the daemon survives with none open) and the tray reopens
        // one. Programmatic close doesn't emit a close-request, so this won't
        // trip the quit-on-close subscription.
        #[cfg(target_os = "linux")]
        {
            self.main_window = None;
            window::close(id)
        }
        #[cfg(not(target_os = "linux"))]
        {
            window::set_mode(id, window::Mode::Hidden)
        }
    }

    /// The overlay window's configuration, rebuilt for a re-summon (Linux closes
    /// the window to hide, so summoning opens a fresh one).
    #[cfg(target_os = "linux")]
    fn overlay_settings(&self) -> window::Settings {
        startup::window_settings(&self.config, !self.overlay)
    }

    fn view(&self) -> Element<'_, Message> {
        let camera = self.display_camera();
        let content_alpha = if self.overlay {
            camera::ease_out_cubic(self.reveal).clamp(0.0, 1.0)
        } else {
            1.0
        };

        // Bottom layer: the dim (overlay) or dark fill (windowed).
        let backdrop_color = if self.overlay {
            iced::Color {
                a: self.config.overlay_dim * content_alpha,
                ..iced::Color::BLACK
            }
        } else {
            board::BACKGROUND
        };
        let backdrop = container(Space::new().width(Fill).height(Fill))
            .width(Fill)
            .height(Fill)
            .style(move |_| container::Style {
                background: Some(backdrop_color.into()),
                ..container::Style::default()
            });

        // Above the dim, below the cards: analytic GPU shadows. Node and
        // cluster shadows crossfade together with their cards.
        let cluster_weight = if self.clusters.is_empty() {
            0.0
        } else {
            board::cluster_opacity(camera.zoom)
        };
        let mut shadow_instances: Vec<shadows::Instance> = Vec::new();

        if cluster_weight < 1.0 {
            let node_alpha = 0.6 * content_alpha * (1.0 - cluster_weight);
            shadow_instances.extend(self.nodes.iter().enumerate().map(|(index, node)| {
                let mut rect = camera.rect_to_screen(node_world_rect(node));
                let mut alpha = node_alpha;
                // Track the spawn zoom-in of freshly created nodes.
                if let Some(started) = self.spawning.get(&index) {
                    let t = (started.elapsed().as_secs_f32()
                        / self.config.anim_secs(SPAWN_DURATION.as_secs_f32()))
                        .clamp(0.0, 1.0);
                    let scale = 0.2 + 0.8 * camera::ease_out_back(t);
                    let center = rect.center();
                    rect = Rectangle::new(
                        Point::new(
                            center.x - rect.width * scale / 2.0,
                            center.y - rect.height * scale / 2.0,
                        ),
                        Size::new(rect.width * scale, rect.height * scale),
                    );
                    alpha *= camera::ease_out_cubic(t);
                }
                shadows::Instance {
                    pos: [rect.x + 5.0 * camera.zoom, rect.y + 7.0 * camera.zoom],
                    size: [rect.width, rect.height],
                    corner_radius: 8.0 * camera.zoom,
                    blur: (22.0 * camera.zoom).max(0.5),
                    alpha,
                    _pad: 0.0,
                }
            }));
        }

        if cluster_weight > 0.0 {
            let cluster_alpha = 0.6 * content_alpha * cluster_weight;
            shadow_instances.extend(self.clusters.iter().map(|cluster| {
                let rect = camera.rect_to_screen(cluster.bounds);
                shadows::Instance {
                    pos: [rect.x + 5.0 * camera.zoom, rect.y + 7.0 * camera.zoom],
                    size: [rect.width, rect.height],
                    corner_radius: 12.0 * camera.zoom,
                    blur: (22.0 * camera.zoom).max(0.5),
                    alpha: cluster_alpha,
                    _pad: 0.0,
                }
            }));
        }
        let shadow_layer = shader(shadows::ShadowLayer {
            instances: shadow_instances,
        })
        .width(Fill)
        .height(Fill);

        let board = iced_canvas(Board {
            nodes: &self.nodes,
            node_index: self
                .nodes
                .iter()
                .enumerate()
                .map(|(index, node)| (node.id.as_str(), index))
                .collect(),
            edges: &self.edges,
            images: &self.images,
            clusters: &self.clusters,
            cluster_links: &self.cluster_links,
            cluster_snapshots: &self.cluster_snapshots,
            config: &self.config,
            theme: &self.theme,
            camera,
            top_safe_inset: self.top_safe_inset,
            cache: &self.cache,
            editing: self.editing.as_ref().map(|editing| editing.index),
            overlay: self.overlay,
            // The settings modal and the command palette both own input while
            // open, so board shortcuts/scroll don't fire underneath them.
            modal: self.settings_overlay || self.command_palette.is_some(),
            show_fps: self.show_fps,
            spawning: &self.spawning,
            text_snapshots: &self.text_snapshots,
            staging_snapshots: &self.staging_snapshots,
            node_scroll: &self.node_scroll,
            reveal: self.reveal,
            selected: &self.selected,
            selected_edge: self.selected_edge,
            hovered_edge: self.hovered_edge,
            search: self.search.as_ref().map(|s| board::SearchView {
                query: &s.query,
                current_node: s.matches.get(s.current).map(|m| m.node),
            }),
            ai_flows: &self.ai_flows,
            ghosts: &self.ghosts,
            ghost_fade: self.ghost_fade(),
            ghost_fade_in: self.ghost_fade_in(),
            ghost_hovered: self.ghost_hovered,
        })
        .width(Fill)
        .height(Fill);

        let mut layers = stack![backdrop, shadow_layer, board];

        if let Some(editing) = &self.editing {
            let node = &self.nodes[editing.index];
            let rect = camera.rect_to_screen(node_world_rect(node));
            let zoom = camera.zoom;

            // Size the editor to its content and center the block in the
            // node, so editing keeps the card's shape and (approximately)
            // the canvas text's centered layout — the widget itself only
            // supports top-left alignment.
            let wrap = (node.width - 12.0).max(20.0);
            // The editor renders inset an `EDITOR_INSET` each side (matching read
            // mode's `MD_PADDING`), so measure and size the text at that inner
            // column, not the full card wrap — otherwise a wrapping note measures
            // a line short and the box mis-centers.
            let text_wrap = (wrap - 2.0 * EDITOR_INSET).max(20.0);
            let text = editing.content.text();
            let (text_width, text_height) =
                text_raster::measure_source(&text, text_wrap, FONT_SIZE, zoom, &self.theme);
            let caret_slack = 4.0;
            let editor_height = text_height + 2.0;

            // Match read mode's alignment while editing: a centered block that
            // hugs the text for simple content, or the full (left-aligned) wrap
            // column when the read view left-aligns it.
            let centered = self.config.center_node_text && markdown::is_simple(&text);
            // The editor box hugs the text (centered nodes) or spans the wrap
            // column (left nodes); either way it's centered in the card, so the
            // left inset matches read mode. Line alignment inside is set below.
            let editor_width = if centered {
                (text_width + caret_slack).clamp(24.0 * zoom, wrap * zoom + caret_slack)
            } else {
                (text_wrap * zoom).min(rect.width)
            };
            let editor_left = (rect.x + (rect.width - editor_width) / 2.0).max(0.0);
            // The editor widget has no alignment API; the vendored iced_graphics
            // reads this to align its lines (center vs left) to match read mode.
            iced_graphics::text::editor::set_editor_centered(centered);

            let editor = text_editor(&editing.content)
                .on_action(Message::EditorAction)
                .key_binding(|key_press| {
                    use iced::keyboard::Key;
                    use iced::keyboard::key::Named;

                    // Delete the word to the left: select it, then delete it.
                    let delete_word_left = || {
                        Some(text_editor::Binding::Sequence(vec![
                            text_editor::Binding::Select(text_editor::Motion::WordLeft),
                            text_editor::Binding::Backspace,
                        ]))
                    };
                    // On macOS, Option+Backspace (the standard delete-word chord —
                    // and what Ctrl+Backspace surfaces as here) also deletes the
                    // previous word, alongside the Cmd+Backspace handled in the
                    // `command()` block below. (Elsewhere `command()` is Ctrl, so
                    // that block already covers Ctrl+Backspace.)
                    #[cfg(target_os = "macos")]
                    if (key_press.modifiers.alt() || key_press.modifiers.control())
                        && matches!(key_press.key, Key::Named(Named::Backspace))
                    {
                        return delete_word_left();
                    }

                    // Cmd+B/I wrap the selection; Cmd+Z / Cmd+Shift+Z drive our
                    // text-level undo; Cmd+Enter commits the edit; Cmd+Backspace
                    // deletes the previous word; Cmd+Up/Down jump to the document
                    // ends (iced's default only remaps Cmd+Left/Right to line
                    // ends), with Shift extending the selection. Everything else
                    // keeps the default binding. `command()` is Cmd on macOS and
                    // Ctrl on Windows/Linux, so these are platform-correct.
                    //
                    // Letter chords match on the *latin* key for the physical
                    // position (same trick iced uses for copy/paste), so they
                    // fire regardless of the active keyboard layout.
                    if key_press.modifiers.command() {
                        let latin = key_press.key.to_latin(key_press.physical_key);
                        match (latin, &key_press.key) {
                            (Some('b'), _) => {
                                return Some(text_editor::Binding::Custom(
                                    Message::WrapSelection("**"),
                                ));
                            }
                            (Some('i'), _) => {
                                return Some(text_editor::Binding::Custom(
                                    Message::WrapSelection("*"),
                                ));
                            }
                            (Some('z'), _) => {
                                return Some(text_editor::Binding::Custom(
                                    if key_press.modifiers.shift() {
                                        Message::RedoEdit
                                    } else {
                                        Message::UndoEdit
                                    },
                                ));
                            }
                            // Cmd/Ctrl+Enter commits (Enter alone is a newline,
                            // since notes are multi-line).
                            (_, Key::Named(Named::Enter)) => {
                                return Some(text_editor::Binding::Custom(
                                    Message::StopEditing,
                                ));
                            }
                            // Cmd/Ctrl+Backspace deletes the word to the left.
                            (_, Key::Named(Named::Backspace)) => {
                                return delete_word_left();
                            }
                            (_, Key::Named(named)) => {
                                let motion = match named {
                                    Named::ArrowUp => Some(text_editor::Motion::DocumentStart),
                                    Named::ArrowDown => Some(text_editor::Motion::DocumentEnd),
                                    _ => None,
                                };
                                if let Some(motion) = motion {
                                    return Some(if key_press.modifiers.shift() {
                                        text_editor::Binding::Select(motion)
                                    } else {
                                        text_editor::Binding::Move(motion)
                                    });
                                }
                            }
                            _ => {}
                        }
                    }
                    text_editor::Binding::from_key_press(key_press)
                })
                .font(md_edit::Palette::from_theme(&self.theme).base_font())
                .size(FONT_SIZE * zoom)
                .line_height(iced::advanced::text::LineHeight::Relative(1.2))
                .padding(0)
                .width(editor_width)
                .height(Length::Fixed(editor_height))
                .style(editor_style)
                // Obsidian-style live styling: markers stay visible, content is
                // styled (bold/italic/code/links/headings) as you type.
                .highlight_with::<md_edit::MdHighlighter>(
                    md_edit::Palette::from_theme(&self.theme),
                    md_edit::to_format,
                );

            layers = layers.push(container(editor).width(Fill).height(Fill).padding(Padding {
                top: (rect.y + (rect.height - editor_height) / 2.0).max(0.0),
                left: editor_left.max(0.0),
                right: 0.0,
                bottom: 0.0,
            }));
        }

        if let Some(menu) = &self.context_menu {
            // A full-screen catcher below the menu dismisses it on any click
            // outside the menu items.
            let catcher = mouse_area(Space::new().width(Fill).height(Fill))
                .on_press(Message::CloseContextMenu);
            layers = layers.push(catcher);

            // Shared chrome styles (captured by Copy, so each `.style()` reuses
            // them) — the menu recolors with the theme like the palette does.
            let theme = self.theme;
            let item_style = move |_t: &Theme, s: button::Status| chrome::menu_item(&theme, s, false);
            let delete_style = move |_t: &Theme, s: button::Status| chrome::danger_item(&theme, s);
            let sep_style = move |_t: &Theme| chrome::separator(&theme);

            // A menu row with a fixed leading "check column": toggle rows draw a
            // real, centered checkbox; plain rows reserve the same column so every
            // label lines up (rather than the old baseline ☑/☐ text glyphs, which
            // sat small on the baseline and left non-toggle rows un-indented).
            let (accent, muted, font) = (theme.accent, theme.text_muted, theme.ui_font());
            let item = move |check: Option<bool>, label: &'static str, msg: Message| -> Element<Message> {
                const BOX: f32 = 15.0;
                let gutter: Element<Message> = match check {
                    Some(on) => {
                        // Black or white tick, whichever contrasts with the accent fill.
                        let lum = 0.299 * accent.r + 0.587 * accent.g + 0.114 * accent.b;
                        let tick = if lum > 0.6 { iced::Color::BLACK } else { iced::Color::WHITE };
                        let mark: Element<Message> = if on {
                            text("✓").size(11).font(font).color(tick).into()
                        } else {
                            Space::new().into()
                        };
                        container(mark)
                            .width(BOX)
                            .height(BOX)
                            .align_x(iced::alignment::Horizontal::Center)
                            .align_y(iced::alignment::Vertical::Center)
                            .style(move |_t: &Theme| container::Style {
                                // Off: a clearly-visible empty box (muted border +
                                // faint fill) so the row still reads as a toggle.
                                background: Some(if on {
                                    accent.into()
                                } else {
                                    iced::Color { a: 0.08, ..muted }.into()
                                }),
                                border: iced::Border {
                                    radius: 4.0.into(),
                                    width: 1.0,
                                    color: if on { accent } else { muted },
                                },
                                ..container::Style::default()
                            })
                            .into()
                    }
                    None => Space::new().width(BOX).height(BOX).into(),
                };
                button(
                    row![gutter, text(label).size(14).font(font)]
                        .spacing(8)
                        .align_y(iced::alignment::Vertical::Center),
                )
                .width(Fill)
                .on_press(msg)
                .padding([6, 12])
                .style(item_style)
                .into()
            };

            let content: Element<Message> = match menu.kind {
                ContextMenuKind::Canvas { world } => {
                    let sep = || {
                        container(Space::new().width(Fill).height(1.0)).style(sep_style)
                    };
                    column![
                        item(None, "Add node", Message::CreateNode { world }),
                        sep(),
                        item(Some(self.config.auto_layout), "Auto-layout", Message::ToggleAutoLayout),
                        sep(),
                        item(Some(self.config.snap_to_grid), "Snap to grid", Message::ToggleSnap),
                        sep(),
                        item(
                            Some(self.config.ai_completions),
                            "AI suggestions",
                            Message::ToggleAiCompletions,
                        ),
                        sep(),
                        item(None, "Paste", Message::Paste { world }),
                    ]
                    .width(Length::Fixed(200.0))
                    .spacing(4)
                    .into()
                }
                ContextMenuKind::Node => {
                    // Swatch/wheel size and gap; the hue bar matches the swatch
                    // height so expanding the picker doesn't change the row
                    // height.
                    const SW: f32 = 24.0;
                    const GAP: f32 = 6.0;

                    let sep = || {
                        container(Space::new().width(Fill).height(1.0)).style(sep_style)
                    };
                    let delete_label = if self.selected.len() > 1 {
                        format!("Delete {} nodes", self.selected.len())
                    } else {
                        "Delete".to_owned()
                    };
                    let delete = button(text(delete_label).size(14))
                        .width(Fill)
                        .on_press(Message::DeleteSelected)
                        .padding([6, 12])
                        .style(delete_style);

                    // Fixed menu width — presets + optional extra + wheel — so
                    // expanding the picker never resizes the menu.
                    let extra = self
                        .extra_swatch
                        .as_deref()
                        .filter(|e| !NODE_PALETTE.contains(e));
                    let menu_width = self.node_menu_width();

                    let body: Element<Message> = if self.color_picker_open {
                        // A smooth hue bar, same height as the swatch row, that
                        // unrolls from center when the picker opens.
                        let reveal = match self.picker_anim {
                            Some(start) => camera::ease_out_cubic(
                                (start.elapsed().as_secs_f32() / self.config.anim_secs(PICKER_REVEAL))
                                    .clamp(0.0, 1.0),
                            ),
                            None => 1.0,
                        };
                        iced_canvas(HuePicker { reveal })
                            .width(Fill)
                            .height(Length::Fixed(SW))
                            .into()
                    } else {
                        let mut colors: Vec<String> =
                            NODE_PALETTE.iter().map(|s| (*s).to_owned()).collect();
                        if let Some(extra) = extra {
                            colors.push(extra.to_owned());
                        }
                        // Leading "default" swatch (the default card color)
                        // clears the node's color.
                        let default_color = self.theme.card_fill;
                        let mut row = Row::new()
                            .push(
                                button(Space::new().width(SW).height(SW))
                                    .padding(0)
                                    .on_press(Message::SetNodeColor(None))
                                    .style(move |_theme, status| {
                                        swatch_style(default_color, status)
                                    }),
                            )
                            .spacing(GAP);
                        for hex in colors {
                            let color = model::parse_color(&hex).unwrap_or(iced::Color::WHITE);
                            row = row.push(
                                button(Space::new().width(SW).height(SW))
                                    .padding(0)
                                    .on_press(Message::SetNodeColor(Some(hex)))
                                    .style(move |_theme, status| swatch_style(color, status)),
                            );
                        }
                        // The rainbow wheel that expands the hue picker.
                        row = row.push(
                            button(Space::new().width(SW).height(SW))
                                .padding(0)
                                .on_press(Message::ExpandColorPicker)
                                .style(|_theme, status| wheel_style(status)),
                        );
                        row.into()
                    };

                    let mut items: Vec<Element<Message>> =
                        vec![container(body).padding([6, 12]).width(Fill).into()];
                    // "Ask AI" only when an AI endpoint is configured (not "Off").
                    if self.config.ai_enabled() {
                        items.push(sep().into());
                        items.push(
                            button(text("Ask AI").size(14))
                                .width(Fill)
                                .on_press(Message::AskAi)
                                .padding([6, 12])
                                .style(item_style)
                                .into(),
                        );
                    }
                    items.push(sep().into());
                    // "Distribute" (Mermaid-style auto-layout) only applies to
                    // more than one node.
                    if self.selected.len() > 1 {
                        // "Auto-layout →" runs the default left-to-right layout;
                        // the small ↓ chip runs top-down.
                        let main = button(text("Auto-layout \u{2192}").size(14))
                            .width(Fill)
                            .on_press(Message::AutoLayout(layout::Direction::Right))
                            .padding([6, 12])
                            .style(item_style);
                        let down = button(text("\u{2193}").size(14).width(Fill).center())
                            .width(Length::Fixed(32.0))
                            .padding([6, 0])
                            .on_press(Message::AutoLayout(layout::Direction::Down))
                            .style(chip_style);
                        let layout_row = Row::new()
                            .push(main)
                            .push(down)
                            .spacing(6)
                            .align_y(iced::alignment::Vertical::Center)
                            .width(Fill)
                            .padding(Padding {
                                top: 0.0,
                                right: 12.0,
                                bottom: 0.0,
                                left: 0.0,
                            });
                        items.push(layout_row.into());
                        items.push(sep().into());
                    }
                    items.push(delete.into());

                    Column::with_children(items)
                        .width(Length::Fixed(menu_width))
                        .spacing(4)
                        .into()
                }
                ContextMenuKind::Link => button(text("Delete").size(14))
                    .on_press(Message::DeleteSelected)
                    .padding([6, 12])
                    .style(delete_style)
                    .into(),
            };
            let panel = container(content)
                .padding(4)
                .style(move |_t: &Theme| chrome::panel(&theme, theme.elevated()));
            // Keep the menu on-screen: shift it in from the right/bottom edges
            // so a click near a corner doesn't clip it.
            let size = self.context_menu_size();
            let viewport = self.camera.viewport;
            const EDGE: f32 = 8.0;
            let left = menu
                .screen
                .x
                .min((viewport.width - size.width - EDGE).max(0.0))
                .max(0.0);
            let top = menu
                .screen
                .y
                .min((viewport.height - size.height - EDGE).max(0.0))
                .max(0.0);
            layers = layers.push(container(panel).width(Fill).height(Fill).padding(Padding {
                top,
                left,
                right: 0.0,
                bottom: 0.0,
            }));
        }

        if let Some(search) = &self.search {
            layers = layers.push(self.search_bar(search));
        }
        if let Some(palette) = &self.command_palette {
            layers = layers.push(command::view(self, palette));
        }
        if self.settings_overlay {
            layers = layers.push(self.settings_overlay_view());
        }

        layers.into()
    }

    /// The settings form as an in-board modal: a dim backdrop (click to close)
    /// behind a centered, scrollable panel with a titled header and × button.
    /// Opened from the command palette; the tray still uses the settings window.
    fn settings_overlay_view(&self) -> Element<'_, Message> {
        let theme = &self.theme;
        let ct = self.theme;
        let font = theme.ui_font();
        let (text_c, muted, accent) = (theme.text, theme.text_muted, theme.accent);

        let close = button(text("×").size(20).font(font).color(muted))
            .padding([0, 8])
            .on_press(Message::CloseSettingsOverlay)
            .style(move |_theme, status| chrome_button_style(status, muted, accent));

        let header = row![
            text("Settings").size(17).font(font).color(text_c).width(Fill),
            close,
        ]
        .align_y(iced::Alignment::Center);

        // Render the form under the settings window's own theme, so its
        // background and widget colors match the standalone Settings window
        // exactly (the board window is `Theme::Dark`, which would recolor it).
        let form = iced::widget::themer(Some(startup::settings_theme(self)), settings::view(self));

        let body = column![
            header,
            iced::widget::scrollable(form).height(Length::Shrink),
        ]
        .spacing(12);

        // The panel shares the form's own background so there's no seam (the
        // form below is themed to match).
        let panel = container(body)
            .width(Length::Fixed(600.0))
            .max_height(720.0)
            .padding(20)
            .style(move |_theme| chrome::panel(&ct, ct.background));

        // A click on the panel is swallowed; a click on the backdrop closes.
        let panel = mouse_area(panel).on_press(Message::Noop);

        let backdrop = mouse_area(
            container(panel)
                .width(Fill)
                .height(Fill)
                .center_x(Fill)
                .center_y(Fill)
                .style(|_theme| container::Style {
                    background: Some(iced::Color { a: 0.45, ..iced::Color::BLACK }.into()),
                    ..container::Style::default()
                }),
        )
        .on_press(Message::CloseSettingsOverlay);

        backdrop.into()
    }

    /// The floating find bar (top-right): query field, `n/m` count, ‹ ›, ×.
    fn search_bar(&self, search: &Search) -> Element<'_, Message> {
        // Styled from the Obsidian ColorTheme so the bar recolors with the theme
        // (the overlay window itself stays on Theme::Dark).
        let theme = &self.theme;
        let ct = self.theme;
        let font = theme.ui_font();
        let (text_c, muted, accent, input_bg, border_c) =
            (theme.text, theme.text_muted, theme.accent, theme.background, theme.border);

        let count = if search.matches.is_empty() {
            if search.query.is_empty() {
                String::new()
            } else {
                "No results".to_owned()
            }
        } else {
            format!("{} / {}", search.current + 1, search.matches.len())
        };

        let step = move |label: &'static str, message: Message| {
            button(text(label).size(18).font(font))
                .padding([2, 10])
                .on_press_maybe((!search.matches.is_empty()).then_some(message))
                .style(move |_theme, status| chrome_button_style(status, muted, accent))
        };

        let input = text_input("Find on board…", &search.query)
            .id(SEARCH_INPUT.clone())
            .on_input(Message::SearchInput)
            .size(15)
            .font(font)
            .width(220)
            .style(move |_theme, status| iced::widget::text_input::Style {
                background: iced::Background::Color(input_bg),
                border: iced::Border {
                    radius: 6.0.into(),
                    width: 1.0,
                    color: if matches!(status, iced::widget::text_input::Status::Focused { .. }) {
                        accent
                    } else {
                        border_c
                    },
                },
                icon: muted,
                placeholder: muted,
                value: text_c,
                selection: iced::Color { a: 0.30, ..accent },
            });

        let bar = row![
            input,
            // Fixed width + centered, so "No results" vs "3 / 12" vs "" never
            // resizes the bar.
            text(count)
                .size(13)
                .font(font)
                .width(Length::Fixed(90.0))
                .align_x(iced::widget::text::Alignment::Center)
                .color(muted),
            step("‹", Message::SearchPrev),
            step("›", Message::SearchNext),
            button(text("×").size(16).font(font))
                .padding([2, 10])
                .on_press(Message::CloseSearch)
                .style(move |_theme, status| chrome_button_style(status, muted, accent)),
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center);

        let panel =
            container(bar).padding([8, 10]).style(move |_theme| chrome::panel(&ct, ct.elevated()));

        container(panel)
            .width(Fill)
            .height(Fill)
            .align_x(iced::alignment::Horizontal::Center)
            .align_y(iced::alignment::Vertical::Top)
            // Extra top inset so the centered bar clears the MacBook notch.
            .padding(Padding {
                top: 44.0,
                right: 16.0,
                bottom: 0.0,
                left: 16.0,
            })
            .into()
    }

    /// Width of the node context menu: the swatch row (default + presets +
    /// optional extra + wheel) plus side padding, so opening the color picker
    /// never resizes it. Shared by `view` and `context_menu_size`.
    fn node_menu_width(&self) -> f32 {
        const SW: f32 = 24.0;
        const GAP: f32 = 6.0;
        let has_extra = self
            .extra_swatch
            .as_deref()
            .is_some_and(|e| !NODE_PALETTE.contains(&e));
        let count = NODE_PALETTE.len() + has_extra as usize + 2;
        count as f32 * SW + (count - 1) as f32 * GAP + 24.0
    }

    /// Approximate on-screen size of the open context menu, used only to keep
    /// it from opening off a window edge. Heights are estimated from the row
    /// count (exactness isn't needed — it just bounds the clamp).
    fn context_menu_size(&self) -> Size {
        const ROW: f32 = 30.0; // a text button with [6,12] padding
        const SEP: f32 = 5.0; // 1px rule + the column's 4px spacing
        const PAD: f32 = 8.0; // the panel's 4px padding on both sides
        let Some(menu) = &self.context_menu else {
            return Size::ZERO;
        };
        let (width, height) = match menu.kind {
            ContextMenuKind::Canvas { .. } => (200.0, ROW * 5.0 + SEP * 4.0),
            ContextMenuKind::Node => {
                // Color row (24px swatch + [6,12] padding) + sep + "Ask AI" +
                // sep + delete, plus the layout row + sep for a multi-selection.
                let mut height = 36.0 + SEP + ROW + SEP + ROW;
                if self.selected.len() > 1 {
                    height += ROW + SEP;
                }
                (self.node_menu_width(), height)
            }
            ContextMenuKind::Link => (80.0, ROW),
        };
        Size::new(width + PAD, height + PAD)
    }

    /// Kicks off background rasterization of text bitmaps that are visible
    /// but missing or whose captured zoom is too far from the current one.
    /// Old bitmaps keep drawing until replacements arrive, so nodes never
    /// go blank; never runs during a zoom gesture. One batch in flight at
    /// a time.
    /// Drops a node's cached bitmap so it draws live until a fresh raster
    /// arrives. `refresh_snapshots` keeps a snapshot while the zoom stays in
    /// band — it can't see that the text or size changed — so any text change
    /// outside the edit-commit path (AI streaming, the pending "…" dots) must
    /// invalidate it explicitly, or the stale bitmap keeps showing.
    fn invalidate_snapshot(&mut self, index: usize) {
        self.text_snapshots.remove(&index);
        self.staging_snapshots.remove(&index);
    }

    fn refresh_snapshots(&mut self) -> Task<Message> {
        // Live text mode draws straight from Markdown; no bitmaps to prepare.
        if !self.config.rasterize_text {
            return Task::none();
        }
        if self.raster_pending {
            return Task::none();
        }

        let zoom = self.camera.zoom;
        let theme = self.theme;
        let center = self.config.center_node_text;
        let editing = self.editing.as_ref().map(|editing| editing.index);

        let jobs: Vec<(usize, String, f32, f32)> = self
            .nodes
            .iter()
            .enumerate()
            .filter(|(index, node)| {
                node.kind == "text"
                    && editing != Some(*index)
                    // Search matches draw live so their highlight shows.
                    && !self.is_search_match(*index)
                    && !self.staging_snapshots.contains_key(index)
                    && self.node_visible(node)
                    && !self.text_snapshots.get(index).is_some_and(|snapshot| {
                        (0.7..=2.0).contains(&(zoom / snapshot.zoom))
                            || (snapshot.scale
                                - text_raster::scale_for(zoom, node.width, node.height))
                            .abs()
                                < 0.001
                    })
            })
            .map(|(index, node)| {
                let text = node.text.as_deref().unwrap_or("(no content)").to_owned();
                (index, text, node.width, node.height)
            })
            .collect();

        if jobs.is_empty() {
            return Task::none();
        }

        self.raster_pending = true;
        Task::perform(
            async move {
                jobs.into_iter()
                    .filter_map(|(index, text, width, height)| {
                        let snapshot = text_raster::snapshot_markdown(
                            &text,
                            width,
                            height,
                            (width - 12.0).max(20.0),
                            FONT_SIZE,
                            theme,
                            center,
                            zoom,
                        )?;
                        Some((index, snapshot))
                    })
                    .collect()
            },
            Message::SnapshotsReady,
        )
    }

    /// The camera used for drawing: during the overlay entrance/exit the
    /// whole scene scales around the viewport center — the Dashboard
    /// full-scene zoom. `screen = center + offset + world*zoom`, so scaling
    /// both `offset` and `zoom` scales the scene about the center.
    fn display_camera(&self) -> Camera {
        let mut camera = self.camera;
        if self.overlay && self.reveal < 1.0 {
            let scale = 1.0 + SCENE_ZOOM * (1.0 - camera::ease_out_cubic(self.reveal));
            camera.zoom *= scale;
            camera.offset = camera.offset * scale;
        }
        camera
    }

    /// Whether the node is within (or near) the viewport.
    fn node_visible(&self, node: &CanvasNode) -> bool {
        let rect = self.camera.rect_to_screen(Rectangle::new(
            Point::new(node.x, node.y),
            Size::new(node.width, node.height),
        ));
        let margin = 0.25 * self.camera.viewport.width.max(self.camera.viewport.height);
        let viewport = Rectangle::new(
            Point::new(-margin, -margin),
            Size::new(
                self.camera.viewport.width + margin * 2.0,
                self.camera.viewport.height + margin * 2.0,
            ),
        );
        viewport.intersects(&rect)
    }

    fn content_bounds(&self) -> Rectangle {
        let mut min = Point::new(f32::MAX, f32::MAX);
        let mut max = Point::new(f32::MIN, f32::MIN);

        for node in &self.nodes {
            min.x = min.x.min(node.x);
            min.y = min.y.min(node.y);
            max.x = max.x.max(node.x + node.width);
            max.y = max.y.max(node.y + node.height);
        }

        if self.nodes.is_empty() {
            Rectangle::new(Point::ORIGIN, Size::new(1.0, 1.0))
        } else {
            Rectangle::new(min, Size::new(max.x - min.x, max.y - min.y))
        }
    }
}

/// Fits an image of `width`x`height` px to a node no larger than `max` on its
/// longest side, preserving aspect ratio and never upscaling (a small image
/// keeps its pixel size).
fn fit_image_size(width: usize, height: usize, max: f32) -> (f32, f32) {
    let (w, h) = (width as f32, height as f32);
    let longest = w.max(h);
    let scale = if longest > 0.0 {
        (max / longest).min(1.0)
    } else {
        1.0
    };
    (w * scale, h * scale)
}

/// The size for an aspect-locked resize: the smallest box with `aspect`
/// (width / height) that covers the `drag_w`×`drag_h` rectangle the cursor
/// swept and the `min_w`/`min_h` floors, without ever distorting `aspect`.
/// Keeps image nodes from stretching as a corner is dragged.
fn aspect_locked_size(aspect: f32, drag_w: f32, drag_h: f32, min_w: f32, min_h: f32) -> (f32, f32) {
    let aspect = aspect.max(0.01);
    let want_w = drag_w.max(min_w);
    let want_h = drag_h.max(min_h);
    // Widen to whichever axis needs more, then derive the other from `aspect`,
    // so the result covers both wants and both minimums with the aspect intact.
    let width = want_w.max(want_h * aspect);
    (width, width / aspect)
}

/// Parses the embedded scratch board. It's compiled in and validated by a
/// unit test, so a parse failure is a build-time bug, not a runtime condition.
fn scratch_document() -> CanvasDocument {
    serde_json::from_str(SCRATCH_CANVAS).expect("embedded scratch canvas is valid")
}

/// The vault root that a canvas's image `file` paths resolve against. Obsidian
/// records those paths relative to the vault root (which may be several levels
/// above the canvas), so:
///   1. walk up from the canvas to a `.obsidian` marker (authoritative);
///   2. else infer it — walk up to the ancestor where an image `file` actually
///      resolves (handles a vault copied without its `.obsidian` folder);
///   3. else fall back to the canvas's own directory.
fn vault_root(canvas: &FsPath, document: &CanvasDocument) -> PathBuf {
    let canvas_dir = canvas.parent().unwrap_or_else(|| FsPath::new("."));

    let mut dir = Some(canvas_dir);
    while let Some(current) = dir {
        if current.join(".obsidian").is_dir() {
            return current.to_path_buf();
        }
        dir = current.parent();
    }

    if let Some(file) = document.nodes.iter().find_map(|node| node.file.as_deref()) {
        let mut dir = Some(canvas_dir);
        while let Some(current) = dir {
            if current.join(file).exists() {
                return current.to_path_buf();
            }
            dir = current.parent();
        }
    }

    canvas_dir.to_path_buf()
}

/// Writes `document` to `path` only when there are unsaved changes — the app
/// calls this to flush a pending autosave before quitting, so edits made
/// inside the autosave debounce window aren't dropped on exit. Returns whether
/// it wrote.
fn save_if_dirty(dirty: bool, document: &CanvasDocument, path: &FsPath) -> bool {
    if !dirty {
        return false;
    }
    if let Err(error) = document.save(path) {
        eprintln!("save on quit failed: {error}");
    }
    true
}

/// The `file` value stored for an image written beside the canvas: its path
/// relative to `base` (the vault root, how Obsidian records file nodes),
/// falling back to the bare file name when the image lies outside the base.
fn vault_relative(path: &FsPath, base: &FsPath) -> String {
    path.strip_prefix(base)
        .map(|rel| rel.to_string_lossy().into_owned())
        .unwrap_or_else(|_| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default()
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::keyboard::Modifiers;
    use iced::keyboard::key::{Code, Key, Named, Physical};

    #[test]
    fn resize_min_height_respects_node_scrollbars() {
        // Scrollbars OFF: the floor is the fitted content (+ vertical padding),
        // never below 60 — so a resize can't shrink a node past its text (it
        // would clip). This is today's autoresize-friendly behavior.
        assert!(resize_min_height(false, 200.0) >= 200.0);
        assert_eq!(resize_min_height(false, 5.0), 60.0); // tiny content → 60 floor
        // Scrollbars ON: a small absolute floor well below the content, so the
        // node CAN be undersized (the overflow scrolls). This is the resolution
        // of the conflict with autoresize — the two behaviors must diverge here.
        assert_eq!(resize_min_height(true, 200.0), MIN_SCROLLABLE_NODE_HEIGHT);
        assert!(resize_min_height(true, 200.0) < resize_min_height(false, 200.0));
    }

    #[test]
    fn embedded_scratch_board_parses() {
        // A parse failure here means the checked-in welcome.canvas is broken —
        // caught at test time rather than panicking on launch.
        let doc = scratch_document();
        assert!(!doc.nodes.is_empty());
        assert!(!doc.edges.is_empty());
    }

    #[test]
    fn scratch_board_has_create_and_open_buttons() {
        let doc = scratch_document();
        let actions: Vec<&str> = doc
            .nodes
            .iter()
            .filter_map(|node| node.extra.get("tb_action").and_then(|v| v.as_str()))
            .collect();
        // Exactly the two onboarding buttons, and every button is a real node
        // the click handler can find by its `tb_action`.
        assert_eq!(actions, ["new_board", "open_board"]);
        for action in ["new_board", "open_board"] {
            let button = doc
                .nodes
                .iter()
                .find(|n| n.extra.get("tb_action").and_then(|v| v.as_str()) == Some(action))
                .unwrap_or_else(|| panic!("missing {action} button"));
            assert_eq!(button.kind, "text");
            assert!(button.text.as_deref().is_some_and(|t| !t.is_empty()));
        }
    }

    #[test]
    fn only_canvas_files_are_accepted_as_drops() {
        assert!(is_canvas_file(FsPath::new("/tmp/board.canvas")));
        // Case-insensitive (macOS/Windows filesystems).
        assert!(is_canvas_file(FsPath::new("/tmp/Board.CANVAS")));
        // Anything else is ignored.
        assert!(!is_canvas_file(FsPath::new("/tmp/photo.png")));
        assert!(!is_canvas_file(FsPath::new("/tmp/notes")));
    }

    #[test]
    fn hotkey_string_builds_parseable_combinations() {
        let k = Physical::Code(Code::KeyK);
        // A plain letter (uppercased); modifiers ordered Ctrl, Alt, Shift, Super.
        assert_eq!(
            hotkey_string(&Key::Character("k".into()), k, Modifiers::empty()),
            Some("K".to_owned())
        );
        // Modifier order follows macOS's ⌃⌥⇧⌘ (Super/Cmd last).
        assert_eq!(
            hotkey_string(&Key::Character("k".into()), k, Modifiers::LOGO | Modifiers::SHIFT),
            Some("Shift+Super+K".to_owned())
        );
        // Named keys use their code name.
        assert_eq!(
            hotkey_string(&Key::Named(Named::F12), k, Modifiers::ALT),
            Some("Alt+F12".to_owned())
        );

        // Every produced string must be accepted by the registrar's parser —
        // otherwise pressing the current hotkey can't re-bind it.
        for combo in [
            hotkey_string(&Key::Character("k".into()), k, Modifiers::LOGO | Modifiers::SHIFT),
            hotkey_string(&Key::Named(Named::F12), k, Modifiers::ALT),
            hotkey_string(&Key::Named(Named::Space), k, Modifiers::CTRL),
        ] {
            let combo = combo.expect("a complete combination");
            assert!(
                startup::parse_hotkey(&combo).is_some(),
                "parser rejected {combo:?}"
            );
        }
    }

    #[test]
    fn hotkey_string_records_the_physical_latin_key_off_qwerty() {
        // A Cyrillic 'к' pressed on the physical K position records as latin "K",
        // so the recorded hotkey matches the physical Code the global hotkey
        // fires on (and the registrar's parser accepts it).
        let combo = hotkey_string(
            &Key::Character("к".into()),
            Physical::Code(Code::KeyK),
            Modifiers::LOGO,
        );
        assert_eq!(combo, Some("Super+K".to_owned()));
        assert!(startup::parse_hotkey(&combo.unwrap()).is_some());
    }

    #[test]
    fn hotkey_string_waits_through_bare_modifiers() {
        let k = Physical::Code(Code::KeyK);
        // A held modifier alone (or an unidentified key) is not a combination.
        assert_eq!(hotkey_string(&Key::Named(Named::Shift), k, Modifiers::SHIFT), None);
        assert_eq!(hotkey_string(&Key::Named(Named::Super), k, Modifiers::LOGO), None);
        assert_eq!(hotkey_string(&Key::Unidentified, k, Modifiers::empty()), None);
    }

    #[test]
    fn find_matches_is_case_insensitive_and_non_overlapping() {
        fn text_node(text: &str) -> CanvasNode {
            serde_json::from_value(serde_json::json!({ "id": "n", "type": "text", "text": text }))
                .unwrap()
        }

        let nodes = vec![text_node("The Quick brown fox"), text_node("quick, quick!")];
        let hits = find_matches(&nodes, "quick");
        // One (case-insensitive) in node 0, two in node 1.
        assert_eq!(hits.len(), 3);
        assert_eq!((hits[0].node, hits[0].start, hits[0].len), (0, 4, 5));
        assert!(hits[1..].iter().all(|m| m.node == 1));

        // Empty query and a miss find nothing.
        assert!(find_matches(&nodes, "").is_empty());
        assert!(find_matches(&nodes, "zzz").is_empty());

        // Overlapping candidates are counted once (non-overlapping scan).
        assert_eq!(find_matches(&[text_node("aaaa")], "aa").len(), 2);
    }

    /// `<sel>` marks the selection range; returns the new text with the
    /// re-selected range marked the same way, or `"<none>"` for a no-op.
    fn toggle(marked: &str, marker: &str) -> String {
        let start = marked.find('<').unwrap();
        // `>` offset in `marked`, minus 1 for the removed `<` before it.
        let end = marked[start + 1..].find('>').unwrap() + start;
        let full = marked.replacen('<', "", 1).replacen('>', "", 1);
        match toggle_wrap(&full, start, end, marker) {
            Some((new_full, s, e)) => {
                let mut out = new_full;
                out.insert(e, '>');
                out.insert(s, '<');
                out
            }
            None => "<none>".to_owned(),
        }
    }

    #[test]
    fn bold_wraps_the_selection() {
        assert_eq!(toggle("<word>", "**"), "<**word**>");
    }

    #[test]
    fn bold_excludes_leading_and_trailing_whitespace() {
        // Selecting `word1< word2>` bolds `word2`, not ` word2`.
        assert_eq!(toggle("word1< word2>", "**"), "word1 <**word2**>");
        assert_eq!(toggle("<word1 >word2", "**"), "<**word1**> word2");
    }

    #[test]
    fn bold_toggles_off_when_markers_are_inside_the_selection() {
        assert_eq!(toggle("<**word**>", "**"), "<word>");
    }

    #[test]
    fn bold_toggles_off_when_markers_are_outside_the_selection() {
        // A double-clicked word selects the text but not the surrounding `**`.
        assert_eq!(toggle("**<word>**", "**"), "<word>");
    }

    #[test]
    fn italic_does_not_strip_bold_markers() {
        // `*` must not match one star of a `**` pair.
        assert_eq!(toggle("<**word**>", "*"), "<***word***>");
    }

    #[test]
    fn whitespace_only_selection_is_a_noop() {
        assert_eq!(toggle("a<   >b", "**"), "<none>");
    }

    #[test]
    fn fit_image_size_scales_down_preserving_aspect() {
        // 2:1 landscape past the limit halves cleanly.
        assert_eq!(fit_image_size(840, 420, 420.0), (420.0, 210.0));
    }

    #[test]
    fn fit_image_size_never_upscales_a_small_image() {
        assert_eq!(fit_image_size(100, 50, 420.0), (100.0, 50.0));
    }

    #[test]
    fn fit_image_size_fits_a_square_to_the_max() {
        assert_eq!(fit_image_size(800, 800, 400.0), (400.0, 400.0));
    }

    #[test]
    fn aspect_locked_size_holds_the_ratio_on_a_horizontal_drag() {
        // 2:1 image, dragged wide but barely tall: height follows the width.
        let (w, h) = aspect_locked_size(2.0, 400.0, 10.0, 80.0, 60.0);
        assert_eq!((w, h), (400.0, 200.0));
    }

    #[test]
    fn aspect_locked_size_holds_the_ratio_on_a_vertical_drag() {
        // Same 2:1 image dragged tall: width grows to keep the ratio.
        let (w, h) = aspect_locked_size(2.0, 10.0, 150.0, 80.0, 60.0);
        assert_eq!((w, h), (300.0, 150.0));
    }

    #[test]
    fn aspect_locked_size_covers_the_drag_rectangle() {
        // The box always contains the swept rectangle, never distorting.
        let (w, h) = aspect_locked_size(2.0, 300.0, 200.0, 80.0, 60.0);
        assert!(w >= 300.0 && h >= 200.0);
        assert!((w / h - 2.0).abs() < 1e-4);
    }

    #[test]
    fn aspect_locked_size_enforces_the_minimum_without_distorting() {
        // A tiny drag snaps up to the floors but keeps the 2:1 ratio.
        let (w, h) = aspect_locked_size(2.0, 5.0, 5.0, 80.0, 60.0);
        assert!(w >= 80.0 && h >= 60.0);
        assert!((w / h - 2.0).abs() < 1e-4);
    }

    #[test]
    fn vault_relative_is_relative_to_the_base() {
        assert_eq!(
            vault_relative(
                FsPath::new("/vault/Hobbies/pasted-1.png"),
                FsPath::new("/vault"),
            ),
            "Hobbies/pasted-1.png"
        );
    }

    #[test]
    fn vault_relative_falls_back_to_the_file_name_outside_the_base() {
        assert_eq!(
            vault_relative(
                FsPath::new("/elsewhere/pasted-2.png"),
                FsPath::new("/vault"),
            ),
            "pasted-2.png"
        );
    }

    #[test]
    fn vault_root_infers_the_base_from_image_paths_without_obsidian() {
        // A canvas in a subfolder whose image `file` is vault-relative
        // ("Hobbies/img.png"): the base is the parent that contains "Hobbies",
        // not the canvas's own dir (which would double the "Hobbies" segment).
        let dir = std::env::temp_dir().join(format!("tb-vault-{}", std::process::id()));
        let hobbies = dir.join("Hobbies");
        std::fs::create_dir_all(&hobbies).unwrap();
        std::fs::write(hobbies.join("img.png"), b"x").unwrap();

        let node = CanvasNode {
            id: "n".into(),
            kind: "file".into(),
            text: None,
            file: Some("Hobbies/img.png".into()),
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 100.0,
            color: None,
            extra: Default::default(),
        };
        let doc = CanvasDocument {
            nodes: vec![node],
            edges: vec![],
            extra: Default::default(),
        };

        assert_eq!(vault_root(&hobbies.join("board.canvas"), &doc), dir);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_if_dirty_flushes_only_when_dirty() {
        let dir = std::env::temp_dir().join(format!("tb-quit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("quit.canvas");

        let node = CanvasNode {
            id: "n".into(),
            kind: "text".into(),
            text: Some("hello".into()),
            file: None,
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 40.0,
            color: None,
            extra: Default::default(),
        };
        let doc = CanvasDocument {
            nodes: vec![node],
            edges: vec![],
            extra: Default::default(),
        };

        // Clean document: quitting writes nothing.
        assert!(!save_if_dirty(false, &doc, &path));
        assert!(!path.exists());

        // Unsaved edits: quitting flushes them, and they round-trip.
        assert!(save_if_dirty(true, &doc, &path));
        let reloaded = CanvasDocument::load(&path).unwrap();
        assert_eq!(reloaded.nodes.len(), 1);
        assert_eq!(reloaded.nodes[0].text.as_deref(), Some("hello"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn removed(ids: &[&str]) -> HashSet<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn deleting_the_streaming_note_cancels_its_stream() {
        // Stream "a" is currently writing into split sibling "a2"; deleting "a2"
        // cancels it. Stream "b" is untouched.
        let streams = [("a", "a2"), ("b", "b")];
        assert_eq!(
            ai_streams_to_cancel(streams.into_iter(), &removed(&["a2"])),
            vec!["a".to_owned()]
        );
    }

    #[test]
    fn deleting_the_anchor_note_cancels_its_stream() {
        let streams = [("a", "a3")];
        assert_eq!(
            ai_streams_to_cancel(streams.into_iter(), &removed(&["a"])),
            vec!["a".to_owned()]
        );
    }

    #[test]
    fn deleting_an_unrelated_note_cancels_nothing() {
        let streams = [("a", "a2"), ("b", "b")];
        assert!(ai_streams_to_cancel(streams.into_iter(), &removed(&["zzz"])).is_empty());
    }

    #[test]
    fn downscale_caps_the_longest_side_preserving_aspect() {
        let big = ::image::DynamicImage::new_rgb8(2000, 1000);
        let png = downscale_to_png(big).unwrap();
        let decoded = ::image::load_from_memory(&png).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (AI_IMAGE_MAX_PX, AI_IMAGE_MAX_PX / 2));
    }

    #[test]
    fn downscale_never_upscales_a_small_image() {
        let small = ::image::DynamicImage::new_rgb8(300, 200);
        let png = downscale_to_png(small).unwrap();
        let decoded = ::image::load_from_memory(&png).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (300, 200));
    }

    fn tnode(id: &str) -> CanvasNode {
        CanvasNode {
            id: id.to_owned(),
            kind: "text".to_owned(),
            text: Some("x".to_owned()),
            file: None,
            x: 0.0,
            y: 0.0,
            width: 1.0,
            height: 1.0,
            color: None,
            extra: Default::default(),
        }
    }

    fn tedge(id: &str, from: &str, to: &str) -> CanvasEdge {
        CanvasEdge {
            id: id.to_owned(),
            from_node: from.to_owned(),
            from_side: "right".to_owned(),
            to_node: to.to_owned(),
            to_side: "left".to_owned(),
            extra: Default::default(),
        }
    }

    #[test]
    fn snap_node_geometry_snaps_position_and_size() {
        // A node created off-grid: every creation site routes through this.
        let mut node = tnode("n");
        node.x = 23.0;
        node.y = -9.0;
        node.width = 247.0;
        node.height = 51.0;
        snap_node_geometry(&mut node, 20.0);
        // Position rounds to the nearest lattice point...
        assert_eq!((node.x, node.y), (20.0, 0.0)); // -9 -> -0
        // ...and size to the nearest whole cell (247->240, 51->60).
        assert_eq!((node.width, node.height), (240.0, 60.0));
    }

    #[test]
    fn snap_node_geometry_is_a_no_op_when_snapping_is_off() {
        let mut node = tnode("n");
        node.x = 23.0;
        node.y = -9.0;
        node.width = 247.0;
        node.height = 51.0;
        snap_node_geometry(&mut node, 0.0);
        assert_eq!((node.x, node.y, node.width, node.height), (23.0, -9.0, 247.0, 51.0));
    }

    #[test]
    fn committed_snapshot_drops_transient_ai_nodes_and_their_edges() {
        // "a" is a user node, "ai" is an in-flight "…" response branching off it.
        let nodes = vec![tnode("a"), tnode("b"), tnode("ai")];
        let edges = vec![tedge("e_ab", "a", "b"), tedge("e_ai", "a", "ai")];
        let transient: HashSet<String> = ["ai".to_owned()].into_iter().collect();

        let snap = committed_snapshot(&nodes, &edges, &transient);

        // The transient node and the edge feeding it are excluded from the
        // undo snapshot; unrelated nodes/edges stay.
        assert_eq!(
            snap.nodes.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(),
            ["a", "b"]
        );
        assert_eq!(
            snap.edges.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            ["e_ab"]
        );
    }

    #[test]
    fn committed_snapshot_keeps_the_whole_document_when_nothing_is_transient() {
        let nodes = vec![tnode("a"), tnode("b")];
        let edges = vec![tedge("e", "a", "b")];
        let snap = committed_snapshot(&nodes, &edges, &HashSet::new());
        assert_eq!(snap.nodes.len(), 2);
        assert_eq!(snap.edges.len(), 1);
    }

    #[test]
    fn committed_snapshot_drops_every_edge_touching_a_transient_node() {
        // Two concurrent in-flight generations off "a": ai1 and ai2, with an
        // edge between them (a split's chained notes). Every edge touching a
        // transient node — including the one between the two transients — is out.
        let nodes = vec![tnode("a"), tnode("ai1"), tnode("ai2"), tnode("b")];
        let edges = vec![
            tedge("e_a_ai1", "a", "ai1"),   // committed -> transient: dropped
            tedge("e_ai1_ai2", "ai1", "ai2"), // transient -> transient: dropped
            tedge("e_ai2_b", "ai2", "b"),   // transient -> committed: dropped
            tedge("e_ab", "a", "b"),        // committed -> committed: kept
        ];
        let transient: HashSet<String> = ["ai1".to_owned(), "ai2".to_owned()].into_iter().collect();

        let snap = committed_snapshot(&nodes, &edges, &transient);

        assert_eq!(
            snap.nodes.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(),
            ["a", "b"]
        );
        assert_eq!(
            snap.edges.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            ["e_ab"]
        );
    }

    /// End-to-end at the data layer: two Ask-AI generations off one node, each
    /// recording a pre-generation snapshot (as the fixed handler now does).
    /// Undo must peel them off one at a time — first gen 2, then gen 1 — and
    /// redo must rebuild them in order. This is the scenario the undo bugs hit.
    #[test]
    fn two_ai_generations_undo_and_redo_one_at_a_time() {
        let ids = |snap: &DocSnapshot| -> Vec<String> {
            snap.nodes.iter().map(|n| n.id.clone()).collect()
        };
        let no_transient = HashSet::new();
        let mut history: History<DocSnapshot> = History::new(100);

        // Start: just the source node "s".
        let mut nodes = vec![tnode("s")];
        let mut edges: Vec<CanvasEdge> = Vec::new();

        // Generation 1: record the pre-gen state, then add g1 (now committed).
        history.record(committed_snapshot(&nodes, &edges, &no_transient));
        nodes.push(tnode("g1"));
        edges.push(tedge("e_g1", "s", "g1"));

        // Generation 2: record the pre-gen state ({s, g1}), then add g2.
        history.record(committed_snapshot(&nodes, &edges, &no_transient));
        nodes.push(tnode("g2"));
        edges.push(tedge("e_g2", "s", "g2"));

        // Undo once → only generation 2 is peeled off.
        let live = committed_snapshot(&nodes, &edges, &no_transient);
        let after_undo1 = history.undo(live).unwrap();
        assert_eq!(ids(&after_undo1), ["s", "g1"]);
        assert_eq!(after_undo1.edges.iter().map(|e| e.id.clone()).collect::<Vec<_>>(), ["e_g1"]);

        // Undo again → generation 1 is peeled off, leaving just the source.
        let after_undo2 = history.undo(after_undo1).unwrap();
        assert_eq!(ids(&after_undo2), ["s"]);
        assert!(after_undo2.edges.is_empty());
        assert!(!history.can_undo());

        // Redo rebuilds gen 1, then gen 2, in order.
        let after_redo1 = history.redo(after_undo2).unwrap();
        assert_eq!(ids(&after_redo1), ["s", "g1"]);
        let after_redo2 = history.redo(after_redo1).unwrap();
        assert_eq!(ids(&after_redo2), ["s", "g1", "g2"]);
        assert!(!history.can_redo());
    }

    #[test]
    fn docsnapshot_is_an_independent_clone_of_the_live_document() {
        let nodes = vec![tnode("a")];
        let edges = vec![tedge("e", "a", "a")];
        let snap = committed_snapshot(&nodes, &edges, &HashSet::new());

        // Mutating the live document after snapshotting must not touch the snap.
        let mut nodes = nodes;
        nodes[0].text = Some("changed".to_owned());
        nodes.push(tnode("b"));
        assert_eq!(snap.nodes.len(), 1);
        assert_eq!(snap.nodes[0].text.as_deref(), Some("x"));
    }

    #[test]
    fn config_color_treats_zero_and_blank_as_no_tint() {
        assert_eq!(config_color("0"), None);
        assert_eq!(config_color(""), None);
        assert_eq!(config_color("3"), Some("3".to_owned()));
        assert_eq!(config_color("6"), Some("6".to_owned()));
    }

    #[test]
    fn nearest_free_offset_nudges_a_blocked_block_and_leaves_a_free_one() {
        let block = [Rectangle::new(Point::new(0.0, 0.0), Size::new(80.0, 40.0))];
        // Obstacle sitting exactly on the block → a non-zero nudge that clears.
        let obstacle = [Rectangle::new(Point::new(0.0, 0.0), Size::new(80.0, 40.0))];
        let (dx, dy) = nearest_free_offset(&block, &obstacle);
        assert!(dx != 0.0 || dy != 0.0);
        let moved = Rectangle::new(Point::new(dx, dy), Size::new(80.0, 40.0));
        assert!(!moved.intersects(&obstacle[0]));

        // Nothing in the way → no offset.
        assert_eq!(nearest_free_offset(&block, &[]), (0.0, 0.0));
    }

    #[test]
    fn text_bitmap_reuse_requires_same_text_and_size() {
        let text = Some("hello".to_owned());
        // Identical text + size → reusable.
        assert!(text_bitmap_reusable(&text, (100.0, 40.0), &text, (100.0, 40.0)));
        // Sub-pixel size drift is within slack → still reusable.
        assert!(text_bitmap_reusable(&text, (100.0, 40.0), &text, (100.3, 40.0)));
        // Different text → not reusable (would show stale content).
        let other = Some("world".to_owned());
        assert!(!text_bitmap_reusable(&text, (100.0, 40.0), &other, (100.0, 40.0)));
        // Different size → not reusable (wrapping/scale differ).
        assert!(!text_bitmap_reusable(&text, (100.0, 40.0), &text, (120.0, 40.0)));
    }
}

