//! COSMIC-desktop backend for Linux (CachyOS / Pop!_OS / any cosmic session).
//!
//! An alternative to [`super::fallback`] for cosmic Wayland sessions. Selected
//! at runtime, not compile time: both backends are compiled on Linux and the
//! facade (`super`) forwards here when `is_cosmic_session()` — a release
//! binary carries both, so it runs correctly on KDE/GNOME too.
//!
//! What it provides over the fallback:
//!
//! * **Transparency** — the overlay window opens without `fullscreen`
//!   (startup consults [`NO_FULLSCREEN`] and sizes to the workarea instead):
//!   cosmic-comp composites fullscreen toplevels opaquely, which defeats
//!   `transparent: true`. Undecorated + maximized, it composites with alpha.
//! * **Summon** — a compositor-level shortcut, registered in
//!   cosmic-settings-daemon's custom-shortcuts config, running
//!   `<exe> --summon`. The spawned process hands the toggle to the resident
//!   instance over a Unix socket. `global-hotkey` cannot work under Wayland
//!   (it is X11/XWayland-only), so this replaces it on cosmic.
//!
//! Everything else mirrors `fallback.rs` (no-ops): cosmic exposes no
//! client-visible workspace-change channel, and iced has no pinch/gesture
//! hooks on Linux. Items the facade never forwards on Linux are simply absent
//! here rather than mirrored — `fallback` remains the single source of the
//! shared no-ops.

use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::OnceLock;

/// Whether this process runs inside a COSMIC desktop session.
///
/// Reads `XDG_CURRENT_DESKTOP` (cosmic sets it to `Cosmic`), cached. A bus
/// probe for `com.system76.CosmicComp` would be more precise, but would pull
/// in zbus for what an env var decides.
pub fn is_cosmic_session() -> bool {
    static COSMIC: OnceLock<bool> = OnceLock::new();
    *COSMIC.get_or_init(|| {
        std::env::var("XDG_CURRENT_DESKTOP")
            .map(|value| {
                value
                    .to_ascii_lowercase()
                    .split(':')
                    .any(|part| part == "cosmic")
            })
            .unwrap_or(false)
    })
}

// ---------------------------------------------------------------------------
// Window treatment
// ---------------------------------------------------------------------------

/// Whether the overlay's see-through can be switched while the app runs.
///
/// Linux closes and reopens the overlay window per summon anyway (a Wayland
/// toplevel cannot be hidden in place), and every open rebuilds the window
/// settings from the current config — so a see-through change lands at the
/// next summon without a restart. The settings UI accordingly drops its
/// "restart required" chip.
pub const LIVE_TRANSPARENCY: bool = true;

// Whether startup must avoid `window::Settings::fullscreen` — answered
// directly in `startup::window_settings` via `cosmic_backend_active()`, so no
// const is exported here.

/// No workspace-change notifications reach Wayland clients on cosmic, so the
/// overlay never auto-dismisses on a desktop switch. (The facade hardcodes
/// false for all of Linux; nothing to add.)

/// Post-creation window fixes. Nothing needed beyond what
/// `window_settings()` asks for at creation: winit clears the Wayland opaque
/// region for a `transparent` window already, and stacking under the panel is
/// what a workarea (maximized, undecorated) window yields by itself.
pub fn configure_overlay_window() {}

/// Hiding the window already hands focus to whatever is behind it here.
pub fn release_focus() {}

/// No notch/menu-bar inset to dodge on this platform.
pub fn top_safe_inset() -> f32 {
    0.0
}

/// Nothing to switch (see [`LIVE_TRANSPARENCY`]): the next summon reopens the
/// window from the current settings.
pub fn set_overlay_opaque(_opaque: bool) {}

// No backdrop picture, pinch or gesture support on this platform — the facade
// hardcodes all of Linux to false for these, so no consts here; the mirrored
// no-op functions live in `fallback` and the facade dispatches there.

// ---------------------------------------------------------------------------
// Summon: COSMIC custom shortcut -> `<exe> --summon` -> socket -> resident
// ---------------------------------------------------------------------------

