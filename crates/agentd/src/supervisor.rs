//! Process supervision for managed agents.

use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use std::os::unix::process::CommandExt;
use std::process::Stdio;
use std::sync::Arc;

use agentdocker_core::{AgentId, AgentRecord, AgentStatus};
use anyhow::Context;
use chrono::Utc;
use tokio::fs::File;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, watch};

use crate::daemon::Daemon;

pub struct Spawned {
    pub pid: u32,
    child: Child,
    pub control: watch::Sender<Option<bool>>,
    stop: watch::Receiver<Option<bool>>,
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
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.as_std_mut().process_group(0);
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
    if let Some(stdout) = child.stdout.take() {
        tokio::spawn(pump(stdout, "out", tx.clone()));
    }
    if let Some(stderr) = child.stderr.take() {
        tokio::spawn(pump(stderr, "err", tx));
    }

    let (control, stop) = watch::channel(None);
    Ok(Spawned {
        pid,
        child,
        control,
        stop,
    })
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
