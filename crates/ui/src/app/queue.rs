//! Bounded UI admission with one queued refresh per snapshot kind/project.
use super::Cmd;
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, mpsc};

pub(super) const CAPACITY: usize = 32;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Key {
    Agents,
    Leases,
    Runtimes,
    Discovered,
    Journal(String),
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
        Cmd::Journal(project) => Key::Journal(project.clone()),
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
