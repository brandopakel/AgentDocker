//! Back and Forward: the places the person went, and the way to them again.
//!
//! A [`Place`] is a destination as the window shows it: the screen and
//! what is open on it — the project, the session, the conversation and its
//! thread, the board's card. Hover, menus, drafts, filters and scroll are
//! not places. Each update the person makes (a click, a key, a link, a
//! notification, an assistive technology's action) is compared before and
//! after; a different place records where they were. Changes the window
//! makes on its own (a reply arriving, a vanished session settling) refine
//! the place on view and record nothing, except a launched session's
//! terminal opening and a notification's destination opening, which finish
//! something the person asked for.
//!
//! The rules are a browser's: Back and Forward restore a place without
//! recording one, a new place after going back drops what was ahead, a
//! place equal to the one on view is passed over, and so is one whose
//! target is gone (a project removed, a session no longer listed, a card
//! archived). At most [`PLACES`] are kept behind.
use super::icons::{Icon, icon};
use super::style::{Colors, alpha};
use super::*;
use crate::controls::{Kind, custom_sized};
use agentdocker_core::TaskId;
use iced::widget::{container, row};
use iced::{Center, Element, Length, Task};
use std::collections::VecDeque;
use std::path::PathBuf;

/// How many places Back can reach; the oldest goes beyond it.
pub const PLACES: usize = 100;

/// What a project screen is about: every project, the sessions outside
/// any, or one project by its root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scope {
    All,
    Other,
    Project(PathBuf),
}

/// Where the person is: the screen, and what is open on it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Place {
    pub screen: Screen,
    /// The project a project screen is about; none on the window's own
    /// screens (Messages or Inbox, To-do, Tools, Settings).
    pub scope: Option<Scope>,
    /// The session open on Agents, or whose terminal is on Terminal.
    pub session: Option<String>,
    /// The conversation open on Messages (a project's Chat is its own).
    pub conversation: Option<String>,
    /// The agent whose thread is open in Inbox, against a daemon without
    /// conversations.
    pub inbox: Option<String>,
    /// The thread open beside the conversation, on Chat or Messages.
    pub thread: Option<MessageId>,
    /// The card open on Board.
    pub card: Option<TaskId>,
}

/// The places behind and ahead of the one on view. Window-local, never
/// saved.
#[derive(Debug, Default)]
pub struct History {
    back: VecDeque<Place>,
    forward: Vec<Place>,
    /// An update is in hand: what it runs inside is part of it.
    busy: bool,
}

impl History {
    /// The person went from `before` to `after`: a different place keeps
    /// `before` behind and drops what was ahead.
    pub fn visit(&mut self, before: Place, after: &Place) {
        if before != *after {
            self.keep(false, before);
            self.forward.clear();
        }
    }

    /// Put a place behind (`ahead == false`) or ahead, within the bound.
    fn keep(&mut self, ahead: bool, place: Place) {
        if ahead {
            if self.forward.len() >= PLACES {
                self.forward.remove(0);
            }
            self.forward.push(place);
        } else {
            if self.back.len() >= PLACES {
                self.back.pop_front();
            }
            self.back.push_back(place);
        }
    }

    /// The nearest place behind or ahead, taken off.
    fn take(&mut self, ahead: bool) -> Option<Place> {
        if ahead {
            self.forward.pop()
        } else {
            self.back.pop_back()
        }
    }

    /// Behind or ahead, nearest first.
    fn nearest(&self, ahead: bool) -> Box<dyn Iterator<Item = &Place> + '_> {
        if ahead {
            Box::new(self.forward.iter().rev())
        } else {
            Box::new(self.back.iter().rev())
        }
    }

    /// The places behind and ahead, for the tests to read.
    #[cfg(test)]
    pub fn lengths(&self) -> (usize, usize) {
        (self.back.len(), self.forward.len())
    }

    /// A card was archived: no place shows it any more.
    pub fn forget_card(&mut self, card: &TaskId) {
        self.back.retain(|p| p.card.as_ref() != Some(card));
        self.forward.retain(|p| p.card.as_ref() != Some(card));
    }
}

