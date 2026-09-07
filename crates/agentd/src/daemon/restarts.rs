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
                Some(record) if !record.status.is_live() && !record.spec.restart.is_no() => {
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
            Ok(spawned) => {
                let pid = spawned.pid;
                let started_at = procinfo::start_time(pid);
                if let Some(session) = spawned.session.clone() {
                    lock(&self.sessions).insert(id.clone(), session);
                }
                {
                    let mut state = lock(&self.state);
                    state.supervised.insert(id.clone(), spawned.control.clone());
                    if let Some(stored) = state.registry.get_mut(id) {
                        stored.pid = Some(pid);
                        stored.process_started_at = started_at;
                        stored.process_group = Some(pid);
                        stored.finished_at = None;
                        stored.restarts = attempt;
                    }
                    if let Some(agent) =
                        state
                            .registry
                            .set_status(id, AgentStatus::Running, Utc::now())
                    {
                        state.persist("agent", |store| store.upsert_agent(&agent));
                    }
                    state.emit(EventKind::AgentRestarted {
                        agent: id.clone(),
                        pid: Some(pid),
                        attempt,
                    });
                }
                supervisor::supervise(self.clone(), id.clone(), spawned);
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
                let policy = {
                    let mut state = lock(&self.state);
                    if let Some(stored) = state.registry.get_mut(id) {
                        stored.restarts = attempt;
                    }
                    if let Some(agent) = state.registry.set_status(id, status.clone(), Utc::now()) {
                        state.persist("agent", |store| store.upsert_agent(&agent));
                        agent.spec.restart
                    } else {
                        return;
                    }
                };
                let _ = policy;
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
