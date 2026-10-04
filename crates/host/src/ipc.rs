//! Local native IPC. Unix uses filesystem sockets; Windows uses private named
//! pipes with explicit ACL and peer checks before application bytes are sent.
#[cfg(unix)]
pub use std::os::unix::net::UnixStream as BlockingStream;
#[cfg(unix)]
pub use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
#[cfg(unix)]
pub use tokio::net::{UnixListener as Listener, UnixStream as Stream};
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{BlockingStream, Listener, OwnedReadHalf, OwnedWriteHalf, Stream};

/// Kernel-reported process ID of this connected, same-user peer. Application
/// protocols must still verify process birth and their own session binding.
pub fn peer_pid(stream: &Stream) -> std::io::Result<u32> {
    #[cfg(unix)]
    {
        let peer = stream.peer_cred()?;
        // SAFETY: geteuid has no preconditions or pointer arguments.
        if peer.uid() != unsafe { libc::geteuid() } {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "local peer belongs to another user",
            ));
        }
        peer.pid()
            .and_then(|pid| u32::try_from(pid).ok())
            .filter(|pid| *pid != 0)
            .ok_or_else(|| std::io::Error::other("local peer process identity unavailable"))
    }
    #[cfg(windows)]
    stream.peer_pid()
}

#[cfg(unix)]
pub async fn pair() -> std::io::Result<(Stream, Stream)> {
    Stream::pair()
}
#[cfg(windows)]
pub async fn pair() -> std::io::Result<(Stream, Stream)> {
    windows::pair().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn peer_pid_child_fixture() {
        let Some(path) = std::env::var_os("AGENTDOCKER_TEST_PEER_PID_PIPE") else {
            return;
        };
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            let mut stream = Stream::connect(std::path::PathBuf::from(path))
                .await
                .unwrap();
            stream.write_u32(std::process::id()).await.unwrap();
            let expected = stream.read_u32().await.unwrap();
            assert_eq!(peer_pid(&stream).unwrap(), expected);
            assert_ne!(expected, std::process::id());
            stream.write_u8(1).await.unwrap();
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn peer_pid_binds_both_ends_to_the_real_process() {
        #[cfg(unix)]
        let root = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        let path = root.path().join("peer.sock");
        #[cfg(windows)]
        let path = std::path::PathBuf::from(format!(
            r"\\.\pipe\agentdocker-peer-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let listener = Listener::bind(&path).unwrap();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "ipc::tests::peer_pid_child_fixture",
                "--nocapture",
            ])
            .env("AGENTDOCKER_TEST_PEER_PID_PIPE", &path)
            .stdin(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let result = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            let (mut stream, _) = listener.accept().await.unwrap();
            assert_eq!(peer_pid(&stream).unwrap(), child.id());
            assert_ne!(peer_pid(&stream).unwrap(), std::process::id());
            assert_eq!(stream.read_u32().await.unwrap(), child.id());
            stream.write_u32(std::process::id()).await.unwrap();
            assert_eq!(stream.read_u8().await.unwrap(), 1);
        })
        .await;
        if result.is_err() {
            let _ = child.kill();
        }
        let status = child.wait().unwrap();
        result.unwrap();
        assert!(status.success());
    }
}
