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
//! itself. **On macOS it cannot**: `ps -E` returns only the command line
//! for a process that is not the caller, whatever the user. So the
//! signal has to arrive another way there, and it does — an agent that
//! registers itself is running *inside* the session, so the client reads
//! its own environment and sends what it saw.
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
    use std::process::Command;

    /// `ps -Ewww` prints the environment after the command line, as
    /// `KEY=VALUE` words. It is the only way to read another process's
    /// environment on macOS without entitlements, and it only works for
    /// our own user — which is exactly the case we need.
    pub fn environment(pid: u32) -> Option<BTreeMap<String, String>> {
        let output = Command::new("ps")
            .args(["-Ewwwo", "command=", "-p", &pid.to_string()])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        Some(parse_ps_environment(&String::from_utf8_lossy(
            &output.stdout,
        )))
    }

    /// Everything after the command line is the environment, one
    /// `KEY=VALUE` per word. The command line comes first and may itself
    /// contain an `=`, so only words whose key looks like a variable
    /// name are taken — and once one is, everything after it is
    /// environment too.
    pub fn parse_ps_environment(text: &str) -> BTreeMap<String, String> {
        let mut env = BTreeMap::new();
        let mut in_environment = false;
        for word in text.split_whitespace() {
            let Some((key, value)) = word.split_once('=') else {
                continue;
            };
            if !in_environment {
                if !looks_like_variable(key) {
                    continue;
                }
                in_environment = true;
            }
            if looks_like_variable(key) {
                env.insert(key.to_owned(), value.to_owned());
            }
        }
        env
    }

    /// A shell variable name: letters, digits and underscores, not
    /// starting with a digit, and not empty. `--flag` and `/a/path` are
    /// not, which is what keeps the command line out.
    fn looks_like_variable(key: &str) -> bool {
        !key.is_empty()
            && !key.starts_with(|c: char| c.is_ascii_digit())
            && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
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

    #[cfg(target_os = "macos")]
    #[test]
    fn ps_output_splits_the_command_line_from_the_environment() {
        // `ps -E` prints argv and then KEY=VALUE words. The command line
        // can contain an `=` of its own, which must not be mistaken for
        // the start of the environment.
        let env = platform::parse_ps_environment(
            "/usr/bin/node --inspect=127.0.0.1 server.js PATH=/usr/bin TMUX_PANE=%2 SHELL=/bin/zsh",
        );
        assert_eq!(env.get("TMUX_PANE").map(String::as_str), Some("%2"));
        assert_eq!(env.get("PATH").map(String::as_str), Some("/usr/bin"));
        assert!(
            !env.contains_key("--inspect"),
            "a flag with an = is not a variable: {env:?}"
        );
        assert_eq!(env.len(), 3);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn proc_environ_is_nul_separated() {
        let env = platform::parse_nul_separated(b"PATH=/usr/bin\0TMUX_PANE=%2\0\0");
        assert_eq!(env.get("TMUX_PANE").map(String::as_str), Some("%2"));
        assert_eq!(env.len(), 2);
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
