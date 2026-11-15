//! Rules on a board: when a card comes into a lane, or has sat in one for
//! some days, comment on it, move it, archive it, or start an agent on it.
//! What a rule does is the daemon's; whether and when it acts is here.
use crate::board::{Board, BoardError, NAME_CHARS};
use crate::task::{COMMENT_CHARS, Task};
use crate::{Column, ProjectId, TaskId};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

/// At most this many rules on one board.
pub const RULES_MAX: usize = 32;
/// An idle rule waits at least a day and at most a year.
pub const IDLE_DAYS_MAX: u32 = 365;
/// At most this many agents one rule has started run at once.
pub const START_MAX: u32 = 8;
/// A rule-started agent's arguments: at most this many.
pub const START_ARGS_MAX: usize = 32;

/// When a rule acts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Trigger {
    /// A card comes into the lane, by any move but a rule's.
    Enter { lane: String },
    /// A card has been in the lane this many days.
    Idle { lane: String, days: u32 },
}

impl Trigger {
    pub fn lane(&self) -> &str {
        match self {
            Trigger::Enter { lane } | Trigger::Idle { lane, .. } => lane,
        }
    }
}

/// What a rule does to the card.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RuleAction {
    /// Say something on it.
    Comment { text: String },
    /// Move it to another lane.
    Move { lane: String },
    /// Take it off the board.
    Archive,
    /// Start an agent of this runtime in the project, hand it the card
    /// and tell it to work on it — no more than `max` of this rule's
    /// agents at once.
    Start {
        runtime: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        args: Vec<String>,
        #[serde(default = "one")]
        max: u32,
    },
}

fn one() -> u32 {
    1
}

/// One rule of a board.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    pub id: String,
    pub project: ProjectId,
    pub board: String,
    pub name: String,
    pub trigger: Trigger,
    pub action: RuleAction,
    pub enabled: bool,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Rule {
    pub fn new(
        project: ProjectId,
        board: &Board,
        name: &str,
        trigger: Trigger,
        action: RuleAction,
        created_by: &str,
        now: DateTime<Utc>,
    ) -> Result<Self, BoardError> {
        let rule = Self {
            id: uuid::Uuid::new_v4().simple().to_string()[..12].to_owned(),
            project,
            board: board.slug.clone(),
            name: name.trim().to_owned(),
            trigger,
            action,
            enabled: true,
            created_by: created_by.to_owned(),
            created_at: now,
            updated_at: now,
        };
        rule.check(board)?;
        Ok(rule)
    }

    /// Its words and lanes within bounds, on `board`.
    pub fn check(&self, board: &Board) -> Result<(), BoardError> {
        let invalid = |reason: &str| Err(BoardError::Invalid(reason.to_owned()));
        let name = self.name.trim();
        if name.is_empty() || name.chars().count() > NAME_CHARS {
            return invalid("a rule's name is one to forty characters");
        }
        let lane = board
            .lane(self.trigger.lane())
            .ok_or_else(|| BoardError::NoLane {
                asked: self.trigger.lane().to_owned(),
                lanes: board.lanes.iter().map(|l| l.slug.clone()).collect(),
            })?;
        if let Trigger::Idle { days, .. } = self.trigger
            && !(1..=IDLE_DAYS_MAX).contains(&days)
        {
            return invalid("an idle rule waits one to 365 days");
        }
        match &self.action {
            RuleAction::Comment { text } => {
                let count = text.trim().chars().count();
                if count == 0 || count > COMMENT_CHARS {
                    return invalid("a rule's comment is one to 4000 characters");
                }
            }
            RuleAction::Move { lane: to } => {
                let to = board.lane(to).ok_or_else(|| BoardError::NoLane {
                    asked: to.clone(),
                    lanes: board.lanes.iter().map(|l| l.slug.clone()).collect(),
                })?;
                if to.slug == lane.slug {
                    return invalid("a rule moves a card to another lane");
                }
            }
            RuleAction::Archive => {}
            RuleAction::Start { runtime, args, max } => {
                if runtime.trim().is_empty()
                    || !runtime
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
                {
                    return invalid("a rule starts a runtime by its name, as `runtimes` lists it");
                }
                if args.len() > START_ARGS_MAX || args.iter().any(|a| a.len() > 4096) {
                    return invalid("a rule's agent takes at most 32 arguments");
                }
                if !(1..=START_MAX).contains(max) {
                    return invalid("a rule runs one to eight agents at once");
                }
                if lane.category.terminal() {
                    return invalid("a rule starts agents on cards still to do, not on ended ones");
                }
            }
        }
        Ok(())
    }

    /// Whether this rule acts on a card that just came into `lane` by a
    /// move that was not a rule's.
    pub fn on_enter(&self, task: &Task, lane: &str) -> bool {
        self.enabled
            && task.archived_at.is_none()
            && task.board.as_deref().unwrap_or(crate::board::MAIN) == self.board
            && matches!(&self.trigger, Trigger::Enter { lane: l } if l == lane)
            && !task.fired.contains(&self.id)
    }

    /// Whether this idle rule is due on a card, which sits in `lane` on
    /// this rule's board: in the lane for its days, and not yet acted on
    /// this stay.
    pub fn idle_due(&self, task: &Task, lane: &str, now: DateTime<Utc>) -> bool {
        let Trigger::Idle { lane: l, days } = &self.trigger else {
            return false;
        };
        self.enabled
            && task.archived_at.is_none()
            && task.board.as_deref().unwrap_or(crate::board::MAIN) == self.board
            && l == lane
            && !task.fired.contains(&self.id)
            && now - task.in_lane_since() >= Duration::days(i64::from(*days))
    }

    /// What it says, in a line: `Testing, 14 days → comment`.
    pub fn summary(&self, board: &Board) -> String {
        let name = |slug: &str| {
            board
                .lane(slug)
                .map_or_else(|| slug.to_owned(), |l| l.name.clone())
        };
        let when = match &self.trigger {
            Trigger::Enter { lane } => format!("into {}", name(lane)),
            Trigger::Idle { lane, days } => {
                format!(
                    "{} for {days} day{}",
                    name(lane),
                    if *days == 1 { "" } else { "s" }
                )
            }
        };
        let what = match &self.action {
            RuleAction::Comment { .. } => "comment".to_owned(),
            RuleAction::Move { lane } => format!("move to {}", name(lane)),
            RuleAction::Archive => "archive".to_owned(),
            RuleAction::Start { runtime, .. } => format!("start {runtime}"),
        };
        format!("{when} → {what}")
    }
}

