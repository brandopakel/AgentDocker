//! The supervisor's shape on Windows, where managed sessions are not yet
//! delivered: launching an agent under a session owner with a terminal
//! needs ConPTY, Job Object ownership of the child and an owner process
//! that survives the daemon, none of which is in place. Every entry point
//! the daemon calls exists here with the same signature and answers that
//! the session is unavailable, so the daemon, the registry, messaging and
//! leases run on Windows while the sessions row stays honest. Nothing here
//! guesses about a process: identity checks use the same pid + birth rule
//! as on Unix, through the host crate.

use std::collections::VecDeque;
use std::sync::Arc;

use agentdocker_core::session::{ExitReport, SessionOwner};
use agentdocker_core::{AgentId, AgentRecord, AgentStatus};
use tokio::sync::{broadcast, mpsc, watch};

use crate::daemon::Daemon;

/// Why nothing launches: said the same way everywhere.
pub const UNAVAILABLE: &str = "managed sessions are not available on Windows yet; register or adopt a process you run yourself";

/// How the daemon runs an owner. Windows has no owner process yet; the
/// variant exists so the daemon's record of its mode reads the same.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OwnerMode {
    Process(std::path::PathBuf),
    InProcess,
}

impl OwnerMode {
    pub fn detect() -> Self {
        Self::InProcess
    }
}

/// A launched agent's handle. Never produced on Windows; the type stands
/// so the daemon's launch path compiles unchanged.
pub struct Spawned {
    pub pid: u32,
    pub process_started_at: chrono::DateTime<chrono::Utc>,
    pub owner: SessionOwner,
    pub control: watch::Sender<Option<bool>>,
    pub session: Option<Session>,
}

impl Spawned {
    pub async fn activate(&mut self, _action: &str) -> anyhow::Result<()> {
        anyhow::bail!(UNAVAILABLE)
    }
}

/// A managed agent's terminal as the daemon presents it. Without a
/// session there is nothing to attach to; the shape is kept so `Attach`
/// and `Resize` answer through the same code as on Unix.
#[derive(Clone)]
pub struct Session {
    output: broadcast::WeakSender<Vec<u8>>,
    pub input: mpsc::Sender<Vec<u8>>,
    resize: mpsc::Sender<(u16, u16)>,
    scrollback: Arc<std::sync::Mutex<VecDeque<u8>>>,
}

impl Session {
    pub fn resize(&self, cols: u16, rows: u16) -> std::io::Result<()> {
        self.resize
            .try_send((cols, rows))
            .map_err(|_| std::io::Error::other("terminal is not accepting resizes"))
    }

    pub fn attach(&self) -> (Vec<u8>, broadcast::Receiver<Vec<u8>>) {
        let seen: Vec<u8> = self
            .scrollback
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .copied()
            .collect();
        let live = self
            .output
            .upgrade()
            .map(|output| output.subscribe())
            .unwrap_or_else(|| broadcast::channel(1).1);
        (seen, live)
    }
}

/// Launching is refused before anything is forked: the record stays as
/// the caller made it and the answer says why.
pub async fn spawn(_daemon: &Daemon, _record: &AgentRecord) -> anyhow::Result<Spawned> {
    anyhow::bail!(UNAVAILABLE)
}

/// One newline-terminated frame at a time, as on the owner wire. Shared
/// with the Unix supervisor's callers so a Windows daemon reads the same
/// framing when an owner exists one day.
#[allow(dead_code)] // the owner wire's framing, kept for the day an owner exists
pub(crate) async fn read_frame<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
    partial: &mut Vec<u8>,
    max: usize,
) -> anyhow::Result<Option<String>> {
    use tokio::io::AsyncBufReadExt;
    loop {
        if let Some(end) = partial.iter().position(|b| *b == b'\n') {
            let frame = partial.drain(..=end).collect::<Vec<u8>>();
            let text =
                String::from_utf8(frame).map_err(|_| anyhow::anyhow!("frame is not UTF-8"))?;
            return Ok(Some(text.trim_end_matches(['\n', '\r']).to_owned()));
        }
        if partial.len() > max {
            anyhow::bail!("frame exceeds {max} bytes");
        }
        let buffer = reader.fill_buf().await?;
        if buffer.is_empty() {
            if partial.is_empty() {
                return Ok(None);
            }
            anyhow::bail!("EOF inside a frame");
        }
        let take = buffer.len().min(max + 1 - partial.len().min(max));
        partial.extend_from_slice(&buffer[..take]);
        reader.consume(take);
    }
}

/// Whether the recorded owner process is still the one that was recorded:
/// same pid, same birth. A recycled pid is not an owner.
pub(crate) fn owner_alive(owner: &SessionOwner) -> bool {
    agentdocker_host::procinfo::start_time(owner.pid) == Some(owner.started_at)
}

/// A record with an owner can only have come from another platform's
/// state directory; nothing is reattached here and the daemon records the
/// owner as lost, as it does for any owner it cannot reach.
pub async fn reattach(_daemon: &Daemon, _record: &AgentRecord) -> anyhow::Result<Reattached> {
    anyhow::bail!(UNAVAILABLE)
}

#[allow(dead_code)] // the surface the daemon matches on; never produced here yet
pub enum Reattached {
    Running(Box<Spawned>),
    Exited(ExitReport),
}

pub(crate) async fn acknowledge_recovered_exit(_home: std::path::PathBuf, _report: ExitReport) {}

/// Nothing to relay: the task ends at once.
pub fn supervise(
    _daemon: Arc<Daemon>,
    _id: AgentId,
    _spawned: Spawned,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async {})
}

#[allow(dead_code)] // reports come only from owners, which do not exist here yet
pub(crate) fn describe_exit(report: &ExitReport) -> String {
    match (report.code, report.signal) {
        (Some(code), _) => format!("it ended with exit code {code}"),
        (None, Some(signal)) => format!("it was ended by signal {signal}"),
        (None, None) if report.log_flushed => {
            "the program could not be executed (is the path right and the tool installed?)"
                .to_owned()
        }
        (None, None) => "it could not be waited for".to_owned(),
    }
}

pub(crate) fn exit_status(report: &ExitReport) -> AgentStatus {
    match (report.code, report.signal) {
        (None, None) if !report.log_flushed => AgentStatus::Failed {
            reason: "the command could not be waited for".into(),
        },
        _ => AgentStatus::Exited { code: report.code },
    }
}

/// A process group is a Unix notion; on Windows a recorded group id is a
/// pid whose Job Object the daemon never made. Uncertainty keeps
/// protection: the group is reported as existing while the pid exists.
pub(crate) fn group_exists(group: u32) -> bool {
    group > 0 && agentdocker_host::procinfo::alive(group)
}
