//! Test support for views: what a rendered element offers, read the way
//! assistive technology reads it.
use crate::accessibility::Semantic;
use crate::app::Message;
use iced::Element;
use iced::advanced::widget::{Operation, Tree, operation::Outcome};
use iced::advanced::{Layout, layout};

/// What a rendered element offers: its controls (id, label, value,
/// enabled), its readable texts and its anchored containers.
#[derive(Default)]
pub(in crate::app) struct Seen {
    pub controls: Vec<(String, String, Option<String>, bool)>,
    pub texts: Vec<String>,
    pub anchors: Vec<iced::advanced::widget::Id>,
}
impl Seen {
    pub fn control(&self, id: &str) -> Option<&(String, String, Option<String>, bool)> {
        self.controls.iter().find(|c| c.0 == id)
    }
    pub fn reads(&self, words: &str) -> bool {
        self.texts.iter().any(|t| t.contains(words))
            || self.controls.iter().any(|c| c.1.contains(words))
    }
}
impl Operation for Seen {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        operate(self);
    }
    fn container(&mut self, id: Option<&iced::advanced::widget::Id>, _: iced::Rectangle) {
        if let Some(id) = id {
            self.anchors.push(id.clone());
        }
    }
    fn text(&mut self, _: Option<&iced::advanced::widget::Id>, _: iced::Rectangle, text: &str) {
        self.texts.push(text.to_owned());
    }
    fn custom(
        &mut self,
        _: Option<&iced::advanced::widget::Id>,
        _: iced::Rectangle,
        state: &mut dyn std::any::Any,
    ) {
        if let Some(semantic) = state.downcast_ref::<Semantic>() {
            self.controls.push((
                semantic.id.clone(),
                semantic.label.clone(),
                semantic.value.clone(),
                semantic.action.is_some() || semantic.change.is_some(),
            ));
        }
    }
    fn finish(&self) -> Outcome<()> {
        Outcome::None
    }
}

pub(in crate::app) fn seen(mut element: Element<'_, Message>) -> Seen {
    let renderer = iced::Renderer::new(iced::Font::DEFAULT, 14.0.into());
    let mut tree = Tree::new(&element);
    let node = element.as_widget_mut().layout(
        &mut tree,
        &renderer,
        &layout::Limits::new(iced::Size::ZERO, iced::Size::new(900.0, 6000.0)),
    );
    let mut seen = Seen::default();
    element
        .as_widget_mut()
        .operate(&mut tree, Layout::new(&node), &renderer, &mut seen);
    seen
}

/// A window driven the way Iced's event loop drives one: a batch of
/// events goes through the laid-out view, the messages it publishes go to
/// the app in order, the view is rebuilt from the widget state it kept,
/// and only then do the tasks' widget operations run, on the rebuilt view.
/// That order is what decides where a click's focus lands when the click
/// also changed what is on screen.
pub(in crate::app) struct Window {
    renderer: iced::Renderer,
    size: iced::Size,
    cache: Option<iced_runtime::user_interface::Cache>,
    runtime: tokio::runtime::Runtime,
}

/// One control's focus as an operation finds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::app) struct Focused {
    pub id: String,
    pub ringed: bool,
}

impl Window {
    pub fn new(width: f32, height: f32) -> Self {
        Self {
            renderer: iced::Renderer::new(iced::Font::DEFAULT, 14.0.into()),
            size: iced::Size::new(width, height),
            cache: None,
            // Tasks the app returns may start futures that need a runtime
            // to be polled at all; they are polled once and left there.
            runtime: tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("a runtime for the app's tasks"),
        }
    }

    fn with<R>(
        &mut self,
        app: &crate::app::App,
        f: impl FnOnce(
            &mut iced_runtime::UserInterface<'_, Message, iced::Theme, iced::Renderer>,
            &mut iced::Renderer,
        ) -> R,
    ) -> R {
        let mut ui = iced_runtime::UserInterface::build(
            app.view(),
            self.size,
            self.cache.take().unwrap_or_default(),
            &mut self.renderer,
        );
        let result = f(&mut ui, &mut self.renderer);
        self.cache = Some(ui.into_cache());
        result
    }

    /// Lay the view out and keep its widget state, as the first frame does.
    pub fn show(&mut self, app: &crate::app::App) {
        self.with(app, |_, _| ());
    }

