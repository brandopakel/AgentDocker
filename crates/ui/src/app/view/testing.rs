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
