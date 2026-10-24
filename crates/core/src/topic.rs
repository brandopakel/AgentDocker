//! Topics keep their messages. A topic message goes into a log the daemon
//! keeps for subscribers that join later, bounded per topic by count and by
//! age; a subscriber identified as an agent keeps a cursor per topic and
//! resumes after it.
//!
//! What is pure about that lives here: whose a kept message is and who may
//! read it, where a subscription starts, how far an acknowledgement moves a
//! cursor and how much one subscribe replays. The log itself, and the
//! cursors, are the daemon's.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap};

use serde::{Deserialize, Serialize};

use crate::{AgentRecord, HUMAN_RUNTIME, ProjectId, ProjectRef};

/// The most messages one subscribe replays from one topic: the newest of
/// those past where the subscriber starts.
pub const REPLAY_PER_TOPIC: usize = 500;

/// The most messages one subscribe replays across every topic its patterns
/// match, again the newest.
pub const REPLAY_TOTAL: usize = 5_000;

/// A kept topic message's place in the log.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TopicPost {
    /// Its sequence in the topic log: increasing, never reused, one
    /// sequence for every topic, so a single number says where a reader
    /// stands across all of them.
    pub seq: u64,
    /// The project it is kept for ([`scope`] of its sender); `None` when
    /// anyone may read it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<ProjectId>,
}

/// How far an agent has read one topic: through `seq`, the last message on
/// it that the agent acknowledged.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TopicCursor {
    pub topic: String,
    pub seq: u64,
}

/// The project whose topic traffic `record` is confined to, as a sender
/// and as a reader: an agent working in a project. `None`, anyone's, for
/// the person, for a party with no record (the daemon's own word, an
/// anonymous subscriber) and for an agent in no project — the same line
/// that keeps every other destination inside its project.
pub fn scope(record: Option<&AgentRecord>) -> Option<ProjectId> {
    record
        .filter(|record| record.spec.runtime != HUMAN_RUNTIME)
        .and_then(|record| record.project.as_ref())
        .map(ProjectRef::id)
}

/// Whether a reader confined to `reader` may see a message kept for
/// `kept_for`: a message kept for nobody in particular reaches everyone, an
/// unconfined reader sees every project's, and otherwise the projects must
/// be the same. Live delivery and replay ask the same question.
pub fn visible(kept_for: Option<&ProjectId>, reader: Option<&ProjectId>) -> bool {
    match (kept_for, reader) {
        (Some(kept_for), Some(reader)) => kept_for == reader,
        _ => true,
    }
}

/// Where a subscription starts reading a topic: after `after` when the
/// subscriber names a point, after its cursor when it is an agent (from the
/// oldest kept message when it has none yet), and nowhere — live only —
/// for a subscriber that is neither.
pub fn start(agent: bool, cursor: Option<u64>, after: Option<u64>) -> Option<u64> {
    match after {
        Some(after) => Some(after),
        None if agent => Some(cursor.unwrap_or(0)),
        None => None,
    }
}

/// The cursors acknowledging `acknowledged` (each a topic and the sequence
/// of one of its messages) moves: each topic to the highest sequence
/// acknowledged on it, when that is past where its cursor stands. A cursor
/// never moves back, and a topic whose cursor is already there is left out.
/// Sorted by topic.
pub fn advance(
    current: impl Fn(&str) -> Option<u64>,
    acknowledged: impl IntoIterator<Item = (String, u64)>,
) -> Vec<TopicCursor> {
    let mut highest: BTreeMap<String, u64> = BTreeMap::new();
    for (topic, seq) in acknowledged {
        let entry = highest.entry(topic).or_insert(seq);
        *entry = (*entry).max(seq);
    }
    highest
        .into_iter()
        .filter(|(topic, seq)| current(topic).is_none_or(|cursor| *seq > cursor))
        .map(|(topic, seq)| TopicCursor { topic, seq })
        .collect()
}

/// What one subscribe replays, as sequences, oldest first; how many
/// messages past where the subscriber starts it leaves out; and the most
/// candidate sequences it held at once while choosing them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReplayPlan {
    pub seqs: Vec<u64>,
    pub skipped: u64,
    pub peak: usize,
}

