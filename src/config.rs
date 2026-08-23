//! User configuration, loaded once at startup from a JSON file.
//!
//! Resolution order: `--config <path>`, then the `THINKBOARD_CONFIG`
//! environment variable, then the per-OS default —
//! `~/.config/thinkboard/config.json` (macOS/Linux) or
//! `%APPDATA%\thinkboard\config.json` (Windows). When that
//! default file is missing it is created, populated with defaults, so users
//! get an editable config without copying the example by hand. An explicit
//! path that is missing falls back to defaults silently (it isn't created);
//! a malformed file warns and falls back, so a bad config never prevents
//! the app from starting. Omitted keys use their defaults, and the default
//! file is upgraded in place when new keys are added by a later version
//! (the user's existing values are kept).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// The canvas to open on launch (the user's single working board). Loaded
    /// if it exists; otherwise the app prompts with a file dialog and remembers
    /// the choice here. A CLI path argument overrides it.
    pub canvas: String,
    /// The Obsidian vault to browse for boards (a folder), if one has been
    /// picked. Blank means no vault: the app still opens any canvas by dialog
    /// or drag-and-drop, it just has no list to switch between. When set it
    /// also *is* the base that a canvas's `file` paths resolve against, which
    /// beats inferring one from the canvas's own location.
    pub vault: String,
    /// Whether clicking empty space on the overlay dismisses the board.
    pub dismiss_on_background_click: bool,
    /// Whether the board stays up when you switch macOS Spaces. When false
    /// (default), switching Spaces auto-dismisses it (re-summon with the
    /// hotkey).
    pub persist_across_spaces: bool,
    /// Whether the overlay is transparent (desktop shows through). Default on,
    /// but a GPU/compositor that can't composite alpha (many VMs, software
    /// GLES) renders it black — set false there for an opaque themed overlay.
    pub overlay_transparent: bool,
    /// Run as an ordinary titled application window instead of the desktop
    /// overlay: no global summon hotkey, tray, or always-on-top click-through.
    /// The `--windowed` CLI flag forces this on regardless. Takes effect on the
    /// next launch.
    pub windowed: bool,
    /// macOS only: draw the menu-bar icon as a monochrome template image that
    /// tracks the menu bar's light/dark appearance, instead of the colored icon.
    /// No effect on other platforms (Windows/Linux tray icons stay colored).
    pub tray_icon_monochrome: bool,
    /// The global summon/dismiss hotkey, e.g. "Alt+F12", "Cmd+Shift+K",
    /// "Ctrl+Space", "PrintScreen". Modifiers: Cmd/Command/Super, Ctrl/Control,
    /// Alt/Option, Shift (and CmdOrCtrl). Defaults to the Pause key; a blank
    /// string disables the hotkey entirely.
    ///
    /// **Pause is a key most Macs don't have** — macOS reports it as F15, which
    /// exists on a full-size external keyboard and no laptop. It stays the
    /// default because it is the one chord nothing else claims, and on macOS
    /// [`Config::summon_gesture`] covers the gap; anyone on a laptop who wants
    /// a key rather than a swipe sets one here.
    pub summon_hotkey: String,
    /// macOS only: summon/dismiss the overlay with a four-finger downward
    /// trackpad swipe. Ignored on other platforms, where it defaults off.
    ///
    /// **On by default on macOS, and the summon the welcome board leads with.**
    /// The hotkey default is the Pause key, which macOS reports as F15 — a key
    /// no laptop has, so on a MacBook the gesture is the only summon that works
    /// out of the box. It taps a private framework, so it runs only while
    /// enabled and is a setting away from off.
    pub summon_gesture: bool,

    // --- Zoom & pan feel ---
    /// Minimum / maximum zoom the camera clamps to.
    pub min_zoom: f32,
    pub max_zoom: f32,
    /// Zoom smoothing time constant (seconds); smaller is snappier.
    pub zoom_smoothing: f32,
    /// Mouse-wheel zoom sensitivity (fraction of current zoom per wheel line).
    pub wheel_zoom_step: f32,
    /// Trackpad pinch zoom sensitivity.
    pub pinch_zoom_step: f32,
    /// Pan coasting retention per 120 Hz tick: 0 disables inertia, higher
    /// (toward 1) coasts longer.
    pub pan_inertia: f32,

    // --- Animation ---
    /// Multiplier on all animation durations: 1.0 is the tuned default, <1
    /// speeds them up, 0 makes them instant.
    pub animation_scale: f32,

    // --- Behavior ---
    /// Idle time after the last edit before autosaving (milliseconds).
    pub autosave_debounce_ms: u64,
    /// Zoom below which nearby notes collapse into semantic-zoom clusters.
    pub cluster_zoom: f32,
    /// Adding a node while zoomed out below `auto_zoom_trigger` flies the
    /// camera in to `auto_zoom_level`.
    pub auto_zoom_trigger: f32,
    pub auto_zoom_level: f32,

    // --- Appearance ---
    /// The theme to draw with, by name — one of the built-in schemes or a file
    /// in `<config dir>/themes`. Blank uses the app's own default.
    ///
    /// Takes precedence over [`Config::theme_css`], which is the older path: a
    /// stylesheet read directly, and what the vault's Obsidian theme still
    /// feeds. Naming a theme is how you opt out of that.
    pub theme: String,
    /// Absolute path to an Obsidian `theme.css` to read node colors from.
    /// Only plain color values are used; `var()`/`color-mix()`/`hsl()` keep
    /// the built-in defaults.
    pub theme_css: Option<String>,
    /// The body font family, by name — any family installed on the system, or
    /// one a theme embeds. Blank takes whatever the theme asks for, and the
    /// platform UI face if it asks for nothing.
    ///
    /// Deliberately *not* part of the theme: a theme is something you adopt
    /// whole, a font is a preference you keep across all of them. It overrides
    /// only the body family — the monospace face still comes from the theme,
    /// since code wants a font chosen for code.
    pub font: String,
    /// Overlay dim opacity over the desktop (0 = none, 1 = solid black).
    pub overlay_dim: f32,
    /// Whether node/cluster text may be centered. When true (default), plain or
    /// simply-emphasized text (only bold/italic) is centered, and anything with
    /// richer formatting (headings, code, links, lists) is left-aligned; when
    /// false, everything is left-aligned — the Obsidian way.
    pub center_node_text: bool,
    /// How node/cluster text is drawn. `true` pre-rasterizes it into bitmap
    /// snapshots (crisp, but large textures cost GPU bandwidth while panning);
    /// `false` draws it live as vector text every frame (always crisp, more
    /// per-frame shaping). Both render identical Markdown + highlighting.
    pub rasterize_text: bool,
    /// Whether a text node smaller than its content shows a draggable vertical
    /// scrollbar (and can be manually resized below its content). Off by default:
    /// nodes clamp to fit their text. On: manual resize can shrink a node past
    /// its text and the overflow scrolls (editing the node re-grows it to fit).
    pub node_scrollbars: bool,

    // --- Node & auto-layout sizing ---
    /// Bounds a freshly created / auto-sized text node grows within.
    pub new_node_min_width: f32,
    pub new_node_min_height: f32,
    pub max_node_width: f32,
    /// Gaps used by the "Auto-layout" command (world px).
    pub layout_rank_gap: f32,
    pub layout_node_gap: f32,

    /// When true, adding a node (port drag or AI) auto-lays-out its connected
    /// graph. Toggled from the canvas context menu; persisted here.
    pub auto_layout: bool,
    /// Auto-layout flow direction: "LR" (left-to-right) or "TD" (top-down).
    pub auto_layout_direction: String,

    /// When true, dragging/resizing a node snaps it to a world-space grid.
    /// Toggled from the canvas context menu; persisted here.
    pub snap_to_grid: bool,
    /// Grid spacing / snap distance in world px: a node's position rounds to the
    /// nearest multiple of this. Also the spacing of the revealed snap dots.
    pub snap_grid_size: f32,

    // --- AI ---
    /// OpenAI-compatible API base URL (e.g. LM Studio's local server). The
    /// `/chat/completions` path is appended. **Empty disables AI entirely** (the
    /// "Off" provider) — no requests are made and the AI actions are hidden.
    pub ai_url: String,
    /// Model name sent to the endpoint.
    pub ai_model: String,
    /// API keys sent as an `Authorization: Bearer` header, one per endpoint —
    /// switching providers keeps each one's key instead of overwriting a single
    /// shared field. Required for hosted providers (e.g. OpenRouter); left blank
    /// for a local endpoint like LM Studio. Stored in plaintext in the config.
    ///
    /// Keyed by endpoint rather than by provider name so a custom URL keeps its
    /// own key, and — the reason it matters — so editing the URL to point
    /// somewhere else never carries the previous host's key along with it.
    #[serde(default)]
    pub ai_api_keys: BTreeMap<String, String>,
    /// The single key earlier versions stored. Read once at load and folded
    /// into `ai_api_keys` under whatever endpoint was configured at the time,
    /// then dropped — see [`Config::migrate_api_key`].
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub ai_api_key: String,
    /// Hard cap on notes a single AI answer may split into (the model is asked
    /// to stay under it; extra content appends to the last note).
    pub max_ai_nodes: usize,
    /// Obsidian color code ("1"–"6") a node is tinted when it triggers an AI
    /// request; "0" leaves it uncolored.
    pub ai_trigger_color: String,
    /// Obsidian color code ("1"–"6") given to AI-generated response nodes; "0"
    /// leaves them uncolored.
    pub ai_node_color: String,
    /// Copilot-style suggestions: after adding a connected node, the AI offers
    /// a few "ghost" sibling nodes you can accept. Toggled from the canvas menu.
    pub ai_completions: bool,
    /// Whether to let a reasoning model "think" before answering. Off by
    /// default — for short node generation the reasoning tokens are wasteful.
    /// When off, requests set `chat_template_kwargs.enable_thinking = false`.
    pub ai_thinking: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            canvas: String::new(),
            vault: String::new(),
            dismiss_on_background_click: true,
            persist_across_spaces: false,
            overlay_transparent: true,
            windowed: false,
            tray_icon_monochrome: false,
            summon_hotkey: "Pause".to_owned(),
            // See the field: on a Mac the default hotkey is a key most Macs
            // haven't got, so the gesture is what makes the overlay reachable.
            summon_gesture: cfg!(target_os = "macos"),

            min_zoom: 0.1,
            max_zoom: 6.0,
            zoom_smoothing: 0.06,
            wheel_zoom_step: 0.15,
            pinch_zoom_step: 0.01,
            pan_inertia: 0.85,

            animation_scale: 1.0,

            autosave_debounce_ms: 400,
            cluster_zoom: 0.4,
            auto_zoom_trigger: 0.5,
            auto_zoom_level: 0.9,

            theme: String::new(),
            theme_css: None,
            font: String::new(),
            overlay_dim: 0.55,
            center_node_text: true,
            rasterize_text: true,
            node_scrollbars: false,

            new_node_min_width: 250.0,
            new_node_min_height: 60.0,
            max_node_width: 420.0,
            layout_rank_gap: 70.0,
            layout_node_gap: 40.0,

            auto_layout: false,
            auto_layout_direction: "LR".to_owned(),

            snap_to_grid: false,
            snap_grid_size: 20.0,

            ai_url: String::new(), // "Off" by default — no endpoint, private

            ai_model: "google/gemma-4-26b-a4b-qat".to_owned(),
            ai_api_keys: BTreeMap::new(),
            ai_api_key: String::new(),
            max_ai_nodes: 6,
            ai_trigger_color: "0".to_owned(), // uncolored
            ai_node_color: "6".to_owned(),    // purple
            ai_completions: false,
            ai_thinking: false,
        }
    }
}

