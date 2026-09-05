//! Process identity beyond the pid, and the process table.
//!
//! A pid is recycled once its process exits, so "pid still exists" is not
//! proof that the agent which reported it is still running — least of all
//! after a reboot. The start time of the process is the cheap identity the
//! OS gives us; the daemon records it at registration and compares it
//! during liveness checks.
//!
//! The process table is read with one `ps` invocation, which is portable
//! across macOS and Linux and gives the full argument list, so a Claude Code
//! run as `node …/@anthropic-ai/claude-code/cli.js` is recognised as well as
//! a native `claude`. Only the working directory needs platform code.

use std::path::{Path, PathBuf};
use std::process::Command;

use chrono::{DateTime, Utc};

/// When the process with this pid started, if the platform can tell us.
pub fn start_time(pid: u32) -> Option<DateTime<Utc>> {
    imp::start_time(pid)
}

/// The current working directory of another process of ours.
pub fn cwd(pid: u32) -> Option<PathBuf> {
    imp::cwd(pid)
}

/// One row of the process table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Process {
    pub pid: u32,
    pub ppid: u32,
    pub argv: Vec<String>,
}

/// Every process `ps` will show us, or nothing if `ps` is unavailable.
pub fn processes() -> Vec<Process> {
    let output = Command::new("ps")
        .args(["-axo", "pid=,ppid=,args="])
        .output();
    match output {
        Ok(output) if output.status.success() => parse_ps(&String::from_utf8_lossy(&output.stdout)),
        _ => Vec::new(),
    }
}

/// One process, if it exists.
pub fn inspect(pid: u32) -> Option<Process> {
    let output = Command::new("ps")
        .args(["-o", "pid=,ppid=,args=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    parse_ps(&String::from_utf8_lossy(&output.stdout))
        .into_iter()
        .find(|p| p.pid == pid)
}

fn parse_ps(text: &str) -> Vec<Process> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse().ok()?;
            let ppid = fields.next()?.parse().ok()?;
            let argv: Vec<String> = fields.map(str::to_owned).collect();
            // `(name)` is what ps prints for a zombie; there is nothing to adopt.
            if argv.first().is_none_or(|first| first.starts_with('(')) {
                return None;
            }
            Some(Process { pid, ppid, argv })
        })
        .collect()
}

/// The agent runtime a command line belongs to, by the executable's name
/// or — for interpreters — the package it runs. `None` for anything that is
/// not a known agent.
pub fn runtime_of(argv: &[String]) -> Option<&'static str> {
    let exe = basename(argv.first()?);
    match exe {
        // Claude Code runs helper processes under the same binary; only the
        // interactive session is an agent.
        "claude"
            if argv
                .get(1)
                .is_some_and(|sub| sub.starts_with("bg-") || sub == "daemon") =>
        {
            None
        }
        "claude" => Some("claude-code"),
        "codex" => Some("codex"),
        "gemini" => Some("gemini-cli"),
        "cursor-agent" => Some("cursor"),
        "aider" => Some("aider"),
        "goose" => Some("goose"),
        "copilot" => Some("copilot"),
        "amp" => Some("amp"),
        "opencode" => Some("opencode"),
        "node" | "bun" | "deno" | "python" | "python3" => {
            let script = argv.get(1)?;
            [
                ("@anthropic-ai/claude-code", "claude-code"),
                ("@openai/codex", "codex"),
                ("@google/gemini-cli", "gemini-cli"),
                ("/aider/", "aider"),
            ]
            .into_iter()
            .find(|(marker, _)| script.contains(marker))
            .map(|(_, runtime)| runtime)
        }
        _ => None,
    }
}

