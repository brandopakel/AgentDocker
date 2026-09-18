//! Explicit terminal actions. Paths and terminal identities travel as arguments,
//! never as shell programs, and opening an agent never types into its prompt.
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::time::Duration;

#[derive(Clone, Debug)]
pub enum Request {
    Project(PathBuf),
    Agent {
        pid: u32,
        started_at: chrono::DateTime<chrono::Utc>,
    },
}

pub fn open(request: Request) -> Result<(), String> {
    match request {
        Request::Project(path) => open_project(&path),
        Request::Agent { pid, started_at } => {
            if agentdocker_host::procinfo::start_time(pid) != Some(started_at) {
                return Err("This agent has ended. Its old terminal was not opened.".into());
            }
            open_agent(pid, started_at)
        }
    }
}

#[cfg(target_os = "macos")]
fn run(root: &Path, args: Vec<String>) -> Result<String, String> {
    let output = agentdocker_host::command::run(root, &args, Duration::from_secs(8))
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

#[cfg(target_os = "macos")]
fn open_agent(pid: u32, started_at: chrono::DateTime<chrono::Utc>) -> Result<(), String> {
    let tty = run(
        Path::new("/"),
        vec![
            "/bin/ps".into(),
            "-p".into(),
            pid.to_string(),
            "-o".into(),
            "tty=".into(),
        ],
    )?;
    let tty = terminal_device(tty.trim())?;
    if agentdocker_host::procinfo::start_time(pid) != Some(started_at) {
        return Err("This agent has ended. Its old terminal was not opened.".into());
    }
    run(
        Path::new("/"),
        vec![
            "/usr/bin/osascript".into(),
            "-l".into(),
            "JavaScript".into(),
            "-e".into(),
            FOCUS_TERMINAL.into(),
            tty,
        ],
    )?;
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn open_agent(_pid: u32, _started_at: chrono::DateTime<chrono::Utc>) -> Result<(), String> {
    Err("This agent runs in an external terminal. Opening that existing terminal is not supported here yet; agents launched in AgentDocker have an in-app terminal.".into())
}

#[cfg(any(target_os = "macos", test))]
fn terminal_device(tty: &str) -> Result<String, String> {
    if tty.starts_with("ttys") && tty.len() > 4 && tty[4..].bytes().all(|b| b.is_ascii_digit()) {
        Ok(format!("/dev/{tty}"))
    } else {
        Err("This agent has no accessible Terminal window. It may run inside another app; use that app to continue.".into())
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
    fn unknown_or_injected_terminal_names_do_not_target_a_window() {
        assert_eq!(terminal_device("ttys003").unwrap(), "/dev/ttys003");
        for tty in [
            "??",
            "",
            "ttys",
            "ttys003\ncommand",
            "ttys003;exit",
            "/dev/ttys003",
        ] {
            assert!(terminal_device(tty).is_err());
        }
    }
    #[test]
    fn an_old_process_identity_cannot_open_another_agents_terminal() {
        let result = open(Request::Agent {
            pid: std::process::id(),
            started_at: chrono::DateTime::UNIX_EPOCH,
        });
        assert!(result.unwrap_err().contains("ended"));
    }
}
