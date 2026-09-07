//! The window: what it shows, how it asks the daemon, and how it keeps
//! up. Requests run on a worker thread and the event stream on another;
//! both hand results to the UI thread through a channel and ask for a
//! repaint, so the window never blocks on the socket.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use agentdocker_core::journal::ago;
use agentdocker_core::{
    Activity, AgentActivity, AgentRecord, DiscoveredProcess, Event, EventKind, JournalEntry, Lease,
    MessageId, ProjectRef, Question, Request, Response, RuntimeInfo,
};
use chrono::Utc;
use egui::{Color32, RichText};

use crate::client::Client;
use crate::terminal::{Status, Terminal};

/// How often agents, leases and discovered processes are re-read.
const REFRESH: Duration = Duration::from_secs(2);
/// How often the runtime inventory is re-read (it asks each CLI).
const RUNTIMES_REFRESH: Duration = Duration::from_secs(30);
/// How long a console command may run. Long enough for anything that
/// finishes, short enough that `watch` or `logs -f` — which never do —
/// give the worker thread back.
const CONSOLE_TIMEOUT: Duration = Duration::from_secs(20);

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
    Terminal,
    Console,
    Runtimes,
    Journal,
    Leases,
    Settings,
}

impl Screen {
    const ALL: [Screen; 8] = [
        Screen::Agents,
        Screen::Questions,
        Screen::Terminal,
        Screen::Console,
        Screen::Runtimes,
        Screen::Journal,
        Screen::Leases,
        Screen::Settings,
    ];

    fn title(self) -> &'static str {
        match self {
            Screen::Agents => "Agents",
            Screen::Questions => "Questions",
            Screen::Terminal => "Terminal",
            Screen::Console => "Console",
            Screen::Runtimes => "Runtimes",
            Screen::Journal => "Journal",
            Screen::Leases => "Leases",
            Screen::Settings => "Settings",
        }
    }
}

/// What the worker is asked to do.
enum Cmd {
    Agents,
    Leases,
    Runtimes,
    Discovered,
    Journal(String),
    Activity,
    /// Register the person at the keyboard, so agents can address them.
    Me,
    Questions,
    Answer(MessageId, String),
    Adopt(u32),
    AdoptAll,
    Stop(String),
    Setup(Vec<String>),
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
    Journal(String, Vec<JournalEntry>),
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
    Console(String),
}

