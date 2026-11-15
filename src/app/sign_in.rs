//! Signing in to OpenRouter for a key, instead of pasting one — the one
//! provider that offers an app a sign-in (`thinkboard_core::sign_in`). It
//! isn't put forward over the others: the way to it is OpenRouter's own key
//! row, where the others have Paste alone.
//!
//! The browser does the signing in. The app opens OpenRouter's page there and
//! waits on a port of this machine for the code to come back — through the
//! site's relay page, which forwards it here — then trades the code for a key
//! and keeps it with OpenRouter's, as a pasted one is kept. One request is
//! answered, the one carrying this sign-in's `state`; anything else reaching
//! the port is turned away. After ten minutes, a code's life, it gives up.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use iced::{Task, window};
use thinkboard_core::sign_in::SignIn;

use super::settings::Field;
use super::{App, Message, Phase};

/// How long a sign-in waits on the browser: as long as the code would last.
const WAIT: Duration = Duration::from_secs(10 * 60);

/// What the browser is left showing once the code has reached the app.
const PAGE: &str = "<!doctype html><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\">\
<title>ThinkBoard</title><body style=\"margin:0;min-height:90vh;display:grid;place-items:center;\
font:16px system-ui,sans-serif;background:#1d1d20;color:#e6e6e6\">\
<p>Signed in. ThinkBoard is finishing up — you can close this tab.</p>";

/// A sign-in, as the key row shows it.
#[derive(Default)]
pub(super) enum Signing {
    #[default]
    Idle,
    /// Waiting on the browser, then on the key. Let go of, both stop.
    Waiting { _stop: Stop, _task: iced::task::Handle },
    /// Why the last one gave no key.
    Failed(String),
}

/// Stops the wait on the browser when it's let go of.
pub(super) struct Stop(Arc<AtomicBool>);

#[cfg(test)]
impl Stop {
    pub(super) fn new() -> Self {
        Stop(Arc::default())
    }
}

impl Drop for Stop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// Whether `url` is the endpoint a sign-in makes a key for.
pub(super) fn signs_in(url: &str) -> bool {
    url.trim().trim_end_matches('/') == thinkboard_core::sign_in::ENDPOINT
}

impl App {
    /// OpenRouter's sign-in in the browser, and a port here waiting for what
    /// comes back.
    pub(super) fn sign_in(&mut self) -> Task<Message> {
        let listening = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).and_then(|listener| {
            listener.set_nonblocking(true)?;
            let port = listener.local_addr()?.port();
            Ok((listener, port))
        });
        let (listener, port) = match listening {
            Ok(listening) => listening,
            Err(error) => {
                self.signing = Signing::Failed(format!("Couldn't wait for the browser to come back: {error}"));
                return Task::none();
            }
        };
        let Some(sign_in) = SignIn::new(port) else {
            self.signing = Signing::Failed("This system gave no randomness to sign in with.".to_owned());
            return Task::none();
        };
        crate::platform::open_url(&sign_in.url());
        let stop = Arc::new(AtomicBool::new(false));
        let (task, handle) = Task::perform(key(listener, sign_in, stop.clone()), Message::SignedIn).abortable();
        self.signing = Signing::Waiting { _stop: Stop(stop), _task: handle.abort_on_drop() };
        // The browser has it from here, and the models come up over the
        // board when it's done: the form in the board would only be in the way.
        let closed = if self.settings_overlay { self.close_settings_overlay() } else { Task::none() };
        Task::batch([closed, task])
    }

    pub(super) fn cancel_sign_in(&mut self) {
        self.signing = Signing::Idle;
    }

    /// A sign-in came to an end: ThinkBoard back in front of the browser, the
    /// key kept with OpenRouter's, and its models listed to choose from while
    /// it's still the provider, at the top of them a line saying the sign-in
    /// worked — or why there's no key.
    pub(super) fn signed_in(&mut self, key: Result<String, String>) -> Task<Message> {
        // Cancelled since: its answer is no one's.
        if !matches!(self.signing, Signing::Waiting { .. }) {
            return Task::none();
        }
        let forward = self.come_back();
        let key = match key {
            Ok(key) => key,
            Err(why) => {
                self.signing = Signing::Failed(why);
                return forward;
            }
        };
        self.signing = Signing::Idle;
        self.config.set_ai_key_for(thinkboard_core::sign_in::ENDPOINT, &key);
        self.persist_config();
        if !signs_in(&self.config.ai_url) {
            return forward;
        }
        self.settings_drafts.insert(Field::AiApiKey, key);
        let picker = self.open_model_picker_from_settings();
        if let Some(picker) = &mut self.model_picker {
            picker.notice = Some("Signed in to OpenRouter. The key is saved in your account as ThinkBoard.");
        }
        Task::batch([forward, picker])
    }

    /// The window the sign-in was started from, to the front: the browser
    /// had the screen meanwhile. The settings window where the form is in
    /// one; else the board's. The overlay comes back as its summon brings
    /// it, on every platform — summoned, if it was put away meanwhile, and
    /// raised (on macOS, made the active app) if not: a panel that doesn't
    /// activate takes no focus from a window's focus request, so that alone
    /// left it behind the browser.
    ///
    /// And it asks for attention, which is all a window can do under Wayland:
    /// there the focus is only ever given to what the person just used, and
    /// a request for it is ignored. Asked of an app in front already — macOS,
    /// Windows, once raised — it's ignored instead.
    fn come_back(&mut self) -> Task<Message> {
        let raise = match self.settings_window {
            Some(id) => window::minimize(id, false).chain(window::gain_focus(id)),
            None if self.overlay => match self.phase {
                Phase::Hidden => self.toggle_overlay(),
                Phase::Running | Phase::Entering { .. } => Task::done(Message::WindowShown),
                Phase::Exiting { .. } => Task::none(),
            },
            None => self.bring_forward(),
        };
        // After raising: on Linux a summon opens the overlay's window anew.
        let attention = self
            .settings_window
            .or(self.main_window)
            .map_or_else(Task::none, |id| window::request_user_attention(id, Some(window::UserAttention::Informational)));
        Task::batch([raise, attention])
    }
}