/// How an update counts for Back and Forward.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Moved {
    /// The person's doing: a change of place is recorded.
    ByPerson,
    /// Back or Forward itself, which keeps the history its own way.
    Travel,
    /// The window keeping up: replies, ticks, saves, the window's own
    /// events. What changes refines the place on view.
    ByWindow,
}

/// What an update is, for Back and Forward. Anything not named here is
/// the person's, so a new control that navigates is recorded.
pub(super) fn moved(message: &Message) -> Moved {
    match message {
        Message::GoBack | Message::GoForward => Moved::Travel,
        Message::Tick
        | Message::DraftsSaved(..)
        | Message::CatalogSaved(..)
        | Message::ExclusionsSynced(..)
        | Message::PullRequests(_)
        | Message::PullRequestOpened(_)
        | Message::EditorsFound(_)
        | Message::EditorOpened(_)
        | Message::NativeTerminalOpened(_)
        | Message::CodexTrustLoaded(..)
        | Message::LaunchChoices(..)
        | Message::ProjectLocation(..)
        | Message::NoticeDone
        | Message::WaitGrace
        | Message::Window(_)
        | Message::WindowScale(_)
        | Message::NativeAccessibility(_)
        | Message::Captured(_)
        | Message::Accessibility(_)
        | Message::Event(iced::Event::Window(_)) => Moved::ByWindow,
        _ => Moved::ByPerson,
    }
}

/// The window's events the app listens to, as messages: keys no control
/// took, the window's own events, and the mouse's Back and Forward
/// buttons, wherever the pointer is.
pub(super) fn listened(event: iced::Event, status: iced::event::Status) -> Option<Message> {
    use iced::mouse::{Button, Event as Mouse};
    match event {
        iced::Event::Keyboard(_) if status == iced::event::Status::Ignored => {
            Some(Message::Event(event))
        }
        iced::Event::Window(iced::window::Event::RedrawRequested(_)) => None,
        iced::Event::Window(_) => Some(Message::Event(event)),
        iced::Event::Mouse(Mouse::ButtonPressed(Button::Back)) => Some(Message::GoBack),
        iced::Event::Mouse(Mouse::ButtonPressed(Button::Forward)) => Some(Message::GoForward),
        _ => None,
    }
}

/// Back (`false`) or Forward (`true`) by key: ⌘[ and ⌘] (Control
/// elsewhere), and a keyboard's own Browser Back and Forward keys.
pub(super) fn travel_key(
    key: &iced::keyboard::Key,
    physical: iced::keyboard::key::Physical,
    modifiers: iced::keyboard::Modifiers,
) -> Option<bool> {
    use iced::keyboard::Key;
    use iced::keyboard::key::{Code, Named, Physical};
    match key {
        Key::Named(Named::BrowserBack) => return Some(false),
        Key::Named(Named::BrowserForward) => return Some(true),
        _ => {}
    }
    if !modifiers.command() || modifiers.shift() || modifiers.alt() {
        return None;
    }
    // The character first; the key's place on the keyboard for layouts
    // whose bracket keys type something else.
    match (key.to_latin(physical), physical) {
        (Some('['), _) | (_, Physical::Code(Code::BracketLeft)) => Some(false),
        (Some(']'), _) | (_, Physical::Code(Code::BracketRight)) => Some(true),
        _ => None,
    }
}

impl App {
    /// Run `f` as one move: when no update is in hand already, the place
    /// before and after it are compared and, when `record`, a change is
    /// kept. What `f` runs inside it belongs to it.
    pub(super) fn navigating<T>(&mut self, record: bool, f: impl FnOnce(&mut Self) -> T) -> T {
        if self.shell.history.busy {
            return f(self);
        }
        let before = record.then(|| self.place());
        self.shell.history.busy = true;
        let out = f(self);
        self.shell.history.busy = false;
        if let Some(before) = before {
            let after = self.place();
            self.shell.history.visit(before, &after);
        }
        out
    }

