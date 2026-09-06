//! Pseudo-terminals, so a managed agent can be an interactive one.
//!
//! `run` gives a child pipes, and an agent that wants a terminal — which
//! is most of them — either refuses to start or degrades to a
//! non-interactive mode. A pty fixes that: the child gets a real terminal
//! as its controlling terminal, the daemon holds the other end, and what
//! the agent writes still reaches the log so `logs` keeps working.
//!
//! Only libc here, no extra dependency: `posix_openpt` and friends exist
//! on macOS and Linux alike, and avoiding `openpty` avoids linking
//! `libutil` on the musl release targets.

use std::ffi::CStr;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

/// A terminal pair: the daemon reads and writes `master`, the child gets
/// `slave` as its standard streams and controlling terminal.
#[derive(Debug)]
pub struct Pty {
    master: OwnedFd,
    slave: Option<OwnedFd>,
}

impl Pty {
    /// Open a new terminal pair.
    pub fn open() -> io::Result<Self> {
        // SAFETY: each call is checked, and the descriptors returned are
        // owned from here on.
        unsafe {
            let master = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY);
            if master < 0 {
                return Err(io::Error::last_os_error());
            }
            let master = OwnedFd::from_raw_fd(master);
            if libc::grantpt(master.as_raw_fd()) < 0 || libc::unlockpt(master.as_raw_fd()) < 0 {
                return Err(io::Error::last_os_error());
            }
            // `ptsname` returns a pointer into static storage; copy it out
            // before anything else can call it.
            let name = libc::ptsname(master.as_raw_fd());
            if name.is_null() {
                return Err(io::Error::last_os_error());
            }
            let name = CStr::from_ptr(name).to_owned();
            let slave = libc::open(name.as_ptr(), libc::O_RDWR | libc::O_NOCTTY);
            if slave < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self {
                master,
                slave: Some(OwnedFd::from_raw_fd(slave)),
            })
        }
    }

    pub fn master(&self) -> &OwnedFd {
        &self.master
    }

    /// The end the child keeps. Taken once, when it is spawned; the
    /// daemon must not hold it afterwards or the master never sees EOF.
    pub fn take_slave(&mut self) -> Option<OwnedFd> {
        self.slave.take()
    }

    /// Tell the terminal how big the window is, so full-screen agents lay
    /// themselves out correctly and `SIGWINCH` reaches them.
    pub fn resize(&self, cols: u16, rows: u16) -> io::Result<()> {
        set_window_size(self.master.as_raw_fd(), cols, rows)
    }

    /// Give up the master, for a caller that wants to own the descriptor.
    pub fn into_master(self) -> OwnedFd {
        self.master
    }
}