fn basename(path: &str) -> &str {
    Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(path)
}

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::{CStr, OsStr};
    use std::os::unix::ffi::OsStrExt;
    use std::path::PathBuf;

    use chrono::{DateTime, Utc};

    pub fn cwd(pid: u32) -> Option<PathBuf> {
        let pid = i32::try_from(pid).ok()?;
        // SAFETY: proc_vnodepathinfo is plain old data, so all-zero is valid.
        let mut info: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
        let size = i32::try_from(std::mem::size_of::<libc::proc_vnodepathinfo>()).ok()?;
        // SAFETY: the buffer is a correctly sized proc_vnodepathinfo and
        // `size` is its length, so the kernel writes within bounds.
        let written = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDVNODEPATHINFO,
                0,
                (&raw mut info).cast::<libc::c_void>(),
                size,
            )
        };
        if written != size {
            return None;
        }
        // SAFETY: the kernel NUL-terminates vip_path within its buffer.
        // libc declares the 1024-byte path as a nested array; view it flat.
        let path =
            unsafe { CStr::from_ptr(info.pvi_cdir.vip_path.as_ptr().cast::<libc::c_char>()) };
        if path.to_bytes().is_empty() {
            return None;
        }
        Some(PathBuf::from(OsStr::from_bytes(path.to_bytes())))
    }

    pub fn start_time(pid: u32) -> Option<DateTime<Utc>> {
        let pid = i32::try_from(pid).ok()?;
        // SAFETY: proc_bsdinfo is plain old data, so all-zero is a valid value.
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = i32::try_from(std::mem::size_of::<libc::proc_bsdinfo>()).ok()?;
        // SAFETY: the buffer is a correctly sized proc_bsdinfo and `size`
        // is its length, so the kernel writes within bounds.
        let written = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                0,
                (&raw mut info).cast::<libc::c_void>(),
                size,
            )
        };
        if written != size {
            return None;
        }
        let secs = i64::try_from(info.pbi_start_tvsec).ok()?;
        let nanos = u32::try_from(info.pbi_start_tvusec)
            .ok()?
            .checked_mul(1000)?;
        DateTime::from_timestamp(secs, nanos)
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::path::PathBuf;

    use chrono::{DateTime, Duration, Utc};

    pub fn cwd(pid: u32) -> Option<PathBuf> {
        std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
    }

    pub fn start_time(pid: u32) -> Option<DateTime<Utc>> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        // Everything after the parenthesised command name; `starttime` is
        // field 22 overall, so the 20th after the closing parenthesis.
        let after_comm = stat.rsplit_once(')')?.1;
        let ticks: i64 = after_comm.split_whitespace().nth(19)?.parse().ok()?;
        // SAFETY: sysconf has no preconditions.
        let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        if hz <= 0 {
            return None;
        }
        let since_boot = Duration::milliseconds(ticks.checked_mul(1000)? / hz);
        Some(boot_time()? + since_boot)
    }

    fn boot_time() -> Option<DateTime<Utc>> {
        let stat = std::fs::read_to_string("/proc/stat").ok()?;
        let secs: i64 = stat
            .lines()
            .find_map(|line| line.strip_prefix("btime "))?
            .trim()
            .parse()
            .ok()?;
        DateTime::from_timestamp(secs, 0)
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod imp {
    use std::path::PathBuf;

    use chrono::{DateTime, Utc};

    pub fn cwd(_pid: u32) -> Option<PathBuf> {
        None
    }

    pub fn start_time(_pid: u32) -> Option<DateTime<Utc>> {
        None
    }
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod tests {
    use super::*;

    #[test]
    fn own_process_has_a_recent_start_time() {
        let started = start_time(std::process::id()).expect("readable on this platform");
        let age = Utc::now() - started;
        assert!(
            age.num_seconds() >= 0,
            "start time in the future: {started}"
        );
        assert!(
            age.num_hours() < 24 * 365,
            "implausible start time: {started}"
        );
    }

    #[test]
    fn dead_process_has_none() {
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        assert!(start_time(pid).is_none());
        assert!(inspect(pid).is_none());
    }

    #[test]
    fn process_table_shows_a_child_with_its_arguments_and_cwd() {
        let dir = tempfile::TempDir::new().unwrap();
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .current_dir(dir.path())
            .spawn()
            .unwrap();
        let pid = child.id();

        let row = processes()
            .into_iter()
            .find(|p| p.pid == pid)
            .expect("child is in the table");
        assert_eq!(row.ppid, std::process::id());
        assert_eq!(basename(&row.argv[0]), "sleep");
        assert_eq!(row.argv[1], "30");
        assert_eq!(inspect(pid).as_ref(), Some(&row));
        assert_eq!(cwd(pid), Some(dir.path().canonicalize().unwrap()));

        child.kill().unwrap();
        child.wait().unwrap();
    }

    #[test]
    fn runtimes_by_executable_or_package() {
        let argv = |s: &str| s.split(' ').map(str::to_owned).collect::<Vec<_>>();
        assert_eq!(
            runtime_of(&argv("/usr/local/bin/claude")),
            Some("claude-code")
        );
        assert_eq!(
            runtime_of(&argv(
                "node /opt/homebrew/lib/node_modules/@anthropic-ai/claude-code/cli.js --resume"
            )),
            Some("claude-code")
        );
        assert_eq!(runtime_of(&argv("claude bg-spare --bg-spare /tmp/x")), None);
        assert_eq!(runtime_of(&argv("claude daemon run --origin x")), None);
        assert_eq!(runtime_of(&argv("claude --resume")), Some("claude-code"));
        assert_eq!(runtime_of(&argv("codex --full-auto")), Some("codex"));
        assert_eq!(
            runtime_of(&argv("node /x/@openai/codex/bin/codex.js")),
            Some("codex")
        );
        assert_eq!(runtime_of(&argv("gemini")), Some("gemini-cli"));
        assert_eq!(runtime_of(&argv("cursor-agent")), Some("cursor"));
        assert_eq!(
            runtime_of(&argv("python3 /venv/lib/aider/main.py")),
            Some("aider")
        );
        assert_eq!(runtime_of(&argv("node /x/some/other.js")), None);
        assert_eq!(runtime_of(&argv("/bin/zsh -l")), None);
        assert_eq!(runtime_of(&[]), None);
    }

    #[test]
    fn ps_lines_parse_and_zombies_are_dropped() {
        let rows = parse_ps(
            "  123     1 /sbin/launchd\n 4567   123 node /a/b.js --flag\n 999 1 (claude)\nbad line\n",
        );
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].pid, 123);
        assert_eq!(rows[1].argv, ["node", "/a/b.js", "--flag"]);
    }
}
