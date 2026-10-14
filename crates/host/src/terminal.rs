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

#[cfg(all(test, unix))]
mod tests {
    use super::*;

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
