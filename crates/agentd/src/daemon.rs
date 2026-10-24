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
use agentdocker_core::session::{Transfer, TransferState};
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
#[cfg(unix)]
use nix::errno::Errno;
#[cfg(unix)]
use nix::sys::signal::{Signal, kill};
#[cfg(unix)]
use nix::unistd::Pid;
use serde_json::{Value, json};
use tokio::sync::{Notify, broadcast, mpsc, oneshot, watch};
use tracing::{debug, error, info, warn};

use agentdocker_host::{multiplexer, procinfo, project, vcs};

use crate::store::{ChangesQuery, JournalQuery, Store};
use crate::supervisor;
mod access;
pub(crate) mod authorship;
mod binding;
mod channels;
mod checkouts;
// Codex's shared server listens on a Unix socket; Windows has none.
#[cfg(unix)]
pub(crate) mod codex_threads;
mod containers;
mod contests;
mod conversations;
mod discovery;
mod handoff;
pub(crate) mod herdr;
pub mod humans;
mod images;
mod inbox;
mod journal;
mod judge;
mod launch;
mod leases;
mod panes;
mod pause;
mod person;
pub mod policies;
mod provider;
mod read_content;
mod recovery;
mod relay;
pub mod reload;
mod restarts;
mod restore;
mod retention;
mod review;
mod roles;
mod secret_reviews;
mod tasks;
mod transport;
mod usage;
mod waiting;
mod webhooks;
mod working;
mod worktrees;

/// Unacknowledged addressed messages per agent. Live streams do not consume them.
const INBOX_CAPACITY: usize = 1000;
const INBOX_BYTES: usize = 4 * 1024 * 1024;
/// How long the person's word to everyone in a project waits for a session
/// that opens there after it, and how many of their latest messages are
/// looked at.
const CATCH_UP_WINDOW: Duration = Duration::hours(1);
const CATCH_UP_MESSAGES: usize = 50;

fn message_bytes(message: &Envelope) -> usize {
    // Refuse admission if a future envelope representation cannot be encoded.
    serde_json::to_vec(message).map_or(usize::MAX, |bytes| bytes.len())
}
/// Leases longer than this are clamped; a TTL is a liveness bound, not a
/// reservation.
const MAX_LEASE_TTL_SECS: u64 = 24 * 60 * 60;
/// Stored event history is trimmed to this many entries.
const EVENT_HISTORY: usize = 10_000;
/// Change observations kept per project. The ledger answers "who touched
/// what lately" (`changes`, `blame`, stale reads), not history; a hundred
/// thousand rows a project was fifty megabytes of database on a busy
/// checkout, for a daemon meant to be light.
const CHANGE_HISTORY: usize = 20_000;
/// The daemon's own log is trimmed past this many bytes.
const DAEMON_LOG_CAP: u64 = 4 * 1024 * 1024;
/// Longest a claim may wait for a conflicting lease to clear.
const MAX_WAIT_SECS: u64 = 600;

/// `now - secs` when that is representable. An age beyond what the clock can
/// express is older than anything stored, so `None` means "nothing is that
/// old" rather than a panic.
fn cutoff_before(secs: u64) -> Option<DateTime<Utc>> {
    let age = Duration::try_seconds(i64::try_from(secs).ok()?)?;
    Utc::now().checked_sub_signed(age)
}
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
/// How long a launch waits for it. Nothing has started yet, and a new
/// worktree on a busy machine (builds writing thousands of files in other
/// watched checkouts) can take seconds to cover: failing the launch over
/// it left the agent dead before it began.
const WATCHER_LAUNCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
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

/// Why a registration of Codex's detached app-server is refused.
const DETACHED_CODEX_HOST: &str = "this process is Codex's detached app-server, which hosts a \
     Codex session's MCP servers and is not a session; its tool calls act as the Codex \
     conversation that makes them";

/// A document written or removed in the same transaction as a message,
/// with the event that announces it: what must not exist without the
/// words that go with it.
pub(crate) struct DocumentTransition {
    pub kind: &'static str,
    pub id: String,
    /// The document to put, or none to delete it.
    pub value: Option<serde_json::Value>,
    pub event: EventKind,
}

/// What became of a write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
enum Persisted {
    Committed,
    /// Fenced: nothing was written and nothing is wrong. Memory stays.
    Skipped,
    /// The store failed and the failure is latched.
    Failed,
}

/// One admitted mutation in progress; dropping it, however the request
/// ends, releases its place.
pub(crate) struct InFlight(Arc<Daemon>);

impl Drop for InFlight {
    fn drop(&mut self) {
        let mut state = lock(&self.0.state);
        state.in_flight = state.in_flight.saturating_sub(1);
    }
}

tokio::task_local! {
    /// The admitted mutation's place, kept where its handler can give it
    /// up: a request that has written what it will write and now only
    /// waits (an `ask` for its answer, a `claim --wait` for its lease)
    /// must not hold an offer back for as long as it waits.
    static ADMISSION: std::cell::RefCell<Option<InFlight>>;
}

/// A recovery write held back until this process is allowed to write.
/// A recovery write a fenced open held back for the acceptance. It takes
/// its event sequence numbers when it runs, from the number given, and
/// says how many it used: assigned at open they would sit below the
/// acceptance's, a hole in the durable head if anything stopped between.
type DeferredWrite = Box<dyn FnOnce(&Store, u64) -> anyhow::Result<u64> + Send>;

/// Who may write the database from this process.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Coordination {
    Serving,
    /// Offered to a successor; every write here is refused until settled.
    Quiescing {
        transfer: String,
    },
    /// The successor accepted; this process is leaving without writing.
    Transferred {
        transfer: String,
    },
}

/// Whether a request would write coordination state. Reads are served
/// throughout a transfer from the projection this process still holds;
/// everything else answers `transferring` until a coordinator owns the
/// database again. `Shutdown` and `Reload` steer the daemon itself
/// rather than its data: neither is fenced, and neither counts as a
/// mutation in flight, or a reload would refuse itself.
fn mutates(request: &Request) -> bool {
    !matches!(
        request,
        Request::Shutdown
            | Request::Reload
            | Request::Ping
            | Request::DaemonProcess
            | Request::Images
            | Request::Stale { .. }
            | Request::Reads { .. }
            | Request::Checkpoints { .. }
            | Request::Handoffs { .. }
            | Request::Validations { .. }
            | Request::WorktreeDiff { .. }
            | Request::ReviewBranch { .. }
            | Request::ReviewGate { .. }
            | Request::List { .. }
            | Request::Inspect { .. }
            | Request::Changes { .. }
            | Request::Overlap { .. }
            | Request::Questions { .. }
            | Request::Activity { .. }
            | Request::Waiting
            | Request::Pauses
            | Request::Contests { .. }
            | Request::Journal { digest: None, .. }
            | Request::Channels { .. }
            | Request::Conversations { .. }
            | Request::History { .. }
            | Request::Delivery { .. }
            | Request::Thread { .. }
            | Request::SearchMessages { .. }
            | Request::Leases { .. }
            | Request::WebhookDeliveries { .. }
            | Request::Tasks { .. }
            | Request::TaskGet { .. }
            | Request::Boards { .. }
            | Request::Rules { .. }
            | Request::Tags { .. }
            | Request::RoleCatalog { .. }
            | Request::TaskSearch { .. }
            | Request::Plans { .. }
            | Request::PlanGet { .. }
            | Request::MemorySearch { .. }
            | Request::MemoryProfile { .. }
            | Request::Files { .. }
            | Request::FileGet { .. }
            | Request::Usage { .. }
            | Request::Events { .. }
            | Request::ResumeEvents { .. }
            | Request::Logs { .. }
            | Request::PeekInput { .. }
            | Request::Subscribe { .. }
            | Request::Attach { .. }
    )
}

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
    /// A memory mirror run is under way; the next waits for the next tick.
    mirroring: std::sync::atomic::AtomicBool,
    /// The opt-in judge's screens, budget and status, off the state lock;
    /// never held across an await or with another lock.
    judge: Mutex<judge::Judge>,
    /// A judge pass is under way; the next waits for the next tick.
    judging: std::sync::atomic::AtomicBool,
    /// The judge's own read of the file and key is under way, apart from
    /// any pass, so a switch-off is seen while a pass waits on an answer.
    judge_watching: std::sync::atomic::AtomicBool,
    scan_finished: Notify,
    /// How session owners are run: as processes of the daemon binary, or
    /// in-process where no daemon binary is on hand (tests).
    owner_mode: supervisor::OwnerMode,
    /// The webhook sinks, run off the state lock; never held across an
    /// await.
    webhooks: Mutex<webhooks::Sinks>,
    /// The listener and daemon lock, kept here from serving onward so a
    /// handover can pass them to a successor. Never held across an await.
    held: Mutex<Option<reload::Held>>,
    /// Signalled once a successor is serving and this daemon may leave.
    transferred_exit: Notify,
    /// A test's hold on a relaunch between its checks and its commit, so
    /// the record can be changed underneath it. Taken once; never held
    /// across an await.
    #[cfg(test)]
    relaunch_hold: Mutex<Option<Arc<tokio::sync::Barrier>>>,
    /// What was found of the processes that asked to act as the person.
    /// Never held with the state lock.
    peers: Mutex<person::Peers>,
    /// Whether a request acting as the person is checked against the
    /// process that sent it (`AGENTDOCKER_PERSON_CHECK=off` at start, for a
    /// test harness's own daemon, turns it off).
    person_check: bool,
    /// The `agentdocker` command the post-commit hook of a worktree the
    /// daemon creates reports with: the CLI beside the daemon, when it is
    /// there. Never held across an await.
    witness_cli: Mutex<Option<PathBuf>>,
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
    /// Cards an agent is being started on, by the rule that asked (none
    /// for the person): not started on twice, and counted against the
    /// rule's limit until the agent has the card.
    starting_cards: BTreeMap<agentdocker_core::TaskId, Option<String>>,
    /// Watcher attachment/gap boundary for each checkout. An attachment and
    /// a flush acknowledgement do not prove OS event completeness; clearing a
    /// changed read additionally requires matching current content evidence.
    ledger_since: HashMap<PathBuf, DateTime<Utc>>,
    content_proofs: read_content::Proofs,
    /// Whether this process may write the database. Serving is the
    /// ordinary case. Offering a transfer stops every writer here,
    /// request or tick, until the successor accepts (this process exits
    /// without touching agents) or the offer is aborted (writing resumes).
    /// A process that opened the database while an offer was pending
    /// starts fenced and may write only after accepting as the successor.
    coordination: Coordination,
    /// The successor a handover in flight may let reach session owners, by
    /// pid and birth as read when it was named: held in memory only, and
    /// taken away the moment the attempt is given up, whatever the store
    /// manages to record.
    owner_successor: Option<(u32, DateTime<Utc>)>,
    /// Startup recovery writes held back by a fenced open; the successor
    /// runs them immediately after accepting, before serving anything.
    deferred_recovery: Vec<DeferredWrite>,
    /// Mutating requests admitted past the fence and still executing. An
    /// offer waits for this to reach zero so its final write follows every
    /// side effect it might otherwise race.
    in_flight: usize,
    /// The last write was skipped by the fence. Every existing "did it
    /// land" check reads `write_failure()`, so a skip is reported there
    /// as `transferring` until a later write lands or the offer is
    /// aborted; unlike a storage failure it is not latched for good, and
    /// unlike one it never refuses a read.
    skipped_write: bool,
    registry: Registry,
    leases: LeaseTable,
    inboxes: HashMap<AgentId, VecDeque<Envelope>>,
    inbox_bytes: HashMap<AgentId, usize>,
    live_subscribers: HashMap<AgentId, usize>,
    supervised: HashMap<AgentId, tokio::sync::watch::Sender<Option<bool>>>,
    pending_restarts: HashSet<AgentId>,
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
    /// The last `agentd.toml` problem reported, so a broken file is logged
    /// once when it breaks and once when it is fixed, not every minute.
    config_notice: Option<String>,
    /// The agent-retention notice, kept apart from `config_notice`: the
    /// journal-retention tick clears that one, and would otherwise re-warn
    /// here every minute for a problem that has not changed.
    agent_config_notice: Option<String>,
    /// The last HEAD a commit entry was written for, per checkout, so a
    /// move seen through several agents is journaled once.
    last_head: HashMap<PathBuf, String>,
    /// The branch each checkout was last seen on, beside `last_head`, so
    /// a move can be told from a branch switch for a checkout that has
    /// no agent record to remember it.
    last_branch: HashMap<PathBuf, Option<String>>,
    /// Who asked the daemon for each linked worktree it created, so a
    /// commit there is theirs without a lease to say so. In memory only:
    /// a daemon that restarts falls back to the leases.
    worktree_creators: HashMap<PathBuf, AgentId>,
    /// Every checkout of each project, by project id: the main one and
    /// its linked worktrees. Refreshed off the lock on the same tick as
    /// the VCS sweep, because enumerating them runs git.
    project_checkouts: HashMap<ProjectId, Vec<PathBuf>>,
    /// Checkouts the daemon is committing in right now. The watcher polls
    /// on its own schedule and will see HEAD move part-way through, so
    /// without this it writes its own guessed-at entry for a commit the
    /// daemon is about to record properly.
    committing: std::collections::BTreeSet<PathBuf>,
    /// A shared installation pin per binding that has a launch descriptor,
    /// held for the binding's life so the release its controller runs
    /// from is not pruned while the controller is dead and waiting to be
    /// started again.
    controller_pins: HashMap<AgentId, agentdocker_host::lock::Lock>,
    /// Questions whose synchronous `ask` is waiting on its connection now.
    /// An answer to one of these is held for that ask rather than shown to
    /// the asker's queue; the ask hands it over or, ending without it,
    /// releases it. In memory only: a daemon that restarts has no asks
    /// waiting, and everything held goes to the queue.
    question_waiters: HashSet<MessageId>,
    /// Answers held for a waiting ask, by answer id, with their question.
    held_answers: HashMap<MessageId, MessageId>,
    /// Stale notices waiting for the next tick, per reader: each path that
    /// changed after its observation, with the change that did it last. A
    /// branch switch touches hundreds of paths in a moment; the reader
    /// gets one message naming them, not one per change.
    pending_stale: HashMap<AgentId, BTreeMap<PathBuf, Change>>,
    /// The stale notice each reader was last sent, while it is still
    /// queued: nothing more is sent until that one has been consumed
    /// (acknowledged, so it left the queue); the paths that change in the
    /// meantime wait and the next notice names them all.
    stale_outstanding: HashMap<AgentId, MessageId>,
    /// Paths that widened a contested channel since the last tick, told
    /// to the channel as one message: the distinct paths, bounded, and
    /// how many widenings there were.
    pending_contested: HashMap<ChannelId, (std::collections::BTreeSet<PathBuf>, usize)>,
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
    secret_reviews: HashMap<String, secret_reviews::Entry>,
    /// The projects told to hold, and why; kept as documents.
    pauses: HashMap<ProjectId, agentdocker_core::Pause>,
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
    /// The screen judge's generation and how it stands, as this lock sees
    /// them. A judgment is recorded only under the generation in force
    /// here while the judge is on, and a stop clears every standing
    /// judgment in the same hold that records it, so nothing judged under
    /// an earlier setup is written after a stop has cleared; a change of
    /// how it stands is announced in that hold too (see `judge.rs`).
    judge_generation: u64,
    judge_status: agentdocker_core::judgment::JudgeState,
    judge_reason: Option<String>,
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
    use std::sync::atomic::AtomicUsize;
    static SAMPLES: AtomicUsize = AtomicUsize::new(0);
    let Some(elapsed) = started.map(|start| start.elapsed()) else {
        return;
    };
    if elapsed >= std::time::Duration::from_millis(250) && claim_state_timing_sample(&SAMPLES) {
        tracing::debug!(target: "agentd_state_timing", operation,
            elapsed_ms = elapsed.as_secs_f64() * 1000.0, "slow daemon state operation");
    }
}

