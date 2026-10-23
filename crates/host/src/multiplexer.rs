//! Recognising an agent that lives inside somebody else's terminal.
//!
//! We do not write a multiplexer. `tmux` exists, `screen` exists, herdr
//! exists, and a multiplexer is not the working set. What is worth
//! having is the ability to *notice* one: an agent discovered running
//! inside a tmux pane or a herdr session is not homeless, it is
//! somebody's guest, and saying so lets a person attach with the tool
//! they already use instead of ours.
//!
//! A multiplexer tells its children who they are through the
//! environment — `TMUX_PANE`, `STY`, `ZELLIJ_SESSION_NAME` — which is
//! exact, names the pane, and is the signal worth having. The
//! conventions themselves live in `agentdocker_core::multiplexer`.
//!
//! Reading it is where the platforms differ, and the difference is worth
//! stating rather than papering over. On Linux `/proc/<pid>/environ` is
//! readable for our own user's processes, so the daemon can look for
//! itself. **On macOS it cannot**: measured on 26.5.1, `ps -E` returns
//! only the command line for a process other than the caller, even one
//! owned by the same user. (A privileged caller was not tested, and
//! nothing here depends on what one would see.) So the signal has to
//! arrive another way there, and it does — an agent that registers
//! itself is running *inside* the session, so the client reads its own
//! environment and sends what it saw.
//!
//! Where neither is available, process ancestry still shows a `tmux` or
//! `zellij` between the agent and its shell. That is weaker — it cannot
//! name the pane — and it is reported as such rather than dressed up.

use std::collections::BTreeMap;
use std::path::Path;

use agentdocker_core::multiplexer::{Evidence, Session, from_environment};

use crate::procinfo::Process;

/// The multiplexers we can recognise by the name of a process.
const BY_NAME: [(&str, &str); 4] = [
    ("tmux", "tmux"),
    ("zellij", "zellij"),
    ("herdr", "herdr"),
    // screen's server is `SCREEN`; the client is `screen`.
    ("screen", "screen"),
];

/// What the ancestry of a process says, when its environment says
/// nothing. Weaker evidence and reported as such: a multiplexer between
/// an agent and its shell is where the agent is, but it cannot say which
/// pane.
///
/// `by_pid` is the process table; walking stops at pid 1, at a missing
/// parent, or after enough steps that a cycle cannot spin.
pub fn from_ancestry(pid: u32, by_pid: &BTreeMap<u32, Process>) -> Option<Session> {
    let mut current = by_pid.get(&pid)?.ppid;
    for _ in 0..32 {
        if current <= 1 {
            return None;
        }
        let process = by_pid.get(&current)?;
        if let Some(kind) = multiplexer_name(&process.argv) {
            return Some(Session {
                kind: kind.to_owned(),
                session: None,
                pane: None,
                socket: None,
                evidence: Evidence::Ancestry,
            });
        }
        current = process.ppid;
    }
    None
}

/// The multiplexer a command line belongs to, by the executable's name.
fn multiplexer_name(argv: &[String]) -> Option<&'static str> {
    let program = argv.first()?;
    let file = Path::new(program).file_name()?.to_str()?;
    // A login shell is `-zsh`; a server may be `SCREEN`.
    let file = file.trim_start_matches('-').to_ascii_lowercase();
    BY_NAME
        .iter()
        .find(|(name, _)| file == *name)
        .map(|(_, kind)| *kind)
}

/// Where a process lives.
///
/// `reported` is what the client said about itself when it registered,
/// which is the only exact answer available on macOS. It is preferred
/// over ancestry and second to what the daemon can read for itself,
/// because a first-hand reading beats a second-hand one.
pub fn of(pid: u32, by_pid: &BTreeMap<u32, Process>, reported: Option<Session>) -> Option<Session> {
    environment(pid)
        .and_then(|env| from_environment(&env))
        .or(reported)
        .or_else(|| from_ancestry(pid, by_pid))
}

/// Where *this* process lives, read from its own environment. A client
/// registering itself is inside whatever session it is reporting, so
/// this is first-hand and works on every platform.
pub fn own() -> Option<Session> {
    from_environment(&std::env::vars().collect())
}

