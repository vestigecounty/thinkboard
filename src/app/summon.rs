//! The ways a summon reaches the app besides the global hotkey `startup`
//! registers: the desktop's own shortcut, where the desktop keeps them
//! (Wayland), and a second launch, from anything that can run a command.
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
//! **A second launch hands over to the copy running** (macOS, Linux), over a
//! socket in the user's runtime directory: it does what the summon key does
//! there — shows or hides the overlay, brings a window forward — or, given a
//! board, shows it and opens that board, and exits. So a desktop without the
//! portal (sway, an older GNOME, COSMIC) can bind a key to `thinkboard`, and
//! the app menu summons rather than restarts. A launch of another build, or in
//! the other mode, asks the copy running to quit — it saves first — and starts
//! in its place: what a freshly built binary, an upgrade, or a change to "Run
//! as a normal window" wants. Every launch used to kill every other copy, which
//! lost an edit made inside the autosave's debounce. Windows still does
//! (`platform::kill_previous_instances`).

#[cfg(unix)]
use std::io::{BufRead, BufReader, Read, Write};
#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};
#[cfg(unix)]
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::time::{Duration, Instant};

#[cfg(any(target_os = "linux", test))]
use global_hotkey::hotkey::{Code, HotKey, Modifiers};

#[cfg(unix)]
use iced::{Task, window};

#[cfg(unix)]
use super::{App, Message, Phase};

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
    /// No GlobalShortcuts portal to ask: a key bound to `thinkboard`, and the
    /// tray, are the ways in.
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

/// What a later launch asks of the copy already running (`hand_over`).
#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Launch {
    /// What the summon key does: shows or hides the overlay, or brings the
    /// window forward.
    Summon,
    /// Shows the board — never hides it — and opens this one on it.
    Open(PathBuf),
    /// Another build, or the other mode: quit, saving first, so the launch
    /// can start in this copy's place.
    Replace,
}

/// How long a launch waits for the copy running to answer before taking it
/// for one that's hung, or an older build that doesn't.
#[cfg(unix)]
const ANSWER_WITHIN: Duration = Duration::from_secs(2);

/// How long a copy making way has to save and go before it's killed.
#[cfg(unix)]
const QUIT_WITHIN: Duration = Duration::from_secs(5);

/// The longest request: a build and a path.
#[cfg(unix)]
const REQUEST_LIMIT: u64 = 8192;

/// What a launch does about a copy already running on the same config file.
/// True when that copy took the launch over, and this one is done. False when
/// this one should start: nothing was running, or what was is another build or
/// mode, and has quit to make way for it.
///
/// Nothing answering, on the default config, is an older build without the
/// socket, or one that's hung, and is killed as every launch used to kill.
/// A copy on a config file of its own kills nothing: it's a copy of its own.
#[cfg(unix)]
pub(super) fn hand_over(windowed: bool, canvas: Option<&Path>) -> bool {
    let path = socket_path();
    // The copy running has a working directory of its own.
    let canvas = canvas.and_then(|canvas| std::path::absolute(canvas).ok());
    let reply = ask(&path, &request(windowed, build(), canvas.as_deref()));
    match reply.as_deref().map(|reply| reply.split_once(' ').unwrap_or((reply, ""))) {
        Some(("ok", _)) => true,
        Some(("bye", pid)) => {
            make_way(&path, pid);
            false
        }
        _ => {
            if super::startup::config_path_arg().is_none() {
                crate::platform::kill_previous_instances();
            }
            false
        }
    }
}

/// This build, as a launch and the copy it finds compare it: the version, and
/// the executable's path and modification time, since a rebuilt binary is
/// another build at the same version. Read once, at launch — a rebuild replaces
/// the file under the copy that's running.
#[cfg(unix)]
fn build() -> &'static str {
    static BUILD: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    BUILD.get_or_init(|| {
        let exe = std::env::current_exe().unwrap_or_default();
        let modified = std::fs::metadata(&exe)
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |since| since.as_nanos());
        format!("{} {} {modified}", env!("CARGO_PKG_VERSION"), exe.display())
    })
}

#[cfg(unix)]
fn mode(windowed: bool) -> &'static str {
    if windowed { "window" } else { "overlay" }
}

/// A launch's request: `launch`, its mode, its build, and the board it was
/// given (empty for none), a tab apart — the path last, so a tab in it can't
/// move the others.
#[cfg(unix)]
fn request(windowed: bool, build: &str, canvas: Option<&Path>) -> String {
    let canvas = canvas.map(|canvas| canvas.to_string_lossy()).unwrap_or_default();
    format!("launch\t{}\t{build}\t{canvas}", mode(windowed))
}

