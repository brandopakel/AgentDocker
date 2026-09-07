//! Snapshot restore: what a restarted daemon brings back.
//!
//! `agentd` going down takes its managed agents with it — they are its
//! children and its shutdown SIGTERMs them. Until now that was the end of
//! them: the records stayed, the processes did not, and the next `ps`
//! showed a column of exited agents.
//!
//! A restarted daemon can do better, because everything needed is already
//! stored. A managed agent's record carries its command, directory,
//! environment and whether it wanted a terminal, so the process can be
//! started again. More usefully, everything *about* the agent is keyed by
//! its id — the read set, the journal cursor, its checkpoints, the leases
//! it held, its rows in the ledger — so relaunching it under the **same
//! id** brings the working set back with it rather than handing it a bare
//! shell in the right directory.
//!
//! That is the whole difference from restoring a terminal multiplexer's
//! layout. A restored agent is told what it was doing, what it had read,
//! which of that changed while it was gone, what it still holds, and
//! where its journal reading had got to. It resumes with evidence.
//!
//! Opt-in, per agent (`run --restore`, or `restore = true` in an
//! `Agentfile.toml`): starting a daemon should never spawn processes
//! nobody asked it to, and `agentdocker ps` starts the daemon.

use super::working::check_reads;
use super::*;
use agentdocker_core::protocol::DEFAULT_LEASE_TTL_SECS;
use agentdocker_core::{Checkpoint, ReadMark, ResourceKey};
use serde::{Deserialize, Serialize};

/// What the daemon knew about a restorable agent when it stopped it.
///
/// Only written on the daemon's own shutdown, and only for agents that
/// asked to be restored. It exists because stopping an agent correctly
/// releases its leases — the resource really is free once nothing is
/// working on it — so by the time a new daemon starts, the lease table no
/// longer says what the agent had. After a crash there is no restore
/// point, and none is needed: nothing released anything, so the lease
/// table is still the truth.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct RestorePoint {
    pub at: DateTime<Utc>,
    pub leases: Vec<HeldLease>,
}

/// A lease as it was held, minus the parts a new one gets fresh: the id,
/// the times, and the ledger watermark.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct HeldLease {
    pub resource: ResourceKey,
    pub mode: LeaseMode,
    #[serde(default)]
    pub note: Option<String>,
}

/// How long a restore point is worth acting on. A daemon that has been
/// down for a day is not resuming a session; re-taking a day-old lease
/// would be claiming a resource on behalf of work nobody is doing.
const RESTORE_POINT_LIFE: Duration = Duration::hours(12);

impl Daemon {
    /// Bring back the agents that were running when the last daemon
    /// stopped. Called once at startup, before the liveness sweep can
    /// retire them and take their leases with them.
    ///
    /// Never fatal: an agent whose command or directory has gone is
    /// recorded as failed and the rest still come back.
    pub async fn restore_agents(self: &Arc<Self>) {
        // Not `live()`: how the last daemon ended decides what the store
        // says. A clean shutdown stopped these agents, so their records
        // read `exited`; a crash wrote nothing, so they still read
        // `running`. Either way the process is gone and the agent asked
        // to come back, which is the whole of the question.
        let candidates: Vec<AgentRecord> = {
            let state = lock(&self.state);
            state
                .registry
                .all()
                .filter(|a| a.managed && a.spec.restore && a.container.is_none())
                .filter(|a| !matches!(a.status, AgentStatus::Created))
                .cloned()
                .collect()
        };
        if candidates.is_empty() {
            return;
        }
        info!(agents = candidates.len(), "restoring agents");
        for record in candidates {
            self.restore_one(record).await;
        }
    }

    /// Record what each restorable agent holds, just before the daemon
    /// stops them. Stopping an agent correctly releases its leases, so
    /// this is the only moment the answer still exists.
    pub fn save_restore_points(self: &Arc<Self>) {
        let mut state = lock(&self.state);
        let restorable: Vec<AgentId> = state
            .registry
            .live()
            .filter(|a| a.managed && a.spec.restore && a.container.is_none())
            .map(|a| a.id.clone())
            .collect();
        let now = Utc::now();
        for id in restorable {
            let leases: Vec<HeldLease> = state
                .leases
                .by_holder(&id)
                .into_iter()
                .map(|lease| HeldLease {
                    resource: lease.resource.clone(),
                    mode: lease.mode,
                    note: lease.note.clone(),
                })
                .collect();
            let point = RestorePoint { at: now, leases };
            state.store_op("restore_point", |store| {
                store.put_document("restore_point", id.as_str(), &point)
            });
        }
    }

