//! Replacing a running daemon without dropping what it was holding.
//!
//! An upgrade must not disturb a running agent, and the hard part is not
//! the state — that is in SQLite and survives anything. It is the open
//! descriptors. The listening socket is one: rebinding it means a window
//! where the path is unbound, and a client that connects in that window
//! is refused rather than made to wait. A pty master is another, and
//! worse, because it cannot be reopened at all — it dies with the
//! process holding it, and `attach` afterwards has nothing on the other
//! end.
//!
//! `SCM_RIGHTS` carries them across, and `agentdocker_host::handoff` is
//! the mechanism. This module is the *protocol*: what the two processes
//! say to each other, in what order, and — the part the previous attempt
//! got wrong — who is still responsible when it fails.
//!
//! The rule that attempt broke, written down so it is not broken again:
//! **the predecessor stays responsible until the successor says it is
//! serving.** That version reported success as soon as the descriptors
//! were sent and then exited; the successor might still have been
//! failing to start, and dropping the live `Child` handles on the way
//! out killed the very agents the upgrade was meant to preserve. So the
//! reply is not "I received", it is "I am serving", and until it arrives
//! nothing is given up. A handover that fails leaves a daemon that never
//! stopped working.
//!
//! Production `Reload` still refuses. The pieces land and are proven one
//! at a time — listener, pty, stdio and log ownership, child lifetime and
//! reaping, readiness, rollback — and only when all of them are proven
//! together does the request stop saying no. A partly-working upgrade is
//! worse than one that admits it cannot go yet.

#[cfg(unix)]
use std::io;
#[cfg(unix)]
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
#[cfg(unix)]
use std::os::unix::net::UnixStream;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::time::Duration;
#[cfg(unix)]
use std::time::Instant;

#[cfg(unix)]
use agentdocker_host::handoff;
use serde::{Deserialize, Serialize};

use super::*;
use agentdocker_core::session::{Transfer, TransferState};

/// How long a successor has to say it is serving.
///
/// It has to open a database and restore its state, and it binds nothing
/// — the socket arrives already bound — so this is generous. It is also
/// bounded: a successor that never answers must not leave the
/// predecessor waiting for ever, because the predecessor is still the
/// one serving and it has stopped doing anything else in order to wait.
pub const READY_WITHIN: Duration = Duration::from_secs(30);

/// The version of this conversation. A successor that does not recognise
/// it refuses rather than guessing what the descriptors mean.
pub const FORMAT: u32 = 2;

/// The environment variable that lets `reload` actually replace the
/// daemon, when set to exactly `1`. Otherwise `reload` refuses as it
/// always has: the mechanism is complete but its acceptance matrix is
/// still being run, and nobody should be replaced by accident.
pub const ENABLE: &str = "AGENTDOCKER_EXPERIMENTAL_RELOAD";

/// How long a candidate gets to answer `--build-info`. A candidate that
/// hangs there would otherwise hold the reload, and its thread, for ever.
pub const BUILD_INFO_WITHIN: Duration = Duration::from_secs(10);

/// Whether the gate is open: exactly `1`, as the documentation says.
pub fn enabled() -> bool {
    std::env::var(ENABLE).ok().as_deref() == Some("1")
}

/// The executable a reload hands over to. Absent, the release the
/// managed installation has activated since this daemon started, if any;
/// otherwise the daemon's own executable.
pub const CANDIDATE: &str = "AGENTDOCKER_RELOAD_CANDIDATE";

/// Which executable a reload hands over to, and why.
pub fn candidate() -> std::io::Result<(PathBuf, &'static str)> {
    if let Some(path) = std::env::var_os(CANDIDATE) {
        return Ok((PathBuf::from(path), "named by the environment"));
    }
    // Resolved, not as invoked: a daemon started through the launcher link
    // must still know which release directory it runs from.
    let own = agentdocker_host::procinfo::executable_path()?;
    match agentdocker_host::installation::activated_daemon(&own) {
        Some(activated) => Ok((activated, "the release the installation activated")),
        None => Ok((own, "this daemon's own executable")),
    }
}

/// What the predecessor hands over, beside the descriptors themselves.
///
/// The descriptors arrive as a list with no names on them, so this says
/// what each one is. Order is the only thing `SCM_RIGHTS` preserves.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Handover {
    pub format: u32,
    /// The coordinator transfer this handover completes; the successor's
    /// first write is accepting exactly this one.
    pub transfer: String,
    /// Where the listening socket sits in the descriptor list.
    pub listener: usize,
    /// Where the daemon lock's descriptor sits: held open by the
    /// successor for its life, so no autostart finds the lock vacant
    /// while the predecessor is leaving.
    pub lock: usize,
    /// Where the restricted container endpoint's listener sits, when the
    /// predecessor had it up; absent means the successor binds its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restricted: Option<usize>,
    /// The home and socket the predecessor served, spelled as it spelled
    /// them, so the successor derives every other path the same way.
    pub home: PathBuf,
    pub socket: PathBuf,
    /// The agents whose terminals are travelling, and where each one's
    /// descriptor sits. Empty since session owners: children and their
    /// terminals stay with their owners, not with any daemon.
    #[serde(default)]
    pub terminals: Vec<Terminal>,
    /// Processes the successor becomes responsible for without becoming
    /// their parent. Empty since session owners, for the same reason.
    #[serde(default)]
    pub adopted: Vec<Adopted>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Terminal {
    pub agent: AgentId,
    pub fd: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Adopted {
    pub agent: AgentId,
    pub pid: u32,
    /// Recorded so a recycled pid cannot be mistaken for the process
    /// that was handed over.
    pub started_at: Option<DateTime<Utc>>,
}

/// What a successor says back.
///
/// Deliberately not a bare success or failure: the reason is what the
/// predecessor logs and hands to whoever asked for the reload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Ready {
    /// Restored and accepting connections. Only now is the predecessor
    /// free to go.
    Serving,
    /// It could not take over. The predecessor keeps serving and says
    /// why; nothing has been given up.
    Failed { reason: String },
}

#[cfg(unix)]
/// Send the descriptors and the map that explains them.
pub fn offer(socket: &UnixStream, handover: &Handover, fds: &[BorrowedFd<'_>]) -> io::Result<()> {
    let payload = serde_json::to_vec(handover).map_err(io::Error::other)?;
    handoff::send(socket, &payload, fds)
}

#[cfg(unix)]
/// Take the descriptors and the map, as the successor.
pub fn accept(socket: &UnixStream) -> io::Result<(Handover, Vec<OwnedFd>)> {
    let (payload, fds) = handoff::receive(socket)?;
    let handover: Handover = serde_json::from_slice(&payload).map_err(io::Error::other)?;
    if handover.format != FORMAT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "handover format {} is not the {FORMAT} this daemon speaks",
                handover.format
            ),
        ));
    }
    // Every index has to name a descriptor that actually arrived.
    // Otherwise the successor takes over holding a terminal it cannot
    // find, and the agent on the other end is attached to nothing with
    // nobody saying so.
    let named: Vec<usize> = [handover.listener, handover.lock]
        .into_iter()
        .chain(handover.restricted)
        .chain(handover.terminals.iter().map(|t| t.fd))
        .collect();
    for &index in &named {
        if index >= fds.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "handover names descriptor {index} but only {} arrived",
                    fds.len()
                ),
            ));
        }
    }
    // Each descriptor belongs to one thing. Two names for one of them —
    // the listener also claimed as a terminal, or two agents pointed at
    // the same pty — is a map that cannot be true, and following it
    // would give an agent somebody else's terminal.
    let mut once = named.clone();
    once.sort_unstable();
    once.dedup();
    if once.len() != named.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "handover gives one descriptor to more than one owner",
        ));
    }
    // And one terminal per agent, for the same reason from the other
    // direction.
    let mut agents: Vec<&AgentId> = handover.terminals.iter().map(|t| &t.agent).collect();
    let owners = agents.len();
    agents.sort_unstable();
    agents.dedup();
    if agents.len() != owners {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "handover gives one agent more than one terminal",
        ));
    }
    // A process handed over without a birth time cannot be told from a
    // recycled pid afterwards, so the successor would be adopting
    // whatever now holds that number.
    if let Some(unverified) = handover.adopted.iter().find(|a| a.started_at.is_none()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "handover adopts pid {} with no start time, which cannot be told from a \
                 recycled one",
                unverified.pid
            ),
        ));
    }
    Ok((handover, fds))
}

#[cfg(unix)]
/// Say whether the takeover worked, as the successor.
pub fn answer(socket: &UnixStream, ready: &Ready) -> io::Result<()> {
    socket.set_write_timeout(Some(READY_WITHIN))?;
    let payload = serde_json::to_vec(ready).map_err(io::Error::other)?;
    handoff::send(socket, &payload, &[])
}

#[cfg(unix)]
/// Both startup outcomes may wait for a predecessor that has stopped reading.
/// Keep the bounded socket write off the async executor in either case.
pub async fn answer_async(socket: UnixStream, ready: Ready) -> io::Result<()> {
    tokio::task::spawn_blocking(move || answer(&socket, &ready))
        .await
        .map_err(io::Error::other)?
}

