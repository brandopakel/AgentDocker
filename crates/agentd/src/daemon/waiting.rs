//! The wait queue, the deadlock check, and what an agent is doing.
//!
//! Three things that turn out to be one thing. Waiting used to be a
//! sleep inside the claim handler: nothing recorded it, so waiters raced
//! when a lease cleared, a ring of agents each holding what the next one
//! wanted could only be broken by TTLs, and "what is this agent doing?"
//! had no better answer than the last line it printed.
//!
//! Recording who waits for what fixes all three. The queue makes waiting
//! fair, the graph over it makes a cycle instantly visible, and an agent
//! in it is blocked on a *named resource held by a named agent* — which
//! is the honest version of the working/blocked/idle a multiplexer has
//! to infer.

use super::*;
use agentdocker_core::wait::{self, Ticket};
use agentdocker_core::{Activity, AgentActivity, Blocked, WaitOutcome};

/// How recently an agent must have acted through the daemon to count as
/// working. Hooks report every tool run and the MCP server every call,
/// so a session that is doing anything touches the daemon far more often
/// than this; a session between turns does not.
const ACTIVE_WINDOW: Duration = Duration::minutes(2);

/// A place in the queue, given up however the wait ends — including when
/// the client simply goes away, which drops the request's future and so
/// this guard with it. Without that, a cancelled claim would sit at the
/// head of a queue forever and starve everyone behind it.
pub(super) struct Waiting<'a> {
    daemon: &'a Daemon,
    ticket: Option<Ticket>,
    resource: ResourceKey,
    requester: AgentId,
}

impl<'a> Waiting<'a> {
    pub(super) fn new(daemon: &'a Daemon, requester: AgentId, resource: ResourceKey) -> Self {
        Self {
            daemon,
            ticket: None,
            resource,
            requester,
        }
    }

    pub(super) fn ticket(&self) -> Option<Ticket> {
        self.ticket
    }

    /// Take a place, announcing where in the queue it landed.
    pub(super) fn join(&mut self, mode: LeaseMode) {
        if self.ticket.is_some() {
            return;
        }
        let mut state = lock(&self.daemon.state);
        let ticket = state.waiting.join(
            self.requester.clone(),
            self.resource.clone(),
            mode,
            Utc::now(),
        );
        self.ticket = Some(ticket);
        let position = state.waiting.position(ticket).unwrap_or(0);
        state.emit(EventKind::LeaseWaiting {
            resource: self.resource.clone(),
            requester: self.requester.clone(),
            position,
        });
    }

    /// Give the place up. Idempotent, so the explicit call on every exit
    /// path and the one in `drop` cannot both count.
    pub(super) fn end(&mut self, outcome: WaitOutcome) {
        let Some(ticket) = self.ticket.take() else {
            return;
        };
        let mut state = lock(&self.daemon.state);
        state.waiting.leave(ticket);
        // Whoever is now at the head of this queue is asleep until
        // something says otherwise, and this is that something.
        state.emit(EventKind::LeaseWaitEnded {
            resource: self.resource.clone(),
            requester: self.requester.clone(),
            outcome,
        });
    }
}

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        self.end(WaitOutcome::Cancelled);
    }
}

impl State {
    /// Whether it is this waiter's turn. A claim with no ticket has not
    /// waited yet and may always try.
    pub(super) fn may_attempt(&self, ticket: Option<Ticket>) -> bool {
        ticket.is_none_or(|ticket| self.waiting.is_next(ticket))
    }

    /// Would waiting for this close a cycle? Built from the two tables
    /// the daemon already keeps, so there is no separate graph to keep
    /// in step with them.
    pub(super) fn deadlock(
        &self,
        requester: &AgentId,
        resource: &ResourceKey,
    ) -> Option<Vec<Blocked>> {
        let holders: Vec<(ResourceKey, AgentId)> = self
            .leases
            .all()
            .into_iter()
            .map(|lease| (lease.resource.clone(), lease.holder.clone()))
            .collect();
        let waits: Vec<(AgentId, ResourceKey)> = self
            .waiting
            .all()
            .iter()
            .map(|w| (w.agent.clone(), w.resource.clone()))
            .collect();
        wait::deadlock(requester, resource, &holders, &waits)
    }

    /// What one agent is doing, from what the daemon recorded rather
    /// than from anything it printed.
    fn activity_of(&self, record: &AgentRecord, now: DateTime<Utc>) -> Activity {
        if !record.status.is_live() {
            return Activity::Finished;
        }
        if record.status == AgentStatus::Created {
            return Activity::Starting;
        }
        if let Some(waiter) = self.waiting.waiting_for(&record.id) {
            return Activity::Blocked {
                resource: waiter.resource.clone(),
                held_by: self
                    .leases
                    .holders_of(&waiter.resource)
                    .into_iter()
                    .map(|lease| lease.holder.clone())
                    .collect(),
                since: waiter.since,
            };
        }
        if now - record.last_seen < ACTIVE_WINDOW {
            Activity::Working {
                since: record.last_seen,
            }
        } else {
            Activity::Idle {
                since: record.last_seen,
            }
        }
    }
}

impl Daemon {
    pub(super) async fn activity(
        self: &Arc<Self>,
        agent: Option<String>,
        project: Option<String>,
        all: bool,
    ) -> Response {
        let project = match project {
            Some(reference) => match self.resolve_project(&reference).await {
                Ok(id) => Some(id),
                Err(response) => return *response,
            },
            None => None,
        };
        let mut state = lock(&self.state);
        let only = match agent.as_deref() {
            Some(reference) => match state.resolve(reference) {
                Ok(id) => Some(id),
                Err(response) => return *response,
            },
            None => None,
        };
        let now = Utc::now();
        let records: Vec<AgentRecord> = state
            .registry
            .all()
            .filter(|a| all || a.status.is_live())
            .filter(|a| only.as_ref().is_none_or(|id| a.id == *id))
            .filter(|a| {
                project
                    .as_ref()
                    .is_none_or(|p| a.project.as_ref().is_some_and(|own| own.id() == *p))
            })
            .cloned()
            .collect();
        let mut activity: Vec<AgentActivity> = records
            .iter()
            .map(|record| AgentActivity {
                agent: record.id.clone(),
                name: record.spec.name.clone(),
                project: record.project.as_ref().map(ProjectRef::id),
                activity: state.activity_of(record, now),
            })
            .collect();
        // Blocked first, because that is the one somebody has to do
        // something about; then working, then the quiet ones.
        activity.sort_by_key(|a| {
            (
                match a.activity {
                    Activity::Blocked { .. } => 0,
                    Activity::Working { .. } => 1,
                    Activity::Starting => 2,
                    Activity::Idle { .. } => 3,
                    Activity::Finished => 4,
                },
                a.name.clone(),
            )
        });
        Response::Activity { activity }
    }

    pub(super) fn waiting(self: &Arc<Self>) -> Response {
        Response::Waiting {
            waiting: lock(&self.state).waiting.all().to_vec(),
        }
    }
}