    /// What the project screens are about now.
    pub(super) fn scope(&self) -> Scope {
        match &self.shell.catalog.selected {
            Some(root) => Scope::Project(root.clone()),
            None if self.shell.catalog.unassigned => Scope::Other,
            None => Scope::All,
        }
    }

    /// Where the person is: only what the screen on view shows.
    pub(super) fn place(&self) -> Place {
        let screen = self.screen;
        let on = |wanted: &[Screen]| wanted.contains(&screen);
        Place {
            screen,
            scope: self.in_project().then(|| self.scope()),
            session: match screen {
                Screen::Agents => self
                    .shell
                    .selected
                    .as_deref()
                    .map(|id| self.canonical_agent(id).to_owned()),
                Screen::Terminal => self.terminal.as_ref().map(|t| t.agent.clone()),
                _ => None,
            },
            conversation: on(&[Screen::Questions])
                .then(|| self.shell.conversation.clone())
                .flatten(),
            inbox: on(&[Screen::Questions])
                .then(|| self.shell.inbox_thread.clone())
                .flatten(),
            thread: on(&[Screen::Chat, Screen::Questions])
                .then(|| self.shell.thread.clone())
                .flatten(),
            card: on(&[Screen::Board])
                .then(|| self.task_open.clone())
                .flatten(),
        }
    }

    /// Whether a place can be shown again: its project still listed, its
    /// session still known (its terminal still attachable), its card not
    /// archived.
    pub(super) fn reachable(&self, place: &Place) -> bool {
        if let Some(Scope::Project(root)) = &place.scope
            && !self
                .shell
                .catalog
                .projects
                .iter()
                .any(|e| &e.project.root == root)
        {
            return false;
        }
        if place.screen == Screen::Terminal {
            let Some(id) = place.session.as_deref() else {
                return false;
            };
            let id = self.canonical_agent(id);
            return self.terminal.as_ref().is_some_and(|t| t.agent == id)
                || self
                    .agents
                    .iter()
                    .any(|a| a.id.as_str() == id && a.managed && a.spec.tty && a.status.is_live());
        }
        if let Some(id) = place.session.as_deref() {
            let id = self.canonical_agent(id);
            if !self.agents.iter().any(|a| a.id.as_str() == id) {
                return false;
            }
        }
        if let Some(card) = &place.card
            && self.card_gone(place, card)
        {
            return false;
        }
        true
    }

    /// A card known to be off its board: archived as last read, or missing
    /// from its project's board read in full.
    fn card_gone(&self, place: &Place, card: &TaskId) -> bool {
        if self
            .card
            .as_ref()
            .is_some_and(|d| &d.task.id == card && d.task.archived_at.is_some())
        {
            return true;
        }
        let Some(Scope::Project(root)) = &place.scope else {
            return false;
        };
        self.tasks.as_ref().is_some_and(|board| {
            board.project == root.display().to_string()
                && !board.more
                && board.pending_more.is_none()
                && !board.cards.iter().any(|t| &t.id == card)
        })
    }

    /// The nearest place Back (`ahead == false`) or Forward would go to.
    pub(super) fn destination(&self, ahead: bool) -> Option<&Place> {
        let current = self.place();
        self.shell
            .history
            .nearest(ahead)
            .find(|p| **p != current && self.reachable(p))
    }

    /// Back or Forward: the nearest place that can be shown, the one left
    /// kept on the other side. Places passed over are let go. Nothing
    /// while the palette or the launch form is over the window.
    pub(super) fn travel(&mut self, ahead: bool) -> Task<Message> {
        if self.shell.palette.open || self.shell.launch {
            return Task::none();
        }
        let current = self.place();
        while let Some(place) = self.shell.history.take(ahead) {
            if place == current || !self.reachable(&place) {
                continue;
            }
            self.shell.history.keep(!ahead, current);
            return self.restore(&place);
        }
        Task::none()
    }

