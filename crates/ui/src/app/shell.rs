//! Iced state transitions. Effects stay on bounded workers or explicit tasks.
use super::*;
use crate::catalog::Catalog;
use iced::{Subscription, Task, keyboard, window};
use std::path::PathBuf;

#[derive(Default)]
pub(super) struct State {
    pub catalog: Catalog,
    pub selected: Option<String>,
    pub search: String,
    pub session_filter: super::sessions::Filter,
    pub more: bool,
    pub session_details: bool,
    pub connection_details: Option<String>,
    pub other_tools: bool,
    pub width: f32,
    pub height: f32,
    pub dpi: f32,
    pub add_path: String,
    pub adding: bool,
    pub launch: bool,
    pub launch_runtime: Option<String>,
    pub launch_name: String,
    pub launch_arguments: String,
    pub launching: bool,
    pub error: Option<String>,
    pub setup_error: Option<String>,
    pub answer_errors: BTreeMap<MessageId, String>,
    pub channel_drafts: BTreeMap<String, ChannelDraft>,
    pub channel_target: Option<String>,
    pub generation: u64,
    pub saved_generation: u64,
    pub saving: bool,
    pub save_enabled: bool,
    pub window: Option<window::Id>,
    pub closing: bool,
    pub checked_project: Option<PathBuf>,
    pub project_available: Option<bool>,
    pub notification_message: Option<MessageId>,
    pending_notification: Option<(agentdocker_host::notify::Action, Instant)>,
}

/// Each room keeps its own draft; a late acknowledgement only clears the text sent.
#[derive(Clone, Debug, Default)]
pub(super) struct ChannelDraft {
    pub text: String,
    pub sending: Option<String>,
    pub error: Option<String>,
}
impl ChannelDraft {
    pub fn begin(&mut self) -> Option<String> {
        if self.sending.is_some() || self.text.trim().is_empty() {
            return None;
        }
        self.error = None;
        self.sending = Some(self.text.clone());
        self.sending.clone()
    }
    pub fn complete(&mut self, result: Result<(), String>) {
        let sent = self.sending.take();
        match result {
            Ok(()) if sent.as_ref() == Some(&self.text) => self.text.clear(),
            Ok(()) => {}
            Err(error) => self.error = Some(error),
        }
    }
}

impl State {
    pub fn load(home: &std::path::Path) -> Self {
        let (catalog, error, save_enabled) = match Catalog::load(home) {
            Ok(catalog) => (catalog, None, true),
            Err(error) => (
                Catalog::default(),
                Some(format!(
                    "Cannot load saved workspace: {error}. The file has been preserved; changes in this window will not overwrite it."
                )),
                false,
            ),
        };
        Self {
            catalog,
            error,
            save_enabled,
            dpi: 1.0,
            width: 1180.0,
            height: 760.0,
            ..Default::default()
        }
    }
    pub fn changed(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }
}

#[derive(Clone, Debug)]
pub enum Message {
    Tick,
    Notification(crate::notification_route::Activation),
    Navigate(Screen),
    SelectProject(PathBuf),
    Unassigned,
    RetryProject,
    SelectSession(String),
    CloseSession,
    Search(String),
    SessionFilter(super::sessions::Filter),
    More,
    SessionDetails,
    ConnectionDetails(String),
    OtherTools,
    AddPath(String),
    ShowAdd,
    PickFolder,
    FolderPicked(Option<PathBuf>),
    ResolveFolder,
    FolderResolved(Result<ProjectRef, String>),
    ProjectLocation(PathBuf, bool),
    Unpin,
    ForgetProject,
    CatalogSaved(u64, Result<(), String>),
    Draft(MessageId, String),
    Answer(MessageId),
    DismissInbox(Vec<MessageId>),
    Adopt(u32),
    AdoptAll,
    Stop(String),
    ShowLaunch,
    LaunchRuntime(String),
    LaunchName(String),
    LaunchArguments(String),
    Launch,
    Attach(String),
    Detach,
    TerminalInput(Vec<u8>),
    TerminalResize(u16, u16),
    TerminalScroll(i32),
    TerminalDismiss,
    ConsoleInput(String),
    RunConsole,
    Recall(bool),
    ChannelTarget(String),
    ChannelDraft(String),
    SendChannel,
    Setup(Vec<String>),
    SetupClose,
    DesktopSource(String),
    DesktopPrefix(String),
    DesktopLocal(bool),
    DesktopUseCurrent,
    DesktopPreview(String),
    DesktopApply,
    Dark(bool),
    TextSize(f32),
    TerminalSize(f32),
    Palette(String),
    Roomy(bool),
    DismissError,
    Event(iced::Event),
    Window(window::Id),
    WindowScale(f32),
    NativeAccessibility(Result<usize, String>),
    Captured(window::Screenshot),
    Focus(String),
    Accessibility(crate::accessibility::Snapshot),
}

async fn notifications(mut output: iced::futures::channel::mpsc::Sender<Message>) {
    use iced::futures::SinkExt;
    let wake = Wake::default();
    loop {
        wake.notified().await;
        if output.send(Message::Tick).await.is_err() {
            break;
        }
    }
}

