//! The connector as a login service, so a browser agent's way in outlives
//! the terminal that first opened it: a launchd agent on macOS, a systemd
//! user unit on Linux, running `agentdocker connector serve` with the
//! arguments given at install time. The daemon's own service does the
//! same for agentd; this borrows its shapes and commands.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};

use super::ServeArgs;
#[cfg(not(windows))]
use crate::service::execute;
#[cfg(any(not(windows), test))]
use crate::service::{Cmd, Plan, systemd_uninstall_plan};

#[cfg(any(windows, test))]
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) mod windows;

pub const LABEL: &str = "dev.agentdocker.connector";
pub const UNIT: &str = "agentdocker-connector.service";

/// What a serving connector writes about itself lives in the host crate,
/// because the desktop reads it too.
pub use agentdocker_host::connector::{
    Serving, TunnelStatus, clear_status, read_status, write_status,
};

/// The service's files and commands, from the executable, the state home
/// and the arguments `serve` was given.
pub struct Layout {
    #[cfg_attr(windows, allow(dead_code))]
    pub agentdocker: PathBuf,
    pub home: PathBuf,
    #[cfg_attr(not(windows), allow(dead_code))]
    pub socket: Option<PathBuf>,
    pub user_home: PathBuf,
    #[cfg_attr(windows, allow(dead_code))]
    pub uid: u32,
    pub serve_args: Vec<String>,
    /// Directories the service's PATH must have: where cloudflared is.
    #[cfg_attr(windows, allow(dead_code))]
    pub path_dirs: Vec<PathBuf>,
}

/// Filesystem inputs shared by registration and strict maintenance inventory.
#[derive(Default, Debug)]
pub(crate) struct Resources {
    pub(crate) executables: Vec<PathBuf>,
    pub(crate) data: Vec<PathBuf>,
}

/// Refuse unknown options rather than assuming they contain no filesystem path.
pub(crate) fn argument_resources(args: &[String]) -> Result<Resources> {
    let mut resources = Resources::default();
    let mut pairs = args.chunks_exact(2);
    for pair in &mut pairs {
        match pair[0].as_str() {
            "--cloudflared" | "--tailscale" => resources.executables.push(PathBuf::from(&pair[1])),
            "--project" => resources.data.push(PathBuf::from(&pair[1])),
            "--allow-from" => {
                if let Some(path) = pair[1].strip_prefix('@') {
                    anyhow::ensure!(!path.is_empty(), "connector feed path is empty");
                    resources.data.push(PathBuf::from(path));
                }
            }
            "--public-url" | "--bind" | "--allow-callback" | "--tunnel" | "--tunnel-name"
            | "--tunnel-port" | "--client-ip-header" => {}
            _ => bail!("unrecognized connector service argument; registration refused"),
        }
    }
    anyhow::ensure!(
        pairs.remainder().is_empty(),
        "incomplete connector service arguments"
    );
    Ok(resources)
}

impl Layout {
    /// Serialize all selected resource stores with maintenance, including
    /// resources outside the installation containing the connector executable.
    fn registration_guard(&self) -> Result<Vec<agentdocker_host::lock::Lock>> {
        let mut executables = vec![self.agentdocker.clone()];
        let mut data = vec![self.home.clone(), self.log(), self.user_home.clone()];
        let resources = argument_resources(&self.serve_args)?;
        executables.extend(resources.executables);
        data.extend(resources.data);
        Ok(agentdocker_host::installation::guard_service_references(
            &executables,
            &data,
        )?)
    }

    #[cfg(any(not(windows), test))]
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

    #[cfg(any(not(windows), test))]
    pub fn plist_path(&self) -> PathBuf {
        self.user_home
            .join("Library/LaunchAgents")
            .join(format!("{LABEL}.plist"))
    }

    #[cfg(any(not(windows), test))]
    pub fn unit_path(&self) -> PathBuf {
        self.user_home.join(".config/systemd/user").join(UNIT)
    }

