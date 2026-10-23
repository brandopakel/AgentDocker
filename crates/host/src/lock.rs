//! Advisory file locks (`flock` on Unix, `LockFileEx` on Windows).
//!
//! A lock is scoped to each daemon endpoint in its private state directory. The daemon holds it for its
//! lifetime, so a second daemon on the same socket exits at once; a client
//! that cannot connect takes it for an instant to tell "no daemon" (it got
//! the lock) from "one is starting" (it did not), and starts one only in
//! the first case.

use std::fs::File;
#[cfg(unix)]
use std::fs::OpenOptions;
use std::io;
#[cfg(unix)]
use std::os::unix::io::AsRawFd;
use std::path::Path;

/// Held for as long as the value lives; dropping it releases the lock.
#[derive(Debug)]
pub struct Lock {
    /// The locked file, until `into_fd` gives it away.
    file: Option<File>,
}

impl Lock {
    fn held(file: File) -> Self {
        Self { file: Some(file) }
    }

    /// Whether `path` still names the file this lock is held on. A lock
    /// is on a file, not a name: one taken on a file that was unlinked
    /// between its open and its lock excludes nobody who opens the path
    /// afterwards, so a holder whose file went takes the lock again.
    pub fn is_at(&self, path: &Path) -> io::Result<bool> {
        let file = self
            .file
            .as_ref()
            .expect("a lock keeps its file until it is dropped or given away");
        let ours = same_file::Handle::from_file(file.try_clone()?)?;
        match same_file::Handle::from_path(path) {
            Ok(there) => Ok(there == ours),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// The lock as a bare descriptor: `flock` locks travel with the open
    /// file, so a process that receives this descriptor holds the lock for
    /// as long as it keeps it open. Unlike dropping the lock, dropping the
    /// descriptor only closes it, so a copy another process received keeps
    /// the lock.
    #[cfg(unix)]
    pub fn into_fd(mut self) -> std::os::fd::OwnedFd {
        self.file
            .take()
            .expect("a lock keeps its file until it is dropped or given away")
            .into()
    }
}

impl Drop for Lock {
    /// Release the lock now. A `flock` lock belongs to the open file, which
    /// a child forked by any thread of this process shares until its exec
    /// closes the close-on-exec descriptor: closing ours alone would leave
    /// the lock held through that window, which a loaded machine stretches
    /// from microseconds to many milliseconds, and the next owner, even this
    /// process reopening the record it just let go of, would be refused.
    /// Unlocking explicitly releases it whoever else still shares the file.
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(file) = &self.file {
            // SAFETY: `flock` only reads the descriptor and the flags.
            unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
        }
    }
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
        return Ok(Some(Lock::held(file)));
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
    exclusive_file(crate::dirs::private_file(path, true, false)?)
}

#[cfg(windows)]
fn exclusive_file(file: File) -> io::Result<Option<Lock>> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::{
        Foundation::ERROR_LOCK_VIOLATION,
        Storage::FileSystem::{LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, LockFileEx},
        System::IO::OVERLAPPED,
    };
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
        return Ok(Some(Lock::held(file)));
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(ERROR_LOCK_VIOLATION as i32) {
        Ok(None)
    } else {
        Err(error)
    }
}

/// Hold a private lock file for a reader without waiting. Release
/// pins use shared readers so multiple binaries can use one retained version.
#[cfg(unix)]
pub fn try_shared(path: &Path) -> io::Result<Option<Lock>> {
    let file = crate::dirs::private_file(path, true, false)?;
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) };
    if rc == 0 {
        return Ok(Some(Lock::held(file)));
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        Some(code) if code == libc::EWOULDBLOCK || code == libc::EAGAIN => Ok(None),
        _ => Err(error),
    }
}

#[cfg(windows)]
pub fn try_shared(path: &Path) -> io::Result<Option<Lock>> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::{
        Foundation::ERROR_LOCK_VIOLATION,
        Storage::FileSystem::{LOCKFILE_FAIL_IMMEDIATELY, LockFileEx},
        System::IO::OVERLAPPED,
    };
    let file = crate::dirs::private_file(path, true, false)?;
    let locked = unsafe {
        let mut overlapped: OVERLAPPED = std::mem::zeroed();
        LockFileEx(
            file.as_raw_handle(),
            LOCKFILE_FAIL_IMMEDIATELY,
            0,
            1,
            0,
            &mut overlapped,
        )
    };
    if locked != 0 {
        return Ok(Some(Lock::held(file)));
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(ERROR_LOCK_VIOLATION as i32) {
        Ok(None)
    } else {
        Err(error)
    }
}

