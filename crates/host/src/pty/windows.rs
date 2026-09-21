//! Pseudo consoles, so a managed agent on Windows can be an interactive one.
//!
//! The Unix twin hands a child the slave end of a terminal pair. Windows has
//! no slave: a pseudo console (`CreatePseudoConsole`) sits over two pipes,
//! the child is bound to it at creation through a process attribute (see
//! `launch`), and the daemon side reads what the console renders from one
//! pipe and types into the other. What the child sees is a console of the
//! size it was told; what this side sees is a stream of bytes with virtual
//! terminal sequences, the same shape the Unix master produces.
//!
//! Only `windows-sys` here, no extra dependency, as the Unix side keeps to
//! libc.

use std::fs::File;
use std::io;
use std::os::windows::io::{AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle};
use std::ptr::{null, null_mut};
use std::sync::Mutex;

use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Console::{
    COORD, ClosePseudoConsole, CreatePseudoConsole, HPCON, ResizePseudoConsole,
};
use windows_sys::Win32::System::Pipes::CreatePipe;

/// The size a console starts with until the first resize; the Unix twin
/// leaves it to the terminal, which reports 0×0 until told, and a
/// full-screen agent lays itself out on the first `SIGWINCH`. A console
/// cannot be 0×0, so this is what an unattached agent sees.
const DEFAULT_COLS: i16 = 80;
const DEFAULT_ROWS: i16 = 24;

/// A pseudo console and this side's ends of its pipes: `output` is what
/// the console renders, `input` is what the child reads as its keyboard.
#[derive(Debug)]
pub struct Pty {
    console: Mutex<Option<HPCON>>,
    startup_pipes: Mutex<Option<(OwnedHandle, OwnedHandle)>>,
    output: OwnedHandle,
    input: OwnedHandle,
}

// SAFETY: a pseudo console handle is a kernel object reference with no
// thread affinity; the pipe ends are owned handles.
unsafe impl Send for Pty {}
unsafe impl Sync for Pty {}

impl Pty {
    /// Open a pseudo console over fresh pipes.
    pub fn open() -> io::Result<Self> {
        let (child_reads, we_write) = pipe()?;
        let (we_read, child_writes) = pipe()?;
        let mut console: HPCON = 0;
        // SAFETY: both handles are open pipe ends and the console pointer
        // is written on success.
        let result = unsafe {
            CreatePseudoConsole(
                COORD {
                    X: DEFAULT_COLS,
                    Y: DEFAULT_ROWS,
                },
                child_reads.as_raw_handle() as HANDLE,
                child_writes.as_raw_handle() as HANDLE,
                0,
                &mut console,
            )
        };
        if result < 0 {
            return Err(io::Error::from_raw_os_error(result));
        }
        // Keep the console's original pipe ends until CreateProcess has
        // connected the child, as required by the ConPTY startup sequence.
        Ok(Self {
            console: Mutex::new(Some(console)),
            startup_pipes: Mutex::new(Some((child_reads, child_writes))),
            output: we_read,
            input: we_write,
        })
    }

    /// The console handle a child is bound to at creation
    /// (`PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE`). Only used during exclusive
    /// launch preparation, before this terminal is shared with controllers.
    /// The caller must finish creating the child before closing the console.
    pub fn console(&mut self) -> io::Result<HPCON> {
        self.console
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .ok_or_else(console_closed)
    }

    /// Release the parent's copies after CreateProcess has attached the
    /// child (which may still be suspended). Keeping these afterward would
    /// prevent output EOF when the console closes its own copies.
    pub fn child_created(&mut self) {
        self.startup_pipes
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
    }

    /// End the console independently of this struct's lifetime. Output
    /// readers may still hold the terminal while waiting for EOF. Keep
    /// draining output concurrently: on older Windows versions this call
    /// waits for the console to finish writing its final output.
    pub fn close(&self) {
        let console = self
            .console
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        // Also cover a failed or abandoned launch with no child_created.
        self.startup_pipes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        // The lock excludes a resize still using the handle. Release it
        // before the potentially blocking close; future resizes fail closed.
        if let Some(console) = console {
            // SAFETY: this call took sole ownership of the live handle.
            unsafe { ClosePseudoConsole(console) };
        }
    }