fn claim_state_timing_sample(samples: &std::sync::atomic::AtomicUsize) -> bool {
    use std::sync::atomic::Ordering;
    let mut count = samples.load(Ordering::Relaxed);
    while count < 256 {
        match samples.compare_exchange_weak(count, count + 1, Ordering::Relaxed, Ordering::Relaxed)
        {
            Ok(_) => return true,
            Err(observed) => count = observed,
        }
    }
    false
}

#[test]
fn concurrent_state_timing_samples_stop_at_the_limit() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let samples = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        let workers: Vec<_> = (0..8)
            .map(|_| {
                scope.spawn(|| {
                    (0..100)
                        .filter(|_| claim_state_timing_sample(&samples))
                        .count()
                })
            })
            .collect();
        let accepted: usize = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .sum();
        assert_eq!(accepted, 256);
    });
    assert_eq!(samples.load(Ordering::Relaxed), 256);
    assert!(!claim_state_timing_sample(&samples));
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

/// A claim refused because the project is paused: the person's reason,
/// and enough to say who paused and when.
fn pause_refusal(pause: &agentdocker_core::Pause) -> Response {
    Response::Error {
        code: ErrorCode::Paused,
        message: format!(
            "the project is paused: {}; finish the step in hand and take nothing new until it is resumed",
            pause.reason
        ),
        details: Some(serde_json::json!({
            "project": pause.project,
            "reason": pause.reason,
            "by": pause.by,
            "at": pause.at,
        })),
    }
}