/// The marker the custom-shortcut command ends with. Registration rewrites
/// only entries carrying it; foreign entries are never touched.
const SUMMON_MARKER: &str = "--summon";

/// Path of the resident instance's summon socket:
/// `$XDG_RUNTIME_DIR/thinkboard-$UID.sock`.
fn socket_path() -> Option<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")?;
    Some(PathBuf::from(runtime).join(format!("thinkboard-{}.sock", unsafe {
        getuid()
    })))
}

// `getuid(2)` alone — the nix crate for one libc call is not worth it.
// SAFETY: the function is called with no arguments and cannot fail; it has no
// preconditions and no memory effects.
unsafe extern "C" {
    fn getuid() -> u32;
}

/// Listens for `--summon` processes on the Unix socket, invoking `on_summon`
/// per connection. Spawns its own thread; the callback forwards into an iced
/// subscription channel (see `summon_stream` in `app/mod.rs`).
///
/// A previous instance's stale socket is removed first — an instance that died
/// uncleanly leaves it behind, and `bind` fails on an existing path.
pub fn listen_for_summon(on_summon: impl Fn() + Send + Sync + 'static) {
    let Some(path) = socket_path() else {
        eprintln!("cosmic: no XDG_RUNTIME_DIR; summon IPC disabled");
        return;
    };
    let _ = std::fs::remove_file(&path); // stale socket from a crashed instance

    let listener = match std::os::unix::net::UnixListener::bind(&path) {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!(
                "cosmic: could not bind the summon socket {}: {error}",
                path.display()
            );
            return;
        }
    };
    // Only this user may talk to the socket; XDG_RUNTIME_DIR is 0700 already.
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));

    std::thread::Builder::new()
        .name("summon-listener".into())
        .spawn(move || {
            use std::io::Read;
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                // Read the one line so the sender's write completes (its side
                // would otherwise see EPIPE when we close early).
                let mut buf = [0u8; 16];
                let _ = stream.read(&mut buf);
                on_summon();
            }
        })
        .expect("spawn summon-listener thread");
}

/// Asks a resident instance to toggle its overlay. Returns `true` when one
/// answered — the `--summon` handler in startup then exits silently.
pub fn request_summon() -> bool {
    let Some(path) = socket_path() else {
        return false;
    };
    let Ok(mut stream) = UnixStream::connect(&path) else {
        return false;
    };
    stream.write_all(b"summon\n").is_ok()
}

/// Writes the summon shortcut into cosmic-settings-daemon's custom-shortcuts
/// config, idempotently, preserving every foreign entry.
///
/// The file (`~/.config/cosmic/com.system76.CosmicSettings.Shortcuts/v1/custom`)
/// is a RON map of `Binding` → `Action`, merged over the defaults by
/// cosmic-comp, which watches it (inotify, via cosmic-config) and reloads live
/// — `Action::Spawn(cmd)` runs `/bin/sh -c cmd` with session env and an XDG
/// activation token. Structure verified against cosmic-settings-daemon's
/// `config/src/shortcuts/{binding,action,modifier}.rs` and against the live
/// file this Settings app wrote.
///
/// The merge is **textual**, not typed: ron's `Value` DOM loses enum variant
/// names (`Minimize` → `()`), so a typed rewrite of foreign entries is
/// impossible without depending on the daemon's own enum — which isn't
/// published to crates.io. Instead we drop exactly the entries whose command
/// carries [`SUMMON_MARKER`] and append ours; everything else is untouched.
pub fn ensure_summon_shortcut(hotkey_spec: &str) {
    if let Err(error) = ensure_summon_shortcut_inner(hotkey_spec) {
        eprintln!(
            "cosmic: could not register the summon shortcut automatically ({error}).\n\
             Add it by hand — Settings → Keyboard → Custom Shortcuts → command:\n  {}",
            summon_command()
        );
    }
}

