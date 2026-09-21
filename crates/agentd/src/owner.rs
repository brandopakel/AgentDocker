//! The session owner: a small process that holds one managed agent's child,
//! its terminal or pipes, and its log, so the daemon that launched it can
//! be replaced or die without the agent noticing.
//!
//! The daemon starts one owner per `run`. The owner prepares the command
//! behind the launch gate exactly as the daemon used to, then serves the
//! daemon on `<home>/sessions/<agent>.sock` with the wire in
//! [`agentdocker_core::session`]. The daemon is the controller: it says
//! when the launch record is durable (`Activate`), relays keystrokes and
//! window sizes, asks for output from an offset, and acknowledges the exit
//! report. The owner reports the prepared identity, every byte of output
//! with its offset, and the exit status once, kept in an exit file until a
//! controller has recorded it.
//!
//! Nothing here reads coordination state: the owner knows its command, its
//! log path and its socket, and nothing else.

use std::collections::{BTreeMap, VecDeque};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
#[cfg(unix)]
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use agentdocker_core::AgentId;
use agentdocker_core::session::{
    ACTIVATE_WITHIN_SECS, ChildIdentity, ExitReport, FORMAT, OwnerCommand, OwnerHello, OwnerReport,
    endpoint, exit_path, socket_path,
};
use agentdocker_host::ipc::{Listener, Stream};
use agentdocker_host::launch::{OwnedChild, Pending};
use agentdocker_host::lock;
use anyhow::Context;
use chrono::Utc;
#[cfg(unix)]
use nix::sys::signal::{Signal, kill};
#[cfg(unix)]
use nix::unistd::Pid;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, broadcast, mpsc, watch};

/// What the daemon hands an owner on its stdin: everything needed to
/// prepare the command, and nothing about coordination.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Launch {
    pub format: u32,
    pub agent: AgentId,
    pub name: String,
    pub command: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub workdir: Option<PathBuf>,
    pub tty: bool,
    pub home: PathBuf,
    pub socket: PathBuf,
    pub log: PathBuf,
}

/// The terminal the owner keeps for as long as the child lives: the master
/// descriptor on Unix; on Windows the pseudo console itself, which must not
/// close before the child does.
#[cfg(unix)]
type Terminal = std::os::fd::OwnedFd;
#[cfg(windows)]
type Terminal = agentdocker_host::pty::Pty;

fn resize_terminal(terminal: &Terminal, cols: u16, rows: u16) -> std::io::Result<()> {
    #[cfg(unix)]
    return agentdocker_host::pty::set_window_size(
        std::os::fd::AsRawFd::as_raw_fd(terminal),
        cols,
        rows,
    );
    #[cfg(windows)]
    terminal.resize(cols, rows)
}

/// The child's process group as this platform ends it: on Unix the group
/// signalled by `-pid`, asked with `SIGTERM` and ended with `SIGKILL`; on
/// Windows the job the child was assigned to before it ran, asked — for a
/// console child — with a Ctrl-C typed into its console (a piped child has
/// nothing to be asked with, so the ask is the end) and ended by ending
/// the job.
#[cfg(unix)]
struct Group(Pid, u32);
#[cfg(unix)]
impl Group {
    fn of(child: &Pending, _input: Option<&mpsc::Sender<Vec<u8>>>) -> anyhow::Result<Self> {
        Ok(Self(Pid::from_raw(-(child.pid as i32)), child.pid))
    }
    fn ask(&self) {
        let _ = kill(self.0, Signal::SIGTERM);
    }
    fn end(&self) {
        let _ = kill(self.0, Signal::SIGKILL);
    }
    fn exists(&self) -> bool {
        crate::supervisor::group_exists(self.1)
    }
}
#[cfg(windows)]
struct Group {
    job: agentdocker_host::launch::Job,
    input: Option<mpsc::Sender<Vec<u8>>>,
}
#[cfg(windows)]
impl Group {
    fn of(child: &Pending, input: Option<&mpsc::Sender<Vec<u8>>>) -> anyhow::Result<Self> {
        Ok(Self {
            job: child.job().context("cannot hold the agent's job")?,
            input: input.cloned(),
        })
    }
    fn ask(&self) {
        // Never write a pipe on the supervision loop: a child that does
        // not read its keyboard must not block the force-stop deadline.
        let typed = self
            .input
            .as_ref()
            .and_then(|input| input.try_send(vec![0x03]).ok());
        if typed.is_none() {
            self.end();
        }
    }
    fn end(&self) {
        let _ = self.job.end();
    }
    fn exists(&self) -> bool {
        self.job.exists()
    }
}

/// What a client attaching late is shown before the live stream.
const SCROLLBACK: usize = 64 * 1024;
/// After the exit report is written, how long to wait for a controller to
/// acknowledge it before exiting anyway; the exit file remains either way.
const ACKNOWLEDGE_WITHIN: Duration = Duration::from_secs(60);
/// How long a stopped group gets before SIGKILL, as the daemon allowed.
const STOP_GRACE: Duration = Duration::from_secs(2);

/// Chunks on the live channel carry their byte offset. Three offsets no
/// output can reach are markers instead: the controller reacts to what
/// changed rather than to bytes.
const MARK_ACTIVATED: u64 = u64::MAX;
const MARK_OUTPUT_FAILED: u64 = u64::MAX - 1;
const MARK_EXITED: u64 = u64::MAX - 2;

