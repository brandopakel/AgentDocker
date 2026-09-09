//! Keyboard-focusable Iced controls with native accessibility metadata.
use crate::{accessibility::Semantic, app::Message};
use iced::advanced::Renderer as _;

/// Scroll ancestors just enough to reveal the newly focused control.
#[derive(Default)]
struct Reveal {
    pending: Option<(widget::Id, Rectangle, iced::Vector)>,
    parents: Vec<(widget::Id, Rectangle, iced::Vector)>,
    adjustments: Vec<(widget::Id, iced::Vector)>,
}
impl Operation for Reveal {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        let pushed = self.pending.take();
        if let Some(parent) = pushed.clone() {
            self.parents.push(parent);
        }
        operate(self);
        if pushed.is_some() {
            self.parents.pop();
        }
    }
    fn scrollable(
        &mut self,
        id: Option<&widget::Id>,
        bounds: Rectangle,
        _: Rectangle,
        translation: iced::Vector,
        _: &mut dyn widget::operation::Scrollable,
    ) {
        self.pending = id.cloned().map(|id| (id, bounds, translation));
    }
    fn focusable(
        &mut self,
        _: Option<&widget::Id>,
        bounds: Rectangle,
        state: &mut dyn widget::operation::Focusable,
    ) {
        if !state.is_focused() {
            return;
        }
        for (id, viewport, offset) in &self.parents {
            let x = (bounds.x - offset.x).max(viewport.x);
            let y = (bounds.y - offset.y).max(viewport.y);
            let want = iced::Vector::new(
                if bounds.x - offset.x < viewport.x {
                    (bounds.x - viewport.x).max(0.0)
                } else if x + bounds.width > viewport.x + viewport.width {
                    (bounds.x + bounds.width - viewport.x - viewport.width).max(0.0)
                } else {
                    offset.x
                },
                if bounds.y - offset.y < viewport.y {
                    (bounds.y - viewport.y).max(0.0)
                } else if y + bounds.height > viewport.y + viewport.height {
                    (bounds.y + bounds.height - viewport.y - viewport.height).max(0.0)
                } else {
                    offset.y
                },
            );
            if want != *offset {
                self.adjustments.push((id.clone(), want));
            }
        }
    }
    fn finish(&self) -> widget::operation::Outcome<()> {
        if self.adjustments.is_empty() {
            widget::operation::Outcome::None
        } else {
            widget::operation::Outcome::Chain(Box::new(Adjust(self.adjustments.clone())))
        }
    }
}
struct Adjust(Vec<(widget::Id, iced::Vector)>);
impl Operation for Adjust {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        operate(self);
    }
    fn scrollable(
        &mut self,
        id: Option<&widget::Id>,
        _: Rectangle,
        _: Rectangle,
        _: iced::Vector,
        state: &mut dyn widget::operation::Scrollable,
    ) {
        if let Some((_, offset)) = self.0.iter().find(|(target, _)| Some(target) == id) {
            state.scroll_to(widget::operation::scrollable::AbsoluteOffset {
                x: Some(offset.x),
                y: Some(offset.y),
            });
        }
    }
}
pub fn reveal_focus() -> iced::Task<Message> {
    iced::advanced::widget::operate(Reveal::default()).discard()
}
use iced::advanced::{
    Clipboard, Layout, Shell, Widget, layout, renderer,
    widget::{self, Operation, Tree, tree},
};
use iced::{Border, Element, Event, Length, Rectangle, Size, keyboard, mouse};

#[derive(Default)]
pub struct Focus {
    pub focused: bool,
    pub id: Option<String>,
}
impl widget::operation::Focusable for Focus {
    fn is_focused(&self) -> bool {
        self.focused
    }
    fn focus(&mut self) {
        self.focused = true;
    }
    fn unfocus(&mut self) {
        self.focused = false;
    }
}

