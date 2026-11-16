//! Whether a daemon is at a socket, observed without starting, creating or
//! locking anything.
//!
//! An [`Observation`] keeps two facts apart. The endpoint is what the
//! socket did when asked `ping`: answered (a pong, or a refusal kept
//! whole), stayed silent, said something that is not an answer, was
//! absent, or could not be reached. The owner is what the daemon's lock
//! file records of the process that took it: a hint about that process,
//! never a read of the kernel lock, which only a daemon ever takes. The
//! lock stays the one authority on which daemon serves: a client that
//! finds nobody listening may spawn one, and that daemon's own lock attempt
//! decides.
//!
//! [`Observation::reading`] says what the two suggest, in words that claim
//! no more than was seen, and [`next`] is the one decision every starter
//! (the CLI's autostart, the desktop's) takes from it.

use std::io::{self, Read, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use agentdocker_core::protocol::{Request, Response};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The largest answer to `ping` read before it is called garbled: well
/// above the largest pong (every feature, the experimental switches, the
/// judge and the authority), so only a peer that is not a daemon reaches it.
pub const MAX_ANSWER: usize = 64 * 1024;
/// The largest owner record read; a larger one is not a daemon's.
const MAX_RECORD: u64 = 256;
/// How long one look waits for an answer, inside a caller's deadline.
pub const ANSWER_WITHIN: Duration = Duration::from_millis(500);

/// What a daemon writes into its lock file once it holds the lock: the
/// process that took it, and when that process started, so a reader can
/// tell it from a later process given the same pid.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerRecord {
    pub pid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
}

impl OwnerRecord {
    /// This process, as it would record itself.
    pub fn this_process() -> Self {
        let pid = std::process::id();
        Self {
            pid,
            started_at: crate::procinfo::start_time(pid),
        }
    }

    fn line(&self) -> String {
        serde_json::to_string(self).expect("an owner record serialises") + "\n"
    }
}

/// Record this process as the lock's owner, through the descriptor that
/// holds it, never by replacing the file. Best-effort: a failure leaves an
/// old, partial or empty record, which readers take as unknown, and
/// decides nothing.
pub fn publish(lock: &crate::lock::Lock) -> io::Result<()> {
    lock.write_contents(OwnerRecord::this_process().line().as_bytes())
}

/// [`publish`] through a bare descriptor that holds the lock, as a
/// successor holds the one handed to it. Writes through a duplicate, so the
/// caller's descriptor, and the lock it holds, are left as they were.
pub fn publish_fd(fd: std::os::fd::BorrowedFd<'_>) -> io::Result<()> {
    use std::io::{Seek, SeekFrom};
    let mut file = std::fs::File::from(fd.try_clone_to_owned()?);
    file.set_len(0)?;
    file.seek(SeekFrom::Start(0))?;
    file.write_all(OwnerRecord::this_process().line().as_bytes())?;
    file.sync_data()
}

/// What the lock file records of the process that took it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Owner {
    /// The recorded process is alive and is the one recorded: alive by its
    /// pid, and started when the record says. A hint, not a lock test.
    Live { pid: u32, started_at: DateTime<Utc> },
    /// The recorded process is positively gone: no process has its pid, or
    /// the one that has it started at another time.
    Gone { pid: u32 },
    /// No lock file: no daemon has taken the lock here.
    None,
    /// Nothing certain: no record (a daemon from before records were kept,
    /// or one between its lock and its record), a partial or unreadable
    /// one, or a live pid whose start cannot be read.
    Unknown { why: String },
}

/// Read a small file without blocking, following a link or creating
/// anything: `Ok(None)` when it is missing; `Err` when it is unreadable,
/// not a regular file (asked of the opened descriptor, so a FIFO put there
/// never holds the read), or larger than `max` bytes.
pub fn read_small(path: &Path, max: u64) -> Result<Option<Vec<u8>>, String> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
    };
    match file.metadata() {
        Ok(meta) if meta.is_file() => {}
        Ok(_) => return Err(format!("{} is not a regular file", path.display())),
        Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
    }
    let mut bytes = Vec::new();
    file.take(max + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    if bytes.len() as u64 > max {
        return Err(format!(
            "{} is larger than any record written there",
            path.display()
        ));
    }
    Ok(Some(bytes))
}

/// The owner record of the lock at `path`: `Ok(None)` when there is no
/// file or it is empty (no record was ever written: no evidence either
/// way), `Err` when it cannot be read or is not a record.
pub fn owner_record(path: &Path) -> Result<Option<OwnerRecord>, String> {
    let Some(bytes) = read_small(path, MAX_RECORD)? else {
        return Ok(None);
    };
    let text = String::from_utf8_lossy(&bytes);
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    serde_json::from_str::<OwnerRecord>(text)
        .map(Some)
        .map_err(|_| "the owner record cannot be read".to_owned())
}

