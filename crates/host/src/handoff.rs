//! Passing open descriptors between two daemons.
//!
//! An upgrade should not disturb a running agent. The processes survive
//! it on their own — they have their own process groups and are
//! reparented when their parent goes — but their *terminals* do not: a
//! pty master is a descriptor, and a descriptor dies with the process
//! holding it. So `attach` after a restart has nothing to reconnect to.
//!
//! `SCM_RIGHTS` is the fix, and it is the only one: a Unix socket can
//! carry an open file descriptor from one process to another, and the
//! receiver gets a descriptor to the *same* open file — the same pty,
//! with the same agent still on the other end of it. Nothing is
//! reopened, so nothing is lost.
//!
//! This module is only the mechanism: bytes and descriptors over a
//! socket, with the framing that keeps them together. What is sent, and
//! what the receiver does with it, is the daemon's business.

use std::io;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;

use nix::sys::socket::{ControlMessage, ControlMessageOwned, MsgFlags, recvmsg, sendmsg};

/// The largest message this will send or accept.
///
/// A handoff carries scrollback — tens of kilobytes per terminal — so
/// the limit is generous, but it is a limit: a receiver must not be
/// asked to allocate whatever a sender claims.
pub const MAX_MESSAGE: usize = 4 * 1024 * 1024;

/// How many descriptors one message may carry. Each is a terminal, and
/// a machine with more than this many agents on one host has other
/// problems.
pub const MAX_FDS: usize = 64;

/// Send `payload` with `fds` attached, as one message.
///
/// The length goes first so the receiver knows what to expect, and the
/// descriptors ride the same `sendmsg` as the first byte of the body —
/// control data belongs to a message, not to a stream, so it has to
/// travel with something.
pub fn send(socket: &UnixStream, payload: &[u8], fds: &[BorrowedFd<'_>]) -> io::Result<()> {
    if payload.len() > MAX_MESSAGE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("handoff message is {} bytes, over the limit", payload.len()),
        ));
    }
    if fds.len() > MAX_FDS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} descriptors is more than a handoff carries", fds.len()),
        ));
    }
    let raw: Vec<RawFd> = fds.iter().map(AsRawFd::as_raw_fd).collect();
    let header = (payload.len() as u32).to_be_bytes();
    // The header and the descriptors go together: a receiver that read a
    // length without its descriptors could not tell a short message from
    // a sender that sent none.
    let control = [ControlMessage::ScmRights(&raw)];
    let sent = sendmsg::<()>(
        socket.as_raw_fd(),
        &[io::IoSlice::new(&header)],
        &control,
        MsgFlags::empty(),
        None,
    )?;
    if sent != header.len() {
        return Err(io::Error::other("handoff header was truncated"));
    }
    // The body is ordinary bytes once the descriptors are across.
    write_all(socket, payload)
}

/// Receive one message and its descriptors.
pub fn receive(socket: &UnixStream) -> io::Result<(Vec<u8>, Vec<OwnedFd>)> {
    let mut header = [0_u8; 4];
    // The receive is its own scope: `RecvMsg` borrows the buffer, which
    // borrows the header, and the header has to be readable afterwards.
    let (read, fds) = {
        let mut buffer = [io::IoSliceMut::new(&mut header)];
        // Room for the control data: one `cmsghdr` plus the descriptors.
        let mut space = nix::cmsg_space!([RawFd; MAX_FDS]);
        let message = recvmsg::<()>(
            socket.as_raw_fd(),
            &mut buffer,
            Some(&mut space),
            MsgFlags::empty(),
        )?;
        let fds: Vec<OwnedFd> = message
            .cmsgs()?
            .flat_map(|control| match control {
                ControlMessageOwned::ScmRights(fds) => fds,
                _ => Vec::new(),
            })
            // SAFETY: the kernel just created these in this process and
            // nothing else refers to them, so this is their only owner.
            .map(|fd| unsafe { OwnedFd::from_raw_fd(fd) })
            .collect();
        (message.bytes, fds)
    };
    if read != header.len() {
        return Err(io::Error::other("handoff ended before its header"));
    }
    let length = u32::from_be_bytes(header) as usize;
    if length > MAX_MESSAGE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("handoff claims {length} bytes, over the limit"),
        ));
    }
    let mut payload = vec![0_u8; length];
    read_exact(socket, &mut payload)?;
    Ok((payload, fds))
}

