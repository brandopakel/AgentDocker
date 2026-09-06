//! Channels: the group message for agents who turn out to be working on
//! the same thing.
//!
//! A lease keeps two agents out of one file. A channel is for when they
//! are in one anyway — two worktrees that have both changed a path, or a
//! task somebody opened deliberately. The daemon opens one, puts the
//! agents in it, and from then on they talk and review each other's work
//! there. Reviews carried in a channel are what settles whose work lands:
//! approvals count, requested changes block, and only a reviewer's latest
//! word counts. When the work is final the channel closes, and closed
//! channels are pruned.
//!
//! This module is the pure half: what a channel is, and how its reviews
//! add up. Opening, routing and storage belong to the daemon.

use std::fmt;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{AgentId, ProjectId};

/// Paths a channel names in its title before "and N more".
const TITLE_PATHS: usize = 3;

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ChannelId(String);

impl ChannelId {
    pub fn generate() -> Self {
        let raw = uuid::Uuid::new_v4().simple().to_string();
        Self(raw[..12].to_owned())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for ChannelId {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

impl From<String> for ChannelId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl fmt::Display for ChannelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Why the agents are in one room.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChannelSubject {
    /// Paths that more than one checkout of the project has changed. The
    /// daemon opens these itself.
    Contested { paths: Vec<PathBuf> },
    /// Something an agent or the human named.
    Task { task: String },
}

impl ChannelSubject {
    /// A short line for tables and messages.
    pub fn title(&self) -> String {
        match self {
            Self::Task { task } => task.clone(),
            Self::Contested { paths } if paths.is_empty() => "contested work".to_owned(),
            Self::Contested { paths } => {
                let named: Vec<String> = paths
                    .iter()
                    .take(TITLE_PATHS)
                    .map(|p| p.display().to_string())
                    .collect();
                let more = paths.len().saturating_sub(named.len());
                if more > 0 {
                    format!("{} (+{more} more)", named.join(", "))
                } else {
                    named.join(", ")
                }
            }
        }
    }
}

/// What a reviewer said about somebody's work.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Good to land.
    Approve,
    /// Not yet: the note says what to change.
    Changes,
    /// Something worth saying that neither approves nor blocks.
    Comment,
}

impl Verdict {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "approve" | "approved" => Self::Approve,
            "changes" => Self::Changes,
            "comment" => Self::Comment,
            _ => return None,
        })
    }
}

impl fmt::Display for Verdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Approve => "approve",
            Self::Changes => "changes",
            Self::Comment => "comment",
        })
    }
}

/// One reviewer's word on one agent's work, at a moment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Review {
    pub by: AgentId,
    pub by_name: String,
    /// Whose work is under review.
    pub of: AgentId,
    #[serde(default)]
    pub of_name: String,
    pub verdict: Verdict,
    pub note: String,
    pub at: DateTime<Utc>,
    /// The reviewed checkout's HEAD, when it had one, so a verdict can be
    /// read against the code it was given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
}

impl Review {
    /// The author as a reader knows them, falling back to the id.
    fn author(&self) -> String {
        if self.of_name.is_empty() {
            self.of.short().to_owned()
        } else {
            self.of_name.clone()
        }
    }

    /// What was said, without naming the reviewer: for the journal, which
    /// already prefixes whose entry it is.
    pub fn summary(&self) -> String {
        let verb = match self.verdict {
            Verdict::Approve => "approved",
            Verdict::Changes => "asked changes of",
            Verdict::Comment => "commented on",
        };
        format!("{verb} work by {}: \"{}\"", self.author(), self.note)
    }

    /// "gemini-2 approved work by codex-1: reads fine".
    pub fn line(&self) -> String {
        format!("{} {}", self.by_name, self.summary())
    }
}

/// Where an agent's work stands with its reviewers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Decision {
    /// Enough approvals, nothing outstanding.
    Approved { approvals: usize },
    /// At least one reviewer wants changes; nothing lands until they say
    /// otherwise.
    Blocked { by: Vec<AgentId> },
    /// Not enough approvals yet.
    Pending { approvals: usize, needed: usize },
}