/// The map key an endpoint's API key is filed under: the URL with surrounding
/// whitespace and any trailing slash removed, so `.../v1` and `.../v1/` are the
/// same endpoint rather than two entries with one key between them.
fn endpoint_key(url: &str) -> &str {
    url.trim().trim_end_matches('/')
}

impl Config {
    /// The API key for the configured endpoint, or empty when it has none.
    pub fn ai_key(&self) -> &str {
        self.ai_api_keys
            .get(endpoint_key(&self.ai_url))
            .map(String::as_str)
            .unwrap_or("")
    }

    /// Sets the API key for the configured endpoint. An empty key removes the
    /// entry rather than storing a blank one, so the config doesn't accumulate
    /// dead endpoints.
    pub fn set_ai_key(&mut self, key: &str) {
        let endpoint = endpoint_key(&self.ai_url).to_owned();
        if endpoint.is_empty() {
            return; // "Off" has no endpoint to attach a key to
        }
        if key.trim().is_empty() {
            self.ai_api_keys.remove(&endpoint);
        } else {
            self.ai_api_keys.insert(endpoint, key.trim().to_owned());
        }
    }

    /// Folds a pre-per-endpoint config's single `ai_api_key` into the map,
    /// under the endpoint it would have been used with. Idempotent: once
    /// carried over, the old field is cleared and stops being serialized.
    pub(crate) fn migrate_api_key(&mut self) {
        let legacy = std::mem::take(&mut self.ai_api_key);
        let legacy = legacy.trim();
        if legacy.is_empty() {
            return;
        }
        let endpoint = endpoint_key(&self.ai_url).to_owned();
        // Don't clobber a key already set for this endpoint.
        if !endpoint.is_empty() {
            self.ai_api_keys.entry(endpoint).or_insert_with(|| legacy.to_owned());
        }
    }

