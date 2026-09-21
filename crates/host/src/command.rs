//! Bounded local command execution for Git integration operations.
#[cfg(windows)]
use process_wrap::std::{ChildWrapper, CommandWrap, JobObject};
use std::io::{self, Read};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::Path;
#[cfg(unix)]
use std::process::Child;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

struct ChildGroup {
    #[cfg(unix)]
    child: Child,
    #[cfg(windows)]
    child: Box<dyn ChildWrapper>,
    reaped: bool,
}
impl Drop for ChildGroup {
    fn drop(&mut self) {
        if !self.reaped {
            // SAFETY: the unreaped child still reserves its PID. Never signal
            // its cached group number once that identity can be reused.
            #[cfg(unix)]
            unsafe {
                libc::kill(-(self.child.id() as i32), libc::SIGKILL);
            }
            #[cfg(windows)]
            let _ = self.child.start_kill();
            let _ = self.child.wait();
        }
    }
}

/// Detach an explicitly started daemon from the requesting terminal. This is
/// separate from bounded command jobs: the daemon must survive its client.
///
/// On Windows the daemon must also not keep the client's own standard
/// handles: a child inherits every inheritable handle of its parent, and
/// the client's stdout and stderr are inheritable when a script or a
/// shell captured them, so a daemon holding them would keep that pipe
/// open — and the capture waiting — for as long as it runs. The client's
/// standard handles are made non-inheritable here, for this process,
/// before the daemon is started; the daemon gets its own stdio from the
/// command.
pub fn detach(command: &mut Command) {
    #[cfg(unix)]
    command.process_group(0);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::Foundation::{HANDLE_FLAG_INHERIT, SetHandleInformation};
        use windows_sys::Win32::System::Console::{
            GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
        };
        use windows_sys::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS};
        for which in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            // SAFETY: GetStdHandle and SetHandleInformation have no
            // preconditions; an absent handle is skipped, and a failure
            // to change one leaves it as it was.
            let handle = unsafe { GetStdHandle(which) };
            if !handle.is_null() && handle != windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
                let _ = unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) };
            }
        }
        command.creation_flags(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS);
    }
}

/// The launchers this host runs, in the order a Windows shell tries them
/// unless `PATHEXT` says otherwise: an npm-installed provider is a `.cmd`
/// shim beside `node.exe`, which is how `claude` and `codex` appear.
/// Scripts a shell would also try (`.vbs`, `.js`) are not launchers here.
#[cfg(windows)]
pub const LAUNCHER_EXTENSIONS: &[&str] = &["com", "exe", "bat", "cmd"];

/// The first program of that name in `dirs`, as a shell would find it.
/// `name` is a bare name or a name with an extension, never a path with
/// directories; relative directories in `dirs` are skipped, so the working
/// directory is never searched by accident.
pub fn find_program(dirs: &[std::path::PathBuf], name: &str) -> Option<std::path::PathBuf> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        dirs.iter()
            .filter(|dir| dir.is_absolute())
            .map(|dir| dir.join(name))
            .find(|candidate| {
                std::fs::metadata(candidate)
                    .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            })
    }
    #[cfg(windows)]
    {
        let names = launcher_names(name)?;
        dirs.iter()
            .filter(|dir| dir.is_absolute())
            .flat_map(|dir| names.iter().map(move |name| dir.join(name)))
            .find(|candidate| std::fs::metadata(candidate).is_ok_and(|m| m.is_file()))
    }
}

/// The file names `name` may stand for on Windows, in the order they are
/// tried: a name with a launcher extension as given; a bare name with
/// each launcher extension in `PATHEXT`'s order (the shell's precedence),
/// or the default order when `PATHEXT` is unset. A name with an unknown
/// extension names no launcher at all — a data file that shares the name
/// is not a CLI.
#[cfg(windows)]
pub fn launcher_names(name: &str) -> Option<Vec<String>> {
    match Path::new(name).extension() {
        Some(extension) => LAUNCHER_EXTENSIONS
            .iter()
            .any(|known| extension.eq_ignore_ascii_case(known))
            .then(|| vec![name.to_owned()]),
        None => Some(
            launcher_order()
                .into_iter()
                .map(|extension| format!("{name}.{extension}"))
                .collect(),
        ),
    }
}