    /// Show a place again, by the messages that went there: the project
    /// first, then the screen, then what is open on it.
    fn restore(&mut self, place: &Place) -> Task<Message> {
        let mut tasks = Vec::new();
        // The page's menus close with it.
        self.shell.more = false;
        self.shell.launch_menu = false;
        self.shell.editor_menu = None;
        self.shell.project_menu = None;
        self.shell.card_menu = None;
        self.shell.tags_menu = false;
        if let Some(scope) = &place.scope
            && *scope != self.scope()
        {
            tasks.push(self.update(match scope {
                Scope::All => Message::AllProjects,
                Scope::Other => Message::Unassigned,
                Scope::Project(root) => Message::SelectProject(root.clone()),
            }));
        }
        match (place.screen, place.session.clone()) {
            (Screen::Terminal, Some(id))
                if self.terminal.as_ref().is_some_and(|t| t.agent == id) =>
            {
                self.screen = Screen::Terminal;
            }
            (Screen::Terminal, Some(id)) => tasks.push(self.update(Message::Attach(id))),
            (screen, _) => tasks.push(self.update(Message::Navigate(screen))),
        }
        match place.screen {
            Screen::Agents => match &place.session {
                Some(id) => tasks.push(self.update(Message::SelectSession(id.clone()))),
                None => self.shell.selected = None,
            },
            Screen::Board => match &place.card {
                Some(card) if self.task_open.as_ref() != Some(card) => {
                    tasks.push(self.update(Message::TaskOpen(card.clone())));
                }
                Some(_) => {}
                None => {
                    self.task_open = None;
                    self.card = None;
                }
            },
            Screen::Questions => {
                self.shell.inbox_thread = place.inbox.clone();
                match &place.conversation {
                    Some(id) => tasks.push(self.update(Message::SelectConversation(id.clone()))),
                    None => {
                        self.shell.conversation = None;
                        self.shell.thread = None;
                        self.thread = None;
                        self.shell.inbox_open = place.inbox.is_some();
                    }
                }
            }
            _ => {}
        }
        if matches!(place.screen, Screen::Chat | Screen::Questions) {
            match &place.thread {
                Some(root) if self.shell.thread.as_ref() != Some(root) => {
                    tasks.push(self.update(Message::OpenThread(root.clone())));
                }
                None if self.shell.thread.is_some() => {
                    tasks.push(self.update(Message::CloseThread));
                }
                _ => {}
            }
        }
        Task::batch(tasks)
    }

    /// A place in a few words, as the arrows' tooltips say it: the
    /// project's name and *Chat*, the project's name and a session's,
    /// *Messages · #everyone*, *Settings*.
    pub(super) fn place_name(&self, place: &Place) -> String {
        let scope = place.scope.as_ref().map(|scope| match scope {
            Scope::All => "All projects".to_owned(),
            Scope::Other => "Other sessions".to_owned(),
            Scope::Project(root) => self
                .shell
                .catalog
                .projects
                .iter()
                .find(|e| &e.project.root == root)
                .map(|e| e.name())
                .unwrap_or_else(|| root.display().to_string()),
        });
        let conversations = self.has_conversations();
        let what = match place.screen {
            Screen::Chat => Some("Chat".to_owned()),
            // A project's Agents is named; every project's, or the
            // sessions outside any, are their scope's name alone.
            Screen::Agents => match &place.session {
                Some(id) => Some(self.name_of(id)),
                None if matches!(place.scope, Some(Scope::Project(_))) => Some("Agents".to_owned()),
                None => None,
            },
            Screen::Terminal => Some(match &place.session {
                Some(id) => format!("{} · Terminal", self.name_of(id)),
                None => "Terminal".to_owned(),
            }),
            Screen::Board => Some(
                place
                    .card
                    .as_ref()
                    .and_then(|card| self.card_name(card))
                    .unwrap_or_else(|| "Board".to_owned()),
            ),
            Screen::Plans => Some("Plans".to_owned()),
            Screen::Usage => Some("Usage".to_owned()),
            Screen::Journal => Some("History".to_owned()),
            Screen::Leases => Some("Files in use".to_owned()),
            Screen::Console => Some("Commands".to_owned()),
            Screen::Todo => Some("To-do".to_owned()),
            Screen::Questions if conversations => Some(
                match place
                    .conversation
                    .as_deref()
                    .and_then(|id| self.conversation_name(id))
                {
                    Some(name) => format!("Messages · {name}"),
                    None => "Messages".to_owned(),
                },
            ),
            Screen::Questions => Some(match &place.inbox {
                Some(agent) => format!("Inbox · {}", self.name_of(agent)),
                None => "Inbox".to_owned(),
            }),
            Screen::Runtimes => Some("Tools".to_owned()),
            Screen::Settings => Some("Settings".to_owned()),
            Screen::Desktop => Some("Settings · Installation".to_owned()),
        };
        scope
            .into_iter()
            .chain(what)
            .collect::<Vec<_>>()
            .join(" · ")
    }

