//! Private local endpoints and host-wide monotonic deadlines for native input.
use super::ledger;
use agentdocker_host::ipc;
#[cfg(unix)]
use anyhow::Context;
use anyhow::{Result, ensure};
use std::path::{Path, PathBuf};

pub(super) fn endpoint(home: &Path, agent: &str, kind: &str) -> Result<PathBuf> {
    ensure!(
        matches!(kind, "hook" | "resolve"),
        "unknown native input endpoint"
    );
    let directory = ledger::directory(home, agent)?;
    #[cfg(unix)]
    return Ok(directory.join(format!("{kind}.sock")));
    #[cfg(windows)]
    {
        let directory = directory.canonicalize()?;
        let id = uuid::Uuid::new_v5(
            &uuid::Uuid::NAMESPACE_URL,
            directory.as_os_str().as_encoded_bytes(),
        );
        Ok(PathBuf::from(format!(
            r"\\.\pipe\agentdocker-codex-{}-{kind}",
            id.simple()
        )))
    }
}

pub(super) struct Listener {
    socket: ipc::Listener,
    #[cfg(unix)]
    path: PathBuf,
}

impl Listener {
    /// The caller owns the ledger lifetime lock before reserving the endpoint.
    pub fn bind(home: &Path, agent: &str, kind: &str) -> Result<Self> {
        let path = endpoint(home, agent, kind)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
            match std::fs::symlink_metadata(&path) {
                Ok(meta) => {
                    // SAFETY: geteuid has no preconditions.
                    ensure!(
                        meta.file_type().is_socket() && meta.uid() == unsafe { libc::geteuid() },
                        "unsafe native input endpoint"
                    );
                    std::fs::remove_file(&path)?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                Err(error) => return Err(error.into()),
            }
            let socket = ipc::Listener::bind(&path)?;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
            Ok(Self { socket, path })
        }
        #[cfg(windows)]
        {
            // Private ACL and first-instance ownership are checked by IPC.
            Ok(Self {
                socket: ipc::Listener::bind(&path)?,
            })
        }
    }

    pub async fn accept(&self) -> std::io::Result<ipc::Stream> {
        self.socket.accept().await.map(|value| value.0)
    }
}

#[cfg(unix)]
impl Drop for Listener {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// A clock shared by all processes on this host, never wall-clock timestamps.
pub(super) fn monotonic_millis() -> Result<u64> {
    #[cfg(unix)]
    {
        let mut now = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: valid timespec output pointer and supported clock selector.
        ensure!(
            unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) } == 0,
            "native hook monotonic clock unavailable"
        );
        u64::try_from(now.tv_sec)?
            .checked_mul(1000)
            .and_then(|s| s.checked_add((now.tv_nsec / 1_000_000) as u64))
            .context("native hook deadline overflow")
    }
    #[cfg(windows)]
    {
        // SAFETY: no pointers or preconditions. Windows includes sleep time,
        // so a hook which spans sleep expires rather than gaining a new budget.
        Ok(unsafe { windows_sys::Win32::System::SystemInformation::GetTickCount64() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn endpoint_separates_homes_agents_and_recovery() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let home = a.path().canonicalize().unwrap();
        let path = endpoint(&home, "agent", "hook").unwrap();
        assert_ne!(path, endpoint(b.path(), "agent", "hook").unwrap());
        assert_ne!(path, endpoint(&home, "other", "hook").unwrap());
        assert_ne!(path, endpoint(&home, "agent", "resolve").unwrap());
        assert!(endpoint(&home, "../outside", "hook").is_err());
        assert!(endpoint(&home, "agent", "../outside").is_err());
        let listener = Listener::bind(&home, "agent", "hook").unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            let mut client = ipc::Stream::connect(&path).await.unwrap();
            let mut server = listener.accept().await.unwrap();
            assert_eq!(ipc::peer_pid(&client).unwrap(), std::process::id());
            assert_eq!(ipc::peer_pid(&server).unwrap(), std::process::id());
            client.write_u8(42).await.unwrap();
            assert_eq!(server.read_u8().await.unwrap(), 42);
        })
        .await
        .unwrap();
    }
}
