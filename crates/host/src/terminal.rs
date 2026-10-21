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

/// What a full-screen program turns on in its terminal and undoes on a
/// clean exit: mouse reporting (every mode, every encoding), focus and
/// bracketed-paste reports, application cursor keys and keypad, the
/// keyboard protocol it pushed, the alternate screen, a hidden cursor and
/// its colours.
const RESTORE_MODES: &[u8] = b"\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1005l\x1b[?1006l\x1b[?1015l\x1b[?1004l\x1b[?2004l\x1b[?1l\x1b>\x1b[<u\x1b[?1049l\x1b[?25h\x1b[0m";

/// Undo, on `tty`, what a full-screen program ended by a signal had no
/// chance to: without it a shell gets its terminal back still reporting
/// every mouse move as typed text. Written as output to the device, never
/// as input, so the shell reading it sees nothing. `tty` is a device path.
#[cfg(unix)]
pub fn restore_modes(tty: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    if !tty.starts_with("/dev/") || tty.contains("..") {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not a terminal device",
        ));
    }
    let mut device = std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NOCTTY | libc::O_NONBLOCK)
        .open(tty)?;
    device.write_all(RESTORE_MODES)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn only_a_device_has_its_modes_restored() {
        for path in ["", "relative", "/etc/passwd/../dev/x", "/tmp/not-a-device"] {
            assert!(restore_modes(path).is_err(), "{path}");
        }
        // Writing to the null device stands in for a terminal.
        restore_modes("/dev/null").unwrap();
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