fn write_all(mut socket: &UnixStream, mut bytes: &[u8]) -> io::Result<()> {
    use io::Write;
    while !bytes.is_empty() {
        match socket.write(bytes) {
            Ok(0) => return Err(io::Error::other("handoff closed while writing")),
            Ok(n) => bytes = &bytes[n..],
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

fn read_exact(mut socket: &UnixStream, mut into: &mut [u8]) -> io::Result<()> {
    use io::Read;
    while !into.is_empty() {
        match socket.read(into) {
            Ok(0) => return Err(io::Error::other("handoff closed while reading")),
            Ok(n) => into = &mut into[n..],
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::fd::AsFd;

    #[test]
    fn a_descriptor_arrives_pointing_at_the_same_open_file() {
        let (here, there) = UnixStream::pair().unwrap();
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), b"written before the handoff").unwrap();
        let opened = std::fs::File::open(file.path()).unwrap();

        send(&here, b"the payload", &[opened.as_fd()]).unwrap();
        let (payload, fds) = receive(&there).unwrap();
        assert_eq!(payload, b"the payload");
        assert_eq!(fds.len(), 1);

        // The same open file, not a reopened one: what the receiver reads
        // is what the sender had open.
        let mut received = std::fs::File::from(fds.into_iter().next().unwrap());
        let mut text = String::new();
        received.read_to_string(&mut text).unwrap();
        assert_eq!(text, "written before the handoff");
    }

    /// The property the whole feature rests on: the descriptor that
    /// arrives is the *same* open file description, so a pty carried
    /// across still has the same process on the other end of it.
    #[test]
    fn a_terminal_survives_the_crossing_with_its_process_still_attached() {
        let mut pty = crate::pty::Pty::open().unwrap();
        let slave = pty.take_slave().unwrap();
        let mut child = {
            let stdin = slave.try_clone().unwrap();
            let stdout = slave.try_clone().unwrap();
            let mut command = std::process::Command::new("sh");
            command
                .arg("-c")
                .arg("read line; echo \"heard:$line\"")
                .stdin(std::process::Stdio::from(stdin))
                .stdout(std::process::Stdio::from(stdout))
                .stderr(std::process::Stdio::from(slave));
            // SAFETY: only async-signal-safe calls, as the contract says.
            unsafe {
                use std::os::unix::process::CommandExt;
                command.pre_exec(|| crate::pty::take_controlling_terminal())
            };
            command.spawn().unwrap()
        };

        // Hand the master across, then drop every reference the sender
        // had — exactly what an exiting daemon does.
        let (here, there) = UnixStream::pair().unwrap();
        let master = pty.into_master();
        send(&here, b"{}", &[master.as_fd()]).unwrap();
        let (_, fds) = receive(&there).unwrap();
        drop(master);
        drop(here);

        // The receiver now drives the same terminal, and the agent that
        // was already on it answers.
        let mut carried = std::fs::File::from(fds.into_iter().next().unwrap());
        carried.write_all(b"hello\n").unwrap();
        let mut seen = String::new();
        let mut buffer = [0_u8; 256];
        while let Ok(read) = carried.read(&mut buffer) {
            if read == 0 {
                break;
            }
            seen.push_str(&String::from_utf8_lossy(&buffer[..read]));
            if seen.contains("heard:hello") {
                break;
            }
        }
        let _ = child.wait();
        assert!(
            seen.contains("heard:hello"),
            "the process was still on the far end: {seen:?}"
        );
    }

    #[test]
    fn several_descriptors_cross_together_and_keep_their_order() {
        let (here, there) = UnixStream::pair().unwrap();
        let files: Vec<_> = (0..4)
            .map(|i| {
                let file = tempfile::NamedTempFile::new().unwrap();
                std::fs::write(file.path(), format!("file {i}")).unwrap();
                (
                    file.path().to_path_buf(),
                    std::fs::File::open(file.path()).unwrap(),
                    file,
                )
            })
            .collect();
        let borrowed: Vec<_> = files.iter().map(|(_, f, _)| f.as_fd()).collect();
        send(&here, b"four", &borrowed).unwrap();
        let (payload, fds) = receive(&there).unwrap();
        assert_eq!(payload, b"four");
        assert_eq!(fds.len(), 4);
        for (i, fd) in fds.into_iter().enumerate() {
            let mut text = String::new();
            std::fs::File::from(fd).read_to_string(&mut text).unwrap();
            assert_eq!(text, format!("file {i}"), "order is preserved");
        }
    }

    #[test]
    fn a_message_with_no_descriptors_is_ordinary() {
        let (here, there) = UnixStream::pair().unwrap();
        send(&here, b"nothing attached", &[]).unwrap();
        let (payload, fds) = receive(&there).unwrap();
        assert_eq!(payload, b"nothing attached");
        assert!(fds.is_empty());
    }

    #[test]
    fn an_oversized_message_is_refused_at_both_ends() {
        let (here, _there) = UnixStream::pair().unwrap();
        let huge = vec![0_u8; MAX_MESSAGE + 1];
        let err = send(&here, &huge, &[]).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);

        // And a receiver does not allocate whatever a sender claims.
        let (here, there) = UnixStream::pair().unwrap();
        let lie = ((MAX_MESSAGE + 1) as u32).to_be_bytes();
        sendmsg::<()>(
            here.as_raw_fd(),
            &[io::IoSlice::new(&lie)],
            &[],
            MsgFlags::empty(),
            None,
        )
        .unwrap();
        let err = receive(&there).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn a_socket_that_closes_early_is_an_error_rather_than_a_hang() {
        let (here, there) = UnixStream::pair().unwrap();
        drop(here);
        assert!(receive(&there).is_err());
    }
}
