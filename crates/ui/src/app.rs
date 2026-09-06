//! The window: what it shows, how it asks the daemon, and how it keeps
//! up. Requests run on a worker thread and the event stream on another;
//! both hand results to the UI thread through a channel and ask for a
//! repaint, so the window never blocks on the socket.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use agentdocker_core::journal::ago;
use agentdocker_core::{
    AgentRecord, DiscoveredProcess, Event, EventKind, JournalEntry, Lease, ProjectRef, Request,
    Response, RuntimeInfo,
};
use chrono::Utc;
use egui::{Color32, RichText};

use crate::client::Client;
use crate::terminal::{Status, Terminal};

/// Events kept for the feed.
const EVENT_HISTORY: usize = 500;
/// How often agents, leases and discovered processes are re-read.
const REFRESH: Duration = Duration::from_secs(2);
/// How often the runtime inventory is re-read (it asks each CLI).
const RUNTIMES_REFRESH: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Screen {
    Agents,
    Terminal,
    Console,
    Runtimes,
    Journal,
    Leases,
    Events,
}

impl Screen {
    const ALL: [Screen; 7] = [
        Screen::Agents,
        Screen::Terminal,
        Screen::Console,
        Screen::Runtimes,
        Screen::Journal,
        Screen::Leases,
        Screen::Events,
    ];

