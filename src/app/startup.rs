//! Application bootstrap: process setup, argument parsing, the iced run
//! loop, and theme/hotkey wiring.

use std::path::{Path as FsPath, PathBuf};

use iced::window;
use iced::{Point, Size, Theme};

use global_hotkey::hotkey::{Code, HotKey};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};

use super::{App, Message, UI_FONT};

/// What the app opens on launch: a real `.canvas` file (autosaving), or the
/// embedded scratch board (a throwaway playground that's never written to disk).
#[derive(Clone)]
pub(crate) enum Boot {
    Canvas(PathBuf),
    Scratch,
}

/// The default summon/dismiss hotkey (also the fallback for an unparseable
/// config value). The Pause/Break key, which macOS delivers as F15.
const DEFAULT_HOTKEY: &str = "Pause";

/// Maps a parsed key to the code the OS actually delivers. macOS reports a PC
/// keyboard's PrintScreen / ScrollLock / Pause keys as F13 / F14 / F15.
#[cfg(target_os = "macos")]
fn platform_key(key: Code) -> Code {
    match key {
        Code::PrintScreen => Code::F13,
        Code::ScrollLock => Code::F14,
        Code::Pause => Code::F15,
        other => other,
    }
}
#[cfg(not(target_os = "macos"))]
fn platform_key(key: Code) -> Code {
    key
}

/// Printed for `--help`.
const USAGE: &str = "\
ThinkBoard — an Obsidian .canvas viewer/editor

Usage:
  thinkboard [file.canvas] [--windowed] [--config <path>]

Arguments:
  [file.canvas]      Canvas to open. If omitted, opens the configured canvas,
                     else the embedded scratch board.

Options:
  --windowed         Run as an ordinary window (no desktop overlay)
  --config <path>    Use a specific config file
  --help, -h         Show this help";

/// Parses a hotkey spec (e.g. `Alt+F12`, `Cmd+Shift+K`, `Ctrl+Space`) into a
/// global hotkey — tolerant of whitespace around `+` and of case — and applies
/// the platform key mapping. `None` if it doesn't parse.
pub(super) fn parse_hotkey(spec: &str) -> Option<HotKey> {
    let compact: String = spec.chars().filter(|c| !c.is_whitespace()).collect();
    let parsed = compact.parse::<HotKey>().ok()?;
    Some(HotKey::new(Some(parsed.mods), platform_key(parsed.key)))
}

/// The summon hotkey for a config value: a blank string disables it (returns
/// `None` — no hotkey registered); an unparseable one logs and falls back to
/// [`DEFAULT_HOTKEY`].
pub(super) fn summon_hotkey(spec: &str) -> Option<HotKey> {
    if spec.trim().is_empty() {
        return None;
    }
    parse_hotkey(spec).or_else(|| {
        eprintln!("summon_hotkey: could not parse {spec:?}; using {DEFAULT_HOTKEY}");
        parse_hotkey(DEFAULT_HOTKEY)
    })
}

/// Creates the global-hotkey manager and registers the summon shortcut. The app
/// keeps the manager so the shortcut can be re-registered when it changes in
/// settings. Returns `(None, _)` in windowed mode or where a manager can't be
/// created (e.g. Wayland has no global-hotkey support).
pub(super) fn register_summon_hotkey(
    spec: &str,
    windowed: bool,
) -> (Option<GlobalHotKeyManager>, Option<HotKey>) {
    if windowed {
        return (None, None);
    }
    let manager = match GlobalHotKeyManager::new() {
        Ok(manager) => manager,
        Err(error) => {
            eprintln!("global hotkeys unavailable: {error}");
            return (None, None);
        }
    };
    let Some(hotkey) = summon_hotkey(spec) else {
        return (Some(manager), None); // blank config: no hotkey, keep the manager
    };
    if let Err(error) = manager.register(hotkey) {
        eprintln!("could not register the summon hotkey: {error}");
        return (Some(manager), None);
    }
    (Some(manager), Some(hotkey))
}

