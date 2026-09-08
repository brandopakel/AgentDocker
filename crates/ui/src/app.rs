//! The window: what it shows, how it asks the daemon, and how it keeps
//! up. Requests run on a worker thread and the event stream on another;
//! both hand results to the UI thread through a channel and ask for a
//! repaint, so the window never blocks on the socket.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};

mod queue;
use queue::{Receiver as CommandReceiver, Sender as CommandSender};
use std::time::{Duration, Instant};

use agentdocker_core::journal::ago;
use agentdocker_core::runtime::Wiring;
use agentdocker_core::{
    Activity, AgentActivity, AgentRecord, DiscoveredProcess, Event, EventKind, JournalEntry, Lease,
    MessageId, ProjectRef, Question, Request, Response, RuntimeInfo,
};
use anyhow::Context;
use chrono::{DateTime, Utc};
use egui::{Color32, RichText};

use crate::client::{Client, RemoteError};
use crate::terminal::{Status, Terminal};
use crate::theme::{ABSENT, UNVERIFIED, WIRED};

/// How often agents, leases and discovered processes are re-read.
const REFRESH: Duration = Duration::from_secs(2);
/// How often the runtime inventory is re-read (it asks each CLI).
const RUNTIMES_REFRESH: Duration = Duration::from_secs(30);
const JOURNAL_WINDOW: usize = 200;
const CONSOLE_BYTES: usize = 256 * 1024;
const MESSAGE_CAPACITY: usize = 64;
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

/// The terminal's frame inset. Named because the grid is sized against
/// it: a screen that measures the room it has, then draws inside a
/// border it forgot to subtract, is a screen that overflows.
const TERMINAL_MARGIN: i8 = 6;

/// One accent, used for the selected thing and nothing else. Taken from
/// the blue the project palette starts at, so the window has one blue
/// rather than two that nearly match.
const ACCENT: Color32 = Color32::from_rgb(0x2F, 0x6F, 0xED);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Screen {
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

impl Screen {
    const ALL: [Screen; 10] = [
        Screen::Agents,
        Screen::Questions,
        Screen::Channels,
        Screen::Terminal,
        Screen::Console,
        Screen::Runtimes,
        Screen::Journal,
        Screen::Leases,
        Screen::Settings,
        Screen::Desktop,
    ];

    fn title(self) -> &'static str {
        match self {
            Screen::Agents => "Agents",
            Screen::Questions => "Questions",
            Screen::Channels => "Channels",
            Screen::Terminal => "Terminal",
            Screen::Console => "Console",
            Screen::Runtimes => "Runtimes",
            Screen::Journal => "Journal",
            Screen::Leases => "Leases",
            Screen::Settings => "Settings",
            Screen::Desktop => "Installation",
        }
    }
}

/// What the worker is asked to do.
#[derive(Debug)]
enum Cmd {
    Agents,
    Leases,
    Runtimes,
    Discovered,
    Journal(String),
    Channels(String),
    Inbox,
    Activity,
    /// Register the person at the keyboard, so agents can address them.
    Me,
    Questions,
    Answer(MessageId, String),
    Adopt(u32),
    AdoptAll,
    Stop(String),
    Setup(Vec<String>),
    Desktop(Vec<String>),
    /// Any `agentdocker` command, so the window is not limited to the
    /// few actions that have buttons.
    Console(String),
}

/// What comes back to the window.
enum Msg {
    Agents(Vec<AgentRecord>),
    Leases(Vec<Lease>),
    Runtimes(Vec<RuntimeInfo>),
    Discovered(Vec<DiscoveredProcess>),
    Journal(String, Option<u64>, Vec<JournalEntry>),
    Channels(String, Vec<agentdocker_core::Channel>),
    Inbox(Vec<agentdocker_core::Envelope>),
    Activity(Vec<AgentActivity>),
    Questions(Vec<Question>),
    /// An answer came back: `Ok` means it was delivered, `Err` carries
    /// why it was not, so what the person typed is not thrown away.
    Answered(MessageId, Result<(), String>),
    Event(Box<Event>),
    Connected,
    Disconnected(String),
    Status(String),
    Setup(Result<serde_json::Value, String>),
    Desktop(Result<serde_json::Value, String>),
    Console(String),
}

pub struct App {
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
    console_focused: bool,
    /// What the reader has chosen about how this looks, and where it is
    /// kept between runs.
    settings: crate::theme::Settings,
    home: std::path::PathBuf,
    /// Applied once per change rather than every frame: setting fonts
    /// rebuilds egui's atlas, which is not something to do at 60Hz.
    applied: Option<crate::theme::Settings>,
    /// The mark beside the title, uploaded to the GPU once and kept.
    mark: Option<egui::TextureHandle>,
    /// Questions put to the human, and what is being typed in reply to
    /// each. The draft is keyed by message id so answering one question
    /// does not disturb another half-written answer.
    questions: Vec<Question>,
    answers: BTreeMap<MessageId, String>,
    /// What each agent is doing, keyed by id. Derived by the daemon, so
    /// it is read rather than computed here.
    activity: BTreeMap<String, Activity>,
    /// Show only this project, by id. `None` is everything, which is the
    /// right default: the window exists to show a fleet.
    focus: Option<String>,
    /// Answers on their way to the daemon, so the same one is not sent
    /// twice while it is in flight.
    sending: std::collections::BTreeSet<MessageId>,
}

