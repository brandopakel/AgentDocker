//! A board of work beside the sessions: cards with what done means, in
//! columns, pulled by agents one at a time. The person files work here
//! without opening a terminal; an agent pulls a card and nobody else can
//! take it; the board says where everything is. The daemon's single
//! mutex is what makes a pull atomic; this module is the rules.
use crate::{AgentId, ProjectId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// What a title or acceptance text may be, so a card is a card and not a
/// document.
pub const TITLE_CHARS: usize = 200;
pub const ACCEPTANCE_CHARS: usize = 4000;

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

/// Where a card sits. Backlog is the person's to fill; Ready is what an
/// agent may pull; the rest is the work moving; Done is terminal until
/// somebody reopens it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Column {
    Backlog,
    Ready,
    InProgress,
    Review,
    Done,
}

impl Column {
    pub const ALL: [Column; 5] = [
        Column::Backlog,
        Column::Ready,
        Column::InProgress,
        Column::Review,
        Column::Done,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Column::Backlog => "backlog",
            Column::Ready => "ready",
            Column::InProgress => "in_progress",
            Column::Review => "review",
            Column::Done => "done",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Column::Backlog => "Backlog",
            Column::Ready => "Ready",
            Column::InProgress => "In progress",
            Column::Review => "Review",
            Column::Done => "Done",
        }
    }

    /// `backlog`, `ready`, `in-progress`/`in_progress`/`doing`, `review`,
    /// `done`, whatever the case.
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_lowercase().replace('-', "_").as_str() {
            "backlog" => Some(Column::Backlog),
            "ready" | "todo" | "approved" => Some(Column::Ready),
            "in_progress" | "doing" | "progress" => Some(Column::InProgress),
            "review" | "in_review" => Some(Column::Review),
            "done" => Some(Column::Done),
            _ => None,
        }
    }
}

impl std::fmt::Display for Column {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// One card.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    pub id: TaskId,
    pub project: ProjectId,
    pub title: String,
    /// What done means. An agent reads this before moving the card.
    #[serde(default)]
    pub acceptance: String,
    pub column: Column,
    /// Who holds it: set by a pull, or by the person.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<AgentId>,
    /// Who filed it, as an agent id or `user`.
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Off the board, kept for the record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<DateTime<Utc>>,
}

/// Why a change to a card is refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskError {
    /// A title that is empty or too long, or acceptance text too long.
    Invalid(&'static str),
    /// A pull of a card somebody already holds, or that is not Ready.
    Taken { assignee: Option<AgentId>, column: Column },
    /// A move by somebody who is neither its assignee nor the person.
    NotYours,
    /// A card that is archived.
    Archived,
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
            TaskError::NotYours => f.write_str("only the card's assignee or the person moves it"),
            TaskError::Archived => f.write_str("the card is archived"),
        }
    }
}

fn valid_texts(title: &str, acceptance: &str) -> Result<(), TaskError> {
    let title = title.trim();
    if title.is_empty() {
        return Err(TaskError::Invalid("a card needs a title"));
    }
    if title.chars().count() > TITLE_CHARS {
        return Err(TaskError::Invalid("a card's title is at most 200 characters"));
    }
    if acceptance.chars().count() > ACCEPTANCE_CHARS {
        return Err(TaskError::Invalid(
            "a card's acceptance text is at most 4000 characters",
        ));
    }
    Ok(())
}

impl Task {
    /// A new card in a column of the person's choosing (Backlog when
    /// none), filed by `created_by`.
    pub fn new(
        project: ProjectId,
        title: &str,
        acceptance: &str,
        column: Option<Column>,
        created_by: &str,
        now: DateTime<Utc>,
    ) -> Result<Self, TaskError> {
        valid_texts(title, acceptance)?;
        Ok(Self {
            id: TaskId::generate(),
            project,
            title: title.trim().to_owned(),
            acceptance: acceptance.trim().to_owned(),
            column: column.unwrap_or(Column::Backlog),
            assignee: None,
            created_by: created_by.to_owned(),
            created_at: now,
            updated_at: now,
            archived_at: None,
        })
    }

