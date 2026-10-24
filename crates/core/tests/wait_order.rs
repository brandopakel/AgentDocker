//! The wait queue's order against a reference: random joins and leaves at
//! random priorities, and after every step the queue is listed in one
//! total order, higher priority first and then arrival; `position` counts
//! exactly the waiters served before a ticket that are in its way; and
//! `is_next` is that count being zero.

use agentdocker_core::{AgentId, LeaseMode, ResourceKey, WaitQueue};
use chrono::Utc;
use proptest::prelude::*;

const KEYS: [&str; 6] = [
    "path:/repo",
    "path:/repo/src",
    "path:/repo/src/a",
    "path:/repository",
    "task:x",
    "task:y",
];

#[derive(Clone, Debug)]
enum Step {
    Join {
        agent: u8,
        key: usize,
        shared: bool,
        priority: i8,
    },
    Leave(usize),
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        3 => (0u8..6, 0..KEYS.len(), any::<bool>(), -9i8..=9).prop_map(
            |(agent, key, shared, priority)| Step::Join { agent, key, shared, priority }
        ),
        1 => any::<usize>().prop_map(Step::Leave),
    ]
}

/// The reference model's own record of one waiter.
#[derive(Clone, Debug)]
struct Expected {
    ticket: u64,
    key: usize,
    shared: bool,
    priority: i8,
}

fn in_the_way(other: &Expected, mine: &Expected) -> bool {
    ResourceKey::new(KEYS[other.key]).overlaps(&ResourceKey::new(KEYS[mine.key]))
        && (!other.shared || !mine.shared)
}

/// Served before: higher priority, or equal and earlier.
fn before(other: &Expected, mine: &Expected) -> bool {
    other.priority > mine.priority
        || (other.priority == mine.priority && other.ticket < mine.ticket)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]
    #[test]
    fn the_queue_serves_by_priority_then_arrival(steps in prop::collection::vec(step(), 1..80)) {
        let mut queue = WaitQueue::new();
        let mut expected: Vec<Expected> = Vec::new();
        let now = Utc::now();
        for step in steps {
            match step {
                Step::Join { agent, key, shared, priority } => {
                    let ticket = queue.join(
                        AgentId::from(format!("agent-{agent}")),
                        ResourceKey::new(KEYS[key]),
                        if shared { LeaseMode::Shared } else { LeaseMode::Exclusive },
                        priority,
                        now,
                    );
                    prop_assert!(expected.iter().all(|e| e.ticket < ticket), "tickets only grow");
                    expected.push(Expected { ticket, key, shared, priority });
                }
                Step::Leave(pick) => {
                    if expected.is_empty() {
                        continue;
                    }
                    let gone = expected.remove(pick % expected.len());
                    prop_assert_eq!(queue.leave(gone.ticket).map(|w| w.ticket), Some(gone.ticket));
                    prop_assert!(queue.leave(gone.ticket).is_none(), "and only once");
                }
            }

            // Listed in one strict order: priority descending, then ticket.
            let listed = queue.all();
            prop_assert_eq!(listed.len(), expected.len());
            for pair in listed.windows(2) {
                prop_assert!(pair[0].ahead_of(&pair[1]), "{:?} before {:?}", pair[0], pair[1]);
                prop_assert!(!pair[1].ahead_of(&pair[0]));
                prop_assert!(
                    pair[0].priority > pair[1].priority
                        || (pair[0].priority == pair[1].priority && pair[0].ticket < pair[1].ticket),
                    "{:?} before {:?}", pair[0], pair[1]
                );
            }

            for mine in &expected {
                let ahead = expected
                    .iter()
                    .filter(|other| before(other, mine) && in_the_way(other, mine))
                    .count();
                prop_assert_eq!(queue.position(mine.ticket), Some(ahead), "ticket {}", mine.ticket);
                prop_assert_eq!(queue.is_next(mine.ticket), ahead == 0, "ticket {}", mine.ticket);
                let listed = listed.iter().find(|w| w.ticket == mine.ticket).unwrap();
                prop_assert_eq!(listed.priority, mine.priority);
            }
        }
    }
}
