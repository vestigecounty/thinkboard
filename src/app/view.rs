//! Everything the app draws: the layered board scene, the in-place editor
//! overlay, the context menus, and the floating find bar / settings modal.
//!
//! The board itself is one `iced` canvas widget ([`Board`]); this module stacks
//! it between the backdrop dim and the GPU shadow layer, then pushes the
//! floating chrome on top.

use iced::widget::{
    Column, Row, Space, button, canvas as iced_canvas, column, container, mouse_area, row, shader,
    stack, text, text_editor, text_input,
};
use iced::{Element, Fill, Length, Padding, Point, Rectangle, Size, Theme};

use super::search::Search;
use super::{App, ContextMenuKind, Message, PICKER_REVEAL, chrome, command, settings, startup};
use crate::canvas::board::{self, Board};
use crate::canvas::hue_picker::HuePicker;
use crate::canvas::{camera, md_edit, shadows};
use crate::document::IntoColor;
use crate::model;

use super::arrange::node_world_rect;

/// Node color swatches offered in the context menu: Obsidian's canonical
/// canvas presets `"1"`–`"6"` (red, orange, yellow, green, cyan, purple),
/// stored and rendered through `model::parse_color` so they round-trip
/// natively and stay consistent with the display swatch.
pub(super) const NODE_PALETTE: [&str; 6] = ["1", "2", "3", "4", "5", "6"];

/// How much larger the whole scene starts during the entrance (and grows
/// to during the exit): the classic Dashboard full-scene zoom.
const SCENE_ZOOM: f32 = 0.25;

/// Transparent, text-only styling for chrome buttons (the find bar's ‹ › ×),
/// using theme colors: `base` normally, `accent` (with a faint wash) on hover.
fn chrome_button_style(
    status: button::Status,
    base: iced::Color,
    accent: iced::Color,
) -> button::Style {
    button::Style {
        background: matches!(status, button::Status::Hovered | button::Status::Pressed)
            .then(|| iced::Background::Color(iced::Color { a: 0.14, ..accent })),
        text_color: match status {
            button::Status::Disabled => iced::Color { a: 0.35, ..base },
            button::Status::Hovered | button::Status::Pressed => accent,
            button::Status::Active => base,
        },
        border: iced::Border {
            radius: chrome::CONTROL_RADIUS.into(),
            ..Default::default()
        },
        ..button::Style::default()
    }
}

/// Chromeless editor: the rounded card drawn by the canvas shows through.
fn editor_style(_theme: &Theme, _status: text_editor::Status) -> text_editor::Style {
    text_editor::Style {
        background: iced::Color::TRANSPARENT.into(),
        border: iced::Border::default(),
        placeholder: iced::Color::from_rgba(1.0, 1.0, 1.0, 0.4),
        value: board::TEXT_COLOR,
        selection: iced::Color::from_rgba(0.35, 0.62, 1.0, 0.4),
    }
}

/// A color swatch in the node menu: a rounded, filled square whose border
/// brightens on hover.
fn swatch_style(color: iced::Color, status: button::Status) -> button::Style {
    let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
    button::Style {
        background: Some(color.into()),
        border: iced::Border {
            radius: chrome::SWATCH_RADIUS.into(),
            width: if hovered { 2.0 } else { 1.0 },
            color: if hovered {
                iced::Color::WHITE
            } else {
                iced::Color::from_rgba(1.0, 1.0, 1.0, 0.35)
            },
        },
        ..button::Style::default()
    }
}

/// The hue-picker (wheel) swatch: same rounded shape and hover border as a
/// color swatch, but filled with a smooth rainbow.
fn wheel_style(status: button::Status) -> button::Style {
    let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
    let mut rainbow = iced::gradient::Linear::new(iced::Radians(std::f32::consts::FRAC_PI_2));
    for i in 0..=6 {
        rainbow = rainbow.add_stop(i as f32 / 6.0, model::hsv(i as f32 / 6.0, 1.0, 1.0).into_color());
    }
    button::Style {
        background: Some(iced::Background::Gradient(rainbow.into())),
        border: iced::Border {
            radius: chrome::SWATCH_RADIUS.into(),
            width: if hovered { 2.0 } else { 1.0 },
            color: if hovered {
                iced::Color::WHITE
            } else {
                iced::Color::from_rgba(1.0, 1.0, 1.0, 0.35)
            },
        },
        ..button::Style::default()
    }
}