    /// Whether AI is on: an endpoint is configured. Empty `ai_url` is the "Off"
    /// provider — no AI requests are made and the AI actions stay hidden.
    pub fn ai_enabled(&self) -> bool {
        !self.ai_url.trim().is_empty()
    }

    /// A base animation duration (seconds) scaled by `animation_scale`, with a
    /// small floor so a zero scale reads as "instant" without dividing by zero.
    pub fn anim_secs(&self, base: f32) -> f32 {
        (base * self.animation_scale).max(0.001)
    }

    /// The autosave debounce as a `Duration`.
    pub fn autosave_debounce(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.autosave_debounce_ms)
    }

    /// Clamps loaded values to sane ranges (a bad file shouldn't wedge the
    /// camera or divide by zero). Returns whether anything was adjusted.
    pub(crate) fn sanitize(&mut self) -> bool {
        let before = self.clone();
        self.min_zoom = self.min_zoom.clamp(0.01, 100.0);
        self.max_zoom = self.max_zoom.max(self.min_zoom * 1.1);
        self.zoom_smoothing = self.zoom_smoothing.max(0.001);
        self.wheel_zoom_step = self.wheel_zoom_step.clamp(0.0, 2.0);
        self.pinch_zoom_step = self.pinch_zoom_step.clamp(0.0, 1.0);
        self.pan_inertia = self.pan_inertia.clamp(0.0, 0.999);
        self.animation_scale = self.animation_scale.clamp(0.0, 20.0);
        self.cluster_zoom = self.cluster_zoom.clamp(0.0, self.max_zoom);
        self.auto_zoom_trigger = self.auto_zoom_trigger.clamp(0.0, self.max_zoom);
        self.auto_zoom_level = self.auto_zoom_level.clamp(self.min_zoom, self.max_zoom);
        self.overlay_dim = self.overlay_dim.clamp(0.0, 1.0);
        self.new_node_min_width = self.new_node_min_width.max(20.0);
        self.new_node_min_height = self.new_node_min_height.max(20.0);
        self.max_node_width = self.max_node_width.max(self.new_node_min_width);
        self.layout_rank_gap = self.layout_rank_gap.max(0.0);
        self.layout_node_gap = self.layout_node_gap.max(0.0);
        self.snap_grid_size = self.snap_grid_size.clamp(2.0, 500.0);
        if self.auto_layout_direction != "LR" && self.auto_layout_direction != "TD" {
            self.auto_layout_direction = "LR".to_owned();
        }
        self.max_ai_nodes = self.max_ai_nodes.clamp(1, 50);
        // AI tint colors: an Obsidian preset "0"–"6" ("0" = none), else default.
        for (code, default) in [
            (&mut self.ai_trigger_color, "0"),
            (&mut self.ai_node_color, "6"),
        ] {
            if !matches!(code.as_str(), "0" | "1" | "2" | "3" | "4" | "5" | "6") {
                *code = default.to_owned();
            }
        }
        *self != before
    }
}

