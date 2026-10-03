//! Contests: several agents attempt one task, and the evidence decides.
//!
//! The usual way to have two agents work on one thing is to let them
//! collide and then sort it out — which is what channels are for. A
//! contest is the deliberate version: the task is announced, each
//! entrant works in its own worktree, and each submits a *passing
//! validation* as its claim to have finished. Nothing that has not
//! passed is ranked at all, so "best" can never mean "fastest to produce
//! something broken".
//!
//! Two rules do the real work.
//!
//! **The measure is declared before anyone starts.** It is recorded when
//! the contest opens and cannot be changed afterwards, so nobody chooses
//! the number that flatters them after seeing the results. Where the
//! daemon can measure it itself — how long the entry's own validation
//! took, which it timed — it does, and the entrant is not consulted.
//! Where it cannot, the entrant reports a number under the name everyone
//! agreed on, and review is the check on it.
//!
//! **A margin inside the noise floor is not a win.** Declared with the
//! measure, again before anyone starts. Everything inside it is a tie,
//! and a tie is settled by the channel: what the other agents say about
//! the work, not who happened to be a hundredth of a second quicker.
//!
//! Pure: no clock, no I/O, no notion of where a validation is stored.

use std::fmt;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{AgentId, ChannelId, ProjectId};

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ContestId(String);

impl ContestId {
    pub fn generate() -> Self {
        let raw = uuid::Uuid::new_v4().simple().to_string();
        Self(raw[..12].to_owned())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for ContestId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl fmt::Display for ContestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What is being compared.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "measure", rename_all = "snake_case")]
pub enum Measure {
    /// How long the entry's own validation took, as the daemon timed it.
    /// Nobody reports this one: it comes from the evidence, so it cannot
    /// be inflated or invented.
    ValidationSeconds,
    /// A number the entrant reports, named here so that everyone reports
    /// the same one. The daemon cannot check it; the channel can.
    Reported { name: String },
    /// Where the opt-in judge model places each entry's change on levels
    /// the opener described, worst first: the expected level, so higher
    /// is better. The daemon asks once per submission and the entrant is
    /// not consulted; an entry is ranked once its answer is in.
    Judged { rubric: Vec<String> },
}

/// A judged measure has at least this many levels…
pub const RUBRIC_MIN: usize = 2;
/// …and at most this many, each at most [`RUBRIC_CHARS`] characters.
pub const RUBRIC_MAX: usize = 10;
pub const RUBRIC_CHARS: usize = 300;

impl Measure {
    pub fn name(&self) -> &str {
        match self {
            Self::ValidationSeconds => "validation seconds",
            Self::Reported { name } => name,
            Self::Judged { .. } => "judged quality",
        }
    }

    pub fn is_judged(&self) -> bool {
        matches!(self, Self::Judged { .. })
    }

    /// Whether the daemon takes this number itself rather than being told
    /// it. Worth showing beside a result: one kind of score is evidence
    /// and the other is a claim.
    pub fn measured(&self) -> bool {
        matches!(self, Self::ValidationSeconds)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// Less is better: seconds, allocations, lines changed.
    Lower,
    /// More is better: cases covered, throughput.
    Higher,
}

/// How entries are ranked, fixed before any of them exists.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Metric {
    pub measure: Measure,
    pub direction: Direction,
    /// Differences smaller than this are not differences. Everything
    /// inside it ties, and a tie goes to review.
    pub noise: f64,
}

impl Metric {
    /// Whether `a` beats `b` by more than the noise floor.
    pub fn beats(&self, a: f64, b: f64) -> bool {
        (b - a > self.noise) == matches!(self.direction, Direction::Lower)
            && (a - b).abs() > self.noise
    }

    /// Whether these two are the same as far as this metric can tell.
    pub fn ties(&self, a: f64, b: f64) -> bool {
        (a - b).abs() <= self.noise
    }
}

/// One agent's attempt, with the evidence that it works.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub agent: AgentId,
    /// The worktree it was built in, so a reviewer can read the code the
    /// score came from.
    pub checkout: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    /// The validation that says this entry is correct. Required, and
    /// required to have passed: an entry that does not work is not
    /// ranked, however good its number.
    pub validation: String,
    pub score: f64,
    pub submitted_at: DateTime<Utc>,
    /// For a judged measure, the answer `score` came from; until it is
    /// in, the entry waits and is not ranked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judged: Option<Judged>,
}

