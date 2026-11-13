//! The ways a summon reaches the app besides the global hotkey `startup`
//! registers: the desktop's own shortcut, where the desktop keeps them
//! (Wayland), and `thinkboard --toggle`, from anything that can run a command.
//!
//! **Wayland gives an app no keys but its own.** `global-hotkey` grabs keys
//! through X11, and under Wayland that's XWayland, which hears a key only while
//! one of its own windows has focus — and ours is a native Wayland window. So
//! there the summon key is the desktop's to grant: we ask through the
//! GlobalShortcuts portal (KDE Plasma, GNOME 48 and later, Hyprland), the
//! desktop confirms it once in a dialog of its own, and from then on it's
//! changed in the desktop's keyboard settings, not ours. `config.summon_hotkey`
//! is only the key we suggest.
//!
//! **A desktop without the portal** (sway, an older GNOME) can still bind a key
//! to a command, so `thinkboard --toggle` tells the running copy to show or
//! hide, over a socket in the user's runtime directory, and starts one if none
//! is running. A plain second launch still replaces the first
//! (`platform::kill_previous_instances`), as a freshly built binary should.

#[cfg(unix)]
use std::io::{BufRead, BufReader, Read, Write};
#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};
#[cfg(unix)]
use std::path::PathBuf;

#[cfg(any(target_os = "linux", test))]
use global_hotkey::hotkey::{Code, HotKey, Modifiers};

#[cfg(unix)]
use super::Message;

/// What the desktop says about the summon shortcut, where it keeps it.
/// `App::summon_portal` is `None` wherever the global hotkey is the way in.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) enum Portal {
    /// Asked; the desktop may be showing its dialog.
    Asking,
    /// Bound, to the key the desktop names here — empty when it was confirmed
    /// without one, or the dialog was dismissed.
    Bound(String),
    /// No GlobalShortcuts portal to ask: `--toggle` and the tray are the ways in.
    Missing,
}

/// Whether this is a Linux session under Wayland, which is what winit picks
/// whenever a Wayland display is named — and then the global hotkey can't be
/// heard.
pub(crate) fn on_wayland() -> bool {
    cfg!(target_os = "linux") && std::env::var_os("WAYLAND_DISPLAY").is_some_and(|display| !display.is_empty())
}

/// The app's id on Linux: the name of its `.desktop` file, the Wayland
/// window's `app_id`, and what the portal files the shortcut under. Two
/// segments at least, which the portal insists on.
#[cfg(target_os = "linux")]
pub(crate) const APP_ID: &str = "com.vestigecounty.ThinkBoard";

/// The one shortcut we ask the portal for.
#[cfg(target_os = "linux")]
const SHORTCUT: &str = "summon";

/// Asks the desktop for the summon shortcut, suggesting `suggested`, and
/// forwards each press as `ToggleOverlay`. A blank suggestion suggests
/// nothing, and the desktop's dialog asks for a key: here the key is the
/// desktop's to keep, not ours to leave unset.
///
/// **Once a run** (`App::summon_suggested` is the config's at launch): a
/// session isn't closed when its stream is dropped, and every session's
/// presses reach every stream — asked again, one press would show the board
/// and hide it again. A new suggestion waits for the next launch, which is
/// soon enough for a key the desktop keeps from then on.
#[cfg(target_os = "linux")]
#[allow(clippy::ptr_arg)] // `Subscription::run_with` hands back the `String` it was given, by reference
pub(super) fn portal(suggested: &String) -> impl iced::futures::Stream<Item = Message> + use<> {
    use ashpd::desktop::ResponseError;
    use ashpd::desktop::global_shortcuts::{GlobalShortcuts, NewShortcut, Shortcut};
    use iced::futures::{SinkExt, StreamExt};

    // The trigger as the desktop's dialog first offers it; without one it
    // asks for a key from scratch.
    let trigger = super::startup::parse_hotkey(suggested).and_then(portal_trigger);
    iced::stream::channel(4, move |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
        let named = |shortcuts: &[Shortcut]| {
            let summon = shortcuts.iter().find(|shortcut| shortcut.id() == SHORTCUT);
            summon.map(|shortcut| shortcut.trigger_description().to_owned()).unwrap_or_default()
        };
        // A copy started from a terminal has no app id the portal can see,
        // and nothing to file the shortcut under; one started from the app
        // menu has, and then this is refused, harmlessly. An older portal
        // hasn't got the registry at all.
        if let Ok(id) = ashpd::AppID::try_from(APP_ID) {
            let _ = ashpd::register_host_app(id).await;
        }
        let bound = async {
            let portal = GlobalShortcuts::new().await?;
            let session = portal.create_session().await?;
            let shortcut = NewShortcut::new(SHORTCUT, "Show or hide the board").preferred_trigger(trigger.as_deref());
            let bound = portal.bind_shortcuts(&session, &[shortcut], None).await?.response();
            Ok::<_, ashpd::Error>((portal, session, bound))
        };
        // The session is held for as long as the shortcut is wanted: closing
        // it lets the shortcut go.
        let (portal, _session, bound) = match bound.await {
            Ok(bound) => bound,
            Err(error) => {
                eprintln!("global shortcuts portal: {error}");
                let _ = output.send(Message::SummonPortal(Portal::Missing)).await;
                return;
            }
        };
        let said = match bound {
            Ok(bound) => named(bound.shortcuts()),
            // Dismissed: there's a portal, and no key in it yet.
            Err(ashpd::Error::Response(ResponseError::Cancelled)) => String::new(),
            Err(error) => {
                eprintln!("global shortcuts portal: {error}");
                let _ = output.send(Message::SummonPortal(Portal::Missing)).await;
                return;
            }
        };
        let _ = output.send(Message::SummonPortal(Portal::Bound(said))).await;

        let (Ok(pressed), Ok(changed)) = (portal.receive_activated().await, portal.receive_shortcuts_changed().await)
        else {
            return;
        };
        let pressed = pressed
            .filter(|activated| std::future::ready(activated.shortcut_id() == SHORTCUT))
            .map(|_| Message::ToggleOverlay);
        // Changed in the desktop's settings while we run.
        let changed = changed.map(move |changed| Message::SummonPortal(Portal::Bound(named(changed.shortcuts()))));
        let mut messages = std::pin::pin!(iced::futures::stream::select(pressed, changed));
        while let Some(message) = messages.next().await {
            let _ = output.send(message).await;
        }
    })
}