/// `TIOCSWINSZ` on any terminal descriptor.
pub fn set_window_size(fd: RawFd, cols: u16, rows: u16) -> io::Result<()> {
    let size = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: `size` outlives the call and `fd` is a terminal.
    let result = unsafe { libc::ioctl(fd, libc::TIOCSWINSZ as _, &size) };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// What a child must do between `fork` and `exec` to own the terminal it
/// was handed: leave the daemon's session, then claim the terminal on
/// its standard input as its controlling one.
///
/// Call this from `Command::pre_exec`, where the standard streams are
/// already the slave. It replaces `process_group(0)`: `setsid` makes the
/// child a session and process-group leader by itself, so signalling the
/// group by `-pid` still reaches its descendants, and calling both would
/// fail because a group leader cannot `setsid`.
///
/// # Safety
///
/// Runs in a forked child, so it uses only async-signal-safe calls and
/// allocates nothing.
pub unsafe fn take_controlling_terminal() -> io::Result<()> {
    // SAFETY: both are async-signal-safe and their results are checked.
    unsafe {
        if libc::setsid() < 0 {
            return Err(io::Error::last_os_error());
        }
        if libc::ioctl(libc::STDIN_FILENO, libc::TIOCSCTTY as _, 0) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// The window size of a terminal, as it reports it.
pub fn window_size(fd: RawFd) -> Option<(u16, u16)> {
    let mut size = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: `size` outlives the call; a non-terminal simply fails.
    let result = unsafe { libc::ioctl(fd, libc::TIOCGWINSZ as _, &mut size) };
    (result == 0 && size.ws_col > 0).then_some((size.ws_col, size.ws_row))
}

/// Put a terminal in raw mode for as long as this lives, so keystrokes
/// reach an attached agent instead of being cooked by the local line
/// discipline. The original settings come back on drop, including when
/// the process unwinds.
pub struct RawMode {
    fd: RawFd,
    saved: libc::termios,
}

impl RawMode {
    pub fn enter(fd: RawFd) -> io::Result<Self> {
        // SAFETY: `saved` is written by tcgetattr before it is read.
        unsafe {
            let mut saved: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(fd, &mut saved) < 0 {
                return Err(io::Error::last_os_error());
            }
            let mut raw = saved;
            libc::cfmakeraw(&mut raw);
            if libc::tcsetattr(fd, libc::TCSANOW, &raw) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self { fd, saved })
        }
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        // SAFETY: restoring what tcgetattr gave us, on the same descriptor.
        unsafe {
            libc::tcsetattr(self.fd, libc::TCSANOW, &self.saved);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    #[test]
    fn a_child_on_the_slave_end_sees_a_real_terminal() {
        let mut pty = Pty::open().unwrap();
        pty.resize(120, 40).unwrap();
        let slave = pty.take_slave().expect("slave taken once");
        assert!(pty.take_slave().is_none(), "and only once");

        let mut child = {
            let stdin = slave.try_clone().unwrap();
            let stdout = slave.try_clone().unwrap();
            let mut command = Command::new("sh");
            command
                .arg("-c")
                // `test -t 0` is the question; the size proves the resize.
                .arg("test -t 0 && echo IS_A_TTY; stty size < /dev/tty")
                .stdin(Stdio::from(stdin))
                .stdout(Stdio::from(stdout))
                .stderr(Stdio::from(slave));
            // SAFETY: only async-signal-safe calls, as the contract requires.
            unsafe { command.pre_exec(|| take_controlling_terminal()) };
            command.spawn().unwrap()
        };

        let mut master = std::fs::File::from(pty.into_master());
        let mut seen = String::new();
        let mut buffer = [0_u8; 512];
        // The master reports EOF once the child exits and no descriptor
        // holds the slave; read until then or until we have the answer.
        while let Ok(read) = master.read(&mut buffer) {
            if read == 0 {
                break;
            }
            seen.push_str(&String::from_utf8_lossy(&buffer[..read]));
            if seen.contains("40 120") {
                break;
            }
        }
        let _ = child.wait();
        assert!(
            seen.contains("IS_A_TTY"),
            "the child had a terminal: {seen:?}"
        );
        assert!(seen.contains("40 120"), "the size we set: {seen:?}");
    }

    #[test]
    fn what_is_written_to_the_master_arrives_as_input() {
        let mut pty = Pty::open().unwrap();
        let slave = pty.take_slave().unwrap();
        let mut child = {
            let stdin = slave.try_clone().unwrap();
            let stdout = slave.try_clone().unwrap();
            let mut command = Command::new("sh");
            command
                .arg("-c")
                .arg("read line; echo \"got:$line\"")
                .stdin(Stdio::from(stdin))
                .stdout(Stdio::from(stdout))
                .stderr(Stdio::from(slave));
            // SAFETY: as above.
            unsafe { command.pre_exec(|| take_controlling_terminal()) };
            command.spawn().unwrap()
        };
        let mut master = std::fs::File::from(pty.into_master());
        master.write_all(b"hello\n").unwrap();
        let mut seen = String::new();
        let mut buffer = [0_u8; 512];
        while let Ok(read) = master.read(&mut buffer) {
            if read == 0 {
                break;
            }
            seen.push_str(&String::from_utf8_lossy(&buffer[..read]));
            if seen.contains("got:hello") {
                break;
            }
        }
        let _ = child.wait();
        assert!(
            seen.contains("got:hello"),
            "input reached the child: {seen:?}"
        );
    }
}
