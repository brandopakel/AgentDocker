//! `agentdocker daemon …`: run `agentd` as a user service.
//!
//! Clients start the daemon on demand, so a service is optional; it makes
//! the daemon come back after a reboot or a crash and keeps it out of any
//! terminal's process group. launchd on macOS, systemd user units on
//! Linux, and a per-user Task Scheduler supervisor on Windows. Definition
//! generation is pure; state preparation and execution perform the mutations.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use agentdocker_core::{Request, Response, paths};
use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};

use crate::client::Client;
use crate::format;

#[cfg(any(windows, test))]
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) mod windows;

pub(crate) const LABEL: &str = "dev.agentdocker.agentd";
/// How long `daemon reload` waits for a mutation that is still executing
/// before giving the refusal to the user.
const RELOAD_WAIT: Duration = Duration::from_secs(30);
pub(crate) const UNIT: &str = "agentd.service";
// Task Scheduler and a cold Windows process start need the same allowance as
// an on-demand Windows launch. Service commands disable client autostart.
const SERVICE_READY_WAIT: Duration = Duration::from_secs(if cfg!(windows) { 10 } else { 5 });

#[derive(Args)]
pub struct DaemonArgs {
    #[command(subcommand)]
    pub command: DaemonCommand,
}

#[derive(Subcommand)]
pub enum DaemonCommand {
    /// Install agentd as a user service and start it.
    Install {
        /// Print the files and commands without touching anything.
        #[arg(long)]
        dry_run: bool,
    },
    /// Stop the service and remove its definition.
    Uninstall {
        #[arg(long)]
        dry_run: bool,
    },
    /// Start the service, or the daemon itself when no service is installed.
    Start,
    /// Stop the service if installed, else ask a running daemon to exit.
    Stop,
    /// Stop, then start.
    Restart,
    /// Request experimental live replacement; requires the daemon reload gate.
    /// Without AGENTDOCKER_EXPERIMENTAL_RELOAD=1 on the daemon, it is refused.
    Reload,
    /// Reclaim disk freed by pruning: SQLite `VACUUM` on the state database.
    /// Refused while sessions are live, since nothing is answered meanwhile.
    Vacuum {
        /// Accept pausing live sessions for the rewrite.
        #[arg(long)]
        force: bool,
    },
    /// Show whether the service is installed and the daemon answering.
    Status,
    /// Internal Windows login-task supervisor.
    #[cfg(windows)]
    #[command(hide = true)]
    Supervise {
        #[arg(long)]
        home: PathBuf,
        #[arg(long)]
        agentd: PathBuf,
        #[arg(long)]
        endpoint: PathBuf,
    },
}

/// What a subcommand would do: files to write and commands to run, in
/// order: write files, run commands, remove files, run `after_remove`.
/// `tolerated` commands may fail without aborting (unloading a
/// service that is not loaded, say).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Plan {
    pub files: Vec<(PathBuf, String)>,
    pub remove: Vec<PathBuf>,
    pub commands: Vec<Cmd>,
    pub after_remove: Vec<Cmd>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Cmd {
    pub argv: Vec<String>,
    pub tolerated: bool,
}

fn cmd(argv: &[&str]) -> Cmd {
    Cmd {
        argv: argv.iter().map(|s| (*s).to_owned()).collect(),
        tolerated: false,
    }
}

fn tolerated(argv: &[&str]) -> Cmd {
    Cmd {
        tolerated: true,
        ..cmd(argv)
    }
}

/// The socket an installed service is told to use: the one asked for, or
/// — when the home is too long for a socket name and the daemon would
/// pick a short directory by its environment — the path resolved here, so
/// the service manager's environment cannot send the daemon elsewhere
/// than the clients look.
fn service_socket(home: &Path, explicit: Option<&Path>) -> Option<PathBuf> {
    explicit.map(Path::to_path_buf).or_else(|| {
        (paths::socket_dir(home) != home).then(|| agentdocker_host::dirs::socket_path(home))
    })
}

fn validate_service_socket(socket: &Path) -> Result<()> {
    if !paths::fits_socket(socket) {
        bail!(
            "socket path {} is {} bytes; this OS allows {}",
            socket.display(),
            socket.as_os_str().len(),
            paths::SOCKET_PATH_MAX
        );
    }
    Ok(())
}

