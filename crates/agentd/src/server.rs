//! Unix-socket server: newline-delimited JSON requests in, responses out.

use std::io::{self, SeekFrom};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use agentdocker_core::{ErrorCode, Request, Response};
use anyhow::Context;
use tokio::fs::File;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncSeekExt, AsyncWriteExt, BufReader, Lines};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::broadcast::error::RecvError;
use tracing::{debug, info, warn};

use crate::daemon::Daemon;

type Reader = Lines<BufReader<OwnedReadHalf>>;

pub async fn serve(daemon: Arc<Daemon>) -> anyhow::Result<()> {
    std::fs::create_dir_all(daemon.home.join("logs"))?;
    if let Some(parent) = daemon.socket.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if daemon.socket.exists() {
        if UnixStream::connect(&daemon.socket).await.is_ok() {
            anyhow::bail!(
                "another agentd is already listening on {}",
                daemon.socket.display()
            );
        }
        std::fs::remove_file(&daemon.socket)?;
    }
    let listener = UnixListener::bind(&daemon.socket)
        .with_context(|| format!("cannot bind {}", daemon.socket.display()))?;
    std::fs::set_permissions(&daemon.socket, std::fs::Permissions::from_mode(0o600))?;
    info!(socket = %daemon.socket.display(), home = %daemon.home.display(), "agentd listening");

    loop {
        let (stream, _) = listener.accept().await?;
        let daemon = daemon.clone();
        tokio::spawn(async move {
            if let Err(err) = handle(daemon, stream).await {
                debug!(%err, "connection closed with error");
            }
        });
    }
}

async fn handle(daemon: Arc<Daemon>, stream: UnixStream) -> io::Result<()> {
    let (read_half, mut writer) = stream.into_split();
    let mut reader = BufReader::new(read_half).lines();

    while let Some(line) = reader.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let request: Request = match serde_json::from_str(&line) {
            Ok(request) => request,
            Err(err) => {
                let response =
                    Response::error(ErrorCode::Invalid, format!("malformed request: {err}"));
                write(&mut writer, &response).await?;
                continue;
            }
        };
        // Streaming requests own the connection until it closes.
        match request {
            Request::Subscribe { agent, topics } => {
                return stream_messages(&daemon, agent, topics, &mut reader, &mut writer).await;
            }
            Request::Events { replay } => {
                return stream_events(&daemon, replay, &mut reader, &mut writer).await;
            }
            Request::Logs {
                agent,
                follow,
                tail,
            } => return stream_logs(&daemon, &agent, follow, tail, &mut reader, &mut writer).await,
            unary => {
                let response = if matches!(&unary, Request::Claim { .. }) {
                    tokio::select! {
                        biased;
                        () = claim_eof(&mut reader) => return Ok(()),
                        response = daemon.handle(unary) => response,
                    }
                } else {
                    daemon.handle(unary).await
                };
                write(&mut writer, &response).await?;
            }
        }
    }
    Ok(())
}

async fn write(writer: &mut OwnedWriteHalf, response: &Response) -> io::Result<()> {
    let mut line = serde_json::to_string(response).map_err(io::Error::other)?;
    line.push('\n');
    writer.write_all(line.as_bytes()).await
}

/// A pending claim owns the connection. EOF or additional input cancels it;
/// clients must await its response before sending another request. This also
/// bounds memory and cancels a peer that closes after sending partial input.
async fn claim_eof(reader: &mut Reader) {
    let _ = reader.get_mut().fill_buf().await;
}

/// Resolves when the client closes its side (or sends garbage).
async fn client_closed(reader: &mut Reader) {
    loop {
        match reader.next_line().await {
            Ok(Some(_)) => continue,
            _ => return,
        }
    }
}

async fn stream_messages(
    daemon: &Arc<Daemon>,
    agent: Option<String>,
    topics: Vec<String>,
    reader: &mut Reader,
    writer: &mut OwnedWriteHalf,
) -> io::Result<()> {
    let (mut subscription, mut receiver) = match daemon.subscribe(agent.as_deref(), topics) {
        Ok(opened) => opened,
        Err(response) => return write(writer, &response).await,
    };
    for message in subscription.take_backlog() {
        write(writer, &Response::Message { message }).await?;
    }
    loop {
        tokio::select! {
            () = client_closed(reader) => break,
            received = receiver.recv() => match received {
                Ok(message) => {
                    if subscription.wants(&message) {
                        write(writer, &Response::Message { message }).await?;
                    }
                }
                Err(RecvError::Lagged(skipped)) => {
                    warn!(skipped, "subscriber fell behind the message bus");
                    write(writer, &Response::Lagged { skipped }).await?;
                }
                Err(RecvError::Closed) => break,
            },
        }
    }
    Ok(())
}

