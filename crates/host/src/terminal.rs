//! Which application a process's terminal is in, read from its ancestry.
//!
//! The conventions — which executable is which host — live in
//! `agentdocker_core::terminal`; this is the I/O: the process table, each
//! ancestor's executable and the process's own terminal device.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use agentdocker_core::terminal::{Host, classify, device};

use crate::procinfo::{self, Process};

/// The host of `pid`'s terminal: its nearest ancestor that is a terminal
/// application or an application bundle, with `pid`'s own terminal
/// device. A multiplexer server is not a host and usually has no parent
/// but launchd, so an agent inside one has none here; it is reached
/// through the multiplexer's client instead. Walking stops at pid 1, at a
/// missing parent, or after enough steps that a cycle cannot spin.
pub fn of(pid: u32, by_pid: &BTreeMap<u32, Process>) -> Option<Host> {
    let mut current = by_pid.get(&pid)?.ppid;
    for _ in 0..32 {
        if current <= 1 {
            return None;
        }
        if let Some(mut host) = procinfo::executable_path_of(current)
            .ok()
            .and_then(|executable| classify(&executable))
        {
            host.tty = device_of(pid);
            return Some(host);
        }
        current = by_pid.get(&current)?.ppid;
    }
    None
}

/// [`of`], reading the process table itself.
pub fn of_process(pid: u32) -> Option<Host> {
    let by_pid: BTreeMap<u32, Process> = procinfo::processes()
        .ok()?
        .into_iter()
        .map(|process| (process.pid, process))
        .collect();
    of(pid, &by_pid)
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
}
