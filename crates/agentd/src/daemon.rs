//! Daemon state and request handling.
//!
//! Locking discipline: every method locks at most one mutex at a time, so
//! there is no lock ordering to get wrong. Locks are `std::sync::Mutex`
//! because no lock is ever held across an `.await`.
//!
//! Durability: the in-memory registry, lease table, and inboxes are the
//! source of truth for reads; every mutation is written through to the
//! [`Store`] so a restarted daemon rebuilds the same state.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use agentdocker_core::{
    AgentId, AgentRecord, AgentSpec, AgentStatus, Claimed, Destination, Envelope, ErrorCode, Event,
    EventKind, Lease, LeaseError, LeaseId, LeaseMode, LeaseTable, MessageId, Registry,
    RegistryError, Request, ResourceKey, Response, topic_matches,
};
use chrono::{DateTime, Duration, Utc};
use nix::errno::Errno;
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use serde_json::{Value, json};
use tokio::sync::broadcast;
use tracing::{error, info, warn};

use crate::procinfo;
use crate::store::Store;
use crate::supervisor;

/// Messages queued per agent while it has no live subscription.
const INBOX_CAPACITY: usize = 1000;
/// Leases longer than this are clamped; a TTL is a liveness bound, not a
/// reservation.
const MAX_LEASE_TTL_SECS: u64 = 24 * 60 * 60;
/// Stored event history is trimmed to this many entries.
const EVENT_HISTORY: usize = 10_000;

pub struct Daemon {
    pub home: PathBuf,
    pub socket: PathBuf,
    started: Instant,
    store: Mutex<Store>,
    registry: Mutex<Registry>,
    leases: Mutex<LeaseTable>,
    inboxes: Mutex<HashMap<AgentId, VecDeque<Envelope>>>,
    /// Number of live subscriptions per agent. Agents with one or more skip
    /// the inbox and get messages pushed directly.
    live_subscribers: Mutex<HashMap<AgentId, usize>>,
    /// Agents whose `Child` handle a supervisor task owns. Every other live
    /// agent with a pid is polled by [`Daemon::check_liveness`].
    supervised: Mutex<HashSet<AgentId>>,
    /// Next event sequence number; continues from the stored history.
    next_seq: AtomicU64,
    bus: broadcast::Sender<Envelope>,
    events: broadcast::Sender<Event>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn registry_error(err: RegistryError) -> Response {
    let code = match err {
        RegistryError::NameTaken(_) => ErrorCode::NameTaken,
        RegistryError::NotFound(_) => ErrorCode::NotFound,
        RegistryError::Ambiguous(_) => ErrorCode::Ambiguous,
    };
    Response::error(code, err.to_string())
}

fn lease_error(err: LeaseError) -> Response {
    let code = match err {
        LeaseError::Conflict { .. } => ErrorCode::Conflict,
        LeaseError::NotFound(_) => ErrorCode::NotFound,
        LeaseError::NotHolder { .. } => ErrorCode::Forbidden,
    };
    Response::error(code, err.to_string())
}

fn ttl(secs: u64) -> Duration {
    let secs = i64::try_from(secs.min(MAX_LEASE_TTL_SECS)).unwrap_or(i64::MAX);
    Duration::seconds(secs)
}

fn default_name(id: &AgentId) -> String {
    format!("agent-{}", &id.as_str()[..6])
}

/// Is there a process with this pid? `EPERM` means it exists but belongs to
/// someone else, which still counts as alive. Zero and out-of-range values
/// would address process groups, so they are never alive.
fn process_exists(pid: u32) -> bool {
    let Ok(raw) = i32::try_from(pid) else {
        return false;
    };
    if raw <= 0 {
        return false;
    }
    match kill(Pid::from_raw(raw), None) {
        Ok(()) | Err(Errno::EPERM) => true,
        Err(_) => false,
    }
}

/// Does the pid still belong to the process that registered it? Compared by
/// start time, with slack for clock granularity. Lenient when either side
/// is unknown: a pid that exists but can't be inspected is assumed alive.
fn same_process(pid: u32, recorded: Option<DateTime<Utc>>) -> bool {
    match (recorded, procinfo::start_time(pid)) {
        (Some(recorded), Some(current)) => (current - recorded).num_seconds().abs() <= 2,
        _ => true,
    }
}

/// An unsupervised live agent whose process the reaper must check.
struct Candidate {
    id: AgentId,
    pid: Option<u32>,
    process_started_at: Option<DateTime<Utc>>,
    managed: bool,
}

impl Daemon {
    /// Open (or create) the state database under `home` and restore state.
    pub fn open(home: PathBuf, socket: PathBuf) -> anyhow::Result<Self> {
        std::fs::create_dir_all(&home)?;
        let store = Store::open(&home.join("state.db"))?;
        Self::with_store(home, socket, store)
    }

