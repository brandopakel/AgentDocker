//! Cancellable reads from the dedicated server's anonymous stderr pipe.
//!
//! Tokio's Windows ChildStderr uses a blocking-pool ReadFile. Cancelling that
//! future cannot cancel its worker if a descendant retains the writer. The
//! owner's runtime would then wait for a pipe held by children its job retires
//! only when the owner exits. This reader is the pipe's sole consumer: peek,
//! read only available bytes, and yield while empty without a blocking worker.
use std::{io, os::windows::io::AsRawHandle, ptr::null_mut, time::Duration};
use tokio::process::ChildStderr;
use windows_sys::Win32::{
    Foundation::{ERROR_BROKEN_PIPE, ERROR_PIPE_NOT_CONNECTED},
    Storage::FileSystem::ReadFile,
    System::Pipes::PeekNamedPipe,
};

fn eof(error: &io::Error) -> bool {
    matches!(error.raw_os_error(), Some(code)
        if code == ERROR_BROKEN_PIPE as i32 || code == ERROR_PIPE_NOT_CONNECTED as i32)
}

pub(super) async fn read(input: &mut ChildStderr, buffer: &mut [u8]) -> io::Result<usize> {
    if buffer.is_empty() {
        return Ok(0);
    }
    loop {
        let mut available = 0;
        // SAFETY: input owns a synchronous pipe handle. No Tokio read is
        // started on it; this task is its only consumer. Peek writes only the
        // available-byte count and does not consume data.
        let ready = unsafe {
            PeekNamedPipe(
                input.as_raw_handle(),
                null_mut(),
                0,
                null_mut(),
                &mut available,
                null_mut(),
            )
        };
        if ready == 0 {
            let error = io::Error::last_os_error();
            return if eof(&error) { Ok(0) } else { Err(error) };
        }
        if available == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
            continue;
        }
        let length = (available as usize).min(buffer.len()) as u32;
        let mut received = 0;
        // SAFETY: this sole reader cannot lose the peeked bytes to another
        // consumer. Reading at most that count does not wait for a writer.
        // buffer and received remain live until the synchronous call returns.
        let success = unsafe {
            ReadFile(
                input.as_raw_handle(),
                buffer.as_mut_ptr(),
                length,
                &mut received,
                null_mut(),
            )
        };
        if success == 0 {
            let error = io::Error::last_os_error();
            return if eof(&error) { Ok(0) } else { Err(error) };
        }
        return Ok(received as usize);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::io::{FromRawHandle, OwnedHandle};
    use windows_sys::Win32::{
        Foundation::GetHandleInformation, Storage::FileSystem::WriteFile, System::Pipes::CreatePipe,
    };

    fn pipe() -> (ChildStderr, OwnedHandle) {
        let (mut reader, mut writer) = (null_mut(), null_mut());
        // SAFETY: initialized output handles; non-inheritable test-only pipe.
        assert_ne!(
            unsafe { CreatePipe(&mut reader, &mut writer, null_mut(), 0) },
            0
        );
        // SAFETY: CreatePipe returned two new owned handles.
        let reader = unsafe { OwnedHandle::from_raw_handle(reader) };
        let writer = unsafe { OwnedHandle::from_raw_handle(writer) };
        (
            ChildStderr::from_std(std::process::ChildStderr::from(reader)).unwrap(),
            writer,
        )
    }

    #[tokio::test]
    async fn cancellation_releases_the_reader_while_a_descendant_keeps_writing_open() {
        let (mut reader, writer) = pipe();
        let raw = reader.as_raw_handle();
        let mut buffer = [0; 16];
        assert!(
            tokio::time::timeout(Duration::from_millis(30), read(&mut reader, &mut buffer))
                .await
                .is_err()
        );
        drop(reader);
        let mut flags = 0;
        // SAFETY: GetHandleInformation only queries numeric handle validity;
        // it does not close or access data through a possibly invalid handle.
        // Retain the writer until after the assertion so EOF cannot hide an
        // uncancellable background read retaining the reader's handle.
        assert_eq!(unsafe { GetHandleInformation(raw, &mut flags) }, 0);
        assert_ne!(
            unsafe { GetHandleInformation(writer.as_raw_handle(), &mut flags) },
            0
        );
    }

    #[tokio::test]
    async fn available_bytes_are_read_before_eof_without_a_background_reader() {
        let (mut reader, writer) = pipe();
        let mut written = 0;
        // SAFETY: the live pipe has room for this bounded four-byte write.
        assert_ne!(
            unsafe {
                WriteFile(
                    writer.as_raw_handle(),
                    b"test".as_ptr(),
                    4,
                    &mut written,
                    null_mut(),
                )
            },
            0
        );
        assert_eq!(written, 4);
        drop(writer);
        let mut buffer = [0; 16];
        assert_eq!(read(&mut reader, &mut buffer).await.unwrap(), 4);
        assert_eq!(&buffer[..4], b"test");
        assert_eq!(read(&mut reader, &mut buffer).await.unwrap(), 0);
    }
}