/// `config.summon_hotkey` as the shortcuts spec writes a trigger —
/// `CTRL+SHIFT+k`: its modifier names, then the key's xkb name. `None` for a
/// key without one here; the desktop then asks for a key itself.
#[cfg(any(target_os = "linux", test))]
pub(super) fn portal_trigger(hotkey: HotKey) -> Option<String> {
    let key = match hotkey.key {
        Code::Space => "space".to_owned(),
        Code::Enter => "Return".to_owned(),
        Code::Tab => "Tab".to_owned(),
        Code::Escape => "Escape".to_owned(),
        Code::Backspace => "BackSpace".to_owned(),
        Code::Delete => "Delete".to_owned(),
        Code::Insert => "Insert".to_owned(),
        Code::Home => "Home".to_owned(),
        Code::End => "End".to_owned(),
        Code::PageUp => "Page_Up".to_owned(),
        Code::PageDown => "Page_Down".to_owned(),
        Code::ArrowUp => "Up".to_owned(),
        Code::ArrowDown => "Down".to_owned(),
        Code::ArrowLeft => "Left".to_owned(),
        Code::ArrowRight => "Right".to_owned(),
        Code::Pause => "Pause".to_owned(),
        Code::PrintScreen => "Print".to_owned(),
        Code::ScrollLock => "Scroll_Lock".to_owned(),
        Code::Minus => "minus".to_owned(),
        Code::Equal => "equal".to_owned(),
        Code::BracketLeft => "bracketleft".to_owned(),
        Code::BracketRight => "bracketright".to_owned(),
        Code::Backslash => "backslash".to_owned(),
        Code::Semicolon => "semicolon".to_owned(),
        Code::Quote => "apostrophe".to_owned(),
        Code::Backquote => "grave".to_owned(),
        Code::Comma => "comma".to_owned(),
        Code::Period => "period".to_owned(),
        Code::Slash => "slash".to_owned(),
        // Letters as xkb names them unshifted, digits and F-keys as they are.
        code => {
            let name = code.to_string();
            if let Some(letter) = name.strip_prefix("Key") {
                letter.to_ascii_lowercase()
            } else if let Some(digit) = name.strip_prefix("Digit") {
                digit.to_owned()
            } else if name.strip_prefix('F').is_some_and(|n| n.parse::<u8>().is_ok()) {
                name
            } else {
                return None;
            }
        }
    };
    let mods = [
        (Modifiers::CONTROL, "CTRL"),
        (Modifiers::ALT, "ALT"),
        (Modifiers::SHIFT, "SHIFT"),
        (Modifiers::SUPER | Modifiers::META, "LOGO"),
    ];
    let mut trigger: Vec<&str> = mods.iter().filter(|(mods, _)| hotkey.mods.intersects(*mods)).map(|(_, name)| *name).collect();
    trigger.push(&key);
    Some(trigger.join("+"))
}

