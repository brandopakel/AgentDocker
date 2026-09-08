//! Read a terminal through readiness notifications, without a blocking stdin
//! task that survives cancellation and prevents the Tokio runtime from exiting.
use std::io::{self, Read};
use std::os::fd::BorrowedFd;
use std::pin::Pin;
use std::task::{Context, Poll, ready};
use tokio::io::unix::AsyncFd;
use tokio::io::{AsyncRead, ReadBuf};

pub(super) struct Input(AsyncFd<std::fs::File>);

impl Input {
    pub(super) fn open(fd: BorrowedFd<'_>) -> io::Result<Self> {
        Ok(Self(AsyncFd::new(
            agentdocker_host::pty::nonblocking_input(fd)?,
        )?))
    }
}

impl AsyncRead for Input {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if buffer.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        loop {
            let mut ready = ready!(self.0.poll_read_ready(cx))?;
            match ready.try_io(|inner| inner.get_ref().read(buffer.initialize_unfilled())) {
                Ok(Ok(bytes)) => {
                    buffer.advance(bytes);
                    return Poll::Ready(Ok(()));
                }
                Ok(Err(error)) if error.kind() == io::ErrorKind::Interrupted => continue,
                Ok(Err(error)) => return Poll::Ready(Err(error)),
                Err(_) => continue,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::fd::{AsFd, AsRawFd};
    use std::time::Duration;
    use tokio::io::AsyncReadExt;

    #[tokio::test]
    async fn cancelling_a_terminal_read_keeps_input_and_inherited_flags_intact() {
        let mut pty = agentdocker_host::pty::Pty::open().unwrap();
        let slave = pty.take_slave().unwrap();
        let _raw = agentdocker_host::pty::RawMode::enter(slave.as_raw_fd()).unwrap();
        // SAFETY: the owned slave remains live through this observation.
        let before = unsafe { libc::fcntl(slave.as_raw_fd(), libc::F_GETFL) };
        assert!(before >= 0);
        let mut input = Input::open(slave.as_fd()).unwrap();
        let mut byte = [0];
        let pending = std::future::poll_fn(|cx| {
            let mut buffer = ReadBuf::new(&mut byte);
            assert!(Pin::new(&mut input).poll_read(cx, &mut buffer).is_pending());
            Poll::Ready(())
        });
        pending.await;
        drop(input);
        // SAFETY: the same owned slave is still live.
        assert_eq!(
            unsafe { libc::fcntl(slave.as_raw_fd(), libc::F_GETFL) },
            before
        );
        let mut input = Input::open(slave.as_fd()).unwrap();
        let mut writer = std::fs::File::from(pty.master().try_clone().unwrap());
        writer.write_all(b"k").unwrap();
        tokio::time::timeout(Duration::from_secs(3), input.read_exact(&mut byte))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(byte, *b"k");
    }
}
