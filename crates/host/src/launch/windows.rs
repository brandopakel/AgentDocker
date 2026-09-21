//! Hold a native command before it runs until its owner durably records its
//! PID — the Windows half of the launch gate.
//!
//! The Unix twin forks and holds the child before `exec`. Windows creates
//! the process suspended (`CREATE_SUSPENDED`): its pid and its birth time
//! are readable from the handle while its first thread has never run,
//! `activate` is `ResumeThread`, and a refusal terminates a process that
//! never executed an instruction. The child is bound to its pseudo console
//! (or to explicit stdio pipes) through the startup attribute list, and
//! assigned to a Job Object before it resumes, so "its process group" is a
//! certainty rather than a race: stopping the job stops every descendant,
//! and the job's active count says whether any is left.
//!
//! Rust's `Command` cannot carry the attribute list on stable, so this is
//! a direct `CreateProcessW`, with the program resolved the way `Command`
//! resolves it, the command line quoted by the standard rules, and an
//! environment block built from this process's environment and the
//! command's overrides.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{
    HANDLE, HANDLE_FLAG_INHERIT, STILL_ACTIVE, SetHandleInformation, WAIT_OBJECT_0,
};
use windows_sys::Win32::System::Console::HPCON;
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
    QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DeleteProcThreadAttributeList,
    EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess, InitializeProcThreadAttributeList,
    LPPROC_THREAD_ATTRIBUTE_LIST, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
    PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, PROCESS_INFORMATION, ResumeThread, STARTF_USESTDHANDLES,
    STARTUPINFOEXW, STARTUPINFOW, TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject,
};

/// The exit code a process ended by its owner reports; the Unix twin
/// reports the signal, and `describe_exit` knows both.
pub const ENDED_BY_OWNER: u32 = 0xC000_013A; // STATUS_CONTROL_C_EXIT

/// A created, suspended process: nothing has run.
pub struct Pending {
    pub pid: u32,
    process: OwnedHandle,
    thread: Option<OwnedHandle>,
    job: OwnedHandle,
    stdout: Option<File>,
    stderr: Option<File>,
}

/// A running child and its job. Dropping it ends the job — every process
/// in it — unless it was disowned.
pub struct OwnedChild {
    pid: u32,
    process: OwnedHandle,
    job: OwnedHandle,
    stdout: Option<File>,
    stderr: Option<File>,
    reaped: bool,
}

impl OwnedChild {
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        use std::os::windows::process::ExitStatusExt;
        let mut code = 0;
        // SAFETY: the process handle is live; the code is written on success.
        if unsafe { GetExitCodeProcess(self.process.as_raw_handle() as HANDLE, &mut code) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if code == STILL_ACTIVE as u32 {
            // A process may legitimately exit with 259; the handle says.
            // SAFETY: a zero wait on a live handle.
            let signalled =
                unsafe { WaitForSingleObject(self.process.as_raw_handle() as HANDLE, 0) }
                    == WAIT_OBJECT_0;
            if !signalled {
                return Ok(None);
            }
        }
        self.reaped = true;
        Ok(Some(ExitStatus::from_raw(code)))
    }

    /// The child's standard output, when it was given pipes rather than a
    /// console. Taken once.
    pub fn take_stdout(&mut self) -> Option<File> {
        self.stdout.take()
    }

    pub fn take_stderr(&mut self) -> Option<File> {
        self.stderr.take()
    }

    pub fn id(&self) -> u32 {
        self.pid
    }

    /// Does any process of the child's job still exist? The count trails
    /// an exit by a moment — a process is signalled before its job
    /// membership is torn down — so a caller that has just seen the exit
    /// asks again rather than concluding from one answer.
    pub fn group_exists(&self) -> bool {
        job_active_processes(&self.job) > 0
    }