/// A small square toggle chip (the auto-layout direction arrows): a subtle
/// filled button whose fill and border brighten on hover.
fn chip_style(_theme: &Theme, status: button::Status) -> button::Style {
    let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
    button::Style {
        background: Some(
            iced::Color::from_rgba(1.0, 1.0, 1.0, if hovered { 0.16 } else { 0.06 }).into(),
        ),
        text_color: board::TEXT_COLOR,
        border: iced::Border {
            radius: 5.0.into(),
            width: 1.0,
            color: iced::Color::from_rgba(1.0, 1.0, 1.0, if hovered { 0.5 } else { 0.2 }),
        },
        ..button::Style::default()
    }
}

impl App {
    /// The camera used for drawing: during the overlay entrance/exit the
    /// whole scene scales around the viewport center — the Dashboard
    /// full-scene zoom. `screen = center + offset + world*zoom`, so scaling
    /// both `offset` and `zoom` scales the scene about the center.
    fn display_camera(&self) -> camera::Camera {
        let mut camera = self.camera;
        if self.overlay && self.reveal < 1.0 {
            let scale = 1.0 + SCENE_ZOOM * (1.0 - camera::ease_out_cubic(self.reveal));
            camera.zoom *= scale;
            camera.offset = camera.offset * scale;
        }
        camera
    }

    pub(super) fn view(&self) -> Element<'_, Message> {
        let camera = self.display_camera();
        let content_alpha = if self.overlay {
            camera::ease_out_cubic(self.reveal).clamp(0.0, 1.0)
        } else {
            1.0
        };

        // Bottom layer: the dim (overlay) or dark fill (windowed).
        let backdrop_color = if self.overlay {
            iced::Color {
                a: self.config.overlay_dim * content_alpha,
                ..iced::Color::BLACK
            }
        } else {
            board::BACKGROUND
        };
        let backdrop = container(Space::new().width(Fill).height(Fill))
            .width(Fill)
            .height(Fill)
            .style(move |_| container::Style {
                background: Some(backdrop_color.into()),
                ..container::Style::default()
            });

        let shadow_layer = shader(shadows::ShadowLayer {
            instances: self.shadow_instances(camera, content_alpha),
        })
        .width(Fill)
        .height(Fill);

        let board = iced_canvas(Board {
            nodes: &self.nodes,
            node_index: self
                .nodes
                .iter()
                .enumerate()
                .map(|(index, node)| (node.id.as_str(), index))
                .collect(),
            edges: &self.edges,
            // Links grouped by the port they leave, so the ones sharing a port
            // can share a trunk.
            ports: self.edges.iter().enumerate().fold(
                std::collections::HashMap::new(),
                |mut ports, (index, edge)| {
                    ports
                        .entry((edge.from_node.as_str(), edge.from_side.as_str()))
                        .or_insert_with(Vec::new)
                        .push(index);
                    ports
                },
            ),
            images: &self.images,
            notes: &self.notes,
            clusters: &self.clusters.list,
            cluster_links: &self.clusters.links,
            cluster_snapshots: &self.clusters.snapshots,
            config: &self.config,
            theme: &self.theme,
            camera,
            top_safe_inset: self.top_safe_inset,
            cache: &self.cache,
            editing: self.editing.as_ref().map(|editing| editing.index),
            overlay: self.overlay,
            // The settings modal and the two pickers all own input while open,
            // so board shortcuts/scroll don't fire underneath them.
            modal: self.settings_overlay
                || self.command_palette.is_some()
                || self.vault_browser.is_some()
                || self.theme_picker.is_some()
                || self.font_picker.is_some()
                || self.model_picker.is_some(),
            show_fps: self.show_fps,
            spawning: &self.spawning,
            text_snapshots: &self.rasters.text,
            staging_snapshots: &self.rasters.staging,
            node_scroll: &self.node_scroll,
            reveal: self.reveal,
            selected: &self.selected,
            selected_edge: self.selected_edge,
            hovered_edge: self.hovered_edge,
            search: self.search.as_ref().map(|s| board::SearchView {
                query: &s.query,
                current_hit: s.matches.get(s.current).map(|current| {
                    let ordinal = s
                        .matches
                        .iter()
                        .filter(|m| m.node == current.node)
                        .position(|m| m.start == current.start)
                        .unwrap_or(0);
                    (current.node, ordinal)
                }),
            }),
            ai_flows: &self.ai.flows,
            ghosts: &self.ghosts.cards,
            ghost_fade: self.ghost_fade(),
            ghost_fade_in: self.ghost_fade_in(),
            ghost_hovered: self.ghosts.hovered,
        })
        .width(Fill)
        .height(Fill);