impl App {
    pub fn with_smoke(mut self, smoke: Option<crate::smoke::Smoke>) -> Self {
        self.smoke = smoke;
        self
    }

    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
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
            cc.egui_ctx.clone(),
            worker_stop.clone(),
        );
        spawn_events(client.clone(), msg_tx, cc.egui_ctx.clone());
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
        Self {
            worker_stop,
            desktop: Default::default(),
            tx: cmd_tx,
            rx: msg_rx,
            screen: Screen::Agents,
            agents: Vec::new(),
            leases: Vec::new(),
            runtimes: Vec::new(),
            discovered: Vec::new(),
            journal: Vec::new(),
            journal_project: None,
            channels: Vec::new(),
            inbox: Vec::new(),
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
            console_focused: false,
            settings: crate::theme::Settings::load(&home),
            home,
            applied: None,
            mark: None,
            questions: Vec::new(),
            answers: BTreeMap::new(),
            sending: std::collections::BTreeSet::new(),
            activity: BTreeMap::new(),
            focus: None,
        }
    }

    /// The window's state without a window or a daemon, for tests.
    #[cfg(test)]
    fn bare(tx: CommandSender, rx: Receiver<Msg>) -> Self {
        Self {
            worker_stop: Default::default(),
            desktop: Default::default(),
            tx,
            rx,
            screen: Screen::Agents,
            agents: Vec::new(),
            leases: Vec::new(),
            runtimes: Vec::new(),
            discovered: Vec::new(),
            journal: Vec::new(),
            journal_project: None,
            channels: Vec::new(),
            inbox: Vec::new(),
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
            console_focused: false,
            settings: crate::theme::Settings::default(),
            home: std::path::PathBuf::new(),
            applied: None,
            mark: None,
            questions: Vec::new(),
            answers: BTreeMap::new(),
            sending: std::collections::BTreeSet::new(),
            activity: BTreeMap::new(),
            focus: None,
        }
    }

    fn send(&mut self, cmd: Cmd) {
        if let Err(queue::Rejected { command, reason }) = self.tx.send(cmd) {
            match command {
                Cmd::Answer(id, _) => {
                    self.sending.remove(&id);
                }
                Cmd::Setup(_) => self.setup_busy = false,
                Cmd::Desktop(_) => self.desktop.receive(Err(reason.into())),
                Cmd::Console(_) => {
                    self.console_running = self.console_running.saturating_sub(1);
                    self.append_console(&format!("Command not queued: {reason}\n"));
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
                Msg::Agents(agents) => {
                    self.agents = agents;
                    let projects = self.channel_projects();
                    self.channels
                        .retain(|channel| projects.contains(&channel.project.to_string()));
                    for project in projects {
                        self.send(Cmd::Channels(project));
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
                Msg::Activity(activity) => {
                    self.activity = activity
                        .into_iter()
                        .map(|a| (a.agent.to_string(), a.activity))
                        .collect();
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
                            self.say("answered");
                        }
                        // The draft stays exactly where it was, so nothing
                        // typed is lost to a daemon that was not listening.
                        Err(reason) => self.say(reason),
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
                            self.send(Cmd::Journal(project.clone()));
                        }
                    }
                }
                Msg::Disconnected(reason) => self.connected = Err(reason),
                Msg::Status(text) => self.say(text),
                Msg::Desktop(result) => self.desktop.receive(result),
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
                        Err(error) => self.say(error),
                    }
                }
                Msg::Console(text) => {
                    self.console_running = self.console_running.saturating_sub(1);
                    // Appended, not replaced: a terminal keeps what it
                    // said, and the command that produced this is
                    // already above it.
                    self.append_console(text.trim_end());
                    self.append_console("\n");
                    self.screen = Screen::Console;
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
        if self.journal_project.is_none()
            && let Some((id, _)) = self.projects().into_iter().next()
        {
            self.journal_project = Some(id.clone());
            self.send(Cmd::Journal(id));
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
            | EventKind::AgentVcsChanged { .. } => self.send(Cmd::Agents),
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

    /// The projects agents work in: (id, name), by name.
    fn projects(&self) -> Vec<(String, String)> {
        let mut seen: BTreeMap<String, String> = BTreeMap::new();
        for project in self.agents.iter().filter_map(|a| a.project.as_ref()) {
            seen.entry(project.id().as_str().to_owned())
                .or_insert_with(|| project.name());
        }
        let mut list: Vec<(String, String)> = seen.into_iter().collect();
        list.sort_by(|a, b| a.1.cmp(&b.1));
        list
    }

    /// The project an agent works in: its id and its name.
    fn project_of(&self, agent: &str) -> Option<(String, String)> {
        self.agents
            .iter()
            .find(|a| a.id.as_str() == agent)
            .and_then(|a| a.project.as_ref())
            .map(|p| (p.id().as_str().to_owned(), p.name()))
    }

    /// A project as a dot and its name, in its own colour. Used wherever
    /// rows from several projects are mixed together, because there the
    /// colour alone asks the reader to remember which is which.
    fn project_label(&self, ui: &mut egui::Ui, agent: &str) {
        ui.horizontal(|ui| match self.project_of(agent) {
            Some((id, name)) => {
                crate::projects::dot(ui, &id);
                ui.label(RichText::new(name).color(crate::projects::colour(&id)));
            }
            None => {
                ui.label(RichText::new("—").weak());
            }
        });
    }

    /// Whether a row belonging to this agent survives the project filter.
    fn in_focus(&self, agent: &str) -> bool {
        match &self.focus {
            None => true,
            Some(only) => self.project_of(agent).is_some_and(|(id, _)| id == *only),
        }
    }

    /// Runtimes whose configuration needs review. This is a setup hint,
    /// not evidence about an individual session's current activity.
    fn unwired(&self) -> std::collections::BTreeSet<String> {
        self.runtimes
            .iter()
            .filter(|r| r.mcp.needs_review() || r.hooks.needs_review())
            .map(|r| r.name.clone())
            .collect()
    }

    fn name_of(&self, id: &str) -> String {
        self.agents
            .iter()
            .find(|a| a.id.as_str() == id)
            .map(|a| a.spec.name.clone())
            .unwrap_or_else(|| id.chars().take(12).collect())
    }

    // ----- screens -------------------------------------------------------

    fn agents_screen(&mut self, ui: &mut egui::Ui) {
        let now = Utc::now();
        let unwired = self.unwired();
        // Grouped by project id, not by name: two checkouts of one
        // repository share a name, and the id is what actually says they
        // are the same work.
        let mut groups: BTreeMap<(String, String), Vec<&AgentRecord>> = BTreeMap::new();
        for agent in self.agents.iter().filter(|a| a.status.is_live()) {
            let key = match &agent.project {
                Some(project) => (project.name(), project.id().as_str().to_owned()),
                None => ("no project".to_owned(), String::new()),
            };
            groups.entry(key).or_default().push(agent);
        }
        let mut stop: Option<String> = None;
        let mut arm: Option<String> = None;
        let mut attach: Option<String> = None;
        // An arming that has gone stale is no arming at all: a Stop
        // clicked once and left alone must not be completable by a click
        // that lands minutes later on a row that has since moved.
        let confirming = self
            .confirm_stop
            .as_ref()
            .filter(|(_, at)| at.elapsed() < CONFIRM_WITHIN)
            .map(|(id, _)| id.clone());
        if groups.is_empty() {
            ui.label(
                RichText::new("No live agents. Adopt one below, or `agentdocker run`.").weak(),
            );
        }
        // One grid for every project rather than one each, so the columns
        // line up down the whole screen. Separate grids size themselves
        // independently, and the result reads as several tables that
        // happen to be stacked.
        egui::Grid::new("agents")
            .striped(true)
            .num_columns(8)
            .show(ui, |ui| {
                // Headers once, at the top. Repeating them per project
                // was noise: the columns are shared now, so the reader
                // only needs telling what they are once.
                for header in [
                    "NAME", "RUNTIME", "DOING", "BRANCH", "LEASES", "SEEN", "", "",
                ] {
                    ui.label(RichText::new(header).strong());
                }
                ui.end_row();
                for ((project, project_id), agents) in &groups {
                    if self.focus.as_ref().is_some_and(|only| only != project_id) {
                        continue;
                    }
                    // The project's own row: its colour, its name, and
                    // what its agents are doing, so a glance answers "is
                    // anything stuck here?" without reading the rows.
                    ui.horizontal(|ui| {
                        crate::projects::dot(ui, project_id);
                        ui.label(
                            RichText::new(project)
                                .heading()
                                .color(crate::projects::colour(project_id)),
                        );
                    });
                    let blocked = agents
                        .iter()
                        .filter(|a| {
                            matches!(
                                self.activity.get(a.id.as_str()),
                                Some(Activity::Blocked { .. })
                            )
                        })
                        .count();
                    let working = agents
                        .iter()
                        .filter(|a| {
                            matches!(
                                self.activity.get(a.id.as_str()),
                                Some(Activity::Working { .. })
                            )
                        })
                        .count();
                    ui.label(
                        RichText::new(format!(
                            "{} agent{}",
                            agents.len(),
                            if agents.len() == 1 { "" } else { "s" }
                        ))
                        .weak(),
                    );
                    // What the project is doing, in the words a reader
                    // would use: "1 blocked" is worth colour, "all idle"
                    // is worth saying, and "0 working" is neither.
                    let unheard = agents
                        .iter()
                        .filter(|agent| matches!(self.activity.get(agent.id.as_str()), None | Some(Activity::Unknown)))
                        .count();
                    if blocked > 0 {
                        ui.label(RichText::new(format!("{blocked} blocked")).color(UNVERIFIED));
                    } else if working > 0 && working == agents.len() {
                        ui.label(RichText::new("all working").color(WIRED));
                    } else if working > 0 {
                        ui.label(RichText::new(format!("{working} working")).color(WIRED));
                    } else if unheard == agents.len() {
                        ui.label(RichText::new("activity unknown").color(UNVERIFIED));
                    } else if unheard > 0 {
                        ui.label(
                            RichText::new(format!("{unheard} unknown")).color(UNVERIFIED),
                        );
                    } else if agents.iter().all(|a| matches!(self.activity.get(a.id.as_str()), Some(Activity::Idle { .. }))) {
                        ui.label(RichText::new("all idle").weak());
                    } else {
                        ui.label(RichText::new("activity unknown").weak());
                    }
                    ui.end_row();

                    for agent in agents {
                        // The dot repeats the project's colour on every
                        // row, so a row read on its own still says whose
                        // it is.
                        ui.horizontal(|ui| {
                            crate::projects::dot(ui, project_id);
                            ui.label(&agent.spec.name);
                        });
                        if agent.spec.runtime == "human" {
                            ui.label(RichText::new("you").italics()).on_hover_text(
                                "The person at this keyboard, registered like any other \
                                     agent so the others can put questions to you and hold \
                                     leases against you.",
                            );
                        } else {
                            ui.label(&agent.spec.runtime);
                        }
                        // What it is doing, not merely that its process
                        // exists: blocked agents say what by.
                        match self.activity.get(agent.id.as_str()) {
                            Some(Activity::Blocked {
                                resource, held_by, ..
                            }) => {
                                ui.label(
                                    RichText::new(format!(
                                        "blocked on {resource}{}",
                                        held_by
                                            .first()
                                            .map(|h| format!(" ({})", self.name_of(h.as_str())))
                                            .unwrap_or_default()
                                    ))
                                    .color(UNVERIFIED),
                                )
                                .on_hover_text("Waiting for a lease another agent holds.");
                            }
                            Some(Activity::Working { .. }) => {
                                ui.label(RichText::new("working").color(WIRED)).on_hover_text(
                                    "Recent coordination or an explicit provider activity report; not inferred from CPU use or terminal output.",
                                );
                            }
                            None | Some(Activity::Unknown) => {
                                let label = ui.label(RichText::new("unknown").color(UNVERIFIED));
                                if unwired.contains(&agent.spec.runtime) {
                                    label.on_hover_text("No fresh activity report. Check this integration in Runtimes; the process may still be working.");
                                } else {
                                    label.on_hover_text("No fresh activity report. Configuration alone does not prove this session is connected or idle.");
                                }
                            }
                            Some(Activity::Idle { .. }) => {
                                ui.label(RichText::new("idle").weak()).on_hover_text(
                                    "The provider recently reported a turn ending or being interrupted. This observation expires without another report.",
                                );
                            }
                            Some(other) => {
                                ui.label(RichText::new(other.label()).weak());
                            }
                        }
                        ui.label(
                            agent
                                .vcs
                                .as_ref()
                                .map(|v| v.describe())
                                .unwrap_or_else(|| "-".to_owned()),
                        );
                        let held = self.leases.iter().filter(|l| l.holder == agent.id).count();
                        ui.label(held.to_string());
                        ui.label(ago(now, agent.last_seen));
                        let armed = confirming.as_deref() == Some(agent.id.as_str());
                        let label = if armed {
                            RichText::new("Stop?").color(ABSENT)
                        } else {
                            RichText::new("Stop")
                        };
                        if ui
                            .add(egui::Button::new(label).small())
                            .on_hover_text(if armed {
                                "Click again to stop it."
                            } else {
                                "Stop this agent's process. Asks once first."
                            })
                            .clicked()
                        {
                            if armed {
                                stop = Some(agent.id.to_string());
                            } else {
                                arm = Some(agent.id.to_string());
                            }
                        }
                        if agent.spec.tty
                            && ui
                                .small_button("Attach")
                                .on_hover_text("Open this agent's terminal in the Terminal screen.")
                                .clicked()
                        {
                            attach = Some(agent.id.to_string());
                        }
                        ui.end_row();
                    }
                    // A blank row between projects: the striping alone
                    // does not separate them once they share a grid.
                    for _ in 0..8 {
                        ui.label("");
                    }
                    ui.end_row();
                }
            });
        if let Some(id) = arm {
            self.confirm_stop = Some((id, Instant::now()));
        }
        if let Some(id) = stop {
            self.confirm_stop = None;
            self.send(Cmd::Stop(id));
        }
        if let Some(id) = attach {
            self.attach(id, ui.ctx().clone());
            self.screen = Screen::Terminal;
        }
        ui.separator();
        ui.heading("Running, not registered");
        if self.discovered.is_empty() {
            ui.label(RichText::new("Nothing found.").weak());
        } else {
            let mut adopt: Option<u32> = None;
            let mut adopt_all = false;
            egui::Grid::new("discovered")
                .striped(true)
                .num_columns(5)
                .show(ui, |ui| {
                    for header in ["PID", "RUNTIME", "PROJECT", "STARTED", ""] {
                        ui.label(RichText::new(header).strong());
                    }
                    ui.end_row();
                    for process in &self.discovered {
                        ui.label(process.pid.to_string());
                        ui.label(&process.runtime);
                        ui.label(
                            process
                                .project
                                .as_ref()
                                .map(ProjectRef::name)
                                .unwrap_or_else(|| "-".to_owned()),
                        );
                        ui.label(
                            process
                                .started_at
                                .map(|at| ago(now, at))
                                .unwrap_or_else(|| "-".to_owned()),
                        );
                        if ui
                            .small_button("Adopt")
                            .on_hover_text(
                                "Register this process with the daemon so it can be messaged, \
                                 hold leases, and appear above. The process is not restarted.",
                            )
                            .clicked()
                        {
                            adopt = Some(process.pid);
                        }
                        ui.end_row();
                    }
                });
            if ui
                .button("Adopt all")
                .on_hover_text("Register every process listed here. None of them is restarted.")
                .clicked()
            {
                adopt_all = true;
            }
            if let Some(pid) = adopt {
                self.send(Cmd::Adopt(pid));
            }
            if adopt_all {
                self.send(Cmd::AdoptAll);
            }
        }
    }

    /// An agent's terminal, or the list of agents that have one.
    fn terminal_screen(&mut self, ui: &mut egui::Ui) {
        let palette = self.settings.palette();
        let size = self.settings.terminal_size;
        // Measured, not assumed. This was a constant for a 13pt cell,
        // which was true until the font size became something the
        // reader chooses: at 20pt the real cell is half again as wide,
        // so a grid computed from the constant is half again too wide
        // and the agent lays itself out past the edge of the window.
        let font = egui::FontId::monospace(size);
        // Ten of them and divide: one glyph's laid-out width carries a
        // rounding error that a whole row of cells multiplies up.
        let ruler =
            ui.painter()
                .layout_no_wrap("MMMMMMMMMM".to_owned(), font.clone(), Color32::WHITE);
        let cell = (ruler.rect.width() / 10.0, ruler.rect.height());
        let Some(terminal) = &mut self.terminal else {
            let attachable: Vec<&AgentRecord> = self
                .agents
                .iter()
                .filter(|a| a.status.is_live() && a.spec.tty)
                .collect();
            if attachable.is_empty() {
                ui.label(
                    RichText::new(
                        "No agent has a terminal. Start one with \
                         `agentdocker run --tty -- <command>`, or `tty = true` in an Agentfile \
                         entry.",
                    )
                    .weak(),
                );
                return;
            }
            let mut attach: Option<String> = None;
            for agent in attachable {
                ui.horizontal(|ui| {
                    ui.label(&agent.spec.name);
                    if ui
                        .button("Attach")
                        .on_hover_text(
                            "Take over this agent's terminal. It keeps running either way.",
                        )
                        .clicked()
                    {
                        attach = Some(agent.id.to_string());
                    }
                });
            }
            if let Some(agent) = attach {
                self.attach(agent, ui.ctx().clone());
            }
            return;
        };

        let mut detach = false;
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("attached to {}", terminal.agent)).strong());
            match terminal.status() {
                Status::Attached => {
                    crate::projects::bullet(ui, WIRED);
                    ui.label(RichText::new("live").color(WIRED))
                        .on_hover_text("Keys typed on this screen go to the agent.");
                }
                Status::Ended(reason) => {
                    crate::projects::bullet(ui, Color32::GRAY);
                    ui.label(RichText::new(reason).color(Color32::GRAY));
                }
            }
            if terminal.scrolled_back() {
                ui.label(RichText::new("· scrolled back").weak());
                if ui
                    .button("Jump to live")
                    .on_hover_text("Back to the bottom of the scrollback.")
                    .clicked()
                {
                    terminal.scroll(i32::MIN / 2);
                }
            }
            if ui
                .button("Detach")
                .on_hover_text("Stop watching this terminal. The agent is not stopped.")
                .clicked()
            {
                detach = true;
            }
        });
        ui.separator();

        // The room inside the frame, not the room the frame is given:
        // its margin comes off both axes before the grid is worked out,
        // or the last column and the last row land under the border.
        let inset = TERMINAL_MARGIN as f32 * 2.0;
        let space = (ui.available_size() - egui::vec2(inset, inset)).max(egui::Vec2::ZERO);
        terminal.resize(
            (space.x / cell.0).floor().max(1.0) as u16,
            (space.y / cell.1).floor().max(1.0) as u16,
        );
        // Everything typed while this screen is up goes to the agent, and
        // the wheel moves through the history the parser kept rather than
        // through a scroll area that only ever holds one screen.
        let (events, wheel) = ui.input(|i| (i.events.clone(), i.smooth_scroll_delta.y));
        if terminal.status() == Status::Attached {
            let bytes = crate::terminal::keystrokes(&events);
            if !bytes.is_empty() {
                terminal.send(bytes);
            }
        }
        if wheel.abs() >= 1.0 {
            terminal.scroll((wheel / cell.1).round() as i32);
        }
        // On the palette's own ground, so the agent's terminal and the
        // console are visibly the same surface.
        egui::Frame::new()
            .fill(palette.ground)
            .inner_margin(egui::Margin::same(TERMINAL_MARGIN))
            .corner_radius(6)
            .show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                egui::ScrollArea::horizontal().show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    terminal.ui(ui, palette, size);
                });
            });
        if detach {
            self.terminal = None;
        }
    }

    fn attach(&mut self, agent: String, ctx: egui::Context) {
        if let Some(client) = &self.client {
            self.terminal = Some(Terminal::attach(client.clone(), agent, ctx));
        }
    }

    /// What agents are waiting on the person at the keyboard.
    ///
    /// An agent that asks a question is blocked until it is answered, so
    /// this screen is the one part of the window where doing nothing has
    /// a cost. The question and the answer box sit together: reading it
    /// and replying to it should not be two places.
    fn questions_screen(&mut self, ui: &mut egui::Ui) {
        if self.questions.is_empty() {
            ui.label(RichText::new("Nothing is waiting on you.").weak());
            return;
        }
        let mut answered: Option<(MessageId, String)> = None;
        let questions = self.questions.clone();
        for question in &questions {
            let in_flight = self.sending.contains(&question.id);
            let left = (question.expires_at - Utc::now()).num_seconds().max(0);
            ui.group(|ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(self.name_of(&question.from)).strong());
                    // Which project is asking: the same question text can
                    // mean different things in different repositories.
                    if let Some((id, name)) = self.project_of(&question.from) {
                        ui.label(
                            RichText::new(format!("· {name}")).color(crate::projects::colour(&id)),
                        );
                    }
                    ui.label(RichText::new(format!("· {} left", span(left))).weak());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new(question.id.to_string()).weak().monospace());
                    });
                });
                ui.add_space(2.0);
                ui.label(&question.text);
                ui.add_space(4.0);
                let draft = self.answers.entry(question.id.clone()).or_default();
                let mut send = false;
                let entry = ui.add(
                    egui::TextEdit::singleline(draft)
                        .desired_width(f32::INFINITY)
                        .hint_text("your answer"),
                );
                if entry.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    send = true;
                }
                // Enabled only when there is something to send. It used
                // to accept the click and drop it, which is the same to
                // the person clicking as a button that does not work.
                let ready = !draft.trim().is_empty();
                if ui
                    .add_enabled(!in_flight && ready, egui::Button::new("Answer"))
                    .on_disabled_hover_text(if in_flight {
                        "Waiting for the daemon to take the last answer."
                    } else {
                        "Type an answer first."
                    })
                    .clicked()
                {
                    send = true;
                }
                if send && !in_flight && !draft.trim().is_empty() {
                    answered = Some((question.id.clone(), draft.clone()));
                }
            });
            ui.add_space(4.0);
        }
        if let Some((id, text)) = answered {
            // The draft and the question both stay until the daemon says
            // it took the answer. Dropping them first would lose what was
            // typed if the send failed.
            self.sending.insert(id.clone());
            self.send(Cmd::Answer(id, text));
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
            .map(|project| project.id().to_string())
            .collect()
    }

    fn channels_screen(&mut self, ui: &mut egui::Ui) {
        let now = Utc::now();
        if self.channels.is_empty() {
            ui.label(
                RichText::new(
                    "No channels. One opens when two checkouts change the same path, or when \
                     an agent opens one for a task.",
                )
                .weak(),
            );
        } else {
            ui.label(
                RichText::new(
                    "Open channels across your active projects. Messages shown are queued for \
                     you — an inbox is not a transcript, and nothing is drained to draw this.",
                )
                .weak()
                .small(),
            );
        }
        let (said, direct) = by_room(&self.inbox);
        let me = self
            .agents
            .iter()
            .find(|a| a.spec.runtime == agentdocker_core::HUMAN_RUNTIME)
            .map(|a| a.id.clone());
        for channel in &self.channels {
            let id = channel.id.as_str().to_owned();
            let mine = me.as_ref().is_some_and(|me| channel.has(me));
            ui.group(|ui| {
                ui.horizontal(|ui| {
                    crate::projects::bullet(
                        ui,
                        if channel.is_open() {
                            WIRED
                        } else {
                            ui.visuals().weak_text_color()
                        },
                    );
                    ui.label(RichText::new(channel.title()).strong());
                    ui.label(RichText::new(&id).weak().small());
                    if !mine {
                        ui.label(RichText::new("· not a member").weak().small())
                            .on_hover_text(
                                "Agents opened this between themselves. It is listed because it \
                                 is part of this project's work, not because anything from it \
                                 reaches you.",
                            );
                    }
                    ui.label(
                        RichText::new(format!(
                            "· {} member{}",
                            channel.members.len(),
                            if channel.members.len() == 1 { "" } else { "s" }
                        ))
                        .weak(),
                    );
                    ui.label(RichText::new(format!("· {}", ago(now, channel.opened_at))).weak());
                });
                ui.label(
                    RichText::new(
                        channel
                            .members
                            .iter()
                            .map(|m| self.name_of(m.as_str()))
                            .collect::<Vec<_>>()
                            .join(", "),
                    )
                    .weak()
                    .small(),
                );
                // A verdict is the point of a room like this, so it is
                // not buried in the transcript with everything else.
                for review in &channel.reviews {
                    use agentdocker_core::channel::Verdict;
                    let (word, colour) = match review.verdict {
                        Verdict::Approve => ("approved", WIRED),
                        Verdict::Changes => ("changes requested", UNVERIFIED),
                        Verdict::Comment => ("commented", ui.visuals().text_color()),
                    };
                    ui.label(
                        RichText::new(format!(
                            "{word} by {} on {}'s work",
                            review.by_name, review.of_name
                        ))
                        .color(colour),
                    )
                    .on_hover_text(&review.note);
                }
                if let Some(why) = &channel.resolution {
                    ui.label(RichText::new(format!("closed: {why}")).weak());
                }
                ui.add_space(2.0);
                // What can honestly be shown here is this person's own
                // undelivered queue, and only where they are a member.
                // An inbox is not a transcript: a message an agent has
                // taken is gone from it, so what is missing below is not
                // silence — it is everything that was already read.
                // Durable history needs somewhere to keep it, and that
                // does not exist yet.
                match said.get(&id) {
                    Some(messages) => {
                        for message in messages.iter().rev().take(20).rev() {
                            said_by(ui, now, &self.name_of(&message.from), message);
                        }
                        ui.label(
                            RichText::new(
                                "Queued for you, not the room's history — anything already \
                                 delivered has left the queue.",
                            )
                            .weak()
                            .small(),
                        );
                    }
                    None if mine => {
                        ui.label(
                            RichText::new("Nothing waiting for you here.")
                                .weak()
                                .small(),
                        );
                    }
                    // Not a member, so none of it was ever going to
                    // arrive. Saying "nothing said here" would be a
                    // claim about the room; this is a claim about us.
                    None => {
                        ui.label(
                            RichText::new(
                                "You are not in this room, so nothing reaches you from it.",
                            )
                            .weak()
                            .small(),
                        );
                    }
                }
            });
            ui.add_space(4.0);
        }
        if !direct.is_empty() {
            ui.add_space(8.0);
            ui.label(RichText::new("Sent to you directly").strong());
            for message in direct.iter().rev().take(30).rev() {
                said_by(ui, now, &self.name_of(&message.from), message);
            }
        }
    }

    /// Any command the CLI has, run from the window.
    ///
    /// Not a shell, and not a second system terminal — the machine has
    /// one of those and being another is somebody else's job. What it
    /// takes from the terminal is the feel: the same monospace on the
    /// same dark ground, a prompt that stays at the foot of a
    /// transcript, the last commands on the up arrow, and output that
    /// accumulates instead of a box that is replaced.
    fn console_screen(&mut self, ui: &mut egui::Ui) {
        if self.console_truncated {
            ui.label("Earlier output omitted. Showing the latest 256 KiB.");
        }
        let mut run = false;
        let palette = self.settings.palette();
        let font = egui::FontId::monospace(self.settings.terminal_size);
        egui::Frame::new()
            .fill(palette.ground)
            .inner_margin(egui::Margin::same(10))
            .corner_radius(6)
            .show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                ui.spacing_mut().item_spacing.y = 2.0;
                // Laid out from the bottom, so the prompt sits on the
                // floor of the panel the way a shell's does and the
                // transcript grows down towards it.
                ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new("agentdocker")
                                .font(font.clone())
                                .color(palette.accent),
                        );
                        let entry = ui.add(
                            egui::TextEdit::singleline(&mut self.console_input)
                                .font(font.clone())
                                .text_color(palette.text)
                                // No box around it: the prompt is part of
                                // the transcript, not a field on top of it.
                                .frame(egui::Frame::NONE)
                                .desired_width(f32::INFINITY)
                                .hint_text(
                                    RichText::new("ps --all   ·   journal --new   ·   runtimes")
                                        .font(font.clone())
                                        .color(palette.dim()),
                                ),
                        );
                        // A console nobody has clicked into is a console
                        // that ignores what is typed at it.
                        if !self.console_focused {
                            entry.request_focus();
                            self.console_focused = true;
                        }
                        if entry.has_focus() {
                            let (up, down) = ui.input(|i| {
                                (
                                    i.key_pressed(egui::Key::ArrowUp),
                                    i.key_pressed(egui::Key::ArrowDown),
                                )
                            });
                            if up || down {
                                self.recall(up);
                            }
                        }
                        if entry.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            run = true;
                            self.console_focused = false;
                        }
                        // Something is still running and the transcript
                        // will not move until it answers. Said at the
                        // prompt, where the person is already looking.
                        if self.console_running > 0 {
                            ui.spinner();
                            ui.label(
                                RichText::new(match self.console_running {
                                    1 => "running…".to_owned(),
                                    many => format!("{many} running…"),
                                })
                                .font(font.clone())
                                .color(palette.dim()),
                            );
                        }
                    });
                    ui.add_space(4.0);
                    // The transcript reads downwards inside the room left over.
                    ui.with_layout(egui::Layout::top_down(egui::Align::LEFT), |ui| {
                        egui::ScrollArea::both()
                            .stick_to_bottom(true)
                            .show(ui, |ui| {
                                ui.set_min_width(ui.available_width());
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(self.console_output.trim_end())
                                            .font(font.clone())
                                            .color(palette.text),
                                    )
                                    .wrap_mode(egui::TextWrapMode::Extend),
                                );
                            });
                    });
                });
            });
        if run && !self.console_input.trim().is_empty() {
            let line = self.console_input.trim().to_owned();
            self.append_console(&format!("agentdocker {line}\n"));
            self.remember_console_command(&line);
            self.console_input.clear();
            self.console_running += 1;
            self.send(Cmd::Console(line));
        }
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

    /// One connection state, in a colour, saying on hover what it means
    /// and what would change it.
    ///
    /// The table printed the word on its own, which is how a reader ends
    /// up looking at "no" and asking what it means and how to clear it.
    /// The answer is two sentences, so it goes where two sentences fit.
    fn wiring_cell(ui: &mut egui::Ui, channel: &str, state: Wiring, dim: bool) {
        let (colour, meaning) = match state {
            Wiring::Wired => (
                WIRED,
                "AgentDocker is registered here. That the configuration says so is not proof a \
                 session has used it — start a fresh one to be sure.",
            ),
            Wiring::Missing => (
                ABSENT,
                "Nothing registers AgentDocker here yet. Review setup works out the change and \
                 shows it to you; nothing is written until you apply it.",
            ),
            Wiring::Unverified => (
                UNVERIFIED,
                "Something is registered under our name that is disabled, malformed, or runs a \
                 different command. Setup will not overwrite it — open the file and decide.",
            ),
            Wiring::Unsupported => (
                ui.visuals().weak_text_color(),
                "This runtime has no such channel, or AgentDocker has no adapter for it yet. \
                 Nothing to do here.",
            ),
        };
        let colour = if dim {
            colour.gamma_multiply(0.5)
        } else {
            colour
        };
        ui.label(RichText::new(state.symbol()).color(colour))
            .on_hover_text(format!("{channel}: {meaning}"));
    }

    fn runtimes_screen(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Agent tools on this machine.").weak());
        let mut setup: Option<String> = None;
        let mut sorted: Vec<&RuntimeInfo> = self.runtimes.iter().collect();
        sorted.sort_by_key(|r| !r.installed());
        egui::Grid::new("runtimes")
            .striped(true)
            .num_columns(8)
            .show(ui, |ui| {
                for header in [
                    "RUNTIME", "VENDOR", "CLI", "VERSION", "APP", "MCP", "HOOKS", "RUNNING",
                ] {
                    ui.label(RichText::new(header).strong());
                }
                ui.label("");
                ui.end_row();
                for runtime in sorted {
                    let dim = !runtime.installed();
                    let text = |s: String| {
                        if dim {
                            RichText::new(s).color(Color32::GRAY)
                        } else {
                            RichText::new(s)
                        }
                    };
                    ui.label(text(runtime.label.clone()));
                    ui.label(text(runtime.vendor.clone()));
                    ui.label(text(
                        runtime
                            .cli
                            .as_ref()
                            .map(|c| c.display().to_string())
                            .unwrap_or_else(|| "-".to_owned()),
                    ));
                    ui.label(text(
                        runtime.version.clone().unwrap_or_else(|| "-".to_owned()),
                    ));
                    let apps = runtime
                        .apps
                        .iter()
                        .map(|a| match &a.version {
                            Some(v) => format!("{} {v}", a.label),
                            None => a.label.clone(),
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    ui.label(text(if apps.is_empty() {
                        "-".to_owned()
                    } else {
                        apps
                    }));
                    Self::wiring_cell(ui, "MCP server", runtime.mcp, dim);
                    Self::wiring_cell(ui, "hooks", runtime.hooks, dim);
                    ui.label(text(runtime.running.to_string()))
                        .on_hover_text("Processes of this runtime that AgentDocker can see right now, registered or not.");
                    let needs_setup = runtime.installed()
                        && (runtime.mcp.needs_review() || runtime.hooks.needs_review());
                    if needs_setup
                        && ui
                            .add_enabled(
                                !self.setup_busy,
                                egui::Button::new("Review setup").small(),
                            )
                            .on_hover_text(
                                "Work out what would have to change to wire this runtime up, \
                                 and show it. Nothing is written until you apply it, and \
                                 anything applied can be undone.",
                            )
                            .clicked()
                    {
                        setup = Some(runtime.name.clone());
                    }
                    ui.end_row();
                }
            });
        if let Some(name) = setup {
            self.setup_busy = true;
            self.send(Cmd::Setup(vec![name, "--preview".into()]));
        }
        ui.add_space(8.0);
        if ui.button("Refresh").clicked() {
            self.send(Cmd::Runtimes);
        }
        self.setup_panel(ui);
    }

    fn setup_panel(&mut self, ui: &mut egui::Ui) {
        ui.separator();
        let mut selected = None;
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!self.setup_busy, egui::Button::new("Check connections"))
                .on_hover_text(
                    "Read every runtime's configuration and report what it says about \
                     AgentDocker. Reads only; nothing is changed.",
                )
                .clicked()
            {
                self.setup_busy = true;
                self.send(Cmd::Setup(vec!["--health".into()]));
            }
            if ui
                .add_enabled(!self.setup_busy, egui::Button::new("Saved setup plans"))
                .on_hover_text(
                    "Every plan previewed on this machine, newest first. Each keeps what the \
                     files said before it, which is what lets it be undone.",
                )
                .clicked()
            {
                self.setup_busy = true;
                self.send(Cmd::Setup(vec!["--list".into()]));
            }
            if self.setup_busy {
                ui.spinner();
                ui.label(RichText::new("working…").weak());
            }
        });
        if !self.setup_history.is_empty() {
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                for plan in &self.setup_history {
                    let id = plan["id"].as_str().unwrap_or("");
                    let phase = plan["phase"].as_str().unwrap_or("unknown");
                    if ui
                        .add_enabled(
                            !self.setup_busy,
                            egui::Button::new(
                                RichText::new(format!(
                                    "{phase} · {}",
                                    id.chars().take(8).collect::<String>()
                                ))
                                .color(phase_colour(ui, phase)),
                            ),
                        )
                        .on_hover_text(format!("Open plan {id}"))
                        .clicked()
                    {
                        selected = Some(id.to_owned());
                    }
                }
            });
        }
        if let Some(id) = selected {
            self.setup_busy = true;
            self.send(Cmd::Setup(vec!["--show".into(), id]));
        }
        if let Some(health) = &self.setup_health {
            ui.add_space(6.0);
            let reachable = health["daemon_reachable"].as_bool().unwrap_or(false);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Daemon").strong());
                ui.label(
                    RichText::new(health["daemon"].as_str().unwrap_or("unknown"))
                        .color(if reachable { WIRED } else { ABSENT }),
                );
            });
            egui::Grid::new("setup-health")
                .striped(true)
                .num_columns(3)
                .show(ui, |ui| {
                    for runtime in health["runtimes"].as_array().into_iter().flatten() {
                        let name = runtime["name"].as_str().unwrap_or("Runtime");
                        for check in runtime["checks"].as_array().into_iter().flatten() {
                            let status = check["status"].as_str().unwrap_or("unknown");
                            if status == "unsupported" {
                                continue;
                            }
                            ui.label(name);
                            ui.label(check["channel"].as_str().unwrap_or("connection"));
                            let detail = check["detail"].as_str().unwrap_or("unknown");
                            // The executable behind the registration is
                            // the thing that makes a check pass or fail,
                            // so it belongs on the row that reports it —
                            // on hover, where a path does not push the
                            // columns off the screen.
                            let row =
                                ui.label(RichText::new(detail).color(health_colour(ui, status)));
                            if let Some(executable) = check["executable"].as_str() {
                                let _ = row.on_hover_text(executable);
                            }
                            ui.end_row();
                        }
                    }
                });
            ui.label(
                RichText::new(
                    "Configuration detection does not prove that a provider has used its \
                     connection. Start a fresh session after setup.",
                )
                .weak()
                .small(),
            );
        }
        let Some(plan) = self.setup_plan.clone() else {
            return;
        };
        let phase = plan["phase"].as_str().unwrap_or("unknown");
        let id = plan["id"].as_str().unwrap_or("").to_owned();
        ui.add_space(8.0);
        ui.separator();
        ui.horizontal(|ui| {
            ui.label(RichText::new("Setup plan").strong());
            ui.label(RichText::new(phase).color(phase_colour(ui, phase)));
            ui.label(RichText::new(&id).weak().small())
                .on_hover_text("Kept under the AgentDocker home, readable only by you.");
        });
        if let Some(executable) = plan["executable"].as_str() {
            ui.label(
                RichText::new(format!("Connect through {executable}"))
                    .weak()
                    .small(),
            );
        }
        let changes = plan["changes"].as_array();
        match changes {
            Some(changes) if !changes.is_empty() => {
                for change in changes {
                    ui.label(format!(
                        "{} · {} · {}",
                        change["runtime"].as_str().unwrap_or(""),
                        change["channel"].as_str().unwrap_or(""),
                        change["path"].as_str().unwrap_or("")
                    ));
                    // What the step does to that file, which is the only
                    // thing separating an edit we make from a
                    // registration the provider's own tool makes on its
                    // own state.
                    if let Some(action) = change["action"].as_str() {
                        ui.label(RichText::new(format!("    {action}")).weak().small());
                    }
                }
            }
            // Said out loud, because a plan with nothing in it beside a
            // greyed-out Apply reads as a failure rather than as the
            // good news it is.
            _ => {
                ui.label(
                    RichText::new("Nothing to change — every channel here is already wired.")
                        .color(WIRED),
                );
            }
        }
        if let Some(notes) = plan["notes"].as_array().filter(|notes| !notes.is_empty()) {
            ui.add_space(4.0);
            for note in notes.iter().filter_map(|note| note.as_str()) {
                ui.label(RichText::new(format!("· {note}")).weak().small());
            }
        }
        ui.add_space(4.0);
        let applicable = matches!(phase, "prepared" | "applying")
            && changes.is_some_and(|changes| !changes.is_empty());
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    !self.setup_busy && applicable,
                    egui::Button::new("Apply changes"),
                )
                .on_hover_text(
                    "Write the changes above. Each file is replaced whole and only if it still \
                     says what it said at preview, so an edit made since is never overwritten.",
                )
                // A disabled button that will not say why is the thing
                // this whole screen was rebuilt to stop doing.
                .on_disabled_hover_text(if self.setup_busy {
                    "A setup command is still running."
                } else if !applicable {
                    "This plan has nothing left to apply: it has already been applied or undone, \
                     or there was nothing to change."
                } else {
                    ""
                })
                .clicked()
            {
                self.setup_busy = true;
                self.send(Cmd::Setup(vec!["--apply".into(), id.clone()]));
            }
            if ui
                .add_enabled(
                    !self.setup_busy && phase != "undone",
                    egui::Button::new("Undo this setup"),
                )
                .on_hover_text(
                    "Put every file back the way this plan found it, and take back any \
                     registration it asked a provider to make.",
                )
                .on_disabled_hover_text(if self.setup_busy {
                    "A setup command is still running."
                } else {
                    "This plan has already been undone."
                })
                .clicked()
            {
                self.setup_busy = true;
                self.send(Cmd::Setup(vec!["--undo".into(), id]));
            }
            if ui
                .add_enabled(!self.setup_busy, egui::Button::new("Close"))
                .on_hover_text("Stop showing this plan. It stays saved and can be reopened.")
                .clicked()
            {
                self.setup_plan = None;
            }
        });
    }

    fn journal_screen(&mut self, ui: &mut egui::Ui) {
        ui.label(format!(
            "Latest {JOURNAL_WINDOW} entries. Earlier entries remain in the project journal."
        ));
        let projects = self.projects();
        let mut changed: Option<String> = None;
        ui.horizontal(|ui| {
            let current = self
                .journal_project
                .as_ref()
                .and_then(|id| projects.iter().find(|p| p.0 == *id))
                .map(|p| p.1.clone())
                .unwrap_or_else(|| "project".to_owned());
            egui::ComboBox::from_label("Project")
                .selected_text(current)
                .show_ui(ui, |ui| {
                    for (id, name) in &projects {
                        let selected = self.journal_project.as_deref() == Some(id.as_str());
                        if ui.selectable_label(selected, name).clicked() && !selected {
                            changed = Some(id.clone());
                        }
                    }
                });
            if ui.button("Refresh").clicked()
                && let Some(id) = self.journal_project.clone()
            {
                self.send(Cmd::Journal(id));
            }
        });
        if let Some(id) = changed {
            self.journal_project = Some(id.clone());
            self.journal.clear();
            self.send(Cmd::Journal(id));
        }
        ui.separator();
        let now = Utc::now();
        if self.journal.is_empty() {
            ui.label(RichText::new("Nothing in the journal yet.").weak());
        }
        for entry in &self.journal {
            ui.horizontal(|ui| {
                ui.monospace(format!("{:>6}", entry.seq));
                ui.label(RichText::new(ago(now, entry.at)).color(Color32::GRAY));
                ui.label(entry.line());
            });
        }
    }

    /// The mark, beside the name it belongs to.
    ///
    /// The same PNG the window sets as its icon, so the thing in the
    /// title bar and the thing in the Dock cannot drift apart. Uploaded
    /// on the first frame that draws it and kept: decoding a PNG every
    /// frame to draw a 20-pixel square would be absurd.
    fn mark(&mut self, ui: &mut egui::Ui) {
        let texture = self.mark.get_or_insert_with(|| {
            let icon = eframe::icon_data::from_png_bytes(include_bytes!("icon.png"))
                .expect("the embedded icon is a valid PNG");
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [icon.width as usize, icon.height as usize],
                &icon.rgba,
            );
            ui.ctx()
                .load_texture("agentdocker-mark", image, egui::TextureOptions::LINEAR)
        });
        let side = ui.text_style_height(&egui::TextStyle::Heading);
        ui.add(egui::Image::new(&*texture).fit_to_exact_size(egui::vec2(side, side)));
    }

    /// Put the chosen sizes and spacing into the context, and only when
    /// they have changed: setting text styles rebuilds the font atlas,
    /// which is not a thing to do on every frame.
    fn apply_settings(&mut self, ctx: &egui::Context) {
        if self.applied.as_ref() == Some(&self.settings) {
            return;
        }
        let text = self.settings.text_size;
        let mono = self.settings.terminal_size;
        let roomy = self.settings.roomy;
        // A light terminal palette inside a dark window is two products
        // in one frame, so the palette decides the window as well.
        ctx.set_theme(if self.settings.palette().is_light() {
            egui::ThemePreference::Light
        } else {
            egui::ThemePreference::Dark
        });
        // Both themes, so a viewer switching light/dark keeps the sizes.
        ctx.all_styles_mut(|style| {
            use egui::{FontFamily, FontId, TextStyle};
            style.text_styles = [
                (TextStyle::Small, FontId::proportional(text - 2.0)),
                (TextStyle::Body, FontId::proportional(text)),
                (TextStyle::Button, FontId::proportional(text)),
                (TextStyle::Heading, FontId::proportional(text + 4.0)),
                (
                    TextStyle::Monospace,
                    FontId::new(mono, FontFamily::Monospace),
                ),
            ]
            .into();
            // Docker Desktop's tables breathe; ours were tight enough to
            // read as a spreadsheet. This is the whole difference.
            let room = if roomy { 10.0 } else { 5.0 };
            style.spacing.item_spacing = egui::vec2(10.0, room);
            style.spacing.button_padding = egui::vec2(8.0, room);
            style.visuals.selection.bg_fill = ACCENT;
            style.visuals.widgets.hovered.corner_radius = 5.into();
            style.visuals.widgets.active.corner_radius = 5.into();
            style.visuals.widgets.inactive.corner_radius = 5.into();
        });
        self.applied = Some(self.settings.clone());
    }

    /// What the window looks like.
    fn settings_screen(&mut self, ui: &mut egui::Ui) {
        let before = self.settings.clone();
        let palette = self.settings.palette();
        egui::Grid::new("appearance")
            .num_columns(2)
            .spacing([16.0, 10.0])
            .show(ui, |ui| {
                ui.label(RichText::new("Palette").strong());
                egui::ComboBox::from_id_salt("palette")
                    .selected_text(self.settings.palette.clone())
                    .show_ui(ui, |ui| {
                        for choice in crate::theme::PALETTES {
                            ui.selectable_value(
                                &mut self.settings.palette,
                                choice.name.to_owned(),
                                choice.name,
                            );
                        }
                    });
                ui.end_row();

                ui.label(RichText::new("Terminal size").strong());
                ui.add(
                    egui::Slider::new(&mut self.settings.terminal_size, 9.0..=24.0)
                        .fixed_decimals(0),
                );
                ui.end_row();

                ui.label(RichText::new("Text size").strong());
                ui.add(
                    egui::Slider::new(&mut self.settings.text_size, 10.0..=24.0).fixed_decimals(0),
                );
                ui.end_row();

                ui.label(RichText::new("Roomy rows").strong());
                ui.checkbox(&mut self.settings.roomy, "");
                ui.end_row();
            });

        // The palette on the palette's own ground, because a swatch on
        // the window's ground says nothing about what it will look like.
        ui.add_space(12.0);
        egui::Frame::new()
            .fill(palette.ground)
            .inner_margin(egui::Margin::same(10))
            .corner_radius(6)
            .show(ui, |ui| {
                let font = egui::FontId::monospace(self.settings.terminal_size);
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("agentdocker")
                            .font(font.clone())
                            .color(palette.accent),
                    );
                    ui.label(
                        RichText::new("ps --all")
                            .font(font.clone())
                            .color(palette.text),
                    );
                });
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    for colour in palette.ansi {
                        ui.label(RichText::new("██").font(font.clone()).color(colour));
                    }
                });
            });

        ui.add_space(12.0);
        if ui
            .button("Reset")
            .on_hover_text("Back to the palette and sizes the window ships with.")
            .clicked()
        {
            self.settings = crate::theme::Settings::default();
        }
        if self.settings != before {
            self.settings = std::mem::take(&mut self.settings).clamped();
            self.settings.save(&self.home);
        }
    }

    fn leases_screen(&mut self, ui: &mut egui::Ui) {
        let now = Utc::now();
        if self.leases.is_empty() {
            ui.label(RichText::new("No leases held.").weak());
            return;
        }
        egui::Grid::new("leases")
            .striped(true)
            .num_columns(6)
            .show(ui, |ui| {
                for header in ["PROJECT", "RESOURCE", "HOLDER", "MODE", "EXPIRES", "NOTE"] {
                    ui.label(RichText::new(header).strong());
                }
                ui.end_row();
                for lease in &self.leases {
                    // A lease belongs to whichever project its holder is
                    // in, and this list mixes them — so each row is
                    // named, not merely coloured.
                    if !self.in_focus(lease.holder.as_str()) {
                        continue;
                    }
                    self.project_label(ui, lease.holder.as_str());
                    ui.label(lease.resource.as_str());
                    ui.label(self.name_of(lease.holder.as_str()));
                    ui.label(format!("{:?}", lease.mode).to_lowercase());
                    let left = (lease.expires_at - now).num_seconds();
                    ui.label(if left > 0 {
                        format!("in {}", span(left))
                    } else {
                        "expired".to_owned()
                    });
                    ui.label(lease.note.clone().unwrap_or_default());
                    ui.end_row();
                }
            });
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.worker_stop
            .store(true, std::sync::atomic::Ordering::Release);
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Hidden windows still consume bounded replies and events, without
        // depending on a rendered frame to release backend backpressure.
        self.drain();
        let delay = if ctx.input(|input| input.viewport().visible()) == Some(false) {
            Duration::from_secs(2)
        } else {
            Duration::from_millis(500)
        };
        ctx.request_repaint_after(delay);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.draw(ui);
    }
}

