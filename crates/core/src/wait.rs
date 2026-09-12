//! Who is waiting for what, and whether that can ever finish.
//!
//! `claim --wait` used to be a retry loop and nothing more: on a conflict
//! the caller slept until an overlapping lease cleared, then raced every
//! other sleeper for it. Two things were missing. A newcomer could take a
//! resource somebody had been waiting on for minutes, and a set of agents
//! each holding what the next one wants had no way of finding out — they
//! all simply waited for their TTLs.
//!
//! Making waiting a fact rather than a sleep fixes both, and gives the
//! daemon a third thing for free: an agent that is blocked is blocked on
//! a *named resource held by a named agent*, which is a far better answer
//! than guessing at what its terminal last printed.
//!
//! Pure: no clock of its own, no I/O. The daemon passes `now` and keeps
//! the queue under its one state lock.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{AgentId, LeaseMode, ResourceKey};

/// A ticket: arrival order across the whole queue, so that waiters on
/// overlapping resources share one order rather than each having their
/// own. Handed out by [`WaitQueue::join`] and given back to leave.
pub type Ticket = u64;

/// One request waiting for a resource.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Waiter {
    pub ticket: Ticket,
    pub agent: AgentId,
    pub resource: ResourceKey,
    pub mode: LeaseMode,
    pub since: DateTime<Utc>,
}

/// Waiters in arrival order, oldest first.
#[derive(Debug, Default)]
pub struct WaitQueue {
    waiters: Vec<Waiter>,
    next_ticket: Ticket,
}

impl WaitQueue {
    pub fn new() -> Self {
        Self::default()
    }

    /// Take a place in the queue. The ticket identifies this waiter for
    /// as long as it waits; the caller gives it back to [`Self::leave`].
    pub fn join(
        &mut self,
        agent: AgentId,
        resource: ResourceKey,
        mode: LeaseMode,
        now: DateTime<Utc>,
    ) -> Ticket {
        let ticket = self.next_ticket;
        self.next_ticket += 1;
        self.waiters.push(Waiter {
            ticket,
            agent,
            resource,
            mode,
            since: now,
        });
        ticket
    }

    /// Give up a place, whether the wait ended in a lease, a timeout, or
    /// a client that went away.
    pub fn leave(&mut self, ticket: Ticket) -> Option<Waiter> {
        let at = self.waiters.iter().position(|w| w.ticket == ticket)?;
        Some(self.waiters.remove(at))
    }

    /// Whether it is this waiter's turn: no older waiter wants something
    /// that overlaps and would exclude it.
    ///
    /// Two shared waiters do not block each other, so a queue of readers
    /// is not serialised by its own fairness rule. An unknown ticket is
    /// not next — it is nobody.
    pub fn is_next(&self, ticket: Ticket) -> bool {
        let Some(mine) = self.waiters.iter().find(|w| w.ticket == ticket) else {
            return false;
        };
        !self
            .waiters
            .iter()
            .any(|other| other.ticket < mine.ticket && self.blocks(other, mine))
    }

    /// How many waiters are ahead of this one *and in its way*. Zero
    /// means it is next; `None` means it is not queued.
    ///
    /// The same exclusion rule as [`Self::is_next`], and it has to be:
    /// this number is published as `lease_waiting.position` and shown as
    /// a queue place, so a shared waiter that may proceed at once must
    /// not be told it is second.
    pub fn position(&self, ticket: Ticket) -> Option<usize> {
        let mine = self.waiters.iter().find(|w| w.ticket == ticket)?;
        Some(
            self.waiters
                .iter()
                .filter(|other| other.ticket < mine.ticket && self.blocks(other, mine))
                .count(),
        )
    }

    /// Whether `other` stands in `mine`'s way: it wants something that
    /// overlaps, and at least one of them wants it exclusively.
    fn blocks(&self, other: &Waiter, mine: &Waiter) -> bool {
        other.resource.overlaps(&mine.resource)
            && (other.mode == LeaseMode::Exclusive || mine.mode == LeaseMode::Exclusive)
    }

    /// What this agent is waiting for, if anything. An agent waits for
    /// one thing at a time — a claim holds its connection — so the
    /// oldest is the answer.
    pub fn waiting_for(&self, agent: &AgentId) -> Option<&Waiter> {
        self.waiters.iter().find(|w| w.agent == *agent)
    }

