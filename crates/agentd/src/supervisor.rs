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
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
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

/// Longest report line accepted from an owner: a scrollback replay is at
/// most 64 KiB of bytes rendered as JSON numbers, well under this.
const MAX_REPORT_BYTES: usize = 1024 * 1024;
/// How long one write to an owner may block before the link is dead.
const WRITE_WITHIN: Duration = Duration::from_secs(5);

struct Controller {
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
    /// A report read so far: kept across a cancelled read, so a frame split
    /// by another select branch winning resumes where it stopped.
    partial: Vec<u8>,
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
            reader: BufReader::new(reader),
            writer,
            partial: Vec::new(),
        })
    }

    async fn send(&mut self, command: &OwnerCommand) -> anyhow::Result<()> {
        let mut line = serde_json::to_vec(command)?;
        line.push(b'\n');
        tokio::time::timeout(WRITE_WITHIN, self.writer.write_all(&line))
            .await
            .context("session owner stopped reading")?
            .context("session owner connection closed")
    }

    /// One line, bounded; `None` at EOF. Cancel-safe: bytes already read
    /// stay in `partial` until a whole frame is parsed.
    async fn line(&mut self) -> anyhow::Result<Option<String>> {
        read_frame(&mut self.reader, &mut self.partial, MAX_REPORT_BYTES)
            .await
            .context("session owner report")
    }

    async fn hello(&mut self) -> anyhow::Result<OwnerHello> {
        let line = tokio::time::timeout(WRITE_WITHIN, self.line())
            .await
            .context("session owner did not say hello in time")??
            .context("session owner closed before its hello")?;
        let hello: OwnerHello = serde_json::from_str(&line).context("malformed owner hello")?;
        anyhow::ensure!(hello.format == FORMAT, "unknown session owner format");
        Ok(hello)
    }

    async fn next(&mut self) -> anyhow::Result<Option<OwnerReport>> {
        loop {
            let Some(line) = self.line().await? else {
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

/// One newline-delimited frame from `reader`, accumulating into `partial`
/// so a read cancelled mid-frame loses nothing: the next call continues.
/// `None` at a clean EOF; an EOF mid-frame or a frame past `max` is an
/// error. Shared by both ends of the owner wire.
pub(crate) async fn read_frame<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
    partial: &mut Vec<u8>,
    max: usize,
) -> anyhow::Result<Option<String>> {
    loop {
        if let Some(end) = partial.iter().position(|b| *b == b'\n') {
            let frame = partial.drain(..=end).collect::<Vec<u8>>();
            let text = String::from_utf8(frame[..frame.len() - 1].to_vec())
                .context("frame is not UTF-8")?;
            return Ok(Some(text));
        }
        anyhow::ensure!(partial.len() <= max, "frame exceeds {max} bytes");
        let budget = (max + 1 - partial.len()) as u64;
        let read = reader.take(budget).read_until(b'\n', partial).await?;
        if read == 0 {
            anyhow::ensure!(partial.is_empty(), "connection closed mid-frame");
            return Ok(None);
        }
    }
}

/// Whether the recorded owner process is still the one that was recorded:
/// same pid, same birth. A recycled pid is not an owner.
fn owner_alive(owner: &SessionOwner) -> bool {
    agentdocker_host::procinfo::start_time(owner.pid) == Some(owner.started_at)
}

fn read_exit_file(path: &Path) -> Option<ExitReport> {
    let text = std::fs::read(path).ok()?;
    serde_json::from_slice(&text).ok()
}

/// Check that the owner answering on the socket is the one on the record,
/// holding the child on the record. Anything else is a stranger.
fn validate_identity(
    hello: &OwnerHello,
    owner: &SessionOwner,
    child_pid: u32,
    child_started_at: chrono::DateTime<Utc>,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        hello.owner_pid == owner.pid && hello.owner_started_at == owner.started_at,
        "session owner identity differs from the record"
    );
    let child = hello.child.as_ref().context("owner has no child")?;
    anyhow::ensure!(
        child.pid == child_pid && child.started_at == child_started_at,
        "session owner holds a different child than the record"
    );
    Ok(())
}

/// Find an owner that outlived the previous daemon and take its child
/// back under supervision. The exit file answers for an owner that has
/// already finished. Identities are checked against the record: a
/// recycled owner pid or a different child is refused.
pub async fn reattach(daemon: &Daemon, record: &AgentRecord) -> anyhow::Result<Reattached> {
    let socket = socket_path(&daemon.home, &record.id);
    let exit = exit_path(&daemon.home, &record.id);
    let owner = record
        .owner
        .clone()
        .context("record has no session owner")?;
    let child_pid = record.pid.context("record has no child pid")?;
    let child_started_at = record
        .process_started_at
        .context("record has no child birth")?;
    if let Some(report) = read_exit_file(&exit) {
        // Finished while nobody watched; the owner may still be waiting to
        // hear that this was recorded.
        return Ok(Reattached::Exited(report));
    }
    anyhow::ensure!(
        owner_alive(&owner),
        "session owner process is gone without an exit report"
    );
    let mut controller = Controller::connect(&socket, Duration::from_millis(500)).await?;
    let hello = controller.hello().await?;
    validate_identity(&hello, &owner, child_pid, child_started_at)?;
    let child = hello.child.clone().expect("validated");
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
        self.show(fresh);
    }

    /// Bytes `..to` scrolled out of the owner's retention while nobody was
    /// attached: say so on the screen, and resume counting at `to` so the
    /// bytes that follow are not mistaken for already shown.
    fn note_gap(&mut self, to: u64) {
        if to > self.relayed {
            self.show(b"\r\n[agentdocker: output gap; see logs]\r\n");
            self.relayed = to;
        }
    }

    fn show(&self, bytes: &[u8]) {
        if let (Some(scrollback), Some(output)) = (&self.scrollback, &self.output) {
            let mut history = lock_scrollback(scrollback);
            history.extend(bytes.iter().copied());
            let excess = history.len().saturating_sub(SCROLLBACK);
            history.drain(..excess);
            let _ = output.send(bytes.to_vec());
        }
    }

    /// Reconnect to the same owner after the transport dropped: the owner
    /// and child must be the ones this supervision started with, and the
    /// screen resumes from the last byte shown.
    async fn reconnect(&mut self, home: &Path, id: &AgentId) -> anyhow::Result<()> {
        let socket = socket_path(home, id);
        let mut controller = Controller::connect(&socket, Duration::from_secs(10)).await?;
        let hello = controller.hello().await?;
        validate_identity(&hello, &self.owner, self.pid, self.process_started_at)?;
        if self.session.is_some() {
            controller
                .send(&OwnerCommand::Attach {
                    after: self.relayed,
                })
                .await?;
        }
        self.controller = controller;
        Ok(())
    }
}