        let mut layers = stack![backdrop, shadow_layer, board];

        if let Some(editor) = self.editor_overlay(camera) {
            layers = layers.push(editor);
        }
        if let Some(menu) = self.context_menu_overlay() {
            layers = layers.push(menu);
        }
        if let Some(search) = &self.search {
            layers = layers.push(self.search_bar(search));
        }
        if let Some(palette) = &self.command_palette {
            layers = layers.push(command::view(self, palette));
        }
        if let Some(browser) = &self.vault_browser {
            layers = layers.push(self.vault_browser_view(browser));
        }
        if let Some(picker) = &self.model_picker {
            layers = layers.push(self.model_picker_view(picker));
        }
        if let Some(picker) = &self.font_picker {
            layers = layers.push(self.font_picker_view(picker));
        }
        if let Some(picker) = &self.theme_picker {
            layers = layers.push(self.theme_picker_view(picker));
        }
        if self.settings_overlay {
            layers = layers.push(self.settings_overlay_view());
        }

        layers.into()
    }

    /// Analytic GPU shadows, drawn above the dim and below the cards. Node and
    /// cluster shadows crossfade together with their cards.
    fn shadow_instances(&self, camera: camera::Camera, content_alpha: f32) -> Vec<shadows::Instance> {
        let cluster_weight = if self.clusters.list.is_empty() {
            0.0
        } else {
            board::cluster_opacity(camera.zoom)
        };
        let mut instances: Vec<shadows::Instance> = Vec::new();

        if cluster_weight < 1.0 {
            let node_alpha = 0.6 * content_alpha * (1.0 - cluster_weight);
            // Frames get no shadow: they sit behind the cards they enclose, so
            // one would fall *across* their own contents.
            let cards = self.nodes.iter().enumerate().filter(|(_, node)| !node.is_group());
            instances.extend(cards.map(|(index, node)| {
                let mut rect = camera.rect_to_screen(node_world_rect(node));
                let mut alpha = node_alpha;
                // Track the spawn zoom-in of freshly created nodes.
                if let Some(started) = self.spawning.get(&index) {
                    let t = (started.elapsed().as_secs_f32()
                        / self.config.anim_secs(super::SPAWN_DURATION.as_secs_f32()))
                    .clamp(0.0, 1.0);
                    let scale = 0.2 + 0.8 * camera::ease_out_back(t);
                    let center = rect.center();
                    rect = Rectangle::new(
                        Point::new(
                            center.x - rect.width * scale / 2.0,
                            center.y - rect.height * scale / 2.0,
                        ),
                        Size::new(rect.width * scale, rect.height * scale),
                    );
                    alpha *= camera::ease_out_cubic(t);
                }
                shadows::Instance {
                    pos: [rect.x + 5.0 * camera.zoom, rect.y + 7.0 * camera.zoom],
                    size: [rect.width, rect.height],
                    corner_radius: 8.0 * camera.zoom,
                    blur: (22.0 * camera.zoom).max(0.5),
                    alpha,
                    _pad: 0.0,
                }
            }));
        }

        if cluster_weight > 0.0 {
            let cluster_alpha = 0.6 * content_alpha * cluster_weight;
            instances.extend(self.clusters.list.iter().map(|cluster| {
                let rect = camera.rect_to_screen(cluster.bounds);
                shadows::Instance {
                    pos: [rect.x + 5.0 * camera.zoom, rect.y + 7.0 * camera.zoom],
                    size: [rect.width, rect.height],
                    corner_radius: 12.0 * camera.zoom,
                    blur: (22.0 * camera.zoom).max(0.5),
                    alpha: cluster_alpha,
                    _pad: 0.0,
                }
            }));
        }
        instances
    }

    /// The in-place editor, positioned over the node being edited. Its box is
    /// computed by [`App::editor_box`], which deferred caret placement also
    /// reads — so the two never drift apart.
    fn editor_overlay(&self, camera: camera::Camera) -> Option<Element<'_, Message>> {
        let editing = self.editing.as_ref()?;
        let b = self.editor_box(editing.index)?;
        let zoom = camera.zoom;

        // The editor widget has no alignment API; the vendored iced_graphics
        // reads this to align its lines (center vs left) to match read mode.
        iced_graphics::text::editor::set_editor_centered(b.centered);

        let editor = text_editor(&editing.content)
            .on_action(Message::EditorAction)
            .key_binding(editor_key_binding)
            .font(md_edit::Palette::from_theme(&self.theme).base_font())
            .size(b.font_size * zoom)
            .line_height(iced::advanced::text::LineHeight::Relative(1.2))
            .padding(0)
            .width(b.width)
            .height(Length::Fixed(b.height))
            .style(editor_style)
            // Obsidian-style live styling: markers stay visible, content is
            // styled (bold/italic/code/links/headings) as you type.
            .highlight_with::<md_edit::MdHighlighter>(
                md_edit::Palette::from_theme(&self.theme),
                md_edit::to_format,
            );

        Some(
            container(editor)
                .width(Fill)
                .height(Fill)
                .padding(Padding {
                    top: b.origin.y,
                    left: b.origin.x,
                    right: 0.0,
                    bottom: 0.0,
                })
                .into(),
        )
    }

    /// The open right-click menu (canvas / node / link), clamped to stay
    /// on-screen, over a full-screen catcher that dismisses it.
    fn context_menu_overlay(&self) -> Option<Element<'_, Message>> {
        let menu = self.context_menu.as_ref()?;

        // Shared chrome styles (captured by Copy, so each `.style()` reuses
        // them) — the menu recolors with the theme like the palette does.
        let theme = self.theme;
        let item_style = move |_t: &Theme, s: button::Status| chrome::menu_item(&theme, s, false);
        let delete_style = move |_t: &Theme, s: button::Status| chrome::danger_item(&theme, s);
        let sep_style = move |_t: &Theme| chrome::separator(&theme);
        let sep = move || container(Space::new().width(Fill).height(1.0)).style(sep_style);

        // A menu row with a fixed leading "check column": toggle rows draw a
        // real, centered checkbox; plain rows reserve the same column so every
        // label lines up (rather than the old baseline ☑/☐ text glyphs, which
        // sat small on the baseline and left non-toggle rows un-indented).
        let (accent, muted, font) = (theme.accent, theme.text_muted, theme.ui_font());
        let item = move |check: Option<bool>, label: &'static str, msg: Message| -> Element<Message> {
            const BOX: f32 = 15.0;
            let gutter: Element<Message> = match check {
                Some(on) => {
                    // Black or white tick, whichever contrasts with the accent fill.
                    let lum = 0.299 * accent.r + 0.587 * accent.g + 0.114 * accent.b;
                    let tick = if lum > 0.6 { iced::Color::BLACK } else { iced::Color::WHITE };
                    let mark: Element<Message> = if on {
                        text("✓").size(11).font(font).color(tick).into()
                    } else {
                        Space::new().into()
                    };
                    container(mark)
                        .width(BOX)
                        .height(BOX)
                        .align_x(iced::alignment::Horizontal::Center)
                        .align_y(iced::alignment::Vertical::Center)
                        .style(move |_t: &Theme| container::Style {
                            // Off: a clearly-visible empty box (muted border +
                            // faint fill) so the row still reads as a toggle.
                            background: Some(if on {
                                accent.into()
                            } else {
                                iced::Color { a: 0.08, ..muted }.into()
                            }),
                            border: iced::Border {
                                radius: chrome::SWATCH_RADIUS.into(),
                                width: 1.0,
                                color: if on { accent } else { muted },
                            },
                            ..container::Style::default()
                        })
                        .into()
                }
                None => Space::new().width(BOX).height(BOX).into(),
            };
            button(
                row![gutter, text(label).size(14).font(font)]
                    .spacing(8)
                    .align_y(iced::alignment::Vertical::Center),
            )
            .width(Fill)
            .on_press(msg)
            .padding([6, 12])
            .style(item_style)
            .into()
        };

        let content: Element<Message> = match menu.kind {
            ContextMenuKind::Canvas { world } => column![
                item(None, "Add node", Message::CreateNode { world }),
                sep(),
                item(Some(self.config.auto_layout), "Auto-layout", Message::ToggleAutoLayout),
                sep(),
                item(Some(self.config.snap_to_grid), "Snap to grid", Message::ToggleSnap),

                sep(),
                item(
                    Some(self.config.ai_completions),
                    "AI suggestions",
                    Message::ToggleAiCompletions,
                ),
                sep(),
                item(None, "Paste", Message::Paste { world }),
            ]
            .width(Length::Fixed(200.0))
            .spacing(4)
            .into(),
            ContextMenuKind::Node => self.node_menu(item_style, delete_style, sep),
            ContextMenuKind::Link => button(text("Delete").size(14))
                .on_press(Message::DeleteSelected)
                .padding([6, 12])
                .style(delete_style)
                .into(),
        };

        let panel = container(content)
            .padding(4)
            .style(move |_t: &Theme| chrome::panel(&theme, theme.elevated()));
        // Keep the menu on-screen: shift it in from the right/bottom edges so a
        // click near a corner doesn't clip it.
        let size = self.context_menu_size();
        let viewport = self.camera.viewport;
        const EDGE: f32 = 8.0;
        let left = menu
            .screen
            .x
            .min((viewport.width - size.width - EDGE).max(0.0))
            .max(0.0);
        let top = menu
            .screen
            .y
            .min((viewport.height - size.height - EDGE).max(0.0))
            .max(0.0);

        // A full-screen catcher below the menu dismisses it on any click
        // outside the menu items.
        let catcher =
            mouse_area(Space::new().width(Fill).height(Fill)).on_press(Message::CloseContextMenu);
        Some(
            stack![
                catcher,
                container(panel).width(Fill).height(Fill).padding(Padding {
                    top,
                    left,
                    right: 0.0,
                    bottom: 0.0,
                })
            ]
            .into(),
        )
    }

    /// The node menu's body: the color swatch row (or the expanded hue picker),
    /// then Ask AI, auto-layout and Delete.
    fn node_menu<'a>(
        &'a self,
        item_style: impl Fn(&Theme, button::Status) -> button::Style + Copy + 'a,
        delete_style: impl Fn(&Theme, button::Status) -> button::Style + Copy + 'a,
        sep: impl Fn() -> container::Container<'a, Message> + Copy + 'a,
    ) -> Element<'a, Message> {
        // Swatch/wheel size and gap; the hue bar matches the swatch height so
        // expanding the picker doesn't change the row height.
        const SW: f32 = 24.0;
        const GAP: f32 = 6.0;

        let delete_label = if self.selected.len() > 1 {
            format!("Delete {} nodes", self.selected.len())
        } else {
            "Delete".to_owned()
        };
        let delete = button(text(delete_label).size(14))
            .width(Fill)
            .on_press(Message::DeleteSelected)
            .padding([6, 12])
            .style(delete_style);

        // Fixed menu width — presets + optional extra + wheel — so expanding
        // the picker never resizes the menu.
        let extra = self
            .extra_swatch
            .as_deref()
            .filter(|e| !NODE_PALETTE.contains(e));
        let menu_width = self.node_menu_width();

        let body: Element<Message> = if self.color_picker_open {
            // A smooth hue bar, same height as the swatch row, that unrolls
            // from center when the picker opens.
            let reveal = match self.picker_anim {
                Some(start) => camera::ease_out_cubic(
                    (start.elapsed().as_secs_f32() / self.config.anim_secs(PICKER_REVEAL))
                        .clamp(0.0, 1.0),
                ),
                None => 1.0,
            };
            iced_canvas(HuePicker { reveal })
                .width(Fill)
                .height(Length::Fixed(SW))
                .into()
        } else {
            let mut colors: Vec<String> = NODE_PALETTE.iter().map(|s| (*s).to_owned()).collect();
            if let Some(extra) = extra {
                colors.push(extra.to_owned());
            }
            // Leading "default" swatch (the default card color) clears the
            // node's color.
            let default_color = self.theme.card_fill;
            let mut row = Row::new()
                .push(
                    button(Space::new().width(SW).height(SW))
                        .padding(0)
                        .on_press(Message::SetNodeColor(None))
                        .style(move |_theme, status| swatch_style(default_color, status)),
                )
                .spacing(GAP);
            for hex in colors {
                let color = model::parse_color(&hex)
                    .map(IntoColor::into_color)
                    .unwrap_or(iced::Color::WHITE);
                row = row.push(
                    button(Space::new().width(SW).height(SW))
                        .padding(0)
                        .on_press(Message::SetNodeColor(Some(hex)))
                        .style(move |_theme, status| swatch_style(color, status)),
                );
            }
            // The rainbow wheel that expands the hue picker.
            row = row.push(
                button(Space::new().width(SW).height(SW))
                    .padding(0)
                    .on_press(Message::ExpandColorPicker)
                    .style(|_theme, status| wheel_style(status)),
            );
            row.into()
        };

        let mut items: Vec<Element<Message>> =
            vec![container(body).padding([6, 12]).width(Fill).into()];
        // "Ask AI" only when an AI endpoint is configured (not "Off").
        if self.config.ai_enabled() {
            items.push(sep().into());
            items.push(
                button(text("Ask AI").size(14))
                    .width(Fill)
                    .on_press(Message::AskAi)
                    .padding([6, 12])
                    .style(item_style)
                    .into(),
            );
        }
        // Grouping, like auto-layout below it, only means anything for more
        // than one node — and a frame can't be wrapped in another frame here.
        if self.groupable_selection() {
            items.push(sep().into());
            items.push(
                button(text("Group").size(14))
                    .width(Fill)
                    .on_press(Message::GroupSelection)
                    .padding([6, 12])
                    .style(item_style)
                    .into(),
            );
        }
        items.push(sep().into());
        // "Distribute" (Mermaid-style auto-layout) only applies to more than
        // one node.
        if self.selected.len() > 1 {
            // "Auto-layout →" runs the default left-to-right layout; the small
            // ↓ chip runs top-down.
            let main = button(text("Auto-layout \u{2192}").size(14))
                .width(Fill)
                .on_press(Message::AutoLayout(crate::document::layout::Direction::Right))
                .padding([6, 12])
                .style(item_style);
            let down = button(text("\u{2193}").size(14).width(Fill).center())
                .width(Length::Fixed(32.0))
                .padding([6, 0])
                .on_press(Message::AutoLayout(crate::document::layout::Direction::Down))
                .style(chip_style);
            let layout_row = Row::new()
                .push(main)
                .push(down)
                .spacing(6)
                .align_y(iced::alignment::Vertical::Center)
                .width(Fill)
                .padding(Padding {
                    top: 0.0,
                    right: 12.0,
                    bottom: 0.0,
                    left: 0.0,
                });
            items.push(layout_row.into());
            items.push(sep().into());
        }
        items.push(delete.into());

        Column::with_children(items)
            .width(Length::Fixed(menu_width))
            .spacing(4)
            .into()
    }

    /// The settings form as an in-board modal: a dim backdrop (click to close)
    /// behind a centered, scrollable panel with a titled header and × button.
    /// Opened from the command palette; the tray still uses the settings window.
    fn settings_overlay_view(&self) -> Element<'_, Message> {
        let theme = &self.theme;
        let ct = self.theme;
        let font = theme.ui_font();
        let (text_c, muted, accent) = (theme.text, theme.text_muted, theme.accent);

        let close = button(text("×").size(20).font(font).color(muted))
            .padding([0, 8])
            .on_press(Message::CloseSettingsOverlay)
            .style(move |_theme, status| chrome_button_style(status, muted, accent));

        let header = row![
            text("Settings").size(17).font(font).color(text_c).width(Fill),
            close,
        ]
        .align_y(iced::Alignment::Center);

        // Render the form under the settings window's own theme, so its
        // background and widget colors match the standalone Settings window
        // exactly (the board window is `Theme::Dark`, which would recolor it).
        let form = iced::widget::themer(Some(startup::settings_theme(self)), settings::view(self));

        let body = column![header, iced::widget::scrollable(form).height(Length::Shrink)].spacing(12);

        // The panel shares the form's own background so there's no seam (the
        // form below is themed to match).
        let panel = container(body)
            .width(Length::Fixed(600.0))
            .max_height(720.0)
            .padding(20)
            .style(move |_theme| chrome::panel(&ct, ct.background));

        // A click on the panel is swallowed; a click on the backdrop closes.
        let panel = mouse_area(panel).on_press(Message::Noop);

        let backdrop = mouse_area(
            container(panel)
                .width(Fill)
                .height(Fill)
                .center_x(Fill)
                .center_y(Fill)
                .style(|_theme| container::Style {
                    background: Some(iced::Color { a: 0.45, ..iced::Color::BLACK }.into()),
                    ..container::Style::default()
                }),
        )
        .on_press(Message::CloseSettingsOverlay);

        backdrop.into()
    }

    /// The floating find bar (top-right): query field, `n/m` count, ‹ ›, ×.
    fn search_bar(&self, search: &Search) -> Element<'_, Message> {
        // Styled from the Obsidian ColorTheme so the bar recolors with the theme
        // (the overlay window itself stays on Theme::Dark).
        let theme = &self.theme;
        let ct = self.theme;
        let font = theme.ui_font();
        let (text_c, muted, accent, input_bg, border_c) =
            (theme.text, theme.text_muted, theme.accent, theme.background, theme.border);

        let count = if search.matches.is_empty() {
            if search.query.is_empty() {
                String::new()
            } else {
                "No results".to_owned()
            }
        } else {
            format!("{} / {}", search.current + 1, search.matches.len())
        };

        let step = move |label: &'static str, message: Message| {
            button(text(label).size(18).font(font))
                .padding([2, 10])
                .on_press_maybe((!search.matches.is_empty()).then_some(message))
                .style(move |_theme, status| chrome_button_style(status, muted, accent))
        };

        let input = text_input("Find on board…", &search.query)
            .id(super::SEARCH_INPUT.clone())
            .on_input(Message::SearchInput)
            .size(15)
            .font(font)
            .width(220)
            .style(move |_theme, status| iced::widget::text_input::Style {
                background: iced::Background::Color(input_bg),
                border: iced::Border {
                    radius: chrome::CONTROL_RADIUS.into(),
                    width: 1.0,
                    color: if matches!(status, iced::widget::text_input::Status::Focused { .. }) {
                        accent
                    } else {
                        border_c
                    },
                },
                icon: muted,
                placeholder: muted,
                value: text_c,
                selection: iced::Color { a: 0.30, ..accent },
            });

        let bar = row![
            input,
            // Fixed width + centered, so "No results" vs "3 / 12" vs "" never
            // resizes the bar.
            text(count)
                .size(13)
                .font(font)
                .width(Length::Fixed(90.0))
                .align_x(iced::widget::text::Alignment::Center)
                .color(muted),
            step("‹", Message::SearchPrev),
            step("›", Message::SearchNext),
            button(text("×").size(16).font(font))
                .padding([2, 10])
                .on_press(Message::CloseSearch)
                .style(move |_theme, status| chrome_button_style(status, muted, accent)),
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center);

        let panel =
            container(bar).padding([8, 10]).style(move |_theme| chrome::panel(&ct, ct.elevated()));

        container(panel)
            .width(Fill)
            .height(Fill)
            .align_x(iced::alignment::Horizontal::Center)
            .align_y(iced::alignment::Vertical::Top)
            // Extra top inset so the centered bar clears the MacBook notch.
            .padding(Padding {
                top: 44.0,
                right: 16.0,
                bottom: 0.0,
                left: 16.0,
            })
            .into()
    }

    /// Width of the node context menu: the swatch row (default + presets +
    /// optional extra + wheel) plus side padding, so opening the color picker
    /// never resizes it. Shared by `node_menu` and `context_menu_size`.
    fn node_menu_width(&self) -> f32 {
        const SW: f32 = 24.0;
        const GAP: f32 = 6.0;
        let has_extra = self
            .extra_swatch
            .as_deref()
            .is_some_and(|e| !NODE_PALETTE.contains(&e));
        let count = NODE_PALETTE.len() + has_extra as usize + 2;
        count as f32 * SW + (count - 1) as f32 * GAP + 24.0
    }

    /// Approximate on-screen size of the open context menu, used only to keep
    /// it from opening off a window edge. Heights are estimated from the row
    /// count (exactness isn't needed — it just bounds the clamp).
    fn context_menu_size(&self) -> Size {
        const ROW: f32 = 30.0; // a text button with [6,12] padding
        const SEP: f32 = 5.0; // 1px rule + the column's 4px spacing
        const PAD: f32 = 8.0; // the panel's 4px padding on both sides
        let Some(menu) = &self.context_menu else {
            return Size::ZERO;
        };
        let (width, height) = match menu.kind {
            ContextMenuKind::Canvas { .. } => (200.0, ROW * 5.0 + SEP * 4.0),
            ContextMenuKind::Node => {
                // Color row (24px swatch + [6,12] padding) + sep + "Ask AI" +
                // sep + delete, plus the layout row + sep for a multi-selection.
                let mut height = 36.0 + SEP + ROW + SEP + ROW;
                if self.selected.len() > 1 {
                    height += ROW + SEP;
                }
                (self.node_menu_width(), height)
            }
            ContextMenuKind::Link => (80.0, ROW),
        };
        Size::new(width + PAD, height + PAD)
    }

    /// Whether the node is within (or near) the viewport.
    pub(super) fn node_visible(&self, node: &crate::model::CanvasNode) -> bool {
        let rect = self.camera.rect_to_screen(node_world_rect(node));
        let margin = 0.25 * self.camera.viewport.width.max(self.camera.viewport.height);
        let viewport = Rectangle::new(
            Point::new(-margin, -margin),
            Size::new(
                self.camera.viewport.width + margin * 2.0,
                self.camera.viewport.height + margin * 2.0,
            ),
        );
        viewport.intersects(&rect)
    }

    pub(super) fn content_bounds(&self) -> Rectangle {
        let mut min = Point::new(f32::MAX, f32::MAX);
        let mut max = Point::new(f32::MIN, f32::MIN);

        for node in &self.nodes {
            min.x = min.x.min(node.x);
            min.y = min.y.min(node.y);
            max.x = max.x.max(node.x + node.width);
            max.y = max.y.max(node.y + node.height);
        }

        if self.nodes.is_empty() {
            Rectangle::new(Point::ORIGIN, Size::new(1.0, 1.0))
        } else {
            Rectangle::new(min, Size::new(max.x - min.x, max.y - min.y))
        }
    }
}