impl App {
    /// Draw separately from eframe's window so every screen can be tested.
    fn draw(&mut self, ui: &mut egui::Ui) {
        self.apply_settings(ui.ctx());

        // The sidebar and the title bar sit on their own ground, the
        // way every desktop control panel worth copying does: the list
        // of places is not the same surface as the place you are in.
        let sidebar = {
            let dark = ui.visuals().dark_mode;
            let base = ui.visuals().window_fill;
            let shift = if dark { -8 } else { 10 };
            Color32::from_rgb(
                base.r().saturating_add_signed(shift),
                base.g().saturating_add_signed(shift),
                base.b().saturating_add_signed(shift),
            )
        };
        egui::Panel::top("top")
            .frame(
                egui::Frame::new()
                    .fill(sidebar)
                    .inner_margin(egui::Margin::symmetric(10, 8)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    self.mark(ui);
                    ui.heading("agentdocker");
                    ui.separator();
                    match &self.connected {
                        Ok(()) => {
                            crate::projects::bullet(ui, WIRED);
                            ui.label(RichText::new("connected").color(WIRED))
                                .on_hover_text("The daemon is answering on this socket.");
                            ui.label(RichText::new(&self.socket).color(Color32::GRAY));
                        }
                        Err(reason) => {
                            crate::projects::bullet(ui, ABSENT);
                            ui.label(RichText::new("disconnected").color(ABSENT)).on_hover_text(
                                "The window keeps trying. Everything on screen is the last thing \
                                 the daemon said.",
                            );
                            ui.label(RichText::new(reason).color(Color32::GRAY));
                        }
                    }
                    // One project at a time, when a fleet is too much at
                    // once. Placed here rather than per screen because it
                    // means the same thing on all of them.
                    let projects = self.projects();
                    if projects.len() > 1 {
                        ui.separator();
                        let showing = self
                            .focus
                            .as_ref()
                            .and_then(|id| projects.iter().find(|(pid, _)| pid == id))
                            .map(|(_, name)| name.clone())
                            .unwrap_or_else(|| "All projects".to_owned());
                        egui::ComboBox::from_id_salt("project-focus")
                            .selected_text(showing)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut self.focus, None, "All projects");
                                for (id, name) in &projects {
                                    let label =
                                        RichText::new(name).color(crate::projects::colour(id));
                                    ui.selectable_value(&mut self.focus, Some(id.clone()), label);
                                }
                            });
                    }
                    // The last thing that happened, and only while it
                    // is still the last thing that happened. A line that
                    // never expires becomes a line nobody reads, and an
                    // error from ten minutes ago reported as news is
                    // worse than no line at all.
                    if !self.status.is_empty() {
                        ui.separator();
                        ui.label(RichText::new(&self.status).weak());
                    }
                });
            });
        egui::Panel::left("nav")
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(sidebar)
                    .inner_margin(egui::Margin::symmetric(8, 10)),
            )
            .show(ui, |ui| {
                ui.set_min_width(150.0);
                for screen in Screen::ALL {
                    // The count belongs beside the name, not inside it:
                    // "Questions" is the place, "3" is what is in it.
                    let count = match screen {
                        Screen::Agents => {
                            Some(self.agents.iter().filter(|a| a.status.is_live()).count())
                        }
                        Screen::Questions if !self.questions.is_empty() => {
                            Some(self.questions.len())
                        }
                        Screen::Leases => Some(self.leases.len()),
                        _ => None,
                    };
                    let selected = self.screen == screen;
                    // Drawn rather than assembled from widgets: the row
                    // is the target, the name is left and the count is
                    // right, and no built-in gives all three at once.
                    let (rect, entry) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), 28.0),
                        egui::Sense::click(),
                    );
                    if ui.is_rect_visible(rect) {
                        let visuals = ui.visuals();
                        if selected {
                            ui.painter().rect_filled(rect, 6.0, ACCENT);
                        } else if entry.hovered() {
                            ui.painter()
                                .rect_filled(rect, 6.0, visuals.widgets.hovered.bg_fill);
                        }
                        // The one place in the window that is painted
                        // rather than built from widgets, so it is also
                        // the one place that has to draw its own focus.
                        // Tab already reached these rows and Enter
                        // already chose one; nothing said which row was
                        // about to be chosen, which is the whole of what
                        // a focus ring is for.
                        if entry.has_focus() {
                            ui.painter().rect_stroke(
                                rect,
                                6.0,
                                visuals.widgets.active.fg_stroke,
                                egui::StrokeKind::Inside,
                            );
                        }
                        let ink = if selected {
                            Color32::WHITE
                        } else {
                            visuals.text_color()
                        };
                        let font = egui::TextStyle::Body.resolve(ui.style());
                        ui.painter().text(
                            rect.left_center() + egui::vec2(10.0, 0.0),
                            egui::Align2::LEFT_CENTER,
                            screen.title(),
                            font.clone(),
                            ink,
                        );
                        if let Some(n) = count {
                            ui.painter().text(
                                rect.right_center() - egui::vec2(10.0, 0.0),
                                egui::Align2::RIGHT_CENTER,
                                n,
                                font,
                                if selected {
                                    ink
                                } else {
                                    visuals.weak_text_color()
                                },
                            );
                        }
                    }
                    if entry.clicked() {
                        self.screen = screen;
                    }
                }
            });
        egui::CentralPanel::default().show(ui, |ui| {
            // The terminal draws its own scroll region and wants every
            // keystroke, so it is not inside the shared scroll area.
            if self.screen == Screen::Terminal {
                self.terminal_screen(ui);
                return;
            }
            // The journal grows downward; every other screen is a list
            // the reader scrolls from the top.
            let follow = self.screen == Screen::Journal;
            // Tables can be wider than the window — a project name, a
            // long branch and a note do not shrink to fit — so they are
            // reachable sideways rather than cut off at the edge. The
            // screens that lay themselves out to the width they are
            // given must not have that: an unbounded width would let
            // the console's panel grow without limit.
            let wide = matches!(
                self.screen,
                Screen::Agents | Screen::Runtimes | Screen::Leases | Screen::Journal
            );
            egui::ScrollArea::new([wide, true])
                .stick_to_bottom(follow)
                .show(ui, |ui| match self.screen {
                    Screen::Agents => self.agents_screen(ui),
                    Screen::Questions => self.questions_screen(ui),
                    Screen::Channels => self.channels_screen(ui),
                    Screen::Console => self.console_screen(ui),
                    Screen::Terminal => {}
                    Screen::Runtimes => self.runtimes_screen(ui),
                    Screen::Journal => self.journal_screen(ui),
                    Screen::Leases => self.leases_screen(ui),
                    Screen::Settings => self.settings_screen(ui),
                    Screen::Desktop => {
                        if let Some(args) = self.desktop.show(ui) {
                            self.send(Cmd::Desktop(args));
                        }
                    }
                });
        });
        if let Some(smoke) = &mut self.smoke {
            smoke.tick(
                ui.ctx(),
                self.connected.is_ok(),
                self.runtimes.len(),
                &self.discovered,
            );
        }
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
/// A channel message carries the room it was sent to in its payload, so
/// the conversation can be put back under the room it happened in.
/// Anything without one was sent to this person directly and belongs on
/// its own, not silently filed under whichever room sorts first.
type Grouped<'a> = (
    BTreeMap<String, Vec<&'a agentdocker_core::Envelope>>,
    Vec<&'a agentdocker_core::Envelope>,
);