    /// An agent takes a Ready card nobody holds: it becomes theirs, in
    /// progress. Anything else is refused, which is the whole point.
    pub fn pull(&mut self, agent: &AgentId, now: DateTime<Utc>) -> Result<(), TaskError> {
        if self.archived_at.is_some() {
            return Err(TaskError::Archived);
        }
        if self.column != Column::Ready || self.assignee.is_some() {
            return Err(TaskError::Taken {
                assignee: self.assignee.clone(),
                column: self.column,
            });
        }
        self.assignee = Some(agent.clone());
        self.column = Column::InProgress;
        self.updated_at = now;
        Ok(())
    }

    /// Move the card: its assignee may, and the person always may. A card
    /// moved back to Backlog or Ready by the person is released for the
    /// next taker; one moved to Done by its assignee stays theirs, for the
    /// record.
    pub fn move_to(
        &mut self,
        by: &str,
        by_is_human: bool,
        column: Column,
        now: DateTime<Utc>,
    ) -> Result<(), TaskError> {
        if self.archived_at.is_some() {
            return Err(TaskError::Archived);
        }
        if !by_is_human && self.assignee.as_ref().is_none_or(|a| a.as_str() != by) {
            return Err(TaskError::NotYours);
        }
        if column == self.column {
            return Ok(());
        }
        self.column = column;
        if by_is_human && matches!(column, Column::Backlog | Column::Ready) {
            self.assignee = None;
        }
        self.updated_at = now;
        Ok(())
    }

    /// The person edits what the card says, or hands it to somebody
    /// (`assignee: Some(None)` takes it away). An agent may only edit a
    /// card it holds, and cannot reassign it.
    pub fn update(
        &mut self,
        by: &str,
        by_is_human: bool,
        title: Option<&str>,
        acceptance: Option<&str>,
        assignee: Option<Option<AgentId>>,
        now: DateTime<Utc>,
    ) -> Result<(), TaskError> {
        if self.archived_at.is_some() {
            return Err(TaskError::Archived);
        }
        if !by_is_human
            && (assignee.is_some() || self.assignee.as_ref().is_none_or(|a| a.as_str() != by))
        {
            return Err(TaskError::NotYours);
        }
        valid_texts(
            title.unwrap_or(&self.title),
            acceptance.unwrap_or(&self.acceptance),
        )?;
        if let Some(title) = title {
            self.title = title.trim().to_owned();
        }
        if let Some(acceptance) = acceptance {
            self.acceptance = acceptance.trim().to_owned();
        }
        if let Some(assignee) = assignee {
            self.assignee = assignee;
        }
        self.updated_at = now;
        Ok(())
    }

