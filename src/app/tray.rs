//! System tray: a menu-bar icon (macOS) / notification-area icon (Windows) via
//! `tray-icon`, and a StatusNotifierItem (Linux) via `ksni`. It offers Show/Hide,
//! creating/opening a board, Settings, and Quit; events are bridged into the app
//! as [`Message`]s through a subscription, like the global hotkey.
//!
//! The two backends differ enough — `tray-icon` needs the main thread and a live
//! run loop (so its icon is created from the app on first show), while `ksni`
//! runs its own async D-Bus service (spawned inside the subscription) — that
//! each is a separate submodule behind the same `events` surface.

#[cfg(any(target_os = "macos", windows))]
pub(super) use desktop::{create, events, set_monochrome};
#[cfg(target_os = "linux")]
pub(super) use linux::{events, set_monochrome};

#[cfg(any(target_os = "macos", windows))]
mod desktop {
    use std::cell::RefCell;

    use iced::futures::Stream;
    use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

    use crate::Message;

    thread_local! {
        /// The live tray handle, parked on the main thread for the process (the
        /// OS removes the icon on exit). Held rather than leaked so later updates
        /// — like the monochrome toggle — can reach it; macOS requires both
        /// creation and mutation on the main thread, which is where this lives.
        static TRAY: RefCell<Option<TrayIcon>> = const { RefCell::new(None) };
    }

    /// Builds the tray icon and its menu and parks it in `TRAY`. Must run on the
    /// main thread while the app is live (macOS `NSStatusItem` requirement).
    /// `monochrome` selects the macOS template-image icon; ignored elsewhere.
    pub(crate) fn create(monochrome: bool) {
        let menu = Menu::new();
        if let Err(error) = menu
            .append(&MenuItem::with_id("show", "Show / Hide ThinkBoard", true, None))
            .and_then(|()| menu.append(&MenuItem::with_id("new_board", "New Board…", true, None)))
            .and_then(|()| menu.append(&MenuItem::with_id("open_board", "Open Board…", true, None)))
            .and_then(|()| menu.append(&MenuItem::with_id("settings", "Settings…", true, None)))
            .and_then(|()| menu.append(&PredefinedMenuItem::separator()))
            .and_then(|()| menu.append(&MenuItem::with_id("quit", "Quit ThinkBoard", true, None)))
        {
            eprintln!("tray: menu build failed: {error}");
            return;
        }

        let builder = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("ThinkBoard")
            .with_icon(icon(monochrome));
        // macOS menu-bar icons open their menu on a single left click, and can be
        // drawn as a monochrome template (the silhouette asset above) that tracks
        // the menu bar's tint. On Windows the menu is the right-click context menu
        // and a double-click summons the app (see events()), so a left click
        // shouldn't open the menu; there's no template mode.
        #[cfg(target_os = "macos")]
        let builder = builder
            .with_menu_on_left_click(true)
            .with_icon_as_template(monochrome);
        #[cfg(windows)]
        let builder = builder.with_menu_on_left_click(false);

        match builder.build() {
            Ok(tray) => TRAY.with(|slot| *slot.borrow_mut() = Some(tray)),
            Err(error) => eprintln!("tray: could not create icon: {error}"),
        }
    }

    /// Retints the live menu-bar icon: the monochrome template silhouette
    /// (macOS) or the colored mark. Swaps the image *and* the template flag —
    /// flagging the colored icon as a template is what draws the "white blob".
    /// A no-op before the tray exists, and on Windows (whose notification-area
    /// icons have no template mode). Main thread only.
    pub(crate) fn set_monochrome(monochrome: bool) {
        TRAY.with(|slot| {
            if let Some(tray) = slot.borrow().as_ref()
                && let Err(error) = tray.set_icon_with_as_template(Some(icon(monochrome)), monochrome)
            {
                eprintln!("tray: could not update icon: {error}");
            }
        });
    }

    /// The tray icon: on macOS a monochrome template silhouette when
    /// `monochrome`, otherwise the colored mark (also used on Windows, which has
    /// no template mode).
    fn icon(monochrome: bool) -> Icon {
        #[cfg(target_os = "macos")]
        let (w, h, rgba) = if monochrome {
            super::decode_png(include_bytes!("../../assets/thinkboard_tray_template.png"))
        } else {
            super::tray_icon_rgba()
        };
        #[cfg(not(target_os = "macos"))]
        let (w, h, rgba) = {
            let _ = monochrome;
            super::tray_icon_rgba()
        };
        Icon::from_rgba(rgba, w, h).expect("valid tray icon")
    }