    pub fn with_store(home: PathBuf, socket: PathBuf, store: Store) -> anyhow::Result<Self> {
        let now = Utc::now();
        let mut registry = Registry::new();
        for mut record in store.load_agents()? {
            if record.managed && record.status == AgentStatus::Created {
                // The previous daemon stopped between creating the record and
                // spawning the process, so nothing is running for it.
                warn!(agent = %record.id.short(), name = %record.spec.name, "agent never started; recording failure");
                record.status = AgentStatus::Failed {
                    reason: "daemon restarted before the process was spawned".to_owned(),
                };
                record.finished_at = Some(now);
                store.upsert_agent(&record)?;
            }
            match registry.insert(record.clone()) {
                Ok(()) => {}
                Err(RegistryError::NameTaken(name)) => {
                    // Two live records with one name can only come from a
                    // damaged store: keep the first, retire the rest so the
                    // store and the registry agree.
                    warn!(%name, agent = %record.id.short(), "duplicate live agent in store; recording it as exited");
                    record.status = AgentStatus::Exited { code: None };
                    record.finished_at = Some(now);
                    store.upsert_agent(&record)?;
                    if let Err(err) = registry.insert(record) {
                        warn!(%err, "skipping stored agent");
                    }
                }
                Err(err) => warn!(%err, "skipping stored agent"),
            }
        }
        let mut leases = LeaseTable::new();
        for lease in store.load_leases()? {
            let holder_live = registry
                .get(&lease.holder)
                .is_some_and(|a| a.status.is_live());
            if holder_live {
                leases.restore(lease);
            } else {
                warn!(lease = %lease.id, holder = %lease.holder.short(), resource = %lease.resource, "dropping lease with no live holder");
                store.delete_lease(&lease.id)?;
            }
        }
        let inboxes = store.load_inboxes()?;
        let next_seq = store.max_event_seq()? + 1;
        info!(
            agents = registry.len(),
            leases = leases.len(),
            inboxes = inboxes.len(),
            "state restored"
        );

        let (bus, _) = broadcast::channel(1024);
        let (events, _) = broadcast::channel(1024);
        Ok(Self {
            home,
            socket,
            started: Instant::now(),
            store: Mutex::new(store),
            registry: Mutex::new(registry),
            leases: Mutex::new(leases),
            inboxes: Mutex::new(inboxes),
            live_subscribers: Mutex::new(HashMap::new()),
            supervised: Mutex::new(HashSet::new()),
            next_seq: AtomicU64::new(next_seq),
            bus,
            events,
        })
    }

    pub fn log_path(&self, id: &AgentId) -> PathBuf {
        self.home.join("logs").join(format!("{id}.log"))
    }