pub struct Control<'a> {
    pub content: Element<'a, Message>,
    pub semantic: Semantic,
    pub button: bool,
}
impl<'a> From<Control<'a>> for Element<'a, Message> {
    fn from(control: Control<'a>) -> Self {
        Self::new(control)
    }
}
impl Widget<Message, iced::Theme, iced::Renderer> for Control<'_> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<Focus>()
    }
    fn state(&self) -> tree::State {
        tree::State::new(Focus::default())
    }
    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }
    fn diff(&self, tree: &mut Tree) {
        let state = tree.state.downcast_mut::<Focus>();
        if state.id.as_deref() != Some(self.semantic.id.as_str()) {
            state.focused = false;
            state.id = Some(self.semantic.id.clone());
        }
        tree.diff_children(std::slice::from_ref(&self.content));
    }
    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }
    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.content
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
        let id = widget::Id::from(self.semantic.id.clone());
        operation.custom(Some(&id), layout.bounds(), &mut self.semantic);
        if self.button && self.semantic.action.is_some() {
            operation.focusable(
                Some(&id),
                layout.bounds(),
                tree.state.downcast_mut::<Focus>(),
            );
        }
        self.content
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
        let state = tree.state.downcast_mut::<Focus>();
        if self.semantic.role != accesskit::Role::Button || self.semantic.action.is_some() {
            if matches!(
                event,
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
            ) && cursor.is_over(layout.bounds())
            {
                shell.publish(Message::Focus(self.semantic.id.clone()));
            }
            if self.button
                && state.focused
                && let Event::Keyboard(keyboard::Event::KeyPressed { key, repeat, .. }) = event
                && (matches!(
                    key,
                    keyboard::Key::Named(keyboard::key::Named::Enter | keyboard::key::Named::Space)
                ) || matches!(key,keyboard::Key::Character(c) if c.as_str()==" "))
            {
                if !repeat {
                    shell.publish(self.semantic.action.clone().unwrap());
                }
                shell.capture_event();
                return;
            }
        }
        self.content.as_widget_mut().update(
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
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
        if self.button && tree.state.downcast_ref::<Focus>().focused {
            renderer.fill_quad(
                renderer::Quad {
                    bounds: layout.bounds(),
                    border: Border {
                        color: theme.palette().primary,
                        width: 2.0,
                        radius: 7.0.into(),
                    },
                    ..Default::default()
                },
                iced::Color::TRANSPARENT,
            );
        }
    }
    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
    }
    fn overlay<'a>(
        &'a mut self,
        tree: &'a mut Tree,
        layout: Layout<'a>,
        renderer: &iced::Renderer,
        viewport: &Rectangle,
        translation: iced::Vector,
    ) -> Option<iced::advanced::overlay::Element<'a, Message, iced::Theme, iced::Renderer>> {
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

pub fn button<'a>(
    id: impl Into<String>,
    label: impl Into<String>,
    message: Option<Message>,
    selected: bool,
) -> Element<'a, Message> {
    sized_button(id, label, message, selected, false)
}
pub fn block_button<'a>(
    id: impl Into<String>,
    label: impl Into<String>,
    message: Option<Message>,
    selected: bool,
) -> Element<'a, Message> {
    sized_button(id, label, message, selected, true)
}
fn sized_button<'a>(
    id: impl Into<String>,
    label: impl Into<String>,
    message: Option<Message>,
    selected: bool,
    block: bool,
) -> Element<'a, Message> {
    let (id, label) = (id.into(), label.into());
    let content = iced::widget::button(iced::widget::text(label.clone()).size(14))
        .padding([10, 12])
        .width(if block { Length::Fill } else { Length::Shrink })
        .on_press_maybe(message.clone())
        .style(move |theme, status| {
            let mut style = iced::widget::button::subtle(theme, status);
            if block && !selected && status == iced::widget::button::Status::Active {
                style.background = None;
            }
            if selected {
                let dark = theme.palette().background.r < 0.5;
                style.background = Some(
                    if dark {
                        iced::color!(0x263a59)
                    } else {
                        iced::color!(0xe8effb)
                    }
                    .into(),
                );
                style.text_color = if dark {
                    iced::color!(0xb8d0ff)
                } else {
                    iced::color!(0x1d4fa4)
                };
            }
            style.border.radius = 7.0.into();
            style
        });
    Control {
        content: content.into(),
        semantic: Semantic::button(id, label, message),
        button: true,
    }
    .into()
}

