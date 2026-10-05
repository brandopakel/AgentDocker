//! Keep cleanup alive when the terminal-facing invocation is killed.
//!
//! The front end holds one authenticated loopback connection. Its child owns
//! the provider processes and observes EOF before performing ordinary cleanup.
//! No terminal input is proxied. Unix also reserves the owner's process group
//! until its children are retired, including when the owner itself crashes.
#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;
use agentdocker_core::ProcessIdentity;
use agentdocker_host::{dirs, procinfo};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{net::Ipv4Addr, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    process::Command,
    task::JoinSet,
    time::timeout,
};
#[cfg(windows)]
pub(super) use windows::contain_owner_until_process_exit;

pub(super) const ENV: &str = "AGENTDOCKER_NATIVE_LIFETIME";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Invitation {
    owner: ProcessIdentity,
    port: u16,
    nonce: String,
    directory: std::path::PathBuf,
}

#[derive(Debug)]
pub(super) struct Connection {
    pub stream: TcpStream,
    pub directory: std::path::PathBuf,
}

fn owner_matches(owner: &ProcessIdentity) -> bool {
    owner.pid != 0
        && owner.pid != std::process::id()
        && procinfo::inspect(std::process::id()).is_some_and(|p| p.ppid == owner.pid)
        && procinfo::start_time(owner.pid) == Some(owner.started_at)
        && matches!(
            (procinfo::executable_path_of(owner.pid), procinfo::executable_path()),
            (Ok(parent), Ok(child)) if parent == child
        )
        && procinfo::start_time(owner.pid) == Some(owner.started_at)
}

/// Called only in the child, before any provider or capability is created.
pub(super) async fn connect(value: &std::ffi::OsStr) -> Result<Connection> {
    let value = value
        .to_str()
        .context("native lifetime invitation is not UTF-8")?;
    ensure!(
        value.len() <= 8192,
        "native lifetime invitation exceeds limit"
    );
    let invitation: Invitation =
        serde_json::from_str(value).context("invalid native lifetime invitation")?;
    ensure!(
        invitation.port != 0
            && invitation.nonce.len() == 32
            && invitation.nonce.bytes().all(|b| b.is_ascii_hexdigit())
            && owner_matches(&invitation.owner),
        "native lifetime owner is not this invocation's exact live parent"
    );
    let parent = dirs::home().join("codex-native").canonicalize()?;
    ensure!(
        invitation.directory.is_absolute()
            && invitation.directory.parent() == Some(parent.as_path())
            && invitation.directory.canonicalize()? == invitation.directory
            && invitation
                .directory
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| { n.len() == 32 && n.bytes().all(|b| b.is_ascii_hexdigit()) }),
        "native lifetime directory is not an owned private launch directory"
    );
    let stream = timeout(Duration::from_secs(10), async {
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, invitation.port)).await?;
        stream.write_all(invitation.nonce.as_bytes()).await?;
        ensure!(
            stream.read_u8().await? == 1,
            "native lifetime handshake refused"
        );
        ensure!(
            owner_matches(&invitation.owner),
            "native lifetime owner exited during startup"
        );
        Ok::<_, anyhow::Error>(stream)
    })
    .await
    .context("native lifetime handshake timed out")??;
    Ok(Connection {
        stream,
        directory: invitation.directory,
    })
}

async fn accept(listener: &TcpListener, nonce: &str) -> Result<TcpStream> {
    timeout(Duration::from_secs(10), async {
        // Unauthenticated local peers must not serialize the owner's startup.
        // Bound both concurrent reads and their lifetime; dropping this set
        // cancels every remaining read and closes its socket on all exits.
        let mut pending = JoinSet::new();
        loop {
            tokio::select! {
                accepted = listener.accept(), if pending.len() < 64 => {
                    let (mut stream, peer) = accepted?;
                    ensure!(peer.ip().is_loopback(), "native lifetime peer is not local");
                    let expected = nonce.as_bytes().to_vec();
                    pending.spawn(async move {
                        let mut offered = [0; 32];
                        match timeout(Duration::from_secs(1), stream.read_exact(&mut offered)).await {
                            Ok(Ok(_)) if offered == expected.as_slice() => Some(stream),
                            _ => None,
                        }
                    });
                }
                completed = pending.join_next(), if !pending.is_empty() => {
                    if let Some(Ok(Some(mut stream))) = completed {
                        stream.write_u8(1).await?;
                        return Ok(stream);
                    }
                }
            }
        }
    })
    .await
    .context("native lifetime child did not connect")?
}

pub(super) async fn disconnected(stream: &mut TcpStream) -> Result<()> {
    let mut byte = [0];
    ensure!(
        stream.read(&mut byte).await? == 0,
        "unexpected native lifetime data"
    );
    Ok(())
}

