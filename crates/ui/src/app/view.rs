//! Projects, attention and contextual tools over live daemon state.
use super::style::Colors;
use super::*;
use crate::controls::{block_button, button as action, input, input_enabled};
use iced::{
    Element, Fill, Font,
    widget::{Space, column, container, row, scrollable, text},
};

fn heading<'a>(value: impl Into<String>, size: u32) -> iced::widget::Text<'a> {
    text(value.into()).size(size).font(Font {
        weight: iced::font::Weight::Semibold,
        ..Font::DEFAULT
    })
}
fn note<'a>(value: impl Into<String>, c: Colors) -> iced::widget::Text<'a> {
    text(value.into()).size(13).color(c.muted)
}
fn card<'a>(content: impl Into<Element<'a, Message>>, c: Colors) -> Element<'a, Message> {
    container(content)
        .padding(18)
        .width(Fill)
        .style(move |_| c.surface(c.subtle, true))
        .into()
}
fn value(json: &serde_json::Value, key: &str) -> String {
    json[key].as_str().unwrap_or("unknown").to_owned()
}

impl App {
    pub fn theme(&self) -> iced::Theme {
        Colors::new(self.shell.catalog.dark).theme()
    }
    pub fn scale_factor(&self) -> f32 {
        self.settings.text_size / 14.0
    }
    fn selected_root(&self) -> Option<&std::path::Path> {
        self.shell.catalog.selected.as_deref()
    }
    fn has_project(&self, project: Option<&ProjectRef>) -> bool {
        match self.selected_root() {
            Some(root) => project.is_some_and(|p| p.root == root),
            None => project.is_none(),
        }
    }
    pub fn view(&self) -> Element<'_, Message> {
        let c = Colors::new(self.shell.catalog.dark);
        let in_project = !matches!(
            self.screen,
            Screen::Questions | Screen::Runtimes | Screen::Settings | Screen::Desktop
        );
        let title = if in_project {
            self.shell
                .catalog
                .selected()
                .map(|e| e.project.name())
                .unwrap_or_else(|| {
                    if self.shell.catalog.unassigned {
                        "Unassigned sessions"
                    } else {
                        "Projects"
                    }
                    .into()
                })
        } else {
            match self.screen {
                Screen::Questions => "Inbox",
                Screen::Runtimes => "Connections",
                _ => "Settings",
            }
            .into()
        };
        let subtitle = if in_project {
            self.shell
                .catalog
                .selected()
                .map(|e| e.project.root.display().to_string())
                .unwrap_or_else(|| "Choose a folder or discover agents already working.".into())
        } else {
            match self.screen {
                Screen::Questions => "Questions and messages addressed to you.",
                Screen::Runtimes => "Installed tools, supported connections, and reviewed setup.",
                _ => "Appearance, installation, and diagnostics.",
            }
            .into()
        };
        let header = column![heading(title, 28), note(subtitle, c)]
            .spacing(6)
            .width(Fill);
        let mut content = column![header].spacing(20).width(Fill);
        if let Err(error) = &self.connected {
            content=content.push(card(column![heading("Connection unavailable",15).color(c.amber),note(format!("{error}\nThe last snapshot remains visible. Drafts are retained and daemon actions are paused."),c)].spacing(7),c));
        }
        if let Some(error) = &self.shell.error {
            content = content.push(card(
                column![
                    text(error.clone()).size(14).color(c.amber),
                    action(
                        "dismiss-error",
                        "Dismiss",
                        Some(Message::DismissError),
                        false
                    )
                ]
                .spacing(8),
                c,
            ));
        }
        if !self.status.is_empty() {
            content = content.push(text(self.status.clone()).size(13).color(c.accent));
        }
        if in_project {
            let mut tabs = row![].spacing(4);
            for (screen, label) in [
                (Screen::Agents, "Sessions"),
                (Screen::Journal, "Activity"),
                (Screen::Channels, "Channels"),
                (Screen::Leases, "Coordination"),
                (Screen::Console, "Commands"),
            ] {
                tabs = tabs.push(action(
                    format!("project-tab-{screen:?}"),
                    label,
                    Some(Message::Navigate(screen)),
                    self.screen == screen,
                ));
            }
            if self.shell.width / self.scale_factor() < 900.0 {
                content = content.push(scrollable(tabs).id("project-tabs").direction(
                    iced::widget::scrollable::Direction::Horizontal(
                        iced::widget::scrollable::Scrollbar::default(),
                    ),
                ));
            } else {
                content = content.push(tabs);
            }
        }
        if self.shell.adding {
            content=content.push(card(column![heading("Add an existing project",18),note("Choose a folder. It stays in your workspace even when no agents are running.",c),input("project-path","Project folder",&self.shell.add_path,Message::AddPath),row![action("browse-folder","Browse…",Some(Message::PickFolder),false),action("pin-folder","Add project",(!self.shell.add_path.trim().is_empty()).then_some(Message::ResolveFolder),true),action("cancel-add","Cancel",Some(Message::ShowAdd),false)].spacing(6)].spacing(12),c));
        }
        let body = match self.screen {
            Screen::Agents => self.sessions(c),
            Screen::Questions => self.questions(c),
            Screen::Runtimes => self.connections(c),
            Screen::Terminal => self.terminal_view(c),
            Screen::Journal => self.journal_view(c),
            Screen::Channels => self.channels_view(c),
            Screen::Leases => self.coordination(c),
            Screen::Console => self.console_view(c),
            Screen::Settings => self.settings_view(c),
            Screen::Desktop => self.installation_view(c),
        };
        content = content.push(body);
        let narrow = self.shell.width / self.scale_factor() < 900.0;
        row![
            self.sidebar(c),
            container(
                scrollable(content)
                    .spacing(12)
                    .width(Fill)
                    .height(Fill)
                    .id("workspace-scroll")
            )
            .padding(if narrow { 18 } else { 28 })
            .width(Fill)
        ]
        .height(Fill)
        .into()
    }

    fn sidebar(&self, c: Colors) -> Element<'_, Message> {
        let project_page = !matches!(
            self.screen,
            Screen::Questions | Screen::Runtimes | Screen::Settings | Screen::Desktop
        );
        let mut nav = column![heading("agentdocker", 22), Space::new().height(16)].spacing(6);
        for (key, label, screen, selected) in [
            (
                "projects",
                "Projects".to_owned(),
                Screen::Agents,
                project_page,
            ),
            (
                "inbox",
                format!(
                    "Inbox{}",
                    if self.questions.is_empty() {
                        String::new()
                    } else {
                        format!("   {}", self.questions.len())
                    }
                ),
                Screen::Questions,
                self.screen == Screen::Questions,
            ),
            (
                "connections",
                "Connections".to_owned(),
                Screen::Runtimes,
                self.screen == Screen::Runtimes,
            ),
        ] {
            nav = nav.push(block_button(
                key,
                label,
                Some(Message::Navigate(screen)),
                selected,
            ));
        }
        nav = nav
            .push(Space::new().height(18))
            .push(note("Your projects", c));
        let mut projects = column![].spacing(5).width(Fill);
        for entry in &self.shell.catalog.projects {
            let path = entry.project.root.clone();
            let label = format!(
                "{}{}",
                if entry.pinned { "• " } else { "" },
                entry.project.name()
            );
            projects = projects.push(block_button(
                format!("project-{}", path.display()),
                label,
                Some(Message::SelectProject(path.clone())),
                project_page && self.selected_root() == Some(path.as_path()),
            ));
        }
        if self
            .agents
            .iter()
            .any(|a| a.project.is_none() && a.spec.runtime != agentdocker_core::HUMAN_RUNTIME)
            || self.discovered.iter().any(|p| p.project.is_none())
        {
            projects = projects.push(block_button(
                "unassigned",
                "Unassigned sessions",
                Some(Message::Unassigned),
                self.shell.catalog.unassigned && project_page,
            ));
        }
        nav = nav
            .push(
                scrollable(projects)
                    .id("sidebar-projects")
                    .spacing(6)
                    .height(Fill),
            )
            .push(block_button(
                "add-project",
                "+ Add project…",
                Some(Message::ShowAdd),
                self.shell.adding,
            ))
            .push(block_button(
                "settings",
                "Settings",
                Some(Message::Navigate(Screen::Settings)),
                matches!(self.screen, Screen::Settings | Screen::Desktop),
            ))
            .push(note(
                if self.connected.is_ok() {
                    "Connected locally"
                } else {
                    "Reconnecting…"
                },
                c,
            ));
        container(nav.height(Fill))
            .padding([24, 14])
            .width(if self.shell.width / self.scale_factor() < 900.0 {
                190
            } else {
                214
            })
            .height(Fill)
            .style(move |_| c.surface(c.sidebar, false))
            .into()
    }

    fn sessions(&self, c: Colors) -> Element<'_, Message> {
        let mut panel = column![]
            .spacing(if self.settings.roomy { 22 } else { 14 })
            .width(Fill);
        if let Some(entry) = self.shell.catalog.selected() {
            panel = panel.push(
                row![
                    action(
                        "launch-agent",
                        "Launch agent…",
                        (self.connected.is_ok() && self.shell.project_available != Some(false))
                            .then_some(Message::ShowLaunch),
                        self.shell.launch
                    ),
                    action(
                        "pin-selected",
                        if entry.pinned {
                            "Unpin project"
                        } else {
                            "Pin project"
                        },
                        Some(Message::Unpin),
                        entry.pinned
                    ),
                    action(
                        "forget-project",
                        "Forget project",
                        Some(Message::ForgetProject),
                        false
                    )
                ]
                .spacing(6),
            );
        }
        if self.shell.project_available == Some(false) {
            panel=panel.push(card(column![heading("Project folder unavailable",18),note("The saved location is missing or cannot be read. The project stays selected; restore the folder or choose another project.",c),action("retry-project-folder","Check folder again",Some(Message::RetryProject),false)].spacing(10),c));
        }
        if self.shell.launch {
            panel = panel.push(self.launch_view(c));
        }
        panel = panel.push(input(
            "session-search",
            "Find a session…",
            &self.shell.search,
            Message::Search,
        ));
        let needle = self.shell.search.to_lowercase();
        let mut count = 0;
        for agent in self.agents.iter().filter(|a| {
            a.spec.runtime != agentdocker_core::HUMAN_RUNTIME
                && self.has_project(a.project.as_ref())
        }) {
            if !format!(
                "{} {} {}",
                agent.spec.name,
                agent.spec.runtime,
                agent.vcs.as_ref().map(|v| v.describe()).unwrap_or_default()
            )
            .to_lowercase()
            .contains(&needle)
            {
                continue;
            }
            count += 1;
            let id = agent.id.to_string();
            let activity = if agent.status.is_live() {
                self.activity
                    .get(&id)
                    .map(Activity::label)
                    .unwrap_or("unknown")
            } else {
                "finished"
            };
            let branch = agent.vcs.as_ref().map(|v| v.describe()).unwrap_or_default();
            let question = self.questions.iter().any(|q| q.from == id);
            let label = format!(
                "{}\n{}{}   ·   {}",
                agent.spec.name,
                agent.spec.runtime,
                if branch.is_empty() {
                    String::new()
                } else {
                    format!(" · {branch}")
                },
                if question { "needs input" } else { activity }
            );
            panel = panel.push(card(
                block_button(
                    format!("session-{id}"),
                    label,
                    Some(Message::SelectSession(id.clone())),
                    self.shell.selected.as_deref() == Some(id.as_str()),
                ),
                c,
            ));
        }
        for process in self
            .discovered
            .iter()
            .filter(|p| self.has_project(p.project.as_ref()))
        {
            if !format!("{} {}", process.runtime, process.command)
                .to_lowercase()
                .contains(&needle)
            {
                continue;
            }
            count += 1;
            panel=panel.push(card(column![heading(process.default_name(),16),note(format!("{} · pid {} · activity unknown",process.runtime,process.pid),c),note("Discovered process. Registering makes it visible to coordination; integration capabilities remain separate.",c),action(format!("adopt-{}",process.pid),"Register session",self.connected.is_ok().then_some(Message::Adopt(process.pid)),false)].spacing(8),c));
        }
        if count == 0 {
            panel=panel.push(card(column![heading(if needle.is_empty(){"No sessions here yet"}else{"No matching sessions"},20),note(if needle.is_empty(){"Start a supported agent in this folder, or launch one here. Your project stays available while it is quiet."}else{"Try another name, tool, or branch."},c)].spacing(10),c));
        }
        if let Some(agent) = self
            .shell
            .selected
            .as_ref()
            .and_then(|id| self.agents.iter().find(|a| a.id.as_str() == id))
        {
            let inspector = self.inspector(agent, c);
            if self.shell.width / self.scale_factor() >= 1120.0 {
                return row![panel, container(inspector).width(320)]
                    .spacing(20)
                    .into();
            }
            panel = panel.push(inspector);
        }
        panel.into()
    }

    fn inspector(&self, agent: &AgentRecord, c: Colors) -> Element<'_, Message> {
        let id = agent.id.to_string();
        let stop_armed = self
            .confirm_stop
            .as_ref()
            .is_some_and(|(armed, at)| armed == &id && at.elapsed() < CONFIRM_WITHIN);
        let mut body = column![
            heading(agent.spec.name.clone(), 20),
            note(
                format!(
                    "{} · {}",
                    agent.spec.runtime,
                    self.activity
                        .get(&id)
                        .map(Activity::label)
                        .unwrap_or("activity unknown")
                ),
                c
            ),
            note(
                agent
                    .project
                    .as_ref()
                    .map(|p| p.dir().display().to_string())
                    .unwrap_or_else(|| "No working directory reported".into()),
                c
            ),
            note(
                agent.vcs.as_ref().map(|v| v.describe()).unwrap_or_default(),
                c
            ),
            note(format!("Last seen {}", ago(Utc::now(), agent.last_seen)), c)
        ]
        .spacing(12);
        if let Some(activity) = self.activity.get(&id) {
            body = body.push(note(format!("Observation: {activity:?}"), c));
        }
        if agent.managed && agent.spec.tty && agent.status.is_live() {
            body = body.push(action(
                "attach-session",
                "Open terminal",
                self.connected
                    .is_ok()
                    .then_some(Message::Attach(id.clone())),
                true,
            ));
        } else {
            body = body.push(note(
                "Terminal access requires a live PTY session managed by AgentDocker.",
                c,
            ));
        }
        if let Some(session) = &agent.session {
            body = body.push(note(format!("External multiplexer: {session:?}"), c));
        }
        if agent.status.is_live() {
            body = body.push(action(
                "stop-session",
                if stop_armed {
                    "Confirm stop"
                } else {
                    "Stop session…"
                },
                self.connected.is_ok().then_some(Message::Stop(id)),
                false,
            ));
        }
        if stop_armed {
            body = body.push(note(
                "Confirm within five seconds to send the stop signal.",
                c,
            ));
        }
        body = body.push(action(
            "close-session",
            "Close details",
            Some(Message::CloseSession),
            false,
        ));
        card(body, c)
    }

    fn launch_view(&self, c: Colors) -> Element<'_, Message> {
        let mut tools=column![heading("Launch in this project",20),note("Choose an installed command-line agent. This starts a new supervised terminal session; it does not restart on app launch.",c)].spacing(10);
        for runtime in self.runtimes.iter().filter(|r| r.cli.is_some()) {
            tools = tools.push(action(
                format!("launch-tool-{}", runtime.name),
                runtime.label.clone(),
                (!self.shell.launching).then_some(Message::LaunchRuntime(runtime.name.clone())),
                self.shell.launch_runtime.as_deref() == Some(runtime.name.as_str()),
            ));
        }
        tools = tools
            .push(input(
                "launch-name",
                "Session name (optional)",
                &self.shell.launch_name,
                Message::LaunchName,
            ))
            .push(input(
                "launch-arguments",
                "Command arguments (optional)",
                &self.shell.launch_arguments,
                Message::LaunchArguments,
            ));
        if let Some(runtime) = self
            .runtimes
            .iter()
            .find(|r| Some(&r.name) == self.shell.launch_runtime.as_ref())
        {
            tools = tools.push(note(
                format!(
                    "Command: {} {}",
                    runtime
                        .cli
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default(),
                    self.shell.launch_arguments
                ),
                c,
            ));
        }
        tools = tools.push(
            row![
                action(
                    "confirm-launch",
                    if self.shell.launching {
                        "Launching…"
                    } else {
                        "Launch agent"
                    },
                    (!self.shell.launching
                        && self.shell.launch_runtime.is_some()
                        && self.connected.is_ok())
                    .then_some(Message::Launch),
                    true
                ),
                action(
                    "cancel-launch",
                    "Close",
                    (!self.shell.launching).then_some(Message::ShowLaunch),
                    false
                )
            ]
            .spacing(6),
        );
        card(tools, c)
    }

    fn questions(&self, c: Colors) -> Element<'_, Message> {
        let mut list = column![].spacing(18).width(Fill);
        if self.questions.is_empty() {
            list = list.push(card(
                column![
                    heading("You're all caught up", 22),
                    note("Questions from your agents will appear here.", c)
                ]
                .spacing(10),
                c,
            ));
        }
        for question in &self.questions {
            let id = question.id.clone();
            let draft_id = id.clone();
            let busy = self.sending.contains(&id);
            let answer = self.answers.get(&id).cloned().unwrap_or_default();
            let mut body = column![
                note(
                    format!(
                        "{} · asked {}",
                        self.name_of(&question.from),
                        ago(Utc::now(), question.asked_at)
                    ),
                    c
                ),
                heading(question.text.clone(), 20),
                input_enabled(
                    format!("answer-{id}"),
                    "Your answer",
                    &answer,
                    move |text| Message::Draft(draft_id.clone(), text),
                    !busy
                ),
                action(
                    format!("send-answer-{id}"),
                    if busy { "Sending…" } else { "Send answer" },
                    (!busy
                        && self.connected.is_ok()
                        && !answer.trim().is_empty()
                        && !question.expired(Utc::now()))
                    .then_some(Message::Answer(id.clone())),
                    true
                )
            ]
            .spacing(12);
            if question.expired(Utc::now()) {
                body=body.push(note("This question has expired; its draft is retained until the next authoritative refresh.",c));
            }
            if let Some(error) = self.shell.answer_errors.get(&id) {
                body = body.push(text(error.clone()).size(13).color(c.amber));
            }
            list = list.push(card(body, c));
        }
        let (_, direct) = by_room(&self.inbox);
        if !direct.is_empty() {
            list = list.push(heading("Messages addressed to you", 18));
        }
        for message in direct.iter().rev().take(30).rev() {
            list = list.push(self.message_view(message, c));
        }
        list.into()
    }

    fn message_view(
        &self,
        message: &agentdocker_core::Envelope,
        c: Colors,
    ) -> Element<'_, Message> {
        let payload = message
            .payload
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| serde_json::to_string_pretty(&message.payload).unwrap_or_default());
        card(
            column![
                note(
                    format!(
                        "{} · {} · {}",
                        self.name_of(&message.from),
                        message.kind,
                        ago(Utc::now(), message.sent_at)
                    ),
                    c
                ),
                text(payload).size(14)
            ]
            .spacing(8),
            c,
        )
    }

    fn channels_view(&self, c: Colors) -> Element<'_, Message> {
        let selected = self.shell.catalog.selected().map(|e| e.project.id());
        let (messages, _) = by_room(&self.inbox);
        let mut list=column![note("Messages shown are queued for you and remain unread by this view. They are not a complete conversation history.",c)].spacing(14);
        let mut count = 0;
        for channel in self
            .channels
            .iter()
            .filter(|ch| Some(&ch.project) == selected.as_ref())
        {
            count += 1;
            let id = channel.id.to_string();
            let mut body = column![
                heading(channel.title(), 18),
                note(
                    format!(
                        "{} members · {}",
                        channel.members.len(),
                        if channel.is_open() { "open" } else { "closed" }
                    ),
                    c
                ),
                note(
                    channel
                        .members
                        .iter()
                        .map(|id| self.name_of(id.as_str()))
                        .collect::<Vec<_>>()
                        .join(", "),
                    c
                )
            ]
            .spacing(10);
            for review in &channel.reviews {
                body = body.push(
                    text(format!(
                        "{:?} by {} on {}: {}",
                        review.verdict, review.by_name, review.of_name, review.note
                    ))
                    .size(14),
                );
            }
            if let Some(resolution) = &channel.resolution {
                body = body.push(note(resolution.clone(), c));
            }
            if let Some(queued) = messages.get(&id) {
                for message in queued.iter().rev().take(20).rev() {
                    body = body.push(self.message_view(message, c));
                }
            } else {
                body = body.push(note("No messages queued for you in this channel.", c));
            }
            body = body.push(action(
                format!("reply-channel-{id}"),
                "Write to channel",
                self.connected
                    .is_ok()
                    .then_some(Message::ChannelTarget(id.clone())),
                self.shell.channel_target.as_deref() == Some(id.as_str()),
            ));
            if self.shell.channel_target.as_deref() == Some(id.as_str()) {
                let draft = self
                    .shell
                    .channel_drafts
                    .get(&id)
                    .cloned()
                    .unwrap_or_default();
                if let Some(error) = &draft.error {
                    body = body.push(text(error.clone()).color(c.amber));
                }
                body = body
                    .push(input(
                        "channel-message",
                        "Message",
                        &draft.text,
                        Message::ChannelDraft,
                    ))
                    .push(action(
                        "send-channel",
                        if draft.sending.is_some() {
                            "Sending…"
                        } else {
                            "Send message"
                        },
                        (draft.sending.is_none()
                            && self.connected.is_ok()
                            && !draft.text.trim().is_empty())
                        .then_some(Message::SendChannel),
                        true,
                    ));
            }
            list = list.push(card(body, c));
        }
        if count == 0 {
            list=list.push(note("No channels in this project. Agents can open one for a task or when work needs coordination.",c));
        }
        list.into()
    }

    fn journal_view(&self, c: Colors) -> Element<'_, Message> {
        let mut list = column![note(
            format!(
                "Latest {JOURNAL_WINDOW} entries. Earlier entries remain in the project journal."
            ),
            c
        )]
        .spacing(12);
        if self.journal.is_empty() {
            list = list.push(heading("No activity recorded yet", 20));
        }
        for entry in self.journal.iter().rev() {
            list = list.push(card(
                column![
                    note(format!("{} · #{}", ago(Utc::now(), entry.at), entry.seq), c),
                    text(entry.line()).size(14)
                ]
                .spacing(7),
                c,
            ));
        }
        list.into()
    }

    fn coordination(&self, c: Colors) -> Element<'_, Message> {
        let mut list = column![
            heading("Resource coordination", 20),
            note(
                "Leases describe cooperative access reported to AgentDocker.",
                c
            )
        ]
        .spacing(14);
        let mut count = 0;
        for lease in &self.leases {
            let holder = self.agents.iter().find(|a| a.id == lease.holder);
            if !holder.is_some_and(|a| self.has_project(a.project.as_ref())) {
                continue;
            }
            count += 1;
            list = list.push(card(
                column![
                    heading(lease.resource.to_string(), 16),
                    note(
                        format!(
                            "{} · {:?} · expires {}",
                            self.name_of(lease.holder.as_str()),
                            lease.mode,
                            span((lease.expires_at - Utc::now()).num_seconds())
                        ),
                        c
                    ),
                    note(lease.note.clone().unwrap_or_default(), c)
                ]
                .spacing(8),
                c,
            ));
        }
        if count == 0 {
            list = list.push(note("No leases held in this project.", c));
        }
        list.into()
    }

    fn terminal_view(&self, c: Colors) -> Element<'_, Message> {
        let Some(terminal) = &self.terminal else {
            return note("Select a managed session and open its terminal.", c).into();
        };
        let mut pane=column![row![heading(self.name_of(&terminal.agent),20),action("detach-terminal","Detach",Some(Message::Detach),false)].spacing(12),note("F6 moves focus out of the terminal. Ctrl+] detaches. Copy copies the visible screen; paste sends clipboard text.",c)].spacing(12);
        if let Status::Ended(reason) = terminal.status() {
            pane = pane.push(text(reason).size(14).color(c.amber));
        }
        if let Some(reason) = terminal.input_notice {
            pane = pane.push(
                row![
                    text(reason).size(13).color(c.amber),
                    action(
                        "dismiss-terminal-input",
                        "Dismiss",
                        Some(Message::TerminalDismiss),
                        false
                    )
                ]
                .spacing(8),
            );
        }
        if terminal.scrolled_back() {
            pane = pane.push(action(
                "terminal-live",
                "Return to live output",
                Some(Message::TerminalScroll(-2000)),
                false,
            ));
        }
        pane.push(crate::terminal::Display {
            terminal,
            palette: self.settings.palette(),
            size: self.settings.terminal_size,
            height: (self.shell.height / self.scale_factor() - 270.0).max(240.0),
        })
        .into()
    }

    fn console_view(&self, c: Colors) -> Element<'_, Message> {
        column![heading("AgentDocker commands",20),note("Run the bundled CLI in the selected project folder. Commands have bounded output and a 20-second deadline.",c),input("console-command","agentdocker command",&self.console_input,Message::ConsoleInput),row![action("run-command",if self.console_running>0{"Run another command"}else{"Run command"},(!self.console_input.trim().is_empty()).then_some(Message::RunConsole),true),action("previous-command","Previous",Some(Message::Recall(true)),false),action("next-command","Next",Some(Message::Recall(false)),false)].spacing(8),card(text(self.console_output.clone()).font(Font::MONOSPACE).size(self.settings.terminal_size),c)].spacing(14).into()
    }

    fn connections(&self, c: Colors) -> Element<'_, Message> {
        let mut list = column![
            row![
                action(
                    "connection-health",
                    "Check connections",
                    (!self.setup_busy).then_some(Message::Setup(vec!["--health".into()])),
                    false
                ),
                action(
                    "saved-setups",
                    "Saved setup plans",
                    (!self.setup_busy).then_some(Message::Setup(vec!["--list".into()])),
                    false
                )
            ]
            .spacing(8)
        ]
        .spacing(16);
        if !self.discovered.is_empty() {
            list = list.push(action(
                "register-all-discovered",
                "Register all discovered sessions on this computer",
                self.connected.is_ok().then_some(Message::AdoptAll),
                false,
            ));
        }
        if self.setup_busy {
            list = list.push(note("Checking setup…", c));
        }
        if let Some(error) = &self.shell.setup_error {
            list = list.push(text(error.clone()).color(c.amber));
        }
        if let Some(plan) = &self.setup_plan {
            list = list.push(self.setup_view(plan, c));
        }
        for plan in &self.setup_history {
            let id = value(plan, "id");
            list = list.push(action(
                format!("saved-setup-{id}"),
                format!("{} · {id}", value(plan, "phase")),
                (!self.setup_busy).then_some(Message::Setup(vec!["--show".into(), id])),
                false,
            ));
        }
        if let Some(health) = &self.setup_health {
            let mut report = column![
                heading("Connection checks", 18),
                note(value(health, "daemon"), c)
            ]
            .spacing(8);
            for runtime in health["runtimes"].as_array().into_iter().flatten() {
                for check in runtime["checks"].as_array().into_iter().flatten() {
                    report = report.push(note(
                        format!(
                            "{} · {} · {}\n{}",
                            value(runtime, "name"),
                            value(check, "channel"),
                            value(check, "status"),
                            value(check, "detail")
                        ),
                        c,
                    ));
                }
            }
            list = list.push(card(report, c));
        }
        let mut runtimes: Vec<_> = self.runtimes.iter().collect();
        runtimes.sort_by_key(|r| !r.installed());
        for runtime in runtimes {
            let cli = runtime
                .cli
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "No CLI found".into());
            let mut row = column![
                heading(runtime.label.clone(), 18),
                note(
                    format!(
                        "{} · {}",
                        runtime.vendor,
                        runtime
                            .version
                            .clone()
                            .unwrap_or_else(|| if runtime.installed() {
                                "Installed"
                            } else {
                                "Not installed"
                            }
                            .into())
                    ),
                    c
                ),
                note(cli, c),
                note(
                    format!(
                        "MCP: {:?} · Hooks: {:?} · {} unregistered process(es)",
                        runtime.mcp, runtime.hooks, runtime.running
                    ),
                    c
                )
            ]
            .spacing(8);
            for app in &runtime.apps {
                row = row.push(note(format!("Application: {}", app.label), c));
            }
            let supported =
                runtime.installed() && (runtime.mcp.needs_review() || runtime.hooks.needs_review());
            if supported {
                row = row.push(action(
                    format!("setup-{}", runtime.name),
                    "Review setup",
                    (!self.setup_busy).then_some(Message::Setup(vec![
                        runtime.name.clone(),
                        "--preview".into(),
                    ])),
                    false,
                ));
            }
            row=row.push(note("Installation and configuration do not establish session activity or message consumption.",c));
            list = list.push(card(row, c));
        }
        list.into()
    }

    fn setup_view(&self, plan: &serde_json::Value, c: Colors) -> Element<'_, Message> {
        let id = value(plan, "id");
        let phase = value(plan, "phase");
        let mut body = column![
            heading("Review integration changes", 20),
            note(format!("{phase} · {id}"), c)
        ]
        .spacing(10);
        if let Some(executable) = plan["executable"].as_str() {
            body = body.push(note(format!("Connect through {executable}"), c));
        }
        let changes = plan["changes"].as_array();
        for change in changes.into_iter().flatten() {
            body = body.push(
                text(format!(
                    "{} · {}\n{}\n{}",
                    value(change, "runtime"),
                    value(change, "channel"),
                    value(change, "path"),
                    value(change, "action")
                ))
                .size(14),
            );
        }
        if changes.is_none_or(|c| c.is_empty()) {
            body = body.push(note(
                "Nothing to change; these connections are already configured.",
                c,
            ));
        }
        for note_text in plan["notes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|s| s.as_str())
        {
            body = body.push(note(note_text, c));
        }
        let applicable = !self.setup_busy
            && matches!(phase.as_str(), "prepared" | "applying")
            && changes.is_some_and(|c| !c.is_empty())
            && !id.is_empty();
        body = body.push(
            row![
                action(
                    "apply-setup",
                    "Apply reviewed changes",
                    applicable.then_some(Message::Setup(vec!["--apply".into(), id.clone()])),
                    true
                ),
                action(
                    "undo-setup",
                    "Undo this setup",
                    (!self.setup_busy && phase != "undone")
                        .then_some(Message::Setup(vec!["--undo".into(), id])),
                    false
                ),
                action(
                    "close-setup",
                    "Close",
                    (!self.setup_busy).then_some(Message::SetupClose),
                    false
                )
            ]
            .spacing(6),
        );
        card(body, c)
    }

    fn settings_view(&self, c: Colors) -> Element<'_, Message> {
        let mut body = column![
            heading("Appearance", 20),
            row![
                action(
                    "light-theme",
                    "Light",
                    Some(Message::Dark(false)),
                    !self.shell.catalog.dark
                ),
                action(
                    "dark-theme",
                    "Dark",
                    Some(Message::Dark(true)),
                    self.shell.catalog.dark
                )
            ]
            .spacing(8),
            row![
                action(
                    "smaller-ui",
                    "Smaller text",
                    Some(Message::TextSize(self.settings.text_size - 1.0)),
                    false
                ),
                action(
                    "larger-ui",
                    "Larger text",
                    Some(Message::TextSize(self.settings.text_size + 1.0)),
                    false
                )
            ]
            .spacing(8),
            heading("Terminal", 20),
            row![
                action(
                    "smaller-terminal",
                    "Smaller terminal text",
                    Some(Message::TerminalSize(self.settings.terminal_size - 1.0)),
                    false
                ),
                action(
                    "larger-terminal",
                    "Larger terminal text",
                    Some(Message::TerminalSize(self.settings.terminal_size + 1.0)),
                    false
                )
            ]
            .spacing(8)
        ]
        .spacing(16);
        for palette in crate::theme::PALETTES {
            body = body.push(action(
                format!("palette-{}", palette.name),
                palette.name,
                Some(Message::Palette(palette.name.into())),
                self.settings.palette == palette.name,
            ));
        }
        body=body.push(action("roomy-rows",if self.settings.roomy{"Use compact rows"}else{"Use roomier rows"},Some(Message::Roomy(!self.settings.roomy)),self.settings.roomy))
            .push(heading("Keyboard and accessibility",20)).push(note("Tab / Shift+Tab move through controls. Enter / Space activate focused buttons. Command/Ctrl+1–4 switch sections. F6 leaves terminal input. Escape closes session details or a draft launch form. Text controls support native input methods.",c))
            .push(heading("Installation",20)).push(action("installation","Manage installation and retained versions",Some(Message::Navigate(Screen::Desktop)),false))
            .push(heading("Diagnostics",20)).push(note(format!("agentdocker {}\nLocal daemon: {}\nWorkspace preferences: {}",env!("CARGO_PKG_VERSION"),self.socket,self.home.join("workspace.json").display()),c));
        body.into()
    }

    fn installation_view(&self, c: Colors) -> Element<'_, Message> {
        let p = &self.desktop;
        let mut body=column![heading("Desktop installation",20),note("Preview an install, rollback, or cleanup before applying it. Activation takes effect on the next app launch; running agents continue.",c),input("desktop-source","Application bundle or extracted package",&p.source,Message::DesktopSource),action("desktop-use-current","Use this application",(!p.busy).then_some(Message::DesktopUseCurrent),false),input("desktop-prefix","Installation prefix (empty uses your home)",&p.prefix,Message::DesktopPrefix)].spacing(14);
        if cfg!(target_os = "macos") {
            body = body.push(action(
                "desktop-local",
                if p.local_preview {
                    "Local preview signatures allowed"
                } else {
                    "Allow locally signed preview builds"
                },
                (!p.busy).then_some(Message::DesktopLocal(!p.local_preview)),
                p.local_preview,
            ));
        }
        for (operation, label) in [
            ("status", "Show installed versions"),
            ("install", "Preview installation"),
            ("rollback", "Preview rollback"),
            ("prune", "Preview cleanup"),
            ("uninstall", "Preview removal"),
        ] {
            body = body.push(action(
                format!("desktop-{operation}"),
                label,
                p.preview(operation)
                    .map(|_| Message::DesktopPreview(operation.into())),
                false,
            ));
        }
        if p.busy {
            body = body.push(note("Verifying installation…", c));
        }
        if let Some(error) = &p.error {
            body = body.push(text(error.clone()).color(c.amber));
        }
        if let Some(report) = &p.report {
            let mut details = column![heading(
                if report["preview"] == true {
                    "Review this plan"
                } else {
                    "Installation report"
                },
                18
            )]
            .spacing(10);
            if let Some(maintenance) = report.get("maintenance") {
                for path in maintenance["remove"].as_array().into_iter().flatten() {
                    details = details.push(
                        text(format!("Remove: {}", path.as_str().unwrap_or("unknown"))).size(14),
                    );
                }
                for entry in maintenance["retained"].as_array().into_iter().flatten() {
                    details = details.push(note(
                        format!(
                            "Keep: {} · {}",
                            value(entry, "path"),
                            value(entry, "reason")
                        ),
                        c,
                    ));
                }
            } else if let Some(installation) = report.get("installation") {
                if installation.is_null() {
                    details = details.push(note("No managed installation at this prefix.", c));
                } else {
                    for (key, label) in [("current", "Active"), ("previous", "Previous")] {
                        if !installation[key].is_null() {
                            details = details.push(note(
                                format!(
                                    "{label}: {} · source {}",
                                    value(&installation[key], "version"),
                                    value(&installation[key], "source_commit")
                                ),
                                c,
                            ));
                        }
                    }
                }
            } else {
                details = details.push(note(
                    format!(
                        "Release {} · source {}",
                        value(&report["candidate"], "version"),
                        value(&report["candidate"], "source_commit")
                    ),
                    c,
                ));
                for key in ["source", "application", "bin", "versions"] {
                    if let Some(path) = report[key].as_str() {
                        details = details.push(text(format!("{key}: {path}")).size(14));
                    }
                }
            }
            if report["preview"] == true {
                details = details.push(action(
                    "desktop-apply",
                    "Apply this reviewed plan",
                    p.apply().map(|_| Message::DesktopApply),
                    true,
                ));
            }
            body = body.push(card(details, c));
        }
        body.into()
    }
}
