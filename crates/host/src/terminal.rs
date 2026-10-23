//! Which application a process's terminal is in, read from its ancestry.
//!
//! The conventions — which executable is which host — live in
//! `agentdocker_core::terminal`; this is the I/O: the process table, each
//! ancestor's executable and the process's own terminal device.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use agentdocker_core::terminal::{Host, Own, bundle_of, classify, device, own};

use crate::procinfo::{self, Process};

/// Where a walk up a process's parents ends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reach {
    /// The application its terminal is in, outside AgentDocker.
    Host(Host),
    /// AgentDocker itself: its daemon (a session owner included) or its
    /// app is an ancestor, so the process runs in AgentDocker's own
    /// terminal or in none. `child` is the process AgentDocker started on
    /// the way down, the one a managed session records as its own.
    AgentDocker { child: u32 },
    /// Nothing on the way up says: the walk met pid 1 or a missing parent,
    /// or ran out of steps.
    Unknown,
}

/// Walk up from `pid` to where its terminal is, reading each ancestor's
/// executable with `executable_of` and asking `ours` whether a bundle not
/// named `AgentDocker.app` is AgentDocker's all the same. AgentDocker's CLI
/// and helpers are passed over to the terminal they run in; its daemon or
/// app ends the walk as [`Reach::AgentDocker`]; the nearest other ancestor
/// that is a terminal application or an application bundle is the host,
/// without a terminal device (the caller reads `pid`'s own). A
/// multiplexer server is not a host and usually has no parent but launchd,
/// so an agent inside one has none here; it is reached through the
/// multiplexer's client instead. An ancestor whose executable cannot be
/// read is passed over. Walking stops at pid 1, at a missing parent, or
/// after enough steps that a cycle cannot spin.
pub fn walk(
    pid: u32,
    by_pid: &BTreeMap<u32, Process>,
    executable_of: impl Fn(u32) -> Option<PathBuf>,
    ours: impl Fn(&Path) -> bool,
) -> Reach {
    let Some(mut current) = by_pid.get(&pid).map(|process| process.ppid) else {
        return Reach::Unknown;
    };
    let mut child = pid;
    for _ in 0..32 {
        if current <= 1 {
            return Reach::Unknown;
        }
        let process = by_pid.get(&current);
        if let Some(executable) = executable_of(current) {
            let argv = process.map_or(&[][..], |process| process.argv.as_slice());
            match own(&executable, argv, &ours) {
                Some(Own::Daemon | Own::App) => return Reach::AgentDocker { child },
                Some(Own::Cli) => {}
                None => {
                    if let Some(host) = classify(&executable) {
                        return Reach::Host(host);
                    }
                }
            }
        }
        let Some(process) = process else {
            return Reach::Unknown;
        };
        (child, current) = (current, process.ppid);
    }
    Reach::Unknown
}

/// [`walk`] over the kernel's executables, with `pid`'s own terminal
/// device on the host it finds.
pub fn reach(pid: u32, by_pid: &BTreeMap<u32, Process>) -> Reach {
    match walk(
        pid,
        by_pid,
        |ancestor| procinfo::executable_path_of(ancestor).ok(),
        ours,
    ) {
        Reach::Host(mut host) => {
            host.tty = device_of(pid);
            Reach::Host(host)
        }
        other => other,
    }
}

/// The host of `pid`'s terminal outside AgentDocker, by [`reach`]: `None`
/// when AgentDocker itself runs it or nothing on the way up says.
pub fn of(pid: u32, by_pid: &BTreeMap<u32, Process>) -> Option<Host> {
    match reach(pid, by_pid) {
        Reach::Host(host) => Some(host),
        Reach::AgentDocker { .. } | Reach::Unknown => None,
    }
}

/// [`reach`], reading the process table itself; [`Reach::Unknown`] when it
/// cannot be read.
pub fn reach_process(pid: u32) -> Reach {
    let Ok(processes) = procinfo::processes() else {
        return Reach::Unknown;
    };
    let by_pid: BTreeMap<u32, Process> = processes
        .into_iter()
        .map(|process| (process.pid, process))
        .collect();
    reach(pid, &by_pid)
}

/// [`of`], reading the process table itself.
pub fn of_process(pid: u32) -> Option<Host> {
    match reach_process(pid) {
        Reach::Host(host) => Some(host),
        Reach::AgentDocker { .. } | Reach::Unknown => None,
    }
}

