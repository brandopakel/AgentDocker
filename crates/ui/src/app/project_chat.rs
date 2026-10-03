//! The project's everyday workspace: shared chat and the agents working here.
use super::icons::{Icon, icon};
use super::messages::look::{link, with_presence};
use super::style::{Colors, weight};
use super::view::{agent_mark, dot, note, rule};
use super::*;
use crate::controls::{Kind, custom, custom_sized};
use iced::{
    Center, Element, Fill,
    widget::{Space, column, container, row, scrollable, text},
};

impl App {
    pub(super) fn open_project_chat(&mut self) {
        let Some(project) = self.selected_project_id() else {
            self.screen = Screen::Agents;
            self.shell.conversation = None;
            self.shell.thread = None;
            self.thread = None;
            self.cancel_reveal();
            return;
        };
        self.screen = Screen::Chat;
        self.shell.thread = None;
        self.thread = None;
        self.cancel_reveal();
        let conversation = format!("everyone:{project}");
        self.shell.conversation = Some(conversation.clone());
        self.shell.inbox_open = true;
        if self.connected.is_ok() {
            self.send(Cmd::Conversations(self.conversation_scope()));
            self.send(Cmd::History(conversation, self.history_epoch));
        }
    }

    pub(super) fn project_chat_view(&self, c: Colors) -> Element<'_, Message> {
        let compact = self.messages_compact();
        let conversation = self.messages_pane(c);
        if compact {
            let body = if self.shell.thread.is_some() {
                column![
                    Self::back_control(
                        "close-thread",
                        "Back to chat",
                        "Chat",
                        Message::CloseThread,
                        c
                    ),
                    self.thread_pane(c),
                ]
                .spacing(6)
                .height(Fill)
                .into()
            } else {
                conversation
            };
            return column![self.project_chat_strip(c), container(body).height(Fill)]
                .spacing(10)
                .height(Fill)
                .into();
        }
        // The columns are told apart by a hairline each, as on Messages.
        let hairline = || container(Space::new().width(1).height(Fill)).style(move |_| c.rule());
        let mut panes = row![container(conversation).width(Fill).height(Fill)].spacing(9);
        if self.shell.thread.is_some() {
            panes = panes
                .push(hairline())
                .push(container(self.thread_pane(c)).width(300).height(Fill));
        }
        panes
            .push(hairline())
            .push(
                container(
                    scrollable(self.project_chat_agents(c))
                        .direction(super::messages::look::slim_scrollbar(4.0))
                        .id("chat-agents"),
                )
                .width(250)
                .height(Fill),
            )
            .height(Fill)
            .into()
    }

    /// The live agents of this project, one row per session.
    pub(super) fn chat_agents(&self) -> Vec<&agentdocker_core::AgentRecord> {
        self.agents
            .iter()
            .filter(|a| {
                self.live_session(a)
                    && a.spec.runtime != agentdocker_core::HUMAN_RUNTIME
                    && self.has_project(a.project.as_ref())
            })
            .collect()
    }

    /// The questions these agents are waiting on, oldest first.
    fn chat_questions(
        &self,
        agents: &[&agentdocker_core::AgentRecord],
    ) -> Vec<&agentdocker_core::Question> {
        self.questions
            .iter()
            .filter(|q| !q.expired(Utc::now()) && agents.iter().any(|a| a.id.as_str() == q.from))
            .collect()
    }

    /// An agent's presence tone: amber while it waits on the person, green
    /// while it reports work, quiet while all that is known is that it runs.
    fn chat_agent_tone(&self, agent: &agentdocker_core::AgentRecord, c: Colors) -> iced::Color {
        if self.needs_input(agent.id.as_str()) || self.delivery_needs_you(agent) {
            c.amber
        } else if agent.status.is_live() {
            match self.activity.get(agent.id.as_str()) {
                None | Some(Activity::Unknown | Activity::Starting) => c.faint,
                Some(_) => c.green,
            }
        } else {
            c.faint
        }
    }

    /// What an agent is doing and for how long: `working · 3m`,
    /// `needs input · 12m`. The span is as coarse as the window's sweep.
    fn chat_agent_status(&self, agent: &agentdocker_core::AgentRecord) -> String {
        let now = Utc::now();
        let label = self.activity_label(agent);
        let since = if self.needs_input(agent.id.as_str()) {
            self.questions
                .iter()
                .filter(|q| q.from == agent.id.as_str() && !q.expired(now))
                .map(|q| q.asked_at)
                .min()
        } else {
            match self.activity.get(agent.id.as_str()) {
                Some(
                    Activity::Working { since }
                    | Activity::Idle { since }
                    | Activity::Blocked { since, .. },
                ) => Some(*since),
                _ => agent.started_at,
            }
        };
        match since {
            Some(since) => format!(
                "{label} · {}",
                super::messages::look::elapsed((now - since).num_seconds())
            ),
            None => label,
        }
    }

    /// The side panel beside shared chat: a header with the count and the
    /// way to every agent, what the agents are waiting on, and one row per
    /// agent — its mark and presence, its name over what it is doing, and
    /// its terminal one quiet click away.
    fn project_chat_agents(&self, c: Colors) -> Element<'_, Message> {
        let agents = self.chat_agents();
        let asking = self.chat_questions(&agents);
        let mut body = column![
            container(
                row![
                    text("Agents")
                        .size(13)
                        .font(weight(iced::font::Weight::Semibold)),
                    text(agents.len().to_string()).size(12).color(c.faint),
                    Space::new().width(Fill),
                    link(
                        "chat-all-agents",
                        "All agents",
                        "All agents",
                        Some(Message::Navigate(Screen::Agents)),
                        c.accent,
                    ),
                ]
                .spacing(6)
                .align_y(Center),
            )
            .padding(iced::Padding {
                top: 4.0,
                right: 0.0,
                bottom: 0.0,
                left: 8.0,
            })
        ]
        .spacing(10)
        .padding(iced::Padding {
            top: 0.0,
            right: 8.0,
            bottom: 8.0,
            left: 6.0,
        });
        // Opens the oldest question itself; the next one follows once it
        // is answered, as in Needs you.
        if let Some(first) = asking.first() {
            let spoken = if asking.len() == 1 {
                "Answer 1 question".to_owned()
            } else {
                format!("Answer {} questions", asking.len())
            };
            body = body.push(
                container(
                    row![
                        dot(c.amber, 6.0, c),
                        text(if asking.len() == 1 {
                            "1 question waiting".to_owned()
                        } else {
                            format!("{} questions waiting", asking.len())
                        })
                        .size(12.5)
                        .font(weight(iced::font::Weight::Medium))
                        .width(Fill),
                        custom(
                            "chat-needs-input",
                            spoken,
                            text("Answer")
                                .size(12)
                                .line_height(iced::Pixels(16.0))
                                .font(weight(iced::font::Weight::Medium)),
                            Some(Message::OpenQuestion(first.id.clone())),
                            false,
                            Kind::Secondary,
                            [4, 10],
                        ),
                    ]
                    .spacing(8)
                    .align_y(Center),
                )
                .padding(iced::Padding {
                    top: 6.0,
                    right: 6.0,
                    bottom: 6.0,
                    left: 10.0,
                })
                .style(move |_| c.attention_style(c.amber)),
            );
        }
        if agents.is_empty() {
            body = body.push(
                container(note("Launch an agent to work on this project.", c).size(12))
                    .padding([0, 8]),
            );
        }
        let mut rows = column![].spacing(2);
        for agent in agents {
            let id = agent.id.to_string();
            let name = self.display_name(agent);
            let open = custom(
                format!("chat-agent-{id}"),
                "Open agent",
                row![
                    with_presence(
                        agent_mark(Some(agent.spec.runtime.as_str()), &name, &id, 28.0, c),
                        28.0,
                        Some(self.chat_agent_tone(agent, c)),
                        c.ground,
                    ),
                    column![
                        container(
                            text(name.clone())
                                .size(13.5)
                                .font(weight(iced::font::Weight::Medium))
                                .wrapping(iced::widget::text::Wrapping::None)
                        )
                        .width(Fill)
                        .clip(true),
                        container(
                            text(self.chat_agent_status(agent))
                                .size(12)
                                .color(c.muted)
                                .wrapping(iced::widget::text::Wrapping::None)
                        )
                        .width(Fill)
                        .clip(true),
                    ]
                    .spacing(1)
                    .width(Fill),
                ]
                .spacing(10)
                .align_y(Center),
                Some(Message::OpenSession(id.clone())),
                false,
                Kind::Quiet,
                [6, 8],
            );
            let terminal = custom_sized(
                format!("chat-terminal-{id}"),
                "Terminal",
                container(icon(Icon::Terminal, c.muted, 15.0)).center(18),
                (!self.shell.terminal_opening).then_some(Message::OpenAgentTerminal(id)),
                false,
                Kind::Ghost,
                [7, 7],
                iced::Length::Shrink,
            );
            rows = rows.push(row![open, terminal].spacing(2).align_y(Center));
        }
        body.push(rows).into()
    }

    /// The same, in a narrow window: one line of agents above the chat —
    /// each a mark and a name to open it, and its terminal beside it.
    fn project_chat_strip(&self, c: Colors) -> Element<'_, Message> {
        let agents = self.chat_agents();
        let asking = self.chat_questions(&agents);
        let mut strip = row![].spacing(4).align_y(Center);
        if let Some(first) = asking.first() {
            let spoken = if asking.len() == 1 {
                "Answer 1 question".to_owned()
            } else {
                format!("Answer {} questions", asking.len())
            };
            strip = strip.push(custom(
                "chat-needs-input",
                spoken,
                row![
                    dot(c.amber, 6.0, c),
                    text(format!("Answer {}", asking.len()))
                        .size(12)
                        .line_height(iced::Pixels(16.0))
                        .font(weight(iced::font::Weight::Medium)),
                ]
                .spacing(6)
                .align_y(Center),
                Some(Message::OpenQuestion(first.id.clone())),
                false,
                Kind::Secondary,
                [5, 10],
            ));
        }
        if agents.is_empty() {
            strip = strip.push(note("Launch an agent to work on this project.", c).size(12));
        }
        for agent in agents {
            let id = agent.id.to_string();
            let name = self.display_name(agent);
            strip = strip.push(custom(
                format!("chat-agent-{id}"),
                name.clone(),
                row![
                    with_presence(
                        agent_mark(Some(agent.spec.runtime.as_str()), &name, &id, 20.0, c),
                        20.0,
                        Some(self.chat_agent_tone(agent, c)),
                        c.ground,
                    ),
                    text(name)
                        .size(12.5)
                        .line_height(iced::Pixels(16.0))
                        .font(weight(iced::font::Weight::Medium)),
                ]
                .spacing(7)
                .align_y(Center),
                Some(Message::OpenSession(id.clone())),
                false,
                Kind::Inline,
                [4, 8],
            ));
            strip = strip.push(custom_sized(
                format!("chat-terminal-{id}"),
                "Terminal",
                container(icon(Icon::Terminal, c.muted, 14.0)).center(16),
                (!self.shell.terminal_opening).then_some(Message::OpenAgentTerminal(id)),
                false,
                Kind::Ghost,
                [6, 6],
                iced::Length::Shrink,
            ));
        }
        strip = strip.push(link(
            "chat-all-agents",
            "All agents",
            "All agents",
            Some(Message::Navigate(Screen::Agents)),
            c.accent,
        ));
        column![
            scrollable(container(strip).padding(iced::Padding {
                top: 0.0,
                right: 0.0,
                bottom: 6.0,
                left: 0.0,
            }))
            .id("chat-agents")
            .direction(iced::widget::scrollable::Direction::Horizontal(
                iced::widget::scrollable::Scrollbar::new()
                    .width(4)
                    .scroller_width(4),
            )),
            rule(c),
        ]
        .spacing(4)
        .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::shell::Message;
    use agentdocker_core::{MessageId, ProjectRef};

    #[test]
    fn project_chat_selects_the_correct_queue_and_keeps_other_drafts() {
        let (tx, commands) = queue::channel();
        let (_sender, rx) = std::sync::mpsc::sync_channel(MESSAGE_CAPACITY);
        let mut app = App::bare(tx, rx);
        app.connected = Ok(());
        for name in ["alpha", "beta"] {
            let mut project = ProjectRef::directory(format!("/fixture/{name}"));
            project.fingerprint = Some(name.to_owned());
            app.shell.catalog.remember(project, false);
        }
        let _ = app.update(Message::SelectProject("/fixture/alpha".into()));
        assert_eq!(app.screen, Screen::Chat);
        let alpha = app.shell.conversation.clone().unwrap();
        assert_eq!(
            app.conversation_destination(&alpha),
            Some("project:alpha".into())
        );
        let _ = app.update(Message::ConversationDraft(
            alpha.clone(),
            "Keep my alpha draft".into(),
        ));
        app.shell.thread = Some(MessageId::from("alpha-thread".to_owned()));
        let _ = app.update(Message::SelectProject("/fixture/beta".into()));
        assert_eq!(app.shell.conversation.as_deref(), Some("everyone:beta"));
        assert!(app.shell.thread.is_none());
        assert_eq!(
            app.shell.conversation_drafts[&alpha].text,
            "Keep my alpha draft"
        );
        assert!(
            commands
                .try_iter()
                .any(|c| matches!(c, Cmd::History(id, _) if id == "everyone:beta"))
        );
        // No background view may mark a hidden conversation as read.
        assert!(app.conversation_pane_visible());
        app.shell.thread = Some(MessageId::from("beta-thread".to_owned()));
        app.shell.width = 700.0;
        assert!(!app.conversation_pane_visible());
        let _ = app.update(Message::Navigate(Screen::Board));
        assert!(!app.conversation_pane_visible());
    }

    #[test]
    fn removing_the_selected_project_cannot_keep_its_chat_target() {
        for removal in 0..3 {
            let root = tempfile::tempdir().unwrap();
            let alpha = root.path().join("alpha");
            let beta = root.path().join("beta");
            std::fs::create_dir(&alpha).unwrap();
            std::fs::create_dir(&beta).unwrap();
            let (tx, _commands) = queue::channel();
            let (_sender, rx) = std::sync::mpsc::sync_channel(MESSAGE_CAPACITY);
            let mut app = App::bare(tx, rx);
            for path in [&alpha, &beta] {
                app.shell
                    .catalog
                    .remember(ProjectRef::directory(path), false);
            }
            let _ = app.update(Message::SelectProject(alpha.clone()));
            let old = app.shell.conversation.clone().unwrap();
            let _ = app.update(Message::ConversationDraft(old.clone(), "Keep alpha".into()));
            match removal {
                0 => {
                    let _ = app.update(Message::ProjectRemove(alpha));
                }
                1 => {
                    let _ = app.update(Message::ForgetProject);
                }
                _ => {
                    std::fs::remove_dir(alpha).unwrap();
                    let _ = app.update(Message::Tick);
                }
            }
            let expected = format!("everyone:{}", ProjectRef::directory(&beta).id());
            assert_eq!(app.shell.catalog.selected.as_ref(), Some(&beta));
            assert_eq!(app.shell.conversation.as_deref(), Some(expected.as_str()));
            assert_eq!(app.shell.conversation_drafts[&old].text, "Keep alpha");
            let _ = app.update(Message::ProjectRemove(beta));
            assert_eq!(app.screen, Screen::Agents);
            assert!(app.shell.conversation.is_none());
        }
    }

    #[test]
    fn adding_a_project_opens_chat_and_a_thread_never_squeezes_its_composer() {
        let (tx, _commands) = queue::channel();
        let (_sender, rx) = std::sync::mpsc::sync_channel(MESSAGE_CAPACITY);
        let mut app = App::bare(tx, rx);
        let _ = app.update(Message::FolderResolved(Ok(ProjectRef::directory(
            "/fixture/new",
        ))));
        assert_eq!(app.screen, Screen::Chat);
        app.shell.width = 1001.0;
        app.panes.window_width(1001.0);
        app.shell.thread = Some(MessageId::from("thread".to_owned()));
        app.panes.sync_thread(true);
        assert!(app.messages_compact());
        app.shell.width = 1500.0;
        app.panes.window_width(1500.0);
        assert!(!app.messages_compact());
    }
}
