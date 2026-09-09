//! Accessible semantics are collected from the same controls that handle input.
use crate::app::Message;
use accesskit::{Action, Node, NodeId, Role, Tree, TreeId, TreeUpdate};
use iced::advanced::widget::{
    Id, Operation,
    operation::{Focusable, Outcome},
};
use iced::{Rectangle, Task, window};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex, OnceLock},
};

#[derive(Clone)]
pub struct Semantic {
    pub id: String,
    pub label: String,
    pub role: Role,
    pub value: Option<String>,
    pub action: Option<Message>,
    pub change: Option<Arc<dyn Fn(String) -> Message + Send + Sync>>,
}
impl std::fmt::Debug for Semantic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Semantic")
            .field("id", &self.id)
            .field("label", &self.label)
            .field("role", &self.role)
            .finish()
    }
}
impl Semantic {
    pub fn button(id: String, label: String, action: Option<Message>) -> Self {
        Self {
            id,
            label,
            role: Role::Button,
            value: None,
            action,
            change: None,
        }
    }
    pub fn input(
        id: String,
        label: String,
        value: String,
        change: Arc<dyn Fn(String) -> Message + Send + Sync>,
    ) -> Self {
        Self {
            id,
            label,
            role: Role::TextInput,
            value: Some(value),
            action: None,
            change: Some(change),
        }
    }
    pub fn terminal(value: String) -> Self {
        Self {
            id: "terminal".into(),
            label: "Agent terminal. F6 moves focus out; Control right bracket detaches.".into(),
            role: Role::Terminal,
            value: Some(value),
            action: None,
            change: None,
        }
    }
}
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub nodes: Vec<(NodeId, Node)>,
    pub controls: BTreeMap<NodeId, Semantic>,
    pub focus: Option<NodeId>,
}
impl Snapshot {
    fn tree(&self) -> TreeUpdate {
        let mut root = Node::new(Role::Window);
        root.set_label("agentdocker");
        root.set_children(self.nodes.iter().map(|(id, _)| *id).collect::<Vec<_>>());
        let mut nodes = self.nodes.clone();
        nodes.push((NodeId(1), root));
        TreeUpdate {
            nodes,
            tree: Some(Tree::new(NodeId(1))),
            tree_id: TreeId::ROOT,
            focus: self
                .focus
                .filter(|id| self.nodes.iter().any(|(n, _)| n == id))
                .unwrap_or(NodeId(1)),
        }
    }
}
fn node_id(id: &str) -> NodeId {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in id.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    NodeId(hash | 1 << 63)
}
fn snapshot() -> &'static Mutex<Snapshot> {
    static STATE: OnceLock<Mutex<Snapshot>> = OnceLock::new();
    STATE.get_or_init(Default::default)
}
fn actions() -> &'static Mutex<VecDeque<Message>> {
    static ACTIONS: OnceLock<Mutex<VecDeque<Message>>> = OnceLock::new();
    ACTIONS.get_or_init(Default::default)
}
pub fn take_actions() -> Vec<Message> {
    actions()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .drain(..)
        .collect()
}

