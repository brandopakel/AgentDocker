//! What herdr says of agents that report nothing of their own.
//!
//! A provider whose lifecycle hooks run says when a turn starts and when it
//! ends. One without them — Codex without its hooks, Gemini, another CLI —
//! leaves the daemon only contact, which is presence rather than work. When
//! such an agent lives in a herdr pane, herdr is watching its terminal and
//! marks it working, blocked, done or idle, so the daemon asks herdr.
//!
//! One background task keeps one watcher per herdr socket that some agent
//! needs. A watcher finds the pane holding each agent's provider session
//! (`pane.list`), subscribes to status changes in those panes and to agents
//! appearing or leaving anywhere (`events.subscribe`), and records what it
//! reads as the agent's activity, renewed well inside the observation's
//! expiry. It reads the panes again when an agent is detected or released,
//! when the agents it serves change, and on a timer; it reconnects with
//! backoff; and it ends, closing its subscription, when no agent needs that
//! socket. It never writes to herdr. Everything is bounded: the sockets
//! watched, the sessions one subscription follows, the length of a line,
//! and every connection, request and acknowledgement by a timeout.
//!
//! Which pane is an agent's is decided here once, for the bridge and for
//! the opt-in screen judge alike: [`herdr_binding`] names the socket and
//! the provider session; `sole_claimants` keeps a session only where one
//! agent alone claims it on that socket; and `held` reads `pane.list` and
//! keeps a session only where exactly one pane holds it. The judge's
//! `read_screen` makes the pane check immediately before and after each
//! `pane.read`, and the judge makes the agent check around it under the
//! state lock, so neither reader can take a pane the other would not.
// herdr on Windows listens on a named pipe and is not followed there.
#![cfg_attr(not(unix), allow(dead_code))]

use super::*;
use agentdocker_core::ActivitySource;
use agentdocker_core::multiplexer::herdr::Status;
#[cfg(unix)]
use agentdocker_core::multiplexer::herdr::{self, Held, Line};

/// Provider sessions to read from each herdr socket, and whose they are.
pub(crate) type Watches = BTreeMap<PathBuf, BTreeMap<String, AgentId>>;

/// Where a herdr session's socket is: herdr's own place under the person's
/// home in the daemon, a test's directory in tests.
pub(crate) type SocketOf = Arc<dyn Fn(Option<&str>) -> Option<PathBuf> + Send + Sync>;

/// A herdr observation saying the same thing is recorded again once it is
/// this old, so a renewal every [`Timing::refresh`] keeps it well inside
/// its five-minute expiry without an event for every repeated status.
const RENEW_AFTER: Duration = Duration::seconds(60);
/// herdr servers followed at once.
const MAX_SOCKETS: usize = 16;
/// Provider sessions one subscription follows.
const MAX_SESSIONS: usize = 64;
/// The longest `pane.list` reply read.
#[cfg(unix)]
const LIST_BYTES: usize = 4 * 1024 * 1024;
/// The longest subscription line read.
#[cfg(unix)]
const LINE_BYTES: usize = 64 * 1024;
/// The longest `pane.read` reply read: a visible screen, escapes stripped.
#[cfg(unix)]
const READ_BYTES: usize = 1024 * 1024;

impl State {
    /// The provider sessions herdr is asked about, by socket: each running
    /// agent recorded in a herdr session that names its provider session
    /// and whose hooks have not run in its current process. A provider
    /// session two such agents claim on one socket is neither's.
    pub(super) fn herdr_watches(&self, socket_of: &SocketOf) -> Watches {
        let mut watches = self.herdr_watched(socket_of);
        for sessions in watches.values_mut() {
            while sessions.len() > MAX_SESSIONS {
                sessions.pop_last();
            }
        }
        watches
    }

    /// The agents herdr is asked about, each the sole claimant of its
    /// provider session on its socket, before any bound on their number.
    fn herdr_watched(&self, socket_of: &SocketOf) -> Watches {
        sole_claimants(self.registry.all(), |record| {
            herdr_watchable(record, socket_of)
        })
    }

