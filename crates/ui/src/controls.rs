//! Keyboard-focusable Iced controls with native accessibility metadata.
use crate::{accessibility::Semantic, app::Message};
use iced::advanced::Renderer as _;
mod composer;
mod popover;
pub use composer::composer;
pub use popover::popover;

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
    /// Whether the focus came from a click. The ring is for the
    /// keyboard — it says where Tab has put the focus, which the pointer
    /// already knows — so a clicked control is focused without one, and
    /// the next key press shows it again.
    pub by_pointer: bool,
}
impl Focus {
    /// Whether to draw the ring: focused, and not by a click.
    pub fn ringed(&self) -> bool {
        self.focused && !self.by_pointer
    }
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
        self.by_pointer = false;
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
            state.by_pointer = false;
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
        // A key press is the keyboard asking where it is: whichever
        // control is focused shows its ring from here on.
        if matches!(event, Event::Keyboard(keyboard::Event::KeyPressed { .. })) {
            state.by_pointer = false;
        }
        if self.semantic.role != accesskit::Role::Button || self.semantic.action.is_some() {
            if matches!(
                event,
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
            ) && cursor.is_over(layout.bounds())
            {
                state.by_pointer = true;
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
        if self.button && tree.state.downcast_ref::<Focus>().ringed() {
            renderer.fill_quad(
                renderer::Quad {
                    bounds: layout.bounds(),
                    border: Border {
                        color: crate::app::style::alpha(theme.palette().primary, 0.75),
                        width: 2.0,
                        radius: crate::app::style::RADIUS_SM.into(),
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

/// How a button asks to be read. A screen has one `Primary`; everything
/// else steps down the ladder — `Secondary` outline, `Ghost` text — so
/// the eye finds the one thing to do before it reads the rest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The one thing to do on this screen: filled with the accent.
    Primary,
    /// An ordinary action: a hairline outline, no fill.
    Secondary,
    /// A tertiary action — Cancel, Dismiss, Details: muted words that
    /// gain a faint surface only under the pointer.
    Ghost,
    /// Rows: no surface until hovered or selected, and the row's whole
    /// width.
    Quiet,
    /// A word in a line — a name to hand to, an archive link: the quiet
    /// look at its own width, so several sit side by side.
    Inline,
    /// A section switch: an underline rather than a surface.
    Tab,
    /// An action with a cost that has already been armed once: solid red.
    Danger,
    /// A destructive menu entry before it is armed: red words, no surface.
    Destructive,
    /// One choice in a segmented control: the selected one is lifted off
    /// the track, the others sit quietly on it.
    Segment,
    /// A row of the navigation rail, inked with the rail's own roles.
    Nav,
}

/// The height every labelled control shares: a 13-point label on an
/// 18-point line inside 7 points of padding top and bottom is 32.
pub const LABEL_LINE: f32 = 18.0;

fn label<'a>(value: String, size: f32) -> iced::widget::Text<'a> {
    iced::widget::text(value)
        .size(size)
        .line_height(iced::Pixels(LABEL_LINE))
        .font(crate::app::style::weight(iced::font::Weight::Medium))
}

fn button_style(
    theme: &iced::Theme,
    status: iced::widget::button::Status,
    kind: Kind,
    selected: bool,
) -> iced::widget::button::Style {
    use crate::app::style::{Colors, RADIUS_MD, RADIUS_SM, alpha, mix};
    use iced::widget::button::Status;
    let page = Colors::of(theme);
    let c = if kind == Kind::Nav { page.rail() } else { page };
    let clear = iced::Color::TRANSPARENT;
    // Hover and press are washes of the ink over whatever the control
    // sits on, so one rule reads on the ground, a card and the rail.
    let wash = |amount: f32| alpha(c.text, amount);
    let (mut background, mut text, mut border) = match (kind, selected) {
        (Kind::Primary, _) => (c.accent, iced::Color::WHITE, clear),
        (Kind::Danger, _) => (c.danger, iced::Color::WHITE, clear),
        (Kind::Destructive, _) => (clear, c.red, clear),
        (Kind::Tab, true) => (clear, c.text, clear),
        (Kind::Tab, false) => (clear, c.muted, clear),
        (Kind::Segment, true) => (
            if c.dark { c.raised } else { c.card },
            c.text,
            if c.dark { c.line_strong } else { c.line },
        ),
        (Kind::Segment, false) => (clear, c.muted, clear),
        (Kind::Nav, true) => (c.raised, c.text, clear),
        (Kind::Nav, false) => (clear, mix(c.text, c.ground, 0.18), clear),
        (Kind::Secondary, true) => (c.accent_soft, c.accent_ink, alpha(c.accent, 0.35)),
        (_, true) => (c.accent_soft, c.accent_ink, clear),
        (Kind::Secondary, false) => (clear, c.text, c.line_strong),
        (Kind::Ghost, false) => (clear, c.muted, clear),
        (Kind::Quiet | Kind::Inline, false) => (clear, c.text, clear),
    };
    let lift = |pressed: bool| {
        let amount = if pressed { 0.08 } else { 0.05 };
        match (kind, selected) {
            (Kind::Primary, _) if pressed => mix(c.accent, iced::Color::BLACK, 0.12),
            (Kind::Primary, _) => c.accent_hover,
            (Kind::Danger, _) => mix(c.danger, iced::Color::BLACK, amount * 1.6),
            (Kind::Destructive, _) => alpha(c.red, amount * 2.0),
            (Kind::Tab, _) | (Kind::Segment, true) => background,
            (Kind::Nav, true) => mix(c.raised, c.text, amount * 0.5),
            (Kind::Secondary | Kind::Quiet | Kind::Inline | Kind::Ghost, true) => {
                mix(c.accent_soft, c.accent, amount * 1.2)
            }
            _ => wash(amount),
        }
    };
    match status {
        Status::Active => {}
        Status::Hovered | Status::Pressed => {
            background = lift(status == Status::Pressed);
            if matches!(kind, Kind::Ghost | Kind::Tab | Kind::Segment | Kind::Nav) && !selected {
                text = c.text;
            }
        }
        Status::Disabled => {
            text = alpha(text, 0.45);
            background = alpha(background, background.a * 0.5);
            border = alpha(border, border.a * 0.5);
        }
    }
    iced::widget::button::Style {
        background: (background.a > 0.0).then(|| background.into()),
        text_color: text,
        border: Border {
            color: border,
            width: if border.a > 0.0 { 1.0 } else { 0.0 },
            radius: if kind == Kind::Nav {
                RADIUS_MD
            } else {
                RADIUS_SM
            }
            .into(),
        },
        // The selected segment's lift: a few hundred pixels, light only;
        // in dark the hairline does it.
        shadow: if kind == Kind::Segment && selected && !c.dark && status != Status::Disabled {
            iced::Shadow {
                color: iced::Color::from_rgba8(16, 24, 40, 0.10),
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
    label_text: impl Into<String>,
    message: Option<Message>,
    selected: bool,
) -> Element<'a, Message> {
    let label_text = label_text.into();
    let content = label(label_text.clone(), 13.0);
    custom(
        id,
        label_text,
        content,
        message,
        selected,
        Kind::Secondary,
        [7, 12],
    )
}
/// A tertiary action: Cancel, Dismiss, Details, Later.
pub fn ghost<'a>(
    id: impl Into<String>,
    label_text: impl Into<String>,
    message: Option<Message>,
) -> Element<'a, Message> {
    let label_text = label_text.into();
    let content = label(label_text.clone(), 13.0);
    custom(
        id,
        label_text,
        content,
        message,
        false,
        Kind::Ghost,
        [7, 10],
    )
}
/// The one action a screen leads with.
pub fn primary<'a>(
    id: impl Into<String>,
    label_text: impl Into<String>,
    message: Option<Message>,
) -> Element<'a, Message> {
    let label_text = label_text.into();
    let content = label(label_text.clone(), 13.0);
    custom(
        id,
        label_text,
        content,
        message,
        false,
        Kind::Primary,
        [7, 14],
    )
}
/// An armed destructive action.
pub fn danger<'a>(
    id: impl Into<String>,
    label_text: impl Into<String>,
    message: Option<Message>,
) -> Element<'a, Message> {
    let label_text = label_text.into();
    let content = label(label_text.clone(), 13.0);
    custom(
        id,
        label_text,
        content,
        message,
        false,
        Kind::Danger,
        [7, 12],
    )
}
/// One choice of a segmented control. Lay several in a `segmented` track.
pub fn segment<'a>(
    id: impl Into<String>,
    label_text: impl Into<String>,
    message: Option<Message>,
    selected: bool,
) -> Element<'a, Message> {
    let label_text = label_text.into();
    let content = iced::widget::text(label_text.clone())
        .size(13)
        .font(crate::app::style::weight(iced::font::Weight::Medium));
    custom(
        id,
        label_text,
        content,
        message,
        selected,
        Kind::Segment,
        [5, 12],
    )
}
/// A full-width quiet row: sidebar entries and list rows.
pub fn block_button<'a>(
    id: impl Into<String>,
    label_text: impl Into<String>,
    message: Option<Message>,
    selected: bool,
) -> Element<'a, Message> {
    let label_text = label_text.into();
    let content = iced::widget::text(label_text.clone())
        .size(14)
        .width(Length::Fill);
    custom(
        id,
        label_text,
        content,
        message,
        selected,
        Kind::Quiet,
        [7, 10],
    )
}
/// A section switch drawn as an underline: the label, then a 2-point bar
/// in the accent under the selected one, sitting on the tab row's rule.
pub fn tab<'a>(
    id: impl Into<String>,
    label_text: impl Into<String>,
    glyph: Option<Element<'a, Message>>,
    message: Option<Message>,
    selected: bool,
) -> Element<'a, Message> {
    let label_text = label_text.into();
    let mut title = iced::widget::row![].spacing(7).align_y(iced::Center);
    if let Some(glyph) = glyph {
        title = title.push(glyph);
    }
    title = title.push(
        iced::widget::text(label_text.clone())
            .size(14)
            .font(crate::app::style::weight(iced::font::Weight::Medium)),
    );
    let content = iced::widget::column![
        iced::widget::container(title).padding([0, 2]),
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
    .spacing(9);
    custom(
        id,
        label_text,
        content,
        message,
        selected,
        Kind::Tab,
        [7, 6],
    )
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
    // Rows take the row's whole width; everything else its own.
    let width = if matches!(kind, Kind::Quiet | Kind::Nav | Kind::Destructive) {
        Length::Fill
    } else {
        Length::Shrink
    };
    custom_sized(id, label, content, message, selected, kind, padding, width)
}
/// [`custom`] at an explicit width: an icon button among rail rows.
#[allow(clippy::too_many_arguments)]
pub fn custom_sized<'a>(
    id: impl Into<String>,
    label: impl Into<String>,
    content: impl Into<Element<'a, Message>>,
    message: Option<Message>,
    selected: bool,
    kind: Kind,
    padding: [u16; 2],
    width: Length,
) -> Element<'a, Message> {
    build(
        id, label, content, message, selected, kind, padding, width, None,
    )
}

