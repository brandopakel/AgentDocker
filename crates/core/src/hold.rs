//! A card's hold: what its own assignment stands at, beside every live
//! lease on it. The two are independent evidence and neither replaces the
//! other: a card whose named holder's hold has lapsed is still lapsed while
//! somebody else holds a lease on it, and that lease is reported beside the
//! claim, never in its place. Whether a new claim on the card succeeds is
//! still the lease table's to decide; this says what the card's assignment
//! is, for every reader that has to say or act on it.
//!
//! A hold is derived, never stored: from the card as it stands, the leases
//! on its `task:` resource, and the presence of the agent it names. The
//! operations that act on a card ([`crate::Task::pull`],
//! [`crate::Task::take_over`], [`crate::Task::start`]) derive it
//! themselves from the evidence and the card under the lock that commits
//! them, so a hold derived earlier can never authorize a change to a card
//! that has changed since.

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::lease::{Lease, LeaseMode};
use crate::{AgentId, Task};

/// Whether the agent a card names is here, as its record says.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Presence {
    /// Its record is live.
    Live,
    /// Its record has ended.
    Ended,
    /// No record of it is kept.
    Forgotten,
    /// Not known: a view that has not yet read the records. Never worded
    /// as ended or forgotten.
    Unknown,
}

impl Presence {
    /// The holder is known to be gone: ended, or no record kept.
    pub fn gone(self) -> bool {
        matches!(self, Presence::Ended | Presence::Forgotten)
    }
}

/// What a card's hold is derived from: the live leases on its `task:`
/// resource, and the presence of the agent it was read for. Presence read
/// for an earlier assignee says nothing of the current one: a hold derived
/// from a card that names somebody else takes its holder as unknown.
#[derive(Clone, Copy, Debug)]
pub struct Evidence<'a> {
    /// The leases on the card's `task:` resource, as found; those expired
    /// by `now` are left out where a hold is derived.
    pub leases: &'a [Lease],
    /// Whom `presence` is about.
    pub holder: Option<&'a AgentId>,
    pub presence: Presence,
    pub now: DateTime<Utc>,
}

impl<'a> Evidence<'a> {
    /// No leases and no word of anybody's presence.
    pub fn none(now: DateTime<Utc>) -> Self {
        Self {
            leases: &[],
            holder: None,
            presence: Presence::Unknown,
            now,
        }
    }
}

/// What a card's own assignment stands at.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Claim {
    /// It names nobody, and is open and on the board.
    Free,
    /// It names a holder that holds no live exclusive lease on it, and it
    /// is not being worked (Backlog, Ready, Blocked).
    Named { holder: AgentId, presence: Presence },
    /// Its named holder's exclusive lease is live, until then.
    Held {
        holder: AgentId,
        until: DateTime<Utc>,
    },
    /// It is being worked (In progress, Review) and names a holder that
    /// holds no live exclusive lease on it.
    Lapsed { holder: AgentId, presence: Presence },
    /// Done or cancelled; whom it named is kept for the record.
    Ended { holder: Option<AgentId> },
    /// Off the board; whom it named is kept for the record.
    Archived { holder: Option<AgentId> },
}

impl Claim {
    /// Its word, as a refusal's `details.state` says it.
    pub fn word(&self) -> &'static str {
        match self {
            Claim::Free => "free",
            Claim::Named { .. } => "named",
            Claim::Held { .. } => "held",
            Claim::Lapsed { .. } => "lapsed",
            Claim::Ended { .. } => "ended",
            Claim::Archived { .. } => "archived",
        }
    }

    /// The agent the card names, if any.
    pub fn holder(&self) -> Option<&AgentId> {
        match self {
            Claim::Free => None,
            Claim::Named { holder, .. }
            | Claim::Held { holder, .. }
            | Claim::Lapsed { holder, .. } => Some(holder),
            Claim::Ended { holder } | Claim::Archived { holder } => holder.as_ref(),
        }
    }
}

/// A card's hold: its claim, and every live lease on it, whole.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hold {
    pub claim: Claim,
    /// Every lease on the card live at the evidence's `now`: its holder's
    /// and any other, each with its holder, actor and mode.
    pub leases: Vec<Lease>,
    /// The named holder's live exclusive lease, whatever the claim: an
    /// ended or archived card can still carry one claimed by hand.
    holder_lease: Option<Lease>,
    /// The card is being worked and names a holder without that lease,
    /// whatever else is true of it: the hold a take-over takes.
    lapsed: Option<AgentId>,
}