enum Location {
    /// A `--config` argument or `THINKBOARD_CONFIG`; used as-is, never created.
    Explicit(PathBuf),
    /// `~/.config/thinkboard/config.json`; created with defaults if missing.
    Default(PathBuf),
    /// No `HOME`; only defaults are available.
    None,
}

impl Config {
    /// Loads config from `explicit` (a `--config` argument) or the default
    /// location, creating the default file on first run.
    /// Points `theme_css` at whatever theme Obsidian is currently set to in the
    /// configured vault, returning whether that changed anything.
    ///
    /// This is the whole reason the vault setting helps here: the theme file
    /// lives at a path derived from `.obsidian/appearance.json`, so it can be
    /// found rather than hunted for, and re-found when it changes.
    ///
    /// **A hand-picked theme is never overwritten.** Only a `theme_css` that is
    /// empty, or that we resolved from this vault ourselves, is adoptable — so
    /// someone who deliberately pointed at a CSS file outside the vault keeps
    /// it, and someone who never chose one gets their vault's automatically.
    /// Clearing the theme in Obsidian clears ours too, by the same rule.
    pub fn adopt_vault_theme(&mut self) -> bool {
        let vault = self.vault.trim();
        if vault.is_empty() {
            return false;
        }
        let vault = Path::new(vault);
        let adoptable = match self.theme_css.as_deref() {
            None => true,
            Some(current) => thinkboard_core::vault::is_vault_theme(Path::new(current), vault),
        };
        if !adoptable {
            return false;
        }
        let resolved = thinkboard_core::vault::active_theme(vault)
            .map(|path| path.to_string_lossy().into_owned());
        if resolved == self.theme_css {
            return false;
        }
        self.theme_css = resolved;
        true
    }

