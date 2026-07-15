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
) -> (Option<GlobalHotKeyManager>, Option<HotKey>) {
    if is_windowed() {
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
    let config = crate::config::Config::load(config_path_arg().as_deref());

    // What to open: CLI arg > configured file > the embedded scratch board.
    // First-run (no configured canvas) lands on the scratch board, whose
    // in-canvas buttons create or open a real, autosaving board.
    let boot = match preselected_canvas(positional_canvas(), &config.canvas) {
        Some(path) => Boot::Canvas(path),
        None => Boot::Scratch,
    };

    // The summon hotkey is registered in App::new (which keeps the manager so
    // the shortcut can be re-registered live from settings).
    let settings = window_settings(&config, is_windowed());

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
    // Give iced's font system the same faces the raster path loads, so the live
    // text path resolves the italic/mono faces (not a generic fallback).
    for file in crate::platform::UI_FONT_FILES {
        app = app.font(std::fs::read(file).unwrap_or_default());
    }
    // The theme's embedded fonts, registered via the builder rather than
    // `iced::font::load`: the builder's fonts are loaded when the compositor is
    // created, whereas a `LoadFont` task in the boot can run *before* the window
    // (and its compositor) exists and be silently dropped — leaving the live
    // text path to fall back to a system font.
    if let Some(css) = config
        .theme_css
        .as_deref()
        .and_then(|path| std::fs::read_to_string(path).ok())
    {
        let theme = crate::theme::Theme::from_css(&css);
        let mut fonts = crate::theme::embedded_fonts(&css);
        fonts.extend(crate::theme::synth_weight_faces(
            &fonts,
            &[theme.weight_normal, theme.weight_bold],
        ));
        for data in fonts {
            app = app.font(data);
        }
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

/// `daemon` renders per window: the main window shows the board, the settings
/// window (when open) shows the settings form. Free fns (not closures) so they
/// satisfy the higher-ranked lifetime bound on the borrowed `Element` — a closure
/// infers one fixed lifetime and iced rejects it as "not general enough".
fn daemon_view(state: &App, window: window::Id) -> iced::Element<'_, Message> {
    if Some(window) == state.settings_window {
        super::settings::view(state)
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
        settings.transparent =
            crate::platform::OVERLAY_TRANSPARENT && config.overlay_transparent;
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

/// Plain-window mode, the pre-overlay behavior.
pub(super) fn is_windowed() -> bool {
    std::env::args().any(|arg| arg == "--windowed")
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

#[cfg(test)]
mod tests {
    use super::{platform_key, preselected_canvas, summon_hotkey};
    use global_hotkey::hotkey::{Code, Modifiers};

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
}