/// Output as the owner keeps it: the last [`SCROLLBACK`] bytes, the offset
/// of the first retained byte, and the offset the next byte will get.
struct Output {
    scrollback: VecDeque<u8>,
    first: u64,
    next: u64,
}

impl Output {
    fn push(&mut self, chunk: &[u8]) -> u64 {
        let offset = self.next;
        self.scrollback.extend(chunk.iter().copied());
        let excess = self.scrollback.len().saturating_sub(SCROLLBACK);
        if excess > 0 {
            self.scrollback.drain(..excess);
            self.first += excess as u64;
        }
        self.next += chunk.len() as u64;
        offset
    }

    /// What a controller that already relayed everything before `after`
    /// still needs: the retained bytes from that offset, or from the
    /// oldest retained byte when `after` has already scrolled away.
    fn since(&self, after: u64) -> (u64, Vec<u8>) {
        let start = after.max(self.first);
        let skip = usize::try_from(start - self.first).unwrap_or(usize::MAX);
        (start, self.scrollback.iter().skip(skip).copied().collect())
    }
}

/// Everything the connection tasks share.
struct Shared {
    launch: Launch,
    /// This owner's own identity, as the record and the exit report carry it.
    owner: agentdocker_core::session::SessionOwner,
    hello: Mutex<OwnerHello>,
    output: Arc<std::sync::Mutex<Output>>,
    /// Live output: the offset of the chunk and the chunk.
    live: broadcast::Sender<(u64, Vec<u8>)>,
    /// Keystrokes for the terminal; absent for a piped command.
    input: Option<mpsc::Sender<Vec<u8>>>,
    terminal: Option<Arc<Terminal>>,
    /// The controller's decisions: activate, stop (with force), acknowledge.
    activate: watch::Sender<bool>,
    stop: watch::Sender<Option<bool>>,
    acknowledged: watch::Sender<bool>,
    /// The final report, once there is one, for controllers that connect
    /// after the exit.
    exit: std::sync::Mutex<Option<ExitReport>>,
    /// One controller at a time: the newest connection wins, and a task
    /// serving an older one stops relaying and stops obeying. A daemon that
    /// replaced another must not share the child with its predecessor.
    epoch: std::sync::atomic::AtomicU64,
    /// Takeover and command acceptance are one line: a takeover bumps the
    /// epoch under this lock, and a command is checked and applied under
    /// it, so no stale command lands after a newer controller arrived.
    dispatch: Mutex<()>,
}

/// Longest command line accepted from a controller: a 64 KiB keystroke
/// frame is up to four JSON characters per byte plus framing, so half a
/// mebibyte covers it with room; everything else is a few bytes.
const MAX_COMMAND_BYTES: usize = 512 * 1024;
/// How long one write to a controller may block before it counts as gone.
const WRITE_WITHIN: Duration = Duration::from_secs(5);

fn lock<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Run an owner to completion. Returns the process exit code.
pub fn main(launch: Launch) -> anyhow::Result<i32> {
    anyhow::ensure!(launch.format == FORMAT, "unknown launch format");
    // Our own session: the daemon signals the agent's group, never ours,
    // and a daemon that dies must not take us with it. On Windows the
    // daemon starts us detached in our own group, which is the same thing.
    #[cfg(unix)]
    {
        let _ = nix::unistd::setsid();
        // SAFETY: setting a disposition to SIG_IGN has no handler to be unsafe.
        unsafe {
            let _ = nix::sys::signal::signal(Signal::SIGHUP, nix::sys::signal::SigHandler::SigIgn);
        }
    }
    // Hold the installed release this executable belongs to for as long as
    // we own a child, so maintenance cannot prune an occupied release after
    // the daemon that launched us has exited.
    let _pin = agentdocker_host::installation::pin_current_executable()
        .context("cannot pin the session owner's installed release")?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(serve(launch))
}

