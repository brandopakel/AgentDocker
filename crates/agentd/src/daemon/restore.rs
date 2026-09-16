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
/// Written on shutdown and retained while restoration is being prepared,
/// only for agents that asked to be restored. Stopping an agent correctly
/// releases its leases — the resource really is free once nothing is
/// working on it — so by the time a new daemon starts, the lease table no
/// longer says what the agent had. After a crash there is no restore
/// point initially: current leases supply a point before relaunch begins.
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
    /// Take back the managed agents whose session owners outlived the
    /// previous daemon. Runs before the liveness sweep, so an agent with a
    /// live owner is never retired for lacking a supervisor here.
    pub async fn reattach_owners(self: &Arc<Self>) {
        let candidates: Vec<AgentRecord> = lock(&self.state)
            .registry
            .live()
            .filter(|a| a.managed && a.owner.is_some())
            .cloned()
            .collect();
        for record in candidates {
            let id = record.id.clone();
            match supervisor::reattach(self, &record).await {
                Ok(supervisor::Reattached::Running(spawned)) => {
                    if let Some(session) = spawned.session.clone() {
                        lock(&self.sessions).insert(id.clone(), session);
                    }
                    let owner_pid = spawned.owner.pid;
                    {
                        let mut state = lock(&self.state);
                        state.supervised.insert(id.clone(), spawned.control.clone());
                        state.emit(EventKind::AgentOwnerReattached {
                            agent: id.clone(),
                            owner_pid,
                        });
                    }
                    info!(agent = %id.short(), owner_pid, "reattached to the session owner");
                    supervisor::supervise(self.clone(), id, *spawned);
                }
                Ok(supervisor::Reattached::Exited(report)) => {
                    info!(agent = %id.short(), "the agent finished while no daemon was watching");
                    self.recover_owner_exit(report);
                }
                Err(error) => {
                    let owner = record.owner.clone().expect("candidates have owners");
                    if !supervisor::owner_alive(&owner) {
                        let reason = format!("session owner lost across restart: {error:#}");
                        warn!(agent = %id.short(), %reason, "cannot reattach");
                        self.emit(EventKind::AgentOwnerLost {
                            agent: id.clone(),
                            reason: reason.clone(),
                        });
                        self.mark_exited(&id, AgentStatus::Failed { reason });
                        continue;
                    }
                    // The owner lives but did not answer in time: the agent
                    // stays owned, out of the liveness sweep's and restore's
                    // reach, and attachment is retried until the owner
                    // answers, exits or dies. Other recovery is not held up.
                    warn!(agent = %id.short(), %error, "session owner alive but not answering; retrying");
                    let (control, stopped) = tokio::sync::watch::channel(None);
                    lock(&self.state).supervised.insert(id.clone(), control);
                    let daemon = self.clone();
                    tokio::spawn(async move {
                        daemon.retry_reattach(record, stopped).await;
                    });
                }
            }
        }
    }

    /// Keep trying to attach to a living owner. Ends when the owner answers
    /// (supervision resumes, a stop asked meanwhile is delivered), exits
    /// (its report is recorded) or dies (the agent is recorded as lost).
    async fn retry_reattach(
        self: Arc<Self>,
        record: AgentRecord,
        stopped: tokio::sync::watch::Receiver<Option<bool>>,
    ) {
        let id = record.id.clone();
        let owner = record.owner.clone().expect("candidates have owners");
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            match supervisor::reattach(&self, &record).await {
                Ok(supervisor::Reattached::Running(spawned)) => {
                    if let Some(session) = spawned.session.clone() {
                        lock(&self.sessions).insert(id.clone(), session);
                    }
                    let owner_pid = spawned.owner.pid;
                    {
                        let mut state = lock(&self.state);
                        // Stop requests use this same mutex. Read the pending
                        // stop and replace its target atomically so neither
                        // the placeholder nor the new controller can miss it.
                        if let Some(force) = *stopped.borrow() {
                            spawned.control.send_replace(Some(force));
                        }
                        state.supervised.insert(id.clone(), spawned.control.clone());
                        state.emit(EventKind::AgentOwnerReattached {
                            agent: id.clone(),
                            owner_pid,
                        });
                    }
                    info!(agent = %id.short(), owner_pid, "reattached to the session owner after retrying");
                    supervisor::supervise(self.clone(), id, *spawned);
                    return;
                }
                Ok(supervisor::Reattached::Exited(report)) => {
                    info!(agent = %id.short(), "the agent finished while no daemon was watching");
                    self.recover_owner_exit(report);
                    return;
                }
                Err(error) if supervisor::owner_alive(&owner) => {
                    tracing::debug!(agent = %id.short(), %error, "session owner still not answering");
                }
                Err(error) => {
                    let reason = format!("session owner lost across restart: {error:#}");
                    warn!(agent = %id.short(), %reason, "cannot reattach");
                    self.emit(EventKind::AgentOwnerLost {
                        agent: id.clone(),
                        reason: reason.clone(),
                    });
                    self.mark_exited(&id, AgentStatus::Failed { reason });
                    return;
                }
            }
        }
    }

    /// Persist a recovered exit before acknowledging its owner. Check the
    /// current generation under the same lock as the write: recovery must not
    /// retire an agent that was restarted while attachment was pending.
    fn recover_owner_exit(&self, report: agentdocker_core::session::ExitReport) {
        let durable = {
            let mut state = lock(&self.state);
            if !state.registry.get(&report.agent).is_some_and(|record| {
                record.owner.as_ref() == Some(&report.owner)
                    && record.pid == Some(report.child.pid)
                    && record.process_started_at == Some(report.child.started_at)
            }) {
                return;
            }
            state.mark_exited_durably(&report.agent, supervisor::exit_status(&report))
        };
        if durable {
            tokio::spawn(supervisor::acknowledge_recovered_exit(
                self.home.clone(),
                report,
            ));
        } else {
            warn!(agent = %report.agent, "recovered exit not durable; owner report kept without acknowledgement");
        }
    }

    /// Recheck after asynchronous log preparation, immediately before spawn.
    /// Restore checks may have yielded while a stop, a storage failure or an
    /// expired/reassigned lease changed whether this writer can start.
    pub(crate) fn validate_native_launch(&self, expected: &AgentRecord) -> anyhow::Result<()> {
        self.refresh_policy_for(expected.project.as_ref());
        let mut state = lock(&self.state);
        storage_ready(&state)?;
        if let Some(response) = state.run_refusal(expected) {
            return Err(policies::LaunchDenied(response).into());
        }
        // The statuses a launch may legitimately start from: `created`
        // for a fresh `run`, and an ended one for a restart or a
        // restore, which start the same agent again under its own id.
        // What this rejects is a record that is gone, one whose restore
        // flag changed under it, and — the case the check exists for —
        // one that is already `running` or `stopping`, meaning somebody
        // else started or stopped it while this launch was preparing.
        anyhow::ensure!(
            state.registry.get(&expected.id).is_some_and(|record| {
                matches!(
                    record.status,
                    AgentStatus::Created | AgentStatus::Exited { .. } | AgentStatus::Failed { .. }
                ) && record.spec.restore == expected.spec.restore
                    && record.spec.restart == expected.spec.restart
                    && record.restarts == expected.restarts
            }),
            "launch was cancelled or its starting identity changed"
        );
        if expected.spec.restore {
            let point: Option<RestorePoint> = state
                .store_read("restore protection", |store| {
                    store.document("restore_point", expected.id.as_str())
                })
                .ok_or_else(|| storage_error(&state))?;
            if let Some(point) = point {
                let now = Utc::now();
                anyhow::ensure!(
                    point.leases.iter().all(|held| state
                        .leases
                        .by_holder(&expected.id)
                        .into_iter()
                        .any(|lease| lease.resource == held.resource
                            && lease.mode == held.mode
                            && !lease.is_expired(now))),
                    "restored protection expired or changed before launch"
                );
            }
        }
        Ok(())
    }

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
                .cloned()
                .collect()
        };
        if candidates.is_empty() {
            return;
        }
        info!(agents = candidates.len(), "restoring agents");
        for record in candidates {
            let id = record.id.clone();
            if let Err(error) = self.restore_one(record).await {
                warn!(agent = %id.short(), %error, "could not restore agent");
                // Storage failure freezes durable protection. Ordinary failures
                // can be recorded and released because no writer was started.
                let mut state = lock(&self.state);
                if state.storage_error.is_some() {
                    break;
                }
                let status = AgentStatus::Failed {
                    reason: format!("could not be restored: {error:#}"),
                };
                if let Some(mut record) = state.registry.get(&id).cloned() {
                    record.status = status.clone();
                    record.finished_at = Some(Utc::now());
                    record.last_seen = Utc::now();
                    let mut event = Event::new(
                        EventKind::AgentExited {
                            agent: id.clone(),
                            status: status.clone(),
                        },
                        Utc::now(),
                    );
                    event.seq = state.next_seq;
                    if state
                        .store_op("restore failure", |store| {
                            store.agent_transition(&record, &event)
                        })
                        .is_none()
                    {
                        break;
                    }
                    *state
                        .registry
                        .get_mut(&id)
                        .expect("restore identity retained") = record;
                    state.next_seq += 1;
                    let _ = state.events.send(event);
                }
                if state
                    .store_op("restore_point", |store| {
                        store.delete_document("restore_point", id.as_str())
                    })
                    .is_none()
                {
                    break;
                }
                state.release_all(id.as_str(), None, SummarySource::Explicit);
                if state.storage_error.is_some() {
                    break;
                }
            }
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
        let Some(current) = state.registry.get(id) else {
            return;
        };
        if !current.spec.restore {
            return;
        }
        let mut record = current.clone();
        record.spec.restore = false;
        let mut event = Event::new(
            EventKind::AgentRestoreCleared {
                agent: record.id.clone(),
            },
            Utc::now(),
        );
        event.seq = state.next_seq;
        let committed = state.persist("restore intent", |store| {
            store.agent_transition(&record, &event)?;
            store.delete_document("restore_point", id.as_str())
        });
        // The intent stays until its clearing is durable: a fenced or
        // failed write leaves memory saying what the disk still says.
        if committed == Persisted::Committed {
            *state.registry.get_mut(id).expect("resolved agent") = record;
            state.next_seq += 1;
            let _ = state.events.send(event);
        }
    }

    async fn restore_one(self: &Arc<Self>, record: AgentRecord) -> anyhow::Result<()> {
        let id = record.id.clone();
        // Process inspection is host I/O: do it without the coordination lock.
        if record.process_group.is_some_and(supervisor::group_exists) {
            warn!(agent = %id.short(), "process group is still alive; not restoring");
            return Ok(());
        }
        let Some((record, reclaimed)) = self.prepare_restore(&record)? else {
            return Ok(());
        };
        if watchable(&record) {
            self.ensure_watched(&record)
                .await
                .map_err(anyhow::Error::msg)?;
        }
        let mut brief = self.brief(&record).await?;
        brief.reclaimed = reclaimed;
        {
            let state = lock(&self.state);
            storage_ready(&state)?;
            // A stop may arrive while attachment/content checks are pending.
            anyhow::ensure!(
                state
                    .registry
                    .get(&id)
                    .is_some_and(|current| current.spec.restore),
                "restoration cancelled by explicit stop"
            );
        }
        let mut spawned = supervisor::spawn(self, &record).await?;
        let pid = spawned.pid;
        let process_started_at = Some(spawned.process_started_at);
        if let Some(session) = spawned.session.clone() {
            lock(&self.sessions).insert(id.clone(), session);
        }
        let persisted = {
            let mut state = lock(&self.state);
            // Supervision must own even a child whose registration fails.
            state.supervised.insert(id.clone(), spawned.control.clone());
            let mut running = state.registry.get(&id).cloned().unwrap_or(record.clone());
            let cancelled = !running.spec.restore || state.run_refusal(&record).is_some();
            running.pid = Some(pid);
            running.process_started_at = process_started_at;
            running.process_group = Some(pid);
            running.owner = Some(spawned.owner.clone());
            running.status = AgentStatus::Running;
            running.started_at = Some(Utc::now());
            running.finished_at = None;
            running.last_seen = Utc::now();
            let mut event = Event::new(
                EventKind::AgentRestored {
                    agent: id.clone(),
                    pid: Some(pid),
                    stale: brief.stale.len(),
                },
                Utc::now(),
            );
            event.seq = state.next_seq;
            // The supervisor owns the child/PID on every path. Expose the
            // Running record only alongside its committed restore event: a
            // write the fence skipped commits nothing, so nothing is exposed
            // and the child is stopped below like any failed completion.
            let committed = !cancelled
                && state.persist("restore completion", |store| {
                    store.finish_restore(&running, &event)
                }) == Persisted::Committed;
            if committed {
                *state
                    .registry
                    .get_mut(&id)
                    .expect("restore identity retained") = running;
                state.next_seq += 1;
                let _ = state.events.send(event);
                state.send(
                    "agentd".to_owned(),
                    Destination::Agent(id.clone()),
                    "restored".to_owned(),
                    brief.payload(&record),
                    None,
                );
            }
            committed
        };
        let activation_error = if persisted {
            spawned.activate("restored").await.err()
        } else {
            None
        };
        if !persisted || activation_error.is_some() {
            // Kill through the child we own; the supervisor reaps the leader
            // and waits for descendants before releasing any protection.
            spawned.control.send_replace(Some(true));
        }
        let supervision = supervisor::supervise(self.clone(), id.clone(), spawned);
        if !persisted || activation_error.is_some() {
            if tokio::time::timeout(SUPERVISION_STOP_TIMEOUT, supervision)
                .await
                .is_err()
            {
                warn!(agent = %id.short(), "restore cleanup still supervised; protection retained");
            }
            // The supervisor records exit and releases protection only after
            // confirmed cleanup; a timeout retains that responsibility.
            if let Some(error) = activation_error {
                warn!(agent = %id.short(), %error, "restored command could not execute");
            }
            return Ok(());
        }
        info!(agent = %id.short(), name = %record.spec.name, pid, "restored");
        Ok(())
    }

    /// Stage a live starting identity, the complete required lease set and
    /// replay events atomically. Retain the point until launch is recorded so
    /// a restart between preparation and spawn can recover the same intent.
    fn prepare_restore(
        &self,
        previous: &AgentRecord,
    ) -> anyhow::Result<Option<(AgentRecord, Vec<String>)>> {
        let mut state = lock(&self.state);
        storage_ready(&state)?;
        let point: Option<RestorePoint> = state
            .store_read("restore_point", |store| {
                store.document("restore_point", previous.id.as_str())
            })
            .ok_or_else(|| storage_error(&state))?;
        let now = Utc::now();
        let point = match point {
            Some(point) if now - point.at < RESTORE_POINT_LIFE => point,
            Some(_) => return Ok(None),
            None if !previous.status.is_live() || previous.status == AgentStatus::Created => {
                return Ok(None);
            }
            None => RestorePoint {
                at: now,
                leases: state
                    .leases
                    .by_holder(&previous.id)
                    .into_iter()
                    .map(|lease| HeldLease {
                        resource: lease.resource.clone(),
                        mode: lease.mode,
                        note: lease.note.clone(),
                    })
                    .collect(),
            },
        };
        let Some(current) = state.registry.get(&previous.id) else {
            return Ok(None);
        };
        if !current.spec.restore || current.pid != previous.pid || current.status != previous.status
        {
            return Ok(None);
        }
        anyhow::ensure!(
            !state
                .registry
                .live()
                .any(|agent| agent.id != previous.id && agent.spec.name == previous.spec.name),
            "another live agent now owns this name"
        );
        let mut record = current.clone();
        record.status = AgentStatus::Created;
        record.pid = None;
        record.process_started_at = None;
        record.process_group = None;
        record.finished_at = None;
        record.last_seen = now;
        // Claiming on a trial table expires old entries. Persist expiration
        // with its events first, so swapping in that table loses no evidence.
        state.expire_leases_at(now);
        storage_ready(&state)?;
        let mut planned = state.leases.clone();
        let mut leases = Vec::new();
        let mut reclaimed = Vec::new();
        for held in &point.leases {
            if planned.by_holder(&record.id).into_iter().any(|lease| {
                lease.resource == held.resource && lease.mode == held.mode && !lease.is_expired(now)
            }) {
                continue;
            }
            let claimed = planned.claim(
                held.resource.clone(),
                record.id.clone(),
                held.mode,
                ttl(DEFAULT_LEASE_TTL_SECS),
                held.note.clone(),
                now,
            )?;
            let mut lease = match claimed {
                Claimed::New(lease) | Claimed::Renewed(lease) => lease,
            };
            lease.change_seq = Some(
                state
                    .store_read("lease ledger boundary", |store| store.change_watermark())
                    .ok_or_else(|| storage_error(&state))?,
            );
            planned.restore(lease.clone());
            reclaimed.push(lease.resource.to_string());
            leases.push(lease);
        }
        let mut events: Vec<Event> = leases
            .iter()
            .map(|lease| {
                Event::new(
                    EventKind::LeaseClaimed {
                        lease: lease.clone(),
                    },
                    now,
                )
            })
            .collect();
        events.push(Event::new(
            EventKind::AgentRestoring {
                agent: record.id.clone(),
            },
            now,
        ));
        for (index, event) in events.iter_mut().enumerate() {
            event.seq = state.next_seq + index as u64;
        }
        let committed = state.persist("restore preparation", |store| {
            store.prepare_restore(&record, &point, &leases, &events)
        });
        if committed != Persisted::Committed {
            return Err(storage_error(&state));
        }
        state.leases = planned;
        *state
            .registry
            .get_mut(&record.id)
            .expect("restore identity retained") = record.clone();
        state.next_seq += events.len() as u64;
        for event in events {
            let _ = state.events.send(event);
        }
        Ok(Some((record, reclaimed)))
    }

    /// Everything the agent needs to pick up where it left off.
    async fn brief(self: &Arc<Self>, record: &AgentRecord) -> anyhow::Result<Brief> {
        let reads: Vec<ReadMark> = {
            let mut state = lock(&self.state);
            state
                .store_read("restore reads", |store| {
                    store.document("reads", record.id.as_str())
                })
                .ok_or_else(|| storage_error(&state))?
                .unwrap_or_default()
        };
        let read_count = reads.len();
        // Content hashing touches the disk, so it does not hold the lock.
        let stale = tokio::task::spawn_blocking(move || check_reads(&reads))
            .await?
            .into_iter()
            .map(|path| path.path)
            .collect();

        let mut state = lock(&self.state);
        let checkpoint = state
            .store_read("restore checkpoint", |store| {
                store.documents::<Checkpoint>("checkpoint", Some(&record.id))
            })
            .ok_or_else(|| storage_error(&state))?
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
        storage_ready(&state)?;
        Ok(Brief {
            checkpoint,
            read_count,
            stale,
            leases,
            reclaimed: Vec::new(),
            cursor,
        })
    }
}

