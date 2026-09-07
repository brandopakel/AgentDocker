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

use super::*;
use agentdocker_core::agent::restart_delay;

impl Daemon {
    /// An agent has exited. Start it again if it asked to be.
    ///
    /// Called from the supervisor once the exit is recorded, so a reader
    /// of the event stream sees the exit before the restart.
    pub(crate) fn consider_restart(self: &Arc<Self>, id: &AgentId, status: &AgentStatus) {
        let record = {
            let state = lock(&self.state);
            if state.storage_error.is_some() {
                return;
            }
            let Some(record) = state.registry.get(id) else {
                return;
            };
            if !record.managed || record.container.is_some() {
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
            record.clone()
        };
        let delay = restart_delay(record.restarts);
        // Weak, not strong. The wait is up to half a minute, and a
        // pending restart must not be the thing keeping a daemon alive
        // — it would hold its SQLite handle and its socket open long
        // after everything else had let go, which is exactly what a
        // leak check catches.
        let daemon = Arc::downgrade(self);
        let id = id.clone();
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            let Some(daemon) = daemon.upgrade() else {
                return;
            };
            daemon.restart_now(&id, delay).await;
        });
    }

    /// Start it again, under the same id.
    async fn restart_now(self: &Arc<Self>, id: &AgentId, waited: std::time::Duration) {
        // Re-read under the lock: the wait is long enough for somebody
        // to have stopped it, removed it, or started it themselves.
        let record = {
            let state = lock(&self.state);
            match state.registry.get(id) {
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
                _ => return,
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
            Ok(mut spawned) => {
                let pid = spawned.pid;
                let started_at = Some(spawned.process_started_at);
                if let Some(session) = spawned.session.clone() {
                    lock(&self.sessions).insert(id.clone(), session);
                }
                let persisted = {
                    let mut state = lock(&self.state);
                    // The supervisor owns this child even if its durable transition fails.
                    state.supervised.insert(id.clone(), spawned.control.clone());
                    let current = state.registry.get(id).cloned();
                    if let Some(mut running) = current.filter(|current| {
                        !current.status.is_live()
                            && current.spec.restart == record.spec.restart
                            && current.restarts == record.restarts
                    }) {
                        running.pid = Some(pid);
                        running.process_started_at = started_at;
                        running.process_group = Some(pid);
                        running.status = AgentStatus::Running;
                        running.started_at = Some(Utc::now());
                        running.finished_at = None;
                        running.last_seen = Utc::now();
                        running.restarts = attempt;
                        let mut event = Event::new(
                            EventKind::AgentRestarted {
                                agent: id.clone(),
                                pid: Some(pid),
                                attempt,
                            },
                            Utc::now(),
                        );
                        event.seq = state.next_seq;
                        state.persist("restart completion", |store| {
                            store.agent_transition(&running, &event)
                        });
                        if state.storage_error.is_none() {
                            *state
                                .registry
                                .get_mut(id)
                                .expect("restart identity retained") = running;
                            state.next_seq += 1;
                            let _ = state.events.send(event);
                            true
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                };
                let activation_failed = if persisted {
                    spawned.activate().await.is_err()
                } else {
                    false
                };
                if !persisted || activation_failed {
                    spawned.control.send_replace(Some(true));
                }
                let supervision = supervisor::supervise(self.clone(), id.clone(), spawned);
                if !persisted || activation_failed {
                    let _ = tokio::time::timeout(SUPERVISION_STOP_TIMEOUT, supervision).await;
                }
            }
            Err(err) => {
                warn!(agent = %id.short(), %err, "restart failed");
                // Count the attempt even though it did not start: a
                // command that cannot be spawned would otherwise be
                // retried forever under `on-failure`, which is the one
                // thing the limit exists to prevent.
                let status = AgentStatus::Failed {
                    reason: format!("restart failed: {err:#}"),
                };
                {
                    let mut state = lock(&self.state);
                    if state.storage_error.is_some() {
                        return;
                    }
                    let Some(mut failed) = state.registry.get(id).cloned().filter(|current| {
                        !current.status.is_live()
                            && current.spec.restart == record.spec.restart
                            && current.restarts == record.restarts
                    }) else {
                        return;
                    };
                    failed.restarts = attempt;
                    failed.status = status.clone();
                    failed.finished_at = Some(Utc::now());
                    let mut event = Event::new(
                        EventKind::AgentExited {
                            agent: id.clone(),
                            status: status.clone(),
                        },
                        Utc::now(),
                    );
                    event.seq = state.next_seq;
                    state.persist("failed restart", |store| {
                        store.agent_transition(&failed, &event)
                    });
                    if state.storage_error.is_some() {
                        return;
                    }
                    *state
                        .registry
                        .get_mut(id)
                        .expect("restart identity retained") = failed;
                    state.next_seq += 1;
                    let _ = state.events.send(event);
                }
                self.consider_restart(id, &status);
            }
        }
    }

    /// An agent stopped on purpose stays stopped. Clearing the policy on
    /// the record says so where a reader will find it, rather than in a
    /// flag only the daemon can see.
    pub(super) fn clear_restart(self: &Arc<Self>, id: &AgentId) {
        let mut state = lock(&self.state);
        let Some(record) = state.registry.get_mut(id) else {
            return;
        };
        if record.spec.restart.is_no() {
            return;
        }
        record.spec.restart = agentdocker_core::RestartPolicy::No;
        let record = record.clone();
        state.persist("agent", |store| store.upsert_agent(&record));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            .restart_now(&previous.id, std::time::Duration::ZERO)
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
}
