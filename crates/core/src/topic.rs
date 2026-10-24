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

/// What one subscribe replays, as sequences, oldest first, and how many
/// messages past where the subscriber starts it leaves out.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReplayPlan {
    pub seqs: Vec<u64>,
    pub skipped: u64,
}

/// Bound a replay: from each topic the newest `per_topic` of its waiting
/// messages, then across all of them the newest `total`. Each topic gives
/// how many messages wait past where the subscriber starts and the
/// sequences of the newest of them (at least `per_topic` of them when that
/// many wait; more are cut here). Everything left out is counted as
/// skipped, so the subscriber is told it missed some rather than left to
/// believe it saw them all. Memory stays within `total` plus one topic's
/// sequences however many topics match.
pub fn plan_replay(
    topics: impl IntoIterator<Item = (u64, Vec<u64>)>,
    per_topic: usize,
    total: usize,
) -> ReplayPlan {
    let mut kept: BinaryHeap<Reverse<u64>> = BinaryHeap::new();
    let mut skipped = 0u64;
    for (waiting, mut seqs) in topics {
        seqs.sort_unstable_by(|a, b| b.cmp(a));
        seqs.dedup();
        seqs.truncate(per_topic);
        skipped += waiting.saturating_sub(seqs.len() as u64);
        for seq in seqs {
            kept.push(Reverse(seq));
            if kept.len() > total {
                kept.pop();
                skipped += 1;
            }
        }
    }
    let mut seqs: Vec<u64> = kept.into_iter().map(|Reverse(seq)| seq).collect();
    seqs.sort_unstable();
    ReplayPlan { seqs, skipped }
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
        let plan = plan_replay([(5, vec![1, 3, 5, 7, 9]), (2, vec![2, 4])], 3, 10);
        assert_eq!(plan.seqs, [2, 4, 5, 7, 9]);
        assert_eq!(plan.skipped, 2, "the two oldest of the first topic");

        let plan = plan_replay([(5, vec![1, 3, 5, 7, 9]), (2, vec![2, 4])], 3, 4);
        assert_eq!(plan.seqs, [4, 5, 7, 9]);
        assert_eq!(plan.skipped, 3, "and the oldest kept across both");

        // A topic may say more wait than it hands over: the rest are
        // counted too.
        let plan = plan_replay([(800, vec![900, 899])], 500, 5_000);
        assert_eq!(plan.seqs, [899, 900]);
        assert_eq!(plan.skipped, 798);

        assert_eq!(plan_replay([], 500, 5_000), ReplayPlan::default());
    }
}
