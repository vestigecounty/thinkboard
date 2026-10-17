//! An offscreen walk through the app, saved as PNGs — for looking at the UI
//! without a display, a window or a Screen Recording permission.
//!
//! It is the real `App`, driven the way a person drives it. Synthetic mouse and
//! keyboard events go through iced's `UserInterface` into the same widgets a
//! window would feed, and whatever those publish goes through `App::update`.
//! Only the runtime is ours, and only as much of it as these screens need:
//!
//! - tasks are polled until they finish, so rasterization and reclustering land
//!   exactly as they would, a frame or two later;
//! - widget operations (focusing an input) run against the tree;
//! - the raw-event handlers a subscription would see are fed from
//!   [`App::event_listeners`], the same list `subscription` maps;
//! - animations are ticked in real time until they stop.
//!
//! Frames are drawn by iced's headless wgpu renderer — the shadow shader and
//! the board's own canvas included — so a shot is what the window would show.
//!
//! ```sh
//! cargo test --locked -p thinkboard tour -- --ignored --nocapture
//! TOUR=first_run cargo test --locked -p thinkboard tour -- --ignored --nocapture
//! ```
//!
//! Shots land in `$TOUR_OUT` (default `target/tour/`), one folder per scene,
//! at 1× unless `TOUR_SCALE=2` asks for a retina frame, in the built-in theme
//! unless `TOUR_THEME` (a name) or `TOUR_THEME_CSS` (an Obsidian stylesheet)
//! says otherwise. The welcome board lists
//! whatever Obsidian vaults this machine has, as it would on a real first run.
//! Ignored by default: it needs a GPU adapter, which CI doesn't promise, and
//! it is a thing to look at rather than a thing that fails.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::LazyLock;
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use iced::advanced::renderer::Headless;
use iced::futures::stream::{BoxStream, StreamExt};
use iced::keyboard::{self, Key, Modifiers, key};
use iced::{Event, Rectangle, mouse};
use iced_runtime::core::widget::Operation;
use iced_runtime::core::widget::operation::Outcome;
use iced_runtime::core::{clipboard, renderer};
use iced_runtime::user_interface::{self, UserInterface};
use iced_runtime::{Action, task};

use super::*;

/// The logical size every scene is framed at: a 14-inch laptop's default.
const VIEWPORT: Size = Size::new(1440.0, 900.0);

/// How long [`Tour::settle`] waits on a quiet app before deciding it's done.
/// Long enough for a background raster to come back, short enough that typing
/// a sentence a key at a time doesn't take a minute.
const QUIET: Duration = Duration::from_millis(60);

/// How long [`Tour::shot`] leaves iced's image worker between its two frames.
const UPLOAD_GRACE: Duration = Duration::from_millis(250);

/// The runtime a window runs tasks on — Tokio, by iced's `tokio` feature.
/// The tour polls tasks itself, but a request made with `reqwest` registers
/// with Tokio's reactor wherever it's polled, and with none entered it panics
/// on the spot.
static RUNTIME: LazyLock<iced::executor::Default> =
    LazyLock::new(|| iced::executor::Default::new().expect("a Tokio runtime"));

/// A driven app and the offscreen renderer it draws into.
struct Tour {
    app: App,
    renderer: iced::Renderer,
    cache: Option<user_interface::Cache>,
    scale: f32,
    cursor: Point,
    modifiers: Modifiers,
    window: window::Id,
    /// Tasks `update` returned that haven't finished yet.
    tasks: Vec<BoxStream<'static, Action<Message>>>,
    /// When the pending answer's dots last stepped (see [`Tour::timers`]).
    dots: Instant,
    /// When the autosave last polled.
    saves: Instant,
    out: PathBuf,
    shots: usize,
    /// The window's size: [`VIEWPORT`], unless a scene [`Tour::resize`]s it.
    viewport: Size,
    /// Touring the settings window rather than the board's — see
    /// [`Tour::settings_window`].
    settings: bool,
}

/// What the toured window shows, as `startup::daemon_view` picks it.
fn root(app: &App, settings: bool) -> iced::Element<'_, Message> {
    if settings { startup::settings_window_view(app) } else { app.view() }
}

impl Tour {
    fn new(scene: &str, mut app: App) -> Self {
        let out = std::env::var_os("TOUR_OUT")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/tour"))
            .join(scene);
        let _ = std::fs::remove_dir_all(&out);
        std::fs::create_dir_all(&out).expect("tour output directory");

        // The faces `startup::run` gives iced: the system UI faces, then
        // whatever the theme embeds (which it hands the builder, and the
        // builder loads into this same font system once a compositor exists).
        startup::load_ui_fonts();
        {
            let mut fonts = iced_graphics::text::font_system().write().expect("font system");
            for data in theme_store::resolve_base(&app.config).1 {
                fonts.load_font(data.into());
            }
        }
        let renderer = block_on(<iced::Renderer as Headless>::new(
            UI_FONT,
            iced::Pixels(16.0),
            Some("wgpu"),
        ))
        .expect("a wgpu adapter for the offscreen renderer");

        let window = window::Id::unique();
        app.main_window = Some(window);
        let scale = std::env::var("TOUR_SCALE").ok().and_then(|s| s.parse().ok()).unwrap_or(1.0);
        let mut tour = Self {
            app,
            renderer,
            cache: None,
            scale,
            cursor: Point::new(VIEWPORT.width / 2.0, VIEWPORT.height / 2.0),
            modifiers: Modifiers::empty(),
            window,
            tasks: Vec::new(),
            dots: Instant::now(),
            saves: Instant::now(),
            out,
            shots: 0,
            viewport: VIEWPORT,
            settings: false,
        };
        // The board learns its size from the first event it sees.
        tour.redraw();
        tour
    }

    // --- The runtime, as far as these scenes need one ---

    /// Runs `events` through the widget tree and the raw-event handlers,
    /// applies everything they published, and settles.
    fn feed(&mut self, events: Vec<Event>) {
        self.deliver(events);
        self.settle();
        // And a frame, as a window would draw one — though nothing is read
        // back. Drawing isn't only output: the editor highlights in its draw,
        // and a run of keystrokes with no frame between them is an editor
        // asked to catch up on several edits at once, which a person typing
        // rarely makes it do. (That is how the tour found `change_line`
        // skipping lines; it is also why a tour that never drew showed it far
        // more often than anyone would see it.)
        self.draw();
    }

    /// [`Tour::feed`] short of settling: whatever the events start is still
    /// under way when it returns.
    fn deliver(&mut self, events: Vec<Event>) {
        let cursor = mouse::Cursor::Available(self.cursor);
        let mut messages = Vec::new();
        let statuses = {
            let Self { app, renderer, cache, viewport, settings, .. } = self;
            let mut ui = UserInterface::build(root(app, *settings), *viewport, cache.take().unwrap_or_default(), renderer);
            let (_, statuses) = ui.update(&events, cursor, renderer, &mut clipboard::Null, &mut messages);
            *cache = Some(ui.into_cache());
            statuses
        };
        // A subscription sees the app as it was when the event arrived, so the
        // listeners are taken before any of the messages land.
        let listeners = self.app.event_listeners();
        for (event, status) in events.into_iter().zip(statuses) {
            // `listen_with` never passes a redraw on.
            if matches!(event, Event::Window(window::Event::RedrawRequested(_))) {
                continue;
            }
            messages.extend(listeners.iter().filter_map(|listen| listen(event.clone(), status, self.window)));
        }
        for message in messages {
            self.send(message);
        }
    }

    /// Delivers one message, as the runtime would — keeping its task.
    ///
    /// Except the two that exist only to talk to the OS — re-raising the
    /// overlay's window, and handing the keyboard back to the app that had it
    /// — which want a real window on the main thread and change no frame.
    fn send(&mut self, message: Message) {
        if matches!(message, Message::WindowShown | Message::ReleaseFocus) {
            return;
        }
        if let Some(stream) = task::into_stream(self.app.update(message)) {
            self.tasks.push(stream);
        }
    }

    /// Polls every unfinished task once; true if any of them moved.
    fn pump(&mut self) -> bool {
        let _runtime = RUNTIME.enter();
        let mut context = Context::from_waker(Waker::noop());
        let mut moved = false;
        let mut i = 0;
        while i < self.tasks.len() {
            match self.tasks[i].poll_next_unpin(&mut context) {
                Poll::Ready(Some(action)) => {
                    moved = true;
                    self.act(action);
                }
                Poll::Ready(None) => {
                    moved = true;
                    drop(self.tasks.swap_remove(i));
                }
                Poll::Pending => i += 1,
            }
        }
        moved
    }

    /// The timers `subscription` runs, as far as these scenes need them: the
    /// dots in an answer still waiting for its first word, which nothing else
    /// steps, and the autosave's poll, on the same terms as the subscription's
    /// — while there's something unsaved on a board with a file.
    fn timers(&mut self) {
        if !self.app.ai.pending.is_empty() && self.dots.elapsed() >= ai::DOTS_INTERVAL {
            self.dots = Instant::now();
            self.send(Message::AiTick);
        }
        if self.app.doc_dirty && !self.app.scratch && !self.app.disk.missing && self.saves.elapsed() >= SAVE_POLL {
            self.saves = Instant::now();
            self.send(Message::SaveTick);
        }
    }

    /// Watches the open board's file, with the watcher `subscription` runs —
    /// the OS's own change events, not a `FileChanged` sent by hand, so a
    /// write the watcher would miss is missed here too. It's a stream that
    /// never ends, so every settle after this waits out [`QUIET`].
    fn watch(&mut self) {
        let path = self.app.canvas_path.clone();
        self.tasks.push(sync::watch_file(&path).map(Action::Output).boxed());
    }

