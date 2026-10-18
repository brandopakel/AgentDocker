//! A project's board as data: named lanes, each standing for one of the
//! card categories (`Column`), with who is meant to work there, how many
//! cards it should hold, and which lane agents pull from and into.
//!
//! The rules about cards stay in `task`, by category: a pull takes a card
//! from the pull source to the pull target, the working categories are In
//! progress and Review, Done and Cancelled end a card. A board only names
//! the lanes those categories are drawn as, so the five-lane board every
//! project had keeps its meaning, and a nine-lane agent workflow is the
//! same rules with more lanes. A card names its lane by slug; one that
//! names none, or a lane the board no longer has, sits in the first lane
//! of its category.
use crate::task::{Column, Task};
use crate::{AgentId, ProjectId, TaskId};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

/// At most this many lanes on one board.
pub const LANES_MAX: usize = 16;
/// A lane's slug: lowercase letters, digits, `-` and `_`, at most this.
pub const SLUG_CHARS: usize = 32;
/// A lane's or a board's name, at most this.
pub const NAME_CHARS: usize = 40;
/// The board every project has, made from `classic` on first use.
pub const MAIN: &str = "main";

/// Who a lane is meant for. A hint, never a lock: the person moves any
/// card anywhere, and an agent pulls from the pull source whatever it
/// says; the hint is where each is told to look.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Owner {
    #[default]
    Any,
    Human,
    Agent,
    /// Agents with this role label (`role=<name>`).
    Role(String),
}

impl Owner {
    pub fn word(&self) -> String {
        match self {
            Owner::Any => "anyone".to_owned(),
            Owner::Human => "the person".to_owned(),
            Owner::Agent => "agents".to_owned(),
            Owner::Role(role) => format!("role {role}"),
        }
    }
}

/// One lane of a board.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lane {
    pub slug: String,
    pub name: String,
    pub category: Column,
    #[serde(default, skip_serializing_if = "is_any")]
    pub owner: Owner,
    /// How many cards it should hold; past it the lane says so. Nothing
    /// is refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wip: Option<u16>,
}

fn is_any(owner: &Owner) -> bool {
    *owner == Owner::Any
}

impl Lane {
    pub fn new(slug: &str, name: &str, category: Column, owner: Owner) -> Self {
        Self {
            slug: slug.to_owned(),
            name: name.to_owned(),
            category,
            owner,
            wip: None,
        }
    }

    /// Over its limit with `count` cards in it.
    pub fn over(&self, count: usize) -> bool {
        self.wip
            .is_some_and(|wip| wip > 0 && count > usize::from(wip))
    }
}

/// A project's board.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Board {
    pub project: ProjectId,
    pub slug: String,
    pub name: String,
    /// The template it was made from: `classic`, `agents` or `simple`.
    pub template: String,
    pub lanes: Vec<Lane>,
    /// The lane agents pull from (a Ready lane) and the one a pull puts
    /// the card in (an In progress lane).
    pub pull_source: String,
    pub pull_target: String,
    /// The display key's prefix, `AD` in `AD-12`, and the last number
    /// handed out.
    pub key_prefix: String,
    #[serde(default)]
    pub last_key: u64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Why a board change is refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BoardError {
    Invalid(String),
    /// No lane answers to these words; the board's lanes are listed.
    NoLane {
        asked: String,
        lanes: Vec<String>,
    },
}

impl std::fmt::Display for BoardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BoardError::Invalid(reason) => f.write_str(reason),
            BoardError::NoLane { asked, lanes } => write!(
                f,
                "no lane is called {asked}; the board's lanes are {}",
                lanes.join(", ")
            ),
        }
    }
}

/// The templates a board is made from.
pub const TEMPLATES: [&str; 3] = ["classic", "agents", "simple"];

