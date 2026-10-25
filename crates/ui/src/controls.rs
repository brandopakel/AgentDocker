//! Keyboard-focusable Iced controls with native accessibility metadata.
use crate::{accessibility::Semantic, app::Message};
use iced::advanced::Renderer as _;
mod composer;
mod popover;
mod reveal;
pub use composer::{ComposerSpec, composer, framed_composer};
pub use popover::popover;
pub use reveal::hover;

/// Scroll ancestors just enough to reveal the newly focused control, or,
/// given a `target`, the container carrying that id. Revealing a target
/// moves nothing but scroll offsets: focus stays where it was.
#[derive(Default)]
struct Reveal {
    target: Option<widget::Id>,
    pending: Option<Parent>,
    parents: Vec<Parent>,
    adjustments: Vec<(widget::Id, iced::Vector)>,
}
/// A scrollable around the control being revealed: its id, its viewport,
/// its translation from the top, and, when it is anchored to its end, how
/// far it can scroll.
#[derive(Clone)]
struct Parent {
    id: widget::Id,
    viewport: Rectangle,
    offset: iced::Vector,
    from_end: Option<f32>,
}
/// The scrollables anchored to their end: the message histories, whose ids
/// all start `history-`. Iced reports their translation from the top but
/// takes `scroll_to` offsets from the end, and its operations do not say
/// which anchor a scrollable has, so the convention is how this knows.
fn anchored_to_end(id: &widget::Id) -> bool {
    format!("{id:?}").contains("\"history-")
}
/// The offset `scroll_to` wants for a translation from the top: the same
/// value, or for an end-anchored scrollable its distance from the end.
fn scroll_offset(translation: f32, from_end: Option<f32>) -> f32 {
    match from_end {
        Some(room) => (room - translation).max(0.0),
        None => translation,
    }
}
impl Reveal {
    fn reveal(&mut self, bounds: Rectangle) {
        for Parent {
            id,
            viewport,
            offset,
            from_end,
        } in &self.parents
        {
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
                self.adjustments.push((
                    id.clone(),
                    iced::Vector::new(want.x, scroll_offset(want.y, *from_end)),
                ));
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
        content: Rectangle,
        translation: iced::Vector,
        _: &mut dyn widget::operation::Scrollable,
    ) {
        self.pending = id.cloned().map(|id| Parent {
            from_end: anchored_to_end(&id).then(|| (content.height - bounds.height).max(0.0)),
            id,
            viewport: bounds,
            offset: translation,
        });
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

/// Whether a key press is the keyboard moving or using the focus: Tab, F6,
/// an arrow, Enter or Space. A modifier on its own, a Command or Control
/// shortcut, Escape, a letter (the board's own keys are letters) and the
/// keys that only scroll do not.
fn navigates(key: &keyboard::Key, modifiers: keyboard::Modifiers) -> bool {
    use keyboard::key::Named;
    if modifiers.command() || modifiers.control() || modifiers.alt() {
        return false;
    }
    match key {
        keyboard::Key::Named(named) => matches!(
            named,
            Named::Tab
                | Named::F6
                | Named::ArrowUp
                | Named::ArrowDown
                | Named::ArrowLeft
                | Named::ArrowRight
                | Named::Enter
                | Named::Space
        ),
        keyboard::Key::Character(c) => c.as_str() == " ",
        keyboard::Key::Unidentified => false,
    }
}

/// Focus the control with this id the way a click does: without its
/// ring, whatever widget state the control has by the time the operation
/// runs — the click may have moved it, opening something or reordering a
/// list, and a moved control starts from fresh state. A target that
/// already has the focus keeps it as it is, so a text field keeps the
/// caret and the selection the click just made. Everything else loses
/// the focus: one thing has it at a time.
pub fn focus_by_pointer(id: impl Into<String>) -> iced::Task<Message> {
    iced::advanced::widget::operate(PointerFocus(widget::Id::from(id.into()))).discard()
}
struct PointerFocus(widget::Id);
impl Operation for PointerFocus {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        operate(self);
    }
    fn custom(&mut self, id: Option<&widget::Id>, _: Rectangle, state: &mut dyn std::any::Any) {
        if id == Some(&self.0)
            && let Some(focus) = state.downcast_mut::<Focus>()
        {
            focus.by_pointer = true;
        }
    }
    fn focusable(
        &mut self,
        id: Option<&widget::Id>,
        _: Rectangle,
        state: &mut dyn widget::operation::Focusable,
    ) {
        if id == Some(&self.0) {
            if !state.is_focused() {
                state.focus();
            }
        } else {
            state.unfocus();
        }
    }
}

/// Move the focus to the next control (or the previous one), as Tab and
/// F6 do, leaving nothing else focused. Iced's own step unfocuses only the
/// control it steps from, so a second focus — a field that focused itself
/// under a dialog, say — would otherwise stay, ringed, wherever it was.
pub fn step_focus(forward: bool) -> iced::Task<Message> {
    iced::advanced::widget::operate(Count {
        forward,
        total: 0,
        focused: None,
    })
    .discard()
}
/// The first pass of [`step_focus`]: how many focusables, and which one
/// has the focus (the last, when more than one does).
struct Count {
    forward: bool,
    total: usize,
    focused: Option<usize>,
}
impl Operation for Count {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        operate(self);
    }
    fn focusable(
        &mut self,
        _: Option<&widget::Id>,
        _: Rectangle,
        state: &mut dyn widget::operation::Focusable,
    ) {
        if state.is_focused() {
            self.focused = Some(self.total);
        }
        self.total += 1;
    }
    fn finish(&self) -> widget::operation::Outcome<()> {
        // As Iced steps: from nothing to the first (or the last), and past
        // either end to nothing, so the next step starts over.
        let target = match (self.focused, self.forward) {
            (None, true) => (self.total > 0).then_some(0),
            (None, false) => self.total.checked_sub(1),
            (Some(focused), true) => (focused + 1 < self.total).then_some(focused + 1),
            (Some(focused), false) => focused.checked_sub(1),
        };
        widget::operation::Outcome::Chain(Box::new(Step { target, current: 0 }))
    }
}
/// The second pass of [`step_focus`]: focus the target, unfocus the rest.
struct Step {
    target: Option<usize>,
    current: usize,
}
impl Operation for Step {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        operate(self);
    }
    fn focusable(
        &mut self,
        _: Option<&widget::Id>,
        _: Rectangle,
        state: &mut dyn widget::operation::Focusable,
    ) {
        if self.target == Some(self.current) {
            if !state.is_focused() {
                state.focus();
            }
        } else {
            state.unfocus();
        }
        self.current += 1;
    }
}

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
impl Control<'_> {
    /// Whether the keyboard can reach it: an enabled button.
    fn focusable(&self) -> bool {
        self.button && self.semantic.action.is_some()
    }
    /// Whether a press on it moves the focus to it: an enabled button, or
    /// a field that takes typing. A label — a chip, a status, words read
    /// as one — never does, so a press on one inside a row is the row's.
    fn takes_press(&self) -> bool {
        use accesskit::Role;
        self.focusable()
            || (matches!(
                self.semantic.role,
                Role::TextInput | Role::MultilineTextInput | Role::PasswordInput
            ) && self.semantic.change.is_some())
    }
}