    /// The live agents whose herdr pane the screen judge may read, by
    /// socket and provider session: each bound to a herdr pane
    /// ([`herdr_binding`]) and the only live agent claiming its session
    /// there (`sole_claimants`). Hooks do not matter here, since the
    /// judge reads the screens of agents whose hooks run too.
    pub(super) fn herdr_bound(&self, socket_of: &SocketOf) -> Watches {
        sole_claimants(
            self.registry.all().filter(|record| record.status.is_live()),
            |record| herdr_binding(record, socket_of),
        )
    }

    /// Whether `agent` is, now, the only live agent claiming `session` on
    /// `socket`: the screen judge's check just before and just after each
    /// read of the pane, and when it records a judgment from it.
    pub(super) fn herdr_sole_claimant(
        &self,
        agent: &AgentId,
        socket: &Path,
        session: &str,
        socket_of: &SocketOf,
    ) -> bool {
        claimant(&self.herdr_bound(socket_of), socket, session) == Some(agent)
    }

    /// Record what herdr says of `agent`, whose provider session `session`
    /// a pane on `socket` holds, while it is still an agent herdr is asked
    /// about there and the word is news: another activity, or the same one
    /// due for renewal — still its session's only claimant there, so a
    /// second agent claiming the session since the panes were read makes
    /// the word neither's. While Codex's shared server has a current word
    /// on the agent, herdr's is not recorded: the server says what the
    /// conversation itself is doing, herdr what its terminal looks like.
    /// Whether it was recorded.
    pub(super) fn report_from_herdr(
        &mut self,
        agent: &AgentId,
        socket: &Path,
        session: &str,
        status: Status,
        socket_of: &SocketOf,
        now: DateTime<Utc>,
    ) -> bool {
        if claimant(&self.herdr_watched(socket_of), socket, session) != Some(agent) {
            return false;
        }
        let Some(record) = self.registry.get(agent) else {
            return false;
        };
        let Some(observation) = status.observation(now) else {
            return false;
        };
        if let Some(previous) = &record.reported_activity {
            if previous.observed_at >= now || previous.keeps_off(&observation, now) {
                return false;
            }
            if previous.source == Some(ActivitySource::Herdr)
                && previous.activity == observation.activity
                && previous.reason == observation.reason
                && now - previous.observed_at < RENEW_AFTER
            {
                return false;
            }
        }
        matches!(self.record_activity(agent, observation, now), Response::Ok)
    }
}

/// The socket and provider session herdr is asked about for `record`, if
/// it is one herdr is asked about: running, bound to a herdr pane
/// ([`herdr_binding`]), and with no hooks run in its current process,
/// since those report its turns themselves.
fn herdr_watchable(record: &AgentRecord, socket_of: &SocketOf) -> Option<(PathBuf, String)> {
    if record.status != AgentStatus::Running || super::waiting::reports_turns(record) {
        return None;
    }
    herdr_binding(record, socket_of)
}

/// The herdr socket and provider session that find `record`'s pane: it is
/// recorded in a herdr session whose socket can be named, and names its
/// provider session. A pane is found by that session (`held`), never by
/// a display name and never by the pane id the agent registered with,
/// which herdr changes when a pane moves. The status bridge and the screen
/// judge both bind an agent to its pane through this, and only this.
pub(crate) fn herdr_binding(
    record: &AgentRecord,
    socket_of: &SocketOf,
) -> Option<(PathBuf, String)> {
    let session = record.session.as_ref().filter(|s| s.kind == "herdr")?;
    let provider = record
        .spec
        .labels
        .get("session_id")
        .filter(|id| !id.is_empty())?;
    Some((socket_of(session.session.as_deref())?, provider.clone()))
}