    pub fn load(explicit: Option<&str>) -> Self {
        match resolve_location(explicit) {
            Location::Explicit(path) => match std::fs::read_to_string(&path) {
                Ok(text) => parse_or_default(&text, &path),
                Err(_) => Self::default(),
            },
            Location::Default(path) => Self::read_or_create(&path),
            Location::None => Self::default(),
        }
    }

    /// Reads the default-location config, creating it with defaults if
    /// missing, or upgrading it in place if it lacks keys this version added
    /// (the user's existing values are preserved).
    fn read_or_create(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let config = parse_or_default(&text, path);
                if missing_keys(&text, &config) {
                    config.write(path);
                }
                config
            }
            Err(_) => {
                let config = Self::default();
                config.write(path);
                config
            }
        }
    }

    /// Persists the config back to its resolved location (for UI toggles like
    /// auto-layout). No-op when there's no writable location.
    pub fn save(&self, explicit: Option<&str>) {
        let location = resolve_location(explicit);
        if !writable(&location) {
            return;
        }
        match location {
            Location::Explicit(path) | Location::Default(path) => self.write(&path),
            Location::None => {}
        }
    }

    /// Writes this config as pretty JSON, creating parent directories.
    /// Failures are reported but non-fatal.
    fn write(&self, path: &Path) {
        if let Some(parent) = path.parent() {
            if let Err(error) = std::fs::create_dir_all(parent) {
                eprintln!("config: could not create {} ({error})", parent.display());
                return;
            }
        }
        match serde_json::to_string_pretty(self) {
            Ok(json) => {
                if let Err(error) = std::fs::write(path, format!("{json}\n")) {
                    eprintln!("config: could not write {} ({error})", path.display());
                }
            }
            Err(error) => eprintln!("config: could not serialize defaults ({error})"),
        }
    }
}