    pub fn subscribe_events(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    /// The last `limit` stored events, oldest first.
    pub fn recent_events(&self, limit: usize) -> Vec<Event> {
        if limit == 0 {
            return Vec::new();
        }
        lock(&self.store)
            .recent_events(limit)
            .unwrap_or_else(|err| {
                error!(%err, "failed to load event history");
                Vec::new()
            })
    }

    /// Run a write against the store, logging rather than propagating
    /// failure: the in-memory state has already changed and the daemon
    /// must keep serving.
    fn persist(&self, what: &str, write: impl FnOnce(&Store) -> anyhow::Result<()>) {
        if let Err(err) = write(&lock(&self.store)) {
            error!(%what, %err, "failed to persist state");
        }
    }

    pub fn emit(&self, kind: EventKind) {
        let mut event = Event::new(kind, Utc::now());
        event.seq = self.next_seq.fetch_add(1, Ordering::SeqCst);
        self.persist("event", |store| store.append_event(&event));
        let _ = self.events.send(event);
    }

    pub fn resolve(&self, reference: &str) -> Result<AgentId, Box<Response>> {
        lock(&self.registry)
            .resolve(reference)
            .map_err(|err| Box::new(registry_error(err)))
    }

    pub fn is_live(&self, id: &AgentId) -> bool {
        lock(&self.registry)
            .get(id)
            .is_some_and(|a| a.status.is_live())
    }

    /// Handle every non-streaming request.
    pub async fn handle(self: &Arc<Self>, request: Request) -> Response {
        match request {
            Request::Ping => Response::Pong {
                version: env!("CARGO_PKG_VERSION").to_owned(),
                uptime_secs: self.started.elapsed().as_secs(),
            },
            Request::Run { spec } => self.run(spec).await,
            Request::Register { spec, pid } => self.register(spec, pid),
            Request::Deregister { agent } => self.deregister(&agent),
            Request::Stop { agent, force } => self.stop(&agent, force),
            Request::Remove { agent } => self.remove(&agent),
            Request::List { all } => Response::Agents {
                agents: lock(&self.registry).list(all),
            },
            Request::Inspect { agent } => self.inspect(&agent),
            Request::Heartbeat { agent } => match self.resolve(&agent) {
                Ok(id) => {
                    self.touch(&id);
                    Response::Ok
                }
                Err(response) => *response,
            },
            Request::Send {
                from,
                to,
                kind,
                payload,
                reply_to,
            } => self.send(from, &to, kind, payload, reply_to),
            Request::Inbox { agent, drain } => self.inbox(&agent, drain),
            Request::Claim {
                agent,
                resource,
                mode,
                ttl_secs,
                note,
            } => self.claim(&agent, resource, mode, ttl_secs, note),
            Request::Renew {
                agent,
                lease,
                ttl_secs,
            } => self.renew(&agent, &lease, ttl_secs),
            Request::Release { agent, lease } => self.release(&agent, &lease),
            Request::Leases { agent, resource } => self.leases(agent.as_deref(), resource),
            Request::Subscribe { .. } | Request::Events { .. } | Request::Logs { .. } => {
                Response::error(ErrorCode::Internal, "streaming request routed as unary")
            }
        }
    }

    async fn run(self: &Arc<Self>, spec: AgentSpec) -> Response {
        if spec.command.is_empty() {
            return Response::error(ErrorCode::Invalid, "run needs a command to launch");
        }
        let mut record = AgentRecord::new(spec, true, Utc::now());
        if record.spec.name.is_empty() {
            record.spec.name = default_name(&record.id);
        }
        if let Err(err) = lock(&self.registry).insert(record.clone()) {
            return registry_error(err);
        }
        self.persist("agent", |store| store.upsert_agent(&record));
        self.emit(EventKind::AgentCreated {
            agent: record.id.clone(),
            name: record.spec.name.clone(),
        });

        match supervisor::spawn(self, &record).await {
            Ok(spawned) => {
                let pid = spawned.pid;
                let process_started_at = procinfo::start_time(pid);
                lock(&self.supervised).insert(record.id.clone());
                let updated = {
                    let mut registry = lock(&self.registry);
                    if let Some(rec) = registry.get_mut(&record.id) {
                        rec.pid = Some(pid);
                        rec.process_started_at = process_started_at;
                    }
                    registry.set_status(&record.id, AgentStatus::Running, Utc::now())
                };
                if let Some(rec) = &updated {
                    self.persist("agent", |store| store.upsert_agent(rec));
                }
                self.emit(EventKind::AgentStarted {
                    agent: record.id.clone(),
                    pid: Some(pid),
                });
                supervisor::supervise(self.clone(), record.id.clone(), spawned);
                info!(agent = %record.id.short(), name = %record.spec.name, pid, "agent started");
                match updated {
                    Some(agent) => Response::Agent { agent },
                    None => Response::error(ErrorCode::Internal, "agent vanished after spawn"),
                }
            }
            Err(err) => {
                let status = AgentStatus::Failed {
                    reason: format!("{err:#}"),
                };
                self.mark_exited(&record.id, status);
                Response::error(ErrorCode::Internal, format!("{err:#}"))
            }
        }
    }

    fn register(&self, spec: AgentSpec, pid: Option<u32>) -> Response {
        let now = Utc::now();
        let mut record = AgentRecord::new(spec, false, now);
        if record.spec.name.is_empty() {
            record.spec.name = default_name(&record.id);
        }
        record.pid = pid;
        record.process_started_at = pid.and_then(procinfo::start_time);
        record.status = AgentStatus::Running;
        record.started_at = Some(now);
        if let Err(err) = lock(&self.registry).insert(record.clone()) {
            return registry_error(err);
        }
        self.persist("agent", |store| store.upsert_agent(&record));
        self.emit(EventKind::AgentCreated {
            agent: record.id.clone(),
            name: record.spec.name.clone(),
        });
        self.emit(EventKind::AgentStarted {
            agent: record.id.clone(),
            pid,
        });
        info!(agent = %record.id.short(), name = %record.spec.name, "agent registered");
        Response::Agent { agent: record }
    }

    fn deregister(&self, reference: &str) -> Response {
        let id = match self.resolve(reference) {
            Ok(id) => id,
            Err(response) => return *response,
        };
        if !self.is_live(&id) {
            return Response::error(ErrorCode::Invalid, "agent has already finished");
        }
        match self.mark_exited(&id, AgentStatus::Exited { code: Some(0) }) {
            Some(agent) => Response::Agent { agent },
            None => Response::error(ErrorCode::NotFound, "agent vanished"),
        }
    }

    fn stop(&self, reference: &str, force: bool) -> Response {
        let id = match self.resolve(reference) {
            Ok(id) => id,
            Err(response) => return *response,
        };
        let Some(record) = lock(&self.registry).get(&id).cloned() else {
            return Response::error(ErrorCode::NotFound, "agent vanished");
        };
        if !record.status.is_live() {
            return Response::error(
                ErrorCode::Invalid,
                format!("agent is already {}", record.status),
            );
        }
        if let Some(pid) = record.pid {
            let signal = if force {
                Signal::SIGKILL
            } else {
                Signal::SIGTERM
            };
            if let Err(err) = kill(Pid::from_raw(pid as i32), signal) {
                return Response::error(
                    ErrorCode::Internal,
                    format!("failed to signal pid {pid}: {err}"),
                );
            }
        } else if record.managed {
            return Response::error(ErrorCode::Internal, "managed agent has no pid");
        }
        if lock(&self.supervised).contains(&id) {
            // The supervisor task observes the exit and updates the record.
            return Response::Agent { agent: record };
        }
        match self.mark_exited(&id, AgentStatus::Exited { code: None }) {
            Some(agent) => Response::Agent { agent },
            None => Response::error(ErrorCode::NotFound, "agent vanished"),
        }
    }

    /// SIGTERM every managed live agent; used on daemon shutdown.
    pub fn stop_all(&self) {
        let managed: Vec<(AgentId, u32)> = lock(&self.registry)
            .live()
            .filter(|a| a.managed)
            .filter_map(|a| a.pid.map(|pid| (a.id.clone(), pid)))
            .collect();
        for (id, pid) in managed {
            info!(agent = %id.short(), pid, "stopping agent");
            let _ = kill(Pid::from_raw(pid as i32), Signal::SIGTERM);
        }
    }

    fn remove(&self, reference: &str) -> Response {
        let id = match self.resolve(reference) {
            Ok(id) => id,
            Err(response) => return *response,
        };
        if self.is_live(&id) {
            return Response::error(ErrorCode::Invalid, "agent is still live; stop it first");
        }
        lock(&self.registry).remove(&id);
        lock(&self.inboxes).remove(&id);
        self.persist("agent", |store| store.delete_agent(&id));
        self.emit(EventKind::AgentRemoved { agent: id });
        Response::Ok
    }

    fn inspect(&self, reference: &str) -> Response {
        match self.resolve(reference) {
            Ok(id) => match lock(&self.registry).get(&id) {
                Some(agent) => Response::Agent {
                    agent: agent.clone(),
                },
                None => Response::error(ErrorCode::NotFound, "agent vanished"),
            },
            Err(response) => *response,
        }
    }

    /// Record an exit: update status, release leases, emit the event.
    pub fn mark_exited(&self, id: &AgentId, status: AgentStatus) -> Option<AgentRecord> {
        let record = lock(&self.registry).set_status(id, status.clone(), Utc::now())?;
        lock(&self.supervised).remove(id);
        self.persist("agent", |store| store.upsert_agent(&record));
        let released = lock(&self.leases).release_all(id);
        for lease in released {
            self.persist("lease", |store| store.delete_lease(&lease.id));
            self.emit(EventKind::LeaseReleased { lease });
        }
        info!(agent = %id.short(), name = %record.spec.name, %status, "agent finished");
        self.emit(EventKind::AgentExited {
            agent: id.clone(),
            status,
        });
        Some(record)
    }

    /// Live agents nobody is supervising get their pid checked; a vanished
    /// process is recorded as an exit so its leases are freed. Covers agents
    /// adopted from a previous daemon run and externally registered ones.
    pub fn check_liveness(&self) {
        let supervised = lock(&self.supervised).clone();
        let candidates: Vec<Candidate> = lock(&self.registry)
            .live()
            .filter(|a| !supervised.contains(&a.id))
            // A managed agent stays `Created` while `run` is still
            // spawning it; it has no pid yet and is not gone.
            .filter(|a| !(a.managed && a.status == AgentStatus::Created))
            .map(|a| Candidate {
                id: a.id.clone(),
                pid: a.pid,
                process_started_at: a.process_started_at,
                managed: a.managed,
            })
            .collect();
        for Candidate {
            id,
            pid,
            process_started_at,
            managed,
        } in candidates
        {
            let alive = match pid {
                Some(pid) => process_exists(pid) && same_process(pid, process_started_at),
                // An external agent that gave no pid can only leave by
                // deregistering; a managed one without a pid never started.
                None => !managed,
            };
            if !alive {
                warn!(agent = %id.short(), ?pid, "process is gone; recording exit");
                self.mark_exited(&id, AgentStatus::Exited { code: None });
            }
        }
    }

    fn touch(&self, id: &AgentId) {
        let record = {
            let mut registry = lock(&self.registry);
            registry.touch(id, Utc::now());
            registry.get(id).cloned()
        };
        if let Some(record) = record {
            self.persist("agent", |store| store.upsert_agent(&record));
        }
    }

    fn send(
        &self,
        from: String,
        to: &str,
        kind: String,
        payload: Value,
        reply_to: Option<MessageId>,
    ) -> Response {
        let from = match lock(&self.registry).resolve(&from) {
            Ok(id) => id.to_string(),
            Err(RegistryError::NotFound(_)) => from,
            Err(err) => return registry_error(err),
        };
        let to = match Destination::parse(to) {
            Destination::Agent(reference) => match self.resolve(reference.as_str()) {
                Ok(id) => Destination::Agent(id),
                Err(response) => return *response,
            },
            other => other,
        };
        let envelope = Envelope::new(from, to, kind, payload, reply_to, Utc::now());

        let recipients: Vec<AgentId> = match &envelope.to {
            Destination::Agent(id) => vec![id.clone()],
            Destination::Broadcast => lock(&self.registry)
                .live()
                .filter(|a| a.id.as_str() != envelope.from)
                .map(|a| a.id.clone())
                .collect(),
            Destination::Topic(_) => Vec::new(),
        };
        let offline: Vec<AgentId> = {
            let live = lock(&self.live_subscribers);
            recipients
                .into_iter()
                .filter(|id| !live.contains_key(id))
                .collect()
        };
        if !offline.is_empty() {
            {
                let mut inboxes = lock(&self.inboxes);
                for id in &offline {
                    let queue = inboxes.entry(id.clone()).or_default();
                    if queue.len() >= INBOX_CAPACITY {
                        queue.pop_front();
                    }
                    queue.push_back(envelope.clone());
                }
            }
            self.persist("inbox", |store| {
                for id in &offline {
                    store.enqueue(id, &envelope, INBOX_CAPACITY)?;
                }
                Ok(())
            });
        }

        let subscribers = self.bus.send(envelope.clone()).unwrap_or(0);
        self.emit(EventKind::MessageSent {
            message: envelope.id.clone(),
            from: envelope.from.clone(),
            to: envelope.to.clone(),
            kind: envelope.kind.clone(),
        });
        self.touch(&AgentId::from(envelope.from.as_str()));
        Response::Sent {
            message: envelope.id,
            subscribers,
        }
    }

    fn inbox(&self, reference: &str, drain: bool) -> Response {
        let id = match self.resolve(reference) {
            Ok(id) => id,
            Err(response) => return *response,
        };
        let messages: Vec<Envelope> = {
            let mut inboxes = lock(&self.inboxes);
            if drain {
                inboxes.remove(&id).map(Vec::from).unwrap_or_default()
            } else {
                inboxes
                    .get(&id)
                    .map(|queue| queue.iter().cloned().collect())
                    .unwrap_or_default()
            }
        };
        if drain && !messages.is_empty() {
            self.persist("inbox", |store| store.clear_inbox(&id));
        }
        self.touch(&id);
        Response::Messages { messages }
    }

    /// Open a live subscription. Returns the filter plus the raw receiver so
    /// the caller can `select!` on the receiver without borrowing the filter.
    pub fn subscribe(
        self: &Arc<Self>,
        agent: Option<&str>,
        topics: Vec<String>,
    ) -> Result<(Subscription, broadcast::Receiver<Envelope>), Box<Response>> {
        let agent = agent.map(|reference| self.resolve(reference)).transpose()?;
        let receiver = self.bus.subscribe();
        let backlog = match &agent {
            Some(id) => {
                *lock(&self.live_subscribers).entry(id.clone()).or_default() += 1;
                let backlog: Vec<Envelope> = lock(&self.inboxes)
                    .remove(id)
                    .map(Vec::from)
                    .unwrap_or_default();
                if !backlog.is_empty() {
                    self.persist("inbox", |store| store.clear_inbox(id));
                }
                backlog
            }
            None => Vec::new(),
        };
        let seen = backlog.iter().map(|m| m.id.clone()).collect();
        Ok((
            Subscription {
                daemon: self.clone(),
                agent,
                topics,
                backlog,
                seen,
            },
            receiver,
        ))
    }

    fn unsubscribe(&self, agent: &AgentId) {
        let mut live = lock(&self.live_subscribers);
        if let Some(count) = live.get_mut(agent) {
            *count -= 1;
            if *count == 0 {
                live.remove(agent);
            }
        }
    }

    fn claim(
        &self,
        reference: &str,
        resource: String,
        mode: LeaseMode,
        ttl_secs: u64,
        note: Option<String>,
    ) -> Response {
        let holder = match self.resolve(reference) {
            Ok(id) => id,
            Err(response) => return *response,
        };
        self.touch(&holder);
        let result = lock(&self.leases).claim(
            ResourceKey::new(resource),
            holder.clone(),
            mode,
            ttl(ttl_secs),
            note,
            Utc::now(),
        );
        match result {
            Ok(Claimed::New(lease)) => {
                self.persist("lease", |store| store.upsert_lease(&lease));
                self.emit(EventKind::LeaseClaimed {
                    lease: lease.clone(),
                });
                Response::Lease { lease }
            }
            Ok(Claimed::Renewed(lease)) => {
                self.persist("lease", |store| store.upsert_lease(&lease));
                self.emit(EventKind::LeaseRenewed {
                    lease: lease.clone(),
                });
                Response::Lease { lease }
            }
            Err(err) => {
                let message = err.to_string();
                match err {
                    LeaseError::Conflict { resource, held_by } => {
                        warn!(agent = %holder.short(), %resource, "lease conflict");
                        self.emit(EventKind::LeaseConflict {
                            resource,
                            requester: holder,
                            held_by: held_by.iter().map(|l| l.holder.clone()).collect(),
                        });
                        Response::Error {
                            code: ErrorCode::Conflict,
                            message,
                            details: Some(json!({ "held_by": held_by })),
                        }
                    }
                    other => lease_error(other),
                }
            }
        }
    }

    fn renew(&self, reference: &str, lease: &LeaseId, ttl_secs: u64) -> Response {
        let holder = match self.resolve(reference) {
            Ok(id) => id,
            Err(response) => return *response,
        };
        self.touch(&holder);
        // Bind first so the lease guard is gone before persisting.
        let result = lock(&self.leases).renew(lease, &holder, ttl(ttl_secs), Utc::now());
        match result {
            Ok(lease) => {
                self.persist("lease", |store| store.upsert_lease(&lease));
                self.emit(EventKind::LeaseRenewed {
                    lease: lease.clone(),
                });
                Response::Lease { lease }
            }
            Err(err) => lease_error(err),
        }
    }

    fn release(&self, reference: &str, lease: &LeaseId) -> Response {
        let holder = match self.resolve(reference) {
            Ok(id) => id,
            Err(response) => return *response,
        };
        let result = lock(&self.leases).release(lease, &holder);
        match result {
            Ok(lease) => {
                self.persist("lease", |store| store.delete_lease(&lease.id));
                self.emit(EventKind::LeaseReleased {
                    lease: lease.clone(),
                });
                Response::Lease { lease }
            }
            Err(err) => lease_error(err),
        }
    }

    fn leases(&self, agent: Option<&str>, resource: Option<String>) -> Response {
        let holder = match agent.map(|reference| self.resolve(reference)).transpose() {
            Ok(holder) => holder,
            Err(response) => return *response,
        };
        let key = resource.map(ResourceKey::new);
        self.expire_leases();
        let leases: Vec<Lease> = lock(&self.leases)
            .all()
            .into_iter()
            .filter(|l| holder.as_ref().is_none_or(|h| l.holder == *h))
            .filter(|l| key.as_ref().is_none_or(|k| l.resource.overlaps(k)))
            .cloned()
            .collect();
        Response::Leases { leases }
    }

    /// Drop leases whose TTL has passed. Called on a timer and before listing.
    pub fn expire_leases(&self) {
        let expired = lock(&self.leases).expire(Utc::now());
        for lease in expired {
            info!(lease = %lease.id, holder = %lease.holder.short(), resource = %lease.resource, "lease expired");
            self.persist("lease", |store| store.delete_lease(&lease.id));
            self.emit(EventKind::LeaseExpired { lease });
        }
    }

    /// Trim stored event history. Called occasionally from the reaper.
    pub fn prune_events(&self) {
        match lock(&self.store).prune_events(EVENT_HISTORY) {
            Ok(0) => {}
            Ok(removed) => info!(removed, "pruned event history"),
            Err(err) => error!(%err, "failed to prune event history"),
        }
    }
}

/// A live message subscription. Dropping it releases the agent's live slot
/// so later messages queue in its inbox again.
pub struct Subscription {
    daemon: Arc<Daemon>,
    agent: Option<AgentId>,
    topics: Vec<String>,
    backlog: Vec<Envelope>,
    seen: HashSet<MessageId>,
}

impl Subscription {
    /// Messages that were queued while the agent was offline.
    pub fn take_backlog(&mut self) -> Vec<Envelope> {
        std::mem::take(&mut self.backlog)
    }

