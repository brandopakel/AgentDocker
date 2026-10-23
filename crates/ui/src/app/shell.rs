//! Iced state transitions. Effects stay on bounded workers or explicit tasks.
use super::*;
use crate::catalog::Catalog;
use iced::{Subscription, Task, keyboard, window};
use std::path::PathBuf;

/// The board's filter toggles: high priority, waiting on something,
/// waiting for the person, nobody holding it, an agent holding it, and the
/// person's.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct BoardFilters {
    pub high: bool,
    pub blocked: bool,
    pub waiting: bool,
    pub unassigned: bool,
    /// Held by an agent.
    pub agent: bool,
    /// Held by the person or by an agent they launched.
    pub mine: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoardFilterKind {
    High,
    Blocked,
    Waiting,
    Unassigned,
    Agent,
    Mine,
}

/// How the board is laid out: lanes side by side, or every card a row.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BoardLayout {
    #[default]
    Lanes,
    Table,
}

/// The plan open on Plans as the person edits it.
pub(super) struct PlanDraft {
    pub id: Option<String>,
    pub title: String,
    pub body: iced::widget::text_editor::Content,
    pub status: agentdocker_core::PlanStatus,
    /// Edited since it was read or saved.
    pub dirty: bool,
}

impl Default for PlanDraft {
    fn default() -> Self {
        Self {
            id: None,
            title: String::new(),
            body: iced::widget::text_editor::Content::new(),
            status: agentdocker_core::PlanStatus::Draft,
            dirty: false,
        }
    }
}

impl PlanDraft {
    /// The plan as read, unless the person is editing it.
    pub fn load(&mut self, plan: &agentdocker_core::Plan) {
        if self.dirty && self.id.as_deref() == Some(plan.id.as_str()) {
            return;
        }
        self.id = Some(plan.id.clone());
        self.title = plan.title.clone();
        self.body = iced::widget::text_editor::Content::with_text(&plan.body);
        self.status = plan.status;
        self.dirty = false;
    }
}

/// A rule being written in the board's settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct RuleDraft {
    pub name: String,
    /// When: a card enters `lane`, or (idle) has sat there `days`.
    pub idle: bool,
    pub lane: String,
    pub days: String,
    pub action: RuleActionKind,
    /// The comment's words, the lane to move to, or the runtime to start.
    pub value: String,
}

impl Default for RuleDraft {
    fn default() -> Self {
        Self {
            name: String::new(),
            idle: false,
            lane: String::new(),
            days: "14".into(),
            action: RuleActionKind::Comment,
            value: String::new(),
        }
    }
}

/// A menu open on the card: which agent to start, which plan it carries
/// out, which of the project's files to attach.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardMenu {
    Start,
    Plan,
    Attach,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuleActionKind {
    Comment,
    Move,
    Archive,
    Start,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuleFieldKind {
    Name,
    Lane,
    Days,
    Value,
}

/// A card's words as the editor took them, or as it last saved them: a
/// save sends only what differs from these, so it never puts back what
/// somebody else changed on a field the person left alone.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct CardWords {
    pub title: String,
    pub description: String,
    pub acceptance: String,
    pub due: String,
    pub blocker: String,
    pub tags: String,
}

/// The open card's fields as the person edits them; saved on CardSave.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct CardDraft {
    pub task: Option<agentdocker_core::TaskId>,
    /// Which opening of the editor this is: a save's reply advances the
    /// baseline only of the draft that sent it, never of a later opening.
    pub generation: u64,
    pub base: CardWords,
    pub title: String,
    pub description: String,
    pub acceptance: String,
    pub due: String,
    pub blocker: String,
    pub tags: String,
    pub comment: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardFieldKind {
    Title,
    Description,
    Acceptance,
    Due,
    Blocker,
    Tags,
    Comment,
}

