//! Explicit terminal actions. Paths and terminal identities travel as arguments,
//! never as shell programs, and opening an agent never types into its prompt.
//!
//! Reaching an agent that runs outside AgentDocker is two steps: inside a
//! multiplexer (tmux, herdr), choose its pane; then bring forward the
//! application its terminal is in, selecting the exact tab where that
//! application can (Terminal and iTerm, by the tab's terminal device). An
//! agent that turns out to run under AgentDocker itself is not another
//! application to bring forward: the app shows it in its own terminal.
use std::path::{Path, PathBuf};
use std::time::Duration;

use agentdocker_core::multiplexer::Session;
use agentdocker_core::terminal::Host;
use agentdocker_host::terminal::Reach;

#[derive(Clone, Debug)]
pub enum Request {
    Project(PathBuf),
    Agent {
        pid: u32,
        started_at: chrono::DateTime<chrono::Utc>,
        /// The multiplexer pane it registered from, if any.
        session: Option<Session>,
        /// The provider's own session id, which herdr also knows the
        /// agent by: a pane moved since it registered is found by it.
        provider_session: Option<String>,
    },
}

/// Where an agent was found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Opened {
    /// Shown in another application, as the status line says it.
    Shown(String),
    /// It runs under AgentDocker itself, in the session whose process is
    /// `pid`: the app shows that session's own terminal.
    InApp { pid: u32 },
}

/// What was brought forward, for the status line, or the session of the
/// app's own that holds the agent.
pub fn open(request: Request) -> Result<Opened, String> {
    match request {
        Request::Project(path) => {
            open_project(&path).map(|()| Opened::Shown("Terminal opened".to_owned()))
        }
        Request::Agent {
            pid,
            started_at,
            session,
            provider_session,
        } => {
            if agentdocker_host::procinfo::start_time(pid) != Some(started_at) {
                return Err(ENDED.into());
            }
            open_agent(
                pid,
                started_at,
                session.as_ref(),
                provider_session.as_deref(),
            )
        }
    }
}

const ENDED: &str = "This agent has ended. Its old terminal was not opened.";

/// An agent under AgentDocker that no session of the app shows.
pub const IN_AGENTDOCKER: &str = "This agent runs inside AgentDocker, not in another app's terminal, and none of its sessions shows it.";

fn run(root: &Path, args: Vec<String>, timeout: Duration) -> Result<String, String> {
    let output = agentdocker_host::command::run(root, &args, timeout)
        .map_err(|e| format!("Could not open the terminal: {e}"))?;
    if !output.success {
        return Err(format!(
            "Could not open the terminal: {}",
            output.text.trim()
        ));
    }
    Ok(output.stdout)
}

fn project_directory(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() || !path.is_dir() {
        return Err(
            "The project folder is unavailable. Locate it before opening a terminal.".into(),
        );
    }
    path.canonicalize()
        .map_err(|e| format!("Could not open the project folder: {e}"))
}