/// The judge's answer for one entry.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Judged {
    /// How concentrated the answer was, 0 to 1: a low one is a reading
    /// the channel should look at before trusting the order.
    pub confidence: f64,
    /// The versioned model that answered.
    pub model: String,
    pub at: DateTime<Utc>,
}

/// Where a contest stands.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "standing", rename_all = "snake_case")]
pub enum Standing {
    /// Nobody has submitted a passing entry yet.
    Open { entrants: usize, entries: usize },
    /// One entry is better than every other by more than the noise.
    Leader {
        agent: AgentId,
        score: f64,
        /// How far clear of the next one it is; `None` when there is no
        /// next one. Not an infinity: JSON has no way to write one, and
        /// `serde_json` turns it into `null`, which will not read back
        /// as a number.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        margin: Option<f64>,
    },
    /// Several are inside the noise floor of the best. The metric has
    /// said all it can; the channel settles it.
    Tied { agents: Vec<AgentId>, score: f64 },
    /// Somebody decided.
    Settled {
        winner: AgentId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resolution: Option<String>,
    },
}

/// One task, several attempts, one answer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Contest {
    pub id: ContestId,
    pub project: ProjectId,
    pub task: String,
    /// Fixed at opening. Nothing changes it.
    pub metric: Metric,
    pub opened_by: String,
    pub opened_at: DateTime<Utc>,
    pub entrants: Vec<AgentId>,
    pub entries: Vec<Entry>,
    /// Where ties are argued and the result announced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<ChannelId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub winner: Option<AgentId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
    /// For a judged measure, the commit each entry's change is read
    /// against: the opener's HEAD when the contest opened.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
}

/// Why an entry was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EntryError {
    Closed,
    NotEntered,
    /// The evidence belongs to somebody else, or to other code.
    Provenance(String),
    /// The validation did not pass, so there is nothing to rank.
    Failed,
}

impl fmt::Display for EntryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Closed => f.write_str("the contest is closed"),
            Self::NotEntered => f.write_str("this agent is not in the contest"),
            Self::Provenance(why) => write!(f, "the evidence does not match the entry: {why}"),
            Self::Failed => {
                f.write_str("the validation did not pass, so the entry cannot be ranked")
            }
        }
    }
}

impl Contest {
    pub fn new(
        project: ProjectId,
        task: String,
        metric: Metric,
        opened_by: String,
        entrants: Vec<AgentId>,
        now: DateTime<Utc>,
    ) -> Self {
        Self {
            id: ContestId::generate(),
            project,
            task,
            metric,
            opened_by,
            opened_at: now,
            entrants,
            entries: Vec::new(),
            channel: None,
            closed_at: None,
            winner: None,
            resolution: None,
            base: None,
        }
    }

    pub fn is_open(&self) -> bool {
        self.closed_at.is_none()
    }

    pub fn has(&self, agent: &AgentId) -> bool {
        self.entrants.contains(agent)
    }

    /// Join. Late entry is allowed while the contest is open — the
    /// measure was fixed before anyone started, so a latecomer gains
    /// nothing by having seen the others.
    pub fn enter(&mut self, agent: AgentId) -> bool {
        if !self.is_open() || self.has(&agent) {
            return false;
        }
        self.entrants.push(agent);
        true
    }

    /// Submit an attempt. One entry per agent: a resubmission replaces
    /// the earlier one, so an agent improves its own answer rather than
    /// filling the table with attempts.
    pub fn submit(&mut self, entry: Entry, validation_passed: bool) -> Result<(), EntryError> {
        if !self.is_open() {
            return Err(EntryError::Closed);
        }
        if !self.has(&entry.agent) {
            return Err(EntryError::NotEntered);
        }
        if !validation_passed {
            return Err(EntryError::Failed);
        }
        match self.entries.iter().position(|e| e.agent == entry.agent) {
            Some(at) => self.entries[at] = entry,
            None => self.entries.push(entry),
        }
        Ok(())
    }

    pub fn entry_of(&self, agent: &AgentId) -> Option<&Entry> {
        self.entries.iter().find(|e| e.agent == *agent)
    }