#[derive(Default)]
struct Collect {
    snapshot: Snapshot,
    texts: usize,
    control_bounds: Vec<Rectangle>,
    offset: iced::Vector,
    pending_offset: Option<iced::Vector>,
}
impl Operation<Snapshot> for Collect {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<Snapshot>)) {
        let previous = self.offset;
        if let Some(offset) = self.pending_offset.take() {
            self.offset += offset;
        }
        operate(self);
        self.offset = previous;
    }
    fn scrollable(
        &mut self,
        _: Option<&Id>,
        _: Rectangle,
        _: Rectangle,
        translation: iced::Vector,
        _: &mut dyn iced::advanced::widget::operation::Scrollable,
    ) {
        self.pending_offset = Some(translation);
    }
    fn custom(&mut self, _: Option<&Id>, bounds: Rectangle, state: &mut dyn std::any::Any) {
        let bounds = Rectangle {
            x: bounds.x - self.offset.x,
            y: bounds.y - self.offset.y,
            ..bounds
        };
        if let Some(semantic) = state.downcast_ref::<Semantic>() {
            let id = node_id(&semantic.id);
            let mut node = Node::new(semantic.role);
            node.set_label(semantic.label.clone());
            node.set_bounds(accesskit::Rect::new(
                f64::from(bounds.x),
                f64::from(bounds.y),
                f64::from(bounds.x + bounds.width),
                f64::from(bounds.y + bounds.height),
            ));
            if let Some(value) = &semantic.value {
                node.set_value(value.clone());
            }
            if semantic.action.is_some() {
                node.add_action(Action::Click);
            }
            if semantic.change.is_some() {
                node.add_action(Action::SetValue);
            }
            let enabled = match semantic.role {
                Role::Button => semantic.action.is_some(),
                Role::TextInput => semantic.change.is_some(),
                _ => true,
            };
            if enabled {
                node.add_action(Action::Focus);
            } else {
                node.set_disabled();
            }
            self.snapshot.controls.insert(id, semantic.clone());
            self.snapshot.nodes.push((id, node));
            self.control_bounds.push(bounds);
        }
    }
    fn focusable(&mut self, id: Option<&Id>, _: Rectangle, state: &mut dyn Focusable) {
        if state.is_focused()
            && let Some(id) = id
        {
            self.snapshot.focus = self
                .snapshot
                .controls
                .iter()
                .find(|(_, s)| Id::from(s.id.clone()) == *id)
                .map(|(id, _)| *id);
        }
    }
    fn text(&mut self, _: Option<&Id>, bounds: Rectangle, text: &str) {
        let bounds = Rectangle {
            x: bounds.x - self.offset.x,
            y: bounds.y - self.offset.y,
            ..bounds
        };
        if text.is_empty()
            || self.control_bounds.iter().any(|b| {
                b.contains(bounds.position())
                    && b.contains(
                        bounds.position() + iced::Vector::new(bounds.width, bounds.height),
                    )
            })
        {
            return;
        }
        self.texts += 1;
        let mut node = Node::new(Role::Label);
        node.set_value(text.to_owned());
        node.set_bounds(accesskit::Rect::new(
            f64::from(bounds.x),
            f64::from(bounds.y),
            f64::from(bounds.x + bounds.width),
            f64::from(bounds.y + bounds.height),
        ));
        self.snapshot
            .nodes
            .push((NodeId(self.texts as u64 + 1), node));
    }
    fn finish(&self) -> Outcome<Snapshot> {
        Outcome::Some(self.snapshot.clone())
    }
}
pub fn collect() -> Task<Message> {
    iced::advanced::widget::operate(Collect::default()).map(Message::Accessibility)
}

struct Handler;
#[cfg(target_os = "linux")]
impl accesskit::DeactivationHandler for Handler {
    fn deactivate_accessibility(&mut self) {}
}
impl accesskit::ActivationHandler for Handler {
    fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
        Some(snapshot().lock().unwrap_or_else(|e| e.into_inner()).tree())
    }
}
impl accesskit::ActionHandler for Handler {
    fn do_action(&mut self, request: accesskit::ActionRequest) {
        let control = snapshot()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .controls
            .get(&request.target_node)
            .cloned();
        let Some(control) = control else {
            return;
        };
        let message = match request.action {
            Action::Click => control.action,
            Action::Focus => Some(Message::Focus(control.id)),
            Action::SetValue => match (control.change, request.data) {
                (Some(change), Some(accesskit::ActionData::Value(value))) => {
                    Some(change(value.into()))
                }
                _ => None,
            },
            _ => None,
        };
        if let Some(message) = message {
            let mut actions = actions().lock().unwrap_or_else(|e| e.into_inner());
            if actions.len() < 64 {
                actions.push_back(message);
                crate::wake::Wake::default().request_repaint();
            }
        }
    }
}

#[cfg(target_os = "macos")]
thread_local! { static ADAPTER:std::cell::RefCell<Option<accesskit_macos::SubclassingAdapter>>=const { std::cell::RefCell::new(None) }; }
#[cfg(target_os = "windows")]
thread_local! { static ADAPTER:std::cell::RefCell<Option<accesskit_windows::SubclassingAdapter>>=const { std::cell::RefCell::new(None) }; }
#[cfg(target_os = "linux")]
thread_local! { static ADAPTER:std::cell::RefCell<Option<accesskit_unix::Adapter>>=const { std::cell::RefCell::new(None) }; }

