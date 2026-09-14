//! Process supervision for managed agents, through a session owner.
//!
//! The daemon no longer holds a managed agent's child itself. Each `run`
//! starts a [session owner](crate::owner): a small process that prepares
//! the command behind the launch gate, owns the child, its terminal or
//! pipes and its log, and serves this daemon on the agent's session socket.
//! What this module keeps is the daemon's side of that conversation — the
//! controller — behind the same shape the rest of the daemon always used:
//! a [`Spawned`] with the child's identity, a stop handle, an optional
//! [`Session`] for `attach`, `activate` once the launch record is durable,
//! and `supervise` until the exit is recorded.
//!
//! Because the owner outlives the daemon, a daemon that restarts finds the
//! owner still there and reattaches (`reattach`) instead of relaunching.

use std::collections::VecDeque;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use agentdocker_core::session::{
    ExitReport, FORMAT, OwnerCommand, OwnerHello, OwnerReport, SessionOwner, exit_path, socket_path,
};
use agentdocker_core::{AgentId, AgentRecord, AgentStatus};
use anyhow::Context;
use chrono::Utc;
use nix::sys::signal::kill;
use nix::unistd::Pid;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::{broadcast, mpsc, watch};

use crate::daemon::Daemon;
use crate::owner::Launch;

/// How long an owner may take to bind its socket and report the prepared
/// child. The launch gate's own deadline is shorter.
const OWNER_READY_WITHIN: Duration = Duration::from_secs(10);

/// What a client attaching late is shown before the live stream: enough
/// to see where the agent got to, not its whole history — the log has
/// that.
const SCROLLBACK: usize = 64 * 1024;

/// How the daemon runs an owner: as its own process (production), or as a
/// task in this process (tests, where the daemon binary is not on hand).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OwnerMode {
    Process(std::path::PathBuf),
    InProcess,
}

impl OwnerMode {
    /// The daemon's own executable runs owners; tests keep them in-process.
    pub fn detect() -> Self {
        if cfg!(test) || std::env::var_os("AGENTDOCKER_OWNER_IN_PROCESS").is_some() {
            return Self::InProcess;
        }
        match std::env::current_exe() {
            Ok(path) => Self::Process(path),
            Err(_) => Self::InProcess,
        }
    }
}

enum OwnerLink {
    Process(std::process::Child),
    InProcess(tokio::task::JoinHandle<anyhow::Result<i32>>),
    /// Reattached after a daemon restart: the owner is nobody's child here.
    Detached,
}

struct Controller {
    lines: Lines<BufReader<OwnedReadHalf>>,
    writer: OwnedWriteHalf,
}

