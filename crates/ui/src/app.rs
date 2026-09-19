//! The window: what it shows, how it asks the daemon, and how it keeps
//! up. Requests run on a worker thread and the event stream on another;
//! both hand results to the UI thread through a channel and ask for a
//! repaint, so the window never blocks on the socket.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};

mod board;
mod icons;
mod messages;
pub(crate) mod panes;
pub(crate) mod queue;
mod send_readiness;
mod sessions;
mod shell;
pub(crate) mod style;
mod usage;
mod view;
use queue::{Receiver as CommandReceiver, Sender as CommandSender};
pub use shell::Message;
use std::time::{Duration, Instant};

use crate::wake::Wake;
use agentdocker_core::journal::ago;
use agentdocker_core::{
    Activity, AgentActivity, AgentRecord, DiscoveredProcess, Event, EventKind, JournalEntry, Lease,
    MessageId, ProjectRef, Question, Request, Response, RuntimeInfo,
};
use anyhow::Context;
use chrono::Utc;

use crate::client::{Client, RemoteError};
use crate::terminal::{Status, Terminal};

/// How often agents, leases and discovered processes are re-read.
const REFRESH: Duration = Duration::from_secs(2);
/// How often the runtime inventory is re-read (it asks each CLI).
const RUNTIMES_REFRESH: Duration = Duration::from_secs(30);
const JOURNAL_WINDOW: usize = 200;
/// One page of a conversation's archive, and of a thread's replies.
const HISTORY_PAGE: usize = 200;
/// The most of one conversation's archive the window keeps: the daemon's
/// own per-conversation cap, so paging back never runs past what the window
/// can hold. Beyond it (a daemon with a larger cap) the earliest go and
/// earlier pages can be asked for again.
const HISTORY_KEEP: usize = 5_000;
/// How many conversations' archives the window keeps.
const HISTORY_CONVERSATIONS: usize = 32;
/// The most replies one thread is read to, the archive's own cap.
const THREAD_CAP: usize = 5_000;
const CONSOLE_BYTES: usize = 256 * 1024;
pub(crate) const MESSAGE_CAPACITY: usize = 64;
const SENT_CHANNEL_LIMIT: usize = 128;
const SENT_CHANNEL_BYTES: usize = 256 * 1024;
const CONSOLE_HISTORY_COMMANDS: usize = 100;
const CONSOLE_HISTORY_BYTES: usize = 64 * 1024;
/// How long a console command may run. Long enough for anything that
/// finishes, short enough that `watch` or `logs -f` — which never do —
/// give the worker thread back.
const CONSOLE_TIMEOUT: Duration = Duration::from_secs(20);

/// How long a Stop stays armed after the first click. Long enough to
/// read what the button now says, short enough that the intent behind it
/// has not gone stale.
const CONFIRM_WITHIN: Duration = Duration::from_secs(5);

/// How long the status line keeps saying the last thing that happened.
/// Nothing lives only there — a failed setup keeps its plan, a lost
/// socket has its own indicator — so letting it go is safe, and a line
/// that never expires is a line the eye stops reading.
const STATUS_FOR: Duration = Duration::from_secs(20);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    Agents,
    Board,
    Usage,
    Questions,
    Channels,
    Terminal,
    Console,
    Runtimes,
    Journal,
    Leases,
    Settings,
    Desktop,
}

/// What the worker is asked to do.
#[derive(Debug)]
enum Cmd {
    Agents,
    Leases,
    Runtimes,
    /// Whether the remote connector is serving here (its status file).
    Connector,
    Discovered,
    Journal(String, String),
    Channels(String, String),
    Inbox,
    Activity,
    /// The selected project's board.
    /// The selected project's board: a page from `offset`, `limit`
    /// cards at most (a refresh asks for as many as are on view), for
    /// the ask numbered `request`, which is what its reply answers to.
    Tasks {
        project: String,
        request: u64,
        offset: usize,
        limit: usize,
    },
    /// The selected project's usage report: what the providers said,
    /// one row per `by`, over the window `since`.
    Usage {
        project: String,
        request: u64,
        since: &'static str,
        by: agentdocker_core::usage::report::Group,
    },
    /// The person files a card.
    TaskCreate {
        project: String,
        /// Which filing this is, so a late reply cannot clear a newer
        /// draft or be taken for it.
        request: u64,
        title: String,
        acceptance: String,
        column: agentdocker_core::Column,
    },
    /// The person moves a card, or an agent it is handed to does.
    TaskMove {
        task: agentdocker_core::TaskId,
        column: agentdocker_core::Column,
    },
    TaskAssign {
        task: agentdocker_core::TaskId,
        assignee: Option<agentdocker_core::AgentId>,
    },
    TaskArchive(agentdocker_core::TaskId),
    /// The projects that are paused, and why.
    Pauses,
    /// The person tells a project's agents to hold, with the reason.
    Pause {
        request: PauseRequest,
        reason: String,
    },
    /// The person lifts a project's pause.
    ResumeProject(PauseRequest),
    SessionLog(String),
    /// Register the person at the keyboard, so agents can address them.
    Me,
    Questions,
    Answer(MessageId, String),
    DismissMessages(Vec<MessageId>),
    Adopt(u32),
    AdoptAll,
    Stop(String),
    ResumeProvider(String, chrono::DateTime<Utc>),
    /// Start a session's bound input receiver again after the daemon gave up.
    RetryController(String),
    Setup(Vec<String>),
    Desktop(Vec<String>),
    UpdateCheck,
    /// Any `agentdocker` command, so the window is not limited to the
    /// few actions that have buttons.
    Console(String, Option<std::path::PathBuf>),
    Launch(Box<agentdocker_core::AgentSpec>),
    ChannelSend(String, String),
    SessionSend(String, String),
    /// Text from the person to every agent in a project, receipted under
    /// the agent whose conversation it was typed in.
    ProjectSend(String, String, String),
    /// The person's conversations, in one project (a selector) or everywhere.
    Conversations(Option<String>),
    /// The newest page of one conversation's archive, in an archive epoch:
    /// a reply from before a prune is not applied after it.
    History(String, u64),
    /// The page of a conversation's archive before an archive seq.
    HistoryBefore(String, u64, u64),
    /// One root and every reply, paged through by the worker.
    Thread(MessageId, u64),
    /// The person read a conversation through an archive seq.
    MarkRead(String, u64),
    /// The person opens a room in the project they are looking at, with
    /// the members they picked (everyone else in it when none).
    ChannelOpen {
        request: MessageId,
        name: String,
        task: String,
        members: Vec<String>,
        project: Option<String>,
    },
    ChannelInvite {
        request: MessageId,
        channel: String,
        member: String,
    },
    /// Text from the person into a conversation: `draft` is the composer it
    /// came from (the conversation, or `<conversation>#<root>` in a thread,
    /// which is where the receipt goes), `to` the destination the
    /// conversation stands for, `reply_to` the thread root.
    ConversationSend {
        draft: String,
        to: String,
        text: String,
        reply_to: Option<MessageId>,
    },
}

/// A bounded, read-only log snapshot. No console command is constructed, and
/// a stalled stream cannot occupy the request worker indefinitely.
fn read_session_log(client: &crate::client::Client, agent: &str) -> anyhow::Result<String> {
    use anyhow::Context;
    use std::io::{BufRead, BufReader, Read};
    let stream = client
        .open_with_read_timeout(
            &Request::Logs {
                agent: agent.into(),
                follow: false,
                tail: 100,
            },
            Some(Duration::from_millis(100)),
        )
        .context("opening session log")?;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut reader = BufReader::new(stream);
    let mut result = String::new();
    let mut frame = Vec::new();
    const LIMIT: usize = 128 * 1024;
    loop {
        anyhow::ensure!(
            std::time::Instant::now() < deadline,
            "session log timed out"
        );
        // Preserve partial bytes (including incomplete UTF-8) across the fixed
        // read timeout. The total deadline never restarts, and no socket option
        // is changed after the request or after a fast peer closes.
        let received = reader
            .by_ref()
            .take((LIMIT + 1 - frame.len()) as u64)
            .read_until(b'\n', &mut frame);
        anyhow::ensure!(frame.len() <= LIMIT, "session log exceeded its size limit");
        match received {
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                continue;
            }
            other => anyhow::ensure!(
                other.context("reading session log")? > 0 && frame.ends_with(b"\n"),
                "session log ended early"
            ),
        }
        match serde_json::from_slice::<Response>(&frame)? {
            Response::Log { line } => {
                if result.len() + line.len() + 1 > LIMIT {
                    result.push_str("\n[Log snapshot truncated]");
                    return Ok(result);
                }
                result.push_str(&line);
                result.push('\n');
            }
            Response::End => return Ok(result),
            Response::Error { message, .. } => anyhow::bail!("{message}"),
            _ => anyhow::bail!("unexpected session log response"),
        }
        frame.clear();
    }
}

/// What comes back to the window.
enum Msg {
    Agents(Vec<AgentRecord>, BTreeMap<String, String>),
    Leases(Vec<Lease>),
    Runtimes(Vec<RuntimeInfo>),
    Connector(Option<agentdocker_host::connector::Serving>),
    Discovered(Vec<DiscoveredProcess>),
    Journal(String, Option<u64>, Vec<JournalEntry>),
    Channels(String, Vec<agentdocker_core::Channel>),
    Inbox(Vec<agentdocker_core::Envelope>),
    Activity(Vec<AgentActivity>),
    /// The board of the project asked for.
    /// The board read for a project: its cards and whether the page
    /// cut the board short, or why it could not be read.
    Tasks(
        String,
        u64,
        Result<(Vec<agentdocker_core::Task>, bool), String>,
    ),
    /// A filing's outcome, for the draft that made it.
    TaskCreated(String, u64, Result<(), String>),
    /// The usage report read for a project, or why it could not be.
    Usage(
        String,
        u64,
        Result<agentdocker_core::usage::report::Report, String>,
    ),
    /// A change to the board, done (the board is read again) or refused.
    TaskChanged(Result<(), String>),
    Pauses(Vec<agentdocker_core::Pause>),
    /// The pause or resume the person asked for a project, done or
    /// refused; the project says which form or control it answers.
    Paused(PauseRequest, Result<(), String>),
    SessionLog(String, Result<String, String>),
    Questions(Vec<Question>),
    /// An answer came back: `Ok` means it was delivered, `Err` carries
    /// why it was not, so what the person typed is not thrown away.
    Answered(MessageId, Result<(), String>),
    MessagesDismissed(Vec<MessageId>, Result<(), String>),
    Event(Box<Event>),
    Connected,
    Disconnected(String),
    Status(String),
    Setup(Result<serde_json::Value, String>),
    Desktop(Result<serde_json::Value, String>),
    UpdateChecked(Result<serde_json::Value, String>),
    Console(String),
    Launched(Result<String, String>),
    ChannelSent(String, Result<QueuedSend, String>),
    /// The room the person asked for, by id, or why not.
    ChannelOpened(MessageId, Result<agentdocker_core::ChannelId, String>),
    ChannelInvited(MessageId, String, Result<agentdocker_core::Channel, String>),
    SessionSent(String, Result<QueuedSend, String>),
    /// `Err` when the daemon does not know conversations at all.
    Conversations(Result<Vec<agentdocker_core::ConversationSummary>, String>),
    History(String, u64, Vec<agentdocker_core::ArchivedMessage>),
    /// An earlier page, complete when it is shorter than a page.
    HistoryEarlier(String, u64, Vec<agentdocker_core::ArchivedMessage>),
    Thread(
        u64,
        agentdocker_core::ArchivedMessage,
        Vec<agentdocker_core::ArchivedMessage>,
    ),
    /// The daemon no longer has this thread's root: it was pruned.
    ThreadGone(MessageId, u64),
    /// The draft the words came from, and the receipt or error.
    ConversationSent(String, Result<QueuedSend, String>),
}

#[derive(Debug)]
struct QueuedSend {
    message: MessageId,
    readiness: Option<agentdocker_core::SendReadiness>,
}

#[cfg(test)]
impl From<MessageId> for QueuedSend {
    fn from(message: MessageId) -> Self {
        Self {
            message,
            readiness: None,
        }
    }
}

pub struct App {
    shell: shell::State,
    /// The window's draggable columns.
    panes: panes::Panes,
    wake: Wake,
    worker_stop: Arc<std::sync::atomic::AtomicBool>,
    desktop: crate::desktop::Panel,
    smoke: Option<crate::smoke::Smoke>,
    setup_plan: Option<serde_json::Value>,
    setup_health: Option<serde_json::Value>,
    setup_history: Vec<serde_json::Value>,
    setup_busy: bool,
    /// When [`App::status`] was last set, so it can stop being news.
    status_at: Instant,
    /// Console commands sent and not yet answered.
    ///
    /// The console runs on a lane of its own now, so a command that
    /// takes its whole twenty seconds no longer freezes anything — which
    /// also means nothing on the screen moves while it runs. Counted, so
    /// the prompt can say so.
    console_running: usize,
    /// The agent whose Stop button is armed, and when it was armed.
    ///
    /// Stopping is the one thing this window does that a person cannot
    /// take back, and the button for it sits in a dense table row next
    /// to Attach. So it asks once. Arming rather than a modal, because a
    /// dialog over a live table is worse than a button that changes its
    /// mind, and it disarms itself so a click forgotten five minutes ago
    /// cannot be completed by accident.
    confirm_stop: Option<(String, Instant)>,
    tx: CommandSender,
    rx: Receiver<Msg>,
    screen: Screen,
    agents: Vec<AgentRecord>,
    aliases: BTreeMap<String, String>,
    leases: Vec<Lease>,
    runtimes: Vec<RuntimeInfo>,
    /// The remote connector serving on this machine, when one is.
    connector: Option<agentdocker_host::connector::Serving>,
    discovered: Vec<DiscoveredProcess>,
    journal: Vec<JournalEntry>,
    journal_project: Option<String>,
    /// Open channels across the projects of registered live agents.
    ///
    /// The person at the keyboard is an agent like any other, so channel
    /// messages arrive in their inbox — and until this screen existed
    /// the window dropped every one of them on the floor. Read without
    /// draining, because a window that polls must not consume what it
    /// shows.
    channels: Vec<agentdocker_core::Channel>,
    inbox: Vec<agentdocker_core::Envelope>,
    /// What the person can read, as the daemon lists it; `None` until the
    /// daemon has answered, `Some(false)` from a daemon without it.
    conversations: Vec<agentdocker_core::ConversationSummary>,
    conversations_supported: Option<bool>,
    /// The form for a conversation the person is starting — a direct
    /// message or a channel — while it is open.
    new_conversation: Option<NewConversation>,
    /// A notification's message to scroll to once its conversation's
    /// archive has it.
    reveal_archived: Option<Seek>,
    /// Archived history per conversation, oldest first, as last fetched.
    history: BTreeMap<String, Vec<agentdocker_core::ArchivedMessage>>,
    /// Conversations whose earliest archived message is on view.
    history_complete: BTreeSet<String>,
    /// Advanced when the daemon prunes; archive replies from an earlier
    /// epoch are dropped rather than bring pruned rows back.
    history_epoch: u64,
    /// The open thread: its root and replies.
    thread: Option<(
        agentdocker_core::ArchivedMessage,
        Vec<agentdocker_core::ArchivedMessage>,
    )>,
    /// Confirmed sends from this window. Inbox polling must not erase them.
    /// Receipt times are local; this bounded cache is not durable channel history.
    sent_channels: std::collections::VecDeque<agentdocker_core::Envelope>,
    connected: Result<(), String>,
    /// The highest event sequence taken, so a reconnect's replay is not
    /// shown or acted on twice. Live-only events carry `0` and always pass.
    last_seq: u64,
    status: String,
    last_refresh: Instant,
    last_runtimes: Instant,
    socket: String,
    client: Option<Arc<Client>>,
    terminal: Option<Terminal>,
    console_input: String,
    console_output: String,
    console_truncated: bool,
    /// What has been typed here before, oldest first, and where the up
    /// arrow currently is in it. `None` is the live line.
    console_history: Vec<String>,
    console_recall: Option<usize>,
    /// What the reader has chosen about how this looks, and where it is
    /// kept between runs.
    settings: crate::theme::Settings,
    home: std::path::PathBuf,
    /// Open questions put to the human; saved answer text lives in the shell.
    questions: Vec<Question>,
    /// What each agent is doing, keyed by id. Derived by the daemon, so
    /// it is read rather than computed here.
    activity: BTreeMap<String, Activity>,
    /// The board on view.
    tasks: Option<Board>,
    /// The usage report on view: which project's, and the report — kept
    /// as last read when a read fails, with the failure said beside it.
    usage: Option<(String, agentdocker_core::usage::report::Report)>,
    usage_error: Option<String>,
    usage_requests: u64,
    usage_pending: Option<(
        u64,
        String,
        &'static str,
        agentdocker_core::usage::report::Group,
    )>,
    /// Board asks on their way, by number: which project's, and from
    /// what offset. A reply answers one ask; a reply to none — an ask
    /// cancelled by a later refresh, or made for a project no longer on
    /// view — moves nothing.
    board_asks: BTreeMap<u64, (String, usize)>,
    /// Filings so far, numbering each so its reply is told apart.
    task_requests: u64,
    /// A card whose acceptance text is open.
    task_open: Option<agentdocker_core::TaskId>,
    /// The projects told to hold, and why.
    pauses: Vec<agentdocker_core::Pause>,
    /// The pause being written, while its form is open: bound to the
    /// project it was opened for, whichever is selected by the time it
    /// is sent.
    pause_states: BTreeMap<String, PauseControl>,
    queued_inputs: BTreeMap<String, usize>,
    /// Per agent, the queued inputs no current receipt covers.
    awaiting_receipt: BTreeMap<String, usize>,
    session_log: Option<(String, Result<String, String>)>,
    /// Answers on their way to the daemon, so the same one is not sent
    /// twice while it is in flight.
    sending: std::collections::BTreeSet<MessageId>,
    dismissing: std::collections::BTreeSet<MessageId>,
}

impl App {
    pub fn with_smoke(mut self, smoke: Option<crate::smoke::Smoke>) -> Self {
        self.smoke = smoke;
        self
    }

    pub fn new() -> Self {
        let wake = Wake::default();
        let client = Arc::new(Client::from_env());
        // The same directory the daemon uses, so a throwaway
        // AGENTDOCKER_HOME gets its own appearance too rather than
        // rewriting the one the real window uses.
        let home = agentdocker_host::dirs::home();
        let (cmd_tx, cmd_rx) = queue::channel();
        let (msg_tx, msg_rx) = sync_channel::<Msg>(MESSAGE_CAPACITY);
        let worker_stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        spawn_worker(
            client.clone(),
            cmd_rx,
            msg_tx.clone(),
            wake.clone(),
            worker_stop.clone(),
        );
        spawn_events(client.clone(), msg_tx, wake.clone());
        // The window is a person being present, so the person is an
        // agent for as long as it is open.
        for cmd in [
            Cmd::Me,
            Cmd::Agents,
            Cmd::Leases,
            Cmd::Discovered,
            Cmd::Runtimes,
            Cmd::Connector,
            Cmd::Questions,
            Cmd::Activity,
        ] {
            let _ = cmd_tx.send(cmd);
        }
        let shell = shell::State::load(&home);
        let settings = shell
            .catalog
            .appearance
            .clone()
            .unwrap_or_else(|| crate::theme::Settings::load(&home))
            .clamped();
        let panes = panes::Panes::new(
            shell.catalog.panes,
            shell.width.max(1180.0) / (settings.text_size / 14.0),
        );
        Self {
            shell,
            panes,
            wake,
            worker_stop,
            desktop: Default::default(),
            tx: cmd_tx,
            rx: msg_rx,
            screen: Screen::Agents,
            agents: Vec::new(),
            aliases: BTreeMap::new(),
            leases: Vec::new(),
            runtimes: Vec::new(),
            connector: None,
            discovered: Vec::new(),
            journal: Vec::new(),
            journal_project: None,
            channels: Vec::new(),
            inbox: Vec::new(),
            conversations: Vec::new(),
            conversations_supported: None,
            new_conversation: None,
            reveal_archived: None,
            history: BTreeMap::new(),
            history_complete: BTreeSet::new(),
            history_epoch: 0,
            thread: None,
            sent_channels: Default::default(),
            smoke: None,
            setup_plan: None,
            setup_health: None,
            setup_history: Vec::new(),
            setup_busy: false,
            status_at: Instant::now(),
            console_running: 0,
            confirm_stop: None,
            connected: Err("connecting…".to_owned()),
            last_seq: 0,
            status: String::new(),
            last_refresh: Instant::now(),
            last_runtimes: Instant::now(),
            socket: client.socket().display().to_string(),
            client: Some(client),
            terminal: None,
            console_input: String::new(),
            console_output: String::new(),
            console_truncated: false,
            console_history: Vec::new(),
            console_recall: None,
            settings,
            home,
            questions: Vec::new(),
            sending: std::collections::BTreeSet::new(),
            dismissing: std::collections::BTreeSet::new(),
            activity: BTreeMap::new(),
            tasks: None,
            usage: None,
            usage_error: None,
            usage_requests: 0,
            usage_pending: None,
            task_requests: 0,
            board_asks: BTreeMap::new(),
            task_open: None,
            pauses: Vec::new(),
            pause_states: BTreeMap::new(),
            queued_inputs: BTreeMap::new(),
            awaiting_receipt: BTreeMap::new(),
            session_log: None,
        }
    }

    /// The window's state without a window or a daemon, for tests.
    #[cfg(test)]
    fn bare(tx: CommandSender, rx: Receiver<Msg>) -> Self {
        Self {
            shell: Default::default(),
            panes: panes::Panes::new(panes::Widths::default(), 1180.0),
            wake: Wake::default(),
            worker_stop: Default::default(),
            desktop: Default::default(),
            tx,
            rx,
            screen: Screen::Agents,
            agents: Vec::new(),
            aliases: BTreeMap::new(),
            leases: Vec::new(),
            runtimes: Vec::new(),
            connector: None,
            discovered: Vec::new(),
            journal: Vec::new(),
            journal_project: None,
            channels: Vec::new(),
            inbox: Vec::new(),
            conversations: Vec::new(),
            conversations_supported: None,
            new_conversation: None,
            reveal_archived: None,
            history: BTreeMap::new(),
            history_complete: BTreeSet::new(),
            history_epoch: 0,
            thread: None,
            sent_channels: Default::default(),
            smoke: None,
            setup_plan: None,
            setup_health: None,
            setup_history: Vec::new(),
            setup_busy: false,
            status_at: Instant::now(),
            console_running: 0,
            confirm_stop: None,
            connected: Ok(()),
            last_seq: 0,
            status: String::new(),
            last_refresh: Instant::now(),
            last_runtimes: Instant::now(),
            socket: String::new(),
            client: None,
            terminal: None,
            console_input: String::new(),
            console_output: String::new(),
            console_truncated: false,
            console_history: Vec::new(),
            console_recall: None,
            settings: crate::theme::Settings::default(),
            home: std::path::PathBuf::new(),
            questions: Vec::new(),
            sending: std::collections::BTreeSet::new(),
            activity: BTreeMap::new(),
            tasks: None,
            usage: None,
            usage_error: None,
            usage_requests: 0,
            usage_pending: None,
            task_requests: 0,
            board_asks: BTreeMap::new(),
            task_open: None,
            pauses: Vec::new(),
            pause_states: BTreeMap::new(),
            queued_inputs: BTreeMap::new(),
            awaiting_receipt: BTreeMap::new(),
            session_log: None,
            dismissing: std::collections::BTreeSet::new(),
        }
    }

    fn session_draft_key(&self, agent: &str) -> String {
        if self.shell.session_drafts.contains_key(agent) {
            return agent.to_owned();
        }
        self.shell
            .session_drafts
            .keys()
            .find(|key| self.canonical_agent(key) == agent)
            .cloned()
            .unwrap_or_else(|| agent.to_owned())
    }

    fn send(&mut self, cmd: Cmd) {
        if let Err(queue::Rejected { command, reason }) = self.tx.send(cmd) {
            self.rejected(*command, reason);
        }
    }

    /// A command the worker queue would not take: whatever the person
    /// started with it is told so, or it would wait for a reply that
    /// never comes.
    fn rejected(&mut self, command: Cmd, reason: &'static str) {
        {
            match command {
                Cmd::Answer(id, _) => {
                    self.sending.remove(&id);
                    if self.shell.pending_answer_reveal.as_ref() == Some(&id) {
                        self.shell.pending_answer_reveal = None;
                    }
                }
                Cmd::DismissMessages(ids) => {
                    self.dismissing.retain(|id| !ids.contains(id));
                }
                Cmd::ChannelOpen { request, .. } | Cmd::ChannelInvite { request, .. } => {
                    if let Some(form) = &mut self.new_conversation
                        && form.request == request
                    {
                        form.creating = false;
                        form.error = Some(reason.into());
                    }
                }
                Cmd::Pause { request, .. } | Cmd::ResumeProject(request) => {
                    self.complete_pause(request, Err(reason.into()));
                }
                Cmd::Setup(_) => self.setup_busy = false,
                Cmd::Desktop(_) => self.desktop.receive(Err(reason.into())),
                Cmd::Console(_, _) => {
                    self.console_running = self.console_running.saturating_sub(1);
                    self.append_console(&format!("Command not queued: {reason}\n"));
                }
                Cmd::Launch(_) => self.shell.launching = false,
                Cmd::ChannelSend(id, _) => {
                    self.shell
                        .channel_drafts
                        .entry(id)
                        .or_default()
                        .complete(Err(reason.into()));
                }
                Cmd::SessionSend(id, _) | Cmd::ProjectSend(id, _, _) => {
                    if let Some(entry) = self.shell.session_drafts.get_mut(&id) {
                        entry.draft.complete(Err(reason.into()));
                    }
                }
                // A conversation draft that could not be queued is told so,
                // or it would stay "sending" and never be retried.
                Cmd::ConversationSend { draft, .. } => {
                    self.shell
                        .conversation_drafts
                        .entry(draft)
                        .or_default()
                        .complete(Err(reason.into()));
                }
                // A filing that could not be queued is told so, or it
                // would stay "Filing…" for good; a move, a hand or an
                // archive says so in the status.
                Cmd::TaskCreate {
                    project, request, ..
                } => {
                    if let Some(draft) = self.shell.task_drafts.get_mut(&project)
                        && draft.sending == Some(request)
                    {
                        draft.sending = None;
                        draft.error = Some(reason.into());
                    }
                }
                // A page asked for by hand that could not be queued is
                // told so; a refresh is not.
                Cmd::Usage { request, .. } => {
                    if self.usage_pending.as_ref().is_some_and(|p| p.0 == request) {
                        self.usage_pending = None;
                        self.usage_error = Some(reason.into());
                    }
                    return;
                }
                Cmd::Tasks {
                    project,
                    request,
                    offset,
                    ..
                } => {
                    self.board_asks.remove(&request);
                    if offset == 0 {
                        return;
                    }
                    if let Some(board) = self.tasks.as_mut().filter(|b| b.project == project)
                        && board.pending_more == Some(request)
                    {
                        board.pending_more = None;
                    }
                }
                Cmd::TaskMove { .. }
                | Cmd::TaskAssign { .. }
                | Cmd::TaskArchive(_)
                | Cmd::Adopt(_)
                | Cmd::AdoptAll
                | Cmd::Stop(_)
                | Cmd::ResumeProvider(..)
                | Cmd::RetryController(_) => {}
                // Full queues may omit refreshes: events and periodic refresh
                // request another snapshot. User actions get an explicit error.
                _ => return,
            }
            self.say(reason);
        }
    }

