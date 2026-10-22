//! Controls that show only while the pointer is over what they sit on, or
//! while the keyboard is on them: a row's Reply, say.
//!
//! Iced's own `hover` keeps the top showing while anything in it has the
//! focus, and a click gives a control the focus, so a clicked Reply stayed
//! drawn after the pointer had left, until the next click elsewhere. Here
//! only the keyboard's focus — the ringed one — keeps it showing.
use super::Focus;
use crate::app::Message;
use iced::advanced::{
    Clipboard, Layout, Shell, Widget, layout, mouse, overlay, renderer,
    widget::{Operation, Tree, tree},
};
use iced::{Element, Event, Length, Rectangle, Size, Vector};

/// `base`, with `top` drawn over it while the pointer is over `base` or
/// the keyboard's focus is in `top`.
pub fn hover<'a>(
    base: impl Into<Element<'a, Message>>,
    top: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    Element::new(Hover {
        base: base.into(),
        top: top.into(),
    })
}

struct Hover<'a> {
    base: Element<'a, Message>,
    top: Element<'a, Message>,
}

/// Whether the pointer was over it at the last event, so crossing its edge
/// redraws once.
#[derive(Default)]
struct State {
    hovered: bool,
}

/// Whether a control under `tree` has the keyboard's focus.
fn ringed_within(tree: &Tree) -> bool {
    (tree.tag == tree::Tag::of::<Focus>() && tree.state.downcast_ref::<Focus>().ringed())
        || tree.children.iter().any(ringed_within)
}

/// Whether the top shows: under the pointer, or holding the keyboard.
fn shows_top(tree: &Tree, layout: Layout<'_>, cursor: mouse::Cursor) -> bool {
    cursor.is_over(layout.bounds()) || ringed_within(&tree.children[1])
}

impl Widget<Message, iced::Theme, iced::Renderer> for Hover<'_> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }
    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }
    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.base), Tree::new(&self.top)]
    }
    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&[&self.base, &self.top]);
    }
    fn size(&self) -> Size<Length> {
        self.base.as_widget().size()
    }
    fn size_hint(&self) -> Size<Length> {
        self.base.as_widget().size_hint()
    }
    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let base = self
            .base
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits);
        let top = self.top.as_widget_mut().layout(
            &mut tree.children[1],
            renderer,
            &layout::Limits::new(Size::ZERO, base.size()),
        );
        layout::Node::with_children(base.size(), vec![base, top])
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
        use iced::advanced::Renderer as _;
        let Some(bounds) = layout.bounds().intersection(viewport) else {
            return;
        };
        let mut children = layout.children();
        let (base, top) = (children.next().unwrap(), children.next().unwrap());
        self.base.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            base,
            cursor,
            viewport,
        );
        if shows_top(tree, layout, cursor) {
            renderer.with_layer(bounds, |renderer| {
                self.top.as_widget().draw(
                    &tree.children[1],
                    renderer,
                    theme,
                    style,
                    top,
                    cursor,
                    viewport,
                );
            });
        }
    }
    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn Operation,
    ) {
        let mut children = layout.children();
        self.base.as_widget_mut().operate(
            &mut tree.children[0],
            children.next().unwrap(),
            renderer,
            operation,
        );
        self.top.as_widget_mut().operate(
            &mut tree.children[1],
            children.next().unwrap(),
            renderer,
            operation,
        );
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
        let hovered = cursor.is_over(layout.bounds());
        let state = tree.state.downcast_mut::<State>();
        if state.hovered != hovered {
            state.hovered = hovered;
            shell.request_redraw();
        }
        let mut children = layout.children();
        let (base, top) = (children.next().unwrap(), children.next().unwrap());
        // The pointer's events always reach the top, so a control in it
        // hears a press elsewhere and lets its focus go; a hidden top is
        // never under the pointer, so nothing in it can be pressed. Keys
        // reach it only while it shows.
        if matches!(event, Event::Mouse(_)) || shows_top(tree, layout, cursor) {
            self.top.as_widget_mut().update(
                &mut tree.children[1],
                event,
                top,
                cursor,
                renderer,
                clipboard,
                shell,
                viewport,
            );
            if shell.is_event_captured() {
                return;
            }
        }
        self.base.as_widget_mut().update(
            &mut tree.children[0],
            event,
            base,
            cursor,
            renderer,
            clipboard,
            shell,
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
        let mut children = layout.children();
        let (base, top) = (children.next().unwrap(), children.next().unwrap());
        let over_top = shows_top(tree, layout, cursor)
            .then(|| {
                self.top.as_widget().mouse_interaction(
                    &tree.children[1],
                    top,
                    cursor,
                    viewport,
                    renderer,
                )
            })
            .filter(|interaction| *interaction != mouse::Interaction::None);
        over_top.unwrap_or_else(|| {
            self.base.as_widget().mouse_interaction(
                &tree.children[0],
                base,
                cursor,
                viewport,
                renderer,
            )
        })
    }
    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &iced::Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, iced::Theme, iced::Renderer>> {
        let (base_tree, top_tree) = tree.children.split_at_mut(1);
        let mut children = layout.children();
        let (base, top) = (children.next().unwrap(), children.next().unwrap());
        let base = self.base.as_widget_mut().overlay(
            &mut base_tree[0],
            base,
            renderer,
            viewport,
            translation,
        );
        let top = self.top.as_widget_mut().overlay(
            &mut top_tree[0],
            top,
            renderer,
            viewport,
            translation,
        );
        match (base, top) {
            (None, None) => None,
            (Some(one), None) | (None, Some(one)) => Some(one),
            (Some(base), Some(top)) => {
                Some(overlay::Group::with_children(vec![base, top]).overlay())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A row with Reply over it: the top shows under the pointer, and
    /// after a click on Reply it goes when the pointer leaves — the
    /// click's quiet focus does not hold it — while the keyboard's focus,
    /// ringed, does.
    #[test]
    fn the_top_shows_for_the_pointer_over_it_or_the_keyboard_on_it() {
        let renderer = iced::Renderer::new(iced::Font::DEFAULT, 14.0.into());
        let mut element = hover(
            iced::widget::Space::new().width(300).height(40),
            super::super::button("reply", "Reply", Some(Message::ShowAdd), false),
        );
        let mut tree = Tree::new(&element);
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, Size::new(400.0, 100.0)),
        );
        let over = mouse::Cursor::Available(iced::Point::new(10.0, 10.0));
        let away = mouse::Cursor::Available(iced::Point::new(390.0, 90.0));
        let shows = |tree: &Tree, cursor| shows_top(tree, Layout::new(&node), cursor);
        assert!(shows(&tree, over));
        assert!(!shows(&tree, away));
        let focus = tree.children[1].state.downcast_mut::<Focus>();
        focus.focused = true;
        focus.by_pointer = true;
        assert!(!shows(&tree, away), "a click's focus does not hold it");
        tree.children[1].state.downcast_mut::<Focus>().by_pointer = false;
        assert!(shows(&tree, away), "the keyboard's does");
    }
}
