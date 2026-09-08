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
use std::os::fd::{AsRawFd, BorrowedFd, OwnedFd};
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
    let named: Vec<usize> = std::iter::once(handover.listener)
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
    // Zero means "no timeout" to the kernel, which is the opposite of
    // anything a caller passing zero could want here. Refused rather
    // than quietly turned into an unbounded wait.
    if within.is_zero() {
        return Err("a successor deadline of zero would never expire".to_owned());
    }
    let deadline = Instant::now() + within;

    // The timeout the kernel enforces is per-read, and `handoff::receive`
    // does a header `recvmsg` and then reads the body until it has all
    // of it. Every one of those resets the clock, so a successor that
    // trickles a byte at a time can take as long as it likes and still
    // come back `Serving` — and the predecessor would hand over on the
    // word of something that took ten minutes to say it. The socket
    // timeout only stops any single read from hanging; the deadline is
    // what bounds the wait.
    //
    // The one failure tolerated in setting it is a peer that has already
    // closed: on macOS that returns EINVAL, and it is exactly the case
    // of a successor that died before speaking. A read on a socket with
    // no peer cannot block, so the wait is still bounded. Any other
    // failure leaves the read able to hang for ever, which is not
    // something to find out later.
    if let Err(error) = socket.set_read_timeout(Some(within))
        && !peer_has_gone(socket)
    {
        return Err(format!("cannot bound the wait for the successor: {error}"));
    }

    let (payload, fds) = handoff::receive(socket).map_err(|e| {
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

/// Whether the other end has already gone.
///
/// Asked only to explain a failure to set a timeout, and answered
/// without consuming anything: a peek returns 0 at end of file, and on a
/// peer that is merely quiet it returns EAGAIN rather than blocking.
fn peer_has_gone(socket: &UnixStream) -> bool {
    use nix::sys::socket::{MsgFlags, recv};
    let mut nothing = [0_u8; 1];
    // End of file, and only that. A peer that is merely quiet answers
    // `EAGAIN` because of `DONTWAIT`, and nothing is consumed because of
    // `PEEK`.
    matches!(
        recv(
            socket.as_raw_fd(),
            &mut nothing,
            MsgFlags::MSG_PEEK | MsgFlags::MSG_DONTWAIT,
        ),
        Ok(0)
    )
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

        offer(&mine, &handover(), &[carried.as_fd()]).unwrap();
        let (received, fds) = accept(&theirs).unwrap();
        assert_eq!(received, handover());
        assert_eq!(fds.len(), 1);
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
        let files = opened(3);
        let borrowed: Vec<_> = files.iter().map(AsFd::as_fd).collect();
        let mut twice = handover();
        twice.terminals = vec![
            Terminal {
                agent: AgentId::from("abc"),
                fd: 1,
            },
            Terminal {
                agent: AgentId::from("abc"),
                fd: 2,
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
        let mut vague = handover();
        vague.adopted.push(Adopted {
            agent: AgentId::from("abc"),
            pid: 4242,
            started_at: None,
        });
        offer(&mine, &vague, &[carried.as_fd()]).unwrap();
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
