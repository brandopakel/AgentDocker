//! Process supervision for managed agents.

use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::Arc;

use agentdocker_core::{AgentId, AgentRecord, AgentStatus};
use agentdocker_host::launch::{OwnedChild, Pending};
use anyhow::Context;
use chrono::Utc;
use std::os::fd::{AsRawFd, OwnedFd};
use tokio::fs::File;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader};
use tokio::sync::{broadcast, mpsc, watch};

use crate::daemon::Daemon;

pub struct Spawned {
    pub pid: u32,
    pub process_started_at: chrono::DateTime<Utc>,
    child: Option<OwnedChild>,
    pending: Option<Pending>,
    batch_log: Option<mpsc::Sender<String>>,
    launch_error: Option<String>,
    pub control: watch::Sender<Option<bool>>,
    stop: watch::Receiver<Option<bool>>,
    /// The daemon's end of the agent's terminal, when it was given one.
    pub session: Option<Session>,
}

/// What a client attaching late is shown before the live stream: enough
/// to see where the agent got to, not its whole history — the log has
/// that.
const SCROLLBACK: usize = 64 * 1024;

/// A managed agent's terminal, as the daemon holds it: what it prints,
/// what can be typed at it, how big its window is, and what it printed
/// just before you looked.
#[derive(Clone)]
pub struct Session {
    /// Only the terminal reader owns the sender. An attached client must not
    /// keep its own output stream alive after the terminal reaches EOF.
    output: broadcast::WeakSender<Vec<u8>>,
    /// Keystrokes on their way to the agent.
    pub input: mpsc::Sender<Vec<u8>>,
    master: Arc<OwnedFd>,
    /// The last [`SCROLLBACK`] bytes it printed, raw, so an attaching
    /// client sees the screen rather than an empty one.
    scrollback: Arc<std::sync::Mutex<std::collections::VecDeque<u8>>>,
}

impl Session {
    /// Tell the terminal its window changed, so full-screen agents relay
    /// out and get `SIGWINCH`.
    pub fn resize(&self, cols: u16, rows: u16) -> std::io::Result<()> {
        agentdocker_host::pty::set_window_size(self.master.as_raw_fd(), cols, rows)
    }

    /// What to show now, and what comes next. Taken together under one
    /// lock so a byte cannot fall between them or arrive twice: anything
    /// already broadcast is in the scrollback, anything broadcast later
    /// reaches the receiver.
    pub fn attach(&self) -> (Vec<u8>, broadcast::Receiver<Vec<u8>>) {
        let history = lock_scrollback(&self.scrollback);
        let seen: Vec<u8> = history.iter().copied().collect();
        let live = self
            .output
            .upgrade()
            .map(|output| output.subscribe())
            // The reader may have ended just before this attach. Replay the
            // final history, followed by a receiver that is already closed.
            .unwrap_or_else(|| broadcast::channel(1).1);
        drop(history);
        (seen, live)
    }
}

