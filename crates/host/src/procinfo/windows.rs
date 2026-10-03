//! Native Windows process snapshots and full-resolution process identities.
use std::io;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
use windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED;
use windows_sys::Win32::{
    Foundation::{FILETIME, WAIT_TIMEOUT},
    System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
        PROCESS_TERMINATE, QueryFullProcessImageNameW, TerminateProcess, WaitForSingleObject,
    },
};

/// Read the loaded image through an owned process handle, never argv or PATH.
/// The caller still compares the expected process birth around this lookup.
pub(super) fn executable_path_of(pid: u32) -> io::Result<PathBuf> {
    // SAFETY: OpenProcess returns null or an owned handle, immediately adopted
    // by RAII. QueryFullProcessImageNameW receives a live handle and the exact
    // capacity of its writable UTF-16 buffer; it retains neither argument.
    let raw = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            0,
            pid,
        )
    };
    if raw.is_null() {
        return Err(io::Error::last_os_error());
    }
    let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
    let gone = || io::Error::new(io::ErrorKind::NotFound, "the process has already exited");
    if start_time_of(&handle).is_none() {
        return Err(gone());
    }
    // Windows extended paths have at most 32,767 UTF-16 units plus NUL.
    let mut buffer = vec![0_u16; 32_768];
    let mut length = buffer.len() as u32;
    if unsafe {
        QueryFullProcessImageNameW(handle.as_raw_handle(), 0, buffer.as_mut_ptr(), &mut length)
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let length = length as usize;
    if length == 0 || length >= buffer.len() || buffer[length] != 0 || buffer[..length].contains(&0)
    {
        return Err(io::Error::other("invalid kernel executable path"));
    }
    let path = PathBuf::from(std::ffi::OsString::from_wide(&buffer[..length]));
    if !path.is_absolute() {
        return Err(io::Error::other("kernel executable path is not absolute"));
    }
    if start_time_of(&handle).is_none() {
        return Err(gone());
    }
    Ok(path)
}

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

/// Whether a process with this pid exists and has not exited: the handle
/// opens and the process is not signalled. Like `kill(pid, 0)` on Unix, an
/// access refusal is still a yes — the process is there, just not ours.
pub(super) fn alive(pid: u32) -> bool {
    // SAFETY: OpenProcess returns an owned handle or null; the handle is
    // owned by RAII at once and WaitForSingleObject only reads it.
    let handle = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            0,
            pid,
        )
    };
    if handle.is_null() {
        return std::io::Error::last_os_error().raw_os_error() == Some(ERROR_ACCESS_DENIED as i32);
    }
    let handle = unsafe { OwnedHandle::from_raw_handle(handle) };
    let signalled = unsafe { WaitForSingleObject(handle.as_raw_handle(), 0) };
    signalled == WAIT_TIMEOUT
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
    start_time_of(&handle)
}

/// End the process that `pid` names only if it is the one born at
/// `started_at`: the birth is read from the very handle that is
/// terminated, so a recycled pid cannot be ended by mistake between a
/// check and the act. Windows has no gentle signal a process is obliged
/// to hear; `force` is recorded by the caller and both end the process.
pub(super) fn end(pid: u32, started_at: DateTime<Utc>, _force: bool) -> io::Result<()> {
    // SAFETY: as in `start_time`; TerminateProcess only reads the handle.
    let handle = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE | PROCESS_TERMINATE,
            0,
            pid,
        )
    };
    if handle.is_null() {
        return Err(io::Error::last_os_error());
    }
    let handle = unsafe { OwnedHandle::from_raw_handle(handle) };
    match start_time_of(&handle) {
        Some(born) if born == started_at => {}
        Some(_) => {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "the pid now belongs to a different process",
            ));
        }
        None => {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "the process has already exited",
            ));
        }
    }
    if unsafe { TerminateProcess(handle.as_raw_handle(), 1) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// The birth time of an open, still-running process.
fn start_time_of(handle: &OwnedHandle) -> Option<DateTime<Utc>> {
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
    #[ignore = "owned child for the native executable-path regression"]
    fn executable_lookup_child() {
        use std::io::Read;
        if std::env::var("AGENTDOCKER_IMAGE_PATH_CHILD").as_deref() == Ok("1") {
            std::io::stdin().read_exact(&mut [0_u8; 1]).unwrap();
        }
    }

    #[test]
    fn kernel_image_lookup_preserves_unicode_and_refuses_exited_processes() {
        use std::io::Write;
        use std::process::{Child, Command, Stdio};
        use std::time::{Duration, Instant};

        struct OwnedChild(Child);
        impl Drop for OwnedChild {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let this = std::env::current_exe().unwrap();
        assert_eq!(
            super::super::executable_path()
                .unwrap()
                .canonicalize()
                .unwrap(),
            this.canonicalize().unwrap()
        );
        assert!(super::super::executable_path_of(0).is_err());
        assert!(super::super::executable_path_of(u32::MAX).is_err());
        let temp = tempfile::Builder::new()
            .prefix("image lookup ü ")
            .tempdir()
            .unwrap();
        let image = temp.path().join("provider image ü.exe");
        std::fs::copy(this, &image).unwrap();
        let mut child = OwnedChild(
            Command::new(&image)
                .args([
                    "--exact",
                    "procinfo::imp::tests::executable_lookup_child",
                    "--ignored",
                ])
                .env("AGENTDOCKER_IMAGE_PATH_CHILD", "1")
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        assert!(child.0.try_wait().unwrap().is_none());
        assert_eq!(
            super::super::executable_path_of(child.0.id())
                .unwrap()
                .canonicalize()
                .unwrap(),
            image.canonicalize().unwrap()
        );
        child.0.stdin.take().unwrap().write_all(b"x").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            assert!(
                Instant::now() < deadline,
                "owned image lookup child did not exit"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(super::super::executable_path_of(child.0.id()).is_err());
    }

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