pub(crate) async fn serve(launch: Launch) -> anyhow::Result<i32> {
    let owner_started_at = agentdocker_host::procinfo::start_time(std::process::id())
        .context("cannot read the session owner's own process birth; refusing to own a child")?;
    let mut command = Command::new(&launch.command[0]);
    command
        .args(&launch.command[1..])
        .envs(&launch.env)
        .env("AGENTDOCKER_HOME", &launch.home)
        .env("AGENTDOCKER_SOCKET", &launch.socket)
        .env_remove("AGENTDOCKER_TOKEN_FILE")
        .env("AGENTDOCKER_NO_AUTOSTART", "1")
        .env("AGENTDOCKER_AGENT_ID", launch.agent.as_str())
        .env("AGENTDOCKER_AGENT_NAME", &launch.name)
        .env(
            "TERM",
            std::env::var("TERM").as_deref().unwrap_or("xterm-256color"),
        );
    let mut pty = if launch.tty {
        Some(agentdocker_host::pty::Pty::open().context("cannot open a terminal for the agent")?)
    } else {
        None
    };
    // On Unix the child is handed the slave as its standard streams and
    // claims it as its controlling terminal between fork and exec; a piped
    // child gets its own process group. On Windows the console is bound
    // to the child at creation, and the group is a job (see `launch`).
    #[cfg(unix)]
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
    if let Some(workdir) = &launch.workdir {
        command.current_dir(workdir);
    }
    // This side of the terminal, to read what the agent writes and to type
    // into it, as plain files either platform's pump can drive.
    let terminal_io: Option<(std::fs::File, std::fs::File)> = pty
        .as_ref()
        .map(|pty| -> std::io::Result<_> {
            #[cfg(unix)]
            return Ok((
                std::fs::File::from(pty.master().try_clone()?),
                std::fs::File::from(pty.master().try_clone()?),
            ));
            #[cfg(windows)]
            Ok((pty.reader()?, pty.writer()?))
        })
        .transpose()
        .context("cannot clone the agent's terminal")?;
    #[cfg(windows)]
    let console = pty.as_mut().map(|pty| pty.console()).transpose()?;
    let log_path = launch.log.clone();
    let log = tokio::task::spawn_blocking(move || {
        agentdocker_host::dirs::secure_state_dir(
            log_path.parent().expect("log path has a parent"),
        )?;
        agentdocker_host::dirs::private_file(&log_path, true, true)
            .with_context(|| format!("cannot open {}", log_path.display()))
    })
    .await??;
    let log = tokio::fs::File::from_std(log);

    // The control socket exists before the command is prepared, so the
    // daemon's connect never races the gate.
    let socket = socket_path(&launch.home, &launch.agent);
    // The private directory the sessions live under, then ours inside it.
    let sessions = socket.parent().expect("socket has a parent").to_path_buf();
    agentdocker_host::dirs::secure_state_dir(
        sessions.parent().expect("sessions dir has a parent"),
    )?;
    agentdocker_host::dirs::secure_state_dir(&sessions)?;
    // Exclusive for this agent id for as long as we live: a second owner
    // for the same id, or a stale socket, is never silently taken over.
    // A previous owner for this id may still be finishing (a restart
    // relaunches the moment the exit is recorded, before the old owner has
    // returned); wait briefly for its lock, never take it by force.
    let lock_path = socket.with_extension("lock");
    let lock_deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let _owner_lock = loop {
        match lock::try_exclusive(&lock_path).context("cannot lock the session")? {
            Some(held) => break held,
            None if tokio::time::Instant::now() >= lock_deadline => {
                anyhow::bail!("another session owner holds this agent")
            }
            None => tokio::time::sleep(Duration::from_millis(50)).await,
        }
    };
    let serving = endpoint(&launch.home, &launch.agent);
    #[cfg(unix)]
    let _ = std::fs::remove_file(&serving);
    let listener = Listener::bind(&serving)
        .with_context(|| format!("cannot listen on {}", serving.display()))?;
    #[cfg(unix)]
    std::fs::set_permissions(
        &serving,
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o600),
    )?;

    let program = launch.command[0].clone();
    #[cfg(unix)]
    let prepare = move || agentdocker_host::launch::prepare(command);
    #[cfg(windows)]
    let prepare = move || agentdocker_host::launch::prepare_with(command, console);
    let pending = tokio::task::spawn_blocking(prepare)
        .await?
        .with_context(|| format!("failed to prepare `{program}`"))?;
    let child = ChildIdentity {
        pid: pending.pid,
        started_at: agentdocker_host::procinfo::start_time(pending.pid)
            .context("cannot verify the prepared command's process identity; exec denied")?,
        tty: launch.tty,
    };

    let (live, _) = broadcast::channel::<(u64, Vec<u8>)>(256);
    let (activate, activated) = watch::channel(false);
    let (stop, stopped) = watch::channel(None);
    let (acknowledged, acknowledgement) = watch::channel(false);
    let (input, keystrokes) = match &pty {
        Some(_) => {
            let (tx, rx) = mpsc::channel::<Vec<u8>>(64);
            (Some(tx), Some(rx))
        }
        None => (None, None),
    };
    #[cfg(unix)]
    let terminal = pty.map(|pty| Arc::new(pty.into_master()));
    #[cfg(windows)]
    let terminal = pty.map(Arc::new);
    let shared = Arc::new(Shared {
        owner: agentdocker_core::session::SessionOwner {
            pid: std::process::id(),
            started_at: owner_started_at,
        },
        hello: Mutex::new(OwnerHello {
            format: FORMAT,
            agent: launch.agent.clone(),
            owner_pid: std::process::id(),
            owner_started_at,
            child: Some(child.clone()),
            activated: false,
            output_offset: 0,
        }),
        launch,
        output: Arc::new(std::sync::Mutex::new(Output {
            scrollback: VecDeque::new(),
            first: 0,
            next: 0,
        })),
        live,
        input,
        terminal,
        activate,
        stop,
        acknowledged,
        exit: std::sync::Mutex::new(None),
        epoch: std::sync::atomic::AtomicU64::new(0),
        dispatch: Mutex::new(()),
    });

    // Controllers come and go; the child does not notice.
    let accepting = {
        let shared = shared.clone();
        tokio::spawn(async move {
            // Every connection task is owned here so none outlives the
            // owner in-process, and finished ones are reaped as they end.
            let mut connections = tokio::task::JoinSet::new();
            let mut current: Option<tokio::task::AbortHandle> = None;
            loop {
                tokio::select! {
                    accepted = listener.accept() => match accepted {
                        Ok((stream, _)) => {
                            let epoch = {
                                let _takeover = shared.dispatch.lock().await;
                                shared
                                    .epoch
                                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                                    + 1
                            };
                            // The superseded controller is cancelled outright:
                            // one blocked on an idle owner would otherwise wait
                            // for an event to learn it had been replaced.
                            if let Some(previous) = current.take() {
                                previous.abort();
                            }
                            let shared = shared.clone();
                            current = Some(connections.spawn(async move {
                                let _ = controller(stream, shared, epoch).await;
                            }));
                        }
                        Err(error) => {
                            tracing::warn!(%error, "owner accept failed");
                            tokio::time::sleep(Duration::from_millis(50)).await;
                        }
                    },
                    Some(_) = connections.join_next(), if !connections.is_empty() => {}
                }
            }
        })
    };

    let code = supervise(
        shared.clone(),
        pending,
        child,
        log,
        terminal_io,
        keystrokes,
        activated,
        stopped,
        acknowledgement,
    )
    .await;
    accepting.abort();
    #[cfg(unix)]
    let _ = std::fs::remove_file(&serving);
    // The lock file stays: unlinking it would let an opener paused between
    // open and flock lock the old inode while a third locks the new one.
    Ok(code)
}