#[cfg(unix)]
/// Wait for the successor to say it is serving.
///
/// This is the only thing that can authorise giving up the socket, and
/// every path out of it that is not `Ok` means "keep serving". A
/// refusal, a malformed reply, a successor that died without saying
/// anything, and a deadline all leave the predecessor exactly as it was.
pub fn await_ready(socket: &UnixStream, within: Duration) -> Result<(), String> {
    // Zero means "no timeout" to the kernel, which is the opposite of
    // anything a caller passing zero could want here. Refused rather
    // than quietly turned into an unbounded wait.
    if within.is_zero() {
        return Err("a successor deadline of zero would never expire".to_owned());
    }
    let deadline = Instant::now()
        .checked_add(within)
        .ok_or_else(|| "successor deadline is out of range".to_owned())?;
    // Poll and recheck the same deadline before every nonblocking read.
    // A per-read socket timeout would restart when another byte arrives.
    let (payload, fds) = handoff::receive_until(socket, deadline).map_err(|e| {
        if Instant::now() >= deadline {
            format!("the successor did not say it was serving within {within:?}")
        } else {
            format!("the successor said nothing: {e}")
        }
    })?;
    // Checked after the read as well as during it: a reply that arrived
    // in pieces can satisfy every per-read timeout and still have taken
    // longer than the caller allowed.
    if Instant::now() >= deadline {
        return Err(format!(
            "the successor did not finish saying it was serving within {within:?}"
        ));
    }
    // A readiness answer carries words, not descriptors. Anything
    // attached to one is a confused successor or not a successor at all,
    // and taking its descriptors on trust is how a daemon ends up
    // holding files nobody meant it to have.
    if !fds.is_empty() {
        return Err(format!(
            "the successor's answer carried {} descriptors, which a readiness reply never does",
            fds.len()
        ));
    }
    match serde_json::from_slice::<Ready>(&payload) {
        Ok(Ready::Serving) => Ok(()),
        Ok(Ready::Failed { reason }) => {
            Err(format!("the successor refused to take over: {reason}"))
        }
        Err(e) => Err(format!("the successor's answer made no sense: {e}")),
    }
}

/// What the predecessor learns about a candidate executable before it
/// trusts it with the database: `agentd --build-info`, read once.
#[derive(Debug, Deserialize)]
#[cfg(unix)]
struct BuildInfo {
    format: u32,
    os: String,
    arch: String,
    state_schema: i64,
}

/// The descriptors and lock a daemon needs to hand over, given to it by
/// its main once serving starts.
#[cfg(unix)]
pub struct Held {
    pub listener: std::os::fd::OwnedFd,
    pub lock: std::os::fd::OwnedFd,
    /// The restricted endpoint's listener, once it is up.
    pub restricted: Option<std::os::fd::OwnedFd>,
    /// The restricted endpoint is up but its listener could not be kept
    /// for a handover: a successor would find the path busy and come up
    /// without container access, so a reload is refused instead.
    pub restricted_unavailable: Option<String>,
}

/// On Windows nothing is handed over: the daemon holds no descriptors a
/// successor could inherit, and `reload` answers `unavailable` up front.
/// The transfer bookkeeping below is shared, so the store's view of a
/// transfer reads the same on every platform.
#[cfg(windows)]
pub struct Held {}

#[cfg(windows)]
impl Daemon {
    pub(super) async fn hand_over(self: &Arc<Self>) -> Response {
        Response::error(
            ErrorCode::Unavailable,
            "live daemon reload is not available on Windows; stop and start the daemon instead, and its agents reconnect",
        )
    }

    pub async fn transferred_exit(&self) {
        self.transferred_exit.notified().await;
    }

    pub fn hold(&self, held: Held) {
        *lock(&self.held) = Some(held);
    }

    pub fn hold_restricted(&self, _fd: std::io::Result<()>) {}
}

#[cfg(unix)]
impl Daemon {
    pub(super) async fn hand_over(self: &Arc<Self>) -> Response {
        if !enabled() {
            // The mechanism is in place; its acceptance matrix is still
            // being run. Refusing is not a failure: the daemon and its
            // agents keep running, which is exactly what the earliest
            // attempt did not manage.
            return Response::error(
                ErrorCode::Unavailable,
                format!(
                    "live daemon reload is unavailable: session owners, the coordinator fence and the \
                     successor handover are in place, but their acceptance matrix is still being recorded; \
                     set {ENABLE}=1 on the daemon to allow it; the current daemon and agents remain running"
                ),
            );
        }
        let held = match self.take_held() {
            Ok(held) => held,
            Err(refusal) => return *refusal,
        };
        let outcome = self.replace(&held).await;
        match outcome {
            Ok(()) => {
                // The successor is serving. This daemon leaves without
                // touching agents, socket or lock: all three are its now.
                self.emit(EventKind::DaemonStopping {
                    reason: "transferred".to_owned(),
                });
                self.transferred_exit.notify_one();
                Response::Ok
            }
            Err(refusal) => {
                // Nothing was given up unless the store says it was.
                *lock(&self.held) = Some(held);
                *refusal
            }
        }
    }

    /// A refusal from the daemon's own side, before or after the offer.
    fn unavailable(reason: impl Into<String>) -> Box<Response> {
        Box::new(Response::error(ErrorCode::Unavailable, reason))
    }

    /// Validate the installed candidate, offer the transfer, spawn the
    /// successor with the listener and lock, and wait until it says it is
    /// serving. On any failure take authority back if the store still
    /// lets us.
    async fn replace(self: &Arc<Self>, held: &Held) -> Result<(), Box<Response>> {
        let (candidate, chosen) = candidate()
            .map_err(|e| Self::unavailable(format!("cannot find this daemon's executable: {e}")))?;
        info!(candidate = %candidate.display(), chosen, "reload candidate");
        let info = tokio::task::spawn_blocking({
            let candidate = candidate.clone();
            move || build_info(&candidate, BUILD_INFO_WITHIN)
        })
        .await
        .map_err(|e| Self::unavailable(e.to_string()))?
        .map_err(Self::unavailable)?;
        let schema = lock(&self.state).store.schema_version();
        if info.format != 1
            || info.os != std::env::consts::OS
            || info.arch != std::env::consts::ARCH
        {
            return Err(Self::unavailable(format!(
                "candidate {} is for {}/{} (format {}); this host is {}/{}",
                candidate.display(),
                info.os,
                info.arch,
                info.format,
                std::env::consts::OS,
                std::env::consts::ARCH
            )));
        }
        if info.state_schema < schema {
            return Err(Self::unavailable(format!(
                "candidate state schema {} is older than the database's {schema}; a successor cannot roll the database back",
                info.state_schema
            )));
        }
        // Pair first, so a successor that cannot be spawned costs nothing.
        let (ours, theirs) = UnixStream::pair()
            .map_err(|e| Self::unavailable(format!("cannot make the handover socket: {e}")))?;
        // The offer's own refusal keeps its code: `backpressure` while a
        // mutation is still executing, `conflict` over another offer.
        let transfer = self.offer_transfer(std::process::id())?;
        let transfer_id = transfer.id.clone();
        // The successor's pid is not known until spawn; the offer named
        // ours as a placeholder and is corrected under the same fence.
        let mut command = std::process::Command::new(&candidate);
        command
            .arg("--take-over")
            .arg("3")
            .env("AGENTDOCKER_HOME", &self.home)
            .env("AGENTDOCKER_SOCKET", &self.socket)
            // The successor's log continues where this daemon's does: the
            // subscriber writes to stdout, and a service manager captures
            // both streams, so both are inherited.
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit());
        let theirs_fd = theirs.as_raw_fd();
        // SAFETY: `dup2`, `fcntl` and `setsid` are async-signal-safe and act
        // only on descriptors this process owns.
        unsafe {
            command.pre_exec(move || {
                if theirs_fd == 3 {
                    // Already where it must be: `dup2` onto itself would
                    // leave close-on-exec set and the successor would find
                    // descriptor 3 closed. Clear the flag instead.
                    let flags = nix::libc::fcntl(3, nix::libc::F_GETFD);
                    if flags < 0
                        || nix::libc::fcntl(3, nix::libc::F_SETFD, flags & !nix::libc::FD_CLOEXEC)
                            < 0
                    {
                        return Err(std::io::Error::last_os_error());
                    }
                } else if nix::libc::dup2(theirs_fd, 3) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                nix::libc::setsid();
                Ok(())
            });
        }
        let child = match command.spawn() {
            Ok(child) => child,
            Err(e) => {
                return Err(self.abort_reload(&format!("cannot spawn the successor: {e}")));
            }
        };
        drop(theirs);
        let successor_pid = child.id();
        if !lock(&self.state).readdress_offer(&transfer_id, successor_pid) {
            let _ = kill_child(child);
            return Err(self.abort_reload("could not address the offer to the spawned successor"));
        }
        let mut fds = vec![held.listener.as_fd(), held.lock.as_fd()];
        let restricted = held.restricted.as_ref().map(|fd| {
            fds.push(fd.as_fd());
            2
        });
        let handover = Handover {
            format: FORMAT,
            transfer: transfer_id.clone(),
            listener: 0,
            lock: 1,
            restricted,
            home: self.home.clone(),
            socket: self.socket.clone(),
            terminals: Vec::new(),
            adopted: Vec::new(),
        };
        let offered = offer(&ours, &handover, &fds);
        if let Err(e) = offered {
            let _ = kill_child(child);
            return Err(self.abort_reload(&format!("cannot send the handover: {e}")));
        }
        let ready = tokio::task::spawn_blocking(move || await_ready(&ours, READY_WITHIN))
            .await
            .map_err(|e| Self::unavailable(e.to_string()))?;
        match ready {
            Ok(()) => Ok(()),
            Err(reason) => {
                // Silent or refusing successor: what does the store say?
                // Accepted means it owns the database whatever it said
                // afterwards; we must go. Otherwise take authority back.
                match self.transfer_state() {
                    Some(t) if t.id == transfer_id && t.state == TransferState::Accepted => {
                        warn!(%reason, "successor accepted the database but did not report serving; leaving anyway");
                        Ok(())
                    }
                    _ => {
                        let _ = kill_child(child);
                        Err(self.abort_reload(&reason))
                    }
                }
            }
        }
    }

    /// Report whether the failed successor's offer was durably withdrawn.
    fn abort_reload(&self, reason: &str) -> Box<Response> {
        let outcome = if self.abort_transfer(reason) {
            "successor failed and the offer was withdrawn"
        } else {
            "successor failed and the offer could not be withdrawn; writing remains disabled, check `daemon status`"
        };
        Self::unavailable(format!("{outcome}: {reason}"))
    }

    /// Resolves once a completed handover says this daemon may leave.
    pub async fn transferred_exit(&self) {
        self.transferred_exit.notified().await;
    }

    /// Give the daemon the listener and lock it will hand over.
    pub fn hold(&self, held: Held) {
        *lock(&self.held) = Some(held);
    }

    /// Check readiness and take descriptors under the registration lock. An
    /// endpoint still binding cannot lose its descriptor to a concurrent reload.
    fn take_held(&self) -> Result<Held, Box<Response>> {
        let mut slot = lock(&self.held);
        let held = slot.as_ref().ok_or_else(|| {
            Self::unavailable("this daemon holds no listener or lock to hand over")
        })?;
        if let Some(reason) = &held.restricted_unavailable {
            return Err(Self::unavailable(format!(
                "the container endpoint's listener cannot be handed over ({reason}); \
                 this daemon keeps serving"
            )));
        }
        if held.restricted.is_none() {
            return Err(Box::new(Response::error(
                ErrorCode::Backpressure,
                "the container endpoint is still starting; retry reload after it is ready",
            )));
        }
        Ok(slot.take().expect("checked descriptor ownership"))
    }

    /// The restricted endpoint came up: keep its listener to hand over,
    /// or remember that it could not be kept, which refuses reloads
    /// rather than handing over to a daemon without container access.
    pub fn hold_restricted(&self, fd: std::io::Result<std::os::fd::OwnedFd>) {
        if let Some(held) = lock(&self.held).as_mut() {
            match fd {
                Ok(fd) => {
                    held.restricted = Some(fd);
                    held.restricted_unavailable = None;
                }
                Err(e) => {
                    warn!(%e, "cannot keep the restricted listener for a handover; reload is refused until restart");
                    held.restricted_unavailable = Some(e.to_string());
                }
            }
        }
    }
}