    /// Writes `text` beside the scene's shots, in `notes.txt`, and prints it —
    /// for what a shot can't show, like what's on disk.
    fn note(&self, text: &str) {
        println!("tour: {text}");
        let mut notes = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.out.join("notes.txt"))
            .expect("the scene's notes");
        let _ = writeln!(notes, "{text}");
    }

    fn act(&mut self, action: Action<Message>) {
        match action {
            Action::Output(message) => self.send(message),
            Action::Widget(operation) => self.operate(operation),
            Action::LoadFont { bytes, channel } => {
                iced_graphics::text::font_system().write().expect("font system").load_font(bytes);
                let _ = channel.send(Ok(()));
            }
            Action::Clipboard(iced_runtime::clipboard::Action::Read { channel, .. }) => {
                let _ = channel.send(None);
            }
            // Window, system and image requests have no window to act on.
            // Dropping one drops its reply channel, which ends the task cleanly.
            _ => {}
        }
    }

    fn operate(&mut self, operation: Box<dyn iced_runtime::core::widget::Operation>) {
        let Self { app, renderer, cache, viewport, settings, .. } = self;
        let mut ui = UserInterface::build(root(app, *settings), *viewport, cache.take().unwrap_or_default(), renderer);
        let mut current = Some(operation);
        while let Some(mut operation) = current.take() {
            ui.operate(renderer, operation.as_mut());
            if let Outcome::Chain(next) = operation.finish() {
                current = Some(next);
            }
        }
        *cache = Some(ui.into_cache());
    }

    /// Runs tasks and animations until the app goes quiet.
    fn settle(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut quiet_since = Instant::now();
        loop {
            let mut busy = self.pump();
            self.timers();
            // Not `animating`: the ghosts' countdown would be waited out, and
            // the shot taken of a board they'd just left.
            if self.app.transitioning() {
                self.send(Message::Tick(Instant::now()));
                // Every animation frame is drawn *and presented*, as
                // `window::frames` drives a window — some of what animates
                // exists to be drawn unseen, like a bitmap warming its upload
                // before it's shown, and the upload happens in the frame's
                // preparation, which offscreen only a read-back runs.
                let _ = self.render();
                std::thread::sleep(Duration::from_millis(16));
                busy = true;
            }
            // A double-click's caret waits for the editor to be laid out,
            // which building the tree does.
            if self.app.editing.as_ref().is_some_and(|e| e.pending_caret.is_some()) {
                self.operate(Box::new(Noop));
                self.send(Message::PlaceEditorCaret);
                busy = true;
            }
            if busy {
                quiet_since = Instant::now();
            } else if self.tasks.is_empty() || quiet_since.elapsed() > QUIET {
                break;
            } else {
                std::thread::sleep(Duration::from_millis(2));
            }
            if Instant::now() > deadline {
                eprintln!("tour: still busy after 15 s; shooting anyway");
                break;
            }
        }
    }

    /// Settles until `done` says so — for a reply with pauses in it longer
    /// than [`QUIET`], which [`Tour::settle`] would take for the end of it.
    fn settle_until(&mut self, done: impl Fn(&App) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        self.settle();
        while !done(&self.app) {
            if Instant::now() > deadline {
                eprintln!("tour: still waiting after 20 s; shooting anyway");
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
            self.settle();
        }
    }

    /// Lets `time` pass as a window would — tasks run, timers fire, frames are
    /// drawn — for something that arrives on its own clock, like the OS's
    /// report of a file changed.
    fn wait(&mut self, time: Duration) {
        let until = Instant::now() + time;
        while Instant::now() < until {
            self.pump();
            self.timers();
            if self.app.transitioning() {
                self.send(Message::Tick(Instant::now()));
                let _ = self.render();
            }
            std::thread::sleep(Duration::from_millis(16));
        }
    }

    /// Makes the window `size`, as dragging its corner does: the tree is laid
    /// out at the new size, and the board hears of it from its own bounds.
    fn resize(&mut self, size: Size) {
        self.viewport = size;
        self.cache = None;
        self.cursor = Point::new(size.width / 2.0, size.height / 2.0);
        self.feed(vec![Event::Window(window::Event::Resized(size))]);
        self.redraw();
    }

    /// The redraw the runtime sends before each frame.
    fn redraw(&mut self) {
        self.feed(vec![Event::Window(window::Event::RedrawRequested(Instant::now()))]);
    }

    /// Saves the current frame as `NN-name.png`.
    ///
    /// Drawn twice. A bitmap over 2 MB — a large card's text — is uploaded by
    /// iced_wgpu on a worker thread and left out of the frame that asked for
    /// it; a window gets a redraw when the upload lands, and this gets one
    /// [`UPLOAD_GRACE`] later instead.
    fn shot(&mut self, name: &str) {
        self.settle();
        self.redraw();
        let _ = self.render();
        std::thread::sleep(UPLOAD_GRACE);
        let (size, rgba) = self.render();
        self.shots += 1;
        let path = self.out.join(format!("{:02}-{name}.png", self.shots));
        ::image::RgbaImage::from_raw(size.width, size.height, rgba)
            .expect("a full frame")
            .save(&path)
            .expect("write the shot");
        println!("tour: {}", path.display());
    }

    /// Lays out and draws the tree, without reading it back.
    ///
    /// The tree that draws is first sent a redraw, as the runtime does every
    /// frame — iced 0.14's button learns its status from that event and keeps
    /// it on the widget rather than in the tree, so a freshly built one that
    /// hasn't seen it draws as `Disabled`: every menu item, dimmed.
    fn draw(&mut self) {
        let theme = self.theme();
        let style = startup::style(&self.app, &theme);
        let cursor = mouse::Cursor::Available(self.cursor);
        let Self { app, renderer, cache, viewport, settings, .. } = self;
        let mut ui = UserInterface::build(root(app, *settings), *viewport, cache.take().unwrap_or_default(), renderer);
        let redraw = Event::Window(window::Event::RedrawRequested(Instant::now()));
        let _ = ui.update(&[redraw], cursor, renderer, &mut clipboard::Null, &mut Vec::new());
        ui.draw(renderer, &theme, &renderer::Style { text_color: style.text_color }, cursor);
        *cache = Some(ui.into_cache());
    }

    /// The theme the toured window is drawn in, as the daemon's `theme` picks it.
    fn theme(&self) -> iced::Theme {
        if self.settings { startup::settings_theme(&self.app) } else { startup::theme(&self.app) }
    }

    /// Tours the settings window — the form as the tray opens it — rather
    /// than the board's, until the scene ends.
    fn settings_window(&mut self) {
        let _ = self.app.open_settings_form();
        self.app.settings_window = Some(window::Id::unique());
        self.settings = true;
        self.cache = None;
        self.draw();
    }

    /// Draws the tree offscreen and reads the frame back as RGBA.
    fn render(&mut self) -> (iced::Size<u32>, Vec<u8>) {
        // Whatever a redraw publishes, `shot`'s own `redraw` already delivered.
        self.draw();
        let style = startup::style(&self.app, &self.theme());
        let size = iced::Size::new(
            (self.viewport.width * self.scale).round() as u32,
            (self.viewport.height * self.scale).round() as u32,
        );
        let mut rgba = Headless::screenshot(&mut self.renderer, size, self.scale, style.background_color);
        // The overlay is see-through: what's under it is part of the picture.
        if style.background_color.a < 1.0 {
            over_a_desktop(&mut rgba, size.width, size.height);
        }
        (size, rgba)
    }

    // --- Strips: one region, frame by frame ---

    /// Starts a strip of `region` (screen coordinates), fixed for its length
    /// so its frames line up.
    fn strip(&self, name: &str, region: Rectangle) -> Strip {
        Strip { name: name.to_owned(), region, frames: Vec::new(), legend: Vec::new() }
    }

    /// Adds the frame a window would show next: rendered once, straight after
    /// the last event settled. Deliberately *not* a [`Tour::shot`] — that
    /// redraws first, and a value that only lands on the frame after this one
    /// is exactly what a strip is for catching.
    fn frame(&mut self, strip: &mut Strip, label: &str) {
        let (size, rgba) = self.render();
        let full = ::image::RgbaImage::from_raw(size.width, size.height, rgba).expect("a full frame");
        let r = strip.region;
        let (x, y) = ((r.x * self.scale).max(0.0) as u32, (r.y * self.scale).max(0.0) as u32);
        let (w, h) = (
            ((r.width * self.scale) as u32).min(size.width - x),
            ((r.height * self.scale) as u32).min(size.height - y),
        );
        let mut tile = ::image::imageops::crop_imm(&full, x, y, w, h).to_image();
        // A whole window per frame makes a sheet too big to read; halve it.
        if tile.width() > 1000 {
            let (tw, th) = (tile.width() / 2, tile.height() / 2);
            tile = ::image::imageops::resize(&tile, tw, th, ::image::imageops::FilterType::Triangle);
        }
        let n = strip.frames.len() + 1;
        stamp_number(&mut tile, n);
        strip.frames.push(tile);
        strip.legend.push(format!("{n:>3}  {label:<28} {}", self.editing_state()));
    }

    /// Frames of an animation as it plays, a frame every ~40 ms until it ends.
    /// For a transition started with [`Tour::send`] — anything that settles
    /// would play it out before the first frame.
    fn film(&mut self, strip: &mut Strip, label: &str) {
        let start = Instant::now();
        while self.app.transitioning() && start.elapsed() < Duration::from_secs(3) {
            self.pump();
            self.send(Message::Tick(Instant::now()));
            self.frame(strip, &format!("{label} +{} ms", start.elapsed().as_millis()));
            std::thread::sleep(Duration::from_millis(40));
        }
        self.settle();
        self.frame(strip, &format!("{label}: settled"));
    }

    /// Frames of whatever `going` says is under way, one every `every` — for a
    /// process with pauses in it, like an answer arriving a word at a time,
    /// which [`Tour::film`] would call finished between two words.
    fn film_while(&mut self, strip: &mut Strip, label: &str, every: Duration, going: impl Fn(&App) -> bool) {
        let start = Instant::now();
        while going(&self.app) && start.elapsed() < Duration::from_secs(10) {
            let next = Instant::now() + every;
            while Instant::now() < next {
                self.pump();
                self.timers();
                if self.app.transitioning() {
                    self.send(Message::Tick(Instant::now()));
                    let _ = self.render();
                }
                std::thread::sleep(Duration::from_millis(8));
            }
            self.frame(strip, &format!("{label} +{} ms", start.elapsed().as_millis()));
        }
        self.settle();
        self.frame(strip, &format!("{label}: settled"));
    }

    /// The numbers a strip's legend carries per frame: what the card, the
    /// editor box and the editor's own content say about the text's size.
    fn editing_state(&self) -> String {
        let Some(editing) = &self.app.editing else {
            let sizes: Vec<String> = self
                .app
                .nodes
                .iter()
                .map(|node| format!("{:.0}x{:.0}", node.width, node.height))
                .collect();
            return format!("reading   cards {}", sizes.join(" "));
        };
        let node = &self.app.nodes[editing.index];
        let text = editing.content.text();
        let cursor = editing.content.cursor().position;
        let editor = self
            .app
            .editor_box(editing.index)
            .map(|b| format!("{:.0}x{:.0}{}", b.width, b.height, if b.centered { " centred" } else { "" }))
            .unwrap_or_default();
        let tail: String = text.chars().rev().take(18).collect::<Vec<_>>().into_iter().rev().collect();
        format!(
            "editing   card {:.0}x{:.0}  editor {editor}  lines {}  caret {}:{}  …{tail:?}",
            node.width,
            node.height,
            editing.content.line_count(),
            cursor.line,
            cursor.column,
        )
    }

    /// Saves a strip as one numbered sheet, `columns` wide, with its legend
    /// beside it as text.
    fn save(&mut self, strip: Strip, columns: usize) {
        const GUTTER: u32 = 6;
        let Some(first) = strip.frames.first() else {
            return;
        };
        let (w, h) = first.dimensions();
        let rows = strip.frames.len().div_ceil(columns) as u32;
        let columns = columns.min(strip.frames.len()) as u32;
        let mut sheet = ::image::RgbaImage::from_pixel(
            columns * (w + GUTTER) + GUTTER,
            rows * (h + GUTTER) + GUTTER,
            ::image::Rgba([60, 60, 66, 255]),
        );
        for (i, frame) in strip.frames.iter().enumerate() {
            let (col, row) = (i as u32 % columns, i as u32 / columns);
            ::image::imageops::overlay(
                &mut sheet,
                frame,
                i64::from(GUTTER + col * (w + GUTTER)),
                i64::from(GUTTER + row * (h + GUTTER)),
            );
        }
        self.shots += 1;
        let base = self.out.join(format!("{:02}-{}", self.shots, strip.name));
        sheet.save(base.with_extension("png")).expect("write the sheet");
        std::fs::write(base.with_extension("txt"), strip.legend.join("\n") + "\n").expect("write the legend");
        println!("tour: {}.png (+ .txt)", base.display());
    }

    // --- What a person does ---

    fn move_to(&mut self, at: Point) {
        self.cursor = at;
        self.feed(vec![Event::Mouse(mouse::Event::CursorMoved { position: at })]);
    }

    fn press(&mut self, button: mouse::Button) {
        self.feed(vec![Event::Mouse(mouse::Event::ButtonPressed(button))]);
    }

    fn release(&mut self, button: mouse::Button) {
        self.feed(vec![Event::Mouse(mouse::Event::ButtonReleased(button))]);
    }

    fn click(&mut self, at: Point) {
        self.move_to(at);
        self.press(mouse::Button::Left);
        self.release(mouse::Button::Left);
    }

    fn double_click(&mut self, at: Point) {
        self.click(at);
        self.click(at);
    }

    fn right_click(&mut self, at: Point) {
        self.move_to(at);
        self.press(mouse::Button::Right);
        self.release(mouse::Button::Right);
    }

    /// A left-button drag in `steps` moves, the way a hand makes one.
    fn drag(&mut self, from: Point, to: Point) {
        const STEPS: usize = 12;
        self.move_to(from);
        self.press(mouse::Button::Left);
        for step in 1..=STEPS {
            let t = step as f32 / STEPS as f32;
            self.move_to(Point::new(from.x + (to.x - from.x) * t, from.y + (to.y - from.y) * t));
        }
        self.release(mouse::Button::Left);
    }

    fn hold(&mut self, modifiers: Modifiers) {
        self.modifiers = modifiers;
        self.feed(vec![Event::Keyboard(keyboard::Event::ModifiersChanged(modifiers))]);
    }

    /// One key, pressed and released under whatever modifiers are held.
    fn key(&mut self, key: Key, text: Option<&str>) {
        let physical = key::Physical::Unidentified(key::NativeCode::Unidentified);
        self.feed(vec![
            Event::Keyboard(keyboard::Event::KeyPressed {
                key: key.clone(),
                modified_key: key.clone(),
                physical_key: physical,
                location: keyboard::Location::Standard,
                modifiers: self.modifiers,
                text: text.map(Into::into),
                repeat: false,
            }),
            Event::Keyboard(keyboard::Event::KeyReleased {
                key: key.clone(),
                modified_key: key,
                physical_key: physical,
                location: keyboard::Location::Standard,
                modifiers: self.modifiers,
            }),
        ]);
    }

    fn named(&mut self, named: key::Named) {
        self.key(Key::Named(named), None);
    }

    /// A shortcut: `letter` under `modifiers`, which are let go afterwards.
    fn chord(&mut self, modifiers: Modifiers, letter: &str) {
        self.hold(modifiers);
        self.key(Key::Character(letter.into()), None);
        self.hold(Modifiers::empty());
    }

    /// A mouse wheel's notches over `at` — on the board, a zoom.
    fn wheel(&mut self, at: Point, lines: f32) {
        self.move_to(at);
        self.feed(vec![Event::Mouse(mouse::Event::WheelScrolled {
            delta: mouse::ScrollDelta::Lines { x: 0.0, y: lines },
        })]);
    }

    /// A trackpad's two-finger scroll over `at` — on the board, a pan; over a
    /// panel, a scroll.
    fn swipe(&mut self, at: Point, dx: f32, dy: f32) {
        self.move_to(at);
        self.feed(vec![Event::Mouse(mouse::Event::WheelScrolled {
            delta: mouse::ScrollDelta::Pixels { x: dx, y: dy },
        })]);
    }

    /// Runs a command the way a person does: the palette, a few letters of its
    /// name, Return.
    fn palette(&mut self, query: &str) {
        self.chord(Modifiers::COMMAND | Modifiers::SHIFT, "p");
        self.type_text(query);
        self.named(key::Named::Enter);
    }

    /// Types `text` a key at a time; `\n` is Return.
    fn type_text(&mut self, text: &str) {
        for c in text.chars() {
            if c == '\n' {
                self.key(Key::Named(key::Named::Enter), Some("\n"));
            } else {
                let s = c.to_string();
                self.key(Key::Character(s.as_str().into()), Some(&s));
            }
        }
    }

    // --- Where things are ---

    /// The centre of the text `label` as it's laid out now — a menu item, a
    /// button, a panel row — found by asking the tree, the way an
    /// accessibility tool would, rather than by arithmetic over a menu that
    /// moves whenever it gains an item. The last match wins, since what's
    /// drawn last is what's on top.
    fn label(&mut self, label: &str) -> Point {
        struct Find<'a> {
            label: &'a str,
            found: Option<Rectangle>,
            /// How far the scrollables around the current widget are scrolled.
            /// A scrollable lays its content out unscrolled and reports the
            /// offset separately, just before visiting it — so a label in a
            /// scrolled form is found where it would be at the top.
            scrolled: Vector,
            /// The offset a scrollable just reported, for the visit after it.
            next: Vector,
        }
        impl Operation for Find<'_> {
            fn scrollable(
                &mut self,
                _id: Option<&iced::widget::Id>,
                _bounds: Rectangle,
                _content: Rectangle,
                translation: Vector,
                _state: &mut dyn iced_runtime::core::widget::operation::Scrollable,
            ) {
                self.next = translation;
            }
            fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
                let outer = self.scrolled;
                self.scrolled += std::mem::take(&mut self.next);
                operate(self);
                self.scrolled = outer;
            }
            fn text(&mut self, _id: Option<&iced::widget::Id>, bounds: Rectangle, text: &str) {
                if text == self.label {
                    self.found = Some(bounds - self.scrolled);
                }
            }
        }
        let mut find = Find { label, found: None, scrolled: Vector::ZERO, next: Vector::ZERO };
        let Self { app, renderer, cache, viewport, settings, .. } = self;
        let mut ui = UserInterface::build(root(app, *settings), *viewport, cache.take().unwrap_or_default(), renderer);
        ui.operate(renderer, &mut find);
        *cache = Some(ui.into_cache());
        find.found.unwrap_or_else(|| panic!("nothing on screen reads {label:?}")).center()
    }

    /// Clicks whatever reads `label`.
    fn click_label(&mut self, label: &str) {
        let at = self.label(label);
        self.click(at);
    }

    /// Clicks whatever reads `label` and returns with what the click started
    /// still going — for filming it. A [`Tour::click`] settles, and an answer
    /// that has settled has finished arriving.
    fn start(&mut self, label: &str) {
        let at = self.label(label);
        self.move_to(at);
        self.press(mouse::Button::Left);
        self.deliver(vec![Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))]);
        self.draw();
    }

    /// The on-screen centre of node `index`.
    fn node_center(&self, index: usize) -> Point {
        let node = &self.app.nodes[index];
        self.app.camera.to_screen(Point::new(node.x + node.width / 2.0, node.y + node.height / 2.0))
    }

    /// The node whose text starts with `prefix`.
    fn find(&self, prefix: &str) -> usize {
        self.app
            .nodes
            .iter()
            .position(|node| node.text.as_deref().is_some_and(|text| text.starts_with(prefix)))
            .unwrap_or_else(|| panic!("no node starting {prefix:?}"))
    }
}

