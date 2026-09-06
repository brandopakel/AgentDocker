//! Minimal client for the agentd socket protocol.
//!
//! A client that cannot connect starts the daemon itself, the way
//! `ssh-agent` and `buildkitd` are started by their clients, unless
//! `AGENTDOCKER_NO_AUTOSTART` is set. See [`Client::with_start_timeout`].

use std::fs::OpenOptions;
use std::future::Future;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use agentdocker_core::{Request, Response, paths};
use agentdocker_host::{dirs, lock};
use anyhow::{Context, Result, bail};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

/// How long the CLI waits for a daemon it started. Hooks use less: they
/// fail open and must not stall the editor.
const START_TIMEOUT: Duration = Duration::from_secs(3);

pub struct Client {
    socket: PathBuf,
    /// How long to wait for a daemon this client starts; `None` never
    /// starts one.
    autostart: Option<Duration>,
}

impl Client {
    pub fn new(socket: Option<PathBuf>) -> Self {
        let socket = socket.unwrap_or_else(|| paths::socket_path(&dirs::home()));
        let disabled = std::env::var_os("AGENTDOCKER_TOKEN_FILE").is_some()
            || std::env::var_os("AGENTDOCKER_NO_AUTOSTART")
                .is_some_and(|value| !value.is_empty() && value != "0");
        Self {
            socket,
            autostart: if disabled { None } else { Some(START_TIMEOUT) },
        }
    }

    /// Wait this long for a daemon started on demand; `None` disables
    /// starting one. `AGENTDOCKER_NO_AUTOSTART` still wins.
    pub fn with_start_timeout(mut self, timeout: Option<Duration>) -> Self {
        if self.autostart.is_some() {
            self.autostart = timeout;
        }
        self
    }

    async fn connect(&self, request: &Request) -> Result<BufReader<UnixStream>> {
        if !paths::fits_socket(&self.socket) {
            bail!(
                "socket path {} is {} bytes; this OS allows {} — set AGENTDOCKER_SOCKET to a shorter path or use a shorter AGENTDOCKER_HOME",
                self.socket.display(),
                self.socket.as_os_str().len(),
                paths::SOCKET_PATH_MAX
            );
        }
        dirs::check_socket_parent(&self.socket).with_context(|| {
            format!("socket directory for {} is unusable", self.socket.display())
        })?;
        let stream = match UnixStream::connect(&self.socket).await {
            Ok(stream) => stream,
            Err(err) if absent(&err) && self.autostart.is_some() => {
                self.start_daemon(self.autostart.unwrap_or(START_TIMEOUT))
                    .await?
            }
            Err(err) => {
                return Err(err).with_context(|| {
                    format!(
                        "cannot reach agentd at {} (start it with `agentd`)",
                        self.socket.display()
                    )
                });
            }
        };
        let mut reader = BufReader::new(stream);
        if let Some(path) = std::env::var_os("AGENTDOCKER_TOKEN_FILE") {
            let token = std::fs::read_to_string(path)
                .context("cannot read restricted endpoint token file")?;
            let auth = serde_json::to_string(&Request::Authenticate {
                token: token.trim().to_owned(),
            })? + "\n";
            reader.get_mut().write_all(auth.as_bytes()).await?;
            let mut response = String::new();
            if reader.read_line(&mut response).await? == 0 {
                bail!("agentd closed the connection without answering");
            }
            if !matches!(
                into_result(serde_json::from_str::<Response>(&response)?)?,
                Response::Ok
            ) {
                bail!("restricted endpoint authentication failed");
            }
        }
        let mut line = serde_json::to_string(request)?;
        line.push('\n');
        reader.get_mut().write_all(line.as_bytes()).await?;
        Ok(reader)
    }

    /// Start `agentd` for this socket if nobody has, then wait for it to
    /// listen. The lock beside the socket says whether a daemon exists:
    /// getting it means none does (it is released again at once, because
    /// the daemon must take it itself); not getting it means one is up or
    /// starting, so only the wait is needed. Two clients racing here may
    /// both spawn a daemon; the loser exits when it finds the lock taken.
    async fn start_daemon(&self, timeout: Duration) -> Result<UnixStream> {
        let home = dirs::home();
        let lock_path = paths::lock_path(&self.socket);
        if let Some(parent) = lock_path.parent() {
            if parent == paths::socket_dir(&home) && parent != home {
                dirs::ensure_private_dir(parent).with_context(|| {
                    format!("socket directory {} is unusable", parent.display())
                })?;
            } else {
                std::fs::create_dir_all(parent)?;
            }
        }
        let vacant = lock::try_exclusive(&lock_path)
            .with_context(|| format!("cannot take {}", lock_path.display()))?
            .is_some();
        let log_path = paths::daemon_log(&home);
        // Our own child, when we started one: its exit is known at once,
        // so a daemon that dies on startup fails this call in milliseconds
        // with its last words, not after the whole timeout.
        let child = if vacant {
            Some(spawn_agentd(&self.socket, &home)?)
        } else {
            None
        };
        wait_for_start(&self.socket, &log_path, timeout, child).await
    }