/// Whether a recorded process is the one recorded, by its pid and birth.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Liveness {
    /// Alive, and started when recorded.
    Live,
    /// Positively gone: no process has the pid, or the one that has it
    /// started at another time.
    Gone,
    /// Nothing certain: alive with no birth recorded, or a birth that
    /// cannot be read, or whether the pid lives cannot be told.
    Unknown { why: String },
}

/// [`Liveness`] of the process recorded as `pid`, started at `started_at`.
/// No such process proves it gone even without a birth.
pub fn liveness(pid: u32, started_at: Option<DateTime<Utc>>) -> Liveness {
    match alive(pid) {
        Some(false) => Liveness::Gone,
        None => Liveness::Unknown {
            why: format!("whether pid {pid} lives cannot be told"),
        },
        Some(true) => match (started_at, crate::procinfo::start_time(pid)) {
            (Some(recorded), Some(now)) if recorded == now => Liveness::Live,
            // The pid was given to another process since.
            (Some(_), Some(_)) => Liveness::Gone,
            (None, _) => Liveness::Unknown {
                why: format!("pid {pid} is alive and no start time was recorded for it"),
            },
            (Some(_), None) => Liveness::Unknown {
                why: format!("pid {pid} is alive but when it started cannot be read"),
            },
        },
    }
}

/// Read the owner record of the lock at `path`, creating nothing.
pub fn owner(path: &Path) -> Owner {
    match owner_record(path) {
        Err(why) => Owner::Unknown { why },
        Ok(None) => match read_small(path, MAX_RECORD) {
            Ok(None) => Owner::None,
            _ => Owner::Unknown {
                why: "no owner record".to_owned(),
            },
        },
        Ok(Some(record)) => match liveness(record.pid, record.started_at) {
            Liveness::Live => Owner::Live {
                pid: record.pid,
                started_at: record.started_at.expect("live needs a recorded birth"),
            },
            Liveness::Gone => Owner::Gone { pid: record.pid },
            Liveness::Unknown { why } => Owner::Unknown { why },
        },
    }
}

/// Whether a process has this pid: `kill(pid, 0)`, which delivers nothing.
/// `None` when it cannot be told.
fn alive(pid: u32) -> Option<bool> {
    let pid = i32::try_from(pid).ok().filter(|pid| *pid > 0)?;
    // SAFETY: signal 0 checks for the process without sending anything.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return Some(true);
    }
    match io::Error::last_os_error().raw_os_error() {
        Some(libc::ESRCH) => Some(false),
        // Someone else's process: it exists.
        Some(libc::EPERM) => Some(true),
        _ => None,
    }
}

/// What the socket did when asked `ping`.
#[derive(Clone, Debug, PartialEq)]
pub enum Endpoint {
    /// A complete answer: a pong, or a refusal, kept whole.
    Answered(Answer),
    /// Connected, and nothing complete came back in time.
    Silent,
    /// Something came back that is not an answer to `ping`: the
    /// connection closed, the bytes are not JSON, the reply is another
    /// kind, or it is larger than any pong.
    Garbled { why: String },
    /// Nothing listens there (no socket, or a socket nobody accepts on).
    Absent,
    /// The socket could not be reached for another reason.
    Unreachable { error: String },
}

/// An answer to `ping`.
#[derive(Clone, Debug, PartialEq)]
pub enum Answer {
    /// The daemon is ready.
    Pong(Box<Response>),
    /// The daemon answered but refused: present, not ready. The code is
    /// kept as sent, so a word from a newer daemon survives.
    Refused {
        code: String,
        message: String,
        details: Option<serde_json::Value>,
    },
}

fn absent(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
    )
}

fn ping_line() -> String {
    let mut line =
        agentdocker_core::protocol::request_json(&Request::Ping).expect("ping serialises");
    line.push('\n');
    line
}

/// Classify one answer line.
fn classify(line: &[u8]) -> Endpoint {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(line) else {
        return Endpoint::Garbled {
            why: "the answer is not JSON".to_owned(),
        };
    };
    if value["type"] == "error" {
        return Endpoint::Answered(Answer::Refused {
            code: value["code"].as_str().unwrap_or("unknown").to_owned(),
            message: value["message"].as_str().unwrap_or_default().to_owned(),
            details: value.get("details").cloned().filter(|d| !d.is_null()),
        });
    }
    match serde_json::from_value::<Response>(value) {
        Ok(pong @ Response::Pong { .. }) => Endpoint::Answered(Answer::Pong(Box::new(pong))),
        Ok(_) => Endpoint::Garbled {
            why: "the answer is not an answer to ping".to_owned(),
        },
        Err(_) => Endpoint::Garbled {
            why: "the answer cannot be read".to_owned(),
        },
    }
}