/// The key a sign-in comes back with: the code, when the browser brings it
/// here, traded for one.
async fn key(listener: TcpListener, sign_in: SignIn, stop: Arc<AtomicBool>) -> Result<String, String> {
    let (send, came_back) = iced::futures::channel::oneshot::channel();
    std::thread::Builder::new()
        .name("sign-in".into())
        .spawn(move || {
            let code = wait_for_code(&listener, &sign_in, &stop, Instant::now() + WAIT);
            let _ = send.send(code.map(|code| (code, sign_in)));
        })
        .map_err(|error| error.to_string())?;
    let (code, sign_in) = came_back.await.map_err(|_| "The sign-in stopped before the browser came back.".to_owned())??;
    sign_in.key(&code).await
}

/// The code, from the request that brings this sign-in's — once `stop` is
/// set, or by `until`, why there's none.
fn wait_for_code(listener: &TcpListener, sign_in: &SignIn, stop: &AtomicBool, until: Instant) -> Result<String, String> {
    loop {
        if stop.load(Ordering::Relaxed) {
            return Err("Cancelled.".to_owned());
        }
        if Instant::now() >= until {
            return Err("The browser didn't come back within ten minutes — sign in again.".to_owned());
        }
        match listener.accept() {
            Ok((mut stream, _)) => {
                if let Some(code) = answer(&mut stream, sign_in) {
                    return Ok(code);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(50)),
            Err(error) => return Err(error.to_string()),
        }
    }
}

/// Reads one request and answers it: the page that sends the person back to
/// the app, for this sign-in's code, and a 404 for anything else.
fn answer(stream: &mut TcpStream, sign_in: &SignIn) -> Option<String> {
    // Accepted from a listener that doesn't block, it may not either (macOS).
    stream.set_nonblocking(false).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    let mut head = Vec::new();
    let mut chunk = [0; 1024];
    while !head.windows(4).any(|end| end == b"\r\n\r\n") && head.len() < 8192 {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(read) => head.extend_from_slice(&chunk[..read]),
        }
    }
    let head = String::from_utf8_lossy(&head);
    let target = head.lines().next()?.strip_prefix("GET ")?.split(' ').next()?;
    let code = sign_in.code_in(target);
    let (status, body) = if code.is_some() { ("200 OK", PAGE) } else { ("404 Not Found", "") };
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sends `target` to the port as a browser would, and returns the
    /// status line it was answered with.
    fn browse(port: u16, target: &str) -> String {
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).expect("the port");
        write!(stream, "GET {target} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n").expect("a request");
        let mut answer = String::new();
        stream.read_to_string(&mut answer).expect("an answer");
        answer.lines().next().unwrap_or_default().to_owned()
    }

    #[test]
    fn the_port_answers_this_sign_ins_code_and_turns_the_rest_away() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("a port");
        listener.set_nonblocking(true).expect("not blocking");
        let port = listener.local_addr().expect("its address").port();
        let sign_in = SignIn::new(port).expect("randomness");
        let state = sign_in.state.clone();
        let stop = AtomicBool::new(false);
        let waiting = std::thread::spawn(move || wait_for_code(&listener, &sign_in, &stop, Instant::now() + WAIT));

        assert_eq!(browse(port, "/favicon.ico"), "HTTP/1.1 404 Not Found");
        assert_eq!(browse(port, "/callback?code=c0de&state=0.someone-else"), "HTTP/1.1 404 Not Found");
        assert_eq!(browse(port, &format!("/callback?code=c0de&state={state}")), "HTTP/1.1 200 OK");
        assert_eq!(waiting.join().expect("the wait"), Ok("c0de".to_owned()));
    }

    #[test]
    fn a_wait_stops_when_told_to_and_when_the_time_is_up() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("a port");
        listener.set_nonblocking(true).expect("not blocking");
        let sign_in = SignIn::new(1).expect("randomness");
        assert!(wait_for_code(&listener, &sign_in, &AtomicBool::new(true), Instant::now() + WAIT).is_err());
        assert!(wait_for_code(&listener, &sign_in, &AtomicBool::new(false), Instant::now()).is_err());
    }

    #[test]
    fn the_key_belongs_to_openrouters_endpoint_however_its_written() {
        assert!(signs_in("https://openrouter.ai/api/v1"));
        assert!(signs_in(" https://openrouter.ai/api/v1/ "));
        assert!(!signs_in("https://api.openai.com/v1"));
    }
}