impl Board {
    /// A board from a template: `classic` (Backlog, Ready, In progress,
    /// Review, Done: the lanes every project had), `agents` (a workflow
    /// for several agents with the person at both ends: Backlog,
    /// Approved, Analyst, In progress, Testing, UAT, Done, Blocked,
    /// Cancelled) or `simple` (To do, Doing, Done).
    pub fn from_template(
        project: ProjectId,
        slug: &str,
        name: &str,
        template: &str,
        key_prefix: &str,
        now: DateTime<Utc>,
    ) -> Result<Self, BoardError> {
        use Column::*;
        let lane = |slug: &str, name: &str, category, owner| Lane::new(slug, name, category, owner);
        let (lanes, source, target) = match template {
            "classic" => (
                vec![
                    lane("backlog", "Backlog", Backlog, Owner::Human),
                    lane("ready", "Ready", Ready, Owner::Agent),
                    lane("in_progress", "In progress", InProgress, Owner::Agent),
                    lane("review", "Review", Review, Owner::Human),
                    lane("done", "Done", Done, Owner::Any),
                ],
                "ready",
                "in_progress",
            ),
            "agents" => (
                vec![
                    lane("backlog", "Backlog", Backlog, Owner::Human),
                    lane("approved", "Approved", Ready, Owner::Agent),
                    lane(
                        "analyst",
                        "Analyst",
                        InProgress,
                        Owner::Role("analyst".into()),
                    ),
                    lane("in_progress", "In progress", InProgress, Owner::Agent),
                    lane("testing", "Testing", Review, Owner::Role("tester".into())),
                    lane("uat", "UAT", Review, Owner::Human),
                    lane("done", "Done", Done, Owner::Human),
                    lane("blocked", "Blocked", Blocked, Owner::Any),
                    lane("cancelled", "Cancelled", Cancelled, Owner::Human),
                ],
                "approved",
                "analyst",
            ),
            "simple" => (
                vec![
                    lane("ready", "To do", Ready, Owner::Any),
                    lane("in_progress", "Doing", InProgress, Owner::Any),
                    lane("done", "Done", Done, Owner::Any),
                ],
                "ready",
                "in_progress",
            ),
            other => {
                return Err(BoardError::Invalid(format!(
                    "no template is called {other}; there are {}",
                    TEMPLATES.join(", ")
                )));
            }
        };
        let board = Self {
            project,
            slug: slug.to_owned(),
            name: name.trim().to_owned(),
            template: template.to_owned(),
            lanes,
            pull_source: source.to_owned(),
            pull_target: target.to_owned(),
            key_prefix: key_prefix.to_owned(),
            last_key: 0,
            created_at: now,
            updated_at: now,
        };
        board.check()?;
        Ok(board)
    }

    /// The board a project has before anybody shapes it: the classic
    /// lanes, called main.
    pub fn main(project: ProjectId, project_name: &str, now: DateTime<Utc>) -> Self {
        Self::from_template(
            project,
            MAIN,
            "Main",
            "classic",
            &key_prefix(project_name),
            now,
        )
        .expect("the classic template is valid")
    }

    /// Lanes that make sense together: some, not too many, each slug once
    /// and well formed, each name short; a pull source that is a Ready
    /// lane and a pull target that is an In progress lane; somewhere for
    /// a card to be done.
    pub fn check(&self) -> Result<(), BoardError> {
        let invalid = |reason: &str| Err(BoardError::Invalid(reason.to_owned()));
        if self.name.is_empty() || self.name.chars().count() > NAME_CHARS {
            return invalid("a board's name is one to forty characters");
        }
        if !valid_slug(&self.slug) {
            return invalid("a board's slug is lowercase letters, digits, - and _");
        }
        if self.lanes.is_empty() || self.lanes.len() > LANES_MAX {
            return invalid("a board has one to sixteen lanes");
        }
        let mut seen = std::collections::BTreeSet::new();
        for lane in &self.lanes {
            if !valid_slug(&lane.slug) {
                return invalid("a lane's slug is lowercase letters, digits, - and _");
            }
            if !seen.insert(lane.slug.as_str()) {
                return invalid("two lanes have the same slug");
            }
            let name = lane.name.trim();
            if name.is_empty() || name.chars().count() > NAME_CHARS {
                return invalid("a lane's name is one to forty characters");
            }
        }
        match self.lane(&self.pull_source) {
            Some(lane) if lane.category == Column::Ready => {}
            _ => return invalid("the pull source is a Ready lane on the board"),
        }
        match self.lane(&self.pull_target) {
            Some(lane) if lane.category == Column::InProgress => {}
            _ => return invalid("the pull target is an In progress lane on the board"),
        }
        if !self.lanes.iter().any(|l| l.category == Column::Done) {
            return invalid("a board has a Done lane");
        }
        Ok(())
    }