fn by_room(inbox: &[agentdocker_core::Envelope]) -> Grouped<'_> {
    let mut said: BTreeMap<String, Vec<&agentdocker_core::Envelope>> = BTreeMap::new();
    let mut direct = Vec::new();
    for message in inbox {
        match message.payload["channel"].as_str() {
            Some(room) => said.entry(room.to_owned()).or_default().push(message),
            None => direct.push(message),
        }
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

/// One line of a conversation: who, when, and what they actually said.
///
/// A message's payload is whatever the sender put there, so `text` is
/// taken when it is there and the rest is shown as it came rather than
/// dropped — an agent that sends a structured payload is still saying
/// something.
fn said_by(
    ui: &mut egui::Ui,
    now: DateTime<Utc>,
    from: &str,
    message: &agentdocker_core::Envelope,
) {
    ui.horizontal_top(|ui| {
        ui.label(RichText::new(from).strong());
        ui.label(RichText::new(ago(now, message.sent_at)).weak().small());
        if message.kind != "chat" {
            ui.label(RichText::new(&message.kind).weak().small());
        }
    });
    let body = match message.payload["text"].as_str() {
        Some(text) => text.to_owned(),
        None => message.payload.to_string(),
    };
    ui.label(body);
    ui.add_space(2.0);
}

fn health_colour(ui: &egui::Ui, status: &str) -> Color32 {
    match status {
        "executable_available" => WIRED,
        // Registered, but pointing at something this machine cannot run.
        "missing" | "executable_missing" => ABSENT,
        "invalid" | "disabled" | "unverified" | "incomplete" => UNVERIFIED,
        _ => ui.visuals().text_color(),
    }
}

/// A saved plan's phase: `applied` is in force, `undone` is not, and the
/// two in between were interrupted and can be resumed either way.
fn phase_colour(ui: &egui::Ui, phase: &str) -> Color32 {
    match phase {
        "applied" => WIRED,
        "applying" | "undoing" => UNVERIFIED,
        "prepared" | "undone" => ui.visuals().text_color(),
        _ => ui.visuals().weak_text_color(),
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
    ctx: egui::Context,
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
    ctx: egui::Context,
    cancelled: Arc<std::sync::atomic::AtomicBool>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let (consoles, console_worker) = lane(
            tx.clone(),
            ctx.clone(),
            cancelled.clone(),
            |line: String| Msg::Console(console(&line)),
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
        while let Ok(cmd) = rx.recv() {
            if cancelled.load(std::sync::atomic::Ordering::Acquire) {
                break;
            }
            // Admission never waits behind a subprocess. A rejected job
            // returns the usual completion shape to clear the UI's busy state.
            let rejected = match cmd {
                Cmd::Console(line) => submit(&consoles, line, Msg::Console),
                Cmd::Setup(args) => submit(&setups, args, |error| Msg::Setup(Err(error))),
                Cmd::Desktop(args) => submit(&desktops, args, |error| Msg::Desktop(Err(error))),
                daemon => {
                    let answer = match &daemon {
                        Cmd::Answer(id, _) => Some(id.clone()),
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
        drop((consoles, setups, desktops));
        for worker in [console_worker, setup_worker, desktop_worker] {
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
            all: false,
            project: None,
            labels: BTreeMap::new(),
        })? {
            Response::Agents { agents } => Some(Msg::Agents(agents)),
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
        Cmd::Journal(project) => match client.call(&Request::Journal {
            project: project.clone(),
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
        Cmd::Activity => match client.call(&Request::Activity {
            agent: None,
            project: None,
            all: false,
        })? {
            Response::Activity { activity } => Some(Msg::Activity(activity)),
            _ => None,
        },
        // Every room in the named project, not only the ones this person is
        // in. A channel between two agents need not have the human as a
        // member — most will not — and a window that listed only its
        // own memberships would show nothing while agents talked.
        Cmd::Channels(project) => match client.call(&Request::Channels {
            project: project.clone(),
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
        Cmd::Setup(args) => Some(Msg::Setup(setup(&args))),
        Cmd::Desktop(args) => Some(Msg::Desktop(desktop(&args))),
        Cmd::Console(line) => Some(Msg::Console(console(&line))),
    })
}

/// Any `agentdocker` command, run with the CLI beside this binary. The
/// command line is the complete surface and it keeps growing; a window
/// that mirrored it in widgets would always lag behind, so the window
/// runs the real thing and shows what it said.
fn console(line: &str) -> String {
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
    let cwd = match std::env::current_dir() {
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
    let cli = beside("agentdocker");
    let mut argv = vec![
        cli.to_str().ok_or("CLI path is not UTF-8")?.to_owned(),
        "desktop".into(),
    ];
    argv.extend_from_slice(args);
    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    let output = agentdocker_host::command::run(&cwd, &argv, Duration::from_secs(600))
        .map_err(|error| error.to_string())?;
    if !output.success {
        return Err(format!("Installation failed: {}", output.text.trim()));
    }
    serde_json::from_str(&output.stdout)
        .map_err(|error| format!("Invalid installation reply: {error}"))
}

fn spawn_events(client: Arc<Client>, tx: SyncSender<Msg>, ctx: egui::Context) {
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

    /// Every screen lays itself out, with something on it.
    ///
    /// A layout mistake — a grid whose rows and columns disagree, a
    /// widget that borrows what it is drawn into — is a panic at paint
    /// time, on whichever screen the person who changed it was not
    /// looking at. Cheap here, expensive to find by opening the window.
    #[test]
    fn every_screen_lays_itself_out_with_something_on_it() {
        let (tx, _requests) = queue::channel();
        let (_messages, rx) = sync_channel::<Msg>(MESSAGE_CAPACITY);
        let mut app = App::bare(tx, rx);
        app.runtimes = serde_json::from_value(serde_json::json!([{
            "name":"claude-code", "vendor":"Anthropic", "label":"Claude Code",
            "cli":"/fixture/claude", "version":"fixture", "apps":[],
            "config_dir":null, "mcp":"missing", "hooks":"wired"
        }, {
            "name":"amp", "vendor":"Sourcegraph", "label":"Amp", "cli":null,
            "version":null, "apps":[], "config_dir":null,
            "mcp":"unverified", "hooks":"unsupported"
        }]))
        .unwrap();
        // One of every state the panels can be in, so the colours, the
        // hover text and the empty branches all get drawn at least once.
        app.setup_health = Some(serde_json::json!({
            "daemon_reachable": true, "daemon": "agentd 0.1.0",
            "runtimes": [{"name": "claude-code", "checks": [
                {"channel": "mcp", "status": "missing", "detail": "no registration"},
                {"channel": "hooks", "status": "executable_available",
                 "detail": "installed", "executable": "/fixture/agentdocker"},
                {"channel": "other", "status": "unsupported", "detail": "n/a"}]}]
        }));
        app.setup_history = vec![
            serde_json::json!({"id": "11111111-2222-3333-4444-555555555555", "phase": "applied"}),
            serde_json::json!({"id": "66666666-7777-8888-9999-000000000000", "phase": "undone"}),
        ];
        app.setup_plan = Some(serde_json::json!({
            "id": "11111111-2222-3333-4444-555555555555", "phase": "prepared",
            "executable": "/fixture/agentdocker",
            "changes": [
                {"runtime":"claude-code", "channel":"hooks",
                 "path":"/fixture/.claude/settings.json", "action":"create configuration"},
                {"runtime":"claude-code", "channel":"mcp", "path":"/fixture/.claude.json",
                 "action":"register through `claude mcp add`"}],
            "notes": ["Restart the selected provider session after applying."]}));
        app.channels = serde_json::from_value(serde_json::json!([{
            "id": "1ff5d0b3d5ec", "project": "p1",
            "subject": {"kind": "task", "task": "Agent identity and activity reporting"},
            "members": ["aaaa", "bbbb"], "opened_by": "aaaa",
            "opened_at": "2026-09-08T03:23:43Z",
            "reviews": [{"by": "bbbb", "by_name": "codex-27221", "of": "aaaa",
                         "of_name": "claude-code-45856", "verdict": "changes",
                         "note": "three remaining blockers", "at": "2026-09-08T03:43:28Z"}]
        }, {
            "id": "ee3cbc67d8b7", "project": "p1",
            "subject": {"kind": "contested", "paths": ["/x/mcp.rs"]},
            "members": ["aaaa"], "opened_at": "2026-09-08T03:27:37Z",
            "closed_at": "2026-09-08T03:40:00Z", "resolution": "everyone left"
        }]))
        .unwrap();
        app.inbox = serde_json::from_value(serde_json::json!([{
            "id": "m1", "from": "bbbb", "to": {"kind": "channel", "value": "1ff5d0b3d5ec"},
            "kind": "coordination", "payload": {"channel": "1ff5d0b3d5ec", "text": "in the room"},
            "sent_at": "2026-09-08T03:24:46Z"
        }, {
            "id": "m2", "from": "bbbb", "to": {"kind": "agent", "value": "aaaa"},
            "kind": "chat", "payload": {"text": "and one straight to you"},
            "sent_at": "2026-09-08T03:25:00Z"
        }, {
            "id": "m3", "from": "bbbb", "to": {"kind": "agent", "value": "aaaa"},
            "kind": "notice", "payload": {"structured": "no text field at all"},
            "sent_at": "2026-09-08T03:26:00Z"
        }]))
        .unwrap();
        app.console_output = "agentdocker ps\nno agents\n".to_owned();
        app.console_running = 1;
        app.status = "something happened".to_owned();
        app.connected = Err("no daemon".to_owned());

        let ctx = egui::Context::default();
        for screen in Screen::ALL {
            app.screen = screen;
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| app.draw(ui));
            // A frame hands back the textures its painter would have
            // uploaded, and drops loudly if nobody takes them. There is
            // no painter here, so this is where they are taken.
            output.textures_delta.clear();
            assert!(
                !ctx.tessellate(output.shapes, output.pixels_per_point)
                    .is_empty(),
                "{} drew nothing",
                screen.title()
            );
        }
    }

    /// The window can be driven without a mouse.
    ///
    /// The sidebar is the one thing here painted rather than assembled
    /// from widgets, which is exactly the kind of thing that quietly
    /// stops being reachable from the keyboard. Tab reaches a row and
    /// Enter chooses it, and if that ever stops being true this is what
    /// says so.
    #[test]
    fn the_sidebar_is_reachable_and_choosable_from_the_keyboard() {
        let (tx, _requests) = queue::channel();
        let (_messages, rx) = sync_channel::<Msg>(MESSAGE_CAPACITY);
        let mut app = App::bare(tx, rx);
        let ctx = egui::Context::default();
        let frame = |events: Vec<egui::Event>, app: &mut App| {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    events,
                    ..Default::default()
                },
                |ui| app.draw(ui),
            );
            output.textures_delta.clear();
        };
        let key = |key: egui::Key, pressed: bool| egui::Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };

        // One frame so the rows exist to be reached, then Tab down to
        // the third of them and choose it.
        frame(Vec::new(), &mut app);
        assert_eq!(app.screen, Screen::ALL[0], "starts where it starts");
        for _ in 0..3 {
            frame(
                vec![key(egui::Key::Tab, true), key(egui::Key::Tab, false)],
                &mut app,
            );
        }
        frame(
            vec![key(egui::Key::Enter, true), key(egui::Key::Enter, false)],
            &mut app,
        );
        assert_eq!(
            app.screen,
            Screen::ALL[2],
            "three tabs and a return should land on {}",
            Screen::ALL[2].title()
        );
    }

    #[test]
    fn activity_rows_follow_reports_instead_of_runtime_capability_guesses() {
        let (tx, _requests) = queue::channel();
        let (_messages, rx) = sync_channel::<Msg>(MESSAGE_CAPACITY);
        let mut app = App::bare(tx, rx);
        let now = Utc::now();
        let mut agent = AgentRecord::new(
            agentdocker_core::AgentSpec {
                name: "owned-codex".into(),
                runtime: "codex".into(),
                ..Default::default()
            },
            false,
            now,
        );
        agent.status = agentdocker_core::AgentStatus::Running;
        let id = agent.id.as_str().to_owned();
        app.agents.push(agent);
        app.runtimes = serde_json::from_value(serde_json::json!([{
            "name":"codex", "vendor":"OpenAI", "label":"Codex", "cli":"/fixture/codex",
            "version":null, "apps":[], "config_dir":null,
            "mcp":"wired", "hooks":"unsupported"
        }]))
        .unwrap();
        let ctx = egui::Context::default();
        for (state, expected) in [
            (None, "unknown"),
            (Some(Activity::Unknown), "unknown"),
            (Some(Activity::Idle { since: now }), "idle"),
            (Some(Activity::Working { since: now }), "working"),
        ] {
            app.activity.clear();
            if let Some(state) = state {
                app.activity.insert(id.clone(), state);
            }
            let mut output = ctx.run_ui(Default::default(), |ui| app.agents_screen(ui));
            output.textures_delta.clear();
            fn collect(shape: &egui::epaint::Shape, texts: &mut Vec<String>) {
                match shape {
                    egui::epaint::Shape::Text(text) => texts.push(text.galley.text().to_owned()),
                    egui::epaint::Shape::Vec(shapes) => {
                        for shape in shapes {
                            collect(shape, texts);
                        }
                    }
                    _ => {}
                }
            }
            let mut texts = Vec::new();
            for shape in output.shapes {
                collect(&shape.shape, &mut texts);
            }
            assert!(
                texts.iter().any(|text| text == expected),
                "expected {expected}; saw {texts:?}"
            );
            if expected == "unknown" {
                assert!(
                    !texts
                        .iter()
                        .any(|text| text == "idle" || text == "all idle")
                );
            }
        }
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
             "kind": "chat", "payload": {"channel": "room-one", "text": "first"},
             "sent_at": "2026-09-08T03:00:00Z"},
            {"id": "m2", "from": "b", "to": {"kind": "channel", "value": "room-two"},
             "kind": "chat", "payload": {"channel": "room-two", "text": "elsewhere"},
             "sent_at": "2026-09-08T03:01:00Z"},
            {"id": "m3", "from": "a", "to": {"kind": "channel", "value": "room-one"},
             "kind": "chat", "payload": {"channel": "room-one", "text": "second"},
             "sent_at": "2026-09-08T03:02:00Z"},
            {"id": "m4", "from": "b", "to": {"kind": "agent", "value": "user"},
             "kind": "chat", "payload": {"text": "just to you"},
             "sent_at": "2026-09-08T03:03:00Z"}
        ]))
        .unwrap();
        let (said, direct) = by_room(&inbox);
        assert_eq!(said["room-one"].len(), 2, "kept together and in order");
        assert_eq!(said["room-one"][0].id.as_str(), "m1");
        assert_eq!(said["room-two"].len(), 1);
        // A message with no room is not filed under whichever room
        // happens to sort first.
        assert_eq!(direct.len(), 1);
        assert_eq!(direct[0].id.as_str(), "m4");
    }

    #[test]
    fn channel_snapshots_keep_other_projects_and_refuse_departed_projects() {
        use agentdocker_core::channel::{Channel, ChannelSubject};
        use agentdocker_core::{AgentSpec, ChannelId, ProjectId};
        let (commands, requests) = std::sync::mpsc::channel();
        let (messages, results) = std::sync::mpsc::channel();
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
        messages.send(Msg::Agents(agents.clone())).unwrap();
        app.drain();
        assert_eq!(
            requests
                .try_iter()
                .filter(|cmd| matches!(cmd, Cmd::Channels(_)))
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
        messages.send(Msg::Agents(agents)).unwrap();
        messages
            .send(Msg::Channels(
                "project-b".into(),
                vec![channel("project-b", "stale")],
            ))
            .unwrap();
        app.drain();
        assert!(app.channels.is_empty());
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
            Cmd::Channels("fixture-project".into()),
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
        app.setup_busy = true;
        app.send(Cmd::Setup(vec!["--health".into()]));
        assert!(!app.setup_busy);
        app.console_running = 1;
        app.send(Cmd::Console("ps".into()));
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
            commands.send(Cmd::Channels("project-a".into())).unwrap();
            commands.send(Cmd::Channels("project-b".into())).unwrap();
            commands.send(Cmd::Inbox).unwrap();
        }
        let pending: Vec<_> = requests.try_iter().collect();
        assert_eq!(pending.len(), 3);
        assert!(matches!(&pending[0], Cmd::Channels(project) if project == "project-a"));
        assert!(matches!(&pending[1], Cmd::Channels(project) if project == "project-b"));
        assert!(matches!(&pending[2], Cmd::Inbox));
        commands.send(Cmd::Channels("project-a".into())).unwrap();
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
        messages.send(Msg::Agents(agents.clone())).unwrap();
        app.drain();
        assert_eq!(
            requests
                .try_iter()
                .filter(|cmd| matches!(cmd, Cmd::Channels(_)))
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
        messages.send(Msg::Agents(agents)).unwrap();
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
        });
        let (commands, requests) = queue::channel();
        let (messages, results) = sync_channel(MESSAGE_CAPACITY);
        let worker = spawn_worker(
            Arc::new(Client::isolated(socket)),
            requests,
            messages,
            egui::Context::default(),
            Default::default(),
        );
        commands.send(cmd).unwrap();
        drop(commands);
        worker.join().unwrap();
        server.join().unwrap();
        results.try_iter().collect()
    }

    #[cfg(unix)]
    #[test]
    fn channel_requests_name_a_project_without_filtering_to_human_membership() {
        use serde_json::json;
        let replies = command_with_replies(
            Cmd::Channels("fixture-project".into()),
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

    fn disconnected_command(cmd: Cmd) -> Vec<Msg> {
        let temp = tempfile::tempdir().unwrap();
        let client = Arc::new(Client::isolated(temp.path().join("missing.sock")));
        let (commands, requests) = queue::channel();
        let (messages, results) = sync_channel(MESSAGE_CAPACITY);
        let worker = spawn_worker(
            client,
            requests,
            messages,
            egui::Context::default(),
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
            egui::Context::default(),
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
        let (quick, quick_worker) = lane(tx, egui::Context::default(), cancelled, Msg::Status);
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
            egui::Context::default(),
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
        app.send(Cmd::Console(answer));
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
                .any(|cmd| matches!(cmd, Cmd::Journal(project) if project == "project-a"))
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
}