async fn stream_events(
    daemon: &Arc<Daemon>,
    replay: usize,
    reader: &mut Reader,
    writer: &mut OwnedWriteHalf,
) -> io::Result<()> {
    let mut receiver = daemon.subscribe_events();
    // Subscribe first so nothing is missed, then drop live events the replay
    // already covered: seqs are strictly increasing.
    let replayed = daemon.recent_events(replay);
    let last_replayed = replayed.last().map(|event| event.seq);
    for event in replayed {
        write(writer, &Response::Event { event }).await?;
    }
    loop {
        tokio::select! {
            () = client_closed(reader) => break,
            received = receiver.recv() => match received {
                Ok(event) => {
                    if event.seq != 0 && last_replayed.is_some_and(|seen| event.seq <= seen) {
                        continue;
                    }
                    write(writer, &Response::Event { event }).await?;
                }
                Err(RecvError::Lagged(skipped)) => {
                    warn!(skipped, "event subscriber fell behind");
                    write(writer, &Response::Lagged { skipped }).await?;
                }
                Err(RecvError::Closed) => break,
            },
        }
    }
    Ok(())
}

async fn stream_logs(
    daemon: &Arc<Daemon>,
    agent: &str,
    follow: bool,
    tail: usize,
    reader: &mut Reader,
    writer: &mut OwnedWriteHalf,
) -> io::Result<()> {
    let id = match daemon.resolve(agent) {
        Ok(id) => id,
        Err(response) => return write(writer, &response).await,
    };
    let path = daemon.log_path(&id);
    let (mut offset, existing) = read_from(&path, 0).await;
    let lines: Vec<&str> = existing.lines().collect();
    let start = if tail == 0 {
        0
    } else {
        lines.len().saturating_sub(tail)
    };
    for line in &lines[start..] {
        write(
            writer,
            &Response::Log {
                line: (*line).to_owned(),
            },
        )
        .await?;
    }
    if !follow {
        return write(writer, &Response::End).await;
    }

    let mut pending = String::new();
    let mut quiet_after_exit = 0;
    let mut ticker = tokio::time::interval(Duration::from_millis(250));
    loop {
        tokio::select! {
            () = client_closed(reader) => break,
            _ = ticker.tick() => {
                let (read, chunk) = read_from(&path, offset).await;
                offset += read;
                pending.push_str(&chunk);
                while let Some(newline) = pending.find('\n') {
                    let line = pending[..newline].to_owned();
                    pending.drain(..=newline);
                    write(writer, &Response::Log { line }).await?;
                }
                if read == 0 && !daemon.is_live(&id) {
                    // Give the log writer a couple of ticks to flush after exit.
                    quiet_after_exit += 1;
                    if quiet_after_exit >= 2 {
                        if !pending.is_empty() {
                            write(writer, &Response::Log { line: std::mem::take(&mut pending) }).await?;
                        }
                        write(writer, &Response::End).await?;
                        break;
                    }
                }
            }
        }
    }
    Ok(())
}

/// Read everything after `offset`. Returns bytes consumed and the text.
async fn read_from(path: &Path, offset: u64) -> (u64, String) {
    let Ok(mut file) = File::open(path).await else {
        return (0, String::new());
    };
    if file.seek(SeekFrom::Start(offset)).await.is_err() {
        return (0, String::new());
    }
    let mut bytes = Vec::new();
    if file.read_to_end(&mut bytes).await.is_err() {
        return (0, String::new());
    }
    let read = bytes.len() as u64;
    (read, String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentdocker_core::{AgentSpec, EventKind, LeaseMode};

    #[tokio::test]
    async fn disconnected_waiter_never_acquires_after_release() {
        let tmp = tempfile::TempDir::new().unwrap();
        let daemon = Arc::new(Daemon::open(tmp.path().into(), tmp.path().join("sock")).unwrap());
        for name in ["holder", "waiter"] {
            daemon
                .handle(Request::Register {
                    spec: AgentSpec {
                        name: name.into(),
                        ..AgentSpec::default()
                    },
                    pid: None,
                })
                .await;
        }
        daemon
            .handle(Request::Claim {
                agent: "holder".into(),
                resource: "task:wait".into(),
                mode: LeaseMode::Exclusive,
                ttl_secs: 60,
                note: None,
                wait_secs: 0,
            })
            .await;
        let mut events = daemon.subscribe_events();
        let (mut client, server) = UnixStream::pair().unwrap();
        let running = tokio::spawn(handle(daemon.clone(), server));
        client.write_all(b"{\"op\":\"claim\",\"agent\":\"waiter\",\"resource\":\"task:wait\",\"wait_secs\":5}\n").await.unwrap();
        while !matches!(
            events.recv().await.unwrap().kind,
            EventKind::LeaseConflict { .. }
        ) {}
        // Partial pipelined input must not hide the subsequent disconnect.
        client.write_all(b"{\"op\":").await.unwrap();
        drop(client);
        tokio::time::timeout(Duration::from_secs(1), running)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        daemon
            .handle(Request::ReleaseAll {
                agent: "holder".into(),
                summary: None,
                summary_source: agentdocker_core::SummarySource::Explicit,
            })
            .await;
        assert!(
            matches!(daemon.handle(Request::Leases{agent:Some("waiter".into()),resource:None}).await,Response::Leases{leases} if leases.is_empty())
        );
    }
}