#[derive(Default)]
pub(super) struct State {
    pub catalog: Catalog,
    pub selected: Option<String>,
    pub search: String,
    pub session_filter: super::sessions::Filter,
    pub more: bool,
    pub terminal_opening: bool,
    /// The usage screen's window and grouping: this window's, not saved.
    pub usage_since: &'static str,
    pub usage_by: agentdocker_core::usage::report::Group,
    pub session_details: bool,
    /// The changed file whose diff is open in the session's Changes.
    pub diff_file: Option<String>,
    pub review_delivery: bool,
    pub session_message: bool,
    pub session_drafts: BTreeMap<String, SessionDraft>,
    /// Denial notes on their way to their agents, by question.
    pub deny_notes: BTreeMap<MessageId, DenyNote>,
    /// Text only, keyed by the original question ID. Never restores approval or sending state.
    pub answers: BTreeMap<MessageId, String>,
    /// Nonsecret form edits stay in memory; ordinary drafts never receive them.
    pub forms: BTreeMap<MessageId, super::forms::Draft>,
    /// Card text belongs to its original project; filing state is never restored.
    pub task_drafts: BTreeMap<String, TaskDraft>,
    /// The board chosen for each project, by its slug; main otherwise.
    pub board_choice: BTreeMap<String, String>,
    /// The board's search words and filters: this window's, not saved.
    pub board_query: String,
    pub board_filters: BoardFilters,
    /// The lane whose quick-add field is open. Its words are the
    /// project's card draft (`task_drafts`), saved with the other drafts.
    pub quick_add: Option<String>,
    pub board_layout: BoardLayout,
    pub board_dense: bool,
    /// Tags a card must all carry to show.
    pub board_tags: BTreeSet<String>,
    pub tags_menu: bool,
    /// A link being added to the open card.
    pub link_kind: Option<agentdocker_core::LinkKind>,
    pub link_target: String,
    pub card_menu: Option<CardMenu>,
    pub tag_draft: String,
    /// The agent whose roles menu is open, and whether it is editing the
    /// project's palette there: a new role's name, and the role whose
    /// delete was pressed once, when.
    pub roles_menu: Option<String>,
    pub roles_editing: bool,
    pub role_draft: String,
    pub role_delete: Option<(String, std::time::Instant)>,
    pub rule_draft: RuleDraft,
    pub plan_draft: PlanDraft,
    /// Each project's memory card: what is being written, searched, and
    /// sent, by project root, so nothing written for one is kept in another.
    pub memory: BTreeMap<String, MemoryForm>,
    /// The open card's fields as the person edits them.
    pub card_draft: CardDraft,
    /// A card being dragged, and the lane (and card) it would land at.
    pub dragging: Option<agentdocker_core::TaskId>,
    pub drag_target: Option<(String, Option<agentdocker_core::TaskId>)>,
    /// The board's lanes as the person reshapes them, before saving.
    pub lane_settings: Option<agentdocker_core::Board>,
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
    /// The Launch agent split button's menu of tools is open.
    pub launch_menu: bool,
    /// The editors installed here, found once as the window opens.
    pub editors: Vec<agentdocker_core::editor::Editor>,
    /// The **Open in…** menu that is open, if one is.
    pub editor_menu: Option<EditorMenu>,
    pub launch_runtime: Option<String>,
    /// What the new agent is asked to do, sent as its first message once
    /// it is launched. Kept like a draft until then.
    pub launch_prompt: String,
    /// The launch form's **More options** (name, arguments, the command)
    /// are open.
    pub launch_more: bool,
    pub launch_name: String,
    pub launch_arguments: String,
    /// The model and effort chosen for the new session; `None` leaves it
    /// to the provider's own default.
    pub launch_model: Option<String>,
    pub launch_effort: Option<String>,
    /// What the chosen tool offers here for both, read off the UI thread,
    /// by the tool it was read for.
    pub launch_choices: Option<(String, agentdocker_core::model_choice::ModelChoices)>,
    /// The model or effort menu that is open.
    pub launch_choice_menu: Option<ChoiceMenu>,
    /// Invalidates choice reads made for an earlier tool.
    launch_choices_generation: u64,
    /// Provider input is on for supported new sessions by default. The launch
    /// control can turn it off; provider consent and policy still apply.
    pub launch_channel: bool,
    /// Whether the launched session gets its own worktree and branch.
    pub launch_worktree: bool,
    /// The project folder Codex does not trust, when Codex is the tool
    /// chosen; this alone does not determine its sandbox or approval policy.
    pub launch_codex_untrusted: Option<std::path::PathBuf>,
    /// Invalidates configuration reads from an earlier launch form or tool.
    launch_codex_trust_generation: u64,
    /// The count last shown on the dock icon, so it is set only on a change.
    pub dock_badge: Option<usize>,
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
    /// The conversation open in Messages, by its id (`everyone:<project>`
    /// or `dm:<a>:<b>`), and the thread root open beside it, if any.
    pub conversation: Option<String>,
    pub thread: Option<MessageId>,
    pub conversation_drafts: BTreeMap<String, TextDraft>,
    /// The sidebar's filter text.
    pub messages_search: String,
    /// Whether the collapsed sidebar groups are open.
    pub earlier_open: bool,
    /// How many of the Earlier group's entries are on screen: a page, and
    /// a page more for each *Show older*; closing the group resets it.
    pub earlier_shown: usize,
    /// Whether the temporary projects (discovered under /tmp, unpinned)
    /// are unfolded in the sidebar: the person's choice once they have
    /// toggled it, until then automatic (open while one of them has a
    /// live session).
    pub temporary_open: Option<bool>,
    /// The project row whose menu is open.
    pub project_menu: Option<PathBuf>,
    /// The project row under the pointer: its menu button shows only
    /// there, on the selected row and while its menu is open.
    pub rail_hover: Option<PathBuf>,
    /// The control a pointer (or assistive technology) last pressed,
    /// until a key is pressed. A menu opened from that control was opened
    /// by pointer and keeps the focus where it is; opened from the
    /// keyboard, the menu takes the focus to its first entry, since menus
    /// float above the page and would otherwise be last in the Tab order.
    pub pressed: Option<String>,
    /// The project being renamed, and the name so far.
    pub project_rename: Option<(PathBuf, String)>,
    /// The conversation open in Inbox: one agent, or every agent at once.
    pub inbox_thread: Option<String>,
    /// In a narrow window Inbox shows either the list or one conversation;
    /// this is which. Wide windows show both and ignore it.
    pub inbox_open: bool,
    pub needs_you_expanded: bool,
    /// The session being renamed and the name typed so far.
    pub renaming: Option<(String, String)>,
    /// Processes a Connect was pressed for, until the daemon answers.
    pub adopting: BTreeSet<u32>,
    /// The footer is asking whether to restart the daemon.
    pub confirm_daemon_restart: bool,
    pub pending_answer_reveal: Option<MessageId>,
    pub reveal_next_question: bool,
    /// An archived message now on view that the next tick scrolls to.
    pub reveal_archived_next: Option<MessageId>,
    pub generation: u64,
    pub saved_generation: u64,
    pub saving: bool,
    pending_update: Option<i64>,
    pub save_enabled: bool,
    /// The removed folders the hooks have, as last written; none until a
    /// write succeeds. While it differs from the catalog's, the write is
    /// tried again (every `EXCLUSIONS_RETRY` after a failure) and the error
    /// says the hooks disagree.
    pub exclusions_synced: Option<Vec<PathBuf>>,
    pub exclusions_busy: bool,
    pub exclusions_failed: Option<(Instant, String)>,
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
    /// (opening the project, the session or its terminal, or already
    /// having it on screen in a focused window when it finished); an
    /// `idle` report on its own never counts as seen. Window-local, never
    /// persisted.
    pub unviewed_done: BTreeSet<String>,
    /// Agents that finished while the window was not in front, waiting to
    /// be told: posted off the window's thread after the update.
    pub finish_notices: Vec<String>,
    /// Agents that stopped for the person in their own terminal while the
    /// window was not in front, and since when: told once the stop has
    /// lasted `WAIT_GRACE`, so a prompt answered at once never notifies.
    pub wait_notices: Vec<(String, Instant)>,
    /// A wake is set for the first pending wait notice to come due.
    pub wait_timer: bool,
    /// What is to be asked of the notification centre, in order, one at a
    /// time: a withdrawal always lands after the post it takes back, and
    /// an older withdrawal never meets a newer post of the same agent. The
    /// daemon posts outside this queue, so its notices never share a tag
    /// with the window's: each question has its own.
    pub notice_queue: std::collections::VecDeque<NoticeOp>,
    /// One operation is with the notification centre now.
    pub notice_busy: bool,
    /// The operations started, in order, for the tests to read.
    #[cfg(test)]
    pub notice_log: Vec<String>,
    /// Tags of notifications that may still sit in the notification
    /// centre: this window's notices about agents (`window_tag`), and the
    /// open questions the daemon posted (`question_tag`). Each is taken
    /// back once it stops being true.
    pub noticed: BTreeSet<String>,
    /// What this window last said it shows (`viewing.json`), and when.
    pub viewing: Option<(agentdocker_host::notify::Viewing, Instant)>,
    /// Cards this window has moved on by their holder's pull request, and
    /// where to: each move is asked once, so an agent that moves a card
    /// back is never fought.
    pub board_moved: BTreeSet<(String, agentdocker_core::task::Column)>,
    /// The window has lost focus; what it shows is not being looked at.
    pub unfocused: bool,
    /// Each shown branch's pull request and checks, as `gh` last said.
    /// Window-local, never persisted.
    pub pull_requests: crate::pull_request::Cache,
    // command palette
    /// The command palette: open or not, its search and nested pages.
    pub palette: super::palette::Palette,
    // approvals
    /// The option chosen for each question whose answer is on the way, so
    /// its card can show the choice as the receipt. Only answers still in
    /// flight are kept. Window-local, display only.
    pub chosen_answers: BTreeMap<MessageId, String>,
    /// The places behind and ahead, for Back and Forward.
    pub history: super::navigation::History,
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

/// The tag this window posts its notice about an agent under: one per
/// agent, a newer replacing its last, never shared with the daemon's.
pub fn window_tag(agent: &str) -> String {
    format!("window.{agent}")
}

/// One request of the notification centre: post an agent's notice, or
/// take back some notices by tag.
#[derive(Clone, Debug)]
pub enum NoticeOp {
    Post(Box<agentdocker_host::notify::Notification>),
    Withdraw(Vec<String>),
}

impl NoticeOp {
    /// Done off the window's thread; the outcome is the person's business
    /// (a refused notification is not an error the window can fix).
    fn run(self) {
        match self {
            Self::Post(notice) => {
                let _ = crate::notify::post_notification(&notice);
            }
            Self::Withdraw(tags) => crate::notify::withdraw(&tags),
        }
    }
    #[cfg(test)]
    fn describe(&self) -> String {
        match self {
            Self::Post(notice) => format!("post {}", notice.tag.as_deref().unwrap_or_default()),
            Self::Withdraw(tags) => format!("withdraw {}", tags.join(",")),
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

/// The clock sweep while the window has focus: "12s ago" labels, the
/// status line clearing, a question's time left.
pub const FOCUSED_SWEEP: Duration = Duration::from_secs(2);
/// How long a stop for the person lasts before it is told: a prompt the
/// person answers at once, at the terminal, is not news (Roo Code waits
/// two seconds too).
pub const WAIT_GRACE: Duration = Duration::from_secs(2);
/// The same sweep while it does not: nobody reads those labels, and each
/// tick paints the whole window.
pub const UNFOCUSED_SWEEP: Duration = Duration::from_secs(30);

/// Each conversation and session keeps its own draft; receipts clear only an
/// untouched submission.
#[derive(Clone, Debug, Default)]
pub(super) struct TextDraft {
    pub text: String,
    pub sending: Option<String>,
    pub error: Option<String>,
    pub readiness: Option<agentdocker_core::SendReadiness>,
    pub readiness_expanded: bool,
    edited_since_send: bool,
}

/// Words sent with an accepted denial, kept until the daemon takes them.
/// Saved with the drafts as the agent's message-box text until then, so a
/// window closed mid-send finds them there, unsent, on reopening.
#[derive(Clone, Debug)]
pub(super) struct DenyNote {
    pub agent: String,
    pub text: String,
    /// Why it did not go, or may not have; then it waits for the agent's
    /// message box.
    pub failed: Option<NoteFailure>,
}

/// Why a note did not go. `certain` when nothing was sent: the daemon
/// refused it or it never left the window. Otherwise the daemon may have
/// taken it before its answer was lost.
#[derive(Clone, Debug)]
pub struct NoteFailure {
    pub reason: String,
    pub certain: bool,
}

#[derive(Default)]
pub(super) struct SessionDraft {
    pub draft: TextDraft,
    pub queued: Option<MessageId>,
}
impl TextDraft {
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
        // Folders removed before the hooks read this list are out of their
        // hands from the first start of a build that writes it. A failure
        // is said, and the write retried from the update's tail.
        let (exclusions_synced, exclusions_failed) = if save_enabled {
            match catalog.sync_exclusions(home) {
                Ok(()) => (Some(catalog.hidden.clone()), None),
                Err(error) => (
                    None,
                    Some((Instant::now(), exclusions_message(&error.to_string()))),
                ),
            }
        } else {
            (None, None)
        };
        let error = error.or_else(|| exclusions_failed.as_ref().map(|(_, m)| m.clone()));
        Self {
            catalog,
            error,
            save_enabled,
            exclusions_synced,
            exclusions_failed,
            drafts,
            draft_home,
            session_drafts: saved
                .sessions
                .into_iter()
                .map(|(key, text)| {
                    (
                        key,
                        SessionDraft {
                            draft: TextDraft {
                                text,
                                ..Default::default()
                            },
                            ..Default::default()
                        },
                    )
                })
                .collect(),
            // A draft for a conversation the app no longer opens (`#all`,
            // a channel) is let go: nothing here could send it.
            conversation_drafts: saved
                .conversations
                .into_iter()
                .filter(|(key, _)| !super::messages::hidden_conversation(split_draft_key(key).0))
                .map(|(key, text)| {
                    (
                        key,
                        TextDraft {
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
            usage_since: super::usage::DEFAULT_SINCE,
            ..Default::default()
        }
    }
    pub fn changed(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }

    /// Failed notification replies whose conversation could not be
    /// opened — the message or project is gone, the message is in a
    /// conversation the app leaves to the CLI, or the route was cancelled
    /// — and which no route is still on its way to. They are
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
    Answer,
    TaskTitle,
    TaskAcceptance,
}

impl State {
    fn draft_snapshot(&self) -> crate::drafts::Snapshot {
        let mut sessions: BTreeMap<String, String> = self
            .session_drafts
            .iter()
            .filter(|(_, d)| !d.draft.text.is_empty())
            .map(|(k, d)| (k.clone(), d.draft.text.clone()))
            .collect();
        // A denial note not yet certainly delivered is saved as its agent's
        // message-box text: restored, it is there to check and send by
        // hand, and is never sent again on its own.
        for note in self.deny_notes.values() {
            sessions
                .entry(note.agent.clone())
                .and_modify(|text| *text = format!("{text}\n\n{}", note.text))
                .or_insert_with(|| note.text.clone());
        }
        crate::drafts::Snapshot {
            sessions,
            conversations: self
                .conversation_drafts
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
            DraftKind::Answer => self.answers.get(&question),
            DraftKind::TaskTitle => self.task_drafts.get(&id).map(|d| &d.title),
            DraftKind::TaskAcceptance => self.task_drafts.get(&id).map(|d| &d.acceptance),
        };
        let total: usize = self
            .session_drafts
            .values()
            .map(|d| d.draft.text.len())
            .chain(self.conversation_drafts.values().map(|d| d.text.len()))
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
}

type PullRequestKey = crate::pull_request::Key;
type PullRequestAnswer = Result<Option<crate::pull_request::PullRequest>, String>;

/// A project's memory card as the person left it.
#[derive(Clone, Debug, Default)]
pub struct MemoryForm {
    /// A memory being written, kept until it is confirmed kept.
    pub draft: String,
    /// The words searched for; empty shows the newest.
    pub query: String,
    /// The last answer, and the words it answers.
    pub results: Option<(String, Vec<agentdocker_core::memory::Memory>)>,
    /// The text sent and not yet confirmed.
    pub adding: Option<String>,
    /// The search on view still has to be asked: its answer is out of date,
    /// or the request could not be sent yet.
    pub search_due: bool,
}

/// Which of the launch form's choices has its menu open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChoiceMenu {
    Model,
    Effort,
}

/// Where an **Open in…** menu opens from: the project's header, or the
/// selected session's details, for the worktree it works in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditorMenu {
    Project,
    Session,
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
    /// Back and Forward through the places the person went: the rail's
    /// arrows, ⌘[ and ⌘] (Control elsewhere), the mouse's own buttons.
    GoBack,
    GoForward,
    /// The usage screen's window (`24h`, `7d`, `30d`) or grouping.
    UsageSince(&'static str),
    UsageBy(agentdocker_core::usage::report::Group),
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
    /// Read the session's branch against the main folder's again.
    ReviewChanges(String),
    /// Open or close one changed file's diff.
    ToggleDiffFile(String),
    /// Merge the session's branch at the head the person looked at.
    MergeBranch(String, String),
    /// Remove an ended session's own worktree once its work is merged.
    RemoveWorktree(String),
    ReviewDelivery,
    /// The Needs-you strip's **Review**: open that session with its
    /// delivery review already unfolded, wherever the person was.
    ReviewSession(String),
    /// Start, edit, submit or cancel renaming a session.
    StartRename(String),
    RenameDraft(String),
    SubmitRename,
    CancelRename,
    /// Open the launch form (the header's Launch agent…); only its own
    /// Close closes it.
    OpenLaunch,
    /// Ask, or stop asking, whether to restart an out-of-date daemon.
    ConfirmDaemonRestart(bool),
    /// Restart it: the CLI beside the window starts the installed release.
    RestartDaemon,
    /// Put away an ended session's undelivered-messages notice; the
    /// messages stay queued.
    DismissDelivery(String),
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
    ToggleEarlier,
    /// One page more of the Earlier group.
    MoreEarlier,
    ToggleTemporary,
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
    TaskMove(agentdocker_core::TaskId, String),
    TaskOpen(agentdocker_core::TaskId),
    TaskAssign(agentdocker_core::TaskId, Option<agentdocker_core::AgentId>),
    TaskArchive(agentdocker_core::TaskId),
    /// The next page of the board.
    TasksMore,
    /// Which board of the project is on view.
    BoardPick(String),
    /// A new board from a template.
    BoardNew(String),
    /// The board's search words, and its filter toggles.
    BoardQuery(String),
    BoardFilter(BoardFilterKind),
    /// A lane's quick add: open (or close) it, and file its words (typed
    /// as `TaskTitle`) there.
    QuickAdd(Option<String>),
    QuickAddFile,
    /// Lanes or a table; roomy or dense; a tag to show only, and the menu
    /// of them.
    BoardLayout,
    BoardDense,
    TagFilter(String),
    TagsMenu,
    /// To-do: tick a card done, or open it on its board.
    TodoDone(agentdocker_core::TaskId),
    TodoOpen(String, agentdocker_core::TaskId),
    /// The open card's links: the kind and target of one to add, adding,
    /// removing, opening and copying one.
    LinkKind(agentdocker_core::LinkKind),
    LinkTarget(String),
    LinkAdd,
    LinkRemove(usize),
    LinkOpen(usize),
    LinkCopy(usize),
    /// The plan the open card carries out; none clears it.
    CardPlan(Option<String>),
    /// The open card's files: open one, upload from disk, attach one the
    /// project has, unlink one.
    FileOpen(String),
    FileUpload,
    FilePicked(Option<PathBuf>),
    FileAttach(String),
    FileUnlink(String),
    /// A menu on the open card, opened or closed; starting an agent of a
    /// runtime on it.
    CardMenu(CardMenu),
    StartAgent(String),
    /// The project's tags, in the board's settings: a new one's name,
    /// making it, its next colour, deleting it.
    TagDraft(String),
    TagCreate,
    TagRecolor(String),
    TagDelete(String),
    /// An agent's roles: its menu opened or closed, a role given or taken
    /// away (agent, role); the palette edited there: editing on or off, a
    /// new role's name, making it, a role's next colour, deleting one
    /// (pressed twice, as it takes the role from every agent holding it).
    RolesMenu(String),
    RoleToggle(String, String),
    RolesEdit,
    RoleDraft(String),
    RoleCreate,
    RoleRecolor(String),
    RoleDelete(String),
    /// The board's rules, in its settings.
    RuleField(RuleFieldKind, String),
    RuleIdle(bool),
    RuleAction(RuleActionKind),
    RuleCreate,
    RuleToggle(String, bool),
    RuleDelete(String),
    /// Plans: a new one, one opened, its words, where it stands, saved.
    PlanNew,
    PlanOpen(String),
    PlanTitle(String),
    PlanBody(iced::widget::text_editor::Action),
    PlanStatus(agentdocker_core::PlanStatus),
    PlanSave,
    /// A project file deleted, from Plans.
    FileDelete(String),
    MemoryDraft(String),
    /// Search the project's memory; empty shows the newest.
    MemoryQuery(String),
    /// Keep the draft as a memory of the project, as the person.
    MemoryAdd,
    MemoryForget(String),
    /// The open card's fields as the person edits them, and saving them.
    CardField(CardFieldKind, String),
    CardPriority(agentdocker_core::Priority),
    CardSize(Option<agentdocker_core::Size>),
    CardSave,
    CardCommentSend,
    /// Approve a card waiting for the person (to Done), or send it back
    /// to be pulled again with the comment typed.
    TaskApprove(agentdocker_core::TaskId),
    TaskBounce(agentdocker_core::TaskId),
    /// Dragging a card: picked up, over a lane (and a card), dropped.
    DragStart(agentdocker_core::TaskId),
    DragOver(String, Option<agentdocker_core::TaskId>),
    DragDrop,
    DragCancel,
    /// The board's lanes: open their settings, edit, save.
    LaneSettings,
    LaneName(usize, String),
    LaneWip(usize, String),
    LaneOwner(usize, agentdocker_core::board::Owner),
    LaneAdd(agentdocker_core::Column),
    LaneRemove(usize),
    LaneMove(usize, bool),
    LanePull(usize),
    LaneSettingsSave,
    ArchiveLane(String),
    /// Tell the selected project's agents to hold: open the reason, or
    /// send it, or lift the pause.
    PauseStart(String),
    PauseDraft(String, String),
    PauseSubmit(String),
    PauseCancel(String),
    ResumeProject(String),

    /// Start a direct message: open or close the form.
    NewConversation,
    /// The person picked who to message: open that direct conversation.
    NewDirect(String),
    /// Open or close the menu under a project row.
    ProjectMenu(PathBuf),
    /// Open or close the Launch agent menu of tools.
    LaunchMenu,
    /// Open the launch form with this tool chosen.
    LaunchWith(String),
    /// The launch form for a runtime in a given project, which is selected
    /// first: a session's own, whichever project is on view.
    LaunchIn(PathBuf, String),
    /// The pointer entered or left a project row of the rail.
    RailHover(PathBuf),
    RailLeave(PathBuf),
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
    /// The removed folders as written for the hooks, or why not.
    ExclusionsSynced(Vec<PathBuf>, Result<(), String>),
    Draft(MessageId, String),
    Answer(MessageId),
    SecretEdit(String, String, Option<agentdocker_core::secret::SecretText>),
    SecretAcknowledge(String, bool),
    SecretSubmit(String),
    SecretCancel(String),
    AnswerChoice(MessageId, String),
    CopyQuestionUrl(MessageId),
    FormEdit(MessageId, String, String),
    FormInclude(MessageId, String, bool),
    FormSelect(MessageId, String, String),
    FormSubmit(MessageId),
    DismissInbox(Vec<MessageId>),
    Adopt(u32),
    AdoptAll,
    Stop(String),
    /// Put an armed Stop back without stopping anything.
    DisarmStop,
    ShowLaunch,
    /// Hand a repair to the agent that owns the work: the prepared words
    /// go to it as a message, as if typed in its panel.
    AskAgent(String, String),
    /// Go to the next agent that calls for the person, loudest first:
    /// what needs you, then failures, then finished work you have not seen.
    NextAttention,
    LaunchRuntime(String),
    /// Saved project trust read off the UI thread for one launch form.
    CodexTrustLoaded(u64, PathBuf, Option<bool>),
    LaunchPrompt(String),
    LaunchMore,
    LaunchName(String),
    LaunchArguments(String),
    /// What a tool offers for its model and effort, read for the form.
    LaunchChoices(u64, String, agentdocker_core::model_choice::ModelChoices),
    LaunchChoiceMenu(Option<ChoiceMenu>),
    /// A model for the new session; `None` is the provider's default.
    LaunchModel(Option<String>),
    LaunchEffort(Option<String>),
    LaunchChannel(bool),
    LaunchWorktree(bool),
    Launch,
    /// Relaunch an ended Claude Code session here, with its conversation
    /// and the AgentDocker channel, so it takes messages live.
    Reconnect(String),
    Attach(String),
    Detach,
    OpenProjectTerminal,
    OpenAgentTerminal(String),
    NativeTerminalOpened(Result<String, String>),
    /// A round of pull request lookups came back, one answer per branch.
    PullRequests(Vec<(PullRequestKey, PullRequestAnswer)>),
    /// Open a pull request's page in the browser; and how that went.
    OpenPullRequest(String),
    PullRequestOpened(Result<(), String>),
    /// The editors installed here, found off the UI thread.
    EditorsFound(Vec<agentdocker_core::editor::Editor>),
    /// Open or close an **Open in…** menu.
    EditorMenu(EditorMenu),
    /// Open this folder in the editor of this kind, and remember the choice.
    OpenIn(String, PathBuf),
    EditorOpened(Result<String, String>),
    TerminalInput(Vec<u8>),
    TerminalResize(u16, u16),
    TerminalScroll(i32),
    TerminalDismiss,
    ConsoleInput(String),
    RunConsole,
    Recall(bool),
    ConnectorEnable(super::ConnectorTunnel),
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
    NotifyFinished(bool),
    /// Mark a session as something to come back to, and leave it.
    MarkUnread(String),
    /// A notice the window posted itself was taken or refused; either way
    /// nothing changes on screen (a refusal is the person's settings).
    /// The notification centre finished the operation in hand.
    NoticeDone,
    DismissError,
    Event(iced::Event),
    Window(window::Id),
    WindowScale(f32),
    NativeAccessibility(Result<usize, String>),
    Captured(window::Screenshot),
    Focus(String),
    /// A click away from a focused control took its focus.
    Blurred,
    Accessibility(crate::accessibility::Snapshot),
    // command palette
    /// Open the command palette, or close it: ⌘K / Control+K, the rail's
    /// Jump to…, a press on its scrim.
    CommandPalette,
    /// The palette's search text.
    CommandQuery(String),
    /// Move its highlighted row up or down, round the ends.
    CommandMove(i32),
    /// A pending wait notice may have come due (`WAIT_GRACE`).
    WaitGrace,
    /// Run its highlighted row (Enter).
    CommandSubmit,
    /// Run the highlighted row's second action (⌘Enter), if it has one.
    CommandSubmitAlt,
    /// Run one of its rows, by id.
    CommandRun(String),
    /// Escape: clear the search, else back out of a page, else close.
    CommandEscape,
    /// Back out of a nested page: the back arrow, or Backspace with
    /// nothing typed.
    CommandBack,
    // Tools, settings and installation
    /// Put off the available update: its banner steps down.
    UpdateLater,
}

/// A wake becomes one frame, a moment later: the answers to a sweep (seven
/// snapshots asked for at once) land within a few milliseconds of each
/// other, and every frame paints the whole window, so the wakes that
/// arrive while this waits are folded into the one it sends.
const WAKE_SETTLE: Duration = Duration::from_millis(40);

async fn notifications(mut output: iced::futures::channel::mpsc::Sender<Message>) {
    use iced::futures::SinkExt;
    let wake = Wake::default();
    loop {
        wake.notified().await;
        tokio::time::sleep(WAKE_SETTLE).await;
        if output.send(Message::Tick).await.is_err() {
            break;
        }
    }
}

/// Whether an answer lets a Codex request through: Allow, or Allow for
/// the session. File changes take either only once they were looked at.
fn grants(answer: &str) -> bool {
    let answer = answer.trim();
    answer.eq_ignore_ascii_case("allow")
        || answer.eq_ignore_ascii_case(agentdocker_core::ALLOW_FOR_SESSION)
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
            // Every tick is a whole frame: the software renderer paints the
            // window again for any message, on a Retina display a tenth of
            // a second of one core. What changes state arrives through the
            // wake above; the sweep only keeps the clock-dependent labels
            // and the status line honest, so it can be slow while nobody is
            // looking at the window.
            iced::time::every(if self.smoke.is_some() {
                Duration::from_millis(100)
            } else if self.shell.unfocused {
                UNFOCUSED_SWEEP
            } else {
                FOCUSED_SWEEP
            })
            .map(|_| Message::Tick),
            iced::event::listen_with(|event, status, _| super::navigation::listened(event, status)),
        ])
    }

    /// Focus a menu's first entry when the menu was opened from the
    /// keyboard; a pointer-opened menu leaves the focus alone.
    fn menu_focus(&self, trigger: &str, first: String) -> Task<Message> {
        if self.shell.pressed.as_deref() == Some(trigger) {
            return Task::none();
        }
        iced::widget::operation::focus(iced::advanced::widget::Id::from(first))
            .chain(crate::controls::reveal_focus())
            .chain(crate::accessibility::collect())
    }

    /// The installed editor of this kind, remembered as the last one
    /// chosen; `None` when it is not installed (any more).
    fn choose_editor(&mut self, kind: &str) -> Option<agentdocker_core::editor::Editor> {
        let editor = self.shell.editors.iter().find(|e| e.kind == kind)?.clone();
        if self.shell.catalog.editor.as_deref() != Some(editor.kind) {
            self.shell.catalog.editor = Some(editor.kind.to_owned());
            self.shell.changed();
        }
        Some(editor)
    }

    pub fn boot() -> (Self, Task<Message>) {
        (
            Self::new(),
            Task::batch([
                window::oldest().and_then(|id| Task::done(Message::Window(id))),
                Task::future(async {
                    crate::clock::warm();
                })
                .discard(),
                find_editors(),
            ]),
        )
    }

    /// One message, as one move for Back and Forward: the person's moves
    /// are recorded, Back and Forward keep the history their own way, and
    /// the window's own updates refine the place on view
    /// (`navigation::moved`).
    pub fn update(&mut self, message: Message) -> Task<Message> {
        match super::navigation::moved(&message) {
            super::navigation::Moved::ByPerson => self.navigating(true, |app| app.apply(message)),
            super::navigation::Moved::Travel => self.navigating(false, |app| app.apply(message)),
            super::navigation::Moved::ByWindow => self.apply(message),
        }
    }

    fn apply(&mut self, message: Message) -> Task<Message> {
        let mut tasks = Vec::new();
        if matches!(
            &message,
            Message::Event(iced::Event::Keyboard(keyboard::Event::KeyPressed { .. }))
        ) {
            self.shell.pressed = None;
        }
        if matches!(
            &message,
            Message::Draft(..)
                | Message::SessionDraft(..)
                | Message::ConversationDraft(..)
                | Message::SelectThread(_)
                | Message::SelectConversation(_)
                | Message::InboxList
                | Message::Navigate(_)
                | Message::GoBack
                | Message::GoForward
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
                | Message::GoBack
                | Message::GoForward
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
                self.ask_due_memory_search();
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
                    // Discovery lists what it may: a folder the person
                    // removed, `/`, the home folder or a full list leaves
                    // the project off without a word.
                    let _ = self.shell.catalog.remember(project, false);
                }
                self.shell.catalog.forget_missing();
                if before != self.shell.catalog {
                    self.shell.changed();
                }
                self.refresh_project_context();
                tasks.push(self.refresh_pull_requests(false));
                // A notification's destination that opens now finishes
                // the person's click on it: a move of theirs.
                tasks.push(self.navigating(true, Self::advance_notification));
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
                self.shell.launch_menu = false;
                self.shell.editor_menu = None;
                if screen == Screen::Chat {
                    self.open_project_chat();
                }
                if screen == Screen::Board {
                    self.request_tasks();
                }
                if matches!(screen, Screen::Board | Screen::Plans)
                    && let Some(project) = self.selected_project_root()
                {
                    self.send(Cmd::Workspace(project));
                }
                if screen == Screen::Todo {
                    self.send(Cmd::Todo);
                }
                if screen == Screen::Usage {
                    self.request_usage();
                }
                if screen == Screen::Runtimes {
                    self.send(Cmd::Runtimes);
                    self.send(Cmd::Connector);
                }
                if screen == Screen::Desktop {
                    self.send(Cmd::Desktop(self.desktop.command("status")));
                }
            }
            Message::GoBack => return self.travel(false),
            Message::GoForward => return self.travel(true),
            Message::UsageSince(since) => {
                self.shell.usage_since = since;
                self.request_usage();
            }
            Message::UsageBy(by) => {
                self.shell.usage_by = by;
                self.request_usage();
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
                // Opening a session is asking to go to its project: one the
                // person removed comes back to the list. A folder under the
                // temporary directory is listed only by a pin.
                if let Some(project) = project.filter(|p| !crate::catalog::is_temporary(&p.root)) {
                    match self.shell.catalog.visit(project) {
                        Ok(changed) => {
                            if changed {
                                self.shell.changed();
                            }
                        }
                        Err(crate::catalog::Refusal::Full) => {
                            self.say(format!(
                                "The project list is full ({} projects). Forget an old project before opening this session.",
                                crate::catalog::MAX_PROJECTS
                            ));
                            return Task::none();
                        }
                        // `/`, the home folder or a relative folder is no
                        // project: the session is one of the Other
                        // sessions. (`visit` brings a removed folder back.)
                        Err(
                            crate::catalog::Refusal::Broad
                            | crate::catalog::Refusal::Relative
                            | crate::catalog::Refusal::Removed,
                        ) => {}
                    }
                }
                let listed = |root: &PathBuf| {
                    self.shell
                        .catalog
                        .projects
                        .iter()
                        .any(|entry| &entry.project.root == root)
                };
                let destination = match root {
                    Some(root) if listed(&root) => Message::SelectProject(root),
                    // An unlisted folder under the temporary directory: the
                    // home view shows every session.
                    Some(root) if !self.shell.catalog.projectless(&root) => Message::AllProjects,
                    _ => Message::Unassigned,
                };
                tasks.push(self.update(destination));
                self.screen = Screen::Agents;
                tasks.push(self.update(Message::SelectSession(id)));
            }
            Message::SelectSession(id) => {
                self.shell.unviewed_done.remove(&id);
                if self.shell.selected.as_deref() != Some(id.as_str()) {
                    self.shell.diff_file = None;
                    self.merge_outcome = None;
                }
                self.request_review(&id);
                self.shell.selected = Some(id);
                self.shell.session_details = false;
                self.shell.editor_menu = None;
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
            Message::NewConversation => self.new_conversation = !self.new_conversation,
            Message::NewDirect(conversation) => {
                self.new_conversation = false;
                return self.update(Message::SelectConversation(conversation));
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
            Message::TaskFile(column) => self.file_card(column, None),
            Message::TaskMove(task, to) => self.send(Cmd::TaskMove {
                task,
                to,
                before: None,
                comment: None,
            }),

            Message::TaskOpen(task) => {
                if self.task_open.as_ref() == Some(&task) {
                    self.task_open = None;
                    self.card = None;
                } else {
                    self.card = None;
                    let generation = self.shell.card_draft.generation + 1;
                    self.shell.card_draft = super::shell::CardDraft {
                        generation,
                        ..self.draft_of(&task)
                    };
                    self.send(Cmd::TaskDetail(task.clone()));
                    self.task_open = Some(task);
                }
            }
            Message::TaskAssign(task, assignee) => self.send(Cmd::TaskEdit {
                task,
                change: super::CardChange {
                    assignee: Some(assignee.map(|a| a.to_string()).unwrap_or_default()),
                    ..Default::default()
                },
                draft: None,
            }),
            Message::TaskArchive(task) => self.send(Cmd::TaskArchive(task)),
            Message::TasksMore => self.request_more_tasks(),
            Message::BoardPick(slug) => {
                if let Some(project) = self.selected_project_root() {
                    self.shell.board_choice.insert(project, slug);
                    self.task_open = None;
                    self.card = None;
                    self.shell.lane_settings = None;
                }
            }
            Message::BoardNew(template) => {
                if let Some(project) = self.selected_project_root() {
                    let name = match template.as_str() {
                        "agents" => "Agents",
                        "simple" => "Simple",
                        _ => "Board",
                    }
                    .to_owned();
                    self.send(Cmd::BoardCreate {
                        project,
                        name,
                        template,
                    });
                }
            }
            Message::BoardQuery(words) => self.shell.board_query = words,
            Message::BoardFilter(kind) => {
                let filters = &mut self.shell.board_filters;
                match kind {
                    BoardFilterKind::High => filters.high = !filters.high,
                    BoardFilterKind::Blocked => filters.blocked = !filters.blocked,
                    BoardFilterKind::Waiting => filters.waiting = !filters.waiting,
                    BoardFilterKind::Unassigned => filters.unassigned = !filters.unassigned,
                    BoardFilterKind::Agent => filters.agent = !filters.agent,
                    BoardFilterKind::Mine => filters.mine = !filters.mine,
                }
            }
            Message::BoardLayout => {
                self.shell.board_layout = match self.shell.board_layout {
                    BoardLayout::Lanes => BoardLayout::Table,
                    BoardLayout::Table => BoardLayout::Lanes,
                };
            }
            Message::BoardDense => self.shell.board_dense = !self.shell.board_dense,
            Message::TagFilter(tag) => {
                if !self.shell.board_tags.remove(&tag) {
                    self.shell.board_tags.insert(tag);
                }
            }
            Message::TagsMenu => self.shell.tags_menu = !self.shell.tags_menu,
            Message::TodoDone(task) => {
                // Ticked: to its board's Done, wherever that is.
                self.send(Cmd::TaskMove {
                    task,
                    to: "done".to_owned(),
                    before: None,
                    comment: None,
                });
            }
            Message::TodoOpen(root, task) => {
                self.shell.catalog.selected = Some(PathBuf::from(root));
                self.shell.changed();
                self.task_open = Some(task.clone());
                self.card = None;
                self.send(Cmd::TaskDetail(task));
                return self.update(Message::Navigate(Screen::Board));
            }
            Message::LinkKind(kind) => {
                // The link button again, with nothing typed, closes it.
                self.shell.link_kind = if self.shell.link_kind == Some(kind)
                    && self.shell.link_target.trim().is_empty()
                {
                    None
                } else {
                    Some(kind)
                };
            }
            Message::LinkTarget(target) => {
                self.shell.link_target = target.chars().take(2048).collect();
            }
            Message::LinkAdd => {
                if let Some(detail) = &self.card
                    && let Some(kind) = self.shell.link_kind
                    && !self.shell.link_target.trim().is_empty()
                {
                    let mut links = detail.task.links.clone();
                    links.push(agentdocker_core::Link {
                        kind,
                        target: self.shell.link_target.trim().to_owned(),
                        note: None,
                    });
                    let task = detail.task.id.clone();
                    self.shell.link_target.clear();
                    self.shell.link_kind = None;
                    self.board_act(update_links(task, links));
                }
            }
            Message::LinkRemove(index) => {
                if let Some(detail) = &self.card
                    && index < detail.task.links.len()
                {
                    let mut links = detail.task.links.clone();
                    links.remove(index);
                    self.board_act(update_links(detail.task.id.clone(), links));
                }
            }
            Message::LinkOpen(index) => {
                if let Some(link) = self.card.as_ref().and_then(|d| d.task.links.get(index)) {
                    match link_destination(link, self.selected_project_root().as_deref()) {
                        LinkDestination::Open(target) => {
                            if let Err(error) = super::open_target(&target) {
                                self.say(error);
                            }
                        }
                        LinkDestination::Card(task) => {
                            self.task_open = Some(task.clone());
                            self.card = None;
                            self.send(Cmd::TaskDetail(task));
                        }
                        LinkDestination::Copy(text) => {
                            self.say("Copied to the clipboard");
                            return iced::clipboard::write(text);
                        }
                    }
                }
            }
            Message::LinkCopy(index) => {
                if let Some(link) = self.card.as_ref().and_then(|d| d.task.links.get(index)) {
                    return iced::clipboard::write(link.target.clone());
                }
            }
            Message::CardPlan(plan) => {
                self.shell.card_menu = None;
                if let Some(detail) = &self.card {
                    let task = detail.task.id.to_string();
                    self.board_act(Request::TaskUpdate {
                        agent: agentdocker_core::HUMAN.into(),
                        task,
                        title: None,
                        acceptance: None,
                        assignee: None,
                        links: None,
                        description: None,
                        priority: None,
                        size: None,
                        due: None,
                        external_blocker: None,
                        parent: None,
                        blocked_by: None,
                        tags: None,
                        plan: Some(plan.unwrap_or_default()),
                        expected_updated_at: None,
                    });
                }
            }
            Message::FileOpen(file) => self.send(Cmd::FileOpen(file)),
            Message::FileUpload => {
                tasks.push(Task::perform(
                    async {
                        rfd::AsyncFileDialog::new()
                            .set_title("Add a file")
                            .pick_file()
                            .await
                            .map(|f| f.path().to_owned())
                    },
                    Message::FilePicked,
                ));
            }
            Message::FilePicked(Some(path)) => {
                if let Some(project) = self.selected_project_root() {
                    let task = match self.screen {
                        Screen::Board => self.task_open.clone(),
                        _ => None,
                    };
                    self.send(Cmd::FileUpload {
                        project,
                        path,
                        task,
                    });
                }
            }
            Message::FilePicked(None) => {}
            Message::FileAttach(file) => {
                self.shell.card_menu = None;
                self.link_file(file, false);
            }
            Message::FileUnlink(file) => self.link_file(file, true),
            Message::FileDelete(file) => self.board_act(Request::FileDelete {
                from: agentdocker_core::HUMAN.into(),
                file,
            }),
            Message::MemoryDraft(text) => {
                if let Some(root) = self.selected_project_root() {
                    self.shell.memory.entry(root).or_default().draft = text
                        .chars()
                        .take(agentdocker_core::memory::MEMORY_CHARS)
                        .collect();
                }
            }
            Message::MemoryQuery(words) => {
                if let Some(root) = self.selected_project_root() {
                    let form = self.shell.memory.entry(root.clone()).or_default();
                    form.query = words.chars().take(200).collect();
                    let query = form.query.trim().to_owned();
                    if query.is_empty() {
                        form.results = None;
                        form.search_due = false;
                    } else {
                        form.search_due = false;
                        self.send(Cmd::MemorySearch {
                            project: root,
                            query,
                        });
                    }
                }
            }
            Message::MemoryAdd => {
                if let Some(root) = self.selected_project_root() {
                    let form = self.shell.memory.entry(root.clone()).or_default();
                    let text = form.draft.trim().to_owned();
                    if !text.is_empty() && form.adding.is_none() {
                        form.adding = Some(text.clone());
                        self.send(Cmd::MemoryAdd {
                            project: root,
                            text,
                        });
                    }
                }
            }
            Message::MemoryForget(memory) => self.board_act(Request::MemoryForget {
                from: agentdocker_core::HUMAN.into(),
                memory,
            }),
            Message::CardMenu(menu) => {
                if menu == CardMenu::Start
                    && !self
                        .task_open
                        .as_ref()
                        .is_some_and(|id| self.card_startable(id))
                {
                    self.shell.card_menu = None;
                } else {
                    self.shell.card_menu = (self.shell.card_menu != Some(menu)).then_some(menu);
                    if menu == CardMenu::Start && self.runtimes.is_empty() {
                        self.send(Cmd::Runtimes);
                    }
                }
            }
            Message::StartAgent(runtime) => {
                self.shell.card_menu = None;
                if let Some(id) = self.task_open.as_ref().filter(|id| self.card_startable(id)) {
                    let workdir = self.selected_project_root().map(PathBuf::from);
                    self.board_act(Request::TaskStart {
                        from: agentdocker_core::HUMAN.into(),
                        task: id.to_string(),
                        runtime,
                        args: Vec::new(),
                        workdir,
                    });
                }
            }
            Message::TagDraft(name) => {
                self.shell.tag_draft = name
                    .chars()
                    .take(agentdocker_core::task::TAG_CHARS)
                    .collect();
            }
            Message::TagCreate => {
                let name = self.shell.tag_draft.trim().to_owned();
                if !name.is_empty() {
                    self.shell.tag_draft.clear();
                    self.board_act(Request::TagSave {
                        from: agentdocker_core::HUMAN.into(),
                        project: self.selected_project_root(),
                        name,
                        color: None,
                        rename: None,
                    });
                }
            }
            Message::TagRecolor(name) => {
                let next = self
                    .selected_project_root()
                    .and_then(|p| self.workspaces.get(&p))
                    .and_then(|w| w.tags.iter().find(|t| t.name == name))
                    .map(|t| {
                        let all = agentdocker_core::TagColor::ALL;
                        let at = all.iter().position(|c| *c == t.color).unwrap_or(0);
                        all[(at + 1) % all.len()]
                    });
                if let Some(color) = next {
                    self.board_act(Request::TagSave {
                        from: agentdocker_core::HUMAN.into(),
                        project: self.selected_project_root(),
                        name,
                        color: Some(color),
                        rename: None,
                    });
                }
            }
            Message::RolesMenu(agent) => {
                if self.shell.roles_menu.as_deref() == Some(agent.as_str()) {
                    self.shell.roles_menu = None;
                } else {
                    // Its project's palette, read if it has not been asked for.
                    if let Some(root) = self.project_of(&agent)
                        && self.palettes_asked.insert(root.clone())
                    {
                        self.send(Cmd::Palette(root));
                    }
                    self.shell.roles_menu = Some(agent);
                }
                self.shell.roles_editing = false;
                self.shell.role_delete = None;
            }
            Message::RoleToggle(agent, role) => {
                let held = self
                    .agents
                    .iter()
                    .find(|a| a.id.as_str() == agent)
                    .is_some_and(|a| a.holds_role(&role));
                let (add, remove) = if held {
                    (Vec::new(), vec![role])
                } else {
                    (vec![role], Vec::new())
                };
                if let Some(root) = self.project_of(&agent) {
                    self.roles_act(
                        root,
                        Request::RoleAssign {
                            from: agentdocker_core::HUMAN.into(),
                            agent,
                            add,
                            remove,
                        },
                    );
                }
            }
            Message::RolesEdit => {
                self.shell.roles_editing = !self.shell.roles_editing;
                self.shell.role_delete = None;
            }
            Message::RoleDraft(name) => {
                self.shell.role_draft = name
                    .chars()
                    .take(agentdocker_core::role::NAME_CHARS)
                    .collect();
            }
            Message::RoleCreate => {
                let name = self.shell.role_draft.trim().to_owned();
                if let Some(root) = self.roles_project()
                    && !name.is_empty()
                {
                    self.shell.role_draft.clear();
                    self.roles_act(
                        root.clone(),
                        Request::RoleSave {
                            from: agentdocker_core::HUMAN.into(),
                            project: Some(root),
                            role: None,
                            name,
                            color: None,
                            description: None,
                            tools: None,
                        },
                    );
                }
            }
            Message::RoleRecolor(role) => {
                if let Some(root) = self.roles_project()
                    && let Some(def) = self
                        .palettes
                        .get(&root)
                        .and_then(Option::as_ref)
                        .and_then(|roles| roles.iter().find(|r| r.role == role))
                        .cloned()
                {
                    let all = agentdocker_core::RoleColor::ALL;
                    let at = all.iter().position(|c| *c == def.color).unwrap_or(0);
                    self.roles_act(
                        root.clone(),
                        Request::RoleSave {
                            from: agentdocker_core::HUMAN.into(),
                            project: Some(root),
                            role: Some(role),
                            name: def.name,
                            color: Some(all[(at + 1) % all.len()]),
                            description: None,
                            tools: None,
                        },
                    );
                }
            }
            Message::RoleDelete(role) => {
                let armed = self
                    .shell
                    .role_delete
                    .as_ref()
                    .is_some_and(|(armed, at)| *armed == role && at.elapsed() < CONFIRM_WITHIN);
                if !armed {
                    self.shell.role_delete = Some((role, std::time::Instant::now()));
                } else if let Some(root) = self.roles_project() {
                    self.shell.role_delete = None;
                    self.roles_act(
                        root.clone(),
                        Request::RoleDelete {
                            from: agentdocker_core::HUMAN.into(),
                            project: Some(root),
                            role,
                        },
                    );
                }
            }
            Message::TagDelete(name) => self.board_act(Request::TagDelete {
                from: agentdocker_core::HUMAN.into(),
                project: self.selected_project_root(),
                name,
            }),
            Message::RuleField(kind, words) => {
                let draft = &mut self.shell.rule_draft;
                let field = match kind {
                    RuleFieldKind::Name => &mut draft.name,
                    RuleFieldKind::Lane => &mut draft.lane,
                    RuleFieldKind::Days => &mut draft.days,
                    RuleFieldKind::Value => &mut draft.value,
                };
                *field = words.chars().take(4000).collect();
            }
            Message::RuleIdle(idle) => self.shell.rule_draft.idle = idle,
            Message::RuleAction(action) => {
                // A comment's words are not a lane or a runtime.
                let draft = &mut self.shell.rule_draft;
                if draft.action != action {
                    draft.value.clear();
                }
                draft.action = action;
            }
            Message::RuleCreate => match self.rule_request() {
                Ok(request) => {
                    self.shell.rule_draft = RuleDraft::default();
                    self.board_act(request);
                }
                Err(error) => self.say(error),
            },
            Message::RuleToggle(rule, enabled) => self.board_act(Request::RuleUpdate {
                from: agentdocker_core::HUMAN.into(),
                rule,
                name: None,
                trigger: None,
                action: None,
                enabled: Some(enabled),
            }),
            Message::RuleDelete(rule) => self.board_act(Request::RuleDelete {
                from: agentdocker_core::HUMAN.into(),
                rule,
            }),
            Message::PlanNew => {
                self.plan_open = None;
                self.shell.plan_draft = PlanDraft {
                    dirty: true,
                    ..PlanDraft::default()
                };
            }
            Message::PlanOpen(plan) => {
                self.shell.plan_draft = PlanDraft::default();
                self.plan_open = None;
                self.send(Cmd::PlanGet(plan));
                if self.screen != Screen::Plans {
                    return self.update(Message::Navigate(Screen::Plans));
                }
            }
            Message::PlanTitle(title) => {
                self.shell.plan_draft.title = title
                    .chars()
                    .take(agentdocker_core::plan::PLAN_TITLE_CHARS)
                    .collect();
                self.shell.plan_draft.dirty = true;
            }
            Message::PlanBody(action) => {
                let edit = action.is_edit();
                self.shell.plan_draft.body.perform(action);
                if edit {
                    self.shell.plan_draft.dirty = true;
                }
            }
            Message::PlanStatus(status) => {
                self.shell.plan_draft.status = status;
                self.shell.plan_draft.dirty = true;
            }
            Message::PlanSave => {
                let project = self.selected_project_root();
                let draft = &mut self.shell.plan_draft;
                if draft.title.trim().is_empty() {
                    self.say("A plan needs a title");
                } else {
                    draft.dirty = false;
                    let request = Request::PlanSave {
                        from: agentdocker_core::HUMAN.into(),
                        project: project.clone(),
                        plan: draft.id.clone(),
                        title: Some(draft.title.trim().to_owned()),
                        body: Some(draft.body.text()),
                        status: Some(draft.status),
                    };
                    // Its answer is the plan, which opens: a new one has
                    // its id from then on.
                    if let Some(project) = project {
                        self.send(Cmd::PlanSave {
                            project,
                            request: Box::new(request),
                        });
                    }
                }
            }
            Message::QuickAdd(lane) => {
                let words = self
                    .selected_project_root()
                    .and_then(|project| self.shell.task_drafts.get(&project))
                    .is_some_and(|draft| !draft.title.trim().is_empty());
                self.shell.quick_add = match (lane, self.shell.quick_add.take()) {
                    // Pressed again: closed, unless words wait in it.
                    (Some(lane), Some(open)) if open == lane => words.then_some(open),
                    (lane, _) => lane,
                };
            }
            Message::QuickAddFile => {
                if let Some(board) = self.board_on_view() {
                    let lane = self
                        .shell
                        .quick_add
                        .clone()
                        .or_else(|| board.lanes.first().map(|l| l.slug.clone()))
                        .and_then(|slug| board.lane(&slug).cloned());
                    if let Some(lane) = lane {
                        // The field stays open for the next one.
                        self.shell.quick_add = Some(lane.slug.clone());
                        self.file_card(lane.category, Some(lane.slug));
                    }
                }
            }
            Message::CardField(kind, words) => {
                let draft = &mut self.shell.card_draft;
                let field = match kind {
                    CardFieldKind::Title => &mut draft.title,
                    CardFieldKind::Description => &mut draft.description,
                    CardFieldKind::Acceptance => &mut draft.acceptance,
                    CardFieldKind::Due => &mut draft.due,
                    CardFieldKind::Blocker => &mut draft.blocker,
                    CardFieldKind::Tags => &mut draft.tags,
                    CardFieldKind::Comment => &mut draft.comment,
                };
                *field = words
                    .chars()
                    .take(agentdocker_core::task::DESCRIPTION_CHARS)
                    .collect();
            }
            Message::CardPriority(priority) => {
                if let Some(task) = self.task_open.clone() {
                    self.send(Cmd::TaskEdit {
                        task,
                        change: super::CardChange {
                            priority: Some(priority),
                            ..Default::default()
                        },
                        draft: None,
                    });
                }
            }
            Message::CardSize(size) => {
                if let Some(task) = self.task_open.clone() {
                    self.send(Cmd::TaskEdit {
                        task,
                        change: super::CardChange {
                            size: Some(size.map(|s| s.as_str().to_owned()).unwrap_or_default()),
                            ..Default::default()
                        },
                        draft: None,
                    });
                }
            }
            Message::CardSave => {
                if let Some(task) = self.task_open.clone()
                    && let Some(change) = self.card_change(&task)
                {
                    let draft = Some(self.shell.card_draft.generation);
                    self.send(Cmd::TaskEdit {
                        task,
                        change,
                        draft,
                    });
                }
            }
            Message::CardCommentSend => {
                let body = self.shell.card_draft.comment.trim().to_owned();
                if let Some(task) = self.task_open.clone()
                    && !body.is_empty()
                {
                    self.shell.card_draft.comment.clear();
                    self.send(Cmd::TaskComment { task, body });
                }
            }
            Message::TaskApprove(task) => {
                if let Some(done) = self.board_on_view().and_then(|b| {
                    b.first(agentdocker_core::Column::Done)
                        .map(|l| l.slug.clone())
                }) {
                    self.send(Cmd::TaskMove {
                        task,
                        to: done,
                        before: None,
                        comment: None,
                    });
                }
            }
            Message::TaskBounce(task) => {
                if let Some(board) = self.board_on_view() {
                    let words = self.shell.card_draft.comment.trim().to_owned();
                    self.shell.card_draft.comment.clear();
                    self.send(Cmd::TaskMove {
                        task,
                        to: board.pull_source.clone(),
                        before: None,
                        comment: (!words.is_empty()).then_some(words),
                    });
                }
            }
            Message::DragStart(task) => {
                self.shell.dragging = Some(task);
                self.shell.drag_target = None;
            }
            Message::DragOver(lane, before) => {
                if self.shell.dragging.is_some() {
                    self.shell.drag_target = Some((lane, before));
                }
            }
            Message::DragDrop => {
                let target = self.shell.drag_target.take();
                if let Some(task) = self.shell.dragging.take() {
                    // Let go where it was picked up — on itself, or in its
                    // own lane with no card to sit before — it was a click.
                    let here = self.board_on_view().and_then(|board| {
                        self.tasks
                            .as_ref()
                            .and_then(|b| b.cards.iter().find(|t| t.id == task))
                            .map(|t| board.lane_of(t).slug.clone())
                    });
                    match target {
                        Some((lane, before))
                            if !(Some(&lane) == here.as_ref()
                                && before.as_ref().is_none_or(|b| *b == task)) =>
                        {
                            self.send(Cmd::TaskMove {
                                task,
                                to: lane,
                                before,
                                comment: None,
                            });
                        }
                        _ => return self.update(Message::TaskOpen(task)),
                    }
                }
            }
            Message::DragCancel => {
                self.shell.dragging = None;
                self.shell.drag_target = None;
            }
            Message::LaneSettings => {
                self.shell.lane_settings = match self.shell.lane_settings.take() {
                    Some(_) => None,
                    None => self.board_on_view(),
                };
                // A rule may start an agent: the runtimes to choose from.
                if self.shell.lane_settings.is_some() && self.runtimes.is_empty() {
                    self.send(Cmd::Runtimes);
                }
            }
            Message::LaneName(index, name) => {
                if let Some(lane) = self
                    .shell
                    .lane_settings
                    .as_mut()
                    .and_then(|b| b.lanes.get_mut(index))
                {
                    lane.name = name
                        .chars()
                        .take(agentdocker_core::board::NAME_CHARS)
                        .collect();
                }
            }
            Message::LaneWip(index, words) => {
                if let Some(lane) = self
                    .shell
                    .lane_settings
                    .as_mut()
                    .and_then(|b| b.lanes.get_mut(index))
                {
                    lane.wip = words.trim().parse::<u16>().ok().filter(|n| *n > 0);
                }
            }
            Message::LaneOwner(index, owner) => {
                if let Some(lane) = self
                    .shell
                    .lane_settings
                    .as_mut()
                    .and_then(|b| b.lanes.get_mut(index))
                {
                    lane.owner = owner;
                }
            }
            Message::LaneAdd(category) => {
                if let Some(board) = self.shell.lane_settings.as_mut()
                    && board.lanes.len() < agentdocker_core::board::LANES_MAX
                {
                    let base = category.as_str().to_owned();
                    let mut slug = base.clone();
                    let mut n = 2;
                    while board.lane(&slug).is_some() {
                        slug = format!("{base}-{n}");
                        n += 1;
                    }
                    board.lanes.push(agentdocker_core::Lane::new(
                        &slug,
                        category.label(),
                        category,
                        agentdocker_core::board::Owner::Any,
                    ));
                }
            }
            Message::LaneRemove(index) => {
                if let Some(board) = self.shell.lane_settings.as_mut()
                    && board.lanes.len() > 1
                    && index < board.lanes.len()
                {
                    board.lanes.remove(index);
                }
            }
            Message::LaneMove(index, up) => {
                if let Some(board) = self.shell.lane_settings.as_mut() {
                    let other = if up {
                        index.checked_sub(1)
                    } else {
                        Some(index + 1)
                    };
                    if let Some(other) = other.filter(|o| *o < board.lanes.len())
                        && index < board.lanes.len()
                    {
                        board.lanes.swap(index, other);
                    }
                }
            }
            Message::LanePull(index) => {
                if let Some(board) = self.shell.lane_settings.as_mut()
                    && let Some(lane) = board.lanes.get(index)
                {
                    match lane.category {
                        agentdocker_core::Column::Ready => board.pull_source = lane.slug.clone(),
                        agentdocker_core::Column::InProgress => {
                            board.pull_target = lane.slug.clone();
                        }
                        _ => {}
                    }
                }
            }
            Message::LaneSettingsSave => {
                if let Some(project) = self.selected_project_root()
                    && let Some(board) = self.shell.lane_settings.take()
                {
                    match board.check() {
                        Ok(()) => self.send(Cmd::BoardSave { project, board }),
                        Err(error) => {
                            self.say(error.to_string());
                            self.shell.lane_settings = Some(board);
                        }
                    }
                }
            }
            Message::ArchiveLane(lane) => {
                if let Some(project) = self.selected_project_root()
                    && let Some(board) = self.board_on_view()
                {
                    self.send(Cmd::TaskArchiveLane {
                        project,
                        board: board.slug,
                        lane,
                    });
                }
            }
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
            Message::RailHover(path) => self.shell.rail_hover = Some(path),
            Message::RailLeave(path) => {
                if self.shell.rail_hover.as_ref() == Some(&path) {
                    self.shell.rail_hover = None;
                }
            }
            Message::ProjectMenu(path) => {
                let open = self.shell.project_menu.as_ref() == Some(&path);
                let key = path.display().to_string();
                self.shell.project_menu = (!open).then_some(path);
                self.shell.project_rename = None;
                if !open {
                    tasks.push(self.menu_focus(
                        &format!("project-menu-{key}"),
                        format!("project-rename-start-{key}"),
                    ));
                }
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
            Message::More => {
                self.shell.more = !self.shell.more;
                if self.shell.more {
                    tasks.push(self.menu_focus("project-more", "project-tab-Journal".to_owned()));
                }
            }
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
            Message::ReviewSession(id) => {
                // Opening selects and folds everything; the review is then
                // unfolded for this session, so the button does something
                // visible even when the session was already selected.
                tasks.push(self.update(Message::OpenSession(id.clone())));
                if self.shell.selected.as_deref() == Some(id.as_str()) {
                    tasks.push(self.update(Message::ReviewDelivery));
                }
            }
            Message::DismissDelivery(id) => {
                if let Some(agent) = self.agents.iter().find(|a| a.id.as_str() == id)
                    && self.shell.catalog.dismiss(
                        agent.id.as_str(),
                        agent.process_started_at,
                        agent.pid,
                    )
                {
                    self.shell.changed();
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
            Message::SessionDetails => {
                self.shell.session_details = !self.shell.session_details;
                self.shell.editor_menu = None;
            }
            Message::ReviewChanges(id) => self.request_review(&id),
            Message::ToggleDiffFile(path) => {
                self.shell.diff_file = if self.shell.diff_file.as_deref() == Some(path.as_str()) {
                    None
                } else {
                    Some(path)
                };
            }
            Message::RemoveWorktree(id) => {
                if self.removing_worktree.is_none() && self.connected.is_ok() {
                    self.removing_worktree = Some(id.clone());
                    self.merge_outcome = None;
                    self.send(Cmd::WorktreeRemove(id));
                }
            }
            Message::MergeBranch(id, head) => {
                if self.merging.is_none() && self.connected.is_ok() {
                    self.merging = Some(id.clone());
                    self.merge_outcome = None;
                    self.send(Cmd::MergeBranch { agent: id, head });
                }
            }
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
            Message::CopyGuidance(text) => {
                self.say("Copied to the clipboard");
                return iced::clipboard::write(text);
            }
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
            Message::ExclusionsSynced(hidden, result) => {
                self.shell.exclusions_busy = false;
                match result {
                    Ok(()) => {
                        // The disagreement is over: its error goes too.
                        if let Some((_, message)) = self.shell.exclusions_failed.take()
                            && self.shell.error.as_deref() == Some(message.as_str())
                        {
                            self.shell.error = None;
                        }
                        self.shell.exclusions_synced = Some(hidden);
                    }
                    Err(error) => {
                        let message = exclusions_message(&error);
                        self.shell.error = Some(message.clone());
                        self.shell.exclusions_failed = Some((Instant::now(), message));
                    }
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
            Message::FormEdit(id, key, text) => {
                self.edit_form(id, key, super::forms::Edit::Text(text))
            }
            Message::FormInclude(id, key, included) => {
                self.edit_form(id, key, super::forms::Edit::Include(included))
            }
            Message::FormSelect(id, key, selected) => {
                self.edit_form(id, key, super::forms::Edit::Select(selected))
            }
            Message::FormSubmit(id) => self.submit_form(id),
            Message::Draft(id, value) => {
                if !self.sending.contains(&id)
                    && !self.questions.iter().any(|q| {
                        q.id == id
                            && matches!(
                                q.presentation,
                                Some(
                                    agentdocker_core::QuestionPresentation::McpUrl { .. }
                                        | agentdocker_core::QuestionPresentation::McpForm { .. }
                                )
                            )
                    })
                {
                    self.shell
                        .edit_draft(DraftKind::Answer, id.to_string(), value);
                }
            }
            Message::CopyQuestionUrl(id) => {
                if self.connected.is_ok()
                    && !self.sending.contains(&id)
                    && let Some(question) = self
                        .questions
                        .iter()
                        .find(|q| q.id == id && !q.expired(Utc::now()))
                    && let Some(
                        presentation @ agentdocker_core::QuestionPresentation::McpUrl {
                            url, ..
                        },
                    ) = &question.presentation
                    && presentation.valid_for(&question.text)
                {
                    let url = url.clone();
                    self.say("Link copied. Open it in your browser if you consent.");
                    return iced::clipboard::write(url);
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
                            ) || !grants(&value)
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
                    let sending = &self.sending;
                    self.shell
                        .chosen_answers
                        .retain(|id, _| sending.contains(id));
                    self.shell.chosen_answers.insert(id.clone(), value.clone());
                    self.send(Cmd::Answer(id, value));
                }
            }
            Message::SecretEdit(id, field, value) => {
                if self.connected.is_ok()
                    && !value.is_some_and(|v| self.secrets.edit(&id, &field, v, Utc::now()))
                {
                    self.say("Temporary input is closed or exceeds its size limit.");
                }
            }
            Message::SecretAcknowledge(id, value) => {
                if self.connected.is_ok() {
                    self.secrets.acknowledge(&id, value, Utc::now());
                }
            }
            Message::SecretSubmit(id) => {
                if self.connected.is_ok()
                    && let Some(answers) = self.secrets.take(&id, Utc::now())
                {
                    self.send(Cmd::SecretAnswer(id, answers));
                }
            }
            Message::SecretCancel(id) => {
                if self.connected.is_ok() && self.secrets.cancel(&id, Utc::now()) {
                    self.send(Cmd::SecretCancel(id));
                }
            }
            Message::Answer(id) => {
                if self.connected.is_ok()
                    && !self.sending.contains(&id)
                    && self.questions.iter().any(|q| {
                        q.id == id
                            && !matches!(
                                q.presentation,
                                Some(
                                    agentdocker_core::QuestionPresentation::McpUrl { .. }
                                        | agentdocker_core::QuestionPresentation::McpForm { .. }
                                )
                            )
                            && !q.expired(Utc::now())
                            && (!matches!(
                                q.presentation,
                                Some(agentdocker_core::QuestionPresentation::CodexFiles { .. })
                            ) || self
                                .shell
                                .answers
                                .get(&id)
                                .is_none_or(|answer| !grants(answer))
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
                    self.shell.chosen_answers.remove(&id);
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
                // Once per press until the daemon answers: a second press
                // would send a second adoption of the same process.
                if self.connected.is_ok()
                    && self.discovered.iter().any(|p| p.pid == pid)
                    && self.shell.adopting.insert(pid)
                {
                    self.send(Cmd::Adopt(pid));
                }
            }
            Message::DisarmStop => self.confirm_stop = None,
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
            Message::StartRename(id) => {
                let current = self
                    .agents
                    .iter()
                    .find(|a| a.id.as_str() == id)
                    .filter(|a| !a.name_is_generated())
                    .map(|a| a.spec.name.clone())
                    .unwrap_or_default();
                self.shell.renaming = Some((id, current));
            }
            Message::RenameDraft(text) => {
                if let Some((_, draft)) = &mut self.shell.renaming {
                    *draft = text
                        .chars()
                        .take(agentdocker_core::agent::NAME_CHARS)
                        .collect();
                }
            }
            Message::SubmitRename => {
                if self.connected.is_ok()
                    && let Some((id, name)) = self.shell.renaming.take()
                {
                    let name = name.trim().to_owned();
                    if agentdocker_core::agent::check_name(&name).is_ok() {
                        self.send(Cmd::Rename(id, name));
                    } else {
                        self.shell.renaming = Some((id, name));
                    }
                }
            }
            Message::CancelRename => self.shell.renaming = None,
            Message::LaunchMenu => {
                self.shell.launch_menu = !self.shell.launch_menu;
                if self.shell.launch_menu
                    && let Some(first) = self.runtimes.iter().find(|r| r.cli.is_some())
                {
                    let first = format!("launch-with-{}", first.name);
                    tasks.push(self.menu_focus("launch-menu", first));
                }
            }
            Message::LaunchIn(project, runtime) => {
                if !self
                    .shell
                    .catalog
                    .projects
                    .iter()
                    .any(|e| e.project.root == project)
                {
                    self.say("Add its project to the list first");
                    return Task::none();
                }
                let select = self.update(Message::SelectProject(project));
                let launch = self.update(Message::LaunchWith(runtime));
                return Task::batch([select, launch]);
            }
            Message::LaunchWith(runtime) => {
                self.shell.launch_menu = false;
                let open = if self.shell.launch {
                    Task::none()
                } else {
                    self.update(Message::ShowLaunch)
                };
                let chosen = self.update(Message::LaunchRuntime(runtime));
                return Task::batch([open, chosen]);
            }
            Message::OpenLaunch => {
                self.shell.launch_menu = false;
                if !self.shell.launch {
                    return self.update(Message::ShowLaunch);
                }
            }
            Message::ShowLaunch => {
                self.shell.launch = !self.shell.launch;
                self.shell.launch_channel = matches!(
                    self.shell.launch_runtime.as_deref(),
                    Some("claude-code" | "codex")
                );
                // A git project starts each session on its own branch in
                // its own worktree unless the person says otherwise.
                self.shell.launch_worktree = self.launch_project_is_git();
                if self.shell.launch {
                    self.shell.selected = None;
                    self.shell.session_filter = super::sessions::Filter::Current;
                    // The form lives on Chat and Agents; from Board or
                    // History, Launch agent… goes to Agents to show it.
                    if !matches!(self.screen, Screen::Chat | Screen::Agents) {
                        // Navigating resets the session view, the form with
                        // it; it is opened again on arrival.
                        let task = self.update(Message::Navigate(Screen::Agents));
                        self.shell.launch = true;
                        return Task::batch([task, self.refresh_codex_trust()]);
                    }
                }
                tasks.push(self.refresh_codex_trust());
            }
            Message::LaunchRuntime(runtime) => {
                // A normal launch connects the supported input adapter. The
                // provider still asks for its own channel/tool consent.
                self.shell.launch_channel = matches!(runtime.as_str(), "claude-code" | "codex");
                // Another tool, other models: what was chosen for the last
                // one is not carried over.
                if self.shell.launch_runtime.as_deref() != Some(runtime.as_str()) {
                    self.shell.launch_model = None;
                    self.shell.launch_effort = None;
                    self.shell.launch_choice_menu = None;
                }
                self.shell.launch_runtime = Some(runtime.clone());
                tasks.push(self.refresh_codex_trust());
                // Only the read made for the tool chosen last applies: one
                // for an earlier choice may finish after it.
                self.shell.launch_choices_generation += 1;
                let generation = self.shell.launch_choices_generation;
                tasks.push(Task::perform(
                    {
                        let runtime = runtime.clone();
                        async move {
                            tokio::task::spawn_blocking(move || {
                                agentdocker_host::models::choices(&runtime)
                            })
                            .await
                            .unwrap_or_default()
                        }
                    },
                    move |choices| Message::LaunchChoices(generation, runtime.clone(), choices),
                ));
            }
            Message::CodexTrustLoaded(generation, root, trusted) => {
                if self.shell.launch
                    && self.shell.launch_runtime.as_deref() == Some("codex")
                    && self.shell.launch_codex_trust_generation == generation
                    && self.shell.catalog.selected.as_ref() == Some(&root)
                {
                    self.shell.launch_codex_untrusted = (trusted == Some(false)).then_some(root);
                }
            }
            Message::AskAgent(id, words) => {
                if self.connected.is_ok() {
                    let name = self
                        .agents
                        .iter()
                        .find(|a| a.id.as_str() == id)
                        .map(|a| self.display_name(a))
                        .unwrap_or_else(|| id.clone());
                    self.send(Cmd::SessionSend(id, words));
                    self.say(format!("Asked {name}"));
                }
            }
            Message::NextAttention => {
                // As in the palette, a folder the person removed does not
                // call for them: walking onto its session would bring it
                // back to the list without their asking.
                let mut calling: Vec<_> = self
                    .agents
                    .iter()
                    .filter(|a| a.spec.runtime != agentdocker_core::HUMAN_RUNTIME)
                    .filter(|a| {
                        a.project
                            .as_ref()
                            .is_none_or(|p| !self.shell.catalog.hidden.contains(&p.root))
                    })
                    .map(|a| (self.standing(a), a.id.to_string()))
                    .filter(|(standing, _)| standing.calls_for_attention())
                    .collect();
                calling.sort();
                if calling.is_empty() {
                } else {
                    // From the one on screen to the next, round again at
                    // the end, so pressing it again walks the whole queue.
                    let next = self
                        .shell
                        .selected
                        .as_deref()
                        .and_then(|current| calling.iter().position(|(_, id)| id == current))
                        .map_or(0, |at| (at + 1) % calling.len());
                    let id = calling[next].1.clone();
                    tasks.push(self.update(Message::OpenSession(id)));
                }
            }
            Message::LaunchPrompt(prompt) => {
                self.shell.launch_prompt = prompt.chars().take(16_000).collect()
            }
            Message::LaunchMore => self.shell.launch_more = !self.shell.launch_more,
            Message::LaunchName(name) => self.shell.launch_name = name.chars().take(120).collect(),
            Message::LaunchArguments(args) => {
                self.shell.launch_arguments = args.chars().take(8192).collect();
                // Arguments may name another model, one the chosen effort
                // does not fit.
                self.settle_effort();
            }
            Message::LaunchChoices(generation, runtime, choices) => {
                if generation == self.shell.launch_choices_generation
                    && self.shell.launch_runtime.as_deref() == Some(runtime.as_str())
                {
                    self.shell.launch_choices = Some((runtime, choices));
                    self.settle_effort();
                }
            }
            Message::LaunchChoiceMenu(menu) => self.shell.launch_choice_menu = menu,
            Message::LaunchModel(model) => {
                self.shell.launch_model = model;
                self.shell.launch_choice_menu = None;
                self.settle_effort();
            }
            Message::LaunchEffort(effort) => {
                self.shell.launch_effort = effort;
                self.shell.launch_choice_menu = None;
            }
            Message::LaunchChannel(on) => self.shell.launch_channel = on,
            Message::LaunchWorktree(on) => self.shell.launch_worktree = on,
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
                    // Its terminal on screen is the agent looked at.
                    self.shell.unviewed_done.remove(&id);
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
                // Going to its terminal is looking at what it finished.
                self.shell.unviewed_done.remove(&id);
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
                            let session = agent.session.clone();
                            let provider_session = agent.spec.labels.get("session_id").cloned();
                            self.shell.terminal_opening = true;
                            self.say("Opening terminal…");
                            tasks.push(open_native_terminal(
                                crate::native_terminal::Request::Agent {
                                    pid,
                                    started_at,
                                    session,
                                    provider_session,
                                },
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
                    Ok(shown) => self.say(shown),
                    Err(error) => self.shell.error = Some(error),
                }
            }
            Message::PullRequests(answers) => {
                self.shell.pull_requests.answered(answers, Instant::now());
                self.move_cards_by_pull_request();
            }
            Message::OpenPullRequest(url) => tasks.push(Task::perform(
                async move {
                    tokio::task::spawn_blocking(move || crate::pull_request::open(&url))
                        .await
                        .unwrap_or_else(|e| Err(e.to_string()))
                },
                Message::PullRequestOpened,
            )),
            Message::PullRequestOpened(result) => {
                if let Err(error) = result {
                    self.say(error);
                }
            }
            Message::EditorsFound(editors) => self.shell.editors = editors,
            Message::EditorMenu(place) => {
                self.shell.editor_menu = (self.shell.editor_menu != Some(place)).then_some(place);
                if self.shell.editor_menu.is_some()
                    && let Some(first) = self.editor_choices().first()
                {
                    let first = format!("open-in-{}", first.kind);
                    let trigger = match place {
                        EditorMenu::Project => "project-open-in",
                        EditorMenu::Session => "session-open-in",
                    };
                    tasks.push(self.menu_focus(trigger, first));
                }
            }
            Message::OpenIn(kind, folder) => {
                self.shell.editor_menu = None;
                if let Some(editor) = self.choose_editor(&kind) {
                    self.say(format!("Opening in {}…", editor.name));
                    tasks.push(open_in_editor(editor, folder));
                }
            }
            Message::EditorOpened(result) => {
                self.status.clear();
                match result {
                    Ok(shown) => self.say(shown),
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
            Message::ConfirmDaemonRestart(asking) => self.shell.confirm_daemon_restart = asking,
            Message::RestartDaemon => {
                if !self.daemon_restarting {
                    self.daemon_restarting = true;
                    self.shell.confirm_daemon_restart = false;
                    self.send(Cmd::RestartDaemon);
                }
            }
            Message::ConnectorEnable(tunnel) => {
                if self.connector_busy.is_none()
                    && self.connector.is_none()
                    && (cfg!(target_os = "macos") || cfg!(target_os = "linux"))
                {
                    self.connector_busy = Some(tunnel);
                    self.connector_error = None;
                    self.send(Cmd::ConnectorEnable(tunnel));
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
            // The update's tail starts the next one, if any.
            Message::NoticeDone => self.shell.notice_busy = false,
            // The update's tail tells what has come due and sets the next.
            Message::WaitGrace => self.shell.wait_timer = false,
            Message::MarkUnread(id) => {
                self.shell.unviewed_done.insert(id);
                // Marked from its own panel: close it, or opening it again
                // at once would count as looking.
                self.shell.selected = None;
            }
            Message::NotifyFinished(on) => {
                self.settings.notify_finished = on;
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
                // Coming back is when CI is looked at: ask again now
                // rather than at the next two-minute sweep.
                tasks.push(self.refresh_pull_requests(true));
                if let Some(id) = self.shell.window {
                    tasks.push(crate::accessibility::focus(id, true));
                }
            }
            Message::Event(iced::Event::Window(window::Event::Unfocused)) => {
                // A prompt may already be waiting when the person leaves.
                // Start its grace now; repeated focus events must not reset
                // the timer or repost a notice that is still present.
                if !self.shell.unfocused && self.settings.notify_finished && self.smoke.is_none() {
                    let since = Instant::now();
                    for agent in &self.agents {
                        let id = agent.id.as_str();
                        if self.waits_for_person(agent)
                            && !self
                                .shell
                                .wait_notices
                                .iter()
                                .any(|(pending, _)| pending == id)
                            && !self.shell.noticed.contains(&window_tag(id))
                        {
                            self.shell.wait_notices.push((id.to_owned(), since));
                        }
                    }
                }
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
                        self.shell.project_menu = None;
                        self.shell.project_rename = None;
                        self.shell.selected = None;
                        self.shell.launch = false;
                        self.shell.adding = false;
                        self.shell.more = false;
                        if self.screen == Screen::Board {
                            // The board's open things close, one press each
                            // from the innermost.
                            if self.shell.card_menu.is_some() || self.shell.tags_menu {
                                self.shell.card_menu = None;
                                self.shell.tags_menu = false;
                            } else if self.shell.quick_add.is_some() {
                                self.shell.quick_add = None;
                            } else if self.task_open.is_some() {
                                self.task_open = None;
                                self.card = None;
                            } else if self.shell.lane_settings.is_some() {
                                self.shell.lane_settings = None;
                            } else {
                                self.shell.board_query.clear();
                            }
                        }
                    }
                    // The board's own keys, when no field took them: n adds
                    // a card to the first lane, / searches, v switches
                    // lanes and table, d density.
                    Key::Character(key)
                        if self.screen == Screen::Board
                            && !modifiers.command()
                            && !modifiers.control()
                            && !modifiers.alt() =>
                    {
                        match key.as_str() {
                            "n" => {
                                if let Some(lane) = self
                                    .board_on_view()
                                    .and_then(|b| b.lanes.first().map(|l| l.slug.clone()))
                                {
                                    self.shell.quick_add = Some(lane.clone());
                                    tasks.push(
                                        self.update(Message::Focus(format!("quick-add-{lane}"))),
                                    );
                                }
                            }
                            "/" => tasks.push(self.update(Message::Focus("board-search".into()))),
                            "v" => tasks.push(self.update(Message::BoardLayout)),
                            "d" => tasks.push(self.update(Message::BoardDense)),
                            _ => {}
                        }
                    }
                    // command palette: here only when no control took the
                    // key first (a terminal that owns Control+K, say).
                    Key::Character(key)
                        if modifiers.command() && key.as_str().eq_ignore_ascii_case("k") =>
                    {
                        tasks.push(self.update(Message::CommandPalette));
                    }
                    // The next agent that needs you, as cmux binds it.
                    Key::Character(key)
                        if modifiers.command()
                            && modifiers.shift()
                            && key.as_str().eq_ignore_ascii_case("u") =>
                    {
                        tasks.push(self.update(Message::NextAttention));
                    }
                    // New session, as in every other session manager: the
                    // launch form for the project on screen.
                    Key::Character(key)
                        if modifiers.command()
                            && key.as_str().eq_ignore_ascii_case("n")
                            && !self.shell.launch =>
                    {
                        tasks.push(self.update(Message::ShowLaunch));
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
            Message::Blurred => tasks.push(crate::accessibility::collect()),
            Message::Focus(id) => tasks.push({
                self.shell.pressed = Some(id.clone());
                iced::widget::operation::focus(iced::advanced::widget::Id::from(id))
                    .chain(crate::controls::reveal_focus())
                    .chain(crate::accessibility::collect())
            }),
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
            // command palette
            message @ (Message::CommandPalette
            | Message::CommandQuery(_)
            | Message::CommandMove(_)
            | Message::CommandSubmit
            | Message::CommandSubmitAlt
            | Message::CommandRun(_)
            | Message::CommandEscape
            | Message::CommandBack) => tasks.push(self.command_palette(message)),
            // Tools, settings and installation
            Message::UpdateLater => {
                self.desktop.later = self.desktop.update_available().map(str::to_owned);
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
            if self.shell.launch && self.shell.launch_runtime.as_deref() == Some("codex") {
                tasks.push(self.refresh_codex_trust());
            }
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
        // The hooks' list follows the catalog's removed folders, retried
        // while it fails.
        if self.shell.save_enabled
            && !self.shell.exclusions_busy
            && self.shell.exclusions_synced.as_ref() != Some(&self.shell.catalog.hidden)
            && self
                .shell
                .exclusions_failed
                .as_ref()
                .is_none_or(|(at, _)| at.elapsed() >= EXCLUSIONS_RETRY)
        {
            self.shell.exclusions_busy = true;
            let (home, catalog) = (self.home.clone(), self.shell.catalog.clone());
            let hidden = catalog.hidden.clone();
            tasks.push(Task::perform(
                async move {
                    tokio::task::spawn_blocking(move || {
                        catalog.sync_exclusions(&home).map_err(|e| e.to_string())
                    })
                    .await
                    .unwrap_or_else(|e| Err(e.to_string()))
                },
                move |result| Message::ExclusionsSynced(hidden.clone(), result),
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
        // Smoke scenarios consume the snapshot as application state. Normal
        // refreshes publish it directly, avoiding a second update and redraw.
        // Explicit focus/reveal chains above keep their completion message.
        // What is on screen, for the notification poster: a notice about
        // it is not posted. Written when it changes and every half minute
        // while focused, so a stale record never silences anything.
        if self.shell.window.is_some() && self.smoke.is_none() {
            let now = self.viewing_now();
            let due = match &self.shell.viewing {
                None => true,
                Some((said, at)) => {
                    (said.focused, &said.agent, &said.chat) != (now.focused, &now.agent, &now.chat)
                        || (now.focused && at.elapsed() >= Duration::from_secs(30))
                }
            };
            if due {
                let path = agentdocker_host::notify::viewing_path(&agentdocker_host::dirs::home());
                if let Ok(text) = serde_json::to_string(&now) {
                    let _ = std::fs::write(path, text);
                }
                self.shell.viewing = Some((now, Instant::now()));
            }
        }
        // An agent finished, or stopped for the person in its own terminal,
        // while they were away: a quiet notice, one per agent, replacing
        // its last.
        let finished = std::mem::take(&mut self.shell.finish_notices)
            .into_iter()
            .map(|id| (id, false));
        // A stop is told once it has lasted the grace; one that ended
        // inside it is forgotten, and one still young sets a wake for when
        // it comes due, since an unfocused window sweeps only twice a
        // minute.
        let now = Instant::now();
        let telling = self.shell.unfocused && self.settings.notify_finished && self.smoke.is_none();
        let mut due = Vec::new();
        let mut young = Vec::new();
        for (id, since) in std::mem::take(&mut self.shell.wait_notices) {
            let still = self
                .agents
                .iter()
                .any(|a| a.id.as_str() == id && self.waits_for_person(a));
            if !telling || !still {
                continue;
            }
            if now.duration_since(since) >= WAIT_GRACE {
                due.push(id);
            } else {
                young.push((id, since));
            }
        }
        if let Some(soonest) = young.iter().map(|(_, since)| *since).min()
            && !self.shell.wait_timer
        {
            self.shell.wait_timer = true;
            let left = WAIT_GRACE.saturating_sub(now.duration_since(soonest));
            tasks.push(Task::perform(
                async move { tokio::time::sleep(left).await },
                |()| Message::WaitGrace,
            ));
        }
        self.shell.wait_notices = young;
        let waiting = due.into_iter().map(|id| (id, true));
        for (id, waits) in finished.chain(waiting).collect::<Vec<_>>() {
            let Some(agent) = self.agents.iter().find(|a| a.id.as_str() == id) else {
                continue;
            };
            let name = self.display_name(agent);
            let (title, then) = if waits {
                (
                    match self.activity.get(id.as_str()) {
                        Some(Activity::Waiting {
                            reason: Some(agentdocker_core::WaitReason::Permission),
                            ..
                        }) => format!("{name} wants permission"),
                        Some(Activity::Waiting {
                            reason: Some(agentdocker_core::WaitReason::Question),
                            ..
                        }) => format!("{name} has a question"),
                        _ => format!("{name} is waiting for you"),
                    },
                    "It is waiting in its terminal.",
                )
            } else {
                (format!("{name} finished"), "Click to see what it did.")
            };
            let notice = agentdocker_host::notify::Notification {
                title,
                body: match agent.project.as_ref() {
                    Some(project) => format!("In {}. {then}", project.name()),
                    None => then.to_owned(),
                },
                action: None,
                tag: Some(window_tag(&id)),
            };
            self.shell.noticed.insert(window_tag(&id));
            self.shell
                .notice_queue
                .push_back(NoticeOp::Post(Box::new(notice)));
        }
        // A notice that stopped being true is taken back: the question was
        // answered, the agent moved on from its prompt, or the person
        // looked at what it finished. One left in the centre would send
        // them to something already done.
        let stale = self.stale_notices();
        if !stale.is_empty() && self.smoke.is_none() {
            self.shell.notice_queue.push_back(NoticeOp::Withdraw(stale));
        }
        // One at a time, in order; the next starts when this one is done.
        if !self.shell.notice_busy
            && let Some(op) = self.shell.notice_queue.pop_front()
        {
            self.shell.notice_busy = true;
            #[cfg(test)]
            self.shell.notice_log.push(op.describe());
            tasks.push(Task::perform(
                async move {
                    let _ = tokio::task::spawn_blocking(move || op.run()).await;
                },
                |()| Message::NoticeDone,
            ));
        }
        // The dock icon counts the questions waiting on the person, in
        // every project: the dock is the whole app's, not one screen's.
        let waiting = self.waiting_on_you();
        if self.shell.dock_badge != Some(waiting) {
            self.shell.dock_badge = Some(waiting);
            crate::dock::badge(waiting);
        }
        tasks.push(match self.shell.window {
            Some(id) if self.smoke.is_none() => crate::accessibility::collect_and_update(
                id,
                f64::from(self.scale_factor() * self.shell.dpi.max(1.0)),
            ),
            _ => crate::accessibility::collect(),
        });
        Task::batch(tasks)
    }

    /// The tags whose notice stopped being true, forgotten as noticed: a
    /// question no longer open, and an agent with no open question, nothing
    /// finished the person has not looked at, and not stopped at a prompt.
    /// Every open question counts as noticed, since the daemon posts those.
    pub(super) fn stale_notices(&mut self) -> Vec<String> {
        let now = Utc::now();
        let open: BTreeSet<String> = self
            .questions
            .iter()
            .filter(|q| !q.expired(now))
            .map(|q| agentdocker_host::notify::question_tag(q.id.as_str()))
            .collect();
        self.shell.noticed.extend(open.iter().cloned());
        let stale: Vec<String> = self
            .shell
            .noticed
            .iter()
            .filter(|tag| match tag.strip_prefix("window.") {
                Some(id) => {
                    !self.needs_input(id)
                        && !self.shell.unviewed_done.contains(id)
                        && !self
                            .agents
                            .iter()
                            .any(|a| a.id.as_str() == id && self.waits_for_person(a))
                }
                None => !open.contains(*tag),
            })
            .cloned()
            .collect();
        for tag in &stale {
            self.shell.noticed.remove(tag);
        }
        stale
    }

    /// Denial notes that did not go, or may not have, go into their
    /// agent's message box, where the person sees them and can send them
    /// again, once no send of its own is on its way there: nothing typed is
    /// lost, and the composer's pending send stays its own.
    pub(super) fn place_failed_notes(&mut self) {
        let ready: Vec<MessageId> = self
            .shell
            .deny_notes
            .iter()
            .filter(|(_, note)| {
                note.failed.is_some()
                    && self
                        .shell
                        .session_drafts
                        .get(&note.agent)
                        .is_none_or(|entry| entry.draft.sending.is_none())
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in ready {
            let Some(note) = self.shell.deny_notes.remove(&id) else {
                continue;
            };
            let existing = self
                .shell
                .session_drafts
                .get(&note.agent)
                .map(|entry| entry.draft.text.clone())
                .unwrap_or_default();
            let text = if existing.trim().is_empty() {
                note.text.clone()
            } else {
                format!("{existing}\n\n{}", note.text)
            };
            let name = self.name_of(&note.agent);
            if self
                .shell
                .edit_draft(DraftKind::Session, note.agent.clone(), text)
            {
                self.say(match &note.failed {
                    Some(NoteFailure {
                        reason,
                        certain: false,
                    }) => format!(
                        "Your note to {name} may have been sent ({reason}). Check the conversation before sending it again; it is in the message box to {name}."
                    ),
                    Some(NoteFailure { reason, .. }) => format!(
                        "Your note to {name} was not sent ({reason}); it is in the message box to {name}."
                    ),
                    None => format!("Your note to {name} is in the message box to {name}."),
                });
            } else {
                // Draft storage is full: it stays here, and the error says why.
                self.shell.deny_notes.insert(id, note);
            }
        }
    }

    /// File the project's card draft in a lane (the first of `column` when
    /// none is named). Only its own reply clears the draft, so a refusal
    /// keeps the words and typing waits for the answer.
    fn file_card(&mut self, column: agentdocker_core::Column, lane: Option<String>) {
        if let Some(project) = self.selected_project_root()
            && let Some(draft) = self.shell.task_drafts.get_mut(&project)
            && !draft.sending()
            && !draft.title.trim().is_empty()
        {
            self.task_requests += 1;
            let request = self.task_requests;
            draft.sending = Some(request);
            draft.error = None;
            let title = draft.title.trim().to_owned();
            let acceptance = draft.acceptance.trim().to_owned();
            let board = self.board_on_view();
            let lane = lane.or_else(|| {
                board
                    .as_ref()
                    .and_then(|b| b.first(column))
                    .map(|l| l.slug.clone())
            });
            self.send(Cmd::TaskCreate {
                project,
                request,
                title,
                acceptance,
                column,
                lane,
                board: board
                    .map(|b| b.slug)
                    .filter(|s| s != agentdocker_core::board::MAIN),
            });
        }
    }

    /// A change to the board's model for the project on view; what it
    /// touched is read again after.
    /// A role given or taken away, or the palette changed, for the
    /// project it belongs to; its palette and cards are read again after.
    fn roles_act(&mut self, project: String, request: Request) {
        self.send(Cmd::BoardAct {
            project,
            request: Box::new(request),
        });
    }

    /// The root of the project an agent works in.
    pub(super) fn project_of(&self, agent: &str) -> Option<String> {
        self.agents
            .iter()
            .find(|a| a.id.as_str() == agent)
            .and_then(|a| a.project.as_ref())
            .map(|p| p.root.display().to_string())
    }

    /// Ask once for the roles of each project an agent on the list works
    /// in, so its labels and its Roles menu are there when it is opened.
    pub(super) fn ask_palettes(&mut self) {
        let roots: BTreeSet<String> = self
            .agents
            .iter()
            .filter(|a| a.spec.runtime != agentdocker_core::HUMAN_RUNTIME)
            .filter_map(|a| a.project.as_ref())
            .map(|p| p.root.display().to_string())
            .collect();
        for root in roots {
            if self.palettes_asked.insert(root.clone()) {
                self.send(Cmd::Palette(root));
            }
        }
    }

    /// The project whose palette the open roles menu edits.
    fn roles_project(&self) -> Option<String> {
        self.shell
            .roles_menu
            .as_deref()
            .and_then(|agent| self.project_of(agent))
    }

    /// Ask again the search on view whose answer is out of date or whose
    /// request could not be sent: on the next tick, so a full queue that
    /// has drained since takes it.
    pub(super) fn ask_due_memory_search(&mut self) {
        let Some(root) = self.selected_project_root() else {
            return;
        };
        let Some(form) = self.shell.memory.get_mut(&root) else {
            return;
        };
        let query = form.query.trim().to_owned();
        if !form.search_due || query.is_empty() {
            return;
        }
        form.search_due = false;
        self.send(Cmd::MemorySearch {
            project: root,
            query,
        });
    }

    pub(super) fn board_act(&mut self, request: Request) {
        if let Some(project) = self.selected_project_root() {
            self.send(Cmd::BoardAct {
                project,
                request: Box::new(request),
            });
        }
    }

    fn link_file(&mut self, file: String, unlink: bool) {
        if let Some(detail) = &self.card {
            let task = detail.task.id.to_string();
            self.board_act(Request::FileLink {
                from: agentdocker_core::HUMAN.into(),
                file,
                task,
                unlink,
            });
        }
    }

    /// The rule the board's settings describe, for the board on view.
    fn rule_request(&self) -> Result<Request, String> {
        use agentdocker_core::{RuleAction, Trigger};
        let draft = &self.shell.rule_draft;
        let board = self.board_on_view().ok_or("Open a board first")?;
        let lane = if draft.lane.trim().is_empty() {
            board
                .lanes
                .first()
                .map(|l| l.slug.clone())
                .unwrap_or_default()
        } else {
            draft.lane.trim().to_owned()
        };
        let trigger = if draft.idle {
            let days: u32 = draft
                .days
                .trim()
                .parse()
                .map_err(|_| "Idle days are a number from 1 to 365".to_owned())?;
            Trigger::Idle { lane, days }
        } else {
            Trigger::Enter { lane }
        };
        let value = draft.value.trim().to_owned();
        let action = match draft.action {
            RuleActionKind::Comment => RuleAction::Comment { text: value },
            RuleActionKind::Move => RuleAction::Move { lane: value },
            RuleActionKind::Archive => RuleAction::Archive,
            RuleActionKind::Start => RuleAction::Start {
                runtime: if value.is_empty() {
                    self.runtimes
                        .iter()
                        .find(|r| r.cli.is_some())
                        .map(|r| r.name.clone())
                        .ok_or("No command-line agent is installed to start")?
                } else {
                    value
                },
                args: Vec::new(),
                max: 1,
            },
        };
        let name = if draft.name.trim().is_empty() {
            match &action {
                RuleAction::Comment { .. } => "Comment",
                RuleAction::Move { .. } => "Move",
                RuleAction::Archive => "Archive",
                RuleAction::Start { .. } => "Start an agent",
            }
            .to_owned()
        } else {
            draft.name.trim().to_owned()
        };
        Ok(Request::RuleCreate {
            from: agentdocker_core::HUMAN.into(),
            project: self.selected_project_root(),
            board: Some(board.slug),
            name,
            trigger,
            action,
        })
    }

    /// What waits on the person in every project: open questions, and
    /// sessions stopped at a prompt in their own terminal. The rows Needs
    /// you holds when it shows them all.
    pub(crate) fn waiting_on_you(&self) -> usize {
        let now = Utc::now();
        self.questions.iter().filter(|q| !q.expired(now)).count()
            + self
                .agents
                .iter()
                .filter(|agent| self.waits_for_person(agent))
                .count()
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

    pub(super) fn refresh_project_context(&mut self) {
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
            }
            // Another project's board, and a card open on the old one
            // is not open on this.
            self.tasks = None;
            self.task_open = None;
            self.board_asks.clear();
            if self.screen == Screen::Board {
                self.request_tasks();
            }
            // The usage on view is the old project's: read the new one's
            // now rather than say "Reading…" until a filter is touched.
            if self.screen == Screen::Usage {
                self.request_usage();
            }
        }
    }

    /// Wait for initial/reconnected snapshots before resolving a click. Route
    /// by the actual message and sender IDs, never by the notification's text.
    /// A message in a conversation the app leaves to the CLI (`#all`, a
    /// channel) opens the direct conversation with its sender when that is
    /// an agent, else Messages, and the status line says where the message
    /// itself is read.
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
        // Where the message went, when that is somewhere the app does not
        // show: a channel — the notification names it, so this is known
        // even once the message has left the inbox — or every agent.
        let hidden = match envelope.map(|m| &m.to) {
            Some(agentdocker_core::Destination::Channel(id)) => Some(format!("channel:{id}")),
            Some(agentdocker_core::Destination::Broadcast) => Some("all".to_owned()),
            _ => target.channel.as_ref().map(|id| format!("channel:{id}")),
        };
        // A message the person has already read is no longer in the inbox
        // but is archived in its conversation; with conversations, the
        // notification's own agent names where it sits, and it is there
        // even when that agent's record is gone: the archive outlives it.
        let archived = question.is_none() && envelope.is_none() && self.has_conversations();
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
        // A question is answered with whoever asked it, wherever it was
        // asked; its card is in that conversation.
        let hidden = hidden.filter(|_| !is_question);
        // The notification's project: the channel's for a channel message,
        // else the sender's when it posted.
        if let Some(project) = &target.project {
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
        self.shell.pending_notification = None;
        self.shell.notification_message = Some(target.message.clone());
        self.shell.message_detail = Some(target.message.clone());
        self.cancel_reveal();
        self.shell.selected = Some(self.canonical_agent(target.agent.as_str()).to_owned());
        self.shell.more = false;
        self.confirm_stop = None;
        self.screen = Screen::Questions;
        let sender = target.agent.as_str();
        let from_agent = sender != agentdocker_core::conversation::DAEMON && !self.is_human(sender);
        if hidden.is_none() || from_agent {
            // Opened, not merely selected: in the narrow layout the list
            // would otherwise hide the conversation the notification names.
            // An archived conversation sits under the id its party had at
            // the time, so a retired identity opens its own, not the one of
            // the record it became; the inbox's threads go by the current.
            let thread_with = if archived {
                target.agent.to_string()
            } else {
                self.canonical_agent(sender).to_owned()
            };
            self.open_thread_with(thread_with);
        }
        if let Some(conversation) = &hidden {
            self.say(format!(
                "That message went to {}, which the app does not show; read it with `agentdocker history {conversation}`.",
                if conversation == "all" {
                    "every agent (#all)"
                } else {
                    "a channel"
                }
            ));
            if !from_agent {
                // Nothing on view is the message's: a reply that did not go
                // stays at the top of Messages to copy, never in whatever
                // conversation happens to be open.
                self.shell.notification_message = None;
            }
        }
        if hidden.is_none() || from_agent {
            self.recover_reply();
        }
        // On the Messages screen the message is a row of the archive, not
        // of the inbox: it is scrolled to once its page is here, paging
        // back for it if the conversation was already open at its newest
        // — a click on a notification must always show its message.
        if self.has_conversations()
            && !is_question
            && hidden.is_none()
            && let Some(conversation) = self.shell.conversation.clone()
        {
            self.start_archive_reveal(conversation, target.message.clone());
        }
        // Revealing the card expands its retained text and scrolls to it. Existing answer
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
        if agent.spec.command.first().is_none_or(String::is_empty)
            && !self
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
        if agent.spec.workdir.is_none() {
            return Err("This session has no checkout to resume in".into());
        }
        let claude = agent
            .spec
            .command
            .first()
            .map(PathBuf::from)
            .or_else(|| {
                self.runtimes
                    .iter()
                    .find(|r| r.name == "claude-code")
                    .and_then(|r| r.cli.clone())
            })
            .ok_or("Claude Code is not installed here")?;
        let cli = cli.map_err(|error| format!("Cannot enable live messages: {error}"))?;
        agentdocker_host::provider_input::reconnect_claude(&agent.spec, &session, &claude, &cli)
            .map_err(|error| format!("Cannot reconstruct the Claude session launch: {error}"))
    }

    /// Refresh only the open Codex form. Late reads cannot change a newer form.
    fn refresh_codex_trust(&mut self) -> Task<Message> {
        self.shell.launch_codex_untrusted = None;
        self.shell.launch_codex_trust_generation =
            self.shell.launch_codex_trust_generation.wrapping_add(1);
        if !self.shell.launch || self.shell.launch_runtime.as_deref() != Some("codex") {
            return Task::none();
        }
        let Some(root) = self.shell.catalog.selected.clone() else {
            return Task::none();
        };
        let generation = self.shell.launch_codex_trust_generation;
        Task::perform(
            load_codex_trust(root.clone(), agentdocker_host::codex_trust::config_path()),
            move |trusted| Message::CodexTrustLoaded(generation, root.clone(), trusted),
        )
    }

    /// Whether the project the launch form is for is a git repository:
    /// only then can a session have a worktree of its own.
    pub(crate) fn launch_project_is_git(&self) -> bool {
        self.shell
            .catalog
            .selected
            .as_ref()
            .and_then(|root| {
                self.shell
                    .catalog
                    .projects
                    .iter()
                    .find(|e| &e.project.root == root)
            })
            .is_some_and(|entry| entry.project.source == agentdocker_core::ProjectSource::Git)
    }

    /// What the chosen tool offers for its model and effort, once read.
    pub(super) fn launch_choices(&self) -> Option<&agentdocker_core::model_choice::ModelChoices> {
        self.shell
            .launch_choices
            .as_ref()
            .filter(|(runtime, _)| self.shell.launch_runtime.as_deref() == Some(runtime.as_str()))
            .map(|(_, choices)| choices)
    }

    /// The model and effort the form's own arguments put in force, which
    /// win over the chips.
    pub(super) fn launch_given(&self) -> (Option<String>, Option<String>) {
        match (
            self.shell.launch_runtime.as_deref(),
            shell_words(&self.shell.launch_arguments),
        ) {
            (Some(runtime), Some(args)) => {
                agentdocker_core::model_choice::given_choices(runtime, &args)
            }
            _ => (None, None),
        }
    }

    /// The model the new session will use, as `launch_spec` decides it:
    /// the arguments', else the one chosen, else the provider's default.
    pub(super) fn launch_model_now(&self) -> Option<String> {
        self.launch_given()
            .0
            .or_else(|| self.shell.launch_model.clone())
            .or_else(|| self.launch_choices().and_then(|c| c.default_model.clone()))
    }

    /// An effort chosen that the model now in force does not take goes back
    /// to the default, so none is ever passed to a model without it.
    fn settle_effort(&mut self) {
        let model = self.launch_model_now();
        let takes = self
            .launch_choices()
            .map(|c| c.efforts_for(model.as_deref()).to_vec())
            .unwrap_or_default();
        if self
            .shell
            .launch_effort
            .as_ref()
            .is_some_and(|effort| !takes.is_empty() && !takes.contains(effort))
        {
            self.shell.launch_effort = None;
        }
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
        let chosen = agentdocker_core::model_choice::choice_args(
            &runtime.name,
            self.shell.launch_model.as_deref(),
            self.shell.launch_effort.as_deref(),
            &args,
        )?;
        // What it runs with, said on its record: the person's own
        // arguments, else the choice, else the provider's default.
        let (given_model, given_effort) =
            agentdocker_core::model_choice::given_choices(&runtime.name, &args);
        let defaults = self.launch_choices();
        let model = given_model
            .or_else(|| self.shell.launch_model.clone())
            .or_else(|| defaults.and_then(|c| c.default_model.clone()));
        let effort = given_effort
            .or_else(|| self.shell.launch_effort.clone())
            .or_else(|| defaults.and_then(|c| c.default_effort.clone()));
        let mut command = vec![cli.to_string_lossy().into_owned()];
        command.extend(chosen);
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
            model,
            command,
            workdir: Some(entry.project.root.clone()),
            env: BTreeMap::new(),
            labels: {
                let mut labels = BTreeMap::new();
                if generated_name {
                    labels.insert(
                        agentdocker_core::agent::NAME_LABEL.to_owned(),
                        agentdocker_core::agent::GENERATED_NAME.to_owned(),
                    );
                }
                if let Some(effort) = effort {
                    labels.insert(
                        agentdocker_core::model_choice::EFFORT_LABEL.to_owned(),
                        effort,
                    );
                }
                labels
            },
            isolate: self.shell.launch_worktree
                && entry.project.source == agentdocker_core::ProjectSource::Git,
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
        .map(|dir| dir.join(format!("agentdocker{}", std::env::consts::EXE_SUFFIX)))
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

/// Reading a user-selected provider profile may block, including on network storage.
async fn load_codex_trust(root: PathBuf, config: Option<PathBuf>) -> Option<bool> {
    let config = config?;
    tokio::task::spawn_blocking(move || {
        let text = std::fs::read_to_string(config).ok()?;
        agentdocker_host::codex_trust::trusts(&text, &root)
    })
    .await
    .ok()
    .flatten()
}

/// The editors installed here, read off the UI thread.
fn find_editors() -> Task<Message> {
    Task::perform(
        async {
            tokio::task::spawn_blocking(agentdocker_host::editor::installed)
                .await
                .unwrap_or_default()
        },
        Message::EditorsFound,
    )
}

/// Open `folder` in `editor`, off the UI thread; the status line says so.
fn open_in_editor(editor: agentdocker_core::editor::Editor, folder: PathBuf) -> Task<Message> {
    Task::perform(
        async move {
            tokio::task::spawn_blocking(move || {
                agentdocker_host::editor::open(&editor, &folder)
                    .map(|()| format!("Opened in {}", editor.name))
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string()))
        },
        Message::EditorOpened,
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

/// How long after it ended an agent's branch keeps its pull request chip.
const PULL_REQUEST_RECENT: chrono::TimeDelta = chrono::TimeDelta::hours(24);

impl App {
    /// The branch an agent's pull request chip is about: its project's
    /// and its own, unless that is a default branch.
    pub(super) fn pull_request_key(agent: &AgentRecord) -> Option<crate::pull_request::Key> {
        let root = &agent.project.as_ref()?.root;
        let branch = agent.vcs.as_ref()?.branch.as_deref()?;
        crate::pull_request::Key::new(root, branch)
    }

    /// The pull request of the branch an agent works on, as last read.
    pub(super) fn pull_request_of(
        &self,
        agent: &AgentRecord,
    ) -> Option<&crate::pull_request::PullRequest> {
        self.shell
            .pull_requests
            .get(&Self::pull_request_key(agent)?)
    }

    /// The branches worth a chip: those of live agents and of agents that
    /// ended within the last day.
    pub(super) fn pull_request_wanted(&self) -> BTreeSet<crate::pull_request::Key> {
        let now = Utc::now();
        self.agents
            .iter()
            .filter(|a| a.spec.runtime != agentdocker_core::HUMAN_RUNTIME)
            .filter(|a| {
                a.status.is_live()
                    || now - a.finished_at.unwrap_or(a.last_seen) < PULL_REQUEST_RECENT
            })
            .filter_map(Self::pull_request_key)
            .collect()
    }

    /// Ask `gh` again about the wanted branches whose answer is old (see
    /// `pull_request::Cache::due`): one round on a background thread, its
    /// answers in one message, so a refresh costs the window one frame.
    /// What the window shows the person now: the agent whose session or
    /// terminal is open, and the project whose chat is open.
    pub(super) fn viewing_now(&self) -> agentdocker_host::notify::Viewing {
        let agent = match self.screen {
            Screen::Terminal => self.terminal.as_ref().map(|t| t.agent.clone()),
            Screen::Agents | Screen::Chat => self.shell.selected.clone(),
            _ => None,
        };
        let chat = (self.screen == Screen::Chat)
            .then(|| {
                self.shell
                    .catalog
                    .selected()
                    .map(|entry| entry.project.id().to_string())
            })
            .flatten();
        agentdocker_host::notify::Viewing {
            focused: !self.shell.unfocused,
            agent,
            chat,
            at: Utc::now(),
        }
    }

    /// A card follows its holder's pull request: opened, the work is in
    /// review; merged, it is done. Only forward, from In progress or
    /// Review, and each move once.
    pub(super) fn move_cards_by_pull_request(&mut self) {
        use crate::pull_request::Status;
        use agentdocker_core::task::Column;
        let Some(board) = self.tasks.as_ref() else {
            return;
        };
        let mut moves = Vec::new();
        for card in &board.cards {
            let Some(holder) = card.assignee.as_ref() else {
                continue;
            };
            let Some(agent) = self.agents.iter().find(|a| &a.id == holder) else {
                continue;
            };
            let Some(pull) = self.pull_request_of(agent) else {
                continue;
            };
            let to = match (pull.status, card.column) {
                (Status::Merged, Column::InProgress | Column::Review) => Column::Done,
                (
                    Status::Open | Status::Passed | Status::Running | Status::Failed,
                    Column::InProgress,
                ) => Column::Review,
                _ => continue,
            };
            if self.shell.board_moved.insert((card.id.to_string(), to)) {
                moves.push((card.id.clone(), to));
            }
        }
        // A category names its board's first lane of that kind.
        for (task, column) in moves {
            self.send(Cmd::TaskMove {
                task,
                to: column.as_str().to_owned(),
                before: None,
                comment: None,
            });
        }
    }

    pub(super) fn refresh_pull_requests(&mut self, focus: bool) -> Task<Message> {
        let wanted = self.pull_request_wanted();
        let due = self.shell.pull_requests.due(&wanted, Instant::now(), focus);
        if due.is_empty() {
            return Task::none();
        }
        Task::perform(
            async move {
                let keys = due.clone();
                tokio::task::spawn_blocking(move || crate::pull_request::lookup_all(due))
                    .await
                    .unwrap_or_else(|e| {
                        keys.into_iter()
                            .map(|key| (key, Err(e.to_string())))
                            .collect()
                    })
            },
            Message::PullRequests,
        )
    }
}

/// How long a failed write of the hooks' removed folders waits before it
/// is tried again.
pub const EXCLUSIONS_RETRY: Duration = Duration::from_secs(30);

/// What the person is told while the hooks' list disagrees with theirs.
fn exclusions_message(error: &str) -> String {
    format!(
        "Removed projects could not be passed to the hooks ({error}); sessions there may still be coordinated. Trying again."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The hooks' list of removed folders is written on its own: a
    /// failure is said and left for a retry rather than stopping the
    /// workspace from saving, and the retry's success takes the error back.
    #[test]
    fn a_failed_exclusion_sync_is_said_and_retried() {
        let (mut app, _commands, _messages) = app();
        app.shell.save_enabled = true;
        let removed = vec![PathBuf::from("/work/gone")];
        app.shell.catalog.hidden = removed.clone();
        let _ = app.update(Message::ExclusionsSynced(
            removed.clone(),
            Err("disk full".into()),
        ));
        assert!(
            app.shell
                .error
                .as_deref()
                .is_some_and(|e| e.contains("disk full"))
        );
        assert!(app.shell.save_enabled, "the workspace still saves");
        assert!(app.shell.exclusions_failed.is_some());
        // Not before the wait; then once more, and its success clears it.
        let _ = app.update(Message::Tick);
        assert!(!app.shell.exclusions_busy, "waits {EXCLUSIONS_RETRY:?}");
        app.shell.exclusions_failed.as_mut().unwrap().0 -= EXCLUSIONS_RETRY;
        let _ = app.update(Message::Tick);
        assert!(app.shell.exclusions_busy, "tried again");
        let _ = app.update(Message::ExclusionsSynced(removed.clone(), Ok(())));
        assert!(app.shell.error.is_none());
        assert_eq!(app.shell.exclusions_synced, Some(removed));
    }
    fn app() -> (App, CommandReceiver, SyncSender<Msg>) {
        let (tx, commands) = queue::channel();
        let (messages, rx) = sync_channel(MESSAGE_CAPACITY);
        (App::bare(tx, rx), commands, messages)
    }

    /// The board's Mine toggle keeps the cards the person holds and those
    /// held by agents they launched, and leaves out the rest: another
    /// agent's, one held by a session the app has no record of, and a card
    /// nobody holds.
    #[test]
    fn the_mine_filter_keeps_the_persons_cards_and_their_agents() {
        use agentdocker_core::task::Column;
        let (mut app, _commands, _messages) = app();
        let project = ProjectRef::directory("/work/alpha");
        let record = |id: &str, runtime: &str, launched: bool| {
            let mut agent = AgentRecord::new(
                agentdocker_core::AgentSpec {
                    name: id.into(),
                    runtime: runtime.into(),
                    ..Default::default()
                },
                false,
                Utc::now(),
            );
            agent.id = id.into();
            if launched {
                agent.spec.labels.insert(
                    agentdocker_core::agent::LAUNCHED_BY_LABEL.into(),
                    agentdocker_core::HUMAN.into(),
                );
            }
            agent
        };
        app.agents = vec![
            record("person", agentdocker_core::HUMAN_RUNTIME, false),
            record("launched", "codex", true),
            record("terminal", "claude-code", false),
        ];
        let board = agentdocker_core::Board::main(project.id(), "alpha", Utc::now());
        let cards = [
            Some("person"),
            Some("launched"),
            Some("terminal"),
            Some("gone"),
            None,
        ]
        .map(|holder| {
            let mut card = agentdocker_core::Task::new(
                project.id(),
                "Card",
                "",
                Some(Column::Ready),
                "user",
                Vec::new(),
                Utc::now(),
            )
            .unwrap();
            card.assignee = holder.map(Into::into);
            card
        });
        let shown = |app: &App| {
            cards
                .iter()
                .map(|t| app.shown(t, &board))
                .collect::<Vec<_>>()
        };
        assert_eq!(shown(&app), [true; 5], "off, every card");
        let _ = app.update(Message::BoardFilter(BoardFilterKind::Mine));
        assert!(app.shell.board_filters.mine);
        assert_eq!(shown(&app), [true, true, false, false, false]);
        let _ = app.update(Message::BoardFilter(BoardFilterKind::Mine));
        assert_eq!(shown(&app), [true; 5], "off again");
    }

    /// The person keeps a fact for the project's agents from its Plans
    /// screen: each project its own draft, the draft kept until the daemon
    /// confirms it (and then only if unchanged), a search answered only for
    /// the words on view, a failed read keeping what was shown, a forget
    /// sent as the person's, and a memory kept anywhere reading again.
    #[test]
    fn the_person_keeps_searches_and_forgets_a_projects_memory() {
        use agentdocker_core::memory::Memory;
        let (mut app, commands, messages) = app();
        app.connected = Ok(());
        let memory = |id: &str, text: &str| Memory {
            id: id.into(),
            project: agentdocker_core::ProjectId::from("a"),
            author: "codex-1".into(),
            text: text.into(),
            links: vec![],
            supersedes: None,
            created_at: Utc::now(),
            standing: false,
            forget_after: None,
        };
        let drain = |commands: &CommandReceiver| commands.try_iter().collect::<Vec<_>>();
        app.shell
            .catalog
            .pin(ProjectRef::directory("/work/b"))
            .unwrap();
        let _ = app.update(Message::MemoryDraft("a fact of b".into()));
        app.shell
            .catalog
            .pin(ProjectRef::directory("/work/a"))
            .unwrap();
        assert!(
            app.shell
                .memory
                .get("/work/a")
                .is_none_or(|f| f.draft.is_empty()),
            "b's draft is b's"
        );
        let _ = app.update(Message::MemoryDraft("   ".into()));
        let _ = app.update(Message::MemoryAdd);
        assert!(
            !drain(&commands)
                .iter()
                .any(|c| matches!(c, Cmd::MemoryAdd { .. })),
            "nothing blank is kept"
        );
        let _ = app.update(Message::MemoryDraft("Codex reviews every PR".into()));
        let _ = app.update(Message::MemoryAdd);
        assert!(drain(&commands).iter().any(|c| matches!(
            c,
            Cmd::MemoryAdd { project, text } if project == "/work/a" && text == "Codex reviews every PR"
        )));
        // Refused: the text stays to fix.
        messages
            .send(Msg::MemoryAdded(
                "/work/a".into(),
                "Codex reviews every PR".into(),
                Err("that reads like a secret".into()),
            ))
            .unwrap();
        app.drain();
        assert_eq!(app.shell.memory["/work/a"].draft, "Codex reviews every PR");
        // Kept while the person went on writing: the newer words stay.
        let _ = app.update(Message::MemoryAdd);
        let _ = app.update(Message::MemoryDraft(
            "Codex reviews every PR, Claude builds".into(),
        ));
        messages
            .send(Msg::MemoryAdded(
                "/work/a".into(),
                "Codex reviews every PR".into(),
                Ok(()),
            ))
            .unwrap();
        app.drain();
        assert_eq!(
            app.shell.memory["/work/a"].draft,
            "Codex reviews every PR, Claude builds"
        );
        assert_eq!(app.shell.memory["/work/b"].draft, "a fact of b");

        // A search: only the answer to the words on view counts.
        let _ = app.update(Message::MemoryQuery("review".into()));
        assert!(drain(&commands).iter().any(|c| matches!(
            c,
            Cmd::MemorySearch { project, query } if project == "/work/a" && query == "review"
        )));
        let _ = app.update(Message::MemoryQuery("reviews".into()));
        messages
            .send(Msg::MemorySearched(
                "/work/a".into(),
                "review".into(),
                Ok(vec![memory("old", "stale answer")]),
            ))
            .unwrap();
        app.drain();
        assert!(
            app.shell.memory["/work/a"].results.is_none(),
            "an older answer is not shown"
        );
        messages
            .send(Msg::MemorySearched(
                "/work/a".into(),
                "reviews".into(),
                Ok(vec![memory("m1", "Codex reviews every PR")]),
            ))
            .unwrap();
        app.drain();
        assert_eq!(
            app.shell.memory["/work/a"]
                .results
                .as_ref()
                .map(|(q, found)| (q.as_str(), found.len())),
            Some(("reviews", 1))
        );

        // A read that fails keeps what was shown.
        messages
            .send(Msg::Workspace(
                "/work/a".into(),
                Ok(Box::new(Workspace {
                    memories: vec![memory("m1", "kept")],
                    ..Workspace::default()
                })),
            ))
            .unwrap();
        messages
            .send(Msg::Workspace(
                "/work/a".into(),
                Ok(Box::new(Workspace {
                    memory: MemoryRead::Failed("storage unavailable".into()),
                    ..Workspace::default()
                })),
            ))
            .unwrap();
        app.drain();
        let workspace = &app.workspaces["/work/a"];
        assert_eq!(workspace.memories.len(), 1, "the old list stays");
        assert!(matches!(workspace.memory, MemoryRead::Failed(_)));

        let _ = app.update(Message::MemoryForget("m1".into()));
        assert!(drain(&commands).iter().any(|c| matches!(
            c,
            Cmd::BoardAct { request, .. } if matches!(
                request.as_ref(),
                Request::MemoryForget { from, memory } if from == agentdocker_core::HUMAN && memory == "m1"
            )
        )));
        app.on_event(Event::new(
            EventKind::MemoryAdded {
                project: agentdocker_core::ProjectId::from("a"),
                memory: "m2".into(),
                by: "codex-1".into(),
            },
            Utc::now(),
        ));
        let after = drain(&commands);
        assert!(
            after
                .iter()
                .any(|c| matches!(c, Cmd::Workspace(root) if root == "/work/a"))
        );
        assert!(
            after
                .iter()
                .any(|c| matches!(c, Cmd::MemorySearch { query, .. } if query == "reviews")),
            "the search on view is asked again"
        );

        // A's search, then B on view while A's memory changes elsewhere:
        // back on A, its search is asked again rather than shown stale.
        app.shell
            .catalog
            .pin(ProjectRef::directory("/work/b"))
            .unwrap();
        app.on_event(Event::new(
            EventKind::MemoryForgotten {
                project: agentdocker_core::ProjectId::from("a"),
                memory: "m1".into(),
                by: "codex-1".into(),
            },
            Utc::now(),
        ));
        assert!(
            !drain(&commands)
                .iter()
                .any(|c| matches!(c, Cmd::MemorySearch { project, .. } if project == "/work/a")),
            "not while another project is on view"
        );
        app.shell
            .catalog
            .pin(ProjectRef::directory("/work/a"))
            .unwrap();
        messages
            .send(Msg::Workspace("/work/a".into(), Ok(Box::default())))
            .unwrap();
        app.drain();
        assert!(drain(&commands).iter().any(|c| matches!(
            c,
            Cmd::MemorySearch { project, query } if project == "/work/a" && query == "reviews"
        )));

        // The queue full as the words change: the search is asked once
        // there is room, on a later tick.
        while app.tx.send(Cmd::Workspace("/filler".into())).is_ok() {}
        let _ = app.update(Message::MemoryQuery("claude builds".into()));
        assert!(app.shell.memory["/work/a"].search_due, "kept to ask");
        drop(drain(&commands));
        let _ = app.update(Message::Tick);
        assert!(drain(&commands).iter().any(|c| matches!(
            c,
            Cmd::MemorySearch { query, .. } if query == "claude builds"
        )));
        assert!(!app.shell.memory["/work/a"].search_due);
    }

    /// A project's roles are read for every project an agent on the list
    /// works in, once, without opening its board; a role changed anywhere
    /// reads every palette read again, whichever project is on view.
    #[test]
    fn palettes_are_read_for_the_agents_on_view_and_again_on_a_change() {
        let (mut app, commands, messages) = app();
        app.connected = Ok(());
        let agent = |name: &str, root: &str| {
            let mut record = agentdocker_core::AgentRecord::new(
                agentdocker_core::AgentSpec {
                    name: name.into(),
                    runtime: "codex".into(),
                    ..Default::default()
                },
                false,
                Utc::now(),
            );
            record.project = Some(agentdocker_core::ProjectRef::directory(root));
            record
        };
        let palettes_asked = |commands: &CommandReceiver| {
            commands
                .try_iter()
                .filter_map(|c| match c {
                    Cmd::Palette(root) => Some(root),
                    _ => None,
                })
                .collect::<BTreeSet<_>>()
        };
        let agents = vec![agent("a", "/work/a"), agent("b", "/work/b")];
        messages
            .send(Msg::Agents(agents.clone(), BTreeMap::new()))
            .unwrap();
        app.drain();
        assert_eq!(
            palettes_asked(&commands),
            BTreeSet::from(["/work/a".to_owned(), "/work/b".to_owned()])
        );
        messages.send(Msg::Agents(agents, BTreeMap::new())).unwrap();
        app.drain();
        assert!(palettes_asked(&commands).is_empty(), "asked once each");
        // A role changes while the first reads are on their way: they are
        // asked again, so an old answer is not the last.
        app.on_event(Event::new(
            EventKind::RoleChanged {
                project: agentdocker_core::ProjectId::from("a"),
                role: "reviewer".into(),
                by: "user".into(),
                deleted: false,
                tools: None,
            },
            Utc::now(),
        ));
        assert_eq!(
            palettes_asked(&commands),
            BTreeSet::from(["/work/a".to_owned(), "/work/b".to_owned()])
        );
        // A read that failed keeps what was there and is asked again with
        // the next list; one a daemon cannot read means no roles there.
        messages
            .send(Msg::Palette(
                "/work/a".into(),
                PaletteRead::Roles(Vec::new()),
            ))
            .unwrap();
        messages
            .send(Msg::Palette("/work/a".into(), PaletteRead::Failed))
            .unwrap();
        messages
            .send(Msg::Palette("/work/b".into(), PaletteRead::Unsupported))
            .unwrap();
        app.drain();
        assert_eq!(app.palettes.get("/work/a"), Some(&Some(Vec::new())));
        assert_eq!(app.palettes.get("/work/b"), Some(&None));
        messages
            .send(Msg::Agents(
                vec![agent("a", "/work/a"), agent("b", "/work/b")],
                BTreeMap::new(),
            ))
            .unwrap();
        app.drain();
        assert_eq!(
            palettes_asked(&commands),
            BTreeSet::from(["/work/a".to_owned()]),
            "the failed one again, not the answered one"
        );
        // A reconnect asks every palette again with its next list.
        app.connected = Err("gone".into());
        messages.send(Msg::Connected).unwrap();
        app.drain();
        let _ = palettes_asked(&commands);
        messages
            .send(Msg::Agents(
                vec![agent("a", "/work/a"), agent("b", "/work/b")],
                BTreeMap::new(),
            ))
            .unwrap();
        app.drain();
        assert_eq!(
            palettes_asked(&commands),
            BTreeSet::from(["/work/a".to_owned(), "/work/b".to_owned()])
        );
        messages
            .send(Msg::Palette(
                "/work/b".into(),
                PaletteRead::Roles(Vec::new()),
            ))
            .unwrap();
        app.drain();
        // Project A on view; a role in B changes.
        app.shell.catalog.selected = Some("/work/a".into());
        app.on_event(Event::new(
            EventKind::RoleChanged {
                project: agentdocker_core::ProjectId::from("b"),
                role: "reviewer".into(),
                by: "user".into(),
                deleted: false,
                tools: None,
            },
            Utc::now(),
        ));
        assert!(
            palettes_asked(&commands).contains("/work/b"),
            "B's palette is read again though A is on view"
        );
    }

    /// A link opens where it can: a web URL or pull request in the
    /// browser, a path (from the project's folder) in its app, a card in
    /// place; a commit, a message or a memory is copied instead.
    #[test]
    fn a_link_opens_where_it_can_and_is_copied_otherwise() {
        use agentdocker_core::{Link, LinkKind};
        let link = |kind, target: &str| Link {
            kind,
            target: target.into(),
            note: None,
        };
        assert_eq!(
            link_destination(&link(LinkKind::Url, "https://example.com"), None),
            LinkDestination::Open("https://example.com".into())
        );
        assert_eq!(
            link_destination(&link(LinkKind::Pr, "#176"), None),
            LinkDestination::Copy("#176".into()),
            "a bare number names no page"
        );
        assert_eq!(
            link_destination(&link(LinkKind::Path, "src/lib.rs"), Some("/repo")),
            LinkDestination::Open("/repo/src/lib.rs".into())
        );
        assert_eq!(
            link_destination(&link(LinkKind::Task, "abcdef012345"), None),
            LinkDestination::Card(agentdocker_core::TaskId::from("abcdef012345".to_owned()))
        );
        assert_eq!(
            link_destination(&link(LinkKind::Commit, "1a2b3c"), None),
            LinkDestination::Copy("1a2b3c".into())
        );
    }

    /// The board's settings make a rule from what is chosen: an enter rule
    /// on the first lane unless one is chosen, idle days a number, and a
    /// name from the action when none is typed.
    #[test]
    fn a_rule_draft_becomes_a_rule_request() {
        use agentdocker_core::{RuleAction, Trigger};
        let (mut app, _commands, _messages) = app();
        let root = PathBuf::from("/repo");
        app.shell.catalog.projects.push(crate::catalog::Entry {
            project: ProjectRef::directory(&root),
            pinned: true,
            label: None,
        });
        app.shell.catalog.selected = Some(root);
        app.shell.rule_draft.value = "Ready for eyes".into();
        let Ok(Request::RuleCreate {
            name,
            trigger,
            action,
            board,
            ..
        }) = app.rule_request()
        else {
            panic!("a rule")
        };
        assert_eq!(name, "Comment");
        assert_eq!(board.as_deref(), Some("main"));
        assert_eq!(
            trigger,
            Trigger::Enter {
                lane: "backlog".into()
            }
        );
        assert_eq!(
            action,
            RuleAction::Comment {
                text: "Ready for eyes".into()
            }
        );
        app.shell.rule_draft.idle = true;
        app.shell.rule_draft.days = "soon".into();
        assert!(app.rule_request().is_err(), "days are a number");
        app.shell.rule_draft.days = "3".into();
        app.shell.rule_draft.lane = "review".into();
        let _ = app.update(Message::RuleAction(RuleActionKind::Start));
        assert!(
            app.shell.rule_draft.value.is_empty(),
            "the comment's words go"
        );
        assert!(app.rule_request().is_err(), "no installed runtime to start");
    }

    /// Board keys act only on the board, and Esc closes what is open there
    /// innermost first.
    #[test]
    fn board_keys_switch_layout_and_density_and_esc_closes_innermost_first() {
        let (mut app, _commands, _messages) = app();
        app.screen = Screen::Board;
        let press = |app: &mut App, key: keyboard::Key| {
            let _ = app.update(Message::Event(iced::Event::Keyboard(
                keyboard::Event::KeyPressed {
                    key: key.clone(),
                    modified_key: key,
                    physical_key: keyboard::key::Physical::Unidentified(
                        keyboard::key::NativeCode::Unidentified,
                    ),
                    location: keyboard::Location::Standard,
                    modifiers: keyboard::Modifiers::default(),
                    text: None,
                    repeat: false,
                },
            )));
        };
        press(&mut app, keyboard::Key::Character("v".into()));
        assert_eq!(app.shell.board_layout, BoardLayout::Table);
        press(&mut app, keyboard::Key::Character("d".into()));
        assert!(app.shell.board_dense);
        app.shell.board_query = "login".into();
        app.shell.quick_add = Some("ready".into());
        app.shell.card_menu = Some(CardMenu::Start);
        let esc = || keyboard::Key::Named(keyboard::key::Named::Escape);
        press(&mut app, esc());
        assert!(app.shell.card_menu.is_none());
        assert!(app.shell.quick_add.is_some(), "one at a time");
        press(&mut app, esc());
        assert!(app.shell.quick_add.is_none());
        press(&mut app, esc());
        assert!(app.shell.board_query.is_empty());
        // Elsewhere the letters are nobody's.
        app.screen = Screen::Agents;
        press(&mut app, keyboard::Key::Character("v".into()));
        assert_eq!(app.shell.board_layout, BoardLayout::Table);
    }

    /// In a git project the launch form starts a session in its own
    /// worktree unless told otherwise; a plain folder has no branches to
    /// Only the read made for the tool chosen last applies, whatever order
    /// the reads finish in; and an argument naming another model takes the
    /// chips' effort away where that model lacks it.
    #[test]
    fn choices_follow_the_last_tool_and_the_model_in_force() {
        use agentdocker_core::model_choice::{ModelChoices, ModelOption};
        let (mut app, _, _) = app();
        let read = |default: &str| ModelChoices {
            default_model: Some(default.into()),
            default_effort: None,
            models: vec![
                ModelOption {
                    id: "a".into(),
                    name: "A".into(),
                    efforts: vec!["low".into(), "xhigh".into()],
                },
                ModelOption {
                    id: "b".into(),
                    name: "B".into(),
                    efforts: vec!["low".into(), "medium".into()],
                },
            ],
            efforts: vec![],
        };
        let _ = app.update(Message::LaunchRuntime("codex".into()));
        let first = app.shell.launch_choices_generation;
        let _ = app.update(Message::LaunchRuntime("claude-code".into()));
        let claude = app.shell.launch_choices_generation;
        let _ = app.update(Message::LaunchRuntime("codex".into()));
        let last = app.shell.launch_choices_generation;
        let _ = app.update(Message::LaunchChoices(last, "codex".into(), read("fresh")));
        let _ = app.update(Message::LaunchChoices(first, "codex".into(), read("stale")));
        let _ = app.update(Message::LaunchChoices(
            claude,
            "claude-code".into(),
            read("other"),
        ));
        assert_eq!(
            app.launch_choices()
                .and_then(|c| c.default_model.as_deref()),
            Some("fresh"),
            "an earlier read finishing later changes nothing"
        );

        let _ = app.update(Message::LaunchModel(Some("a".into())));
        let _ = app.update(Message::LaunchEffort(Some("xhigh".into())));
        let _ = app.update(Message::LaunchArguments("-m b".into()));
        assert_eq!(
            app.launch_model_now().as_deref(),
            Some("b"),
            "the argument's model"
        );
        assert_eq!(
            app.shell.launch_effort, None,
            "an effort b does not take is never passed to it"
        );
        let _ = app.update(Message::LaunchArguments(
            "-c=model_reasoning_effort=low".into(),
        ));
        assert_eq!(app.launch_given().1.as_deref(), Some("low"));
    }

    /// The model and effort chosen in the form reach the command in the
    /// tool's own options and the record says them; nothing chosen, the
    /// provider's default is said and the command is left alone; the
    /// person's own arguments win, and a new tool starts from its defaults.
    #[test]
    fn a_launch_runs_and_records_the_model_and_effort_chosen() {
        use agentdocker_core::model_choice::{EFFORT_LABEL, ModelChoices, ModelOption};
        let (mut app, _, _) = app();
        let runtime = |name: &str| agentdocker_core::runtime::RuntimeInfo {
            name: name.into(),
            vendor: "fixture".into(),
            label: runtime_label(name),
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
        };
        app.runtimes = vec![runtime("codex"), runtime("claude-code")];
        app.shell
            .catalog
            .pin(ProjectRef::directory("/plain"))
            .unwrap();
        let _ = app.update(Message::LaunchRuntime("codex".into()));
        let generation = app.shell.launch_choices_generation;
        let _ = app.update(Message::LaunchChoices(
            generation,
            "codex".into(),
            ModelChoices {
                default_model: Some("gpt-6-astra".into()),
                default_effort: Some("xhigh".into()),
                models: vec![
                    ModelOption {
                        id: "gpt-6-astra".into(),
                        name: "GPT-6-Astra".into(),
                        efforts: vec!["low".into(), "xhigh".into()],
                    },
                    ModelOption {
                        id: "gpt-6-luna".into(),
                        name: "GPT-6-Luna".into(),
                        efforts: vec!["low".into(), "medium".into()],
                    },
                ],
                efforts: vec![],
            },
        ));
        let spec = app.launch_spec().unwrap();
        assert_eq!(
            spec.command,
            ["/fixture/provider"],
            "the provider's own default"
        );
        assert_eq!(spec.model.as_deref(), Some("gpt-6-astra"));
        assert_eq!(
            spec.labels.get(EFFORT_LABEL).map(String::as_str),
            Some("xhigh")
        );

        let _ = app.update(Message::LaunchEffort(Some("xhigh".into())));
        let _ = app.update(Message::LaunchModel(Some("gpt-6-luna".into())));
        assert_eq!(
            app.shell.launch_effort, None,
            "an effort the new model lacks goes"
        );
        let _ = app.update(Message::LaunchEffort(Some("medium".into())));
        let spec = app.launch_spec().unwrap();
        assert_eq!(
            spec.command,
            [
                "/fixture/provider",
                "--model",
                "gpt-6-luna",
                "-c",
                "model_reasoning_effort=\"medium\""
            ]
        );
        assert_eq!(spec.model.as_deref(), Some("gpt-6-luna"));
        assert_eq!(
            spec.labels.get(EFFORT_LABEL).map(String::as_str),
            Some("medium")
        );
        assert_eq!(
            view::model_and_effort(&AgentRecord::new(spec, true, Utc::now())).as_deref(),
            Some("gpt-6-luna · medium")
        );

        let _ = app.update(Message::LaunchArguments("-m o9".into()));
        let spec = app.launch_spec().unwrap();
        assert_eq!(
            spec.command,
            [
                "/fixture/provider",
                "-c",
                "model_reasoning_effort=\"medium\"",
                "-m",
                "o9"
            ],
            "the person's own model wins"
        );
        assert_eq!(spec.model.as_deref(), Some("o9"));

        let _ = app.update(Message::LaunchArguments(String::new()));
        let _ = app.update(Message::LaunchRuntime("claude-code".into()));
        assert_eq!(
            (
                app.shell.launch_model.as_deref(),
                app.shell.launch_effort.as_deref()
            ),
            (None, None),
            "another tool starts from its own defaults"
        );
    }

    /// give, so it is never asked for one.
    #[test]
    fn a_git_project_launches_into_its_own_worktree_by_default() {
        let (mut app, _, _) = app();
        app.runtimes = {
            let name = "claude-code";
            vec![agentdocker_core::runtime::RuntimeInfo {
                name: name.into(),
                vendor: "fixture".into(),
                label: runtime_label(name),
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
            }]
        };
        app.shell.launch_runtime = Some("claude-code".into());
        let git = ProjectRef {
            root: "/repo".into(),
            worktree: None,
            fingerprint: None,
            source: agentdocker_core::ProjectSource::Git,
        };
        app.shell.catalog.pin(git).unwrap();
        let _ = app.update(Message::ShowLaunch);
        assert!(app.shell.launch_worktree);
        assert!(app.launch_spec().unwrap().isolate);
        let _ = app.update(Message::LaunchWorktree(false));
        assert!(!app.launch_spec().unwrap().isolate);
        let _ = app.update(Message::ShowLaunch);

        app.shell
            .catalog
            .pin(ProjectRef::directory("/plain"))
            .unwrap();
        let _ = app.update(Message::ShowLaunch);
        assert!(!app.shell.launch_worktree);
        let _ = app.update(Message::LaunchWorktree(true));
        assert!(
            !app.launch_spec().unwrap().isolate,
            "a folder has no branches"
        );
    }

    #[test]
    fn a_menu_opened_by_pointer_keeps_focus_and_navigation_closes_the_launch_menu() {
        let (mut app, _commands, _messages) = app();
        // A pointer press on the trigger marks it; a key press forgets it.
        let _ = app.update(Message::Focus("project-more".into()));
        assert_eq!(app.shell.pressed.as_deref(), Some("project-more"));
        let _ = app.update(Message::More);
        assert!(app.shell.more);
        let _ = app.update(Message::Event(iced::Event::Keyboard(
            keyboard::Event::KeyPressed {
                key: keyboard::Key::Named(keyboard::key::Named::Tab),
                modified_key: keyboard::Key::Named(keyboard::key::Named::Tab),
                physical_key: keyboard::key::Physical::Unidentified(
                    keyboard::key::NativeCode::Unidentified,
                ),
                location: keyboard::Location::Standard,
                modifiers: keyboard::Modifiers::empty(),
                text: None,
                repeat: false,
            },
        )));
        assert_eq!(app.shell.pressed, None, "the keyboard is back in charge");
        // The Launch agent menu never outlives the screen it opened on.
        let _ = app.update(Message::LaunchMenu);
        assert!(app.shell.launch_menu);
        let _ = app.update(Message::Navigate(Screen::Settings));
        assert!(!app.shell.launch_menu);
        let _ = app.update(Message::LaunchMenu);
        app.reset_session_view();
        assert!(!app.shell.launch_menu);
    }

    /// Choosing an editor opens the folder in it — Launch Services given
    /// the bundle and the folder, one argument each — and remembers it with
    /// the workspace preferences; an editor no longer installed opens
    /// nothing; a menu closes on a second press and never outlives its
    /// screen; a failure is said where errors are.
    #[test]
    fn choosing_an_editor_opens_the_folder_in_it_and_remembers_it() {
        let (mut app, _commands, _messages) = app();
        app.shell.editors = agentdocker_core::editor::from_bundles(
            ["/Applications/Zed.app", "/Applications/RustRover.app"].map(PathBuf::from),
        );
        let folder = PathBuf::from("/work/alpha");
        let _ = app.update(Message::EditorMenu(EditorMenu::Project));
        assert_eq!(app.shell.editor_menu, Some(EditorMenu::Project));
        let saved = app.shell.generation;
        let _ = app.update(Message::OpenIn("rustrover".into(), folder.clone()));
        assert_eq!(app.shell.editor_menu, None);
        assert_eq!(app.shell.catalog.editor.as_deref(), Some("rustrover"));
        assert_ne!(app.shell.generation, saved, "the choice is saved");
        assert_eq!(app.status, "Opening in RustRover…");
        let editor = app.choose_editor("rustrover").unwrap();
        assert_eq!(
            editor.argv(&folder),
            [
                "/usr/bin/open",
                "-a",
                "/Applications/RustRover.app",
                "/work/alpha"
            ]
            .map(std::ffi::OsString::from)
        );
        let saved = app.shell.generation;
        let _ = app.update(Message::OpenIn("rustrover".into(), folder.clone()));
        assert_eq!(app.shell.generation, saved, "the same choice saves nothing");

        app.status.clear();
        let _ = app.update(Message::OpenIn("xcode".into(), folder.clone()));
        assert_eq!(app.shell.catalog.editor.as_deref(), Some("rustrover"));
        assert!(app.status.is_empty(), "nothing opened");

        let _ = app.update(Message::EditorMenu(EditorMenu::Session));
        assert_eq!(app.shell.editor_menu, Some(EditorMenu::Session));
        let _ = app.update(Message::EditorMenu(EditorMenu::Session));
        assert_eq!(app.shell.editor_menu, None);
        let _ = app.update(Message::EditorMenu(EditorMenu::Project));
        let _ = app.update(Message::Navigate(Screen::Settings));
        assert_eq!(app.shell.editor_menu, None);

        let _ = app.update(Message::EditorOpened(
            Err("Could not open Zed: gone".into()),
        ));
        assert_eq!(app.shell.error.as_deref(), Some("Could not open Zed: gone"));
        let _ = app.update(Message::EditorOpened(Ok("Opened in Zed".into())));
        assert_eq!(app.status, "Opened in Zed");
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

    /// The card editor sends only what the person changed, so a field
    /// somebody else changed meanwhile is left alone; after a save goes
    /// through, the next sends only what changed since; one that failed
    /// leaves its fields to send again.
    #[test]
    fn a_card_save_sends_only_what_the_person_changed() {
        let (mut app, requests, messages) = app();
        app.connected = Ok(());
        let id = agentdocker_core::TaskId::from("aaaaaaaaaaaa".to_owned());
        let words = CardWords {
            title: "Port the parser".into(),
            description: "old words".into(),
            acceptance: String::new(),
            due: String::new(),
            blocker: String::new(),
            tags: "parser".into(),
        };
        app.shell.card_draft = CardDraft {
            task: Some(id.clone()),
            generation: 1,
            title: words.title.clone(),
            description: words.description.clone(),
            acceptance: words.acceptance.clone(),
            due: words.due.clone(),
            blocker: words.blocker.clone(),
            tags: words.tags.clone(),
            base: words,
            comment: String::new(),
        };
        app.task_open = Some(id.clone());
        let saved = |requests: &CommandReceiver| {
            requests
                .try_iter()
                .find_map(|c| match c {
                    Cmd::TaskEdit { change, .. } => Some(change),
                    _ => None,
                })
                .expect("a save")
        };
        let _ = app.update(Message::CardField(
            CardFieldKind::Description,
            "new words".into(),
        ));
        let _ = app.update(Message::CardSave);
        let change = saved(&requests);
        assert_eq!(change.description.as_deref(), Some("new words"));
        assert!(
            change.title.is_none() && change.tags.is_none() && change.acceptance.is_none(),
            "nothing the person left alone: {change:?}"
        );
        messages
            .send(Msg::CardEdited(id.clone(), change, Some(1), Ok(())))
            .unwrap();
        app.drain();
        let _ = app.update(Message::CardField(
            CardFieldKind::Acceptance,
            "tests pass".into(),
        ));
        let _ = app.update(Message::CardSave);
        let change = saved(&requests);
        assert_eq!(change.acceptance.as_deref(), Some("tests pass"));
        assert!(
            change.description.is_none(),
            "the first save is not sent again"
        );
        messages
            .send(Msg::CardEdited(
                id.clone(),
                change,
                Some(1),
                Err("refused".into()),
            ))
            .unwrap();
        app.drain();
        let _ = app.update(Message::CardSave);
        let change = saved(&requests);
        assert_eq!(
            change.acceptance.as_deref(),
            Some("tests pass"),
            "a save that failed is sent again"
        );

        // The card is closed and opened again before that save answers:
        // the reply belongs to the earlier opening and moves nothing of
        // the new one, so its untouched title is never sent.
        let reopened = CardWords {
            title: "Port the parser".into(),
            ..CardWords::default()
        };
        app.shell.card_draft = CardDraft {
            task: Some(id.clone()),
            generation: 2,
            title: reopened.title.clone(),
            base: reopened,
            ..CardDraft::default()
        };
        let mut retitled = change;
        retitled.title = Some("Rewrite the parser".into());
        messages
            .send(Msg::CardEdited(id.clone(), retitled, Some(1), Ok(())))
            .unwrap();
        app.drain();
        assert_eq!(app.shell.card_draft.base.title, "Port the parser");
        let _ = app.update(Message::CardField(
            CardFieldKind::Description,
            "newer words".into(),
        ));
        let _ = app.update(Message::CardSave);
        let change = saved(&requests);
        assert_eq!(change.description.as_deref(), Some("newer words"));
        assert!(
            change.title.is_none(),
            "the later opening's untouched title is not sent"
        );
    }

    /// A card's draft is the project's: text typed for one board waits
    /// while another is on view, a move or hand of some other card does
    /// not clear it, a late reply to an earlier filing does not take
    /// newer text, a filing that cannot be queued says so instead of
    /// staying "Adding…", and a board that cannot be read stays as last
    /// read.
    #[test]
    fn a_card_draft_survives_other_board_actions_late_replies_and_refused_queues() {
        let (mut app, requests, messages) = app();
        app.connected = Ok(());
        let dir = tempfile::tempdir().unwrap();
        let alpha = agentdocker_core::ProjectRef::directory(dir.path().join("alpha"));
        let beta = agentdocker_core::ProjectRef::directory(dir.path().join("beta"));
        app.shell.catalog.remember(alpha.clone(), true).unwrap();
        app.shell.catalog.remember(beta.clone(), true).unwrap();
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
            key: String::new(),
            board: None,
            description: String::new(),
            lane: None,
            rank: String::new(),
            priority: Default::default(),
            size: None,
            due: None,
            external_blocker: String::new(),
            parent: None,
            blocked_by: Vec::new(),
            tags: Vec::new(),
            plan: None,
            lane_since: None,
            fired: Vec::new(),
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
            "review".to_owned(),
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

        // A lane's quick add types into the same saved draft and files it
        // in that lane; it stays open for the next card.
        let _ = app.update(Message::QuickAdd(Some("review".into())));
        let _ = app.update(Message::TaskTitle("From the review lane".into()));
        assert!(
            app.shell
                .draft_snapshot()
                .boards
                .contains_key(alpha_root.as_str()),
            "saved with the drafts"
        );
        let _ = app.update(Message::QuickAddFile);
        assert!(requests.try_iter().any(|c| matches!(
            c,
            Cmd::TaskCreate { ref lane, ref title, .. }
                if lane.as_deref() == Some("review") && title == "From the review lane"
        )));
        assert_eq!(app.shell.quick_add.as_deref(), Some("review"));
        let request = app.shell.task_drafts[&alpha_root].sending.expect("filing");
        messages
            .send(Msg::TaskCreated(alpha_root.clone(), request, Ok(())))
            .unwrap();
        app.drain();
        assert_eq!(app.shell.task_drafts[&alpha_root].title, "");

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
                lane: None,
                board: None,
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
        app.shell.catalog.remember(alpha.clone(), true).unwrap();
        app.shell.catalog.remember(beta.clone(), true).unwrap();
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
                    key: String::new(),
                    board: None,
                    description: String::new(),
                    lane: None,
                    rank: String::new(),
                    priority: Default::default(),
                    size: None,
                    due: None,
                    external_blocker: String::new(),
                    parent: None,
                    blocked_by: Vec::new(),
                    tags: Vec::new(),
                    plan: None,
                    lane_since: None,
                    fired: Vec::new(),
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
        app.shell.catalog.remember(alpha.clone(), true).unwrap();
        app.shell.catalog.remember(beta.clone(), false).unwrap();
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
        assert_eq!(
            app.shell.catalog.remember(beta, false),
            Err(crate::catalog::Refusal::Removed),
            "kept off the list"
        );
    }

    #[test]
    fn secret_submission_and_disconnect_never_save_or_restore_the_ordinary_answer_draft() {
        use agentdocker_core::{
            ProcessIdentity,
            secret::{SecretField, SecretReview, SecretReviewSpec, SecretText},
        };
        let (mut app, commands, messages) = app();
        app.connected = Ok(());
        let now = Utc::now();
        let review = SecretReview {
            id: "temporary".into(),
            agent: "asker".into(),
            recipient: "human".into(),
            owner: ProcessIdentity {
                pid: 1,
                started_at: now,
            },
            expires_at: now + chrono::Duration::minutes(5),
            request: SecretReviewSpec {
                thread: "thread".into(),
                turn: "turn".into(),
                fields: vec![SecretField {
                    id: "secret".into(),
                    question: "Temporary value?".into(),
                    is_secret: true,
                }],
            },
        };
        app.shell
            .answers
            .insert("ordinary".to_owned().into(), "preserve me".into());
        let before = app.shell.draft_snapshot();
        app.secrets.refresh(vec![review.clone()], now);
        let edit = Message::SecretEdit(
            "temporary".into(),
            "secret".into(),
            Some(SecretText::new("invented-UI-canary".into()).unwrap()),
        );
        assert!(!format!("{edit:?}").contains("invented-UI-canary"));
        let _ = app.update(edit);
        let _ = app.update(Message::SecretSubmit("temporary".into()));
        assert_eq!(commands.try_iter().count(), 0, "consent is required");
        assert_eq!(app.shell.draft_snapshot(), before);
        let _ = app.update(Message::SecretAcknowledge("temporary".into(), true));
        let _ = app.update(Message::SecretSubmit("temporary".into()));
        let submitted: Vec<_> = commands.try_iter().collect();
        assert!(matches!(submitted.as_slice(),[Cmd::SecretAnswer(id, _)] if id=="temporary"));
        assert!(!format!("{submitted:?}").contains("invented-UI-canary"));
        assert!(app.secrets.drafts.is_empty());
        assert_eq!(app.shell.draft_snapshot(), before);
        let _ = app.update(Message::SecretSubmit("temporary".into()));
        assert_eq!(
            commands.try_iter().count(),
            0,
            "no double click resubmission"
        );
        messages.send(Msg::Disconnected("fixture".into())).unwrap();
        app.drain();
        assert!(app.secrets.drafts.is_empty() && app.secrets.reviews.is_empty());
        assert_eq!(app.shell.draft_snapshot(), before);
    }

    #[test]
    fn file_approval_requires_open_review_and_preserves_drafts_on_stale_clicks() {
        use agentdocker_core::{QuestionFileChange, QuestionFileChangeKind, QuestionPresentation};
        // Both answers that let the change through wait for the diff.
        for (for_session, grant) in [
            (false, "Allow"),
            (true, agentdocker_core::ALLOW_FOR_SESSION),
        ] {
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
                for_session,
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
            let _ = app.update(Message::AnswerChoice(id.clone(), grant.into()));
            assert_eq!(app.shell.answers[&id], "original draft");
            assert_eq!(commands.try_iter().count(), 0, "{grant}");
            let typed = format!(" {grant} ");
            app.shell.answers.insert(id.clone(), typed.clone());
            let _ = app.update(Message::Answer(id.clone()));
            assert_eq!(
                commands.try_iter().count(),
                0,
                "keyboard submit also requires reviewing the diff: {grant}"
            );
            let _ = app.update(Message::ReviewFiles(id.clone()));
            assert_eq!(app.shell.file_review, Some(id.clone()));
            let _ = app.update(Message::AnswerChoice(id.clone(), grant.into()));
            assert!(
                matches!(commands.try_iter().collect::<Vec<_>>().as_slice(),[Cmd::Answer(answer,text)] if answer == &id && text == grant)
            );
            messages.send(Msg::Questions(Vec::new())).unwrap();
            let _ = app.update(Message::Tick);
            assert!(app.shell.file_review.is_none());
            assert_eq!(
                app.shell.answers[&id], typed,
                "the typed draft stays retained while the explicit choice is in flight"
            );
        }
    }

    #[test]
    fn structured_choices_use_one_answer_command_and_reject_stale_or_invalid_clicks() {
        let (mut app, commands, _) = app();
        app.connected = Ok(());
        let presentation = agentdocker_core::QuestionPresentation::CodexCommand {
            command: "printf hello".into(),
            cwd: "/owned".into(),
            reason: "Fixture".into(),
            for_session: false,
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

    /// Deny with words: the denial is the one answer sent, and the words
    /// go to the agent as a message only once the denial is accepted.
    /// Allow sends none.
    #[test]
    fn a_denial_carries_its_note_to_the_agent_once_accepted() {
        let (mut app, commands, messages) = app();
        app.connected = Ok(());
        for (choice, noted) in [("Deny", true), ("Allow", false)] {
            let presentation = agentdocker_core::QuestionPresentation::CodexCommand {
                command: "rm -rf build".into(),
                cwd: "/owned".into(),
                reason: "Fixture".into(),
                for_session: false,
            };
            let id = MessageId::from(format!("question-{choice}"));
            app.questions.push(Question {
                id: id.clone(),
                from: "asker".into(),
                to: agentdocker_core::Destination::Agent("user".into()),
                text: presentation.text(),
                presentation: Some(presentation),
                asked_at: Utc::now(),
                expires_at: Utc::now() + chrono::Duration::minutes(5),
            });
            let _ = app.update(Message::Draft(
                id.clone(),
                "  Use the fixture script instead  ".into(),
            ));
            let _ = app.update(Message::AnswerChoice(id.clone(), choice.into()));
            assert!(
                matches!(commands.try_iter().collect::<Vec<_>>().as_slice(), [Cmd::Answer(message, answer)] if message == &id && answer == choice),
                "{choice}: the choice alone is the answer"
            );
            messages.send(Msg::Answered(id.clone(), Ok(()))).unwrap();
            app.drain();
            let told = commands.try_iter().any(|cmd| {
                matches!(cmd, Cmd::DenyNote(question, to, text) if question == id && to == "asker" && text == "Denied `rm -rf build`: Use the fixture script instead")
            });
            assert_eq!(told, noted, "{choice}");
            assert!(!app.shell.answers.contains_key(&id));
            assert_eq!(
                app.shell.deny_notes.contains_key(&id),
                noted,
                "kept until taken"
            );
        }
    }

    /// A note refused after its denial was accepted is not lost: it goes
    /// into the agent's message box. Its completion never touches the
    /// composer's own pending send; a refused note waits for that send to
    /// finish before it goes in.
    #[test]
    fn a_refused_denial_note_lands_in_the_message_box_and_leaves_other_sends_alone() {
        let (mut app, commands, messages) = app();
        app.connected = Ok(());
        let note = "Denied `rm -rf build`: Use the fixture script instead";
        let deny = |app: &mut App, id: &str| -> MessageId {
            let presentation = agentdocker_core::QuestionPresentation::CodexCommand {
                command: "rm -rf build".into(),
                cwd: "/owned".into(),
                reason: "Fixture".into(),
                for_session: false,
            };
            let id = MessageId::from(id.to_owned());
            app.questions.push(Question {
                id: id.clone(),
                from: "asker".into(),
                to: agentdocker_core::Destination::Agent("user".into()),
                text: presentation.text(),
                presentation: Some(presentation),
                asked_at: Utc::now(),
                expires_at: Utc::now() + chrono::Duration::minutes(5),
            });
            let _ = app.update(Message::Draft(
                id.clone(),
                "Use the fixture script instead".into(),
            ));
            let _ = app.update(Message::AnswerChoice(id.clone(), "Deny".into()));
            id
        };

        // (1) The denial is accepted, then the note is refused.
        let first = deny(&mut app, "first");
        messages.send(Msg::Answered(first.clone(), Ok(()))).unwrap();
        app.drain();
        messages
            .send(Msg::DenyNoteSent(
                first.clone(),
                Err(NoteFailure {
                    reason: "the project is paused".into(),
                    certain: true,
                }),
            ))
            .unwrap();
        app.drain();
        assert!(app.shell.deny_notes.is_empty());
        assert_eq!(app.shell.session_drafts["asker"].draft.text, note);
        let _ = commands.try_iter().count();

        // (2) An ordinary send to the same agent is in flight while a
        // second note comes back: taken, it leaves that send alone;
        // refused, it waits for that send, then joins the message box.
        let _ = app.update(Message::SessionDraft("asker".into(), "hello".into()));
        let _ = app
            .shell
            .session_drafts
            .get_mut("asker")
            .unwrap()
            .draft
            .begin();
        let second = deny(&mut app, "second");
        messages
            .send(Msg::Answered(second.clone(), Ok(())))
            .unwrap();
        app.drain();
        messages
            .send(Msg::DenyNoteSent(second.clone(), Ok(())))
            .unwrap();
        app.drain();
        let entry = &app.shell.session_drafts["asker"];
        assert_eq!(
            entry.draft.sending.as_deref(),
            Some("hello"),
            "still its own"
        );
        assert_eq!(entry.draft.text, "hello");

        let third = deny(&mut app, "third");
        messages.send(Msg::Answered(third.clone(), Ok(()))).unwrap();
        app.drain();
        messages
            .send(Msg::DenyNoteSent(
                third.clone(),
                Err(NoteFailure {
                    reason: "disconnected".into(),
                    certain: false,
                }),
            ))
            .unwrap();
        app.drain();
        assert!(
            app.shell.deny_notes.contains_key(&third),
            "waits for the send"
        );
        assert_eq!(app.shell.session_drafts["asker"].draft.text, "hello");
        messages
            .send(Msg::SessionSent(
                "asker".into(),
                Ok(super::super::QueuedSend {
                    message: MessageId::from("sent".to_owned()),
                    readiness: None,
                }),
            ))
            .unwrap();
        app.drain();
        assert!(app.shell.deny_notes.is_empty());
        assert_eq!(app.shell.session_drafts["asker"].draft.text, note);
        assert!(app.shell.session_drafts["asker"].draft.sending.is_none());
        assert!(
            app.status.contains("may have been sent"),
            "a lost answer is not a refusal: {}",
            app.status
        );
    }

    /// Between an accepted denial and its note's answer the note is saved
    /// with the drafts: a window closed then finds it in the agent's
    /// message box on reopening, unsent, and sends nothing on its own.
    /// Delivered, its saved copy goes.
    #[test]
    fn a_denial_note_survives_a_reopen_until_it_is_certainly_sent() {
        let home = tempfile::tempdir().unwrap();
        let (mut app, commands, messages) = app();
        app.shell = State::load(home.path());
        app.connected = Ok(());
        let presentation = agentdocker_core::QuestionPresentation::CodexCommand {
            command: "rm -rf build".into(),
            cwd: "/owned".into(),
            reason: "Fixture".into(),
            for_session: false,
        };
        let id = MessageId::from("question-reopen".to_owned());
        app.questions.push(Question {
            id: id.clone(),
            from: "asker".into(),
            to: agentdocker_core::Destination::Agent("user".into()),
            text: presentation.text(),
            presentation: Some(presentation),
            asked_at: Utc::now(),
            expires_at: Utc::now() + chrono::Duration::minutes(5),
        });
        let _ = app.update(Message::SessionDraft(
            "asker".into(),
            "half a thought".into(),
        ));
        let _ = app.update(Message::Draft(id.clone(), "Use the fixture script".into()));
        let _ = app.update(Message::AnswerChoice(id.clone(), "Deny".into()));
        messages.send(Msg::Answered(id.clone(), Ok(()))).unwrap();
        app.drain();
        assert!(app.shell.deny_notes.contains_key(&id), "on its way");
        let saved = app.shell.draft_snapshot();
        assert_eq!(
            saved.sessions["asker"],
            "half a thought\n\nDenied `rm -rf build`: Use the fixture script"
        );
        assert!(!saved.answers.contains_key(id.as_str()));
        saved.save(&app.shell.draft_home).unwrap();
        let _ = commands.try_iter().count();
        app.shell = State::load(home.path());
        assert_eq!(
            app.shell.session_drafts["asker"].draft.text,
            "half a thought\n\nDenied `rm -rf build`: Use the fixture script"
        );
        assert!(app.shell.session_drafts["asker"].draft.sending.is_none());
        assert_eq!(commands.try_iter().count(), 0, "nothing resent");

        // Delivered: the saved copy goes with it.
        let (mut app, _commands, messages) = self::app();
        app.connected = Ok(());
        app.shell.deny_notes.insert(
            id.clone(),
            DenyNote {
                agent: "asker".into(),
                text: "Denied: why".into(),
                failed: None,
            },
        );
        assert!(app.shell.drafts.clean());
        messages
            .send(Msg::DenyNoteSent(id.clone(), Ok(())))
            .unwrap();
        app.drain();
        assert!(app.shell.deny_notes.is_empty());
        assert!(!app.shell.draft_snapshot().sessions.contains_key("asker"));
        assert!(!app.shell.drafts.clean(), "saved again, without it");
    }

    /// A note the worker queue would not take never left the window: it
    /// is certainly unsent, and goes to the agent's message box.
    #[test]
    fn a_denial_note_the_queue_rejects_goes_to_the_message_box() {
        let (mut app, _commands, messages) = app();
        app.connected = Ok(());
        let id = MessageId::from("question-full".to_owned());
        let presentation = agentdocker_core::QuestionPresentation::CodexCommand {
            command: "make clean".into(),
            cwd: "/owned".into(),
            reason: "Fixture".into(),
            for_session: false,
        };
        app.questions.push(Question {
            id: id.clone(),
            from: "asker".into(),
            to: agentdocker_core::Destination::Agent("user".into()),
            text: presentation.text(),
            presentation: Some(presentation),
            asked_at: Utc::now(),
            expires_at: Utc::now() + chrono::Duration::minutes(5),
        });
        let _ = app.update(Message::Draft(id.clone(), "Not the cache".into()));
        let _ = app.update(Message::AnswerChoice(id.clone(), "Deny".into()));
        for _ in 0..super::super::queue::CAPACITY {
            app.send(Cmd::Stop("fixture".into()));
        }
        messages.send(Msg::Answered(id.clone(), Ok(()))).unwrap();
        app.drain();
        assert!(app.shell.deny_notes.is_empty());
        assert_eq!(
            app.shell.session_drafts["asker"].draft.text,
            "Denied `make clean`: Not the cache"
        );
        assert!(app.status.contains("was not sent"), "{}", app.status);
    }

    /// Command+N opens the launch form, as in every session manager, and
    /// the dock icon counts the live questions waiting on the person,
    /// set again only when the count changes.
    #[test]
    fn command_n_launches_and_the_dock_counts_what_waits_on_you() {
        let (mut app, _, _) = app();
        let key = keyboard::Key::Character("n".into());
        let press = |modifiers| {
            Message::Event(iced::Event::Keyboard(keyboard::Event::KeyPressed {
                modified_key: key.clone(),
                key: key.clone(),
                physical_key: keyboard::key::Physical::Unidentified(
                    keyboard::key::NativeCode::Unidentified,
                ),
                location: keyboard::Location::Standard,
                modifiers,
                text: None,
                repeat: false,
            }))
        };
        let _ = app.update(press(keyboard::Modifiers::empty()));
        assert!(!app.shell.launch, "a plain n is typing, not a shortcut");
        let _ = app.update(press(keyboard::Modifiers::COMMAND));
        assert!(app.shell.launch);
        let _ = app.update(press(keyboard::Modifiers::COMMAND));
        assert!(app.shell.launch, "pressing it again keeps the form open");

        assert_eq!(app.shell.dock_badge, Some(0));
        let question = |id: &str, minutes: i64| Question {
            id: MessageId::from(id.to_owned()),
            from: "asker".into(),
            to: agentdocker_core::Destination::Agent("user".into()),
            text: "Which branch?".into(),
            presentation: None,
            asked_at: Utc::now(),
            expires_at: Utc::now() + chrono::Duration::minutes(minutes),
        };
        app.questions.push(question("live", 5));
        app.questions.push(question("expired", -1));
        let _ = app.update(Message::Event(iced::Event::Window(window::Event::Focused)));
        assert_eq!(app.waiting_on_you(), 1);
        assert_eq!(app.shell.dock_badge, Some(1));

        // A session stopped at a permission prompt in its terminal waits
        // on the person as much as a question does.
        let mut prompt = AgentRecord::new(
            agentdocker_core::AgentSpec {
                runtime: "claude-code".into(),
                ..Default::default()
            },
            false,
            Utc::now(),
        );
        prompt.id = "prompted".into();
        prompt.status = agentdocker_core::AgentStatus::Running;
        app.agents.push(prompt);
        app.activity.insert(
            "prompted".into(),
            agentdocker_core::Activity::Waiting {
                since: Utc::now(),
                reason: None,
            },
        );
        let _ = app.update(Message::Event(iced::Event::Window(window::Event::Focused)));
        assert_eq!(app.waiting_on_you(), 2);
        assert_eq!(app.shell.dock_badge, Some(2));
    }

    /// A launched agent's terminal opens as soon as the list shows it
    /// running, so what it asks before it starts is in front of the person.
    #[test]
    fn a_card_follows_its_holders_pull_request_once() {
        use crate::pull_request::{Key, PullRequest, Status};
        use agentdocker_core::task::Column;
        let (mut app, commands, _messages) = app();
        let project = agentdocker_core::ProjectRef::directory("/work/alpha");
        let mut agent = AgentRecord::new(
            agentdocker_core::AgentSpec {
                name: "worker".into(),
                runtime: "codex".into(),
                ..Default::default()
            },
            false,
            Utc::now(),
        );
        agent.id = "worker".into();
        agent.status = agentdocker_core::AgentStatus::Running;
        agent.project = Some(project.clone());
        agent.vcs = Some(agentdocker_core::VcsState {
            branch: Some("feat/x".into()),
            head: None,
            dirty: None,
            updated_at: Utc::now(),
        });
        app.agents.push(agent);
        let mut card = agentdocker_core::Task::new(
            project.id(),
            "Ship it",
            "",
            Some(Column::InProgress),
            "user",
            Vec::new(),
            Utc::now(),
        )
        .unwrap();
        card.assignee = Some("worker".into());
        let task = card.id.clone();
        app.tasks = Some(crate::app::Board {
            project: project.id().to_string(),
            cards: vec![card],
            more: false,
            pending_more: None,
        });
        let key = Key::new(std::path::Path::new("/work/alpha"), "feat/x").unwrap();
        let answer = |status| {
            Message::PullRequests(vec![(
                key.clone(),
                Ok(Some(PullRequest {
                    number: 7,
                    url: "https://example.com/7".into(),
                    status,
                    head: None,
                })),
            )])
        };
        let moves = |commands: &CommandReceiver| -> Vec<Column> {
            commands
                .try_iter()
                .filter_map(|cmd| match cmd {
                    Cmd::TaskMove { to, .. } => Column::parse(&to),
                    _ => None,
                })
                .collect()
        };
        let _ = app.update(answer(Status::Running));
        assert_eq!(moves(&commands), [Column::Review], "a pull request opened");
        let _ = app.update(answer(Status::Running));
        assert!(moves(&commands).is_empty(), "asked once");
        let _ = app.update(answer(Status::Merged));
        assert_eq!(moves(&commands), [Column::Done], "merged");
        assert!(
            app.shell
                .board_moved
                .contains(&(task.to_string(), Column::Done))
        );
    }

    #[test]
    fn asking_an_agent_to_repair_sends_it_the_words() {
        let (mut app, commands, _messages) = app();
        app.connected = Ok(());
        let _ = commands.try_iter().count();
        let _ = app.update(Message::AskAgent(
            "worker".into(),
            "Please resolve the conflicts.".into(),
        ));
        assert!(commands.try_iter().any(|cmd| matches!(cmd,
            Cmd::SessionSend(id, text) if id == "worker" && text == "Please resolve the conflicts.")));
    }

    #[test]
    fn next_attention_walks_every_agent_that_calls_for_the_person() {
        let (mut app, _commands, _messages) = app();
        // Opening a project marks its finished agents seen, so the two
        // that call for the person are in different projects.
        for (name, root) in [
            ("first", "/work/alpha"),
            ("quiet", "/work/alpha"),
            ("second", "/work/beta"),
            ("removed", "/work/gone"),
        ] {
            let mut agent = AgentRecord::new(
                agentdocker_core::AgentSpec {
                    name: name.into(),
                    runtime: "claude-code".into(),
                    ..Default::default()
                },
                false,
                Utc::now(),
            );
            agent.id = name.into();
            agent.status = agentdocker_core::AgentStatus::Running;
            agent.project = Some(agentdocker_core::ProjectRef::directory(root));
            app.agents.push(agent);
        }
        app.shell.unviewed_done.insert("first".into());
        app.shell.unviewed_done.insert("second".into());
        // A session in a folder the person removed is not walked to, so
        // the folder is not brought back behind their back.
        app.shell.catalog.hidden.push("/work/gone".into());
        app.shell.unviewed_done.insert("removed".into());
        let _ = app.update(Message::NextAttention);
        assert_eq!(app.shell.selected.as_deref(), Some("first"));
        let _ = app.update(Message::NextAttention);
        assert_eq!(app.shell.selected.as_deref(), Some("second"));
        // Both have been looked at now: nothing is left to walk to.
        let _ = app.update(Message::NextAttention);
        assert_eq!(app.shell.selected.as_deref(), Some("second"));
        assert_eq!(
            app.shell.unviewed_done,
            ["removed".to_owned()].into_iter().collect()
        );
        assert_eq!(app.shell.catalog.hidden, [PathBuf::from("/work/gone")]);
    }

    #[test]
    fn the_launch_prompt_is_the_new_agents_first_message() {
        let (mut app, commands, messages) = app();
        let _ = app.update(Message::LaunchPrompt("Fix the flaky test".into()));
        let _ = commands.try_iter().count();
        messages.send(Msg::Launched(Ok("fresh".into()))).unwrap();
        app.drain();
        assert!(
            commands
                .try_iter()
                .any(|cmd| matches!(cmd, Cmd::SessionSend(id, text) if id == "fresh" && text == "Fix the flaky test")),
            "the prompt is queued to the agent it launched"
        );
        assert!(
            app.shell.launch_prompt.is_empty(),
            "and the form starts empty next time"
        );
        // No prompt, no message.
        messages.send(Msg::Launched(Ok("bare".into()))).unwrap();
        app.drain();
        assert!(
            !commands
                .try_iter()
                .any(|cmd| matches!(cmd, Cmd::SessionSend(..)))
        );
    }

    #[test]
    fn a_launched_agent_opens_in_its_terminal_once_it_is_listed() {
        let (mut app, _commands, messages) = app();
        messages.send(Msg::Launched(Ok("fresh".into()))).unwrap();
        app.drain();
        assert_eq!(app.shell.attach_when_listed, Some(("fresh".into(), None)));
        assert_ne!(app.screen, Screen::Terminal);
        let mut agent = AgentRecord::new(
            agentdocker_core::AgentSpec {
                runtime: "claude-code".into(),
                tty: true,
                ..Default::default()
            },
            true,
            Utc::now(),
        );
        agent.id = "fresh".into();
        agent.status = agentdocker_core::AgentStatus::Running;
        agent.process_started_at = Some(Utc::now());
        messages
            .send(Msg::Agents(vec![agent], BTreeMap::new()))
            .unwrap();
        app.drain();
        assert_eq!(app.screen, Screen::Terminal);
        assert_eq!(app.shell.attach_when_listed, None);
    }

    #[tokio::test]
    async fn the_codex_form_applies_a_private_profile_read() {
        let temporary = tempfile::tempdir().unwrap();
        let config = temporary.path().join("config.toml");
        std::fs::write(&config, "model = \"test-only\"\n").unwrap();
        let root = temporary.path().join("project");
        let (mut app, _commands, _messages) = app();
        app.shell.catalog.selected = Some(root.clone());
        app.shell.launch_runtime = Some("codex".into());
        let _ = app.update(Message::ShowLaunch);
        assert!(app.shell.launch);
        let generation = app.shell.launch_codex_trust_generation;
        let trusted = load_codex_trust(root.clone(), Some(config.clone())).await;
        assert_eq!(trusted, Some(false));
        let _ = app.update(Message::CodexTrustLoaded(generation, root.clone(), trusted));
        assert_eq!(app.shell.launch_codex_untrusted, Some(root.clone()));
        std::fs::remove_file(config).unwrap();
        let trusted = load_codex_trust(root.clone(), Some(temporary.path().join("missing"))).await;
        assert_eq!(trusted, None);
        let _ = app.update(Message::CodexTrustLoaded(generation, root, trusted));
        assert_eq!(app.shell.launch_codex_untrusted, None);
    }

    #[test]
    fn stale_codex_trust_reads_cannot_change_a_reopened_or_different_form() {
        let (mut app, _commands, _messages) = app();
        let root = PathBuf::from("/work/new");
        app.shell.catalog.selected = Some(root.clone());
        app.shell.launch_runtime = Some("codex".into());
        let _ = app.update(Message::ShowLaunch);
        let old = app.shell.launch_codex_trust_generation;
        let _ = app.update(Message::ShowLaunch);
        let _ = app.update(Message::CodexTrustLoaded(old, root.clone(), Some(false)));
        assert_eq!(app.shell.launch_codex_untrusted, None);
        let _ = app.update(Message::ShowLaunch);
        let _ = app.update(Message::CodexTrustLoaded(old, root.clone(), Some(false)));
        assert_eq!(app.shell.launch_codex_untrusted, None);
        let current = app.shell.launch_codex_trust_generation;
        let _ = app.update(Message::CodexTrustLoaded(
            current,
            root.clone(),
            Some(false),
        ));
        assert_eq!(app.shell.launch_codex_untrusted, Some(root.clone()));
        app.shell.catalog.selected = Some("/work/other".into());
        let _ = app.update(Message::LaunchRuntime("codex".into()));
        let _ = app.update(Message::CodexTrustLoaded(
            current,
            root.clone(),
            Some(false),
        ));
        assert_eq!(app.shell.launch_codex_untrusted, None);
        let _ = app.update(Message::LaunchRuntime("claude-code".into()));
        let _ = app.update(Message::CodexTrustLoaded(current, root, Some(false)));
        assert_eq!(app.shell.launch_codex_untrusted, None);
    }

    #[test]
    fn a_notification_refreshes_trust_for_the_open_codex_form() {
        for completed_read in [false, true] {
            let (mut app, _commands, _messages, home, mut action) = notification_app();
            let previous = home.path().join("previous");
            let selected = home.path().join("selected");
            std::fs::create_dir(&previous).unwrap();
            std::fs::create_dir(&selected).unwrap();
            let project = crate::catalog::resolve(&selected).unwrap();
            app.shell.catalog.remember(project.clone(), false).unwrap();
            action.target.project = Some(project.id());
            app.questions.push(Question {
                presentation: None,
                id: action.target.message.clone(),
                from: action.target.agent.to_string(),
                to: agentdocker_core::Destination::Agent("user".into()),
                text: "which option?".into(),
                asked_at: Utc::now(),
                expires_at: Utc::now() + chrono::Duration::minutes(5),
            });
            app.shell.catalog.selected = Some(previous.clone());
            app.shell.checked_project = Some(previous.clone());
            app.shell.launch_runtime = Some("codex".into());
            let _ = app.update(Message::ShowLaunch);
            let old = app.shell.launch_codex_trust_generation;
            if completed_read {
                let _ = app.update(Message::CodexTrustLoaded(
                    old,
                    previous.clone(),
                    Some(false),
                ));
                assert_eq!(app.shell.launch_codex_untrusted, Some(previous.clone()));
            }
            let _ = app.update(Message::Notification(
                crate::notification_route::Activation::Open(action),
            ));
            assert!(app.shell.launch);
            assert_eq!(app.shell.catalog.selected, Some(project.root.clone()));
            let current = app.shell.launch_codex_trust_generation;
            assert_ne!(
                old, current,
                "a notification must request the new project's trust"
            );
            assert_eq!(app.shell.launch_codex_untrusted, None);
            let _ = app.update(Message::CodexTrustLoaded(old, previous, Some(false)));
            assert_eq!(app.shell.launch_codex_untrusted, None);
            let _ = app.update(Message::CodexTrustLoaded(
                current,
                project.root.clone(),
                Some(false),
            ));
            assert_eq!(app.shell.launch_codex_untrusted, Some(project.root));
        }
    }

    #[test]
    fn the_codex_trust_note_is_for_codex_alone() {
        let (mut app, _commands, _messages) = app();
        app.shell.launch_codex_untrusted = Some("/work/new".into());
        // Another tool reads nothing and shows no Codex note.
        let _ = app.update(Message::LaunchRuntime("claude-code".into()));
        assert_eq!(app.shell.launch_codex_untrusted, None);
    }

    #[test]
    fn mcp_url_questions_never_submit_or_save_a_typed_draft() {
        let (mut app, commands, _) = app();
        app.connected = Ok(());
        let presentation = agentdocker_core::QuestionPresentation::McpUrl {
            server: "example-tools".into(),
            elicitation_id: "one".into(),
            message: "Connect your example account.".into(),
            url: "https://example.com/consent".into(),
        };
        let id = MessageId::from("mcp-url".to_owned());
        app.questions.push(Question {
            id: id.clone(),
            from: "asker".into(),
            to: agentdocker_core::Destination::Agent("human".into()),
            text: presentation.text(),
            presentation: Some(presentation),
            asked_at: Utc::now(),
            expires_at: Utc::now() + chrono::Duration::minutes(5),
        });
        let _ = app.update(Message::Draft(id.clone(), "private input".into()));
        assert!(!app.shell.answers.contains_key(&id));
        app.shell.answers.insert(id.clone(), "earlier draft".into());
        let _ = app.update(Message::Answer(id.clone()));
        let _ = app.update(Message::AnswerChoice(id.clone(), "private input".into()));
        let _ = app.update(Message::CopyQuestionUrl(id.clone()));
        assert_eq!(commands.try_iter().count(), 0, "copying is not a decision");
        let _ = app.update(Message::AnswerChoice(id.clone(), "Accept".into()));
        let _ = app.update(Message::AnswerChoice(id.clone(), "Decline".into()));
        assert!(
            matches!(commands.try_iter().collect::<Vec<_>>().as_slice(), [Cmd::Answer(q, value)] if q == &id && value == "Accept")
        );
        assert_eq!(app.shell.answers[&id], "earlier draft");
        app.sending.clear();
        app.questions[0].expires_at = Utc::now() - chrono::Duration::seconds(1);
        let _ = app.update(Message::CopyQuestionUrl(id.clone()));
        let _ = app.update(Message::AnswerChoice(id, "Accept".into()));
        assert_eq!(commands.try_iter().count(), 0);
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
        for (index, chunk) in vec![b'z'; remaining].chunks(4_000).enumerate() {
            assert!(app.shell.edit_draft(
                DraftKind::TaskAcceptance,
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
                for_session: false,
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
            app.shell.catalog.remember(project.clone(), true).unwrap();
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
            assert!(state.edit_draft(DraftKind::Answer, index.to_string(), "x".repeat(count)));
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
            &snapshot.answers,
        ]
        .into_iter()
        .flat_map(|drafts| drafts.values())
        .map(String::len)
        .sum::<usize>();
        let left = crate::drafts::MAX_TOTAL_BYTES - used;
        // Fill the remaining shared space without changing any answer.
        for (index, chunk) in "z".repeat(left).as_bytes().chunks(4_000).enumerate() {
            assert!(state.edit_draft(
                DraftKind::TaskAcceptance,
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
        state
            .session_drafts
            .get_mut("agent-a")
            .unwrap()
            .draft
            .begin();
        state.session_drafts.get_mut("agent-a").unwrap().queued =
            Some(MessageId::from("old-receipt".to_owned()));
        state.conversation_drafts.get_mut("dm:a:b").unwrap().begin();
        state
            .conversation_drafts
            .get_mut("dm:a:b/thread")
            .unwrap()
            .error = Some("old failure".into());
        let saved = state.draft_snapshot();
        saved.save(&state.draft_home).unwrap();
        let reopened = State::load(home.path());
        assert_eq!(reopened.draft_snapshot(), saved);
        assert!(reopened.session_drafts["agent-a"].draft.sending.is_none());
        assert!(reopened.session_drafts["agent-a"].queued.is_none());
        assert!(reopened.conversation_drafts["dm:a:b"].sending.is_none());
        assert!(
            reopened.conversation_drafts["dm:a:b/thread"]
                .error
                .is_none()
        );
        assert!(reopened.drafts.clean());
        assert!(reopened.drafts.error.is_none());
    }

    /// A drafts file from when the app had channels still loads: what was
    /// typed for a channel, `#all` or a contested room is let go — nothing
    /// here could send it — and everything else comes back, saved without
    /// them from then on.
    #[test]
    fn an_old_drafts_file_loads_and_lets_go_of_channel_drafts() {
        let home = tempfile::tempdir().unwrap();
        let draft_home = State::load(home.path()).draft_home;
        let old = crate::drafts::Snapshot {
            sessions: BTreeMap::from([("agent-a".into(), "session words".into())]),
            conversations: BTreeMap::from([
                ("dm:a:b".into(), "direct words".into()),
                ("dm:a:b#root".into(), "thread words".into()),
                ("everyone:project".into(), "chat words".into()),
                ("all".into(), "to everyone".into()),
                ("channel:room".into(), "room words".into()),
                ("channel:room#root".into(), "room thread words".into()),
            ]),
            channels: BTreeMap::from([("room".into(), "channels screen words".into())]),
            answers: BTreeMap::from([("question".into(), "an answer".into())]),
            ..Default::default()
        };
        old.save(&draft_home).unwrap();
        let state = State::load(home.path());
        assert!(state.drafts.error.is_none(), "{:?}", state.drafts.error);
        let kept: Vec<&str> = state
            .conversation_drafts
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(kept, ["dm:a:b", "dm:a:b#root", "everyone:project"]);
        assert_eq!(state.session_drafts["agent-a"].draft.text, "session words");
        assert_eq!(
            state.answers[&MessageId::from("question".to_owned())],
            "an answer"
        );
        let saved = state.draft_snapshot();
        assert!(saved.channels.is_empty());
        assert!(
            !saved
                .conversations
                .keys()
                .any(|k| k.starts_with("channel:") || k == "all")
        );
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
        for kind in [DraftKind::Session, DraftKind::Conversation] {
            app.shell.edit_draft(kind, "original".into(), "sent".into());
            let target = match kind {
                DraftKind::Session => DeliveryTarget::Session("original".into()),
                DraftKind::Conversation => DeliveryTarget::Conversation("original".into()),
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
                    DraftKind::Answer | DraftKind::TaskTitle | DraftKind::TaskAcceptance => {
                        unreachable!("message drafts only")
                    }
                })
                .unwrap();
            app.drain();
            let draft = match kind {
                DraftKind::Session => &app.shell.session_drafts["original"].draft,
                DraftKind::Conversation => &app.shell.conversation_drafts["original"],
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
    }

    #[test]
    fn retyped_conversation_and_session_drafts_survive_late_receipts() {
        fn draft(app: &mut App, session: bool) -> &mut TextDraft {
            if session {
                &mut app.shell.session_drafts.get_mut("recipient").unwrap().draft
            } else {
                app.shell.conversation_drafts.get_mut("recipient").unwrap()
            }
        }
        fn edit(session: bool, text: &str) -> Message {
            if session {
                Message::SessionDraft("recipient".into(), text.into())
            } else {
                Message::ConversationDraft("recipient".into(), text.into())
            }
        }
        for session in [false, true] {
            let (mut app, _, messages) = app();
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
                    Msg::ConversationSent("recipient".into(), receipt)
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
        // Selecting a session reads its Changes; that read is not part of
        // what this test counts.
        assert!(
            commands
                .try_iter()
                .all(|cmd| matches!(cmd, Cmd::ReviewBranch(ref id) if id == "other"))
        );
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
        app.shell.catalog.remember(project.clone(), false).unwrap();
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
        app.shell.conversation_drafts.insert(
            "everyone:another-project".into(),
            TextDraft {
                text: "unfinished chat message".into(),
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
            app.shell.conversation_drafts["everyone:another-project"].text,
            "unfinished chat message"
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
                | Cmd::ConversationSend { .. }
                | Cmd::Launch(..)
                | Cmd::Resume(..)
                | Cmd::Stop(..)
                | Cmd::HistoryBefore(..)
        )));
    }

    /// An archived message's notification opens its conversation even when
    /// its sender's record is gone: the archive outlives it, and no
    /// ten-second wait ends in "no longer available".
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
            TextDraft {
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
        for kind in [DraftKind::Session, DraftKind::Conversation] {
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
                .map(String::len)
                .sum()
        };
        for (index, chunk) in "x"
            .repeat(crate::drafts::MAX_TOTAL_BYTES - used)
            .as_bytes()
            .chunks(4_000)
            .enumerate()
        {
            assert!(app.shell.edit_draft(
                DraftKind::TaskAcceptance,
                format!("/filler-{index}"),
                String::from_utf8(chunk.to_vec()).unwrap(),
            ));
        }
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
    fn an_archived_notification_opens_its_conversation_without_a_live_sender() {
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
    }

    /// A notification about a message in a conversation the app leaves to
    /// the CLI — a channel, a contested room, `#all` — never opens a screen
    /// that is gone: from an agent it opens the direct conversation with
    /// that agent, from AgentDocker itself just Messages, and either way
    /// the status line says how to read the message. A reply to it that
    /// did not go waits in the direct conversation's composer, or, with no
    /// conversation of its own, at the top of Messages.
    #[test]
    fn a_notification_about_a_hidden_conversation_opens_what_the_app_shows() {
        // A channel message, read already, from an agent.
        let (mut app, commands, messages, _home, mut action) = notification_app();
        action.target.channel = Some(agentdocker_core::ChannelId::from("planning".to_owned()));
        messages.send(Msg::Conversations(Ok(Vec::new()))).unwrap();
        messages.send(Msg::Inbox(Vec::new())).unwrap();
        messages.send(Msg::Questions(Vec::new())).unwrap();
        let _ = app.update(Message::Notification(
            crate::notification_route::Activation::Open(action),
        ));
        let _ = app.update(Message::Tick);
        assert!(app.shell.pending_notification.is_none(), "routed at once");
        assert_eq!(app.screen, Screen::Questions);
        let direct = agentdocker_core::ConversationId::dm("user", "sender-1").to_string();
        assert_eq!(app.shell.conversation.as_deref(), Some(direct.as_str()));
        assert!(
            app.status
                .contains("`agentdocker history channel:planning`"),
            "{}",
            app.status
        );
        assert!(app.reveal_archived.is_none(), "nothing to look for there");
        assert!(!commands.try_iter().any(|cmd| matches!(
            cmd,
            Cmd::History(ref c, _) | Cmd::HistoryBefore(ref c, ..) if c.starts_with("channel:")
        )));

        // A message to every agent, still in the inbox.
        let (mut app, _commands, messages, _home, action) = notification_app();
        let mut envelope = agentdocker_core::Envelope::new(
            "sender-1",
            agentdocker_core::Destination::Broadcast,
            "chat",
            serde_json::json!({"text": "to everyone"}),
            None,
            Utc::now(),
        );
        envelope.id = action.target.message.clone();
        messages.send(Msg::Conversations(Ok(Vec::new()))).unwrap();
        messages.send(Msg::Inbox(vec![envelope])).unwrap();
        messages.send(Msg::Questions(Vec::new())).unwrap();
        let _ = app.update(Message::Notification(
            crate::notification_route::Activation::Open(action),
        ));
        let _ = app.update(Message::Tick);
        assert_eq!(app.screen, Screen::Questions);
        assert_eq!(app.shell.conversation.as_deref(), Some(direct.as_str()));
        assert!(
            app.status.contains("`agentdocker history all`"),
            "{}",
            app.status
        );

        // AgentDocker's own notice in a contested room: Messages, and a
        // reply that did not go stays at the top of it to copy, never in
        // whatever conversation was open.
        let (mut app, _commands, messages, _home, mut action) = notification_app();
        action.target.agent =
            agentdocker_core::AgentId::from(agentdocker_core::conversation::DAEMON);
        action.target.channel = Some(agentdocker_core::ChannelId::from("contested".to_owned()));
        app.shell.conversation = Some("everyone:project".into());
        messages.send(Msg::Conversations(Ok(Vec::new()))).unwrap();
        messages.send(Msg::Inbox(Vec::new())).unwrap();
        messages.send(Msg::Questions(Vec::new())).unwrap();
        let _ = app.update(Message::Notification(
            crate::notification_route::Activation::ReplyFailed {
                action,
                text: "my words".into(),
                reason: "the app does not reply in channels".into(),
                certain: true,
            },
        ));
        let _ = app.update(Message::Tick);
        assert_eq!(app.screen, Screen::Questions);
        assert_eq!(app.shell.conversation.as_deref(), Some("everyone:project"));
        assert!(
            !app.shell
                .conversation_drafts
                .contains_key("everyone:project"),
            "the words are not put in an unrelated conversation"
        );
        let orphans = app.shell.orphan_reply_recoveries();
        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans[0].text, "my words");
        assert!(
            app.status
                .contains("`agentdocker history channel:contested`")
        );
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

    /// The Needs-you strip's **Review** does something visible: it opens
    /// the session with its delivery review unfolded and asks for its log,
    /// even when that session was already the selected one (selecting
    /// alone folds the review, which is what made the button look dead).
    #[test]
    fn needs_you_review_opens_the_sessions_delivery_review() {
        let (mut app, commands, _) = app();
        app.connected = Ok(());
        let now = Utc::now();
        let mut paused = AgentRecord::new(
            agentdocker_core::AgentSpec {
                name: "claude-code-4245".into(),
                runtime: "claude-code".into(),
                workdir: Some("/work/repo".into()),
                ..Default::default()
            },
            false,
            now,
        );
        paused.id = "paused-claude".into();
        paused.status = agentdocker_core::AgentStatus::Running;
        paused.process_started_at = Some(now);
        paused.input_delivery = Some(agentdocker_core::InputDelivery {
            process_started_at: now,
            paused: true,
            pause_reason: Some("Retained input requires review".into()),
            reported_at: now,
            received: None,
            received_at: None,
        });
        app.agents.push(paused);
        // Already selected, review folded: exactly the screenshot's state.
        app.shell.selected = Some("paused-claude".into());
        app.shell.review_delivery = false;
        let _ = app.update(Message::ReviewSession("paused-claude".into()));
        assert_eq!(app.shell.selected.as_deref(), Some("paused-claude"));
        assert!(app.shell.review_delivery, "the review is unfolded");
        assert_eq!(app.screen, Screen::Agents);
        assert!(
            commands
                .try_iter()
                .any(|cmd| matches!(cmd, Cmd::SessionLog(ref id) if id == "paused-claude")),
            "the session log is asked for"
        );
        // Pressed again it toggles like the inspector's own control would
        // not — it stays a way in, never a way out.
        let _ = app.update(Message::ReviewSession("paused-claude".into()));
        assert!(app.shell.review_delivery);
    }

    /// The header's Launch agent… opens the form and never closes it; a
    /// Connect is sent once until the daemon answers; a copy says so.
    #[test]
    fn launch_opens_only_connect_sends_once_and_a_copy_is_confirmed() {
        let (mut app, commands, _) = app();
        app.shell
            .catalog
            .remember(ProjectRef::directory("/work/repo"), true)
            .unwrap();
        app.shell.catalog.selected = Some("/work/repo".into());
        let _ = app.update(Message::OpenLaunch);
        assert!(app.shell.launch);
        let _ = app.update(Message::OpenLaunch);
        assert!(app.shell.launch, "pressing it again leaves the form open");

        app.discovered.push(DiscoveredProcess {
            pid: 4242,
            ppid: 1,
            runtime: "codex".into(),
            command: "codex".into(),
            cwd: None,
            project: None,
            started_at: None,
            session: None,
        });
        let _ = commands.try_iter().count();
        let _ = app.update(Message::Adopt(4242));
        let _ = app.update(Message::Adopt(4242));
        let adopts = commands
            .try_iter()
            .filter(|cmd| matches!(cmd, Cmd::Adopt(4242)))
            .count();
        assert_eq!(adopts, 1, "a second press waits for the first answer");
        assert!(app.shell.adopting.contains(&4242));

        let _ = app.update(Message::CopyGuidance("resume".into()));
        assert!(app.status.contains("Copied"), "{}", app.status);
    }

    /// The ended sessions' Earlier group opens on a page of entries and
    /// grows a page per *Show older*; closing the group
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
        app.shell
            .catalog
            .remember(
                agentdocker_core::ProjectRef::directory("/private/tmp/fixture/workspace"),
                false,
            )
            .unwrap();
        let _ = app.update(Message::ToggleTemporary);
        assert_eq!(app.shell.temporary_open, Some(false));
        assert!(
            !app.temporary_fold_open(),
            "closable while a scratch session runs"
        );
        app.shell.temporary_open = None;
        assert!(app.temporary_fold_open(), "automatic: open while one runs");
        // A selected scratch project with nothing running opens the fold
        // by itself, and one click closes it: the click negates the fold
        // as drawn, selection included.
        app.agents.clear();
        app.shell.catalog.selected = Some("/private/tmp/fixture/workspace".into());
        app.screen = Screen::Agents;
        assert!(
            app.temporary_fold_open(),
            "automatic: open for the project on view"
        );
        let _ = app.update(Message::ToggleTemporary);
        assert_eq!(app.shell.temporary_open, Some(false));
        assert!(!app.temporary_fold_open(), "one click closes it");
        let _ = app.update(Message::ToggleTemporary);
        assert!(app.temporary_fold_open());
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
                    "5e55a0d1-7c3b-4f2e-9a61-0b4d2c8e7f13".to_owned(),
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
                .any(|w| w[0] == "--resume" && w[1] == "5e55a0d1-7c3b-4f2e-9a61-0b4d2c8e7f13"),
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
    fn reconnect_keeps_the_recorded_executable_permissions_and_profile() {
        let (mut app, commands, _) = app();
        let cli = tempfile::NamedTempFile::new().unwrap();
        let mut spec = agentdocker_core::AgentSpec {
            name: "restricted-session".into(),
            runtime: "claude-code".into(),
            command: vec![
                "/fixture/chosen-claude".into(),
                "--permission-mode".into(),
                "dontAsk".into(),
                "--tools".into(),
                "".into(),
                "--strict-mcp-config".into(),
                "--".into(),
                "do not replay this prompt".into(),
            ],
            workdir: Some("/fixture/project".into()),
            env: BTreeMap::from([("CLAUDE_CONFIG_DIR".into(), "/fixture/profile".into())]),
            labels: BTreeMap::from([("session_id".into(), "original-conversation".into())]),
            tty: true,
            ..Default::default()
        };
        agentdocker_host::provider_input::enable_claude_channel(&mut spec, cli.path()).unwrap();
        let mut agent = AgentRecord::new(spec.clone(), true, Utc::now());
        agent.id = "restricted-session".into();
        agent.status = agentdocker_core::AgentStatus::Exited { code: Some(0) };
        app.agents.push(agent);
        // A recorded executable remains usable without choosing a different
        // provider from the current PATH inventory.
        app.runtimes.clear();
        let resumed = app
            .reconnect_spec("restricted-session", Ok(cli.path().into()))
            .unwrap();
        assert_eq!(resumed.command[0], "/fixture/chosen-claude");
        assert_eq!(resumed.env, spec.env);
        assert_eq!(
            &resumed.command[5..],
            [
                "--permission-mode",
                "dontAsk",
                "--tools",
                "",
                "--strict-mcp-config",
                "--resume",
                "original-conversation"
            ]
        );
        assert_eq!(commands.try_iter().count(), 0);

        app.agents[0].spec.command.push("--unknown-option".into());
        assert!(
            app.reconnect_spec("restricted-session", Ok(cli.path().into()))
                .is_err()
        );
        assert_eq!(commands.try_iter().count(), 0);
        assert_eq!(
            app.agents[0].status,
            agentdocker_core::AgentStatus::Exited { code: Some(0) }
        );
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
            assert_eq!(
                app.display_name(&generated),
                format!(
                    "{} · {}",
                    runtime_label(runtime),
                    super::naming::word_for(generated.id.as_str())
                )
            );
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
                TextDraft {
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
            app.shell.catalog.remember(project, false).unwrap();
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

    /// Opening a session is asking to go to its project. One in a folder
    /// the person removed brings that project back to the list, unpinned;
    /// one in `/`, the home folder or a relative folder is one of the
    /// Other sessions, with no word; and only a list at its limit says it
    /// is full, naming the limit.
    #[test]
    fn opening_a_session_lists_its_project_and_only_a_full_list_refuses() {
        use agentdocker_core::AgentSpec;
        let (mut app, _, _) = app();
        let session = |name: &str, root: Option<PathBuf>| {
            let mut agent = AgentRecord::new(
                AgentSpec {
                    name: name.into(),
                    ..Default::default()
                },
                false,
                Utc::now(),
            );
            agent.id = name.into();
            agent.status = agentdocker_core::AgentStatus::Running;
            agent.project = root.map(ProjectRef::directory);
            agent
        };
        let removed = PathBuf::from("/fixture/removed");
        app.shell
            .catalog
            .remember(ProjectRef::directory(&removed), false)
            .unwrap();
        app.shell
            .catalog
            .remember(ProjectRef::directory("/fixture/kept"), true)
            .unwrap();
        assert!(app.shell.catalog.remove(&removed).unwrap());
        let home = std::env::home_dir().unwrap();
        app.agents = vec![
            session("in-removed", Some(removed.clone())),
            session("at-home", Some(home)),
            session("at-root", Some("/".into())),
            session("relative", Some("relative/folder".into())),
            session("newcomer", Some("/fixture/newcomer".into())),
        ];
        let on_view = |app: &App| -> Vec<String> {
            app.session_records(sessions::Filter::Current)
                .iter()
                .map(|a| a.id.to_string())
                .collect()
        };

        // The removed project is listed again, unpinned, with its session
        // open in it, and nothing says the list is full.
        let _ = app.update(Message::OpenSession("in-removed".into()));
        assert!(app.shell.catalog.hidden.is_empty(), "no longer removed");
        let entry = app
            .shell
            .catalog
            .projects
            .iter()
            .find(|e| e.project.root == removed)
            .expect("listed again");
        assert!(!entry.pinned);
        assert_eq!(app.shell.catalog.selected.as_ref(), Some(&removed));
        assert_eq!(app.shell.selected.as_deref(), Some("in-removed"));
        assert_eq!(on_view(&app), ["in-removed"]);
        assert!(!app.status.contains("full"), "{}", app.status);

        // `/`, the home folder and a relative folder are no project: each
        // session opens under Other sessions, where it is on view.
        for id in ["at-home", "at-root", "relative"] {
            let _ = app.update(Message::OpenSession(id.into()));
            assert!(app.shell.catalog.unassigned, "{id}");
            assert_eq!(app.shell.selected.as_deref(), Some(id));
            assert!(on_view(&app).iter().any(|a| a == id), "{id} is on view");
            assert!(!app.status.contains("full"), "{id}: {}", app.status);
        }
        assert_eq!(app.shell.catalog.projects.len(), 2, "nothing more listed");

        // At the limit, a listed project's session still opens; a new one
        // is refused with the limit and the advice, and nothing moves.
        let mut filler = 0;
        while app.shell.catalog.projects.len() < crate::catalog::MAX_PROJECTS {
            app.shell
                .catalog
                .remember(
                    ProjectRef::directory(format!("/fixture/filler-{filler}")),
                    false,
                )
                .unwrap();
            filler += 1;
        }
        let _ = app.update(Message::OpenSession("in-removed".into()));
        assert_eq!(app.shell.selected.as_deref(), Some("in-removed"));
        assert!(!app.status.contains("full"), "{}", app.status);
        let _ = app.update(Message::OpenSession("newcomer".into()));
        assert_eq!(
            app.status,
            "The project list is full (512 projects). Forget an old project before opening this session."
        );
        assert_eq!(
            app.shell.selected.as_deref(),
            Some("in-removed"),
            "nothing moved"
        );
        assert_eq!(app.shell.catalog.selected.as_ref(), Some(&removed));
        assert!(
            !app.shell
                .catalog
                .projects
                .iter()
                .any(|e| e.project.root == std::path::Path::new("/fixture/newcomer"))
        );
    }

    #[test]
    fn a_new_notification_cancels_the_previous_reveal_before_its_data_arrives() {
        let (mut app, commands, messages, _home, mut action) = notification_app();
        app.conversations_supported = Some(true);
        app.screen = Screen::Questions;
        let conversation = "dm:previous:user".to_owned();
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
            Cmd::Journal(_, _) | Cmd::Conversations(_) | Cmd::History(_, _)
        )));
        assert_eq!(std::fs::read_dir(folder.path()).unwrap().count(), 0);
    }
    #[test]
    fn late_conversation_acknowledgements_preserve_new_text_and_other_conversations() {
        let mut first = TextDraft {
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
        app.shell
            .conversation_drafts
            .insert("dm:one:user".into(), first);
        app.shell.conversation_drafts.insert(
            "dm:two:user".into(),
            TextDraft {
                text: "other conversation".into(),
                ..Default::default()
            },
        );
        app.shell
            .conversation_drafts
            .get_mut("dm:one:user")
            .unwrap()
            .begin();
        messages
            .send(Msg::ConversationSent(
                "dm:one:user".into(),
                Ok(MessageId::from("confirmed".to_owned()).into()),
            ))
            .unwrap();
        app.drain();
        assert!(app.shell.conversation_drafts["dm:one:user"].text.is_empty());
        assert_eq!(
            app.shell.conversation_drafts["dm:two:user"].text,
            "other conversation"
        );
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
    fn stale_folder_checks_are_ignored() {
        let folder = tempfile::tempdir().unwrap();
        let (mut app, _, _) = app();
        let project = crate::catalog::resolve(folder.path()).unwrap();
        app.shell.catalog.pin(project).unwrap();
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

    // Back and Forward (navigation.rs).

    /// What a travelling window's test keeps alive: the worker's end of
    /// its queue, the way replies reach it, and the projects' folder.
    struct Kept {
        _commands: CommandReceiver,
        messages: SyncSender<Msg>,
        _dir: tempfile::TempDir,
    }

    /// A window with two pinned projects, `alpha` and `beta`, on All
    /// projects.
    fn travel() -> (App, PathBuf, PathBuf, Kept) {
        let (mut app, _commands, messages) = app();
        let dir = tempfile::tempdir().unwrap();
        let alpha = ProjectRef::directory(dir.path().join("alpha"));
        let beta = ProjectRef::directory(dir.path().join("beta"));
        app.shell.catalog.remember(alpha.clone(), true).unwrap();
        app.shell.catalog.remember(beta.clone(), true).unwrap();
        let kept = Kept {
            _commands,
            messages,
            _dir: dir,
        };
        (app, alpha.root, beta.root, kept)
    }

    /// A session of `runtime` in the project at `root`, running.
    fn session_in(id: &str, root: &std::path::Path) -> AgentRecord {
        let mut agent = AgentRecord::new(
            agentdocker_core::AgentSpec {
                name: id.into(),
                runtime: "claude-code".into(),
                ..Default::default()
            },
            true,
            Utc::now(),
        );
        agent.id = id.into();
        agent.status = agentdocker_core::AgentStatus::Running;
        agent.project = Some(ProjectRef::directory(root));
        agent
    }

    fn card_in(id: &str, root: &std::path::Path, title: &str) -> agentdocker_core::Task {
        let now = Utc::now();
        serde_json::from_value(serde_json::json!({
            "id": id, "project": ProjectRef::directory(root).id(), "title": title,
            "acceptance": "", "column": "ready", "assignee": null,
            "created_by": "user", "created_at": now, "updated_at": now,
            "archived_at": null, "links": []
        }))
        .unwrap()
    }

    /// The screen and the project on view.
    fn on(app: &App) -> (Screen, Option<PathBuf>) {
        (app.screen, app.shell.catalog.selected.clone())
    }

    /// Back and Forward walk the places the person went, in order, each
    /// restored whole; going there records nothing, and at either end
    /// there is nowhere further to go.
    #[test]
    fn back_and_forward_walk_the_places_the_person_went() {
        let (mut app, alpha, beta, _kept) = travel();
        assert!(app.destination(false).is_none() && app.destination(true).is_none());
        let _ = app.update(Message::SelectProject(alpha.clone()));
        let _ = app.update(Message::Navigate(Screen::Board));
        let _ = app.update(Message::SelectProject(beta.clone()));
        assert_eq!(on(&app), (Screen::Chat, Some(beta.clone())));
        assert_eq!(app.shell.history.lengths(), (3, 0));

        let _ = app.update(Message::GoBack);
        assert_eq!(on(&app), (Screen::Board, Some(alpha.clone())));
        let _ = app.update(Message::GoBack);
        assert_eq!(on(&app), (Screen::Chat, Some(alpha.clone())));
        assert_eq!(
            app.shell.history.lengths(),
            (1, 2),
            "going back moves places from behind to ahead and adds none"
        );
        let _ = app.update(Message::GoBack);
        assert_eq!(on(&app), (Screen::Agents, None), "All projects, the start");
        assert!(app.destination(false).is_none());
        let _ = app.update(Message::GoBack);
        assert_eq!(on(&app), (Screen::Agents, None), "nothing behind the start");

        let _ = app.update(Message::GoForward);
        assert_eq!(on(&app), (Screen::Chat, Some(alpha.clone())));
        let _ = app.update(Message::GoForward);
        assert_eq!(on(&app), (Screen::Board, Some(alpha.clone())));
        let _ = app.update(Message::GoForward);
        assert_eq!(on(&app), (Screen::Chat, Some(beta.clone())));
        assert!(app.destination(true).is_none());
        let _ = app.update(Message::GoForward);
        assert_eq!(
            on(&app),
            (Screen::Chat, Some(beta)),
            "nothing ahead of the end"
        );
        assert_eq!(app.shell.history.lengths(), (3, 0));
    }

    /// A new place after going back drops the places that were ahead, as a
    /// browser does; the places behind stay.
    #[test]
    fn a_new_place_after_going_back_drops_what_was_ahead() {
        let (mut app, alpha, _, _kept) = travel();
        let _ = app.update(Message::SelectProject(alpha.clone()));
        let _ = app.update(Message::Navigate(Screen::Board));
        let _ = app.update(Message::Navigate(Screen::Usage));
        let _ = app.update(Message::GoBack);
        let _ = app.update(Message::GoBack);
        assert_eq!(on(&app), (Screen::Chat, Some(alpha.clone())));
        assert_eq!(app.shell.history.lengths().1, 2);

        let _ = app.update(Message::Navigate(Screen::Journal));
        assert_eq!(app.shell.history.lengths(), (2, 0), "nothing ahead now");
        assert!(app.destination(true).is_none());
        let _ = app.update(Message::GoForward);
        assert_eq!(on(&app), (Screen::Journal, Some(alpha.clone())));
        let _ = app.update(Message::GoBack);
        assert_eq!(on(&app), (Screen::Chat, Some(alpha)));
    }

    /// At most a hundred places are kept behind: the oldest go first.
    #[test]
    fn history_keeps_the_newest_hundred_places() {
        let (mut app, alpha, _, _kept) = travel();
        let _ = app.update(Message::SelectProject(alpha));
        for i in 0..150 {
            let screen = if i % 2 == 0 {
                Screen::Board
            } else {
                Screen::Journal
            };
            let _ = app.update(Message::Navigate(screen));
        }
        assert_eq!(
            app.shell.history.lengths(),
            (super::super::navigation::PLACES, 0)
        );
        let mut steps = 0;
        while app.destination(false).is_some() {
            let _ = app.update(Message::GoBack);
            steps += 1;
            assert!(steps <= 100, "Back ends");
        }
        assert_eq!(steps, 100);
        assert_eq!(
            app.screen,
            Screen::Journal,
            "the hundredth place back, not the first project chat"
        );
    }

    /// Hover, menus, drafts, searches, filters, layout and the window's
    /// own ticks are not places, and nor is going where the person already
    /// is.
    #[test]
    fn transient_state_and_the_same_place_are_not_recorded() {
        let (mut app, alpha, _, _kept) = travel();
        let _ = app.update(Message::SelectProject(alpha.clone()));
        let _ = app.update(Message::Navigate(Screen::Board));
        let recorded = app.shell.history.lengths();
        for message in [
            Message::RailHover(alpha.clone()),
            Message::RailLeave(alpha.clone()),
            Message::More,
            Message::More,
            Message::ProjectMenu(alpha.clone()),
            Message::ProjectMenu(alpha.clone()),
            Message::TagsMenu,
            Message::TagsMenu,
            Message::BoardQuery("parser".into()),
            Message::BoardLayout,
            Message::BoardDense,
            Message::TaskTitle("Port the parser".into()),
            Message::MessagesSearch("alpha".into()),
            Message::LaunchMenu,
            Message::LaunchMenu,
            Message::Tick,
            Message::Navigate(Screen::Board),
        ] {
            let _ = app.update(message);
        }
        assert_eq!(app.shell.history.lengths(), recorded);
        assert_eq!(app.destination(false).map(|p| p.screen), Some(Screen::Chat));
    }

    /// What is open on a screen comes back with it: the session on Agents,
    /// the card on Board, the thread beside the chat.
    #[test]
    fn back_restores_the_session_card_and_thread_that_were_open() {
        let (mut app, alpha, beta, _kept) = travel();
        app.agents = vec![session_in("worker", &alpha)];
        let card = card_in("aaaaaaaaaaaa", &beta, "Port the parser");
        let thread = MessageId::from("thread-root".to_owned());
        let _ = app.update(Message::OpenSession("worker".into()));
        assert_eq!(on(&app), (Screen::Agents, Some(alpha.clone())));
        let _ = app.update(Message::SelectProject(beta.clone()));
        let _ = app.update(Message::OpenThread(thread.clone()));
        let _ = app.update(Message::Navigate(Screen::Board));
        let _ = app.update(Message::TaskOpen(card.id.clone()));
        let _ = app.update(Message::Navigate(Screen::Journal));

        let _ = app.update(Message::GoBack);
        assert_eq!(on(&app), (Screen::Board, Some(beta.clone())));
        assert_eq!(app.task_open.as_ref(), Some(&card.id));
        let _ = app.update(Message::GoBack);
        assert_eq!(app.task_open, None, "the board before the card opened");
        let _ = app.update(Message::GoBack);
        assert_eq!(on(&app), (Screen::Chat, Some(beta.clone())));
        assert_eq!(app.shell.thread.as_ref(), Some(&thread));
        let _ = app.update(Message::GoBack);
        assert_eq!(app.shell.thread, None);
        let _ = app.update(Message::GoBack);
        assert_eq!(on(&app), (Screen::Agents, Some(alpha.clone())));
        assert_eq!(app.shell.selected.as_deref(), Some("worker"));

        // Forward again opens the card on the other project's board.
        for _ in 0..4 {
            let _ = app.update(Message::GoForward);
        }
        assert_eq!(on(&app), (Screen::Board, Some(beta)));
        assert_eq!(app.task_open.as_ref(), Some(&card.id));
    }

    /// A place whose project was removed, whose session is no longer
    /// listed or whose card was archived is passed over, and the arrows
    /// say so before they are pressed.
    #[test]
    fn back_passes_over_places_that_are_gone() {
        let (mut app, alpha, beta, _kept) = travel();
        app.agents = vec![session_in("gone", &beta), session_in("kept", &beta)];
        let card = card_in("aaaaaaaaaaaa", &beta, "Port the parser");
        let _ = app.update(Message::SelectProject(alpha.clone()));
        let _ = app.update(Message::OpenSession("kept".into()));
        let _ = app.update(Message::OpenSession("gone".into()));
        let _ = app.update(Message::Navigate(Screen::Board));
        let _ = app.update(Message::TaskOpen(card.id.clone()));
        let _ = app.update(Message::Navigate(Screen::Journal));

        // The card is archived, the session leaves the list, and the first
        // project is removed.
        app.on_event(agentdocker_core::Event::new(
            agentdocker_core::EventKind::TaskArchived {
                task: card.id.clone(),
                project: ProjectRef::directory(&beta).id(),
                by: agentdocker_core::HUMAN.into(),
            },
            Utc::now(),
        ));
        app.agents.retain(|a| a.id.as_str() != "gone");
        let _ = app.update(Message::ProjectRemove(alpha.clone()));
        assert_eq!(on(&app), (Screen::Journal, Some(beta.clone())));

        assert_eq!(
            app.destination(false).map(|p| (p.screen, p.card.clone())),
            Some((Screen::Board, None)),
            "the archived card's place is not offered"
        );
        let _ = app.update(Message::GoBack);
        assert_eq!(on(&app), (Screen::Board, Some(beta.clone())));
        assert_eq!(app.task_open, None);
        let _ = app.update(Message::GoBack);
        assert_eq!(on(&app), (Screen::Agents, Some(beta.clone())));
        assert_eq!(
            app.shell.selected.as_deref(),
            Some("kept"),
            "the gone session's place is passed over"
        );
        let _ = app.update(Message::GoBack);
        assert_eq!(
            on(&app),
            (Screen::Agents, None),
            "the removed project's chat is passed over too"
        );
        assert!(app.destination(false).is_none());
    }

    /// Replies that settle the place on view (a thread pruned under the
    /// person) record nothing and keep what is ahead; a launched
    /// session's terminal opening finishes the person's launch and is a
    /// move Back undoes.
    #[test]
    fn the_windows_own_changes_refine_the_place_and_a_launch_moves() {
        let (mut app, alpha, _, kept) = travel();
        let messages = &kept.messages;
        let thread = MessageId::from("thread-root".to_owned());
        let _ = app.update(Message::SelectProject(alpha.clone()));
        let _ = app.update(Message::OpenThread(thread.clone()));
        let _ = app.update(Message::Navigate(Screen::Board));
        let _ = app.update(Message::GoBack);
        assert_eq!(app.shell.thread.as_ref(), Some(&thread));
        let recorded = app.shell.history.lengths();
        messages
            .send(Msg::ThreadGone(thread, app.history_epoch))
            .unwrap();
        let _ = app.update(Message::Tick);
        assert_eq!(app.shell.thread, None, "the pruned thread closed");
        assert_eq!(app.shell.history.lengths(), recorded, "and nothing moved");
        assert!(app.destination(true).is_some(), "Board is still ahead");

        // The launched session is listed running: its terminal opens.
        app.shell.attach_when_listed = Some(("launched".into(), None));
        let mut launched = session_in("launched", &alpha);
        launched.spec.tty = true;
        messages
            .send(Msg::Agents(vec![launched], BTreeMap::new()))
            .unwrap();
        let _ = app.update(Message::Tick);
        assert_eq!(app.screen, Screen::Terminal);
        assert_eq!(app.shell.history.lengths(), (recorded.0 + 1, 0));
        assert_eq!(
            app.destination(false).map(|p| p.screen),
            Some(Screen::Chat),
            "Back returns to where the launch was asked"
        );
    }

    /// The arrows: disabled with nowhere to go, named Back and Forward to
    /// a reader, and saying where they go when pointed at.
    #[test]
    fn the_arrows_are_disabled_at_the_ends_and_name_where_they_go() {
        use super::super::view::testing::seen;
        let (mut app, alpha, _, _kept) = travel();
        let c = super::super::style::Colors::new(false);
        let arrows = |app: &App| {
            let seen = seen(app.travel_controls(c.rail(), c));
            let state = |id: &str| {
                let (_, label, _, enabled) = seen.control(id).expect(id).clone();
                (label, enabled)
            };
            (state("history-back"), state("history-forward"))
        };
        assert_eq!(
            arrows(&app),
            (("Back".into(), false), ("Forward".into(), false))
        );
        assert_eq!(app.travel_tip(false), "Back");

        let _ = app.update(Message::SelectProject(alpha.clone()));
        assert_eq!(app.travel_tip(false), "Back to All projects");
        let _ = app.update(Message::Navigate(Screen::Board));
        assert_eq!(
            arrows(&app),
            (("Back".into(), true), ("Forward".into(), false))
        );
        assert_eq!(app.travel_tip(false), "Back to alpha · Chat");

        let _ = app.update(Message::GoBack);
        assert_eq!(
            arrows(&app),
            (("Back".into(), true), ("Forward".into(), true))
        );
        assert_eq!(app.travel_tip(true), "Forward to alpha · Board");
        let _ = app.update(Message::GoBack);
        assert_eq!(
            arrows(&app),
            (("Back".into(), false), ("Forward".into(), true))
        );

        // A session and a place of the window's own read by name.
        app.agents = vec![session_in("worker", &alpha)];
        let _ = app.update(Message::OpenSession("worker".into()));
        let _ = app.update(Message::Navigate(Screen::Settings));
        let name = app.name_of("worker");
        assert_eq!(app.travel_tip(false), format!("Back to alpha · {name}"));
        let _ = app.update(Message::GoBack);
        assert_eq!(app.travel_tip(true), "Forward to Settings");
    }

    /// The board on view is part of a Board place: a card comes back on
    /// its own board, picked first, and a switch of boards is a move of
    /// its own; a board that is gone is passed over.
    #[test]
    fn back_restores_the_board_before_its_card() {
        let (mut app, alpha, _, _kept) = travel();
        let root = alpha.display().to_string();
        let main =
            agentdocker_core::Board::main(ProjectRef::directory(&alpha).id(), "alpha", Utc::now());
        let mut other = main.clone();
        other.slug = "agents".into();
        other.name = "Agents".into();
        app.boards.insert(root.clone(), vec![main.clone(), other]);
        let card = card_in("aaaaaaaaaaaa", &alpha, "Port the parser");
        let board_on_view = |app: &App| app.board_on_view().map(|b| b.slug);
        let _ = app.update(Message::SelectProject(alpha.clone()));
        let _ = app.update(Message::Navigate(Screen::Board));
        let _ = app.update(Message::TaskOpen(card.id.clone()));
        let _ = app.update(Message::BoardPick("agents".into()));
        assert_eq!(app.task_open, None, "picking a board closes the card");
        assert_eq!(
            app.destination(false)
                .map(|p| (p.board.clone(), p.card.clone())),
            Some((Some("main".into()), Some(card.id.clone()))),
            "switching boards was a move"
        );
        let _ = app.update(Message::Navigate(Screen::Journal));

        let _ = app.update(Message::GoBack);
        assert_eq!(board_on_view(&app).as_deref(), Some("agents"));
        assert_eq!(app.task_open, None);
        assert_eq!(app.travel_tip(false), "Back to alpha · Board");
        let _ = app.update(Message::GoBack);
        assert_eq!(board_on_view(&app).as_deref(), Some("main"));
        assert_eq!(
            app.task_open.as_ref(),
            Some(&card.id),
            "the card is open on the board it was open on"
        );
        assert_eq!(app.travel_tip(true), "Forward to alpha · Agents");
        let _ = app.update(Message::GoForward);
        assert_eq!(board_on_view(&app).as_deref(), Some("agents"));
        assert_eq!(app.task_open, None);
        let _ = app.update(Message::GoForward);
        assert_eq!(app.screen, Screen::Journal);

        // The other board is deleted: its place is passed over.
        app.boards.insert(root, vec![main]);
        assert_eq!(
            app.destination(false)
                .map(|p| (p.board.clone(), p.card.clone())),
            Some((Some("main".into()), Some(card.id.clone())))
        );
    }

    /// A compact window shows the list of conversations or one of them:
    /// each is a place, in Messages and in the older Inbox alike, and
    /// going back restores which was on view.
    #[test]
    fn the_list_and_a_conversation_are_places_of_their_own() {
        let (mut app, alpha, _, _kept) = travel();
        app.agents = vec![session_in("worker", &alpha)];
        let _ = app.update(Message::Navigate(Screen::Questions));
        let _ = app.update(Message::SelectConversation("dm:user:worker".into()));
        assert!(app.shell.inbox_open);
        let _ = app.update(Message::InboxList);
        assert!(!app.shell.inbox_open);
        assert_eq!(
            app.destination(false).map(|p| p.detail),
            Some(true),
            "leaving the conversation for the list was a move"
        );
        let _ = app.update(Message::Navigate(Screen::Settings));

        let _ = app.update(Message::GoBack);
        assert_eq!(app.screen, Screen::Questions);
        assert_eq!(app.shell.conversation.as_deref(), Some("dm:user:worker"));
        assert!(!app.shell.inbox_open, "the list, as it was");
        let _ = app.update(Message::GoBack);
        assert!(app.shell.inbox_open, "then the conversation");
        let _ = app.update(Message::GoForward);
        assert!(!app.shell.inbox_open);

        // The older Inbox: one agent's thread, or the list.
        let _ = app.update(Message::SelectThread(Some("worker".into())));
        let _ = app.update(Message::InboxList);
        let _ = app.update(Message::GoBack);
        assert_eq!(app.shell.inbox_thread.as_deref(), Some("worker"));
        assert!(app.shell.inbox_open);
        let _ = app.update(Message::GoForward);
        assert!(!app.shell.inbox_open);
    }

    /// Run a task's widget operations against one focusable control with
    /// this id, and say whether it ends up with the keyboard.
    fn focuses(task: Task<Message>, id: &'static str) -> bool {
        use iced::advanced::widget::operation::Focusable;
        use iced::futures::StreamExt;
        struct Probe(bool);
        impl Focusable for Probe {
            fn is_focused(&self) -> bool {
                self.0
            }
            fn focus(&mut self) {
                self.0 = true;
            }
            fn unfocus(&mut self) {
                self.0 = false;
            }
        }
        let Some(mut stream) = iced_runtime::task::into_stream(task) else {
            return false;
        };
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(async {
                let mut probe = Probe(false);
                while let Ok(Some(action)) =
                    tokio::time::timeout(Duration::from_millis(200), stream.next()).await
                {
                    if let iced_runtime::Action::Widget(mut operation) = action {
                        operation.focusable(
                            Some(&iced::advanced::widget::Id::new(id)),
                            iced::Rectangle::default(),
                            &mut probe,
                        );
                    }
                }
                probe.0
            })
    }

    /// Going back or forward to a terminal still attached keeps its
    /// connection and gives its pane the keyboard again, so typing
    /// reaches the agent at once.
    #[test]
    fn back_to_an_attached_terminal_gives_it_the_keyboard() {
        let (mut app, alpha, _, _kept) = travel();
        // Not one the window could attach anew: only the connection it
        // holds can bring the terminal back.
        app.agents = vec![session_in("worker", &alpha)];
        let _ = app.update(Message::SelectProject(alpha.clone()));
        app.navigating(true, |app| {
            let mut terminal = crate::terminal::Terminal::unserved("worker");
            terminal.input_notice = Some("the same connection");
            app.terminal = Some(terminal);
            app.screen = Screen::Terminal;
        });
        let _ = app.update(Message::Navigate(Screen::Board));

        let task = app.update(Message::GoBack);
        assert_eq!(app.screen, Screen::Terminal);
        assert!(focuses(task, "terminal"), "the pane takes the keyboard");
        assert_eq!(
            app.terminal.as_ref().and_then(|t| t.input_notice),
            Some("the same connection"),
            "not reconnected"
        );
        let _ = app.update(Message::GoBack);
        assert_eq!(app.screen, Screen::Chat);
        let task = app.update(Message::GoForward);
        assert_eq!(app.screen, Screen::Terminal);
        assert!(focuses(task, "terminal"));
        let task = app.update(Message::GoForward);
        assert_eq!(app.screen, Screen::Board);
        assert!(!focuses(task, "terminal"), "Board does not take it");
    }

    /// ⌘[ and ⌘] (Control elsewhere) go back and forward ahead of a
    /// focused field, as do a keyboard's Browser keys and the mouse's own
    /// buttons; a terminal that owns Control keeps them, the open palette
    /// swallows them, and nothing moves while it is open.
    #[test]
    fn keys_and_mouse_buttons_go_back_and_forward() {
        use super::super::palette::{Command, Handling, Keys, handle};
        use iced::keyboard::key::{Code, Named, Physical};
        use iced::keyboard::{Key, Modifiers};
        let bracket = |c: &str, code: Code, modifiers: Modifiers, keys: Keys, repeat: bool| {
            handle(
                keys,
                &Key::Character(c.into()),
                Physical::Code(code),
                modifiers,
                repeat,
            )
        };
        let closed = Keys::Closed { shortcut: true };
        let terminal = Keys::Closed { shortcut: false };
        let open = Keys::Open {
            query_empty: true,
            nested: false,
        };
        let command = Modifiers::COMMAND;
        assert!(matches!(
            bracket("[", Code::BracketLeft, command, closed, false),
            Handling::Send(Command::Travel(false))
        ));
        assert!(matches!(
            bracket("]", Code::BracketRight, command, closed, false),
            Handling::Send(Command::Travel(true))
        ));
        // A layout whose bracket key types another letter goes by the key.
        assert!(matches!(
            bracket("ü", Code::BracketLeft, command, closed, false),
            Handling::Send(Command::Travel(false))
        ));
        assert!(matches!(
            bracket("[", Code::BracketLeft, command, closed, true),
            Handling::Swallow
        ));
        assert!(matches!(
            bracket("[", Code::BracketLeft, command, terminal, false),
            Handling::Pass
        ));
        assert!(matches!(
            bracket("]", Code::BracketRight, command, open, false),
            Handling::Swallow
        ));
        for modifiers in [
            Modifiers::empty(),
            command | Modifiers::SHIFT,
            command | Modifiers::ALT,
        ] {
            assert!(matches!(
                bracket("[", Code::BracketLeft, modifiers, closed, false),
                Handling::Pass
            ));
        }
        // A keyboard's own Back and Forward keys mean nothing to a shell:
        // they go back and forward over a terminal too.
        for keys in [closed, terminal] {
            assert!(matches!(
                handle(
                    keys,
                    &Key::Named(Named::BrowserBack),
                    Physical::Code(Code::BrowserBack),
                    Modifiers::empty(),
                    false
                ),
                Handling::Send(Command::Travel(false))
            ));
            assert!(matches!(
                handle(
                    keys,
                    &Key::Named(Named::BrowserForward),
                    Physical::Code(Code::BrowserForward),
                    Modifiers::empty(),
                    false
                ),
                Handling::Send(Command::Travel(true))
            ));
        }
        assert!(matches!(
            bracket("]", Code::BracketRight, command, terminal, false),
            Handling::Pass
        ));
        assert!(matches!(
            Message::from(Command::Travel(false)),
            Message::GoBack
        ));
        assert!(matches!(
            Message::from(Command::Travel(true)),
            Message::GoForward
        ));
        use iced::event::Status;
        use iced::mouse::{Button, Event as Mouse};
        for status in [Status::Ignored, Status::Captured] {
            assert!(matches!(
                super::super::navigation::listened(
                    iced::Event::Mouse(Mouse::ButtonPressed(Button::Back)),
                    status
                ),
                Some(Message::GoBack)
            ));
            assert!(matches!(
                super::super::navigation::listened(
                    iced::Event::Mouse(Mouse::ButtonPressed(Button::Forward)),
                    status
                ),
                Some(Message::GoForward)
            ));
        }
        assert!(
            super::super::navigation::listened(
                iced::Event::Mouse(Mouse::ButtonPressed(Button::Left)),
                Status::Ignored
            )
            .is_none()
        );

        // Over the palette, Back does nothing.
        let (mut app, alpha, _, _kept) = travel();
        let _ = app.update(Message::SelectProject(alpha.clone()));
        let _ = app.update(Message::CommandPalette);
        let _ = app.update(Message::GoBack);
        assert_eq!(on(&app), (Screen::Chat, Some(alpha)));
        let _ = app.update(Message::CommandPalette);
        let _ = app.update(Message::GoBack);
        assert_eq!(on(&app), (Screen::Agents, None));
    }
}

/// The whole of a card's links, replacing its own.
fn update_links(task: agentdocker_core::TaskId, links: Vec<agentdocker_core::Link>) -> Request {
    Request::TaskUpdate {
        agent: agentdocker_core::HUMAN.into(),
        task: task.to_string(),
        title: None,
        acceptance: None,
        assignee: None,
        links: Some(links),
        description: None,
        priority: None,
        size: None,
        due: None,
        external_blocker: None,
        parent: None,
        blocked_by: None,
        tags: None,
        plan: None,
        expected_updated_at: None,
    }
}

/// Where opening a link goes.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum LinkDestination {
    /// A URL, or a path, opened with the system's app for it.
    Open(String),
    /// Another card, opened in place.
    Card(agentdocker_core::TaskId),
    /// Something with nowhere to open (a commit, a message, a memory):
    /// its words go to the clipboard.
    Copy(String),
}

/// Where a link opens: a URL or a pull request URL in the browser, a path
/// (relative to the project) in its app, a card in place; anything else is
/// copied.
pub(super) fn link_destination(
    link: &agentdocker_core::Link,
    project: Option<&str>,
) -> LinkDestination {
    use agentdocker_core::LinkKind;
    let target = link.target.trim();
    let web = target.starts_with("https://") || target.starts_with("http://");
    match link.kind {
        LinkKind::Url | LinkKind::Pr if web => LinkDestination::Open(target.to_owned()),
        LinkKind::Path => {
            let path = std::path::Path::new(target);
            let path = match project {
                Some(root) if path.is_relative() => std::path::Path::new(root).join(path),
                _ => path.to_path_buf(),
            };
            LinkDestination::Open(path.display().to_string())
        }
        LinkKind::Task => LinkDestination::Card(agentdocker_core::TaskId::from(target.to_owned())),
        _ => LinkDestination::Copy(target.to_owned()),
    }
}
