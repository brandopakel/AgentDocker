//! A launcher's words while a provider's full-screen program has the
//! terminal.
//!
//! A launcher that starts a provider's own terminal program in the person's
//! terminal (native Codex, from `agentdocker codex`) may speak there before
//! that program starts and after it has ended, never in between: the screen
//! is the program's, and a line written under it lands on whatever row the
//! program is drawing (its composer, usually), where the next redraws eat
//! part of it and leave the rest looking typed. [`Console::hand_over`] gives
//! the terminal up. Until [`Console::take_back`], this process's own
//! standard output and error are its launch log: what it says
//! ([`Console::say`]), and anything else it would have written there, a
//! stray `eprintln!` or a panic message, goes to the log instead of the
//! screen. A child started before the hand-over keeps the terminal it was
//! given; one started during it gets the log unless it is given something
//! else.

use std::{
    fs::File,
    io::{self, Write},
    path::Path,
};

/// Where a launcher's words go: the terminal, or its log while the terminal
/// is a provider's.
pub struct Console {
    log: File,
    handed: Option<platform::Saved>,
}

impl Console {
    /// A console on this process's terminal, with its private log at `path`
    /// (created, or appended to).
    pub fn open(path: &Path) -> io::Result<Self> {
        Ok(Self {
            log: agentdocker_host::dirs::private_file(path, true, true)?,
            handed: None,
        })
    }

    /// A provider's program has the terminal now: nothing this process
    /// writes to its standard output or error reaches the screen until
    /// [`Console::take_back`]. Handing over twice is handing over once.
    pub fn hand_over(&mut self) -> io::Result<()> {
        if self.handed.is_none() {
            self.handed = Some(platform::redirect(&self.log)?);
        }
        Ok(())
    }

    /// The program has ended: standard output and error are the terminal
    /// they were before the hand-over.
    pub fn take_back(&mut self) -> io::Result<()> {
        match self.handed.take() {
            Some(saved) => platform::restore(saved),
            None => Ok(()),
        }
    }

    /// One line from the launcher: on the terminal while it is this
    /// process's, in the log with the time while it is the provider's. A
    /// line that cannot be written is dropped; saying something never fails
    /// a launch.
    pub fn say(&self, line: std::fmt::Arguments<'_>) {
        let _ = if self.handed.is_some() {
            let at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
            let mut log = &self.log;
            writeln!(log, "{at} {line}")
        } else {
            writeln!(io::stderr().lock(), "{line}")
        };
    }
}

impl Drop for Console {
    fn drop(&mut self) {
        let _ = self.take_back();
    }
}

#[cfg(unix)]
mod platform {
    use std::{
        fs::File,
        io::{self, Write},
        os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
    };

    /// The terminal this process's standard output and error were, kept
    /// while they are the log. A descriptor that was closed is not here.
    pub struct Saved(Vec<(RawFd, OwnedFd)>);

    pub fn redirect(log: &File) -> io::Result<Saved> {
        flush();
        let mut saved = Saved(Vec::new());
        for slot in [libc::STDOUT_FILENO, libc::STDERR_FILENO] {
            let original = match duplicate(slot) {
                Ok(original) => original,
                // Nothing to keep off the screen through a closed descriptor.
                Err(error) if error.raw_os_error() == Some(libc::EBADF) => continue,
                Err(error) => return Err(undo(saved, error)),
            };
            if let Err(error) = replace(slot, log.as_raw_fd()) {
                return Err(undo(saved, error));
            }
            saved.0.push((slot, original));
        }
        Ok(saved)
    }

    pub fn restore(saved: Saved) -> io::Result<()> {
        flush();
        let mut result = Ok(());
        for (slot, original) in saved.0 {
            result = result.and(replace(slot, original.as_raw_fd()));
        }
        result
    }

    fn undo(saved: Saved, error: io::Error) -> io::Error {
        let _ = restore(saved);
        error
    }

    /// A private close-on-exec copy of `fd`, numbered above the standard
    /// three.
    fn duplicate(fd: RawFd) -> io::Result<OwnedFd> {
        // SAFETY: F_DUPFD_CLOEXEC only reads `fd`; the descriptor it returns
        // is new and owned by nobody else.
        let copy = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
        if copy == -1 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `copy` is a fresh open descriptor, owned from here on.
        Ok(unsafe { OwnedFd::from_raw_fd(copy) })
    }

