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
    executable_path_of(std::process::id())
}

/// Kernel executable identity of another live process. Combine with its birth
/// time before and after lookup to reject PID reuse.
pub fn executable_path_of(pid: u32) -> std::io::Result<PathBuf> {
    if pid == 0 || pid > i32::MAX as u32 {
        return Err(std::io::Error::other("invalid process ID"));
    }
    #[cfg(target_os = "macos")]
    {
        use std::ffi::CStr;
        use std::os::unix::ffi::OsStrExt;

        // PROC_PIDPATHINFO_MAXSIZE in the macOS SDK is 4 * MAXPATHLEN.
        let mut buffer = [0_u8; 4096];
        // SAFETY: getpid takes no arguments; proc_pidpath receives the writable
        // buffer and its exact capacity, and cannot write past it.
        let written = unsafe {
            libc::proc_pidpath(pid as i32, buffer.as_mut_ptr().cast(), buffer.len() as u32)
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
    #[cfg(target_os = "linux")]
    {
        std::fs::read_link(format!("/proc/{pid}/exe"))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        if pid == std::process::id() {
            std::env::current_exe()
        } else {
            Err(std::io::Error::other(
                "external executable lookup is unsupported on this platform",
            ))
        }
    }
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

#[cfg(windows)]
pub fn alive(pid: u32) -> bool {
    pid > 0 && imp::alive(pid)
}

#[cfg(not(any(unix, windows)))]
pub fn alive(_pid: u32) -> bool {
    false
}

/// End the process `pid` names, only if it is the one born at `started_at`.
/// On Unix that is `SIGTERM` (or `SIGKILL` when `force`) after the birth
/// is compared; on Windows the birth is read from the handle that is then
/// terminated, so the check and the act cannot straddle a pid recycling.
/// `NotFound` means the process is gone or the pid is another's now;
/// anything else is a refusal.
pub fn end(pid: u32, started_at: DateTime<Utc>, force: bool) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let raw = i32::try_from(pid)
            .ok()
            .filter(|raw| *raw > 0)
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid pid"))?;
        match start_time(pid) {
            Some(born) if born == started_at => {}
            Some(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "the pid now belongs to a different process",
                ));
            }
            None => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "the process has already exited",
                ));
            }
        }
        let signal = if force { libc::SIGKILL } else { libc::SIGTERM };
        // SAFETY: kill only reads its scalar arguments.
        if unsafe { libc::kill(raw, signal) } == 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            return Err(std::io::Error::new(std::io::ErrorKind::NotFound, error));
        }
        Err(error)
    }
    #[cfg(windows)]
    {
        imp::end(pid, started_at, force)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (pid, started_at, force);
        Err(std::io::Error::other("not supported on this platform"))
    }
}

/// This process's parent pid, from the process table: what
/// `std::os::unix::process::parent_id` answers on Unix.
#[cfg(windows)]
pub fn parent_id() -> u32 {
    inspect(std::process::id()).map(|p| p.ppid).unwrap_or(0)
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
        "codex" => codex_runtime(&argv[1..]),
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
            .and_then(|(_, runtime)| match runtime {
                "claude-code" => claude_runtime(&argv[2..]),
                "codex" => codex_runtime(&argv[2..]),
                _ => Some(runtime),
            })
        }
        _ => None,
    }
}

/// A process of a known runtime that is not a session: the daemon it
/// keeps, a background task, an API sidecar, the bridge a browser
/// extension launches. [`runtime_of`] says no to these; this says what
/// they are, so an attempt to adopt one is refused with the reason
/// instead of registered as a `custom` agent nobody will hear from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Helper {
    pub runtime: &'static str,
    /// What the process is, for the person: "Claude Code's bridge for
    /// the browser extension".
    pub role: &'static str,
}

/// The helper a command line is, when it belongs to a known runtime but
/// is not a session of it.
pub fn helper_of(argv: &[String]) -> Option<Helper> {
    let (runtime, arguments) = match executable(argv)?.as_ref() {
        "claude" => ("claude-code", &argv[1..]),
        "codex" => ("codex", &argv[1..]),
        exe if interpreter(exe) => {
            let script = argv.get(1)?;
            #[cfg(windows)]
            let script = script.replace('\\', "/");
            if script.contains("@anthropic-ai/claude-code") {
                ("claude-code", &argv[2..])
            } else if script.contains("@openai/codex") {
                ("codex", &argv[2..])
            } else {
                return None;
            }
        }
        _ => return None,
    };
    let role = match runtime {
        "claude-code" => claude_helper(arguments),
        _ => codex_helper(arguments),
    }?;
    Some(Helper { runtime, role })
}

/// Known service entry points use the same executable as actual sessions.
/// Only inspect the first mode argument; a prompt can mention these strings.
/// `codex app-server` is the API sidecar a receiver or a reviewer speaks
/// to, not a session anybody could connect or message.
fn codex_helper(arguments: &[String]) -> Option<&'static str> {
    (arguments.first().map(String::as_str) == Some("app-server"))
        .then_some("Codex's app-server sidecar")
}