#[allow(clippy::too_many_arguments)]
async fn supervise(
    shared: Arc<Shared>,
    pending: Pending,
    child: ChildIdentity,
    log: tokio::fs::File,
    terminal_io: Option<(std::fs::File, std::fs::File)>,
    keystrokes: Option<mpsc::Receiver<Vec<u8>>>,
    mut activated: watch::Receiver<bool>,
    mut stopped: watch::Receiver<Option<bool>>,
    mut acknowledgement: watch::Receiver<bool>,
) -> i32 {
    // Nothing runs until the daemon says the launch is durable, and an
    // abandoned launch never runs at all.
    let authorised = tokio::time::timeout(Duration::from_secs(ACTIVATE_WITHIN_SECS), async {
        loop {
            if *activated.borrow() {
                return true;
            }
            if stopped.borrow().is_some() {
                return false;
            }
            tokio::select! {
                changed = activated.changed() => if changed.is_err() { return false; },
                changed = stopped.changed() => if changed.is_err() { return false; },
            }
        }
    })
    .await
    .unwrap_or(false);
    if !authorised {
        drop(pending);
        tracing::info!(agent = %shared.launch.agent, "launch was never activated; exec denied");
        return 3;
    }
    // Acquire every fallible ownership handle before activation. Reporting
    // a failed group clone after starting the child would publish Exited
    // while the unowned command was still allowed to run.
    let group = match Group::of(&pending, shared.input.as_ref()) {
        Ok(group) => group,
        Err(error) => {
            drop(pending);
            return finish_failed(&shared, &child, format!("{error:#}"), &mut acknowledgement)
                .await;
        }
    };
    let mut owned = match tokio::task::spawn_blocking(move || pending.activate()).await {
        Ok(Ok(child)) => child,
        Ok(Err(error)) => {
            return finish_failed(&shared, &child, format!("{error:#}"), &mut acknowledgement)
                .await;
        }
        Err(error) => {
            return finish_failed(&shared, &child, error.to_string(), &mut acknowledgement).await;
        }
    };
    {
        let mut hello = shared.hello.lock().await;
        hello.activated = true;
    }
    let _ = shared.live.send((MARK_ACTIVATED, Vec::new()));

    let (lines, sink) = mpsc::channel::<String>(256);
    let mut tasks = tokio::task::JoinSet::new();
    // The log writer is joined on its own: its result is what `log_flushed`
    // means, and it ends only after every pump has dropped its sender.
    let log_task = tokio::spawn(write_log(log, sink));
    let mut input_task = None;
    match terminal_io {
        Some((reader, writer)) => {
            tasks.spawn(pump_terminal(
                tokio::fs::File::from_std(reader),
                lines,
                shared.clone(),
            ));
            if let Some(keystrokes) = keystrokes {
                input_task = Some(tokio::spawn(type_into_terminal(
                    tokio::fs::File::from_std(writer),
                    keystrokes,
                )));
            }
        }
        None => {
            // Unix pipes are read as the async child streams they are;
            // Windows pipe ends are files read on the blocking pool.
            #[cfg(unix)]
            {
                if let Some(stdout) = owned.take_stdout() {
                    tasks.spawn(pump(
                        tokio::process::ChildStdout::from_std(stdout)
                            .expect("stdout is nonblocking"),
                        "out",
                        lines.clone(),
                    ));
                }
                if let Some(stderr) = owned.take_stderr() {
                    tasks.spawn(pump(
                        tokio::process::ChildStderr::from_std(stderr)
                            .expect("stderr is nonblocking"),
                        "err",
                        lines,
                    ));
                }
            }
            #[cfg(windows)]
            {
                if let Some(stdout) = owned.take_stdout() {
                    tasks.spawn(pump(
                        tokio::fs::File::from_std(stdout),
                        "out",
                        lines.clone(),
                    ));
                }
                if let Some(stderr) = owned.take_stderr() {
                    tasks.spawn(pump(tokio::fs::File::from_std(stderr), "err", lines));
                }
            }
        }
    }

    let mut stopping = false;
    let mut deadline = tokio::time::Instant::now();
    let mut output_error: Option<String> = None;
    let status = loop {
        tokio::select! {
            biased;
            result = wait_owned_child(&mut owned) => break result,
            Some(result) = tasks.join_next(), if !tasks.is_empty() => {
                let error = match result {
                    Ok(Ok(())) => None,
                    Ok(Err(error)) => Some(format!("{error:#}")),
                    Err(error) => Some(format!("output task failed: {error}")),
                };
                if let Some(error) = error && output_error.is_none() {
                    output_error = Some(error);
                    group.ask();
                    if !stopping {
                        deadline = tokio::time::Instant::now() + STOP_GRACE;
                        stopping = true;
                    }
                }
            }
            Ok(()) = stopped.changed() => {
                if let Some(force) = *stopped.borrow_and_update() {
                    if force { group.end() } else { group.ask() }
                    if !stopping {
                        deadline = tokio::time::Instant::now() + STOP_GRACE;
                        stopping = true;
                    }
                }
            }
            () = tokio::time::sleep_until(deadline), if stopping => {
                group.end();
                stopping = false;
            }
        }
    };
    // Descendants stop before the exit is reported, so leases released on
    // that report cover nothing still running.
    if group.exists() {
        group.ask();
        let deadline = tokio::time::Instant::now() + STOP_GRACE;
        while group.exists() {
            if tokio::time::Instant::now() >= deadline {
                group.end();
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
    if let Some(input) = input_task {
        input.abort();
        let _ = input.await;
    }
    // ConPTY owns the output pipe's write end even after the child exits.
    // The pump holds Shared (and thus the terminal), so waiting for EOF
    // before closing it deadlocks. Close on another thread while the pump
    // continues draining, preserving the final bytes on older Windows too.
    #[cfg(windows)]
    if let Some(terminal) = shared.terminal.clone()
        && let Err(error) = tokio::task::spawn_blocking(move || terminal.close()).await
        && output_error.is_none()
    {
        output_error = Some(format!("cannot close the agent's console: {error}"));
    }
    while let Some(result) = tasks.join_next().await {
        if let Ok(Err(error)) = result
            && output_error.is_none()
        {
            output_error = Some(format!("{error:#}"));
        }
    }
    // Pumps are done, so the log channel is closed; the writer's own
    // result says whether every line reached the file.
    let log_flushed = matches!(log_task.await, Ok(Ok(())));
    if let Some(reason) = output_error {
        let _ = shared.live.send((MARK_OUTPUT_FAILED, reason.into_bytes()));
    }
    let report = match status {
        Ok(exit) => ExitReport {
            agent: shared.launch.agent.clone(),
            owner: shared.owner.clone(),
            child: child.clone(),
            code: exit.code(),
            #[cfg(unix)]
            signal: std::os::unix::process::ExitStatusExt::signal(&exit),
            #[cfg(windows)]
            signal: None,
            log_flushed,
            at: Utc::now(),
        },
        Err(error) => {
            tracing::warn!(agent = %shared.launch.agent, %error, "cannot wait for the agent");
            ExitReport {
                agent: shared.launch.agent.clone(),
                owner: shared.owner.clone(),
                child: child.clone(),
                code: None,
                signal: None,
                log_flushed,
                at: Utc::now(),
            }
        }
    };
    finish(&shared, report, &mut acknowledgement).await
}

async fn finish_failed(
    shared: &Arc<Shared>,
    child: &ChildIdentity,
    reason: String,
    acknowledgement: &mut watch::Receiver<bool>,
) -> i32 {
    tracing::warn!(agent = %shared.launch.agent, %reason, "launch failed");
    let report = ExitReport {
        agent: shared.launch.agent.clone(),
        owner: shared.owner.clone(),
        child: child.clone(),
        code: None,
        signal: None,
        log_flushed: true,
        at: Utc::now(),
    };
    finish(shared, report, acknowledgement).await
}

/// Write the exit file, tell every controller, and wait (boundedly) for
/// one of them to say it has recorded the exit.
async fn finish(
    shared: &Arc<Shared>,
    report: ExitReport,
    acknowledgement: &mut watch::Receiver<bool>,
) -> i32 {
    let path = exit_path(&shared.launch.home, &shared.launch.agent);
    match serde_json::to_vec(&report) {
        Ok(bytes) => {
            if let Err(error) = write_durably(&path, &bytes) {
                tracing::error!(%error, path = %path.display(), "cannot write the exit report");
            }
        }
        Err(error) => tracing::error!(%error, "cannot encode the exit report"),
    }
    *lock(&shared.exit) = Some(report);
    let _ = shared.live.send((MARK_EXITED, Vec::new()));
    let acknowledged = tokio::time::timeout(ACKNOWLEDGE_WITHIN, async {
        loop {
            if *acknowledgement.borrow() {
                return true;
            }
            if acknowledgement.changed().await.is_err() {
                return false;
            }
        }
    })
    .await
    .unwrap_or(false);
    if !acknowledged {
        tracing::info!(agent = %shared.launch.agent, "exit not acknowledged in time; the exit file remains");
    }
    0
}

/// Write, sync, rename, sync the directory: the exit report either exists
/// whole after a crash or not at all.
fn write_durably(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let staged = path.with_extension("exit.staging");
    let mut file = std::fs::File::create(&staged)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    agentdocker_host::files::publish_staged(&staged, path)
}

/// One controller connection: hello, then commands in, reports out.
async fn controller(stream: Stream, shared: Arc<Shared>, epoch: u64) -> anyhow::Result<()> {
    // Only the newest connection's commands are obeyed; an older one is
    // told nothing more and dropped.
    let current = || shared.epoch.load(std::sync::atomic::Ordering::SeqCst) == epoch;
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    // Subscribe before any snapshot, so an exit or activation that lands
    // between the snapshot and the first receive is still heard.
    let mut live = shared.live.subscribe();
    let hello = {
        let mut hello = shared.hello.lock().await.clone();
        hello.output_offset = lock(&shared.output).next;
        hello
    };
    send(&mut writer, &hello).await?;
    if let Some(child) = &hello.child {
        send(
            &mut writer,
            &OwnerReport::Prepared {
                child: child.clone(),
            },
        )
        .await?;
    }
    if hello.activated {
        send(&mut writer, &OwnerReport::Activated).await?;
    }
    let already_exited = lock(&shared.exit).clone();
    if let Some(report) = already_exited {
        send(&mut writer, &OwnerReport::Exited { status: report }).await?;
    }
    // Every controller hears the markers (activation, output failure, exit)
    // from the moment it connects; output bytes are relayed only after it
    // asks with `Attach`, from the offset it names.
    let mut attached = false;
    let mut relayed: u64 = 0;
    let mut line = Vec::new();
    loop {
        tokio::select! {
            read = read_command(&mut reader, &mut line) => {
                let Some(command) = read? else {
                    // A controller that leaves before authorising the launch
                    // has given up on it: deny exec rather than run a
                    // command nobody recorded. A superseded controller's
                    // leaving says nothing: the newer one decides.
                    let _dispatch = shared.dispatch.lock().await;
                    if current() && !shared.hello.lock().await.activated {
                        let _ = shared.stop.send(Some(true));
                    }
                    return Ok(());
                };
                // Checked and applied under the dispatch lock, with nothing
                // awaited in between, so a takeover cannot interleave.
                let after_dispatch = {
                    let _dispatch = shared.dispatch.lock().await;
                    if !current() {
                        // Superseded: a newer controller owns this child now.
                        return Ok(());
                    }
                    match command {
                        OwnerCommand::Activate => {
                            let _ = shared.activate.send(true);
                            None
                        }
                        OwnerCommand::Stop { force } => {
                            let _ = shared.stop.send(Some(force));
                            None
                        }
                        OwnerCommand::Input { bytes } => match &shared.input {
                            Some(input) if input.try_send(bytes).is_err() => Some(Deferred::InputDropped),
                            _ => None,
                        },
                        OwnerCommand::Resize { cols, rows } => {
                            if let Some(terminal) = &shared.terminal {
                                let _ = resize_terminal(terminal, cols, rows);
                            }
                            None
                        }
                        OwnerCommand::Attach { after } => {
                            let (start, bytes) = lock(&shared.output).since(after);
                            relayed = start + bytes.len() as u64;
                            attached = true;
                            Some(Deferred::Replay { after, start, bytes })
                        }
                        OwnerCommand::Acknowledge => {
                            let _ = shared.acknowledged.send(true);
                            None
                        }
                    }
                };
                // Replies go out after the lock is released.
                match after_dispatch {
                    Some(Deferred::InputDropped) => send(&mut writer, &OwnerReport::InputDropped).await?,
                    Some(Deferred::Replay { after, start, bytes }) => {
                        if start > after {
                            send(&mut writer, &OwnerReport::Gap { from: after, to: start }).await?;
                        }
                        if !bytes.is_empty() {
                            send(&mut writer, &OwnerReport::Output { offset: start, bytes }).await?;
                        }
                    }
                    None => {}
                }
            }
            received = live.recv() => {
                if !current() {
                    return Ok(());
                }
                match received {
                    Ok((offset, bytes)) if offset >= MARK_EXITED => {
                        // A marker, not output: activation, an output
                        // failure, or the exit. Report what changed.
                        if offset == MARK_ACTIVATED {
                            send(&mut writer, &OwnerReport::Activated).await?;
                        } else if offset == MARK_OUTPUT_FAILED {
                            send(&mut writer, &OwnerReport::OutputFailed { reason: String::from_utf8_lossy(&bytes).into_owned() }).await?;
                        } else {
                            let report = lock(&shared.exit).clone();
                            if let Some(report) = report {
                                send(&mut writer, &OwnerReport::Exited { status: report }).await?;
                            }
                        }
                    }
                    Ok(_) if !attached => {}
                    Ok((offset, bytes)) => {
                        // Skip what the retained replay already covered.
                        let end = offset + bytes.len() as u64;
                        if end > relayed {
                            let skip = usize::try_from(relayed.saturating_sub(offset)).unwrap_or(0);
                            send(&mut writer, &OwnerReport::Output { offset: offset + skip as u64, bytes: bytes[skip.min(bytes.len())..].to_vec() }).await?;
                            relayed = end;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) if !attached => {}
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        // Resend from what is retained; the controller's
                        // offsets tell it what is new, and a gap is said.
                        let (start, bytes) = {
                            let output = lock(&shared.output);
                            output.since(relayed)
                        };
                        if start > relayed {
                            send(&mut writer, &OwnerReport::Gap { from: relayed, to: start }).await?;
                        }
                        if !bytes.is_empty() {
                            send(&mut writer, &OwnerReport::Output { offset: start, bytes: bytes.clone() }).await?;
                            relayed = start + bytes.len() as u64;
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => return Ok(()),
                }
            }
        }
    }
}

/// A reply owed after the dispatch lock is released.
enum Deferred {
    InputDropped,
    Replay {
        after: u64,
        start: u64,
        bytes: Vec<u8>,
    },
}

/// One bounded command line; `None` at EOF. `line` is the connection's
/// partial frame, kept across a cancelled read so nothing is lost when the
/// live-output branch wins the select mid-frame.
async fn read_command(
    reader: &mut BufReader<agentdocker_host::ipc::OwnedReadHalf>,
    line: &mut Vec<u8>,
) -> anyhow::Result<Option<OwnerCommand>> {
    loop {
        let Some(text) = crate::supervisor::read_frame(reader, line, MAX_COMMAND_BYTES)
            .await
            .context("controller command")?
        else {
            return Ok(None);
        };
        match serde_json::from_str::<OwnerCommand>(&text) {
            Ok(command) => return Ok(Some(command)),
            // An unknown command from a newer daemon is ignored, not fatal.
            Err(_) => continue,
        }
    }
}

async fn send<W: AsyncWrite + Unpin, T: Serialize>(
    writer: &mut W,
    value: &T,
) -> anyhow::Result<()> {
    let mut line = serde_json::to_vec(value)?;
    line.push(b'\n');
    tokio::time::timeout(WRITE_WITHIN, writer.write_all(&line))
        .await
        .context("controller stopped reading")??;
    Ok(())
}

/// Read the agent's terminal: every byte to the scrollback and whoever is
/// attached, whole lines to the log.
async fn pump_terminal(
    mut terminal: tokio::fs::File,
    log: mpsc::Sender<String>,
    shared: Arc<Shared>,
) -> anyhow::Result<()> {
    use tokio::io::AsyncReadExt;
    let mut buffer = vec![0_u8; 8192];
    let mut line = String::new();
    loop {
        let read = match terminal.read(&mut buffer).await {
            Ok(0) => break,
            Err(error) if terminal_closed(&error) => break,
            Err(error) => return Err(error).context("cannot read agent terminal output"),
            Ok(read) => read,
        };
        let chunk = &buffer[..read];
        {
            let mut output = lock(&shared.output);
            let offset = output.push(chunk);
            let _ = shared.live.send((offset, chunk.to_vec()));
        }
        line.push_str(&String::from_utf8_lossy(chunk));
        while let Some(end) = line.find('\n') {
            let complete: String = line.drain(..=end).collect();
            let complete = complete.trim_end_matches(['\n', '\r']).to_owned();
            log.send(format!("out {complete}\n"))
                .await
                .context("terminal log writer closed")?;
        }
        if line.len() > 4096 {
            let partial = std::mem::take(&mut line);
            log.send(format!("out {partial}\n"))
                .await
                .context("terminal log writer closed")?;
        }
    }
    if !line.is_empty() {
        log.send(format!("out {line}\n"))
            .await
            .context("terminal log writer closed")?;
    }
    Ok(())
}

async fn type_into_terminal(
    mut terminal: tokio::fs::File,
    mut keystrokes: mpsc::Receiver<Vec<u8>>,
) {
    while let Some(bytes) = keystrokes.recv().await {
        if terminal.write_all(&bytes).await.is_err() || terminal.flush().await.is_err() {
            return;
        }
    }
}

/// How a terminal reports that its other end is gone: `EIO` from a Unix
/// master whose slave closed, a broken pipe from a Windows console that
/// closed.
fn terminal_closed(error: &std::io::Error) -> bool {
    #[cfg(unix)]
    return error.raw_os_error() == Some(nix::libc::EIO);
    #[cfg(windows)]
    matches!(error.kind(), std::io::ErrorKind::BrokenPipe)
}

/// The child's exit: woken by `SIGCHLD` on Unix; polled on Windows, where
/// a wait on the handle could not be cancelled by the select around it.
#[cfg(unix)]
async fn wait_owned_child(child: &mut OwnedChild) -> std::io::Result<std::process::ExitStatus> {
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
#[cfg(windows)]
async fn wait_owned_child(child: &mut OwnedChild) -> std::io::Result<std::process::ExitStatus> {
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn pump<R: AsyncRead + Unpin>(
    reader: R,
    stream: &'static str,
    tx: mpsc::Sender<String>,
) -> anyhow::Result<()> {
    let mut lines = BufReader::new(reader).lines();
    while let Some(line) = lines
        .next_line()
        .await
        .context("cannot read agent pipe output")?
    {
        let stamp = Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ");
        tx.send(format!("{stamp} [{stream}] {line}\n"))
            .await
            .context("agent log writer closed")?;
    }
    Ok(())
}

async fn write_log<W: AsyncWrite + Unpin>(
    mut log: W,
    mut rx: mpsc::Receiver<String>,
) -> anyhow::Result<()> {
    while let Some(line) = rx.recv().await {
        log.write_all(line.as_bytes())
            .await
            .context("cannot write agent log")?;
    }
    log.flush().await.context("cannot flush agent log")
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::fd::OwnedFd;

    #[tokio::test]
    async fn failed_exec_waits_for_its_actual_controller_acknowledgement() {
        let dir = tempfile::tempdir().unwrap();
        let agent = AgentId::from("failed-exec");
        let home = dir.path().join("state");
        let socket = socket_path(&home, &agent);
        let launch = Launch {
            format: FORMAT,
            agent: agent.clone(),
            name: "failed-exec".into(),
            command: vec![
                dir.path()
                    .join("missing-command")
                    .to_string_lossy()
                    .into_owned(),
            ],
            env: BTreeMap::new(),
            workdir: Some(dir.path().to_path_buf()),
            tty: false,
            home: home.clone(),
            socket: dir.path().join("daemon.sock"),
            log: dir.path().join("agent.log"),
        };
        let mut owner = tokio::spawn(serve(launch));
        let stream = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(stream) = Stream::connect(&socket).await {
                    break stream;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let (reader, mut writer) = stream.into_split();
        let mut lines = BufReader::new(reader).lines();
        let hello = tokio::time::timeout(Duration::from_secs(5), lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::from_str::<OwnerHello>(&hello).unwrap().agent,
            agent
        );
        send(&mut writer, &OwnerCommand::Activate).await.unwrap();
        let report = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let line = lines
                    .next_line()
                    .await
                    .unwrap()
                    .expect("owner exited before reporting");
                if let OwnerReport::Exited { status } = serde_json::from_str(&line).unwrap() {
                    break status;
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(report.code, None);
        assert_eq!(report.signal, None);
        assert!(exit_path(&home, &agent).exists());
        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut owner)
                .await
                .is_err(),
            "failed exec must retain the real acknowledgement receiver"
        );
        send(&mut writer, &OwnerCommand::Acknowledge).await.unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), owner)
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn the_log_writer_waits_for_a_slow_sink_and_keeps_the_partial_last_line() {
        use tokio::io::AsyncReadExt;
        let (writer, mut reader) = tokio::io::duplex(8);
        let (tx, rx) = mpsc::channel(1);
        let mut tasks = tokio::task::JoinSet::new();
        tasks.spawn(write_log(writer, rx));
        tasks.spawn(pump(&b"first\nlast without newline"[..], "out", tx));
        let mut written = String::new();
        reader.read_to_string(&mut written).await.unwrap();
        while let Some(result) = tasks.join_next().await {
            result.unwrap().unwrap();
        }
        assert!(written.contains("[out] first\n"), "{written}");
        assert!(
            written.ends_with("[out] last without newline\n"),
            "{written}"
        );
    }

    #[tokio::test]
    async fn a_failed_log_sink_is_reported_and_every_task_finishes() {
        let (writer, reader) = tokio::io::duplex(8);
        drop(reader);
        let (tx, rx) = mpsc::channel(1);
        let mut tasks = tokio::task::JoinSet::new();
        tasks.spawn(write_log(writer, rx));
        tasks.spawn(pump(&b"first\nsecond\nthird\n"[..], "out", tx));
        let mut errors = Vec::new();
        while let Some(result) = tokio::time::timeout(Duration::from_secs(2), tasks.join_next())
            .await
            .unwrap()
        {
            if let Err(error) = result.unwrap() {
                errors.push(format!("{error:#}"));
            }
        }
        assert!(errors.iter().any(|e| e.contains("log")), "{errors:?}");
    }

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
                let mut info: nix::libc::siginfo_t = unsafe { std::mem::zeroed() };
                assert_eq!(
                    unsafe {
                        nix::libc::waitid(
                            nix::libc::P_PID,
                            pid,
                            &mut info,
                            nix::libc::WEXITED | nix::libc::WNOWAIT,
                        )
                    },
                    0
                );
            }
            let mut waiting = tokio::spawn(async move { wait_owned_child(&mut child).await });
            if !already_exited {
                tokio::task::yield_now().await;
                input.write_all(b"exit\n").unwrap();
            }
            let status = match tokio::time::timeout(Duration::from_secs(5), &mut waiting).await {
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

    #[test]
    fn scrollback_keeps_the_newest_bytes_and_their_offsets() {
        let mut output = Output {
            scrollback: VecDeque::new(),
            first: 0,
            next: 0,
        };
        assert_eq!(output.push(b"abc"), 0);
        assert_eq!(output.push(b"def"), 3);
        assert_eq!(output.since(0), (0, b"abcdef".to_vec()));
        assert_eq!(output.since(4), (4, b"ef".to_vec()));
        assert_eq!(output.since(6), (6, Vec::new()));
        // Fill past the retained size: the oldest bytes go, and a replay
        // from before them starts at the oldest retained byte.
        let big = vec![b'x'; SCROLLBACK];
        assert_eq!(output.push(&big), 6);
        assert_eq!(output.first, 6);
        let (start, bytes) = output.since(0);
        assert_eq!(start, 6);
        assert_eq!(bytes.len(), SCROLLBACK);
    }
}
