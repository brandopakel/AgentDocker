//! Any acyclic set of `after` conditions, any exit statuses, any order of
//! endings: bringing the file up the way `agentdocker up` does starts an
//! entry exactly when everything it waits on started and exited 0, never
//! leaves one undecided, and says of each entry not started what failed
//! at the head of its chain.
use std::collections::BTreeMap;

use agentdocker_core::RestartPolicy;
use agentdocker_core::agentfile::{self, After, Ending, Entry, Progress, Readiness};
use proptest::prelude::*;

/// For each entry by rank, the lower ranks it waits on; each one's exit
/// status; the order the file lists ranks in; the order running agents
/// end in.
type Plan = (Vec<Vec<usize>>, Vec<i32>, Vec<usize>, Vec<usize>);

fn plan() -> impl Strategy<Value = Plan> {
    (1usize..9).prop_flat_map(|n| {
        let waits: Vec<BoxedStrategy<Vec<usize>>> = (0..n)
            .map(|rank| {
                if rank == 0 {
                    Just(Vec::new()).boxed()
                } else {
                    prop::collection::vec(0..rank, 0..=rank.min(3)).boxed()
                }
            })
            .collect();
        let ranks: Vec<usize> = (0..n).collect();
        (
            waits,
            prop::collection::vec(0i32..3, n),
            Just(ranks.clone()).prop_shuffle(),
            Just(ranks).prop_shuffle(),
        )
    })
}

fn name(rank: usize) -> String {
    format!("e{rank}")
}

/// Decide every pending entry in order until a pass changes nothing.
fn decide(entries: &[Entry], order: &[usize], known: &mut BTreeMap<String, Progress>) {
    loop {
        let mut changed = false;
        for &at in order {
            let entry = &entries[at];
            if known[&entry.name] != Progress::Pending {
                continue;
            }
            match agentfile::readiness(entry, known) {
                Readiness::Ready => {
                    known.insert(entry.name.clone(), Progress::Started);
                    changed = true;
                }
                Readiness::Waiting(on) => assert!(!on.is_empty()),
                Readiness::Blocked { reason, ending } => {
                    // The reason names what this entry waited on.
                    assert!(
                        entry
                            .after
                            .iter()
                            .any(|after| reason.starts_with(&format!("{} ", after.name))),
                        "{reason}"
                    );
                    known.insert(entry.name.clone(), Progress::Ended(ending));
                    changed = true;
                }
            }
        }
        if !changed {
            return;
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]
    #[test]
    fn an_entry_starts_exactly_when_what_it_waits_on_succeeded((waits, codes, file, ends) in plan()) {
        let entries: Vec<Entry> = file
            .iter()
            .map(|&rank| Entry {
                name: name(rank),
                depends_on: Vec::new(),
                after: waits[rank].iter().map(|&j| After { name: name(j) }).collect(),
                restart: RestartPolicy::No,
            })
            .collect();
        prop_assert_eq!(agentfile::check(&entries), Ok(()));

        // What is waited on comes first in the order.
        let order = agentfile::order(&entries);
        prop_assert_eq!(order.len(), entries.len());
        let position: BTreeMap<&str, usize> = order
            .iter()
            .enumerate()
            .map(|(at, &i)| (entries[i].name.as_str(), at))
            .collect();
        for entry in &entries {
            for after in &entry.after {
                prop_assert!(position[after.name.as_str()] < position[entry.name.as_str()]);
            }
        }

        let mut known: BTreeMap<String, Progress> = entries
            .iter()
            .map(|entry| (entry.name.clone(), Progress::Pending))
            .collect();
        decide(&entries, &order, &mut known);
        // Running agents end one at a time, in the generated order.
        while let Some(&rank) = ends.iter().find(|&&rank| known[&name(rank)] == Progress::Started) {
            known.insert(
                name(rank),
                Progress::Ended(Ending::Exited { code: Some(codes[rank]) }),
            );
            decide(&entries, &order, &mut known);
        }

        // Ranks ascend along every wait, so one pass computes the model.
        let mut runs = vec![false; waits.len()];
        for rank in 0..waits.len() {
            runs[rank] = waits[rank].iter().all(|&j| runs[j] && codes[j] == 0);
        }
        for rank in 0..waits.len() {
            match &known[&name(rank)] {
                Progress::Ended(Ending::Exited { code }) => {
                    prop_assert!(runs[rank], "e{} ran", rank);
                    prop_assert_eq!(*code, Some(codes[rank]));
                }
                Progress::Ended(Ending::NotStarted { cause }) => {
                    prop_assert!(!runs[rank], "e{} was not started: {}", rank, cause);
                    // The cause is a failure at the head of the chain: an
                    // entry that ran and exited otherwise than 0.
                    let (head, code) = cause.split_once(" exited ").unwrap();
                    let head: usize = head.trim_start_matches('e').parse().unwrap();
                    prop_assert!(runs[head]);
                    prop_assert_ne!(codes[head], 0);
                    prop_assert_eq!(code, codes[head].to_string());
                }
                other => prop_assert!(false, "e{} left {:?}", rank, other),
            }
        }
    }
}
