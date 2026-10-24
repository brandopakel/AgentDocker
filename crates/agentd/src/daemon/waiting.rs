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
use agentdocker_core::wait::{self, Priority, Ticket};
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

    /// Wait as `requester` instead, before any place is taken: the record
    /// a claim resolved was folded into the one that stays. A record with
    /// a place in a queue is never folded, so there is no ticket to move.
    pub(super) fn follow(&mut self, requester: AgentId) {
        debug_assert!(self.ticket.is_none(), "a waiting record is not folded");
        if self.ticket.is_none() {
            self.requester = requester;
        }
    }

    /// Take a place, announcing where in the queue it landed.
    ///
    /// Takes the lock the caller already holds. The deadlock check and
    /// this join have to happen without letting go of it: the daemon
    /// serves each connection on its own task, so two claims that both
    /// looked before either joined would each see no cycle and both wait
    /// out their deadlines instead of the ring being broken at one.
    pub(super) fn join_locked(&mut self, state: &mut State, mode: LeaseMode, priority: Priority) {
        if self.ticket.is_some() {
            return;
        }
        let ticket = state.waiting.join(
            self.requester.clone(),
            self.resource.clone(),
            mode,
            priority,
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

    /// Say that a wait ended without one ever having begun — the
    /// deadlock refusal, which never joins the queue but is still a wait
    /// that ended, and whose outcome subscribers are told to expect.
    pub(super) fn never_started(&self, state: &mut State, outcome: WaitOutcome) {
        state.emit(EventKind::LeaseWaitEnded {
            resource: self.resource.clone(),
            requester: self.requester.clone(),
            outcome,
        });
    }

    /// Give the place up, in the hold that decided the claim's answer.
    ///
    /// Every answer a waiting claim gives — a lease, a timeout, a refusal —
    /// is decided under the state lock, and its place has to go in that
    /// same hold. Given up later, it would stay in the queue after the
    /// claim had its answer, where another claim could pick it as a ring's
    /// victim: a granted claim announced as deadlocked, or a deadlocked
    /// one answering something else. The caller has seen
    /// [`Self::deadlocked`] answer nothing in this hold, so the wait is
    /// still queued and this says it ended. Idempotent, so the guard's
    /// drop cannot count it twice.
    pub(super) fn end_locked(&mut self, state: &mut State, outcome: WaitOutcome) {
        let Some(ticket) = self.ticket.take() else {
            return;
        };
        // A wait another claim already ended to break a ring was said to
        // end then, and is not said again.
        if state.waiting.leave(ticket).is_none() {
            return;
        }
        // Whoever is now at the head of this queue is asleep until
        // something says otherwise, and this is that something.
        state.emit(EventKind::LeaseWaitEnded {
            resource: self.resource.clone(),
            requester: self.requester.clone(),
            outcome,
        });
    }

    /// The ring this wait was ended to break, when another claim chose it
    /// as the ring's victim. Its place is already gone and its
    /// `lease_wait_ended` already said; this forgets the ticket, so the
    /// claim answers `deadlock` and nothing is said twice.
    pub(super) fn deadlocked(&mut self, state: &mut State) -> Option<Vec<Blocked>> {
        let ticket = self.ticket?;
        let cycle = state.waiting.deadlocked(ticket)?.to_vec();
        state.waiting.leave(ticket);
        self.ticket = None;
        Some(cycle)
    }
}

/// Only a claim that went away without an answer — its client gone, its
/// future dropped — still holds a place here; every answer gives the
/// place up in its own hold ([`Waiting::end_locked`]).
impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        if self.ticket.is_none() {
            return;
        }
        let mut state = lock(&self.daemon.state);
        self.end_locked(&mut state, WaitOutcome::Cancelled);
    }
}

/// Where a test may hold one agent's claim (`Daemon::claim_hold`).
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ClaimPoint {
    /// Woken from its wait (or out of time), before the hold that decides
    /// what the wake means.
    Woken,
    /// After that hold, before the claim answers what it decided.
    Decided,
    /// After the hold in which an attempt gave the claim its answer — a
    /// lease, say — before it is returned.
    Answered,
}