    /// End every process in the child's job now. There is no graceful
    /// signal on Windows; a console child is asked first by a Ctrl-C typed
    /// into its console, and this is the end after the grace period.
    pub fn end_group(&self) -> io::Result<()> {
        // SAFETY: the job handle is live for as long as `self` is.
        if unsafe { TerminateJobObject(self.job.as_raw_handle() as HANDLE, ENDED_BY_OWNER) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// The child's job as a handle of its own, for a holder that ends or
    /// inspects the group while the child is being waited on elsewhere.
    pub fn job(&self) -> io::Result<Job> {
        Ok(Job(self.job.try_clone()?))
    }

    /// Give up ownership: the process keeps running and its job is not
    /// ended when this drops. See the Unix twin for why the default is
    /// the opposite.
    /// Clearing the owner's crash protection must succeed before ownership
    /// is surrendered. On error, this still drops as an owned child.
    pub fn disown(mut self) -> io::Result<u32> {
        kill_on_close(&self.job, false)?;
        self.reaped = true;
        Ok(self.pid)
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if !self.reaped {
            let _ = self.end_group();
            // SAFETY: waiting on our own live handle, briefly, so the
            // caller's next step sees the process gone.
            unsafe { WaitForSingleObject(self.process.as_raw_handle() as HANDLE, 5_000) };
        }
    }
}

impl Pending {
    /// Acquire supervision ownership while the child still cannot run.
    pub fn job(&self) -> io::Result<Job> {
        Ok(Job(self.job.try_clone()?))
    }

    /// Let the process run, now that the record is durable.
    pub fn activate(mut self) -> io::Result<OwnedChild> {
        // Clone before ResumeThread. Failure must leave the launch gate
        // closed so Pending::drop still terminates the suspended child.
        let process = self.process.try_clone()?;
        let job = self.job.try_clone()?;
        let thread = self
            .thread
            .take()
            .expect("thread retained until activation");
        // SAFETY: the primary thread's handle, resumed exactly once; the
        // previous suspend count is 1 for a process created suspended.
        if unsafe { ResumeThread(thread.as_raw_handle() as HANDLE) } == u32::MAX {
            let error = io::Error::last_os_error();
            // Still suspended and now nobody's: end it rather than leak it.
            // SAFETY: our own live handles.
            unsafe {
                TerminateJobObject(self.job.as_raw_handle() as HANDLE, ENDED_BY_OWNER);
                TerminateProcess(self.process.as_raw_handle() as HANDLE, ENDED_BY_OWNER);
            }
            return Err(error);
        }
        Ok(OwnedChild {
            pid: self.pid,
            process,
            job,
            stdout: self.stdout.take(),
            stderr: self.stderr.take(),
            reaped: false,
        })
    }

    /// The process handle, for reading its birth time before activation.
    pub fn process_handle(&self) -> HANDLE {
        self.process.as_raw_handle() as HANDLE
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        if self.thread.is_some() {
            // Never activated: end a process that never ran an instruction,
            // and its job with it.
            // SAFETY: our own live handles.
            unsafe {
                TerminateJobObject(self.job.as_raw_handle() as HANDLE, ENDED_BY_OWNER);
                TerminateProcess(self.process.as_raw_handle() as HANDLE, ENDED_BY_OWNER);
            }
        }
    }
}

/// Prepare a command with pipes for its standard output and error and no
/// standard input; the Unix twin's default shape.
pub fn prepare(command: Command) -> io::Result<Pending> {
    prepare_with(command, None)
}

/// Prepare a command bound to `console` when one is given (its stdio is
/// the console's), else with stdout/stderr pipes. The process is created
/// suspended and assigned to a fresh job before this returns.
///
/// Not in a new process group of its own: `CREATE_NEW_PROCESS_GROUP`
/// makes a process ignore Ctrl-C, and a child inherits its parent's
/// ignoring, so the Ctrl-C typed into a console child's console as the
/// polite stop would reach nothing. This process was itself started
/// detached in its own group (see `command::detach`), so it stops
/// ignoring Ctrl-C here, before the child exists, and the child inherits
/// that. The job is the group that is ended.
pub fn prepare_with(command: Command, console: Option<HPCON>) -> io::Result<Pending> {
    // SAFETY: a null handler with `false` clears this process's own
    // ignore-Ctrl-C attribute; nothing is registered.
    unsafe { windows_sys::Win32::System::Console::SetConsoleCtrlHandler(None, 0) };
    let program = resolve_program(&command)?;
    // A batch launcher (an npm-installed provider is one) cannot be
    // started by itself: cmd.exe runs it, with the standard library's own
    // batch command line and argument rules, so what the child sees is
    // what `Command` would have given it.
    let batch = crate::command::is_batch_launcher(&program);
    let (program, line) = if batch {
        (
            command_prompt()?,
            batch_command_line(&program, command.get_args())?,
        )
    } else {
        let mut line = quoted(program.as_os_str());
        for arg in command.get_args() {
            line.push(' ');
            line.push_str(&quoted(arg));
        }
        (program, line)
    };
    let mut line_wide: Vec<u16> = OsStr::new(&line).encode_wide().chain([0]).collect();
    let program_wide: Vec<u16> = program.as_os_str().encode_wide().chain([0]).collect();
    let environment = environment_block(&command);
    // cmd.exe refuses a verbatim working directory ("UNC paths are not
    // supported", then the Windows directory instead — the first runner
    // showed it), and a canonical Windows path is verbatim. A batch
    // launcher gets the plain directory, by the same round-trip rule as
    // its script; any other program takes the directory as given.
    let workdir: Option<OsString> = match command.get_current_dir() {
        Some(dir) if batch => Some(batch_workdir(dir)?),
        Some(dir) => Some(dir.as_os_str().to_os_string()),
        None => None,
    };
    let workdir_wide: Option<Vec<u16>> = workdir.map(|dir| dir.encode_wide().chain([0]).collect());

    // The job the child belongs to from before its first instruction.
    // SAFETY: a fresh, unnamed job with default security.
    let job = unsafe { CreateJobObjectW(null(), null()) };
    if job.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a handle this process owns from here on.
    let job = unsafe { OwnedHandle::from_raw_handle(job) };
    // The durable session owner holds this job, not the daemon. A daemon
    // crash leaves it intact; an owner crash closes every handle and ends
    // the whole tree even when no Rust destructor can run.
    kill_on_close(&job, true)?;

    // What the child inherits: with a console, nothing (its stdio is the
    // console's); with pipes, exactly its two pipe ends.
    let mut stdio: Option<(File, OwnedHandle, OwnedHandle, File, File)> = None;
    let mut inherited: Vec<HANDLE> = Vec::new();
    if console.is_none() {
        let null_input = File::open("NUL")?;
        let (out_read, out_write) = pipe()?;
        let (err_read, err_write) = pipe()?;
        for handle in [
            null_input.as_raw_handle() as HANDLE,
            out_write.as_raw_handle() as HANDLE,
            err_write.as_raw_handle() as HANDLE,
        ] {
            // SAFETY: setting the inherit flag on a handle we own.
            if unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) }
                == 0
            {
                return Err(io::Error::last_os_error());
            }
            inherited.push(handle);
        }
        stdio = Some((
            null_input,
            out_write,
            err_write,
            File::from(out_read),
            File::from(err_read),
        ));
    }