fn codex_runtime(arguments: &[String]) -> Option<&'static str> {
    codex_helper(arguments).is_none().then_some("codex")
}

/// Whether the process is Codex's own binary in any role, sidecar
/// included: what the supervised bridge attributes its app-server by.
pub fn is_codex_binary(argv: &[String]) -> bool {
    match executable(argv).as_deref() {
        Some("codex") => true,
        Some(exe) if interpreter(exe) => argv
            .get(1)
            .is_some_and(|script| script.contains("@openai/codex")),
        _ => false,
    }
}

/// `--chrome-native-host` is what the browser launches through native
/// messaging so the extension can reach Claude Code: the bridge for a
/// terminal session that drives the browser, not a session and not the
/// agent working in the browser's side panel, which has no process here.
///
/// Claude Code also runs a session in two processes: a `bg-spare` that
/// holds the model — it registers itself through its own hooks and MCP
/// server once a terminal takes it, so discovery has nothing to add and
/// `adopt` nothing to make of it — and the `attach` (later retitled
/// `agents`) terminal in front of it, which is that session's screen,
/// not a second session; adopting it would make one session two agents.
fn claude_helper(arguments: &[String]) -> Option<&'static str> {
    let mode = arguments.first()?;
    if mode.starts_with("bg-") {
        Some(
            "a Claude Code background session process, which registers itself through its own adapters",
        )
    } else {
        match mode.as_str() {
            "daemon" => Some("Claude Code's background daemon"),
            "--chrome-native-host" => Some("Claude Code's bridge for the browser extension"),
            "attach" | "agents" => {
                Some("the terminal attached to a background Claude Code session, not the session")
            }
            _ => None,
        }
    }
}

fn claude_runtime(arguments: &[String]) -> Option<&'static str> {
    claude_helper(arguments).is_none().then_some("claude-code")
}

/// The runtime whose session a process *is*, for telling who is calling:
/// [`runtime_of`], and also Claude Code's `bg-spare`, the process that
/// holds a background session's model and runs its tools. Discovery leaves
/// it alone because it registers itself; a command run from one of its
/// tools belongs to that registered session all the same.
pub fn session_runtime_of(argv: &[String]) -> Option<&'static str> {
    runtime_of(argv).or_else(|| {
        let spare = match executable(argv)?.as_ref() {
            "claude" => argv.get(1),
            exe if interpreter(exe)
                && argv
                    .get(1)
                    .is_some_and(|script| script.contains("@anthropic-ai/claude-code")) =>
            {
                argv.get(2)
            }
            _ => None,
        }?;
        (spare == "bg-spare").then_some("claude-code")
    })
}

/// What a Claude Code command line asks for by way of an earlier session:
/// `--resume <id>` / `-r <id>` / `--resume=<id>` name one, `--resume` alone
/// opens a picker and `--continue` / `-c` takes the latest, so those name
/// none. A request is a claim about what the process *asked* for; which
/// session actually runs is only known once its hooks say so — this is
/// never an identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResumeRequest {
    /// The session id the command line named, when it named one.
    pub session: Option<String>,
}

