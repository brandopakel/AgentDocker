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

use std::io;
use std::os::fd::{BorrowedFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use agentdocker_host::handoff;
use serde::{Deserialize, Serialize};

use super::*;

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
pub const FORMAT: u32 = 1;

/// What the predecessor hands over, beside the descriptors themselves.
///
/// The descriptors arrive as a list with no names on them, so this says
/// what each one is. Order is the only thing `SCM_RIGHTS` preserves.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Handover {
    pub format: u32,
    /// Where the listening socket sits in the descriptor list.
    pub listener: usize,
    /// The agents whose terminals are travelling, and where each one's
    /// descriptor sits.
    pub terminals: Vec<Terminal>,
    /// Processes the successor becomes responsible for without becoming
    /// their parent. It cannot wait on them, so it tracks them the way
    /// it tracks any externally started agent: by pid and start time.
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

/// Send the descriptors and the map that explains them.
pub fn offer(socket: &UnixStream, handover: &Handover, fds: &[BorrowedFd<'_>]) -> io::Result<()> {
    let payload = serde_json::to_vec(handover).map_err(io::Error::other)?;
    handoff::send(socket, &payload, fds)
}

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
    let named = std::iter::once(handover.listener).chain(handover.terminals.iter().map(|t| t.fd));
    for index in named {
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
    Ok((handover, fds))
}

/// Say whether the takeover worked, as the successor.
pub fn answer(socket: &UnixStream, ready: &Ready) -> io::Result<()> {
    let payload = serde_json::to_vec(ready).map_err(io::Error::other)?;
    handoff::send(socket, &payload, &[])
}

/// Wait for the successor to say it is serving.
///
/// This is the only thing that can authorise giving up the socket, and
/// every path out of it that is not `Ok` means "keep serving". A
/// refusal, a malformed reply, a successor that died without saying
/// anything, and a deadline all leave the predecessor exactly as it was.
pub fn await_ready(socket: &UnixStream, within: Duration) -> Result<(), String> {
    // A deadline the wait cannot outlast — except that on macOS setting
    // one fails with EINVAL when the peer has already closed, which is
    // precisely the case of a successor that died before it said
    // anything. Refusing there would report the wrong thing about the
    // right outcome, and the read that follows cannot block on a socket
    // with no peer, so the deadline is not needed to escape it. Anything
    // still attached accepts the timeout, as it must for a successor
    // that is alive and simply silent.
    let bounded = socket.set_read_timeout(Some(within)).is_ok();
    let deadline = Instant::now() + within;
    let (payload, _) = handoff::receive(socket).map_err(|e| {
        if bounded && Instant::now() >= deadline {
            format!("the successor did not say it was serving within {within:?}")
        } else {
            format!("the successor said nothing: {e}")
        }
    })?;
    match serde_json::from_slice::<Ready>(&payload) {
        Ok(Ready::Serving) => Ok(()),
        Ok(Ready::Failed { reason }) => {
            Err(format!("the successor refused to take over: {reason}"))
        }
        Err(e) => Err(format!("the successor's answer made no sense: {e}")),
    }
}

impl Daemon {
    pub(super) async fn hand_over(self: &Arc<Self>) -> Response {
        // Still no. The protocol above exists and is tested, but a
        // handover is only safe once every descriptor, every child's
        // lifetime and the rollback have been proven together against
        // owned fixtures. Refusing is not a failure here: the daemon and
        // its agents keep running, which is exactly what the previous
        // attempt did not manage.
        Response::error(
            ErrorCode::Unavailable,
            "live daemon reload is unavailable: the descriptor handover protocol is in place \
             but process, I/O and successor-readiness transfer are not proven end to end yet; \
             the current daemon and agents remain running",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Seek, Write};
    use std::os::fd::{AsFd, AsRawFd, FromRawFd};

    fn handover() -> Handover {
        Handover {
            format: FORMAT,
            listener: 0,
            terminals: Vec::new(),
            adopted: Vec::new(),
        }
    }

    /// What arrives is the same open file, not a copy of its name.
    ///
    /// This is the property the whole upgrade rests on. A pty master
    /// cannot be reopened — reopening gives a different terminal with
    /// nobody on it — so if what crosses is not the *same* open file,
    /// none of the rest is worth building.
    #[test]
    fn what_arrives_is_the_same_open_file_not_its_name() {
        let (mine, theirs) = UnixStream::pair().unwrap();
        let file = tempfile::NamedTempFile::new().unwrap();
        let carried = std::fs::File::open(file.path()).unwrap();

        offer(&mine, &handover(), &[carried.as_fd()]).unwrap();
        let (received, fds) = accept(&theirs).unwrap();
        assert_eq!(received, handover());
        assert_eq!(fds.len(), 1);

        // Written after the descriptor crossed, and read back through
        // the descriptor that arrived.
        std::fs::write(file.path(), b"after the handover").unwrap();
        let mut back = std::fs::File::from(fds.into_iter().next().unwrap());
        back.rewind().unwrap();
        let mut said = String::new();
        back.read_to_string(&mut said).unwrap();
        assert_eq!(said, "after the handover");
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
        offer(&mine, &handover(), &[listening.as_fd()]).unwrap();
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
        let mut lying = handover();
        lying.terminals.push(Terminal {
            agent: AgentId::from("abc"),
            fd: 7,
        });
        offer(&mine, &lying, &[carried.as_fd()]).unwrap();
        let refused = accept(&theirs).unwrap_err();
        assert!(refused.to_string().contains("only 1 arrived"), "{refused}");
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
