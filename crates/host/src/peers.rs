//! Which processes hold the two ends of a Unix-domain connection, as the
//! kernel says it, never as a process says it of itself: macOS names each
//! socket and its peer by their protocol control blocks
//! (`proc_pidfdinfo(PROC_PIDFDSOCKETINFO)`, as `lsof` reads them), Linux by
//! inode, each process's from `/proc/<pid>/fd` and the peer of each from
//! `sock_diag` (`UNIX_DIAG_PEER`, as `ss -x` reads it). Elsewhere nothing is
//! known.

/// Whether `client` holds a connected Unix-domain socket whose other end
/// `server` holds. `None` when either's sockets, or the connections between
/// them, cannot be read: another user's process, one that has exited, or a
/// platform that cannot say.
pub fn connected(client: u32, server: u32) -> Option<bool> {
    imp::connected(client, server)
}

#[cfg(target_os = "macos")]
mod imp {
    use std::collections::HashSet;

    /// `PROC_PIDFDSOCKETINFO` (`sys/proc_info.h`): one descriptor's
    /// `socket_fdinfo`.
    const PROC_PIDFDSOCKETINFO: libc::c_int = 3;
    /// `SOCKINFO_UN`: the `soi_proto` union holds a `un_sockinfo`.
    const SOCKINFO_UN: i32 = 3;
    /// Byte offsets into `socket_fdinfo`: a 24-byte `proc_fileinfo`, then
    /// `socket_info`, whose 136-byte `vinfo_stat` is followed by `soi_so`,
    /// `soi_pcb`, `soi_type`, `soi_protocol` and `soi_family`; `soi_kind`
    /// comes after the counters and both `sockbuf_info`s, and the
    /// `soi_proto` union after `soi_kind` and a reserved word. For
    /// `SOCKINFO_UN` it starts with `unsi_conn_so` and `unsi_conn_pcb`.
    const PCB: usize = 24 + 136 + 8;
    const FAMILY: usize = 24 + 136 + 24;
    const KIND: usize = 24 + 232;
    const CONN_PCB: usize = 24 + 240 + 8;
    /// `sizeof(struct socket_fdinfo)`: what the kernel writes.
    const SOCKET_FDINFO: usize = 792;

