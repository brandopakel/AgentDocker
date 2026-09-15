//! The successor's side of a live replacement: receive the predecessor's
//! listener, daemon lock and transfer on an inherited descriptor before
//! anything else happens, so the new process starts holding what the old
//! one held and never competes for it.

use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;

use anyhow::Context;

use crate::daemon::reload::{self, Handover};

/// Everything a take-over starts with.
#[derive(Debug)]
pub struct Takeover {
    pub handover: Handover,
    /// The predecessor's end of the conversation: the readiness answer
    /// goes back on it.
    pub socket: UnixStream,
    /// The listening socket, still bound and still accepting.
    pub listener: OwnedFd,
    /// The daemon lock, held for as long as this descriptor is open.
    pub lock: OwnedFd,
    /// The restricted endpoint's listener, when the predecessor had one.
    pub restricted: Option<OwnedFd>,
}

/// Receive the handover on `fd`, which the predecessor arranged to be
/// inherited. Refuses anything that is not a validated FORMAT handover
/// carrying both descriptors it names.
pub fn receive(fd: i32) -> anyhow::Result<Takeover> {
    anyhow::ensure!(fd >= 0, "take-over descriptor must be non-negative");
    // SAFETY: the predecessor dup'd its end of a socketpair onto this
    // descriptor before exec; nothing else in this process owns it.
    let socket = unsafe { UnixStream::from_raw_fd(fd) };
    let (handover, mut fds) = reload::accept(&socket).context("cannot receive the handover")?;
    anyhow::ensure!(
        handover.terminals.is_empty() && handover.adopted.is_empty(),
        "a handover with terminals or adopted processes predates session owners"
    );
    // Take the two named descriptors out of the list; `accept` already
    // proved both indices exist and differ.
    let mut take = |index: usize| -> OwnedFd {
        // Replace with a harmless duplicate of stderr so indices stay valid.
        let placeholder = unsafe { OwnedFd::from_raw_fd(nix::libc::dup(2)) };
        std::mem::replace(&mut fds[index], placeholder)
    };
    let listener = take(handover.listener);
    let lock = take(handover.lock);
    let restricted = handover.restricted.map(&mut take);
    drop(fds);
    Ok(Takeover {
        handover,
        socket,
        listener,
        lock,
        restricted,
    })
}

impl Takeover {
    /// The listener as tokio serves it. The descriptor is set nonblocking
    /// here; the predecessor's copy stays as it was.
    pub fn tokio_listener(&self) -> anyhow::Result<tokio::net::UnixListener> {
        adopt(&self.listener)
    }

    /// The restricted listener as tokio serves it, when one travelled.
    pub fn tokio_restricted(&self) -> anyhow::Result<Option<tokio::net::UnixListener>> {
        self.restricted.as_ref().map(adopt).transpose()
    }
}

