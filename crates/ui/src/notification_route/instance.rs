//! Private activation IPC: one window per daemon origin, with bounded requests.
use super::Activation;
use agentdocker_host::{dirs, ipc, lock};
use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

const MAX_REQUEST: usize = agentdocker_host::notify::ACTION_BYTES + 1024;
const REQUEST_TIME: Duration = Duration::from_millis(500);

pub enum Launch {
    Primary(Instance),
    Forwarded,
}

pub struct Instance {
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Drop for Instance {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn location(home: &Path, daemon: &Path) -> (PathBuf, PathBuf) {
    let key = agentdocker_host::notify::instance_key(home, daemon);
    #[cfg(unix)]
    {
        let directory = PathBuf::from("/tmp").join(format!("agentdocker-ui-{key}"));
        let socket = directory.join("activate.sock");
        (directory, socket)
    }
    #[cfg(windows)]
    {
        (
            home.join("ui").join(&key),
            PathBuf::from(format!(r"\\.\pipe\agentdocker-ui-{key}")),
        )
    }
}

pub fn start(home: &Path, daemon: &Path, initial: Activation) -> io::Result<Launch> {
    start_with(home, daemon, initial, Arc::new(super::enqueue))
}

/// A native callback can reach a window belonging to another daemon origin.
/// Start the same executable with child-only origin settings; its normal
/// single-instance handshake forwards to the right window or opens that origin.
#[cfg(target_os = "macos")]
pub fn open_origin(action: &agentdocker_host::notify::Action) -> Result<(), String> {
    let encoded = serde_json::to_string(action).map_err(|e| e.to_string())?;
    agentdocker_host::notify::Action::parse(&encoded)?;
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    let home = action.home.clone();
    let socket = action.socket.clone();
    std::thread::Builder::new()
        .name("desktop-open-origin".into())
        .spawn(move || {
            let outcome = std::process::Command::new(executable)
                .args(["--open-notification", &encoded])
                .env("AGENTDOCKER_HOME", home)
                .env("AGENTDOCKER_SOCKET", socket)
                .stdin(std::process::Stdio::null())
                .status();
            if !matches!(outcome, Ok(status) if status.success()) {
                eprintln!("Could not open the notification's desktop workspace.");
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}

type Handler = Arc<dyn Fn(Activation) -> Result<(), String> + Send + Sync>;

fn start_with(
    home: &Path,
    daemon: &Path,
    initial: Activation,
    handler: Handler,
) -> io::Result<Launch> {
    let (directory, socket) = location(home, daemon);
    dirs::ensure_private_dir(&directory)?;
    let lock_path = directory.join("instance.lock");
    dirs::private_file(&lock_path, true, false)?;
    let Some(held) = lock::try_exclusive_existing(&lock_path)? else {
        forward(home, daemon, &initial)?;
        return Ok(Launch::Forwarded);
    };
    remove_stale(&socket, &directory)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let listener = {
        let _entered = runtime.enter();
        ipc::Listener::bind(&socket)?
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(unix)]
    let owned = std::fs::symlink_metadata(&socket)?;
    handler(initial).map_err(io::Error::other)?;
    let home = home.to_owned();
    let daemon = daemon.to_owned();
    let (stop, mut stopped) = tokio::sync::oneshot::channel();
    let thread = std::thread::Builder::new().name("desktop-activation".into()).spawn(move || {
        let _held = held;
        runtime.block_on(async {
            loop {
                tokio::select! {
                    _ = &mut stopped => break,
                    accepted = listener.accept() => {
                        let Ok((stream, _)) = accepted else { break };
                        let _ = tokio::time::timeout(REQUEST_TIME, handle(stream, &home, &daemon, &handler)).await;
                    }
                }
            }
        });
        drop(listener);
        #[cfg(unix)]
        remove_owned(&socket, &owned);
    })?;
    Ok(Launch::Primary(Instance {
        stop: Some(stop),
        thread: Some(thread),
    }))
}

async fn handle(
    mut stream: ipc::Stream,
    home: &Path,
    daemon: &Path,
    handler: &Handler,
) -> io::Result<()> {
    let mut bytes = Vec::new();
    BufReader::new((&mut stream).take(MAX_REQUEST as u64 + 1))
        .read_until(b'\n', &mut bytes)
        .await?;
    let accepted = bytes.len() <= MAX_REQUEST
        && bytes.last() == Some(&b'\n')
        && serde_json::from_slice::<Activation>(&bytes)
            .ok()
            .is_some_and(|activation| {
                let valid = match &activation {
                    Activation::Open(action) => {
                        action.home == home
                            && action.socket == daemon
                            && serde_json::to_string(action).ok().is_some_and(|s| {
                                agentdocker_host::notify::Action::parse(&s).is_ok()
                            })
                    }
                    Activation::Focus | Activation::Inbox => true,
                };
                valid && handler(activation).is_ok()
            });
    stream
        .write_all(if accepted { b"ok\n" } else { b"no\n" })
        .await
}

pub fn forward(home: &Path, daemon: &Path, activation: &Activation) -> io::Result<()> {
    let (directory, socket) = location(home, daemon);
    // Connecting never creates state. This also refuses symlinked/foreign directories.
    std::fs::symlink_metadata(&directory)?;
    dirs::ensure_private_dir(&directory)?;
    let mut bytes = serde_json::to_vec(activation).map_err(io::Error::other)?;
    bytes.push(b'\n');
    if bytes.len() > MAX_REQUEST {
        return Err(io::Error::other("notification navigation is too large"));
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(2), async {
            let mut stream = loop {
                match checked_connect(&socket, &directory).await {
                    Ok(stream) => break stream,
                    Err(e)
                        if matches!(
                            e.kind(),
                            io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
                        ) =>
                    {
                        tokio::time::sleep(Duration::from_millis(25)).await;
                    }
                    Err(e) => return Err(e),
                }
            };
            stream.write_all(&bytes).await?;
            let mut reply = [0; 3];
            stream.read_exact(&mut reply).await?;
            if &reply == b"ok\n" {
                Ok(())
            } else {
                Err(io::Error::other(
                    "existing window could not queue navigation",
                ))
            }
        })
        .await
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                "existing window did not accept navigation",
            )
        })?
    })
}