    fn title(self) -> &'static str {
        match self {
            Screen::Agents => "Agents",
            Screen::Terminal => "Terminal",
            Screen::Console => "Console",
            Screen::Runtimes => "Runtimes",
            Screen::Journal => "Journal",
            Screen::Leases => "Leases",
            Screen::Events => "Events",
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
    Adopt(u32),
    AdoptAll,
    Stop(String),
    Setup(String),
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
    Event(Box<Event>),
    Connected,
    Disconnected(String),
    Status(String),
    Console(String),
}

pub struct App {
    tx: Sender<Cmd>,
    rx: Receiver<Msg>,
    screen: Screen,
    agents: Vec<AgentRecord>,
    leases: Vec<Lease>,
    runtimes: Vec<RuntimeInfo>,
    discovered: Vec<DiscoveredProcess>,
    journal: Vec<JournalEntry>,
    journal_project: Option<String>,
    events: VecDeque<Event>,
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
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let client = Arc::new(Client::from_env());
        let (cmd_tx, cmd_rx) = channel::<Cmd>();
        let (msg_tx, msg_rx) = channel::<Msg>();
        spawn_worker(client.clone(), cmd_rx, msg_tx.clone(), cc.egui_ctx.clone());
        spawn_events(client.clone(), msg_tx, cc.egui_ctx.clone());
        for cmd in [Cmd::Agents, Cmd::Leases, Cmd::Discovered, Cmd::Runtimes] {
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
            events: VecDeque::new(),
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
            events: VecDeque::new(),
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
                Msg::Event(event) => self.on_event(*event),
                Msg::Connected => {
                    if self.connected.is_err() {
                        self.connected = Ok(());
                        for cmd in [Cmd::Agents, Cmd::Leases, Cmd::Discovered, Cmd::Runtimes] {
                            self.send(cmd);
                        }
                        if let Some(project) = &self.journal_project {
                            self.send(Cmd::Journal(project.clone()));
                        }
                    }
                }
                Msg::Disconnected(reason) => self.connected = Err(reason),
                Msg::Status(text) => self.status = text,
                Msg::Console(text) => {
                    self.console_output = text;
                    self.screen = Screen::Console;
                }
            }
        }
        // Nothing is asked of a daemon that is not there: each request
        // would wait out its start timeout, and the queue would outrun the
        // worker. Coming back re-reads everything anyway.
        if self.connected.is_ok() && self.last_refresh.elapsed() >= REFRESH {
            self.last_refresh = Instant::now();
            for cmd in [Cmd::Agents, Cmd::Leases, Cmd::Discovered] {
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
            EventKind::JournalAppended { entry }
                if self.journal_project.as_deref() == Some(entry.project.as_str())
                    && self.journal.last().is_none_or(|last| last.seq < entry.seq) =>
            {
                self.journal.push(entry.clone());
            }
            _ => {}
        }
        self.events.push_front(event);
        while self.events.len() > EVENT_HISTORY {
            self.events.pop_back();
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
        let mut groups: BTreeMap<String, Vec<&AgentRecord>> = BTreeMap::new();
        for agent in self.agents.iter().filter(|a| a.status.is_live()) {
            let key = agent
                .project
                .as_ref()
                .map(ProjectRef::name)
                .unwrap_or_else(|| "no project".to_owned());
            groups.entry(key).or_default().push(agent);
        }
        let mut stop: Option<String> = None;
        let mut attach: Option<String> = None;
        if groups.is_empty() {
            ui.label("No live agents. Start one with `agentdocker run`, or adopt one below.");
        }
        for (project, agents) in &groups {
            ui.heading(project);
            egui::Grid::new(format!("agents-{project}"))
                .striped(true)
                .num_columns(7)
                .show(ui, |ui| {
                    for header in ["NAME", "RUNTIME", "STATUS", "BRANCH", "LEASES", "SEEN", ""] {
                        ui.label(RichText::new(header).strong());
                    }
                    ui.end_row();
                    for agent in agents {
                        ui.label(&agent.spec.name);
                        ui.label(&agent.spec.runtime);
                        ui.label(agent.status.to_string());
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
                });
            ui.add_space(8.0);
        }
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
            ui.label("Nothing found. The daemon scans every five seconds for Claude Code, Codex, Gemini CLI and other known agents.");
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
        // A 13pt monospace cell, near enough: the agent lays itself out to
        // whatever size we report, so a pixel here or there is harmless.
        const CELL: (f32, f32) = (7.8, 16.0);
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
                    ui.label(RichText::new("● live").color(Color32::from_rgb(60, 170, 90)));
                }
                Status::Ended(reason) => {
                    ui.label(RichText::new(format!("● {reason}")).color(Color32::GRAY));
                }
            }
            if ui.button("Detach").clicked() {
                detach = true;
            }
        });
        ui.separator();

        let space = ui.available_size();
        terminal.resize(
            (space.x / CELL.0) as u16,
            ((space.y / CELL.1) as u16).saturating_sub(1),
        );
        // Everything typed while this screen is up goes to the agent.
        if terminal.status() == Status::Attached {
            let events = ui.input(|i| i.events.clone());
            let bytes = crate::terminal::keystrokes(&events);
            if !bytes.is_empty() {
                terminal.send(bytes);
            }
        }
        egui::ScrollArea::both()
            .stick_to_bottom(true)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                terminal.ui(ui);
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

    /// Any command the CLI has, run from the window.
    fn console_screen(&mut self, ui: &mut egui::Ui) {
        ui.label(
            "Every `agentdocker` command, run here. The command line is the whole surface, so this \
             is the whole surface.",
        );
        let mut run = false;
        ui.horizontal(|ui| {
            ui.label("agentdocker");
            let entry = ui.add(
                egui::TextEdit::singleline(&mut self.console_input)
                    .desired_width(f32::INFINITY)
                    .hint_text("ps --all   ·   journal --new   ·   channels   ·   runtimes"),
            );
            if entry.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                run = true;
            }
        });
        ui.horizontal(|ui| {
            if ui.button("Run").clicked() {
                run = true;
            }
            if ui.button("Clear").clicked() {
                self.console_output.clear();
            }
        });
        if run && !self.console_input.trim().is_empty() {
            let line = self.console_input.clone();
            self.console_output = format!("$ agentdocker {line}\n");
            self.send(Cmd::Console(line));
        }
        ui.separator();
        egui::ScrollArea::both().show(ui, |ui| {
            ui.add(
                egui::Label::new(egui::RichText::new(&self.console_output).monospace())
                    .wrap_mode(egui::TextWrapMode::Extend),
            );
        });
    }

    fn runtimes_screen(&mut self, ui: &mut egui::Ui) {
        ui.label("The agent tools on this machine, and whether AgentDocker is wired into each.");
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
                    if needs_setup && ui.small_button("Set up").clicked() {
                        setup = Some(runtime.name.clone());
                    }
                    ui.end_row();
                }
            });
        if let Some(name) = setup {
            self.send(Cmd::Setup(name));
        }
        ui.add_space(8.0);
        if ui.button("Refresh").clicked() {
            self.send(Cmd::Runtimes);
        }
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
            ui.label("Nothing in the journal yet. Releases, notes, commits, arrivals and handoffs land here.");
        }
        for entry in &self.journal {
            ui.horizontal(|ui| {
                ui.monospace(format!("{:>6}", entry.seq));
                ui.label(RichText::new(ago(now, entry.at)).color(Color32::GRAY));
                ui.label(entry.line());
            });
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
            .num_columns(5)
            .show(ui, |ui| {
                for header in ["RESOURCE", "HOLDER", "MODE", "EXPIRES", "NOTE"] {
                    ui.label(RichText::new(header).strong());
                }
                ui.end_row();
                for lease in &self.leases {
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

    fn events_screen(&mut self, ui: &mut egui::Ui) {
        if self.events.is_empty() {
            ui.label("Waiting for events.");
        }
        for event in &self.events {
            ui.horizontal(|ui| {
                ui.monospace(
                    event
                        .at
                        .with_timezone(&chrono::Local)
                        .format("%H:%M:%S")
                        .to_string(),
                );
                ui.label(summary(&event.kind));
            });
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.drain();
        ui.ctx().request_repaint_after(Duration::from_millis(500));

        egui::Panel::top("top").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("AgentDocker");
                ui.separator();
                match &self.connected {
                    Ok(()) => {
                        ui.label(
                            RichText::new("● connected").color(Color32::from_rgb(60, 170, 90)),
                        );
                        ui.label(RichText::new(&self.socket).color(Color32::GRAY));
                    }
                    Err(reason) => {
                        ui.label(
                            RichText::new("● disconnected").color(Color32::from_rgb(200, 80, 60)),
                        );
                        ui.label(RichText::new(reason).color(Color32::GRAY));
                    }
                }
                if !self.status.is_empty() {
                    ui.separator();
                    ui.label(&self.status);
                }
            });
        });
        egui::Panel::left("nav").resizable(false).show(ui, |ui| {
            ui.add_space(6.0);
            for screen in Screen::ALL {
                let label = match screen {
                    Screen::Agents => format!(
                        "Agents ({})",
                        self.agents.iter().filter(|a| a.status.is_live()).count()
                    ),
                    Screen::Leases => format!("Leases ({})", self.leases.len()),
                    Screen::Events => format!("Events ({})", self.events.len()),
                    other => other.title().to_owned(),
                };
                if ui.selectable_label(self.screen == screen, label).clicked() {
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
            egui::ScrollArea::vertical().show(ui, |ui| match self.screen {
                Screen::Agents => self.agents_screen(ui),
                Screen::Console => self.console_screen(ui),
                Screen::Terminal => {}
                Screen::Runtimes => self.runtimes_screen(ui),
                Screen::Journal => self.journal_screen(ui),
                Screen::Leases => self.leases_screen(ui),
                Screen::Events => self.events_screen(ui),
            });
        });
    }
}

/// "45s", "3m", "2h".
fn span(secs: i64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s => format!("{}h", s / 3600),
    }
}

/// One line for an event, from its tag and the few fields worth reading.
fn summary(kind: &EventKind) -> String {
    match kind {
        EventKind::JournalAppended { entry } => format!("journal: {}", entry.line()),
        EventKind::AgentCreated { name, .. } => format!("agent created: {name}"),
        EventKind::AgentExited { agent, status } => {
            format!("agent exited: {} ({status})", agent.short())
        }
        EventKind::AgentDiscovered { pid, runtime, .. } => {
            format!("agent found: {runtime} pid {pid}")
        }
        EventKind::AgentVanished {
            pid,
            runtime,
            adopted,
            ..
        } => format!(
            "agent {}: {runtime} pid {pid}",
            if *adopted { "adopted" } else { "gone" }
        ),
        EventKind::LeaseClaimed { lease } => {
            format!(
                "lease claimed: {} by {}",
                lease.resource.as_str(),
                lease.holder.short()
            )
        }
        EventKind::LeaseReleased { lease } => {
            format!(
                "lease released: {} by {}",
                lease.resource.as_str(),
                lease.holder.short()
            )
        }
        EventKind::LeaseExpired { lease } => format!("lease expired: {}", lease.resource.as_str()),
        EventKind::LeaseConflict {
            resource,
            requester,
            ..
        } => format!(
            "lease conflict: {} wanted by {}",
            resource.as_str(),
            requester.short()
        ),
        EventKind::MessageSent { from, to, kind, .. } => {
            format!(
                "message: {} → {to} [{kind}]",
                from.chars().take(12).collect::<String>()
            )
        }
        other => {
            let value = serde_json::to_value(other).unwrap_or_default();
            let tag = value
                .get("event")
                .and_then(|t| t.as_str())
                .unwrap_or("event")
                .replace('_', " ");
            let mut rest = value;
            if let Some(object) = rest.as_object_mut() {
                object.remove("event");
            }
            let detail: String = rest.to_string().chars().take(120).collect();
            format!("{tag}: {detail}")
        }
    }
}

// ----- threads ---------------------------------------------------------------

fn spawn_worker(client: Arc<Client>, rx: Receiver<Cmd>, tx: Sender<Msg>, ctx: egui::Context) {
    std::thread::spawn(move || {
        while let Ok(cmd) = rx.recv() {
            let outcome = run(&client, cmd);
            let disconnected = outcome.is_err();
            let msg = match outcome {
                Ok(Some(msg)) => msg,
                Ok(None) => continue,
                Err(err) => Msg::Disconnected(err.to_string()),
            };
            let _ = tx.send(msg);
            if !disconnected {
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
        Cmd::Setup(runtime) => Some(Msg::Status(setup(&runtime))),
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
    match std::process::Command::new(&cli).args(&args).output() {
        Ok(output) => {
            let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
            let errors = String::from_utf8_lossy(&output.stderr);
            if !errors.trim().is_empty() {
                if !text.is_empty() && !text.ends_with('\n') {
                    text.push('\n');
                }
                text.push_str(&errors);
            }
            if text.trim().is_empty() {
                text = format!("({})", output.status);
            }
            text
        }
        Err(err) => format!("cannot run {}: {err}", cli.display()),
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

/// `agentdocker setup <runtime>`, with the CLI beside this binary: it
/// writes the runtime's configuration, and the app shows what it said.
fn setup(runtime: &str) -> String {
    let cli = beside("agentdocker");
    let Some(cli_arg) = cli.to_str() else {
        return "CLI path is not UTF-8".into();
    };
    let argv = [cli_arg.to_owned(), "setup".into(), runtime.to_owned()];
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(error) => return error.to_string(),
    };
    match agentdocker_host::command::run(&cwd, &argv, Duration::from_secs(60)) {
        Ok(output) => {
            let text = output
                .text
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .collect::<Vec<_>>()
                .join(" · ");
            if output.success {
                text
            } else {
                format!("Setup failed: {text}")
            }
        }
        Err(err) => format!("cannot run {}: {err}", cli.display()),
    }
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
        assert_eq!(received.len(), 5);
        assert!(received.iter().any(|cmd| matches!(cmd, Cmd::Agents)));
        assert!(received.iter().any(|cmd| matches!(cmd, Cmd::Leases)));
        assert!(received.iter().any(|cmd| matches!(cmd, Cmd::Discovered)));
        assert!(received.iter().any(|cmd| matches!(cmd, Cmd::Runtimes)));
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
        let (tx, _rx) = channel::<Cmd>();
        let (_mtx, mrx) = channel::<Msg>();
        let mut app = App::bare(tx, mrx);
        app.on_event(stopping(1));
        app.on_event(stopping(2));
        assert_eq!(app.events.len(), 2);
        // A reconnect replays what was already taken.
        app.on_event(stopping(1));
        app.on_event(stopping(2));
        assert_eq!(app.events.len(), 2, "replayed events are not taken twice");
        app.on_event(stopping(3));
        assert_eq!(app.events.len(), 3, "and newer ones still are");
        // Live-only events carry no sequence and always count.
        let mut live = Event::new(
            EventKind::WatcherGap {
                reason: "overflow".into(),
            },
            Utc::now(),
        );
        live.seq = 0;
        app.on_event(live.clone());
        app.on_event(live);
        assert_eq!(app.events.len(), 5);
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
    fn spans_and_summaries_read_well() {
        assert_eq!(span(45), "45s");
        assert_eq!(span(180), "3m");
        assert_eq!(span(7200), "2h");
        let found = EventKind::AgentDiscovered {
            pid: 42,
            started_at: None,
            runtime: "codex".into(),
            project: None,
            cwd: None,
        };
        assert_eq!(summary(&found), "agent found: codex pid 42");
        let other = EventKind::DaemonStopping {
            reason: "signal".into(),
        };
        assert_eq!(summary(&other), "daemon stopping: {\"reason\":\"signal\"}");
    }
}
