//! Iced state transitions. Effects stay on bounded workers or explicit tasks.
use super::*;
use crate::catalog::Catalog;
use iced::{Subscription, Task, keyboard, window};
use std::path::PathBuf;

#[derive(Default)]
pub(super) struct State {
    pub catalog: Catalog,
    pub selected: Option<String>,
    pub search: String,
    pub session_filter: super::sessions::Filter,
    pub more: bool,
    pub terminal_opening: bool,
    pub session_details: bool,
    pub review_delivery: bool,
    pub session_message: bool,
    pub session_drafts: BTreeMap<String, SessionDraft>,
    /// Text only, keyed by the original question ID. Never restores approval or sending state.
    pub answers: BTreeMap<MessageId, String>,
    /// Card text belongs to its original project; filing state is never restored.
    pub task_drafts: BTreeMap<String, TaskDraft>,
    pub drafts: crate::drafts::Persistence,
    pub draft_home: PathBuf,
    pub connection_details: Option<String>,
    pub other_tools: bool,
    pub width: f32,
    pub height: f32,
    pub dpi: f32,
    pub add_path: String,
    pub adding: bool,
    pub launch: bool,
    pub launch_runtime: Option<String>,
    pub launch_name: String,
    pub launch_arguments: String,
    /// Provider input is on for supported new sessions by default. The launch
    /// control can turn it off; provider consent and policy still apply.
    pub launch_channel: bool,
    pub launching: bool,
    /// The session whose resume is on its way to the daemon, so its
    /// button alone says "Reconnecting…" (a launch from the Launch form
    /// sets `launching` too, and is not a reconnect).
    pub reconnecting: Option<String>,
    /// A session just reconnected: its pane opens when the list shows the
    /// process the daemon started (by its start time) running, and the
    /// list decides — a session that ended at once stays in the list with
    /// its exit and no pane opens on it; an older list waits.
    pub attach_when_listed: Option<(String, Option<chrono::DateTime<chrono::Utc>>)>,
    pub error: Option<String>,
    pub setup_error: Option<String>,
    pub answer_errors: BTreeMap<MessageId, String>,
    pub file_review: Option<MessageId>,
    pub message_detail: Option<MessageId>,
    /// The conversation open in Messages, by its id (`everyone:<project>`,
    /// `channel:<id>`, `dm:<a>:<b>`, ...), and the thread root open beside
    /// it, if any.
    pub conversation: Option<String>,
    pub thread: Option<MessageId>,
    pub conversation_drafts: BTreeMap<String, ChannelDraft>,
    /// The sidebar's filter text.
    pub messages_search: String,
    /// Whether the collapsed sidebar groups are open.
    pub collisions_open: bool,
    pub earlier_open: bool,
    /// How many of the Earlier group's entries are on screen: a page, and
    /// a page more for each *Show older*; closing the group resets it.
    pub earlier_shown: usize,
    /// Whether the temporary projects (discovered under /tmp, unpinned)
    /// are unfolded in the sidebar: the person's choice once they have
    /// toggled it, until then automatic (open while one of them has a
    /// live session).
    pub temporary_open: Option<bool>,
    /// Whether the conversations between agents are unfolded.
    pub peers_open: bool,
    /// The project row whose menu is open.
    pub project_menu: Option<PathBuf>,
    /// The project being renamed, and the name so far.
    pub project_rename: Option<(PathBuf, String)>,
    /// The conversation open in Inbox: one agent, or every agent at once.
    pub inbox_thread: Option<String>,
    /// In a narrow window Inbox shows either the list or one conversation;
    /// this is which. Wide windows show both and ignore it.
    pub inbox_open: bool,
    pub needs_you_expanded: bool,
    pub pending_answer_reveal: Option<MessageId>,
    pub reveal_next_question: bool,
    /// An archived message now on view that the next tick scrolls to.
    pub reveal_archived_next: Option<MessageId>,
    pub channel_drafts: BTreeMap<String, ChannelDraft>,
    pub channel_target: Option<String>,
    pub generation: u64,
    pub saved_generation: u64,
    pub saving: bool,
    pending_update: Option<i64>,
    pub save_enabled: bool,
    pub window: Option<window::Id>,
    pub closing: bool,
    pub checked_project: Option<PathBuf>,
    pub project_available: Option<bool>,
    pub notification_message: Option<MessageId>,
    pending_notification: Option<(agentdocker_host::notify::Action, Instant)>,
    /// Replies typed into notifications that did not go: each waits for
    /// its conversation to open so the words become its draft, and stays
    /// — shown beside the composer to copy or dismiss — while the draft
    /// cannot take them. At most [`REPLY_RECOVERIES`]; a later one is
    /// refused and said to be.
    pub reply_recoveries: Vec<ReplyRecovery>,
    /// Sessions whose turn finished while nobody was looking at them:
    /// finished as observed, not yet viewed. Viewing is an explicit act
    /// (opening the project or the session, or already having it on
    /// screen in a focused window when it finished); an `idle` report on
    /// its own never counts as seen. Window-local, never persisted.
    pub unviewed_done: BTreeSet<String>,
    /// The window has lost focus; what it shows is not being looked at.
    pub unfocused: bool,
}

impl State {
    /// Every unviewed completion in the project at `root`, for a badge.
    pub fn unviewed_in(&self, agents: &[AgentRecord], root: &std::path::Path) -> usize {
        agents
            .iter()
            .filter(|a| {
                self.unviewed_done.contains(a.id.as_str())
                    && a.project.as_ref().is_some_and(|p| p.root == root)
            })
            .count()
    }

    /// The user opened the project at `root`: its completions are viewed.
    pub fn viewed_project(&mut self, agents: &[AgentRecord], root: &std::path::Path) {
        for agent in agents {
            if agent.project.as_ref().is_some_and(|p| p.root == root) {
                self.unviewed_done.remove(agent.id.as_str());
            }
        }
    }
}

/// The words of a failed notification reply, and why it failed, until
/// they are in the conversation's draft, copied, or dismissed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplyRecovery {
    pub message: MessageId,
    pub text: String,
    pub reason: String,
    pub certain: bool,
    /// The conversation it was placed towards, once known, so the
    /// composer there can show what the draft could not take.
    pub conversation: Option<String>,
}

/// How many failed replies the window keeps at once.
pub const REPLY_RECOVERIES: usize = 8;

/// Each room keeps its own draft; receipts clear only an untouched submission.
#[derive(Clone, Debug, Default)]
pub(super) struct ChannelDraft {
    pub text: String,
    pub sending: Option<String>,
    pub error: Option<String>,
    pub readiness: Option<agentdocker_core::SendReadiness>,
    pub readiness_expanded: bool,
    edited_since_send: bool,
}

#[derive(Default)]
pub(super) struct SessionDraft {
    pub draft: ChannelDraft,
    pub queued: Option<MessageId>,
}
impl ChannelDraft {
    pub fn edit(&mut self, text: String) {
        self.text = text.chars().take(16_000).collect();
        // Only one send can be pending. Remember any edit during that send,
        // including changing back to identical text, without a wrapping counter.
        self.edited_since_send = true;
    }
    pub fn begin(&mut self) -> Option<String> {
        if self.sending.is_some() || self.text.trim().is_empty() {
            return None;
        }
        self.error = None;
        self.readiness = None;
        self.readiness_expanded = false;
        self.edited_since_send = false;
        self.sending = Some(self.text.clone());
        self.sending.clone()
    }
    pub fn complete(&mut self, result: Result<(), String>) {
        let sent = self.sending.take();
        match result {
            Ok(()) if !self.edited_since_send && sent.as_ref() == Some(&self.text) => {
                self.text.clear();
            }
            Ok(()) => {}
            Err(error) => self.error = Some(error),
        }
    }
}

impl State {
    pub fn load(home: &std::path::Path) -> Self {
        let (catalog, error, save_enabled) = match Catalog::load(home) {
            Ok(mut catalog) => {
                // Folders discovered under the temporary directories before
                // discovery stopped listing them go, and so does any
                // discovered folder that no longer exists; a pinned one
                // stays.
                catalog
                    .projects
                    .retain(|e| e.pinned || !crate::catalog::is_temporary(&e.project.root));
                catalog.forget_missing();
                if catalog.selected().is_none() {
                    catalog.selected = None;
                }
                (catalog, None, true)
            }
            Err(error) => (
                Catalog::default(),
                Some(format!(
                    "Cannot load saved workspace: {error}. The file has been preserved; changes in this window will not overwrite it."
                )),
                false,
            ),
        };
        // Different daemon sockets can have independent desktop windows even
        // under one state root. Never restore or overwrite another's drafts.
        let draft_home = home
            .join("drafts")
            .join(agentdocker_host::notify::instance_key(
                home,
                &agentdocker_host::dirs::socket_path(home),
            ));
        let (saved, drafts) = match crate::drafts::Snapshot::load(&draft_home) {
            Ok(saved) => (saved, crate::drafts::Persistence::loaded()),
            Err(error) => (
                crate::drafts::Snapshot::default(),
                crate::drafts::Persistence::unavailable(format!(
                    "Saved drafts could not be opened: {error}. The file is preserved; new text cannot be saved until it is recovered."
                )),
            ),
        };
        Self {
            catalog,
            error,
            save_enabled,
            drafts,
            draft_home,
            session_drafts: saved
                .sessions
                .into_iter()
                .map(|(key, text)| {
                    (
                        key,
                        SessionDraft {
                            draft: ChannelDraft {
                                text,
                                ..Default::default()
                            },
                            ..Default::default()
                        },
                    )
                })
                .collect(),
            conversation_drafts: saved
                .conversations
                .into_iter()
                .map(|(key, text)| {
                    (
                        key,
                        ChannelDraft {
                            text,
                            ..Default::default()
                        },
                    )
                })
                .collect(),
            channel_drafts: saved
                .channels
                .into_iter()
                .map(|(key, text)| {
                    (
                        key,
                        ChannelDraft {
                            text,
                            ..Default::default()
                        },
                    )
                })
                .collect(),
            answers: saved
                .answers
                .into_iter()
                .map(|(id, text)| (id.into(), text))
                .collect(),
            task_drafts: saved
                .boards
                .into_iter()
                .map(|(project, draft)| {
                    (
                        project,
                        TaskDraft {
                            title: draft.title,
                            acceptance: draft.acceptance,
                            ..Default::default()
                        },
                    )
                })
                .collect(),
            dpi: 1.0,
            width: 1180.0,
            height: 760.0,
            ..Default::default()
        }
    }
    pub fn changed(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }

    /// Failed notification replies whose conversation could not be
    /// opened — the message, project or channel is gone, or the route was
    /// cancelled — and which no route is still on its way to. They are
    /// shown where the person can always reach them, whatever is on view.
    pub fn orphan_reply_recoveries(&self) -> Vec<&ReplyRecovery> {
        let routing: Option<&MessageId> = self
            .pending_notification
            .as_ref()
            .map(|(action, _)| &action.target.message);
        self.reply_recoveries
            .iter()
            .filter(|r| r.conversation.is_none())
            .filter(|r| routing != Some(&r.message))
            .filter(|r| self.notification_message.as_ref() != Some(&r.message))
            .collect()
    }
}

#[derive(Clone, Copy)]
enum DraftKind {
    Session,
    Conversation,
    Channel,
    Answer,
    TaskTitle,
    TaskAcceptance,
}

impl State {
    fn draft_snapshot(&self) -> crate::drafts::Snapshot {
        crate::drafts::Snapshot {
            sessions: self
                .session_drafts
                .iter()
                .filter(|(_, d)| !d.draft.text.is_empty())
                .map(|(k, d)| (k.clone(), d.draft.text.clone()))
                .collect(),
            conversations: self
                .conversation_drafts
                .iter()
                .filter(|(_, d)| !d.text.is_empty())
                .map(|(k, d)| (k.clone(), d.text.clone()))
                .collect(),
            channels: self
                .channel_drafts
                .iter()
                .filter(|(_, d)| !d.text.is_empty())
                .map(|(k, d)| (k.clone(), d.text.clone()))
                .collect(),
            answers: self
                .answers
                .iter()
                .filter(|(_, text)| !text.is_empty())
                .map(|(id, text)| (id.to_string(), text.clone()))
                .collect(),
            boards: self
                .task_drafts
                .iter()
                .filter(|(_, draft)| !draft.title.is_empty() || !draft.acceptance.is_empty())
                .map(|(project, draft)| {
                    (
                        project.clone(),
                        crate::drafts::BoardDraft {
                            title: draft.title.clone(),
                            acceptance: draft.acceptance.clone(),
                        },
                    )
                })
                .collect(),
            ..Default::default()
        }
    }

    fn edit_draft(&mut self, kind: DraftKind, id: String, text: String) -> bool {
        let question = MessageId::from(id.clone());
        let old = match kind {
            DraftKind::Session => self.session_drafts.get(&id).map(|d| &d.draft.text),
            DraftKind::Conversation => self.conversation_drafts.get(&id).map(|d| &d.text),
            DraftKind::Channel => self.channel_drafts.get(&id).map(|d| &d.text),
            DraftKind::Answer => self.answers.get(&question),
            DraftKind::TaskTitle => self.task_drafts.get(&id).map(|d| &d.title),
            DraftKind::TaskAcceptance => self.task_drafts.get(&id).map(|d| &d.acceptance),
        };
        let total: usize = self
            .session_drafts
            .values()
            .map(|d| d.draft.text.len())
            .chain(self.conversation_drafts.values().map(|d| d.text.len()))
            .chain(self.channel_drafts.values().map(|d| d.text.len()))
            .chain(self.answers.values().map(String::len))
            .chain(
                self.task_drafts
                    .values()
                    .map(|d| d.title.len() + d.acceptance.len()),
            )
            .sum();
        let error = if id.is_empty() || id.len() > 1024 {
            Some("This draft destination is too long.")
        } else if matches!(kind, DraftKind::TaskTitle)
            && text.chars().count() > agentdocker_core::task::TITLE_CHARS
        {
            Some("Card titles can contain up to 200 characters. Your earlier text was kept.")
        } else if matches!(kind, DraftKind::TaskAcceptance)
            && text.chars().count() > agentdocker_core::task::ACCEPTANCE_CHARS
        {
            Some("Acceptance text can contain up to 4,000 characters. Your earlier text was kept.")
        } else if text.chars().count() > crate::drafts::MAX_TEXT_CHARS {
            Some("Drafts can contain up to 16,000 characters. Your earlier text was kept.")
        } else if total - old.map_or(0, String::len) + text.len() > crate::drafts::MAX_TOTAL_BYTES {
            Some(
                "Draft storage is full. Finish or clear an earlier draft first; your earlier text was kept.",
            )
        } else {
            None
        };
        if let Some(error) = error {
            if matches!(kind, DraftKind::TaskTitle | DraftKind::TaskAcceptance)
                && let Some(draft) = self.task_drafts.get_mut(&id)
            {
                draft.error = Some(error.into());
            } else {
                self.error = Some(error.into());
            }
            return false;
        }
        let edited = match kind {
            DraftKind::TaskTitle | DraftKind::TaskAcceptance => {
                self.task_drafts.retain(|key, d| {
                    key == &id || d.sending() || !d.title.is_empty() || !d.acceptance.is_empty()
                });
                if self.task_drafts.contains_key(&id) || self.task_drafts.len() < 128 {
                    let draft = self.task_drafts.entry(id).or_default();
                    if matches!(kind, DraftKind::TaskTitle) {
                        draft.title = text;
                    } else {
                        draft.acceptance = text;
                    }
                    draft.error = None;
                    true
                } else {
                    false
                }
            }
            DraftKind::Answer => {
                self.answers
                    .retain(|key, text| key == &question || !text.is_empty());
                if self.answers.contains_key(&question) || self.answers.len() < 128 {
                    self.answers.insert(question, text);
                    true
                } else {
                    false
                }
            }
            DraftKind::Session => {
                self.session_drafts.retain(|key, d| {
                    key == &id || !d.draft.text.is_empty() || d.draft.sending.is_some()
                });
                if self.session_drafts.contains_key(&id) || self.session_drafts.len() < 128 {
                    self.session_drafts.entry(id).or_default().draft.edit(text);
                    true
                } else {
                    false
                }
            }
            DraftKind::Conversation => {
                self.conversation_drafts
                    .retain(|key, d| key == &id || !d.text.is_empty() || d.sending.is_some());
                if self.conversation_drafts.contains_key(&id)
                    || self.conversation_drafts.len() < 128
                {
                    self.conversation_drafts.entry(id).or_default().edit(text);
                    true
                } else {
                    false
                }
            }
            DraftKind::Channel => {
                self.channel_drafts
                    .retain(|key, d| key == &id || !d.text.is_empty() || d.sending.is_some());
                if self.channel_drafts.contains_key(&id) || self.channel_drafts.len() < 128 {
                    self.channel_drafts.entry(id).or_default().edit(text);
                    true
                } else {
                    false
                }
            }
        };
        if edited {
            self.drafts.changed();
        } else {
            self.error =
                Some("Finish or clear an earlier draft first. Existing drafts were kept.".into());
        }
        edited
    }
}

#[derive(Clone, Debug)]
pub enum DeliveryTarget {
    Conversation(String),
    Session(String),
    Channel(String),
}

#[derive(Clone, Debug)]
pub enum Message {
    Tick,
    DeliveryDetails(DeliveryTarget),
    CopyGuidance(String),
    DraftsSaved(u64, Result<(), String>),
    RetryDraftSave,
    CloseWithoutDraftSave,
    Notification(crate::notification_route::Activation),
    Navigate(Screen),
    SelectProject(PathBuf),
    /// Every project at once: the home view.
    AllProjects,
    OpenQuestion(MessageId),
    ToggleNeedsYou,
    Unassigned,
    RetryProject,
    SelectSession(String),
    /// Jump to one session from anywhere: its project first, then the row.
    OpenSession(String),
    CloseSession,
    Search(String),
    SessionFilter(super::sessions::Filter),
    More,
    SessionDetails,
    ReviewDelivery,
    ResumeProvider(String, chrono::DateTime<Utc>),
    RetryController(String),
    ComposeSession,
    SessionDraft(String, String),
    SendSession(String),
    ConnectionDetails(String),
    /// Open this runtime's connection guidance without changing any draft.
    OpenConnection(String),
    ReviewFiles(MessageId),
    QuestionDetails(MessageId),
    /// Open one agent's conversation in Inbox, or all of them.
    SelectThread(Option<String>),
    SelectConversation(String),
    ConversationDraft(String, String),
    /// A kept reply from a notification, by its message: to the
    /// clipboard, or let go.
    ReplyRecoveryCopy(MessageId),
    ReplyRecoveryDismiss(MessageId),
    SendConversation(String),
    OpenThread(MessageId),
    CloseThread,
    MessagesSearch(String),
    ToggleCollisions,
    ToggleEarlier,
    /// One page more of the Earlier group.
    MoreEarlier,
    ToggleTemporary,
    TogglePeers,
    /// A divider between the window's columns was dragged, in one grid.
    PaneResized(super::panes::Grid, iced::widget::pane_grid::ResizeEvent),
    /// Every conversation the person owes a read is read through its head.
    MarkAllRead,
    /// The board: a card being filed, filed to Backlog or straight to
    /// Ready, moved, opened to read what done means, handed to an agent,
    /// or taken off the board.
    TaskTitle(String),
    TaskAcceptance(String),
    TaskFile(agentdocker_core::Column),
    TaskMove(agentdocker_core::TaskId, agentdocker_core::Column),
    TaskOpen(agentdocker_core::TaskId),
    TaskAssign(agentdocker_core::TaskId, Option<agentdocker_core::AgentId>),
    TaskArchive(agentdocker_core::TaskId),
    /// The next page of the board.
    TasksMore,
    /// Tell the selected project's agents to hold: open the reason, or
    /// send it, or lift the pause.
    PauseStart(String),
    PauseDraft(String, String),
    PauseSubmit(String),
    PauseCancel(String),
    ResumeProject(String),

    /// Start a conversation: open or close the form.
    NewConversation,
    /// A direct message or a channel.
    NewConversationKind(super::NewKind),
    NewChannelName(String),
    NewChannelPurpose(String),
    /// Put an agent in the new channel, or take it out.
    NewChannelMember(agentdocker_core::AgentId),
    /// Open the channel the form describes.
    CreateChannel,
    InviteChannel(String),
    InviteMember(String),
    /// The person picked who to message: open that direct conversation.
    NewDirect(String),
    /// Open or close the menu under a project row.
    ProjectMenu(PathBuf),
    ProjectRenameStart(PathBuf),
    ProjectRenameDraft(String),
    ProjectRenameSubmit,
    ProjectPin(PathBuf),
    /// Take a project off the list, and keep it off until it is added again.
    ProjectRemove(PathBuf),
    /// The page of the conversation's archive before what is shown.
    EarlierHistory(String),
    /// Unfold or fold one archived message's full text.
    ExpandArchived(MessageId),
    /// Back from a conversation to the list, in a narrow window.
    InboxList,
    /// Send the agent's draft to everyone in its project as well.
    SendProject(String),
    OtherTools,
    AddPath(String),
    ShowAdd,
    PickFolder,
    FolderPicked(Option<PathBuf>),
    ResolveFolder,
    FolderResolved(Result<ProjectRef, String>),
    ProjectLocation(PathBuf, bool),
    Unpin,
    ForgetProject,
    CatalogSaved(u64, Result<(), String>),
    Draft(MessageId, String),
    Answer(MessageId),
    AnswerChoice(MessageId, String),
    DismissInbox(Vec<MessageId>),
    Adopt(u32),
    AdoptAll,
    Stop(String),
    ShowLaunch,
    LaunchRuntime(String),
    LaunchName(String),
    LaunchArguments(String),
    LaunchChannel(bool),
    Launch,
    /// Relaunch an ended Claude Code session here, with its conversation
    /// and the AgentDocker channel, so it takes messages live.
    Reconnect(String),
    Attach(String),
    Detach,
    OpenProjectTerminal,
    OpenAgentTerminal(String),
    NativeTerminalOpened(Result<(), String>),
    TerminalInput(Vec<u8>),
    TerminalResize(u16, u16),
    TerminalScroll(i32),
    TerminalDismiss,
    ConsoleInput(String),
    RunConsole,
    Recall(bool),
    ChannelTarget(String),
    ChannelDraft(String),
    SendChannel,
    Setup(Vec<String>),
    SetupClose,
    DesktopSource(String),
    DesktopPrefix(String),
    DesktopLocal(bool),
    DesktopUseCurrent,
    DesktopPreview(String),
    DesktopApply,
    AutomaticUpdates(bool),
    Dark(bool),
    TextSize(f32),
    TerminalSize(f32),
    Palette(String),
    Roomy(bool),
    DismissError,
    Event(iced::Event),
    Window(window::Id),
    WindowScale(f32),
    NativeAccessibility(Result<usize, String>),
    Captured(window::Screenshot),
    Focus(String),
    Accessibility(crate::accessibility::Snapshot),
}