impl Daemon {
    /// Stop writing and offer coordination to `successor_pid`. From here
    /// until [`Daemon::abort_transfer`] or the successor's accept, every
    /// mutating request answers `transferring` and every tick writer
    /// skips its turn; reads keep being served from memory.
    pub fn offer_transfer(&self, successor_pid: u32) -> Result<Transfer, Box<Response>> {
        lock(&self.state).offer_transfer(successor_pid)
    }

    /// Take authority back if the successor has not accepted. Returns
    /// whether this daemon is writing again.
    pub fn abort_transfer(&self, reason: &str) -> bool {
        lock(&self.state).abort_transfer(reason)
    }

    /// Whether this daemon is not the coordinator right now: an offer is
    /// open, or has been accepted by a successor.
    pub fn fenced(&self) -> bool {
        lock(&self.state).fenced()
    }

    /// Whether this daemon has ceded coordination for good.
    pub fn transferred(&self) -> bool {
        matches!(
            lock(&self.state).coordination,
            Coordination::Transferred { .. }
        )
    }

    /// What the store says about the current offer.
    pub fn transfer_state(&self) -> Option<Transfer> {
        lock(&self.state).transfer_state()
    }

    /// The successor's first act: accept the offer addressed to it, or
    /// learn it must not write. Called on a fresh `Daemon` opened over the
    /// same database before it serves anything.
    pub fn accept_transfer(&self, transfer: &str) -> Result<(), String> {
        let mut state = lock(&self.state);
        let now = Utc::now();
        let mut event = Event::new(
            EventKind::DaemonTransferAccepted {
                transfer: transfer.to_owned(),
            },
            now,
        );
        event.seq = state.next_seq;
        match state.store.settle_transfer(
            transfer,
            Some(std::process::id()),
            TransferState::Accepted,
            now,
            &event,
        ) {
            Ok(true) => {
                state.next_seq += 1;
                let _ = state.events.send(event);
                // Authority is ours: run the recovery a fenced open held
                // back, in the order it was decided, each taking its
                // sequence numbers above the acceptance's now, so the
                // durable head grows without a hole. Nobody subscribes to
                // this daemon before it serves, and a stop in between loses
                // nothing: the same recovery is derived again at the next
                // open.
                state.coordination = Coordination::Serving;
                let deferred = std::mem::take(&mut state.deferred_recovery);
                for write in deferred {
                    let seq = state.next_seq;
                    let mut used = 0;
                    if state.persist("deferred recovery", |store| {
                        used = write(store, seq)?;
                        Ok(())
                    }) == Persisted::Failed
                    {
                        // Authority is already ours and the predecessor
                        // will leave on our word: a refusal now would leave
                        // nobody serving. This daemon serves with storage
                        // disabled, as any storage failure leaves it, and
                        // the next open derives the same recovery again.
                        error!(
                            "deferred recovery write failed after acceptance; serving with storage disabled"
                        );
                        break;
                    }
                    state.next_seq += used;
                }
                Ok(())
            }
            Ok(false) => Err(format!(
                "transfer {transfer} is not offered to this process; refusing to write"
            )),
            Err(err) => Err(format!("cannot accept transfer {transfer}: {err}")),
        }
    }
}

#[cfg(unix)]
/// `candidate --build-info`, read once, within [`BUILD_INFO_WITHIN`]: the
/// bounded host runner ends the whole process group on the deadline, so
/// a candidate that hangs costs a reload, not a thread.
fn build_info(candidate: &std::path::Path, within: Duration) -> Result<BuildInfo, String> {
    let root = candidate
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(std::env::temp_dir);
    let argv = [candidate.display().to_string(), "--build-info".to_owned()];
    let output = agentdocker_host::command::run(&root, &argv, within)
        .map_err(|e| format!("cannot run the candidate for --build-info: {e}"))?;
    if !output.success {
        return Err(format!(
            "candidate failed --build-info: {}",
            output.text.trim()
        ));
    }
    serde_json::from_str(&output.stdout)
        .map_err(|e| format!("candidate build info unreadable: {e}"))
}

