//! Bounded UI admission with one queued refresh per snapshot kind/project.
use super::Cmd;
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, mpsc};

pub(super) const CAPACITY: usize = 32;
pub(super) const COMMAND_BYTES: usize = 64 * 1024;

/// Bound retained command allocations as well as the number of commands.
fn bytes(command: &Cmd) -> usize {
    match command {
        Cmd::Stop(text) => text.capacity(),
        Cmd::Journal(id, selector) | Cmd::Channels(id, selector) => {
            id.capacity().saturating_add(selector.capacity())
        }
        Cmd::Console(text, cwd) => text
            .capacity()
            .saturating_add(cwd.as_ref().map_or(0, |p| p.capacity())),
        Cmd::Answer(_, text) => text.capacity(),
        Cmd::ChannelSend(id, text) => id.capacity().saturating_add(text.capacity()),
        Cmd::Launch(spec) => {
            serde_json::to_vec(spec).map_or(COMMAND_BYTES + 1, |bytes| bytes.len())
        }
        Cmd::Setup(args) | Cmd::Desktop(args) => args.iter().fold(
            args.capacity().saturating_mul(size_of::<String>()),
            |total, arg| total.saturating_add(arg.capacity()),
        ),
        _ => 0,
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Key {
    Agents,
    Leases,
    Runtimes,
    Discovered,
    Journal(String, String),
    Channels(String, String),
    Inbox,
    Activity,
    Me,
    Questions,
}

fn key(command: &Cmd) -> Option<Key> {
    Some(match command {
        Cmd::Agents => Key::Agents,
        Cmd::Leases => Key::Leases,
        Cmd::Runtimes => Key::Runtimes,
        Cmd::Discovered => Key::Discovered,
        Cmd::Journal(project, selector) => Key::Journal(project.clone(), selector.clone()),
        Cmd::Channels(project, selector) => Key::Channels(project.clone(), selector.clone()),
        Cmd::Inbox => Key::Inbox,
        Cmd::Activity => Key::Activity,
        Cmd::Me => Key::Me,
        Cmd::Questions => Key::Questions,
        _ => return None,
    })
}

type Pending = Arc<Mutex<BTreeSet<Key>>>;

#[derive(Clone)]
pub(super) struct Sender {
    inner: mpsc::SyncSender<Cmd>,
    pending: Pending,
}

pub(super) struct Receiver {
    inner: mpsc::Receiver<Cmd>,
    pending: Pending,
}

#[derive(Debug)]
pub(super) struct Rejected {
    pub command: Cmd,
    pub reason: &'static str,
}

pub(super) fn channel() -> (Sender, Receiver) {
    let (tx, rx) = mpsc::sync_channel(CAPACITY);
    let pending = Pending::default();
    (
        Sender {
            inner: tx,
            pending: pending.clone(),
        },
        Receiver { inner: rx, pending },
    )
}

impl Sender {
    /// UI submission never waits for the daemon or for room in its queue.
    pub(super) fn send(&self, command: Cmd) -> Result<(), Rejected> {
        if bytes(&command) > COMMAND_BYTES {
            return Err(Rejected {
                command,
                reason: "It exceeds the 64 KiB command limit.",
            });
        }
        let refresh = key(&command);
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(key) = &refresh
            && !pending.insert(key.clone())
        {
            return Ok(());
        }
        let error = match self.inner.try_send(command) {
            Ok(()) => return Ok(()),
            Err(mpsc::TrySendError::Full(command)) => Rejected {
                command,
                reason: "The daemon request queue is full; try again when it catches up.",
            },
            Err(mpsc::TrySendError::Disconnected(command)) => Rejected {
                command,
                reason: "The window's request worker stopped; reopen agentdocker.",
            },
        };
        if let Some(key) = refresh {
            pending.remove(&key);
        }
        Err(error)
    }
}

impl Receiver {
    fn dispatched(&self, command: &Cmd) {
        if let Some(key) = key(command) {
            self.pending
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .remove(&key);
        }
    }

    /// Clear the coalescing key before I/O, so changes during an in-flight
    /// snapshot can still schedule one follow-up request.
    pub(super) fn recv(&self) -> Result<Cmd, mpsc::RecvError> {
        let command = self.inner.recv()?;
        self.dispatched(&command);
        Ok(command)
    }

    #[cfg(test)]
    pub(super) fn try_iter(&self) -> impl Iterator<Item = Cmd> + '_ {
        self.inner
            .try_iter()
            .inspect(|command| self.dispatched(command))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refreshes_with_distinct_roots_are_not_dropped() {
        let (sender, receiver) = channel();
        for selector in ["/clone-one", "/clone-two", "/clone-two"] {
            sender
                .send(Cmd::Journal("same-fingerprint".into(), selector.into()))
                .unwrap();
            sender
                .send(Cmd::Channels("same-fingerprint".into(), selector.into()))
                .unwrap();
        }
        assert_eq!(receiver.try_iter().count(), 4);
    }
}