    /// Send one request and read exactly one response. Error responses
    /// become `Err`.
    pub async fn call(&self, request: &Request) -> Result<Response> {
        into_result(self.call_raw(request).await?)
    }

    /// Like [`Client::call`], but hands back [`Response::Error`] as a value
    /// so the caller can act on its code. Only transport failures are `Err`.
    pub async fn call_raw(&self, request: &Request) -> Result<Response> {
        let mut reader = self.connect(request).await?;
        let mut line = String::new();
        if reader.read_line(&mut line).await? == 0 {
            bail!("agentd closed the connection without answering");
        }
        Ok(serde_json::from_str(&line)?)
    }

    /// Send one request and feed every response to `on_response` until the
    /// daemon ends the stream or `on_response` returns `false`.
    pub async fn stream(
        &self,
        request: &Request,
        mut on_response: impl FnMut(Response) -> Result<bool>,
    ) -> Result<()> {
        self.stream_after(request, async { Ok(()) }, |(), response| {
            on_response(response)
        })
        .await
    }

    /// Like [`Client::stream`], but events wait for the daemon's subscription
    /// acknowledgement before polling `then`. Its snapshot can therefore be
    /// deduplicated against the live tail without a subscription race.
    /// Falling behind returns an error, so incomplete output is never silent.
    pub async fn stream_after<T>(
        &self,
        request: &Request,
        then: impl Future<Output = Result<T>>,
        mut on_response: impl FnMut(&T, Response) -> Result<bool>,
    ) -> Result<()> {
        let subscription = match request {
            Request::Events { replay, .. } => Request::Events {
                replay: *replay,
                ready: true,
            },
            other => other.clone(),
        };
        let mut reader = self.connect(&subscription).await?;
        if matches!(subscription, Request::Events { .. }) {
            let mut line = String::new();
            if reader.read_line(&mut line).await? == 0 {
                bail!("agentd closed the connection before subscription readiness");
            }
            if !matches!(
                into_result(serde_json::from_str(&line)?)?,
                Response::EventsReady
            ) {
                bail!("agentd did not acknowledge event subscription readiness; update the daemon");
            }
        }
        let snapshot = then.await?;
        let mut line = String::new();
        loop {
            line.clear();
            if reader.read_line(&mut line).await? == 0 {
                return Ok(());
            }
            match into_result(serde_json::from_str(&line)?)? {
                Response::End => return Ok(()),
                Response::Lagged { skipped } => {
                    if matches!(request, Request::Events { .. }) {
                        bail!(
                            "event stream lost {skipped} events; rerun journal with --since to recover retained entries"
                        );
                    } else {
                        eprintln!(
                            "agentdocker: skipped {skipped} live messages; reconnect to resume delivery and ask senders to resend missing payloads (live messages cannot be replayed)"
                        );
                    }
                }
                response => {
                    if !on_response(&snapshot, response)? {
                        return Ok(());
                    }
                }
            }
        }
    }
}

