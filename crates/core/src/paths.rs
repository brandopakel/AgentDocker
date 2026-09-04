//! Filesystem locations shared by the daemon and its clients.

use std::env;
use std::path::{Path, PathBuf};

/// `$AGENTDOCKER_HOME`, or `~/.agentdocker`.
pub fn default_home() -> PathBuf {
    if let Some(home) = env::var_os("AGENTDOCKER_HOME") {
        return PathBuf::from(home);
    }
    env::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".agentdocker")
}

/// `$AGENTDOCKER_SOCKET`, or `<home>/agentd.sock`.
pub fn socket_path(home: &Path) -> PathBuf {
    if let Some(sock) = env::var_os("AGENTDOCKER_SOCKET") {
        return PathBuf::from(sock);
    }
    home.join("agentd.sock")
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
