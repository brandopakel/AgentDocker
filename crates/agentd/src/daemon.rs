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
use agentdocker_core::paths;
use agentdocker_core::{
    AgentId, AgentRecord, AgentSpec, AgentStatus, Attribution, Change, ChangeKind, Claimed,
    Destination, DiscoveredProcess, Envelope, ErrorCode, Event, EventKind, JournalEntry,
    JournalKind, Lease, LeaseError, LeaseId, LeaseMode, LeaseTable, MessageId, ProjectId,
    ProjectRef, ProjectSource, Registry, RegistryError, Request, ResourceKey, Response,
    SummarySource, VcsState,
    channel::{Channel, ChannelId},
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

use agentdocker_host::{multiplexer, procinfo, project, vcs};

use crate::store::{ChangesQuery, JournalQuery, Store};
use crate::supervisor;
mod access;
mod channels;
mod containers;
mod contests;
mod handoff;
pub mod humans;
mod images;
mod panes;
pub mod policies;
mod recovery;
mod relay;
pub mod reload;
mod restarts;
mod restore;
mod transport;
mod waiting;
mod working;
mod worktrees;

/// Unacknowledged addressed messages per agent. Live streams do not consume them.
const INBOX_CAPACITY: usize = 1000;
const INBOX_BYTES: usize = 4 * 1024 * 1024;

fn message_bytes(message: &Envelope) -> usize {
    // Refuse admission if a future envelope representation cannot be encoded.
    serde_json::to_vec(message).map_or(usize::MAX, |bytes| bytes.len())
}
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
/// Ledger rows an overlap query reads at most, newest first.
const OVERLAP_SCAN: usize = 50_000;
/// Rows an overlap query reads per hold of the state lock.
const OVERLAP_PAGE: usize = 2_000;

/// Maximum foreground wait while failed-launch supervision stops its owned group.
const SUPERVISION_STOP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

pub struct Daemon {
    pub home: PathBuf,
    pub socket: PathBuf,
    started: Instant,
    state: Mutex<State>,
    shutdown: Notify,
    /// Asks the watcher to flush pending observations now; set once the
    /// watcher runs. Never held across an await.
    watcher_flush: Mutex<Option<mpsc::Sender<oneshot::Sender<()>>>>,
    container_backend: Arc<dyn agentdocker_host::containers::ContainerBackend>,
    container_slots: Arc<tokio::sync::Semaphore>,
    /// Asks the watcher to reconcile its watches now, so a checkout is
    /// covered from the moment its first agent is registered rather than
    /// from the next tick. Starts `Off`; the daemon's main says when a
    /// watcher is coming so registrations wait for it instead.
    watcher_attach: watch::Sender<WatcherLink>,
    /// The restricted container endpoint: where it serves, or why it does
    /// not. Grants need it; the host socket does not.
    restricted: Mutex<RestrictedEndpoint>,
    /// Terminals of managed agents that were given one, so `attach` can
    /// find them. Held here rather than under the state lock: attaching
    /// is I/O and must not block a coordination request.
    sessions: Mutex<HashMap<AgentId, supervisor::Session>>,
    /// One scan at a time: the tick and an on-demand `discover` must not
    /// interleave scans. A flag and notification allow callers to join a
    /// pending scan without holding any lock across async work.
    scanning: std::sync::atomic::AtomicBool,
    scan_finished: Notify,
}

/// Release the scan slot and wake joiners on completion or cancellation.
struct ScanGuard<'a> {
    scanning: &'a std::sync::atomic::AtomicBool,
    finished: &'a Notify,
}

impl Drop for ScanGuard<'_> {
    fn drop(&mut self) {
        self.scanning
            .store(false, std::sync::atomic::Ordering::Release);
        self.finished.notify_waiters();
    }
}

/// The last process scan.
#[derive(Default)]
struct Discovered {
    at: Option<Instant>,
    processes: Vec<DiscoveredProcess>,
    error: Option<String>,
}

/// A `discover` younger than this answers from the last scan.
const DISCOVERY_FRESH: std::time::Duration = std::time::Duration::from_secs(4);

/// The restricted endpoint's state, as `ping` and `grant-access` see it.
#[derive(Clone, Debug)]
pub enum RestrictedEndpoint {
    Starting,
    On(PathBuf),
    Off(String),
}

/// Whether registrations can ask the watcher to cover their checkout.
#[derive(Clone)]
enum WatcherLink {
    /// Explicitly embedded without a watcher (unit tests).
    Off,
    Unavailable(String),
    /// One is being spawned; wait for it rather than skip it.
    Starting,
    On(mpsc::Sender<WatcherAttachment>),
}

pub(crate) struct WatcherAttachment {
    pub checkout: PathBuf,
    pub ack: oneshot::Sender<Result<(), String>>,
}

/// One synchronous transition owns memory, persistence and publication.
/// Host I/O and waits are performed before or after this guard, never across await.
struct State {
    discovered: Discovered,
    store: Store,
    storage_error: Option<String>,
    registry: Registry,
    leases: LeaseTable,
    inboxes: HashMap<AgentId, VecDeque<Envelope>>,
    inbox_bytes: HashMap<AgentId, usize>,
    live_subscribers: HashMap<AgentId, usize>,
    supervised: HashMap<AgentId, tokio::sync::watch::Sender<Option<bool>>>,
    container_busy: HashSet<AgentId>,
    transports: HashMap<AgentId, transport::Transport>,
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
    /// The branch each checkout was last seen on, beside `last_head`, so
    /// a move can be told from a branch switch for a checkout that has
    /// no agent record to remember it.
    last_branch: HashMap<PathBuf, Option<String>>,
    /// Every checkout of each project, by project id: the main one and
    /// its linked worktrees. Refreshed off the lock on the same tick as
    /// the VCS sweep, because enumerating them runs git.
    project_checkouts: HashMap<ProjectId, Vec<PathBuf>>,
    /// Checkouts the daemon is committing in right now. The watcher polls
    /// on its own schedule and will see HEAD move part-way through, so
    /// without this it writes its own guessed-at entry for a commit the
    /// daemon is about to record properly.
    committing: std::collections::BTreeSet<PathBuf>,
    /// Duplicate pairs already announced, so a finding that lasts as
    /// long as a session is said once rather than every sweep.
    reported_duplicates: std::collections::BTreeSet<(AgentId, AgentId)>,
    /// Readers' journal cursors, loaded from the store on first use and
    /// written through when they move.
    journal_cursors: HashMap<(String, ProjectId), u64>,
    /// The rooms agents share, open and closed-but-unpruned, loaded whole
    /// at startup: message routing needs them under the state lock.
    channels: HashMap<ChannelId, Channel>,
    /// Which checkouts have changed each path, so the second one is a
    /// collision the daemon can act on without scanning the ledger.
    contested: HashMap<(ProjectId, PathBuf), HashSet<PathBuf>>,
    /// Questions somebody is blocked on, by message id: an answer names
    /// one and only the question knows who is waiting for it.
    questions: HashMap<MessageId, agentdocker_core::Question>,
    /// Claims waiting for a resource, in arrival order. Connection-scoped
    /// and never persisted: a restart drops every waiting client, which
    /// reconnects and takes a new place.
    waiting: agentdocker_core::WaitQueue,
    /// Where desktop notifications are handed off to be posted. `None`
    /// until the daemon starts its notifier, and in tests.
    notifier: Option<mpsc::Sender<humans::Notice>>,
    /// The machine owner's policy, and one per project that has a file.
    /// Empty means everything is allowed, which is what no file means.
    host_policy: policies::Loaded,
    project_policies: HashMap<PathBuf, policies::Loaded>,
}

struct JournalRing {
    entries: VecDeque<JournalEntry>,
    touched: Instant,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    let started = state_timing_start();
    let guard = mutex.lock().unwrap_or_else(PoisonError::into_inner);
    state_timing_finish("lock_wait", started);
    guard
}

/// Opt-in diagnostics contain durations and static operation names only.
/// The first 256 slow samples bound logging even during a prolonged stall.
fn state_timing_start() -> Option<Instant> {
    tracing::enabled!(target: "agentd_state_timing", tracing::Level::DEBUG).then(Instant::now)
}

fn state_timing_finish(operation: &str, started: Option<Instant>) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static SAMPLES: AtomicUsize = AtomicUsize::new(0);
    let Some(elapsed) = started.map(|start| start.elapsed()) else {
        return;
    };
    if elapsed >= std::time::Duration::from_millis(250)
        && SAMPLES
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
                (count < 256).then_some(count + 1)
            })
            .is_ok()
    {
        tracing::debug!(target: "agentd_state_timing", operation,
            elapsed_ms = elapsed.as_secs_f64() * 1000.0, "slow daemon state operation");
    }
}