/// Whether `bundle` is AgentDocker's under any name: the bundle this
/// process runs from, or one whose `Contents/MacOS` holds AgentDocker's
/// CLI. A bundle named `AgentDocker.app` needs no asking.
pub fn ours(bundle: &Path) -> bool {
    static RUNNING: OnceLock<Option<PathBuf>> = OnceLock::new();
    let running = RUNNING.get_or_init(|| {
        let executable = std::env::current_exe().ok()?;
        let executable = std::fs::canonicalize(&executable).unwrap_or(executable);
        bundle_of(&executable).map(|(bundle, _)| bundle)
    });
    let resolved = std::fs::canonicalize(bundle).unwrap_or_else(|_| bundle.to_owned());
    running.as_deref() == Some(resolved.as_path())
        || bundle
            .join("Contents")
            .join("MacOS")
            .join(crate::program::Role::Cli.file_name())
            .is_file()
}

/// The terminal device `pid` reads and writes, as `ps` reports it, or
/// `None` for a process without one.
pub fn device_of(pid: u32) -> Option<String> {
    let output = crate::command::run(
        Path::new("/"),
        &[
            "ps".into(),
            "-p".into(),
            pid.to_string(),
            "-o".into(),
            "tty=".into(),
        ],
        Duration::from_secs(3),
    )
    .ok()?;
    if !output.success {
        return None;
    }
    device(&output.stdout)
}

/// What a full-screen program turns on in its terminal that a shell at its
/// prompt does not use: mouse reporting (every mode, every encoding), focus
/// reports, the keyboard protocol it pushed, a hidden cursor. Turning these
/// off never takes away the returning shell's own modes; bracketed paste,
/// cursor-key and keypad modes and the alternate screen are the shell's to
/// set at its prompt, and are left alone.
#[cfg(unix)]
const RESTORE_MODES: &[u8] =
    b"\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1005l\x1b[?1006l\x1b[?1015l\x1b[?1004l\x1b[<u\x1b[?25h";

/// A terminal held open from the moment a program on it is stopped, so what
/// is written later reaches that terminal: when its window closes, the
/// device is revoked and the write fails, never landing in a window that
/// opened on the same path since.
#[cfg(unix)]
pub struct Pinned {
    device: std::fs::File,
}

/// Hold `tty` open, for writing only and never as a controlling terminal.
/// Refused for anything but a terminal device.
#[cfg(unix)]
pub fn pin(tty: &str) -> std::io::Result<Pinned> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;
    if !tty.starts_with("/dev/") || tty.contains("..") {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not a terminal device",
        ));
    }
    let device = std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NOCTTY | libc::O_NONBLOCK)
        .open(tty)?;
    // SAFETY: `isatty` only reads the descriptor.
    if unsafe { libc::isatty(device.as_raw_fd()) } != 1 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not a terminal device",
        ));
    }
    Ok(Pinned { device })
}

#[cfg(unix)]
impl Pinned {
    /// Undo what a full-screen program ended by a signal had no chance to:
    /// without it a shell gets its terminal back still reporting every mouse
    /// move as typed text. Written as output, so the shell reads nothing.
    pub fn restore_modes(&mut self) -> std::io::Result<()> {
        std::io::Write::write_all(&mut self.device, RESTORE_MODES)
    }
}

/// A process's place on its terminal, as `ps` reports it: its own process
/// group, the group in the terminal's foreground, and the terminal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Foreground {
    pub pgid: u32,
    pub tpgid: u32,
    pub tty: String,
}

impl Foreground {
    /// Whether the process's own job is the one in the foreground of `tty`:
    /// a shell has its terminal back, and no job it started holds it.
    pub fn holds(&self, tty: &str) -> bool {
        self.tty == tty && self.pgid != 0 && self.pgid == self.tpgid
    }
}