/// Whose each provider session on each herdr socket is, among `records` as
/// `bind` binds them: one agent record per socket and session, and a
/// session two records claim on one socket is neither's. This is the
/// agent-side half of the identity rule, beside the pane-side half in
/// `held` (exactly one pane holds the session); the status bridge and the
/// screen judge both apply both halves, each to the agents it reads for.
fn sole_claimants<'a>(
    records: impl IntoIterator<Item = &'a AgentRecord>,
    bind: impl Fn(&AgentRecord) -> Option<(PathBuf, String)>,
) -> Watches {
    let mut claims = Watches::new();
    let mut twice = Vec::new();
    for record in records {
        let Some((socket, session)) = bind(record) else {
            continue;
        };
        match claims.entry(socket.clone()).or_default().entry(session) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(record.id.clone());
            }
            std::collections::btree_map::Entry::Occupied(entry) => {
                twice.push((socket, entry.key().clone()));
            }
        }
    }
    for (socket, session) in twice {
        if let Some(sessions) = claims.get_mut(&socket) {
            sessions.remove(&session);
        }
    }
    claims.retain(|_, sessions| !sessions.is_empty());
    claims
}

/// The one agent claiming `session` on `socket` in `claims`, if any.
pub(super) fn claimant<'a>(
    claims: &'a Watches,
    socket: &Path,
    session: &str,
) -> Option<&'a AgentId> {
    claims.get(socket)?.get(session)
}

impl Daemon {
    pub(crate) fn herdr_watches(&self, socket_of: &SocketOf) -> Watches {
        lock(&self.state).herdr_watches(socket_of)
    }

    fn report_from_herdr(
        &self,
        agent: &AgentId,
        socket: &Path,
        session: &str,
        status: Status,
        socket_of: &SocketOf,
    ) -> bool {
        lock(&self.state).report_from_herdr(agent, socket, session, status, socket_of, Utc::now())
    }
}

/// How often things happen; tests run them fast.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Timing {
    /// How often the agents herdr is asked about are looked at again.
    pub tick: std::time::Duration,
    /// How often the panes are read again while every session is found,
    /// renewing what herdr says well inside its five-minute expiry.
    pub refresh: std::time::Duration,
    /// How often while some session is in no pane yet: herdr announces no
    /// event when a pane's agent names its session.
    pub seek: std::time::Duration,
    /// The least time between two subscriptions to one socket.
    pub settle: std::time::Duration,
    /// Every connection, request and acknowledgement.
    pub timeout: std::time::Duration,
    /// The first wait after a failure, doubling up to the second.
    pub backoff: (std::time::Duration, std::time::Duration),
}

impl Default for Timing {
    fn default() -> Self {
        use std::time::Duration;
        Self {
            tick: Duration::from_secs(2),
            refresh: Duration::from_secs(120),
            seek: Duration::from_secs(10),
            settle: Duration::from_secs(1),
            timeout: Duration::from_secs(3),
            backoff: (Duration::from_secs(1), Duration::from_secs(60)),
        }
    }
}

/// Follow herdr for the agents that need it, for the daemon's life.
#[cfg(unix)]
pub(crate) fn spawn(daemon: Arc<Daemon>) {
    let socket_of: SocketOf = Arc::new(agentdocker_host::multiplexer::herdr_socket);
    tokio::spawn(run(daemon, Timing::default(), socket_of));
}

/// herdr on Windows listens on a named pipe; it is not followed there.
#[cfg(not(unix))]
pub(crate) fn spawn(_daemon: Arc<Daemon>) {}

/// One socket's watcher: the sessions it serves, and its task, which ends
/// with the watcher.
#[cfg(unix)]
struct Watcher {
    sessions: watch::Sender<BTreeMap<String, AgentId>>,
    task: tokio::task::JoinHandle<()>,
}

