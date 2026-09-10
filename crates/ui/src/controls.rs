//! Keyboard-focusable Iced controls with native accessibility metadata.
use crate::{accessibility::Semantic, app::Message};
use iced::advanced::Renderer as _;

/// Scroll ancestors just enough to reveal the newly focused control, or,
/// given a `target`, the container carrying that id. Revealing a target
/// moves nothing but scroll offsets: focus stays where it was.
#[derive(Default)]
struct Reveal {
    target: Option<widget::Id>,
    pending: Option<(widget::Id, Rectangle, iced::Vector)>,
    parents: Vec<(widget::Id, Rectangle, iced::Vector)>,
    adjustments: Vec<(widget::Id, iced::Vector)>,
}
impl Reveal {
    fn reveal(&mut self, bounds: Rectangle) {
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
    fn container(&mut self, id: Option<&widget::Id>, bounds: Rectangle) {
        if self.target.is_some() && id == self.target.as_ref() {
            self.reveal(bounds);
        }
    }
    fn focusable(
        &mut self,
        _: Option<&widget::Id>,
        bounds: Rectangle,
        state: &mut dyn widget::operation::Focusable,
    ) {
        if self.target.is_none() && state.is_focused() {
            self.reveal(bounds);
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
/// Scroll so the container with this id is in view, without touching focus.
/// Ids the view hands out for this: `notification-question-<id>`,
/// `notification-message-<id>`, `notification-channel-<id>`.
// Wired by notification routing in `app/shell.rs`; unused until that lands.
#[allow(dead_code)]
pub fn reveal(id: impl Into<String>) -> iced::Task<Message> {
    iced::advanced::widget::operate(Reveal {
        target: Some(widget::Id::from(id.into())),
        ..Reveal::default()
    })
    .discard()
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
                        radius: 9.0.into(),
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

/// How a button asks to be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The one thing to do on this screen: filled with the accent.
    Primary,
    /// An ordinary action: a quiet raised surface.
    Secondary,
    /// Rows and navigation: no surface until hovered or selected.
    Quiet,
    /// A section switch: an underline rather than a surface.
    Tab,
    /// An action with a cost that has already been armed once.
    Danger,
    /// One choice in a segmented control: the selected one is lifted off
    /// the track, the others sit quietly on it.
    Segment,
}

fn button_style(
    theme: &iced::Theme,
    status: iced::widget::button::Status,
    kind: Kind,
    selected: bool,
) -> iced::widget::button::Style {
    use crate::app::style::{Colors, alpha, mix};
    use iced::widget::button::Status;
    let c = Colors::of(theme);
    let (mut background, mut text) = match (kind, selected) {
        (Kind::Primary, _) => (Some(c.accent), iced::Color::WHITE),
        (Kind::Danger, _) => (Some(alpha(c.red, if c.dark { 0.18 } else { 0.12 })), c.red),
        (Kind::Tab, true) => (None, c.accent_ink),
        (Kind::Segment, true) => (Some(if c.dark { c.ground } else { c.card }), c.text),
        (Kind::Segment, false) => (None, c.muted),
        (_, true) => (Some(c.accent_soft), c.accent_ink),
        (Kind::Secondary, false) => (Some(c.raised), c.text),
        (Kind::Quiet | Kind::Tab, false) => (None, c.text),
    };
    let lift = |amount: f32| match (kind, selected) {
        (Kind::Primary, _) => mix(c.accent, iced::Color::BLACK, amount),
        (Kind::Danger, _) => alpha(c.red, 0.18 + amount),
        (Kind::Tab, _) => alpha(c.raised, 0.7 + amount),
        (Kind::Segment, true) => mix(if c.dark { c.ground } else { c.card }, c.text, amount * 0.3),
        (Kind::Segment, false) => alpha(c.text, amount * 0.6),
        (_, true) => mix(c.accent_soft, c.accent, amount * 0.6),
        (Kind::Secondary, false) => mix(c.raised, c.text, amount * 0.5),
        (Kind::Quiet, false) => c.raised,
    };
    match status {
        Status::Active => {}
        Status::Hovered => background = Some(lift(0.08)),
        Status::Pressed => background = Some(lift(0.16)),
        Status::Disabled => {
            text = alpha(text, 0.45);
            background = background.map(|b| alpha(b, 0.5));
        }
    }
    iced::widget::button::Style {
        background: background.map(Into::into),
        text_color: text,
        border: Border {
            radius: if kind == Kind::Segment { 7.0 } else { 9.0 }.into(),
            ..Default::default()
        },
        shadow: if kind == Kind::Segment && selected && status != Status::Disabled {
            iced::Shadow {
                color: iced::Color::from_rgba8(16, 24, 40, if c.dark { 0.4 } else { 0.1 }),
                offset: iced::Vector::new(0.0, 1.0),
                blur_radius: 2.0,
            }
        } else {
            Default::default()
        },
        snap: true,
    }
}

/// A labelled action; `selected` marks the current choice among peers.
pub fn button<'a>(
    id: impl Into<String>,
    label: impl Into<String>,
    message: Option<Message>,
    selected: bool,
) -> Element<'a, Message> {
    let label = label.into();
    let content = iced::widget::text(label.clone()).size(14);
    custom(
        id,
        label,
        content,
        message,
        selected,
        Kind::Secondary,
        [9, 13],
    )
}
/// The one action a screen leads with.
pub fn primary<'a>(
    id: impl Into<String>,
    label: impl Into<String>,
    message: Option<Message>,
) -> Element<'a, Message> {
    let label = label.into();
    // Regular weight: the heavier faces of the system sans lack the
    // ellipsis glyph and borrow it from a monospace fallback.
    let content = iced::widget::text(label.clone()).size(14);
    custom(id, label, content, message, false, Kind::Primary, [9, 15])
}
/// An armed destructive action.
pub fn danger<'a>(
    id: impl Into<String>,
    label: impl Into<String>,
    message: Option<Message>,
) -> Element<'a, Message> {
    let label = label.into();
    let content = iced::widget::text(label.clone()).size(14);
    custom(id, label, content, message, false, Kind::Danger, [9, 13])
}
/// One choice of a segmented control. Lay several in a `segmented` track.
pub fn segment<'a>(
    id: impl Into<String>,
    label: impl Into<String>,
    message: Option<Message>,
    selected: bool,
) -> Element<'a, Message> {
    let label = label.into();
    let content = iced::widget::text(label.clone())
        .size(13)
        .font(crate::app::style::weight(if selected {
            iced::font::Weight::Semibold
        } else {
            iced::font::Weight::Medium
        }));
    custom(
        id,
        label,
        content,
        message,
        selected,
        Kind::Segment,
        [6, 12],
    )
}
/// A full-width quiet row: sidebar entries and list rows.
pub fn block_button<'a>(
    id: impl Into<String>,
    label: impl Into<String>,
    message: Option<Message>,
    selected: bool,
) -> Element<'a, Message> {
    let label = label.into();
    let content = iced::widget::text(label.clone())
        .size(14)
        .width(Length::Fill);
    custom(id, label, content, message, selected, Kind::Quiet, [9, 12])
}
/// A section switch drawn as an underline.
pub fn tab<'a>(
    id: impl Into<String>,
    label: impl Into<String>,
    glyph: Option<Element<'a, Message>>,
    message: Option<Message>,
    selected: bool,
) -> Element<'a, Message> {
    let label = label.into();
    let mut title = iced::widget::row![].spacing(7).align_y(iced::Center);
    if let Some(glyph) = glyph {
        title = title.push(glyph);
    }
    title = title.push(
        iced::widget::text(label.clone())
            .size(14)
            .font(crate::app::style::weight(if selected {
                iced::font::Weight::Semibold
            } else {
                iced::font::Weight::Normal
            })),
    );
    let content = iced::widget::column![
        title,
        iced::widget::container(iced::widget::Space::new().width(Length::Fill).height(2)).style(
            move |theme: &iced::Theme| {
                let c = crate::app::style::Colors::of(theme);
                iced::widget::container::Style {
                    background: Some(
                        if selected {
                            c.accent
                        } else {
                            iced::Color::TRANSPARENT
                        }
                        .into(),
                    ),
                    border: Border {
                        radius: 1.0.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }
            }
        )
    ]
    .spacing(8);
    custom(id, label, content, message, selected, Kind::Tab, [8, 6])
}
/// Any content as a keyboard-focusable, accessible button. `label` is what
/// assistive technology reads; the content is what the eye does.
pub fn custom<'a>(
    id: impl Into<String>,
    label: impl Into<String>,
    content: impl Into<Element<'a, Message>>,
    message: Option<Message>,
    selected: bool,
    kind: Kind,
    padding: [u16; 2],
) -> Element<'a, Message> {
    let (id, label) = (id.into(), label.into());
    let content = iced::widget::button(content)
        .padding(padding)
        .width(if kind == Kind::Quiet {
            Length::Fill
        } else {
            Length::Shrink
        })
        .on_press_maybe(message.clone())
        .style(move |theme, status| button_style(theme, status, kind, selected));
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
        .padding([11, 13])
        .size(14)
        .style(|theme, status| {
            use crate::app::style::{Colors, alpha};
            use iced::widget::text_input::Status;
            let c = Colors::of(theme);
            let (border, background) = match status {
                Status::Focused { .. } => (c.accent, c.card),
                Status::Hovered => (alpha(c.accent, 0.6), c.card),
                Status::Active => (c.line, c.card),
                Status::Disabled => (c.line, c.raised),
            };
            iced::widget::text_input::Style {
                background: background.into(),
                border: Border {
                    color: border,
                    width: 1.0,
                    radius: 9.0.into(),
                },
                icon: c.muted,
                placeholder: c.faint,
                value: if matches!(status, Status::Disabled) {
                    c.muted
                } else {
                    c.text
                },
                selection: alpha(c.accent, 0.35),
            }
        })
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

    #[test]
    fn primary_labels_keep_contrast_during_pointer_interaction() {
        use crate::app::style::Colors;
        use iced::widget::button::Status;

        let luminance = |color: iced::Color| {
            let linear = |channel: f32| {
                if channel <= 0.04045 {
                    channel / 12.92
                } else {
                    ((channel + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * linear(color.r) + 0.7152 * linear(color.g) + 0.0722 * linear(color.b)
        };
        for dark in [false, true] {
            let theme = Colors::new(dark).theme();
            for status in [Status::Active, Status::Hovered, Status::Pressed] {
                let style = button_style(&theme, status, Kind::Primary, false);
                let Some(iced::Background::Color(background)) = style.background else {
                    panic!("primary button needs a solid background");
                };
                assert_eq!(style.text_color.a, 1.0);
                assert_eq!(background.a, 1.0);
                let foreground = luminance(style.text_color) + 0.05;
                let background = luminance(background) + 0.05;
                let contrast = foreground.max(background) / foreground.min(background);
                assert!(
                    contrast >= 4.5,
                    "dark={dark}, status={status:?}: {contrast}"
                );
            }
        }
    }

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
