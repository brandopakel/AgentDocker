//! The window's resizable columns: the rail beside the workspace, and on
//! the Messages screen the conversation list, the conversation and its
//! thread. Each divider drags; the widths are kept in pixels, so a wider
//! window gives the conversation the room and the columns stay as set,
//! and they are saved with the workspace preferences.
use iced::widget::pane_grid::{self, Axis, Pane, Split, State};
use serde::{Deserialize, Serialize};

/// Which grid a divider belongs to: split ids are the grid's own, so a
/// drag says where it happened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Grid {
    Shell,
    Messages,
}

/// What a pane holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    Rail,
    Workspace,
    Sidebar,
    Conversation,
    Thread,
}

/// The column widths a person set, in logical pixels.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Widths {
    pub rail: f32,
    pub sidebar: f32,
    pub thread: f32,
}

impl Default for Widths {
    fn default() -> Self {
        Self {
            rail: 236.0,
            sidebar: 272.0,
            thread: 340.0,
        }
    }
}

/// The bounds a column is kept within, whatever was dragged or saved.
const RAIL: (f32, f32) = (180.0, 440.0);
const SIDEBAR: (f32, f32) = (200.0, 560.0);
const THREAD: (f32, f32) = (240.0, 640.0);
/// The workspace's own padding beside the rail, which the Messages grid
/// does not get.
const WORKSPACE_CHROME: f32 = 61.0;
/// Keep the message body and composer usable after side columns shrink.
const CONVERSATION_MIN: f32 = 320.0;

pub struct Panes {
    pub shell: State<Slot>,
    rail_split: Split,
    pub messages: State<Slot>,
    sidebar_split: Split,
    conversation: Pane,
    thread: Option<(Pane, Split)>,
    pub widths: Widths,
    window: f32,
}

impl Panes {
    pub fn new(widths: Widths, window: f32) -> Self {
        let (mut shell, rail) = State::new(Slot::Rail);
        let (_, rail_split) = shell
            .split(Axis::Vertical, rail, Slot::Workspace)
            .expect("a fresh grid splits");
        let (mut messages, sidebar) = State::new(Slot::Sidebar);
        let (conversation, sidebar_split) = messages
            .split(Axis::Vertical, sidebar, Slot::Conversation)
            .expect("a fresh grid splits");
        let mut panes = Self {
            shell,
            rail_split,
            messages,
            sidebar_split,
            conversation,
            thread: None,
            widths: Widths {
                rail: widths.rail.clamp(RAIL.0, RAIL.1),
                sidebar: widths.sidebar.clamp(SIDEBAR.0, SIDEBAR.1),
                thread: widths.thread.clamp(THREAD.0, THREAD.1),
            },
            window: window.max(1.0),
        };
        panes.apply();
        panes
    }

    /// The window's logical width changed: the columns keep their pixels.
    pub fn window_width(&mut self, width: f32) {
        self.window = width.max(1.0);
        self.apply();
    }

    /// Whether the Messages grid has a thread column.
    #[cfg(test)]
    pub fn thread_open(&self) -> bool {
        self.thread.is_some()
    }

    /// Show or take away the thread column to match whether a thread is
    /// open; the conversation keeps its pane either way.
    pub fn sync_thread(&mut self, open: bool) {
        match (open, self.thread) {
            (true, None) => {
                if let Some((pane, split)) =
                    self.messages
                        .split(Axis::Vertical, self.conversation, Slot::Thread)
                {
                    self.thread = Some((pane, split));
                    self.apply();
                }
            }
            (false, Some((pane, _))) => {
                self.messages.close(pane);
                self.thread = None;
                self.apply();
            }
            _ => {}
        }
    }

    /// A divider was dragged: the column it moves keeps the whole pixels
    /// this ratio means now. Whether a saved width changed.
    pub fn resized(&mut self, grid: Grid, event: pane_grid::ResizeEvent) -> bool {
        if !event.ratio.is_finite() {
            return false;
        }
        let before = self.widths;
        match grid {
            Grid::Shell if event.split == self.rail_split => {
                self.widths.rail = (event.ratio * self.window).round().clamp(RAIL.0, RAIL.1);
            }
            Grid::Messages if event.split == self.sidebar_split => {
                self.widths.sidebar = (event.ratio * self.messages_width())
                    .round()
                    .clamp(SIDEBAR.0, SIDEBAR.1);
            }
            Grid::Messages if self.thread.is_some_and(|(_, split)| split == event.split) => {
                let rest = self.messages_width() - self.effective_widths().sidebar;
                self.widths.thread = ((1.0 - event.ratio) * rest)
                    .round()
                    .clamp(THREAD.0, THREAD.1);
            }
            _ => return false,
        }
        self.apply();
        self.widths != before
    }

    fn minimum_messages_width(&self) -> f32 {
        SIDEBAR.0 + CONVERSATION_MIN + if self.thread.is_some() { THREAD.0 } else { 0.0 }
    }

    /// Preferred widths survive a constrained window. Only the rendered
    /// columns shrink; growing the window restores the person's preferences.
    fn effective_widths(&self) -> Widths {
        let rail = self
            .widths
            .rail
            .min((self.window - WORKSPACE_CHROME - self.minimum_messages_width()).max(RAIL.0));
        let available = (self.window - rail - WORKSPACE_CHROME).max(1.0);
        let thread_min = if self.thread.is_some() { THREAD.0 } else { 0.0 };
        let sidebar = self
            .widths
            .sidebar
            .min((available - CONVERSATION_MIN - thread_min).max(SIDEBAR.0));
        let thread = self
            .widths
            .thread
            .min((available - sidebar - CONVERSATION_MIN).max(THREAD.0));
        Widths {
            rail,
            sidebar,
            thread,
        }
    }