impl Controller {
    async fn connect(socket: &Path, within: Duration) -> anyhow::Result<Self> {
        let deadline = tokio::time::Instant::now() + within;
        let stream = loop {
            match UnixStream::connect(socket).await {
                Ok(stream) => break stream,
                Err(error) if tokio::time::Instant::now() >= deadline => {
                    return Err(error).with_context(|| {
                        format!("session owner at {} did not answer", socket.display())
                    });
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
            }
        };
        let (reader, writer) = stream.into_split();
        Ok(Self {
            lines: BufReader::new(reader).lines(),
            writer,
        })
    }

    async fn send(&mut self, command: &OwnerCommand) -> anyhow::Result<()> {
        let mut line = serde_json::to_vec(command)?;
        line.push(b'\n');
        self.writer
            .write_all(&line)
            .await
            .context("session owner connection closed")
    }

    async fn hello(&mut self) -> anyhow::Result<OwnerHello> {
        let line = self
            .lines
            .next_line()
            .await?
            .context("session owner closed before its hello")?;
        let hello: OwnerHello = serde_json::from_str(&line).context("malformed owner hello")?;
        anyhow::ensure!(hello.format == FORMAT, "unknown session owner format");
        Ok(hello)
    }

    async fn next(&mut self) -> anyhow::Result<Option<OwnerReport>> {
        loop {
            let Some(line) = self.lines.next_line().await? else {
                return Ok(None);
            };
            match serde_json::from_str::<OwnerReport>(&line) {
                Ok(report) => return Ok(Some(report)),
                // A newer owner may say things this daemon does not know;
                // that is not a reason to abandon the child.
                Err(_) => continue,
            }
        }
    }
}

pub struct Spawned {
    pub pid: u32,
    pub process_started_at: chrono::DateTime<Utc>,
    /// The owner process, recorded on the agent so a restarted daemon can
    /// find it again.
    pub owner: SessionOwner,
    link: OwnerLink,
    controller: Controller,
    launch_error: Option<String>,
    activated: bool,
    pub control: watch::Sender<Option<bool>>,
    stop: watch::Receiver<Option<bool>>,
    /// The daemon's end of the agent's terminal, when it was given one.
    pub session: Option<Session>,
    /// What the session's clients send, relayed to the owner by `supervise`.
    keystrokes: Option<mpsc::Receiver<Vec<u8>>>,
    resizes: Option<mpsc::Receiver<(u16, u16)>>,
    /// Where relayed output goes.
    output: Option<broadcast::Sender<Vec<u8>>>,
    scrollback: Option<Arc<std::sync::Mutex<VecDeque<u8>>>>,
    /// Bytes of output already relayed into the scrollback; an `Attach`
    /// after a reattach asks for what follows.
    relayed: u64,
}

/// A managed agent's terminal, as the daemon presents it: what it prints,
/// what can be typed at it, how big its window is, and what it printed
/// just before you looked. The bytes come from the owner; the shape is
/// the one `attach` always used.
#[derive(Clone)]
pub struct Session {
    /// Only the relay owns the sender. An attached client must not keep
    /// its own output stream alive after the terminal reaches EOF.
    output: broadcast::WeakSender<Vec<u8>>,
    /// Keystrokes on their way to the agent.
    pub input: mpsc::Sender<Vec<u8>>,
    resize: mpsc::Sender<(u16, u16)>,
    scrollback: Arc<std::sync::Mutex<VecDeque<u8>>>,
}

impl Session {
    /// Tell the terminal its window changed, so full-screen agents relay
    /// out and get `SIGWINCH`.
    pub fn resize(&self, cols: u16, rows: u16) -> std::io::Result<()> {
        self.resize
            .try_send((cols, rows))
            .map_err(|_| std::io::Error::other("terminal is not accepting resizes"))
    }

