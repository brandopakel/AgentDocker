//! `daemon reload`: hand the terminals to a replacement and step aside.
//!
//! A managed agent survives a daemon restart on its own — it has its own
//! process group, and it is reparented when its parent goes. Its
//! *terminal* does not: a pty master is a descriptor, and a descriptor
//! dies with the process holding it, so `attach` after a restart has
//! nothing to reconnect to.
//!
//! So a reload moves the descriptors rather than the processes. The old
//! daemon binds a private socket, starts the replacement pointed at it,
//! sends each session's master with `SCM_RIGHTS`, and **exits without
//! stopping anything**. That last part is the whole risk of the feature:
//! an ordinary shutdown SIGTERMs every managed agent, and doing that
//! here would kill the very things the reload exists to preserve.
//!
//! What the new daemon inherits is a terminal, not a child. It never
//! forked those processes, so it cannot wait on them; they are watched
//! by pid the way an adopted agent is, and stopped by signal. That is
//! the honest shape of it, and it is what herdr's own documentation says
//! about the same mechanism: it does not move the processes, it moves
//! ownership of the terminals they are already attached to.

use std::os::fd::AsFd;
use std::os::unix::net::{UnixListener, UnixStream};

use agentdocker_host::handoff;
use serde::{Deserialize, Serialize};

use super::*;

/// What the old daemon tells the new one about each terminal it is
/// sending. The descriptors travel alongside, in this order.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Carried {
    pub agent: AgentId,
    /// The process on the far end, so the replacement can watch it.
    pub pid: u32,
    /// What the terminal has printed, so an `attach` after the reload
    /// shows the screen rather than an empty one.
    #[serde(with = "scrollback")]
    pub scrollback: Vec<u8>,
}

/// The whole message: one entry per descriptor, in the same order.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Handover {
    pub sessions: Vec<Carried>,
}

/// Scrollback is arbitrary bytes — escape sequences and partial UTF-8 —
/// so it travels base64 rather than as a string that would not survive
/// the round trip.
mod scrollback {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], out: S) -> Result<S::Ok, S::Error> {
        out.serialize_str(&agentdocker_core::protocol::encode_bytes(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(input: D) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(input)?;
        agentdocker_core::protocol::decode_bytes(&text)
            .ok_or_else(|| serde::de::Error::custom("scrollback is not valid base64"))
    }
}

/// The terminals, taken off the socket and waiting to be installed.
///
/// Collected before the daemon exists, because it has to happen before
/// the lock: the daemon being replaced holds the lock until the
/// descriptors are across.
pub struct Carriage {
    handover: Handover,
    fds: Vec<std::os::fd::OwnedFd>,
}

impl Carriage {
    /// Build one directly, for tests that have a terminal but no
    /// predecessor to get it from.
    #[cfg(test)]
    pub fn for_test(sessions: Vec<Carried>, fds: Vec<std::os::fd::OwnedFd>) -> Self {
        Self {
            handover: Handover { sessions },
            fds,
        }
    }
}

/// Connect to the daemon being replaced and take its terminals.
pub fn collect(socket: &Path) -> anyhow::Result<Carriage> {
    let stream = UnixStream::connect(socket)?;
    let (message, fds) = handoff::receive(&stream)?;
    let handover: Handover = serde_json::from_slice(&message)?;
    if handover.sessions.len() != fds.len() {
        anyhow::bail!(
            "handoff described {} terminals and carried {}",
            handover.sessions.len(),
            fds.len()
        );
    }
    Ok(Carriage { handover, fds })
}

impl Daemon {
    /// Put the carried terminals in place.
    ///
    /// Called before serving, so an `attach` never arrives between the
    /// socket opening and the sessions existing.
    pub fn install_handoff(self: &Arc<Self>, carriage: Carriage) -> usize {
        let Carriage { handover, fds } = carriage;
        let mut taken = 0;
        for (carried, master) in handover.sessions.into_iter().zip(fds) {
            // The record has to exist and still name this process: a
            // stale one would mean adopting a terminal for an agent that
            // is not there any more.
            let known = {
                let state = lock(&self.state);
                state
                    .registry
                    .get(&carried.agent)
                    .is_some_and(|record| record.pid == Some(carried.pid))
            };
            if !known {
                warn!(
                    agent = %carried.agent.short(),
                    pid = carried.pid,
                    "handoff carried a terminal for an agent this daemon does not know; closing it"
                );
                continue;
            }
            match supervisor::Session::adopt(master, carried.scrollback) {
                Ok(session) => {
                    lock(&self.sessions).insert(carried.agent.clone(), session);
                    taken += 1;
                }
                Err(error) => warn!(
                    agent = %carried.agent.short(),
                    %error,
                    "could not take over a terminal"
                ),
            }
        }
        if taken > 0 {
            info!(
                terminals = taken,
                "took over terminals from the previous daemon"
            );
        }
        taken
    }