    pub fn wants(&self, envelope: &Envelope) -> bool {
        if self.seen.contains(&envelope.id) {
            return false;
        }
        match &envelope.to {
            Destination::Agent(id) => self.agent.as_ref() == Some(id),
            Destination::Broadcast => self
                .agent
                .as_ref()
                .is_none_or(|me| me.as_str() != envelope.from),
            Destination::Topic(topic) => self
                .topics
                .iter()
                .any(|pattern| topic_matches(pattern, topic)),
        }
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        if let Some(agent) = &self.agent {
            self.daemon.unsubscribe(agent);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn open(dir: &TempDir) -> Arc<Daemon> {
        let home = dir.path().to_path_buf();
        Arc::new(Daemon::open(home.clone(), home.join("sock")).unwrap())
    }

    fn spec(name: &str) -> AgentSpec {
        AgentSpec {
            name: name.to_owned(),
            ..AgentSpec::default()
        }
    }

    async fn register(daemon: &Arc<Daemon>, name: &str, pid: Option<u32>) -> AgentRecord {
        match daemon
            .handle(Request::Register {
                spec: spec(name),
                pid,
            })
            .await
        {
            Response::Agent { agent } => agent,
            other => panic!("unexpected {other:?}"),
        }
    }

    async fn claim(daemon: &Arc<Daemon>, agent: &str, resource: &str) -> Response {
        daemon
            .handle(Request::Claim {
                agent: agent.to_owned(),
                resource: resource.to_owned(),
                mode: LeaseMode::Exclusive,
                ttl_secs: 60,
                note: None,
            })
            .await
    }

    async fn list_leases(daemon: &Arc<Daemon>) -> Vec<Lease> {
        match daemon
            .handle(Request::Leases {
                agent: None,
                resource: None,
            })
            .await
        {
            Response::Leases { leases } => leases,
            other => panic!("unexpected {other:?}"),
        }
    }

    async fn inbox(daemon: &Arc<Daemon>, agent: &str, drain: bool) -> Vec<Envelope> {
        match daemon
            .handle(Request::Inbox {
                agent: agent.to_owned(),
                drain,
            })
            .await
        {
            Response::Messages { messages } => messages,
            other => panic!("unexpected {other:?}"),
        }
    }

    /// A pid that certainly no longer exists: a child we already reaped.
    fn dead_pid() -> u32 {
        let child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        let mut child = child;
        child.wait().unwrap();
        pid
    }

    #[tokio::test]
    async fn state_survives_restart() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let alpha = register(&daemon, "alpha", Some(std::process::id())).await;
        assert!(matches!(
            claim(&daemon, "alpha", "task:1").await,
            Response::Lease { .. }
        ));
        daemon
            .handle(Request::Send {
                from: "user".into(),
                to: "alpha".into(),
                kind: "chat".into(),
                payload: json!({ "text": "hello" }),
                reply_to: None,
            })
            .await;
        drop(daemon);

        let daemon = open(&dir);
        let agents = lock(&daemon.registry).list(true);
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].id, alpha.id);
        assert!(agents[0].status.is_live());