async fn checked_connect(socket: &Path, directory: &Path) -> io::Result<ipc::Stream> {
    #[cfg(unix)]
    check_socket(socket, directory)?;
    #[cfg(windows)]
    let _ = directory;
    ipc::Stream::connect(socket).await
}

#[cfg(unix)]
fn check_socket(socket: &Path, directory: &Path) -> io::Result<()> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let metadata = std::fs::symlink_metadata(socket)?;
    if !metadata.file_type().is_socket() || metadata.uid() != std::fs::metadata(directory)?.uid() {
        return Err(io::Error::other(
            "desktop activation path is not an owned socket",
        ));
    }
    Ok(())
}

fn remove_stale(socket: &Path, directory: &Path) -> io::Result<()> {
    #[cfg(unix)]
    match std::fs::symlink_metadata(socket) {
        Ok(_) => {
            check_socket(socket, directory)?;
            std::fs::remove_file(socket)?;
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => (),
        Err(e) => return Err(e),
    }
    #[cfg(windows)]
    let _ = (socket, directory);
    Ok(())
}

#[cfg(unix)]
fn remove_owned(socket: &Path, owned: &std::fs::Metadata) {
    use std::os::unix::fs::MetadataExt;
    if let Ok(current) = std::fs::symlink_metadata(socket)
        && current.dev() == owned.dev()
        && current.ino() == owned.ino()
    {
        let _ = std::fs::remove_file(socket);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_launch_forwards_and_shutdown_releases_only_its_endpoint() {
        let home = tempfile::tempdir().unwrap();
        let daemon = home.path().join("daemon.sock");
        let (tx, rx) = std::sync::mpsc::channel();
        let handler: Handler = Arc::new(move |action| tx.send(action).map_err(|e| e.to_string()));
        let Launch::Primary(first) =
            start_with(home.path(), &daemon, Activation::Focus, handler.clone()).unwrap()
        else {
            panic!("first instance")
        };
        assert_eq!(rx.recv().unwrap(), Activation::Focus);
        assert!(matches!(
            start_with(home.path(), &daemon, Activation::Inbox, handler.clone()).unwrap(),
            Launch::Forwarded
        ));
        assert_eq!(rx.recv().unwrap(), Activation::Inbox);
        drop(first);
        let Launch::Primary(next) =
            start_with(home.path(), &daemon, Activation::Focus, handler).unwrap()
        else {
            panic!("replacement instance")
        };
        drop(next);
    }

    #[test]
    fn queue_refusal_is_not_reported_as_success() {
        let home = tempfile::tempdir().unwrap();
        let daemon = home.path().join("daemon.sock");
        let handler: Handler = Arc::new(|action| {
            if action == Activation::Focus {
                Ok(())
            } else {
                Err("busy".into())
            }
        });
        let Launch::Primary(first) =
            start_with(home.path(), &daemon, Activation::Focus, handler).unwrap()
        else {
            panic!("first instance")
        };
        assert!(forward(home.path(), &daemon, &Activation::Inbox).is_err());
        drop(first);
    }

    #[cfg(unix)]
    #[test]
    fn existing_regular_files_are_preserved_and_never_used_as_activation_sockets() {
        let home = tempfile::tempdir().unwrap();
        let daemon = home.path().join("daemon.sock");
        let (directory, socket) = location(home.path(), &daemon);
        dirs::ensure_private_dir(&directory).unwrap();
        std::fs::write(&socket, b"preserve").unwrap();
        assert!(
            start_with(
                home.path(),
                &daemon,
                Activation::Focus,
                Arc::new(|_| Ok(()))
            )
            .is_err()
        );
        assert_eq!(std::fs::read(&socket).unwrap(), b"preserve");
        std::fs::remove_file(socket).unwrap();
    }
}