/// Bound a replay as its topics are read, one at a time: from each topic
/// the newest `per_topic` of its waiting messages, across all of them the
/// newest `total`. It never holds more than `total` candidate sequences,
/// however many topics match: a topic is offered its sequences newest
/// first, and [`ReplayBound::offer`] says when nothing more from that topic
/// could be kept, so the reader stops reading it. Everything waiting that is
/// not kept is counted as skipped, so the subscriber is told it missed some
/// rather than left to believe it saw them all.
#[derive(Clone, Debug)]
pub struct ReplayBound {
    per_topic: usize,
    total: usize,
    kept: BinaryHeap<Reverse<u64>>,
    waiting: u64,
    taken: usize,
    peak: usize,
}

impl ReplayBound {
    pub fn new(per_topic: usize, total: usize) -> Self {
        Self {
            per_topic,
            total,
            kept: BinaryHeap::new(),
            waiting: 0,
            taken: 0,
            peak: 0,
        }
    }

    /// The next topic: how many of its messages wait past where the
    /// subscriber starts there. Its sequences follow through
    /// [`ReplayBound::offer`], newest first.
    pub fn topic(&mut self, waiting: u64) {
        self.waiting += waiting;
        self.taken = 0;
    }

    /// Offer the current topic's next sequence, newest first. `false` when
    /// nothing more from this topic can be kept — its share is taken, or
    /// this and everything older loses to what is already kept — and the
    /// reader should go on to the next topic.
    pub fn offer(&mut self, seq: u64) -> bool {
        if self.taken >= self.per_topic || self.total == 0 {
            return false;
        }
        if self.kept.len() >= self.total {
            match self.kept.peek() {
                Some(Reverse(oldest)) if seq > *oldest => {
                    self.kept.pop();
                }
                _ => return false,
            }
        }
        self.kept.push(Reverse(seq));
        self.taken += 1;
        self.peak = self.peak.max(self.kept.len());
        self.taken < self.per_topic
    }