    let attributes = AttributeList::new(1)?;
    // The attribute takes the console handle by value: a pointer-sized
    // value the list copies, so the console itself is what is bound.
    let console_value: HPCON = console.unwrap_or(0);
    if console.is_some() {
        // SAFETY: the value outlives the list; the attribute copies a
        // pointer-sized handle.
        if unsafe {
            UpdateProcThreadAttribute(
                attributes.list,
                0,
                PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                console_value as *const core::ffi::c_void,
                std::mem::size_of::<HPCON>(),
                null_mut(),
                null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
    } else {
        // SAFETY: `inherited` outlives the list and holds live handles.
        if unsafe {
            UpdateProcThreadAttribute(
                attributes.list,
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                inherited.as_ptr().cast(),
                inherited.len() * std::mem::size_of::<HANDLE>(),
                null_mut(),
                null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
    }

    let mut startup = STARTUPINFOEXW {
        StartupInfo: STARTUPINFOW {
            cb: std::mem::size_of::<STARTUPINFOEXW>() as u32,
            lpReserved: null_mut(),
            lpDesktop: null_mut(),
            lpTitle: null_mut(),
            dwX: 0,
            dwY: 0,
            dwXSize: 0,
            dwYSize: 0,
            dwXCountChars: 0,
            dwYCountChars: 0,
            dwFillAttribute: 0,
            // Explicit null standard handles let ConPTY supply them. With
            // this flag absent, Windows can copy the owner's redirected
            // NUL handles even though bInheritHandles is false: input then
            // sees EOF and output disappears instead of reaching ConPTY.
            dwFlags: STARTF_USESTDHANDLES,
            wShowWindow: 0,
            cbReserved2: 0,
            lpReserved2: null_mut(),
            hStdInput: null_mut(),
            hStdOutput: null_mut(),
            hStdError: null_mut(),
        },
        lpAttributeList: attributes.list,
    };
    let inherit_handles = if let Some((null_input, out_write, err_write, _, _)) = &stdio {
        startup.StartupInfo.hStdInput = null_input.as_raw_handle() as HANDLE;
        startup.StartupInfo.hStdOutput = out_write.as_raw_handle() as HANDLE;
        startup.StartupInfo.hStdError = err_write.as_raw_handle() as HANDLE;
        1
    } else {
        0
    };
    let mut information = PROCESS_INFORMATION {
        hProcess: null_mut(),
        hThread: null_mut(),
        dwProcessId: 0,
        dwThreadId: 0,
    };
    // SAFETY: every pointer is to a live, nul-terminated buffer or a
    // struct that outlives the call; the command line is mutable as the
    // API requires; the information struct is written on success.
    let created = unsafe {
        CreateProcessW(
            program_wide.as_ptr(),
            line_wide.as_mut_ptr(),
            null(),
            null(),
            inherit_handles,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT,
            environment.as_ptr().cast(),
            workdir_wide.as_ref().map_or(null(), |dir| dir.as_ptr()),
            &startup.StartupInfo,
            &mut information,
        )
    };
    if created == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fresh handles from a successful creation.
    let process = unsafe { OwnedHandle::from_raw_handle(information.hProcess) };
    let thread = unsafe { OwnedHandle::from_raw_handle(information.hThread) };
    // In the job before it can run a single instruction.
    // SAFETY: both handles are live.
    if unsafe {
        AssignProcessToJobObject(
            job.as_raw_handle() as HANDLE,
            process.as_raw_handle() as HANDLE,
        )
    } == 0
    {
        let error = io::Error::last_os_error();
        // SAFETY: ending a suspended process we created.
        unsafe { TerminateProcess(process.as_raw_handle() as HANDLE, ENDED_BY_OWNER) };
        return Err(error);
    }
    // The child's ends close here, so its exit ends the pipes.
    let (stdout, stderr) = match stdio {
        Some((_null_input, _out_write, _err_write, out_read, err_read)) => {
            (Some(out_read), Some(err_read))
        }
        None => (None, None),
    };
    Ok(Pending {
        pid: information.dwProcessId,
        process,
        thread: Some(thread),
        job,
        stdout,
        stderr,
    })
}

/// A child's job, held apart from the child.
pub struct Job(OwnedHandle);

fn kill_on_close(job: &OwnedHandle, enabled: bool) -> io::Result<()> {
    // SAFETY: the Win32 structure is entirely numeric fields; zero is valid.
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    // Preserve all other limits when deliberately disowning the child.
    // SAFETY: the live handle and the correctly sized out-pointer last
    // throughout the call.
    if unsafe {
        QueryInformationJobObject(
            job.as_raw_handle() as HANDLE,
            JobObjectExtendedLimitInformation,
            std::ptr::addr_of_mut!(limits).cast(),
            std::mem::size_of_val(&limits) as u32,
            null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    if enabled {
        limits.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    } else {
        limits.BasicLimitInformation.LimitFlags &= !JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    }
    // SAFETY: the same live handle and initialized limits, passed by value.
    if unsafe {
        SetInformationJobObject(
            job.as_raw_handle() as HANDLE,
            JobObjectExtendedLimitInformation,
            std::ptr::addr_of!(limits).cast(),
            std::mem::size_of_val(&limits) as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

impl Job {
    /// Does any process in it still exist?
    pub fn exists(&self) -> bool {
        job_active_processes(&self.0) > 0
    }

    /// End every process in it now.
    pub fn end(&self) -> io::Result<()> {
        // SAFETY: the handle is live for as long as `self` is.
        if unsafe { TerminateJobObject(self.0.as_raw_handle() as HANDLE, ENDED_BY_OWNER) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

/// How many processes the job still has.
fn job_active_processes(job: &OwnedHandle) -> u32 {
    let mut info = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION {
        TotalUserTime: 0,
        TotalKernelTime: 0,
        ThisPeriodTotalUserTime: 0,
        ThisPeriodTotalKernelTime: 0,
        TotalPageFaultCount: 0,
        TotalProcesses: 0,
        ActiveProcesses: 0,
        TotalTerminatedProcesses: 0,
    };
    // SAFETY: `info` is the struct the class names and outlives the call.
    let ok = unsafe {
        QueryInformationJobObject(
            job.as_raw_handle() as HANDLE,
            JobObjectBasicAccountingInformation,
            std::ptr::addr_of_mut!(info).cast(),
            std::mem::size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
            null_mut(),
        )
    };
    if ok == 0 { 0 } else { info.ActiveProcesses }
}

/// An attribute list sized for `count` attributes, deleted on drop.
struct AttributeList {
    buffer: Vec<u8>,
    list: LPPROC_THREAD_ATTRIBUTE_LIST,
}

impl AttributeList {
    fn new(count: u32) -> io::Result<Self> {
        let mut size = 0usize;
        // SAFETY: the sizing call, expected to fail with the needed size.
        unsafe { InitializeProcThreadAttributeList(null_mut(), count, 0, &mut size) };
        if size == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut buffer = vec![0u8; size];
        let list: LPPROC_THREAD_ATTRIBUTE_LIST = buffer.as_mut_ptr().cast();
        // SAFETY: the buffer is at least `size` bytes and outlives the list.
        if unsafe { InitializeProcThreadAttributeList(list, count, 0, &mut size) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { buffer, list })
    }
}

impl Drop for AttributeList {
    fn drop(&mut self) {
        // SAFETY: initialised in `new`; the buffer is still alive here.
        unsafe { DeleteProcThreadAttributeList(self.list) };
        let _ = &self.buffer;
    }
}

/// An anonymous pipe, both ends non-inheritable until one is marked.
fn pipe() -> io::Result<(OwnedHandle, OwnedHandle)> {
    let mut read: HANDLE = null_mut();
    let mut write: HANDLE = null_mut();
    // SAFETY: both out-pointers are written on success.
    if unsafe { CreatePipe(&mut read, &mut write, null(), 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fresh handles this process owns from here on.
    Ok(unsafe {
        (
            OwnedHandle::from_raw_handle(read),
            OwnedHandle::from_raw_handle(write),
        )
    })
}

/// The program as a shell would find it, and as the runtime inventory
/// finds it (`command::find_program`): a path with a directory part is
/// taken as given, or with each launcher extension in `PATHEXT` order
/// when it has none; a bare name is looked up on `PATH` the same way —
/// never in the working directory, which a bare name does not name.
fn resolve_program(command: &Command) -> io::Result<PathBuf> {
    let given = Path::new(command.get_program());
    let not_found = || {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("program not found: {}", given.display()),
        )
    };
    let name = given
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(not_found)?;
    // The child's PATH and PATHEXT, not this process's: the command's
    // overrides are what the environment block gives the child, so the
    // lookup sees the same variables the child will.
    let pathext = effective_env(command, "PATHEXT");
    if given.components().count() > 1 || given.is_absolute() {
        // Checked and returned as an absolute path: a relative script
        // would otherwise be checked against this process's directory and
        // run by cmd.exe from the child's.
        let absolute = if given.is_absolute() {
            given.to_path_buf()
        } else {
            std::env::current_dir()?.join(given)
        };
        if absolute.is_file() {
            return Ok(absolute);
        }
        let dir = absolute.parent().map(Path::to_path_buf).unwrap_or_default();
        return crate::command::find_program_with(&[dir], name, pathext.as_deref())
            .ok_or_else(not_found);
    }
    let dirs: Vec<PathBuf> = effective_env(command, "PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default();
    crate::command::find_program_with(&dirs, name, pathext.as_deref()).ok_or_else(not_found)
}

/// A variable as the child will see it: the command's override when it
/// has one (matched without case, as Windows keys are), removed when the
/// command removes it, else this process's own.
fn effective_env(command: &Command, key: &str) -> Option<OsString> {
    for (name, value) in command.get_envs() {
        if name.to_string_lossy().eq_ignore_ascii_case(key) {
            return value.map(OsStr::to_os_string);
        }
    }
    std::env::var_os(key)
}

/// `cmd.exe` from the system directory, as the standard library runs a
/// batch file with.
fn command_prompt() -> io::Result<PathBuf> {
    use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;
    let mut buffer = vec![0u16; 260];
    // SAFETY: the buffer's length is what is passed; the call writes at
    // most that many units and returns the length needed.
    let length = unsafe { GetSystemDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) };
    if length == 0 {
        return Err(io::Error::last_os_error());
    }
    if length as usize > buffer.len() {
        buffer = vec![0u16; length as usize];
        // SAFETY: as above, with a buffer of the length asked for.
        let again = unsafe { GetSystemDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) };
        if again == 0 || again as usize > buffer.len() {
            return Err(io::Error::last_os_error());
        }
        buffer.truncate(again as usize);
    } else {
        buffer.truncate(length as usize);
    }
    Ok(PathBuf::from(OsString::from_wide(&buffer)).join("cmd.exe"))
}

/// The command line the standard library builds to run a batch file:
/// `cmd.exe /e:ON /v:OFF /d /c ""script" args…"` — the whole command in
/// one extra pair of quotes, the script quoted, and every argument by
/// `batch_argument`'s rules. A script path with a `"` or a trailing `\`
/// cannot be named to cmd; a verbatim (`\\?\`) path is given as the plain
/// path cmd understands.
pub fn batch_command_line<'a>(
    script: &Path,
    args: impl IntoIterator<Item = &'a OsStr>,
) -> io::Result<String> {
    let script = user_path(script)?;
    if script.contains('"') || script.ends_with('\\') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a batch file's name may not contain `\"` or end with `\\`",
        ));
    }
    let mut line = String::from("cmd.exe /e:ON /v:OFF /d /c \"");
    line.push('"');
    line.push_str(&script);
    line.push('"');
    for arg in args {
        line.push(' ');
        batch_argument(&mut line, arg)?;
    }
    line.push('"');
    Ok(line)
}

/// One argument for a batch file, by the standard library's rules: `\r`
/// and `\n` refused (they would truncate the command line), `%` turned
/// into `%%cd:~,%` so `%VAR%` is never expanded, quoted unless made only
/// of characters cmd leaves alone, `"` doubled inside quotes and the
/// backslashes before one doubled too.
fn batch_argument(line: &mut String, arg: &OsStr) -> io::Result<()> {
    let text = arg.to_str().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "batch file arguments must be Unicode",
        )
    })?;
    if text.contains(['\r', '\n', '\0']) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "batch file arguments are invalid",
        ));
    }
    const UNQUOTED: &str = r"#$*+-./:?@\_";
    let quote = text.is_empty()
        || text.ends_with('\\')
        || text.chars().any(|c| {
            (c.is_ascii() && !(c.is_ascii_alphanumeric() || UNQUOTED.contains(c))) || c.is_control()
        });
    if quote {
        line.push('"');
    }
    let mut backslashes = 0;
    for c in text.chars() {
        if c == '\\' {
            backslashes += 1;
        } else {
            if c == '"' {
                line.extend(std::iter::repeat_n('\\', backslashes));
                line.push('"');
            } else if c == '%' {
                line.push_str("%%cd:~,");
            }
            backslashes = 0;
        }
        line.push(c);
    }
    if quote {
        line.extend(std::iter::repeat_n('\\', backslashes));
        line.push('"');
    }
    Ok(())
}

/// A path as cmd.exe understands it: the verbatim prefix a canonical
/// Windows path carries is not one cmd accepts. The prefix comes off only
/// when the plain form names the same path — `GetFullPathNameW` gives it
/// back unchanged — as the standard library decides it; a verbatim path
/// the plain form would normalise into something else (a trailing dot or
/// space, a component the plain rules rewrite) is refused rather than
/// handed to cmd as a different file.
fn user_path(path: &Path) -> io::Result<String> {
    let text = path.to_string_lossy().into_owned();
    let plain = if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        rest.to_owned()
    } else {
        return Ok(text);
    };
    if full_path_name(&plain)? == plain {
        Ok(plain)
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{text} cannot be named to cmd.exe without its verbatim prefix"),
        ))
    }
}

