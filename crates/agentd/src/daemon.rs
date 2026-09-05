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
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use agentdocker_core::journal::{Reader, cursor_donor, digest as render_digest, initial_cursor};
use agentdocker_core::{
    AgentId, AgentRecord, AgentSpec, AgentStatus, Attribution, Change, ChangeKind, Claimed,
    Destination, DiscoveredProcess, Envelope, ErrorCode, Event, EventKind, JournalEntry,
    JournalKind, Lease, LeaseError, LeaseId, LeaseMode, LeaseTable, MessageId, ProjectId,
    ProjectRef, ProjectSource, Registry, RegistryError, Request, ResourceKey, Response,
    SummarySource, VcsState,
    journal::{cap_paths, synthesise_summary},
    topic_matches,
};
use agentdocker_core::{DigestBudget, DigestRequest};
use chrono::{DateTime, Duration, Utc};
use nix::errno::Errno;
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use serde_json::{Value, json};
use tokio::sync::{Notify, broadcast, mpsc, oneshot, watch};
use tracing::{debug, error, info, warn};

use agentdocker_host::{procinfo, project, vcs};

use crate::store::{ChangesQuery, JournalQuery, Store};
use crate::supervisor;
mod access;
mod recovery;
mod working;
mod worktrees;

/// Messages queued per agent while it has no live subscription.
const INBOX_CAPACITY: usize = 1000;
/// Leases longer than this are clamped; a TTL is a liveness bound, not a
/// reservation.
const MAX_LEASE_TTL_SECS: u64 = 24 * 60 * 60;
/// Stored event history is trimmed to this many entries.
const EVENT_HISTORY: usize = 10_000;
/// The ledger keeps this many entries per project.
const CHANGE_HISTORY: usize = 100_000;
/// Longest a claim may wait for a conflicting lease to clear.
const MAX_WAIT_SECS: u64 = 600;
/// How long `git` may take to find a repository's root commit before the
/// project falls back to grouping by path for this daemon run.
const FINGERPRINT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);
/// How long `git log` may take to name a commit for a journal entry.
const SUBJECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);
/// Newest journal entries kept in memory per active project.
const JOURNAL_RING: usize = 256;
/// A project's ring is dropped this long after its last live agent left.
const RING_IDLE: std::time::Duration = std::time::Duration::from_secs(600);
/// How long a release waits for the watcher to flush pending observations.
const WATCHER_FLUSH_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(500);
/// How long a registration waits for the watcher to cover its checkout.
const WATCHER_ATTACH_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(500);
/// Ledger rows examined per released resource when building an entry.
const RELEASE_SCAN: usize = 10_000;
/// Entries after a cursor a digest reads from the store at most; older
/// ones beyond that are not counted.
const DIGEST_SCAN: usize = 1_000;
/// The human's cursor key: `journal --new` reads as this.
const USER_READER: &str = "user";

pub struct Daemon {
    pub home: PathBuf,
    pub socket: PathBuf,
    started: Instant,
    state: Mutex<State>,
    shutdown: Notify,
    /// Asks the watcher to flush pending observations now; set once the
    /// watcher runs. Never held across an await.
    watcher_flush: Mutex<Option<mpsc::Sender<oneshot::Sender<()>>>>,
    /// Asks the watcher to reconcile its watches now, so a checkout is
    /// covered from the moment its first agent is registered rather than
    /// from the next tick. Starts `Off`; the daemon's main says when a
    /// watcher is coming so registrations wait for it instead.
    watcher_attach: watch::Sender<WatcherLink>,
}

/// Whether registrations can ask the watcher to cover their checkout.
#[derive(Clone)]
enum WatcherLink {
    /// No watcher runs: tests, or it failed to start.
    Off,
    /// One is being spawned; wait for it rather than skip it.
    Starting,
    On(mpsc::Sender<oneshot::Sender<()>>),
}

/// One synchronous transition owns memory, persistence and publication.
/// Host I/O and waits are performed before or after this guard, never across await.
struct State {
    store: Store,
    storage_error: Option<String>,
    registry: Registry,
    leases: LeaseTable,
    inboxes: HashMap<AgentId, VecDeque<Envelope>>,
    live_subscribers: HashMap<AgentId, usize>,
    supervised: HashMap<AgentId, tokio::sync::watch::Sender<Option<bool>>>,
    projects: HashMap<PathBuf, Option<String>>,
    next_seq: u64,
    bus: broadcast::Sender<Envelope>,
    events: broadcast::Sender<Event>,
    /// Next journal seq per project, loaded from the store on first use.
    journal_seq: HashMap<ProjectId, u64>,
    /// Newest entries per active project, so digests never touch SQLite.
    journal_rings: HashMap<ProjectId, JournalRing>,
    /// The last HEAD a commit entry was written for, per checkout, so a
    /// move seen through several agents is journaled once.
    last_head: HashMap<PathBuf, String>,
    /// Readers' journal cursors, loaded from the store on first use and
    /// written through when they move.
    journal_cursors: HashMap<(String, ProjectId), u64>,
}

