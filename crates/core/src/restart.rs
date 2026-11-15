//! Whether an ended run comes back. Two answers, kept apart:
//!
//! - [`due`] is the restart policy's own answer for an ended record,
//!   from the record alone: a restart is due (from a time, where the
//!   record says when it ended), or the policy has given up. It claims
//!   nothing about whether a restart is scheduled.
//! - [`view`] is what a reader is told. It joins that answer to what
//!   only the daemon knows: whether its scheduler holds an entry for the
//!   agent, in which [`Phase`], and whether the daemon is fenced for a
//!   handover. "Scheduled" and "starting" are said only on that evidence.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::agent::{AgentRecord, RestartPolicy, restart_delay};

/// The restart policy's answer for an ended record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Due {
    /// The policy restarts it: this attempt, from this time. `at` is
    /// `None` when the record does not say when it ended (one written
    /// before that was kept): the delay's end is unknown, never invented.
    Restart {
        attempt: u32,
        at: Option<DateTime<Utc>>,
    },
    /// The policy would restart an ending like this, but has used up its
    /// restarts.
    GivenUp { attempts: u32 },
}

/// The policy's answer for this record: `None` while it is live, for an
/// agent the daemon does not restart (registered, or a container), and
/// for an ending its policy does not restart.
pub fn due(record: &AgentRecord) -> Option<Due> {
    if !record.managed || record.container.is_some() || record.status.is_live() {
        return None;
    }
    let policy = record.spec.restart;
    if policy.restarts(&record.status, record.restarts) {
        let delay = chrono::Duration::from_std(restart_delay(record.restarts)).ok()?;
        Some(Due::Restart {
            attempt: record.restarts.saturating_add(1),
            at: record.finished_at.map(|finished| finished + delay),
        })
    } else if policy.exhausted(&record.status, record.restarts) {
        Some(Due::GivenUp {
            attempts: record.restarts,
        })
    } else {
        None
    }
}

/// Where the daemon's scheduler entry for one restart stands, and which
/// attempt it was installed for. Only the daemon holds it, in memory. An
/// entry speaks only of its own attempt: once the record has moved on (a
/// failed spawn counted, a restarted child ended again) and the entry is
/// not yet removed, it says nothing of the attempt that follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Installed, waiting out the policy's delay until `due` (unknown
    /// when the record does not say when it ended).
    Delay {
        attempt: u32,
        due: Option<DateTime<Utc>>,
    },
    /// An attempt was refused admission by a transfer at `at`; the task
    /// backs off before it tries again.
    Refused { attempt: u32, at: DateTime<Utc> },
    /// An attempt was admitted and is under way. Evidence of admission,
    /// not that the child runs or that its record will commit.
    Starting { attempt: u32 },
}

impl Phase {
    /// The attempt this entry was installed for.
    pub fn attempt(&self) -> u32 {
        match self {
            Self::Delay { attempt, .. }
            | Self::Refused { attempt, .. }
            | Self::Starting { attempt } => *attempt,
        }
    }
}

/// What a reader is told of whether an ended run comes back, tagged by
/// `state`. A word from a newer daemon reads as `unknown`, and so does a
/// view a daemon did not give (an older one, or a refused read): never
/// as "no restart pending".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RestartView {
    /// The scheduler holds this attempt, waiting out its delay.
    Scheduled {
        attempt: u32,
        /// The policy's limit, for `on-failure`; none for `always`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        of: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        at: Option<DateTime<Utc>>,
    },
    /// This attempt was admitted and is under way.
    Starting { attempt: u32 },
    /// The policy restarts it, but nothing is starting it now, and why.
    Waiting {
        attempt: u32,
        why: Waiting,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        at: Option<DateTime<Utc>>,
    },
    /// This automatic policy has used up its restarts. It says nothing of
    /// a manual relaunch or a restore.
    GivenUp {
        attempts: u32,
        policy: RestartPolicy,
    },
    #[serde(other)]
    Unknown,
}