/// The directory cmd.exe starts a batch launcher in: the plain form of
/// the checkout, and never a network (UNC) directory — cmd.exe does not
/// start in one; it says so and runs in the Windows directory instead,
/// which is a project command in the wrong folder. Refused up front.
fn batch_workdir(dir: &Path) -> io::Result<OsString> {
    let plain = user_path(dir)?;
    if plain.starts_with(r"\\") || plain.starts_with("//") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "cmd.exe cannot run a batch launcher in a network directory: {}",
                dir.display()
            ),
        ));
    }
    Ok(OsString::from(plain))
}

/// `GetFullPathNameW` of `path`: what the plain path rules make of it.
fn full_path_name(path: &str) -> io::Result<String> {
    use windows_sys::Win32::Storage::FileSystem::GetFullPathNameW;
    let wide: Vec<u16> = OsStr::new(path).encode_wide().chain([0]).collect();
    let mut buffer = vec![0u16; 1024];
    // SAFETY: the name is nul-terminated and the buffer's length is what
    // is passed; the call writes at most that many units.
    let length = unsafe {
        GetFullPathNameW(
            wide.as_ptr(),
            buffer.len() as u32,
            buffer.as_mut_ptr(),
            null_mut(),
        )
    };
    if length == 0 {
        return Err(io::Error::last_os_error());
    }
    if length as usize > buffer.len() {
        buffer = vec![0u16; length as usize];
        // SAFETY: as above, with the length asked for.
        let again = unsafe {
            GetFullPathNameW(
                wide.as_ptr(),
                buffer.len() as u32,
                buffer.as_mut_ptr(),
                null_mut(),
            )
        };
        if again == 0 || again as usize > buffer.len() {
            return Err(io::Error::last_os_error());
        }
        buffer.truncate(again as usize);
    } else {
        buffer.truncate(length as usize);
    }
    Ok(String::from_utf16_lossy(&buffer))
}