/// A test's hold: the point, the agent whose claim stops there, and the
/// barrier it waits at twice — once to say it arrived, once to go on.
#[cfg(test)]
pub(super) type ClaimHold = (ClaimPoint, AgentId, Arc<tokio::sync::Barrier>);

#[cfg(test)]
impl Daemon {
    /// Stop here if a test asked this agent's claim to stop at `point`.
    pub(super) async fn claim_pause(&self, point: ClaimPoint, agent: &AgentId) {
        let hold = {
            let mut hold = lock(&self.claim_hold);
            if hold
                .as_ref()
                .is_some_and(|(at, who, _)| *at == point && who == agent)
            {
                hold.take()
            } else {
                None
            }
        };
        if let Some((_, _, barrier)) = hold {
            barrier.wait().await;
            barrier.wait().await;
        }
    }
}

impl State {
    pub(super) fn report_adapter(
        &mut self,
        reference: &str,
        adapter: agentdocker_core::AdapterKind,
        contact: agentdocker_core::AdapterContact,
        now: DateTime<Utc>,
    ) -> Response {
        let id = match self.resolve(reference) {
            Ok(id) => id,
            Err(response) => return *response,
        };
        let mut record = self.registry.get(&id).expect("resolved agent").clone();
        if !record.status.is_live()
            || record.spec.runtime == agentdocker_core::HUMAN_RUNTIME
            || !contact.current_for(record.process_started_at, now)
            || (adapter == agentdocker_core::AdapterKind::Hooks
                && !agentdocker_core::runtime::RUNTIMES
                    .iter()
                    .any(|runtime| runtime.name == record.spec.runtime && runtime.hooks))
        {
            return Response::error(
                ErrorCode::Invalid,
                "adapter contact has no matching live process or is stale",
            );
        }
        if record
            .adapter_contacts
            .get(&adapter)
            .is_some_and(|previous| {
                previous.process_started_at == contact.process_started_at
                    && contact.observed_at - previous.observed_at < Duration::seconds(30)
            })
        {
            return Response::Ok;
        }
        record.adapter_contacts.insert(adapter, contact.clone());
        let event = agentdocker_core::Event::new(
            agentdocker_core::EventKind::AdapterContactReported {
                agent: id.clone(),
                adapter,
                contact,
            },
            now,
        );
        let _ = self.transition(
            "adapter contact",
            event,
            |store, event| store.agent_transition(&record, event),
            |state| {
                *state.registry.get_mut(&id).expect("resolved agent") = record.clone();
            },
        );
        self.write_failure().unwrap_or(Response::Ok)
    }