/// How supervision ended: with the owner's exit report, or without one.
enum Outcome {
    Exited(ExitReport),
    Failed(String),
}

/// Relay until the owner reports the exit or is gone for good. A dropped
/// transport is not an exit: while the owner process lives, reconnect and
/// carry on, so a daemon hiccup never releases a running agent's leases.
async fn relay_until_exit(
    daemon: &Daemon,
    id: &AgentId,
    spawned: &mut Spawned,
    keystrokes: &mut Option<mpsc::Receiver<Vec<u8>>>,
    resizes: &mut Option<mpsc::Receiver<(u16, u16)>>,
) -> Outcome {
    let exit_file = exit_path(&daemon.home, id);
    loop {
        let dropped = loop {
            tokio::select! {
                biased;
                report = spawned.controller.next() => match report {
                    Ok(Some(OwnerReport::Output { offset, bytes })) => spawned.relay(offset, &bytes),
                    Ok(Some(OwnerReport::Gap { from, to })) => {
                        tracing::info!(agent = %id, from, to, "terminal output skipped ahead after a gap");
                        spawned.note_gap(to);
                    }
                    Ok(Some(OwnerReport::InputDropped)) => {
                        // Said on the screen and in the event stream: bytes
                        // the client saw accepted were not typed, and nothing
                        // replays them.
                        tracing::warn!(agent = %id, "terminal input was dropped: the agent is not reading its terminal");
                        spawned.show(b"\r\n[agentdocker: input dropped, the agent is not reading its terminal; retype it]\r\n");
                        daemon.emit(agentdocker_core::EventKind::AgentInputDropped { agent: id.clone() });
                    }
                    Ok(Some(OwnerReport::OutputFailed { reason })) => {
                        tracing::warn!(agent = %id, %reason, "agent output capture failed");
                        daemon.emit(agentdocker_core::EventKind::AgentOutputFailed {
                            agent: id.clone(),
                            reason,
                        });
                    }
                    Ok(Some(OwnerReport::Exited { status })) => return Outcome::Exited(status),
                    Ok(Some(_)) => {}
                    Ok(None) => break None,
                    Err(error) => break Some(error),
                },
                Some(bytes) = async { keystrokes.as_mut().expect("guarded").recv().await }, if keystrokes.is_some() => {
                    if spawned.controller.send(&OwnerCommand::Input { bytes }).await.is_err() {
                        *keystrokes = None;
                    }
                }
                Some((cols, rows)) = async { resizes.as_mut().expect("guarded").recv().await }, if resizes.is_some() => {
                    if spawned.controller.send(&OwnerCommand::Resize { cols, rows }).await.is_err() {
                        *resizes = None;
                    }
                }
                Ok(()) = spawned.stop.changed() => {
                    let pending = *spawned.stop.borrow_and_update();
                    if let Some(force) = pending {
                        let _ = spawned.controller.send(&OwnerCommand::Stop { force }).await;
                    }
                }
            }
        };
        // The transport went away. The exit file, if any, is the truth;
        // otherwise a living owner is reconnected and a dead one is lost.
        if let Some(report) = read_exit_file(&exit_file) {
            return Outcome::Exited(report);
        }
        if let Some(error) = &dropped {
            tracing::warn!(agent = %id, %error, "session owner link failed");
        }
        // Transport unavailable is not the agent gone: keep trying for as
        // long as the owner process lives, and only its death, or its exit
        // report, ends supervision.
        loop {
            if let Some(report) = read_exit_file(&exit_file) {
                return Outcome::Exited(report);
            }
            if !owner_alive(&spawned.owner) {
                return Outcome::Failed("session owner lost".into());
            }
            match spawned.reconnect(&daemon.home, id).await {
                Ok(()) => {
                    tracing::info!(agent = %id, "reconnected to the session owner");
                    // A stop asked for during the outage is still owed.
                    let pending = *spawned.stop.borrow();
                    if let Some(force) = pending {
                        let _ = spawned.controller.send(&OwnerCommand::Stop { force }).await;
                    }
                    break;
                }
                Err(error) => {
                    tracing::warn!(agent = %id, %error, "session owner unreachable; retrying while it lives");
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            }
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
        let outcome = if !spawned.activated {
            // The launch record never became durable: tell the owner to deny
            // exec now rather than at its own deadline; reaping the link
            // below waits for it to go.
            let reason = spawned
                .launch_error
                .take()
                .unwrap_or_else(|| "launch was not activated".into());
            let _ = spawned
                .controller
                .send(&OwnerCommand::Stop { force: true })
                .await;
            Outcome::Failed(reason)
        } else {
            let mut keystrokes = spawned.keystrokes.take();
            let mut resizes = spawned.resizes.take();
            relay_until_exit(&daemon, &id, &mut spawned, &mut keystrokes, &mut resizes).await
        };
        daemon.end_session(&id);
        let status = match outcome {
            Outcome::Exited(report) => {
                let status = exit_status(&report);
                // Durable first, then acknowledged: an exit the store did not
                // keep stays in the exit file for a daemon that can keep it.
                if daemon.mark_exited(&id, status.clone()).is_some() {
                    let _ = spawned.controller.send(&OwnerCommand::Acknowledge).await;
                    let _ = std::fs::remove_file(exit_path(&daemon.home, &id));
                } else {
                    tracing::warn!(agent = %id, "exit not recorded durably; the owner's exit report is kept");
                }
                status
            }
            Outcome::Failed(reason) => {
                let status = AgentStatus::Failed { reason };
                daemon.mark_exited(&id, status.clone());
                status
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A frame split across reads, with the read cancelled between the
    /// halves as another select branch winning would cancel it, still
    /// arrives whole: the partial bytes are the connection's, not the
    /// call's. Split UTF-8 and a following frame are included.
    #[tokio::test]
    async fn a_cancelled_read_keeps_the_partial_frame() {
        let (mut client, server) = tokio::net::UnixStream::pair().unwrap();
        let (reader, _writer) = server.into_split();
        let mut reader = BufReader::new(reader);
        let mut partial = Vec::new();
        let whole = "{\"event\":\"output\",\"offset\":0,\"bytes\":[195,169]} caf\u{e9}\n{\"event\":\"activated\"}\n";
        let bytes = whole.as_bytes();
        // Cut inside the two-byte é.
        let cut = whole.find("caf").unwrap() + 4;
        client.write_all(&bytes[..cut]).await.unwrap();
        // The first read sees no newline yet and is cancelled by a deadline.
        let cancelled = tokio::time::timeout(
            Duration::from_millis(100),
            read_frame(&mut reader, &mut partial, 1024),
        )
        .await;
        assert!(cancelled.is_err(), "no whole frame yet");
        assert_eq!(partial, &bytes[..cut], "the half read stays");
        client.write_all(&bytes[cut..]).await.unwrap();
        let first = read_frame(&mut reader, &mut partial, 1024)
            .await
            .unwrap()
            .unwrap();
        assert!(first.ends_with("caf\u{e9}"), "{first}");
        let second = read_frame(&mut reader, &mut partial, 1024)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(second, "{\"event\":\"activated\"}");
        assert!(partial.is_empty());
        // An oversized frame is refused, never buffered without bound.
        client.write_all(&vec![b'x'; 2048]).await.unwrap();
        let error = read_frame(&mut reader, &mut partial, 1024)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("exceeds"), "{error}");
        // EOF mid-frame is an error, EOF between frames is the end.
        let (mut client, server) = tokio::net::UnixStream::pair().unwrap();
        let (reader, _writer) = server.into_split();
        let mut reader = BufReader::new(reader);
        let mut partial = Vec::new();
        client
            .write_all(b"{\"event\":\"activated\"}\nhalf")
            .await
            .unwrap();
        drop(client);
        assert!(
            read_frame(&mut reader, &mut partial, 1024)
                .await
                .unwrap()
                .is_some()
        );
        assert!(read_frame(&mut reader, &mut partial, 1024).await.is_err());
    }
}