    /// Tell the console how big the window is; it tells the child.
    pub fn resize(&self, cols: u16, rows: u16) -> io::Result<()> {
        let size = COORD {
            X: i16::try_from(cols.max(1)).unwrap_or(i16::MAX),
            Y: i16::try_from(rows.max(1)).unwrap_or(i16::MAX),
        };
        let console = self
            .console
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let handle = (*console).ok_or_else(console_closed)?;
        // SAFETY: close cannot take this handle while this guard is held.
        let result = unsafe { ResizePseudoConsole(handle, size) };
        if result < 0 {
            return Err(io::Error::from_raw_os_error(result));
        }
        Ok(())
    }

    /// What the console renders, as a file to read from.
    pub fn reader(&self) -> io::Result<File> {
        Ok(File::from(self.output.try_clone()?))
    }

    /// The child's keyboard, as a file to write to.
    ///
    /// The console must outlive the child: dropping this `Pty` closes it,
    /// and a child whose console closes is ended by the system. Clone the
    /// ends; keep the `Pty`.
    pub fn writer(&self) -> io::Result<File> {
        Ok(File::from(self.input.try_clone()?))
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        self.close();
    }
}

fn console_closed() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "the agent's console is closed")
}

/// An anonymous pipe, both ends non-inheritable: the child gets its ends
/// through the console (or an explicit handle list), never by inheriting
/// whatever this process holds.
fn pipe() -> io::Result<(OwnedHandle, OwnedHandle)> {
    let mut read: HANDLE = null_mut();
    let mut write: HANDLE = null_mut();
    // SAFETY: both out-pointers are written on success; a null attribute
    // pointer means non-inheritable handles with the default security.
    if unsafe { CreatePipe(&mut read, &mut write, null(), 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if read == INVALID_HANDLE_VALUE || write == INVALID_HANDLE_VALUE {
        return Err(io::Error::other("pipe creation returned no handles"));
    }
    // SAFETY: fresh handles this process owns from here on.
    Ok(unsafe {
        (
            OwnedHandle::from_raw_handle(read),
            OwnedHandle::from_raw_handle(write),
        )
    })
}

/// The window size of the console on `handle`, as it reports it.
pub fn window_size(handle: BorrowedHandle<'_>) -> Option<(u16, u16)> {
    let handle = handle.as_raw_handle() as HANDLE;
    use windows_sys::Win32::System::Console::{
        CONSOLE_SCREEN_BUFFER_INFO, GetConsoleScreenBufferInfo,
    };
    let mut info = CONSOLE_SCREEN_BUFFER_INFO {
        dwSize: COORD { X: 0, Y: 0 },
        dwCursorPosition: COORD { X: 0, Y: 0 },
        wAttributes: 0,
        srWindow: windows_sys::Win32::System::Console::SMALL_RECT {
            Left: 0,
            Top: 0,
            Right: 0,
            Bottom: 0,
        },
        dwMaximumWindowSize: COORD { X: 0, Y: 0 },
    };
    // SAFETY: `info` outlives the call; a non-console handle simply fails.
    if unsafe { GetConsoleScreenBufferInfo(handle, &mut info) } == 0 {
        return None;
    }
    let cols = i32::from(info.srWindow.Right) - i32::from(info.srWindow.Left) + 1;
    let rows = i32::from(info.srWindow.Bottom) - i32::from(info.srWindow.Top) + 1;
    (cols > 0 && rows > 0).then_some((cols as u16, rows as u16))
}

/// Put a console in raw mode for as long as this lives: input without
/// line editing, echo or Ctrl-C processing and with virtual terminal
/// input, output with virtual terminal processing, so keystrokes reach an
/// attached agent and its escape sequences reach the screen. The original
/// modes come back on drop, including when the process unwinds.
pub struct RawMode {
    input: HANDLE,
    output: HANDLE,
    saved_input: u32,
    saved_output: u32,
}

impl RawMode {
    pub fn enter(input: BorrowedHandle<'_>, output: BorrowedHandle<'_>) -> io::Result<Self> {
        use windows_sys::Win32::System::Console::{
            ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT, ENABLE_PROCESSED_INPUT, ENABLE_PROCESSED_OUTPUT,
            ENABLE_VIRTUAL_TERMINAL_INPUT, ENABLE_VIRTUAL_TERMINAL_PROCESSING, GetConsoleMode,
            SetConsoleMode,
        };
        let input = input.as_raw_handle() as HANDLE;
        let output = output.as_raw_handle() as HANDLE;
        let mut saved_input = 0;
        let mut saved_output = 0;
        // SAFETY: the out-pointers are written on success; a non-console
        // handle fails and nothing has been changed.
        if unsafe { GetConsoleMode(input, &mut saved_input) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if unsafe { GetConsoleMode(output, &mut saved_output) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let raw_input = (saved_input
            & !(ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT | ENABLE_PROCESSED_INPUT))
            | ENABLE_VIRTUAL_TERMINAL_INPUT;
        let raw_output =
            saved_output | ENABLE_PROCESSED_OUTPUT | ENABLE_VIRTUAL_TERMINAL_PROCESSING;
        // SAFETY: setting modes on handles just read; the input is restored
        // if the output refuses, so a failure leaves the console as found.
        if unsafe { SetConsoleMode(input, raw_input) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if unsafe { SetConsoleMode(output, raw_output) } == 0 {
            let error = io::Error::last_os_error();
            unsafe { SetConsoleMode(input, saved_input) };
            return Err(error);
        }
        Ok(Self {
            input,
            output,
            saved_input,
            saved_output,
        })
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        use windows_sys::Win32::System::Console::SetConsoleMode;
        // SAFETY: restoring what GetConsoleMode gave us, on the same handles.
        unsafe {
            SetConsoleMode(self.input, self.saved_input);
            SetConsoleMode(self.output, self.saved_output);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    /// A console renders what is typed into it and resizes without
    /// complaint; closing it ends the output stream.
    #[test]
    fn a_console_opens_resizes_and_closes() {
        let pty = Pty::open().expect("a pseudo console");
        pty.resize(120, 40).expect("resize");
        let mut writer = pty.writer().expect("writer");
        let mut reader = pty.reader().expect("reader");
        // Nothing is bound to the console, so what is typed is not echoed
        // by any child; the pipe itself must still accept the write.
        writer.write_all(b"x").expect("typed");
        drop(writer);
        let drain = std::thread::spawn(move || {
            let mut rest = Vec::new();
            let _ = reader.read_to_end(&mut rest);
        });
        drop(pty);
        // Read concurrently: ClosePseudoConsole itself can wait for the
        // output drain on Windows versions before 11 24H2.
        drain.join().unwrap();
    }

    #[test]
    fn explicit_close_ends_output_while_the_terminal_is_still_shared() {
        use std::sync::{Arc, Barrier, mpsc};
        use std::time::Duration;
        let pty = Arc::new(Pty::open().unwrap());
        let mut reader = pty.reader().unwrap();
        let (eof, received) = mpsc::channel();
        // Drain concurrently with ClosePseudoConsole, including on Windows
        // versions that wait for the final console output during close.
        let retained = pty.clone();
        std::thread::spawn(move || {
            let mut output = Vec::new();
            let result = reader.read_to_end(&mut output);
            let _ = eof.send(result);
            drop(retained);
        });
        let barrier = Arc::new(Barrier::new(2));
        let resizing = pty.clone();
        let start = barrier.clone();
        let resizer = std::thread::spawn(move || {
            start.wait();
            for _ in 0..100 {
                if let Err(error) = resizing.resize(100, 30) {
                    assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
                    return;
                }
            }
        });
        barrier.wait();
        pty.close();
        pty.close(); // idempotent, including the eventual Drop
        resizer.join().unwrap();
        assert_eq!(
            pty.resize(80, 24).unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
        let result = received
            .recv_timeout(Duration::from_secs(5))
            .expect("output EOF");
        assert!(result.is_ok() || result.unwrap_err().kind() == io::ErrorKind::BrokenPipe);
    }

    #[test]
    fn a_non_console_handle_has_no_window_size() {
        use std::os::windows::io::AsHandle;
        let file = std::fs::File::open(std::env::current_exe().unwrap()).unwrap();
        assert_eq!(window_size(file.as_handle()), None);
    }
}