/// What the copy running — `windowed` or an overlay, of `build` — makes of a
/// request. `None` for anything that isn't a launch, which is left alone.
#[cfg(unix)]
fn answer(request: &str, windowed: bool, build: &str) -> Option<Launch> {
    let mut fields = request.splitn(4, '\t');
    let (Some("launch"), Some(theirs), Some(their_build), Some(canvas)) =
        (fields.next(), fields.next(), fields.next(), fields.next())
    else {
        return None;
    };
    Some(if (theirs, their_build) != (mode(windowed), build) {
        Launch::Replace
    } else if canvas.is_empty() {
        Launch::Summon
    } else {
        Launch::Open(PathBuf::from(canvas))
    })
}

/// Sends `request` to the copy listening at `path`, and returns its one-line
/// reply: `None` when nothing listens there, or what does doesn't answer.
#[cfg(unix)]
fn ask(path: &Path, request: &str) -> Option<String> {
    let mut stream = UnixStream::connect(path).ok()?;
    stream.set_read_timeout(Some(ANSWER_WITHIN)).ok()?;
    writeln!(stream, "{request}").ok()?;
    let mut reply = String::new();
    BufReader::new(stream.take(64)).read_line(&mut reply).ok()?;
    let reply = reply.trim();
    (!reply.is_empty()).then(|| reply.to_owned())
}