    /// Where the control with this id is, before any scrolling.
    pub fn bounds(&mut self, app: &crate::app::App, id: &str) -> Option<iced::Rectangle> {
        struct Find<'a>(&'a str, Option<iced::Rectangle>);
        impl Operation for Find<'_> {
            fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
                operate(self);
            }
            fn custom(
                &mut self,
                _: Option<&iced::advanced::widget::Id>,
                bounds: iced::Rectangle,
                state: &mut dyn std::any::Any,
            ) {
                if self.1.is_none()
                    && state
                        .downcast_ref::<Semantic>()
                        .is_some_and(|semantic| semantic.id == self.0)
                {
                    self.1 = Some(bounds);
                }
            }
        }
        self.with(app, |ui, renderer| {
            let mut find = Find(id, None);
            ui.operate(renderer, &mut find);
            find.1
        })
    }

    /// How far the scrollable with this id is scrolled from its top.
    pub fn scrolled(&mut self, app: &crate::app::App, id: &str) -> Option<iced::Vector> {
        struct Find<'a>(&'a str, Option<iced::Vector>);
        impl Operation for Find<'_> {
            fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
                operate(self);
            }
            fn scrollable(
                &mut self,
                id: Option<&iced::advanced::widget::Id>,
                _: iced::Rectangle,
                _: iced::Rectangle,
                translation: iced::Vector,
                _: &mut dyn iced::advanced::widget::operation::Scrollable,
            ) {
                if id == Some(&iced::advanced::widget::Id::from(self.0.to_owned())) {
                    self.1 = Some(translation);
                }
            }
        }
        self.with(app, |ui, renderer| {
            let mut find = Find(id, None);
            ui.operate(renderer, &mut find);
            find.1
        })
    }

    /// The controls that have the focus, and whether each shows its ring.
    pub fn focused(&mut self, app: &crate::app::App) -> Vec<Focused> {
        /// A control hands over its accessible node, then its focus.
        #[derive(Default)]
        struct Probe {
            control: String,
            found: Vec<Focused>,
        }
        impl Operation for Probe {
            fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
                operate(self);
            }
            fn custom(
                &mut self,
                _: Option<&iced::advanced::widget::Id>,
                _: iced::Rectangle,
                state: &mut dyn std::any::Any,
            ) {
                if let Some(semantic) = state.downcast_ref::<Semantic>() {
                    self.control = semantic.id.clone();
                } else if let Some(focus) = state.downcast_ref::<crate::controls::Focus>()
                    && focus.focused
                {
                    self.found.push(Focused {
                        id: self.control.clone(),
                        ringed: focus.ringed(),
                    });
                }
            }
        }
        self.with(app, |ui, renderer| {
            let mut probe = Probe::default();
            ui.operate(renderer, &mut probe);
            probe.found
        })
    }

    /// One pass of the event loop: `events` as one batch with the pointer
    /// at `cursor`, their messages to the app, the view rebuilt, then the
    /// widget operations of the tasks those messages returned. Returns the
    /// messages as the app received them, in order, written out.
    pub fn events(
        &mut self,
        app: &mut crate::app::App,
        events: &[iced::Event],
        cursor: iced::mouse::Cursor,
    ) -> Vec<String> {
        let messages = self.dispatch(app, events, cursor);
        let heard = messages.iter().map(|m| format!("{m:?}")).collect();
        self.deliver(app, messages);
        heard
    }

    /// The first half of a pass: `events` through the view, and the
    /// messages they publish, not yet delivered.
    pub fn dispatch(
        &mut self,
        app: &crate::app::App,
        events: &[iced::Event],
        cursor: iced::mouse::Cursor,
    ) -> Vec<Message> {
        let mut messages = Vec::new();
        self.with(app, |ui, renderer| {
            ui.update(
                events,
                cursor,
                renderer,
                &mut iced::advanced::clipboard::Null,
                &mut messages,
            );
        });
        messages
    }

    /// Messages to the app, then the rebuilt view, then their tasks'
    /// widget operations: the rest of one pass of the event loop.
    pub fn deliver(&mut self, app: &mut crate::app::App, messages: Vec<Message>) {
        let tasks: Vec<_> = messages.into_iter().map(|m| app.update(m)).collect();
        let _runtime = self.runtime.enter();
        self.with(app, |ui, renderer| {
            for task in tasks {
                run(task, ui, renderer);
            }
        });
    }

    /// A click as a tap on a trackpad makes it: the pointer there, the
    /// press and the release in the same pass of the event loop.
    pub fn tap(&mut self, app: &mut crate::app::App, at: iced::Point) -> Vec<String> {
        self.events(app, &tap(at), iced::mouse::Cursor::Available(at))
    }

    /// A slower click: the press in one pass, the release in the next.
    pub fn click(&mut self, app: &mut crate::app::App, at: iced::Point) -> Vec<String> {
        use iced::mouse::{Button, Event as Mouse};
        let cursor = iced::mouse::Cursor::Available(at);
        let mut heard = self.events(
            app,
            &[
                iced::Event::Mouse(Mouse::CursorMoved { position: at }),
                iced::Event::Mouse(Mouse::ButtonPressed(Button::Left)),
            ],
            cursor,
        );
        heard.extend(self.events(
            app,
            &[iced::Event::Mouse(Mouse::ButtonReleased(Button::Left))],
            cursor,
        ));
        heard
    }

    /// A key pressed and let go, with nothing held.
    pub fn key(&mut self, app: &mut crate::app::App, key: iced::keyboard::Key) {
        self.key_with(app, key, iced::keyboard::Modifiers::empty(), None);
    }

    /// A character typed, as a text field takes it.
    pub fn typed(&mut self, app: &mut crate::app::App, character: &str) {
        self.key_with(
            app,
            iced::keyboard::Key::Character(character.into()),
            iced::keyboard::Modifiers::empty(),
            Some(character),
        );
    }

    pub fn key_with(
        &mut self,
        app: &mut crate::app::App,
        key: iced::keyboard::Key,
        modifiers: iced::keyboard::Modifiers,
        text: Option<&str>,
    ) {
        use iced::keyboard::{self, key::Physical};
        let physical = Physical::Unidentified(keyboard::key::NativeCode::Unidentified);
        let pressed = iced::Event::Keyboard(keyboard::Event::KeyPressed {
            key: key.clone(),
            modified_key: key.clone(),
            physical_key: physical,
            location: keyboard::Location::Standard,
            modifiers,
            text: text.map(Into::into),
            repeat: false,
        });
        // The app hears a key no control took, as its subscription does.
        let mut messages = Vec::new();
        let statuses = self.with(app, |ui, renderer| {
            ui.update(
                std::slice::from_ref(&pressed),
                iced::mouse::Cursor::Unavailable,
                renderer,
                &mut iced::advanced::clipboard::Null,
                &mut messages,
            )
            .1
        });
        if statuses.first() == Some(&iced::event::Status::Ignored) {
            messages.push(Message::Event(pressed));
        }
        self.deliver(app, messages);
    }
}