/// The answer once its frame is complete or too large: a line of at most
/// [`MAX_ANSWER`] bytes, measured before it is read, so padding past the
/// bound never passes for a pong.
fn framed(answer: &[u8]) -> Option<Endpoint> {
    match answer.iter().position(|b| *b == b'\n') {
        Some(end) if end <= MAX_ANSWER => Some(classify(&answer[..end])),
        Some(_) => Some(Endpoint::Garbled {
            why: "the answer is larger than any pong".to_owned(),
        }),
        None if answer.len() > MAX_ANSWER => Some(Endpoint::Garbled {
            why: "the answer is larger than any pong".to_owned(),
        }),
        None => None,
    }
}

/// Read one line of at most [`MAX_ANSWER`] bytes from `read`, which returns
/// `Ok(0)` at the end of the stream; `Err(TimedOut)` when time runs out.
fn read_answer(mut read: impl FnMut(&mut [u8]) -> io::Result<usize>) -> Endpoint {
    let mut answer = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match read(&mut chunk) {
            Ok(0) => {
                return Endpoint::Garbled {
                    why: "the connection closed without an answer".to_owned(),
                };
            }
            Ok(n) => {
                answer.extend_from_slice(&chunk[..n]);
                if let Some(ended) = framed(&answer) {
                    return ended;
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                ) =>
            {
                return Endpoint::Silent;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => {
                return Endpoint::Garbled {
                    why: "the connection failed before an answer".to_owned(),
                };
            }
        }
    }
}

fn left(deadline: Instant) -> Option<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|left| !left.is_zero())
}