    /// Start a replacement, give it the terminals, and step aside.
    ///
    /// Returns once the replacement has taken them; the caller then
    /// shuts down without stopping any agent.
    pub(super) async fn hand_over(self: &Arc<Self>) -> Response {
        let sessions: Vec<(AgentId, supervisor::Session)> = lock(&self.sessions)
            .iter()
            .map(|(id, session)| (id.clone(), session.clone()))
            .collect();
        if sessions.len() > handoff::MAX_FDS {
            return Response::error(
                ErrorCode::Invalid,
                format!(
                    "{} terminals is more than a handoff carries",
                    sessions.len()
                ),
            );
        }

        // A private socket beside the daemon's own, in a directory only
        // this user can read: it briefly carries every terminal on the
        // host, so it must not be reachable by anyone else.
        let path = self.home.join("handoff.sock");
        let _ = std::fs::remove_file(&path);
        let listener = match UnixListener::bind(&path) {
            Ok(listener) => listener,
            Err(error) => {
                return Response::error(
                    ErrorCode::Internal,
                    format!("cannot open a handoff socket: {error}"),
                );
            }
        };

        let mut child = match self.spawn_replacement(&path) {
            Ok(child) => child,
            Err(error) => {
                let _ = std::fs::remove_file(&path);
                return Response::error(ErrorCode::Internal, error.to_string());
            }
        };
        let pid = child.id();

        let carried: Vec<Carried> = sessions
            .iter()
            .map(|(id, session)| Carried {
                agent: id.clone(),
                pid: self.agent_pid(id).unwrap_or_default(),
                scrollback: session.scrollback(),
            })
            .collect();
        let message = match serde_json::to_vec(&Handover { sessions: carried }) {
            Ok(message) => message,
            Err(error) => {
                let _ = std::fs::remove_file(&path);
                return Response::error(ErrorCode::Internal, error.to_string());
            }
        };

        let masters: Vec<Arc<std::os::fd::OwnedFd>> =
            sessions.iter().map(|(_, s)| s.master()).collect();
        let sent = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let stream = await_replacement(&listener, &mut child)?;
            let borrowed: Vec<_> = masters.iter().map(|fd| fd.as_fd()).collect();
            handoff::send(&stream, &message, &borrowed)?;
            Ok(())
        })
        .await;
        let _ = std::fs::remove_file(&path);

        match sent {
            Ok(Ok(())) => {
                info!(
                    terminals = sessions.len(),
                    pid, "handed the terminals to the replacement"
                );
                // From here the agents belong to nobody until the
                // replacement claims them, so this daemon must leave
                // without touching them.
                self.hand_off_and_exit();
                Response::Ok
            }
            Ok(Err(error)) => Response::error(
                ErrorCode::Internal,
                format!("could not hand the terminals over: {error:#}"),
            ),
            Err(error) => Response::error(
                ErrorCode::Internal,
                format!("handoff worker failed: {error}"),
            ),
        }
    }

    fn agent_pid(&self, id: &AgentId) -> Option<u32> {
        lock(&self.state).registry.get(id).and_then(|a| a.pid)
    }

    /// Start the replacement, pointed at the handoff socket and at the
    /// same home. It waits for the lock this daemon still holds, so the
    /// two never serve at once.
    fn spawn_replacement(&self, handoff_socket: &Path) -> anyhow::Result<std::process::Child> {
        let program = std::env::current_exe()?;
        Ok(std::process::Command::new(program)
            .arg("--home")
            .arg(&self.home)
            .arg("--socket")
            .arg(&self.socket)
            .arg("--receive-handoff")
            .arg(handoff_socket)
            .stdin(std::process::Stdio::null())
            .spawn()?)
    }
}