        let leases = list_leases(&daemon).await;
        assert_eq!(leases.len(), 1);
        assert_eq!(leases[0].holder, alpha.id);
        // The restored lease still blocks others.
        assert!(matches!(
            register(&daemon, "beta", None).await.status,
            AgentStatus::Running
        ));
        assert!(matches!(
            claim(&daemon, "beta", "task:1").await,
            Response::Error {
                code: ErrorCode::Conflict,
                ..
            }
        ));

        let queued = inbox(&daemon, "alpha", true).await;
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].payload["text"], "hello");
        drop(daemon);

        // Draining was persisted too.
        let daemon = open(&dir);
        assert!(inbox(&daemon, "alpha", false).await.is_empty());
        // Our own pid is alive, so the liveness check leaves alpha alone.
        daemon.check_liveness();
        assert!(daemon.is_live(&alpha.id));
    }

    #[tokio::test]
    async fn vanished_process_is_recorded_as_exited_and_freed() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let ghost = register(&daemon, "ghost", Some(dead_pid())).await;
        assert!(matches!(
            claim(&daemon, "ghost", "path:/tmp/x").await,
            Response::Lease { .. }
        ));
        // Without a pid there is nothing to poll, so this one stays live.
        let quiet = register(&daemon, "quiet", None).await;

        daemon.check_liveness();

        assert!(!daemon.is_live(&ghost.id));
        assert!(daemon.is_live(&quiet.id));
        assert!(list_leases(&daemon).await.is_empty());
        drop(daemon);

        let daemon = open(&dir);
        assert!(!daemon.is_live(&ghost.id));
        assert!(lock(&daemon.leases).is_empty());
    }

    #[tokio::test]
    async fn remove_forgets_agent_and_inbox() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let done = register(&daemon, "done", None).await;
        daemon
            .handle(Request::Send {
                from: "user".into(),
                to: "done".into(),
                kind: "chat".into(),
                payload: json!({ "text": "late" }),
                reply_to: None,
            })
            .await;
        daemon
            .handle(Request::Deregister {
                agent: "done".into(),
            })
            .await;
        assert!(matches!(
            daemon
                .handle(Request::Remove {
                    agent: "done".into()
                })
                .await,
            Response::Ok
        ));
        drop(daemon);

        let daemon = open(&dir);
        assert!(lock(&daemon.registry).get(&done.id).is_none());
        assert!(lock(&daemon.inboxes).is_empty());
    }

    #[tokio::test]
    async fn events_are_stored_for_replay() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        register(&daemon, "a", None).await;
        let recent = daemon.recent_events(10);
        assert!(matches!(
            recent.as_slice(),
            [
                Event {
                    kind: EventKind::AgentCreated { .. },
                    ..
                },
                Event {
                    kind: EventKind::AgentStarted { .. },
                    ..
                }
            ]
        ));
        assert!(daemon.recent_events(0).is_empty());
    }

    #[tokio::test]
    async fn event_seqs_increase_and_survive_restart() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let mut receiver = daemon.subscribe_events();
        register(&daemon, "a", None).await;
        let replayed = daemon.recent_events(10);
        assert_eq!(replayed.len(), 2);
        assert!(replayed[0].seq < replayed[1].seq);
        // The same event reaches a live subscriber with the same seq, which
        // is what lets the server drop it after a replay.
        let live = receiver.recv().await.unwrap();
        assert_eq!(live.seq, replayed[0].seq);
        drop(daemon);

        let daemon = open(&dir);
        register(&daemon, "b", None).await;
        let all = daemon.recent_events(10);
        assert_eq!(all.len(), 4);
        assert!(all.windows(2).all(|pair| pair[0].seq < pair[1].seq));
    }

    #[tokio::test]
    async fn spawning_agents_are_not_reaped() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        // What `run` looks like between registry insert and spawn completing.
        let record = AgentRecord::new(spec("spawning"), true, Utc::now());
        let id = record.id.clone();
        lock(&daemon.registry).insert(record).unwrap();

        daemon.check_liveness();
        assert!(daemon.is_live(&id));
    }

    #[tokio::test]
    async fn restore_retires_half_spawned_duplicates_and_orphaned_leases() {
        let dir = TempDir::new().unwrap();
        let now = Utc::now();
        let first_id;
        {
            let store = Store::open(&dir.path().join("state.db")).unwrap();
            let mut first = AgentRecord::new(spec("twin"), false, now);
            first.status = AgentStatus::Running;
            first_id = first.id.clone();
            let mut second = AgentRecord::new(spec("twin"), false, now + Duration::seconds(1));
            second.status = AgentStatus::Running;
            let half_spawned = AgentRecord::new(spec("half"), true, now);
            for record in [&first, &second, &half_spawned] {
                store.upsert_agent(record).unwrap();
            }
            let lease = |id: &str, holder: AgentId, resource: &str| Lease {
                id: LeaseId::from(id),
                resource: ResourceKey::new(resource),
                holder,
                mode: LeaseMode::Exclusive,
                acquired_at: now,
                expires_at: now + Duration::hours(1),
                note: None,
            };
            store
                .upsert_lease(&lease("kept", first.id.clone(), "task:kept"))
                .unwrap();
            store
                .upsert_lease(&lease("orphan", AgentId::from("ghost"), "task:orphan"))
                .unwrap();
            store
                .upsert_lease(&lease("twin2", second.id.clone(), "task:twin2"))
                .unwrap();
        }

        let daemon = open(&dir);
        let agents = lock(&daemon.registry).list(true);
        assert_eq!(agents.len(), 3, "every stored record is still listed");
        let live: Vec<&AgentRecord> = agents.iter().filter(|a| a.status.is_live()).collect();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].id, first_id);
        assert!(
            agents
                .iter()
                .any(|a| a.spec.name == "half" && matches!(a.status, AgentStatus::Failed { .. }))
        );

        let leases = list_leases(&daemon).await;
        assert_eq!(leases.len(), 1);
        assert_eq!(leases[0].id, LeaseId::from("kept"));
        drop(daemon);

        // The retired records and dropped leases were written back.
        let store = Store::open(&dir.path().join("state.db")).unwrap();
        assert_eq!(store.load_leases().unwrap().len(), 1);
        let live_in_store = store
            .load_agents()
            .unwrap()
            .into_iter()
            .filter(|a| a.status.is_live())
            .count();
        assert_eq!(live_in_store, 1);
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[tokio::test]
    async fn recycled_pid_is_not_mistaken_for_the_agent() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let stale = register(&daemon, "stale", Some(std::process::id())).await;
        // Pretend the process that registered started long before the one
        // holding the pid now (as after a reboot).
        lock(&daemon.registry)
            .get_mut(&stale.id)
            .unwrap()
            .process_started_at = Some(Utc::now() - Duration::hours(24 * 30));
        let fresh = register(&daemon, "fresh", Some(std::process::id())).await;
        assert!(fresh.process_started_at.is_some());

        daemon.check_liveness();
        assert!(!daemon.is_live(&stale.id));
        assert!(daemon.is_live(&fresh.id));
    }

    #[test]
    fn invalid_pids_are_never_alive() {
        assert!(!process_exists(0));
        assert!(!process_exists(u32::MAX));
        assert!(process_exists(std::process::id()));
    }
}