/// A listener whose queue is full, or a connect the deadline ran out on:
/// something is there and is not taking connections.
pub fn busy(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

fn set_flag(
    fd: std::os::fd::RawFd,
    get: libc::c_int,
    set: libc::c_int,
    flag: libc::c_int,
    on: bool,
) -> io::Result<()> {
    // SAFETY: fcntl reads and sets scalar flags on a descriptor we own.
    let flags = unsafe { libc::fcntl(fd, get) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    let flags = if on { flags | flag } else { flags & !flag };
    // SAFETY: as above.
    if unsafe { libc::fcntl(fd, set, flags) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Connect to the socket at `socket` without waiting past `deadline`. An
/// ordinary connect to a listener whose queue is full waits on Linux for
/// room, however long that takes; this one never waits past the deadline:
/// a full queue is `WouldBlock`, and the deadline `TimedOut` ([`busy`]).
pub fn connect_within(socket: &Path, deadline: Instant) -> io::Result<crate::ipc::BlockingStream> {
    use nix::errno::Errno;
    use nix::sys::socket::{UnixAddr, connect, getsockopt, sockopt::SocketError};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    let address = UnixAddr::new(socket).map_err(io::Error::from)?;
    // SAFETY: socket() takes only scalar arguments.
    let raw = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `raw` is a descriptor this function has just made and alone owns.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    set_flag(raw, libc::F_GETFD, libc::F_SETFD, libc::FD_CLOEXEC, true)?;
    set_flag(raw, libc::F_GETFL, libc::F_SETFL, libc::O_NONBLOCK, true)?;
    match connect(fd.as_raw_fd(), &address) {
        Ok(()) => {}
        Err(Errno::EINPROGRESS) => {
            loop {
                let Some(time) = left(deadline) else {
                    return Err(io::ErrorKind::TimedOut.into());
                };
                let mut wanted = libc::pollfd {
                    fd: raw,
                    events: libc::POLLOUT,
                    revents: 0,
                };
                let ms = i32::try_from(time.as_millis().max(1)).unwrap_or(i32::MAX);
                // SAFETY: one pollfd, owned here, for the length given.
                match unsafe { libc::poll(&mut wanted, 1, ms) } {
                    0 => return Err(io::ErrorKind::TimedOut.into()),
                    n if n > 0 => break,
                    _ => {
                        let error = io::Error::last_os_error();
                        if error.kind() != io::ErrorKind::Interrupted {
                            return Err(error);
                        }
                    }
                }
            }
            match getsockopt(&fd, SocketError) {
                Ok(0) => {}
                Ok(code) => return Err(io::Error::from_raw_os_error(code)),
                Err(errno) => return Err(io::Error::from(errno)),
            }
        }
        Err(Errno::EAGAIN) => {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "the listener's queue is full",
            ));
        }
        Err(errno) => return Err(io::Error::from(errno)),
    }
    set_flag(raw, libc::F_GETFL, libc::F_SETFL, libc::O_NONBLOCK, false)?;
    Ok(crate::ipc::BlockingStream::from(fd))
}

/// Ask the socket `ping`, every step within `deadline`. Blocking: for a
/// caller off any async executor.
pub fn endpoint_blocking(socket: &Path, deadline: Instant) -> Endpoint {
    let mut stream = match connect_within(socket, deadline) {
        Ok(stream) => stream,
        Err(error) if absent(&error) => return Endpoint::Absent,
        Err(error) if busy(&error) => return Endpoint::Silent,
        Err(error) => {
            return Endpoint::Unreachable {
                error: error.to_string(),
            };
        }
    };
    let Some(time) = left(deadline) else {
        return Endpoint::Silent;
    };
    if stream.set_write_timeout(Some(time)).is_err()
        || stream.write_all(ping_line().as_bytes()).is_err()
    {
        return Endpoint::Silent;
    }
    read_answer(|buf| {
        let Some(time) = left(deadline) else {
            return Err(io::ErrorKind::TimedOut.into());
        };
        stream.set_read_timeout(Some(time))?;
        stream.read(buf)
    })
}

/// [`endpoint_blocking`] for an async caller.
pub async fn endpoint(socket: &Path, deadline: Instant) -> Endpoint {
    use crate::ipc::Stream;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let Some(time) = left(deadline) else {
        return Endpoint::Silent;
    };
    let mut stream = match tokio::time::timeout(time, Stream::connect(socket)).await {
        Err(_) => return Endpoint::Silent,
        Ok(Err(error)) if absent(&error) => return Endpoint::Absent,
        // A full queue: something is there and not taking connections.
        Ok(Err(error)) if busy(&error) => return Endpoint::Silent,
        Ok(Err(error)) => {
            return Endpoint::Unreachable {
                error: error.to_string(),
            };
        }
        Ok(Ok(stream)) => stream,
    };
    let Some(time) = left(deadline) else {
        return Endpoint::Silent;
    };
    match tokio::time::timeout(time, stream.write_all(ping_line().as_bytes())).await {
        Ok(Ok(())) => {}
        _ => return Endpoint::Silent,
    }
    let mut answer = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let Some(time) = left(deadline) else {
            return Endpoint::Silent;
        };
        match tokio::time::timeout(time, stream.read(&mut chunk)).await {
            Err(_) => return Endpoint::Silent,
            Ok(Ok(0)) => {
                return Endpoint::Garbled {
                    why: "the connection closed without an answer".to_owned(),
                };
            }
            Ok(Ok(n)) => {
                answer.extend_from_slice(&chunk[..n]);
                if let Some(ended) = framed(&answer) {
                    return ended;
                }
            }
            Ok(Err(_)) => {
                return Endpoint::Garbled {
                    why: "the connection failed before an answer".to_owned(),
                };
            }
        }
    }
}

/// What a socket and its lock file showed, kept apart.
#[derive(Clone, Debug, PartialEq)]
pub struct Observation {
    pub endpoint: Endpoint,
    pub owner: Owner,
}

/// Observe the daemon at `socket`, whose lock is `lock`, within `deadline`,
/// creating nothing. Blocking.
pub fn observe_blocking(socket: &Path, lock: &Path, deadline: Instant) -> Observation {
    Observation {
        endpoint: endpoint_blocking(socket, deadline),
        owner: owner(lock),
    }
}

/// [`observe_blocking`] for an async caller.
pub async fn observe(socket: &Path, lock: &Path, deadline: Instant) -> Observation {
    Observation {
        endpoint: endpoint(socket, deadline).await,
        owner: owner(lock),
    }
}

/// What an observation suggests, claiming no more than was seen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reading {
    /// A pong: ready.
    Ready,
    /// A refusal: a daemon is there, and not ready.
    Refusing,
    /// Something is there and did not answer.
    NotAnswering,
    /// Nothing listens, and the recorded owner is alive: a daemon may be
    /// starting or stopping.
    Held { pid: u32 },
    /// Nothing listens, and no owner is recorded or the recorded one is
    /// gone.
    Vacant,
    /// Nothing listens, and the owner cannot be told.
    Unknown,
    /// The socket cannot be reached.
    Unreachable,
}

impl Reading {
    /// Its word, as `daemon status --json` gives it.
    pub fn word(&self) -> &'static str {
        match self {
            Reading::Ready => "ready",
            Reading::Refusing => "refusing",
            Reading::NotAnswering => "not_answering",
            Reading::Held { .. } => "held",
            Reading::Vacant => "vacant",
            Reading::Unknown => "unknown",
            Reading::Unreachable => "unreachable",
        }
    }
}