/// Where the running copy listens for `--toggle`: the user's runtime
/// directory, private to them, or failing that the temp directory under the
/// user's name, since there it's shared.
#[cfg(unix)]
fn socket_path() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR").filter(|dir| !dir.is_empty()) {
        Some(dir) => PathBuf::from(dir).join("thinkboard.sock"),
        None => {
            let user = std::env::var("USER").unwrap_or_default();
            std::env::temp_dir().join(format!("thinkboard-{user}.sock"))
        }
    }
}

/// `thinkboard --toggle`: tells the running copy to show or hide. False when
/// nothing answers, and the caller starts one instead — which comes up shown.
#[cfg(unix)]
pub(super) fn send_toggle() -> bool {
    send(&socket_path(), "toggle")
}

#[cfg(unix)]
fn send(path: &std::path::Path, request: &str) -> bool {
    UnixStream::connect(path).and_then(|mut stream| writeln!(stream, "{request}")).is_ok()
}

/// Listens for `--toggle` and forwards each as `ToggleOverlay`.
#[cfg(unix)]
pub(super) fn toggle_requests() -> impl iced::futures::Stream<Item = Message> {
    iced::stream::channel(4, |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
        use iced::futures::{SinkExt, StreamExt};

        let Some(listener) = listen(&socket_path()) else {
            return;
        };
        let (sender, mut receiver) = iced::futures::channel::mpsc::unbounded();
        std::thread::spawn(move || serve(listener, |request| {
            if request == "toggle" {
                let _ = sender.unbounded_send(());
            }
        }));
        while receiver.next().await.is_some() {
            let _ = output.send(Message::ToggleOverlay).await;
        }
    })
}

/// Takes `path` to listen on. A socket already there is the last copy's,
/// killed at launch (`kill_previous_instances`) and unable to remove it.
#[cfg(unix)]
fn listen(path: &std::path::Path) -> Option<UnixListener> {
    use std::os::unix::fs::PermissionsExt;

    let _ = std::fs::remove_file(path);
    match UnixListener::bind(path) {
        Ok(listener) => {
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
            Some(listener)
        }
        Err(error) => {
            eprintln!("thinkboard --toggle won't reach this copy: {error}");
            None
        }
    }
}

/// Answers each connection's one line, in turn — a short line at most, and a
/// sender that never finishes it is given up on rather than left holding up
/// the rest.
#[cfg(unix)]
fn serve(listener: UnixListener, mut answer: impl FnMut(&str)) {
    for stream in listener.incoming().flatten() {
        let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(1)));
        let mut request = String::new();
        if BufReader::new(stream.take(64)).read_line(&mut request).is_ok() {
            answer(request.trim());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::portal_trigger;

    /// Parsed as on Linux, where `startup::parse_hotkey` leaves the key as it
    /// is — here it would make Pause the F15 macOS reports it as.
    fn trigger(spec: &str) -> Option<String> {
        portal_trigger(spec.parse().expect("a hotkey"))
    }

    #[test]
    fn a_hotkey_is_suggested_to_the_desktop_as_the_shortcuts_spec_writes_it() {
        assert_eq!(trigger("Pause").as_deref(), Some("Pause"));
        assert_eq!(trigger("Ctrl+Shift+K").as_deref(), Some("CTRL+SHIFT+k"));
        assert_eq!(trigger("Alt+F12").as_deref(), Some("ALT+F12"));
        assert_eq!(trigger("Super+Space").as_deref(), Some("LOGO+space"));
        assert_eq!(trigger("Ctrl+Alt+1").as_deref(), Some("CTRL+ALT+1"));
        assert_eq!(trigger("Ctrl+[").as_deref(), Some("CTRL+bracketleft"));
    }

    #[cfg(unix)]
    #[test]
    fn a_toggle_reaches_the_copy_listening_and_nothing_else_is_taken_for_one() {
        let dir = std::env::temp_dir().join(format!("thinkboard-summon-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("thinkboard.sock");
        assert!(!super::send(&path, "toggle"), "nothing listening yet: the caller starts a copy");

        // A socket left by a copy that was killed is taken over.
        std::fs::write(&path, "").unwrap();
        let listener = super::listen(&path).expect("listening");
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || super::serve(listener, move |request| sender.send(request.to_owned()).unwrap()));

        assert!(super::send(&path, "toggle"));
        assert!(super::send(&path, "something else"));
        let timeout = std::time::Duration::from_secs(5);
        assert_eq!(receiver.recv_timeout(timeout).unwrap(), "toggle");
        assert_eq!(receiver.recv_timeout(timeout).unwrap(), "something else", "passed on, and turned away by the caller");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