impl App {
    pub fn subscription(&self) -> Subscription<Message> {
        fn updates() -> impl iced::futures::Stream<Item = Message> {
            iced::stream::channel(1, notifications)
        }
        Subscription::batch([
            Subscription::run(updates),
            iced::time::every(if self.smoke.is_some() {
                Duration::from_millis(100)
            } else {
                Duration::from_secs(2)
            })
            .map(|_| Message::Tick),
            iced::event::listen_with(|event, status, _| match event {
                iced::Event::Keyboard(_) if status == iced::event::Status::Ignored => {
                    Some(Message::Event(event))
                }
                iced::Event::Window(iced::window::Event::RedrawRequested(_)) => None,
                iced::Event::Window(_) => Some(Message::Event(event)),
                _ => None,
            }),
        ])
    }

    pub fn boot() -> (Self, Task<Message>) {
        (
            Self::new(),
            window::oldest().and_then(|id| Task::done(Message::Window(id))),
        )
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        let mut tasks = Vec::new();
        if matches!(
            &message,
            Message::Navigate(_)
                | Message::SelectProject(_)
                | Message::Unassigned
                | Message::SelectSession(_)
                | Message::Search(_)
        ) {
            self.shell.pending_notification = None;
            self.shell.notification_message = None;
        }
        if matches!(
            &message,
            Message::Event(iced::Event::Window(
                window::Event::Moved(_) | window::Event::Resized(_) | window::Event::Rescaled(_)
            ))
        ) && let Some(id) = self.shell.window
        {
            tasks.push(crate::accessibility::geometry(id));
        }
        match message {
            Message::Tick => {
                for activation in crate::notification_route::take() {
                    tasks.push(self.update(Message::Notification(activation)));
                }
                for action in crate::accessibility::take_actions() {
                    tasks.push(self.update(action));
                }
                self.drain();
                let before = self.shell.catalog.clone();
                for project in self
                    .discovered
                    .iter()
                    .filter_map(|p| p.project.clone())
                    .chain(self.agents.iter().filter_map(|a| a.project.clone()))
                {
                    self.shell.catalog.remember(project, false);
                }
                if before != self.shell.catalog {
                    self.shell.changed();
                }
                self.refresh_project_context();
                tasks.push(self.advance_notification());
                if let Some(smoke) = &mut self.smoke {
                    tasks.push(smoke.tick(
                        self.connected.is_ok(),
                        self.runtimes.len(),
                        &self.discovered,
                    ));
                }
            }
            Message::Notification(activation) => {
                if let Some(id) = self.shell.window {
                    tasks.push(window::minimize(id, false).chain(window::gain_focus(id)));
                }
                match activation {
                    crate::notification_route::Activation::Open(action)
                        if action.home == self.home
                            && self
                                .client
                                .as_ref()
                                .is_some_and(|c| c.socket() == action.socket) =>
                    {
                        self.shell.pending_notification = Some((action, Instant::now()));
                        for cmd in [Cmd::Agents, Cmd::Questions, Cmd::Inbox] {
                            self.send(cmd);
                        }
                        tasks.push(self.advance_notification());
                    }
                    crate::notification_route::Activation::Open(_) => {
                        self.say("This notification belongs to another local workspace.")
                    }
                    crate::notification_route::Activation::Focus => {}
                    crate::notification_route::Activation::Inbox => {
                        self.shell.pending_notification = None;
                        self.shell.notification_message = None;
                        self.screen = Screen::Questions;
                    }
                }
            }
            Message::Window(id) => {
                self.shell.window = Some(id);
                tasks.push(
                    crate::accessibility::install(id)
                        .chain(crate::accessibility::geometry(id))
                        .chain(window::scale_factor(id).map(Message::WindowScale)),
                );
            }
            Message::WindowScale(scale) => self.shell.dpi = scale,
            Message::NativeAccessibility(result) => {
                if let Some(smoke) = &mut self.smoke {
                    tasks.push(smoke.native_accessibility(result));
                }
            }
            Message::Navigate(screen) => {
                self.shell.pending_notification = None;
                self.shell.notification_message = None;
                self.screen = screen;
                self.shell.more = false;
                if screen == Screen::Runtimes {
                    self.send(Cmd::Runtimes);
                }
                if screen == Screen::Desktop {
                    self.send(Cmd::Desktop(self.desktop.command("status")));
                }
            }
            Message::RetryProject => self.shell.checked_project = None,
            Message::Unassigned => {
                self.shell.catalog.unassigned = true;
                self.shell.catalog.selected = None;
                self.shell.selected = None;
                self.shell.search.clear();
                self.reset_session_view();
                self.screen = Screen::Agents;
                self.shell.changed();
                self.refresh_project_context();
            }
            Message::SelectProject(path) => {
                if self
                    .shell
                    .catalog
                    .projects
                    .iter()
                    .any(|e| e.project.root == path)
                {
                    self.shell.catalog.unassigned = false;
                    self.shell.catalog.selected = Some(path);
                    self.shell.selected = None;
                    self.shell.search.clear();
                    self.reset_session_view();
                    self.screen = Screen::Agents;
                    self.shell.changed();
                    self.refresh_project_context();
                }
            }
            Message::SelectSession(id) => {
                self.shell.selected = Some(id);
                self.shell.session_details = false;
            }
            Message::CloseSession => self.shell.selected = None,
            Message::SessionFilter(filter) => {
                self.shell.session_filter = filter;
                self.shell.selected = None;
                self.confirm_stop = None;
            }
            Message::More => self.shell.more = !self.shell.more,
            Message::SessionDetails => self.shell.session_details = !self.shell.session_details,
            Message::ConnectionDetails(name) => {
                self.shell.connection_details =
                    (self.shell.connection_details.as_ref() != Some(&name)).then_some(name);
            }
            Message::OtherTools => self.shell.other_tools = !self.shell.other_tools,
            Message::Search(text) => {
                self.shell.search = text.chars().take(1024).collect();
                self.shell.selected = None;
            }
            Message::ShowAdd => {
                self.shell.adding = !self.shell.adding;
            }
            Message::AddPath(path) => self.shell.add_path = path.chars().take(4096).collect(),
            Message::PickFolder => {
                tasks.push(Task::perform(
                    async {
                        rfd::AsyncFileDialog::new()
                            .set_title("Add project")
                            .pick_folder()
                            .await
                            .map(|f| f.path().to_owned())
                    },
                    Message::FolderPicked,
                ));
            }
            Message::FolderPicked(Some(path)) => tasks.push(resolve_folder(path)),
            Message::FolderPicked(None) => {}
            Message::ResolveFolder => {
                let path = self.shell.add_path.trim();
                let path = if let Some(rest) = path.strip_prefix("~/") {
                    std::env::var_os("HOME")
                        .map(PathBuf::from)
                        .unwrap_or_default()
                        .join(rest)
                } else {
                    PathBuf::from(path)
                };
                tasks.push(resolve_folder(path));
            }
            Message::FolderResolved(result) => match result {
                Ok(project) => {
                    if let Err(error) = self.shell.catalog.pin(project) {
                        self.shell.error = Some(error.to_string());
                    } else {
                        self.shell.adding = false;
                        self.shell.add_path.clear();
                        self.shell.selected = None;
                        self.reset_session_view();
                        self.screen = Screen::Agents;
                        self.shell.changed();
                        self.refresh_project_context();
                    }
                }
                Err(error) => self.shell.error = Some(error),
            },
            Message::ProjectLocation(path, available) => {
                if self.shell.catalog.selected.as_ref() == Some(&path) {
                    self.shell.project_available = Some(available);
                }
            }
            Message::Unpin => {
                let selected = self.shell.catalog.selected.clone();
                if let Some(entry) = self
                    .shell
                    .catalog
                    .projects
                    .iter_mut()
                    .find(|e| Some(&e.project.root) == selected.as_ref())
                {
                    entry.pinned = !entry.pinned;
                    self.shell.changed();
                }
            }
            Message::ForgetProject => {
                let selected = self.shell.catalog.selected.clone();
                self.shell
                    .catalog
                    .projects
                    .retain(|e| Some(&e.project.root) != selected.as_ref());
                self.shell.catalog.selected = self
                    .shell
                    .catalog
                    .projects
                    .first()
                    .map(|e| e.project.root.clone());
                self.shell.selected = None;
                self.reset_session_view();
                self.shell.changed();
                self.refresh_project_context();
            }
            Message::CatalogSaved(generation, result) => {
                self.shell.saving = false;
                match result {
                    Ok(()) => self.shell.saved_generation = generation,
                    Err(error) => {
                        self.shell.error = Some(format!("Could not save workspace: {error}"));
                        self.shell.save_enabled = false;
                    }
                }
            }
            Message::Draft(id, value) => {
                if !self.sending.contains(&id) {
                    self.answers
                        .insert(id, value.chars().take(16_000).collect());
                }
            }
            Message::Answer(id) => {
                if self.connected.is_ok()
                    && !self.sending.contains(&id)
                    && self
                        .questions
                        .iter()
                        .any(|q| q.id == id && !q.expired(Utc::now()))
                    && let Some(answer) = self
                        .answers
                        .get(&id)
                        .filter(|s| !s.trim().is_empty())
                        .cloned()
                {
                    self.shell.answer_errors.remove(&id);
                    self.sending.insert(id.clone());
                    self.send(Cmd::Answer(id, answer));
                }
            }
            Message::DismissInbox(mut ids) => {
                if self.connected.is_ok() && ids.len() <= 1000 {
                    ids.retain(|id| {
                        !self.dismissing.contains(id)
                            && self.inbox.iter().any(|message| &message.id == id)
                            && !self.questions.iter().any(|question| &question.id == id)
                    });
                    ids.sort();
                    ids.dedup();
                    if !ids.is_empty() {
                        self.dismissing.extend(ids.iter().cloned());
                        self.send(Cmd::DismissMessages(ids));
                    }
                }
            }
            Message::AdoptAll => {
                if self.connected.is_ok() {
                    self.send(Cmd::AdoptAll);
                }
            }
            Message::Adopt(pid) => {
                if self.connected.is_ok() && self.discovered.iter().any(|p| p.pid == pid) {
                    self.send(Cmd::Adopt(pid));
                }
            }
            Message::Stop(id) => {
                if self.connected.is_ok()
                    && self
                        .agents
                        .iter()
                        .any(|a| a.id.as_str() == id && a.status.is_live())
                {
                    if self
                        .confirm_stop
                        .as_ref()
                        .is_some_and(|(armed, at)| armed == &id && at.elapsed() < CONFIRM_WITHIN)
                    {
                        self.confirm_stop = None;
                        self.send(Cmd::Stop(id));
                    } else {
                        self.confirm_stop = Some((id, Instant::now()));
                    }
                }
            }
            Message::ShowLaunch => {
                self.shell.launch = !self.shell.launch;
                if self.shell.launch {
                    self.shell.selected = None;
                    self.shell.session_filter = super::sessions::Filter::Current;
                }
            }
            Message::LaunchRuntime(runtime) => self.shell.launch_runtime = Some(runtime),
            Message::LaunchName(name) => self.shell.launch_name = name.chars().take(120).collect(),
            Message::LaunchArguments(args) => {
                self.shell.launch_arguments = args.chars().take(8192).collect()
            }
            Message::Launch => {
                if self.connected.is_ok() && !self.shell.launching {
                    match self.launch_spec() {
                        Ok(spec) => {
                            self.shell.launching = true;
                            self.shell.error = None;
                            self.send(Cmd::Launch(Box::new(spec)));
                        }
                        Err(error) => self.shell.error = Some(error),
                    }
                }
            }
            Message::Attach(id) => {
                if self
                    .agents
                    .iter()
                    .any(|a| a.id.as_str() == id && a.managed && a.spec.tty && a.status.is_live())
                {
                    self.attach(id, self.wake.clone());
                    self.screen = Screen::Terminal;
                    tasks.push(iced::widget::operation::focus(
                        iced::advanced::widget::Id::new("terminal"),
                    ));
                }
            }
            Message::Detach => {
                self.terminal = None;
                self.screen = Screen::Agents;
            }
            Message::TerminalInput(bytes) => {
                if let Some(terminal) = &mut self.terminal {
                    if bytes == [29] {
                        self.terminal = None;
                        self.screen = Screen::Agents;
                    } else {
                        terminal.send(bytes);
                    }
                }
            }
            Message::TerminalResize(cols, rows) => {
                if let Some(terminal) = &mut self.terminal {
                    terminal.resize(cols, rows);
                }
            }
            Message::TerminalScroll(rows) => {
                if let Some(terminal) = &mut self.terminal {
                    terminal.scroll(rows);
                }
            }
            Message::TerminalDismiss => {
                if let Some(terminal) = &mut self.terminal {
                    terminal.input_notice = None;
                }
            }
            Message::ConsoleInput(text) => self.console_input = text.chars().take(16_000).collect(),
            Message::RunConsole => {
                let line = self.console_input.trim().to_owned();
                if !line.is_empty() {
                    self.remember_console_command(&line);
                    self.append_console(&format!("$ {line}\n"));
                    self.console_running += 1;
                    self.send(Cmd::Console(line, self.shell.catalog.selected.clone()));
                }
            }
            Message::Recall(back) => self.recall(back),
            Message::ChannelTarget(id) => {
                self.shell
                    .channel_drafts
                    .retain(|_, draft| !draft.text.is_empty() || draft.sending.is_some());
                if self
                    .channels
                    .iter()
                    .any(|c| c.id.as_str() == id && c.is_open())
                {
                    self.shell.channel_target = Some(id);
                }
            }
            Message::ChannelDraft(text) => {
                if let Some(id) = self.shell.channel_target.clone() {
                    if self.shell.channel_drafts.len() < 128
                        || self.shell.channel_drafts.contains_key(&id)
                    {
                        self.shell.channel_drafts.entry(id).or_default().text =
                            text.chars().take(16_000).collect();
                    } else {
                        self.shell.error = Some(
                            "Finish or clear an earlier channel draft before writing another."
                                .into(),
                        );
                    }
                }
            }
            Message::SendChannel => {
                if self.connected.is_ok()
                    && let Some(id) = self.shell.channel_target.clone()
                    && self
                        .channels
                        .iter()
                        .any(|c| c.id.as_str() == id && c.is_open())
                    && let Some(text) = self
                        .shell
                        .channel_drafts
                        .entry(id.clone())
                        .or_default()
                        .begin()
                {
                    self.send(Cmd::ChannelSend(id, text));
                }
            }
            Message::Setup(args) => {
                if !self.setup_busy {
                    self.shell.setup_error = None;
                    self.setup_busy = true;
                    self.send(Cmd::Setup(args));
                }
            }
            Message::SetupClose => self.setup_plan = None,
            Message::DesktopSource(value) => {
                if !self.desktop.busy {
                    self.desktop.source = value;
                    self.desktop.report = None;
                }
            }
            Message::DesktopPrefix(value) => {
                if !self.desktop.busy {
                    self.desktop.prefix = value;
                    self.desktop.report = None;
                    self.desktop.installed = None;
                }
            }
            Message::DesktopLocal(value) => {
                if !self.desktop.busy {
                    self.desktop.local_preview = value;
                    self.desktop.report = None;
                }
            }
            Message::DesktopUseCurrent => {
                if let Ok(exe) = agentdocker_host::procinfo::executable_path()
                    && let Some(parent) = exe.parent().and_then(|p| p.parent())
                {
                    self.desktop.source = if cfg!(target_os = "macos") {
                        parent.parent().unwrap_or(parent)
                    } else {
                        parent
                    }
                    .display()
                    .to_string();
                    self.desktop.report = None;
                }
            }
            Message::DesktopPreview(operation) => {
                if !self.desktop.busy
                    && let Some(args) = self.desktop.preview(&operation)
                {
                    self.desktop.busy = true;
                    self.send(Cmd::Desktop(args));
                }
            }
            Message::DesktopApply => {
                if !self.desktop.busy
                    && let Some(args) = self.desktop.apply()
                {
                    self.desktop.busy = true;
                    self.send(Cmd::Desktop(args));
                }
            }
            Message::Dark(dark) => {
                self.shell.catalog.dark = dark;
                self.shell.changed();
            }
            Message::TextSize(size) => {
                self.settings.text_size = size;
                self.settings = self.settings.clone().clamped();
                self.shell.catalog.appearance = Some(self.settings.clone());
                self.shell.changed();
            }
            Message::TerminalSize(size) => {
                self.settings.terminal_size = size;
                self.settings = self.settings.clone().clamped();
                self.shell.catalog.appearance = Some(self.settings.clone());
                self.shell.changed();
            }
            Message::Palette(palette) => {
                self.settings.palette = palette;
                self.settings = self.settings.clone().clamped();
                self.shell.catalog.appearance = Some(self.settings.clone());
                self.shell.changed();
            }
            Message::Roomy(roomy) => {
                self.settings.roomy = roomy;
                self.shell.catalog.appearance = Some(self.settings.clone());
                self.shell.changed();
            }
            Message::DismissError => {
                self.shell.error = None;
                self.status.clear();
            }
            Message::Event(iced::Event::Window(window::Event::Rescaled(scale))) => {
                self.shell.dpi = scale
            }
            Message::Event(iced::Event::Window(window::Event::Focused)) => {
                if let Some(id) = self.shell.window {
                    tasks.push(crate::accessibility::focus(id, true));
                }
            }
            Message::Event(iced::Event::Window(window::Event::Unfocused)) => {
                if let Some(id) = self.shell.window {
                    tasks.push(crate::accessibility::focus(id, false));
                }
            }
            Message::Event(iced::Event::Window(window::Event::CloseRequested)) => {
                self.shell.closing = true
            }
            Message::Event(iced::Event::Window(window::Event::Resized(size))) => {
                self.shell.width = size.width;
                self.shell.height = size.height;
            }
            Message::Event(iced::Event::Keyboard(keyboard::Event::KeyPressed {
                key,
                modifiers,
                ..
            })) => {
                use keyboard::{Key, key::Named};
                match key {
                    Key::Named(Named::Tab | Named::F6) => tasks.push(
                        (if modifiers.shift() {
                            iced::widget::operation::focus_previous()
                        } else {
                            iced::widget::operation::focus_next()
                        })
                        .chain(crate::controls::reveal_focus())
                        .chain(crate::accessibility::collect()),
                    ),
                    Key::Named(Named::Escape) => {
                        self.shell.pending_notification = None;
                        self.shell.selected = None;
                        self.shell.launch = false;
                        self.shell.adding = false;
                        self.shell.more = false;
                    }
                    Key::Character(key) if modifiers.command() => {
                        let screen = match key.as_str() {
                            "1" => Some(Screen::Agents),
                            "2" => Some(Screen::Questions),
                            "3" => Some(Screen::Runtimes),
                            "4" => Some(Screen::Settings),
                            _ => None,
                        };
                        if let Some(screen) = screen {
                            tasks.push(self.update(Message::Navigate(screen)));
                        }
                    }
                    _ => {}
                }
            }
            Message::Event(_) => {}
            Message::Captured(capture) => {
                if let Some(smoke) = &mut self.smoke {
                    tasks.push(smoke.captured(capture));
                }
            }
            Message::Focus(id) => tasks.push(
                iced::widget::operation::focus(iced::advanced::widget::Id::from(id))
                    .chain(crate::controls::reveal_focus())
                    .chain(crate::accessibility::collect()),
            ),
            Message::Accessibility(snapshot) => {
                if let Some(scenario) = self.smoke.as_mut().and_then(|s| s.scenario.as_mut()) {
                    scenario.snapshot = snapshot.clone();
                }
                if let Some(id) = self.shell.window {
                    tasks.push(crate::accessibility::update(
                        id,
                        snapshot,
                        f64::from(self.scale_factor() * self.shell.dpi.max(1.0)),
                    ));
                }
                return Task::batch(tasks);
            }
        }
        if self.shell.catalog.selected != self.shell.checked_project {
            self.shell.checked_project = self.shell.catalog.selected.clone();
            self.shell.project_available = None;
            if let Some(path) = self.shell.checked_project.clone() {
                tasks.push(Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || {
                            let available = path.is_dir();
                            (path, available)
                        })
                        .await
                        .unwrap_or_default()
                    },
                    |(path, available)| Message::ProjectLocation(path, available),
                ));
            }
        }
        if self.shell.closing
            && (!self.shell.save_enabled
                || (!self.shell.saving && self.shell.generation == self.shell.saved_generation))
        {
            return iced::exit();
        }
        if self.shell.save_enabled
            && !self.shell.saving
            && self.shell.generation != self.shell.saved_generation
        {
            self.shell.saving = true;
            let (home, catalog, generation) = (
                self.home.clone(),
                self.shell.catalog.clone(),
                self.shell.generation,
            );
            tasks.push(Task::perform(
                async move {
                    tokio::task::spawn_blocking(move || {
                        catalog.save(&home).map_err(|e| e.to_string())
                    })
                    .await
                    .unwrap_or_else(|e| Err(e.to_string()))
                },
                move |result| Message::CatalogSaved(generation, result),
            ));
        }
        tasks.push(crate::accessibility::collect());
        Task::batch(tasks)
    }

    fn refresh_project_context(&mut self) {
        let selected = self
            .shell
            .catalog
            .selected()
            .map(|e| e.project.id().to_string());
        if self.journal_project != selected {
            self.journal_project = selected.clone();
            self.journal.clear();
            if let Some(project) = selected {
                self.request_journal(project);
                if let Some(entry) = self.shell.catalog.selected() {
                    self.request_channels(entry.project.id().to_string());
                }
            }
        }
    }

    /// Wait for initial/reconnected snapshots before resolving a click. Route
    /// by the actual message and sender IDs, never by the notification's text.
    fn advance_notification(&mut self) -> Task<Message> {
        let Some((action, started)) = self.shell.pending_notification.clone() else {
            return Task::none();
        };
        let target = &action.target;
        let question = self
            .questions
            .iter()
            .find(|q| q.id == target.message && q.from == target.agent.as_str());
        let envelope = self
            .inbox
            .iter()
            .find(|m| m.id == target.message && m.from == target.agent.as_str());
        let channel = envelope.and_then(|m| match &m.to {
            agentdocker_core::Destination::Channel(id) => Some(id.clone()),
            _ => None,
        });
        let found = question.is_some() || envelope.is_some();
        if !found {
            if started.elapsed() >= Duration::from_secs(10) {
                self.shell.pending_notification = None;
                self.shell.notification_message = None;
                self.screen = Screen::Questions;
                self.say(if self.connected.is_ok() {
                    "This notification's message is no longer available."
                } else {
                    "Cannot open the notification while disconnected. Try again after reconnecting."
                });
            }
            return Task::none();
        }
        let is_question = question.is_some();
        // Prefer the actual channel's project. The source agent may have moved
        // since posting; a retained project ID is a fallback for direct messages.
        let project = channel
            .as_ref()
            .and_then(|id| self.channels.iter().find(|c| &c.id == id))
            .map(|c| c.project.clone())
            .or_else(|| target.project.clone());
        if let Some(project) = &project {
            let root = self
                .shell
                .catalog
                .projects
                .iter()
                .find(|entry| &entry.project.id() == project)
                .map(|entry| entry.project.root.clone());
            if let Some(root) = root {
                self.shell.catalog.selected = Some(root);
                self.shell.catalog.unassigned = false;
                self.shell.changed();
                self.refresh_project_context();
            } else if started.elapsed() < Duration::from_secs(10) {
                return Task::none();
            } else {
                self.shell.pending_notification = None;
                self.shell.notification_message = None;
                self.screen = Screen::Questions;
                self.say("This notification's project is no longer available.");
                return Task::none();
            }
        }
        if let Some(channel) = &channel
            && !self.channels.iter().any(|c| &c.id == channel)
        {
            if started.elapsed() < Duration::from_secs(10) {
                return Task::none();
            }
            self.shell.pending_notification = None;
            self.shell.notification_message = None;
            self.screen = Screen::Questions;
            self.say("This notification's channel is no longer available.");
            return Task::none();
        }
        self.shell.pending_notification = None;
        self.shell.notification_message = Some(target.message.clone());
        self.shell.selected = Some(target.agent.to_string());
        self.shell.more = false;
        self.confirm_stop = None;
        self.screen = if channel.is_some() {
            Screen::Channels
        } else {
            Screen::Questions
        };
        if let Some(channel) = channel {
            self.shell.channel_target = Some(channel.to_string());
        }
        // Revealing the card changes only scrolling. Existing answer/channel
        // drafts and their keyboard focus are not submitted or rewritten.
        crate::controls::reveal(format!(
            "notification-{}-{}",
            if is_question { "question" } else { "message" },
            target.message
        ))
    }

    fn launch_spec(&self) -> Result<agentdocker_core::AgentSpec, String> {
        let entry = self
            .shell
            .catalog
            .selected()
            .ok_or("Choose a project first")?;
        let runtime = self
            .runtimes
            .iter()
            .find(|r| Some(&r.name) == self.shell.launch_runtime.as_ref())
            .ok_or("Choose an installed command-line agent")?;
        let cli = runtime
            .cli
            .as_ref()
            .ok_or("This tool has no launchable command-line executable")?;
        let args = shell_words(&self.shell.launch_arguments)
            .ok_or("The launch arguments have unbalanced quotes")?;
        let mut command = vec![cli.to_string_lossy().into_owned()];
        command.extend(args);
        let name = if self.shell.launch_name.trim().is_empty() {
            format!("{}-{}", runtime.name, Utc::now().timestamp())
        } else {
            self.shell.launch_name.trim().into()
        };
        Ok(agentdocker_core::AgentSpec {
            name,
            runtime: runtime.name.clone(),
            provider: None,
            model: None,
            command,
            workdir: Some(entry.project.root.clone()),
            env: BTreeMap::new(),
            labels: BTreeMap::new(),
            isolate: false,
            tty: true,
            restore: false,
            in_pane: false,
            restart: Default::default(),
            depends_on: Vec::new(),
        })
    }
}