    /// Whether an entry has its number: always, except a judged entry
    /// whose answer is not in.
    pub fn is_scored(&self, entry: &Entry) -> bool {
        !self.metric.measure.is_judged() || entry.judged.is_some()
    }

    /// Record the judge's answer for an agent's entry, if that entry is
    /// still the submission the answer was for and still waits for one.
    pub fn judge(&mut self, agent: &AgentId, validation: &str, score: f64, judged: Judged) -> bool {
        if !self.is_open() || !self.metric.measure.is_judged() || !score.is_finite() {
            return false;
        }
        match self
            .entries
            .iter_mut()
            .find(|e| e.agent == *agent && e.validation == validation && e.judged.is_none())
        {
            Some(entry) => {
                entry.score = score;
                entry.judged = Some(judged);
                true
            }
            None => false,
        }
    }

    /// Entries best-first. Only submitted ones are here, and only
    /// passing ones were ever accepted; a judged entry still waiting for
    /// its answer is left out until it has one.
    pub fn ranked(&self) -> Vec<&Entry> {
        let mut ranked: Vec<&Entry> = self.entries.iter().filter(|e| self.is_scored(e)).collect();
        ranked.sort_by(|a, b| {
            let ordering = a
                .score
                .partial_cmp(&b.score)
                .unwrap_or(std::cmp::Ordering::Equal);
            match self.metric.direction {
                Direction::Lower => ordering,
                Direction::Higher => ordering.reverse(),
            }
            // Oldest first among equals, so the order is stable and does
            // not reward resubmitting an identical number.
            .then_with(|| a.submitted_at.cmp(&b.submitted_at))
        });
        ranked
    }

    /// Where this stands: nobody yet, one clear leader, a tie for review,
    /// or a decision somebody has made.
    pub fn standing(&self) -> Standing {
        if let Some(winner) = &self.winner {
            return Standing::Settled {
                winner: winner.clone(),
                resolution: self.resolution.clone(),
            };
        }
        let ranked = self.ranked();
        let Some(best) = ranked.first() else {
            return Standing::Open {
                entrants: self.entrants.len(),
                entries: self.entries.len(),
            };
        };
        let tied: Vec<AgentId> = ranked
            .iter()
            .filter(|e| self.metric.ties(e.score, best.score))
            .map(|e| e.agent.clone())
            .collect();
        if tied.len() > 1 {
            return Standing::Tied {
                agents: tied,
                score: best.score,
            };
        }
        Standing::Leader {
            agent: best.agent.clone(),
            score: best.score,
            margin: ranked.get(1).map(|next| (next.score - best.score).abs()),
        }
    }

    /// One line for a listing.
    pub fn line(&self) -> String {
        let state = if self.is_open() { "open" } else { "closed" };
        format!(
            "{} [{state}] {} — by {} ({} entrant(s), {} entry(ies))",
            self.id,
            self.task,
            self.metric.measure.name(),
            self.entrants.len(),
            self.entries.len()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000, 0).unwrap()
    }

    fn contest(direction: Direction, noise: f64) -> Contest {
        Contest::new(
            ProjectId::from("p"),
            "make the parser faster".to_owned(),
            Metric {
                measure: Measure::ValidationSeconds,
                direction,
                noise,
            },
            "user".to_owned(),
            vec![AgentId::from("a"), AgentId::from("b")],
            now(),
        )
    }

    fn entry(agent: &str, score: f64, seconds: i64) -> Entry {
        Entry {
            agent: AgentId::from(agent),
            checkout: PathBuf::from(format!("/work/{agent}")),
            head: Some("abc123".to_owned()),
            validation: format!("v-{agent}"),
            score,
            submitted_at: now() + chrono::Duration::seconds(seconds),
            judged: None,
        }
    }