/// Spawn the same loaded binary with the exact original arguments. The child
/// inherits the terminal directly; only this private control socket is new.
pub(super) async fn supervise(client: &crate::client::Client) -> Result<()> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let parent = dirs::home().join("codex-native");
    dirs::ensure_private_dir(&parent)?;
    let directory = parent.join(uuid::Uuid::new_v4().simple().to_string());
    dirs::ensure_private_dir(&directory)?;
    let directory = directory.canonicalize()?;
    // Both participants own revocation. The child still revokes after front-end
    // death; the front end can revoke even if its child dies before binding.
    let _capability = tempfile::TempPath::try_from_path(directory.join("capability"))?;
    let owner = ProcessIdentity {
        pid: std::process::id(),
        started_at: procinfo::start_time(std::process::id())
            .context("native lifetime owner birth unavailable")?,
    };
    let invitation = Invitation {
        owner,
        port: listener.local_addr()?.port(),
        nonce: uuid::Uuid::new_v4().simple().to_string(),
        directory,
    };
    let mut command = Command::new(procinfo::executable_path()?);
    command
        .args(std::env::args_os().skip(1))
        .env(ENV, serde_json::to_string(&invitation)?)
        .env("AGENTDOCKER_HOME", dirs::home())
        .env("AGENTDOCKER_SOCKET", client.socket_path())
        .env_remove("AGENTDOCKER_AGENT_ID")
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        // It must survive the front end long enough to retire its own children.
        .kill_on_drop(false);
    let mut child = Process::spawn(command).context("cannot start native lifetime owner")?;
    let result = tokio::select! {
        _ = super::stop_signal() => Ok(()),
        result = async {
            let stream = tokio::select! {
                result = accept(&listener, &invitation.nonce) => result?,
                exited = child.exited() => {
                    exited?;
                    anyhow::bail!("native lifetime owner exited before binding");
                },
            };
            drop(listener);
            child.exited().await?;
            // On graceful stop, EOF asks the owner to execute the same bounded
            // cleanup as terminal exit. SIGKILL closes this descriptor in-kernel.
            drop(stream);
            Ok(())
        } => result,
    };
    timeout(Duration::from_secs(30), child.exited())
        .await
        .context("native lifetime owner cleanup did not finish")??;
    let exited = child.finish().await?;
    ensure!(exited.success(), "native lifetime owner cleanup failed");
    result
}

#[cfg(unix)]
use unix::Process;

#[cfg(windows)]
struct Process(tokio::process::Child);
#[cfg(windows)]
impl Process {
    fn spawn(mut command: Command) -> Result<Self> {
        Ok(Self(command.spawn()?))
    }
    async fn exited(&mut self) -> Result<()> {
        self.0.wait().await?;
        Ok(())
    }
    async fn finish(&mut self) -> Result<std::process::ExitStatus> {
        Ok(self.0.wait().await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn only_matching_handshake_opens_lifetime_and_frontend_drop_ends_it() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let nonce = "0123456789abcdef0123456789abcdef";
        let accepting = tokio::spawn(async move { accept(&listener, nonce).await.unwrap() });
        let mut stranger = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
            .await
            .unwrap();
        stranger.write_all(&[b'x'; 32]).await.unwrap();
        let mut byte = [0];
        assert_eq!(stranger.read(&mut byte).await.unwrap(), 0);
        let mut child = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
            .await
            .unwrap();
        child.write_all(nonce.as_bytes()).await.unwrap();
        assert_eq!(child.read_u8().await.unwrap(), 1);
        let frontend = accepting.await.unwrap();
        assert!(
            timeout(Duration::from_millis(30), disconnected(&mut child))
                .await
                .is_err()
        );
        drop(frontend);
        timeout(Duration::from_secs(1), disconnected(&mut child))
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn stalled_handshakes_do_not_delay_the_owner_and_are_closed_after_acceptance() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let nonce = "0123456789abcdef0123456789abcdef";
        let accepting = tokio::spawn(async move { accept(&listener, nonce).await });
        let mut stalled = Vec::new();
        for _ in 0..16 {
            let mut peer = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
                .await
                .unwrap();
            peer.write_u8(b'x').await.unwrap();
            stalled.push(peer);
        }
        let mut child = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
            .await
            .unwrap();
        child.write_all(nonce.as_bytes()).await.unwrap();
        let acknowledgement = timeout(Duration::from_secs(2), child.read_u8()).await;
        if acknowledgement.is_err() {
            accepting.abort();
        }
        assert_eq!(acknowledgement.unwrap().unwrap(), 1);
        let frontend = accepting.await.unwrap().unwrap();
        for mut peer in stalled {
            let mut byte = [0];
            assert_eq!(
                timeout(Duration::from_secs(2), peer.read(&mut byte))
                    .await
                    .unwrap()
                    .unwrap(),
                0
            );
        }
        drop(frontend);
        timeout(Duration::from_secs(1), disconnected(&mut child))
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn forged_owner_is_refused_before_connecting() {
        let value = serde_json::to_string(&Invitation {
            owner: ProcessIdentity {
                pid: std::process::id(),
                started_at: chrono::Utc::now(),
            },
            port: 1,
            nonce: "0123456789abcdef0123456789abcdef".into(),
            directory: std::path::PathBuf::from("forged"),
        })
        .unwrap();
        assert!(
            connect(std::ffi::OsStr::new(&value))
                .await
                .unwrap_err()
                .to_string()
                .contains("exact live parent")
        );
    }
}