    #[cfg(any(not(windows), test))]
    fn domain(&self) -> String {
        format!("gui/{}", self.uid)
    }

    #[cfg(any(not(windows), test))]
    fn target(&self) -> String {
        format!("gui/{}/{LABEL}", self.uid)
    }

    #[cfg(any(not(windows), test))]
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

#[cfg(any(not(windows), test))]
fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(any(not(windows), test))]
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
#[cfg(any(not(windows), test))]
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

#[cfg(any(not(windows), test))]
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

#[cfg(any(not(windows), test))]
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
            ..Plan::default()
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
            ..Plan::default()
        }
    }
}

#[cfg(any(not(windows), test))]
pub fn uninstall_plan(layout: &Layout, macos: bool) -> Plan {
    if macos {
        Plan {
            files: vec![],
            remove: vec![layout.plist_path()],
            commands: vec![Cmd {
                argv: vec!["launchctl".into(), "bootout".into(), layout.target()],
                tolerated: true,
            }],
            ..Plan::default()
        }
    } else {
        systemd_uninstall_plan(UNIT, layout.unit_path())
    }
}

fn layout(args: &ServeArgs) -> Result<Layout> {
    let captured = capture_service_paths(args, &std::env::current_dir()?)?;
    let args = &captured;
    let agentdocker = std::env::current_exe().context("cannot locate this executable")?;
    let home = agentdocker_host::project::try_canonical(&agentdocker_host::dirs::home())
        .context("cannot resolve connector service state directory")?;
    let user_home = agentdocker_host::project::try_canonical(
        &std::env::home_dir().context("no home directory")?,
    )?;
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
    Ok(Layout {
        agentdocker,
        home,
        socket: args.service_socket.clone(),
        user_home,
        uid: crate::service::current_uid_for_service(),
        serve_args,
        path_dirs,
    })
}

#[cfg(not(windows))]
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
    let _registration = if dry_run {
        Vec::new()
    } else {
        layout.registration_guard()?
    };
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