    /// A judged entry waits, unranked, until its answer is in; the answer
    /// lands only on the submission it was for, once, while the contest
    /// is open; then it ranks like any number, higher first.
    #[test]
    fn a_judged_entry_ranks_only_once_its_answer_is_in() {
        let mut contest = contest(Direction::Higher, 0.25);
        contest.metric.measure = Measure::Judged {
            rubric: vec!["broken".into(), "works".into(), "clean and complete".into()],
        };
        contest.submit(entry("a", 0.0, 0), true).unwrap();
        contest.submit(entry("b", 0.0, 1), true).unwrap();
        assert!(contest.ranked().is_empty(), "nothing judged yet");
        assert!(matches!(contest.standing(), Standing::Open { .. }));
        let judged = |confidence| Judged {
            confidence,
            model: "jev-1.13.0".into(),
            at: now(),
        };
        assert!(
            !contest.judge(&AgentId::from("a"), "v-other", 1.0, judged(0.9)),
            "another submission"
        );
        assert!(
            !contest.judge(&AgentId::from("a"), "v-a", f64::NAN, judged(0.9)),
            "not a number"
        );
        assert!(contest.judge(&AgentId::from("a"), "v-a", 1.2, judged(0.8)));
        assert!(
            !contest.judge(&AgentId::from("a"), "v-a", 2.0, judged(0.8)),
            "answered once"
        );
        let Standing::Leader {
            agent,
            score,
            margin,
        } = contest.standing()
        else {
            panic!("one judged entry leads")
        };
        assert_eq!((agent.as_str(), score, margin), ("a", 1.2, None));
        assert!(contest.judge(&AgentId::from("b"), "v-b", 1.9, judged(0.7)));
        let ranked: Vec<&str> = contest.ranked().iter().map(|e| e.agent.as_str()).collect();
        assert_eq!(ranked, ["b", "a"], "higher is better");
        contest.closed_at = Some(now());
        contest.entries[0].judged = None;
        assert!(
            !contest.judge(&AgentId::from("a"), "v-a", 2.0, judged(0.8)),
            "closed"
        );
        let json = serde_json::to_string(&contest).unwrap();
        assert_eq!(serde_json::from_str::<Contest>(&json).unwrap(), contest);
    }

    #[test]
    fn an_entry_that_did_not_pass_is_not_ranked_however_good_its_number() {
        let mut contest = contest(Direction::Lower, 0.0);
        assert_eq!(
            contest.submit(entry("a", 0.001, 0), false),
            Err(EntryError::Failed),
            "the fastest broken answer is still a broken answer"
        );
        assert!(contest.entries.is_empty());
        assert!(matches!(
            contest.standing(),
            Standing::Open { entries: 0, .. }
        ));
    }

    #[test]
    fn an_agent_that_never_entered_cannot_submit() {
        let mut contest = contest(Direction::Lower, 0.0);
        assert_eq!(
            contest.submit(entry("outsider", 1.0, 0), true),
            Err(EntryError::NotEntered)
        );
    }

    #[test]
    fn a_closed_contest_takes_no_more_entries() {
        let mut contest = contest(Direction::Lower, 0.0);
        contest.closed_at = Some(now());
        assert_eq!(
            contest.submit(entry("a", 1.0, 0), true),
            Err(EntryError::Closed)
        );
        assert!(!contest.enter(AgentId::from("c")));
    }

    #[test]
    fn the_best_passing_entry_leads_when_it_is_clear_of_the_rest() {
        let mut contest = contest(Direction::Lower, 0.1);
        contest.submit(entry("a", 5.0, 0), true).unwrap();
        contest.submit(entry("b", 2.0, 1), true).unwrap();
        let Standing::Leader {
            agent,
            score,
            margin,
        } = contest.standing()
        else {
            panic!("b is three seconds clear: {:?}", contest.standing())
        };
        assert_eq!(agent, AgentId::from("b"));
        assert_eq!(score, 2.0);
        assert_eq!(margin, Some(3.0));
    }

    #[test]
    fn higher_is_better_when_the_metric_says_so() {
        let mut contest = contest(Direction::Higher, 0.1);
        contest.submit(entry("a", 5.0, 0), true).unwrap();
        contest.submit(entry("b", 2.0, 1), true).unwrap();
        let Standing::Leader { agent, .. } = contest.standing() else {
            panic!("a covers more cases")
        };
        assert_eq!(agent, AgentId::from("a"));
    }

    #[test]
    fn a_margin_inside_the_noise_floor_is_a_tie_for_review_to_settle() {
        let mut contest = contest(Direction::Lower, 0.5);
        contest.submit(entry("a", 2.0, 0), true).unwrap();
        contest.submit(entry("b", 2.3, 1), true).unwrap();
        let Standing::Tied { agents, score } = contest.standing() else {
            panic!(
                "0.3s apart with a 0.5s floor is a tie: {:?}",
                contest.standing()
            )
        };
        assert_eq!(agents, vec![AgentId::from("a"), AgentId::from("b")]);
        assert_eq!(score, 2.0, "the best of the tied scores");
    }