/// Where `pid` stands on its terminal, or `None` for a process without one.
pub fn foreground_of(pid: u32) -> Option<Foreground> {
    let output = crate::command::run(
        Path::new("/"),
        &[
            "ps".into(),
            "-p".into(),
            pid.to_string(),
            "-o".into(),
            "pgid=,tpgid=,tty=".into(),
        ],
        Duration::from_secs(3),
    )
    .ok()?;
    if !output.success {
        return None;
    }
    let mut fields = output.stdout.split_whitespace();
    let pgid = fields.next()?.parse().ok()?;
    let tpgid = fields.next()?.parse().ok()?;
    let tty = device(fields.next()?)?;
    Some(Foreground { pgid, tpgid, tty })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn only_a_terminal_device_is_pinned() {
        for path in [
            "",
            "relative",
            "/etc/passwd/../dev/x",
            "/tmp/not-a-device",
            "/dev/null",
        ] {
            assert!(pin(path).is_err(), "{path}");
        }
    }

    #[test]
    fn a_shell_holds_its_terminal_only_while_its_own_job_is_in_front() {
        let shell = |tpgid| Foreground {
            pgid: 400,
            tpgid,
            tty: "/dev/ttys002".into(),
        };
        assert!(shell(400).holds("/dev/ttys002"), "back at its prompt");
        assert!(
            !shell(512).holds("/dev/ttys002"),
            "a job it started next holds the terminal"
        );
        assert!(!shell(400).holds("/dev/ttys003"), "another terminal");
        assert!(
            !Foreground {
                pgid: 0,
                tpgid: 0,
                tty: "/dev/ttys002".into()
            }
            .holds("/dev/ttys002")
        );
        assert!(
            !String::from_utf8_lossy(RESTORE_MODES).contains("?2004l")
                && !String::from_utf8_lossy(RESTORE_MODES).contains("?1049l"),
            "the shell's own modes are left alone"
        );
    }

    #[test]
    fn a_missing_process_has_no_host_and_a_device_is_a_path() {
        assert_eq!(of(u32::MAX - 1, &BTreeMap::new()), None);
        // The test runner may or may not have a terminal; either way the
        // answer is a device path or nothing, never `??`.
        if let Some(tty) = device_of(std::process::id()) {
            assert!(tty.starts_with("/dev/"), "{tty}");
        }
    }

    #[test]
    fn the_walk_stops_at_launchd_and_at_a_missing_parent() {
        let table: BTreeMap<u32, Process> = [
            (
                900_001,
                Process {
                    pid: 900_001,
                    ppid: 900_002,
                    argv: vec!["claude".into()],
                },
            ),
            (
                900_002,
                Process {
                    pid: 900_002,
                    ppid: 1,
                    argv: vec!["-zsh".into()],
                },
            ),
        ]
        .into();
        // 900_002 does not exist as a real process, so its executable
        // cannot be read; the walk reaches pid 1 and gives up.
        assert_eq!(of(900_001, &table), None);
    }

    /// A staged process table: each process's pid, parent, executable and
    /// arguments, from the agent up.
    type Chain<'a, S> = &'a [(u32, u32, S, &'a str)];

    fn walked<S: AsRef<str>>(chain: Chain<'_, S>, ours: impl Fn(&Path) -> bool) -> Reach {
        let table: BTreeMap<u32, Process> = chain
            .iter()
            .map(|(pid, ppid, _, words)| {
                let argv = words.split(' ').map(str::to_owned).collect();
                (
                    *pid,
                    Process {
                        pid: *pid,
                        ppid: *ppid,
                        argv,
                    },
                )
            })
            .collect();
        let executables: BTreeMap<u32, PathBuf> = chain
            .iter()
            .map(|(pid, _, executable, _)| (*pid, PathBuf::from(executable.as_ref())))
            .collect();
        walk(
            chain[0].0,
            &table,
            |pid| executables.get(&pid).cloned(),
            ours,
        )
    }

    const TERMINAL: &str = "/System/Applications/Utilities/Terminal.app/Contents/MacOS/Terminal";
    const ITERM: &str = "/Applications/iTerm.app/Contents/MacOS/iTerm2";
    const APPLICATIONS: &str = "/Applications/AgentDocker.app/Contents/MacOS";
    const VERSIONED: &str = "/Users/me/.local/share/agentdocker/desktop/versions/0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f/AgentDocker.app/Contents/MacOS";

    /// `agentdocker codex` in a terminal: Codex under the CLI's wrapper
    /// under the CLI, in a login shell of `host`.
    fn codex_in(bundle: &str, host: &str) -> Vec<(u32, u32, String, &'static str)> {
        vec![
            (500, 501, "/opt/homebrew/bin/codex".to_owned(), "codex"),
            (
                501,
                502,
                format!("{bundle}/agentdocker"),
                "agentdocker codex",
            ),
            (
                502,
                503,
                format!("{bundle}/agentdocker"),
                "agentdocker codex",
            ),
            (503, 504, "/bin/zsh".to_owned(), "-zsh"),
            (504, 505, "/usr/bin/login".to_owned(), "login -pf me"),
            (505, 1, host.to_owned(), "Terminal"),
        ]
    }

    fn host_kind(reach: Reach) -> Option<(String, String)> {
        match reach {
            Reach::Host(host) => Some((host.kind, host.name)),
            _ => None,
        }
    }

    #[test]
    fn the_walk_passes_agentdockers_own_cli_to_the_real_terminal() {
        for bundle in [APPLICATIONS, VERSIONED] {
            let chain = codex_in(bundle, TERMINAL);
            assert_eq!(
                host_kind(walked(&chain, |_| false)),
                Some(("terminal".into(), "Terminal".into())),
                "{bundle}"
            );
            let chain = codex_in(bundle, ITERM);
            assert_eq!(
                host_kind(walked(&chain, |_| false)),
                Some(("iterm2".into(), "iTerm".into())),
                "{bundle}"
            );
        }
        // A copy under another name is passed over too, once the
        // filesystem says it is AgentDocker's: by its own helper's name it
        // would otherwise be an application of its own.
        let renamed = "/Applications/Copied.app/Contents/MacOS";
        let mut chain = codex_in(renamed, TERMINAL);
        chain[1].2 = format!("{renamed}/helper");
        chain[2].2 = format!("{renamed}/helper");
        assert_eq!(
            host_kind(walked(&chain, |bundle| bundle
                == Path::new("/Applications/Copied.app"))),
            Some(("terminal".into(), "Terminal".into()))
        );
        assert_eq!(
            host_kind(walked(&chain, |_| false)),
            Some(("app".into(), "Copied".into()))
        );
    }

    #[test]
    fn a_session_under_agentdockers_daemon_or_app_has_no_other_host() {
        // A managed session: the agent under its session owner, under
        // launchd.
        for owner in [
            (format!("{VERSIONED}/agentd"), "agentd --session-owner"),
            (
                format!("{VERSIONED}/agentdocker"),
                "agentdocker daemon run --session-owner",
            ),
        ] {
            let chain = [
                (600, 601, "/opt/homebrew/bin/codex", "codex"),
                (601, 1, owner.0.as_str(), owner.1),
            ];
            assert_eq!(
                walked(&chain, |_| false),
                Reach::AgentDocker { child: 600 },
                "{}",
                owner.1
            );
        }
        // The daemon started by hand in a terminal is still the end of the
        // walk: what it runs is not in that terminal's tab. The child
        // named is the one the daemon started, under any wrapper.
        let chain = [
            (700, 701, "/opt/homebrew/bin/codex", "codex"),
            (
                701,
                702,
                "/Users/me/.local/bin/agentdocker",
                "agentdocker codex",
            ),
            (702, 703, "/Users/me/src/target/debug/agentd", "agentd"),
            (703, 704, "/bin/zsh", "-zsh"),
            (704, 705, "/usr/bin/login", "login -pf me"),
            (705, 1, TERMINAL, "Terminal"),
        ];
        assert_eq!(walked(&chain, |_| false), Reach::AgentDocker { child: 701 });
        // Started by the app itself.
        let chain = [
            (800, 801, "/opt/homebrew/bin/codex", "codex"),
            (
                801,
                1,
                "/Applications/AgentDocker.app/Contents/MacOS/agentdocker-ui",
                "agentdocker-ui",
            ),
        ];
        assert_eq!(walked(&chain, |_| false), Reach::AgentDocker { child: 800 });
        // Only launchd above it: nothing says where it is.
        let chain = [
            (900, 901, "/opt/homebrew/bin/codex", "codex"),
            (901, 1, "/bin/zsh", "-zsh"),
        ];
        assert_eq!(walked(&chain, |_| false), Reach::Unknown);
    }

    #[test]
    fn a_bundle_holding_agentdockers_cli_is_agentdockers() {
        let root = tempfile::tempdir().unwrap();
        let bundle = root.path().join("Renamed.app");
        let macos = bundle.join("Contents/MacOS");
        std::fs::create_dir_all(&macos).unwrap();
        assert!(!ours(&bundle));
        std::fs::write(macos.join(crate::program::Role::Cli.file_name()), b"cli").unwrap();
        assert!(ours(&bundle));
        assert!(!ours(&root.path().join("Elsewhere.app")));
    }
}
