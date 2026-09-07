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

        let child = match self.spawn_replacement(&path) {
            Ok(child) => child,
            Err(error) => {
                let _ = std::fs::remove_file(&path);
                return Response::error(ErrorCode::Internal, error.to_string());
            }
        };

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
        let sent = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            // Bounded: a replacement that never connects must not leave
            // this daemon waiting with a client on the line.
            listener.set_nonblocking(false)?;
            let (stream, _) = listener.accept()?;
            let borrowed: Vec<_> = masters.iter().map(|fd| fd.as_fd()).collect();
            handoff::send(&stream, &message, &borrowed)
        })
        .await;
        let _ = std::fs::remove_file(&path);

        match sent {
            Ok(Ok(())) => {
                info!(
                    terminals = sessions.len(),
                    pid = child,
                    "handed the terminals to the replacement"
                );
                // From here the agents belong to nobody until the
                // replacement claims them, so this daemon must leave
                // without touching them.
                self.hand_off_and_exit();
                Response::Ok
            }
            Ok(Err(error)) => Response::error(
                ErrorCode::Internal,
                format!("could not hand the terminals over: {error}"),
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
    fn spawn_replacement(&self, handoff_socket: &Path) -> anyhow::Result<u32> {
        let program = std::env::current_exe()?;
        let child = std::process::Command::new(program)
            .arg("--home")
            .arg(&self.home)
            .arg("--socket")
            .arg(&self.socket)
            .arg("--receive-handoff")
            .arg(handoff_socket)
            .stdin(std::process::Stdio::null())
            .spawn()?;
        Ok(child.id())
    }
}