fn ensure_summon_shortcut_inner(hotkey_spec: &str) -> Result<(), String> {
    let binding = summon_binding(hotkey_spec)?;
    let action = format!("Spawn({:?})", summon_command());

    let path = shortcuts_config_path();
    let existing = std::fs::read_to_string(&path).unwrap_or_default();

    // Drop our previous entries (identified by the marker inside their
    // Spawn(...) command), keep everything else, then append ours.
    // `entries_of` yields entries WITHOUT their trailing comma; each kept one
    // is re-terminated uniformly.
    let mut kept = String::with_capacity(existing.len() + action.len() + 32);
    for entry in entries_of(&existing) {
        if !entry.contains(SUMMON_MARKER) {
            kept.push_str(entry);
            kept.push_str(",\n");
        }
    }

    // Round-trip the composed file through ron's DOM as a syntax check: it
    // must parse even though we don't trust it to preserve variants.
    let composed = format!("{{\n{kept}{binding}: {action},\n}}");
    ron::from_str::<ron::Value>(&composed).map_err(|error| format!("generated RON is invalid: {error}"))?;

    // Write atomically (tmp + rename) so the daemon never reads a torn file;
    // named `.<file>.tmp` so the daemon's watcher ignores it while it exists.
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_file_name(format!(
        ".{}.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("custom")
    ));
    std::fs::write(&tmp, composed.as_bytes()).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    Ok(())
}

/// Splits a shortcuts RON map into its `(...): ...` entry texts.
///
/// Entries are recognized structurally: a `(` at map depth 0 opens the key
/// struct, the matching `)` closes it, then the value runs to the next `,` at
/// depth 0. String literals (and the parens/commas inside them) are skipped
/// over so a command like `sh -c "a, b"` cannot split an entry. Comments are
/// not the daemon's format; they'd only appear in hand-edited files, and the
/// `(`-scan treats them as text (a `(` inside a comment would confuse the
/// scan — acceptable: hand-edits that add comments to a machine file are
/// vanishingly rare, and the syntax check above rejects broken output before
/// anything is written).
fn entries_of(ron_map: &str) -> Vec<&str> {
    let bytes = ron_map.as_bytes();
    let mut entries = Vec::new();
    let mut depth = 0usize; // parens inside the current entry (keys + values)
    let mut entry_start: Option<usize> = None;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'(' => {
                if depth == 0 {
                    entry_start = Some(i);
                }
                depth += 1;
            }
            b')' => depth = depth.saturating_sub(1),
            b'"' => {
                // Skip a string literal, honoring escapes.
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    if bytes[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            b',' if depth == 0 => {
                if let Some(start) = entry_start.take() {
                    // Exclude the comma itself: callers re-terminate every
                    // kept entry uniformly (`entry + ","`), and a trailing
                    // entry without one stays comma-free.
                    entries.push(ron_map[start..i].trim());
                }
            }
            _ => {}
        }
        i += 1;
    }
    // A trailing entry without its comma (single-entry hand-edited file):
    // closed by the file end rather than a top-level comma.
    if let Some(start) = entry_start {
        entries.push(ron_map[start..].trim_end());
    }
    entries
}

/// `<exe> --summon` with the executable resolved from /proc/self/exe, so a dev
/// build registers itself rather than a stale installed copy. Quoting is
/// shell-safe (`/bin/sh -c` runs this string): paths with spaces must survive.
fn summon_command() -> String {
    let exe = std::env::current_exe()
        .ok()
        .and_then(|path| path.canonicalize().ok())
        .unwrap_or_else(|| PathBuf::from("thinkboard"));
    let quoted = format!("{:?}", exe.display().to_string());
    format!("{quoted} {SUMMON_MARKER}")
}

/// The cosmic-settings-daemon path for the shortcuts config.
fn shortcuts_config_path() -> PathBuf {
    let config_dir = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"));
    config_dir
        .join("cosmic")
        .join("com.system76.CosmicSettings.Shortcuts")
        .join("v1")
        .join("custom")
}

// ---------------------------------------------------------------------------
// Binding construction (daemon-format, textual)
// ---------------------------------------------------------------------------