/// Everything the service definition needs to know.
#[derive(Debug, Clone)]
pub struct Layout {
    pub agentd: PathBuf,
    pub home: PathBuf,
    /// Only when the socket is not the default under `home`.
    pub socket: Option<PathBuf>,
    pub uid: u32,
    pub user_home: PathBuf,
}

impl Layout {
    fn client(&self) -> Client {
        Client::new(Some(
            self.socket
                .clone()
                .unwrap_or_else(|| agentdocker_host::dirs::socket_path(&self.home)),
        ))
    }

    fn discover(socket: Option<&Path>) -> Result<Self> {
        if let Some(socket) = socket {
            validate_service_socket(socket)?;
        }
        let agentd_binary = format!("agentd{}", std::env::consts::EXE_SUFFIX);
        let agentd = agentdocker_host::procinfo::executable_path()
            .ok()
            .and_then(|me| me.parent().map(|dir| dir.join(&agentd_binary)))
            .filter(|sibling| sibling.is_file())
            .or_else(|| which(&agentd_binary))
            .context("cannot find the agentd binary beside agentdocker or on PATH")?;
        let home = agentdocker_host::dirs::home();
        // Discovery is also used by status/dry-run; neither creates state.
        // Service ownership is the caller's identity, not a directory's owner.
        let uid = current_uid();
        let user_home = std::env::home_dir().context("no home directory")?;
        let home = home.canonicalize().unwrap_or(home);
        let socket = service_socket(&home, socket);
        if let Some(socket) = &socket {
            validate_service_socket(socket)?;
        }
        Ok(Self {
            agentd: agentd.canonicalize().unwrap_or(agentd),
            home,
            socket,
            uid,
            user_home,
        })
    }

    fn argv(&self) -> Vec<String> {
        let mut argv = vec![
            self.agentd.to_string_lossy().into_owned(),
            "--home".to_owned(),
            self.home.to_string_lossy().into_owned(),
        ];
        if let Some(socket) = &self.socket {
            argv.push("--socket".to_owned());
            argv.push(socket.to_string_lossy().into_owned());
        }
        argv
    }

    fn log(&self) -> PathBuf {
        paths::daemon_log(&self.home)
    }

    pub fn plist_path(&self) -> PathBuf {
        self.user_home
            .join("Library/LaunchAgents")
            .join(format!("{LABEL}.plist"))
    }

    pub fn unit_path(&self) -> PathBuf {
        self.user_home.join(".config/systemd/user").join(UNIT)
    }

    fn domain(&self) -> String {
        format!("gui/{}", self.uid)
    }

    fn target(&self) -> String {
        format!("gui/{}/{LABEL}", self.uid)
    }
}

fn which(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

// ----- file contents ----------------------------------------------------

/// A launchd agent: starts at login, restarts after a crash, but not
/// after a clean exit — which is what agentd does when a daemon started
/// on demand already holds the lock.
pub fn launchd_plist(layout: &Layout) -> String {
    let args: String = layout
        .argv()
        .iter()
        .map(|a| format!("        <string>{}</string>\n", xml(a)))
        .collect();
    let log = xml(&layout.log().to_string_lossy());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LABEL}</string>
    <key>ProgramArguments</key>
    <array>
{args}    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
    </dict>
    <key>ProcessType</key>
    <string>Background</string>
    <key>StandardOutPath</key>
    <string>{log}</string>
    <key>StandardErrorPath</key>
    <string>{log}</string>
    <key>EnvironmentVariables</key>
    <dict>
        <key>RUST_LOG</key>
        <string>info</string>
    </dict>
</dict>
</plist>
"#
    )
}

