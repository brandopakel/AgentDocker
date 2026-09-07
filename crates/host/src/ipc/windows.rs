//! Byte-stream named pipes with explicit private ACLs and same-user peers.
use std::cell::Cell;
use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicBool, Ordering},
};
use std::task::{Context, Poll};
use std::time::Duration;

use crate::dirs::windows::{Access, Protection, same_user_process};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::windows::named_pipe::{
    ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
};
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore};
use windows_sys::Win32::{
    Foundation::{ERROR_PIPE_BUSY, HANDLE},
    Storage::FileSystem::SECURITY_IDENTIFICATION,
    System::Pipes::{GetNamedPipeClientProcessId, GetNamedPipeServerProcessId},
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(1);
// One additional pipe instance stays ready for the next connection. Windows
// allows 255 instances; retaining a permit bounds active connections below it.
const ACTIVE_CONNECTIONS: usize = 254;

fn server(path: &Path, first: bool) -> io::Result<NamedPipeServer> {
    crate::dirs::check_socket_parent(path)?;
    let protection = Protection::new()?;
    let mut attributes = protection.attributes();
    let mut options = ServerOptions::new();
    options
        .first_pipe_instance(first)
        .reject_remote_clients(true)
        .in_buffer_size(64 * 1024)
        .out_buffer_size(64 * 1024);
    // SAFETY: attributes and its descriptor stay valid throughout CreateNamedPipe.
    // The handles are not inheritable, and the ACL is supplied at creation.
    let pipe = unsafe {
        options.create_with_security_attributes_raw(
            path,
            (&mut attributes as *mut windows_sys::Win32::Security::SECURITY_ATTRIBUTES).cast(),
        )
    }?;
    protection.validate_access(pipe.as_raw_handle(), Access::Pipe)?;
    Ok(pipe)
}

fn peer(handle: HANDLE, server_end: bool) -> io::Result<()> {
    let mut pid = 0;
    // SAFETY: handle belongs to the live connected pipe and pid is an output.
    let ok = unsafe {
        if server_end {
            GetNamedPipeClientProcessId(handle, &mut pid)
        } else {
            GetNamedPipeServerProcessId(handle, &mut pid)
        }
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    same_user_process(pid)
}

async fn client(path: &Path) -> io::Result<NamedPipeClient> {
    crate::dirs::check_socket_parent(path)?;
    let deadline = tokio::time::Instant::now() + CONNECT_TIMEOUT;
    loop {
        // Identification is enough for peer verification; the server cannot
        // impersonate a connecting desktop/CLI with the client's privileges.
        match ClientOptions::new()
            .security_qos_flags(SECURITY_IDENTIFICATION)
            .open(path)
        {
            Ok(pipe) => {
                Protection::new()?.validate_access(pipe.as_raw_handle(), Access::Pipe)?;
                peer(pipe.as_raw_handle(), false)?;
                return Ok(pipe);
            }
            Err(error) if error.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => {
                if tokio::time::Instant::now() >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "named-pipe listener is busy",
                    ));
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Err(error) => return Err(error),
        }
    }
}

#[derive(Debug)]
pub struct Listener {
    path: PathBuf,
    waiting: tokio::sync::Mutex<NamedPipeServer>,
    capacity: Arc<Semaphore>,
}
impl Listener {
    pub fn bind(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref().to_owned();
        let waiting = server(&path, true)?;
        Ok(Self {
            path,
            waiting: tokio::sync::Mutex::new(waiting),
            capacity: Arc::new(Semaphore::new(ACTIVE_CONNECTIONS)),
        })
    }

    pub async fn accept(&self) -> io::Result<(Stream, ())> {
        let permit = self
            .capacity
            .clone()
            .acquire_owned()
            .await
            .map_err(io::Error::other)?;
        let mut waiting = self.waiting.lock().await;
        loop {
            waiting.connect().await?;
            // Keep an instance available before handing this one to its caller.
            // The existing handle reserves the name while the next is created.
            let next = server(&self.path, false)?;
            let connected = std::mem::replace(&mut *waiting, next);
            if peer(connected.as_raw_handle(), true).is_err() {
                // A peer can exit before the identity query. Reject only that
                // connection, preserving the listener for subsequent clients.
                continue;
            }
            return Ok((
                Stream::Server {
                    pipe: connected,
                    _permit: permit,
                },
                (),
            ));
        }
    }
}

#[derive(Debug)]
pub enum Stream {
    Closed,
    Client(NamedPipeClient),
    Server {
        pipe: NamedPipeServer,
        _permit: OwnedSemaphorePermit,
    },
}
pub type OwnedReadHalf = tokio::io::ReadHalf<Stream>;
pub type OwnedWriteHalf = tokio::io::WriteHalf<Stream>;
impl Stream {
    pub async fn connect(path: impl AsRef<Path>) -> io::Result<Self> {
        client(path.as_ref()).await.map(Self::Client)
    }
    pub fn into_split(self) -> (OwnedReadHalf, OwnedWriteHalf) {
        tokio::io::split(self)
    }
}
impl AsyncRead for Stream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Closed => Poll::Ready(Ok(())),
            Self::Client(pipe) => Pin::new(pipe).poll_read(cx, buffer),
            Self::Server { pipe, .. } => Pin::new(pipe).poll_read(cx, buffer),
        }
    }
}
impl AsyncWrite for Stream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Self::Closed => Poll::Ready(Err(closed())),
            Self::Client(pipe) => Pin::new(pipe).poll_write(cx, bytes),
            Self::Server { pipe, .. } => Pin::new(pipe).poll_write(cx, bytes),
        }
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Closed => Poll::Ready(Err(closed())),
            Self::Client(pipe) => Pin::new(pipe).poll_flush(cx),
            Self::Server { pipe, .. } => Pin::new(pipe).poll_flush(cx),
        }
    }
    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        // Named pipes have no stream half-close. Explicit shutdown closes the
        // connection and wakes the peer, including when split handles remain.
        *self.get_mut() = Self::Closed;
        Poll::Ready(Ok(()))
    }
}

