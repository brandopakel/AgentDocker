//! A blocking client for the daemon's NDJSON protocol, enough for a window
//! that asks questions from a worker thread and follows the event stream
//! from another. Starts the daemon on demand the way the CLI does.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use agentdocker_core::{Event, Request, Response, paths};
use anyhow::{Context, Result, bail};

#[derive(Clone, Debug)]
pub struct Client {
    socket: PathBuf,
}

const START_TIMEOUT: Duration = Duration::from_secs(3);
const CALL_TIMEOUT: Duration = Duration::from_secs(10);

impl Client {
    pub fn from_env() -> Self {
        Self {
            socket: paths::socket_path(&paths::default_home()),
        }
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// One request, one reply; an error reply is an `Err`.
    pub fn call(&self, request: &Request) -> Result<Response> {
        let stream = self.connect()?;
        stream.set_read_timeout(Some(CALL_TIMEOUT))?;
        let mut reader = BufReader::new(stream);
        let mut line = serde_json::to_string(request)?;
        line.push('\n');
        reader.get_mut().write_all(line.as_bytes())?;
        let mut reply = String::new();
        if reader.read_line(&mut reply)? == 0 {
            bail!("agentd closed the connection without answering");
        }
        match serde_json::from_str::<Response>(&reply)? {
            Response::Error { message, .. } => bail!("{message}"),
            response => Ok(response),
        }
    }

    /// Follow the event stream until the daemon ends it, the connection
    /// drops, or `on` returns `false`.
    pub fn events(&self, replay: usize, mut on: impl FnMut(Event) -> bool) -> Result<()> {
        let stream = self.connect()?;
        let mut reader = BufReader::new(stream);
        let mut line = serde_json::to_string(&Request::Events {
            replay,
            ready: true,
        })?;
        line.push('\n');
        reader.get_mut().write_all(line.as_bytes())?;
        let mut buffer = String::new();
        loop {
            buffer.clear();
            if reader.read_line(&mut buffer)? == 0 {
                return Ok(());
            }
            match serde_json::from_str::<Response>(&buffer)? {
                Response::Event { event } => {
                    if !on(event) {
                        return Ok(());
                    }
                }
                Response::End => return Ok(()),
                Response::Error { message, .. } => bail!("{message}"),
                _ => {}
            }
        }
    }

    /// Connect, starting the daemon when nobody listens, as the CLI does.
    fn connect(&self) -> Result<UnixStream> {
        match UnixStream::connect(&self.socket) {
            Ok(stream) => return Ok(stream),
            Err(err) if absent(&err) => {}
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("cannot reach agentd at {}", self.socket.display()));
            }
        }
        spawn_agentd(&self.socket)?;
        let deadline = Instant::now() + START_TIMEOUT;
        loop {
            match UnixStream::connect(&self.socket) {
                Ok(stream) => return Ok(stream),
                Err(err) if absent(&err) && Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(err) => {
                    return Err(err).with_context(|| {
                        format!("agentd did not come up at {}", self.socket.display())
                    });
                }
            }
        }
    }
}

fn absent(err: &std::io::Error) -> bool {
    matches!(
        err.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
    )
}

/// The `agentd` beside this binary, else on `PATH`, detached with its
/// output on the daemon log.
fn spawn_agentd(socket: &Path) -> Result<()> {
    let exe = std::env::current_exe()
        .ok()
        .and_then(|me| me.parent().map(|dir| dir.join("agentd")))
        .filter(|sibling| sibling.is_file())
        .unwrap_or_else(|| PathBuf::from("agentd"));
    let home = paths::default_home();
    std::fs::create_dir_all(&home)?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(paths::daemon_log(&home))?;
    Command::new(&exe)
        .arg("--socket")
        .arg(socket)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .spawn()
        .with_context(|| format!("cannot start {}", exe.display()))?;
    Ok(())
}
