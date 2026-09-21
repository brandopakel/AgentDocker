//! A bounded multiline composer. The application still owns the draft and send
//! transaction; only cursor, selection and IME preedit live in the widget tree.
use super::*;
use iced::widget::text_editor::{self, Action, Binding, Content, KeyPress, Motion, Status};
use std::sync::Arc;

#[derive(Clone, Debug)]
enum Edit {
    Action(Action),
    Send,
    Ignore,
}

struct State {
    content: Content,
    id: String,
    value: String,
    preedit: bool,
    status: Option<Status>,
}
impl State {
    fn new(id: String, value: String) -> Self {
        let mut content = Content::with_text(&value);
        content.perform(Action::Move(Motion::DocumentEnd));
        Self {
            content,
            id,
            value,
            preedit: false,
            status: None,
        }
    }
}

struct Composer {
    id: String,
    label: String,
    value: String,
    change: Arc<dyn Fn(String) -> Message + Send + Sync>,
    enabled: bool,
    submit: Option<Message>,
}

// Keep selection, clipboard and navigation bindings supplied by Iced. During
// preedit Enter belongs to the input method, never to message submission.
fn binding(key: KeyPress, suppress_enter: bool) -> Option<Binding<Edit>> {
    if matches!(key.status, Status::Focused { .. })
        && key.key == keyboard::Key::Named(keyboard::key::Named::Enter)
    {
        return Some(if suppress_enter {
            Binding::Custom(Edit::Ignore)
        } else if key.modifiers == keyboard::Modifiers::SHIFT {
            Binding::Enter
        } else if key.modifiers.is_empty() {
            Binding::Custom(Edit::Send)
        } else {
            Binding::Custom(Edit::Ignore)
        });
    }
    Binding::from_key_press(key)
}

impl Composer {
    fn editor<'a>(
        &'a self,
        content: &'a Content,
        suppress_enter: bool,
        drawn_status: Option<Status>,
    ) -> Element<'a, Edit> {
        let mut editor = iced::widget::text_editor(content)
            .id(widget::Id::from(self.id.clone()))
            .placeholder(&self.label)
            .padding([11, 13])
            .size(14)
            .height(Length::Shrink)
            // Shrink layout in Iced adds vertical padding after this bound.
            .max_height(98)
            .key_binding(move |key| binding(key, suppress_enter))
            .style(move |theme, status| {
                let status = drawn_status.unwrap_or(status);
                use crate::app::style::{Colors, alpha};
                let c = Colors::of(theme);
                let (border, background) = match status {
                    Status::Focused { .. } => (c.accent, c.card),
                    Status::Hovered => (alpha(c.accent, 0.6), c.card),
                    Status::Active => (c.line, c.card),
                    Status::Disabled => (c.line, c.raised),
                };
                text_editor::Style {
                    background: background.into(),
                    border: Border {
                        color: border,
                        width: 1.0,
                        radius: 9.0.into(),
                    },
                    placeholder: c.faint,
                    value: if matches!(status, Status::Disabled) {
                        c.muted
                    } else {
                        c.text
                    },
                    selection: alpha(c.accent, 0.35),
                }
            });
        if self.enabled {
            editor = editor.on_action(Edit::Action);
        }
        editor.into()
    }
}