/// Waits for the copy that said it's quitting to stop listening — saving
/// takes a moment — and kills it, by the `pid` it gave, if it hasn't by
/// [`QUIT_WITHIN`].
#[cfg(unix)]
fn make_way(path: &Path, pid: &str) {
    let deadline = Instant::now() + QUIT_WITHIN;
    while UnixStream::connect(path).is_ok() {
        if Instant::now() > deadline {
            if let Ok(pid) = pid.parse::<u32>() {
                let _ = std::process::Command::new("kill").arg(pid.to_string()).status();
            }
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Where the running copy listens: the user's runtime directory, private to
/// them, or failing that the temp directory under the user's name, since there
/// it's shared.
#[cfg(unix)]
fn socket_path() -> PathBuf {
    let name = socket_name(super::startup::config_path_arg().as_deref().map(Path::new));
    match std::env::var_os("XDG_RUNTIME_DIR").filter(|dir| !dir.is_empty()) {
        Some(dir) => PathBuf::from(dir).join(format!("{name}.sock")),
        None => {
            let user = std::env::var("USER").unwrap_or_default();
            std::env::temp_dir().join(format!("{name}-{user}.sock"))
        }
    }
}

/// The socket's name for a copy on `config` (`None`: the default). A copy on a
/// config file of its own — a throwaway one, say — listens apart, so it
/// neither takes the everyday copy's launches nor makes way for them. The path
/// is hashed with FNV-1a, which every build agrees on, where std's hasher
/// promises nothing across releases.
#[cfg(unix)]
fn socket_name(config: Option<&Path>) -> String {
    let Some(config) = config else {
        return "thinkboard".to_owned();
    };
    let config = std::path::absolute(config).unwrap_or_else(|_| config.to_path_buf());
    let hash = config
        .as_os_str()
        .as_encoded_bytes()
        .iter()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, &byte| (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3));
    format!("thinkboard-{hash:016x}")
}

/// Listens for later launches (`hand_over`), answering each as this copy —
/// `windowed` or an overlay, and of this build — and passing it on as
/// `Launched`.
#[cfg(unix)]
pub(super) fn launches(windowed: &bool) -> impl iced::futures::Stream<Item = Message> + use<> {
    let windowed = *windowed;
    iced::stream::channel(4, move |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
        use iced::futures::{SinkExt, StreamExt};

        let Some(listener) = listen(&socket_path()) else {
            return;
        };
        let (sender, mut receiver) = iced::futures::channel::mpsc::unbounded();
        std::thread::spawn(move || {
            serve(listener, |request| answer(request, windowed, build()), |launch| {
                let _ = sender.unbounded_send(launch);
            });
        });
        while let Some(launch) = receiver.next().await {
            let _ = output.send(Message::Launched(launch)).await;
        }
    })
}

/// Takes `path` to listen on. A socket already there is the last copy's,
/// which quit without removing it, or was killed.
#[cfg(unix)]
fn listen(path: &Path) -> Option<UnixListener> {
    use std::os::unix::fs::PermissionsExt;

    let _ = std::fs::remove_file(path);
    match UnixListener::bind(path) {
        Ok(listener) => {
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
            Some(listener)
        }
        Err(error) => {
            eprintln!("a second launch won't reach this copy: {error}");
            None
        }
    }
}

/// Answers each connection's one line, in turn, and then acts on it — the
/// reply first, since a launch this copy is making way for must hear so before
/// it goes. A sender that never finishes its line is given up on rather than
/// left holding up the rest.
#[cfg(unix)]
fn serve(listener: UnixListener, mut answer: impl FnMut(&str) -> Option<Launch>, mut act: impl FnMut(Launch)) {
    for mut stream in listener.incoming().flatten() {
        let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
        let mut request = String::new();
        if BufReader::new((&stream).take(REQUEST_LIMIT)).read_line(&mut request).is_err() {
            continue;
        }
        let Some(launch) = answer(request.trim_end_matches(['\n', '\r'])) else {
            continue;
        };
        let reply = match launch {
            Launch::Replace => format!("bye {}", std::process::id()),
            Launch::Summon | Launch::Open(_) => "ok".to_owned(),
        };
        let _ = writeln!(stream, "{reply}");
        act(launch);
    }
}

#[cfg(unix)]
impl App {
    /// A later launch handed itself over to this copy (`hand_over`).
    pub(super) fn launched(&mut self, launch: Launch) -> Task<Message> {
        match launch {
            // As the key's own message, so recording a new key ignores it too.
            Launch::Summon if self.overlay => Task::done(Message::ToggleOverlay),
            Launch::Summon => self.bring_forward(),
            Launch::Open(path) => {
                let open = self.open_canvas(path);
                let show = if !self.overlay {
                    self.bring_forward()
                } else if matches!(self.phase, Phase::Hidden) {
                    self.toggle_overlay()
                } else {
                    Task::none()
                };
                Task::batch([open, show])
            }
            Launch::Replace => self.save_and_exit(),
        }
    }

    /// The window to the front, as launching an app that's running does.
    fn bring_forward(&self) -> Task<Message> {
        match self.main_window {
            Some(id) => window::minimize(id, false).chain(window::gain_focus(id)),
            None => Task::none(),
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
    fn a_launch_of_the_same_build_and_mode_is_taken_over_and_any_other_replaces_the_copy_running() {
        use super::{Launch, answer, request};
        use std::path::Path;

        let ours = "0.9.0 /bin/thinkboard 1";
        let from = |windowed, build, canvas: Option<&str>| answer(&request(windowed, build, canvas.map(Path::new)), false, ours);
        assert_eq!(from(false, ours, None), Some(Launch::Summon));
        assert_eq!(from(false, ours, Some("/b/trip.canvas")), Some(Launch::Open("/b/trip.canvas".into())));
        assert_eq!(from(false, "0.9.0 /bin/thinkboard 2", None), Some(Launch::Replace), "rebuilt");
        assert_eq!(from(false, "0.9.1 /bin/thinkboard 1", Some("/b/trip.canvas")), Some(Launch::Replace), "upgraded");
        assert_eq!(from(true, ours, None), Some(Launch::Replace), "asked for a window, running as an overlay");
        assert_eq!(from(false, ours, Some("/b/a\tb.canvas")), Some(Launch::Open("/b/a\tb.canvas".into())));
        assert_eq!(answer("toggle", false, ours), None, "not a launch");
        assert_eq!(answer("", false, ours), None);
    }

    #[cfg(unix)]
    #[test]
    fn a_copy_on_a_config_file_of_its_own_listens_apart_under_a_name_every_build_agrees_on() {
        use super::socket_name;
        use std::path::Path;

        assert_eq!(socket_name(None), "thinkboard");
        assert_eq!(socket_name(Some(Path::new("/tmp/x.json"))), "thinkboard-a04df78558a74320");
        assert_ne!(socket_name(Some(Path::new("/tmp/y.json"))), socket_name(Some(Path::new("/tmp/x.json"))));
    }

    #[cfg(unix)]
    #[test]
    fn the_copy_listening_answers_a_launch_before_acting_on_it_and_ignores_anything_else() {
        use super::{Launch, answer, ask, listen, request, serve};

        let dir = std::env::temp_dir().join(format!("thinkboard-summon-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("thinkboard.sock");
        assert_eq!(ask(&path, &request(false, "b1", None)), None, "nothing listening yet: the launch starts");

        // A socket left by a copy that quit, or was killed, is taken over.
        std::fs::write(&path, "").unwrap();
        let listener = listen(&path).expect("listening");
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || serve(listener, |request| answer(request, false, "b1"), move |launch| sender.send(launch).unwrap()));

        assert_eq!(ask(&path, &request(false, "b1", None)).as_deref(), Some("ok"));
        assert_eq!(ask(&path, "toggle"), None, "not a launch: nothing said, nothing done");
        assert_eq!(ask(&path, &request(false, "b2", None)), Some(format!("bye {}", std::process::id())), "with the pid to kill if it doesn't go");
        let timeout = std::time::Duration::from_secs(5);
        assert_eq!(receiver.recv_timeout(timeout).unwrap(), Launch::Summon);
        assert_eq!(receiver.recv_timeout(timeout).unwrap(), Launch::Replace);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
