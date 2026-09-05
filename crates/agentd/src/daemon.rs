//! Daemon state and request handling.
//!
//! Locking discipline: every method locks at most one mutex at a time, so
//! there is no lock ordering to get wrong. Locks are `std::sync::Mutex`
//! because no lock is ever held across an `.await`.
//!
//! Durability: the in-memory registry, lease table, and inboxes are the
//! source of truth for reads; every mutation is written through to the
//! [`Store`] so a restarted daemon rebuilds the same state.

use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use agentdocker_core::{
    AgentId, AgentRecord, AgentSpec, AgentStatus, Claimed, Destination, Envelope, ErrorCode, Event,
    EventKind, Lease, LeaseError, LeaseId, LeaseMode, LeaseTable, MessageId, ProjectId, ProjectRef,
    ProjectSource, Registry, RegistryError, Request, ResourceKey, Response, topic_matches,
};
use chrono::{DateTime, Duration, Utc};
use nix::errno::Errno;
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use serde_json::{Value, json};
use tokio::sync::{Notify, broadcast};
use tracing::{error, info, warn};

use agentdocker_host::{procinfo, project};

use crate::store::Store;
use crate::supervisor;

/// Messages queued per agent while it has no live subscription.
const INBOX_CAPACITY: usize = 1000;
/// Leases longer than this are clamped; a TTL is a liveness bound, not a
/// reservation.
const MAX_LEASE_TTL_SECS: u64 = 24 * 60 * 60;
/// Stored event history is trimmed to this many entries.
const EVENT_HISTORY: usize = 10_000;
/// Longest a claim may wait for a conflicting lease to clear.
const MAX_WAIT_SECS: u64 = 600;
/// How long `git` may take to find a repository's root commit before the
/// project falls back to grouping by path for this daemon run.
const FINGERPRINT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

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
    /// Fingerprint per repository root. `None` means the lookup failed this
    /// run (git missing, no commits, or timed out): kept so it is not
    /// retried on every registration, never persisted so a restart retries.
    projects: Mutex<HashMap<PathBuf, Option<String>>>,
    /// Next event sequence number; continues from the stored history.
    next_seq: AtomicU64,
    bus: broadcast::Sender<Envelope>,
    events: broadcast::Sender<Event>,
    /// Fired by a `shutdown` request; the main loop waits on it.
    shutdown: Notify,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn registry_error(err: RegistryError) -> Response {
    let code = match err {
        RegistryError::NameTaken(_) => ErrorCode::NameTaken,
        RegistryError::NotFound(_) => ErrorCode::NotFound,
        RegistryError::Ambiguous(_) | RegistryError::ProjectAmbiguous(_) => ErrorCode::Ambiguous,
        RegistryError::ProjectNotFound(_) => ErrorCode::NotFound,
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

/// Wait until a lease overlapping `resource` is released or expires, or the
/// deadline passes. `true` means a retry is worthwhile.
async fn wait_for_release(
    events: &mut broadcast::Receiver<Event>,
    resource: &ResourceKey,
    deadline: tokio::time::Instant,
) -> bool {
    loop {
        match tokio::time::timeout_at(deadline, events.recv()).await {
            Ok(Ok(event)) => match &event.kind {
                EventKind::LeaseReleased { lease } | EventKind::LeaseExpired { lease }
                    if lease.resource.overlaps(resource) =>
                {
                    return true;
                }
                _ => {}
            },
            // Events were dropped; a retry costs nothing.
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => return true,
            Ok(Err(broadcast::error::RecvError::Closed)) | Err(_) => return false,
        }
    }
}

/// `path` as a `file:` key of `project`, if it lies in the project's
/// checkout (the worktree first, then the main root).
fn file_key(project: &ProjectRef, path: &Path) -> Option<ResourceKey> {
    [project.dir(), project.root.as_path()]
        .into_iter()
        .find_map(|base| path.strip_prefix(base).ok())
        .map(|relative| ResourceKey::file(&project.id(), relative))
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
        let projects: HashMap<PathBuf, Option<String>> = store
            .load_projects()?
            .into_iter()
            .map(|(root, fingerprint)| (root, Some(fingerprint)))
            .collect();
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
            projects: Mutex::new(projects),
            next_seq: AtomicU64::new(next_seq),
            bus,
            events,
            shutdown: Notify::new(),
        })
    }

    pub fn log_path(&self, id: &AgentId) -> PathBuf {
        self.home.join("logs").join(format!("{id}.log"))
    }

    pub fn subscribe_events(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    /// Resolves once a client has asked the daemon to exit.
    pub async fn shutdown_requested(&self) {
        self.shutdown.notified().await;
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
            Request::Register { spec, pid } => self.register(spec, pid).await,
            Request::Deregister { agent } => self.deregister(&agent),
            Request::Stop { agent, force } => self.stop(&agent, force),
            Request::Remove { agent } => self.remove(&agent),
            Request::List {
                all,
                project,
                labels,
            } => self.list(all, project, labels).await,
            Request::Inspect { agent } => self.inspect(&agent),
            Request::Heartbeat { agent } => match self.resolve(&agent) {
                Ok(id) => {
                    self.touch(&id);
                    Response::Ok
                }
                Err(response) => *response,
            },
            Request::Shutdown => {
                info!("shutdown requested by a client");
                self.shutdown.notify_one();
                Response::Ok
            }
            Request::Send {
                from,
                to,
                kind,
                payload,
                reply_to,
            } => self.send(from, &to, kind, payload, reply_to).await,
            Request::Inbox { agent, drain } => self.inbox(&agent, drain),
            Request::Claim {
                agent,
                resource,
                mode,
                ttl_secs,
                note,
                wait_secs,
            } => {
                self.claim(&agent, resource, mode, ttl_secs, note, wait_secs)
                    .await
            }
            Request::Renew {
                agent,
                lease,
                ttl_secs,
            } => self.renew(&agent, &lease, ttl_secs),
            Request::Release { agent, lease } => self.release(&agent, &lease),
            Request::ReleaseAll { agent } => self.release_all(&agent),
            Request::Leases { agent, resource } => self.leases(agent.as_deref(), resource).await,
            Request::Subscribe { .. } | Request::Events { .. } | Request::Logs { .. } => {
                Response::error(ErrorCode::Internal, "streaming request routed as unary")
            }
        }
    }

    async fn run(self: &Arc<Self>, spec: AgentSpec) -> Response {
        if spec.command.is_empty() {
            return Response::error(ErrorCode::Invalid, "run needs a command to launch");
        }
        let project = self.project_for(spec.workdir.clone(), true).await;
        let mut record = AgentRecord::new(spec, true, Utc::now());
        record.project = project;
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
            project: record.project.as_ref().map(ProjectRef::id),
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

    async fn register(&self, spec: AgentSpec, pid: Option<u32>) -> Response {
        let project = self.project_for(spec.workdir.clone(), true).await;
        let now = Utc::now();
        let mut record = AgentRecord::new(spec, false, now);
        record.project = project;
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
            project: record.project.as_ref().map(ProjectRef::id),
        });
        self.emit(EventKind::AgentStarted {
            agent: record.id.clone(),
            pid,
        });
        info!(agent = %record.id.short(), name = %record.spec.name, "agent registered");
        Response::Agent { agent: record }
    }

    /// The project containing `workdir`, fingerprinted from the cache or by
    /// `git`. With `record`, a repository seen for the first time is cached,
    /// persisted, and announced; without it (resolving a `ps --project
    /// <path>` selector) nothing is written.
    async fn project_for(&self, workdir: Option<PathBuf>, record: bool) -> Option<ProjectRef> {
        let workdir = workdir?;
        let mut project = tokio::task::spawn_blocking(move || project::discover(&workdir))
            .await
            .ok()?;
        if project.source != ProjectSource::Git {
            return Some(project);
        }
        if let Some(cached) = lock(&self.projects).get(&project.root).cloned() {
            project.fingerprint = cached;
            return Some(project);
        }
        let root = project.root.clone();
        let fingerprint =
            tokio::task::spawn_blocking(move || project::fingerprint(&root, FINGERPRINT_TIMEOUT))
                .await
                .ok()
                .flatten();
        if !record {
            project.fingerprint = fingerprint;
            return Some(project);
        }
        // Two registrations can race to fingerprint one repository; the
        // first to cache wins so every agent in it gets the same id.
        let fresh = match lock(&self.projects).entry(project.root.clone()) {
            Entry::Occupied(entry) => {
                project.fingerprint = entry.get().clone();
                false
            }
            Entry::Vacant(entry) => {
                entry.insert(fingerprint.clone());
                project.fingerprint = fingerprint;
                true
            }
        };
        if fresh {
            match &project.fingerprint {
                Some(fingerprint) => {
                    self.persist("project", |store| {
                        store.upsert_project(&project.root, fingerprint)
                    });
                }
                None => warn!(
                    root = %project.root.display(),
                    "repository has no usable fingerprint (git missing, no commits, or timed out); grouping by path"
                ),
            }
            info!(project = %project.id().short(), root = %project.root.display(), "project discovered");
            self.emit(EventKind::ProjectDiscovered {
                project: ProjectRef {
                    worktree: None,
                    ..project.clone()
                },
            });
        }
        Some(project)
    }

    /// A project as a client names it: an absolute path inside it, or an id
    /// (any unique prefix) of a project some agent works in.
    async fn resolve_project(&self, selector: &str) -> Result<ProjectId, Box<Response>> {
        if Path::new(selector).is_absolute() {
            return match self.project_for(Some(PathBuf::from(selector)), false).await {
                Some(project) => Ok(project.id()),
                None => Err(Box::new(Response::error(
                    ErrorCode::Internal,
                    "project lookup failed",
                ))),
            };
        }
        lock(&self.registry)
            .resolve_project(selector)
            .map_err(|err| Box::new(registry_error(err)))
    }

    async fn list(
        &self,
        all: bool,
        project: Option<String>,
        labels: BTreeMap<String, String>,
    ) -> Response {
        let project = match project {
            None => None,
            Some(selector) => match self.resolve_project(&selector).await {
                Ok(id) => Some(id),
                Err(response) => return *response,
            },
        };
        Response::Agents {
            agents: lock(&self.registry).matching(all, project.as_ref(), &labels),
        }
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

    async fn send(
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
            Destination::Project(selector) => match self.resolve_project(selector.as_str()).await {
                Ok(id) => Destination::Project(id),
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
            Destination::Project(project) => lock(&self.registry)
                .live()
                .filter(|a| a.id.as_str() != envelope.from)
                .filter(|a| a.project.as_ref().is_some_and(|p| p.id() == *project))
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
        let project = agent.as_ref().and_then(|id| {
            lock(&self.registry)
                .get(id)
                .and_then(|a| a.project.as_ref().map(ProjectRef::id))
        });
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
                project,
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

    async fn claim(
        &self,
        reference: &str,
        resource: String,
        mode: LeaseMode,
        ttl_secs: u64,
        note: Option<String>,
        wait_secs: u64,
    ) -> Response {
        let holder = match self.resolve(reference) {
            Ok(id) => id,
            Err(response) => return *response,
        };
        self.touch(&holder);
        let resource = self
            .localise(ResourceKey::new(resource), Some(&holder), true)
            .await;
        let deadline = tokio::time::Instant::now()
            + std::time::Duration::from_secs(wait_secs.min(MAX_WAIT_SECS));
        // Subscribe before the first attempt so a release that lands between
        // a failed attempt and the wait is not missed.
        let mut events = self.subscribe_events();
        let mut reported_conflict = false;
        loop {
            let result = lock(&self.leases).claim(
                resource.clone(),
                holder.clone(),
                mode,
                ttl(ttl_secs),
                note.clone(),
                Utc::now(),
            );
            let (message, held_by) = match result {
                Ok(Claimed::New(lease)) => {
                    self.persist("lease", |store| store.upsert_lease(&lease));
                    self.emit(EventKind::LeaseClaimed {
                        lease: lease.clone(),
                    });
                    return Response::Lease { lease };
                }
                Ok(Claimed::Renewed(lease)) => {
                    self.persist("lease", |store| store.upsert_lease(&lease));
                    self.emit(EventKind::LeaseRenewed {
                        lease: lease.clone(),
                    });
                    return Response::Lease { lease };
                }
                Err(err) => {
                    let message = err.to_string();
                    match err {
                        LeaseError::Conflict { held_by, .. } => (message, held_by),
                        other => return lease_error(other),
                    }
                }
            };
            // One conflict event per request, however long it waits.
            if !reported_conflict {
                reported_conflict = true;
                warn!(agent = %holder.short(), %resource, waiting = wait_secs > 0, "lease conflict");
                self.emit(EventKind::LeaseConflict {
                    resource: resource.clone(),
                    requester: holder.clone(),
                    held_by: held_by.iter().map(|l| l.holder.clone()).collect(),
                });
            }
            if wait_secs == 0 || !wait_for_release(&mut events, &resource, deadline).await {
                return Response::Error {
                    code: ErrorCode::Conflict,
                    message,
                    details: Some(json!({ "held_by": held_by })),
                };
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

    fn release_all(&self, reference: &str) -> Response {
        let holder = match self.resolve(reference) {
            Ok(id) => id,
            Err(response) => return *response,
        };
        let released = lock(&self.leases).release_all(&holder);
        for lease in &released {
            self.persist("lease", |store| store.delete_lease(&lease.id));
            self.emit(EventKind::LeaseReleased {
                lease: lease.clone(),
            });
        }
        Response::Leases { leases: released }
    }

    async fn leases(&self, agent: Option<&str>, resource: Option<String>) -> Response {
        let holder = match agent.map(|reference| self.resolve(reference)).transpose() {
            Ok(holder) => holder,
            Err(response) => return *response,
        };
        // A path query matches both its `file:` form and, for leases held
        // outside any project, the raw path.
        let mut keys: Vec<ResourceKey> = Vec::new();
        if let Some(resource) = resource {
            let raw = ResourceKey::new(resource);
            let local = self.localise(raw.clone(), holder.as_ref(), false).await;
            if local != raw {
                keys.push(local);
            }
            keys.push(raw);
        }
        self.expire_leases();
        let leases: Vec<Lease> = lock(&self.leases)
            .all()
            .into_iter()
            .filter(|l| holder.as_ref().is_none_or(|h| l.holder == *h))
            .filter(|l| keys.is_empty() || keys.iter().any(|k| l.resource.overlaps(k)))
            .cloned()
            .collect();
        Response::Leases { leases }
    }

    /// Rewrite a `path:` key to its project-relative `file:` form when the
    /// path lies in a project: the holder's own project first (so a plain
    /// directory project still works for its members), else the repository
    /// or Agentfile root containing the path. Anything else is left alone.
    /// With `record`, a repository met this way counts as discovered.
    async fn localise(
        &self,
        key: ResourceKey,
        holder: Option<&AgentId>,
        record: bool,
    ) -> ResourceKey {
        if key.kind() != "path" || !Path::new(key.value()).is_absolute() {
            return key;
        }
        let raw = PathBuf::from(key.value());
        let path = tokio::task::spawn_blocking(move || project::canonical(&raw))
            .await
            .unwrap_or_else(|_| PathBuf::from(key.value()));
        let own =
            holder.and_then(|id| lock(&self.registry).get(id).and_then(|a| a.project.clone()));
        if let Some(file) = own.as_ref().and_then(|project| file_key(project, &path)) {
            return file;
        }
        let parent = path.parent().map(Path::to_path_buf);
        match self.project_for(parent, record).await {
            Some(found)
                if matches!(found.source, ProjectSource::Git | ProjectSource::Agentfile) =>
            {
                file_key(&found, &path).unwrap_or(key)
            }
            _ => key,
        }
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
    /// The agent's project when it subscribed, for `project:` deliveries.
    project: Option<ProjectId>,
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
            Destination::Project(project) => {
                self.project.as_ref() == Some(project)
                    && self
                        .agent
                        .as_ref()
                        .is_some_and(|me| me.as_str() != envelope.from)
            }
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
                wait_secs: 0,
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

    #[tokio::test]
    async fn claim_wait_acquires_when_the_holder_releases() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        register(&daemon, "a", None).await;
        let b = register(&daemon, "b", None).await;
        let Response::Lease { lease } = claim(&daemon, "a", "task:w").await else {
            panic!("a should hold task:w")
        };
        let waiter = {
            let daemon = daemon.clone();
            tokio::spawn(async move {
                daemon
                    .handle(Request::Claim {
                        agent: "b".into(),
                        resource: "task:w".into(),
                        mode: LeaseMode::Exclusive,
                        ttl_secs: 60,
                        note: None,
                        wait_secs: 5,
                    })
                    .await
            })
        };
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        daemon
            .handle(Request::Release {
                agent: "a".into(),
                lease: lease.id,
            })
            .await;
        let response = waiter.await.unwrap();
        assert!(matches!(response, Response::Lease { lease } if lease.holder == b.id));
    }

    #[tokio::test]
    async fn claim_wait_gives_up_at_the_deadline() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        register(&daemon, "a", None).await;
        register(&daemon, "b", None).await;
        claim(&daemon, "a", "task:w").await;
        let started = std::time::Instant::now();
        let response = daemon
            .handle(Request::Claim {
                agent: "b".into(),
                resource: "task:w".into(),
                mode: LeaseMode::Exclusive,
                ttl_secs: 60,
                note: None,
                wait_secs: 1,
            })
            .await;
        assert!(started.elapsed() >= std::time::Duration::from_secs(1));
        assert!(matches!(
            response,
            Response::Error {
                code: ErrorCode::Conflict,
                ..
            }
        ));
        // One conflict event per request, however long it waited.
        let conflicts = daemon
            .recent_events(50)
            .iter()
            .filter(|e| matches!(e.kind, EventKind::LeaseConflict { .. }))
            .count();
        assert_eq!(conflicts, 1);
    }

    fn spec_in(name: &str, workdir: &Path) -> AgentSpec {
        AgentSpec {
            name: name.to_owned(),
            workdir: Some(workdir.to_path_buf()),
            ..AgentSpec::default()
        }
    }

    async fn register_spec(daemon: &Arc<Daemon>, spec: AgentSpec) -> AgentRecord {
        match daemon.handle(Request::Register { spec, pid: None }).await {
            Response::Agent { agent } => agent,
            other => panic!("unexpected {other:?}"),
        }
    }

    async fn register_in(daemon: &Arc<Daemon>, name: &str, workdir: &Path) -> AgentRecord {
        register_spec(daemon, spec_in(name, workdir)).await
    }

    async fn list(
        daemon: &Arc<Daemon>,
        project: Option<&str>,
        labels: &[(&str, &str)],
    ) -> Response {
        daemon
            .handle(Request::List {
                all: false,
                project: project.map(str::to_owned),
                labels: labels
                    .iter()
                    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                    .collect(),
            })
            .await
    }

    fn names(response: Response) -> Vec<String> {
        match response {
            Response::Agents { agents } => agents.into_iter().map(|a| a.spec.name).collect(),
            other => panic!("unexpected {other:?}"),
        }
    }

    fn drain_discovered(events: &mut broadcast::Receiver<Event>) -> Vec<PathBuf> {
        let mut roots = Vec::new();
        while let Ok(event) = events.try_recv() {
            if let EventKind::ProjectDiscovered { project } = event.kind {
                roots.push(project.root);
            }
        }
        roots
    }

    fn git(home: &Path, dir: &Path, args: &[&str]) -> bool {
        std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args([
                "-c",
                "commit.gpgsign=false",
                "-c",
                "init.defaultBranch=main",
            ])
            .args(args)
            .env("HOME", home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    fn have_git() -> bool {
        std::process::Command::new("git")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    #[tokio::test]
    async fn agents_group_by_project_and_list_filters_by_it() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let alpha = dir.path().join("alpha");
        let beta = dir.path().join("beta");
        std::fs::create_dir_all(alpha.join("src")).unwrap();
        std::fs::create_dir_all(&beta).unwrap();
        std::fs::write(alpha.join("Agentfile.toml"), "").unwrap();

        let one = register_in(&daemon, "one", &alpha).await;
        let two = register_in(&daemon, "two", &alpha.join("src")).await;
        let three = register_in(&daemon, "three", &beta).await;
        let loner = register(&daemon, "loner", None).await;
        let mut tagged = spec_in("tagged", &alpha);
        tagged.labels.insert("team".to_owned(), "x".to_owned());
        let tagged = register_spec(&daemon, tagged).await;

        let project = one.project.clone().expect("derived from workdir");
        assert_eq!(project.source, ProjectSource::Agentfile);
        assert_eq!(project.root, alpha.canonicalize().unwrap());
        assert_eq!(project.worktree, None);
        assert_eq!(
            two.project.as_ref(),
            Some(&project),
            "nested dirs share the root"
        );
        assert_eq!(tagged.project.as_ref(), Some(&project));
        assert_eq!(
            three.project.as_ref().unwrap().source,
            ProjectSource::Directory
        );
        assert_ne!(three.project.as_ref().unwrap().id(), project.id());
        assert_eq!(loner.project, None);

        // Grouped by project, agents outside any project last.
        assert_eq!(
            names(list(&daemon, None, &[]).await),
            ["one", "two", "tagged", "three", "loner"]
        );
        // A path inside the project, or any unique prefix of its id.
        let inside = alpha.join("src").to_string_lossy().into_owned();
        assert_eq!(
            names(list(&daemon, Some(&inside), &[]).await),
            ["one", "two", "tagged"]
        );
        let id = project.id();
        let prefix = &id.as_str()[..8];
        assert_eq!(
            names(list(&daemon, Some(prefix), &[]).await),
            ["one", "two", "tagged"]
        );
        assert_eq!(
            names(list(&daemon, Some(prefix), &[("team", "x")]).await),
            ["tagged"]
        );
        assert_eq!(
            names(list(&daemon, None, &[("team", "x")]).await),
            ["tagged"]
        );
        assert!(matches!(
            list(&daemon, Some("zzzz"), &[]).await,
            Response::Error {
                code: ErrorCode::NotFound,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn repositories_are_fingerprinted_once_and_announced() {
        if !have_git() {
            return;
        }
        let dir = TempDir::new().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join("sub")).unwrap();
        assert!(git(dir.path(), &repo, &["init", "-q"]));
        assert!(git(
            dir.path(),
            &repo,
            &["commit", "-q", "--allow-empty", "-m", "root"]
        ));

        let daemon = open(&dir);
        let mut events = daemon.subscribe_events();
        let a = register_in(&daemon, "a", &repo).await;
        let b = register_in(&daemon, "b", &repo.join("sub")).await;
        let project = a.project.clone().unwrap();
        assert_eq!(project.source, ProjectSource::Git);
        let fingerprint = project.fingerprint.clone().expect("root commit");
        assert_eq!(project.id().as_str(), fingerprint);
        assert_eq!(b.project.unwrap().id(), project.id());
        assert_eq!(drain_discovered(&mut events), vec![project.root.clone()]);

        // The fingerprint is persisted: a restarted daemon neither walks the
        // history again nor announces the repository twice.
        drop(daemon);
        let daemon = open(&dir);
        let mut events = daemon.subscribe_events();
        let c = register_in(&daemon, "c", &repo).await;
        assert_eq!(
            c.project.unwrap().fingerprint.as_deref(),
            Some(fingerprint.as_str())
        );
        assert!(drain_discovered(&mut events).is_empty());
    }

    async fn send(daemon: &Arc<Daemon>, from: &str, to: &str) -> Response {
        daemon
            .handle(Request::Send {
                from: from.to_owned(),
                to: to.to_owned(),
                kind: "chat".to_owned(),
                payload: json!({ "text": "hi" }),
                reply_to: None,
            })
            .await
    }

    #[tokio::test]
    async fn project_messages_reach_everyone_in_the_project_but_the_sender() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let alpha = dir.path().join("alpha");
        let beta = dir.path().join("beta");
        std::fs::create_dir_all(alpha.join("src")).unwrap();
        std::fs::create_dir_all(&beta).unwrap();
        std::fs::write(alpha.join("Agentfile.toml"), "").unwrap();
        let one = register_in(&daemon, "one", &alpha).await;
        let two = register_in(&daemon, "two", &alpha.join("src")).await;
        let three = register_in(&daemon, "three", &beta).await;
        let project = one.project.clone().unwrap().id();

        // Addressed by a path inside the project.
        let inside = alpha.join("src").to_string_lossy().into_owned();
        let Response::Sent { subscribers, .. } =
            send(&daemon, "one", &format!("project:{inside}")).await
        else {
            panic!("send failed");
        };
        assert_eq!(subscribers, 0, "nobody is live-subscribed");
        assert_eq!(inbox(&daemon, "two", false).await.len(), 1);
        assert!(
            inbox(&daemon, "one", false).await.is_empty(),
            "not to the sender"
        );
        assert!(
            inbox(&daemon, "three", false).await.is_empty(),
            "other project"
        );
        let queued = &inbox(&daemon, "two", true).await[0];
        assert_eq!(queued.to, Destination::Project(project.clone()));
        assert_eq!(queued.from, one.id.as_str());

        // Addressed by an id prefix; the user is not in any project and
        // still reaches everyone in it.
        let prefix = &project.as_str()[..8];
        assert!(matches!(
            send(&daemon, "user", &format!("project:{prefix}")).await,
            Response::Sent { .. }
        ));
        assert_eq!(inbox(&daemon, "one", false).await.len(), 1);
        assert_eq!(inbox(&daemon, "two", false).await.len(), 1);
        assert!(inbox(&daemon, "three", false).await.is_empty());
        assert!(matches!(
            send(&daemon, "user", "project:zzzz").await,
            Response::Error {
                code: ErrorCode::NotFound,
                ..
            }
        ));

        // A live subscription filters by the subscriber's own project.
        let (in_alpha, _rx) = daemon.subscribe(Some("two"), Vec::new()).unwrap();
        let (in_beta, _rx) = daemon.subscribe(Some("three"), Vec::new()).unwrap();
        let (sender, _rx) = daemon.subscribe(Some("one"), Vec::new()).unwrap();
        let envelope = Envelope::new(
            one.id.as_str(),
            Destination::Project(project.clone()),
            "chat",
            json!({}),
            None,
            Utc::now(),
        );
        assert!(in_alpha.wants(&envelope));
        assert!(!in_beta.wants(&envelope));
        assert!(!sender.wants(&envelope));
        drop(two);
        drop(three);
    }

    #[tokio::test]
    async fn path_claims_inside_a_project_become_file_keys() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let alpha = dir.path().join("alpha");
        std::fs::create_dir_all(alpha.join("src")).unwrap();
        std::fs::write(alpha.join("Agentfile.toml"), "").unwrap();
        let one = register_in(&daemon, "one", &alpha).await;
        register_in(&daemon, "two", &alpha.join("src")).await;
        register(&daemon, "outsider", None).await;
        let project = one.project.clone().unwrap().id();

        // The claimed path is under the non-canonical temp dir and the file
        // does not exist yet; the key is still project-relative.
        let lib = alpha.join("src/lib.rs");
        let Response::Lease { lease } =
            claim(&daemon, "one", &format!("path:{}", lib.display())).await
        else {
            panic!("claim failed");
        };
        assert_eq!(
            lease.resource.as_str(),
            format!("file:{project}/src/lib.rs")
        );

        // Everyone naming that file collides: a project mate, and an agent
        // with no project at all (the Agentfile root is found from the path).
        for who in ["two", "outsider"] {
            assert!(
                matches!(
                    claim(&daemon, who, &format!("path:{}", lib.display())).await,
                    Response::Error {
                        code: ErrorCode::Conflict,
                        ..
                    }
                ),
                "{who} should conflict"
            );
        }
        // So does the whole project.
        assert!(matches!(
            claim(&daemon, "two", &format!("path:{}", alpha.display())).await,
            Response::Error {
                code: ErrorCode::Conflict,
                ..
            }
        ));

        // Queries by path find the translated lease.
        let Response::Leases { leases } = daemon
            .handle(Request::Leases {
                agent: None,
                resource: Some(format!("path:{}", alpha.join("src").display())),
            })
            .await
        else {
            panic!("leases failed");
        };
        assert_eq!(leases.len(), 1);

        // Paths outside any project keep their kind.
        let Response::Lease { lease } =
            claim(&daemon, "outsider", "path:/definitely/not/here/x").await
        else {
            panic!("claim failed");
        };
        assert_eq!(lease.resource.as_str(), "path:/definitely/not/here/x");
        let Response::Lease { lease } = claim(&daemon, "outsider", "task:ISSUE-1").await else {
            panic!("claim failed");
        };
        assert_eq!(lease.resource.as_str(), "task:ISSUE-1");
    }
}