    /// Responses identify the exact operation, not just the selected project.
    /// A delayed pause/resume completion must not consume a later draft or request.
    fn complete_pause(&mut self, request: PauseRequest, result: Result<(), String>) {
        let Some(control) = self.pause_states.get_mut(&request.project) else {
            return;
        };
        if control.pending.as_ref() != Some(&request) {
            return;
        }
        control.pending = None;
        match result {
            Ok(()) => {
                control.error = None;
                if request.action == PauseAction::Pause {
                    control.draft = None;
                }
                if control.draft.is_none() {
                    self.pause_states.remove(&request.project);
                }
                self.send(Cmd::Pauses);
            }
            Err(error) => control.error = Some(error),
        }
    }

    fn submit_pause(&mut self, project: String, action: PauseAction) {
        if self.connected.is_err() {
            return;
        }
        if !self.pause_states.contains_key(&project) && self.pause_states.len() >= PAUSE_CONTROLS {
            self.say("Finish or cancel an existing pause draft first.");
            return;
        }
        let control = self.pause_states.entry(project.clone()).or_default();
        if control.pending.is_some() {
            return;
        }
        let reason = if action == PauseAction::Pause {
            let Some(reason) = &control.draft else { return };
            if reason.trim().is_empty() || reason.chars().count() > 400 {
                control.error = Some("Enter a reason of 1–400 characters.".into());
                return;
            }
            Some(reason.trim().to_owned())
        } else {
            None
        };
        let request = PauseRequest {
            id: MessageId::generate(),
            project,
            action,
        };
        control.pending = Some(request.clone());
        control.error = None;
        self.send(match reason {
            Some(reason) => Cmd::Pause { request, reason },
            None => Cmd::ResumeProject(request),
        });
    }

    /// Say the last thing that happened, and remember when.
    fn say(&mut self, text: impl Into<String>) {
        self.status = text.into();
        self.status_at = Instant::now();
    }

    /// Take everything the threads sent since the last frame.
    fn drain(&mut self) {
        // Long enough ago that it is no longer what just happened.
        if !self.status.is_empty() && self.status_at.elapsed() >= STATUS_FOR {
            self.status.clear();
        }
        for _ in 0..MESSAGE_CAPACITY {
            let Ok(msg) = self.rx.try_recv() else { break };
            match msg {
                Msg::Agents(agents, aliases) => {
                    self.aliases = aliases;
                    if let Some(selected) = &self.shell.selected {
                        self.shell.selected = Some(self.canonical_agent(selected).to_owned());
                    }
                    self.agents = agents;
                    let projects = self.channel_projects();
                    self.channels
                        .retain(|channel| projects.contains(&channel.project.to_string()));
                    for project in projects {
                        self.request_channels(project);
                    }
                }
                Msg::Leases(leases) => self.leases = leases,
                Msg::Runtimes(runtimes) => self.runtimes = runtimes,
                Msg::Connector(serving) => self.connector = serving,
                Msg::Channels(project, channels) => {
                    // A late reply for a project no longer on screen must not
                    // restore it. Other projects keep their current snapshots.
                    if self.channel_projects().contains(&project) {
                        self.channels
                            .retain(|channel| channel.project.as_str() != project);
                        self.channels.extend(channels);
                        self.channels.sort_by(|a, b| {
                            a.opened_at.cmp(&b.opened_at).then_with(|| a.id.cmp(&b.id))
                        });
                    }
                }
                Msg::Inbox(inbox) => {
                    // An unfolded message folds when it is gone from
                    // wherever it was shown: the inbox, an archive, a thread.
                    if self.shell.message_detail.as_ref().is_some_and(|id| {
                        !inbox.iter().any(|message| &message.id == id) && !self.archived_on_view(id)
                    }) {
                        self.shell.message_detail = None;
                    }
                    self.inbox = inbox;
                }
                Msg::MessagesDismissed(ids, result) => {
                    self.dismissing.retain(|id| !ids.contains(id));
                    match result {
                        Ok(()) => {
                            self.inbox.retain(|message| !ids.contains(&message.id));
                            if self
                                .shell
                                .message_detail
                                .as_ref()
                                .is_some_and(|id| ids.contains(id))
                            {
                                self.shell.message_detail = None;
                            }
                            if self
                                .shell
                                .notification_message
                                .as_ref()
                                .is_some_and(|id| ids.contains(id))
                            {
                                self.shell.notification_message = None;
                            }
                            self.say("Messages dismissed");
                        }
                        Err(reason) => self.say(reason),
                    }
                }
                Msg::Discovered(found) => self.discovered = found,
                Msg::Journal(project, head_seq, entries) => {
                    if self.journal_project.as_deref() == Some(project.as_str()) {
                        // An in-flight snapshot may predate live events. Merge
                        // by sequence so a late reply cannot roll the view back.
                        let mut merged: BTreeMap<_, _> = entries
                            .into_iter()
                            .map(|entry| (entry.seq, entry))
                            .collect();
                        // The durable head also makes an empty post-prune
                        // snapshot authoritative. An older daemon has no such
                        // boundary, so use its snapshot without guessing.
                        merged.extend(
                            self.journal
                                .drain(..)
                                .filter(|entry| head_seq.is_some_and(|head| entry.seq > head))
                                .map(|entry| (entry.seq, entry)),
                        );
                        self.journal = merged.into_values().rev().take(JOURNAL_WINDOW).collect();
                        self.journal.reverse();
                    }
                }
                Msg::SessionLog(agent, result) => {
                    if self.shell.selected.as_deref() == Some(agent.as_str()) {
                        self.session_log = Some((agent, result));
                    }
                }
                Msg::Tasks(project, request, result) => {
                    // Only a reply to an ask still standing, for the
                    // project on view, moves the board.
                    let Some((asked_for, offset)) = self.board_asks.remove(&request) else {
                        continue;
                    };
                    if let Some(board) = self.tasks.as_mut().filter(|b| b.project == project)
                        && board.pending_more == Some(request)
                    {
                        board.pending_more = None;
                    }
                    if asked_for != project
                        || self.selected_project_root().as_deref() != Some(project.as_str())
                    {
                        continue;
                    }
                    match result {
                        // A first page is the board anew, keeping a later
                        // page still on its way; a later page is appended
                        // only where it was asked for.
                        Ok((tasks, more)) if offset == 0 => {
                            let pending_more = self
                                .tasks
                                .as_ref()
                                .filter(|b| b.project == project)
                                .and_then(|b| b.pending_more);
                            self.tasks = Some(Board {
                                project,
                                cards: tasks,
                                more,
                                pending_more,
                            });
                        }
                        Ok((tasks, more)) => {
                            if let Some(board) =
                                self.tasks.as_mut().filter(|b| b.project == project)
                                && board.cards.len() == offset
                            {
                                board.cards.extend(tasks);
                                board.more = more;
                            }
                        }
                        // The board as last read stays on view; the
                        // person is told why it is not newer.
                        Err(error) => {
                            self.say(format!("The board could not be read: {error}"));
                        }
                    }
                }
                Msg::Usage(project, request, result) => {
                    // A later range/group selection supersedes the old read,
                    // even when both asks concern the same project.
                    if self
                        .usage_pending
                        .as_ref()
                        .is_some_and(|p| p.0 == request && p.1 == project)
                    {
                        self.usage_pending = None;
                    } else {
                        continue;
                    }
                    if self.selected_project_root().as_deref() == Some(project.as_str()) {
                        match result {
                            Ok(report) => {
                                self.usage = Some((project, report));
                                self.usage_error = None;
                            }
                            Err(error) => self.usage_error = Some(error),
                        }
                    }
                }
                Msg::TaskCreated(project, request, result) => {
                    // Only the filing this reply answers: a draft typed
                    // since, after a refusal, keeps its text.
                    if let Some(draft) = self.shell.task_drafts.get_mut(&project)
                        && draft.sending == Some(request)
                    {
                        draft.sending = None;
                        match result {
                            Ok(()) => {
                                draft.title.clear();
                                draft.acceptance.clear();
                                draft.error = None;
                                self.shell.drafts.changed();
                            }
                            Err(error) => draft.error = Some(error),
                        }
                    }
                    self.request_tasks();
                }
                Msg::TaskChanged(result) => match result {
                    Ok(()) => self.request_tasks(),
                    Err(error) => self.say(error),
                },
                Msg::Pauses(pauses) => self.pauses = pauses,
                Msg::Paused(request, result) => self.complete_pause(request, result),
                Msg::Activity(activity) => {
                    self.queued_inputs = activity
                        .iter()
                        .filter_map(|a| a.queued_inputs.map(|count| (a.agent.to_string(), count)))
                        .collect();
                    self.awaiting_receipt = activity
                        .iter()
                        .filter_map(|a| {
                            a.awaiting_receipt.map(|count| (a.agent.to_string(), count))
                        })
                        .collect();
                    let fresh: BTreeMap<String, Activity> = activity
                        .into_iter()
                        .map(|a| (a.agent.to_string(), a.activity))
                        .collect();
                    self.note_completions(&fresh);
                    self.activity = fresh;
                }
                Msg::Questions(questions) => {
                    if self
                        .shell
                        .file_review
                        .as_ref()
                        .is_some_and(|id| !questions.iter().any(|q| &q.id == id))
                    {
                        self.shell.file_review = None;
                    }
                    // Forget drafts for questions nobody is waiting on any
                    // more, so the map does not grow with the session — but
                    // not one still in flight, whose question the daemon
                    // has already forgotten.
                    let before = self.shell.answers.len();
                    self.shell.answers.retain(|id, _| {
                        self.sending.contains(id) || questions.iter().any(|q| q.id == *id)
                    });
                    if self.shell.answers.len() != before {
                        self.shell.drafts.changed();
                    }
                    self.questions = questions;
                }
                Msg::Answered(id, result) => {
                    self.sending.remove(&id);
                    match result {
                        Ok(()) => {
                            if self.shell.answers.remove(&id).is_some() {
                                self.shell.drafts.changed();
                            }
                            if self.shell.file_review.as_ref() == Some(&id) {
                                self.shell.file_review = None;
                            }
                            self.questions.retain(|q| q.id != id);
                            if self.shell.pending_answer_reveal.as_ref() == Some(&id) {
                                self.shell.pending_answer_reveal = None;
                                self.shell.reveal_next_question = true;
                            }
                            self.say("answered");
                        }
                        // The draft stays exactly where it was, so nothing
                        // typed is lost to a daemon that was not listening.
                        Err(reason) => {
                            if self.shell.pending_answer_reveal.as_ref() == Some(&id) {
                                self.shell.pending_answer_reveal = None;
                            }
                            self.shell.answer_errors.insert(id, reason.clone());
                            self.say(reason);
                        }
                    }
                }
                Msg::Event(event) => self.on_event(*event),
                Msg::Connected => {
                    if self.connected.is_err() {
                        self.connected = Ok(());
                        for cmd in [
                            Cmd::Me,
                            Cmd::Agents,
                            Cmd::Leases,
                            Cmd::Discovered,
                            Cmd::Runtimes,
                            Cmd::Connector,
                            Cmd::Questions,
                            Cmd::Activity,
                            Cmd::Inbox,
                            Cmd::Pauses,
                        ] {
                            self.send(cmd);
                        }
                        if let Some(project) = &self.journal_project {
                            self.request_journal(project.clone());
                        }
                    }
                }
                Msg::Disconnected(reason) => {
                    self.connected = Err(reason);
                    // A page asked for will not come: a notification's
                    // search ends rather than wait on it.
                    self.cancel_reveal();
                }
                Msg::Status(text) => self.say(text),
                Msg::Desktop(result) => self.desktop.receive(result),
                Msg::UpdateChecked(result) => {
                    if self.desktop.prefix.trim().is_empty() {
                        self.desktop.receive_update(result);
                    } else {
                        self.desktop.checking_updates = false;
                    }
                }
                Msg::Setup(result) => {
                    self.setup_busy = false;
                    match result {
                        Ok(value) if value.get("daemon_reachable").is_some() => {
                            self.setup_health = Some(value)
                        }
                        Ok(value) if value.get("plans").is_some() => {
                            if let Some(count) = value["skipped_receipts"]
                                .as_u64()
                                .filter(|count| *count > 0)
                            {
                                self.say(format!(
                                    "{count} saved setup receipt(s) could not be read; the files were preserved"
                                ));
                            }
                            self.setup_history =
                                value["plans"].as_array().cloned().unwrap_or_default();
                        }
                        Ok(value) => {
                            self.say(format!(
                                "Setup {}",
                                value["phase"].as_str().unwrap_or("updated")
                            ));
                            self.setup_plan = Some(value);
                            self.send(Cmd::Runtimes);
                        }
                        Err(error) => {
                            self.shell.setup_error = Some(error.clone());
                            self.say(error);
                        }
                    }
                }
                Msg::Console(text) => {
                    self.console_running = self.console_running.saturating_sub(1);
                    // Appended, not replaced: a terminal keeps what it
                    // said, and the command that produced this is
                    // already above it.
                    self.append_console(text.trim_end());
                    self.append_console("\n");
                }
                Msg::Launched(result) => {
                    self.shell.launching = false;
                    match result {
                        Ok(id) => {
                            self.shell.launch = false;
                            self.shell.selected = Some(id);
                            self.send(Cmd::Agents);
                            self.say("Agent launched");
                        }
                        Err(error) => self.shell.error = Some(error),
                    }
                }
                Msg::Conversations(result) => match result {
                    Ok(conversations) => {
                        self.conversations_supported = Some(true);
                        self.conversations = conversations;
                        // A thread opened before the daemon answered (a
                        // notification at launch) names its conversation now.
                        if self.shell.inbox_open && self.shell.conversation.is_none() {
                            self.adopt_inbox_thread();
                        } else if let Some(open) = self.shell.conversation.clone()
                            && self.screen == Screen::Questions
                        {
                            // Reading the open conversation as its history
                            // arrives marks it read; a conversation that
                            // gained unread words while open is fetched again.
                            self.send(Cmd::History(open, self.history_epoch));
                        }
                    }
                    Err(error) => {
                        if self.conversations_supported.is_none() {
                            eprintln!("conversations unavailable, the inbox stays: {error}");
                        }
                        self.conversations_supported = Some(false);
                    }
                },
                Msg::History(conversation, epoch, messages) => {
                    if epoch != self.history_epoch {
                        continue;
                    }
                    let open = self.shell.conversation.as_deref() == Some(conversation.as_str());
                    // Reading is seeing: only the pane on view marks read, never
                    // the list a narrow window shows instead of it.
                    if open
                        && self.conversation_pane_visible()
                        && let Some(last) = messages.last()
                        && self
                            .conversations
                            .iter()
                            .any(|c| c.conversation.as_str() == conversation && c.unread > 0)
                    {
                        self.send(Cmd::MarkRead(conversation.clone(), last.seq));
                    }
                    // A short newest page is the whole archive; a full one
                    // says nothing about what came before it.
                    if messages.len() < HISTORY_PAGE {
                        self.history_complete.insert(conversation.clone());
                    } else {
                        self.history_complete.remove(&conversation);
                    }
                    // Earlier pages already shown stay in front of the newest.
                    let mut merged: Vec<_> = self
                        .history
                        .remove(&conversation)
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|m| messages.first().is_none_or(|first| m.seq < first.seq))
                        .collect();
                    merged.extend(messages);
                    self.keep_history(conversation.clone(), merged);
                    if let Some(seek) = &mut self.reveal_archived
                        && seek.conversation == conversation
                    {
                        seek.refresh_pending = false;
                    }
                    self.seek_archived(&conversation);
                }
                Msg::HistoryEarlier(conversation, epoch, earlier) => {
                    if epoch != self.history_epoch {
                        continue;
                    }
                    if earlier.len() < HISTORY_PAGE {
                        self.history_complete.insert(conversation.clone());
                    }
                    let mut shown = self.history.remove(&conversation).unwrap_or_default();
                    let first = shown.first().map(|m| m.seq);
                    let mut merged: Vec<_> = earlier
                        .into_iter()
                        .filter(|m| first.is_none_or(|f| m.seq < f))
                        .collect();
                    merged.append(&mut shown);
                    self.keep_history(conversation.clone(), merged);
                    self.seek_archived(&conversation);
                }
                Msg::Thread(epoch, root, replies) => {
                    if epoch == self.history_epoch
                        && self.shell.thread.as_ref() == Some(&root.envelope.id)
                    {
                        self.thread = Some((root, replies));
                    }
                }
                Msg::ThreadGone(root, epoch) => {
                    // Pruned under the person: the thread closes rather than
                    // stay on view as if it were still there.
                    if epoch == self.history_epoch && self.shell.thread.as_ref() == Some(&root) {
                        self.shell.thread = None;
                        self.thread = None;
                    }
                }
                Msg::ConversationSent(key, result) => {
                    let (conversation, _) = split_draft_key(&key);
                    let conversation = conversation.to_owned();
                    let draft = self.shell.conversation_drafts.entry(key).or_default();
                    match result {
                        Ok(receipt) => {
                            draft.complete(Ok(()));
                            draft.readiness = receipt.readiness;
                            self.send(Cmd::History(conversation, self.history_epoch));
                            if let Some(root) = self.shell.thread.clone() {
                                self.send(Cmd::Thread(root, self.history_epoch));
                            }
                            self.send(Cmd::Conversations(self.conversation_scope()));
                        }
                        Err(error) => draft.complete(Err(error)),
                    }
                    self.shell.drafts.changed();
                }
                Msg::SessionSent(id, result) => {
                    if let Some(entry) = self.shell.session_drafts.get_mut(&id) {
                        match result {
                            Ok(receipt) => {
                                entry.queued = Some(receipt.message);
                                entry.draft.readiness = receipt.readiness;
                                entry.draft.complete(Ok(()));
                            }
                            Err(error) => entry.draft.complete(Err(error)),
                        }
                    }
                    self.shell.drafts.changed();
                }
                Msg::ChannelInvited(request, member, result) => {
                    if let Some(form) = &mut self.new_conversation
                        && form.request == request
                    {
                        form.creating = false;
                        match result {
                            Ok(channel) => {
                                form.members.insert(member.into());
                                form.error = None;
                                self.channels.retain(|c| c.id != channel.id);
                                self.channels.push(channel);
                                self.send(Cmd::Conversations(self.conversation_scope()));
                            }
                            Err(error) => form.error = Some(error),
                        }
                    }
                }
                Msg::ChannelOpened(request, result) => {
                    if self
                        .new_conversation
                        .as_ref()
                        .is_none_or(|form| form.request != request)
                    {
                        continue;
                    }
                    match result {
                        Ok(id) => {
                            self.new_conversation = None;
                            let conversation = agentdocker_core::ConversationId::channel(&id)
                                .as_str()
                                .to_owned();
                            if let Some(project) = self.selected_project_id() {
                                self.request_channels(project);
                            }
                            self.send(Cmd::Conversations(self.conversation_scope()));
                            // The form's message, not a Task: drain has none
                            // to return, and selecting needs no effect of
                            // its own beyond the history request it sends.
                            let _ = self.update(Message::SelectConversation(conversation));
                        }
                        Err(error) => {
                            if let Some(form) = &mut self.new_conversation {
                                form.creating = false;
                                form.error = Some(error);
                            }
                        }
                    }
                }
                Msg::ChannelSent(id, result) => {
                    let draft = self.shell.channel_drafts.entry(id.clone()).or_default();
                    match result {
                        Ok(message) => {
                            let sent = draft.sending.clone();
                            draft.complete(Ok(()));
                            draft.readiness = message.readiness;
                            if let Some(sent) = sent {
                                let mut receipt = agentdocker_core::Envelope::new(
                                    agentdocker_core::HUMAN,
                                    agentdocker_core::Destination::Channel(id.into()),
                                    "message",
                                    serde_json::Value::String(sent),
                                    None,
                                    Utc::now(),
                                );
                                receipt.id = message.message;
                                self.sent_channels.push_back(receipt);
                                while self.sent_channels.len() > SENT_CHANNEL_LIMIT
                                    || self
                                        .sent_channels
                                        .iter()
                                        .map(|item| item.payload.as_str().map_or(0, str::len))
                                        .sum::<usize>()
                                        > SENT_CHANNEL_BYTES
                                {
                                    self.sent_channels.pop_front();
                                }
                            }
                            self.send(Cmd::Inbox);
                            self.say("Message sent");
                        }
                        Err(error) => draft.complete(Err(error)),
                    }
                    self.shell.drafts.changed();
                }
            }
        }
        // Nothing is asked of a daemon that is not there: each request
        // would wait out its start timeout, and the queue would outrun the
        // worker. Coming back re-reads everything anyway.
        if self.connected.is_ok() && self.last_refresh.elapsed() >= REFRESH {
            self.last_refresh = Instant::now();
            if self.conversations_supported != Some(false) {
                self.send(Cmd::Conversations(self.conversation_scope()));
            }
            for cmd in [
                Cmd::Agents,
                Cmd::Leases,
                Cmd::Discovered,
                Cmd::Questions,
                Cmd::Activity,
                Cmd::Inbox,
            ] {
                self.send(cmd);
            }
        }
        if self.connected.is_ok() && self.last_runtimes.elapsed() >= RUNTIMES_REFRESH {
            self.last_runtimes = Instant::now();
            self.send(Cmd::Runtimes);
        }
    }

    /// Keep the screens current from the stream instead of polling. A
    /// reconnect replays recent events so nothing produced while the
    /// window was disconnected is missed; anything already taken is not
    /// taken again.
    fn on_event(&mut self, event: Event) {
        if event.seq != 0 {
            if event.seq <= self.last_seq {
                return;
            }
            self.last_seq = event.seq;
        }
        match &event.kind {
            EventKind::AgentCreated { .. }
            | EventKind::AgentStarted { .. }
            | EventKind::AgentStopping { .. }
            | EventKind::AgentExited { .. }
            | EventKind::AgentRemoved { .. }
            | EventKind::AgentReconciled { .. }
            | EventKind::HumanLocationChanged { .. }
            | EventKind::AgentVcsChanged { .. }
            | EventKind::AdapterContactReported { .. } => self.send(Cmd::Agents),
            EventKind::InputDeliveryReported { .. }
            | EventKind::InputBound { .. }
            | EventKind::InputUnbound { .. }
            | EventKind::InputControllerEnded { .. }
            | EventKind::InputControllerLaunched { .. }
            | EventKind::InputControllerLaunchFailed { .. }
            | EventKind::InputRestartsExhausted { .. }
            | EventKind::InputRestartsReset { .. }
            | EventKind::InputControllerUpgraded { .. }
            | EventKind::InputResumed { .. }
            | EventKind::SessionResumed { .. } => {
                self.send(Cmd::Agents);
                self.send(Cmd::Activity);
            }
            EventKind::TaskCreated { .. }
            | EventKind::TaskPulled { .. }
            | EventKind::TaskMoved { .. }
            | EventKind::TaskUpdated { .. }
            | EventKind::TaskArchived { .. } => self.request_tasks(),
            // Collection moved: the report on view is read again, only
            // while it is on view.
            EventKind::UsageRecorded { .. } | EventKind::UsageReconciled { .. } => {
                if self.screen == Screen::Usage {
                    self.request_usage();
                }
            }
            EventKind::ProjectPaused { .. } | EventKind::ProjectResumed { .. } => {
                self.send(Cmd::Pauses);
            }
            EventKind::ProviderAvailabilityReported {
                agent,
                availability,
                ..
            } => {
                if let Some(record) = self.agents.iter_mut().find(|a| a.id == *agent) {
                    record.provider_availability = Some(availability.clone());
                }
                let blocked: Vec<_> = self
                    .agents
                    .iter()
                    .filter(|a| agentdocker_core::provider_block(a, &self.agents).is_some())
                    .map(|a| a.id.to_string())
                    .collect();
                self.shell.unviewed_done.retain(|id| !blocked.contains(id));
                self.send(Cmd::Agents);
                self.send(Cmd::Activity);
            }
            EventKind::InboxAcknowledged { .. } => self.send(Cmd::Activity),
            EventKind::AgentActivityReported { .. } => {
                self.send(Cmd::Agents);
                self.send(Cmd::Activity);
            }
            EventKind::RoleSet { .. } => self.send(Cmd::Agents),
            EventKind::LeaseClaimed { .. }
            | EventKind::LeaseRenewed { .. }
            | EventKind::LeaseReleased { .. }
            | EventKind::LeaseExpired { .. }
            | EventKind::LeaseTransferred { .. } => self.send(Cmd::Leases),
            EventKind::AgentDiscovered { .. } | EventKind::AgentVanished { .. } => {
                self.send(Cmd::Discovered);
            }
            // A question is worth showing the moment it is asked, not on
            // the next two-second sweep: somebody is blocked on it.
            EventKind::MessageSent { kind, .. } if kind == "question" || kind == "answer" => {
                self.send(Cmd::Questions);
                self.on_conversation_activity();
            }
            EventKind::MessageSent { .. }
            | EventKind::ConversationRead { .. }
            | EventKind::ChannelInvited { .. }
            | EventKind::ChannelOpened { .. }
            | EventKind::ChannelJoined { .. }
            | EventKind::ChannelClosed { .. } => self.on_conversation_activity(),
            // What the daemon pruned must not live on here: the archives
            // are dropped and the open one read again.
            EventKind::MessagesPruned { .. } => {
                self.history_epoch += 1;
                self.history.clear();
                self.history_complete.clear();
                self.thread = None;
                // A notification's search starts over from the newest
                // page: the page it was waiting for is of the old archive.
                if let Some(seek) = self.reveal_archived.clone() {
                    self.start_archive_reveal(seek.conversation, seek.message);
                }
                self.on_conversation_activity();
            }
            EventKind::QuestionClosed { .. } | EventKind::QuestionCancelled { .. } => {
                self.send(Cmd::Questions)
            }
            EventKind::JournalAppended { entry }
                if self.journal_project.as_deref() == Some(entry.project.as_str())
                    && self.journal.last().is_none_or(|last| last.seq < entry.seq) =>
            {
                self.journal.push(entry.clone());
                let excess = self.journal.len().saturating_sub(JOURNAL_WINDOW);
                self.journal.drain(..excess);
            }
            _ => {}
        }
    }

    /// A turn just finished: an agent that was working or blocked is now
    /// idle or gone. That is the observed state; whether anyone saw it is
    /// a separate question, answered here once and then only by the user.
    /// A completion on the screen the user is looking at, in a focused
    /// window, is viewed as it happens; every other one waits for them.
    fn note_completions(&mut self, fresh: &BTreeMap<String, Activity>) {
        for (id, now) in fresh {
            let was_busy = matches!(
                self.activity.get(id),
                Some(Activity::Working { .. } | Activity::Blocked { .. })
            );
            if !was_busy || !matches!(now, Activity::Idle { .. } | Activity::Finished) {
                continue;
            }
            if self
                .agents
                .iter()
                .any(|a| a.id.as_str() == id && self.delivery_paused(a))
            {
                continue;
            }
            let on_screen = self.screen == Screen::Agents
                && !self.shell.unfocused
                && self.agents.iter().any(|a| {
                    a.id.as_str() == id
                        && a.project.as_ref().map(|p| p.root.as_path()) == self.selected_root()
                });
            if !on_screen {
                self.shell.unviewed_done.insert(id.clone());
            }
        }
        // A session that started working again, or left, is no longer a
        // finished-and-unviewed one.
        self.shell.unviewed_done.retain(|id| {
            matches!(
                fresh.get(id),
                Some(Activity::Idle { .. } | Activity::Finished)
            )
        });
    }

    fn canonical_agent<'a>(&'a self, id: &'a str) -> &'a str {
        self.aliases.get(id).map(String::as_str).unwrap_or(id)
    }

    fn name_of(&self, id: &str) -> String {
        let id = self.canonical_agent(id);
        self.agents
            .iter()
            .find(|a| a.id.as_str() == id)
            .map(|a| self.display_name(a))
            // Not knowing the record does not prove the session ended.
            .unwrap_or_else(|| "an unknown session".to_owned())
    }

    /// The name a person reads for an agent. Adapters register sessions as
    /// `<runtime>-<pid or session id>`; the record says when its name was
    /// generated like that, and then the tool's label is shown instead,
    /// with the branch it works on (*Claude Code · main*), because that is
    /// what tells two sessions of one tool apart. Only when two live
    /// sessions of one tool share a branch, or neither has one, does an
    /// ordinal by first appearance follow (*Codex · main (2)*); an ended
    /// session keeps no number, its branch is enough in the Earlier
    /// group. A name somebody chose is shown as chosen.
    fn display_name(&self, agent: &AgentRecord) -> String {
        if !agent.name_is_generated() {
            return agent.spec.name.clone();
        }
        let tool = runtime_label(&agent.spec.runtime);
        let branch = agent.vcs.as_ref().and_then(|v| v.branch.clone());
        let base = match &branch {
            Some(branch) => format!("{tool} · {branch}"),
            None => tool,
        };
        if !agent.status.is_live() {
            return base;
        }
        let mut peers: Vec<&AgentRecord> = self
            .agents
            .iter()
            .filter(|a| {
                a.name_is_generated()
                    && a.status.is_live()
                    && a.spec.runtime == agent.spec.runtime
                    && a.project.as_ref().map(|p| p.id()) == agent.project.as_ref().map(|p| p.id())
                    && a.vcs.as_ref().and_then(|v| v.branch.as_deref()) == branch.as_deref()
            })
            .collect();
        if peers.len() < 2 {
            return base;
        }
        peers.sort_by_key(|a| (a.created_at, a.id.to_string()));
        match peers.iter().position(|a| a.id == agent.id) {
            Some(index) => format!("{base} ({})", index + 1),
            None => base,
        }
    }

    /// Journal lines name agents as they registered; show them as people
    /// read them. The author is found by id, never by name: two records can
    /// share a name and only one of them wrote the line.
    fn journal_line(&self, entry: &agentdocker_core::JournalEntry) -> String {
        let line = entry.line();
        let Some(id) = entry.agent.as_ref() else {
            return line;
        };
        let id = self.canonical_agent(id.as_str());
        let Some(agent) = self.agents.iter().find(|a| a.id.as_str() == id) else {
            return line;
        };
        let shown = self.display_name(agent);
        if shown == entry.agent_name {
            line
        } else {
            line.replacen(&entry.agent_name, &shown, 1)
        }
    }

    // ----- screens -------------------------------------------------------

    fn attach(&mut self, agent: String, ctx: Wake) {
        if let Some(client) = &self.client {
            self.terminal = Some(Terminal::attach(client.clone(), agent, ctx));
        }
    }

    /// Every room in the project, and what is still queued for this
    /// person in each.
    ///
    /// Two corrections to the obvious design, both of which this screen
    /// got wrong first. A channel between two agents need not have the
    /// human as a member — most will not — so listing only this person's
    /// memberships shows an empty screen while agents talk. And an inbox
    /// is a queue, not a transcript: a message an agent has taken is
    /// gone from it, so what is here is what has not been delivered yet,
    /// never the history of the room. Durable history needs somewhere to
    /// keep it and a bound on how much; neither exists, so this does not
    /// pretend otherwise.
    fn channel_projects(&self) -> BTreeSet<String> {
        self.agents
            .iter()
            .filter_map(|agent| agent.project.as_ref())
            .chain(self.shell.catalog.selected().map(|entry| &entry.project))
            .map(|project| project.id().to_string())
            .collect()
    }

    fn project_selector(&self, id: &str) -> String {
        self.agents
            .iter()
            .filter_map(|a| a.project.as_ref())
            .chain(self.shell.catalog.projects.iter().map(|e| &e.project))
            .find(|p| p.id().as_str() == id)
            .map_or_else(|| id.to_owned(), |p| p.root.to_string_lossy().into_owned())
    }
    /// Read the selected project's board, when there is one and the
    /// daemon is there: as many cards as are on view, so a board
    /// expanded past its first page stays expanded through a refresh.
    /// Read the selected project's usage as the screen is set: its
    /// window and grouping.
    pub(crate) fn request_usage(&mut self) {
        if self.connected.is_ok()
            && let Some(project) = self.selected_project_root()
        {
            let since = if self.shell.usage_since.is_empty() {
                usage::DEFAULT_SINCE
            } else {
                self.shell.usage_since
            };
            let by = self.shell.usage_by;
            if self
                .usage_pending
                .as_ref()
                .is_some_and(|p| p.1 == project && p.2 == since && p.3 == by)
            {
                return;
            }
            let Some(request) = self.usage_requests.checked_add(1) else {
                self.usage_error =
                    Some("Usage request counter exhausted; reopen the window".into());
                return;
            };
            self.usage_requests = request;
            self.usage_pending = Some((request, project.clone(), since, by));
            self.usage_error = None;
            self.send(Cmd::Usage {
                project,
                request,
                since,
                by,
            });
        }
    }

    pub(crate) fn request_tasks(&mut self) {
        if self.connected.is_ok()
            && let Some(project) = self.selected_project_root()
        {
            // A refresh supersedes every ask still on its way for this
            // board — earlier refreshes and a page alike: were a page to
            // land first, the refresh, sized to what was on view when it
            // was asked, would fold the board back; were an earlier
            // refresh to land last, it would overwrite the newer read.
            self.board_asks.retain(|_, (p, _)| *p != project);
            let on_view = match self.tasks.as_mut().filter(|b| b.project == project) {
                Some(board) => {
                    board.pending_more = None;
                    board.cards.len()
                }
                None => 0,
            };
            let limit = on_view.clamp(agentdocker_core::protocol::TASKS_LIMIT, BOARD_KEEP);
            let request = self.next_board_ask(&project, 0);
            self.send(Cmd::Tasks {
                project,
                request,
                offset: 0,
                limit,
            });
        }
    }

    /// The next page of the board on view, appended to it, up to what
    /// the window keeps. Not while any ask for this board is on its way:
    /// a page appended now would be to a board a refresh is about to
    /// replace, or would double one already asked for.
    pub(crate) fn request_more_tasks(&mut self) {
        if self.connected.is_ok()
            && let Some(project) = self.selected_project_root()
            && !self.board_asks.values().any(|(p, _)| *p == project)
            && self
                .tasks
                .as_ref()
                .is_some_and(|b| b.project == project && b.more && b.cards.len() < BOARD_KEEP)
        {
            let offset = self.tasks.as_ref().map_or(0, |b| b.cards.len());
            let limit = agentdocker_core::protocol::TASKS_LIMIT.min(BOARD_KEEP - offset);
            let request = self.next_board_ask(&project, offset);
            if let Some(board) = self.tasks.as_mut() {
                board.pending_more = Some(request);
            }
            self.send(Cmd::Tasks {
                project,
                request,
                offset,
                limit,
            });
        }
    }

    /// Number a board ask and remember what it was for. Asks are few
    /// and answered or refused in order; the map stays small, and is
    /// cleared with the board when another project is chosen.
    fn next_board_ask(&mut self, project: &str, offset: usize) -> u64 {
        self.task_requests += 1;
        let request = self.task_requests;
        self.board_asks
            .insert(request, (project.to_owned(), offset));
        request
    }

    fn request_channels(&mut self, id: String) {
        let selector = self.project_selector(&id);
        self.send(Cmd::Channels(id, selector));
    }
    /// The id of the project the sidebar is scoped to, when one is.
    pub(crate) fn selected_project_id(&self) -> Option<String> {
        let root = self.shell.catalog.selected.as_deref()?;
        self.shell
            .catalog
            .projects
            .iter()
            .map(|e| &e.project)
            .chain(self.agents.iter().filter_map(|a| a.project.as_ref()))
            .find(|p| p.root == root)
            .map(|p| p.id().as_str().to_owned())
    }
    /// The project the sidebar is scoped to, as a selector, or none for
    /// everywhere.
    pub(crate) fn conversation_scope(&self) -> Option<String> {
        self.shell
            .catalog
            .selected()
            .map(|entry| entry.project.dir().display().to_string())
    }

    /// Whether an archived message is in a history or the thread on view.
    pub(crate) fn archived_on_view(&self, id: &MessageId) -> bool {
        self.history
            .values()
            .flatten()
            .any(|m| &m.envelope.id == id)
            || self.thread.as_ref().is_some_and(|(root, replies)| {
                &root.envelope.id == id || replies.iter().any(|m| &m.envelope.id == id)
            })
    }

    /// Whether the open conversation's pane is on the screen: the Messages
    /// screen, and in a compact layout the conversation rather than the list.
    pub(crate) fn conversation_pane_visible(&self) -> bool {
        self.screen == Screen::Questions
            && self.shell.conversation.is_some()
            // A compact layout replaces this pane with the list or a thread.
            && (!self.messages_compact()
                || (self.shell.inbox_open && self.shell.thread.is_none()))
    }

    pub(crate) fn is_human(&self, id: &str) -> bool {
        id == agentdocker_core::HUMAN
            || self
                .agents
                .iter()
                .any(|a| a.id.as_str() == id && a.spec.runtime == agentdocker_core::HUMAN_RUNTIME)
    }

    /// Where a message typed into a conversation goes: the destination the
    /// conversation stands for, or none for the daemon's own notices.
    pub(crate) fn conversation_destination(&self, conversation: &str) -> Option<String> {
        use agentdocker_core::{ConversationId, ConversationKind};
        let id = ConversationId::from(conversation);
        match id.kind()? {
            ConversationKind::Everyone => id
                .everyone_project()
                .map(|p| format!("project:{}", p.as_str())),
            ConversationKind::All => Some("all".to_owned()),
            ConversationKind::Channel | ConversationKind::Collision => {
                id.channel_id().map(|c| format!("channel:{c}"))
            }
            // Only a direct conversation the person is in can be written
            // to; one between two agents is theirs to read here.
            ConversationKind::Dm => {
                let (a, b) = id.dm_parties()?;
                if self.is_human(a) {
                    Some(b.to_owned())
                } else if self.is_human(b) {
                    Some(a.to_owned())
                } else {
                    None
                }
            }
            ConversationKind::Notices => None,
        }
    }

    /// Something was said or read: the sidebar and the open conversation
    /// are fetched again, if this daemon has conversations at all.
    /// Keep one conversation's archive within the window's bound: the
    /// newest rows stay, and once the earliest have gone they can be paged
    /// in again.
    /// How far back a notification's message is looked for: pages of the
    /// archive, past the newest one.
    const REVEAL_PAGES: usize = 5;

    /// A notification needs a fresh answer before cached history can say
    /// its message is missing. A new epoch rejects already queued responses,
    /// including one for the same conversation from before the click.
    fn start_archive_reveal(&mut self, conversation: String, message: MessageId) {
        self.cancel_reveal();
        self.history_epoch += 1;
        if self.queue_reveal_page(Cmd::History(conversation.clone(), self.history_epoch)) {
            self.reveal_archived = Some(Seek {
                conversation,
                message,
                pages: 0,
                before: None,
                refresh_pending: true,
            });
        }
    }

    /// Unlike a periodic refresh, a refused notification lookup must finish
    /// with an error: there will be no page response to advance its cursor.
    fn queue_reveal_page(&mut self, command: Cmd) -> bool {
        if let Err(queue::Rejected { reason, .. }) = self.tx.send(command) {
            self.cancel_reveal();
            self.say(reason);
            false
        } else {
            true
        }
    }

    /// A page of a conversation arrived: if a notification's message is
    /// wanted there, it is either on view now — the next tick scrolls to
    /// it — or further back, and the page before is asked for, up to a
    /// bound; past that the person is told what was searched, and the
    /// conversation stays open at its newest. One page is in flight at a
    /// time: a refresh of the newest page while an earlier one is on its
    /// way asks for nothing more, so the bound counts pages, not
    /// refreshes. The seek is the open conversation's: navigation away
    /// ends it.
    fn seek_archived(&mut self, conversation: &str) {
        let Some(seek) = self.reveal_archived.clone() else {
            return;
        };
        if seek.conversation != conversation {
            return;
        }
        if self.shell.conversation.as_deref() != Some(conversation) {
            self.reveal_archived = None;
            return;
        }
        if seek.refresh_pending {
            return;
        }
        // No page yet: the one asked for is still on its way.
        let Some(messages) = self.history.get(conversation) else {
            return;
        };
        if messages.iter().any(|m| m.envelope.id == seek.message) {
            self.reveal_archived = None;
            self.shell.pending_answer_reveal = None;
            self.shell.reveal_archived_next = Some(seek.message);
            return;
        }
        let first = messages.first().map(|m| m.seq);
        let loaded = messages.len();
        // The archive's start is here and the message is not: it was
        // pruned, and no page can bring it. An empty page before the one
        // asked for says the same, so this comes before the cursor is
        // looked at. Nor can a page come while disconnected.
        let ended = if self.history_complete.contains(conversation) || first.is_none() {
            Some(format!(
                "This notification's message is no longer in this conversation's archive ({loaded} messages here)."
            ))
        } else if self.connected.is_err() {
            Some(format!(
                "This notification's message is not in the {loaded} messages loaded, and the daemon is not connected."
            ))
        } else {
            None
        };
        if let Some(text) = ended {
            self.reveal_archived = None;
            self.say(text);
            return;
        }
        // The page before the one asked for has not arrived: this is a
        // refresh of what is already here, not an answer.
        if let (Some(asked), Some(first)) = (seek.before, first)
            && first >= asked
        {
            return;
        }
        // What the window keeps is bounded, and a conversation at that
        // bound drops the page it is given before it is read — so Show
        // earlier messages cannot help there either, and is not offered.
        if loaded + HISTORY_PAGE > HISTORY_KEEP {
            self.reveal_archived = None;
            self.say(format!(
                "This notification's message is not among the {loaded} messages this window keeps of a conversation."
            ));
            return;
        }
        if seek.pages >= Self::REVEAL_PAGES {
            self.reveal_archived = None;
            self.say(format!(
                "This notification's message is not in the {loaded} messages loaded; Show earlier messages reads further back."
            ));
            return;
        }
        let first = first.expect("checked");
        if self.queue_reveal_page(Cmd::HistoryBefore(
            conversation.to_owned(),
            first,
            self.history_epoch,
        )) {
            self.reveal_archived = Some(Seek {
                pages: seek.pages + 1,
                before: Some(first),
                ..seek
            });
        }
    }

    /// A seek and a deferred scroll are a notification's, for the
    /// conversation it opened: another notification, or navigation, ends
    /// them, so a late page for the old conversation moves nothing.
    pub(crate) fn cancel_reveal(&mut self) {
        self.reveal_archived = None;
        self.shell.reveal_archived_next = None;
    }

    fn keep_history(
        &mut self,
        conversation: String,
        mut messages: Vec<agentdocker_core::ArchivedMessage>,
    ) {
        if messages.len() > HISTORY_KEEP {
            let drop = messages.len() - HISTORY_KEEP;
            messages.drain(..drop);
            self.history_complete.remove(&conversation);
        }
        self.history.insert(conversation, messages);
        while self.history.len() > HISTORY_CONVERSATIONS
            && let Some(oldest) = self
                .history
                .keys()
                .find(|k| Some(k.as_str()) != self.shell.conversation.as_deref())
                .cloned()
        {
            self.history.remove(&oldest);
            self.history_complete.remove(&oldest);
        }
    }

    fn on_conversation_activity(&mut self) {
        if self.conversations_supported == Some(false) {
            return;
        }
        self.send(Cmd::Conversations(self.conversation_scope()));
        if let Some(open) = self.shell.conversation.clone() {
            self.send(Cmd::History(open, self.history_epoch));
        }
        if let Some(root) = self.shell.thread.clone() {
            self.send(Cmd::Thread(root, self.history_epoch));
        }
    }

    fn request_journal(&mut self, id: String) {
        let selector = self.project_selector(&id);
        self.send(Cmd::Journal(id, selector));
    }

    fn remember_console_command(&mut self, line: &str) {
        self.console_recall = None;
        // Never save a truncated command for later execution.
        if line.len() > CONSOLE_HISTORY_BYTES
            || self.console_history.last().is_some_and(|last| last == line)
        {
            return;
        }
        self.console_history.push(line.to_owned());
        let mut bytes: usize = self.console_history.iter().map(String::len).sum();
        while self.console_history.len() > CONSOLE_HISTORY_COMMANDS || bytes > CONSOLE_HISTORY_BYTES
        {
            bytes -= self.console_history.remove(0).len();
        }
    }

    /// Retain a UTF-8 tail without growing the window's buffer with each reply.
    fn append_console(&mut self, text: &str) {
        fn boundary(text: &str, mut at: usize) -> usize {
            while !text.is_char_boundary(at) {
                at += 1;
            }
            at
        }
        if text.len() >= CONSOLE_BYTES {
            let at = boundary(text, text.len() - CONSOLE_BYTES);
            self.console_output = text[at..].to_owned();
            self.console_truncated = true;
            return;
        }
        let excess = (self.console_output.len() + text.len()).saturating_sub(CONSOLE_BYTES);
        if excess > 0 {
            let at = boundary(&self.console_output, excess);
            self.console_output.drain(..at);
            self.console_truncated = true;
        }
        self.console_output.push_str(text);
        // String::drain retains capacity. Also release any previously oversized
        // allocation, rather than merely hiding its bytes from the UI.
        if self.console_output.capacity() > CONSOLE_BYTES * 2 {
            self.console_output.shrink_to(CONSOLE_BYTES);
        }
    }

    /// Step back and forward through what has been typed, as a shell does.
    fn recall(&mut self, back: bool) {
        if self.console_history.is_empty() {
            return;
        }
        let last = self.console_history.len() - 1;
        self.console_recall = match (self.console_recall, back) {
            (None, true) => Some(last),
            (Some(0), true) => Some(0),
            (Some(at), true) => Some(at - 1),
            (Some(at), false) if at >= last => None,
            (Some(at), false) => Some(at + 1),
            (None, false) => None,
        };
        self.console_input = match self.console_recall {
            Some(at) => self.console_history[at].clone(),
            None => String::new(),
        };
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.worker_stop
            .store(true, std::sync::atomic::Ordering::Release);
    }
}