/// An operation that visits nothing: building the tree to run it is the point.
struct Noop;

impl iced_runtime::core::widget::Operation for Noop {
    fn traverse(&mut self, _operate: &mut dyn FnMut(&mut dyn iced_runtime::core::widget::Operation)) {}
}

/// Puts a translucent frame (premultiplied RGBA, as read back) over a stand-in
/// desktop — see [`desktop`].
fn over_a_desktop(rgba: &mut [u8], width: u32, height: u32) {
    let under = desktop(width, height);
    for (pixel, below) in rgba.chunks_exact_mut(4).zip(under.chunks_exact(3)) {
        let clear = 255 - u16::from(pixel[3]);
        for c in 0..3 {
            pixel[c] = (u16::from(pixel[c]) + u16::from(below[c]) * clear / 255).min(255) as u8;
        }
        pixel[3] = 255;
    }
}

/// A stand-in desktop, RGB: a dusk gradient and a light document window —
/// the case a dim has to work hardest for, light text-heavy content right
/// under the board. Drawn once per size; a film composites every frame.
fn desktop(width: u32, height: u32) -> std::sync::Arc<Vec<u8>> {
    /// The last desktop drawn, and the size it was drawn at.
    type Drawn = ((u32, u32), std::sync::Arc<Vec<u8>>);
    static CACHE: std::sync::Mutex<Option<Drawn>> = std::sync::Mutex::new(None);
    let mut cache = CACHE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some((size, pixels)) = cache.as_ref()
        && *size == (width, height)
    {
        return pixels.clone();
    }
    let window = (width as f32 * 0.08, height as f32 * 0.12, width as f32 * 0.55, height as f32 * 0.7);
    let line = 22.0 * (width as f32 / VIEWPORT.width);
    let mut pixels = Vec::with_capacity((width * height * 3) as usize);
    for y in 0..height {
        for x in 0..width {
            let (fx, fy) = (x as f32, y as f32);
            let t = (fx / width as f32 + fy / height as f32) / 2.0;
            let mut rgb = [
                (58.0 + (230.0 - 58.0) * t) as u8,
                (96.0 + (120.0 - 96.0) * t) as u8,
                (140.0 + (90.0 - 140.0) * t) as u8,
            ];
            let (wx, wy) = (fx - window.0, fy - window.1);
            if wx >= 0.0 && wy >= 0.0 && wx < window.2 && wy < window.3 {
                let row = (wy / line) as u32;
                let in_line = (wy % line) / line;
                let ends = window.2 * (0.55 + 0.4 * ((row * 37 % 11) as f32 / 11.0));
                // A title bar, then lines of "text".
                rgb = if wy < line * 1.3 {
                    [226, 226, 224]
                } else if (0.36..0.64).contains(&in_line) && wx > line && wx < ends {
                    [60, 60, 66]
                } else {
                    [246, 246, 244]
                };
            }
            pixels.extend_from_slice(&rgb);
        }
    }
    let pixels = std::sync::Arc::new(pixels);
    *cache = Some(((width, height), pixels.clone()));
    pixels
}

/// One region of the screen over a run of steps — see [`Tour::strip`].
struct Strip {
    name: String,
    region: Rectangle,
    frames: Vec<::image::RgbaImage>,
    legend: Vec<String>,
}

/// Stamps `n` into a tile's top-left corner in a 3×5 pixel face, so a sheet's
/// frames can be told apart and found in its legend. Nothing in the
/// dependency tree draws text into an image, and ten digits don't need it to.
fn stamp_number(tile: &mut ::image::RgbaImage, n: usize) {
    const DIGITS: [u16; 10] = [
        0b111_101_101_101_111, 0b010_110_010_010_111, 0b111_001_111_100_111,
        0b111_001_111_001_111, 0b101_101_111_001_001, 0b111_100_111_001_111,
        0b111_100_111_101_111, 0b111_001_010_010_010, 0b111_101_111_101_111,
        0b111_101_111_001_111,
    ];
    const PX: u32 = 3;
    let digits: Vec<u16> = n.to_string().bytes().map(|b| DIGITS[usize::from(b - b'0')]).collect();
    let (width, height) = (digits.len() as u32 * 4 * PX + PX, 7 * PX);
    for y in 0..height.min(tile.height()) {
        for x in 0..width.min(tile.width()) {
            tile.put_pixel(x, y, ::image::Rgba([0, 0, 0, 255]));
        }
    }
    for (i, glyph) in digits.iter().enumerate() {
        for bit in 0..15u32 {
            if glyph & (1 << (14 - bit)) == 0 {
                continue;
            }
            let (gx, gy) = (PX + i as u32 * 4 * PX + (bit % 3) * PX, PX + (bit / 3) * PX);
            for dy in 0..PX {
                for dx in 0..PX {
                    if gx + dx < tile.width() && gy + dy < tile.height() {
                        tile.put_pixel(gx + dx, gy + dy, ::image::Rgba([255, 214, 0, 255]));
                    }
                }
            }
        }
    }
}

/// Polls a future that native wgpu resolves without ever really waiting.
fn block_on<F: std::future::Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    let mut context = Context::from_waker(Waker::noop());
    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut context) {
            return output;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// An OpenAI-compatible endpoint on a loopback port, answering from a
/// [`Script`].
///
/// The AI scenes need something to ask, and it has to be the real client that
/// asks it: the streamed `data:` lines, the `---` that splits an answer into
/// cards, the sentence a refused request turns into — those are what a person
/// sees, and a message sent straight to `update` would skip every one. Nothing
/// leaves the machine and no key is needed.
struct Provider {
    /// The base URL to configure, `/v1` included.
    url: String,
}

/// What a [`Provider`] says.
struct Script {
    /// What `GET /models` lists: id, display name, context window. A request
    /// for any other model is refused with a 404, as OpenAI refuses one.
    models: &'static [(&'static str, Option<&'static str>, Option<u64>)],
    /// Streamed answers, each under a cue: a request gets the one whose cue
    /// comes latest in its prompt, which ends with the card asked about.
    answers: &'static [(&'static str, &'static str)],
    /// Refuse every request as an unrecognised key.
    reject_key: bool,
}

/// A local model's pause before its first word, long enough to see the board
/// waiting on it.
const FIRST_WORD: Duration = Duration::from_millis(1200);
/// Between words, about a fast local model's pace.
const NEXT_WORD: Duration = Duration::from_millis(35);
/// How long the model list takes to come back.
const LOOKUP: Duration = Duration::from_millis(700);

impl Provider {
    fn start(script: &'static Script) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let url = format!("http://{}/v1", listener.local_addr().expect("its address"));
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                std::thread::spawn(move || serve(stream, script));
            }
        });
        Self { url }
    }

    /// A URL nothing answers at — a local runtime that isn't running.
    fn absent() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        format!("http://{}/v1", listener.local_addr().expect("its address"))
    }
}

/// Answers one request on `stream`, then closes it.
fn serve(mut stream: TcpStream, script: &Script) {
    let Some((head, body)) = read_request(&mut stream) else {
        return;
    };
    if script.reject_key {
        let error = "Incorrect API key provided. You can find your API key in your account settings.";
        return respond(&mut stream, "401 Unauthorized", &serde_json::json!({ "error": { "message": error, "code": "invalid_api_key" } }));
    }
    if head.split_whitespace().nth(1).is_some_and(|path| path.ends_with("/models")) {
        std::thread::sleep(LOOKUP);
        let data: Vec<serde_json::Value> = script
            .models
            .iter()
            .map(|(id, name, context)| serde_json::json!({ "id": id, "name": name, "context_length": context }))
            .collect();
        return respond(&mut stream, "200 OK", &serde_json::json!({ "data": data }));
    }
    let request: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
    let model = request["model"].as_str().unwrap_or_default();
    if !script.models.iter().any(|(id, ..)| *id == model) {
        let error = format!("The model `{model}` does not exist or you do not have access to it.");
        return respond(&mut stream, "404 Not Found", &serde_json::json!({ "error": { "message": error, "code": "model_not_found" } }));
    }
    let prompt = request.pointer("/messages/1/content").and_then(|c| c.as_str()).unwrap_or_default();
    // Server-sent events, a word to an event; no length, so the body ends
    // where the connection does.
    let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n";
    if stream.write_all(head.as_bytes()).is_err() {
        return;
    }
    std::thread::sleep(FIRST_WORD);
    for word in cued(script.answers, prompt).split_inclusive(' ') {
        let event = serde_json::json!({ "choices": [{ "delta": { "content": word } }] });
        if stream.write_all(format!("data: {event}\n\n").as_bytes()).is_err() {
            return;
        }
        std::thread::sleep(NEXT_WORD);
    }
    let _ = stream.write_all(b"data: [DONE]\n\n");
}

/// The reply under whichever of `table`'s cues comes latest in `prompt`.
fn cued(table: &'static [(&'static str, &'static str)], prompt: &str) -> &'static str {
    table
        .iter()
        .filter_map(|(cue, reply)| prompt.rfind(cue).map(|at| (at, *reply)))
        .max_by_key(|(at, _)| *at)
        .map_or("", |(_, reply)| reply)
}

/// A request's head and body, read as far as its `Content-Length` says.
fn read_request(stream: &mut TcpStream) -> Option<(String, Vec<u8>)> {
    let mut buffer = Vec::new();
    let mut chunk = [0; 4096];
    let end = loop {
        if let Some(at) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            break at + 4;
        }
        let read = stream.read(&mut chunk).ok().filter(|&n| n > 0)?;
        buffer.extend_from_slice(&chunk[..read]);
    };
    let head = String::from_utf8_lossy(&buffer[..end]).into_owned();
    let length = head
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.trim().parse().ok())
        .unwrap_or(0);
    let mut body = buffer.split_off(end);
    while body.len() < length {
        let read = stream.read(&mut chunk).ok().filter(|&n| n > 0)?;
        body.extend_from_slice(&chunk[..read]);
    }
    Some((head, body))
}

fn respond(stream: &mut TcpStream, status: &str, body: &serde_json::Value) {
    let body = body.to_string();
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len(),
    );
}

/// An app with the board at `path` open, as a launch opens a configured one:
/// read from the file, saved back to it, and watched — none of which the
/// scratch board is.
fn file_launch(path: &std::path::Path) -> App {
    let document = CanvasDocument::load(path).expect("the board");
    let mut app = launch(|_| document, true, path.parent().expect("its folder").to_owned());
    app.canvas_path = path.to_owned();
    app.disk = sync::OnDisk::new(app.current_document(), path);
    app.scratch = false;
    app
}

/// An app as a first launch builds it — default config, the theme that config
/// resolves to, nothing written to disk — showing the board `document` makes
/// for that theme. Resolved in `App::new`'s order: the theme's fonts before
/// `use_font` reads the database.
fn first_launch(document: impl FnOnce(&ColorTheme) -> CanvasDocument) -> App {
    launch(document, true, PathBuf::from("."))
}

/// [`first_launch`] as the overlay — the app's own mode, summoned over the
/// desktop rather than opened in a window.
///
/// Marked as already set up: the first frame of a real overlay restyles its
/// window and creates the menu-bar icon, both of which want a real window on
/// the main thread, and neither of which shows in a frame.
fn overlay_launch(document: impl FnOnce(&ColorTheme) -> CanvasDocument) -> App {
    let mut app = launch(document, false, PathBuf::from("."));
    app.overlay_configured = true;
    app
}

/// An app built from a default config showing `document`, windowed or as the
/// overlay, with file nodes resolved against `base`.
fn launch(document: impl FnOnce(&ColorTheme) -> CanvasDocument, windowed: bool, base: PathBuf) -> App {
    let mut config = config::Config::default();
    // A theme of the caller's, for looking at the app as a particular user
    // has it: `TOUR_THEME` names a built-in one, `TOUR_THEME_CSS` points at an
    // Obsidian stylesheet the way a vault's does.
    if let Ok(name) = std::env::var("TOUR_THEME") {
        config.theme = name;
    }
    if let Ok(path) = std::env::var("TOUR_THEME_CSS") {
        config.theme_css = Some(path);
    }
    let (mut theme, embedded_fonts) = theme_store::resolve_base(&config);
    if !embedded_fonts.is_empty() {
        text_raster::set_embedded_fonts(embedded_fonts);
    }
    theme.use_font(&config.font);
    text_raster::warm_up();
    let document = document(&theme);
    App::build(Init {
        config,
        theme,
        document,
        canvas_path: PathBuf::new(),
        base_path: base,
        scratch: true,
        hotkey_manager: None,
        registered_hotkey: None,
        windowed,
    })
}

fn empty_document(_: &ColorTheme) -> CanvasDocument {
    CanvasDocument { nodes: Vec::new(), edges: Vec::new(), extra: serde_json::Map::new() }
}

/// Panned off into empty space: a marker on the window's edge toward each
/// group of cards — the nearest named, the rest smaller and fainter — the
/// name of another while it's hovered, and a click on it that flies there.
fn offscreen() {
    let mut tour = Tour::new("offscreen", first_launch(sample_board));
    // Past the board's top-right corner — far enough at the fitted zoom the
    // board opens at that none of it is in view: groups to the left and
    // below, near and far.
    tour.send(Message::FocusWorld(Point::new(4600.0, -2000.0)));
    tour.shot("past-the-corner");
    let points = tour.app.board(tour.app.camera).marker_points();
    assert!(points.len() > 1, "a marker for more than one group");
    // The farthest's name, on hover.
    let far = *points.last().expect("markers");
    tour.move_to(far);
    tour.shot("hovering-a-far-one");
    // A sweep along the edge, across dots and the gaps between them: a chip
    // comes and goes with each dot, and the nearest group's name holds still
    // under it — hidden while any dot was hovered, it blinked with each.
    let region = Rectangle::new(Point::new(0.0, 760.0), Size::new(560.0, 140.0));
    let mut sweep = tour.strip("sweep", region);
    let start = points[0];
    for step in 0..10 {
        tour.move_to(Point::new(start.x + step as f32 * 8.0, start.y));
        tour.frame(&mut sweep, &format!("x + {}", step * 8));
    }
    tour.save(sweep, 2);
    tour.click(far);
    tour.shot("flown-there");
    // Below the board, looking up at all of it.
    tour.send(Message::FocusWorld(Point::new(900.0, 5200.0)));
    tour.shot("below");
}