    #[test]
    fn resubmitting_replaces_rather_than_stacks() {
        let mut contest = contest(Direction::Lower, 0.0);
        contest.submit(entry("a", 9.0, 0), true).unwrap();
        contest.submit(entry("a", 1.0, 1), true).unwrap();
        assert_eq!(contest.entries.len(), 1, "one entry per agent");
        assert_eq!(contest.entry_of(&AgentId::from("a")).unwrap().score, 1.0);
    }

    #[test]
    fn equal_scores_rank_by_who_submitted_first() {
        let mut contest = contest(Direction::Lower, 0.0);
        contest.submit(entry("b", 2.0, 10), true).unwrap();
        contest.submit(entry("a", 2.0, 1), true).unwrap();
        let ranked = contest.ranked();
        assert_eq!(ranked[0].agent, AgentId::from("a"), "a was first");
        // With a zero noise floor identical scores still tie: the metric
        // cannot separate them, so order is presentation, not a verdict.
        assert!(matches!(contest.standing(), Standing::Tied { .. }));
    }

    #[test]
    fn a_decision_overrides_the_ranking_because_review_is_the_tie_break() {
        let mut contest = contest(Direction::Lower, 0.5);
        contest.submit(entry("a", 2.0, 0), true).unwrap();
        contest.submit(entry("b", 2.3, 1), true).unwrap();
        contest.winner = Some(AgentId::from("b"));
        contest.resolution = Some("clearer, and the numbers were a tie".to_owned());
        let Standing::Settled { winner, resolution } = contest.standing() else {
            panic!("somebody decided")
        };
        assert_eq!(winner, AgentId::from("b"));
        assert_eq!(
            resolution.as_deref(),
            Some("clearer, and the numbers were a tie")
        );
    }

    #[test]
    fn the_noise_floor_is_symmetric_and_beating_it_is_not() {
        let metric = Metric {
            measure: Measure::Reported {
                name: "allocations".to_owned(),
            },
            direction: Direction::Lower,
            noise: 1.0,
        };
        assert!(metric.ties(10.0, 10.5));
        assert!(metric.ties(10.5, 10.0));
        assert!(!metric.ties(10.0, 12.0));
        assert!(metric.beats(10.0, 12.0), "lower is better");
        assert!(!metric.beats(12.0, 10.0));
        assert!(!metric.beats(10.0, 10.5), "not by more than the noise");
    }

    #[test]
    fn a_standing_survives_a_round_trip_through_json() {
        // A lone leader has nothing to be clear of. An infinite margin
        // would serialise as `null` and refuse to read back, which is a
        // failure the wire only shows once there is exactly one entry.
        let mut contest = contest(Direction::Lower, 0.0);
        contest.submit(entry("a", 2.0, 0), true).unwrap();
        let standing = contest.standing();
        assert!(matches!(standing, Standing::Leader { margin: None, .. }));
        let json = serde_json::to_string(&standing).unwrap();
        assert_eq!(
            serde_json::from_str::<Standing>(&json).unwrap(),
            standing,
            "{json}"
        );
        // And so does the whole contest.
        let json = serde_json::to_string(&contest).unwrap();
        assert_eq!(serde_json::from_str::<Contest>(&json).unwrap(), contest);
    }

    #[test]
    fn a_measured_metric_is_evidence_and_a_reported_one_is_a_claim() {
        assert!(Measure::ValidationSeconds.measured());
        assert!(
            !Measure::Reported {
                name: "lines".to_owned()
            }
            .measured()
        );
        assert_eq!(
            Measure::Reported {
                name: "lines".to_owned()
            }
            .name(),
            "lines"
        );
    }

    #[test]
    fn late_entry_is_allowed_but_only_once() {
        let mut contest = contest(Direction::Lower, 0.0);
        assert!(contest.enter(AgentId::from("c")));
        assert!(!contest.enter(AgentId::from("c")), "and only once");
        assert!(contest.has(&AgentId::from("c")));
    }
}
