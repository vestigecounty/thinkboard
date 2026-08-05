//! Application state and lifecycle.
//!
//! This module owns the spine: the [`App`] state, the [`Message`] vocabulary,
//! and the `update`/`subscription` loop that drives them. The work each message
//! does lives in a feature module beside this one, so the dispatch below stays
//! readable as a table of contents:
//!
//! - [`arrange`] — where nodes go and how big they are (auto-size, placement,
//!   the layered auto-layout)
//! - [`edit`] — the in-place editor and its text-level undo
//! - [`view`] — everything drawn: the board scene, menus, and floating chrome
//! - [`history`] — document undo/redo and the deletions that record it
//! - [`raster`] — text bitmaps and semantic-zoom clusters
//! - [`ai`] / [`ghosts`] — streamed answers and Copilot-style suggestions
//! - [`canvas_file`] — opening, autosaving and reloading the `.canvas` file
//! - [`search`] — the in-board find
//! - [`settings`] / [`startup`] — the settings form and the launch/hotkey
//!   plumbing, each of which also owns its `App` methods

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::LazyLock;
use std::time::{Duration, Instant, SystemTime};

use iced::widget::{image, text_editor};
use iced::window;
use iced::{Font, Point, Size, Subscription, Task, Vector};

use global_hotkey::GlobalHotKeyManager;
use global_hotkey::hotkey::HotKey;

use crate::ai as ai_client;
use crate::canvas::board;
use crate::canvas::camera::{self, Camera, Inertia};
use crate::canvas::text_raster;
use crate::config;
use crate::document::cluster;
use crate::document::layout;
use crate::document::model::{self, CanvasDocument, CanvasEdge, CanvasNode, NodeKind};
use crate::platform;
use crate::theme::Theme as ColorTheme;

mod ai;
mod arrange;
mod canvas_file;
mod chrome;
mod clipboard;
mod command;
mod edit;
mod ghosts;
mod history;
mod raster;
mod search;
mod settings;
mod startup;
mod sync;
#[cfg(any(target_os = "macos", windows, target_os = "linux"))]
mod tray;
#[cfg(test)]
mod update_tests;
mod view;

use ai::Ai;
use arrange::LayoutAnim;
use edit::Editing;
use ghosts::Ghosts;
use history::{DocSnapshot, MAX_UNDO};
use raster::{Clusters, Rasters};
use search::Search;
use thinkboard_core::history::History;

pub(crate) use startup::run;
use startup::{config_path_arg, hotkey_toggles, is_windowed};
use sync::{should_save, watch_file};

/// Re-exported for `crate::StagedSnapshot`, which the board draws from.
pub use raster::StagedSnapshot;

const FONT_SIZE: f32 = 17.0;

/// The find bar's text input, so opening the bar can focus it.
static SEARCH_INPUT: LazyLock<iced::advanced::widget::Id> =
    LazyLock::new(|| iced::advanced::widget::Id::new("board-search"));

/// Duration (seconds) of the hue picker's open (unroll) transition.
const PICKER_REVEAL: f32 = 0.18;

/// Duration (seconds) of the auto-layout ease-to-position animation.
const LAYOUT_ANIM: f32 = 0.4;

/// The system UI font (SF Pro on macOS, Segoe UI on Windows), matching what
/// the OS renders with. Its faces are loaded from disk at startup
/// ([`platform::UI_FONT_FILES`]); iced's own default resolves the generic
/// sans-serif family to a different font entirely.
pub const UI_FONT: Font = Font::with_name(platform::UI_FONT_NAME);

/// Velocity samples older than this are stale — releasing the button after
/// holding still should not coast.
const VELOCITY_FRESHNESS: Duration = Duration::from_millis(120);

/// Dashboard-style overlay entrance/exit animation lengths.
const ENTER_DURATION: f32 = 0.45;
const EXIT_DURATION: f32 = 0.35;

/// Zoom-in + fade of a freshly created node.
pub const SPAWN_DURATION: Duration = Duration::from_millis(280);

