//! Unix-socket server: newline-delimited JSON requests in, responses out.

use std::io::{self, SeekFrom};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use agentdocker_core::{ErrorCode, Request, Response, paths};
use agentdocker_host::dirs;
use anyhow::Context;
use tokio::fs::File;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncSeekExt, AsyncWriteExt, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::broadcast::error::RecvError;
use tracing::{debug, info, warn};

use crate::daemon::Daemon;

// The bundle itself is capped at IMPORT_BYTES. Allow a bounded amount of
// additional space for the request envelope, including its agent selector.
const HOST_REQUEST_BYTES: usize = agentdocker_core::handoff::IMPORT_BYTES + 64 * 1024;

struct Reader {
    inner: BufReader<OwnedReadHalf>,
    pending: Vec<u8>,
}

impl Reader {
    fn new(inner: OwnedReadHalf) -> Self {
        Self {
            inner: BufReader::new(inner),
            pending: Vec::new(),
        }
    }

    // Partial input belongs to the reader, so cancellation by a streaming
    // select does not discard it or reset the frame's byte budget.
    async fn next_line(&mut self) -> io::Result<Option<String>> {
        loop {
            let bytes = self.inner.fill_buf().await?;
            if bytes.is_empty() {
                return self.finish_line();
            }
            let newline = bytes.iter().position(|byte| *byte == b'\n');
            let count = newline.map_or(bytes.len(), |index| index + 1);
            if count > HOST_REQUEST_BYTES - self.pending.len() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("request frame exceeds {HOST_REQUEST_BYTES} bytes"),
                ));
            }
            self.pending.extend_from_slice(&bytes[..count]);
            self.inner.consume(count);
            if newline.is_some() {
                return self.finish_line();
            }
        }
    }

    fn finish_line(&mut self) -> io::Result<Option<String>> {
        if self.pending.is_empty() {
            return Ok(None);
        }
        let mut bytes = std::mem::take(&mut self.pending);
        if bytes.last() == Some(&b'\n') {
            bytes.pop();
            if bytes.last() == Some(&b'\r') {
                bytes.pop();
            }
        }
        String::from_utf8(bytes)
            .map(Some)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    }
}