/// Whether a name can be the last component of `agent/<name>` as a git
/// branch and of a worktree path: a conservative subset of what git
/// accepts, so the answer never depends on git's own parsing.
fn isolate_name_ok(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && !name.starts_with(['-', '.'])
        && !name.ends_with('.')
        && !name.ends_with(".lock")
        && !name.contains("..")
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
        RegistryError::IdentityReserved(_) => ErrorCode::Conflict,
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

/// Whether two records stand for the same agent.
///
/// One process is one agent, and this is what "one process" means. Each
/// clause is load-bearing.
///
/// The start time goes with the pid because pids are reused: an old
/// record and a new process that happens to land on its number are not
/// the same process, and folding them would hand a stranger somebody
/// else's identity. It must also be *known* — two unreadable start
/// times are not evidence of anything.
///
/// The runtime and the project go with them because sharing a process
/// is not the same as being the same agent: a host that runs several
/// kinds of session in one process would otherwise have them all
/// collapse into whichever registered first. Project rather than
/// workdir, because the two halves disagree about the workdir the
/// moment a session changes directory — the hooks adapter reports where
/// the session is now, the MCP server where it was launched — while
/// both still resolve to the same project.
///
/// And the session, where both sides name one. A provider host can
/// multiplex several sessions into one process, and those are several
/// agents however much the rest agrees. Only one half registers a
/// session id, so an absent one cannot be a mismatch: absent means "the
/// other half of a session I am already part of", while two *different*
/// ids mean two sessions that happen to share a process.
fn same_agent(a: &AgentRecord, b: &AgentRecord) -> bool {
    agentdocker_core::identity::same_registration(a, b)
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
/// A cycle as a sentence: `a → task:x (b) → task:y (a)`.
fn describe(cycle: &[agentdocker_core::Blocked]) -> String {
    cycle
        .iter()
        .map(|step| {
            format!(
                "{} waits for {} held by {}",
                step.agent.short(),
                step.resource,
                step.held_by.short()
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// Sleep until something happens that could change this waiter's answer:
/// an overlapping lease clearing, or a waiter ahead of it leaving the
/// queue. The second matters as much as the first — the head of a queue
/// giving up makes the next one next, and nothing else would say so.
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
                EventKind::LeaseWaitEnded {
                    resource: freed, ..
                } if freed.overlaps(resource) => return true,
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

/// How many checkouts of one project the watcher will take on beyond
/// the ones agents are registered in.
///
/// Generous — a repository with more live worktrees than this is
/// unusual — and finite, because each one costs a recursive filesystem
/// watch and the operating system will not hand out unlimited numbers
/// of those.
const MAX_EXTRA_CHECKOUTS: usize = 32;

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
    /// Report a duplicate record left over from before one process was
    /// one agent. Report, not repair.
    ///
    /// Refusing new duplicates does not help a session that is already
    /// showing twice: both halves share a live pid, so neither is ever
    /// reaped and the pair persists as long as the session does. The
    /// obvious repair is to retire the half that holds no leases and has
    /// an empty inbox — and that is a guess, which review caught before
    /// it shipped.
    ///
    /// An idle transport is not an unused agent. A connected MCP server
    /// holds that id and will use it on its next call; the record may
    /// own channel membership, pending questions, journal cursors and
    /// observations, none of which show up as a lease or a queued
    /// message. Marking it exited strands all of that and silently drops
    /// messages addressed to it afterwards, on the evidence that its
    /// inbox happened to be empty at the moment we looked.
    ///
    /// Repairing it properly means keeping the old id as a durable
    /// resolvable alias and migrating every reference atomically. Until
    /// that exists, this says what it found and leaves both alone: a
    /// duplicate a person can see is better than one silently resolved
    /// the wrong way.
    pub fn duplicates(&self) -> Vec<(AgentId, AgentId)> {
        let state = lock(&self.state);
        // Sorted, because the registry is not ordered and "the earlier
        // one" has to mean the one that registered first rather than
        // whichever the map happened to yield. A report a person acts on
        // should not name a different half each time it is read.
        let mut live: Vec<&AgentRecord> = state.registry.live().collect();
        live.sort_by_key(|a| (a.created_at, a.id.clone()));
        let mut found = Vec::new();
        for (at, agent) in live.iter().enumerate() {
            if let Some(earlier) = live[..at].iter().find(|kept| same_agent(kept, agent)) {
                found.push((earlier.id.clone(), agent.id.clone()));
            }
        }
        found
    }

    /// Say so once per pair, not once per sweep.
    ///
    /// The sweep runs for as long as the daemon does, and a duplicate
    /// persists for the life of the session that has it — so warning on
    /// every pass turns one finding into a log that grows without bound
    /// and buries everything else. Each pair is announced when it
    /// appears and then kept quiet; a pair that goes away and returns is
    /// news again.
    fn report_duplicates(&self) {
        let found = self.duplicates();
        let mut state = lock(&self.state);
        let current: std::collections::BTreeSet<_> = found.iter().cloned().collect();
        for pair in &current {
            if state.reported_duplicates.contains(pair) {
                continue;
            }
            warn!(
                first = %pair.0, second = %pair.1,
                "two live records for one process, from before one process was one agent; \
                 both are left alone — retiring either can strand channel membership, \
                 pending questions or a journal cursor that no lease or inbox would show"
            );
        }
        state.reported_duplicates = current;
    }

    pub fn check_liveness(&self) {
        self.report_duplicates();
        let candidates: Vec<_> = {
            let state = lock(&self.state);
            state
                .registry
                .live()
                .filter(|a| a.container.is_none())
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
    pub async fn stop_all(self: &Arc<Self>) {
        // Before anything stops: stopping releases leases, so what a
        // restorable agent holds has to be written down while it holds it.
        self.save_restore_points();
        let managed: Vec<_> = lock(&self.state)
            .registry
            .live()
            .filter(|a| a.managed)
            .map(|a| a.id.clone())
            .collect();
        for id in managed {
            if self.container_record(&id).is_some() {
                if let Err(error) = self.request_container_stop(&id, false) {
                    warn!(agent = %id, %error, "container shutdown intent could not be persisted");
                }
                continue;
            }
            if let response @ Response::Error { .. } = self.stop(id.as_str(), false) {
                warn!(agent = %id, ?response, "managed agent did not stop during shutdown");
            }
        }
        self.reconcile_containers();
        // Keep supervision alive during shutdown so owned children are reaped
        // and their groups stop before durable protection is released.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(8);
        while {
            let state = lock(&self.state);
            !state.supervised.is_empty() || state.registry.live().any(|a| a.container.is_some())
        } {
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
        let mut state = lock(&self.state);
        state.expire_leases();
        state.expire_questions(Utc::now());
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
        agentdocker_host::dirs::secure_state_dir(&home)?;
        let logs = home.join("logs");
        agentdocker_host::dirs::secure_state_dir(&logs)?;
        for entry in std::fs::read_dir(&logs)? {
            let path = entry?.path();
            if path.extension().is_some_and(|ext| ext == "log") {
                agentdocker_host::dirs::private_file(&path, false, false)?;
            }
        }
        let daemon_log = paths::daemon_log(&home);
        if std::fs::symlink_metadata(&daemon_log).is_ok() {
            agentdocker_host::dirs::private_file(&daemon_log, false, false)?;
        }
        let store = Store::open(&home.join("state.db"))?;
        Self::with_store(home, socket, store)
    }

    pub fn with_store(home: PathBuf, socket: PathBuf, store: Store) -> anyhow::Result<Self> {
        let now = Utc::now();
        let records = store.load_agents()?;
        // A shared name does not prove a shared identity. Refuse before any
        // recovery writes: retiring either record can strand its inbox and
        // release protection still held by a running process. Reconciliation
        // needs a durable alias migration, not a choice based on load order.
        let mut live_names = HashMap::new();
        for record in records.iter().filter(|record| record.status.is_live()) {
            if let Some(previous) = live_names.insert(&record.spec.name, &record.id) {
                anyhow::bail!(
                    "duplicate live agent name {} in stored records {previous} and {}; \
                     refusing recovery to preserve both identities and their protection",
                    record.spec.name,
                    record.id,
                );
            }
        }
        let mut registry = Registry::new();
        // Validate every durable route before recovery can write any events,
        // statuses, leases or questions. A malformed alias is never ignored.
        for record in &records {
            registry.insert(record.clone())?;
        }
        registry.restore_aliases(&store.identity_aliases()?)?;
        let mut next_seq = store.max_event_seq()? + 1;
        for mut record in records {
            if record.managed
                && record.container.is_none()
                && record.status == AgentStatus::Created
                && !(record.spec.restore
                    && store
                        .document::<restore::RestorePoint>("restore_point", record.id.as_str())?
                        .is_some())
            {
                // The previous daemon stopped between creating the record and
                // spawning the process, so nothing is running for it.
                warn!(agent = %record.id.short(), name = %record.spec.name, "agent never started; recording failure");
                record.status = AgentStatus::Failed {
                    reason: "daemon restarted before the process was spawned".to_owned(),
                };
                record.finished_at = Some(now);
                let mut event = Event::new(
                    EventKind::AgentExited {
                        agent: record.id.clone(),
                        status: record.status.clone(),
                    },
                    now,
                );
                event.seq = next_seq;
                store.agent_transition(&record, &event)?;
                next_seq += 1;
            }
            let stored = registry.get_mut(&record.id).expect("record was validated");
            *stored = record;
        }
        let channels: HashMap<ChannelId, Channel> = store
            .documents::<Channel>("channel", None)
            .unwrap_or_default()
            .into_iter()
            .map(|channel| (channel.id.clone(), channel))
            .collect();
        let mut leases = LeaseTable::new();
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
        let inbox_bytes = inboxes
            .iter()
            .map(|(agent, messages)| {
                (
                    agent.clone(),
                    messages.iter().fold(0usize, |size, message| {
                        size.saturating_add(message_bytes(message))
                    }),
                )
            })
            .collect();
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

        let mut questions: HashMap<_, _> = store
            .documents::<agentdocker_core::Question>("question", None)?
            .into_iter()
            .map(|question| (question.id.clone(), question))
            .collect();
        let mut expired: Vec<_> = questions
            .values()
            .filter(|question| question.expired(now))
            .map(|question| question.id.clone())
            .collect();
        expired.sort();
        let expiration_events: Vec<_> = expired
            .iter()
            .enumerate()
            .map(|(index, question)| {
                let mut event = Event::new(
                    EventKind::QuestionClosed {
                        question: question.clone(),
                        answer: None,
                    },
                    now,
                );
                event.seq = next_seq + index as u64;
                event
            })
            .collect();
        store.close_questions(&expired, &expiration_events)?;
        next_seq += expiration_events.len() as u64;
        for id in expired {
            questions.remove(&id);
        }
        anyhow::ensure!(
            questions.len() <= humans::MAX_QUESTIONS,
            "too many stored pending questions"
        );

        let (bus, _) = broadcast::channel(1024);
        let (events, _) = broadcast::channel(1024);
        Ok(Self {
            home,
            socket,
            started: Instant::now(),
            state: Mutex::new(State {
                discovered: Discovered::default(),
                store,
                storage_error: None,
                registry,
                leases,
                inboxes,
                inbox_bytes,
                live_subscribers: HashMap::new(),
                supervised: HashMap::new(),
                container_busy: HashSet::new(),
                transports: HashMap::new(),
                projects,
                next_seq,
                bus,
                events,
                journal_seq: HashMap::new(),
                journal_rings: HashMap::new(),
                last_head: HashMap::new(),
                last_branch: HashMap::new(),
                project_checkouts: HashMap::new(),
                committing: std::collections::BTreeSet::new(),
                reported_duplicates: std::collections::BTreeSet::new(),
                journal_cursors: HashMap::new(),
                channels,
                contested: HashMap::new(),
                questions,
                waiting: agentdocker_core::WaitQueue::new(),
                notifier: None,
                host_policy: policies::Loaded::default(),
                project_policies: HashMap::new(),
            }),
            shutdown: Notify::new(),
            watcher_flush: Mutex::new(None),
            scanning: std::sync::atomic::AtomicBool::new(false),
            scan_finished: Notify::new(),
            container_backend: Arc::new(agentdocker_host::containers::CliContainers),
            container_slots: Arc::new(tokio::sync::Semaphore::new(8)),
            watcher_attach: watch::channel(WatcherLink::Off).0,
            restricted: Mutex::new(RestrictedEndpoint::Starting),
            sessions: Mutex::new(HashMap::new()),
        })
    }

    /// Start posting desktop notifications for messages that reach a
    /// person. Separate from `open` so tests, which have no desktop and
    /// want no side effects, simply never call it.
    pub fn notify_desktop(self: &Arc<Self>) {
        if std::env::var_os("AGENTDOCKER_NO_NOTIFICATIONS")
            .is_some_and(|value| !value.is_empty() && value != "0")
        {
            return;
        }
        let (tx, rx) = mpsc::channel(64);
        lock(&self.state).notifier = Some(tx);
        tokio::spawn(humans::notifier(rx, self.home.clone(), self.socket.clone()));
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
        // Boxed: `handle_healthy` is one match over every request the
        // protocol has, so the future it returns is as large as the
        // biggest arm plus everything the match holds live across an
        // await. Left inline it lands on the caller's stack, and every
        // caller that awaits a `handle` inside its own async fn pays for
        // it again — which overflows a thread stack once the protocol is
        // big enough. On the heap it costs one allocation per request.
        let response = Box::pin(self.handle_healthy(request)).await;
        lock(&self.state).storage_failure().unwrap_or(response)
    }

    async fn handle_healthy(self: &Arc<Self>, request: Request) -> Response {
        match request {
            Request::BuildImage { spec } => self.build_image(spec).await,
            Request::Images => self.images(),
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
            Request::Handoff {
                agent,
                to,
                task,
                note,
                transfer_leases,
                key,
            } => {
                self.handoff(&agent, to.as_deref(), task, note, transfer_leases, key)
                    .await
            }
            Request::Handoffs { agent } => self.handoffs(agent.as_deref()),
            Request::Import { agent, bundle } => self.import(&agent, *bundle).await,
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
            Request::Commit {
                agent,
                message,
                all,
                push,
            } => self.commit(&agent, message, all, push).await,
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
                restricted: match self.restricted() {
                    RestrictedEndpoint::On(socket) => Some(socket),
                    _ => None,
                },
            },
            Request::Run { spec } => self.run(spec).await,
            Request::RunContainer {
                spec,
                build,
                options,
            } => {
                if spec.in_pane {
                    // A container has its own lifecycle and its own
                    // terminal; a tmux pane would own neither.
                    Response::error(
                        ErrorCode::Invalid,
                        "in_pane is for a process on this host; a container is started by the \
                         engine, so there is nothing for tmux to own",
                    )
                } else {
                    self.run_container(spec, build, options).await
                }
            }
            Request::RestartContainer { agent } => self.restart_container(&agent).await,
            Request::Register { spec, pid, session } => self.register(spec, pid, session).await,
            Request::Deregister { agent } => lock(&self.state).deregister(&agent),
            Request::Discover => self.discover().await,
            Request::Runtimes => self.runtimes().await,
            Request::Adopt { pid, name, runtime } => self.adopt(pid, name, runtime).await,
            Request::Stop { agent, force } => self.stop_agent(&agent, force).await,
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
            Request::ReportActivity { agent, observation } => {
                lock(&self.state).report_activity(&agent, observation, Utc::now())
            }
            Request::Changes {
                project,
                since_seq,
                path,
                agent,
                limit,
            } => self.changes(&project, since_seq, path, agent, limit).await,
            Request::Overlap {
                project,
                since_seq,
                agent,
            } => self.overlap(&project, since_seq, agent).await,
            Request::Shutdown => {
                info!("shutdown requested by a client");
                self.shutdown.notify_one();
                Response::Ok
            }
            Request::Reload => self.hand_over().await,
            Request::Send {
                from,
                to,
                kind,
                payload,
                reply_to,
            } => self.send(from, &to, kind, payload, reply_to).await,
            Request::Me { workdir } => self.me(workdir).await,
            Request::Ask {
                from,
                to,
                question,
                timeout_secs,
            } => self.ask(from, to, question, timeout_secs).await,
            Request::Answer {
                from,
                message,
                text,
            } => self.answer(from, message, text).await,
            Request::Questions { agent } => self.questions(agent),
            Request::Activity {
                agent,
                project,
                all,
            } => self.activity(agent, project, all).await,
            Request::Waiting => self.waiting(),
            Request::ContestOpen {
                agent,
                project,
                task,
                metric,
                entrants,
                channel,
            } => self.contest_open(&agent, project, task, metric, entrants, channel),
            Request::ContestEnter { agent, contest } => self.contest_enter(&agent, &contest),
            Request::ContestSubmit {
                agent,
                contest,
                validation,
                score,
            } => self.contest_submit(&agent, &contest, &validation, score),
            Request::Contests {
                contest,
                project,
                agent,
                all,
            } => self.contests(contest, project, agent, all),
            Request::ContestClose {
                agent,
                contest,
                winner,
                resolution,
            } => self.contest_close(&agent, &contest, winner, resolution),
            Request::Inbox { agent, drain } => lock(&self.state).inbox(&agent, drain),
            Request::AckInbox { agent, messages } => lock(&self.state).ack_inbox(&agent, &messages),
            Request::Claim {
                agent,
                resource,
                mode,
                amount,
                ttl_secs,
                note,
                wait_secs,
            } => {
                self.claim(&agent, resource, mode, amount, ttl_secs, note, wait_secs)
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
            Request::Channels {
                project,
                all,
                agent,
            } => self.channels(&project, all, agent).await,
            Request::ChannelOpen {
                agent,
                task,
                members,
            } => self.channel_open(&agent, task, members),
            Request::ChannelClose {
                agent,
                channel,
                resolution,
            } => self.channel_close(&agent, &channel, resolution),
            Request::ChannelPrune {
                project,
                before_secs,
            } => self.channel_prune(&project, before_secs).await,
            Request::ReviewRequest {
                agent,
                channel,
                note,
            } => self.review_request(&agent, &channel, note),
            Request::Review {
                agent,
                channel,
                of,
                verdict,
                note,
            } => self.review(&agent, &channel, of, &verdict, note),
            Request::JournalPrune {
                project,
                before_seq,
            } => match self.resolve_project(&project).await {
                Ok(id) => lock(&self.state).journal_prune(&id, before_seq),
                Err(response) => *response,
            },
            Request::Leases { agent, resource } => self.leases(agent.as_deref(), resource).await,
            Request::Subscribe { .. }
            | Request::Events { .. }
            | Request::Logs { .. }
            | Request::Attach { .. }
            | Request::AttachInput { .. }
            | Request::AttachResize { .. } => {
                Response::error(ErrorCode::Internal, "streaming request routed as unary")
            }
        }
    }

    async fn run(self: &Arc<Self>, spec: AgentSpec) -> Response {
        // A pane is somebody else's to own, so that path registers what
        // tmux starts rather than supervising a child of ours.
        if spec.in_pane {
            return self.run_in_pane(spec).await;
        }
        if spec.command.first().is_none_or(String::is_empty) {
            return Response::error(ErrorCode::Invalid, "run needs a nonempty command");
        }
        let mut record = AgentRecord::new(spec, true, Utc::now());
        if let Err(response) = self.admit_run(&mut record).await {
            return *response;
        }
        if record.spec.isolate {
            match self.isolate(&record).await {
                Ok(path) => {
                    record.spec.workdir = Some(path.clone());
                }
                Err(response) => return *response,
            }
        }
        let project = self.project_for(record.spec.workdir.clone(), true).await;
        let vcs = Self::vcs_for(record.spec.workdir.clone()).await;
        record.project = project;
        record.vcs = vcs;
        let inserted = lock(&self.state).insert_record(record.clone());
        let record = match inserted {
            Response::Agent { agent } => agent,
            other => {
                self.cleanup_isolate(&record).await;
                return other;
            }
        };
        // Watched before the process exists, so its first edit is seen.
        if watchable(&record) {
            if let Err(reason) = self.ensure_watched(&record).await {
                self.mark_exited(
                    &record.id,
                    AgentStatus::Failed {
                        reason: reason.clone(),
                    },
                );
                self.cleanup_isolate(&record).await;
                return Response::error(ErrorCode::Unavailable, reason);
            }
        }
        match supervisor::spawn(self, &record).await {
            Ok(mut spawned) => {
                let pid = spawned.pid;
                let process_started_at = Some(spawned.process_started_at);
                if let Some(session) = spawned.session.clone() {
                    lock(&self.sessions).insert(record.id.clone(), session);
                }
                let mut admission_error = None;
                let updated = {
                    let mut state = lock(&self.state);
                    state
                        .supervised
                        .insert(record.id.clone(), spawned.control.clone());
                    let candidate = state
                        .registry
                        .get(&record.id)
                        .cloned()
                        .filter(|current| current.status == AgentStatus::Created);
                    if let Some(error) = state.run_refusal(&record) {
                        admission_error = Some(error);
                        None
                    } else if let Some(mut running) = candidate {
                        let now = Utc::now();
                        running.pid = Some(pid);
                        running.process_started_at = process_started_at;
                        running.process_group = Some(pid);
                        running.status = AgentStatus::Running;
                        running.started_at = Some(now);
                        running.last_seen = now;
                        let mut event = Event::new(
                            EventKind::AgentStarted {
                                agent: record.id.clone(),
                                pid: Some(pid),
                            },
                            now,
                        );
                        event.seq = state.next_seq;
                        state.persist("launch completion", |store| {
                            store.agent_transition(&running, &event)
                        });
                        if state.storage_error.is_none() {
                            *state
                                .registry
                                .get_mut(&record.id)
                                .expect("launch identity retained") = running.clone();
                            state.next_seq += 1;
                            let _ = state.events.send(event);
                            Some(running)
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                };
                let failed = lock(&self.state).storage_failure().or(admission_error);
                let activation_error = if failed.is_none() && updated.is_some() {
                    spawned.activate("launched").await.err()
                } else {
                    None
                };
                if failed.is_some() || updated.is_none() || activation_error.is_some() {
                    spawned.control.send_replace(Some(true));
                }
                let supervision = supervisor::supervise(self.clone(), record.id, spawned);
                if let Some(error) = failed.or_else(|| {
                    activation_error.map(|error| {
                        Response::error(
                            ErrorCode::Internal,
                            format!("failed to execute command: {error:#}"),
                        )
                    })
                }) {
                    let _ = tokio::time::timeout(SUPERVISION_STOP_TIMEOUT, supervision).await;
                    return error;
                }
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
                self.cleanup_isolate(&record).await;
                err.downcast_ref::<policies::LaunchDenied>()
                    .map(|denied| denied.0.clone())
                    .unwrap_or_else(|| Response::error(ErrorCode::Internal, format!("{err:#}")))
            }
        }
    }

    async fn register(
        &self,
        spec: AgentSpec,
        pid: Option<u32>,
        reported: Option<agentdocker_core::multiplexer::Session>,
    ) -> Response {
        if pid.is_some_and(|pid| signal_pid(pid).is_none()) {
            return Response::error(
                ErrorCode::Invalid,
                "pid must be a positive process id within i32 range",
            );
        }
        // Resolved here, off the state thread and before the record
        // exists: identity compares the physical checkout, and `/tmp`
        // and `/private/tmp` are one directory spelled two ways. Two
        // halves of a session that spell it differently are still one
        // session, and two worktrees that resolve apart are still two.
        // Touching the filesystem is not something the comparison may
        // do — it runs under the state mutex — so it happens once, now.
        let mut spec = spec;
        if let Some(workdir) = spec.workdir.take() {
            // Identity needs an existing directory. The project helper
            // permits nonexistent suffixes, which is useful for planned
            // paths but cannot establish a registration's checkout.
            let given = workdir.clone();
            let resolved = match tokio::task::spawn_blocking(move || {
                let path = std::fs::canonicalize(&workdir)?;
                if !path.is_dir() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "working directory is not a directory",
                    ));
                }
                Ok(path)
            })
            .await
            {
                Ok(result) => result.ok(),
                Err(_) => {
                    return Response::error(
                        ErrorCode::Internal,
                        "the working-directory resolver failed",
                    );
                }
            };
            let Some(resolved) = resolved else {
                // Neither retain an unverified path nor turn a supplied
                // directory into None: both would lose identity evidence.
                return Response::error(
                    ErrorCode::Invalid,
                    format!(
                        "cannot resolve the working directory {}; register from a directory \
                         that exists, or with none at all",
                        given.display()
                    ),
                );
            };
            spec.workdir = Some(resolved);
        }
        let project = self.project_for(spec.workdir.clone(), true).await;
        let vcs = Self::vcs_for(spec.workdir.clone()).await;
        let mut record = AgentRecord::new(spec, false, Utc::now());
        record.project = project;
        record.vcs = vcs;
        self.refresh_policy_for(record.project.as_ref());
        record.pid = pid;
        record.process_started_at = pid.and_then(procinfo::start_time);
        record.status = AgentStatus::Running;
        record.started_at = Some(Utc::now());
        // An agent that registered itself was started by somebody, and
        // that somebody may have been a multiplexer. Knowing which lets
        // a person reach it with the tool that already owns its
        // terminal.
        record.session = pid.and_then(|pid| Self::session_of(pid, reported.clone()));
        let response = lock(&self.state).insert_record(record);
        // The reply is what a session waits for before its first edit, so
        // the checkout is watched by the time it goes out.
        if let Response::Agent { agent } = &response {
            if watchable(agent) {
                if let Err(reason) = self.ensure_watched(agent).await {
                    // An externally started process may already be writing. Keep its
                    // identity and protection, but do not report coverage as successful.
                    return Response::error(
                        ErrorCode::Unavailable,
                        format!(
                            "agent {} registered but checkout coverage is unavailable: {reason}",
                            agent.id
                        ),
                    );
                }
            }
        }
        response
    }

    /// Where a process lives, when that is somebody else's terminal.
    /// Best-effort: reading another process's environment is refused on
    /// macOS, so a `None` here means "we could not tell", never "it is
    /// homeless".
    fn session_of(
        pid: u32,
        reported: Option<agentdocker_core::multiplexer::Session>,
    ) -> Option<agentdocker_core::multiplexer::Session> {
        let by_pid: BTreeMap<u32, procinfo::Process> = procinfo::processes()
            .map(|table| table.into_iter().map(|p| (p.pid, p)).collect())
            .unwrap_or_default();
        multiplexer::of(pid, &by_pid, reported)
    }

    async fn cleanup_isolate(&self, record: &AgentRecord) {
        if !record.spec.isolate {
            return;
        }
        let Some(path) = &record.spec.workdir else {
            return;
        };
        let (worktree_removed, branch_removed, reason) = worktrees::cleanup_unstarted(record).await;
        self.emit(EventKind::WorktreeCleanup {
            agent: record.id.clone(),
            path: path.clone(),
            worktree_removed,
            branch_removed,
            reason,
        });
    }

    /// A linked worktree of the agent's repository, made for it under the
    /// daemon's sibling worktree directory, on a branch named after it: `agent/<name>`, or with
    /// its id appended when that is taken by an earlier run.
    async fn isolate(&self, record: &AgentRecord) -> Result<PathBuf, Box<Response>> {
        let invalid = |text: &str| Box::new(Response::error(ErrorCode::Invalid, text));
        let Some(workdir) = record.spec.workdir.clone() else {
            return Err(invalid(
                "isolate needs a working directory inside a git checkout",
            ));
        };
        let base = self
            .project_for(Some(workdir), true)
            .await
            .filter(|p| p.source == ProjectSource::Git)
            .ok_or_else(|| invalid("isolate needs a working directory inside a git checkout"))?;
        let name = if record.spec.name.is_empty() {
            default_name(&record.id)
        } else {
            record.spec.name.clone()
        };
        // The name becomes a path component and part of a git ref: say so
        // before anything is created rather than let git say "invalid".
        if !isolate_name_ok(&name) {
            return Err(invalid(&format!(
                "agent name `{name}` cannot name a worktree and branch: use letters, digits, '-', '_' and '.', not starting with '-' or '.', without '..' or a '.lock' ending"
            )));
        }
        let dir = paths::worktree_dir(&self.home).join(base.id().short());
        if let Err(err) = std::fs::create_dir_all(&dir) {
            return Err(Box::new(Response::error(
                ErrorCode::Internal,
                format!("cannot create {}: {err}", dir.display()),
            )));
        }
        let root = project::canonical(base.dir());
        let dir = project::canonical(&dir);
        let candidates = [
            (dir.join(&name), format!("agent/{name}")),
            (
                dir.join(format!("{name}-{}", record.id.short())),
                format!("agent/{name}-{}", record.id.short()),
            ),
        ];
        let mut last: Option<Box<Response>> = None;
        for (path, branch) in candidates {
            if path.exists() {
                continue;
            }
            match worktrees::add_worktree(root.clone(), &path, &branch).await {
                Ok(()) => {
                    info!(agent = %record.id.short(), path = %path.display(), %branch, "isolated in a worktree");
                    self.emit(EventKind::WorktreeCreated {
                        agent: record.id.clone(),
                        path: path.clone(),
                    });
                    return Ok(path);
                }
                Err(response) => last = Some(response),
            }
        }
        Err(last.unwrap_or_else(|| invalid("no free worktree path for the agent")))
    }

    /// Paths changed in more than one physical checkout of a project; with
    /// an agent, only those involving its checkout.
    async fn overlap(
        &self,
        project: &str,
        since_seq: Option<u64>,
        agent: Option<String>,
    ) -> Response {
        let mine: Option<(ProjectId, PathBuf)> = match agent {
            Some(reference) => {
                let id = match self.resolve(&reference) {
                    Ok(id) => id,
                    Err(response) => return *response,
                };
                let found = lock(&self.state).registry.get(&id).and_then(|record| {
                    record
                        .project
                        .as_ref()
                        .map(|p| (p.id(), project::canonical(p.dir())))
                });
                match found {
                    Some(found) => Some(found),
                    None => {
                        return Response::error(ErrorCode::Invalid, "the agent is in no project");
                    }
                }
            }
            None => None,
        };
        let project = if project.is_empty() {
            match &mine {
                Some((project, _)) => project.clone(),
                None => {
                    return Response::error(ErrorCode::Invalid, "name a project or an agent");
                }
            }
        } else {
            match self.resolve_project(project).await {
                Ok(id) => id,
                Err(response) => return *response,
            }
        };
        // Newest first, a page per hold of the lock, so a long ledger never
        // stalls other requests for the whole scan. Rows are append-only,
        // so paging below the oldest seq seen is consistent.
        let mut changes: Vec<Change> = Vec::new();
        let mut before_seq: Option<u64> = None;
        while changes.len() < OVERLAP_SCAN {
            let query = ChangesQuery {
                project: project.clone(),
                since_seq,
                path: None,
                agent: None,
                limit: OVERLAP_PAGE.min(OVERLAP_SCAN - changes.len()),
                after: None,
                before_seq,
            };
            let page = match lock(&self.state).store.changes(&query) {
                Ok(page) => page,
                Err(err) => {
                    return Response::error(
                        ErrorCode::StorageUnavailable,
                        format!("ledger query failed: {err}"),
                    );
                }
            };
            let Some(oldest) = page.first().map(|c| c.seq) else {
                break;
            };
            before_seq = Some(oldest);
            changes.extend(page);
            tokio::task::yield_now().await;
        }
        let mut overlaps = agentdocker_core::overlaps(&changes);
        if let Some((_, checkout)) = mine {
            overlaps.retain(|o| o.parties.iter().any(|p| p.checkout == checkout));
        }
        Response::Overlap { overlaps }
    }

    /// Return the last successful scan only while it is fresh and healthy.
    async fn discover(&self) -> Response {
        {
            let state = lock(&self.state);
            let cache = &state.discovered;
            if cache.error.is_none() && cache.at.is_some_and(|at| at.elapsed() < DISCOVERY_FRESH) {
                return Response::Processes {
                    processes: cache.processes.clone(),
                };
            }
        }
        match self.scan_agents().await {
            Ok(processes) => Response::Processes { processes },
            Err(error) => Response::error(ErrorCode::Unavailable, error),
        }
    }

    /// Serialize scans, then reconcile their results with the current registry
    /// in one synchronous transition. Registration may advance while ps runs.
    pub async fn scan_agents(&self) -> Result<Vec<DiscoveredProcess>, String> {
        use std::sync::atomic::Ordering;
        loop {
            // Register before checking the flag so completion between the
            // check and await cannot be missed.
            let finished = self.scan_finished.notified();
            tokio::pin!(finished);
            finished.as_mut().enable();
            if self
                .scanning
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                break;
            }
            finished.await;
            let state = lock(&self.state);
            if let Some(error) = &state.discovered.error {
                return Err(error.clone());
            }
            if state
                .discovered
                .at
                .is_some_and(|at| at.elapsed() < DISCOVERY_FRESH)
            {
                return Ok(state.discovered.processes.clone());
            }
            // The previous owner was cancelled without a fresh result. Try
            // to become the scanner instead of fabricating an empty result.
        }
        let _clear = ScanGuard {
            scanning: &self.scanning,
            finished: &self.scan_finished,
        };
        self.apply_scan(self.scan().await)
    }

    fn apply_scan(
        &self,
        result: Result<Vec<DiscoveredProcess>, String>,
    ) -> Result<Vec<DiscoveredProcess>, String> {
        let mut state = lock(&self.state);
        let mut found = match result {
            Ok(found) => found,
            Err(reason) => {
                if state.discovered.error.as_ref() != Some(&reason) {
                    state.emit(EventKind::DiscoveryUnavailable {
                        reason: reason.clone(),
                    });
                }
                state.discovered.error = Some(reason.clone());
                return Err(reason);
            }
        };
        let registered: HashMap<_, _> = state
            .registry
            .live()
            .filter_map(|a| a.pid.map(|pid| (pid, a.process_started_at)))
            .collect();
        let is_registered = |p: &DiscoveredProcess| {
            registered.get(&p.pid).is_some_and(|started| {
                started.is_none() || p.started_at.is_none() || *started == p.started_at
            })
        };
        found.retain(|p| !is_registered(p));
        let previous = std::mem::replace(&mut state.discovered.processes, found.clone());
        let identity = |p: &DiscoveredProcess| (p.pid, p.started_at);
        let now_ids: HashSet<_> = found.iter().map(identity).collect();
        // Remove the old PID generation before announcing its replacement.
        for process in previous.iter().filter(|p| !now_ids.contains(&identity(p))) {
            state.emit(EventKind::AgentVanished {
                pid: process.pid,
                started_at: process.started_at,
                runtime: process.runtime.clone(),
                adopted: is_registered(process),
            });
        }
        for process in found
            .iter()
            .filter(|p| !previous.iter().any(|old| old == *p))
        {
            state.emit(EventKind::AgentDiscovered {
                pid: process.pid,
                started_at: process.started_at,
                runtime: process.runtime.clone(),
                project: process.project.as_ref().map(ProjectRef::id),
                cwd: process.cwd.clone(),
            });
        }
        state.discovered.at = Some(Instant::now());
        if state.discovered.error.take().is_some() {
            state.emit(EventKind::DiscoveryAvailable);
        }
        Ok(found)
    }

    /// The agent tools on this machine, with how many unregistered
    /// processes of each the last scan saw.
    async fn runtimes(&self) -> Response {
        let roots = agentdocker_host::runtimes::Roots::from_env();
        let inventory = tokio::task::spawn_blocking(move || {
            agentdocker_host::runtimes::inventory(&roots, "agentdocker")
        })
        .await;
        let mut runtimes = match inventory {
            Ok(Ok(runtimes)) => runtimes,
            Ok(Err(err)) => return Response::error(ErrorCode::Unavailable, err.to_string()),
            Err(_) => {
                return Response::error(ErrorCode::Unavailable, "runtime inventory worker failed");
            }
        };
        let processes = match self.discover().await {
            Response::Processes { processes } => processes,
            error => return error,
        };
        for runtime in &mut runtimes {
            runtime.running = processes
                .iter()
                .filter(|p| p.runtime == runtime.name)
                .count();
        }
        Response::Runtimes { runtimes }
    }

    /// The process table, filtered to known agent runtimes that no live
    /// agent claims by pid. Projects come without fingerprints: this runs
    /// on every scan, and a process nobody adopted should not warm the
    /// cache or announce a repository.
    async fn scan(&self) -> Result<Vec<DiscoveredProcess>, String> {
        let mine = std::process::id();
        tokio::task::spawn_blocking(move || {
            let table = procinfo::processes().map_err(|e| e.to_string())?;
            let launchers = procinfo::codex_launchers(&table);
            // Ancestry needs the whole table, and only agents are asked
            // about, so it is built once rather than per candidate.
            let by_pid: BTreeMap<u32, procinfo::Process> =
                table.iter().map(|p| (p.pid, p.clone())).collect();
            let mut found: Vec<DiscoveredProcess> = table
                .into_iter()
                .filter(|p| p.pid != mine && !launchers.contains(&p.pid))
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
                        session: multiplexer::of(p.pid, &by_pid, None),
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
            Ok(found)
        })
        .await
        .map_err(|e| format!("process scan worker failed: {e}"))?
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
            runtime: runtime.clone(),
            workdir: cwd,
            labels: BTreeMap::from([("adopted".to_owned(), "true".to_owned())]),
            ..AgentSpec::default()
        };
        let response = self.register(spec, Some(pid), None).await;
        if matches!(response, Response::Agent { .. }) {
            let mut state = lock(&self.state);
            if let Some(index) = state.discovered.processes.iter().position(|p| p.pid == pid) {
                let process = state.discovered.processes.remove(index);
                state.emit(EventKind::AgentVanished {
                    pid,
                    started_at: process.started_at,
                    runtime,
                    adopted: true,
                });
            }
        }
        response
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
    /// Every directory the watcher should be watching.
    ///
    /// Not merely "where each agent is". A project is the repository,
    /// and the repository is every worktree of it: an agent can commit
    /// in a checkout nobody registered — its own `--isolate` worktree,
    /// or one a person made by hand — and if that directory is not
    /// watched, the commit reaches neither the journal nor the ledger,
    /// and `overlap` answers "nothing collides" from a single checkout.
    /// That last one is the worst of the three, because it is a
    /// confident wrong answer to the question the feature exists for.
    pub fn watch_targets(&self) -> Vec<Checkout> {
        let mut seen: HashSet<PathBuf> = HashSet::new();
        let state = lock(&self.state);
        let mut targets: Vec<Checkout> = state
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
            .collect();
        // Then the rest of each project's checkouts, from the cache the
        // VCS sweep keeps. Nested ones are skipped: a worktree inside a
        // watched directory is already covered, and watching it twice
        // would record every change in it twice.
        let known: Vec<(ProjectId, Vec<PathBuf>)> = state
            .project_checkouts
            .iter()
            .map(|(id, dirs)| (id.clone(), dirs.clone()))
            .collect();
        drop(state);
        for (project, dirs) in known {
            if !targets.iter().any(|c| c.project == project) {
                continue; // nothing live in it; nothing to watch for
            }
            let mut added = 0usize;
            let mut skipped = 0usize;
            for dir in dirs {
                if seen.contains(&dir) || seen.iter().any(|w| dir.starts_with(w)) {
                    continue;
                }
                if added >= MAX_EXTRA_CHECKOUTS {
                    skipped += 1;
                    continue;
                }
                seen.insert(dir.clone());
                added += 1;
                targets.push(Checkout {
                    dir: dir.clone(),
                    project: project.clone(),
                    // Anything that is not where an agent lives is a
                    // linked worktree of the same repository.
                    worktree: Some(dir),
                });
            }
            if skipped > 0 {
                // Said out loud rather than silently dropped: an
                // unwatched checkout is a hole in the ledger, and a hole
                // nobody is told about is the failure mode this whole
                // change exists to remove.
                self.emit(EventKind::WatcherGap {
                    reason: format!(
                        "{skipped} more checkout{} of project {} than the {MAX_EXTRA_CHECKOUTS} \
                         this watches; changes in them are not recorded",
                        if skipped == 1 { "" } else { "s" },
                        project.short()
                    ),
                });
            }
        }
        targets
    }

    /// Read one checkout's HEAD and journal it if it moved.
    ///
    /// The agent-driven sweep only looks where agents are. This looks at
    /// a checkout as a checkout, so a commit in a worktree nobody
    /// registered still reaches the journal.
    pub(crate) async fn note_checkout(&self, checkout: &Checkout) {
        let dir = checkout.dir.clone();
        let known = lock(&self.state).last_head.get(&dir).cloned();
        let dir_for_read = dir.clone();
        let known_for_read = known.clone();
        let observed = tokio::task::spawn_blocking(move || {
            let state = vcs::state(&dir_for_read)?;
            // Naming the commit runs `git log`, so it is only done when
            // there is something new to name. This is called for every
            // checkout of every project on every sweep.
            let subject = match &state.head {
                Some(head) if Some(head) != known_for_read.as_ref() => {
                    vcs::subject(&dir_for_read, head, SUBJECT_TIMEOUT)
                }
                _ => None,
            };
            Some((state, subject))
        })
        .await
        .ok()
        .flatten();
        let Some((state, subject)) = observed else {
            return;
        };
        // A checkout seen for the first time is recorded, not announced:
        // its history did not happen while we were watching. The branch
        // goes down with the head, or the next commit here would read as
        // a switch onto a branch it was already on.
        let Some(before) = known else {
            if let Some(head) = state.head {
                let mut daemon = lock(&self.state);
                daemon.last_head.insert(dir.clone(), head);
                daemon.last_branch.insert(dir, state.branch);
            }
            return;
        };
        if state.head.as_deref() == Some(before.as_str()) {
            return;
        }
        // Only the head is known to have been different; the branch it
        // was on is remembered per checkout, and `note_checkout_move`
        // falls back to that.
        let old = VcsState {
            head: Some(before),
            branch: None,
            ..state.clone()
        };
        lock(&self.state).note_checkout_move(
            checkout.project.clone(),
            dir,
            checkout.worktree.clone(),
            Some(&old),
            &state,
            subject,
        );
    }

    /// Re-read where each live project's checkouts are.
    ///
    /// Runs git, so it happens off the lock and on the same five-second
    /// tick as the VCS sweep. A project whose enumeration fails keeps
    /// the checkouts it had: an empty answer means "ask again", never
    /// "there is only one", and treating it as the latter would quietly
    /// stop watching real work.
    pub async fn refresh_project_checkouts(&self) {
        let roots: Vec<(ProjectId, PathBuf)> = {
            let mut seen: HashSet<ProjectId> = HashSet::new();
            lock(&self.state)
                .registry
                .live()
                .filter_map(|a| a.project.as_ref())
                .filter(|p| matches!(p.source, ProjectSource::Git | ProjectSource::Agentfile))
                .filter(|p| seen.insert(p.id()))
                .map(|p| (p.id(), p.dir().to_path_buf()))
                .collect()
        };
        if roots.is_empty() {
            return;
        }
        let found = tokio::task::spawn_blocking(move || {
            roots
                .into_iter()
                .map(|(id, dir)| {
                    let dirs = vcs::worktrees(&dir, SUBJECT_TIMEOUT);
                    (id, dirs)
                })
                .collect::<Vec<_>>()
        })
        .await
        .unwrap_or_default();
        {
            let mut state = lock(&self.state);
            for (id, dirs) in found {
                if dirs.is_empty() {
                    continue;
                }
                state.project_checkouts.insert(id, dirs);
            }
        }
        // Take each checkout's head now rather than waiting for a
        // filesystem event. Two reasons: a checkout first seen at the
        // moment of a commit would have that commit read as its
        // starting point and swallowed, and this is the polling net
        // under the watcher for the days it misses something.
        for checkout in self.watch_targets() {
            self.note_checkout(&checkout).await;
        }
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
    pub(crate) fn set_watcher_attach(&self, sender: mpsc::Sender<WatcherAttachment>) {
        self.watcher_attach.send_replace(WatcherLink::On(sender));
        self.emit(EventKind::WatcherStarted);
    }

    /// The watcher could not start after all; stop waiting for it.
    pub fn watcher_off(&self, reason: String) {
        self.watcher_attach
            .send_replace(WatcherLink::Unavailable(reason.clone()));
        self.emit(EventKind::WatcherUnavailable { reason });
    }

    /// The terminal of a managed agent, when it was given one.
    pub fn session(&self, agent: &AgentId) -> Option<supervisor::Session> {
        lock(&self.sessions).get(agent).cloned()
    }

    /// The agent is gone; so is its terminal.
    pub fn end_session(&self, agent: &AgentId) {
        lock(&self.sessions).remove(agent);
    }

    /// The restricted endpoint is serving on `socket`.
    pub fn restricted_listening(&self, socket: PathBuf) {
        *lock(&self.restricted) = RestrictedEndpoint::On(socket.clone());
        self.emit(EventKind::RestrictedEndpointListening { socket });
    }

    /// The restricted endpoint could not be served; grants are refused
    /// from here on and the host socket carries on.
    pub fn restricted_unavailable(&self, reason: String) {
        *lock(&self.restricted) = RestrictedEndpoint::Off(reason.clone());
        self.emit(EventKind::RestrictedEndpointUnavailable { reason });
    }

    pub fn restricted(&self) -> RestrictedEndpoint {
        lock(&self.restricted).clone()
    }

    /// Have the watcher cover every checkout a live agent works in, now:
    /// registration awaits this so the agent's first edit is never made in
    /// the gap before the next reconcile tick. Bounded, and a no-op when no
    /// watcher runs.
    async fn ensure_watched(&self, record: &AgentRecord) -> Result<(), String> {
        let checkout = record
            .project
            .as_ref()
            .ok_or("agent has no watchable checkout")?
            .dir()
            .to_path_buf();
        tokio::time::timeout(WATCHER_ATTACH_TIMEOUT, async {
            let mut link = self.watcher_attach.subscribe();
            let sender = match link
                .wait_for(|link| !matches!(link, WatcherLink::Starting))
                .await
            {
                Ok(link) => match &*link {
                    WatcherLink::On(sender) => sender.clone(),
                    WatcherLink::Off => return Ok(()),
                    WatcherLink::Unavailable(reason) => return Err(reason.clone()),
                    WatcherLink::Starting => unreachable!(),
                },
                Err(_) => return Err("watcher stopped during startup".into()),
            };
            let (ack, done) = oneshot::channel();
            sender
                .send(WatcherAttachment { checkout, ack })
                .await
                .map_err(|_| "watcher stopped before attachment".to_string())?;
            done.await
                .map_err(|_| "watcher stopped before confirming coverage".to_string())?
        })
        .await
        .map_err(|_| "watcher did not confirm checkout coverage within 500 ms".to_string())?
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
            // The strongest "working" signal there is: a file changed
            // under a lease this agent holds. Recording it keeps derived
            // activity honest for a runtime with no hooks at all.
            if let Attribution::Agent { agent, .. } = &change.by {
                let agent = agent.clone();
                state.registry.touch(&agent, now);
            }
            state.warn_readers(&change, physical.as_deref());
            // A second checkout on this path means two agents are in the
            // same work: give them a room.
            state.note_contested(&change);
            debug!(project = %change.project.short(), path = %change.path.display(), %kind, "file changed");
            let _ = state
                .events
                .send(Event::new(EventKind::FileChanged { change }, now));
        }
        // A repository's refs are shared between its worktrees: a commit
        // made in a linked one writes `refs/heads/<branch>` under the
        // *main* checkout's git directory. So the touched checkout says
        // which repository moved, not which checkout of it — and every
        // checkout of that repository has to be looked at.
        let touched: HashSet<ProjectId> = vcs_touched.into_iter().map(|c| c.project).collect();
        if touched.is_empty() {
            return;
        }
        for checkout in self
            .watch_targets()
            .into_iter()
            .filter(|c| touched.contains(&c.project))
        {
            // Both, and in this order: the agents in that checkout get
            // their branch and head refreshed, and the checkout itself
            // is journaled even when no agent lives in it. A worktree
            // nobody registered is where most of the work happens.
            self.refresh_vcs(Some(&checkout.dir)).await;
            self.note_checkout(&checkout).await;
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
            before_seq: None,
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
        let state = lock(&self.state);
        let agents = state.registry.matching(all, project.as_ref(), &labels);
        let ids: HashSet<_> = agents.iter().map(|agent| &agent.id).collect();
        let aliases = state
            .registry
            .aliases()
            .iter()
            .filter(|(_, canonical)| ids.contains(canonical))
            .map(|(old, canonical)| (old.clone(), canonical.clone()))
            .collect();
        Response::Agents { agents, aliases }
    }

    async fn send(
        &self,
        from: String,
        to: &str,
        kind: String,
        payload: Value,
        reply_to: Option<MessageId>,
    ) -> Response {
        let (from, to) = match self.endpoints(from, to).await {
            Ok(pair) => pair,
            Err(response) => return *response,
        };
        let mut state = lock(&self.state);
        // `agentd` and a bare `user` speak without a record, and a rule
        // has nothing to match them against; the daemon's own notices
        // are not the thing a policy is for.
        let sender = AgentId::from(from.as_str());
        if state.registry.get(&sender).is_some() {
            let action = format!("send:{to}");
            let ruling = state.permits(&sender, &action);
            if !ruling.is_allowed() {
                return state.refuse(&sender, &action, ruling);
            }
        }
        state.send(from, to, kind, payload, reply_to)
    }

    /// Turn a sender name and a destination shorthand into what the bus
    /// routes on. `ask` needs the resolved pair as well as the send, so
    /// that it can record who is waiting for the answer.
    async fn endpoints(
        &self,
        from: String,
        to: &str,
    ) -> Result<(String, Destination), Box<Response>> {
        let from = match lock(&self.state).registry.resolve(&from) {
            Ok(id) => id.to_string(),
            // An unregistered sender is allowed: `agentd` and a bare
            // `user` both speak without a record of their own.
            Err(RegistryError::NotFound(_)) => from,
            Err(err) => return Err(Box::new(registry_error(err))),
        };
        let to = match Destination::parse(to) {
            Destination::Agent(reference) => Destination::Agent(self.resolve(reference.as_str())?),
            Destination::Project(selector) => {
                Destination::Project(self.resolve_project(selector.as_str()).await?)
            }
            other => other,
        };
        Ok((from, to))
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
                let backlog = state.read_inbox(id, false)?;
                *state.live_subscribers.entry(id.clone()).or_default() += 1;
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

    #[allow(clippy::too_many_arguments)]
    async fn claim(
        &self,
        reference: &str,
        resource: String,
        mode: LeaseMode,
        amount: Option<u64>,
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
        // Asked once, before the first attempt: a policy answer does not
        // change while a claim waits, and re-asking would put a refusal
        // in the log for every retry.
        {
            let mut state = lock(&self.state);
            let action = format!("claim:{resource}");
            let ruling = state.permits(&holder, &action);
            if !ruling.is_allowed() {
                return state.refuse(&holder, &action, ruling);
            }
        }
        let deadline = tokio::time::Instant::now()
            + std::time::Duration::from_secs(wait_secs.min(MAX_WAIT_SECS));
        // Subscribe before the first attempt so a release that lands between
        // a failed attempt and the wait is not missed.
        let mut events = self.subscribe_events();
        let mut reported_conflict = false;
        // The place in the queue, taken on the first conflict and given
        // up however this ends — including by the client disappearing,
        // which drops this future and with it the guard. A cancelled
        // claim left at the head of a queue would starve everyone
        // behind it.
        let mut waiting = waiting::Waiting::new(self, holder.clone(), resource.clone());
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
                let now = Utc::now();
                state.expire_leases_at(now);
                if let Some(error) = state.storage_failure() {
                    return error;
                }
                // Fairness: a waiter with somebody ahead of it on an
                // overlapping resource does not try, so a newcomer
                // cannot take what somebody has been waiting minutes
                // for. A first attempt has no ticket and always tries.
                // A quota is a number, not a place: several agents hold
                // one at once, and what decides is whether the sum fits.
                // Checked here, under the same lock as the table, so two
                // claims cannot both see room for the last of it.
                if resource.kind() == "quota"
                    && let Some(capacity) = state.quota_capacity(&holder, resource.value())
                {
                    let want = amount.unwrap_or(1);
                    let committed = state.leases.committed(&resource);
                    if committed.saturating_add(want) > capacity {
                        state.touch(&holder);
                        if let Some(error) = state.storage_failure() {
                            return error;
                        }
                        return Response::Error {
                            code: ErrorCode::Conflict,
                            message: format!(
                                "{resource} has {} of {capacity} left and {want} was asked for",
                                capacity.saturating_sub(committed)
                            ),
                            details: Some(json!({
                                "quota": resource.value(),
                                "capacity": capacity,
                                "committed": committed,
                                "requested": want,
                            })),
                        };
                    }
                }
                let result = if state.may_attempt(waiting.ticket()) {
                    state.leases.clone().claim(
                        resource.clone(),
                        holder.clone(),
                        // A quota is shared by construction: it is spent,
                        // not occupied, so exclusivity would make every
                        // budget a lock on itself.
                        if resource.kind() == "quota" {
                            LeaseMode::Shared
                        } else {
                            mode
                        },
                        ttl(ttl_secs),
                        note.clone(),
                        now,
                    )
                } else {
                    Err(LeaseError::Conflict {
                        resource: resource.clone(),
                        held_by: state
                            .leases
                            .holders_of(&resource)
                            .into_iter()
                            .cloned()
                            .collect(),
                    })
                };
                let (message, held_by) = match result {
                    Ok(Claimed::New(mut lease)) => {
                        lease.change_seq = state
                            .store_op("lease ledger boundary", |store| store.change_watermark());
                        if lease.resource.kind() == "quota" {
                            lease.amount = amount.unwrap_or(1);
                        }
                        if !state.commit_lease_activity(
                            &holder,
                            Some(&lease),
                            Some(EventKind::LeaseClaimed {
                                lease: lease.clone(),
                            }),
                            now,
                        ) {
                            return state
                                .storage_failure()
                                .expect("failed lease commit freezes storage");
                        }
                        drop(state);
                        waiting.end(agentdocker_core::WaitOutcome::Claimed);
                        return Response::Lease { lease };
                    }
                    Ok(Claimed::Renewed(lease)) => {
                        if !state.commit_lease_activity(
                            &holder,
                            Some(&lease),
                            Some(EventKind::LeaseRenewed {
                                lease: lease.clone(),
                            }),
                            now,
                        ) {
                            return state
                                .storage_failure()
                                .expect("failed lease commit freezes storage");
                        }
                        drop(state);
                        waiting.end(agentdocker_core::WaitOutcome::Claimed);
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
                // One conflict event per request, committed with liveness.
                let conflict = (!reported_conflict).then(|| EventKind::LeaseConflict {
                    resource: resource.clone(),
                    requester: holder.clone(),
                    held_by: held_by.iter().map(|l| l.holder.clone()).collect(),
                });
                if !state.commit_lease_activity(&holder, None, conflict, now) {
                    return state
                        .storage_failure()
                        .expect("failed conflict commit freezes storage");
                }
                if !reported_conflict {
                    reported_conflict = true;
                    warn!(agent = %holder.short(), %resource, waiting = wait_secs > 0, "lease conflict");
                }
                // Before agreeing to wait: would waiting close a cycle?
                // If it would, nobody in it could ever proceed, and the
                // newcomer is always the victim — deterministic, and it
                // needs no priorities.
                if wait_secs > 0 && waiting.ticket().is_none() {
                    if let Some(cycle) = state.deadlock(&holder, &resource) {
                        warn!(agent = %holder.short(), %resource, "claim would deadlock");
                        state.emit(EventKind::LeaseDeadlock {
                            cycle: cycle.clone(),
                        });
                        // A wait that never began still ended, and its
                        // outcome is one subscribers are told to expect.
                        waiting.never_started(&mut state, agentdocker_core::WaitOutcome::Deadlock);
                        return Response::Error {
                            code: ErrorCode::Deadlock,
                            message: format!(
                                "waiting for {resource} would deadlock: {}",
                                describe(&cycle)
                            ),
                            details: Some(json!({ "cycle": cycle, "held_by": held_by })),
                        };
                    }
                    // Joined under the same lock the check ran under.
                    // Letting go in between would let two claims each
                    // see no cycle and then both create one.
                    waiting.join_locked(&mut state, mode);
                }
                (message, held_by)
            };
            if wait_secs == 0 {
                return Response::Error {
                    code: ErrorCode::Conflict,
                    message,
                    details: Some(json!({ "held_by": held_by })),
                };
            }
            if !wait_for_release(&mut events, &resource, deadline).await {
                waiting.end(agentdocker_core::WaitOutcome::Timeout);
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
        let started = state_timing_start();
        let result = op(&self.store);
        state_timing_finish(what, started);
        match result {
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
        self.persist("event", |store| store.append_event(&event));
        if self.storage_error.is_none() {
            self.next_seq += 1;
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
        self.inbox_bytes.remove(&id);
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
        // Called only after the owned process group has drained. A cancellation
        // can have made the registry terminal already; still retire its owner.
        self.supervised.remove(id);
        if self.storage_error.is_some() {
            return self.registry.get(id).cloned();
        }
        if !self.is_live(id) {
            return self.registry.get(id).cloned();
        }
        let now = Utc::now();
        let mut record = self.registry.get(id)?.clone();
        record.status = status.clone();
        record.finished_at.get_or_insert(now);
        record.last_seen = now;
        let released: Vec<_> = self.leases.by_holder(id).into_iter().cloned().collect();
        let mut journal = Vec::new();
        if !released.is_empty() {
            if let Some(entry) = self.release_entry(id, &released, None, SummarySource::Explicit) {
                journal.push(entry);
            }
        }
        if let Some(entry) = self.plain_entry(
            &record,
            JournalKind::Leave,
            format!("left ({status})"),
            SummarySource::Explicit,
        ) {
            journal.push(entry);
        }
        let channels = self.closing_channels(id, now);
        let previous_seq = self.journal_seq.clone();
        let mut kinds: Vec<_> = released
            .iter()
            .cloned()
            .map(|lease| EventKind::LeaseReleased { lease })
            .collect();
        for entry in &mut journal {
            entry.seq = self.next_journal_seq(&entry.project);
            kinds.push(EventKind::JournalAppended {
                entry: entry.clone(),
            });
        }
        kinds.extend(channels.iter().map(|channel| EventKind::ChannelClosed {
            channel: channel.id.clone(),
            resolution: channel.resolution.clone(),
        }));
        kinds.push(EventKind::AgentExited {
            agent: id.clone(),
            status: status.clone(),
        });
        let events: Vec<_> = kinds
            .into_iter()
            .enumerate()
            .map(|(index, kind)| {
                let mut event = Event::new(kind, now);
                event.seq = self.next_seq + index as u64;
                event
            })
            .collect();
        let leases: Vec<_> = released.iter().map(|lease| lease.id.clone()).collect();
        self.persist("agent exit", |store| {
            store.agent_exit(&record, &leases, &journal, &channels, &events)
        });
        if self.storage_error.is_some() {
            self.journal_seq = previous_seq;
            return self.registry.get(id).cloned();
        }
        *self.registry.get_mut(id).expect("exit identity retained") = record.clone();
        self.leases.release_all(id);
        for entry in journal {
            self.cache_journal(entry);
        }
        for channel in channels {
            self.channels.insert(channel.id.clone(), channel);
        }
        self.next_seq += events.len() as u64;
        for event in events {
            let _ = self.events.send(event);
        }
        info!(agent = %id.short(), name = %record.spec.name, %status, "agent finished");
        Some(record)
    }

    fn touch(&mut self, id: &AgentId) {
        if let Some(mut record) = self.registry.get(id).cloned() {
            record.last_seen = Utc::now();
            self.persist("agent", |store| store.upsert_agent(&record));
            if self.storage_error.is_none() {
                *self
                    .registry
                    .get_mut(id)
                    .expect("liveness identity retained") = record;
            }
        }
    }

    fn commit_lease_activity(
        &mut self,
        holder: &AgentId,
        lease: Option<&Lease>,
        kind: Option<EventKind>,
        now: DateTime<Utc>,
    ) -> bool {
        let mut record = self
            .registry
            .get(holder)
            .expect("claim identity retained")
            .clone();
        record.last_seen = now;
        let event = kind.map(|kind| {
            let mut event = Event::new(kind, now);
            event.seq = self.next_seq;
            event
        });
        self.persist("lease activity", |store| {
            store.lease_activity(&record, lease, event.as_ref())
        });
        if self.storage_error.is_some() {
            return false;
        }
        *self
            .registry
            .get_mut(holder)
            .expect("claim identity retained") = record;
        if let Some(lease) = lease {
            self.leases.restore(lease.clone());
        }
        if let Some(event) = event {
            self.next_seq += 1;
            let _ = self.events.send(event);
        }
        true
    }

    fn ack_inbox(&mut self, reference: &str, messages: &[MessageId]) -> Response {
        let id = match self.resolve(reference) {
            Ok(id) => id,
            Err(response) => return *response,
        };
        // Repeated or fabricated IDs do not establish a new receipt. Retain
        // inbox order and record only the accepted work this call removes.
        let wanted: HashSet<&MessageId> = messages.iter().collect();
        let mut acknowledged = HashSet::new();
        let messages: Vec<MessageId> = self
            .inboxes
            .get(&id)
            .into_iter()
            .flatten()
            .filter(|message| {
                wanted.contains(&message.id) && acknowledged.insert(message.id.clone())
            })
            .map(|message| message.id.clone())
            .collect();
        if messages.is_empty() {
            return Response::Ok;
        }
        let mut event = Event::new(
            EventKind::InboxAcknowledged {
                agent: id.clone(),
                messages: messages.to_vec(),
            },
            Utc::now(),
        );
        event.seq = self.next_seq;
        self.persist("inbox acknowledgement", |store| {
            store.ack_inbox(&id, &messages, &event)
        });
        if let Some(error) = self.storage_failure() {
            return error;
        }
        if let Some(queue) = self.inboxes.get_mut(&id) {
            let bytes = self.inbox_bytes.entry(id.clone()).or_default();
            queue.retain(|message| {
                if messages.contains(&message.id) {
                    *bytes = bytes.saturating_sub(message_bytes(message));
                    false
                } else {
                    true
                }
            });
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
        // A failed liveness write must stop the operation before queue removal.
        self.touch(&id);
        let messages = match self.read_inbox(&id, drain) {
            Ok(messages) => messages,
            Err(error) => return *error,
        };
        Response::Messages { messages }
    }

    /// Snapshot before removal; an explicit destructive read commits its exact
    /// IDs and replay event together. Opening a stream never acknowledges work.
    fn read_inbox(&mut self, id: &AgentId, drain: bool) -> Result<Vec<Envelope>, Box<Response>> {
        if let Some(error) = self.storage_failure() {
            return Err(Box::new(error));
        }
        let messages: Vec<Envelope> = self
            .inboxes
            .get(id)
            .map(|queue| queue.iter().cloned().collect())
            .unwrap_or_default();
        if drain && !messages.is_empty() {
            let ids = messages
                .iter()
                .map(|message| message.id.clone())
                .collect::<Vec<_>>();
            let response = self.ack_inbox(id.as_str(), &ids);
            if !matches!(response, Response::Ok) {
                return Err(Box::new(response));
            }
        }
        Ok(messages)
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
        if !self
            .registry
            .get(&holder)
            .is_some_and(|a| a.status == AgentStatus::Running)
        {
            return Response::error(ErrorCode::Invalid, "agent is not running");
        }
        let now = Utc::now();
        self.expire_leases_at(now);
        if let Some(error) = self.storage_failure() {
            return error;
        }
        let result = self
            .leases
            .clone()
            .renew(lease, &holder, ttl(ttl_secs), now);
        match result {
            Ok(lease) => {
                if !self.commit_lease_activity(
                    &holder,
                    Some(&lease),
                    Some(EventKind::LeaseRenewed {
                        lease: lease.clone(),
                    }),
                    now,
                ) {
                    return self
                        .storage_failure()
                        .expect("failed renewal freezes storage");
                }
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
        match self.leases.clone().release(lease, &holder) {
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
        let released = self
            .leases
            .by_holder(&holder)
            .into_iter()
            .cloned()
            .collect();
        let released = self.finish_release(&holder, released, summary, source);
        Response::Leases { leases: released }
    }

    /// Stage deletion and the journal/replay evidence in one transaction,
    /// then remove memory protection and announce. Returns leases for the reply.
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
        let previous_seq = self.journal_seq.clone();
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
            for lease in &released {
                self.leases
                    .release(&lease.id, &lease.holder)
                    .expect("release protection retained until commit");
            }
            if let Some(entry) = entry {
                self.cache_journal(entry);
            }
            self.next_seq += events.len() as u64;
            for event in events {
                let _ = self.events.send(event);
            }
        } else {
            self.journal_seq = previous_seq;
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
                before_seq: None,
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

    /// A checkout's HEAD moved, seen through an agent that lives in it.
    fn note_head_move(
        &mut self,
        id: &AgentId,
        old: Option<&VcsState>,
        new: &VcsState,
        subject: Option<String>,
    ) {
        let Some(record) = self.registry.get(id) else {
            return;
        };
        let Some(project) = record.project.clone() else {
            return;
        };
        self.note_checkout_move(
            project.id(),
            project.dir().to_path_buf(),
            project.worktree.clone(),
            old,
            new,
            subject,
        );
    }

    /// A checkout's HEAD moved: one `commit` entry per checkout and HEAD,
    /// however many agents share it. Attributed to the only agent in that
    /// checkout, else the holder of its `branch:` lease, else nobody.
    ///
    /// Takes the checkout rather than an agent, because most checkouts
    /// of a project have no agent registered in them — a `--isolate`
    /// worktree, or one a person made by hand — and a commit there is
    /// still a commit in this project.
    fn note_checkout_move(
        &mut self,
        project_id: ProjectId,
        checkout: PathBuf,
        worktree: Option<PathBuf>,
        old: Option<&VcsState>,
        new: &VcsState,
        subject: Option<String>,
    ) {
        let Some(head) = new.head.clone() else {
            return;
        };
        if self.last_head.get(&checkout) == Some(&head) {
            return;
        }
        if self.committing.contains(&checkout) {
            // The daemon is making this commit itself and will record it
            // against the agent that asked, so there is nothing to say
            // here. Nothing is remembered either: advancing `last_head`
            // would mean that if the commit never got as far as writing
            // its entry, no later sweep would notice the move and the
            // commit would go unrecorded by anyone. Leaving the mark
            // where it was costs one repeated check per sweep and makes
            // the watcher the backstop it is supposed to be.
            return;
        }
        self.last_head.insert(checkout.clone(), head.clone());
        let was_on = self
            .last_branch
            .insert(checkout.clone(), new.branch.clone())
            .flatten();
        if old.is_none() {
            return; // first observation, not a move
        }
        let short: String = head.chars().take(7).collect();
        let before_branch = old.and_then(|o| o.branch.clone()).or(was_on);
        let summary = match (&before_branch, &new.branch) {
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
        // Built here rather than from an agent record: the checkout is
        // what this entry is about, and there may be no agent in it.
        let (agent, agent_name) = match &attributed {
            Some(agent) => (Some(agent.id.clone()), agent.spec.name.clone()),
            None => (None, "external".to_owned()),
        };
        let mut entry = JournalEntry {
            project: project_id,
            seq: 0,
            at: Utc::now(),
            agent,
            agent_name,
            branch: None,
            checkout: Some(checkout.clone()),
            worktree,
            kind: JournalKind::Commit,
            summary,
            summary_source: SummarySource::Synthesised,
            resources: Vec::new(),
            paths: Vec::new(),
            paths_total: 0,
            head_before: None,
            head_after: None,
            changes: None,
        };
        entry.branch = new.branch.clone();
        entry.head_before = old.and_then(|o| o.head.clone());
        entry.head_after = Some(head);
        self.append_journal(entry);
    }

    /// Serve a query, from the ring when it can be.
    fn journal_query(&mut self, query: JournalQuery) -> Response {
        let Some(head_seq) = self.store_op("journal head", |store| {
            store.max_journal_seq(&query.project)
        }) else {
            return Response::error(
                ErrorCode::StorageUnavailable,
                "journal head could not be read",
            );
        };
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
                    head_seq: Some(head_seq),
                };
            }
        }
        match self.store_op("journal", |store| store.journal(&query)) {
            Some(entries) => Response::Journal {
                project: query.project,
                entries,
                head_seq: Some(head_seq),
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
        self.publish(Envelope::new(from, to, kind, payload, reply_to, Utc::now()))
    }

    /// Route an envelope that is already built. `ask` needs the message
    /// id before the message goes out — it has to record who is waiting
    /// on the answer before an answer can arrive — so it builds its own.
    fn publish(&mut self, envelope: Envelope) -> Response {
        self.publish_question(envelope, None)
    }

    fn publish_question(
        &mut self,
        envelope: Envelope,
        question: Option<agentdocker_core::Question>,
    ) -> Response {
        if let Some(error) = self.storage_failure() {
            return error;
        }
        self.expire_questions(Utc::now());
        if let Some(error) = self.storage_failure() {
            return error;
        }
        if question.is_some() && self.questions.len() >= humans::MAX_QUESTIONS {
            return Response::error(
                ErrorCode::Unavailable,
                "too many questions are already waiting for an answer",
            );
        }
        let mut recipients: Vec<AgentId> = match &envelope.to {
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
            Destination::Channel(channel) => self
                .channel_members(channel)
                .into_iter()
                .filter(|id| id.as_str() != envelope.from)
                .collect(),
            Destination::Topic(_) => Vec::new(),
        };
        // New channel membership is unique already; legacy records may not be.
        // A recipient must consume one slot and one durable row per message.
        recipients.sort();
        recipients.dedup();
        let bytes = message_bytes(&envelope);
        if let Some(full) = recipients.iter().find(|id| {
            self.inboxes
                .get(*id)
                .is_some_and(|queue| queue.len() >= INBOX_CAPACITY)
                || self
                    .inbox_bytes
                    .get(*id)
                    .copied()
                    .unwrap_or_default()
                    .saturating_add(bytes)
                    > INBOX_BYTES
        }) {
            return Response::error(
                ErrorCode::Backpressure,
                format!(
                    "Inbox for {} is full (limit {INBOX_CAPACITY} messages / {} MiB). Acknowledge existing messages before retrying; nothing was sent.",
                    full.short(),
                    INBOX_BYTES / (1024 * 1024)
                ),
            );
        }
        let closed = envelope.reply_to.as_ref().and_then(|id| self.questions.get(id))
            .filter(|pending| matches!(&envelope.to, Destination::Agent(id) if id.as_str() == pending.from))
            .map(|pending| pending.id.clone());
        let sender = self
            .registry
            .get(&AgentId::from(envelope.from.as_str()))
            .cloned()
            .map(|mut record| {
                record.last_seen = Utc::now();
                record
            });
        let mut kinds = vec![EventKind::MessageSent {
            message: envelope.id.clone(),
            from: envelope.from.clone(),
            to: envelope.to.clone(),
            kind: envelope.kind.clone(),
        }];
        if let Some(question) = &question {
            kinds.push(EventKind::QuestionOpened {
                question: question.id.clone(),
                expires_at: question.expires_at,
            });
        }
        if let Some(closed) = &closed {
            kinds.push(EventKind::QuestionClosed {
                question: closed.clone(),
                answer: Some(envelope.id.clone()),
            });
        }
        let events: Vec<_> = kinds
            .into_iter()
            .enumerate()
            .map(|(index, kind)| {
                let mut event = Event::new(kind, envelope.sent_at);
                event.seq = self.next_seq + index as u64;
                event
            })
            .collect();
        self.persist("message", |store| {
            store.publish_message(
                &envelope,
                &recipients,
                INBOX_CAPACITY,
                sender.as_ref(),
                question.as_ref(),
                closed.as_ref(),
                &events,
            )
        });
        if let Some(error) = self.storage_failure() {
            return error;
        }
        for id in &recipients {
            let queue = self.inboxes.entry(id.clone()).or_default();
            queue.push_back(envelope.clone());
            *self.inbox_bytes.entry(id.clone()).or_default() += bytes;
        }
        if let Some(sender) = sender {
            let current = self
                .registry
                .get_mut(&sender.id)
                .expect("sender retained under lock");
            *current = sender;
        }
        if let Some(question) = question {
            self.questions.insert(question.id.clone(), question);
        }
        if let Some(closed) = closed {
            self.questions.remove(&closed);
        }
        self.next_seq += events.len() as u64;
        for event in events {
            let _ = self.events.send(event);
        }
        self.notify_humans(&envelope, &recipients);
        let subscribers = self.bus.send(envelope.clone()).unwrap_or(0);
        Response::Sent {
            message: envelope.id,
            subscribers,
        }
    }

    fn insert_record(&mut self, record: AgentRecord) -> Response {
        self.insert_record_announcing(record, true)
    }

    /// `announce_start` is false where the record exists before its
    /// process does — a pane agent, whose process tmux has not been asked
    /// for yet. Announcing there would tell subscribers an agent started
    /// that might never start, and would then announce it twice when it
    /// did.
    fn insert_record_announcing(
        &mut self,
        mut record: AgentRecord,
        announce_start: bool,
    ) -> Response {
        if let Some(error) = self.storage_failure() {
            return error;
        }
        if record.spec.name.is_empty() {
            record.spec.name = default_name(&record.id);
        }
        // One process, one agent.
        //
        // `adopt` has always held to that and refuses a pid it already
        // knows. Self-registration did not, and Claude Code registers
        // twice: its hooks adapter as `claude-<session>` and the MCP
        // server it launches as `claude-code-<pid>`, both giving the
        // same pid. One session appeared as two agents, each half
        // reporting to a different row — so the MCP-side row was
        // permanently idle while the hooks-side row did the work, and
        // a message sent to one never reached the other.
        //
        // Four things have to agree, and each one is load-bearing.
        //
        // The start time goes with the pid because pids are reused: an
        // old record and a new process that happens to land on its
        // number are not the same process, and folding them would hand
        // a stranger somebody else's identity. It must also be *known*
        // — two unreadable start times are not evidence of anything,
        // and `None == None` would have coalesced on no evidence at all.
        //
        // The runtime and the project go with them because sharing a
        // process is not the same as being the same agent: a host that
        // runs several sessions in one process would otherwise have
        // them all collapse into whichever registered first. Project
        // rather than workdir, because the two halves disagree about
        // the workdir the moment a session changes directory — the
        // hooks adapter reports where the session is now and the MCP
        // server reports where it was launched — while both still
        // resolve to the same project.
        let adopted = record
            .spec
            .labels
            .get("adopted")
            .is_some_and(|v| v == "true");
        // Every candidate, not the first one the map yields. A
        // registration that names no session matches any session in the
        // same process — that is what lets an MCP server join its hooks
        // half — so once a process holds two sessions there are two
        // candidates and no way to tell which this transport belongs
        // to. Picking one by iteration order would hand it a different
        // identity depending on the day. Two candidates means the
        // question is unanswerable, so it gets its own record and the
        // ambiguity is said out loud rather than resolved by luck.
        let mut candidates = self
            .registry
            .live()
            .filter(|a| same_agent(a, &record))
            .map(|a| a.id.clone())
            .collect::<Vec<_>>();
        candidates.sort();
        let existing = match candidates.as_slice() {
            [only] => Some(only.clone()),
            [] => None,
            many => {
                // Refused, not resolved and not given a record of its
                // own. A third sessionless record would be another
                // wildcard: it names no session, so it would match every
                // future session in this process too and breed more
                // duplicates. The caller is told to name its session,
                // because it is the only one that can know.
                return Response::error(
                    ErrorCode::Invalid,
                    format!(
                        "this process already has {} agents in different sessions and this \
                         registration names none, so which one it belongs to cannot be told; \
                         register with a session_id label",
                        many.len()
                    ),
                );
            }
        };
        if let Some(id) = existing {
            if adopted {
                return Response::error(ErrorCode::Invalid, "pid is already registered");
            }
            // The record takes on the session it has just been shown,
            // and this is not bookkeeping — without it the rule is not
            // transitive and two sessions collapse into one.
            //
            // Only the hooks adapter names a session, so an MCP-first
            // record has none, and "absent on one side is not a
            // mismatch" is what lets its hooks half join. Left that way,
            // the record still names no session afterwards, so the
            // *next* session in the same process matches it too and
            // adopts the same identity. Learning the first verified
            // session id closes it: the second session then disagrees
            // with a session that is present, and gets its own record.
            let learned = record
                .spec
                .labels
                .get("session_id")
                .filter(|id| !id.is_empty())
                .cloned();
            // An empty label is an absent one. A record carrying
            // `session_id: ""` would otherwise count as already knowing
            // whose it is and never learn, leaving the wildcard open.
            let bound = learned.filter(|_| {
                self.registry.get(&id).is_some_and(|a| {
                    a.spec
                        .labels
                        .get("session_id")
                        .is_none_or(|existing| existing.is_empty())
                })
            });
            if let Some(session) = bound {
                // One commit, then memory. The row and the event that
                // announces it go into a single transaction — written
                // separately there is a window where the record says one
                // thing and the log another, and a crash inside it lets
                // the next restart decide which. Memory is updated only
                // once the store has taken both, so a storage failure is
                // an error the caller sees rather than a divergence that
                // is resolved later by forgetting whose session this was.
                let mut updated = self.registry.get(&id).cloned().expect("just found");
                updated
                    .spec
                    .labels
                    .insert("session_id".to_owned(), session.clone());
                let mut event = Event::new(
                    EventKind::AgentSessionBound {
                        agent: id.clone(),
                        session,
                    },
                    Utc::now(),
                );
                event.seq = self.next_seq;
                self.persist("session binding", |store| {
                    store.agent_transition(&updated, &event)
                });
                if let Some(error) = self.storage_failure() {
                    return error;
                }
                *self.registry.get_mut(&id).expect("just found") = updated;
                self.next_seq += 1;
                let _ = self.events.send(event);
            }
            let agent = self.registry.get(&id).cloned().expect("just found");
            return Response::Agent { agent };
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
        if !record.managed && announce_start {
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

/// A live view of messages. Both replay and newly streamed addressed messages
/// remain in the durable inbox until explicitly acknowledged.
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
    /// Unacknowledged messages present when this stream opened.
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
            // Membership decides, not a subscription pattern: an agent put
            // in a channel hears it without having asked. Read outside any
            // state lock, from the stream loop.
            Destination::Channel(channel) => self.agent.as_ref().is_some_and(|me| {
                me.as_str() != envelope.from
                    && lock(&self.daemon.state)
                        .channel_members(channel)
                        .contains(me)
            }),
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
    use agentdocker_core::Activity;
    use agentdocker_core::contest::{Contest, Measure, Metric, Standing};
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

    /// A spec that says where it is working.
    ///
    /// Identity compares the physical checkout and will not match two
    /// agents that cannot say where they are, so a fixture standing in
    /// for a real session has to have one — a session always does.
    fn spec_here(name: &str) -> AgentSpec {
        AgentSpec {
            workdir: Some(std::env::temp_dir()),
            ..spec(name)
        }
    }

    /// One process is one agent, however many of its halves register.
    ///
    /// Claude Code registers twice for a single session — its hooks
    /// adapter under the session id, and the MCP server it launches
    /// under the pid — and both give the same pid. Before this, one
    /// session showed up as two agents: the MCP-side row never left
    /// idle, because activity arrives through hooks, and a message sent
    /// to one half never reached the other.
    #[tokio::test]
    async fn two_halves_of_one_session_register_as_one_agent() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let me = std::process::id();

        let hooks = register_here(&daemon, "claude-2c79ae10", Some(me)).await;
        let mcp = register_here(&daemon, "claude-code-45856", Some(me)).await;
        assert_eq!(mcp.id, hooks.id, "the second half is the first half");
        assert_eq!(
            mcp.spec.name, "claude-2c79ae10",
            "and keeps the name the first half registered"
        );
        assert_eq!(
            lock(&daemon.state).registry.live().count(),
            1,
            "one process, one row"
        );

        // A different process is a different agent, and an agent with no
        // pid at all — the human — is never folded into one.
        let other = register_here(&daemon, "codex-27221", Some(1)).await;
        assert_ne!(other.id, hooks.id);
        let human = register(&daemon, "user", None).await;
        let also_human = register(&daemon, "someone-else", None).await;
        assert_ne!(
            human.id, also_human.id,
            "no pid is not the same pid; two people are two agents"
        );
    }

    /// Two spellings of one directory are one checkout.
    ///
    /// Comparing raw paths let `/var/...` and `/private/var/...` — one
    /// directory, two spellings — look like two places to work. The
    /// alias is made here rather than borrowed from the platform,
    /// because the system temp directory is reached through a symlink on
    /// macOS and directly on Linux, and a test that relied on that would
    /// assert something true on one machine and false on another.
    #[tokio::test]
    async fn two_spellings_of_one_checkout_are_one_agent() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let me = std::process::id();
        let real = TempDir::new().unwrap();
        let alias = dir.path().join("alias-to-the-checkout");
        std::os::unix::fs::symlink(real.path(), &alias).unwrap();

        let register = async |name: &str, workdir: &std::path::Path| {
            let mut spec = spec(name);
            spec.workdir = Some(workdir.to_path_buf());
            match daemon
                .handle(Request::Register {
                    spec,
                    pid: Some(me),
                    session: None,
                })
                .await
            {
                Response::Agent { agent } => agent,
                other => panic!("unexpected {other:?}"),
            }
        };
        let first = register("claude-direct", real.path()).await;
        let through_alias = register("claude-aliased", &alias).await;
        assert_eq!(
            through_alias.id, first.id,
            "the same directory under another name is the same checkout"
        );
        assert_eq!(lock(&daemon.state).registry.live().count(), 1);
    }

    #[tokio::test]
    async fn registration_refuses_unresolved_or_non_directory_workdirs_without_state_changes() {
        let directory = TempDir::new().unwrap();
        let daemon = open(&directory);
        let fixture = TempDir::new().unwrap();
        let file = fixture.path().join("file");
        std::fs::write(&file, "fixture").unwrap();
        let cycle = fixture.path().join("cycle");
        std::os::unix::fs::symlink(&cycle, &cycle).unwrap();
        let before = daemon.recent_events(100);
        for workdir in [fixture.path().join("missing"), file, cycle] {
            let response = daemon
                .handle(Request::Register {
                    spec: AgentSpec {
                        workdir: Some(workdir),
                        ..spec("refused")
                    },
                    pid: Some(std::process::id()),
                    session: None,
                })
                .await;
            assert!(
                matches!(
                    response,
                    Response::Error {
                        code: ErrorCode::Invalid,
                        ..
                    }
                ),
                "{response:?}"
            );
            let state = lock(&daemon.state);
            assert_eq!(state.registry.live().count(), 0);
            assert!(state.store.load_agents().unwrap().is_empty());
            drop(state);
            assert_eq!(daemon.recent_events(100), before);
        }
    }

    /// Two checkouts of one project are two agents.
    ///
    /// Asserted against the predicate directly, with records that agree
    /// on everything else including the project id. Two unrelated
    /// temporary directories would have been rejected by the project
    /// comparison before the checkout clause was ever reached, so a test
    /// built that way passes whether or not the clause exists.
    #[tokio::test]
    async fn one_project_in_two_checkouts_is_two_agents() {
        let me = std::process::id();
        let born = procinfo::start_time(me);
        let one_project = ProjectRef::directory("/somewhere/that/is/one/project");
        let in_tree = |tree: &str| {
            let mut record = AgentRecord::new(spec("half"), false, Utc::now());
            record.spec.workdir = Some(PathBuf::from(tree));
            record.project = Some(one_project.clone());
            record.pid = Some(me);
            record.process_started_at = born;
            record.status = AgentStatus::Running;
            record
        };
        let main_checkout = in_tree("/repo");
        let worktree = in_tree("/repo-worktree");
        assert_eq!(
            main_checkout.project.as_ref().map(ProjectRef::id),
            worktree.project.as_ref().map(ProjectRef::id),
            "one project, which is exactly why the project alone is not enough"
        );
        assert!(
            !same_agent(&main_checkout, &worktree),
            "a linked worktree is another place to work"
        );
        // And the same tree really is the same agent, so the clause is
        // not simply refusing everything.
        assert!(same_agent(&main_checkout, &in_tree("/repo")));
    }

    /// Two sessions in one process do not collapse into one agent.
    ///
    /// "Absent on one side is not a mismatch" is what lets an MCP-first
    /// record be joined by its hooks half, and on its own it is not
    /// transitive: the record still named no session afterwards, so the
    /// *next* session in the same process matched it too and adopted
    /// the same identity. Learning the first verified session id is
    /// what closes it.
    #[tokio::test]
    async fn a_record_learns_its_session_so_the_next_one_cannot_take_it() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let me = std::process::id();
        let with_session = |name: &str, session: Option<&str>| {
            let mut spec = spec_here(name);
            if let Some(session) = session {
                spec.labels
                    .insert("session_id".to_owned(), session.to_owned());
            }
            spec
        };
        let register = async |spec: AgentSpec| match daemon
            .handle(Request::Register {
                spec,
                pid: Some(me),
                session: None,
            })
            .await
        {
            Response::Agent { agent } => agent,
            other => panic!("unexpected {other:?}"),
        };

        // MCP first, naming no session. Then session A joins it.
        let mcp = register(with_session("claude-code-45856", None)).await;
        let first = register(with_session("claude-aaaa", Some("session-a"))).await;
        assert_eq!(first.id, mcp.id, "the hooks half joins the MCP record");
        assert_eq!(
            first.spec.labels.get("session_id").map(String::as_str),
            Some("session-a"),
            "and the record learns whose session it is"
        );

        // A second session in the same process is a second agent.
        let second = register(with_session("claude-bbbb", Some("session-b"))).await;
        assert_ne!(
            second.id, mcp.id,
            "a different session must not inherit the first one's identity"
        );
        assert_eq!(lock(&daemon.state).registry.live().count(), 2);

        // And session A rejoining still finds its own.
        let again = register(with_session("claude-aaaa", Some("session-a"))).await;
        assert_eq!(again.id, first.id);
    }

    /// One identity survives both halves registering, and keeps what it
    /// was given.
    ///
    /// Named for what it proves. It drives the daemon through the
    /// registration order and asserts the record keeps its lease and its
    /// inbox — it does **not** run `McpServer::shutdown`, which lives in
    /// another crate and talks over a socket. That end-to-end trial
    /// against the real adapter is still owed and is not claimed here;
    /// what `shutdown` decides is covered separately, by unit test, in
    /// `mcp.rs`.
    #[tokio::test]
    async fn one_identity_survives_both_halves_and_keeps_what_it_was_given() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let me = std::process::id();
        let register = async |name: &str, session: Option<&str>| {
            let mut spec = spec_here(name);
            spec.labels.insert("via".to_owned(), "mcp".to_owned());
            if let Some(session) = session {
                spec.labels
                    .insert("session_id".to_owned(), session.to_owned());
            }
            match daemon
                .handle(Request::Register {
                    spec,
                    pid: Some(me),
                    session: None,
                })
                .await
            {
                Response::Agent { agent } => agent,
                other => panic!("unexpected {other:?}"),
            }
        };

        // MCP registers, hooks joins the same record, and the session
        // takes a lease and is sent something.
        let mcp = register("claude-code-45856", None).await;
        let hooks = register("claude-2c79ae10", Some("session-a")).await;
        assert_eq!(hooks.id, mcp.id);
        // The name that survives is the first registrar's, so the hooks
        // half is answered as `claude-code-45856` however it asked. That
        // is a race and it is deliberately not settled here; what
        // matters is that there is one identity, and the id is it.
        assert_eq!(hooks.spec.name, "claude-code-45856");
        assert!(matches!(
            claim(&daemon, &hooks.id.to_string(), "task:in-progress").await,
            Response::Lease { .. }
        ));

        // Something is sent to it, so there is state that a wrongly
        // timed deregistration would strand.
        let delivered = daemon
            .handle(Request::Send {
                from: hooks.id.to_string(),
                to: mcp.id.to_string(),
                kind: "chat".into(),
                payload: json!({"text": "still here?"}),
                reply_to: None,
            })
            .await;
        assert!(matches!(delivered, Response::Sent { .. }), "{delivered:?}");

        assert!(daemon.is_live(&mcp.id), "one identity, still running");
        assert_eq!(
            lock(&daemon.state).leases.by_holder(&mcp.id).len(),
            1,
            "still holding what it took"
        );
        assert_eq!(
            lock(&daemon.state)
                .inboxes
                .get(&mcp.id)
                .map_or(0, VecDeque::len),
            1,
            "and still holding what it was sent"
        );

        // Only the process ending ends the agent.
        let ended = lock(&daemon.state).deregister(&hooks.id.to_string());
        let _ = &ended;
        assert!(matches!(ended, Response::Agent { .. }));
        assert!(!daemon.is_live(&mcp.id));
    }

    /// A registration that names no session cannot pick between two.
    ///
    /// Absent-on-one-side is what lets an MCP server join its hooks
    /// half. Once a process holds two sessions there are two candidates
    /// and nothing to tell them apart, and taking the first the registry
    /// yields would hand the same transport a different identity from
    /// one run to the next.
    #[tokio::test]
    async fn an_unnamed_registration_refuses_to_guess_between_two_sessions() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let me = std::process::id();
        let register = async |name: &str, session: Option<&str>| {
            let mut spec = spec_here(name);
            if let Some(session) = session {
                spec.labels
                    .insert("session_id".to_owned(), session.to_owned());
            }
            daemon
                .handle(Request::Register {
                    spec,
                    pid: Some(me),
                    session: None,
                })
                .await
        };
        let named = async |r: Response| match r {
            Response::Agent { agent } => agent,
            other => panic!("unexpected {other:?}"),
        };
        let a = named(register("claude-aaaa", Some("session-a")).await).await;
        let b = named(register("claude-bbbb", Some("session-b")).await).await;
        assert_ne!(a.id, b.id);

        // Refused outright. Giving it a record of its own would be
        // giving this process a third sessionless wildcard — one that
        // matches every future session here and breeds more duplicates.
        let refused = register("claude-code-45856", None).await;
        let Response::Error { code, message, .. } = refused else {
            panic!("a registration that cannot be placed must not be placed anyway")
        };
        assert_eq!(code, ErrorCode::Invalid);
        assert!(message.contains("session_id"), "{message}");
        assert_eq!(
            lock(&daemon.state).registry.live().count(),
            2,
            "and no third record was created"
        );

        // The named sessions are untouched by the refusal and still
        // resolve to themselves.
        let Response::Agent { agent: a_again } = register("claude-aaaa", Some("session-a")).await
        else {
            panic!("a named session still knows which it is")
        };
        assert_eq!(a_again.id, a.id);
        let Response::Agent { agent: b_again } = register("claude-bbbb", Some("session-b")).await
        else {
            panic!("and so does the other")
        };
        assert_eq!(b_again.id, b.id);
        assert_eq!(lock(&daemon.state).registry.live().count(), 2);
    }

    /// Learning a session is written down before it is believed.
    #[tokio::test]
    async fn a_bound_session_is_stored_and_announced_before_it_is_answered() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let me = std::process::id();
        let register = async |name: &str, session: Option<&str>| {
            let mut spec = spec_here(name);
            if let Some(session) = session {
                spec.labels
                    .insert("session_id".to_owned(), session.to_owned());
            }
            daemon
                .handle(Request::Register {
                    spec,
                    pid: Some(me),
                    session: None,
                })
                .await
        };
        register("claude-code-45856", None).await;
        let joined = register("claude-aaaa", Some("session-a")).await;
        let Response::Agent { agent } = joined else {
            panic!("{joined:?}")
        };

        // On disk, not only in memory: a restart must not forget whose
        // session this was and let the next one adopt it.
        drop(daemon);
        let daemon = open(&dir);
        let reopened = lock(&daemon.state)
            .registry
            .get(&agent.id)
            .cloned()
            .unwrap();
        assert_eq!(
            reopened.spec.labels.get("session_id").map(String::as_str),
            Some("session-a")
        );
        // And it was announced, because every state change is.
        assert!(
            lock(&daemon.state)
                .store
                .recent_events(50)
                .unwrap()
                .iter()
                .any(
                    |e| matches!(&e.kind, EventKind::AgentSessionBound { session, .. }
                    if session == "session-a")
                ),
            "binding a session is a state change and says so"
        );
    }

    /// A binding that cannot be stored is not answered as if it were.
    ///
    /// The row and the event announcing it are one transaction, so a
    /// storage failure takes both or neither. What must never happen is
    /// the third outcome: memory saying the record has a session, the
    /// store saying it does not, and a restart picking whichever it
    /// reads first — at which point the next session in this process
    /// adopts an identity that is already spoken for.
    ///
    /// This test is here because the claim that it was one transaction
    /// was made before it was true, and nothing failed.
    #[tokio::test]
    async fn a_binding_that_cannot_be_stored_changes_nothing() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let me = std::process::id();
        let register = async |name: &str, session: Option<&str>| {
            let mut spec = spec_here(name);
            if let Some(session) = session {
                spec.labels
                    .insert("session_id".to_owned(), session.to_owned());
            }
            daemon
                .handle(Request::Register {
                    spec,
                    pid: Some(me),
                    session: None,
                })
                .await
        };
        let Response::Agent { agent: mcp } = register("claude-code-45856", None).await else {
            panic!("the first half registers")
        };
        let events_before = daemon.recent_events(200).len();

        // The event insert alone, not every write. That is the case
        // the old sequence got wrong: it wrote the row, failed on the
        // event, and moved memory on anyway. Rejecting everything
        // cannot tell that apart from taking neither.
        lock(&daemon.state)
            .store
            .reject_session_binding_event_for_test();
        let refused = register("claude-aaaa", Some("session-a")).await;
        assert!(
            matches!(
                refused,
                Response::Error {
                    code: ErrorCode::StorageUnavailable,
                    ..
                }
            ),
            "{refused:?}"
        );
        // The row must have gone back with the event. If the write had
        // been two commits, this is where the agent row would already
        // carry the session while the log knew nothing about it.
        assert!(
            lock(&daemon.state)
                .store
                .load_agents()
                .unwrap()
                .iter()
                .find(|a| a.id == mcp.id)
                .is_some_and(|a| !a.spec.labels.contains_key("session_id")),
            "the row was rolled back with the event it could not write"
        );

        // Memory did not move ahead of the store.
        assert!(
            lock(&daemon.state)
                .registry
                .get(&mcp.id)
                .is_some_and(|a| !a.spec.labels.contains_key("session_id")),
            "the binding was not published to memory"
        );
        assert_eq!(
            daemon.recent_events(200).len(),
            events_before,
            "and nothing was announced"
        );

        // Nor to disk: reopening finds the record exactly as it was, so
        // the next session in this process is still free to claim it.
        drop(daemon);
        let daemon = open(&dir);
        assert!(
            lock(&daemon.state)
                .registry
                .get(&mcp.id)
                .is_some_and(|a| !a.spec.labels.contains_key("session_id")),
            "and the store never took it either"
        );
    }

    /// An empty session label is an absent one.
    ///
    /// A record carrying `session_id: ""` would otherwise count as
    /// already knowing whose it is and never learn, leaving the wildcard
    /// open for the next session in the process.
    #[tokio::test]
    async fn an_empty_session_label_is_treated_as_no_session_at_all() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let me = std::process::id();
        let register = async |name: &str, session: &str| {
            let mut spec = spec_here(name);
            spec.labels
                .insert("session_id".to_owned(), session.to_owned());
            match daemon
                .handle(Request::Register {
                    spec,
                    pid: Some(me),
                    session: None,
                })
                .await
            {
                Response::Agent { agent } => agent,
                other => panic!("unexpected {other:?}"),
            }
        };
        let blank = register("claude-code-45856", "").await;
        let joined = register("claude-aaaa", "session-a").await;
        assert_eq!(joined.id, blank.id, "the empty label did not block joining");
        assert_eq!(
            joined.spec.labels.get("session_id").map(String::as_str),
            Some("session-a"),
            "and the empty label was replaced rather than kept"
        );
    }

    /// Duplicates that already exist are reported, not repaired.
    ///
    /// The obvious repair — retire the half with no leases and an empty
    /// inbox — is a guess. An idle transport is not an unused agent: a
    /// connected MCP server holds that id for its next call, and the
    /// record may own channel membership, pending questions, a journal
    /// cursor and observations, none of which appear as a lease or a
    /// queued message. So both are left alone and named.
    #[tokio::test]
    async fn an_existing_duplicate_is_reported_and_left_alone() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let me = std::process::id();

        let hooks = register_here(&daemon, "claude-2c79ae10", Some(me)).await;
        let stale = {
            // Built by hand, so everything registration would have
            // filled in has to be filled in here — including the
            // resolved workdir and the project, which identity compares.
            let mut record = AgentRecord::new(spec_here("claude-code-45856"), false, Utc::now());
            record.spec.workdir = hooks.spec.workdir.clone();
            record.project = hooks.project.clone();
            record.pid = Some(me);
            record.process_started_at = procinfo::start_time(me);
            record.status = AgentStatus::Running;
            lock(&daemon.state).registry.insert(record.clone()).unwrap();
            record
        };

        assert_eq!(
            daemon.duplicates(),
            vec![(hooks.id.clone(), stale.id.clone())],
            "the pair is found and named, oldest first"
        );
        #[derive(Clone, Default)]
        struct Captured(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
        impl std::io::Write for Captured {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let captured = Captured::default();
        let writer = captured.clone();
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_max_level(tracing::Level::WARN)
            .with_writer(move || writer.clone())
            .finish();
        let _subscriber = tracing::subscriber::set_default(subscriber);
        let warnings = || {
            String::from_utf8_lossy(&captured.0.lock().unwrap())
                .matches("two live records for one process")
                .count()
        };
        daemon.check_liveness();
        assert!(daemon.is_live(&hooks.id), "neither is retired:");
        assert!(
            daemon.is_live(&stale.id),
            "an empty inbox now is not evidence nothing will arrive"
        );
        assert_eq!(warnings(), 1);
        daemon.check_liveness();
        assert_eq!(
            warnings(),
            1,
            "unchanged duplicates must not grow the warning log"
        );
        lock(&daemon.state).registry.remove(&stale.id);
        daemon.check_liveness();
        lock(&daemon.state).registry.insert(stale).unwrap();
        daemon.check_liveness();
        assert_eq!(
            warnings(),
            2,
            "a pair that reappears should be announced again"
        );
    }

    /// Sharing a process is not the same as being the same agent.
    ///
    /// Every one of these was raised in review by the Codex session
    /// working alongside, and every one of them coalesced before it was.
    #[tokio::test]
    async fn one_process_is_not_one_agent_on_the_pid_alone() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let me = std::process::id();
        let mine = register_here(&daemon, "claude-2c79ae10", Some(me)).await;

        // A different runtime in the same process is a different agent:
        // a host that runs several kinds of session in one process would
        // otherwise have them all collapse into whichever arrived first.
        let elsewhere = {
            let mut spec = spec_here("codex-in-the-same-process");
            spec.runtime = "codex".to_owned();
            match daemon
                .handle(Request::Register {
                    spec,
                    pid: Some(me),
                    session: None,
                })
                .await
            {
                Response::Agent { agent } => agent,
                other => panic!("unexpected {other:?}"),
            }
        };
        assert_ne!(elsewhere.id, mine.id, "a different runtime is not us");

        // And a birth time nobody could read is not evidence that two
        // records are the same process. `None == None` is not proof.
        let unreadable = |name: &str| {
            let mut record = AgentRecord::new(spec(name), false, Utc::now());
            record.pid = Some(424_242);
            record.process_started_at = None;
            record.status = AgentStatus::Running;
            record
        };
        let first = match lock(&daemon.state).insert_record(unreadable("ghost-one")) {
            Response::Agent { agent } => agent,
            other => panic!("unexpected {other:?}"),
        };
        let second = match lock(&daemon.state).insert_record(unreadable("ghost-two")) {
            Response::Agent { agent } => agent,
            other => panic!("unexpected {other:?}"),
        };
        assert_ne!(
            first.id, second.id,
            "two unreadable start times are not one process"
        );
    }

    /// A pid no other agent in this test is using.
    ///
    /// One process is one agent now, so a fixture that handed several
    /// agents the test process's own pid was describing something that
    /// cannot happen — and got one agent back where it wanted three.
    /// These tests want distinct agents, not live processes; the ones
    /// that sweep for liveness ask for a real pid themselves.
    fn distinct_pid() -> Option<u32> {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);
        Some(NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
    }

    async fn register(daemon: &Arc<Daemon>, name: &str, pid: Option<u32>) -> AgentRecord {
        match daemon
            .handle(Request::Register {
                spec: spec(name),
                pid,
                session: None,
            })
            .await
        {
            Response::Agent { agent } => agent,
            other => panic!("unexpected {other:?}"),
        }
    }

    /// Register something that could be matched with another half.
    ///
    /// Identity compares the physical checkout, so only a registration
    /// that says where it is working can be. Kept separate from
    /// `register` because giving every fixture a directory would put
    /// them all in one project and change what the grouping and event
    /// tests are looking at.
    async fn register_here(daemon: &Arc<Daemon>, name: &str, pid: Option<u32>) -> AgentRecord {
        match daemon
            .handle(Request::Register {
                spec: spec_here(name),
                pid,
                session: None,
            })
            .await
        {
            Response::Agent { agent } => agent,
            other => panic!("unexpected {other:?}"),
        }
    }

    // ----- the human as an agent -----------------------------------------

    async fn me(daemon: &Arc<Daemon>) -> AgentRecord {
        match daemon.handle(Request::Me { workdir: None }).await {
            Response::Agent { agent } => agent,
            other => panic!("unexpected {other:?}"),
        }
    }

    #[tokio::test]
    async fn me_is_idempotent_and_outlives_the_liveness_check() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);

        let first = me(&daemon).await;
        assert_eq!(first.spec.name, agentdocker_core::HUMAN);
        assert_eq!(first.spec.runtime, agentdocker_core::HUMAN_RUNTIME);
        assert_eq!(first.pid, None, "a person is not a process");

        let again = me(&daemon).await;
        assert_eq!(again.id, first.id, "the same person, not a second one");

        // A record with no pid has nothing for liveness to check, so the
        // human is still here after a sweep that reaps dead processes.
        daemon.check_liveness();
        assert!(daemon.is_live(&first.id));
    }

    /// A window started from a Dock or a launcher inherits `/` as its
    /// working directory and reports it. Taking that at face value moved
    /// the person out of the project they were in, and everything that
    /// needs their checkout — `commit`, the journal digest — then had a
    /// filesystem root to work with instead of a repository.
    #[tokio::test]
    async fn a_client_with_no_working_directory_does_not_move_the_human_to_nowhere() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let workdir = dir.path().to_path_buf();
        // What the daemon stores is the resolved path, because identity
        // compares physical checkouts and `/var/folders` and
        // `/private/var/folders` are one directory spelled two ways.
        let resolved = project::canonical(&workdir);

        let Response::Agent { agent } = daemon
            .handle(Request::Me {
                workdir: Some(workdir.clone()),
            })
            .await
        else {
            panic!("me failed")
        };
        assert_eq!(agent.spec.workdir, Some(resolved.clone()));

        // The same person, reported from nowhere.
        let Response::Agent { agent } = daemon
            .handle(Request::Me {
                workdir: Some(PathBuf::from("/")),
            })
            .await
        else {
            panic!("me failed")
        };
        assert_eq!(
            agent.spec.workdir,
            Some(resolved.clone()),
            "the record they had is kept, not replaced with the root"
        );

        // And an actual directory still moves them, which is the whole
        // point of `me` being idempotent rather than write-once.
        let elsewhere = dir.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        let Response::Agent { agent } = daemon
            .handle(Request::Me {
                workdir: Some(elsewhere.clone()),
            })
            .await
        else {
            panic!("me failed")
        };
        assert_eq!(agent.spec.workdir, Some(elsewhere));
    }

    #[tokio::test]
    async fn ask_returns_the_answer_that_names_it() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let human = me(&daemon).await;
        let asker = register(&daemon, "worker", distinct_pid()).await;

        let answering = {
            let daemon = daemon.clone();
            tokio::spawn(async move {
                // Wait for the question to be recorded, then answer it.
                let id = loop {
                    let Response::Questions { questions } = daemon
                        .handle(Request::Questions {
                            agent: Some("user".to_owned()),
                        })
                        .await
                    else {
                        panic!("questions did not answer with questions")
                    };
                    if let Some(question) = questions.first() {
                        assert_eq!(question.text, "ship it?");
                        break question.id.clone();
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                };
                daemon
                    .handle(Request::Answer {
                        from: None,
                        message: id,
                        text: "yes".to_owned(),
                    })
                    .await
            })
        };

        let response = daemon
            .handle(Request::Ask {
                from: asker.id.to_string(),
                to: "user".to_owned(),
                question: "ship it?".to_owned(),
                timeout_secs: 10,
            })
            .await;
        let Response::Answer { from, text, .. } = response else {
            panic!("unexpected {response:?}")
        };
        assert_eq!(text, "yes");
        assert_eq!(from, human.id.to_string());
        answering.await.unwrap();

        // The question is no longer waiting for anyone.
        let Response::Questions { questions } =
            daemon.handle(Request::Questions { agent: None }).await
        else {
            panic!("questions did not answer with questions")
        };
        assert!(questions.is_empty(), "answered, so no longer open");

        // And the asker can still read the answer, because an answer is an
        // ordinary message as well as the end of a wait.
        let waiting = inbox(&daemon, asker.id.as_str(), true).await;
        assert_eq!(waiting.len(), 1);
        assert_eq!(waiting[0].kind, "answer");
    }

    #[tokio::test]
    async fn ask_times_out_rather_than_waiting_forever() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        me(&daemon).await;
        let asker = register(&daemon, "worker", distinct_pid()).await;

        let response = daemon
            .handle(Request::Ask {
                from: asker.id.to_string(),
                to: "user".to_owned(),
                question: "anyone there?".to_owned(),
                timeout_secs: 1,
            })
            .await;
        assert!(
            matches!(
                &response,
                Response::Error {
                    code: ErrorCode::Timeout,
                    ..
                }
            ),
            "unexpected {response:?}"
        );
        // Nobody is blocked on it any more, so it is not still open.
        let Response::Questions { questions } =
            daemon.handle(Request::Questions { agent: None }).await
        else {
            panic!("questions did not answer with questions")
        };
        assert!(questions.is_empty());
        // The question itself still reached the human's inbox: a timeout
        // is the asker giving up, not the message being withdrawn.
        let waiting = inbox(&daemon, "user", true).await;
        assert_eq!(waiting.len(), 1);
        assert_eq!(waiting[0].kind, "question");
    }

    #[tokio::test]
    async fn an_empty_question_is_refused_and_an_unknown_one_is_not_found() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        me(&daemon).await;

        let response = daemon
            .handle(Request::Ask {
                from: "user".to_owned(),
                to: "user".to_owned(),
                question: "   ".to_owned(),
                timeout_secs: 1,
            })
            .await;
        assert!(
            matches!(
                &response,
                Response::Error {
                    code: ErrorCode::Invalid,
                    ..
                }
            ),
            "unexpected {response:?}"
        );

        let response = daemon
            .handle(Request::Answer {
                from: None,
                message: MessageId::from("nosuchquestion".to_owned()),
                text: "hello?".to_owned(),
            })
            .await;
        assert!(
            matches!(
                &response,
                Response::Error {
                    code: ErrorCode::NotFound,
                    ..
                }
            ),
            "unexpected {response:?}"
        );
    }

    #[tokio::test]
    async fn questions_are_listed_only_for_whoever_was_asked() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        me(&daemon).await;
        let alpha = register(&daemon, "alpha", distinct_pid()).await;
        let beta = register(&daemon, "beta", distinct_pid()).await;

        // Two questions in flight, to different agents. Neither is
        // answered, so both asks are still waiting when we look.
        let to_human = {
            let daemon = daemon.clone();
            let from = alpha.id.to_string();
            tokio::spawn(async move {
                daemon
                    .handle(Request::Ask {
                        from,
                        to: "user".to_owned(),
                        question: "for the human".to_owned(),
                        timeout_secs: 30,
                    })
                    .await
            })
        };
        let to_beta = {
            let daemon = daemon.clone();
            let from = alpha.id.to_string();
            let beta = beta.id.to_string();
            tokio::spawn(async move {
                daemon
                    .handle(Request::Ask {
                        from,
                        to: beta,
                        question: "for beta".to_owned(),
                        timeout_secs: 30,
                    })
                    .await
            })
        };

        let both = loop {
            let Response::Questions { questions } =
                daemon.handle(Request::Questions { agent: None }).await
            else {
                panic!("questions did not answer with questions")
            };
            if questions.len() == 2 {
                break questions;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        };
        assert!(both[0].asked_at >= both[1].asked_at, "newest first");

        let Response::Questions { questions } = daemon
            .handle(Request::Questions {
                agent: Some("user".to_owned()),
            })
            .await
        else {
            panic!("questions did not answer with questions")
        };
        assert_eq!(questions.len(), 1);
        assert_eq!(questions[0].text, "for the human");

        to_human.abort();
        to_beta.abort();
    }

    #[tokio::test]
    async fn a_message_to_a_person_asks_for_a_notification_and_one_to_a_program_does_not() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let human = me(&daemon).await;
        let worker = register(&daemon, "worker", distinct_pid()).await;

        let (tx, mut notices) = mpsc::channel(8);
        lock(&daemon.state).notifier = Some(tx);

        daemon
            .handle(Request::Send {
                from: worker.id.to_string(),
                to: human.id.to_string(),
                kind: "chat".to_owned(),
                payload: json!({ "text": "look at this" }),
                reply_to: None,
            })
            .await;
        let notice = notices.try_recv().expect("a person is worth interrupting");
        assert_eq!(notice.from, "worker", "by name, not by id");
        assert_eq!(notice.kind, "chat");
        assert_eq!(notice.text, "look at this");
        assert_eq!(notice.target.agent, worker.id);
        let Response::Messages { messages } = daemon
            .handle(Request::Inbox {
                agent: human.id.to_string(),
                drain: false,
            })
            .await
        else {
            panic!("inbox")
        };
        assert_eq!(notice.target.message, messages[0].id);
        assert!(notice.target.channel.is_none());

        daemon
            .handle(Request::Send {
                from: "user".to_owned(),
                to: worker.id.to_string(),
                kind: "chat".to_owned(),
                payload: json!({ "text": "carry on" }),
                reply_to: None,
            })
            .await;
        assert!(
            notices.try_recv().is_err(),
            "a program has no desktop to interrupt"
        );
    }

    // ----- snapshot restore ----------------------------------------------

    /// A daemon that stops and comes back on the same home, as a restart
    /// looks from the store's point of view.
    async fn restart(dir: &TempDir, daemon: Arc<Daemon>) -> Arc<Daemon> {
        daemon.stop_all().await;
        drop(daemon);
        let daemon = open(dir);
        daemon.restore_agents().await;
        daemon
    }

    #[tokio::test]
    async fn a_restarted_daemon_brings_back_a_restorable_agent_under_its_own_identity() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let mut command = spec("keeper");
        command.workdir = Some(dir.path().to_path_buf());
        command.restore = true;
        command.command = vec!["sh".into(), "-c".into(), "sleep 30".into()];
        let Response::Agent { agent } = daemon.handle(Request::Run { spec: command }).await else {
            panic!("managed launch failed");
        };
        // Something worth keeping: a lease, and a journal cursor, both
        // keyed by this agent's id.
        assert!(matches!(
            claim(&daemon, "keeper", "task:the-work").await,
            Response::Lease { .. }
        ));
        let before = agent.pid;

        let daemon = restart(&dir, daemon).await;

        let Response::Agent { agent: after } = daemon
            .handle(Request::Inspect {
                agent: agent.id.to_string(),
            })
            .await
        else {
            panic!("the agent is gone")
        };
        assert_eq!(after.id, agent.id, "the same identity, not a new agent");
        assert_eq!(after.status, AgentStatus::Running);
        assert_ne!(after.pid, before, "a new process under the old identity");
        // The lease is still held, because its holder never stopped being
        // live and never stopped being this id.
        let leases = list_leases(&daemon).await;
        assert_eq!(leases.len(), 1);
        assert_eq!(leases[0].holder, agent.id);

        daemon.stop_all().await;
    }

    #[tokio::test]
    async fn an_agent_that_did_not_ask_to_be_restored_is_not() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let mut command = spec("ephemeral");
        command.workdir = Some(dir.path().to_path_buf());
        command.command = vec!["sh".into(), "-c".into(), "sleep 30".into()];
        let Response::Agent { agent } = daemon.handle(Request::Run { spec: command }).await else {
            panic!("managed launch failed");
        };

        let daemon = restart(&dir, daemon).await;

        // Nothing was relaunched, and the liveness sweep retires it.
        daemon.check_liveness();
        assert!(!daemon.is_live(&agent.id));
        daemon.stop_all().await;
    }

    #[tokio::test]
    async fn a_restored_agent_is_told_what_changed_while_it_was_down() {
        let dir = TempDir::new().unwrap();
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        let watched = work.join("parser.rs");
        std::fs::write(&watched, "fn parse() {}\n").unwrap();

        let daemon = open(&dir);
        let mut command = spec("reader");
        command.workdir = Some(work.clone());
        command.restore = true;
        command.command = vec!["sh".into(), "-c".into(), "sleep 30".into()];
        let Response::Agent { agent } = daemon.handle(Request::Run { spec: command }).await else {
            panic!("managed launch failed");
        };
        let observed = daemon
            .handle(Request::Observe {
                agent: agent.id.to_string(),
                paths: vec![watched.display().to_string()],
            })
            .await;
        assert!(matches!(observed, Response::Reads { .. }), "{observed:?}");
        let Response::Checkpoint { checkpoint } = daemon
            .handle(Request::Checkpoint {
                agent: agent.id.to_string(),
                key: "k1".to_owned(),
                task: "rewrite the tokenizer".to_owned(),
                assumptions: Vec::new(),
                next_steps: vec!["handle raw strings".to_owned()],
                release_leases: false,
            })
            .await
        else {
            panic!("checkpoint failed")
        };

        daemon.stop_all().await;
        drop(daemon);
        // Somebody edits the file while nothing is running.
        std::fs::write(&watched, "fn parse() { todo!() }\n").unwrap();
        let daemon = open(&dir);
        daemon.restore_agents().await;

        let waiting = inbox(&daemon, agent.id.as_str(), true).await;
        let brief = waiting
            .iter()
            .find(|m| m.kind == "restored")
            .expect("a restored agent is told it was restored");
        assert_eq!(brief.payload["restored"], json!(true));
        assert_eq!(brief.payload["reads"], json!(1));
        let stale = brief.payload["stale"].as_array().unwrap();
        assert_eq!(stale.len(), 1, "the edited path: {stale:?}");
        assert_eq!(brief.payload["checkpoint"], json!(checkpoint.id));
        assert_eq!(brief.payload["task"], json!("rewrite the tokenizer"));
        assert_eq!(brief.payload["next_steps"], json!(["handle raw strings"]));
        let text = brief.payload["text"].as_str().unwrap();
        assert!(text.contains("reread"), "and told to reread it: {text}");
        assert!(text.contains("reader"), "under its own name: {text}");
        assert!(
            text.contains("rewrite the tokenizer") && text.contains("handle raw strings"),
            "and what it was doing: {text}"
        );

        daemon.stop_all().await;
    }

    #[tokio::test]
    async fn a_restore_that_cannot_run_records_the_failure_and_does_not_stop_the_others() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);

        let mut broken = spec("broken");
        broken.workdir = Some(dir.path().to_path_buf());
        broken.restore = true;
        broken.command = vec!["sh".into(), "-c".into(), "sleep 30".into()];
        let Response::Agent { agent: broken } = daemon.handle(Request::Run { spec: broken }).await
        else {
            panic!("managed launch failed");
        };
        let mut fine = spec("fine");
        fine.workdir = Some(dir.path().to_path_buf());
        fine.restore = true;
        fine.command = vec!["sh".into(), "-c".into(), "sleep 30".into()];
        let Response::Agent { agent: fine } = daemon.handle(Request::Run { spec: fine }).await
        else {
            panic!("managed launch failed");
        };

        daemon.stop_all().await;
        drop(daemon);
        // The command is gone by the time the daemon comes back.
        let daemon = open(&dir);
        {
            let mut state = lock(&daemon.state);
            let record = state.registry.get_mut(&broken.id).unwrap();
            record.spec.command = vec!["/nonexistent/agentdocker-no-such-binary".into()];
            let record = record.clone();
            state.persist("agent", |store| store.upsert_agent(&record));
        }
        daemon.restore_agents().await;

        let Response::Agent { agent: broken } = daemon
            .handle(Request::Inspect {
                agent: broken.id.to_string(),
            })
            .await
        else {
            panic!("record gone")
        };
        assert!(
            matches!(&broken.status, AgentStatus::Failed { reason } if reason.contains("restored")),
            "unexpected {:?}",
            broken.status
        );
        assert!(daemon.is_live(&fine.id), "the other one still came back");
        daemon.stop_all().await;
    }

    #[tokio::test]
    async fn an_agent_stopped_on_purpose_is_not_brought_back() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let mut command = spec("finished");
        command.workdir = Some(dir.path().to_path_buf());
        command.restore = true;
        command.command = vec!["sh".into(), "-c".into(), "sleep 30".into()];
        let Response::Agent { agent } = daemon.handle(Request::Run { spec: command }).await else {
            panic!("managed launch failed");
        };
        daemon
            .handle(Request::Stop {
                agent: agent.id.to_string(),
                force: true,
            })
            .await;
        // The flag is cleared in the record itself, so the reason it will
        // not come back is visible rather than hidden.
        let Response::Agent { agent: stopped } = daemon
            .handle(Request::Inspect {
                agent: agent.id.to_string(),
            })
            .await
        else {
            panic!("record gone")
        };
        assert!(!stopped.spec.restore);

        let daemon = restart(&dir, daemon).await;
        daemon.check_liveness();
        assert!(!daemon.is_live(&agent.id));
        daemon.stop_all().await;
    }

    #[tokio::test]
    async fn a_clean_shutdown_hands_the_leases_back_and_says_it_did() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let mut command = spec("holder");
        command.workdir = Some(dir.path().to_path_buf());
        command.restore = true;
        command.command = vec!["sh".into(), "-c".into(), "sleep 30".into()];
        let Response::Agent { agent } = daemon.handle(Request::Run { spec: command }).await else {
            panic!("managed launch failed");
        };
        let Response::Lease { lease } = daemon
            .handle(Request::Claim {
                agent: agent.id.to_string(),
                resource: "task:the-refactor".to_owned(),
                mode: LeaseMode::Shared,
                amount: None,
                ttl_secs: 300,
                note: Some("halfway through".to_owned()),
                wait_secs: 0,
            })
            .await
        else {
            panic!("claim failed")
        };

        // A clean shutdown stops the agent, and stopping it releases the
        // lease — correctly, because nothing is working on it any more.
        daemon.stop_all().await;
        assert!(list_leases(&daemon).await.is_empty());
        drop(daemon);

        let daemon = open(&dir);
        daemon.restore_agents().await;

        let leases = list_leases(&daemon).await;
        assert_eq!(leases.len(), 1, "put back for the restored agent");
        assert_eq!(leases[0].holder, agent.id);
        assert_eq!(leases[0].resource, lease.resource);
        assert_eq!(leases[0].mode, LeaseMode::Shared, "as it was held");
        assert_eq!(leases[0].note.as_deref(), Some("halfway through"));
        assert_ne!(leases[0].id, lease.id, "a new lease, not a resurrected one");

        let waiting = inbox(&daemon, agent.id.as_str(), true).await;
        let brief = waiting.iter().find(|m| m.kind == "restored").unwrap();
        assert_eq!(
            brief.payload["reclaimed_leases"],
            json!([lease.resource.to_string()])
        );
        assert_eq!(
            brief.payload["leases"],
            json!([lease.resource.to_string()]),
            "listed once, not once per way it came back"
        );
        let text = brief.payload["text"].as_str().unwrap();
        assert_eq!(
            text.matches("task:the-refactor").count(),
            1,
            "and read once: {text}"
        );
        assert!(text.contains("You still hold"), "{text}");
        daemon.stop_all().await;
    }

    #[tokio::test]
    async fn a_question_has_to_reach_somebody_who_can_answer_it() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        me(&daemon).await;
        // A topic delivers only to live subscribers and queues for
        // nobody, so a question put to one could wait out its whole
        // timeout without ever appearing in `questions`.
        let response = daemon
            .handle(Request::Ask {
                from: "user".into(),
                to: "topic:reviews".into(),
                question: "anyone?".into(),
                timeout_secs: 1,
            })
            .await;
        assert!(
            matches!(
                &response,
                Response::Error {
                    code: ErrorCode::Invalid,
                    ..
                }
            ),
            "unexpected {response:?}"
        );
        let Response::Questions { questions } =
            daemon.handle(Request::Questions { agent: None }).await
        else {
            panic!("questions did not answer with questions")
        };
        assert!(questions.is_empty(), "and nothing was left waiting");
    }

    /// The daemon has to know about a question before anyone can see it.
    /// A recipient that answers the instant the message arrives must not
    /// be told there is no such question.
    #[tokio::test]
    async fn an_answer_that_races_the_question_still_lands() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let human = me(&daemon).await;
        let asker = register(&daemon, "worker", distinct_pid()).await;

        let mut messages = lock(&daemon.state).bus.subscribe();
        let racing = {
            let daemon = daemon.clone();
            tokio::spawn(async move {
                loop {
                    let Ok(envelope) = messages.recv().await else {
                        return None;
                    };
                    if envelope.kind == "question" {
                        // No pause: whatever the daemon knows at the
                        // moment the message goes out is all it knows.
                        return Some(
                            daemon
                                .handle(Request::Answer {
                                    from: Some(human.id.to_string()),
                                    message: envelope.id,
                                    text: "at once".to_owned(),
                                })
                                .await,
                        );
                    }
                }
            })
        };

        let response = daemon
            .handle(Request::Ask {
                from: asker.id.to_string(),
                to: "user".into(),
                question: "now?".into(),
                timeout_secs: 10,
            })
            .await;
        let answered = racing.await.unwrap().expect("the question was published");
        assert!(
            matches!(answered, Response::Sent { .. }),
            "the answer found its question: {answered:?}"
        );
        let Response::Answer { text, .. } = response else {
            panic!("unexpected {response:?}")
        };
        assert_eq!(text, "at once");
    }

    // ----- the wait queue, deadlock, and derived activity ----------------

    async fn activity_of(daemon: &Arc<Daemon>, agent: &str) -> Activity {
        let Response::Activity { activity } = daemon
            .handle(Request::Activity {
                agent: Some(agent.to_owned()),
                project: None,
                all: true,
            })
            .await
        else {
            panic!("activity did not answer with activity")
        };
        activity.into_iter().next().expect("one agent").activity
    }

    async fn claim_waiting(
        daemon: &Arc<Daemon>,
        agent: &str,
        resource: &str,
        wait_secs: u64,
    ) -> Response {
        daemon
            .handle(Request::Claim {
                agent: agent.to_owned(),
                resource: resource.to_owned(),
                mode: LeaseMode::Exclusive,
                amount: None,
                ttl_secs: 60,
                note: None,
                wait_secs,
            })
            .await
    }

    /// Wait until the queue is this deep, so a test never races the tasks
    /// it started.
    async fn queued(daemon: &Arc<Daemon>, want: usize) -> Vec<agentdocker_core::Waiter> {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let Response::Waiting { waiting } = daemon.handle(Request::Waiting).await else {
                    panic!("waiting did not answer with waiters")
                };
                if waiting.len() == want {
                    return waiting;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the queue reached the expected depth")
    }

    async fn release(daemon: &Arc<Daemon>, agent: &str, lease: LeaseId) {
        daemon
            .handle(Request::Release {
                agent: agent.to_owned(),
                lease,
                summary: None,
                summary_source: agentdocker_core::SummarySource::Explicit,
            })
            .await;
    }

    #[tokio::test]
    async fn the_agent_that_waited_longest_gets_the_lease() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let holder = register(&daemon, "holder", distinct_pid()).await;
        let first = register(&daemon, "first", distinct_pid()).await;
        let second = register(&daemon, "second", distinct_pid()).await;

        let Response::Lease { lease } = claim(&daemon, "holder", "task:contested").await else {
            panic!("the first claim should succeed")
        };

        let first_claim = {
            let daemon = daemon.clone();
            tokio::spawn(async move { claim_waiting(&daemon, "first", "task:contested", 10).await })
        };
        // Queued before the second one arrives, so the arrival order
        // under test is the one intended rather than whichever task the
        // runtime happened to poll first.
        let queue = queued(&daemon, 1).await;
        assert_eq!(queue[0].agent, first.id);

        let second_claim = {
            let daemon = daemon.clone();
            tokio::spawn(
                async move { claim_waiting(&daemon, "second", "task:contested", 10).await },
            )
        };
        let queue = queued(&daemon, 2).await;
        assert_eq!(queue[1].agent, second.id, "and behind the first");

        release(&daemon, holder.id.as_str(), lease.id).await;

        let won = tokio::time::timeout(std::time::Duration::from_secs(5), first_claim)
            .await
            .expect("the first waiter finished")
            .unwrap();
        let Response::Lease { lease: won } = won else {
            panic!("the older waiter should have won: {won:?}")
        };
        assert_eq!(won.holder, first.id, "not whoever raced fastest");
        second_claim.abort();
    }

    #[tokio::test]
    async fn a_waiter_that_goes_away_does_not_hold_the_queue() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let holder = register(&daemon, "holder", distinct_pid()).await;
        register(&daemon, "leaver", distinct_pid()).await;
        let stayer = register(&daemon, "stayer", distinct_pid()).await;

        let Response::Lease { lease } = claim(&daemon, "holder", "task:contested").await else {
            panic!("the first claim should succeed")
        };
        let leaving = {
            let daemon = daemon.clone();
            tokio::spawn(
                async move { claim_waiting(&daemon, "leaver", "task:contested", 30).await },
            )
        };
        queued(&daemon, 1).await;
        let staying = {
            let daemon = daemon.clone();
            tokio::spawn(
                async move { claim_waiting(&daemon, "stayer", "task:contested", 30).await },
            )
        };
        queued(&daemon, 2).await;

        // The client at the head of the queue disconnects: its future is
        // dropped, which must give up its place rather than starve the
        // one behind it.
        leaving.abort();
        queued(&daemon, 1).await;
        release(&daemon, holder.id.as_str(), lease.id).await;

        let won = tokio::time::timeout(std::time::Duration::from_secs(5), staying)
            .await
            .expect("the remaining waiter finished")
            .unwrap();
        let Response::Lease { lease: won } = won else {
            panic!("the waiter behind it should have won: {won:?}")
        };
        assert_eq!(won.holder, stayer.id);
    }

    #[tokio::test]
    async fn a_claim_that_would_close_a_ring_is_refused_with_the_ring() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let alpha = register(&daemon, "alpha", distinct_pid()).await;
        let beta = register(&daemon, "beta", distinct_pid()).await;

        // alpha holds x, beta holds y.
        assert!(matches!(
            claim(&daemon, "alpha", "task:x").await,
            Response::Lease { .. }
        ));
        assert!(matches!(
            claim(&daemon, "beta", "task:y").await,
            Response::Lease { .. }
        ));
        // alpha waits for y.
        let alpha_waits = {
            let daemon = daemon.clone();
            tokio::spawn(async move { claim_waiting(&daemon, "alpha", "task:y", 30).await })
        };
        queued(&daemon, 1).await;

        // beta asking for x closes the ring, so it is refused at once
        // rather than after both TTLs.
        let refused = claim_waiting(&daemon, "beta", "task:x", 30).await;
        let Response::Error {
            code,
            message,
            details,
        } = refused
        else {
            panic!("beta should have been refused")
        };
        assert_eq!(code, ErrorCode::Deadlock);
        assert!(message.contains("deadlock"), "{message}");
        let cycle = details.expect("a cycle").get("cycle").cloned().unwrap();
        let cycle: Vec<agentdocker_core::Blocked> = serde_json::from_value(cycle).unwrap();
        assert_eq!(cycle.len(), 2, "beta → alpha → beta: {cycle:?}");
        assert_eq!(cycle[0].agent, beta.id);
        assert_eq!(cycle[0].held_by, alpha.id);
        assert_eq!(cycle[1].agent, alpha.id);
        assert_eq!(cycle[1].held_by, beta.id);
        // And the refusal changed nothing: alpha is still waiting.
        assert_eq!(queued(&daemon, 1).await.len(), 1);
        alpha_waits.abort();
    }

    #[tokio::test]
    async fn plain_contention_still_waits_rather_than_being_called_a_deadlock() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        register(&daemon, "holder", distinct_pid()).await;
        register(&daemon, "waiter", distinct_pid()).await;
        assert!(matches!(
            claim(&daemon, "holder", "task:x").await,
            Response::Lease { .. }
        ));
        // The holder wants nothing, so there is no cycle: the waiter
        // waits and then reports an ordinary conflict.
        let response = claim_waiting(&daemon, "waiter", "task:x", 1).await;
        assert!(
            matches!(
                &response,
                Response::Error {
                    code: ErrorCode::Conflict,
                    ..
                }
            ),
            "unexpected {response:?}"
        );
    }

    #[tokio::test]
    async fn activity_says_what_an_agent_is_blocked_on_and_who_has_it() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let holder = register(&daemon, "holder", distinct_pid()).await;
        let waiter = register(&daemon, "waiter", distinct_pid()).await;

        // Registration proves presence, not work.
        assert!(matches!(
            activity_of(&daemon, "holder").await,
            Activity::Unknown
        ));

        assert!(matches!(
            claim(&daemon, "holder", "task:the-work").await,
            Response::Lease { .. }
        ));
        let blocked = {
            let daemon = daemon.clone();
            tokio::spawn(async move { claim_waiting(&daemon, "waiter", "task:the-work", 30).await })
        };
        queued(&daemon, 1).await;

        let activity = activity_of(&daemon, waiter.id.as_str()).await;
        let Activity::Blocked {
            resource, held_by, ..
        } = activity
        else {
            panic!("the waiter should be blocked: {activity:?}")
        };
        assert_eq!(resource.as_str(), "task:the-work", "on a named resource");
        assert_eq!(held_by, vec![holder.id.clone()], "held by a named agent");

        // Blocked sorts first, because it is the one somebody has to do
        // something about.
        let Response::Activity { activity } = daemon
            .handle(Request::Activity {
                agent: None,
                project: None,
                all: false,
            })
            .await
        else {
            panic!("activity did not answer with activity")
        };
        assert_eq!(activity.len(), 2);
        assert_eq!(activity[0].agent, waiter.id);
        assert_eq!(activity[0].activity.label(), "blocked");

        blocked.abort();
    }

    #[tokio::test]
    async fn a_quiet_agent_is_unknown_and_a_finished_one_is_finished() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let quiet = register(&daemon, "quiet", distinct_pid()).await;
        // Backdate its last contact past the activity window.
        {
            let mut state = lock(&daemon.state);
            let record = state.registry.get_mut(&quiet.id).unwrap();
            record.last_seen = Utc::now() - Duration::hours(1);
        }
        assert!(matches!(
            activity_of(&daemon, quiet.id.as_str()).await,
            Activity::Unknown
        ));

        daemon
            .handle(Request::Deregister {
                agent: quiet.id.to_string(),
            })
            .await;
        assert_eq!(
            activity_of(&daemon, quiet.id.as_str()).await,
            Activity::Finished
        );
    }

    #[tokio::test]
    async fn provider_activity_is_ordered_expires_and_commits_with_its_event() {
        use agentdocker_core::{ActivityObservation, ReportedActivity};
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let agent = register(&daemon, "activity-fixture", None).await;
        let now = Utc::now();
        let working = ActivityObservation {
            activity: ReportedActivity::Working,
            observed_at: now - Duration::seconds(2),
        };
        let idle = ActivityObservation {
            activity: ReportedActivity::Idle,
            observed_at: now - Duration::seconds(1),
        };
        let mut events = daemon.subscribe_events();
        {
            let mut state = lock(&daemon.state);
            for observation in [&working, &idle] {
                assert!(matches!(
                    state.report_activity(agent.id.as_str(), observation.clone(), now),
                    Response::Ok
                ));
                assert!(matches!(
                    events.try_recv().unwrap().kind,
                    EventKind::AgentActivityReported { .. }
                ));
            }
            assert!(matches!(
                state.report_activity(agent.id.as_str(), working.clone(), now),
                Response::Ok
            ));
            assert!(
                events.try_recv().is_err(),
                "late reports do not revive a stopped turn"
            );
            let record = state.registry.get(&agent.id).unwrap();
            assert_eq!(record.reported_activity, Some(idle.clone()));
            assert_eq!(
                state.store.load_agents().unwrap()[0].reported_activity,
                Some(idle)
            );
            assert!(matches!(
                state.activity_of(record, now),
                Activity::Idle { .. }
            ));
            assert_eq!(
                state.activity_of(record, now + Duration::minutes(5)),
                Activity::Unknown
            );
            assert!(matches!(
                state.report_activity(agent.id.as_str(), working, now + Duration::minutes(6)),
                Response::Error {
                    code: ErrorCode::Invalid,
                    ..
                }
            ));
        }
        assert!(events.try_recv().is_err());
    }

    #[tokio::test]
    async fn failed_activity_write_publishes_no_state_or_event() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let agent = register(&daemon, "activity-write-failure", None).await;
        let before = daemon.recent_events(100);
        let mut events = daemon.subscribe_events();
        lock(&daemon.state).store.reject_agent_writes_for_test();
        let response = daemon
            .handle(Request::ReportActivity {
                agent: agent.id.to_string(),
                observation: agentdocker_core::ActivityObservation {
                    activity: agentdocker_core::ReportedActivity::Working,
                    observed_at: Utc::now(),
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
        assert_eq!(state.registry.get(&agent.id), Some(&agent));
        assert_eq!(state.store.load_agents().unwrap(), [agent]);
        drop(state);
        assert_eq!(daemon.recent_events(100), before);
        assert!(events.try_recv().is_err());
    }

    #[tokio::test]
    async fn delayed_idle_report_cannot_become_work_when_it_expires() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let agent = register(&daemon, "delayed-report", None).await;
        let now = Utc::now();
        let mut state = lock(&daemon.state);
        let record = state.registry.get_mut(&agent.id).unwrap();
        record.created_at = now - Duration::hours(1);
        record.last_seen = record.created_at;
        state.report_activity(
            agent.id.as_str(),
            agentdocker_core::ActivityObservation {
                activity: agentdocker_core::ReportedActivity::Idle,
                observed_at: now - Duration::seconds(299),
            },
            now,
        );
        let record = state.registry.get(&agent.id).unwrap();
        assert!(matches!(
            state.activity_of(record, now),
            Activity::Idle { .. }
        ));
        assert_eq!(
            state.activity_of(record, now + Duration::seconds(1)),
            Activity::Unknown
        );
        state.registry.get_mut(&agent.id).unwrap().last_seen = now;
        assert!(
            matches!(
                state.activity_of(state.registry.get(&agent.id).unwrap(), now),
                Activity::Working { .. }
            ),
            "newer coordination supersedes the old idle observation"
        );
    }

    #[tokio::test]
    async fn a_deadlock_refusal_says_the_wait_ended() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        register(&daemon, "alpha", distinct_pid()).await;
        register(&daemon, "beta", distinct_pid()).await;
        assert!(matches!(
            claim(&daemon, "alpha", "task:x").await,
            Response::Lease { .. }
        ));
        assert!(matches!(
            claim(&daemon, "beta", "task:y").await,
            Response::Lease { .. }
        ));
        let alpha_waits = {
            let daemon = daemon.clone();
            tokio::spawn(async move { claim_waiting(&daemon, "alpha", "task:y", 30).await })
        };
        queued(&daemon, 1).await;

        let mut events = daemon.subscribe_events();
        let refused = claim_waiting(&daemon, "beta", "task:x", 30).await;
        assert!(matches!(
            &refused,
            Response::Error {
                code: ErrorCode::Deadlock,
                ..
            }
        ));

        // A wait that never began still ended, and `lease_wait_ended` is
        // the event subscribers are told to expect for every outcome —
        // including the one that was refused before it could queue.
        let mut saw_deadlock = false;
        let mut saw_ended = false;
        while let Ok(event) = events.try_recv() {
            match event.kind {
                EventKind::LeaseDeadlock { .. } => saw_deadlock = true,
                EventKind::LeaseWaitEnded {
                    outcome: agentdocker_core::WaitOutcome::Deadlock,
                    ..
                } => saw_ended = true,
                _ => {}
            }
        }
        assert!(saw_deadlock, "the cycle itself is announced");
        assert!(saw_ended, "and so is the wait ending on it");
        alpha_waits.abort();
    }

    // ----- contests -------------------------------------------------------

    /// A registered agent in its own checkout, which is what a contest
    /// entrant is.
    async fn entrant(daemon: &Arc<Daemon>, root: &Path, name: &str) -> AgentRecord {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("work.txt"), name).unwrap();
        let mut spec = spec(name);
        spec.workdir = Some(dir);
        register_spec(daemon, spec).await
    }

    /// Run a validation for this agent and return its id.
    async fn validate(daemon: &Arc<Daemon>, agent: &str, command: &str) -> Response {
        daemon
            .handle(Request::Validate {
                agent: agent.to_owned(),
                command: vec!["sh".into(), "-c".into(), command.to_owned()],
                timeout_secs: 30,
            })
            .await
    }

    async fn validation_id(daemon: &Arc<Daemon>, agent: &str, command: &str) -> String {
        match validate(daemon, agent, command).await {
            Response::Validation { validation, .. } => validation.id,
            other => panic!("validate failed: {other:?}"),
        }
    }

    async fn open_contest(
        daemon: &Arc<Daemon>,
        agent: &str,
        measure: Measure,
        noise: f64,
        entrants: Vec<String>,
    ) -> Contest {
        let response = daemon
            .handle(Request::ContestOpen {
                agent: agent.to_owned(),
                project: None,
                task: "make it faster".to_owned(),
                metric: Metric {
                    measure,
                    direction: agentdocker_core::contest::Direction::Lower,
                    noise,
                },
                entrants,
                channel: true,
            })
            .await;
        match response {
            Response::Contest { contest, .. } => contest,
            other => panic!("contest_open failed: {other:?}"),
        }
    }

    #[tokio::test]
    async fn an_entry_needs_a_passing_validation_of_its_own() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let root = dir.path().join("work");
        let alpha = entrant(&daemon, &root, "alpha").await;
        let beta = entrant(&daemon, &root, "beta").await;
        let contest = open_contest(
            &daemon,
            alpha.id.as_str(),
            Measure::Reported {
                name: "allocations".to_owned(),
            },
            0.0,
            vec![beta.id.to_string()],
        )
        .await;

        // A failing run is not evidence of anything.
        let failed = validation_id(&daemon, alpha.id.as_str(), "exit 1").await;
        let response = daemon
            .handle(Request::ContestSubmit {
                agent: alpha.id.to_string(),
                contest: contest.id.clone(),
                validation: failed,
                score: Some(1.0),
            })
            .await;
        assert!(
            matches!(
                &response,
                Response::Error {
                    code: ErrorCode::Invalid,
                    message,
                    ..
                } if message.contains("did not pass")
            ),
            "unexpected {response:?}"
        );

        // Nor is somebody else's passing run.
        let borrowed = validation_id(&daemon, beta.id.as_str(), "true").await;
        let response = daemon
            .handle(Request::ContestSubmit {
                agent: alpha.id.to_string(),
                contest: contest.id.clone(),
                validation: borrowed,
                score: Some(1.0),
            })
            .await;
        assert!(
            matches!(
                &response,
                Response::Error {
                    code: ErrorCode::Forbidden,
                    ..
                }
            ),
            "unexpected {response:?}"
        );

        // Its own passing run is.
        let mine = validation_id(&daemon, alpha.id.as_str(), "true").await;
        let Response::Contest { contest, .. } = daemon
            .handle(Request::ContestSubmit {
                agent: alpha.id.to_string(),
                contest: contest.id.clone(),
                validation: mine.clone(),
                score: Some(7.0),
            })
            .await
        else {
            panic!("a passing validation of its own should be accepted")
        };
        assert_eq!(contest.entries.len(), 1);
        assert_eq!(contest.entries[0].validation, mine);
        assert_eq!(contest.entries[0].score, 7.0);
    }

    #[tokio::test]
    async fn a_measured_contest_ignores_what_the_entrant_says_the_score_was() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let root = dir.path().join("work");
        let alpha = entrant(&daemon, &root, "alpha").await;
        let contest = open_contest(
            &daemon,
            alpha.id.as_str(),
            Measure::ValidationSeconds,
            0.0,
            vec![],
        )
        .await;

        let mine = validation_id(&daemon, alpha.id.as_str(), "true").await;
        let Response::Contest { contest, .. } = daemon
            .handle(Request::ContestSubmit {
                agent: alpha.id.to_string(),
                contest: contest.id.clone(),
                validation: mine,
                // A flattering claim, which must be ignored: the daemon
                // timed the run itself.
                score: Some(-999.0),
            })
            .await
        else {
            panic!("submit failed")
        };
        let score = contest.entries[0].score;
        assert!(
            (0.0..30.0).contains(&score),
            "the daemon's own timing, not the claim: {score}"
        );
    }

    #[tokio::test]
    async fn a_reported_contest_needs_a_number() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let root = dir.path().join("work");
        let alpha = entrant(&daemon, &root, "alpha").await;
        let contest = open_contest(
            &daemon,
            alpha.id.as_str(),
            Measure::Reported {
                name: "allocations".to_owned(),
            },
            0.0,
            vec![],
        )
        .await;
        let mine = validation_id(&daemon, alpha.id.as_str(), "true").await;
        let response = daemon
            .handle(Request::ContestSubmit {
                agent: alpha.id.to_string(),
                contest: contest.id,
                validation: mine,
                score: None,
            })
            .await;
        assert!(
            matches!(
                &response,
                Response::Error {
                    code: ErrorCode::Invalid,
                    message,
                    ..
                } if message.contains("allocations")
            ),
            "unexpected {response:?}"
        );
    }

    #[tokio::test]
    async fn a_tie_cannot_be_closed_by_the_metric_and_a_clear_win_can() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let root = dir.path().join("work");
        let alpha = entrant(&daemon, &root, "alpha").await;
        let beta = entrant(&daemon, &root, "beta").await;
        let contest = open_contest(
            &daemon,
            alpha.id.as_str(),
            Measure::Reported {
                name: "allocations".to_owned(),
            },
            // A generous noise floor, so two close numbers tie.
            5.0,
            vec![beta.id.to_string()],
        )
        .await;

        for (agent, score) in [(&alpha, 100.0), (&beta, 102.0)] {
            let evidence = validation_id(&daemon, agent.id.as_str(), "true").await;
            daemon
                .handle(Request::ContestSubmit {
                    agent: agent.id.to_string(),
                    contest: contest.id.clone(),
                    validation: evidence,
                    score: Some(score),
                })
                .await;
        }

        // Two apart with a floor of five: the metric has said all it can.
        let refused = daemon
            .handle(Request::ContestClose {
                agent: alpha.id.to_string(),
                contest: contest.id.clone(),
                winner: None,
                resolution: None,
            })
            .await;
        assert!(
            matches!(
                &refused,
                Response::Error {
                    code: ErrorCode::Conflict,
                    message,
                    ..
                } if message.contains("noise floor")
            ),
            "unexpected {refused:?}"
        );

        // Review is the tie-break, so an explicit winner settles it.
        let Response::Contest { contest, standing } = daemon
            .handle(Request::ContestClose {
                agent: alpha.id.to_string(),
                contest: contest.id.clone(),
                winner: Some(beta.id.to_string()),
                resolution: Some("clearer, and the numbers were a tie".to_owned()),
            })
            .await
        else {
            panic!("an explicit winner should settle it")
        };
        assert_eq!(contest.winner, Some(beta.id.clone()));
        assert!(!contest.is_open());
        assert!(matches!(standing, Standing::Settled { .. }));

        // And a closed contest takes nothing more.
        let evidence = validation_id(&daemon, alpha.id.as_str(), "true").await;
        let response = daemon
            .handle(Request::ContestSubmit {
                agent: alpha.id.to_string(),
                contest: contest.id,
                validation: evidence,
                score: Some(1.0),
            })
            .await;
        assert!(
            matches!(&response, Response::Error { .. }),
            "unexpected {response:?}"
        );
    }

    #[tokio::test]
    async fn a_clear_winner_needs_no_arbiter_and_a_stranger_cannot_enter_by_submitting() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let root = dir.path().join("work");
        let alpha = entrant(&daemon, &root, "alpha").await;
        let beta = entrant(&daemon, &root, "beta").await;
        let stranger = entrant(&daemon, &root, "stranger").await;
        let contest = open_contest(
            &daemon,
            alpha.id.as_str(),
            Measure::Reported {
                name: "allocations".to_owned(),
            },
            1.0,
            vec![beta.id.to_string()],
        )
        .await;
        // A channel came with it, because a tie has to be argued
        // somewhere and asking for a room afterwards looks like a
        // rematch.
        assert!(contest.channel.is_some());

        for (agent, score) in [(&alpha, 100.0), (&beta, 40.0)] {
            let evidence = validation_id(&daemon, agent.id.as_str(), "true").await;
            daemon
                .handle(Request::ContestSubmit {
                    agent: agent.id.to_string(),
                    contest: contest.id.clone(),
                    validation: evidence,
                    score: Some(score),
                })
                .await;
        }

        // Somebody who never entered cannot submit.
        let evidence = validation_id(&daemon, stranger.id.as_str(), "true").await;
        let response = daemon
            .handle(Request::ContestSubmit {
                agent: stranger.id.to_string(),
                contest: contest.id.clone(),
                validation: evidence,
                score: Some(1.0),
            })
            .await;
        assert!(
            matches!(
                &response,
                Response::Error {
                    code: ErrorCode::Forbidden,
                    ..
                }
            ),
            "unexpected {response:?}"
        );

        // Sixty apart with a floor of one: no arbiter needed.
        let Response::Contest { contest, standing } = daemon
            .handle(Request::ContestClose {
                agent: alpha.id.to_string(),
                contest: contest.id.clone(),
                winner: None,
                resolution: None,
            })
            .await
        else {
            panic!("a clear win should close on the ranking")
        };
        assert_eq!(contest.winner, Some(beta.id.clone()));
        assert!(matches!(standing, Standing::Settled { winner, .. } if winner == beta.id));
    }

    #[tokio::test]
    async fn entering_admits_a_latecomer_to_the_contests_channel() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let root = dir.path().join("work");
        let alpha = entrant(&daemon, &root, "alpha").await;
        let latecomer = entrant(&daemon, &root, "latecomer").await;
        let contest = open_contest(
            &daemon,
            alpha.id.as_str(),
            Measure::ValidationSeconds,
            0.0,
            vec![],
        )
        .await;
        let channel = contest.channel.clone().expect("a room");

        let Response::Contest { contest, .. } = daemon
            .handle(Request::ContestEnter {
                agent: latecomer.id.to_string(),
                contest: contest.id,
            })
            .await
        else {
            panic!("entering failed")
        };
        assert!(contest.has(&latecomer.id));
        let room = lock(&daemon.state)
            .channels
            .get(&channel)
            .cloned()
            .expect("the channel is still there");
        assert!(
            room.has(&latecomer.id),
            "and the latecomer can argue its own case"
        );
    }

    #[tokio::test]
    async fn only_the_people_in_a_contest_can_close_it() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let root = dir.path().join("work");
        let alpha = entrant(&daemon, &root, "alpha").await;
        let beta = entrant(&daemon, &root, "beta").await;
        let outsider = entrant(&daemon, &root, "outsider").await;
        let contest = open_contest(
            &daemon,
            alpha.id.as_str(),
            Measure::ValidationSeconds,
            0.0,
            vec![beta.id.to_string()],
        )
        .await;
        let evidence = validation_id(&daemon, beta.id.as_str(), "true").await;
        daemon
            .handle(Request::ContestSubmit {
                agent: beta.id.to_string(),
                contest: contest.id.clone(),
                validation: evidence,
                score: None,
            })
            .await;

        // Closing settles somebody's work, so an agent that is neither
        // the opener nor an entrant may not do it.
        let refused = daemon
            .handle(Request::ContestClose {
                agent: outsider.id.to_string(),
                contest: contest.id.clone(),
                winner: Some(beta.id.to_string()),
                resolution: None,
            })
            .await;
        assert!(
            matches!(
                &refused,
                Response::Error {
                    code: ErrorCode::Forbidden,
                    ..
                }
            ),
            "unexpected {refused:?}"
        );

        // An entrant can, and so could the opener.
        let Response::Contest { contest, .. } = daemon
            .handle(Request::ContestClose {
                agent: beta.id.to_string(),
                contest: contest.id.clone(),
                winner: None,
                resolution: None,
            })
            .await
        else {
            panic!("an entrant should be able to close it")
        };
        assert_eq!(contest.winner, Some(beta.id));
    }

    #[tokio::test]
    async fn one_contest_is_looked_up_rather_than_scanned_for() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let root = dir.path().join("work");
        let alpha = entrant(&daemon, &root, "alpha").await;
        let wanted = open_contest(
            &daemon,
            alpha.id.as_str(),
            Measure::ValidationSeconds,
            0.0,
            vec![],
        )
        .await;
        open_contest(
            &daemon,
            alpha.id.as_str(),
            Measure::ValidationSeconds,
            0.0,
            vec![],
        )
        .await;

        let Response::Contests { contests } = daemon
            .handle(Request::Contests {
                contest: Some(wanted.id.clone()),
                project: None,
                agent: None,
                all: true,
            })
            .await
        else {
            panic!("contests did not answer with contests")
        };
        assert_eq!(contests.len(), 1, "the one asked for, not both");
        assert_eq!(contests[0].id, wanted.id);

        let missing = daemon
            .handle(Request::Contests {
                contest: Some(agentdocker_core::ContestId::from(
                    "nosuchcontest".to_owned(),
                )),
                project: None,
                agent: None,
                all: true,
            })
            .await;
        assert!(
            matches!(
                &missing,
                Response::Error {
                    code: ErrorCode::NotFound,
                    ..
                }
            ),
            "a lookup that finds nothing says so: {missing:?}"
        );
    }

    #[tokio::test]
    async fn an_agent_says_which_multiplexer_it_lives_in_and_the_daemon_keeps_it() {
        use agentdocker_core::multiplexer::{Evidence, Session};
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);

        // A client registering itself is inside the session it reports,
        // and on macOS that is the only exact answer: a process's
        // environment is not readable from outside.
        let reported = Session {
            kind: "tmux".to_owned(),
            session: Some("work".to_owned()),
            pane: Some("%3".to_owned()),
            evidence: Evidence::Environment,
        };
        let Response::Agent { agent } = daemon
            .handle(Request::Register {
                spec: spec("in-tmux"),
                pid: Some(std::process::id()),
                session: Some(reported.clone()),
            })
            .await
        else {
            panic!("register failed")
        };
        assert_eq!(
            agent.session.as_ref().map(Session::describe).as_deref(),
            Some("tmux:%3")
        );
        assert_eq!(agent.session, Some(reported));

        // And it survives, because it is part of the record rather than
        // something re-derived on every listing.
        let Response::Agent { agent } = daemon
            .handle(Request::Inspect {
                agent: agent.id.to_string(),
            })
            .await
        else {
            panic!("inspect failed")
        };
        assert_eq!(agent.session.map(|s| s.kind), Some("tmux".to_owned()));
    }

    #[tokio::test]
    async fn an_agent_with_no_process_is_in_no_session() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        // No pid: nothing to look up, and nothing reported.
        let agent = register(&daemon, "bodiless", None).await;
        assert_eq!(agent.session, None);
    }

    // ----- run --in-pane --------------------------------------------------

    /// Kill a tmux session however a test ends, so a failure does not
    /// leave a stray server behind for the next one.
    struct TmuxSession(String);

    impl Drop for TmuxSession {
        fn drop(&mut self) {
            let _ = std::process::Command::new("tmux")
                .args(["kill-session", "-t", &self.0])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
    }

    fn tmux_says(pane: &str, format: &str) -> String {
        let out = std::process::Command::new("tmux")
            .args(["display-message", "-p", "-t", pane, format])
            .output()
            .expect("tmux answers");
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    #[tokio::test]
    async fn an_agent_in_a_pane_is_registered_rather_than_supervised() {
        if !agentdocker_host::multiplexer::tmux::available() {
            return;
        }
        let dir = TempDir::new().unwrap();
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        let daemon = open(&dir);
        // A distinct name per run, so concurrent tests cannot collide on
        // one tmux session.
        let name = format!("paned-{}", std::process::id());
        let _cleanup = TmuxSession(name.clone());

        let mut spec = spec(&name);
        spec.workdir = Some(work.clone());
        spec.in_pane = true;
        spec.command = vec!["sh".into(), "-c".into(), "sleep 30".into()];
        let Response::Agent { agent } = daemon.handle(Request::Run { spec }).await else {
            panic!("run --in-pane failed")
        };

        // tmux owns the process, so the daemon registered it rather than
        // supervising a child of its own.
        assert!(!agent.managed, "tmux started it, not us");
        assert_eq!(agent.status, AgentStatus::Running);
        let session = agent.session.as_ref().expect("the pane is on the record");
        assert_eq!(session.kind, "tmux");
        assert_eq!(session.session.as_deref(), Some(name.as_str()));
        let pane = session.pane.as_deref().expect("a pane id");

        // And tmux agrees about which process that is, which is what
        // makes `stop`, liveness and attribution work at all.
        assert_eq!(
            tmux_says(pane, "#{pane_pid}"),
            agent.pid.expect("a pid").to_string(),
            "the record's pid is the pane's pid"
        );
        assert_eq!(tmux_says(pane, "#{session_name}"), name);

        // It can coordinate like any other agent: the pane got its id.
        assert!(matches!(
            claim(&daemon, agent.id.as_str(), "task:in-a-pane").await,
            Response::Lease { .. }
        ));
    }

    #[tokio::test]
    async fn a_pane_agent_cannot_also_ask_for_a_terminal_or_a_restore() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let work = dir.path().to_path_buf();

        // The CLI refuses these combinations too, but the daemon has to:
        // MCP, an Agentfile and the protocol itself all bypass clap.
        let mut both = spec("two-terminals");
        both.workdir = Some(work.clone());
        both.in_pane = true;
        both.tty = true;
        both.command = vec!["sh".into()];
        let response = daemon.handle(Request::Run { spec: both }).await;
        assert!(
            matches!(&response, Response::Error { code: ErrorCode::Invalid, message, .. }
                     if message.contains("two terminals")),
            "unexpected {response:?}"
        );

        let mut restoring = spec("not-ours-to-restore");
        restoring.workdir = Some(work);
        restoring.in_pane = true;
        restoring.restore = true;
        restoring.command = vec!["sh".into()];
        let response = daemon.handle(Request::Run { spec: restoring }).await;
        assert!(
            matches!(&response, Response::Error { code: ErrorCode::Invalid, message, .. }
                     if message.contains("nothing to bring back")),
            "unexpected {response:?}"
        );

        // And a container is the engine's to start, so there is nothing
        // for tmux to own. `run_container` never reached `run_in_pane`,
        // so the flag would otherwise have been silently ignored.
        let mut containerised = spec("engine-started");
        containerised.workdir = Some(dir.path().to_path_buf());
        containerised.in_pane = true;
        containerised.command = vec!["sh".into()];
        let response = daemon
            .handle(Request::RunContainer {
                spec: containerised,
                build: "nosuchbuild".to_owned(),
                options: agentdocker_core::container::ContainerRunOptions::default(),
            })
            .await;
        assert!(
            matches!(&response, Response::Error { code: ErrorCode::Invalid, message, .. }
                     if message.contains("nothing for tmux to own")),
            "unexpected {response:?}"
        );
    }

    #[tokio::test]
    /// Deliberately not skipped where tmux is absent: whether a request
    /// is well formed does not depend on the machine, and CI proved the
    /// point by failing here when the tmux probe ran first.
    async fn a_pane_agent_needs_a_workdir_for_tmux_to_start_in() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let mut spec = spec("nowhere");
        spec.in_pane = true;
        spec.workdir = None;
        spec.command = vec!["sh".into()];
        let response = daemon.handle(Request::Run { spec }).await;
        assert!(
            matches!(&response, Response::Error { code: ErrorCode::Invalid, message, .. }
                     if message.contains("workdir")),
            "unexpected {response:?}"
        );
    }

    /// Two agents each holding what the other wants, asking at once:
    /// exactly one is refused and the other simply waits.
    ///
    /// This is the user-visible invariant — a ring is always broken, and
    /// broken once — and it holds whichever order the two arrive in,
    /// because the state lock serialises their first attempts.
    ///
    /// It is **not** a test of the atomicity fix, and it is worth saying
    /// so where somebody will read it. I checked: reintroducing the old
    /// shape — releasing the lock between the deadlock check and the
    /// join — leaves this test passing, because the window is nanoseconds
    /// wide in the same task and nothing makes the other task land in it.
    /// A test that did catch it would need a hook inside that window,
    /// and the fix's whole point is that the window no longer exists, so
    /// the hook would have to be restored by the same regression it was
    /// meant to catch. That fix rests on reading the code: there is no
    /// unlock between `state.deadlock(..)` and `waiting.join_locked(..)`.
    // Two workers, not four: the test needs the two claims to run at
    // once and nothing more, and this is the only multi-threaded runtime
    // in the suite — on a machine already running one test per core,
    // extra worker threads are contention every other test pays for.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn two_agents_closing_one_ring_leave_exactly_one_refused() {
        for round in 0..3 {
            let dir = TempDir::new().unwrap();
            let daemon = open(&dir);
            let alpha = register(&daemon, "alpha", distinct_pid()).await;
            let beta = register(&daemon, "beta", distinct_pid()).await;
            assert!(matches!(
                claim(&daemon, "alpha", "task:x").await,
                Response::Lease { .. }
            ));
            assert!(matches!(
                claim(&daemon, "beta", "task:y").await,
                Response::Lease { .. }
            ));

            let a = {
                let daemon = daemon.clone();
                let id = alpha.id.to_string();
                tokio::spawn(async move { claim_waiting(&daemon, &id, "task:y", 1).await })
            };
            let b = {
                let daemon = daemon.clone();
                let id = beta.id.to_string();
                tokio::spawn(async move { claim_waiting(&daemon, &id, "task:x", 1).await })
            };
            let outcomes = [a.await.unwrap(), b.await.unwrap()];

            let deadlocked = outcomes
                .iter()
                .filter(|r| {
                    matches!(
                        r,
                        Response::Error {
                            code: ErrorCode::Deadlock,
                            ..
                        }
                    )
                })
                .count();
            assert_eq!(
                deadlocked, 1,
                "round {round}: the ring is broken exactly once, got {outcomes:?}"
            );
            // The other waited its second and reported an ordinary
            // conflict, which is the honest answer: it is queued behind a
            // lease nobody released.
            assert!(
                outcomes.iter().any(|r| matches!(
                    r,
                    Response::Error {
                        code: ErrorCode::Conflict,
                        ..
                    }
                )),
                "round {round}: the other one simply waited: {outcomes:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_pane_agent_is_announced_as_started_once_and_only_after_tmux_has_it() {
        if !agentdocker_host::multiplexer::tmux::available() {
            return;
        }
        let dir = TempDir::new().unwrap();
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        let daemon = open(&dir);
        let name = format!("announce-{}", std::process::id());
        let _cleanup = TmuxSession(name.clone());

        let mut events = daemon.subscribe_events();
        let mut spec = spec(&name);
        spec.workdir = Some(work);
        spec.in_pane = true;
        spec.command = vec!["sh".into(), "-c".into(), "sleep 30".into()];
        let Response::Agent { agent } = daemon.handle(Request::Run { spec }).await else {
            panic!("run --in-pane failed")
        };

        // The record exists before tmux is asked for the process, so the
        // ordinary unmanaged-registration announcement is suppressed:
        // otherwise subscribers would be told an agent started that might
        // never start, and told again when it did.
        let mut starts = Vec::new();
        while let Ok(event) = events.try_recv() {
            if let EventKind::AgentStarted { agent: id, pid } = event.kind
                && id == agent.id
            {
                starts.push(pid);
            }
        }
        assert_eq!(starts.len(), 1, "announced once, not twice: {starts:?}");
        assert_eq!(
            starts[0], agent.pid,
            "and with the pid tmux actually started, never None"
        );
    }

    // ----- restart policies -----------------------------------------------

    /// Wait until an agent's process group is really gone.
    ///
    /// Signalling is not reaping. A test that returns while its children
    /// are still dying leaves them for whatever runs next, which under a
    /// parallel runner shows up as somebody else's leak.
    async fn gone(pid: Option<u32>) {
        let Some(pid) = pid else { return };
        let _ = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while supervisor::group_exists(pid) {
                let _ = nix::sys::signal::kill(
                    nix::unistd::Pid::from_raw(-(pid as i32)),
                    nix::sys::signal::Signal::SIGKILL,
                );
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await;
    }

    /// Wait for a condition on an agent, so a test never races the
    /// supervisor's own tasks.
    async fn until(
        daemon: &Arc<Daemon>,
        id: &AgentId,
        what: &str,
        ready: impl Fn(&AgentRecord) -> bool,
    ) -> AgentRecord {
        tokio::time::timeout(std::time::Duration::from_secs(20), async {
            loop {
                let found = lock(&daemon.state).registry.get(id).cloned();
                if let Some(record) = found
                    && ready(&record)
                {
                    return record;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
    }

    #[tokio::test]
    async fn a_failing_agent_comes_back_until_its_limit_and_then_stays_down() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let mut spec = spec("flaky");
        spec.workdir = Some(dir.path().to_path_buf());
        spec.command = vec!["sh".into(), "-c".into(), "exit 7".into()];
        spec.restart = agentdocker_core::RestartPolicy::OnFailure { max: 2 };
        let Response::Agent { agent } = daemon.handle(Request::Run { spec }).await else {
            panic!("run failed")
        };

        // Two restarts, and then it is left alone: the command is broken
        // rather than flaky, and retrying forever would say nothing new.
        let settled = until(&daemon, &agent.id, "the limit to be reached", |r| {
            r.restarts >= 2 && !r.status.is_live()
        })
        .await;
        assert_eq!(settled.restarts, 2, "exactly the limit");
        // It stays that way.
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
        let after = lock(&daemon.state)
            .registry
            .get(&agent.id)
            .cloned()
            .unwrap();
        assert_eq!(after.restarts, 2, "and no more");
        assert!(!after.status.is_live());
        gone(after.pid).await;
    }

    #[tokio::test]
    async fn a_clean_exit_is_the_end_of_an_on_failure_agent() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let mut spec = spec("tidy");
        spec.workdir = Some(dir.path().to_path_buf());
        spec.command = vec!["sh".into(), "-c".into(), "exit 0".into()];
        spec.restart = agentdocker_core::RestartPolicy::OnFailure { max: 5 };
        let Response::Agent { agent } = daemon.handle(Request::Run { spec }).await else {
            panic!("run failed")
        };
        until(&daemon, &agent.id, "it to finish", |r| !r.status.is_live()).await;
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let after = lock(&daemon.state)
            .registry
            .get(&agent.id)
            .cloned()
            .unwrap();
        assert_eq!(after.restarts, 0, "nothing failed, so nothing to retry");
        assert_eq!(after.status, AgentStatus::Exited { code: Some(0) });
        gone(after.pid).await;
    }

    #[tokio::test]
    async fn a_restart_keeps_the_agents_identity_and_says_so() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let mut spec = spec("service");
        spec.workdir = Some(dir.path().to_path_buf());
        spec.command = vec!["sh".into(), "-c".into(), "exit 1".into()];
        spec.restart = agentdocker_core::RestartPolicy::OnFailure { max: 1 };
        // Subscribed before the agent exists. `exit 1` is over almost at
        // once, and a subscription taken afterwards can miss the very
        // event this is about — the stream carries what happens after
        // you join it, not what already happened.
        let mut events = daemon.subscribe_events();
        let Response::Agent { agent } = daemon.handle(Request::Run { spec }).await else {
            panic!("run failed")
        };

        let restarted = until(&daemon, &agent.id, "one restart", |r| r.restarts >= 1).await;
        // Same identity: everything already recorded about it — its read
        // set, journal cursor, leases, ledger rows — still describes it.
        assert_eq!(restarted.id, agent.id);
        assert_eq!(restarted.spec.name, "service");

        let mut announced = None;
        while let Ok(event) = events.try_recv() {
            if let EventKind::AgentRestarted {
                agent: id, attempt, ..
            } = event.kind
                && id == agent.id
            {
                announced = Some(attempt);
            }
        }
        assert_eq!(
            announced,
            Some(1),
            "the restart is announced, with its number"
        );
        gone(restarted.pid).await;
    }

    #[tokio::test]
    async fn an_agent_stopped_on_purpose_is_not_restarted() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let mut spec = spec("stoppable");
        spec.workdir = Some(dir.path().to_path_buf());
        spec.command = vec![
            "sh".into(),
            "-c".into(),
            "while true; do sleep 1; done".into(),
        ];
        spec.restart = agentdocker_core::RestartPolicy::Always;
        let Response::Agent { agent } = daemon.handle(Request::Run { spec }).await else {
            panic!("run failed")
        };
        until(&daemon, &agent.id, "it to be running", |r| {
            r.status == AgentStatus::Running
        })
        .await;

        daemon
            .handle(Request::Stop {
                agent: agent.id.to_string(),
                force: true,
            })
            .await;
        let stopped = until(&daemon, &agent.id, "it to stop", |r| !r.status.is_live()).await;
        // The policy is cleared on the record, so the reason it will not
        // come back is visible in `inspect` rather than hidden here.
        assert!(stopped.spec.restart.is_no(), "stopping clears the policy");

        tokio::time::sleep(std::time::Duration::from_millis(700)).await;
        let after = lock(&daemon.state)
            .registry
            .get(&agent.id)
            .cloned()
            .unwrap();
        assert_eq!(after.restarts, 0, "and it stayed stopped");
        assert!(!after.status.is_live());
        gone(after.pid).await;
    }

    #[tokio::test]
    async fn an_unmanaged_agent_is_never_restarted_whatever_it_asks_for() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let mut spec = spec("not-ours");
        spec.restart = agentdocker_core::RestartPolicy::Always;
        let agent = register_spec(&daemon, spec).await;
        // The daemon did not start it, so it cannot start it again.
        daemon.mark_exited(&agent.id, AgentStatus::Exited { code: Some(1) });
        daemon.consider_restart(&agent.id, &AgentStatus::Exited { code: Some(1) });
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        let after = lock(&daemon.state)
            .registry
            .get(&agent.id)
            .cloned()
            .unwrap();
        assert_eq!(after.restarts, 0);
        assert!(!after.status.is_live());
    }

    // ----- admission policy and quotas ------------------------------------

    /// Write a host policy and wait for the daemon to pick it up, which
    /// it does on its own tick by modification time.
    fn write_policy(daemon: &Arc<Daemon>, toml: &str) {
        std::fs::write(daemon.home.join("policy.toml"), toml).unwrap();
        daemon.reload_policies();
    }

    #[tokio::test]
    async fn a_denied_claim_is_refused_and_says_which_rule() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let agent = register(&daemon, "writer", distinct_pid()).await;
        write_policy(
            &daemon,
            r#"
[[rule]]
name = "migrations are mine"
deny = ["claim:task:migrations"]
"#,
        );

        let mut events = daemon.subscribe_events();
        let refused = claim(&daemon, agent.id.as_str(), "task:migrations").await;
        let Response::Error {
            code,
            message,
            details,
        } = refused
        else {
            panic!("the policy should have refused it")
        };
        assert_eq!(code, ErrorCode::Forbidden);
        assert!(message.contains("migrations are mine"), "{message}");
        let details = details.expect("the rule is in the details");
        assert_eq!(details["rule"], json!("host: migrations are mine"));
        assert_eq!(details["action"], json!("claim:task:migrations"));

        // A refusal is explainable from the event stream alone.
        let mut announced = false;
        while let Ok(event) = events.try_recv() {
            if let EventKind::PolicyDenied { action, .. } = &event.kind
                && action == "claim:task:migrations"
            {
                announced = true;
            }
        }
        assert!(announced, "policy_denied is emitted");

        // Everything else is untouched.
        assert!(matches!(
            claim(&daemon, agent.id.as_str(), "task:anything-else").await,
            Response::Lease { .. }
        ));
    }

    #[tokio::test]
    async fn a_policy_takes_effect_without_a_restart_and_a_broken_one_does_not_disarm_it() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let agent = register(&daemon, "writer", distinct_pid()).await;
        // Nothing written: everything allowed.
        assert!(matches!(
            claim(&daemon, agent.id.as_str(), "task:one").await,
            Response::Lease { .. }
        ));

        write_policy(&daemon, "[[rule]]\ndeny = [\"claim:task:**\"]\n");
        assert!(matches!(
            claim(&daemon, agent.id.as_str(), "task:two").await,
            Response::Error {
                code: ErrorCode::Forbidden,
                ..
            }
        ));

        // A file that will not parse must not read as "no rules": an
        // empty policy allows everything, so a typo would switch off
        // every rule it was written to enforce.
        std::fs::write(daemon.home.join("policy.toml"), "[[rule]\ndeny = oops").unwrap();
        daemon.reload_policies();
        assert!(
            matches!(
                claim(&daemon, agent.id.as_str(), "task:three").await,
                Response::Error {
                    code: ErrorCode::Forbidden,
                    ..
                }
            ),
            "the last good policy stays in force"
        );

        // Removing the file is a decision, and does take effect.
        std::fs::remove_file(daemon.home.join("policy.toml")).unwrap();
        daemon.reload_policies();
        assert!(matches!(
            claim(&daemon, agent.id.as_str(), "task:four").await,
            Response::Lease { .. }
        ));
    }

    #[tokio::test]
    async fn a_denied_message_never_reaches_anybody() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let sender = register(&daemon, "loud", distinct_pid()).await;
        let listener = register(&daemon, "quiet", distinct_pid()).await;
        write_policy(
            &daemon,
            r#"
[[rule]]
agent = "loud"
deny = ["send:all"]
"#,
        );
        let refused = daemon
            .handle(Request::Send {
                from: sender.id.to_string(),
                to: "all".into(),
                kind: "chat".into(),
                payload: json!({ "text": "everyone!" }),
                reply_to: None,
            })
            .await;
        assert!(matches!(
            &refused,
            Response::Error {
                code: ErrorCode::Forbidden,
                ..
            }
        ));
        assert!(
            inbox(&daemon, listener.id.as_str(), true).await.is_empty(),
            "a refused message is not delivered to anyone"
        );
        // Addressing one agent is still fine.
        assert!(matches!(
            daemon
                .handle(Request::Send {
                    from: sender.id.to_string(),
                    to: listener.id.to_string(),
                    kind: "chat".into(),
                    payload: json!({ "text": "just you" }),
                    reply_to: None,
                })
                .await,
            Response::Sent { .. }
        ));
    }

    #[tokio::test]
    async fn a_quota_is_spent_by_several_agents_until_it_runs_out() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let alpha = register(&daemon, "alpha", distinct_pid()).await;
        let beta = register(&daemon, "beta", distinct_pid()).await;
        write_policy(&daemon, "[quota]\ntokens = 100\n");

        let take = |agent: String, amount: u64| {
            let daemon = daemon.clone();
            async move {
                daemon
                    .handle(Request::Claim {
                        agent,
                        resource: "quota:tokens".into(),
                        mode: LeaseMode::Shared,
                        amount: Some(amount),
                        ttl_secs: 300,
                        note: None,
                        wait_secs: 0,
                    })
                    .await
            }
        };

        // A quota is shared: several agents hold it at once.
        assert!(matches!(
            take(alpha.id.to_string(), 60).await,
            Response::Lease { .. }
        ));
        assert!(matches!(
            take(beta.id.to_string(), 30).await,
            Response::Lease { .. }
        ));
        // 90 of 100 spent; 20 does not fit.
        let refused = take(alpha.id.to_string(), 20).await;
        let Response::Error {
            code,
            message,
            details,
        } = refused
        else {
            panic!("the quota should have refused it: {refused:?}")
        };
        assert_eq!(code, ErrorCode::Conflict);
        assert!(message.contains("10 of 100 left"), "{message}");
        let details = details.expect("the arithmetic is in the details");
        assert_eq!(details["committed"], json!(90));
        assert_eq!(details["capacity"], json!(100));
        // What does fit still does.
        assert!(matches!(
            take(beta.id.to_string(), 10).await,
            Response::Lease { .. }
        ));
    }

    #[tokio::test]
    async fn a_quota_nobody_set_a_capacity_for_is_unlimited() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let agent = register(&daemon, "spender", distinct_pid()).await;
        write_policy(&daemon, "[quota]\ntokens = 10\n");
        // A typo in a quota name must loosen nothing that was not
        // already loose, so an unmentioned quota has no ceiling.
        let response = daemon
            .handle(Request::Claim {
                agent: agent.id.to_string(),
                resource: "quota:tokns".into(),
                mode: LeaseMode::Shared,
                amount: Some(1_000_000),
                ttl_secs: 300,
                note: None,
                wait_secs: 0,
            })
            .await;
        assert!(matches!(response, Response::Lease { .. }), "{response:?}");
    }

    // ----- every checkout of a project --------------------------------------

    /// The bug this exists for, found by auditing a real fleet: a
    /// project had eight checkouts, the daemon watched the one an agent
    /// was registered in, and twenty-seven commits produced four
    /// journal entries. Worse, `overlap` answered "no path was changed
    /// in more than one checkout" — a confident wrong answer to the
    /// question the feature exists for — because it could only see one.
    #[tokio::test]
    async fn a_commit_in_a_worktree_nobody_registered_still_reaches_the_journal() {
        if !have_git() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join("kept.txt"), "one\n").unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.name", "t"],
            vec!["config", "user.email", "t@example.com"],
            vec!["config", "commit.gpgsign", "false"],
            vec!["add", "."],
            vec!["commit", "-q", "-m", "root"],
        ] {
            assert!(git(dir.path(), &repo, &args), "{args:?}");
        }
        // A second checkout of the same repository, with no agent in it.
        let elsewhere = dir.path().join("elsewhere");
        assert!(git(
            dir.path(),
            &repo,
            &[
                "worktree",
                "add",
                "-b",
                "side",
                "--",
                elsewhere.to_str().unwrap(),
                "HEAD",
            ],
        ));

        let daemon = open(&dir);
        daemon.expect_watcher();
        tokio::spawn(crate::watcher::run(
            daemon.clone(),
            std::time::Duration::from_millis(100),
            std::time::Duration::from_millis(50),
        ));
        register_in(&daemon, "home", &repo).await;
        // The enumeration is what tells the watcher the second checkout
        // exists; on the daemon it runs on the five-second tick.
        daemon.refresh_project_checkouts().await;
        let watched: Vec<_> = daemon.watch_targets().into_iter().map(|c| c.dir).collect();
        assert!(
            watched.iter().any(|d| d.ends_with("elsewhere")),
            "the other checkout is watched: {watched:?}"
        );

        // Work happens over there, and nothing is registered there.
        std::fs::write(elsewhere.join("kept.txt"), "changed elsewhere\n").unwrap();
        assert!(git(dir.path(), &elsewhere, &["add", "."]));
        assert!(git(
            dir.path(),
            &elsewhere,
            &["commit", "-q", "-m", "work nobody registered"],
        ));

        let entries = eventually(async || {
            let Response::Journal { entries, .. } = daemon
                .handle(Request::Journal {
                    project: repo.display().to_string(),
                    agent: None,
                    since_seq: None,
                    until_seq: None,
                    branch: None,
                    kind: Some("commit".into()),
                    path: None,
                    grep: None,
                    limit: 10,
                    digest: None,
                })
                .await
            else {
                panic!("journal failed")
            };
            (!entries.is_empty()).then_some(entries)
        })
        .await;
        let entry = &entries[0];
        assert!(
            entry.summary.contains("work nobody registered"),
            "the commit is named: {}",
            entry.summary
        );
        assert_eq!(
            entry.checkout,
            Some(elsewhere.canonicalize().unwrap()),
            "and attributed to the checkout it happened in"
        );
    }

    // ----- commit ----------------------------------------------------------

    /// Set up a repository with one commit and an agent registered in
    /// it, with a git identity local to the repository so the test does
    /// not depend on the machine having one.
    async fn repo_with_agent(dir: &TempDir, name: &str) -> (Arc<Daemon>, PathBuf) {
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join("kept.txt"), "one\n").unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.name", "t"],
            vec!["config", "user.email", "t@example.com"],
            vec!["config", "commit.gpgsign", "false"],
            vec!["add", "."],
            vec!["commit", "-q", "-m", "root"],
        ] {
            assert!(git(dir.path(), &repo, &args), "{args:?}");
        }
        let daemon = open(dir);
        register_in(&daemon, name, &repo).await;
        (daemon, repo)
    }

    async fn commit(daemon: &Arc<Daemon>, agent: &str, message: &str, all: bool) -> Response {
        daemon
            .handle(Request::Commit {
                agent: agent.into(),
                message: message.into(),
                all,
                push: false,
            })
            .await
    }

    /// The point of the whole request. The watcher already writes a
    /// `commit` entry when it sees HEAD move, but it has to guess whose
    /// it was and can only synthesise a summary from the sha. Going
    /// through the daemon means the agent that asked is the agent
    /// recorded, with the message it actually wrote.
    #[tokio::test]
    async fn a_commit_through_the_daemon_is_attributed_and_carries_its_message() {
        if !have_git() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let (daemon, repo) = repo_with_agent(&dir, "writer").await;
        std::fs::write(repo.join("new.txt"), "two\n").unwrap();
        assert!(git(dir.path(), &repo, &["add", "."]));

        let Response::Committed {
            head,
            branch,
            files,
            pushed,
        } = commit(&daemon, "writer", "teach the parser about dashes", false).await
        else {
            panic!("commit failed");
        };
        assert_eq!(files, 1);
        assert!(!pushed, "nothing asked for a push");
        assert_eq!(branch.as_deref(), Some("main"));
        assert_eq!(head.len(), 40, "a full sha: {head}");

        let Response::Journal { entries, .. } = daemon
            .handle(Request::Journal {
                project: repo.display().to_string(),
                agent: Some("writer".into()),
                since_seq: None,
                until_seq: None,
                branch: None,
                kind: Some("commit".into()),
                path: None,
                grep: None,
                limit: 10,
                digest: None,
            })
            .await
        else {
            panic!("journal failed")
        };
        assert_eq!(entries.len(), 1, "exactly one entry: {entries:?}");
        let entry = &entries[0];
        assert_eq!(entry.agent_name, "writer", "attributed, not guessed");
        assert!(
            entry.summary.contains("teach the parser about dashes"),
            "the agent's own message, not a summary of the sha: {}",
            entry.summary
        );
        assert_eq!(entry.head_after.as_ref(), Some(&head));
        assert_eq!(entry.branch.as_deref(), Some("main"));
        assert!(
            daemon.recent_events(200).iter().any(|e| matches!(
                &e.kind,
                EventKind::Committed { agent, files: 1, .. } if agent.as_str() == entry.agent.as_ref().unwrap().as_str()
            )),
            "the state change is an event"
        );
    }

    /// A watcher running alongside must not add a second `commit` entry
    /// for the same move a moment later, attributed by guesswork.
    #[tokio::test]
    async fn the_watcher_does_not_double_up_on_a_commit_the_daemon_made() {
        if !have_git() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let (daemon, repo) = repo_with_agent(&dir, "writer").await;
        daemon.expect_watcher();
        tokio::spawn(crate::watcher::run(
            daemon.clone(),
            std::time::Duration::from_secs(60),
            std::time::Duration::from_millis(50),
        ));
        std::fs::write(repo.join("new.txt"), "two\n").unwrap();
        assert!(git(dir.path(), &repo, &["add", "."]));
        let Response::Committed { .. } = commit(&daemon, "writer", "one change", false).await
        else {
            panic!("commit failed");
        };
        // Long enough for the watcher to have looked several times.
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        let Response::Journal { entries, .. } = daemon
            .handle(Request::Journal {
                project: repo.display().to_string(),
                agent: None,
                since_seq: None,
                until_seq: None,
                branch: None,
                kind: Some("commit".into()),
                path: None,
                grep: None,
                limit: 10,
                digest: None,
            })
            .await
        else {
            panic!("journal failed")
        };
        assert_eq!(entries.len(), 1, "one commit, one entry: {entries:?}");
    }

    #[tokio::test]
    async fn a_commit_with_nothing_in_it_is_refused() {
        if !have_git() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let (daemon, repo) = repo_with_agent(&dir, "writer").await;

        // Nothing at all.
        let refused = commit(&daemon, "writer", "empty", false).await;
        assert!(
            matches!(&refused, Response::Error { code, .. } if *code == ErrorCode::Conflict),
            "{refused:?}"
        );

        // Changed but unstaged is still nothing, without --all: an agent
        // that meant to commit everything must say so.
        std::fs::write(repo.join("kept.txt"), "changed\n").unwrap();
        let refused = commit(&daemon, "writer", "unstaged", false).await;
        assert!(
            matches!(&refused, Response::Error { code, message, .. }
                if *code == ErrorCode::Conflict && message.contains("--all")),
            "{refused:?}"
        );

        // And with --all it goes in.
        let Response::Committed { files, .. } = commit(&daemon, "writer", "with all", true).await
        else {
            panic!("commit failed");
        };
        assert_eq!(files, 1);
    }

    /// A commit that fails must not leave the checkout marked. The mark
    /// tells the watcher to keep out, so one left behind does not fail
    /// loudly — it silently stops that checkout being journaled for as
    /// long as the daemon lives.
    #[tokio::test]
    async fn a_failed_commit_does_not_leave_the_checkout_marked() {
        if !have_git() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let (daemon, repo) = repo_with_agent(&dir, "writer").await;
        std::fs::write(repo.join("new.txt"), "two\n").unwrap();
        assert!(git(dir.path(), &repo, &["add", "."]));

        // A commit git will refuse. A pre-commit hook that says no is
        // the portable way to arrange that; the point is the failure,
        // not which failure.
        let hooks = dir.path().join("hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        let hook = hooks.join("pre-commit");
        let write_hook = |body: &str| {
            use std::os::unix::fs::PermissionsExt;
            std::fs::write(&hook, body).unwrap();
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        write_hook("#!/bin/sh\nexit 1\n");
        assert!(git(
            dir.path(),
            &repo,
            &["config", "core.hooksPath", hooks.to_str().unwrap()],
        ));
        let refused = commit(&daemon, "writer", "cannot be made", false).await;
        assert!(
            matches!(refused, Response::Error { .. }),
            "the commit failed: {refused:?}"
        );
        assert!(
            lock(&daemon.state).committing.is_empty(),
            "and the checkout is not still marked"
        );

        // Which means the next one works, and is journaled.
        write_hook("#!/bin/sh\nexit 0\n");
        let Response::Committed { .. } = commit(&daemon, "writer", "and now it can", false).await
        else {
            panic!("commit failed")
        };
        assert!(lock(&daemon.state).committing.is_empty());
    }

    /// A message beginning with a dash is a message. Passing it as
    /// `--message=<text>` or positionally would make git read it as a
    /// flag and fail, or worse, succeed at something else.
    #[tokio::test]
    async fn a_message_that_looks_like_a_flag_is_still_a_message() {
        if !have_git() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let (daemon, repo) = repo_with_agent(&dir, "writer").await;
        std::fs::write(repo.join("new.txt"), "two\n").unwrap();
        assert!(git(dir.path(), &repo, &["add", "."]));
        let Response::Committed { head, .. } =
            commit(&daemon, "writer", "--amend is not what I meant", false).await
        else {
            panic!("commit failed");
        };
        let subject = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["log", "-1", "--format=%s", &head])
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&subject.stdout).trim(),
            "--amend is not what I meant"
        );
        // And it is one commit on top of the root, not an amended root.
        let count = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["rev-list", "--count", "HEAD"])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&count.stdout).trim(), "2");
    }

    /// Committing is an action a policy can refuse, like any other.
    #[tokio::test]
    async fn a_policy_can_refuse_a_commit() {
        if !have_git() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let (daemon, repo) = repo_with_agent(&dir, "writer").await;
        std::fs::write(
            dir.path().join("policy.toml"),
            "[[rule]]\nname = \"no commits\"\ndeny = [\"commit:**\"]\n",
        )
        .unwrap();
        daemon.reload_policies();
        std::fs::write(repo.join("new.txt"), "two\n").unwrap();
        assert!(git(dir.path(), &repo, &["add", "."]));
        let refused = commit(&daemon, "writer", "blocked", false).await;
        assert!(
            matches!(&refused, Response::Error { code, message, .. }
                if *code == ErrorCode::Forbidden && message.contains("no commits")),
            "{refused:?}"
        );
    }

    // ----- daemon reload --------------------------------------------------

    #[tokio::test]
    async fn a_linked_worktree_commit_uses_project_policy_and_writes_only_its_checkout() {
        let dir = TempDir::new().unwrap();
        let (daemon, repo) = repo_with_agent(&dir, "main-writer").await;
        let worktree = dir.path().join("linked");
        assert!(git(
            dir.path(),
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "linked",
                worktree.to_str().unwrap()
            ]
        ));
        let nested = worktree.join("nested");
        std::fs::create_dir(&nested).unwrap();
        let writer = register_in(&daemon, "linked-writer", &nested).await;
        let policy_root = writer.project.as_ref().unwrap().dir();
        assert_eq!(policy_root, worktree.canonicalize().unwrap());
        std::fs::write(worktree.join("kept.txt"), "linked change\n").unwrap();
        let policy = dir.path().join("policy.toml");
        for action in ["commit", "push"] {
            std::fs::write(
                &policy,
                format!(
                    "[[rule]]\nname = \"project policy\"\ndeny = [\"{action}:{}\"]\n",
                    policy_root.display()
                ),
            )
            .unwrap();
            daemon.reload_policies();
            let reply = daemon
                .handle(Request::Commit {
                    agent: "linked-writer".into(),
                    message: "linked change".into(),
                    all: true,
                    push: action == "push",
                })
                .await;
            assert!(
                matches!(
                    reply,
                    Response::Error {
                        code: ErrorCode::Forbidden,
                        ..
                    }
                ),
                "{reply:?}"
            );
        }
        std::fs::write(policy, "").unwrap();
        daemon.reload_policies();
        let reply = commit(&daemon, "linked-writer", "linked change", true).await;
        assert!(matches!(reply, Response::Committed { .. }), "{reply:?}");
        assert_eq!(
            std::fs::read_to_string(repo.join("kept.txt")).unwrap(),
            "one\n"
        );
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(&worktree)
            .args(["status", "--porcelain"])
            .output()
            .unwrap();
        assert!(output.status.success() && output.stdout.is_empty());
    }

    #[tokio::test]
    async fn an_ordinary_shutdown_still_stops_what_it_started() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let mut spec = spec("ordinary");
        spec.workdir = Some(dir.path().to_path_buf());
        spec.command = vec![
            "sh".into(),
            "-c".into(),
            "while true; do sleep 1; done".into(),
        ];
        let Response::Agent { agent } = daemon.handle(Request::Run { spec }).await else {
            panic!("run failed")
        };
        let pid = agent.pid.expect("a pid");
        daemon.stop_all().await;
        assert!(
            !supervisor::group_exists(pid),
            "the usual shutdown still takes its children with it"
        );
        gone(Some(pid)).await;
    }

    async fn claim(daemon: &Arc<Daemon>, agent: &str, resource: &str) -> Response {
        daemon
            .handle(Request::Claim {
                agent: agent.to_owned(),
                resource: resource.to_owned(),
                mode: LeaseMode::Exclusive,
                amount: None,
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
    async fn destructive_inbox_failure_retains_queue_events_and_subscription_routing() {
        for event_failure in [false, true] {
            let dir = TempDir::new().unwrap();
            let daemon = open(&dir);
            let receiver = register(&daemon, "receiver", None).await;
            assert!(matches!(
                daemon
                    .handle(Request::Send {
                        from: "user".into(),
                        to: "receiver".into(),
                        kind: "chat".into(),
                        payload: json!({"text":"retain after failed drain"}),
                        reply_to: None,
                    })
                    .await,
                Response::Sent { .. }
            ));
            let queued = inbox(&daemon, "receiver", false).await;
            let mut events = daemon.subscribe_events();
            let next_seq = {
                let state = lock(&daemon.state);
                if event_failure {
                    state.store.reject_event_for_test("inbox_acknowledged");
                } else {
                    state.store.reject_writes_for_test();
                }
                state.next_seq
            };
            let response = daemon
                .handle(Request::Inbox {
                    agent: "receiver".into(),
                    drain: true,
                })
                .await;
            assert!(
                matches!(
                    response,
                    Response::Error {
                        code: ErrorCode::StorageUnavailable,
                        ..
                    }
                ),
                "{response:?}"
            );
            let state = lock(&daemon.state);
            assert_eq!(
                state.inboxes[&receiver.id]
                    .iter()
                    .map(|message| &message.id)
                    .collect::<Vec<_>>(),
                queued.iter().map(|message| &message.id).collect::<Vec<_>>()
            );
            assert_eq!(
                state.store.load_inboxes().unwrap()[&receiver.id].len(),
                queued.len()
            );
            assert_eq!(state.next_seq, next_seq);
            assert!(!state.live_subscribers.contains_key(&receiver.id));
            assert!(events.try_recv().is_err());
        }
    }

    #[tokio::test]
    async fn failed_inbox_touch_does_not_acknowledge_messages() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let receiver = register(&daemon, "receiver", None).await;
        assert!(matches!(
            daemon
                .handle(Request::Send {
                    from: "user".into(),
                    to: "receiver".into(),
                    kind: "chat".into(),
                    payload: json!({"text":"retain after failed touch"}),
                    reply_to: None,
                })
                .await,
            Response::Sent { .. }
        ));
        let mut events = daemon.subscribe_events();
        let next_seq = {
            let state = lock(&daemon.state);
            state.store.reject_agent_writes_for_test();
            state.next_seq
        };
        let response = daemon
            .handle(Request::Inbox {
                agent: "receiver".into(),
                drain: true,
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
        assert_eq!(state.inboxes[&receiver.id].len(), 1);
        assert_eq!(state.store.load_inboxes().unwrap()[&receiver.id].len(), 1);
        assert_eq!(state.next_seq, next_seq);
        assert!(events.try_recv().is_err());
    }

    #[tokio::test]
    async fn destructive_inbox_success_commits_one_acknowledgement_before_delivery() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let receiver = register(&daemon, "receiver", None).await;
        for text in ["first", "second"] {
            assert!(matches!(
                daemon
                    .handle(Request::Send {
                        from: "user".into(),
                        to: "receiver".into(),
                        kind: "chat".into(),
                        payload: json!({"text":text}),
                        reply_to: None,
                    })
                    .await,
                Response::Sent { .. }
            ));
        }
        let mut events = daemon.subscribe_events();
        let delivered = inbox(&daemon, "receiver", true).await;
        assert_eq!(delivered.len(), 2);
        let event = events.try_recv().unwrap();
        assert!(
            matches!(&event.kind, EventKind::InboxAcknowledged { agent, messages }
                if agent == &receiver.id && *messages == delivered.iter().map(|message| message.id.clone()).collect::<Vec<_>>())
        );
        let state = lock(&daemon.state);
        assert!(
            state
                .inboxes
                .get(&receiver.id)
                .is_none_or(|queue| queue.is_empty())
        );
        assert!(
            !state
                .store
                .load_inboxes()
                .unwrap()
                .contains_key(&receiver.id)
        );
        assert_eq!(state.store.recent_events(1).unwrap()[0].seq, event.seq);
        assert!(events.try_recv().is_err());
    }

    #[tokio::test]
    async fn stream_reconnect_and_restart_replay_unacknowledged_human_and_peer_input() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let receiver = register(&daemon, "receiver", None).await;
        register(&daemon, "peer", None).await;
        assert!(matches!(
            send(&daemon, "user", "receiver").await,
            Response::Sent { .. }
        ));
        let (mut stream, mut live) = daemon.subscribe(Some("receiver"), vec![]).unwrap();
        let mut accepted = stream.take_backlog();
        assert_eq!(accepted.len(), 1);
        for from in ["peer", "user", "peer"] {
            assert!(matches!(
                send(&daemon, from, "receiver").await,
                Response::Sent { .. }
            ));
            accepted.push(live.try_recv().unwrap());
        }
        assert_eq!(inbox(&daemon, "receiver", false).await, accepted);
        drop(stream);
        drop(live);
        let (mut reconnected, _) = daemon.subscribe(Some("receiver"), vec![]).unwrap();
        assert_eq!(reconnected.take_backlog(), accepted);
        drop(reconnected);
        drop(daemon);
        let daemon = open(&dir);
        let (mut reconnected, _) = daemon.subscribe(Some("receiver"), vec![]).unwrap();
        assert_eq!(reconnected.take_backlog(), accepted);
        assert_eq!(
            lock(&daemon.state).inbox_bytes[&receiver.id],
            accepted.iter().map(message_bytes).sum::<usize>()
        );
        assert!(matches!(
            daemon
                .handle(Request::AckInbox {
                    agent: "receiver".into(),
                    messages: accepted
                        .iter()
                        .take(2)
                        .map(|message| message.id.clone())
                        .collect(),
                })
                .await,
            Response::Ok
        ));
        assert_eq!(inbox(&daemon, "receiver", false).await, accepted[2..]);
        assert_eq!(
            lock(&daemon.state).inbox_bytes[&receiver.id],
            accepted[2..].iter().map(message_bytes).sum::<usize>()
        );
        drop(reconnected);
        drop(daemon);
        assert_eq!(inbox(&open(&dir), "receiver", false).await, accepted[2..]);
    }

    #[tokio::test]
    async fn subscription_does_not_write_an_acknowledgement() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        register(&daemon, "receiver", None).await;
        assert!(matches!(
            send(&daemon, "user", "receiver").await,
            Response::Sent { .. }
        ));
        let accepted = inbox(&daemon, "receiver", false).await;
        let mut events = daemon.subscribe_events();
        lock(&daemon.state).store.reject_writes_for_test();
        let (mut stream, _) = daemon.subscribe(Some("receiver"), vec![]).unwrap();
        assert_eq!(stream.take_backlog(), accepted);
        assert!(events.try_recv().is_err());
        assert!(lock(&daemon.state).storage_error.is_none());
    }

    #[tokio::test]
    async fn full_inbox_rejects_entire_broadcast_without_evicting_accepted_work() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let full = register(&daemon, "full", None).await;
        let other = register(&daemon, "other", None).await;
        let (stream, mut live) = daemon.subscribe(Some("other"), vec![]).unwrap();
        for _ in 0..INBOX_CAPACITY {
            assert!(matches!(
                send(&daemon, "user", "full").await,
                Response::Sent { .. }
            ));
        }
        // Discard this raw bus receiver's unrelated traffic before checking
        // that the rejected broadcast never reaches even a healthy recipient.
        while live.try_recv().is_ok() {}
        let accepted = inbox(&daemon, "full", false).await;
        let mut events = daemon.subscribe_events();
        let next_seq = lock(&daemon.state).next_seq;
        let response = send(&daemon, "user", "all").await;
        assert!(
            matches!(
                response,
                Response::Error {
                    code: ErrorCode::Backpressure,
                    ..
                }
            ),
            "{response:?}"
        );
        assert!(live.try_recv().is_err());
        assert!(events.try_recv().is_err());
        {
            let state = lock(&daemon.state);
            assert_eq!(state.next_seq, next_seq);
            assert!(state.storage_error.is_none());
            let stored = state.store.load_inboxes().unwrap();
            assert_eq!(
                stored[&full.id].iter().cloned().collect::<Vec<_>>(),
                accepted
            );
            assert!(!stored.contains_key(&other.id));
        }
        assert_eq!(inbox(&daemon, "full", true).await, accepted);
        assert!(matches!(
            send(&daemon, "user", "all").await,
            Response::Sent { .. }
        ));
        assert_eq!(inbox(&daemon, "full", false).await.len(), 1);
        assert_eq!(inbox(&daemon, "other", false).await.len(), 1);
        drop(stream);
    }

    #[tokio::test]
    async fn inbox_byte_pressure_survives_restart_and_acknowledgement_releases_capacity() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        register(&daemon, "receiver", None).await;
        let payload = json!({"text": "x".repeat(512 * 1024)});
        let request = Request::Send {
            from: "user".into(),
            to: "receiver".into(),
            kind: "chat".into(),
            payload,
            reply_to: None,
        };
        for _ in 0..7 {
            assert!(matches!(
                daemon.handle(request.clone()).await,
                Response::Sent { .. }
            ));
        }
        assert!(matches!(
            daemon.handle(request.clone()).await,
            Response::Error {
                code: ErrorCode::Backpressure,
                ..
            }
        ));
        let accepted = inbox(&daemon, "receiver", false).await;
        drop(daemon);
        let daemon = open(&dir);
        assert!(matches!(
            daemon.handle(request.clone()).await,
            Response::Error {
                code: ErrorCode::Backpressure,
                ..
            }
        ));
        assert_eq!(inbox(&daemon, "receiver", false).await, accepted);
        assert!(matches!(
            daemon
                .handle(Request::AckInbox {
                    agent: "receiver".into(),
                    messages: vec![accepted[0].id.clone()]
                })
                .await,
            Response::Ok
        ));
        assert!(matches!(
            daemon.handle(request).await,
            Response::Sent { .. }
        ));
        let remaining = inbox(&daemon, "receiver", false).await;
        assert_eq!(remaining.len(), 7);
        assert_eq!(remaining[..6], accepted[1..]);
    }

    #[tokio::test]
    async fn legacy_duplicate_channel_members_receive_one_durable_message() {
        let dir = TempDir::new().unwrap();
        let project = dir.path().join("project");
        std::fs::create_dir(&project).unwrap();
        std::fs::write(project.join("Agentfile.toml"), "").unwrap();
        let daemon = open(&dir);
        register_in(&daemon, "sender", &project).await;
        let receiver = register_in(&daemon, "receiver", &project).await;
        let Response::Channel { mut channel } = daemon
            .handle(Request::ChannelOpen {
                agent: "sender".into(),
                task: "legacy membership".into(),
                members: vec!["receiver".into()],
            })
            .await
        else {
            panic!("channel failed")
        };
        inbox(&daemon, "receiver", true).await;
        channel.members.push(receiver.id.clone());
        lock(&daemon.state)
            .store
            .put_document("channel", channel.id.as_str(), &channel)
            .unwrap();
        drop(daemon);
        let daemon = open(&dir);
        assert!(matches!(
            send(&daemon, "sender", &format!("channel:{}", channel.id)).await,
            Response::Sent { .. }
        ));
        let queued = inbox(&daemon, "receiver", false).await;
        assert_eq!(queued.len(), 1);
        let state = lock(&daemon.state);
        assert_eq!(state.store.load_inboxes().unwrap()[&receiver.id].len(), 1);
        assert_eq!(state.inbox_bytes[&receiver.id], message_bytes(&queued[0]));
    }

    #[tokio::test]
    async fn inbox_receipts_name_only_real_unacknowledged_ids_once() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let receiver = register(&daemon, "receiver", None).await;
        assert!(matches!(
            send(&daemon, "user", "receiver").await,
            Response::Sent { .. }
        ));
        let accepted = inbox(&daemon, "receiver", false).await;
        let bogus = MessageId::from("never-sent".to_owned());
        let mut events = daemon.subscribe_events();
        let next_seq = lock(&daemon.state).next_seq;
        assert!(matches!(
            daemon
                .handle(Request::AckInbox {
                    agent: "receiver".into(),
                    messages: vec![bogus.clone()],
                })
                .await,
            Response::Ok
        ));
        assert_eq!(lock(&daemon.state).next_seq, next_seq);
        assert!(events.try_recv().is_err());
        let ids = vec![bogus, accepted[0].id.clone(), accepted[0].id.clone()];
        for _ in 0..2 {
            assert!(matches!(
                daemon
                    .handle(Request::AckInbox {
                        agent: "receiver".into(),
                        messages: ids.clone(),
                    })
                    .await,
                Response::Ok
            ));
        }
        let event = events.try_recv().unwrap();
        assert!(
            matches!(event.kind, EventKind::InboxAcknowledged { agent, messages }
            if agent == receiver.id && messages == vec![accepted[0].id.clone()])
        );
        assert!(events.try_recv().is_err());
        assert_eq!(lock(&daemon.state).next_seq, next_seq + 1);
        assert!(inbox(&daemon, "receiver", false).await.is_empty());
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
        // A real pid, because this one does sweep for liveness at the
        // end and expects to find alpha still there.
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
    async fn restore_retires_half_spawned_agents_and_orphaned_leases() {
        let dir = TempDir::new().unwrap();
        let now = Utc::now();
        let first_id;
        {
            let store = Store::open(&dir.path().join("state.db")).unwrap();
            let mut first = AgentRecord::new(spec("twin"), false, now);
            first.status = AgentStatus::Running;
            first_id = first.id.clone();
            let mut second = AgentRecord::new(spec("twin"), false, now + Duration::seconds(1));
            second.status = AgentStatus::Exited { code: Some(0) };
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
                amount: 0,
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
        let half = agents
            .iter()
            .find(|record| record.spec.name == "half")
            .unwrap();
        assert!(events.iter().any(|event| matches!(&event.kind,
            EventKind::AgentExited { agent, status } if agent == &half.id && status == &half.status)));
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

    #[test]
    fn ambiguous_live_names_refuse_before_recovery_changes_any_record_or_lease() {
        let dir = TempDir::new().unwrap();
        let database = dir.path().join("state.db");
        let store = Store::open(&database).unwrap();
        let now = Utc::now();
        // This record would normally be marked failed during startup. Its
        // earlier position must not permit a partial recovery before refusal.
        let half_spawned = AgentRecord::new(spec("half-spawned"), true, now);
        let mut first = AgentRecord::new(spec("ambiguous"), false, now + Duration::seconds(1));
        first.status = AgentStatus::Running;
        let mut second = AgentRecord::new(spec("ambiguous"), false, now + Duration::seconds(2));
        second.status = AgentStatus::Running;
        for record in [&half_spawned, &first, &second] {
            store.upsert_agent(record).unwrap();
        }
        let protection = Lease {
            id: LeaseId::from("still-owned"),
            resource: ResourceKey::new("task:protected"),
            holder: second.id.clone(),
            mode: LeaseMode::Exclusive,
            acquired_at: now,
            change_seq: None,
            expires_at: now + Duration::hours(1),
            note: None,
            amount: 0,
        };
        store.upsert_lease(&protection).unwrap();
        let before = store.load_agents().unwrap();
        let result = Daemon::with_store(dir.path().into(), dir.path().join("sock"), store);
        let error = result
            .err()
            .expect("ambiguous live names must refuse")
            .to_string();
        assert!(
            error.contains("duplicate live agent name ambiguous"),
            "{error}"
        );
        assert!(error.contains(first.id.as_str()) && error.contains(second.id.as_str()));
        let reopened = Store::open(&database).unwrap();
        assert_eq!(reopened.load_agents().unwrap(), before);
        assert_eq!(reopened.load_leases().unwrap(), [protection]);
        assert!(reopened.recent_events(10).unwrap().is_empty());
    }

    #[test]
    fn startup_failure_and_its_event_commit_together() {
        let dir = TempDir::new().unwrap();
        let database = dir.path().join("state.db");
        let store = Store::open(&database).unwrap();
        let record = AgentRecord::new(spec("never-started"), true, Utc::now());
        store.upsert_agent(&record).unwrap();
        store.reject_event_for_test("agent_exited");
        assert!(Daemon::with_store(dir.path().into(), dir.path().join("sock"), store).is_err());
        let reopened = Store::open(&database).unwrap();
        assert_eq!(
            reopened.load_agents().unwrap()[0].status,
            AgentStatus::Created
        );
        assert!(reopened.recent_events(10).unwrap().is_empty());
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[tokio::test]
    async fn recycled_pid_is_not_mistaken_for_the_agent() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        // Both on this process's own pid, on purpose: a real one, so
        // liveness has something to check, and the same one, so the
        // second registration is the recycled-pid case. It is not folded
        // into the first because their start times disagree — which is
        // the whole reason "one process, one agent" compares both.
        let stale = register(&daemon, "stale", Some(std::process::id())).await;
        // Pretend the process that registered started long before the one
        // holding the pid now (as after a reboot).
        lock(&daemon.state)
            .registry
            .get_mut(&stale.id)
            .unwrap()
            .process_started_at = Some(Utc::now() - Duration::hours(24 * 30));
        let fresh = register(&daemon, "fresh", Some(std::process::id())).await;
        assert_ne!(fresh.id, stale.id, "a recycled pid is a different agent");
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
                        amount: None,
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
                amount: None,
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
        match daemon
            .handle(Request::Register {
                spec,
                pid: None,
                session: None,
            })
            .await
        {
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
            Response::Agents { agents, .. } => agents.into_iter().map(|a| a.spec.name).collect(),
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

    fn discovery_row(pid: u32, started_at: chrono::DateTime<Utc>) -> DiscoveredProcess {
        DiscoveredProcess {
            pid,
            ppid: 1,
            runtime: "codex".into(),
            command: "codex".into(),
            cwd: None,
            project: None,
            started_at: Some(started_at),
            session: None,
        }
    }

    #[tokio::test]
    async fn concurrent_discovery_joins_the_inflight_scan_and_preserves_its_failure() {
        use std::sync::atomic::Ordering;
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let row = discovery_row(123456, Utc::now());
        for result in [Ok(vec![row]), Err("scan failed".to_owned())] {
            assert!(!daemon.scanning.swap(true, Ordering::AcqRel));
            let flight = ScanGuard {
                scanning: &daemon.scanning,
                finished: &daemon.scan_finished,
            };
            let joined = daemon.scan_agents();
            tokio::pin!(joined);
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(20), &mut joined)
                    .await
                    .is_err()
            );
            let committed = daemon.apply_scan(result);
            drop(flight);
            assert_eq!(
                tokio::time::timeout(std::time::Duration::from_secs(1), joined)
                    .await
                    .unwrap(),
                committed
            );
            assert!(!daemon.scanning.load(Ordering::Acquire));
        }
    }

    #[test]
    fn scan_failure_retains_snapshot_and_recovery_distinguishes_pid_generations() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let first = discovery_row(123456, Utc::now());
        let mut events = daemon.subscribe_events();
        daemon.apply_scan(Ok(vec![first.clone()])).unwrap();
        while events.try_recv().is_ok() {}
        let at = lock(&daemon.state).discovered.at;
        assert!(daemon.apply_scan(Err("ps unavailable".into())).is_err());
        assert_eq!(
            lock(&daemon.state).discovered.processes,
            vec![first.clone()]
        );
        assert_eq!(lock(&daemon.state).discovered.at, at);
        assert!(matches!(
            events.try_recv().unwrap().kind,
            EventKind::DiscoveryUnavailable { .. }
        ));
        assert!(
            events.try_recv().is_err(),
            "failure never invents vanished agents"
        );
        assert!(daemon.apply_scan(Err("ps unavailable".into())).is_err());
        assert!(
            events.try_recv().is_err(),
            "same failure is not repeated every tick"
        );
        let replacement =
            discovery_row(first.pid, first.started_at.unwrap() + Duration::seconds(1));
        daemon.apply_scan(Ok(vec![replacement.clone()])).unwrap();
        assert!(
            matches!(events.try_recv().unwrap().kind, EventKind::AgentVanished { started_at, adopted: false, .. } if started_at == first.started_at)
        );
        assert!(
            matches!(events.try_recv().unwrap().kind, EventKind::AgentDiscovered { started_at, .. } if started_at == replacement.started_at)
        );
        assert!(matches!(
            events.try_recv().unwrap().kind,
            EventKind::DiscoveryAvailable
        ));
        daemon.apply_scan(Ok(vec![replacement.clone()])).unwrap();
        assert!(events.try_recv().is_err());
        let mut moved = replacement;
        moved.cwd = Some(PathBuf::from("/tmp/new-project"));
        daemon.apply_scan(Ok(vec![moved])).unwrap();
        assert!(matches!(
            events.try_recv().unwrap().kind,
            EventKind::AgentDiscovered { cwd: Some(_), .. }
        ));
        assert!(events.try_recv().is_err());
    }

    #[tokio::test]
    async fn a_scan_started_before_registration_cannot_restore_an_adopted_process() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let row = discovery_row(child.id(), procinfo::start_time(child.id()).unwrap());
        daemon.apply_scan(Ok(vec![row.clone()])).unwrap();
        let response = daemon
            .register(
                AgentSpec {
                    name: "joined-during-scan".into(),
                    ..AgentSpec::default()
                },
                Some(child.id()),
                None,
            )
            .await;
        assert!(matches!(response, Response::Agent { .. }), "{response:?}");
        let mut events = daemon.subscribe_events();
        assert!(daemon.apply_scan(Ok(vec![row.clone()])).unwrap().is_empty());
        assert!(matches!(
            events.try_recv().unwrap().kind,
            EventKind::AgentVanished { adopted: true, .. }
        ));
        assert!(daemon.apply_scan(Ok(vec![row])).unwrap().is_empty());
        assert!(events.try_recv().is_err());
        child.kill().unwrap();
        child.wait().unwrap();
    }

    #[tokio::test]
    async fn scans_announce_agents_appearing_going_and_adopted() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let project = dir.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("Agentfile.toml"), "").unwrap();
        let fake = dir.path().join("codex");
        std::os::unix::fs::symlink("/bin/sleep", &fake).unwrap();
        let mut events = daemon.subscribe_events();
        daemon.scan_agents().await.unwrap();
        let mut child = std::process::Command::new(&fake)
            .arg("60")
            .current_dir(&project)
            .spawn()
            .unwrap();
        let pid = child.id();

        daemon.scan_agents().await.unwrap();
        let mut seen = Vec::new();
        while let Ok(event) = events.try_recv() {
            seen.push(event.kind);
        }
        assert!(
            seen.iter().any(|k| matches!(k, EventKind::AgentDiscovered { pid: p, runtime, project: Some(_), .. } if *p == pid && runtime == "codex")),
            "{seen:?}"
        );
        // Fresh scans answer from the cache and announce nothing twice.
        daemon.scan_agents().await.unwrap();
        assert!(
            !std::iter::from_fn(|| events.try_recv().ok())
                .any(|e| matches!(e.kind, EventKind::AgentDiscovered { pid: p, .. } if p == pid))
        );
        let Response::Processes { processes } = daemon.handle(Request::Discover).await else {
            panic!("discover failed");
        };
        assert!(processes.iter().any(|p| p.pid == pid));
        let Response::Runtimes { runtimes } = daemon.handle(Request::Runtimes).await else {
            panic!("runtimes failed");
        };
        let codex = runtimes.iter().find(|r| r.name == "codex").unwrap();
        assert!(codex.running >= 1, "{codex:?}");

        // Adopted: it leaves the discovered set at once, as adopted, not
        // as gone — and a fresh discover no longer lists it.
        assert!(matches!(
            daemon
                .handle(Request::Adopt {
                    pid,
                    name: None,
                    runtime: None,
                })
                .await,
            Response::Agent { .. }
        ));
        let kinds: Vec<EventKind> = std::iter::from_fn(|| events.try_recv().ok())
            .map(|e| e.kind)
            .collect();
        assert!(
            kinds.iter().any(
                |k| matches!(k, EventKind::AgentVanished { pid: p, adopted: true, .. } if *p == pid)
            ),
            "{kinds:?}"
        );
        let Response::Processes { processes } = daemon.handle(Request::Discover).await else {
            panic!("discover failed");
        };
        assert!(processes.iter().all(|p| p.pid != pid));
        daemon.scan_agents().await.unwrap();
        assert!(
            !std::iter::from_fn(|| events.try_recv().ok())
                .any(|e| matches!(e.kind, EventKind::AgentVanished { pid: p, .. } if p == pid)),
            "announced once"
        );

        // Gone: a second fake that exits is announced as vanished.
        let mut short = std::process::Command::new(&fake)
            .arg("30")
            .current_dir(&project)
            .spawn()
            .unwrap();
        let short_pid = short.id();
        daemon.scan_agents().await.unwrap();
        assert!(std::iter::from_fn(|| events.try_recv().ok()).any(
            |e| matches!(e.kind, EventKind::AgentDiscovered { pid: p, .. } if p == short_pid)
        ));
        short.kill().unwrap();
        short.wait().unwrap();
        daemon.scan_agents().await.unwrap();
        assert!(
            std::iter::from_fn(|| events.try_recv().ok())
                .any(|e| matches!(e.kind, EventKind::AgentVanished { pid: p, adopted: false, .. } if p == short_pid))
        );
        child.kill().unwrap();
        child.wait().unwrap();
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
                        amount: None,
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
                        pid: Some(pid),
                        session: None,
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
            amount: 0,
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

    async fn status_of(daemon: &Arc<Daemon>, reference: &str) -> AgentStatus {
        match daemon
            .handle(Request::Inspect {
                agent: reference.to_owned(),
            })
            .await
        {
            Response::Agent { agent } => agent.status,
            other => panic!("unexpected {other:?}"),
        }
    }

    #[tokio::test]
    async fn removing_a_watched_worktree_keeps_real_deletions_without_false_conflicts() {
        if !have_git() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        let worktree = dir.path().join("temporary-checkout");
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(repo.join("src/a.rs"), "a\n").unwrap();
        assert!(git(dir.path(), &repo, &["init", "-q"]));
        assert!(git(dir.path(), &repo, &["add", "."]));
        assert!(git(dir.path(), &repo, &["commit", "-q", "-m", "root"]));
        assert!(git(
            dir.path(),
            &repo,
            &["worktree", "add", "--detach", worktree.to_str().unwrap()]
        ));
        let daemon = open(&dir);
        daemon.expect_watcher();
        let watcher = tokio::spawn(crate::watcher::run(
            daemon.clone(),
            std::time::Duration::from_millis(50),
            std::time::Duration::from_millis(50),
        ));
        let home = register_in(&daemon, "home", &repo).await;
        daemon.refresh_project_checkouts().await;
        daemon.ensure_watched(&home).await.unwrap();
        // Reconcile also covers discovered checkouts without registered agents.
        assert!(
            daemon
                .watch_targets()
                .iter()
                .any(|target| target.dir == project::canonical(&worktree))
        );
        std::fs::remove_dir_all(&worktree).unwrap();
        std::fs::remove_file(repo.join("src/a.rs")).unwrap();
        eventually(async || {
            let entries = ledger(&daemon, &repo, Some("src/a.rs")).await;
            entries
                .iter()
                .any(|entry| entry.kind == ChangeKind::Removed)
                .then_some(())
        })
        .await;
        eventually(async || daemon.recent_events(200).iter().any(|event| {
            matches!(&event.kind, EventKind::WatcherGap { reason } if reason.contains("removed or became unavailable"))
        }).then_some(())).await;
        // Drain callbacks already queued by the OS before checking absence.
        let flush = lock(&daemon.watcher_flush).clone().unwrap();
        let (ack, done) = oneshot::channel();
        flush.send(ack).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), done)
            .await
            .unwrap()
            .unwrap();
        let entries = ledger(&daemon, &repo, Some("src/a.rs")).await;
        assert!(
            entries.iter().all(|entry| entry.worktree.is_none()),
            "{entries:?}"
        );
        assert!(
            !daemon
                .recent_events(200)
                .iter()
                .any(|event| matches!(&event.kind, EventKind::ChannelOpened { .. }))
        );
        watcher.abort();
        let _ = watcher.await;
    }

    #[tokio::test]
    async fn a_second_checkout_touching_a_path_opens_a_channel_by_itself() {
        if !have_git() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(repo.join("src/a.rs"), "a\n").unwrap();
        std::fs::write(repo.join("src/b.rs"), "b\n").unwrap();
        assert!(git(dir.path(), &repo, &["init", "-q"]));
        assert!(git(dir.path(), &repo, &["add", "."]));
        assert!(git(dir.path(), &repo, &["commit", "-q", "-m", "root"]));
        let daemon = open(&dir);
        daemon.expect_watcher();
        tokio::spawn(crate::watcher::run(
            daemon.clone(),
            std::time::Duration::from_secs(60),
            std::time::Duration::from_millis(50),
        ));
        register_in(&daemon, "home", &repo).await;
        let mut command = spec("isolated");
        command.workdir = Some(repo.clone());
        command.isolate = true;
        command.command = vec!["sh".into(), "-c".into(), "sleep 10".into()];
        let Response::Agent { agent: isolated } =
            daemon.handle(Request::Run { spec: command }).await
        else {
            panic!("isolated run failed");
        };
        let worktree = isolated.spec.workdir.clone().unwrap();

        // One checkout changing a path is nobody's business.
        std::fs::write(repo.join("src/a.rs"), "a from main\n").unwrap();
        let channels = |daemon: Arc<Daemon>| async move {
            match daemon
                .handle(Request::Channels {
                    project: String::new(),
                    all: true,
                    agent: Some("home".into()),
                })
                .await
            {
                Response::Channels { channels } => channels,
                other => panic!("{other:?}"),
            }
        };

        // The second checkout on the same path is: a channel opens itself
        // with both agents in it.
        std::fs::write(worktree.join("src/a.rs"), "a from the worktree\n").unwrap();
        let opened = eventually(async || {
            let found = channels(daemon.clone()).await;
            (!found.is_empty()).then_some(found)
        })
        .await;
        assert_eq!(opened.len(), 1, "{opened:?}");
        let channel = &opened[0];
        assert_eq!(channel.members.len(), 2, "both agents are in it");
        assert!(channel.opened_by.is_none(), "the daemon opened it");
        assert_eq!(channel.paths(), [PathBuf::from("src/a.rs")]);
        assert!(daemon.recent_events(200).iter().any(
            |e| matches!(&e.kind, EventKind::ChannelOpened { members, .. } if members.len() == 2)
        ));

        // A second contested path joins the same room rather than opening
        // another.
        std::fs::write(repo.join("src/b.rs"), "b from main\n").unwrap();
        std::fs::write(worktree.join("src/b.rs"), "b from the worktree\n").unwrap();
        let widened = eventually(async || {
            let found = channels(daemon.clone()).await;
            found.first().filter(|c| c.paths().len() == 2).cloned()
        })
        .await;
        assert_eq!(channels(daemon.clone()).await.len(), 1, "one room, not two");
        assert_eq!(
            widened.paths(),
            [PathBuf::from("src/a.rs"), PathBuf::from("src/b.rs")]
        );

        // Both were told, and the journal says the room exists.
        assert!(matches!(
            daemon
                .handle(Request::Inbox {
                    agent: "home".into(),
                    drain: true,
                })
                .await,
            Response::Messages { messages } if !messages.is_empty()
        ));
        let Response::Journal { entries, .. } = daemon
            .handle(Request::Journal {
                project: repo.to_string_lossy().into_owned(),
                since_seq: None,
                until_seq: None,
                agent: None,
                branch: None,
                kind: Some("review".into()),
                path: None,
                grep: None,
                limit: 50,
                digest: None,
            })
            .await
        else {
            panic!("journal failed");
        };
        assert!(!entries.is_empty(), "the channel is in the journal");

        // When the last member leaves, the room closes itself.
        daemon
            .handle(Request::Deregister {
                agent: "home".into(),
            })
            .await;
        daemon
            .handle(Request::Stop {
                agent: isolated.id.to_string(),
                force: true,
            })
            .await;
        let closed = eventually(async || {
            channels(daemon.clone())
                .await
                .into_iter()
                .find(|c| !c.is_open())
        })
        .await;
        assert_eq!(closed.resolution.as_deref(), Some("everyone left"));
    }

    #[tokio::test]
    async fn a_managed_agent_with_a_tty_gets_a_terminal_and_a_session() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let mut command = spec("interactive");
        command.workdir = Some(dir.path().to_path_buf());
        command.tty = true;
        // Proves it is a terminal, then waits so the session is still there
        // to inspect.
        command.command = vec![
            "sh".into(),
            "-c".into(),
            "test -t 0 && echo I_HAVE_A_TTY; sleep 5".into(),
        ];
        let Response::Agent { agent } = daemon.handle(Request::Run { spec: command }).await else {
            panic!("managed launch failed");
        };
        assert!(agent.spec.tty);

        // The daemon holds its terminal, and what it prints reaches both a
        // watcher and the log.
        let session = eventually(async || daemon.session(&agent.id)).await;
        let (_, mut watching) = session.attach();
        let printed = eventually(async || {
            let log = std::fs::read_to_string(daemon.log_path(&agent.id)).unwrap_or_default();
            log.contains("I_HAVE_A_TTY").then_some(log)
        })
        .await;
        assert!(printed.contains("I_HAVE_A_TTY"), "{printed}");

        // Typing at it arrives: `cat` echoes on a terminal, so what we
        // send comes straight back to anyone attached.
        session.input.send(b"hello\n".to_vec()).await.unwrap();
        let echoed = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let mut seen = String::new();
            while let Ok(bytes) = watching.recv().await {
                seen.push_str(&String::from_utf8_lossy(&bytes));
                if seen.contains("hello") {
                    return seen;
                }
            }
            seen
        })
        .await
        .unwrap_or_default();
        assert!(
            echoed.contains("hello"),
            "input reached the terminal: {echoed:?}"
        );

        // Resizing is accepted while it lives.
        session.resize(100, 30).unwrap();

        // Attaching late still shows what it printed, and shows it once:
        // the scrollback and the live stream are taken together.
        let (seen, mut live) = session.attach();
        let seen = String::from_utf8_lossy(&seen).into_owned();
        assert!(seen.contains("I_HAVE_A_TTY"), "the screen so far: {seen:?}");
        session.input.send(b"after\n".to_vec()).await.unwrap();
        let next = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let mut got = String::new();
            while let Ok(bytes) = live.recv().await {
                got.push_str(&String::from_utf8_lossy(&bytes));
                if got.contains("after") {
                    return got;
                }
            }
            got
        })
        .await
        .unwrap_or_default();
        assert!(next.contains("after"), "and what comes next: {next:?}");
        assert!(
            !next.contains("I_HAVE_A_TTY"),
            "without repeating the history: {next:?}"
        );

        // When it ends, so does the terminal: an attach afterwards has
        // nothing to connect to.
        daemon
            .handle(Request::Stop {
                agent: agent.id.to_string(),
                force: true,
            })
            .await;
        eventually(async || daemon.session(&agent.id).is_none().then_some(())).await;
        // Keep the client's Session alive: it must not hold a sender that
        // prevents an existing attachment from observing terminal EOF.
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                match live.recv().await {
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                }
            }
        })
        .await
        .expect("an attached terminal closes when its child exits");
        let (final_history, mut ended) = session.attach();
        assert!(!final_history.is_empty());
        assert!(matches!(
            ended.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Closed)
        ));
    }

    #[tokio::test]
    async fn an_agent_without_a_tty_has_no_session() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let mut command = spec("piped");
        command.workdir = Some(dir.path().to_path_buf());
        command.command = vec!["sh".into(), "-c".into(), "echo piped; sleep 5".into()];
        let Response::Agent { agent } = daemon.handle(Request::Run { spec: command }).await else {
            panic!("managed launch failed");
        };
        assert!(!agent.spec.tty);
        let printed = eventually(async || {
            let log = std::fs::read_to_string(daemon.log_path(&agent.id)).unwrap_or_default();
            log.contains("piped").then_some(log)
        })
        .await;
        assert!(printed.contains("piped"));
        assert!(daemon.session(&agent.id).is_none(), "pipes, not a terminal");
        daemon
            .handle(Request::Stop {
                agent: agent.id.to_string(),
                force: true,
            })
            .await;
    }

    #[tokio::test]
    async fn run_isolate_gives_the_agent_its_own_worktree_and_branch() {
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
        let daemon =
            Arc::new(Daemon::open(dir.path().join("state"), dir.path().join("sock")).unwrap());
        let home = register_in(&daemon, "home", &repo).await;

        let mut command = spec("writer");
        command.workdir = Some(repo.clone());
        command.isolate = true;
        command.command = vec!["sh".into(), "-c".into(), "sleep 0.2".into()];
        let Response::Agent { agent } = daemon
            .handle(Request::Run {
                spec: command.clone(),
            })
            .await
        else {
            panic!("isolated run failed");
        };
        let worktree = agent
            .spec
            .workdir
            .clone()
            .expect("workdir moved to the worktree");
        let worktrees = project::canonical(&paths::worktree_dir(&daemon.home));
        assert!(worktree.starts_with(&worktrees), "{}", worktree.display());
        assert!(worktree.ends_with("writer"), "{}", worktree.display());
        assert!(worktree.join(".git").exists(), "a linked worktree");
        assert!(agent.spec.isolate);
        let project = agent.project.clone().expect("in a project");
        assert_eq!(
            project.id(),
            home.project.as_ref().unwrap().id(),
            "same repository"
        );
        assert_eq!(project.worktree.as_deref(), Some(worktree.as_path()));
        assert_eq!(
            agent.vcs.as_ref().and_then(|v| v.branch.as_deref()),
            Some("agent/writer")
        );
        assert!(
            daemon
                .recent_events(50)
                .iter()
                .any(|e| matches!(&e.kind, EventKind::WorktreeCreated { agent: who, path } if *who == agent.id && *path == worktree))
        );

        // The same name again, once the first has finished: the path is
        // taken, so the second gets its id appended.
        eventually(async || (!status_of(&daemon, "writer").await.is_live()).then_some(())).await;
        let Response::Agent { agent: again } = daemon
            .handle(Request::Run {
                spec: command.clone(),
            })
            .await
        else {
            panic!("second isolated run failed");
        };
        let second = again.spec.workdir.clone().unwrap();
        assert_ne!(second, worktree);
        assert!(
            second
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("writer-")
        );
        assert_eq!(
            again.vcs.as_ref().and_then(|v| v.branch.as_deref()),
            Some(format!("agent/writer-{}", again.id.short()).as_str())
        );

        // A name git could not take: refused before anything is created.
        let mut command = spec("bad name!");
        command.workdir = Some(repo.clone());
        command.isolate = true;
        command.command = vec!["sh".into(), "-c".into(), "true".into()];
        assert!(matches!(
            daemon.handle(Request::Run { spec: command }).await,
            Response::Error { code: ErrorCode::Invalid, message, .. } if message.contains("bad name!")
        ));
        assert!(!worktrees.join("bad name!").exists());
        for (name, ok) in [
            ("writer", true),
            ("w.1_2-3", true),
            ("-lead", false),
            (".hidden", false),
            ("a..b", false),
            ("x.lock", false),
            ("with space", false),
            ("", false),
        ] {
            assert_eq!(isolate_name_ok(name), ok, "{name:?}");
        }

        // Not a repository: refused, nothing spawned.
        let plain = dir.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        let mut command = spec("loose");
        command.workdir = Some(plain);
        command.isolate = true;
        command.command = vec!["sh".into(), "-c".into(), "true".into()];
        assert!(matches!(
            daemon.handle(Request::Run { spec: command }).await,
            Response::Error {
                code: ErrorCode::Invalid,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn launch_refuses_failed_or_unconfirmed_checkout_coverage() {
        if !have_git() {
            return;
        }
        for failure in ["startup", "attachment", "timeout"] {
            let dir = TempDir::new().unwrap();
            let repo = dir.path().join("repo");
            std::fs::create_dir(&repo).unwrap();
            assert!(git(dir.path(), &repo, &["init", "-q"]));
            assert!(git(
                dir.path(),
                &repo,
                &["commit", "-q", "--allow-empty", "-m", "root"]
            ));
            let daemon = open(&dir);
            daemon.expect_watcher();
            let (tx, mut rx) = mpsc::channel::<WatcherAttachment>(1);
            let responder = if failure == "startup" {
                daemon.watcher_off("startup failed".into());
                None
            } else if failure == "attachment" {
                daemon.set_watcher_attach(tx);
                let expected = project::canonical(&repo);
                Some(tokio::spawn(async move {
                    let request = rx.recv().await.unwrap();
                    assert_eq!(request.checkout, expected);
                    request
                        .ack
                        .send(Err("watch installation failed".into()))
                        .unwrap();
                }))
            } else {
                None
            };
            let mut command = spec("blocked-writer");
            command.workdir = Some(repo.clone());
            command.command = vec!["sh".into(), "-c".into(), "touch should-not-exist".into()];
            let response = daemon.handle(Request::Run { spec: command }).await;
            assert!(
                matches!(
                    response,
                    Response::Error {
                        code: ErrorCode::Unavailable,
                        ..
                    }
                ),
                "{failure}: {response:?}"
            );
            assert!(!repo.join("should-not-exist").exists());
            assert!(lock(&daemon.state).supervised.is_empty());
            assert_eq!(lock(&daemon.state).registry.live().count(), 0);
            if let Some(task) = responder {
                task.await.unwrap();
            }
        }
    }

    #[tokio::test]
    async fn failed_isolated_launches_clean_up_only_unchanged_worktrees() {
        if !have_git() {
            return;
        }
        for failure in ["duplicate", "watcher", "edited"] {
            let dir = TempDir::new().unwrap();
            let repo = dir.path().join("repo");
            std::fs::create_dir(&repo).unwrap();
            assert!(git(dir.path(), &repo, &["init", "-q"]));
            assert!(git(
                dir.path(),
                &repo,
                &["commit", "-q", "--allow-empty", "-m", "root"]
            ));
            let daemon =
                Arc::new(Daemon::open(dir.path().join("state"), dir.path().join("sock")).unwrap());
            let mut responder = None;
            if failure == "duplicate" {
                register_in(&daemon, "blocked", &repo).await;
            } else if failure == "watcher" {
                daemon.watcher_off("cannot start".into());
            } else {
                let (tx, mut rx) = mpsc::channel::<WatcherAttachment>(1);
                daemon.set_watcher_attach(tx);
                responder = Some(tokio::spawn(async move {
                    let request = rx.recv().await.unwrap();
                    std::fs::write(request.checkout.join("keep-me"), "user edit").unwrap();
                    request.ack.send(Err("attachment failed".into())).unwrap();
                }));
            }
            let mut command = spec("blocked");
            command.isolate = true;
            command.workdir = Some(repo.clone());
            command.command = vec!["sh".into(), "-c".into(), "touch should-not-run".into()];
            assert!(matches!(
                daemon.handle(Request::Run { spec: command }).await,
                Response::Error { .. }
            ));
            if let Some(task) = responder {
                task.await.unwrap();
            }
            let events = daemon.recent_events(30);
            let (path, removed) = events
                .iter()
                .find_map(|event| match &event.kind {
                    EventKind::WorktreeCleanup {
                        path,
                        worktree_removed,
                        ..
                    } => Some((path.clone(), *worktree_removed)),
                    _ => None,
                })
                .expect("cleanup is observable");
            assert!(!path.join("should-not-run").exists());
            if failure == "edited" {
                assert!(!removed);
                assert_eq!(
                    std::fs::read_to_string(path.join("keep-me")).unwrap(),
                    "user edit"
                );
            } else {
                assert!(removed, "{failure}");
                assert!(!path.exists());
                assert!(
                    git(dir.path(), &repo, &["branch", "agent/blocked"]),
                    "failed branch was removed and its name is reusable"
                );
            }
        }
    }

    #[tokio::test]
    async fn overlap_names_paths_changed_in_two_checkouts() {
        if !have_git() {
            return;
        }
        let dir = TempDir::new().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(repo.join("src/a.rs"), "a\n").unwrap();
        std::fs::write(repo.join("src/b.rs"), "b\n").unwrap();
        assert!(git(dir.path(), &repo, &["init", "-q"]));
        assert!(git(dir.path(), &repo, &["add", "."]));
        assert!(git(dir.path(), &repo, &["commit", "-q", "-m", "root"]));
        let daemon =
            Arc::new(Daemon::open(dir.path().join("state"), dir.path().join("sock")).unwrap());
        daemon.expect_watcher();
        tokio::spawn(crate::watcher::run(
            daemon.clone(),
            std::time::Duration::from_secs(60),
            std::time::Duration::from_millis(50),
        ));
        let a = register_in(&daemon, "a", &repo).await;
        let mut command = spec("b");
        command.workdir = Some(repo.clone());
        command.isolate = true;
        command.command = vec!["sh".into(), "-c".into(), "sleep 5".into()];
        let Response::Agent { agent: b } = daemon.handle(Request::Run { spec: command }).await
        else {
            panic!("isolated run failed");
        };
        let worktree = b.spec.workdir.clone().unwrap();
        std::fs::write(repo.join("src/a.rs"), "a from main\n").unwrap();
        std::fs::write(worktree.join("src/a.rs"), "a from the worktree\n").unwrap();
        std::fs::write(worktree.join("src/b.rs"), "only here\n").unwrap();
        let overlaps = eventually(async || {
            match daemon
                .handle(Request::Overlap {
                    project: repo.to_string_lossy().into_owned(),
                    since_seq: None,
                    agent: None,
                })
                .await
            {
                Response::Overlap { overlaps } if !overlaps.is_empty() => Some(overlaps),
                _ => None,
            }
        })
        .await;
        assert_eq!(overlaps.len(), 1, "{overlaps:?}");
        assert_eq!(overlaps[0].path, Path::new("src/a.rs"));
        assert_eq!(overlaps[0].parties.len(), 2);
        assert!(
            overlaps[0]
                .parties
                .iter()
                .any(|p| p.checkout == project::canonical(&repo))
        );
        assert!(
            overlaps[0]
                .parties
                .iter()
                .any(|p| p.checkout == worktree && p.worktree.is_some())
        );
        // Nobody held the files: external on both sides.
        assert!(overlaps[0].parties.iter().all(|p| p.agents.is_empty()));
        // Seen from an agent: its checkout must be a party, and an empty
        // project means its own.
        let Response::Overlap { overlaps: mine } = daemon
            .handle(Request::Overlap {
                project: String::new(),
                since_seq: None,
                agent: Some(a.id.to_string()),
            })
            .await
        else {
            panic!("overlap failed");
        };
        assert_eq!(mine.len(), 1);
        assert!(matches!(
            daemon
                .handle(Request::Overlap {
                    project: String::new(),
                    since_seq: None,
                    agent: None,
                })
                .await,
            Response::Error {
                code: ErrorCode::Invalid,
                ..
            }
        ));
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
    async fn reject_lease_transition(operation: &str) {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let owner = register(&daemon, "owner", None).await;
        let original = if operation == "new" {
            None
        } else {
            let Response::Lease { lease } = claim(&daemon, "owner", "task:atomic").await else {
                panic!()
            };
            Some(lease)
        };
        let before_events = daemon.recent_events(100);
        let (before_agent, before_seq) = {
            let state = lock(&daemon.state);
            (
                state.registry.get(&owner.id).unwrap().clone(),
                state.next_seq,
            )
        };
        let mut live = daemon.subscribe_events();
        lock(&daemon.state)
            .store
            .reject_event_for_test(if operation == "new" {
                "lease_claimed"
            } else {
                "lease_renewed"
            });
        let response = if operation == "renew" {
            daemon
                .handle(Request::Renew {
                    agent: "owner".into(),
                    lease: original.as_ref().unwrap().id.clone(),
                    ttl_secs: 3600,
                })
                .await
        } else {
            daemon
                .handle(Request::Claim {
                    agent: "owner".into(),
                    resource: "task:atomic".into(),
                    mode: LeaseMode::Exclusive,
                    amount: None,
                    ttl_secs: 3600,
                    note: None,
                    wait_secs: 0,
                })
                .await
        };
        assert!(
            matches!(
                response,
                Response::Error {
                    code: ErrorCode::StorageUnavailable,
                    ..
                }
            ),
            "{response:?}"
        );
        let state = lock(&daemon.state);
        let expected: Vec<_> = original.into_iter().collect();
        assert_eq!(
            state.store.load_leases().unwrap(),
            expected,
            "lease cannot commit without its replay event"
        );
        assert_eq!(
            state.leases.all().into_iter().cloned().collect::<Vec<_>>(),
            expected,
            "memory cannot advance after rollback"
        );
        assert_eq!(
            state.registry.get(&owner.id).unwrap(),
            &before_agent,
            "claim liveness is part of the failed transition"
        );
        assert_eq!(state.next_seq, before_seq);
        drop(state);
        assert_eq!(daemon.recent_events(100), before_events);
        assert!(live.try_recv().is_err());
    }

    #[tokio::test]
    async fn rejected_claim_event_rolls_back_new_lease_and_liveness() {
        reject_lease_transition("new").await;
    }

    #[tokio::test]
    async fn rejected_reclaim_event_rolls_back_renewal_and_liveness() {
        reject_lease_transition("reclaim").await;
    }

    #[tokio::test]
    async fn rejected_renew_event_rolls_back_renewal_and_liveness() {
        reject_lease_transition("renew").await;
    }

    #[tokio::test]
    async fn rejected_claim_row_rolls_back_its_liveness_and_event() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let owner = register(&daemon, "owner", None).await;
        let before = daemon.recent_events(100);
        let mut live = daemon.subscribe_events();
        lock(&daemon.state)
            .store
            .reject_lease_change_for_test("INSERT");
        assert!(matches!(
            claim(&daemon, "owner", "task:atomic").await,
            Response::Error {
                code: ErrorCode::StorageUnavailable,
                ..
            }
        ));
        let state = lock(&daemon.state);
        assert!(state.leases.is_empty());
        assert!(state.store.load_leases().unwrap().is_empty());
        assert_eq!(state.registry.get(&owner.id), Some(&owner));
        assert_eq!(state.store.load_agents().unwrap(), [owner]);
        drop(state);
        assert_eq!(daemon.recent_events(100), before);
        assert!(live.try_recv().is_err());
    }

    #[tokio::test]
    async fn rejected_conflict_event_preserves_requester_liveness_and_held_lease() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        register(&daemon, "owner", None).await;
        let peer = register(&daemon, "peer", None).await;
        let Response::Lease { lease } = claim(&daemon, "owner", "task:atomic").await else {
            panic!()
        };
        let before = daemon.recent_events(100);
        let mut live = daemon.subscribe_events();
        lock(&daemon.state)
            .store
            .reject_event_for_test("lease_conflict");
        assert!(matches!(
            claim(&daemon, "peer", "task:atomic").await,
            Response::Error {
                code: ErrorCode::StorageUnavailable,
                ..
            }
        ));
        let state = lock(&daemon.state);
        assert_eq!(state.registry.get(&peer.id), Some(&peer));
        assert_eq!(
            state
                .store
                .load_agents()
                .unwrap()
                .into_iter()
                .find(|a| a.id == peer.id),
            Some(peer)
        );
        assert_eq!(state.leases.get(&lease.id), Some(&lease));
        assert_eq!(state.store.load_leases().unwrap(), [lease]);
        drop(state);
        assert_eq!(daemon.recent_events(100), before);
        assert!(live.try_recv().is_err());
    }

    #[tokio::test]
    async fn rejected_release_deletion_keeps_memory_and_replay_protection() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        register(&daemon, "owner", None).await;
        let Response::Lease { lease } = claim(&daemon, "owner", "task:atomic").await else {
            panic!()
        };
        let before = daemon.recent_events(100);
        let mut live = daemon.subscribe_events();
        lock(&daemon.state)
            .store
            .reject_lease_change_for_test("DELETE");
        assert!(matches!(
            daemon
                .handle(Request::Release {
                    agent: "owner".into(),
                    lease: lease.id.clone(),
                    summary: None,
                    summary_source: SummarySource::Explicit
                })
                .await,
            Response::Error {
                code: ErrorCode::StorageUnavailable,
                ..
            }
        ));
        let state = lock(&daemon.state);
        assert_eq!(state.leases.get(&lease.id), Some(&lease));
        assert_eq!(state.store.load_leases().unwrap(), [lease]);
        drop(state);
        assert_eq!(daemon.recent_events(100), before);
        assert!(live.try_recv().is_err());
    }

    #[tokio::test]
    async fn failed_liveness_write_does_not_advance_memory() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let owner = register(&daemon, "owner", None).await;
        let mut state = lock(&daemon.state);
        let mut before = state.registry.get(&owner.id).unwrap().clone();
        before.last_seen = Utc::now() - chrono::Duration::seconds(30);
        state.store.upsert_agent(&before).unwrap();
        *state.registry.get_mut(&owner.id).unwrap() = before.clone();
        state.store.reject_agent_writes_for_test();
        state.touch(&owner.id);
        assert!(state.storage_failure().is_some());
        assert_eq!(state.registry.get(&owner.id).unwrap(), &before);
        assert_eq!(state.store.load_agents().unwrap(), [before]);
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
            assert_eq!(
                lock(&daemon.state).leases.get(&lease.id),
                Some(&lease),
                "failed release must retain protection in memory"
            );
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
            for filtered in [false, true] {
                let mut query = JournalQuery::new(project.clone(), 200);
                if filtered {
                    query.grep = Some("absent".into());
                }
                let Response::Journal {
                    entries, head_seq, ..
                } = state.journal_query(query)
                else {
                    panic!()
                };
                assert!(entries.is_empty());
                assert_eq!(
                    head_seq,
                    Some(last),
                    "pruning preserves the durable snapshot head"
                );
            }
        }
        drop(daemon);
        let daemon = open(&dir);
        let Response::Journal {
            entries, head_seq, ..
        } = lock(&daemon.state).journal_query(JournalQuery::new(project.clone(), 200))
        else {
            panic!()
        };
        assert!(entries.is_empty());
        assert_eq!(head_seq, Some(last), "empty snapshot head survives restart");
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

#[cfg(test)]
mod leak_detector_proof {
    /// Deliberately orphans a process that holds this test's output
    /// pipe. Ignored, so it never runs in the suite; run it by name to
    /// confirm the leak detector still fails a genuine leak:
    ///
    /// ```text
    /// cargo nextest run -p agentd -E 'test(a_real_leak_is_still_caught)' --run-ignored all
    /// ```
    #[test]
    #[ignore = "proves the leak detector works; leaves a process for 30s on purpose"]
    fn a_real_leak_is_still_caught() {
        // Deliberately never reaped: an orphan holding this test's
        // output pipe is exactly what is being demonstrated, and
        // waiting for it would defeat the point.
        #[allow(clippy::zombie_processes)]
        let _orphan = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("sleep");
    }
}
