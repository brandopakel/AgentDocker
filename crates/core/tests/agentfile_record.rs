//! Any lifecycle of one managed agent — exits with any status, a stop
//! asked at any point (which clears its restart policy, as `stop` does),
//! restarts by its policy — kept on its record the way the daemon keeps it
//! (`mark_stopping` with `agent_stopping`, `begin_run` with every restart),
//! by a daemon that keeps the mark from the start or only from an upgrade
//! partway through (before it, stops and restarts leave the mark alone, as
//! a daemon that does not know it does). Once the agent has ended for good:
//! a record whose mark is known judges it exactly as the complete, ordered
//! event stream does, and one whose mark is unknown never judges success
//! the events would not. While it is live, a known mark is exactly
//! `stopping`. This is what lets `agentdocker up` take an ending on record
//! for an entry it did not watch.
use agentdocker_core::agentfile::{Ending, Seen};
use agentdocker_core::{AgentRecord, AgentSpec, AgentStatus, EventKind, RestartPolicy};
use proptest::prelude::*;

#[derive(Clone, Debug)]
enum Step {
    /// Its process ends with this status (`None`: a signal).
    Exit(Option<i32>),
    /// Somebody asks for it to stop.
    Stop,
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        3 => prop_oneof![Just(None), (0i32..3).prop_map(Some)].prop_map(Step::Exit),
        1 => Just(Step::Stop),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]
    #[test]
    fn an_ended_agents_record_judges_it_as_its_events_do(
        max in 0u32..4,
        // Steps before this one run under a daemon that does not keep the
        // mark; zero is a daemon that keeps it from the agent's creation.
        upgrade_at in prop_oneof![2 => Just(0usize), 1 => 1usize..16],
        steps in prop::collection::vec(step(), 1..16),
    ) {
        let launched = AgentSpec {
            name: "build".into(),
            command: vec!["build".into()],
            restart: if max == 0 {
                RestartPolicy::No
            } else {
                RestartPolicy::OnFailure { max }
            },
            ..AgentSpec::default()
        };
        let now = chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let mut record = AgentRecord::new(launched.clone(), true, now);
        record.status = AgentStatus::Running;
        if upgrade_at > 0 {
            record.stop_requested = None;
        }
        let id = record.id.clone();
        let mut seen = Seen::new(&record);
        let mut ended = false;
        for (at, step) in steps.into_iter().enumerate() {
            let keeps = at >= upgrade_at;
            let mut published = Vec::new();
            match step {
                Step::Stop if record.status == AgentStatus::Running => {
                    // `stop` clears the restart policy first, then commits
                    // `stopping` with its event.
                    if !record.spec.restart.is_no() {
                        record.spec.restart = RestartPolicy::No;
                        published.push(EventKind::AgentRestartCleared { agent: id.clone() });
                    }
                    if keeps {
                        record.mark_stopping();
                    } else {
                        record.status = AgentStatus::Stopping;
                    }
                    published.push(EventKind::AgentStopping {
                        agent: id.clone(),
                        force: false,
                    });
                }
                Step::Stop => continue,
                Step::Exit(code) => {
                    let status = AgentStatus::Exited { code };
                    record.status = status.clone();
                    published.push(EventKind::AgentExited {
                        agent: id.clone(),
                        status: status.clone(),
                    });
                    if record.spec.restart.restarts(&status, record.restarts) {
                        record.restarts += 1;
                        record.status = AgentStatus::Running;
                        if keeps {
                            record.begin_run();
                        }
                        published.push(EventKind::AgentRestarted {
                            agent: id.clone(),
                            pid: None,
                            attempt: record.restarts,
                        });
                    }
                }
            }
            for kind in &published {
                seen.event(kind);
            }
            let known = record.stop_requested.is_some();
            let from_record = Ending::before(&record, &launched);
            if record.status.is_live() {
                prop_assert_eq!(from_record, None);
                if known {
                    prop_assert_eq!(
                        record.stopped_on_request(),
                        record.status == AgentStatus::Stopping
                    );
                }
                continue;
            }
            let from_events = Ending::judged(&record, seen, true);
            prop_assert!(from_events.is_some(), "every event was taken: {:?}", record);
            if known {
                prop_assert_eq!(from_record, from_events);
            } else if from_record.as_ref().is_some_and(Ending::succeeded) {
                prop_assert!(
                    from_events.as_ref().is_some_and(Ending::succeeded),
                    "a record without the mark judged success the events did not: {:?}",
                    from_events
                );
            }
            ended = true;
            break;
        }
        if !ended {
            prop_assert!(record.status.is_live());
        }
    }
}