    /// What to show now, and what comes next, taken together under one
    /// lock so a byte cannot fall between them or arrive twice.
    pub fn attach(&self) -> (Vec<u8>, broadcast::Receiver<Vec<u8>>) {
        let history = lock_scrollback(&self.scrollback);
        let seen: Vec<u8> = history.iter().copied().collect();
        let live = self
            .output
            .upgrade()
            .map(|output| output.subscribe())
            .unwrap_or_else(|| broadcast::channel(1).1);
        drop(history);
        (seen, live)
    }
}

fn lock_scrollback(
    scrollback: &std::sync::Mutex<VecDeque<u8>>,
) -> std::sync::MutexGuard<'_, VecDeque<u8>> {
    scrollback
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn launch_for(daemon: &Daemon, record: &AgentRecord) -> Launch {
    Launch {
        format: FORMAT,
        agent: record.id.clone(),
        name: record.spec.name.clone(),
        command: record.spec.command.clone(),
        env: record.spec.env.clone(),
        workdir: record.spec.workdir.clone(),
        tty: record.spec.tty,
        home: daemon.home.clone(),
        socket: daemon.socket.clone(),
        log: daemon.log_path(&record.id),
    }
}

/// Start an owner for the agent's command and learn the prepared child's
/// identity. The command does not run until [`Spawned::activate`].
pub async fn spawn(daemon: &Daemon, record: &AgentRecord) -> anyhow::Result<Spawned> {
    anyhow::ensure!(!record.spec.command.is_empty(), "empty command");
    daemon.validate_native_launch(record)?;
    let launch = launch_for(daemon, record);
    let socket = socket_path(&daemon.home, &record.id);
    // A stale exit file from an earlier life of this id must not be read
    // as this launch's exit.
    let _ = std::fs::remove_file(exit_path(&daemon.home, &record.id));
    let link = match daemon.owner_mode() {
        OwnerMode::InProcess => OwnerLink::InProcess(tokio::spawn(crate::owner::serve(launch))),
        OwnerMode::Process(executable) => {
            let mut command = std::process::Command::new(executable);
            command
                .arg("--session-owner")
                .env("AGENTDOCKER_HOME", &daemon.home)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::inherit())
                // Its own group: a signal meant for a managed agent's group,
                // or for the daemon's, must never reach the owner.
                .process_group(0);
            let mut child = command.spawn().context("cannot start the session owner")?;
            let mut stdin = child.stdin.take().expect("piped");
            let text = serde_json::to_vec(&launch)?;
            tokio::task::spawn_blocking(move || {
                use std::io::Write;
                stdin.write_all(&text).and_then(|()| stdin.flush())
            })
            .await?
            .context("cannot hand the launch to the session owner")?;
            OwnerLink::Process(child)
        }
    };
    let mut controller = Controller::connect(&socket, OWNER_READY_WITHIN).await?;
    let hello = controller.hello().await?;
    let child = hello
        .child
        .clone()
        .context("session owner reported no prepared child")?;
    daemon.validate_native_launch(record)?;
    let owner = SessionOwner {
        pid: hello.owner_pid,
        started_at: hello.owner_started_at,
    };
    let (control, stop) = watch::channel(None);
    let mut spawned = Spawned {
        pid: child.pid,
        process_started_at: child.started_at,
        owner,
        link,
        controller,
        launch_error: None,
        activated: false,
        control,
        stop,
        session: None,
        keystrokes: None,
        resizes: None,
        output: None,
        scrollback: None,
        relayed: 0,
    };
    if child.tty {
        spawned.open_session();
    }
    Ok(spawned)
}

/// Find an owner that outlived the previous daemon and take its child
/// back under supervision. The exit file answers for an owner that has
/// already finished.
pub async fn reattach(daemon: &Daemon, record: &AgentRecord) -> anyhow::Result<Reattached> {
    let socket = socket_path(&daemon.home, &record.id);
    let exit = exit_path(&daemon.home, &record.id);
    let mut controller = match Controller::connect(&socket, Duration::from_millis(500)).await {
        Ok(controller) => controller,
        Err(error) => {
            if let Ok(text) = std::fs::read(&exit)
                && let Ok(report) = serde_json::from_slice::<ExitReport>(&text)
            {
                let _ = std::fs::remove_file(&exit);
                let _ = std::fs::remove_file(&socket);
                return Ok(Reattached::Exited(report));
            }
            return Err(error);
        }
    };
    let hello = controller.hello().await?;
    let child = hello.child.clone().context("owner has no child")?;
    let owner = SessionOwner {
        pid: hello.owner_pid,
        started_at: hello.owner_started_at,
    };
    let (control, stop) = watch::channel(None);
    let mut spawned = Spawned {
        pid: child.pid,
        process_started_at: child.started_at,
        owner,
        link: OwnerLink::Detached,
        controller,
        launch_error: None,
        activated: false,
        control,
        stop,
        session: None,
        keystrokes: None,
        resizes: None,
        output: None,
        scrollback: None,
        relayed: 0,
    };
    if child.tty {
        spawned.open_session();
        // Show what it printed while nobody was looking.
        spawned
            .controller
            .send(&OwnerCommand::Attach { after: 0 })
            .await?;
    }
    spawned.activated = hello.activated;
    if !hello.activated {
        // A launch the old daemon never authorised: its record was never
        // committed as running either, so deny it rather than guess.
        spawned
            .controller
            .send(&OwnerCommand::Stop { force: true })
            .await?;
    }
    Ok(Reattached::Running(Box::new(spawned)))
}