impl Hold {
    /// Derive a card's hold from the evidence. Presence counts only when
    /// it was read for the agent the card names now.
    pub fn of(task: &Task, evidence: &Evidence) -> Hold {
        let leases: Vec<Lease> = evidence
            .leases
            .iter()
            .filter(|lease| !lease.is_expired(evidence.now))
            .cloned()
            .collect();
        let holder_lease = task.assignee.as_ref().and_then(|holder| {
            leases
                .iter()
                .find(|lease| &lease.holder == holder && lease.mode == LeaseMode::Exclusive)
                .cloned()
        });
        let presence = match (&task.assignee, evidence.holder) {
            (Some(named), Some(read)) if named == read => evidence.presence,
            _ => Presence::Unknown,
        };
        let lapsed = task
            .assignee
            .clone()
            .filter(|_| holder_lease.is_none() && task.column.working());
        let claim = match &task.assignee {
            _ if task.archived_at.is_some() => Claim::Archived {
                holder: task.assignee.clone(),
            },
            _ if task.column.terminal() => Claim::Ended {
                holder: task.assignee.clone(),
            },
            None => Claim::Free,
            Some(holder) => match &holder_lease {
                Some(lease) => Claim::Held {
                    holder: holder.clone(),
                    until: lease.expires_at,
                },
                None if task.column.working() => Claim::Lapsed {
                    holder: holder.clone(),
                    presence,
                },
                None => Claim::Named {
                    holder: holder.clone(),
                    presence,
                },
            },
        };
        Hold {
            claim,
            leases,
            holder_lease,
            lapsed,
        }
    }

    /// The named holder's live exclusive lease, whatever the claim says.
    pub fn holder_lease(&self) -> Option<&Lease> {
        self.holder_lease.as_ref()
    }

    /// The holder whose hold a take-over takes: the card is being worked
    /// and names a holder without its lease. Independent of the claim, so
    /// an archived card that is still being worked answers as it always
    /// has: a take-over is then refused because it is archived.
    pub fn lapsed(&self) -> Option<&AgentId> {
        self.lapsed.as_ref()
    }

    /// `details.hold` as a refusal has always said it, now only where it
    /// is true: `live` while the named holder's exclusive lease is live,
    /// `lapsed` while a worked card names a holder without one (another
    /// agent's lease beside it changes nothing), and nothing otherwise.
    pub fn lease_word(&self) -> Option<&'static str> {
        if self.lapsed.is_some() {
            Some("lapsed")
        } else if self.holder_lease.is_some() {
            Some("live")
        } else {
            None
        }
    }
}

/// The hold-related step that would get past a refusal. It says nothing
/// of what else a card waits on: its blockers still apply.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum Step {
    /// Name the card: a pull with no card named takes only from the pull
    /// source.
    PullByName,
    /// The caller's own card outside the pull source: move it there, and
    /// pull it again.
    MoveToSource { lane: String },
    /// Take the lapsed hold over by name.
    TakeOverFrom { holder: AgentId },
    /// A live hold: wait for it, or ask its holder.
    Wait { until: Option<DateTime<Utc>> },
    /// Another's assignment: a project member moving the card to a
    /// waiting lane releases it, or the person hands it on.
    Release,
    /// Done or cancelled: move it to an open lane first.
    Reopen,
    /// Off the board: the person brings it back.
    Unarchive,
}

