//! The window: what it shows, how it asks the daemon, and how it keeps
//! up. Requests run on a worker thread and the event stream on another;
//! both hand results to the UI thread through a channel and ask for a
//! repaint, so the window never blocks on the socket.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};

mod icons;
mod queue;
mod sessions;
mod shell;
pub(crate) mod style;
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
const CONSOLE_BYTES: usize = 256 * 1024;
const MESSAGE_CAPACITY: usize = 64;
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
    Discovered,
    Journal(String, String),
    Channels(String, String),
    Inbox,
    Activity,
    SessionLog(String),
    /// Register the person at the keyboard, so agents can address them.
    Me,
    Questions,
    Answer(MessageId, String),
    DismissMessages(Vec<MessageId>),
    Adopt(u32),
    AdoptAll,
    Stop(String),
    Setup(Vec<String>),
    Desktop(Vec<String>),
    UpdateCheck,
    /// Any `agentdocker` command, so the window is not limited to the
    /// few actions that have buttons.
    Console(String, Option<std::path::PathBuf>),
    Launch(Box<agentdocker_core::AgentSpec>),
    ChannelSend(String, String),
    SessionSend(String, String),
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
    Discovered(Vec<DiscoveredProcess>),
    Journal(String, Option<u64>, Vec<JournalEntry>),
    Channels(String, Vec<agentdocker_core::Channel>),
    Inbox(Vec<agentdocker_core::Envelope>),
    Activity(Vec<AgentActivity>),
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
    ChannelSent(String, Result<MessageId, String>),
    SessionSent(String, Result<MessageId, String>),
}

