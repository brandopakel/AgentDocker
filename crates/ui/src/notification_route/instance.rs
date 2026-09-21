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

// JSON can encode one control character as six bytes. Reserve the full
// supported recovery payload, its action and framing; forward/handle still
// enforce the complete serialized byte limit, independently of character limits.
const MAX_REQUEST: usize = agentdocker_host::notify::ACTION_BYTES
    + 6 * (super::RECOVERY_CHARS + super::RECOVERY_REASON_CHARS)
    + 1024;
const REQUEST_TIME: Duration = Duration::from_millis(500);
/// How many times a launch takes the instance lock again after finding it
/// on a file that was removed under it.
const LOCK_ATTEMPTS: u32 = 3;

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
    start_locked(home, daemon, initial, handler, lock::try_exclusive_existing)
}

/// `start_with`, taking the instance lock through `take`: the real lock, or
/// a test's stand-in for one whose file goes between its open and its lock.
fn start_locked(
    home: &Path,
    daemon: &Path,
    initial: Activation,
    handler: Handler,
    mut take: impl FnMut(&Path) -> io::Result<Option<lock::Lock>>,
) -> io::Result<Launch> {
    let (directory, socket) = location(home, daemon);
    let lock_path = directory.join("instance.lock");
    // A lock is on a file, not a name. A window that is exiting removes
    // its lock file and directory while it still holds the lock; a launch
    // arriving meanwhile can find them gone between its own steps, or can
    // lock a file that was unlinked between its open and its lock, which
    // excludes nobody. Either way it starts over, the directory made
    // afresh, a bounded number of times.
    let mut attempts = 0;
    let held = loop {
        attempts += 1;
        let taken = dirs::ensure_private_dir(&directory)
            .and_then(|()| dirs::private_file(&lock_path, true, false).map(drop))
            .and_then(|()| take(&lock_path));
        match taken {
            Ok(Some(held)) if held.is_at(&lock_path)? => break held,
            Ok(Some(_)) => {}
            Ok(None) => {
                forward(home, daemon, &initial)?;
                return Ok(Launch::Forwarded);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        if attempts == LOCK_ATTEMPTS {
            return Err(io::Error::other(
                "desktop activation lock kept changing under this launch",
            ));
        }
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
        // Nothing of ours stays behind: the lock file goes while we still
        // hold it, and the directory when it is empty. A launch that has
        // meanwhile made its own lock and socket keeps its directory.
        let _ = std::fs::remove_file(&lock_path);
        let _ = std::fs::remove_dir(&directory);
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
                    Activation::ReplyFailed {
                        action,
                        text,
                        reason,
                        ..
                    } => {
                        action.home == home
                            && action.socket == daemon
                            && text.chars().count() <= super::RECOVERY_CHARS
                            && reason.chars().count() <= super::RECOVERY_REASON_CHARS
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
        let (directory, _) = location(home.path(), &daemon);
        assert!(!directory.exists(), "{}", directory.display());
    }

    #[test]
    fn a_launch_whose_lock_file_went_takes_the_lock_on_the_file_the_name_has_now() {
        let home = tempfile::tempdir().unwrap();
        let daemon = home.path().join("daemon.sock");
        let (directory, _) = location(home.path(), &daemon);
        // What an exiting window does while it still holds the lock,
        // landing on this launch twice: the file and directory go before
        // the launch opens the lock (a missing path), then a file the
        // launch has opened goes before it locks it (a lock on nothing).
        // The launch starts over each time and holds the file the name has
        // by then.
        let mut takes = 0;
        let taken = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = taken.clone();
        let Launch::Primary(first) = start_locked(
            home.path(),
            &daemon,
            Activation::Focus,
            Arc::new(|_| Ok(())),
            |path| {
                takes += 1;
                if takes == 1 {
                    std::fs::remove_file(path)?;
                    std::fs::remove_dir(path.parent().unwrap())?;
                }
                let held = lock::try_exclusive_existing(path)?;
                if takes == 2 {
                    std::fs::remove_file(path)?;
                    std::fs::remove_dir(path.parent().unwrap())?;
                }
                seen.lock().unwrap().push(takes);
                Ok(held)
            },
        )
        .unwrap() else {
            panic!("primary")
        };
        assert_eq!(
            *taken.lock().unwrap(),
            vec![2, 3],
            "the first take found no file"
        );
        assert!(
            lock::try_exclusive_existing(&directory.join("instance.lock"))
                .unwrap()
                .is_none(),
            "the launch holds the lock the name has now"
        );
        drop(first);
        assert!(!directory.exists());

        // A file that keeps going is given up on, not looped on.
        let error = start_locked(
            home.path(),
            &daemon,
            Activation::Focus,
            Arc::new(|_| Ok(())),
            |path| {
                let held = lock::try_exclusive_existing(path)?;
                std::fs::remove_file(path)?;
                Ok(held)
            },
        )
        .err()
        .expect("a lock that never settles is an error");
        assert!(error.to_string().contains("kept changing"), "{error}");
    }

    #[test]
    fn failed_reply_forwarding_preserves_full_unicode_and_escaped_text_within_bounds() {
        let home = tempfile::tempdir().unwrap();
        let daemon = home.path().join("daemon.sock");
        let (tx, rx) = std::sync::mpsc::channel();
        let handler: Handler =
            Arc::new(move |activation| tx.send(activation).map_err(|e| e.to_string()));
        let Launch::Primary(first) =
            start_with(home.path(), &daemon, Activation::Focus, handler).unwrap()
        else {
            panic!("first instance")
        };
        assert_eq!(rx.recv().unwrap(), Activation::Focus);
        let mut activation = Activation::ReplyFailed {
            action: agentdocker_host::notify::Action {
                home: home.path().to_owned(),
                socket: daemon.clone(),
                target: agentdocker_core::NotificationTarget {
                    message: agentdocker_core::MessageId::from("message".to_owned()),
                    agent: agentdocker_core::AgentId::from("agent"),
                    project: None,
                    channel: None,
                },
            },
            text: "\0".repeat(super::super::RECOVERY_CHARS - 1) + "🦀",
            reason: "\0".repeat(super::super::RECOVERY_REASON_CHARS),
            certain: false,
        };
        assert!(
            serde_json::to_vec(&activation).unwrap().len()
                > agentdocker_host::notify::ACTION_BYTES + 1024
        );
        forward(home.path(), &daemon, &activation).unwrap();
        assert_eq!(rx.recv().unwrap(), activation);
        if let Activation::ReplyFailed { text, .. } = &mut activation {
            text.push('x');
        }
        assert!(forward(home.path(), &daemon, &activation).is_err());
        assert!(rx.try_recv().is_err());
        if let Activation::ReplyFailed { text, .. } = &mut activation {
            *text = "x".repeat(MAX_REQUEST);
        }
        assert!(forward(home.path(), &daemon, &activation).is_err());
        assert!(rx.try_recv().is_err());
        drop(first);
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
