//! The connector as a login service, so a browser agent's way in outlives
//! the terminal that first opened it: a launchd agent on macOS, a systemd
//! user unit on Linux, running `agentdocker connector serve` with the
//! arguments given at install time. The daemon's own service does the
//! same for agentd; this borrows its shapes and commands.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use super::ServeArgs;
use crate::service::{Cmd, Plan, execute};

pub const LABEL: &str = "dev.agentdocker.connector";
pub const UNIT: &str = "agentdocker-connector.service";

/// What a serving connector writes about itself, for `connector status`
/// and for the person who started it as a service and needs the pairing
/// code without a terminal to read it from.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Serving {
    pub pid: u32,
    pub public_url: String,
    pub bind: String,
    pub project: PathBuf,
    pub pairing_code: String,
    pub started_at: chrono::DateTime<chrono::Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tunnel: Option<TunnelStatus>,
    #[serde(default)]
    pub allowlist_prefixes: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TunnelStatus {
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Where the serving connector describes itself.
pub fn status_path(home: &Path) -> PathBuf {
    home.join("connector").join("serve.json")
}

pub fn write_status(home: &Path, serving: &Serving) -> Result<()> {
    let path = status_path(home);
    let parent = path.parent().context("status path has no parent")?;
    std::fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".serve.{}.tmp", serving.pid));
    std::fs::write(&temporary, serde_json::to_vec_pretty(serving)?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&temporary, &path)?;
    Ok(())
}

pub fn clear_status(home: &Path, pid: u32) {
    let path = status_path(home);
    // Only the process that wrote it takes it away.
    if read_status(home).is_some_and(|s| s.pid == pid) {
        let _ = std::fs::remove_file(path);
    }
}

pub fn read_status(home: &Path) -> Option<Serving> {
    let text = std::fs::read_to_string(status_path(home)).ok()?;
    serde_json::from_str(&text).ok()
}

/// The service's files and commands, from the executable, the state home
/// and the arguments `serve` was given.
pub struct Layout {
    pub agentdocker: PathBuf,
    pub home: PathBuf,
    pub user_home: PathBuf,
    pub uid: u32,
    pub serve_args: Vec<String>,
    /// Directories the service's PATH must have: where cloudflared is.
    pub path_dirs: Vec<PathBuf>,
}

impl Layout {
    pub fn argv(&self) -> Vec<String> {
        let mut argv = vec![
            self.agentdocker.to_string_lossy().into_owned(),
            "connector".to_owned(),
            "serve".to_owned(),
        ];
        argv.extend(self.serve_args.iter().cloned());
        argv
    }