#[cfg(unix)]
impl Drop for Watcher {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[cfg(unix)]
pub(crate) async fn run(daemon: Arc<Daemon>, timing: Timing, socket_of: SocketOf) {
    let mut watchers: BTreeMap<PathBuf, Watcher> = BTreeMap::new();
    let mut crowded = false;
    let mut ticker = tokio::time::interval(timing.tick);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        let wanted = daemon.herdr_watches(&socket_of);
        // A socket no agent needs any more loses its watcher, and with it
        // the subscription.
        watchers
            .retain(|socket, watcher| wanted.contains_key(socket) && !watcher.task.is_finished());
        for (socket, sessions) in wanted {
            if let Some(watcher) = watchers.get(&socket) {
                watcher.sessions.send_if_modified(|current| {
                    let changed = *current != sessions;
                    if changed {
                        *current = sessions;
                    }
                    changed
                });
                continue;
            }
            if watchers.len() >= MAX_SOCKETS {
                if !crowded {
                    warn!(
                        limit = MAX_SOCKETS,
                        "more herdr sessions hold hookless agents than are followed at once"
                    );
                    crowded = true;
                }
                continue;
            }
            let (sessions, receiver) = watch::channel(sessions);
            let task = tokio::spawn(watch_socket(
                daemon.clone(),
                socket.clone(),
                receiver,
                timing,
                socket_of.clone(),
            ));
            watchers.insert(socket, Watcher { sessions, task });
        }
    }
}

/// How one subscription ended.
#[cfg(unix)]
enum Ended {
    /// No agent needs this socket any more.
    Gone,
    /// The panes or the agents changed: subscribe again.
    Again,
    /// herdr is not there, refused, or ended the subscription.
    Failed(String),
}

#[cfg(unix)]
async fn watch_socket(
    daemon: Arc<Daemon>,
    socket: PathBuf,
    mut sessions: watch::Receiver<BTreeMap<String, AgentId>>,
    timing: Timing,
    socket_of: SocketOf,
) {
    let mut backoff = timing.backoff.0;
    loop {
        let pause = match follow(&daemon, &socket, &mut sessions, &timing, &socket_of).await {
            Ended::Gone => return,
            Ended::Again => {
                backoff = timing.backoff.0;
                timing.settle
            }
            Ended::Failed(reason) => {
                debug!(socket = %socket.display(), %reason, "herdr not followed for now");
                let pause = backoff;
                backoff = (backoff * 2).min(timing.backoff.1);
                pause
            }
        };
        tokio::select! {
            () = tokio::time::sleep(pause) => {}
            changed = sessions.changed() => if changed.is_err() {
                return;
            },
        }
    }
}

/// One subscription, from finding the panes to its end.
#[cfg(unix)]
async fn follow(
    daemon: &Daemon,
    socket: &Path,
    sessions: &mut watch::Receiver<BTreeMap<String, AgentId>>,
    timing: &Timing,
    socket_of: &SocketOf,
) -> Ended {
    let wanted = sessions.borrow_and_update().clone();
    let held = match self::held(socket, wanted.keys().map(String::as_str), timing.timeout).await {
        Ok(held) => held,
        Err(reason) => return Ended::Failed(reason),
    };
    let mut stream = match subscribe(socket, &held, timing.timeout).await {
        Ok(stream) => stream,
        Err(reason) => return Ended::Failed(reason),
    };
    // Read again once changes are followed, so none made between the
    // first reading and the subscription is missed.
    match self::held(socket, wanted.keys().map(String::as_str), timing.timeout).await {
        Ok(current) => {
            if !same_panes(&held, &current) {
                return Ended::Again;
            }
            report(daemon, socket, &wanted, &current, socket_of);
        }
        Err(reason) => return Ended::Failed(reason),
    }
    let every = if held.len() < wanted.len() {
        timing.seek
    } else {
        timing.refresh
    };
    let renew = tokio::time::sleep(every);
    tokio::pin!(renew);
    loop {
        let recheck = tokio::select! {
            line = read_line(&mut stream, LINE_BYTES) => match line {
                Ok(Some(text)) => match herdr::line(&text) {
                    Line::Status { pane, status } => {
                        if let Some((session, _)) = held.iter().find(|(_, h)| h.pane == pane)
                            && let Some(agent) = wanted.get(session)
                        {
                            daemon.report_from_herdr(agent, socket, session, status, socket_of);
                        }
                        false
                    }
                    // An agent arrived in or left some pane: it may be one
                    // of these sessions, moved.
                    Line::Detected { .. } => true,
                    Line::Refused(reason) => return Ended::Failed(reason),
                    Line::Started | Line::Other => false,
                },
                Ok(None) => return Ended::Failed("herdr ended the subscription".into()),
                Err(error) => return Ended::Failed(error.to_string()),
            },
            () = &mut renew => true,
            changed = sessions.changed() => {
                return if changed.is_err() { Ended::Gone } else { Ended::Again };
            }
        };
        if recheck {
            match self::held(socket, wanted.keys().map(String::as_str), timing.timeout).await {
                Ok(current) => {
                    if !same_panes(&held, &current) {
                        return Ended::Again;
                    }
                    report(daemon, socket, &wanted, &current, socket_of);
                }
                Err(reason) => return Ended::Failed(reason),
            }
            renew.as_mut().reset(tokio::time::Instant::now() + every);
        }
    }
}