    pub(super) fn report_input(
        &mut self,
        reference: &str,
        process_started_at: DateTime<Utc>,
        observed_at: DateTime<Utc>,
        report: agentdocker_core::InputReport,
        token: Option<&str>,
        now: DateTime<Utc>,
    ) -> Response {
        use agentdocker_core::{InputDelivery, InputReceipt, InputReport};
        let id = match self.resolve(reference) {
            Ok(id) => id,
            Err(response) => return *response,
        };
        // While a controller is bound, only it reports: readiness and
        // receipts for this queue are its evidence, nobody else's.
        let bound = match self.binding_for(&id, token) {
            Ok(binding) => binding.is_some(),
            Err(refusal) => return *refusal,
        };
        let mut record = self.registry.get(&id).expect("resolved agent").clone();
        let codex = agentdocker_host::provider_input::is_codex_input(&record);
        let claude = record.container.is_none() && record.spec.runtime == "claude-code";
        if !record.status.is_live()
            || record.process_started_at != Some(process_started_at)
            || (!codex && !claude && !bound)
        {
            return Response::error(
                ErrorCode::Invalid,
                "input report has no matching live provider process",
            );
        }
        if observed_at > now
            || now - observed_at >= Duration::minutes(5)
            || record
                .input_delivery
                .as_ref()
                .is_some_and(|d| observed_at < d.reported_at)
        {
            return Response::error(ErrorCode::Invalid, "input report is stale or in the future");
        }
        let mut held = None;
        let mut delivery = record.input_delivery.clone().unwrap_or(InputDelivery {
            process_started_at,
            paused: false,
            pause_reason: None,
            reported_at: observed_at,
            received: None,
            received_at: None,
            typing_until: None,
        });
        match report {
            InputReport::Received { mut input } => {
                let provider_matches = bound
                    || match input.receipt {
                        InputReceipt::Codex { .. } => codex,
                        InputReceipt::ClaudeChannel => claude,
                    };
                if !input.valid() || !provider_matches {
                    return Response::error(ErrorCode::Invalid, "invalid provider input receipt");
                }
                // Preserve ACK idempotence, including a batch that repeats an
                // older receipt alongside a newly received queue head. Unknown
                // IDs are no-ops and can never create provider evidence.
                let queued = self.inboxes.get(&id);
                input.messages.retain(|message| {
                    queued.is_some_and(|queue| queue.iter().any(|envelope| &envelope.id == message))
                });
                if input.messages.is_empty() {
                    return Response::Ok;
                }
                if delivery.received.as_ref() != Some(&input) {
                    delivery.received_at = Some(now);
                    delivery.received = Some(input);
                }
                delivery.paused = false;
                delivery.pause_reason = None;
                // What was held has gone in.
                delivery.typing_until = None;
            }
            // A heartbeat says nothing about the person's typing: a hold
            // in force lasts until its own time.
            InputReport::Ready => {
                delivery.paused = false;
                delivery.pause_reason = None;
            }
            InputReport::Paused { reason } => {
                if reason.trim().is_empty()
                    || reason.len() > 2048
                    || reason.chars().any(char::is_control)
                {
                    return Response::error(
                        ErrorCode::Invalid,
                        "pause reason must be plain text, 1 to 2048 bytes",
                    );
                }
                delivery.paused = true;
                delivery.pause_reason = Some(reason);
                delivery.typing_until = None;
            }
            // Only a hold the rule could have made: over after it was
            // observed and within one quiet window of it. It is no evidence
            // that a pause is over.
            InputReport::HeldForTyping { until } => {
                if !agentdocker_core::typing::plausible(until, observed_at) {
                    return Response::error(
                        ErrorCode::Invalid,
                        "a hold for typing must end within one quiet window of its observation",
                    );
                }
                held = Some(until);
                delivery.typing_until = Some(until);
            }
        }
        delivery.process_started_at = process_started_at;
        delivery.reported_at = observed_at;
        if record.input_delivery.as_ref() == Some(&delivery) {
            return Response::Ok;
        }
        if record
            .input_delivery
            .as_ref()
            .is_some_and(|previous| observed_at == previous.reported_at)
        {
            return Response::error(
                ErrorCode::Invalid,
                "input report conflicts with another observation at the same time",
            );
        }
        record.input_delivery = Some(delivery.clone());
        let kind = match held {
            Some(until) => agentdocker_core::EventKind::InputHeldForTyping {
                agent: id.clone(),
                until,
            },
            None => agentdocker_core::EventKind::InputDeliveryReported {
                agent: id.clone(),
                delivery,
            },
        };
        let event = agentdocker_core::Event::new(kind, now);
        let _ = self.transition(
            "input delivery report",
            event,
            |store, event| store.agent_transition(&record, event),
            |state| {
                *state.registry.get_mut(&id).expect("resolved agent") = record.clone();
            },
        );
        self.write_failure().unwrap_or(Response::Ok)
    }

    pub(super) fn report_activity(
        &mut self,
        reference: &str,
        observation: agentdocker_core::ActivityObservation,
        now: DateTime<Utc>,
    ) -> Response {
        // A client reports what it saw itself; who else observed is the
        // daemon's to say, and only the daemon reads herdr and Codex's
        // shared server.
        if observation.source.is_some() {
            return Response::error(
                ErrorCode::Invalid,
                "an activity report names no source: only the daemon observes for an agent",
            );
        }
        if observation.current(now).is_none() {
            return Response::error(
                ErrorCode::Invalid,
                "activity observation is stale or in the future",
            );
        }
        let id = match self.resolve(reference) {
            Ok(id) => id,
            Err(response) => return *response,
        };
        self.record_activity(&id, observation, now)
    }

