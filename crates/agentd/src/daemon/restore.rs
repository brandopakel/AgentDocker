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
    /// Recheck after asynchronous log preparation, immediately before spawn.
    /// Restore checks may have yielded while a stop, a storage failure or an
    /// expired/reassigned lease changed whether this writer can start.
    pub(crate) fn validate_native_launch(&self, expected: &AgentRecord) -> anyhow::Result<()> {
        let mut state = lock(&self.state);
        storage_ready(&state)?;
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
                .store_op("restore protection", |store| {
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
                let protected = state.leases.clone();
                let released = state.leases.release_all(&id);
                state.finish_release(&id, released, None, SummarySource::Explicit);
                if state.storage_error.is_some() {
                    state.leases = protected;
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
        let spawned = supervisor::spawn(self, &record).await?;
        let pid = spawned.pid;
        let process_started_at = procinfo::start_time(pid);
        if let Some(session) = spawned.session.clone() {
            lock(&self.sessions).insert(id.clone(), session);
        }
        let persisted = {
            let mut state = lock(&self.state);
            // Supervision must own even a child whose registration fails.
            state.supervised.insert(id.clone(), spawned.control.clone());
            let mut running = state.registry.get(&id).cloned().unwrap_or(record.clone());
            let cancelled = !running.spec.restore;
            running.pid = Some(pid);
            running.process_started_at = process_started_at;
            running.process_group = Some(pid);
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
            if !cancelled {
                state.persist("restore completion", |store| {
                    store.finish_restore(&running, &event)
                });
            }
            if state.storage_error.is_none() && !cancelled {
                // The supervisor owns the child/PID on every path. Expose the
                // Running record only alongside its committed restore event.
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
            !cancelled && state.storage_error.is_none()
        };
        if !persisted {
            // Kill through the child we own; the supervisor reaps the leader
            // and waits for descendants before releasing any protection.
            spawned.control.send_replace(Some(true));
        }
        let supervision = supervisor::supervise(self.clone(), id.clone(), spawned);
        if !persisted {
            if tokio::time::timeout(SUPERVISION_STOP_TIMEOUT, supervision)
                .await
                .is_err()
            {
                warn!(agent = %id.short(), "restore cleanup still supervised; protection retained");
            }
            // Timed-out supervision remains alive and owns the child/group.
            // The caller must not release protection without confirmed exit.
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
            .store_op("restore_point", |store| {
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
                    .store_op("lease ledger boundary", |store| store.change_watermark())
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
        state.persist("restore preparation", |store| {
            store.prepare_restore(&record, &point, &leases, &events)
        });
        storage_ready(&state)?;
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
                .store_op("restore reads", |store| {
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
            .store_op("restore checkpoint", |store| {
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
        let Response::Agent { agent } = daemon.handle(Request::Run { spec }).await else {
            panic!("fixture launch failed")
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
                    command: vec!["sh".into(), "-c".into(), "exec sleep 30".into()],
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
        assert!(!supervisor::group_exists(record.pid.unwrap()));
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
        let (_dir, daemon, record, _marker) = saved().await;
        lock(&daemon.state)
            .store
            .reject_event_for_test("agent_restored");
        daemon.restore_agents().await;
        let state = lock(&daemon.state);
        assert!(state.storage_error.is_some());
        assert!(state.supervised.is_empty(), "failed launch must be reaped");
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
