//! Reserve a native owner's group until cleanup, without proxying its terminal.
use anyhow::{Context, Result, ensure};
use std::{
    fs::File,
    io,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::process::CommandExt,
    },
    process::{Child, ExitStatus},
    time::Duration,
};

pub(super) struct Process {
    child: Child,
    terminal: Option<Terminal>,
    reaped: bool,
}

impl Process {
    pub(super) fn spawn(command: tokio::process::Command) -> Result<Self> {
        let terminal = Terminal::capture()?;
        let mut command = command.into_std();
        command.process_group(0);
        // Use a std child: Tokio's wait would reap the leader before its group
        // is retired. A zombie child still reserves this exact group number.
        let child = command.spawn()?;
        let mut owned = Self {
            child,
            terminal,
            reaped: false,
        };
        if let Some(terminal) = &mut owned.terminal {
            terminal.activate(owned.child.id() as i32)?;
        }
        Ok(owned)
    }

    fn event(&mut self, options: i32) -> Result<bool> {
        // SAFETY: this unreaped child is ours; waitid writes a valid siginfo_t.
        let mut event: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                self.child.id() as _,
                &mut event,
                options | libc::WNOHANG,
            )
        };
        if result != 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ECHILD) {
                // Another reaper would invalidate our group reservation. Never
                // signal the saved group number after losing that ownership.
                self.reaped = true;
            }
            if error.raw_os_error() == Some(libc::EINTR) {
                return Ok(false);
            }
            return Err(error.into());
        }
        Ok(event.si_signo != 0)
    }

    pub(super) async fn exited(&mut self) -> Result<()> {
        ensure!(!self.reaped, "native lifetime child is already reaped");
        loop {
            if self.event(libc::WEXITED | libc::WNOWAIT)? {
                return Ok(());
            }
            // Consume only the stop notification, never the exit. Propagate
            // foreground job suspension to the shell-facing process and keep
            // the owner's terminal modes across fg/continue.
            if self.event(libc::WSTOPPED)? {
                let group = self.child.id() as i32;
                self.signal_group(libc::SIGSTOP)?;
                let terminal = self
                    .terminal
                    .as_mut()
                    .context("native lifetime owner stopped without a controlling terminal")?;
                let modes = terminal.settings()?;
                terminal.restore(group)?;
                // SAFETY: suspend only this front end. Its shell can continue
                // it normally; SIGSTOP cannot be swallowed by an inherited mask.
                if unsafe { libc::kill(libc::getpid(), libc::SIGSTOP) } != 0 {
                    return Err(io::Error::last_os_error().into());
                }
                if terminal.activate(group)? {
                    terminal.set_settings(&modes)?;
                }
                self.signal_group(libc::SIGCONT)?;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    fn signal_group(&self, signal: i32) -> io::Result<()> {
        if self.reaped {
            return Err(io::Error::other("native lifetime group ownership was lost"));
        }
        // SAFETY: process_group(0) created this group before exec; the std child
        // remains unreaped even after exit. Its PID cannot be reused here.
        if unsafe { libc::kill(-(self.child.id() as i32), signal) } == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(error)
        }
    }

    pub(super) async fn finish(&mut self) -> Result<ExitStatus> {
        self.signal_group(libc::SIGKILL)?;
        let terminal = self
            .terminal
            .as_mut()
            .map(|terminal| terminal.restore(self.child.id() as i32))
            .transpose();
        let status = self.child.wait();
        self.reaped = status.is_ok()
            || status
                .as_ref()
                .is_err_and(|e| e.raw_os_error() == Some(libc::ECHILD));
        terminal?;
        Ok(status?)
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        if !self.reaped {
            let _ = self.signal_group(libc::SIGKILL);
            let _ = self.child.kill();
            if let Some(terminal) = &mut self.terminal {
                let _ = terminal.restore(self.child.id() as i32);
            }
            let _ = self.child.wait();
        }
    }
}

struct Terminal {
    file: File,
    frontend: i32,
    original: libc::termios,
    active: bool,
}

impl Terminal {
    fn capture() -> io::Result<Option<Self>> {
        // A synthetic PTY may be attached as stdin without being this process's
        // controlling terminal. It needs no foreground handoff in that case.
        let foreground = unsafe { libc::tcgetpgrp(libc::STDIN_FILENO) };
        if foreground < 0 {
            let error = io::Error::last_os_error();
            return if matches!(error.raw_os_error(), Some(libc::ENOTTY | libc::EBADF)) {
                Ok(None)
            } else {
                Err(error)
            };
        }
        let fd = unsafe { libc::fcntl(libc::STDIN_FILENO, libc::F_DUPFD_CLOEXEC, 3) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: fcntl returned a fresh owned descriptor, closed on exec.
        let file = unsafe { File::from_raw_fd(fd) };
        let mut terminal = Self {
            file,
            frontend: unsafe { libc::getpgrp() },
            original: unsafe { std::mem::zeroed() },
            active: false,
        };
        terminal.original = terminal.settings()?;
        Ok(Some(terminal))
    }

    fn settings(&self) -> io::Result<libc::termios> {
        let mut settings = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(self.file.as_raw_fd(), &mut settings) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(settings)
    }

    fn set_settings(&self, settings: &libc::termios) -> io::Result<()> {
        without_ttou(|| unsafe { libc::tcsetattr(self.file.as_raw_fd(), libc::TCSANOW, settings) })
    }

    fn foreground(&self) -> io::Result<i32> {
        let group = unsafe { libc::tcgetpgrp(self.file.as_raw_fd()) };
        if group < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(group)
        }
    }

    fn activate(&mut self, group: i32) -> io::Result<bool> {
        if self.foreground()? != self.frontend {
            // A background launch must not steal the shell's foreground.
            return Ok(false);
        }
        self.original = self.settings()?;
        without_ttou(|| unsafe { libc::tcsetpgrp(self.file.as_raw_fd(), group) })?;
        self.active = true;
        Ok(true)
    }

    fn restore(&mut self, group: i32) -> io::Result<()> {
        if self.active {
            let foreground = self.foreground()?;
            if foreground == group || foreground == self.frontend {
                without_ttou(|| unsafe { libc::tcsetpgrp(self.file.as_raw_fd(), self.frontend) })?;
                self.set_settings(&self.original)?;
            }
            self.active = false;
        }
        Ok(())
    }
}

/// Terminal foreground changes from the supervising background group otherwise
/// raise SIGTTOU. Block it on this thread for these synchronous calls only.
fn without_ttou(call: impl FnOnce() -> i32) -> io::Result<()> {
    let mut blocked = unsafe { std::mem::zeroed() };
    let mut previous = unsafe { std::mem::zeroed() };
    unsafe {
        libc::sigemptyset(&mut blocked);
        libc::sigaddset(&mut blocked, libc::SIGTTOU);
    }
    let error = unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &blocked, &mut previous) };
    if error != 0 {
        return Err(io::Error::from_raw_os_error(error));
    }
    let result = if call() == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    };
    let error =
        unsafe { libc::pthread_sigmask(libc::SIG_SETMASK, &previous, std::ptr::null_mut()) };
    result?;
    if error == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(error))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Stdio;
    use tokio::{io::AsyncReadExt, process::Command, time::timeout};

    #[tokio::test]
    async fn exited_and_killed_owners_keep_their_group_reserved_through_cleanup() {
        for killed in [false, true] {
            let mut other = Command::new("/bin/sleep")
                .arg("30")
                .kill_on_drop(true)
                .spawn()
                .unwrap();
            let mut command = Command::new("/bin/sh");
            command
                .args([
                    "-c",
                    if killed {
                        "sleep 30 & printf 'ready\\n'; wait"
                    } else {
                        "sleep 30 & printf 'ready\\n'; exit 0"
                    },
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
            let mut owner = Process::spawn(command).unwrap();
            let mut output =
                tokio::process::ChildStdout::from_std(owner.child.stdout.take().unwrap()).unwrap();
            let mut ready = [0; 6];
            timeout(Duration::from_secs(5), output.read_exact(&mut ready))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(&ready, b"ready\n");
            if killed {
                owner.child.kill().unwrap();
            }
            timeout(Duration::from_secs(5), owner.exited())
                .await
                .unwrap()
                .unwrap();
            // Repeated observation must not reap. The descendant still holds
            // stdout, so EOF after finish proves group cleanup, not just reaping
            // the leader. The unrelated live child must survive that cleanup.
            owner.exited().await.unwrap();
            let mut rest = Vec::new();
            assert!(
                timeout(Duration::from_millis(25), output.read_to_end(&mut rest))
                    .await
                    .is_err()
            );
            let status = owner.finish().await.unwrap();
            assert_eq!(status.success(), !killed);
            timeout(Duration::from_secs(5), output.read_to_end(&mut rest))
                .await
                .unwrap()
                .unwrap();
            assert!(rest.is_empty());
            assert!(other.try_wait().unwrap().is_none());
            other.kill().await.unwrap();
        }
    }
}