fn storage_error(state: &State) -> anyhow::Error {
    if state.storage_error.is_none() && state.fenced() {
        return anyhow::anyhow!(
            "coordination is being transferred; restoration is left to the daemon that writes next"
        );
    }
    anyhow::anyhow!(
        "storage unavailable: {}",
        state.storage_error.as_deref().unwrap_or("unknown failure")
    )
}

fn storage_ready(state: &State) -> anyhow::Result<()> {
    if state.storage_error.is_some() {
        Err(storage_error(state))
    } else {
        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn an_exit_the_store_cannot_keep_leaves_the_owner_report_in_place() {
        failed_live_exit(false).await;
    }

    #[tokio::test]
    async fn an_already_failed_store_never_acknowledges_a_live_owner_exit() {
        failed_live_exit(true).await;
    }

    /// Hold the child until storage is poisoned. Test both failure during the
    /// exit write and a previous failure that takes mark_exited's early return.
    async fn failed_live_exit(already_failed: bool) {
        let dir = TempDir::new().unwrap();
        let daemon =
            Arc::new(Daemon::open(dir.path().join("state"), dir.path().join("sock")).unwrap());
        let Response::Agent { agent } = daemon
            .handle(Request::Run {
                spec: AgentSpec {
                    name: "poisoned".into(),
                    command: vec![
                        "sh".into(),
                        "-c".into(),
                        "while [ ! -e finish ]; do sleep 0.02; done; exit 5".into(),
                    ],
                    workdir: Some(dir.path().to_path_buf()),
                    ..Default::default()
                },
            })
            .await
        else {
            panic!("launch failed")
        };
        {
            let mut state = lock(&daemon.state);
            state.store.reject_writes_for_test();
            if already_failed {
                let _ = state.persist("prior failure", |store| store.upsert_agent(&agent));
            }
            assert_eq!(state.storage_error.is_some(), already_failed);
        }
        std::fs::write(dir.path().join("finish"), b"exit now").unwrap();
        let exit = agentdocker_core::session::exit_path(&daemon.home, &agent.id);
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if !lock(&daemon.state).supervised.contains_key(&agent.id) {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "supervision did not end"
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        let state = lock(&daemon.state);
        assert!(state.storage_error.is_some());
        assert!(
            exit.exists(),
            "the owner's exit report must survive an exit the store could not keep"
        );
        let report: agentdocker_core::session::ExitReport =
            serde_json::from_slice(&std::fs::read(&exit).unwrap()).unwrap();
        assert_eq!(report.agent, agent.id);
        assert_eq!(report.code, Some(5));
    }

    #[tokio::test]
    async fn recovered_disk_exit_is_durable_before_acknowledgement_and_cleanup() {
        recovered_exit(0, false, false).await;
    }

    #[tokio::test]
    async fn failed_recovered_exit_write_keeps_the_report_without_acknowledgement() {
        recovered_exit(1, false, false).await;
    }

    #[tokio::test]
    async fn previously_failed_store_keeps_recovered_exit_without_acknowledgement() {
        recovered_exit(2, false, false).await;
    }

    #[tokio::test]
    async fn recovered_exit_does_not_acknowledge_another_agent_on_the_socket() {
        recovered_exit(0, true, false).await;
    }

    #[tokio::test]
    async fn recovered_exit_cleanup_preserves_another_generations_report() {
        recovered_exit(0, false, true).await;
    }

    async fn recovered_exit(storage_failure: u8, wrong_agent: bool, replace_report: bool) {
        use agentdocker_core::session::{
            ChildIdentity, ExitReport, FORMAT, OwnerCommand, OwnerHello, SessionOwner, exit_path,
            socket_path,
        };
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let dir = TempDir::new().unwrap();
        let daemon =
            Arc::new(Daemon::open(dir.path().join("state"), dir.path().join("sock")).unwrap());
        let Response::Agent { mut agent } = daemon
            .handle(Request::Register {
                spec: AgentSpec {
                    name: "disk-exit".into(),
                    ..Default::default()
                },
                pid: None,
                session: None,
            })
            .await
        else {
            panic!("register failed")
        };
        let owner = SessionOwner {
            pid: std::process::id(),
            started_at: agentdocker_host::procinfo::start_time(std::process::id()).unwrap(),
        };
        let child = ChildIdentity {
            pid: 1234,
            started_at: Utc::now(),
            tty: false,
        };
        agent.managed = true;
        agent.pid = Some(child.pid);
        agent.process_started_at = Some(child.started_at);
        agent.owner = Some(owner.clone());
        agent.status = AgentStatus::Running;
        {
            let mut state = lock(&daemon.state);
            *state.registry.get_mut(&agent.id).unwrap() = agent.clone();
            state.store.upsert_agent(&agent).unwrap();
            if storage_failure != 0 {
                state.store.reject_writes_for_test();
            }
            if storage_failure == 2 {
                let _ = state.persist("previous failure", |store| store.upsert_agent(&agent));
            }
            assert_eq!(state.storage_error.is_some(), storage_failure == 2);
        }
        let report = ExitReport {
            agent: agent.id.clone(),
            owner: owner.clone(),
            child: child.clone(),
            code: Some(7),
            signal: None,
            log_flushed: true,
            at: Utc::now(),
        };
        let socket = socket_path(&daemon.home, &agent.id);
        let exit = exit_path(&daemon.home, &agent.id);
        std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
        std::fs::write(&exit, serde_json::to_vec(&report).unwrap()).unwrap();
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let held = agentdocker_host::lock::try_exclusive(&socket.with_extension("lock"))
            .unwrap()
            .unwrap();
        daemon.reattach_owners().await;
        if storage_failure != 0 {
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(100), listener.accept())
                    .await
                    .is_err(),
                "no connection or ACK before durable exit"
            );
            let state = lock(&daemon.state);
            assert!(state.storage_error.is_some());
            assert_eq!(
                state.store.load_agents().unwrap()[0].status,
                AgentStatus::Running
            );
            assert_eq!(
                std::fs::read(&exit).unwrap(),
                serde_json::to_vec(&report).unwrap()
            );
            return;
        }
        let (stream, _) =
            tokio::time::timeout(std::time::Duration::from_secs(5), listener.accept())
                .await
                .unwrap()
                .unwrap();
        let (reader, mut writer) = stream.into_split();
        let hello = OwnerHello {
            format: FORMAT,
            agent: if wrong_agent {
                AgentId::from("stranger")
            } else {
                agent.id.clone()
            },
            owner_pid: owner.pid,
            owner_started_at: owner.started_at,
            child: Some(child),
            activated: true,
            output_offset: 0,
        };
        let mut line = serde_json::to_vec(&hello).unwrap();
        line.push(b'\n');
        writer.write_all(&line).await.unwrap();
        let mut lines = BufReader::new(reader).lines();
        let received = tokio::time::timeout(std::time::Duration::from_secs(5), lines.next_line())
            .await
            .unwrap()
            .unwrap();
        if wrong_agent {
            assert!(received.is_none(), "a different agent must receive no ACK");
            assert!(exit.exists());
            return;
        }
        assert_eq!(
            serde_json::from_str::<OwnerCommand>(&received.unwrap()).unwrap(),
            OwnerCommand::Acknowledge
        );
        assert_eq!(
            lock(&daemon.state).store.load_agents().unwrap()[0].status,
            AgentStatus::Exited { code: Some(7) },
            "the exit is already durable at ACK"
        );
        let replacement = if replace_report {
            let mut changed = report.clone();
            changed.child.started_at += Duration::seconds(1);
            let bytes = serde_json::to_vec(&changed).unwrap();
            std::fs::write(&exit, &bytes).unwrap();
            Some(bytes)
        } else {
            None
        };
        drop(held);
        // Successful cleanup releases the stable lock and removes only this
        // generation's exit. A replacement must survive the same cleanup path.
        if let Some(bytes) = replacement {
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            assert_eq!(std::fs::read(&exit).unwrap(), bytes);
        } else {
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
            while exit.exists() {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "exit report was not cleaned up"
                );
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
        }
    }

    /// An owner that is alive but slow to answer at daemon startup keeps its
    /// agent owned: no liveness retirement, no relaunch, and once it answers
    /// supervision resumes and its exit is recorded exactly.
    #[tokio::test]
    async fn a_slow_owner_at_startup_keeps_its_agent_owned_until_it_answers() {
        slow_owner(false).await;
    }

    #[tokio::test]
    async fn a_stop_queued_while_the_owner_is_unavailable_reaches_the_new_controller() {
        slow_owner(true).await;
    }

    async fn slow_owner(pending_stop: bool) {
        use agentdocker_core::session::{
            ChildIdentity, ExitReport, FORMAT, OwnerCommand, OwnerHello, OwnerReport, SessionOwner,
            socket_path,
        };
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let dir = TempDir::new().unwrap();
        let home = dir.path().join("state");
        let daemon = Arc::new(Daemon::open(home.clone(), dir.path().join("sock")).unwrap());
        // The "owner" is this test process; the child is a real sleep, so
        // both identities are live and verifiable.
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let child_pid = child.id();
        let child_started_at = agentdocker_host::procinfo::start_time(child_pid).unwrap();
        let owner = SessionOwner {
            pid: std::process::id(),
            started_at: agentdocker_host::procinfo::start_time(std::process::id()).unwrap(),
        };
        let Response::Agent { agent } = daemon
            .handle(Request::Register {
                spec: AgentSpec {
                    name: "slow-owned".into(),
                    command: vec!["sleep".into(), "30".into()],
                    workdir: Some(dir.path().to_path_buf()),
                    ..Default::default()
                },
                pid: None,
                session: None,
            })
            .await
        else {
            panic!("register failed")
        };
        {
            let mut state = lock(&daemon.state);
            let record = state.registry.get_mut(&agent.id).unwrap();
            record.managed = true;
            record.pid = Some(child_pid);
            record.process_started_at = Some(child_started_at);
            record.process_group = Some(child_pid);
            record.owner = Some(owner.clone());
            record.status = AgentStatus::Running;
            let record = record.clone();
            state.store.upsert_agent(&record).unwrap();
        }
        let socket = socket_path(&home, &agent.id);
        std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
        let identity = ChildIdentity {
            pid: child_pid,
            started_at: child_started_at,
            tty: false,
        };
        // The fake owner binds only after the daemon's first attempt has
        // given up, then answers like a real one.
        let fake = {
            let socket = socket.clone();
            let owner = owner.clone();
            let identity = identity.clone();
            let agent_id = agent.id.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
                let listener = tokio::net::UnixListener::bind(&socket).unwrap();
                let (stream, _) = listener.accept().await.unwrap();
                let (reader, mut writer) = stream.into_split();
                let mut lines = BufReader::new(reader).lines();
                let framed = |value: String| {
                    let mut line = value.into_bytes();
                    line.push(b'\n');
                    line
                };
                let hello = OwnerHello {
                    format: FORMAT,
                    agent: agent_id.clone(),
                    owner_pid: owner.pid,
                    owner_started_at: owner.started_at,
                    child: Some(identity.clone()),
                    activated: true,
                    output_offset: 0,
                };
                for text in [
                    serde_json::to_string(&hello).unwrap(),
                    serde_json::to_string(&OwnerReport::Prepared {
                        child: identity.clone(),
                    })
                    .unwrap(),
                    serde_json::to_string(&OwnerReport::Activated).unwrap(),
                ] {
                    writer.write_all(&framed(text)).await.unwrap();
                }
                if pending_stop {
                    let command = lines.next_line().await.unwrap().unwrap();
                    assert_eq!(
                        serde_json::from_str::<OwnerCommand>(&command).unwrap(),
                        OwnerCommand::Stop { force: true }
                    );
                }
                // Report an exit and expect the acknowledgement.
                let report = ExitReport {
                    agent: agent_id,
                    owner,
                    child: identity,
                    code: Some(9),
                    signal: None,
                    log_flushed: true,
                    at: Utc::now(),
                };
                let text = serde_json::to_string(&OwnerReport::Exited { status: report }).unwrap();
                writer.write_all(&framed(text)).await.unwrap();
                loop {
                    let Some(line) = lines.next_line().await.unwrap() else {
                        return false;
                    };
                    if matches!(
                        serde_json::from_str::<OwnerCommand>(&line),
                        Ok(OwnerCommand::Acknowledge)
                    ) {
                        return true;
                    }
                }
            })
        };
        daemon.reattach_owners().await;
        // First attempt gave up, but the agent stays owned and running.
        {
            let state = lock(&daemon.state);
            assert!(
                state.supervised.contains_key(&agent.id),
                "kept owned while retrying"
            );
            assert_eq!(
                state.registry.get(&agent.id).unwrap().status,
                AgentStatus::Running
            );
        }
        daemon.check_liveness();
        assert_eq!(
            lock(&daemon.state).registry.get(&agent.id).unwrap().status,
            AgentStatus::Running,
            "the liveness sweep leaves an owned agent alone"
        );
        if pending_stop {
            let response = daemon
                .handle(Request::Stop {
                    agent: agent.id.to_string(),
                    force: true,
                })
                .await;
            assert!(!matches!(response, Response::Error { .. }), "{response:?}");
        }
        let acknowledged = tokio::time::timeout(std::time::Duration::from_secs(10), fake)
            .await
            .expect("the retry attached in time")
            .unwrap();
        assert!(acknowledged, "the exit was acknowledged after recording");
        assert_eq!(
            lock(&daemon.state).registry.get(&agent.id).unwrap().status,
            AgentStatus::Exited { code: Some(9) }
        );
        let _ = child.kill();
        let _ = child.wait();
    }

    async fn saved() -> (TempDir, Arc<Daemon>, AgentRecord, PathBuf) {
        let dir = TempDir::new().unwrap();
        let home = dir.path().join("state");
        let socket = dir.path().join("host.sock");
        let daemon = Arc::new(Daemon::open(home.clone(), socket.clone()).unwrap());
        let spec = AgentSpec {
            name: "restore-fixture".into(),
            workdir: Some(dir.path().to_path_buf()),
            command: vec!["sh".into(), "-c".into(), "exec sleep 30".into()],
            restore: true,
            ..Default::default()
        };
        let agent = match daemon.handle(Request::Run { spec }).await {
            Response::Agent { agent } => agent,
            other => panic!("fixture launch failed: {other:?}"),
        };
        assert!(matches!(
            daemon
                .handle(Request::Claim {
                    agent: agent.id.to_string(),
                    resource: "task:restore-protection".into(),
                    mode: LeaseMode::Exclusive,
                    ttl_secs: 300,
                    wait_secs: 0,
                    note: None,
                    amount: None,
                })
                .await,
            Response::Lease { .. }
        ));
        daemon.stop_all().await;
        drop(daemon);
        let daemon = Arc::new(Daemon::open(home, socket).unwrap());
        let marker = dir.path().join("writer-started");
        let record = {
            let mut state = lock(&daemon.state);
            let record = state.registry.get_mut(&agent.id).unwrap();
            record.spec.command = vec![
                "sh".into(),
                "-c".into(),
                "printf started > \"$1\"; exec sleep 30".into(),
                "fixture".into(),
                marker.display().to_string(),
            ];
            let record = record.clone();
            state.store.upsert_agent(&record).unwrap();
            record
        };
        (dir, daemon, record, marker)
    }

    #[tokio::test]
    async fn failed_initial_launch_persistence_also_reaps_its_owned_group() {
        let dir = TempDir::new().unwrap();
        let daemon =
            Arc::new(Daemon::open(dir.path().join("state"), dir.path().join("host.sock")).unwrap());
        lock(&daemon.state)
            .store
            .reject_event_for_test("agent_started");
        let response = daemon
            .handle(Request::Run {
                spec: AgentSpec {
                    name: "failed-initial-launch".into(),
                    command: vec![
                        "sh".into(),
                        "-c".into(),
                        "printf executed > must-not-execute; exec sleep 30".into(),
                    ],
                    workdir: Some(dir.path().to_path_buf()),
                    ..Default::default()
                },
            })
            .await;
        assert!(matches!(
            response,
            Response::Error {
                code: ErrorCode::StorageUnavailable,
                ..
            }
        ));
        let state = lock(&daemon.state);
        assert!(state.supervised.is_empty());
        let record = state.registry.all().next().unwrap();
        assert_eq!(record.status, AgentStatus::Created);
        assert!(record.pid.is_none());
        assert_eq!(
            state.store.load_agents().unwrap()[0].status,
            AgentStatus::Created
        );
        assert!(!dir.path().join("must-not-execute").exists());
    }

    #[tokio::test]
    async fn failed_restore_preparation_never_starts_a_writer_or_consumes_its_point() {
        for statement in [
            "CREATE TRIGGER fail BEFORE INSERT ON leases BEGIN SELECT RAISE(ABORT, 'lease fault'); END;",
            "CREATE TRIGGER fail BEFORE INSERT ON events WHEN json_extract(NEW.json, '$.kind.event') = 'agent_restoring' BEGIN SELECT RAISE(ABORT, 'event fault'); END;",
            "CREATE TRIGGER fail BEFORE INSERT ON agents WHEN json_extract(NEW.json, '$.status.state') = 'created' BEGIN SELECT RAISE(ABORT, 'identity fault'); END;",
        ] {
            let (_dir, daemon, record, marker) = saved().await;
            let conn = rusqlite::Connection::open(daemon.home.join("state.db")).unwrap();
            conn.execute_batch(statement).unwrap();
            daemon.restore_agents().await;
            let state = lock(&daemon.state);
            assert!(
                state.storage_error.is_some(),
                "fixture trigger must fire: {statement}"
            );
            assert!(!marker.exists(), "no writer starts on failed preparation");
            assert!(state.supervised.is_empty());
            assert!(
                state.store.load_leases().unwrap().is_empty(),
                "atomic lease rollback"
            );
            assert!(
                state
                    .store
                    .document::<RestorePoint>("restore_point", record.id.as_str())
                    .unwrap()
                    .is_some()
            );
            assert_eq!(state.store.load_agents().unwrap()[0].status, record.status);
        }
    }

    #[tokio::test]
    async fn failed_prelaunch_cleanup_preserves_memory_and_durable_protection() {
        for statement in [
            "CREATE TRIGGER fail BEFORE INSERT ON agents WHEN json_extract(NEW.json, '$.status.state') = 'failed' BEGIN SELECT RAISE(ABORT, 'status fault'); END;",
            "CREATE TRIGGER fail BEFORE DELETE ON documents WHEN OLD.kind = 'restore_point' BEGIN SELECT RAISE(ABORT, 'point fault'); END;",
            "CREATE TRIGGER fail BEFORE DELETE ON leases BEGIN SELECT RAISE(ABORT, 'release fault'); END;",
            "CREATE TRIGGER fail BEFORE INSERT ON events WHEN json_extract(NEW.json, '$.kind.event') = 'agent_exited' BEGIN SELECT RAISE(ABORT, 'exit event fault'); END;",
        ] {
            let (_dir, daemon, record, marker) = saved().await;
            {
                let mut state = lock(&daemon.state);
                let current = state.registry.get_mut(&record.id).unwrap();
                current.spec.command = vec!["/agentdocker-missing-fixture-executable".into()];
                let current = current.clone();
                state.store.upsert_agent(&current).unwrap();
            }
            let connection = rusqlite::Connection::open(daemon.home.join("state.db")).unwrap();
            connection.execute_batch(statement).unwrap();
            daemon.restore_agents().await;
            let state = lock(&daemon.state);
            assert!(state.storage_error.is_some(), "{statement}");
            assert_eq!(state.leases.by_holder(&record.id).len(), 1, "{statement}");
            assert_eq!(state.store.load_leases().unwrap().len(), 1, "{statement}");
            assert_eq!(
                state.registry.get(&record.id).unwrap().status,
                state.store.load_agents().unwrap()[0].status
            );
            assert!(!marker.exists());
            assert!(state.supervised.is_empty());
        }
    }

    #[tokio::test]
    async fn failed_restore_completion_kills_and_reaps_the_owned_group() {
        let (_dir, daemon, record, marker) = saved().await;
        lock(&daemon.state)
            .store
            .reject_event_for_test("agent_restored");
        daemon.restore_agents().await;
        let state = lock(&daemon.state);
        assert!(state.storage_error.is_some());
        assert!(state.supervised.is_empty(), "failed launch must be reaped");
        assert!(
            !marker.exists(),
            "failed completion cannot execute the command"
        );
        let current = state.registry.get(&record.id).unwrap();
        assert_eq!(current.status, AgentStatus::Created);
        assert!(
            current.pid.is_none(),
            "uncommitted Running identity stays private to supervision"
        );
        assert_eq!(state.leases.by_holder(&record.id).len(), 1);
        assert!(
            state
                .store
                .document::<RestorePoint>("restore_point", record.id.as_str())
                .unwrap()
                .is_some()
        );
        assert_eq!(
            state.store.load_leases().unwrap().len(),
            1,
            "failed storage retains durable protection"
        );
    }

    #[tokio::test]
    async fn native_exit_faults_retain_the_record_leases_channel_and_journal() {
        use agentdocker_core::channel::{Channel, ChannelId, ChannelSubject};
        for fault in [
            "CREATE TRIGGER fail BEFORE INSERT ON events WHEN json_extract(NEW.json, '$.kind.event') = 'agent_exited' BEGIN SELECT RAISE(ABORT, 'exit event'); END;",
            "CREATE TRIGGER fail BEFORE DELETE ON leases BEGIN SELECT RAISE(ABORT, 'lease delete'); END;",
            "CREATE TRIGGER fail BEFORE INSERT ON documents WHEN NEW.kind = 'channel' AND json_extract(NEW.json, '$.closed_at') IS NOT NULL BEGIN SELECT RAISE(ABORT, 'channel close'); END;",
            "CREATE TRIGGER fail BEFORE INSERT ON journal BEGIN SELECT RAISE(ABORT, 'journal'); END;",
        ] {
            let (_dir, daemon, record, _marker) = saved().await;
            daemon.restore_agents().await;
            let channel = Channel {
                id: ChannelId::from("exit-fixture"),
                project: record.project.as_ref().unwrap().id(),
                subject: ChannelSubject::Task {
                    task: "exit fixture".into(),
                },
                members: vec![record.id.clone()],
                opened_by: None,
                opened_at: Utc::now(),
                reviews: Vec::new(),
                closed_at: None,
                resolution: None,
            };
            let journal_before = {
                let mut state = lock(&daemon.state);
                state
                    .store
                    .put_document("channel", channel.id.as_str(), &channel)
                    .unwrap();
                state.channels.insert(channel.id.clone(), channel.clone());
                state.store.max_journal_seq(&channel.project).unwrap()
            };
            let connection = rusqlite::Connection::open(daemon.home.join("state.db")).unwrap();
            connection.execute_batch(fault).unwrap();
            let response = daemon
                .handle(Request::Stop {
                    agent: record.id.to_string(),
                    force: true,
                })
                .await;
            assert!(matches!(response, Response::Agent { .. }), "{response:?}");
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
            while !lock(&daemon.state).supervised.is_empty() {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "owned fixture did not exit"
                );
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            let state = lock(&daemon.state);
            assert!(
                state.storage_error.is_some(),
                "fault was not reached: {fault}"
            );
            let current = state.registry.get(&record.id).unwrap();
            assert_eq!(current.status, AgentStatus::Stopping);
            assert_eq!(state.store.load_agents().unwrap()[0].status, current.status);
            assert_eq!(state.leases.by_holder(&record.id).len(), 1);
            assert_eq!(state.store.load_leases().unwrap().len(), 1);
            assert_eq!(state.channels.get(&channel.id).unwrap(), &channel);
            assert_eq!(
                state
                    .store
                    .document::<Channel>("channel", channel.id.as_str())
                    .unwrap(),
                Some(channel.clone())
            );
            assert_eq!(
                state.store.max_journal_seq(&channel.project).unwrap(),
                journal_before
            );
            assert!(!supervisor::group_exists(current.pid.unwrap()));
        }
    }

    #[tokio::test]
    async fn a_restore_waits_for_coverage_and_failed_attachment_prevents_its_first_edit() {
        let (dir, daemon, record, marker) = saved().await;
        std::fs::write(dir.path().join("Agentfile.toml"), "").unwrap();
        {
            let project = project::discover(dir.path());
            let mut state = lock(&daemon.state);
            let current = state.registry.get_mut(&record.id).unwrap();
            current.project = Some(project);
            let current = current.clone();
            state.store.upsert_agent(&current).unwrap();
        }
        daemon.expect_watcher();
        let (sender, mut pending) = tokio::sync::mpsc::channel(8);
        daemon.set_watcher_attach(sender);
        let restoring = {
            let daemon = daemon.clone();
            tokio::spawn(async move { daemon.restore_agents().await })
        };
        let attachment = tokio::time::timeout(std::time::Duration::from_secs(2), pending.recv())
            .await
            .unwrap()
            .expect("restore requests actual checkout attachment");
        assert!(
            !marker.exists(),
            "worker cannot edit while attachment is pending"
        );
        attachment
            .ack
            .send(Err("fixture watcher unavailable".into()))
            .unwrap();
        restoring.await.unwrap();
        assert!(!marker.exists(), "no worker on failed attachment");
        assert!(lock(&daemon.state).supervised.is_empty());
    }

    /// While coordination is offered to a successor, a restore writes
    /// nothing and changes nothing: no lease moves in memory, no writer
    /// starts, the restore point stays, and the store has no fault. The
    /// daemon that writes next — this one after an abort — restores it.
    #[tokio::test]
    async fn a_fenced_restore_leaves_the_intent_for_the_daemon_that_writes_next() {
        let (_dir, daemon, record, marker) = saved().await;
        let leases_before = lock(&daemon.state).leases.by_holder(&record.id).len();
        let seq_before = lock(&daemon.state).next_seq;
        daemon.offer_transfer(1).expect("nothing in flight");

        daemon.restore_agents().await;
        {
            let state = lock(&daemon.state);
            assert!(
                state.storage_error.is_none(),
                "a fenced skip is not a fault"
            );
            assert!(!marker.exists(), "no writer starts while fenced");
            assert!(state.supervised.is_empty());
            assert_eq!(state.leases.by_holder(&record.id).len(), leases_before);
            assert_eq!(
                state.registry.get(&record.id).unwrap().status,
                record.status,
                "not even the starting identity moved"
            );
            assert!(
                state
                    .store
                    .document::<RestorePoint>("restore_point", record.id.as_str())
                    .unwrap()
                    .is_some()
            );
            assert_eq!(
                state.next_seq,
                seq_before + 1,
                "only the offer itself was recorded"
            );
        }
        // An explicit stop while fenced cannot clear the intent either:
        // memory keeps saying what the disk says.
        daemon.clear_restore(&record.id);
        assert!(
            lock(&daemon.state)
                .registry
                .get(&record.id)
                .unwrap()
                .spec
                .restore,
            "restore intent survives a fenced clear"
        );

        assert!(daemon.abort_transfer("test"));
        daemon.restore_agents().await;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !marker.exists() {
            assert!(
                deadline > std::time::Instant::now(),
                "restored after the abort"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(
            lock(&daemon.state).registry.get(&record.id).unwrap().status,
            AgentStatus::Running
        );
        daemon.stop_all().await;
    }

    #[tokio::test]
    async fn a_naturally_completed_restore_enabled_command_stays_completed() {
        let (_dir, daemon, record, marker) = saved().await;
        // A completed command has no shutdown point. Opt-in restore is not a
        // restart-on-success policy.
        lock(&daemon.state)
            .store
            .delete_document("restore_point", record.id.as_str())
            .unwrap();
        daemon.restore_agents().await;
        assert!(!marker.exists());
        assert!(lock(&daemon.state).supervised.is_empty());
    }

    #[tokio::test]
    async fn interrupted_preparation_recovers_the_same_identity_and_complete_protection() {
        let (_dir, daemon, record, marker) = saved().await;
        let home = daemon.home.clone();
        let socket = daemon.socket.clone();
        let (prepared, _) = daemon.prepare_restore(&record).unwrap().unwrap();
        assert_eq!(prepared.status, AgentStatus::Created);
        assert!(!marker.exists());
        // Lose the daemon after the transaction and before process spawn.
        drop(daemon);
        let daemon = Arc::new(Daemon::open(home, socket).unwrap());
        daemon.restore_agents().await;
        let (running, protected) = {
            let state = lock(&daemon.state);
            (
                state.registry.get(&record.id).unwrap().status == AgentStatus::Running,
                state.store.load_leases().unwrap().len() == 1,
            )
        };
        daemon.stop_all().await;
        assert!(running && protected);
    }

    #[tokio::test]
    async fn invalid_restore_evidence_disables_coordination_instead_of_becoming_empty_context() {
        let (_dir, daemon, record, marker) = saved().await;
        lock(&daemon.state)
            .store
            .put_document("reads", record.id.as_str(), &json!({"invalid":"read set"}))
            .unwrap();
        daemon.restore_agents().await;
        assert!(!marker.exists());
        let state = lock(&daemon.state);
        assert!(state.storage_error.is_some());
        assert!(state.supervised.is_empty());
        assert!(
            state
                .store
                .document::<RestorePoint>("restore_point", record.id.as_str())
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn protection_expiring_during_restore_checks_prevents_spawn() {
        let (_dir, daemon, record, marker) = saved().await;
        let (prepared, _) = daemon.prepare_restore(&record).unwrap().unwrap();
        {
            let mut state = lock(&daemon.state);
            let mut lease = state.leases.by_holder(&record.id)[0].clone();
            lease.expires_at = Utc::now() - Duration::seconds(1);
            state.leases.restore(lease);
        }
        assert!(supervisor::spawn(&daemon, &prepared).await.is_err());
        assert!(!marker.exists());
        assert!(lock(&daemon.state).supervised.is_empty());
    }

    #[tokio::test]
    async fn conflicting_reclaimed_protection_prevents_restore() {
        let (_dir, daemon, record, marker) = saved().await;
        let spec = AgentSpec {
            name: "rival".into(),
            ..Default::default()
        };
        let Response::Agent { agent: rival } = daemon
            .handle(Request::Register {
                spec,
                pid: None,
                session: None,
            })
            .await
        else {
            panic!("rival registration failed")
        };
        assert!(matches!(
            daemon
                .handle(Request::Claim {
                    agent: rival.id.to_string(),
                    resource: "task:restore-protection".into(),
                    mode: LeaseMode::Exclusive,
                    ttl_secs: 300,
                    wait_secs: 0,
                    note: None,
                    amount: None,
                })
                .await,
            Response::Lease { .. }
        ));
        daemon.restore_agents().await;
        assert!(!marker.exists());
        let state = lock(&daemon.state);
        assert!(matches!(
            state.registry.get(&record.id).unwrap().status,
            AgentStatus::Failed { .. }
        ));
        assert_eq!(state.leases.by_holder(&rival.id).len(), 1);
    }
}