    fn replace(slot: RawFd, with: RawFd) -> io::Result<()> {
        loop {
            // SAFETY: dup2 makes `slot`, one of this process's standard
            // descriptors, refer to what `with` refers to; `with` is open.
            if unsafe { libc::dup2(with, slot) } != -1 {
                return Ok(());
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }

    /// What is still buffered goes where it was written for.
    fn flush() {
        let _ = io::stdout().flush();
        let _ = io::stderr().flush();
    }
}

#[cfg(windows)]
mod platform {
    use std::{
        fs::File,
        io::{self, Write},
        os::windows::io::AsRawHandle,
    };
    use windows_sys::Win32::{
        Foundation::{HANDLE, INVALID_HANDLE_VALUE},
        System::Console::{
            GetStdHandle, STD_ERROR_HANDLE, STD_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle,
        },
    };

    /// The handles standard output and error were, kept while they are the
    /// log. They are the process's, not owned here: restoring puts them
    /// back as they were.
    pub struct Saved(Vec<(STD_HANDLE, usize)>);

    pub fn redirect(log: &File) -> io::Result<Saved> {
        flush();
        let mut saved = Saved(Vec::new());
        for slot in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            // SAFETY: GetStdHandle has no preconditions.
            let original = unsafe { GetStdHandle(slot) };
            if original == INVALID_HANDLE_VALUE {
                return Err(undo(saved, io::Error::last_os_error()));
            }
            if let Err(error) = set(slot, log.as_raw_handle() as HANDLE) {
                return Err(undo(saved, error));
            }
            saved.0.push((slot, original as usize));
        }
        Ok(saved)
    }

    pub fn restore(saved: Saved) -> io::Result<()> {
        flush();
        let mut result = Ok(());
        for (slot, original) in saved.0 {
            result = result.and(set(slot, original as HANDLE));
        }
        result
    }

    fn undo(saved: Saved, error: io::Error) -> io::Error {
        let _ = restore(saved);
        error
    }

    fn set(slot: STD_HANDLE, handle: HANDLE) -> io::Result<()> {
        // SAFETY: SetStdHandle only records the value. The log's handle
        // outlives the hand-over (the console holds its file), and an
        // original is put back exactly as it was read.
        if unsafe { SetStdHandle(slot, handle) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// What is still buffered goes where it was written for.
    fn flush() {
        let _ = io::stdout().flush();
        let _ = io::stderr().flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    const CHILD: &str = "AGENTDOCKER_TEST_CONSOLE_LOG";

    /// The hand-over runs in a child process of its own, since it changes
    /// this process's standard output and error, which the test runner and
    /// every other test in the process share. The child's pipes stand in
    /// for the terminal.
    #[test]
    fn while_handed_over_everything_this_process_writes_goes_to_the_log() {
        if let Some(log) = std::env::var_os(CHILD) {
            let mut console = Console::open(Path::new(&log)).unwrap();
            console.say(format_args!("before the program starts"));
            console.hand_over().unwrap();
            console.hand_over().unwrap();
            console.say(format_args!("native input ready: fixture"));
            eprintln!("a stray eprintln");
            println!("a stray println");
            console.take_back().unwrap();
            console.say(format_args!("after the program ended"));
            println!("standard output is the terminal again");
            // A console dropped while handed over gives the terminal back.
            console.hand_over().unwrap();
            eprintln!("written while handed over a second time");
            drop(console);
            eprintln!("written after the console was dropped");
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let log = root.path().join("launcher.log");
        let module = module_path!().split_once("::").unwrap().1;
        let name =
            format!("{module}::while_handed_over_everything_this_process_writes_goes_to_the_log");
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &name, "--nocapture", "--test-threads", "1"])
            .env(CHILD, &log)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        let terminal_out = String::from_utf8_lossy(&output.stdout);
        let terminal_err = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{terminal_out}\n{terminal_err}");
        assert!(terminal_out.contains("1 passed"), "{terminal_out}");
        let logged = std::fs::read_to_string(&log).unwrap();
        for said in [
            "before the program starts",
            "after the program ended",
            "written after the console was dropped",
        ] {
            assert!(terminal_err.contains(said), "{said}: {terminal_err}");
            assert!(!logged.contains(said), "{said}: {logged}");
        }
        assert!(terminal_out.contains("standard output is the terminal again"));
        for kept_off in [
            "native input ready: fixture",
            "a stray eprintln",
            "a stray println",
            "written while handed over a second time",
        ] {
            assert!(logged.contains(kept_off), "{kept_off}: {logged}");
            assert!(
                !terminal_err.contains(kept_off) && !terminal_out.contains(kept_off),
                "{kept_off} reached the terminal:\n{terminal_out}\n{terminal_err}"
            );
        }
        // What the launcher says on purpose is timed in the log.
        let ready = logged
            .lines()
            .find(|line| line.ends_with("native input ready: fixture"))
            .unwrap();
        let (at, _) = ready.split_once(' ').unwrap();
        assert!(chrono::DateTime::parse_from_rfc3339(at).is_ok(), "{ready}");
    }
}