/// The canvas to open without prompting: a CLI path argument (a dev override)
/// wins; otherwise the configured canvas if it exists on disk. `None` means the
/// caller should fall back to the embedded scratch board.
fn preselected_canvas(arg: Option<String>, configured: &str) -> Option<PathBuf> {
    if let Some(arg) = arg {
        return Some(PathBuf::from(arg));
    }
    if !configured.is_empty() && FsPath::new(configured).exists() {
        return Some(PathBuf::from(configured));
    }
    None
}

pub(crate) fn run() -> iced::Result {
    // Windows: select the DirectComposition swapchain so the transparent
    // overlay composites instead of rendering black. Our patched iced_wgpu
    // reads this via wgpu's BackendOptions::from_env_or_default.
    // SAFETY: run() is the first thing main() does — no other threads yet.
    #[cfg(windows)]
    unsafe {
        std::env::set_var("WGPU_DX12_PRESENTATION_SYSTEM", "visual");
    }

    if std::env::args().any(|arg| arg == "--help" || arg == "-h") {
        println!("{USAGE}");
        return Ok(());
    }
    crate::platform::kill_previous_instances();

    // Loaded again in App::new; read here for the hotkey and window flags.
    let mut config = crate::config::Config::load(config_path_arg().as_deref());
    // Resolve the vault's theme *before* the builder below registers its fonts,
    // and identically to App::new's own resolution, so both agree on the file.
    config.adopt_vault_theme();

    // What to open: CLI arg > configured file > the embedded scratch board.
    // First-run (no configured canvas) lands on the scratch board, whose
    // in-canvas buttons create or open a real, autosaving board.
    let boot = match preselected_canvas(positional_canvas(), &config.canvas) {
        Some(path) => Boot::Canvas(path),
        None => Boot::Scratch,
    };

    // The summon hotkey is registered in App::new (which keeps the manager so
    // the shortcut can be re-registered live from settings).
    let settings = window_settings(&config, is_windowed(&config));

    // A `daemon` on every platform (not `application`): it renders per window, so
    // the settings window carries its own content, and it survives with no window
    // open (the Linux hide = close/reopen). The boot task opens the main window;
    // `daemon`'s `view`/`title` take a window id, dispatched below.
    let mut app = iced::daemon(
        move || {
            let (mut app, task) = App::new(boot.clone());
            let (id, open) = window::open(settings.clone());
            app.main_window = Some(id);
            (app, iced::Task::batch([task, open.discard()]))
        },
        App::update,
        daemon_view,
    );
    load_ui_fonts();
    // The theme's embedded fonts, registered via the builder rather than
    // `iced::font::load`: the builder's fonts are loaded when the compositor is
    // created, whereas a `LoadFont` task in the boot can run *before* the window
    // (and its compositor) exists and be silently dropped — leaving the live
    // text path to fall back to a system font.
    //
    // Resolved the same way `App::new` resolves it, so the faces registered
    // here are the ones the board will actually ask for — a named theme brings
    // none, and registering some other stylesheet's would be dead weight.
    for data in super::theme_store::resolve_base(&config).1 {
        app = app.font(data);
    }
    app.default_font(UI_FONT)
        .subscription(App::subscription)
        .theme(|state: &App, window| {
            if Some(window) == state.settings_window {
                settings_theme(state)
            } else {
                theme(state)
            }
        })
        .style(style)
        .antialiasing(true)
        .title(daemon_title)
        .run()
}

/// Gives iced's font system the same system UI faces the raster path loads, so
/// the live text path resolves the italic/mono faces (not a generic fallback),
/// a bold that isn't SF Mono, and the same `monospace` the bitmaps use
/// (`text_raster::register_ui_families`).
///
/// Written straight into the global font system rather than handed to the
/// builder like the theme's fonts below, because the weight aliases have to be
/// added *after* the faces they alias — and a builder font isn't loaded until
/// the compositor exists, past any point we could add them. Nothing has shaped
/// text yet, so there is no cache for this to leave stale.
pub(super) fn load_ui_fonts() {
    let mut fonts = iced_graphics::text::font_system().write().expect("font system");
    for file in crate::platform::UI_FONT_FILES {
        if let Ok(bytes) = std::fs::read(file) {
            fonts.load_font(bytes.into());
        }
    }
    crate::text_raster::register_ui_families(fonts.raw().db_mut());
}