fn parse_or_default(text: &str, path: &Path) -> Config {
    match serde_json::from_str::<Config>(text) {
        Ok(mut config) => {
            // Before sanitize, so folding an older config's single key into the
            // map isn't reported as a clamped value.
            config.migrate_api_key();
            if config.sanitize() {
                eprintln!("config: clamped out-of-range values in {}", path.display());
            }
            config
        }
        Err(error) => {
            eprintln!("config: ignoring {} ({error})", path.display());
            Config::default()
        }
    }
}

/// Whether the on-disk JSON object is missing any key the current config
/// has — i.e. an older config that a newer version should top up. Returns
/// false when the file can't be compared (so we never clobber it blindly).
fn missing_keys(text: &str, config: &Config) -> bool {
    let (Ok(Value::Object(on_disk)), Ok(Value::Object(full))) =
        (serde_json::from_str::<Value>(text), serde_json::to_value(config))
    else {
        return false;
    };
    full.keys().any(|key| !on_disk.contains_key(key))
}

/// Whether a resolved location may be written to.
///
/// **The default location is off-limits from a test.** `App::persist_config`
/// sits behind ordinary state transitions — opening a board, picking a vault,
/// adopting its theme, flipping a setting — so any test that drives one would
/// otherwise overwrite the developer's own `config.json` with whatever the test
/// had built, and `cargo test` would silently take their canvas, vault and API
/// keys with it. (It did. That is why this exists.)
///
/// A test that means to exercise saving passes an explicit path and still can.
fn writable(location: &Location) -> bool {
    !(cfg!(test) && matches!(location, Location::Default(_)))
}

/// The directory the config lives in, which is also where a user's own themes
/// go. `None` where there's no home to put them in.
pub fn config_dir() -> Option<PathBuf> {
    config_base().map(|base| base.join("thinkboard"))
}

fn resolve_location(explicit: Option<&str>) -> Location {
    if let Some(path) = explicit {
        return Location::Explicit(PathBuf::from(path));
    }
    if let Some(path) = std::env::var_os("THINKBOARD_CONFIG") {
        return Location::Explicit(PathBuf::from(path));
    }
    match config_base() {
        Some(base) => Location::Default(base.join("thinkboard").join("config.json")),
        None => Location::None,
    }
}

