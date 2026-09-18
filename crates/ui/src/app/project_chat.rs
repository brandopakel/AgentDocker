//! The project's everyday workspace: shared chat and the agents working here.
use super::style::Colors;
use super::view::{heading, note, rule, small};
use super::*;
use crate::controls::{button as action, primary};
use iced::{
    Element, Fill,
    widget::{column, container, row, scrollable},
};

impl App {
    pub(super) fn open_project_chat(&mut self) {
        let Some(project) = self.selected_project_id() else {
            self.screen = Screen::Agents;
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
        let height = (self.shell.height / self.scale_factor() - 275.0).max(280.0);
        let agents = self.project_chat_agents(c);
        let conversation = self.messages_pane(c);
        if self.messages_compact() {
            let body = if self.shell.thread.is_some() {
                self.thread_pane(c)
            } else {
                conversation
            };
            return column![
                container(scrollable(agents)).height(140),
                container(body).height((height - 152.0).max(200.0)),
            ]
            .spacing(12)
            .into();
        }
        let mut panes = row![container(conversation).width(Fill).height(height)].spacing(18);
        if self.shell.thread.is_some() {
            panes = panes.push(container(self.thread_pane(c)).width(300).height(height));
        }
        panes
            .push(container(scrollable(agents)).width(250).height(height))
            .into()
    }

    fn project_chat_agents(&self, c: Colors) -> Element<'_, Message> {
        let agents: Vec<_> = self
            .agents
            .iter()
            .filter(|a| {
                a.status.is_live()
                    && a.spec.runtime != agentdocker_core::HUMAN_RUNTIME
                    && self.has_project(a.project.as_ref())
            })
            .collect();
        let mut body = column![heading(format!("Agents ({})", agents.len()), 16)].spacing(12);
        let needs_input = self
            .questions
            .iter()
            .filter(|q| !q.expired(Utc::now()) && agents.iter().any(|a| a.id.as_str() == q.from))
            .count();
        if needs_input > 0 {
            body = body.push(action(
                "chat-needs-input",
                format!("{needs_input} need your input"),
                Some(Message::Navigate(Screen::Questions)),
                false,
            ));
        }
        if agents.is_empty() {
            body = body.push(note("Launch an agent to work on this project.", c));
        }
        for agent in agents {
            let id = agent.id.to_string();
            body = body.push(
                column![
                    heading(self.display_name(agent), 14),
                    small(self.activity_label(agent), c),
                    row![
                        action(
                            format!("chat-agent-{id}"),
                            "Open agent",
                            Some(Message::OpenSession(id.clone())),
                            false
                        ),
                        primary(
                            format!("chat-terminal-{id}"),
                            "Terminal",
                            (!self.shell.terminal_opening)
                                .then_some(Message::OpenAgentTerminal(id))
                        )
                    ]
                    .spacing(6)
                    .wrap(),
                    rule(c),
                ]
                .spacing(6),
            );
        }
        body.push(action(
            "chat-all-agents",
            "All agents",
            Some(Message::Navigate(Screen::Agents)),
            false,
        ))
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
}