/// Whether a control in `tree` has the focus — the keyboard's only, when
/// `ringed`.
pub(crate) fn focused_within(tree: &Tree, ringed: bool) -> bool {
    (tree.tag == tree::Tag::of::<Focus>() && {
        let focus = tree.state.downcast_ref::<Focus>();
        if ringed {
            focus.ringed()
        } else {
            focus.focused
        }
    }) || tree
        .children
        .iter()
        .any(|child| focused_within(child, ringed))
}
impl Widget<Message, iced::Theme, iced::Renderer> for Control<'_> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<Focus>()
    }
    fn state(&self) -> tree::State {
        // Fresh state already belongs to this control: a focus given to it
        // before the next rebuild is its own, not a stranger's to drop.
        tree::State::new(Focus {
            id: Some(self.semantic.id.clone()),
            ..Focus::default()
        })
    }
    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }
    fn diff(&self, tree: &mut Tree) {
        let state = tree.state.downcast_mut::<Focus>();
        // Another control's state, or a control that can no longer be
        // pressed: neither keeps a focus, or a ring, nothing can move on.
        if state.id.as_deref() != Some(self.semantic.id.as_str()) || !self.focusable() {
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
        if self.focusable() {
            let focus = tree.state.downcast_mut::<Focus>();
            // The focus itself as well, for the operations that need to
            // know where it came from: a click's carries no ring.
            operation.custom(Some(&id), layout.bounds(), focus);
            operation.focusable(Some(&id), layout.bounds(), focus);
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
        // A key that moves or uses the focus is the keyboard asking where it
        // is: whichever control is focused shows its ring from here on. A
        // modifier, a shortcut or Escape is not, so a screenshot's Cmd-Shift
        // or closing a menu leaves a clicked control without one.
        if let Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) = event
            && navigates(key, *modifiers)
        {
            state.by_pointer = false;
        }
        // A click anywhere else takes the focus away, as a click on the
        // page does in a browser: nothing stays focused, or ringed, after
        // the pointer has moved on.
        if state.focused
            && matches!(
                event,
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
            )
            && !cursor.is_over(layout.bounds())
        {
            state.focused = false;
            state.by_pointer = false;
            // Its ring goes now, and the screen reader hears the focus left.
            shell.request_redraw();
            shell.publish(Message::Blurred);
        }
        // A press on it: the focus comes here, by the pointer. The
        // operation that moves it runs after the view is rebuilt, and marks
        // whatever state this control has then.
        if self.takes_press()
            && matches!(
                event,
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
            )
            && cursor.is_over(layout.bounds())
        {
            state.by_pointer = true;
            shell.publish(Message::Pressed(self.semantic.id.clone()));
        }
        // Enter or Space presses a focused button, once — unless a widget
        // before it in the window already took the key (a field sending on
        // Enter, say): one key, one action.
        if state.focused
            && !shell.is_event_captured()
            && let Some(action) = self.semantic.action.clone().filter(|_| self.button)
            && let Event::Keyboard(keyboard::Event::KeyPressed { key, repeat, .. }) = event
            && (matches!(
                key,
                keyboard::Key::Named(keyboard::key::Named::Enter | keyboard::key::Named::Space)
            ) || matches!(key, keyboard::Key::Character(c) if c.as_str() == " "))
        {
            if !repeat {
                // The keyboard pressed it, said first: a menu it opens was
                // opened from the keyboard, whatever the pointer did before.
                shell.publish(Message::KeyPressedControl);
                shell.publish(action);
            }
            shell.capture_event();
            return;
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
        if self.focusable() && tree.state.downcast_ref::<Focus>().ringed() {
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
        (Kind::Primary, _) => (c.primary, c.on_primary, clear),
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
        (Kind::Secondary, true) => (c.raised, c.text, c.line_strong),
        (_, true) => (c.raised, c.text, clear),
        (Kind::Secondary, false) => (clear, c.text, c.line_strong),
        (Kind::Ghost, false) => (clear, c.muted, clear),
        (Kind::Quiet | Kind::Inline, false) => (clear, c.text, clear),
    };
    let lift = |pressed: bool| {
        let amount = if pressed { 0.08 } else { 0.05 };
        match (kind, selected) {
            (Kind::Primary, _) if pressed => mix(c.primary_hover, c.ground, 0.12),
            (Kind::Primary, _) => c.primary_hover,
            (Kind::Danger, _) => mix(c.danger, iced::Color::BLACK, amount * 1.6),
            (Kind::Destructive, _) => alpha(c.red, amount * 2.0),
            (Kind::Tab, _) | (Kind::Segment, true) => background,
            (Kind::Nav, true) => mix(c.raised, c.text, amount * 0.5),
            (Kind::Secondary | Kind::Quiet | Kind::Inline | Kind::Ghost, true) => {
                mix(c.raised, c.text, amount)
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
                            c.text
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
// The control ladder's entry from fifty screens; its arguments are the
// control's few properties, read in one order everywhere, so a struct would
// only lengthen every call.
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
    build(Spec {
        id: id.into(),
        label: label.into(),
        content: content.into(),
        message,
        selected,
        kind,
        padding,
        width,
        split: None,
    })
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
    build(Spec {
        id: id.into(),
        label: label_text,
        content: content.into(),
        message,
        selected: false,
        kind: Kind::Primary,
        padding: [7, 14],
        width: Length::Shrink,
        split: Some(side),
    })
}

/// The chevron half of a split primary action.
pub fn split_primary_glyph<'a>(
    id: impl Into<String>,
    spoken: impl Into<String>,
    glyph: impl Into<Element<'a, Message>>,
    message: Option<Message>,
) -> Element<'a, Message> {
    let content = iced::widget::container(glyph).center_y(LABEL_LINE);
    build(Spec {
        id: id.into(),
        label: spoken.into(),
        content: content.into(),
        message,
        selected: false,
        kind: Kind::Primary,
        padding: [7, 9],
        width: Length::Shrink,
        split: Some(Split::End),
    })
}

/// What a button is built from: its id and spoken label, the content the
/// eye sees, what pressing sends, its state, weight and shape, and which
/// half of a split button it is.
struct Spec<'a> {
    id: String,
    label: String,
    content: Element<'a, Message>,
    message: Option<Message>,
    selected: bool,
    kind: Kind,
    padding: [u16; 2],
    width: Length,
    split: Option<Split>,
}

fn build<'a>(spec: Spec<'a>) -> Element<'a, Message> {
    let Spec {
        id,
        label,
        content,
        message,
        selected,
        kind,
        padding,
        width,
        split,
    } = spec;
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

/// Password entry never places plaintext on the accessibility tree. Iced's
/// secure input masks rendering and disables copying/cutting the selection.
pub fn secret_input<'a>(
    id: impl Into<String>,
    label: &str,
    value: &str,
    change: impl Fn(String) -> Message + Send + Sync + 'static,
    enabled: bool,
) -> Element<'a, Message> {
    let id = id.into();
    let change = std::sync::Arc::new(change);
    let on_input = change.clone();
    let content = iced::widget::text_input("Temporary answer", value)
        .id(widget::Id::from(id.clone()))
        .secure(true)
        .padding([7, 10])
        .size(14)
        .on_input_maybe(enabled.then_some(move |value| on_input(value)));
    let mut semantic = Semantic::password(id, label.into(), change);
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
    fn an_end_anchored_history_is_scrolled_from_its_end() {
        assert!(anchored_to_end(&widget::Id::from(
            "history-conv-1".to_owned()
        )));
        assert!(!anchored_to_end(&widget::Id::from("workspace-scroll")));
        assert!(!anchored_to_end(&widget::Id::from(
            "notification-question-history-1".to_owned()
        )));
        // Revealing something 120 from the top of a history that can
        // scroll 500 asks for 380 from its end, never the mirror image.
        assert_eq!(scroll_offset(120.0, Some(500.0)), 380.0);
        assert_eq!(scroll_offset(120.0, None), 120.0);
        assert_eq!(scroll_offset(600.0, Some(500.0)), 0.0);
    }

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
                // The keyboard says it pressed the button, then the action.
                if expected == 1 {
                    assert!(
                        matches!(
                            messages.as_slice(),
                            [Message::KeyPressedControl, Message::ShowAdd]
                        ),
                        "{messages:?}"
                    );
                } else {
                    assert!(messages.is_empty(), "{messages:?}");
                }
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
            matches!(clicked.as_slice(), [Message::Pressed(id)] if id == "add"),
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

        // A modifier, a shortcut, Escape, a letter (the board's own keys)
        // or a key that only scrolls, after a click, shows no ring.
        for (key, modifiers) in [
            (
                keyboard::Key::Named(keyboard::key::Named::Shift),
                keyboard::Modifiers::SHIFT,
            ),
            (
                keyboard::Key::Character("4".into()),
                keyboard::Modifiers::COMMAND | keyboard::Modifiers::SHIFT,
            ),
            (
                keyboard::Key::Character("n".into()),
                keyboard::Modifiers::empty(),
            ),
            (
                keyboard::Key::Named(keyboard::key::Named::PageDown),
                keyboard::Modifiers::empty(),
            ),
            (
                keyboard::Key::Named(keyboard::key::Named::Escape),
                keyboard::Modifiers::empty(),
            ),
        ] {
            update(
                &mut element,
                &mut tree,
                &Event::Keyboard(keyboard::Event::KeyPressed {
                    key: key.clone(),
                    modified_key: key.clone(),
                    physical_key: keyboard::key::Physical::Unidentified(
                        keyboard::key::NativeCode::Unidentified,
                    ),
                    location: keyboard::Location::Standard,
                    modifiers,
                    text: None,
                    repeat: false,
                }),
            );
            assert!(
                !tree.state.downcast_ref::<Focus>().ringed(),
                "{key:?} is not the keyboard asking"
            );
        }

        // A click elsewhere takes the focus away.
        let mut messages = Vec::new();
        let mut shell = Shell::new(&mut messages);
        element.as_widget_mut().update(
            &mut tree,
            &click,
            Layout::new(&node),
            mouse::Cursor::Available(iced::Point::new(390.0, 90.0)),
            &renderer,
            &mut iced::advanced::clipboard::Null,
            &mut shell,
            &viewport,
        );
        let redraw = shell.redraw_request();
        drop(shell);
        assert!(
            !tree.state.downcast_ref::<Focus>().focused,
            "a click elsewhere blurs it"
        );
        assert_ne!(
            redraw,
            iced::window::RedrawRequest::Wait,
            "the ring goes now, not at the next sweep"
        );
        assert!(
            matches!(messages.as_slice(), [Message::Blurred]),
            "the app hears it, to tell assistive technology: {messages:?}"
        );
    }
    /// The keys that show the ring are the ones that move or use the
    /// focus, and only those.
    #[test]
    fn only_the_keyboard_moving_or_using_the_focus_shows_the_ring() {
        use keyboard::key::Named;
        let plain = keyboard::Modifiers::empty();
        for key in [
            Named::Tab,
            Named::F6,
            Named::ArrowUp,
            Named::ArrowDown,
            Named::ArrowLeft,
            Named::ArrowRight,
            Named::Enter,
            Named::Space,
        ] {
            assert!(navigates(&keyboard::Key::Named(key), plain), "{key:?}");
            assert!(navigates(
                &keyboard::Key::Named(key),
                keyboard::Modifiers::SHIFT
            ));
        }
        assert!(navigates(&keyboard::Key::Character(" ".into()), plain));
        for key in [
            keyboard::Key::Named(Named::Escape),
            keyboard::Key::Named(Named::Shift),
            keyboard::Key::Named(Named::Home),
            keyboard::Key::Named(Named::PageUp),
            keyboard::Key::Character("v".into()),
            keyboard::Key::Character("/".into()),
            keyboard::Key::Unidentified,
        ] {
            assert!(!navigates(&key, plain), "{key:?}");
        }
        for modifiers in [
            keyboard::Modifiers::COMMAND,
            keyboard::Modifiers::CTRL,
            keyboard::Modifiers::ALT,
        ] {
            assert!(!navigates(&keyboard::Key::Named(Named::Tab), modifiers));
        }
    }

    /// Lay out `element` in a 400 by 300 window with fresh state.
    fn laid_out(element: &mut Element<'_, Message>) -> (Tree, layout::Node) {
        let renderer = iced::Renderer::new(iced::Font::DEFAULT, 14.0.into());
        let mut tree = Tree::new(&*element);
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, Size::new(400.0, 300.0)),
        );
        (tree, node)
    }
    /// Run an operation on a laid-out element, and the ones it chains.
    fn run(
        element: &mut Element<'_, Message>,
        tree: &mut Tree,
        node: &layout::Node,
        operation: Box<dyn Operation>,
    ) {
        let renderer = iced::Renderer::new(iced::Font::DEFAULT, 14.0.into());
        let mut next = Some(operation);
        while let Some(mut operation) = next.take() {
            element
                .as_widget_mut()
                .operate(tree, Layout::new(node), &renderer, operation.as_mut());
            if let widget::operation::Outcome::Chain(chained) = operation.finish() {
                next = Some(chained);
            }
        }
    }
    /// Each button's focus, in order: (focused, ringed).
    fn focuses(tree: &Tree) -> Vec<(bool, bool)> {
        tree.children
            .iter()
            .map(|child| {
                let focus = child.state.downcast_ref::<Focus>();
                (focus.focused, focus.ringed())
            })
            .collect()
    }
    fn three() -> Element<'static, Message> {
        iced::widget::column![
            button("one", "One", Some(Message::ShowAdd), false),
            button("two", "Two", Some(Message::ShowAdd), false),
            button("three", "Three", Some(Message::ShowAdd), false),
        ]
        .into()
    }

    /// A control's fresh widget state is its own: the focus an operation
    /// gives it before the next rebuild survives that rebuild. (It used to
    /// be dropped there, as if it were another control's, which lost the
    /// focus Enter had just put into a menu.)
    #[test]
    fn a_focus_given_to_fresh_state_survives_the_next_rebuild() {
        let mut element = three();
        let (mut tree, node) = laid_out(&mut element);
        run(
            &mut element,
            &mut tree,
            &node,
            Box::new(widget::operation::focusable::focus::<()>(widget::Id::from(
                "two",
            ))),
        );
        element.as_widget().diff(&mut tree);
        assert_eq!(
            focuses(&tree),
            [(false, false), (true, true), (false, false)]
        );
    }

    /// The pointer's focus is marked on whatever state the control has when
    /// the operation runs — fresh state, after a click that moved it,
    /// included — and nothing else keeps a focus.
    #[test]
    fn the_pointer_focus_is_quiet_wherever_the_control_now_is() {
        let mut element = three();
        let (mut tree, node) = laid_out(&mut element);
        tree.children[0].state.downcast_mut::<Focus>().focused = true;
        run(
            &mut element,
            &mut tree,
            &node,
            Box::new(PointerFocus(widget::Id::from("three"))),
        );
        assert_eq!(
            focuses(&tree),
            [(false, false), (false, false), (true, false)]
        );
        // A key that moves or uses the focus shows the ring again.
        let renderer = iced::Renderer::new(iced::Font::DEFAULT, 14.0.into());
        let mut messages = Vec::new();
        let mut shell = Shell::new(&mut messages);
        element.as_widget_mut().update(
            &mut tree,
            &press(keyboard::Key::Named(keyboard::key::Named::ArrowDown), false),
            Layout::new(&node),
            mouse::Cursor::Unavailable,
            &renderer,
            &mut iced::advanced::clipboard::Null,
            &mut shell,
            &Rectangle::with_size(Size::new(400.0, 300.0)),
        );
        assert_eq!(focuses(&tree)[2], (true, true));
    }

    /// A text field the click has already focused keeps its caret and its
    /// selection: the pointer's focus does not focus it again, which would
    /// put the caret at the end of the draft.
    #[test]
    fn the_pointer_focus_leaves_a_focused_field_as_the_click_left_it() {
        #[derive(Default)]
        struct Field {
            focused: bool,
            focus_calls: usize,
        }
        impl widget::operation::Focusable for Field {
            fn is_focused(&self) -> bool {
                self.focused
            }
            fn focus(&mut self) {
                self.focused = true;
                self.focus_calls += 1;
            }
            fn unfocus(&mut self) {
                self.focused = false;
            }
        }
        let id = widget::Id::from("draft");
        let mut operation = PointerFocus(id.clone());
        let mut clicked = Field {
            focused: true,
            ..Field::default()
        };
        operation.focusable(Some(&id), Rectangle::default(), &mut clicked);
        assert!(clicked.focused);
        assert_eq!(clicked.focus_calls, 0, "already focused: left alone");
        let mut moved = Field::default();
        operation.focusable(Some(&id), Rectangle::default(), &mut moved);
        assert!(moved.focused, "fresh state is focused");
        let mut other = Field {
            focused: true,
            ..Field::default()
        };
        operation.focusable(
            Some(&widget::Id::from("elsewhere")),
            Rectangle::default(),
            &mut other,
        );
        assert!(!other.focused, "one focus at a time");
    }

    /// Tab steps from the focus to the next control, and leaves nothing
    /// else focused: a second focus — a field that focused itself where a
    /// click could not reach the rest — is not left behind with a ring.
    #[test]
    fn tab_leaves_one_focus_even_where_two_were() {
        let mut element = three();
        let (mut tree, node) = laid_out(&mut element);
        for at in [0, 1] {
            tree.children[at].state.downcast_mut::<Focus>().focused = true;
        }
        run(
            &mut element,
            &mut tree,
            &node,
            Box::new(Count {
                forward: true,
                total: 0,
                focused: None,
            }),
        );
        assert_eq!(
            focuses(&tree),
            [(false, false), (false, false), (true, true)]
        );
        // Past the end, nothing; then the first again. Backwards likewise.
        for (forward, expected) in [
            (true, [(false, false), (false, false), (false, false)]),
            (true, [(true, true), (false, false), (false, false)]),
            (false, [(false, false), (false, false), (false, false)]),
            (false, [(false, false), (false, false), (true, true)]),
            (false, [(false, false), (true, true), (false, false)]),
        ] {
            run(
                &mut element,
                &mut tree,
                &node,
                Box::new(Count {
                    forward,
                    total: 0,
                    focused: None,
                }),
            );
            assert_eq!(focuses(&tree), expected, "forward={forward}");
        }
    }

    /// A focused control that can no longer be pressed lets the focus go,
    /// and its ring with it: nothing could move a focus on from there.
    #[test]
    fn a_control_that_becomes_disabled_loses_its_focus_and_ring() {
        let enabled = button("reconnect", "Reconnect here", Some(Message::ShowAdd), false);
        let mut tree = Tree::new(&enabled);
        tree.state.downcast_mut::<Focus>().focused = true;
        enabled.as_widget().diff(&mut tree);
        assert!(tree.state.downcast_ref::<Focus>().focused);
        let disabled = button("reconnect", "Reconnecting…", None, false);
        disabled.as_widget().diff(&mut tree);
        assert!(!tree.state.downcast_ref::<Focus>().focused);
    }

    /// One key, one action: Enter that a widget before the button already
    /// took (a field sending its words) does not press the button too.
    #[test]
    fn a_key_already_taken_does_not_press_a_focused_button() {
        let renderer = iced::Renderer::new(iced::Font::DEFAULT, 14.0.into());
        let mut element = button("add", "Add project", Some(Message::ShowAdd), false);
        let (mut tree, node) = laid_out(&mut element);
        tree.state.downcast_mut::<Focus>().focused = true;
        let mut messages = Vec::new();
        let mut shell = Shell::new(&mut messages);
        shell.capture_event();
        element.as_widget_mut().update(
            &mut tree,
            &press(keyboard::Key::Named(keyboard::key::Named::Enter), false),
            Layout::new(&node),
            mouse::Cursor::Unavailable,
            &renderer,
            &mut iced::advanced::clipboard::Null,
            &mut shell,
            &Rectangle::with_size(Size::new(400.0, 300.0)),
        );
        drop(shell);
        assert!(messages.is_empty(), "{messages:?}");
    }

    /// A label inside a row — a status chip, say — is read, not pressed:
    /// a press on it is the row's, which takes the pointer's focus.
    #[test]
    fn a_press_on_a_label_in_a_row_is_the_rows() {
        let renderer = iced::Renderer::new(iced::Font::DEFAULT, 14.0.into());
        let chip: Element<'_, Message> = Control {
            content: iced::widget::Space::new().width(60).height(16).into(),
            semantic: Semantic {
                role: accesskit::Role::Label,
                ..Semantic::button("chip".into(), "Running".into(), None)
            },
            button: false,
        }
        .into();
        let mut element = custom(
            "row",
            "A session",
            chip,
            Some(Message::ShowAdd),
            false,
            Kind::Quiet,
            [0, 0],
        );
        let (mut tree, node) = laid_out(&mut element);
        let mut messages = Vec::new();
        let mut shell = Shell::new(&mut messages);
        element.as_widget_mut().update(
            &mut tree,
            &Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            Layout::new(&node),
            mouse::Cursor::Available(iced::Point::new(5.0, 5.0)),
            &renderer,
            &mut iced::advanced::clipboard::Null,
            &mut shell,
            &Rectangle::with_size(Size::new(400.0, 300.0)),
        );
        drop(shell);
        assert!(
            matches!(messages.as_slice(), [Message::Pressed(id)] if id == "row"),
            "{messages:?}"
        );
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