fn resolve_folder(path: PathBuf) -> Task<Message> {
    Task::perform(
        async move {
            tokio::task::spawn_blocking(move || {
                crate::catalog::resolve(&path).map_err(|e| e.to_string())
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string()))
        },
        Message::FolderResolved,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn app() -> (App, CommandReceiver, SyncSender<Msg>) {
        let (tx, commands) = queue::channel();
        let (messages, rx) = sync_channel(MESSAGE_CAPACITY);
        (App::bare(tx, rx), commands, messages)
    }

    fn notification_app() -> (
        App,
        CommandReceiver,
        SyncSender<Msg>,
        tempfile::TempDir,
        agentdocker_host::notify::Action,
    ) {
        let (mut app, commands, messages) = app();
        let home = tempfile::tempdir().unwrap();
        let socket = home.path().join("agentd.sock");
        app.home = home.path().to_owned();
        app.client = Some(Arc::new(Client::isolated(socket.clone())));
        let action = agentdocker_host::notify::Action {
            home: app.home.clone(),
            socket,
            target: agentdocker_core::NotificationTarget {
                message: MessageId::from("question-1".to_owned()),
                agent: agentdocker_core::AgentId::from("sender-1"),
                project: None,
                channel: None,
            },
        };
        (app, commands, messages, home, action)
    }

    #[test]
    fn notification_waits_for_data_then_opens_the_question_without_submitting_drafts() {
        let (mut app, commands, messages, home, mut action) = notification_app();
        let project = crate::catalog::resolve(home.path()).unwrap();
        app.shell.catalog.remember(project.clone(), false);
        action.target.project = Some(project.id());
        let question = Question {
            id: action.target.message.clone(),
            from: action.target.agent.to_string(),
            to: agentdocker_core::Destination::Agent(agentdocker_core::AgentId::from("user")),
            text: "which option?".into(),
            asked_at: Utc::now(),
            expires_at: Utc::now() + chrono::Duration::minutes(5),
        };
        app.answers
            .insert(question.id.clone(), "unfinished answer".into());
        app.shell.channel_drafts.insert(
            "another-room".into(),
            ChannelDraft {
                text: "unfinished channel message".into(),
                ..Default::default()
            },
        );
        let _ = app.update(Message::Notification(
            crate::notification_route::Activation::Open(action),
        ));
        assert!(app.shell.pending_notification.is_some());
        assert_eq!(app.screen, Screen::Agents);
        messages
            .send(Msg::Questions(vec![question.clone()]))
            .unwrap();
        let _ = app.update(Message::Tick);
        assert_eq!(app.screen, Screen::Questions);
        assert_eq!(app.shell.catalog.selected.as_ref(), Some(&project.root));
        assert_eq!(app.shell.notification_message.as_ref(), Some(&question.id));
        assert!(app.shell.pending_notification.is_none());
        assert_eq!(app.answers[&question.id], "unfinished answer");
        assert_eq!(
            app.shell.channel_drafts["another-room"].text,
            "unfinished channel message"
        );
        assert!(app.sending.is_empty());
        assert!(commands.try_iter().all(|cmd| !matches!(
            cmd,
            Cmd::Answer(..) | Cmd::ChannelSend(..) | Cmd::Launch(..) | Cmd::Stop(..)
        )));
    }

    #[test]
    fn notification_cannot_hijack_manual_navigation_or_a_different_daemon() {
        let (mut app, _, _, _home, action) = notification_app();
        let _ = app.update(Message::Notification(
            crate::notification_route::Activation::Open(action.clone()),
        ));
        assert!(app.shell.pending_notification.is_some());
        let _ = app.update(Message::Navigate(Screen::Settings));
        assert!(app.shell.pending_notification.is_none());
        let mut foreign = action.clone();
        foreign.socket = foreign.socket.with_file_name("another-daemon.sock");
        let _ = app.update(Message::Notification(
            crate::notification_route::Activation::Open(foreign),
        ));
        assert_eq!(app.screen, Screen::Settings);
        assert!(app.status.contains("another local workspace"));
        app.shell.pending_notification = Some((action, Instant::now() - Duration::from_secs(11)));
        let _ = app.advance_notification();
        assert_eq!(app.screen, Screen::Questions);
        assert!(app.status.contains("no longer available"));
        assert!(app.shell.notification_message.is_none());
    }
    #[test]
    fn adding_an_existing_folder_only_pins_and_reads_context() {
        let folder = tempfile::tempdir().unwrap();
        let (mut app, commands, _) = app();
        let project = crate::catalog::resolve(folder.path()).unwrap();
        let _ = app.update(Message::FolderResolved(Ok(project.clone())));
        assert!(app.shell.catalog.selected().unwrap().pinned);
        assert_eq!(app.shell.catalog.selected.as_ref(), Some(&project.root));
        assert!(
            commands
                .try_iter()
                .all(|cmd| matches!(cmd, Cmd::Journal(_, _) | Cmd::Channels(_, _)))
        );
        assert_eq!(std::fs::read_dir(folder.path()).unwrap().count(), 0);
    }
    #[test]
    fn late_channel_acknowledgements_preserve_new_text_and_other_rooms() {
        let mut first = ChannelDraft {
            text: "sent text".into(),
            ..Default::default()
        };
        assert_eq!(first.begin().as_deref(), Some("sent text"));
        assert!(first.begin().is_none());
        first.text = "next thought".into();
        first.complete(Ok(()));
        assert_eq!(first.text, "next thought");
        assert_eq!(first.begin().as_deref(), Some("next thought"));
        first.complete(Err("connection lost".into()));
        assert_eq!(first.text, "next thought");
        assert_eq!(first.error.as_deref(), Some("connection lost"));
        let (mut app, _, messages) = app();
        app.shell.channel_drafts.insert("one".into(), first);
        app.shell.channel_drafts.insert(
            "two".into(),
            ChannelDraft {
                text: "other room".into(),
                ..Default::default()
            },
        );
        app.shell.channel_drafts.get_mut("one").unwrap().begin();
        messages
            .send(Msg::ChannelSent(
                "one".into(),
                Ok(MessageId::from("confirmed".to_owned())),
            ))
            .unwrap();
        app.drain();
        assert!(app.shell.channel_drafts["one"].text.is_empty());
        assert_eq!(app.shell.channel_drafts["two"].text, "other room");
        assert_eq!(app.sent_channels.back().unwrap().payload, "next thought");
    }
    #[test]
    fn commands_capture_project_context_and_a_late_reply_does_not_navigate() {
        let folder = tempfile::tempdir().unwrap();
        let (mut app, commands, messages) = app();
        app.shell
            .catalog
            .pin(crate::catalog::resolve(folder.path()).unwrap())
            .unwrap();
        let selected = app.shell.catalog.selected.clone();
        let _ = app.update(Message::ConsoleInput("status".into()));
        let _ = app.update(Message::RunConsole);
        let queued = commands
            .try_iter()
            .find(|cmd| matches!(cmd, Cmd::Console(_, _)))
            .unwrap();
        assert!(matches!(queued, Cmd::Console(line, cwd) if line == "status" && cwd == selected));
        let _ = app.update(Message::Navigate(Screen::Settings));
        messages.send(Msg::Console("finished".into())).unwrap();
        app.drain();
        assert_eq!(app.screen, Screen::Settings);
        assert_eq!(app.console_running, 0);
    }
    #[test]
    fn quiet_project_channels_remain_queryable_and_stale_folder_checks_are_ignored() {
        let folder = tempfile::tempdir().unwrap();
        let (mut app, _, _) = app();
        let project = crate::catalog::resolve(folder.path()).unwrap();
        let id = project.id().to_string();
        app.shell.catalog.pin(project).unwrap();
        assert!(app.agents.is_empty());
        assert!(app.channel_projects().contains(&id));
        let _ = app.update(Message::ProjectLocation(
            PathBuf::from("/some/other/project"),
            false,
        ));
        assert_ne!(app.shell.project_available, Some(false));
    }
    #[test]
    fn failed_preferences_load_never_overwrites_the_existing_file() {
        use std::io::Write;
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("workspace.json");
        agentdocker_host::dirs::private_file(&path, true, false)
            .unwrap()
            .write_all(b"not JSON")
            .unwrap();
        let state = State::load(home.path());
        assert!(!state.save_enabled);
        assert!(state.error.unwrap().contains("preserved"));
        assert_eq!(std::fs::read(path).unwrap(), b"not JSON");
    }
}