/// A poisoned scrollback is still readable bytes; nothing here can leave
/// it inconsistent.
fn lock_scrollback(
    scrollback: &std::sync::Mutex<std::collections::VecDeque<u8>>,
) -> std::sync::MutexGuard<'_, std::collections::VecDeque<u8>> {
    scrollback
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Launch the agent's command with its output captured to a log file.
/// The child inherits the daemon's environment plus `spec.env` and the
/// `AGENTDOCKER_*` variables that let it find the daemon and itself.
pub async fn spawn(daemon: &Daemon, record: &AgentRecord) -> anyhow::Result<Spawned> {
    let Some((program, args)) = record.spec.command.split_first() else {
        anyhow::bail!("empty command");
    };
    let mut command = Command::new(program);
    command
        .args(args)
        .envs(&record.spec.env)
        .env("AGENTDOCKER_HOME", &daemon.home)
        .env("AGENTDOCKER_SOCKET", &daemon.socket)
        .env_remove("AGENTDOCKER_TOKEN_FILE")
        .env("AGENTDOCKER_NO_AUTOSTART", "1")
        .env("AGENTDOCKER_AGENT_ID", record.id.as_str())
        .env("AGENTDOCKER_AGENT_NAME", &record.spec.name)
        .env(
            "TERM",
            std::env::var("TERM").as_deref().unwrap_or("xterm-256color"),
        );
    // A terminal when the agent asked for one: interactive runtimes need
    // it, and it is what `attach` connects to. `setsid` in the child makes
    // it a process-group leader by itself, so `process_group` would only
    // make the later `setsid` fail.
    let mut pty = if record.spec.tty {
        Some(agentdocker_host::pty::Pty::open().context("cannot open a terminal for the agent")?)
    } else {
        None
    };
    match pty.as_mut().and_then(|pty| pty.take_slave()) {
        Some(slave) => {
            let stdin = slave.try_clone()?;
            let stdout = slave.try_clone()?;
            command
                .stdin(Stdio::from(stdin))
                .stdout(Stdio::from(stdout))
                .stderr(Stdio::from(slave));
            // SAFETY: `take_controlling_terminal` uses only
            // async-signal-safe calls, as its contract requires.
            unsafe { command.pre_exec(|| agentdocker_host::pty::take_controlling_terminal()) };
        }
        None => {
            command
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            command.process_group(0);
        }
    }
    if let Some(workdir) = &record.spec.workdir {
        command.current_dir(workdir);
    }

    // Allocate every fallible terminal descriptor before a process can start.
    let terminal_io = pty
        .as_ref()
        .map(|pty| -> std::io::Result<_> {
            Ok((pty.master().try_clone()?, pty.master().try_clone()?))
        })
        .transpose()
        .context("cannot clone the agent's terminal")?;

    let log_path = daemon.log_path(&record.id);
    let log = tokio::task::spawn_blocking(move || {
        agentdocker_host::dirs::secure_state_dir(
            log_path.parent().expect("log path has a parent"),
        )?;
        agentdocker_host::dirs::private_file(&log_path, true, true)
            .with_context(|| format!("cannot open {}", log_path.display()))
    })
    .await??;
    let log = File::from_std(log);
    daemon.validate_native_launch(record)?;
    let pending = tokio::task::spawn_blocking(move || agentdocker_host::launch::prepare(command))
        .await?
        .with_context(|| format!("failed to prepare `{program}`"))?;
    let pid = pending.pid;
    let process_started_at = agentdocker_host::procinfo::start_time(pid)
        .context("cannot verify the prepared command's process identity; exec denied")?;
    daemon.validate_native_launch(record)?;

    let (tx, rx) = mpsc::channel::<String>(256);
    tokio::spawn(write_log(log, rx));
    let mut batch_log = None;
    let session = match pty {
        Some(pty) => {
            let master = Arc::new(pty.into_master());
            let (output, _) = broadcast::channel::<Vec<u8>>(256);
            let session_output = output.downgrade();
            let (input, keystrokes) = mpsc::channel::<Vec<u8>>(64);
            let scrollback = Arc::new(std::sync::Mutex::new(
                std::collections::VecDeque::<u8>::new(),
            ));
            // One task reads the terminal into the log, the scrollback and
            // whoever is attached; another types into it.
            //
            let (reader, writer) = terminal_io.expect("allocated before launch");
            tokio::spawn(pump_terminal(
                tokio::fs::File::from_std(std::fs::File::from(reader)),
                tx,
                output,
                scrollback.clone(),
            ));
            tokio::spawn(type_into_terminal(
                tokio::fs::File::from_std(std::fs::File::from(writer)),
                keystrokes,
            ));
            Some(Session {
                output: session_output,
                input,
                master,
                scrollback,
            })
        }
        None => {
            batch_log = Some(tx);
            None
        }
    };

    let (control, stop) = watch::channel(None);
    Ok(Spawned {
        pid,
        process_started_at,
        child: None,
        pending: Some(pending),
        batch_log,
        launch_error: None,
        control,
        stop,
        session,
    })
}

impl Spawned {
    /// The durable identity and event must already be committed. Dropping an
    /// unactivated Spawned closes its gate; the command never executes.
    pub async fn activate(&mut self, action: &str) -> anyhow::Result<()> {
        let result = self
            .activate_inner()
            .await
            .with_context(|| format!("command could not be {action}"));
        if let Err(error) = &result {
            self.launch_error = Some(format!("{error:#}"));
        }
        result
    }

