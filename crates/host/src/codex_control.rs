//! Codex's shared app-server control socket.
//!
//! Codex 0.162 and later runs a terminal's conversations in one shared
//! app-server (`codex app-server --listen unix:// --managed-daemon`), and its
//! terminals reach it at `$CODEX_HOME/app-server-control/app-server-control.sock`:
//! JSON-RPC over a WebSocket on that Unix socket. This module finds the
//! socket, refuses one another user could have placed or replaced, opens the
//! WebSocket, and speaks the read-only part of the protocol AgentDocker uses
//! to see which conversations are loaded and whether one waits on the person
//! (`thread/loaded/list`, `thread/read`, `thread/status/changed`).
//!
//! A profile without the socket has no shared server, and callers change
//! nothing. This client never answers a server request: approvals and
//! questions belong to the person's terminal.

use std::{
    io,
    path::{Path, PathBuf},
};

use agentdocker_core::{ReportedActivity, WaitReason};
use serde_json::Value;

/// The directory under the profile that holds the socket's name.
pub const DIRECTORY: &str = "app-server-control";
/// The socket's name in [`DIRECTORY`].
pub const SOCKET: &str = "app-server-control.sock";
/// The largest frame either side may send on one of these connections.
pub const MAX_FRAME: usize = 4 * 1024 * 1024;
/// The request URI Codex's own terminal sends on the Unix socket.
pub const REQUEST_URI: &str = "ws://localhost/";

/// Notifications a read-only client does not want: streamed output it would
/// only discard. Status changes stay on.
pub const OPT_OUT: &[&str] = &[
    "thread/started",
    "thread/tokenUsage/updated",
    "remoteControl/status/changed",
    "item/agentMessage/delta",
    "item/reasoning/textDelta",
    "item/reasoning/summaryTextDelta",
    "item/commandExecution/outputDelta",
    "item/fileChange/outputDelta",
    "turn/diff/updated",
    "turn/plan/updated",
];

/// Where `profile` names its shared server's socket, whether or not it is there.
pub fn socket_path(profile: &Path) -> PathBuf {
    profile.join(DIRECTORY).join(SOCKET)
}

#[cfg(unix)]
fn refused(path: &Path, why: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!("Codex control socket {}: {why}", path.display()),
    )
}

/// The shared server's socket for `profile`, resolved, when the profile has
/// one: `Ok(None)` when it has none (or names one that is gone), an error when
/// it has one this user plainly cannot trust. The name may be a link, as Codex
/// makes it, into a private directory elsewhere. Both the directory holding the
/// name and the one holding the socket must be this user's (or root's) and
/// refuse other users' replacements; the socket itself must be this user's.
///
/// These checks name the socket; they do not prove whose process answers on
/// it. An ancestor another user can rename would let them put their own socket
/// at the same path after this lookup. [`connect_stream`] closes that: the
/// connected peer must be this user's process, by the kernel's credentials,
/// before a byte is sent.
#[cfg(unix)]
pub fn locate(profile: &Path) -> io::Result<Option<PathBuf>> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let directory = profile.join(DIRECTORY);
    let entry = directory.join(SOCKET);
    let named = match std::fs::symlink_metadata(&entry) {
        Ok(meta) => meta,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    // SAFETY: geteuid has no preconditions and cannot fail.
    let me = unsafe { libc::geteuid() };
    guarded_directory(&directory, me)?;
    let target = if named.file_type().is_symlink() {
        if named.uid() != me {
            return Err(refused(&entry, "the link is not this user's"));
        }
        let to = std::fs::read_link(&entry)?;
        let to = if to.is_absolute() {
            to
        } else {
            directory.join(to)
        };
        let name = to
            .file_name()
            .ok_or_else(|| refused(&entry, "the link names no socket"))?
            .to_owned();
        let parent = match to.parent().map(Path::canonicalize) {
            Some(Ok(parent)) => parent,
            // The server that made the link is gone with its directory.
            Some(Err(error)) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Some(Err(error)) => return Err(error),
            None => return Err(refused(&entry, "the link names no directory")),
        };
        guarded_directory(&parent, me)?;
        parent.join(name)
    } else {
        entry.clone()
    };
    let socket = match std::fs::symlink_metadata(&target) {
        Ok(meta) => meta,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !socket.file_type().is_socket() {
        return Err(refused(&target, "it is not a socket"));
    }
    if socket.uid() != me {
        return Err(refused(&target, "the socket is not this user's"));
    }
    Ok(Some(target))
}

/// Windows has no such socket; nothing changes there.
#[cfg(not(unix))]
pub fn locate(_profile: &Path) -> io::Result<Option<PathBuf>> {
    Ok(None)
}

/// A directory nobody but this user (or root) can put a socket name into:
/// owned by this user or root, a real directory, and either writable by its
/// owner alone or sticky, so another user cannot replace an entry there.
#[cfg(unix)]
fn guarded_directory(path: &Path, me: u32) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::symlink_metadata(path)?;
    if !meta.is_dir() {
        return Err(refused(path, "it is not a directory"));
    }
    if meta.uid() != me && meta.uid() != 0 {
        return Err(refused(path, "the directory is another user's"));
    }
    let shared = meta.mode() & 0o022 != 0;
    let sticky = meta.mode() & 0o1000 != 0;
    if shared && !sticky {
        return Err(refused(
            path,
            "other users can replace entries in this directory",
        ));
    }
    Ok(())
}