async fn notifications(mut output: iced::futures::channel::mpsc::Sender<Message>) {
    use iced::futures::SinkExt;
    let wake = Wake::default();
    loop {
        wake.notified().await;
        if output.send(Message::Tick).await.is_err() {
            break;
        }
    }
}

impl App {
    /// Open the conversation with one agent: the inbox thread, and with a
    /// daemon that has conversations, the direct conversation between the
    /// person and that agent.
    pub(super) fn open_thread_with(&mut self, agent: String) {
        self.shell.inbox_thread = Some(agent.clone());
        self.shell.inbox_open = true;
        if self.has_conversations() {
            self.adopt_inbox_thread();
        }
    }

    /// The direct conversation of the thread the person opened, once the
    /// daemon's conversations and the person's own record are known. A
    /// notification can open a thread before either has arrived; the
    /// conversation is settled here when they do.
    pub(super) fn adopt_inbox_thread(&mut self) {
        let Some(agent) = self.shell.inbox_thread.clone() else {
            return;
        };
        let listed = self
            .conversations
            .iter()
            .filter(|c| c.kind == agentdocker_core::ConversationKind::Dm)
            .find(|c| {
                c.conversation.dm_parties().is_some_and(|(a, b)| {
                    (a == agent && self.is_human(b)) || (b == agent && self.is_human(a))
                })
            })
            .map(|c| c.conversation.as_str().to_owned());
        let conversation = listed.unwrap_or_else(|| {
            let human = self
                .agents
                .iter()
                .find(|a| a.spec.runtime == agentdocker_core::HUMAN_RUNTIME)
                .map(|a| a.id.as_str().to_owned())
                .unwrap_or_else(|| agentdocker_core::HUMAN.to_owned());
            agentdocker_core::ConversationId::dm(&human, &agent)
                .as_str()
                .to_owned()
        });
        if self.shell.conversation.as_deref() != Some(conversation.as_str()) {
            self.shell.thread = None;
            self.thread = None;
        }
        self.shell.conversation = Some(conversation.clone());
        if self.connected.is_ok() {
            self.send(Cmd::History(conversation, self.history_epoch));
        }
        self.recover_reply();
    }

    /// Put a failed notification reply's words into the draft of the
    /// conversation the notification opened, once it is open: after what
    /// is already there, so nothing typed in either place is lost, and
    /// with the reason in the status line — for an unknown outcome, that
    /// the history decides whether to send again. A draft that cannot
    /// take them (the storage is full) keeps the recovery beside the
    /// composer, to copy or dismiss.
    pub(super) fn recover_reply(&mut self) {
        let Some(conversation) = self.shell.conversation.clone() else {
            return;
        };
        let Some(message) = self.shell.notification_message.clone() else {
            return;
        };
        let Some(index) = self
            .shell
            .reply_recoveries
            .iter()
            .position(|r| r.message == message && r.conversation.is_none())
        else {
            return;
        };
        let recovery = self.shell.reply_recoveries[index].clone();
        let existing = self
            .shell
            .conversation_drafts
            .get(&conversation)
            .map(|d| d.text.clone())
            .unwrap_or_default();
        let text = if existing.trim().is_empty() {
            recovery.text.clone()
        } else {
            format!("{existing}\n\n{}", recovery.text)
        };
        self.shell
            .edit_draft(DraftKind::Conversation, conversation.clone(), text.clone());
        let placed = self
            .shell
            .conversation_drafts
            .get(&conversation)
            .is_some_and(|d| d.text == text);
        if placed {
            self.shell.reply_recoveries.remove(index);
        } else {
            self.shell.reply_recoveries[index].conversation = Some(conversation);
        }
        self.say(match (recovery.certain, placed) {
            (true, true) => format!(
                "Your reply from the notification was not sent: {}. It is in the composer.",
                recovery.reason
            ),
            (false, true) => format!(
                "Your reply from the notification may not have been sent: {}. Check the history above before sending it again from the composer.",
                recovery.reason
            ),
            (true, false) => format!(
                "Your reply from the notification was not sent: {}. The composer could not take it; it is kept beside the composer to copy.",
                recovery.reason
            ),
            (false, false) => format!(
                "Your reply from the notification may not have been sent: {}. Check the history above; it is kept beside the composer to copy.",
                recovery.reason
            ),
        });
    }

    fn take_answer_reveal(&mut self) -> Option<MessageId> {
        if std::mem::take(&mut self.shell.reveal_next_question)
            && self.screen == Screen::Questions
            && self.shell.pending_notification.is_none()
        {
            let next = self
                .questions
                .iter()
                .find(|q| !q.expired(Utc::now()))
                .map(|q| (q.id.clone(), self.canonical_agent(&q.from).to_owned()));
            if let Some((_, agent)) = &next {
                self.open_thread_with(agent.clone());
            }
            next.map(|(id, _)| id)
        } else {
            None
        }
    }
    pub fn subscription(&self) -> Subscription<Message> {
        fn updates() -> impl iced::futures::Stream<Item = Message> {
            iced::stream::channel(1, notifications)
        }
        Subscription::batch([
            Subscription::run(updates),
            iced::time::every(if self.smoke.is_some() {
                Duration::from_millis(100)
            } else {
                Duration::from_secs(2)
            })
            .map(|_| Message::Tick),
            iced::event::listen_with(|event, status, _| match event {
                iced::Event::Keyboard(_) if status == iced::event::Status::Ignored => {
                    Some(Message::Event(event))
                }
                iced::Event::Window(iced::window::Event::RedrawRequested(_)) => None,
                iced::Event::Window(_) => Some(Message::Event(event)),
                _ => None,
            }),
        ])
    }