/// `PATHEXT`'s order restricted to the launchers this host runs; the
/// default order when it is unset or names none of them.
#[cfg(windows)]
fn launcher_order() -> Vec<&'static str> {
    let ordered: Vec<&'static str> = std::env::var("PATHEXT")
        .map(|pathext| {
            pathext
                .split(';')
                .filter_map(|entry| {
                    let entry = entry.trim().trim_start_matches('.');
                    LAUNCHER_EXTENSIONS
                        .iter()
                        .copied()
                        .find(|known| known.eq_ignore_ascii_case(entry))
                })
                .collect()
        })
        .unwrap_or_default();
    if ordered.is_empty() {
        LAUNCHER_EXTENSIONS.to_vec()
    } else {
        ordered
    }
}

/// Whether a resolved program is a batch launcher, which Windows cannot
/// start on its own: `cmd.exe` runs it.
#[cfg(windows)]
pub fn is_batch_launcher(program: &Path) -> bool {
    program.extension().is_some_and(|extension| {
        ["cmd", "bat"]
            .iter()
            .any(|batch| extension.eq_ignore_ascii_case(batch))
    })
}

pub struct Output {
    pub success: bool,
    /// Standard output only, for structured engine responses.
    pub stdout: String,
    /// Standard output followed by standard error, for diagnostics.
    pub text: String,
}

/// Capture at most 4 MiB and stop the entire subprocess group on timeout.
pub fn run(root: &Path, argv: &[String], timeout: Duration) -> io::Result<Output> {
    run_with_env(root, argv, timeout, &[])
}

