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
pub(super) use desktop::{create, events};
#[cfg(target_os = "linux")]
pub(super) use linux::events;

#[cfg(any(target_os = "macos", windows))]
mod desktop {
    use iced::futures::Stream;
    use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

    use crate::Message;

    /// Builds the tray icon and its menu. Must run on the main thread while the
    /// app is live (macOS `NSStatusItem` requirement). The returned handle must
    /// be kept alive for the icon to stay visible.
    pub(crate) fn create() -> Option<TrayIcon> {
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
            return None;
        }

        let builder = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("ThinkBoard")
            .with_icon(icon());
        // macOS menu-bar icons open their menu on a single left click. On Windows
        // the menu is the right-click context menu and a double-click summons the
        // app (see events()), so a left click shouldn't open the menu.
        #[cfg(target_os = "macos")]
        let builder = builder.with_menu_on_left_click(true);
        #[cfg(windows)]
        let builder = builder.with_menu_on_left_click(false);

        match builder.build() {
            Ok(tray) => Some(tray),
            Err(error) => {
                eprintln!("tray: could not create icon: {error}");
                None
            }
        }
    }

    fn icon() -> Icon {
        let (w, h, rgba) = super::rounded_square(|a, r, g, b| [r, g, b, a]);
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
            let (width, height, data) = super::rounded_square(|a, r, g, b| [a, r, g, b]);
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

/// A 32×32 rounded-square placeholder icon (until real assets land). `pack(a, r,
/// g, b)` orders the channels for the backend: RGBA for tray-icon, ARGB for
/// ksni. Returns `(width, height, bytes)`.
fn rounded_square(pack: impl Fn(u8, u8, u8, u8) -> [u8; 4]) -> (u32, u32, Vec<u8>) {
    const N: u32 = 32;
    const M: f32 = 3.0; // margin
    const R: f32 = 7.0; // corner radius
    let (lo, hi) = (M, N as f32 - M);

    let mut bytes = Vec::with_capacity((N * N * 4) as usize);
    for y in 0..N {
        for x in 0..N {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let (cx, cy) = (px.clamp(lo + R, hi - R), py.clamp(lo + R, hi - R));
            let inside = (px - cx).powi(2) + (py - cy).powi(2) <= R * R;
            let pixel = if inside {
                pack(0xff, 0x4a, 0x7c, 0xff)
            } else {
                pack(0, 0, 0, 0)
            };
            bytes.extend_from_slice(&pixel);
        }
    }
    (N, N, bytes)
}