/// Retain startup ownership until the daemon accepts a connection. A failed or
/// cancelled startup reaps only the child this client created.
struct StartingChild(Option<std::process::Child>);
impl Drop for StartingChild {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

async fn wait_for_start(
    socket: &Path,
    log_path: &Path,
    timeout: Duration,
    child: Option<std::process::Child>,
) -> Result<UnixStream> {
    let mut child = StartingChild(child);
    let deadline = Instant::now() + timeout;
    loop {
        dirs::check_socket_parent(socket)?;
        match UnixStream::connect(socket).await {
            Ok(stream) => {
                child.0.take();
                return Ok(stream);
            }
            Err(err) if absent(&err) && Instant::now() < deadline => {
                if let Some(status) = child.0.as_mut().and_then(|c| c.try_wait().ok().flatten()) {
                    bail!(
                        "agentd exited ({status}) before listening on {}:\n{}",
                        socket.display(),
                        log_tail(log_path, 6)
                    );
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(err) => {
                return Err(err).with_context(|| {
                    format!(
                        "agentd did not come up at {} within {timeout:?} (see {}):\n{}",
                        socket.display(),
                        log_path.display(),
                        log_tail(log_path, 6)
                    )
                });
            }
        }
    }
}

/// No socket file, or nobody listening on it.
fn absent(err: &std::io::Error) -> bool {
    matches!(
        err.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
    )
}

/// Launch `agentd` detached: its own process group, stdio on the daemon
/// log, the same home as this client. The binary beside ours is preferred
/// so a build in `target/` starts the matching daemon; else `PATH`.
fn spawn_agentd(socket: &Path, home: &Path) -> Result<std::process::Child> {
    let exe = std::env::current_exe()
        .ok()
        .and_then(|me| me.parent().map(|dir| dir.join("agentd")))
        .filter(|sibling| sibling.is_file())
        .unwrap_or_else(|| PathBuf::from("agentd"));
    std::fs::create_dir_all(home)?;
    let log_path = paths::daemon_log(home);
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("cannot open {}", log_path.display()))?;
    Command::new(&exe)
        .arg("--socket")
        .arg(socket)
        .arg("--home")
        .arg(home)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .process_group(0)
        .spawn()
        .with_context(|| format!("cannot start {}", exe.display()))
}

/// How much of the daemon log's end is read for an error message.
const LOG_TAIL_BYTES: u64 = 16 * 1024;

/// The last `lines` of the daemon log, for an error message; empty when
/// there is no log. Only the log's last [`LOG_TAIL_BYTES`] are read, so a
/// log that grew for months costs nothing here.
fn log_tail(path: &Path, lines: usize) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut file) = std::fs::File::open(path) else {
        return String::new();
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(LOG_TAIL_BYTES);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    let mut bytes = Vec::new();
    if file.read_to_end(&mut bytes).is_err() {
        return String::new();
    }
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    if start > 0 {
        // The window opened mid-line; that fragment is not a line.
        match text.find('\n') {
            Some(cut) => {
                text.drain(..=cut);
            }
            None => return String::new(),
        }
    }
    let all: Vec<&str> = text.lines().collect();
    let first = all.len().saturating_sub(lines);
    all[first..]
        .iter()
        .map(|line| format!("  {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// How adapters (MCP server, hooks) reach agentd. Abstracted so their logic
/// can be tested without a daemon. Daemon-level errors come back as
/// [`Response::Error`]; only transport failures are `Err`.
pub trait Backend {
    fn call(&self, request: Request) -> impl Future<Output = Result<Response>>;
}

impl Backend for Client {
    async fn call(&self, request: Request) -> Result<Response> {
        self.call_raw(&request).await
    }
}

fn into_result(response: Response) -> Result<Response> {
    match response {
        Response::Error {
            code,
            message,
            details,
        } => {
            let mut text = format!("{message} ({code:?})");
            if let Some(details) = details {
                text.push('\n');
                text.push_str(&serde_json::to_string_pretty(&details)?);
            }
            bail!(text)
        }
        other => Ok(other),
    }
}

#[cfg(test)]
pub mod mock {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use agentdocker_core::{Request, Response};
    use anyhow::Result;

    use super::Backend;

    /// Records requests and replays canned responses in order, answering
    /// `Response::Ok` once they run out.
    #[derive(Default)]
    pub struct Mock {
        pub requests: Mutex<Vec<Request>>,
        pub responses: Mutex<VecDeque<Response>>,
    }

    impl Mock {
        pub fn with(responses: Vec<Response>) -> Self {
            Self {
                requests: Mutex::new(Vec::new()),
                responses: Mutex::new(responses.into()),
            }
        }

        pub fn requests(&self) -> Vec<Request> {
            self.requests.lock().unwrap().clone()
        }
    }

    impl Backend for Mock {
        fn call(&self, request: Request) -> impl Future<Output = Result<Response>> {
            self.requests.lock().unwrap().push(request);
            let response = self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Response::Ok);
            async move { Ok(response) }
        }
    }
}

#[cfg(test)]
mod stream_tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use tokio::net::UnixListener;

    #[tokio::test]
    async fn snapshot_waits_for_subscription_acknowledgement() {
        let tmp = tempfile::tempdir().unwrap();
        let socket = tmp.path().join("sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let polled = Arc::new(AtomicBool::new(false));
        let snapshot_polled = polled.clone();
        let client = Client::new(Some(socket)).with_start_timeout(None);
        let task = tokio::spawn(async move {
            client
                .stream_after(
                    &Request::Events {
                        replay: 0,
                        ready: false,
                    },
                    async move {
                        snapshot_polled.store(true, Ordering::SeqCst);
                        Ok(17)
                    },
                    |snapshot, response| {
                        assert_eq!(*snapshot, 17);
                        assert!(matches!(response, Response::Ok));
                        Ok(false)
                    },
                )
                .await
        });
        let (stream, _) = listener.accept().await.unwrap();
        let mut peer = BufReader::new(stream);
        let mut line = String::new();
        peer.read_line(&mut line).await.unwrap();
        assert!(matches!(
            serde_json::from_str::<Request>(&line).unwrap(),
            Request::Events { ready: true, .. }
        ));
        // The request has reached the server, but the snapshot must stay unpolled.
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(!polled.load(Ordering::SeqCst));
        for response in [Response::EventsReady, Response::Ok] {
            peer.get_mut()
                .write_all((serde_json::to_string(&response).unwrap() + "\n").as_bytes())
                .await
                .unwrap();
        }
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(polled.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn lost_event_stream_returns_failure_instead_of_incomplete_success() {
        let tmp = tempfile::tempdir().unwrap();
        let socket = tmp.path().join("sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let client = Client::new(Some(socket)).with_start_timeout(None);
        let task = tokio::spawn(async move {
            client
                .stream(
                    &Request::Events {
                        replay: 0,
                        ready: true,
                    },
                    |_| Ok(true),
                )
                .await
        });
        let (stream, _) = listener.accept().await.unwrap();
        let mut peer = BufReader::new(stream);
        let mut line = String::new();
        peer.read_line(&mut line).await.unwrap();
        for response in [Response::EventsReady, Response::Lagged { skipped: 3 }] {
            peer.get_mut()
                .write_all((serde_json::to_string(&response).unwrap() + "\n").as_bytes())
                .await
                .unwrap();
        }
        let error = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert!(error.to_string().contains("lost 3 events"));
    }

    #[test]
    fn log_tail_reads_only_the_end_and_whole_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("agentd.log");
        let body: String = (0..5000)
            .map(|i| format!("line {i} {}\n", "x".repeat(20)))
            .collect();
        assert!(body.len() as u64 > LOG_TAIL_BYTES);
        std::fs::write(&log, &body).unwrap();
        let tail = log_tail(&log, 3);
        let lines: Vec<&str> = tail.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with("  line 4997 "), "{tail}");
        assert!(lines[2].starts_with("  line 4999 "), "{tail}");
        assert!(
            lines.iter().all(|l| l.ends_with(&"x".repeat(20))),
            "whole lines only"
        );
        assert_eq!(log_tail(&tmp.path().join("missing"), 3), "");
        std::fs::write(&log, "one\ntwo\n").unwrap();
        assert_eq!(log_tail(&log, 5), "  one\n  two");
    }

    #[tokio::test]
    async fn untrusted_existing_fallback_socket_is_rejected_without_connecting() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("h".repeat(120));
        let parent = paths::socket_dir(&home);
        dirs::ensure_private_dir(&parent).unwrap();
        let socket = parent.join(paths::HOST_SOCKET);
        let listener = UnixListener::bind(&socket).unwrap();
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o777)).unwrap();
        let err = Client::new(Some(socket.clone()))
            .with_start_timeout(None)
            .call(&Request::Ping)
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("writable by others"), "{err:#}");
        assert!(
            tokio::time::timeout(Duration::from_millis(30), listener.accept())
                .await
                .is_err()
        );
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700)).unwrap();
        drop(listener);
        std::fs::remove_file(socket).unwrap();
        std::fs::remove_dir(parent).unwrap();
    }

    #[tokio::test]
    async fn startup_timeout_reaps_our_child_and_success_releases_it() {
        let tmp = tempfile::tempdir().unwrap();
        let socket = tmp.path().join("sock");
        let log = tmp.path().join("log");
        let child = Command::new("/bin/sleep").arg("30").spawn().unwrap();
        let pid = child.id();
        assert!(
            wait_for_start(&socket, &log, Duration::from_millis(30), Some(child))
                .await
                .is_err()
        );
        assert_eq!(
            unsafe { libc::kill(pid as i32, 0) },
            -1,
            "timed-out child was reaped"
        );
        let listener = UnixListener::bind(&socket).unwrap();
        let child = Command::new("/bin/sleep").arg("30").spawn().unwrap();
        let pid = child.id();
        let stream = wait_for_start(&socket, &log, Duration::from_secs(1), Some(child))
            .await
            .unwrap();
        assert_eq!(
            unsafe { libc::kill(pid as i32, 0) },
            0,
            "successful daemon stays alive"
        );
        unsafe {
            libc::kill(pid as i32, libc::SIGKILL);
            libc::waitpid(pid as i32, std::ptr::null_mut(), 0);
        }
        drop(stream);
        drop(listener);
    }

    #[tokio::test]
    async fn a_socket_path_too_long_for_the_os_fails_fast_and_says_why() {
        let long = PathBuf::from(format!("/tmp/{}.sock", "x".repeat(paths::SOCKET_PATH_MAX)));
        let client = Client::new(Some(long));
        let started = Instant::now();
        let err = client.call(&Request::Ping).await.unwrap_err().to_string();
        assert!(err.contains("bytes"), "{err}");
        assert!(err.contains("AGENTDOCKER_SOCKET"), "{err}");
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "no daemon was waited for"
        );
    }
}
