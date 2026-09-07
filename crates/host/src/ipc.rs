//! Local native IPC. Unix uses filesystem sockets; Windows uses private named
//! pipes with explicit ACL and peer checks before application bytes are sent.
#[cfg(unix)]
pub use std::os::unix::net::UnixStream as BlockingStream;
#[cfg(unix)]
pub use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
#[cfg(unix)]
pub use tokio::net::{UnixListener as Listener, UnixStream as Stream};
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{BlockingStream, Listener, OwnedReadHalf, OwnedWriteHalf, Stream};

#[cfg(unix)]
pub async fn pair() -> std::io::Result<(Stream, Stream)> {
    Stream::pair()
}
#[cfg(windows)]
pub async fn pair() -> std::io::Result<(Stream, Stream)> {
    windows::pair().await
}