pub struct App {
    smoke: Option<crate::smoke::Smoke>,
    setup_plan: Option<serde_json::Value>,
    setup_health: Option<serde_json::Value>,
    setup_history: Vec<serde_json::Value>,
    setup_busy: bool,
    tx: Sender<Cmd>,
    rx: Receiver<Msg>,
    screen: Screen,
    agents: Vec<AgentRecord>,
    leases: Vec<Lease>,
    runtimes: Vec<RuntimeInfo>,
    discovered: Vec<DiscoveredProcess>,
    journal: Vec<JournalEntry>,
    journal_project: Option<String>,
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
        let (cmd_tx, cmd_rx) = channel::<Cmd>();
        let (msg_tx, msg_rx) = channel::<Msg>();
        spawn_worker(client.clone(), cmd_rx, msg_tx.clone(), cc.egui_ctx.clone());
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
            tx: cmd_tx,
            rx: msg_rx,
            screen: Screen::Agents,
            agents: Vec::new(),
            leases: Vec::new(),
            runtimes: Vec::new(),
            discovered: Vec::new(),
            journal: Vec::new(),
            journal_project: None,
            smoke: None,
            setup_plan: None,
            setup_health: None,
            setup_history: Vec::new(),
            setup_busy: false,
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
            console_history: Vec::new(),
            console_recall: None,
            console_focused: false,
            settings: crate::theme::Settings::load(&home),
            home,
            applied: None,
            questions: Vec::new(),
            answers: BTreeMap::new(),
            sending: std::collections::BTreeSet::new(),
            activity: BTreeMap::new(),
            focus: None,
        }
    }

    /// The window's state without a window or a daemon, for tests.
    #[cfg(test)]
    fn bare(tx: Sender<Cmd>, rx: Receiver<Msg>) -> Self {
        Self {
            tx,
            rx,
            screen: Screen::Agents,
            agents: Vec::new(),
            leases: Vec::new(),
            runtimes: Vec::new(),
            discovered: Vec::new(),
            journal: Vec::new(),
            journal_project: None,
            smoke: None,
            setup_plan: None,
            setup_health: None,
            setup_history: Vec::new(),
            setup_busy: false,
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
            console_history: Vec::new(),
            console_recall: None,
            console_focused: false,
            settings: crate::theme::Settings::default(),
            home: std::path::PathBuf::new(),
            applied: None,
            questions: Vec::new(),
            answers: BTreeMap::new(),
            sending: std::collections::BTreeSet::new(),
            activity: BTreeMap::new(),
            focus: None,
        }
    }

    fn send(&self, cmd: Cmd) {
        let _ = self.tx.send(cmd);
    }

    /// Take everything the threads sent since the last frame.
    fn drain(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Agents(agents) => self.agents = agents,
                Msg::Leases(leases) => self.leases = leases,
                Msg::Runtimes(runtimes) => self.runtimes = runtimes,
                Msg::Discovered(found) => self.discovered = found,
                Msg::Journal(project, entries) => {
                    if self.journal_project.as_deref() == Some(project.as_str()) {
                        self.journal = entries;
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
                            self.status = "answered".to_owned();
                        }
                        // The draft stays exactly where it was, so nothing
                        // typed is lost to a daemon that was not listening.
                        Err(reason) => self.status = reason,
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
                        ] {
                            self.send(cmd);
                        }
                        if let Some(project) = &self.journal_project {
                            self.send(Cmd::Journal(project.clone()));
                        }
                    }
                }
                Msg::Disconnected(reason) => self.connected = Err(reason),
                Msg::Status(text) => self.status = text,
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
                                self.status = format!(
                                    "{count} saved setup receipt(s) could not be read; the files were preserved"
                                );
                            }
                            self.setup_history =
                                value["plans"].as_array().cloned().unwrap_or_default();
                        }
                        Ok(value) => {
                            self.status =
                                format!("Setup {}", value["phase"].as_str().unwrap_or("updated"));
                            self.setup_plan = Some(value);
                            self.send(Cmd::Runtimes);
                        }
                        Err(error) => self.status = error,
                    }
                }
                Msg::Console(text) => {
                    // Appended, not replaced: a terminal keeps what it
                    // said, and the command that produced this is
                    // already above it.
                    self.console_output.push_str(text.trim_end());
                    self.console_output.push('\n');
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

    /// Runtimes with no way to tell the daemon what their agents are
    /// doing, by the name an agent registers under.
    fn unwired(&self) -> std::collections::BTreeSet<String> {
        self.runtimes
            .iter()
            .filter(|r| {
                r.mcp == agentdocker_core::Wiring::Missing
                    || r.hooks == agentdocker_core::Wiring::Missing
            })
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
        let mut attach: Option<String> = None;
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
                    if blocked > 0 {
                        ui.label(
                            RichText::new(format!("{blocked} blocked"))
                                .color(Color32::from_rgb(200, 140, 60)),
                        );
                    } else if working > 0 && working == agents.len() {
                        ui.label(
                            RichText::new("all working").color(Color32::from_rgb(60, 170, 90)),
                        );
                    } else if working > 0 {
                        ui.label(
                            RichText::new(format!("{working} working"))
                                .color(Color32::from_rgb(60, 170, 90)),
                        );
                    } else {
                        ui.label(RichText::new("all idle").weak());
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
                                    .color(Color32::from_rgb(200, 140, 60)),
                                )
                                .on_hover_text("Waiting for a lease another agent holds.");
                            }
                            Some(Activity::Working { .. }) => {
                                ui.label(
                                    RichText::new("working").color(Color32::from_rgb(60, 170, 90)),
                                );
                            }
                            Some(other) => {
                                let label = ui.label(RichText::new(other.label()).weak());
                                // "idle" is a real answer, and it is
                                // also what an unwired runtime always
                                // says. Which one this is belongs on
                                // the cell, not in a paragraph under
                                // the table.
                                if unwired.contains(&agent.spec.runtime) {
                                    label.on_hover_text(
                                        "Not wired up, so it reports nothing — Runtimes.",
                                    );
                                }
                            }
                            None => {
                                ui.label(RichText::new(agent.status.to_string()).weak());
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
                        if ui.small_button("Stop").clicked() {
                            stop = Some(agent.id.to_string());
                        }
                        if agent.spec.tty && ui.small_button("Attach").clicked() {
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
        if let Some(id) = stop {
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
                        if ui.small_button("Adopt").clicked() {
                            adopt = Some(process.pid);
                        }
                        ui.end_row();
                    }
                });
            if ui.button("Adopt all").clicked() {
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
                    "No agent has a terminal. Start one with `agentdocker run --tty -- <command>`, \
                     or `tty = true` in an Agentfile entry.",
                );
                return;
            }
            let mut attach: Option<String> = None;
            for agent in attachable {
                ui.horizontal(|ui| {
                    ui.label(&agent.spec.name);
                    if ui.button("Attach").clicked() {
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
                    let green = Color32::from_rgb(60, 170, 90);
                    crate::projects::bullet(ui, green);
                    ui.label(RichText::new("live").color(green));
                }
                Status::Ended(reason) => {
                    crate::projects::bullet(ui, Color32::GRAY);
                    ui.label(RichText::new(reason).color(Color32::GRAY));
                }
            }
            if terminal.scrolled_back() {
                ui.label(RichText::new("· scrolled back").weak());
                if ui.button("Jump to live").clicked() {
                    terminal.scroll(i32::MIN / 2);
                }
            }
            if ui.button("Detach").clicked() {
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
                if ui
                    .add_enabled(!in_flight, egui::Button::new("Answer"))
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

    /// Any command the CLI has, run from the window.
    ///
    /// Not a shell, and not a second system terminal — the machine has
    /// one of those and being another is somebody else's job. What it
    /// takes from the terminal is the feel: the same monospace on the
    /// same dark ground, a prompt that stays at the foot of a
    /// transcript, the last commands on the up arrow, and output that
    /// accumulates instead of a box that is replaced.
    fn console_screen(&mut self, ui: &mut egui::Ui) {
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
            self.console_output
                .push_str(&format!("agentdocker {line}\n"));
            if self.console_history.last() != Some(&line) {
                self.console_history.push(line.clone());
            }
            self.console_recall = None;
            self.console_input.clear();
            self.send(Cmd::Console(line));
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
                    ui.label(text(runtime.mcp.symbol().to_owned()));
                    ui.label(text(runtime.hooks.symbol().to_owned()));
                    ui.label(text(runtime.running.to_string()));
                    let needs_setup = runtime.installed()
                        && (runtime.mcp == agentdocker_core::Wiring::Missing
                            || runtime.hooks == agentdocker_core::Wiring::Missing);
                    if needs_setup
                        && ui
                            .add_enabled(
                                !self.setup_busy,
                                egui::Button::new("Review setup").small(),
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
        if ui
            .add_enabled(!self.setup_busy, egui::Button::new("Check connections"))
            .clicked()
        {
            self.setup_busy = true;
            self.send(Cmd::Setup(vec!["--health".into()]));
        }
        if ui
            .add_enabled(!self.setup_busy, egui::Button::new("Saved setup plans"))
            .clicked()
        {
            self.setup_busy = true;
            self.send(Cmd::Setup(vec!["--list".into()]));
        }
        let mut selected = None;
        for plan in &self.setup_history {
            let id = plan["id"].as_str().unwrap_or("");
            let phase = plan["phase"].as_str().unwrap_or("unknown");
            if ui
                .add_enabled(
                    !self.setup_busy,
                    egui::Button::new(format!(
                        "{phase} · {}",
                        id.chars().take(8).collect::<String>()
                    )),
                )
                .clicked()
            {
                selected = Some(id.to_owned());
            }
        }
        if let Some(id) = selected {
            self.setup_busy = true;
            self.send(Cmd::Setup(vec!["--show".into(), id]));
        }
        if self.setup_busy {
            ui.label("Checking setup…");
        }
        if let Some(health) = &self.setup_health {
            ui.label(format!(
                "Daemon: {}",
                health["daemon"].as_str().unwrap_or("unknown")
            ));
            ui.label("Configuration detection does not prove that a provider has used its connection. Start a fresh session after setup.");
        }
        let Some(plan) = self.setup_plan.clone() else {
            return;
        };
        let phase = plan["phase"].as_str().unwrap_or("unknown");
        ui.heading(format!("Setup: {phase}"));
        ui.label(format!("Plan {}", plan["id"].as_str().unwrap_or("")));
        if let Some(executable) = plan["executable"].as_str() {
            ui.label(format!("Connect through {executable}"));
        }
        let changes = plan["changes"].as_array();
        if let Some(changes) = changes {
            for change in changes {
                ui.label(format!(
                    "{} · {} · {}",
                    change["runtime"].as_str().unwrap_or(""),
                    change["channel"].as_str().unwrap_or(""),
                    change["path"].as_str().unwrap_or("")
                ));
            }
        }
        if let Some(notes) = plan["notes"].as_array() {
            for note in notes {
                if let Some(note) = note.as_str() {
                    ui.label(note);
                }
            }
        }
        let id = plan["id"].as_str().unwrap_or("").to_owned();
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    !self.setup_busy
                        && matches!(phase, "prepared" | "applying")
                        && changes.is_some_and(|changes| !changes.is_empty()),
                    egui::Button::new("Apply changes"),
                )
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
                .clicked()
            {
                self.setup_busy = true;
                self.send(Cmd::Setup(vec!["--undo".into(), id]));
            }
            if ui
                .add_enabled(!self.setup_busy, egui::Button::new("Close preview"))
                .clicked()
            {
                self.setup_plan = None;
            }
        });
    }

    fn journal_screen(&mut self, ui: &mut egui::Ui) {
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
        if ui.button("Reset").clicked() {
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
            ui.label("No leases held.");
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

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.drain();
        ui.ctx().request_repaint_after(Duration::from_millis(500));
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
                    ui.heading("AgentDocker");
                    ui.separator();
                    match &self.connected {
                        Ok(()) => {
                            let green = Color32::from_rgb(60, 170, 90);
                            crate::projects::bullet(ui, green);
                            ui.label(RichText::new("connected").color(green));
                            ui.label(RichText::new(&self.socket).color(Color32::GRAY));
                        }
                        Err(reason) => {
                            let red = Color32::from_rgb(200, 80, 60);
                            crate::projects::bullet(ui, red);
                            ui.label(RichText::new("disconnected").color(red));
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
                    if !self.status.is_empty() {
                        ui.separator();
                        ui.label(&self.status);
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
                    Screen::Console => self.console_screen(ui),
                    Screen::Terminal => {}
                    Screen::Runtimes => self.runtimes_screen(ui),
                    Screen::Journal => self.journal_screen(ui),
                    Screen::Leases => self.leases_screen(ui),
                    Screen::Settings => self.settings_screen(ui),
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
fn span(secs: i64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s => format!("{}h", s / 3600),
    }
}

// ----- threads ---------------------------------------------------------------

fn spawn_worker(client: Arc<Client>, rx: Receiver<Cmd>, tx: Sender<Msg>, ctx: egui::Context) {
    std::thread::spawn(move || {
        while let Ok(cmd) = rx.recv() {
            let talks_to_daemon = !matches!(cmd, Cmd::Setup(_) | Cmd::Console(_));
            let outcome = run(&client, cmd);
            let disconnected = outcome.is_err();
            let msg = match outcome {
                Ok(Some(msg)) => msg,
                Ok(None) => continue,
                Err(err) => Msg::Disconnected(err.to_string()),
            };
            let _ = tx.send(msg);
            if !disconnected && talks_to_daemon {
                let _ = tx.send(Msg::Connected);
            }
            ctx.request_repaint();
        }
    });
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
            limit: 200,
            digest: None,
        })? {
            Response::Journal { entries, .. } => Some(Msg::Journal(project, entries)),
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
        Cmd::Questions => match client.call(&Request::Questions {
            agent: Some(agentdocker_core::HUMAN.to_owned()),
        })? {
            Response::Questions { questions } => Some(Msg::Questions(questions)),
            _ => None,
        },
        Cmd::Answer(message, text) => Some(Msg::Answered(
            message.clone(),
            client
                .call(&Request::Answer {
                    from: None,
                    message,
                    text,
                })
                .map(|_| ())
                .map_err(|err| err.to_string()),
        )),
        Cmd::Adopt(pid) => Some(
            match client.call(&Request::Adopt {
                pid,
                name: None,
                runtime: None,
            }) {
                Ok(Response::Agent { agent }) => {
                    Msg::Status(format!("adopted {}", agent.spec.name))
                }
                Ok(_) => Msg::Status(format!("adopted pid {pid}")),
                Err(err) => Msg::Status(format!("pid {pid}: {err}")),
            },
        ),
        Cmd::AdoptAll => {
            let Response::Processes { processes } = client.call(&Request::Discover)? else {
                return Ok(None);
            };
            let mut adopted = 0;
            for process in processes {
                if client
                    .call(&Request::Adopt {
                        pid: process.pid,
                        name: None,
                        runtime: None,
                    })
                    .is_ok()
                {
                    adopted += 1;
                }
            }
            Some(Msg::Status(format!("adopted {adopted} process(es)")))
        }
        Cmd::Stop(agent) => Some(
            match client.call(&Request::Stop {
                agent: agent.clone(),
                force: false,
            }) {
                Ok(_) => Msg::Status(format!(
                    "stopping {}",
                    agent.chars().take(12).collect::<String>()
                )),
                Err(err) => Msg::Status(err.to_string()),
            },
        ),
        Cmd::Setup(args) => Some(Msg::Setup(setup(&args))),
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

/// The named binary next to this one, else whatever is on `PATH`.
fn beside(name: &str) -> std::path::PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|me| me.parent().map(|dir| dir.join(name)))
        .filter(|sibling| sibling.is_file())
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

fn spawn_events(client: Arc<Client>, tx: Sender<Msg>, ctx: egui::Context) {
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

    #[test]
    fn reconnect_refreshes_all_snapshots_including_the_selected_journal() {
        let (tx, requests) = channel::<Cmd>();
        let (messages, rx) = channel::<Msg>();
        let mut app = App::bare(tx, rx);
        app.connected = Err("offline".into());
        app.journal_project = Some("project-a".into());
        messages.send(Msg::Connected).unwrap();
        app.drain();
        let received: Vec<_> = requests.try_iter().collect();
        assert!(app.connected.is_ok());
        assert_eq!(received.len(), 8);
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
        let (tx, requests) = channel::<Cmd>();
        let (_mtx, mrx) = channel::<Msg>();
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
        assert!(requests.try_iter().count() >= 2, "and each one is acted on");
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
