//! A board of work beside the sessions: cards with what done means, in
//! lanes, pulled by agents one at a time. The person files work here
//! without opening a terminal; an agent pulls a card and nobody else can
//! take it; the board says where everything is. The daemon's single
//! mutex is what makes a pull atomic; this module is the rules, and
//! `board` the lanes they are drawn in.
use crate::board::{Board, Lane};
use crate::{AgentId, ProjectId};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

/// What a card's words may be, so a card is a card and not a document.
pub const TITLE_CHARS: usize = 200;
pub const ACCEPTANCE_CHARS: usize = 4000;
pub const DESCRIPTION_CHARS: usize = 16_000;
pub const BLOCKER_CHARS: usize = 500;
pub const COMMENT_CHARS: usize = 4000;
/// At most this many tags on a card, each at most this long.
pub const TAGS_MAX: usize = 8;
pub const TAG_CHARS: usize = 32;
/// At most this many cards a card waits on.
pub const BLOCKERS_MAX: usize = 16;
/// At most this many cards filed in one batch.
pub const BATCH_MAX: usize = 50;

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TaskId(String);

impl TaskId {
    pub fn generate() -> Self {
        let raw = uuid::Uuid::new_v4().simple().to_string();
        Self(raw[..12].to_owned())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for TaskId {
    fn from(raw: String) -> Self {
        Self(raw)
    }
}

impl std::fmt::Display for TaskId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// What kind of place a card is in. Backlog is the person's to fill;
/// Ready is what an agent may pull; In progress and Review are the work
/// moving; Done ends it, Cancelled too, without it being done; Blocked
/// waits on something off the board. A board's lanes each stand for one
/// of these (`board::Lane`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Column {
    Backlog,
    Ready,
    InProgress,
    Review,
    Done,
    Blocked,
    Cancelled,
}

impl Column {
    /// The five lanes every project's board had, in order.
    pub const ALL: [Column; 5] = [
        Column::Backlog,
        Column::Ready,
        Column::InProgress,
        Column::Review,
        Column::Done,
    ];

    /// Every category, in board order.
    pub const CATEGORIES: [Column; 7] = [
        Column::Backlog,
        Column::Ready,
        Column::InProgress,
        Column::Review,
        Column::Done,
        Column::Blocked,
        Column::Cancelled,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Column::Backlog => "backlog",
            Column::Ready => "ready",
            Column::InProgress => "in_progress",
            Column::Review => "review",
            Column::Done => "done",
            Column::Blocked => "blocked",
            Column::Cancelled => "cancelled",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Column::Backlog => "Backlog",
            Column::Ready => "Ready",
            Column::InProgress => "In progress",
            Column::Review => "Review",
            Column::Done => "Done",
            Column::Blocked => "Blocked",
            Column::Cancelled => "Cancelled",
        }
    }

    /// `backlog`, `ready`, `in-progress`/`in_progress`/`doing`, `review`,
    /// `done`, `blocked`, `cancelled`, whatever the case.
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_lowercase().replace(['-', ' '], "_").as_str() {
            "backlog" => Some(Column::Backlog),
            "ready" | "todo" | "to_do" | "approved" => Some(Column::Ready),
            "in_progress" | "doing" | "progress" => Some(Column::InProgress),
            "review" | "in_review" => Some(Column::Review),
            "done" => Some(Column::Done),
            "blocked" => Some(Column::Blocked),
            "cancelled" | "canceled" => Some(Column::Cancelled),
            _ => None,
        }
    }

    /// Where the work is under way: a card here is held by its lease.
    pub fn working(&self) -> bool {
        matches!(self, Column::InProgress | Column::Review)
    }

    /// Where a card ends: done, or given up.
    pub fn terminal(&self) -> bool {
        matches!(self, Column::Done | Column::Cancelled)
    }

    /// Where the person puts a card back for the next taker.
    pub fn waiting(&self) -> bool {
        matches!(self, Column::Backlog | Column::Ready)
    }
}

impl std::fmt::Display for Column {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// How much a card matters next to the others.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    High,
    #[default]
    Normal,
    Low,
}

impl Priority {
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_lowercase().as_str() {
            "high" | "h" | "urgent" => Some(Priority::High),
            "normal" | "n" | "norm" | "medium" => Some(Priority::Normal),
            "low" | "l" => Some(Priority::Low),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Priority::High => "high",
            Priority::Normal => "normal",
            Priority::Low => "low",
        }
    }

    fn is_normal(&self) -> bool {
        *self == Priority::Normal
    }
}

/// Roughly how much work a card is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Size {
    S,
    M,
    L,
}

impl Size {
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_lowercase().as_str() {
            "s" | "small" => Some(Size::S),
            "m" | "medium" => Some(Size::M),
            "l" | "large" => Some(Size::L),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Size::S => "s",
            Size::M => "m",
            Size::L => "l",
        }
    }
}