/// Apply explicit child-only overrides without mutating the caller's environment.
/// None removes an inherited value; all command bounds and group cleanup remain.
pub fn run_with_env(
    root: &Path,
    argv: &[String],
    timeout: Duration,
    environment: &[(&str, Option<&std::ffi::OsStr>)],
) -> io::Result<Output> {
    let program = argv
        .first()
        .ok_or_else(|| io::Error::other("empty command"))?;
    let mut log = tempfile::tempfile()?;
    let mut errors = tempfile::tempfile()?;
    let mut command = Command::new(program);
    command
        .args(&argv[1..])
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(errors.try_clone()?);
    for (name, value) in environment {
        if let Some(value) = value {
            command.env(name, value);
        } else {
            command.env_remove(name);
        }
    }
    #[cfg(unix)]
    command.process_group(0);
    #[cfg(windows)]
    let mut command = {
        let mut wrapped = CommandWrap::from(command);
        // Assignment happens while suspended, before the command can spawn a
        // descendant. Dropping the wrapper closes its kill-on-close Job Object.
        wrapped.wrap(JobObject);
        wrapped
    };
    let mut child = ChildGroup {
        child: command.spawn()?,
        reaped: false,
    };
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.child.try_wait()? {
            child.reaped = true;
            break status;
        }
        if started.elapsed() >= timeout {
            return Err(io::Error::other(format!(
                "command timed out after {timeout:?}"
            )));
        }
        if log
            .metadata()?
            .len()
            .saturating_add(errors.metadata()?.len())
            > 4 * 1024 * 1024
        {
            return Err(io::Error::other("command output exceeded 4 MiB"));
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    use std::io::{Seek, SeekFrom};
    log.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    log.take(4 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 4 * 1024 * 1024 {
        return Err(io::Error::other("command output exceeded 4 MiB"));
    }
    errors.seek(SeekFrom::Start(0))?;
    let mut stderr = Vec::new();
    errors.take(4 * 1024 * 1024 + 1).read_to_end(&mut stderr)?;
    if bytes.len().saturating_add(stderr.len()) > 4 * 1024 * 1024 {
        return Err(io::Error::other("command output exceeded 4 MiB"));
    }
    let stdout = String::from_utf8_lossy(&bytes).into_owned();
    Ok(Output {
        success: status.success(),
        text: format!("{stdout}{}", String::from_utf8_lossy(&stderr)),
        stdout,
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[test]
    fn environment_overrides_are_applied_only_to_the_owned_child() {
        let directory = tempfile::tempdir().unwrap();
        let before = std::env::var_os("AGENTDOCKER_TEST_COMMAND_ENV");
        let output = run_with_env(
            directory.path(),
            &[
                "/bin/sh".into(),
                "-c".into(),
                "printf '%s' \"$AGENTDOCKER_TEST_COMMAND_ENV\"".into(),
            ],
            Duration::from_secs(5),
            &[(
                "AGENTDOCKER_TEST_COMMAND_ENV",
                Some(std::ffi::OsStr::new("child-only")),
            )],
        )
        .unwrap();
        assert!(output.success);
        assert_eq!(output.stdout, "child-only");
        assert_eq!(std::env::var_os("AGENTDOCKER_TEST_COMMAND_ENV"), before);
    }

    #[test]
    fn time_and_output_limits_report_distinct_causes() {
        let tmp = tempfile::tempdir().unwrap();
        let timeout = run(
            tmp.path(),
            &["sh".into(), "-c".into(), "sleep 10".into()],
            Duration::from_millis(20),
        )
        .err()
        .unwrap();
        assert!(timeout.to_string().contains("timed out"));
        let output = run(
            tmp.path(),
            &["sh".into(), "-c".into(), "head -c 5000000 /dev/zero".into()],
            Duration::from_secs(5),
        )
        .err()
        .unwrap();
        assert!(output.to_string().contains("output exceeded"));
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    fn fixture(name: &str) -> Vec<String> {
        vec![
            std::env::current_exe()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            "--exact".into(),
            format!("command::windows_tests::{name}"),
            "--ignored".into(),
            "--nocapture".into(),
        ]
    }

    // These are child entry points invoked by the acceptance test below. They
    // exercise native Rust process startup, independent of PowerShell cold start.
    #[test]
    #[ignore = "fixture subprocess, invoked by timeout_terminates_the_owned_descendant_and_output_is_bounded"]
    fn job_descendant_fixture() {
        if std::env::var_os("AGENTDOCKER_TEST_JOB_DESCENDANT").is_none() {
            std::fs::write("phase", "entered").unwrap();
            let argv = fixture("job_descendant_fixture");
            let mut child = Command::new(&argv[0])
                .args(&argv[1..])
                .env("AGENTDOCKER_TEST_JOB_DESCENDANT", "1")
                .spawn()
                .unwrap();
            std::fs::write("descendant.pid", child.id().to_string()).unwrap();
            child.wait().unwrap();
        } else {
            std::fs::write("descendant-ready", "running").unwrap();
        }
        std::thread::sleep(Duration::from_secs(30));
    }

    #[test]
    #[ignore = "fixture subprocess, invoked by timeout_terminates_the_owned_descendant_and_output_is_bounded"]
    fn excessive_output_fixture() {
        use std::io::Write;
        std::io::stdout()
            .write_all(&vec![b'x'; 5 * 1024 * 1024])
            .unwrap();
    }

    #[test]
    fn timeout_terminates_the_owned_descendant_and_output_is_bounded() {
        let temporary = tempfile::tempdir().unwrap();
        let marker = temporary.path().join("descendant.pid");
        let phase = temporary.path().join("phase");
        let argv = fixture("job_descendant_fixture");
        let result = run(temporary.path(), &argv, Duration::from_secs(5));
        match result {
            Err(error) => assert!(error.to_string().contains("timed out"), "{error}"),
            Ok(output) => panic!("fixture exited before its deadline: {}", output.text),
        }
        let pid: u32 = std::fs::read_to_string(&marker)
            .unwrap_or_else(|error| {
                panic!(
                    "descendant marker unavailable: {error}; native fixture phase={:?}",
                    std::fs::read_to_string(&phase)
                )
            })
            .trim()
            .parse()
            .unwrap();
        assert!(
            crate::procinfo::start_time(pid).is_none(),
            "owned job descendant survived cancellation"
        );
        assert!(
            temporary.path().join("descendant-ready").exists(),
            "the descendant actually executed before cancellation"
        );
        let output = run(
            temporary.path(),
            &fixture("excessive_output_fixture"),
            Duration::from_secs(10),
        );
        assert!(
            output
                .err()
                .unwrap()
                .to_string()
                .contains("output exceeded")
        );
    }
}
