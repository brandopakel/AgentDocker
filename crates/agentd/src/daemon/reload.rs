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

impl Daemon {
    pub(super) async fn hand_over(self: &Arc<Self>) -> Response {
        // Still no. The coordinator fence below and the session owners
        // exist and are tested, but a handover also needs a successor
        // that is validated, spawned, and proven ready before this daemon
        // leaves (the next phase). Refusing is not a failure here: the
        // daemon and its agents keep running, which is exactly what the
        // previous attempt did not manage.
        Response::error(
            ErrorCode::Unavailable,
            "live daemon reload is unavailable: session owners and the coordinator fence are in place \
             but successor selection, readiness and recovery are not proven end to end yet; \
             the current daemon and agents remain running",
        )
    }

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
                // back, in the order it was decided.
                state.coordination = Coordination::Serving;
                let deferred = std::mem::take(&mut state.deferred_recovery);
                for write in deferred {
                    if state.persist("deferred recovery", write) == Persisted::Failed {
                        return Err("deferred recovery write failed; storage disabled".into());
                    }
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

#[cfg(test)]
mod fence_tests {
    use super::*;
    use agentdocker_core::{AgentSpec, LeaseMode};
    use tempfile::TempDir;

    fn open(dir: &TempDir) -> Arc<Daemon> {
        let home = dir.path().to_path_buf();
        Arc::new(Daemon::open(home.clone(), home.join("sock")).unwrap())
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
        }
    }

    fn transfer_events(daemon: &Arc<Daemon>) -> Vec<String> {
        daemon
            .recent_events(100)
            .into_iter()
            .filter_map(|e| match e.kind {
                EventKind::DaemonTransferOffered { .. } => Some("offered".to_owned()),
                EventKind::DaemonTransferAccepted { .. } => Some("accepted".to_owned()),
                EventKind::DaemonTransferAborted { .. } => Some("aborted".to_owned()),
                _ => None,
            })
            .collect()
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
        assert!(
            after
                .iter()
                .any(|e| matches!(e.kind, EventKind::DaemonTransferAccepted { .. }))
        );
        assert!(
            after
                .iter()
                .any(|e| matches!(e.kind, EventKind::LeaseReleased { .. }))
        );
        assert!(after.iter().map(|e| e.seq).max().unwrap() > events_before);
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