/// `daemon` renders per window: the main window shows the board, the settings
/// window (when open) shows the settings form. Free fns (not closures) so they
/// satisfy the higher-ranked lifetime bound on the borrowed `Element` — a closure
/// infers one fixed lifetime and iced rejects it as "not general enough".
fn daemon_view(state: &App, window: window::Id) -> iced::Element<'_, Message> {
    if Some(window) == state.settings_window {
        let form = super::settings::view(state);
        // The model picker, when it was opened from this form, over it.
        match state.model_picker.as_ref().filter(|picker| picker.over_settings) {
            Some(picker) => iced::widget::stack![form, state.model_picker_view(picker)].into(),
            None => form,
        }
    } else {
        state.view()
    }
}

fn daemon_title(state: &App, window: window::Id) -> String {
    if Some(window) == state.settings_window {
        "ThinkBoard Settings".to_owned()
    } else if state.scratch {
        "ThinkBoard — Scratch".to_owned()
    } else {
        "ThinkBoard".to_owned()
    }
}

/// The settings window: a normal decorated, resizable, opaque window. We route
/// its close ourselves (`exit_on_close_request: false`) so closing it doesn't
/// quit the app.
pub(super) fn settings_window_settings() -> window::Settings {
    window::Settings {
        size: Size::new(820.0, 760.0),
        resizable: true,
        decorations: true,
        transparent: false,
        exit_on_close_request: false,
        icon: window_icon(),
        ..window::Settings::default()
    }
}

/// The window/taskbar icon, decoded from the PNG embedded from `assets/`. Mainly
/// matters on Linux (where there's no bundle/exe icon); macOS uses the `.app`'s
/// `.icns` and Windows the icon embedded in the `.exe`, so it's redundant there.
fn window_icon() -> Option<window::Icon> {
    const PNG: &[u8] = include_bytes!("../../assets/thinkboard.png");
    let image = image::load_from_memory(PNG).ok()?.into_rgba8();
    let (w, h) = image.dimensions();
    window::icon::from_rgba(image.into_raw(), w, h).ok()
}

/// The window configuration for a launch. In overlay mode (not `--windowed`)
/// it's an undecorated, always-on-top, screen-filling surface: the platform
/// layer stretches it to the screen after creation (macOS/Windows), while Linux
/// has no such hook so it starts borderless-fullscreen (the compositor keeps it
/// transparent). Kept a pure function of the config so Linux can rebuild it for
/// each re-summon.
pub(super) fn window_settings(config: &crate::config::Config, windowed: bool) -> window::Settings {
    let mut settings = window::Settings {
        size: Size::new(1400.0, 900.0),
        // We handle the close request ourselves (App::update) to flush the
        // autosave before quitting.
        exit_on_close_request: false,
        icon: window_icon(),
        ..window::Settings::default()
    };
    if !windowed {
        // Made see-through wherever the opacity can be switched later, which
        // the setting then does as the overlay comes up — so turning it back
        // on doesn't need a window that was never made able to be.
        settings.transparent = crate::platform::OVERLAY_TRANSPARENT
            && (crate::platform::LIVE_TRANSPARENCY || config.overlay_transparent);
        settings.decorations = false;
        settings.resizable = false;
        settings.level = window::Level::AlwaysOnTop;
        settings.position = window::Position::Specific(Point::ORIGIN);
        #[cfg(not(any(target_os = "macos", windows)))]
        {
            settings.fullscreen = true;
        }
    }
    settings
}

/// Whether the `--windowed` CLI flag was passed.
fn windowed_flag() -> bool {
    std::env::args().any(|arg| arg == "--windowed")
}

/// Plain-window mode, the pre-overlay behavior: the `--windowed` CLI flag (a dev
/// override) or the persisted `windowed` setting.
pub(super) fn is_windowed(config: &crate::config::Config) -> bool {
    windowed_flag() || config.windowed
}

/// First positional argument (the canvas path), skipping flags and the
/// value that follows `--config`.
pub(super) fn positional_canvas() -> Option<String> {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--config" {
            args.next();
        } else if !arg.starts_with("--") {
            return Some(arg);
        }
    }
    None
}

