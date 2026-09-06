//! `agentdocker daemon …`: run `agentd` as a user service.
//!
//! Clients start the daemon on demand, so a service is optional; it makes
//! the daemon come back after a reboot or a crash and keeps it out of any
//! terminal's process group. launchd on macOS, systemd user units on
//! Linux. The file contents and command sequences are pure so they are
//! tested; only [`execute`] touches the system.

use std::io::Write;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use agentdocker_core::{Request, Response, paths};
use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};

use crate::client::Client;
use crate::format;

const LABEL: &str = "dev.agentdocker.agentd";
const UNIT: &str = "agentd.service";

#[derive(Args)]
pub struct DaemonArgs {
    #[command(subcommand)]
    pub command: DaemonCommand,
}

#[derive(Subcommand)]
pub enum DaemonCommand {
    /// Install agentd as a user service (launchd or systemd) and start it.
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
    /// Show whether the service is installed and the daemon answering.
    Status,
}

/// What a subcommand would do: files to write and commands to run, in
/// order. `tolerated` commands may fail without aborting (unloading a
/// service that is not loaded, say).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Plan {
    pub files: Vec<(PathBuf, String)>,
    pub remove: Vec<PathBuf>,
    pub commands: Vec<Cmd>,
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
    explicit
        .map(Path::to_path_buf)
        .or_else(|| (paths::socket_dir(home) != home).then(|| paths::socket_path(home)))
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
                .unwrap_or_else(|| paths::socket_path(&self.home)),
        ))
    }

    fn discover(socket: Option<&Path>) -> Result<Self> {
        let agentd = std::env::current_exe()
            .ok()
            .and_then(|me| me.parent().map(|dir| dir.join("agentd")))
            .filter(|sibling| sibling.is_file())
            .or_else(|| which("agentd"))
            .context("cannot find the agentd binary beside agentdocker or on PATH")?;
        let home = paths::default_home();
        std::fs::create_dir_all(&home)?;
        let uid = std::fs::metadata(&home)?.uid();
        let user_home = std::env::home_dir().context("no home directory")?;
        let home = home.canonicalize().unwrap_or(home);
        let socket = service_socket(&home, socket);
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
    std::env::var_os("PATH")?
        .to_str()?
        .split(':')
        .map(|dir| Path::new(dir).join(name))
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
            ],
        }
    } else {
        Plan {
            files: vec![(layout.unit_path(), systemd_unit(layout))],
            remove: Vec::new(),
            commands: vec![
                cmd(&["systemctl", "--user", "daemon-reload"]),
                cmd(&["systemctl", "--user", "enable", "--now", UNIT]),
            ],
        }
    }
}

pub fn uninstall_plan(layout: &Layout, macos: bool) -> Plan {
    if macos {
        Plan {
            files: Vec::new(),
            remove: vec![layout.plist_path()],
            commands: vec![tolerated(&["launchctl", "bootout", &layout.target()])],
        }
    } else {
        Plan {
            files: Vec::new(),
            remove: vec![layout.unit_path()],
            commands: vec![
                tolerated(&["systemctl", "--user", "disable", "--now", UNIT]),
                tolerated(&["systemctl", "--user", "daemon-reload"]),
            ],
        }
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

/// Is the service definition on disk?
fn installed(layout: &Layout, macos: bool) -> bool {
    if macos {
        layout.plist_path().is_file()
    } else {
        layout.unit_path().is_file()
    }
}

/// Does the service manager consider it loaded / active?
fn loaded(layout: &Layout, macos: bool) -> bool {
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

fn execute(plan: &Plan, dry_run: bool) -> Result<()> {
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
    for Cmd { argv, tolerated } in &plan.commands {
        let line = argv.join(" ");
        if dry_run {
            println!("# would run: {line}");
            continue;
        }
        let output = Command::new(&argv[0])
            .args(&argv[1..])
            .output()
            .with_context(|| format!("cannot run {line}"))?;
        if !output.status.success() && !tolerated {
            let _ = std::io::stderr().write_all(&output.stderr);
            bail!("`{line}` failed with {}", output.status);
        }
    }
    Ok(())
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
    let deadline = Instant::now() + Duration::from_secs(5);
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
            bail!("agentd did not answer within 5 s; see the log");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

pub async fn run(socket: Option<PathBuf>, args: DaemonArgs) -> Result<()> {
    let macos = cfg!(target_os = "macos");
    if !macos && !cfg!(target_os = "linux") {
        bail!("service management is supported on macOS (launchd) and Linux (systemd) only");
    }
    // Nothing here may start a daemon by accident.
    let layout = Layout::discover(socket.as_deref())?;
    let client = layout.client().with_start_timeout(None);
    match args.command {
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
                Client::new(socket.clone())
                    .with_start_timeout(Some(Duration::from_secs(5)))
                    .call(&Request::Ping)
                    .await?;
            }
            wait_for_daemon(&client).await?;
        }
        DaemonCommand::Status => {
            let definition = if macos {
                layout.plist_path()
            } else {
                layout.unit_path()
            };
            let manager = if macos { "launchd" } else { "systemd" };
            if installed(&layout, macos) {
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
            let socket = socket.unwrap_or_else(|| paths::socket_path(&layout.home));
            match client.call(&Request::Ping).await {
                Ok(Response::Pong {
                    version,
                    uptime_secs,
                    restricted,
                }) => {
                    println!(
                        "daemon    agentd {version} up {} at {}",
                        format::span_secs(uptime_secs),
                        socket.display()
                    );
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