/// A systemd user unit with the same policy.
pub fn systemd_unit(layout: &Layout) -> String {
    let exec: Vec<String> = layout.argv().iter().map(|a| systemd_quote(a)).collect();
    format!(
        "[Unit]\n\
         Description=AgentDocker daemon\n\
         Documentation=https://github.com/brandopakel/AgentDocker\n\
         \n\
         [Service]\n\
         ExecStart={}\n\
         Restart=on-failure\n\
         RestartSec=2\n\
         Environment=RUST_LOG=info\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        exec.join(" ")
    )
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn systemd_quote(s: &str) -> String {
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || "/-._=:".contains(c))
    {
        s.to_owned()
    } else {
        format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

// ----- plans -------------------------------------------------------------

pub fn install_plan(layout: &Layout, macos: bool) -> Plan {
    if macos {
        let plist = layout.plist_path();
        Plan {
            files: vec![(plist.clone(), launchd_plist(layout))],
            remove: Vec::new(),
            commands: vec![
                tolerated(&["launchctl", "bootout", &layout.target()]),
                cmd(&[
                    "launchctl",
                    "bootstrap",
                    &layout.domain(),
                    &plist.to_string_lossy(),
                ]),
                // RunAtLoad may stay pending in an on-demand-only GUI domain.
                // An explicit install must request the same immediate start
                // as `daemon start`, rather than wait for an unstarted job.
                cmd(&["launchctl", "kickstart", &layout.target()]),
            ],
            ..Plan::default()
        }
    } else {
        Plan {
            files: vec![(layout.unit_path(), systemd_unit(layout))],
            remove: Vec::new(),
            commands: vec![
                cmd(&["systemctl", "--user", "daemon-reload"]),
                cmd(&["systemctl", "--user", "enable", "--now", UNIT]),
            ],
            ..Plan::default()
        }
    }
}

pub fn uninstall_plan(layout: &Layout, macos: bool) -> Plan {
    if macos {
        Plan {
            files: Vec::new(),
            remove: vec![layout.plist_path()],
            commands: vec![tolerated(&["launchctl", "bootout", &layout.target()])],
            ..Plan::default()
        }
    } else {
        systemd_uninstall_plan(UNIT, layout.unit_path())
    }
}

/// systemd needs the definition while stopping/disabling a service. Keep it
/// available if that operation fails, then reload only after removing it.
pub(crate) fn systemd_uninstall_plan(unit: &str, path: PathBuf) -> Plan {
    let missing = !path.exists();
    Plan {
        commands: vec![
            Cmd {
                argv: ["systemctl", "--user", "stop", unit]
                    .map(str::to_owned)
                    .into(),
                tolerated: missing,
            },
            Cmd {
                argv: ["systemctl", "--user", "disable", unit]
                    .map(str::to_owned)
                    .into(),
                tolerated: missing,
            },
        ],
        remove: vec![path],
        after_remove: vec![cmd(&["systemctl", "--user", "daemon-reload"])],
        ..Plan::default()
    }
}

pub fn start_plan(layout: &Layout, macos: bool) -> Plan {
    let commands = if macos {
        vec![
            tolerated(&[
                "launchctl",
                "bootstrap",
                &layout.domain(),
                &layout.plist_path().to_string_lossy(),
            ]),
            cmd(&["launchctl", "kickstart", &layout.target()]),
        ]
    } else {
        vec![cmd(&["systemctl", "--user", "start", UNIT])]
    };
    Plan {
        commands,
        ..Plan::default()
    }
}

pub fn stop_plan(layout: &Layout, macos: bool) -> Plan {
    let commands = if macos {
        vec![cmd(&["launchctl", "bootout", &layout.target()])]
    } else {
        vec![cmd(&["systemctl", "--user", "stop", UNIT])]
    };
    Plan {
        commands,
        ..Plan::default()
    }
}

/// Is the service definition on disk? Never on a platform without a
/// service manager.
fn installed(layout: &Layout, macos: bool) -> bool {
    if !SERVICE_MANAGER {
        return false;
    }
    if macos {
        layout.plist_path().is_file()
    } else {
        layout.unit_path().is_file()
    }
}

/// Does the service manager consider it loaded / active?
fn loaded(layout: &Layout, macos: bool) -> bool {
    if !SERVICE_MANAGER {
        return false;
    }
    let status = if macos {
        Command::new("launchctl")
            .args(["print", &layout.target()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
    } else {
        Command::new("systemctl")
            .args(["--user", "is-active", "--quiet", UNIT])
            .status()
    };
    status.is_ok_and(|s| s.success())
}

// ----- execution ---------------------------------------------------------

pub(crate) fn execute(plan: &Plan, dry_run: bool) -> Result<()> {
    for (path, contents) in &plan.files {
        if dry_run {
            println!("# would write {}\n{contents}", path.display());
            continue;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, contents)
            .with_context(|| format!("cannot write {}", path.display()))?;
        println!("wrote {}", path.display());
    }
    execute_commands(&plan.commands, dry_run)?;
    for path in &plan.remove {
        if dry_run {
            println!("# would remove {}", path.display());
            continue;
        }
        match std::fs::remove_file(path) {
            Ok(()) => println!("removed {}", path.display()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(err).with_context(|| format!("cannot remove {}", path.display()));
            }
        }
    }
    execute_commands(&plan.after_remove, dry_run)
}

fn execute_commands(commands: &[Cmd], dry_run: bool) -> Result<()> {
    execute_commands_with(
        commands,
        dry_run,
        &mut |argv| {
            Command::new(&argv[0])
                .args(&argv[1..])
                .output()
                .with_context(|| format!("cannot run {}", argv.join(" ")))
        },
        Duration::from_secs(5),
    )
}

fn execute_commands_with(
    commands: &[Cmd],
    dry_run: bool,
    run: &mut impl FnMut(&[String]) -> Result<std::process::Output>,
    unload_wait: Duration,
) -> Result<()> {
    for Cmd { argv, tolerated } in commands {
        let line = argv.join(" ");
        if dry_run {
            println!("# would run: {line}");
            continue;
        }
        let output = run(argv)?;
        if !output.status.success() && !tolerated {
            let _ = std::io::stderr().write_all(&output.stderr);
            bail!("`{line}` failed with {}", output.status);
        }
        if argv.len() == 3 && argv[0] == "launchctl" && argv[1] == "bootout" {
            wait_for_bootout(&argv[2], run, unload_wait)?;
        }
    }
    Ok(())
}

/// `bootout` returns before launchd finishes removing a running job. A
/// premature bootstrap can fail while kickstart still succeeds against the
/// retiring job. Confirm removal before starting its replacement or deleting
/// the definition. An unknown manager failure is not evidence of removal.
fn wait_for_bootout(
    target: &str,
    run: &mut impl FnMut(&[String]) -> Result<std::process::Output>,
    timeout: Duration,
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    let query = ["launchctl", "print", target].map(str::to_owned);
    loop {
        let output = run(&query)?;
        // launchctl uses 113 for a service absent from the selected domain.
        if output.status.code() == Some(113) {
            return Ok(());
        }
        if !output.status.success() {
            bail!(
                "cannot confirm launchd service {target} was removed: {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        if Instant::now() >= deadline {
            bail!(
                "launchd service {target} was not removed within {} s",
                timeout.as_secs()
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Ask a daemon on this socket to exit, then wait until it is gone. Used
/// before handing the socket to the service, so the service's daemon does
/// not find the lock taken.
async fn retire(client: &Client) -> Result<()> {
    if client.call(&Request::Ping).await.is_err() {
        return Ok(());
    }
    client.call(&Request::Shutdown).await?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while client.call(&Request::Ping).await.is_ok() {
        if Instant::now() > deadline {
            bail!("the running agentd did not exit");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    println!("stopped the running agentd");
    Ok(())
}

async fn wait_for_daemon(client: &Client) -> Result<()> {
    let deadline = Instant::now() + SERVICE_READY_WAIT;
    loop {
        if let Ok(Response::Pong {
            version,
            uptime_secs,
            ..
        }) = client.call(&Request::Ping).await
        {
            println!("agentd {version} up {}", format::span_secs(uptime_secs));
            return Ok(());
        }
        if Instant::now() > deadline {
            bail!(
                "agentd did not answer within {} s; see the log",
                SERVICE_READY_WAIT.as_secs()
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Unix manager plans live here; Windows lifecycle commands are handled by
/// the Task Scheduler backend before the shared daemon-only operations.
const SERVICE_MANAGER: bool = cfg!(any(target_os = "macos", target_os = "linux"));

pub async fn run(socket: Option<PathBuf>, args: DaemonArgs) -> Result<()> {
    #[cfg(windows)]
    if let DaemonCommand::Supervise {
        home,
        agentd,
        endpoint,
    } = &args.command
    {
        return windows::supervise(home, agentd, endpoint);
    }
    let macos = cfg!(target_os = "macos");
    if !SERVICE_MANAGER
        && !cfg!(windows)
        && matches!(
            &args.command,
            DaemonCommand::Install { .. } | DaemonCommand::Uninstall { .. }
        )
    {
        bail!(
            "the daemon service is not available on this platform: run `agentd` yourself, or `agentdocker daemon start` starts one for this home and `daemon stop` asks it to exit"
        );
    }
    // Service commands always use the canonical layout socket.
    let layout = Layout::discover(socket.as_deref())?;
    let client = layout.client().with_start_timeout(None);
    if matches!(
        &args.command,
        DaemonCommand::Install { dry_run: false } | DaemonCommand::Start | DaemonCommand::Restart
    ) {
        agentdocker_host::dirs::secure_state_dir(&layout.home)?;
        // launchd opens its log before executing agentd. Precreate/protect it
        // before service installation or restart can give it any output.
        agentdocker_host::dirs::private_file(&layout.log(), true, true)?;
    }
    #[cfg(windows)]
    if windows::handle(&layout, &client, &args.command).await? {
        return Ok(());
    }
    match args.command {
        #[cfg(windows)]
        DaemonCommand::Supervise { .. } => {
            unreachable!("supervisor handled before layout discovery")
        }
        DaemonCommand::Install { dry_run } => {
            if !dry_run {
                retire(&client).await?;
            }
            execute(&install_plan(&layout, macos), dry_run)?;
            if !dry_run {
                wait_for_daemon(&client).await?;
            }
        }
        DaemonCommand::Uninstall { dry_run } => {
            execute(&uninstall_plan(&layout, macos), dry_run)?;
        }
        DaemonCommand::Start => {
            if installed(&layout, macos) {
                execute(&start_plan(&layout, macos), false)?;
            } else {
                // No service: a client with autostart starts one on demand.
                layout
                    .client()
                    .with_start_timeout(Some(Duration::from_secs(5)))
                    .call(&Request::Ping)
                    .await?;
            }
            wait_for_daemon(&client).await?;
        }
        DaemonCommand::Stop => {
            if installed(&layout, macos) && loaded(&layout, macos) {
                execute(&stop_plan(&layout, macos), false)?;
            }
            retire(&client).await?;
        }
        DaemonCommand::Restart => {
            if installed(&layout, macos) && loaded(&layout, macos) {
                execute(&stop_plan(&layout, macos), false)?;
            }
            retire(&client).await?;
            if installed(&layout, macos) {
                execute(&start_plan(&layout, macos), false)?;
            } else {
                layout
                    .client()
                    .with_start_timeout(Some(Duration::from_secs(5)))
                    .call(&Request::Ping)
                    .await?;
            }
            wait_for_daemon(&client).await?;
        }
        DaemonCommand::Vacuum { force } => {
            let response = client
                .with_start_timeout(None)
                .call(&Request::Vacuum { force })
                .await
                .context("vacuum failed")?;
            if let Response::Vacuumed {
                before_bytes,
                after_bytes,
            } = response
            {
                println!(
                    "vacuumed: {before_bytes} -> {after_bytes} bytes ({} reclaimed)",
                    before_bytes.saturating_sub(after_bytes)
                );
            }
        }
        DaemonCommand::Reload => {
            // A refused reload does not start a daemon or imply a completed
            // upgrade. `backpressure` means a mutation admitted before the
            // offer is still executing; the daemon offers again once it is
            // done, so wait for it rather than hand the user a retry.
            // Each attempt runs for as long as the daemon takes: a handover
            // in progress is not abandoned from this side, and the daemon
            // bounds it itself. What is bounded is how long this keeps
            // asking again after `backpressure`.
            let quiet = client.with_start_timeout(None);
            let deadline = Instant::now() + RELOAD_WAIT;
            let mut told = false;
            let response = loop {
                let response = quiet.call_raw(&Request::Reload).await?;
                let busy = matches!(
                    &response,
                    Response::Error {
                        code: agentdocker_core::ErrorCode::Backpressure,
                        ..
                    }
                );
                let remaining = deadline.saturating_duration_since(Instant::now());
                if !busy || remaining.is_zero() {
                    break response;
                }
                if !told {
                    eprintln!("waiting for a request still executing before the handover");
                    told = true;
                }
                tokio::time::sleep(Duration::from_millis(500).min(remaining)).await;
            };
            // A refusal keeps its class: unavailable or busy ends as such,
            // with the daemon's details, not as something unexpected.
            crate::client::into_result(response).context("reload failed")?;
            println!("reloaded: a new agentd is serving; agents kept running");
        }
        DaemonCommand::Status => {
            let definition = if macos {
                layout.plist_path()
            } else {
                layout.unit_path()
            };
            let manager = if macos { "launchd" } else { "systemd" };
            if !SERVICE_MANAGER {
                #[cfg(not(windows))]
                println!(
                    "service   none on Windows yet (run `agentd` yourself, or `agentdocker daemon start` starts one)"
                );
            } else if installed(&layout, macos) {
                let state = if loaded(&layout, macos) {
                    "loaded"
                } else {
                    "not loaded"
                };
                println!("service   {manager}, {state} ({})", definition.display());
            } else {
                println!(
                    "service   not installed (`agentdocker daemon install` adds a {manager} user service)"
                );
            }
            let socket =
                socket.unwrap_or_else(|| agentdocker_host::dirs::socket_path(&layout.home));
            match client.call(&Request::Ping).await {
                Ok(Response::Pong {
                    version,
                    uptime_secs,
                    restricted,
                    pid,
                    executable,
                }) => {
                    println!(
                        "daemon    agentd {version} up {} at {}{}",
                        format::span_secs(uptime_secs),
                        socket.display(),
                        pid.map(|pid| format!(" (pid {pid})")).unwrap_or_default()
                    );
                    if let Some(executable) = executable {
                        println!("serving   {}", executable.display());
                    }
                    match restricted {
                        Some(path) => println!("container {}", path.display()),
                        None => println!(
                            "container endpoint off (see the daemon log); grants are refused"
                        ),
                    }
                }
                _ => {
                    println!(
                        "daemon    not running (clients start it on demand at {})",
                        socket.display()
                    );
                    println!(
                        "container {}",
                        paths::container_socket(&layout.home).display()
                    );
                }
            }
            println!("log       {}", layout.log().display());
        }
    }
    Ok(())
}

/// The effective user id a launchd or systemd unit is filed under. Windows
/// services use the caller's SID instead of a numeric Unix uid.
pub(crate) fn current_uid_for_service() -> u32 {
    current_uid()
}

fn current_uid() -> u32 {
    #[cfg(unix)]
    {
        // SAFETY: geteuid has no preconditions.
        unsafe { libc::geteuid() }
    }
    #[cfg(windows)]
    {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn manager_output(code: i32) -> std::process::Output {
        use std::os::unix::process::ExitStatusExt;
        std::process::Output {
            status: std::process::ExitStatus::from_raw(code << 8),
            stdout: Vec::new(),
            stderr: Vec::new(),
        }
    }

    #[test]
    #[cfg(unix)]
    fn launchd_replacement_waits_until_the_old_job_is_absent() {
        let plan = install_plan(&layout(), true);
        let mut trace = Vec::new();
        let mut queries = 0;
        execute_commands_with(
            &plan.commands,
            false,
            &mut |argv| {
                trace.push(argv[1].clone());
                if argv[1] == "print" {
                    queries += 1;
                    return Ok(manager_output(if queries == 1 { 0 } else { 113 }));
                }
                if argv[1] == "bootstrap" {
                    assert_eq!(queries, 2, "replacement must not meet the retiring job");
                }
                Ok(manager_output(0))
            },
            Duration::from_secs(1),
        )
        .unwrap();
        assert_eq!(
            trace,
            ["bootout", "print", "print", "bootstrap", "kickstart"]
        );
    }

    #[test]
    #[cfg(unix)]
    fn launchd_unknown_removal_errors_and_timeouts_stop_the_plan() {
        for query_code in [0, 5] {
            let plan = install_plan(&layout(), true);
            let mut trace = Vec::new();
            let error = execute_commands_with(
                &plan.commands,
                false,
                &mut |argv| {
                    trace.push(argv[1].clone());
                    Ok(manager_output(if argv[1] == "print" {
                        query_code
                    } else {
                        0
                    }))
                },
                Duration::ZERO,
            )
            .unwrap_err();
            assert_eq!(trace, ["bootout", "print"]);
            assert!(
                error.to_string().contains(if query_code == 0 {
                    "was not removed"
                } else {
                    "cannot confirm"
                }),
                "{error}"
            );
        }
    }

    #[test]
    #[cfg(unix)]
    fn launchd_absent_uninstall_is_idempotent_but_still_verifies_absence() {
        let plan = uninstall_plan(&layout(), true);
        let mut trace = Vec::new();
        execute_commands_with(
            &plan.commands,
            false,
            &mut |argv| {
                trace.push(argv[1].clone());
                Ok(manager_output(if argv[1] == "print" { 113 } else { 3 }))
            },
            Duration::ZERO,
        )
        .unwrap();
        assert_eq!(trace, ["bootout", "print"]);
        execute_commands_with(
            &plan.commands,
            true,
            &mut |_| panic!("dry run must not query or mutate launchd"),
            Duration::ZERO,
        )
        .unwrap();
    }

    #[test]
    fn overlong_service_socket_is_rejected_before_layout_side_effects() {
        let long = PathBuf::from(format!("/tmp/{}", "x".repeat(paths::SOCKET_PATH_MAX)));
        let error = Layout::discover(Some(&long)).unwrap_err().to_string();
        assert!(
            error.contains("bytes") && error.contains("allows"),
            "{error}"
        );
        validate_service_socket(Path::new("/tmp/agentd.sock")).unwrap();
    }

    #[tokio::test]
    async fn symlinked_long_home_clients_use_the_service_socket() {
        let tmp = tempfile::tempdir().unwrap();
        let actual = tmp.path().join("a".repeat(100));
        std::fs::create_dir(&actual).unwrap();
        let alias = tmp.path().join("b".repeat(100));
        std::os::unix::fs::symlink(&actual, &alias).unwrap();
        let home = alias.canonicalize().unwrap();
        let mut layout = layout();
        layout.home = home.clone();
        layout.socket = service_socket(&home, None);
        let socket = layout.socket.clone().unwrap();
        assert_ne!(socket, paths::socket_path(&alias));
        let parent = socket.parent().unwrap();
        agentdocker_host::dirs::ensure_private_dir(parent).unwrap();
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(async move {
            use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
            let (peer, _) = listener.accept().await.unwrap();
            let mut peer = BufReader::new(peer);
            let mut request = String::new();
            peer.read_line(&mut request).await.unwrap();
            peer.get_mut()
                .write_all(b"{\"type\":\"ok\"}\n")
                .await
                .unwrap();
        });
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            layout
                .client()
                .with_start_timeout(None)
                .call(&Request::Ping),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(matches!(result, Response::Ok));
        server.await.unwrap();
        std::fs::remove_file(&socket).unwrap();
        std::fs::remove_dir(parent).unwrap();
    }

    fn layout() -> Layout {
        Layout {
            agentd: PathBuf::from("/opt/agentdocker/bin/agentd"),
            home: PathBuf::from("/Users/me/.agentdocker"),
            socket: None,
            uid: 501,
            user_home: PathBuf::from("/Users/me"),
        }
    }

    #[test]
    fn a_long_home_pins_the_resolved_socket_into_the_service() {
        let short = PathBuf::from("/Users/me/.agentdocker");
        assert_eq!(service_socket(&short, None), None);
        assert_eq!(
            service_socket(&short, Some(Path::new("/tmp/x.sock"))),
            Some(PathBuf::from("/tmp/x.sock"))
        );
        let long = PathBuf::from(format!("/Users/me/{}", "d".repeat(paths::SOCKET_PATH_MAX)));
        let pinned = service_socket(&long, None).expect("resolved for the service");
        assert!(paths::fits_socket(&pinned));
        assert!(pinned.ends_with("agentd.sock"));
        let mut layout = layout();
        layout.home = long;
        layout.socket = Some(pinned.clone());
        assert!(layout.argv().contains(&"--socket".to_owned()));
        assert!(launchd_plist(&layout).contains(&pinned.to_string_lossy().into_owned()));
    }

    #[test]
    fn plist_runs_agentd_with_the_home_and_restarts_only_on_failure() {
        let text = launchd_plist(&layout());
        assert!(text.contains("<string>dev.agentdocker.agentd</string>"));
        assert!(text.contains("<string>/opt/agentdocker/bin/agentd</string>\n        <string>--home</string>\n        <string>/Users/me/.agentdocker</string>"));
        assert!(text.contains("<key>SuccessfulExit</key>\n        <false/>"));
        assert!(text.contains("<string>/Users/me/.agentdocker/agentd.log</string>"));
        assert!(!text.contains("--socket"));

        let mut with_socket = layout();
        with_socket.socket = Some(PathBuf::from("/tmp/a&b.sock"));
        let text = launchd_plist(&with_socket);
        assert!(
            text.contains("<string>--socket</string>\n        <string>/tmp/a&amp;b.sock</string>")
        );
    }

    #[test]
    fn unit_quotes_only_what_needs_it() {
        let text = systemd_unit(&layout());
        assert!(
            text.contains("ExecStart=/opt/agentdocker/bin/agentd --home /Users/me/.agentdocker\n")
        );
        assert!(text.contains("Restart=on-failure"));
        assert!(text.contains("WantedBy=default.target"));
        let mut odd = layout();
        odd.home = PathBuf::from("/home/me/my agents");
        assert!(systemd_unit(&odd).contains("--home \"/home/me/my agents\""));
    }

    #[test]
    fn plans_target_the_user_domain() {
        let plan = install_plan(&layout(), true);
        assert_eq!(
            plan.files[0].0,
            PathBuf::from("/Users/me/Library/LaunchAgents/dev.agentdocker.agentd.plist")
        );
        assert_eq!(
            plan.commands[0],
            tolerated(&["launchctl", "bootout", "gui/501/dev.agentdocker.agentd"])
        );
        assert_eq!(
            plan.commands[1].argv,
            [
                "launchctl",
                "bootstrap",
                "gui/501",
                "/Users/me/Library/LaunchAgents/dev.agentdocker.agentd.plist"
            ]
        );
        assert!(!plan.commands[1].tolerated);
        assert_eq!(
            plan.commands[2],
            cmd(&["launchctl", "kickstart", "gui/501/dev.agentdocker.agentd"])
        );
        assert_eq!(plan.commands.len(), 3);

        let plan = install_plan(&layout(), false);
        assert_eq!(
            plan.files[0].0,
            PathBuf::from("/Users/me/.config/systemd/user/agentd.service")
        );
        assert_eq!(
            plan.commands[1].argv,
            ["systemctl", "--user", "enable", "--now", "agentd.service"]
        );

        let plan = uninstall_plan(&layout(), true);
        assert_eq!(plan.remove, vec![layout().plist_path()]);
        assert!(plan.commands.iter().all(|c| c.tolerated));
        assert_eq!(
            stop_plan(&layout(), false).commands[0].argv,
            ["systemctl", "--user", "stop", "agentd.service"]
        );
    }

    #[test]
    fn dry_run_touches_nothing() {
        let dir = tempfile::TempDir::new().unwrap();
        let mut lay = layout();
        lay.user_home = dir.path().to_path_buf();
        execute(&install_plan(&lay, true), true).unwrap();
        assert!(!lay.plist_path().exists());
    }

    #[cfg(unix)]
    fn fixture_command(script: &str, paths: &[&Path]) -> Cmd {
        let mut argv = vec![
            "/bin/sh".into(),
            "-c".into(),
            script.into(),
            "fixture".into(),
        ];
        argv.extend(paths.iter().map(|p| p.to_string_lossy().into_owned()));
        Cmd {
            argv,
            tolerated: false,
        }
    }

    #[cfg(unix)]
    #[test]
    fn uninstall_stops_before_removing_the_definition_and_reloads_afterwards() {
        for unit in [UNIT, crate::connector::service::UNIT] {
            let dir = tempfile::tempdir().unwrap();
            let definition = dir.path().join(unit);
            let running = dir.path().join("running");
            let reload = dir.path().join("reloaded");
            std::fs::write(&definition, "owned definition").unwrap();
            std::fs::write(&running, "running").unwrap();
            let mut plan = systemd_uninstall_plan(unit, definition.clone());
            assert!(plan.commands.iter().all(|c| !c.tolerated));
            assert_eq!(plan.commands[0].argv, ["systemctl", "--user", "stop", unit]);
            assert_eq!(
                plan.commands[1].argv,
                ["systemctl", "--user", "disable", unit]
            );
            plan.commands = vec![fixture_command(
                "test -f \"$1\" && rm \"$2\"",
                &[&definition, &running],
            )];
            plan.after_remove = vec![fixture_command(
                "test ! -e \"$1\" && test ! -e \"$2\" && touch \"$3\"",
                &[&definition, &running, &reload],
            )];
            execute(&plan, true).unwrap();
            assert!(definition.exists() && running.exists() && !reload.exists());
            execute(&plan, false).unwrap();
            assert!(!definition.exists() && !running.exists() && reload.exists());
        }
    }

    #[cfg(unix)]
    #[test]
    fn failed_service_stop_preserves_definition_and_skips_reload() {
        let dir = tempfile::tempdir().unwrap();
        let definition = dir.path().join(UNIT);
        let reload = dir.path().join("reloaded");
        std::fs::write(&definition, "owned definition").unwrap();
        let mut plan = systemd_uninstall_plan(UNIT, definition.clone());
        plan.commands[0].argv = fixture_command("exit 1", &[]).argv;
        plan.after_remove = vec![fixture_command("touch \"$1\"", &[&reload])];
        assert!(execute(&plan, false).is_err());
        assert_eq!(
            std::fs::read_to_string(definition).unwrap(),
            "owned definition"
        );
        assert!(!reload.exists());
    }
}