    /// Each Unix-domain socket `pid` holds, as its own control block and
    /// its peer's (zero when unconnected).
    pub(super) fn sockets(pid: u32) -> Option<Vec<(u64, u64)>> {
        let pid = libc::c_int::try_from(pid).ok()?;
        let entry = std::mem::size_of::<libc::proc_fdinfo>();
        // SAFETY: a null buffer asks only for the size the list would take.
        let wanted =
            unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, std::ptr::null_mut(), 0) };
        let wanted = usize::try_from(wanted).ok().filter(|size| *size > 0)?;
        // Room for descriptors opened since the size was asked.
        let mut fds: Vec<libc::proc_fdinfo> = Vec::with_capacity(wanted / entry + 32);
        let room = libc::c_int::try_from(fds.capacity() * entry).ok()?;
        // SAFETY: the buffer has `room` bytes of capacity for proc_fdinfo
        // entries, as the kernel is told, and only what it wrote is used.
        let written = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDLISTFDS,
                0,
                fds.as_mut_ptr().cast::<libc::c_void>(),
                room,
            )
        };
        let written = usize::try_from(written).ok().filter(|size| *size > 0)?;
        // SAFETY: the kernel wrote `written` bytes of whole entries.
        unsafe { fds.set_len(written / entry) };
        let mut sockets = Vec::new();
        for fd in fds
            .iter()
            .filter(|fd| fd.proc_fdtype == libc::PROX_FDTYPE_SOCKET as u32)
        {
            // Aligned for the union's 64-bit fields, and larger than the
            // structure, which the kernel accepts.
            let mut info = [0u64; 128];
            // SAFETY: the buffer holds 1024 bytes, as the kernel is told.
            let written = unsafe {
                libc::proc_pidfdinfo(
                    pid,
                    fd.proc_fd,
                    PROC_PIDFDSOCKETINFO,
                    info.as_mut_ptr().cast::<libc::c_void>(),
                    1024,
                )
            };
            // A descriptor closed since it was listed is no socket now.
            if usize::try_from(written).ok() != Some(SOCKET_FDINFO) {
                continue;
            }
            // SAFETY: the 1024-byte buffer viewed as bytes.
            let bytes: &[u8; 1024] = unsafe { &*info.as_ptr().cast::<[u8; 1024]>() };
            let word = |at: usize| i32::from_ne_bytes(bytes[at..at + 4].try_into().unwrap());
            let handle = |at: usize| u64::from_ne_bytes(bytes[at..at + 8].try_into().unwrap());
            if word(FAMILY) == libc::AF_UNIX && word(KIND) == SOCKINFO_UN {
                sockets.push((handle(PCB), handle(CONN_PCB)));
            }
        }
        Some(sockets)
    }

    pub(super) fn connected(client: u32, server: u32) -> Option<bool> {
        let peers: HashSet<u64> = sockets(client)?
            .into_iter()
            .map(|(_, peer)| peer)
            .filter(|peer| *peer != 0)
            .collect();
        Some(sockets(server)?.iter().any(|(own, _)| peers.contains(own)))
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::collections::{HashMap, HashSet};

    /// `NETLINK_SOCK_DIAG` and its request (`linux/sock_diag.h`,
    /// `linux/unix_diag.h`).
    const NETLINK_SOCK_DIAG: libc::c_int = 4;
    const SOCK_DIAG_BY_FAMILY: u16 = 20;
    const UDIAG_SHOW_PEER: u32 = 0x4;
    const UNIX_DIAG_PEER: u16 = 2;
    const NLMSG_ERROR: u16 = 2;
    const NLMSG_DONE: u16 = 3;
    /// `nlmsghdr`, `unix_diag_msg` and `rtattr` lengths.
    const HEADER: usize = 16;
    const MESSAGE: usize = 16;
    const ATTRIBUTE: usize = 4;

    #[repr(C)]
    struct Request {
        header: libc::nlmsghdr,
        family: u8,
        protocol: u8,
        pad: u16,
        states: u32,
        ino: u32,
        show: u32,
        cookie: [u32; 2],
    }

    /// The inodes of the sockets `pid` holds.
    pub(super) fn inodes(pid: u32) -> Option<HashSet<u64>> {
        let mut inodes = HashSet::new();
        for entry in std::fs::read_dir(format!("/proc/{pid}/fd")).ok()?.flatten() {
            let Ok(target) = std::fs::read_link(entry.path()) else {
                continue;
            };
            if let Some(inode) = target
                .to_str()
                .and_then(|target| target.strip_prefix("socket:["))
                .and_then(|rest| rest.strip_suffix(']'))
                .and_then(|inode| inode.parse().ok())
            {
                inodes.insert(inode);
            }
        }
        Some(inodes)
    }

    fn align(length: usize) -> usize {
        (length + 3) & !3
    }

    /// Every connected Unix-domain socket in this network namespace, by
    /// inode, with its peer's inode.
    pub(super) fn peers() -> Option<HashMap<u64, u64>> {
        // SAFETY: socket has no memory preconditions; the descriptor is
        // owned and closed below.
        let fd = unsafe {
            libc::socket(
                libc::AF_NETLINK,
                libc::SOCK_DGRAM | libc::SOCK_CLOEXEC,
                NETLINK_SOCK_DIAG,
            )
        };
        if fd < 0 {
            return None;
        }
        // SAFETY: `fd` was just opened and nothing else owns it.
        let socket = unsafe { <std::os::fd::OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(fd) };
        let raw = std::os::fd::AsRawFd::as_raw_fd(&socket);
        let wait = libc::timeval {
            tv_sec: 2,
            tv_usec: 0,
        };
        // SAFETY: the option value is a timeval of the size given.
        unsafe {
            libc::setsockopt(
                raw,
                libc::SOL_SOCKET,
                libc::SO_RCVTIMEO,
                (&raw const wait).cast::<libc::c_void>(),
                std::mem::size_of::<libc::timeval>() as libc::socklen_t,
            )
        };
        // SAFETY: nlmsghdr is plain old data, so all-zero is valid.
        let mut header: libc::nlmsghdr = unsafe { std::mem::zeroed() };
        header.nlmsg_len = std::mem::size_of::<Request>() as u32;
        header.nlmsg_type = SOCK_DIAG_BY_FAMILY;
        header.nlmsg_flags = (libc::NLM_F_REQUEST | libc::NLM_F_DUMP) as u16;
        header.nlmsg_seq = 1;
        let request = Request {
            header,
            family: libc::AF_UNIX as u8,
            protocol: 0,
            pad: 0,
            states: u32::MAX,
            ino: 0,
            show: UDIAG_SHOW_PEER,
            cookie: [u32::MAX; 2],
        };
        // SAFETY: sockaddr_nl is plain old data; zero names the kernel.
        let mut kernel: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
        kernel.nl_family = libc::AF_NETLINK as libc::sa_family_t;
        // SAFETY: the request and the address are of the sizes given.
        let sent = unsafe {
            libc::sendto(
                raw,
                (&raw const request).cast::<libc::c_void>(),
                std::mem::size_of::<Request>(),
                0,
                (&raw const kernel).cast::<libc::sockaddr>(),
                std::mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t,
            )
        };
        if usize::try_from(sent).ok() != Some(std::mem::size_of::<Request>()) {
            return None;
        }
        let mut peers = HashMap::new();
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            // SAFETY: the buffer holds its length in bytes.
            let read = unsafe {
                libc::recv(
                    raw,
                    buffer.as_mut_ptr().cast::<libc::c_void>(),
                    buffer.len(),
                    0,
                )
            };
            let read = usize::try_from(read).ok().filter(|read| *read > 0)?;
            let mut at = 0;
            while at + HEADER <= read {
                let field = |offset: usize, width: usize| &buffer[at + offset..at + offset + width];
                let length = u32::from_ne_bytes(field(0, 4).try_into().ok()?) as usize;
                let kind = u16::from_ne_bytes(field(4, 2).try_into().ok()?);
                if length < HEADER || at + length > read {
                    return None;
                }
                match kind {
                    NLMSG_DONE => return Some(peers),
                    NLMSG_ERROR => return None,
                    SOCK_DIAG_BY_FAMILY if length >= HEADER + MESSAGE => {
                        let inode = u32::from_ne_bytes(field(HEADER + 4, 4).try_into().ok()?);
                        let mut attribute = HEADER + MESSAGE;
                        while attribute + ATTRIBUTE <= length {
                            let size =
                                u16::from_ne_bytes(field(attribute, 2).try_into().ok()?) as usize;
                            let name = u16::from_ne_bytes(field(attribute + 2, 2).try_into().ok()?);
                            if size < ATTRIBUTE || attribute + size > length {
                                break;
                            }
                            if name == UNIX_DIAG_PEER && size >= ATTRIBUTE + 4 {
                                let peer = u32::from_ne_bytes(
                                    field(attribute + ATTRIBUTE, 4).try_into().ok()?,
                                );
                                if peer != 0 {
                                    peers.insert(u64::from(inode), u64::from(peer));
                                }
                            }
                            attribute += align(size);
                        }
                    }
                    _ => {}
                }
                at += align(length);
            }
        }
    }

    pub(super) fn connected(client: u32, server: u32) -> Option<bool> {
        let client = inodes(client)?;
        let server = inodes(server)?;
        let peers = peers()?;
        Some(
            client
                .iter()
                .filter_map(|inode| peers.get(inode))
                .any(|peer| server.contains(peer)),
        )
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod imp {
    pub(super) fn connected(_client: u32, _server: u32) -> Option<bool> {
        None
    }
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod tests {
    use std::io::BufRead as _;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::process::{Command, Stdio};

    use super::connected;

    /// A process that connects to `socket` and says so, then waits.
    fn client(socket: &std::path::Path) -> std::process::Child {
        let mut child = Command::new("python3")
            .arg("-c")
            .arg("import socket,sys,time\ns=socket.socket(socket.AF_UNIX)\ns.connect(sys.argv[1])\nprint('connected',flush=True)\ntime.sleep(60)\n")
            .arg(socket)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut line = String::new();
        std::io::BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        assert_eq!(line.trim(), "connected");
        child
    }

    /// A process connected to a socket this one serves is its client, and
    /// this one holds the other end; one connected elsewhere, or nowhere,
    /// is not.
    #[test]
    fn a_process_is_the_client_only_of_the_server_holding_its_peer() {
        let dir = tempfile::tempdir().unwrap();
        let ours = dir.path().join("ours.sock");
        let theirs = dir.path().join("theirs.sock");
        let listener = UnixListener::bind(&ours).unwrap();
        let other = UnixListener::bind(&theirs).unwrap();
        let mut terminal = client(&ours);
        let (_accepted, _) = listener.accept().unwrap();
        let mut stranger = client(&theirs);
        let (_elsewhere, _) = other.accept().unwrap();
        let mut idle = Command::new("sleep").arg("60").spawn().unwrap();
        let me = std::process::id();
        assert_eq!(connected(terminal.id(), me), Some(true), "our client");
        assert_eq!(connected(me, terminal.id()), Some(true), "both ends");
        assert_eq!(connected(stranger.id(), terminal.id()), Some(false));
        assert_eq!(connected(idle.id(), me), Some(false), "connected nowhere");
        for child in [&mut terminal, &mut stranger, &mut idle] {
            let _ = child.kill();
            let _ = child.wait();
        }
        assert_eq!(connected(terminal.id(), me), None, "gone");
    }

    /// Both ends of a pair in one process name each other.
    #[test]
    fn both_ends_of_a_pair_are_read() {
        let (_one, _two) = UnixStream::pair().unwrap();
        let me = std::process::id();
        assert_eq!(connected(me, me), Some(true));
    }

    /// The structure's layout, as the bytes read here assume it, matches
    /// the kernel's: the 136-byte `vinfo_stat` that starts `socket_info`.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_socket_layout_matches_the_kernel_s() {
        assert_eq!(std::mem::size_of::<libc::vinfo_stat>(), 136);
        let (one, two) = UnixStream::pair().unwrap();
        let sockets = super::imp::sockets(std::process::id()).unwrap();
        let pair: Vec<_> = sockets
            .iter()
            .filter(|(own, peer)| {
                sockets
                    .iter()
                    .any(|(other, back)| other == peer && back == own)
            })
            .collect();
        assert!(pair.len() >= 2, "{sockets:?}");
        assert!(
            pair.iter()
                .all(|(own, peer)| *own != 0 && *peer != 0 && own != peer)
        );
        drop((one, two));
    }
}