/// The per-OS base directory for application config: `%APPDATA%` on Windows,
/// `$HOME/.config` elsewhere (unchanged on macOS/Linux).
#[cfg(windows)]
fn config_base() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(PathBuf::from)
}
#[cfg(not(windows))]
fn config_base() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn tests_can_never_write_the_real_config() {
        use super::{Location, writable};
        use std::path::PathBuf;
        // The guard that stops `cargo test` from clobbering the developer's
        // own settings. Under test the default location must be refused …
        assert!(!writable(&Location::Default(PathBuf::from("/home/x/.config/thinkboard/config.json"))));
        // … while an explicit path — which is how a test opts *in* — still works.
        assert!(writable(&Location::Explicit(PathBuf::from("/tmp/throwaway.json"))));
        // `None` isn't this function's business — it means there is nowhere to
        // write at all, which `save` handles by doing nothing.
    }

    use super::*;

    #[test]
    fn missing_config_uses_defaults() {
        let config = Config::load(Some("/nonexistent/thinkboard.json"));
        assert!(config.dismiss_on_background_click);
    }

    #[test]
    fn ai_is_off_by_default_and_a_url_enables_it() {
        let mut config = Config::default();
        assert!(config.ai_url.is_empty());
        assert!(!config.ai_enabled()); // private by default: no endpoint
        config.ai_url = "http://localhost:1234/v1".to_owned();
        assert!(config.ai_enabled());
        // Whitespace-only stays off.
        config.ai_url = "   ".to_owned();
        assert!(!config.ai_enabled());
    }

    #[test]
    fn an_api_key_is_kept_per_endpoint() {
        let mut config = Config::default();
        config.ai_url = "https://api.openai.com/v1".to_owned();
        config.set_ai_key("sk-openai");
        config.ai_url = "https://api.fireworks.ai/inference/v1".to_owned();
        config.set_ai_key("fw-key");

        // Switching back finds the first key still there — the point of the
        // change: one field per provider, not one shared field.
        config.ai_url = "https://api.openai.com/v1".to_owned();
        assert_eq!(config.ai_key(), "sk-openai");
        config.ai_url = "https://api.fireworks.ai/inference/v1".to_owned();
        assert_eq!(config.ai_key(), "fw-key");
    }

    #[test]
    fn an_endpoint_with_no_key_reports_none_rather_than_anothers() {
        let mut config = Config::default();
        config.ai_url = "https://api.openai.com/v1".to_owned();
        config.set_ai_key("sk-openai");

        // A URL edited to point elsewhere must not carry the old host's key.
        config.ai_url = "https://internal.example.com/v1".to_owned();
        assert_eq!(config.ai_key(), "");
    }

    #[test]
    fn a_trailing_slash_is_the_same_endpoint() {
        let mut config = Config::default();
        config.ai_url = "https://api.openai.com/v1".to_owned();
        config.set_ai_key("sk-openai");
        config.ai_url = "https://api.openai.com/v1/".to_owned();
        assert_eq!(config.ai_key(), "sk-openai");
    }

    #[test]
    fn clearing_a_key_drops_its_entry() {
        let mut config = Config::default();
        config.ai_url = "https://api.openai.com/v1".to_owned();
        config.set_ai_key("sk-openai");
        config.set_ai_key("   ");
        assert_eq!(config.ai_key(), "");
        assert!(config.ai_api_keys.is_empty(), "no blank entry left behind");
    }

    #[test]
    fn an_older_configs_single_key_is_carried_over() {
        // What an existing install looks like on disk.
        let text = r#"{"ai_url": "https://api.openai.com/v1", "ai_api_key": "sk-legacy"}"#;
        let mut config: Config = serde_json::from_str(text).unwrap();
        config.migrate_api_key();

        assert_eq!(config.ai_key(), "sk-legacy", "filed under the endpoint in use");
        assert!(config.ai_api_key.is_empty(), "and the old field is cleared");
        // It doesn't come back or overwrite on a second pass.
        config.set_ai_key("sk-new");
        config.migrate_api_key();
        assert_eq!(config.ai_key(), "sk-new");
    }

    #[test]
    fn a_migrated_key_is_not_written_back_to_the_old_field() {
        let text = r#"{"ai_url": "https://api.openai.com/v1", "ai_api_key": "sk-legacy"}"#;
        let mut config: Config = serde_json::from_str(text).unwrap();
        config.migrate_api_key();
        let json = serde_json::to_string(&config).unwrap();
        assert!(!json.contains("sk-legacy\",\"ai_api_key"), "no duplicate");
        assert!(json.contains("ai_api_keys"));
        // The legacy field is skipped entirely once empty.
        assert!(!json.contains("\"ai_api_key\":"), "{json}");
    }

    #[test]
    fn migrating_a_key_is_not_reported_as_a_clamped_value() {
        // `sanitize` returning true prints a warning about out-of-range values.
        // Carrying an old key over is neither out of range nor worth a warning.
        let text = r#"{"ai_url": "https://api.openai.com/v1", "ai_api_key": "sk-legacy"}"#;
        let mut config: Config = serde_json::from_str(text).unwrap();
        config.migrate_api_key();
        assert!(!config.sanitize(), "nothing to clamp in a valid config");
    }

    #[test]
    fn sanitize_clamps_out_of_range_values() {
        let mut config = Config::default();
        config.min_zoom = 5.0;
        config.max_zoom = 1.0; // below min: must be pushed above it
        config.zoom_smoothing = 0.0; // would divide by zero
        config.pan_inertia = 2.0; // never stops
        config.animation_scale = -3.0; // negative

        assert!(config.sanitize());
        assert!(config.max_zoom > config.min_zoom);
        assert!(config.zoom_smoothing > 0.0);
        assert!(config.pan_inertia <= 0.999);
        assert!(config.animation_scale >= 0.0);
    }

    #[test]
    fn sanitize_leaves_valid_defaults_untouched() {
        let mut config = Config::default();
        assert!(!config.sanitize());
        assert_eq!(config, Config::default());
    }

    #[test]
    fn zero_animation_scale_stays_finite() {
        let mut config = Config::default();
        config.animation_scale = 0.0;
        config.sanitize();
        // No divide-by-zero: a zero scale still yields a tiny positive base.
        assert!(config.anim_secs(0.4) > 0.0);
    }

    #[test]
    fn partial_config_keeps_other_defaults() {
        let dir = std::env::temp_dir().join(format!("tb-config-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        std::fs::write(&path, r#"{"dismiss_on_background_click": false}"#).unwrap();

        let config = Config::load(Some(path.to_str().unwrap()));
        assert!(!config.dismiss_on_background_click);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn malformed_config_falls_back_to_defaults() {
        let dir = std::env::temp_dir().join(format!("tb-config-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        std::fs::write(&path, "{ not valid json").unwrap();

        let config = Config::load(Some(path.to_str().unwrap()));
        assert!(config.dismiss_on_background_click);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn creates_default_file_when_missing() {
        let dir = std::env::temp_dir().join(format!("tb-config-new-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        assert!(!path.exists());

        let config = Config::read_or_create(&path);
        assert!(config.dismiss_on_background_click);
        assert!(path.exists());
        // The written file re-parses.
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("dismiss_on_background_click"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn upgrades_config_missing_keys_but_keeps_values() {
        let dir = std::env::temp_dir().join(format!("tb-config-up-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        std::fs::write(&path, "{}").unwrap();

        let config = Config::read_or_create(&path);
        assert!(config.dismiss_on_background_click);
        // The missing key was written back to disk.
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("dismiss_on_background_click"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn complete_config_is_left_untouched() {
        let dir = std::env::temp_dir().join(format!("tb-config-keep-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        // A complete config (all keys present) must not be rewritten.
        let mut custom = Config::default();
        custom.dismiss_on_background_click = false;
        custom.persist_across_spaces = true;
        let original = format!("{}\n", serde_json::to_string_pretty(&custom).unwrap());
        std::fs::write(&path, &original).unwrap();

        let config = Config::read_or_create(&path);
        assert!(!config.dismiss_on_background_click);
        assert!(config.persist_across_spaces);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_trackpad_gesture_is_on_where_the_default_hotkey_is_unreachable() {
        // macOS reports the Pause key as F15, which no laptop has — so on a Mac
        // the gesture is the only summon that works out of the box. Elsewhere
        // the key exists and the gesture isn't ours to enable.
        let fresh = Config::default();
        assert_eq!(fresh.summon_gesture, cfg!(target_os = "macos"));
        assert_eq!(fresh.summon_hotkey, "Pause");
    }

}