impl Widget<Message, iced::Theme, iced::Renderer> for Composer {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }
    fn state(&self) -> tree::State {
        tree::State::new(State::new(self.id.clone(), self.value.clone()))
    }
    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(self.editor(&Content::new(), false, None))]
    }
    fn diff(&self, tree: &mut Tree) {
        let state = tree.state.downcast_mut::<State>();
        if state.id != self.id {
            *state = State::new(self.id.clone(), self.value.clone());
            tree.children = self.children();
        } else if state.value != self.value {
            // A receipt cleared this draft, a mention/notification changed it,
            // or the application refused an edit. Its value is authoritative.
            let preedit = state.preedit;
            *state = State::new(self.id.clone(), self.value.clone());
            // The child retains focus and native preedit. A SetValue or
            // notification must not turn the composition's Enter into Send.
            state.preedit = preedit;
        }
    }
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Shrink)
    }
    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let state = tree.state.downcast_ref::<State>();
        self.editor(&state.content, false, None)
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
        let state = tree.state.downcast_ref::<State>();
        self.editor(&state.content, false, None)
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
        let state = tree.state.downcast_mut::<State>();
        // Only the focused editor receives composition events. Losing focus
        // clears our guard as well as Iced's native preedit state.
        let focused = tree.children[0]
            .state
            .downcast_ref::<text_editor::State<iced::advanced::text::highlighter::PlainText>>()
            .is_focused();
        if !focused {
            state.preedit = false;
        }
        if focused {
            match event {
                Event::InputMethod(iced::advanced::input_method::Event::Preedit(text, _)) => {
                    state.preedit = !text.is_empty()
                }
                Event::InputMethod(
                    iced::advanced::input_method::Event::Commit(_)
                    | iced::advanced::input_method::Event::Closed,
                ) => state.preedit = false,
                _ => {}
            }
        }
        let repeated = matches!(
            event,
            Event::Keyboard(keyboard::Event::KeyPressed { repeat: true, .. })
        );
        let mut edits = Vec::new();
        let mut local = Shell::new(&mut edits);
        self.editor(&state.content, state.preedit || repeated, None)
            .as_widget_mut()
            .update(
                &mut tree.children[0],
                event,
                layout,
                cursor,
                renderer,
                clipboard,
                &mut local,
                viewport,
            );
        if local.is_event_captured() {
            shell.capture_event();
        }
        shell.request_redraw_at(local.redraw_request());
        shell.request_input_method(local.input_method());
        if local.is_layout_invalid() {
            shell.invalidate_layout();
        }
        if local.are_widgets_invalid() {
            shell.invalidate_widgets();
        }
        for edit in edits {
            match edit {
                Edit::Action(action) => {
                    let changed = action.is_edit();
                    state.content.perform(action);
                    if changed {
                        // Let the application reject over-limit edits, keeping
                        // the earlier draft and reporting why. Truncating here
                        // would silently delete its suffix when typing at the
                        // start of a full draft. diff restores a refused edit.
                        let value = state.content.text();
                        state.value = value.clone();
                        shell.publish((self.change)(value));
                        shell.invalidate_layout();
                    }
                    shell.request_redraw();
                }
                Edit::Send => {
                    if let Some(submit) = &self.submit {
                        shell.publish(submit.clone());
                    }
                }
                Edit::Ignore => {}
            }
        }
        let status = visual_status(self.enabled, &tree.children[0], layout, cursor);
        if state.status != Some(status) {
            state.status = Some(status);
            shell.request_redraw();
        }
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
        let state = tree.state.downcast_ref::<State>();
        // TextEditor caches its draw status on the widget value, which this
        // adapter recreates for each method. Derive it from persistent focus
        // and this frame's pointer so focus/disabled affordances remain true.
        let status = visual_status(self.enabled, &tree.children[0], layout, cursor);
        self.editor(&state.content, false, Some(status))
            .as_widget()
            .draw(
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
        let state = tree.state.downcast_ref::<State>();
        self.editor(&state.content, false, None)
            .as_widget()
            .mouse_interaction(&tree.children[0], layout, cursor, viewport, renderer)
    }
}

fn visual_status(enabled: bool, tree: &Tree, layout: Layout<'_>, cursor: mouse::Cursor) -> Status {
    let focused = tree
        .state
        .downcast_ref::<text_editor::State<iced::advanced::text::highlighter::PlainText>>()
        .is_focused();
    if !enabled {
        Status::Disabled
    } else if focused {
        Status::Focused {
            is_hovered: cursor.is_over(layout.bounds()),
        }
    } else if cursor.is_over(layout.bounds()) {
        Status::Hovered
    } else {
        Status::Active
    }
}