    /// An agent stopped on purpose stays stopped. Clearing the flag says
    /// so in the record itself rather than in state nobody can see.
    pub(super) fn clear_restore(self: &Arc<Self>, id: &AgentId) {
        let mut state = lock(&self.state);
        let Some(record) = state.registry.get_mut(id) else {
            return;
        };
        if !record.spec.restore {
            return;
        }
        record.spec.restore = false;
        let record = record.clone();
        state.persist("agent", |store| store.upsert_agent(&record));
        state.store_op("restore_point", |store| {
            store.delete_document("restore_point", id.as_str())
        });
    }

    async fn restore_one(self: &Arc<Self>, record: AgentRecord) {
        let id = record.id.clone();
        // The old process usually went down with the old daemon, but a
        // descendant can outlive it. Leave anything still running alone
        // rather than start a second copy beside it.
        if record.process_group.is_some_and(supervisor::group_exists) {
            warn!(agent = %id.short(), "process group is still alive; not restoring");
            return;
        }
        // Leases first: the agent must hold what it held before its
        // process exists to act on any of it.
        let reclaimed = self.reclaim_leases(&record);
        // What it was doing, gathered before the relaunch so the brief
        // describes the state it is being handed.
        let mut brief = self.brief(&record).await;
        brief.reclaimed = reclaimed;

        match supervisor::spawn(self, &record).await {
            Ok(spawned) => {
                let pid = spawned.pid;
                if let Some(session) = spawned.session.clone() {
                    lock(&self.sessions).insert(id.clone(), session);
                }
                {
                    let mut state = lock(&self.state);
                    state.supervised.insert(id.clone(), spawned.control.clone());
                    if let Some(rec) = state.registry.get_mut(&id) {
                        rec.pid = Some(pid);
                        rec.process_started_at = procinfo::start_time(pid);
                        rec.process_group = Some(pid);
                        rec.finished_at = None;
                    }
                    if let Some(rec) =
                        state
                            .registry
                            .set_status(&id, AgentStatus::Running, Utc::now())
                    {
                        state.persist("agent", |store| store.upsert_agent(&rec));
                    }
                    state.emit(EventKind::AgentRestored {
                        agent: id.clone(),
                        pid: Some(pid),
                        stale: brief.stale.len(),
                    });
                    // Delivered as a message, so it arrives by whatever
                    // route the runtime already reads: the inbox, a hook's
                    // injected context, `wait_for_messages`.
                    state.send(
                        "agentd".to_owned(),
                        Destination::Agent(id.clone()),
                        "restored".to_owned(),
                        brief.payload(&record),
                        None,
                    );
                }
                supervisor::supervise(self.clone(), id.clone(), spawned);
                info!(agent = %id.short(), name = %record.spec.name, pid, "restored");
            }
            Err(err) => {
                warn!(agent = %id.short(), %err, "could not restore agent");
                let status = AgentStatus::Failed {
                    reason: format!("could not be restored: {err:#}"),
                };
                // Not `mark_exited`: after a clean shutdown the record is
                // already finished, and that path declines to touch a
                // record twice. The failure still has to be recorded, and
                // whatever was reclaimed a moment ago has to go back.
                let mut state = lock(&self.state);
                if let Some(record) = state.registry.set_status(&id, status.clone(), Utc::now()) {
                    state.persist("agent", |store| store.upsert_agent(&record));
                }
                let released = state.leases.release_all(&id);
                state.finish_release(&id, released, None, SummarySource::Explicit);
                state.emit(EventKind::AgentExited { agent: id, status });
            }
        }
    }

