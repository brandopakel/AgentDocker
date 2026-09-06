//! A blocking client for the daemon's NDJSON protocol, enough for a window
//! that asks questions from a worker thread and follows the event stream
//! from another. Starts the daemon on demand the way the CLI does.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use agentdocker_core::{Event, Request, Response, paths};
use agentdocker_host::{dirs, lock};
use anyhow::{Context, Result, bail};

#[derive(Clone, Debug)]
pub struct Client {
    socket: PathBuf,
    home: PathBuf,
    autostart: bool,
}

const START_TIMEOUT: Duration = Duration::from_secs(3);
const CALL_TIMEOUT: Duration = Duration::from_secs(10);
/// The window has two threads that reconnect on their own schedules; one
/// daemon start attempt per this long is enough for both.
const START_COOLDOWN: Duration = Duration::from_secs(15);

/// When the last start attempt was made, whichever thread made it.
static LAST_SPAWN: Mutex<Option<Instant>> = Mutex::new(None);

/// Whether this caller may try to start the daemon now.
fn may_spawn() -> bool {
    let mut last = LAST_SPAWN.lock().unwrap_or_else(|e| e.into_inner());
    let now = Instant::now();
    if last.is_some_and(|at| now.duration_since(at) < START_COOLDOWN) {
        return false;
    }
    *last = Some(now);
    true
}

impl Client {
    pub fn from_env() -> Self {
        let home = dirs::home();
        Self {
            socket: paths::socket_path(&home),
            home,
            autostart: !std::env::var_os("AGENTDOCKER_NO_AUTOSTART")
                .is_some_and(|value| !value.is_empty() && value != "0"),
        }
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// One request, one reply; an error reply is an `Err`.
    pub fn call(&self, request: &Request) -> Result<Response> {
        let stream = self.connect()?;
        stream.set_read_timeout(Some(CALL_TIMEOUT))?;
        stream.set_write_timeout(Some(CALL_TIMEOUT))?;
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
    pub fn events(
        &self,
        replay: usize,
        mut on_ready: impl FnMut(),
        mut on: impl FnMut(Event) -> bool,
    ) -> Result<()> {
        let stream = self.connect()?;
        stream.set_write_timeout(Some(CALL_TIMEOUT))?;
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
                Response::EventsReady => on_ready(),
                Response::Event { event } => {
                    if !on(event) {
                        return Ok(());
                    }
                }
                Response::End => return Ok(()),
                Response::Error { message, .. } => bail!("{message}"),
                Response::Lagged { skipped } => {
                    bail!("event stream missed {skipped} events; reconnecting to refresh snapshots")
                }
                _ => {}
            }
        }
    }

    /// Connect, starting the daemon when nobody listens, as the CLI does.
    fn connect(&self) -> Result<UnixStream> {
        check_socket(&self.socket)?;
        match UnixStream::connect(&self.socket) {
            Ok(stream) => return Ok(stream),
            Err(err) if absent(&err) => {}
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("cannot reach agentd at {}", self.socket.display()));
            }
        }
        if !self.autostart {
            bail!(
                "agentd is not listening at {} (autostart is disabled)",
                self.socket.display()
            );
        }
        if !may_spawn() {
            bail!(
                "agentd is not listening at {} (a start was attempted moments ago)",
                self.socket.display()
            );
        }
        let lock_path = paths::lock_path(&self.socket);
        if let Some(parent) = lock_path.parent() {
            if parent == paths::socket_dir(&self.home) && parent != self.home {
                dirs::ensure_private_dir(parent)?;
            } else {
                std::fs::create_dir_all(parent)?;
            }
        }
        check_socket(&self.socket)?;
        let vacant = lock::try_exclusive(&lock_path)
            .with_context(|| format!("cannot take {}", lock_path.display()))?
            .is_some();
        let child = if vacant {
            Some(spawn_agentd(&self.socket, &self.home)?)
        } else {
            None
        };
        wait_for_start(&self.socket, START_TIMEOUT, child)
    }
}

fn check_socket(socket: &Path) -> Result<()> {
    if !paths::fits_socket(socket) {
        bail!(
            "socket path {} is {} bytes; this OS allows {}",
            socket.display(),
            socket.as_os_str().len(),
            paths::SOCKET_PATH_MAX
        );
    }
    dirs::check_socket_parent(socket)
        .with_context(|| format!("socket directory for {} is unusable", socket.display()))
}

struct StartingChild(Option<Child>);