    pub fn lane(&self, slug: &str) -> Option<&Lane> {
        self.lanes.iter().find(|lane| lane.slug == slug)
    }

    /// The first lane of a category, where a card of it with no lane of
    /// its own sits.
    pub fn first(&self, category: Column) -> Option<&Lane> {
        self.lanes.iter().find(|lane| lane.category == category)
    }

    /// The lane a card is drawn in: its own, when the board has it; else
    /// the first of its category; else the first lane, so no card is
    /// ever off the board.
    pub fn lane_of(&self, task: &Task) -> &Lane {
        task.lane
            .as_deref()
            .and_then(|slug| self.lane(slug))
            .filter(|lane| lane.category == task.column)
            .or_else(|| self.first(task.column))
            .unwrap_or(&self.lanes[0])
    }

    /// The lane some words name: its slug, its name (any case), a
    /// category (`review` is the first Review lane), or a unique prefix
    /// of three or more letters of a slug or name. Refused with the
    /// board's lanes when nothing, or more than one, answers.
    pub fn resolve(&self, words: &str) -> Result<&Lane, BoardError> {
        let asked = words.trim();
        let folded = asked.to_lowercase().replace([' ', '-'], "_");
        let names = || self.lanes.iter().map(|l| l.slug.clone()).collect();
        if let Some(lane) = self
            .lanes
            .iter()
            .find(|l| l.slug == folded || l.name.to_lowercase() == asked.to_lowercase())
        {
            return Ok(lane);
        }
        if let Some(category) = Column::parse(asked)
            && let Some(lane) = self.first(category)
        {
            return Ok(lane);
        }
        if folded.chars().count() >= 3 {
            let mut matches = self.lanes.iter().filter(|l| {
                l.slug.starts_with(&folded)
                    || l.name.to_lowercase().starts_with(&asked.to_lowercase())
            });
            if let (Some(lane), None) = (matches.next(), matches.next()) {
                return Ok(lane);
            }
        }
        Err(BoardError::NoLane {
            asked: asked.to_owned(),
            lanes: names(),
        })
    }

    /// The next display key, `AD-13`: the board's counter moves on.
    pub fn take_key(&mut self) -> String {
        self.last_key += 1;
        format!("{}-{}", self.key_prefix, self.last_key)
    }

    /// The card an agent pulls when it names none: the first in the pull
    /// source, in board order, that is free or already the caller's and
    /// is not blocked, by an outside blocker or by an open card it waits
    /// on. `open_blocker` says whether a card id names a card that is
    /// still open (neither done, cancelled nor archived).
    pub fn next_ready<'t>(
        &self,
        cards: &'t [Task],
        caller: &AgentId,
        open_blocker: impl Fn(&TaskId) -> bool,
    ) -> Pick<'t> {
        let mut pick = Pick::default();
        let mut ready: Vec<&Task> = cards
            .iter()
            .filter(|t| t.archived_at.is_none() && self.lane_of(t).slug == self.pull_source)
            .collect();
        ready.sort_by(|a, b| order(a, b));
        for card in ready {
            pick.total += 1;
            if card.assignee.as_ref().is_some_and(|a| a != caller) {
                pick.held += 1;
                continue;
            }
            if !card.external_blocker.trim().is_empty() || card.blocked_by.iter().any(&open_blocker)
            {
                pick.blocked += 1;
                continue;
            }
            if pick.card.is_none() {
                pick.card = Some(card);
            }
        }
        pick
    }
}