/// One argument quoted by the rules `CommandLineToArgvW` and the C runtime
/// parse by: quoted when empty or holding a space, a tab or a quote;
/// backslashes before a quote (or the closing quote) doubled; a quote
/// escaped with a backslash.
pub fn quoted(arg: &OsStr) -> String {
    let text = arg.to_string_lossy();
    let needs_quotes = text.is_empty() || text.chars().any(|c| matches!(c, ' ' | '\t' | '"'));
    if !needs_quotes {
        return text.into_owned();
    }
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    let mut backslashes = 0;
    for c in text.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                backslashes = 0;
                out.push('"');
            }
            other => {
                out.extend(std::iter::repeat_n('\\', backslashes));
                backslashes = 0;
                out.push(other);
            }
        }
    }
    out.extend(std::iter::repeat_n('\\', backslashes * 2));
    out.push('"');
    out
}

/// This process's environment with the command's overrides applied, as
/// the nul-separated, doubly terminated UTF-16 block `CreateProcessW`
/// takes, sorted case-insensitively by name as the system keeps it.
fn environment_block(command: &Command) -> Vec<u16> {
    let mut variables: BTreeMap<String, (OsString, OsString)> = BTreeMap::new();
    for (key, value) in std::env::vars_os() {
        variables.insert(fold(&key), (key, value));
    }
    for (key, value) in command.get_envs() {
        match value {
            Some(value) => {
                variables.insert(fold(key), (key.to_os_string(), value.to_os_string()));
            }
            None => {
                variables.remove(&fold(key));
            }
        }
    }
    let mut block = Vec::new();
    for (key, value) in variables.values() {
        block.extend(key.encode_wide());
        block.push(u16::from(b'='));
        block.extend(value.encode_wide());
        block.push(0);
    }
    block.push(0);
    block
}