/// How long the daemon being replaced waits for its successor to come
/// and take the terminals.
///
/// Generous, because the replacement has to start a runtime, open its
/// store and read the state file before it connects, and a loaded host
/// makes all three slower. Bounded, because the alternative is worse:
/// see `await_replacement`.
const HANDOFF_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Wait for the replacement to connect, and give up if it will not.
///
/// The obvious version of this is a blocking `accept()`, and it is
/// wrong. A replacement can fail to arrive — the binary under
/// `current_exe` was overwritten in place and the kernel refuses to run
/// it, a library it needs is gone, the machine is out of memory — and a
/// blocking accept turns that into a daemon wedged forever with a
/// client still on the line and a stale handoff socket on disk. That is
/// a worse failure than a reload that reports it could not happen.
///
/// So this polls, and watches the child while it does: a replacement
/// that exits before connecting is reported with its status, which is
/// the thing the operator actually needs to see, rather than as a
/// timeout thirty seconds later.
fn await_replacement(
    listener: &UnixListener,
    child: &mut std::process::Child,
) -> anyhow::Result<UnixStream> {
    listener.set_nonblocking(true)?;
    let deadline = std::time::Instant::now() + HANDOFF_TIMEOUT;
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                // handoff::send wants an ordinary blocking socket.
                stream.set_nonblocking(false)?;
                return Ok(stream);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error.into()),
        }
        if let Some(status) = child.try_wait()? {
            anyhow::bail!("the replacement exited before taking the terminals ({status})");
        }
        if std::time::Instant::now() >= deadline {
            // It is alive but not coming. Nothing has moved yet, so the
            // safe end is to stop it and stay the daemon.
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!(
                "the replacement did not take the terminals within {}s",
                HANDOFF_TIMEOUT.as_secs()
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listener(dir: &tempfile::TempDir) -> (UnixListener, PathBuf) {
        let path = dir.path().join("handoff.sock");
        (UnixListener::bind(&path).unwrap(), path)
    }

    /// The failure that actually happened: the replacement could not
    /// run at all — its binary had been overwritten in place, so the
    /// kernel killed it on exec — and the old daemon sat in `accept()`
    /// forever with the client still waiting. It must report instead,
    /// and report the status, not time out half a minute later.
    #[test]
    fn a_replacement_that_dies_before_connecting_is_reported() {
        let dir = tempfile::TempDir::new().unwrap();
        let (listener, _path) = listener(&dir);
        let mut child = std::process::Command::new("sh")
            .args(["-c", "exit 3"])
            .spawn()
            .unwrap();
        let started = std::time::Instant::now();
        let error = await_replacement(&listener, &mut child).unwrap_err();
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "the death is noticed, not waited out: took {:?}",
            started.elapsed()
        );
        let said = error.to_string();
        assert!(
            said.contains("exited before taking the terminals"),
            "{said}"
        );
        assert!(said.contains('3'), "the status is in the message: {said}");
    }

    #[test]
    fn a_replacement_that_connects_hands_back_a_blocking_socket() {
        let dir = tempfile::TempDir::new().unwrap();
        let (listener, path) = listener(&dir);
        // Alive but idle, standing in for a replacement still starting.
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let connector = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            UnixStream::connect(&path).unwrap()
        });
        let stream = await_replacement(&listener, &mut child).unwrap();
        // Blocking again, because handoff::send needs it to be: a read
        // with nothing to read waits for the timeout rather than
        // returning at once, which is the difference that matters.
        stream
            .set_read_timeout(Some(std::time::Duration::from_millis(200)))
            .unwrap();
        let mut byte = [0u8; 1];
        let waited = std::time::Instant::now();
        let _ = std::io::Read::read(&mut &stream, &mut byte);
        assert!(
            waited.elapsed() >= std::time::Duration::from_millis(150),
            "a non-blocking socket returns at once; this one waited {:?}",
            waited.elapsed()
        );
        let _ = connector.join();
        let _ = child.kill();
        let _ = child.wait();
    }
}