    pub fn all(&self) -> &[Waiter] {
        &self.waiters
    }

    pub fn is_empty(&self) -> bool {
        self.waiters.is_empty()
    }
}

/// One step of a deadlock: an agent, what it is waiting for, and who
/// holds it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Blocked {
    pub agent: AgentId,
    pub resource: ResourceKey,
    pub held_by: AgentId,
}

/// Would `requester` waiting for `resource` close a cycle?
///
/// The graph has an edge from each waiter to every holder of what it
/// waits for. A cycle through the requester means every agent in it is
/// waiting for something another member holds, so none of them can ever
/// proceed: TTLs would eventually break it, but only after everybody has
/// wasted the whole of one.
///
/// The answer is the cycle itself, starting and ending at the requester,
/// so the refusal can say what the problem is rather than that there is
/// one.
///
/// `holders` says who holds each resource; `waits` says what each agent
/// is already waiting for. Both are the daemon's own tables, passed in
/// rather than borrowed, so this stays pure and testable.
pub fn deadlock(
    requester: &AgentId,
    resource: &ResourceKey,
    holders: &[(ResourceKey, AgentId)],
    waits: &[(AgentId, ResourceKey)],
) -> Option<Vec<Blocked>> {
    let waiting: HashMap<&AgentId, &ResourceKey> = waits.iter().map(|(a, r)| (a, r)).collect();
    // Depth-first from the requester, following "waiting for something
    // this agent holds". A path back to the requester is the cycle.
    let mut path = Vec::new();
    let mut seen = HashSet::new();
    seen.insert(requester.clone());
    walk(
        requester, resource, requester, holders, &waiting, &mut path, &mut seen,
    )
    .then_some(path)
}

/// One step of the search: from `from`, which wants `want`, to every
/// agent holding something that overlaps it.
fn walk(
    from: &AgentId,
    want: &ResourceKey,
    requester: &AgentId,
    holders: &[(ResourceKey, AgentId)],
    waiting: &HashMap<&AgentId, &ResourceKey>,
    path: &mut Vec<Blocked>,
    seen: &mut HashSet<AgentId>,
) -> bool {
    for (held, holder) in holders {
        if !held.overlaps(want) {
            continue;
        }
        // Holding something you also want is not a deadlock; it is a
        // renewal, and the lease table settles it.
        if holder == from {
            continue;
        }
        path.push(Blocked {
            agent: from.clone(),
            resource: want.clone(),
            held_by: holder.clone(),
        });
        if holder == requester {
            return true;
        }
        if seen.insert(holder.clone())
            && let Some(next) = waiting.get(holder)
            && walk(holder, next, requester, holders, waiting, path, seen)
        {
            return true;
        }
        path.pop();
    }
    false
}

/// What an agent is actually doing, as far as the working set can say.
///
/// Not a terminal heuristic. A multiplexer has to guess from what a pane
/// last printed; the daemon knows, because every one of these comes from
/// something it recorded: the wait queue says who is blocked and on
/// what, the registry says when an agent last acted through the daemon,
/// and the status says whether it is there at all.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Activity {
    /// The process is present, but no fresh activity evidence is available.
    Unknown,
    /// Registered, no process yet.
    Starting,
    /// Acted through the daemon within the activity window — claimed,
    /// released, observed, reported a tool run, changed a file it holds.
    Working { since: DateTime<Utc> },
    /// Waiting on a claim. The one state where a multiplexer can only
    /// say "blocked" and this can say what by, and who has it.
    Blocked {
        resource: ResourceKey,
        held_by: Vec<AgentId>,
        since: DateTime<Utc>,
    },
    /// Explicitly reported quiet by an adapter; silence alone cannot prove it.
    Idle { since: DateTime<Utc> },
    /// Not running any more.
    Finished,
}

impl Activity {
    /// A word for a column.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Starting => "starting",
            Self::Working { .. } => "working",
            Self::Blocked { .. } => "blocked",
            Self::Idle { .. } => "idle",
            Self::Finished => "finished",
        }
    }
}