/// A conversation's status, as the shared server says it in `thread/read`
/// and `thread/status/changed` (`ThreadStatus`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ThreadStatus {
    NotLoaded,
    Idle,
    SystemError,
    /// A turn is running. The flags say it stopped for the person: an
    /// approval (`waitingOnApproval`) or a question (`waitingOnUserInput`).
    Active {
        waiting_on_approval: bool,
        waiting_on_user_input: bool,
    },
}

impl ThreadStatus {
    /// The status in `value`, or `None` for a shape this build does not know.
    /// Flags it does not know are ignored: the turn is still running.
    pub fn parse(value: &Value) -> Option<Self> {
        match value["type"].as_str()? {
            "notLoaded" => Some(Self::NotLoaded),
            "idle" => Some(Self::Idle),
            "systemError" => Some(Self::SystemError),
            "active" => {
                let flags = value["activeFlags"].as_array()?;
                let has = |flag: &str| flags.iter().any(|f| f.as_str() == Some(flag));
                Some(Self::Active {
                    waiting_on_approval: has("waitingOnApproval"),
                    waiting_on_user_input: has("waitingOnUserInput"),
                })
            }
            _ => None,
        }
    }

    /// What it says about the agent, as an adapter would report it: a stop for
    /// the person (an approval before a question), a running turn, or quiet.
    /// A conversation that is not loaded, or failed, says nothing.
    pub fn activity(&self) -> Option<(ReportedActivity, Option<WaitReason>)> {
        match self {
            Self::Idle => Some((ReportedActivity::Idle, None)),
            Self::Active {
                waiting_on_approval: true,
                ..
            } => Some((ReportedActivity::Waiting, Some(WaitReason::Permission))),
            Self::Active {
                waiting_on_user_input: true,
                ..
            } => Some((ReportedActivity::Waiting, Some(WaitReason::Question))),
            Self::Active { .. } => Some((ReportedActivity::Working, None)),
            Self::NotLoaded | Self::SystemError => None,
        }
    }

    /// A word for a listing.
    pub fn label(&self) -> &'static str {
        match self {
            Self::NotLoaded => "not loaded",
            Self::Idle => "idle",
            Self::SystemError => "system error",
            Self::Active {
                waiting_on_approval: true,
                ..
            } => "waiting on approval",
            Self::Active {
                waiting_on_user_input: true,
                ..
            } => "waiting on user input",
            Self::Active { .. } => "active",
        }
    }
}

/// One `thread/status/changed` notification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusChange {
    pub thread: String,
    pub status: ThreadStatus,
}

/// One loaded conversation, from `thread/read` without its turns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Thread {
    pub id: String,
    pub cwd: Option<PathBuf>,
    /// Set for a child (sub-agent) conversation.
    pub parent: Option<String>,
    pub status: ThreadStatus,
}

/// The server's own error answer to one request: it answered, so the
/// connection stands, and the request did nothing.
#[derive(Debug)]
pub struct Rejection {
    pub method: String,
    pub code: Option<i64>,
    pub message: String,
}

impl std::fmt::Display for Rejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Codex refused {}: {}", self.method, self.message)
    }
}

impl std::error::Error for Rejection {}

/// A well-formed answer this client cannot use for one conversation: a read
/// of another conversation, or a status this build does not know.
#[derive(Debug)]
pub struct Unusable(pub String);

impl std::fmt::Display for Unusable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Unusable {}

/// The server's error answer in `error`, when it is one rather than a failed
/// connection or a malformed frame.
pub fn rejection(error: &io::Error) -> Option<&Rejection> {
    error.get_ref()?.downcast_ref::<Rejection>()
}