pub enum Reattached {
    Running(Box<Spawned>),
    Exited(ExitReport),
}

impl Spawned {
    fn open_session(&mut self) {
        let (output, _) = broadcast::channel::<Vec<u8>>(256);
        let (input, keystrokes) = mpsc::channel::<Vec<u8>>(64);
        let (resize, resizes) = mpsc::channel::<(u16, u16)>(8);
        let scrollback = Arc::new(std::sync::Mutex::new(VecDeque::<u8>::new()));
        self.session = Some(Session {
            output: output.downgrade(),
            input,
            resize,
            scrollback: scrollback.clone(),
        });
        self.keystrokes = Some(keystrokes);
        self.resizes = Some(resizes);
        self.output = Some(output);
        self.scrollback = Some(scrollback);
    }

    /// The durable identity and event must already be committed. Dropping an
    /// unactivated Spawned closes its controller; the owner's gate then
    /// times out and the command never executes.
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
        self.controller.send(&OwnerCommand::Activate).await?;
        // Nothing but activation, or an early exit, can arrive here: output
        // begins only once the command runs.
        loop {
            match self.controller.next().await? {
                Some(OwnerReport::Activated) => break,
                Some(OwnerReport::Exited { status }) => {
                    anyhow::bail!("command exited before it was activated: {status:?}")
                }
                Some(_) => continue,
                None => anyhow::bail!("session owner closed during activation"),
            }
        }
        self.activated = true;
        if self.session.is_some() {
            self.controller
                .send(&OwnerCommand::Attach {
                    after: self.relayed,
                })
                .await?;
        }
        Ok(())
    }

    fn relay(&mut self, offset: u64, bytes: &[u8]) {
        // Offsets make a replay after a reconnect idempotent: only bytes
        // past what this daemon already showed are added.
        let end = offset + bytes.len() as u64;
        if end <= self.relayed {
            return;
        }
        let skip = usize::try_from(self.relayed.saturating_sub(offset)).unwrap_or(0);
        let fresh = &bytes[skip.min(bytes.len())..];
        self.relayed = end;
        if let (Some(scrollback), Some(output)) = (&self.scrollback, &self.output) {
            let mut history = lock_scrollback(scrollback);
            history.extend(fresh.iter().copied());
            let excess = history.len().saturating_sub(SCROLLBACK);
            history.drain(..excess);
            let _ = output.send(fresh.to_vec());
        }
    }
}

