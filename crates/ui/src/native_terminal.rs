//! Explicit terminal actions. Paths and terminal identities travel as arguments,
//! never as shell programs, and opening an agent never types into its prompt.
//!
//! Reaching an agent that runs outside AgentDocker is two steps: inside a
//! multiplexer (tmux, herdr), choose its pane; then bring forward the
//! application its terminal is in, selecting the exact tab where that
//! application can (Terminal and iTerm, by the tab's terminal device).
use std::path::{Path, PathBuf};
use std::time::Duration;

use agentdocker_core::multiplexer::Session;
use agentdocker_core::terminal::Host;

#[derive(Clone, Debug)]
pub enum Request {
    Project(PathBuf),
    Agent {
        pid: u32,
        started_at: chrono::DateTime<chrono::Utc>,
        /// The multiplexer pane it registered from, if any.
        session: Option<Session>,
    },
}

/// What was brought forward, for the status line.
pub fn open(request: Request) -> Result<String, String> {
    match request {
        Request::Project(path) => open_project(&path).map(|()| "Terminal opened".to_owned()),
        Request::Agent {
            pid,
            started_at,
            session,
        } => {
            if agentdocker_host::procinfo::start_time(pid) != Some(started_at) {
                return Err(ENDED.into());
            }
            open_agent(pid, started_at, session.as_ref())
        }
    }
}

const ENDED: &str = "This agent has ended. Its old terminal was not opened.";

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
) -> Result<String, String> {
    // Inside a multiplexer the agent's own ancestry ends at its server;
    // the window to bring forward is the one the multiplexer's client is
    // in, once the agent's pane is chosen.
    let (reach, pane) = match session {
        Some(session) if session.kind == "tmux" && session.pane.is_some() => {
            let pane = session.pane.as_deref().unwrap_or_default();
            (tmux::select(pane)?, Some(format!("tmux {pane}")))
        }
        Some(session) if session.kind == "herdr" && session.pane.is_some() => {
            let pane = session.pane.as_deref().unwrap_or_default();
            (
                herdr::select(session.session.as_deref(), pane)?,
                Some(format!("herdr {pane}")),
            )
        }
        _ => (pid, None),
    };
    let host = agentdocker_host::terminal::of_process(reach).ok_or(
        "This agent's terminal could not be found. It may run inside another app; use that app to continue.",
    )?;
    if agentdocker_host::procinfo::start_time(pid) != Some(started_at) {
        return Err(ENDED.into());
    }
    let shown = focus(&host, pane.is_some())?;
    Ok(match pane {
        Some(pane) => format!("{shown} · {pane}"),
        None => shown,
    })
}

/// Bring the host forward, on its exact tab where it can select one.
#[cfg(target_os = "macos")]
fn focus(host: &Host, _pane_chosen: bool) -> Result<String, String> {
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

    /// Focus `pane` in herdr over its socket, and return a herdr client
    /// process (the terminal window herdr is drawn in).
    pub(super) fn select(session: Option<&str>, pane: &str) -> Result<u32, String> {
        let socket = socket(session).ok_or("This agent's herdr session cannot be addressed.")?;
        focus(&socket, pane)?;
        client().ok_or_else(|| {
            "herdr has no window open. Run `herdr` to attach and see the agent.".to_owned()
        })
    }

    /// herdr's API socket: the default session's, or a named session's.
    pub(super) fn socket(session: Option<&str>) -> Option<PathBuf> {
        let base = agentdocker_host::dirs::home().join(".config").join("herdr");
        match session {
            None => Some(base.join("herdr.sock")),
            Some(name)
                if !name.is_empty()
                    && name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
                    && !name.starts_with('.') =>
            {
                Some(base.join("sessions").join(name).join("herdr.sock"))
            }
            Some(_) => None,
        }
    }

    /// One `pane.focus` request: newline-delimited JSON over herdr's
    /// Unix socket. It moves herdr's focus; it types nothing.
    #[cfg(unix)]
    fn focus(socket: &Path, pane: &str) -> Result<(), String> {
        use std::io::{BufRead, BufReader, Read, Write};
        let unreachable = |_| "herdr is not running for this agent's session.".to_owned();
        let mut stream = std::os::unix::net::UnixStream::connect(socket).map_err(unreachable)?;
        let limit = Some(Duration::from_secs(3));
        let _ = stream.set_read_timeout(limit);
        let _ = stream.set_write_timeout(limit);
        let request = serde_json::json!({
            "id": "agentdocker-focus",
            "method": "pane.focus",
            "params": {"pane_id": pane},
        });
        stream
            .write_all(format!("{request}\n").as_bytes())
            .map_err(|e| format!("Could not ask herdr to show the pane: {e}"))?;
        let mut reply = String::new();
        BufReader::new(stream)
            .take(64 * 1024)
            .read_line(&mut reply)
            .map_err(|e| format!("herdr did not answer: {e}"))?;
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
        let base = agentdocker_host::dirs::home().join(".config/herdr");
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

    #[test]
    fn an_old_process_identity_cannot_open_another_agents_terminal() {
        let result = open(Request::Agent {
            pid: std::process::id(),
            started_at: chrono::DateTime::UNIX_EPOCH,
            session: None,
        });
        assert!(result.unwrap_err().contains("ended"));
    }
}
