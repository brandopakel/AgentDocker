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
#[cfg(unix)]
use std::process::Command;

use chrono::{DateTime, Utc};

/// The executable loaded by this process, independent of a mutable launch link.
/// macOS `current_exe` can return the original symlink spelling. Resolving that
/// spelling after activation could select another release, so ask the kernel.
pub fn executable_path() -> std::io::Result<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        use std::ffi::CStr;
        use std::os::unix::ffi::OsStrExt;

        // PROC_PIDPATHINFO_MAXSIZE in the macOS SDK is 4 * MAXPATHLEN.
        let mut buffer = [0_u8; 4096];
        // SAFETY: getpid takes no arguments; proc_pidpath receives the writable
        // buffer and its exact capacity, and cannot write past it.
        let written = unsafe {
            libc::proc_pidpath(
                libc::getpid(),
                buffer.as_mut_ptr().cast(),
                buffer.len() as u32,
            )
        };
        if written <= 0 {
            return Err(std::io::Error::last_os_error());
        }
        let path = CStr::from_bytes_until_nul(&buffer)
            .map_err(|_| std::io::Error::other("kernel executable path is not terminated"))?;
        let path = PathBuf::from(std::ffi::OsStr::from_bytes(path.to_bytes()));
        if !path.is_absolute() {
            return Err(std::io::Error::other(
                "kernel executable path is not absolute",
            ));
        }
        Ok(path)
    }
    #[cfg(not(target_os = "macos"))]
    std::env::current_exe()
}

/// When the process with this pid started, if the platform can tell us.
pub fn start_time(pid: u32) -> Option<DateTime<Utc>> {
    imp::start_time(pid)
}