/// Desktop setup must not replace a service configured outside this action.
/// A complete, synced definition is published exclusively. A concurrent creator
/// is compared, never overwritten or observed while its content is incomplete.
#[cfg(any(not(windows), test))]
fn ensure_definition(path: &std::path::Path, contents: &str) -> Result<()> {
    use std::io::{Read, Write};
    let parent = path.parent().context("service definition has no parent")?;
    std::fs::create_dir_all(parent)?;
    let mut staged = tempfile::NamedTempFile::new_in(parent)?;
    staged.write_all(contents.as_bytes())?;
    staged.as_file().sync_all()?;
    // A same-directory link publishes all bytes together without replacing a
    // path another creator won. Failure before this point leaves no definition.
    match std::fs::hard_link(staged.path(), path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            anyhow::ensure!(
                std::fs::symlink_metadata(path)?.file_type().is_file(),
                "connector service definition is not a regular file; nothing was changed"
            );
            let mut actual = String::new();
            agentdocker_host::files::open_regular(path)?
                .take(contents.len() as u64 + 1)
                .read_to_string(&mut actual)?;
            anyhow::ensure!(
                actual == contents,
                "a differently configured connector service already exists; use `agentdocker connector install` to review and replace it explicitly; nothing was changed"
            );
        }
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

/// Start an identical installed service, or install it without overwriting any
/// existing definition. This is the conservative entry point for desktop setup.
#[cfg(not(windows))]
pub fn enable(args: &ServeArgs, dry_run: bool) -> Result<()> {
    let macos = cfg!(target_os = "macos");
    anyhow::ensure!(
        macos || cfg!(target_os = "linux"),
        "browser connector services require macOS or Linux"
    );
    anyhow::ensure!(
        args.tunnel.is_some() || args.public_url.is_some(),
        "choose a tunnel or provide its public URL"
    );
    let layout = layout(args)?;
    let mut plan = install_plan(&layout, macos);
    if macos {
        // Bootstrap is idempotent only when this exact definition is already
        // loaded. Kickstart (without -k) leaves a running service undisturbed.
        plan.commands = vec![
            Cmd {
                argv: vec![
                    "launchctl".into(),
                    "bootstrap".into(),
                    layout.domain(),
                    layout.plist_path().to_string_lossy().into_owned(),
                ],
                tolerated: true,
            },
            Cmd {
                argv: vec!["launchctl".into(), "kickstart".into(), layout.target()],
                tolerated: false,
            },
        ];
    }
    if dry_run {
        return execute(&plan, true);
    }
    let _registration = layout.registration_guard()?;
    for (path, contents) in &plan.files {
        ensure_definition(path, contents)?;
    }
    agentdocker_host::dirs::secure_state_dir(&layout.home.join("connector"))?;
    plan.files.clear();
    execute(&plan, false)?;
    eprintln!(
        "Browser connector service enabled. It starts at login; its address and pairing code appear when the tunnel is ready."
    );
    if macos {
        eprintln!("Startup log: {}", layout.log().display());
    } else {
        eprintln!("Startup log: journalctl --user -u {UNIT}");
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn uninstall(dry_run: bool) -> Result<()> {
    let macos = cfg!(target_os = "macos");
    if !macos && !cfg!(target_os = "linux") {
        bail!("the connector service is supported on macOS (launchd) and Linux (systemd) only");
    }
    let layout = layout(&ServeArgs::default())?;
    execute(&uninstall_plan(&layout, macos), dry_run)
}

/// A login task does not inherit the installing shell's working directory.
/// Capture configured project, feed and executable paths before registration.
fn capture_service_paths(args: &ServeArgs, cwd: &std::path::Path) -> Result<ServeArgs> {
    anyhow::ensure!(
        cwd.is_absolute(),
        "service setup directory must be absolute"
    );
    let resolve = |path: &std::path::Path| {
        cwd.join(path)
            .canonicalize()
            .with_context(|| format!("cannot resolve connector service path {}", path.display()))
    };
    let mut args = args.clone();
    for path in [
        &mut args.project,
        &mut args.cloudflared,
        &mut args.tailscale,
    ]
    .into_iter()
    .flatten()
    {
        *path = resolve(path)?;
    }
    for value in &mut args.allow_from {
        if let Some(raw) = value.strip_prefix('@') {
            anyhow::ensure!(!raw.is_empty(), "connector feed path is empty");
            let path = resolve(std::path::Path::new(raw))?;
            *value = format!(
                "@{}",
                path.to_str().context("connector feed path is not UTF-8")?
            );
        }
    }
    Ok(args)
}

#[cfg(windows)]
fn windows_layout(args: &ServeArgs) -> Result<Layout> {
    if args.tunnel.is_none() && args.public_url.is_none() {
        bail!("choose a tunnel or provide its public URL");
    }
    let captured =
        capture_windows_arguments(args, &std::env::current_dir()?, |tunnel| match tunnel {
            "cloudflared" => super::tunnel::find_cloudflared(None),
            "tailscale" => super::tunnel::find_tailscale(None),
            _ => unreachable!("only supported tunnel variants are resolved"),
        })?;
    layout(&captured)
}

#[cfg(any(windows, test))]
fn capture_windows_arguments(
    args: &ServeArgs,
    cwd: &std::path::Path,
    find: impl Fn(&str) -> Result<PathBuf>,
) -> Result<ServeArgs> {
    let mut args = capture_service_paths(args, cwd)?;
    // Pin discovery before serialization so install and reparsed service-run
    // produce the same argument order, even with trailing allowlist options.
    match args.tunnel.as_deref() {
        Some("cloudflared") if args.cloudflared.is_none() => {
            args.cloudflared = Some(find("cloudflared")?.canonicalize()?);
        }
        Some("tailscale") if args.tailscale.is_none() => {
            args.tailscale = Some(find("tailscale")?.canonicalize()?);
        }
        _ => {}
    }
    Ok(args)
}

#[cfg(windows)]
pub fn install(args: &ServeArgs, dry_run: bool) -> Result<()> {
    windows::install(&windows_layout(args)?, dry_run, false)
}

#[cfg(windows)]
pub fn enable(args: &ServeArgs, dry_run: bool) -> Result<()> {
    windows::install(&windows_layout(args)?, dry_run, true)
}

#[cfg(windows)]
pub fn uninstall(dry_run: bool) -> Result<()> {
    windows::uninstall(&layout(&ServeArgs::default())?, dry_run)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connector_registration_guards_tunnel_project_and_feed_versions_in_other_stores() {
        use agentdocker_host::{dirs, installation, lock};
        for option in ["--cloudflared", "--tailscale", "--project", "--allow-from"] {
            let temp = tempfile::tempdir().unwrap();
            let store = temp.path().join(if cfg!(windows) {
                "AgentDocker/desktop"
            } else {
                ".local/share/agentdocker/desktop"
            });
            dirs::secure_state_dir(&store).unwrap();
            let payload = store.join("versions").join("a".repeat(64)).join("payload");
            std::fs::create_dir_all(&payload).unwrap();
            let executable = temp.path().join("portable-controller");
            let resource = payload.join("selected-resource");
            std::fs::write(&executable, "fixture").unwrap();
            if option == "--project" {
                std::fs::create_dir(&resource).unwrap();
            } else {
                std::fs::write(&resource, "fixture").unwrap();
            }
            let value = if option == "--allow-from" {
                format!("@{}", resource.display())
            } else {
                resource.to_string_lossy().into_owned()
            };
            let mut layout = layout();
            layout.agentdocker = executable;
            layout.home = temp.path().join("absent-state");
            layout.user_home = temp.path().to_owned();
            layout.serve_args = vec![option.into(), value];
            let _guard = layout.registration_guard().unwrap();
            assert_eq!(
                installation::service_inventory_guard(&store, true)
                    .unwrap_err()
                    .kind(),
                std::io::ErrorKind::WouldBlock
            );
            assert!(
                lock::try_exclusive_existing(
                    &installation::pin_path(&store, &"a".repeat(64)).unwrap()
                )
                .unwrap()
                .is_none()
            );
            assert!(!layout.home.exists());
        }
    }

    #[test]
    fn discovered_windows_tunnel_arguments_survive_service_run_reparse() {
        use clap::Parser;
        #[derive(Parser)]
        struct Parsed {
            #[command(flatten)]
            args: ServeArgs,
        }
        let temp = tempfile::tempdir().unwrap();
        let binary = temp.path().join("tunnel.exe");
        std::fs::write(&binary, "fixture").unwrap();
        for tunnel in ["cloudflared", "tailscale"] {
            let initial = Parsed::parse_from([
                "test",
                "--tunnel",
                tunnel,
                "--allow-from",
                "openai",
                "--allow-from",
                "anthropic",
            ])
            .args;
            let captured = capture_windows_arguments(&initial, temp.path(), |kind| {
                assert_eq!(kind, tunnel);
                Ok(binary.clone())
            })
            .unwrap();
            let installed = super::layout(&captured).unwrap();
            let reparsed = Parsed::parse_from(
                std::iter::once("test".to_string()).chain(installed.serve_args.clone()),
            )
            .args;
            let running = super::layout(&reparsed).unwrap();
            assert_eq!(installed.serve_args, running.serve_args);
            assert!(initial.cloudflared.is_none() && initial.tailscale.is_none());
        }
    }

    fn layout() -> Layout {
        Layout {
            agentdocker: "/opt/agentdocker".into(),
            home: "/Users/p/.agentdocker".into(),
            socket: None,
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
    fn login_configuration_captures_paths_without_changing_the_callers_arguments() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("project")).unwrap();
        std::fs::write(temp.path().join("egress.txt"), "127.0.0.1/32\n").unwrap();
        std::fs::write(temp.path().join("tunnel.exe"), "fixture").unwrap();
        let args = ServeArgs {
            project: Some("project".into()),
            cloudflared: Some("tunnel.exe".into()),
            allow_from: vec!["@egress.txt".into(), "openai".into(), "192.0.2.0/24".into()],
            ..ServeArgs::default()
        };
        let captured = capture_service_paths(&args, temp.path()).unwrap();
        assert_eq!(
            captured.project.unwrap(),
            temp.path().join("project").canonicalize().unwrap()
        );
        assert_eq!(
            captured.cloudflared.unwrap(),
            temp.path().join("tunnel.exe").canonicalize().unwrap()
        );
        assert_eq!(
            captured.allow_from[0],
            format!(
                "@{}",
                temp.path()
                    .join("egress.txt")
                    .canonicalize()
                    .unwrap()
                    .display()
            )
        );
        assert_eq!(&captured.allow_from[1..], &args.allow_from[1..]);
        assert_eq!(args.project.unwrap(), PathBuf::from("project"));
        assert_eq!(args.allow_from[0], "@egress.txt");
    }

    #[test]
    fn login_configuration_refuses_an_unresolved_feed_before_registration() {
        let temp = tempfile::tempdir().unwrap();
        for value in ["@missing", "@"] {
            let args = ServeArgs {
                allow_from: vec![value.into()],
                ..ServeArgs::default()
            };
            assert!(capture_service_paths(&args, temp.path()).is_err());
        }
    }

    #[test]
    fn desktop_enable_never_overwrites_an_existing_definition() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("user/connector.service");
        ensure_definition(&path, "first definition").unwrap();
        ensure_definition(&path, "first definition").unwrap();
        assert!(ensure_definition(&path, "different definition").is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "first definition");
        #[cfg(unix)]
        {
            let link = temp.path().join("linked.service");
            std::os::unix::fs::symlink(&path, &link).unwrap();
            assert!(ensure_definition(&link, "first definition").is_err());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "first definition");
        }
    }

    #[test]
    fn concurrent_enable_publishes_one_complete_definition() {
        use std::sync::{Arc, Barrier};
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("connector.service");
        let start = Arc::new(Barrier::new(8));
        let workers: Vec<_> = (0..8)
            .map(|index| {
                let path = path.clone();
                let start = start.clone();
                std::thread::spawn(move || {
                    let contents = if index % 2 == 0 { "a" } else { "b" }.repeat(1024 * 1024);
                    start.wait();
                    let result = ensure_definition(&path, &contents);
                    (contents, result)
                })
            })
            .collect();
        let results: Vec<_> = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect();
        let published = std::fs::read_to_string(&path).unwrap();
        assert_eq!(published.len(), 1024 * 1024);
        for (contents, result) in results {
            assert_eq!(result.is_ok(), contents == published, "{result:?}");
        }
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 1);
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
        assert!(uninstall.commands[0].argv.contains(&"stop".to_owned()));
        assert!(uninstall.commands[1].argv.contains(&"disable".to_owned()));
        assert!(
            uninstall.after_remove[0]
                .argv
                .contains(&"daemon-reload".to_owned())
        );
    }

    #[test]
    fn the_status_file_is_private_and_only_its_writer_clears_it() {
        let dir = tempfile::tempdir().unwrap();
        let serving = Serving {
            pid: 4242,
            public_url: "https://x.trycloudflare.com".into(),
            bind: "127.0.0.1:62800".into(),
            default_project: Some("/Users/p/keel".into()),
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
            let mode = std::fs::metadata(agentdocker_host::connector::status_path(dir.path()))
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