    pub fn finish(self) -> ReplayPlan {
        let mut seqs: Vec<u64> = self.kept.into_iter().map(|Reverse(seq)| seq).collect();
        seqs.sort_unstable();
        ReplayPlan {
            skipped: self.waiting.saturating_sub(seqs.len() as u64),
            seqs,
            peak: self.peak,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AgentSpec, topic_matches};

    fn record(runtime: &str, project: Option<&str>) -> AgentRecord {
        let mut record = AgentRecord::new(
            AgentSpec {
                name: "agent-1".into(),
                runtime: runtime.into(),
                ..AgentSpec::default()
            },
            false,
            chrono::Utc::now(),
        );
        record.project = project.map(|name| ProjectRef::directory(format!("/work/{name}")));
        record
    }

    /// An agent working in a project is confined to it as a sender and as
    /// a reader; the person, a party with no record and an agent in no
    /// project are not.
    #[test]
    fn only_an_agent_in_a_project_is_confined_to_it() {
        let alpha = ProjectRef::directory("/work/alpha").id();
        assert_eq!(scope(Some(&record("claude", Some("alpha")))), Some(alpha));
        assert_eq!(scope(Some(&record(HUMAN_RUNTIME, Some("alpha")))), None);
        assert_eq!(scope(Some(&record("claude", None))), None);
        assert_eq!(scope(None), None);
    }

    /// A message kept for a project reaches that project and unconfined
    /// readers, never another project; one kept for nobody reaches all.
    #[test]
    fn a_kept_message_is_visible_to_exactly_who_could_have_had_it_live() {
        let alpha = ProjectId::from("alpha");
        let beta = ProjectId::from("beta");
        assert!(visible(Some(&alpha), Some(&alpha)));
        assert!(!visible(Some(&alpha), Some(&beta)));
        assert!(visible(Some(&alpha), None));
        assert!(visible(None, Some(&beta)));
        assert!(visible(None, None));
    }

    /// Patterns pick the kept topics a subscription replays from, the same
    /// way they pick live ones.
    #[test]
    fn patterns_select_the_kept_topics_to_replay() {
        let kept = [
            "repo/backend/reviews",
            "repo/frontend/reviews",
            "repo",
            "other/reviews",
        ];
        let matching = |pattern: &str| {
            kept.iter()
                .copied()
                .filter(|topic| topic_matches(pattern, topic))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            matching("repo/+/reviews"),
            ["repo/backend/reviews", "repo/frontend/reviews"]
        );
        assert_eq!(
            matching("repo/#"),
            ["repo/backend/reviews", "repo/frontend/reviews", "repo"]
        );
        assert_eq!(matching("+/reviews"), ["other/reviews"]);
        assert!(matching("nothing/+").is_empty());
    }

    /// A named point wins, an agent resumes after its cursor or from the
    /// oldest kept message, and anyone else is live only.
    #[test]
    fn where_a_subscription_starts() {
        assert_eq!(start(true, Some(7), None), Some(7));
        assert_eq!(start(true, None, None), Some(0), "a first-time agent");
        assert_eq!(start(true, Some(7), Some(3)), Some(3));
        assert_eq!(start(false, None, Some(3)), Some(3));
        assert_eq!(start(false, None, None), None, "live only");
    }

    /// Each topic moves to the highest sequence acknowledged on it, never
    /// back, and a topic already there is not moved at all.
    #[test]
    fn acknowledging_moves_each_cursor_forward_to_the_highest() {
        let current = |topic: &str| match topic {
            "a" => Some(10),
            "b" => Some(4),
            _ => None,
        };
        let moved = advance(
            current,
            [
                ("b".to_owned(), 6),
                ("a".to_owned(), 9),
                ("b".to_owned(), 5),
                ("c".to_owned(), 2),
                ("a".to_owned(), 10),
            ],
        );
        assert_eq!(
            moved,
            [
                TopicCursor {
                    topic: "b".into(),
                    seq: 6
                },
                TopicCursor {
                    topic: "c".into(),
                    seq: 2
                },
            ]
        );
        assert!(advance(current, []).is_empty());
    }

    /// The newest per topic, then the newest overall, oldest first, with
    /// everything left out counted.
    #[test]
    fn a_replay_keeps_the_newest_and_counts_the_rest() {
        // Each topic: how many wait, and its sequences, newest first.
        let plan = |topics: &[(u64, &[u64])], per_topic: usize, total: usize| {
            let mut bound = ReplayBound::new(per_topic, total);
            for (waiting, seqs) in topics {
                bound.topic(*waiting);
                for seq in *seqs {
                    if !bound.offer(*seq) {
                        break;
                    }
                }
            }
            bound.finish()
        };
        let both: &[(u64, &[u64])] = &[(5, &[9, 7, 5, 3, 1]), (2, &[4, 2])];
        let replay = plan(both, 3, 10);
        assert_eq!(replay.seqs, [2, 4, 5, 7, 9]);
        assert_eq!(replay.skipped, 2, "the two oldest of the first topic");

        let replay = plan(both, 3, 4);
        assert_eq!(replay.seqs, [4, 5, 7, 9]);
        assert_eq!(replay.skipped, 3, "and the oldest kept across both");
        assert_eq!(replay.peak, 4);

        // A topic may say more wait than it hands over: the rest are
        // counted too.
        let replay = plan(&[(800, &[900, 899])], 500, 5_000);
        assert_eq!(replay.seqs, [899, 900]);
        assert_eq!(replay.skipped, 798);

        assert_eq!(plan(&[], 500, 5_000), ReplayPlan::default());
        assert_eq!(plan(&[(3, &[3, 2, 1])], 500, 0).skipped, 3);
    }

    /// However many topics are offered, no more than `total` candidates
    /// are ever held, and a topic stops being read as soon as nothing more
    /// of it could be kept.
    #[test]
    fn a_replay_holds_no_more_than_its_total_however_many_topics() {
        let mut bound = ReplayBound::new(500, 50);
        let mut read = 0usize;
        // A thousand topics of five hundred each, interleaved sequences.
        for topic in 0..1_000u64 {
            bound.topic(500);
            for n in (0..500u64).rev() {
                read += 1;
                if !bound.offer(n * 1_000 + topic) {
                    break;
                }
            }
        }
        let replay = bound.finish();
        assert!(replay.peak <= 50, "held {}", replay.peak);
        assert_eq!(replay.seqs.len(), 50);
        assert_eq!(
            replay.seqs,
            (0..50).map(|n| 499_000 + 950 + n).collect::<Vec<_>>()
        );
        assert_eq!(replay.skipped, 500_000 - 50);
        assert!(
            read < 5_000,
            "read {read}: a few rows of each topic, not five hundred"
        );
    }
}