/// A process's environment, where the operating system will show it.
///
/// Only ever our own user's processes, which is all we look at: the
/// daemon inspects agents on the same machine, started by the same
/// person. A refusal is `None`, not an error, because ancestry is the
/// fallback and a missing answer is not a broken one.
pub fn environment(pid: u32) -> Option<BTreeMap<String, String>> {
    platform::environment(pid)
}

/// herdr's API socket for a session, where herdr puts it: under the
/// person's own home, `~/.config/herdr/herdr.sock` for the default
/// session and `~/.config/herdr/sessions/<name>/herdr.sock` for a named
/// one. `None` where there is no home, or for a name that is not a plain
/// session name (empty, hidden, or a path), which names no socket.
///
/// A server started on a socket of its own (`HERDR_SOCKET_PATH`) is not
/// followed: only the session's name is recorded with an agent.
pub fn herdr_socket(session: Option<&str>) -> Option<std::path::PathBuf> {
    let base = std::env::home_dir()?.join(".config").join("herdr");
    herdr_socket_in(&base, session)
}

/// [`herdr_socket`] under a given herdr directory.
pub fn herdr_socket_in(base: &Path, session: Option<&str>) -> Option<std::path::PathBuf> {
    match session {
        None => Some(base.join("herdr.sock")),
        Some(name)
            if !name.is_empty()
                && !name.starts_with('.')
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b)) =>
        {
            Some(base.join("sessions").join(name).join("herdr.sock"))
        }
        Some(_) => None,
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::*;

    pub fn environment(pid: u32) -> Option<BTreeMap<String, String>> {
        let raw = std::fs::read(format!("/proc/{pid}/environ")).ok()?;
        Some(parse_nul_separated(&raw))
    }

    /// `/proc/<pid>/environ` is NUL-separated `KEY=VALUE`.
    pub fn parse_nul_separated(raw: &[u8]) -> BTreeMap<String, String> {
        raw.split(|byte| *byte == 0)
            .filter(|entry| !entry.is_empty())
            .filter_map(|entry| {
                let text = String::from_utf8_lossy(entry);
                let (key, value) = text.split_once('=')?;
                Some((key.to_owned(), value.to_owned()))
            })
            .collect()
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;

    /// Self-inspection uses the process environment directly, preserving value
    /// boundaries. For peers, ps flattens argv and any visible environment into
    /// indistinguishable text; it cannot establish first-hand evidence. Use the
    /// registering client's reported environment or process ancestry instead.
    pub fn environment(pid: u32) -> Option<BTreeMap<String, String>> {
        if pid != std::process::id() {
            return None;
        }
        Some(
            std::env::vars_os()
                .filter_map(|(key, value)| {
                    Some((key.into_string().ok()?, value.into_string().ok()?))
                })
                .collect(),
        )
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod platform {
    use super::*;

    pub fn environment(_pid: u32) -> Option<BTreeMap<String, String>> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(pid: u32, ppid: u32, argv: &[&str]) -> Process {
        Process {
            pid,
            ppid,
            argv: argv.iter().map(|a| (*a).to_owned()).collect(),
        }
    }

    #[test]
    fn ancestry_finds_a_multiplexer_the_environment_did_not_mention() {
        let table: BTreeMap<u32, Process> = [
            process(1, 0, &["/sbin/launchd"]),
            process(10, 1, &["/opt/homebrew/bin/tmux", "new-session"]),
            process(20, 10, &["-zsh"]),
            process(30, 20, &["claude"]),
        ]
        .into_iter()
        .map(|p| (p.pid, p))
        .collect();
        let session = from_ancestry(30, &table).expect("tmux is two steps up");
        assert_eq!(session.kind, "tmux");
        assert_eq!(session.evidence, Evidence::Ancestry, "weaker, and said so");
        assert_eq!(session.describe(), "tmux (ancestry)");
    }

    /// herdr's sockets live under the person's home, not AgentDocker's,
    /// and only a plain session name reaches one.
    #[test]
    fn herdr_sockets_follow_its_session_names_and_refuse_paths() {
        let base = Path::new("/home/me/.config/herdr");
        assert_eq!(
            herdr_socket_in(base, None).as_deref(),
            Some(Path::new("/home/me/.config/herdr/herdr.sock"))
        );
        assert_eq!(
            herdr_socket_in(base, Some("backend")).as_deref(),
            Some(Path::new(
                "/home/me/.config/herdr/sessions/backend/herdr.sock"
            ))
        );
        for name in ["", "../x", "a/b", ".hidden", "a b", ".."] {
            assert_eq!(herdr_socket_in(base, Some(name)), None, "{name:?}");
        }
        if let Some(home) = std::env::home_dir() {
            assert_eq!(
                herdr_socket(None),
                Some(home.join(".config/herdr/herdr.sock"))
            );
        }
    }

    #[test]
    fn an_agent_started_from_a_plain_shell_is_in_no_session() {
        let table: BTreeMap<u32, Process> = [
            process(1, 0, &["/sbin/launchd"]),
            process(20, 1, &["-zsh"]),
            process(30, 20, &["claude"]),
        ]
        .into_iter()
        .map(|p| (p.pid, p))
        .collect();
        assert!(from_ancestry(30, &table).is_none());
    }

    #[test]
    fn a_cycle_in_the_process_table_terminates() {
        // A damaged table: two processes claiming each other as parent.
        let table: BTreeMap<u32, Process> = [process(30, 31, &["a"]), process(31, 30, &["b"])]
            .into_iter()
            .map(|p| (p.pid, p))
            .collect();
        assert!(from_ancestry(30, &table).is_none());
    }

    #[test]
    fn a_login_shell_dash_does_not_hide_the_name() {
        assert_eq!(
            multiplexer_name(&["-tmux".to_owned()]),
            Some("tmux"),
            "a leading dash is a login shell convention, not part of the name"
        );
        assert_eq!(
            multiplexer_name(&["/usr/bin/SCREEN".to_owned()]),
            Some("screen")
        );
        assert_eq!(multiplexer_name(&["/bin/zsh".to_owned()]), None);
        assert_eq!(multiplexer_name(&[]), None);
        // A path that merely contains the name is not the program.
        assert_eq!(multiplexer_name(&["/home/tmux/agent".to_owned()]), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn proc_environ_is_nul_separated() {
        let env = platform::parse_nul_separated(b"PATH=/usr/bin\0TMUX_PANE=%2\0\0");
        assert_eq!(env.get("TMUX_PANE").map(String::as_str), Some("%2"));
        assert_eq!(env.len(), 2);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn mac_environment_keeps_complete_self_values() {
        const MARKER: &str = "AGENTDOCKER_MULTIPLEXER_TEST_CHILD";
        const PATH: &str = "/usr/bin:/bin:/tmp/AgentDocker fixture path";
        if std::env::var_os(MARKER).is_some() {
            let actual = environment(std::process::id()).expect("own environment is available");
            assert_eq!(actual.get("PATH").map(String::as_str), Some(PATH));
            return;
        }
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "multiplexer::tests::mac_environment_keeps_complete_self_values",
                "--nocapture",
            ])
            .env(MARKER, "1")
            .env("PATH", PATH)
            .status()
            .unwrap();
        assert!(
            status.success(),
            "self environment must preserve spaces without changing the parent environment"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn mac_environment_does_not_treat_peer_arguments_as_environment() {
        use std::process::{Command, Stdio};
        // This owned shell has no TMUX_PANE environment variable. Its argv
        // deliberately looks like one in ps's ambiguous flattened output.
        let mut child = Command::new("/bin/sh")
            .args(["-c", "read -r ignored", "TMUX_PANE=%forged"])
            .env_clear()
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        let observed = environment(child.id());
        drop(child.stdin.take());
        let _ = child.wait().unwrap();
        assert!(
            observed.is_none(),
            "foreign command text must not become first-hand environment evidence"
        );
    }

    /// The real thing, on this machine: our own process must be readable,
    /// and what it says must agree with what we can see for ourselves.
    #[test]
    fn our_own_environment_is_readable_and_agrees_with_std_env() {
        let Some(env) = environment(std::process::id()) else {
            // A platform we do not read environments on; ancestry covers it.
            return;
        };
        assert!(!env.is_empty(), "our own environment is not empty");
        if let Ok(path) = std::env::var("PATH") {
            assert_eq!(
                env.get("PATH"),
                Some(&path),
                "read back the PATH we were started with"
            );
        }
    }
}

/// Placing an agent in a `tmux` pane, so the human can reach it with the
/// tool that already owns terminals.
///
/// This is the other half of recognising one. The daemon does not
/// supervise what it puts here — tmux owns the process, and that is the
/// point: we own the coordination, they own the terminal. So the agent
/// is *registered*, not run, and everything that follows from that is
/// true of it: no captured log (tmux has the output), and it ends when
/// its command ends.
pub mod tmux {
    use std::collections::BTreeMap;
    use std::io;
    use std::path::Path;
    use std::process::Command;

    /// A pane that now exists, and the process in it.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Pane {
        /// `%3`, which is what every other tmux command takes as a target.
        pub id: String,
        /// The session it landed in, by name.
        pub session: String,
        /// The process tmux started, which is the agent.
        pub pid: u32,
        /// The server's socket, so the pane is addressed there and nowhere
        /// else; `None` when tmux would not say.
        pub socket: Option<String>,
    }

    /// `new-session -e`, which is how the agent is told its own id,
    /// arrived in tmux 3.2. An older tmux rejects the flag, and the
    /// failure would surface as a started-then-failed agent rather than
    /// as the plain fact that this tmux is too old.
    pub const MINIMUM: (u32, u32) = (3, 2);

    /// Whether this machine's tmux can do what `--in-pane` needs. Asked
    /// before anything is created, so both "not installed" and "too old"
    /// are answers rather than failures.
    pub fn usable() -> Result<(), String> {
        let output = Command::new("tmux")
            .arg("-V")
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|_| {
                "tmux is not on this machine's PATH, so there is no pane to run in".to_owned()
            })?;
        if !output.status.success() {
            return Err("tmux is on the PATH but would not report its version".to_owned());
        }
        let reported = String::from_utf8_lossy(&output.stdout);
        let Some(version) = parse_version(&reported) else {
            return Err(format!(
                "could not read a version out of `tmux -V` ({}); \
                 --in-pane needs {}.{} or newer",
                reported.trim(),
                MINIMUM.0,
                MINIMUM.1
            ));
        };
        if version < MINIMUM {
            return Err(format!(
                "tmux {}.{} is too old for --in-pane: passing the agent its own id needs \
                 `new-session -e`, which arrived in tmux {}.{}",
                version.0, version.1, MINIMUM.0, MINIMUM.1
            ));
        }
        Ok(())
    }

    /// The major and minor out of `tmux -V`. Its output is `tmux 3.7c`,
    /// and on a development build `tmux next-3.4` — so the digits are
    /// found rather than assumed to start the word, and a trailing letter
    /// is not part of the minor.
    pub fn parse_version(reported: &str) -> Option<(u32, u32)> {
        let digits = reported.find(|c: char| c.is_ascii_digit())?;
        let rest = &reported[digits..];
        let mut parts = rest.split('.');
        let major = parts.next()?;
        let minor = parts.next().unwrap_or("0");
        let take_number = |text: &str| {
            let end = text
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(text.len());
            text[..end].parse::<u32>().ok()
        };
        Some((take_number(major)?, take_number(minor).unwrap_or(0)))
    }

    /// Whether tmux is on the PATH and new enough.
    pub fn available() -> bool {
        usable().is_ok()
    }

    /// Start `command` in a new detached session named `session`, in
    /// `workdir`, with `env` set for the process.
    ///
    /// Detached because the daemon is not a terminal: the human attaches
    /// afterwards with `tmux attach -t <session>`, which is the whole
    /// reason for doing this rather than running the agent ourselves.
    pub fn new_session(
        session: &str,
        workdir: &Path,
        env: &BTreeMap<String, String>,
        command: &[String],
    ) -> io::Result<Pane> {
        if command.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a pane needs a command to run",
            ));
        }
        let mut tmux = Command::new("tmux");
        tmux.args(["new-session", "-d", "-P", "-F", "#{pane_id}"])
            .args(["-s", session])
            .arg("-c")
            .arg(workdir);
        for (key, value) in env {
            tmux.arg("-e").arg(format!("{key}={value}"));
        }
        // `--` so an agent's own flags are never read as tmux's.
        tmux.arg("--").args(command);
        let output = tmux.output()?;
        if !output.status.success() {
            return Err(io::Error::other(format!(
                "tmux refused to create the session: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        let id = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if id.is_empty() {
            return Err(io::Error::other("tmux created a pane but did not name it"));
        }
        let pid = pane_pid(&id)?;
        Ok(Pane {
            session: describe_pane(&id, "#{session_name}").unwrap_or_else(|_| session.to_owned()),
            socket: describe_pane(&id, "#{socket_path}")
                .ok()
                .filter(|socket| socket.starts_with('/')),
            id,
            pid,
        })
    }

    /// Remove only the pane this launch created, never a reused session name.
    pub fn remove_owned_pane(pane: &Pane) -> io::Result<()> {
        if pane_pid(&pane.id)? != pane.pid
            || describe_pane(&pane.id, "#{session_name}")? != pane.session
        {
            return Err(io::Error::other("tmux pane identity changed"));
        }
        let output = Command::new("tmux")
            .args(["kill-pane", "-t", &pane.id])
            .output()?;
        if !output.status.success() {
            return Err(io::Error::other(format!(
                "tmux could not remove owned pane: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Ok(())
    }

    /// The process tmux started in a pane. This is the agent's pid: what
    /// `ps` will show, what liveness checks, and what `stop` signals.
    pub fn pane_pid(pane: &str) -> io::Result<u32> {
        describe_pane(pane, "#{pane_pid}")?
            .parse()
            .map_err(|_| io::Error::other("tmux reported a pane pid that is not a number"))
    }

    /// The most a capture may take, unless the caller says otherwise.
    pub const CAPTURE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(2);

    /// What a pane shows now, as plain text — its visible screen, one line
    /// per row, without escapes (`capture-pane -p` without `-e`) — read
    /// only when the pane provably holds the agent it is read for.
    ///
    /// The pane is addressed at `socket` (`tmux -S`), the server the agent
    /// was told it lives in, never at whichever server this process would
    /// select: pane ids are per server, and two servers each have a `%0`.
    /// Its process (`#{pane_pid}`) must satisfy `holds` — the caller's
    /// proof that the agent is that process or descends from it — both
    /// before the capture and after it, unchanged, so a pane replaced or a
    /// server restarted in between is refused rather than read. Every tmux
    /// run is `program` (`tmux`, or a stand-in in tests) as a child of its
    /// own with a deadline of `deadline`, killed and reaped past it, its
    /// output bounded.
    pub fn capture_bound(
        program: &Path,
        socket: &Path,
        pane: &str,
        holds: impl Fn(u32) -> bool,
        deadline: std::time::Duration,
    ) -> io::Result<String> {
        let number = pane.strip_prefix('%').unwrap_or_default();
        if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a pane is named by its id, `%` and a number",
            ));
        }
        if !socket.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a tmux server is named by its socket's absolute path",
            ));
        }
        let run = |args: &[&str]| -> io::Result<String> {
            let mut argv = vec![
                program.display().to_string(),
                "-S".to_owned(),
                socket.display().to_string(),
            ];
            argv.extend(args.iter().map(|arg| (*arg).to_owned()));
            let output = crate::command::run(Path::new("/"), &argv, deadline)?;
            if !output.success {
                return Err(io::Error::other(format!(
                    "tmux could not reach pane {pane} at its server"
                )));
            }
            Ok(output.stdout)
        };
        let pid = |text: String| -> io::Result<u32> {
            text.trim()
                .parse()
                .map_err(|_| io::Error::other("tmux reported a pane pid that is not a number"))
        };
        let before = pid(run(&["display-message", "-p", "-t", pane, "#{pane_pid}"])?)?;
        if !holds(before) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the pane does not hold the agent's process",
            ));
        }
        let text = run(&["capture-pane", "-p", "-t", pane])?;
        let after = pid(run(&["display-message", "-p", "-t", pane, "#{pane_pid}"])?)?;
        if after != before {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the pane changed hands while it was read",
            ));
        }
        Ok(text)
    }

    /// Ask tmux one thing about one pane.
    fn describe_pane(pane: &str, format: &str) -> io::Result<String> {
        let output = Command::new("tmux")
            .args(["display-message", "-p", "-t", pane, format])
            .output()?;
        if !output.status.success() {
            return Err(io::Error::other(format!(
                "tmux could not describe pane {pane}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }

    /// A session name tmux will accept: it uses `.` and `:` in target
    /// syntax, so neither can appear in a name we then use as a target.
    pub fn session_name(agent: &str) -> String {
        let cleaned: String = agent
            .chars()
            .map(|c| if c == '.' || c == ':' { '-' } else { c })
            .collect();
        let cleaned = cleaned.trim_matches('-');
        if cleaned.is_empty() {
            "agent".to_owned()
        } else {
            cleaned.to_owned()
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn a_session_name_avoids_tmuxs_own_target_syntax() {
            assert_eq!(session_name("claude-main"), "claude-main");
            // `.` and `:` separate session, window and pane in a target,
            // so a name containing them cannot be used as one.
            assert_eq!(session_name("claude.main:1"), "claude-main-1");
            assert_eq!(session_name("..."), "agent");
            assert_eq!(session_name(""), "agent");
        }

        #[test]
        fn versions_are_read_out_of_what_tmux_actually_prints() {
            assert_eq!(parse_version("tmux 3.7c\n"), Some((3, 7)));
            assert_eq!(parse_version("tmux 2.8"), Some((2, 8)));
            assert_eq!(parse_version("tmux 3.2a"), Some((3, 2)));
            // A development build names itself before the number.
            assert_eq!(parse_version("tmux next-3.4"), Some((3, 4)));
            assert_eq!(parse_version("tmux 3"), Some((3, 0)));
            assert_eq!(parse_version("tmux master"), None);
            assert_eq!(parse_version(""), None);
            // And the comparison the gate makes is the ordinary one.
            assert!((3, 2) >= MINIMUM);
            assert!((3, 7) >= MINIMUM);
            assert!((3, 1) < MINIMUM);
            assert!((2, 9) < MINIMUM);
        }

        #[test]
        fn this_machines_tmux_is_reported_consistently() {
            // Whatever is installed here, the two answers must agree.
            assert_eq!(usable().is_ok(), available());
        }

        #[test]
        fn a_pane_needs_a_command() {
            let err = new_session("x", Path::new("/tmp"), &BTreeMap::new(), &[]).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        }

        /// Only a pane id at an absolute socket is captured: a target that
        /// names a session, a window or anything argument-shaped, or a
        /// server named any other way, is refused before tmux runs.
        #[test]
        fn only_a_pane_id_at_a_named_server_is_captured() {
            let never = |_| panic!("nothing is asked of tmux");
            let tmux = Path::new("/nonexistent/tmux");
            let socket = Path::new("/tmp/tmux-test/default");
            for target in ["", "%", "main", "main:1.0", "%3x", "-t", "%1 %2"] {
                let err = capture_bound(tmux, socket, target, never, CAPTURE_DEADLINE).unwrap_err();
                assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{target}");
            }
            let err = capture_bound(tmux, Path::new("default"), "%0", never, CAPTURE_DEADLINE)
                .unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        }

        /// A tmux that never answers is killed at the deadline and its
        /// process reaped, and a pane whose process is not the agent's is
        /// refused before anything is captured.
        #[cfg(unix)]
        #[test]
        fn a_capture_is_bounded_and_bound_to_the_agents_process() {
            use std::os::unix::fs::PermissionsExt;
            let dir = tempfile::tempdir().unwrap();
            let stand_in = |name: &str, body: &str| {
                let path = dir.path().join(name);
                std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
                path
            };
            let socket = dir.path().join("server");
            let marker = dir.path().join("hung");
            let hanging = stand_in(
                "hanging",
                &format!("echo $$ > {}; exec sleep 30", marker.display()),
            );
            let started = std::time::Instant::now();
            let err = capture_bound(
                &hanging,
                &socket,
                "%0",
                |_| true,
                std::time::Duration::from_millis(300),
            )
            .unwrap_err();
            assert!(err.to_string().contains("timed out"), "{err}");
            assert!(started.elapsed() < std::time::Duration::from_secs(5));
            let hung: u32 = std::fs::read_to_string(&marker)
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            assert!(!crate::procinfo::alive(hung), "killed and reaped");

            // A stand-in that says the pane holds process 4242, and records
            // whether a capture was asked for.
            let captured = dir.path().join("captured");
            let answering = stand_in(
                "answering",
                &format!(
                    "case \"$3\" in display-message) echo 4242;; capture-pane) touch {}; echo screen;; esac",
                    captured.display()
                ),
            );
            let err = capture_bound(&answering, &socket, "%0", |pid| pid == 1, CAPTURE_DEADLINE)
                .unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
            assert!(!captured.exists(), "nothing was captured");
            let text = capture_bound(
                &answering,
                &socket,
                "%0",
                |pid| pid == 4242,
                CAPTURE_DEADLINE,
            )
            .unwrap();
            assert_eq!(text.trim(), "screen");
        }
    }
}