    /// Record an observation of `id`'s activity — its adapter's, or one the
    /// daemon made — and say so with `agent_activity_reported`. One older
    /// than the record's is ignored, and so is one that a current
    /// observation from a source outranking its own keeps off
    /// (`ActivityObservation::keeps_off`: Codex's shared server over herdr).
    pub(super) fn record_activity(
        &mut self,
        id: &AgentId,
        observation: agentdocker_core::ActivityObservation,
        now: DateTime<Utc>,
    ) -> Response {
        if observation.current(now).is_none() {
            return Response::error(
                ErrorCode::Invalid,
                "activity observation is stale or in the future",
            );
        }
        let Some(record) = self.registry.get(id) else {
            return Response::error(ErrorCode::NotFound, "no such agent");
        };
        let id = id.clone();
        let mut record = record.clone();
        if !record.status.is_live() {
            return Response::error(
                ErrorCode::Invalid,
                "cannot report activity for a finished agent",
            );
        }
        if record.reported_activity.as_ref().is_some_and(|previous| {
            previous.observed_at >= observation.observed_at || previous.keeps_off(&observation, now)
        }) {
            return Response::Ok;
        }
        // A wait's detail is kept only as one bounded, printable line that
        // reads like no secret, whoever sent it; and a plain repeat of a
        // detailed wait keeps what the first said.
        let observation = observation.settled(record.reported_activity.as_ref());
        // Preserve the observation's age. Using receipt time here would let a
        // delayed idle report fall back to "working" after its own expiry.
        // What another observer saw is not the agent's own contact.
        if observation.source.is_none() {
            record.last_seen = record.last_seen.max(observation.observed_at);
        }
        record.reported_activity = Some(observation.clone());
        let event = agentdocker_core::Event::new(
            agentdocker_core::EventKind::AgentActivityReported {
                agent: id.clone(),
                observation,
            },
            now,
        );
        let _ = self.transition(
            "activity report",
            event,
            |store, event| store.agent_transition(&record, event),
            |state| {
                *state.registry.get_mut(&id).expect("resolved agent") = record.clone();
            },
        );
        self.write_failure().unwrap_or(Response::Ok)
    }

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

    /// Before a claim agrees to wait: break every ring its wait would
    /// close, each at the wait `wait::victim` picks — the lowest priority,
    /// the youngest among equals, this claim counting as the youngest of
    /// all. An older wait it picks is ended here (`lease_deadlock` naming
    /// it, then its `lease_wait_ended {outcome: deadlock}`), and its claim
    /// answers `deadlock` when it wakes; then this claim looks again, since
    /// another ring may remain. Each pass ends one queued wait, so it
    /// stops. The ring is returned when this claim is the victim; nothing
    /// for it is said yet but `lease_deadlock`.
    ///
    /// Called under the same hold of the state lock as the join that
    /// follows it, so a ring is broken exactly once.
    pub(super) fn break_rings(
        &mut self,
        requester: &AgentId,
        resource: &ResourceKey,
        priority: Priority,
    ) -> Option<Vec<Blocked>> {
        while let Some(cycle) = self.deadlock(requester, resource) {
            let victim = wait::victim(&cycle, priority, self.waiting.all());
            let ended = victim
                .ticket
                .and_then(|ticket| self.waiting.break_ring(ticket, cycle.clone()));
            let step = if ended.is_some() { victim.step } else { 0 };
            self.emit(EventKind::LeaseDeadlock {
                cycle: cycle.clone(),
                victim: cycle.get(step).cloned(),
            });
            let Some(waiter) = ended else {
                return Some(cycle);
            };
            warn!(
                agent = %waiter.agent.short(),
                resource = %waiter.resource,
                by = %requester.short(),
                "wait ended to break a deadlock"
            );
            self.emit(EventKind::LeaseWaitEnded {
                resource: waiter.resource,
                requester: waiter.agent,
                outcome: WaitOutcome::Deadlock,
            });
        }
        None
    }