/// Value of the `--config <path>` argument, if present.
pub(super) fn config_path_arg() -> Option<String> {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--config" {
            return args.next();
        }
    }
    None
}

pub(super) fn theme(_app: &App) -> Theme {
    Theme::Dark
}

/// A custom iced theme for the settings window, derived from the Obsidian
/// `ColorTheme`, so every widget in it recolors with the loaded theme. (The
/// main overlay stays `Theme::Dark`; its chrome is styled per-widget.)
pub(super) fn settings_theme(app: &App) -> Theme {
    let c = &app.theme;
    Theme::custom(
        "ThinkBoard".to_owned(),
        iced::theme::Palette {
            background: c.background,
            text: c.text,
            primary: c.accent,
            success: iced::Color::from_rgb(0.30, 0.72, 0.47),
            warning: iced::Color::from_rgb(0.90, 0.68, 0.24),
            danger: iced::Color::from_rgb(0.90, 0.37, 0.37),
        },
    )
}

/// Forwards global hotkey presses (F15) into the update loop.
pub(super) fn hotkey_toggles() -> impl iced::futures::Stream<Item = Message> {
    iced::stream::channel(
        4,
        |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
            use iced::futures::{SinkExt, StreamExt};

            let (sender, mut receiver) = iced::futures::channel::mpsc::unbounded();
            GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
                if event.state == HotKeyState::Pressed {
                    let _ = sender.unbounded_send(());
                }
            }));

            while receiver.next().await.is_some() {
                let _ = output.send(Message::ToggleOverlay).await;
            }
        },
    )
}

