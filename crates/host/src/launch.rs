//! Hold a native command before exec until its owner durably records its PID.
//!
//! Only async-signal-safe syscalls run in the forked child. The application
//! cannot execute before `activate`; dropping the gate or losing its parent
//! aborts exec. A dedicated worker completes Command's exec-error handshake.
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::{net::UnixStream, process::CommandExt};
use std::process::{Child, Command, ExitStatus};
use std::sync::mpsc;
use std::time::Duration;

const DEADLINE: Duration = Duration::from_secs(10);

pub struct Pending {
    pub pid: u32,
    gate: Option<UnixStream>,
    result: mpsc::Receiver<io::Result<OwnedChild>>,
}

/// An executed child remains owned even if an async activation is cancelled.
pub struct OwnedChild {
    child: Child,
}

impl OwnedChild {
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }

    pub fn take_stdout(&mut self) -> Option<std::process::ChildStdout> {
        self.child.stdout.take()
    }

    pub fn take_stderr(&mut self) -> Option<std::process::ChildStderr> {
        self.child.stderr.take()
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            // This child has not been reaped, so its PID cannot name somebody
            // else. Native launches establish a dedicated group before gating.
            // SAFETY: kill only reads its scalar arguments.
            unsafe { libc::kill(-(self.child.id() as i32), libc::SIGKILL) };
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// Prepare a command whose pre-exec setup establishes its dedicated process
/// group. This blocks for the fork handshake; async callers use spawn_blocking.
pub fn prepare(mut command: Command) -> io::Result<Pending> {
    let (mut parent, child) = UnixStream::pair()?;
    parent.set_read_timeout(Some(DEADLINE))?;
    parent.set_write_timeout(Some(DEADLINE))?;
    let parent_fd = parent.as_raw_fd();
    // SAFETY: the closure only accesses inherited scalars/descriptors and uses
    // async-signal-safe libc calls. It does not allocate or acquire locks.
    unsafe {
        command.pre_exec(move || {
            libc::close(parent_fd);
            let fd = child.as_raw_fd();
            let pid = libc::getpid().to_be_bytes();
            if libc::write(fd, pid.as_ptr().cast(), pid.len()) != pid.len() as isize {
                return Err(io::Error::from_raw_os_error(libc::EIO));
            }
            let mut poll = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            // A hung owner cannot strand an unexecuted child indefinitely.
            if libc::poll(&mut poll, 1, 30_000) <= 0 {
                return Err(io::Error::from_raw_os_error(libc::ECANCELED));
            }
            let mut token = 0_u8;
            if libc::read(fd, (&mut token as *mut u8).cast(), 1) != 1 || token != b'G' {
                return Err(io::Error::from_raw_os_error(libc::ECANCELED));
            }
            Ok(())
        });
    }
    let (sender, result) = mpsc::channel();
    std::thread::Builder::new()
        .name("agent-launch".into())
        .spawn(move || {
            let launched = command.spawn().map(|child| OwnedChild { child });
            // Close the parent's copy of the child's gate even on exec error.
            drop(command);
            // Failed send drops OwnedChild, killing/reaping a cancelled launch.
            let _ = sender.send(launched);
        })?;
    let mut bytes = [0; 4];
    if let Err(error) = parent.read_exact(&mut bytes) {
        let _ = parent.shutdown(std::net::Shutdown::Both);
        return match result.recv_timeout(DEADLINE) {
            Ok(Err(spawn_error)) => Err(spawn_error),
            _ => Err(error),
        };
    }
    let pid = i32::from_be_bytes(bytes);
    if pid <= 0 {
        let _ = parent.shutdown(std::net::Shutdown::Both);
        return Err(io::Error::other(
            "launch returned an invalid process identity",
        ));
    }
    Ok(Pending {
        pid: pid as u32,
        gate: Some(parent),
        result,
    })
}

impl Pending {
    /// Authorize exec only after the PID/protection transaction commits.
    /// A failed exec is returned with Command's original OS error.
    pub fn activate(mut self) -> io::Result<OwnedChild> {
        self.gate.as_mut().expect("gate retained").write_all(b"G")?;
        let result = self
            .result
            .recv_timeout(DEADLINE)
            .map_err(io::Error::other)?;
        self.gate.take();
        result
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        if let Some(gate) = &self.gate {
            // shutdown also wakes a child if another fork briefly inherited a
            // parent descriptor before its close-on-exec cleanup.
            let _ = gate.shutdown(std::net::Shutdown::Both);
        }
        // Dropping the receiver also drops a queued OwnedChild; a late worker
        // send fails and drops its OwnedChild instead. Neither loses ownership.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn fixture(path: &std::path::Path) -> Command {
        let mut command = Command::new("sh");
        command.args(["-c", "printf executed > \"$1\"", "fixture"]);
        command.arg(path).process_group(0);
        command
    }

    fn wait_gone(pid: u32) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            // SAFETY: signal zero probes only this recorded fixture PID.
            if unsafe { libc::kill(pid as i32, 0) } != 0 {
                break;
            }
            assert!(Instant::now() < deadline, "gated child was not reaped");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn activation_is_required_before_the_first_command_instruction() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("executed");
        let pending = prepare(fixture(&marker)).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        assert!(!marker.exists());
        assert!(crate::procinfo::start_time(pending.pid).is_some());
        let mut child = pending.activate().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while child.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(std::fs::read_to_string(marker).unwrap(), "executed");
    }

    #[test]
    fn dropping_a_pending_launch_aborts_and_reaps_without_executing() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("executed");
        let pending = prepare(fixture(&marker)).unwrap();
        let pid = pending.pid;
        drop(pending);
        wait_gone(pid);
        assert!(!marker.exists());
    }

    #[test]
    fn setup_and_exec_errors_keep_the_original_cause() {
        let dir = tempfile::tempdir().unwrap();
        let mut command = fixture(&dir.path().join("marker"));
        command.current_dir(dir.path().join("missing"));
        let error = prepare(command).err().expect("missing cwd fails");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        let mut command = Command::new(dir.path().join("missing-executable"));
        command.process_group(0);
        let pending = prepare(command).unwrap();
        let pid = pending.pid;
        assert_eq!(
            pending.activate().err().unwrap().kind(),
            io::ErrorKind::NotFound
        );
        wait_gone(pid);
    }

    #[test]
    #[ignore = "subprocess fixture invoked by owner_death_denies_exec"]
    fn gated_parent_fixture() {
        let root = std::path::PathBuf::from(std::env::var_os("AD_LAUNCH_FIXTURE").unwrap());
        let pending = prepare(fixture(&root.join("executed"))).unwrap();
        std::fs::write(root.join("pid"), pending.pid.to_string()).unwrap();
        // The driver kills this parent without running Pending::drop.
        loop {
            std::thread::park();
        }
    }

    #[test]
    fn owner_death_denies_exec() {
        let dir = tempfile::tempdir().unwrap();
        let mut parent = Command::new(std::env::current_exe().unwrap());
        parent
            .args([
                "--ignored",
                "--exact",
                "launch::tests::gated_parent_fixture",
            ])
            .env("AD_LAUNCH_FIXTURE", dir.path())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .process_group(0);
        let mut parent = OwnedChild {
            child: parent.spawn().unwrap(),
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        let pid = loop {
            if let Ok(text) = std::fs::read_to_string(dir.path().join("pid")) {
                if let Ok(pid) = text.parse::<u32>() {
                    break pid;
                }
            }
            assert!(
                parent.try_wait().unwrap().is_none(),
                "fixture parent died early"
            );
            assert!(
                Instant::now() < deadline,
                "fixture parent did not prepare a child"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        parent.child.kill().unwrap();
        parent.child.wait().unwrap();
        wait_gone(pid);
        assert!(!dir.path().join("executed").exists());
    }
}