    pub fn log(&self) -> PathBuf {
        self.home.join("connector").join("serve.log")
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

    fn path_value(&self) -> String {
        let mut dirs: Vec<String> = self
            .path_dirs
            .iter()
            .map(|d| d.to_string_lossy().into_owned())
            .collect();
        for usual in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"] {
            if !dirs.iter().any(|d| d == usual) {
                dirs.push(usual.to_owned());
            }
        }
        dirs.join(":")
    }
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn systemd_quote(s: &str) -> String {
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || "/-._=:@".contains(c))
    {
        s.to_owned()
    } else {
        format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

/// A launchd agent: starts at login, restarts after a crash, not after a
/// clean exit; its output is the serve log, which holds the banner and
/// so the pairing code.
pub fn launchd_plist(layout: &Layout) -> String {
    let args: String = layout
        .argv()
        .iter()
        .map(|a| format!("        <string>{}</string>\n", xml(a)))
        .collect();
    let log = xml(&layout.log().to_string_lossy());
    let home = xml(&layout.home.to_string_lossy());
    let path = xml(&layout.path_value());
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
        <key>AGENTDOCKER_HOME</key>
        <string>{home}</string>
        <key>PATH</key>
        <string>{path}</string>
    </dict>
</dict>
</plist>
"#
    )
}

pub fn systemd_unit(layout: &Layout) -> String {
    let exec: Vec<String> = layout.argv().iter().map(|a| systemd_quote(a)).collect();
    format!(
        "[Unit]\n\
         Description=AgentDocker remote connector\n\
         Documentation=https://github.com/brandopakel/AgentDocker\n\
         \n\
         [Service]\n\
         ExecStart={}\n\
         Restart=on-failure\n\
         RestartSec=2\n\
         Environment=AGENTDOCKER_HOME={}\n\
         Environment=PATH={}\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        exec.join(" "),
        systemd_quote(&layout.home.to_string_lossy()),
        systemd_quote(&layout.path_value()),
    )
}

pub fn install_plan(layout: &Layout, macos: bool) -> Plan {
    if macos {
        let plist = layout.plist_path();
        Plan {
            files: vec![(plist.clone(), launchd_plist(layout))],
            remove: vec![],
            commands: vec![
                Cmd {
                    argv: vec!["launchctl".into(), "bootout".into(), layout.target()],
                    tolerated: true,
                },
                Cmd {
                    argv: vec![
                        "launchctl".into(),
                        "bootstrap".into(),
                        layout.domain(),
                        plist.to_string_lossy().into_owned(),
                    ],
                    tolerated: false,
                },
            ],
        }
    } else {
        Plan {
            files: vec![(layout.unit_path(), systemd_unit(layout))],
            remove: vec![],
            commands: vec![
                Cmd {
                    argv: vec!["systemctl".into(), "--user".into(), "daemon-reload".into()],
                    tolerated: false,
                },
                Cmd {
                    argv: vec![
                        "systemctl".into(),
                        "--user".into(),
                        "enable".into(),
                        "--now".into(),
                        UNIT.into(),
                    ],
                    tolerated: false,
                },
            ],
        }
    }
}

pub fn uninstall_plan(layout: &Layout, macos: bool) -> Plan {
    if macos {
        Plan {
            files: vec![],
            remove: vec![layout.plist_path()],
            commands: vec![Cmd {
                argv: vec!["launchctl".into(), "bootout".into(), layout.target()],
                tolerated: true,
            }],
        }
    } else {
        Plan {
            files: vec![],
            remove: vec![layout.unit_path()],
            commands: vec![
                Cmd {
                    argv: vec![
                        "systemctl".into(),
                        "--user".into(),
                        "disable".into(),
                        "--now".into(),
                        UNIT.into(),
                    ],
                    tolerated: true,
                },
                Cmd {
                    argv: vec!["systemctl".into(), "--user".into(), "daemon-reload".into()],
                    tolerated: true,
                },
            ],
        }
    }
}

fn layout(args: &ServeArgs) -> Result<Layout> {
    let agentdocker = std::env::current_exe().context("cannot locate this executable")?;
    let home = agentdocker_host::dirs::home();
    let user_home = std::env::home_dir().context("no home directory")?;
    let mut serve_args = args.to_argv();
    let mut path_dirs = Vec::new();
    // Resolve the tunnel's binary now: launchd's PATH will not, and a
    // moved binary is a new install.
    match args.tunnel.as_deref() {
        Some("cloudflared") => {
            let binary = super::tunnel::find_cloudflared(args.cloudflared.as_deref())?;
            if args.cloudflared.is_none() {
                serve_args.push("--cloudflared".into());
                serve_args.push(binary.to_string_lossy().into_owned());
            }
            if let Some(dir) = binary.parent() {
                path_dirs.push(dir.to_owned());
            }
        }
        Some("tailscale") => {
            let binary = super::tunnel::find_tailscale(args.tailscale.as_deref())?;
            if args.tailscale.is_none() {
                serve_args.push("--tailscale".into());
                serve_args.push(binary.to_string_lossy().into_owned());
            }
            if let Some(dir) = binary.parent() {
                path_dirs.push(dir.to_owned());
            }
        }
        _ => {}
    }
    let project = match &args.project {
        Some(path) => path.clone(),
        None => std::env::current_dir()?,
    };
    if args.project.is_none() {
        // The service has no working directory of the person's; name it.
        serve_args.push("--project".into());
        serve_args.push(project.to_string_lossy().into_owned());
    }
    Ok(Layout {
        agentdocker,
        home,
        user_home,
        uid: unsafe { libc::getuid() },
        serve_args,
        path_dirs,
    })
}

pub fn install(args: &ServeArgs, dry_run: bool) -> Result<()> {
    let macos = cfg!(target_os = "macos");
    if !macos && !cfg!(target_os = "linux") {
        bail!("the connector service is supported on macOS (launchd) and Linux (systemd) only");
    }
    if args.tunnel.is_none() && args.public_url.is_none() {
        bail!(
            "a service needs a public address: --tunnel cloudflared (a quick tunnel, new hostname at every start) or --public-url with your own tunnel in front"
        );
    }
    if args.tunnel.as_deref() == Some("cloudflared") && args.tunnel_name.is_none() {
        eprintln!(
            "note: a quick tunnel gets a new hostname every time the service starts, and the connector saved in Claude or ChatGPT must then be added again; for a hostname that stays, use --tunnel tailscale (Funnel on this machine's own name) or a named cloudflared tunnel with --tunnel-name and --public-url"
        );
    }
    let layout = layout(args)?;
    let plan = install_plan(&layout, macos);
    execute(&plan, dry_run)?;
    if !dry_run {
        eprintln!(
            "connector service installed; `agentdocker connector status` shows its address and pairing code once it is up, and {} is its log",
            layout.log().display()
        );
    }
    Ok(())
}

pub fn uninstall(dry_run: bool) -> Result<()> {
    let macos = cfg!(target_os = "macos");
    if !macos && !cfg!(target_os = "linux") {
        bail!("the connector service is supported on macOS (launchd) and Linux (systemd) only");
    }
    let layout = layout(&ServeArgs::default())?;
    execute(&uninstall_plan(&layout, macos), dry_run)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> Layout {
        Layout {
            agentdocker: "/opt/agentdocker".into(),
            home: "/Users/p/.agentdocker".into(),
            user_home: "/Users/p".into(),
            uid: 501,
            serve_args: vec![
                "--tunnel".into(),
                "cloudflared".into(),
                "--project".into(),
                "/Users/p/keel".into(),
                "--allow-from".into(),
                "anthropic".into(),
            ],
            path_dirs: vec!["/opt/homebrew/bin".into()],
        }
    }

    #[test]
    fn the_service_definitions_carry_the_serve_arguments_home_and_path() {
        let layout = layout();
        let plist = launchd_plist(&layout);
        assert!(plist.contains("<string>dev.agentdocker.connector</string>"));
        assert!(plist.contains("<string>connector</string>\n        <string>serve</string>\n        <string>--tunnel</string>"));
        assert!(plist.contains(
            "<key>AGENTDOCKER_HOME</key>\n        <string>/Users/p/.agentdocker</string>"
        ));
        assert!(plist.contains("<string>/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin</string>"));
        assert!(plist.contains("<string>/Users/p/.agentdocker/connector/serve.log</string>"));
        let unit = systemd_unit(&layout);
        assert!(unit.contains("ExecStart=/opt/agentdocker connector serve --tunnel cloudflared --project /Users/p/keel --allow-from anthropic\n"));
        assert!(unit.contains("Environment=AGENTDOCKER_HOME=/Users/p/.agentdocker\n"));
        assert!(unit.contains("Restart=on-failure"));
        let install = install_plan(&layout, true);
        assert_eq!(install.files[0].0, layout.plist_path());
        assert!(install.commands.iter().any(|c| c.argv[1] == "bootstrap"));
        let uninstall = uninstall_plan(&layout, false);
        assert_eq!(uninstall.remove, vec![layout.unit_path()]);
        assert!(uninstall.commands[0].argv.contains(&"disable".to_owned()));
    }

    #[test]
    fn the_status_file_is_private_and_only_its_writer_clears_it() {
        let dir = tempfile::tempdir().unwrap();
        let serving = Serving {
            pid: 4242,
            public_url: "https://x.trycloudflare.com".into(),
            bind: "127.0.0.1:62800".into(),
            project: "/Users/p/keel".into(),
            pairing_code: "ABCD-EFGH".into(),
            started_at: chrono::Utc::now(),
            tunnel: Some(TunnelStatus {
                provider: "cloudflared".into(),
                pid: Some(4243),
                name: None,
            }),
            allowlist_prefixes: 1,
        };
        write_status(dir.path(), &serving).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(status_path(dir.path()))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        assert_eq!(read_status(dir.path()).unwrap(), serving);
        clear_status(dir.path(), 1);
        assert!(
            read_status(dir.path()).is_some(),
            "another pid clears nothing"
        );
        clear_status(dir.path(), 4242);
        assert!(read_status(dir.path()).is_none());
    }
}
