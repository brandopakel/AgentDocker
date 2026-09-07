//! Advisory file locks (`flock`).
//!
//! One lock lives beside each daemon socket. The daemon holds it for its
//! lifetime, so a second daemon on the same socket exits at once; a client
//! that cannot connect takes it for an instant to tell "no daemon" (it got
//! the lock) from "one is starting" (it did not), and starts one only in
//! the first case.

use std::fs::{File, OpenOptions};
use std::io;
#[cfg(unix)]
use std::os::unix::io::AsRawFd;
use std::path::Path;

/// Held for as long as the value lives; dropping it releases the lock.
#[derive(Debug)]
pub struct Lock {
    _file: File,
}

/// Take the exclusive lock on `path` without waiting. `Ok(None)` means
/// another process holds it.
#[cfg(unix)]
pub fn try_exclusive(path: &Path) -> io::Result<Option<Lock>> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    // SAFETY: `flock` only reads the descriptor and the flags.
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        return Ok(Some(Lock { _file: file }));
    }
    let err = io::Error::last_os_error();
    match err.raw_os_error() {
        Some(code) if code == libc::EWOULDBLOCK || code == libc::EAGAIN => Ok(None),
        _ => Err(err),
    }
}

/// Windows byte-range locks are released with their owning file handle. A
/// separate open in the same process still contends, matching the Unix API.
#[cfg(windows)]
pub fn try_exclusive(path: &Path) -> io::Result<Option<Lock>> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::{
        Foundation::ERROR_LOCK_VIOLATION,
        Storage::FileSystem::{LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, LockFileEx},
        System::IO::OVERLAPPED,
    };
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    // SAFETY: the synchronous file handle stays live; OVERLAPPED describes
    // byte zero and is valid for this nonblocking, synchronous lock call.
    let locked = unsafe {
        let mut overlapped: OVERLAPPED = std::mem::zeroed();
        LockFileEx(
            file.as_raw_handle(),
            LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
            0,
            1,
            0,
            &mut overlapped,
        )
    };
    if locked != 0 {
        return Ok(Some(Lock { _file: file }));
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(ERROR_LOCK_VIOLATION as i32) {
        Ok(None)
    } else {
        Err(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exclusive_until_dropped() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("agentd.lock");
        let held = try_exclusive(&path).unwrap().expect("first taker wins");
        // Another descriptor in the same process still contends: flock
        // locks are per open file description, not per process.
        assert!(try_exclusive(&path).unwrap().is_none());
        drop(held);
        // A child forked by another test in this process (git, `true`)
        // inherits our descriptor until it execs and CLOEXEC closes it, so
        // the release can lag by that window. Nothing here bypasses the
        // lock; wait it out rather than race it.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if try_exclusive(&path).unwrap().is_some() {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "lock never released");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}