/// Autosave poll interval while there are pending changes (the debounce
/// itself is `config.autosave_debounce_ms`).
const SAVE_POLL: Duration = Duration::from_millis(250);

/// After summoning the overlay, ignore Space-change notifications for this long.
/// Activating/ordering the panel onto the current Space itself emits an
/// active-Space-change, which would otherwise trip the auto-dismiss and hide the
/// board we just brought up. A real user switch away happens well after this.
const SPACE_DISMISS_COOLDOWN: Duration = Duration::from_millis(1200);

/// Multiplier on a trackpad pinch's incremental magnification (1.0 ≈ the OS's
/// native feel); the per-event zoom factor is `1 + magnification * this`.
const PINCH_SPEED: f32 = 1.0;

/// Ease time constant of the auto-zoom fly-to (its trigger/target zoom are in
/// config).
const FOCUS_TAU: f32 = 0.12;

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
    Ai(String, ai_client::Event),
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

/// Smoothly approaches `target` zoom, keeping `cursor` anchored.
struct ZoomAnimation {
    target: f32,
    cursor: Point,
    last_tick: Instant,
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

struct App {
    nodes: Vec<CanvasNode>,
    edges: Vec<CanvasEdge>,
    images: HashMap<String, image::Handle>,
    /// Body text of the file nodes that embed a Markdown note, keyed by node
    /// id. Read from the vault at load; a note that can't be read is absent and
    /// the card shows just its title.
    notes: HashMap<String, String>,
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
    /// Semantic-zoom cluster summaries and their labels.
    clusters: Clusters,
    /// Text bitmaps for the nodes, and the ones warming up to replace them.
    rasters: Rasters,
    camera: Camera,
    /// Top safe-area inset (px), refreshed on resize; passed to the board so the
    /// top wayfinding button clears the macOS notch.
    top_safe_inset: f32,
    cache: iced::widget::canvas::Cache,
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
    /// Vertical scroll offset (world px from the top) per node id, for nodes
    /// resized below their content while `node_scrollbars` is on. Clamped to the
    /// content overflow on use; stale entries clamp to 0 harmlessly.
    node_scroll: HashMap<String, f32>,
    /// In-flight AI generations: their streams, pending notes, and shimmers.
    ai: Ai,
    /// Copilot-style suggestion cards awaiting accept/dismiss.
    ghosts: Ghosts,
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

/// Everything [`App::new`] resolves before the state itself can be built: the
/// loaded document and where it came from, the theme, and the process-level
/// handles only a real launch has. Splitting it out keeps the field
/// initializers in one place ([`App::build`]) that tests can also reach —
/// `App::new` itself reads the user's config file and registers an OS-level
/// hotkey, neither of which belongs in a test.
struct Init {
    config: config::Config,
    theme: ColorTheme,
    document: CanvasDocument,
    canvas_path: PathBuf,
    base_path: PathBuf,
    file_mtime: Option<SystemTime>,
    scratch: bool,
    hotkey_manager: Option<GlobalHotKeyManager>,
    registered_hotkey: Option<HotKey>,
    windowed: bool,
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
                let base_path = canvas_file::vault_root(&path, &document);
                let file_mtime = sync::file_mtime(&path);
                (document, path, base_path, file_mtime, false)
            }
            startup::Boot::Scratch => {
                let document = canvas_file::scratch_document();
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

        let app = Self::build(Init {
            config,
            theme,
            document,
            canvas_path,
            base_path,
            file_mtime,
            scratch,
            hotkey_manager,
            registered_hotkey,
            windowed,
        });
        (app, Task::none())
    }