/// A composer's draft key: the conversation, or `<conversation>#<root>`
/// for the composer of a thread, so a thread never takes the words typed
/// for the conversation itself.
pub(crate) fn draft_key(conversation: &str, root: Option<&MessageId>) -> String {
    match root {
        Some(root) => format!("{conversation}#{root}"),
        None => conversation.to_owned(),
    }
}

pub(crate) fn split_draft_key(key: &str) -> (&str, Option<MessageId>) {
    match key.split_once('#') {
        Some((conversation, root)) => (conversation, Some(MessageId::from(root.to_owned()))),
        None => (key, None),
    }
}

/// Where the person running this window is working, if that can be
/// said at all.
///
/// `me` follows the person to wherever they are, which is right when
/// the window is started from a shell inside a checkout and wrong when
/// it is started any other way. An app opened from the Dock or the app
/// switcher inherits `/` as its working directory, and reporting that
/// moves the human's record out of whatever project they were in and
/// into the filesystem root — where `commit`, `journal` and everything
/// else that needs a checkout then has nothing to work with.
///
/// So: a directory is only reported when it could plausibly be work.
/// `None` leaves the record alone, which is the right answer when we
/// have nothing to say rather than a reason to say `/`.
fn launched_in() -> Option<std::path::PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    // The filesystem root: a Dock launch, not a choice.
    cwd.parent()?;
    if std::env::var_os("HOME").is_some_and(|home| cwd == std::path::Path::new(&home)) {
        return None; // the home directory: a launcher's default, not a choice
    }
    Some(cwd)
}

/// "45s", "3m", "2h".
/// A health check's status in the colour the runtimes table gives the
/// same thing, so the two panels above and below the separator do not
/// say it two different ways.
/// The inbox split into rooms and everything else.
///
/// Route by the envelope's destination, including ordinary string payloads.
/// A payload field cannot move a message to a different room. Prefer the
/// inbox's authoritative envelope when it overlaps a local send receipt.
type Grouped<'a> = (
    BTreeMap<String, Vec<&'a agentdocker_core::Envelope>>,
    Vec<&'a agentdocker_core::Envelope>,
);

fn by_room<'a>(inbox: impl IntoIterator<Item = &'a agentdocker_core::Envelope>) -> Grouped<'a> {
    let mut said: BTreeMap<String, Vec<&agentdocker_core::Envelope>> = BTreeMap::new();
    let mut direct = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for message in inbox {
        if !seen.insert(&message.id) {
            continue;
        }
        match &message.to {
            agentdocker_core::Destination::Channel(room) => {
                said.entry(room.to_string()).or_default().push(message)
            }
            _ => direct.push(message),
        }
    }
    for messages in said.values_mut() {
        messages.sort_by_key(|message| (message.sent_at, &message.id));
    }
    (said, direct)
}

/// What the window asks for when it reads the conversation.
///
/// Named so the one thing that matters about it can be asserted: a
/// window that polls every couple of seconds must never drain the inbox
/// it is showing, or it deletes the conversation out from under whoever
/// is reading it.
fn inbox_request() -> Request {
    Request::Inbox {
        agent: agentdocker_core::HUMAN.to_owned(),
        drain: false,
    }
}

fn span(secs: i64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s => format!("{}h", s / 3600),
    }
}

// ----- threads ---------------------------------------------------------------

/// Shell commands keep their order without delaying daemon requests. Each
/// window owns three fixed workers, each with at most four queued jobs.
const SHELL_QUEUE_CAPACITY: usize = 4;

fn lane<T: Send + 'static>(
    tx: SyncSender<Msg>,
    ctx: Wake,
    cancelled: Arc<std::sync::atomic::AtomicBool>,
    mut work: impl FnMut(T) -> Msg + Send + 'static,
) -> (SyncSender<T>, std::thread::JoinHandle<()>) {
    let (lane, jobs) = sync_channel::<T>(SHELL_QUEUE_CAPACITY);
    let worker = std::thread::spawn(move || {
        while let Ok(job) = jobs.recv() {
            if cancelled.load(std::sync::atomic::Ordering::Acquire) {
                break;
            }
            if tx.send(work(job)).is_err() {
                break;
            }
            ctx.request_repaint();
        }
    });
    (lane, worker)
}

fn submit<T>(lane: &SyncSender<T>, job: T, rejected: impl FnOnce(String) -> Msg) -> Option<Msg> {
    use std::sync::mpsc::TrySendError;
    match lane.try_send(job) {
        Ok(()) => None,
        Err(TrySendError::Full(_)) => Some(rejected(
            "Command not queued: this worker's queue is full; try again after it catches up."
                .into(),
        )),
        Err(TrySendError::Disconnected(_)) => Some(rejected(
            "Command not queued: this worker stopped; reopen agentdocker.".into(),
        )),
    }
}