impl Observation {
    pub fn reading(&self) -> Reading {
        match &self.endpoint {
            Endpoint::Answered(Answer::Pong(_)) => Reading::Ready,
            Endpoint::Answered(Answer::Refused { .. }) => Reading::Refusing,
            Endpoint::Silent | Endpoint::Garbled { .. } => Reading::NotAnswering,
            Endpoint::Unreachable { .. } => Reading::Unreachable,
            Endpoint::Absent => match &self.owner {
                Owner::Live { pid, .. } => Reading::Held { pid: *pid },
                Owner::Gone { .. } | Owner::None => Reading::Vacant,
                Owner::Unknown { .. } => Reading::Unknown,
            },
        }
    }

    /// The reading in words a person can act on.
    pub fn words(&self) -> String {
        match (&self.endpoint, self.reading()) {
            (_, Reading::Ready) => "serving".to_owned(),
            (Endpoint::Answered(Answer::Refused { message, .. }), _) => {
                format!("answering, but not ready: {message}")
            }
            (Endpoint::Garbled { why }, _) => format!("connected, but no answer to ping ({why})"),
            (_, Reading::NotAnswering) => "connected, but no answer to ping".to_owned(),
            (_, Reading::Held { pid }) => format!(
                "nothing listens; its recorded owner, pid {pid}, is alive: a daemon may be starting or stopping"
            ),
            (_, Reading::Vacant) => "no daemon".to_owned(),
            (_, Reading::Unknown) => match &self.owner {
                Owner::Unknown { why } => {
                    format!("nothing listens, and its owner cannot be told ({why})")
                }
                _ => "nothing listens, and its owner cannot be told".to_owned(),
            },
            (Endpoint::Unreachable { error }, _) => format!("cannot be reached: {error}"),
            (_, Reading::Unreachable | Reading::Refusing) => "cannot be reached".to_owned(),
        }
    }
}

/// What a starter does after an observation: the one decision the CLI's
/// autostart and the desktop's share.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Next {
    /// A daemon answers: connect. A refusal included, so the caller hears
    /// the daemon's own word on its request; nothing is started over it.
    Connect,
    /// Something may be starting or stopping, or is there without
    /// answering: look again.
    Wait,
    /// Nothing listens and no live owner is recorded: start a daemon,
    /// whose own lock attempt decides.
    Spawn,
    /// It cannot be reached: say so.
    Fail,
}

pub fn next(observation: &Observation) -> Next {
    match observation.reading() {
        Reading::Ready | Reading::Refusing => Next::Connect,
        Reading::NotAnswering | Reading::Held { .. } => Next::Wait,
        Reading::Vacant | Reading::Unknown => Next::Spawn,
        Reading::Unreachable => Next::Fail,
    }
}

