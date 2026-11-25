//! Starting a managed agent again after it exits.
//!
//! The daemon watches its own children, so it is the one place that can
//! act on an exit the moment it happens rather than noticing later. What
//! it must not do is act on every exit: a supervisor that restarts by
//! default turns a command that fails immediately into a loop, so the
//! policy is `no` unless somebody asked for something else.
//!
//! Three rules do the work, and each exists because of a way this goes
//! wrong without it.
//!
//! An agent **stopped on purpose stays stopped** — the policy is cleared
//! on the record, the same way `--restore` is, so the reason it will not
//! come back is visible in `inspect` rather than hidden here.
//!
//! Restarts **back off**, doubling from a fifth of a second to half a
//! minute, because the other case is a command that will fail every time
//! and the daemon should not spend a core finding that out.
//!
//! And the agent comes back **under its own id**, like a restored one,
//! so its read set, journal cursor, leases and ledger attribution
//! continue to describe it. A restart is the same process running again,
//! not a different agent with the same name.

use super::markers::PendingRestart;
use super::*;
use agentdocker_core::agent::restart_delay;

/// What came of one attempt to start an agent again ([`Daemon::restart_now`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Attempt {
    /// The attempt is over, whatever came of it: its marker is released.
    Done,
    /// A transfer refused admission: the pending task tries again.
    Refused,
    /// The record has another ending than the one this restart was
    /// scheduled for. Nothing was done, and the marker there is now is
    /// that ending's own.
    Superseded,
}

impl State {
    /// A restart is scheduled for this ending of the agent.
    pub(super) fn restart_waits(
        &self,
        id: &AgentId,
        ending: Option<chrono::DateTime<Utc>>,
    ) -> bool {
        self.pending_restarts.contains_key(id)
            && self
                .restart_endings
                .get(id)
                .is_none_or(|scheduled| *scheduled == ending)
    }

    /// Take off the marker of the restart scheduled for `ending`, and say
    /// so. A marker set since for a later ending under the same id stays:
    /// it is another task's.
    pub(super) fn release_restart(&mut self, id: &AgentId, ending: Option<chrono::DateTime<Utc>>) {
        if self
            .restart_endings
            .get(id)
            .is_some_and(|scheduled| *scheduled != ending)
        {
            return;
        }
        self.pending_restarts.remove(id);
        self.restart_endings.remove(id);
        self.publish_restart(id);
    }
}

impl Daemon {
    /// An agent has exited. Start it again if it asked to be.
    ///
    /// Called from the supervisor once the exit is recorded, so a reader
    /// of the event stream sees the exit before the restart.
    pub(crate) fn consider_restart(self: &Arc<Self>, id: &AgentId, status: &AgentStatus) {
        self.restart_if(id, status, None);
    }

    /// [`Daemon::consider_restart`] for one ending of the agent: the one
    /// whose record finished at `finished_at`. A supervision asks this once
    /// its exit is recorded and its owner reaped, by when another run under
    /// the same id may have started and ended; that run's ending is not
    /// the one `status` speaks of, and nothing is restarted on it here.
    pub(crate) fn consider_restart_of(
        self: &Arc<Self>,
        id: &AgentId,
        status: &AgentStatus,
        finished_at: Option<chrono::DateTime<Utc>>,
    ) {
        self.restart_if(id, status, Some(finished_at));
    }