/// Whether the server answered the request, with its own error or with a
/// result this client cannot use: about that request alone, the connection
/// standing. A failed, closed or confused connection is not answered.
pub fn answered(error: &io::Error) -> bool {
    rejection(error).is_some() || error.get_ref().is_some_and(|e| e.is::<Unusable>())
}

/// Whether `error` is the shared server saying this one conversation is not
/// loaded: `thread/read` answers an unloaded thread with code -32600 and the
/// message `thread not loaded: <thread>`. Nothing else is taken for it.
pub fn not_loaded(error: &io::Error, thread: &str) -> bool {
    rejection(error).is_some_and(|rejection| {
        rejection.code == Some(-32600)
            && rejection
                .message
                .strip_prefix("thread not loaded: ")
                .is_some_and(|named| named == thread)
    })
}

#[cfg(unix)]
pub use unix::{Session, connect, connect_stream};

/// The profile whose shared server runs from `executable`, a process's
/// kernel image, canonical: Codex runs its managed daemon from its own copy
/// in the profile's package cache,
/// `<profile>/packages/app-server-daemon/releases/<version>/bin/codex`, so
/// the image's path says whose server it is. `None` for any other path. It
/// is not proof by itself: [`serving_process`] says which process serves
/// the profile.
pub fn shared_server_profile(executable: &Path) -> Option<PathBuf> {
    if !executable.is_absolute() {
        return None;
    }
    let parts: Vec<_> = executable.ancestors().take(6).collect();
    let named = |part: usize, name: &str| parts[part].file_name().is_some_and(|v| v == name);
    (parts.len() == 6
        && matches!(
            executable.file_name().and_then(|v| v.to_str()),
            Some("codex" | "codex.exe")
        )
        && named(1, "bin")
        && named(3, "releases")
        && named(4, "app-server-daemon")
        && named(5, "packages"))
    .then(|| parts[5].parent().map(Path::to_owned))
    .flatten()
}

/// The process serving `profile`'s shared server, by the kernel's
/// credentials for the process on the other end of its control socket
/// ([`locate`], [`connect_stream`]), on a connection closed at once with
/// nothing sent: `Ok(None)` when the profile has no shared server (and on
/// Windows, which has no such socket).
pub async fn serving_process(profile: &Path) -> io::Result<Option<u32>> {
    #[cfg(unix)]
    {
        let Some(socket) = locate(profile)? else {
            return Ok(None);
        };
        let stream = connect_stream(&socket).await?;
        Ok(stream
            .peer_cred()?
            .pid()
            .and_then(|pid| u32::try_from(pid).ok())
            .filter(|pid| *pid != 0))
    }
    #[cfg(not(unix))]
    {
        let _ = profile;
        Ok(None)
    }
}

/// Windows has no shared server's socket: a session never opens there.
#[cfg(not(unix))]
pub struct Session {
    never: std::convert::Infallible,
    pub initialized: Value,
}

#[cfg(not(unix))]
impl Session {
    pub async fn open(_profile: &Path) -> io::Result<Option<Self>> {
        Ok(None)
    }

    pub async fn request(&mut self, _method: &str, _params: Value) -> io::Result<Value> {
        match self.never {}
    }

    pub async fn loaded(&mut self) -> io::Result<Vec<String>> {
        match self.never {}
    }

    pub async fn read(&mut self, _thread: &str) -> io::Result<Thread> {
        match self.never {}
    }

    pub async fn next_change(&mut self) -> io::Result<StatusChange> {
        match self.never {}
    }

    pub async fn close(self) {
        match self.never {}
    }
}

#[cfg(unix)]
mod unix {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use serde_json::json;
    use std::{collections::VecDeque, time::Duration};
    use tokio::{net::UnixStream, time::timeout};
    use tokio_tungstenite::{
        WebSocketStream, client_async_with_config,
        tungstenite::{Message, client::IntoClientRequest, protocol::WebSocketConfig},
    };

    /// How long a connection, a handshake or one request may take.
    const TIMEOUT: Duration = Duration::from_secs(10);
    /// Status changes kept between reads; older ones are superseded anyway.
    const MAX_CHANGES: usize = 256;
    /// Pages of `thread/loaded/list` read at most, and their size.
    const MAX_PAGES: usize = 64;
    const PAGE: u32 = 256;

    fn invalid(what: impl Into<String>) -> io::Error {
        io::Error::new(io::ErrorKind::InvalidData, what.into())
    }