    pub fn boot() -> (Self, Task<Message>) {
        (
            Self::new(),
            window::oldest().and_then(|id| Task::done(Message::Window(id))),
        )
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        let mut tasks = Vec::new();
        if matches!(
            &message,
            Message::Draft(..)
                | Message::SessionDraft(..)
                | Message::ConversationDraft(..)
                | Message::SelectThread(_)
                | Message::SelectConversation(_)
                | Message::InboxList
                | Message::Navigate(_)
                | Message::SelectProject(_)
                | Message::SelectSession(_)
                | Message::Unassigned
                | Message::AllProjects
                | Message::OpenSession(_)
                | Message::OpenQuestion(_)
                | Message::Answer(_)
                | Message::AnswerChoice(..)
                | Message::ReviewFiles(_)
                | Message::QuestionDetails(_)
                | Message::Notification(_)
                | Message::Event(iced::Event::Keyboard(keyboard::Event::KeyPressed { .. }))
                | Message::Event(iced::Event::Mouse(iced::mouse::Event::WheelScrolled { .. }))
        ) {
            self.shell.pending_answer_reveal = None;
            self.shell.reveal_next_question = false;
        }
        if matches!(
            &message,
            Message::Navigate(_)
                | Message::SelectThread(_)
                | Message::InboxList
                | Message::SelectProject(_)
                | Message::Unassigned
                | Message::AllProjects
                | Message::OpenSession(_)
                | Message::OpenQuestion(_)
                | Message::SelectSession(_)
                | Message::Search(_)
        ) {
            self.shell.pending_notification = None;
            self.shell.notification_message = None;
            // The search for a notification's archived message, and the
            // scroll to it, are its conversation's: the person moving on
            // ends them, so a late page moves nothing.
            self.cancel_reveal();
        }
        if matches!(
            &message,
            Message::Event(iced::Event::Window(
                window::Event::Moved(_) | window::Event::Resized(_) | window::Event::Rescaled(_)
            ))
        ) && let Some(id) = self.shell.window
        {
            tasks.push(crate::accessibility::geometry(id));
        }
        match message {
            Message::Tick => {
                for activation in crate::notification_route::take() {
                    tasks.push(self.update(Message::Notification(activation)));
                }
                for action in crate::accessibility::take_actions() {
                    tasks.push(self.update(action));
                }
                self.drain();
                if let Some(id) = self.take_answer_reveal() {
                    tasks.push(crate::controls::reveal(format!(
                        "notification-question-{id}"
                    )));
                }
                // The scroll is for the Messages screen the page arrived
                // on; anywhere else the row is not on view.
                if let Some(id) = self.shell.reveal_archived_next.take()
                    && self.screen == Screen::Questions
                    && self.shell.conversation.is_some()
                {
                    tasks.push(crate::controls::reveal(format!(
                        "notification-message-{id}"
                    )));
                }
                self.schedule_update_check(chrono::Utc::now().timestamp());
                let before = self.shell.catalog.clone();
                for project in self
                    .discovered
                    .iter()
                    .filter_map(|p| p.project.clone())
                    .chain(self.agents.iter().filter_map(|a| a.project.clone()))
                    // A fixture's folder under the temporary directories is
                    // not a project the person keeps; only a pin lists it.
                    .filter(|p| !crate::catalog::is_temporary(&p.root))
                {
                    self.shell.catalog.remember(project, false);
                }
                self.shell.catalog.forget_missing();
                if before != self.shell.catalog {
                    self.shell.changed();
                }
                self.refresh_project_context();
                tasks.push(self.advance_notification());
                if let Some(smoke) = &mut self.smoke {
                    tasks.push(smoke.tick(
                        self.connected.is_ok(),
                        self.runtimes.len(),
                        &self.discovered,
                    ));
                }
            }
            Message::Notification(activation) => {
                if let Some(id) = self.shell.window {
                    crate::notification_route::unhide_application();
                    tasks.push(window::minimize(id, false).chain(window::gain_focus(id)));
                }
                match activation {
                    crate::notification_route::Activation::Open(action)
                        if action.home == self.home
                            && self
                                .client
                                .as_ref()
                                .is_some_and(|c| c.socket() == action.socket) =>
                    {
                        self.cancel_reveal();
                        self.shell.pending_notification = Some((action, Instant::now()));
                        for cmd in [Cmd::Agents, Cmd::Questions, Cmd::Inbox] {
                            self.send(cmd);
                        }
                        tasks.push(self.advance_notification());
                    }
                    crate::notification_route::Activation::Open(_) => {
                        self.say("This notification belongs to another local workspace.")
                    }
                    // A reply that did not go comes back as the words of
                    // its conversation's draft, once that conversation is
                    // open — the same route a click takes to the message.
                    crate::notification_route::Activation::ReplyFailed {
                        action,
                        text,
                        reason,
                        certain,
                    } if action.home == self.home
                        && self
                            .client
                            .as_ref()
                            .is_some_and(|c| c.socket() == action.socket) =>
                    {
                        if self.shell.reply_recoveries.len() >= REPLY_RECOVERIES {
                            let outcome = if certain {
                                "was not sent"
                            } else {
                                "may not have been sent"
                            };
                            self.say(format!(
                                "A reply from a notification {outcome} ({reason}) and the window holds as many unplaced replies as it keeps; copy or dismiss one first. You wrote: {}",
                                super::view::first_line(&text, 200)
                            ));
                        } else {
                            self.shell.reply_recoveries.push(ReplyRecovery {
                                message: action.target.message.clone(),
                                text,
                                reason,
                                certain,
                                conversation: None,
                            });
                            self.cancel_reveal();
                            self.shell.pending_notification = Some((action, Instant::now()));
                            for cmd in [Cmd::Agents, Cmd::Questions, Cmd::Inbox] {
                                self.send(cmd);
                            }
                            tasks.push(self.advance_notification());
                        }
                    }
                    crate::notification_route::Activation::ReplyFailed { reason, .. } => {
                        self.say(format!(
                            "A reply to another workspace's notification was not sent: {reason}"
                        ));
                    }
                    crate::notification_route::Activation::Focus => {}
                    crate::notification_route::Activation::Inbox => {
                        self.shell.pending_notification = None;
                        self.shell.notification_message = None;
                        self.screen = Screen::Questions;
                    }
                }
            }
            Message::Window(id) => {
                self.shell.window = Some(id);
                tasks.push(
                    crate::accessibility::install(id)
                        .chain(crate::accessibility::geometry(id))
                        .chain(window::scale_factor(id).map(Message::WindowScale)),
                );
            }
            Message::WindowScale(scale) => self.shell.dpi = scale,
            Message::NativeAccessibility(result) => {
                if let Some(smoke) = &mut self.smoke {
                    tasks.push(smoke.native_accessibility(result));
                }
            }
            Message::Navigate(screen) => {
                self.shell.pending_notification = None;
                self.shell.notification_message = None;
                self.screen = screen;
                self.shell.more = false;
                if screen == Screen::Chat {
                    self.open_project_chat();
                }
                if screen == Screen::Board {
                    self.request_tasks();
                }
                if screen == Screen::Runtimes {
                    self.send(Cmd::Runtimes);
                    self.send(Cmd::Connector);
                }
                if screen == Screen::Desktop {
                    self.send(Cmd::Desktop(self.desktop.command("status")));
                }
            }
            Message::RetryProject => self.shell.checked_project = None,
            Message::ToggleNeedsYou => {
                self.shell.needs_you_expanded = !self.shell.needs_you_expanded;
            }
            Message::OpenQuestion(id) => {
                self.screen = Screen::Questions;
                if let Some(question) = self
                    .questions
                    .iter()
                    .find(|q| q.id == id && !q.expired(Utc::now()))
                {
                    let agent = self.canonical_agent(&question.from).to_owned();
                    self.open_thread_with(agent);
                    self.shell.message_detail = Some(id.clone());
                    tasks.push(crate::controls::reveal(format!(
                        "notification-question-{id}"
                    )));
                } else {
                    self.say("This question is no longer waiting for an answer.");
                }
            }
            Message::AllProjects => {
                self.shell.catalog.unassigned = false;
                self.shell.catalog.selected = None;
                self.shell.selected = None;
                self.shell.search.clear();
                self.reset_session_view();
                self.screen = Screen::Agents;
                self.shell.changed();
                self.refresh_project_context();
            }
            Message::Unassigned => {
                self.shell.catalog.unassigned = true;
                self.shell.catalog.selected = None;
                self.shell.selected = None;
                self.shell.search.clear();
                self.reset_session_view();
                self.screen = Screen::Agents;
                self.shell.changed();
                self.refresh_project_context();
            }
            Message::SelectProject(path) => {
                if self
                    .shell
                    .catalog
                    .projects
                    .iter()
                    .any(|e| e.project.root == path)
                {
                    self.shell.catalog.unassigned = false;
                    self.shell.viewed_project(&self.agents, &path);
                    self.shell.catalog.selected = Some(path);
                    self.shell.selected = None;
                    self.shell.search.clear();
                    self.reset_session_view();
                    self.shell.changed();
                    self.refresh_project_context();
                    self.open_project_chat();
                }
            }
            Message::OpenSession(id) => {
                let Some(agent) = self.agents.iter().find(|a| a.id.as_str() == id) else {
                    self.say("This session is no longer available.");
                    return Task::none();
                };
                let project = agent.project.clone();
                let root = project.as_ref().map(|p| p.root.clone());
                if let Some(project) = project.filter(|p| !crate::catalog::is_temporary(&p.root)) {
                    self.shell.catalog.remember(project, false);
                    if !self
                        .shell
                        .catalog
                        .projects
                        .iter()
                        .any(|entry| Some(&entry.project.root) == root.as_ref())
                    {
                        self.say("The project list is full. Forget an old project before opening this session.");
                        return Task::none();
                    }
                }
                match root {
                    Some(root) => {
                        tasks.push(self.update(Message::SelectProject(root)));
                    }
                    None => {
                        tasks.push(self.update(Message::Unassigned));
                    }
                }
                self.screen = Screen::Agents;
                tasks.push(self.update(Message::SelectSession(id)));
            }
            Message::SelectSession(id) => {
                self.shell.unviewed_done.remove(&id);
                self.shell.selected = Some(id);
                self.shell.session_details = false;
                self.shell.review_delivery = false;
                self.shell.session_message = false;
            }
            Message::ComposeSession => {
                self.shell.session_message = !self.shell.session_message;
                let selected = self.shell.selected.as_deref();
                self.shell.session_drafts.retain(|id, entry| {
                    selected == Some(id.as_str())
                        || !entry.draft.text.is_empty()
                        || entry.draft.sending.is_some()
                });
            }
            Message::SessionDraft(id, text) => {
                self.shell.edit_draft(DraftKind::Session, id, text);
            }
            Message::SelectConversation(id) => {
                if self.shell.conversation.as_deref() != Some(id.as_str()) {
                    self.shell.thread = None;
                    self.thread = None;
                    self.cancel_reveal();
                }
                self.shell.conversation = Some(id.clone());
                self.shell.inbox_open = true;
                if self.connected.is_ok() {
                    self.send(Cmd::History(id, self.history_epoch));
                }
            }
            Message::ReplyRecoveryCopy(message) => {
                if let Some(recovery) = self
                    .shell
                    .reply_recoveries
                    .iter()
                    .find(|r| r.message == message)
                {
                    tasks.push(iced::clipboard::write(recovery.text.clone()));
                    self.say("Your reply is on the clipboard.");
                }
            }
            Message::ReplyRecoveryDismiss(message) => {
                self.shell.reply_recoveries.retain(|r| r.message != message);
            }
            Message::ConversationDraft(id, text) => {
                self.shell.edit_draft(DraftKind::Conversation, id, text);
            }
            Message::SendConversation(key) => {
                // The key says where the words were typed: the conversation's
                // own composer, or a thread's, which alone sets `reply_to`.
                let (conversation, reply_to) = super::split_draft_key(&key);
                let conversation = conversation.to_owned();
                let to = self.conversation_destination(&conversation);
                if self.connected.is_ok()
                    && self.conversation_can_send(&conversation)
                    && let Some(to) = to
                    && let Some(draft) = self.shell.conversation_drafts.get_mut(&key)
                    && let Some(text) = draft.begin()
                {
                    self.send(Cmd::ConversationSend {
                        draft: key,
                        to,
                        text,
                        reply_to,
                    });
                }
            }
            Message::EarlierHistory(conversation) => {
                if self.connected.is_ok()
                    && let Some(first) = self
                        .history
                        .get(&conversation)
                        .and_then(|messages| messages.first())
                {
                    self.send(Cmd::HistoryBefore(
                        conversation.clone(),
                        first.seq,
                        self.history_epoch,
                    ));
                }
            }
            Message::ExpandArchived(id) => {
                let archived = self.history.values().flatten().any(|m| m.envelope.id == id)
                    || self.thread.as_ref().is_some_and(|(root, replies)| {
                        root.envelope.id == id || replies.iter().any(|m| m.envelope.id == id)
                    });
                if archived {
                    self.shell.message_detail =
                        (self.shell.message_detail.as_ref() != Some(&id)).then_some(id);
                }
            }
            Message::OpenThread(root) => {
                self.shell.thread = Some(root.clone());
                self.thread = None;
                if self.connected.is_ok() {
                    self.send(Cmd::Thread(root, self.history_epoch));
                }
            }
            Message::CloseThread => {
                self.shell.thread = None;
                self.thread = None;
            }
            Message::MessagesSearch(text) => {
                self.shell.messages_search = text.chars().take(200).collect();
            }
            Message::ToggleCollisions => self.shell.collisions_open = !self.shell.collisions_open,
            Message::ToggleEarlier => {
                self.shell.earlier_open = !self.shell.earlier_open;
                self.shell.earlier_shown = EARLIER_PAGE;
            }
            Message::MoreEarlier => {
                self.shell.earlier_shown = self
                    .shell
                    .earlier_shown
                    .max(EARLIER_PAGE)
                    .saturating_add(EARLIER_PAGE);
            }
            Message::ToggleTemporary => {
                let open = self.temporary_fold_open();
                self.shell.temporary_open = Some(!open);
            }
            Message::TogglePeers => self.shell.peers_open = !self.shell.peers_open,
            Message::PaneResized(grid, event) => {
                if self.panes.resized(grid, event) {
                    self.shell.catalog.panes = self.panes.widths;
                    self.shell.changed();
                }
            }
            Message::PauseStart(project) => {
                if self.pause_states.len() >= PAUSE_CONTROLS
                    && !self.pause_states.contains_key(&project)
                {
                    self.say("Finish or cancel an existing pause draft first.");
                } else {
                    let control = self.pause_states.entry(project).or_default();
                    if control.pending.is_none() {
                        control.draft.get_or_insert_with(String::new);
                        control.error = None;
                    }
                }
            }
            Message::PauseDraft(project, reason) => {
                if let Some(control) = self.pause_states.get_mut(&project)
                    && control.pending.is_none()
                    && control.draft.is_some()
                {
                    if reason.chars().count() > 400 {
                        control.error = Some("A pause reason is at most 400 characters.".into());
                    } else {
                        // Copy only the accepted text, not a paste buffer's capacity.
                        control.draft = Some(reason.as_str().to_owned());
                        control.error = None;
                    }
                }
            }
            Message::PauseCancel(project) => {
                if self
                    .pause_states
                    .get(&project)
                    .is_some_and(|control| control.pending.is_none())
                {
                    self.pause_states.remove(&project);
                }
            }
            Message::PauseSubmit(project) => self.submit_pause(project, PauseAction::Pause),
            Message::ResumeProject(project) => self.submit_pause(project, PauseAction::Resume),
            Message::NewConversation => {
                self.new_conversation = match self.new_conversation {
                    Some(_) => None,
                    None => Some(super::NewConversation::new()),
                };
            }
            Message::InviteChannel(channel) => {
                let mut form = super::NewConversation::new();
                form.invite = Some(channel);
                self.new_conversation = Some(form);
            }
            Message::InviteMember(member) => {
                if let Some(form) = &mut self.new_conversation
                    && !form.creating
                    && let Some(channel) = &form.invite
                {
                    form.creating = true;
                    form.error = None;
                    let cmd = Cmd::ChannelInvite {
                        request: form.request.clone(),
                        channel: channel.clone(),
                        member,
                    };
                    self.send(cmd);
                }
            }
            Message::NewConversationKind(kind) => {
                if let Some(form) = &mut self.new_conversation {
                    form.kind = kind;
                    form.error = None;
                }
            }
            Message::NewChannelName(name) => {
                if let Some(form) = &mut self.new_conversation {
                    // What a channel name is: lowercase letters, digits and
                    // hyphens, so what is typed is kept to that.
                    form.name = name
                        .to_lowercase()
                        .chars()
                        .map(|ch| if ch.is_whitespace() { '-' } else { ch })
                        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '-')
                        .take(40)
                        .collect();
                    form.error = None;
                }
            }
            Message::NewChannelPurpose(purpose) => {
                if let Some(form) = &mut self.new_conversation {
                    form.purpose = purpose.chars().take(400).collect();
                    form.error = None;
                }
            }
            Message::NewChannelMember(agent) => {
                if let Some(form) = &mut self.new_conversation
                    && !form.members.remove(&agent)
                {
                    form.members.insert(agent);
                }
            }
            Message::NewDirect(conversation) => {
                self.new_conversation = None;
                return self.update(Message::SelectConversation(conversation));
            }
            Message::CreateChannel => {
                let project = self
                    .shell
                    .catalog
                    .selected
                    .as_ref()
                    .map(|root| root.display().to_string());
                if let Some(form) = &mut self.new_conversation
                    && !form.creating
                    && !form.name.is_empty()
                {
                    form.creating = true;
                    form.error = None;
                    let task = if form.purpose.trim().is_empty() {
                        form.name.replace('-', " ")
                    } else {
                        form.purpose.trim().to_owned()
                    };
                    let cmd = Cmd::ChannelOpen {
                        request: form.request.clone(),
                        name: form.name.clone(),
                        task,
                        members: form.members.iter().map(|id| id.to_string()).collect(),
                        project,
                    };
                    self.send(cmd);
                }
            }
            Message::TaskTitle(title) => {
                if let Some(project) = self.selected_project_root()
                    && !self
                        .shell
                        .task_drafts
                        .get(&project)
                        .is_some_and(TaskDraft::sending)
                {
                    self.shell.edit_draft(DraftKind::TaskTitle, project, title);
                }
            }
            Message::TaskAcceptance(acceptance) => {
                if let Some(project) = self.selected_project_root()
                    && !self
                        .shell
                        .task_drafts
                        .get(&project)
                        .is_some_and(TaskDraft::sending)
                {
                    self.shell
                        .edit_draft(DraftKind::TaskAcceptance, project, acceptance);
                }
            }
            Message::TaskFile(column) => {
                if let Some(project) = self.selected_project_root()
                    && let Some(draft) = self.shell.task_drafts.get_mut(&project)
                    && !draft.sending()
                    && !draft.title.trim().is_empty()
                {
                    self.task_requests += 1;
                    let request = self.task_requests;
                    draft.sending = Some(request);
                    draft.error = None;
                    let cmd = Cmd::TaskCreate {
                        project,
                        request,
                        title: draft.title.trim().to_owned(),
                        acceptance: draft.acceptance.trim().to_owned(),
                        column,
                    };
                    self.send(cmd);
                }
            }
            Message::TaskMove(task, column) => self.send(Cmd::TaskMove { task, column }),
            Message::TaskOpen(task) => {
                self.task_open = if self.task_open.as_ref() == Some(&task) {
                    None
                } else {
                    Some(task)
                };
            }
            Message::TaskAssign(task, assignee) => self.send(Cmd::TaskAssign { task, assignee }),
            Message::TaskArchive(task) => self.send(Cmd::TaskArchive(task)),
            Message::TasksMore => self.request_more_tasks(),
            Message::MarkAllRead => {
                if self.connected.is_ok() {
                    let heads: Vec<(String, u64)> = self
                        .conversations
                        .iter()
                        .filter(|s| s.unread > 0 && self.counts_for_person(s))
                        .filter_map(|s| {
                            s.last_seq
                                .map(|seq| (s.conversation.as_str().to_owned(), seq))
                        })
                        .collect();
                    for (conversation, seq) in heads {
                        self.send(Cmd::MarkRead(conversation, seq));
                    }
                }
            }
            Message::ProjectMenu(path) => {
                let open = self.shell.project_menu.as_ref() == Some(&path);
                self.shell.project_menu = (!open).then_some(path);
                self.shell.project_rename = None;
            }
            Message::ProjectRenameStart(path) => {
                let name = self
                    .shell
                    .catalog
                    .projects
                    .iter()
                    .find(|e| e.project.root == path)
                    .map(|e| e.name())
                    .unwrap_or_default();
                self.shell.project_rename = Some((path, name));
            }
            Message::ProjectRenameDraft(text) => {
                if let Some((_, draft)) = self.shell.project_rename.as_mut() {
                    *draft = text.chars().take(80).collect();
                }
            }
            Message::ProjectRenameSubmit => {
                if let Some((path, name)) = self.shell.project_rename.take()
                    && self.shell.catalog.rename(&path, &name)
                {
                    self.shell.changed();
                }
                self.shell.project_menu = None;
            }
            Message::ProjectPin(path) => {
                if let Some(entry) = self
                    .shell
                    .catalog
                    .projects
                    .iter_mut()
                    .find(|e| e.project.root == path)
                {
                    entry.pinned = !entry.pinned;
                    self.shell.changed();
                }
                self.shell.project_menu = None;
            }
            Message::ProjectRemove(path) => {
                let was_selected = self.shell.catalog.selected.as_ref() == Some(&path);
                match self.shell.catalog.remove(&path) {
                    Ok(true) => {
                        if was_selected {
                            self.shell.selected = None;
                            self.reset_session_view();
                            self.refresh_project_context();
                        }
                        self.shell.changed();
                    }
                    Ok(false) => {}
                    // At the bound the project stays on the list and the
                    // person is told why.
                    Err(error) => self.say(error.to_string()),
                }
                self.shell.project_menu = None;
                self.shell.project_rename = None;
            }
            Message::SendSession(id) => {
                if self.connected.is_ok()
                    && self.agents.iter().any(|a| {
                        a.id.as_str() == self.canonical_agent(&id)
                            && a.status.is_live()
                            && a.spec.runtime != "human"
                    })
                    && let Some(entry) = self.shell.session_drafts.get_mut(&id)
                    && let Some(text) = entry.draft.begin()
                {
                    entry.queued = None;
                    self.send(Cmd::SessionSend(id, text));
                }
            }
            Message::CloseSession => self.shell.selected = None,
            Message::SessionFilter(filter) => {
                self.shell.session_filter = filter;
                self.shell.selected = None;
                self.confirm_stop = None;
            }
            Message::More => self.shell.more = !self.shell.more,
            Message::ResumeProvider(agent, blocked_at) => {
                if self.connected.is_ok() {
                    self.send(Cmd::ResumeProvider(agent, blocked_at));
                }
            }
            Message::RetryController(agent) => {
                if self.connected.is_ok() {
                    self.send(Cmd::RetryController(agent));
                }
            }
            Message::ReviewDelivery => {
                if self.connected.is_ok()
                    && let Some(id) = self.shell.selected.clone()
                    && self.agents.iter().any(|agent| {
                        agent.id.as_str() == id
                            && agent
                                .input_delivery
                                .as_ref()
                                .is_some_and(|d| d.paused_for(agent.process_started_at))
                    })
                {
                    self.shell.review_delivery = !self.shell.review_delivery;
                    if self.shell.review_delivery {
                        self.session_log = None;
                        self.send(Cmd::SessionLog(id));
                    }
                }
            }
            Message::SessionDetails => self.shell.session_details = !self.shell.session_details,
            Message::SelectThread(agent) => {
                self.shell.inbox_thread = agent;
                self.shell.inbox_open = true;
                self.shell.message_detail = None;
            }
            Message::InboxList => {
                self.shell.inbox_open = false;
                self.shell.message_detail = None;
            }
            Message::SendProject(id) => {
                let project = self
                    .agents
                    .iter()
                    .find(|a| a.id.as_str() == self.canonical_agent(&id))
                    .and_then(|a| a.project.as_ref().map(|p| p.id().to_string()));
                if self.connected.is_ok()
                    && let Some(project) = project
                    && let Some(entry) = self.shell.session_drafts.get_mut(&id)
                    && let Some(text) = entry.draft.begin()
                {
                    entry.queued = None;
                    self.send(Cmd::ProjectSend(id, project, text));
                }
            }
            Message::QuestionDetails(id) => {
                if self.inbox.iter().any(|message| message.id == id) {
                    self.shell.message_detail =
                        (self.shell.message_detail.as_ref() != Some(&id)).then_some(id);
                }
            }
            Message::ReviewFiles(id) => {
                if self.questions.iter().any(|q| {
                    q.id == id
                        && matches!(
                            q.presentation,
                            Some(agentdocker_core::QuestionPresentation::CodexFiles { .. })
                        )
                }) {
                    self.shell.file_review =
                        (self.shell.file_review.as_ref() != Some(&id)).then_some(id);
                }
            }
            Message::ConnectionDetails(name) => {
                self.shell.connection_details =
                    (self.shell.connection_details.as_ref() != Some(&name)).then_some(name);
            }
            Message::OpenConnection(name) => {
                self.screen = Screen::Runtimes;
                self.shell.connection_details = Some(name);
                self.shell.other_tools = true;
                self.send(Cmd::Runtimes);
                self.send(Cmd::Connector);
            }
            Message::OtherTools => self.shell.other_tools = !self.shell.other_tools,
            Message::Search(text) => {
                self.shell.search = text.chars().take(1024).collect();
                self.shell.selected = None;
            }
            Message::ShowAdd => {
                self.shell.adding = !self.shell.adding;
            }
            Message::AddPath(path) => self.shell.add_path = path.chars().take(4096).collect(),
            Message::PickFolder => {
                tasks.push(Task::perform(
                    async {
                        rfd::AsyncFileDialog::new()
                            .set_title("Add project")
                            .pick_folder()
                            .await
                            .map(|f| f.path().to_owned())
                    },
                    Message::FolderPicked,
                ));
            }
            Message::FolderPicked(Some(path)) => tasks.push(resolve_folder(path)),
            Message::FolderPicked(None) => {}
            Message::ResolveFolder => {
                let path = self.shell.add_path.trim();
                let path = if let Some(rest) = path.strip_prefix("~/") {
                    std::env::var_os("HOME")
                        .map(PathBuf::from)
                        .unwrap_or_default()
                        .join(rest)
                } else {
                    PathBuf::from(path)
                };
                tasks.push(resolve_folder(path));
            }
            Message::FolderResolved(result) => match result {
                Ok(project) => {
                    if let Err(error) = self.shell.catalog.pin(project) {
                        self.shell.error = Some(error.to_string());
                    } else {
                        self.shell.adding = false;
                        self.shell.add_path.clear();
                        self.shell.selected = None;
                        self.reset_session_view();
                        self.open_project_chat();
                        self.shell.changed();
                        self.refresh_project_context();
                    }
                }
                Err(error) => self.shell.error = Some(error),
            },
            Message::ProjectLocation(path, available) => {
                if self.shell.catalog.selected.as_ref() == Some(&path) {
                    self.shell.project_available = Some(available);
                }
            }
            Message::Unpin => {
                let selected = self.shell.catalog.selected.clone();
                if let Some(entry) = self
                    .shell
                    .catalog
                    .projects
                    .iter_mut()
                    .find(|e| Some(&e.project.root) == selected.as_ref())
                {
                    entry.pinned = !entry.pinned;
                    self.shell.changed();
                }
            }
            Message::ForgetProject => {
                // Off the list, and kept off: discovery brought a forgotten
                // folder straight back before.
                if let Some(selected) = self.shell.catalog.selected.clone() {
                    match self.shell.catalog.remove(&selected) {
                        Ok(_) => {
                            self.shell.selected = None;
                            self.reset_session_view();
                            self.shell.changed();
                            self.refresh_project_context();
                        }
                        Err(error) => self.say(error.to_string()),
                    }
                }
            }
            Message::CopyGuidance(text) => return iced::clipboard::write(text),
            Message::DeliveryDetails(target) => {
                let draft = match target {
                    DeliveryTarget::Conversation(key) => {
                        self.shell.conversation_drafts.get_mut(&key)
                    }
                    DeliveryTarget::Session(key) => self
                        .shell
                        .session_drafts
                        .get_mut(&key)
                        .map(|entry| &mut entry.draft),
                    DeliveryTarget::Channel(key) => self.shell.channel_drafts.get_mut(&key),
                };
                if let Some(draft) = draft {
                    draft.readiness_expanded = !draft.readiness_expanded;
                }
            }
            Message::DraftsSaved(generation, result) => {
                self.shell.drafts.complete(generation, result);
            }
            Message::RetryDraftSave => {
                if self.shell.drafts.readable {
                    self.shell.drafts.error = None;
                    self.shell.drafts.close_blocked = false;
                }
            }
            Message::CloseWithoutDraftSave => {
                if self.shell.drafts.close_blocked && self.shell.drafts.error.is_some() {
                    self.shell.drafts.discard_on_close = true;
                    self.shell.closing = true;
                }
            }
            Message::CatalogSaved(generation, result) => {
                self.shell.saving = false;
                match result {
                    Ok(()) => self.shell.saved_generation = generation,
                    Err(error) => {
                        self.shell.error = Some(format!("Could not save workspace: {error}"));
                        self.shell.save_enabled = false;
                    }
                }
            }
            Message::Draft(id, value) => {
                if !self.sending.contains(&id) {
                    self.shell
                        .edit_draft(DraftKind::Answer, id.to_string(), value);
                }
            }
            Message::AnswerChoice(id, value) => {
                if self.connected.is_ok()
                    && !self.sending.contains(&id)
                    && self.questions.iter().any(|q| {
                        q.id == id
                            && !q.expired(Utc::now())
                            && (!matches!(
                                q.presentation,
                                Some(agentdocker_core::QuestionPresentation::CodexFiles { .. })
                            ) || !value.trim().eq_ignore_ascii_case("allow")
                                || self.shell.file_review.as_ref() == Some(&id))
                            && q.presentation
                                .as_ref()
                                .is_some_and(|p| p.valid_for(&q.text) && p.permits_choice(&value))
                    })
                {
                    // A choice is an explicit submission, not a draft edit.
                    // Full draft storage must not block Allow/Deny, nor may
                    // clicking a choice overwrite earlier typed text on failure.
                    self.shell.answer_errors.remove(&id);
                    self.sending.insert(id.clone());
                    self.shell.pending_answer_reveal = Some(id.clone());
                    self.send(Cmd::Answer(id, value));
                }
            }
            Message::Answer(id) => {
                if self.connected.is_ok()
                    && !self.sending.contains(&id)
                    && self.questions.iter().any(|q| {
                        q.id == id
                            && !q.expired(Utc::now())
                            && (!matches!(
                                q.presentation,
                                Some(agentdocker_core::QuestionPresentation::CodexFiles { .. })
                            ) || self
                                .shell
                                .answers
                                .get(&id)
                                .is_none_or(|answer| !answer.trim().eq_ignore_ascii_case("allow"))
                                || self.shell.file_review.as_ref() == Some(&id))
                    })
                    && let Some(answer) = self
                        .shell
                        .answers
                        .get(&id)
                        .filter(|s| !s.trim().is_empty())
                        .cloned()
                {
                    self.shell.answer_errors.remove(&id);
                    self.sending.insert(id.clone());
                    self.shell.pending_answer_reveal = Some(id.clone());
                    self.send(Cmd::Answer(id, answer));
                }
            }
            Message::DismissInbox(mut ids) => {
                if self.connected.is_ok() && ids.len() <= 1000 {
                    ids.retain(|id| {
                        !self.dismissing.contains(id)
                            && self.inbox.iter().any(|message| &message.id == id)
                            && !self.questions.iter().any(|question| &question.id == id)
                    });
                    ids.sort();
                    ids.dedup();
                    if !ids.is_empty() {
                        self.dismissing.extend(ids.iter().cloned());
                        self.send(Cmd::DismissMessages(ids));
                    }
                }
            }
            Message::AdoptAll => {
                if self.connected.is_ok() {
                    self.send(Cmd::AdoptAll);
                }
            }
            Message::Adopt(pid) => {
                if self.connected.is_ok() && self.discovered.iter().any(|p| p.pid == pid) {
                    self.send(Cmd::Adopt(pid));
                }
            }
            Message::Stop(id) => {
                if self.connected.is_ok()
                    && self
                        .agents
                        .iter()
                        .any(|a| a.id.as_str() == id && a.status.is_live())
                {
                    if self
                        .confirm_stop
                        .as_ref()
                        .is_some_and(|(armed, at)| armed == &id && at.elapsed() < CONFIRM_WITHIN)
                    {
                        self.confirm_stop = None;
                        self.send(Cmd::Stop(id));
                    } else {
                        self.confirm_stop = Some((id, Instant::now()));
                    }
                }
            }
            Message::ShowLaunch => {
                self.shell.launch = !self.shell.launch;
                self.shell.launch_channel = matches!(
                    self.shell.launch_runtime.as_deref(),
                    Some("claude-code" | "codex")
                );
                if self.shell.launch {
                    self.shell.selected = None;
                    self.shell.session_filter = super::sessions::Filter::Current;
                }
            }
            Message::LaunchRuntime(runtime) => {
                // A normal launch connects the supported input adapter. The
                // provider still asks for its own channel/tool consent.
                self.shell.launch_channel = matches!(runtime.as_str(), "claude-code" | "codex");
                self.shell.launch_runtime = Some(runtime);
            }
            Message::LaunchName(name) => self.shell.launch_name = name.chars().take(120).collect(),
            Message::LaunchArguments(args) => {
                self.shell.launch_arguments = args.chars().take(8192).collect()
            }
            Message::LaunchChannel(on) => self.shell.launch_channel = on,
            Message::Launch => {
                if self.connected.is_ok() && !self.shell.launching {
                    match self
                        .launch_spec()
                        .and_then(|spec| self.prepare_launch(spec, sibling_cli()))
                    {
                        Ok(spec) => {
                            self.shell.launching = true;
                            self.shell.error = None;
                            self.send(Cmd::Launch(Box::new(spec)));
                        }
                        Err(error) => self.shell.error = Some(error),
                    }
                }
            }
            Message::Reconnect(id) => self.reconnect(&id, sibling_cli()),
            Message::Attach(id) => {
                if self
                    .agents
                    .iter()
                    .any(|a| a.id.as_str() == id && a.managed && a.spec.tty && a.status.is_live())
                {
                    self.attach(id, self.wake.clone());
                    self.screen = Screen::Terminal;
                    tasks.push(iced::widget::operation::focus(
                        iced::advanced::widget::Id::new("terminal"),
                    ));
                }
            }
            Message::Detach => {
                self.terminal = None;
                self.screen = Screen::Agents;
            }
            Message::OpenProjectTerminal => {
                if !self.shell.terminal_opening
                    && let Some(path) = self.shell.catalog.selected.as_ref()
                {
                    let path = path.clone();
                    self.shell.terminal_opening = true;
                    self.say("Opening terminal…");
                    tasks.push(open_native_terminal(
                        crate::native_terminal::Request::Project(path),
                    ));
                }
            }
            Message::OpenAgentTerminal(id) => {
                if let Some(agent) = self
                    .agents
                    .iter()
                    .find(|a| a.id.as_str() == id && a.status.is_live())
                {
                    if agent.managed && agent.spec.tty {
                        tasks.push(self.update(Message::Attach(id)));
                    } else if !self.shell.terminal_opening {
                        if let (Some(pid), Some(started_at)) = (agent.pid, agent.process_started_at)
                        {
                            self.shell.terminal_opening = true;
                            self.say("Opening terminal…");
                            tasks.push(open_native_terminal(
                                crate::native_terminal::Request::Agent { pid, started_at },
                            ));
                        } else {
                            self.shell.error = Some("This agent has not reported its terminal process. Open the app where it started.".into());
                        }
                    }
                }
            }
            Message::NativeTerminalOpened(result) => {
                self.shell.terminal_opening = false;
                self.status.clear();
                match result {
                    Ok(()) => self.say("Terminal opened"),
                    Err(error) => self.shell.error = Some(error),
                }
            }
            Message::TerminalInput(bytes) => {
                if let Some(terminal) = &mut self.terminal {
                    if bytes == [29] {
                        self.terminal = None;
                        self.screen = Screen::Agents;
                    } else {
                        terminal.send(bytes);
                    }
                }
            }
            Message::TerminalResize(cols, rows) => {
                if let Some(terminal) = &mut self.terminal {
                    terminal.resize(cols, rows);
                }
            }
            Message::TerminalScroll(rows) => {
                if let Some(terminal) = &mut self.terminal {
                    terminal.scroll(rows);
                }
            }
            Message::TerminalDismiss => {
                if let Some(terminal) = &mut self.terminal {
                    terminal.input_notice = None;
                }
            }
            Message::ConsoleInput(text) => self.console_input = text.chars().take(16_000).collect(),
            Message::RunConsole => {
                let line = self.console_input.trim().to_owned();
                if !line.is_empty() {
                    self.remember_console_command(&line);
                    self.append_console(&format!("$ {line}\n"));
                    self.console_running += 1;
                    self.send(Cmd::Console(line, self.shell.catalog.selected.clone()));
                }
            }
            Message::Recall(back) => self.recall(back),
            Message::ChannelTarget(id) => {
                self.shell
                    .channel_drafts
                    .retain(|_, draft| !draft.text.is_empty() || draft.sending.is_some());
                if self
                    .channels
                    .iter()
                    .any(|c| c.id.as_str() == id && c.is_open())
                {
                    self.shell.channel_target = Some(id);
                }
            }
            Message::ChannelDraft(text) => {
                if let Some(id) = self.shell.channel_target.clone() {
                    self.shell.edit_draft(DraftKind::Channel, id, text);
                }
            }
            Message::SendChannel => {
                if self.connected.is_ok()
                    && let Some(id) = self.shell.channel_target.clone()
                    && self
                        .channels
                        .iter()
                        .any(|c| c.id.as_str() == id && c.is_open())
                    && let Some(text) = self
                        .shell
                        .channel_drafts
                        .entry(id.clone())
                        .or_default()
                        .begin()
                {
                    self.send(Cmd::ChannelSend(id, text));
                }
            }
            Message::Setup(args) => {
                if !self.setup_busy {
                    self.screen = Screen::Runtimes;
                    self.shell.setup_error = None;
                    self.setup_busy = true;
                    self.send(Cmd::Setup(args));
                }
            }
            Message::SetupClose => {
                self.setup_plan = None;
                self.setup_health = None;
            }
            Message::DesktopSource(value) => {
                if !self.desktop.busy {
                    self.desktop.source = value;
                    self.desktop.report = None;
                }
            }
            Message::DesktopPrefix(value) => {
                if !self.desktop.busy {
                    self.desktop.prefix = value;
                    self.desktop.report = None;
                    self.desktop.installed = None;
                    self.desktop.update = None;
                    self.desktop.update_check_error = false;
                }
            }
            Message::DesktopLocal(value) => {
                if !self.desktop.busy {
                    self.desktop.local_preview = value;
                    self.desktop.report = None;
                }
            }
            Message::DesktopUseCurrent => {
                if let Ok(exe) = agentdocker_host::procinfo::executable_path()
                    && let Some(parent) = exe.parent().and_then(|p| p.parent())
                {
                    self.desktop.source = if cfg!(target_os = "macos") {
                        parent.parent().unwrap_or(parent)
                    } else {
                        parent
                    }
                    .display()
                    .to_string();
                    self.desktop.report = None;
                }
            }
            Message::DesktopPreview(operation) => {
                if !self.desktop.busy
                    && let Some(args) = self.desktop.preview(&operation)
                {
                    if operation == "update-check" && self.desktop.prefix.trim().is_empty() {
                        self.shell.pending_update = None;
                        self.shell.catalog.updates.last_attempt =
                            Some(chrono::Utc::now().timestamp());
                        self.shell.changed();
                    }
                    self.desktop.busy = true;
                    self.send(Cmd::Desktop(args));
                }
            }
            Message::DesktopApply => {
                if !self.desktop.busy
                    && let Some(args) = self.desktop.apply()
                {
                    self.desktop.busy = true;
                    self.send(Cmd::Desktop(args));
                }
            }
            Message::AutomaticUpdates(enabled) => {
                self.shell.catalog.updates.enabled = enabled;
                self.shell.changed();
            }
            Message::Dark(dark) => {
                self.shell.catalog.dark = dark;
                self.shell.changed();
            }
            Message::TextSize(size) => {
                self.settings.text_size = size;
                self.settings = self.settings.clone().clamped();
                self.shell.catalog.appearance = Some(self.settings.clone());
                self.shell.changed();
            }
            Message::TerminalSize(size) => {
                self.settings.terminal_size = size;
                self.settings = self.settings.clone().clamped();
                self.shell.catalog.appearance = Some(self.settings.clone());
                self.shell.changed();
            }
            Message::Palette(palette) => {
                self.settings.palette = palette;
                self.settings = self.settings.clone().clamped();
                self.shell.catalog.appearance = Some(self.settings.clone());
                self.shell.changed();
            }
            Message::Roomy(roomy) => {
                self.settings.roomy = roomy;
                self.shell.catalog.appearance = Some(self.settings.clone());
                self.shell.changed();
            }
            Message::DismissError => {
                self.shell.error = None;
                self.status.clear();
            }
            Message::Event(iced::Event::Window(window::Event::Rescaled(scale))) => {
                self.shell.dpi = scale
            }
            Message::Event(iced::Event::Window(window::Event::Focused)) => {
                // Construction precedes the hidden window's first activation.
                // Request OS permission only once an actual window has focus.
                crate::notify::request_permission();
                self.shell.unfocused = false;
                if let Some(id) = self.shell.window {
                    tasks.push(crate::accessibility::focus(id, true));
                }
            }
            Message::Event(iced::Event::Window(window::Event::Unfocused)) => {
                self.shell.unfocused = true;
                if let Some(id) = self.shell.window {
                    tasks.push(crate::accessibility::focus(id, false));
                }
            }
            Message::Event(iced::Event::Window(window::Event::CloseRequested)) => {
                self.shell.closing = true
            }
            Message::Event(iced::Event::Window(window::Event::Resized(size))) => {
                self.shell.width = size.width;
                self.shell.height = size.height;
            }
            Message::Event(iced::Event::Keyboard(keyboard::Event::KeyPressed {
                key,
                modifiers,
                ..
            })) => {
                use keyboard::{Key, key::Named};
                match key {
                    Key::Named(Named::Tab | Named::F6) => tasks.push(
                        (if modifiers.shift() {
                            iced::widget::operation::focus_previous()
                        } else {
                            iced::widget::operation::focus_next()
                        })
                        .chain(crate::controls::reveal_focus())
                        .chain(crate::accessibility::collect()),
                    ),
                    Key::Named(Named::Escape) => {
                        self.shell.pending_notification = None;
                        self.shell.notification_message = None;
                        self.shell.selected = None;
                        self.shell.launch = false;
                        self.shell.adding = false;
                        self.shell.more = false;
                    }
                    Key::Character(key) if modifiers.command() => {
                        let screen = match key.as_str() {
                            "1" => Some(Screen::Agents),
                            "2" => Some(Screen::Questions),
                            "3" => Some(Screen::Runtimes),
                            "4" => Some(Screen::Settings),
                            _ => None,
                        };
                        if let Some(screen) = screen {
                            tasks.push(self.update(Message::Navigate(screen)));
                        }
                    }
                    _ => {}
                }
            }
            Message::Event(_) => {}
            Message::Captured(capture) => {
                if let Some(smoke) = &mut self.smoke {
                    tasks.push(smoke.captured(capture));
                }
            }
            Message::Focus(id) => tasks.push(
                iced::widget::operation::focus(iced::advanced::widget::Id::from(id))
                    .chain(crate::controls::reveal_focus())
                    .chain(crate::accessibility::collect()),
            ),
            Message::Accessibility(snapshot) => {
                if let Some(scenario) = self.smoke.as_mut().and_then(|s| s.scenario.as_mut()) {
                    scenario.snapshot = snapshot.clone();
                }
                if let Some(id) = self.shell.window {
                    tasks.push(crate::accessibility::update(
                        id,
                        snapshot,
                        f64::from(self.scale_factor() * self.shell.dpi.max(1.0)),
                    ));
                }
                return Task::batch(tasks);
            }
        }
        // Catalog removal and missing-folder cleanup can choose another
        // project too. Its header must never accompany the previous queue.
        if self.screen == Screen::Chat
            && self.shell.conversation
                != self
                    .selected_project_id()
                    .map(|id| format!("everyone:{id}"))
        {
            self.open_project_chat();
        }
        if self.shell.catalog.selected != self.shell.checked_project {
            self.shell.checked_project = self.shell.catalog.selected.clone();
            self.shell.project_available = None;
            if let Some(path) = self.shell.checked_project.clone() {
                tasks.push(Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || {
                            let available = path.is_dir();
                            (path, available)
                        })
                        .await
                        .unwrap_or_default()
                    },
                    |(path, available)| Message::ProjectLocation(path, available),
                ));
            }
        }
        // A failed draft write must not turn a normal close into silent loss.
        if self.shell.closing
            && !self.shell.drafts.clean()
            && !self.shell.drafts.discard_on_close
            && (!self.shell.drafts.readable || self.shell.drafts.error.is_some())
        {
            self.shell.drafts.close_blocked = true;
            self.shell.closing = false;
        }
        if self.shell.closing
            && (self.shell.drafts.clean() || self.shell.drafts.discard_on_close)
            && (!self.shell.save_enabled
                || (!self.shell.saving && self.shell.generation == self.shell.saved_generation))
        {
            return iced::exit();
        }
        if self.shell.save_enabled
            && !self.shell.saving
            && self.shell.generation != self.shell.saved_generation
        {
            self.shell.saving = true;
            let (home, catalog, generation) = (
                self.home.clone(),
                self.shell.catalog.clone(),
                self.shell.generation,
            );
            tasks.push(Task::perform(
                async move {
                    tokio::task::spawn_blocking(move || {
                        catalog.save(&home).map_err(|e| e.to_string())
                    })
                    .await
                    .unwrap_or_else(|e| Err(e.to_string()))
                },
                move |result| Message::CatalogSaved(generation, result),
            ));
        }
        if let Some(generation) = self.shell.drafts.begin(self.shell.closing) {
            let home = self.shell.draft_home.clone();
            let saved = self.shell.draft_snapshot();
            tasks.push(Task::perform(
                async move {
                    tokio::task::spawn_blocking(move || {
                        saved.save(&home).map_err(|e| e.to_string())
                    })
                    .await
                    .unwrap_or_else(|e| Err(e.to_string()))
                },
                move |result| Message::DraftsSaved(generation, result),
            ));
        }
        // The thread column follows the thread, whichever message opened or
        // closed it; the grid is checked here once rather than at each.
        self.panes
            .window_width(self.shell.width / self.scale_factor());
        self.panes.sync_thread(self.shell.thread.is_some());
        tasks.push(crate::accessibility::collect());
        Task::batch(tasks)
    }

    fn schedule_update_check(&mut self, now: i64) {
        if self.shell.closing || !self.shell.save_enabled || !self.shell.catalog.updates.enabled {
            if self.shell.pending_update.take().is_some() {
                self.shell.catalog.updates.last_attempt = None;
                self.shell.changed();
            }
            return;
        }
        if self.desktop.busy
            || self.desktop.checking_updates
            || !self.desktop.prefix.trim().is_empty()
        {
            return;
        }
        if self.shell.pending_update.is_some() {
            // Persist the daily reservation before any network work. A full
            // worker queue keeps the reservation pending instead of losing it.
            if !self.shell.saving
                && self.shell.generation == self.shell.saved_generation
                && self.tx.send(Cmd::UpdateCheck).is_ok()
            {
                self.shell.pending_update = None;
                self.desktop.checking_updates = true;
            }
        } else if self.shell.catalog.updates.due(now) {
            self.shell.catalog.updates.last_attempt = Some(now);
            self.shell.pending_update = Some(now);
            self.shell.changed();
        }
    }

    fn refresh_project_context(&mut self) {
        let selected = self
            .shell
            .catalog
            .selected()
            .map(|e| e.project.id().to_string());
        if self.journal_project != selected {
            self.journal_project = selected.clone();
            self.journal.clear();
            if let Some(project) = selected {
                self.request_journal(project);
                if let Some(entry) = self.shell.catalog.selected() {
                    self.request_channels(entry.project.id().to_string());
                }
            }
            // Another project's board, and a card open on the old one
            // is not open on this.
            self.tasks = None;
            self.task_open = None;
            self.board_asks.clear();
            if self.screen == Screen::Board {
                self.request_tasks();
            }
        }
    }

    /// Wait for initial/reconnected snapshots before resolving a click. Route
    /// by the actual message and sender IDs, never by the notification's text.
    fn advance_notification(&mut self) -> Task<Message> {
        let Some((action, started)) = self.shell.pending_notification.clone() else {
            return Task::none();
        };
        let target = &action.target;
        let question = self.questions.iter().find(|q| {
            q.id == target.message
                && self.canonical_agent(&q.from) == self.canonical_agent(target.agent.as_str())
        });
        let envelope = self.inbox.iter().find(|m| {
            m.id == target.message
                && self.canonical_agent(&m.from) == self.canonical_agent(target.agent.as_str())
        });
        let channel = envelope.and_then(|m| match &m.to {
            agentdocker_core::Destination::Channel(id) => Some(id.clone()),
            _ => None,
        });
        // A message the person has already read is no longer in the inbox
        // but is archived in its conversation; with conversations, the
        // notification's own agent and channel name where it sits, and it
        // is there even when that agent's record or the channel is gone:
        // the archive outlives both.
        let archived = question.is_none() && envelope.is_none() && self.has_conversations();
        let channel = channel.or_else(|| archived.then(|| target.channel.clone()).flatten());
        let found = question.is_some() || envelope.is_some() || archived;
        if !found {
            if started.elapsed() >= Duration::from_secs(10) {
                self.shell.pending_notification = None;
                self.shell.notification_message = None;
                self.screen = Screen::Questions;
                self.say(if self.connected.is_ok() {
                    "This notification's message is no longer available."
                } else {
                    "Cannot open the notification while disconnected. Try again after reconnecting."
                });
            }
            return Task::none();
        }
        let is_question = question.is_some();
        // Prefer the actual channel's project. The source agent may have moved
        // since posting; a retained project ID is a fallback for direct messages.
        let project = channel
            .as_ref()
            .and_then(|id| self.channels.iter().find(|c| &c.id == id))
            .map(|c| c.project.clone())
            .or_else(|| target.project.clone());
        if let Some(project) = &project {
            let root = self
                .shell
                .catalog
                .projects
                .iter()
                .find(|entry| &entry.project.id() == project)
                .map(|entry| entry.project.root.clone());
            if let Some(root) = root {
                self.shell.catalog.selected = Some(root);
                self.shell.catalog.unassigned = false;
                self.shell.changed();
                self.refresh_project_context();
            } else if started.elapsed() < Duration::from_secs(10) {
                return Task::none();
            } else {
                self.shell.pending_notification = None;
                self.shell.notification_message = None;
                self.screen = Screen::Questions;
                self.say("This notification's project is no longer available.");
                return Task::none();
            }
        }
        // The Channels screen needs the channel open; an archived
        // conversation needs only its id.
        if let Some(channel) = &channel
            && !archived
            && !self.channels.iter().any(|c| &c.id == channel)
        {
            if started.elapsed() < Duration::from_secs(10) {
                return Task::none();
            }
            self.shell.pending_notification = None;
            self.shell.notification_message = None;
            self.screen = Screen::Questions;
            self.say("This notification's channel is no longer available.");
            return Task::none();
        }
        self.shell.pending_notification = None;
        self.shell.notification_message = Some(target.message.clone());
        self.shell.message_detail = Some(target.message.clone());
        self.cancel_reveal();
        self.shell.selected = Some(self.canonical_agent(target.agent.as_str()).to_owned());
        self.shell.more = false;
        self.confirm_stop = None;
        self.screen = if channel.is_some() {
            Screen::Channels
        } else {
            // Opened, not merely selected: in the narrow layout the list
            // would otherwise hide the conversation the notification names.
            // An archived conversation sits under the id its party had at
            // the time, so a retired identity opens its own, not the one of
            // the record it became; the inbox's threads go by the current.
            let thread_with = if archived {
                target.agent.to_string()
            } else {
                self.canonical_agent(target.agent.as_str()).to_owned()
            };
            self.open_thread_with(thread_with);
            Screen::Questions
        };
        if let Some(channel) = channel {
            self.shell.channel_target = Some(channel.to_string());
            if self.has_conversations() {
                self.screen = Screen::Questions;
                let conversation = format!("channel:{channel}");
                // Another conversation's thread does not follow.
                if self.shell.conversation.as_deref() != Some(conversation.as_str()) {
                    self.shell.thread = None;
                    self.thread = None;
                }
                self.shell.conversation = Some(conversation.clone());
                self.shell.inbox_open = true;
                self.send(Cmd::History(conversation, self.history_epoch));
            }
        }
        self.recover_reply();
        // On the Messages screen the message is a row of the archive, not
        // of the inbox: it is scrolled to once its page is here, paging
        // back for it if the conversation was already open at its newest
        // — a click on a notification must always show its message.
        if self.has_conversations()
            && !is_question
            && self.screen == Screen::Questions
            && let Some(conversation) = self.shell.conversation.clone()
        {
            self.start_archive_reveal(conversation, target.message.clone());
        }
        // Revealing the card expands its retained text and scrolls to it. Existing answer/channel
        // drafts and their keyboard focus are not submitted or rewritten.
        crate::controls::reveal(format!(
            "notification-{}-{}",
            if is_question { "question" } else { "message" },
            target.message
        ))
    }

    /// Enable the selected provider's input adapter using this installation's
    /// matching CLI. A PATH fallback could select an incompatible old install.
    fn prepare_launch(
        &self,
        mut spec: agentdocker_core::AgentSpec,
        cli: Result<PathBuf, String>,
    ) -> Result<agentdocker_core::AgentSpec, String> {
        if !self.shell.launch_channel || !matches!(spec.runtime.as_str(), "claude-code" | "codex") {
            return Ok(spec);
        }
        let cli = cli?;
        let enable = if spec.runtime == "codex" {
            agentdocker_host::provider_input::enable_codex_input
        } else {
            agentdocker_host::provider_input::enable_claude_channel
        };
        enable(&mut spec, &cli)
            .map_err(|error| format!("Cannot enable messages while idle: {error}"))?;
        Ok(spec)
    }

    /// Why a session cannot be reconnected here right now, or nothing
    /// when it can: it must be a Claude Code session with a conversation
    /// to resume, its process must have ended (the app cannot exit a
    /// session it does not own, and resuming a conversation a live
    /// process still holds would start a second one), and its tool must
    /// be installed. The words are the button's.
    pub(super) fn reconnect_blocker(
        &self,
        agent: &agentdocker_core::AgentRecord,
    ) -> Option<&'static str> {
        if agent.spec.runtime != "claude-code" {
            return Some("Only a Claude Code session can be reconnected here.");
        }
        if !agent.spec.labels.contains_key("session_id") {
            return Some("This session has no conversation id to resume.");
        }
        if agent.status.is_live() {
            return Some(
                "Exit the session in its terminal first (/exit); this enables when it has.",
            );
        }
        if !self
            .runtimes
            .iter()
            .any(|r| r.name == "claude-code" && r.cli.is_some())
        {
            return Some("Claude Code is not installed here.");
        }
        None
    }

    /// One press of **Reconnect here**: the launch goes to the daemon as
    /// a resume of that record, once. A second press while the first is
    /// unanswered does nothing, and the daemon's refusal (the list was
    /// stale: the process is back, the checkout differs, somebody is
    /// attached) comes back as the error under the button.
    pub(super) fn reconnect(&mut self, id: &str, cli: Result<PathBuf, String>) {
        if self.connected.is_ok() && !self.shell.launching {
            match self.reconnect_spec(id, cli) {
                Ok(spec) => {
                    self.shell.launching = true;
                    self.shell.reconnecting = Some(id.to_owned());
                    self.shell.error = None;
                    self.send(Cmd::Resume(id.to_owned(), Box::new(spec)));
                }
                Err(error) => self.shell.error = Some(error),
            }
        }
    }

    /// The launch that reconnects an ended Claude Code session: its own
    /// tool, `--resume` with its conversation, in its own checkout, with
    /// the AgentDocker channel, under its name. The daemon checks it
    /// against the record and brings the record back under its own id
    /// with its queue. Nothing is sent or acknowledged on its behalf;
    /// consent is Claude's own prompt in the pane this opens.
    fn reconnect_spec(
        &self,
        id: &str,
        cli: Result<PathBuf, String>,
    ) -> Result<agentdocker_core::AgentSpec, String> {
        let agent = self
            .agents
            .iter()
            .find(|a| a.id.as_str() == id)
            .ok_or("This session is no longer listed")?;
        if let Some(blocker) = self.reconnect_blocker(agent) {
            return Err(blocker.to_owned());
        }
        let session = agent
            .spec
            .labels
            .get("session_id")
            .cloned()
            .ok_or("This session has no conversation id to resume")?;
        let claude = self
            .runtimes
            .iter()
            .find(|r| r.name == "claude-code")
            .and_then(|r| r.cli.clone())
            .ok_or("Claude Code is not installed here")?;
        let workdir = agent
            .spec
            .workdir
            .clone()
            .ok_or("This session has no checkout to resume in")?;
        let mut spec = agentdocker_core::AgentSpec {
            name: agent.spec.name.clone(),
            runtime: "claude-code".into(),
            provider: None,
            model: None,
            command: vec![
                claude.to_string_lossy().into_owned(),
                "--resume".into(),
                session,
            ],
            workdir: Some(workdir),
            env: BTreeMap::new(),
            labels: BTreeMap::new(),
            isolate: false,
            tty: true,
            restore: false,
            in_pane: false,
            restart: Default::default(),
            depends_on: Vec::new(),
        };
        let cli = cli.map_err(|error| format!("Cannot enable live messages: {error}"))?;
        agentdocker_host::provider_input::enable_claude_channel(&mut spec, &cli)
            .map_err(|error| format!("Cannot enable live messages: {error}"))?;
        Ok(spec)
    }

    fn launch_spec(&self) -> Result<agentdocker_core::AgentSpec, String> {
        let entry = self
            .shell
            .catalog
            .selected()
            .ok_or("Choose a project first")?;
        let runtime = self
            .runtimes
            .iter()
            .find(|r| Some(&r.name) == self.shell.launch_runtime.as_ref())
            .ok_or("Choose an installed command-line agent")?;
        let cli = runtime
            .cli
            .as_ref()
            .ok_or("This tool has no launchable command-line executable")?;
        let args = shell_words(&self.shell.launch_arguments)
            .ok_or("The launch arguments have unbalanced quotes")?;
        let mut command = vec![cli.to_string_lossy().into_owned()];
        command.extend(args);
        let generated_name = self.shell.launch_name.trim().is_empty();
        let name = if generated_name {
            format!("{}-{}", runtime.name, Utc::now().timestamp())
        } else {
            self.shell.launch_name.trim().into()
        };
        Ok(agentdocker_core::AgentSpec {
            name,
            runtime: runtime.name.clone(),
            provider: None,
            model: None,
            command,
            workdir: Some(entry.project.root.clone()),
            env: BTreeMap::new(),
            labels: if generated_name {
                BTreeMap::from([(
                    agentdocker_core::agent::NAME_LABEL.to_owned(),
                    agentdocker_core::agent::GENERATED_NAME.to_owned(),
                )])
            } else {
                BTreeMap::new()
            },
            isolate: false,
            tty: true,
            restore: false,
            in_pane: false,
            restart: Default::default(),
            depends_on: Vec::new(),
        })
    }
}