fn spawn_worker(
    client: Arc<Client>,
    rx: CommandReceiver,
    tx: SyncSender<Msg>,
    ctx: Wake,
    cancelled: Arc<std::sync::atomic::AtomicBool>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let (consoles, console_worker) = lane(
            tx.clone(),
            ctx.clone(),
            cancelled.clone(),
            |(line, cwd): (String, Option<std::path::PathBuf>)| {
                Msg::Console(console(&line, cwd.as_deref()))
            },
        );
        let (setups, setup_worker) = lane(
            tx.clone(),
            ctx.clone(),
            cancelled.clone(),
            |args: Vec<String>| Msg::Setup(setup(&args)),
        );
        let (desktops, desktop_worker) = lane(
            tx.clone(),
            ctx.clone(),
            cancelled.clone(),
            |args: Vec<String>| Msg::Desktop(desktop(&args)),
        );
        let (updates, update_worker) =
            lane(tx.clone(), ctx.clone(), cancelled.clone(), |(): ()| {
                Msg::UpdateChecked(check_update())
            });
        while let Ok(cmd) = rx.recv() {
            if cancelled.load(std::sync::atomic::Ordering::Acquire) {
                break;
            }
            // Admission never waits behind a subprocess. A rejected job
            // returns the usual completion shape to clear the UI's busy state.
            let rejected = match cmd {
                Cmd::Console(line, cwd) => submit(&consoles, (line, cwd), Msg::Console),
                Cmd::Setup(args) => submit(&setups, args, |error| Msg::Setup(Err(error))),
                Cmd::Desktop(args) => submit(&desktops, args, |error| Msg::Desktop(Err(error))),
                Cmd::UpdateCheck => submit(&updates, (), |error| Msg::UpdateChecked(Err(error))),
                daemon => {
                    let answer = match &daemon {
                        Cmd::Answer(id, _) => Some(id.clone()),
                        _ => None,
                    };
                    let launch = matches!(&daemon, Cmd::Launch(_));
                    let dismissal = match &daemon {
                        Cmd::DismissMessages(id) => Some(id.clone()),
                        _ => None,
                    };
                    let channel = match &daemon {
                        Cmd::ChannelSend(id, _) => Some(id.clone()),
                        _ => None,
                    };
                    let session = match &daemon {
                        Cmd::SessionSend(id, _) | Cmd::ProjectSend(id, _, _) => Some(id.clone()),
                        _ => None,
                    };
                    let conversation_draft = match &daemon {
                        Cmd::ConversationSend { draft, .. } => Some(draft.clone()),
                        _ => None,
                    };
                    let pause = match &daemon {
                        Cmd::Pause { request, .. } | Cmd::ResumeProject(request) => {
                            Some(request.clone())
                        }
                        _ => None,
                    };
                    let outcome = run(&client, daemon);
                    let disconnected = outcome
                        .as_ref()
                        .err()
                        .is_some_and(|error| error.downcast_ref::<RemoteError>().is_none());
                    let msg = match outcome {
                        Ok(Some(msg)) => msg,
                        Ok(None) => continue,
                        Err(err) => {
                            if let Some(request) = pause
                                && tx.send(Msg::Paused(request, Err(format!(
                                    "The result was not confirmed. Check the project status before retrying: {err:#}"
                                )))).is_err()
                            {
                                break;
                            }
                            if let Some(id) = session
                                && tx
                                    .send(Msg::SessionSent(
                                        id,
                                        Err(format!("Queue acceptance was not confirmed: {err:#}")),
                                    ))
                                    .is_err()
                            {
                                break;
                            }
                            if let Some(draft) = conversation_draft
                                && tx
                                    .send(Msg::ConversationSent(draft, Err(format!("{err:#}"))))
                                    .is_err()
                            {
                                break;
                            }
                            if let Some(id) = dismissal
                                && tx
                                    .send(Msg::MessagesDismissed(id, Err(format!("{err:#}"))))
                                    .is_err()
                            {
                                break;
                            }
                            if launch && tx.send(Msg::Launched(Err(format!("{err:#}")))).is_err() {
                                break;
                            }
                            if let Some(id) = channel
                                && tx
                                    .send(Msg::ChannelSent(id, Err(format!("{err:#}"))))
                                    .is_err()
                            {
                                break;
                            }
                            if let Some(id) = answer {
                                // Preserve the draft even when no daemon reply arrived.
                                if tx.send(Msg::Answered(id, Err(format!("{err:#}")))).is_err() {
                                    break;
                                }
                            }
                            failure_message(err)
                        }
                    };
                    if tx.send(msg).is_err() {
                        break;
                    }
                    if !disconnected && tx.send(Msg::Connected).is_err() {
                        break;
                    }
                    ctx.request_repaint();
                    continue;
                }
            };
            if let Some(message) = rejected {
                if tx.send(message).is_err() {
                    break;
                }
                ctx.request_repaint();
            }
        }
        // Closing a window must not run its remaining queued installations.
        // In-flight subprocesses keep their existing deadlines; joining happens
        // on this background worker, never on the UI thread.
        cancelled.store(true, std::sync::atomic::Ordering::Release);
        drop((consoles, setups, desktops, updates));
        for worker in [console_worker, setup_worker, desktop_worker, update_worker] {
            let _ = worker.join();
        }
    })
}

fn failure_message(error: anyhow::Error) -> Msg {
    if error.downcast_ref::<RemoteError>().is_none() {
        Msg::Disconnected(format!("{error:#}"))
    } else {
        Msg::Status(format!("{error:#}"))
    }
}

/// One command against the daemon; `Ok(None)` when there is nothing to
/// show for it.
fn run(client: &Client, cmd: Cmd) -> anyhow::Result<Option<Msg>> {
    Ok(match cmd {
        Cmd::Agents => match client.call(&Request::List {
            all: true,
            project: None,
            labels: BTreeMap::new(),
        })? {
            Response::Agents { agents, aliases } => Some(Msg::Agents(
                agents,
                aliases
                    .into_iter()
                    .map(|(old, current)| (old.to_string(), current.to_string()))
                    .collect(),
            )),
            _ => None,
        },
        Cmd::Leases => match client.call(&Request::Leases {
            agent: None,
            resource: None,
        })? {
            Response::Leases { leases } => Some(Msg::Leases(leases)),
            _ => None,
        },
        Cmd::Runtimes => match client.call(&Request::Runtimes)? {
            Response::Runtimes { runtimes } => Some(Msg::Runtimes(runtimes)),
            _ => None,
        },
        // A file under the state home, not a daemon question: the
        // connector is its own process and the daemon does not know it.
        Cmd::Connector => Some(Msg::Connector(agentdocker_host::connector::serving(
            &agentdocker_host::dirs::home(),
        ))),
        Cmd::Discovered => match client.call(&Request::Discover)? {
            Response::Processes { processes } => Some(Msg::Discovered(processes)),
            _ => None,
        },
        Cmd::Journal(project, selector) => match client.call(&Request::Journal {
            project: selector,
            since_seq: None,
            until_seq: None,
            agent: None,
            branch: None,
            kind: None,
            path: None,
            grep: None,
            limit: JOURNAL_WINDOW,
            digest: None,
        })? {
            Response::Journal {
                entries, head_seq, ..
            } => Some(Msg::Journal(project, head_seq, entries)),
            _ => None,
        },
        Cmd::Me => match client.call(&Request::Me {
            workdir: launched_in(),
        })? {
            Response::Agent { .. } => None,
            _ => None,
        },
        Cmd::SessionLog(agent) => {
            let result = read_session_log(client, &agent).map_err(|error| format!("{error:#}"));
            Some(Msg::SessionLog(agent, result))
        }
        Cmd::Tasks {
            project,
            request,
            offset,
            limit,
        } => {
            let result = match client.call(&Request::Tasks {
                project: Some(project.clone()),
                column: None,
                archived: false,
                offset,
                limit,
            }) {
                Ok(Response::Tasks { tasks, more }) => Ok((tasks, more)),
                Ok(Response::Error { message, .. }) => Err(message),
                Ok(other) => Err(format!("Unexpected reply: {other:?}")),
                Err(error) => Err(format!("{error:#}")),
            };
            Some(Msg::Tasks(project, request, result))
        }
        Cmd::Usage {
            project,
            request,
            since,
            by,
        } => {
            let result = match client.call(&Request::Usage {
                project: Some(project.clone()),
                agent: None,
                since: Some(since.to_owned()),
                until: None,
                by,
            }) {
                Ok(Response::Usage { report }) => Ok(report),
                Ok(Response::Error { message, .. }) => Err(message),
                Ok(other) => Err(format!("Unexpected reply: {other:?}")),
                Err(error) => Err(format!("{error:#}")),
            };
            Some(Msg::Usage(project, request, result))
        }
        Cmd::TaskCreate {
            project,
            request,
            title,
            acceptance,
            column,
        } => {
            let result = match client.call(&Request::TaskCreate {
                from: agentdocker_core::HUMAN.into(),
                project: Some(project.clone()),
                title,
                acceptance,
                column: Some(column),
                links: Vec::new(),
            }) {
                Ok(Response::Task { .. }) => Ok(()),
                Ok(Response::Error { message, .. }) => Err(message),
                Ok(other) => Err(format!("Unexpected reply: {other:?}")),
                Err(error) => Err(format!("{error:#}")),
            };
            Some(Msg::TaskCreated(project, request, result))
        }
        Cmd::TaskMove { task, column } => {
            let result = match client.call(&Request::TaskMove {
                agent: agentdocker_core::HUMAN.into(),
                task: task.to_string(),
                column,
            }) {
                Ok(Response::Task { .. }) => Ok(()),
                Ok(Response::Error { message, .. }) => Err(message),
                Ok(other) => Err(format!("Unexpected reply: {other:?}")),
                Err(error) => Err(format!("{error:#}")),
            };
            Some(Msg::TaskChanged(result))
        }
        Cmd::TaskAssign { task, assignee } => {
            let result = match client.call(&Request::TaskUpdate {
                agent: agentdocker_core::HUMAN.into(),
                task: task.to_string(),
                title: None,
                acceptance: None,
                assignee: Some(assignee.map(|a| a.to_string()).unwrap_or_default()),
                links: None,
            }) {
                Ok(Response::Task { .. }) => Ok(()),
                Ok(Response::Error { message, .. }) => Err(message),
                Ok(other) => Err(format!("Unexpected reply: {other:?}")),
                Err(error) => Err(format!("{error:#}")),
            };
            Some(Msg::TaskChanged(result))
        }
        Cmd::TaskArchive(task) => {
            let result = match client.call(&Request::TaskArchive {
                agent: agentdocker_core::HUMAN.into(),
                task: task.to_string(),
            }) {
                Ok(Response::Ok) => Ok(()),
                Ok(Response::Error { message, .. }) => Err(message),
                Ok(other) => Err(format!("Unexpected reply: {other:?}")),
                Err(error) => Err(format!("{error:#}")),
            };
            Some(Msg::TaskChanged(result))
        }
        Cmd::Pauses => match client.call(&Request::Pauses)? {
            Response::Pauses { pauses } => Some(Msg::Pauses(pauses)),
            _ => None,
        },
        Cmd::Pause { request, reason } => {
            let result = match client.call(&Request::Pause {
                from: agentdocker_core::HUMAN.into(),
                project: Some(request.project.clone()),
                reason,
            }) {
                Ok(Response::Pause { .. }) => Ok(()),
                Ok(other) => anyhow::bail!("Unexpected pause reply: {other:?}"),
                Err(error) if error.downcast_ref::<RemoteError>().is_some() => {
                    Err(format!("{error:#}"))
                }
                Err(error) => return Err(error),
            };
            Some(Msg::Paused(request, result))
        }
        Cmd::ResumeProject(request) => {
            let result = match client.call(&Request::ResumeProject {
                from: agentdocker_core::HUMAN.into(),
                project: Some(request.project.clone()),
            }) {
                Ok(Response::Ok) => Ok(()),
                Ok(other) => anyhow::bail!("Unexpected resume reply: {other:?}"),
                Err(error) if error.downcast_ref::<RemoteError>().is_some() => {
                    Err(format!("{error:#}"))
                }
                Err(error) => return Err(error),
            };
            Some(Msg::Paused(request, result))
        }
        Cmd::Activity => match client.call(&Request::Activity {
            agent: None,
            project: None,
            all: true,
        })? {
            Response::Activity { activity } => Some(Msg::Activity(activity)),
            _ => None,
        },
        // Every room in the named project, not only the ones this person is
        // in. A channel between two agents need not have the human as a
        // member — most will not — and a window that listed only its
        // own memberships would show nothing while agents talked.
        Cmd::Channels(project, selector) => match client.call(&Request::Channels {
            project: selector,
            all: false,
            agent: None,
        })? {
            Response::Channels { channels } => Some(Msg::Channels(project, channels)),
            _ => None,
        },
        // Never drained: this is a window looking, not a consumer
        // taking. Draining here would delete the conversation out from
        // under whoever is reading it.
        Cmd::Inbox => match client.call(&inbox_request())? {
            Response::Messages { messages } => Some(Msg::Inbox(messages)),
            _ => None,
        },
        Cmd::Questions => match client.call(&Request::Questions {
            agent: Some(agentdocker_core::HUMAN.to_owned()),
        })? {
            Response::Questions { questions } => Some(Msg::Questions(questions)),
            _ => None,
        },
        Cmd::Answer(message, text) => {
            client.call(&Request::Answer {
                from: None,
                message: message.clone(),
                text,
            })?;
            Some(Msg::Answered(message, Ok(())))
        }
        Cmd::DismissMessages(message) => {
            let response = client.call(&Request::AckInbox {
                agent: agentdocker_core::HUMAN.to_owned(),
                messages: message.clone(),
            })?;
            anyhow::ensure!(
                matches!(response, Response::Ok),
                "The daemon did not confirm dismissal; messages were retained."
            );
            Some(Msg::MessagesDismissed(message, Ok(())))
        }
        Cmd::Adopt(pid) => Some(
            match client
                .call(&Request::Adopt {
                    pid,
                    name: None,
                    runtime: None,
                })
                .with_context(|| format!("pid {pid}"))?
            {
                Response::Agent { agent } => Msg::Status(format!("adopted {}", agent.spec.name)),
                _ => Msg::Status(format!("adopted pid {pid}")),
            },
        ),
        Cmd::AdoptAll => {
            let Response::Processes { processes } = client.call(&Request::Discover)? else {
                return Ok(None);
            };
            let mut adopted = 0;
            for process in processes {
                client
                    .call(&Request::Adopt {
                        pid: process.pid,
                        name: None,
                        runtime: None,
                    })
                    .with_context(|| {
                        format!(
                            "adopted {adopted} process(es); failed at pid {}",
                            process.pid
                        )
                    })?;
                adopted += 1;
            }
            Some(Msg::Status(format!("adopted {adopted} process(es)")))
        }
        Cmd::Stop(agent) => {
            client.call(&Request::Stop {
                agent: agent.clone(),
                force: false,
            })?;
            Some(Msg::Status(format!(
                "stopping {}",
                agent.chars().take(12).collect::<String>()
            )))
        }
        Cmd::ResumeProvider(agent, blocked_at) => {
            let response = client.call(&Request::ResumeProvider { agent, blocked_at })?;
            anyhow::ensure!(
                matches!(response, Response::Ok),
                "Provider resumption refused: {response:?}"
            );
            Some(Msg::Status(
                "Delivery resumed; previously received input will not be replayed".into(),
            ))
        }
        Cmd::RetryController(agent) => {
            let response = client.call(&Request::RetryController { agent })?;
            anyhow::ensure!(
                matches!(response, Response::Ok),
                "Receiver retry refused: {response:?}"
            );
            Some(Msg::Status(
                "The receiver will be started again within a second; queued input waits for it"
                    .into(),
            ))
        }
        Cmd::Launch(spec) => match client.call(&Request::Run { spec: *spec })? {
            Response::Agent { agent } => Some(Msg::Launched(Ok(agent.id.to_string()))),
            _ => Some(Msg::Launched(Err("Unexpected launch response".into()))),
        },
        Cmd::Conversations(project) => match client.call(&Request::Conversations {
            project,
            reader: None,
        }) {
            Ok(Response::Conversations { conversations }) => {
                Some(Msg::Conversations(Ok(conversations)))
            }
            Ok(_) => None,
            // An older daemon answers `invalid` for a request it has never
            // heard of; that is "no conversations here", not a failure.
            // Anything else (storage, transport) is a failure like any
            // other and leaves the screen as it is.
            Err(error)
                if error
                    .downcast_ref::<RemoteError>()
                    .is_some_and(|e| e.code == agentdocker_core::ErrorCode::Invalid) =>
            {
                Some(Msg::Conversations(Err(error.to_string())))
            }
            Err(error) => return Err(error),
        },
        Cmd::History(conversation, epoch) => match client.call(&Request::History {
            conversation: agentdocker_core::ConversationId::from(conversation.clone()),
            before_seq: None,
            limit: HISTORY_PAGE,
        })? {
            Response::History { messages } => Some(Msg::History(conversation, epoch, messages)),
            _ => None,
        },
        Cmd::HistoryBefore(conversation, before, epoch) => match client.call(&Request::History {
            conversation: agentdocker_core::ConversationId::from(conversation.clone()),
            before_seq: Some(before),
            limit: HISTORY_PAGE,
        })? {
            Response::History { messages } => {
                Some(Msg::HistoryEarlier(conversation, epoch, messages))
            }
            _ => None,
        },
        Cmd::Thread(message, epoch) => {
            // A thread is read whole: page after page until one is short,
            // within the archive's own per-conversation cap. A root the
            // daemon no longer has was pruned.
            let mut after_seq = None;
            let mut replies = Vec::new();
            let root = loop {
                let page = client.call(&Request::Thread {
                    message: message.clone(),
                    after_seq,
                    limit: HISTORY_PAGE,
                });
                let (root, page) = match page {
                    Ok(Response::Thread { root, replies }) => (root, replies),
                    Ok(_) => return Ok(None),
                    Err(error)
                        if error
                            .downcast_ref::<RemoteError>()
                            .is_some_and(|e| e.code == agentdocker_core::ErrorCode::NotFound) =>
                    {
                        return Ok(Some(Msg::ThreadGone(message, epoch)));
                    }
                    Err(error) => return Err(error),
                };
                let short = page.len() < HISTORY_PAGE;
                after_seq = page.last().map(|m| m.seq);
                replies.extend(page);
                if short || replies.len() >= THREAD_CAP {
                    break root;
                }
            };
            Some(Msg::Thread(epoch, root, replies))
        }
        Cmd::MarkRead(conversation, through) => {
            client.call(&Request::MarkRead {
                conversation: agentdocker_core::ConversationId::from(conversation),
                through,
                reader: None,
            })?;
            None
        }
        Cmd::ChannelOpen {
            request,
            name,
            task,
            members,
            project,
        } => {
            let result = match client.call(&Request::ChannelOpen {
                agent: agentdocker_core::HUMAN.into(),
                task,
                members,
                name: Some(name),
                project,
            }) {
                Ok(Response::Channel { channel }) => Ok(channel.id),
                Ok(Response::Error { message, .. }) => Err(message),
                Ok(other) => Err(format!("Unexpected reply: {other:?}")),
                Err(error) => Err(format!("{error:#}")),
            };
            Some(Msg::ChannelOpened(request, result))
        }
        Cmd::ChannelInvite {
            request,
            channel,
            member,
        } => {
            let result = match client.call(&Request::ChannelInvite {
                agent: agentdocker_core::HUMAN.into(),
                channel,
                member: member.clone(),
            }) {
                Ok(Response::Channel { channel }) => Ok(channel),
                Ok(Response::Error { message, .. }) => Err(message),
                Ok(other) => Err(format!("Unexpected reply: {other:?}")),
                Err(error) => Err(format!("{error:#}")),
            };
            Some(Msg::ChannelInvited(request, member, result))
        }
        Cmd::ConversationSend {
            draft,
            to,
            text,
            reply_to,
        } => {
            let response = client.call(&Request::Send {
                from: agentdocker_core::HUMAN.into(),
                to,
                kind: "message".into(),
                payload: serde_json::json!({ "text": text }),
                reply_to,
                links: Vec::new(),
            })?;
            let result = match response {
                Response::Sent {
                    message,
                    recipient_readiness,
                    ..
                } => Ok(QueuedSend {
                    message,
                    readiness: recipient_readiness,
                }),
                other => Err(format!("Unexpected send response: {other:?}")),
            };
            Some(Msg::ConversationSent(draft, result))
        }
        Cmd::SessionSend(agent, text) => {
            let response = client.call(&Request::Send {
                from: agentdocker_core::HUMAN.into(),
                to: agent.clone(),
                kind: "message".into(),
                payload: serde_json::Value::String(text),
                reply_to: None,
                links: Vec::new(),
            })?;
            let result = match response {
                Response::Sent {
                    message,
                    recipient_readiness,
                    ..
                } => Ok(QueuedSend {
                    message,
                    readiness: recipient_readiness,
                }),
                _ => Err("Unexpected message response".into()),
            };
            Some(Msg::SessionSent(agent, result))
        }
        Cmd::ProjectSend(agent, project, text) => {
            let response = client.call(&Request::Send {
                from: agentdocker_core::HUMAN.into(),
                to: format!("project:{project}"),
                kind: "message".into(),
                payload: serde_json::Value::String(text),
                reply_to: None,
                links: Vec::new(),
            })?;
            let result = match response {
                Response::Sent {
                    message,
                    recipient_readiness,
                    ..
                } => Ok(QueuedSend {
                    message,
                    readiness: recipient_readiness,
                }),
                _ => Err("Unexpected message response".into()),
            };
            Some(Msg::SessionSent(agent, result))
        }
        Cmd::ChannelSend(channel, text) => {
            let response = client.call(&Request::Send {
                from: agentdocker_core::HUMAN.into(),
                to: format!("channel:{channel}"),
                kind: "message".into(),
                payload: serde_json::Value::String(text),
                reply_to: None,
                links: Vec::new(),
            })?;
            let result = match response {
                Response::Sent {
                    message,
                    recipient_readiness,
                    ..
                } => Ok(QueuedSend {
                    message,
                    readiness: recipient_readiness,
                }),
                _ => Err("Unexpected message response".into()),
            };
            Some(Msg::ChannelSent(channel, result))
        }
        Cmd::Setup(args) => Some(Msg::Setup(setup(&args))),
        Cmd::Desktop(args) => Some(Msg::Desktop(desktop(&args))),
        Cmd::UpdateCheck => Some(Msg::UpdateChecked(check_update())),
        Cmd::Console(line, cwd) => Some(Msg::Console(console(&line, cwd.as_deref()))),
    })
}

/// Any `agentdocker` command, run with the CLI beside this binary. The
/// command line is the complete surface and it keeps growing; a window
/// that mirrored it in widgets would always lag behind, so the window
/// runs the real thing and shows what it said.
fn console(line: &str, project: Option<&std::path::Path>) -> String {
    let words = match shell_words(line) {
        Some(words) if !words.is_empty() => words,
        Some(_) => return String::new(),
        None => return "unbalanced quotes".to_owned(),
    };
    // `agentdocker agentdocker ps` is a typo worth forgiving.
    let args: Vec<String> = match words.split_first() {
        Some((first, rest)) if first == "agentdocker" => rest.to_vec(),
        _ => words,
    };
    let cli = beside("agentdocker");
    let Some(cli) = cli.to_str().map(str::to_owned) else {
        return "the CLI path is not UTF-8".to_owned();
    };
    let argv: Vec<String> = std::iter::once(cli.clone()).chain(args).collect();
    let cwd = match project
        .map(std::path::Path::to_owned)
        .map(Ok)
        .unwrap_or_else(std::env::current_dir)
    {
        Ok(cwd) => cwd,
        Err(err) => return err.to_string(),
    };
    // Bounded, because several commands never finish on their own:
    // `watch`, `events` and `logs -f` stream until the connection closes.
    // Without a limit the first one typed would take the worker thread
    // with it and the window would stop answering.
    match agentdocker_host::command::run(&cwd, &argv, CONSOLE_TIMEOUT) {
        Ok(output) if output.text.trim().is_empty() => {
            format!("(exit {})", if output.success { 0 } else { 1 })
        }
        Ok(output) => output.text,
        Err(err) => format!("cannot run {cli}: {err}"),
    }
}

/// Split a command line on whitespace, honouring single and double
/// quotes, so a note or a summary can contain spaces.
fn shell_words(line: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote: Option<char> = None;
    let mut any = false;
    for c in line.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => word.push(c),
            None if c == '\'' || c == '"' => {
                quote = Some(c);
                any = true;
            }
            None if c.is_whitespace() => {
                if !word.is_empty() || any {
                    words.push(std::mem::take(&mut word));
                    any = false;
                }
            }
            None => word.push(c),
        }
    }
    if quote.is_some() {
        return None;
    }
    if !word.is_empty() || any {
        words.push(word);
    }
    Some(words)
}

/// The sibling tool of this name, or the bare name for `PATH` to
/// resolve.
///
/// The identity check is the point. macOS volumes are case-insensitive
/// by default, so a bundle whose executable is `AgentDocker` answers
/// `is_file()` for `agentdocker` — and the window then runs *itself*
/// with a CLI argument and reports "unknown argument: setup" from every
/// button that shells out. Comparing against our own path costs one
/// `canonicalize` and rules that out however the bundle is laid out.
fn beside(name: &str) -> std::path::PathBuf {
    let me = agentdocker_host::procinfo::executable_path().ok();
    let real = |path: &std::path::Path| path.canonicalize().ok();
    me.as_deref()
        .and_then(|me| me.parent())
        .map(|dir| dir.join(name))
        .filter(|sibling| sibling.is_file())
        .filter(|sibling| real(sibling) != me.as_deref().and_then(real))
        .unwrap_or_else(|| std::path::PathBuf::from(name))
}