    /// Off the board. The person's to do, or the assignee's for a card in
    /// Done.
    pub fn archive(&mut self, by: &str, by_is_human: bool, now: DateTime<Utc>) -> Result<(), TaskError> {
        if self.archived_at.is_some() {
            return Ok(());
        }
        let theirs_done =
            self.column == Column::Done && self.assignee.as_ref().is_some_and(|a| a.as_str() == by);
        if !by_is_human && !theirs_done {
            return Err(TaskError::NotYours);
        }
        self.archived_at = Some(now);
        self.updated_at = now;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card() -> Task {
        Task::new(
            ProjectId::from("p"),
            "Fix login",
            "Login works with SSO",
            Some(Column::Ready),
            "user",
            Utc::now(),
        )
        .unwrap()
    }

    /// A card is a title and what done means, within bounds.
    #[test]
    fn a_card_is_a_bounded_title_and_acceptance() {
        assert_eq!(
            Task::new(ProjectId::from("p"), "  ", "", None, "user", Utc::now()).unwrap_err(),
            TaskError::Invalid("a card needs a title")
        );
        assert!(Task::new(ProjectId::from("p"), &"x".repeat(201), "", None, "user", Utc::now()).is_err());
        assert!(Task::new(ProjectId::from("p"), "t", &"x".repeat(4001), None, "user", Utc::now()).is_err());
        let card = Task::new(ProjectId::from("p"), " Fix login ", " when ", None, "user", Utc::now()).unwrap();
        assert_eq!((card.title.as_str(), card.acceptance.as_str(), card.column), ("Fix login", "when", Column::Backlog));
        assert_eq!(Column::parse("In-Progress"), Some(Column::InProgress));
        assert_eq!(Column::parse("todo"), Some(Column::Ready));
        assert_eq!(Column::parse("elsewhere"), None);
    }

    /// One pull, from Ready, by the first taker; the second is told who
    /// holds it. A Backlog card is not ready to pull.
    #[test]
    fn a_pull_takes_a_ready_card_once() {
        let mut card = card();
        let a = AgentId::from("a".to_owned());
        let b = AgentId::from("b".to_owned());
        card.pull(&a, Utc::now()).unwrap();
        assert_eq!((card.assignee.as_ref(), card.column), (Some(&a), Column::InProgress));
        assert_eq!(
            card.pull(&b, Utc::now()).unwrap_err(),
            TaskError::Taken { assignee: Some(a.clone()), column: Column::InProgress }
        );
        let mut backlog = card();
        backlog.column = Column::Backlog;
        assert_eq!(
            backlog.pull(&b, Utc::now()).unwrap_err(),
            TaskError::Taken { assignee: None, column: Column::Backlog }
        );
    }

    /// The assignee moves its own card and nobody else's; the person
    /// moves any, and moving one back to Ready or Backlog releases it.
    #[test]
    fn moves_are_the_assignees_or_the_persons() {
        let mut card = card();
        let a = AgentId::from("a".to_owned());
        card.pull(&a, Utc::now()).unwrap();
        assert_eq!(card.move_to("b", false, Column::Review, Utc::now()).unwrap_err(), TaskError::NotYours);
        card.move_to("a", false, Column::Review, Utc::now()).unwrap();
        assert_eq!(card.column, Column::Review);
        card.move_to("a", false, Column::Done, Utc::now()).unwrap();
        assert_eq!(card.assignee.as_ref(), Some(&a), "done stays theirs, for the record");
        card.move_to("user", true, Column::Ready, Utc::now()).unwrap();
        assert_eq!((card.assignee.as_ref(), card.column), (None, Column::Ready), "released for the next taker");
        let b = AgentId::from("b".to_owned());
        card.pull(&b, Utc::now()).unwrap();
        // Edits: the holder edits words, the person reassigns; an archived
        // card is done with.
        assert_eq!(card.update("a", false, Some("x"), None, None, Utc::now()).unwrap_err(), TaskError::NotYours);
        card.update("b", false, Some("Fix login properly"), None, None, Utc::now()).unwrap();
        assert_eq!(card.update("b", false, None, None, Some(None), Utc::now()).unwrap_err(), TaskError::NotYours);
        card.update("user", true, None, Some("SSO and password"), Some(Some(a.clone())), Utc::now()).unwrap();
        assert_eq!((card.title.as_str(), card.assignee.as_ref()), ("Fix login properly", Some(&a)));
        assert_eq!(card.archive("b", false, Utc::now()).unwrap_err(), TaskError::NotYours);
        card.move_to("a", false, Column::Done, Utc::now()).unwrap();
        card.archive("a", false, Utc::now()).unwrap();
        assert!(card.archived_at.is_some());
        assert_eq!(card.move_to("user", true, Column::Ready, Utc::now()).unwrap_err(), TaskError::Archived);
        assert_eq!(card.pull(&b, Utc::now()).unwrap_err(), TaskError::Archived);
    }
}