pub struct App {
    shell: shell::State,
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
    /// Questions put to the human, and what is being typed in reply to
    /// each. The draft is keyed by message id so answering one question
    /// does not disturb another half-written answer.
    questions: Vec<Question>,
    answers: BTreeMap<MessageId, String>,
    /// What each agent is doing, keyed by id. Derived by the daemon, so
    /// it is read rather than computed here.
    activity: BTreeMap<String, Activity>,
    queued_inputs: BTreeMap<String, usize>,
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
        // Permission requests belong to the foreground app and its run loop.
        crate::notify::request_permission();
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
        Self {
            shell,
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
            discovered: Vec::new(),
            journal: Vec::new(),
            journal_project: None,
            channels: Vec::new(),
            inbox: Vec::new(),
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
            answers: BTreeMap::new(),
            sending: std::collections::BTreeSet::new(),
            dismissing: std::collections::BTreeSet::new(),
            activity: BTreeMap::new(),
            queued_inputs: BTreeMap::new(),
            session_log: None,
        }
    }

    /// The window's state without a window or a daemon, for tests.
    #[cfg(test)]
    fn bare(tx: CommandSender, rx: Receiver<Msg>) -> Self {
        Self {
            shell: Default::default(),
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
            discovered: Vec::new(),
            journal: Vec::new(),
            journal_project: None,
            channels: Vec::new(),
            inbox: Vec::new(),
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
            answers: BTreeMap::new(),
            sending: std::collections::BTreeSet::new(),
            activity: BTreeMap::new(),
            queued_inputs: BTreeMap::new(),
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
                Cmd::SessionSend(id, _) => {
                    if let Some(entry) = self.shell.session_drafts.get_mut(&id) {
                        entry.draft.complete(Err(reason.into()));
                    }
                }
                Cmd::Adopt(_) | Cmd::AdoptAll | Cmd::Stop(_) => {}
                // Full queues may omit refreshes: events and periodic refresh
                // request another snapshot. User actions get an explicit error.
                _ => return,
            }
            self.say(reason);
        }
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
                Msg::Inbox(inbox) => self.inbox = inbox,
                Msg::MessagesDismissed(ids, result) => {
                    self.dismissing.retain(|id| !ids.contains(id));
                    match result {
                        Ok(()) => {
                            self.inbox.retain(|message| !ids.contains(&message.id));
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
                Msg::Activity(activity) => {
                    self.queued_inputs = activity
                        .iter()
                        .filter_map(|a| a.queued_inputs.map(|count| (a.agent.to_string(), count)))
                        .collect();
                    let fresh: BTreeMap<String, Activity> = activity
                        .into_iter()
                        .map(|a| (a.agent.to_string(), a.activity))
                        .collect();
                    self.note_completions(&fresh);
                    self.activity = fresh;
                }
                Msg::Questions(questions) => {
                    // Forget drafts for questions nobody is waiting on any
                    // more, so the map does not grow with the session — but
                    // not one still in flight, whose question the daemon
                    // has already forgotten.
                    self.answers.retain(|id, _| {
                        self.sending.contains(id) || questions.iter().any(|q| q.id == *id)
                    });
                    self.questions = questions;
                }
                Msg::Answered(id, result) => {
                    self.sending.remove(&id);
                    match result {
                        Ok(()) => {
                            self.answers.remove(&id);
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
                            Cmd::Questions,
                            Cmd::Activity,
                            Cmd::Inbox,
                        ] {
                            self.send(cmd);
                        }
                        if let Some(project) = &self.journal_project {
                            self.request_journal(project.clone());
                        }
                    }
                }
                Msg::Disconnected(reason) => self.connected = Err(reason),
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
                Msg::SessionSent(id, result) => {
                    if let Some(entry) = self.shell.session_drafts.get_mut(&id) {
                        match result {
                            Ok(receipt) => {
                                entry.queued = Some(receipt);
                                entry.draft.complete(Ok(()));
                            }
                            Err(error) => entry.draft.complete(Err(error)),
                        }
                    }
                }
                Msg::ChannelSent(id, result) => {
                    let draft = self.shell.channel_drafts.entry(id.clone()).or_default();
                    match result {
                        Ok(message) => {
                            let sent = draft.sending.clone();
                            draft.complete(Ok(()));
                            if let Some(sent) = sent {
                                let mut receipt = agentdocker_core::Envelope::new(
                                    agentdocker_core::HUMAN,
                                    agentdocker_core::Destination::Channel(id.into()),
                                    "message",
                                    serde_json::Value::String(sent),
                                    None,
                                    Utc::now(),
                                );
                                receipt.id = message;
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
                }
            }
        }
        // Nothing is asked of a daemon that is not there: each request
        // would wait out its start timeout, and the queue would outrun the
        // worker. Coming back re-reads everything anyway.
        if self.connected.is_ok() && self.last_refresh.elapsed() >= REFRESH {
            self.last_refresh = Instant::now();
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
            | EventKind::AgentVcsChanged { .. } => self.send(Cmd::Agents),
            EventKind::InputDeliveryReported { .. } => {
                self.send(Cmd::Agents);
                self.send(Cmd::Activity);
            }
            EventKind::InboxAcknowledged { .. } => self.send(Cmd::Activity),
            EventKind::AgentActivityReported { .. } => {
                self.send(Cmd::Agents);
                self.send(Cmd::Activity);
            }
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
            .map(|a| a.spec.name.clone())
            .unwrap_or_else(|| id.chars().take(12).collect())
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
    fn request_channels(&mut self, id: String) {
        let selector = self.project_selector(&id);
        self.send(Cmd::Channels(id, selector));
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

impl App {}

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
                        Cmd::SessionSend(id, _) => Some(id.clone()),
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
        Cmd::Launch(spec) => match client.call(&Request::Run { spec: *spec })? {
            Response::Agent { agent } => Some(Msg::Launched(Ok(agent.id.to_string()))),
            _ => Some(Msg::Launched(Err("Unexpected launch response".into()))),
        },
        Cmd::SessionSend(agent, text) => {
            let response = client.call(&Request::Send {
                from: agentdocker_core::HUMAN.into(),
                to: agent.clone(),
                kind: "message".into(),
                payload: serde_json::Value::String(text),
                reply_to: None,
            })?;
            let result = match response {
                Response::Sent { message, .. } => Ok(message),
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
            })?;
            let result = match response {
                Response::Sent { message, .. } => Ok(message),
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

#[cfg(test)]
mod tests {
    use super::*;

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
                    Ok(MessageId::from(format!("sent-{index}"))),
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
        app.answers.insert(id.clone(), "unfinished".into());
        app.dismissing.insert(id.clone());
        messages
            .send(Msg::MessagesDismissed(
                vec![id.clone()],
                Err(error.to_string()),
            ))
            .unwrap();
        app.drain();
        assert_eq!(app.inbox, vec![envelope]);
        assert_eq!(app.answers[&id], "unfinished");
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
        app.answers
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
        assert_eq!(app.answers[&question], "unfinished answer");
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
        app.answers
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
        assert_eq!(app.answers[&question], "unfinished answer");
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
        app.answers.insert(id.clone(), "draft".into());
        app.sending.insert(id.clone());
        app.send(Cmd::Answer(id.clone(), "draft".into()));
        assert_eq!(app.answers[&id], "draft");
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
        app.answers.insert(id.clone(), "draft".into());
        app.sending.insert(id.clone());
        app.send(Cmd::Answer(id.clone(), "draft".into()));
        assert_eq!(app.answers[&id], "draft");
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
        app.answers.insert(id.clone(), "draft".into());
        app.sending.insert(id.clone());
        for message in replies {
            messages.send(message).unwrap();
        }
        app.drain();
        assert_eq!(app.answers[&id], "draft");
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

    #[cfg(unix)]
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
        assert!(matches!(requests.recv(), Ok(Cmd::SessionLog(target)) if target == id));
        messages
            .send(Msg::Activity(vec![AgentActivity {
                agent: agent.id.clone(),
                name: agent.spec.name.clone(),
                project: None,
                activity: Activity::Finished,
                queued_inputs: Some(2),
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
    fn failed_answer_reports_transport_failure_and_keeps_the_draft() {
        let id = MessageId::from("owned-question".to_owned());
        let result = disconnected_command(Cmd::Answer(id.clone(), "draft answer".into()));
        let (commands, _) = queue::channel();
        let (messages, results) = sync_channel(MESSAGE_CAPACITY);
        let mut app = App::bare(commands, results);
        app.answers.insert(id.clone(), "draft answer".into());
        app.sending.insert(id.clone());
        for message in result {
            messages.send(message).unwrap();
        }
        app.drain();
        assert_eq!(app.answers[&id], "draft answer");
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
        app.answers.insert(id.clone(), answer.clone());
        app.sending.insert(id.clone());
        app.send(Cmd::Answer(id.clone(), answer.clone()));
        assert_eq!(app.answers[&id], answer);
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
        assert_eq!(received.len(), 9);
        assert!(received.iter().any(|cmd| matches!(cmd, Cmd::Me)));
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
}