impl Decision {
    pub fn is_approved(&self) -> bool {
        matches!(self, Self::Approved { .. })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Channel {
    pub id: ChannelId,
    pub project: ProjectId,
    pub subject: ChannelSubject,
    pub members: Vec<AgentId>,
    /// `None` when the daemon opened it because the ledger showed two
    /// checkouts on one path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opened_by: Option<AgentId>,
    pub opened_at: DateTime<Utc>,
    #[serde(default)]
    pub reviews: Vec<Review>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<DateTime<Utc>>,
    /// Why it closed: what the work settled on, or that everyone left.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
}

impl Channel {
    pub fn is_open(&self) -> bool {
        self.closed_at.is_none()
    }

    pub fn title(&self) -> String {
        self.subject.title()
    }

    pub fn has(&self, agent: &AgentId) -> bool {
        self.members.iter().any(|m| m == agent)
    }

    /// Add an agent that turns out to belong here. Returns whether it was
    /// new.
    pub fn admit(&mut self, agent: AgentId) -> bool {
        if self.has(&agent) {
            return false;
        }
        self.members.push(agent);
        true
    }

    /// The paths this channel is about; empty for a task channel.
    pub fn paths(&self) -> &[PathBuf] {
        match &self.subject {
            ChannelSubject::Contested { paths } => paths,
            ChannelSubject::Task { .. } => &[],
        }
    }

    /// Fold in another contested path, so one channel covers a spreading
    /// overlap rather than one channel per file. Returns whether it was new.
    pub fn add_path(&mut self, path: PathBuf) -> bool {
        match &mut self.subject {
            ChannelSubject::Contested { paths } => {
                if paths.contains(&path) {
                    return false;
                }
                paths.push(path);
                paths.sort();
                true
            }
            ChannelSubject::Task { .. } => false,
        }
    }

    /// Only a reviewer's latest word on a given author counts, and nobody
    /// reviews themselves.
    fn latest_on(&self, author: &AgentId) -> Vec<&Review> {
        let mut latest: Vec<&Review> = Vec::new();
        for review in self
            .reviews
            .iter()
            .filter(|r| &r.of == author && &r.by != author)
        {
            match latest.iter_mut().find(|kept| kept.by == review.by) {
                Some(kept) if kept.at <= review.at => *kept = review,
                Some(_) => {}
                None => latest.push(review),
            }
        }
        latest
    }

    /// Where `author`'s work stands: blocked by anyone asking for changes,
    /// else approved once `required` reviewers have said so. This is the
    /// tie-break — when two agents have both done the work, the reviews
    /// decide, not whoever finished first.
    pub fn decision(&self, author: &AgentId, required: usize) -> Decision {
        let latest = self.latest_on(author);
        let blocked: Vec<AgentId> = latest
            .iter()
            .filter(|r| r.verdict == Verdict::Changes)
            .map(|r| r.by.clone())
            .collect();
        if !blocked.is_empty() {
            return Decision::Blocked { by: blocked };
        }
        let approvals = latest
            .iter()
            .filter(|r| r.verdict == Verdict::Approve)
            .count();
        if approvals >= required.max(1) {
            Decision::Approved { approvals }
        } else {
            Decision::Pending {
                approvals,
                needed: required.max(1),
            }
        }
    }

    /// Everyone in the channel who is not this agent: who to ask.
    pub fn others(&self, agent: &AgentId) -> Vec<AgentId> {
        self.members
            .iter()
            .filter(|m| *m != agent)
            .cloned()
            .collect()
    }

    /// One line for tables and the journal.
    pub fn line(&self) -> String {
        let state = match &self.closed_at {
            Some(_) => "closed",
            None => "open",
        };
        format!(
            "{} [{state}] {} member(s): {}",
            self.id,
            self.members.len(),
            self.title()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel() -> Channel {
        Channel {
            id: ChannelId::from("c1"),
            project: ProjectId::from("p1"),
            subject: ChannelSubject::Contested {
                paths: vec![PathBuf::from("src/parser.rs")],
            },
            members: vec![
                AgentId::from("a1"),
                AgentId::from("b2"),
                AgentId::from("c3"),
            ],
            opened_by: None,
            opened_at: Utc::now(),
            reviews: Vec::new(),
            closed_at: None,
            resolution: None,
        }
    }

    fn review(by: &str, of: &str, verdict: Verdict, minutes: i64) -> Review {
        Review {
            by: AgentId::from(by),
            by_name: by.to_owned(),
            of: AgentId::from(of),
            of_name: of.to_owned(),
            verdict,
            note: format!("{verdict} from {by}"),
            at: Utc::now() - chrono::Duration::minutes(minutes),
            head: None,
        }
    }

    #[test]
    fn a_title_names_its_paths_and_counts_the_rest() {
        let task = ChannelSubject::Task {
            task: "finish the parser".into(),
        };
        assert_eq!(task.title(), "finish the parser");
        let few = ChannelSubject::Contested {
            paths: vec!["a.rs".into(), "b.rs".into()],
        };
        assert_eq!(few.title(), "a.rs, b.rs");
        let many = ChannelSubject::Contested {
            paths: (0..6).map(|i| PathBuf::from(format!("f{i}.rs"))).collect(),
        };
        assert_eq!(many.title(), "f0.rs, f1.rs, f2.rs (+3 more)");
        assert_eq!(
            ChannelSubject::Contested { paths: vec![] }.title(),
            "contested work"
        );
    }

    #[test]
    fn membership_and_paths_grow_without_duplicating() {
        let mut c = channel();
        assert!(c.has(&AgentId::from("a1")));
        assert!(!c.admit(AgentId::from("a1")), "already a member");
        assert!(c.admit(AgentId::from("d4")));
        assert_eq!(c.members.len(), 4);
        assert_eq!(c.others(&AgentId::from("a1")).len(), 3);

        assert!(c.add_path(PathBuf::from("src/lexer.rs")));
        assert!(!c.add_path(PathBuf::from("src/lexer.rs")));
        assert_eq!(
            c.paths(),
            [
                PathBuf::from("src/lexer.rs"),
                PathBuf::from("src/parser.rs")
            ],
            "sorted"
        );
        let mut task = channel();
        task.subject = ChannelSubject::Task { task: "x".into() };
        assert!(!task.add_path(PathBuf::from("a.rs")), "a task has no paths");
        assert!(task.paths().is_empty());
    }

    #[test]
    fn reviews_decide_by_each_reviewers_latest_word() {
        let a1 = AgentId::from("a1");
        let mut c = channel();
        assert_eq!(
            c.decision(&a1, 1),
            Decision::Pending {
                approvals: 0,
                needed: 1
            }
        );

        // A comment neither approves nor blocks.
        c.reviews.push(review("b2", "a1", Verdict::Comment, 10));
        assert_eq!(
            c.decision(&a1, 1),
            Decision::Pending {
                approvals: 0,
                needed: 1
            }
        );

        // One approval is enough by default.
        c.reviews.push(review("b2", "a1", Verdict::Approve, 9));
        assert_eq!(c.decision(&a1, 1), Decision::Approved { approvals: 1 });
        assert!(c.decision(&a1, 1).is_approved());
        assert_eq!(
            c.decision(&a1, 2),
            Decision::Pending {
                approvals: 1,
                needed: 2
            },
            "two required, one given"
        );

        // Anyone asking for changes blocks it, however many approve.
        c.reviews.push(review("c3", "a1", Verdict::Changes, 5));
        assert_eq!(
            c.decision(&a1, 1),
            Decision::Blocked {
                by: vec![AgentId::from("c3")]
            }
        );

        // The same reviewer's newer word replaces the old one.
        c.reviews.push(review("c3", "a1", Verdict::Approve, 1));
        assert_eq!(c.decision(&a1, 1), Decision::Approved { approvals: 2 });

        // An older review never overrides a newer one, whatever the order
        // they were recorded in.
        c.reviews.push(review("c3", "a1", Verdict::Changes, 30));
        assert_eq!(
            c.decision(&a1, 1),
            Decision::Approved { approvals: 2 },
            "a stale verdict does not resurrect"
        );

        // Reviews of somebody else do not count, and nobody reviews itself.
        c.reviews.push(review("a1", "a1", Verdict::Approve, 0));
        assert_eq!(
            c.decision(&a1, 3),
            Decision::Pending {
                approvals: 2,
                needed: 3
            }
        );
        assert_eq!(
            c.decision(&AgentId::from("b2"), 1),
            Decision::Pending {
                approvals: 0,
                needed: 1
            }
        );
    }

    #[test]
    fn verdicts_and_lines_round_trip() {
        assert_eq!(Verdict::parse("approve"), Some(Verdict::Approve));
        assert_eq!(Verdict::parse("approved"), Some(Verdict::Approve));
        assert_eq!(Verdict::parse("changes"), Some(Verdict::Changes));
        assert_eq!(Verdict::parse("nope"), None);
        assert_eq!(Verdict::Changes.to_string(), "changes");
        let r = review("b2", "a1", Verdict::Approve, 0);
        assert_eq!(r.line(), "b2 approved work by a1: \"approve from b2\"");
        assert_eq!(
            r.summary(),
            "approved work by a1: \"approve from b2\"",
            "the journal prefixes the reviewer itself"
        );
        let anonymous = Review {
            of_name: String::new(),
            ..r.clone()
        };
        assert!(anonymous.summary().contains("a1"), "falls back to the id");
        let c = channel();
        assert!(c.line().contains("[open] 3 member(s)"));
        assert!(c.is_open());
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(serde_json::from_str::<Channel>(&json).unwrap(), c);
    }
}