    /// Assembles the initial state from an already-resolved [`Init`]: loads the
    /// document's images, computes the first clustering, and seeds every
    /// transient field. No I/O beyond reading the images the document names.
    fn build(init: Init) -> Self {
        let Init {
            config,
            theme,
            document,
            canvas_path,
            base_path,
            file_mtime,
            scratch,
            hotkey_manager,
            registered_hotkey,
            windowed,
        } = init;

        let images = document
            .nodes
            .iter()
            .filter(|node| node.node_kind() == NodeKind::Image)
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

        // Captured before `config` is moved into the struct below.
        let (min_zoom, max_zoom) = (config.min_zoom, config.max_zoom);

        let mut app = Self {
            nodes: document.nodes,
            edges: document.edges,
            images,
            notes: HashMap::new(),
            config,
            theme,
            canvas_path,
            scratch,
            base_path,
            doc_extra: document.extra,
            doc_dirty: false,
            last_edit: Instant::now(),
            file_mtime,
            clusters: Clusters::default(),
            rasters: Rasters::default(),
            camera: Camera {
                min_zoom,
                max_zoom,
                ..Camera::default()
            },
            cache: iced::widget::canvas::Cache::new(),
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
            node_scroll: HashMap::new(),
            ai: Ai::default(),
            ghosts: Ghosts::default(),
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
        app.notes = app.load_notes(&app.nodes);
        app.clusters = app.initial_clusters();
        app
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            // --- Camera ---
            Message::ViewportResized(size) => return self.viewport_resized(size),
            Message::Pan { delta, track } => {
                self.pan(delta, track);
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
            Message::CancelInertia => self.inertia = None,
            Message::FocusWorld(world) => self.focus_on_world(world),
            Message::ZoomBy { factor, cursor } => {
                self.zoom_by(factor, cursor);
                return self.exit_edit_if_offscreen();
            }
            Message::PinchZoom { magnification, x, y } => {
                // The gesture reports a bottom-left-origin window point; flip it
                // into iced's top-left canvas space to zoom around the pinch.
                let cursor = Point::new(x, (self.camera.viewport.height - y).max(0.0));
                self.zoom_by((1.0 + magnification * PINCH_SPEED).max(0.1), cursor);
                return self.exit_edit_if_offscreen();
            }

            // --- Direct manipulation ---
            Message::MoveNode { index, position } => self.move_node(index, position),
            Message::MoveCluster { index, delta } => self.move_cluster(index, delta),
            Message::ClusterDragEnd | Message::NodeDragEnd => return self.schedule_recluster(),
            Message::ResizeNode { index, anchor, cursor } => self.resize_node(index, anchor, cursor),
            Message::ResizeEnd => {
                return Task::batch([self.refresh_snapshots(), self.schedule_recluster()]);
            }
            Message::SetNodeScroll { id, offset } => {
                // Stored raw; clamped to the content overflow where it's read
                // (the board), so a later text/size change can't leave it stuck.
                self.node_scroll.insert(id, offset.max(0.0));
                self.cache.clear();
            }

            // --- Structure ---
            Message::AddEdge { from, side, to, to_side } => {
                return self.add_edge(from, side, to, to_side);
            }
            Message::CreateConnectedNode { from, side, world } => {
                self.create_connected_node(from, side, world);
            }
            Message::CreateNode { world } => {
                self.interacted = true;
                self.context_menu = None;
                self.dismiss_ghosts();
                let index = self.spawn_text_node(world);
                self.focus_on_node(index);
            }
            Message::DeleteSelected => {
                self.context_menu = None;
                return self.delete_selected();
            }
            Message::CopyNodes => self.copy_selected_nodes(),
            Message::Paste { world } => {
                self.interacted = true;
                self.context_menu = None;
                return self.paste_at(world);
            }
            Message::SetNodeColor(color) => self.set_node_color(color),
            Message::AutoLayout(dir) => {
                self.close_context_menu();
                if self.selected.len() > 1 {
                    let subset = self.selected.clone();
                    self.apply_layered_layout(&subset, dir, true);
                }
            }

            // --- Selection ---
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

            // --- Context menus ---
            Message::OpenContextMenu { screen, world } => {
                self.context_menu = Some(ContextMenu {
                    screen,
                    kind: ContextMenuKind::Canvas { world },
                });
            }
            Message::OpenNodeMenu { screen, node } => self.open_node_menu(screen, node),
            Message::OpenLinkMenu { screen, edge } => {
                // Right-clicking a link selects it (highlight on) and offers Delete.
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
                self.close_context_menu();
            }
            Message::ExpandColorPicker => {
                self.color_picker_open = true;
                self.picker_anim = Some(Instant::now());
            }

            // --- Settings toggles reachable from the canvas menu ---
            Message::ToggleAutoLayout => {
                // Leaves the menu open so the checkbox state visibly updates.
                self.config.auto_layout = !self.config.auto_layout;
                self.persist_config();
            }
            Message::ToggleSnap => {
                // Leaves the menu open so the checkbox state visibly updates.
                self.config.snap_to_grid = !self.config.snap_to_grid;
                self.persist_config();
                self.cache.clear();
            }
            Message::ToggleAiCompletions => {
                self.config.ai_completions = !self.config.ai_completions;
                self.persist_config();
                if !self.config.ai_completions {
                    self.dismiss_ghosts();
                }
            }

            // --- AI ---
            Message::AskAi => return self.ask_ai(),
            Message::Ai(stream_id, event) => return self.ai_event(stream_id, event),
            Message::AiTick => self.tick_ai_dots(),

            // --- Suggestion ghosts ---
            Message::Suggestions { generation, parent, anchor, texts } => {
                self.show_suggestions(generation, parent, anchor, texts);
            }
            Message::AcceptGhost(index) => return self.accept_ghost_at(index),
            Message::MoveGhost { index, position } => self.move_ghost(index, position),
            Message::DismissGhost(index) => self.dismiss_ghost(index),
            Message::HoverGhost(over) => self.hover_ghosts(over),
            Message::DismissGhosts => self.dismiss_ghosts(),

            // --- Editing ---
            Message::StartEditing { index, at } => {
                self.dismiss_ghosts();
                return self.begin_editing(index, at);
            }
            Message::PlaceEditorCaret => self.place_editor_caret(),
            Message::StopEditing => return self.stop_editing(),
            Message::EditorAction(action) => self.editor_action(action),
            Message::WrapSelection(marker) => self.wrap_selection(marker),
            Message::UndoEdit => self.apply_edit_history(false),
            Message::RedoEdit => self.apply_edit_history(true),
            Message::Undo => return self.undo(),
            Message::Redo => return self.redo(),

            // --- Background results and the frame clock ---
            Message::Tick(now) => return self.tick(now),
            Message::SnapshotsReady(snapshots) => return self.snapshots_ready(snapshots),
            Message::ClustersReady(clustering, snapshots) => {
                return self.clusters_ready(clustering, snapshots);
            }

            // --- The document on disk ---
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
            Message::FileChanged => return self.check_reload(),
            Message::CanvasAction(action) => return self.canvas_action(&action),
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

            // --- Find bar ---
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
            Message::SearchInput(query) => return self.search_input(query),
            Message::SearchNext | Message::SearchPrev => {
                let forward = matches!(message, Message::SearchNext);
                if let Some(search) = &mut self.search
                    && !search.matches.is_empty()
                {
                    let n = search.matches.len();
                    search.current = if forward {
                        (search.current + 1) % n
                    } else {
                        (search.current + n - 1) % n
                    };
                }
                self.cache.clear();
                return self.focus_current_match();
            }
            Message::CloseSearch => return self.close_search(),

            // --- Command palette ---
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
                let id = self
                    .command_palette
                    .as_ref()
                    .and_then(|p| command::filtered(&p.query).get(p.selected).map(|c| c.id));
                if let Some(id) = id
                    && id.is_enabled(self)
                {
                    return self.run_command(id);
                }
            }

            // --- Settings ---
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
            Message::SettingsBrowsed(field, path) => self.settings_browsed(field, path),
            Message::RecordHotkey => self.hotkey_recording = true,
            Message::HotkeyCaptured(key, physical, modifiers) => {
                self.capture_hotkey(key, physical, modifiers);
            }

            // --- Window lifecycle ---
            Message::OpenUrl(url) => platform::open_url(&url),
            Message::Noop => {}
            Message::Exit => {
                if !self.overlay {
                    return self.save_and_exit();
                }
                match self.phase {
                    Phase::Running | Phase::Entering { .. } => self.begin_exit(),
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
            Message::ToggleOverlay => return self.toggle_overlay(),
            Message::WindowShown => {
                // Re-raise and re-frame the overlay window (also re-adapts
                // to display changes while hidden).
                platform::configure_overlay_window();
                self.overlay_shown_at = Some(Instant::now());
            }
            Message::SpaceChanged => return self.space_changed(),
            Message::WindowCloseRequested(id) => {
                if self.settings_window == Some(id) {
                    self.close_settings_window(true);
                    return window::close(id);
                }
                // The main window's close button: quit (flushing the autosave).
                return self.save_and_exit();
            }
            Message::WindowClosed(id) => {
                if self.settings_window == Some(id) {
                    self.close_settings_window(false);
                }
            }
        }

        Task::none()
    }

    // --- Camera and viewport ---

    fn viewport_resized(&mut self, size: Size) -> Task<Message> {
        self.camera.viewport = size;
        self.top_safe_inset = platform::top_safe_inset();
        if self.overlay && !self.overlay_configured {
            self.overlay_configured = true;
            platform::configure_overlay_window();
            // Create the tray icon now that the app is running (macOS needs the
            // run loop live). It's parked in a main-thread slot for the process,
            // so later toggles (e.g. the monochrome icon) can retint it.
            #[cfg(any(target_os = "macos", windows))]
            tray::create(self.config.tray_icon_monochrome);
            // Restart the entrance clock now that the window is actually on
            // screen.
            self.phase = Phase::Entering {
                started: Instant::now(),
            };
            // Start listening for the summon gesture if opted in (the tap must
            // be armed on the main thread, which we're on).
            if platform::SUPPORTS_GESTURES {
                platform::set_summon_gesture_enabled(self.config.summon_gesture);
            }
        }
        // The overlay resizes itself to the screen right after opening; keep
        // refitting until the user takes over.
        if !self.interacted {
            self.camera.fit(self.content_bounds());
        }
        self.cache.clear();
        self.refresh_snapshots()
    }

    fn pan(&mut self, delta: Vector, track: bool) {
        self.interacted = true;
        self.inertia = None;
        self.camera_focus = None;
        self.camera.offset = self.camera.offset + delta;
        if track {
            let now = Instant::now();
            if let Some(last) = self.last_pan {
                let dt = now.duration_since(last).as_secs_f32().max(0.001);
                // Blend with previous samples so one slow event just before
                // release doesn't kill the glide.
                let sample = delta * (1.0 / dt);
                self.pan_velocity = self.pan_velocity * 0.5 + sample * 0.5;
            }
            self.last_pan = Some(now);
        }
        self.cache.clear();
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

    // --- Direct manipulation ---

    fn move_node(&mut self, index: usize, position: Point) {
        self.interacted = true;
        if let Some(node) = self.nodes.get(index) {
            // Dragging a node in a multi-selection moves the whole group by the
            // same delta; otherwise just the one node.
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

    fn move_cluster(&mut self, index: usize, delta: Vector) {
        self.interacted = true;
        let Some(cluster) = self.clusters.list.get(index) else {
            return;
        };
        // Move every member node and the card itself; the drag ends with a
        // recluster to resettle bounds.
        for &m in &cluster.members.clone() {
            if let Some(node) = self.nodes.get_mut(m) {
                node.x += delta.x;
                node.y += delta.y;
            }
        }
        if let Some(cluster) = self.clusters.list.get_mut(index) {
            cluster.bounds.x += delta.x;
            cluster.bounds.y += delta.y;
        }
        self.mark_dirty();
        self.cache.clear();
    }

    fn resize_node(&mut self, index: usize, anchor: Point, cursor: Point) {
        self.interacted = true;
        let Some(node) = self.nodes.get(index) else {
            return;
        };
        let (drag_w, drag_h) = ((cursor.x - anchor.x).abs(), (cursor.y - anchor.y).abs());

        let (width, height) = if node.is_text() {
            self.resized_text_size(node, drag_w, drag_h)
        } else {
            // Image nodes keep their aspect ratio so the picture never
            // stretches. They're created aspect-correct (fit_image_size), so
            // preserving the current ratio holds it true.
            let aspect = node.width / node.height;
            arrange::aspect_locked_size(aspect, drag_w, drag_h, 80.0, 60.0)
        };

        let node = &mut self.nodes[index];
        node.x = if cursor.x >= anchor.x { anchor.x } else { anchor.x - width };
        node.y = if cursor.y >= anchor.y { anchor.y } else { anchor.y - height };
        node.width = width;
        node.height = height;

        // The stale bitmap would stretch; draw live text until the resize ends
        // and a fresh raster arrives.
        self.rasters.invalidate(index);
        self.mark_dirty();
        self.cache.clear();
    }

    // --- Structure ---

    fn add_edge(
        &mut self,
        from: usize,
        side: &'static str,
        to: usize,
        to_side: &'static str,
    ) -> Task<Message> {
        self.interacted = true;
        let endpoints = self
            .nodes
            .get(from)
            .map(|s| s.id.clone())
            .zip(self.nodes.get(to).map(|t| t.id.clone()));
        let Some((from_id, to_id)) = endpoints else {
            return Task::none();
        };
        self.push_undo();
        self.edges.push(CanvasEdge {
            id: model::random_id(),
            from_node: from_id,
            from_side: side.to_owned(),
            to_node: to_id,
            to_side: to_side.to_owned(),
            extra: Default::default(),
        });
        // Connecting two nodes merges their graphs; re-lay-out if enabled
        // (covered by the undo snapshot just recorded).
        self.auto_relayout(to, false);
        self.cache.clear();
        self.mark_dirty();
        self.schedule_recluster()
    }

    fn create_connected_node(&mut self, from: usize, side: &'static str, world: Point) {
        self.interacted = true;
        self.dismiss_ghosts();
        let Some(source) = self.nodes.get(from) else {
            return;
        };
        let source_id = source.id.clone();

        // Place the node so its connecting side sits at the release point
        // (matching the drag preview) and the body extends away from the source
        // — right of a left link, under a top link, etc.
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

    /// Applies a color to every selected node (undoable); `None` clears it back
    /// to default. Closes the menu so the pick reads as confirmed — one undo
    /// entry.
    fn set_node_color(&mut self, color: Option<String>) {
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
        if let Some(c) = color.filter(|c| !view::NODE_PALETTE.contains(&c.as_str())) {
            self.last_picked_color = Some(c);
        }
        self.close_context_menu();
        self.selected.clear();
    }

    fn open_node_menu(&mut self, screen: Point, node: usize) {
        // Right-clicking a node selects it (highlight on) and offers colors +
        // Delete.
        self.inertia = None;
        // Keep an existing multi-selection when right-clicking one of its
        // members; otherwise select just this node.
        if !self.selected.contains(&node) {
            self.selected = vec![node];
        }
        self.selected_edge = None;
        self.color_picker_open = false;
        self.picker_anim = None;
        // Extra swatch: a color on the clipboard, else the last custom color
        // picked. Text-only clipboard read (cheap).
        self.extra_swatch = clipboard::read_text()
            .and_then(|text| model::parse_css_color(text.trim()).map(model::color_to_hex))
            .or_else(|| self.last_picked_color.clone());
        self.context_menu = Some(ContextMenu {
            screen,
            kind: ContextMenuKind::Node,
        });
        self.cache.clear();
    }

    /// Closes any open context menu and collapses its hue picker.
    fn close_context_menu(&mut self) {
        self.context_menu = None;
        self.color_picker_open = false;
        self.picker_anim = None;
        self.cache.clear();
    }

    // --- Find bar ---

    fn search_input(&mut self, query: String) -> Task<Message> {
        // Nodes that were or are now matched must re-render: newly matched ones
        // draw live (highlighted), newly unmatched ones re-rasterize.
        let mut affected: std::collections::HashSet<usize> = self
            .search
            .as_ref()
            .map(|s| s.matches.iter().map(|m| m.node).collect())
            .unwrap_or_default();
        let matches = search::find_matches(&self.nodes, &query);
        affected.extend(matches.iter().map(|m| m.node));
        for index in affected {
            self.invalidate_snapshot(index);
        }
        self.search = Some(Search { query, matches, current: 0 });
        self.cache.clear();
        let focus = self.focus_current_match();
        Task::batch([self.refresh_snapshots(), focus])
    }

    fn close_search(&mut self) -> Task<Message> {
        if let Some(search) = self.search.take() {
            for index in search
                .matches
                .iter()
                .map(|m| m.node)
                .collect::<std::collections::HashSet<_>>()
            {
                self.invalidate_snapshot(index);
            }
        }
        self.cache.clear();
        self.refresh_snapshots()
    }

    // --- Boards ---

    fn canvas_action(&mut self, action: &str) -> Task<Message> {
        let create = match action {
            "new_board" => true,
            "open_board" => false,
            _ => return Task::none(),
        };
        // The overlay is always-on-top and would cover the native file dialog,
        // so dismiss it first and open the dialog once it's hidden. In windowed
        // mode there's nothing to hide.
        if self.overlay {
            self.pending_board_create = Some(create);
            return self.hide_window().chain(Task::done(Message::OpenBoardDialog));
        }
        self.choose_board(create)
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

    // --- Overlay lifecycle ---

    fn begin_exit(&mut self) {
        self.editing = None;
        self.phase = Phase::Exiting {
            started: Instant::now(),
        };
        self.cache.clear();
    }

    fn toggle_overlay(&mut self) -> Task<Message> {
        match self.phase {
            Phase::Hidden => {
                self.phase = Phase::Entering {
                    started: Instant::now(),
                };
                self.reveal = 0.0;
                self.cache.clear();
                // Linux reopens a fresh window (it was closed to hide); its
                // ViewportResized drives the rest. Elsewhere the resident main
                // window is simply un-hidden.
                #[cfg(target_os = "linux")]
                {
                    let (id, open) = window::open(startup::window_settings(&self.config, !self.overlay));
                    self.main_window = Some(id);
                    open.discard().chain(Task::done(Message::WindowShown))
                }
                #[cfg(not(target_os = "linux"))]
                match self.main_window {
                    Some(id) => window::set_mode(id, window::Mode::Windowed)
                        .chain(Task::done(Message::WindowShown)),
                    None => Task::none(),
                }
            }
            Phase::Running | Phase::Entering { .. } => {
                self.begin_exit();
                Task::none()
            }
            Phase::Exiting { .. } => Task::none(),
        }
    }

    /// The active macOS Space changed. Auto-dismiss (the subscription only fires
    /// this when persist is off). Hide instantly — the exit animation would play
    /// on a Space the user is no longer looking at.
    ///
    /// But ordering/activating the panel onto the current Space at summon
    /// *itself* emits an active-Space-change; acting on that would hide the
    /// board we just brought up (and, on a non-launch desktop, it fires
    /// repeatedly). So ignore Space-changes for a short cooldown after a summon
    /// — a genuine switch away lands well after it.
    fn space_changed(&mut self) -> Task<Message> {
        let summon_churn = self
            .overlay_shown_at
            .is_some_and(|t| t.elapsed() < SPACE_DISMISS_COOLDOWN);
        let dismiss = self.overlay
            && !summon_churn
            && matches!(self.phase, Phase::Running | Phase::Entering { .. });
        if dismiss {
            return self.hide_window();
        }
        Task::none()
    }

    /// Hides the overlay window, keeping the app resident for the next summon.
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

    fn mark_dirty(&mut self) {
        self.doc_dirty = true;
        self.last_edit = Instant::now();
    }

    // --- The frame clock ---

    /// One animation frame: advances every in-flight transition and returns any
    /// follow-up work they finished into.
    fn tick(&mut self, now: Instant) -> Task<Message> {
        let mut follow_up = Task::none();

        // Expire ignored ghost suggestions once fully faded.
        if self.ghosts_expired() {
            self.dismiss_ghosts();
        }

        // Stop driving frames once the hue picker has finished opening; `view`
        // reads the elapsed time to draw the unroll.
        if let Some(start) = self.picker_anim
            && now.duration_since(start).as_secs_f32() >= self.config.anim_secs(PICKER_REVEAL)
        {
            self.picker_anim = None;
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

        // Finished spawn animations; open the editor of the node that was just
        // dragged into existence.
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

        // Warm each pending texture invisibly, then ramp its cross-fade opacity;
        // once fully faded in, promote it to the active bitmap.
        if self.tick_snapshots(now) {
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
            let dt = now.duration_since(animation.last_tick).as_secs_f32().min(0.1);
            animation.last_tick = now;

            let progress = 1.0 - (-dt / self.config.zoom_smoothing).exp();
            let zoom = self.camera.zoom + (animation.target - self.camera.zoom) * progress;

            if (animation.target - zoom).abs() < 0.001 {
                self.camera.zoom_around(animation.cursor, animation.target);
                self.zoom_animation = None;
                // Settled: re-raster any bitmaps that drifted out of their
                // quality band.
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
            // Keep resetting the save debounce so only the final positions are
            // written.
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
        follow_up
    }

    /// Whether a continuous redraw loop is running (`window::frames()`): camera
    /// motion or an in-flight animation. Frame-pacing stats are only meaningful
    /// here — outside it, redraws are sparse and event-driven (hover), so their
    /// inter-frame gaps are idle time, not render cost.
    fn animating(&self) -> bool {
        self.inertia.is_some()
            || self.zoom_animation.is_some()
            || self.camera_focus.is_some()
            || self.rasters.staging_any()
            || !self.spawning.is_empty()
            || self.picker_anim.is_some()
            || self.layout_anim.is_some()
            || !self.ai.flows.is_empty()
            || self.ghosts_expiring()
            || self.ghosts_fading_in()
            || matches!(self.phase, Phase::Entering { .. } | Phase::Exiting { .. })
    }

    fn subscription(&self) -> Subscription<Message> {
        let mut subscriptions = Vec::new();
        if self.animating() {
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
            subscriptions.push(iced::event::listen_with(startup::capture_hotkey_event));
        }
        // While the find bar is open, Enter/Shift+Enter/Esc drive it.
        if self.search.is_some() {
            subscriptions.push(iced::event::listen_with(search::key_event));
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
        subscriptions.push(iced::event::listen_with(canvas_file::drop_event));
        if self.overlay {
            subscriptions.push(Subscription::run(hotkey_toggles));
            // Tray menu clicks (Show/Hide, Quit) where a tray exists.
            #[cfg(any(target_os = "macos", windows, target_os = "linux"))]
            subscriptions.push(Subscription::run(tray::events));
        }
        // Auto-dismiss when the active workspace/Space changes, unless
        // configured to persist. Only where the platform reports it
        // (event-driven, no polling).
        if platform::WATCHES_WORKSPACE && self.overlay && !self.config.persist_across_spaces {
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
        if !self.ai.pending.is_empty() {
            subscriptions.push(iced::time::every(ai::DOTS_INTERVAL).map(|_| Message::AiTick));
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
}