    /// Forwards tray menu clicks into the app as messages.
    pub(crate) fn events() -> impl Stream<Item = Message> {
        iced::stream::channel(
            8,
            |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
                use iced::futures::{SinkExt, StreamExt};

                let (sender, mut receiver) = iced::futures::channel::mpsc::unbounded();

                // Windows: a double-click on the notification-area icon summons
                // the app directly (macOS relies on the menu and the hotkey).
                #[cfg(windows)]
                {
                    use tray_icon::{MouseButton, TrayIconEvent};
                    let sender = sender.clone();
                    TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
                        if let TrayIconEvent::DoubleClick {
                            button: MouseButton::Left,
                            ..
                        } = event
                        {
                            let _ = sender.unbounded_send(Message::ToggleOverlay);
                        }
                    }));
                }

                MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
                    let message = match event.id.0.as_str() {
                        "show" => Some(Message::ToggleOverlay),
                        "new_board" => Some(Message::CanvasAction("new_board".to_owned())),
                        "open_board" => Some(Message::CanvasAction("open_board".to_owned())),
                        "settings" => Some(Message::OpenSettings),
                        "quit" => Some(Message::Quit),
                        _ => None,
                    };
                    if let Some(message) = message {
                        let _ = sender.unbounded_send(message);
                    }
                }));

                while let Some(message) = receiver.next().await {
                    let _ = output.send(message).await;
                }
            },
        )
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use iced::futures::Stream;
    use iced::futures::channel::mpsc::UnboundedSender;
    use ksni::TrayMethods;

    use crate::Message;

    /// The monochrome menu-bar icon is a macOS-only (template-image) concept, so
    /// this is a no-op — it just keeps the toggle's apply path cross-platform.
    pub(crate) fn set_monochrome(_monochrome: bool) {}

    struct Tray {
        sender: UnboundedSender<Message>,
    }

    impl ksni::Tray for Tray {
        fn id(&self) -> String {
            "thinkboard".into()
        }
        fn title(&self) -> String {
            "ThinkBoard".into()
        }
        // Clicking the icon toggles the overlay directly, skipping the menu. The
        // gesture that triggers this (single- or double-click) is chosen by the
        // desktop's SNI host, not us; the right-click menu still works either way.
        fn activate(&mut self, _x: i32, _y: i32) {
            let _ = self.sender.unbounded_send(Message::ToggleOverlay);
        }
        fn icon_pixmap(&self) -> Vec<ksni::Icon> {
            let (width, height, mut data) = super::tray_icon_rgba();
            // ksni wants ARGB; the decoder produces RGBA.
            for px in data.chunks_exact_mut(4) {
                px.rotate_right(1);
            }
            vec![ksni::Icon { width: width as i32, height: height as i32, data }]
        }
        fn menu(&self) -> Vec<ksni::menu::MenuItem<Self>> {
            use ksni::menu::{MenuItem, StandardItem};
            vec![
                StandardItem {
                    label: "Show / Hide ThinkBoard".into(),
                    activate: Box::new(|tray: &mut Self| {
                        let _ = tray.sender.unbounded_send(Message::ToggleOverlay);
                    }),
                    ..Default::default()
                }
                .into(),
                StandardItem {
                    label: "New Board…".into(),
                    activate: Box::new(|tray: &mut Self| {
                        let _ = tray
                            .sender
                            .unbounded_send(Message::CanvasAction("new_board".to_owned()));
                    }),
                    ..Default::default()
                }
                .into(),
                StandardItem {
                    label: "Open Board…".into(),
                    activate: Box::new(|tray: &mut Self| {
                        let _ = tray
                            .sender
                            .unbounded_send(Message::CanvasAction("open_board".to_owned()));
                    }),
                    ..Default::default()
                }
                .into(),
                StandardItem {
                    label: "Settings…".into(),
                    activate: Box::new(|tray: &mut Self| {
                        let _ = tray.sender.unbounded_send(Message::OpenSettings);
                    }),
                    ..Default::default()
                }
                .into(),
                MenuItem::Separator,
                StandardItem {
                    label: "Quit ThinkBoard".into(),
                    activate: Box::new(|tray: &mut Self| {
                        let _ = tray.sender.unbounded_send(Message::Quit);
                    }),
                    ..Default::default()
                }
                .into(),
            ]
        }
    }

    /// Spawns the StatusNotifierItem D-Bus service and forwards its menu clicks
    /// into the app. The handle is held for the subscription's lifetime.
    pub(crate) fn events() -> impl Stream<Item = Message> {
        iced::stream::channel(
            8,
            |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
                use iced::futures::{SinkExt, StreamExt};

                let (sender, mut receiver) = iced::futures::channel::mpsc::unbounded();
                match (Tray { sender }).spawn().await {
                    Ok(_handle) => {
                        while let Some(message) = receiver.next().await {
                            let _ = output.send(message).await;
                        }
                    }
                    Err(error) => {
                        eprintln!("tray: could not start StatusNotifierItem: {error}");
                    }
                }
            },
        )
    }
}

/// The colored tray icon as raw RGBA8 pixels, decoded from the PNG embedded from
/// `assets/`. Returns `(width, height, rgba)`; callers reorder the channels for
/// their backend (tray-icon wants RGBA, ksni wants ARGB).
fn tray_icon_rgba() -> (u32, u32, Vec<u8>) {
    decode_png(include_bytes!("../../assets/thinkboard_tray.png"))
}

/// Decodes an embedded PNG to raw RGBA8 `(width, height, rgba)`.
fn decode_png(png: &[u8]) -> (u32, u32, Vec<u8>) {
    let image = image::load_from_memory(png)
        .expect("embedded tray icon decodes")
        .into_rgba8();
    let (w, h) = image.dimensions();
    (w, h, image.into_raw())
}
