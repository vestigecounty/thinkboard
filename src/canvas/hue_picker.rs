//! A smooth hue-spectrum picker for choosing a node color.
//!
//! A full-saturation, full-value hue bar; clicking picks the hue under the
//! cursor. A small sliding "window" tracks the cursor to preview where the
//! pick will land, decorated like the color-swatch hover.

use iced::widget::canvas::{self, Frame, Path, Stroke, gradient};
use iced::{Color, Event, Point, Rectangle, Renderer, Size, Theme, mouse};

use crate::Message;
use crate::model::{self, hsv};

pub struct HuePicker {
    /// Open-transition progress, 0..1 (1 = fully shown). The bar unrolls from
    /// its center as this eases to 1.
    pub reveal: f32,
}

#[derive(Default)]
pub struct State {
    /// Cursor x within the bar (local px) while hovering, else `None`.
    hover: Option<f32>,
}

impl canvas::Program<Message> for HuePicker {
    type State = State;

    fn update(
        &self,
        state: &mut State,
        event: &Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<Message>> {
        match event {
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let x = cursor.position_in(bounds).map(|p| p.x);
                if state.hover != x {
                    state.hover = x;
                    return Some(canvas::Action::request_redraw());
                }
                None
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let point = cursor.position_in(bounds)?;
                let hue = (point.x / bounds.width).clamp(0.0, 1.0);
                let hex = model::color_to_hex(hsv(hue, 1.0, 1.0));
                Some(canvas::Action::publish(Message::SetNodeColor(Some(hex))).and_capture())
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        state: &State,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let (w, h) = (bounds.width, bounds.height);

        // The bar unrolls from the center as the picker opens.
        let reveal = self.reveal.clamp(0.0, 1.0);
        let bar_w = w * reveal;
        if bar_w <= 1.0 {
            return vec![frame.into_geometry()];
        }
        let x0 = (w - bar_w) / 2.0;
        let radius = 4.0_f32.min(bar_w / 2.0).min(h / 2.0);
        let bar = Path::rounded_rectangle(Point::new(x0, 0.0), Size::new(bar_w, h), radius.into());

        // Seven stops at the hue vertices, positioned across the full width so
        // the unroll reveals the fixed spectrum rather than stretching it: a
        // full-sat/val hue sweep is piecewise-linear in RGB, so interpolating
        // between them is exact and the spectrum comes out smooth.
        let mut spectrum = gradient::Linear::new(Point::new(0.0, 0.0), Point::new(w, 0.0));
        for i in 0..=6 {
            spectrum = spectrum.add_stop(i as f32 / 6.0, hsv(i as f32 / 6.0, 1.0, 1.0));
        }
        frame.fill(&bar, spectrum);

        // The hover window only makes sense once the bar is fully open.
        if reveal < 0.999 {
            return vec![frame.into_geometry()];
        }
        if let Some(hover_x) = state.hover {
            // Sliding window: same decoration as the swatch hover (2px white,
            // rounded), clamped inside the bar.
            let win_w = 16.0;
            let left = (hover_x - win_w / 2.0).clamp(0.0, w - win_w);
            let window =
                Path::rounded_rectangle(Point::new(left, 0.5), Size::new(win_w, h - 1.0), 4.0.into());
            frame.stroke(
                &window,
                Stroke::default().with_color(Color::WHITE).with_width(2.0),
            );
        }

        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        _state: &State,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        if cursor.is_over(bounds) {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::default()
        }
    }
}