/// Cmd+B/I wrap the selection; Cmd+Z / Cmd+Shift+Z drive our text-level undo;
/// Cmd+Enter commits the edit; Cmd+Backspace deletes the previous word;
/// Cmd+Up/Down jump to the document ends (iced's default only remaps
/// Cmd+Left/Right to line ends), with Shift extending the selection. Everything
/// else keeps the default binding. `command()` is Cmd on macOS and Ctrl on
/// Windows/Linux, so these are platform-correct.
///
/// Letter chords match on the *latin* key for the physical position (same trick
/// iced uses for copy/paste), so they fire regardless of the active keyboard
/// layout.
fn editor_key_binding(
    key_press: text_editor::KeyPress,
) -> Option<text_editor::Binding<Message>> {
    use iced::keyboard::Key;
    use iced::keyboard::key::Named;

    // Delete the word to the left: select it, then delete it.
    let delete_word_left = || {
        Some(text_editor::Binding::Sequence(vec![
            text_editor::Binding::Select(text_editor::Motion::WordLeft),
            text_editor::Binding::Backspace,
        ]))
    };
    // On macOS, Option+Backspace (the standard delete-word chord — and what
    // Ctrl+Backspace surfaces as here) also deletes the previous word,
    // alongside the Cmd+Backspace handled in the `command()` block below.
    // (Elsewhere `command()` is Ctrl, so that block already covers
    // Ctrl+Backspace.)
    #[cfg(target_os = "macos")]
    if (key_press.modifiers.alt() || key_press.modifiers.control())
        && matches!(key_press.key, Key::Named(Named::Backspace))
    {
        return delete_word_left();
    }

    if key_press.modifiers.command() {
        let latin = key_press.key.to_latin(key_press.physical_key);
        match (latin, &key_press.key) {
            (Some('b'), _) => {
                return Some(text_editor::Binding::Custom(Message::WrapSelection("**")));
            }
            (Some('i'), _) => {
                return Some(text_editor::Binding::Custom(Message::WrapSelection("*")));
            }
            (Some('z'), _) => {
                return Some(text_editor::Binding::Custom(if key_press.modifiers.shift() {
                    Message::RedoEdit
                } else {
                    Message::UndoEdit
                }));
            }
            // Cmd/Ctrl+Enter commits (Enter alone is a newline, since notes are
            // multi-line).
            (_, Key::Named(Named::Enter)) => {
                return Some(text_editor::Binding::Custom(Message::StopEditing));
            }
            // Cmd/Ctrl+Backspace deletes the word to the left.
            (_, Key::Named(Named::Backspace)) => {
                return delete_word_left();
            }
            (_, Key::Named(named)) => {
                let motion = match named {
                    Named::ArrowUp => Some(text_editor::Motion::DocumentStart),
                    Named::ArrowDown => Some(text_editor::Motion::DocumentEnd),
                    _ => None,
                };
                if let Some(motion) = motion {
                    return Some(if key_press.modifiers.shift() {
                        text_editor::Binding::Select(motion)
                    } else {
                        text_editor::Binding::Move(motion)
                    });
                }
            }
            _ => {}
        }
    }
    text_editor::Binding::from_key_press(key_press)
}