/// One card.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    pub id: TaskId,
    pub project: ProjectId,
    /// Its display key, `AD-12`; empty for a card filed before keys.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub key: String,
    /// The board it is on; the project's main board when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub board: Option<String>,
    pub title: String,
    /// What done means. An agent reads this before moving the card.
    #[serde(default)]
    pub acceptance: String,
    /// What the work is, at length, in Markdown.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Its category.
    pub column: Column,
    /// The board lane it is in, by slug; the first of its category when
    /// absent or gone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lane: Option<String>,
    /// Where it sits in its lane: `board::order`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub rank: String,
    /// Who holds it: set by a pull, or by the person.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<AgentId>,
    #[serde(default, skip_serializing_if = "Priority::is_normal")]
    pub priority: Priority,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<Size>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due: Option<NaiveDate>,
    /// Something off the board it waits on, in words (a vendor, a DNS
    /// change). A pull with no card named passes it by.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub external_blocker: String,
    /// The card it is part of.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<TaskId>,
    /// Cards that must be done first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_by: Vec<TaskId>,
    /// Lowercase words the person sorts cards by.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Who filed it, as an agent id or `user`.
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Off the board, kept for the record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<DateTime<Utc>>,
    /// What the card points at: a path, a pull request, a commit, a
    /// message, a memory for whoever takes it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<crate::Link>,
    /// The plan (a spec document) it carries out, by id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    /// When it came into the lane it is in: what an idle rule counts
    /// from. Its last change, for a card from before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lane_since: Option<DateTime<Utc>>,
    /// The rules that have acted on it in this lane, by id, so an idle
    /// rule acts once a stay. Cleared when it changes lane.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fired: Vec<String>,
}

/// A card being filed: everything but who, when and where on the board,
/// which the filing decides.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NewTask {
    pub title: String,
    pub acceptance: String,
    pub description: String,
    /// Backlog when absent.
    pub column: Option<Column>,
    pub lane: Option<String>,
    pub priority: Priority,
    pub size: Option<Size>,
    pub due: Option<NaiveDate>,
    pub external_blocker: String,
    pub parent: Option<TaskId>,
    pub blocked_by: Vec<TaskId>,
    pub tags: Vec<String>,
    pub links: Vec<crate::Link>,
    pub plan: Option<String>,
}

/// What an update changes: each field left `None` stays as it was; an
/// inner `None` clears it (`assignee: Some(None)` takes the card away
/// from whoever holds it).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TaskEdit<'a> {
    pub title: Option<&'a str>,
    pub acceptance: Option<&'a str>,
    pub description: Option<&'a str>,
    pub assignee: Option<Option<AgentId>>,
    pub links: Option<Vec<crate::Link>>,
    pub priority: Option<Priority>,
    pub size: Option<Option<Size>>,
    pub due: Option<Option<NaiveDate>>,
    pub external_blocker: Option<&'a str>,
    pub parent: Option<Option<TaskId>>,
    pub blocked_by: Option<Vec<TaskId>>,
    pub tags: Option<Vec<String>>,
    pub plan: Option<Option<String>>,
}

/// Why a change to a card is refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskError {
    /// Words out of bounds, or a reference that cannot be.
    Invalid(&'static str),
    /// A pull of a card somebody already holds, or that is not in the
    /// pull source.
    Taken {
        assignee: Option<AgentId>,
        column: Column,
    },
    /// A move by somebody who is neither its assignee nor the person.
    NotYours,
    /// A card that is archived.
    Archived,
    /// A parent or blocker that would make a card wait on itself.
    Cycle,
}

impl std::fmt::Display for TaskError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TaskError::Invalid(reason) => f.write_str(reason),
            TaskError::Taken {
                assignee: Some(who),
                ..
            } => write!(f, "the card is held by {who}"),
            TaskError::Taken { column, .. } => {
                write!(f, "the card is not ready to pull; it is in {column}")
            }
            TaskError::NotYours => f.write_str(
                "only the card's assignee or the person moves it; an agent takes a card nobody holds by pulling it by name",
            ),
            TaskError::Archived => f.write_str("the card is archived"),
            TaskError::Cycle => f.write_str("that would make the card wait on itself"),
        }
    }
}

/// A tag as cards keep it: trimmed, lowercase, inner spaces as `-`.
pub fn tag_of(text: &str) -> String {
    text.trim()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-")
}

impl Task {
    /// A new card filed by `created_by`, in the category and lane asked
    /// (Backlog when none), with its words checked.
    pub fn file(
        project: ProjectId,
        new: NewTask,
        created_by: &str,
        now: DateTime<Utc>,
    ) -> Result<Self, TaskError> {
        let mut tags: Vec<String> = new.tags.iter().map(|t| tag_of(t)).collect();
        tags.sort();
        tags.dedup();
        let task = Self {
            id: TaskId::generate(),
            project,
            key: String::new(),
            board: None,
            title: new.title.trim().to_owned(),
            acceptance: new.acceptance.trim().to_owned(),
            description: new.description.trim().to_owned(),
            column: new.column.unwrap_or(Column::Backlog),
            lane: new.lane,
            rank: String::new(),
            assignee: None,
            priority: new.priority,
            size: new.size,
            due: new.due,
            external_blocker: new.external_blocker.trim().to_owned(),
            parent: new.parent,
            blocked_by: new.blocked_by,
            tags,
            created_by: created_by.to_owned(),
            created_at: now,
            updated_at: now,
            archived_at: None,
            links: new.links,
            plan: new.plan,
            lane_since: Some(now),
            fired: Vec::new(),
        };
        task.check()?;
        Ok(task)
    }