/// Why a restart the policy wants is not starting now.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Waiting {
    /// The daemon is fenced for a handover now; the restart waits for it.
    HandingOver,
    /// A transfer refused the last attempt; the fence has lifted, and the
    /// retry has not yet been admitted.
    RetryAfterTransfer,
    /// The scheduler holds no entry: before the daemon's maintenance has
    /// reached it, or after its task ended without one.
    NotScheduled,
    #[serde(other)]
    Unknown,
}

/// The view of one record, from the policy's answer, the scheduler's
/// phase for it (if any) and whether the daemon is fenced, all read in
/// the same hold. `None` when nothing applies (a live record, a policy
/// that does not restart this ending, a policy a stop cleared), which a
/// reader shows as no restart.
pub fn view(record: &AgentRecord, phase: Option<Phase>, fenced: bool) -> Option<RestartView> {
    let (attempt, at) = match due(record)? {
        Due::GivenUp { attempts } => {
            return Some(RestartView::GivenUp {
                attempts,
                policy: record.spec.restart,
            });
        }
        Due::Restart { attempt, at } => (attempt, at),
    };
    // An entry for another attempt (the one before, not yet removed) says
    // nothing of this one.
    let phase = phase.filter(|phase| phase.attempt() == attempt);
    let of = match record.spec.restart {
        RestartPolicy::OnFailure { max } => Some(max),
        RestartPolicy::No | RestartPolicy::Always => None,
    };
    Some(match phase {
        None => RestartView::Waiting {
            attempt,
            why: Waiting::NotScheduled,
            at,
        },
        Some(Phase::Starting { .. }) => RestartView::Starting { attempt },
        Some(Phase::Delay { due, .. }) if fenced => RestartView::Waiting {
            attempt,
            why: Waiting::HandingOver,
            at: due,
        },
        Some(Phase::Delay { due, .. }) => RestartView::Scheduled {
            attempt,
            of,
            at: due,
        },
        Some(Phase::Refused { .. }) => RestartView::Waiting {
            attempt,
            why: if fenced {
                Waiting::HandingOver
            } else {
                Waiting::RetryAfterTransfer
            },
            at,
        },
    })
}

/// The run a view was read against, as the record stood in that hold:
/// what an exit, a restart, a failed spawn, a stop or a cleared policy
/// each changes. A reader joins a view to the record it shows only when
/// these are equal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunEvidence {
    pub status: crate::AgentStatus,
    pub restarts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<DateTime<Utc>>,
    pub policy: RestartPolicy,
}

impl RunEvidence {
    pub fn of(record: &AgentRecord) -> Self {
        Self {
            status: record.status.clone(),
            restarts: record.restarts,
            finished_at: record.finished_at,
            policy: record.spec.restart,
        }
    }
}

/// One agent's answer to `restarts`, its view and the run it was read
/// against, from one hold. `view` absent is a known absence for that run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outlook {
    pub run: RunEvidence,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view: Option<RestartView>,
}

/// What a reader knows of the restart of the record it shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Known {
    /// Read against this very run: the view, or none applies.
    Known(Option<RestartView>),
    /// No answer for this run: none came, it was refused, or it was read
    /// against another run of the record. Nothing is said.
    Unknown,
}

impl Outlook {
    /// The answer for `record`, joined by its run: known only when the
    /// outlook was read against the run the record shows.
    pub fn known_for(outlook: Option<&Outlook>, record: &AgentRecord) -> Known {
        match outlook {
            Some(outlook) if outlook.run == RunEvidence::of(record) => Known::Known(outlook.view),
            _ => Known::Unknown,
        }
    }
}