/// Whether two readings put every session in the same pane.
#[cfg(unix)]
fn same_panes(a: &BTreeMap<String, Held>, b: &BTreeMap<String, Held>) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|((sa, ha), (sb, hb))| sa == sb && ha.pane == hb.pane)
}

#[cfg(unix)]
fn report(
    daemon: &Daemon,
    socket: &Path,
    wanted: &BTreeMap<String, AgentId>,
    held: &BTreeMap<String, Held>,
    socket_of: &SocketOf,
) {
    for (session, held) in held {
        if let Some(agent) = wanted.get(session) {
            daemon.report_from_herdr(agent, socket, session, held.status, socket_of);
        }
    }
}

#[cfg(unix)]
type Stream = tokio::io::BufReader<tokio::net::UnixStream>;

#[cfg(unix)]
async fn connect(socket: &Path) -> Result<Stream, String> {
    tokio::net::UnixStream::connect(socket)
        .await
        .map(tokio::io::BufReader::new)
        .map_err(|error| format!("herdr is not answering: {error}"))
}

#[cfg(unix)]
async fn send(stream: &mut Stream, request: &Value) -> Result<(), String> {
    use tokio::io::AsyncWriteExt;
    stream
        .get_mut()
        .write_all(format!("{request}\n").as_bytes())
        .await
        .map_err(|error| format!("could not ask herdr: {error}"))
}

/// One request on a connection of its own, and herdr's one-line answer of
/// at most `limit` bytes, all within `timeout`.
#[cfg(unix)]
async fn ask(
    socket: &Path,
    request: &Value,
    limit: usize,
    timeout: std::time::Duration,
) -> Result<String, String> {
    let ask = async {
        let mut stream = connect(socket).await?;
        send(&mut stream, request).await?;
        read_line(&mut stream, limit)
            .await
            .map_err(|error| format!("herdr's answer could not be read: {error}"))?
            .ok_or_else(|| "herdr closed the connection without answering".to_owned())
    };
    tokio::time::timeout(timeout, ask)
        .await
        .map_err(|_| "herdr did not answer in time".to_owned())?
}

/// Every pane, by `pane.list` on a connection of its own.
#[cfg(unix)]
async fn list(socket: &Path, timeout: std::time::Duration) -> Result<Vec<herdr::Pane>, String> {
    herdr::panes(&ask(socket, &herdr::list_request(), LIST_BYTES, timeout).await?)
}

/// Which of `sessions` exactly one pane on `socket` holds, and where, as
/// `pane.list` says now: the one pane-identity check, made by the status
/// bridge for every reading and by the screen judge around every read.
#[cfg(unix)]
pub(crate) async fn held<'a>(
    socket: &Path,
    sessions: impl IntoIterator<Item = &'a str>,
    timeout: std::time::Duration,
) -> Result<BTreeMap<String, Held>, String> {
    Ok(herdr::holding(&list(socket, timeout).await?, sessions))
}

