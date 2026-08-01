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
    pub summon_hotkey: String,
    /// macOS only (experimental): summon/dismiss the overlay with a four-finger
    /// downward trackpad swipe. Off by default; taps a private framework, so it
    /// only runs while enabled. Ignored on other platforms.
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
    /// Absolute path to an Obsidian `theme.css` to read node colors from.
    /// Only plain color values are used; `var()`/`color-mix()`/`hsl()` keep
    /// the built-in defaults.
    pub theme_css: Option<String>,
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
    /// API key sent as an `Authorization: Bearer` header. Required for hosted
    /// providers (e.g. OpenRouter); left blank for a local endpoint like LM
    /// Studio. Stored in plaintext in the config file.
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
            dismiss_on_background_click: true,
            persist_across_spaces: false,
            overlay_transparent: true,
            windowed: false,
            tray_icon_monochrome: false,
            summon_hotkey: "Pause".to_owned(),
            summon_gesture: false,

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

            theme_css: None,
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
            ai_api_key: String::new(),
            max_ai_nodes: 6,
            ai_trigger_color: "3".to_owned(), // yellow
            ai_node_color: "6".to_owned(),    // purple
            ai_completions: false,
            ai_thinking: false,
        }
    }
}

impl Config {
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
            (&mut self.ai_trigger_color, "3"),
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
        match resolve_location(explicit) {
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

fn resolve_location(explicit: Option<&str>) -> Location {
    if let Some(path) = explicit {
        return Location::Explicit(PathBuf::from(path));
    }
    if let Some(path) = std::env::var_os("THINKBOARD_CONFIG") {
        return Location::Explicit(PathBuf::from(path));
    }
    match config_base() {
        Some(base) => Location::Default(base.join("thinkboard/config.json")),
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
}