/// A board with some life in it: eight topics, each a card linked to a
/// handful of its own, laid out in blocks — enough to cluster when zoomed out
/// and to have something on every side when panned.
fn sample_board(_: &ColorTheme) -> CanvasDocument {
    use thinkboard_core::fixtures::{edge, node};
    const TOPICS: &[(&str, &[&str])] = &[
        ("# Trip to Lisbon", &["Flights\n\n- TAP, Fri evening\n- back Mon night", "Stay in **Alfama**", "Pastéis de nata at Manteigaria", "Day trip to *Sintra*", "Budget: ~€900"]),
        ("# Garden", &["Tomatoes: start indoors in March", "Fix the **fence** before spring", "Compost bin", "Ask about the apple tree"]),
        ("# Reading list", &["*The Overstory*", "*Piranesi* — reread", "Designing Data-Intensive Applications", "*A Swim in a Pond in the Rain*", "Poetry: Mary Oliver"]),
        ("# Q4 plan", &["Ship the **sync** rewrite", "Hire: two engineers", "Offsite in November", "OKR: p95 latency < 200 ms", "Retire the old dashboard"]),
        ("# Home renovation", &["Kitchen: quotes from 3 builders", "Paint the hallway — *sage green*?", "Bathroom tiles", "Budget and timeline"]),
        ("# Learning Rust", &["Finish the **ownership** chapter", "Write a CLI tool", "Read about `async`", "Advent of Code in Rust"]),
        ("# Birthday party", &["Guest list (~20)", "Cake: lemon & olive oil", "Playlist", "Venue: the back garden"]),
        ("# Running", &["10k in under 50 min", "Long run on Sundays", "New shoes", "Half marathon in April?"]),
    ];
    let (mut nodes, mut edges) = (Vec::new(), Vec::new());
    for (t, (root, children)) in TOPICS.iter().enumerate() {
        let (bx, by) = ((t % 2) as f32 * 1100.0, (t / 2) as f32 * 760.0);
        let id = format!("t{t}");
        nodes.push(CanvasNode { text: Some((*root).to_owned()), x: bx, y: by + 200.0, width: 260.0, height: 80.0, ..node(&id) });
        for (c, text) in children.iter().enumerate() {
            let child = format!("t{t}c{c}");
            nodes.push(CanvasNode {
                text: Some((*text).to_owned()),
                x: bx + 420.0,
                y: by + c as f32 * 120.0,
                width: 280.0,
                height: if text.contains('\n') { 110.0 } else { 70.0 },
                ..node(&child)
            });
            edges.push(edge(&id, &child));
        }
    }
    CanvasDocument { nodes, edges, extra: serde_json::Map::new() }
}

/// One board with a bit of everything a theme colours: heading, bold,
/// italic, code (inline and a block), a link, a list with tasks, a quote, a
/// labelled link between cards, every card colour, and a named frame.
fn theme_sampler(_: &ColorTheme) -> CanvasDocument {
    use thinkboard_core::fixtures::{edge, node};
    let text = |id: &str, text: &str, (x, y, w, h): (f32, f32, f32, f32)| CanvasNode {
        text: Some(text.to_owned()),
        x,
        y,
        width: w,
        height: h,
        ..node(id)
    };
    let mut nodes = vec![
        text("intro", "# Theme sampler\n\nBody text with **bold**, *italic*, `inline code` and a [link](https://example.com).", (0.0, 0.0, 400.0, 170.0)),
        text("lists", "## Lists\n\n- an item\n- [ ] a task\n- [x] a done task\n\n> a quote", (520.0, -60.0, 300.0, 230.0)),
        text("code", "```rust\nfn main() {\n    println!(\"hi\");\n}\n```", (520.0, 220.0, 300.0, 130.0)),
    ];
    let names = ["Red", "Orange", "Yellow", "Green", "Cyan", "Purple"];
    for (i, name) in names.iter().enumerate() {
        nodes.push(CanvasNode {
            color: Some((i + 1).to_string()),
            ..text(&format!("c{i}"), name, (i as f32 * 140.0 - 20.0, 300.0, 120.0, 60.0))
        });
    }
    let mut frame = node("frame");
    frame.kind = "group".to_owned();
    frame.text = None;
    frame.x = -50.0;
    frame.y = 260.0;
    frame.width = 880.0;
    frame.height = 130.0;
    frame.set_label("Colors");
    nodes.insert(0, frame);
    let mut labelled = edge("intro", "lists");
    labelled.set_label("links");
    CanvasDocument { nodes, edges: vec![labelled, edge("intro", "code")], extra: serde_json::Map::new() }
}

// --- Scenes ---

/// Launch with nothing configured: the welcome board, then the first things a
/// newcomer tries on it.
fn first_run() {
    let mut tour = Tour::new("first_run", first_launch(canvas_file::scratch_document));
    tour.shot("welcome");

    let tip = tour.find("**Double-click**");
    tour.move_to(tour.node_center(tip));
    tour.shot("hover-a-tip");

    tour.right_click(Point::new(1200.0, 780.0));
    tour.shot("context-menu-empty");
    tour.named(key::Named::Escape);

    // The chord the welcome board names first.
    tour.chord(Modifiers::COMMAND, "k");
    tour.shot("command-palette");
    tour.type_text("theme");
    tour.shot("command-palette-filtered");
    tour.named(key::Named::Escape);
    tour.shot("after-palette");
}

/// A newcomer's first board: empty, then built up by hand — cards, an edit, a
/// link dragged from a port, a second link, a frame around a pair.
fn build_a_board() {
    let mut tour = Tour::new("build_a_board", first_launch(empty_document));
    tour.shot("empty-board");

    tour.chord(Modifiers::COMMAND | Modifiers::SHIFT, "p");
    tour.shot("palette-nothing-selected");
    tour.named(key::Named::Escape);

    // Where most people try first.
    tour.double_click(Point::new(560.0, 380.0));
    tour.shot("double-click-empty");

    tour.right_click(Point::new(560.0, 380.0));
    tour.shot("context-menu");
    // Whatever the menu calls it, adding a card is a `CreateNode` at the click.
    let world = tour.app.camera.to_world(Point::new(560.0, 380.0));
    tour.send(Message::CloseContextMenu);
    tour.send(Message::CreateNode { world });
    tour.shot("new-note");

    tour.type_text("# Trip to Lisbon\n\nPlan the **long weekend** in May.");
    tour.shot("typing");
    tour.named(key::Named::Escape);
    tour.shot("first-note");

    // Drag out of the card's right-hand port onto empty space.
    let first = 0;
    let node = &tour.app.nodes[first];
    let port = tour.app.camera.to_screen(Point::new(node.x + node.width, node.y + node.height / 2.0));
    tour.move_to(Point::new(port.x - 10.0, port.y));
    tour.move_to(port);
    tour.shot("hover-port");
    tour.drag(port, Point::new(port.x + 260.0, port.y - 120.0));
    tour.shot("dragged-link");
    tour.type_text("Flights\n\n- TAP, Fri evening\nback Mon night");
    tour.named(key::Named::Escape);

    tour.drag(port, Point::new(port.x + 260.0, port.y + 140.0));
    tour.type_text("Where to stay\n\nAlfama or *Príncipe Real*?");
    tour.named(key::Named::Escape);
    tour.shot("three-notes");

    tour.click(tour.node_center(1));
    tour.hold(Modifiers::COMMAND);
    tour.click(tour.node_center(2));
    tour.hold(Modifiers::empty());
    tour.shot("two-selected");
    tour.chord(Modifiers::COMMAND | Modifiers::SHIFT, "p");
    tour.shot("palette-two-selected");
    tour.named(key::Named::Escape);
    tour.right_click(tour.node_center(2));
    tour.shot("node-menu");
    tour.named(key::Named::Escape);

    tour.click(Point::new(200.0, 800.0));
    tour.shot("deselected");
}

/// The opt-in Obsidian gesture: double-click empty space for a card. A window
/// is never dismissed by a click, so here it works with the setting alone.
fn double_click_to_add() {
    let mut tour = Tour::new("double_click_to_add", first_launch(empty_document));
    tour.app.config.double_click_adds_card = true;
    tour.shot("empty-board-hint");
    tour.double_click(Point::new(720.0, 300.0));
    tour.shot("double-clicked");
    tour.type_text("Made with a double-click");
    tour.named(key::Named::Escape);
    tour.double_click(Point::new(720.0, 600.0));
    tour.type_text("And another");
    tour.named(key::Named::Escape);
    tour.shot("two-notes");

    tour.send(Message::OpenSettingsOverlay);
    tour.shot("settings");
}

/// Writing in a card, a frame per step: what the card and the editor do as
/// lines break, wrap, and outgrow the card — and what changes when editing
/// ends and starts again. Read the sheets with their `.txt` legends: the
/// numbers say which of card, editor box and content fell behind.
fn editing() {
    let mut tour = Tour::new("editing", first_launch(empty_document));

    // A card made the way the hint says: right-click, "Add card".
    let at = Point::new(560.0, 300.0);
    tour.right_click(at);
    tour.click(Point::new(at.x + 60.0, at.y + 19.0));
    let region = Rectangle::new(Point::new(at.x - 200.0, at.y - 130.0), Size::new(400.0, 260.0));

    // Line breaks: a frame after every word and every Return.
    let mut breaks = tour.strip("line-breaks", region);
    tour.frame(&mut breaks, "new note");
    // The list is typed as it is in Obsidian: the marker once, then Return
    // carries it, and Return on the empty item ends the list.
    for step in ["Packing", " list", "\n", "\n", "- passport", "\n", "charger", "\n", "\n", "Shoes?"] {
        tour.type_text(step);
        tour.frame(&mut breaks, &format!("typed {step:?}"));
    }
    tour.named(key::Named::Backspace);
    tour.frame(&mut breaks, "backspace");
    tour.named(key::Named::Escape);
    tour.frame(&mut breaks, "esc: read view");
    std::thread::sleep(UPLOAD_GRACE);
    tour.frame(&mut breaks, "esc + 250 ms");
    tour.save(breaks, 4);

    // Back in, by double-clicking a word; a heading, then out again.
    let mut reenter = tour.strip("re-enter", region);
    let card = tour.node_center(0);
    tour.double_click(Point::new(card.x, card.y - 10.0));
    tour.frame(&mut reenter, "double-click: editing");
    tour.chord(Modifiers::COMMAND, "a");
    tour.type_text("# Trip to Lisbon\n\nA long weekend in May — flights, somewhere to stay in Alfama, and a list of tascas.");
    tour.frame(&mut reenter, "replaced with heading + wrap");
    tour.named(key::Named::Escape);
    tour.frame(&mut reenter, "esc: read view");
    let card = tour.node_center(0);
    tour.double_click(card);
    tour.frame(&mut reenter, "double-click again");
    tour.named(key::Named::Escape);
    tour.save(reenter, 2);

    // A heading, a blank line and a body with bold in it, a word at a time:
    // the styling has to hold on every line as the card widens, and the last
    // frame shows how far the card jumps when the heading renders full size.
    let at = Point::new(1000.0, 680.0);
    tour.right_click(at);
    tour.click(Point::new(at.x + 60.0, at.y + 19.0));
    let mut heading = tour.strip("heading", Rectangle::new(Point::new(at.x - 260.0, at.y - 90.0), Size::new(520.0, 180.0)));
    for key in ["# Trip to Lisbon", "\n", "\n", "Plan", " the", " **long", " weekend**", " in", " May.", " And", " June."] {
        tour.type_text(key);
        tour.frame(&mut heading, &format!("typed {key:?}"));
    }
    tour.named(key::Named::Escape);
    tour.frame(&mut heading, "esc: read view");
    tour.save(heading, 4);
    tour.click(Point::new(1300.0, 850.0));

    // A long sentence a word at a time: grow, then wrap.
    let at = Point::new(980.0, 300.0);
    tour.right_click(at);
    tour.click(Point::new(at.x + 60.0, at.y + 19.0));
    let mut wrap = tour.strip("wrapping", Rectangle::new(Point::new(at.x - 230.0, at.y - 110.0), Size::new(460.0, 220.0)));
    let sentence = "Remember to check whether the museum is closed on Mondays before planning the route";
    for (i, word) in sentence.split(' ').enumerate() {
        tour.type_text(if i == 0 { word.to_owned() } else { format!(" {word}") }.as_str());
        tour.frame(&mut wrap, &format!("+{word:?}"));
    }
    tour.named(key::Named::Escape);
    tour.frame(&mut wrap, "esc: read view");
    tour.save(wrap, 4);

    // Past the auto-size ceiling: the card stops, the editor scrolls.
    let at = Point::new(560.0, 640.0);
    tour.right_click(at);
    tour.click(Point::new(at.x + 60.0, at.y + 19.0));
    let mut tall = tour.strip("overflow", Rectangle::new(Point::new(at.x - 220.0, 0.0), Size::new(440.0, 900.0)));
    for line in 1..=60 {
        tour.type_text(&format!("Line {line} of a long note\n"));
        if line % 10 == 0 {
            tour.frame(&mut tall, &format!("{line} lines"));
        }
    }
    tour.named(key::Named::Escape);
    tour.frame(&mut tall, "esc: read view");
    tour.save(tall, 6);
    tour.shot("board");
}

/// Cards over cards: one dragged across another, then left there. A card's
/// fill has to cover the text of the one beneath it — in the same renderer
/// layer it can't, and both texts showed through each other.
fn overlapping() {
    let mut tour = Tour::new("overlapping", first_launch(canvas_file::scratch_document));
    let dragged = tour.find("**Double-click**");
    let target = tour.find("Hover a card");
    let (from, to) = (tour.node_center(dragged), tour.node_center(target));
    tour.move_to(from);
    tour.press(mouse::Button::Left);
    for step in 1..=10 {
        let t = step as f32 / 10.0;
        tour.move_to(Point::new(from.x + (to.x - from.x) * t, from.y + (to.y - from.y) * t + 8.0));
    }
    tour.shot("dragging-over");
    tour.release(mouse::Button::Left);
    tour.click(Point::new(1300.0, 860.0));
    tour.shot("left-over");
}