    /// A thread id as the server gives it: bounded, printable.
    fn plain_id(id: &str) -> bool {
        !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control)
    }

    enum Frame {
        Answer(Value),
        Noted,
    }

    fn closed() -> io::Error {
        io::Error::new(io::ErrorKind::UnexpectedEof, "Codex control socket closed")
    }

    fn timed_out(what: &str) -> io::Error {
        io::Error::new(io::ErrorKind::TimedOut, what.to_owned())
    }

    /// Connect to a socket [`locate`] returned and refuse it, before anything
    /// is sent, unless the kernel says the process on the other end runs as
    /// this user. Every connection to a shared server is made here.
    pub async fn connect_stream(socket: &Path) -> io::Result<UnixStream> {
        // SAFETY: geteuid has no preconditions and cannot fail.
        connect_stream_as(socket, unsafe { libc::geteuid() }).await
    }

    /// [`connect_stream`] for a given user, so a test can stand for another.
    pub(super) async fn connect_stream_as(socket: &Path, uid: u32) -> io::Result<UnixStream> {
        let stream = timeout(TIMEOUT, UnixStream::connect(socket))
            .await
            .map_err(|_| timed_out("Codex control socket timed out"))??;
        crate::ipc::require_peer_uid(&stream, uid)
            .map_err(|_| refused(socket, "the process serving it is another user's"))?;
        Ok(stream)
    }

    /// Open the WebSocket on a socket [`locate`] returned, bounded like every
    /// other provider connection: no proxy, no redirect, no body echoed.
    pub async fn connect(socket: &Path) -> io::Result<WebSocketStream<UnixStream>> {
        let request = REQUEST_URI
            .into_client_request()
            .map_err(|_| invalid("invalid control request"))?;
        let config = WebSocketConfig::default()
            .read_buffer_size(8192)
            .write_buffer_size(0)
            .max_write_buffer_size(MAX_FRAME + 8192)
            .max_message_size(Some(MAX_FRAME))
            .max_frame_size(Some(MAX_FRAME));
        let stream = connect_stream(socket).await?;
        timeout(TIMEOUT, async {
            let (socket, _) = client_async_with_config(request, stream, Some(config))
                .await
                .map_err(|_| io::Error::other("Codex control socket handshake failed"))?;
            Ok(socket)
        })
        .await
        .map_err(|_| timed_out("Codex control socket timed out"))?
    }

    /// A read-only JSON-RPC session with the shared server of one profile.
    pub struct Session {
        socket: WebSocketStream<UnixStream>,
        sequence: u64,
        changes: VecDeque<StatusChange>,
        /// What the server said in `initialize`.
        pub initialized: Value,
    }

    impl Session {
        /// Find the profile's socket and open a session on it: `Ok(None)` when
        /// the profile has no shared server. The server must say it serves this
        /// profile (`codexHome`), canonically.
        pub async fn open(profile: &Path) -> io::Result<Option<Self>> {
            let Some(socket) = locate(profile)? else {
                return Ok(None);
            };
            let mut session = Self {
                socket: connect(&socket).await?,
                sequence: 0,
                changes: VecDeque::new(),
                initialized: Value::Null,
            };
            let initialized = session
                .request(
                    "initialize",
                    json!({
                        "clientInfo": {
                            "name": "agentdocker_codex_threads",
                            "title": "AgentDocker",
                            "version": env!("CARGO_PKG_VERSION"),
                        },
                        "capabilities": {"optOutNotificationMethods": OPT_OUT},
                    }),
                )
                .await?;
            let serves = initialized["codexHome"]
                .as_str()
                .and_then(|home| Path::new(home).canonicalize().ok());
            if serves.is_none() || serves != profile.canonicalize().ok() {
                let _ = timeout(Duration::from_secs(2), session.socket.close(None)).await;
                return Err(refused(&socket, "the server serves another profile"));
            }
            session.notify("initialized", json!({})).await?;
            session.initialized = initialized;
            Ok(Some(session))
        }

        async fn send(&mut self, value: &Value) -> io::Result<()> {
            let text = value.to_string();
            if text.len() > MAX_FRAME {
                return Err(invalid("Codex control request exceeds the frame limit"));
            }
            timeout(TIMEOUT, self.socket.send(Message::text(text)))
                .await
                .map_err(|_| timed_out("Codex control write timed out"))?
                .map_err(|_| io::Error::other("Codex control write failed"))
        }

        async fn notify(&mut self, method: &str, params: Value) -> io::Result<()> {
            self.send(&json!({"method": method, "params": params}))
                .await
        }

        /// The next protocol frame: an answer, or a notification taken in.
        /// Control frames are answered by the library; a binary frame, a close
        /// or a server request ends the session, since this client answers
        /// nothing.
        async fn frame(&mut self) -> io::Result<Frame> {
            loop {
                let message = self
                    .socket
                    .next()
                    .await
                    .ok_or_else(closed)?
                    .map_err(|_| io::Error::other("Codex control frame failed"))?;
                match message {
                    Message::Text(text) => {
                        let value: Value = serde_json::from_slice(text.as_bytes())
                            .map_err(|_| invalid("Codex control sent invalid JSON"))?;
                        if value.get("id").is_some() && value.get("method").is_some() {
                            return Err(invalid(
                                "the shared Codex server asked a read-only client to answer a request",
                            ));
                        }
                        if value.get("id").is_none() {
                            self.note(&value);
                            return Ok(Frame::Noted);
                        }
                        return Ok(Frame::Answer(value));
                    }
                    Message::Ping(_) | Message::Pong(_) => {
                        timeout(TIMEOUT, self.socket.flush())
                            .await
                            .map_err(|_| timed_out("Codex control timed out"))?
                            .map_err(|_| io::Error::other("Codex control frame failed"))?;
                    }
                    Message::Close(_) => return Err(closed()),
                    _ => return Err(invalid("Codex control sent a non-text frame")),
                }
            }
        }

        /// Keep a status change, the newest last; drop every other notification.
        fn note(&mut self, value: &Value) {
            if value["method"] != "thread/status/changed" {
                return;
            }
            let params = &value["params"];
            let (Some(thread), Some(status)) = (
                params["threadId"].as_str().filter(|id| plain_id(id)),
                ThreadStatus::parse(&params["status"]),
            ) else {
                return;
            };
            if self.changes.len() == MAX_CHANGES {
                self.changes.pop_front();
            }
            self.changes.push_back(StatusChange {
                thread: thread.to_owned(),
                status,
            });
        }

        /// One request and its answer; the server's error is an error here.
        pub async fn request(&mut self, method: &str, params: Value) -> io::Result<Value> {
            self.sequence += 1;
            let id = self.sequence;
            self.send(&json!({"id": id, "method": method, "params": params}))
                .await?;
            let answer = timeout(TIMEOUT, async {
                loop {
                    if let Frame::Answer(value) = self.frame().await? {
                        return Ok::<_, io::Error>(value);
                    }
                }
            })
            .await
            .map_err(|_| timed_out(&format!("Codex {method} timed out")))??;
            if answer["id"].as_u64() != Some(id) {
                return Err(invalid("Codex control answered another request"));
            }
            if let Some(error) = answer.get("error") {
                return Err(io::Error::other(Rejection {
                    method: method.to_owned(),
                    code: error["code"].as_i64(),
                    message: error["message"]
                        .as_str()
                        .unwrap_or("no reason given")
                        .chars()
                        .take(1024)
                        .collect(),
                }));
            }
            answer
                .get("result")
                .cloned()
                .ok_or_else(|| invalid("Codex control answer has no result"))
        }

        /// The ids of the conversations loaded in the server, every page.
        pub async fn loaded(&mut self) -> io::Result<Vec<String>> {
            let mut threads = Vec::new();
            let mut cursor: Option<String> = None;
            let mut seen = std::collections::HashSet::new();
            for _ in 0..MAX_PAGES {
                let page = self
                    .request(
                        "thread/loaded/list",
                        json!({"cursor": cursor, "limit": PAGE}),
                    )
                    .await?;
                let data = page["data"]
                    .as_array()
                    .ok_or_else(|| invalid("thread/loaded/list has no data"))?;
                if data.len() > PAGE as usize {
                    return Err(invalid("thread/loaded/list exceeded its page size"));
                }
                for id in data {
                    let id = id
                        .as_str()
                        .filter(|id| plain_id(id))
                        .ok_or_else(|| invalid("thread/loaded/list named an invalid thread"))?;
                    threads.push(id.to_owned());
                }
                cursor = match page.get("nextCursor") {
                    None | Some(Value::Null) => return Ok(threads),
                    Some(Value::String(next)) if !next.is_empty() && next.len() <= 4096 => {
                        if !seen.insert(next.clone()) {
                            return Err(invalid("thread/loaded/list repeated a cursor"));
                        }
                        Some(next.clone())
                    }
                    _ => return Err(invalid("thread/loaded/list returned an invalid cursor")),
                };
            }
            Err(invalid("thread/loaded/list exceeded the page limit"))
        }

        /// One conversation's id, checkout, parent and status, without turns.
        pub async fn read(&mut self, thread: &str) -> io::Result<Thread> {
            let value = self
                .request(
                    "thread/read",
                    json!({"threadId": thread, "includeTurns": false}),
                )
                .await?;
            let read = &value["thread"];
            let unusable =
                |what: &str| io::Error::new(io::ErrorKind::InvalidData, Unusable(what.to_owned()));
            if read["id"].as_str() != Some(thread) {
                return Err(unusable("thread/read answered another conversation"));
            }
            let status = ThreadStatus::parse(&read["status"])
                .ok_or_else(|| unusable("thread/read has no known status"))?;
            Ok(Thread {
                id: thread.to_owned(),
                cwd: read["cwd"].as_str().map(PathBuf::from),
                parent: read["parentThreadId"].as_str().map(str::to_owned),
                status,
            })
        }

        /// The next status change the server announced, waiting for one.
        /// Between requests nothing but notifications may arrive.
        pub async fn next_change(&mut self) -> io::Result<StatusChange> {
            loop {
                if let Some(change) = self.changes.pop_front() {
                    return Ok(change);
                }
                if let Frame::Answer(value) = self.frame().await? {
                    return Err(invalid(format!(
                        "Codex control sent an unrequested answer: {}",
                        value["id"]
                    )));
                }
            }
        }

        /// Close this connection only; the server and its terminals go on.
        pub async fn close(mut self) {
            let _ = timeout(Duration::from_secs(2), self.socket.close(None)).await;
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use serde_json::json;
    use std::os::unix::fs::PermissionsExt;
    use tokio::net::UnixListener;
    use tokio_tungstenite::tungstenite::Message;

    /// A short root: a socket path may have only 104 bytes on macOS.
    fn root() -> tempfile::TempDir {
        let base = if cfg!(target_os = "macos") {
            PathBuf::from("/tmp")
        } else {
            std::env::temp_dir()
        };
        tempfile::Builder::new()
            .prefix("cc-")
            .tempdir_in(base)
            .unwrap()
    }

    fn private(path: &Path) {
        std::fs::create_dir_all(path).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    /// A shared server's profile comes only from its image's place in that
    /// profile's package cache.
    #[test]
    fn a_shared_server_s_profile_is_named_by_its_image_s_package_cache() {
        let profile = PathBuf::from("/home/p/.codex");
        for binary in ["codex", "codex.exe"] {
            let image = profile
                .join("packages/app-server-daemon/releases/0.162.0-target/bin")
                .join(binary);
            assert_eq!(shared_server_profile(&image), Some(profile.clone()));
        }
        for suffix in [
            "bin/codex",
            "packages/other-server/releases/version/bin/codex",
            "packages/app-server-daemon/releases/version/other/codex",
            "packages/app-server-daemon/releases/version/bin/python3",
        ] {
            assert_eq!(
                shared_server_profile(&profile.join(suffix)),
                None,
                "{suffix}"
            );
        }
        assert_eq!(
            shared_server_profile(Path::new(
                "relative/packages/app-server-daemon/releases/v/bin/codex"
            )),
            None
        );
    }

    /// The process serving a profile's control socket is the one the kernel
    /// names on the other end of a connection; a profile without the
    /// socket has none.
    #[tokio::test]
    async fn the_process_serving_a_profile_s_socket_is_named_by_the_kernel() {
        let root = root();
        let profile = root.path().join("p");
        private(&profile.join(DIRECTORY));
        assert_eq!(serving_process(&profile).await.unwrap(), None);
        let _listener = UnixListener::bind(socket_path(&profile)).unwrap();
        assert_eq!(
            serving_process(&profile).await.unwrap(),
            Some(std::process::id())
        );
    }

    #[test]
    fn statuses_map_to_what_an_adapter_reports() {
        let parse = |v: Value| ThreadStatus::parse(&v).unwrap();
        assert_eq!(
            parse(
                json!({"type":"active","activeFlags":["waitingOnApproval","waitingOnUserInput"]})
            )
            .activity(),
            Some((ReportedActivity::Waiting, Some(WaitReason::Permission))),
            "an approval before a question"
        );
        assert_eq!(
            parse(json!({"type":"active","activeFlags":["waitingOnUserInput"]})).activity(),
            Some((ReportedActivity::Waiting, Some(WaitReason::Question)))
        );
        assert_eq!(
            parse(json!({"type":"active","activeFlags":["somethingNewer"]})).activity(),
            Some((ReportedActivity::Working, None)),
            "an unknown flag is a running turn"
        );
        assert_eq!(
            parse(json!({"type":"idle"})).activity(),
            Some((ReportedActivity::Idle, None))
        );
        assert_eq!(parse(json!({"type":"notLoaded"})).activity(), None);
        assert_eq!(parse(json!({"type":"systemError"})).activity(), None);
        assert_eq!(ThreadStatus::parse(&json!({"type":"later"})), None);
        assert_eq!(ThreadStatus::parse(&json!({"type":"active"})), None);
    }

    /// Only the server's own "thread not loaded" answer for this very
    /// conversation is taken for it, as Codex 0.163 answers `thread/read`
    /// for a thread it has not loaded.
    #[test]
    fn only_the_servers_not_loaded_answer_for_this_thread_means_not_loaded() {
        let answer = |code: Option<i64>, message: &str| {
            io::Error::other(Rejection {
                method: "thread/read".into(),
                code,
                message: message.into(),
            })
        };
        assert!(not_loaded(
            &answer(Some(-32600), "thread not loaded: t1"),
            "t1"
        ));
        assert!(!not_loaded(
            &answer(Some(-32600), "thread not loaded: t2"),
            "t1"
        ));
        assert!(!not_loaded(
            &answer(Some(-32603), "thread not loaded: t1"),
            "t1"
        ));
        assert!(!not_loaded(
            &answer(Some(-32600), "thread not found: t1"),
            "t1"
        ));
        assert!(!not_loaded(&answer(None, "thread not loaded: t1"), "t1"));
        assert!(!not_loaded(
            &io::Error::other("thread not loaded: t1"),
            "t1"
        ));
        assert!(answered(&answer(Some(-32603), "boom")));
        assert!(answered(&io::Error::new(
            io::ErrorKind::InvalidData,
            Unusable("no known status".into())
        )));
        assert!(!answered(&io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "closed"
        )));
        assert!(!answered(&io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid JSON"
        )));
    }

    #[test]
    fn an_absent_or_gone_socket_is_none_and_an_untrusted_one_is_refused() {
        let root = root();
        let profile = root.path().join("profile");
        std::fs::create_dir_all(&profile).unwrap();
        assert!(locate(&profile).unwrap().is_none(), "no directory");
        let directory = profile.join(DIRECTORY);
        private(&directory);
        assert!(locate(&profile).unwrap().is_none(), "no name");
        // A link into a private directory elsewhere, as Codex makes it.
        let elsewhere = root.path().join("daemon");
        private(&elsewhere);
        let target = elsewhere.join("abc");
        std::os::unix::fs::symlink(&target, directory.join(SOCKET)).unwrap();
        assert!(locate(&profile).unwrap().is_none(), "a server that is gone");
        let _listener = std::os::unix::net::UnixListener::bind(&target).unwrap();
        assert_eq!(
            locate(&profile).unwrap(),
            Some(elsewhere.canonicalize().unwrap().join("abc"))
        );
        // Another user could replace the socket there.
        std::fs::set_permissions(&elsewhere, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert_eq!(
            locate(&profile).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        std::fs::set_permissions(&elsewhere, std::fs::Permissions::from_mode(0o1777)).unwrap();
        assert!(locate(&profile).unwrap().is_some(), "a sticky directory");
        std::fs::set_permissions(&elsewhere, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o770)).unwrap();
        assert!(locate(&profile).is_err(), "the name's directory is shared");
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        // A file where the socket should be.
        std::fs::remove_file(directory.join(SOCKET)).unwrap();
        std::fs::write(directory.join(SOCKET), b"").unwrap();
        assert!(locate(&profile).is_err());
    }

    /// The fake shared server: answers `initialize` for `home`, two pages of
    /// loaded threads and their reads, then announces a change.
    async fn serve(listener: UnixListener, home: PathBuf) {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        while let Some(Ok(Message::Text(text))) = socket.next().await {
            let request: Value = serde_json::from_str(&text).unwrap();
            let Some(id) = request.get("id").cloned() else {
                continue;
            };
            let params = &request["params"];
            let result = match request["method"].as_str().unwrap() {
                "initialize" => {
                    assert!(
                        params["capabilities"]["optOutNotificationMethods"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .all(|m| m != "thread/status/changed"),
                        "status changes stay on"
                    );
                    json!({"codexHome": home, "userAgent": "codex_cli_rs/0.163.0 (test)",
                        "platformFamily": "unix", "platformOs": "test"})
                }
                "thread/loaded/list" if params["cursor"].is_null() => {
                    json!({"data": ["root-a"], "nextCursor": "page-2"})
                }
                "thread/loaded/list" => json!({"data": ["root-b"], "nextCursor": null}),
                "thread/read" => {
                    // A notification before the answer is kept, not lost.
                    socket
                        .send(Message::text(
                            json!({"method":"thread/status/changed","params":{"threadId":"root-b",
                                "status":{"type":"active","activeFlags":["waitingOnUserInput"]}}})
                            .to_string(),
                        ))
                        .await
                        .unwrap();
                    json!({"thread": {"id": params["threadId"], "cwd": "/checkout",
                        "parentThreadId": null,
                        "status": {"type":"active","activeFlags":["waitingOnApproval"]}}})
                }
                other => panic!("unexpected {other}"),
            };
            socket
                .send(Message::text(
                    json!({"id": id, "result": result}).to_string(),
                ))
                .await
                .unwrap();
        }
    }

    /// The process on the other end must be this user's: one the kernel says
    /// runs as another user is refused before a single byte is sent to it.
    /// (A test cannot run a server as another user, so it asks for one.)
    #[tokio::test]
    async fn a_server_run_by_another_user_is_refused_before_anything_is_sent() {
        use tokio::io::AsyncReadExt;
        let root = root();
        let path = root.path().join("control.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut received = Vec::new();
            stream.read_to_end(&mut received).await.unwrap();
            received
        });
        // SAFETY: geteuid has no preconditions and cannot fail.
        let me = unsafe { libc::geteuid() };
        let other = me.wrapping_add(1);
        let error = unix::connect_stream_as(&path, other).await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(error.to_string().contains("another user"), "{error}");
        assert!(server.await.unwrap().is_empty(), "nothing was sent");
        // The same server, run by this user, is let through.
        let listener = {
            std::fs::remove_file(&path).unwrap();
            UnixListener::bind(&path).unwrap()
        };
        let accepted = tokio::spawn(async move { listener.accept().await.map(|_| ()) });
        unix::connect_stream_as(&path, me).await.unwrap();
        accepted.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn a_session_lists_loaded_threads_reads_status_and_keeps_changes() {
        let root = root();
        let profile = root.path().join("profile");
        private(&profile.join(DIRECTORY));
        let listener = UnixListener::bind(socket_path(&profile)).unwrap();
        let server = tokio::spawn(serve(listener, profile.canonicalize().unwrap()));
        let mut session = Session::open(&profile).await.unwrap().unwrap();
        assert_eq!(session.loaded().await.unwrap(), ["root-a", "root-b"]);
        let thread = session.read("root-a").await.unwrap();
        assert_eq!(
            thread.status,
            ThreadStatus::Active {
                waiting_on_approval: true,
                waiting_on_user_input: false
            }
        );
        assert_eq!(thread.cwd.as_deref(), Some(Path::new("/checkout")));
        assert_eq!(
            session.next_change().await.unwrap(),
            StatusChange {
                thread: "root-b".into(),
                status: ThreadStatus::Active {
                    waiting_on_approval: false,
                    waiting_on_user_input: true
                }
            }
        );
        session.close().await;
        server.await.unwrap();
    }

    #[tokio::test]
    async fn a_server_for_another_profile_or_asking_for_an_answer_is_refused() {
        let root = root();
        let profile = root.path().join("profile");
        private(&profile.join(DIRECTORY));
        let listener = UnixListener::bind(socket_path(&profile)).unwrap();
        let other = root.path().to_path_buf();
        let server = tokio::spawn(serve(listener, other));
        assert_eq!(
            Session::open(&profile).await.err().unwrap().kind(),
            io::ErrorKind::PermissionDenied
        );
        server.await.unwrap();

        std::fs::remove_file(socket_path(&profile)).unwrap();
        let listener = UnixListener::bind(socket_path(&profile)).unwrap();
        let home = profile.canonicalize().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let Some(Ok(Message::Text(text))) = socket.next().await else {
                panic!("initialize");
            };
            let request: Value = serde_json::from_str(&text).unwrap();
            socket
                .send(Message::text(
                    json!({"id": request["id"], "result": {"codexHome": home}}).to_string(),
                ))
                .await
                .unwrap();
            // An approval asked of every connection is never answered here.
            socket
                .send(Message::text(
                    json!({"id": 7, "method": "item/commandExecution/requestApproval",
                        "params": {}})
                    .to_string(),
                ))
                .await
                .unwrap();
            while let Some(Ok(message)) = socket.next().await {
                if let Message::Text(text) = message {
                    let value: Value = serde_json::from_str(&text).unwrap();
                    assert!(value.get("result").is_none(), "no answer: {value}");
                }
            }
        });
        let mut session = Session::open(&profile).await.unwrap().unwrap();
        assert!(session.loaded().await.is_err());
        session.close().await;
        server.await.unwrap();
    }
}
