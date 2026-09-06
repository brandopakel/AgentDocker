//! Filesystem locations shared by the daemon and its clients.
//!
//! Unix socket names are short by law — `sun_path` holds 104 bytes on
//! macOS and the BSDs, 108 on Linux — while a home directory can be as long
//! as anyone likes. The rules here keep the two apart: a home whose path
//! leaves no room for `container.sock` gets its sockets in a short private
//! directory under `/tmp` instead — the one place every process of a user
//! sees the same way, whatever its environment — named by a stable hash of
//! the home's own bytes, so the daemon and every client agree on the place
//! without a pointer file or a running daemon to ask.

use std::env;
use std::path::{Path, PathBuf};

/// The longest path a Unix socket can have, excluding the terminating NUL.
#[cfg(any(target_os = "linux", target_os = "android"))]
pub const SOCKET_PATH_MAX: usize = 107;
/// The longest path a Unix socket can have, excluding the terminating NUL.
#[cfg(not(any(target_os = "linux", target_os = "android")))]
pub const SOCKET_PATH_MAX: usize = 103;

/// The host control socket's file name.
pub const HOST_SOCKET: &str = "agentd.sock";
/// The restricted, authenticated endpoint's file name.
pub const CONTAINER_SOCKET: &str = "container.sock";

/// `$AGENTDOCKER_HOME`, or `~/.agentdocker`.
pub fn default_home() -> PathBuf {
    if let Some(home) = env::var_os("AGENTDOCKER_HOME") {
        return PathBuf::from(home);
    }
    env::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".agentdocker")
}

/// Whether a path is short enough to name a Unix socket at all.
pub fn fits_socket(path: &Path) -> bool {
    path.as_os_str().len() <= SOCKET_PATH_MAX
}

/// Where a home's sockets live: the home itself when both socket names fit
/// there, else `agentdocker-<hash of the home>` under `/tmp` — never an
/// environment-dependent directory such as `$TMPDIR` or
/// `$XDG_RUNTIME_DIR`, which a service, a cron job and a shell can each
/// see differently. The hash is over the path's own bytes, so two homes
/// that differ only outside UTF-8 still get two directories. Deterministic,
/// so the daemon and its clients compute the same place independently.
pub fn socket_dir(home: &Path) -> PathBuf {
    if fits_socket(&home.join(CONTAINER_SOCKET)) && fits_socket(&home.join(HOST_SOCKET)) {
        return home.to_path_buf();
    }
    let hash = uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_URL,
        home.as_os_str().as_encoded_bytes(),
    )
    .simple()
    .to_string();
    runtime_dir().join(format!("agentdocker-{}", &hash[..12]))
}

/// Where the short socket directories live. `/tmp` is world-writable, so
/// the directory itself is created private and checked for ownership
/// before anything binds in it (see `agentdocker_host::dirs`).
fn runtime_dir() -> PathBuf {
    PathBuf::from("/tmp")
}

/// `$AGENTDOCKER_SOCKET`, or `agentd.sock` in the home's socket directory.
pub fn socket_path(home: &Path) -> PathBuf {
    if let Some(sock) = env::var_os("AGENTDOCKER_SOCKET") {
        return PathBuf::from(sock);
    }
    socket_dir(home).join(HOST_SOCKET)
}

/// The restricted authenticated endpoint, beside the host control socket in
/// the home's socket directory and never the same file.
pub fn container_socket(home: &Path) -> PathBuf {
    socket_dir(home).join(CONTAINER_SOCKET)
}

/// The lock that guarantees one daemon per socket: `agentd.sock` →
/// `agentd.lock`, beside it.
pub fn lock_path(socket: &Path) -> PathBuf {
    socket.with_extension("lock")
}

/// Where a daemon started by a client writes its output.
pub fn daemon_log(home: &Path) -> PathBuf {
    home.join("agentd.log")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_homes_get_a_short_stable_socket_directory() {
        let short = Path::new("/home/me/.agentdocker");
        assert_eq!(socket_dir(short), short);
        assert!(fits_socket(&container_socket(short)));

        let long = PathBuf::from(format!("/{}", "x".repeat(SOCKET_PATH_MAX)));
        assert!(!fits_socket(&long.join(CONTAINER_SOCKET)));
        let dir = socket_dir(&long);
        assert_ne!(dir, long);
        assert!(
            fits_socket(&dir.join(CONTAINER_SOCKET)),
            "{}",
            dir.display()
        );
        assert!(fits_socket(&dir.join(HOST_SOCKET)));
        assert_eq!(dir, socket_dir(&long), "deterministic");
        let other = PathBuf::from(format!("/{}", "y".repeat(SOCKET_PATH_MAX)));
        assert_ne!(dir, socket_dir(&other), "one directory per home");
        assert!(
            dir.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("agentdocker-")
        );
        assert_eq!(container_socket(&long), dir.join(CONTAINER_SOCKET));
        assert_ne!(container_socket(&long), dir.join(HOST_SOCKET));

        // Homes that differ only in bytes that are not UTF-8 are still
        // two homes.
        {
            use std::ffi::OsStr;
            use std::os::unix::ffi::OsStrExt;
            let mut a = b"/".to_vec();
            a.extend(std::iter::repeat_n(b'x', SOCKET_PATH_MAX));
            let mut b = a.clone();
            a.push(0xff);
            b.push(0xfe);
            let a = PathBuf::from(OsStr::from_bytes(&a));
            let b = PathBuf::from(OsStr::from_bytes(&b));
            assert_eq!(
                a.to_string_lossy(),
                b.to_string_lossy(),
                "lossy text agrees"
            );
            assert_ne!(socket_dir(&a), socket_dir(&b), "the directories do not");
        }

        // Exactly at the limit still fits; one more byte does not.
        let edge = PathBuf::from(format!("/{}", "a".repeat(SOCKET_PATH_MAX - 1)));
        assert!(fits_socket(&edge));
        assert!(!fits_socket(&edge.join("b")));
    }

    #[test]
    fn lock_sits_beside_the_socket() {
        assert_eq!(
            lock_path(Path::new("/home/me/.agentdocker/agentd.sock")),
            PathBuf::from("/home/me/.agentdocker/agentd.lock")
        );
        assert_eq!(
            lock_path(Path::new("/tmp/ad")),
            PathBuf::from("/tmp/ad.lock")
        );
    }
}