/// This app's own `agentdocker`, beside its executable, or why not. No
/// `PATH` fallback: a session pinned to a stranger's CLI is worse than no
/// session.
fn sibling_cli() -> Result<PathBuf, String> {
    let me = agentdocker_host::procinfo::executable_path()
        .map_err(|error| format!("Cannot locate this app: {error}"))?;
    let cli = me
        .parent()
        .map(|dir| dir.join("agentdocker"))
        .filter(|cli| cli.is_file())
        .filter(|cli| cli.canonicalize().ok() != me.canonicalize().ok())
        .ok_or("The agentdocker command-line tool is not installed beside this app")?;
    Ok(cli)
}

fn open_native_terminal(request: crate::native_terminal::Request) -> Task<Message> {
    Task::perform(
        async move {
            tokio::task::spawn_blocking(move || crate::native_terminal::open(request))
                .await
                .unwrap_or_else(|e| Err(e.to_string()))
        },
        Message::NativeTerminalOpened,
    )
}

fn resolve_folder(path: PathBuf) -> Task<Message> {
    Task::perform(
        async move {
            tokio::task::spawn_blocking(move || {
                crate::catalog::resolve(&path).map_err(|e| e.to_string())
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string()))
        },
        Message::FolderResolved,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn app() -> (App, CommandReceiver, SyncSender<Msg>) {
        let (tx, commands) = queue::channel();
        let (messages, rx) = sync_channel(MESSAGE_CAPACITY);
        (App::bare(tx, rx), commands, messages)
    }

    /// The board asks sent so far, newest last: `(request, offset, limit)`.
    fn board_asks(requests: &CommandReceiver) -> Vec<(u64, usize, usize)> {
        requests
            .try_iter()
            .filter_map(|c| match c {
                Cmd::Tasks {
                    request,
                    offset,
                    limit,
                    ..
                } => Some((request, offset, limit)),
                _ => None,
            })
            .collect()
    }

    /// A card's draft is the project's: text typed for one board waits
    /// while another is on view, a move or hand of some other card does
    /// not clear it, a late reply to an earlier filing does not take
    /// newer text, a filing that cannot be queued says so instead of
    /// staying "Filing…", and a board that cannot be read stays as last
    /// read.
    #[test]
    fn a_card_draft_survives_other_board_actions_late_replies_and_refused_queues() {
        let (mut app, requests, messages) = app();
        app.connected = Ok(());
        let dir = tempfile::tempdir().unwrap();
        let alpha = agentdocker_core::ProjectRef::directory(dir.path().join("alpha"));
        let beta = agentdocker_core::ProjectRef::directory(dir.path().join("beta"));
        app.shell.catalog.remember(alpha.clone(), true);
        app.shell.catalog.remember(beta.clone(), true);
        app.shell.catalog.selected = Some(alpha.root.clone());
        let alpha_root = alpha.root.display().to_string();
        let beta_root = beta.root.display().to_string();
        let card = |id: &str, column: agentdocker_core::Column| agentdocker_core::Task {
            id: agentdocker_core::TaskId::from(id.to_owned()),
            project: alpha.id(),
            title: format!("card {id}"),
            acceptance: String::new(),
            column,
            assignee: None,
            created_by: "user".into(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            archived_at: None,
            links: Vec::new(),
        };
        app.request_tasks();
        let (ask, _, _) = board_asks(&requests).pop().expect("asked");
        messages
            .send(Msg::Tasks(
                alpha_root.clone(),
                ask,
                Ok((
                    vec![card("aaaaaaaaaaaa", agentdocker_core::Column::Ready)],
                    false,
                )),
            ))
            .unwrap();
        app.drain();
        assert_eq!(app.tasks.as_ref().map(|b| b.cards.len()), Some(1));

        let _ = app.update(Message::TaskTitle("Port the parser".into()));
        let _ = app.update(Message::TaskAcceptance("tests pass".into()));
        // Another card is moved and the reply comes: the draft stays.
        let _ = app.update(Message::TaskMove(
            agentdocker_core::TaskId::from("aaaaaaaaaaaa".to_owned()),
            agentdocker_core::Column::Review,
        ));
        messages.send(Msg::TaskChanged(Ok(()))).unwrap();
        app.drain();
        assert_eq!(
            app.shell.task_drafts[&alpha_root].title, "Port the parser",
            "a move of some other card is not a filing"
        );
        // A board that cannot be read is said so, and the last board stays.
        app.request_tasks();
        let (ask, _, _) = board_asks(&requests).pop().expect("asked");
        messages
            .send(Msg::Tasks(
                alpha_root.clone(),
                ask,
                Err("storage failed".into()),
            ))
            .unwrap();
        app.drain();
        assert_eq!(app.tasks.as_ref().map(|b| b.cards.len()), Some(1));
        assert!(app.status.contains("storage failed"), "{}", app.status);

        // Filed: typing waits; a reply to an *earlier* filing changes
        // nothing; the reply to this one clears the text.
        let _ = app.update(Message::TaskFile(agentdocker_core::Column::Ready));
        let request = app.shell.task_drafts[&alpha_root].sending.expect("filing");
        assert!(requests.try_iter().any(|c| matches!(
            c,
            Cmd::TaskCreate { ref project, request: r, .. } if *project == alpha_root && r == request
        )));
        let _ = app.update(Message::TaskTitle("typed while filing".into()));
        assert_eq!(app.shell.task_drafts[&alpha_root].title, "Port the parser");
        messages
            .send(Msg::TaskCreated(alpha_root.clone(), request - 1, Ok(())))
            .unwrap();
        app.drain();
        assert_eq!(
            app.shell.task_drafts[&alpha_root].sending,
            Some(request),
            "a late reply to an earlier filing is not this one's"
        );
        messages
            .send(Msg::TaskCreated(alpha_root.clone(), request, Ok(())))
            .unwrap();
        app.drain();
        assert_eq!(app.shell.task_drafts[&alpha_root].title, "");
        assert!(app.shell.task_drafts[&alpha_root].sending.is_none());

        // Beta's draft is beta's: alpha's text waits while beta is on view.
        let _ = app.update(Message::TaskTitle("alpha again".into()));
        app.shell.catalog.selected = Some(beta.root.clone());
        let _ = app.update(Message::TaskTitle("beta's card".into()));
        assert_eq!(app.shell.task_drafts[&alpha_root].title, "alpha again");
        assert_eq!(app.shell.task_drafts[&beta_root].title, "beta's card");

        // A filing the command queue refuses is told so at once.
        let _ = app.update(Message::TaskFile(agentdocker_core::Column::Backlog));
        let request = app.shell.task_drafts[&beta_root].sending.expect("filing");
        app.rejected(
            Cmd::TaskCreate {
                project: beta_root.clone(),
                request,
                title: "beta's card".into(),
                acceptance: String::new(),
                column: agentdocker_core::Column::Backlog,
            },
            "the command queue is full",
        );
        let draft = &app.shell.task_drafts[&beta_root];
        assert!(draft.sending.is_none(), "not left filing for good");
        assert!(draft.error.is_some());
        assert_eq!(draft.title, "beta's card", "the text is kept to retry");
    }

    /// The board goes on past a page: Show more asks for the next page
    /// where the board ends and appends it only when that very ask is
    /// answered; a reply to no standing ask moves nothing; a refresh
    /// asks for as many cards as are on view and supersedes a page still
    /// on its way, whichever order the replies come in, so the board
    /// never folds back; choosing another project and back forgets the
    /// old asks; the window keeps five pages and then asks for no more.
    #[test]
    fn the_board_shows_more_a_page_at_a_time_and_stays_expanded_through_a_refresh() {
        let (mut app, requests, messages) = app();
        app.connected = Ok(());
        let dir = tempfile::tempdir().unwrap();
        let alpha = agentdocker_core::ProjectRef::directory(dir.path().join("alpha"));
        let beta = agentdocker_core::ProjectRef::directory(dir.path().join("beta"));
        app.shell.catalog.remember(alpha.clone(), true);
        app.shell.catalog.remember(beta.clone(), true);
        app.shell.catalog.selected = Some(alpha.root.clone());
        let root = alpha.root.display().to_string();
        let page = |from: usize, count: usize| {
            (from..from + count)
                .map(|i| agentdocker_core::Task {
                    id: agentdocker_core::TaskId::from(format!("{i:012x}")),
                    project: alpha.id(),
                    title: format!("card {i}"),
                    acceptance: String::new(),
                    column: agentdocker_core::Column::Backlog,
                    assignee: None,
                    created_by: "user".into(),
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                    archived_at: None,
                    links: Vec::new(),
                })
                .collect::<Vec<_>>()
        };
        let limit = agentdocker_core::protocol::TASKS_LIMIT;
        let cards = |app: &App| app.tasks.as_ref().map_or(0, |b| b.cards.len());

        // A reply to no ask moves nothing.
        messages
            .send(Msg::Tasks(root.clone(), 999, Ok((page(0, 3), false))))
            .unwrap();
        app.drain();
        assert!(app.tasks.is_none());

        app.request_tasks();
        let (first, _, _) = board_asks(&requests).pop().expect("asked");
        messages
            .send(Msg::Tasks(root.clone(), first, Ok((page(0, limit), true))))
            .unwrap();
        app.drain();
        assert_eq!(cards(&app), limit);
        let _ = app.update(Message::TasksMore);
        assert!(app.tasks.as_ref().unwrap().loading_more());
        let (more_ask, offset, asked) = board_asks(&requests).pop().expect("asked for more");
        assert_eq!((offset, asked), (limit, limit));
        // A second click while a page is on its way asks for nothing;
        // a reply to an ask already answered is not appended.
        let _ = app.update(Message::TasksMore);
        assert!(board_asks(&requests).is_empty());
        messages
            .send(Msg::Tasks(root.clone(), first, Ok((page(7, 3), true))))
            .unwrap();
        app.drain();
        assert_eq!(cards(&app), limit);
        messages
            .send(Msg::Tasks(
                root.clone(),
                more_ask,
                Ok((page(limit, limit), true)),
            ))
            .unwrap();
        app.drain();
        let board = app.tasks.as_ref().unwrap();
        assert_eq!(
            (board.cards.len(), board.more, board.loading_more()),
            (2 * limit, true, false)
        );

        // Order one: a refresh is asked while a page is on its way. The
        // refresh supersedes the page — whichever reply lands first the
        // board is what the refresh says, and never folds back.
        let _ = app.update(Message::TasksMore);
        let (superseded, _, _) = board_asks(&requests).pop().unwrap();
        app.request_tasks();
        let (refresh, _, asked) = board_asks(&requests).pop().unwrap();
        assert_eq!(asked, 2 * limit, "a refresh asks for what is on view");
        assert!(!app.tasks.as_ref().unwrap().loading_more(), "superseded");
        messages
            .send(Msg::Tasks(
                root.clone(),
                superseded,
                Ok((page(2 * limit, limit), true)),
            ))
            .unwrap();
        app.drain();
        assert_eq!(cards(&app), 2 * limit, "a superseded page is not appended");
        messages
            .send(Msg::Tasks(
                root.clone(),
                refresh,
                Ok((page(0, 2 * limit), true)),
            ))
            .unwrap();
        app.drain();
        assert_eq!(cards(&app), 2 * limit);

        // Order two: while a refresh is on its way, Show more asks for
        // nothing — a page appended now would be to a board the refresh
        // is about to replace — and works again once the refresh lands.
        app.request_tasks();
        let (refresh, _, _) = board_asks(&requests).pop().unwrap();
        let _ = app.update(Message::TasksMore);
        assert!(
            board_asks(&requests).is_empty(),
            "deferred behind the refresh"
        );
        assert!(!app.tasks.as_ref().unwrap().loading_more());
        messages
            .send(Msg::Tasks(
                root.clone(),
                refresh,
                Ok((page(0, 2 * limit), true)),
            ))
            .unwrap();
        app.drain();
        let _ = app.update(Message::TasksMore);
        let (more_ask, _, _) = board_asks(&requests).pop().unwrap();
        messages
            .send(Msg::Tasks(
                root.clone(),
                more_ask,
                Ok((page(2 * limit, limit), true)),
            ))
            .unwrap();
        app.drain();
        assert_eq!(cards(&app), 3 * limit);

        // Two refreshes: the newer supersedes the older, so the older's
        // reply — even landing last — cannot overwrite the newer read.
        app.request_tasks();
        let (older, _, _) = board_asks(&requests).pop().unwrap();
        app.request_tasks();
        let (newer, _, _) = board_asks(&requests).pop().unwrap();
        messages
            .send(Msg::Tasks(
                root.clone(),
                newer,
                Ok((page(0, 3 * limit), true)),
            ))
            .unwrap();
        messages
            .send(Msg::Tasks(root.clone(), older, Ok((page(0, limit), true))))
            .unwrap();
        app.drain();
        assert_eq!(cards(&app), 3 * limit, "the older refresh is ignored");

        // Another project and back: the old asks are forgotten, so a
        // late page for alpha moves nothing, and alpha is read anew.
        let _ = app.update(Message::TasksMore);
        let (late, _, _) = board_asks(&requests).pop().unwrap();
        let _ = app.update(Message::SelectProject(beta.root.clone()));
        assert!(app.tasks.is_none());
        let _ = app.update(Message::SelectProject(alpha.root.clone()));
        messages
            .send(Msg::Tasks(
                root.clone(),
                late,
                Ok((page(3 * limit, limit), true)),
            ))
            .unwrap();
        app.drain();
        assert!(
            app.tasks.is_none(),
            "a page for an ask made before the switch"
        );

        // Filled to what the window keeps: no more is asked for.
        app.request_tasks();
        let (fill, _, _) = board_asks(&requests).pop().unwrap();
        messages
            .send(Msg::Tasks(
                root.clone(),
                fill,
                Ok((page(0, BOARD_KEEP), true)),
            ))
            .unwrap();
        app.drain();
        let _ = app.update(Message::TasksMore);
        assert!(board_asks(&requests).is_empty());
        assert_eq!(cards(&app), BOARD_KEEP);
    }

    /// The menu under a project row renames the entry here, pins it, or
    /// takes it off the list for good; nothing else moves.
    #[test]
    fn a_project_row_menu_renames_pins_and_removes_the_entry() {
        let (mut app, _requests, _messages) = app();
        let dir = tempfile::tempdir().unwrap();
        let alpha = agentdocker_core::ProjectRef::directory(dir.path().join("alpha"));
        let beta = agentdocker_core::ProjectRef::directory(dir.path().join("beta"));
        app.shell.catalog.remember(alpha.clone(), true);
        app.shell.catalog.remember(beta.clone(), false);
        let root = alpha.root.clone();
        let _ = app.update(Message::ProjectMenu(root.clone()));
        assert_eq!(app.shell.project_menu.as_ref(), Some(&root));
        let _ = app.update(Message::ProjectRenameStart(root.clone()));
        assert_eq!(
            app.shell.project_rename,
            Some((root.clone(), "alpha".to_owned()))
        );
        let _ = app.update(Message::ProjectRenameDraft("Alpha project".into()));
        let _ = app.update(Message::ProjectRenameSubmit);
        assert_eq!(app.shell.catalog.projects[0].name(), "Alpha project");
        assert!(app.shell.project_menu.is_none(), "saving closes the menu");
        // Opening the menu again and renaming to nothing goes back to the folder.
        let _ = app.update(Message::ProjectMenu(root.clone()));
        let _ = app.update(Message::ProjectRenameStart(root.clone()));
        assert_eq!(
            app.shell.project_rename,
            Some((root.clone(), "Alpha project".to_owned()))
        );
        let _ = app.update(Message::ProjectRenameDraft(String::new()));
        let _ = app.update(Message::ProjectRenameSubmit);
        assert_eq!(app.shell.catalog.projects[0].name(), "alpha");
        let _ = app.update(Message::ProjectPin(beta.root.clone()));
        assert!(app.shell.catalog.projects[1].pinned);
        let _ = app.update(Message::ProjectRemove(beta.root.clone()));
        assert_eq!(app.shell.catalog.projects.len(), 1);
        assert!(
            !app.shell.catalog.remember(beta, false),
            "kept off the list"
        );
    }

    #[test]
    fn file_approval_requires_open_review_and_preserves_drafts_on_stale_clicks() {
        use agentdocker_core::{QuestionFileChange, QuestionFileChangeKind, QuestionPresentation};
        let (mut app, commands, messages) = app();
        app.connected = Ok(());
        let presentation = QuestionPresentation::CodexFiles {
            cwd: "/owned".into(),
            reason: "Fixture".into(),
            changes: vec![QuestionFileChange {
                path: "/owned/a".into(),
                kind: QuestionFileChangeKind::Delete,
                diff: "-old\n".into(),
            }],
        };
        let id = MessageId::from("file-question".to_owned());
        app.questions.push(Question {
            id: id.clone(),
            from: "asker".into(),
            to: agentdocker_core::Destination::Agent("human".into()),
            text: presentation.text(),
            presentation: Some(presentation),
            asked_at: Utc::now(),
            expires_at: Utc::now() + chrono::Duration::minutes(5),
        });
        app.shell
            .answers
            .insert(id.clone(), "original draft".into());
        let _ = app.update(Message::AnswerChoice(id.clone(), "Allow".into()));
        assert_eq!(app.shell.answers[&id], "original draft");
        assert_eq!(commands.try_iter().count(), 0);
        app.shell.answers.insert(id.clone(), " Allow ".into());
        let _ = app.update(Message::Answer(id.clone()));
        assert_eq!(
            commands.try_iter().count(),
            0,
            "keyboard submit also requires reviewing the diff"
        );
        let _ = app.update(Message::ReviewFiles(id.clone()));
        assert_eq!(app.shell.file_review, Some(id.clone()));
        let _ = app.update(Message::AnswerChoice(id.clone(), "Allow".into()));
        assert!(
            matches!(commands.try_iter().collect::<Vec<_>>().as_slice(),[Cmd::Answer(answer,text)] if answer == &id && text == "Allow")
        );
        messages.send(Msg::Questions(Vec::new())).unwrap();
        let _ = app.update(Message::Tick);
        assert!(app.shell.file_review.is_none());
        assert_eq!(
            app.shell.answers[&id], " Allow ",
            "the typed draft stays retained while the explicit choice is in flight"
        );
    }

    #[test]
    fn structured_choices_use_one_answer_command_and_reject_stale_or_invalid_clicks() {
        let (mut app, commands, _) = app();
        app.connected = Ok(());
        let presentation = agentdocker_core::QuestionPresentation::CodexCommand {
            command: "printf hello".into(),
            cwd: "/owned".into(),
            reason: "Fixture".into(),
        };
        let id = MessageId::from("question".to_owned());
        let question = Question {
            id: id.clone(),
            from: "asker".into(),
            to: agentdocker_core::Destination::Agent("user".into()),
            text: presentation.text(),
            presentation: Some(presentation),
            asked_at: Utc::now(),
            expires_at: Utc::now() + chrono::Duration::minutes(5),
        };
        app.questions.push(question.clone());
        app.shell.answers.insert(id.clone(), "earlier draft".into());
        let _ = app.update(Message::AnswerChoice(
            id.clone(),
            "Allow for this session".into(),
        ));
        assert_eq!(commands.try_iter().count(), 0);
        assert_eq!(app.shell.answers[&id], "earlier draft");
        let _ = app.update(Message::AnswerChoice(id.clone(), "Allow".into()));
        let _ = app.update(Message::AnswerChoice(id.clone(), "Deny".into()));
        assert!(
            matches!(commands.try_iter().collect::<Vec<_>>().as_slice(), [Cmd::Answer(message, answer)] if message == &id && answer == "Allow")
        );
        assert_eq!(app.shell.answers[&id], "earlier draft");
        app.sending.clear();
        app.questions.clear();
        let _ = app.update(Message::AnswerChoice(id.clone(), "Deny".into()));
        let mut expired = question;
        expired.expires_at = Utc::now() - chrono::Duration::seconds(1);
        app.questions.push(expired);
        let _ = app.update(Message::AnswerChoice(id.clone(), "Deny".into()));
        assert_eq!(commands.try_iter().count(), 0);
        assert_eq!(app.shell.answers[&id], "earlier draft");
    }

    #[test]
    fn explicit_choices_bypass_full_draft_storage_and_failed_delivery_keeps_text() {
        let (mut app, commands, messages) = app();
        app.connected = Ok(());
        for index in 0..128 {
            assert!(
                app.shell
                    .edit_draft(DraftKind::Answer, index.to_string(), "x".into())
            );
            assert!(app.shell.edit_draft(
                DraftKind::Session,
                index.to_string(),
                "s".repeat(16_000)
            ));
            assert!(app.shell.edit_draft(
                DraftKind::Conversation,
                index.to_string(),
                "c".repeat(16_000)
            ));
        }
        let remaining = crate::drafts::MAX_TOTAL_BYTES - 128 * (1 + 16_000 * 2);
        for (index, chunk) in vec![b'z'; remaining].chunks(16_000).enumerate() {
            assert!(app.shell.edit_draft(
                DraftKind::Channel,
                index.to_string(),
                String::from_utf8(chunk.to_vec()).unwrap()
            ));
        }
        app.shell.drafts = crate::drafts::Persistence::loaded();
        let before = app.shell.draft_snapshot();
        before.validate().unwrap();
        for key in ["0", "without-a-draft"] {
            let id = MessageId::from(key.to_owned());
            let presentation = agentdocker_core::QuestionPresentation::CodexCommand {
                command: "printf hello".into(),
                cwd: "/owned".into(),
                reason: "Fixture".into(),
            };
            app.questions.push(Question {
                id: id.clone(),
                from: "asker".into(),
                to: agentdocker_core::Destination::Agent("human".into()),
                text: presentation.text(),
                presentation: Some(presentation),
                asked_at: Utc::now(),
                expires_at: Utc::now() + chrono::Duration::minutes(5),
            });
            let _ = app.update(Message::AnswerChoice(id.clone(), "Allow".into()));
            assert!(matches!(commands.try_iter().collect::<Vec<_>>().as_slice(),
                [Cmd::Answer(question, answer)] if question == &id && answer == "Allow"));
            assert!(app.sending.contains(&id));
            assert_eq!(app.shell.draft_snapshot(), before);
            assert!(app.shell.drafts.clean());
            messages
                .send(Msg::Answered(id.clone(), Err("offline".into())))
                .unwrap();
            app.drain();
            assert!(!app.sending.contains(&id));
            assert_eq!(app.shell.answer_errors[&id], "offline");
            assert_eq!(app.shell.draft_snapshot(), before);
            assert!(app.shell.drafts.clean());
        }
    }

    #[test]
    fn scheduled_checks_wait_for_persistence_and_queue_capacity_then_throttle_failures() {
        let (mut app, commands, messages) = app();
        app.shell.save_enabled = true;
        app.shell.catalog.updates.enabled = true;
        app.schedule_update_check(100_000);
        assert_eq!(app.shell.pending_update, Some(100_000));
        app.schedule_update_check(100_001);
        assert_eq!(
            commands.try_iter().count(),
            0,
            "reservation is not persisted yet"
        );
        app.shell.saved_generation = app.shell.generation;
        for _ in 0..queue::CAPACITY {
            app.tx.send(Cmd::Stop("fixture".into())).unwrap();
        }
        app.schedule_update_check(100_002);
        assert!(app.shell.pending_update.is_some());
        assert!(!app.desktop.checking_updates);
        assert_eq!(commands.try_iter().count(), queue::CAPACITY);
        app.schedule_update_check(100_003);
        assert!(matches!(
            commands.try_iter().collect::<Vec<_>>().as_slice(),
            [Cmd::UpdateCheck]
        ));
        assert!(app.desktop.checking_updates);
        assert!(app.shell.pending_update.is_none());
        app.schedule_update_check(200_000);
        assert_eq!(
            commands.try_iter().count(),
            0,
            "only one check may be in flight"
        );
        messages
            .send(Msg::UpdateChecked(Err("offline fixture".into())))
            .unwrap();
        app.drain();
        app.schedule_update_check(186_399);
        assert!(app.shell.pending_update.is_none());
        app.schedule_update_check(186_400);
        assert_eq!(app.shell.pending_update, Some(186_400));
        app.shell.catalog.updates.enabled = false;
        app.schedule_update_check(186_401);
        assert!(app.shell.pending_update.is_none());
    }

    #[test]
    fn answer_drafts_restore_unsent_and_follow_confirmed_question_lifecycle() {
        let home = tempfile::tempdir().unwrap();
        let (mut app, commands, messages) = app();
        app.shell = State::load(home.path());
        app.connected = Ok(());
        let id = MessageId::from("question-one".to_owned());
        let other = MessageId::from("question-two".to_owned());
        let question = Question {
            presentation: None,
            id: id.clone(),
            from: "asker".into(),
            to: agentdocker_core::Destination::Agent("human".into()),
            text: "Continue?".into(),
            asked_at: Utc::now(),
            expires_at: Utc::now() + chrono::Duration::minutes(5),
        };
        app.questions = vec![
            question.clone(),
            Question {
                id: other.clone(),
                ..question.clone()
            },
        ];
        let _ = app.update(Message::Draft(id.clone(), "café 日本語\nnot yet".into()));
        let _ = app.update(Message::Draft(other.clone(), "keep this one".into()));
        app.shell
            .draft_snapshot()
            .save(&app.shell.draft_home)
            .unwrap();
        app.shell = State::load(home.path());
        assert_eq!(app.shell.answers[&id], "café 日本語\nnot yet");
        assert!(app.sending.is_empty());
        assert!(app.shell.file_review.is_none());
        assert!(
            commands
                .try_iter()
                .all(|cmd| !matches!(cmd, Cmd::Answer(..))),
            "restoration is not consent or submission"
        );
        let _ = app.update(Message::Answer(id.clone()));
        assert!(
            commands
                .try_iter()
                .any(|cmd| matches!(cmd, Cmd::Answer(ref key, _) if key == &id))
        );
        messages
            .send(Msg::Answered(id.clone(), Err("offline".into())))
            .unwrap();
        app.drain();
        assert_eq!(app.shell.answers[&id], "café 日本語\nnot yet");
        app.shell
            .draft_snapshot()
            .save(&app.shell.draft_home)
            .unwrap();
        assert_eq!(
            State::load(home.path()).answers[&id],
            "café 日本語\nnot yet"
        );
        messages.send(Msg::Answered(id.clone(), Ok(()))).unwrap();
        app.drain();
        assert!(!app.shell.drafts.clean());
        app.shell
            .draft_snapshot()
            .save(&app.shell.draft_home)
            .unwrap();
        let reopened = State::load(home.path());
        assert!(!reopened.answers.contains_key(&id));
        assert_eq!(reopened.answers[&other], "keep this one");
        messages.send(Msg::Questions(Vec::new())).unwrap();
        app.drain();
        assert!(
            app.shell.answers.is_empty(),
            "a completed question no longer has a draft"
        );
        app.shell
            .draft_snapshot()
            .save(&app.shell.draft_home)
            .unwrap();
        assert!(State::load(home.path()).answers.is_empty());
    }

    #[test]
    fn card_drafts_reopen_per_project_and_clear_only_after_their_confirmed_filing() {
        let home = tempfile::tempdir().unwrap();
        let (mut app, commands, messages) = app();
        app.shell = State::load(home.path());
        app.connected = Ok(());
        let alpha = agentdocker_core::ProjectRef::directory(home.path().join("alpha"));
        let beta = agentdocker_core::ProjectRef::directory(home.path().join("beta"));
        for project in [&alpha, &beta] {
            app.shell.catalog.remember(project.clone(), true);
        }
        let a = alpha.root.display().to_string();
        let b = beta.root.display().to_string();
        app.shell.catalog.selected = Some(alpha.root.clone());
        let _ = app.update(Message::TaskTitle("café 日本語".into()));
        let _ = app.update(Message::TaskAcceptance(
            "Run the checks\nThen review".into(),
        ));
        app.shell.catalog.selected = Some(beta.root.clone());
        let _ = app.update(Message::TaskAcceptance("A title will follow".into()));
        app.shell.task_drafts.get_mut(&a).unwrap().sending = Some(99);
        app.shell.task_drafts.get_mut(&a).unwrap().error = Some("old failure".into());
        app.shell
            .draft_snapshot()
            .save(&app.shell.draft_home)
            .unwrap();
        let catalog = std::mem::take(&mut app.shell.catalog);
        app.shell = State::load(home.path());
        app.shell.catalog = catalog;
        assert_eq!(app.shell.task_drafts[&a].title, "café 日本語");
        assert_eq!(
            app.shell.task_drafts[&a].acceptance,
            "Run the checks\nThen review"
        );
        assert_eq!(app.shell.task_drafts[&b].acceptance, "A title will follow");
        assert!(
            app.shell
                .task_drafts
                .values()
                .all(|d| !d.sending() && d.error.is_none())
        );
        assert!(
            commands
                .try_iter()
                .all(|cmd| !matches!(cmd, Cmd::TaskCreate { .. }))
        );
        app.shell.catalog.selected = Some(alpha.root.clone());
        let _ = app.update(Message::TaskFile(agentdocker_core::Column::Backlog));
        let request = app.shell.task_drafts[&a].sending.unwrap();
        assert!(commands.try_iter().any(|cmd| matches!(cmd, Cmd::TaskCreate { project, request: r, .. } if project == a && r == request)));
        messages
            .send(Msg::TaskCreated(a.clone(), request, Err("offline".into())))
            .unwrap();
        app.drain();
        app.shell
            .draft_snapshot()
            .save(&app.shell.draft_home)
            .unwrap();
        assert_eq!(
            State::load(home.path()).task_drafts[&a].title,
            "café 日本語"
        );
        let _ = app.update(Message::TaskFile(agentdocker_core::Column::Backlog));
        let retry = app.shell.task_drafts[&a].sending.unwrap();
        messages
            .send(Msg::TaskCreated(a.clone(), request, Ok(())))
            .unwrap();
        app.drain();
        assert_eq!(
            app.shell.task_drafts[&a].title, "café 日本語",
            "old receipt cannot clear this filing"
        );
        app.shell.catalog.selected = Some(beta.root);
        messages
            .send(Msg::TaskCreated(a.clone(), retry, Ok(())))
            .unwrap();
        app.drain();
        assert!(!app.shell.drafts.clean());
        app.shell
            .draft_snapshot()
            .save(&app.shell.draft_home)
            .unwrap();
        let restored = State::load(home.path());
        assert!(!restored.task_drafts.contains_key(&a));
        assert_eq!(restored.task_drafts[&b].acceptance, "A title will follow");
    }

    #[test]
    fn card_drafts_share_storage_pressure_without_evicting_unfinished_text() {
        let mut state = State::default();
        for index in 0..128 {
            assert!(state.edit_draft(DraftKind::TaskTitle, index.to_string(), "keep".into()));
        }
        let before = state.draft_snapshot();
        assert!(!state.edit_draft(DraftKind::TaskTitle, "overflow".into(), "new".into()));
        assert!(!state.edit_draft(DraftKind::TaskTitle, "0".into(), "界".repeat(201)));
        assert!(!state.edit_draft(DraftKind::TaskAcceptance, "0".into(), "界".repeat(4001)));
        assert!(
            state.task_drafts["0"]
                .error
                .as_deref()
                .unwrap()
                .contains("4,000")
        );
        assert_eq!(state.draft_snapshot(), before);
        assert!(state.edit_draft(DraftKind::TaskTitle, "0".into(), String::new()));
        assert!(state.edit_draft(DraftKind::TaskTitle, "overflow".into(), "new".into()));
        for kind in [DraftKind::Session, DraftKind::Conversation] {
            for index in 0..128 {
                assert!(state.edit_draft(kind, index.to_string(), "x".repeat(16_000)));
            }
        }
        let used_cards: usize = state
            .task_drafts
            .values()
            .map(|d| d.title.len() + d.acceptance.len())
            .sum();
        let mut remaining = crate::drafts::MAX_TOTAL_BYTES - 256 * 16_000 - used_cards;
        for index in 0..128 {
            if remaining == 0 {
                break;
            }
            let count = remaining.min(16_000);
            assert!(state.edit_draft(DraftKind::Channel, index.to_string(), "x".repeat(count)));
            remaining -= count;
        }
        let before = state.draft_snapshot();
        before.validate().unwrap();
        assert!(!state.edit_draft(DraftKind::TaskAcceptance, "1".into(), "more".into()));
        assert!(!state.edit_draft(DraftKind::Answer, "question".into(), "more".into()));
        assert_eq!(state.draft_snapshot(), before);
    }

    #[test]
    fn answer_edits_share_the_total_budget_and_never_truncate_or_evict_text() {
        let mut state = State::default();
        for index in 0..128 {
            assert!(state.edit_draft(DraftKind::Answer, index.to_string(), "keep".into()));
        }
        let before = state.draft_snapshot();
        assert!(!state.edit_draft(DraftKind::Answer, "overflow".into(), "new".into()));
        assert_eq!(state.draft_snapshot(), before);
        assert!(!state.edit_draft(DraftKind::Answer, "0".into(), "界".repeat(16_001)));
        assert_eq!(state.draft_snapshot(), before);
        assert!(state.edit_draft(DraftKind::Answer, "0".into(), String::new()));
        assert!(state.edit_draft(DraftKind::Answer, "overflow".into(), "new".into()));
        for index in 0..128 {
            assert!(state.edit_draft(DraftKind::Session, index.to_string(), "x".repeat(16_000)));
        }
        for index in 0..127 {
            assert!(state.edit_draft(
                DraftKind::Conversation,
                index.to_string(),
                "x".repeat(16_000)
            ));
        }
        assert!(state.edit_draft(DraftKind::Conversation, "last".into(), "x".repeat(16_000)));
        let snapshot = state.draft_snapshot();
        let used = [
            &snapshot.sessions,
            &snapshot.conversations,
            &snapshot.channels,
            &snapshot.answers,
        ]
        .into_iter()
        .flat_map(|drafts| drafts.values())
        .map(String::len)
        .sum::<usize>();
        let left = crate::drafts::MAX_TOTAL_BYTES - used;
        // Fill the remaining shared space without changing any answer.
        for (index, chunk) in "z".repeat(left).as_bytes().chunks(16_000).enumerate() {
            assert!(state.edit_draft(
                DraftKind::Channel,
                index.to_string(),
                String::from_utf8(chunk.to_vec()).unwrap()
            ));
        }
        let before = state.draft_snapshot();
        assert!(!state.edit_draft(DraftKind::Answer, "1".into(), "longer than keep".into()));
        assert_eq!(state.draft_snapshot(), before);
    }

    #[test]
    fn saved_message_drafts_reopen_as_text_without_delivery_state() {
        let home = tempfile::tempdir().unwrap();
        let mut state = State::load(home.path());
        state.edit_draft(
            DraftKind::Session,
            "agent-a".into(),
            "next session input".into(),
        );
        state.edit_draft(
            DraftKind::Conversation,
            "dm:a:b".into(),
            "你好\nconversation".into(),
        );
        state.edit_draft(
            DraftKind::Conversation,
            "dm:a:b/thread".into(),
            "thread input".into(),
        );
        state.edit_draft(DraftKind::Channel, "room".into(), "channel input".into());
        state
            .session_drafts
            .get_mut("agent-a")
            .unwrap()
            .draft
            .begin();
        state.session_drafts.get_mut("agent-a").unwrap().queued =
            Some(MessageId::from("old-receipt".to_owned()));
        state.conversation_drafts.get_mut("dm:a:b").unwrap().begin();
        state.channel_drafts.get_mut("room").unwrap().error = Some("old failure".into());
        let saved = state.draft_snapshot();
        saved.save(&state.draft_home).unwrap();
        let reopened = State::load(home.path());
        assert_eq!(reopened.draft_snapshot(), saved);
        assert!(reopened.session_drafts["agent-a"].draft.sending.is_none());
        assert!(reopened.session_drafts["agent-a"].queued.is_none());
        assert!(reopened.conversation_drafts["dm:a:b"].sending.is_none());
        assert!(reopened.channel_drafts["room"].error.is_none());
        assert!(reopened.drafts.clean());
        assert!(reopened.drafts.error.is_none());
    }

    #[test]
    fn late_send_results_and_save_completions_preserve_newer_persisted_text() {
        let (mut app, _requests, messages) = app();
        let home = tempfile::tempdir().unwrap();
        app.home = home.path().to_owned();
        app.shell = State::load(home.path());
        app.shell
            .edit_draft(DraftKind::Session, "recipient".into(), "submitted".into());
        app.shell
            .session_drafts
            .get_mut("recipient")
            .unwrap()
            .draft
            .begin();
        let old_generation = app.shell.drafts.begin(true).unwrap();
        app.shell
            .draft_snapshot()
            .save(&app.shell.draft_home)
            .unwrap();
        app.shell
            .edit_draft(DraftKind::Session, "recipient".into(), "next draft".into());
        messages
            .send(Msg::SessionSent(
                "recipient".into(),
                Ok(MessageId::from("receipt".to_owned()).into()),
            ))
            .unwrap();
        app.drain();
        app.shell.drafts.complete(old_generation, Ok(()));
        assert!(
            !app.shell.drafts.clean(),
            "the saved old submission is not the newer draft"
        );
        let latest = app.shell.drafts.begin(true).unwrap();
        app.shell
            .draft_snapshot()
            .save(&app.shell.draft_home)
            .unwrap();
        app.shell.drafts.complete(latest, Ok(()));
        assert_eq!(
            State::load(home.path()).session_drafts["recipient"]
                .draft
                .text,
            "next draft"
        );
        // Only success for the unchanged next submission clears it on disk.
        app.shell
            .session_drafts
            .get_mut("recipient")
            .unwrap()
            .draft
            .begin();
        messages
            .send(Msg::SessionSent(
                "recipient".into(),
                Ok(MessageId::from("next-receipt".to_owned()).into()),
            ))
            .unwrap();
        app.drain();
        app.shell
            .draft_snapshot()
            .save(&app.shell.draft_home)
            .unwrap();
        assert!(
            !State::load(home.path())
                .session_drafts
                .contains_key("recipient")
        );
    }

    #[test]
    fn draft_admission_keeps_nonempty_text_and_corrupt_storage_is_not_overwritten() {
        let home = tempfile::tempdir().unwrap();
        let mut state = State::load(home.path());
        for index in 0..128 {
            state.edit_draft(
                DraftKind::Conversation,
                index.to_string(),
                format!("draft {index}"),
            );
        }
        let before = state.draft_snapshot();
        state.edit_draft(DraftKind::Conversation, "extra".into(), "cannot fit".into());
        assert_eq!(state.draft_snapshot(), before);
        assert!(
            state
                .error
                .as_deref()
                .unwrap()
                .contains("Existing drafts were kept")
        );
        state.edit_draft(DraftKind::Conversation, "0".into(), "x".repeat(16_001));
        assert_eq!(state.draft_snapshot(), before);
        state.edit_draft(DraftKind::Conversation, "0".into(), String::new());
        state.edit_draft(
            DraftKind::Conversation,
            "extra".into(),
            "fits after clearing an empty draft".into(),
        );
        assert_eq!(state.conversation_drafts.len(), 128);
        assert_eq!(state.conversation_drafts["1"].text, "draft 1");
        state.draft_snapshot().save(&state.draft_home).unwrap();
        std::fs::write(state.draft_home.join("drafts.json"), b"corrupt").unwrap();
        let mut reopened = State::load(home.path());
        assert!(!reopened.drafts.readable);
        assert!(
            reopened
                .drafts
                .error
                .as_deref()
                .unwrap()
                .contains("preserved")
        );
        reopened.edit_draft(DraftKind::Session, "agent".into(), "still editable".into());
        assert!(reopened.drafts.begin(true).is_none());
        assert_eq!(
            std::fs::read(state.draft_home.join("drafts.json")).unwrap(),
            b"corrupt"
        );
    }

    #[test]
    fn late_send_readiness_stays_with_its_destination_and_does_not_restore_as_current() {
        let (mut app, _commands, messages) = app();
        let home = tempfile::tempdir().unwrap();
        app.shell = State::load(home.path());
        app.shell.conversation = Some("elsewhere".into());
        for kind in [
            DraftKind::Session,
            DraftKind::Conversation,
            DraftKind::Channel,
        ] {
            app.shell.edit_draft(kind, "original".into(), "sent".into());
            let target = match kind {
                DraftKind::Session => DeliveryTarget::Session("original".into()),
                DraftKind::Conversation => DeliveryTarget::Conversation("original".into()),
                DraftKind::Channel => DeliveryTarget::Channel("original".into()),
                DraftKind::Answer | DraftKind::TaskTitle | DraftKind::TaskAcceptance => {
                    unreachable!("message drafts only")
                }
            };
            let mut report = agentdocker_core::SendReadiness::default();
            report.observe(Some(agentdocker_core::RecipientReadiness::unknown(
                "missing".into(),
            )));
            let receipt = Ok(QueuedSend {
                message: "queued".to_owned().into(),
                readiness: Some(report.clone()),
            });
            messages
                .send(match kind {
                    DraftKind::Session => Msg::SessionSent("original".into(), receipt),
                    DraftKind::Conversation => Msg::ConversationSent("original".into(), receipt),
                    DraftKind::Channel => Msg::ChannelSent("original".into(), receipt),
                    DraftKind::Answer | DraftKind::TaskTitle | DraftKind::TaskAcceptance => {
                        unreachable!("message drafts only")
                    }
                })
                .unwrap();
            app.drain();
            let draft = match kind {
                DraftKind::Session => &app.shell.session_drafts["original"].draft,
                DraftKind::Conversation => &app.shell.conversation_drafts["original"],
                DraftKind::Channel => &app.shell.channel_drafts["original"],
                DraftKind::Answer | DraftKind::TaskTitle | DraftKind::TaskAcceptance => {
                    unreachable!("message drafts only")
                }
            };
            assert_eq!(draft.readiness.as_ref(), Some(&report));
            assert!(!draft.readiness_expanded);
            assert_eq!(app.shell.conversation.as_deref(), Some("elsewhere"));
            let _ = app.update(Message::DeliveryDetails(target));
        }
        app.shell
            .draft_snapshot()
            .save(&app.shell.draft_home)
            .unwrap();
        let reopened = State::load(home.path());
        assert!(
            reopened
                .session_drafts
                .values()
                .all(|entry| entry.draft.readiness.is_none())
        );
        assert!(
            reopened
                .conversation_drafts
                .values()
                .all(|draft| draft.readiness.is_none())
        );
        assert!(
            reopened
                .channel_drafts
                .values()
                .all(|draft| draft.readiness.is_none())
        );
    }

    #[test]
    fn retyped_channel_and_session_drafts_survive_late_receipts() {
        fn draft(app: &mut App, session: bool) -> &mut ChannelDraft {
            if session {
                &mut app.shell.session_drafts.get_mut("recipient").unwrap().draft
            } else {
                app.shell.channel_drafts.get_mut("recipient").unwrap()
            }
        }
        fn edit(session: bool, text: &str) -> Message {
            if session {
                Message::SessionDraft("recipient".into(), text.into())
            } else {
                Message::ChannelDraft(text.into())
            }
        }
        for session in [false, true] {
            let (mut app, _, messages) = app();
            app.shell.channel_target = Some("recipient".into());
            let _ = app.update(edit(session, "sent text"));
            assert_eq!(
                draft(&mut app, session).begin().as_deref(),
                Some("sent text")
            );
            let _ = app.update(edit(session, "changed text"));
            let _ = app.update(edit(session, "sent text"));
            let receipt = Ok(MessageId::from("receipt".to_owned()).into());
            messages
                .send(if session {
                    Msg::SessionSent("recipient".into(), receipt)
                } else {
                    Msg::ChannelSent("recipient".into(), receipt)
                })
                .unwrap();
            app.drain();
            let current = draft(&mut app, session);
            assert_eq!(
                current.text, "sent text",
                "new draft lost; session={session}"
            );
            // A subsequent untouched send still clears only its own draft.
            assert_eq!(current.begin().as_deref(), Some("sent text"));
            current.complete(Ok(()));
            assert!(current.text.is_empty());
        }
    }

    #[test]
    fn session_messages_keep_new_and_other_drafts_after_late_receipts_and_rejection() {
        let (mut app, commands, messages) = app();
        let mut agent = AgentRecord::new(
            agentdocker_core::AgentSpec {
                runtime: "fixture".into(),
                ..Default::default()
            },
            false,
            Utc::now(),
        );
        agent.id = "recipient".into();
        app.agents.push(agent);
        let _ = app.update(Message::SessionDraft(
            "recipient".into(),
            "first input".into(),
        ));
        let _ = app.update(Message::SendSession("recipient".into()));
        let _ = app.update(Message::SendSession("recipient".into()));
        assert!(matches!(commands.try_iter().collect::<Vec<_>>().as_slice(),
            [Cmd::SessionSend(id, text)] if id == "recipient" && text == "first input"));
        let _ = app.update(Message::SessionDraft(
            "recipient".into(),
            "next input".into(),
        ));
        let _ = app.update(Message::SessionDraft("other".into(), "other draft".into()));
        let _ = app.update(Message::SelectSession("other".into()));
        messages
            .send(Msg::SessionSent(
                "recipient".into(),
                Ok(MessageId::from("receipt".to_owned()).into()),
            ))
            .unwrap();
        app.drain();
        assert_eq!(
            app.shell.session_drafts["recipient"].draft.text,
            "next input"
        );
        assert_eq!(app.shell.session_drafts["other"].draft.text, "other draft");
        assert_eq!(
            app.shell.session_drafts["recipient"]
                .queued
                .as_ref()
                .unwrap()
                .as_str(),
            "receipt"
        );
        for _ in 0..queue::CAPACITY {
            app.tx.send(Cmd::Stop("fixture".into())).unwrap();
        }
        let _ = app.update(Message::SendSession("recipient".into()));
        let draft = &app.shell.session_drafts["recipient"].draft;
        assert_eq!(draft.text, "next input");
        assert!(draft.sending.is_none());
        assert!(draft.error.as_ref().unwrap().contains("full"));
        assert_eq!(commands.try_iter().count(), queue::CAPACITY);
        app.agents.clear();
        let _ = app.update(Message::SendSession("recipient".into()));
        assert_eq!(commands.try_iter().count(), 0);
    }

    #[test]
    fn answering_reveals_the_next_question_only_while_that_interaction_is_current() {
        let (mut app, commands, messages) = app();
        app.screen = Screen::Questions;
        app.connected = Ok(());
        let first = MessageId::from("first".to_owned());
        let second = MessageId::from("second".to_owned());
        let question = Question {
            presentation: None,
            id: first.clone(),
            from: "asker".into(),
            to: agentdocker_core::Destination::Agent("user".into()),
            text: "Continue?".into(),
            asked_at: Utc::now(),
            expires_at: Utc::now() + chrono::Duration::minutes(5),
        };
        app.questions = vec![
            question.clone(),
            Question {
                id: second.clone(),
                from: "next-asker".into(),
                ..question.clone()
            },
        ];
        app.shell.answers.insert(first.clone(), "Yes".into());
        app.shell
            .answers
            .insert(second.clone(), "Keep my draft".into());
        let _ = app.update(Message::Answer(first.clone()));
        assert!(
            matches!(commands.try_iter().collect::<Vec<_>>().as_slice(), [Cmd::Answer(id, _)] if id == &first)
        );
        messages.send(Msg::Answered(first.clone(), Ok(()))).unwrap();
        app.drain();
        assert_eq!(app.take_answer_reveal(), Some(second.clone()));
        assert_eq!(app.shell.inbox_thread.as_deref(), Some("next-asker"));
        assert!(app.take_answer_reveal().is_none());
        assert_eq!(app.shell.answers[&second], "Keep my draft");
        app.questions.insert(0, question);
        app.shell.answers.insert(first.clone(), "Yes".into());
        let _ = app.update(Message::Answer(first.clone()));
        let _ = app.update(Message::Draft(second.clone(), "Newer draft".into()));
        messages.send(Msg::Answered(first, Ok(()))).unwrap();
        app.drain();
        assert!(app.take_answer_reveal().is_none());
        assert_eq!(app.shell.answers[&second], "Newer draft");
    }

    #[test]
    fn returning_to_conversations_cancels_an_answer_reveal_before_or_after_its_response() {
        for response_before_back in [false, true] {
            let (mut app, commands, messages) = app();
            app.screen = Screen::Questions;
            app.connected = Ok(());
            app.shell.inbox_open = true;
            app.shell.inbox_thread = Some("first-asker".into());
            let first = MessageId::from("first".to_owned());
            let second = MessageId::from("second".to_owned());
            let question = Question {
                presentation: None,
                id: first.clone(),
                from: "first-asker".into(),
                to: agentdocker_core::Destination::Agent("user".into()),
                text: "Continue?".into(),
                asked_at: Utc::now(),
                expires_at: Utc::now() + chrono::Duration::minutes(5),
            };
            app.questions = vec![
                question.clone(),
                Question {
                    id: second.clone(),
                    from: "next-asker".into(),
                    ..question
                },
            ];
            app.shell.answers.insert(first.clone(), "Yes".into());
            app.shell
                .answers
                .insert(second.clone(), "Keep this draft".into());
            let _ = app.update(Message::Answer(first.clone()));
            assert!(matches!(commands.try_iter().collect::<Vec<_>>().as_slice(),
                [Cmd::Answer(id, _)] if id == &first));
            if response_before_back {
                messages.send(Msg::Answered(first.clone(), Ok(()))).unwrap();
                app.drain();
            }
            let _ = app.update(Message::InboxList);
            if !response_before_back {
                messages.send(Msg::Answered(first, Ok(()))).unwrap();
            }
            let _ = app.update(Message::Tick);
            assert!(!app.shell.inbox_open, "a late answer must not undo Back");
            assert_eq!(app.shell.inbox_thread.as_deref(), Some("first-asker"));
            assert!(app.shell.pending_answer_reveal.is_none());
            assert!(!app.shell.reveal_next_question);
            assert_eq!(app.shell.answers[&second], "Keep this draft");
        }
    }

    fn notification_app() -> (
        App,
        CommandReceiver,
        SyncSender<Msg>,
        tempfile::TempDir,
        agentdocker_host::notify::Action,
    ) {
        let (mut app, commands, messages) = app();
        let home = tempfile::tempdir().unwrap();
        let socket = home.path().join("agentd.sock");
        app.home = home.path().to_owned();
        app.client = Some(Arc::new(Client::isolated(socket.clone())));
        let action = agentdocker_host::notify::Action {
            home: app.home.clone(),
            socket,
            target: agentdocker_core::NotificationTarget {
                message: MessageId::from("question-1".to_owned()),
                agent: agentdocker_core::AgentId::from("sender-1"),
                project: None,
                channel: None,
            },
        };
        (app, commands, messages, home, action)
    }

    #[test]
    fn notification_waits_for_data_then_opens_the_question_without_submitting_drafts() {
        let (mut app, commands, messages, home, mut action) = notification_app();
        app.shell.inbox_thread = Some("another-agent".into());
        let project = crate::catalog::resolve(home.path()).unwrap();
        app.shell.catalog.remember(project.clone(), false);
        action.target.project = Some(project.id());
        let question = Question {
            presentation: None,
            id: action.target.message.clone(),
            from: action.target.agent.to_string(),
            to: agentdocker_core::Destination::Agent(agentdocker_core::AgentId::from("user")),
            text: "which option?".into(),
            asked_at: Utc::now(),
            expires_at: Utc::now() + chrono::Duration::minutes(5),
        };
        app.shell
            .answers
            .insert(question.id.clone(), "unfinished answer".into());
        app.shell.channel_drafts.insert(
            "another-room".into(),
            ChannelDraft {
                text: "unfinished channel message".into(),
                ..Default::default()
            },
        );
        let _ = app.update(Message::Notification(
            crate::notification_route::Activation::Open(action),
        ));
        assert!(app.shell.pending_notification.is_some());
        assert_eq!(app.screen, Screen::Agents);
        app.conversations_supported = Some(true);
        let conversation = agentdocker_core::ConversationId::dm("user", "sender-1").to_string();
        app.history.insert(conversation.clone(), Vec::new());
        app.history_complete.insert(conversation);
        messages
            .send(Msg::Questions(vec![question.clone()]))
            .unwrap();
        let _ = app.update(Message::Tick);
        assert_eq!(app.screen, Screen::Questions);
        assert_eq!(app.shell.inbox_thread.as_deref(), Some("sender-1"));
        assert_eq!(app.shell.catalog.selected.as_ref(), Some(&project.root));
        assert_eq!(app.shell.notification_message.as_ref(), Some(&question.id));
        assert!(app.shell.pending_notification.is_none());
        assert_eq!(app.shell.answers[&question.id], "unfinished answer");
        assert_eq!(
            app.shell.channel_drafts["another-room"].text,
            "unfinished channel message"
        );
        assert!(app.sending.is_empty());
        assert!(
            app.reveal_archived.is_none(),
            "a live question is not an archive lookup"
        );
        assert!(app.status.is_empty(), "no false missing-archive warning");
        assert!(commands.try_iter().all(|cmd| !matches!(
            cmd,
            Cmd::Answer(..)
                | Cmd::ChannelSend(..)
                | Cmd::Launch(..)
                | Cmd::Resume(..)
                | Cmd::Stop(..)
                | Cmd::HistoryBefore(..)
        )));
    }

    /// An archived message's notification opens its conversation even when
    /// its sender's record is gone and its channel is closed: the archive
    /// outlives both, and no ten-second wait ends in "no longer available".
    /// A reply typed into a notification that did not go opens the
    /// conversation the way a click does and puts the words in its
    /// composer — after what was already there, never over it — and says
    /// why; an unknown outcome says to read the history first. Another
    /// workspace's failure is only said.
    #[test]
    fn a_failed_notification_reply_comes_back_as_the_conversations_draft() {
        let (mut app, _commands, messages, _home, action) = notification_app();
        messages.send(Msg::Conversations(Ok(Vec::new()))).unwrap();
        messages.send(Msg::Inbox(Vec::new())).unwrap();
        messages.send(Msg::Questions(Vec::new())).unwrap();
        let conversation = agentdocker_core::ConversationId::dm("user", "sender-1")
            .as_str()
            .to_owned();
        app.shell.conversation_drafts.insert(
            conversation.clone(),
            ChannelDraft {
                text: "half typed".into(),
                ..Default::default()
            },
        );
        let _ = app.update(Message::Notification(
            crate::notification_route::Activation::ReplyFailed {
                action: action.clone(),
                text: "on it".into(),
                reason: "refused: recipient is paused".into(),
                certain: true,
            },
        ));
        let _ = app.update(Message::Tick);
        assert!(app.shell.reply_recoveries.is_empty(), "placed once opened");
        assert_eq!(
            app.shell.conversation.as_deref(),
            Some(conversation.as_str())
        );
        assert_eq!(
            app.shell.conversation_drafts[&conversation].text, "half typed\n\non it",
            "both drafts kept, the reply after"
        );
        assert!(
            app.status
                .contains("was not sent: refused: recipient is paused")
        );
        assert!(app.status.contains("in the composer"));

        let (mut app, _commands, messages, _home, action) = notification_app();
        messages.send(Msg::Conversations(Ok(Vec::new()))).unwrap();
        messages.send(Msg::Inbox(Vec::new())).unwrap();
        messages.send(Msg::Questions(Vec::new())).unwrap();
        let _ = app.update(Message::Notification(
            crate::notification_route::Activation::ReplyFailed {
                action,
                text: "still here".into(),
                reason: "agentd closed the connection without answering".into(),
                certain: false,
            },
        ));
        let _ = app.update(Message::Tick);
        assert_eq!(
            app.shell.conversation_drafts[&conversation].text,
            "still here"
        );
        assert!(app.status.contains("may not have been sent"));
        assert!(app.status.contains("Check the history"));

        let (mut app, _commands, _messages, _home, mut action) = notification_app();
        action.home = std::path::PathBuf::from("/elsewhere");
        action.socket = std::path::PathBuf::from("/elsewhere/agentd.sock");
        let _ = app.update(Message::Notification(
            crate::notification_route::Activation::ReplyFailed {
                action,
                text: "lost".into(),
                reason: "refused".into(),
                certain: true,
            },
        ));
        assert!(app.shell.reply_recoveries.is_empty());
        assert!(app.status.contains("another workspace"));

        // Draft storage full: the words are kept beside the composer to
        // copy or dismiss, not lost in a status line.
        let (mut app, _commands, messages, _home, action) = notification_app();
        messages.send(Msg::Conversations(Ok(Vec::new()))).unwrap();
        messages.send(Msg::Inbox(Vec::new())).unwrap();
        messages.send(Msg::Questions(Vec::new())).unwrap();
        for kind in [
            DraftKind::Session,
            DraftKind::Channel,
            DraftKind::Conversation,
        ] {
            for index in 0..crate::drafts::MAX_PER_KIND {
                app.shell
                    .edit_draft(kind, format!("filler-{index}"), "x".repeat(16_000));
            }
        }
        // To the last byte.
        let used: usize = {
            let snapshot = app.shell.draft_snapshot();
            snapshot
                .sessions
                .values()
                .chain(snapshot.conversations.values())
                .chain(snapshot.channels.values())
                .map(String::len)
                .sum()
        };
        app.shell.edit_draft(
            DraftKind::Conversation,
            "filler-room".into(),
            "x".repeat(crate::drafts::MAX_TOTAL_BYTES - used),
        );
        app.shell.error = None;
        let before = app.shell.draft_snapshot();
        let _ = app.update(Message::Notification(
            crate::notification_route::Activation::ReplyFailed {
                action: action.clone(),
                text: "kept words".into(),
                reason: "refused".into(),
                certain: true,
            },
        ));
        let _ = app.update(Message::Tick);
        assert_eq!(
            app.shell.draft_snapshot(),
            before,
            "no draft was made room for"
        );
        assert_eq!(app.shell.reply_recoveries.len(), 1);
        assert_eq!(
            app.shell.reply_recoveries[0].conversation.as_deref(),
            Some(conversation.as_str())
        );
        assert!(app.status.contains("kept beside the composer"));
        let _ = app.update(Message::ReplyRecoveryCopy(action.target.message.clone()));
        assert!(app.status.contains("clipboard"));
        assert_eq!(app.shell.reply_recoveries.len(), 1, "copying keeps it");
        let _ = app.update(Message::ReplyRecoveryDismiss(action.target.message.clone()));
        assert!(app.shell.reply_recoveries.is_empty());

        // The window keeps a bounded number; one more is said, not kept.
        let (mut app, _commands, _messages, _home, action) = notification_app();
        for index in 0..REPLY_RECOVERIES {
            let mut action = action.clone();
            action.target.message = MessageId::from(format!("m-{index}"));
            let _ = app.update(Message::Notification(
                crate::notification_route::Activation::ReplyFailed {
                    action,
                    text: format!("words {index}"),
                    reason: "refused".into(),
                    certain: true,
                },
            ));
        }
        assert_eq!(app.shell.reply_recoveries.len(), REPLY_RECOVERIES);
        let _ = app.update(Message::Notification(
            crate::notification_route::Activation::ReplyFailed {
                action,
                text: "one too many ".to_owned() + &"🦀".repeat(4000),
                reason: "connection lost".into(),
                certain: false,
            },
        ));
        assert_eq!(app.shell.reply_recoveries.len(), REPLY_RECOVERIES);
        assert!(app.status.contains("one too many"));
        assert!(app.status.contains("may not have been sent"));
        assert!(app.status.chars().count() < 512);

        // A route that gives up — the message is gone and no
        // conversation opens — leaves the words reachable at the top of
        // the Messages list, not bound to whatever conversation is on view.
        let (mut app, _commands, messages, _home, action) = notification_app();
        messages.send(Msg::Conversations(Ok(Vec::new()))).unwrap();
        messages.send(Msg::Inbox(Vec::new())).unwrap();
        messages.send(Msg::Questions(Vec::new())).unwrap();
        app.shell.reply_recoveries.push(ReplyRecovery {
            message: action.target.message.clone(),
            text: "orphaned words".into(),
            reason: "refused".into(),
            certain: true,
            conversation: None,
        });
        app.shell.pending_notification = Some((action.clone(), Instant::now()));
        assert!(
            app.shell.orphan_reply_recoveries().is_empty(),
            "still on its way to its conversation"
        );
        app.shell.pending_notification = None;
        app.shell.notification_message = None;
        let orphans = app.shell.orphan_reply_recoveries();
        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans[0].text, "orphaned words");
        let _ = app.update(Message::ReplyRecoveryDismiss(action.target.message));
        assert!(app.shell.orphan_reply_recoveries().is_empty());
    }

    #[test]
    fn an_archived_notification_opens_its_conversation_without_a_live_sender_or_channel() {
        // A direct message from a sender nobody has a record of.
        let (mut app, commands, messages, _home, action) = notification_app();
        messages.send(Msg::Conversations(Ok(Vec::new()))).unwrap();
        messages.send(Msg::Inbox(Vec::new())).unwrap();
        messages.send(Msg::Questions(Vec::new())).unwrap();
        let _ = app.update(Message::Notification(
            crate::notification_route::Activation::Open(action),
        ));
        let _ = app.update(Message::Tick);
        assert!(app.shell.pending_notification.is_none(), "routed at once");
        assert_eq!(app.screen, Screen::Questions);
        assert!(app.shell.inbox_open);
        assert_eq!(
            app.shell.conversation.as_deref(),
            Some(agentdocker_core::ConversationId::dm("user", "sender-1").as_str())
        );
        assert!(
            commands
                .try_iter()
                .any(|cmd| matches!(cmd, Cmd::History(ref c, _) if c.starts_with("dm:"))),
            "the conversation's archive is asked for"
        );
        assert!(app.status.is_empty(), "nothing said to be unavailable");

        // A sender retired into another record: its archive sits under the
        // id it had, and that is the conversation opened.
        let (mut app, _commands, messages, _home, action) = notification_app();
        app.aliases
            .insert("sender-1".to_owned(), "sender-now".to_owned());
        messages.send(Msg::Conversations(Ok(Vec::new()))).unwrap();
        messages.send(Msg::Inbox(Vec::new())).unwrap();
        messages.send(Msg::Questions(Vec::new())).unwrap();
        let _ = app.update(Message::Notification(
            crate::notification_route::Activation::Open(action),
        ));
        let _ = app.update(Message::Tick);
        assert_eq!(app.shell.selected.as_deref(), Some("sender-now"));
        assert_eq!(
            app.shell.conversation.as_deref(),
            Some(agentdocker_core::ConversationId::dm("user", "sender-1").as_str()),
            "the archived conversation, not the current record's"
        );

        // A channel message whose channel is no longer open.
        let (mut app, commands, messages, _home, mut action) = notification_app();
        action.target.channel = Some(agentdocker_core::ChannelId::from("closed-room".to_owned()));
        messages.send(Msg::Conversations(Ok(Vec::new()))).unwrap();
        messages.send(Msg::Inbox(Vec::new())).unwrap();
        messages.send(Msg::Questions(Vec::new())).unwrap();
        let _ = app.update(Message::Notification(
            crate::notification_route::Activation::Open(action),
        ));
        let _ = app.update(Message::Tick);
        assert!(app.shell.pending_notification.is_none(), "routed at once");
        assert_eq!(app.screen, Screen::Questions);
        assert_eq!(
            app.shell.conversation.as_deref(),
            Some("channel:closed-room")
        );
        assert!(
            commands
                .try_iter()
                .any(|cmd| matches!(cmd, Cmd::History(ref c, _) if c == "channel:closed-room"))
        );
        assert!(app.status.is_empty());
    }

    #[test]
    fn former_identity_notification_finds_the_current_sender_without_changing_drafts() {
        let (mut app, _, messages, _home, action) = notification_app();
        let mut record = agentdocker_core::AgentRecord::new(
            agentdocker_core::AgentSpec {
                name: "one-session".into(),
                ..Default::default()
            },
            false,
            Utc::now(),
        );
        record.id = "canonical".into();
        messages
            .send(Msg::Agents(
                vec![record],
                BTreeMap::from([(action.target.agent.to_string(), "canonical".into())]),
            ))
            .unwrap();
        messages
            .send(Msg::Questions(vec![Question {
                presentation: None,
                id: action.target.message.clone(),
                from: "canonical".into(),
                to: agentdocker_core::Destination::Agent("user".into()),
                text: "question".into(),
                asked_at: Utc::now(),
                expires_at: Utc::now() + chrono::Duration::minutes(5),
            }]))
            .unwrap();
        app.shell
            .answers
            .insert(action.target.message.clone(), "unfinished".into());
        let _ = app.update(Message::Notification(
            crate::notification_route::Activation::Open(action.clone()),
        ));
        let _ = app.update(Message::Tick);
        assert_eq!(app.screen, Screen::Questions);
        assert_eq!(app.shell.selected.as_deref(), Some("canonical"));
        assert_eq!(app.name_of(action.target.agent.as_str()), "one-session");
        assert_eq!(
            app.shell.notification_message.as_ref(),
            Some(&action.target.message)
        );
        assert_eq!(app.shell.answers[&action.target.message], "unfinished");
        assert!(app.sending.is_empty());
    }

    /// The Earlier groups (ended sessions, earlier conversations) open on a
    /// page of entries and grow a page per *Show older*; closing a group
    /// forgets how far it was opened, and the temporary projects fold
    /// opens and closes on its own toggle.
    #[test]
    fn earlier_groups_page_and_the_temporary_fold_toggles() {
        let (mut app, _commands, _) = app();
        assert!(!app.shell.earlier_open);
        let _ = app.update(Message::ToggleEarlier);
        assert!(app.shell.earlier_open);
        assert_eq!(app.shell.earlier_shown, EARLIER_PAGE);
        let _ = app.update(Message::MoreEarlier);
        let _ = app.update(Message::MoreEarlier);
        assert_eq!(app.shell.earlier_shown, 3 * EARLIER_PAGE);
        let _ = app.update(Message::ToggleEarlier);
        assert!(!app.shell.earlier_open);
        let _ = app.update(Message::ToggleEarlier);
        assert_eq!(app.shell.earlier_shown, EARLIER_PAGE, "reopened at a page");
        // The temporary fold is automatic until toggled: closed with
        // nothing running in a scratch project, and a toggle is the
        // person's choice from then on — closable even while one runs.
        assert_eq!(app.shell.temporary_open, None);
        assert!(!app.temporary_fold_open());
        let _ = app.update(Message::ToggleTemporary);
        assert_eq!(app.shell.temporary_open, Some(true));
        assert!(app.temporary_fold_open());
        let mut scratch = AgentRecord::new(
            agentdocker_core::AgentSpec {
                name: "claude-code-77".into(),
                runtime: "claude-code".into(),
                workdir: Some("/private/tmp/fixture/workspace".into()),
                ..Default::default()
            },
            false,
            Utc::now(),
        );
        scratch.id = "scratch-live".into();
        scratch.status = agentdocker_core::AgentStatus::Running;
        scratch.project = Some(agentdocker_core::ProjectRef::directory(
            "/private/tmp/fixture/workspace",
        ));
        app.agents.push(scratch);
        app.shell.catalog.remember(
            agentdocker_core::ProjectRef::directory("/private/tmp/fixture/workspace"),
            false,
        );
        let _ = app.update(Message::ToggleTemporary);
        assert_eq!(app.shell.temporary_open, Some(false));
        assert!(
            !app.temporary_fold_open(),
            "closable while a scratch session runs"
        );
        app.shell.temporary_open = None;
        assert!(app.temporary_fold_open(), "automatic: open while one runs");
    }

    /// Reconnecting an ended Claude Code session launches its own tool
    /// with `--resume` and its conversation, in its folder, with the
    /// AgentDocker channel, under its name; a live process, another
    /// runtime, a missing conversation id or a missing tool is refused
    /// with the reason, and nothing is launched.
    #[test]
    fn reconnect_here_relaunches_an_ended_claude_session_with_its_conversation_and_the_channel() {
        let (mut app, commands, messages) = app();
        app.connected = Ok(());
        let cli = tempfile::NamedTempFile::new().unwrap();
        let cli_path = cli.path().to_path_buf();
        app.runtimes = vec![agentdocker_core::runtime::RuntimeInfo {
            name: "claude-code".into(),
            vendor: "fixture".into(),
            label: "Claude Code".into(),
            cli: Some("/fixture/claude".into()),
            version: None,
            apps: vec![],
            extensions: vec![],
            incomplete: vec![],
            config_dir: None,
            mcp: agentdocker_core::runtime::Wiring::Missing,
            hooks: agentdocker_core::runtime::Wiring::Missing,
            hooks_missing: vec![],
            shell: agentdocker_core::runtime::Wiring::Unsupported,
            running: 0,
        }];
        let mut agent = AgentRecord::new(
            agentdocker_core::AgentSpec {
                name: "claude-code-4242".into(),
                runtime: "claude-code".into(),
                workdir: Some("/work/repo".into()),
                labels: BTreeMap::from([(
                    "session_id".to_owned(),
                    "218845eb-ba1e-4457-bb5a-e1829f5652dd".to_owned(),
                )]),
                ..Default::default()
            },
            false,
            Utc::now(),
        );
        agent.id = "ended-claude".into();
        agent.status = agentdocker_core::AgentStatus::Exited { code: Some(0) };
        app.agents.push(agent.clone());
        let spec = app
            .reconnect_spec("ended-claude", Ok(cli_path.clone()))
            .unwrap();
        assert_eq!(spec.name, "claude-code-4242");
        assert_eq!(spec.runtime, "claude-code");
        assert_eq!(
            spec.workdir.as_deref(),
            Some(std::path::Path::new("/work/repo"))
        );
        assert!(spec.tty);
        assert_eq!(spec.command[0], "/fixture/claude");
        assert!(
            spec.command
                .windows(2)
                .any(|w| w[0] == "--resume" && w[1] == "218845eb-ba1e-4457-bb5a-e1829f5652dd"),
            "{:?}",
            spec.command
        );
        assert!(
            spec.command
                .windows(2)
                .any(|w| w[0] == "--dangerously-load-development-channels"
                    && w[1] == "server:agentdocker")
        );
        assert_eq!(
            spec.env
                .get(agentdocker_host::provider_input::CLAUDE_CHANNEL_ENV),
            Some(&"1".to_owned())
        );
        // Through the message the app's own sibling CLI is required; a
        // test binary has none, and that is said rather than launched.
        let _ = app.update(Message::Reconnect("ended-claude".into()));
        assert!(!app.shell.launching);
        assert!(
            app.shell
                .error
                .as_deref()
                .is_some_and(|e| e.contains("live messages")),
            "{:?}",
            app.shell.error
        );
        assert_eq!(commands.try_iter().count(), 0);

        // With the CLI beside the app the press is one resume of that
        // record at the daemon, and a second press while it is unanswered
        // is nothing: the button waits for the answer.
        app.reconnect("ended-claude", Ok(cli_path.clone()));
        assert!(app.shell.launching);
        assert_eq!(app.shell.reconnecting.as_deref(), Some("ended-claude"));
        assert_eq!(app.shell.error, None);
        app.reconnect("ended-claude", Ok(cli_path.clone()));
        let sent: Vec<Cmd> = commands.try_iter().collect();
        assert_eq!(sent.len(), 1, "{sent:?}");
        match &sent[0] {
            Cmd::Resume(id, spec) => {
                assert_eq!(id, "ended-claude");
                assert_eq!(spec.name, "claude-code-4242");
                assert!(spec.command.windows(2).any(|w| w[0] == "--resume"));
            }
            other => panic!("a reconnect is a resume, not {other:?}"),
        }
        // The list was stale: the daemon saw the process back, or the
        // checkout moved. Its refusal is the error under the button, and
        // the button is pressable again.
        messages
            .send(Msg::Reconnected(Err(
                "the session is still live; a live session is not relaunched".into(),
            )))
            .unwrap();
        app.drain();
        assert!(!app.shell.launching);
        assert_eq!(
            app.shell.reconnecting, None,
            "the button says Reconnect here again"
        );
        assert!(
            app.shell
                .error
                .as_deref()
                .is_some_and(|e| e.contains("still live"))
        );
        // The daemon's yes selects the same record, and asks for the list.
        let generation = Some(Utc::now());
        app.reconnect("ended-claude", Ok(cli_path.clone()));
        assert_eq!(commands.try_iter().count(), 1);
        messages
            .send(Msg::Reconnected(Ok(("ended-claude".into(), generation))))
            .unwrap();
        app.drain();
        assert!(!app.shell.launching);
        assert_eq!(app.shell.error, None);
        assert_eq!(app.shell.selected.as_deref(), Some("ended-claude"));
        assert!(commands.try_iter().any(|cmd| matches!(cmd, Cmd::Agents)));
        // An older list, from before the reconnect, says nothing: the
        // request waits. The list that shows the process the daemon
        // started opens the pane when it runs; when it ended at once the
        // session stays in the list with its exit and no pane opens.
        messages
            .send(Msg::Agents(vec![agent.clone()], BTreeMap::new()))
            .unwrap();
        app.drain();
        assert_ne!(app.screen, Screen::Terminal);
        assert!(app.shell.attach_when_listed.is_some());
        let mut listed = agent.clone();
        listed.managed = true;
        listed.spec.tty = true;
        listed.process_started_at = generation;
        listed.status = agentdocker_core::AgentStatus::Exited { code: Some(1) };
        messages
            .send(Msg::Agents(vec![listed.clone()], BTreeMap::new()))
            .unwrap();
        app.drain();
        assert_ne!(app.screen, Screen::Terminal);
        assert_eq!(app.shell.attach_when_listed, None);
        app.reconnect("ended-claude", Ok(cli_path.clone()));
        messages
            .send(Msg::Reconnected(Ok(("ended-claude".into(), generation))))
            .unwrap();
        app.drain();
        listed.status = agentdocker_core::AgentStatus::Running;
        messages
            .send(Msg::Agents(vec![listed], BTreeMap::new()))
            .unwrap();
        app.drain();
        assert_eq!(app.screen, Screen::Terminal);
        assert_eq!(app.shell.attach_when_listed, None);
        commands.try_iter().count();
        app.screen = Screen::Agents;

        // A live process is refused with the reason, and nothing launches.
        app.agents[0].status = agentdocker_core::AgentStatus::Running;
        assert_eq!(
            app.reconnect_blocker(&app.agents[0]),
            Some("Exit the session in its terminal first (/exit); this enables when it has.")
        );
        let _ = app.update(Message::Reconnect("ended-claude".into()));
        assert!(!app.shell.launching);
        assert!(
            app.shell
                .error
                .as_deref()
                .is_some_and(|e| e.contains("Exit the session"))
        );
        assert_eq!(commands.try_iter().count(), 0);

        // Another runtime, no conversation id, no installed tool.
        app.agents[0].status = agentdocker_core::AgentStatus::Exited { code: Some(0) };
        app.agents[0].spec.runtime = "codex".into();
        assert!(app.reconnect_blocker(&app.agents[0]).is_some());
        app.agents[0].spec.runtime = "claude-code".into();
        app.agents[0].spec.labels.clear();
        assert!(app.reconnect_blocker(&app.agents[0]).is_some());
        app.agents[0]
            .spec
            .labels
            .insert("session_id".into(), "x".into());
        app.runtimes.clear();
        assert!(app.reconnect_blocker(&app.agents[0]).is_some());
    }

    #[test]
    fn default_ui_launches_show_tool_names_while_chosen_names_stay_literal() {
        let (mut app, _, _) = app();
        app.shell
            .catalog
            .pin(ProjectRef::directory("/fixture"))
            .unwrap();
        for runtime in ["codex", "claude-code"] {
            app.runtimes = vec![agentdocker_core::runtime::RuntimeInfo {
                name: runtime.into(),
                vendor: "fixture".into(),
                label: runtime_label(runtime),
                cli: Some("/fixture/provider".into()),
                version: None,
                apps: vec![],
                extensions: vec![],
                incomplete: vec![],
                config_dir: None,
                mcp: agentdocker_core::runtime::Wiring::Missing,
                hooks: agentdocker_core::runtime::Wiring::Missing,
                hooks_missing: vec![],
                shell: agentdocker_core::runtime::Wiring::Unsupported,
                running: 0,
            }];
            app.shell.launch_runtime = Some(runtime.into());
            app.shell.launch_name.clear();
            let generated = AgentRecord::new(app.launch_spec().unwrap(), true, Utc::now());
            assert_eq!(app.display_name(&generated), runtime_label(runtime));
            app.shell.launch_name = "reviewer-cafe".into();
            let chosen = AgentRecord::new(app.launch_spec().unwrap(), true, Utc::now());
            assert_eq!(app.display_name(&chosen), "reviewer-cafe");
            assert!(!chosen.name_is_generated());
        }
    }

    #[test]
    fn connection_guidance_preserves_conversation_and_thread_drafts() {
        let (mut app, commands, _) = app();
        let conversation = "dm:user:worker";
        let root = MessageId::from("root".to_owned());
        let thread_key = super::draft_key(conversation, Some(&root));
        app.shell.conversation = Some(conversation.into());
        app.shell.thread = Some(root);
        for (key, text) in [
            (conversation, "Conversation draft"),
            (thread_key.as_str(), "Thread draft"),
        ] {
            app.shell.conversation_drafts.insert(
                key.into(),
                ChannelDraft {
                    text: text.into(),
                    ..Default::default()
                },
            );
        }
        let _ = app.update(Message::OpenConnection("claude-code".into()));
        assert!(matches!(commands.try_iter().next(), Some(Cmd::Runtimes)));
        assert_eq!(app.screen, Screen::Runtimes);
        assert_eq!(app.shell.connection_details.as_deref(), Some("claude-code"));
        assert_eq!(app.shell.conversation.as_deref(), Some(conversation));
        assert_eq!(app.shell.thread.as_ref().unwrap().as_str(), "root");
        assert_eq!(
            app.shell.conversation_drafts[conversation].text,
            "Conversation draft"
        );
        assert_eq!(
            app.shell.conversation_drafts[&thread_key].text,
            "Thread draft"
        );
        assert!(!app.shell.launching);
    }

    #[test]
    fn input_launch_selects_the_provider_adapter_and_needs_the_sibling_cli() {
        use agentdocker_core::AgentSpec;
        let (mut app, _, _) = app();
        let directory = tempfile::tempdir().unwrap();
        let cli = directory.path().join("agentdocker");
        std::fs::write(&cli, b"fixture").unwrap();
        let spec = |runtime: &str| AgentSpec {
            name: "s".into(),
            runtime: runtime.into(),
            command: vec![runtime.into(), "--model".into(), "m".into()],
            tty: true,
            ..Default::default()
        };

        // Unticked: every runtime passes through, whatever the CLI situation.
        for runtime in ["claude-code", "codex"] {
            assert_eq!(
                app.prepare_launch(spec(runtime), Err("no cli".into())),
                Ok(spec(runtime))
            );
        }

        let _ = app.update(Message::LaunchChannel(true));
        // Unsupported runtimes pass through; Codex selects its own adapter.
        assert_eq!(
            app.prepare_launch(spec("custom"), Ok(cli.clone())),
            Ok(spec("custom"))
        );
        let codex = app.prepare_launch(spec("codex"), Ok(cli.clone())).unwrap();
        assert_eq!(&codex.command[1..4], ["codex-input", "--", "codex"]);
        assert_eq!(
            codex.env[agentdocker_host::provider_input::CODEX_INPUT_ENV],
            "1"
        );
        assert!(
            !codex
                .env
                .contains_key(agentdocker_host::provider_input::CLAUDE_CHANNEL_ENV)
        );
        // Claude gains the channel, after the user's own arguments are kept.
        let launched = app
            .prepare_launch(spec("claude-code"), Ok(cli.clone()))
            .unwrap();
        assert!(launched.command.contains(&"--mcp-config".to_owned()));
        assert!(
            launched
                .command
                .contains(&"--dangerously-load-development-channels".to_owned())
        );
        assert_eq!(
            &launched.command[launched.command.len() - 2..],
            ["--model", "m"]
        );
        assert_eq!(
            launched.env[agentdocker_host::provider_input::CLAUDE_CHANNEL_ENV],
            "1"
        );
        // Without the sibling CLI the launch fails instead of falling back.
        let error = app
            .prepare_launch(spec("claude-code"), Err("missing".into()))
            .unwrap_err();
        assert_eq!(error, "missing");
        // Every catalog runtime and an unknown one must get an explicit,
        // accurate default; switching providers never inherits another route.
        for runtime in agentdocker_core::runtime::RUNTIMES
            .iter()
            .map(|runtime| runtime.name)
            .chain(["custom"])
        {
            let supported = matches!(runtime, "claude-code" | "codex");
            let _ = app.update(Message::LaunchRuntime(runtime.into()));
            assert_eq!(app.shell.launch_channel, supported, "{runtime}");
            let _ = app.update(Message::LaunchChannel(false));
            assert_eq!(
                app.prepare_launch(spec(runtime), Err("no cli".into())),
                Ok(spec(runtime))
            );
            let _ = app.update(Message::ShowLaunch);
            assert_eq!(app.shell.launch_channel, supported, "reopen {runtime}");
        }
    }

    #[test]
    fn home_navigation_cancels_pending_reveals_and_preserves_answer_drafts() {
        let (mut app, commands, _) = app();
        let id = MessageId::from("target".to_owned());
        let other = MessageId::from("other".to_owned());
        app.questions.push(Question {
            presentation: None,
            id: id.clone(),
            from: "asker".into(),
            to: agentdocker_core::Destination::Agent("user".into()),
            text: "Review this exact question".into(),
            asked_at: Utc::now(),
            expires_at: Utc::now() + chrono::Duration::minutes(5),
        });
        app.shell.answers.insert(id.clone(), "target draft".into());
        app.shell
            .answers
            .insert(other.clone(), "other draft".into());
        app.shell.pending_answer_reveal = Some(other.clone());
        app.shell.reveal_next_question = true;
        app.shell.inbox_thread = Some("another-agent".into());
        let _ = app.update(Message::OpenQuestion(id.clone()));
        assert_eq!(app.screen, Screen::Questions);
        assert_eq!(app.shell.message_detail, Some(id.clone()));
        assert_eq!(app.shell.inbox_thread.as_deref(), Some("asker"));
        assert!(
            app.shell.inbox_open,
            "a notification opens the conversation, narrow or wide"
        );
        assert!(app.shell.pending_answer_reveal.is_none());
        assert!(!app.shell.reveal_next_question);
        assert_eq!(app.shell.answers[&id], "target draft");
        assert_eq!(app.shell.answers[&other], "other draft");
        assert_eq!(
            commands.try_iter().count(),
            0,
            "opening a question never answers it"
        );
        let _ = app.update(Message::AllProjects);
        let _ = app.update(Message::ToggleNeedsYou);
        assert!(app.shell.needs_you_expanded);
        let _ = app.update(Message::AllProjects);
        assert!(!app.shell.needs_you_expanded);
        let _ = app.update(Message::OpenSession("removed-session".into()));
        assert!(app.all_projects());
        assert!(app.shell.selected.is_none());
        assert_eq!(app.screen, Screen::Agents);
        app.questions.clear();
        let _ = app.update(Message::OpenQuestion(id.clone()));
        assert_eq!(app.shell.answers[&id], "target draft");
        assert_eq!(app.screen, Screen::Questions);
    }

    #[test]
    fn the_home_view_shows_every_project_until_one_is_chosen() {
        use agentdocker_core::AgentSpec;
        let (mut app, _, _) = app();
        let mut agents = Vec::new();
        for (name, root) in [
            ("a-worker", "/fixture/alpha"),
            ("b-worker", "/fixture/beta"),
        ] {
            let mut agent = AgentRecord::new(
                AgentSpec {
                    name: name.into(),
                    ..Default::default()
                },
                false,
                Utc::now(),
            );
            agent.status = agentdocker_core::AgentStatus::Running;
            let mut project = ProjectRef::directory(root);
            project.fingerprint = Some(name.into());
            agent.project = Some(project.clone());
            app.shell.catalog.remember(project, false);
            agents.push(agent);
        }
        let mut homeless = AgentRecord::new(
            AgentSpec {
                name: "nowhere".into(),
                ..Default::default()
            },
            false,
            Utc::now(),
        );
        homeless.status = agentdocker_core::AgentStatus::Running;
        agents.push(homeless.clone());
        app.agents = agents.clone();

        // Home: everything, projectless included.
        let _ = app.update(Message::AllProjects);
        assert!(app.all_projects());
        assert_eq!(app.shell.catalog.selected, None);
        assert_eq!(
            app.session_records(sessions::Filter::Current).len(),
            3,
            "the home view lists every live agent"
        );
        // A project narrows it.
        let _ = app.update(Message::SelectProject("/fixture/alpha".into()));
        assert!(!app.all_projects());
        let names: Vec<_> = app
            .session_records(sessions::Filter::Current)
            .iter()
            .map(|a| a.spec.name.clone())
            .collect();
        assert_eq!(names, ["a-worker"]);
        // Other sessions: only the projectless.
        let _ = app.update(Message::Unassigned);
        assert!(!app.all_projects());
        let names: Vec<_> = app
            .session_records(sessions::Filter::Current)
            .iter()
            .map(|a| a.spec.name.clone())
            .collect();
        assert_eq!(names, ["nowhere"]);
        // Opening a session from anywhere lands in its project with it selected.
        let _ = app.update(Message::OpenSession(agents[1].id.to_string()));
        assert_eq!(
            app.shell.catalog.selected.as_deref(),
            Some(std::path::Path::new("/fixture/beta"))
        );
        assert_eq!(app.shell.selected.as_deref(), Some(agents[1].id.as_str()));
        assert_eq!(app.screen, Screen::Agents);
        let _ = app.update(Message::OpenSession(homeless.id.to_string()));
        assert!(app.shell.catalog.unassigned);
        assert_eq!(app.shell.selected.as_deref(), Some(homeless.id.as_str()));
    }

    #[test]
    fn a_new_notification_cancels_the_previous_reveal_before_its_data_arrives() {
        let (mut app, commands, messages, _home, mut action) = notification_app();
        app.conversations_supported = Some(true);
        app.screen = Screen::Questions;
        let conversation = "channel:previous".to_owned();
        app.shell.conversation = Some(conversation.clone());
        let old = MessageId::from("previous-message".to_owned());
        app.reveal_archived = Some(super::Seek {
            conversation: conversation.clone(),
            message: old.clone(),
            pages: 1,
            before: Some(801),
            refresh_pending: false,
        });
        app.shell.reveal_archived_next = Some(old.clone());
        let _ = app.update(Message::Notification(
            crate::notification_route::Activation::Focus,
        ));
        assert!(app.reveal_archived.is_some(), "focusing is not navigation");
        let mut foreign = action.clone();
        foreign.socket = foreign.socket.with_file_name("another-daemon.sock");
        let _ = app.update(Message::Notification(
            crate::notification_route::Activation::Open(foreign),
        ));
        assert!(
            app.reveal_archived.is_some(),
            "a different workspace is not navigation"
        );
        // The new target must wait for a project snapshot. It still ends
        // the previous click's search and deferred scroll immediately.
        action.target.project = Some(ProjectRef::directory("/not-loaded-yet").id());
        let _ = app.update(Message::Notification(
            crate::notification_route::Activation::Open(action),
        ));
        assert!(app.shell.pending_notification.is_some());
        assert!(app.reveal_archived.is_none());
        assert!(app.shell.reveal_archived_next.is_none());
        let mut envelope = agentdocker_core::Envelope::new(
            "sender",
            agentdocker_core::Destination::Broadcast,
            "chat",
            serde_json::json!({"text": "old page arrives late"}),
            None,
            Utc::now(),
        );
        envelope.id = old;
        messages
            .send(Msg::HistoryEarlier(
                conversation.clone(),
                app.history_epoch,
                vec![agentdocker_core::ArchivedMessage {
                    seq: 700,
                    conversation: agentdocker_core::ConversationId::from(conversation),
                    envelope,
                    replies: 0,
                }],
            ))
            .unwrap();
        app.drain();
        assert!(app.shell.pending_notification.is_some());
        assert!(app.reveal_archived.is_none());
        assert!(app.shell.reveal_archived_next.is_none());
        assert!(
            !commands
                .try_iter()
                .any(|c| matches!(c, Cmd::HistoryBefore(..)))
        );
    }

    #[test]
    fn notification_cannot_hijack_manual_navigation_or_a_different_daemon() {
        let (mut app, _, _, _home, action) = notification_app();
        let _ = app.update(Message::Notification(
            crate::notification_route::Activation::Open(action.clone()),
        ));
        assert!(app.shell.pending_notification.is_some());
        let _ = app.update(Message::Navigate(Screen::Settings));
        assert!(app.shell.pending_notification.is_none());
        for navigation in [
            Message::AllProjects,
            Message::OpenSession("removed".into()),
            Message::OpenQuestion(MessageId::from("removed".to_owned())),
        ] {
            app.shell.pending_notification = Some((action.clone(), Instant::now()));
            let _ = app.update(navigation);
            assert!(app.shell.pending_notification.is_none());
        }
        app.shell.pending_notification = Some((action.clone(), Instant::now()));
        app.shell.notification_message = Some(MessageId::from("cancelled".to_owned()));
        let key = keyboard::Key::Named(keyboard::key::Named::Escape);
        let _ = app.update(Message::Event(iced::Event::Keyboard(
            keyboard::Event::KeyPressed {
                modified_key: key.clone(),
                key,
                physical_key: keyboard::key::Physical::Unidentified(
                    keyboard::key::NativeCode::Unidentified,
                ),
                location: keyboard::Location::Standard,
                modifiers: keyboard::Modifiers::empty(),
                text: None,
                repeat: false,
            },
        )));
        assert!(app.shell.pending_notification.is_none());
        assert!(app.shell.notification_message.is_none());
        let _ = app.update(Message::Navigate(Screen::Settings));
        let mut foreign = action.clone();
        foreign.socket = foreign.socket.with_file_name("another-daemon.sock");
        let _ = app.update(Message::Notification(
            crate::notification_route::Activation::Open(foreign),
        ));
        assert_eq!(app.screen, Screen::Settings);
        assert!(app.status.contains("another local workspace"));
        app.shell.pending_notification = Some((action, Instant::now() - Duration::from_secs(11)));
        let _ = app.advance_notification();
        assert_eq!(app.screen, Screen::Questions);
        assert!(app.status.contains("no longer available"));
        assert!(app.shell.notification_message.is_none());
    }
    #[test]
    fn adding_an_existing_folder_only_pins_and_reads_context() {
        let folder = tempfile::tempdir().unwrap();
        let (mut app, commands, _) = app();
        let project = crate::catalog::resolve(folder.path()).unwrap();
        let _ = app.update(Message::FolderResolved(Ok(project.clone())));
        assert!(app.shell.catalog.selected().unwrap().pinned);
        assert_eq!(app.shell.catalog.selected.as_ref(), Some(&project.root));
        assert_eq!(app.screen, Screen::Chat);
        assert_eq!(
            app.shell.conversation,
            Some(format!("everyone:{}", project.id()))
        );
        assert!(commands.try_iter().all(|cmd| matches!(
            cmd,
            Cmd::Journal(_, _) | Cmd::Channels(_, _) | Cmd::Conversations(_) | Cmd::History(_, _)
        )));
        assert_eq!(std::fs::read_dir(folder.path()).unwrap().count(), 0);
    }
    #[test]
    fn late_channel_acknowledgements_preserve_new_text_and_other_rooms() {
        let mut first = ChannelDraft {
            text: "sent text".into(),
            ..Default::default()
        };
        assert_eq!(first.begin().as_deref(), Some("sent text"));
        assert!(first.begin().is_none());
        first.text = "next thought".into();
        first.complete(Ok(()));
        assert_eq!(first.text, "next thought");
        assert_eq!(first.begin().as_deref(), Some("next thought"));
        first.complete(Err("connection lost".into()));
        assert_eq!(first.text, "next thought");
        assert_eq!(first.error.as_deref(), Some("connection lost"));
        let (mut app, _, messages) = app();
        app.shell.channel_drafts.insert("one".into(), first);
        app.shell.channel_drafts.insert(
            "two".into(),
            ChannelDraft {
                text: "other room".into(),
                ..Default::default()
            },
        );
        app.shell.channel_drafts.get_mut("one").unwrap().begin();
        messages
            .send(Msg::ChannelSent(
                "one".into(),
                Ok(MessageId::from("confirmed".to_owned()).into()),
            ))
            .unwrap();
        app.drain();
        assert!(app.shell.channel_drafts["one"].text.is_empty());
        assert_eq!(app.shell.channel_drafts["two"].text, "other room");
        assert_eq!(app.sent_channels.back().unwrap().payload, "next thought");
    }
    #[test]
    fn commands_capture_project_context_and_a_late_reply_does_not_navigate() {
        let folder = tempfile::tempdir().unwrap();
        let (mut app, commands, messages) = app();
        app.shell
            .catalog
            .pin(crate::catalog::resolve(folder.path()).unwrap())
            .unwrap();
        let selected = app.shell.catalog.selected.clone();
        let _ = app.update(Message::ConsoleInput("status".into()));
        let _ = app.update(Message::RunConsole);
        let queued = commands
            .try_iter()
            .find(|cmd| matches!(cmd, Cmd::Console(_, _)))
            .unwrap();
        assert!(matches!(queued, Cmd::Console(line, cwd) if line == "status" && cwd == selected));
        let _ = app.update(Message::Navigate(Screen::Settings));
        messages.send(Msg::Console("finished".into())).unwrap();
        app.drain();
        assert_eq!(app.screen, Screen::Settings);
        assert_eq!(app.console_running, 0);
    }
    #[test]
    fn quiet_project_channels_remain_queryable_and_stale_folder_checks_are_ignored() {
        let folder = tempfile::tempdir().unwrap();
        let (mut app, _, _) = app();
        let project = crate::catalog::resolve(folder.path()).unwrap();
        let id = project.id().to_string();
        app.shell.catalog.pin(project).unwrap();
        assert!(app.agents.is_empty());
        assert!(app.channel_projects().contains(&id));
        let _ = app.update(Message::ProjectLocation(
            PathBuf::from("/some/other/project"),
            false,
        ));
        assert_ne!(app.shell.project_available, Some(false));
    }
    #[test]
    fn failed_preferences_load_never_overwrites_the_existing_file() {
        use std::io::Write;
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("workspace.json");
        agentdocker_host::dirs::private_file(&path, true, false)
            .unwrap()
            .write_all(b"not JSON")
            .unwrap();
        let state = State::load(home.path());
        assert!(!state.save_enabled);
        assert!(state.error.unwrap().contains("preserved"));
        assert_eq!(std::fs::read(path).unwrap(), b"not JSON");
    }
}