/// Test an existing pin without creating a new filesystem entry.
#[cfg(unix)]
pub fn try_exclusive_existing(path: &Path) -> io::Result<Option<Lock>> {
    let file = crate::dirs::private_file(path, false, false)?;
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        return Ok(Some(Lock::held(file)));
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        Some(code) if code == libc::EWOULDBLOCK || code == libc::EAGAIN => Ok(None),
        _ => Err(error),
    }
}

/// Test an existing pin without creating a new filesystem entry.
#[cfg(windows)]
pub fn try_exclusive_existing(path: &Path) -> io::Result<Option<Lock>> {
    exclusive_file(crate::dirs::private_file(path, false, false)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lock_knows_when_its_file_was_replaced_under_it() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("owner.lock");
        let held = try_exclusive(&path).unwrap().unwrap();
        assert!(held.is_at(&path).unwrap());
        std::fs::remove_file(&path).unwrap();
        assert!(!held.is_at(&path).unwrap(), "gone");
        let other = try_exclusive(&path).unwrap().unwrap();
        assert!(!held.is_at(&path).unwrap(), "another file by that name");
        assert!(other.is_at(&path).unwrap());
    }

    #[test]
    fn existing_lock_does_not_create_files_and_contends_with_readers() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("release.lock");
        assert_eq!(
            try_exclusive_existing(&path).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        assert!(!path.exists());
        let reader = try_shared(&path).unwrap().unwrap();
        assert!(try_exclusive_existing(&path).unwrap().is_none());
        drop(reader);
        assert!(try_exclusive_existing(&path).unwrap().is_some());
    }

    #[test]
    fn exclusive_until_dropped() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("agentd.lock");
        let held = try_exclusive(&path).unwrap().expect("first taker wins");
        // Another descriptor in the same process still contends: flock
        // locks are per open file description, not per process.
        assert!(try_exclusive(&path).unwrap().is_none());
        drop(held);
        // At once, even when a child another test forked (git, `true`)
        // still shares our descriptor until its exec closes it.
        assert!(try_exclusive(&path).unwrap().is_some());
    }

    /// A child forked by any thread shares every open file of this process,
    /// a lock's among them, until its exec. A lock dropped in that window,
    /// exclusive or shared, is released at once all the same, so whoever
    /// takes it next (the same process reopening a record it just let go
    /// of, say) is not refused for as long as the child takes to exec.
    #[cfg(unix)]
    #[test]
    fn dropping_releases_a_lock_a_forked_child_still_shares() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("owner.lock");
        type Take = fn(&Path) -> io::Result<Option<Lock>>;
        for take in [try_exclusive as Take, try_shared] {
            let held = take(&path).unwrap().expect("nobody else holds it");
            let child = ForkedHolder::fork();
            drop(held);
            let retaken = try_exclusive(&path).unwrap();
            drop(child);
            assert!(retaken.is_some(), "the forked child kept a dropped lock");
        }
        let given = try_exclusive(&path).unwrap().unwrap().into_fd();
        assert!(
            try_exclusive(&path).unwrap().is_none(),
            "a lock given away as a descriptor stays held by it"
        );
        drop(given);
    }

    /// A forked child holding a copy of every descriptor this process had
    /// open, as a child of `Command::spawn` does between its fork and its
    /// exec, until this is dropped (or for ten seconds at most).
    #[cfg(unix)]
    struct ForkedHolder {
        pid: libc::pid_t,
        release: Option<std::io::PipeWriter>,
    }

    #[cfg(unix)]
    impl ForkedHolder {
        fn fork() -> Self {
            use std::os::fd::AsRawFd;
            // Close-on-exec, so no program another thread starts meanwhile
            // can keep the child waiting.
            let (wait, release) = std::io::pipe().unwrap();
            let (wait_fd, release_fd) = (wait.as_raw_fd(), release.as_raw_fd());
            // SAFETY: the child only closes, polls and exits, each
            // async-signal-safe, as a child of a threaded process must.
            let pid = unsafe { libc::fork() };
            if pid == 0 {
                // SAFETY: as above; `ready` outlives the poll on it.
                unsafe {
                    libc::close(release_fd);
                    let mut ready = libc::pollfd {
                        fd: wait_fd,
                        events: libc::POLLIN,
                        revents: 0,
                    };
                    libc::poll(&mut ready, 1, 10_000);
                    libc::_exit(0);
                }
            }
            assert!(pid > 0, "fork: {}", std::io::Error::last_os_error());
            drop(wait);
            Self {
                pid,
                release: Some(release),
            }
        }
    }

    #[cfg(unix)]
    impl Drop for ForkedHolder {
        fn drop(&mut self) {
            // The end of the pipe ends the child's wait; it is ours to reap.
            drop(self.release.take());
            // SAFETY: waits for this holder's own child only.
            unsafe { libc::waitpid(self.pid, std::ptr::null_mut(), 0) };
        }
    }
}
