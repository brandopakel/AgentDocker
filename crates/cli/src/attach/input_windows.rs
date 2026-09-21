//! Read the console's keyboard as bytes an attached agent understands.
//!
//! A console input handle is read with `ReadConsoleW`, which blocks and
//! cannot be polled from the runtime, so a plain thread reads it and hands
//! chunks over a channel; with virtual terminal input on (see
//! `pty::RawMode`), what arrives is the same escape sequences a Unix
//! terminal sends, in UTF-16, and they leave here as UTF-8. The thread is
//! not the runtime's: it does not keep the runtime from exiting, and
//! dropping the reader cancels its read so the console is free again.
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, ReadBuf};
use tokio::sync::mpsc;

use std::os::windows::io::{AsRawHandle, BorrowedHandle};
use windows_sys::Win32::Foundation::HANDLE;

pub(super) struct Input {
    chunks: mpsc::Receiver<io::Result<Vec<u8>>>,
    pending: Vec<u8>,
    reader: Option<std::thread::JoinHandle<()>>,
    reader_thread: HANDLE,
}

// SAFETY: the thread handle is only ever passed to CancelSynchronousIo.
unsafe impl Send for Input {}

impl Input {
    /// Read the console on `handle` (the process's standard input), which
    /// must stay open for as long as this does.
    pub(super) fn open(handle: BorrowedHandle<'_>) -> io::Result<Self> {
        use windows_sys::Win32::System::Console::ReadConsoleW;
        let (sender, chunks) = mpsc::channel(64);
        let raw = handle.as_raw_handle() as usize;
        let reader = std::thread::Builder::new()
            .name("console-input".into())
            .spawn(move || {
                let handle = raw as HANDLE;
                let mut units = [0u16; 512];
                loop {
                    let mut read = 0u32;
                    // SAFETY: the buffer outlives the call and its length
                    // is what is passed; `read` is written on success.
                    let ok = unsafe {
                        ReadConsoleW(
                            handle,
                            units.as_mut_ptr().cast(),
                            units.len() as u32,
                            &mut read,
                            std::ptr::null(),
                        )
                    };
                    if ok == 0 {
                        let error = io::Error::last_os_error();
                        // A cancelled read is the reader being dropped.
                        if error.raw_os_error() != Some(995) {
                            let _ = sender.blocking_send(Err(error));
                        }
                        return;
                    }
                    if read == 0 {
                        return;
                    }
                    let text = String::from_utf16_lossy(&units[..read as usize]);
                    if sender.blocking_send(Ok(text.into_bytes())).is_err() {
                        return;
                    }
                }
            })?;
        let reader_thread = reader.as_raw_handle() as HANDLE;
        Ok(Self {
            chunks,
            pending: Vec::new(),
            reader: Some(reader),
            reader_thread,
        })
    }
}

impl AsyncRead for Input {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if buffer.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        if self.pending.is_empty() {
            match self.chunks.poll_recv(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => return Poll::Ready(Ok(())),
                Poll::Ready(Some(Err(error))) => return Poll::Ready(Err(error)),
                Poll::Ready(Some(Ok(chunk))) => self.pending = chunk,
            }
        }
        let take = self.pending.len().min(buffer.remaining());
        let rest = self.pending.split_off(take);
        let head = std::mem::replace(&mut self.pending, rest);
        buffer.put_slice(&head);
        Poll::Ready(Ok(()))
    }
}

impl Drop for Input {
    fn drop(&mut self) {
        use windows_sys::Win32::System::IO::CancelSynchronousIo;
        if let Some(reader) = self.reader.take() {
            // SAFETY: the join handle keeps the thread handle valid here.
            unsafe { CancelSynchronousIo(self.reader_thread) };
            // Not joined: a console that ignores the cancellation would
            // hold the thread until the next keystroke, and the command is
            // ending anyway. The thread ends with the process at the latest.
            drop(reader);
        }
    }
}