/// Builds the `Binding` half of a custom-shortcut entry from our hotkey spec
/// (`"Pause"`, `"Ctrl+Shift+K"` — what `startup::parse_hotkey` accepts),
/// printed exactly as the daemon writes bindings: a struct with `modifiers`
/// (a list of enum variants in the daemon's own order: Super, Ctrl, Alt,
/// Shift) and `key` (the xkbcommon keysym name).
fn summon_binding(hotkey_spec: &str) -> Result<String, String> {
    let mut logo = false;
    let mut ctrl = false;
    let mut alt = false;
    let mut shift = false;
    let mut key: Option<String> = None;

    for token in hotkey_spec.split('+') {
        let token = token.trim();
        match token.to_ascii_lowercase().as_str() {
            "super" | "logo" | "cmd" => logo = true,
            "ctrl" | "control" => ctrl = true,
            "alt" | "option" => alt = true,
            "shift" => shift = true,
            _ => {
                if key.is_some() {
                    return Err(format!("multiple keys in hotkey {hotkey_spec:?}"));
                }
                key = Some(keysym_name(token)?);
            }
        }
    }
    let Some(key) = key else {
        return Err(format!("hotkey {hotkey_spec:?} has no key"));
    };

    let mut modifiers = Vec::new();
    if logo {
        modifiers.push("Super");
    }
    if ctrl {
        modifiers.push("Ctrl");
    }
    if alt {
        modifiers.push("Alt");
    }
    if shift {
        modifiers.push("Shift");
    }
    let modifiers = if modifiers.is_empty() {
        "[]".to_owned()
    } else {
        format!("[\n{},\n        ]", modifiers.join(",\n            "))
    };

    Ok(format!(
        "(\n        modifiers: {modifiers},\n        key: {key:?},\n    )"
    ))
}