/// A tokio listener over a duplicate of a bound descriptor; the original
/// stays as it was for the handover after this one.
fn adopt(fd: &OwnedFd) -> anyhow::Result<tokio::net::UnixListener> {
    let dup = fd.try_clone().context("cannot duplicate the listener")?;
    let std = std::os::unix::net::UnixListener::from(dup);
    std.set_nonblocking(true)?;
    tokio::net::UnixListener::from_std(std).context("cannot adopt the listener")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::{AsFd, AsRawFd, IntoRawFd};
    use std::os::unix::fs::MetadataExt;
    use std::path::PathBuf;

    fn handover(restricted: Option<usize>) -> Handover {
        Handover {
            format: reload::FORMAT,
            transfer: "t".into(),
            listener: 0,
            lock: 1,
            restricted,
            home: PathBuf::from("/tmp/h"),
            socket: PathBuf::from("/tmp/h/agentd.sock"),
            terminals: Vec::new(),
            adopted: Vec::new(),
        }
    }

    fn identity(fd: &OwnedFd) -> (u64, u64) {
        let file = std::fs::File::from(fd.try_clone().unwrap());
        let meta = file.metadata().unwrap();
        (meta.dev(), meta.ino())
    }

    /// Each named descriptor comes out under its own name, whatever order
    /// the predecessor listed them in, and nothing else is kept.
    #[test]
    fn receive_sorts_the_descriptors_by_the_names_the_handover_gives() {
        let dir = tempfile::TempDir::new().unwrap();
        let listener = std::os::unix::net::UnixListener::bind(dir.path().join("host")).unwrap();
        let lock = std::fs::File::create(dir.path().join("lock")).unwrap();
        let restricted =
            std::os::unix::net::UnixListener::bind(dir.path().join("container")).unwrap();
        let (mine, theirs) = UnixStream::pair().unwrap();
        // The lock first, then the restricted endpoint, then the listener:
        // the map, not the order, says which is which.
        let mut shuffled = handover(Some(1));
        shuffled.listener = 2;
        shuffled.lock = 0;
        reload::offer(
            &mine,
            &shuffled,
            &[lock.as_fd(), restricted.as_fd(), listener.as_fd()],
        )
        .unwrap();

        let took = receive(theirs.into_raw_fd()).unwrap();
        assert_eq!(took.handover, shuffled);
        let lock_id = {
            let meta = lock.metadata().unwrap();
            (meta.dev(), meta.ino())
        };
        assert_eq!(identity(&took.lock), lock_id);
        let served = std::os::unix::net::UnixListener::from(took.listener.try_clone().unwrap());
        assert_eq!(
            served.local_addr().unwrap().as_pathname(),
            Some(dir.path().join("host").as_path())
        );
        let container =
            std::os::unix::net::UnixListener::from(took.restricted.unwrap().try_clone().unwrap());
        assert_eq!(
            container.local_addr().unwrap().as_pathname(),
            Some(dir.path().join("container").as_path())
        );
        // The conversation itself is kept for the readiness answer.
        assert!(took.socket.as_raw_fd() >= 0);
        reload::answer(&took.socket, &reload::Ready::Serving).unwrap();
        reload::await_ready(&mine, std::time::Duration::from_secs(1)).unwrap();
    }

    /// Without a restricted index the successor binds its own container
    /// endpoint, so none is taken.
    #[test]
    fn receive_leaves_the_restricted_endpoint_absent_when_unnamed() {
        let dir = tempfile::TempDir::new().unwrap();
        let listener = std::os::unix::net::UnixListener::bind(dir.path().join("host")).unwrap();
        let lock = std::fs::File::create(dir.path().join("lock")).unwrap();
        let (mine, theirs) = UnixStream::pair().unwrap();
        reload::offer(&mine, &handover(None), &[listener.as_fd(), lock.as_fd()]).unwrap();
        let took = receive(theirs.into_raw_fd()).unwrap();
        assert!(took.restricted.is_none());
        assert!(took.tokio_restricted().unwrap().is_none());
    }

    /// A handover shaped for the design before session owners — terminals
    /// or adopted processes riding along — is refused rather than served
    /// with children nobody supervises.
    #[test]
    fn receive_refuses_a_handover_that_predates_session_owners() {
        let dir = tempfile::TempDir::new().unwrap();
        let listener = std::os::unix::net::UnixListener::bind(dir.path().join("host")).unwrap();
        let lock = std::fs::File::create(dir.path().join("lock")).unwrap();
        let terminal = std::fs::File::open("/dev/null").unwrap();
        let (mine, theirs) = UnixStream::pair().unwrap();
        let mut old = handover(None);
        old.terminals.push(reload::Terminal {
            agent: agentdocker_core::AgentId::from("abc"),
            fd: 2,
        });
        reload::offer(
            &mine,
            &old,
            &[listener.as_fd(), lock.as_fd(), terminal.as_fd()],
        )
        .unwrap();
        let refused = receive(theirs.into_raw_fd()).unwrap_err();
        assert!(
            refused.to_string().contains("predates session owners"),
            "{refused:#}"
        );
    }

    /// A descriptor number that cannot be a socket is refused before
    /// anything is read from it.
    #[test]
    fn receive_refuses_a_negative_descriptor() {
        let refused = receive(-1).unwrap_err();
        assert!(refused.to_string().contains("non-negative"), "{refused:#}");
    }
}
