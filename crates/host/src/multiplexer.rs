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
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::time::Duration;

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

    /// A tmux server by generation: its pid and the time it started, as tmux
    /// reports them (`#{pid}`, `#{start_time}`). A server started later at
    /// the same socket is another generation even with the same pid, and
    /// pane ids are unique only within one generation.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Server {
        pub pid: u32,
        pub started: u64,
    }

    /// The pane a launch made, read from the one line `new-session -P`
    /// printed ([`LAUNCH_FORMAT`]): the evidence a later removal is checked
    /// against, at the server it names.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Launched {
        /// `%<digits>`.
        pub pane: String,
        /// The pane's first process, which is the agent.
        pub pid: u32,
        pub server: Server,
        pub session: String,
        /// The server's socket, absolute.
        pub socket: PathBuf,
    }

    impl Launched {
        /// The pane as an agent's record keeps it.
        pub fn pane(&self) -> Pane {
            Pane {
                id: self.pane.clone(),
                session: self.session.clone(),
                pid: self.pid,
                socket: Some(self.socket.display().to_string()),
            }
        }
    }

    /// What `new-session -P` prints: the pane, its process, the server's
    /// generation, the session and the socket, tab-separated on one line.
    pub const LAUNCH_FORMAT: &str =
        "#{pane_id}\t#{pane_pid}\t#{pid}\t#{start_time}\t#{session_name}\t#{socket_path}";

    /// The most a launch's tmux client may take before its answer is taken
    /// as lost.
    pub const LAUNCH_DEADLINE: Duration = Duration::from_secs(30);

    /// The most one removal may take.
    pub const REMOVAL_DEADLINE: Duration = Duration::from_secs(5);

    /// How long a removed pane's first process is given to end.
    pub const LEADER_GRACE: Duration = Duration::from_secs(2);

    /// Whether `text` is a pane id: `%` and digits.
    fn is_pane_id(text: &str) -> bool {
        text.strip_prefix('%')
            .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
    }

    /// A positive number of digits alone.
    fn digits<T: std::str::FromStr + PartialEq + Default>(text: &str) -> Option<T> {
        if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        text.parse().ok().filter(|value| *value != T::default())
    }

    /// The launch line, read strictly: exactly one line of the six fields,
    /// or nothing. Hook output, several lines, a missing or malformed field
    /// or a relative socket are not a pane, and never a target.
    pub fn parse_launched(stdout: &str) -> Option<Launched> {
        let line = stdout.strip_suffix('\n').unwrap_or(stdout);
        if line.contains(['\n', '\r', '\0']) {
            return None;
        }
        let mut fields = line.splitn(6, '\t');
        let pane = fields.next().filter(|pane| is_pane_id(pane))?;
        let pid = digits(fields.next()?)?;
        let server = Server {
            pid: digits(fields.next()?)?,
            started: digits(fields.next()?)?,
        };
        let session = fields.next().filter(|session| !session.is_empty())?;
        let socket = fields.next().filter(|socket| socket.starts_with('/'))?;
        Some(Launched {
            pane: pane.to_owned(),
            pid,
            server,
            session: session.to_owned(),
            socket: PathBuf::from(socket),
        })
    }

    /// How a launch ended: nothing reached a server, the pane was made and
    /// named, or a client ran and what it did is not known.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum Launch {
        /// No launch request reached a tmux server: the command was empty,
        /// or tmux could not be started.
        NotStarted { why: String },
        /// The pane was made, and tmux named it.
        Started(Launched),
        /// A tmux client ran, so the server may have made the session and
        /// started its command, whatever the client said. `pane` is the
        /// pane when a valid launch line survived; `output` is what the
        /// client printed when it was not one, bounded.
        Indeterminate {
            pane: Option<Launched>,
            output: Option<String>,
            why: String,
        },
    }

    /// The most of a client's unreadable output kept for the message.
    const SHOWN_OUTPUT: usize = 512;

    fn line_or_output(stdout: &str) -> (Option<Launched>, Option<String>) {
        match parse_launched(stdout) {
            Some(launched) => (Some(launched), None),
            None => {
                let shown: String = stdout.trim().chars().take(SHOWN_OUTPUT).collect();
                (None, (!shown.is_empty()).then_some(shown))
            }
        }
    }

    /// A launch's outcome from what became of its tmux client. Only a
    /// client never started is [`Launch::NotStarted`]; a lost wait or read
    /// is [`Launch::Indeterminate`], keeping a valid line it had printed.
    pub fn classify(observed: crate::command::Observed) -> Launch {
        use crate::command::Observed;
        match observed {
            Observed::NotRun(error) => Launch::NotStarted {
                why: format!("tmux could not be run: {error}"),
            },
            Observed::Lost { error, stdout } => {
                let (pane, output) = line_or_output(&stdout);
                Launch::Indeterminate {
                    pane,
                    output,
                    why: format!("tmux was started and its answer was lost ({error})"),
                }
            }
            Observed::Exited(out) => {
                let said = out.text.get(out.stdout.len()..).unwrap_or_default().trim();
                let (pane, output) = line_or_output(&out.stdout);
                match (out.success, pane) {
                    (true, Some(launched)) => Launch::Started(launched),
                    (true, None) => Launch::Indeterminate {
                        pane: None,
                        output,
                        why: "tmux reported success but did not name the pane it made".to_owned(),
                    },
                    (false, Some(launched)) => Launch::Indeterminate {
                        why: format!(
                            "tmux made pane {} but then reported a failure: {said}",
                            launched.pane
                        ),
                        pane: Some(launched),
                        output: None,
                    },
                    (false, None) => Launch::Indeterminate {
                        pane: None,
                        output,
                        why: format!("tmux did not confirm the session: {said}"),
                    },
                }
            }
        }
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
    ) -> Launch {
        launch_with(
            Path::new("tmux"),
            None,
            session,
            workdir,
            env,
            command,
            LAUNCH_DEADLINE,
        )
    }

    /// [`new_session`] with `program` for tmux and, when `server` is given,
    /// that named server (`-L`) instead of the default one.
    pub fn launch_with(
        program: &Path,
        server: Option<&str>,
        session: &str,
        workdir: &Path,
        env: &BTreeMap<String, String>,
        command: &[String],
        deadline: Duration,
    ) -> Launch {
        if command.is_empty() {
            return Launch::NotStarted {
                why: "a pane needs a command to run".to_owned(),
            };
        }
        let Some(workdir) = workdir.to_str() else {
            return Launch::NotStarted {
                why: "the working directory is not UTF-8, and tmux is handed it as text".to_owned(),
            };
        };
        let mut argv = vec![program.display().to_string()];
        if let Some(server) = server {
            argv.extend(["-L".to_owned(), server.to_owned()]);
        }
        argv.extend(
            [
                "new-session",
                "-d",
                "-P",
                "-F",
                LAUNCH_FORMAT,
                "-s",
                session,
                "-c",
                workdir,
            ]
            .map(str::to_owned),
        );
        for (key, value) in env {
            argv.extend(["-e".to_owned(), format!("{key}={value}")]);
        }
        // `--` so an agent's own flags are never read as tmux's.
        argv.push("--".to_owned());
        argv.extend(command.iter().cloned());
        classify(crate::command::run_observed(
            Path::new("/"),
            &argv,
            deadline,
        ))
    }

    /// What one removal of a launched pane found, at the server the launch
    /// named. Only [`Removal::Removed`] says this call removed it, and none
    /// says anything of processes that outlive the pane.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum Removal {
        /// The launch's server answered, the pane held the launch's process,
        /// and it was killed, checked and killed in one server command.
        Removed,
        /// The launch's server answered and has no such pane: pane ids are
        /// never reused within a generation.
        Gone,
        /// The launch's server was not reached: no server answers at its
        /// socket (`now: None`) or another generation does. Nothing was
        /// killed, and this says nothing of the original pane, which may
        /// still exist where its server is.
        NotReached { now: Option<Server> },
        /// The pane id is there with another process: not this launch's.
        Mismatch { pid: u32 },
        /// Not known: tmux could not be run, gave no answer in time (and may
        /// have removed the pane), or answered in a form not read.
        Unknown { why: String },
    }

    /// Remove the pane `launched` names, only while it is that launch's.
    pub fn remove_launched(launched: &Launched) -> Removal {
        remove_launched_with(Path::new("tmux"), launched, REMOVAL_DEADLINE)
    }

    /// [`remove_launched`] with `program` for tmux. One invocation at the
    /// launch's socket: the server's generation, its panes, then an
    /// `if-shell -F` whose condition is the whole identity (generation,
    /// pane id, pane process) and whose branch kills the pane. tmux runs
    /// one client's command list in one queue, and neither branch waits, so
    /// nothing changes between the check and the kill. The pane id is in the
    /// condition because `if-shell -t` does not fail on a missing pane.
    pub fn remove_launched_with(
        program: &Path,
        launched: &Launched,
        deadline: Duration,
    ) -> Removal {
        if !is_pane_id(&launched.pane) || !launched.socket.is_absolute() {
            return Removal::Unknown {
                why: "the launch named no pane at an absolute socket".to_owned(),
            };
        }
        let pane = &launched.pane;
        let expected = format!(
            "{}/{}/{}/{}",
            launched.server.pid, launched.server.started, pane, launched.pid
        );
        let argv: Vec<String> = vec![
            program.display().to_string(),
            "-S".to_owned(),
            launched.socket.display().to_string(),
            "display-message".to_owned(),
            "-p".to_owned(),
            "server #{pid} #{start_time}".to_owned(),
            ";".to_owned(),
            "list-panes".to_owned(),
            "-a".to_owned(),
            "-F".to_owned(),
            "pane #{pane_id} #{pane_pid}".to_owned(),
            ";".to_owned(),
            "if-shell".to_owned(),
            "-F".to_owned(),
            "-t".to_owned(),
            pane.clone(),
            format!("#{{==:#{{pid}}/#{{start_time}}/#{{pane_id}}/#{{pane_pid}},{expected}}}"),
            format!("kill-pane -t {pane} ; display-message -p removed"),
            "display-message -p kept".to_owned(),
        ];
        read_removal(
            launched,
            crate::command::run_observed(Path::new("/"), &argv, deadline),
        )
    }

    /// A removal's answer, read strictly: exit status, the server line, the
    /// pane lines and the branch taken, and nothing else.
    pub fn read_removal(launched: &Launched, observed: crate::command::Observed) -> Removal {
        use crate::command::Observed;
        let unknown = |why: String| Removal::Unknown { why };
        let out = match observed {
            Observed::NotRun(error) => return unknown(format!("tmux could not be run: {error}")),
            Observed::Lost { error, .. } => {
                return unknown(format!(
                    "tmux gave no answer ({error}), and may have removed the pane"
                ));
            }
            Observed::Exited(out) => out,
        };
        let said = out.text.get(out.stdout.len()..).unwrap_or_default().trim();
        if !out.success {
            if said.starts_with("no server running on") || said.starts_with("error connecting to") {
                return Removal::NotReached { now: None };
            }
            return unknown(format!("tmux refused the removal: {said}"));
        }
        let mut lines = out.stdout.lines();
        let server = lines.next().and_then(|line| {
            let mut words = line.strip_prefix("server ")?.split(' ');
            let server = Server {
                pid: digits(words.next()?)?,
                started: digits(words.next()?)?,
            };
            words.next().is_none().then_some(server)
        });
        let Some(server) = server else {
            return unknown("tmux did not name its server".to_owned());
        };
        let mut listed = None;
        let mut branch = None;
        for line in lines {
            if let Some(rest) = line.strip_prefix("pane ") {
                let Some((id, pid)) = rest.split_once(' ') else {
                    return unknown(format!("tmux listed a pane as {line:?}"));
                };
                let Some(pid) = digits::<u32>(pid).filter(|_| is_pane_id(id)) else {
                    return unknown(format!("tmux listed a pane as {line:?}"));
                };
                if branch.is_some() {
                    return unknown("tmux listed a pane after its answer".to_owned());
                }
                if id == launched.pane {
                    listed = Some(pid);
                }
            } else if (line == "removed" || line == "kept") && branch.is_none() {
                branch = Some(line == "removed");
            } else {
                return unknown(format!("tmux answered {line:?}"));
            }
        }
        if server != launched.server {
            return match branch {
                Some(false) => Removal::NotReached { now: Some(server) },
                _ => unknown("another server answered and the answer is not one read".to_owned()),
            };
        }
        match (branch, listed) {
            (Some(true), Some(pid)) if pid == launched.pid => Removal::Removed,
            (Some(false), None) => Removal::Gone,
            (Some(false), Some(pid)) if pid != launched.pid => Removal::Mismatch { pid },
            _ => unknown("tmux's answer does not agree with itself".to_owned()),
        }
    }

    /// Whether a launched pane's first process has ended.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum Leader {
        Gone,
        Live,
        Unknown { why: String },
    }

    /// The first process of a launched pane, by its pid and the birth read
    /// at launch, given up to `grace` to end. A birth not read at launch
    /// leaves it unknown: tmux's server start time is not its birth.
    #[cfg(unix)]
    pub fn leader(
        pid: u32,
        born: Option<chrono::DateTime<chrono::Utc>>,
        grace: Duration,
    ) -> Leader {
        use crate::presence::Liveness;
        let Some(born) = born else {
            return Leader::Unknown {
                why: "its start time was not read at launch".to_owned(),
            };
        };
        let until = std::time::Instant::now() + grace;
        loop {
            match crate::presence::liveness(pid, Some(born)) {
                Liveness::Gone => return Leader::Gone,
                Liveness::Unknown { why } => return Leader::Unknown { why },
                Liveness::Live if std::time::Instant::now() >= until => return Leader::Live,
                Liveness::Live => std::thread::sleep(Duration::from_millis(20)),
            }
        }
    }

    /// No process identity is read here.
    #[cfg(not(unix))]
    pub fn leader(
        _pid: u32,
        _born: Option<chrono::DateTime<chrono::Utc>>,
        _grace: Duration,
    ) -> Leader {
        Leader::Unknown {
            why: "process identity is not read on this platform".to_owned(),
        }
    }

    /// Single-quoted for a POSIX shell, for a command a message shows.
    pub fn shell_quoted(text: &str) -> String {
        format!("'{}'", text.replace('\'', "'\\''"))
    }

    /// The most a capture may take, unless the caller says otherwise.
    pub const CAPTURE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(2);

    /// Why a pane was not captured.
    #[derive(Debug)]
    pub enum Uncaptured {
        /// tmux gave no answer: it could not be run, or ran past its
        /// deadline and was killed, or answered past its output's bound.
        /// This says nothing of the pane, only of the server or of tmux.
        Unanswered(io::Error),
        /// The pane is not the agent's to read: not named as a pane id at
        /// an absolute socket, no such pane or no server on that socket
        /// (tmux said so), a pane whose process is not the agent's, or one
        /// that changed hands while it was read.
        Refused(io::Error),
    }

    impl Uncaptured {
        pub fn error(&self) -> &io::Error {
            match self {
                Self::Unanswered(error) | Self::Refused(error) => error,
            }
        }
    }

    impl std::fmt::Display for Uncaptured {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            self.error().fmt(f)
        }
    }

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
    /// output bounded. A tmux that gave no answer is told apart from one
    /// that answered that the pane is not to be read ([`Uncaptured`]).
    pub fn capture_bound(
        program: &Path,
        socket: &Path,
        pane: &str,
        holds: impl Fn(u32) -> bool,
        deadline: std::time::Duration,
    ) -> Result<String, Uncaptured> {
        let refused = |kind, words: &str| Uncaptured::Refused(io::Error::new(kind, words));
        let number = pane.strip_prefix('%').unwrap_or_default();
        if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
            return Err(refused(
                io::ErrorKind::InvalidInput,
                "a pane is named by its id, `%` and a number",
            ));
        }
        if !socket.is_absolute() {
            return Err(refused(
                io::ErrorKind::InvalidInput,
                "a tmux server is named by its socket's absolute path",
            ));
        }
        let run = |args: &[&str]| -> Result<String, Uncaptured> {
            let mut argv = vec![
                program.display().to_string(),
                "-S".to_owned(),
                socket.display().to_string(),
            ];
            argv.extend(args.iter().map(|arg| (*arg).to_owned()));
            let output = crate::command::run(Path::new("/"), &argv, deadline)
                .map_err(Uncaptured::Unanswered)?;
            if !output.success {
                return Err(Uncaptured::Refused(io::Error::other(format!(
                    "tmux could not reach pane {pane} at its server"
                ))));
            }
            Ok(output.stdout)
        };
        let pid = |text: String| -> Result<u32, Uncaptured> {
            text.trim().parse().map_err(|_| {
                Uncaptured::Refused(io::Error::other(
                    "tmux reported a pane pid that is not a number",
                ))
            })
        };
        let before = pid(run(&["display-message", "-p", "-t", pane, "#{pane_pid}"])?)?;
        if !holds(before) {
            return Err(refused(
                io::ErrorKind::PermissionDenied,
                "the pane does not hold the agent's process",
            ));
        }
        let text = run(&["capture-pane", "-p", "-t", pane])?;
        let after = pid(run(&["display-message", "-p", "-t", pane, "#{pane_pid}"])?)?;
        if after != before {
            return Err(refused(
                io::ErrorKind::PermissionDenied,
                "the pane changed hands while it was read",
            ));
        }
        Ok(text)
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

        use crate::command::{Observed, Output};

        const LINE: &str = "%12\t4321\t987\t1700000000\tprobe\t/tmp/tmux-1/default\n";

        fn launched() -> Launched {
            parse_launched(LINE).unwrap()
        }

        fn exited(success: bool, stdout: &str, stderr: &str) -> Observed {
            Observed::Exited(Output {
                success,
                stdout: stdout.to_owned(),
                text: format!("{stdout}{stderr}"),
            })
        }

        fn lost(stdout: &str) -> Observed {
            Observed::Lost {
                error: io::Error::other("command timed out after 30s"),
                stdout: stdout.to_owned(),
            }
        }

        /// The launch line is read whole or not at all.
        #[test]
        fn a_launch_line_is_read_strictly() {
            assert_eq!(
                launched(),
                Launched {
                    pane: "%12".into(),
                    pid: 4321,
                    server: Server {
                        pid: 987,
                        started: 1_700_000_000
                    },
                    session: "probe".into(),
                    socket: "/tmp/tmux-1/default".into(),
                }
            );
            for bad in [
                "",
                "%12",
                "12\t4321\t987\t1700000000\tprobe\t/tmp/s",
                "%1x\t4321\t987\t1700000000\tprobe\t/tmp/s",
                "%12\t\t987\t1700000000\tprobe\t/tmp/s",
                "%12\t0\t987\t1700000000\tprobe\t/tmp/s",
                "%12\t4321\t987\t\tprobe\t/tmp/s",
                "%12\t4321\t987\t1700000000\t\t/tmp/s",
                "%12\t4321\t987\t1700000000\tprobe\trelative/s",
                "%12\t4321\t987\t1700000000\tprobe\t/tmp/s\n%13\t1\t987\t1\tx\t/tmp/s",
                "hook said hello\n%12\t4321\t987\t1700000000\tprobe\t/tmp/s",
            ] {
                assert_eq!(parse_launched(bad), None, "{bad:?}");
            }
        }

        /// Only a client never started is not started; anything after a
        /// client ran is indeterminate, keeping a valid line it printed.
        #[test]
        fn a_launch_is_classified_by_what_its_client_did() {
            let not_run = classify(Observed::NotRun(io::Error::from(io::ErrorKind::NotFound)));
            assert!(matches!(not_run, Launch::NotStarted { .. }), "{not_run:?}");
            assert_eq!(
                classify(exited(true, LINE, "")),
                Launch::Started(launched())
            );
            // Success with no line, or with a line not read.
            for stdout in ["", "\n", "%12\n", "hello from a hook\n"] {
                let launch = classify(exited(true, stdout, ""));
                assert!(
                    matches!(&launch, Launch::Indeterminate { pane: None, .. }),
                    "{stdout:?} {launch:?}"
                );
            }
            let Launch::Indeterminate { output, .. } = classify(exited(true, "hello\n", "")) else {
                panic!()
            };
            assert_eq!(output.as_deref(), Some("hello"));
            // A failure after a valid line: the pane exists.
            let hooked = classify(exited(false, LINE, "'exit 1' returned 1"));
            let Launch::Indeterminate { pane, why, .. } = &hooked else {
                panic!("{hooked:?}")
            };
            assert_eq!(pane.as_ref(), Some(&launched()));
            assert!(
                why.contains("%12") && why.contains("'exit 1' returned 1"),
                "{why}"
            );
            // A failure with nothing printed proves nothing of the server.
            let said_no = classify(exited(false, "", "duplicate session: probe"));
            assert!(
                matches!(&said_no, Launch::Indeterminate { pane: None, why, .. } if why.contains("duplicate session")),
                "{said_no:?}"
            );
            // A lost answer keeps a valid line that arrived, or none.
            assert!(matches!(
                classify(lost(LINE)),
                Launch::Indeterminate { pane: Some(_), .. }
            ));
            assert!(matches!(
                classify(lost("")),
                Launch::Indeterminate { pane: None, .. }
            ));
            // An empty command never reaches tmux.
            let empty = launch_with(
                Path::new("/nonexistent/tmux"),
                None,
                "s",
                Path::new("/"),
                &BTreeMap::new(),
                &[],
                LAUNCH_DEADLINE,
            );
            assert!(matches!(empty, Launch::NotStarted { .. }), "{empty:?}");
            let missing = launch_with(
                Path::new("/nonexistent/tmux"),
                None,
                "s",
                Path::new("/"),
                &BTreeMap::new(),
                &["true".to_owned()],
                LAUNCH_DEADLINE,
            );
            assert!(matches!(missing, Launch::NotStarted { .. }), "{missing:?}");
        }

        /// Through the real runner: a client that printed a valid launch
        /// line and then more than the output bound on stderr keeps its
        /// line, so the pane is still known.
        #[cfg(unix)]
        #[test]
        fn a_line_survives_an_overflowing_client() {
            // printf reads `\t` and `\n` in its format; `%` is doubled.
            let format = LINE
                .replace('%', "%%")
                .replace('\t', "\\t")
                .replace('\n', "\\n");
            let script = format!("printf '{format}'; head -c 5000000 /dev/zero >&2");
            let argv = ["/bin/sh".to_owned(), "-c".to_owned(), script];
            let observed = crate::command::run_observed(Path::new("/"), &argv, LAUNCH_DEADLINE);
            assert!(matches!(observed, Observed::Lost { .. }), "{observed:?}");
            assert_eq!(
                classify(observed),
                Launch::Indeterminate {
                    pane: Some(launched()),
                    output: None,
                    why: "tmux was started and its answer was lost (command output exceeded 4 MiB)"
                        .to_owned(),
                }
            );
        }

        /// A removal's answer, read strictly from each form it can take.
        #[test]
        fn a_removal_is_read_from_the_whole_answer() {
            let us = launched();
            let read = |observed| read_removal(&us, observed);
            let server = "server 987 1700000000\n";
            assert_eq!(
                read(exited(
                    true,
                    &format!("{server}pane %3 11\npane %12 4321\nremoved\n"),
                    ""
                )),
                Removal::Removed
            );
            assert_eq!(
                read(exited(true, &format!("{server}pane %3 11\nkept\n"), "")),
                Removal::Gone
            );
            assert_eq!(
                read(exited(true, &format!("{server}pane %12 55\nkept\n"), "")),
                Removal::Mismatch { pid: 55 }
            );
            // Another generation at the socket: not reached, never gone.
            assert_eq!(
                read(exited(
                    true,
                    "server 990 1700000500\npane %12 4321\nkept\n",
                    ""
                )),
                Removal::NotReached {
                    now: Some(Server {
                        pid: 990,
                        started: 1_700_000_500
                    })
                }
            );
            for said in [
                "no server running on /tmp/tmux-1/default",
                "error connecting to /tmp/tmux-1/default (No such file or directory)",
            ] {
                assert_eq!(
                    read(exited(false, "", said)),
                    Removal::NotReached { now: None }
                );
            }
            // Everything else is unknown, a timeout above all: it may have
            // sent the kill.
            for observed in [
                lost(&format!("{server}pane %12 4321\n")),
                Observed::NotRun(io::Error::from(io::ErrorKind::NotFound)),
                exited(false, "", "unknown command: if-shell"),
                exited(true, "", ""),
                exited(true, server, ""),
                exited(true, &format!("{server}pane %12 4321\nkept\n"), ""),
                exited(true, &format!("{server}removed\n"), ""),
                exited(
                    true,
                    &format!("{server}pane %12 4321\nremoved\nremoved\n"),
                    "",
                ),
                exited(true, &format!("{server}pane %12 x\nkept\n"), ""),
                exited(true, &format!("{server}hello\nkept\n"), ""),
                exited(true, "server 990 1700000500\nremoved\n", ""),
            ] {
                let removal = read(observed);
                assert!(matches!(removal, Removal::Unknown { .. }), "{removal:?}");
            }
        }

        #[test]
        fn a_shown_command_is_single_quoted() {
            assert_eq!(shell_quoted("/tmp/a b"), "'/tmp/a b'");
            assert_eq!(shell_quoted("it's"), "'it'\\''s'");
        }

        /// A private tmux server, killed and its socket removed on drop.
        struct Private {
            name: String,
            /// The socket, from the first launch: the server may be gone
            /// by the time it is cleaned up.
            socket: std::cell::RefCell<Option<PathBuf>>,
        }

        impl Private {
            fn new(tag: &str) -> Option<Self> {
                available().then(|| Private {
                    name: format!("agentdocker-test-{tag}-{}", std::process::id()),
                    socket: std::cell::RefCell::new(None),
                })
            }
            fn tmux(&self, args: &[&str]) -> std::process::Output {
                Command::new("tmux")
                    .args(["-L", &self.name])
                    .args(args)
                    .output()
                    .unwrap()
            }
            /// End `server` and wait until its process is gone: a client
            /// started while it is still exiting can reach it and be told
            /// "server exited unexpectedly" instead of finding none.
            fn end(&self, server: Server) {
                let _ = self.tmux(&["kill-server"]);
                let until = std::time::Instant::now() + Duration::from_secs(10);
                while crate::procinfo::alive(server.pid) {
                    assert!(
                        std::time::Instant::now() < until,
                        "the tmux server ({}) did not end",
                        server.pid
                    );
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
            fn launch(&self, session: &str) -> Launched {
                let launch = launch_with(
                    Path::new("tmux"),
                    Some(&self.name),
                    session,
                    Path::new("/"),
                    &BTreeMap::new(),
                    &["sleep".to_owned(), "300".to_owned()],
                    LAUNCH_DEADLINE,
                );
                match launch {
                    Launch::Started(launched) => {
                        self.socket
                            .borrow_mut()
                            .get_or_insert_with(|| launched.socket.clone());
                        launched
                    }
                    other => panic!("{other:?}"),
                }
            }
        }

        impl Drop for Private {
            fn drop(&mut self) {
                let _ = self.tmux(&["kill-server"]);
                if let Some(socket) = self.socket.borrow().as_ref() {
                    let _ = std::fs::remove_file(socket);
                }
            }
        }

        /// On a private server: the launch's own pane is removed and its
        /// process ends; a wrong process, a gone pane and a successor
        /// server reusing the pane id and the session name are not touched.
        #[test]
        fn only_the_launchs_own_pane_is_removed() {
            let Some(server) = Private::new("own") else {
                return;
            };
            let _keep = server.launch("keep");
            let ours = server.launch("probe");
            let born = crate::procinfo::start_time(ours.pid);
            let wrong = Launched {
                pid: ours.pid + 1,
                ..ours.clone()
            };
            assert!(matches!(
                remove_launched(&wrong),
                Removal::Mismatch { pid } if pid == ours.pid
            ));
            assert_eq!(remove_launched(&ours), Removal::Removed);
            #[cfg(unix)]
            assert_eq!(leader(ours.pid, born, LEADER_GRACE), Leader::Gone);
            let _ = born;
            assert_eq!(remove_launched(&ours), Removal::Gone);
            // The server ends; a successor at the same socket reuses `%0`
            // and the name.
            let first = server.launch("again");
            server.end(first.server);
            let successor = server.launch("again");
            assert_eq!(successor.socket, first.socket);
            assert_ne!(successor.server, first.server);
            let reused = Launched {
                pane: successor.pane.clone(),
                ..first.clone()
            };
            assert_eq!(
                remove_launched(&reused),
                Removal::NotReached {
                    now: Some(successor.server)
                }
            );
            // The successor's own pane and process under the first server's
            // generation: refused inside tmux, so the pane is not killed.
            let wrong_generation = Launched {
                server: first.server,
                ..successor.clone()
            };
            assert_eq!(
                remove_launched(&wrong_generation),
                Removal::NotReached {
                    now: Some(successor.server)
                }
            );
            assert_eq!(remove_launched(&successor), Removal::Removed);
        }

        /// A socket moved aside and another server started at its path:
        /// the recorded server is not reached there, and its pane survives
        /// where its socket went.
        #[test]
        fn a_replaced_socket_reaches_another_server_and_kills_nothing() {
            let Some(a) = Private::new("moved") else {
                return;
            };
            let first = a.launch("probe");
            let aside = first.socket.with_extension("aside");
            std::fs::rename(&first.socket, &aside).unwrap();
            // B is started at A's original path.
            let started = Command::new("tmux")
                .arg("-S")
                .arg(&first.socket)
                .args([
                    "new-session",
                    "-d",
                    "-P",
                    "-F",
                    LAUNCH_FORMAT,
                    "-s",
                    "probe",
                    "--",
                    "sleep",
                    "300",
                ])
                .output()
                .unwrap();
            let other = parse_launched(&String::from_utf8_lossy(&started.stdout)).unwrap();
            assert!(matches!(
                remove_launched(&first),
                Removal::NotReached { now: Some(server) } if server == other.server
            ));
            // A's pane is still there, at the moved socket.
            let still = Launched {
                socket: aside.clone(),
                ..first.clone()
            };
            assert_eq!(remove_launched(&still), Removal::Removed);
            let _ = Command::new("tmux")
                .arg("-S")
                .arg(&first.socket)
                .arg("kill-server")
                .output();
            let _ = Command::new("tmux")
                .arg("-S")
                .arg(&aside)
                .arg("kill-server")
                .output();
            let _ = std::fs::remove_file(&first.socket);
            let _ = std::fs::remove_file(&aside);
            drop(a);
        }

        /// No server at the socket: not reached.
        #[test]
        fn no_server_at_the_socket_is_not_reached() {
            let Some(server) = Private::new("none") else {
                return;
            };
            let launched = server.launch("probe");
            server.end(launched.server);
            assert_eq!(
                remove_launched(&launched),
                Removal::NotReached { now: None }
            );
        }

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
            let launch = new_session("x", Path::new("/tmp"), &BTreeMap::new(), &[]);
            assert!(matches!(launch, Launch::NotStarted { .. }), "{launch:?}");
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
                assert!(matches!(err, Uncaptured::Refused(_)), "{target}");
                assert_eq!(err.error().kind(), io::ErrorKind::InvalidInput, "{target}");
            }
            let err = capture_bound(tmux, Path::new("default"), "%0", never, CAPTURE_DEADLINE)
                .unwrap_err();
            assert!(matches!(err, Uncaptured::Refused(_)));
            assert_eq!(err.error().kind(), io::ErrorKind::InvalidInput);
        }

        /// A tmux that never answers is given up on at the deadline, one
        /// that answers past its output's bound is killed there and its
        /// process reaped, and a pane whose process is not the agent's is
        /// refused before anything is captured. Nothing here needs a
        /// stand-in to reach a line before a clock runs out: the deadline
        /// is held only to what it guarantees wherever the stand-in was,
        /// the output's bound is crossed only after the stand-in has named
        /// itself, and a stand-in meant to answer has `BOUND` to do it.
        #[cfg(unix)]
        #[test]
        fn a_capture_is_bounded_and_bound_to_the_agents_process() {
            use std::os::unix::fs::PermissionsExt;
            use std::time::{Duration, Instant};
            /// What a stand-in meant to answer is given: a backstop a
            /// loaded machine does not reach, never what the test waits on.
            const BOUND: Duration = Duration::from_secs(10);
            let dir = tempfile::tempdir().unwrap();
            let stand_in = |name: &str, body: &str| {
                let path = dir.path().join(name);
                std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
                path
            };
            // The pid a stand-in wrote of itself, once all of it is there.
            let named = |marker: &Path| -> Option<u32> {
                std::fs::read_to_string(marker).ok()?.trim().parse().ok()
            };
            let socket = dir.path().join("server");

            // A tmux that never answers is given up on at the deadline,
            // whether it was still starting then or already asleep. One
            // that got as far as naming itself is gone and reaped; under
            // load it may not have, and the output's bound below shows the
            // reaping whatever the clock did.
            let hung = dir.path().join("hung");
            let hanging = stand_in(
                "hanging",
                &format!("echo $$ > {}; exec sleep 30", hung.display()),
            );
            let started = Instant::now();
            let err = capture_bound(
                &hanging,
                &socket,
                "%0",
                |_| true,
                Duration::from_millis(300),
            )
            .unwrap_err();
            assert!(err.to_string().contains("timed out"), "{err}");
            assert!(
                matches!(err, Uncaptured::Unanswered(_)),
                "a tmux that gave no answer said nothing of the pane"
            );
            assert!(started.elapsed() < BOUND);
            if let Some(pid) = named(&hung) {
                assert!(!crate::procinfo::alive(pid), "killed and reaped");
            }

            // A tmux that names itself and then answers past its output's
            // bound while still running: given up on there, killed and
            // reaped. Its pid is written before any of its output, so the
            // order is the script's own and no clock is raced.
            let flooded = dir.path().join("flooded");
            let flooding = stand_in(
                "flooding",
                &format!(
                    "echo $$ > {}; head -c 5000000 /dev/zero; exec sleep 30",
                    flooded.display()
                ),
            );
            let err = capture_bound(&flooding, &socket, "%0", |_| true, BOUND).unwrap_err();
            assert!(err.to_string().contains("output exceeded"), "{err}");
            assert!(matches!(err, Uncaptured::Unanswered(_)), "{err}");
            let pid = named(&flooded).expect("it named itself before its output");
            assert!(!crate::procinfo::alive(pid), "killed and reaped");

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
            let err = capture_bound(&answering, &socket, "%0", |pid| pid == 1, BOUND).unwrap_err();
            assert!(matches!(err, Uncaptured::Refused(_)));
            assert_eq!(err.error().kind(), io::ErrorKind::PermissionDenied);
            assert!(!captured.exists(), "nothing was captured");

            // A server that answers that the pane is not there.
            let missing = stand_in("missing", "echo \"can't find pane: %0\" >&2; exit 1");
            let err = capture_bound(&missing, &socket, "%0", |_| true, BOUND).unwrap_err();
            assert!(matches!(err, Uncaptured::Refused(_)), "{err}");

            // A tmux that cannot be run at all.
            let err = capture_bound(
                &dir.path().join("no-such-tmux"),
                &socket,
                "%0",
                |_| true,
                CAPTURE_DEADLINE,
            )
            .unwrap_err();
            assert!(matches!(err, Uncaptured::Unanswered(_)), "{err}");
            let text = capture_bound(&answering, &socket, "%0", |pid| pid == 4242, BOUND).unwrap();
            assert_eq!(text.trim(), "screen");
        }
    }
}