/// The label a rule-started agent carries, naming its rule: how the
/// daemon counts them against `max`.
pub const STARTED_BY_LABEL: &str = "agentdocker.rule";

/// What a rule-started agent is told first: the card, and what to do
/// with it.
pub fn start_prompt(task: &Task) -> String {
    let mut prompt = format!(
        "Work on card {}: {}. It is yours: read it with `get_task {}`, comment on it as you go, and move it to review when its acceptance text is met.",
        task.handle(),
        task.title,
        task.handle()
    );
    if !task.acceptance.trim().is_empty() {
        prompt.push_str(&format!("\n\nDone when: {}", task.acceptance.trim()));
    }
    prompt
}

/// A card an agent may be started on: one nobody holds yet, still to
/// do, and waiting on nothing — no open card it is blocked by
/// (`open_blocker` says which are), nothing off the board. What a pull
/// asks of a card, so a start never takes one a pull would refuse.
pub fn startable(task: &Task, open_blocker: impl Fn(&TaskId) -> bool) -> bool {
    task.assignee.is_none()
        && task.archived_at.is_none()
        && !matches!(task.column, Column::Done | Column::Cancelled)
        && task.external_blocker.trim().is_empty()
        && !task.blocked_by.iter().any(open_blocker)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::NewTask;

    fn board() -> Board {
        Board::main(ProjectId::from("p"), "p", Utc::now())
    }

    fn card(lane: &str, since: DateTime<Utc>) -> Task {
        let mut task = Task::file(
            ProjectId::from("p"),
            NewTask {
                title: "Fix".into(),
                ..Default::default()
            },
            "user",
            since,
        )
        .unwrap();
        task.lane = Some(lane.into());
        task.lane_since = Some(since);
        task
    }

    #[test]
    fn a_rule_is_checked_against_its_board() {
        let board = board();
        let now = Utc::now();
        let rule = |trigger, action| {
            Rule::new(
                ProjectId::from("p"),
                &board,
                "Nudge",
                trigger,
                action,
                "user",
                now,
            )
        };
        let enter = |lane: &str| Trigger::Enter { lane: lane.into() };
        assert!(rule(enter("review"), RuleAction::Archive).is_ok());
        assert!(matches!(
            rule(enter("nowhere"), RuleAction::Archive),
            Err(BoardError::NoLane { .. })
        ));
        assert!(
            rule(
                enter("review"),
                RuleAction::Move {
                    lane: "review".into()
                }
            )
            .is_err(),
            "a move to where it is"
        );
        assert!(
            rule(
                Trigger::Idle {
                    lane: "review".into(),
                    days: 0
                },
                RuleAction::Archive
            )
            .is_err()
        );
        assert!(rule(enter("ready"), RuleAction::Comment { text: "  ".into() }).is_err());
        let start = |max| RuleAction::Start {
            runtime: "codex".into(),
            args: Vec::new(),
            max,
        };
        assert!(rule(enter("ready"), start(1)).is_ok());
        assert!(rule(enter("ready"), start(0)).is_err());
        assert!(rule(enter("done"), start(1)).is_err(), "not on ended cards");
        assert!(
            rule(
                enter("ready"),
                RuleAction::Start {
                    runtime: "codex; rm".into(),
                    args: Vec::new(),
                    max: 1
                }
            )
            .is_err()
        );
    }

    #[test]
    fn an_idle_rule_is_due_once_a_stay() {
        let board = board();
        let now = Utc::now();
        let rule = Rule::new(
            ProjectId::from("p"),
            &board,
            "Stale review",
            Trigger::Idle {
                lane: "review".into(),
                days: 14,
            },
            RuleAction::Comment {
                text: "Still waiting".into(),
            },
            "user",
            now,
        )
        .unwrap();
        let mut old = card("review", now - Duration::days(15));
        assert!(rule.idle_due(&old, "review", now));
        assert!(!rule.idle_due(&card("review", now - Duration::days(3)), "review", now));
        assert!(!rule.idle_due(&old, "ready", now), "another lane");
        old.fired.push(rule.id.clone());
        assert!(!rule.idle_due(&old, "review", now), "acted on this stay");
        let mut off = rule.clone();
        off.enabled = false;
        assert!(!off.idle_due(&card("review", now - Duration::days(30)), "review", now));
        assert_eq!(rule.summary(&board), "Review for 14 days → comment");
    }

    #[test]
    fn an_enter_rule_acts_on_its_own_board_once() {
        let board = board();
        let now = Utc::now();
        let rule = Rule::new(
            ProjectId::from("p"),
            &board,
            "Start",
            Trigger::Enter {
                lane: "ready".into(),
            },
            RuleAction::Start {
                runtime: "codex".into(),
                args: Vec::new(),
                max: 2,
            },
            "user",
            now,
        )
        .unwrap();
        let mut task = card("ready", now);
        assert!(rule.on_enter(&task, "ready"));
        assert!(!rule.on_enter(&task, "review"));
        task.board = Some("sprint".into());
        assert!(!rule.on_enter(&task, "ready"), "another board");
        assert!(startable(&card("ready", now), |_| false));
        assert!(start_prompt(&card("ready", now)).contains("Work on card"));
    }

    #[test]
    fn a_card_that_waits_is_not_startable() {
        let now = Utc::now();
        let blocker = card("in_progress", now);
        let mut waiting = card("ready", now);
        waiting.blocked_by.push(blocker.id.clone());
        assert!(
            !startable(&waiting, |id| id == &blocker.id),
            "an open blocker"
        );
        assert!(startable(&waiting, |_| false), "its blocker ended");
        waiting.external_blocker = "the vendor's sign-off".into();
        assert!(!startable(&waiting, |_| false), "something off the board");
        waiting.external_blocker = "  ".into();
        assert!(startable(&waiting, |_| false));
        let mut held = card("ready", now);
        held.assignee = Some("codex".into());
        assert!(!startable(&held, |_| false));
    }
}