/// The settings panel, opened from the palette and scrolled through.
fn settings() {
    let mut tour = Tour::new("settings", first_launch(canvas_file::scratch_document));
    // By its shortcut, through the board as a keypress reaches it.
    tour.chord(Modifiers::COMMAND, ",");
    tour.shot("general");
    // A changed setting is marked, and a row says what it does once it's
    // focused — here by moving its slider, as a hand would.
    tour.send(Message::Settings(settings::Msg::Slide(settings::Field::OverlayDim, 0.4)));
    tour.shot("general-changed");
    for tab in ["Appearance", "Canvas", "AI", "Advanced"] {
        tour.click_label(tab);
        tour.shot(&tab.to_lowercase());
    }
    // With a provider, the rest of the AI tab — and with "Custom" picked
    // before a URL is typed, which must still show the URL field.
    tour.click_label("AI");
    tour.send(Message::Settings(settings::Msg::PickPreset("Custom".to_owned())));
    tour.shot("ai-custom");
    tour.send(Message::Settings(settings::Msg::PickPreset(settings::PROVIDER_OFF.to_owned())));
    // The theme picker, opened from its row, over the form.
    tour.click_label("Appearance");
    tour.click_label("ThinkBoard Dark");
    tour.named(key::Named::ArrowDown);
    tour.shot("theme-over-form");
    tour.named(key::Named::Escape);
    tour.shot("theme-cancelled");
    tour.named(key::Named::Escape);

    // The same form in the window the tray opens, at the size it opens at
    // and then narrowed until its rows stack.
    tour.resize(Size::new(820.0, 760.0));
    tour.settings_window();
    tour.click_label("General");
    tour.shot("window-general");
    tour.click_label("Advanced");
    tour.shot("window-advanced");
    tour.resize(Size::new(420.0, 760.0));
    tour.click_label("Appearance");
    tour.shot("window-narrow-appearance");
}

/// Trying themes and fonts on: the pickers preview as you move, and Esc puts
/// the board back.
fn pickers() {
    let mut tour = Tour::new("pickers", first_launch(sample_board));
    tour.palette("theme");
    tour.shot("theme-picker");
    for _ in 0..3 {
        tour.named(key::Named::ArrowDown);
    }
    tour.shot("theme-previewing");
    tour.type_text("nord");
    tour.shot("theme-filtered");
    tour.named(key::Named::Escape);
    tour.shot("theme-cancelled");

    tour.palette("font");
    tour.shot("font-picker");
    tour.type_text("georgia");
    tour.shot("font-filtered");
    tour.named(key::Named::Escape);
}

/// Settings from the palette: a choice found by its name and applied, a
/// setting's choices listed with the one in force marked, and a setting set
/// in the form opened at its row.
fn palette_settings() {
    let mut tour = Tour::new("palette_settings", first_launch(sample_board));
    tour.chord(Modifiers::COMMAND | Modifiers::SHIFT, "p");
    tour.type_text("mermaid");
    tour.shot("mermaid");
    tour.named(key::Named::Enter);
    tour.shot("mermaid-applied");
    tour.chord(Modifiers::COMMAND | Modifiers::SHIFT, "p");
    tour.type_text("link");
    tour.shot("link-choices");
    tour.named(key::Named::Escape);
    tour.palette("pan inertia");
    tour.shot("opened-at-its-row");
}

/// Find on board: a query, the next hit, and the way out.
fn find() {
    let mut tour = Tour::new("find", first_launch(sample_board));
    tour.chord(Modifiers::COMMAND, "f");
    tour.shot("find-bar");
    // A word on a few cards of many: those stay, the rest dim, and the one
    // being stepped through is ringed.
    tour.type_text("trip");
    tour.shot("hits");
    tour.named(key::Named::Enter);
    tour.shot("next-hit");
    tour.type_text("zzz");
    tour.shot("no-hits");
    tour.named(key::Named::Escape);
    tour.shot("closed");
    // A click on the board puts it away too — the first click, before any
    // other the board might act on.
    tour.chord(Modifiers::COMMAND, "f");
    tour.type_text("trip");
    tour.click(Point::new(200.0, 820.0));
    assert!(tour.app.search.is_none(), "a click on empty space closes the find bar");
    tour.shot("closed-by-click");
}

/// Links: hovered, selected, its menu — and the board in each link style.
fn links() {
    let mut tour = Tour::new("links", first_launch(sample_board));
    // Close in on the first topic: its card on the left, its links fanning right.
    let root = tour.find("# Trip to Lisbon");
    let factor = 1.0 / tour.app.camera.zoom;
    tour.send(Message::ZoomBy { factor, cursor: Point::new(720.0, 450.0) });
    tour.settle();
    let to_left = Point::new(360.0, 450.0) - tour.node_center(root);
    tour.send(Message::Pan { delta: to_left, track: false });
    tour.settle();
    // The middle of the link to the third card: halfway between its ports.
    let child = tour.find("Pastéis");
    let (a, b) = (&tour.app.nodes[root], &tour.app.nodes[child]);
    let from = tour.app.camera.to_screen(Point::new(a.x + a.width, a.y + a.height / 2.0));
    let to = tour.app.camera.to_screen(Point::new(b.x, b.y + b.height / 2.0));
    let middle = Point::new((from.x + to.x) / 2.0, (from.y + to.y) / 2.0);
    tour.move_to(middle);
    tour.shot("link-hovered");
    tour.click(middle);
    tour.shot("link-selected");
    tour.right_click(middle);
    tour.shot("link-menu");
    // The menu opens at the click: colour row, label field, arrow chips.
    let width = tour.app.node_menu_width();
    let (left, top) = (middle.x + 4.0, middle.y + 4.0);
    tour.click(Point::new(left + width / 2.0, top + 64.0));
    tour.type_text("the plan\n");
    tour.shot("labelled");
    tour.right_click(middle);
    tour.click(Point::new(left + width - 32.0, top + 103.0));
    tour.shot("both-ways");
    tour.right_click(middle);
    tour.click(Point::new(left + 12.0 + 30.0 * 1.0 + 12.0, top + 18.0));
    // Off the link, or its hover highlight stands in for its colour.
    let empty = Point::new(from.x - 40.0, 860.0);
    tour.move_to(empty);
    tour.shot("coloured");
    tour.click(empty);
    for style in ["mermaid", "square"] {
        tour.app.config.link_style = style.parse().expect("a link style");
        tour.app.cache.clear();
        tour.shot(&format!("style-{style}"));
    }
    // Square links with triangle tips: the diagram look the two make together.
    tour.app.config.arrow_tip = "triangle".to_owned();
    tour.app.cache.clear();
    tour.shot("tips-triangle");
    // Hovered, on its curve: the line drawn into a big head is cut short, and
    // has to stay on the ring drawn under it all the way along.
    tour.app.config.link_style = "obsidian".to_owned();
    tour.app.cache.clear();
    tour.move_to(middle);
    tour.shot("tips-triangle-hovered");
}

/// Grouping: three cards swept up with a marquee, framed, named, moved.
fn groups() {
    let mut tour = Tour::new("groups", first_launch(sample_board));
    let factor = 0.9 / tour.app.camera.zoom;
    tour.send(Message::ZoomBy { factor, cursor: Point::new(720.0, 450.0) });
    tour.settle();
    // A marquee around the first topic's first three cards.
    let (first, third) = (&tour.app.nodes[1], &tour.app.nodes[3]);
    let top_left = tour.app.camera.to_screen(Point::new(first.x - 30.0, first.y - 30.0));
    let bottom_right = tour.app.camera.to_screen(Point::new(third.x + third.width + 30.0, third.y + third.height + 30.0));
    tour.drag(top_left, bottom_right);
    tour.shot("marquee-selected");
    tour.palette("group");
    tour.shot("group-naming");
    tour.type_text("Logistics");
    tour.named(key::Named::Escape);
    tour.shot("group-named");
    tour.click(Point::new(40.0, 860.0));
    tour.shot("deselected");
}