pub(super) fn style(app: &App, theme: &Theme) -> iced::theme::Style {
    use iced::theme::Base;

    let mut style = theme.base();
    if app.overlay && crate::platform::OVERLAY_TRANSPARENT && app.config.overlay_transparent {
        // Let the desktop show through; the canvas paints the dim itself.
        // Where the platform can't composite it, keep the opaque themed
        // background so the overlay reads as a dark canvas, not a black void.
        style.background_color = iced::Color::TRANSPARENT;
    }
    style
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
            Named::Shift | Named::Control | Named::Alt | Named::Super | Named::Meta | Named::Hyper,
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

/// Filters raw events while recording a summon hotkey: forwards each key press
/// so [`App::capture_hotkey`] can turn one into the shortcut. Subscribed only
/// while recording (see `App::subscription`).
pub(super) fn capture_hotkey_event(
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

impl App {
    /// Turns a captured key + modifiers into a summon-hotkey string, validates it
    /// with the real parser, and sets it if valid. Escape cancels; a bare
    /// modifier keeps waiting for the actual key.
    pub(super) fn capture_hotkey(
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
        if parse_hotkey(&candidate).is_some() {
            self.config.summon_hotkey = candidate.clone();
            self.persist_config();
            self.settings_drafts
                .insert(super::settings::Field::SummonHotkey, candidate);
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
        if let Some(hotkey) = summon_hotkey(&self.config.summon_hotkey) {
            match manager.register(hotkey) {
                Ok(()) => self.registered_hotkey = Some(hotkey),
                Err(error) => eprintln!("could not register the summon hotkey: {error}"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{hotkey_string, parse_hotkey, platform_key, preselected_canvas, summon_hotkey};
    use global_hotkey::hotkey::{Code, Modifiers};
    // The recorder works in iced's key vocabulary; the registrar in
    // global_hotkey's. Both name a `Code`/`Modifiers`, so alias iced's apart.
    use iced::keyboard::Modifiers as KeyMods;
    use iced::keyboard::key::{Code as KeyCode, Key, Named, Physical};

    #[test]
    fn canvas_arg_wins_over_config() {
        assert_eq!(
            preselected_canvas(Some("cli.canvas".into()), "configured.canvas"),
            Some("cli.canvas".into())
        );
    }

    #[test]
    fn configured_canvas_used_only_when_it_exists() {
        // A path that doesn't exist → prompt (None).
        assert_eq!(preselected_canvas(None, "/no/such/file.canvas"), None);
        // Blank config → prompt (None).
        assert_eq!(preselected_canvas(None, ""), None);

        // An existing configured file is used.
        let dir = std::env::temp_dir().join(format!("tb-canvas-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("board.canvas");
        std::fs::write(&path, b"{}").unwrap();
        assert_eq!(
            preselected_canvas(None, &path.to_string_lossy()),
            Some(path.clone())
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parses_a_modifier_and_function_key() {
        let hk = summon_hotkey("Alt+F12").unwrap();
        assert_eq!(hk.mods, Modifiers::ALT);
        assert_eq!(hk.key, Code::F12);
    }

    #[test]
    fn tolerates_whitespace_and_case() {
        let hk = summon_hotkey("  cmd + shift + k ").unwrap();
        assert_eq!(hk.mods, Modifiers::SUPER | Modifiers::SHIFT);
        assert_eq!(hk.key, Code::KeyK);
    }

    #[test]
    fn blank_disables_and_invalid_falls_back() {
        // Blank = no hotkey.
        assert!(summon_hotkey("").is_none());
        assert!(summon_hotkey("   ").is_none());
        // Unparseable falls back to the default (still a hotkey).
        assert!(summon_hotkey("Alt+NotAKey").is_some());
    }

    #[test]
    fn pc_special_keys_map_to_f13_f14_f15_on_macos() {
        let mac = cfg!(target_os = "macos");
        assert_eq!(platform_key(Code::PrintScreen), if mac { Code::F13 } else { Code::PrintScreen });
        assert_eq!(platform_key(Code::ScrollLock), if mac { Code::F14 } else { Code::ScrollLock });
        assert_eq!(platform_key(Code::Pause), if mac { Code::F15 } else { Code::Pause });
        // Ordinary keys are unchanged on every platform.
        assert_eq!(platform_key(Code::F12), Code::F12);
    }

    #[test]
    fn hotkey_string_builds_parseable_combinations() {
        let k = Physical::Code(KeyCode::KeyK);
        // A plain letter (uppercased); modifiers ordered Ctrl, Alt, Shift, Super.
        assert_eq!(
            hotkey_string(&Key::Character("k".into()), k, KeyMods::empty()),
            Some("K".to_owned())
        );
        // Modifier order follows macOS's ⌃⌥⇧⌘ (Super/Cmd last).
        assert_eq!(
            hotkey_string(&Key::Character("k".into()), k, KeyMods::LOGO | KeyMods::SHIFT),
            Some("Shift+Super+K".to_owned())
        );
        // Named keys use their code name.
        assert_eq!(
            hotkey_string(&Key::Named(Named::F12), k, KeyMods::ALT),
            Some("Alt+F12".to_owned())
        );

        // Every produced string must be accepted by the registrar's parser —
        // otherwise pressing the current hotkey can't re-bind it.
        for combo in [
            hotkey_string(&Key::Character("k".into()), k, KeyMods::LOGO | KeyMods::SHIFT),
            hotkey_string(&Key::Named(Named::F12), k, KeyMods::ALT),
            hotkey_string(&Key::Named(Named::Space), k, KeyMods::CTRL),
        ] {
            let combo = combo.expect("a complete combination");
            assert!(parse_hotkey(&combo).is_some(), "parser rejected {combo:?}");
        }
    }

    #[test]
    fn hotkey_string_records_the_physical_latin_key_off_qwerty() {
        // A Cyrillic 'к' pressed on the physical K position records as latin "K",
        // so the recorded hotkey matches the physical Code the global hotkey
        // fires on (and the registrar's parser accepts it).
        let combo = hotkey_string(
            &Key::Character("к".into()),
            Physical::Code(KeyCode::KeyK),
            KeyMods::LOGO,
        );
        assert_eq!(combo, Some("Super+K".to_owned()));
        assert!(parse_hotkey(&combo.unwrap()).is_some());
    }

    #[test]
    fn hotkey_string_waits_through_bare_modifiers() {
        let k = Physical::Code(KeyCode::KeyK);
        // A held modifier alone (or an unidentified key) is not a combination.
        assert_eq!(hotkey_string(&Key::Named(Named::Shift), k, KeyMods::SHIFT), None);
        assert_eq!(hotkey_string(&Key::Named(Named::Super), k, KeyMods::LOGO), None);
        assert_eq!(hotkey_string(&Key::Unidentified, k, KeyMods::empty()), None);
    }
}