/// The resume request on a Claude Code command line, or `None` for a fresh
/// session or a command line that is not a Claude Code session at all.
pub fn resume_request(argv: &[String]) -> Option<ResumeRequest> {
    if runtime_of(argv) != Some("claude-code") {
        return None;
    }
    let mut arguments = argv.iter().skip(1).peekable();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--" => return None,
            "--continue" | "-c" => return Some(ResumeRequest { session: None }),
            "--resume" | "-r" => {
                let session = arguments
                    .peek()
                    .filter(|next| !next.starts_with('-') && !next.is_empty())
                    .map(|next| next.to_string());
                return Some(ResumeRequest { session });
            }
            other => {
                if let Some(session) = other.strip_prefix("--resume=") {
                    return Some(ResumeRequest {
                        session: Some(session.to_owned()).filter(|s| !s.is_empty()),
                    });
                }
            }
        }
    }
    None
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
    /// A resume is read from the command line the way Claude reads it,
    /// and only from a Claude Code session: the named id, a picker or the
    /// latest with no id, and nothing for a fresh session or another tool.
    #[test]
    fn a_resume_request_is_read_from_claude_arguments_only() {
        let argv = |parts: &[&str]| parts.iter().map(|p| p.to_string()).collect::<Vec<_>>();
        let named = Some("0042a5aa-1111-2222-3333-444444444444".to_owned());
        assert_eq!(
            resume_request(&argv(&["claude", "--resume", named.as_ref().unwrap()])),
            Some(ResumeRequest {
                session: named.clone()
            })
        );
        assert_eq!(
            resume_request(&argv(&[
                "claude",
                "-r",
                named.as_ref().unwrap(),
                "--verbose"
            ])),
            Some(ResumeRequest {
                session: named.clone()
            })
        );
        assert_eq!(
            resume_request(&argv(&[
                "claude",
                &format!("--resume={}", named.as_ref().unwrap())
            ])),
            Some(ResumeRequest {
                session: named.clone()
            })
        );
        assert_eq!(
            resume_request(&argv(&["claude", "--resume"])),
            Some(ResumeRequest { session: None }),
            "a picker names nothing"
        );
        assert_eq!(
            resume_request(&argv(&["claude", "--resume", "--verbose"])),
            Some(ResumeRequest { session: None })
        );
        assert_eq!(
            resume_request(&argv(&["claude", "-c"])),
            Some(ResumeRequest { session: None }),
            "continue names nothing"
        );
        for prompt in ["--resume", "--resume=session", "-r", "--continue", "-c"] {
            assert_eq!(
                resume_request(&argv(&["claude", "--", prompt, "session"])),
                None,
                "text after the option separator is a prompt"
            );
        }
        assert_eq!(
            resume_request(&argv(&["claude", "--continue", "--", "--resume"])),
            Some(ResumeRequest { session: None }),
            "a resume option before the separator still selects the wait"
        );
        assert_eq!(resume_request(&argv(&["claude"])), None);
        assert_eq!(resume_request(&argv(&["claude", "--verbose"])), None);
        assert_eq!(
            resume_request(&argv(&["codex", "resume", "abc"])),
            None,
            "not a Claude session"
        );
        assert_eq!(
            resume_request(&argv(&["claude", "daemon", "--resume", "x"])),
            None,
            "not a session either"
        );
    }

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
            "claude bg-spare --bg-spare /tmp/cc-daemon-501/x/spare/y.claim.sock",
            "claude attach ef93b626",
            "/Users/p/.local/bin/claude agents",
        ] {
            assert_eq!(runtime_of(&argv(command)), None, "{command}");
            let helper = helper_of(&argv(command)).unwrap_or_else(|| panic!("{command}"));
            assert_eq!(helper.runtime, "claude-code", "{command}");
        }
        assert_eq!(
            helper_of(&argv("claude --chrome-native-host")).map(|h| h.role),
            Some("Claude Code's bridge for the browser extension")
        );
        assert!(
            helper_of(&argv("claude attach ef93b626"))
                .map(|h| h.role)
                .is_some_and(|role| role.contains("terminal attached"))
        );
        for command in [
            "claude --chrome",
            "claude -- --chrome-native-host",
            "claude --resume session",
            "node /x/@anthropic-ai/claude-code/cli.js --chrome",
            "node /x/@anthropic-ai/claude-code/cli.js -- --chrome-native-host",
            "claude -p attach the file",
            "claude -- agents",
        ] {
            assert_eq!(runtime_of(&argv(command)), Some("claude-code"), "{command}");
            assert_eq!(helper_of(&argv(command)), None, "{command}");
        }
    }

    /// A helper is a known runtime's process; a program of its own is not
    /// one, however it is called.
    #[test]
    fn helpers_belong_to_known_runtimes_only() {
        let argv = |command: &str| {
            command
                .split_whitespace()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        let codex = helper_of(&argv("codex app-server --stdio")).unwrap();
        assert_eq!(
            (codex.runtime, codex.role),
            ("codex", "Codex's app-server sidecar")
        );
        assert_eq!(
            helper_of(&argv("node /x/@openai/codex/bin/codex.js app-server")).map(|h| h.runtime),
            Some("codex")
        );
        assert_eq!(runtime_of(&argv("codex app-server --stdio")), None);
        for command in [
            "codex resume 01a0",
            "sleep --chrome-native-host",
            "chrome-native-host",
            "node /x/other/cli.js --chrome-native-host",
        ] {
            assert_eq!(helper_of(&argv(command)), None, "{command}");
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
        // A background session's own process is that session to a caller,
        // though discovery leaves it to register itself; its terminal and
        // host are not.
        assert_eq!(
            session_runtime_of(&argv("claude bg-spare --bg-spare /tmp/x")),
            Some("claude-code")
        );
        assert_eq!(
            session_runtime_of(&argv("node /x/@anthropic-ai/claude-code/cli.js bg-spare")),
            Some("claude-code")
        );
        assert_eq!(
            session_runtime_of(&argv("claude bg-pty-host --bg-pty-host /tmp/x")),
            None
        );
        assert_eq!(session_runtime_of(&argv("claude attach")), None);
        assert_eq!(session_runtime_of(&argv("claude daemon run")), None);
        assert_eq!(
            session_runtime_of(&argv("codex --full-auto")),
            Some("codex")
        );
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
        // The API sidecar is Codex's binary but not a session.
        let sidecar: Vec<String> = ["/opt/codex/bin/codex", "app-server", "--stdio"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        assert_eq!(runtime_of(&sidecar), None);
        assert!(is_codex_binary(&sidecar));
        let session: Vec<String> = ["/opt/codex/bin/codex", "resume", "abc"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        assert_eq!(runtime_of(&session), Some("codex"));
        for interpreter in ["node", "bun", "deno", "python", "python3"] {
            let args = |mode: &str| {
                [
                    interpreter,
                    "/x/@openai/codex/bin/codex.js",
                    mode,
                    "--stdio",
                ]
                .map(str::to_owned)
            };
            assert_eq!(runtime_of(&args("app-server")), None);
            assert_eq!(runtime_of(&args("resume")), Some("codex"));
        }
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