    fn messages_width(&self) -> f32 {
        (self.window - self.effective_widths().rail - WORKSPACE_CHROME).max(1.0)
    }

    /// When even the minimum columns do not fit, use the existing
    /// conversation/thread navigation instead of squeezing the composer.
    pub fn compact_messages(&self) -> bool {
        self.messages_width() < self.minimum_messages_width()
    }

    /// Ratios from effective pixels, for the window as it is now.
    fn apply(&mut self) {
        let widths = self.effective_widths();
        let ratio = |part: f32, whole: f32| (part / whole.max(1.0)).clamp(0.05, 0.95);
        self.shell
            .resize(self.rail_split, ratio(widths.rail, self.window));
        let width = self.messages_width();
        self.messages
            .resize(self.sidebar_split, ratio(widths.sidebar, width));
        if let Some((_, split)) = self.thread {
            let rest = (width - widths.sidebar).max(1.0);
            self.messages
                .resize(split, 1.0 - ratio(widths.thread, rest));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constrained_windows_keep_a_usable_conversation_and_restore_preferences() {
        let preferred = Widths {
            rail: 440.0,
            sidebar: 560.0,
            thread: 640.0,
        };
        let mut panes = Panes::new(preferred, 2400.0);
        for thread in [false, true] {
            panes.sync_thread(thread);
            for width in [900.0, 1000.0, 1200.0, 1600.0, 2400.0] {
                panes.window_width(width);
                assert_eq!(
                    panes.widths, preferred,
                    "shrinking must not save new preferences"
                );
                if !panes.compact_messages() {
                    let regions = panes.messages.layout().pane_regions(
                        0.0,
                        50.0,
                        iced::Size::new(panes.messages_width(), 600.0),
                    );
                    assert!(
                        regions[&panes.conversation].width >= CONVERSATION_MIN - 0.01,
                        "conversation collapsed at width {width}, thread {thread}"
                    );
                } else {
                    assert!(thread && width < 1001.0);
                }
            }
            assert_eq!(panes.effective_widths(), preferred);
        }
        panes.window_width(950.0);
        assert!(panes.compact_messages());
        panes.sync_thread(false);
        assert!(
            !panes.compact_messages(),
            "closing a thread restores columns when they fit"
        );
        assert_eq!(panes.widths, preferred);
    }

    /// Widths are pixels: a wider window leaves the rail where it was, a
    /// drag sets it, and the thread column comes and goes with the thread.
    #[test]
    fn columns_keep_their_pixels_and_the_thread_comes_and_goes() {
        let mut panes = Panes::new(Widths::default(), 1200.0);
        let rail_ratio = |panes: &Panes| match panes.shell.layout() {
            pane_grid::Node::Split { ratio, .. } => *ratio,
            pane_grid::Node::Pane(_) => panic!("split"),
        };
        assert!((rail_ratio(&panes) - 236.0 / 1200.0).abs() < 1e-4);
        panes.window_width(2400.0);
        assert!(
            (rail_ratio(&panes) - 236.0 / 2400.0).abs() < 1e-4,
            "pixels, not a share"
        );
        assert!(panes.resized(
            Grid::Shell,
            pane_grid::ResizeEvent {
                split: panes.rail_split,
                ratio: 0.125,
            }
        ));
        assert!((panes.widths.rail - 300.0).abs() < 1e-3);
        // Dragged past the bound, the rail stops at it.
        panes.resized(
            Grid::Shell,
            pane_grid::ResizeEvent {
                split: panes.rail_split,
                ratio: 0.9,
            },
        );
        assert!((panes.widths.rail - RAIL.1).abs() < 1e-3);
        assert!(!panes.thread_open());
        panes.sync_thread(true);
        assert!(panes.thread_open());
        assert_eq!(panes.messages.iter().count(), 3);
        panes.sync_thread(true);
        assert_eq!(panes.messages.iter().count(), 3, "once");
        let (_, thread_split) = panes.thread.expect("thread column");
        assert!(panes.resized(
            Grid::Messages,
            pane_grid::ResizeEvent {
                split: thread_split,
                ratio: 0.75,
            }
        ));
        let rest = panes.messages_width() - 272.0;
        assert!((panes.widths.thread - (0.25 * rest).round()).abs() < 1e-3);
        panes.sync_thread(false);
        assert!(!panes.thread_open());
        assert_eq!(panes.messages.iter().count(), 2);
        // A drag that leaves a column where it is, of a split that is no
        // longer there, or of one grid's split reported for the other,
        // changes nothing to remember.
        let widths = panes.widths;
        assert!(!panes.resized(
            Grid::Messages,
            pane_grid::ResizeEvent {
                split: panes.sidebar_split,
                ratio: panes.widths.sidebar / panes.messages_width(),
            }
        ));
        assert!(!panes.resized(
            Grid::Messages,
            pane_grid::ResizeEvent {
                split: thread_split,
                ratio: 0.3,
            }
        ));
        assert!(!panes.resized(
            Grid::Shell,
            pane_grid::ResizeEvent {
                split: panes.sidebar_split,
                ratio: 0.3,
            }
        ));
        assert_eq!(panes.widths, widths);
    }
}