    /// Put back the leases the agent held when the last daemon stopped
    /// it. Nothing else is running yet, so a conflict here means another
    /// live holder took the resource in between; that one keeps it, and
    /// the brief says which are missing rather than pretending.
    fn reclaim_leases(self: &Arc<Self>, record: &AgentRecord) -> Vec<String> {
        let mut state = lock(&self.state);
        // A crash released nothing, so what it already holds is the truth.
        if !state.leases.by_holder(&record.id).is_empty() {
            return Vec::new();
        }
        let point: Option<RestorePoint> = state
            .store
            .document("restore_point", record.id.as_str())
            .ok()
            .flatten();
        state.store_op("restore_point", |store| {
            store.delete_document("restore_point", record.id.as_str())
        });
        let now = Utc::now();
        let Some(point) = point.filter(|p| now - p.at < RESTORE_POINT_LIFE) else {
            return Vec::new();
        };
        let mut reclaimed = Vec::new();
        for held in point.leases {
            let shown = held.resource.to_string();
            // The stored key is already physical: it was localised when the
            // lease was first taken, so it is claimed as it stands.
            let claimed = state.leases.claim(
                held.resource,
                record.id.clone(),
                held.mode,
                ttl(DEFAULT_LEASE_TTL_SECS),
                held.note,
                now,
            );
            match claimed {
                Ok(Claimed::New(mut lease)) | Ok(Claimed::Renewed(mut lease)) => {
                    lease.change_seq =
                        state.store_op("lease ledger boundary", |store| store.change_watermark());
                    state.leases.restore(lease.clone());
                    state.persist("lease", |store| store.upsert_lease(&lease));
                    state.emit(EventKind::LeaseClaimed {
                        lease: lease.clone(),
                    });
                    reclaimed.push(shown);
                }
                Err(err) => {
                    warn!(agent = %record.id.short(), resource = %shown, %err, "lease not reclaimed");
                }
            }
        }
        reclaimed
    }

    /// Everything the agent needs to pick up where it left off.
    async fn brief(self: &Arc<Self>, record: &AgentRecord) -> Brief {
        let reads: Vec<ReadMark> = lock(&self.state)
            .store
            .document("reads", record.id.as_str())
            .ok()
            .flatten()
            .unwrap_or_default();
        let read_count = reads.len();
        // Content hashing touches the disk, so it does not hold the lock.
        let stale = tokio::task::spawn_blocking(move || check_reads(&reads))
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|path| path.path)
            .collect();

        let mut state = lock(&self.state);
        let checkpoint = state
            .store
            .documents::<Checkpoint>("checkpoint", Some(&record.id))
            .unwrap_or_default()
            .into_iter()
            .max_by_key(|c| c.created_at);
        let leases = state
            .leases
            .by_holder(&record.id)
            .into_iter()
            .map(|lease| lease.resource.to_string())
            .collect();
        let cursor = record
            .project
            .as_ref()
            .and_then(|project| state.cursor(record.id.as_str(), &project.id()));
        Brief {
            checkpoint,
            read_count,
            stale,
            leases,
            reclaimed: Vec::new(),
            cursor,
        }
    }
}

/// What a restored agent is told.
struct Brief {
    checkpoint: Option<Checkpoint>,
    read_count: usize,
    stale: Vec<PathBuf>,
    leases: Vec<String>,
    /// The subset of `leases` that had to be put back, as opposed to
    /// the ones it never lost. `brief` runs after the reclaim, so
    /// `leases` already includes these.
    reclaimed: Vec<String>,
    cursor: Option<u64>,
}

impl Brief {
    fn payload(&self, record: &AgentRecord) -> Value {
        let mut text = format!(
            "You were relaunched after agentd restarted. You are still {}, so your read set, \
             journal cursor, checkpoints and leases are the ones you had.",
            record.spec.name
        );
        if let Some(checkpoint) = &self.checkpoint {
            text.push_str(&format!(
                " Your last checkpoint ({}) was: {}.",
                checkpoint.id, checkpoint.task
            ));
            if !checkpoint.next_steps.is_empty() {
                text.push_str(&format!(
                    " Next steps you recorded: {}.",
                    checkpoint.next_steps.join("; ")
                ));
            }
            text.push_str(" `resume_checkpoint` on it for the full context.");
        }
        if self.stale.is_empty() {
            text.push_str(" Nothing you had read changed while you were down.");
        } else {
            text.push_str(&format!(
                " {} path(s) you had read changed while you were down; reread them before \
                 editing: {}.",
                self.stale.len(),
                self.stale
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !self.leases.is_empty() {
            text.push_str(&format!(" You still hold {}.", self.leases.join(", ")));
        }
        json!({
            "text": text,
            "restored": true,
            "checkpoint": self.checkpoint.as_ref().map(|c| c.id.clone()),
            "task": self.checkpoint.as_ref().map(|c| c.task.clone()),
            "next_steps": self.checkpoint.as_ref().map(|c| c.next_steps.clone()),
            "reads": self.read_count,
            "stale": self.stale,
            "leases": self.leases,
            "reclaimed_leases": self.reclaimed,
            "journal_cursor": self.cursor,
        })
    }
}