impl Drop for StartingChild {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn wait_for_start(socket: &Path, timeout: Duration, child: Option<Child>) -> Result<UnixStream> {
    let mut child = StartingChild(child);
    let deadline = Instant::now() + timeout;
    loop {
        check_socket(socket)?;
        if let Some(process) = child.0.as_mut()
            && let Some(status) = process.try_wait()?
        {
            bail!("agentd exited during startup ({status})");
        }
        match UnixStream::connect(socket) {
            Ok(stream) => {
                // The app stays open, so reap our detached daemon if it later
                // exits. Startup failures are instead killed and reaped by Drop.
                if let Some(mut process) = child.0.take() {
                    std::thread::spawn(move || {
                        let _ = process.wait();
                    });
                }
                return Ok(stream);
            }
            Err(err) if absent(&err) && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("agentd did not come up at {}", socket.display()));
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
fn spawn_agentd(socket: &Path, home: &Path) -> Result<Child> {
    let exe = std::env::current_exe()
        .ok()
        .and_then(|me| me.parent().map(|dir| dir.join("agentd")))
        .filter(|sibling| sibling.is_file())
        .unwrap_or_else(|| PathBuf::from("agentd"));
    std::fs::create_dir_all(home)?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(paths::daemon_log(home))?;
    Command::new(&exe)
        .arg("--socket")
        .arg(socket)
        .arg("--home")
        .arg(home)
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .spawn()
        .with_context(|| format!("cannot start {}", exe.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;

    #[test]
    fn event_readiness_recovers_an_idle_connection_and_lag_is_reported() {
        let tmp = tempfile::tempdir().unwrap();
        let socket = tmp.path().join("sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream);
            let mut request = String::new();
            reader.read_line(&mut request).unwrap();
            assert!(matches!(
                serde_json::from_str::<Request>(&request).unwrap(),
                Request::Events { ready: true, .. }
            ));
            reader
                .get_mut()
                .write_all(b"{\"type\":\"events_ready\"}\n{\"type\":\"lagged\",\"skipped\":7}\n")
                .unwrap();
        });
        let client = Client {
            socket,
            home: tmp.path().into(),
            autostart: false,
        };
        let mut ready = false;
        let error = client
            .events(
                100,
                || ready = true,
                |_| panic!("no event needed for readiness"),
            )
            .unwrap_err();
        server.join().unwrap();
        assert!(ready);
        assert!(error.to_string().contains("missed 7 events"));
    }

    #[test]
    fn untrusted_existing_fallback_is_rejected_before_connection() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("h".repeat(120));
        let parent = paths::socket_dir(&home);
        dirs::ensure_private_dir(&parent).unwrap();
        let socket = parent.join(paths::HOST_SOCKET);
        let listener = UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o777)).unwrap();
        let client = Client {
            socket: socket.clone(),
            home,
            autostart: false,
        };
        let error = client.call(&Request::Ping).unwrap_err();
        // Restore and clean up our fixture before asserting the result.
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700)).unwrap();
        let accepted = listener.accept();
        drop(listener);
        std::fs::remove_file(socket).unwrap();
        std::fs::remove_dir(parent).unwrap();
        assert!(format!("{error:#}").contains("writable by others"));
        assert_eq!(accepted.unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
    }

    #[test]
    fn disabled_autostart_and_overlong_socket_have_no_startup_side_effects() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("new-home");
        let client = Client {
            socket: tmp.path().join("sock"),
            home: home.clone(),
            autostart: false,
        };
        assert!(
            client
                .connect()
                .unwrap_err()
                .to_string()
                .contains("autostart is disabled")
        );
        let client = Client {
            socket: tmp.path().join("s".repeat(paths::SOCKET_PATH_MAX)),
            home: home.clone(),
            autostart: true,
        };
        assert!(client.connect().unwrap_err().to_string().contains("bytes"));
        assert!(!home.exists());
    }

    #[test]
    fn startup_failure_reaps_owned_child_and_success_keeps_it_alive() {
        let tmp = tempfile::tempdir().unwrap();
        let socket = tmp.path().join("sock");
        let child = Command::new("/bin/sleep").arg("30").spawn().unwrap();
        let pid = child.id() as i32;
        assert!(wait_for_start(&socket, Duration::ZERO, Some(child)).is_err());
        // SAFETY: the PID belongs to the child started above; signal 0 only probes.
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);

        let child = Command::new("/bin/sh")
            .args(["-c", "exit 7"])
            .spawn()
            .unwrap();
        let error = wait_for_start(&socket, Duration::from_secs(2), Some(child)).unwrap_err();
        assert!(error.to_string().contains("exited during startup"));

        let listener = UnixListener::bind(&socket).unwrap();
        let child = Command::new("/bin/sleep").arg("30").spawn().unwrap();
        let pid = child.id() as i32;
        let stream = wait_for_start(&socket, Duration::from_secs(1), Some(child)).unwrap();
        // SAFETY: only this test's still-running child is probed and terminated.
        let alive = unsafe { libc::kill(pid, 0) };
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
        assert_eq!(alive, 0);
        let deadline = Instant::now() + Duration::from_secs(2);
        while unsafe { libc::kill(pid, 0) } == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            unsafe { libc::kill(pid, 0) },
            -1,
            "the background waiter reaps a later exit"
        );
        drop(stream);
        drop(listener);
    }
}