impl RestartView {
    /// What a status line adds after an ended agent's own words. `now`
    /// turns a due time into how long is left.
    pub fn words(&self, now: DateTime<Utc>) -> String {
        let when = |at: &Option<DateTime<Utc>>| match at {
            Some(at) if *at > now => {
                let seconds = (*at - now).num_seconds().max(1);
                format!(" in {seconds}s")
            }
            Some(_) => " now".to_owned(),
            None => String::new(),
        };
        match self {
            Self::Scheduled { attempt, of, at } => {
                let of = of.map(|of| format!(" of {of}")).unwrap_or_default();
                format!("restart {attempt}{of}{}", when(at))
            }
            Self::Starting { attempt } => format!("restart {attempt} starting"),
            Self::Waiting { why, .. } => match why {
                Waiting::HandingOver => "restart waiting: daemon handing over".to_owned(),
                Waiting::RetryAfterTransfer => {
                    "restart waiting: to retry after a handover".to_owned()
                }
                Waiting::NotScheduled => "restart waiting: not scheduled".to_owned(),
                Waiting::Unknown => "restart waiting".to_owned(),
            },
            Self::GivenUp { attempts, policy } => format!(
                "gave up restarting ({}, {attempts} restarts)",
                policy.describe()
            ),
            Self::Unknown => "restart unknown".to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AgentSpec, AgentStatus};

    fn ended(policy: RestartPolicy, status: AgentStatus, restarts: u32) -> AgentRecord {
        let mut record = AgentRecord::new(
            AgentSpec {
                restart: policy,
                ..AgentSpec::default()
            },
            true,
            Utc::now(),
        );
        record.status = status;
        record.restarts = restarts;
        record.finished_at = Some(DateTime::from_timestamp(1_000_000, 0).unwrap());
        record
    }

    /// The policy's answer agrees with the restart decision everywhere: a
    /// restart is due exactly where `restarts` says so, given up exactly
    /// where an ending it would restart has used its count, and nothing
    /// for a live, registered or container agent.
    #[test]
    fn the_due_answer_agrees_with_the_restart_decision() {
        let statuses = [
            AgentStatus::Exited { code: Some(0) },
            AgentStatus::Exited { code: Some(1) },
            AgentStatus::Exited { code: None },
            AgentStatus::Failed {
                reason: "fixture".into(),
            },
        ];
        let policies = [
            RestartPolicy::No,
            RestartPolicy::Always,
            RestartPolicy::OnFailure { max: 0 },
            RestartPolicy::OnFailure { max: 3 },
        ];
        for policy in policies {
            for status in &statuses {
                for restarts in 0..5 {
                    let record = ended(policy, status.clone(), restarts);
                    let case = format!("{policy:?} {status:?} {restarts}");
                    match due(&record) {
                        Some(Due::Restart { attempt, at }) => {
                            assert!(policy.restarts(status, restarts), "{case}");
                            assert_eq!(attempt, restarts + 1, "{case}");
                            let delay =
                                chrono::Duration::from_std(restart_delay(restarts)).unwrap();
                            assert_eq!(at, record.finished_at.map(|f| f + delay), "{case}");
                        }
                        Some(Due::GivenUp { attempts }) => {
                            assert!(!policy.restarts(status, restarts), "{case}");
                            assert!(
                                matches!(policy, RestartPolicy::OnFailure { max } if restarts >= max),
                                "{case}"
                            );
                            assert_ne!(*status, AgentStatus::Exited { code: Some(0) }, "{case}");
                            assert_eq!(attempts, restarts, "{case}");
                        }
                        None => {
                            assert!(!policy.restarts(status, restarts), "{case}");
                            assert!(!policy.exhausted(status, restarts), "{case}");
                        }
                    }
                }
            }
        }
        let mut live = ended(RestartPolicy::Always, AgentStatus::Running, 0);
        assert_eq!(due(&live), None);
        live.status = AgentStatus::Exited { code: Some(1) };
        live.managed = false;
        assert_eq!(due(&live), None, "a registered agent is not restarted");
        let mut unknown = ended(
            RestartPolicy::Always,
            AgentStatus::Exited { code: Some(1) },
            2,
        );
        unknown.finished_at = None;
        assert_eq!(
            due(&unknown),
            Some(Due::Restart {
                attempt: 3,
                at: None
            }),
            "no due time is invented"
        );
    }

    /// Each phase, fenced or not, maps to one view; "scheduled" and
    /// "starting" only with a scheduler entry, "handing over" only under
    /// a fence, and a refusal the fence has lifted from is a retry that
    /// waits, not a handover.
    #[test]
    fn the_view_says_only_what_the_scheduler_and_the_fence_show() {
        let record = ended(
            RestartPolicy::OnFailure { max: 3 },
            AgentStatus::Exited { code: Some(1) },
            1,
        );
        let at = due(&record).and_then(|due| match due {
            Due::Restart { at, .. } => at,
            Due::GivenUp { .. } => None,
        });
        let refused = Phase::Refused {
            attempt: 2,
            at: Utc::now(),
        };
        let rows = [
            (
                None,
                false,
                RestartView::Waiting {
                    attempt: 2,
                    why: Waiting::NotScheduled,
                    at,
                },
            ),
            (
                None,
                true,
                RestartView::Waiting {
                    attempt: 2,
                    why: Waiting::NotScheduled,
                    at,
                },
            ),
            (
                Some(Phase::Delay {
                    attempt: 2,
                    due: at,
                }),
                false,
                RestartView::Scheduled {
                    attempt: 2,
                    of: Some(3),
                    at,
                },
            ),
            (
                Some(Phase::Delay {
                    attempt: 2,
                    due: at,
                }),
                true,
                RestartView::Waiting {
                    attempt: 2,
                    why: Waiting::HandingOver,
                    at,
                },
            ),
            (
                Some(refused),
                false,
                RestartView::Waiting {
                    attempt: 2,
                    why: Waiting::RetryAfterTransfer,
                    at,
                },
            ),
            (
                Some(refused),
                true,
                RestartView::Waiting {
                    attempt: 2,
                    why: Waiting::HandingOver,
                    at,
                },
            ),
            (
                Some(Phase::Starting { attempt: 2 }),
                false,
                RestartView::Starting { attempt: 2 },
            ),
            (
                Some(Phase::Starting { attempt: 2 }),
                true,
                RestartView::Starting { attempt: 2 },
            ),
        ];
        for (phase, fenced, expected) in rows {
            assert_eq!(
                view(&record, phase, fenced),
                Some(expected),
                "{phase:?} fenced={fenced}"
            );
        }
        // An entry left from the attempt before (a failed spawn counted,
        // the entry not yet removed) says nothing of this attempt: not
        // starting, not scheduled, until its own install and admission.
        for stale in [
            Phase::Starting { attempt: 1 },
            Phase::Delay {
                attempt: 1,
                due: at,
            },
            Phase::Refused {
                attempt: 1,
                at: Utc::now(),
            },
        ] {
            assert_eq!(
                view(&record, Some(stale), false),
                Some(RestartView::Waiting {
                    attempt: 2,
                    why: Waiting::NotScheduled,
                    at
                }),
                "{stale:?}"
            );
        }
        // Nothing applies: a live record, a policy a stop cleared.
        let mut live = record.clone();
        live.status = AgentStatus::Running;
        assert_eq!(
            view(&live, Some(Phase::Starting { attempt: 2 }), false),
            None
        );
        let mut cleared = record.clone();
        cleared.spec.restart = RestartPolicy::No;
        assert_eq!(
            view(
                &cleared,
                Some(Phase::Delay {
                    attempt: 2,
                    due: at
                }),
                false
            ),
            None
        );
        // Given up is the record's own, whatever the scheduler holds.
        let spent = ended(
            RestartPolicy::OnFailure { max: 3 },
            AgentStatus::Exited { code: Some(1) },
            3,
        );
        for phase in [None, Some(Phase::Starting { attempt: 2 })] {
            assert_eq!(
                view(&spent, phase, false),
                Some(RestartView::GivenUp {
                    attempts: 3,
                    policy: RestartPolicy::OnFailure { max: 3 }
                })
            );
        }
    }

    /// An answer is joined only to the run it was read against: a failed
    /// spawn, an exit, a restart or a cleared policy between the record
    /// read and the restart read makes it unknown, not a view of another
    /// run, and no answer is unknown, not a known absence.
    #[test]
    fn an_answer_is_known_only_for_the_run_it_was_read_against() {
        let record = ended(
            RestartPolicy::OnFailure { max: 1 },
            AgentStatus::Exited { code: Some(1) },
            0,
        );
        let outlook = Outlook {
            run: RunEvidence::of(&record),
            view: view(&record, None, false),
        };
        assert_eq!(
            Outlook::known_for(Some(&outlook), &record),
            Known::Known(outlook.view)
        );
        assert_eq!(Outlook::known_for(None, &record), Known::Unknown);
        // The scheduled restart failed to spawn between the two reads: a
        // new count, status and finish, and given up. Shown beside the
        // record read first, it is unknown, never "gave up".
        let mut spent = record.clone();
        spent.restarts = 1;
        spent.status = AgentStatus::Failed {
            reason: "spawn".into(),
        };
        spent.finished_at = record.finished_at.map(|f| f + chrono::Duration::seconds(1));
        let later = Outlook {
            run: RunEvidence::of(&spent),
            view: view(&spent, None, false),
        };
        assert!(matches!(later.view, Some(RestartView::GivenUp { .. })));
        assert_eq!(Outlook::known_for(Some(&later), &record), Known::Unknown);
        // A known absence is known only for its own run.
        let mut live = record.clone();
        live.status = AgentStatus::Running;
        let none = Outlook {
            run: RunEvidence::of(&live),
            view: None,
        };
        assert_eq!(Outlook::known_for(Some(&none), &live), Known::Known(None));
        assert_eq!(Outlook::known_for(Some(&none), &record), Known::Unknown);
        let mut cleared = record.clone();
        cleared.spec.restart = RestartPolicy::No;
        assert_eq!(Outlook::known_for(Some(&outlook), &cleared), Known::Unknown);
    }

    /// The view on the wire: tagged by state, unknown words from a newer
    /// daemon read as unknown, and each view has its words.
    #[test]
    fn the_view_travels_tagged_and_reads_unknown_words_as_unknown() {
        let now = DateTime::from_timestamp(1_000_000, 0).unwrap();
        let views = [
            (
                RestartView::Scheduled {
                    attempt: 2,
                    of: Some(3),
                    at: Some(now + chrono::Duration::seconds(4)),
                },
                "scheduled",
                "restart 2 of 3 in 4s",
            ),
            (
                RestartView::Scheduled {
                    attempt: 1,
                    of: None,
                    at: None,
                },
                "scheduled",
                "restart 1",
            ),
            (
                RestartView::Starting { attempt: 2 },
                "starting",
                "restart 2 starting",
            ),
            (
                RestartView::Waiting {
                    attempt: 2,
                    why: Waiting::HandingOver,
                    at: None,
                },
                "waiting",
                "restart waiting: daemon handing over",
            ),
            (
                RestartView::Waiting {
                    attempt: 2,
                    why: Waiting::RetryAfterTransfer,
                    at: Some(now),
                },
                "waiting",
                "restart waiting: to retry after a handover",
            ),
            (
                RestartView::Waiting {
                    attempt: 2,
                    why: Waiting::NotScheduled,
                    at: None,
                },
                "waiting",
                "restart waiting: not scheduled",
            ),
            (
                RestartView::GivenUp {
                    attempts: 3,
                    policy: RestartPolicy::OnFailure { max: 3 },
                },
                "given_up",
                "gave up restarting (on-failure:3, 3 restarts)",
            ),
        ];
        for (view, state, words) in views {
            let json = serde_json::to_value(view).unwrap();
            assert_eq!(json["state"], state);
            assert_eq!(serde_json::from_value::<RestartView>(json).unwrap(), view);
            assert_eq!(view.words(now), words);
        }
        let newer: RestartView =
            serde_json::from_value(serde_json::json!({"state": "a_newer_state"})).unwrap();
        assert_eq!(newer, RestartView::Unknown);
        let newer_why: RestartView = serde_json::from_value(
            serde_json::json!({"state": "waiting", "attempt": 2, "why": "a_newer_reason"}),
        )
        .unwrap();
        assert_eq!(
            newer_why,
            RestartView::Waiting {
                attempt: 2,
                why: Waiting::Unknown,
                at: None
            }
        );
    }
}