    /// A new card with a title and what done means, in a category of the
    /// person's choosing: `file` with nothing else said.
    pub fn new(
        project: ProjectId,
        title: &str,
        acceptance: &str,
        column: Option<Column>,
        created_by: &str,
        links: Vec<crate::Link>,
        now: DateTime<Utc>,
    ) -> Result<Self, TaskError> {
        Self::file(
            project,
            NewTask {
                title: title.to_owned(),
                acceptance: acceptance.to_owned(),
                column,
                links,
                ..NewTask::default()
            },
            created_by,
            now,
        )
    }

    /// Its words and references within bounds.
    pub fn check(&self) -> Result<(), TaskError> {
        let title = self.title.trim();
        if title.is_empty() {
            return Err(TaskError::Invalid("a card needs a title"));
        }
        if title.chars().count() > TITLE_CHARS {
            return Err(TaskError::Invalid(
                "a card's title is at most 200 characters",
            ));
        }
        if self.acceptance.chars().count() > ACCEPTANCE_CHARS {
            return Err(TaskError::Invalid(
                "a card's acceptance text is at most 4000 characters",
            ));
        }
        if self.description.chars().count() > DESCRIPTION_CHARS {
            return Err(TaskError::Invalid(
                "a card's description is at most 16000 characters",
            ));
        }
        if self.external_blocker.chars().count() > BLOCKER_CHARS {
            return Err(TaskError::Invalid(
                "a card's outside blocker is at most 500 characters",
            ));
        }
        if self.tags.len() > TAGS_MAX {
            return Err(TaskError::Invalid("a card has at most eight tags"));
        }
        if self.tags.iter().any(|t| {
            t.is_empty()
                || t.chars().count() > TAG_CHARS
                || !t
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '.')
        }) {
            return Err(TaskError::Invalid(
                "a tag is one to thirty-two letters, digits, -, _ or .",
            ));
        }
        if self.blocked_by.len() > BLOCKERS_MAX {
            return Err(TaskError::Invalid("a card waits on at most sixteen cards"));
        }
        if self.parent.as_ref() == Some(&self.id) || self.blocked_by.contains(&self.id) {
            return Err(TaskError::Cycle);
        }
        crate::link::check(&self.links).map_err(TaskError::Invalid)?;
        Ok(())
    }

    /// The lane it is in on `board`, by slug.
    pub fn lane_on<'b>(&self, board: &'b Board) -> &'b Lane {
        board.lane_of(self)
    }

    /// Put it in a lane: its category comes with it, and its stay there
    /// starts now, with no rule yet acted on it.
    fn place(&mut self, lane: &Lane, now: DateTime<Utc>) {
        self.column = lane.category;
        self.lane = Some(lane.slug.clone());
        self.lane_since = Some(now);
        self.fired.clear();
    }

    /// When its stay in its lane began.
    pub fn in_lane_since(&self) -> DateTime<Utc> {
        self.lane_since.unwrap_or(self.updated_at)
    }

    /// An agent takes a card nobody else holds — free, or already its own
    /// — that waits on nothing off the board, and it becomes theirs. A card
    /// it looks for (`named` false) must be in the board's pull source, so
    /// an agent finding work never helps itself to the person's backlog; a
    /// card it names may sit in any open lane, since taking a card nobody
    /// holds is ordinary work, not the person's say. One waiting (Backlog,
    /// Ready) goes to the pull target; one further on stays where it is.
    /// Anything else is refused, which is the whole point — a card somebody
    /// holds, or held once and lapsed, is only taken over by name, with
    /// [`Task::take_over`]. Whether its blocking cards are done is the
    /// daemon's to check, from the board, before this.
    pub fn pull(
        &mut self,
        agent: &AgentId,
        board: &Board,
        named: bool,
        now: DateTime<Utc>,
    ) -> Result<(), TaskError> {
        if self.archived_at.is_some() {
            return Err(TaskError::Archived);
        }
        let in_source = self.lane_on(board).slug == board.pull_source;
        let reachable = in_source || (named && self.open());
        let free = self.assignee.as_ref().is_none_or(|a| a == agent);
        if !reachable || !free {
            return Err(TaskError::Taken {
                assignee: self.assignee.clone(),
                column: self.column,
            });
        }
        if !self.external_blocker.trim().is_empty() {
            return Err(TaskError::Invalid(
                "the card waits on something off the board",
            ));
        }
        if in_source || self.column.waiting() {
            let target = board
                .lane(&board.pull_target)
                .expect("a checked board has its pull target");
            self.place(target, now);
        }
        self.assignee = Some(agent.clone());
        self.updated_at = now;
        Ok(())
    }

    /// Explicit recovery of a card whose holder's lease lapsed: the taker
    /// names whom it expects to take the card from — its holder, or
    /// itself, re-taking its own lapsed hold — and the card passes where
    /// it sits, in progress or review. Whether the hold has in fact
    /// lapsed is the daemon's to check, from the lease, before this; a
    /// card that names somebody else, or sits in another column, is
    /// refused so a stale expectation never takes a card.
    pub fn take_over(
        &mut self,
        agent: &AgentId,
        from: &AgentId,
        now: DateTime<Utc>,
    ) -> Result<(), TaskError> {
        if self.archived_at.is_some() {
            return Err(TaskError::Archived);
        }
        if !self.column.working() || self.assignee.as_ref() != Some(from) {
            return Err(TaskError::Taken {
                assignee: self.assignee.clone(),
                column: self.column,
            });
        }
        self.assignee = Some(agent.clone());
        self.updated_at = now;
        Ok(())
    }

    /// Move the card to a lane: its assignee may, and the person always
    /// may. A card moved back to Backlog or Ready by the person is
    /// released for the next taker; one moved to Done by its assignee
    /// stays theirs, for the record. Answers whether anything changed: a
    /// move to where it already is changes nothing.
    pub fn move_to(
        &mut self,
        by: &str,
        by_is_human: bool,
        lane: &Lane,
        now: DateTime<Utc>,
    ) -> Result<bool, TaskError> {
        if self.archived_at.is_some() {
            return Err(TaskError::Archived);
        }
        if !by_is_human && self.assignee.as_ref().is_none_or(|a| a.as_str() != by) {
            return Err(TaskError::NotYours);
        }
        if lane.category == self.column && self.lane.as_deref().is_none_or(|l| l == lane.slug) {
            if self.lane.is_none() {
                // Its lane is said from now on; nothing moved.
                self.lane = Some(lane.slug.clone());
            }
            return Ok(false);
        }
        self.place(lane, now);
        if by_is_human && lane.category.waiting() {
            self.assignee = None;
        }
        self.updated_at = now;
        Ok(true)
    }

    /// The person edits what the card says, or hands it to somebody
    /// (`assignee: Some(None)` takes it away). An agent may only edit a
    /// card it holds, and cannot reassign it. `links`, `tags` and
    /// `blocked_by` replace the card's whole. Answers the fields that
    /// changed, by name, for the record.
    pub fn update(
        &mut self,
        by: &str,
        by_is_human: bool,
        edit: TaskEdit,
        now: DateTime<Utc>,
    ) -> Result<Vec<&'static str>, TaskError> {
        if self.archived_at.is_some() {
            return Err(TaskError::Archived);
        }
        if !by_is_human
            && (edit.assignee.is_some() || self.assignee.as_ref().is_none_or(|a| a.as_str() != by))
        {
            return Err(TaskError::NotYours);
        }
        let mut next = self.clone();
        let mut changed = Vec::new();
        macro_rules! set {
            ($field:ident, $value:expr, $name:literal) => {
                let value = $value;
                if next.$field != value {
                    next.$field = value;
                    changed.push($name);
                }
            };
        }
        if let Some(title) = edit.title {
            set!(title, title.trim().to_owned(), "title");
        }
        if let Some(acceptance) = edit.acceptance {
            set!(acceptance, acceptance.trim().to_owned(), "acceptance");
        }
        if let Some(description) = edit.description {
            set!(description, description.trim().to_owned(), "description");
        }
        if let Some(assignee) = edit.assignee {
            set!(assignee, assignee, "assignee");
        }
        if let Some(links) = edit.links {
            set!(links, links, "links");
        }
        if let Some(priority) = edit.priority {
            set!(priority, priority, "priority");
        }
        if let Some(size) = edit.size {
            set!(size, size, "size");
        }
        if let Some(due) = edit.due {
            set!(due, due, "due");
        }
        if let Some(blocker) = edit.external_blocker {
            set!(
                external_blocker,
                blocker.trim().to_owned(),
                "external_blocker"
            );
        }
        if let Some(parent) = edit.parent {
            set!(parent, parent, "parent");
        }
        if let Some(blocked_by) = edit.blocked_by {
            let mut blocked_by = blocked_by;
            blocked_by.dedup();
            set!(blocked_by, blocked_by, "blocked_by");
        }
        if let Some(tags) = edit.tags {
            let mut tags: Vec<String> = tags.iter().map(|t| tag_of(t)).collect();
            tags.sort();
            tags.dedup();
            set!(tags, tags, "tags");
        }
        if let Some(plan) = edit.plan {
            set!(plan, plan, "plan");
        }
        next.check()?;
        if !changed.is_empty() {
            next.updated_at = now;
        }
        *self = next;
        Ok(changed)
    }

    /// Off the board. The person's to do, or the assignee's for a card
    /// that has ended (done or cancelled).
    pub fn archive(
        &mut self,
        by: &str,
        by_is_human: bool,
        now: DateTime<Utc>,
    ) -> Result<(), TaskError> {
        if self.archived_at.is_some() {
            return Ok(());
        }
        let theirs_ended =
            self.column.terminal() && self.assignee.as_ref().is_some_and(|a| a.as_str() == by);
        if !by_is_human && !theirs_ended {
            return Err(TaskError::NotYours);
        }
        self.archived_at = Some(now);
        self.updated_at = now;
        Ok(())
    }

    /// Back on the board where it was: the person's to do.
    pub fn unarchive(&mut self, by_is_human: bool, now: DateTime<Utc>) -> Result<(), TaskError> {
        if !by_is_human {
            return Err(TaskError::NotYours);
        }
        if self.archived_at.take().is_some() {
            self.updated_at = now;
        }
        Ok(())
    }

    /// Whether it is still to be finished: not ended, not archived.
    pub fn open(&self) -> bool {
        self.archived_at.is_none() && !self.column.terminal()
    }

    /// What people call it: its key, or the start of its id.
    pub fn handle(&self) -> String {
        if self.key.is_empty() {
            self.id.as_str().chars().take(7).collect()
        } else {
            self.key.clone()
        }
    }
}