/// What a pull with no card named found in the pull source: the card it
/// takes, and how many were there, held by others or blocked.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pick<'t> {
    pub card: Option<&'t Task>,
    pub total: usize,
    pub held: usize,
    pub blocked: usize,
}

/// Board order within a lane: a card's rank, then when it was filed.
/// Cards ranked before ranks existed (an empty rank) come first.
pub fn order(a: &Task, b: &Task) -> std::cmp::Ordering {
    a.rank
        .cmp(&b.rank)
        .then(a.created_at.cmp(&b.created_at))
        .then(a.id.cmp(&b.id))
}

pub fn valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug.chars().count() <= SLUG_CHARS
        && slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// A board slug from a name: `Release train` is `release-train`.
pub fn slug_of(name: &str) -> String {
    let mut slug = String::new();
    for c in name.trim().to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
    }
    let slug: String = slug
        .trim_end_matches('-')
        .chars()
        .take(SLUG_CHARS)
        .collect();
    if slug.is_empty() {
        "board".to_owned()
    } else {
        slug
    }
}

/// A display-key prefix from a project's name: its words' initials, or
/// the humps of a camel-cased name (`AgentDocker` is `AD`, `river-lab`
/// is `RL`); a single word gives its first two letters (`orchard` is
/// `OR`). Letters only, two to four of them.
pub fn key_prefix(name: &str) -> String {
    let mut initials = String::new();
    let mut previous: Option<char> = None;
    for c in name.chars() {
        let starts = match previous {
            None => true,
            Some(p) => {
                !p.is_ascii_alphanumeric() || (p.is_ascii_lowercase() && c.is_ascii_uppercase())
            }
        };
        if c.is_ascii_alphabetic() && starts {
            initials.push(c.to_ascii_uppercase());
        }
        previous = Some(c);
    }
    if initials.chars().count() >= 2 {
        return initials.chars().take(4).collect();
    }
    let letters: String = name
        .chars()
        .filter(|c| c.is_ascii_alphabetic())
        .take(2)
        .collect::<String>()
        .to_ascii_uppercase();
    if letters.is_empty() {
        "T".to_owned()
    } else {
        letters
    }
}

/// Where a due date stands on a given day.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DueState {
    Overdue,
    Today,
    Later,
}

pub fn due_state(due: NaiveDate, today: NaiveDate) -> DueState {
    match due.cmp(&today) {
        std::cmp::Ordering::Less => DueState::Overdue,
        std::cmp::Ordering::Equal => DueState::Today,
        std::cmp::Ordering::Greater => DueState::Later,
    }
}

/// Ranks: strings that sort between any two, so a card moves between
/// two others without renumbering the lane. Digits and lowercase
/// letters, compared as bytes; the empty string is before everything and
/// no rank ends in `0`, so there is always room between two.
pub mod rank {
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    const BASE: usize = 36;

    fn index(byte: u8) -> usize {
        DIGITS.iter().position(|d| *d == byte).unwrap_or(0)
    }

    /// A rank after `before` and before `after` (either may be absent);
    /// strictly between them when `before < after`.
    pub fn between(before: Option<&str>, after: Option<&str>) -> String {
        let low = before.unwrap_or("").as_bytes();
        let high = after.map(str::as_bytes).filter(|h| !h.is_empty());
        String::from_utf8(midpoint(low, high)).expect("ascii digits")
    }