pub async fn pair() -> io::Result<(Stream, Stream)> {
    let path = PathBuf::from(format!(
        r"\\.\pipe\agentdocker-pair-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let listener = Listener::bind(&path)?;
    let connected = Stream::connect(&path).await?;
    let (accepted, ()) = listener.accept().await?;
    Ok((connected, accepted))
}

// The desktop's socket workers are blocking threads. A shared runtime drives
// overlapped pipe readiness without adding a runtime/thread for each GUI call.
fn runtime() -> io::Result<&'static tokio::runtime::Runtime> {
    static RUNTIME: OnceLock<Result<tokio::runtime::Runtime, String>> = OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .thread_name("agentdocker-ipc")
                .build()
                .map_err(|error| error.to_string())
        })
        .as_ref()
        .map_err(|error| io::Error::other(error.clone()))
}

#[derive(Debug)]
struct BlockingShared {
    pipe: Mutex<Option<Arc<NamedPipeClient>>>,
    closed: AtomicBool,
    wake: Notify,
}
#[derive(Debug)]
pub struct BlockingStream {
    shared: Arc<BlockingShared>,
    read_timeout: Cell<Option<Duration>>,
    write_timeout: Cell<Option<Duration>>,
}
impl BlockingStream {
    pub fn connect(path: impl AsRef<Path>) -> io::Result<Self> {
        let pipe = runtime()?.block_on(client(path.as_ref()))?;
        Ok(Self {
            shared: Arc::new(BlockingShared {
                pipe: Mutex::new(Some(Arc::new(pipe))),
                closed: AtomicBool::new(false),
                wake: Notify::new(),
            }),
            read_timeout: Cell::new(None),
            write_timeout: Cell::new(None),
        })
    }
    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        check_timeout(timeout)?;
        self.read_timeout.set(timeout);
        Ok(())
    }
    pub fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        check_timeout(timeout)?;
        self.write_timeout.set(timeout);
        Ok(())
    }
    pub fn try_clone(&self) -> io::Result<Self> {
        if self.shared.closed.load(Ordering::Acquire) {
            return Err(closed());
        }
        Ok(Self {
            shared: self.shared.clone(),
            read_timeout: self.read_timeout.clone(),
            write_timeout: self.write_timeout.clone(),
        })
    }
    /// The desktop uses Both to cancel a terminal. Wake all cloned readers and
    /// writers; they release their temporary handle references as they return.
    pub fn shutdown(&self, how: Shutdown) -> io::Result<()> {
        if how != Shutdown::Both {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "named-pipe half-close is unavailable",
            ));
        }
        self.shared.closed.store(true, Ordering::Release);
        self.shared
            .pipe
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        self.shared.wake.notify_waiters();
        Ok(())
    }
    fn pipe(&self) -> io::Result<Arc<NamedPipeClient>> {
        self.shared
            .pipe
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
            .ok_or_else(closed)
    }
}
fn closed() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "local IPC connection is closed")
}
fn check_timeout(timeout: Option<Duration>) -> io::Result<()> {
    if timeout.is_some_and(|duration| duration.is_zero()) {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "timeout must be nonzero",
        ))
    } else {
        Ok(())
    }
}
async fn bounded<T>(
    duration: Option<Duration>,
    future: impl Future<Output = io::Result<T>>,
) -> io::Result<T> {
    match duration {
        Some(duration) => tokio::time::timeout(duration, future).await.map_err(|_| {
            io::Error::new(io::ErrorKind::TimedOut, "local IPC operation timed out")
        })?,
        None => future.await,
    }
}
impl Read for BlockingStream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() || self.shared.closed.load(Ordering::Acquire) {
            return Ok(0);
        }
        let pipe = match self.pipe() {
            Ok(pipe) => pipe,
            Err(_) if self.shared.closed.load(Ordering::Acquire) => return Ok(0),
            Err(error) => return Err(error),
        };
        runtime()?.block_on(bounded(self.read_timeout.get(), async {
            loop {
                let wake = self.shared.wake.notified();
                tokio::pin!(wake);
                wake.as_mut().enable();
                if self.shared.closed.load(Ordering::Acquire) {
                    return Ok(0);
                }
                tokio::select! {
                    _ = &mut wake => continue,
                    ready = pipe.readable() => ready?,
                }
                match pipe.try_read(buffer) {
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => continue,
                    Err(error) if error.kind() == io::ErrorKind::BrokenPipe => return Ok(0),
                    result => return result,
                }
            }
        }))
    }
}
impl Write for BlockingStream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        let pipe = self.pipe()?;
        runtime()?.block_on(bounded(self.write_timeout.get(), async {
            loop {
                let wake = self.shared.wake.notified();
                tokio::pin!(wake);
                wake.as_mut().enable();
                if self.shared.closed.load(Ordering::Acquire) {
                    return Err(closed());
                }
                tokio::select! {
                    _ = &mut wake => continue,
                    ready = pipe.writable() => ready?,
                }
                match pipe.try_write(bytes) {
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => continue,
                    result => return result,
                }
            }
        }))
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn name() -> PathBuf {
        PathBuf::from(format!(
            r"\\.\pipe\agentdocker-test-{}",
            uuid::Uuid::new_v4().simple()
        ))
    }

    #[tokio::test]
    async fn private_pipe_reserves_its_name_and_transfers_beyond_the_pipe_buffer() {
        tokio::time::timeout(Duration::from_secs(10), async {
            let path = name();
            let listener = Listener::bind(&path).unwrap();
            assert!(
                Listener::bind(&path).is_err(),
                "the first instance owns the name"
            );
            let mut client = Stream::connect(&path).await.unwrap();
            let (mut server, ()) = listener.accept().await.unwrap();
            let sent = vec![0x6d; 512 * 1024];
            let mut received = vec![0; sent.len()];
            let ((), _) =
                tokio::try_join!(client.write_all(&sent), server.read_exact(&mut received))
                    .unwrap();
            assert_eq!(sent, received);
            client.shutdown().await.unwrap();
            assert_eq!(server.read(&mut [0; 1]).await.unwrap(), 0);
            drop(server);
            drop(listener);
            // Mio retains pending overlapped operations until their cancelled
            // IOCP completions run. Dropping handles is not a synchronous name
            // release; keep the surrounding ten-second bound while driving I/O.
            loop {
                match Listener::bind(&path) {
                    Ok(_) => break,
                    Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
                        tokio::time::sleep(Duration::from_millis(1)).await;
                    }
                    Err(error) => panic!("closed pipe could not be rebound: {error}"),
                }
            }
        })
        .await
        .expect("pipe transfer is bounded");
    }

    #[tokio::test]
    async fn admission_backpressure_is_cancel_safe() {
        tokio::time::timeout(Duration::from_secs(5), async {
            let path = name();
            let mut listener = Listener::bind(&path).unwrap();
            listener.capacity = Arc::new(Semaphore::new(1));
            let first = Stream::connect(&path).await.unwrap();
            let (accepted, ()) = listener.accept().await.unwrap();
            let mut second = Stream::connect(&path).await.unwrap();
            assert!(
                tokio::time::timeout(Duration::from_millis(20), listener.accept())
                    .await
                    .is_err()
            );
            drop(accepted);
            drop(first);
            let (mut accepted, ()) = listener.accept().await.unwrap();
            second.write_all(b"next").await.unwrap();
            let mut bytes = [0; 4];
            accepted.read_exact(&mut bytes).await.unwrap();
            assert_eq!(&bytes, b"next");
        })
        .await
        .expect("admission must resume after a connection closes");
    }

    #[tokio::test]
    async fn a_public_pipe_acl_is_rejected_before_sending_application_data() {
        use std::ptr::null_mut;
        use windows_sys::Win32::Security::{
            Authorization::{
                ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
            },
            SECURITY_ATTRIBUTES,
        };
        let path = name();
        let sid = crate::dirs::windows::current_sid().unwrap();
        let sddl: Vec<_> = format!("O:{sid}D:P(A;;GA;;;SY)(A;;GA;;;{sid})(A;;GR;;;WD)")
            .encode_utf16()
            .chain([0])
            .collect();
        let mut descriptor = null_mut();
        assert_ne!(
            unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    sddl.as_ptr(),
                    SDDL_REVISION_1,
                    &mut descriptor,
                    null_mut(),
                )
            },
            0
        );
        let mut attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        };
        let result = unsafe {
            ServerOptions::new()
                .first_pipe_instance(true)
                .reject_remote_clients(true)
                .create_with_security_attributes_raw(
                    &path,
                    (&mut attributes as *mut SECURITY_ATTRIBUTES).cast(),
                )
        };
        unsafe {
            windows_sys::Win32::Foundation::LocalFree(descriptor);
        }
        let _server = result.unwrap();
        let error = Stream::connect(&path).await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn desktop_read_timeout_and_cloned_connection_cancellation_are_bounded() {
        tokio::time::timeout(Duration::from_secs(10), async {
            let path = name();
            let listener = Listener::bind(&path).unwrap();
            let worker = tokio::task::spawn_blocking(move || {
                let mut connection = BlockingStream::connect(&path).unwrap();
                connection
                    .set_read_timeout(Some(Duration::from_millis(20)))
                    .unwrap();
                assert_eq!(
                    Read::read(&mut connection, &mut [0; 1]).unwrap_err().kind(),
                    io::ErrorKind::TimedOut
                );
                connection.set_read_timeout(None).unwrap();
                let cancel = connection.try_clone().unwrap();
                let (sender, receiver) = std::sync::mpsc::channel();
                let reader = std::thread::spawn(move || {
                    sender
                        .send(Read::read(&mut connection, &mut [0; 1]))
                        .unwrap();
                });
                cancel.shutdown(Shutdown::Both).unwrap();
                assert_eq!(
                    receiver
                        .recv_timeout(Duration::from_secs(2))
                        .unwrap()
                        .unwrap(),
                    0
                );
                reader.join().unwrap();
                assert!(cancel.try_clone().is_err());
            });
            let (mut accepted, ()) = listener.accept().await.unwrap();
            worker.await.unwrap();
            assert_eq!(accepted.read(&mut [0; 1]).await.unwrap(), 0);
        })
        .await
        .expect("desktop cancellation must release its connection");
    }
}