/// Whether a process with this pid exists at all.
///
/// Only that. It says nothing about *which* process — a recycled pid
/// exists just as convincingly as the one that registered it, which is
/// what [`start_time`] is for. Signal zero is the portable way to ask:
/// it performs the permission checks and reaches the process without
/// delivering anything, so `EPERM` is a yes.
#[cfg(unix)]
pub fn alive(pid: u32) -> bool {
    let Ok(raw) = i32::try_from(pid) else {
        return false;
    };
    if raw <= 0 {
        return false;
    }
    // SAFETY: kill only reads its scalar arguments, and signal zero
    // delivers nothing.
    if unsafe { libc::kill(raw, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
pub fn alive(_pid: u32) -> bool {
    false
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

/// Every process `ps` will show us. Failure is distinct from an empty
/// successful scan, so background discovery never invents exits on an outage.
#[cfg(unix)]
pub fn processes() -> std::io::Result<Vec<Process>> {
    let output = crate::command::run(
        Path::new("/"),
        &["ps".into(), "-axo".into(), "pid=,ppid=,args=".into()],
        std::time::Duration::from_secs(3),
    )?;
    if !output.success {
        return Err(std::io::Error::other("ps process scan failed"));
    }
    Ok(parse_ps(&output.stdout))
}

/// One process, if it exists.
#[cfg(unix)]
pub fn inspect(pid: u32) -> Option<Process> {
    let output = Command::new("ps")
        .args(["-o", "pid=,ppid=,args=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    parse_ps(&String::from_utf8_lossy(&output.stdout))
        .into_iter()
        .find(|p| p.pid == pid)
}

#[cfg(unix)]
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
fn executable(argv: &[String]) -> Option<std::borrow::Cow<'_, str>> {
    let exe = basename(argv.first()?);
    #[cfg(windows)]
    {
        let normalized = exe.to_ascii_lowercase();
        Some(
            normalized
                .strip_suffix(".exe")
                .unwrap_or(&normalized)
                .to_owned()
                .into(),
        )
    }
    #[cfg(not(windows))]
    Some(exe.into())
}

fn interpreter(exe: &str) -> bool {
    matches!(exe, "node" | "bun" | "deno" | "python" | "python3")
}

pub fn runtime_of(argv: &[String]) -> Option<&'static str> {
    match executable(argv)?.as_ref() {
        "claude" => claude_runtime(&argv[1..]),
        "codex" => Some("codex"),
        "gemini" => Some("gemini-cli"),
        "cursor-agent" => Some("cursor"),
        "aider" => Some("aider"),
        "goose" => Some("goose"),
        "copilot" => Some("copilot"),
        "amp" => Some("amp"),
        "opencode" => Some("opencode"),
        exe if interpreter(exe) => {
            let script = argv.get(1)?;
            #[cfg(windows)]
            let script = script.replace('\\', "/");
            [
                ("@anthropic-ai/claude-code", "claude-code"),
                ("@openai/codex", "codex"),
                ("@google/gemini-cli", "gemini-cli"),
                ("/aider/", "aider"),
            ]
            .into_iter()
            .find(|(marker, _)| script.contains(marker))
            .and_then(|(_, runtime)| {
                if runtime == "claude-code" {
                    claude_runtime(&argv[2..])
                } else {
                    Some(runtime)
                }
            })
        }
        _ => None,
    }
}

/// Known service entry points use the same executable as actual sessions.
/// Only inspect the first mode argument; a prompt can mention these strings.
fn claude_runtime(arguments: &[String]) -> Option<&'static str> {
    (!arguments.first().is_some_and(|mode| {
        mode.starts_with("bg-") || matches!(mode.as_str(), "daemon" | "--chrome-native-host")
    }))
    .then_some("claude-code")
}

/// Codex's interpreter launcher starts a native child and then waits for it. Only
/// that child is a session. Do not apply a generic parent/child rule: agents
/// can launch other agents, and those must remain independently discoverable.
pub fn codex_launchers(table: &[Process]) -> std::collections::BTreeSet<u32> {
    let parents: std::collections::BTreeSet<_> = table
        .iter()
        .filter(|p| {
            executable(&p.argv).as_deref() == Some("codex") && runtime_of(&p.argv) == Some("codex")
        })
        .map(|p| p.ppid)
        .collect();
    table
        .iter()
        .filter(|p| {
            parents.contains(&p.pid)
                && executable(&p.argv).as_deref().is_some_and(interpreter)
                && runtime_of(&p.argv) == Some("codex")
        })
        .map(|p| p.pid)
        .collect()
}

#[cfg(windows)]
pub fn processes() -> std::io::Result<Vec<Process>> {
    imp::processes()
}

#[cfg(windows)]
pub fn inspect(pid: u32) -> Option<Process> {
    imp::inspect(pid)
}

#[cfg(windows)]
#[path = "procinfo/windows.rs"]
mod imp;

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

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
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

#[cfg(all(test, windows))]
mod windows_identity_tests {
    use super::*;

    #[test]
    fn codex_launchers_share_windows_executable_normalization() {
        for host in ["NODE", "Bun", "DENO", "Python", "PYTHON3"] {
            let wrapper = Process {
                pid: 10,
                ppid: 1,
                argv: vec![
                    format!(r"C:\tools\{host}.EXE"),
                    r"C:\pkg\@openai\codex\bin\codex.js".into(),
                ],
            };
            let native = Process {
                pid: 11,
                ppid: 10,
                argv: vec![r"C:\vendor\CODEX.Exe".into()],
            };
            assert_eq!(runtime_of(&wrapper.argv), Some("codex"));
            assert_eq!(codex_launchers(&[wrapper, native]), [10].into());
        }
    }
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod tests {
    #[test]
    fn claude_browser_hosts_are_not_sessions_or_duplicate_discovery_candidates() {
        let argv = |command: &str| {
            command
                .split_whitespace()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        for command in [
            "claude --chrome-native-host",
            "node /x/@anthropic-ai/claude-code/cli.js --chrome-native-host",
            "node /x/@anthropic-ai/claude-code/cli.js daemon run",
            "node /x/@anthropic-ai/claude-code/cli.js bg-spare",
        ] {
            assert_eq!(runtime_of(&argv(command)), None, "{command}");
        }
        for command in [
            "claude --chrome",
            "claude -- --chrome-native-host",
            "claude --resume session",
            "node /x/@anthropic-ai/claude-code/cli.js --chrome",
            "node /x/@anthropic-ai/claude-code/cli.js -- --chrome-native-host",
        ] {
            assert_eq!(runtime_of(&argv(command)), Some("claude-code"), "{command}");
        }
    }

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
            .unwrap()
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
    fn codex_interpreter_launcher_is_not_a_second_session() {
        let process = |pid, ppid, command: &str| Process {
            pid,
            ppid,
            argv: command.split_whitespace().map(str::to_owned).collect(),
        };
        let wrapper = process(10, 1, "node /x/@openai/codex/bin/codex.js");
        let native = process(11, 10, "/x/vendor/bin/codex");
        assert_eq!(
            codex_launchers(&[wrapper.clone(), native.clone()]),
            [10].into()
        );
        // A standalone JS runtime, a nested native agent, and an unrelated
        // node host are all separate observations.
        assert!(codex_launchers(&[wrapper]).is_empty());
        assert!(codex_launchers(&[native.clone(), process(12, 11, "codex")]).is_empty());
        assert!(
            codex_launchers(&[process(10, 1, "node /app/server.js"), native.clone()]).is_empty()
        );
        for host in ["node", "bun", "deno", "python", "python3"] {
            let wrapper = process(10, 1, &format!("{host} /x/@openai/codex/bin/codex.js"));
            assert_eq!(codex_launchers(&[wrapper, native.clone()]), [10].into());
        }
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