/// Which half of a split button a control is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Split {
    /// The action itself, square on its right.
    Start,
    /// The chevron that opens its menu, square on its left.
    End,
}

/// The labelled half of a split primary action.
pub fn split_primary<'a>(
    id: impl Into<String>,
    label_text: impl Into<String>,
    message: Option<Message>,
    side: Split,
) -> Element<'a, Message> {
    let label_text = label_text.into();
    let content = label(label_text.clone(), 13.0);
    build(
        id,
        label_text,
        content,
        message,
        false,
        Kind::Primary,
        [7, 14],
        Length::Shrink,
        Some(side),
    )
}

/// The chevron half of a split primary action.
pub fn split_primary_glyph<'a>(
    id: impl Into<String>,
    spoken: impl Into<String>,
    glyph: impl Into<Element<'a, Message>>,
    message: Option<Message>,
) -> Element<'a, Message> {
    let content = iced::widget::container(glyph).center_y(LABEL_LINE);
    build(
        id,
        spoken,
        content,
        message,
        false,
        Kind::Primary,
        [7, 9],
        Length::Shrink,
        Some(Split::End),
    )
}

#[allow(clippy::too_many_arguments)]
fn build<'a>(
    id: impl Into<String>,
    label: impl Into<String>,
    content: impl Into<Element<'a, Message>>,
    message: Option<Message>,
    selected: bool,
    kind: Kind,
    padding: [u16; 2],
    width: Length,
    split: Option<Split>,
) -> Element<'a, Message> {
    let (id, label) = (id.into(), label.into());
    let content = iced::widget::button(content)
        .padding(padding)
        .width(width)
        .on_press_maybe(message.clone())
        .style(move |theme, status| {
            let mut style = button_style(theme, status, kind, selected);
            let r = crate::app::style::RADIUS_SM;
            match split {
                Some(Split::Start) => {
                    style.border.radius = iced::border::left(r);
                }
                Some(Split::End) => {
                    style.border.radius = iced::border::right(r);
                }
                None => {}
            }
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
    input_submitting(id, label, value, change, enabled, None)
}

/// A one-line input that also sends on Enter: a composer. `submit` is
/// what Enter does when the words are ready to go — the same message the
/// button beside it sends — and nothing while they are not, so an empty
/// or already-sending draft is not sent twice by a second keystroke. The
/// accessibility node carries it as the input's action.
pub fn input_submitting<'a>(
    id: impl Into<String>,
    label: &str,
    value: &str,
    change: impl Fn(String) -> Message + Send + Sync + 'static,
    enabled: bool,
    submit: Option<Message>,
) -> Element<'a, Message> {
    let id = id.into();
    let change = std::sync::Arc::new(change);
    let on_input = change.clone();
    let content = iced::widget::text_input(label, value)
        .id(widget::Id::from(id.clone()))
        .padding([7, 10])
        .size(14)
        .line_height(iced::Pixels(LABEL_LINE))
        .style(|theme, status| {
            use crate::app::style::{Colors, RADIUS_SM, alpha, mix};
            use iced::widget::text_input::Status;
            let c = Colors::of(theme);
            // Focus is a wider accent edge, drawn inside the field so
            // nothing moves: colour alone would not be enough of a cue.
            let (border, width, background) = match status {
                Status::Focused { .. } => (alpha(c.accent, 0.8), 2.0, c.card),
                Status::Hovered => (mix(c.line_strong, c.muted, 0.45), 1.0, c.card),
                Status::Active => (c.line_strong, 1.0, c.card),
                Status::Disabled => (c.line, 1.0, c.raised),
            };
            iced::widget::text_input::Style {
                background: background.into(),
                border: Border {
                    color: border,
                    width,
                    radius: RADIUS_SM.into(),
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
        .on_input_maybe(enabled.then_some(move |v| on_input(v)))
        .on_submit_maybe(submit.clone());
    let mut semantic = Semantic::input(id, label.into(), value.into(), change);
    if !enabled {
        semantic.change = None;
    }
    semantic.action = submit;
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
    /// A click focuses a control without a ring — the pointer knows
    /// where it clicked — and the next key press shows the ring on
    /// whatever is focused; losing focus forgets the click.
    #[test]
    fn a_clicked_control_is_focused_without_a_ring_until_the_keyboard_asks() {
        use iced::advanced::widget::operation::Focusable;
        let renderer = iced::Renderer::new(iced::Font::DEFAULT, 14.0.into());
        let mut element = button("add", "Add project", Some(Message::ShowAdd), false);
        let mut tree = Tree::new(&element);
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, Size::new(400.0, 100.0)),
        );
        let viewport = Rectangle::with_size(Size::new(400.0, 100.0));
        let update = |element: &mut Element<'_, Message>, tree: &mut Tree, event: &Event| {
            let mut messages = Vec::new();
            let mut shell = Shell::new(&mut messages);
            element.as_widget_mut().update(
                tree,
                event,
                Layout::new(&node),
                mouse::Cursor::Available(iced::Point::new(4.0, 4.0)),
                &renderer,
                &mut iced::advanced::clipboard::Null,
                &mut shell,
                &viewport,
            );
            messages
        };
        let click = Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));
        let clicked = update(&mut element, &mut tree, &click);
        assert!(
            matches!(clicked.as_slice(), [Message::Focus(id)] if id == "add"),
            "a click asks for the focus"
        );
        tree.state.downcast_mut::<Focus>().focus();
        assert!(
            !tree.state.downcast_ref::<Focus>().ringed(),
            "focused by a click: no ring"
        );
        update(
            &mut element,
            &mut tree,
            &press(keyboard::Key::Named(keyboard::key::Named::Tab), false),
        );
        assert!(
            tree.state.downcast_ref::<Focus>().ringed(),
            "the keyboard asked: the ring shows"
        );
        tree.state.downcast_mut::<Focus>().unfocus();
        update(&mut element, &mut tree, &click);
        tree.state.downcast_mut::<Focus>().focus();
        assert!(!tree.state.downcast_ref::<Focus>().ringed());
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