fn registry_error(err: RegistryError) -> Response {
    let code = match err {
        RegistryError::IdentityReserved(_) => ErrorCode::Conflict,
        RegistryError::NameTaken(_) => ErrorCode::NameTaken,
        RegistryError::NotFound(_) | RegistryError::RoleNotFound(_) => ErrorCode::NotFound,
        RegistryError::Ambiguous(_)
        | RegistryError::ProjectAmbiguous(_)
        | RegistryError::RoleAmbiguous(_)
        | RegistryError::RoleShadowed(_) => ErrorCode::Ambiguous,
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

/// Whether `pid` is still the process born at `born`: strictly, so a
/// process that is gone or whose birth cannot be read is not it.
#[cfg(unix)]
fn born_as(pid: u32, born: DateTime<Utc>) -> bool {
    procinfo::start_time(pid) == Some(born)
}

/// Where a stopped terminal program ran: its terminal, held open since
/// before the signal, and the shell that started it, by pid and birth.
#[cfg(unix)]
struct RanIn {
    terminal: agentdocker_host::terminal::Pinned,
    tty: String,
    shell: u32,
    shell_born: DateTime<Utc>,
}

/// Once the stopped terminal program `pid` has gone, give its terminal back
/// clean: the modes it set (mouse reporting above all) are turned off. Only
/// when the same shell (pid and birth) has its own job back in the
/// terminal's foreground: a job it started next keeps its modes, and a
/// terminal closed meanwhile has its device revoked, so nothing reaches
/// another window. Off the caller's thread; a program that outlives the
/// wait, or a shell that never takes the terminal back, is left alone.
#[cfg(unix)]
fn restore_terminal_after(pid: u32, started: DateTime<Utc>, mut ran: RanIn) {
    let _ = std::thread::Builder::new()
        .name("restore-terminal".into())
        .spawn(move || {
            let wait_until = |seconds: u64, done: &mut dyn FnMut() -> bool| {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(seconds);
                loop {
                    if done() {
                        return true;
                    }
                    if std::time::Instant::now() >= deadline {
                        return false;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            };
            if !wait_until(10, &mut || {
                !(process_exists(pid) && same_process(pid, Some(started)))
            }) {
                return;
            }
            let shell_is_back = wait_until(2, &mut || {
                born_as(ran.shell, ran.shell_born)
                    && agentdocker_host::terminal::foreground_of(ran.shell)
                        .is_some_and(|place| place.holds(&ran.tty))
            });
            if shell_is_back
                && born_as(ran.shell, ran.shell_born)
                && let Err(error) = ran.terminal.restore_modes()
            {
                debug!(%error, tty = ran.tty, "could not restore the stopped program's terminal");
            }
        });
}

/// Is there a process with this pid? `EPERM` means it exists but belongs to
/// someone else, which still counts as alive. Zero and out-of-range values
/// would address process groups, so they are never alive.
/// A pid a signal could address: positive, and within the range the OS
/// uses. Unix keeps the typed pid; Windows only needs the check.
#[cfg(unix)]
fn signal_pid(pid: u32) -> Option<Pid> {
    let raw = i32::try_from(pid).ok()?;
    (raw > 0).then(|| Pid::from_raw(raw))
}

#[cfg(windows)]
fn signal_pid(pid: u32) -> Option<u32> {
    i32::try_from(pid).ok().filter(|raw| *raw > 0).map(|_| pid)
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

/// What a stopped daemon leaves behind in its session directory, taken
/// away: the lock file of every owner nobody holds any more (an owner's
/// lock outlives its socket on purpose, so a late opener never locks a
/// dead inode), with that owner's socket if it was killed before it
/// could unlink it — never an exit report, which waits for a daemon to
/// record it, and never a lock somebody still holds. Then the directory
/// itself when nothing is left, and the short `/tmp` directory a long
/// home falls back to when it is empty too: a machine that runs test
/// daemons by the hundred, or one person's daemon for a year, keeps
/// nothing in `/tmp` for a daemon that is gone. Returns true while an owner
/// still holds its lock, so shutdown can wait for acknowledged owners to retire.
pub(crate) fn sweep_sessions(home: &std::path::Path) -> bool {
    let dir = agentdocker_core::session::sessions_dir(home);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return false;
    };
    let mut held_owners = false;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "lock") {
            continue;
        }
        // Held by a living owner: theirs, left alone. Ours stays held
        // until both files are gone: an owner for the same id that opens
        // the lock file meanwhile finds it taken, not an inode about to be
        // unlinked from under a lock it just won.
        let _held = match agentdocker_host::lock::try_exclusive_existing(&path) {
            Ok(Some(held)) => held,
            Ok(None) => {
                held_owners = true;
                continue;
            }
            Err(_) => continue,
        };
        let exit = path.with_extension("exit");
        if exit.exists() {
            // An unacknowledged exit report is a daemon's to read.
            continue;
        }
        let _ = std::fs::remove_file(path.with_extension("sock"));
        let _ = std::fs::remove_file(&path);
    }
    // Non-recursive on purpose: anything still inside keeps the directory.
    let _ = std::fs::remove_dir(&dir);
    let short = agentdocker_core::paths::short_socket_dir(home);
    if short != home && dir.starts_with(&short) {
        let _ = std::fs::remove_dir(&short);
    }
    held_owners
}

/// The agent a file under `logs/` belongs to: `<id>.log`, `<id>.log.1`
/// and `<id>.controller.log` (a relaunched input receiver's output) all
/// name `<id>`. Anything else is nobody's.
fn log_owner(name: &str) -> Option<&str> {
    let stem = name
        .strip_suffix(".log.1")
        .or_else(|| name.strip_suffix(".log"))?;
    Some(stem.strip_suffix(".controller").unwrap_or(stem))
}

/// Where a relaunched input receiver's output goes (see `binding.rs`).
fn controller_log(home: &Path, id: &AgentId) -> PathBuf {
    home.join("logs").join(format!("{id}.controller.log"))
}

/// Copy a log past `DAEMON_LOG_CAP` to `<name>.1` and truncate it in
/// place, for a file whose writers hold it in append mode.
fn trim_in_place(path: &Path, what: &str) {
    let Ok(mut file) = agentdocker_host::dirs::private_file(path, false, false) else {
        return;
    };
    let Ok(meta) = file.metadata() else {
        return;
    };
    if meta.len() <= DAEMON_LOG_CAP {
        return;
    }
    let rotated = paths::rotated_log(path);
    let copied =
        agentdocker_host::dirs::private_file(&rotated, true, false).and_then(|mut earlier| {
            earlier.set_len(0)?;
            std::io::copy(&mut file, &mut earlier).map(|_| ())
        });
    match copied.and_then(|()| file.set_len(0)) {
        Ok(()) => info!(bytes = meta.len(), path = %path.display(), "trimmed {what}"),
        Err(error) => warn!(%error, path = %path.display(), "could not trim {what}"),
    }
}

/// Whether a process with this pid exists — ours or not; an access
/// refusal is a yes. Both platforms answer through the host crate.
fn process_exists(pid: u32) -> bool {
    signal_pid(pid).is_some_and(|_| procinfo::alive(pid))
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

    /// Record an exit and say whether the store kept it. A store that was
    /// already unavailable, or fails on this write, still retires the
    /// supervision in memory but answers false, so the caller keeps whatever
    /// external evidence of the exit it holds.
    pub fn mark_exited_durably(&self, id: &AgentId, status: AgentStatus) -> bool {
        lock(&self.state).mark_exited_durably(id, status)
    }

    /// Lift the transient provider blocks whose retry is due.
    pub fn retry_providers(&self) {
        lock(&self.state).retry_providers(Utc::now());
    }

    pub fn check_liveness(&self) {
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
                        // Disk first, memory on commit: a fenced or failed
                        // write leaves the record live, so the next look
                        // (this daemon's after an abort, or the successor's)
                        // finds it again.
                        let mut agent = candidate.clone();
                        agent.status = AgentStatus::Stopping;
                        if state.persist("agent", |store| store.upsert_agent(&agent))
                            == Persisted::Committed
                        {
                            state.registry.set_status(
                                &candidate.id,
                                AgentStatus::Stopping,
                                Utc::now(),
                            );
                            state.emit(EventKind::AgentStopping {
                                agent: candidate.id,
                                force: false,
                            });
                        }
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
        // Successful live handover returns from serve() before stop_all();
        // successor-owned sessions never reach this shutdown sweep.
        // Recording an exit removes its supervision entry before the owner
        // receives the acknowledgement and releases its lock. Wait within the
        // same shutdown budget for that retirement, then sweep once it is safe.
        // Held locks and unacknowledged exit reports remain protected.
        while sweep_sessions(&self.home) {
            if tokio::time::Instant::now() >= deadline {
                warn!("session owners did not finish retirement; held files retained");
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    }

    fn stop(&self, reference: &str, force: bool) -> Response {
        let record = {
            let mut state = lock(&self.state);
            if let Some(error) = state.storage_failure().or_else(|| state.transferring()) {
                return error;
            }
            let id = match state.registry.resolve(reference) {
                Ok(id) => id,
                Err(e) => return registry_error(e),
            };
            let record = state.registry.get(&id).unwrap().clone();
            if !record.status.is_live() {
                return Response::error(ErrorCode::Invalid, "agent has already finished");
            }
            if let Some(control) = state.supervised.get(&id).cloned() {
                // Save the intent and event before the supervisor can signal the child.
                let agent = match state.commit_stop(record, force) {
                    Ok(agent) => agent,
                    Err(error) => return *error,
                };
                control.send_modify(|pending| *pending = Some(force || pending.unwrap_or(false)));
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
            let agent = {
                let mut state = lock(&self.state);
                let Some(current) = state.registry.get(&record.id) else {
                    return Response::error(ErrorCode::NotFound, "agent vanished");
                };
                if current.pid != record.pid
                    || current.process_started_at != record.process_started_at
                {
                    return Response::error(
                        ErrorCode::Conflict,
                        "agent identity changed during stop",
                    );
                }
                if !current.status.is_live() {
                    return Response::Agent {
                        agent: current.clone(),
                    };
                }
                let current = current.clone();
                match state.commit_stop(current, force) {
                    Ok(agent) => agent,
                    Err(error) => return *error,
                }
            };
            // The identity checked above is the identity ended: on Unix a
            // signal to the pid (or the managed process group it leads);
            // on Windows the host crate terminates the process through a
            // handle whose birth it compares first, so a recycled pid is
            // never ended by mistake.
            #[cfg(unix)]
            {
                // A terminal program AgentDocker did not start ends without
                // undoing its terminal modes; read where it runs first.
                let restore = (!record.managed)
                    .then(|| {
                        let tty = record
                            .terminal
                            .as_ref()
                            .and_then(|host| host.tty.clone())
                            .or_else(|| agentdocker_host::terminal::device_of(pid))?;
                        let shell = procinfo::inspect(pid)?.ppid;
                        let shell_born = procinfo::start_time(shell)?;
                        let terminal = agentdocker_host::terminal::pin(&tty).ok()?;
                        Some(RanIn {
                            terminal,
                            tty,
                            shell,
                            shell_born,
                        })
                    })
                    .flatten();
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
                if let Some(ran) = restore {
                    restore_terminal_after(pid, started, ran);
                }
            }
            #[cfg(windows)]
            {
                let _ = target;
                if let Err(err) = procinfo::end(pid, started, force)
                    && err.kind() != std::io::ErrorKind::NotFound
                {
                    return Response::error(
                        ErrorCode::Forbidden,
                        format!("cannot end pid {pid}: {err}"),
                    );
                }
            }
            return Response::Agent { agent };
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
        match state.mark_exited(&record.id, AgentStatus::Exited { code: None }) {
            Some(agent) => Response::Agent { agent },
            None => state
                .write_failure()
                .unwrap_or_else(|| Response::error(ErrorCode::NotFound, "agent vanished")),
        }
    }
    pub fn expire_leases(&self) {
        let mut state = lock(&self.state);
        state.expire_leases();
        state.expire_questions(Utc::now());
        state.expire_secret_reviews();
    }
    /// Send the stale and contested notices that accumulated since the
    /// last tick, one message per reader and per channel.
    pub fn flush_notices(&self) {
        lock(&self.state).flush_notices();
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
        crate::startup_checkpoint("home_security_started");
        Self::secure_home(&home)?;
        crate::startup_checkpoint("home_security_ready");
        let store = Store::open(&home.join("state.db"))?;
        Self::with_store(home, socket, store)
    }

    /// Open the store as a successor that has not accepted yet: the
    /// schema is brought forward but its recorded version is not, so an
    /// aborted takeover leaves a database the predecessor still opens.
    pub fn open_pending(home: PathBuf, socket: PathBuf) -> anyhow::Result<Self> {
        crate::startup_checkpoint("home_security_started");
        Self::secure_home(&home)?;
        crate::startup_checkpoint("home_security_ready");
        let store = Store::open_pending(&home.join("state.db"))?;
        Self::with_store(home, socket, store)
    }

    fn secure_home(home: &Path) -> anyhow::Result<()> {
        agentdocker_host::dirs::secure_state_dir(home)?;
        let logs = home.join("logs");
        agentdocker_host::dirs::secure_state_dir(&logs)?;
        for entry in std::fs::read_dir(&logs)? {
            let path = entry?.path();
            if path.extension().is_some_and(|ext| ext == "log") {
                agentdocker_host::dirs::private_file(&path, false, false)?;
            }
        }
        let daemon_log = paths::daemon_log(home);
        if std::fs::symlink_metadata(&daemon_log).is_ok() {
            agentdocker_host::dirs::private_file(&daemon_log, false, false)?;
        }
        Ok(())
    }

    pub fn with_store(home: PathBuf, socket: PathBuf, store: Store) -> anyhow::Result<Self> {
        crate::startup_checkpoint("state_restore_started");
        let now = Utc::now();
        // Before any recovery write: is coordination already offered to
        // someone? Then this process may not write until it has accepted
        // that offer as the named successor. Recovery still corrects memory
        // so the projection is right; the writes wait in `deferred` and land
        // as the successor's first act after acceptance. A stranger never
        // gets to run them at all.
        let pending_transfer = store
            .transfer()?
            .filter(|transfer| transfer.state == TransferState::Offered);
        let fenced = pending_transfer.is_some();
        let mut deferred: Vec<DeferredWrite> = Vec::new();
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
        let high_water = store.max_event_seq()?;
        anyhow::ensure!(
            high_water < i64::MAX as u64,
            "durable event sequence exhausted"
        );
        let mut next_seq = high_water + 1;
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
                if fenced {
                    let record = record.clone();
                    deferred.push(Box::new(move |store, seq| {
                        event.seq = seq;
                        store.agent_transition(&record, &event)?;
                        Ok(1)
                    }));
                } else {
                    event.seq = next_seq;
                    store.agent_transition(&record, &event)?;
                    next_seq += 1;
                }
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
                if fenced {
                    let id = lease.id.clone();
                    deferred.push(Box::new(move |store, seq| {
                        event.seq = seq;
                        store.delete_lease_with_event(&id, &event)?;
                        Ok(1)
                    }));
                } else {
                    event.seq = next_seq;
                    store.delete_lease_with_event(&lease.id, &event)?;
                    next_seq += 1;
                }
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
                if fenced {
                    let lease = lease.clone();
                    deferred.push(Box::new(move |store, _| {
                        store.upsert_lease(&lease)?;
                        Ok(0)
                    }));
                } else {
                    store.upsert_lease(&lease)?;
                }
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

        let pauses: HashMap<ProjectId, agentdocker_core::Pause> = store
            .documents::<agentdocker_core::Pause>("pause", None)?
            .into_iter()
            .map(|pause| (pause.project.clone(), pause))
            .collect();
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
        let mut expiration_events: Vec<_> = expired
            .iter()
            .map(|question| {
                Event::new(
                    EventKind::QuestionClosed {
                        question: question.clone(),
                        answer: None,
                    },
                    now,
                )
            })
            .collect();
        if fenced {
            let expired = expired.clone();
            deferred.push(Box::new(move |store, seq| {
                for (index, event) in expiration_events.iter_mut().enumerate() {
                    event.seq = seq + index as u64;
                }
                store.close_questions(&expired, &expiration_events)?;
                Ok(expiration_events.len() as u64)
            }));
        } else {
            for (index, event) in expiration_events.iter_mut().enumerate() {
                event.seq = next_seq + index as u64;
            }
            next_seq += expiration_events.len() as u64;
            store.close_questions(&expired, &expiration_events)?;
        }
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
                starting_cards: BTreeMap::new(),
                ledger_since: HashMap::new(),
                content_proofs: read_content::Proofs::default(),
                coordination: match pending_transfer {
                    Some(transfer) => Coordination::Quiescing {
                        transfer: transfer.id,
                    },
                    None => Coordination::Serving,
                },
                owner_successor: None,
                deferred_recovery: deferred,
                in_flight: 0,
                skipped_write: false,
                registry,
                leases,
                inboxes,
                inbox_bytes,
                live_subscribers: HashMap::new(),
                supervised: HashMap::new(),
                pending_restarts: HashSet::new(),
                container_busy: HashSet::new(),
                transports: HashMap::new(),
                projects,
                next_seq,
                bus,
                events,
                journal_seq: HashMap::new(),
                journal_rings: HashMap::new(),
                config_notice: None,
                agent_config_notice: None,
                last_head: HashMap::new(),
                last_branch: HashMap::new(),
                worktree_creators: HashMap::new(),
                project_checkouts: HashMap::new(),
                committing: std::collections::BTreeSet::new(),
                controller_pins: HashMap::new(),
                question_waiters: HashSet::new(),
                held_answers: HashMap::new(),
                pending_stale: HashMap::new(),
                stale_outstanding: HashMap::new(),
                pending_contested: HashMap::new(),
                journal_cursors: HashMap::new(),
                channels,
                contested: HashMap::new(),
                questions,
                secret_reviews: HashMap::new(),
                pauses,
                waiting: agentdocker_core::WaitQueue::new(),
                notifier: None,
                host_policy: policies::Loaded::default(),
                project_policies: HashMap::new(),
                judge_generation: 0,
                judge_status: agentdocker_core::judgment::JudgeState::Off,
                judge_reason: None,
            }),
            shutdown: Notify::new(),
            watcher_flush: Mutex::new(None),
            scanning: std::sync::atomic::AtomicBool::new(false),
            mirroring: std::sync::atomic::AtomicBool::new(false),
            judge: Mutex::new(judge::Judge::default()),
            judging: std::sync::atomic::AtomicBool::new(false),
            judge_watching: std::sync::atomic::AtomicBool::new(false),
            owner_mode: supervisor::OwnerMode::detect(),
            webhooks: Mutex::new(webhooks::Sinks::default()),
            held: Mutex::new(None),
            transferred_exit: Notify::new(),
            #[cfg(test)]
            relaunch_hold: Mutex::new(None),
            peers: Mutex::new(person::Peers::default()),
            person_check: std::env::var_os("AGENTDOCKER_PERSON_CHECK").is_none_or(|v| v != "off"),
            witness_cli: Mutex::new(person::companion_cli().filter(|cli| cli.is_file())),
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

    /// Forget a record and, with it, the output captured for it: a
    /// forgotten agent's log is nobody's to read, its rotated half too.
    fn remove(&self, reference: &str) -> Response {
        let id = match self.resolve(reference) {
            Ok(id) => id,
            Err(response) => return *response,
        };
        let response = lock(&self.state).remove(&id.to_string());
        if matches!(response, Response::Ok) {
            self.forget_logs(&id);
        }
        response
    }

    /// The logs of a record that is gone: its own and its controller's.
    fn forget_logs(&self, id: &AgentId) {
        let log = self.log_path(id);
        let _ = std::fs::remove_file(&log);
        let _ = std::fs::remove_file(paths::rotated_log(&log));
        let controller = controller_log(&self.home, id);
        let _ = std::fs::remove_file(&controller);
        let _ = std::fs::remove_file(paths::rotated_log(&controller));
    }

    /// Apply `[agents] retention` from `agentd.toml`, thirty days unless
    /// the file says otherwise: the records of agents that ended before
    /// the window and hold nothing — no lease, no open question, no input
    /// binding, no restart or restore policy, no provider limit — are
    /// forgotten the way `rm` forgets one, a bounded batch per tick, each
    /// said as `agent_removed` and the batch as `agents_pruned`. Every
    /// record is loaded at startup and looked at on every tick, so a
    /// daemon that kept them all would slow down for as long as it was
    /// used; the journal and the ledger keep a forgotten agent's id.
    pub fn apply_agent_retention(&self) {
        use agentdocker_core::config::{AGENT_RETENTION_BATCH, FILE_NAME};
        let path = self.home.join(FILE_NAME);
        let read = crate::config::read(&self.home).and_then(|config| {
            config
                .agents_retention()
                .map_err(|error| format!("{}: {error}", path.display()))
        });
        let removed = {
            let mut state = lock(&self.state);
            let window = match read {
                Ok(window) => {
                    state.agent_config_notice = None;
                    window
                }
                Err(notice) => {
                    if state.agent_config_notice.as_ref() != Some(&notice) {
                        warn!(%notice, "agent retention not applied");
                        state.agent_config_notice = Some(notice);
                    }
                    return;
                }
            };
            let Some(cutoff) = window.and_then(|window| Utc::now().checked_sub_signed(window))
            else {
                return;
            };
            let mut removed = Vec::new();
            for id in state.retirable_agents(cutoff, AGENT_RETENTION_BATCH) {
                // A refusal is a write that failed or was fenced: nothing
                // more lands this tick either.
                if !matches!(state.remove(id.as_str()), Response::Ok) {
                    break;
                }
                removed.push(id);
            }
            if !removed.is_empty() {
                info!(
                    count = removed.len(),
                    "forgot the records of agents that ended before the retention window"
                );
                state.emit(EventKind::AgentsPruned {
                    removed: removed.len(),
                });
            }
            removed
        };
        for id in &removed {
            self.forget_logs(id);
        }
    }

    pub fn subscribe_events(&self) -> broadcast::Receiver<Event> {
        lock(&self.state).events.subscribe()
    }

    /// Whether the periodic process scan has anyone to tell: the app, an
    /// `events` stream, a webhook sink. Its findings are events and a
    /// cache; with nobody subscribed, `discover` scans when it is asked,
    /// and the daemon forks no `ps` every five seconds for nobody.
    pub fn discovery_watched(&self) -> bool {
        lock(&self.state).events.receiver_count() > 0
    }

    /// A daemon-configuration problem, said once per distinct notice
    /// and cleared when the file reads again.
    pub(crate) fn config_notice(&self, notice: String) {
        let mut state = lock(&self.state);
        if state.config_notice.as_ref() != Some(&notice) {
            warn!(%notice, "daemon configuration ignored");
            state.config_notice = Some(notice);
        }
    }

    /// Whether a synchronous `ask` is waiting on its connection for this
    /// question right now.
    #[cfg(test)]
    pub(crate) fn question_waiting(&self, question: &MessageId) -> bool {
        lock(&self.state).question_waiters.contains(question)
    }

    /// Subscribe and validate durable history in the same state snapshot.
    pub(crate) fn resume_events(
        &self,
        after: Option<&agentdocker_core::EventCursor>,
    ) -> Result<(broadcast::Receiver<Event>, crate::store::EventReplay), Box<Response>> {
        let state = lock(&self.state);
        if let Some(error) = state.storage_failure() {
            return Err(Box::new(error));
        }
        let receiver = state.events.subscribe();
        let replay = state
            .store
            .event_replay(after)
            .map_err(|error| {
                Box::new(Response::error(
                    ErrorCode::StorageUnavailable,
                    format!("cannot read durable event history: {error}"),
                ))
            })?
            .map_err(|error| Box::new(Response::error(ErrorCode::EventHistoryLost, error)))?;
        Ok((receiver, replay))
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
        // Admission and the in-flight count move together under the lock:
        // an offer that finds the count at zero knows no admitted mutation
        // is still executing, so its final write follows every side effect.
        let admitted = mutates(&request);
        let in_flight = if admitted {
            let mut state = lock(&self.state);
            if let Some(refusal) = state.transferring() {
                return refusal;
            }
            state.in_flight += 1;
            // Released on every exit, including a future dropped because
            // the client hung up mid-request; a leaked count would refuse
            // offers for the rest of this daemon's life.
            Some(InFlight(self.clone()))
        } else {
            None
        };
        // Boxed: `handle_healthy` is one match over every request the
        // protocol has, so the future it returns is as large as the
        // biggest arm plus everything the match holds live across an
        // await. Left inline it lands on the caller's stack, and every
        // caller that awaits a `handle` inside its own async fn pays for
        // it again — which overflows a thread stack once the protocol is
        // big enough. On the heap it costs one allocation per request.
        let control = !admitted;
        let response = ADMISSION
            .scope(
                std::cell::RefCell::new(in_flight),
                Box::pin(self.handle_healthy(request)),
            )
            .await;
        if control {
            // Reads and control requests answer for themselves: a write the
            // fence skipped meanwhile is not their failure.
            return response;
        }
        lock(&self.state).write_failure().unwrap_or(response)
    }

    /// This request has written what it will write and is about to wait.
    /// Give its in-flight place up, so an offer need not wait with it;
    /// anything it writes after this is fenced like a tick writer's.
    fn done_writing(&self) {
        let _ = ADMISSION.try_with(|place| place.borrow_mut().take());
    }

    /// Keep background host work ahead of any transfer offer, just like an
    /// admitted request. The guard covers its asynchronous host side effects.
    pub(crate) fn admit_background(self: &Arc<Self>) -> Result<InFlight, Box<Response>> {
        let mut state = lock(&self.state);
        if let Some(error) = state.storage_failure().or_else(|| state.transferring()) {
            return Err(Box::new(error));
        }
        state.in_flight += 1;
        Ok(InFlight(self.clone()))
    }

    /// The wait is over and the request will write again: take a place
    /// back first. Refused, with nothing applied, once coordination has
    /// been offered meanwhile.
    fn readmit(self: &Arc<Self>) -> Result<(), Box<Response>> {
        let place = {
            let mut state = lock(&self.state);
            if let Some(refusal) = state.transferring() {
                return Err(Box::new(refusal));
            }
            state.in_flight += 1;
            InFlight(self.clone())
        };
        // Outside a request (a direct call in a test) the place goes
        // straight back with the guard.
        let _ = ADMISSION.try_with(|slot| *slot.borrow_mut() = Some(place));
        Ok(())
    }

    async fn handle_healthy(self: &Arc<Self>, request: Request) -> Response {
        match request {
            Request::RecordEmission { agent, emission } => self.record_emission(&agent, emission),
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
                links,
                release_leases,
            } => {
                self.checkpoint(
                    &agent,
                    key,
                    task,
                    assumptions,
                    next_steps,
                    links,
                    release_leases,
                )
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
                links,
                transfer_leases,
                key,
            } => {
                self.handoff(
                    &agent,
                    to.as_deref(),
                    task,
                    note,
                    links,
                    transfer_leases,
                    key,
                )
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
                from,
                seed_build,
            } => {
                self.worktree_create(&agent, path, branch, from, seed_build)
                    .await
            }
            Request::WorktreeDiff { agent } => self.worktree_diff(&agent).await,
            Request::WorktreeRemove { agent } => self.worktree_remove(&agent).await,
            Request::ReviewBranch { agent } => self.review_branch(&agent).await,
            Request::MergeBranch { from, agent, head } => {
                self.merge_branch(&from, &agent, &head).await
            }
            Request::Commit {
                agent,
                message,
                all,
                push,
            } => self.commit(&agent, message, all, push).await,
            Request::ReportCommit {
                checkout,
                token,
                head,
                branch,
                agent,
            } => {
                self.report_commit(checkout, token, head, branch, agent)
                    .await
            }
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
            Request::Usage {
                project,
                agent,
                since,
                until,
                by,
            } => {
                self.usage(agentdocker_core::usage::report::Query {
                    project,
                    agent,
                    since,
                    until,
                    by,
                })
                .await
            }
            Request::Ping => Response::Pong {
                version: env!("CARGO_PKG_VERSION").to_owned(),
                uptime_secs: self.started.elapsed().as_secs(),
                restricted: match self.restricted() {
                    RestrictedEndpoint::On(socket) => Some(socket),
                    _ => None,
                },
                pid: Some(std::process::id()),
                executable: agentdocker_host::procinfo::executable_path().ok(),
                experimental: Some(crate::config::experimental(&self.home)),
                judge: Some(self.judge_status()),
            },
            Request::DaemonProcess => {
                let state = lock(&self.state);
                let successor = match &state.coordination {
                    Coordination::Quiescing { .. } | Coordination::Transferred { .. } => {
                        state.owner_successor
                    }
                    Coordination::Serving => None,
                };
                Response::DaemonProcess {
                    pid: std::process::id(),
                    started_at: agentdocker_host::procinfo::start_time(std::process::id()),
                    successor: successor.map(|(pid, _)| pid),
                    successor_started_at: successor.map(|(_, born)| born),
                }
            }
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
            Request::Register { spec, pid, session } => {
                let registered = self.register(spec, pid, session).await;
                self.trace_starter(&registered);
                registered
            }
            Request::SessionAttach {
                runtime,
                profile,
                session,
                workdir,
                adapter,
            } => {
                let attached = self
                    .session_attach(runtime, profile, session, workdir, adapter)
                    .await;
                self.trace_starter(&attached);
                attached
            }
            Request::ResumeSession { agent, spec } => self.resume_session(&agent, spec).await,
            Request::Deregister { agent } => lock(&self.state).deregister(&agent),
            Request::Discover => self.discover().await,
            Request::Runtimes => self.runtimes().await,
            Request::Adopt { pid, name, runtime } => {
                let adopted = self.adopt(pid, name, runtime).await;
                self.trace_starter(&adopted);
                adopted
            }
            Request::Stop { agent, force } => self.stop_agent(&agent, force).await,
            Request::Remove { agent } => self.remove(&agent),
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
            Request::Role { agent, role } => lock(&self.state).set_role(&agent, role, Utc::now()),
            Request::RoleAssign {
                from,
                agent,
                add,
                remove,
            } => lock(&self.state).role_assign(&from, &agent, &add, &remove),
            Request::Rename { agent, name } => lock(&self.state).rename(&agent, name, Utc::now()),
            Request::ReportAdapter {
                agent,
                adapter,
                contact,
            } => lock(&self.state).report_adapter(&agent, adapter, contact, Utc::now()),
            Request::ReportInput {
                agent,
                process_started_at,
                observed_at,
                report,
                token,
            } => lock(&self.state).report_input(
                &agent,
                process_started_at,
                observed_at,
                report,
                token.as_deref(),
                Utc::now(),
            ),
            Request::ReportProvider {
                agent,
                process_started_at,
                observed_at,
                report,
            } => lock(&self.state).report_provider(
                &agent,
                process_started_at,
                observed_at,
                report,
                Utc::now(),
            ),
            Request::ResumeProvider { agent, blocked_at } => {
                lock(&self.state).resume_provider(&agent, blocked_at, Utc::now())
            }
            Request::BindInput {
                agent,
                provider,
                controller,
                token,
                launch,
            } => lock(&self.state).bind_input(
                &agent,
                provider,
                controller,
                &token,
                launch,
                Utc::now(),
            ),
            Request::UpgradeController {
                agent,
                provider,
                controller,
                previous,
                launch,
                token,
            } => lock(&self.state).upgrade_controller(
                &agent,
                (provider, controller, previous),
                launch,
                &token,
                Utc::now(),
            ),
            Request::UnbindInput {
                agent,
                token,
                force,
            } => lock(&self.state).unbind_input(&agent, token.as_deref(), force, Utc::now()),
            Request::RetryController { agent } => {
                lock(&self.state).retry_controller(&agent, Utc::now())
            }
            Request::ResumeInput {
                agent,
                predecessor,
                provider,
                launch,
            } => lock(&self.state).resume_input(&agent, &predecessor, provider, launch, Utc::now()),
            Request::DeliveryQueue { agent } => {
                let mut state = lock(&self.state);
                match state.input_consumer(&agent, false) {
                    Ok(()) => {
                        let response = state.delivery_queue(&agent);
                        state.offered_to_legacy(&agent, &response);
                        response
                    }
                    Err(error) => *error,
                }
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
                links,
            } => self.send(from, &to, kind, payload, reply_to, links).await,
            Request::Me { workdir } => self.me(workdir).await,
            Request::OpenSecretReview {
                agent,
                owner,
                recipient,
                request,
                token,
            } => lock(&self.state).open_secret_review(&agent, owner, &recipient, request, token),
            Request::SecretReviews { recipient } => {
                lock(&self.state).list_secret_reviews(&recipient)
            }
            Request::AnswerSecretReview {
                from,
                review,
                answers,
                retention_acknowledged,
            } => lock(&self.state).answer_secret_review(
                &from,
                &review,
                answers,
                retention_acknowledged,
            ),
            Request::CancelSecretReview { from, review } => {
                lock(&self.state).cancel_secret_review(&from, &review)
            }
            Request::PollSecretReview {
                agent,
                owner,
                review,
                token,
                close,
            } => lock(&self.state).poll_secret_review(&agent, &owner, &review, &token, close),
            Request::Ask {
                from,
                to,
                question,
                timeout_secs,
            } => self.ask(from, to, question, timeout_secs).await,
            Request::PostQuestion {
                from,
                to,
                question,
                presentation,
                timeout_secs,
            } => {
                self.post_question(from, to, question, presentation, timeout_secs)
                    .await
            }
            Request::CancelQuestion { agent, message } => self.cancel_question(&agent, &message),
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
            Request::TaskCreate {
                from,
                project,
                title,
                acceptance,
                column,
                links,
                board,
                lane,
                description,
                priority,
                size,
                due,
                external_blocker,
                parent,
                blocked_by,
                tags,
                plan,
            } => {
                let card = agentdocker_core::protocol::BatchCard {
                    reference: None,
                    title,
                    acceptance,
                    description,
                    lane: None,
                    priority,
                    size,
                    due,
                    external_blocker,
                    parent,
                    blocked_by,
                    tags,
                    links,
                    plan,
                };
                self.task_file(
                    tasks::Filing {
                        from,
                        project,
                        board,
                        lane,
                        column,
                        tags: Vec::new(),
                        cards: vec![card],
                    },
                    false,
                )
                .await
            }
            Request::TaskCreateMany {
                from,
                project,
                board,
                lane,
                tags,
                cards,
            } => {
                self.task_file(
                    tasks::Filing {
                        from,
                        project,
                        board,
                        lane,
                        column: None,
                        tags,
                        cards,
                    },
                    true,
                )
                .await
            }
            Request::TaskGet { task } => self.task_get(&task),
            Request::TaskPull {
                agent,
                task,
                take_over_from,
                project,
                board,
            } => {
                self.task_pull(
                    &agent,
                    task.as_deref(),
                    take_over_from.as_deref(),
                    project.as_deref(),
                    board.as_deref(),
                )
                .await
            }
            Request::TaskMove {
                agent,
                task,
                column,
                to,
                after,
                before,
                comment,
            } => self.task_move(
                &agent,
                &task,
                column,
                to.as_deref(),
                after.as_deref(),
                before.as_deref(),
                comment.as_deref(),
            ),
            Request::TaskUpdate {
                agent,
                task,
                title,
                acceptance,
                assignee,
                links,
                description,
                priority,
                size,
                due,
                external_blocker,
                parent,
                blocked_by,
                tags,
                plan,
                expected_updated_at,
            } => self.task_update(
                &agent,
                &task,
                tasks::TaskChange {
                    expected_updated_at,
                    title,
                    acceptance,
                    assignee,
                    links,
                    description,
                    priority,
                    size,
                    due,
                    external_blocker,
                    parent,
                    blocked_by,
                    tags,
                    plan,
                },
            ),
            Request::TaskArchive { agent, task } => self.task_archive(&agent, &task),
            Request::Rules { project, board } => self.rules(project, board).await,
            Request::RuleCreate {
                from,
                project,
                board,
                name,
                trigger,
                action,
            } => {
                self.rule_create(&from, project, board, &name, trigger, action)
                    .await
            }
            Request::RuleUpdate {
                from,
                rule,
                name,
                trigger,
                action,
                enabled,
            } => self.rule_update(&from, &rule, name, trigger, action, enabled),
            Request::RuleDelete { from, rule } => self.rule_delete(&from, &rule),
            Request::TaskStart {
                from,
                task,
                runtime,
                args,
                workdir,
            } => self.task_start(&from, &task, runtime, args, workdir).await,
            Request::Tags { project } => self.tags(project).await,
            Request::TagSave {
                from,
                project,
                name,
                color,
                rename,
            } => self.tag_save(&from, project, &name, color, rename).await,
            Request::TagDelete {
                from,
                project,
                name,
            } => self.tag_delete(&from, project, &name).await,
            Request::RoleCatalog { project } => self.role_catalog(project).await,
            Request::RoleSave {
                from,
                project,
                role,
                name,
                color,
                description,
                tools,
            } => {
                self.role_save(&from, project, role, &name, color, description, tools)
                    .await
            }
            Request::RoleDelete {
                from,
                project,
                role,
            } => self.role_delete(&from, project, &role).await,
            Request::TaskSearch {
                query,
                project,
                archived,
                limit,
            } => self.task_search(&query, project, archived, limit).await,
            Request::Plans { project, archived } => self.plans(project, archived).await,
            Request::PlanGet { plan } => self.plan_get(&plan),
            Request::PlanSave {
                from,
                project,
                plan,
                title,
                body,
                status,
            } => {
                self.plan_save(&from, project, plan, title, body, status)
                    .await
            }
            Request::MemoryAdd {
                from,
                project,
                text,
                links,
                supersedes,
                standing,
                forget_after,
            } => {
                self.memory_add(
                    &from,
                    project,
                    &text,
                    links,
                    supersedes,
                    standing,
                    forget_after,
                )
                .await
            }
            Request::MemorySearch {
                from,
                project,
                query,
                limit,
            } => self.memory_search(from, project, &query, limit).await,
            Request::MemoryProfile {
                from,
                project,
                max_chars,
            } => self.memory_profile(from, project, max_chars).await,
            Request::MemoryForget { from, memory } => self.memory_forget(&from, &memory),
            Request::Files { project, task } => self.files(project, task).await,
            Request::FilePut {
                from,
                project,
                name,
                media_type,
                text,
                base64,
                task,
            } => {
                self.file_put(&from, project, &name, media_type, text, base64, task)
                    .await
            }
            Request::FileGet { file } => self.file_get(&file),
            Request::FileLink {
                from,
                file,
                task,
                unlink,
            } => self.file_link(&from, &file, &task, unlink),
            Request::FileDelete { from, file } => self.file_delete(&from, &file),
            Request::TaskUnarchive { agent, task } => self.task_unarchive(&agent, &task),
            Request::TaskArchiveLane {
                from,
                project,
                board,
                lane,
            } => {
                self.task_archive_lane(&from, project.as_deref(), board.as_deref(), &lane)
                    .await
            }
            Request::TaskComment {
                from,
                task,
                body,
                reply_to,
            } => self.task_comment(&from, &task, &body, reply_to.as_deref()),
            Request::TaskCommentEdit {
                from,
                comment,
                body,
            } => self.task_comment_change(&from, &comment, Some(&body)),
            Request::TaskCommentDelete { from, comment } => {
                self.task_comment_change(&from, &comment, None)
            }
            Request::Boards { project } => self.boards(project.as_deref(), None).await,
            Request::BoardCreate {
                from,
                project,
                name,
                template,
            } => {
                self.board_create(&from, project.as_deref(), &name, &template)
                    .await
            }
            Request::BoardUpdate {
                from,
                project,
                board,
                name,
                lanes,
                pull_source,
                pull_target,
            } => {
                self.board_update(
                    &from,
                    project.as_deref(),
                    &board,
                    name,
                    lanes,
                    pull_source,
                    pull_target,
                )
                .await
            }
            Request::Tasks {
                project,
                column,
                archived,
                offset,
                limit,
                board,
                lane,
                assignee,
                agent,
                open,
                query,
                tag,
            } => {
                self.tasks(tasks::TaskQuery {
                    project,
                    column,
                    archived,
                    offset,
                    limit,
                    board,
                    lane,
                    assignee,
                    agent,
                    open,
                    query,
                    tag,
                })
                .await
            }
            Request::Pause {
                from,
                project,
                reason,
            } => self.pause(from, project, reason).await,
            Request::ResumeProject { from, project } => self.resume_project(from, project).await,
            Request::Pauses => self.pauses(),
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
            Request::Inbox { agent, drain } => {
                let mut state = lock(&self.state);
                // Even a read without drain may be the session's own reader
                // putting the text in front of the model, and a bound
                // controller cannot be told that this happened between its
                // snapshot and its enqueue; so a bound queue answers
                // `input_owned` to every legacy read, and a person looks
                // with `peek_input`. A managed bridge session keeps its
                // non-draining read for the app, as before.
                match if drain {
                    state.input_consumer(&agent, false)
                } else {
                    state.readable_inbox(&agent)
                } {
                    Ok(()) => {
                        let response = state.inbox(&agent, drain);
                        if !drain {
                            state.offered_to_legacy(&agent, &response);
                        }
                        response
                    }
                    Err(error) => *error,
                }
            }
            Request::PeekInput { agent } => {
                let mut state = lock(&self.state);
                match state.resolve(&agent) {
                    Ok(id) => match state.read_inbox(&id, false) {
                        Ok(messages) => Response::Messages { messages },
                        Err(error) => *error,
                    },
                    Err(error) => *error,
                }
            }
            Request::AckInbox { agent, messages } => {
                let mut state = lock(&self.state);
                match state.legacy_ack_allowed(&agent, &messages) {
                    Ok(()) => {
                        let response = state.ack_inbox(&agent, &messages);
                        if matches!(response, Response::Ok)
                            && let Ok(id) = state.resolve(&agent)
                        {
                            state.forget_delivered(&id, &messages);
                        }
                        response
                    }
                    Err(error) => *error,
                }
            }
            Request::ProviderInbox {
                agent,
                acknowledge,
                token,
            } => {
                let mut state = lock(&self.state);
                if let Some(token) = token.as_deref()
                    && state
                        .resolve(&agent)
                        .ok()
                        .and_then(|id| state.registry.get(&id).map(|r| r.input_binding.is_some()))
                        .unwrap_or(false)
                {
                    return state.bound_read(&agent, &acknowledge, token);
                }
                if let Ok(id) = state.resolve(&agent)
                    && state
                        .registry
                        .get(&id)
                        .is_some_and(|r| r.input_binding.is_some())
                {
                    return Response::error(
                        ErrorCode::Forbidden,
                        "this agent's input is bound to a controller; a token is required",
                    );
                }
                match state.input_consumer(&agent, true) {
                    Ok(()) => match state.ack_inbox(&agent, &acknowledge) {
                        Response::Ok => match state.delivery_queue(&agent) {
                            // Existing owned consumers predate InputWaiting.
                            // A gated poll offers no input and retains all rows;
                            // proven acknowledgements above still complete.
                            Response::InputWaiting { .. } => Response::Messages {
                                messages: Vec::new(),
                            },
                            response => response,
                        },
                        error => error,
                    },
                    Err(error) => *error,
                }
            }
            Request::Claim {
                agent,
                resource,
                mode,
                amount,
                ttl_secs,
                note,
                wait_secs,
                automatic,
            } => {
                self.claim(
                    &agent, resource, mode, amount, ttl_secs, note, wait_secs, automatic,
                )
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
                only_automatic,
            } => {
                self.flush_release_watcher(&agent, None).await;
                lock(&self.state).release_all(&agent, summary, summary_source, only_automatic)
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
            Request::Conversations { project, reader } => self.conversations(project, reader).await,
            Request::History {
                conversation,
                before_seq,
                limit,
            } => self.history(conversation, before_seq, limit),
            Request::Delivery { message } => self.delivery(message),
            Request::Thread {
                message,
                after_seq,
                limit,
            } => self.thread(message, after_seq, limit),
            Request::MarkRead {
                conversation,
                through,
                reader,
            } => self.mark_read(conversation, through, reader),
            Request::SearchMessages {
                query,
                project,
                reader,
                before_seq,
                limit,
            } => {
                self.search_messages(query, project, reader, before_seq, limit)
                    .await
            }
            Request::ChannelOpen {
                agent,
                task,
                members,
                name,
                project,
            } => {
                self.channel_open(&agent, task, members, name, project)
                    .await
            }
            Request::ChannelInvite {
                agent,
                channel,
                member,
            } => self.channel_invite(&agent, &channel, &member),
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
            Request::CodeVerdict {
                agent,
                project,
                branch,
                head,
                from,
                verdict,
                findings,
                note,
            } => {
                self.code_verdict(
                    &agent, project, &branch, &head, from, &verdict, findings, note,
                )
                .await
            }
            Request::ReviewGate {
                agent,
                project,
                branch,
                head,
                role,
            } => {
                self.review_gate(agent.as_deref(), project, &branch, &head, role)
                    .await
            }
            Request::JournalPrune {
                project,
                before_seq,
                older_than_secs,
            } => {
                self.journal_prune_request(&project, before_seq, older_than_secs)
                    .await
            }
            Request::CheckpointPrune {
                agent,
                older_than_secs,
            } => self.checkpoint_prune(agent.as_deref(), older_than_secs),
            Request::Vacuum { force } => self.vacuum(force),
            Request::Leases { agent, resource } => self.leases(agent.as_deref(), resource).await,
            Request::WebhookDeliveries { webhook, limit } => {
                self.webhook_deliveries(&webhook, limit)
            }
            Request::Subscribe { .. }
            | Request::Events { .. }
            | Request::ResumeEvents { .. }
            | Request::Logs { .. }
            | Request::Attach { .. }
            | Request::AttachInput { .. }
            | Request::AttachResize { .. } => {
                Response::error(ErrorCode::Internal, "streaming request routed as unary")
            }
        }
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

    /// The terminal of a managed agent, when it was given one.
    pub fn session(&self, agent: &AgentId) -> Option<supervisor::Session> {
        lock(&self.sessions).get(agent).cloned()
    }

    pub fn owner_mode(&self) -> supervisor::OwnerMode {
        self.owner_mode.clone()
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
                    let _ = state.persist("project", |store| {
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
        links: Vec<agentdocker_core::Link>,
    ) -> Response {
        if let Err(reason) = agentdocker_core::link::check(&links) {
            return Response::error(ErrorCode::Invalid, reason);
        }
        if pause::reserved_message_kind(&kind) {
            return Response::error(
                ErrorCode::Forbidden,
                "pause and resume notices require a project lifecycle request",
            );
        }
        // An agent's word to a role goes to that role's room in its project.
        if let Some(role) = to.strip_prefix(agentdocker_core::agent::ROLE_PREFIX)
            && let Some(response) = lock(&self.state).send_to_role(
                &from,
                role,
                kind.clone(),
                payload.clone(),
                reply_to.clone(),
                links.clone(),
            )
        {
            return response;
        }
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
            // A role's room by its channel id is that role: a rule on
            // `send:role:<role>` governs both spellings.
            if let Destination::Channel(channel) = &to
                && let Some(role) = state.room_role(channel)
            {
                let action = format!("send:role:{role}");
                let ruling = state.permits(&sender, &action);
                if !ruling.is_allowed() {
                    return state.refuse(&sender, &action, ruling);
                }
            }
        }
        let mut envelope = Envelope::new(from, to, kind, payload, reply_to, Utc::now());
        envelope.links = links;
        state.publish(envelope)
    }

    /// Turn a sender name and a destination shorthand into what the bus
    /// routes on. `ask` needs the resolved pair as well as the send, so
    /// that it can record who is waiting for the answer.
    async fn endpoints(
        &self,
        from: String,
        to: &str,
    ) -> Result<(String, Destination), Box<Response>> {
        let sender = match lock(&self.state).registry.resolve(&from) {
            Ok(id) => Some(id),
            // An unregistered sender is allowed: `agentd` and a bare
            // `user` both speak without a record of their own.
            Err(RegistryError::NotFound(_)) => None,
            Err(err) => return Err(Box::new(registry_error(err))),
        };
        let from = sender.as_ref().map_or(from, ToString::to_string);
        let to = match Destination::parse(to) {
            Destination::Agent(reference) => Destination::Agent(
                lock(&self.state).resolve_from(sender.as_ref(), reference.as_str())?,
            ),
            Destination::Project(selector) => {
                Destination::Project(self.resolve_project(selector.as_str()).await?)
            }
            other => other,
        };
        let to = lock(&self.state).within_project(sender.as_ref(), to)?;
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
                state.input_consumer(id.as_str(), false)?;
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
}

impl State {
    /// Commit the stop record and event before publishing or signaling a process.
    fn commit_stop(
        &mut self,
        mut agent: AgentRecord,
        force: bool,
    ) -> Result<AgentRecord, Box<Response>> {
        let now = Utc::now();
        agent.status = AgentStatus::Stopping;
        agent.last_seen = now;
        let event = Event::new(
            EventKind::AgentStopping {
                agent: agent.id.clone(),
                force,
            },
            now,
        );
        let committed = self.transition(
            "stop intent",
            event,
            |store, event| store.agent_transition(&agent, event),
            |state| {
                *state
                    .registry
                    .get_mut(&agent.id)
                    .expect("stop identity retained") = agent.clone();
            },
        );
        if committed != Persisted::Committed {
            return Err(Box::new(self.write_failure().unwrap_or_else(|| {
                Response::error(
                    ErrorCode::StorageUnavailable,
                    "stop intent was not committed",
                )
            })));
        }
        Ok(agent)
    }

    /// Execute a store operation as part of the current ordered transition.
    /// A store write, answered `None` while fenced (and noted as skipped)
    /// or once storage has failed; callers leave memory as it was then.
    fn store_op<T>(
        &mut self,
        what: &str,
        op: impl FnOnce(&Store) -> anyhow::Result<T>,
    ) -> Option<T> {
        if self.storage_error.is_some() {
            return None;
        }
        if self.fenced() {
            debug!(%what, "store operation skipped: coordination is being transferred");
            self.skipped_write = true;
            return None;
        }
        self.store_read(what, op)
    }

    /// A store read: served through a fence, since a transfer stops
    /// writes and nothing else, and refused only once storage has failed.
    fn store_read<T>(
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
    /// Whether writes are fenced off because a transfer is in flight.
    fn fenced(&self) -> bool {
        !matches!(self.coordination, Coordination::Serving)
    }

    /// The answer a mutating request gets while fenced.
    fn transferring(&self) -> Option<Response> {
        self.fenced().then(|| {
            Response::error(
                ErrorCode::Transferring,
                "the daemon is handing coordination to its successor; nothing was applied, retry against the daemon that answers next",
            )
        })
    }

    /// Stop writing and record the offer durably: the last write this
    /// process makes until the offer settles. Refused while a storage
    /// failure is latched (nothing here is trustworthy to hand over) or
    /// while another offer is open.
    fn offer_transfer(&mut self, successor_pid: u32) -> Result<Transfer, Box<Response>> {
        if let Some(error) = self.write_failure() {
            return Err(Box::new(error));
        }
        if self.fenced() {
            return Err(Box::new(Response::error(
                ErrorCode::Conflict,
                "a coordination transfer is already in flight",
            )));
        }
        if self.in_flight > 0 {
            // A mutation admitted before this offer is still executing and
            // may still write; the offer's last write must follow it.
            return Err(Box::new(Response::error(
                ErrorCode::Backpressure,
                format!(
                    "{} mutating request(s) still executing; offer again once they finish",
                    self.in_flight
                ),
            )));
        }
        let transfer = Transfer {
            id: AgentId::generate().to_string(),
            predecessor_pid: std::process::id(),
            successor_pid: Some(successor_pid),
            state: TransferState::Offered,
            offered_at: Utc::now(),
            settled_at: None,
        };
        let mut event = Event::new(
            EventKind::DaemonTransferOffered {
                transfer: transfer.id.clone(),
                successor_pid,
            },
            transfer.offered_at,
        );
        event.seq = self.next_seq;
        let offered = self.store_op("transfer offer", |store| {
            store.offer_transfer(&transfer, &event)
        });
        match offered {
            Some(true) => {
                self.next_seq += 1;
                let _ = self.events.send(event);
                self.coordination = Coordination::Quiescing {
                    transfer: transfer.id.clone(),
                };
                info!(transfer = %transfer.id, successor_pid, "coordination offered; writes fenced");
                Ok(transfer)
            }
            Some(false) => Err(Box::new(Response::error(
                ErrorCode::Conflict,
                "another coordination transfer is still offered in the database",
            ))),
            None => Err(Box::new(self.write_failure().unwrap_or_else(|| {
                Response::error(ErrorCode::Internal, "cannot record the offer")
            }))),
        }
    }

    /// Name the successor once it exists. The offer was made under this
    /// process's own pid as a placeholder; the row is rewritten with the
    /// successor's, still `Offered`, so only that process can accept.
    #[cfg_attr(windows, allow(dead_code))] // only a Unix handover names a successor
    fn readdress_offer(&mut self, transfer: &str, successor_pid: u32) -> bool {
        let Coordination::Quiescing { transfer: current } = &self.coordination else {
            return false;
        };
        if current != transfer {
            return false;
        }
        let mut event = Event::new(
            EventKind::DaemonTransferReaddressed {
                transfer: transfer.to_owned(),
                successor_pid,
            },
            Utc::now(),
        );
        event.seq = self.next_seq;
        match self
            .store
            .readdress_transfer(transfer, successor_pid, &event)
        {
            Ok(true) => {
                self.next_seq += 1;
                let _ = self.events.send(event);
                self.owner_successor = agentdocker_host::procinfo::start_time(successor_pid)
                    .map(|born| (successor_pid, born));
                true
            }
            Ok(false) => false,
            Err(err) => {
                error!(%err, "cannot readdress the transfer offer");
                false
            }
        }
    }

    /// Take authority back: abort the offer if it is still ours to abort.
    /// Returns whether writing resumed. If the successor accepted first,
    /// this process must leave: it says so and stays fenced.
    fn abort_transfer(&mut self, reason: &str) -> bool {
        // The successor reaches no owner from here on, whether or not the
        // abort below is recorded.
        self.owner_successor = None;
        let Coordination::Quiescing { transfer } = self.coordination.clone() else {
            return !self.fenced();
        };
        // Written past the fence on purpose: settling is the one write a
        // fenced coordinator may make, and only as a compare-and-set.
        let now = Utc::now();
        let mut event = Event::new(
            EventKind::DaemonTransferAborted {
                transfer: transfer.clone(),
                reason: reason.to_owned(),
            },
            now,
        );
        event.seq = self.next_seq;
        let settled =
            self.store
                .settle_transfer(&transfer, None, TransferState::Aborted, now, &event);
        match settled {
            Ok(true) => {
                self.coordination = Coordination::Serving;
                self.skipped_write = false;
                self.next_seq += 1;
                let _ = self.events.send(event);
                warn!(%transfer, reason, "coordination transfer aborted; writing resumed");
                true
            }
            Ok(false) => {
                // Not ours to abort any more: the successor accepted.
                self.coordination = Coordination::Transferred { transfer };
                false
            }
            Err(err) => {
                error!(%err, "cannot settle the transfer; staying fenced");
                false
            }
        }
    }

    /// What the store says about the offer, read past the fence: the
    /// predecessor's way to learn that a silent successor did accept.
    fn transfer_state(&self) -> Option<Transfer> {
        self.store.transfer().ok().flatten()
    }

    /// Storage has failed for good: nothing may be served from this
    /// projection, reads included, until a restart reloads durable state.
    fn storage_failure(&self) -> Option<Response> {
        self.storage_error.as_ref().map(|error| {
            Response::error(
                ErrorCode::StorageUnavailable,
                format!("storage failed ({error}); coordination disabled until daemon restart"),
            )
        })
    }

    /// Did the last write land? A storage failure, or a write the fence
    /// skipped: whatever the caller was about to apply in memory must not
    /// be, and the client must retry later. A skip is not the reads'
    /// concern: they keep being served from the projection the skip left
    /// as it was.
    fn write_failure(&self) -> Option<Response> {
        if let Some(error) = self.storage_failure() {
            return Some(error);
        }
        if self.skipped_write {
            return self.transferring();
        }
        None
    }

    /// Write, and say what happened. Callers move memory and publish only
    /// on `Committed`; a `Skipped` write (fenced) is not a failure and not
    /// a success, and the caller must leave memory as it was.
    fn persist(
        &mut self,
        what: &str,
        write: impl FnOnce(&Store) -> anyhow::Result<()>,
    ) -> Persisted {
        if self.storage_error.is_some() {
            return Persisted::Failed;
        }
        if self.fenced() {
            // Not an error: the caller's request was refused before it got
            // here, and a tick writer simply skips its turn. Nothing lands,
            // and `write_failure()` says so until a write does.
            debug!(%what, "write skipped: coordination is being transferred");
            self.skipped_write = true;
            return Persisted::Skipped;
        }
        if let Err(err) = write(&self.store) {
            error!(%what, %err, "storage failed; disabling coordination until restart");
            self.storage_error = Some(format!("{what}: {err}"));
            return Persisted::Failed;
        }
        self.skipped_write = false;
        Persisted::Committed
    }

    /// One durable transition, the way every state change is made: the
    /// events take the next sequence numbers, `write` commits them with
    /// the data they describe in one store transaction, and only on commit
    /// does `apply` change memory and the events go out, in that order. A
    /// write the fence skipped or the store refused leaves memory as it
    /// was, and `write_failure()` says so. `write` may hand `apply` what
    /// it computed, so nothing is computed twice.
    fn commit<T>(
        &mut self,
        what: &str,
        mut events: Vec<Event>,
        write: impl FnOnce(&Store, &[Event]) -> anyhow::Result<T>,
        apply: impl FnOnce(&mut Self, &[Event], T),
    ) -> Persisted {
        for (index, event) in events.iter_mut().enumerate() {
            event.seq = self.next_seq + index as u64;
        }
        let mut written = None;
        let committed = self.persist(what, |store| {
            written = Some(write(store, &events)?);
            Ok(())
        });
        if committed == Persisted::Committed
            && let Some(written) = written
        {
            self.next_seq += events.len() as u64;
            apply(self, &events, written);
            for event in events {
                let _ = self.events.send(event);
            }
        }
        committed
    }

    /// [`State::commit`] for the common transition: one event, a write
    /// that returns nothing, and the memory change that follows it.
    fn transition(
        &mut self,
        what: &str,
        event: Event,
        write: impl FnOnce(&Store, &Event) -> anyhow::Result<()>,
        apply: impl FnOnce(&mut Self),
    ) -> Persisted {
        self.commit(
            what,
            vec![event],
            |store, events| write(store, &events[0]),
            |state, _, ()| apply(state),
        )
    }

    /// [`State::commit`] for a store that composes the events itself from
    /// the first sequence number it is given, when only it knows what
    /// changed: how many archive rows a prune removed, which samples a scan
    /// accounted. The events it returns are published on commit, in order,
    /// and the sequence advances by their count.
    fn commit_produced<T>(
        &mut self,
        what: &str,
        write: impl FnOnce(&Store, u64) -> anyhow::Result<(Vec<Event>, T)>,
        apply: impl FnOnce(&mut Self, &[Event], T),
    ) -> Persisted {
        let first = self.next_seq;
        let mut produced = None;
        let committed = self.persist(what, |store| {
            produced = Some(write(store, first)?);
            Ok(())
        });
        if committed == Persisted::Committed
            && let Some((events, written)) = produced
        {
            self.next_seq += events.len() as u64;
            apply(self, &events, written);
            for event in events {
                let _ = self.events.send(event);
            }
        }
        committed
    }

    pub fn emit(&mut self, kind: EventKind) {
        let _ = self.transition(
            "event",
            Event::new(kind, Utc::now()),
            |store, event| store.append_event(event),
            |_| {},
        );
    }

    pub fn resolve(&mut self, reference: &str) -> Result<AgentId, Box<Response>> {
        if let Some(error) = self.storage_failure() {
            return Err(Box::new(error));
        }
        self.registry
            .resolve(reference)
            .map_err(|err| Box::new(registry_error(err)))
    }

    /// Resolve a reference as `from` would: `role:<name>` is the one live
    /// holder in the sender's project when the sender works in one, so
    /// "the reviewer" means this project's reviewer; anything else, and
    /// a sender without a project, resolves as [`State::resolve`] does.
    pub fn resolve_from(
        &mut self,
        from: Option<&AgentId>,
        reference: &str,
    ) -> Result<AgentId, Box<Response>> {
        let project = from
            .and_then(|from| self.registry.get(from))
            .and_then(|record| record.project.as_ref())
            .map(agentdocker_core::ProjectRef::id);
        match (
            reference.strip_prefix(agentdocker_core::agent::ROLE_PREFIX),
            project,
        ) {
            (Some(role), Some(project)) => {
                if let Some(error) = self.storage_failure() {
                    return Err(Box::new(error));
                }
                self.registry
                    .resolve_role(role, Some(&project))
                    .map_err(|err| Box::new(registry_error(err)))
            }
            _ => self.resolve(reference),
        }
    }

    /// Keep an agent's word inside the project it works in. The person,
    /// and a sender with no record such as `agentd`, may address anyone;
    /// an agent in a project reaches that project's agents, its channels
    /// and the person, and its `all` is said to its own project. Without
    /// this, `all` and a name found in another repository carried one
    /// project's messages into every other project's terminals.
    pub fn within_project(
        &self,
        sender: Option<&AgentId>,
        to: Destination,
    ) -> Result<Destination, Box<Response>> {
        let Some(record) = sender.and_then(|id| self.registry.get(id)) else {
            return Ok(to);
        };
        if humans::is_human(record) {
            return Ok(to);
        }
        let Some(own) = record.project.as_ref().map(ProjectRef::id) else {
            return Ok(to);
        };
        let refuse = |what: String| {
            Box::new(Response::error(
                ErrorCode::Forbidden,
                format!(
                    "{what}: an agent reaches only its own project and the person; \
                     ask the person to pass it on"
                ),
            ))
        };
        match to {
            Destination::Broadcast => Ok(Destination::Project(own)),
            Destination::Project(project) if project != own => {
                Err(refuse("that is another project".to_owned()))
            }
            // An agent in no project is in nobody's way, and reachable.
            Destination::Agent(id) => match self.registry.get(&id) {
                Some(target) if !humans::is_human(target) => match &target.project {
                    Some(project) if project.id() != own => Err(refuse(format!(
                        "{} works in project `{}`",
                        target.spec.name,
                        project.name()
                    ))),
                    _ => Ok(Destination::Agent(id)),
                },
                _ => Ok(Destination::Agent(id)),
            },
            Destination::Channel(id)
                if self
                    .channels
                    .get(&id)
                    .is_some_and(|channel| channel.project != own) =>
            {
                Err(refuse(format!("channel {id} belongs to another project")))
            }
            other => Ok(other),
        }
    }

    /// Give `reference` one role in place of whatever it held, or take its
    /// roles away. A role is one word
    /// ([`agentdocker_core::agent::check_role`]), kept in the `role` label
    /// on a live record and said as `role_set`; the same role again is
    /// nothing. Several live agents may hold one role; `role_assign` gives
    /// several to one agent.
    pub(super) fn set_role(
        &mut self,
        reference: &str,
        role: Option<String>,
        now: DateTime<Utc>,
    ) -> Response {
        if let Some(role) = &role
            && let Err(reason) = agentdocker_core::agent::check_role(role)
        {
            return Response::error(ErrorCode::Invalid, reason);
        }
        let id = match self.resolve(reference) {
            Ok(id) => id,
            Err(response) => return *response,
        };
        if !self
            .registry
            .get(&id)
            .expect("resolved agent")
            .status
            .is_live()
        {
            return Response::error(ErrorCode::Invalid, "a finished agent has no role");
        }
        // One role in place of whatever it held, or none.
        match self.write_roles(&id, role, now) {
            Ok(record) => Response::Agent { agent: record },
            Err(response) => *response,
        }
    }

    /// Give a live agent the name a person chose
    /// ([`agentdocker_core::agent::check_name`]): unique among live agents,
    /// said as `agent_renamed`, the generated-name label dropped so the name
    /// reads as chosen. The same name again is nothing; the id, and every
    /// reference by id, is untouched.
    pub(super) fn rename(&mut self, reference: &str, name: String, now: DateTime<Utc>) -> Response {
        if let Err(reason) = agentdocker_core::agent::check_name(&name) {
            return Response::error(ErrorCode::Invalid, reason);
        }
        let id = match self.resolve(reference) {
            Ok(id) => id,
            Err(response) => return *response,
        };
        let mut record = self.registry.get(&id).expect("resolved agent").clone();
        if !record.status.is_live() {
            return Response::error(ErrorCode::Invalid, "a finished agent keeps its name");
        }
        if record.spec.name == name && !record.name_is_generated() {
            return Response::Agent { agent: record };
        }
        if self
            .registry
            .live()
            .any(|other| other.id != id && other.spec.name == name)
        {
            return Response::error(
                ErrorCode::NameTaken,
                format!("an agent named `{name}` is already live; pick another name"),
            );
        }
        record.spec.name = name.clone();
        // Marked chosen, not merely unlabelled: a name spelled like the
        // adapter's own (`codex-<pid>`) must not be inferred generated again.
        record.spec.labels.insert(
            agentdocker_core::agent::NAME_LABEL.to_owned(),
            agentdocker_core::agent::CHOSEN_NAME.to_owned(),
        );
        let event = agentdocker_core::Event::new(
            EventKind::AgentRenamed {
                agent: id.clone(),
                project: record
                    .project
                    .as_ref()
                    .map(agentdocker_core::ProjectRef::id),
                name,
            },
            now,
        );
        let _ = self.transition(
            "rename",
            event,
            |store, event| store.agent_transition(&record, event),
            |state| {
                *state.registry.get_mut(&id).expect("resolved agent") = record.clone();
            },
        );
        self.write_failure()
            .unwrap_or(Response::Agent { agent: record })
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
        // Disk first, memory on commit: a write skipped by a fence or
        // failed leaves the old checkout in memory, so the change is
        // noticed again rather than lost.
        let Some(current) = self.registry.get(id).cloned() else {
            return;
        };
        let changed = !current.vcs.as_ref().is_some_and(|old| old.same_as(&vcs));
        if !changed {
            let _ = self.registry.set_vcs(id, vcs);
            return;
        }
        let mut record = current;
        record.vcs = Some(vcs.clone());
        info!(agent = %id.short(), checkout = %vcs.describe(), "checkout moved");
        if self.persist("agent", |store| store.upsert_agent(&record)) == Persisted::Committed {
            let _ = self.registry.set_vcs(id, vcs.clone());
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

    /// The agents retention may forget, oldest first: ended before
    /// `cutoff`, not the person, and holding or owed nothing: no lease,
    /// no open question, no queued message, no input binding, no restart
    /// or restore, no provider limit.
    fn retirable_agents(&self, cutoff: DateTime<Utc>, limit: usize) -> Vec<AgentId> {
        let mut retirable: Vec<&AgentRecord> = self
            .registry
            .all()
            .filter(|record| {
                !record.status.is_live()
                    && record.id.as_str() != agentdocker_core::HUMAN
                    && record.spec.runtime != agentdocker_core::HUMAN_RUNTIME
                    && record.finished_at.unwrap_or(record.last_seen) < cutoff
                    && record.last_seen < cutoff
                    && record.input_binding.is_none()
                    && !record.spec.restore
                    && record.spec.restart.is_no()
                    && record
                        .provider_availability
                        .as_ref()
                        .is_none_or(|availability| availability.issue.is_none())
                    && self.leases.by_holder(&record.id).is_empty()
                    && self.inboxes.get(&record.id).is_none_or(VecDeque::is_empty)
                    && !self.pending_restarts.contains(&record.id)
                    && !self.supervised.contains_key(&record.id)
                    && !self.questions.values().any(|question| {
                        question.from == record.id.as_str()
                            || matches!(&question.to, Destination::Agent(to) if to == &record.id)
                    })
            })
            .collect();
        retirable.sort_by_key(|record| record.finished_at.unwrap_or(record.last_seen));
        retirable
            .into_iter()
            .take(limit)
            .map(|record| record.id.clone())
            .collect()
    }

    fn remove(&mut self, reference: &str) -> Response {
        let id = match self.resolve(reference) {
            Ok(id) => id,
            Err(response) => return *response,
        };
        if self.is_live(&id) {
            // `stop` signals the process. For one AgentDocker did not
            // start, that is not the person's intent when forgetting
            // its record; `deregister` marks it finished untouched.
            let managed = self.registry.get(&id).is_some_and(|a| a.managed);
            return Response::error(
                ErrorCode::Invalid,
                if managed {
                    "agent is still live; stop it first".to_owned()
                } else {
                    format!(
                        "agent is still live and AgentDocker did not start it; `deregister --as {reference}` marks it finished without signalling the process, then `rm` forgets it"
                    )
                },
            );
        }
        if self.registry.get(&id).is_some_and(|a| {
            a.provider_availability
                .as_ref()
                .is_some_and(|p| p.issue.is_some())
        }) {
            return Response::error(
                ErrorCode::Conflict,
                "resolve this agent's provider limit before removing its record",
            );
        }
        let _ = self.transition(
            "agent removal",
            Event::new(EventKind::AgentRemoved { agent: id.clone() }, Utc::now()),
            |store, event| store.delete_agent(&id, event),
            |state| {
                state.registry.remove(&id);
                state.inboxes.remove(&id);
                state.inbox_bytes.remove(&id);
                state
                    .journal_cursors
                    .retain(|(reader, _), _| reader != id.as_str());
            },
        );
        self.write_failure().unwrap_or(Response::Ok)
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

    fn mark_exited_durably(&mut self, id: &AgentId, status: AgentStatus) -> bool {
        let was_available = self.storage_error.is_none();
        let recorded = self.mark_exited(id, status);
        was_available
            && self.storage_error.is_none()
            && recorded.is_some_and(|record| !record.status.is_live())
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
            .map(|kind| Event::new(kind, now))
            .collect();
        let leases: Vec<_> = released.iter().map(|lease| lease.id.clone()).collect();
        let committed = self.commit(
            "agent exit",
            events,
            |store, events| store.agent_exit(&record, &leases, &journal, &channels, events),
            |state, _, ()| {
                *state.registry.get_mut(id).expect("exit identity retained") = record.clone();
                state.leases.release_all(id);
                for entry in &journal {
                    state.cache_journal(entry.clone());
                }
                for channel in &channels {
                    state.channels.insert(channel.id.clone(), channel.clone());
                }
            },
        );
        if committed != Persisted::Committed {
            self.journal_seq = previous_seq;
            return self.registry.get(id).cloned();
        }
        info!(agent = %id.short(), name = %record.spec.name, %status, "agent finished");
        Some(record)
    }

    /// Seen, as one that read its own queue: the contact of a session with
    /// no adapter, bound to its current process.
    fn touch_reader(&mut self, id: &AgentId) {
        if let Some(mut record) = self.registry.get(id).cloned() {
            let now = Utc::now();
            record.last_seen = now;
            if let Some(born) = record.process_started_at {
                record.queue_read = Some(agentdocker_core::AdapterContact {
                    process_started_at: born,
                    observed_at: now,
                });
            }
            let committed = self.persist("agent", |store| store.upsert_agent(&record));
            if committed == Persisted::Committed {
                *self
                    .registry
                    .get_mut(id)
                    .expect("liveness identity retained") = record;
            }
        }
    }

    /// The record a session key names, its contact noted, or `None` when
    /// no record names it. Every record is looked at, ended ones included,
    /// and the bindings too, all under this one lock: a binding that names
    /// the conversation owns its delivery and refuses; a record of it that
    /// does not say its profile, or says another, refuses (absence is no
    /// wildcard); two live records for the key refuse. A live record with a
    /// process is answered as it is. An ended record known by its key alone
    /// is the conversation's still, its queue with it: it is live again
    /// under its own id.
    fn keyed_session(
        &mut self,
        runtime: &str,
        profile: &str,
        session: &str,
        adapter: agentdocker_core::AdapterKind,
        now: DateTime<Utc>,
    ) -> Option<Response> {
        if self.registry.all().any(|a| {
            a.input_binding
                .as_ref()
                .is_some_and(|binding| binding.provider.session == session)
        }) {
            return Some(Response::error(
                ErrorCode::Conflict,
                "an input binding owns this conversation; recover it from its session",
            ));
        }
        let named: Vec<AgentRecord> = self
            .registry
            .all()
            .filter(|a| {
                a.spec.runtime == runtime
                    && a.spec.labels.get("session_id").map(String::as_str) == Some(session)
            })
            .cloned()
            .collect();
        if named.iter().any(|a| {
            a.spec
                .labels
                .get(agentdocker_core::agent::PROFILE_LABEL)
                .map(String::as_str)
                != Some(profile)
        }) {
            return Some(Response::error(
                ErrorCode::Conflict,
                "this conversation's record does not say it belongs to this provider profile",
            ));
        }
        if named.is_empty() {
            // Nobody owns it: the caller makes its record.
            return None;
        }
        let live: Vec<&AgentRecord> = named.iter().filter(|a| a.status.is_live()).collect();
        let mut record = match live.as_slice() {
            [only] => (*only).clone(),
            [] => {
                // Ended: the latest record of the conversation is its owner
                // still, back under its own id, whether or not a process
                // once ran it. Ended never means nobody owns it.
                let mut ended: Vec<&AgentRecord> = named.iter().filter(|a| !a.managed).collect();
                ended.sort_by_key(|a| a.last_seen);
                // Records name the conversation but none can come back by
                // contact (AgentDocker supervised it): it has an owner all
                // the same, to relaunch, never a second record beside it.
                let Some(latest) = ended.last() else {
                    return Some(Response::error(
                        ErrorCode::Conflict,
                        "this conversation's record ended under AgentDocker's supervision; relaunch it from the app",
                    ));
                };
                let mut record = (*latest).clone();
                if record.pid.is_some_and(|pid| {
                    process_exists(pid) && same_process(pid, record.process_started_at)
                }) {
                    return Some(Response::error(
                        ErrorCode::Conflict,
                        "the conversation's ended record still has its process; let it finish first",
                    ));
                }
                // Its process is gone: kept by contact from here on.
                record.pid = None;
                record.process_started_at = None;
                record.process_group = None;
                record.session = None;
                record.terminal = None;
                self.free_name(&mut record);
                record.status = AgentStatus::Running;
                record.finished_at = None;
                record.started_at = Some(now);
                record.last_seen = now;
                record.session_contact = Some(agentdocker_core::SessionContact {
                    adapter,
                    observed_at: now,
                });
                if let Some(error) = self.write_failure() {
                    return Some(error);
                }
                if self.persist("agent", |store| store.upsert_agent(&record))
                    != Persisted::Committed
                {
                    return Some(self.write_failure().expect("a refused write has a reason"));
                }
                *self
                    .registry
                    .get_mut(&record.id)
                    .expect("keyed session retained") = record.clone();
                self.emit(EventKind::AgentStarted {
                    agent: record.id.clone(),
                    pid: None,
                });
                return Some(Response::Agent { agent: record });
            }
            _ => {
                return Some(Response::error(
                    ErrorCode::Conflict,
                    "more than one record names this conversation",
                ));
            }
        };
        if !record.keyed_only() {
            return Some(Response::Agent { agent: record });
        }
        // A contact a moment ago is the same presence; the store is not
        // written for every tool call.
        let fresh = record.session_contact.as_ref().is_some_and(|c| {
            now.signed_duration_since(c.observed_at) < chrono::Duration::seconds(30)
        });
        if !fresh {
            record.last_seen = now;
            record.session_contact = Some(agentdocker_core::SessionContact {
                adapter,
                observed_at: now,
            });
            if let Some(error) = self.write_failure() {
                return Some(error);
            }
            if self.persist("agent", |store| store.upsert_agent(&record)) == Persisted::Committed {
                *self
                    .registry
                    .get_mut(&record.id)
                    .expect("keyed session retained") = record.clone();
            }
        }
        Some(Response::Agent { agent: record })
    }

    /// A Codex process proven to run a conversation known by its key alone
    /// joins that record: the same id, so its queue, leases, questions and
    /// roles stay with it, never a second record to fold later. The key
    /// (runtime, profile, thread) must match and the process must run in
    /// the conversation's own checkout. `None` when no such record exists.
    fn prove_keyed(
        &mut self,
        spec: &AgentSpec,
        pid: u32,
        born: Option<DateTime<Utc>>,
        place: (
            Option<agentdocker_core::multiplexer::Session>,
            Option<agentdocker_core::terminal::Host>,
        ),
        now: DateTime<Utc>,
    ) -> Option<Response> {
        let session = spec.labels.get("session_id").filter(|s| !s.is_empty())?;
        let profile = spec.labels.get(agentdocker_core::agent::PROFILE_LABEL)?;
        let mut keyed: Vec<AgentRecord> = self
            .registry
            .all()
            .filter(|a| {
                a.spec.runtime == spec.runtime
                    && a.pid.is_none()
                    && !a.managed
                    && a.session_contact.is_some()
                    && a.spec.labels.get("session_id") == Some(session)
                    && a.spec.labels.get(agentdocker_core::agent::PROFILE_LABEL) == Some(profile)
            })
            .cloned()
            .collect();
        keyed.sort_by_key(|a| (a.status.is_live(), a.last_seen));
        let mut record = keyed.pop()?;
        if record.spec.workdir != spec.workdir {
            return Some(Response::error(
                ErrorCode::Conflict,
                "the process runs in another checkout than its conversation",
            ));
        }
        let Some(born) = born else {
            return Some(Response::error(
                ErrorCode::Invalid,
                "the process's birth cannot be read to prove it",
            ));
        };
        // One process, one record: a terminal that already has its own
        // (adopted before this proof, say) is not given a second one.
        if self
            .registry
            .live()
            .any(|a| a.id != record.id && a.pid == Some(pid) && a.process_started_at == Some(born))
        {
            return Some(Response::error(
                ErrorCode::Conflict,
                "this terminal already has a record of its own; forget one of the two to join it to its conversation",
            ));
        }
        if let Some(error) = self.write_failure() {
            return Some(error);
        }
        if !record.status.is_live() {
            self.free_name(&mut record);
        }
        record.pid = Some(pid);
        record.process_started_at = Some(born);
        record.status = AgentStatus::Running;
        record.finished_at = None;
        record.started_at = Some(now);
        record.last_seen = now;
        // Its process says whether it lives from now on.
        record.session_contact = None;
        for (key, value) in &spec.labels {
            if key != "adopted" {
                record
                    .spec
                    .labels
                    .entry(key.clone())
                    .or_insert_with(|| value.clone());
            }
        }
        (record.session, record.terminal) = place;
        if self.persist("agent", |store| store.upsert_agent(&record)) != Persisted::Committed {
            return Some(self.write_failure().expect("a refused write has a reason"));
        }
        *self
            .registry
            .get_mut(&record.id)
            .expect("keyed session retained") = record.clone();
        self.emit(EventKind::AgentStarted {
            agent: record.id.clone(),
            pid: Some(pid),
        });
        Some(Response::Agent { agent: record })
    }

    /// Give `record` a name no other live agent holds: its own, or one made
    /// from its id when another took it while it was ended.
    fn free_name(&self, record: &mut AgentRecord) {
        let id = record.id.clone();
        let taken = |name: &str| {
            self.registry
                .live()
                .any(|a| a.id != id && a.spec.name == name)
        };
        if !taken(&record.spec.name) {
            return;
        }
        let base = default_name(&id);
        let mut name = base.clone();
        let mut n = 2;
        while taken(&name) {
            name = format!("{base}-{n}");
            n += 1;
        }
        record.spec.name = name;
        record.spec.labels.insert(
            agentdocker_core::agent::NAME_LABEL.to_owned(),
            agentdocker_core::agent::GENERATED_NAME.to_owned(),
        );
    }

    fn touch(&mut self, id: &AgentId) {
        if let Some(mut record) = self.registry.get(id).cloned() {
            record.last_seen = Utc::now();
            let committed = self.persist("agent", |store| store.upsert_agent(&record));
            if committed == Persisted::Committed {
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
        let events: Vec<Event> = kind.map(|kind| Event::new(kind, now)).into_iter().collect();
        let committed = self.commit(
            "lease activity",
            events,
            |store, events| store.lease_activity(&record, lease, events.first()),
            |state, _, ()| {
                *state
                    .registry
                    .get_mut(holder)
                    .expect("claim identity retained") = record.clone();
                if let Some(lease) = lease {
                    state.leases.restore(lease.clone());
                }
            },
        );
        committed == Persisted::Committed
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

    fn next_journal_seq(&mut self, project: &ProjectId) -> u64 {
        if let Some(next) = self.journal_seq.get_mut(project) {
            let seq = *next;
            *next += 1;
            return seq;
        }
        let stored = self
            .store_read("journal", |store| store.max_journal_seq(project))
            .unwrap_or(0);
        self.journal_seq.insert(project.clone(), stored + 2);
        stored + 1
    }

    /// Assign a seq, persist (own transaction), ring, announce.
    fn append_journal(&mut self, mut entry: JournalEntry) -> JournalEntry {
        entry.seq = self.next_journal_seq(&entry.project);
        let event = Event::new(
            EventKind::JournalAppended {
                entry: entry.clone(),
            },
            Utc::now(),
        );
        let _ = self.transition(
            "journal",
            event,
            |store, event| store.append_journal_with_event(&entry, event),
            |state| state.cache_journal(entry.clone()),
        );
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
    /// checkout, else the holder of its `branch:` lease, else the live
    /// agent the daemon created the worktree for, else the holder of an
    /// exclusive `path:` lease over the checkout, else nobody.
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
            _ => new
                .branch
                .as_ref()
                .and_then(|branch| {
                    let key = ResourceKey::new(format!("branch:{branch}"));
                    let holder = self
                        .leases
                        .holders_of(&key)
                        .first()
                        .map(|l| l.holder.clone())?;
                    self.registry.get(&holder).cloned()
                })
                .or_else(|| {
                    // The agent that asked for this worktree, while it lives.
                    let creator = self.worktree_creators.get(&checkout)?;
                    self.registry
                        .get(creator)
                        .filter(|a| a.status.is_live())
                        .cloned()
                })
                .or_else(|| match self.attribute(&checkout) {
                    // Holding the checkout is holding what is committed in it.
                    Attribution::Agent { agent, .. } => self.registry.get(&agent).cloned(),
                    Attribution::External => None,
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
        let Some(head_seq) = self.store_read("journal head", |store| {
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
        match self.store_read("journal", |store| store.journal(&query)) {
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
                self.store_read("journal", |store| store.journal(&query))
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
            .store_read("journal", |store| store.journal_cursor(key, project))
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
        let event = Event::new(
            EventKind::JournalRead {
                reader: key.to_owned(),
                project: project.clone(),
                seq,
            },
            Utc::now(),
        );
        let _ = self.transition(
            "journal cursor",
            event,
            |store, event| store.set_journal_cursor_with_event(key, project, seq, event),
            |state| {
                state
                    .journal_cursors
                    .insert((key.to_owned(), project.clone()), seq);
            },
        );
    }

    /// The project's ring, loaded from the store on first use.
    fn ring(&mut self, project: &ProjectId) -> &mut JournalRing {
        if !self.journal_rings.contains_key(project) {
            let newest = self
                .store_read("journal", |store| {
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

    fn journal_prune(&mut self, project: &ProjectId, before_seq: u64, reason: &str) -> Response {
        match self.store_op("journal", |store| store.prune_journal(project, before_seq)) {
            Some(removed) => {
                if let Some(ring) = self.journal_rings.get_mut(project) {
                    ring.entries.retain(|e| e.seq >= before_seq);
                }
                if removed > 0 {
                    info!(project = %project.short(), removed, reason, "pruned the journal");
                    self.emit(EventKind::JournalPruned {
                        project: project.clone(),
                        before_seq,
                        removed,
                        reason: reason.to_owned(),
                    });
                }
                Response::Pruned { removed }
            }
            None => self
                .write_failure()
                .unwrap_or_else(|| Response::error(ErrorCode::Internal, "journal prune failed")),
        }
    }

    pub fn prune_events(&mut self) {
        if let Some(removed) =
            self.store_op("event retention", |store| store.prune_events(EVENT_HISTORY))
            && removed > 0
        {
            info!(removed, "pruned event history");
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
        self.publish_with_channel(envelope, question, None, None)
    }

    /// Prepare channel state and ancillary effects without changing live state;
    /// publish the complete transition only after the message transaction commits.
    /// `document` is one more thing that lands with the message or not at
    /// all — a project's pause, whose word to the agents and whose record
    /// must not exist without each other — with the event that announces it.
    fn publish_with_channel(
        &mut self,
        envelope: Envelope,
        question: Option<agentdocker_core::Question>,
        channel: Option<(Channel, Vec<EventKind>, Option<JournalEntry>)>,
        document: Option<DocumentTransition>,
    ) -> Response {
        let (channel, transition, mut journal) = match channel {
            Some((channel, events, journal)) => (Some(channel), events, journal),
            None => (None, Vec::new(), None),
        };
        if let Some(error) = self.write_failure() {
            return error;
        }
        self.expire_questions(Utc::now());
        if let Some(error) = self.write_failure() {
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
            Destination::Channel(id) => channel
                .as_ref()
                .filter(|staged| staged.id == *id)
                .map(|staged| staged.members.clone())
                .unwrap_or_else(|| self.channel_members(id))
                .into_iter()
                .filter(|id| id.as_str() != envelope.from)
                .collect(),
            Destination::Topic(_) => Vec::new(),
        };
        // New channel membership is unique already; legacy records may not be.
        // A recipient must consume one slot and one durable row per message.
        recipients.sort();
        recipients.dedup();
        // A reply that closes a question is its answer whichever request
        // carried it: `send --reply-to` and `answer` are one act, so the
        // kind says so before it is sized, stored or routed, and the route
        // is chosen here for both. With an `ask` waiting the answer is held
        // for it; otherwise the queue is its route, said at once.
        let closed = envelope.reply_to.as_ref().and_then(|id| self.questions.get(id))
            .filter(|pending| matches!(&envelope.to, Destination::Agent(id) if id.as_str() == pending.from))
            .filter(|pending| self.put_to(pending, &AgentId::from(envelope.from.as_str())))
            .map(|pending| pending.id.clone());
        if let Some(
            presentation @ (agentdocker_core::QuestionPresentation::McpUrl { .. }
            | agentdocker_core::QuestionPresentation::McpForm { .. }),
        ) = closed
            .as_ref()
            .and_then(|id| self.questions.get(id))
            .and_then(|q| q.presentation.as_ref())
            && !envelope.payload.as_object().is_some_and(|payload| {
                payload.len() == 1
                    && payload
                        .get("text")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|value| presentation.permits_answer(value))
            })
        {
            return Response::error(
                ErrorCode::Invalid,
                "answer does not match the reviewed MCP request; use the displayed choices or valid form values",
            );
        }
        let mut envelope = envelope;
        if closed.is_some() {
            envelope.kind = "answer".to_owned();
        }
        let held = closed
            .as_ref()
            .is_some_and(|question| self.question_waiters.contains(question));
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
        let sender = self
            .registry
            .get(&AgentId::from(envelope.from.as_str()))
            .cloned()
            .map(|mut record| {
                record.last_seen = Utc::now();
                record
            });
        if let Some(entry) = &mut journal {
            entry.seq = match self.journal_seq.get(&entry.project) {
                Some(seq) => *seq,
                None => match self
                    .store_read("journal", |store| store.max_journal_seq(&entry.project))
                {
                    Some(seq) => seq + 1,
                    None => return self.write_failure().expect("failed journal read"),
                },
            };
        }
        let mut kinds: Vec<_> = transition.into_iter().collect();
        if let Some(document) = &document {
            kinds.push(document.event.clone());
        }
        kinds.push(EventKind::MessageSent {
            message: envelope.id.clone(),
            from: envelope.from.clone(),
            to: envelope.to.clone(),
            kind: envelope.kind.clone(),
        });
        if let Some(entry) = &journal {
            kinds.push(EventKind::JournalAppended {
                entry: entry.clone(),
            });
        }
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
            .map(|kind| Event::new(kind, envelope.sent_at))
            .collect();
        // The person's own messages are followed per recipient, so they can
        // see where each stands; agents' traffic is not.
        let tracked = self.is_human_id(&AgentId::from(envelope.from.as_str()));
        let _ = self.commit(
            "message",
            events,
            |store, events| {
                store.publish_message_with_channel(
                    &envelope,
                    &recipients,
                    INBOX_CAPACITY,
                    sender.as_ref(),
                    question.as_ref(),
                    closed.as_ref(),
                    events,
                    channel.as_ref().map(|channel| (channel, journal.as_ref())),
                    document
                        .as_ref()
                        .map(|d| (d.kind, d.id.as_str(), d.value.as_ref())),
                    tracked,
                )
            },
            |state, _, ()| {
                if let Some(channel) = &channel {
                    state.channels.insert(channel.id.clone(), channel.clone());
                }
                if let Some(entry) = &journal {
                    state
                        .journal_seq
                        .insert(entry.project.clone(), entry.seq + 1);
                    state.cache_journal(entry.clone());
                }
                for id in &recipients {
                    let queue = state.inboxes.entry(id.clone()).or_default();
                    queue.push_back(envelope.clone());
                    *state.inbox_bytes.entry(id.clone()).or_default() += bytes;
                }
                if let Some(sender) = &sender {
                    let current = state
                        .registry
                        .get_mut(&sender.id)
                        .expect("sender retained under lock");
                    *current = sender.clone();
                }
                if let Some(question) = &question {
                    state
                        .questions
                        .insert(question.id.clone(), question.clone());
                }
                if let Some(closed) = &closed {
                    state.questions.remove(closed);
                }
            },
        );
        if let Some(error) = self.write_failure() {
            return error;
        }
        if let Some(question) = closed {
            if held {
                self.held_answers.insert(envelope.id.clone(), question);
            } else {
                self.emit(EventKind::AnswerRouted {
                    question,
                    answer: envelope.id.clone(),
                    route: agentdocker_core::AnswerRoute::Queue,
                });
            }
        }
        self.notify_humans(&envelope, &recipients);
        let subscribers = self.bus.send(envelope.clone()).unwrap_or(0);
        let now = Utc::now();
        let mut readiness = agentdocker_core::SendReadiness::default();
        let blocked_sources: Vec<_> = self
            .registry
            .provider_block_ids()
            .into_iter()
            .filter_map(|id| self.registry.get(&id))
            .collect();
        for id in &recipients {
            let issue = self.registry.get(id).map_or_else(
                || Some(agentdocker_core::RecipientReadiness::unknown(id.clone())),
                |agent| {
                    let blocked = agentdocker_core::readiness::provider_issue(
                        agent,
                        blocked_sources.iter().copied(),
                    );
                    agentdocker_core::RecipientReadiness::for_agent(agent, now, blocked)
                },
            );
            readiness.observe(issue);
        }
        Response::Sent {
            message: envelope.id,
            subscribers,
            recipient_readiness: Some(readiness),
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
        if let Some(error) = self.write_failure() {
            return error;
        }
        if record.spec.name.is_empty() {
            record.spec.name = default_name(&record.id);
            record.spec.labels.insert(
                agentdocker_core::agent::NAME_LABEL.to_owned(),
                agentdocker_core::agent::GENERATED_NAME.to_owned(),
            );
        }
        // `role:<name>` is how a role is addressed; a name spelled that
        // way would be reached as the role or shadow it, never plainly.
        if record
            .spec
            .name
            .starts_with(agentdocker_core::agent::ROLE_PREFIX)
        {
            return Response::error(
                ErrorCode::Invalid,
                "an agent's name cannot start with `role:`; that is how a role is addressed",
            );
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
        // resolve to the same project. (The checkout came back in to tell
        // linked worktrees apart; it decides only where a side names no
        // session — see `identity::same_registration`.)
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
        let session = record
            .spec
            .labels
            .get("session_id")
            .filter(|id| !id.is_empty());
        let existing = match candidates.as_slice() {
            [only] => Some(only.clone()),
            [] => None,
            // A registration that names its session matches the records
            // that name the same session, wherever each is, and a
            // transport in its own checkout that names none yet. A record
            // that names the session is the session's: when there is one,
            // it answers, and when there are several (one session counted
            // twice, left by a registration from a moved shell before the
            // rule above) the one that stays answers (`identity::keeper`).
            // A transport beside it is neither joined nor taught the
            // session, which would make it a second record of the session.
            // With none naming it, the candidates are transports in this
            // checkout that cannot be told apart, and are refused.
            many if session.is_some() => {
                let named = many
                    .iter()
                    .filter_map(|id| self.registry.get(id))
                    .filter(|a| {
                        a.spec
                            .labels
                            .get("session_id")
                            .is_some_and(|id| !id.is_empty())
                    });
                match agentdocker_core::identity::keeper(named, |a| {
                    self.live_subscribers.get(&a.id).is_some_and(|n| *n > 0)
                }) {
                    Some(kept) => Some(kept.id.clone()),
                    None => {
                        return Response::error(
                            ErrorCode::Invalid,
                            format!(
                                "this process already has {} agents in this checkout that name \
                                 no session, so which one is this session's cannot be told",
                                many.len()
                            ),
                        );
                    }
                }
            }
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
                // With the verified profile its registration carries, so
                // the conversation's key is whole on the record.
                if let Some(profile) = record
                    .spec
                    .labels
                    .get(agentdocker_core::agent::PROFILE_LABEL)
                {
                    updated
                        .spec
                        .labels
                        .entry(agentdocker_core::agent::PROFILE_LABEL.to_owned())
                        .or_insert_with(|| profile.clone());
                }
                let event = Event::new(
                    EventKind::AgentSessionBound {
                        agent: id.clone(),
                        session,
                    },
                    Utc::now(),
                );
                let _ = self.transition(
                    "session binding",
                    event,
                    |store, event| store.agent_transition(&updated, event),
                    |state| {
                        *state.registry.get_mut(&id).expect("just found") = updated.clone();
                    },
                );
                if let Some(error) = self.write_failure() {
                    return error;
                }
            }
            let agent = self.registry.get(&id).cloned().expect("just found");
            return Response::Agent {
                agent: self.resume_session(agent),
            };
        }
        if let Err(err) = self.registry.insert(record.clone()) {
            return registry_error(err);
        }
        if self.persist("agent", |store| store.upsert_agent(&record)) != Persisted::Committed {
            self.registry.remove(&record.id);
            return self.write_failure().expect("a refused write has a reason");
        }
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
        if let Some(error) = self.write_failure() {
            return error;
        }
        let new = record.id.clone();
        let agent = self.resume_session(record);
        // A session that took up its own earlier record carries on with its
        // own queue; only one new to the project is caught up.
        if agent.id == new {
            self.catch_up(&agent);
        }
        Response::Agent { agent }
    }

    /// A session that opens in a project shortly after the person said
    /// something to everyone there gets it as its first input: the person
    /// should not have to know which sessions were open the moment they
    /// spoke. Only the person's messages to that project's everyone room
    /// in the last hour count, never a pause or resume notice, and never
    /// twice: a record that shares a session or a process with another
    /// already had its chance. A queue without room is caught up as far as
    /// it has room, and the session is never refused for it.
    fn catch_up(&mut self, record: &AgentRecord) {
        if humans::is_human(record) || self.write_failure().is_some() {
            return;
        }
        let Some(project) = record.project.as_ref().map(ProjectRef::id) else {
            return;
        };
        let session = record
            .spec
            .labels
            .get("session_id")
            .filter(|id| !id.is_empty());
        let process = record.pid.map(|pid| (pid, record.process_started_at));
        let twin = self.registry.all().any(|other| {
            other.id != record.id
                && (session.is_some_and(|id| other.spec.labels.get("session_id") == Some(id))
                    || process.is_some_and(|process| {
                        other.pid.map(|pid| (pid, other.process_started_at)) == Some(process)
                    }))
        });
        if twin {
            return;
        }
        let conversation = agentdocker_core::ConversationId::everyone(&project);
        let Ok(recent) = self.store.history(&conversation, None, CATCH_UP_MESSAGES) else {
            return;
        };
        let since = Utc::now() - CATCH_UP_WINDOW;
        let queue = self.inboxes.get(&record.id);
        let queued: HashSet<&MessageId> = queue.into_iter().flatten().map(|m| &m.id).collect();
        let mut count = queue.map_or(0, VecDeque::len);
        let mut bytes = self.inbox_bytes.get(&record.id).copied().unwrap_or(0);
        let mut messages = Vec::new();
        for archived in recent {
            let envelope = archived.envelope;
            if envelope.sent_at < since
                || pause::reserved_message_kind(&envelope.kind)
                || queued.contains(&envelope.id)
                || !self.is_human_id(&AgentId::from(envelope.from.as_str()))
            {
                continue;
            }
            let size = message_bytes(&envelope);
            if count >= INBOX_CAPACITY || bytes + size > INBOX_BYTES {
                break;
            }
            count += 1;
            bytes += size;
            messages.push(envelope);
        }
        drop(queued);
        if messages.is_empty() {
            return;
        }
        let event = Event::new(
            EventKind::MessagesCaughtUp {
                agent: record.id.clone(),
                messages: messages.iter().map(|m| m.id.clone()).collect(),
            },
            Utc::now(),
        );
        let _ = self.transition(
            "catch-up",
            event,
            |store, event| store.enqueue_late(&record.id, &messages, INBOX_CAPACITY, event),
            |state| {
                for message in &messages {
                    *state.inbox_bytes.entry(record.id.clone()).or_default() +=
                        message_bytes(message);
                    state
                        .inboxes
                        .entry(record.id.clone())
                        .or_default()
                        .push_back(message.clone());
                }
            },
        );
    }

    /// A session that came back as a new process takes up the record it
    /// ended with. The hooks adapter vouches for the session (its
    /// `session_id` label), the rule in core says which ended records held
    /// it, and then — as `resume_input` does for a bound provider — the
    /// one that ended last stays canonical and takes the new process: its
    /// id, its conversations, its cursor and what was still queued for it
    /// carry on; the fresh record and any earlier ended record of the
    /// session are retired into it, their queues joining in durable sequence
    /// order with each message once, the documents that named them
    /// rewritten, aliases that pointed at them flattened, and their ids
    /// becoming aliases. The store plans and writes it as one transaction
    /// with the event; memory follows. Anything that does not qualify — a
    /// record whose process is still there, one that holds leases,
    /// is waiting on something, has stale changes owed to
    /// it, has a live subscriber, has ambiguous observations, or a
    /// store that cannot say or refuses the rewrite — leaves the fresh
    /// record as it is.
    fn resume_session(&mut self, fresh: AgentRecord) -> AgentRecord {
        // An initialized receiver may already have offered its queue head.
        // Folding older backlog ahead of that offer would reorder delivery.
        // Before initialization, channel ownership is also locked by provider
        // process generation so changing an agent ID cannot admit a second MCP.
        if fresh.input_delivery.is_some() {
            return fresh;
        }
        let Some(session) = fresh
            .spec
            .labels
            .get("session_id")
            .filter(|id| !id.is_empty())
            .cloned()
        else {
            return fresh;
        };
        let ended: Vec<AgentRecord> =
            agentdocker_core::identity::resumed_session(self.registry.all(), &fresh)
                .into_iter()
                .cloned()
                .collect();
        let Some((prior, earlier)) = ended.split_first() else {
            return fresh;
        };
        let Some(pid) = fresh.pid else {
            return fresh;
        };
        // Ended on a record is not enough: the process it named must be
        // gone, or this is two lives of one session at once.
        for record in earlier.iter().chain(std::iter::once(prior)) {
            if record.pid.is_some_and(|old| {
                process_exists(old) && same_process(old, record.process_started_at)
            }) {
                return fresh;
            }
        }
        // Nothing any folded record did is lost: one that holds leases,
        // waits on a lease, has stale changes
        // owed to it or has somebody subscribed to it keeps its own record.
        // Observations and eligible channel memberships join transactionally
        // in plan_fold; a rewrite that would create a self-review refuses.
        // Fail closed on unreadable observations, including the canonical
        // record: malformed storage must still disable coordination.
        let mut folded: Vec<&AgentRecord> = vec![&fresh];
        folded.extend(earlier);
        for record in folded.iter().copied().chain(std::iter::once(prior)) {
            if self
                .store_read("reads", |store| {
                    store.document::<Vec<agentdocker_core::ReadMark>>("reads", record.id.as_str())
                })
                .is_none()
            {
                return fresh;
            }
        }
        for record in &folded {
            if !self.leases.by_holder(&record.id).is_empty()
                || self.waiting.waiting_for(&record.id).is_some()
                || self.pending_stale.contains_key(&record.id)
                || self.stale_outstanding.contains_key(&record.id)
                || self
                    .live_subscribers
                    .get(&record.id)
                    .is_some_and(|n| *n > 0)
            {
                return fresh;
            }
        }
        let now = Utc::now();
        let mut canonical = prior.clone();
        canonical.pid = Some(pid);
        canonical.process_started_at = fresh.process_started_at;
        canonical.process_group = fresh.process_group;
        canonical.status = fresh.status.clone();
        canonical.started_at = fresh.started_at;
        canonical.finished_at = None;
        canonical.last_seen = now;
        canonical.session = fresh.session.clone();
        canonical.terminal = fresh.terminal.clone();
        canonical.vcs = fresh.vcs.clone();
        canonical.reported_activity = fresh.reported_activity.clone();
        canonical.adapter_contacts = fresh.adapter_contacts.clone();
        canonical.queue_read = fresh.queue_read.clone();
        canonical.provider_availability = None;
        for record in &folded {
            for (key, value) in &record.spec.labels {
                canonical
                    .spec
                    .labels
                    .entry(key.clone())
                    .or_insert_with(|| value.clone());
            }
            for (message, at) in &record.legacy_offers {
                canonical
                    .legacy_offers
                    .entry(message.clone())
                    .or_insert(*at);
            }
        }
        let retired: Vec<AgentId> = folded.iter().map(|record| record.id.clone()).collect();
        let kind = EventKind::SessionResumed {
            agent: prior.id.clone(),
            retired: retired.clone(),
            session,
            pid,
        };
        match self.commit_fold(canonical, &retired, kind, now) {
            Ok(canonical) => canonical,
            Err(error) => {
                info!(agent = %prior.id, %error, "a session's return was not folded");
                fresh
            }
        }
    }

    /// Retire `retired` into `canonical`, which already carries what it
    /// takes from them, as one store transaction with the event `kind`, then
    /// memory the same way: their queues become its queue in durable order
    /// with each message once, the documents and aliases that named them
    /// name it, their cursors go, and questions and channel memberships in
    /// memory follow the store. Nothing changes on a refusal, which is
    /// returned.
    fn commit_fold(
        &mut self,
        canonical: AgentRecord,
        retired: &[AgentId],
        kind: EventKind,
        now: chrono::DateTime<Utc>,
    ) -> Result<AgentRecord, String> {
        let kept = canonical.id.clone();
        let plan = self
            .store
            .plan_fold(&canonical, retired)
            .map_err(|error| error.to_string())?;
        // The one queue is the store's: durable order, each message once
        // (the same broadcast reached more than one life), so a reopen
        // delivers exactly what memory does now.
        let merged = plan.queue.clone();
        // Memory moves only on a committed write: a fenced daemon skips
        // the write and leaves the records as they were, and they stand
        // until a later attempt after the transfer folds them.
        let committed = self.transition(
            "record fold",
            Event::new(kind, now),
            |store, event| store.write_resume(&plan, event),
            |state| {
                match state.registry.fold_into(retired, &kept) {
                    Ok(_) => {}
                    Err(error) => {
                        // Checked by the plan; the store has the aliases, memory
                        // must follow them.
                        error!(%error, "folding records into one");
                    }
                }
                *state.registry.get_mut(&kept).expect("canonical record") = canonical.clone();
                let project = canonical.project.as_ref().map(ProjectRef::id);
                for id in retired {
                    state.inboxes.remove(id);
                    state.inbox_bytes.remove(id);
                    state.live_subscribers.remove(id);
                    if let Some(project) = &project {
                        // A retired record's cursor goes with it — the fresh one's
                        // freshly seeded, an earlier life's long passed; the
                        // canonical record's own continues where it left off.
                        state
                            .journal_cursors
                            .remove(&(id.as_str().to_owned(), project.clone()));
                    }
                }
                // What the store rewrote, memory rewrites the same way: a question
                // an earlier life asked is the canonical record's to cancel now,
                // and channel membership names the record that is.
                for question in state.questions.values_mut() {
                    if retired.iter().any(|id| id.as_str() == question.from) {
                        question.from = kept.to_string();
                    }
                    if let agentdocker_core::Destination::Agent(to) = &mut question.to
                        && retired.contains(to)
                    {
                        *to = kept.clone();
                    }
                }
                for channel in state.channels.values_mut() {
                    for member in &mut channel.members {
                        if retired.contains(member) {
                            *member = kept.clone();
                        }
                    }
                    let mut seen = HashSet::new();
                    channel.members.retain(|id| seen.insert(id.clone()));
                    if let Some(opened_by) = &mut channel.opened_by
                        && retired.contains(opened_by)
                    {
                        *opened_by = kept.clone();
                    }
                    for review in &mut channel.reviews {
                        if retired.contains(&review.by) {
                            review.by = kept.clone();
                        }
                        if retired.contains(&review.of) {
                            review.of = kept.clone();
                        }
                    }
                }
                let moved: usize = merged.iter().map(message_bytes).sum();
                state
                    .inboxes
                    .insert(kept.clone(), merged.into_iter().collect());
                state.inbox_bytes.insert(kept.clone(), moved);
                // Anything retired that the daemon had created a worktree for is the
                // canonical record's worktree now.
                for creator in state.worktree_creators.values_mut() {
                    if retired.contains(creator) {
                        *creator = kept.clone();
                    }
                }
                // Stale notices owed for the observations that just joined are owed
                // to the canonical record; one still queued moved with the queue.
                for id in retired {
                    if let Some(pending) = state.pending_stale.remove(id) {
                        let owed = state.pending_stale.entry(kept.clone()).or_default();
                        for (path, change) in pending {
                            if owed.get(&path).is_none_or(|known| known.at < change.at) {
                                owed.insert(path, change);
                            }
                        }
                    }
                    if let Some(message) = state.stale_outstanding.remove(id) {
                        state
                            .stale_outstanding
                            .entry(kept.clone())
                            .or_insert(message);
                    }
                }
            },
        );
        if committed != Persisted::Committed {
            return Err("the store did not take the fold".into());
        }
        Ok(canonical)
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
mod tests;

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