/// How soon a starter may spawn again after its child exited as a
/// duplicate (another daemon took the lock first).
pub const RESPAWN_AFTER: Duration = Duration::from_millis(250);

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::os::unix::net::UnixListener;

    /// A listener that answers every `ping` with `answer`, or closes,
    /// or says nothing, as told.
    fn listener(socket: &Path, answer: Option<&'static str>) -> UnixListener {
        let listener = UnixListener::bind(socket).unwrap();
        let serving = listener.try_clone().unwrap();
        std::thread::spawn(move || {
            for stream in serving.incoming() {
                let Ok(stream) = stream else { return };
                let mut line = String::new();
                let _ = BufReader::new(stream.try_clone().unwrap()).read_line(&mut line);
                match answer {
                    Some(answer) => {
                        let _ = (&stream).write_all(answer.as_bytes());
                    }
                    // Silent: keep the connection open, say nothing.
                    None => std::thread::sleep(Duration::from_secs(5)),
                }
            }
        });
        listener
    }

    fn soon() -> Instant {
        Instant::now() + Duration::from_millis(300)
    }

    /// Each endpoint state from a real socket, blocking and async alike.
    #[tokio::test]
    async fn the_endpoint_says_what_the_socket_did() {
        let dir = tempfile::tempdir().unwrap();
        let path = |name: &str| dir.path().join(name);
        let _pong = listener(
            &path("pong.sock"),
            Some("{\"type\":\"pong\",\"version\":\"1.2.3\",\"uptime_secs\":4,\"pid\":7}\n"),
        );
        for endpoint in [
            endpoint_blocking(&path("pong.sock"), soon()),
            endpoint(&path("pong.sock"), soon()).await,
        ] {
            assert!(
                matches!(&endpoint, Endpoint::Answered(Answer::Pong(pong)) if matches!(pong.as_ref(), Response::Pong { version, pid: Some(7), .. } if version == "1.2.3")),
                "{endpoint:?}"
            );
        }
        // A refusal is kept whole, with and without details, an unknown
        // code included.
        let _refusal = listener(
            &path("refuse.sock"),
            Some(
                "{\"type\":\"error\",\"code\":\"storage_unavailable\",\"message\":\"disk full\",\"details\":{\"authority\":\"storage_failed\"}}\n",
            ),
        );
        assert_eq!(
            endpoint_blocking(&path("refuse.sock"), soon()),
            Endpoint::Answered(Answer::Refused {
                code: "storage_unavailable".into(),
                message: "disk full".into(),
                details: Some(serde_json::json!({"authority": "storage_failed"})),
            })
        );
        let _newer = listener(
            &path("newer.sock"),
            Some("{\"type\":\"error\",\"code\":\"a_newer_code\",\"message\":\"later\"}\n"),
        );
        assert_eq!(
            endpoint(&path("newer.sock"), soon()).await,
            Endpoint::Answered(Answer::Refused {
                code: "a_newer_code".into(),
                message: "later".into(),
                details: None,
            })
        );
        // Silent, garbage, EOF, another kind of reply, oversize.
        let _silent = listener(&path("silent.sock"), None);
        assert_eq!(
            endpoint_blocking(&path("silent.sock"), soon()),
            Endpoint::Silent
        );
        assert_eq!(
            endpoint(&path("silent.sock"), soon()).await,
            Endpoint::Silent
        );
        let _garbage = listener(&path("garbage.sock"), Some("not json\n"));
        assert!(matches!(
            endpoint_blocking(&path("garbage.sock"), soon()),
            Endpoint::Garbled { .. }
        ));
        let _closed = listener(&path("closed.sock"), Some(""));
        assert!(matches!(
            endpoint(&path("closed.sock"), soon()).await,
            Endpoint::Garbled { .. }
        ));
        let _other = listener(&path("other.sock"), Some("{\"type\":\"ok\"}\n"));
        assert!(matches!(
            endpoint_blocking(&path("other.sock"), soon()),
            Endpoint::Garbled { .. }
        ));
        let huge: &'static str = Box::leak("x".repeat(MAX_ANSWER + 10).into_boxed_str());
        let _huge = listener(&path("huge.sock"), Some(huge));
        assert!(matches!(
            endpoint_blocking(&path("huge.sock"), Instant::now() + Duration::from_secs(2)),
            Endpoint::Garbled { .. }
        ));
        // Absent: no socket, or a stale socket file nobody accepts on.
        assert_eq!(endpoint(&path("none.sock"), soon()).await, Endpoint::Absent);
        drop(UnixListener::bind(path("stale.sock")).unwrap());
        assert_eq!(
            endpoint_blocking(&path("stale.sock"), soon()),
            Endpoint::Absent
        );
        // Unreachable: a socket in a directory nobody may enter.
        use std::os::unix::fs::PermissionsExt;
        let closed_dir = path("closed");
        std::fs::create_dir(&closed_dir).unwrap();
        let _inside = UnixListener::bind(closed_dir.join("d.sock")).unwrap();
        std::fs::set_permissions(&closed_dir, std::fs::Permissions::from_mode(0o000)).unwrap();
        let unreachable = endpoint_blocking(&closed_dir.join("d.sock"), soon());
        std::fs::set_permissions(&closed_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        // Running as root reaches it anyway; anyone else is refused.
        if unsafe { libc::geteuid() } != 0 {
            assert!(
                matches!(unreachable, Endpoint::Unreachable { .. }),
                "{unreachable:?}"
            );
        }
    }

    /// The owner record is read without creating anything: a hint about a
    /// recorded process, gone only on positive evidence.
    #[test]
    fn the_owner_record_is_read_as_a_hint() {
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("agentd.lock");
        assert_eq!(owner(&lock), Owner::None);
        assert!(!lock.exists(), "reading creates nothing");
        let write = |text: &str| std::fs::write(&lock, text).unwrap();
        write("");
        assert!(
            matches!(owner(&lock), Owner::Unknown { .. }),
            "an empty record"
        );
        write("{\"pid\":");
        assert!(
            matches!(owner(&lock), Owner::Unknown { .. }),
            "a partial record"
        );
        write(&"x".repeat(300));
        assert!(
            matches!(owner(&lock), Owner::Unknown { .. }),
            "an oversize record"
        );
        let me = OwnerRecord::this_process();
        write(&me.line());
        assert_eq!(
            owner(&lock),
            Owner::Live {
                pid: me.pid,
                started_at: me.started_at.unwrap()
            }
        );
        // The same pid, another birth: the pid was given to another process.
        let reused = OwnerRecord {
            started_at: Some(me.started_at.unwrap() - chrono::Duration::days(1)),
            ..me.clone()
        };
        write(&reused.line());
        assert_eq!(owner(&lock), Owner::Gone { pid: me.pid });
        // Alive, no birth recorded: nothing certain.
        write(
            &OwnerRecord {
                pid: me.pid,
                started_at: None,
            }
            .line(),
        );
        assert!(matches!(owner(&lock), Owner::Unknown { .. }));
        // A process that has ended.
        let mut ended = std::process::Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .spawn()
            .unwrap();
        let pid = ended.id();
        ended.wait().unwrap();
        write(
            &OwnerRecord {
                pid,
                started_at: None,
            }
            .line(),
        );
        assert_eq!(owner(&lock), Owner::Gone { pid });
    }

    /// A listener whose queue is full never holds a connect past its
    /// deadline: Linux would make an ordinary connect wait for room; this
    /// one says busy (or, where the kernel refuses, absent) in time, and
    /// the look reads it as not answering, never as ready.
    #[test]
    fn a_full_queue_is_answered_within_the_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("full.sock");
        let _listener = UnixListener::bind(&socket).unwrap();
        let mut held = Vec::new();
        let mut refused = false;
        for _ in 0..4096 {
            let started = Instant::now();
            match connect_within(&socket, Instant::now() + Duration::from_millis(200)) {
                Ok(stream) => held.push(stream),
                Err(error) => {
                    assert!(
                        busy(&error) || absent(&error),
                        "a full queue is busy or refused: {error}"
                    );
                    refused = true;
                }
            }
            assert!(started.elapsed() < Duration::from_secs(2), "bounded");
            if refused {
                break;
            }
        }
        assert!(refused, "the queue filled");
        let started = Instant::now();
        let endpoint = endpoint_blocking(&socket, Instant::now() + Duration::from_millis(300));
        assert!(started.elapsed() < Duration::from_secs(2), "bounded");
        assert!(
            matches!(endpoint, Endpoint::Silent | Endpoint::Absent),
            "{endpoint:?}"
        );
    }

    /// A FIFO put where the owner record belongs is read as unknown at once,
    /// with no writer and with a silent one, and left as it was.
    #[test]
    fn a_fifo_in_place_of_the_record_never_holds_a_look() {
        use std::os::unix::fs::FileTypeExt;
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("agentd.lock");
        let path = std::ffi::CString::new(lock.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: a NUL-terminated path and a mode.
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        let look = |lock: std::path::PathBuf| {
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let _ = tx.send(owner(&lock));
            });
            rx.recv_timeout(Duration::from_secs(5))
                .expect("the look came back in time")
        };
        assert!(matches!(look(lock.clone()), Owner::Unknown { .. }));
        // A writer that stays silent.
        let _writer = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock)
            .unwrap();
        assert!(matches!(look(lock.clone()), Owner::Unknown { .. }));
        assert!(std::fs::metadata(&lock).unwrap().file_type().is_fifo());
    }

    /// A valid pong padded past the frame bound before its newline is not
    /// a pong, through either reader.
    #[tokio::test]
    async fn padding_past_the_bound_is_not_a_pong() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("padded.sock");
        let padded: &'static str = Box::leak(
            format!(
                "{{\"type\":\"pong\",\"version\":\"1\",\"uptime_secs\":0}}{}\n",
                " ".repeat(MAX_ANSWER + 1)
            )
            .into_boxed_str(),
        );
        let _padded = listener(&socket, Some(padded));
        let bound = || Instant::now() + Duration::from_secs(2);
        assert!(matches!(
            endpoint_blocking(&socket, bound()),
            Endpoint::Garbled { .. }
        ));
        assert!(matches!(
            endpoint(&socket, bound()).await,
            Endpoint::Garbled { .. }
        ));
    }

    /// Observing a missing or empty home creates nothing in it.
    #[tokio::test]
    async fn observing_creates_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing");
        let observation = observe(
            &missing.join("agentd.sock"),
            &missing.join("agentd.lock"),
            Instant::now() + Duration::from_millis(100),
        )
        .await;
        assert_eq!(observation.reading(), Reading::Vacant);
        assert!(!missing.exists(), "nothing was made");
        let empty = dir.path().join("empty");
        std::fs::create_dir(&empty).unwrap();
        let blocking = observe_blocking(
            &empty.join("agentd.sock"),
            &empty.join("agentd.lock"),
            Instant::now() + Duration::from_millis(100),
        );
        assert_eq!(blocking.reading(), Reading::Vacant);
        assert_eq!(std::fs::read_dir(&empty).unwrap().count(), 0, "still empty");
    }

    /// A daemon records itself through the descriptor that holds its lock,
    /// a successor through its copy of that descriptor; neither replaces
    /// the file, and a reader never takes the lock.
    #[test]
    fn a_holder_records_itself_through_its_lock() {
        use std::os::fd::AsFd;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agentd.lock");
        let held = crate::lock::try_exclusive(&path).unwrap().expect("free");
        assert!(
            matches!(owner(&path), Owner::Unknown { .. }),
            "the window before the record"
        );
        publish(&held).unwrap();
        assert!(matches!(owner(&path), Owner::Live { pid, .. } if pid == std::process::id()));
        // Reading took nothing: the lock is still ours alone.
        assert!(crate::lock::try_exclusive(&path).unwrap().is_none());
        let fd = held.into_fd();
        std::fs::write(&path, "").unwrap_or_default();
        publish_fd(fd.as_fd()).unwrap();
        assert!(matches!(owner(&path), Owner::Live { .. }));
        assert!(
            crate::lock::try_exclusive(&path).unwrap().is_none(),
            "the copy kept the lock"
        );
    }

    /// What a reading suggests, and the one decision a starter takes: a
    /// start only where nobody listens and no live owner is recorded.
    #[test]
    fn readings_and_the_next_step() {
        let pong = Endpoint::Answered(Answer::Pong(Box::new(Response::Pong {
            version: "1".into(),
            uptime_secs: 0,
            restricted: None,
            pid: None,
            executable: None,
            experimental: None,
            judge: None,
            features: Vec::new(),
            authority: None,
        })));
        let refused = Endpoint::Answered(Answer::Refused {
            code: "storage_unavailable".into(),
            message: "x".into(),
            details: None,
        });
        let live = Owner::Live {
            pid: 9,
            started_at: Utc::now(),
        };
        let unknown = Owner::Unknown {
            why: "no owner record".into(),
        };
        let look = |endpoint: &Endpoint, owner: &Owner| Observation {
            endpoint: endpoint.clone(),
            owner: owner.clone(),
        };
        let cases = [
            (look(&pong, &Owner::None), Reading::Ready, Next::Connect),
            (
                look(&refused, &Owner::None),
                Reading::Refusing,
                Next::Connect,
            ),
            (
                look(&Endpoint::Silent, &Owner::None),
                Reading::NotAnswering,
                Next::Wait,
            ),
            (
                look(&Endpoint::Garbled { why: "x".into() }, &live),
                Reading::NotAnswering,
                Next::Wait,
            ),
            (
                look(&Endpoint::Absent, &live),
                Reading::Held { pid: 9 },
                Next::Wait,
            ),
            (
                look(&Endpoint::Absent, &Owner::None),
                Reading::Vacant,
                Next::Spawn,
            ),
            (
                look(&Endpoint::Absent, &Owner::Gone { pid: 9 }),
                Reading::Vacant,
                Next::Spawn,
            ),
            (
                look(&Endpoint::Absent, &unknown),
                Reading::Unknown,
                Next::Spawn,
            ),
            (
                look(&Endpoint::Unreachable { error: "x".into() }, &Owner::None),
                Reading::Unreachable,
                Next::Fail,
            ),
        ];
        for (observation, reading, step) in cases {
            assert_eq!(observation.reading(), reading, "{observation:?}");
            assert_eq!(next(&observation), step, "{observation:?}");
            // Never claims the lock from the record.
            assert!(
                !observation.words().contains("lock held"),
                "{}",
                observation.words()
            );
        }
        assert!(
            look(&Endpoint::Absent, &live)
                .words()
                .contains("may be starting or stopping")
        );
    }

    /// The largest pong a daemon sends fits well inside the frame bound.
    #[test]
    fn a_maximal_pong_fits_the_frame_bound() {
        let pong = Response::Pong {
            version: "x".repeat(64),
            uptime_secs: u64::MAX,
            restricted: Some(std::path::PathBuf::from("/".to_owned() + &"r".repeat(1024))),
            pid: Some(u32::MAX),
            executable: Some(std::path::PathBuf::from("/".to_owned() + &"e".repeat(1024))),
            experimental: Some(Default::default()),
            judge: None,
            features: agentdocker_core::protocol::FEATURES
                .iter()
                .map(|f| f.to_string())
                .collect(),
            authority: Some(agentdocker_core::protocol::DaemonAuthority::Serving),
        };
        let line = serde_json::to_string(&pong).unwrap();
        assert!(line.len() * 4 < MAX_ANSWER, "{} bytes", line.len());
    }
}