    async fn activate_inner(&mut self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.stop.borrow().is_none(),
            "launch was stopped before activation"
        );
        let pending = self.pending.take().context("launch already activated")?;
        let mut child = tokio::task::spawn_blocking(move || pending.activate()).await??;
        if let Some(tx) = self.batch_log.take() {
            let stdout = child
                .take_stdout()
                .map(tokio::process::ChildStdout::from_std)
                .transpose()?;
            let stderr = child
                .take_stderr()
                .map(tokio::process::ChildStderr::from_std)
                .transpose()?;
            if let Some(stdout) = stdout {
                tokio::spawn(pump(stdout, "out", tx.clone()));
            }
            if let Some(stderr) = stderr {
                tokio::spawn(pump(stderr, "err", tx));
            }
        }
        self.child = Some(child);
        Ok(())
    }
}

/// Read the agent's terminal: every byte goes to whoever is attached, and
/// whole lines go to the log so `logs` reads the same as it always did.
async fn pump_terminal(
    mut terminal: tokio::fs::File,
    log: mpsc::Sender<String>,
    output: broadcast::Sender<Vec<u8>>,
    scrollback: Arc<std::sync::Mutex<std::collections::VecDeque<u8>>>,
) {
    use tokio::io::AsyncReadExt;
    let mut buffer = vec![0_u8; 8192];
    let mut line = String::new();
    loop {
        // A closed terminal reads zero; a vanished one errors. Either ends
        // the session.
        let read = match terminal.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        let chunk = &buffer[..read];
        {
            // Remember it, then hand it on, both under the one lock, so a
            // client attaching sees every byte exactly once.
            let mut history = lock_scrollback(&scrollback);
            history.extend(chunk.iter().copied());
            let excess = history.len().saturating_sub(SCROLLBACK);
            history.drain(..excess);
            // No receiver simply means nobody is watching right now.
            let _ = output.send(chunk.to_vec());
        }
        line.push_str(&String::from_utf8_lossy(chunk));
        while let Some(end) = line.find('\n') {
            let complete: String = line.drain(..=end).collect();
            let complete = complete.trim_end_matches(['\n', '\r']).to_owned();
            if log.send(format!("out {complete}")).await.is_err() {
                return;
            }
        }
        // A prompt with no newline should not be held forever.
        if line.len() > 4096 {
            let partial = std::mem::take(&mut line);
            if log.send(format!("out {partial}")).await.is_err() {
                return;
            }
        }
    }
    if !line.is_empty() {
        let _ = log.send(format!("out {line}")).await;
    }
}

/// Type what an attached client sends into the agent's terminal.
async fn type_into_terminal(
    mut terminal: tokio::fs::File,
    mut keystrokes: mpsc::Receiver<Vec<u8>>,
) {
    use tokio::io::AsyncWriteExt;
    while let Some(bytes) = keystrokes.recv().await {
        if terminal.write_all(&bytes).await.is_err() || terminal.flush().await.is_err() {
            return;
        }
    }
}

/// Wait for the child in the background and record how it ended.
async fn wait_owned_child(
    child: &mut agentdocker_host::launch::OwnedChild,
) -> std::io::Result<std::process::ExitStatus> {
    // Subscribe before checking waitpid, so an exit between the check and
    // receive is retained. Signals can coalesce or describe another child;
    // only this owned PID is reaped, and no timer wakes idle agents.
    let mut changes = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::child())?;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        changes
            .recv()
            .await
            .ok_or_else(|| std::io::Error::other("child signal stream closed"))?;
    }
}