    /// A card by its key, else its title, from what the window has read.
    fn card_name(&self, card: &TaskId) -> Option<String> {
        let task = self
            .card
            .as_ref()
            .map(|d| &d.task)
            .filter(|t| &t.id == card)
            .or_else(|| {
                self.tasks
                    .as_ref()
                    .and_then(|b| b.cards.iter().find(|t| &t.id == card))
            })
            .or_else(|| {
                self.todo
                    .as_ref()
                    .and_then(|cards| cards.iter().find(|t| &t.id == card))
            })?;
        Some(if task.key.is_empty() {
            super::view::first_line(&task.title, 40)
        } else {
            task.key.clone()
        })
    }

    /// A conversation as the Messages sidebar names it, when listed.
    fn conversation_name(&self, id: &str) -> Option<String> {
        self.conversations
            .iter()
            .find(|c| c.conversation.as_str() == id)
            .map(|summary| self.conversation_label(summary))
    }

    /// What an arrow says when pointed at: where it goes, or its name
    /// alone when there is nowhere to go.
    #[cfg(test)]
    pub(super) fn travel_tip(&self, ahead: bool) -> String {
        self.arrow_words(ahead, self.destination(ahead))
    }

    fn arrow_words(&self, ahead: bool, destination: Option<&Place>) -> String {
        let word = if ahead { "Forward" } else { "Back" };
        match destination {
            Some(place) => format!("{word} to {}", self.place_name(place)),
            None => word.to_owned(),
        }
    }

    /// Back and Forward, beside the mark at the top of the rail: two
    /// arrows, each disabled while there is nowhere to go, named *Back*
    /// and *Forward* to a reader and saying where they go when pointed at.
    pub(super) fn travel_controls(&self, r: Colors, c: Colors) -> Element<'_, Message> {
        let arrow = |ahead: bool| -> Element<'_, Message> {
            let (id, word, glyph, message) = if ahead {
                (
                    "history-forward",
                    "Forward",
                    Icon::ChevronRight,
                    Message::GoForward,
                )
            } else {
                ("history-back", "Back", Icon::ChevronLeft, Message::GoBack)
            };
            let destination = self.destination(ahead);
            let open = destination.is_some();
            let ink = if open { r.muted } else { alpha(r.muted, 0.45) };
            super::view::tip(
                custom_sized(
                    id,
                    word,
                    container(icon(glyph, ink, 14.0)).center(16),
                    open.then_some(message),
                    false,
                    Kind::Nav,
                    [4, 4],
                    Length::Shrink,
                ),
                self.arrow_words(ahead, destination),
                c,
            )
        };
        row![arrow(false), arrow(true)]
            .spacing(2)
            .align_y(Center)
            .into()
    }
}
