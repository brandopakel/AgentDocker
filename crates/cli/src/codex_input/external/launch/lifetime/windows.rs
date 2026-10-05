//! Make owner death retire its descendants without closing the caller's console.
use std::{
    io,
    os::windows::io::{AsRawHandle, FromRawHandle, IntoRawHandle, OwnedHandle},
    ptr::null,
};
use windows_sys::Win32::System::{
    JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    },
    Threading::GetCurrentProcess,
};

/// Called once by the authenticated internal owner, before spawning anything.
/// Its children inherit this job, including nested jobs, with no breakaway.
pub(crate) fn contain_owner_until_process_exit() -> io::Result<()> {
    // SAFETY: null security attributes create a non-inheritable handle; the
    // unnamed job cannot be opened by children through a shared public name.
    let raw = unsafe { CreateJobObjectW(null(), null()) };
    if raw.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: CreateJobObjectW returned a fresh owned handle.
    let job = unsafe { OwnedHandle::from_raw_handle(raw) };
    // SAFETY: the Win32 limits structure contains only numeric fields.
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    // SAFETY: the job and correctly sized initialized limits outlive this call.
    if unsafe {
        SetInformationJobObject(
            job.as_raw_handle(),
            JobObjectExtendedLimitInformation,
            std::ptr::addr_of!(limits).cast(),
            std::mem::size_of_val(&limits) as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the pseudo handle names this exact process. Assignment happens
    // before any provider/receiver spawn, so no descendant can escape a race.
    if unsafe { AssignProcessToJobObject(job.as_raw_handle(), GetCurrentProcess()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // Keep exactly one non-inheritable handle until this short-lived owner
    // process exits. Closing it in Rust Drop would terminate the owner itself
    // before it can return its normal status. Windows closes it on every exit,
    // including TerminateProcess, and kills any surviving descendants. The
    // front end stays outside the job; its death still allows ordinary EOF
    // cleanup and capability revocation by this owner.
    let _process_lifetime_handle = job.into_raw_handle();
    Ok(())
}