pub async fn serve(daemon: Arc<Daemon>) -> anyhow::Result<()> {
    std::fs::create_dir_all(daemon.home.join("logs"))?;
    require_fits(&daemon.socket)?;
    prepare_socket_parent(&daemon.home, &daemon.socket)?;
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
    let mut reader = Reader::new(read_half);

    loop {
        let line = match reader.next_line().await {
            Ok(Some(line)) => line,
            Ok(None) => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::InvalidData => {
                return write(
                    &mut writer,
                    &Response::error(ErrorCode::Invalid, error.to_string()),
                )
                .await;
            }
            Err(error) => return Err(error),
        };
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
            Request::Events { replay, ready } => {
                return stream_events(&daemon, replay, ready, &mut reader, &mut writer).await;
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
    let _ = reader.inner.fill_buf().await;
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
    ready: bool,
    reader: &mut Reader,
    writer: &mut OwnedWriteHalf,
) -> io::Result<()> {
    let mut receiver = daemon.subscribe_events();
    if ready {
        write(writer, &Response::EventsReady).await?;
    }
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
    if let Some(record) = daemon.container_record(&id) {
        if follow {
            return write(
                writer,
                &Response::error(
                    ErrorCode::Invalid,
                    "container logs currently support snapshots; omit --follow",
                ),
            )
            .await;
        }
        match daemon.container_logs(record, tail).await {
            Ok(text) => {
                for line in text.lines() {
                    write(writer, &Response::Log { line: line.into() }).await?;
                }
                return write(writer, &Response::End).await;
            }
            Err(e) => {
                return write(
                    writer,
                    &Response::error(ErrorCode::EngineUnavailable, e.to_string()),
                )
                .await;
            }
        }
    }
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

/// A distinct socket prevents optional-token bypass through the host endpoint.
/// The restricted endpoint is optional: when it cannot be served the daemon
/// says so, marks container access off so new grants are refused, and
/// keeps serving the host socket. Never returns.
pub async fn restricted_endpoint(daemon: Arc<Daemon>, socket: PathBuf) {
    if let Err(err) = serve_restricted(daemon.clone(), socket.clone()).await {
        tracing::error!(%err, socket = %socket.display(), "restricted endpoint unavailable; container access is off");
        daemon.restricted_unavailable(format!("{err:#}"));
    }
    std::future::pending::<()>().await;
}

/// Serve the restricted endpoint on `socket`. Returns only on failure; the
/// daemon's main treats that as "container access is off", not as a
/// reason to stop serving the host socket.
pub async fn serve_restricted(daemon: Arc<Daemon>, socket: PathBuf) -> anyhow::Result<()> {
    if socket == daemon.socket {
        anyhow::bail!("restricted and host sockets must differ");
    }
    require_fits(&socket)?;
    prepare_socket_parent(&daemon.home, &socket)?;
    if socket.exists() {
        if UnixStream::connect(&socket).await.is_ok() {
            anyhow::bail!("restricted endpoint is already listening");
        }
        std::fs::remove_file(&socket)?;
    }
    let listener =
        UnixListener::bind(&socket).with_context(|| format!("cannot bind {}", socket.display()))?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
    info!(socket = %socket.display(), "restricted endpoint listening");
    daemon.restricted_listening(socket.clone());
    loop {
        let (stream, _) = listener.accept().await?;
        let daemon = daemon.clone();
        tokio::spawn(async move {
            // Bounded admission also limits unauthenticated, idle connections.
            let _ = tokio::time::timeout(
                Duration::from_secs(30),
                restricted_connection(daemon, stream),
            )
            .await;
        });
    }
}

/// A directory-mounted proxy reconnects to the restricted socket after daemon restart.
pub(crate) async fn serve_workspace(listener: UnixListener, target: std::path::PathBuf) {
    let mut delay = Duration::from_millis(10);
    loop {
        let (mut stream, _) = match listener.accept().await {
            Ok(connection) => {
                delay = Duration::from_millis(10);
                connection
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted
                        | io::ErrorKind::WouldBlock
                        | io::ErrorKind::ConnectionAborted
                ) || matches!(
                    error.raw_os_error(),
                    Some(nix::libc::ENFILE | nix::libc::EMFILE)
                ) =>
            {
                debug!(%error,"temporary workspace accept failure; retrying");
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(1));
                continue;
            }
            Err(error) => {
                warn!(%error,"workspace listener failed; reconciliation will rebuild it");
                return;
            }
        };
        let target = target.clone();
        tokio::spawn(async move {
            let _ = tokio::time::timeout(Duration::from_secs(30), async move {
                let mut upstream = UnixStream::connect(target).await?;
                tokio::io::copy_bidirectional(&mut stream, &mut upstream).await
            })
            .await;
        });
    }
}
/// A socket path the kernel would refuse is refused here first, with the
/// limit and the way out spelled out.
fn require_fits(socket: &Path) -> anyhow::Result<()> {
    if paths::fits_socket(socket) {
        return Ok(());
    }
    anyhow::bail!(
        "socket path {} is {} bytes; this OS allows {} — set AGENTDOCKER_SOCKET to a shorter path or use a shorter AGENTDOCKER_HOME",
        socket.display(),
        socket.as_os_str().len(),
        paths::SOCKET_PATH_MAX
    )
}

/// The socket's directory, created: the home's own is simply made, the
/// short fallback under the runtime directory must be ours alone.
fn prepare_socket_parent(home: &Path, socket: &Path) -> anyhow::Result<()> {
    let Some(parent) = socket.parent() else {
        return Ok(());
    };
    if parent == paths::socket_dir(home) && parent != home {
        dirs::ensure_private_dir(parent)
            .with_context(|| format!("socket directory {} is unusable", parent.display()))?;
    } else {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}

async fn restricted_frame(reader: &mut BufReader<UnixStream>) -> io::Result<Request> {
    let mut line = String::new();
    (&mut *reader)
        .take(1024 * 1024 + 1)
        .read_line(&mut line)
        .await?;
    if line.len() > 1024 * 1024 || !line.ends_with('\n') {
        return Err(io::Error::other("invalid frame length"));
    }
    serde_json::from_str(&line).map_err(io::Error::other)
}
async fn restricted_reply(
    reader: &mut BufReader<UnixStream>,
    response: &Response,
) -> io::Result<()> {
    let mut line = serde_json::to_vec(response).map_err(io::Error::other)?;
    line.push(b'\n');
    reader.get_mut().write_all(&line).await
}

async fn restricted_connection(daemon: Arc<Daemon>, stream: UnixStream) -> io::Result<()> {
    let mut reader = BufReader::new(stream);
    let token = match restricted_frame(&mut reader).await? {
        Request::Authenticate { token } => token,
        _ => {
            return restricted_reply(
                &mut reader,
                &Response::error(ErrorCode::Forbidden, "authentication required"),
            )
            .await;
        }
    };
    if let Err(response) = daemon.restricted_request(&token, Request::Ping) {
        return restricted_reply(&mut reader, &response).await;
    }
    restricted_reply(&mut reader, &Response::Ok).await?;
    let request = match daemon.restricted_request(&token, restricted_frame(&mut reader).await?) {
        Ok(request) => request,
        Err(response) => return restricted_reply(&mut reader, &response).await,
    };
    let response = daemon.handle(request).await;
    restricted_reply(&mut reader, &response).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentdocker_core::{AgentSpec, EventKind, LeaseMode};

    #[tokio::test]
    async fn host_frames_are_bounded_before_decoding_with_or_without_newline() {
        let tmp = tempfile::tempdir().unwrap();
        let daemon = Arc::new(Daemon::open(tmp.path().into(), tmp.path().join("sock")).unwrap());
        for newline in [false, true] {
            let (client, server) = UnixStream::pair().unwrap();
            let task = tokio::spawn(handle(daemon.clone(), server));
            let mut client = BufReader::new(client);
            let mut frame = vec![b' '; HOST_REQUEST_BYTES + 1];
            if newline {
                frame.push(b'\n');
            }
            let _ = client.get_mut().write_all(&frame).await;
            let mut reply = String::new();
            tokio::time::timeout(Duration::from_secs(2), client.read_line(&mut reply))
                .await
                .unwrap()
                .unwrap();
            assert!(matches!(
                serde_json::from_str::<Response>(&reply).unwrap(),
                Response::Error { code: ErrorCode::Invalid, message, .. }
                    if message.contains("request frame exceeds")
            ));
            task.await.unwrap().unwrap();
        }

        // A frame exactly at the limit remains valid, and the byte budget
        // resets for the next request on the same connection.
        let (client, server) = UnixStream::pair().unwrap();
        let task = tokio::spawn(handle(daemon, server));
        let mut client = BufReader::new(client);
        let mut frame = b"{\"op\":\"ping\"}".to_vec();
        frame.resize(HOST_REQUEST_BYTES - 1, b' ');
        frame.push(b'\n');
        client.get_mut().write_all(&frame).await.unwrap();
        for second in [false, true] {
            if second {
                client
                    .get_mut()
                    .write_all(b"{\"op\":\"ping\"}\n")
                    .await
                    .unwrap();
            }
            let mut reply = String::new();
            tokio::time::timeout(Duration::from_secs(2), client.read_line(&mut reply))
                .await
                .unwrap()
                .unwrap();
            assert!(matches!(
                serde_json::from_str::<Response>(&reply).unwrap(),
                Response::Pong { .. }
            ));
        }
        drop(client);
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn cancelled_host_read_retains_partial_input_and_its_byte_budget() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let (read, _write) = server.into_split();
        let mut reader = Reader::new(read);
        client.write_all(b"{\"op\":").await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(20), reader.next_line())
                .await
                .is_err()
        );
        client.write_all(b"\"ping\"}\r\n").await.unwrap();
        assert_eq!(
            reader.next_line().await.unwrap().unwrap(),
            "{\"op\":\"ping\"}"
        );

        let sending = tokio::spawn(async move {
            let _ = client.write_all(&vec![b' '; HOST_REQUEST_BYTES]).await;
            client
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(100), reader.next_line())
                .await
                .is_err()
        );
        let mut client = tokio::time::timeout(Duration::from_secs(2), sending)
            .await
            .expect("the frame writer must finish before checking cancellation")
            .unwrap();
        assert_eq!(reader.pending.len(), HOST_REQUEST_BYTES);
        client.write_all(b"\n").await.unwrap();
        assert_eq!(
            reader.next_line().await.unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[tokio::test]
    async fn restricted_socket_requires_auth_and_rechecks_revocation() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("checkout");
        std::fs::create_dir(&root).unwrap();
        let daemon =
            Arc::new(Daemon::open(tmp.path().join("state"), tmp.path().join("sock")).unwrap());
        daemon
            .handle(Request::Register {
                spec: AgentSpec {
                    name: "worker".into(),
                    workdir: Some(root),
                    ..AgentSpec::default()
                },
                pid: None,
            })
            .await;
        let Response::Access { token, grant, .. } = daemon
            .handle(Request::GrantAccess {
                agent: "worker".into(),
                container_root: "/workspace".into(),
                ttl_secs: 60,
            })
            .await
        else {
            panic!()
        };
        let (client, server) = UnixStream::pair().unwrap();
        let task = tokio::spawn(restricted_connection(daemon.clone(), server));
        let mut client = BufReader::new(client);
        client
            .get_mut()
            .write_all(b"{\"op\":\"ping\"}\n")
            .await
            .unwrap();
        let mut line = String::new();
        client.read_line(&mut line).await.unwrap();
        assert!(matches!(
            serde_json::from_str::<Response>(&line).unwrap(),
            Response::Error {
                code: ErrorCode::Forbidden,
                ..
            }
        ));
        task.await.unwrap().unwrap();
        let (client, server) = UnixStream::pair().unwrap();
        let task = tokio::spawn(restricted_connection(daemon.clone(), server));
        let mut client = BufReader::new(client);
        let auth = serde_json::to_string(&Request::Authenticate {
            token: token.clone(),
        })
        .unwrap()
            + "\n";
        client.get_mut().write_all(auth.as_bytes()).await.unwrap();
        line.clear();
        client.read_line(&mut line).await.unwrap();
        assert_eq!(
            serde_json::from_str::<Response>(&line).unwrap(),
            Response::Ok
        );
        // A valid credential must not allow an oversized operation through the
        // frame reader. The connection closes without acquiring its lease.
        let (oversized, server) = UnixStream::pair().unwrap();
        let oversized_task = tokio::spawn(restricted_connection(daemon.clone(), server));
        let mut oversized = BufReader::new(oversized);
        oversized
            .get_mut()
            .write_all(auth.as_bytes())
            .await
            .unwrap();
        let mut reply = String::new();
        oversized.read_line(&mut reply).await.unwrap();
        assert!(matches!(
            serde_json::from_str::<Response>(&reply).unwrap(),
            Response::Ok
        ));
        let frame = serde_json::to_string(&Request::Claim {
            agent: "worker".into(),
            resource: "path:/workspace/oversized".into(),
            mode: LeaseMode::Exclusive,
            ttl_secs: 60,
            wait_secs: 0,
            note: Some("x".repeat(1024 * 1024)),
        })
        .unwrap()
            + "\n";
        let _ = oversized.get_mut().write_all(frame.as_bytes()).await;
        reply.clear();
        let closed = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            oversized.read_line(&mut reply),
        )
        .await
        .unwrap();
        assert!(matches!(closed, Ok(0) | Err(_)));
        assert!(oversized_task.await.unwrap().is_err());
        assert!(
            matches!(daemon.handle(Request::Leases { agent: Some("worker".into()), resource: None }).await, Response::Leases { leases } if leases.is_empty())
        );

        daemon.handle(Request::RevokeAccess { grant }).await;
        client
            .get_mut()
            .write_all(b"{\"op\":\"ping\"}\n")
            .await
            .unwrap();
        line.clear();
        client.read_line(&mut line).await.unwrap();
        assert!(matches!(
            serde_json::from_str::<Response>(&line).unwrap(),
            Response::Error {
                code: ErrorCode::Forbidden,
                ..
            }
        ));
        task.await.unwrap().unwrap();
    }

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

    #[tokio::test]
    async fn a_restricted_endpoint_that_cannot_bind_turns_container_access_off() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("checkout");
        std::fs::create_dir(&root).unwrap();
        let daemon =
            Arc::new(Daemon::open(tmp.path().join("state"), tmp.path().join("sock")).unwrap());
        daemon
            .handle(Request::Register {
                spec: AgentSpec {
                    name: "worker".into(),
                    workdir: Some(root),
                    ..AgentSpec::default()
                },
                pid: None,
            })
            .await;
        // Too long for the kernel: refused up front, with the limit named.
        let long = PathBuf::from(format!("/tmp/{}.sock", "x".repeat(paths::SOCKET_PATH_MAX)));
        let err = serve_restricted(daemon.clone(), long.clone())
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("bytes"), "{err}");
        assert!(!long.exists());

        // Through the daemon's wrapper the host side keeps going: the
        // failure is announced, pinged as "off", and grants are refused.
        let mut events = daemon.subscribe_events();
        let endpoint = tokio::spawn(restricted_endpoint(daemon.clone(), long));
        let announced = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Ok(event) = events.recv().await
                    && matches!(event.kind, EventKind::RestrictedEndpointUnavailable { .. })
                {
                    return;
                }
            }
        })
        .await;
        assert!(
            announced.is_ok(),
            "restricted_endpoint_unavailable announced"
        );
        assert!(matches!(
            daemon.handle(Request::Ping).await,
            Response::Pong {
                restricted: None,
                ..
            }
        ));
        assert!(matches!(
            daemon
                .handle(Request::GrantAccess {
                    agent: "worker".into(),
                    container_root: "/workspace".into(),
                    ttl_secs: 60,
                })
                .await,
            Response::Error {
                code: ErrorCode::Unavailable,
                ..
            }
        ));
        endpoint.abort();

        // A socket that fits is served, announced, and reported.
        let good = tmp.path().join("container.sock");
        let mut events = daemon.subscribe_events();
        let endpoint = tokio::spawn(restricted_endpoint(daemon.clone(), good.clone()));
        let announced = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Ok(event) = events.recv().await
                    && matches!(&event.kind, EventKind::RestrictedEndpointListening { socket } if *socket == good)
                {
                    return;
                }
            }
        })
        .await;
        assert!(announced.is_ok(), "restricted_endpoint_listening announced");
        let up = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if matches!(daemon.handle(Request::Ping).await, Response::Pong { restricted: Some(ref s), .. } if *s == good) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        assert!(up.is_ok(), "restricted endpoint reported once serving");
        endpoint.abort();
    }
}
