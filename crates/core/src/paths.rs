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