fn open_project(path: &Path) -> Result<(), String> {
    let path = project_directory(path)?;
    #[cfg(target_os = "macos")]
    {
        // Terminal's directory open action starts a fresh shell at the folder.
        // It does not send a cd command into an existing agent's input.
        run(
            &path,
            vec![
                "/usr/bin/open".into(),
                "-a".into(),
                "Terminal".into(),
                path.to_str()
                    .ok_or("The project folder name cannot be passed to Terminal.")?
                    .to_owned(),
            ],
            Duration::from_secs(8),
        )?;
        Ok(())
    }
    #[cfg(target_os = "linux")]
    {
        // A fresh terminal inherits cwd. No terminal command contains the path
        // as executable shell text. Reap the emulator when it eventually exits.
        for executable in [
            "x-terminal-emulator",
            "gnome-terminal",
            "konsole",
            "xfce4-terminal",
            "xterm",
        ] {
            let mut command = std::process::Command::new(executable);
            command
                .current_dir(&path)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            match executable {
                "gnome-terminal" | "xfce4-terminal" => {
                    command.arg("--working-directory").arg(&path);
                }
                "konsole" => {
                    command.arg("--workdir").arg(&path);
                }
                _ => {}
            }
            match command.spawn() {
                Ok(mut child) => {
                    std::thread::spawn(move || {
                        let _ = child.wait();
                    });
                    return Ok(());
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(format!("Could not start the project terminal: {e}")),
            }
        }
        Err("No terminal application was found. Install a terminal and try again.".into())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = path;
        Err("Opening a project terminal is not available on this platform yet.".into())
    }
}

fn open_agent(
    pid: u32,
    started_at: chrono::DateTime<chrono::Utc>,
    session: Option<&Session>,
    provider_session: Option<&str>,
) -> Result<Opened, String> {
    // Inside a multiplexer the agent's own ancestry ends at its server;
    // the window to bring forward is the one the multiplexer's client is
    // in, once the agent's pane is chosen.
    let (reach, pane) = match session {
        Some(session) if session.kind == "tmux" && session.pane.is_some() => {
            let pane = session.pane.as_deref().unwrap_or_default();
            (tmux::select(pane)?, Some(format!("tmux {pane}")))
        }
        Some(session) if session.kind == "herdr" && session.pane.is_some() => {
            let recorded = session.pane.as_deref().unwrap_or_default();
            let (reach, pane) =
                herdr::select(session.session.as_deref(), recorded, provider_session)?;
            (reach, Some(format!("herdr {pane}")))
        }
        _ => (pid, None),
    };
    let found = agentdocker_host::terminal::reach_process(reach);
    if agentdocker_host::procinfo::start_time(pid) != Some(started_at) {
        return Err(ENDED.into());
    }
    let host = match found {
        Reach::Host(host) => host,
        // Under AgentDocker's own daemon or app (a multiplexer's client
        // included, its pane now chosen): no other window holds it, and
        // the app shows the session it runs in.
        Reach::AgentDocker { child } => return Ok(Opened::InApp { pid: child }),
        Reach::Unknown => {
            return Err(
                "This agent's terminal could not be found. It may run inside another app; use that app to continue."
                    .into(),
            );
        }
    };
    let shown = focus(&host, pane.is_some())?;
    Ok(Opened::Shown(match pane {
        Some(pane) => format!("{shown} · {pane}"),
        None => shown,
    }))
}

/// A host that names AgentDocker itself is never brought forward: the
/// person is already in it, and saying so would be no answer.
fn another_app(host: &Host) -> Result<(), String> {
    if host.is_agentdocker() {
        Err(IN_AGENTDOCKER.into())
    } else {
        Ok(())
    }
}

/// Bring the host forward, on its exact tab where it can select one.
#[cfg(target_os = "macos")]
fn focus(host: &Host, _pane_chosen: bool) -> Result<String, String> {
    another_app(host)?;
    let script = match host.kind.as_str() {
        "terminal" => Some(FOCUS_TERMINAL),
        "iterm2" => Some(FOCUS_ITERM),
        _ => None,
    };
    if let (Some(script), Some(tty)) = (script, host.tty.as_deref()) {
        let selected = run(
            Path::new("/"),
            vec![
                "/usr/bin/osascript".into(),
                "-l".into(),
                "JavaScript".into(),
                "-e".into(),
                script.into(),
                tty.to_owned(),
            ],
            Duration::from_secs(30),
        );
        match selected {
            Ok(_) => return Ok(format!("Opened {}", host.describe())),
            Err(error) => {
                activate(host)?;
                return Ok(if permission_refused(&error) {
                    format!(
                        "Brought {name} forward. To open the agent's own tab, allow AgentDocker to control {name} in System Settings → Privacy & Security → Automation.",
                        name = host.name
                    )
                } else {
                    format!(
                        "Brought {} forward; the agent's tab was not found.",
                        host.name
                    )
                });
            }
        }
    }
    activate(host)?;
    Ok(format!("Brought {} forward", host.name))
}

/// Elsewhere AgentDocker cannot raise another application's window. A
/// chosen multiplexer pane is still where the agent now shows.
#[cfg(not(target_os = "macos"))]
fn focus(host: &Host, pane_chosen: bool) -> Result<String, String> {
    another_app(host)?;
    if pane_chosen {
        Ok(format!("Showing it in {}", host.name))
    } else {
        Err(format!(
            "This agent runs in {}. Switch to it there; raising another app's window is not supported on this system yet.",
            host.name
        ))
    }
}

#[cfg(target_os = "macos")]
fn activate(host: &Host) -> Result<(), String> {
    let app = host
        .app
        .as_ref()
        .and_then(|app| app.to_str())
        .ok_or_else(|| format!("{} cannot be brought forward.", host.name))?;
    // `open -a` on a running application activates it; it starts nothing
    // and types nothing.
    run(
        Path::new("/"),
        vec!["/usr/bin/open".into(), "-a".into(), app.to_owned()],
        Duration::from_secs(8),
    )
    .map(|_| ())
}

/// macOS refuses an Apple event the person has not allowed with -1743.
#[cfg(any(target_os = "macos", test))]
fn permission_refused(error: &str) -> bool {
    error.contains("-1743") || error.contains("Not authorized") || error.contains("not allowed")
}

/// A program installed outside the minimal path a desktop app is given.
fn tool(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .chain(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"].map(PathBuf::from))
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

mod tmux {
    use super::*;

    /// Make `pane` the visible pane of its window and session, and return
    /// the client that now shows it.
    pub(super) fn select(pane: &str) -> Result<u32, String> {
        if !valid_pane(pane) {
            return Err("This agent's tmux pane is not one tmux names.".into());
        }
        let tmux = tool("tmux").ok_or("tmux could not be found to show this agent's pane.")?;
        let tmux = tmux.to_string_lossy().into_owned();
        let call = |args: &[&str]| {
            let mut argv = vec![tmux.clone()];
            argv.extend(args.iter().map(|arg| (*arg).to_owned()));
            run(Path::new("/"), argv, Duration::from_secs(5))
        };
        let session = call(&["display-message", "-p", "-t", pane, "#{session_name}"])
            .map_err(|_| "This agent's tmux pane is gone.".to_owned())?;
        let session = session.trim().to_owned();
        call(&["select-window", "-t", pane])?;
        call(&["select-pane", "-t", pane])?;
        let clients = call(&[
            "list-clients",
            "-F",
            "#{client_pid}\t#{client_tty}\t#{client_session}\t#{client_activity}",
        ])?;
        let (pid, tty, on_session) = client(&clients, &session).ok_or_else(|| {
            format!("No terminal is attached to tmux session {session}. Run `tmux attach -t {session}` to see the agent.")
        })?;
        if !on_session {
            call(&["switch-client", "-c", &tty, "-t", pane])?;
        }
        Ok(pid)
    }

    /// `%` and a number, as tmux names a pane.
    pub(super) fn valid_pane(pane: &str) -> bool {
        pane.strip_prefix('%')
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    }

    /// The client to show the pane in: one already on its session, else
    /// the most recently active. `(pid, tty, already on the session)`.
    pub(super) fn client(listing: &str, session: &str) -> Option<(u32, String, bool)> {
        let clients: Vec<(u32, String, bool, u64)> = listing
            .lines()
            .filter_map(|line| {
                let mut fields = line.split('\t');
                let pid = fields.next()?.parse().ok()?;
                let tty = fields.next()?.to_owned();
                let on = fields.next()? == session;
                let activity = fields.next().and_then(|a| a.parse().ok()).unwrap_or(0);
                Some((pid, tty, on, activity))
            })
            .collect();
        clients
            .iter()
            .filter(|c| c.2)
            .max_by_key(|c| c.3)
            .or_else(|| clients.iter().max_by_key(|c| c.3))
            .map(|(pid, tty, on, _)| (*pid, tty.clone(), *on))
    }
}

mod herdr {
    use super::*;

    /// Focus the agent's pane in herdr over its socket, and return a
    /// herdr client process (the terminal window herdr is drawn in) with
    /// the pane shown. A pane moved to another workspace gets a new id, so
    /// the one herdr says holds the agent's own session is taken first;
    /// the id recorded when it registered is the fallback.
    pub(super) fn select(
        session: Option<&str>,
        recorded: &str,
        provider_session: Option<&str>,
    ) -> Result<(u32, String), String> {
        let socket = socket(session).ok_or("This agent's herdr session cannot be addressed.")?;
        let pane = provider_session
            .and_then(|id| holding(&socket, id))
            .unwrap_or_else(|| recorded.to_owned());
        focus(&socket, &pane)?;
        let client = client().ok_or_else(|| {
            "herdr has no window open. Run `herdr` to attach and see the agent.".to_owned()
        })?;
        Ok((client, pane))
    }

    /// The pane herdr says holds the provider session `id`, if exactly one
    /// does: `pane.list`, each pane's `agent_session.value`.
    #[cfg(unix)]
    fn holding(socket: &Path, id: &str) -> Option<String> {
        let request = serde_json::json!({
            "id": "agentdocker-find",
            "method": "pane.list",
            "params": {},
        });
        pane_holding(&ask(socket, &request).ok()?, id)
    }

    #[cfg(not(unix))]
    fn holding(_socket: &Path, _id: &str) -> Option<String> {
        None
    }

    #[cfg(any(unix, test))]
    pub(super) fn pane_holding(reply: &str, id: &str) -> Option<String> {
        use agentdocker_core::multiplexer::herdr;
        herdr::holding(&herdr::panes(reply).ok()?, [id])
            .remove(id)
            .map(|held| held.pane)
    }

    /// One request and its one-line answer, over herdr's Unix socket.
    #[cfg(unix)]
    fn ask(socket: &Path, request: &serde_json::Value) -> Result<String, String> {
        use std::io::{BufRead, BufReader, Read, Write};
        let unreachable = |_| "herdr is not running for this agent's session.".to_owned();
        let mut stream = std::os::unix::net::UnixStream::connect(socket).map_err(unreachable)?;
        let limit = Some(Duration::from_secs(3));
        let _ = stream.set_read_timeout(limit);
        let _ = stream.set_write_timeout(limit);
        stream
            .write_all(format!("{request}\n").as_bytes())
            .map_err(|e| format!("Could not ask herdr: {e}"))?;
        let mut reply = String::new();
        BufReader::new(stream)
            .take(4 * 1024 * 1024)
            .read_line(&mut reply)
            .map_err(|e| format!("herdr did not answer: {e}"))?;
        Ok(reply)
    }

    /// herdr's API socket: the default session's, or a named session's,
    /// under the person's home (where herdr puts it, not AgentDocker's).
    pub(super) fn socket(session: Option<&str>) -> Option<PathBuf> {
        agentdocker_host::multiplexer::herdr_socket(session)
    }

    /// One `pane.focus` request: newline-delimited JSON over herdr's
    /// Unix socket. It moves herdr's focus; it types nothing.
    #[cfg(unix)]
    fn focus(socket: &Path, pane: &str) -> Result<(), String> {
        let request = serde_json::json!({
            "id": "agentdocker-focus",
            "method": "pane.focus",
            "params": {"pane_id": pane},
        });
        let reply = ask(socket, &request)?;
        refused(&reply).map_or(Ok(()), Err)
    }

    #[cfg(not(unix))]
    fn focus(_socket: &Path, _pane: &str) -> Result<(), String> {
        Err("herdr is not available on this system.".into())
    }

    /// herdr's reply, `Some(reason)` when it refused.
    #[cfg(any(unix, test))]
    pub(super) fn refused(reply: &str) -> Option<String> {
        match serde_json::from_str::<serde_json::Value>(reply.trim()) {
            Ok(value) if value.get("result").is_some() => None,
            Ok(value) => Some(format!(
                "herdr would not show the pane: {}",
                value["error"]["message"]
                    .as_str()
                    .unwrap_or("no reason given")
            )),
            Err(_) => Some("herdr gave an answer AgentDocker cannot read.".into()),
        }
    }

    /// A herdr process with a terminal of its own: a client, not the
    /// server, which runs detached.
    fn client() -> Option<u32> {
        agentdocker_host::procinfo::processes()
            .ok()?
            .into_iter()
            .filter(|process| {
                process.argv.first().is_some_and(|program| {
                    Path::new(program).file_name().and_then(|f| f.to_str()) == Some("herdr")
                })
            })
            .find(|process| agentdocker_host::terminal::device_of(process.pid).is_some())
            .map(|process| process.pid)
    }
}

#[cfg(target_os = "macos")]
const FOCUS_TERMINAL: &str = r#"function run(argv) {
    const terminal = Application('Terminal');
    if (!terminal.running()) throw new Error('The original Terminal window is closed.');
    const matches = [];
    for (const w of terminal.windows()) for (const t of (w.tabs() || [])) {
        if (t.tty() === argv[0]) matches.push({window:w, tab:t});
    }
    if (matches.length !== 1) throw new Error('The original terminal was not found in Terminal. Open the app where this agent started.');
    matches[0].tab.selected = true;
    matches[0].window.index = 1;
    terminal.activate();
}"#;

#[cfg(target_os = "macos")]
const FOCUS_ITERM: &str = r#"function run(argv) {
    const iterm = Application('com.googlecode.iterm2');
    if (!iterm.running()) throw new Error('The original iTerm window is closed.');
    for (const w of iterm.windows()) for (const t of w.tabs()) for (const s of t.sessions()) {
        if (s.tty() === argv[0]) {
            s.select();
            t.select();
            w.select();
            iterm.activate();
            return;
        }
    }
    throw new Error('The original terminal was not found in iTerm.');
}"#;

#[cfg(test)]
mod tests {
    use super::*;

    /// The pane herdr says holds the agent's session, by `pane.list`'s own
    /// shape; none when no pane, or more than one, holds it.
    #[test]
    fn a_moved_herdr_pane_is_found_by_its_agent_session() {
        let reply = serde_json::json!({"id": "agentdocker-find", "result": {"type": "pane_list", "panes": [
            {"pane_id": "w1:p1", "agent_session": {"agent": "claude", "kind": "id", "source": "hook", "value": "s-other"}},
            {"pane_id": "w2:p4", "agent_session": {"agent": "claude", "kind": "id", "source": "hook", "value": "s-mine"}},
            {"pane_id": "w1:p2", "agent_session": null},
        ]}}).to_string();
        assert_eq!(
            herdr::pane_holding(&reply, "s-mine").as_deref(),
            Some("w2:p4")
        );
        assert_eq!(herdr::pane_holding(&reply, "s-gone"), None);
        let twice = reply.replace("s-other", "s-mine");
        assert_eq!(
            herdr::pane_holding(&twice, "s-mine"),
            None,
            "two panes: not one to choose"
        );
        assert_eq!(herdr::pane_holding("not json", "s-mine"), None);
    }
    #[test]
    fn project_paths_are_checked_as_directories_without_interpreting_shell_text() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("project ' $(touch marker) ; with spaces");
        std::fs::create_dir(&path).unwrap();
        assert_eq!(
            project_directory(&path).unwrap(),
            path.canonicalize().unwrap()
        );
        assert!(!root.path().join("marker").exists());
        assert!(project_directory(Path::new("relative")).is_err());
        assert!(project_directory(&root.path().join("missing")).is_err());
    }

    #[test]
    fn only_a_tmux_pane_name_reaches_tmux() {
        assert!(tmux::valid_pane("%3"));
        assert!(tmux::valid_pane("%120"));
        for pane in ["", "%", "3", "%3;kill-server", "%-1", "-t", "%3 "] {
            assert!(!tmux::valid_pane(pane), "{pane:?}");
        }
    }

    #[test]
    fn the_pane_shows_in_a_client_on_its_session_before_any_other() {
        let listing = "501\t/dev/ttys002\tother\t900\n502\t/dev/ttys004\twork\t100\n503\t/dev/ttys006\twork\t300\n";
        assert_eq!(
            tmux::client(listing, "work"),
            Some((503, "/dev/ttys006".into(), true))
        );
        // Nobody on the session: the most recently active client is
        // switched to it.
        assert_eq!(
            tmux::client(listing, "review"),
            Some((501, "/dev/ttys002".into(), false))
        );
        assert_eq!(tmux::client("", "work"), None);
    }

    #[test]
    fn herdr_sockets_follow_its_session_names_and_refuse_paths() {
        let base = std::env::home_dir().unwrap().join(".config/herdr");
        assert_eq!(herdr::socket(None), Some(base.join("herdr.sock")));
        assert_eq!(
            herdr::socket(Some("backend")),
            Some(base.join("sessions/backend/herdr.sock"))
        );
        for name in ["", "../x", "a/b", ".hidden", "a b"] {
            assert_eq!(herdr::socket(Some(name)), None, "{name:?}");
        }
    }

    #[test]
    fn a_herdr_reply_is_a_result_or_a_reason() {
        assert_eq!(
            herdr::refused(r#"{"id":"agentdocker-focus","result":{"type":"ok"}}"#),
            None
        );
        assert_eq!(
            herdr::refused(r#"{"id":"x","error":{"code":"not_found","message":"no pane w9:p9"}}"#)
                .as_deref(),
            Some("herdr would not show the pane: no pane w9:p9")
        );
        assert!(herdr::refused("garbage").is_some());
    }

    #[test]
    fn a_refused_apple_event_is_told_apart_from_a_missing_tab() {
        assert!(permission_refused(
            "Could not open the terminal: execution error: Not authorized to send Apple events to Terminal. (-1743)"
        ));
        assert!(!permission_refused(
            "Could not open the terminal: Error: The original terminal was not found in Terminal."
        ));
    }

    /// A host naming AgentDocker, as a record kept from before the walk
    /// passed over AgentDocker's own programs may, is refused before
    /// anything is activated: never *Brought AgentDocker forward*.
    #[test]
    fn agentdocker_itself_is_never_brought_forward() {
        let kept = Host {
            kind: "app".into(),
            name: "AgentDocker".into(),
            app: Some(PathBuf::from("/Applications/AgentDocker.app")),
            tty: Some("/dev/ttys005".into()),
        };
        for pane_chosen in [false, true] {
            assert_eq!(
                focus(&kept, pane_chosen).unwrap_err(),
                IN_AGENTDOCKER,
                "{pane_chosen}"
            );
        }
        assert!(
            another_app(&Host {
                kind: "terminal".into(),
                name: "Terminal".into(),
                app: Some(PathBuf::from("/System/Applications/Utilities/Terminal.app")),
                tty: Some("/dev/ttys005".into()),
            })
            .is_ok()
        );
    }

    #[test]
    fn an_old_process_identity_cannot_open_another_agents_terminal() {
        let result = open(Request::Agent {
            pid: std::process::id(),
            started_at: chrono::DateTime::UNIX_EPOCH,
            session: None,
            provider_session: None,
        });
        assert!(result.unwrap_err().contains("ended"));
    }
}