    fn restart_if(
        self: &Arc<Self>,
        id: &AgentId,
        status: &AgentStatus,
        ending: Option<Option<chrono::DateTime<Utc>>>,
    ) {
        let record = {
            let mut state = lock(&self.state);
            if state.storage_error.is_some() {
                return;
            }
            let Some(record) = state.registry.get(id) else {
                return;
            };
            if ending.is_some_and(|finished_at| record.finished_at != finished_at) {
                return;
            }
            if !record.managed || record.container.is_some() || record.status.is_live() {
                return;
            }
            if !record.spec.restart.restarts(status, record.restarts) {
                if !record.spec.restart.is_no() {
                    info!(
                        agent = %id.short(),
                        policy = %record.spec.restart.describe(),
                        restarts = record.restarts,
                        %status,
                        "not restarting"
                    );
                }
                return;
            }
            let record = record.clone();
            // One restart waits per ending. One already waiting for this
            // ending is the one. One waiting for an earlier ending under
            // the same id (a run that was relaunched and ended while the
            // earlier delay ran) is not this run's: this ending gets its
            // own delay, and the earlier task finds on waking that it was
            // replaced.
            if state.restart_waits(id, record.finished_at) {
                return;
            }
            // Installed and said in one hold, before its task exists, so
            // nothing the task does can be said first. While fenced it is
            // installed all the same, for an aborted handover to resume,
            // and the view says it waits on the fence.
            // Installed for this attempt: it speaks of no other.
            let (attempt, due) = match agentdocker_core::restart::due(&record) {
                Some(agentdocker_core::restart::Due::Restart { attempt, at }) => (attempt, at),
                _ => (record.restarts.saturating_add(1), None),
            };
            state.pending_restarts.insert(
                id.clone(),
                agentdocker_core::restart::Phase::Delay { attempt, due },
            );
            state.restart_endings.insert(id.clone(), record.finished_at);
            state.publish_restart(id);
            record
        };
        // The ending this restart is for, carried through the wait, the
        // attempt and the release of its marker.
        let ending = record.finished_at;
        let elapsed = record
            .finished_at
            .and_then(|at| (Utc::now() - at).to_std().ok())
            .unwrap_or_default();
        let delay = restart_delay(record.restarts).saturating_sub(elapsed);
        // Weak, not strong. The wait is up to half a minute, and a
        // pending restart must not be the thing keeping a daemon alive
        // — it would hold its SQLite handle and its socket open long
        // after everything else had let go, which is exactly what a
        // leak check catches.
        let pending = PendingRestart::held(self, id.clone(), ending);
        let daemon = Arc::downgrade(self);
        let id = id.clone();
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            let mut refusal_delay = std::time::Duration::from_millis(250);
            loop {
                {
                    let Some(daemon) = daemon.upgrade() else {
                        return;
                    };
                    daemon.fail_point("restart");
                    match daemon.restart_now(&id, delay, ending).await {
                        Attempt::Done => {
                            let status = {
                                let mut state = lock(&daemon.state);
                                state.release_restart(&id, ending);
                                state.registry.get(&id).map(|record| record.status.clone())
                            };
                            pending.released();
                            // A failed spawn can need another policy attempt. A
                            // running child will arrange its own on durable exit.
                            if let Some(status) = status {
                                daemon.consider_restart(&id, &status);
                            }
                            return;
                        }
                        // The record is at another ending than this one's:
                        // relaunched and running, ended some way its policy
                        // does not restart, failed before a spawn, or ended
                        // again with a restart of its own waiting. Nothing
                        // is started on this deadline, and the guard, let
                        // go here, takes off this restart's marker if it is
                        // still the one there and leaves a later one.
                        Attempt::Superseded => return,
                        Attempt::Refused => {}
                    }
                }
                // A transfer refusal applies nothing. Keep the pending job
                // for an aborted handover without keeping the daemon alive.
                tokio::time::sleep(refusal_delay).await;
                refusal_delay = (refusal_delay * 2).min(std::time::Duration::from_secs(5));
            }
        });
    }

    /// Reconstruct delayed work from durable exit policy after takeover.
    /// The pending set also makes repeated maintenance scans harmless.
    pub(crate) fn resume_restarts(self: &Arc<Self>) {
        let candidates: Vec<_> = {
            let state = lock(&self.state);
            if state.storage_error.is_some() || state.fenced() {
                return;
            }
            state
                .registry
                .all()
                .filter(|record| {
                    record.managed
                        && record.container.is_none()
                        && !record.status.is_live()
                        && record
                            .spec
                            .restart
                            .restarts(&record.status, record.restarts)
                        // Only a restart waiting for this very ending: a
                        // marker left by an earlier one says nothing of it.
                        && !state.restart_waits(&record.id, record.finished_at)
                })
                .map(|record| (record.id.clone(), record.status.clone()))
                .collect()
        };
        for (id, status) in candidates {
            self.consider_restart(&id, &status);
        }
    }

    /// Start it again, under the same id, for the ending the restart was
    /// scheduled for. [`Attempt::Refused`] means a transfer refused
    /// admission and the pending task must retry; [`Attempt::Done`]
    /// finishes this attempt; [`Attempt::Superseded`] means the record has
    /// another ending now, and nothing was done.
    pub(super) async fn restart_now(
        self: &Arc<Self>,
        id: &AgentId,
        waited: std::time::Duration,
        ending: Option<chrono::DateTime<Utc>>,
    ) -> Attempt {
        let _admitted = match self.admit_restart(id, ending) {
            RestartAdmission::Admitted(admitted) => admitted,
            RestartAdmission::Superseded => return Attempt::Superseded,
            RestartAdmission::Refused(error) => {
                return if matches!(
                    *error,
                    Response::Error {
                        code: ErrorCode::Transferring,
                        ..
                    }
                ) {
                    Attempt::Refused
                } else {
                    Attempt::Done
                };
            }
        };
        // Re-read under the lock: the wait is long enough for somebody
        // to have stopped it, removed it, or started it themselves.
        let record = {
            let state = lock(&self.state);
            match state.registry.get(id) {
                // The run that ended when this was scheduled is not the
                // one that has ended now: this deadline is not its.
                Some(record) if record.finished_at != ending => return Attempt::Superseded,
                Some(record)
                    if state.storage_error.is_none()
                        && !record.status.is_live()
                        && record
                            .spec
                            .restart
                            .restarts(&record.status, record.restarts) =>
                {
                    record.clone()
                }
                _ => return Attempt::Done,
            }
        };
        let attempt = record.restarts + 1;
        info!(
            agent = %id.short(),
            name = %record.spec.name,
            attempt,
            waited = ?waited,
            "restarting"
        );
        match supervisor::spawn(self, &record).await {
            Ok(spawned) => self.restarted(&record, ending, spawned).await,
            Err(err) => {
                warn!(agent = %id.short(), %err, "restart failed");
                self.restart_failed(&record, ending, format!("{err:#}"));
            }
        }
        Attempt::Done
    }

    /// What a restart does with the child it spawned for `record`'s
    /// ending. The spawn takes time, and a session can be relaunched under
    /// the id, or fail again, while it runs: so whether this child is the
    /// record's next run is decided under the lock before anything is
    /// installed. Where the record is still at the ending the restart was
    /// scheduled for, the child is installed, committed as its run and let
    /// go. Where it is not (live again, or at another ending), nothing is
    /// installed over the newer start and nothing of the record is
    /// written: the child is stopped and seen to its end under a
    /// supervision that holds nothing of the agent's.
    pub(super) async fn restarted(
        self: &Arc<Self>,
        record: &AgentRecord,
        ending: Option<chrono::DateTime<Utc>>,
        mut spawned: supervisor::Spawned,
    ) {
        let id = &record.id;
        let attempt = record.restarts + 1;
        let pid = spawned.pid;
        let started_at = Some(spawned.process_started_at);
        let supervising = self.supervision_of(id, &spawned);
        let persisted = {
            let mut state = lock(&self.state);
            let current = state.registry.get(id).cloned().filter(|current| {
                !current.status.is_live()
                    && current.finished_at == ending
                    && current.spec.restart == record.spec.restart
                    && current.restarts == record.restarts
            });
            match current {
                None => false,
                Some(mut running) => {
                    // The supervisor owns this child even if its durable
                    // transition fails: installed for this start, which is
                    // on no record until the commit below.
                    state.install(
                        &supervising,
                        spawned.control.clone(),
                        spawned.session.clone(),
                        false,
                    );
                    if state.run_refusal(record).is_some() {
                        false
                    } else {
                        running.pid = Some(pid);
                        running.process_started_at = started_at;
                        running.process_group = Some(pid);
                        running.owner = Some(spawned.owner.clone());
                        running.status = AgentStatus::Running;
                        // A new run, committed with `agent_restarted`.
                        running.begin_run();
                        running.started_at = Some(Utc::now());
                        running.finished_at = None;
                        running.last_seen = Utc::now();
                        running.restarts = attempt;
                        let event = Event::new(
                            EventKind::AgentRestarted {
                                agent: id.clone(),
                                pid: Some(pid),
                                attempt,
                            },
                            Utc::now(),
                        );
                        let committed = state.transition(
                            "restart completion",
                            event,
                            |store, event| store.agent_transition(&running, event),
                            |state| {
                                *state
                                    .registry
                                    .get_mut(id)
                                    .expect("restart identity retained") = running.clone();
                            },
                        ) == Persisted::Committed;
                        if committed {
                            state.written(&supervising);
                        }
                        committed
                    }
                }
            }
        };
        let activation_failed = if persisted {
            spawned.activate("restarted").await.is_err()
        } else {
            false
        };
        if !persisted || activation_failed {
            spawned.control.send_replace(Some(true));
        }
        let supervision = supervisor::supervise(self.clone(), supervising, spawned);
        if !persisted || activation_failed {
            let _ = tokio::time::timeout(SUPERVISION_STOP_TIMEOUT, supervision).await;
        }
    }

    /// A restart's spawn failed. The attempt is counted and its failure
    /// written as the record's ending, while the record is still at the
    /// ending the restart was scheduled for: a run that began or ended
    /// under the id meanwhile keeps its own.
    pub(super) fn restart_failed(
        self: &Arc<Self>,
        record: &AgentRecord,
        ending: Option<chrono::DateTime<Utc>>,
        why: String,
    ) {
        let id = &record.id;
        let attempt = record.restarts + 1;
        // Count the attempt even though it did not start: a command that
        // cannot be spawned would otherwise be retried forever under
        // `on-failure`, which is the one thing the limit exists to prevent.
        let status = AgentStatus::Failed {
            reason: format!("restart failed: {why}"),
        };
        let mut state = lock(&self.state);
        if state.storage_error.is_some() {
            return;
        }
        let Some(mut failed) = state.registry.get(id).cloned().filter(|current| {
            !current.status.is_live()
                && current.finished_at == ending
                && current.spec.restart == record.spec.restart
                && current.restarts == record.restarts
        }) else {
            return;
        };
        failed.restarts = attempt;
        // This attempt's own failure: whatever ended the run before is not
        // how this one ended.
        failed.end_run(status.clone(), None);
        failed.finished_at = Some(Utc::now());
        let now = Utc::now();
        let mut events = vec![Event::new(
            EventKind::AgentExited {
                agent: id.clone(),
                status: status.clone(),
                ending: None,
            },
            now,
        )];
        // The failed attempt that spends the policy says so with its
        // ending.
        events.extend(super::restarts_exhausted(&failed).map(|kind| Event::new(kind, now)));
        let _ = state.commit(
            "failed restart",
            events,
            |store, events| store.agent_exit(&failed, &[], &[], &[], events),
            |state, _, ()| {
                *state
                    .registry
                    .get_mut(id)
                    .expect("restart identity retained") = failed.clone();
            },
        );
    }

    /// An agent stopped on purpose stays stopped. Clearing the policy on
    /// the record says so where a reader will find it, rather than in a
    /// flag only the daemon can see.
    /// Whether the policy is cleared (already, or by a committed write):
    /// a stop must not go on when the policy that would start the agent
    /// again is still in force on disk.
    pub(super) fn clear_restart(self: &Arc<Self>, id: &AgentId) -> bool {
        let mut state = lock(&self.state);
        let Some(record) = state.registry.get(id) else {
            return true;
        };
        if record.spec.restart.is_no() {
            return true;
        }
        // Disk first, memory on commit: a write that did not land leaves
        // the policy as it was, in both places.
        let mut record = record.clone();
        // What it was is kept beside the `No` an older daemon reads.
        let previous = record.spec.restart;
        record.spec.restart = agentdocker_core::RestartPolicy::No;
        record.restart_before_stop = Some(previous);
        let event = Event::new(
            EventKind::AgentRestartCleared {
                agent: record.id.clone(),
                previous: Some(previous),
            },
            Utc::now(),
        );
        state.transition(
            "restart policy",
            event,
            |store, event| store.agent_transition(&record, event),
            |state| {
                if let Some(stored) = state.registry.get_mut(id) {
                    stored.spec.restart = agentdocker_core::RestartPolicy::No;
                    stored.restart_before_stop = Some(previous);
                }
                // A restart waiting for this agent no longer applies.
                if state.pending_restarts.contains_key(id) {
                    state.publish_restart(id);
                }
            },
        ) == Persisted::Committed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ending the record holds, as a restart scheduled for it carries.
    fn ended(daemon: &Daemon, id: &AgentId) -> Option<chrono::DateTime<Utc>> {
        lock(&daemon.state)
            .registry
            .get(id)
            .and_then(|record| record.finished_at)
    }

    #[tokio::test]
    async fn restart_due_during_transfer_runs_once_after_abort_or_takeover() {
        for takeover in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let home = dir.path().join("state");
            let predecessor =
                Arc::new(Daemon::open(home.clone(), dir.path().join("sock")).unwrap());
            let record = {
                let mut record = AgentRecord::new(
                    AgentSpec {
                        name: "delayed-restart".into(),
                        command: vec!["sleep".into(), "30".into()],
                        restart: agentdocker_core::RestartPolicy::OnFailure { max: 1 },
                        ..Default::default()
                    },
                    true,
                    Utc::now(),
                );
                record.status = AgentStatus::Exited { code: Some(1) };
                record.finished_at = Some(Utc::now());
                assert!(matches!(
                    lock(&predecessor.state).insert_record(record.clone()),
                    Response::Agent { .. }
                ));
                record
            };
            predecessor.consider_restart(&record.id, &record.status);
            let transfer = predecessor.offer_transfer(std::process::id()).unwrap();
            // Let the backoff expire while admission is fenced.
            tokio::time::sleep(std::time::Duration::from_millis(350)).await;
            assert_eq!(
                lock(&predecessor.state)
                    .registry
                    .get(&record.id)
                    .unwrap()
                    .restarts,
                0
            );
            assert!(
                lock(&predecessor.state)
                    .pending_restarts
                    .contains_key(&record.id)
            );
            let serving = if takeover {
                let successor =
                    Arc::new(Daemon::open_pending(home, dir.path().join("next.sock")).unwrap());
                successor.accept_transfer(&transfer.id).unwrap();
                assert!(!predecessor.abort_transfer("already accepted"));
                drop(predecessor);
                successor
            } else {
                assert!(predecessor.abort_transfer("candidate refused"));
                predecessor
            };
            for _ in 0..3 {
                serving.resume_restarts();
            }
            let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                loop {
                    let ready = {
                        let state = lock(&serving.state);
                        state
                            .registry
                            .get(&record.id)
                            .is_some_and(|a| a.status == AgentStatus::Running && a.restarts == 1)
                    };
                    if ready {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
            })
            .await;
            // Reap owned work even if the assertion above would have failed.
            serving.clear_restart(&record.id);
            serving.stop_all().await;
            result.expect("the scheduled restart must survive the handover");
            assert_eq!(
                serving
                    .recent_events(100)
                    .iter()
                    .filter(|event| matches!(event.kind, EventKind::AgentRestarted { .. }))
                    .count(),
                1
            );
        }
    }

    /// The daemon's own shutdown stops a running agent without clearing
    /// its restart policy, so the next daemon can start it again by that
    /// policy, under its id, with the shutdown's stop marked on the run
    /// that ended. The restart begins a run of its own: the mark is not
    /// carried into it, in memory or on disk, and a stop of the new run is
    /// marked again.
    #[tokio::test]
    async fn a_restarted_agent_does_not_inherit_a_stop_marked_on_the_run_before() {
        let dir = tempfile::TempDir::new().unwrap();
        let daemon =
            Arc::new(Daemon::open(dir.path().join("state"), dir.path().join("host.sock")).unwrap());
        let mut previous = AgentRecord::new(
            AgentSpec {
                name: "stopped-then-restarted".into(),
                command: vec!["sleep".into(), "30".into()],
                workdir: Some(dir.path().to_path_buf()),
                restart: agentdocker_core::RestartPolicy::OnFailure { max: 1 },
                ..Default::default()
            },
            true,
            Utc::now(),
        );
        previous.mark_stopping();
        previous.status = AgentStatus::Exited { code: None };
        previous.finished_at = Some(Utc::now());
        assert!(previous.stopped_on_request());
        assert!(matches!(
            lock(&daemon.state).insert_record(previous.clone()),
            Response::Agent { .. }
        ));
        daemon
            .restart_now(
                &previous.id,
                std::time::Duration::ZERO,
                ended(&daemon, &previous.id),
            )
            .await;
        let (current, durable) = {
            let state = lock(&daemon.state);
            (
                state.registry.get(&previous.id).unwrap().clone(),
                state.store.load_agents().unwrap()[0].clone(),
            )
        };
        // Cleanup precedes assertions so a failure cannot leak a child.
        daemon.stop_all().await;
        let stopped = lock(&daemon.state)
            .registry
            .get(&previous.id)
            .unwrap()
            .clone();
        assert_eq!(current.status, AgentStatus::Running, "{current:?}");
        assert_eq!(current.restarts, 1);
        assert_eq!(current.stop_requested, Some(false), "not inherited");
        assert_eq!(durable.stop_requested, Some(false), "nor on disk");
        assert!(stopped.stopped_on_request(), "the new run's own stop");
        assert!(!stopped.status.is_live());
    }

    #[tokio::test]
    async fn automatic_restart_obeys_new_run_policy_before_execution() {
        let dir = tempfile::tempdir().unwrap();
        let daemon =
            Arc::new(Daemon::open(dir.path().join("state"), dir.path().join("host.sock")).unwrap());
        let marker = dir.path().join("executed");
        let mut record = AgentRecord::new(
            AgentSpec {
                name: "denied-restart".into(),
                command: vec![
                    "sh".into(),
                    "-c".into(),
                    "printf executed > \"$1\"".into(),
                    "fixture".into(),
                    marker.to_string_lossy().into(),
                ],
                restart: agentdocker_core::RestartPolicy::OnFailure { max: 1 },
                ..Default::default()
            },
            true,
            Utc::now(),
        );
        // Its last run ended by a signal: the failed restart is this
        // attempt's own ending, and keeps none of the last one's.
        record.end_run(
            AgentStatus::Exited { code: None },
            Some(agentdocker_core::RunEnd::Signal { signal: 9 }),
        );
        lock(&daemon.state).insert_record(record.clone());
        std::fs::write(
            daemon.home.join("policy.toml"),
            "[[rule]]\ndeny = [\"run:**\"]\n",
        )
        .unwrap();
        daemon
            .restart_now(
                &record.id,
                std::time::Duration::ZERO,
                ended(&daemon, &record.id),
            )
            .await;
        daemon.stop_all().await;
        assert!(!marker.exists());
        let current = lock(&daemon.state)
            .registry
            .get(&record.id)
            .unwrap()
            .clone();
        assert_eq!(current.restarts, 1);
        assert!(current.pid.is_none());
        assert!(
            matches!(&current.status, AgentStatus::Failed { reason } if reason.contains("run:denied-restart")),
            "{:?}",
            current.status
        );
        assert_eq!(current.ending, None, "not the last run's signal");
        let durable = lock(&daemon.state).store.load_agents().unwrap()[0].clone();
        assert_eq!(durable.ending, None);
    }

    #[tokio::test]
    async fn a_failed_restart_event_reaps_the_child_and_preserves_the_previous_record() {
        let dir = tempfile::TempDir::new().unwrap();
        let daemon =
            Arc::new(Daemon::open(dir.path().join("state"), dir.path().join("host.sock")).unwrap());
        let mut previous = AgentRecord::new(
            AgentSpec {
                name: "restart-storage-fixture".into(),
                command: vec![
                    "sh".into(),
                    "-c".into(),
                    "printf executed > must-not-execute; exec sleep 30".into(),
                ],
                workdir: Some(dir.path().to_path_buf()),
                restart: agentdocker_core::RestartPolicy::OnFailure { max: 1 },
                ..Default::default()
            },
            true,
            Utc::now(),
        );
        previous.status = AgentStatus::Exited { code: Some(1) };
        previous.finished_at = Some(Utc::now());
        {
            let mut state = lock(&daemon.state);
            assert!(matches!(
                state.insert_record(previous.clone()),
                Response::Agent { .. }
            ));
            state.store.reject_event_for_test("agent_restarted");
        }
        daemon
            .restart_now(
                &previous.id,
                std::time::Duration::ZERO,
                ended(&daemon, &previous.id),
            )
            .await;
        let (controlled, durable, current, failed) = {
            let state = lock(&daemon.state);
            (
                state.supervised.contains_key(&previous.id),
                state.store.load_agents().unwrap()[0].clone(),
                state.registry.get(&previous.id).unwrap().clone(),
                state.storage_error.is_some(),
            )
        };
        // Cleanup precedes assertions so the before-fix failure cannot leak a child.
        daemon.stop_all().await;
        assert!(failed, "the event fault must be reached");
        assert!(!dir.path().join("must-not-execute").exists());
        assert!(
            !controlled,
            "failed restart persistence must finish owned cleanup"
        );
        assert_eq!(
            durable.status, previous.status,
            "status and restart event must commit together"
        );
        assert_eq!(
            current.status, previous.status,
            "failed persistence must not expose Running"
        );
        assert_eq!(durable.restarts, previous.restarts);
    }

    fn ended_record(name: &str, policy: agentdocker_core::RestartPolicy) -> AgentRecord {
        let mut record = AgentRecord::new(
            AgentSpec {
                name: name.into(),
                command: vec!["sleep".into(), "30".into()],
                restart: policy,
                ..Default::default()
            },
            true,
            Utc::now(),
        );
        record.status = AgentStatus::Exited { code: Some(1) };
        // Long over: the restart is due at once.
        record.finished_at = Some(Utc::now() - chrono::Duration::hours(1));
        record
    }

    /// The views said for one agent, in order, up to and including its
    /// restart, and the one after it.
    fn said(
        events: &mut tokio::sync::broadcast::Receiver<Event>,
        id: &AgentId,
    ) -> Vec<Option<agentdocker_core::restart::RestartView>> {
        let mut said = Vec::new();
        while let Ok(event) = events.try_recv() {
            match event.kind {
                EventKind::AgentRestartChanged { agent, view, .. } if agent == *id => {
                    said.push(view)
                }
                EventKind::AgentRestarted { agent, .. } if agent == *id => {
                    said.push(Some(agentdocker_core::restart::RestartView::Unknown))
                }
                _ => {}
            }
        }
        said
    }

    async fn until_restarted(daemon: &Arc<Daemon>, id: &AgentId) {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                // Restarted, and its task has let its entry go (which is
                // what says it cleared): both, not the first alone.
                let restarted = {
                    let state = lock(&daemon.state);
                    state
                        .registry
                        .get(id)
                        .is_some_and(|a| a.status == AgentStatus::Running && a.restarts == 1)
                        && !state.pending_restarts.contains_key(id)
                };
                if restarted {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the restart ran");
    }

    /// Where a restart stands is said from the hold that moves it, so the
    /// order is the scheduler's: installed under a fence it waits on the
    /// handover, refused it still waits, the fence lifted before the retry
    /// it waits to retry, admitted it is starting, restarted it is cleared.
    /// Unfenced, an overdue job is said scheduled before it is starting.
    /// (An `AgentRestarted` is marked `Unknown` in the lists below.)
    #[tokio::test]
    async fn a_restart_is_said_where_it_stands_in_the_order_it_moves() {
        use agentdocker_core::restart::{RestartView, Waiting};
        let dir = tempfile::tempdir().unwrap();
        let daemon =
            Arc::new(Daemon::open(dir.path().join("state"), dir.path().join("sock")).unwrap());
        let policy = agentdocker_core::RestartPolicy::OnFailure { max: 3 };
        let fenced = ended_record("fenced-restart", policy);
        let free = ended_record("free-restart", policy);
        for record in [&fenced, &free] {
            assert!(matches!(
                lock(&daemon.state).insert_record(record.clone()),
                Response::Agent { .. }
            ));
        }
        let due = agentdocker_core::restart::due(&fenced).and_then(|due| match due {
            agentdocker_core::restart::Due::Restart { at, .. } => at,
            agentdocker_core::restart::Due::GivenUp { .. } => None,
        });
        // Due by its policy, and nothing installed.
        assert_eq!(
            lock(&daemon.state).restart_view(&fenced.id),
            Some(RestartView::Waiting {
                attempt: 1,
                why: Waiting::NotScheduled,
                at: due
            })
        );
        let mut events = daemon.subscribe_events();
        daemon.offer_transfer(4242).unwrap();
        daemon.consider_restart(&fenced.id, &fenced.status);
        // Overdue, so it tries at once and the fence refuses it: waited
        // for, not slept for. The retry backs off a quarter of a second
        // from that refusal, so the abort below comes before it.
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let refused = matches!(
                    lock(&daemon.state).pending_restarts.get(&fenced.id),
                    Some(agentdocker_core::restart::Phase::Refused { .. })
                );
                if refused {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(2)).await;
            }
        })
        .await
        .expect("the fence refused the attempt");
        let handing_over = Some(RestartView::Waiting {
            attempt: 1,
            why: Waiting::HandingOver,
            at: due,
        });
        assert_eq!(lock(&daemon.state).restart_view(&fenced.id), handing_over);
        assert!(daemon.abort_transfer("candidate refused"));
        // The fence lifted, the retry not yet admitted: not a handover.
        assert_eq!(
            lock(&daemon.state).restart_view(&fenced.id),
            Some(RestartView::Waiting {
                attempt: 1,
                why: Waiting::RetryAfterTransfer,
                at: due
            })
        );
        until_restarted(&daemon, &fenced.id).await;
        let fenced_said = said(&mut events, &fenced.id);

        let mut events = daemon.subscribe_events();
        daemon.consider_restart(&free.id, &free.status);
        until_restarted(&daemon, &free.id).await;
        let free_said = said(&mut events, &free.id);
        daemon.clear_restart(&fenced.id);
        daemon.clear_restart(&free.id);
        daemon.stop_all().await;

        assert_eq!(fenced_said.first(), Some(&handing_over), "{fenced_said:?}");
        let at = |view: &Option<RestartView>| fenced_said.iter().position(|v| v == view);
        let retry = at(&Some(RestartView::Waiting {
            attempt: 1,
            why: Waiting::RetryAfterTransfer,
            at: due,
        }))
        .expect("the lifted fence is said");
        let starting = at(&Some(RestartView::Starting { attempt: 1 })).expect("starting");
        let restarted = at(&Some(RestartView::Unknown)).expect("restarted");
        assert!(retry < starting && starting < restarted, "{fenced_said:?}");
        assert_eq!(
            fenced_said.get(restarted + 1),
            Some(&None),
            "cleared after: {fenced_said:?}"
        );
        assert!(
            !fenced_said
                .iter()
                .any(|v| matches!(v, Some(RestartView::Scheduled { .. }))),
            "nothing announced a schedule the fence held: {fenced_said:?}"
        );
        let free_due = agentdocker_core::restart::due(&free).and_then(|due| match due {
            agentdocker_core::restart::Due::Restart { at, .. } => at,
            agentdocker_core::restart::Due::GivenUp { .. } => None,
        });
        assert_eq!(
            free_said,
            vec![
                Some(RestartView::Scheduled {
                    attempt: 1,
                    of: Some(3),
                    at: free_due
                }),
                Some(RestartView::Starting { attempt: 1 }),
                Some(RestartView::Unknown),
                None,
            ]
        );
    }

    /// An entry speaks only of its own attempt. Attempt 1 is admitted, its
    /// spawn fails and is counted, and its entry is not yet removed (the
    /// task removes it in a later hold): read through the daemon, attempt 2
    /// is not scheduled, never starting. Only its own install says it is
    /// scheduled.
    #[tokio::test]
    async fn an_old_attempts_entry_never_speaks_for_the_next() {
        use agentdocker_core::restart::{Known, Outlook, Phase, RestartView, Waiting};
        let dir = tempfile::tempdir().unwrap();
        let daemon =
            Arc::new(Daemon::open(dir.path().join("state"), dir.path().join("sock")).unwrap());
        let policy = agentdocker_core::RestartPolicy::OnFailure { max: 3 };
        let record = ended_record("next-attempt", policy);
        assert!(matches!(
            lock(&daemon.state).insert_record(record.clone()),
            Response::Agent { .. }
        ));
        // Attempt 1 admitted, as its task holds it.
        lock(&daemon.state)
            .pending_restarts
            .insert(record.id.clone(), Phase::Starting { attempt: 1 });
        // Its spawn is refused by a new rule: a failed attempt, counted.
        std::fs::write(
            daemon.home.join("policy.toml"),
            "[[rule]]\ndeny = [\"run:**\"]\n",
        )
        .unwrap();
        daemon
            .restart_now(
                &record.id,
                std::time::Duration::ZERO,
                ended(&daemon, &record.id),
            )
            .await;
        let (current, answer) = {
            let mut state = lock(&daemon.state);
            let current = state.registry.get(&record.id).unwrap().clone();
            (current, state.restarts(Some(record.id.as_str())))
        };
        assert_eq!(current.restarts, 1);
        assert!(matches!(current.status, AgentStatus::Failed { .. }));
        assert!(
            lock(&daemon.state)
                .pending_restarts
                .contains_key(&record.id),
            "between the commit and the removal"
        );
        let Response::Restarts { outlooks } = answer else {
            panic!("{answer:?}")
        };
        assert!(
            matches!(
                Outlook::known_for(outlooks.get(&record.id), &current),
                Known::Known(Some(RestartView::Waiting {
                    attempt: 2,
                    why: Waiting::NotScheduled,
                    ..
                }))
            ),
            "{outlooks:?}"
        );
        // Its own install: the old entry let go, attempt 2 installed.
        lock(&daemon.state).pending_restarts.remove(&record.id);
        daemon.consider_restart(&record.id, &current.status);
        assert!(matches!(
            lock(&daemon.state).restart_view(&record.id),
            Some(RestartView::Scheduled { attempt: 2, .. })
        ));
        daemon.clear_restart(&record.id);
        daemon.stop_all().await;
    }

    /// The ending that spends an `on-failure` policy commits its give-up
    /// with it, right after its `agent_exited`, once: never again from
    /// `consider_restart`, and not at all when the store keeps nothing.
    #[tokio::test]
    async fn a_policy_that_gives_up_says_so_once_with_its_ending() {
        for store_fails in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let daemon =
                Arc::new(Daemon::open(dir.path().join("state"), dir.path().join("sock")).unwrap());
            let mut record = ended_record(
                "spent",
                agentdocker_core::RestartPolicy::OnFailure { max: 2 },
            );
            record.status = AgentStatus::Running;
            record.finished_at = None;
            record.restarts = 2;
            assert!(matches!(
                lock(&daemon.state).insert_record(record.clone()),
                Response::Agent { .. }
            ));
            let before = lock(&daemon.state).store.max_event_seq().unwrap();
            if store_fails {
                lock(&daemon.state).store.reject_writes_for_test();
            }
            let status = AgentStatus::Exited { code: Some(1) };
            lock(&daemon.state).mark_exited(&record.id, status.clone());
            daemon.consider_restart(&record.id, &status);
            daemon.consider_restart(&record.id, &status);
            if store_fails {
                assert!(daemon.is_live(&record.id), "nothing was kept");
                continue;
            }
            let events: Vec<_> = lock(&daemon.state)
                .store
                .recent_events(50)
                .unwrap()
                .into_iter()
                .filter(|event| event.seq > before)
                .collect();
            let exit = events
                .iter()
                .find(|e| matches!(e.kind, EventKind::AgentExited { .. }))
                .unwrap();
            let gave_up: Vec<_> = events
                .iter()
                .filter(|e| matches!(e.kind, EventKind::AgentRestartsExhausted { .. }))
                .collect();
            assert_eq!(gave_up.len(), 1, "{events:?}");
            assert_eq!(gave_up[0].seq, exit.seq + 1, "in the ending's transaction");
            assert!(matches!(
                &gave_up[0].kind,
                EventKind::AgentRestartsExhausted { attempts: 2, policy, .. }
                    if *policy == agentdocker_core::RestartPolicy::OnFailure { max: 2 }
            ));
            let answer = lock(&daemon.state).restarts(None);
            let current = lock(&daemon.state)
                .registry
                .get(&record.id)
                .unwrap()
                .clone();
            let Response::Restarts { outlooks } = answer else {
                panic!("{answer:?}")
            };
            assert!(matches!(
                agentdocker_core::restart::Outlook::known_for(outlooks.get(&record.id), &current),
                agentdocker_core::restart::Known::Known(Some(
                    agentdocker_core::restart::RestartView::GivenUp { attempts: 2, .. }
                ))
            ));
        }
    }

    /// A stop keeps the policy it cleared beside the `No` an older daemon
    /// reads, in one commit with its event; a second stop does not replace
    /// it, and the daemon's own shutdown clears nothing.
    #[tokio::test]
    async fn a_stop_keeps_the_policy_it_cleared() {
        let dir = tempfile::tempdir().unwrap();
        let daemon =
            Arc::new(Daemon::open(dir.path().join("state"), dir.path().join("sock")).unwrap());
        let policy = agentdocker_core::RestartPolicy::OnFailure { max: 3 };
        let mut record = ended_record("kept-policy", policy);
        record.status = AgentStatus::Running;
        assert!(matches!(
            lock(&daemon.state).insert_record(record.clone()),
            Response::Agent { .. }
        ));
        let mut events = daemon.subscribe_events();
        assert!(daemon.clear_restart(&record.id));
        assert!(
            daemon.clear_restart(&record.id),
            "a second stop finds it cleared"
        );
        let (current, durable) = {
            let state = lock(&daemon.state);
            (
                state.registry.get(&record.id).unwrap().clone(),
                state.store.load_agents().unwrap()[0].clone(),
            )
        };
        for kept in [&current, &durable] {
            assert!(kept.spec.restart.is_no(), "the No an older daemon reads");
            assert_eq!(kept.restart_before_stop, Some(policy));
        }
        assert_eq!(
            current.restart_cleared_words().as_deref(),
            Some("restart cleared by a stop (was on-failure:3)")
        );
        let mut cleared = Vec::new();
        while let Ok(event) = events.try_recv() {
            if let EventKind::AgentRestartCleared { previous, .. } = event.kind {
                cleared.push(previous);
            }
        }
        assert_eq!(
            cleared,
            vec![Some(policy)],
            "one clearing, with what it was"
        );
        // Rearmed, the clearing is history and not said.
        let mut rearmed = current.clone();
        rearmed.spec.restart = agentdocker_core::RestartPolicy::Always;
        assert_eq!(rearmed.restart_cleared_words(), None);

        let other = {
            let mut other = ended_record("shut-down", policy);
            other.status = AgentStatus::Running;
            assert!(matches!(
                lock(&daemon.state).insert_record(other.clone()),
                Response::Agent { .. }
            ));
            other
        };
        daemon.stop_all().await;
        let after = lock(&daemon.state).registry.get(&other.id).unwrap().clone();
        assert_eq!(after.spec.restart, policy, "a shutdown keeps the policy");
        assert_eq!(after.restart_before_stop, None);
    }
}