    /// What one agent is doing, from what the daemon recorded — and,
    /// where the opt-in judge read its quiet screen as waiting on the
    /// person, raised to that. A judgment only raises working, idle or
    /// unknown; it never stands over anything else.
    pub(super) fn activity_of(&self, record: &AgentRecord, now: DateTime<Utc>) -> Activity {
        let recorded = self.recorded_activity_of(record, now);
        match record.judged.as_ref().filter(|j| j.stands_for(record)) {
            Some(judgment) => agentdocker_core::judgment::raise(recorded, judgment),
            None => recorded,
        }
    }

    /// What one agent is doing, from what the daemon recorded rather
    /// than from anything it printed.
    pub(super) fn recorded_activity_of(
        &self,
        record: &AgentRecord,
        now: DateTime<Utc>,
    ) -> Activity {
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
        // A provider whose lifecycle hooks have run in this process reports
        // its own turns and says when the next one starts. Contact between
        // them is its MCP server polling for messages, which is presence, not
        // work: counting it turned every idle report into "working" within
        // seconds, and a finished turn was never seen to finish. The same
        // holds while herdr, watching its terminal, or Codex's shared
        // server, hosting its conversation, says what it is doing: that
        // observation stands over contact until it expires, and past it
        // contact counts again, since the source is no longer saying.
        let reports_turns = reports_turns(record);
        if let Some(observation) = &record.reported_activity
            && (reports_turns
                || (observation.source.is_some() && observation.current(now).is_some())
                || record.last_seen <= observation.observed_at
                || record.last_seen == record.created_at)
        {
            return observation.current(now).unwrap_or(Activity::Unknown);
        }
        if reports_turns {
            return Activity::Unknown;
        }
        // Adoption/registration proves presence, not work. Nor does silence
        // prove idleness: MCP calls are optional, and hooks may be disconnected.
        if record.last_seen > record.created_at && now - record.last_seen < ACTIVE_WINDOW {
            Activity::Working {
                since: record.last_seen,
            }
        } else {
            Activity::Unknown
        }
    }
}

/// Whether the agent's lifecycle hooks have run in its current process, so
/// that it reports its own turns.
pub(super) fn reports_turns(record: &AgentRecord) -> bool {
    record
        .adapter_contacts
        .get(&agentdocker_core::AdapterKind::Hooks)
        .is_some_and(|hooks| Some(hooks.process_started_at) == record.process_started_at)
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
            .map(|record| {
                let queue = state.inboxes.get(&record.id);
                // A receipt from the current process covers the messages it
                // names; what it does not name is still waiting.
                let receipted = record
                    .input_delivery
                    .as_ref()
                    .filter(|d| d.received_for(record.process_started_at, now))
                    .and_then(|d| d.received.as_ref())
                    .map(|r| r.messages.as_slice())
                    .unwrap_or(&[]);
                AgentActivity {
                    agent: record.id.clone(),
                    name: record.spec.name.clone(),
                    project: record.project.as_ref().map(ProjectRef::id),
                    activity: state.activity_of(record, now),
                    queued_inputs: Some(queue.map_or(0, |queue| queue.len())),
                    awaiting_receipt: Some(queue.map_or(0, |queue| {
                        queue.iter().filter(|m| !receipted.contains(&m.id)).count()
                    })),
                }
            })
            .collect();
        // Waiting on the person and blocked first, because those are the
        // ones somebody has to do something about; then working, then the
        // quiet ones.
        activity.sort_by_key(|a| {
            (
                match a.activity {
                    Activity::Waiting { .. } | Activity::Blocked { .. } => 0,
                    Activity::Working { .. } => 1,
                    Activity::Starting => 2,
                    Activity::Idle { .. } => 3,
                    Activity::Unknown => 4,
                    Activity::Finished => 5,
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
