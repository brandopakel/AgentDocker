//! Minimal client for the agentd socket protocol.
//!
//! A client that cannot connect starts the daemon itself, the way
//! `ssh-agent` and `buildkitd` are started by their clients, unless
//! `AGENTDOCKER_NO_AUTOSTART` is set. See [`Client::with_start_timeout`].

use std::fs::OpenOptions;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use agentdocker_core::{Request, Response, paths};
use agentdocker_host::lock;
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
        let socket = socket.unwrap_or_else(|| paths::socket_path(&paths::default_home()));
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
        let lock_path = paths::lock_path(&self.socket);
        if let Some(parent) = lock_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let vacant = lock::try_exclusive(&lock_path)
            .with_context(|| format!("cannot take {}", lock_path.display()))?
            .is_some();
        if vacant {
            spawn_agentd(&self.socket)?;
        }
        let deadline = Instant::now() + timeout;
        loop {
            match UnixStream::connect(&self.socket).await {
                Ok(stream) => return Ok(stream),
                Err(err) if absent(&err) && Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                Err(err) => {
                    return Err(err).with_context(|| {
                        format!(
                            "agentd did not come up at {} within {timeout:?} (see {})",
                            self.socket.display(),
                            paths::daemon_log(&paths::default_home()).display()
                        )
                    });
                }
            }
        }
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
        let mut reader = self.connect(request).await?;
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
                        eprintln!(
                            "agentdocker: skipped {skipped} events; use events --replay to recover retained event history"
                        );
                    } else {
                        eprintln!(
                            "agentdocker: skipped {skipped} live messages; reconnect to resume delivery and ask senders to resend missing payloads (live messages cannot be replayed)"
                        );
                    }
                }
                response => {
                    if !on_response(response)? {
                        return Ok(());
                    }
                }
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
fn spawn_agentd(socket: &Path) -> Result<()> {
    let exe = std::env::current_exe()
        .ok()
        .and_then(|me| me.parent().map(|dir| dir.join("agentd")))
        .filter(|sibling| sibling.is_file())
        .unwrap_or_else(|| PathBuf::from("agentd"));
    let home = paths::default_home();
    std::fs::create_dir_all(&home)?;
    let log_path = paths::daemon_log(&home);
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("cannot open {}", log_path.display()))?;
    Command::new(&exe)
        .arg("--socket")
        .arg(socket)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .process_group(0)
        .spawn()
        .with_context(|| format!("cannot start {}", exe.display()))?;
    Ok(())
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