/// Whether making `task` wait on (or be part of) `edges` closes a loop:
/// following `next` (each card's parent and blockers) from those cards
/// leads back to `task`. `next` answers the edges of a card by id; the
/// walk visits at most a few thousand cards.
pub fn closes_cycle(
    task: &TaskId,
    edges: &[TaskId],
    mut next: impl FnMut(&TaskId) -> Vec<TaskId>,
) -> bool {
    let mut seen = std::collections::BTreeSet::new();
    let mut stack: Vec<TaskId> = edges.to_vec();
    while let Some(id) = stack.pop() {
        if &id == task {
            return true;
        }
        if seen.len() > 4096 || !seen.insert(id.clone()) {
            continue;
        }
        stack.extend(next(&id));
    }
    false
}

/// A comment's id.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CommentId(String);

impl CommentId {
    pub fn generate() -> Self {
        let raw = uuid::Uuid::new_v4().simple().to_string();
        Self(raw[..12].to_owned())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for CommentId {
    fn from(raw: String) -> Self {
        Self(raw)
    }
}

impl std::fmt::Display for CommentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Words on a card, in its thread, one reply deep.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comment {
    pub id: CommentId,
    pub task: TaskId,
    pub project: ProjectId,
    /// An agent id, or `user`.
    pub author: String,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<CommentId>,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edited_at: Option<DateTime<Utc>>,
    /// Deleted: the body is gone, the place in the thread kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<DateTime<Utc>>,
}

impl Comment {
    pub fn new(
        task: &Task,
        author: &str,
        body: &str,
        reply_to: Option<CommentId>,
        now: DateTime<Utc>,
    ) -> Result<Self, TaskError> {
        check_body(body)?;
        Ok(Self {
            id: CommentId::generate(),
            task: task.id.clone(),
            project: task.project.clone(),
            author: author.to_owned(),
            body: body.trim().to_owned(),
            reply_to,
            created_at: now,
            edited_at: None,
            deleted_at: None,
        })
    }

    /// Its author edits it.
    pub fn edit(&mut self, by: &str, body: &str, now: DateTime<Utc>) -> Result<(), TaskError> {
        if self.author != by {
            return Err(TaskError::NotYours);
        }
        if self.deleted_at.is_some() {
            return Err(TaskError::Invalid("the comment was deleted"));
        }
        check_body(body)?;
        self.body = body.trim().to_owned();
        self.edited_at = Some(now);
        Ok(())
    }

    /// Its author or the person deletes it.
    pub fn delete(
        &mut self,
        by: &str,
        by_is_human: bool,
        now: DateTime<Utc>,
    ) -> Result<(), TaskError> {
        if self.author != by && !by_is_human {
            return Err(TaskError::NotYours);
        }
        if self.deleted_at.is_none() {
            self.body.clear();
            self.deleted_at = Some(now);
        }
        Ok(())
    }
}

fn check_body(body: &str) -> Result<(), TaskError> {
    let body = body.trim();
    if body.is_empty() {
        return Err(TaskError::Invalid("a comment needs words"));
    }
    if body.chars().count() > COMMENT_CHARS {
        return Err(TaskError::Invalid("a comment is at most 4000 characters"));
    }
    Ok(())
}

/// One line of a card's history: who did what to it, when.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskEntry {
    pub id: String,
    pub task: TaskId,
    pub project: ProjectId,
    pub at: DateTime<Utc>,
    /// An agent id, `user`, or `agentd`.
    pub by: String,
    pub action: TaskAction,
}

/// What happened to a card.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TaskAction {
    Created {
        lane: String,
    },
    Pulled {
        lane: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<String>,
    },
    Moved {
        from: String,
        to: String,
    },
    Updated {
        fields: Vec<String>,
    },
    Assigned {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        to: Option<String>,
    },
    Commented {
        comment: CommentId,
    },
    Archived,
    Unarchived,
}