/// The events of a tap at `at`: the pointer there, pressed and released.
pub(in crate::app) fn tap(at: iced::Point) -> [iced::Event; 3] {
    use iced::mouse::{Button, Event as Mouse};
    [
        iced::Event::Mouse(Mouse::CursorMoved { position: at }),
        iced::Event::Mouse(Mouse::ButtonPressed(Button::Left)),
        iced::Event::Mouse(Mouse::ButtonReleased(Button::Left)),
    ]
}

/// Run a task's widget operations on `ui` as the event loop does, until it
/// finishes or waits on something else.
fn run(
    task: iced::Task<Message>,
    ui: &mut iced_runtime::UserInterface<'_, Message, iced::Theme, iced::Renderer>,
    renderer: &mut iced::Renderer,
) {
    use iced::futures::StreamExt;
    let Some(mut stream) = iced_runtime::task::into_stream(task) else {
        return;
    };
    let waker = iced::futures::task::noop_waker_ref();
    let mut context = std::task::Context::from_waker(waker);
    while let std::task::Poll::Ready(Some(action)) = stream.poll_next_unpin(&mut context) {
        if let iced_runtime::Action::Widget(operation) = action {
            let mut current = Some(operation);
            while let Some(mut operation) = current.take() {
                ui.operate(renderer, operation.as_mut());
                if let Outcome::Chain(next) = operation.finish() {
                    current = Some(next);
                }
            }
        }
    }
}