pub fn composer<'a>(
    id: impl Into<String>,
    label: &str,
    value: &str,
    change: impl Fn(String) -> Message + Send + Sync + 'static,
    enabled: bool,
    submit: Option<Message>,
) -> Element<'a, Message> {
    let id = id.into();
    let change = Arc::new(change);
    let mut semantic = Semantic::input(
        id.clone(),
        format!("{label} Enter to send. Shift+Enter for a new line."),
        value.into(),
        change.clone(),
    );
    semantic.role = accesskit::Role::MultilineTextInput;
    semantic.action = submit.clone();
    if !enabled {
        semantic.change = None;
    }
    Control {
        content: Element::new(Composer {
            id,
            label: label.into(),
            value: value.into(),
            change,
            enabled,
            submit,
        }),
        semantic,
        button: false,
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::advanced::{input_method, widget::operation::Focusable};

    #[derive(Default)]
    struct MemoryClipboard(Option<String>);
    impl Clipboard for MemoryClipboard {
        fn read(&self, _: iced::advanced::clipboard::Kind) -> Option<String> {
            self.0.clone()
        }
        fn write(&mut self, _: iced::advanced::clipboard::Kind, text: String) {
            self.0 = Some(text);
        }
    }
    fn fixture(id: &str, text: &str, enabled: bool, ready: bool) -> Composer {
        Composer {
            id: id.into(),
            label: "Message".into(),
            value: text.into(),
            change: Arc::new(Message::ChannelDraft),
            enabled,
            submit: ready.then_some(Message::SendChannel),
        }
    }
    fn key(key: keyboard::Key, modifiers: keyboard::Modifiers, repeat: bool) -> Event {
        Event::Keyboard(keyboard::Event::KeyPressed {
            modified_key: key.clone(),
            key,
            physical_key: keyboard::key::Physical::Unidentified(
                keyboard::key::NativeCode::Unidentified,
            ),
            location: keyboard::Location::Standard,
            modifiers,
            text: None,
            repeat,
        })
    }
    fn enter(modifiers: keyboard::Modifiers, repeat: bool) -> Event {
        key(
            keyboard::Key::Named(keyboard::key::Named::Enter),
            modifiers,
            repeat,
        )
    }
    fn focus(tree: &mut Tree) {
        tree.children[0]
            .state
            .downcast_mut::<text_editor::State<iced::advanced::text::highlighter::PlainText>>()
            .focus();
    }
    fn event(
        composer: &mut Composer,
        tree: &mut Tree,
        event: Event,
        clipboard: &mut MemoryClipboard,
    ) -> Vec<Message> {
        let renderer = iced::Renderer::new(iced::Font::DEFAULT, 14.0.into());
        let viewport = Rectangle::with_size(Size::new(220.0, 160.0));
        let node = composer.layout(
            tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, viewport.size()),
        );
        let mut messages = Vec::new();
        composer.update(
            tree,
            &event,
            Layout::new(&node),
            mouse::Cursor::Unavailable,
            &renderer,
            clipboard,
            &mut Shell::new(&mut messages),
            &viewport,
        );
        messages
    }
    fn tree(composer: &Composer) -> Tree {
        Tree::new(composer as &dyn Widget<Message, iced::Theme, iced::Renderer>)
    }

    #[test]
    fn native_enter_sends_once_shift_enter_edits_and_unavailable_composers_never_send() {
        let mut clipboard = MemoryClipboard::default();
        for (focused, enabled, ready, repeat, sends) in [
            (true, true, true, false, 1),
            (false, true, true, false, 0),
            (true, false, false, false, 0),
            (true, true, false, false, 0),
            (true, true, true, true, 0),
        ] {
            let mut composer = fixture("room", "line one", enabled, ready);
            let mut tree = tree(&composer);
            if focused {
                focus(&mut tree);
            }
            let messages = event(
                &mut composer,
                &mut tree,
                enter(keyboard::Modifiers::empty(), repeat),
                &mut clipboard,
            );
            assert_eq!(messages.len(), sends);
            assert!(messages.iter().all(|m| matches!(m, Message::SendChannel)));
            assert_eq!(
                tree.state.downcast_ref::<State>().content.text(),
                "line one"
            );
        }
        let mut composer = fixture("room", "line one", true, true);
        let mut tree = tree(&composer);
        focus(&mut tree);
        let messages = event(
            &mut composer,
            &mut tree,
            enter(keyboard::Modifiers::SHIFT, false),
            &mut clipboard,
        );
        assert!(
            matches!(messages.as_slice(), [Message::ChannelDraft(text)] if text == "line one\n")
        );
        assert_eq!(
            tree.state.downcast_ref::<State>().content.text(),
            "line one\n"
        );
    }

    #[test]
    fn native_preedit_enter_does_not_submit_and_commit_keeps_unicode() {
        let mut composer = fixture("room", "before\n", true, true);
        let mut tree = tree(&composer);
        focus(&mut tree);
        let mut clipboard = MemoryClipboard::default();
        assert!(
            event(
                &mut composer,
                &mut tree,
                Event::InputMethod(input_method::Event::Preedit("にほん".into(), Some(0..9))),
                &mut clipboard
            )
            .is_empty()
        );
        assert!(
            event(
                &mut composer,
                &mut tree,
                enter(keyboard::Modifiers::empty(), false),
                &mut clipboard
            )
            .is_empty()
        );
        let messages = event(
            &mut composer,
            &mut tree,
            Event::InputMethod(input_method::Event::Commit("日本語".into())),
            &mut clipboard,
        );
        assert!(
            matches!(messages.as_slice(), [Message::ChannelDraft(text)] if text == "before\n日本語")
        );
        assert!(matches!(
            event(
                &mut composer,
                &mut tree,
                enter(keyboard::Modifiers::empty(), false),
                &mut clipboard
            )
            .as_slice(),
            [Message::SendChannel]
        ));
    }

    #[test]
    fn replacing_the_authoritative_draft_during_preedit_does_not_arm_enter() {
        let mut composer = fixture("room", "old", true, true);
        let mut tree = tree(&composer);
        focus(&mut tree);
        let mut clipboard = MemoryClipboard::default();
        event(
            &mut composer,
            &mut tree,
            Event::InputMethod(input_method::Event::Preedit("にほん".into(), Some(0..9))),
            &mut clipboard,
        );
        let mut replacement = fixture("room", "changed\n", true, true);
        replacement.diff(&mut tree);
        assert!(
            event(
                &mut replacement,
                &mut tree,
                enter(keyboard::Modifiers::empty(), false),
                &mut clipboard
            )
            .is_empty()
        );
        let messages = event(
            &mut replacement,
            &mut tree,
            Event::InputMethod(input_method::Event::Commit("日本語".into())),
            &mut clipboard,
        );
        assert!(
            matches!(messages.as_slice(), [Message::ChannelDraft(text)] if text == "changed\n日本語")
        );
    }

    #[test]
    fn native_clipboard_keeps_lines_selection_and_the_existing_draft_bound() {
        let mut composer = fixture("room", "", true, false);
        let mut tree = tree(&composer);
        focus(&mut tree);
        let mut clipboard = MemoryClipboard(Some("café\n日本語\nlast line".into()));
        let command = if cfg!(target_os = "macos") {
            keyboard::Modifiers::LOGO
        } else {
            keyboard::Modifiers::CTRL
        };
        let messages = event(
            &mut composer,
            &mut tree,
            key(keyboard::Key::Character("v".into()), command, false),
            &mut clipboard,
        );
        assert!(
            matches!(messages.as_slice(), [Message::ChannelDraft(text)] if text == "café\n日本語\nlast line")
        );
        event(
            &mut composer,
            &mut tree,
            key(keyboard::Key::Character("a".into()), command, false),
            &mut clipboard,
        );
        clipboard.0 = None;
        event(
            &mut composer,
            &mut tree,
            key(keyboard::Key::Character("c".into()), command, false),
            &mut clipboard,
        );
        assert_eq!(clipboard.0.as_deref(), Some("café\n日本語\nlast line"));
        clipboard.0 = Some("é\n".repeat(9000));
        let messages = event(
            &mut composer,
            &mut tree,
            key(keyboard::Key::Character("v".into()), command, false),
            &mut clipboard,
        );
        assert!(
            matches!(messages.as_slice(), [Message::ChannelDraft(text)] if text.chars().count() == 18000)
        );
        assert_eq!(
            tree.state
                .downcast_ref::<State>()
                .content
                .text()
                .chars()
                .count(),
            18000
        );
        // The application refuses the oversized edit and rebuilds with its
        // unchanged authoritative draft. Nothing is silently truncated.
        let old = "café\n日本語\nlast line";
        fixture("room", old, true, true).diff(&mut tree);
        assert_eq!(tree.state.downcast_ref::<State>().content.text(), old);
        composer = fixture("room", &"é\n".repeat(8000), true, true);
        composer.diff(&mut tree);
        let renderer = iced::Renderer::new(iced::Font::DEFAULT, 14.0.into());
        let node = composer.layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, Size::new(220.0, 300.0)),
        );
        assert!(
            node.bounds().height <= 120.0,
            "long paste must scroll inside the composer"
        );
    }

    #[test]
    fn typing_at_the_start_of_a_full_draft_cannot_silently_delete_its_suffix() {
        let original = format!("{}END", "x".repeat(crate::drafts::MAX_TEXT_CHARS - 3));
        let mut composer = fixture("room", &original, true, true);
        let mut tree = tree(&composer);
        focus(&mut tree);
        tree.state
            .downcast_mut::<State>()
            .content
            .perform(Action::Move(Motion::DocumentStart));
        let mut clipboard = MemoryClipboard::default();
        let messages = event(
            &mut composer,
            &mut tree,
            Event::InputMethod(input_method::Event::Commit("日本語".into())),
            &mut clipboard,
        );
        assert!(
            matches!(messages.as_slice(), [Message::ChannelDraft(text)] if text.starts_with("日本語") && text.ends_with("END") && text.chars().count() == crate::drafts::MAX_TEXT_CHARS + 3)
        );
        composer.diff(&mut tree);
        assert_eq!(tree.state.downcast_ref::<State>().content.text(), original);
    }

    #[test]
    fn destination_changes_reset_focus_but_same_draft_rebuilds_keep_the_cursor() {
        let composer = fixture("conversation", "first\nsecond", true, true);
        let mut tree = tree(&composer);
        focus(&mut tree);
        tree.state
            .downcast_mut::<State>()
            .content
            .perform(Action::Move(Motion::DocumentStart));
        composer.diff(&mut tree);
        let cursor = tree.state.downcast_ref::<State>().content.cursor();
        assert_eq!(cursor.position.line, 0);
        assert_eq!(cursor.position.column, 0);
        let cleared = fixture("conversation", "", true, false);
        cleared.diff(&mut tree);
        assert!(tree.state.downcast_ref::<State>().content.text().is_empty());
        let thread = fixture("thread", "thread's draft\nretained", true, true);
        thread.diff(&mut tree);
        assert_eq!(
            tree.state.downcast_ref::<State>().content.text(),
            "thread's draft\nretained"
        );
        assert!(
            !tree.children[0]
                .state
                .downcast_ref::<text_editor::State<iced::advanced::text::highlighter::PlainText>>()
                .is_focused()
        );
    }

    #[test]
    fn focus_hover_and_disabled_status_survive_reconstructed_native_widgets() {
        let renderer = iced::Renderer::new(iced::Font::DEFAULT, 14.0.into());
        let mut composer = fixture("room", "message", true, true);
        let mut tree = tree(&composer);
        let bounds = Rectangle::with_size(Size::new(220.0, 160.0));
        let node = composer.layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, bounds.size()),
        );
        let layout = Layout::new(&node);
        let pointer = mouse::Cursor::Available(iced::Point::new(20.0, 20.0));
        assert_eq!(
            visual_status(true, &tree.children[0], layout, pointer),
            Status::Hovered
        );
        focus(&mut tree);
        assert_eq!(
            visual_status(true, &tree.children[0], layout, pointer),
            Status::Focused { is_hovered: true }
        );
        assert_eq!(
            visual_status(false, &tree.children[0], layout, pointer),
            Status::Disabled
        );
        // Rebuilding the native value must not cause an immediate redraw loop.
        let mut clipboard = MemoryClipboard::default();
        let redraw = Event::Window(iced::window::Event::RedrawRequested(
            std::time::Instant::now(),
        ));
        event(&mut composer, &mut tree, redraw.clone(), &mut clipboard);
        let mut messages = Vec::new();
        let mut shell = Shell::new(&mut messages);
        composer.update(
            &mut tree,
            &redraw,
            layout,
            mouse::Cursor::Unavailable,
            &renderer,
            &mut clipboard,
            &mut shell,
            &bounds,
        );
        assert_ne!(
            shell.redraw_request(),
            iced::window::RedrawRequest::NextFrame
        );
    }
}