    fn midpoint(low: &[u8], high: Option<&[u8]>) -> Vec<u8> {
        if let Some(high) = high {
            // The common prefix, reading a short low as padded with 0.
            let mut n = 0;
            while n < high.len() && low.get(n).copied().unwrap_or(b'0') == high[n] {
                n += 1;
            }
            if n > 0 {
                let mut out = high[..n].to_vec();
                let rest_low = if n < low.len() { &low[n..] } else { &[][..] };
                out.extend(midpoint(
                    rest_low,
                    Some(&high[n..]).filter(|h| !h.is_empty()),
                ));
                return out;
            }
        }
        let a = low.first().map_or(0, |b| index(*b));
        let b = high.map_or(BASE, |h| index(h[0]));
        if b > a + 1 {
            return vec![DIGITS[(a + b) / 2]];
        }
        // Consecutive digits: a longer high's first digit is between; else
        // keep low's digit and find room after it.
        if let Some(high) = high
            && high.len() > 1
        {
            return vec![high[0]];
        }
        let mut out = vec![DIGITS[a]];
        out.extend(midpoint(
            if low.len() > 1 { &low[1..] } else { &[][..] },
            None,
        ));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::{NewTask, Task};

    fn now() -> DateTime<Utc> {
        Utc::now()
    }

    fn card(board: &Board, title: &str, lane: &str) -> Task {
        let lane = board.lane(lane).unwrap();
        let mut task = Task::file(
            ProjectId::from("p"),
            NewTask {
                title: title.into(),
                column: Some(lane.category),
                lane: Some(lane.slug.clone()),
                ..NewTask::default()
            },
            "user",
            now(),
        )
        .unwrap();
        task.rank = rank::between(None, None);
        task
    }

    /// The templates make valid boards with their pull lanes where the
    /// rules need them; a classic board is the five lanes cards always
    /// had, so an old card lands where it was.
    #[test]
    fn the_templates_are_valid_and_classic_is_the_old_board() {
        for template in TEMPLATES {
            Board::from_template(ProjectId::from("p"), "b", "B", template, "P", now()).unwrap();
        }
        let main = Board::main(ProjectId::from("p"), "AgentDocker", now());
        let slugs: Vec<&str> = main.lanes.iter().map(|l| l.slug.as_str()).collect();
        assert_eq!(slugs, ["backlog", "ready", "in_progress", "review", "done"]);
        for (lane, column) in main.lanes.iter().zip(Column::ALL) {
            assert_eq!(lane.category, column);
            assert_eq!(lane.slug, column.as_str());
        }
        assert_eq!(main.key_prefix, "AD");
        assert!(
            Board::from_template(ProjectId::from("p"), "b", "B", "kanban", "P", now()).is_err()
        );
    }

    #[test]
    fn a_board_refuses_lanes_that_break_the_rules() {
        let mut board = Board::main(ProjectId::from("p"), "p", now());
        board.pull_source = "backlog".into();
        assert!(board.check().is_err(), "a Backlog lane is no pull source");
        let mut board = Board::main(ProjectId::from("p"), "p", now());
        board.lanes.retain(|l| l.category != Column::Done);
        assert!(board.check().is_err(), "nowhere to be done");
        let mut board = Board::main(ProjectId::from("p"), "p", now());
        board.lanes[1].slug = "backlog".into();
        assert!(board.check().is_err(), "a slug twice");
        let mut board = Board::main(ProjectId::from("p"), "p", now());
        board.lanes[0].slug = "Back Log".into();
        assert!(board.check().is_err(), "a slug with a space");
    }

    /// A lane is named by slug, name, category or a unique prefix; an
    /// unknown name lists the lanes.
    #[test]
    fn a_lane_is_found_by_slug_name_category_or_prefix() {
        let board =
            Board::from_template(ProjectId::from("p"), "b", "B", "agents", "P", now()).unwrap();
        assert_eq!(board.resolve("uat").unwrap().slug, "uat");
        assert_eq!(board.resolve("In progress").unwrap().slug, "in_progress");
        assert_eq!(
            board.resolve("review").unwrap().slug,
            "testing",
            "first Review lane"
        );
        assert_eq!(board.resolve("done").unwrap().slug, "done");
        assert_eq!(board.resolve("ana").unwrap().slug, "analyst");
        assert_eq!(board.resolve("can").unwrap().slug, "cancelled");
        match board.resolve("somewhere") {
            Err(BoardError::NoLane { lanes, .. }) => assert!(lanes.contains(&"uat".to_owned())),
            other => panic!("{other:?}"),
        }
    }

    /// Pulling with no card named takes the first free, unblocked card in
    /// the pull source, in board order, and says what else was there.
    #[test]
    fn pull_next_takes_the_first_free_unblocked_card() {
        let board = Board::main(ProjectId::from("p"), "p", now());
        let me = AgentId::from("me".to_owned());
        let other = AgentId::from("other".to_owned());
        let mut first = card(&board, "first", "ready");
        first.rank = "1".into();
        first.assignee = Some(other.clone());
        let mut second = card(&board, "second", "ready");
        second.rank = "2".into();
        second.external_blocker = "waiting on DNS".into();
        let mut third = card(&board, "third", "ready");
        third.rank = "3".into();
        let blocker = card(&board, "blocker", "in_progress");
        third.blocked_by = vec![blocker.id.clone()];
        let mut fourth = card(&board, "fourth", "ready");
        fourth.rank = "4".into();
        let backlog = card(&board, "later", "backlog");
        let cards = vec![
            fourth.clone(),
            third.clone(),
            second,
            first,
            backlog,
            blocker.clone(),
        ];
        let pick = board.next_ready(&cards, &me, |id| *id == blocker.id);
        assert_eq!(pick.card.map(|t| t.title.as_str()), Some("fourth"));
        assert_eq!((pick.total, pick.held, pick.blocked), (4, 1, 2));
        // Once the blocker is done, the third goes first.
        let pick = board.next_ready(&cards, &me, |_| false);
        assert_eq!(pick.card.map(|t| t.title.as_str()), Some("third"));
        // A card already the caller's is the caller's to pull again.
        let mut mine = fourth;
        mine.rank = "0".into();
        mine.assignee = Some(me.clone());
        let pick = board.next_ready(std::slice::from_ref(&mine), &me, |_| false);
        assert_eq!(pick.card.map(|t| t.title.as_str()), Some("fourth"));
    }

    #[test]
    fn ranks_sort_between_their_neighbours() {
        let mut ranks = vec![rank::between(None, None)];
        // Append, prepend and insert in the middle, many times over.
        for i in 0..200 {
            let next = match i % 3 {
                0 => rank::between(ranks.last().map(String::as_str), None),
                1 => rank::between(None, ranks.first().map(String::as_str)),
                _ => {
                    let mid = ranks.len() / 2;
                    rank::between(Some(&ranks[mid - 1]), Some(&ranks[mid]))
                }
            };
            ranks.push(next);
            ranks.sort();
            ranks.dedup();
            assert_eq!(ranks.len(), i + 2, "every rank is new");
        }
        for pair in ranks.windows(2) {
            let between = rank::between(Some(&pair[0]), Some(&pair[1]));
            assert!(pair[0] < between && between < pair[1], "{pair:?} {between}");
        }
    }

    #[test]
    fn keys_slugs_and_due_days_read_as_people_name_them() {
        assert_eq!(key_prefix("AgentDocker"), "AD");
        assert_eq!(key_prefix("river-lab"), "RL");
        assert_eq!(key_prefix("orchard"), "OR");
        assert_eq!(key_prefix("my big web app"), "MBWA");
        assert_eq!(key_prefix("42"), "T");
        assert_eq!(slug_of("Release train!"), "release-train");
        assert_eq!(slug_of("  "), "board");
        let mut board = Board::main(ProjectId::from("p"), "AgentDocker", now());
        assert_eq!(board.take_key(), "AD-1");
        assert_eq!(board.take_key(), "AD-2");
        let today = NaiveDate::from_ymd_opt(2026, 10, 16).unwrap();
        assert_eq!(
            due_state(today.pred_opt().unwrap(), today),
            DueState::Overdue
        );
        assert_eq!(due_state(today, today), DueState::Today);
        assert_eq!(due_state(today.succ_opt().unwrap(), today), DueState::Later);
    }

    #[test]
    fn a_lane_over_its_limit_says_so_and_refuses_nothing() {
        let mut lane = Lane::new("doing", "Doing", Column::InProgress, Owner::Agent);
        assert!(!lane.over(99), "no limit");
        lane.wip = Some(3);
        assert!(!lane.over(3));
        assert!(lane.over(4));
    }
}