/// Keep provider edits in the CLI; only its redacted plan/report crosses
/// into the window. Configuration snapshots stay in private setup receipts.
fn setup(args: &[String]) -> Result<serde_json::Value, String> {
    let cli = beside("agentdocker");
    let cli_arg = cli.to_str().ok_or("CLI path is not UTF-8")?;
    let mut argv = vec![cli_arg.to_owned(), "setup".into()];
    argv.extend_from_slice(args);
    argv.push("--json".into());
    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    let output = agentdocker_host::command::run(&cwd, &argv, Duration::from_secs(60))
        .map_err(|error| error.to_string())?;
    if !output.success {
        return Err(format!("Setup failed: {}", output.text.trim()));
    }
    serde_json::from_str(&output.stdout).map_err(|error| format!("Invalid setup reply: {error}"))
}

/// Copying and OS signature verification run separately from socket refreshes.
fn desktop(args: &[String]) -> Result<serde_json::Value, String> {
    desktop_with_timeout(args, Duration::from_secs(600))
}

fn check_update() -> Result<serde_json::Value, String> {
    desktop_with_timeout(
        &["update".into(), "--check".into()],
        Duration::from_secs(45),
    )
}

fn desktop_with_timeout(args: &[String], timeout: Duration) -> Result<serde_json::Value, String> {
    let cli = beside("agentdocker");
    let mut argv = vec![
        cli.to_str().ok_or("CLI path is not UTF-8")?.to_owned(),
        "desktop".into(),
    ];
    argv.extend_from_slice(args);
    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    let output =
        agentdocker_host::command::run(&cwd, &argv, timeout).map_err(|error| error.to_string())?;
    if !output.success {
        return Err(format!("Installation failed: {}", output.text.trim()));
    }
    serde_json::from_str(&output.stdout)
        .map_err(|error| format!("Invalid installation reply: {error}"))
}

fn spawn_events(client: Arc<Client>, tx: SyncSender<Msg>, ctx: Wake) {
    std::thread::spawn(move || {
        loop {
            let tx_events = tx.clone();
            let ctx_events = ctx.clone();
            let ready_tx = tx.clone();
            let ready_ctx = ctx.clone();
            let result = client.events(
                100,
                move || {
                    let _ = ready_tx.send(Msg::Connected);
                    ready_ctx.request_repaint();
                },
                move |event| {
                    let sent = tx_events.send(Msg::Event(Box::new(event))).is_ok();
                    ctx_events.request_repaint();
                    sent
                },
            );
            let reason = match result {
                Ok(()) => "event stream ended".to_owned(),
                Err(err) => err.to_string(),
            };
            if tx.send(Msg::Disconnected(reason)).is_err() {
                return;
            }
            ctx.request_repaint();
            std::thread::sleep(Duration::from_secs(2));
        }
    });
}

/// The label a tool is shown under, or its runtime id when the catalog
/// does not know it.
pub(crate) fn runtime_label(runtime: &str) -> String {
    agentdocker_core::runtime::spec(runtime)
        .map(|spec| spec.label.to_owned())
        .unwrap_or_else(|| runtime.to_owned())
}

/// How many cards the window keeps of one board: five pages. Past that
/// the board says so; archiving done cards is how it gets shorter.
pub(crate) const BOARD_KEEP: usize = 500;

/// The board on view: which project's, its cards as read so far —
/// pages appended as asked for — and whether the daemon has more.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Board {
    pub project: String,
    pub cards: Vec<agentdocker_core::Task>,
    pub more: bool,
    /// The page on its way, by ask; the control says so and asks for no
    /// other until it is answered, refused or superseded.
    pub pending_more: Option<u64>,
}

impl Board {
    pub fn loading_more(&self) -> bool {
        self.pending_more.is_some()
    }
}

/// A card being filed from the board.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct TaskDraft {
    pub title: String,
    pub acceptance: String,
    /// The filing on its way, by number; typing waits for its reply.
    pub sending: Option<u64>,
    pub error: Option<String>,
}

impl TaskDraft {
    pub fn sending(&self) -> bool {
        self.sending.is_some()
    }
}

const PAUSE_CONTROLS: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PauseAction {
    Pause,
    Resume,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PauseRequest {
    id: MessageId,
    project: String,
    action: PauseAction,
}

/// Drafts and in-flight operations remain with their project across navigation.
#[derive(Clone, Debug, Default)]
struct PauseControl {
    draft: Option<String>,
    pending: Option<PauseRequest>,
    error: Option<String>,
}

/// The one line under a browser extension's name: which browsers have
/// it, and that its sessions never appear here.
/// The one line under an in-browser runtime's name: where its extension
/// is, and whether its sessions reach here — through the connector when
/// one is serving, and what to do when none is.
pub(crate) fn in_browser_word(
    runtime: &agentdocker_core::runtime::RuntimeInfo,
    connected: usize,
    connector: Option<&agentdocker_host::connector::Serving>,
) -> String {
    let mut browsers: Vec<&str> = runtime
        .extensions
        .iter()
        .map(|e| e.browser.as_str())
        .collect();
    browsers.dedup();
    let installed = if browsers.is_empty() {
        "Not installed in a browser here".to_owned()
    } else {
        format!("Installed in {}", browsers.join(", "))
    };
    match (connected, connector) {
        (0, Some(_)) => format!(
            "{installed} · the connector is ready; add it in {} and its sessions appear here",
            surface_word(&runtime.name)
        ),
        (0, None) => {
            format!(
                "{installed} · its sessions reach here only through the connector, which is not running"
            )
        }
        (1, _) => format!("{installed} · one browser agent connected through the connector"),
        (n, _) => format!("{installed} · {n} browser agents connected through the connector"),
    }
}

/// The hosted surface an in-browser runtime is, as the person calls it.
fn surface_word(runtime: &str) -> &'static str {
    match runtime {
        "claude-browser" => "Claude",
        "chatgpt-browser" => "ChatGPT",
        _ => "the vendor's app",
    }
}

/// Where a vendor's settings take a connector's MCP URL.
pub(crate) fn add_connector_words(runtime: &str) -> &'static str {
    match runtime {
        "claude-browser" => {
            "Claude › Settings › Connectors › Add custom connector › paste the MCP URL › Connect"
        }
        "chatgpt-browser" => {
            "ChatGPT › Settings › Connectors › Advanced › Developer mode › Create › paste the MCP URL, OAuth"
        }
        _ => "in the vendor's connector settings, paste the MCP URL",
    }
}

/// "Chrome · Work · 1.0.93": the browser, the profile when there are
/// several, the version when the manifest says one.
pub(crate) fn extension_words(extension: &agentdocker_core::runtime::InstalledExtension) -> String {
    let mut parts = vec![extension.browser.clone()];
    parts.extend(extension.profile.clone());
    parts.extend(extension.version.clone());
    parts.join(" · ")
}

/// What "needs setup" is missing, for the Tools row: the MCP entry, the
/// hooks, or the one or two hook events a release began to require.
pub(crate) fn missing_setup(runtime: &agentdocker_core::runtime::RuntimeInfo) -> String {
    use agentdocker_core::runtime::Wiring;
    let mut parts = Vec::new();
    if runtime.mcp == Wiring::Missing {
        parts.push("MCP entry".to_owned());
    }
    if runtime.hooks == Wiring::Missing {
        let missing = &runtime.hooks_missing;
        let required = agentdocker_host::runtimes::hook_events(&runtime.name).len();
        parts.push(match missing.len() {
            0 => "hooks".to_owned(),
            n if n == required => "hooks".to_owned(),
            1 => format!("{} hook", missing[0]),
            2 => format!("{} and {} hooks", missing[0], missing[1]),
            n => format!("{} and {} more hooks", missing[0], n - 1),
        });
    }
    match parts.as_slice() {
        [] => "Needs setup".to_owned(),
        [one] => format!("Needs setup · missing {one}"),
        [a, b] => format!("Needs setup · missing {a} and {b}"),
        _ => "Needs setup".to_owned(),
    }
}

/// Which kind of conversation the person is starting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NewKind {
    Direct,
    Channel,
}

/// The form for a conversation the person is starting: a direct message
/// is one pick, a channel is a name, what it is for and who is in it.
#[derive(Clone, Debug)]
pub(crate) struct NewConversation {
    pub request: MessageId,
    pub invite: Option<String>,
    pub kind: NewKind,
    pub name: String,
    pub purpose: String,
    pub members: BTreeSet<agentdocker_core::AgentId>,
    pub creating: bool,
    pub error: Option<String>,
}

impl NewConversation {
    pub(crate) fn new() -> Self {
        Self {
            request: MessageId::generate(),
            invite: None,
            kind: NewKind::Direct,
            name: String::new(),
            purpose: String::new(),
            members: BTreeSet::new(),
            creating: false,
            error: None,
        }
    }
}