/// Relay between the owner and the daemon until the exit is recorded.
pub fn supervise(
    daemon: Arc<Daemon>,
    id: AgentId,
    mut spawned: Spawned,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let status = if !spawned.activated {
            // The launch record never became durable: tell the owner to deny
            // exec now rather than at its own deadline, and wait for it to go.
            let reason = spawned
                .launch_error
                .take()
                .unwrap_or_else(|| "launch was not activated".into());
            let _ = spawned
                .controller
                .send(&OwnerCommand::Stop { force: true })
                .await;
            // The owner ends on its own once told; reaping the link below
            // waits for that, so no fixed pause here.
            AgentStatus::Failed { reason }
        } else {
            let mut keystrokes = spawned.keystrokes.take();
            let mut resizes = spawned.resizes.take();
            loop {
                tokio::select! {
                    biased;
                    report = spawned.controller.next() => match report {
                        Ok(Some(OwnerReport::Output { offset, bytes })) => spawned.relay(offset, &bytes),
                        Ok(Some(OwnerReport::InputDropped)) => {
                            tracing::warn!(agent = %id, "terminal input was dropped: the agent is not reading its terminal");
                        }
                        Ok(Some(OwnerReport::Gap { from, to })) => {
                            // Said, not hidden: the screen skips ahead and
                            // the log keeps the bytes.
                            tracing::info!(agent = %id, from, to, "terminal output skipped ahead after a gap");
                            spawned.relay(to, b"\r\n[agentdocker: output gap; see logs]\r\n");
                        }
                        Ok(Some(OwnerReport::OutputFailed { reason })) => {
                            tracing::warn!(agent = %id, %reason, "agent output capture failed");
                            daemon.emit(agentdocker_core::EventKind::AgentOutputFailed {
                                agent: id.clone(),
                                reason,
                            });
                        }
                        Ok(Some(OwnerReport::Exited { status })) => {
                            let _ = spawned.controller.send(&OwnerCommand::Acknowledge).await;
                            let _ = std::fs::remove_file(exit_path(&daemon.home, &id));
                            break exit_status(&status);
                        }
                        Ok(Some(_)) => {}
                        Ok(None) | Err(_) => {
                            // The owner went away without an exit report. Its
                            // exit file, if any, is the truth; otherwise the
                            // child is lost with it.
                            let exit = exit_path(&daemon.home, &id);
                            match std::fs::read(&exit).ok().and_then(|text| serde_json::from_slice::<ExitReport>(&text).ok()) {
                                Some(report) => {
                                    let _ = std::fs::remove_file(&exit);
                                    break exit_status(&report);
                                }
                                None => break AgentStatus::Failed { reason: "session owner lost".into() },
                            }
                        }
                    },
                    Some(bytes) = async { keystrokes.as_mut().expect("guarded").recv().await }, if keystrokes.is_some() => {
                        if spawned.controller.send(&OwnerCommand::Input { bytes }).await.is_err() {
                            keystrokes = None;
                        }
                    }
                    Some((cols, rows)) = async { resizes.as_mut().expect("guarded").recv().await }, if resizes.is_some() => {
                        if spawned.controller.send(&OwnerCommand::Resize { cols, rows }).await.is_err() {
                            resizes = None;
                        }
                    }
                    Ok(()) = spawned.stop.changed() => {
                        let pending = *spawned.stop.borrow_and_update();
                        if let Some(force) = pending {
                            let _ = spawned.controller.send(&OwnerCommand::Stop { force }).await;
                        }
                    }
                }
            }
        };
        // The owner finishes on its own once acknowledged; reap it here so
        // no zombie outlives the supervision that started it.
        match spawned.link {
            OwnerLink::Process(mut child) => {
                let _ = tokio::task::spawn_blocking(move || {
                    let deadline = std::time::Instant::now() + Duration::from_secs(5);
                    loop {
                        match child.try_wait() {
                            Ok(Some(_)) | Err(_) => break,
                            Ok(None) if std::time::Instant::now() >= deadline => {
                                let _ = child.kill();
                                let _ = child.wait();
                                break;
                            }
                            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
                        }
                    }
                })
                .await;
            }
            OwnerLink::InProcess(task) => {
                let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
            }
            OwnerLink::Detached => {}
        }
        let _ = std::fs::remove_file(socket_path(&daemon.home, &id));
        // Publish completion only after the owner has flushed the log, so
        // shutdown/restart cannot abandon the tail of the log.
        daemon.end_session(&id);
        daemon.mark_exited(&id, status.clone());
        // After the exit is recorded, so a reader of the event stream
        // sees the agent end before it sees it start again.
        daemon.consider_restart(&id, &status);
    })
}

pub(crate) fn exit_status(report: &ExitReport) -> AgentStatus {
    match (report.code, report.signal) {
        (None, None) if !report.log_flushed => AgentStatus::Failed {
            reason: "the command could not be waited for".into(),
        },
        _ => AgentStatus::Exited { code: report.code },
    }
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
