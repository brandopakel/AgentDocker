//! Process supervision for managed agents.

use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use std::os::unix::process::CommandExt;
use std::process::Stdio;
use std::sync::Arc;

use agentdocker_core::{AgentId, AgentRecord, AgentStatus};
use anyhow::Context;
use chrono::Utc;
use std::os::fd::{AsRawFd, OwnedFd};
use tokio::fs::File;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{broadcast, mpsc, watch};

use crate::daemon::Daemon;

pub struct Spawned {
    pub pid: u32,
    child: Child,
    pub control: watch::Sender<Option<bool>>,
    stop: watch::Receiver<Option<bool>>,
    /// The daemon's end of the agent's terminal, when it was given one.
    pub session: Option<Session>,
}

/// A managed agent's terminal, as the daemon holds it: what it prints,
/// what can be typed at it, and how big its window is.
#[derive(Clone)]
pub struct Session {
    /// Everything the agent writes, to whoever is attached. Late joiners
    /// get what comes next; the log has the rest.
    pub output: broadcast::Sender<Vec<u8>>,
    /// Keystrokes on their way to the agent.
    pub input: mpsc::Sender<Vec<u8>>,
    master: Arc<OwnedFd>,
}

impl Session {
    /// Tell the terminal its window changed, so full-screen agents relay
    /// out and get `SIGWINCH`.
    pub fn resize(&self, cols: u16, rows: u16) -> std::io::Result<()> {
        agentdocker_host::pty::set_window_size(self.master.as_raw_fd(), cols, rows)
    }
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
        .env("AGENTDOCKER_SOCKET", &daemon.socket)
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
            unsafe {
                command
                    .as_std_mut()
                    .pre_exec(|| agentdocker_host::pty::take_controlling_terminal())
            };
        }
        None => {
            command
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            command.as_std_mut().process_group(0);
        }
    }
    if let Some(workdir) = &record.spec.workdir {
        command.current_dir(workdir);
    }

    let log_path = daemon.log_path(&record.id);
    tokio::fs::create_dir_all(log_path.parent().expect("log path has a parent")).await?;
    let log = File::create(&log_path)
        .await
        .with_context(|| format!("cannot create {}", log_path.display()))?;
    let mut child = command
        .spawn()
        .with_context(|| format!("failed to launch `{program}`"))?;
    let pid = child.id().context("child exited before reporting a pid")?;

    let (tx, rx) = mpsc::channel::<String>(256);
    tokio::spawn(write_log(log, rx));
    let session = match pty {
        Some(pty) => {
            let master = Arc::new(pty.into_master());
            let (output, _) = broadcast::channel::<Vec<u8>>(256);
            let (input, keystrokes) = mpsc::channel::<Vec<u8>>(64);
            // One task reads the terminal into the log and to whoever is
            // attached; another types into it.
            let reader = tokio::fs::File::from_std(std::fs::File::from(master.try_clone()?));
            tokio::spawn(pump_terminal(reader, tx, output.clone()));
            let writer = tokio::fs::File::from_std(std::fs::File::from(master.try_clone()?));
            tokio::spawn(type_into_terminal(writer, keystrokes));
            Some(Session {
                output,
                input,
                master,
            })
        }
        None => {
            if let Some(stdout) = child.stdout.take() {
                tokio::spawn(pump(stdout, "out", tx.clone()));
            }
            if let Some(stderr) = child.stderr.take() {
                tokio::spawn(pump(stderr, "err", tx));
            }
            None
        }
    };

    let (control, stop) = watch::channel(None);
    Ok(Spawned {
        pid,
        child,
        control,
        stop,
        session,
    })
}

/// Read the agent's terminal: every byte goes to whoever is attached, and
/// whole lines go to the log so `logs` reads the same as it always did.
async fn pump_terminal(
    mut terminal: tokio::fs::File,
    log: mpsc::Sender<String>,
    output: broadcast::Sender<Vec<u8>>,
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
        // No receiver simply means nobody is watching right now.
        let _ = output.send(chunk.to_vec());
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
pub fn supervise(daemon: Arc<Daemon>, id: AgentId, mut spawned: Spawned) {
    tokio::spawn(async move {
        let group = Pid::from_raw(-(spawned.pid as i32));
        let mut stopping = false;
        let mut deadline = tokio::time::Instant::now();
        let result = loop {
            tokio::select! {
                biased;
                result = spawned.child.wait() => break result,
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
        daemon.mark_exited(&id, status);
    });
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
