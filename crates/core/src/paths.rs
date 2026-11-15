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

/// The state directory's name under a user's home on Unix
/// (`~/.agentdocker`); under `%LOCALAPPDATA%` on Windows it is `agentdocker`.
/// Which of those applies, and whether `$AGENTDOCKER_HOME` overrides it, is
/// the host's to read: `agentdocker_host::dirs::default_home`. Core only
/// derives paths from a home it is given.
pub const HOME_DIR_NAME: &str = ".agentdocker";
/// The Windows state directory's name under `%LOCALAPPDATA%`.
pub const WINDOWS_HOME_DIR_NAME: &str = "agentdocker";

/// Whether a path is short enough to name a Unix socket at all.
#[cfg(unix)]
pub fn fits_socket(path: &Path) -> bool {
    path.as_os_str().len() <= SOCKET_PATH_MAX
}

/// Windows control endpoints are local named pipes, bounded in UTF-16 units.
#[cfg(windows)]
pub fn fits_socket(path: &Path) -> bool {
    let Some(text) = path.to_str() else {
        return false;
    };
    text.strip_prefix(r"\\.\pipe\").is_some_and(|name| {
        !name.is_empty() && !name.contains(['\\', '/', '\0']) && text.encode_utf16().count() <= 256
    })
}

/// Where a home's sockets live: the home itself when both socket names fit
/// there, else `agentdocker-<hash of the home>` under `/tmp` — never an
/// environment-dependent directory such as `$TMPDIR` or
/// `$XDG_RUNTIME_DIR`, which a service, a cron job and a shell can each
/// see differently. The hash is over the path's own bytes, so two homes
/// that differ only outside UTF-8 still get two directories. Deterministic,
/// so the daemon and its clients compute the same place independently.
pub fn socket_dir(home: &Path) -> PathBuf {
    #[cfg(windows)]
    return home.to_owned();
    #[cfg(unix)]
    socket_dir_in(home, &runtime_dir())
}

#[cfg(unix)]
fn socket_dir_in(home: &Path, runtime: &Path) -> PathBuf {
    if fits_socket(&home.join(CONTAINER_SOCKET)) && fits_socket(&home.join(HOST_SOCKET)) {
        return home.to_path_buf();
    }
    short_socket_dir_in(home, runtime)
}

/// The short private directory a home's sockets fall back to, whether or
/// not the home itself would have fitted: for socket names longer than the
/// daemon's own, such as per-agent session sockets.
#[cfg(unix)]
pub fn short_socket_dir(home: &Path) -> PathBuf {
    short_socket_dir_in(home, &runtime_dir())
}

#[cfg(unix)]
fn short_socket_dir_in(home: &Path, runtime: &Path) -> PathBuf {
    let hash = uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_URL,
        home.as_os_str().as_encoded_bytes(),
    )
    .simple()
    .to_string();
    let name = format!("agentdocker-{}", &hash[..12]);
    let candidate = runtime.join(&name);
    if fits_socket(&candidate.join(CONTAINER_SOCKET)) && fits_socket(&candidate.join(HOST_SOCKET)) {
        return candidate;
    }
    Path::new("/tmp").join(name)
}

/// Windows has no socket-name limit to fall back from.
#[cfg(windows)]
pub fn short_socket_dir(home: &Path) -> PathBuf {
    home.to_path_buf()
}

/// Workspace state needs more socket room than the public endpoints (including
/// OpenSSH's temporary control-socket suffix). Keep credentials private per home.
pub fn workspace_dir(home: &Path) -> PathBuf {
    #[cfg(windows)]
    return home.join("mounts");
    #[cfg(unix)]
    workspace_dir_in(home, &runtime_dir())
}
#[cfg(unix)]
const WORKSPACE_SOCKET_MAX: usize = 103;
#[cfg(unix)]
const CANONICAL_PREFIX_ALLOWANCE: usize = 8;
#[cfg(unix)]
fn workspace_fits(root: &Path) -> bool {
    let longest = root
        .join(format!("bridge-{}", "0".repeat(32)))
        .join(format!("ctl.{}", "0".repeat(16)));
    longest.as_os_str().len() + CANONICAL_PREFIX_ALLOWANCE <= WORKSPACE_SOCKET_MAX
}
#[cfg(unix)]
fn workspace_dir_in(home: &Path, runtime: &Path) -> PathBuf {
    let candidate = home.join("mounts");
    if workspace_fits(&candidate) {
        return candidate;
    }
    let hash = uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_URL,
        home.as_os_str().as_encoded_bytes(),
    )
    .simple()
    .to_string();
    let name = format!("adw-{}", &hash[..12]);
    let candidate = runtime.join(&name);
    if workspace_fits(&candidate) {
        return candidate;
    }
    Path::new("/tmp").join(name)
}

/// Where a project keeps the checkouts made for its agents, relative to
/// its root, unless the daemon's settings name another place: one folder
/// inside the project, so that everything of a project lives in the
/// folder a person cloned or downloaded. `.agentdocker/` may hold files
/// the project tracks (its policy), so only this folder under it is kept
/// out of git, by the repository's own `info/exclude`.
pub const PROJECT_WORKTREES: &str = ".agentdocker/worktrees";

/// The folder a worktree made for `name` gets inside the worktrees
/// folder: a branch's or an agent's name with everything that is not an
/// ASCII letter, a digit, `-`, `_` or `.` turned into `-`, so
/// `claude/fix-login` is `claude-fix-login`. Never empty, never hidden,
/// never a step up, and no longer than a folder name may be anywhere.
pub fn worktree_folder(name: &str) -> String {
    let folder: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let folder: String = folder
        .trim_start_matches(['.', '-'])
        .trim_end_matches(['.', '-'])
        .chars()
        .take(100)
        .collect();
    if folder.is_empty() {
        "worktree".to_owned()
    } else {
        folder
    }
}