/// The vault browser over a vault made for it: boards in folders, one hidden.
fn vault() {
    let root = std::env::temp_dir().join(format!("thinkboard-tour-vault-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for path in [
        "Trips/Lisbon.canvas", "Trips/Japan 2027.canvas", "Work/Q4 plan.canvas", "Work/Hiring.canvas",
        "Home/Renovation.canvas", "Reading.canvas", ".obsidian/workspace.canvas",
    ] {
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().expect("a folder")).expect("vault folder");
        std::fs::write(&file, r#"{"nodes":[],"edges":[]}"#).expect("a board");
    }
    let mut tour = Tour::new("vault", first_launch(sample_board));
    tour.app.config.vault = root.display().to_string();
    tour.chord(Modifiers::COMMAND, "o");
    tour.shot("browser");
    tour.type_text("trip");
    tour.shot("filtered");
    tour.named(key::Named::Escape);
    let _ = std::fs::remove_dir_all(&root);
}

/// A larger board from far out to close in, then panned off into nothing.
fn big_board() {
    let mut tour = Tour::new("big_board", first_launch(sample_board));
    tour.shot("fitted");
    // The clusters' links with triangle tips: big heads in short gaps.
    tour.app.config.arrow_tip = "triangle".to_owned();
    tour.app.cache.clear();
    tour.shot("fitted-triangle");
    tour.app.config.arrow_tip = "chevron".to_owned();
    tour.app.cache.clear();
    let center = Point::new(720.0, 450.0);
    for step in 1..=3 {
        tour.wheel(center, 3.0);
        tour.shot(&format!("zoom-in-{step}"));
    }
    for _ in 0..12 {
        tour.swipe(center, -400.0, 0.0);
    }
    tour.shot("panned-away");
}

/// The overlay, the way the app is meant to be met: summoned over the
/// desktop, dismissed with Esc and with a click on empty space, summoned again.
fn overlay() {
    let mut tour = Tour::new("overlay", overlay_launch(canvas_file::scratch_document));
    tour.shot("summoned");
    // Slowed, so a strip catches the frames in between: at full speed the
    // whole exit is over between two of them.
    tour.app.config.animation_scale = 4.0;
    let everything = Rectangle::new(Point::ORIGIN, VIEWPORT);
    let mut exit = tour.strip("dismissed-with-esc", everything);
    // What the board publishes for Esc in the overlay; sent rather than typed,
    // since a keystroke settles and would play the whole exit out unseen.
    tour.send(Message::Exit);
    tour.film(&mut exit, "esc");
    tour.save(exit, 3);
    let mut summon = tour.strip("summoned-again", everything);
    tour.send(Message::ToggleOverlay);
    tour.film(&mut summon, "summon");
    tour.save(summon, 3);
    tour.app.config.animation_scale = 1.0;
    tour.click(Point::new(1380.0, 860.0));
    tour.shot("clicked-away");
}

/// Every built-in theme on one board, each chosen the way a person chooses
/// one: the palette, the picker, its name, Return.
fn themes() {
    let mut tour = Tour::new("themes", first_launch(theme_sampler));
    let mut sheet = tour.strip("every-theme", Rectangle::new(Point::ORIGIN, VIEWPORT));
    tour.frame(&mut sheet, "built-in default");
    for name in [
        "ThinkBoard Dark", "Catppuccin Mocha", "Dracula", "Gruvbox Dark", "Monokai",
        "Nord", "One Dark", "Rosé Pine", "Solarized Dark", "Tokyo Night",
    ] {
        tour.palette("theme");
        tour.type_text(name);
        tour.named(key::Named::Enter);
        tour.frame(&mut sheet, name);
    }
    tour.save(sheet, 2);
}

/// What a card can be besides text: an image, an embedded note, an
/// attachment, a web link — from a vault made for them.
fn card_kinds() {
    let dir = std::env::temp_dir().join(format!("thinkboard-tour-kinds-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a folder for the files");
    // A picture to show: a sky, a sea, a sun.
    let image = ::image::RgbaImage::from_fn(480, 300, |x, y| {
        let (fx, fy) = (x as f32 / 480.0, y as f32 / 300.0);
        if (fx - 0.72).hypot((fy - 0.32) * 0.62) < 0.08 {
            ::image::Rgba([255, 214, 120, 255])
        } else if fy > 0.62 {
            ::image::Rgba([(30.0 + 40.0 * fy) as u8, (90.0 + 30.0 * fy) as u8, (140.0 + 20.0 * fy) as u8, 255])
        } else {
            ::image::Rgba([(250.0 - 90.0 * fy) as u8, (170.0 + 30.0 * fy) as u8, (150.0 + 90.0 * fy) as u8, 255])
        }
    });
    image.save(dir.join("Sunset.png")).expect("the picture");
    std::fs::write(dir.join("Meeting notes.md"), "# Meeting notes\n\nAgreed to ship the **sync** rewrite first.\n\n- Ana: migration plan\n- Bo: load tests\n").expect("a note");
    std::fs::write(dir.join("Quarterly report.pdf"), "%PDF-1.4\n").expect("an attachment");

    let document = |_: &ColorTheme| {
        use thinkboard_core::fixtures::node;
        let file = |id: &str, path: &str, (x, y, w, h): (f32, f32, f32, f32)| CanvasNode {
            kind: "file".to_owned(),
            text: None,
            file: Some(path.to_owned()),
            x,
            y,
            width: w,
            height: h,
            ..node(id)
        };
        let mut link = CanvasNode { kind: "link".to_owned(), text: None, x: 520.0, y: 300.0, width: 320.0, height: 90.0, ..node("link") };
        link.extra.insert("url".to_owned(), serde_json::Value::String("https://www.rust-lang.org/learn".to_owned()));
        let nodes = vec![
            file("image", "Sunset.png", (0.0, 0.0, 400.0, 250.0)),
            file("note", "Meeting notes.md", (520.0, 0.0, 320.0, 240.0)),
            file("pdf", "Quarterly report.pdf", (0.0, 320.0, 400.0, 70.0)),
            link,
            CanvasNode { text: Some("A plain **text** card, for comparison.".to_owned()), x: 0.0, y: 450.0, width: 400.0, height: 70.0, ..node("text") },
        ];
        CanvasDocument { nodes, edges: Vec::new(), extra: serde_json::Map::new() }
    };
    let mut tour = Tour::new("card_kinds", launch(document, true, dir.clone()));
    tour.shot("kinds");
    let note = tour.app.nodes.iter().position(|n| n.id == "note").expect("the note");
    tour.move_to(tour.node_center(note));
    tour.shot("note-hovered");
    let image = tour.app.nodes.iter().position(|n| n.id == "image").expect("the image");
    tour.click(tour.node_center(image));
    tour.shot("image-selected");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Tidying a board: a messy tree laid out with one command, a card dragged
/// with snap to grid on, and one resized by its corner.
fn arranging() {
    let messy = |_: &ColorTheme| {
        use thinkboard_core::fixtures::{edge, node};
        let card = |id: &str, text: &str, x: f32, y: f32| CanvasNode {
            text: Some(text.to_owned()),
            x,
            y,
            width: 200.0,
            height: 60.0,
            ..node(id)
        };
        CanvasDocument {
            nodes: vec![
                card("a", "Launch plan", 300.0, 260.0),
                card("b", "Marketing", -120.0, 40.0),
                card("c", "Engineering", 620.0, -80.0),
                card("d", "Landing page", 80.0, 420.0),
                card("e", "Press kit", 520.0, 460.0),
                card("f", "Release build", 900.0, 220.0),
            ],
            edges: vec![edge("a", "b"), edge("a", "c"), edge("b", "d"), edge("b", "e"), edge("c", "f")],
            extra: serde_json::Map::new(),
        }
    };
    let mut tour = Tour::new("arranging", first_launch(messy));
    tour.shot("messy");
    tour.drag(Point::new(12.0, 12.0), Point::new(1428.0, 888.0));
    tour.shot("all-selected");
    tour.palette("auto-layout sel");
    tour.shot("laid-out");

    // Snap to grid, from the board's menu.
    tour.click(Point::new(12.0, 888.0));
    let at = Point::new(120.0, 120.0);
    tour.right_click(at);
    tour.click(Point::new(at.x + 60.0, at.y + 4.0 + 93.0));
    // A toggle leaves the menu up, showing its tick; Esc puts it away (a
    // click would only close it, and start no drag).
    tour.named(key::Named::Escape);
    let first = tour.find("Launch plan");
    let from = tour.node_center(first);
    tour.move_to(from);
    tour.press(mouse::Button::Left);
    for step in 1..=8 {
        tour.move_to(Point::new(from.x + step as f32 * 9.0, from.y + step as f32 * 7.0));
    }
    tour.shot("dragging-on-the-grid");
    tour.release(mouse::Button::Left);

    // A card's corner, dragged out.
    let card = &tour.app.nodes[first];
    let corner = tour.app.camera.to_screen(Point::new(card.x + card.width - 1.0, card.y + card.height - 1.0));
    tour.move_to(corner);
    tour.press(mouse::Button::Left);
    for step in 1..=6 {
        tour.move_to(Point::new(corner.x + step as f32 * 20.0, corner.y + step as f32 * 12.0));
    }
    tour.shot("resizing");
    tour.release(mouse::Button::Left);
    tour.shot("resized");
}

/// What the stand-in providers serve. None of these is the model a new config
/// names: that one is a local runtime's, and a hosted provider wouldn't have it.
const MODELS: &[(&str, Option<&str>, Option<u64>)] = &[
    ("google/gemma-3-27b-it", Some("Google: Gemma 3 27B"), Some(131_072)),
    ("meta-llama/llama-3.3-70b-instruct", Some("Meta: Llama 3.3 70B Instruct"), Some(131_072)),
    ("mistralai/mistral-small-3.2-24b-instruct", Some("Mistral: Mistral Small 3.2 24B"), Some(131_072)),
    ("qwen/qwen3-30b-a3b", Some("Qwen: Qwen3 30B A3B"), Some(40_960)),
];

/// A provider that knows about the trip board.
static TRIP: Script = Script {
    models: MODELS,
    answers: &[
        ("eat", "**Cervejaria Ramiro** — seafood, worth the queue; go early on a weekday.\n---\n**Taberna da Rua das Flores** — small plates and no bookings, so arrive by seven.\n---\n**Time Out Market** — a quick lunch, half the city's kitchens under one roof."),
        ("Sintra", "Take the train from **Rossio**: about 40 minutes, every 20 or so. Go early — by eleven the queue for *Pena Palace* is long, and the 434 bus up the hill fills fast.\n\nA full day covers Pena, the Moorish Castle and *Quinta da Regaleira*. Book Pena's tickets online the day before."),
    ],
    reject_key: false,
};

/// A provider that refuses the key it's given.
static REJECTING: Script = Script { models: MODELS, answers: &[], reject_key: true };

/// A trip being planned: the trip, and two questions about it.
fn trip_board(_: &ColorTheme) -> CanvasDocument {
    use thinkboard_core::fixtures::{edge, node};
    let card = |id: &str, text: &str, (x, y, width, height): (f32, f32, f32, f32)| CanvasNode {
        text: Some(text.to_owned()),
        x,
        y,
        width,
        height,
        ..node(id)
    };
    CanvasDocument {
        nodes: vec![
            card("trip", "# Trip to Lisbon\n\nA long weekend in **October**, two of us.", (-520.0, -60.0, 300.0, 120.0)),
            card("eat", "Where should we eat?", (-120.0, -200.0, 240.0, 60.0)),
            card("sintra", "How do we get to *Sintra*?", (-120.0, 140.0, 240.0, 60.0)),
        ],
        edges: vec![edge("trip", "eat"), edge("trip", "sintra")],
        extra: serde_json::Map::new(),
    }
}

/// Zoom 1, with the trip's card a sixth of the way in.
fn frame_trip(tour: &mut Tour) {
    let factor = 1.0 / tour.app.camera.zoom;
    tour.send(Message::ZoomBy { factor, cursor: Point::new(720.0, 450.0) });
    tour.settle();
    let trip = tour.find("# Trip to Lisbon");
    let delta = Point::new(240.0, 450.0) - tour.node_center(trip);
    tour.send(Message::Pan { delta, track: false });
    tour.settle();
}

/// Points the app at `url` with a model it serves — the way `ai_setup` gets
/// there by hand.
fn configure(tour: &mut Tour, url: &str) {
    tour.app.config.ai_url = url.to_owned();
    tour.app.config.ai_model = "qwen/qwen3-30b-a3b".to_owned();
}

/// Asks about the card `index` from its menu.
fn ask(tour: &mut Tour, index: usize) {
    tour.right_click(tour.node_center(index));
    tour.click_label("Ask AI");
}

/// Asking for the first time: no provider yet, then one chosen in settings
/// and its model chosen beside it, from the provider's own list.
fn ai_setup() {
    let provider = Provider::start(&TRIP);
    let mut tour = Tour::new("ai_setup", first_launch(trip_board));
    frame_trip(&mut tour);
    let eat = tour.find("Where should");
    tour.right_click(tour.node_center(eat));
    tour.shot("no-provider-note-menu");
    tour.named(key::Named::Escape);
    tour.chord(Modifiers::COMMAND | Modifiers::SHIFT, "p");
    tour.type_text("ask");
    tour.shot("no-provider-palette");
    tour.named(key::Named::Escape);

    // Settings, AI, "Custom", and the URL typed in. The pick list is set by
    // its message, as in `settings`: its menu is an overlay of its own.
    tour.palette("settings");
    tour.click_label("AI");
    tour.send(Message::Settings(settings::Msg::PickPreset("Custom".to_owned())));
    tour.send(Message::Settings(settings::Msg::Edit(settings::Field::AiUrl, provider.url.clone())));
    tour.send(Message::Settings(settings::Msg::Commit));
    tour.shot("provider-chosen");

    tour.click_label("Choose…");
    tour.shot("model-picker-fetching");
    tour.settle_until(|app| app.model_picker.as_ref().is_some_and(|p| !p.loading));
    tour.shot("model-picker");
    tour.type_text("qwen");
    tour.shot("model-picker-filtered");
    tour.named(key::Named::Enter);
    tour.shot("model-chosen");
    tour.named(key::Named::Escape);

    tour.right_click(tour.node_center(eat));
    tour.shot("note-menu");
    tour.click_label("Ask AI");
    tour.settle_until(|app| app.ai.streams.is_empty());
    tour.shot("answered");
}

/// An answer: the board waiting on its first word, the words arriving, and
/// the cards it splits into at each `---` — then a second question, asked
/// from the palette, answered in prose.
fn ai_answers() {
    let provider = Provider::start(&TRIP);
    let mut tour = Tour::new("ai_answers", first_launch(trip_board));
    configure(&mut tour, &provider.url);
    frame_trip(&mut tour);
    let eat = tour.find("Where should");
    let at = tour.node_center(eat);
    tour.right_click(at);
    tour.start("Ask AI");
    let region = Rectangle::new(Point::new(at.x - 140.0, at.y - 180.0), Size::new(980.0, 560.0));
    let mut strip = tour.strip("arriving", region);
    tour.film_while(&mut strip, "asked", Duration::from_millis(150), |app| !app.ai.streams.is_empty());
    tour.save(strip, 4);
    tour.shot("answered");

    let sintra = tour.find("How do we get");
    tour.click(tour.node_center(sintra));
    tour.palette("ask");
    tour.settle_until(|app| app.ai.streams.is_empty());
    tour.shot("asked-from-the-palette");
    tour.click(Point::new(1100.0, 860.0));
    tour.shot("deselected");
}

/// When asking fails: a model the provider doesn't serve (the one a new
/// config names, kept by someone who never chose), nothing listening at the
/// URL (a local runtime not started yet), no key where one is wanted, and a
/// key the provider refuses.
fn ai_errors() {
    let provider = Provider::start(&TRIP);
    let mut tour = Tour::new("ai_errors", first_launch(trip_board));
    configure(&mut tour, &provider.url);
    tour.app.config.ai_model = config::Config::default().ai_model;
    frame_trip(&mut tour);
    let eat = tour.find("Where should");
    ask(&mut tour, eat);
    tour.settle_until(|app| app.ai.streams.is_empty());
    tour.shot("model-not-served");

    configure(&mut tour, &Provider::absent());
    let sintra = tour.find("How do we get");
    ask(&mut tour, sintra);
    tour.settle_until(|app| app.ai.streams.is_empty());
    tour.shot("nothing-listening");

    // A hosted provider asked with no key, then with one it refuses.
    let rejecting = Provider::start(&REJECTING);
    configure(&mut tour, &rejecting.url);
    let trip = tour.find("# Trip to Lisbon");
    ask(&mut tour, trip);
    tour.settle_until(|app| app.ai.streams.is_empty());
    tour.shot("no-key");
    tour.app.config.set_ai_key("sk-not-a-real-key");
    ask(&mut tour, sintra);
    tour.settle_until(|app| app.ai.streams.is_empty());
    tour.shot("key-rejected");
    tour.palette("model");
    tour.settle_until(|app| app.model_picker.as_ref().is_some_and(|p| !p.loading));
    tour.shot("model-list-key-rejected");
    tour.named(key::Named::Escape);
}

/// Changes the board at `path` the way another program does — Obsidian, a
/// sync client: reads it, changes it, writes it back whole.
fn elsewhere(path: &std::path::Path, change: impl FnOnce(&mut CanvasDocument)) {
    let mut document = CanvasDocument::load(path).expect("the board");
    change(&mut document);
    document.save(path).expect("written back");
}

/// The cards in the file at `path`, by their first line.
fn on_disk(path: &std::path::Path) -> String {
    match CanvasDocument::load(path) {
        Ok(document) => document
            .nodes
            .iter()
            .map(|node| node.text.as_deref().unwrap_or_default().lines().next().unwrap_or_default().to_owned())
            .collect::<Vec<_>>()
            .join(" | "),
        Err(error) => format!("unreadable: {error}"),
    }
}

/// The board's file changed by another program while it's open — idle, with
/// a card selected, after an edit here that undo should take back, while a
/// card is being edited, renamed, and deleted. What the file holds after each
/// is in `notes.txt` beside the shots.
fn outside_changes() {
    use thinkboard_core::fixtures::{edge, node};
    let dir = std::env::temp_dir().join(format!("thinkboard-tour-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a folder for the board");
    let path = dir.join("Trip.canvas");
    trip_board(&ColorTheme::default()).save(&path).expect("the board on disk");
    let card = |id: &str, text: &str, y: f32| CanvasNode {
        text: Some(text.to_owned()),
        x: -120.0,
        y,
        width: 240.0,
        height: 60.0,
        ..node(id)
    };

    let mut tour = Tour::new("outside_changes", file_launch(&path));
    frame_trip(&mut tour);
    tour.watch();
    tour.shot("open");

    // Nothing happening here: a card added there, and a question reworded.
    elsewhere(&path, |doc| {
        doc.nodes.push(card("umbrella", "Pack an umbrella", 300.0));
        doc.edges.push(edge("trip", "umbrella"));
        if let Some(eat) = doc.nodes.iter_mut().find(|n| n.id == "eat") {
            eat.text = Some("Where should we eat on Saturday?".to_owned());
        }
    });
    tour.settle_until(|app| app.nodes.len() == 4);
    tour.shot("changed-while-idle");

    // A card selected here, and moved there.
    let sintra = tour.find("How do we get");
    tour.click(tour.node_center(sintra));
    tour.shot("selected");
    elsewhere(&path, |doc| {
        if let Some(sintra) = doc.nodes.iter_mut().find(|n| n.id == "sintra") {
            sintra.x += 160.0;
        }
    });
    tour.settle_until(|app| app.nodes.iter().any(|n| n.id == "sintra" && n.x > -100.0));
    tour.shot("changed-while-selected");

    // A card moved here and saved, then something changed there — and undo.
    let umbrella = tour.find("Pack an umbrella");
    let from = tour.node_center(umbrella);
    tour.drag(from, Point::new(from.x + 300.0, from.y));
    tour.settle_until(|app| !app.doc_dirty);
    let moved_to = tour.app.nodes[tour.find("Pack an umbrella")].x;
    elsewhere(&path, |doc| {
        if let Some(sintra) = doc.nodes.iter_mut().find(|n| n.id == "sintra") {
            sintra.text = Some("How do we get to *Sintra* and back?".to_owned());
        }
    });
    tour.settle_until(|app| app.nodes.iter().any(|n| n.text.as_deref().is_some_and(|t| t.ends_with("and back?"))));
    let state = |tour: &Tour| (tour.app.nodes.clone(), tour.app.edges.clone());
    for step in 1..=2 {
        let before = state(&tour);
        tour.chord(Modifiers::COMMAND, "z");
        tour.note(&format!("Cmd+Z #{step} after a change there (the umbrella was moved here to x={moved_to}): {}", changes(&before, &state(&tour))));
        tour.shot(&format!("undo-{step}-after-a-change-there"));
    }
    // And back, so the scene goes on from the board as it was.
    for _ in 1..=2 {
        tour.chord(Modifiers::COMMAND | Modifiers::SHIFT, "z");
    }

    // A card being edited here while one is added there.
    let eat = tour.find("Where should we eat");
    tour.double_click(tour.node_center(eat));
    // To the end of the card, which wraps onto a second line.
    tour.named(key::Named::ArrowDown);
    tour.named(key::Named::End);
    tour.type_text(" Near Alfama.");
    elsewhere(&path, |doc| {
        doc.nodes.push(card("tram", "Buy tram passes", 440.0));
        doc.edges.push(edge("trip", "tram"));
    });
    tour.note(&format!("added there while editing here — on disk: {}", on_disk(&path)));
    tour.wait(Duration::from_secs(1));
    tour.shot("changed-while-editing");
    tour.named(key::Named::Escape);
    tour.settle_until(|app| !app.doc_dirty);
    tour.wait(Duration::from_secs(1));
    tour.shot("the-edit-saved");
    tour.note(&format!("after the edit here was saved — on disk: {}", on_disk(&path)));

    // Renamed there, then a card moved here.
    let renamed = dir.join("Lisbon.canvas");
    std::fs::rename(&path, &renamed).expect("renamed");
    tour.settle_until(|app| app.canvas_path == renamed);
    // The subscription watches whatever `canvas_path` names; so does this.
    tour.watch();
    let eat = tour.find("Where should");
    let from = tour.node_center(eat);
    tour.drag(from, Point::new(from.x, from.y - 40.0));
    tour.settle_until(|app| !app.doc_dirty);
    tour.shot("moved-after-a-rename");
    let mut files: Vec<String> = std::fs::read_dir(&dir)
        .expect("the folder")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    files.sort();
    tour.note(&format!("renamed Trip.canvas to Lisbon.canvas there, then moved a card here — the folder holds: {}", files.join(", ")));

    // Deleted there, then a card moved here: nothing is saved until it's
    // asked for.
    std::fs::remove_file(&renamed).expect("deleted");
    tour.settle_until(|app| app.disk.missing);
    let from = tour.node_center(tour.find("Where should"));
    tour.drag(from, Point::new(from.x, from.y + 40.0));
    tour.wait(Duration::from_secs(1));
    tour.shot("deleted-there");
    tour.note(&format!("deleted there, then moved a card here — Lisbon.canvas exists: {}", renamed.exists()));
    tour.click_label("Save it again");
    tour.settle_until(|app| !app.disk.missing);
    tour.shot("saved-again");
    tour.note(&format!("after \"Save it again\" — on disk: {}", on_disk(&renamed)));

    let _ = std::fs::remove_dir_all(&dir);
}

/// What changed between two states of the board, card by card.
fn changes(before: &(Vec<CanvasNode>, Vec<CanvasEdge>), after: &(Vec<CanvasNode>, Vec<CanvasEdge>)) -> String {
    let name = |node: &CanvasNode| {
        node.text
            .as_deref()
            .and_then(|text| text.lines().next())
            .or_else(|| node.extra.get("label").and_then(|label| label.as_str()))
            .or(node.file.as_deref())
            .unwrap_or(node.kind.as_str())
            .to_owned()
    };
    let mut out = Vec::new();
    for node in &after.0 {
        let Some(old) = before.0.iter().find(|old| old.id == node.id) else {
            out.push(format!("+{:?}", name(node)));
            continue;
        };
        if (old.x, old.y) != (node.x, node.y) {
            out.push(format!("moved {:?}", name(node)));
        }
        if (old.width, old.height) != (node.width, node.height) {
            out.push(format!("resized {:?}", name(node)));
        }
        if old.text != node.text {
            out.push(format!("text of {:?}", name(old)));
        }
    }
    for node in &before.0 {
        if !after.0.iter().any(|new| new.id == node.id) {
            out.push(format!("-{:?}", name(node)));
        }
    }
    match after.1.len() as i64 - before.1.len() as i64 {
        0 => {}
        links => out.push(format!("{links:+} link(s)")),
    }
    if out.is_empty() { "nothing".to_owned() } else { out.join(", ") }
}

/// Undo and the clipboard. Ordinary edits, then Cmd+Z a step at a time and
/// Shift+Cmd+Z back — what each step undid is in `notes.txt` — then cards
/// copied and pasted, and what another app left on the clipboard pasted:
/// Markdown, a long paragraph, an image.
fn undo_and_clipboard() {
    let dir = std::env::temp_dir().join(format!("thinkboard-tour-clip-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a folder for the board");
    let path = dir.join("Trip.canvas");
    trip_board(&ColorTheme::default()).save(&path).expect("the board on disk");
    let mut tour = Tour::new("undo_and_clipboard", file_launch(&path));
    frame_trip(&mut tour);
    let state = |tour: &Tour| (tour.app.nodes.clone(), tour.app.edges.clone());
    let empty = Point::new(1100.0, 820.0);

    // The edits.
    let mut done = vec![state(&tour)];
    let eat = tour.find("Where should");
    let from = tour.node_center(eat);
    tour.drag(from, Point::new(from.x + 40.0, from.y - 80.0));
    done.push(state(&tour));
    let sintra = &tour.app.nodes[tour.find("How do we get")];
    let corner = tour.app.camera.to_screen(Point::new(sintra.x + sintra.width - 1.0, sintra.y + sintra.height - 1.0));
    tour.drag(corner, Point::new(corner.x + 80.0, corner.y + 40.0));
    done.push(state(&tour));
    tour.right_click(Point::new(560.0, 790.0));
    tour.click_label("Add card");
    tour.type_text("Pack an umbrella");
    tour.named(key::Named::Escape);
    done.push(state(&tour));
    let trip = tour.find("# Trip to Lisbon");
    tour.double_click(tour.node_center(trip));
    for _ in 0..4 {
        tour.named(key::Named::ArrowDown);
    }
    tour.named(key::Named::End);
    tour.type_text(" Flights booked.");
    tour.named(key::Named::Escape);
    done.push(state(&tour));
    let sintra = tour.find("How do we get");
    tour.click(tour.node_center(sintra));
    tour.named(key::Named::Backspace);
    done.push(state(&tour));
    tour.click(tour.node_center(tour.find("# Trip to Lisbon")));
    tour.hold(Modifiers::COMMAND);
    tour.click(tour.node_center(tour.find("Where should")));
    tour.hold(Modifiers::empty());
    tour.palette("group");
    tour.type_text("Plan");
    tour.named(key::Named::Escape);
    done.push(state(&tour));
    tour.click(empty);
    tour.shot("edited");
    let names = ["moved a card", "resized a card", "added a note", "edited a note", "deleted a card", "grouped two"];
    for (i, name) in names.iter().enumerate() {
        tour.note(&format!("did {name}: {}", changes(&done[i], &done[i + 1])));
    }

    // And back, a step at a time.
    let mut strip = tour.strip("undoing", Rectangle::new(Point::ORIGIN, VIEWPORT));
    tour.frame(&mut strip, "edited");
    for step in 1..=7 {
        let before = state(&tour);
        tour.chord(Modifiers::COMMAND, "z");
        let undid = changes(&before, &state(&tour));
        tour.note(&format!("Cmd+Z #{step}: {undid}"));
        tour.frame(&mut strip, &format!("Cmd+Z #{step}"));
    }
    for step in 1..=2 {
        let before = state(&tour);
        tour.chord(Modifiers::COMMAND | Modifiers::SHIFT, "z");
        tour.note(&format!("Shift+Cmd+Z #{step}: {}", changes(&before, &state(&tour))));
        tour.frame(&mut strip, &format!("Shift+Cmd+Z #{step}"));
    }
    tour.save(strip, 4);

    // Cards copied and pasted: the two of them, and the link between.
    tour.click(tour.node_center(tour.find("# Trip to Lisbon")));
    tour.hold(Modifiers::COMMAND);
    tour.click(tour.node_center(tour.find("Where should")));
    tour.hold(Modifiers::empty());
    tour.chord(Modifiers::COMMAND, "c");
    tour.move_to(Point::new(1000.0, 700.0));
    let before = state(&tour);
    tour.chord(Modifiers::COMMAND, "v");
    tour.note(&format!("pasted copied cards: {}", changes(&before, &state(&tour))));
    tour.shot("cards-pasted");

    // What another app copied.
    let pastes: [(&str, super::clipboard::Clip, Point); 3] = [
        ("markdown", super::clipboard::Clip::Text("## Packing\n\n- passport\n- adapter\n- **sunscreen**".to_owned()), Point::new(1180.0, 160.0)),
        ("a-paragraph", super::clipboard::Clip::Text("Lisbon's trams are part of the city: the 28 climbs from Martim Moniz through Graça and Alfama to Estrela, past most of what a first visit wants to see, in old wooden cars that rattle and lean on every corner. Ride it early, before the queues, or take the 12 for a shorter loop. Validate a Viva Viagem card as you board.".to_owned()), Point::new(1180.0, 420.0)),
        ("an-image", super::clipboard::Clip::Image { width: 480, height: 320, rgba: postcard(480, 320) }, Point::new(380.0, 760.0)),
    ];
    for (name, clip, at) in pastes {
        tour.click(empty);
        *super::clipboard::system::held() = Some(clip);
        tour.move_to(at);
        let before = state(&tour);
        tour.chord(Modifiers::COMMAND, "v");
        tour.note(&format!("pasted {name}: {}", changes(&before, &state(&tour))));
        let node = tour.app.nodes.last().expect("pasted");
        let (top_left, bottom_right) = (
            tour.app.camera.to_screen(Point::new(node.x, node.y)),
            tour.app.camera.to_screen(Point::new(node.x + node.width, node.y + node.height)),
        );
        tour.note(&format!(
            "  pasted at the cursor {at:?}; landed on screen at {top_left:?}–{bottom_right:?}, {}×{} at zoom {:.2}",
            node.width, node.height, tour.app.camera.zoom,
        ));
        tour.shot(&format!("pasted-{name}"));
    }
    let pasted: Vec<String> = std::fs::read_dir(&dir)
        .expect("the folder")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    tour.note(&format!("the board's folder holds: {}", pasted.join(", ")));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A picture to paste: a warm sky over a sea, as RGBA.
fn postcard(width: usize, height: usize) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(width * height * 4);
    for y in 0..height {
        for x in 0..width {
            let t = y as f32 / height as f32;
            let sea = t > 0.62;
            let (r, g, b) = if sea {
                (30.0 + 40.0 * t, 90.0 + 30.0 * t, 140.0 + 20.0 * (x as f32 / width as f32))
            } else {
                (250.0 - 60.0 * t, 170.0 - 90.0 * t, 90.0 + 80.0 * t)
            };
            rgba.extend_from_slice(&[r as u8, g as u8, b as u8, 255]);
        }
    }
    rgba
}

/// A guide to getting around, long enough to pass the auto-size ceiling when
/// it's pasted — with one word, *Cacilhas*, only near its end, to find.
const ARTICLE: &str = "# Getting around Lisbon

Lisbon is built on seven hills, and most of what a first visit wants to see is on the side of one. Good shoes matter more than any ticket.

## Trams

The **28** is the famous one: it climbs from Martim Moniz through Graça and Alfama to Estrela in old wooden cars that rattle and lean on every corner. Ride it early, before the queues, or take the 12 for a shorter loop through the same streets.

The 15 is the opposite: a modern articulated tram along the river to Belém, quick and dull, and the right way to reach the monastery and the tower.

## Funiculars and the lift

Three funiculars — Glória, Bica and Lavra — haul you up the steepest streets in a single car. The Santa Justa lift goes straight up from the Baixa to the Carmo ruins; the queue is longer than the ride.

## Metro

Four lines, clean and frequent. The **green** line runs from Cais do Sodré to the old town; the **red** goes to the airport in about twenty minutes, which beats any taxi at rush hour.

## Tickets

Buy a *Viva Viagem* card at any metro station and load it with zapping credit: one tap per ride, cheaper than single tickets, and it works on trams, buses, funiculars, the lift and the ferries. Validate it as you board — inspectors are common on the 28.

A 24-hour pass pays off after five rides. It doesn't cover the Sintra or Cascais trains, which have their own fares.

## Trains

Trains to Sintra leave from Rossio every twenty minutes; the ride is forty. Trains to Cascais leave from Cais do Sodré along the coast, and the right-hand seats have the sea.

## Walking

Walk downhill. Take a tram or a funicular up, then walk the long way down through the alleys, where the city is. Pavements are polished limestone and slippery when it rains.

## Ferries

The ferry from Cais do Sodré to Cacilhas takes ten minutes and costs less than a coffee. Go at sunset for the view back across the river to the whole city, then eat grilled fish by the water before the last boat home.";

/// Text an Obsidian card was left too small for — how a card drawn in
/// Obsidian holds text that later grew.
const GROWN: &str = "## Restaurants to try

**Taberna da Rua das Flores** — no bookings, so arrive at opening; the menu is a chalkboard and changes daily.

**Cervejaria Ramiro** — seafood, loud, worth the queue; finish with a steak sandwich, as everyone does.

**A Cevicheria** — the giant octopus on the ceiling is the landmark.

**Tasca do Chico** — fado after nine, tiny, bring cash.";

/// A long note in the vault, embedded on the board as a file.
const GUIDE: &str = "---
tags: [travel, lisbon]
---
# Lisbon guide

Notes gathered over two visits, for whoever goes next.

## Neighbourhoods

**Alfama** is the oldest: a maze of stairs below the castle, washing hung across the alleys, fado in the evenings. Stay here for atmosphere, not for sleep.

**Baixa** and **Chiado** are the grid rebuilt after the 1755 earthquake — shops, cafés, the big squares down to the river.

**Bairro Alto** is quiet by day and a street party by night.

**Belém** is a tram ride west: the monastery, the tower, the monument to the discoveries, and the original pastéis de nata.

## Food

Grilled sardines in June, bifanas any time, a ginjinha in a chocolate cup if you insist. Lunch is the big meal, and the *prato do dia* is the cheap way to eat well.

## Practical

Tap water is fine. Cards are taken almost everywhere, but small tascas want cash. Sundays are quiet; Mondays many museums close.";

/// Long cards: a guide pasted past the auto-size ceiling, a card Obsidian
/// left smaller than its text, a link and a line of code too long to wrap, a
/// table, and a long note from the vault — read, scrolled, searched, edited
/// and zoomed. What a shot can't show (sizes, scroll offsets) is in
/// `notes.txt`.
fn long_cards() {
    let dir = std::env::temp_dir().join(format!("thinkboard-tour-long-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a folder for the vault");
    std::fs::write(dir.join("Lisbon guide.md"), GUIDE).expect("a note in the vault");
    let document = |_: &ColorTheme| {
        use thinkboard_core::fixtures::node;
        let text = |id: &str, text: &str, (x, y, w, h): (f32, f32, f32, f32)| CanvasNode {
            text: Some(text.to_owned()),
            x,
            y,
            width: w,
            height: h,
            ..node(id)
        };
        let nodes = vec![
            text("grown", GROWN, (0.0, 0.0, 400.0, 180.0)),
            text(
                "link",
                "Booking: https://www.example-hotels.com/lisbon/alfama/rooms/deluxe-river-view?checkin=2026-10-23&checkout=2026-10-26&guests=2",
                (0.0, 240.0, 300.0, 100.0),
            ),
            text(
                "code",
                "```sh\ncurl -s \"https://api.example.com/v1/forecast?lat=38.72&lon=-9.14&days=4\" | jq '.daily[] | {date, rain: .precipitation_probability}'\n```",
                (0.0, 400.0, 360.0, 130.0),
            ),
            text(
                "table",
                "| Day | Morning | Evening |\n|---|---|---|\n| Fri | Land, check in | Alfama, fado |\n| Sat | Belém | Bairro Alto |\n| Sun | Sintra | Ferry at sunset |",
                (0.0, 590.0, 400.0, 170.0),
            ),
            CanvasNode {
                kind: "file".to_owned(),
                text: None,
                file: Some("Lisbon guide.md".to_owned()),
                x: 480.0,
                y: 0.0,
                width: 400.0,
                height: 400.0,
                ..node("guide")
            },
        ];
        CanvasDocument { nodes, edges: Vec::new(), extra: serde_json::Map::new() }
    };
    let mut tour = Tour::new("long_cards", launch(document, true, dir.clone()));
    tour.shot("board");

    // The guide, pasted onto the board from wherever it was written.
    *super::clipboard::system::held() = Some(super::clipboard::Clip::Text(ARTICLE.to_owned()));
    let beside = tour.app.camera.to_screen(Point::new(1100.0, 300.0));
    tour.move_to(beside);
    tour.chord(Modifiers::COMMAND, "v");
    let article = tour.find("# Getting around");
    let card = tour.app.nodes[article].clone();
    tour.note(&format!(
        "pasted the guide ({} chars): card {}×{}, ceiling {}",
        ARTICLE.len(), card.width, card.height, text_raster::MAX_AUTO_HEIGHT
    ));
    tour.shot("pasted");

    // Read at 1:1, from its top.
    let zoom = tour.app.camera.zoom;
    tour.send(Message::ZoomBy { factor: 1.0 / zoom, cursor: Point::new(720.0, 450.0) });
    tour.settle();
    let card = tour.app.nodes[article].clone();
    tour.send(Message::FocusWorld(Point::new(card.x + card.width / 2.0, card.y + 420.0)));
    tour.settle();
    tour.shot("article-top");

    // Scrolled the way a trackpad scrolls, over the card.
    let over = tour.node_center(article);
    let before = (tour.app.camera.offset, tour.app.node_scroll.get(&card.id).copied());
    tour.swipe(over, 0.0, -300.0);
    let after = (tour.app.camera.offset, tour.app.node_scroll.get(&card.id).copied());
    tour.note(&format!("two-finger scroll over the guide: camera {:?} → {:?}, the card's own scroll {:?} → {:?}", before.0, after.0, before.1, after.1));
    tour.shot("scrolled-over-it");
    tour.swipe(over, 0.0, 300.0);

    // By its scrollbar, which is the way the card has.
    let rect = tour.app.camera.rect_to_screen(arrange::node_world_rect(&tour.app.nodes[article]));
    let grab = Point::new(rect.x + rect.width - 6.0, rect.y.max(0.0) + 30.0);
    tour.drag(grab, Point::new(grab.x, grab.y + 500.0));
    tour.note(&format!("dragged the scrollbar down 500 px: the card's scroll is {:?}", tour.app.node_scroll.get(&card.id)));
    tour.shot("scrolled-by-its-bar");

    // A word down there, double-clicked to edit: where does the caret go, and
    // where does the editor open?
    let region = Rectangle::new(Point::new(rect.x - 20.0, 0.0), Size::new(rect.width + 40.0, 900.0));
    let mut edit = tour.strip("editing-scrolled", region);
    tour.frame(&mut edit, "scrolled, reading");
    tour.double_click(Point::new(rect.x + rect.width / 2.0, 520.0));
    tour.frame(&mut edit, "double-clicked at y=520");
    tour.type_text(" HERE");
    tour.frame(&mut edit, "typed \" HERE\"");
    tour.named(key::Named::Escape);
    tour.frame(&mut edit, "esc");
    tour.save(edit, 4);
    let text = tour.app.nodes[article].text.clone().unwrap_or_default();
    let line = text.lines().find(|line| line.contains("HERE")).unwrap_or("(nowhere)");
    tour.note(&format!("double-clicked a line 520 px down the scrolled card; typed into: {line:?}"));
    tour.chord(Modifiers::COMMAND, "z");

    // A word that's only near its end, looked for with the card back at its
    // top.
    tour.send(Message::SetNodeScroll { id: card.id.clone(), offset: 0.0 });
    tour.chord(Modifiers::COMMAND, "f");
    tour.type_text("Cacilhas");
    tour.settle();
    tour.note(&format!(
        "find \"Cacilhas\": {} hit(s); the card's scroll is {:?}",
        tour.app.search.as_ref().map_or(0, |s| s.matches.len()),
        tour.app.node_scroll.get(&card.id)
    ));
    tour.shot("found-near-the-end");
    tour.named(key::Named::Escape);

    // The card Obsidian left too small.
    let grown = tour.find("## Restaurants");
    let at = tour.app.nodes[grown].clone();
    tour.send(Message::FocusWorld(Point::new(at.x + at.width / 2.0, at.y + at.height / 2.0 + 150.0)));
    tour.settle();
    tour.shot("left-too-small");
    tour.move_to(tour.node_center(grown));
    tour.swipe(tour.node_center(grown), 0.0, -120.0);
    tour.shot("left-too-small-scrolled-over");
    tour.swipe(tour.node_center(grown), 0.0, 120.0);
    tour.double_click(tour.node_center(grown));
    tour.shot("left-too-small-editing");
    tour.named(key::Named::Escape);
    let after = &tour.app.nodes[grown];
    tour.note(&format!("the card Obsidian left at {}×{}: after an edit that changed nothing it's {}×{}", at.width, at.height, after.width, after.height));
    tour.shot("left-too-small-after-editing");

    // Too long to wrap, and a table.
    let link = tour.app.nodes.iter().position(|n| n.id == "link").expect("the link card");
    let at = tour.app.nodes[link].clone();
    tour.send(Message::FocusWorld(Point::new(at.x + 200.0, at.y + 250.0)));
    tour.settle();
    tour.shot("link-code-table");

    // The vault's note, embedded.
    let guide = tour.app.nodes.iter().position(|n| n.id == "guide").expect("the embedded note");
    let at = tour.app.nodes[guide].clone();
    tour.send(Message::FocusWorld(Point::new(at.x + at.width / 2.0, at.y + at.height / 2.0)));
    tour.settle();
    tour.shot("embedded-note");
    tour.swipe(tour.node_center(guide), 0.0, -200.0);
    tour.shot("embedded-note-scrolled-over");

    // Close up, and far away.
    let card = tour.app.nodes[article].clone();
    tour.send(Message::FocusWorld(Point::new(card.x + card.width / 2.0, card.y + 200.0)));
    tour.settle();
    tour.send(Message::ZoomBy { factor: 2.5, cursor: Point::new(720.0, 450.0) });
    tour.settle();
    std::thread::sleep(UPLOAD_GRACE);
    tour.shot("article-at-2.5x");
    tour.send(Message::ZoomBy { factor: 0.06 / tour.app.camera.zoom, cursor: Point::new(720.0, 450.0) });
    tour.settle();
    tour.shot("far-away");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Keyboard only: after launch, not one click — moving from card to card,
/// editing, adding linked cards, nudging, zooming, colouring from the palette,
/// finding and acting on what was found, and the settings form. What each key
/// changed (the selection, the editor, the camera) is in `notes.txt`.
fn keyboard_only() {
    let mut tour = Tour::new("keyboard_only", first_launch(trip_board));
    frame_trip(&mut tour);
    let first_line = |node: &CanvasNode| node.text.as_deref().unwrap_or_default().lines().next().unwrap_or_default().to_owned();
    let state = move |tour: &Tour| {
        let selected: Vec<String> = tour.app.selected.iter().filter_map(|&i| tour.app.nodes.get(i)).map(first_line).collect();
        let editing = tour.app.editing.as_ref().and_then(|e| tour.app.nodes.get(e.index)).map(first_line);
        let camera = tour.app.camera;
        format!(
            "selected {selected:?}, editing {editing:?}, camera at ({:.0}, {:.0}) zoom {:.2}, {} cards, {} links",
            camera.offset.x, camera.offset.y, camera.zoom, tour.app.nodes.len(), tour.app.edges.len()
        )
    };
    let key = |tour: &mut Tour, what: &str, press: &dyn Fn(&mut Tour)| {
        let before = state(tour);
        press(tour);
        let after = state(tour);
        tour.note(&format!("{what}: {}", if before == after { format!("nothing ({after})") } else { after }));
    };
    tour.note(&format!("start: {}", state(&tour)));
    tour.shot("start");

    // From card to card.
    key(&mut tour, "→ (nothing selected yet)", &|t| t.named(key::Named::ArrowRight));
    key(&mut tour, "→", &|t| t.named(key::Named::ArrowRight));
    key(&mut tour, "↓", &|t| t.named(key::Named::ArrowDown));
    tour.shot("moved-to-sintra");

    // Editing it, and the next card beside it.
    key(&mut tour, "Enter", &|t| t.named(key::Named::Enter));
    key(&mut tour, "typed \" By train.\"", &|t| t.type_text(" By train."));
    key(&mut tour, "Esc", &|t| t.named(key::Named::Escape));
    key(&mut tour, "Tab", &|t| t.named(key::Named::Tab));
    key(&mut tour, "typed \"Book the 9:10\"", &|t| t.type_text("Book the 9:10"));
    tour.shot("a-linked-note");
    key(&mut tour, "Esc", &|t| t.named(key::Named::Escape));
    tour.shot("after-the-linked-note");

    // Moving it, and taking that back.
    key(&mut tour, "⌥→ ×3", &|t| {
        t.hold(Modifiers::ALT);
        for _ in 0..3 {
            t.named(key::Named::ArrowRight);
        }
        t.hold(Modifiers::empty());
    });
    key(&mut tour, "⌘Z", &|t| t.chord(Modifiers::COMMAND, "z"));

    // Colouring from the palette.
    key(&mut tour, "⌘⇧P \"color green\"", &|t| t.palette("color green"));
    tour.shot("coloured");

    // Zooming.
    key(&mut tour, "⌘−", &|t| t.chord(Modifiers::COMMAND, "-"));
    key(&mut tour, "⌘=", &|t| t.chord(Modifiers::COMMAND, "="));
    key(&mut tour, "⌘0", &|t| t.chord(Modifiers::COMMAND, "0"));
    tour.shot("fitted");

    // Finding a card and acting on it.
    key(&mut tour, "⌘F \"eat\"", &|t| {
        t.chord(Modifiers::COMMAND, "f");
        t.type_text("eat");
    });
    key(&mut tour, "Esc (closes the find bar)", &|t| t.named(key::Named::Escape));
    key(&mut tour, "Enter", &|t| t.named(key::Named::Enter));
    tour.shot("editing-what-was-found");
    key(&mut tour, "Esc", &|t| t.named(key::Named::Escape));

    // The settings form.
    tour.palette("settings");
    tour.named(key::Named::Tab);
    tour.shot("settings-first-field");
    tour.named(key::Named::Tab);
    tour.shot("settings-second-field");
    tour.named(key::Named::Escape);
    tour.shot("end");
}

/// Small windows: the board in a window made smaller and smaller — a small
/// laptop's, a short one, a tall narrow one beside another app, and narrower
/// still — with each of the things that float over it: the palette, the
/// settings, the find bar, a card's menu near the edge, a picker and the
/// editor. One sheet per size; the welcome board at the narrowest last.
fn small_windows() {
    const SIZES: [(f32, f32); 4] = [(1024.0, 640.0), (720.0, 480.0), (480.0, 720.0), (360.0, 600.0)];
    let mut tour = Tour::new("small_windows", first_launch(sample_board));
    for (width, height) in SIZES {
        let size = Size::new(width, height);
        tour.resize(size);
        tour.settle();
        let mut sheet = tour.strip(&format!("{}x{}", width as u32, height as u32), Rectangle::new(Point::ORIGIN, size));
        tour.frame(&mut sheet, "board");
        tour.chord(Modifiers::COMMAND | Modifiers::SHIFT, "p");
        tour.frame(&mut sheet, "palette");
        tour.named(key::Named::Escape);
        tour.palette("settings");
        tour.frame(&mut sheet, "settings");
        tour.named(key::Named::Escape);
        tour.chord(Modifiers::COMMAND, "f");
        tour.type_text("trip");
        tour.frame(&mut sheet, "find");
        tour.named(key::Named::Escape);
        // The card nearest the right edge, whose menu has least room.
        let rightmost = (0..tour.app.nodes.len())
            .filter(|&i| {
                let at = tour.node_center(i);
                at.x > 0.0 && at.x < width && at.y > 0.0 && at.y < height
            })
            .max_by(|&a, &b| tour.node_center(a).x.total_cmp(&tour.node_center(b).x));
        if let Some(card) = rightmost {
            tour.right_click(tour.node_center(card));
            tour.frame(&mut sheet, "card menu");
            tour.named(key::Named::Escape);
        }
        tour.palette("theme");
        tour.frame(&mut sheet, "theme picker");
        tour.named(key::Named::Escape);
        if let Some(card) = rightmost {
            tour.send(Message::SelectNode(Some(card)));
            tour.named(key::Named::Enter);
            tour.frame(&mut sheet, "editing");
            tour.named(key::Named::Escape);
        }
        tour.save(sheet, 4);
        tour.note(&format!(
            "{}x{}: zoom {:.2} after the resize; {} of {} cards on screen",
            width as u32,
            height as u32,
            tour.app.camera.zoom,
            (0..tour.app.nodes.len()).filter(|&i| {
                let at = tour.node_center(i);
                at.x > 0.0 && at.x < width && at.y > 0.0 && at.y < height
            }).count(),
            tour.app.nodes.len(),
        ));
    }

    let mut tour = Tour::new("small_windows_welcome", first_launch(canvas_file::scratch_document));
    tour.resize(Size::new(360.0, 600.0));
    tour.settle();
    tour.shot("welcome-360x600");
}

const SCENES: &[(&str, fn())] = &[
    ("first_run", first_run),
    ("build_a_board", build_a_board),
    ("double_click_to_add", double_click_to_add),
    ("editing", editing),
    ("overlapping", overlapping),
    ("settings", settings),
    ("pickers", pickers),
    ("palette_settings", palette_settings),
    ("find", find),
    ("links", links),
    ("groups", groups),
    ("vault", vault),
    ("big_board", big_board),
    ("overlay", overlay),
    ("themes", themes),
    ("card_kinds", card_kinds),
    ("arranging", arranging),
    ("ai_setup", ai_setup),
    ("ai_answers", ai_answers),
    ("ai_errors", ai_errors),
    ("outside_changes", outside_changes),
    ("undo_and_clipboard", undo_and_clipboard),
    ("long_cards", long_cards),
    ("keyboard_only", keyboard_only),
    ("offscreen", offscreen),
    ("small_windows", small_windows),
];

#[test]
#[ignore = "renders to PNGs for a person to look at; needs a GPU adapter"]
fn tour() {
    let only = std::env::var("TOUR").ok();
    for (name, scene) in SCENES {
        if only.as_deref().is_none_or(|only| only.split(',').any(|o| o == *name)) {
            scene();
        }
    }
}