/// Maps a hotkey token to an xkbcommon keysym name. xkbcommon accepts most
/// names verbatim (`Pause`, `F12`, `Print`); single printable characters are
/// their lowercase selves (`"K"` → `"k"`). The hotkey recorder's `Code`
/// (keyboard_types) names align with keysym names for everything it can emit;
/// the few divergences are normalized here. `keysym_from_name` on the daemon
/// side is case-sensitive first, so the case of these names matters.
fn keysym_name(token: &str) -> Result<String, String> {
    let compact: String = token.chars().filter(|c| !c.is_whitespace()).collect();
    if compact.is_empty() {
        return Err("empty key token".to_owned());
    }
    if compact.len() == 1 {
        let c = compact.chars().next().expect("len 1");
        if c.is_ascii_alphanumeric() {
            return Ok(c.to_ascii_lowercase().to_string());
        }
    }
    Ok(match compact.as_str() {
        // keyboard_types/iced `Named` names (what the hotkey recorder writes)
        // → xkbcommon keysym names. Every mapping below was verified against
        // `xkb_keysym_from_name(name, KEYSYM_NO_FLAGS)` resolving (the
        // daemon's strict pass; its case-insensitive fallback then never
        // fires, so no warning on the daemon side). "Pause", the default,
        // needs no mapping.
        "Space" => "space",
        "Enter" => "Return",
        "Backspace" => "BackSpace",
        "PageUp" => "Page_Up",
        "PageDown" => "Page_Down",
        "ArrowLeft" => "Left",
        "ArrowRight" => "Right",
        "ArrowUp" => "Up",
        "ArrowDown" => "Down",
        "PrintScreen" => "Print",
        "ScrollLock" => "Scroll_Lock",
        "NumLock" => "Num_Lock",
        "CapsLock" => "Caps_Lock",
        "ContextMenu" => "Menu",
        "Backquote" => "grave",
        "Quote" => "apostrophe",
        "Minus" => "minus",
        "Equal" => "equal",
        "Comma" => "comma",
        "Period" => "period",
        "Slash" => "slash",
        "BracketLeft" => "bracketleft",
        "BracketRight" => "bracketright",
        "Semicolon" => "semicolon",
        "Backslash" => "backslash",
        other => other,
    }
    .to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosmic_detection_reads_the_env_it_was_handed() {
        // Not asserting a value — the test process inherits whatever desktop
        // it runs under (this machine IS cosmic; CI is not). The env is also
        // read once into a `OnceLock` on first call, so mutating it here would
        // be both unsafe (edition 2024) and racy with other tests. What the
        // assertion pins: the call neither panics nor loops, and the answer
        // stays stable across calls (the cache works).
        let first = is_cosmic_session();
        let second = is_cosmic_session();
        assert_eq!(first, second);
    }

    #[test]
    fn binding_builds_basic_specs() {
        let b = summon_binding("Ctrl+Alt+T").expect("valid");
        assert!(b.contains("Ctrl") && b.contains("Alt"));
        assert!(b.contains(r#""t""#));

        let b = summon_binding("Super+Space").expect("valid");
        assert!(b.contains("Super"));
        assert!(b.contains(r#""space""#));
    }

    #[test]
    fn binding_rejects_bad_specs() {
        assert!(summon_binding("Ctrl").is_err()); // no key
        assert!(summon_binding("Ctrl+K+X").is_err()); // two keys
    }

    #[test]
    fn keysym_names_normalize() {
        assert_eq!(keysym_name("PrintScreen").as_deref(), Ok("Print"));
        assert_eq!(keysym_name("ScrollLock").as_deref(), Ok("Scroll_Lock"));
        assert_eq!(keysym_name("Pause").as_deref(), Ok("Pause"));
        assert_eq!(keysym_name("K").as_deref(), Ok("k"));
        assert_eq!(keysym_name("F12").as_deref(), Ok("F12"));
        assert_eq!(keysym_name("Space").as_deref(), Ok("space"));
        assert_eq!(keysym_name("Enter").as_deref(), Ok("Return"));
    }

    #[test]
    fn entries_split_respects_strings_and_nesting() {
        let text = r#"{
    (
        modifiers: [
            Super,
        ],
        key: "n",
    ): Minimize,
    (
        modifiers: [Ctrl, Alt],
        key: "t",
    ): Spawn("sh -c \"echo a, b\" --summon"),
}"#;
        let entries = entries_of(text);
        assert_eq!(entries.len(), 2, "comma inside the string must not split: {entries:#?}");
        assert!(entries[0].contains("Minimize"));
        assert!(entries[1].contains("--summon"));
    }

    #[test]
    fn textual_merge_preserves_foreign_and_replaces_ours() {
        let existing = r#"{
    (
        modifiers: [
            Super,
        ],
        key: "n",
    ): Minimize,
    (
        modifiers: [Ctrl, Alt],
        key: "p",
    ): Spawn("/old/path/thinkboard --summon"),
}"#;

        let binding = summon_binding("Pause").expect("valid");
        let action = format!("Spawn({:?})", "/new/path/thinkboard --summon");
        let mut kept = String::new();
        for entry in entries_of(existing) {
            if !entry.contains(SUMMON_MARKER) {
                kept.push_str(entry);
                kept.push_str(",\n");
            }
        }
        let composed = format!("{{\n{kept}{binding}: {action},\n}}");

        // Parses as RON…
        ron::from_str::<ron::Value>(&composed).expect("valid RON");
        // …keeps the foreign entry…
        assert!(composed.contains("Minimize"));
        // …dropped the old binding (old path)…
        assert!(!composed.contains("/old/path/"));
        // …and carries the new one.
        assert!(composed.contains("/new/path/thinkboard --summon"));
        // The binding is the Pause key struct — "Pause" passes through
        // unmapped (its keysym name IS "Pause").
        assert!(composed.contains(r#""Pause""#));
    }

    #[test]
    fn summon_command_quotes_spaces() {
        // Not testing the real exe path (env-dependent); the quoting rule is
        // Rust's Debug for str, which sh reads back correctly.
        let path = "/home/some user/target/debug/thinkboard";
        let quoted = format!("{:?}", path);
        assert_eq!(quoted, "\"/home/some user/target/debug/thinkboard\"");
    }
}