/// Where the short socket directories live. `/tmp` is world-writable, so
/// the directory itself is created private and checked for ownership
/// before anything binds in it (see `agentdocker_host::dirs`).
#[cfg(unix)]
fn runtime_dir() -> PathBuf {
    PathBuf::from("/tmp")
}

/// `agentd.sock` in the home's socket directory. The `$AGENTDOCKER_SOCKET`
/// override is the host's to read: `agentdocker_host::dirs::socket_path`.
pub fn socket_path(home: &Path) -> PathBuf {
    #[cfg(windows)]
    return pipe_name(home, "host");
    #[cfg(unix)]
    socket_dir(home).join(HOST_SOCKET)
}

/// The restricted authenticated endpoint, beside the host control socket in
/// the home's socket directory and never the same file.
pub fn container_socket(home: &Path) -> PathBuf {
    #[cfg(windows)]
    return pipe_name(home, "restricted");
    #[cfg(unix)]
    socket_dir(home).join(CONTAINER_SOCKET)
}

#[cfg(windows)]
pub(crate) fn pipe_name(home: &Path, channel: &str) -> PathBuf {
    let id = uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_URL,
        home.as_os_str().as_encoded_bytes(),
    );
    PathBuf::from(format!(r"\\.\pipe\agentdocker-{}-{channel}", id.simple()))
}

/// Windows pipe names have no filesystem parent. Keep the daemon lock in its
/// explicit state directory, independent of the caller's inherited home.
pub fn daemon_lock(home: &Path, socket: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let name = socket.to_string_lossy().to_lowercase();
        let id = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, name.as_bytes());
        home.join(format!("agentd-{}.lock", id.simple()))
    }
    #[cfg(unix)]
    {
        let _ = home;
        lock_path(socket)
    }
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

/// The earlier half of a captured log: `<name>.log.1` beside `<name>.log`,
/// where a writer that reached its cap moved what it had written so far.
pub fn rotated_log(log: &Path) -> PathBuf {
    let mut name = log
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(".1");
    log.with_file_name(name)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn the_rotated_log_sits_beside_the_live_one() {
        assert_eq!(
            rotated_log(Path::new("/home/me/.agentdocker/logs/abc.log")),
            PathBuf::from("/home/me/.agentdocker/logs/abc.log.1")
        );
    }

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
                control.as_os_str().len() + CANONICAL_PREFIX_ALLOWANCE <= WORKSPACE_SOCKET_MAX,
                "{}",
                control.display()
            );
            assert_eq!(workspace_dir(&home), mounts);
        }
    }

    #[test]
    fn an_overlong_runtime_directory_also_falls_back() {
        let home = PathBuf::from(format!("/tmp/{}", "h".repeat(120)));
        let runtime = PathBuf::from(format!("/tmp/{}", "r".repeat(120)));
        let sockets = socket_dir_in(&home, &runtime);
        assert_eq!(sockets.parent(), Some(Path::new("/tmp")));
        assert!(fits_socket(&sockets.join(CONTAINER_SOCKET)));
        let workspace = workspace_dir_in(&home, &runtime);
        assert!(workspace_fits(&workspace));
        assert_eq!(workspace.parent(), Some(Path::new("/tmp")));
    }

    #[test]
    fn a_worktrees_folder_is_named_for_its_branch_or_agent() {
        assert_eq!(worktree_folder("claude/fix-login"), "claude-fix-login");
        assert_eq!(worktree_folder("agent/reviewer-1"), "agent-reviewer-1");
        assert_eq!(worktree_folder("v1.2_hotfix"), "v1.2_hotfix");
        // Never hidden, never a step up, never empty, never unbounded.
        assert_eq!(worktree_folder("../../etc"), "etc");
        assert_eq!(worktree_folder(".hidden"), "hidden");
        assert_eq!(worktree_folder("///"), "worktree");
        assert_eq!(worktree_folder(""), "worktree");
        assert_eq!(worktree_folder("caf\u{e9} au lait"), "caf--au-lait");
        assert_eq!(worktree_folder(&"x".repeat(300)).len(), 100);
        assert!(!worktree_folder("a/../b").contains('/'));
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

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    #[test]
    fn pipes_are_local_bounded_and_separated_from_state_locks() {
        let home = PathBuf::from(format!(
            r"C:\Users\fixture\{}\agentdocker",
            "long".repeat(100)
        ));
        let host = pipe_name(&home, "host");
        let restricted = container_socket(&home);
        assert!(fits_socket(&host));
        assert!(fits_socket(&restricted));
        assert_ne!(host, restricted);
        assert_eq!(host, pipe_name(&home, "host"));
        assert_ne!(host, pipe_name(&home.join("another"), "host"));
        assert_eq!(daemon_lock(&home, &host).parent(), Some(home.as_path()));
        assert_eq!(
            daemon_lock(&home, &host),
            daemon_lock(&home, &PathBuf::from(host.to_str().unwrap().to_uppercase()))
        );
        for invalid in [
            r"\\server\pipe\remote",
            r"C:\state\agentd.sock",
            r"\\.\pipe\",
            r"\\.\pipe\nested\path",
        ] {
            assert!(!fits_socket(Path::new(invalid)));
        }
    }
}