struct JournalRing {
    entries: VecDeque<JournalEntry>,
    touched: Instant,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Whether the watcher would cover this agent's checkout at all.
fn watchable(record: &AgentRecord) -> bool {
    record
        .project
        .as_ref()
        .is_some_and(|p| matches!(p.source, ProjectSource::Git | ProjectSource::Agentfile))
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
fn signal_pid(pid: u32) -> Option<Pid> {
    let raw = i32::try_from(pid).ok()?;
    (raw > 0).then(|| Pid::from_raw(raw))
}

fn process_exists(pid: u32) -> bool {
    let Some(pid) = signal_pid(pid) else {
        return false;
    };
    match kill(pid, None) {
        Ok(()) | Err(Errno::EPERM) => true,
        Err(_) => false,
    }
}

/// Does the pid still belong to the process that registered it? Compared by
/// exact start time. Liveness is lenient when either side
/// is unknown: a pid that exists but can't be inspected is assumed alive.
fn same_process(pid: u32, recorded: Option<DateTime<Utc>>) -> bool {
    match (recorded, procinfo::start_time(pid)) {
        (Some(recorded), Some(current)) => current == recorded,
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

/// Resolve a logical file name only within its recorded holder checkout.
fn physical_file(key: &ResourceKey, project: Option<&ProjectRef>) -> Option<PathBuf> {
    let project = project?;
    let (id, relative) = key.value().split_once('/').unwrap_or((key.value(), ""));
    if id != project.id().as_str() {
        return None;
    }
    let relative = Path::new(relative);
    if relative.components().any(|c| {
        !matches!(
            c,
            std::path::Component::Normal(_) | std::path::Component::CurDir
        )
    }) {
        return None;
    }
    Some(project.dir().join(relative))
}

/// A directory the watcher keeps an eye on: the main root or a linked
/// worktree of a project some live agent works in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checkout {
    pub dir: PathBuf,
    pub project: ProjectId,
    /// `Some(dir)` when this checkout is a linked worktree.
    pub worktree: Option<PathBuf>,
}

/// One file change the watcher saw, before attribution.
#[derive(Clone, Debug)]
pub struct Observed {
    pub checkout: Checkout,
    /// Relative to the checkout.
    pub path: PathBuf,
    pub kind: ChangeKind,
}

impl Daemon {
    pub fn emit(&self, kind: EventKind) {
        lock(&self.state).emit(kind);
    }
    pub fn resolve(&self, reference: &str) -> Result<AgentId, Box<Response>> {
        lock(&self.state).resolve(reference)
    }
    pub fn is_live(&self, id: &AgentId) -> bool {
        lock(&self.state).is_live(id)
    }
    pub fn mark_exited(&self, id: &AgentId, status: AgentStatus) -> Option<AgentRecord> {
        lock(&self.state).mark_exited(id, status)
    }
    pub fn check_liveness(&self) {
        let candidates: Vec<_> = {
            let state = lock(&self.state);
            state
                .registry
                .live()
                .filter(|a| !state.supervised.contains_key(&a.id))
                .filter(|a| !(a.managed && a.status == AgentStatus::Created))
                .cloned()
                .collect()
        };
        for candidate in candidates {
            let alive = match candidate.pid {
                Some(pid) => process_exists(pid) && same_process(pid, candidate.process_started_at),
                None => !candidate.managed,
            };
            if alive {
                continue;
            }
            let group_alive = candidate
                .process_group
                .is_some_and(supervisor::group_exists);
            let mut state = lock(&self.state);
            if !state.supervised.contains_key(&candidate.id)
                && state.registry.get(&candidate.id).is_some_and(|a| {
                    a.status.is_live()
                        && a.pid == candidate.pid
                        && a.process_started_at == candidate.process_started_at
                })
            {
                if group_alive {
                    if candidate.status != AgentStatus::Stopping {
                        let agent = state
                            .registry
                            .set_status(&candidate.id, AgentStatus::Stopping, Utc::now())
                            .unwrap();
                        state.persist("agent", |store| store.upsert_agent(&agent));
                        state.emit(EventKind::AgentStopping {
                            agent: candidate.id,
                            force: false,
                        });
                    }
                } else {
                    state.mark_exited(&candidate.id, AgentStatus::Exited { code: None });
                }
            }
        }
    }
    pub async fn stop_all(&self) {
        let managed: Vec<_> = lock(&self.state)
            .registry
            .live()
            .filter(|a| a.managed)
            .map(|a| a.id.clone())
            .collect();
        for id in managed {
            if let response @ Response::Error { .. } = self.stop(id.as_str(), false) {
                warn!(agent = %id, ?response, "managed agent did not stop during shutdown");
            }
        }
        // Keep supervision alive during shutdown so owned children are reaped
        // and their groups stop before durable protection is released.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(8);
        while !lock(&self.state).supervised.is_empty() {
            if tokio::time::Instant::now() >= deadline {
                warn!("managed groups did not finish shutdown; durable protection retained");
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    }

    fn stop(&self, reference: &str, force: bool) -> Response {
        let record = {
            let mut state = lock(&self.state);
            let id = match state.registry.resolve(reference) {
                Ok(id) => id,
                Err(e) => return registry_error(e),
            };
            let record = state.registry.get(&id).unwrap().clone();
            if !record.status.is_live() {
                return Response::error(ErrorCode::Invalid, "agent has already finished");
            }
            if let Some(control) = state.supervised.get(&id) {
                // The supervisor still owns the Child. Route signals through
                // that handle even when process start-time inspection fails.
                control.send_modify(|pending| *pending = Some(force || pending.unwrap_or(false)));
                let agent = state
                    .registry
                    .set_status(&id, AgentStatus::Stopping, Utc::now())
                    .unwrap();
                state.persist("agent", |store| store.upsert_agent(&agent));
                state.emit(EventKind::AgentStopping { agent: id, force });
                return Response::Agent { agent };
            }
            if record.pid.is_none() {
                if record.managed {
                    return Response::error(ErrorCode::Invalid, "agent is still starting");
                }
                return match state.mark_exited(&id, AgentStatus::Exited { code: None }) {
                    Some(agent) => Response::Agent { agent },
                    None => Response::error(ErrorCode::NotFound, "agent vanished"),
                };
            }
            record
        };
        let pid = record.pid.unwrap();
        let Some(target) = signal_pid(pid) else {
            return Response::error(ErrorCode::Invalid, "invalid signal target");
        };
        // Host inspection and signaling never hold the global coordination guard.
        let alive = process_exists(pid);
        let current_started_at = procinfo::start_time(pid);
        let group_alive = record.process_group.is_some_and(supervisor::group_exists);
        if !alive && group_alive {
            return Response::error(
                ErrorCode::Forbidden,
                "managed descendants remain but the leader identity is unavailable; leases retained until group exit",
            );
        }
        if alive {
            let Some(started) = record.process_started_at else {
                return Response::error(
                    ErrorCode::Forbidden,
                    "cannot verify process identity before signaling",
                );
            };
            if current_started_at != Some(started) {
                return Response::error(
                    ErrorCode::Forbidden,
                    "process identity changed or is unavailable",
                );
            }
            let target = if record.managed && record.process_group == Some(pid) {
                Pid::from_raw(-target.as_raw())
            } else {
                target
            };
            if let Err(err) = kill(
                target,
                if force {
                    Signal::SIGKILL
                } else {
                    Signal::SIGTERM
                },
            ) {
                if err != Errno::ESRCH {
                    return Response::error(
                        ErrorCode::Forbidden,
                        format!("cannot signal pid {pid}: {err}"),
                    );
                }
            }
        }
        let mut state = lock(&self.state);
        let Some(current) = state.registry.get(&record.id) else {
            return Response::error(ErrorCode::NotFound, "agent vanished");
        };
        if current.pid != record.pid || current.process_started_at != record.process_started_at {
            return Response::error(ErrorCode::Conflict, "agent identity changed during stop");
        }
        if !current.status.is_live() {
            return Response::Agent {
                agent: current.clone(),
            };
        }
        if !alive {
            return Response::Agent {
                agent: state
                    .mark_exited(&record.id, AgentStatus::Exited { code: None })
                    .unwrap(),
            };
        }
        let agent = state
            .registry
            .set_status(&record.id, AgentStatus::Stopping, Utc::now())
            .unwrap();
        state.persist("agent", |store| store.upsert_agent(&agent));
        state.emit(EventKind::AgentStopping {
            agent: record.id,
            force,
        });
        Response::Agent { agent }
    }
    pub fn expire_leases(&self) {
        lock(&self.state).expire_leases();
    }
    pub fn prune_events(&self) {
        lock(&self.state).prune_events();
    }
    fn apply_vcs(&self, id: &AgentId, vcs: VcsState) {
        lock(&self.state).apply_vcs(id, vcs);
    }
    fn unsubscribe(&self, id: &AgentId) {
        lock(&self.state).unsubscribe(id);
    }

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
        let mut next_seq = store.max_event_seq()? + 1;
        for mut lease in store.load_leases()? {
            if lease.is_expired(now)
                || !registry
                    .get(&lease.holder)
                    .is_some_and(|a| a.status.is_live())
            {
                let kind = if lease.is_expired(now) {
                    EventKind::LeaseExpired {
                        lease: lease.clone(),
                    }
                } else {
                    EventKind::LeaseReleased {
                        lease: lease.clone(),
                    }
                };
                let mut event = Event::new(kind, now);
                event.seq = next_seq;
                store.delete_lease_with_event(&lease.id, &event)?;
                next_seq += 1;
                continue;
            }
            if lease.resource.kind() == "file" {
                let path = physical_file(
                    &lease.resource,
                    registry.get(&lease.holder).and_then(|a| a.project.as_ref()),
                )
                .ok_or_else(|| {
                    anyhow::anyhow!("cannot migrate lease {} without its checkout", lease.id)
                })?;
                lease.resource =
                    ResourceKey::new(format!("path:{}", project::try_canonical(&path)?.display()));
                store.upsert_lease(&lease)?;
            }
            if leases.holders_of(&lease.resource).iter().any(|held| {
                held.holder != lease.holder
                    && (held.mode == LeaseMode::Exclusive || lease.mode == LeaseMode::Exclusive)
            }) {
                anyhow::bail!(
                    "stored lease {} overlaps another live holder after physical migration; stop the holders and retry",
                    lease.id
                );
            }
            leases.restore(lease);
        }
        let inboxes = store.load_inboxes()?;
        let projects: HashMap<PathBuf, Option<String>> = store
            .load_projects()?
            .into_iter()
            .map(|(root, fingerprint)| (root, Some(fingerprint)))
            .collect();
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
            state: Mutex::new(State {
                store,
                storage_error: None,
                registry,
                leases,
                inboxes,
                live_subscribers: HashMap::new(),
                supervised: HashMap::new(),
                projects,
                next_seq,
                bus,
                events,
                journal_seq: HashMap::new(),
                journal_rings: HashMap::new(),
                last_head: HashMap::new(),
                journal_cursors: HashMap::new(),
            }),
            shutdown: Notify::new(),
            watcher_flush: Mutex::new(None),
            watcher_attach: watch::channel(WatcherLink::Off).0,
        })
    }

    pub fn log_path(&self, id: &AgentId) -> PathBuf {
        self.home.join("logs").join(format!("{id}.log"))
    }

    pub fn subscribe_events(&self) -> broadcast::Receiver<Event> {
        lock(&self.state).events.subscribe()
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
        lock(&self.state)
            .store
            .recent_events(limit)
            .unwrap_or_else(|err| {
                error!(%err, "failed to load event history");
                Vec::new()
            })
    }

    /// Handle every non-streaming request.
    pub async fn handle(self: &Arc<Self>, request: Request) -> Response {
        // A failed write makes the in-memory projection unsafe to serve. Keep
        // the daemon unavailable until restart reloads durable state; never
        // acknowledge a mutation or grant new protection from that projection.
        if matches!(request, Request::Shutdown) {
            self.shutdown.notify_one();
            return Response::Ok;
        }
        if let Some(error) = lock(&self.state).storage_failure() {
            return error;
        }
        let response = self.handle_healthy(request).await;
        lock(&self.state).storage_failure().unwrap_or(response)
    }

    async fn handle_healthy(self: &Arc<Self>, request: Request) -> Response {
        match request {
            Request::Observe { agent, paths } => self.observe(&agent, paths).await,
            Request::Stale { agent, paths } => self.stale(&agent, paths).await,
            Request::Reads { agent } => self.reads(&agent),
            Request::Checkpoint {
                agent,
                key,
                task,
                assumptions,
                next_steps,
                release_leases,
            } => {
                self.checkpoint(&agent, key, task, assumptions, next_steps, release_leases)
                    .await
            }
            Request::Resume {
                agent,
                checkpoint,
                acknowledge,
            } => self.resume(&agent, &checkpoint, acknowledge).await,
            Request::Checkpoints { agent } => self.checkpoints(agent.as_deref()),
            Request::Validate {
                agent,
                command,
                timeout_secs,
            } => self.validate(&agent, command, timeout_secs).await,
            Request::Validations { agent } => self.validations(&agent),
            Request::WorktreeCreate {
                agent,
                path,
                branch,
            } => self.worktree_create(&agent, path, branch).await,
            Request::WorktreeDiff { agent } => self.worktree_diff(&agent).await,
            Request::Integrate {
                agent,
                source,
                validation,
                apply,
            } => self.integrate(&agent, source, validation, apply).await,
            Request::Authenticate { .. } => Response::error(
                ErrorCode::Forbidden,
                "authenticate only on the restricted endpoint",
            ),
            Request::GrantAccess {
                agent,
                container_root,
                ttl_secs,
            } => self.grant_access(&agent, container_root, ttl_secs),
            Request::RevokeAccess { grant } => self.revoke_access(&grant),
            Request::Ping => Response::Pong {
                version: env!("CARGO_PKG_VERSION").to_owned(),
                uptime_secs: self.started.elapsed().as_secs(),
            },
            Request::Run { spec } => self.run(spec).await,
            Request::Register { spec, pid } => self.register(spec, pid).await,
            Request::Deregister { agent } => lock(&self.state).deregister(&agent),
            Request::Discover => self.discover().await,
            Request::Adopt { pid, name, runtime } => self.adopt(pid, name, runtime).await,
            Request::Stop { agent, force } => self.stop(&agent, force),
            Request::Remove { agent } => lock(&self.state).remove(&agent),
            Request::List {
                all,
                project,
                labels,
            } => self.list(all, project, labels).await,
            Request::Inspect { agent } => lock(&self.state).inspect(&agent),
            Request::Heartbeat { agent } => match self.resolve(&agent) {
                Ok(id) => {
                    lock(&self.state).touch(&id);
                    Response::Ok
                }
                Err(response) => *response,
            },
            Request::Report { agent, vcs } => lock(&self.state).report(&agent, vcs),
            Request::Changes {
                project,
                since_seq,
                path,
                agent,
                limit,
            } => self.changes(&project, since_seq, path, agent, limit).await,
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
            Request::Inbox { agent, drain } => lock(&self.state).inbox(&agent, drain),
            Request::AckInbox { agent, messages } => lock(&self.state).ack_inbox(&agent, &messages),
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
            } => lock(&self.state).renew(&agent, &lease, ttl_secs),
            Request::Release {
                agent,
                lease,
                summary,
                summary_source,
            } => {
                self.flush_release_watcher(&agent, Some(&lease)).await;
                lock(&self.state).release(&agent, &lease, summary, summary_source)
            }
            Request::ReleaseAll {
                agent,
                summary,
                summary_source,
            } => {
                self.flush_release_watcher(&agent, None).await;
                lock(&self.state).release_all(&agent, summary, summary_source)
            }
            Request::JournalAdd { agent, summary } => {
                lock(&self.state).journal_add(&agent, summary)
            }
            Request::Journal {
                project,
                since_seq,
                until_seq,
                agent,
                branch,
                kind,
                path,
                grep,
                limit,
                digest,
            } => {
                self.journal(
                    &project, since_seq, until_seq, agent, branch, kind, path, grep, limit, digest,
                )
                .await
            }
            Request::JournalPrune {
                project,
                before_seq,
            } => match self.resolve_project(&project).await {
                Ok(id) => lock(&self.state).journal_prune(&id, before_seq),
                Err(response) => *response,
            },
            Request::Leases { agent, resource } => self.leases(agent.as_deref(), resource).await,
            Request::Subscribe { .. } | Request::Events { .. } | Request::Logs { .. } => {
                Response::error(ErrorCode::Internal, "streaming request routed as unary")
            }
        }
    }

    async fn run(self: &Arc<Self>, spec: AgentSpec) -> Response {
        if spec.command.first().is_none_or(String::is_empty) {
            return Response::error(ErrorCode::Invalid, "run needs a nonempty command");
        }
        let project = self.project_for(spec.workdir.clone(), true).await;
        let vcs = Self::vcs_for(spec.workdir.clone()).await;
        let mut record = AgentRecord::new(spec, true, Utc::now());
        record.project = project;
        record.vcs = vcs;
        let record = match lock(&self.state).insert_record(record) {
            Response::Agent { agent } => agent,
            other => return other,
        };
        // Watched before the process exists, so its first edit is seen.
        if watchable(&record) {
            self.ensure_watched().await;
        }
        match supervisor::spawn(self, &record).await {
            Ok(spawned) => {
                let pid = spawned.pid;
                let process_started_at = procinfo::start_time(pid);
                let updated = {
                    let mut state = lock(&self.state);
                    state
                        .supervised
                        .insert(record.id.clone(), spawned.control.clone());
                    if let Some(rec) = state.registry.get_mut(&record.id) {
                        rec.pid = Some(pid);
                        rec.process_started_at = process_started_at;
                        rec.process_group = Some(pid);
                    }
                    let updated =
                        state
                            .registry
                            .set_status(&record.id, AgentStatus::Running, Utc::now());
                    if let Some(rec) = &updated {
                        state.persist("agent", |store| store.upsert_agent(rec));
                    }
                    state.emit(EventKind::AgentStarted {
                        agent: record.id.clone(),
                        pid: Some(pid),
                    });
                    updated
                };
                supervisor::supervise(self.clone(), record.id, spawned);
                match updated {
                    Some(agent) => Response::Agent { agent },
                    None => Response::error(ErrorCode::NotFound, "agent vanished"),
                }
            }
            Err(err) => {
                self.mark_exited(
                    &record.id,
                    AgentStatus::Failed {
                        reason: format!("{err:#}"),
                    },
                );
                Response::error(ErrorCode::Internal, format!("{err:#}"))
            }
        }
    }

    async fn register(&self, spec: AgentSpec, pid: Option<u32>) -> Response {
        if pid.is_some_and(|pid| signal_pid(pid).is_none()) {
            return Response::error(
                ErrorCode::Invalid,
                "pid must be a positive process id within i32 range",
            );
        }
        let project = self.project_for(spec.workdir.clone(), true).await;
        let vcs = Self::vcs_for(spec.workdir.clone()).await;
        let mut record = AgentRecord::new(spec, false, Utc::now());
        record.project = project;
        record.vcs = vcs;
        record.pid = pid;
        record.process_started_at = pid.and_then(procinfo::start_time);
        record.status = AgentStatus::Running;
        record.started_at = Some(Utc::now());
        let response = lock(&self.state).insert_record(record);
        // The reply is what a session waits for before its first edit, so
        // the checkout is watched by the time it goes out.
        if let Response::Agent { agent } = &response {
            if watchable(agent) {
                self.ensure_watched().await;
            }
        }
        response
    }

    /// Agent processes of known runtimes that no live agent claims by pid.
    /// Projects come without fingerprints: this runs on every `ps`, and a
    /// process nobody adopted should not warm the cache or announce a
    /// repository.
    async fn discover(&self) -> Response {
        let registered: HashSet<u32> = lock(&self.state)
            .registry
            .live()
            .filter_map(|a| a.pid)
            .collect();
        let mine = std::process::id();
        let processes = tokio::task::spawn_blocking(move || {
            let mut found: Vec<DiscoveredProcess> = procinfo::processes()
                .into_iter()
                .filter(|p| p.pid != mine && !registered.contains(&p.pid))
                .filter_map(|p| {
                    let runtime = procinfo::runtime_of(&p.argv)?;
                    let cwd = procinfo::cwd(p.pid);
                    Some(DiscoveredProcess {
                        pid: p.pid,
                        ppid: p.ppid,
                        runtime: runtime.to_owned(),
                        command: p.argv.join(" "),
                        project: cwd.as_deref().map(project::discover),
                        cwd,
                        started_at: procinfo::start_time(p.pid),
                    })
                })
                .collect();
            found.sort_by(|a, b| {
                let key = |p: &DiscoveredProcess| {
                    (
                        p.project.is_none(),
                        p.project.as_ref().map(ProjectRef::name),
                        p.pid,
                    )
                };
                key(a).cmp(&key(b))
            });
            found
        })
        .await
        .unwrap_or_default();
        Response::Processes { processes }
    }

    /// Register a running process by pid: runtime from the known table
    /// unless given, working directory from the process, so it lands in
    /// its project. Adopted agents run no hooks, so they hold no leases
    /// and report nothing, but they are visible, messageable, and counted.
    async fn adopt(&self, pid: u32, name: Option<String>, runtime: Option<String>) -> Response {
        let already = lock(&self.state)
            .registry
            .live()
            .find(|a| a.pid == Some(pid))
            .map(|a| a.spec.name.clone());
        if let Some(name) = already {
            return Response::error(
                ErrorCode::Invalid,
                format!("pid {pid} is already agent `{name}`"),
            );
        }
        let found = tokio::task::spawn_blocking(move || {
            procinfo::inspect(pid).map(|p| (p, procinfo::cwd(pid)))
        })
        .await
        .ok()
        .flatten();
        let Some((process, cwd)) = found else {
            return Response::error(ErrorCode::NotFound, format!("no process with pid {pid}"));
        };
        let runtime = runtime
            .or_else(|| procinfo::runtime_of(&process.argv).map(str::to_owned))
            .unwrap_or_else(|| "custom".to_owned());
        let spec = AgentSpec {
            name: name.unwrap_or_else(|| format!("{runtime}-{pid}")),
            runtime,
            workdir: cwd,
            labels: BTreeMap::from([("adopted".to_owned(), "true".to_owned())]),
            ..AgentSpec::default()
        };
        self.register(spec, Some(pid)).await
    }

    async fn vcs_for(workdir: Option<PathBuf>) -> Option<VcsState> {
        let dir = workdir?;
        tokio::task::spawn_blocking(move || vcs::state(&dir))
            .await
            .ok()
            .flatten()
    }

    /// Read every live agent's checkout and record what moved. Called on a
    /// timer, so branch and head stay right for agents that never report —
    /// adopted ones, and anything started with `run`.
    pub async fn refresh_vcs(&self, dir: Option<&Path>) {
        let targets: Vec<(AgentId, PathBuf, Option<VcsState>)> = lock(&self.state)
            .registry
            .live()
            .filter(|a| dir.is_none_or(|dir| a.project.as_ref().is_some_and(|p| p.dir() == dir)))
            .filter_map(|a| {
                a.spec
                    .workdir
                    .clone()
                    .map(|workdir| (a.id.clone(), workdir, a.vcs.clone()))
            })
            .collect();
        if targets.is_empty() {
            return;
        }
        // A moved HEAD is named while we are off the lock anyway.
        let observed = tokio::task::spawn_blocking(move || {
            targets
                .into_iter()
                .filter_map(|(id, dir, old)| {
                    let state = vcs::state(&dir)?;
                    let moved = state.head.is_some()
                        && old.as_ref().and_then(|o| o.head.clone()) != state.head;
                    let subject = if moved {
                        state
                            .head
                            .as_deref()
                            .and_then(|sha| vcs::subject(&dir, sha, SUBJECT_TIMEOUT))
                    } else {
                        None
                    };
                    Some((id, state, old, moved, subject))
                })
                .collect::<Vec<_>>()
        })
        .await
        .unwrap_or_default();
        for (id, state, old, moved, subject) in observed {
            self.apply_vcs(&id, state.clone());
            if moved {
                lock(&self.state).note_head_move(&id, old.as_ref(), &state, subject);
            }
        }
    }

    /// The checkouts the watcher should cover: every distinct directory a
    /// live agent works in whose project is a repository or an Agentfile
    /// root. Plain directories are left alone — a recursive watch on a
    /// home directory is what inotify cannot afford.
    pub fn watch_targets(&self) -> Vec<Checkout> {
        let mut seen: HashSet<PathBuf> = HashSet::new();
        lock(&self.state)
            .registry
            .live()
            .filter_map(|a| a.project.as_ref())
            .filter(|p| matches!(p.source, ProjectSource::Git | ProjectSource::Agentfile))
            .filter(|p| seen.insert(p.dir().to_path_buf()))
            .map(|p| Checkout {
                dir: p.dir().to_path_buf(),
                project: p.id(),
                worktree: p.worktree.clone(),
            })
            .collect()
    }

    /// Let the watcher hand us its flush channel.
    pub fn set_watcher_flush(&self, sender: mpsc::Sender<oneshot::Sender<()>>) {
        *lock(&self.watcher_flush) = Some(sender);
    }

    /// A watcher is about to be spawned: registrations arriving before it
    /// has handed over its channel wait for it, within the same bound,
    /// instead of taking startup for no watcher at all.
    pub fn expect_watcher(&self) {
        self.watcher_attach.send_replace(WatcherLink::Starting);
        self.emit(EventKind::WatcherStarting);
    }

    /// Let the watcher hand us its reconcile channel.
    pub fn set_watcher_attach(&self, sender: mpsc::Sender<oneshot::Sender<()>>) {
        self.watcher_attach.send_replace(WatcherLink::On(sender));
        self.emit(EventKind::WatcherStarted);
    }

    /// The watcher could not start after all; stop waiting for it.
    pub fn watcher_off(&self, reason: String) {
        self.watcher_attach.send_replace(WatcherLink::Off);
        self.emit(EventKind::WatcherUnavailable { reason });
    }

    /// Have the watcher cover every checkout a live agent works in, now:
    /// registration awaits this so the agent's first edit is never made in
    /// the gap before the next reconcile tick. Bounded, and a no-op when no
    /// watcher runs.
    async fn ensure_watched(&self) {
        let _ = tokio::time::timeout(WATCHER_ATTACH_TIMEOUT, async {
            let mut link = self.watcher_attach.subscribe();
            let sender = match link
                .wait_for(|link| !matches!(link, WatcherLink::Starting))
                .await
            {
                Ok(link) => match &*link {
                    WatcherLink::On(sender) => sender.clone(),
                    _ => return,
                },
                Err(_) => return,
            };
            let (ack, done) = oneshot::channel();
            if sender.send(ack).await.is_ok() {
                let _ = done.await;
            }
        })
        .await;
    }

    /// Only path leases need pending filesystem observations for their summary.
    /// Select under the state lock, then await the watcher with no guard held.
    pub(super) async fn flush_release_watcher(&self, reference: &str, lease: Option<&LeaseId>) {
        let needs_flush = {
            let state = lock(&self.state);
            state
                .registry
                .resolve(reference)
                .ok()
                .is_some_and(|holder| {
                    state.leases.all().into_iter().any(|held| {
                        held.holder == holder
                            && held.resource.kind() == "path"
                            && lease.is_none_or(|id| held.id == *id)
                    })
                })
        };
        if !needs_flush {
            return;
        }
        let sender = lock(&self.watcher_flush).clone();
        let Some(sender) = sender else {
            return;
        };
        // Bound both queue admission and acknowledgement: a full channel must
        // never leave a release waiting indefinitely for the watcher.
        let _ = tokio::time::timeout(WATCHER_FLUSH_TIMEOUT, async {
            let (ack, done) = oneshot::channel();
            if sender.send(ack).await.is_ok() {
                let _ = done.await;
            }
        })
        .await;
    }

    /// An absolute path made relative to the checkout containing it;
    /// anything else passes through.
    async fn relative_path(&self, raw: String) -> String {
        if !Path::new(&raw).is_absolute() {
            return raw;
        }
        let given = PathBuf::from(&raw);
        let (absolute, discovery_dir) = tokio::task::spawn_blocking(move || {
            let absolute = project::canonical(&given);
            let discovery_dir = if absolute.is_dir() {
                Some(absolute.clone())
            } else {
                absolute.parent().map(Path::to_path_buf)
            };
            (absolute, discovery_dir)
        })
        .await
        .unwrap_or_else(|_| (PathBuf::from(&raw), None));
        match self.project_for(discovery_dir, false).await {
            Some(found) => match absolute.strip_prefix(found.dir()) {
                Ok(relative) => relative.to_string_lossy().into_owned(),
                Err(_) => raw,
            },
            None => raw,
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn journal(
        &self,
        project: &str,
        since_seq: Option<u64>,
        until_seq: Option<u64>,
        agent: Option<String>,
        branch: Option<String>,
        kind: Option<String>,
        path: Option<String>,
        grep: Option<String>,
        limit: usize,
        digest: Option<DigestRequest>,
    ) -> Response {
        if let Some(request) = digest {
            return self.journal_digest(project, since_seq, request).await;
        }
        let project = match self.resolve_project(project).await {
            Ok(id) => id,
            Err(response) => return *response,
        };
        let agent = match agent.map(|reference| self.resolve(&reference)).transpose() {
            Ok(agent) => agent,
            Err(response) => return *response,
        };
        let kind = match kind.as_deref().map(JournalKind::parse) {
            None => None,
            Some(Some(kind)) => Some(kind),
            Some(None) => {
                return Response::error(
                    ErrorCode::Invalid,
                    "kind must be release, note, commit, join, leave, or handoff",
                );
            }
        };
        let path = match path {
            Some(raw) => Some(self.relative_path(raw).await),
            None => None,
        };
        let query = JournalQuery {
            project,
            since_seq,
            until_seq,
            agent,
            branch,
            kind,
            path,
            grep,
            limit: limit.clamp(1, 10_000),
        };
        lock(&self.state).journal_query(query)
    }

    /// The digest form of `journal`: who is reading decides the cursor and
    /// the branch filter. An empty project means the reader's own.
    async fn journal_digest(
        &self,
        project: &str,
        since_seq: Option<u64>,
        request: DigestRequest,
    ) -> Response {
        let (key, name, branch, own_project) = if request.reader == USER_READER {
            (USER_READER.to_owned(), USER_READER.to_owned(), None, None)
        } else {
            let id = match self.resolve(&request.reader) {
                Ok(id) => id,
                Err(response) => return *response,
            };
            let Some(record) = lock(&self.state).registry.get(&id).cloned() else {
                return Response::error(ErrorCode::NotFound, "agent vanished");
            };
            (
                id.as_str().to_owned(),
                record.spec.name.clone(),
                record.vcs.as_ref().and_then(|v| v.branch.clone()),
                record.project.as_ref().map(ProjectRef::id),
            )
        };
        let project = if project.is_empty() {
            match own_project {
                Some(id) => id,
                None => {
                    return Response::error(
                        ErrorCode::Invalid,
                        "the reader is in no project; name one",
                    );
                }
            }
        } else {
            match self.resolve_project(project).await {
                Ok(id) => id,
                Err(response) => return *response,
            }
        };
        lock(&self.state).journal_digest(
            project,
            &key,
            &name,
            branch.as_deref(),
            since_seq,
            &request,
        )
    }

    /// What the watcher saw in one debounced batch: file changes become
    /// ledger entries (persisted in `changes`, announced live as
    /// `file_changed`), and checkouts whose HEAD moved get their agents'
    /// branch and head re-read.
    pub async fn record_fs_changes(&self, observed: Vec<Observed>, vcs_touched: Vec<Checkout>) {
        let dirs: Vec<PathBuf> = {
            let mut dirs: Vec<PathBuf> = observed.iter().map(|o| o.checkout.dir.clone()).collect();
            dirs.sort();
            dirs.dedup();
            dirs
        };
        let heads: HashMap<PathBuf, Option<String>> = tokio::task::spawn_blocking(move || {
            dirs.into_iter()
                .map(|dir| {
                    let head = vcs::state(&dir).and_then(|s| s.head);
                    (dir, head)
                })
                .collect()
        })
        .await
        .unwrap_or_default();
        let observed = tokio::task::spawn_blocking(move || {
            observed
                .into_iter()
                .map(|entry| {
                    let physical =
                        project::try_canonical(&entry.checkout.dir.join(&entry.path)).ok();
                    (entry, physical)
                })
                .collect::<Vec<_>>()
        })
        .await
        .unwrap_or_default();
        let now = Utc::now();
        for (
            Observed {
                checkout,
                path,
                kind,
            },
            physical,
        ) in observed
        {
            let mut state = lock(&self.state);
            let by = physical
                .as_deref()
                .map_or(Attribution::External, |path| state.attribute(path));
            let mut change = Change {
                seq: 0,
                project: checkout.project.clone(),
                checkout: Some(checkout.dir.clone()),
                worktree: checkout.worktree.clone(),
                path,
                kind,
                at: now,
                by,
                head: heads.get(&checkout.dir).cloned().flatten(),
            };
            let Some(seq) = state.store_op("change", |store| store.append_change(&change)) else {
                continue;
            };
            change.seq = seq;
            state.warn_readers(&change, physical.as_deref());
            debug!(project = %change.project.short(), path = %change.path.display(), %kind, "file changed");
            let _ = state
                .events
                .send(Event::new(EventKind::FileChanged { change }, now));
        }
        for checkout in vcs_touched {
            self.refresh_vcs(Some(&checkout.dir)).await;
        }
    }

    async fn changes(
        &self,
        project: &str,
        since_seq: Option<u64>,
        path: Option<String>,
        agent: Option<String>,
        limit: usize,
    ) -> Response {
        let project = match self.resolve_project(project).await {
            Ok(id) => id,
            Err(response) => return *response,
        };
        let agent = match agent.map(|reference| self.resolve(&reference)).transpose() {
            Ok(agent) => agent,
            Err(response) => return *response,
        };
        // An absolute path is made relative to the checkout containing it.
        let path = match path {
            Some(raw) => Some(self.relative_path(raw).await),
            None => None,
        };
        let query = ChangesQuery {
            project,
            since_seq,
            path,
            agent,
            limit: limit.clamp(1, 10_000),
            after: None,
        };
        match lock(&self.state).store.changes(&query) {
            Ok(changes) => Response::Changes { changes },
            Err(err) => Response::error(
                ErrorCode::StorageUnavailable,
                format!("ledger query failed: {err}"),
            ),
        }
    }

    /// Trim the ledger. Called occasionally from the reaper.
    pub fn evict_journal_rings(&self) {
        lock(&self.state).evict_journal_rings();
    }

    pub fn prune_changes(&self) {
        match lock(&self.state).store.prune_changes(CHANGE_HISTORY) {
            Ok(0) => {}
            Ok(removed) => info!(removed, "pruned the ledger"),
            Err(err) => error!(%err, "failed to prune the ledger"),
        }
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
        if let Some(cached) = lock(&self.state).projects.get(&project.root).cloned() {
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
        let mut state = lock(&self.state);
        let fresh = match state.projects.entry(project.root.clone()) {
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
                    state.persist("project", |store| {
                        store.upsert_project(&project.root, fingerprint)
                    });
                }
                None => warn!(
                    root = %project.root.display(),
                    "repository has no usable fingerprint (git missing, no commits, or timed out); grouping by path"
                ),
            }
            info!(project = %project.id().short(), root = %project.root.display(), "project discovered");
            state.emit(EventKind::ProjectDiscovered {
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
        lock(&self.state)
            .registry
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
            agents: lock(&self.state)
                .registry
                .matching(all, project.as_ref(), &labels),
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
        let from = match lock(&self.state).registry.resolve(&from) {
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
        lock(&self.state).send(from, to, kind, payload, reply_to)
    }

    /// Open a live subscription. Returns the filter plus the raw receiver so
    /// the caller can `select!` on the receiver without borrowing the filter.
    pub fn subscribe(
        self: &Arc<Self>,
        agent: Option<&str>,
        topics: Vec<String>,
    ) -> Result<(Subscription, broadcast::Receiver<Envelope>), Box<Response>> {
        let mut state = lock(&self.state);
        let agent = agent
            .map(|reference| state.resolve(reference))
            .transpose()?;
        let project = agent
            .as_ref()
            .and_then(|id| state.registry.get(id))
            .and_then(|a| a.project.as_ref().map(ProjectRef::id));
        let receiver = state.bus.subscribe();
        let backlog = match &agent {
            Some(id) => {
                *state.live_subscribers.entry(id.clone()).or_default() += 1;
                let backlog = state.inboxes.remove(id).map(Vec::from).unwrap_or_default();
                if !backlog.is_empty() {
                    state.persist("inbox", |store| store.clear_inbox(id));
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

        let resource = match self
            .localise(ResourceKey::new(resource), Some(&holder), true)
            .await
        {
            Ok(resource) => resource,
            Err(response) => return *response,
        };
        let deadline = tokio::time::Instant::now()
            + std::time::Duration::from_secs(wait_secs.min(MAX_WAIT_SECS));
        // Subscribe before the first attempt so a release that lands between
        // a failed attempt and the wait is not missed.
        let mut events = self.subscribe_events();
        let mut reported_conflict = false;
        loop {
            let (message, held_by) = {
                let mut state = lock(&self.state);
                if !state
                    .registry
                    .get(&holder)
                    .is_some_and(|a| a.status == AgentStatus::Running)
                {
                    return Response::error(ErrorCode::Invalid, "agent is not running");
                }
                if let Some(error) = state.storage_failure() {
                    return error;
                }
                state.touch(&holder);
                let now = Utc::now();
                state.expire_leases_at(now);
                let result = state.leases.claim(
                    resource.clone(),
                    holder.clone(),
                    mode,
                    ttl(ttl_secs),
                    note.clone(),
                    now,
                );
                let (message, held_by) = match result {
                    Ok(Claimed::New(mut lease)) => {
                        lease.change_seq = state
                            .store_op("lease ledger boundary", |store| store.change_watermark());
                        state.leases.restore(lease.clone());
                        state.persist("lease", |store| store.upsert_lease(&lease));
                        state.emit(EventKind::LeaseClaimed {
                            lease: lease.clone(),
                        });
                        return Response::Lease { lease };
                    }
                    Ok(Claimed::Renewed(lease)) => {
                        state.persist("lease", |store| store.upsert_lease(&lease));
                        state.emit(EventKind::LeaseRenewed {
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
                    state.emit(EventKind::LeaseConflict {
                        resource: resource.clone(),
                        requester: holder.clone(),
                        held_by: held_by.iter().map(|l| l.holder.clone()).collect(),
                    });
                }
                (message, held_by)
            };
            if wait_secs == 0 || !wait_for_release(&mut events, &resource, deadline).await {
                return Response::Error {
                    code: ErrorCode::Conflict,
                    message,
                    details: Some(json!({ "held_by": held_by })),
                };
            }
        }
    }

    async fn leases(&self, agent: Option<&str>, resource: Option<String>) -> Response {
        let holder = match agent.map(|reference| self.resolve(reference)).transpose() {
            Ok(holder) => holder,
            Err(response) => return *response,
        };
        // Normalize query aliases using the same physical identity as claims.
        let mut keys: Vec<ResourceKey> = Vec::new();
        if let Some(resource) = resource {
            let raw = ResourceKey::new(resource);
            let local = match self.localise(raw.clone(), holder.as_ref(), false).await {
                Ok(key) => key,
                Err(response) => return *response,
            };
            if local != raw {
                keys.push(local);
            }
            keys.push(raw);
        }
        let mut state = lock(&self.state);
        state.expire_leases();
        let leases: Vec<Lease> = state
            .leases
            .all()
            .into_iter()
            .filter(|l| holder.as_ref().is_none_or(|h| l.holder == *h))
            .filter(|l| keys.is_empty() || keys.iter().any(|k| l.resource.overlaps(k)))
            .cloned()
            .collect();
        Response::Leases { leases }
    }

    /// Canonical physical identity for write protection. A logical file key
    /// is accepted only with an explicit holder checkout; it is never a second lock domain.
    async fn localise(
        &self,
        key: ResourceKey,
        holder: Option<&AgentId>,
        _record: bool,
    ) -> Result<ResourceKey, Box<Response>> {
        let path = if key.kind() == "path" {
            let path = PathBuf::from(key.value());
            if !path.is_absolute() {
                return Err(Box::new(Response::error(
                    ErrorCode::Invalid,
                    "path resources must be absolute",
                )));
            }
            path
        } else if key.kind() == "file" {
            let state = lock(&self.state);
            physical_file(
                &key,
                holder
                    .and_then(|id| state.registry.get(id))
                    .and_then(|a| a.project.as_ref()),
            )
            .ok_or_else(|| {
                Box::new(Response::error(
                    ErrorCode::Invalid,
                    "file resources require a matching agent project and a safe relative path",
                ))
            })?
        } else {
            return Ok(key);
        };
        let path = tokio::task::spawn_blocking(move || project::try_canonical(&path))
            .await
            .map_err(|err| Box::new(Response::error(ErrorCode::Internal, err.to_string())))?
            .map_err(|err| Box::new(Response::error(ErrorCode::Invalid, err.to_string())))?;
        if !path.is_absolute() {
            return Err(Box::new(Response::error(
                ErrorCode::Invalid,
                "physical paths must be absolute",
            )));
        }
        Ok(ResourceKey::new(format!("path:{}", path.display())))
    }
}

impl State {
    /// Execute a store operation as part of the current ordered transition.
    fn store_op<T>(
        &mut self,
        what: &str,
        op: impl FnOnce(&Store) -> anyhow::Result<T>,
    ) -> Option<T> {
        if self.storage_error.is_some() {
            return None;
        }
        match op(&self.store) {
            Ok(value) => Some(value),
            Err(err) => {
                error!(%what, %err, "store operation failed");
                self.storage_error = Some(format!("{what}: {err}"));
                None
            }
        }
    }
    fn attribute(&self, path: &Path) -> Attribution {
        let key = ResourceKey::new(format!("path:{}", path.display()));
        let leases = &self.leases;
        let mut overlapping: Vec<&Lease> = leases
            .all()
            .into_iter()
            .filter(|l| {
                l.mode == LeaseMode::Exclusive
                    && !l.is_expired(Utc::now())
                    && l.resource.overlaps(&key)
            })
            .collect();
        overlapping.sort_by_key(|l| (l.mode != LeaseMode::Exclusive, l.acquired_at));
        match overlapping.first() {
            Some(lease) => Attribution::Agent {
                agent: lease.holder.clone(),
                lease: lease.id.clone(),
                note: lease.note.clone(),
            },
            None => Attribution::External,
        }
    }
    fn storage_failure(&self) -> Option<Response> {
        self.storage_error.as_ref().map(|error| {
            Response::error(
                ErrorCode::StorageUnavailable,
                format!("storage failed ({error}); coordination disabled until daemon restart"),
            )
        })
    }

    fn persist(&mut self, what: &str, write: impl FnOnce(&Store) -> anyhow::Result<()>) {
        if self.storage_error.is_some() {
            return;
        }
        if let Err(err) = write(&self.store) {
            error!(%what, %err, "storage failed; disabling coordination until restart");
            self.storage_error = Some(format!("{what}: {err}"));
        }
    }

    pub fn emit(&mut self, kind: EventKind) {
        if self.storage_error.is_some() {
            return;
        }
        let mut event = Event::new(kind, Utc::now());
        event.seq = self.next_seq;
        self.next_seq += 1;
        self.persist("event", |store| store.append_event(&event));
        if self.storage_error.is_none() {
            let _ = self.events.send(event);
        }
    }

    pub fn resolve(&mut self, reference: &str) -> Result<AgentId, Box<Response>> {
        if let Some(error) = self.storage_failure() {
            return Err(Box::new(error));
        }
        self.registry
            .resolve(reference)
            .map_err(|err| Box::new(registry_error(err)))
    }

    pub fn is_live(&mut self, id: &AgentId) -> bool {
        self.registry.get(id).is_some_and(|a| a.status.is_live())
    }

    fn report(&mut self, reference: &str, vcs: Option<VcsState>) -> Response {
        let id = match self.resolve(reference) {
            Ok(id) => id,
            Err(response) => return *response,
        };
        self.touch(&id);
        if let Some(vcs) = vcs {
            self.apply_vcs(&id, vcs);
        }
        Response::Ok
    }

    fn apply_vcs(&mut self, id: &AgentId, vcs: VcsState) {
        if !self.registry.get(id).is_some_and(|a| {
            a.status.is_live()
                && a.vcs
                    .as_ref()
                    .is_none_or(|old| old.updated_at <= vcs.updated_at)
        }) {
            return;
        }
        let Some((record, changed)) = self.registry.set_vcs(id, vcs.clone()) else {
            return;
        };
        if changed {
            info!(agent = %id.short(), checkout = %vcs.describe(), "checkout moved");
            self.persist("agent", |store| store.upsert_agent(&record));
            self.emit(EventKind::AgentVcsChanged {
                agent: id.clone(),
                vcs,
            });
        }
    }

    fn deregister(&mut self, reference: &str) -> Response {
        let id = match self.resolve(reference) {
            Ok(id) => id,
            Err(response) => return *response,
        };
        if !self.is_live(&id) {
            return Response::error(ErrorCode::Invalid, "agent has already finished");
        }
        if self.registry.get(&id).is_some_and(|a| a.managed) {
            return Response::error(
                ErrorCode::Invalid,
                "managed agents finish when their process exits; use stop",
            );
        }
        match self.mark_exited(&id, AgentStatus::Exited { code: Some(0) }) {
            Some(agent) => Response::Agent { agent },
            None => Response::error(ErrorCode::NotFound, "agent vanished"),
        }
    }

    fn remove(&mut self, reference: &str) -> Response {
        let id = match self.resolve(reference) {
            Ok(id) => id,
            Err(response) => return *response,
        };
        if self.is_live(&id) {
            return Response::error(ErrorCode::Invalid, "agent is still live; stop it first");
        }
        self.registry.remove(&id);
        self.inboxes.remove(&id);
        self.journal_cursors
            .retain(|(reader, _), _| reader != id.as_str());
        self.persist("agent", |store| store.delete_agent(&id));
        self.emit(EventKind::AgentRemoved { agent: id });
        Response::Ok
    }

    fn inspect(&mut self, reference: &str) -> Response {
        match self.resolve(reference) {
            Ok(id) => match self.registry.get(&id) {
                Some(agent) => Response::Agent {
                    agent: agent.clone(),
                },
                None => Response::error(ErrorCode::NotFound, "agent vanished"),
            },
            Err(response) => *response,
        }
    }

    pub fn mark_exited(&mut self, id: &AgentId, status: AgentStatus) -> Option<AgentRecord> {
        if !self.is_live(id) {
            return self.registry.get(id).cloned();
        }
        let record = self.registry.set_status(id, status.clone(), Utc::now())?;
        self.supervised.remove(id);
        self.persist("agent", |store| store.upsert_agent(&record));
        let released = self.leases.release_all(id);
        self.finish_release(id, released, None, SummarySource::Explicit);
        self.journal_event(&record, JournalKind::Leave, format!("left ({status})"));
        info!(agent = %id.short(), name = %record.spec.name, %status, "agent finished");
        self.emit(EventKind::AgentExited {
            agent: id.clone(),
            status,
        });
        Some(record)
    }

    fn touch(&mut self, id: &AgentId) {
        let record = {
            let registry = &mut self.registry;
            registry.touch(id, Utc::now());
            registry.get(id).cloned()
        };
        if let Some(record) = record {
            self.persist("agent", |store| store.upsert_agent(&record));
        }
    }

    fn ack_inbox(&mut self, reference: &str, messages: &[MessageId]) -> Response {
        let id = match self.resolve(reference) {
            Ok(id) => id,
            Err(response) => return *response,
        };
        let mut event = Event::new(
            EventKind::InboxAcknowledged {
                agent: id.clone(),
                messages: messages.to_vec(),
            },
            Utc::now(),
        );
        event.seq = self.next_seq;
        self.persist("inbox acknowledgement", |store| {
            store.ack_inbox(&id, messages, &event)
        });
        if let Some(error) = self.storage_failure() {
            return error;
        }
        if let Some(queue) = self.inboxes.get_mut(&id) {
            queue.retain(|message| !messages.contains(&message.id));
        }
        self.next_seq += 1;
        let _ = self.events.send(event);
        Response::Ok
    }

    fn inbox(&mut self, reference: &str, drain: bool) -> Response {
        let id = match self.resolve(reference) {
            Ok(id) => id,
            Err(response) => return *response,
        };
        let messages: Vec<Envelope> = {
            let inboxes = &mut self.inboxes;
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

    fn unsubscribe(&mut self, agent: &AgentId) {
        let live = &mut self.live_subscribers;
        if let Some(count) = live.get_mut(agent) {
            *count -= 1;
            if *count == 0 {
                live.remove(agent);
            }
        }
    }

    fn renew(&mut self, reference: &str, lease: &LeaseId, ttl_secs: u64) -> Response {
        let holder = match self.resolve(reference) {
            Ok(id) => id,
            Err(response) => return *response,
        };
        self.touch(&holder);
        if !self
            .registry
            .get(&holder)
            .is_some_and(|a| a.status == AgentStatus::Running)
        {
            return Response::error(ErrorCode::Invalid, "agent is not running");
        }
        let now = Utc::now();
        self.expire_leases_at(now);
        let result = self.leases.renew(lease, &holder, ttl(ttl_secs), now);
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

    fn release(
        &mut self,
        reference: &str,
        lease: &LeaseId,
        summary: Option<String>,
        source: SummarySource,
    ) -> Response {
        let holder = match self.resolve(reference) {
            Ok(id) => id,
            Err(response) => return *response,
        };
        match self.leases.release(lease, &holder) {
            Ok(lease) => {
                let mut released = self.finish_release(&holder, vec![lease], summary, source);
                Response::Lease {
                    lease: released.remove(0),
                }
            }
            Err(err) => lease_error(err),
        }
    }

    fn release_all(
        &mut self,
        reference: &str,
        summary: Option<String>,
        source: SummarySource,
    ) -> Response {
        let holder = match self.resolve(reference) {
            Ok(id) => id,
            Err(response) => return *response,
        };
        let released = self.leases.release_all(&holder);
        let released = self.finish_release(&holder, released, summary, source);
        Response::Leases { leases: released }
    }

    /// Leases already dropped from the table: persist their deletion with
    /// the journal entry describing the release in one transaction, then
    /// announce both. Returns the leases for the reply.
    fn finish_release(
        &mut self,
        holder: &AgentId,
        released: Vec<Lease>,
        summary: Option<String>,
        source: SummarySource,
    ) -> Vec<Lease> {
        // A summary the agent typed is explicit whatever a client claims;
        // only a quoted transcript is not.
        let source = match source {
            SummarySource::Transcript => SummarySource::Transcript,
            _ => SummarySource::Explicit,
        };
        if released.is_empty() {
            // Nothing was freed, but an explicit summary is still something
            // said: record it rather than drop the agent's text silently. A
            // transcript tail only ever describes leases actually released.
            let explicit = summary
                .filter(|_| source == SummarySource::Explicit)
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty());
            if let (Some(text), Some(record)) = (explicit, self.registry.get(holder).cloned()) {
                if let Some(entry) =
                    self.plain_entry(&record, JournalKind::Release, text, SummarySource::Explicit)
                {
                    self.append_journal(entry);
                }
            }
            return released;
        }
        let entry = self
            .release_entry(holder, &released, summary, source)
            .map(|mut entry| {
                entry.seq = self.next_journal_seq(&entry.project);
                entry
            });
        let ids: Vec<LeaseId> = released.iter().map(|l| l.id.clone()).collect();
        let now = Utc::now();
        let mut events: Vec<Event> = released
            .iter()
            .enumerate()
            .map(|(i, lease)| {
                let mut event = Event::new(
                    EventKind::LeaseReleased {
                        lease: lease.clone(),
                    },
                    now,
                );
                event.seq = self.next_seq + i as u64;
                event
            })
            .collect();
        if let Some(entry) = &entry {
            let mut event = Event::new(
                EventKind::JournalAppended {
                    entry: entry.clone(),
                },
                now,
            );
            event.seq = self.next_seq + events.len() as u64;
            events.push(event);
        }
        self.persist("release", |store| {
            store.release_leases(&ids, entry.as_ref(), &events)
        });
        if self.storage_error.is_none() {
            if let Some(entry) = entry {
                self.cache_journal(entry);
            }
            self.next_seq += events.len() as u64;
            for event in events {
                let _ = self.events.send(event);
            }
        }
        released
    }

    /// The journal entry for a release: what the ledger saw change under
    /// the released paths while they were held, plus the summary the agent
    /// gave, or one synthesised from those paths. `None` when there is
    /// nothing to say.
    fn release_entry(
        &mut self,
        holder: &AgentId,
        released: &[Lease],
        summary: Option<String>,
        source: SummarySource,
    ) -> Option<JournalEntry> {
        let record = self.registry.get(holder)?.clone();
        let project = record.project.clone()?;
        let checkout = project.dir().to_path_buf();
        let mut paths: Vec<PathBuf> = Vec::new();
        let mut range: Option<(u64, u64)> = None;
        let mut head_before: Option<String> = None;
        for lease in released.iter().filter(|l| l.resource.kind() == "path") {
            let physical = Path::new(lease.resource.value());
            let Ok(relative) = physical.strip_prefix(&checkout) else {
                continue;
            };
            let query = ChangesQuery {
                project: project.id(),
                since_seq: lease.change_seq,
                path: Some(relative.to_string_lossy().into_owned()),
                agent: None,
                limit: RELEASE_SCAN,
                after: lease.change_seq.is_none().then_some(lease.acquired_at),
            };
            let Some(changes) = self.store_op("journal", |store| store.changes(&query)) else {
                continue;
            };
            for change in changes
                .into_iter()
                .filter(|c| c.checkout.as_deref().is_none_or(|dir| dir == checkout))
            {
                range = Some(match range {
                    None => (change.seq, change.seq),
                    Some((lo, hi)) => (lo.min(change.seq), hi.max(change.seq)),
                });
                if head_before.is_none() {
                    head_before = change.head.clone();
                }
                paths.push(change.path);
            }
        }
        let (paths, paths_total) = cap_paths(paths);
        let (summary, summary_source) = match summary
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
        {
            Some(text) => (text, source),
            None if paths.is_empty() => return None,
            None => (
                synthesise_summary(&paths, paths_total),
                SummarySource::Synthesised,
            ),
        };
        Some(JournalEntry {
            project: project.id(),
            seq: 0,
            at: Utc::now(),
            agent: Some(holder.clone()),
            agent_name: record.spec.name.clone(),
            branch: record.vcs.as_ref().and_then(|v| v.branch.clone()),
            checkout: Some(checkout),
            worktree: project.worktree.clone(),
            kind: JournalKind::Release,
            summary,
            summary_source,
            resources: released.iter().map(|l| l.resource.clone()).collect(),
            paths,
            paths_total,
            head_before,
            head_after: record.vcs.as_ref().and_then(|v| v.head.clone()),
            changes: range,
        })
    }

    /// A journal entry about an agent that needs no ledger: join, leave,
    /// note, commit.
    fn plain_entry(
        &self,
        record: &AgentRecord,
        kind: JournalKind,
        summary: String,
        source: SummarySource,
    ) -> Option<JournalEntry> {
        let project = record.project.as_ref()?;
        Some(JournalEntry {
            project: project.id(),
            seq: 0,
            at: Utc::now(),
            agent: Some(record.id.clone()),
            agent_name: record.spec.name.clone(),
            branch: record.vcs.as_ref().and_then(|v| v.branch.clone()),
            checkout: Some(project.dir().to_path_buf()),
            worktree: project.worktree.clone(),
            kind,
            summary,
            summary_source: source,
            resources: Vec::new(),
            paths: Vec::new(),
            paths_total: 0,
            head_before: None,
            head_after: record.vcs.as_ref().and_then(|v| v.head.clone()),
            changes: None,
        })
    }

    fn next_journal_seq(&mut self, project: &ProjectId) -> u64 {
        if let Some(next) = self.journal_seq.get_mut(project) {
            let seq = *next;
            *next += 1;
            return seq;
        }
        let stored = self
            .store_op("journal", |store| store.max_journal_seq(project))
            .unwrap_or(0);
        self.journal_seq.insert(project.clone(), stored + 2);
        stored + 1
    }

    /// Assign a seq, persist (own transaction), ring, announce.
    fn append_journal(&mut self, mut entry: JournalEntry) -> JournalEntry {
        entry.seq = self.next_journal_seq(&entry.project);
        let mut event = Event::new(
            EventKind::JournalAppended {
                entry: entry.clone(),
            },
            Utc::now(),
        );
        event.seq = self.next_seq;
        self.persist("journal", |store| {
            store.append_journal_with_event(&entry, &event)
        });
        if self.storage_error.is_none() {
            self.cache_journal(entry.clone());
            self.next_seq += 1;
            let _ = self.events.send(event);
        }
        entry
    }

    /// Cache an entry only after its journal row and event have committed.
    fn cache_journal(&mut self, entry: JournalEntry) {
        if let Some(ring) = self.journal_rings.get_mut(&entry.project) {
            ring.entries.push_back(entry.clone());
            while ring.entries.len() > JOURNAL_RING {
                ring.entries.pop_front();
            }
            ring.touched = Instant::now();
        }
        debug!(project = %entry.project.short(), seq = entry.seq, kind = %entry.kind, "journal");
    }

    /// Entries with a seq assigned go through the same path whether they
    /// travel with a release or alone.
    fn journal_event(&mut self, record: &AgentRecord, kind: JournalKind, summary: String) {
        if let Some(entry) = self.plain_entry(record, kind, summary, SummarySource::Synthesised) {
            self.append_journal(entry);
        }
    }

    fn journal_add(&mut self, reference: &str, summary: String) -> Response {
        let id = match self.resolve(reference) {
            Ok(id) => id,
            Err(response) => return *response,
        };
        let summary = summary.trim().to_owned();
        if summary.is_empty() {
            return Response::error(ErrorCode::Invalid, "a note needs some text");
        }
        let Some(record) = self.registry.get(&id).cloned() else {
            return Response::error(ErrorCode::NotFound, "agent vanished");
        };
        let Some(entry) =
            self.plain_entry(&record, JournalKind::Note, summary, SummarySource::Explicit)
        else {
            return Response::error(ErrorCode::Invalid, "the agent is in no project");
        };
        let entry = self.append_journal(entry);
        Response::JournalEntry { entry }
    }

    /// A checkout's HEAD moved: one `commit` entry per checkout and HEAD,
    /// however many agents share it. Attributed to the only agent in that
    /// checkout, else the holder of its `branch:` lease, else nobody.
    fn note_head_move(
        &mut self,
        id: &AgentId,
        old: Option<&VcsState>,
        new: &VcsState,
        subject: Option<String>,
    ) {
        let Some(head) = new.head.clone() else {
            return;
        };
        let Some(record) = self.registry.get(id).cloned() else {
            return;
        };
        let Some(project) = record.project.clone() else {
            return;
        };
        let checkout = project.dir().to_path_buf();
        if self.last_head.get(&checkout) == Some(&head) {
            return;
        }
        self.last_head.insert(checkout.clone(), head.clone());
        if old.is_none() {
            return; // first observation, not a move
        }
        let short: String = head.chars().take(7).collect();
        let summary = match (&old.and_then(|o| o.branch.clone()), &new.branch) {
            (before, Some(branch)) if before.as_deref() != Some(branch) => {
                format!("switched to {branch} at {short}")
            }
            (_, None) => format!("detached at {short}"),
            _ => match subject {
                Some(subject) => format!("committed {short}: {subject}"),
                None => format!("committed {short}"),
            },
        };
        let in_checkout: Vec<AgentRecord> = self
            .registry
            .live()
            .filter(|a| a.project.as_ref().is_some_and(|p| p.dir() == checkout))
            .cloned()
            .collect();
        let attributed = match in_checkout.as_slice() {
            [one] => Some(one.clone()),
            _ => new.branch.as_ref().and_then(|branch| {
                let key = ResourceKey::new(format!("branch:{branch}"));
                let holder = self
                    .leases
                    .holders_of(&key)
                    .first()
                    .map(|l| l.holder.clone())?;
                self.registry.get(&holder).cloned()
            }),
        };
        let attributed_entry = attributed.as_ref().and_then(|agent| {
            self.plain_entry(
                agent,
                JournalKind::Commit,
                summary.clone(),
                SummarySource::Synthesised,
            )
        });
        // The branch holder may work outside any project; the move is then
        // recorded against the observed checkout and attributed to nobody.
        let Some(mut entry) = attributed_entry.or_else(|| {
            self.plain_entry(
                &record,
                JournalKind::Commit,
                summary,
                SummarySource::Synthesised,
            )
            .map(|mut e| {
                e.agent = None;
                e.agent_name = "external".to_owned();
                e
            })
        }) else {
            return;
        };
        entry.branch = new.branch.clone();
        entry.head_before = old.and_then(|o| o.head.clone());
        entry.head_after = Some(head);
        self.append_journal(entry);
    }

    /// Serve a query, from the ring when it can be.
    fn journal_query(&mut self, query: JournalQuery) -> Response {
        let simple = query.agent.is_none()
            && query.branch.is_none()
            && query.kind.is_none()
            && query.path.is_none()
            && query.grep.is_none()
            && query.until_seq.is_none();
        if simple {
            let ring = self.ring(&query.project);
            let oldest = ring.entries.front().map(|e| e.seq).unwrap_or(u64::MAX);
            let covers = ring.entries.len() < JOURNAL_RING
                || query
                    .since_seq
                    .is_some_and(|since| since.saturating_add(1) >= oldest);
            if covers {
                let entries: Vec<JournalEntry> = ring
                    .entries
                    .iter()
                    .filter(|e| query.since_seq.is_none_or(|since| e.seq > since))
                    .cloned()
                    .collect();
                let start = entries.len().saturating_sub(query.limit.max(1));
                return Response::Journal {
                    project: query.project,
                    entries: entries[start..].to_vec(),
                };
            }
        }
        match self.store_op("journal", |store| store.journal(&query)) {
            Some(entries) => Response::Journal {
                project: query.project,
                entries,
            },
            None => Response::error(ErrorCode::Internal, "journal query failed"),
        }
    }

    /// Entries after the reader's cursor (or `since_seq`), rendered within
    /// the budget; with `advance`, the cursor moves to the head when text
    /// was produced. Served from the ring when the cursor lies inside it.
    fn journal_digest(
        &mut self,
        project: ProjectId,
        key: &str,
        name: &str,
        branch: Option<&str>,
        since_seq: Option<u64>,
        request: &DigestRequest,
    ) -> Response {
        let now = Utc::now();
        let cursor = match since_seq.or_else(|| self.cursor(key, &project)) {
            Some(cursor) => cursor,
            None => initial_cursor(self.ring(&project).entries.make_contiguous(), now),
        };
        let entries: Vec<JournalEntry> = {
            let ring = self.ring(&project);
            let oldest = ring.entries.front().map(|e| e.seq).unwrap_or(u64::MAX);
            if ring.entries.len() < JOURNAL_RING || cursor.saturating_add(1) >= oldest {
                ring.entries
                    .iter()
                    .filter(|e| e.seq > cursor)
                    .cloned()
                    .collect()
            } else {
                let mut query = JournalQuery::new(project.clone(), DIGEST_SCAN);
                query.since_seq = Some(cursor);
                self.store_op("journal", |store| store.journal(&query))
                    .unwrap_or_default()
            }
        };
        let reader = Reader {
            name,
            branch,
            all_branches: request.all_branches,
        };
        let budget = DigestBudget {
            max_entries: request.max_entries,
            max_chars: request.max_chars,
        };
        let digest = render_digest(&entries, cursor, &reader, budget, now);
        if request.advance && !digest.text.is_empty() {
            self.move_cursor(key, &project, digest.head_seq);
        }
        Response::Digest { project, digest }
    }

    /// A reader's cursor, from the cache or the store; `None` for a reader
    /// that has never been shown anything in the project.
    fn cursor(&mut self, key: &str, project: &ProjectId) -> Option<u64> {
        let cache_key = (key.to_owned(), project.clone());
        if let Some(seq) = self.journal_cursors.get(&cache_key) {
            return Some(*seq);
        }
        let found = self
            .store_op("journal", |store| store.journal_cursor(key, project))
            .flatten();
        if let Some(seq) = found {
            self.journal_cursors.insert(cache_key, seq);
        }
        found
    }

    /// Record what a reader has been shown. Only ever forward, and written
    /// only when it moves.
    fn move_cursor(&mut self, key: &str, project: &ProjectId, seq: u64) {
        if self
            .cursor(key, project)
            .is_some_and(|current| current >= seq)
        {
            return;
        }
        let mut event = Event::new(
            EventKind::JournalRead {
                reader: key.to_owned(),
                project: project.clone(),
                seq,
            },
            Utc::now(),
        );
        event.seq = self.next_seq;
        self.persist("journal cursor", |store| {
            store.set_journal_cursor_with_event(key, project, seq, &event)
        });
        if self.storage_error.is_none() {
            self.journal_cursors
                .insert((key.to_owned(), project.clone()), seq);
            self.next_seq += 1;
            let _ = self.events.send(event);
        }
    }

    /// The project's ring, loaded from the store on first use.
    fn ring(&mut self, project: &ProjectId) -> &mut JournalRing {
        if !self.journal_rings.contains_key(project) {
            let newest = self
                .store_op("journal", |store| {
                    store.journal(&JournalQuery::new(project.clone(), JOURNAL_RING))
                })
                .unwrap_or_default();
            self.journal_rings.insert(
                project.clone(),
                JournalRing {
                    entries: newest.into_iter().collect(),
                    touched: Instant::now(),
                },
            );
        }
        let ring = self.journal_rings.get_mut(project).expect("just inserted");
        ring.touched = Instant::now();
        ring
    }

    /// Drop rings of projects with no live agent that nobody read lately.
    pub fn evict_journal_rings(&mut self) {
        let active: HashSet<ProjectId> = self
            .registry
            .live()
            .filter_map(|a| a.project.as_ref().map(ProjectRef::id))
            .collect();
        self.journal_rings
            .retain(|project, ring| active.contains(project) || ring.touched.elapsed() < RING_IDLE);
    }

    fn journal_prune(&mut self, project: &ProjectId, before_seq: u64) -> Response {
        match self.store_op("journal", |store| store.prune_journal(project, before_seq)) {
            Some(removed) => {
                if let Some(ring) = self.journal_rings.get_mut(project) {
                    ring.entries.retain(|e| e.seq >= before_seq);
                }
                if removed > 0 {
                    info!(project = %project.short(), removed, "pruned the journal");
                }
                Response::Pruned { removed }
            }
            None => Response::error(ErrorCode::Internal, "journal prune failed"),
        }
    }

    pub fn expire_leases(&mut self) {
        self.expire_leases_at(Utc::now());
    }

    fn persist_lease_removal(&mut self, lease: &Lease, expired: bool) {
        let kind = if expired {
            EventKind::LeaseExpired {
                lease: lease.clone(),
            }
        } else {
            EventKind::LeaseReleased {
                lease: lease.clone(),
            }
        };
        let mut event = Event::new(kind, Utc::now());
        event.seq = self.next_seq;
        self.persist("lease removal", |store| {
            store.delete_lease_with_event(&lease.id, &event)
        });
        if self.storage_error.is_none() {
            self.next_seq += 1;
            let _ = self.events.send(event);
        }
    }

    fn expire_leases_at(&mut self, now: DateTime<Utc>) {
        let expired = self.leases.expire(now);
        for lease in expired {
            info!(lease = %lease.id, holder = %lease.holder.short(), resource = %lease.resource, "lease expired");
            self.persist_lease_removal(&lease, true);
        }
    }

    pub fn prune_events(&mut self) {
        match self.store.prune_events(EVENT_HISTORY) {
            Ok(0) => {}
            Ok(removed) => info!(removed, "pruned event history"),
            Err(err) => error!(%err, "failed to prune event history"),
        }
    }

    fn send(
        &mut self,
        from: String,
        to: Destination,
        kind: String,
        payload: Value,
        reply_to: Option<MessageId>,
    ) -> Response {
        let envelope = Envelope::new(from, to, kind, payload, reply_to, Utc::now());

        let recipients: Vec<AgentId> = match &envelope.to {
            Destination::Agent(id) => vec![id.clone()],
            Destination::Broadcast => self
                .registry
                .live()
                .filter(|a| a.id.as_str() != envelope.from)
                .map(|a| a.id.clone())
                .collect(),
            Destination::Project(project) => self
                .registry
                .live()
                .filter(|a| a.id.as_str() != envelope.from)
                .filter(|a| a.project.as_ref().is_some_and(|p| p.id() == *project))
                .map(|a| a.id.clone())
                .collect(),
            Destination::Topic(_) => Vec::new(),
        };
        let offline: Vec<AgentId> = {
            let live = &self.live_subscribers;
            recipients
                .into_iter()
                .filter(|id| !live.contains_key(id))
                .collect()
        };
        if !offline.is_empty() {
            {
                let inboxes = &mut self.inboxes;
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

        if let Some(error) = self.storage_failure() {
            return error;
        }
        self.touch(&AgentId::from(envelope.from.as_str()));
        self.emit(EventKind::MessageSent {
            message: envelope.id.clone(),
            from: envelope.from.clone(),
            to: envelope.to.clone(),
            kind: envelope.kind.clone(),
        });
        if let Some(error) = self.storage_failure() {
            return error;
        }
        let subscribers = self.bus.send(envelope.clone()).unwrap_or(0);
        Response::Sent {
            message: envelope.id,
            subscribers,
        }
    }

    fn insert_record(&mut self, mut record: AgentRecord) -> Response {
        if let Some(error) = self.storage_failure() {
            return error;
        }
        if record.spec.name.is_empty() {
            record.spec.name = default_name(&record.id);
        }
        if record
            .spec
            .labels
            .get("adopted")
            .is_some_and(|v| v == "true")
            && self.registry.live().any(|a| a.pid == record.pid)
        {
            return Response::error(ErrorCode::Invalid, "pid is already registered");
        }
        if let Err(err) = self.registry.insert(record.clone()) {
            return registry_error(err);
        }
        self.persist("agent", |store| store.upsert_agent(&record));
        self.emit(EventKind::AgentCreated {
            agent: record.id.clone(),
            name: record.spec.name.clone(),
            project: record.project.as_ref().map(ProjectRef::id),
        });
        if !record.managed {
            self.emit(EventKind::AgentStarted {
                agent: record.id.clone(),
                pid: record.pid,
            });
        }
        if let Some(project) = &record.project {
            let mut what = String::from("joined");
            let mut details: Vec<String> = Vec::new();
            if let Some(worktree) = &project.worktree {
                details.push(format!(
                    "worktree {}",
                    worktree
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default()
                ));
            }
            if let Some(branch) = record.vcs.as_ref().and_then(|v| v.branch.clone()) {
                details.push(format!("branch {branch}"));
            }
            if !details.is_empty() {
                what.push_str(&format!(" ({})", details.join(", ")));
            }
            // Seed the newcomer's cursor: a finished namesake's, so a resumed
            // session continues where it left off, else recent history.
            let now = Utc::now();
            let project_id = project.id();
            let donor = cursor_donor(self.registry.all(), &record.spec.name, &project_id, now)
                .map(|donor| donor.id.as_str().to_owned());
            let seed = match donor.and_then(|donor| self.cursor(&donor, &project_id)) {
                Some(seq) => seq,
                None => initial_cursor(self.ring(&project_id).entries.make_contiguous(), now),
            };
            self.move_cursor(record.id.as_str(), &project_id, seed);
            self.journal_event(&record, JournalKind::Join, what);
        }
        self.storage_failure()
            .unwrap_or(Response::Agent { agent: record })
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

    #[tokio::test]
    async fn inbox_ack_is_idempotent_and_preserves_new_arrivals() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        register(&daemon, "receiver", None).await;
        for text in ["first", "second"] {
            daemon
                .handle(Request::Send {
                    from: "user".into(),
                    to: "receiver".into(),
                    kind: "chat".into(),
                    payload: json!({"text": text}),
                    reply_to: None,
                })
                .await;
        }
        let first = inbox(&daemon, "receiver", false).await[0].id.clone();
        for _ in 0..2 {
            assert!(matches!(
                daemon
                    .handle(Request::AckInbox {
                        agent: "receiver".into(),
                        messages: vec![first.clone()],
                    })
                    .await,
                Response::Ok
            ));
        }
        let remaining = inbox(&daemon, "receiver", false).await;
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].payload["text"], "second");
        assert_eq!(
            lock(&daemon.state)
                .store
                .load_inboxes()
                .unwrap()
                .values()
                .next()
                .unwrap()
                .len(),
            1
        );
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
        let agents = lock(&daemon.state).registry.list(true);
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
        assert!(lock(&daemon.state).leases.is_empty());
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
        assert!(lock(&daemon.state).registry.get(&done.id).is_none());
        assert!(lock(&daemon.state).inboxes.is_empty());
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
        lock(&daemon.state).registry.insert(record).unwrap();

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
                change_seq: None,
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
        let agents = lock(&daemon.state).registry.list(true);
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
        let events = daemon.recent_events(100);
        let mut removed: Vec<_> = events
            .iter()
            .filter_map(|event| match &event.kind {
                EventKind::LeaseReleased { lease } => Some(lease.id.to_string()),
                _ => None,
            })
            .collect();
        removed.sort();
        assert_eq!(removed, ["orphan", "twin2"]);
        drop(daemon);
        let daemon = open(&dir);
        assert_eq!(
            daemon.recent_events(100),
            events,
            "restart does not duplicate cleanup events"
        );
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
        lock(&daemon.state)
            .registry
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
                summary: None,
                summary_source: SummarySource::Explicit,
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
    async fn path_claims_protect_the_physical_checkout() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let alpha = dir.path().join("alpha");
        std::fs::create_dir_all(alpha.join("src")).unwrap();
        std::fs::write(alpha.join("Agentfile.toml"), "").unwrap();
        let one = register_in(&daemon, "one", &alpha).await;
        register_in(&daemon, "two", &alpha.join("src")).await;
        register(&daemon, "outsider", None).await;
        let _project = one.project.clone().unwrap().id();

        // The claimed path is under the non-canonical temp dir and the file
        // does not exist yet; the key still has its eventual physical identity.
        let lib = alpha.join("src/lib.rs");
        let Response::Lease { lease } =
            claim(&daemon, "one", &format!("path:{}", lib.display())).await
        else {
            panic!("claim failed");
        };
        assert_eq!(
            lease.resource.as_str(),
            format!("path:{}", project::canonical(&lib).display())
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

    #[tokio::test]
    async fn discover_finds_known_runtimes_and_adopt_registers_them() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        // A process whose executable is called `claude`, working in an
        // Agentfile project, is what a hook-less Claude Code session looks
        // like from the process table.
        let project = dir.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("Agentfile.toml"), "").unwrap();
        let fake = dir.path().join("claude");
        std::os::unix::fs::symlink("/bin/sleep", &fake).unwrap();
        let mut child = std::process::Command::new(&fake)
            .arg("60")
            .current_dir(&project)
            .spawn()
            .unwrap();
        let pid = child.id();

        let found = match daemon.handle(Request::Discover).await {
            Response::Processes { processes } => processes,
            other => panic!("unexpected {other:?}"),
        };
        let mine = found
            .iter()
            .find(|p| p.pid == pid)
            .expect("the fake claude is discovered");
        assert_eq!(mine.runtime, "claude-code");
        assert_eq!(mine.cwd, Some(project.canonicalize().unwrap()));
        assert_eq!(
            mine.project.as_ref().map(|p| p.source),
            Some(ProjectSource::Agentfile)
        );
        assert_eq!(mine.default_name(), format!("claude-code-{pid}"));

        let adopted = match daemon
            .handle(Request::Adopt {
                pid,
                name: None,
                runtime: None,
            })
            .await
        {
            Response::Agent { agent } => agent,
            other => panic!("unexpected {other:?}"),
        };
        assert_eq!(adopted.spec.name, format!("claude-code-{pid}"));
        assert_eq!(adopted.spec.runtime, "claude-code");
        assert_eq!(adopted.pid, Some(pid));
        assert_eq!(
            adopted.spec.labels.get("adopted").map(String::as_str),
            Some("true")
        );
        assert_eq!(
            adopted.project.as_ref().map(|p| p.root.clone()),
            Some(project.canonicalize().unwrap())
        );
        assert!(process_exists(pid));

        // Registered pids disappear from discovery, and cannot be adopted twice.
        let found = match daemon.handle(Request::Discover).await {
            Response::Processes { processes } => processes,
            other => panic!("unexpected {other:?}"),
        };
        assert!(found.iter().all(|p| p.pid != pid));
        assert!(matches!(
            daemon
                .handle(Request::Adopt {
                    pid,
                    name: None,
                    runtime: None
                })
                .await,
            Response::Error {
                code: ErrorCode::Invalid,
                ..
            }
        ));

        child.kill().unwrap();
        child.wait().unwrap();
        assert!(matches!(
            daemon
                .handle(Request::Adopt {
                    pid: dead_pid(),
                    name: None,
                    runtime: None
                })
                .await,
            Response::Error {
                code: ErrorCode::NotFound,
                ..
            }
        ));
    }

    fn drain_vcs(events: &mut broadcast::Receiver<Event>) -> Vec<Option<String>> {
        let mut branches = Vec::new();
        while let Ok(event) = events.try_recv() {
            if let EventKind::AgentVcsChanged { vcs, .. } = event.kind {
                branches.push(vcs.branch);
            }
        }
        branches
    }

    #[tokio::test]
    async fn checkout_is_observed_at_creation_refreshed_and_reported() {
        if !have_git() {
            return;
        }
        let dir = TempDir::new().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        assert!(git(dir.path(), &repo, &["init", "-q"]));
        assert!(git(
            dir.path(),
            &repo,
            &["commit", "-q", "--allow-empty", "-m", "root"]
        ));
        let daemon = open(&dir);
        let mut events = daemon.subscribe_events();

        // Known from the moment of registration, with no event yet.
        let a = register_in(&daemon, "a", &repo).await;
        let initial = a.vcs.clone().expect("checkout read at creation");
        assert_eq!(initial.branch.as_deref(), Some("main"));
        assert!(initial.head.is_some());
        assert!(drain_vcs(&mut events).is_empty());

        // The timer notices a branch switch made outside AgentDocker.
        assert!(git(dir.path(), &repo, &["checkout", "-q", "-b", "feature"]));
        daemon.refresh_vcs(None).await;
        assert_eq!(drain_vcs(&mut events), vec![Some("feature".to_owned())]);
        daemon.refresh_vcs(None).await;
        assert!(drain_vcs(&mut events).is_empty(), "no change, no event");

        // A hook can report ahead of the timer; the same state is silent,
        // a different one is announced and visible in the record.
        let reported = VcsState {
            branch: Some("feature".to_owned()),
            head: initial.head.clone(),
            dirty: None,
            updated_at: Utc::now(),
        };
        assert!(matches!(
            daemon
                .handle(Request::Report {
                    agent: "a".to_owned(),
                    vcs: Some(reported.clone()),
                })
                .await,
            Response::Ok
        ));
        assert!(drain_vcs(&mut events).is_empty());
        daemon
            .handle(Request::Report {
                agent: "a".to_owned(),
                vcs: Some(VcsState {
                    branch: Some("hotfix".to_owned()),
                    ..reported
                }),
            })
            .await;
        assert_eq!(drain_vcs(&mut events), vec![Some("hotfix".to_owned())]);
        let Response::Agent { agent } = daemon
            .handle(Request::Inspect {
                agent: "a".to_owned(),
            })
            .await
        else {
            panic!("inspect failed");
        };
        assert_eq!(agent.vcs.unwrap().branch.as_deref(), Some("hotfix"));
    }

    #[tokio::test]
    async fn aliases_and_root_claims_conflict_for_outsiders_without_project_markers() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let checkout = dir.path().join("checkout");
        std::fs::create_dir_all(&checkout).unwrap();
        let owner = register_in(&daemon, "owner", &checkout).await;
        register(&daemon, "outsider", None).await;
        let path = checkout.join("file");
        assert!(matches!(
            claim(&daemon, "owner", &format!("path:{}", path.display())).await,
            Response::Lease { .. }
        ));
        for alias in [&path, &checkout, &checkout.join("missing/../file")] {
            assert!(matches!(
                claim(&daemon, "outsider", &format!("path:{}", alias.display())).await,
                Response::Error {
                    code: ErrorCode::Conflict,
                    ..
                }
            ));
        }
        let id = owner.project.unwrap().id();
        assert!(matches!(
            claim(&daemon, "owner", &format!("file:{id}/file")).await,
            Response::Lease { .. }
        ));
        let Response::Leases { leases } = daemon
            .handle(Request::Leases {
                agent: None,
                resource: Some(format!("path:{}", checkout.display())),
            })
            .await
        else {
            panic!()
        };
        assert_eq!(leases.len(), 1);
    }

    #[tokio::test]
    async fn exited_agents_cannot_claim_or_win_a_pending_wait() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        register(&daemon, "holder", None).await;
        register(&daemon, "waiter", None).await;
        assert!(matches!(
            claim(&daemon, "holder", "task:wait").await,
            Response::Lease { .. }
        ));
        let mut events = daemon.subscribe_events();
        let pending = {
            let daemon = daemon.clone();
            tokio::spawn(async move {
                daemon
                    .handle(Request::Claim {
                        agent: "waiter".into(),
                        resource: "task:wait".into(),
                        mode: LeaseMode::Exclusive,
                        ttl_secs: 60,
                        note: None,
                        wait_secs: 5,
                    })
                    .await
            })
        };
        while !matches!(
            events.recv().await.unwrap().kind,
            EventKind::LeaseConflict { .. }
        ) {}
        daemon
            .handle(Request::Deregister {
                agent: "waiter".into(),
            })
            .await;
        daemon
            .handle(Request::ReleaseAll {
                agent: "holder".into(),
                summary: None,
                summary_source: SummarySource::Explicit,
            })
            .await;
        assert!(matches!(
            pending.await.unwrap(),
            Response::Error {
                code: ErrorCode::Invalid,
                ..
            }
        ));
        assert!(matches!(
            claim(&daemon, "waiter", "task:other").await,
            Response::Error {
                code: ErrorCode::Invalid,
                ..
            }
        ));
        assert!(list_leases(&daemon).await.is_empty());
    }

    #[tokio::test]
    async fn invalid_pid_registration_never_reaches_a_signal_target() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        for pid in [0, i32::MAX as u32 + 1, u32::MAX] {
            assert!(signal_pid(pid).is_none());
            assert!(matches!(
                daemon
                    .handle(Request::Register {
                        spec: spec("invalid"),
                        pid: Some(pid)
                    })
                    .await,
                Response::Error {
                    code: ErrorCode::Invalid,
                    ..
                }
            ));
        }
    }

    #[tokio::test]
    async fn stopping_process_keeps_leases_until_observed_exit() {
        use std::io::{BufRead, BufReader};
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        // The only process signalled is this test's child. trap is installed before READY.
        let mut child = std::process::Command::new("sh")
            .args(["-c", "trap '' TERM; echo READY; while :; do :; done"])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut ready = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut ready)
            .unwrap();
        assert_eq!(ready.trim(), "READY");
        let record = register(&daemon, "resistant", Some(child.id())).await;
        claim(&daemon, "resistant", "task:live").await;
        let response = daemon
            .handle(Request::Stop {
                agent: record.id.to_string(),
                force: false,
            })
            .await;
        let alive = child.try_wait().unwrap().is_none();
        let held = list_leases(&daemon).await.len();
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(
            matches!(response,Response::Agent {agent} if agent.status == AgentStatus::Stopping)
        );
        assert!(alive);
        assert_eq!(held, 1);
        daemon.check_liveness();
        assert!(list_leases(&daemon).await.is_empty());
    }

    #[tokio::test]
    async fn managed_group_keeps_protection_until_descendants_stop() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let mut command = spec("managed-group");
        command.workdir = Some(dir.path().to_path_buf());
        // A file gate makes the lease acquisition deterministic. The child
        // ignores TERM, so the supervisor must escalate and observe its exit.
        command.command = vec!["sh".into(), "-c".into(),
            "trap '' TERM; sleep 30 & echo $! > child.pid; while [ ! -f exit-now ]; do sleep 0.05; done".into()];
        let Response::Agent { agent } = daemon.handle(Request::Run { spec: command }).await else {
            panic!("managed launch failed");
        };
        assert_eq!(agent.process_group, agent.pid);
        assert!(matches!(
            claim(&daemon, "managed-group", "task:group").await,
            Response::Lease { .. }
        ));
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !dir.path().join("child.pid").exists() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        std::fs::write(dir.path().join("exit-now"), "").unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert!(supervisor::group_exists(agent.pid.unwrap()));
        assert_eq!(list_leases(&daemon).await.len(), 1);
        tokio::time::timeout(std::time::Duration::from_secs(8), async {
            while !list_leases(&daemon).await.is_empty() {
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
        })
        .await
        .unwrap();
        assert!(!supervisor::group_exists(agent.pid.unwrap()));
    }

    #[tokio::test]
    async fn failed_storage_never_acknowledges_or_serves_new_coordination() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        register(&daemon, "owner", None).await;
        register(&daemon, "other", None).await;
        let Response::Lease { lease } = claim(&daemon, "owner", "task:durable").await else {
            panic!()
        };
        let before = daemon.recent_events(100).len();
        lock(&daemon.state).store.reject_writes_for_test();
        let failed = daemon
            .handle(Request::Release {
                summary: None,
                summary_source: agentdocker_core::SummarySource::Explicit,
                agent: "owner".into(),
                lease: lease.id,
            })
            .await;
        assert!(matches!(
            failed,
            Response::Error {
                code: ErrorCode::StorageUnavailable,
                ..
            }
        ));
        assert!(matches!(
            claim(&daemon, "other", "task:durable").await,
            Response::Error {
                code: ErrorCode::StorageUnavailable,
                ..
            }
        ));
        assert_eq!(daemon.recent_events(100).len(), before);
        drop(daemon);
        let daemon = open(&dir);
        assert!(matches!(
            claim(&daemon, "other", "task:durable").await,
            Response::Error {
                code: ErrorCode::Conflict,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn failed_message_event_never_reaches_a_live_subscriber() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        register(&daemon, "sender", None).await;
        let recipient = register(&daemon, "recipient", None).await;
        let (_subscription, mut messages) = daemon.subscribe(Some("recipient"), vec![]).unwrap();
        let mut events = daemon.subscribe_events();
        lock(&daemon.state).next_seq -= 1;
        let response = lock(&daemon.state).send(
            "sender".into(),
            Destination::Agent(recipient.id),
            "chat".into(),
            json!({"text": "must not be delivered"}),
            None,
        );
        assert!(matches!(
            response,
            Response::Error {
                code: ErrorCode::StorageUnavailable,
                ..
            }
        ));
        assert!(messages.try_recv().is_err());
        assert!(events.try_recv().is_err());
    }

    #[tokio::test]
    async fn managed_shutdown_uses_owned_child_when_start_time_is_unavailable() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let mut command = spec("owned");
        command.workdir = Some(dir.path().to_path_buf());
        command.command = vec![
            "sh".into(),
            "-c".into(),
            "trap '' TERM; echo ready > ready; while :; do sleep 0.05; done".into(),
        ];
        let Response::Agent { agent } = daemon.handle(Request::Run { spec: command }).await else {
            panic!("launch failed");
        };
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !dir.path().join("ready").exists() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        claim(&daemon, "owned", "task:shutdown").await;
        lock(&daemon.state)
            .registry
            .get_mut(&agent.id)
            .unwrap()
            .process_started_at = None;
        let response = daemon.stop("owned", false);
        let held_while_stopping = list_leases(&daemon).await.len();
        daemon.stop_all().await;
        assert!(
            matches!(response, Response::Agent { agent } if agent.status == AgentStatus::Stopping)
        );
        assert_eq!(held_while_stopping, 1);
        assert!(!supervisor::group_exists(agent.pid.unwrap()));
        assert!(list_leases(&daemon).await.is_empty());
        assert!(!daemon.is_live(&agent.id));
    }

    #[tokio::test]
    async fn claim_expiration_removes_storage_and_emits_once() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let owner = register(&daemon, "owner", None).await;
        register(&daemon, "next", None).await;
        let now = Utc::now();
        let expired = Lease {
            id: LeaseId::from("expired"),
            resource: ResourceKey::new("task:expire"),
            holder: owner.id,
            mode: LeaseMode::Exclusive,
            acquired_at: now - Duration::seconds(5),
            change_seq: None,
            expires_at: now - Duration::seconds(1),
            note: None,
        };
        {
            let mut state = lock(&daemon.state);
            state.store.upsert_lease(&expired).unwrap();
            state.leases.restore(expired);
        }
        let mut events = daemon.subscribe_events();
        assert!(matches!(
            claim(&daemon, "next", "task:expire").await,
            Response::Lease { .. }
        ));
        daemon.expire_leases();
        let mut count = 0;
        while let Ok(e) = events.try_recv() {
            if matches!(e.kind, EventKind::LeaseExpired { .. }) {
                count += 1;
            }
        }
        assert_eq!(count, 1);
        assert!(
            lock(&daemon.state)
                .store
                .load_leases()
                .unwrap()
                .iter()
                .all(|l| l.id.as_str() != "expired")
        );
    }

    #[tokio::test]
    async fn vcs_observations_cannot_rewind_and_survive_restart() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let owner = register(&daemon, "owner", None).await;
        let now = Utc::now();
        let newest = VcsState {
            branch: Some("new".into()),
            head: None,
            dirty: None,
            updated_at: now,
        };
        daemon.apply_vcs(&owner.id, newest.clone());
        daemon.apply_vcs(
            &owner.id,
            VcsState {
                branch: Some("old".into()),
                updated_at: now - Duration::seconds(1),
                ..newest.clone()
            },
        );
        drop(daemon);
        let daemon = open(&dir);
        assert_eq!(
            lock(&daemon.state).registry.get(&owner.id).unwrap().vcs,
            Some(newest)
        );
    }

    #[test]
    fn concurrent_events_publish_in_persisted_sequence_order() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let mut events = daemon.subscribe_events();
        std::thread::scope(|scope| {
            for _ in 0..8 {
                let daemon = &daemon;
                scope.spawn(move || {
                    for _ in 0..25 {
                        daemon.emit(EventKind::DaemonStopping {
                            reason: "test".into(),
                        });
                    }
                });
            }
        });
        let mut received = Vec::new();
        while let Ok(event) = events.try_recv() {
            received.push(event.seq);
        }
        assert_eq!(received, (1..=200).collect::<Vec<_>>());
        assert_eq!(
            daemon
                .recent_events(200)
                .iter()
                .map(|e| e.seq)
                .collect::<Vec<_>>(),
            received
        );
    }

    async fn ledger(daemon: &Arc<Daemon>, project: &Path, path: Option<&str>) -> Vec<Change> {
        match daemon
            .handle(Request::Changes {
                project: project.to_string_lossy().into_owned(),
                since_seq: None,
                path: path.map(str::to_owned),
                agent: None,
                limit: 50,
            })
            .await
        {
            Response::Changes { changes } => changes,
            other => panic!("unexpected {other:?}"),
        }
    }

    #[tokio::test]
    async fn watcher_attribution_uses_physical_aliases_and_root_queries_include_all_paths() {
        let dir = TempDir::new().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(repo.join("Agentfile.toml"), "").unwrap();
        let target = repo.join("src/lib.rs");
        std::fs::write(&target, "before").unwrap();
        std::os::unix::fs::symlink("src/lib.rs", repo.join("alias.rs")).unwrap();
        std::os::unix::fs::symlink("src", repo.join("alias-dir")).unwrap();
        let daemon = open(&dir);
        let agent = register_in(&daemon, "writer", &repo).await;
        register_in(&daemon, "reader", &repo).await;
        assert!(matches!(
            daemon
                .handle(Request::Observe {
                    agent: "reader".into(),
                    paths: vec!["src/lib.rs".into()],
                })
                .await,
            Response::Reads { .. }
        ));
        let Response::Lease { lease } = claim(
            &daemon,
            "writer",
            &format!("path:{}", repo.join("alias.rs").display()),
        )
        .await
        else {
            panic!("claim failed");
        };
        let checkout = daemon.watch_targets().pop().unwrap();
        std::fs::write(&target, "after").unwrap();
        daemon
            .record_fs_changes(
                ["alias.rs", "alias-dir/lib.rs", "notes.md"]
                    .into_iter()
                    .map(|path| Observed {
                        checkout: checkout.clone(),
                        path: path.into(),
                        kind: ChangeKind::Modified,
                    })
                    .collect(),
                vec![],
            )
            .await;
        let attributed = Attribution::Agent {
            agent: agent.id,
            lease: lease.id,
            note: None,
        };
        let entries = ledger(&daemon, &repo, None).await;
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].by, attributed);
        assert_eq!(entries[1].by, attributed);
        assert_eq!(entries[2].by, Attribution::External);
        let Response::Messages { messages } = daemon
            .handle(Request::Inbox {
                agent: "reader".into(),
                drain: true,
            })
            .await
        else {
            panic!("inbox failed");
        };
        assert_eq!(messages.len(), 2, "both alias changes warn the reader");
        for message in messages {
            assert_eq!(message.kind, "stale");
            assert_eq!(
                message.payload["paths"],
                json!([project::canonical(&target)])
            );
        }
        let root = repo.to_string_lossy();
        for filter in ["", ".", "./", root.as_ref()] {
            assert_eq!(ledger(&daemon, &repo, Some(filter)).await, entries);
        }
        assert_eq!(ledger(&daemon, &repo, Some("alias-dir")).await.len(), 1);

        // A deleted regular file still has its normalized physical key. A
        // removed symlink no longer supplies evidence of its former target.
        std::fs::remove_file(&target).unwrap();
        std::fs::remove_file(repo.join("alias.rs")).unwrap();
        daemon
            .record_fs_changes(
                ["alias-dir/lib.rs", "alias.rs"]
                    .into_iter()
                    .map(|path| Observed {
                        checkout: checkout.clone(),
                        path: path.into(),
                        kind: ChangeKind::Removed,
                    })
                    .collect(),
                vec![],
            )
            .await;
        let entries = ledger(&daemon, &repo, None).await;
        assert_eq!(entries.len(), 5);
        assert_eq!(entries[3].by, attributed);
        assert_eq!(entries[4].by, Attribution::External);
    }

    /// Poll until `check` passes or five seconds elapse.
    async fn eventually<T>(mut check: impl AsyncFnMut() -> Option<T>) -> T {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Some(value) = check().await {
                return value;
            }
            assert!(tokio::time::Instant::now() < deadline, "timed out");
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }

    #[tokio::test]
    async fn registration_attaches_the_watcher_before_replying() {
        if !have_git() {
            return;
        }
        let dir = TempDir::new().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join("src")).unwrap();
        assert!(git(dir.path(), &repo, &["init", "-q"]));
        assert!(git(
            dir.path(),
            &repo,
            &["commit", "-q", "--allow-empty", "-m", "root"]
        ));
        let daemon = open(&dir);
        // A reconcile tick that never comes during the test: only the
        // registration's own nudge can attach the watch. The watcher is
        // spawned and the agent registered at once, with no time for the
        // spawned task to have run: startup is waited for, not skipped.
        daemon.expect_watcher();
        tokio::spawn(crate::watcher::run(
            daemon.clone(),
            std::time::Duration::from_secs(60),
            std::time::Duration::from_millis(50),
        ));
        register_in(&daemon, "a", &repo).await;
        let lib = repo.join("src/lib.rs");
        std::fs::write(&lib, "fn a() {}\n").unwrap();
        let seen = eventually(async || {
            ledger(&daemon, &repo, None)
                .await
                .into_iter()
                .find(|c| c.path == Path::new("src/lib.rs"))
        })
        .await;
        assert!(
            seen.seq > 0,
            "an edit right after registration is in the ledger"
        );
        let kinds: Vec<EventKind> = daemon
            .recent_events(50)
            .into_iter()
            .map(|e| e.kind)
            .collect();
        let starting = kinds
            .iter()
            .position(|k| matches!(k, EventKind::WatcherStarting))
            .expect("starting announced");
        let started = kinds
            .iter()
            .position(|k| matches!(k, EventKind::WatcherStarted))
            .expect("started announced");
        assert!(starting < started);
    }

    #[tokio::test]
    async fn watcher_records_attributed_changes_and_refreshes_branches() {
        if !have_git() {
            return;
        }
        let dir = TempDir::new().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(repo.join(".gitignore"), "target/\n").unwrap();
        assert!(git(dir.path(), &repo, &["init", "-q"]));
        assert!(git(dir.path(), &repo, &["add", "."]));
        assert!(git(dir.path(), &repo, &["commit", "-q", "-m", "root"]));
        let daemon = open(&dir);
        let a = register_in(&daemon, "a", &repo).await;
        tokio::spawn(crate::watcher::run(
            daemon.clone(),
            std::time::Duration::from_millis(50),
            std::time::Duration::from_millis(50),
        ));
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;

        // `a` holds src/lib.rs; nobody holds notes.md; target/ is ignored.
        let lib = repo.join("src/lib.rs");
        let Response::Lease { lease } =
            claim(&daemon, "a", &format!("path:{}", lib.display())).await
        else {
            panic!("claim failed");
        };
        std::fs::write(&lib, "fn a() {}\n").unwrap();
        std::fs::write(repo.join("notes.md"), "hi\n").unwrap();
        std::fs::create_dir_all(repo.join("target")).unwrap();
        std::fs::write(repo.join("target/out.bin"), "x").unwrap();

        let (mine, theirs) = eventually(async || {
            let entries = ledger(&daemon, &repo, None).await;
            let mine = entries
                .iter()
                .find(|c| c.path == Path::new("src/lib.rs"))?
                .clone();
            let theirs = entries
                .iter()
                .find(|c| c.path == Path::new("notes.md"))?
                .clone();
            Some((mine, theirs))
        })
        .await;
        assert_eq!(
            mine.by,
            Attribution::Agent {
                agent: a.id.clone(),
                lease: lease.id.clone(),
                note: None,
            }
        );
        assert!(mine.seq > 0);
        assert!(mine.head.is_some(), "head recorded");
        assert_eq!(theirs.by, Attribution::External);
        // Queries narrow by path (absolute, made relative by the daemon).
        let by_path = ledger(&daemon, &repo, Some(&lib.to_string_lossy())).await;
        assert!(!by_path.is_empty() && by_path.iter().all(|c| c.path == Path::new("src/lib.rs")));
        std::fs::write(repo.join("watcher-marker"), "processed").unwrap();
        eventually(async || {
            ledger(&daemon, &repo, None)
                .await
                .iter()
                .any(|c| c.path == Path::new("watcher-marker"))
                .then_some(())
        })
        .await;
        assert!(
            ledger(&daemon, &repo, None)
                .await
                .iter()
                .all(|c| !c.path.starts_with("target")),
            "ignored paths never reach the ledger"
        );

        // A branch switch reaches the record through the watcher, not a poll.
        assert!(git(dir.path(), &repo, &["checkout", "-q", "-b", "feature"]));
        eventually(async || {
            match daemon
                .handle(Request::Inspect {
                    agent: "a".to_owned(),
                })
                .await
            {
                Response::Agent { agent }
                    if agent.vcs.as_ref().and_then(|v| v.branch.as_deref()) == Some("feature") =>
                {
                    Some(())
                }
                _ => None,
            }
        })
        .await;
    }

    async fn journal_of(
        daemon: &Arc<Daemon>,
        project: &Path,
        kind: Option<&str>,
        path: Option<&str>,
        grep: Option<&str>,
    ) -> Vec<JournalEntry> {
        match daemon
            .handle(Request::Journal {
                project: project.to_string_lossy().into_owned(),
                since_seq: None,
                until_seq: None,
                agent: None,
                branch: None,
                kind: kind.map(str::to_owned),
                path: path.map(str::to_owned),
                grep: grep.map(str::to_owned),
                limit: 50,
                digest: None,
            })
            .await
        {
            Response::Journal { entries, .. } => entries,
            other => panic!("unexpected {other:?}"),
        }
    }

    #[tokio::test]
    async fn releases_notes_joins_and_leaves_are_journaled() {
        if !have_git() {
            return;
        }
        let dir = TempDir::new().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(repo.join(".gitignore"), "target/\n").unwrap();
        assert!(git(dir.path(), &repo, &["init", "-q"]));
        assert!(git(dir.path(), &repo, &["add", "."]));
        assert!(git(dir.path(), &repo, &["commit", "-q", "-m", "root"]));
        let daemon = open(&dir);
        let a = register_in(&daemon, "a", &repo).await;
        tokio::spawn(crate::watcher::run(
            daemon.clone(),
            std::time::Duration::from_millis(50),
            std::time::Duration::from_millis(50),
        ));
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;

        // Joining is the first entry.
        let entries = journal_of(&daemon, &repo, None, None, None).await;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].kind, JournalKind::Join);
        assert_eq!(entries[0].seq, 1);
        assert!(
            entries[0].summary.contains("branch main"),
            "{}",
            entries[0].summary
        );

        // Two files edited under leases, released with no summary: one
        // synthesised entry naming both, with the ledger range.
        let lib = repo.join("src/lib.rs");
        let main = repo.join("src/main.rs");
        for path in [&lib, &main] {
            assert!(matches!(
                claim(&daemon, "a", &format!("path:{}", path.display())).await,
                Response::Lease { .. }
            ));
        }
        std::fs::write(&lib, "fn a() {}\n").unwrap();
        std::fs::write(&main, "fn main() {}\n").unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        let Response::Leases { leases } = daemon
            .handle(Request::ReleaseAll {
                agent: "a".to_owned(),
                summary: None,
                summary_source: SummarySource::Explicit,
            })
            .await
        else {
            panic!("release_all failed");
        };
        assert_eq!(leases.len(), 2);
        let entries = journal_of(&daemon, &repo, Some("release"), None, None).await;
        assert_eq!(entries.len(), 1, "{entries:?}");
        let release = &entries[0];
        assert_eq!(release.summary_source, SummarySource::Synthesised);
        assert_eq!(
            release.summary,
            "edited 2 files under src/: lib.rs, main.rs"
        );
        assert_eq!(
            release.paths,
            vec![PathBuf::from("src/lib.rs"), PathBuf::from("src/main.rs")]
        );
        assert_eq!(release.paths_total, 2);
        assert!(release.changes.is_some_and(|(lo, hi)| lo <= hi));
        assert_eq!(release.resources.len(), 2);
        assert_eq!(release.agent, Some(a.id.clone()));
        assert!(release.head_after.is_some());

        // The barrier: a change made right before the release, with no
        // wait for the watcher's debounce, is still in the entry.
        assert!(matches!(
            claim(&daemon, "a", &format!("path:{}", lib.display())).await,
            Response::Lease { .. }
        ));
        std::fs::write(&lib, "fn a() { /* parser */ }\n").unwrap();
        let Response::Leases { .. } = daemon
            .handle(Request::ReleaseAll {
                agent: "a".to_owned(),
                summary: Some("rewrote the parser".to_owned()),
                summary_source: SummarySource::Explicit,
            })
            .await
        else {
            panic!("release_all failed");
        };
        let entries = journal_of(&daemon, &repo, Some("release"), None, None).await;
        assert_eq!(entries.len(), 2);
        let explicit = &entries[1];
        assert_eq!(explicit.summary_source, SummarySource::Explicit);
        assert_eq!(explicit.summary, "rewrote the parser");
        assert_eq!(
            explicit.paths,
            vec![PathBuf::from("src/lib.rs")],
            "barrier flushed the watcher"
        );
        assert!(explicit.seq > release.seq);

        // Nothing changed and nothing said: no entry.
        assert!(matches!(
            claim(&daemon, "a", &format!("path:{}", main.display())).await,
            Response::Lease { .. }
        ));
        daemon
            .handle(Request::ReleaseAll {
                agent: "a".to_owned(),
                summary: None,
                summary_source: SummarySource::Explicit,
            })
            .await;
        assert_eq!(
            journal_of(&daemon, &repo, Some("release"), None, None)
                .await
                .len(),
            2
        );

        // Nothing held, but something said: the words are kept.
        daemon
            .handle(Request::ReleaseAll {
                agent: "a".to_owned(),
                summary: Some("  reviewed the plan, no edits  ".to_owned()),
                summary_source: SummarySource::Explicit,
            })
            .await;
        let entries = journal_of(&daemon, &repo, Some("release"), None, None).await;
        assert_eq!(entries.len(), 3);
        let said = &entries[2];
        assert_eq!(said.summary, "reviewed the plan, no edits");
        assert_eq!(said.summary_source, SummarySource::Explicit);
        assert!(said.paths.is_empty() && said.resources.is_empty());

        // Notes, and the filters.
        let Response::JournalEntry { entry: note } = daemon
            .handle(Request::JournalAdd {
                agent: "a".to_owned(),
                summary: "lexer is next".to_owned(),
            })
            .await
        else {
            panic!("journal_add failed");
        };
        assert_eq!(note.kind, JournalKind::Note);
        assert_eq!(
            journal_of(&daemon, &repo, Some("note"), None, None)
                .await
                .len(),
            1
        );
        let by_path = journal_of(&daemon, &repo, None, Some(&lib.to_string_lossy()), None).await;
        assert_eq!(by_path.len(), 2, "both releases touched src/lib.rs");
        let by_dir = journal_of(&daemon, &repo, None, Some("src"), None).await;
        assert_eq!(by_dir.len(), 2);
        let grep = journal_of(&daemon, &repo, None, None, Some("parser")).await;
        assert_eq!(grep.len(), 1);
        assert_eq!(grep[0].seq, explicit.seq);
        assert!(matches!(
            daemon
                .handle(Request::Journal {
                    project: repo.to_string_lossy().into_owned(),
                    since_seq: None,
                    until_seq: None,
                    agent: None,
                    branch: None,
                    kind: Some("bogus".to_owned()),
                    path: None,
                    grep: None,
                    limit: 50,
                    digest: None,
                })
                .await,
            Response::Error {
                code: ErrorCode::Invalid,
                ..
            }
        ));

        // Leaving is journaled, and the ring survives a prune.
        daemon
            .handle(Request::Deregister {
                agent: "a".to_owned(),
            })
            .await;
        let all = journal_of(&daemon, &repo, None, None, None).await;
        assert_eq!(all.last().map(|e| e.kind), Some(JournalKind::Leave));
        let seqs: Vec<u64> = all.iter().map(|e| e.seq).collect();
        assert_eq!(
            seqs,
            (1..=seqs.len() as u64).collect::<Vec<_>>(),
            "dense per-project seqs"
        );
        let Response::Pruned { removed } = daemon
            .handle(Request::JournalPrune {
                project: repo.to_string_lossy().into_owned(),
                before_seq: 3,
            })
            .await
        else {
            panic!("prune failed");
        };
        assert_eq!(removed, 2);
        assert_eq!(journal_of(&daemon, &repo, None, None, None).await[0].seq, 3);

        // A restart continues the seq and reloads the ring.
        drop(daemon);
        let daemon = open(&dir);
        register_in(&daemon, "b", &repo).await;
        let after = journal_of(&daemon, &repo, None, None, None).await;
        assert_eq!(after.last().map(|e| e.kind), Some(JournalKind::Join));
        assert_eq!(after.last().map(|e| e.seq), Some(seqs.len() as u64 + 1));
    }

    async fn digest_for(
        daemon: &Arc<Daemon>,
        project: &str,
        reader: &str,
        since: Option<u64>,
        budget: DigestBudget,
        advance: bool,
    ) -> agentdocker_core::Digest {
        match daemon
            .handle(Request::Journal {
                project: project.to_owned(),
                since_seq: since,
                until_seq: None,
                agent: None,
                branch: None,
                kind: None,
                path: None,
                grep: None,
                limit: 50,
                digest: Some(DigestRequest {
                    reader: reader.to_owned(),
                    max_entries: budget.max_entries,
                    max_chars: budget.max_chars,
                    all_branches: false,
                    advance,
                }),
            })
            .await
        {
            Response::Digest { digest, .. } => digest,
            other => panic!("unexpected {other:?}"),
        }
    }

    async fn note(daemon: &Arc<Daemon>, agent: &str, text: &str) -> u64 {
        match daemon
            .handle(Request::JournalAdd {
                agent: agent.to_owned(),
                summary: text.to_owned(),
            })
            .await
        {
            Response::JournalEntry { entry } => entry.seq,
            other => panic!("unexpected {other:?}"),
        }
    }

    #[tokio::test]
    async fn digests_follow_cursors_that_are_seeded_by_name() {
        if !have_git() {
            return;
        }
        let dir = TempDir::new().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        assert!(git(dir.path(), &repo, &["init", "-q"]));
        let daemon = open(&dir);
        let repo_ref = repo.to_string_lossy().into_owned();
        register_in(&daemon, "a", &repo).await;
        for i in 0..25 {
            note(&daemon, "a", &format!("note {i}")).await;
        }

        // A newcomer is told the twenty newest entries, not its own join.
        let b = register_in(&daemon, "b", &repo).await;
        let first = digest_for(
            &daemon,
            &repo_ref,
            "b",
            None,
            DigestBudget::SESSION_START,
            true,
        )
        .await;
        assert_eq!(
            (first.shown, first.collapsed, first.other_branches),
            (20, 0, 0),
            "{}",
            first.text
        );
        assert!(
            first
                .text
                .starts_with("Since you last looked (20 entries):\n")
        );
        assert!(first.text.contains("noted: \"note 24\""));
        assert!(!first.text.contains("noted: \"note 4\""), "{}", first.text);
        assert!(
            !first.text.contains("b [") || !first.text.contains("joined"),
            "{}",
            first.text
        );

        // Advanced: nothing new until something happens, then only that.
        let again = digest_for(
            &daemon,
            &repo_ref,
            "b",
            None,
            DigestBudget::SESSION_START,
            true,
        )
        .await;
        assert_eq!(again.text, "");
        assert_eq!(again.head_seq, first.head_seq);
        let seq = note(&daemon, "a", "parser is next").await;
        let prompt = digest_for(&daemon, &repo_ref, "b", None, DigestBudget::PROMPT, true).await;
        assert_eq!((prompt.shown, prompt.head_seq), (1, seq), "{}", prompt.text);
        assert!(prompt.text.contains("parser is next"));

        // An empty project means the reader's own; a since overrides the
        // cursor without losing it.
        let own = digest_for(
            &daemon,
            "",
            "b",
            Some(0),
            DigestBudget::SESSION_START,
            false,
        )
        .await;
        assert_eq!(
            own.collapsed + own.shown,
            27,
            "a's join and notes: {}",
            own.text
        );
        assert_eq!(
            digest_for(&daemon, &repo_ref, "b", None, DigestBudget::PROMPT, true)
                .await
                .text,
            ""
        );

        // The human has a cursor too.
        let user = digest_for(
            &daemon,
            &repo_ref,
            "user",
            None,
            DigestBudget::SESSION_START,
            true,
        )
        .await;
        assert_eq!(user.shown, 20, "{}", user.text);
        note(&daemon, "a", "for the human").await;
        let user = digest_for(
            &daemon,
            &repo_ref,
            "user",
            None,
            DigestBudget::SESSION_START,
            false,
        )
        .await;
        assert_eq!(user.shown, 1);
        assert!(user.text.contains("for the human"));

        // b leaves and comes back under the same name: its cursor carries
        // over, so it is not told what it already saw, nor its own leave.
        daemon
            .handle(Request::Deregister {
                agent: "b".to_owned(),
            })
            .await;
        note(&daemon, "a", "while b was away").await;
        let b2 = register_in(&daemon, "b", &repo).await;
        assert_ne!(b2.id, b.id);
        let resumed = digest_for(
            &daemon,
            &repo_ref,
            "b",
            None,
            DigestBudget::SESSION_START,
            true,
        )
        .await;
        assert_eq!(resumed.shown, 2, "{}", resumed.text);
        assert!(resumed.text.contains("for the human"));
        assert!(resumed.text.contains("while b was away"));
        assert!(!resumed.text.contains("left"), "{}", resumed.text);

        // A stranger's name inherits nothing, and neither does a namesake
        // in another project.
        let other = dir.path().join("other");
        std::fs::create_dir_all(&other).unwrap();
        assert!(git(dir.path(), &other, &["init", "-q"]));
        register_in(&daemon, "c", &other).await;
        note(&daemon, "c", "elsewhere").await;
        daemon
            .handle(Request::Deregister {
                agent: "c".to_owned(),
            })
            .await;
        register_in(&daemon, "c", &repo).await;
        let fresh = digest_for(
            &daemon,
            &repo_ref,
            "c",
            None,
            DigestBudget::SESSION_START,
            false,
        )
        .await;
        assert_eq!(
            fresh.shown, 20,
            "seeded from history, not from c's other-project cursor"
        );

        // A transcript tail describes released leases only: with none
        // held it writes nothing, unlike an explicit summary.
        let before = journal_of(&daemon, &repo, Some("release"), None, None)
            .await
            .len();
        daemon
            .handle(Request::ReleaseAll {
                agent: "a".to_owned(),
                summary: Some("I finished the parser.".to_owned()),
                summary_source: SummarySource::Transcript,
            })
            .await;
        assert_eq!(
            journal_of(&daemon, &repo, Some("release"), None, None)
                .await
                .len(),
            before
        );
        let src = repo.join("src.rs");
        assert!(matches!(
            claim(&daemon, "a", &format!("path:{}", src.display())).await,
            Response::Lease { .. }
        ));
        daemon
            .handle(Request::ReleaseAll {
                agent: "a".to_owned(),
                summary: Some("I finished the parser.".to_owned()),
                summary_source: SummarySource::Transcript,
            })
            .await;
        let releases = journal_of(&daemon, &repo, Some("release"), None, None).await;
        assert_eq!(releases.len(), before + 1);
        assert_eq!(
            releases.last().unwrap().summary_source,
            SummarySource::Transcript
        );
        assert_eq!(releases.last().unwrap().summary, "I finished the parser.");

        // Cursors survive a restart.
        drop(daemon);
        let daemon = open(&dir);
        let after = digest_for(
            &daemon,
            &repo_ref,
            "user",
            None,
            DigestBudget::SESSION_START,
            false,
        )
        .await;
        assert_eq!(after.shown, 6, "{}", after.text);
        assert!(
            !after.text.contains("note 24"),
            "already shown: {}",
            after.text
        );
    }

    #[tokio::test]
    async fn head_moves_are_journaled_once_per_checkout() {
        if !have_git() {
            return;
        }
        let dir = TempDir::new().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        assert!(git(dir.path(), &repo, &["init", "-q"]));
        assert!(git(
            dir.path(),
            &repo,
            &["commit", "-q", "--allow-empty", "-m", "root"]
        ));
        let daemon = open(&dir);
        register_in(&daemon, "a", &repo).await;
        register_in(&daemon, "b", &repo).await;
        daemon.refresh_vcs(None).await;
        assert!(
            journal_of(&daemon, &repo, Some("commit"), None, None)
                .await
                .is_empty()
        );

        assert!(git(
            dir.path(),
            &repo,
            &["commit", "-q", "--allow-empty", "-m", "Add lexer"]
        ));
        daemon.refresh_vcs(None).await;
        daemon.refresh_vcs(None).await;
        let commits = journal_of(&daemon, &repo, Some("commit"), None, None).await;
        assert_eq!(commits.len(), 1, "two agents, one checkout, one entry");
        assert!(
            commits[0].summary.starts_with("committed "),
            "{}",
            commits[0].summary
        );
        assert!(
            commits[0].summary.ends_with(": Add lexer"),
            "{}",
            commits[0].summary
        );
        assert_eq!(
            commits[0].agent, None,
            "shared checkout, nobody holds the branch"
        );
        assert_eq!(commits[0].agent_name, "external");
        assert!(commits[0].head_before.is_some() && commits[0].head_after.is_some());

        assert!(git(dir.path(), &repo, &["checkout", "-q", "-b", "feature"]));
        daemon.refresh_vcs(None).await;
        let commits = journal_of(&daemon, &repo, Some("commit"), None, None).await;
        assert_eq!(commits.len(), 1, "same head, no new entry");
        assert!(git(
            dir.path(),
            &repo,
            &["commit", "-q", "--allow-empty", "-m", "On feature"]
        ));
        daemon.refresh_vcs(None).await;
        let commits = journal_of(&daemon, &repo, Some("commit"), None, None).await;
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[1].branch.as_deref(), Some("feature"));
    }
    #[tokio::test]
    async fn journal_release_event_failures_roll_back_leases_entries_and_publication() {
        for rejected in ["lease_released", "journal_appended"] {
            let dir = TempDir::new().unwrap();
            let checkout = dir.path().join("checkout");
            std::fs::create_dir(&checkout).unwrap();
            let daemon = open(&dir);
            register_in(&daemon, "owner", &checkout).await;
            register_in(&daemon, "competitor", &checkout).await;
            let Response::Lease { lease } = claim(&daemon, "owner", "task:durable").await else {
                panic!()
            };
            let before = daemon.recent_events(100);
            let mut live = daemon.subscribe_events();
            lock(&daemon.state).store.reject_event_for_test(rejected);
            assert!(matches!(
                daemon
                    .handle(Request::ReleaseAll {
                        agent: "owner".into(),
                        summary: Some("release with durable summary".into()),
                        summary_source: SummarySource::Explicit,
                    })
                    .await,
                Response::Error {
                    code: ErrorCode::StorageUnavailable,
                    ..
                }
            ));
            assert!(
                live.try_recv().is_err(),
                "failed transaction must not publish"
            );
            assert!(matches!(
                claim(&daemon, "competitor", "task:durable").await,
                Response::Error {
                    code: ErrorCode::StorageUnavailable,
                    ..
                }
            ));
            drop(daemon);
            let restarted = open(&dir);
            assert_eq!(list_leases(&restarted).await, [lease]);
            assert!(
                journal_of(&restarted, &checkout, Some("release"), None, None)
                    .await
                    .is_empty()
            );
            assert_eq!(restarted.recent_events(100), before);
            assert!(matches!(
                claim(&restarted, "competitor", "task:durable").await,
                Response::Error {
                    code: ErrorCode::Conflict,
                    ..
                }
            ));
        }
    }

    #[tokio::test]
    async fn journal_notes_and_cursors_do_not_survive_failed_replay_events() {
        for rejected in ["journal_appended", "journal_read"] {
            let dir = TempDir::new().unwrap();
            let checkout = dir.path().join("checkout");
            std::fs::create_dir(&checkout).unwrap();
            let daemon = open(&dir);
            let agent = register_in(&daemon, "owner", &checkout).await;
            let project = agent.project.as_ref().unwrap().id();
            let before = journal_of(&daemon, &checkout, None, None, None).await;
            let mut live = daemon.subscribe_events();
            {
                let mut state = lock(&daemon.state);
                state.store.reject_event_for_test(rejected);
                if rejected == "journal_read" {
                    state.move_cursor("user", &project, 10);
                    assert_eq!(state.store.journal_cursor("user", &project).unwrap(), None);
                } else {
                    state.journal_add("owner", "must roll back".into());
                }
                assert!(state.storage_failure().is_some());
            }
            assert!(live.try_recv().is_err());
            drop(daemon);
            let restarted = open(&dir);
            assert_eq!(
                journal_of(&restarted, &checkout, None, None, None).await,
                before
            );
            assert_eq!(
                lock(&restarted.state)
                    .store
                    .journal_cursor("user", &project)
                    .unwrap(),
                None
            );
        }
    }

    #[tokio::test]
    async fn journal_sequence_survives_pruning_every_entry_and_restart() {
        let dir = TempDir::new().unwrap();
        let checkout = dir.path().join("checkout");
        std::fs::create_dir(&checkout).unwrap();
        let daemon = open(&dir);
        let agent = register_in(&daemon, "owner", &checkout).await;
        let project = agent.project.unwrap().id();
        let last = journal_of(&daemon, &checkout, None, None, None)
            .await
            .last()
            .unwrap()
            .seq;
        {
            let mut state = lock(&daemon.state);
            state.move_cursor("user", &project, last);
            state.journal_prune(&project, last + 1);
        }
        drop(daemon);
        let daemon = open(&dir);
        let Response::JournalEntry { entry } = daemon
            .handle(Request::JournalAdd {
                agent: "owner".into(),
                summary: "after prune".into(),
            })
            .await
        else {
            panic!()
        };
        assert!(entry.seq > last);
        assert_eq!(
            lock(&daemon.state)
                .store
                .journal_cursor("user", &project)
                .unwrap(),
            Some(last)
        );
    }

    #[tokio::test]
    async fn release_summary_uses_lease_sequence_when_wall_clock_moves_backwards() {
        let dir = TempDir::new().unwrap();
        let checkout = dir.path().join("checkout");
        std::fs::create_dir(&checkout).unwrap();
        let daemon = open(&dir);
        let agent = register_in(&daemon, "owner", &checkout).await;
        let project = agent.project.unwrap();
        let change = |path: &str, at| Change {
            seq: 0,
            project: project.id(),
            checkout: Some(project.dir().to_path_buf()),
            worktree: None,
            path: path.into(),
            kind: ChangeKind::Modified,
            at,
            by: Attribution::External,
            head: None,
        };
        lock(&daemon.state)
            .store
            .append_change(&change("old", Utc::now() + Duration::hours(1)))
            .unwrap();
        let Response::Lease { lease } =
            claim(&daemon, "owner", &format!("path:{}", checkout.display())).await
        else {
            panic!()
        };
        assert_eq!(lease.change_seq, Some(1));
        let Response::Lease { lease: renewed } =
            claim(&daemon, "owner", &format!("path:{}", checkout.display())).await
        else {
            panic!()
        };
        assert_eq!(lease.change_seq, renewed.change_seq);
        lock(&daemon.state)
            .store
            .append_change(&change("new", lease.acquired_at - Duration::hours(1)))
            .unwrap();
        // The acquisition boundary is durable and survives daemon recovery.
        drop(daemon);
        let daemon = open(&dir);
        assert!(matches!(
            daemon
                .handle(Request::ReleaseAll {
                    agent: "owner".into(),
                    summary: None,
                    summary_source: SummarySource::Explicit,
                })
                .await,
            Response::Leases { .. }
        ));
        let entries = journal_of(&daemon, &checkout, Some("release"), None, None).await;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].paths, [PathBuf::from("new")]);
        assert_eq!(entries[0].changes, Some((2, 2)));
    }
    #[tokio::test]
    async fn task_release_skips_watcher_and_path_release_waits_for_pending_changes() {
        let dir = TempDir::new().unwrap();
        let checkout = dir.path().join("checkout");
        std::fs::create_dir(&checkout).unwrap();
        let daemon = open(&dir);
        let agent = register_in(&daemon, "owner", &checkout).await;
        let project = agent.project.unwrap();
        let (sender, mut receiver) = mpsc::channel(1);
        daemon.set_watcher_flush(sender);
        claim(&daemon, "owner", "task:unrelated").await;
        let release = || Request::ReleaseAll {
            agent: "owner".into(),
            summary: None,
            summary_source: SummarySource::Explicit,
        };
        assert!(matches!(
            tokio::time::timeout(
                std::time::Duration::from_millis(200),
                daemon.handle(release())
            )
            .await
            .unwrap(),
            Response::Leases { .. }
        ));
        assert!(receiver.try_recv().is_err());
        claim(&daemon, "owner", &format!("path:{}", checkout.display())).await;
        let worker = daemon.clone();
        let task = tokio::spawn(async move { worker.handle(release()).await });
        let ack = tokio::time::timeout(std::time::Duration::from_secs(1), receiver.recv())
            .await
            .unwrap()
            .unwrap();
        // Flush processing must be able to acquire state before release builds its summary.
        lock(&daemon.state)
            .store
            .append_change(&Change {
                seq: 0,
                project: project.id(),
                checkout: Some(project.dir().to_path_buf()),
                worktree: None,
                path: "pending.rs".into(),
                kind: ChangeKind::Modified,
                at: Utc::now(),
                by: Attribution::External,
                head: None,
            })
            .unwrap();
        ack.send(()).unwrap();
        assert!(matches!(task.await.unwrap(), Response::Leases { .. }));
        let entries = journal_of(&daemon, &checkout, Some("release"), None, None).await;
        assert_eq!(entries[0].paths, [PathBuf::from("pending.rs")]);
    }

    #[tokio::test]
    async fn a_full_watcher_flush_queue_cannot_block_release_indefinitely() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        register_in(&daemon, "owner", dir.path()).await;
        claim(&daemon, "owner", &format!("path:{}", dir.path().display())).await;
        let (sender, _receiver) = mpsc::channel(1);
        let (ack, _done) = oneshot::channel();
        sender.try_send(ack).unwrap();
        daemon.set_watcher_flush(sender);
        let response = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            daemon.handle(Request::ReleaseAll {
                agent: "owner".into(),
                summary: None,
                summary_source: SummarySource::Explicit,
            }),
        )
        .await
        .expect("queue admission must share the flush timeout");
        assert!(matches!(response, Response::Leases { .. }));
    }
}