#[cfg(unix)]
/// End a successor that will not serve. It was started in its own
/// session, so its whole process group goes with it: anything it spawned
/// while fenced never had authority and must not outlive the attempt.
fn kill_child(mut child: std::process::Child) -> std::io::Result<()> {
    if let Ok(pid) = i32::try_from(child.id()) {
        let _ =
            nix::sys::signal::killpg(nix::unistd::Pid::from_raw(pid), nix::sys::signal::SIGKILL);
    }
    child.kill()?;
    child.wait().map(|_| ())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::{Read, Seek, Write};
    use std::os::fd::{AsFd, AsRawFd, FromRawFd};

    #[tokio::test(flavor = "current_thread")]
    async fn unread_readiness_reply_does_not_block_async_progress() {
        let (successor, predecessor) = UnixStream::pair().unwrap();
        nix::sys::socket::setsockopt(&successor, nix::sys::socket::sockopt::SndBuf, &4096).unwrap();
        // Leave room for the descriptor header. Filling the socket first
        // makes macOS reject sendmsg's control message immediately instead of
        // blocking. A large failure body exercises the actual blocking write
        // while the predecessor retains its socket without reading anything.
        let writer = tokio::spawn(answer_async(
            successor,
            Ready::Failed {
                reason: "r".repeat(256 * 1024),
            },
        ));
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(
            !writer.is_finished(),
            "the predecessor has not read the reply"
        );
        drop(predecessor);
        let result = tokio::time::timeout(Duration::from_secs(2), writer)
            .await
            .expect("readiness worker did not finish after peer closed")
            .expect("readiness task panicked");
        assert!(result.is_err());
    }

    /// A stand-in for the daemon lock, which every handover names at
    /// index 1.
    fn lock_stand_in() -> std::fs::File {
        std::fs::File::open("/dev/null").unwrap()
    }

    fn handover() -> Handover {
        Handover {
            format: FORMAT,
            transfer: "t".into(),
            listener: 0,
            lock: 1,
            restricted: None,
            home: PathBuf::from("/tmp/h"),
            socket: PathBuf::from("/tmp/h/agentd.sock"),
            terminals: Vec::new(),
            adopted: Vec::new(),
        }
    }

    /// What arrives is the same *open file description*, not another
    /// handle on the same path.
    ///
    /// This is the property the whole upgrade rests on, and reading the
    /// same bytes does not prove it — two independent opens of one path
    /// read the same bytes too. What only a shared description gives is
    /// a shared file offset, so this seeks on one side and reads on the
    /// other, which cannot work unless the descriptor was duplicated
    /// rather than reopened. A pty master has no path to reopen at all,
    /// so anything weaker would not be testing the thing that matters.
    #[test]
    fn what_arrives_shares_the_sender_s_file_offset() {
        let (mine, theirs) = UnixStream::pair().unwrap();
        let path = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(path.path(), b"0123456789").unwrap();
        let carried = std::fs::File::open(path.path()).unwrap();
        let lock = lock_stand_in();

        offer(&mine, &handover(), &[carried.as_fd(), lock.as_fd()]).unwrap();
        let (received, fds) = accept(&theirs).unwrap();
        assert_eq!(received, handover());
        assert_eq!(fds.len(), 2);
        let mut arrived = std::fs::File::from(fds.into_iter().next().unwrap());

        // Move the offset using the sender's handle. A reopened file
        // would start at zero however this one was moved.
        (&carried).seek(std::io::SeekFrom::Start(6)).unwrap();
        let mut said = String::new();
        arrived.read_to_string(&mut said).unwrap();
        assert_eq!(
            said, "6789",
            "the descriptor was duplicated, not reopened: the offset is shared"
        );

        // And the other way, so this is one description rather than two
        // that happened to agree once.
        arrived.seek(std::io::SeekFrom::Start(2)).unwrap();
        let mut also = String::new();
        (&carried).read_to_string(&mut also).unwrap();
        assert_eq!(also, "23456789");
    }

    /// A map that gives one descriptor to two owners is refused.
    #[test]
    fn a_handover_that_double_books_a_descriptor_is_refused() {
        let opened = |n: usize| {
            (0..n)
                .map(|_| std::fs::File::open("/dev/null").unwrap())
                .collect::<Vec<_>>()
        };

        // The listener claimed as a terminal as well.
        let files = opened(2);
        let borrowed: Vec<_> = files.iter().map(AsFd::as_fd).collect();
        let mut clash = handover();
        clash.terminals.push(Terminal {
            agent: AgentId::from("abc"),
            fd: 0,
        });
        let (mine, theirs) = UnixStream::pair().unwrap();
        offer(&mine, &clash, &borrowed).unwrap();
        let refused = accept(&theirs).unwrap_err();
        assert!(
            refused.to_string().contains("more than one owner"),
            "{refused}"
        );

        // One agent given two terminals.
        let files = opened(4);
        let borrowed: Vec<_> = files.iter().map(AsFd::as_fd).collect();
        let mut twice = handover();
        twice.terminals = vec![
            Terminal {
                agent: AgentId::from("abc"),
                fd: 2,
            },
            Terminal {
                agent: AgentId::from("abc"),
                fd: 3,
            },
        ];
        let (mine, theirs) = UnixStream::pair().unwrap();
        offer(&mine, &twice, &borrowed).unwrap();
        let refused = accept(&theirs).unwrap_err();
        assert!(
            refused.to_string().contains("more than one terminal"),
            "{refused}"
        );
    }

    /// A process handed over without a birth time is refused: it cannot
    /// be told from whatever now holds that pid.
    #[test]
    fn an_adopted_process_with_no_birth_time_is_refused() {
        let (mine, theirs) = UnixStream::pair().unwrap();
        let carried = std::fs::File::open("/dev/null").unwrap();
        let lock = lock_stand_in();
        let mut vague = handover();
        vague.adopted.push(Adopted {
            agent: AgentId::from("abc"),
            pid: 4242,
            started_at: None,
        });
        offer(&mine, &vague, &[carried.as_fd(), lock.as_fd()]).unwrap();
        let refused = accept(&theirs).unwrap_err();
        assert!(refused.to_string().contains("recycled one"), "{refused}");
    }

    /// A deadline of zero is refused rather than turned into for ever.
    #[test]
    fn a_zero_deadline_is_refused_rather_than_waiting_for_ever() {
        let (mine, theirs) = UnixStream::pair().unwrap();
        let reason = await_ready(&mine, Duration::ZERO).unwrap_err();
        assert!(reason.contains("never expire"), "{reason}");
        drop(theirs);
    }

    #[test]
    fn a_trickling_readiness_reply_cannot_extend_the_deadline() {
        use std::io::Write;
        let (mine, mut theirs) = UnixStream::pair().unwrap();
        let sender = std::thread::spawn(move || {
            let payload = serde_json::to_vec(&Ready::Serving).unwrap();
            theirs
                .write_all(&(payload.len() as u32).to_be_bytes())
                .unwrap();
            for byte in payload {
                if theirs.write_all(&[byte]).is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(30));
            }
        });
        let started = Instant::now();
        let result = await_ready(&mine, Duration::from_millis(100));
        let elapsed = started.elapsed();
        drop(mine);
        sender.join().unwrap();
        assert!(result.unwrap_err().contains("within"));
        assert!(
            elapsed < Duration::from_millis(400),
            "trickle extended the deadline: {elapsed:?}"
        );
    }

    /// A readiness answer carrying descriptors is refused.
    #[test]
    fn a_readiness_answer_never_carries_descriptors() {
        let (mine, theirs) = UnixStream::pair().unwrap();
        let carried = std::fs::File::open("/dev/null").unwrap();
        let payload = serde_json::to_vec(&Ready::Serving).unwrap();
        handoff::send(&theirs, &payload, &[carried.as_fd()]).unwrap();
        let reason = await_ready(&mine, Duration::from_millis(200)).unwrap_err();
        assert!(reason.contains("never does"), "{reason}");
    }

    /// A listening socket crosses still bound, and still accepts.
    ///
    /// The reason for carrying it rather than rebinding: between an
    /// unbind and a rebind the path is not listening, and a client that
    /// connects in that window is refused instead of waiting. Carrying
    /// the descriptor has no such window — here the predecessor's
    /// listener is dropped entirely and the path stays up.
    #[test]
    fn a_listening_socket_crosses_still_bound_and_still_accepting() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("sock");
        let listening = std::os::unix::net::UnixListener::bind(&path).unwrap();

        let (mine, theirs) = UnixStream::pair().unwrap();
        let lock = lock_stand_in();
        offer(&mine, &handover(), &[listening.as_fd(), lock.as_fd()]).unwrap();
        let (_, fds) = accept(&theirs).unwrap();

        // The predecessor lets go, as it would on the way out.
        drop(listening);
        let successor = {
            let fd = fds.into_iter().next().unwrap();
            let raw = fd.as_raw_fd();
            std::mem::forget(fd);
            // SAFETY: the descriptor arrived from `accept` and nothing
            // else owns it after the forget above.
            unsafe { std::os::unix::net::UnixListener::from_raw_fd(raw) }
        };
        let client = UnixStream::connect(&path).expect("still listening after the handover");
        let (mut served, _) = successor.accept().unwrap();
        served.write_all(b"hello").unwrap();
        let mut heard = [0_u8; 5];
        (&client).read_exact(&mut heard).unwrap();
        assert_eq!(&heard, b"hello");
    }

    /// Nothing is given up until the successor says it is serving.
    ///
    /// The previous attempt reported success as soon as the descriptors
    /// were sent. The successor might still have been failing to start,
    /// and by then the predecessor had gone — taking its children with
    /// it. Every answer other than "serving" has to leave the
    /// predecessor exactly where it was.
    #[test]
    fn only_serving_authorises_the_predecessor_to_go() {
        for (answered, expected) in [
            (Some(Ready::Serving), ""),
            (
                Some(Ready::Failed {
                    reason: "database is locked".into(),
                }),
                "refused to take over",
            ),
            (None, "said nothing"),
        ] {
            let (mine, theirs) = UnixStream::pair().unwrap();
            match &answered {
                Some(ready) => answer(&theirs, ready).unwrap(),
                // A successor that died before saying anything.
                None => drop(theirs),
            }
            let outcome = await_ready(&mine, Duration::from_millis(200));
            match answered {
                Some(Ready::Serving) => assert!(outcome.is_ok(), "{outcome:?}"),
                _ => {
                    let reason = outcome.unwrap_err();
                    assert!(reason.contains(expected), "{reason}");
                }
            }
        }
    }

    /// A silent successor hits a deadline rather than hanging.
    ///
    /// The predecessor is still the one serving, and it has stopped
    /// doing anything else in order to wait.
    #[test]
    fn a_silent_successor_hits_a_deadline() {
        let (mine, theirs) = UnixStream::pair().unwrap();
        let started = Instant::now();
        let reason = await_ready(&mine, Duration::from_millis(150)).unwrap_err();
        assert!(reason.contains("did not say it was serving"), "{reason}");
        assert!(started.elapsed() < Duration::from_secs(5));
        drop(theirs);
    }

    /// A handover naming a descriptor it did not send is refused.
    ///
    /// Taking it would mean serving while holding a terminal that is not
    /// there, and the agent on the other end would be attached to
    /// nothing with nobody saying so.
    #[test]
    fn a_handover_naming_a_descriptor_that_did_not_arrive_is_refused() {
        let (mine, theirs) = UnixStream::pair().unwrap();
        let file = tempfile::NamedTempFile::new().unwrap();
        let carried = std::fs::File::open(file.path()).unwrap();
        let lock = lock_stand_in();
        let mut lying = handover();
        lying.terminals.push(Terminal {
            agent: AgentId::from("abc"),
            fd: 7,
        });
        offer(&mine, &lying, &[carried.as_fd(), lock.as_fd()]).unwrap();
        let refused = accept(&theirs).unwrap_err();
        assert!(refused.to_string().contains("only 2 arrived"), "{refused}");
    }

    /// A handover that names no lock — the shape before session owners —
    /// is refused: a successor that does not hold the lock leaves a
    /// window in which an autostart finds it vacant.
    #[test]
    fn a_handover_without_the_lock_is_refused() {
        let (mine, theirs) = UnixStream::pair().unwrap();
        let carried = std::fs::File::open("/dev/null").unwrap();
        offer(&mine, &handover(), &[carried.as_fd()]).unwrap();
        let refused = accept(&theirs).unwrap_err();
        assert!(refused.to_string().contains("only 1 arrived"), "{refused}");
    }

    /// The restricted endpoint's listener travels as a third descriptor
    /// when the predecessor had it up, and its index is checked like the
    /// others.
    #[test]
    fn the_restricted_listener_travels_when_named() {
        let dir = tempfile::TempDir::new().unwrap();
        let listening = std::os::unix::net::UnixListener::bind(dir.path().join("host")).unwrap();
        let restricted =
            std::os::unix::net::UnixListener::bind(dir.path().join("container")).unwrap();
        let lock = lock_stand_in();
        let mut with_restricted = handover();
        with_restricted.restricted = Some(2);

        let (mine, theirs) = UnixStream::pair().unwrap();
        offer(
            &mine,
            &with_restricted,
            &[listening.as_fd(), lock.as_fd(), restricted.as_fd()],
        )
        .unwrap();
        let (received, fds) = accept(&theirs).unwrap();
        assert_eq!(received.restricted, Some(2));
        assert_eq!(fds.len(), 3);
        // Order is the only thing the descriptor list preserves, so the
        // third one must be the container endpoint and nothing else.
        let arrived = std::os::unix::net::UnixListener::from(fds.into_iter().nth(2).unwrap());
        assert_eq!(
            arrived.local_addr().unwrap().as_pathname(),
            Some(dir.path().join("container").as_path())
        );

        // Naming it without sending it is the lie the index check catches.
        let (mine, theirs) = UnixStream::pair().unwrap();
        offer(&mine, &with_restricted, &[listening.as_fd(), lock.as_fd()]).unwrap();
        let refused = accept(&theirs).unwrap_err();
        assert!(refused.to_string().contains("only 2 arrived"), "{refused}");
    }

    /// A successor that hangs is ended with everything it started: it ran
    /// in its own session, and a child it spawned while fenced never had
    /// authority to outlive the attempt.
    #[test]
    fn kill_child_ends_the_successor_s_whole_session() {
        use std::os::unix::process::CommandExt;
        let dir = tempfile::TempDir::new().unwrap();
        let pidfile = dir.path().join("grandchild");
        let mut command = std::process::Command::new("sh");
        command
            .arg("-c")
            .arg("sleep 60 & echo $! > \"$1\"; wait")
            .arg("session")
            .arg(&pidfile)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        // SAFETY: `setsid` is async-signal-safe and touches nothing
        // shared with the parent.
        unsafe {
            command.pre_exec(|| {
                nix::libc::setsid();
                Ok(())
            });
        }
        let child = command.spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let grandchild = loop {
            if let Ok(text) = std::fs::read_to_string(&pidfile)
                && let Ok(pid) = text.trim().parse::<i32>()
            {
                break nix::unistd::Pid::from_raw(pid);
            }
            assert!(Instant::now() < deadline, "the fixture never forked");
            std::thread::sleep(Duration::from_millis(10));
        };
        assert!(nix::sys::signal::kill(grandchild, None).is_ok());

        kill_child(child).unwrap();

        // SIGKILL is delivered asynchronously; the orphan is reparented and
        // reaped by init within moments.
        let deadline = Instant::now() + Duration::from_secs(5);
        while nix::sys::signal::kill(grandchild, None).is_ok() {
            assert!(
                Instant::now() < deadline,
                "the successor's child outlived the kill"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn candidate_script(dir: &tempfile::TempDir, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.path().join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// A candidate that hangs on `--build-info` costs a reload, not a
    /// thread: the read is bounded and the whole process group is ended.
    #[test]
    fn a_candidate_that_hangs_on_build_info_is_ended_at_the_deadline() {
        let dir = tempfile::TempDir::new().unwrap();
        let hangs = candidate_script(&dir, "hangs", "sleep 30");
        let started = Instant::now();
        let reason = build_info(&hangs, Duration::from_millis(300)).unwrap_err();
        assert!(reason.contains("timed out"), "{reason}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    /// What a candidate says on `--build-info` is read as is; a refusal
    /// or nonsense is named rather than guessed at.
    #[test]
    fn build_info_reads_a_candidate_s_answer_and_names_a_bad_one() {
        let dir = tempfile::TempDir::new().unwrap();
        let good = candidate_script(
            &dir,
            "good",
            r#"printf '{"arch":"x","format":1,"os":"y","state_schema":7,"version":"0"}\n'"#,
        );
        let info = build_info(&good, Duration::from_secs(5)).unwrap();
        assert_eq!((info.format, info.state_schema), (1, 7));
        assert_eq!((info.os.as_str(), info.arch.as_str()), ("y", "x"));

        let fails = candidate_script(&dir, "fails", "echo nope >&2; exit 3");
        let reason = build_info(&fails, Duration::from_secs(5)).unwrap_err();
        assert!(
            reason.contains("failed --build-info") && reason.contains("nope"),
            "{reason}"
        );

        let babbles = candidate_script(&dir, "babbles", "echo hello");
        let reason = build_info(&babbles, Duration::from_secs(5)).unwrap_err();
        assert!(reason.contains("unreadable"), "{reason}");
    }

    /// A successor speaking a different format refuses rather than
    /// guessing what the descriptors mean.
    #[test]
    fn an_unknown_handover_format_is_refused() {
        let (mine, theirs) = UnixStream::pair().unwrap();
        let file = tempfile::NamedTempFile::new().unwrap();
        let carried = std::fs::File::open(file.path()).unwrap();
        let mut future = handover();
        future.format = FORMAT + 1;
        offer(&mine, &future, &[carried.as_fd()]).unwrap();
        let refused = accept(&theirs).unwrap_err();
        assert!(refused.to_string().contains("is not the"), "{refused}");
    }
}

#[cfg(test)]
mod fence_tests {
    use super::*;
    use agentdocker_core::{AgentSpec, LeaseMode};
    use tempfile::TempDir;

    fn open(dir: &TempDir) -> Arc<Daemon> {
        let home = dir.path().to_path_buf();
        Arc::new(Daemon::open(home.clone(), home.join("sock")).unwrap())
    }

    #[tokio::test]
    async fn a_new_agent_is_not_kept_when_its_first_write_is_failed_or_fenced() {
        for fenced in [false, true] {
            let dir = TempDir::new().unwrap();
            let daemon = open(&dir);
            if fenced {
                daemon.offer_transfer(1).unwrap();
            }
            let mut state = lock(&daemon.state);
            if !fenced {
                state.store.reject_writes_for_test();
            }
            let next_seq = state.next_seq;
            let mut events = state.events.subscribe();
            let record = agentdocker_core::AgentRecord::new(
                AgentSpec {
                    name: "uncommitted-agent".into(),
                    ..Default::default()
                },
                false,
                Utc::now(),
            );
            let id = record.id.clone();
            let response = state.insert_record(record);
            assert!(matches!(response, Response::Error { code, .. }
                if code == if fenced { ErrorCode::Transferring } else { ErrorCode::StorageUnavailable }));
            assert!(state.registry.get(&id).is_none());
            assert!(
                !state
                    .store
                    .load_agents()
                    .unwrap()
                    .iter()
                    .any(|record| record.id == id)
            );
            assert_eq!(state.next_seq, next_seq);
            assert!(events.try_recv().is_err());
        }
    }

    #[test]
    fn reload_waits_for_restricted_registration_without_losing_descriptors() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let descriptor = || -> OwnedFd { std::fs::File::open("/dev/null").unwrap().into() };
        daemon.hold(Held {
            listener: descriptor(),
            lock: descriptor(),
            restricted: None,
            restricted_unavailable: None,
        });
        assert!(matches!(
            *daemon.take_held().err().unwrap(),
            Response::Error {
                code: ErrorCode::Backpressure,
                ..
            }
        ));
        assert!(lock(&daemon.held).is_some());
        daemon.hold_restricted(Err(io::Error::other("bind failed")));
        assert!(matches!(
            *daemon.take_held().err().unwrap(),
            Response::Error {
                code: ErrorCode::Unavailable,
                ..
            }
        ));
        assert!(lock(&daemon.held).is_some());
        let restricted = descriptor();
        let raw = restricted.as_raw_fd();
        daemon.hold_restricted(Ok(restricted));
        let held = daemon.take_held().unwrap();
        assert_eq!(held.restricted.unwrap().as_raw_fd(), raw);
        assert!(held.restricted_unavailable.is_none());
        assert!(lock(&daemon.held).is_none());
    }

    async fn register(daemon: &Arc<Daemon>, name: &str) -> AgentId {
        match daemon
            .handle(Request::Register {
                spec: AgentSpec {
                    name: name.into(),
                    ..Default::default()
                },
                pid: None,
                session: None,
            })
            .await
        {
            Response::Agent { agent } => agent.id,
            other => panic!("{other:?}"),
        }
    }

    fn claim(agent: &AgentId, resource: &str) -> Request {
        Request::Claim {
            agent: agent.to_string(),
            resource: resource.into(),
            mode: LeaseMode::Exclusive,
            amount: None,
            ttl_secs: 60,
            note: None,
            wait_secs: 0,
            automatic: false,
        }
    }

    fn transfer_events(daemon: &Arc<Daemon>) -> Vec<String> {
        daemon
            .recent_events(100)
            .into_iter()
            .filter_map(|e| match e.kind {
                EventKind::DaemonTransferOffered { .. } => Some("offered".to_owned()),
                EventKind::DaemonTransferReaddressed { successor_pid, .. } => {
                    Some(format!("readdressed to {successor_pid}"))
                }
                EventKind::DaemonTransferAccepted { .. } => Some("accepted".to_owned()),
                EventKind::DaemonTransferAborted { .. } => Some("aborted".to_owned()),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn supervised_stop_commits_before_memory_events_or_signals() {
        for mode in ["fenced", "failed", "committed"] {
            let dir = TempDir::new().unwrap();
            let daemon = open(&dir);
            let mut agent = AgentRecord::new(AgentSpec::default(), true, Utc::now());
            agent.status = AgentStatus::Running;
            let (control, receiver) = tokio::sync::watch::channel(None);
            {
                let mut state = lock(&daemon.state);
                agent = match state.insert_record(agent.clone()) {
                    Response::Agent { agent } => agent,
                    other => panic!("{other:?}"),
                };
                state.supervised.insert(agent.id.clone(), control);
            }
            if mode == "fenced" {
                daemon.offer_transfer(4242).unwrap();
            } else if mode == "failed" {
                lock(&daemon.state)
                    .store
                    .reject_event_for_test("agent_stopping");
            }
            let mut events = daemon.subscribe_events();
            let response = daemon.stop(agent.id.as_str(), true);
            let state = lock(&daemon.state);
            let durable = state.store.load_agents().unwrap().pop().unwrap();
            let current = state.registry.get(&agent.id).unwrap();
            if mode == "committed" {
                assert!(matches!(response, Response::Agent { .. }));
                assert_eq!(*receiver.borrow(), Some(true));
                assert_eq!(durable.status, AgentStatus::Stopping);
                assert_eq!(*current, durable);
                assert!(matches!(
                    events.try_recv().unwrap().kind,
                    EventKind::AgentStopping { force: true, .. }
                ));
            } else {
                let expected = if mode == "fenced" {
                    ErrorCode::Transferring
                } else {
                    ErrorCode::StorageUnavailable
                };
                assert!(matches!(response, Response::Error { code, .. } if code == expected));
                assert_eq!(*receiver.borrow(), None);
                assert_eq!(*current, agent);
                assert_eq!(durable, agent);
                assert!(events.try_recv().is_err());
            }
        }
    }

    #[tokio::test]
    async fn failed_stop_commit_does_not_signal_a_verified_external_process() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let mut child = tokio::process::Command::new("sleep")
            .arg("30")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let pid = child.id().unwrap();
        let mut agent = AgentRecord::new(
            AgentSpec {
                name: "external-stop-fixture".into(),
                ..Default::default()
            },
            false,
            Utc::now(),
        );
        agent.status = AgentStatus::Running;
        agent.pid = Some(pid);
        agent.process_started_at = Some(procinfo::start_time(pid).expect("owned process birth"));
        {
            let mut state = lock(&daemon.state);
            agent = match state.insert_record(agent) {
                Response::Agent { agent } => agent,
                other => panic!("{other:?}"),
            };
            state.store.reject_event_for_test("agent_stopping");
        }
        let response = daemon.stop(agent.id.as_str(), true);
        let still_running = child.try_wait().unwrap().is_none();
        // Clean up our child before assertions, including the before-fix failure.
        let _ = child.start_kill();
        child.wait().await.unwrap();
        assert!(matches!(
            response,
            Response::Error {
                code: ErrorCode::StorageUnavailable,
                ..
            }
        ));
        assert!(still_running, "a refused stop must not signal the process");
        let state = lock(&daemon.state);
        assert_eq!(state.registry.get(&agent.id).unwrap(), &agent);
        assert_eq!(state.store.load_agents().unwrap(), vec![agent]);
    }

    #[tokio::test]
    async fn failed_policy_clearing_never_requests_a_supervised_stop() {
        for restore in [true, false] {
            let dir = TempDir::new().unwrap();
            let daemon = open(&dir);
            let mut agent = AgentRecord::new(
                AgentSpec {
                    restore,
                    restart: if restore {
                        agentdocker_core::RestartPolicy::No
                    } else {
                        agentdocker_core::RestartPolicy::OnFailure { max: 2 }
                    },
                    ..Default::default()
                },
                true,
                Utc::now(),
            );
            agent.status = AgentStatus::Running;
            let (control, receiver) = tokio::sync::watch::channel(None);
            {
                let mut state = lock(&daemon.state);
                agent = match state.insert_record(agent.clone()) {
                    Response::Agent { agent } => agent,
                    other => panic!("{other:?}"),
                };
                state.supervised.insert(agent.id.clone(), control);
                state.store.reject_event_for_test(if restore {
                    "agent_restore_cleared"
                } else {
                    "agent_restart_cleared"
                });
            }
            let mut events = daemon.subscribe_events();
            let response = daemon.stop_agent(agent.id.as_str(), false).await;
            assert!(matches!(
                response,
                Response::Error {
                    code: ErrorCode::StorageUnavailable,
                    ..
                }
            ));
            assert_eq!(*receiver.borrow(), None);
            assert!(events.try_recv().is_err());
            let state = lock(&daemon.state);
            assert_eq!(state.registry.get(&agent.id).unwrap(), &agent);
            assert_eq!(state.store.load_agents().unwrap(), vec![agent]);
        }
    }

    #[tokio::test]
    async fn a_transfer_preserves_retained_history_until_it_is_aborted() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        // Large enough to actually delete rows if either reaper bypasses the fence.
        // Payloads are unused: this test observes the retained rows, not event decoding.
        let conn = crate::sqlite_fixture::open(dir.path().join("state.db")).unwrap();
        conn.execute("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x < ?1) INSERT INTO events(seq, at, json) SELECT x, '2026-09-15T00:00:00Z', '{}' FROM n", [EVENT_HISTORY as i64 + 3]).unwrap();
        conn.execute("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x < ?1) INSERT INTO changes(project, path, at, json) SELECT 'fixture', 'fixture', '2026-09-15T00:00:00Z', '{}' FROM n", [CHANGE_HISTORY as i64 + 3]).unwrap();
        lock(&daemon.state).next_seq = EVENT_HISTORY as u64 + 4;
        daemon.offer_transfer(4242).unwrap();
        let count = |table: &str| {
            conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap()
        };
        let before = (count("events"), count("changes"));
        daemon.prune_events();
        daemon.prune_changes();
        assert_eq!((count("events"), count("changes")), before);
        assert!(daemon.abort_transfer("retention trial"));
        daemon.prune_events();
        daemon.prune_changes();
        assert_eq!(count("events"), EVENT_HISTORY as i64);
        assert_eq!(count("changes"), CHANGE_HISTORY as i64);
    }

    /// While an offer is open: mutations are refused with `transferring`
    /// and leave nothing behind, reads still answer from memory, tick
    /// writers skip, and aborting resumes everything.
    #[tokio::test]
    async fn an_open_offer_fences_writes_but_not_reads() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let a = register(&daemon, "a").await;
        assert!(matches!(
            daemon.handle(claim(&a, "task:before")).await,
            Response::Lease { .. }
        ));
        let seq_before = daemon.recent_events(1)[0].seq;

        let transfer = daemon.offer_transfer(4242).expect("offered");
        assert_eq!(transfer.state, TransferState::Offered);
        assert_eq!(transfer_events(&daemon), ["offered"]);

        // A mutation is refused, and refused before anything was applied.
        let refused = daemon.handle(claim(&a, "task:during")).await;
        assert!(
            matches!(
                &refused,
                Response::Error {
                    code: ErrorCode::Transferring,
                    ..
                }
            ),
            "{refused:?}"
        );
        let Response::Leases { leases } = daemon
            .handle(Request::Leases {
                agent: None,
                resource: None,
            })
            .await
        else {
            panic!()
        };
        assert_eq!(leases.len(), 1, "the refused claim left no lease");
        assert!(matches!(
            daemon
                .handle(Request::Register {
                    spec: AgentSpec {
                        name: "b".into(),
                        ..Default::default()
                    },
                    pid: None,
                    session: None
                })
                .await,
            Response::Error {
                code: ErrorCode::Transferring,
                ..
            }
        ));
        // Reads are served.
        assert!(matches!(
            daemon.handle(Request::Ping).await,
            Response::Pong { .. }
        ));
        assert!(matches!(
            daemon
                .handle(Request::Inspect {
                    agent: a.to_string()
                })
                .await,
            Response::Agent { .. }
        ));
        assert!(matches!(
            daemon
                .handle(Request::List {
                    all: true,
                    project: None,
                    labels: Default::default()
                })
                .await,
            Response::Agents { .. }
        ));
        // Tick writers write nothing: the only event since the offer is the offer.
        daemon.prune_events();
        daemon.expire_leases();
        daemon.check_liveness();
        let latest = daemon.recent_events(1)[0].seq;
        assert_eq!(
            latest,
            seq_before + 1,
            "only the offer event landed while fenced"
        );

        // Abort: authority returns, writes land again.
        assert!(daemon.abort_transfer("test"));
        assert_eq!(transfer_events(&daemon), ["offered", "aborted"]);
        assert!(matches!(
            daemon.handle(claim(&a, "task:after")).await,
            Response::Lease { .. }
        ));
        assert_eq!(
            daemon.transfer_state().unwrap().state,
            TransferState::Aborted
        );
    }

    /// The successor's accept and the predecessor's abort are one
    /// compare-and-set: whichever lands first wins and the other learns it.
    #[tokio::test]
    async fn accept_and_abort_race_through_the_store() {
        // Accept first: the predecessor cannot take authority back.
        let dir = TempDir::new().unwrap();
        let predecessor = open(&dir);
        let transfer = predecessor.offer_transfer(std::process::id()).unwrap();
        let successor =
            Arc::new(Daemon::open(dir.path().to_path_buf(), dir.path().join("sock2")).unwrap());
        successor
            .accept_transfer(&transfer.id)
            .expect("offered to this pid");
        assert!(
            !predecessor.abort_transfer("too late"),
            "accepted first: abort must fail"
        );
        assert!(predecessor.transferred());
        assert!(
            matches!(
                predecessor
                    .handle(Request::Register {
                        spec: AgentSpec {
                            name: "x".into(),
                            ..Default::default()
                        },
                        pid: None,
                        session: None
                    })
                    .await,
                Response::Error {
                    code: ErrorCode::Transferring,
                    ..
                }
            ),
            "a transferred predecessor never writes again"
        );
        assert_eq!(
            predecessor.transfer_state().unwrap().state,
            TransferState::Accepted
        );
        // The successor writes normally.
        register(&successor, "on-successor").await;
        drop(successor);

        // Abort first: the successor must not write.
        let dir = TempDir::new().unwrap();
        let predecessor = open(&dir);
        let transfer = predecessor.offer_transfer(std::process::id()).unwrap();
        assert!(predecessor.abort_transfer("changed my mind"));
        let successor =
            Arc::new(Daemon::open(dir.path().to_path_buf(), dir.path().join("sock2")).unwrap());
        let refused = successor.accept_transfer(&transfer.id).unwrap_err();
        assert!(refused.contains("not offered to this process"), "{refused}");
    }

    /// An offer names its successor; another process cannot accept it,
    /// and a second offer cannot be opened over an open one.
    #[tokio::test]
    async fn an_offer_is_addressed_and_exclusive() {
        let dir = TempDir::new().unwrap();
        let predecessor = open(&dir);
        let transfer = predecessor.offer_transfer(1).unwrap();
        let stranger =
            Arc::new(Daemon::open(dir.path().to_path_buf(), dir.path().join("sock2")).unwrap());
        assert!(
            stranger.accept_transfer(&transfer.id).is_err(),
            "offered to pid 1, not to this process"
        );
        assert!(stranger.accept_transfer("no-such-transfer").is_err());
        // Readdressing names the process that was actually started, still
        // offered, with its own event in the same transaction; a
        // successor named earlier can no longer accept.
        assert!(lock(&predecessor.state).readdress_offer(&transfer.id, 7));
        assert_eq!(
            predecessor
                .transfer_state()
                .map(|t| (t.successor_pid, t.state)),
            Some((Some(7), TransferState::Offered))
        );
        assert!(
            transfer_events(&predecessor).contains(&"readdressed to 7".to_owned()),
            "{:?}",
            transfer_events(&predecessor)
        );
        assert!(
            !lock(&predecessor.state).readdress_offer("no-such-transfer", 8),
            "only the open offer can be readdressed"
        );
        let second = predecessor.offer_transfer(2).unwrap_err();
        assert!(
            matches!(
                *second,
                Response::Error {
                    code: ErrorCode::Conflict,
                    ..
                }
            ),
            "{second:?}"
        );
        assert!(predecessor.abort_transfer("cleanup"));
    }

    /// Opening a database whose transfer is still offered starts fenced:
    /// startup recovery corrects memory but writes nothing, a stranger
    /// never writes, and the named successor's accept runs the held-back
    /// recovery as its first act.
    #[tokio::test]
    async fn a_fenced_open_defers_recovery_until_the_successor_accepts() {
        let dir = TempDir::new().unwrap();
        let predecessor = open(&dir);
        let a = register(&predecessor, "holder").await;
        assert!(matches!(
            predecessor.handle(claim(&a, "task:held")).await,
            Response::Lease { .. }
        ));
        // Make the holder look dead on disk so startup recovery has a
        // write to make (dropping its lease), then offer to this pid.
        {
            let state = lock(&predecessor.state);
            let mut record = state.registry.get(&a).unwrap().clone();
            record.status = AgentStatus::Exited { code: Some(0) };
            state.store.upsert_agent(&record).unwrap();
        }
        let transfer = predecessor.offer_transfer(std::process::id()).unwrap();
        let events_before = predecessor.recent_events(1)[0].seq;

        // A successor opens: fenced, the lease is gone from memory but still
        // on disk, and no event was written.
        let successor =
            Arc::new(Daemon::open(dir.path().to_path_buf(), dir.path().join("sock2")).unwrap());
        assert!(lock(&successor.state).fenced(), "opened fenced");
        assert!(
            lock(&successor.state).leases.by_holder(&a).is_empty(),
            "memory corrected"
        );
        assert_eq!(
            lock(&successor.state).store.load_leases().unwrap().len(),
            1,
            "disk untouched"
        );
        assert!(matches!(
            successor.handle(claim(&a, "task:blocked")).await,
            Response::Error {
                code: ErrorCode::Transferring,
                ..
            }
        ));
        // Accept: the deferred lease drop lands, and writes work.
        successor.accept_transfer(&transfer.id).unwrap();
        assert!(!lock(&successor.state).fenced());
        assert!(
            lock(&successor.state)
                .store
                .load_leases()
                .unwrap()
                .is_empty(),
            "deferred recovery ran"
        );
        let after = successor.recent_events(10);
        let accepted = after
            .iter()
            .find(|e| matches!(e.kind, EventKind::DaemonTransferAccepted { .. }))
            .expect("acceptance recorded");
        let released = after
            .iter()
            .find(|e| matches!(e.kind, EventKind::LeaseReleased { .. }))
            .expect("deferred release recorded");
        // The acceptance is the first write and the held-back recovery
        // follows it: no sequence number below the head is missing, and
        // memory's next number is the head's successor.
        assert_eq!(accepted.seq, events_before + 1);
        assert_eq!(released.seq, events_before + 2);
        let seqs: Vec<u64> = after.iter().map(|e| e.seq).collect();
        assert!(
            seqs.windows(2).all(|w| w[1] == w[0] + 1),
            "contiguous: {seqs:?}"
        );
        // One guard at a time: two in one expression self-deadlock.
        {
            let state = lock(&successor.state);
            assert_eq!(state.next_seq, state.store.max_event_seq().unwrap() + 1);
        }
        register(&successor, "after-accept").await;
    }

    /// An offer must not overtake a mutation the gate already admitted.
    #[tokio::test]
    async fn an_offer_waits_for_admitted_mutations() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        // Simulate an admitted, still-executing mutation.
        lock(&daemon.state).in_flight = 1;
        let refused = daemon.offer_transfer(1).unwrap_err();
        assert!(
            matches!(
                *refused,
                Response::Error {
                    code: ErrorCode::Backpressure,
                    ..
                }
            ),
            "{refused:?}"
        );
        lock(&daemon.state).in_flight = 0;
        daemon
            .offer_transfer(1)
            .expect("offered once nothing is in flight");
        assert!(daemon.abort_transfer("cleanup"));
    }

    /// A fenced expiry tick changes nothing: the lease stays in memory and
    /// on disk together, and no event is published for a write that did
    /// not happen.
    /// A mutation whose writes are done and is only waiting — an `ask`
    /// for an answer, a `claim --wait` for a lease — gives up its
    /// in-flight place, so an offer need not wait hours with it. When the
    /// wait ends the waiter takes a place back before it writes again.
    #[tokio::test]
    async fn a_waiting_ask_or_claim_does_not_hold_up_an_offer() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let asker = register(&daemon, "asker").await;
        let answerer = register(&daemon, "answerer").await;
        let asking = tokio::spawn({
            let daemon = daemon.clone();
            let (from, to) = (asker.to_string(), answerer.to_string());
            async move {
                daemon
                    .handle(Request::Ask {
                        from,
                        to,
                        question: "is the offer held up?".into(),
                        timeout_secs: 30,
                    })
                    .await
            }
        });
        // Once the request is waiting, its place must be free.
        let settled = async |daemon: &Arc<Daemon>, waiting: fn(&State) -> bool| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while !waiting(&lock(&daemon.state)) {
                assert!(
                    std::time::Instant::now() < deadline,
                    "the request never started waiting"
                );
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            while lock(&daemon.state).in_flight > 0 {
                assert!(
                    std::time::Instant::now() < deadline,
                    "the waiter kept its place"
                );
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        };
        settled(&daemon, |state| !state.questions.is_empty()).await;
        let offered = daemon.offer_transfer(1);
        assert!(offered.is_ok(), "{offered:?}");
        assert!(daemon.abort_transfer("cleanup"));
        // The question is still open and still answerable.
        let question = match lock(&daemon.state).questions.keys().next() {
            Some(question) => question.clone(),
            None => panic!("the question vanished"),
        };
        let answered = daemon
            .handle(Request::Send {
                from: answerer.to_string(),
                to: asker.to_string(),
                kind: "answer".into(),
                payload: serde_json::json!({"text": "no"}),
                reply_to: Some(question),
                links: Vec::new(),
            })
            .await;
        assert!(matches!(answered, Response::Sent { .. }), "{answered:?}");
        assert!(matches!(
            asking.await.unwrap(),
            Response::Answer { text, .. } if text == "no"
        ));

        // A claim waiting behind a holder: the same, and it takes its
        // place back to claim once the holder lets go.
        let holder = register(&daemon, "holder").await;
        let waiter = register(&daemon, "waiter").await;
        assert!(matches!(
            daemon.handle(claim(&holder, "task:x")).await,
            Response::Lease { .. }
        ));
        let waiting = tokio::spawn({
            let daemon = daemon.clone();
            let waiter = waiter.to_string();
            async move {
                daemon
                    .handle(Request::Claim {
                        agent: waiter,
                        resource: "task:x".into(),
                        mode: LeaseMode::Exclusive,
                        ttl_secs: 60,
                        wait_secs: 30,
                        note: None,
                        amount: None,
                        automatic: false,
                    })
                    .await
            }
        });
        settled(&daemon, |state| !state.waiting.is_empty()).await;
        let offered = daemon.offer_transfer(1);
        assert!(offered.is_ok(), "{offered:?}");
        assert!(daemon.abort_transfer("cleanup"));
        let held = match lock(&daemon.state).leases.by_holder(&holder).first() {
            Some(lease) => lease.id.clone(),
            None => panic!("the holder lost its lease"),
        };
        let released = daemon
            .handle(Request::Release {
                agent: holder.to_string(),
                lease: held,
                summary: None,
                summary_source: Default::default(),
            })
            .await;
        assert!(!matches!(released, Response::Error { .. }), "{released:?}");
        assert!(matches!(waiting.await.unwrap(), Response::Lease { .. }));
        assert_eq!(lock(&daemon.state).in_flight, 0, "every place given back");
    }

    #[tokio::test]
    async fn a_fenced_expiry_tick_leaves_memory_and_disk_agreeing() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let a = register(&daemon, "expiring").await;
        let Response::Lease { lease } = daemon
            .handle(Request::Claim {
                agent: a.to_string(),
                resource: "task:short".into(),
                mode: LeaseMode::Exclusive,
                amount: None,
                ttl_secs: 1,
                note: None,
                wait_secs: 0,
                automatic: false,
            })
            .await
        else {
            panic!()
        };
        daemon.offer_transfer(1).unwrap();
        let seq = daemon.recent_events(1)[0].seq;
        // Well past expiry, but fenced.
        lock(&daemon.state).expire_leases_at(Utc::now() + chrono::Duration::seconds(60));
        {
            let state = lock(&daemon.state);
            assert_eq!(state.leases.by_holder(&a).len(), 1, "memory kept the lease");
            assert_eq!(
                state.store.load_leases().unwrap().len(),
                1,
                "disk kept the lease"
            );
        }
        assert_eq!(
            daemon.recent_events(1)[0].seq,
            seq,
            "no event for a skipped write"
        );
        assert!(daemon.abort_transfer("cleanup"));
        lock(&daemon.state).expire_leases_at(Utc::now() + chrono::Duration::seconds(60));
        assert!(
            lock(&daemon.state).leases.by_holder(&a).is_empty(),
            "expiry resumes after abort"
        );
        let _ = lease;
    }

    /// A mutation whose future is dropped mid-flight (the client hung up)
    /// still releases its place, so a later offer is not refused for ever.
    #[tokio::test]
    async fn a_cancelled_mutation_releases_its_in_flight_place() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        let a = match daemon
            .handle(Request::Register {
                spec: AgentSpec {
                    name: "validator".into(),
                    workdir: Some(work.clone()),
                    ..Default::default()
                },
                pid: None,
                session: None,
            })
            .await
        {
            Response::Agent { agent } => agent.id,
            other => panic!("{other:?}"),
        };
        // A validation is in flight for as long as its command runs, since
        // its writes come after; drop that future as the server does on
        // EOF.
        let validating = daemon.handle(Request::Validate {
            agent: a.to_string(),
            command: vec!["sh".into(), "-c".into(), "sleep 2".into()],
            timeout_secs: 30,
        });
        let mut validating = Box::pin(validating);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(200), &mut validating)
                .await
                .is_err(),
            "the validation is running"
        );
        assert_eq!(lock(&daemon.state).in_flight, 1);
        assert!(
            daemon.offer_transfer(1).is_err(),
            "an offer waits for the running mutation"
        );
        drop(validating);
        assert_eq!(
            lock(&daemon.state).in_flight,
            0,
            "the dropped request released its place"
        );
        daemon.offer_transfer(1).expect("nothing in flight");
        assert!(daemon.abort_transfer("cleanup"));
    }

    /// A background write skipped by the fence is reported through the
    /// same gate every handler already checks, and clears once a write
    /// lands again.
    #[tokio::test]
    async fn a_skipped_write_shows_as_transferring_until_writes_resume() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let a = register(&daemon, "exiting").await;
        daemon.offer_transfer(1).unwrap();
        // A supervised exit reaching mark_exited while fenced: nothing
        // lands and the record stays live in memory as on disk.
        let before = lock(&daemon.state).registry.get(&a).unwrap().status.clone();
        daemon.mark_exited(&a, AgentStatus::Exited { code: Some(0) });
        {
            let state = lock(&daemon.state);
            assert_eq!(
                state.registry.get(&a).unwrap().status,
                before,
                "memory unchanged"
            );
            assert!(matches!(
                state.write_failure(),
                Some(Response::Error {
                    code: ErrorCode::Transferring,
                    ..
                })
            ));
            assert!(
                state.storage_failure().is_none(),
                "a skip is not a storage failure"
            );
        }
        // Reads are served from the projection the skip left alone: the
        // latch refuses the next write, never a ping, a listing or an
        // inspection.
        assert!(matches!(
            daemon.handle(Request::Ping).await,
            Response::Pong { .. }
        ));
        assert!(matches!(
            daemon
                .handle(Request::Inspect {
                    agent: a.to_string()
                })
                .await,
            Response::Agent { .. }
        ));
        assert!(matches!(
            daemon
                .handle(Request::List {
                    all: false,
                    project: None,
                    labels: Default::default(),
                })
                .await,
            Response::Agents { .. }
        ));
        assert!(daemon.abort_transfer("cleanup"));
        assert!(
            lock(&daemon.state).write_failure().is_none(),
            "cleared by the abort"
        );
        daemon.mark_exited(&a, AgentStatus::Exited { code: Some(0) });
        assert_eq!(
            lock(&daemon.state).registry.get(&a).unwrap().status,
            AgentStatus::Exited { code: Some(0) }
        );
    }

    /// A store that has already failed has nothing trustworthy to hand
    /// over: the offer is refused with the storage error.
    #[tokio::test]
    async fn a_failed_store_cannot_offer() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        {
            let mut state = lock(&daemon.state);
            state.store.reject_writes_for_test();
            let a = state.registry.all().next().cloned();
            if let Some(a) = a {
                let _ = state.persist("poison", |store| store.upsert_agent(&a));
            } else {
                state.storage_error = Some("poisoned".into());
            }
        }
        let refused = daemon.offer_transfer(1).unwrap_err();
        assert!(
            matches!(
                *refused,
                Response::Error {
                    code: ErrorCode::StorageUnavailable,
                    ..
                }
            ),
            "{refused:?}"
        );
    }
}
