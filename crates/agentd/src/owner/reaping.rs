//! Linux session owners reap their own orphaned process-group descendants.
//!
//! A zombie still makes kill(-pgid, 0) succeed. Letting an external init reap
//! it can therefore hold leases indefinitely after every writer has exited.

use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
use nix::unistd::Pid;

/// Enable adoption only in the dedicated owner process, before child launch.
/// This setting is not inherited across fork and does not alter the daemon.
pub(super) fn enable() -> std::io::Result<()> {
    // SAFETY: this prctl operation takes an integer flag and no pointers.
    if unsafe { nix::libc::prctl(nix::libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) } == -1 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Consume only waitable children of this process in the already-owned group.
/// The original child's exit status must have been collected before this call.
pub(super) fn group(group: u32) -> std::io::Result<()> {
    let group = i32::try_from(group)
        .ok()
        .filter(|group| *group > 0)
        .ok_or_else(|| std::io::Error::other("invalid owned process group"))?;
    loop {
        match waitpid(Pid::from_raw(-group), Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::StillAlive) | Err(nix::errno::Errno::ECHILD) => return Ok(()),
            Ok(_) | Err(nix::errno::Errno::EINTR) => {}
            Err(error) => return Err(error.into()),
        }
    }
}