impl TaskEntry {
    pub fn new(task: &Task, by: &str, action: TaskAction, at: DateTime<Utc>) -> Self {
        Self {
            id: format!("{}-{}", task.id, uuid::Uuid::new_v4().simple()),
            task: task.id.clone(),
            project: task.project.clone(),
            at,
            by: by.to_owned(),
            action,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn board() -> Board {
        Board::main(ProjectId::from("p"), "p", Utc::now())
    }

    fn lane(slug: &str) -> Lane {
        board().lane(slug).unwrap().clone()
    }

    fn ready_card() -> Task {
        Task::new(
            ProjectId::from("p"),
            "Fix login",
            "Login works with SSO",
            Some(Column::Ready),
            "user",
            Vec::new(),
            Utc::now(),
        )
        .unwrap()
    }

    fn edit<'a>() -> TaskEdit<'a> {
        TaskEdit::default()
    }

    /// A card is a title and what done means, within bounds.
    #[test]
    fn a_card_is_a_bounded_title_and_acceptance() {
        assert_eq!(
            Task::new(
                ProjectId::from("p"),
                "  ",
                "",
                None,
                "user",
                Vec::new(),
                Utc::now()
            )
            .unwrap_err(),
            TaskError::Invalid("a card needs a title")
        );
        assert!(
            Task::new(
                ProjectId::from("p"),
                &"x".repeat(201),
                "",
                None,
                "user",
                Vec::new(),
                Utc::now()
            )
            .is_err()
        );
        assert!(
            Task::new(
                ProjectId::from("p"),
                "t",
                &"x".repeat(4001),
                None,
                "user",
                Vec::new(),
                Utc::now()
            )
            .is_err()
        );
        let card = Task::new(
            ProjectId::from("p"),
            " Fix login ",
            " when ",
            None,
            "user",
            Vec::new(),
            Utc::now(),
        )
        .unwrap();
        assert_eq!(
            (card.title.as_str(), card.acceptance.as_str(), card.column),
            ("Fix login", "when", Column::Backlog)
        );
        assert_eq!(Column::parse("In-Progress"), Some(Column::InProgress));
        assert_eq!(Column::parse("todo"), Some(Column::Ready));
        assert_eq!(Column::parse("canceled"), Some(Column::Cancelled));
        assert_eq!(Column::parse("elsewhere"), None);
    }

    /// The fields beside the words: a priority, a size, a due day, an
    /// outside blocker, a parent, blockers and tags, each within bounds;
    /// tags come out lowercase, sorted, once.
    #[test]
    fn a_card_carries_its_fields_within_bounds() {
        let card = Task::file(
            ProjectId::from("p"),
            NewTask {
                title: "Ship it".into(),
                priority: Priority::High,
                size: Some(Size::L),
                due: NaiveDate::from_ymd_opt(2026, 11, 1),
                tags: vec!["Bug".into(), "ui  polish".into(), "bug".into()],
                ..NewTask::default()
            },
            "user",
            Utc::now(),
        )
        .unwrap();
        assert_eq!(card.tags, ["bug", "ui-polish"]);
        assert_eq!((card.priority, card.size), (Priority::High, Some(Size::L)));
        let too_many = NewTask {
            title: "t".into(),
            tags: (0..9).map(|i| format!("t{i}")).collect(),
            ..NewTask::default()
        };
        assert!(Task::file(ProjectId::from("p"), too_many, "user", Utc::now()).is_err());
        let bad_tag = NewTask {
            title: "t".into(),
            tags: vec!["no/slash".into()],
            ..NewTask::default()
        };
        assert!(Task::file(ProjectId::from("p"), bad_tag, "user", Utc::now()).is_err());
        // Serialized, a plain card says no more than it did before fields.
        let plain = ready_card();
        let json = serde_json::to_value(&plain).unwrap();
        for absent in [
            "priority",
            "size",
            "due",
            "tags",
            "blocked_by",
            "lane",
            "rank",
            "key",
        ] {
            assert!(json.get(absent).is_none(), "{absent} on a plain card");
        }
        let back: Task = serde_json::from_value(json).unwrap();
        assert_eq!(back, plain);
    }

    /// A card named by the agent that takes it may sit in any open lane
    /// when nobody holds it: from Backlog it goes to the pull target, from
    /// Review it stays there, and either is then the taker's to move on. A
    /// held card, an ended one or an archived one is not taken that way.
    #[test]
    fn a_named_card_nobody_holds_is_taken_from_any_open_lane() {
        let board = board();
        let a = AgentId::from("a".to_owned());
        let b = AgentId::from("b".to_owned());
        let in_lane = |slug: &str| {
            let mut card = ready_card();
            card.place(&lane(slug), Utc::now());
            card
        };
        let mut backlog = in_lane("backlog");
        backlog.pull(&a, &board, true, Utc::now()).unwrap();
        assert_eq!(
            (backlog.assignee.as_ref(), backlog.column),
            (Some(&a), Column::InProgress)
        );
        assert!(
            backlog
                .move_to(a.as_str(), false, &lane("done"), Utc::now())
                .unwrap()
        );
        let mut review = in_lane("review");
        review.pull(&b, &board, true, Utc::now()).unwrap();
        assert_eq!(
            (review.assignee.as_ref(), review.column),
            (Some(&b), Column::Review)
        );
        // Held by another, ended, or archived: not taken by name either.
        let mut held = in_lane("backlog");
        held.assignee = Some(a.clone());
        assert!(held.pull(&b, &board, true, Utc::now()).is_err());
        let mut done = in_lane("done");
        assert!(done.pull(&b, &board, true, Utc::now()).is_err());
        let mut archived = in_lane("backlog");
        archived.archived_at = Some(Utc::now());
        assert_eq!(
            archived.pull(&b, &board, true, Utc::now()).unwrap_err(),
            TaskError::Archived
        );
        // Looking for work, the agent still takes only from the source.
        let mut skipped = in_lane("backlog");
        assert!(skipped.pull(&b, &board, false, Utc::now()).is_err());
    }

    /// One pull, from the pull source, by the first taker; the second is
    /// told who holds it. A Backlog card is not ready to pull. A card the
    /// caller already holds in the pull source is theirs to pull again.
    #[test]
    fn a_pull_takes_a_ready_card_once() {
        let board = board();
        let mut card = ready_card();
        let a = AgentId::from("a".to_owned());
        let b = AgentId::from("b".to_owned());
        card.pull(&a, &board, false, Utc::now()).unwrap();
        assert_eq!(
            (card.assignee.as_ref(), card.column, card.lane.as_deref()),
            (Some(&a), Column::InProgress, Some("in_progress"))
        );
        assert_eq!(
            card.pull(&b, &board, false, Utc::now()).unwrap_err(),
            TaskError::Taken {
                assignee: Some(a.clone()),
                column: Column::InProgress
            }
        );
        let mut backlog = ready_card();
        backlog.column = Column::Backlog;
        assert_eq!(
            backlog.pull(&b, &board, false, Utc::now()).unwrap_err(),
            TaskError::Taken {
                assignee: None,
                column: Column::Backlog
            }
        );
        // Back in Ready by its holder, still named theirs: theirs to pull.
        let mut stranded = ready_card();
        stranded.assignee = Some(a.clone());
        stranded.pull(&a, &board, false, Utc::now()).unwrap();
        assert_eq!(stranded.column, Column::InProgress);
        // Waiting on something off the board: not pulled.
        let mut waiting = ready_card();
        waiting.external_blocker = "vendor".into();
        assert!(waiting.pull(&a, &board, false, Utc::now()).is_err());
        // A take-over names whom the card is taken from: the wrong name,
        // or a card outside In progress/Review, takes nothing. The right
        // name passes the card where it sits — including to its own
        // holder, re-taking a lapsed hold.
        assert_eq!(
            backlog.take_over(&b, &a, Utc::now()).unwrap_err(),
            TaskError::Taken {
                assignee: None,
                column: Column::Backlog
            }
        );
        card.move_to("a", false, &lane("review"), Utc::now())
            .unwrap();
        assert_eq!(
            card.take_over(&b, &b, Utc::now()).unwrap_err(),
            TaskError::Taken {
                assignee: Some(a.clone()),
                column: Column::Review
            }
        );
        card.take_over(&a, &a, Utc::now()).unwrap();
        assert_eq!(card.assignee.as_ref(), Some(&a), "re-taken by its holder");
        card.take_over(&b, &a, Utc::now()).unwrap();
        assert_eq!(
            (card.assignee.as_ref(), card.column),
            (Some(&b), Column::Review),
            "taken over where it sat"
        );
    }

    /// A pull on a board with its own lanes takes the card from the pull
    /// source into the pull target, by slug.
    #[test]
    fn a_pull_follows_the_boards_lanes() {
        let board = Board::from_template(ProjectId::from("p"), "w", "W", "agents", "P", Utc::now())
            .unwrap();
        let mut card = ready_card();
        card.lane = Some("approved".into());
        let a = AgentId::from("a".to_owned());
        card.pull(&a, &board, false, Utc::now()).unwrap();
        assert_eq!(
            (card.column, card.lane.as_deref()),
            (Column::InProgress, Some("analyst"))
        );
        // Into UAT, a second Review lane, by name.
        let uat = board.resolve("UAT").unwrap();
        assert!(card.move_to("a", false, uat, Utc::now()).unwrap());
        assert_eq!(
            (card.column, card.lane.as_deref()),
            (Column::Review, Some("uat"))
        );
        assert!(
            !card.move_to("a", false, uat, Utc::now()).unwrap(),
            "a move to where it is changes nothing"
        );
    }

    /// The assignee moves its own card and nobody else's; the person
    /// moves any, and moving one back to Ready or Backlog releases it.
    #[test]
    fn moves_are_the_assignees_or_the_persons() {
        let board = board();
        let mut card = ready_card();
        let a = AgentId::from("a".to_owned());
        card.pull(&a, &board, false, Utc::now()).unwrap();
        assert_eq!(
            card.move_to("b", false, &lane("review"), Utc::now())
                .unwrap_err(),
            TaskError::NotYours
        );
        card.move_to("a", false, &lane("review"), Utc::now())
            .unwrap();
        assert_eq!(card.column, Column::Review);
        card.move_to("a", false, &lane("done"), Utc::now()).unwrap();
        assert_eq!(
            card.assignee.as_ref(),
            Some(&a),
            "done stays theirs, for the record"
        );
        card.move_to("user", true, &lane("ready"), Utc::now())
            .unwrap();
        assert_eq!(
            (card.assignee.as_ref(), card.column),
            (None, Column::Ready),
            "released for the next taker"
        );
        let b = AgentId::from("b".to_owned());
        card.pull(&b, &board, false, Utc::now()).unwrap();
        // Edits: the holder edits words, the person reassigns; an archived
        // card is done with.
        assert_eq!(
            card.update(
                "a",
                false,
                TaskEdit {
                    title: Some("x"),
                    ..edit()
                },
                Utc::now()
            )
            .unwrap_err(),
            TaskError::NotYours
        );
        let changed = card
            .update(
                "b",
                false,
                TaskEdit {
                    title: Some("Fix login properly"),
                    priority: Some(Priority::High),
                    ..edit()
                },
                Utc::now(),
            )
            .unwrap();
        assert_eq!(changed, ["title", "priority"]);
        assert_eq!(
            card.update(
                "b",
                false,
                TaskEdit {
                    assignee: Some(None),
                    ..edit()
                },
                Utc::now()
            )
            .unwrap_err(),
            TaskError::NotYours
        );
        card.update(
            "user",
            true,
            TaskEdit {
                acceptance: Some("SSO and password"),
                assignee: Some(Some(a.clone())),
                ..edit()
            },
            Utc::now(),
        )
        .unwrap();
        assert_eq!(
            (card.title.as_str(), card.assignee.as_ref()),
            ("Fix login properly", Some(&a))
        );
        assert!(
            card.update("user", true, edit(), Utc::now())
                .unwrap()
                .is_empty(),
            "nothing said, nothing changed"
        );
        assert_eq!(
            card.archive("b", false, Utc::now()).unwrap_err(),
            TaskError::NotYours
        );
        card.move_to("a", false, &lane("done"), Utc::now()).unwrap();
        card.archive("a", false, Utc::now()).unwrap();
        assert!(card.archived_at.is_some());
        assert_eq!(
            card.move_to("user", true, &lane("ready"), Utc::now())
                .unwrap_err(),
            TaskError::Archived
        );
        assert_eq!(
            card.pull(&b, &board, false, Utc::now()).unwrap_err(),
            TaskError::Archived
        );
        assert_eq!(
            card.unarchive(false, Utc::now()).unwrap_err(),
            TaskError::NotYours
        );
        card.unarchive(true, Utc::now()).unwrap();
        assert!(card.archived_at.is_none() && card.column == Column::Done);
    }

    /// A card cannot wait on itself, directly or around a loop.
    #[test]
    fn blockers_and_parents_never_close_a_loop() {
        let mut card = ready_card();
        let own = card.id.clone();
        assert_eq!(
            card.update(
                "user",
                true,
                TaskEdit {
                    blocked_by: Some(vec![own.clone()]),
                    ..edit()
                },
                Utc::now()
            )
            .unwrap_err(),
            TaskError::Cycle
        );
        // a waits on b, b on c: c waiting on a closes the loop.
        let (a, b, c) = (
            TaskId::from("a".to_owned()),
            TaskId::from("b".to_owned()),
            TaskId::from("c".to_owned()),
        );
        let edges = |id: &TaskId| match id.as_str() {
            "a" => vec![b.clone()],
            "b" => vec![c.clone()],
            _ => Vec::new(),
        };
        assert!(closes_cycle(&c, std::slice::from_ref(&a), edges));
        assert!(!closes_cycle(&a, std::slice::from_ref(&c), edges));
    }

    /// Comments are their author's to edit, theirs or the person's to
    /// delete, and keep their place once deleted.
    #[test]
    fn comments_are_bounded_and_owned() {
        let card = ready_card();
        assert!(Comment::new(&card, "a", "  ", None, Utc::now()).is_err());
        let mut comment = Comment::new(&card, "a", "Tests pass", None, Utc::now()).unwrap();
        assert_eq!(
            comment.edit("b", "x", Utc::now()).unwrap_err(),
            TaskError::NotYours
        );
        comment.edit("a", "Tests pass on CI", Utc::now()).unwrap();
        assert!(comment.edited_at.is_some());
        assert_eq!(
            comment.delete("b", false, Utc::now()).unwrap_err(),
            TaskError::NotYours
        );
        comment.delete("user", true, Utc::now()).unwrap();
        assert!(comment.body.is_empty() && comment.deleted_at.is_some());
        assert!(comment.edit("a", "again", Utc::now()).is_err());
    }
}
