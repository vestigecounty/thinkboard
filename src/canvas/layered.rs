//! iced's canvas widget, with one change: a geometry its program draws can be
//! given a renderer layer of its own.
//!
//! Within a layer, iced_wgpu draws all of its meshes, then all of its images,
//! then all of its text, whatever order they were recorded in — and the stock
//! canvas draws every geometry it is handed into the *same* layer, though its
//! own source calls them "layers". So a card's fill could never cover another
//! card's bitmap or text: a dragged card showed the text of the card beneath
//! it interleaved with its own, and so did any two cards left overlapping.
//! Splitting the board into geometries (the selected cards' overlay, the
//! planes of `Board::draw`) was meant to stack them, and with a layer each it
//! does.
//!
//! **A layer is not free**, which is why it's asked for per geometry rather
//! than given to each. With antialiasing on, every layer holding meshes is a
//! multisampled pass over the whole window and a blit back — measured at about
//! 0.15 ms a layer on a retina frame. So a geometry only starts a layer when it
//! covers something beneath it, and a board with no overlapping cards draws in
//! one layer, exactly as the stock canvas did.
//!
//! Everything but [`Widget::draw`] is the stock widget's, so events, state and
//! the cursor behave exactly as before.

use std::marker::PhantomData;

use iced::advanced::graphics::geometry::Renderer as _;
use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer::{self, Renderer as _};
use iced::advanced::widget::tree::{self, Tree};
use iced::advanced::{Clipboard, Shell, Widget};
use iced::widget::canvas::{Event, Geometry, Program};
use iced::{Element, Length, Rectangle, Renderer, Size, Theme, Vector, event, mouse, window};

/// What a [`Layered`] canvas draws: a program whose geometries say which of
/// them have to sit above the ones before.
pub trait Stacked<Message>: Program<Message, Theme, Renderer> {
    /// The geometries, bottom first, each with whether it starts a layer of
    /// its own — true only where it covers what's beneath it, since a layer
    /// costs a pass of the whole window. One that doesn't joins the layer of
    /// the one before it.
    fn draw_stacked(
        &self,
        state: &Self::State,
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Vec<(Geometry, bool)>;
}

/// A canvas whose geometries stack — see the module docs.
pub struct Layered<P, Message> {
    program: P,
    width: Length,
    height: Length,
    last_mouse_interaction: Option<mouse::Interaction>,
    message: PhantomData<Message>,
}

/// A [`Layered`] canvas filling its parent.
pub fn layered<P, Message>(program: P) -> Layered<P, Message>
where
    P: Stacked<Message>,
{
    Layered {
        program,
        width: Length::Fill,
        height: Length::Fill,
        last_mouse_interaction: None,
        message: PhantomData,
    }
}

impl<P, Message> Widget<Message, Theme, Renderer> for Layered<P, Message>
where
    P: Stacked<Message>,
{
    fn tag(&self) -> tree::Tag {
        struct Tag<T>(T);
        tree::Tag::of::<Tag<P::State>>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(P::State::default())
    }

    fn size(&self) -> Size<Length> {
        Size { width: self.width, height: self.height }
    }

    fn layout(&mut self, _tree: &mut Tree, _renderer: &Renderer, limits: &layout::Limits) -> layout::Node {
        layout::atomic(limits, self.width, self.height)
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        let state = tree.state.downcast_mut::<P::State>();
        let is_redraw_request = matches!(event, Event::Window(window::Event::RedrawRequested(_)));

        if let Some(action) = self.program.update(state, event, bounds, cursor) {
            let (message, redraw_request, event_status) = action.into_inner();
            shell.request_redraw_at(redraw_request);
            if let Some(message) = message {
                shell.publish(message);
            }
            if event_status == event::Status::Captured {
                shell.capture_event();
            }
        }

        if shell.redraw_request() != window::RedrawRequest::NextFrame {
            let mouse_interaction = self.mouse_interaction(tree, layout, cursor, viewport, renderer);
            if is_redraw_request {
                self.last_mouse_interaction = Some(mouse_interaction);
            } else if self.last_mouse_interaction.is_some_and(|last| last != mouse_interaction) {
                shell.request_redraw();
            }
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &Renderer,
    ) -> mouse::Interaction {
        let state = tree.state.downcast_ref::<P::State>();
        self.program.mouse_interaction(state, layout.bounds(), cursor)
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        if bounds.width < 1.0 || bounds.height < 1.0 {
            return;
        }
        let state = tree.state.downcast_ref::<P::State>();
        let offset = Vector::new(bounds.x, bounds.y);
        // Opened by hand rather than with `with_layer`, so a geometry that
        // doesn't start one lands in the last that was started, above
        // everything before it, instead of falling back beneath.
        let mut open = false;
        for (i, (geometry, own_layer)) in
            self.program.draw_stacked(state, renderer, theme, bounds, cursor).into_iter().enumerate()
        {
            // The first needs no layer of its own whatever it says: the canvas
            // already sits in one (it's in a stack), with nothing of ours under it.
            if own_layer && i > 0 {
                if open {
                    renderer.end_layer();
                }
                renderer.start_layer(bounds);
                open = true;
            }
            renderer.with_translation(offset, |renderer| renderer.draw_geometry(geometry));
        }
        if open {
            renderer.end_layer();
        }
    }
}

impl<'a, P, Message> From<Layered<P, Message>> for Element<'a, Message>
where
    P: Stacked<Message> + 'a,
    Message: 'a,
{
    fn from(canvas: Layered<P, Message>) -> Self {
        Element::new(canvas)
    }
}