/// A notification's message being looked for in its conversation's
/// archive: the conversation, the message, how many pages back have
/// been asked for, and the seq the last page was asked before — so a
/// refresh of the newest page is not taken for the answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Seek {
    pub conversation: String,
    pub message: MessageId,
    pub pages: usize,
    pub before: Option<u64>,
    pub refresh_pending: bool,
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The Tools row says what setup is missing: the one hook event a
    /// release began to require, the MCP entry, or both — and only
    /// "hooks" when none of them is wired.
    /// A browser extension's row says which browsers have it and that no
    /// session of it can appear here; its details name each profile.
    #[test]
    fn the_tools_row_for_a_browser_extension_says_its_sessions_are_elsewhere() {
        use agentdocker_core::runtime::{InstalledExtension, RuntimeInfo, Wiring};
        let extension = |browser: &str, profile: Option<&str>| InstalledExtension {
            label: "Claude".into(),
            browser: browser.into(),
            profile: profile.map(str::to_owned),
            version: Some("1.0.93".into()),
            bridge: None,
        };
        let runtime = RuntimeInfo {
            name: "claude-browser".into(),
            vendor: "Anthropic".into(),
            label: "Claude (browser extension)".into(),
            cli: None,
            version: None,
            apps: vec![],
            extensions: vec![
                extension("Chrome", Some("Person 1")),
                extension("Chrome", Some("Work")),
                extension("Brave", None),
            ],
            incomplete: vec![],
            config_dir: None,
            mcp: Wiring::Unsupported,
            hooks: Wiring::Unsupported,
            hooks_missing: vec![],
            shell: Wiring::Unsupported,
            running: 0,
        };
        assert!(runtime.installed() && runtime.in_browser());
        let serving = agentdocker_host::connector::Serving {
            pid: 1,
            public_url: "https://node.example.ts.net".into(),
            bind: "127.0.0.1:1".into(),
            default_project: None,
            pairing_code: "ABCD-EFGH".into(),
            started_at: Utc::now(),
            tunnel: None,
            allowlist_prefixes: 0,
        };
        assert_eq!(
            in_browser_word(&runtime, 0, None),
            "Installed in Chrome, Brave · its sessions reach here only through the connector, which is not running"
        );
        assert_eq!(
            in_browser_word(&runtime, 0, Some(&serving)),
            "Installed in Chrome, Brave · the connector is ready; add it in Claude and its sessions appear here"
        );
        assert_eq!(
            in_browser_word(&runtime, 1, Some(&serving)),
            "Installed in Chrome, Brave · one browser agent connected through the connector"
        );
        assert_eq!(
            in_browser_word(&runtime, 2, None),
            "Installed in Chrome, Brave · 2 browser agents connected through the connector"
        );
        assert!(add_connector_words("claude-browser").starts_with("Claude › Settings"));
        assert!(add_connector_words("chatgpt-browser").starts_with("ChatGPT › Settings"));
        assert_eq!(
            extension_words(&runtime.extensions[1]),
            "Chrome · Work · 1.0.93"
        );
        assert_eq!(extension_words(&runtime.extensions[2]), "Brave · 1.0.93");
    }

    #[test]
    fn the_tools_row_says_which_hook_is_missing() {
        use agentdocker_core::runtime::{RuntimeInfo, Wiring};
        let mut runtime = RuntimeInfo {
            name: "claude-code".into(),
            vendor: "Anthropic".into(),
            label: "Claude Code".into(),
            cli: Some("/opt/claude".into()),
            version: None,
            apps: vec![],
            extensions: vec![],
            incomplete: vec![],
            config_dir: None,
            mcp: Wiring::Wired,
            hooks: Wiring::Missing,
            hooks_missing: vec!["StopFailure".into()],
            shell: Wiring::Unsupported,
            running: 0,
        };
        assert_eq!(
            missing_setup(&runtime),
            "Needs setup · missing StopFailure hook"
        );
        runtime.hooks_missing = vec!["Stop".into(), "StopFailure".into()];
        assert_eq!(
            missing_setup(&runtime),
            "Needs setup · missing Stop and StopFailure hooks"
        );
        runtime.hooks_missing = vec!["Stop".into(), "StopFailure".into(), "SessionEnd".into()];
        assert_eq!(
            missing_setup(&runtime),
            "Needs setup · missing Stop and 2 more hooks"
        );
        runtime.hooks_missing = agentdocker_host::runtimes::hook_events("claude-code")
            .iter()
            .map(|(e, _)| e.to_string())
            .collect();
        runtime.mcp = Wiring::Missing;
        assert_eq!(
            missing_setup(&runtime),
            "Needs setup · missing MCP entry and hooks"
        );
        runtime.hooks = Wiring::Wired;
        runtime.hooks_missing.clear();
        assert_eq!(missing_setup(&runtime), "Needs setup · missing MCP entry");
    }

    #[test]
    fn channel_creation_replies_belong_to_the_submitted_form() {
        let (commands, _requests) = queue::channel();
        let (messages, results) = sync_channel(MESSAGE_CAPACITY);
        let mut app = App::bare(commands, results);
        let first = NewConversation::new();
        let mut second = NewConversation::new();
        second.creating = true;
        second.name = "second".into();
        let expected = second.request.clone();
        app.new_conversation = Some(second);
        for result in [Ok("first-room".into()), Err("first failure".into())] {
            messages
                .send(Msg::ChannelOpened(first.request.clone(), result))
                .unwrap();
            app.drain();
            let current = app.new_conversation.as_ref().unwrap();
            assert_eq!(current.request, expected);
            assert_eq!(current.name, "second");
            assert!(current.creating);
            assert!(current.error.is_none());
            assert!(app.shell.conversation.is_none());
        }
        messages
            .send(Msg::ChannelOpened(expected, Err("second failure".into())))
            .unwrap();
        app.drain();
        let current = app.new_conversation.as_ref().unwrap();
        assert!(!current.creating);
        assert_eq!(current.error.as_deref(), Some("second failure"));
    }

    #[test]
    fn channel_invitation_replies_belong_to_the_submitted_form() {
        let (commands, _requests) = queue::channel();
        let (messages, results) = sync_channel(MESSAGE_CAPACITY);
        let mut app = App::bare(commands, results);
        let first = NewConversation::new();
        let mut second = NewConversation::new();
        second.invite = Some("second-room".into());
        second.creating = true;
        second.name = "second".into();
        second.members.insert("existing-member".into());
        let expected = second.request.clone();
        app.new_conversation = Some(second);
        let channel = agentdocker_core::Channel {
            id: "first-room".into(),
            project: "project".into(),
            name: Some("first".into()),
            subject: agentdocker_core::ChannelSubject::Task {
                task: "first".into(),
            },
            members: vec!["new-member".into()],
            opened_by: None,
            opened_at: chrono::Utc::now(),
            reviews: vec![],
            closed_at: None,
            resolution: None,
        };
        for result in [Ok(channel), Err("first failure".into())] {
            messages
                .send(Msg::ChannelInvited(
                    first.request.clone(),
                    "new-member".into(),
                    result,
                ))
                .unwrap();
            app.drain();
            let current = app.new_conversation.as_ref().unwrap();
            assert_eq!(current.request, expected);
            assert_eq!(current.name, "second");
            assert_eq!(current.invite.as_deref(), Some("second-room"));
            assert_eq!(current.members, BTreeSet::from(["existing-member".into()]));
            assert!(current.creating);
            assert!(current.error.is_none());
            assert!(app.channels.is_empty());
        }
        messages
            .send(Msg::ChannelInvited(
                expected,
                "new-member".into(),
                Err("second failure".into()),
            ))
            .unwrap();
        app.drain();
        let current = app.new_conversation.as_ref().unwrap();
        assert!(!current.creating);
        assert_eq!(current.error.as_deref(), Some("second failure"));
    }

    #[test]
    fn text_zoom_reflows_messages_without_a_window_resize() {
        let (commands, _requests) = queue::channel();
        let (_messages, results) = sync_channel(MESSAGE_CAPACITY);
        let mut app = App::bare(commands, results);
        app.shell.width = 1180.0;
        app.shell.thread = Some("thread-fixture".to_owned().into());
        let _ = app.update(Message::TextSize(14.0));
        let preferred = app.panes.widths;
        assert!(!app.messages_compact());
        let _ = app.update(Message::TextSize(18.0));
        assert!(
            !app.narrow(),
            "exercise the wide shell with a constrained workspace"
        );
        assert!(
            app.messages_compact(),
            "zoom alone must prevent collapsed columns"
        );
        let _ = app.update(Message::TextSize(14.0));
        assert!(!app.messages_compact());
        assert_eq!(app.panes.widths, preferred);
    }

    /// The window looks at the conversation; it does not consume it.
    ///
    /// Channel messages reach the person at the keyboard because they
    /// are an agent like any other, and their inbox is where those
    /// messages sit. Reading it with `drain: true` would delete the
    /// conversation out from under whoever is reading the screen — and
    /// this screen polls, so it would do it every couple of seconds.
    #[test]
    fn the_conversation_is_read_without_being_taken_and_kept_in_its_room() {
        assert!(
            matches!(inbox_request(), Request::Inbox { drain: false, .. }),
            "a window that polls must never drain what it shows"
        );

        let inbox: Vec<agentdocker_core::Envelope> = serde_json::from_value(serde_json::json!([
            {"id": "m1", "from": "a", "to": {"kind": "channel", "value": "room-one"},
             "kind": "chat", "payload": "first",
             "sent_at": "2026-09-08T03:00:00Z"},
            {"id": "m2", "from": "b", "to": {"kind": "channel", "value": "room-two"},
             "kind": "chat", "payload": {"channel": "wrong-room", "text": "elsewhere"},
             "sent_at": "2026-09-08T03:01:00Z"},
            {"id": "m3", "from": "a", "to": {"kind": "channel", "value": "room-one"},
             "kind": "chat", "payload": {"channel": "room-one", "text": "second"},
             "sent_at": "2026-09-08T03:02:00Z"},
            {"id": "m4", "from": "b", "to": {"kind": "agent", "value": "user"},
             "kind": "chat", "payload": {"channel": "room-one", "text": "just to you"},
             "sent_at": "2026-09-08T03:03:00Z"}
        ]))
        .unwrap();
        let (said, direct) = by_room(inbox.iter().chain(std::iter::once(&inbox[0])));
        assert_eq!(said["room-one"].len(), 2, "kept together and in order");
        assert_eq!(said["room-one"][0].id.as_str(), "m1");
        assert_eq!(said["room-two"].len(), 1);
        // A message with no room is not filed under whichever room
        // happens to sort first.
        assert_eq!(direct.len(), 1);
        assert_eq!(direct[0].id.as_str(), "m4");
    }

    #[test]
    fn confirmed_channel_messages_survive_refresh_and_failures_never_look_sent() {
        let (commands, _requests) = queue::channel();
        let (messages, results) = sync_channel(MESSAGE_CAPACITY);
        let mut app = App::bare(commands, results);
        let draft = app.shell.channel_drafts.entry("room".into()).or_default();
        draft.text = "keep my draft".into();
        draft.begin();
        messages
            .send(Msg::ChannelSent("room".into(), Err("offline".into())))
            .unwrap();
        app.drain();
        assert!(app.sent_channels.is_empty());
        assert_eq!(app.shell.channel_drafts["room"].text, "keep my draft");
        for index in 0..SENT_CHANNEL_LIMIT + 1 {
            let draft = app.shell.channel_drafts.get_mut("room").unwrap();
            draft.text = "x".repeat(8192);
            draft.begin();
            messages
                .send(Msg::ChannelSent(
                    "room".into(),
                    Ok(MessageId::from(format!("sent-{index}")).into()),
                ))
                .unwrap();
            app.drain();
        }
        let retained = app.sent_channels.len();
        assert!(retained > 0 && retained <= SENT_CHANNEL_LIMIT);
        assert!(
            app.sent_channels
                .iter()
                .map(|item| item.payload.as_str().unwrap().len())
                .sum::<usize>()
                <= SENT_CHANNEL_BYTES
        );
        messages.send(Msg::Inbox(Vec::new())).unwrap();
        app.drain();
        assert_eq!(
            app.sent_channels.len(),
            retained,
            "an inbox refresh erased confirmed sends"
        );
        assert_eq!(
            app.sent_channels.back().unwrap().id.as_str(),
            format!("sent-{SENT_CHANNEL_LIMIT}")
        );
    }

    #[test]
    fn channel_request_uses_explicit_project_and_no_membership_filter() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("fixture.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let server = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(3);
            let stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "missing channel request");
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("fixture accept: {error}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let request: serde_json::Value = serde_json::from_str(&line).unwrap();
            assert_eq!(request["op"], "channels");
            assert_eq!(request["project"], "fixture-project");
            assert_eq!(request["all"], false);
            assert!(request["agent"].is_null());
            reader
                .get_mut()
                .write_all(b"{\"type\":\"channels\",\"channels\":[]}\n")
                .unwrap();
        });
        let response = run(
            &Client::isolated(socket),
            Cmd::Channels("fixture-project".into(), "fixture-project".into()),
        );
        server.join().unwrap();
        assert!(
            matches!(response.unwrap(), Some(Msg::Channels(project, channels))
            if project == "fixture-project" && channels.is_empty())
        );
    }

    /// The status line stops being news.
    #[test]
    fn the_status_line_lets_go_of_what_is_no_longer_news() {
        let (tx, _requests) = queue::channel();
        let (messages, rx) = sync_channel::<Msg>(MESSAGE_CAPACITY);
        let mut app = App::bare(tx, rx);
        messages
            .send(Msg::Status("half the fleet is on fire".to_owned()))
            .unwrap();
        app.drain();
        assert_eq!(app.status, "half the fleet is on fire");
        // Still true, perhaps, but no longer what just happened — and a
        // line that never expires is a line the eye stops reading.
        app.status_at = Instant::now()
            .checked_sub(STATUS_FOR + Duration::from_secs(1))
            .expect("the machine has been up longer than the status timeout");
        app.drain();
        assert!(app.status.is_empty());
    }

    #[test]
    fn unexpected_ack_response_retains_the_message_and_its_draft() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;
        let tmp = tempfile::tempdir().unwrap();
        let socket = tmp.path().join("fixture.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let server = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(3);
            let stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "missing acknowledgement request");
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(e) => panic!("{e}"),
                }
            };
            // Darwin inherits the listener's O_NONBLOCK flag. The request
            // reader must wait for bytes using the bounded socket deadline.
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&line).unwrap()["op"],
                "ack_inbox"
            );
            reader
                .get_mut()
                .write_all(b"{\"type\":\"messages\",\"messages\":[]}\n")
                .unwrap();
        });
        let id = MessageId::from("retained".to_owned());
        let error = run(
            &Client::isolated(socket),
            Cmd::DismissMessages(vec![id.clone()]),
        )
        .err()
        .expect("unexpected success response must fail");
        server.join().unwrap();
        let (commands, _) = queue::channel();
        let (messages, results) = sync_channel(MESSAGE_CAPACITY);
        let mut app = App::bare(commands, results);
        let mut envelope = agentdocker_core::Envelope::new(
            "peer",
            agentdocker_core::Destination::Agent("user".into()),
            "chat",
            serde_json::json!("message"),
            None,
            Utc::now(),
        );
        envelope.id = id.clone();
        app.inbox.push(envelope.clone());
        app.shell.answers.insert(id.clone(), "unfinished".into());
        app.dismissing.insert(id.clone());
        messages
            .send(Msg::MessagesDismissed(
                vec![id.clone()],
                Err(error.to_string()),
            ))
            .unwrap();
        app.drain();
        assert_eq!(app.inbox, vec![envelope]);
        assert_eq!(app.shell.answers[&id], "unfinished");
        assert!(app.dismissing.is_empty());
    }

    #[test]
    fn message_dismissal_waits_for_success_and_preserves_drafts_and_new_arrivals() {
        let (commands, requests) = queue::channel();
        let (messages, results) = sync_channel(MESSAGE_CAPACITY);
        let mut app = App::bare(commands, results);
        let first = agentdocker_core::Envelope::new(
            "peer",
            agentdocker_core::Destination::Agent("user".into()),
            "chat",
            serde_json::json!("first"),
            None,
            Utc::now(),
        );
        let second = agentdocker_core::Envelope::new(
            "peer",
            agentdocker_core::Destination::Agent("user".into()),
            "chat",
            serde_json::json!("second"),
            None,
            Utc::now(),
        );
        app.inbox.push(first.clone());
        let question = MessageId::from("pending-question".to_owned());
        app.shell
            .answers
            .insert(question.clone(), "unfinished answer".into());
        let _ = app.update(shell::Message::DismissInbox(vec![first.id.clone()]));
        let _ = app.update(shell::Message::DismissInbox(vec![first.id.clone()]));
        assert_eq!(
            requests
                .try_iter()
                .filter(|command| matches!(command, Cmd::DismissMessages(_)))
                .count(),
            1
        );
        assert_eq!(app.inbox.as_slice(), std::slice::from_ref(&first));
        messages
            .send(Msg::MessagesDismissed(
                vec![first.id.clone()],
                Err("inbox retained".into()),
            ))
            .unwrap();
        app.drain();
        assert_eq!(app.inbox.as_slice(), std::slice::from_ref(&first));
        assert!(!app.dismissing.contains(&first.id));
        assert_eq!(app.status, "inbox retained");
        let _ = app.update(shell::Message::DismissInbox(vec![first.id.clone()]));
        app.inbox.push(second.clone());
        messages
            .send(Msg::MessagesDismissed(vec![first.id.clone()], Ok(())))
            .unwrap();
        app.drain();
        assert_eq!(app.inbox, [second]);
        assert_eq!(app.shell.answers[&question], "unfinished answer");
        assert!(!app.dismissing.contains(&first.id));
    }

    #[test]
    fn bulk_dismissal_is_one_receipt_and_excludes_questions_and_unshown_messages() {
        let (commands, requests) = queue::channel();
        let (messages, results) = sync_channel(MESSAGE_CAPACITY);
        let mut app = App::bare(commands, results);
        let received: Vec<_> = (0..5)
            .map(|index| {
                agentdocker_core::Envelope::new(
                    "peer",
                    agentdocker_core::Destination::Agent("user".into()),
                    "chat",
                    serde_json::json!(index),
                    None,
                    Utc::now(),
                )
            })
            .collect();
        app.inbox = received[..4].to_vec();
        let question = received[3].id.clone();
        app.questions.push(Question {
            presentation: None,
            id: question.clone(),
            from: "peer".into(),
            to: agentdocker_core::Destination::Agent("user".into()),
            text: "Keep this question".into(),
            asked_at: Utc::now(),
            expires_at: Utc::now() + chrono::Duration::minutes(1),
        });
        app.shell
            .answers
            .insert(question.clone(), "unfinished answer".into());
        let _ = app.update(shell::Message::DismissInbox(vec![
            received[1].id.clone(),
            received[2].id.clone(),
            received[1].id.clone(),
            question.clone(),
            MessageId::from("unknown".to_owned()),
        ]));
        let receipts: Vec<_> = requests
            .try_iter()
            .filter_map(|command| match command {
                Cmd::DismissMessages(ids) => Some(ids),
                _ => None,
            })
            .collect();
        assert_eq!(receipts.len(), 1);
        let ids = &receipts[0];
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&received[1].id) && ids.contains(&received[2].id));
        assert_eq!(app.inbox.len(), 4, "wait for the durable receipt");
        app.inbox.push(received[4].clone());
        messages
            .send(Msg::MessagesDismissed(ids.clone(), Ok(())))
            .unwrap();
        app.drain();
        assert_eq!(
            app.inbox,
            [
                received[0].clone(),
                received[3].clone(),
                received[4].clone()
            ]
        );
        assert_eq!(app.shell.answers[&question], "unfinished answer");
        assert!(app.dismissing.is_empty());
    }

    #[test]
    fn a_full_command_queue_preserves_drafts_and_releases_busy_controls() {
        let (commands, requests) = queue::channel();
        for _ in 0..queue::CAPACITY {
            commands.send(Cmd::Stop("fixture".into())).unwrap();
        }
        let (_messages, results) = sync_channel(MESSAGE_CAPACITY);
        let mut app = App::bare(commands, results);
        let id = MessageId::from("fixture-question".to_owned());
        app.shell.answers.insert(id.clone(), "draft".into());
        app.sending.insert(id.clone());
        app.send(Cmd::Answer(id.clone(), "draft".into()));
        assert_eq!(app.shell.answers[&id], "draft");
        assert!(!app.sending.contains(&id));
        assert!(app.status.contains("queue is full"));
        app.dismissing.insert(id.clone());
        let second = MessageId::from("second-message".to_owned());
        app.dismissing.insert(second.clone());
        app.send(Cmd::DismissMessages(vec![id.clone(), second]));
        assert!(app.dismissing.is_empty());
        assert!(app.status.contains("queue is full"));
        app.setup_busy = true;
        app.send(Cmd::Setup(vec!["--health".into()]));
        assert!(!app.setup_busy);
        app.console_running = 1;
        app.send(Cmd::Console("ps".into(), None));
        assert_eq!(app.console_running, 0);
        assert!(app.console_output.contains("queue is full"));
        app.drain();
        assert!(app.status.contains("queue is full"));
        app.send(Cmd::Agents); // Rejected refresh must release its coalescing key.
        assert_eq!(requests.try_iter().count(), queue::CAPACITY);
        app.send(Cmd::Agents);
        assert!(matches!(requests.try_iter().next(), Some(Cmd::Agents)));
    }

    #[test]
    fn a_closed_command_worker_does_not_leave_an_answer_in_flight() {
        let (commands, requests) = queue::channel();
        drop(requests);
        let (_messages, results) = sync_channel(MESSAGE_CAPACITY);
        let mut app = App::bare(commands, results);
        let id = MessageId::from("fixture-question".to_owned());
        app.shell.answers.insert(id.clone(), "draft".into());
        app.sending.insert(id.clone());
        app.send(Cmd::Answer(id.clone(), "draft".into()));
        assert_eq!(app.shell.answers[&id], "draft");
        assert!(!app.sending.contains(&id));
        assert!(app.status.contains("worker stopped"));
    }

    #[test]
    fn command_recall_keeps_complete_recent_commands_with_count_and_byte_limits() {
        let (commands, _requests) = queue::channel();
        let (_messages, results) = sync_channel(MESSAGE_CAPACITY);
        let mut app = App::bare(commands, results);
        for number in 0..150 {
            app.remember_console_command(&format!("fixture {number}"));
        }
        assert_eq!(app.console_history.len(), CONSOLE_HISTORY_COMMANDS);
        app.recall(true);
        assert_eq!(app.console_input, "fixture 149");
        for _ in 0..150 {
            app.recall(true);
        }
        assert_eq!(app.console_input, "fixture 50");
        app.remember_console_command(&"x".repeat(CONSOLE_HISTORY_BYTES + 1));
        app.recall(true);
        assert_eq!(
            app.console_input, "fixture 149",
            "oversized commands are never recalled in truncated form"
        );
        let mut latest = String::new();
        for number in 0..100 {
            latest = format!("{} {number}", "é".repeat(1000));
            app.remember_console_command(&latest);
        }
        assert!(
            app.console_history.iter().map(String::len).sum::<usize>() <= CONSOLE_HISTORY_BYTES
        );
        app.recall(true);
        assert_eq!(app.console_input, latest);
    }

    #[test]
    fn event_burst_coalesces_refresh_work_while_the_daemon_is_busy() {
        let (tx, requests) = queue::channel();
        let (_messages, results) = sync_channel::<Msg>(MESSAGE_CAPACITY);
        let mut app = App::bare(tx, results);
        for _ in 0..10_000 {
            app.on_event(Event::new(
                EventKind::AgentRemoved {
                    agent: "fixture".into(),
                },
                Utc::now(),
            ));
        }
        let pending: Vec<_> = requests.try_iter().collect();
        assert_eq!(
            pending.len(),
            1,
            "one snapshot covers a burst of changes while queued"
        );
        assert!(matches!(pending[0], Cmd::Agents));
        app.on_event(Event::new(
            EventKind::AgentRemoved {
                agent: "fixture".into(),
            },
            Utc::now(),
        ));
        assert_eq!(
            requests.try_iter().count(),
            1,
            "a change after dispatch still refreshes"
        );
    }

    #[test]
    fn channel_and_inbox_refreshes_coalesce_without_losing_other_projects() {
        let (commands, requests) = queue::channel();
        for _ in 0..10_000 {
            commands
                .send(Cmd::Channels("project-a".into(), "project-a".into()))
                .unwrap();
            commands
                .send(Cmd::Channels("project-b".into(), "project-b".into()))
                .unwrap();
            commands.send(Cmd::Inbox).unwrap();
        }
        let pending: Vec<_> = requests.try_iter().collect();
        assert_eq!(pending.len(), 3);
        assert!(matches!(&pending[0], Cmd::Channels(project, _) if project == "project-a"));
        assert!(matches!(&pending[1], Cmd::Channels(project, _) if project == "project-b"));
        assert!(matches!(&pending[2], Cmd::Inbox));
        commands
            .send(Cmd::Channels("project-a".into(), "project-a".into()))
            .unwrap();
        commands.send(Cmd::Inbox).unwrap();
        assert_eq!(requests.try_iter().count(), 2);
    }

    #[test]
    fn channel_snapshots_keep_other_projects_and_refuse_departed_projects() {
        use agentdocker_core::channel::{Channel, ChannelSubject};
        use agentdocker_core::{AgentSpec, ChannelId, ProjectId};
        let (commands, requests) = queue::channel();
        let (messages, results) = sync_channel(MESSAGE_CAPACITY);
        let mut app = App::bare(commands, results);
        let mut agents = Vec::new();
        for name in ["project-a", "project-a", "project-b"] {
            let mut agent = AgentRecord::new(
                AgentSpec {
                    name: name.into(),
                    ..Default::default()
                },
                false,
                Utc::now(),
            );
            let mut project = ProjectRef::directory(format!("/fixture/{name}"));
            project.fingerprint = Some(name.into());
            agent.project = Some(project);
            agents.push(agent);
        }
        messages
            .send(Msg::Agents(agents.clone(), BTreeMap::new()))
            .unwrap();
        app.drain();
        assert_eq!(
            requests
                .try_iter()
                .filter(|cmd| matches!(cmd, Cmd::Channels(_, _)))
                .count(),
            2
        );
        let channel = |project: &str, id: &str| Channel {
            id: ChannelId::from(id),
            project: ProjectId::from(project),
            name: None,
            subject: ChannelSubject::Task {
                task: "fixture".into(),
            },
            members: Vec::new(),
            opened_by: None,
            opened_at: Utc::now(),
            closed_at: None,
            resolution: None,
            reviews: Vec::new(),
        };
        messages
            .send(Msg::Channels(
                "project-a".into(),
                vec![channel("project-a", "a")],
            ))
            .unwrap();
        messages
            .send(Msg::Channels(
                "project-b".into(),
                vec![channel("project-b", "b")],
            ))
            .unwrap();
        app.drain();
        assert_eq!(app.channels.len(), 2);
        messages
            .send(Msg::Channels("project-a".into(), Vec::new()))
            .unwrap();
        app.drain();
        assert_eq!(app.channels.len(), 1);
        assert_eq!(app.channels[0].project.as_str(), "project-b");
        agents.retain(|agent| agent.project.as_ref().unwrap().id().as_str() == "project-a");
        messages.send(Msg::Agents(agents, BTreeMap::new())).unwrap();
        messages
            .send(Msg::Channels(
                "project-b".into(),
                vec![channel("project-b", "stale")],
            ))
            .unwrap();
        app.drain();
        assert!(app.channels.is_empty());
    }

    fn journal_entry(seq: u64) -> JournalEntry {
        serde_json::from_value(serde_json::json!({
            "project":"fixture-project", "seq":seq, "at":Utc::now(),
            "agent_name":"fixture", "kind":"note", "summary":format!("entry {seq}"),
            "summary_source":"explicit"
        }))
        .unwrap()
    }

    #[test]
    fn history_live_journal_is_bounded_and_late_snapshot_keeps_newer_entries() {
        let (commands, _requests) = queue::channel();
        let (messages, results) = sync_channel(MESSAGE_CAPACITY);
        let mut app = App::bare(commands, results);
        app.journal_project = Some("fixture-project".into());
        for seq in 1..=1000 {
            app.on_event(Event::new(
                EventKind::JournalAppended {
                    entry: journal_entry(seq),
                },
                Utc::now(),
            ));
        }
        assert_eq!(
            app.journal.len(),
            200,
            "live updates must respect the snapshot window"
        );
        assert_eq!(app.journal.first().unwrap().seq, 801);
        // The RPC snapshot can have been read before the most recent events.
        messages
            .send(Msg::Journal(
                "fixture-project".into(),
                Some(900),
                (701..=900).map(journal_entry).collect(),
            ))
            .unwrap();
        app.drain();
        assert_eq!(app.journal.first().unwrap().seq, 801);
        assert_eq!(app.journal.last().unwrap().seq, 1000);
        assert_eq!(app.journal.len(), 200);
    }

    #[test]
    fn history_late_journal_snapshot_does_not_replace_newer_live_entries() {
        let (commands, _requests) = queue::channel();
        let (messages, results) = sync_channel(MESSAGE_CAPACITY);
        let mut app = App::bare(commands, results);
        app.journal_project = Some("fixture-project".into());
        app.journal = (801..=1000).map(journal_entry).collect();
        messages
            .send(Msg::Journal(
                "fixture-project".into(),
                Some(900),
                (701..=900).map(journal_entry).collect(),
            ))
            .unwrap();
        app.drain();
        assert_eq!(app.journal.last().unwrap().seq, 1000);
        assert_eq!(app.journal.len(), 200);
    }

    #[test]
    fn history_empty_snapshot_respects_pruning_and_legacy_responses() {
        let (commands, _requests) = queue::channel();
        let (messages, results) = sync_channel(MESSAGE_CAPACITY);
        let mut app = App::bare(commands, results);
        app.journal_project = Some("fixture-project".into());
        app.journal = (801..=1001).map(journal_entry).collect();
        messages
            .send(Msg::Journal("fixture-project".into(), Some(1000), vec![]))
            .unwrap();
        app.drain();
        assert_eq!(
            app.journal
                .iter()
                .map(|entry| entry.seq)
                .collect::<Vec<_>>(),
            [1001]
        );
        messages
            .send(Msg::Journal("fixture-project".into(), None, vec![]))
            .unwrap();
        app.drain();
        assert!(
            app.journal.is_empty(),
            "do not invent a head for an older daemon"
        );
    }

    #[test]
    fn history_console_keeps_a_bounded_utf8_tail_across_large_and_repeated_replies() {
        let (commands, _requests) = queue::channel();
        let (messages, results) = sync_channel(MESSAGE_CAPACITY);
        let mut app = App::bare(commands, results);
        messages
            .send(Msg::Console(format!("{}\nLATEST", "🐋".repeat(200_000))))
            .unwrap();
        app.drain();
        assert!(app.console_output.len() <= 256 * 1024);
        assert!(
            app.console_output.capacity() <= 512 * 1024,
            "trimming must also bound retained allocation"
        );
        assert!(app.console_output.ends_with("\nLATEST\n"));
        for _ in 0..20 {
            messages.send(Msg::Console("é".repeat(20_000))).unwrap();
            app.drain();
        }
        assert!(app.console_output.len() <= 256 * 1024);
        assert!(app.console_output.capacity() <= 512 * 1024);
        assert!(app.console_output.ends_with("é\n"));
    }

    #[cfg(unix)]
    fn command_with_replies(
        cmd: Cmd,
        replies: Vec<(serde_json::Value, Option<serde_json::Value>)>,
    ) -> Vec<Msg> {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("fixture.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let server = std::thread::spawn(move || {
            for (expected, reply) in replies {
                let deadline = Instant::now() + Duration::from_secs(3);
                let stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(
                                Instant::now() < deadline,
                                "missing expected fixture request"
                            );
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("fixture accept failed: {error}"),
                    }
                };
                // Darwin inherits O_NONBLOCK from the listener. The accept
                // loop is polled, but this fixture's request reader uses the
                // socket deadlines below and must wait for the request bytes.
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let request: serde_json::Value = serde_json::from_str(&line).unwrap();
                for (key, value) in expected.as_object().unwrap() {
                    assert_eq!(request[key], *value);
                }
                if let Some(reply) = reply {
                    serde_json::to_writer(reader.get_mut(), &reply).unwrap();
                    reader.get_mut().write_all(b"\n").unwrap();
                }
            }
            listener
        });
        let (commands, requests) = queue::channel();
        let (messages, results) = sync_channel(MESSAGE_CAPACITY);
        let worker = spawn_worker(
            Arc::new(Client::isolated(socket)),
            requests,
            messages,
            Wake::default(),
            Default::default(),
        );
        commands.send(cmd).unwrap();
        drop(commands);
        worker.join().unwrap();
        let listener = server.join().unwrap();
        assert!(
            matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
            "worker sent an unexpected extra request after the planned replies"
        );
        results.try_iter().collect()
    }

    #[cfg(unix)]
    #[test]
    fn channel_requests_name_a_project_without_filtering_to_human_membership() {
        use serde_json::json;
        let replies = command_with_replies(
            Cmd::Channels("fixture-project".into(), "fixture-project".into()),
            vec![(
                json!({"op":"channels", "project":"fixture-project", "all":false, "agent":null}),
                Some(json!({"type":"channels", "channels":[]})),
            )],
        );
        assert!(replies.iter().any(|reply| matches!(reply,
            Msg::Channels(project, channels) if project == "fixture-project" && channels.is_empty())));
    }

    #[cfg(unix)]
    #[test]
    fn rejected_answer_preserves_draft_and_reports_a_working_connection() {
        use serde_json::json;
        let id = MessageId::from("fixture-question".to_owned());
        let replies = command_with_replies(
            Cmd::Answer(id.clone(), "draft".into()),
            vec![(
                json!({"op":"answer", "message":id, "text":"draft"}),
                Some(json!({"type":"error", "code":"not_found", "message":"question expired"})),
            )],
        );
        assert!(
            replies
                .iter()
                .any(|message| matches!(message, Msg::Connected))
        );
        assert!(
            !replies
                .iter()
                .any(|message| matches!(message, Msg::Disconnected(_)))
        );
        let (commands, _requests) = queue::channel();
        let (messages, results) = sync_channel(MESSAGE_CAPACITY);
        let mut app = App::bare(commands, results);
        app.shell.answers.insert(id.clone(), "draft".into());
        app.sending.insert(id.clone());
        for message in replies {
            messages.send(message).unwrap();
        }
        app.drain();
        assert_eq!(app.shell.answers[&id], "draft");
        assert!(!app.sending.contains(&id));
        assert!(app.connected.is_ok());
        assert!(app.status.contains("question expired"));
    }

    #[cfg(unix)]
    #[test]
    fn bulk_adoption_preserves_partial_progress_and_distinguishes_remote_failure() {
        use serde_json::json;
        for reply in [
            None,
            Some(json!({"type":"error", "code":"not_found", "message":"process exited"})),
        ] {
            let transport_failed = reply.is_none();
            let messages = command_with_replies(
                Cmd::AdoptAll,
                vec![
                    (
                        json!({"op":"discover"}),
                        Some(json!({"type":"processes", "processes": [
                            {"pid":101,"ppid":1,"runtime":"codex","command":"fixture"},
                            {"pid":102,"ppid":1,"runtime":"codex","command":"fixture"},
                            {"pid":103,"ppid":1,"runtime":"codex","command":"fixture"}
                        ]})),
                    ),
                    (json!({"op":"adopt", "pid":101}), Some(json!({"type":"ok"}))),
                    (json!({"op":"adopt", "pid":102}), reply),
                ],
            );
            assert_eq!(
                messages
                    .iter()
                    .any(|message| matches!(message, Msg::Disconnected(_))),
                transport_failed
            );
            assert_eq!(
                messages
                    .iter()
                    .any(|message| matches!(message, Msg::Connected)),
                !transport_failed
            );
            assert!(messages.iter().any(|message| match message {
                Msg::Status(text) | Msg::Disconnected(text) =>
                    text.contains("adopted 1 process(es); failed at pid 102"),
                _ => false,
            }));
        }
    }

    #[cfg(unix)]
    #[test]
    fn session_log_reads_a_bounded_snapshot_from_a_real_socket() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;
        for lines in [vec![], vec!["log line".to_owned(), "日本語".to_owned()]] {
            let temporary = tempfile::tempdir().unwrap();
            let socket = temporary.path().join("log.sock");
            let listener = UnixListener::bind(&socket).unwrap();
            let expected = lines
                .iter()
                .map(|line| format!("{line}\n"))
                .collect::<String>();
            let server = std::thread::spawn(move || {
                let (stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut reader = BufReader::new(stream);
                let mut request = String::new();
                reader.read_line(&mut request).unwrap();
                assert!(
                    matches!(serde_json::from_str::<Request>(&request).unwrap(), Request::Logs { agent, follow: false, tail: 100 } if agent == "owned-agent")
                );
                for line in lines {
                    let bytes = serde_json::to_vec(&Response::Log { line }).unwrap();
                    if let Some(index) = bytes.iter().position(|byte| *byte >= 128) {
                        // Deliberately split a UTF-8 character across a read timeout.
                        reader.get_mut().write_all(&bytes[..=index]).unwrap();
                        std::thread::sleep(Duration::from_millis(150));
                        reader.get_mut().write_all(&bytes[index + 1..]).unwrap();
                    } else {
                        reader.get_mut().write_all(&bytes).unwrap();
                    }
                    reader.get_mut().write_all(b"\n").unwrap();
                }
                reader.get_mut().write_all(b"{\"type\":\"end\"}\n").unwrap();
            });
            let result = read_session_log(&Client::isolated(socket), "owned-agent");
            server.join().unwrap();
            assert_eq!(result.unwrap(), expected);
        }
    }

    #[test]
    fn delivery_review_preserves_drafts_and_ignores_another_sessions_log() {
        let (commands, requests) = queue::channel();
        let (messages, results) = sync_channel(MESSAGE_CAPACITY);
        let mut app = App::bare(commands, results);
        app.connected = Ok(());
        let now = Utc::now();
        let mut agent = AgentRecord::new(agentdocker_core::AgentSpec::default(), true, now);
        agent.process_started_at = Some(now);
        agent.input_delivery = Some(agentdocker_core::InputDelivery {
            process_started_at: now,
            paused: true,
            pause_reason: Some("Retained input requires review".into()),
            reported_at: now,
            received: None,
            received_at: None,
        });
        let id = agent.id.to_string();
        app.agents.push(agent.clone());
        app.shell.selected = Some(id.clone());
        app.shell.session_message = true;
        app.shell
            .session_drafts
            .entry(id.clone())
            .or_default()
            .draft
            .text = "unfinished message".into();
        let _ = app.update(Message::ReviewDelivery);
        assert!(app.shell.review_delivery);
        assert!(!app.shell.session_details);
        assert!(
            matches!(requests.try_iter().next(), Some(Cmd::SessionLog(target)) if target == id)
        );
        messages
            .send(Msg::Activity(vec![AgentActivity {
                agent: agent.id.clone(),
                name: agent.spec.name.clone(),
                project: None,
                activity: Activity::Finished,
                queued_inputs: Some(2),
                awaiting_receipt: None,
            }]))
            .unwrap();
        messages
            .send(Msg::SessionLog(id.clone(), Ok("input retained".into())))
            .unwrap();
        app.drain();
        assert_eq!(app.queued_inputs[&id], 2);
        assert_eq!(
            app.session_log.as_ref().unwrap().1.as_ref().unwrap(),
            "input retained"
        );
        assert_eq!(
            app.shell.session_drafts[&id].draft.text,
            "unfinished message"
        );
        assert!(app.shell.session_message);
        app.shell.selected = Some("another-session".into());
        messages
            .send(Msg::SessionLog(id.clone(), Ok("late reply".into())))
            .unwrap();
        app.drain();
        assert_eq!(
            app.session_log.as_ref().unwrap().1.as_ref().unwrap(),
            "input retained"
        );
        assert_eq!(
            app.shell.session_drafts[&id].draft.text,
            "unfinished message"
        );
        // Old daemons omit counts; do not retain a stale zero/nonzero snapshot.
        messages
            .send(Msg::Activity(vec![AgentActivity {
                agent: agent.id,
                name: agent.spec.name,
                project: None,
                activity: Activity::Unknown,
                queued_inputs: None,
                awaiting_receipt: None,
            }]))
            .unwrap();
        app.drain();
        assert!(!app.queued_inputs.contains_key(&id));
    }

    #[cfg(unix)]
    #[test]
    fn direct_messages_use_the_common_send_request_and_never_retry_lost_receipts() {
        use serde_json::json;
        for reply in [
            Some(json!({"type":"sent", "message":"queue-receipt", "subscribers":0})),
            Some(json!({"type":"error", "code":"not_found", "message":"agent exited"})),
            None,
        ] {
            let accepted = reply.as_ref().is_some_and(|value| value["type"] == "sent");
            let messages = command_with_replies(
                Cmd::SessionSend("recipient".into(), "input".into()),
                vec![(
                    json!({"op":"send", "from":agentdocker_core::HUMAN, "to":"recipient", "kind":"message", "payload":"input"}),
                    reply,
                )],
            );
            assert!(messages.iter().any(|message| matches!(message,
                Msg::SessionSent(id, result) if id == "recipient" && result.is_ok() == accepted)));
        }
    }

    fn disconnected_command(cmd: Cmd) -> Vec<Msg> {
        let temp = tempfile::tempdir().unwrap();
        let client = Arc::new(Client::isolated(temp.path().join("missing.sock")));
        let (commands, requests) = queue::channel();
        let (messages, results) = sync_channel(MESSAGE_CAPACITY);
        let worker = spawn_worker(
            client,
            requests,
            messages,
            Wake::default(),
            Default::default(),
        );
        commands.send(cmd).unwrap();
        drop(commands);
        worker.join().unwrap();
        let messages: Vec<_> = results.try_iter().collect();
        assert!(
            messages.iter().any(|m| matches!(m, Msg::Disconnected(_))),
            "failed command must report disconnection"
        );
        assert!(
            !messages.iter().any(|m| matches!(m, Msg::Connected)),
            "failed command must not report connectivity"
        );
        messages
    }

    #[test]
    fn pause_transport_failure_keeps_the_reason_and_reports_disconnection() {
        for action in [PauseAction::Pause, PauseAction::Resume] {
            let request = PauseRequest {
                id: MessageId::generate(),
                project: "/fixture/a".into(),
                action,
            };
            let command = match action {
                PauseAction::Pause => Cmd::Pause {
                    request: request.clone(),
                    reason: "saved reason".into(),
                },
                PauseAction::Resume => Cmd::ResumeProject(request.clone()),
            };
            let result = disconnected_command(command);
            let (commands, _requests) = queue::channel();
            let (messages, results) = sync_channel(MESSAGE_CAPACITY);
            let mut app = App::bare(commands, results);
            app.pause_states.insert(
                request.project.clone(),
                PauseControl {
                    draft: Some("saved reason".into()),
                    pending: Some(request.clone()),
                    error: None,
                },
            );
            for message in result {
                messages.send(message).unwrap();
            }
            app.drain();
            let control = &app.pause_states[&request.project];
            assert_eq!(control.draft.as_deref(), Some("saved reason"));
            assert!(control.pending.is_none());
            assert!(control.error.as_ref().unwrap().contains("not confirmed"));
            assert!(app.connected.is_err());
        }
    }

    #[test]
    fn failed_answer_reports_transport_failure_and_keeps_the_draft() {
        let id = MessageId::from("owned-question".to_owned());
        let result = disconnected_command(Cmd::Answer(id.clone(), "draft answer".into()));
        let (commands, _) = queue::channel();
        let (messages, results) = sync_channel(MESSAGE_CAPACITY);
        let mut app = App::bare(commands, results);
        app.shell.answers.insert(id.clone(), "draft answer".into());
        app.sending.insert(id.clone());
        for message in result {
            messages.send(message).unwrap();
        }
        app.drain();
        assert_eq!(app.shell.answers[&id], "draft answer");
        assert!(!app.sending.contains(&id));
        assert!(app.connected.is_err());
    }

    #[test]
    fn failed_adoption_reports_transport_failure() {
        disconnected_command(Cmd::Adopt(123));
    }

    #[test]
    fn failed_stop_reports_transport_failure() {
        disconnected_command(Cmd::Stop("owned-fixture".into()));
    }

    #[test]
    fn failed_inventory_preserves_previous_rows_and_daemon_connection() {
        let (tx, requests) = queue::channel();
        let (messages, rx) = sync_channel::<Msg>(MESSAGE_CAPACITY);
        let mut app = App::bare(tx, rx);
        let rows: Vec<RuntimeInfo> = serde_json::from_value(serde_json::json!([{
            "name":"codex", "vendor":"OpenAI", "label":"Codex",
            "cli":"/fixture/codex", "version":"fixture", "apps":[],
            "config_dir":null, "mcp":"missing", "hooks":"unsupported"
        }]))
        .unwrap();
        messages.send(Msg::Runtimes(rows)).unwrap();
        app.drain();
        messages
            .send(failure_message(
                RemoteError {
                    code: agentdocker_core::ErrorCode::Unavailable,
                    message: "launcher cannot be read".into(),
                }
                .into(),
            ))
            .unwrap();
        app.drain();
        assert!(app.connected.is_ok());
        assert_eq!(app.runtimes.len(), 1);
        assert_eq!(app.runtimes[0].name, "codex");
        assert!(app.status.contains("launcher cannot be read"));
        assert_eq!(requests.try_iter().count(), 0);
        // The daemon answering with an error is a status; anything else
        // on that path is the socket. There is no third case any more:
        // the commands that shell out never reach `failure_message`,
        // because `spawn_worker` takes them off the daemon's thread
        // before it runs them.
        assert!(matches!(
            failure_message(anyhow::anyhow!("socket closed")),
            Msg::Disconnected(_)
        ));
        assert!(matches!(
            failure_message(
                RemoteError {
                    code: agentdocker_core::ErrorCode::Unavailable,
                    message: "the daemon said no".into(),
                }
                .into()
            ),
            Msg::Status(_)
        ));
    }

    #[test]
    fn bounded_shell_lanes_keep_order_without_blocking_each_other() {
        let (tx, rx) = sync_channel(MESSAGE_CAPACITY);
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (started, running) = sync_channel(1);
        let (release, gate) = sync_channel(1);
        let mut first = true;
        let (slow, slow_worker) = lane(
            tx.clone(),
            Wake::default(),
            cancelled.clone(),
            move |line: String| {
                if first {
                    first = false;
                    started.send(()).unwrap();
                    gate.recv_timeout(Duration::from_secs(3)).unwrap();
                }
                Msg::Console(line)
            },
        );
        let (quick, quick_worker) = lane(tx, Wake::default(), cancelled, Msg::Status);
        slow.try_send("first".into()).unwrap();
        running.recv_timeout(Duration::from_secs(3)).unwrap();
        for index in 0..SHELL_QUEUE_CAPACITY {
            assert!(submit(&slow, format!("queued {index}"), Msg::Console).is_none());
        }
        assert!(
            matches!(submit(&slow, "overflow".into(), Msg::Console), Some(Msg::Console(text)) if text.contains("queue is full"))
        );
        // The first worker remains gated, with a full queue, while another
        // completes. No wall-clock race or sleep is used to establish order.
        quick.try_send("meanwhile".into()).unwrap();
        assert!(
            matches!(rx.recv_timeout(Duration::from_secs(3)).unwrap(), Msg::Status(text) if text == "meanwhile")
        );
        release.send(()).unwrap();
        for expected in std::iter::once("first".to_owned())
            .chain((0..SHELL_QUEUE_CAPACITY).map(|index| format!("queued {index}")))
        {
            assert!(
                matches!(rx.recv_timeout(Duration::from_secs(3)).unwrap(), Msg::Console(text) if text == expected)
            );
        }
        drop((slow, quick));
        slow_worker.join().unwrap();
        quick_worker.join().unwrap();
    }

    #[test]
    fn closing_shell_workers_discards_pending_jobs_and_joins() {
        let (tx, rx) = sync_channel(MESSAGE_CAPACITY);
        let (commands, _requests) = queue::channel();
        let (_messages, results) = sync_channel(MESSAGE_CAPACITY);
        let app = App::bare(commands, results);
        let cancelled = app.worker_stop.clone();
        let (started, running) = sync_channel(1);
        let (release, gate) = sync_channel(1);
        let (jobs, worker) = lane(
            tx,
            Wake::default(),
            cancelled.clone(),
            move |line: String| {
                started.send(()).unwrap();
                gate.recv_timeout(Duration::from_secs(3)).unwrap();
                Msg::Console(line)
            },
        );
        jobs.try_send("in flight".into()).unwrap();
        running.recv_timeout(Duration::from_secs(3)).unwrap();
        jobs.try_send("must not execute".into()).unwrap();
        drop(app);
        assert!(cancelled.load(std::sync::atomic::Ordering::Acquire));
        drop(jobs);
        release.send(()).unwrap();
        worker.join().unwrap();
        assert!(matches!(rx.recv().unwrap(), Msg::Console(text) if text == "in flight"));
        assert!(rx.recv().is_err());
        assert!(running.try_recv().is_err());
    }

    #[test]
    fn oversized_command_rejection_preserves_drafts_and_console_state() {
        let (commands, requests) = queue::channel();
        let (_messages, results) = sync_channel(MESSAGE_CAPACITY);
        let mut app = App::bare(commands, results);
        let id = MessageId::from("owned-question".to_owned());
        let answer = "x".repeat(queue::COMMAND_BYTES + 1);
        app.shell.answers.insert(id.clone(), answer.clone());
        app.sending.insert(id.clone());
        app.send(Cmd::Answer(id.clone(), answer.clone()));
        assert_eq!(app.shell.answers[&id], answer);
        assert!(!app.sending.contains(&id));
        app.console_running = 1;
        app.send(Cmd::Console(answer, None));
        assert_eq!(app.console_running, 0);
        assert!(app.console_output.contains("64 KiB"));
        app.setup_busy = true;
        app.send(Cmd::Setup(vec!["x".repeat(queue::COMMAND_BYTES)]));
        assert!(!app.setup_busy);
        assert_eq!(requests.try_iter().count(), 0);
        app.send(Cmd::Agents);
        assert!(matches!(requests.try_iter().next(), Some(Cmd::Agents)));
    }

    /// A notification's message on the Messages screen is scrolled to
    /// once its page is here: on the newest page at once; older, the
    /// pages before are asked for one at a time until it is, and then;
    /// and past the bound the person is told and nothing more is asked.
    #[test]
    fn a_notification_pages_back_to_its_archived_message_and_then_reveals_it() {
        let (tx, requests) = queue::channel();
        let (messages, rx) = sync_channel::<Msg>(MESSAGE_CAPACITY);
        let mut app = App::bare(tx, rx);
        app.connected = Ok(());
        app.conversations_supported = Some(true);
        app.screen = Screen::Questions;
        let room = "channel:abc".to_owned();
        app.shell.conversation = Some(room.clone());
        let archived = |seq: u64| agentdocker_core::ArchivedMessage {
            seq,
            conversation: agentdocker_core::ConversationId::from(room.clone()),
            envelope: {
                let mut envelope = agentdocker_core::Envelope::new(
                    "a",
                    agentdocker_core::Destination::Broadcast,
                    "chat",
                    serde_json::json!({ "text": format!("{seq}") }),
                    None,
                    Utc::now(),
                );
                envelope.id = MessageId::from(format!("m{seq}"));
                envelope
            },
            replies: 0,
        };
        let page = |from: u64| {
            (from..from + HISTORY_PAGE as u64)
                .map(archived)
                .collect::<Vec<_>>()
        };
        // The newest page has it: revealed on the next tick, nothing asked.
        let seek = |message: &str| Seek {
            conversation: room.clone(),
            message: MessageId::from(message.to_owned()),
            pages: 0,
            before: None,
            refresh_pending: false,
        };
        app.reveal_archived = Some(seek("m950"));
        messages
            .send(Msg::History(room.clone(), app.history_epoch, page(801)))
            .unwrap();
        app.drain();
        assert_eq!(
            app.shell
                .reveal_archived_next
                .take()
                .map(|id| id.to_string()),
            Some("m950".into())
        );
        assert!(app.reveal_archived.is_none());
        assert!(
            !requests
                .try_iter()
                .any(|c| matches!(c, Cmd::HistoryBefore(..)))
        );
        // Older: the page before is asked for, once per page, until found.
        app.reveal_archived = Some(seek("m700"));
        app.seek_archived(&room);
        assert!(matches!(
            requests.try_iter().next(),
            Some(Cmd::HistoryBefore(c, 801, _)) if c == room
        ));
        assert_eq!(app.reveal_archived.as_ref().map(|r| r.pages), Some(1));
        // A refresh of the newest page while the earlier one is on its
        // way asks for nothing more and spends no page.
        messages
            .send(Msg::History(room.clone(), app.history_epoch, page(801)))
            .unwrap();
        app.drain();
        assert_eq!(
            requests.try_iter().count(),
            0,
            "no second ask for the same page"
        );
        assert_eq!(app.reveal_archived.as_ref().map(|r| r.pages), Some(1));
        messages
            .send(Msg::HistoryEarlier(
                room.clone(),
                app.history_epoch,
                page(601),
            ))
            .unwrap();
        app.drain();
        assert_eq!(
            app.shell
                .reveal_archived_next
                .take()
                .map(|id| id.to_string()),
            Some("m700".into())
        );
        assert!(app.reveal_archived.is_none());
        // A message that is nowhere: the pages before are asked for until
        // the archive's first page says there is no more, then the person
        // is told and nothing more is asked.
        app.reveal_archived = Some(seek("m0"));
        app.seek_archived(&room);
        let mut asked = 0;
        for from in [401u64, 201, 1] {
            let before: Vec<_> = requests.try_iter().collect();
            assert!(
                before
                    .iter()
                    .any(|c| matches!(c, Cmd::HistoryBefore(c, _, _) if *c == room)),
                "asked for the page before, round {asked}: {before:?}"
            );
            asked += 1;
            let earlier = if from == 1 {
                page(1)[..HISTORY_PAGE - 1].to_vec()
            } else {
                page(from)
            };
            messages
                .send(Msg::HistoryEarlier(
                    room.clone(),
                    app.history_epoch,
                    earlier,
                ))
                .unwrap();
            app.drain();
        }
        assert!(
            app.reveal_archived.is_none(),
            "given up at the archive's start"
        );
        assert!(app.shell.reveal_archived_next.is_none());
        assert!(
            app.status
                .contains("no longer in this conversation's archive"),
            "{}",
            app.status
        );
        assert!(
            !requests
                .try_iter()
                .any(|c| matches!(c, Cmd::HistoryBefore(..)))
        );
        assert!(asked <= App::REVEAL_PAGES);
    }

    /// A cached complete page and replies queued before the click cannot
    /// declare a new notification missing. Only the fresh request can finish it.
    #[test]
    fn a_notification_waits_for_fresh_history_before_using_a_cached_archive() {
        let (tx, requests) = queue::channel();
        let (messages, rx) = sync_channel::<Msg>(MESSAGE_CAPACITY);
        let mut app = App::bare(tx, rx);
        app.connected = Ok(());
        let room = "channel:abc".to_owned();
        app.shell.conversation = Some(room.clone());
        app.history.insert(room.clone(), Vec::new());
        app.history_complete.insert(room.clone());
        let envelope = agentdocker_core::Envelope::new(
            "a",
            agentdocker_core::Destination::Broadcast,
            "chat",
            serde_json::json!({"text": "new since the cached page"}),
            None,
            Utc::now(),
        );
        let target = envelope.id.clone();
        let old_epoch = app.history_epoch;
        app.start_archive_reveal(room.clone(), target.clone());
        assert!(
            matches!(requests.try_iter().next(), Some(Cmd::History(c, epoch))
            if c == room && epoch == app.history_epoch && epoch != old_epoch)
        );
        app.seek_archived(&room);
        assert!(app.reveal_archived.as_ref().unwrap().refresh_pending);
        messages
            .send(Msg::History(room.clone(), old_epoch, Vec::new()))
            .unwrap();
        messages
            .send(Msg::HistoryEarlier(room.clone(), old_epoch, Vec::new()))
            .unwrap();
        app.drain();
        assert!(app.reveal_archived.as_ref().unwrap().refresh_pending);
        assert!(app.status.is_empty());
        assert!(app.shell.reveal_archived_next.is_none());
        messages
            .send(Msg::History(
                room.clone(),
                app.history_epoch,
                vec![agentdocker_core::ArchivedMessage {
                    seq: 1,
                    conversation: agentdocker_core::ConversationId::from(room),
                    envelope,
                    replies: 0,
                }],
            ))
            .unwrap();
        app.drain();
        assert!(app.reveal_archived.is_none());
        assert_eq!(app.shell.reveal_archived_next.as_ref(), Some(&target));
        assert!(app.status.is_empty());
        assert_eq!(requests.try_iter().count(), 0);
    }

    /// Both the first page and subsequent pages must be admitted before
    /// the lookup waits for them. Refusal leaves no cursor or deferred scroll.
    #[test]
    fn a_notification_reports_refused_history_requests_instead_of_waiting() {
        for earlier in [false, true] {
            for stopped in [false, true] {
                let (tx, requests) = queue::channel();
                let (_messages, rx) = sync_channel::<Msg>(MESSAGE_CAPACITY);
                let mut app = App::bare(tx, rx);
                app.connected = Ok(());
                let room = "channel:abc".to_owned();
                app.shell.conversation = Some(room.clone());
                let target = MessageId::from("missing".to_owned());
                if earlier {
                    app.start_archive_reveal(room.clone(), target.clone());
                    assert_eq!(requests.try_iter().count(), 1);
                    app.reveal_archived.as_mut().unwrap().refresh_pending = false;
                    let page = (1..=HISTORY_PAGE as u64)
                        .map(|seq| agentdocker_core::ArchivedMessage {
                            seq,
                            conversation: agentdocker_core::ConversationId::from(room.clone()),
                            envelope: agentdocker_core::Envelope::new(
                                "a",
                                agentdocker_core::Destination::Broadcast,
                                "chat",
                                serde_json::json!({"text": seq}),
                                None,
                                Utc::now(),
                            ),
                            replies: 0,
                        })
                        .collect();
                    app.history.insert(room.clone(), page);
                }
                let requests = if stopped {
                    drop(requests);
                    None
                } else {
                    for _ in 0..queue::CAPACITY {
                        app.tx.send(Cmd::History("another".into(), 0)).unwrap();
                    }
                    Some(requests)
                };
                if earlier {
                    app.seek_archived(&room);
                } else {
                    app.start_archive_reveal(room, target);
                }
                assert!(app.reveal_archived.is_none());
                assert!(app.shell.reveal_archived_next.is_none());
                assert!(
                    app.status.contains(if stopped {
                        "worker stopped"
                    } else {
                        "queue is full"
                    }),
                    "earlier={earlier}, stopped={stopped}: {}",
                    app.status
                );
                if let Some(requests) = requests {
                    assert_eq!(requests.try_iter().count(), queue::CAPACITY);
                }
            }
        }
    }

    /// A notification's search ends with a word when the archive has no
    /// page before (an empty page, not a full one, is the terminal case),
    /// when the window holds all it keeps, when the daemon goes, or when
    /// the person moves on — and a page that then arrives late, even one
    /// with the message, reveals nothing.
    #[test]
    fn a_notification_search_ends_at_the_archive_the_window_the_daemon_or_navigation() {
        let (tx, requests) = queue::channel();
        let (messages, rx) = sync_channel::<Msg>(MESSAGE_CAPACITY);
        let mut app = App::bare(tx, rx);
        app.connected = Ok(());
        app.conversations_supported = Some(true);
        app.screen = Screen::Questions;
        let room = "channel:abc".to_owned();
        app.shell.conversation = Some(room.clone());
        let archived = |seq: u64| agentdocker_core::ArchivedMessage {
            seq,
            conversation: agentdocker_core::ConversationId::from(room.clone()),
            envelope: {
                let mut envelope = agentdocker_core::Envelope::new(
                    "a",
                    agentdocker_core::Destination::Broadcast,
                    "chat",
                    serde_json::json!({ "text": format!("{seq}") }),
                    None,
                    Utc::now(),
                );
                envelope.id = MessageId::from(format!("m{seq}"));
                envelope
            },
            replies: 0,
        };
        let page = |from: u64| {
            (from..from + HISTORY_PAGE as u64)
                .map(archived)
                .collect::<Vec<_>>()
        };
        let seek = |message: &str| Seek {
            conversation: room.clone(),
            message: MessageId::from(message.to_owned()),
            pages: 0,
            before: None,
            refresh_pending: false,
        };
        let asked = |requests: &CommandReceiver| {
            requests
                .try_iter()
                .filter(|c| matches!(c, Cmd::HistoryBefore(..)))
                .count()
        };
        messages
            .send(Msg::History(room.clone(), app.history_epoch, page(801)))
            .unwrap();
        app.drain();

        // The page before is empty: the archive starts with what is here,
        // so the search ends with a word instead of waiting for a page
        // that begins earlier.
        app.reveal_archived = Some(seek("m700"));
        app.seek_archived(&room);
        assert_eq!(asked(&requests), 1);
        messages
            .send(Msg::HistoryEarlier(
                room.clone(),
                app.history_epoch,
                Vec::new(),
            ))
            .unwrap();
        app.drain();
        assert!(app.reveal_archived.is_none(), "ended at an empty page");
        assert!(
            app.status
                .contains("no longer in this conversation's archive"),
            "{}",
            app.status
        );
        assert_eq!(asked(&requests), 0);

        // Navigation ends a search: the page that then comes with the
        // message reveals nothing and asks for nothing.
        app.history_complete.remove(&room);
        app.status.clear();
        app.reveal_archived = Some(seek("m700"));
        app.seek_archived(&room);
        assert_eq!(asked(&requests), 1);
        let _ = app.update(Message::Navigate(Screen::Agents));
        assert!(app.reveal_archived.is_none());
        messages
            .send(Msg::HistoryEarlier(
                room.clone(),
                app.history_epoch,
                page(601),
            ))
            .unwrap();
        app.drain();
        assert!(
            app.shell.reveal_archived_next.is_none(),
            "a late page moves nothing"
        );
        assert!(app.status.is_empty(), "{}", app.status);
        assert_eq!(asked(&requests), 0);

        // Another conversation opened before the tick drops the deferred
        // scroll along with the search; a late page for the old one is
        // kept as history but reveals nothing.
        app.screen = Screen::Questions;
        app.shell.conversation = Some(room.clone());
        app.shell.reveal_archived_next = Some(MessageId::from("m700".to_owned()));
        app.reveal_archived = Some(seek("m500"));
        let _ = app.update(Message::SelectConversation("channel:other".to_owned()));
        assert!(app.shell.reveal_archived_next.is_none());
        assert!(app.reveal_archived.is_none());
        messages
            .send(Msg::HistoryEarlier(
                room.clone(),
                app.history_epoch,
                page(401),
            ))
            .unwrap();
        app.drain();
        assert!(app.shell.reveal_archived_next.is_none());
        assert_eq!(app.history[&room].first().unwrap().seq, 401);
        let _ = requests.try_iter().count();

        // Project navigation hides the conversation: the search and the
        // deferred scroll go with it.
        app.shell.conversation = Some(room.clone());
        app.shell.reveal_archived_next = Some(MessageId::from("m500".to_owned()));
        app.reveal_archived = Some(seek("m300"));
        let _ = app.update(Message::Unassigned);
        assert!(app.shell.reveal_archived_next.is_none());
        assert!(app.reveal_archived.is_none());
        let _ = requests.try_iter().count();

        // The daemon goes while a page is on its way: the search ends.
        app.screen = Screen::Questions;
        app.shell.conversation = Some(room.clone());
        app.reveal_archived = Some(seek("m300"));
        app.seek_archived(&room);
        assert_eq!(asked(&requests), 1);
        messages.send(Msg::Disconnected("gone".into())).unwrap();
        app.drain();
        assert!(app.reveal_archived.is_none(), "ended on disconnect");

        // A window holding all it keeps is not paged further, and is not
        // told that Show earlier messages would help.
        app.connected = Ok(());
        let full: Vec<_> = (1..=HISTORY_KEEP as u64).map(archived).collect();
        app.keep_history(room.clone(), full);
        app.reveal_archived = Some(seek("m0"));
        app.seek_archived(&room);
        assert!(app.reveal_archived.is_none());
        assert!(
            app.status.contains("messages this window keeps"),
            "{}",
            app.status
        );
        assert!(!app.status.contains("Show earlier"), "{}", app.status);
        assert_eq!(asked(&requests), 0);
    }

    #[test]
    fn pause_drafts_and_completions_stay_with_their_project_and_request() {
        let (tx, requests) = queue::channel();
        let (_messages, rx) = sync_channel::<Msg>(MESSAGE_CAPACITY);
        let mut app = App::bare(tx, rx);
        app.connected = Ok(());
        let a = "/fixture/a".to_owned();
        let b = "/fixture/b".to_owned();
        for project in [&a, &b] {
            let _ = app.update(Message::PauseStart(project.clone()));
            let _ = app.update(Message::PauseDraft(
                project.clone(),
                format!("hold {project}"),
            ));
        }
        // Starting B and reopening A retain both unsent reasons.
        let _ = app.update(Message::PauseStart(a.clone()));
        assert_eq!(
            app.pause_states[&a].draft.as_deref(),
            Some("hold /fixture/a")
        );
        assert_eq!(
            app.pause_states[&b].draft.as_deref(),
            Some("hold /fixture/b")
        );
        let _ = app.update(Message::PauseSubmit(a.clone()));
        let first = match requests.try_iter().next().unwrap() {
            Cmd::Pause { request, reason } => {
                assert_eq!(reason, "hold /fixture/a");
                request
            }
            other => panic!("{other:?}"),
        };
        let _ = app.update(Message::PauseSubmit(a.clone()));
        let _ = app.update(Message::PauseCancel(a.clone()));
        let _ = app.update(Message::ResumeProject(a.clone()));
        assert_eq!(requests.try_iter().count(), 0, "one operation per project");
        app.complete_pause(first.clone(), Err("refused".into()));
        assert!(app.pause_states[&a].pending.is_none());
        assert_eq!(
            app.pause_states[&a].draft.as_deref(),
            Some("hold /fixture/a")
        );
        assert_eq!(app.pause_states[&a].error.as_deref(), Some("refused"));
        let _ = app.update(Message::PauseSubmit(a.clone()));
        let second = app.pause_states[&a].pending.clone().unwrap();
        assert_ne!(first.id, second.id);
        requests.try_iter().for_each(drop);
        for result in [Ok(()), Err("late refusal".into())] {
            app.complete_pause(first.clone(), result);
            assert_eq!(app.pause_states[&a].pending.as_ref(), Some(&second));
            assert!(app.pause_states[&a].error.is_none());
        }
        let mut wrong_action = second.clone();
        wrong_action.action = PauseAction::Resume;
        app.complete_pause(wrong_action, Ok(()));
        assert_eq!(app.pause_states[&a].pending.as_ref(), Some(&second));
        app.complete_pause(second, Ok(()));
        assert!(!app.pause_states.contains_key(&a));
        assert_eq!(
            app.pause_states[&b].draft.as_deref(),
            Some("hold /fixture/b")
        );
        assert!(matches!(requests.try_iter().next(), Some(Cmd::Pauses)));
        let _ = app.update(Message::ResumeProject(a.clone()));
        let resume = app.pause_states[&a].pending.clone().unwrap();
        requests.try_iter().for_each(drop);
        app.complete_pause(resume.clone(), Ok(()));
        requests.try_iter().for_each(drop);
        let _ = app.update(Message::PauseStart(a.clone()));
        let _ = app.update(Message::PauseDraft(a.clone(), "new hold".into()));
        let _ = app.update(Message::PauseSubmit(a.clone()));
        let current = app.pause_states[&a].pending.clone();
        app.complete_pause(resume, Ok(()));
        assert_eq!(app.pause_states[&a].pending, current);
        assert_eq!(app.pause_states[&a].draft.as_deref(), Some("new hold"));
    }

    #[test]
    fn rejected_pause_admission_keeps_a_bounded_retryable_draft() {
        let (tx, requests) = queue::channel();
        let (_messages, rx) = sync_channel::<Msg>(MESSAGE_CAPACITY);
        let mut app = App::bare(tx, rx);
        app.connected = Ok(());
        let project = "/fixture/a".to_owned();
        let _ = app.update(Message::PauseStart(project.clone()));
        let _ = app.update(Message::PauseDraft(project.clone(), "keep this".into()));
        let _ = app.update(Message::PauseDraft(project.clone(), "界".repeat(401)));
        assert_eq!(
            app.pause_states[&project].draft.as_deref(),
            Some("keep this")
        );
        assert!(
            app.pause_states[&project]
                .error
                .as_ref()
                .unwrap()
                .contains("400")
        );
        for _ in 0..queue::CAPACITY {
            app.send(Cmd::Stop("fixture".into()));
        }
        let _ = app.update(Message::PauseSubmit(project.clone()));
        assert!(app.pause_states[&project].pending.is_none());
        assert!(app.pause_states[&project].error.is_some());
        assert_eq!(
            app.pause_states[&project].draft.as_deref(),
            Some("keep this")
        );
        requests.try_iter().for_each(drop);
        let _ = app.update(Message::PauseSubmit(project.clone()));
        assert!(matches!(
            requests.try_iter().next(),
            Some(Cmd::Pause { .. })
        ));
        for n in 0..PAUSE_CONTROLS + 1 {
            let _ = app.update(Message::PauseStart(format!("/fixture/{n}")));
        }
        assert_eq!(app.pause_states.len(), PAUSE_CONTROLS);
    }

    #[test]
    fn reconnect_refreshes_all_snapshots_including_the_selected_journal() {
        let (tx, requests) = queue::channel();
        let (messages, rx) = sync_channel::<Msg>(MESSAGE_CAPACITY);
        let mut app = App::bare(tx, rx);
        app.connected = Err("offline".into());
        app.journal_project = Some("project-a".into());
        messages.send(Msg::Connected).unwrap();
        app.drain();
        let received: Vec<_> = requests.try_iter().collect();
        assert!(app.connected.is_ok());
        assert_eq!(received.len(), 11);
        assert!(received.iter().any(|cmd| matches!(cmd, Cmd::Me)));
        assert!(received.iter().any(|cmd| matches!(cmd, Cmd::Connector)));
        assert!(received.iter().any(|cmd| matches!(cmd, Cmd::Pauses)));
        assert!(received.iter().any(|cmd| matches!(cmd, Cmd::Activity)));
        assert!(received.iter().any(|cmd| matches!(cmd, Cmd::Agents)));
        assert!(received.iter().any(|cmd| matches!(cmd, Cmd::Leases)));
        assert!(received.iter().any(|cmd| matches!(cmd, Cmd::Discovered)));
        assert!(received.iter().any(|cmd| matches!(cmd, Cmd::Runtimes)));
        assert!(received.iter().any(|cmd| matches!(cmd, Cmd::Questions)));
        assert!(
            received
                .iter()
                .any(|cmd| matches!(cmd, Cmd::Journal(project, _) if project == "project-a"))
        );
    }

    #[test]
    fn a_replayed_event_is_taken_once() {
        use agentdocker_core::AgentId;
        let stopping = |seq: u64| {
            let mut event = Event::new(
                EventKind::AgentRemoved {
                    agent: AgentId::from("a1"),
                },
                Utc::now(),
            );
            event.seq = seq;
            event
        };
        let (tx, requests) = queue::channel();
        let (_mtx, mrx) = sync_channel::<Msg>(MESSAGE_CAPACITY);
        let mut app = App::bare(tx, mrx);
        app.on_event(stopping(1));
        app.on_event(stopping(2));
        assert_eq!(app.last_seq, 2);
        let asked = requests.try_iter().count();
        assert!(asked > 0, "an agent event refreshes the agent list");

        // A reconnect replays what was already taken. Acting on those
        // again would refetch everything for nothing.
        app.on_event(stopping(1));
        app.on_event(stopping(2));
        assert_eq!(app.last_seq, 2, "replayed events are not taken twice");
        assert_eq!(
            requests.try_iter().count(),
            0,
            "and nothing is asked of the daemon for them"
        );

        app.on_event(stopping(3));
        assert_eq!(app.last_seq, 3, "and newer ones still are");
        assert!(requests.try_iter().count() > 0);

        // Live-only events carry no sequence and always count.
        let mut live = Event::new(
            EventKind::AgentRemoved {
                agent: AgentId::from("a2"),
            },
            Utc::now(),
        );
        live.seq = 0;
        app.on_event(live.clone());
        app.on_event(live);
        assert_eq!(app.last_seq, 3, "a live event does not move the cursor");
        assert_eq!(
            requests.try_iter().count(),
            1,
            "live events coalesce into a fresh snapshot"
        );
    }

    #[test]
    fn a_command_line_splits_the_way_a_shell_would() {
        let words = |line: &str| shell_words(line).unwrap();
        assert_eq!(words("ps --all"), ["ps", "--all"]);
        assert_eq!(words("   ps   "), ["ps"]);
        assert!(words("").is_empty());
        assert_eq!(
            words("review c1 --changes \"handle the empty input\""),
            ["review", "c1", "--changes", "handle the empty input"]
        );
        assert_eq!(
            words("journal add --as me 'two words'"),
            ["journal", "add", "--as", "me", "two words"]
        );
        // An empty quoted argument is still an argument.
        assert_eq!(words("send --to x \"\""), ["send", "--to", "x", ""]);
        assert_eq!(shell_words("unbalanced \"quote"), None);
    }

    #[test]
    fn spans_read_well() {
        assert_eq!(span(45), "45s");
        assert_eq!(span(180), "3m");
        assert_eq!(span(7200), "2h");
    }

    #[test]
    fn a_turn_finished_out_of_view_waits_to_be_viewed_and_idle_alone_is_not_seen() {
        use agentdocker_core::AgentSpec;
        let (commands, _requests) = queue::channel();
        let (messages, results) = sync_channel(MESSAGE_CAPACITY);
        let mut app = App::bare(commands, results);
        let mut agents = Vec::new();
        for (name, root) in [("worker", "/fixture/alpha"), ("other", "/fixture/beta")] {
            let mut agent = AgentRecord::new(
                AgentSpec {
                    name: name.into(),
                    ..Default::default()
                },
                false,
                Utc::now(),
            );
            let mut project = ProjectRef::directory(root);
            project.fingerprint = Some(name.into());
            agent.project = Some(project.clone());
            app.shell.catalog.remember(project, false);
            agents.push(agent);
        }
        let (worker, other) = (agents[0].clone(), agents[1].clone());
        messages.send(Msg::Agents(agents, BTreeMap::new())).unwrap();
        app.drain();
        let report = |records: &[(&AgentRecord, Activity)]| {
            Msg::Activity(
                records
                    .iter()
                    .map(|(agent, activity)| AgentActivity {
                        agent: agent.id.clone(),
                        name: agent.spec.name.clone(),
                        project: agent.project.as_ref().map(|p| p.id()),
                        activity: activity.clone(),
                        queued_inputs: Some(0),
                        awaiting_receipt: None,
                    })
                    .collect(),
            )
        };
        let working = Activity::Working { since: Utc::now() };
        let idle = Activity::Idle { since: Utc::now() };

        // Idle from the start is a state, not a completion.
        messages
            .send(report(&[(&worker, idle.clone()), (&other, idle.clone())]))
            .unwrap();
        app.drain();
        assert!(app.shell.unviewed_done.is_empty());

        // The user is looking at beta while alpha's worker finishes.
        let _ = app.update(Message::SelectProject("/fixture/beta".into()));
        messages
            .send(report(&[
                (&worker, working.clone()),
                (&other, working.clone()),
            ]))
            .unwrap();
        app.drain();
        messages
            .send(report(&[(&worker, idle.clone()), (&other, idle.clone())]))
            .unwrap();
        app.drain();
        assert!(app.shell.unviewed_done.contains(worker.id.as_str()));
        assert!(
            !app.shell.unviewed_done.contains(other.id.as_str()),
            "a completion on the screen being looked at is viewed as it happens"
        );
        assert_eq!(
            app.shell
                .unviewed_in(&app.agents, std::path::Path::new("/fixture/alpha")),
            1
        );

        // Idle reports keep arriving; none of them counts as viewing.
        messages.send(report(&[(&worker, idle.clone())])).unwrap();
        app.drain();
        assert!(app.shell.unviewed_done.contains(worker.id.as_str()));

        // Opening the project is viewing.
        let _ = app.update(Message::SelectProject("/fixture/alpha".into()));
        assert!(app.shell.unviewed_done.is_empty());

        // In an unfocused window even the open project is not being looked at.
        let _ = app.update(Message::Event(iced::Event::Window(
            iced::window::Event::Unfocused,
        )));
        messages
            .send(report(&[(&worker, working.clone())]))
            .unwrap();
        app.drain();
        messages.send(report(&[(&worker, idle.clone())])).unwrap();
        app.drain();
        assert!(app.shell.unviewed_done.contains(worker.id.as_str()));
        // Selecting the session is viewing it.
        let _ = app.update(Message::SelectSession(worker.id.to_string()));
        assert!(app.shell.unviewed_done.is_empty());

        // Working again clears a stale badge without anyone viewing it.
        let _ = app.update(Message::Event(iced::Event::Window(
            iced::window::Event::Unfocused,
        )));
        messages
            .send(report(&[(&worker, working.clone())]))
            .unwrap();
        app.drain();
        messages.send(report(&[(&worker, idle)])).unwrap();
        app.drain();
        assert!(app.shell.unviewed_done.contains(worker.id.as_str()));
        messages.send(report(&[(&worker, working)])).unwrap();
        app.drain();
        assert!(app.shell.unviewed_done.is_empty());
    }

    pub(crate) fn record(name: &str, runtime: &str, pid: Option<u32>) -> AgentRecord {
        let mut record = AgentRecord::new(
            agentdocker_core::AgentSpec {
                name: name.into(),
                runtime: runtime.into(),
                ..Default::default()
            },
            false,
            Utc::now(),
        );
        record.pid = pid;
        record.status = agentdocker_core::AgentStatus::Running;
        record
    }

    #[test]
    fn a_name_is_generated_by_provenance_not_by_its_shape() {
        let (commands, _requests) = queue::channel();
        let (_messages, results) = sync_channel(MESSAGE_CAPACITY);
        let mut app = App::bare(commands, results);
        // Labelled by its adapter: generated, whatever it looks like.
        let mut labelled = record("anything", "codex", None);
        labelled.spec.labels.insert(
            agentdocker_core::agent::NAME_LABEL.into(),
            agentdocker_core::agent::GENERATED_NAME.into(),
        );
        // An older record: only the exact adapter form from its own pid.
        let by_pid = record("codex-51242", "codex", Some(51242));
        let mut by_session = record("claude-218845eb", "claude-code", None);
        by_session.spec.labels.insert(
            "session_id".into(),
            "218845eb-ba1e-4457-bb5a-e1829f5652dd".into(),
        );
        // Chosen names that merely look like identifiers stay as chosen.
        let chosen_hex = record("codex-cafe", "codex", Some(51242));
        let chosen_pid_elsewhere = record("codex-51242", "codex", Some(999));
        let chosen = record("reviewer", "codex", Some(1));
        app.agents = vec![
            labelled.clone(),
            by_pid.clone(),
            by_session.clone(),
            chosen_hex.clone(),
            chosen_pid_elsewhere.clone(),
            chosen.clone(),
        ];
        assert!(app.display_name(&labelled).starts_with("Codex"));
        assert!(app.display_name(&by_pid).starts_with("Codex"));
        assert_eq!(app.display_name(&by_session), "Claude Code");
        assert_eq!(app.display_name(&chosen_hex), "codex-cafe");
        assert_eq!(app.display_name(&chosen_pid_elsewhere), "codex-51242");
        assert_eq!(app.display_name(&chosen), "reviewer");
    }

    /// A thread's composer keeps its own draft and alone sets `reply_to`;
    /// a conversation between two agents has no destination from here; the
    /// pane on view is what marks read, never the list a narrow window
    /// shows instead of it.
    /// A conversation draft whose send could not even be queued is told
    /// so at once, so it can be sent again; a draft whose send failed at
    /// the daemon hears the same through the worker.
    #[test]
    fn a_conversation_draft_is_completed_when_its_send_is_refused() {
        let (commands, requests) = queue::channel();
        let (messages, results) = sync_channel(MESSAGE_CAPACITY);
        let mut app = App::bare(commands, results);
        app.connected = Ok(());
        app.conversations_supported = Some(true);
        let mut human = record("user", agentdocker_core::HUMAN_RUNTIME, None);
        human.id = agentdocker_core::AgentId::from("human-id");
        let mut agent = record("codex-1", "codex", Some(1));
        agent.id = agentdocker_core::AgentId::from("agent-a");
        app.agents = vec![human, agent];
        let own = agentdocker_core::ConversationId::dm("human-id", "agent-a");
        let key = own.as_str().to_owned();
        // The worker is gone: the queue refuses everything.
        drop(requests);
        let _ = app.update(Message::ConversationDraft(key.clone(), "hello".into()));
        let _ = app.update(Message::SendConversation(key.clone()));
        let draft = &app.shell.conversation_drafts[&key];
        assert!(draft.sending.is_none(), "not left sending");
        assert!(
            draft
                .error
                .as_deref()
                .is_some_and(|e| e.contains("worker stopped")),
            "{draft:?}"
        );
        assert_eq!(draft.text, "hello", "the words are kept");
        // The worker's answer to a daemon that refused the send.
        messages
            .send(Msg::ConversationSent(key.clone(), Err("refused".into())))
            .unwrap();
        app.drain();
        assert_eq!(
            app.shell.conversation_drafts[&key].error.as_deref(),
            Some("refused")
        );
        assert!(app.shell.conversation_drafts[&key].sending.is_none());
    }

    #[test]
    fn a_thread_has_its_own_draft_and_a_peer_conversation_is_read_only() {
        let (commands, requests) = queue::channel();
        let (_messages, results) = sync_channel(MESSAGE_CAPACITY);
        let mut app = App::bare(commands, results);
        app.connected = Ok(());
        app.conversations_supported = Some(true);
        let mut human = record("user", agentdocker_core::HUMAN_RUNTIME, None);
        human.id = agentdocker_core::AgentId::from("human-id");
        let mut agent = record("codex-1", "codex", Some(1));
        agent.id = agentdocker_core::AgentId::from("agent-a");
        let mut other = record("codex-2", "codex", Some(2));
        other.id = agentdocker_core::AgentId::from("agent-b");
        app.agents = vec![human, agent, other];
        let own = agentdocker_core::ConversationId::dm("human-id", "agent-a");
        let peers = agentdocker_core::ConversationId::dm("agent-a", "agent-b");
        assert_eq!(
            app.conversation_destination(own.as_str()).as_deref(),
            Some("agent-a")
        );
        assert_eq!(app.conversation_destination(peers.as_str()), None);

        let root = MessageId::from("root-1".to_owned());
        let key = draft_key(own.as_str(), Some(&root));
        assert_eq!(split_draft_key(&key), (own.as_str(), Some(root.clone())));
        assert_eq!(split_draft_key(own.as_str()), (own.as_str(), None));
        app.screen = Screen::Questions;
        app.shell.conversation = Some(own.as_str().to_owned());
        app.shell.thread = Some(root.clone());
        let _ = app.update(Message::ConversationDraft(
            own.as_str().to_owned(),
            "for the conversation".into(),
        ));
        let _ = app.update(Message::ConversationDraft(
            key.clone(),
            "for the thread".into(),
        ));
        let _ = app.update(Message::SendConversation(key.clone()));
        let sent = requests
            .try_iter()
            .find_map(|cmd| match cmd {
                Cmd::ConversationSend {
                    draft,
                    to,
                    text,
                    reply_to,
                } => Some((draft, to, text, reply_to)),
                _ => None,
            })
            .expect("the thread's words are sent");
        assert_eq!(sent.0, key);
        assert_eq!(sent.1, "agent-a");
        assert_eq!(sent.2, "for the thread");
        assert_eq!(sent.3, Some(root));
        assert_eq!(
            app.shell.conversation_drafts[own.as_str()].text,
            "for the conversation",
            "the conversation's own draft is untouched"
        );

        // Narrow, list on view: nothing is marked read; nor behind a thread.
        app.shell.width = 700.0;
        app.shell.inbox_open = false;
        assert!(!app.conversation_pane_visible());
        app.shell.inbox_open = true;
        assert!(
            !app.conversation_pane_visible(),
            "the thread stands in for the pane"
        );
        app.shell.thread = None;
        assert!(app.conversation_pane_visible());
    }

    #[test]
    fn history_marks_read_only_when_the_conversation_is_rendered() {
        for (width, text_size, inbox_open, thread_open, visible) in [
            (1000.0, 14.0, true, true, false),
            (1180.0, 18.0, true, true, false),
            (1000.0, 14.0, true, false, true),
            (1000.0, 14.0, false, true, false),
            (1180.0, 14.0, true, true, true),
            (700.0, 14.0, false, false, false),
            (700.0, 14.0, true, false, true),
        ] {
            let (commands, requests) = queue::channel();
            let (messages, results) = sync_channel(MESSAGE_CAPACITY);
            let mut app = App::bare(commands, results);
            app.screen = Screen::Questions;
            app.shell.width = width;
            app.shell.inbox_open = inbox_open;
            app.shell.thread = thread_open.then(|| MessageId::from("root".to_owned()));
            let room = "channel:unread-fixture".to_owned();
            app.shell.conversation = Some(room.clone());
            app.conversations = vec![agentdocker_core::ConversationSummary {
                conversation: room.clone().into(),
                kind: agentdocker_core::ConversationKind::Channel,
                name: None,
                title: "room".into(),
                members: Vec::new(),
                unread: 1,
                mentions: 0,
                last_seq: Some(7),
                last_at: None,
                last_from: None,
                last_line: None,
            }];
            let _ = app.update(Message::TextSize(text_size));
            messages
                .send(Msg::History(
                    room.clone(),
                    app.history_epoch,
                    vec![agentdocker_core::ArchivedMessage {
                        seq: 7,
                        conversation: room.clone().into(),
                        replies: 0,
                        envelope: agentdocker_core::Envelope::new(
                            "sender",
                            agentdocker_core::Destination::Broadcast,
                            "chat",
                            serde_json::json!({"text":"unread message"}),
                            None,
                            Utc::now(),
                        ),
                    }],
                ))
                .unwrap();
            app.drain();
            let read = requests
                .try_iter()
                .filter_map(|cmd| match cmd {
                    Cmd::MarkRead(conversation, through) => Some((conversation, through)),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(
                read,
                if visible { vec![(room, 7)] } else { vec![] },
                "width={width}, text={text_size}, inbox={inbox_open}, thread={thread_open}"
            );
        }
    }

    /// Narrow with a thread open nothing marks read; paging back keeps every
    /// page up to the daemon's own cap, with the oldest on view moving each
    /// time; a prune drops the archives and the thread, and replies from
    /// before it are not applied after it.
    #[test]
    fn a_hidden_thread_marks_nothing_read_and_the_archive_cache_is_bounded() {
        let (commands, requests) = queue::channel();
        let (messages, results) = sync_channel(MESSAGE_CAPACITY);
        let mut app = App::bare(commands, results);
        app.connected = Ok(());
        app.conversations_supported = Some(true);
        app.screen = Screen::Questions;
        let room = "channel:abc".to_owned();
        app.shell.conversation = Some(room.clone());
        app.shell.inbox_open = true;
        app.shell.width = 700.0;
        let root = MessageId::from("root".to_owned());
        app.shell.thread = Some(root.clone());
        assert!(
            !app.conversation_pane_visible(),
            "a thread stands in for the pane"
        );
        let archived = |seq: u64| agentdocker_core::ArchivedMessage {
            seq,
            conversation: agentdocker_core::ConversationId::from(room.clone()),
            envelope: agentdocker_core::Envelope::new(
                "a",
                agentdocker_core::Destination::Broadcast,
                "chat",
                serde_json::json!({ "text": format!("{seq}") }),
                None,
                Utc::now(),
            ),
            replies: 0,
        };
        app.conversations = vec![agentdocker_core::ConversationSummary {
            conversation: agentdocker_core::ConversationId::from(room.clone()),
            kind: agentdocker_core::ConversationKind::Channel,
            name: None,
            title: "room".into(),
            members: Vec::new(),
            unread: 3,
            mentions: 0,
            last_seq: Some(3),
            last_at: None,
            last_from: None,
            last_line: None,
        }];
        let epoch = app.history_epoch;
        messages
            .send(Msg::History(
                room.clone(),
                epoch,
                (1..=3).map(archived).collect(),
            ))
            .unwrap();
        app.drain();
        assert!(
            !requests
                .try_iter()
                .any(|cmd| matches!(cmd, Cmd::MarkRead(..))),
            "nothing is read behind a thread"
        );
        assert!(
            app.history_complete.contains(&room),
            "a short page is the whole archive"
        );

        // Paging back: each earlier page stays and the oldest on view moves,
        // all the way to the daemon's cap, where nothing earlier remains.
        let big = "channel:big".to_owned();
        let pages = HISTORY_KEEP as u64 / HISTORY_PAGE as u64;
        let base = pages * HISTORY_PAGE as u64;
        let full: Vec<_> = (base..base + HISTORY_PAGE as u64).map(archived).collect();
        messages
            .send(Msg::History(big.clone(), epoch, full))
            .unwrap();
        app.drain();
        for page in 0..pages - 1 {
            let before = app.history[&big].first().unwrap().seq;
            let start = base - (page + 1) * HISTORY_PAGE as u64;
            let earlier: Vec<_> = (start..start + HISTORY_PAGE as u64).map(archived).collect();
            messages
                .send(Msg::HistoryEarlier(big.clone(), epoch, earlier))
                .unwrap();
            app.drain();
            assert_eq!(app.history[&big].first().unwrap().seq, start);
            assert!(app.history[&big].first().unwrap().seq < before);
            assert!(!app.history_complete.contains(&big));
        }
        assert_eq!(app.history[&big].len(), HISTORY_KEEP);
        assert_eq!(
            app.history[&big].last().unwrap().seq,
            base + HISTORY_PAGE as u64 - 1
        );
        // The daemon's cap is reached: the page before is empty, and the
        // archive is complete.
        messages
            .send(Msg::HistoryEarlier(big.clone(), epoch, Vec::new()))
            .unwrap();
        app.drain();
        assert!(app.history_complete.contains(&big));
        assert_eq!(app.history[&big].len(), HISTORY_KEEP);

        // A prune: archives and the thread go, the open ones are asked for
        // again, and replies from before the prune are dropped.
        app.thread = Some((archived(3), Vec::new()));
        app.on_event(agentdocker_core::Event {
            seq: 1,
            at: Utc::now(),
            kind: EventKind::MessagesPruned { removed: 5 },
        });
        assert!(app.history.is_empty() && app.history_complete.is_empty());
        assert!(app.thread.is_none());
        assert!(
            requests
                .try_iter()
                .any(|cmd| matches!(cmd, Cmd::Thread(_, e) if e == epoch + 1))
        );
        messages
            .send(Msg::History(
                room.clone(),
                epoch,
                (1..=3).map(archived).collect(),
            ))
            .unwrap();
        messages
            .send(Msg::HistoryEarlier(big.clone(), epoch, vec![archived(9)]))
            .unwrap();
        messages
            .send(Msg::Thread(epoch, archived(3), Vec::new()))
            .unwrap();
        app.drain();
        assert!(app.history.is_empty(), "a reply from before the prune");
        assert!(app.thread.is_none());
        // The root itself was pruned: the thread closes.
        messages
            .send(Msg::ThreadGone(root.clone(), epoch + 1))
            .unwrap();
        app.drain();
        assert!(app.shell.thread.is_none());
    }

    #[test]
    fn sessions_are_named_by_tool_and_branch_and_numbered_only_when_that_is_not_enough() {
        let (commands, _requests) = queue::channel();
        let (_messages, results) = sync_channel(MESSAGE_CAPACITY);
        let mut app = App::bare(commands, results);
        let now = Utc::now();
        let on = |branch: &str| {
            Some(agentdocker_core::VcsState {
                branch: Some(branch.into()),
                head: None,
                dirty: None,
                updated_at: now,
            })
        };
        let mut first = record("codex-5124", "codex", Some(5124));
        first.created_at = now;
        first.vcs = on("main");
        let mut second = record("codex-6250", "codex", Some(6250));
        second.id = agentdocker_core::AgentId::from("second-id");
        second.created_at = now + chrono::Duration::seconds(1);
        second.vcs = on("feature/x");
        app.agents = vec![second.clone(), first.clone()];
        // Different branches: the branch is the name, no number.
        assert_eq!(app.display_name(&first), "Codex · main");
        assert_eq!(app.display_name(&second), "Codex · feature/x");
        // Two live sessions on one branch: numbered by first appearance.
        second.vcs = on("main");
        app.agents = vec![second.clone(), first.clone()];
        assert_eq!(app.display_name(&first), "Codex · main (1)");
        assert_eq!(app.display_name(&second), "Codex · main (2)");
        // The first one ends: it drops its number, the second is alone
        // among the live ones and drops its number too.
        first.status = agentdocker_core::AgentStatus::Exited { code: Some(0) };
        app.agents = vec![second.clone(), first.clone()];
        assert_eq!(app.display_name(&first), "Codex · main");
        assert_eq!(app.display_name(&second), "Codex · main");
        // No branch at all: the tool alone, numbered only in company.
        let mut bare = record("codex-7000", "codex", Some(7000));
        bare.id = agentdocker_core::AgentId::from("third-id");
        app.agents = vec![bare.clone()];
        assert_eq!(app.display_name(&bare), "Codex");
    }

    #[test]
    fn journal_lines_are_attributed_by_agent_id_not_by_name() {
        let (commands, _requests) = queue::channel();
        let (_messages, results) = sync_channel(MESSAGE_CAPACITY);
        let mut app = App::bare(commands, results);
        let now = Utc::now();
        let mut older = record("codex-1", "codex", Some(1));
        older.created_at = now;
        let mut newer = record("codex-2", "codex", Some(2));
        newer.id = agentdocker_core::AgentId::from("newer-id");
        newer.created_at = now + chrono::Duration::seconds(1);
        app.agents = vec![older.clone(), newer.clone()];
        let entry: agentdocker_core::JournalEntry = serde_json::from_value(serde_json::json!({
            "project": "p", "seq": 7, "at": now, "agent": newer.id.as_str(), "agent_name": "codex-2",
            "kind": "note", "summary": "done", "summary_source": "explicit"
        }))
        .unwrap();
        let line = app.journal_line(&entry);
        assert!(line.contains("Codex (2)"), "{line}");
        assert!(!line.contains("codex-2"), "{line}");
        // A line whose author is not on record is left as it is.
        let unknown: agentdocker_core::JournalEntry = serde_json::from_value(serde_json::json!({
            "project": "p", "seq": 8, "at": now, "agent": "nobody", "agent_name": "codex-2",
            "kind": "note", "summary": "done", "summary_source": "explicit"
        }))
        .unwrap();
        assert!(app.journal_line(&unknown).contains("codex-2"));
        assert_eq!(app.name_of("nobody"), "an unknown session");
    }
}