/// Explicit adapter observations; process liveness and MCP configuration are
/// not turn observations. No prompts, tool arguments or transcripts are stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportedActivity {
    Working,
    Idle,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityObservation {
    pub activity: ReportedActivity,
    pub observed_at: DateTime<Utc>,
}

impl ActivityObservation {
    /// A missed hook must not leave a permanent working/idle assertion. Long
    /// silent turns become unknown until another provider event arrives.
    pub fn current(&self, now: DateTime<Utc>) -> Option<Activity> {
        let age = now - self.observed_at;
        if age < chrono::Duration::zero() || age >= chrono::Duration::minutes(5) {
            return None;
        }
        Some(match self.activity {
            ReportedActivity::Working => Activity::Working {
                since: self.observed_at,
            },
            ReportedActivity::Idle => Activity::Idle {
                since: self.observed_at,
            },
        })
    }
}

/// One agent's activity, with enough of its identity to show it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentActivity {
    pub agent: AgentId,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<crate::ProjectId>,
    pub activity: Activity,
    /// Durable queue size. None when talking to a daemon predating this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queued_inputs: Option<usize>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activity_observations_expire_instead_of_guessing_idle() {
        let now = now();
        for activity in [ReportedActivity::Working, ReportedActivity::Idle] {
            let observation = ActivityObservation {
                activity,
                observed_at: now,
            };
            assert!(observation.current(now).is_some());
            assert!(
                observation
                    .current(now + chrono::Duration::seconds(299))
                    .is_some()
            );
            assert_eq!(
                observation.current(now + chrono::Duration::minutes(5)),
                None
            );
            assert_eq!(
                observation.current(now - chrono::Duration::seconds(1)),
                None
            );
        }
    }

    fn agent(name: &str) -> AgentId {
        AgentId::from(name)
    }

    fn key(k: &str) -> ResourceKey {
        ResourceKey::new(k)
    }

    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000, 0).unwrap()
    }

    #[test]
    fn the_oldest_waiter_on_an_overlapping_resource_goes_first() {
        let mut queue = WaitQueue::new();
        let first = queue.join(agent("a"), key("task:x"), LeaseMode::Exclusive, now());
        let second = queue.join(agent("b"), key("task:x"), LeaseMode::Exclusive, now());
        assert!(queue.is_next(first));
        assert!(!queue.is_next(second), "a newcomer cannot jump the queue");
        assert_eq!(queue.position(first), Some(0));
        assert_eq!(queue.position(second), Some(1));

        // When the head leaves — claimed, timed out, or gone — the next
        // one is next.
        queue.leave(first);
        assert!(queue.is_next(second));
        assert_eq!(queue.position(second), Some(0));
    }

    #[test]
    fn an_unrelated_resource_is_not_in_the_way() {
        let mut queue = WaitQueue::new();
        queue.join(agent("a"), key("task:x"), LeaseMode::Exclusive, now());
        let other = queue.join(agent("b"), key("task:y"), LeaseMode::Exclusive, now());
        assert!(queue.is_next(other));
        assert_eq!(queue.position(other), Some(0));
    }

    #[test]
    fn a_hierarchical_key_is_in_the_way_of_what_it_contains() {
        let mut queue = WaitQueue::new();
        let dir = queue.join(
            agent("a"),
            key("path:/repo/src"),
            LeaseMode::Exclusive,
            now(),
        );
        let file = queue.join(
            agent("b"),
            key("path:/repo/src/lib.rs"),
            LeaseMode::Exclusive,
            now(),
        );
        assert!(queue.is_next(dir));
        assert!(!queue.is_next(file), "the directory covers the file");
    }

    #[test]
    fn readers_do_not_queue_behind_each_other() {
        let mut queue = WaitQueue::new();
        queue.join(agent("a"), key("task:x"), LeaseMode::Shared, now());
        let second = queue.join(agent("b"), key("task:x"), LeaseMode::Shared, now());
        assert!(
            queue.is_next(second),
            "two shared waiters can be satisfied together"
        );
        // And the number published as its queue place has to agree: a
        // waiter that may proceed at once must not be shown as second.
        assert_eq!(queue.position(second), Some(0));
        // A writer behind them still waits, behind both of them.
        let writer = queue.join(agent("c"), key("task:x"), LeaseMode::Exclusive, now());
        assert!(!queue.is_next(writer));
        assert_eq!(queue.position(writer), Some(2));
    }

    #[test]
    fn a_ticket_that_left_is_nobody() {
        let mut queue = WaitQueue::new();
        let ticket = queue.join(agent("a"), key("task:x"), LeaseMode::Exclusive, now());
        assert_eq!(queue.leave(ticket).map(|w| w.agent), Some(agent("a")));
        assert!(queue.leave(ticket).is_none(), "and only leaves once");
        assert!(!queue.is_next(ticket));
        assert_eq!(queue.position(ticket), None);
        assert!(queue.is_empty());
    }

    #[test]
    fn waiting_for_names_the_one_thing_an_agent_waits_on() {
        let mut queue = WaitQueue::new();
        queue.join(agent("a"), key("task:x"), LeaseMode::Exclusive, now());
        assert_eq!(
            queue.waiting_for(&agent("a")).map(|w| w.resource.clone()),
            Some(key("task:x"))
        );
        assert!(queue.waiting_for(&agent("b")).is_none());
    }

    #[test]
    fn two_agents_each_holding_what_the_other_wants_is_a_deadlock() {
        // a holds x and wants y; b holds y and is about to want x.
        let holders = [(key("task:x"), agent("a")), (key("task:y"), agent("b"))];
        let waits = [(agent("a"), key("task:y"))];
        let cycle = deadlock(&agent("b"), &key("task:x"), &holders, &waits)
            .expect("b waiting for x closes the cycle");
        assert_eq!(
            cycle,
            vec![
                Blocked {
                    agent: agent("b"),
                    resource: key("task:x"),
                    held_by: agent("a"),
                },
                Blocked {
                    agent: agent("a"),
                    resource: key("task:y"),
                    held_by: agent("b"),
                },
            ]
        );
    }

    #[test]
    fn three_agents_round_a_ring_is_a_deadlock() {
        let holders = [
            (key("task:x"), agent("a")),
            (key("task:y"), agent("b")),
            (key("task:z"), agent("c")),
        ];
        let waits = [(agent("a"), key("task:y")), (agent("b"), key("task:z"))];
        let cycle = deadlock(&agent("c"), &key("task:x"), &holders, &waits).expect("a ring");
        assert_eq!(cycle.len(), 3);
        assert_eq!(cycle[0].agent, agent("c"));
        assert_eq!(cycle.last().unwrap().held_by, agent("c"));
    }

    #[test]
    fn plain_contention_is_not_a_deadlock() {
        // a holds x and wants nothing; b waiting for x simply waits.
        let holders = [(key("task:x"), agent("a"))];
        assert!(deadlock(&agent("b"), &key("task:x"), &holders, &[]).is_none());
        // Even when a is waiting, for something nobody holds.
        let waits = [(agent("a"), key("task:free"))];
        assert!(deadlock(&agent("b"), &key("task:x"), &holders, &waits).is_none());
    }

    #[test]
    fn wanting_what_you_already_hold_is_a_renewal_not_a_cycle() {
        let holders = [(key("task:x"), agent("a"))];
        assert!(deadlock(&agent("a"), &key("task:x"), &holders, &[]).is_none());
    }

    #[test]
    fn a_cycle_through_overlapping_paths_is_still_a_cycle() {
        // Hierarchical keys: holding a directory blocks a file under it.
        let holders = [
            (key("path:/repo/src"), agent("a")),
            (key("path:/repo/tests"), agent("b")),
        ];
        let waits = [(agent("a"), key("path:/repo/tests/it.rs"))];
        let cycle = deadlock(&agent("b"), &key("path:/repo/src/lib.rs"), &holders, &waits)
            .expect("the directories overlap what each wants");
        assert_eq!(cycle.len(), 2);
    }

    #[test]
    fn a_long_chain_that_does_not_close_terminates() {
        // A chain of a hundred waiters, none of which reaches back.
        let holders: Vec<_> = (0..100)
            .map(|i| (key(&format!("task:{i}")), agent(&format!("a{i}"))))
            .collect();
        let waits: Vec<_> = (0..99)
            .map(|i| (agent(&format!("a{i}")), key(&format!("task:{}", i + 1))))
            .collect();
        assert!(deadlock(&agent("newcomer"), &key("task:0"), &holders, &waits).is_none());
    }
}
