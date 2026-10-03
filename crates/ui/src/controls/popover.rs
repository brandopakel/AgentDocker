//! A menu that opens from a control and floats over the page.
//!
//! The anchor is laid out where it stands; the popup, while open, is an
//! overlay placed under the anchor (above it when there is no room below),
//! aligned to the anchor's start or end edge and kept inside the window.
//! It takes no space in the page, so opening it moves nothing. A press
//! outside both, or Escape, asks for it to close; whether it is open is
//! the application's state, not the widget's.
//!
//! Operations reach the popup through Iced's overlay pass, so its
//! controls are focusable, carry their accessibility nodes and are found
//! by the workflow driver like any other control.
use crate::app::Message;
use iced::advanced::{
    Clipboard, Layout, Shell, Widget, layout, mouse, overlay, renderer,
    widget::{Operation, Tree},
};
use iced::{Element, Event, Length, Point, Rectangle, Size, Vector, keyboard};

pub struct Popover<'a> {
    anchor: Element<'a, Message>,
    popup: Option<Element<'a, Message>>,
    width: f32,
    align_end: bool,
    on_dismiss: Option<Message>,
}

/// `anchor`, with `popup` floating under it while it is `Some`.
pub fn popover<'a>(
    anchor: impl Into<Element<'a, Message>>,
    popup: Option<Element<'a, Message>>,
) -> Popover<'a> {
    Popover {
        anchor: anchor.into(),
        popup,
        width: 220.0,
        align_end: false,
        on_dismiss: None,
    }
}

impl Popover<'_> {
    /// The popup's width in logical pixels.
    pub fn width(mut self, width: f32) -> Self {
        self.width = width;
        self
    }
    /// Line the popup up with the anchor's right edge rather than its left.
    pub fn align_end(mut self) -> Self {
        self.align_end = true;
        self
    }
    /// What a press outside or Escape sends.
    pub fn on_dismiss(mut self, message: Message) -> Self {
        self.on_dismiss = Some(message);
        self
    }
}

impl<'a> From<Popover<'a>> for Element<'a, Message> {
    fn from(popover: Popover<'a>) -> Self {
        Element::new(popover)
    }
}

impl Widget<Message, iced::Theme, iced::Renderer> for Popover<'_> {
    fn children(&self) -> Vec<Tree> {
        let mut children = vec![Tree::new(&self.anchor)];
        if let Some(popup) = &self.popup {
            children.push(Tree::new(popup));
        }
        children
    }
    fn diff(&self, tree: &mut Tree) {
        match &self.popup {
            Some(popup) => tree.diff_children(&[&self.anchor, popup]),
            None => tree.diff_children(std::slice::from_ref(&self.anchor)),
        }
    }
    fn size(&self) -> Size<Length> {
        self.anchor.as_widget().size()
    }
    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.anchor
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }
    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn Operation,
    ) {
        self.anchor
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
    }
    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        self.anchor.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
    }
    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut iced::Renderer,
        theme: &iced::Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.anchor.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
    }
    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        self.anchor.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
    }
    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &iced::Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, iced::Theme, iced::Renderer>> {
        let anchor_bounds = layout.bounds() + translation;
        let (anchor_tree, popup_tree) = match tree.children.split_first_mut() {
            Some((first, rest)) => (first, rest.first_mut()),
            None => return None,
        };
        let nested = self.anchor.as_widget_mut().overlay(
            anchor_tree,
            layout,
            renderer,
            viewport,
            translation,
        );
        let popup = match (self.popup.as_mut(), popup_tree) {
            (Some(content), Some(tree)) => Some(overlay::Element::new(Box::new(Popup {
                content,
                tree,
                anchor: anchor_bounds,
                width: self.width,
                align_end: self.align_end,
                on_dismiss: self.on_dismiss.clone(),
            }))),
            _ => None,
        };
        match (nested, popup) {
            (None, None) => None,
            (Some(one), None) | (None, Some(one)) => Some(one),
            (Some(nested), Some(popup)) => {
                Some(overlay::Group::with_children(vec![nested, popup]).overlay())
            }
        }
    }
}