pub fn input<'a>(
    id: impl Into<String>,
    label: &str,
    value: &str,
    change: impl Fn(String) -> Message + Send + Sync + 'static,
) -> Element<'a, Message> {
    input_enabled(id, label, value, change, true)
}

pub fn input_enabled<'a>(
    id: impl Into<String>,
    label: &str,
    value: &str,
    change: impl Fn(String) -> Message + Send + Sync + 'static,
    enabled: bool,
) -> Element<'a, Message> {
    let id = id.into();
    let change = std::sync::Arc::new(change);
    let on_input = change.clone();
    let content = iced::widget::text_input(label, value)
        .id(widget::Id::from(id.clone()))
        .padding(12)
        .on_input_maybe(enabled.then_some(move |v| on_input(v)));
    let mut semantic = Semantic::input(id, label.into(), value.into(), change);
    if !enabled {
        semantic.change = None;
    }
    Control {
        content: content.into(),
        semantic,
        button: false,
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn press(key: keyboard::Key, repeat: bool) -> Event {
        Event::Keyboard(keyboard::Event::KeyPressed {
            modified_key: key.clone(),
            key,
            physical_key: keyboard::key::Physical::Unidentified(
                keyboard::key::NativeCode::Unidentified,
            ),
            location: keyboard::Location::Standard,
            modifiers: keyboard::Modifiers::empty(),
            text: None,
            repeat,
        })
    }
    #[test]
    fn keyboard_activation_requires_an_enabled_focused_button_and_never_repeats() {
        let renderer = iced::Renderer::new(iced::Font::DEFAULT, 14.0.into());
        for (focused, enabled, repeat, expected) in [
            (true, true, false, 1),
            (false, true, false, 0),
            (true, false, false, 0),
            (true, true, true, 0),
        ] {
            for key in [
                keyboard::Key::Named(keyboard::key::Named::Enter),
                keyboard::Key::Character(" ".into()),
            ] {
                let mut element = button(
                    "add",
                    "Add project",
                    enabled.then_some(Message::ShowAdd),
                    false,
                );
                let mut tree = Tree::new(&element);
                tree.state.downcast_mut::<Focus>().focused = focused;
                let node = element.as_widget_mut().layout(
                    &mut tree,
                    &renderer,
                    &layout::Limits::new(Size::ZERO, Size::new(400.0, 100.0)),
                );
                let mut messages = Vec::new();
                let mut shell = Shell::new(&mut messages);
                element.as_widget_mut().update(
                    &mut tree,
                    &press(key, repeat),
                    Layout::new(&node),
                    mouse::Cursor::Unavailable,
                    &renderer,
                    &mut iced::advanced::clipboard::Null,
                    &mut shell,
                    &Rectangle::with_size(Size::new(400.0, 100.0)),
                );
                assert_eq!(messages.len(), expected);
                assert!(messages.iter().all(|m| matches!(m, Message::ShowAdd)));
            }
        }
    }
    #[test]
    fn reordering_session_controls_does_not_transfer_keyboard_focus_to_another_agent() {
        let first = button("session-one", "First", Some(Message::ShowAdd), false);
        let mut tree = Tree::new(&first);
        first.as_widget().diff(&mut tree);
        tree.state.downcast_mut::<Focus>().focused = true;
        let next = button("session-two", "Second", Some(Message::ShowAdd), false);
        next.as_widget().diff(&mut tree);
        assert!(!tree.state.downcast_ref::<Focus>().focused);
    }
}
