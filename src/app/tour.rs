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

use std::path::PathBuf;
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use iced::advanced::renderer::Headless;
use iced::futures::stream::{BoxStream, StreamExt};
use iced::keyboard::{self, Key, Modifiers, key};
use iced::{Event, Rectangle, mouse};
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
    out: PathBuf,
    shots: usize,
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
            out,
            shots: 0,
        };
        // The board learns its size from the first event it sees.
        tour.redraw();
        tour
    }

    // --- The runtime, as far as these scenes need one ---

    /// Runs `events` through the widget tree and the raw-event handlers, then
    /// applies everything they published.
    fn feed(&mut self, events: Vec<Event>) {
        let cursor = mouse::Cursor::Available(self.cursor);
        let mut messages = Vec::new();
        let statuses = {
            let Self { app, renderer, cache, .. } = self;
            let mut ui = UserInterface::build(app.view(), VIEWPORT, cache.take().unwrap_or_default(), renderer);
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

    /// Delivers one message, as the runtime would — keeping its task.
    fn send(&mut self, message: Message) {
        if let Some(stream) = task::into_stream(self.app.update(message)) {
            self.tasks.push(stream);
        }
    }

    /// Polls every unfinished task once; true if any of them moved.
    fn pump(&mut self) -> bool {
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
        let Self { app, renderer, cache, .. } = self;
        let mut ui = UserInterface::build(app.view(), VIEWPORT, cache.take().unwrap_or_default(), renderer);
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
            if self.app.animating() {
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
        let theme = startup::theme(&self.app);
        let style = startup::style(&self.app, &theme);
        let cursor = mouse::Cursor::Available(self.cursor);
        let Self { app, renderer, cache, .. } = self;
        let mut ui = UserInterface::build(app.view(), VIEWPORT, cache.take().unwrap_or_default(), renderer);
        let redraw = Event::Window(window::Event::RedrawRequested(Instant::now()));
        let _ = ui.update(&[redraw], cursor, renderer, &mut clipboard::Null, &mut Vec::new());
        ui.draw(renderer, &theme, &renderer::Style { text_color: style.text_color }, cursor);
        *cache = Some(ui.into_cache());
    }

    /// Draws the tree offscreen and reads the frame back as RGBA.
    fn render(&mut self) -> (iced::Size<u32>, Vec<u8>) {
        // Whatever a redraw publishes, `shot`'s own `redraw` already delivered.
        self.draw();
        let style = startup::style(&self.app, &startup::theme(&self.app));
        let size = iced::Size::new(
            (VIEWPORT.width * self.scale).round() as u32,
            (VIEWPORT.height * self.scale).round() as u32,
        );
        (size, Headless::screenshot(&mut self.renderer, size, self.scale, style.background_color))
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
        let n = strip.frames.len() + 1;
        stamp_number(&mut tile, n);
        strip.frames.push(tile);
        strip.legend.push(format!("{n:>3}  {label:<28} {}", self.editing_state()));
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

/// An app as a first launch builds it — default config, the theme that config
/// resolves to, nothing written to disk — showing the board `document` makes
/// for that theme. Resolved in `App::new`'s order: the theme's fonts before
/// `use_font` reads the database.
fn first_launch(document: impl FnOnce(&ColorTheme) -> CanvasDocument) -> App {
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
        base_path: PathBuf::from("."),
        file_mtime: None,
        scratch: true,
        hotkey_manager: None,
        registered_hotkey: None,
        windowed: true,
    })
}

fn empty_document(_: &ColorTheme) -> CanvasDocument {
    CanvasDocument { nodes: Vec::new(), edges: Vec::new(), extra: serde_json::Map::new() }
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

    tour.chord(Modifiers::COMMAND | Modifiers::SHIFT, "p");
    tour.shot("command-palette");
    tour.type_text("theme");
    tour.shot("command-palette-filtered");
    tour.named(key::Named::Escape);
    tour.shot("after-palette");
}

/// A newcomer's first board: empty, then built up by hand — notes, an edit, a
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
    // Whatever the menu calls it, adding a note is a `CreateNode` at the click.
    let world = tour.app.camera.to_world(Point::new(560.0, 380.0));
    tour.send(Message::CloseContextMenu);
    tour.send(Message::CreateNode { world });
    tour.shot("new-note");

    tour.type_text("# Trip to Lisbon\n\nPlan the **long weekend** in May.");
    tour.shot("typing");
    tour.named(key::Named::Escape);
    tour.shot("first-note");

    // Drag out of the note's right-hand port onto empty space.
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

/// The opt-in Obsidian gesture: double-click empty space for a note. A window
/// is never dismissed by a click, so here it works with the setting alone.
fn double_click_to_add() {
    let mut tour = Tour::new("double_click_to_add", first_launch(empty_document));
    tour.app.config.double_click_adds_note = true;
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

/// Writing in a note, a frame per step: what the card and the editor do as
/// lines break, wrap, and outgrow the card — and what changes when editing
/// ends and starts again. Read the sheets with their `.txt` legends: the
/// numbers say which of card, editor box and content fell behind.
fn editing() {
    let mut tour = Tour::new("editing", first_launch(empty_document));

    // A note made the way the hint says: right-click, "Add node".
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
    let target = tour.find("Hover a note");
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

const SCENES: &[(&str, fn())] = &[
    ("first_run", first_run),
    ("build_a_board", build_a_board),
    ("double_click_to_add", double_click_to_add),
    ("editing", editing),
    ("overlapping", overlapping),
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
