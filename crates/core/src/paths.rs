//! Filesystem locations shared by the daemon and its clients.
//!
//! Unix socket names are short by law — `sun_path` holds 104 bytes on
//! macOS and the BSDs, 108 on Linux — while a home directory can be as long
//! as anyone likes. The rules here keep the two apart: a home whose path
//! leaves no room for `container.sock` gets its sockets in a short private
//! directory instead — under `$XDG_RUNTIME_DIR` where a session manager
//! provides one, else `/tmp` — named by a stable hash of the home's own
//! bytes, so the daemon and every client, whatever their environment,
//! agree on the place without a pointer file or a running daemon to ask.

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
/// there, else `agentdocker-<hash of the home>` under the runtime directory
/// (`$XDG_RUNTIME_DIR`, else `/tmp` — never a per-shell `$TMPDIR`, which
/// would let two environments of one user disagree). The hash is over the
/// path's own bytes, so two homes that differ only outside UTF-8 still get
/// two directories. Deterministic, so the daemon and its clients compute
/// the same place independently.
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

/// Workspace state needs more socket room than the public endpoints (including
/// OpenSSH's temporary control-socket suffix). Keep credentials private per home.
pub fn workspace_dir(home: &Path) -> PathBuf {
    let candidate = home.join("mounts");
    let longest = candidate
        .join(format!("bridge-{}", "0".repeat(32)))
        .join(format!("ctl.{}", "0".repeat(16)));
    if longest.as_os_str().len() + 8 <= 103 {
        return candidate;
    }
    let hash = uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_URL,
        home.as_os_str().as_encoded_bytes(),
    )
    .simple()
    .to_string();
    runtime_dir().join(format!("adw-{}", &hash[..12]))
}

/// Persistent linked checkouts are siblings of daemon state, never inside it.
pub fn worktree_dir(home: &Path) -> PathBuf {
    let mut name = home.as_os_str().to_os_string();
    name.push(".worktrees");
    PathBuf::from(name)
}

/// The user's runtime directory for short-lived, private files: the
/// session manager's when it names one, else `/tmp`.
fn runtime_dir() -> PathBuf {
    env::var_os("XDG_RUNTIME_DIR")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
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
    fn workspace_paths_reserve_the_ssh_temporary_suffix_and_stay_outside_worktrees() {
        for home in [
            PathBuf::from("/tmp/ad"),
            PathBuf::from(format!("/tmp/{}", "long".repeat(40))),
        ] {
            let mounts = workspace_dir(&home);
            let control = mounts
                .join(format!("bridge-{}", "a".repeat(32)))
                .join(format!("ctl.{}", "b".repeat(16)));
            assert!(
                control.as_os_str().len() + 8 <= 103,
                "{}",
                control.display()
            );
            assert_eq!(workspace_dir(&home), mounts);
            assert!(!worktree_dir(&home).starts_with(&home));
            assert_ne!(worktree_dir(&home), mounts);
        }
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
