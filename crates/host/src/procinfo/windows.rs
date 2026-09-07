//! Native Windows process snapshots and full-resolution process identities.
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
use windows_sys::Win32::{
    Foundation::{FILETIME, WAIT_TIMEOUT},
    System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
        WaitForSingleObject,
    },
};

/// Discover PIDs first, then verify tokens before requesting command/cwd data.
/// sysinfo's optional cached user field is not the authority for ownership.
fn snapshot(pid: Option<u32>) -> io::Result<(System, Vec<Pid>)> {
    let me = Pid::from_u32(std::process::id());
    let owner = crate::dirs::current_sid()?;
    // The dependency clears an entry's refreshed bit while retiring missing
    // PIDs. Passing our PID twice removes the live entry on the second visit.
    let mut selected = vec![me];
    if let Some(pid) = pid.map(Pid::from_u32).filter(|pid| *pid != me) {
        selected.push(pid);
    }
    let mut system = System::new();
    system.refresh_processes_specifics(
        if pid.is_some() {
            ProcessesToUpdate::Some(&selected)
        } else {
            ProcessesToUpdate::All
        },
        true,
        ProcessRefreshKind::nothing(),
    );
    let owned: Vec<_> = system
        .processes()
        .keys()
        .copied()
        .filter(|pid| crate::dirs::process_sid(pid.as_u32()).is_ok_and(|sid| sid == owner))
        .collect();
    if !owned.contains(&me) {
        return Err(io::Error::other(
            "Windows process scan could not verify the current user",
        ));
    }
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&owned),
        true,
        ProcessRefreshKind::nothing()
            .with_cmd(UpdateKind::Always)
            .with_cwd(UpdateKind::Always)
            .with_exe(UpdateKind::Always),
    );
    Ok((system, owned))
}

fn row(process: &sysinfo::Process) -> Option<super::Process> {
    if process.cmd().is_empty() {
        return None;
    }
    Some(super::Process {
        pid: process.pid().as_u32(),
        ppid: process.parent().map_or(0, Pid::as_u32),
        argv: process
            .cmd()
            .iter()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect(),
    })
}

pub(super) fn processes() -> io::Result<Vec<super::Process>> {
    let (system, owned) = snapshot(None)?;
    Ok(system
        .processes()
        .values()
        .filter(|process| owned.contains(&process.pid()))
        .filter_map(row)
        .collect())
}

pub(super) fn inspect(pid: u32) -> Option<super::Process> {
    let (system, owned) = snapshot(Some(pid)).ok()?;
    let process = system.process(Pid::from_u32(pid))?;
    (owned.contains(&process.pid()))
        .then(|| row(process))
        .flatten()
}

pub(super) fn cwd(pid: u32) -> Option<PathBuf> {
    let (system, owned) = snapshot(Some(pid)).ok()?;
    let process = system.process(Pid::from_u32(pid))?;
    (owned.contains(&process.pid()))
        .then(|| process.cwd().map(PathBuf::from))
        .flatten()
}

pub(super) fn start_time(pid: u32) -> Option<DateTime<Utc>> {
    // SAFETY: OpenProcess returns an owned handle or null; the handle is
    // immediately owned by RAII, and all GetProcessTimes outputs are valid.
    let handle = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            0,
            pid,
        )
    };
    if handle.is_null() {
        return None;
    }
    let handle = unsafe { OwnedHandle::from_raw_handle(handle) };
    if unsafe { WaitForSingleObject(handle.as_raw_handle(), 0) } != WAIT_TIMEOUT {
        return None;
    }
    let mut created = FILETIME::default();
    let mut exited = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    if unsafe {
        GetProcessTimes(
            handle.as_raw_handle(),
            &mut created,
            &mut exited,
            &mut kernel,
            &mut user,
        )
    } == 0
    {
        return None;
    }
    // FILETIME is in 100 ns ticks since 1601. Keep its precision: rounding to
    // seconds could mistake a recycled PID for the original process.
    let ticks = ((created.dwHighDateTime as u64) << 32) | created.dwLowDateTime as u64;
    let unix_ticks = ticks.checked_sub(116_444_736_000_000_000)?;
    DateTime::from_timestamp(
        (unix_ticks / 10_000_000).try_into().ok()?,
        ((unix_ticks % 10_000_000) * 100) as u32,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_process_identity_and_arguments_are_available_without_a_shell() {
        let pid = std::process::id();
        let first = start_time(pid).unwrap();
        assert_eq!(start_time(pid), Some(first));
        assert!(first <= Utc::now());
        assert_eq!(
            crate::dirs::process_sid(pid).unwrap(),
            crate::dirs::current_sid().unwrap()
        );
        assert!(crate::dirs::process_sid(u32::MAX).is_err());
        let (system, owned) = snapshot(Some(pid)).expect("same-user snapshot");
        let process = system
            .process(Pid::from_u32(pid))
            .expect("own PID in snapshot");
        assert!(owned.contains(&process.pid()));
        assert!(system.processes().values().all(|p| p.environ().is_empty()));
        assert!(
            !process.cmd().is_empty(),
            "own process fields: owner={}, executable={}, cwd={}",
            process.user_id().is_some(),
            process.exe().is_some(),
            process.cwd().is_some()
        );
        assert_eq!(inspect(pid).expect("own command line available").pid, pid);
        assert!(
            processes()
                .unwrap()
                .iter()
                .any(|process| process.pid == pid)
        );
        assert_eq!(
            cwd(pid).unwrap().canonicalize().unwrap(),
            std::env::current_dir().unwrap().canonicalize().unwrap()
        );
        assert!(start_time(u32::MAX).is_none());
    }
}