/// The pane holding `session` on `socket`, when exactly one does.
#[cfg(unix)]
async fn pane_holding(
    socket: &Path,
    session: &str,
    timeout: std::time::Duration,
) -> Result<String, String> {
    held(socket, [session], timeout)
        .await?
        .remove(session)
        .map(|held| held.pane)
        .ok_or_else(|| "no one pane holds the agent's session".to_owned())
}

/// What the pane holding `session` on `socket` shows, by `pane.read`, only
/// while that pane is the agent's: exactly one pane holds the session just
/// before the read, the reply is that pane's screen, and the same pane
/// alone still holds it just after. A pane moved (herdr gives it a new
/// id), replaced, released or claimed twice is never read, or its screen
/// is dropped. Each request is bounded by `timeout`, as the bridge's are.
#[cfg(unix)]
pub(crate) async fn read_screen(
    socket: &Path,
    session: &str,
    timeout: std::time::Duration,
) -> Result<String, String> {
    let pane = pane_holding(socket, session, timeout).await?;
    let reply = ask(socket, &herdr::read_request(&pane), READ_BYTES, timeout).await?;
    let text = herdr::screen(&reply, &pane)?;
    if pane_holding(socket, session, timeout).await? != pane {
        return Err("the agent's session moved to another pane while it was read".to_owned());
    }
    Ok(text)
}

/// herdr on Windows listens on a named pipe; no pane is read there.
#[cfg(not(unix))]
pub(crate) async fn read_screen(
    _socket: &Path,
    _session: &str,
    _timeout: std::time::Duration,
) -> Result<String, String> {
    Err("herdr is not followed on this platform".to_owned())
}

/// A subscription to agents appearing anywhere and to the status of each
/// held pane, open once herdr has acknowledged it.
#[cfg(unix)]
async fn subscribe(
    socket: &Path,
    held: &BTreeMap<String, Held>,
    timeout: std::time::Duration,
) -> Result<Stream, String> {
    let request = herdr::subscribe_request(held.values().map(|h| h.pane.as_str()));
    let open = async {
        let mut stream = connect(socket).await?;
        send(&mut stream, &request).await?;
        let ack = read_line(&mut stream, LINE_BYTES)
            .await
            .map_err(|error| format!("herdr's answer could not be read: {error}"))?
            .ok_or_else(|| "herdr closed the subscription unanswered".to_owned())?;
        match herdr::line(&ack) {
            Line::Started => Ok(stream),
            Line::Refused(reason) => Err(reason),
            _ => Err("herdr did not start the subscription".to_owned()),
        }
    };
    tokio::time::timeout(timeout, open)
        .await
        .map_err(|_| "herdr did not acknowledge the subscription in time".to_owned())?
}

/// One newline-ended line of at most `limit` bytes; `None` at a clean end.
#[cfg(unix)]
async fn read_line<R>(reader: &mut R, limit: usize) -> std::io::Result<Option<String>>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    use tokio::io::AsyncBufReadExt;
    let too_long = || std::io::Error::new(std::io::ErrorKind::InvalidData, "line too long");
    let mut line = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return if line.is_empty() {
                Ok(None)
            } else {
                Err(std::io::ErrorKind::UnexpectedEof.into())
            };
        }
        let (taken, ended) = match available.iter().position(|&b| b == b'\n') {
            Some(end) => (end, true),
            None => (available.len(), false),
        };
        if line.len() + taken > limit {
            return Err(too_long());
        }
        line.extend_from_slice(&available[..taken]);
        reader.consume(taken + usize::from(ended));
        if ended {
            return String::from_utf8(line)
                .map(Some)
                .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "not UTF-8"));
        }
    }
}

#[cfg(all(test, unix))]
pub(crate) mod fake;
#[cfg(test)]
mod tests;