/// Retain process ownership through exit, cancellation and group cleanup.
pub fn supervise(
    daemon: Arc<Daemon>,
    id: AgentId,
    mut spawned: Spawned,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let group = Pid::from_raw(-(spawned.pid as i32));
        let mut stopping = false;
        let mut deadline = tokio::time::Instant::now();
        let result = if let Some(child) = &mut spawned.child {
            loop {
                tokio::select! {
                    biased;
                    result = wait_owned_child(child) => break result,
                    Ok(()) = spawned.stop.changed() => {
                        if let Some(force) = *spawned.stop.borrow_and_update() {
                            let _ = kill(group, if force { Signal::SIGKILL } else { Signal::SIGTERM });
                            if !stopping {
                                deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
                                stopping = true;
                            }
                        }
                    }
                    () = tokio::time::sleep_until(deadline), if stopping => {
                        let _ = kill(group, Signal::SIGKILL);
                        stopping = false;
                    }
                }
            }
        } else {
            // Pending's socket shutdown denies exec, including after storage
            // failure. Command's worker reaps its pre-exec failure.
            spawned.pending.take();
            Err(std::io::Error::other(
                spawned
                    .launch_error
                    .take()
                    .unwrap_or_else(|| "launch was not activated".into()),
            ))
        };
        let status = match result {
            Ok(exit) => AgentStatus::Exited { code: exit.code() },
            Err(err) => AgentStatus::Failed {
                reason: err.to_string(),
            },
        };
        // A managed command owns its process group. Descendants must stop
        // before the agent's leases can be released, even on a normal exit.
        let group = Pid::from_raw(-(spawned.pid as i32));
        if group_exists(spawned.pid) {
            let _ = kill(group, Signal::SIGTERM);
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
            while group_exists(spawned.pid) {
                if tokio::time::Instant::now() >= deadline {
                    let _ = kill(group, Signal::SIGKILL);
                }
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
        }
        // The terminal goes with the agent: anyone attached sees the
        // stream end rather than a room that is no longer there.
        daemon.end_session(&id);
        daemon.mark_exited(&id, status.clone());
        // After the exit is recorded, so a reader of the event stream
        // sees the agent end before it sees it start again.
        daemon.consider_restart(&id, &status);
    })
}

async fn pump<R: AsyncRead + Unpin>(reader: R, stream: &'static str, tx: mpsc::Sender<String>) {
    let mut lines = BufReader::new(reader).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let stamp = Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ");
        if tx
            .send(format!("{stamp} [{stream}] {line}\n"))
            .await
            .is_err()
        {
            break;
        }
    }
}

async fn write_log(mut log: File, mut rx: mpsc::Receiver<String>) {
    while let Some(line) = rx.recv().await {
        if log.write_all(line.as_bytes()).await.is_err() {
            break;
        }
    }
    let _ = log.flush().await;
}

/// Whether a validated dedicated group still has any processes. Uncertainty
/// retains protection rather than reporting a running writer as exited.
pub(crate) fn group_exists(group: u32) -> bool {
    let Ok(group) = i32::try_from(group) else {
        return false;
    };
    group > 0
        && matches!(
            kill(Pid::from_raw(-group), None),
            Ok(()) | Err(nix::errno::Errno::EPERM)
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use nix::libc;
    use std::io::Write;
    use std::os::{fd::OwnedFd, unix::process::CommandExt};

    #[tokio::test]
    async fn child_exit_before_or_after_signal_subscription_is_not_lost() {
        for already_exited in [true, false] {
            let (mut input, stdin) = std::os::unix::net::UnixStream::pair().unwrap();
            let mut command = std::process::Command::new("sh");
            command.args(["-c", "read line; exit 17"]).process_group(0);
            command.stdin(Stdio::from(OwnedFd::from(stdin)));
            let pending = agentdocker_host::launch::prepare(command).unwrap();
            let pid = pending.pid;
            let mut child = pending.activate().unwrap();
            if already_exited {
                input.write_all(b"exit\n").unwrap();
                // Observe exit without reaping; OwnedChild retains the PID.
                let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
                assert_eq!(
                    unsafe {
                        libc::waitid(libc::P_PID, pid, &mut info, libc::WEXITED | libc::WNOWAIT)
                    },
                    0
                );
            }
            let mut waiting = tokio::spawn(async move { wait_owned_child(&mut child).await });
            if !already_exited {
                tokio::task::yield_now().await;
                input.write_all(b"exit\n").unwrap();
            }
            let status =
                match tokio::time::timeout(std::time::Duration::from_secs(5), &mut waiting).await {
                    Ok(result) => result.unwrap().unwrap(),
                    Err(error) => {
                        waiting.abort();
                        let _ = waiting.await;
                        panic!("{error}");
                    }
                };
            assert_eq!(status.code(), Some(17));
        }
    }
}
