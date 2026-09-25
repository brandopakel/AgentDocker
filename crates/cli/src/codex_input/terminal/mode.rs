//! The managed PTY must not impose its much smaller kernel canonical-line cap
//! before the bridge can enforce its own limit. Preserve signal/output behavior;
//! the bounded editor supplies input echo and editing while this guard lives.
use std::io;

#[cfg(unix)]
use std::{
    io::IsTerminal,
    os::fd::{AsFd, AsRawFd, OwnedFd},
};

pub(in crate::codex_input) struct Mode {
    #[cfg(unix)]
    saved: Option<(OwnedFd, libc::termios)>,
}

impl Mode {
    pub fn enter(enabled: bool) -> io::Result<Self> {
        #[cfg(unix)]
        {
            let stdin = io::stdin();
            if enabled && stdin.is_terminal() {
                return Self::on(stdin.as_fd().try_clone_to_owned()?);
            }
            Ok(Self { saved: None })
        }
        #[cfg(not(unix))]
        {
            let _ = enabled;
            Ok(Self {})
        }
    }

    pub fn editing(&self) -> bool {
        #[cfg(unix)]
        {
            self.saved.is_some()
        }
        #[cfg(not(unix))]
        {
            false
        }
    }

    #[cfg(unix)]
    fn on(fd: OwnedFd) -> io::Result<Self> {
        // SAFETY: the guard owns this descriptor and both termios values live
        // through the calls. A failed read never supplies fabricated settings.
        unsafe {
            let mut saved: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(fd.as_raw_fd(), &mut saved) < 0 {
                return Err(io::Error::last_os_error());
            }
            let mut mode = saved;
            mode.c_lflag &= !(libc::ICANON | libc::ECHO | libc::ECHONL);
            mode.c_iflag |= libc::ICRNL;
            mode.c_cc[libc::VMIN] = 1;
            mode.c_cc[libc::VTIME] = 0;
            if libc::tcsetattr(fd.as_raw_fd(), libc::TCSANOW, &mode) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self {
                saved: Some((fd, saved)),
            })
        }
    }
}

#[cfg(unix)]
impl Drop for Mode {
    fn drop(&mut self) {
        if let Some((fd, saved)) = &self.saved {
            // SAFETY: fd is still owned here; restore exactly what was read.
            unsafe {
                libc::tcsetattr(fd.as_raw_fd(), libc::TCSANOW, saved);
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn settings(fd: &OwnedFd) -> libc::termios {
        // SAFETY: fd is live and value is initialized by a successful call.
        unsafe {
            let mut value = std::mem::zeroed();
            assert_eq!(libc::tcgetattr(fd.as_raw_fd(), &mut value), 0);
            value
        }
    }

    #[test]
    fn managed_pty_disables_kernel_line_cap_and_restores_settings() {
        let mut pty = agentdocker_host::pty::Pty::open().unwrap();
        let slave = pty.take_slave().unwrap();
        let before = settings(&slave);
        let mode = Mode::on(slave.try_clone().unwrap()).unwrap();
        assert!(mode.editing());
        let active = settings(&slave);
        assert_eq!(
            active.c_lflag & (libc::ICANON | libc::ECHO | libc::ECHONL),
            0
        );
        assert_eq!(active.c_lflag & libc::ISIG, before.c_lflag & libc::ISIG);
        assert_eq!(active.c_oflag, before.c_oflag);
        assert_eq!(active.c_cc[libc::VMIN], 1);
        drop(mode);
        let after = settings(&slave);
        assert_eq!(after.c_iflag, before.c_iflag);
        assert_eq!(after.c_oflag, before.c_oflag);
        // Darwin sets PENDIN (retype pending input) when canonical mode is
        // restored. It is kernel state, not a changed terminal preference.
        assert_eq!(
            after.c_lflag & !libc::PENDIN,
            before.c_lflag & !libc::PENDIN
        );
        assert_eq!(after.c_cc, before.c_cc);
    }
}