/// The case fold Windows compares variable names by.
fn fold(key: &OsStr) -> String {
    key.to_string_lossy().to_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn conpty_fixture_command(name: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command.args([
            "--exact",
            &format!("launch::tests::{name}"),
            "--ignored",
            "--nocapture",
        ]);
        command
    }

    fn fixture_stdio_are_consoles() -> [bool; 3] {
        use windows_sys::Win32::System::Console::{
            GetConsoleMode, GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
        };
        [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE].map(|which| {
            let mut mode = 0;
            // SAFETY: inspecting this fixture's standard handles; a null,
            // redirected or invalid handle simply fails GetConsoleMode.
            unsafe { GetConsoleMode(GetStdHandle(which), &mut mode) != 0 }
        })
    }

    #[test]
    #[ignore = "subprocess fixture for conpty_child_uses_its_console_from_a_redirected_parent"]
    fn conpty_stdio_child_fixture() {
        use std::io::Write;
        assert_eq!(fixture_stdio_are_consoles(), [true; 3]);
        std::io::stdout().write_all(b"conpty-ready\n").unwrap();
        std::io::stdout().flush().unwrap();
        std::io::stderr().write_all(b"conpty-stderr\n").unwrap();
        std::io::stderr().flush().unwrap();
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).unwrap();
        assert_eq!(line.trim_end(), "typed-through-conpty");
        std::io::stdout()
            .write_all(b"conpty-input-ok final-partial")
            .unwrap();
        std::io::stdout().flush().unwrap();
    }

    #[test]
    #[ignore = "subprocess fixture for conpty_child_uses_its_console_from_a_redirected_parent"]
    fn conpty_redirected_parent_fixture() {
        use std::io::Write;
        use std::sync::{Arc, mpsc};
        use std::time::{Duration, Instant};

        // Preserve failures despite deliberately redirecting stderr to NUL.
        // This hook exists only in this isolated fixture subprocess.
        let diagnostic = std::env::var_os("AGENTDOCKER_TEST_CONPTY_DIAGNOSTIC").unwrap();
        std::panic::set_hook(Box::new(move |panic| {
            let _ = std::fs::write(&diagnostic, panic.to_string());
        }));
        // A separate process keeps these NUL handles out of concurrently
        // running tests. This is the detached session owner's launch shape.
        assert_eq!(fixture_stdio_are_consoles(), [false; 3]);
        let mut pty = crate::pty::Pty::open().unwrap();
        let mut reader = pty.reader().unwrap();
        let mut writer = pty.writer().unwrap();
        let pending = prepare_with(
            conpty_fixture_command("conpty_stdio_child_fixture"),
            Some(pty.console().unwrap()),
        )
        .unwrap();
        pty.child_created();
        let pty = Arc::new(pty);
        let mut child = pending.activate().unwrap();

        // Drain concurrently through ClosePseudoConsole, which may itself
        // wait for the final output on older Windows versions.
        let (output, chunks) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut buffer = [0; 4096];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => return,
                    Ok(count) => {
                        if output.send(Ok(buffer[..count].to_vec())).is_err() {
                            return;
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::BrokenPipe => return,
                    Err(error) => {
                        let _ = output.send(Err(error));
                        return;
                    }
                }
            }
        });
        let mut screen = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !String::from_utf8_lossy(&screen).contains("conpty-ready") {
            let bytes = chunks
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("console child's ready output before input")
                .unwrap();
            screen.extend(bytes);
            assert!(screen.len() <= 65536, "unbounded fixture output");
        }
        assert!(
            child.try_wait().unwrap().is_none(),
            "stdin reached EOF before typing"
        );
        let (written, writing) = mpsc::channel();
        let writer = std::thread::spawn(move || {
            let _ = written.send(writer.write_all(b"typed-through-conpty\r"));
        });
        writing
            .recv_timeout(Duration::from_secs(5))
            .expect("bounded console input write")
            .unwrap();
        writer.join().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "console child did not exit after input"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        let (closed, closing) = mpsc::channel();
        let closer = std::thread::spawn(move || {
            pty.close();
            let _ = closed.send(());
        });
        closing
            .recv_timeout(Duration::from_secs(5))
            .expect("bounded console close");
        closer.join().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match chunks.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(bytes) => {
                    screen.extend(bytes.unwrap());
                    assert!(screen.len() <= 65536, "unbounded fixture output");
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => panic!("console output did not reach EOF"),
            }
        }
        reader.join().unwrap();
        let screen = String::from_utf8_lossy(&screen);
        assert!(status.success(), "{status}: {screen:?}");
        for expected in [
            "conpty-ready",
            "conpty-stderr",
            "conpty-input-ok final-partial",
        ] {
            assert!(screen.contains(expected), "missing {expected}: {screen:?}");
        }
    }

    #[test]
    fn conpty_child_uses_its_console_from_a_redirected_parent() {
        use std::os::windows::process::CommandExt;
        use std::process::Stdio;
        use std::time::{Duration, Instant};
        use windows_sys::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS};

        // If native startup/close wedges, killing this owner closes its job
        // handles and ends its child. Hold only the process, not a job clone
        // that would defeat KILL_ON_JOB_CLOSE during this fallback.
        struct Fixture(std::process::Child);
        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = self.0.kill();
                // SAFETY: the child process handle is owned throughout this
                // bounded wait. No PID lookup or unrelated process is used.
                unsafe { WaitForSingleObject(self.0.as_raw_handle() as HANDLE, 5_000) };
            }
        }
        let mut command = conpty_fixture_command("conpty_redirected_parent_fixture");
        let temporary = tempfile::tempdir().unwrap();
        let diagnostic = temporary.path().join("conpty-failure.txt");
        command
            .env("AGENTDOCKER_TEST_CONPTY_DIAGNOSTIC", &diagnostic)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS);
        let mut owner = Fixture(command.spawn().unwrap());
        let deadline = Instant::now() + Duration::from_secs(30);
        let status = loop {
            if let Some(status) = owner.0.try_wait().unwrap() {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "redirected ConPTY fixture timed out"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        assert!(
            status.success(),
            "redirected ConPTY fixture failed: {status}: {}",
            std::fs::read_to_string(diagnostic).unwrap_or_default()
        );
    }
    /// A batch launcher's command line is the standard library's: the whole
    /// command in one extra pair of quotes, the script quoted with its
    /// verbatim prefix gone, arguments quoted unless made of safe
    /// characters, `%` neutralised, and a line-breaking argument refused.
    #[test]
    fn a_batch_command_line_follows_the_standard_librarys_rules() {
        let args = ["hello world", "a&b", "100%", "plain", "", "back\\"];
        let line = batch_command_line(
            Path::new(r"\\?\C:\tools\shim.cmd"),
            args.iter().map(OsStr::new),
        )
        .unwrap();
        assert_eq!(
            line,
            r#"cmd.exe /e:ON /v:OFF /d /c ""C:\tools\shim.cmd" "hello world" "a&b" "100%%cd:~,%" plain "" "back\\"""#
        );
        let refused =
            batch_command_line(Path::new(r"C:\tools\shim.cmd"), [OsStr::new("x\ny")]).unwrap_err();
        assert_eq!(refused.kind(), io::ErrorKind::InvalidInput);
    }

    /// A canonical (verbatim) script path is given to cmd.exe as the plain
    /// path only when that plain path names the same file; a verbatim path
    /// whose plain form the path rules would rewrite — a trailing dot here
    /// — is refused rather than run as some other file.
    #[test]
    fn a_verbatim_script_path_is_given_plainly_only_when_it_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let shim = tmp.path().join("shim.cmd");
        std::fs::write(&shim, "@echo off\r\n").unwrap();
        let canonical = std::fs::canonicalize(&shim).unwrap();
        assert!(
            canonical.to_string_lossy().starts_with(r"\\?\"),
            "{}",
            canonical.display()
        );
        let plain = user_path(&canonical).unwrap();
        assert!(
            !plain.starts_with(r"\\?\") && plain.ends_with("shim.cmd"),
            "{plain}"
        );
        let dotted = PathBuf::from(format!(r"\\?\{}", plain.replace("shim.cmd", "shim.cmd.")));
        let error = user_path(&dotted).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    /// A relative script is the one this process sees, made absolute
    /// before cmd.exe runs it from the child's own directory: a script of
    /// the same name in the child's directory is not the one that runs.
    /// A relative path needs a working directory of its own, so this runs
    /// as a fixture process started in `here` (`relative_script_fixture`).
    #[test]
    fn a_relative_script_is_resolved_here_not_in_the_childs_directory() {
        use std::process::Stdio;
        let tmp = tempfile::tempdir().unwrap();
        let here = tmp.path().join("here");
        let there = tmp.path().join("there");
        std::fs::create_dir_all(&here).unwrap();
        std::fs::create_dir_all(&there).unwrap();
        std::fs::write(here.join("which.cmd"), "@echo off\r\necho ran-here\r\n").unwrap();
        std::fs::write(there.join("which.cmd"), "@echo off\r\necho ran-there\r\n").unwrap();
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "launch::tests::relative_script_fixture",
                "--ignored",
                "--nocapture",
            ])
            .current_dir(&here)
            .env("AGENTDOCKER_TEST_RELATIVE_THERE", &there)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success() && stdout.contains("relative-fixture-ok"),
            "{stdout}\n{stderr}"
        );
    }

    /// The fixture: started in `here`, it names the script relatively and
    /// runs it from `there`; what runs must be `here`'s.
    #[test]
    #[ignore = "subprocess fixture for a_relative_script_is_resolved_here_not_in_the_childs_directory"]
    fn relative_script_fixture() {
        let there = PathBuf::from(std::env::var_os("AGENTDOCKER_TEST_RELATIVE_THERE").unwrap());
        let mut command = Command::new(r".\which.cmd");
        command.current_dir(&there);
        let program = resolve_program(&command).unwrap();
        assert!(
            program.is_absolute() && program.ends_with(r"here\which.cmd"),
            "{}",
            program.display()
        );
        let pending = prepare(command).expect("a suspended cmd.exe");
        let mut owned = pending.activate().expect("resumed");
        let mut stdout = owned.take_stdout().expect("a stdout pipe");
        let mut text = String::new();
        stdout.read_to_string(&mut text).unwrap();
        assert!(
            text.contains("ran-here") && !text.contains("ran-there"),
            "{text:?}"
        );
        while owned.try_wait().unwrap().is_none() {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        println!("relative-fixture-ok");
    }

    /// A batch launcher's working directory is the plain checkout, and a
    /// network directory is refused before anything is created: cmd.exe
    /// would run there in the Windows directory instead and say so only
    /// on stderr.
    #[test]
    fn a_batch_launchers_working_directory_is_plain_and_never_a_network_one() {
        let tmp = tempfile::tempdir().unwrap();
        let canonical = std::fs::canonicalize(tmp.path()).unwrap();
        let plain = batch_workdir(&canonical).unwrap();
        assert!(
            !plain.to_string_lossy().starts_with(r"\\?\"),
            "{}",
            plain.to_string_lossy()
        );
        for unc in [r"\\?\UNC\server\share\repo", r"\\server\share\repo"] {
            let error = batch_workdir(Path::new(unc)).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{unc}");
        }
    }

    /// The child's own `PATHEXT` and `PATH` decide the lookup when the
    /// command overrides them: with `.CMD;.EXE`, the shim wins over the
    /// executable beside it, and the directory comes from the command's
    /// `PATH`, not this process's.
    #[test]
    fn the_childs_pathext_and_path_decide_a_bare_name() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        std::fs::write(dir.join("both.cmd"), "@echo off\r\n").unwrap();
        std::fs::write(dir.join("both.exe"), "MZ").unwrap();
        let cmd_first = crate::command::find_program_with(
            std::slice::from_ref(&dir),
            "both",
            Some(OsStr::new(".CMD;.EXE")),
        )
        .unwrap();
        assert!(cmd_first.ends_with("both.cmd"), "{}", cmd_first.display());
        let exe_first = crate::command::find_program_with(
            std::slice::from_ref(&dir),
            "both",
            Some(OsStr::new(".EXE;.CMD")),
        )
        .unwrap();
        assert!(exe_first.ends_with("both.exe"), "{}", exe_first.display());
        let mut command = Command::new("both");
        command.env("Path", &dir).env("pathext", ".CMD;.EXE");
        assert_eq!(resolve_program(&command).unwrap(), dir.join("both.cmd"));
        let mut removed = Command::new("both");
        removed.env("Path", &dir).env_remove("PATHEXT");
        assert_eq!(
            resolve_program(&removed).unwrap(),
            dir.join("both.exe"),
            "the default order puts exe first"
        );
    }

    /// A `.cmd` launcher — the shape of an npm-installed provider — runs
    /// under the gate like any program: through cmd.exe, with its
    /// arguments intact, its output piped, in its job.
    #[test]
    fn a_batch_launcher_runs_under_cmd_with_its_arguments() {
        let tmp = tempfile::tempdir().unwrap();
        let shim = tmp.path().join("shim.cmd");
        std::fs::write(&shim, "@echo off\r\necho shim-args %1 %2\r\n").unwrap();
        let mut command = Command::new(&shim);
        command.args(["hello world", "a&b"]);
        let pending = prepare(command).expect("a suspended cmd.exe");
        let mut owned = pending.activate().expect("resumed");
        let mut stdout = owned.take_stdout().expect("a stdout pipe");
        let mut text = String::new();
        stdout.read_to_string(&mut text).expect("read to the end");
        assert!(
            text.contains("shim-args \"hello world\" \"a&b\""),
            "{text:?}"
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while owned.try_wait().expect("wait").is_none() {
            assert!(std::time::Instant::now() < deadline, "cmd.exe did not exit");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    /// An argument cmd would rewrite is refused before anything runs: the
    /// launcher that would have left a marker leaves none.
    #[test]
    fn a_refused_batch_argument_runs_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let shim = tmp.path().join("shim.cmd");
        let marker = tmp.path().join("marker.txt");
        std::fs::write(
            &shim,
            format!("@echo off\r\necho ran > \"{}\"\r\n", marker.display()),
        )
        .unwrap();
        let mut command = Command::new(&shim);
        command.arg("first line\nsecond line");
        let Err(error) = prepare(command) else {
            panic!("a line-breaking argument was accepted");
        };
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(!marker.exists(), "a refused launch ran the launcher");
    }

    /// A bare name resolves the way a shell resolves it: by launcher
    /// extension in `PATHEXT`'s order, never a data file, never a relative
    /// directory.
    #[test]
    fn a_bare_launcher_name_resolves_by_pathext_precedence() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        std::fs::write(dir.join("shim.cmd"), "@echo off\r\n").unwrap();
        std::fs::write(dir.join("shim.txt"), "not a launcher").unwrap();
        let found = crate::command::find_program(std::slice::from_ref(&dir), "shim").unwrap();
        assert!(found.ends_with("shim.cmd"), "{}", found.display());
        assert!(crate::command::find_program(std::slice::from_ref(&dir), "shim.txt").is_none());
        assert!(crate::command::find_program(&[PathBuf::from("relative")], "shim").is_none());
        // An executable beside the shim wins, as PATHEXT orders them.
        std::fs::write(dir.join("shim.exe"), "MZ").unwrap();
        let found = crate::command::find_program(&[dir], "shim").unwrap();
        assert!(found.ends_with("shim.exe"), "{}", found.display());
    }

    #[test]
    fn arguments_are_quoted_by_the_c_runtime_rules() {
        assert_eq!(quoted(OsStr::new("plain")), "plain");
        assert_eq!(quoted(OsStr::new("")), "\"\"");
        assert_eq!(quoted(OsStr::new("two words")), "\"two words\"");
        assert_eq!(quoted(OsStr::new("say \"hi\"")), "\"say \\\"hi\\\"\"");
        assert_eq!(quoted(OsStr::new("C:\\path\\")), "C:\\path\\");
        assert_eq!(quoted(OsStr::new("C:\\a b\\")), "\"C:\\a b\\\\\"");
    }

    #[test]
    fn a_prepared_command_runs_only_when_activated_and_its_output_is_piped() {
        let mut command = Command::new("cmd");
        command.args(["/c", "echo hello from a gated child"]);
        let pending = prepare(command).expect("a suspended process");
        assert!(pending.pid > 0);
        // Suspended: no exit code yet, and its job holds exactly it.
        let mut owned = pending.activate().expect("resumed");
        let mut stdout = owned.take_stdout().expect("a stdout pipe");
        let mut text = String::new();
        stdout.read_to_string(&mut text).expect("read to the end");
        assert!(text.contains("hello from a gated child"), "{text:?}");
        let status = loop {
            if let Some(status) = owned.try_wait().expect("wait") {
                break status;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        assert!(status.success(), "{status:?}");
        // The job's accounting trails the exit by a moment: the process
        // is signalled before its job membership is torn down (seen on
        // the first runner), so the group is empty soon, not at once.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while owned.group_exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "the job still counts the exited child"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[test]
    fn a_never_activated_command_never_runs() {
        let tmp = tempfile::tempdir().unwrap();
        let marker = tmp.path().join("ran");
        let mut command = Command::new("cmd");
        command.args(["/c", &format!("echo x > \"{}\"", marker.display())]);
        let pending = prepare(command).expect("a suspended process");
        drop(pending);
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(!marker.exists(), "a refused launch wrote its marker");
    }

    #[test]
    fn ending_the_group_ends_a_child_and_its_descendants() {
        let mut command = Command::new("cmd");
        command.args([
            "/c",
            // `ping` as a sleep: `timeout` refuses a redirected stdin.
            "start /b ping -n 31 127.0.0.1 > nul & ping -n 31 127.0.0.1 > nul",
        ]);
        let pending = prepare(command).expect("a suspended process");
        let owned = pending.activate().expect("resumed");
        std::thread::sleep(std::time::Duration::from_millis(500));
        assert!(owned.group_exists());
        owned.end_group().expect("job ended");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while owned.group_exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "the job still has processes"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    #[test]
    fn disown_clears_crash_protection_before_the_last_job_handle_closes() {
        // Direct executable: cleanup of this deliberately disowned test
        // must not strand a shell's descendant outside our process handle.
        let mut command = Command::new("ping.exe");
        command.args(["-n", "31", "127.0.0.1"]);
        let child = prepare(command).unwrap().activate().unwrap();
        // Keep only a process handle, never a job clone that would mask a
        // missing flag clear by preventing last-job-handle close.
        let process = child.process.try_clone().unwrap();
        let pid = child.id();
        assert_eq!(child.disown().unwrap(), pid);
        let handle = process.as_raw_handle() as HANDLE;
        // SAFETY: the handle remains open throughout the wait and cleanup.
        let waited = unsafe { WaitForSingleObject(handle, 200) };
        unsafe {
            TerminateProcess(handle, ENDED_BY_OWNER);
            WaitForSingleObject(handle, 5_000);
        }
        assert_eq!(waited, windows_sys::Win32::Foundation::WAIT_TIMEOUT);
    }
}