struct Popup<'a, 'b> {
    content: &'b mut Element<'a, Message>,
    tree: &'b mut Tree,
    anchor: Rectangle,
    width: f32,
    align_end: bool,
    on_dismiss: Option<Message>,
}

/// Room left between the popup and the window's edge, and between the
/// popup and its anchor.
const MARGIN: f32 = 4.0;

impl overlay::Overlay<Message, iced::Theme, iced::Renderer> for Popup<'_, '_> {
    fn layout(&mut self, renderer: &iced::Renderer, bounds: Size) -> layout::Node {
        let width = self.width.min(bounds.width - 2.0 * MARGIN).max(1.0);
        let limits = layout::Limits::new(
            Size::new(width, 0.0),
            Size::new(width, (bounds.height - 2.0 * MARGIN).max(1.0)),
        );
        let node = self
            .content
            .as_widget_mut()
            .layout(self.tree, renderer, &limits);
        let size = node.size();
        let x = if self.align_end {
            self.anchor.x + self.anchor.width - size.width
        } else {
            self.anchor.x
        }
        .clamp(MARGIN, (bounds.width - size.width - MARGIN).max(MARGIN));
        let below = self.anchor.y + self.anchor.height + MARGIN;
        let y = if below + size.height <= bounds.height - MARGIN {
            below
        } else {
            (self.anchor.y - size.height - MARGIN).max(MARGIN)
        };
        node.move_to(Point::new(x, y))
    }
    fn draw(
        &self,
        renderer: &mut iced::Renderer,
        theme: &iced::Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
    ) {
        self.content.as_widget().draw(
            self.tree,
            renderer,
            theme,
            style,
            layout,
            cursor,
            &layout.bounds(),
        );
    }
    fn operate(
        &mut self,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn Operation,
    ) {
        self.content
            .as_widget_mut()
            .operate(self.tree, layout, renderer, operation);
    }
    fn update(
        &mut self,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
    ) {
        let outside_press = matches!(event, Event::Mouse(mouse::Event::ButtonPressed(_)))
            && !cursor.is_over(layout.bounds())
            && !cursor.is_over(self.anchor);
        let escape = matches!(
            event,
            Event::Keyboard(keyboard::Event::KeyPressed {
                key: keyboard::Key::Named(keyboard::key::Named::Escape),
                ..
            })
        );
        if (outside_press || escape)
            && let Some(dismiss) = &self.on_dismiss
        {
            shell.publish(dismiss.clone());
            if escape {
                shell.capture_event();
                return;
            }
        }
        self.content.as_widget_mut().update(
            self.tree,
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            &layout.bounds(),
        );
        // The popup is a surface, not a stencil: a press on its padding,
        // a separator or a line of text stops here rather than reaching
        // the control drawn beneath it.
        if matches!(event, Event::Mouse(_)) && cursor.is_over(layout.bounds()) {
            shell.capture_event();
        }
    }
    fn mouse_interaction(
        &self,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        let inner = self.content.as_widget().mouse_interaction(
            self.tree,
            layout,
            cursor,
            &layout.bounds(),
            renderer,
        );
        // Iced takes `None` to mean the pointer is not over an overlay and
        // hands it to the page beneath; over the popup it never is.
        if inner == mouse::Interaction::None && cursor.is_over(layout.bounds()) {
            mouse::Interaction::Idle
        } else {
            inner
        }
    }
    fn overlay<'c>(
        &'c mut self,
        layout: Layout<'c>,
        renderer: &iced::Renderer,
    ) -> Option<overlay::Element<'c, Message, iced::Theme, iced::Renderer>> {
        let bounds = layout.bounds();
        self.content
            .as_widget_mut()
            .overlay(self.tree, layout, renderer, &bounds, Vector::ZERO)
    }
}