pub fn install(id: window::Id) -> Task<Message> {
    window::run(id, |window| {
        #[cfg(target_os = "macos")]
        {
            use raw_window_handle::RawWindowHandle;
            if let Ok(handle) = window.window_handle()
                && let RawWindowHandle::AppKit(handle) = handle.as_raw()
            {
                ADAPTER.with(|adapter| {
                    // Iced invokes this on the window thread while retaining NSView.
                    // The window starts hidden so the adapter precedes first focus.
                    *adapter.borrow_mut() = Some(unsafe {
                        accesskit_macos::SubclassingAdapter::new(
                            handle.ns_view.as_ptr(),
                            Handler,
                            Handler,
                        )
                    });
                });
            }
        }
        #[cfg(target_os = "windows")]
        {
            if let Ok(handle) = window.window_handle()
                && let raw_window_handle::RawWindowHandle::Win32(handle) = handle.as_raw()
            {
                ADAPTER.with(|adapter| {
                    *adapter.borrow_mut() = Some(accesskit_windows::SubclassingAdapter::new(
                        accesskit_windows::HWND(handle.hwnd.get() as *mut _),
                        Handler,
                        Handler,
                    ))
                });
            }
        }
        #[cfg(target_os = "linux")]
        {
            let _ = window;
            ADAPTER.with(|adapter| {
                *adapter.borrow_mut() =
                    Some(accesskit_unix::Adapter::new(Handler, Handler, Handler))
            });
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
        let _ = window;
    })
    .discard()
    .chain(window::set_mode(id, window::Mode::Windowed))
}

pub fn update(id: window::Id, mut update: Snapshot, scale: f64) -> Task<Message> {
    // All native adapters consume physical pixels, including macOS's NSView bridge.
    for (_, node) in &mut update.nodes {
        if let Some(b) = node.bounds() {
            node.set_bounds(accesskit::Rect::new(
                b.x0 * scale,
                b.y0 * scale,
                b.x1 * scale,
                b.y1 * scale,
            ));
        }
    }
    *snapshot().lock().unwrap_or_else(|e| e.into_inner()) = update.clone();
    window::run(id, move |_| {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        ADAPTER.with(|adapter| {
            if let Some(adapter) = adapter.borrow_mut().as_mut()
                && let Some(events) = adapter.update_if_active(|| update.tree())
            {
                events.raise();
            }
        });
        #[cfg(target_os = "linux")]
        ADAPTER.with(|adapter| {
            if let Some(adapter) = adapter.borrow_mut().as_mut() {
                adapter.update_if_active(|| update.tree());
            }
        });
        #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
        let _ = update;
    })
    .discard()
}

pub fn focus(id: window::Id, focused: bool) -> Task<Message> {
    window::run(id, move |_| {
        #[cfg(target_os = "macos")]
        ADAPTER.with(|adapter| {
            if let Some(adapter) = adapter.borrow_mut().as_mut()
                && let Some(events) = adapter.update_view_focus_state(focused)
            {
                events.raise();
            }
        });
        #[cfg(target_os = "linux")]
        ADAPTER.with(|adapter| {
            if let Some(adapter) = adapter.borrow_mut().as_mut() {
                adapter.update_window_focus_state(focused);
            }
        });
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        let _ = focused;
    })
    .discard()
}

/// Supply AT-SPI with the actual X11 client origin; Wayland does not expose
/// global window positions to applications. Native Mac/Windows adapters own this.
pub fn geometry(id: window::Id) -> Task<Message> {
    #[cfg(target_os = "linux")]
    {
        window::run(id, |window| {
            use raw_window_handle::{RawDisplayHandle, RawWindowHandle};
            let (Ok(display), Ok(handle)) = (window.display_handle(), window.window_handle()) else { return; };
            let (RawDisplayHandle::Xlib(display), RawWindowHandle::Xlib(handle)) = (display.as_raw(), handle.as_raw()) else { return; };
            let Some(display) = display.display else { return; };
            thread_local! { static XLIB: Option<x11_dl::xlib::Xlib> = x11_dl::xlib::Xlib::open().ok(); }
            XLIB.with(|library| {
                let Some(xlib) = library else { return; };
                let (mut root, mut child, mut x, mut y, mut width, mut height, mut border, mut depth) = (0,0,0,0,0,0,0,0);
                // Winit retains this display/window for the duration of window::run.
                // Query only our client window on its event thread.
                let ok = unsafe {
                    (xlib.XGetGeometry)(display.as_ptr().cast(), handle.window, &mut root, &mut x, &mut y, &mut width, &mut height, &mut border, &mut depth) != 0
                    && (xlib.XTranslateCoordinates)(display.as_ptr().cast(), handle.window, root, 0, 0, &mut x, &mut y, &mut child) != 0
                };
                if ok {
                    let bounds = accesskit::Rect::new(f64::from(x), f64::from(y), f64::from(x)+f64::from(width), f64::from(y)+f64::from(height));
                    ADAPTER.with(|adapter| { if let Some(adapter) = adapter.borrow_mut().as_mut() { adapter.set_root_window_bounds(bounds, bounds); } });
                }
            });
        }).discard()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = id;
        Task::none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use accesskit::ActionHandler;
    #[test]
    fn accessibility_actions_use_the_same_messages_as_visible_controls() {
        let semantic = Semantic::button(
            "add-project".into(),
            "Add project".into(),
            Some(Message::ShowAdd),
        );
        let id = node_id(&semantic.id);
        let mut tree = Snapshot::default();
        tree.controls.insert(id, semantic);
        *snapshot().lock().unwrap() = tree;
        Handler.do_action(accesskit::ActionRequest {
            action: Action::Click,
            target_tree: TreeId::ROOT,
            target_node: id,
            data: None,
        });
        assert!(matches!(take_actions().as_slice(), [Message::ShowAdd]));
    }
}