impl Step {
    /// Its word, as a refusal's `details.step` says it.
    pub fn word(&self) -> &'static str {
        match self {
            Step::PullByName => "pull_by_name",
            Step::MoveToSource { .. } => "move_to_source",
            Step::TakeOverFrom { .. } => "take_over_from",
            Step::Wait { .. } => "wait",
            Step::Release => "release",
            Step::Reopen => "reopen",
            Step::Unarchive => "unarchive",
        }
    }

    /// What it says, after a refusal's own words.
    pub fn sentence(&self) -> String {
        match self {
            Step::PullByName => "name the card to take it from where it sits".to_owned(),
            Step::MoveToSource { lane } => {
                format!("it is yours: move it to {lane} and pull it again")
            }
            Step::TakeOverFrom { holder } => {
                format!("to take it over, pull again with take_over_from naming {holder}")
            }
            Step::Wait {
                until: Some(until),
            } => format!(
                "its hold is live until {}; wait, or ask its holder",
                until.format("%Y-%m-%d %H:%M UTC")
            ),
            Step::Wait { until: None } => "its hold is live; wait, or ask its holder".to_owned(),
            Step::Release => "a project member moving it to a waiting lane releases it, or the person hands it on".to_owned(),
            Step::Reopen => "move it to an open lane first".to_owned(),
            Step::Unarchive => "only the person brings it back".to_owned(),
        }
    }

    /// The step past a card somebody else holds, as its claim says.
    pub fn past(claim: &Claim) -> Step {
        match claim {
            Claim::Free => Step::PullByName,
            Claim::Named { .. } => Step::Release,
            Claim::Held { until, .. } => Step::Wait {
                until: Some(*until),
            },
            Claim::Lapsed { holder, .. } => Step::TakeOverFrom {
                holder: holder.clone(),
            },
            Claim::Ended { .. } => Step::Reopen,
            Claim::Archived { .. } => Step::Unarchive,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lease::{LeaseId, ResourceKey};
    use crate::{Column, ProjectId};
    use chrono::Duration;

    fn card(column: Column, assignee: Option<&AgentId>) -> Task {
        let mut task = Task::new(
            ProjectId::from("p"),
            "Fix login",
            "",
            Some(column),
            "user",
            Vec::new(),
            Utc::now(),
        )
        .unwrap();
        task.column = column;
        task.assignee = assignee.cloned();
        task
    }

    fn lease(task: &Task, holder: &AgentId, mode: LeaseMode, until: DateTime<Utc>) -> Lease {
        Lease {
            id: LeaseId::generate(),
            resource: ResourceKey::new(format!("task:{}", task.id)),
            holder: holder.clone(),
            mode,
            acquired_at: until - Duration::hours(4),
            held_since: None,
            change_seq: None,
            expires_at: until,
            note: None,
            amount: 0,
            automatic: false,
            actor: None,
        }
    }

    /// A card's claim is its own assignment, said from its column, whom it
    /// names, that holder's exclusive lease and presence; nobody else's
    /// lease changes it, and every live lease is kept beside it, whole.
    #[test]
    fn a_cards_claim_is_its_assignment_and_the_leases_sit_beside_it() {
        let now = Utc::now();
        let a = AgentId::from("a".to_owned());
        let b = AgentId::from("b".to_owned());
        let later = now + Duration::hours(1);
        let of = |task: &Task, leases: &[Lease], presence: Presence| {
            Hold::of(
                task,
                &Evidence {
                    leases,
                    holder: task.assignee.as_ref(),
                    presence,
                    now,
                },
            )
        };
        use Column::*;
        // Nobody named: free, open or not.
        assert_eq!(
            of(&card(Ready, None), &[], Presence::Unknown).claim,
            Claim::Free
        );
        assert_eq!(
            of(&card(Done, None), &[], Presence::Unknown).claim,
            Claim::Ended { holder: None }
        );
        // Named, not being worked, no lease: named, with the holder's presence.
        for column in [Backlog, Ready, Blocked] {
            for presence in [
                Presence::Live,
                Presence::Ended,
                Presence::Forgotten,
                Presence::Unknown,
            ] {
                let hold = of(&card(column, Some(&a)), &[], presence);
                assert_eq!(
                    hold.claim,
                    Claim::Named {
                        holder: a.clone(),
                        presence
                    },
                    "{column:?} {presence:?}"
                );
                assert_eq!(hold.lease_word(), None, "no lease, nothing live");
                assert_eq!(hold.lapsed(), None);
            }
        }
        // Being worked: held while the holder's exclusive lease lives,
        // lapsed from the instant it expires.
        for column in [InProgress, Review] {
            let task = card(column, Some(&a));
            let held = of(
                &task,
                &[lease(&task, &a, LeaseMode::Exclusive, later)],
                Presence::Live,
            );
            assert_eq!(
                held.claim,
                Claim::Held {
                    holder: a.clone(),
                    until: later
                }
            );
            assert_eq!(held.lease_word(), Some("live"));
            let at_expiry = of(
                &task,
                &[lease(&task, &a, LeaseMode::Exclusive, now)],
                Presence::Live,
            );
            assert_eq!(
                at_expiry.claim,
                Claim::Lapsed {
                    holder: a.clone(),
                    presence: Presence::Live
                }
            );
            assert!(at_expiry.leases.is_empty(), "an expired lease is not live");
            let just_before = of(
                &task,
                &[lease(
                    &task,
                    &a,
                    LeaseMode::Exclusive,
                    now + Duration::milliseconds(1),
                )],
                Presence::Live,
            );
            assert!(matches!(just_before.claim, Claim::Held { .. }));
            // Another agent's lease leaves the holder's lapsed hold lapsed,
            // and is kept beside it.
            let theirs = lease(&task, &b, LeaseMode::Shared, later);
            let shared = of(&task, std::slice::from_ref(&theirs), Presence::Ended);
            assert_eq!(
                shared.claim,
                Claim::Lapsed {
                    holder: a.clone(),
                    presence: Presence::Ended
                }
            );
            assert_eq!(shared.leases, [theirs]);
            assert_eq!(shared.lapsed(), Some(&a));
            assert_eq!(shared.lease_word(), Some("lapsed"));
            // The holder's own shared claim is not a hold.
            let own_shared = of(
                &task,
                &[lease(&task, &a, LeaseMode::Shared, later)],
                Presence::Live,
            );
            assert!(matches!(own_shared.claim, Claim::Lapsed { .. }));
        }
        // A holder's exclusive lease on a card not being worked: held.
        let task = card(Ready, Some(&a));
        assert!(matches!(
            of(
                &task,
                &[lease(&task, &a, LeaseMode::Exclusive, later)],
                Presence::Live
            )
            .claim,
            Claim::Held { .. }
        ));
        // Ended and archived name whom they named, and keep the holder's
        // lease as evidence: a renewal of one's own lease still sees it.
        let task = card(Done, Some(&a));
        let ended = of(
            &task,
            &[lease(&task, &a, LeaseMode::Exclusive, later)],
            Presence::Live,
        );
        assert_eq!(
            ended.claim,
            Claim::Ended {
                holder: Some(a.clone())
            }
        );
        assert!(ended.holder_lease().is_some());
        let mut archived = card(InProgress, Some(&a));
        archived.archived_at = Some(now);
        let hold = of(&archived, &[], Presence::Live);
        assert_eq!(
            hold.claim,
            Claim::Archived {
                holder: Some(a.clone())
            }
        );
        assert_eq!(hold.lapsed(), Some(&a), "worked, without its lease");
        // Somebody else's lease on a free card: free, the lease beside it.
        let task = card(Ready, None);
        let by_hand = of(
            &task,
            &[lease(&task, &b, LeaseMode::Exclusive, later)],
            Presence::Unknown,
        );
        assert_eq!(by_hand.claim, Claim::Free);
        assert_eq!(by_hand.leases.len(), 1);
        assert_eq!(by_hand.lease_word(), None);
    }

    /// Presence read for one agent says nothing of another: a card that
    /// names somebody else since takes its holder as unknown.
    #[test]
    fn presence_counts_only_for_the_agent_it_was_read_for() {
        let now = Utc::now();
        let a = AgentId::from("a".to_owned());
        let b = AgentId::from("b".to_owned());
        let task = card(Column::Ready, Some(&a));
        let stale = Evidence {
            leases: &[],
            holder: Some(&b),
            presence: Presence::Ended,
            now,
        };
        assert_eq!(
            Hold::of(&task, &stale).claim,
            Claim::Named {
                holder: a,
                presence: Presence::Unknown
            }
        );
    }

    #[test]
    fn each_claim_and_step_has_its_word() {
        let a = AgentId::from("a".to_owned());
        let words: Vec<&str> = [
            Claim::Free,
            Claim::Named {
                holder: a.clone(),
                presence: Presence::Live,
            },
            Claim::Held {
                holder: a.clone(),
                until: Utc::now(),
            },
            Claim::Lapsed {
                holder: a.clone(),
                presence: Presence::Live,
            },
            Claim::Ended { holder: None },
            Claim::Archived { holder: None },
        ]
        .iter()
        .map(Claim::word)
        .collect();
        assert_eq!(
            words,
            ["free", "named", "held", "lapsed", "ended", "archived"]
        );
        assert_eq!(
            Step::past(&Claim::Lapsed {
                holder: a.clone(),
                presence: Presence::Ended
            }),
            Step::TakeOverFrom { holder: a }
        );
    }
}
