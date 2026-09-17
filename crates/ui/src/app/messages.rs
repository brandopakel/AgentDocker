//! Messages as a workspace: a sidebar of channels and direct conversations
//! with what is unread, one pane that always has a composer, and a thread
//! beside it. The shape people know from Slack and Discord, over the
//! daemon's conversations: the archive is what is shown, the queues stay
//! the agents' own.
use super::panes::{Grid, Slot};
use super::style::{Colors, weight};
use super::view::{
    dot, empty, eyebrow, first_line, monogram, note, panel, pill, rule, small, split_style,
};
use super::*;
use crate::controls::{
    Kind, button as action, custom, input_enabled, input_submitting, primary, segment,
};
use agentdocker_core::conversation::line_of;
use agentdocker_core::journal::ago;
use agentdocker_core::{
    AgentId, AgentRecord, ArchivedMessage, ConversationKind, ConversationSummary, MessageId,
};
use iced::{
    Center, Element, Fill,
    widget::{Space, column, container, row, scrollable, text},
};

impl App {
    /// Whether the daemon lists conversations, so this screen can be shown
    /// instead of the inbox.
    pub(super) fn has_conversations(&self) -> bool {
        self.conversations_supported == Some(true)
    }

    /// Unread across the conversations that are the person's to answer:
    /// channels, broadcasts and their own direct messages. What two agents
    /// say to each other, what AgentDocker told them, and the collision
    /// rooms it opens between two checkouts (the person is not admitted to
    /// those; their rows are the daemon's own contested-path notices) are
    /// read here, not owed.
    pub(super) fn unread_total(&self) -> u64 {
        self.conversations
            .iter()
            .filter(|c| self.counts_for_person(c))
            .map(|c| c.unread)
            .sum()
    }

    /// What a sidebar row says is unread: what the person owes, and on a
    /// collision room what the daemon noted there, worth a look without
    /// being owed; a room two agents share or a notice to an agent is
    /// listed quietly with its last line.
    fn row_unread(&self, summary: &ConversationSummary) -> u64 {
        if self.counts_for_person(summary) || summary.kind == ConversationKind::Collision {
            summary.unread
        } else {
            0
        }
    }

    pub(super) fn counts_for_person(&self, summary: &ConversationSummary) -> bool {
        match summary.kind {
            ConversationKind::Dm => self.counterpart(summary).is_some(),
            ConversationKind::Notices | ConversationKind::Collision => false,
            _ => true,
        }
    }

    /// What `@name` means the person: the person's record name, `user`
    /// and `you`.
    fn mention_names(&self) -> Vec<String> {
        let mut names = vec![agentdocker_core::HUMAN.to_owned(), "you".to_owned()];
        names.extend(
            self.agents
                .iter()
                .filter(|a| self.is_human(a.id.as_str()))
                .map(|a| a.spec.name.clone()),
        );
        names
    }

    /// The tool an agent is, without the branch: `Codex`, `Claude Code`.
    fn tool_of(&self, id: &str) -> String {
        let name = self.name_of(id);
        name.split(" · ").next().unwrap_or(&name).to_owned()
    }

    /// The other party of a direct conversation the person is in; none for
    /// one between two agents, which is read here and written by neither
    /// side of this window.
    fn counterpart<'a>(&self, summary: &'a ConversationSummary) -> Option<&'a str> {
        let (a, b) = summary.conversation.dm_parties()?;
        if self.is_human(a) {
            Some(b)
        } else if self.is_human(b) {
            Some(a)
        } else {
            None
        }
    }

    fn agent_live(&self, id: &str) -> bool {
        let id = self.canonical_agent(id);
        self.agents
            .iter()
            .any(|a| a.id.as_str() == id && a.status.is_live())
    }

    /// The name the sidebar and headers show: `#name` for a channel or the
    /// broadcast, the other party's name for a direct conversation, the
    /// room's task for a collision, AgentDocker for notices.
    fn conversation_label(&self, summary: &ConversationSummary) -> String {
        match summary.kind {
            // Every project has one; when more than one is on view, say
            // whose.
            ConversationKind::Everyone => {
                if self.conversation_scope().is_some() {
                    "#everyone".to_owned()
                } else {
                    format!("#everyone · {}", summary.title)
                }
            }
            ConversationKind::All => "#all".to_owned(),
            // A room opened before names, or a collision room, is called
            // by a short name made from its task or paths, never by the
            // whole task; the header carries the rest.
            ConversationKind::Channel | ConversationKind::Collision => match &summary.name {
                Some(name) if !name.is_empty() => format!("#{name}"),
                _ => format!("#{}", Self::short_room_name(summary)),
            },
            // A pair of agents reads as their tools: the branch each is on
            // belongs under the header, not in the list.
            ConversationKind::Dm => match self.counterpart(summary) {
                Some(id) => self.name_of(id),
                None => match summary.conversation.dm_parties() {
                    Some((a, b)) => format!("{} ↔ {}", self.tool_of(a), self.tool_of(b)),
                    None => summary.title.clone(),
                },
            },
            // Each agent's notices are its own conversation: say whose.
            ConversationKind::Notices => summary
                .conversation
                .notices_agent()
                .map(|agent| format!("AgentDocker → {}", self.name_of(agent.as_str())))
                .unwrap_or_else(|| "AgentDocker".to_owned()),
        }
    }

    /// A slug from the room's task or first path, at most a few words; an
    /// empty title falls back to the room's short id.
    fn short_room_name(summary: &ConversationSummary) -> String {
        let title = summary.title.trim();
        let from_title = agentdocker_core::conversation::channel_name_from(title);
        match from_title {
            Some(name) if !name.is_empty() => {
                // At most four words of the slug, so the list stays a list.
                name.split('-')
                    .filter(|w| !w.is_empty())
                    .take(4)
                    .collect::<Vec<_>>()
                    .join("-")
            }
            _ => summary
                .conversation
                .channel_id()
                .map(|id| {
                    format!(
                        "room-{}",
                        id.to_string().chars().take(6).collect::<String>()
                    )
                })
                .unwrap_or_else(|| "room".to_owned()),
        }
    }

    fn selected_summary(&self) -> Option<&ConversationSummary> {
        let open = self.shell.conversation.as_deref()?;
        self.conversations
            .iter()
            .find(|c| c.conversation.as_str() == open)
    }

    /// The open conversation as the pane needs it: the daemon's summary
    /// when it has listed it, else one made from the id alone, so a direct
    /// conversation nobody has spoken in yet, or a channel opened before the
    /// list arrived, still shows its header and composer.
    fn open_summary(&self) -> Option<ConversationSummary> {
        if let Some(summary) = self.selected_summary() {
            return Some(summary.clone());
        }
        let open = self.shell.conversation.as_deref()?;
        let conversation = agentdocker_core::ConversationId::from(open.to_owned());
        let kind = conversation.kind()?;
        let (name, title, members) = match kind {
            ConversationKind::Dm => {
                let (a, b) = conversation.dm_parties()?;
                let other = if self.is_human(a) { b } else { a };
                (
                    None,
                    self.name_of(other),
                    vec![AgentId::from(a.to_owned()), AgentId::from(b.to_owned())],
                )
            }
            ConversationKind::Channel => {
                let id = conversation.channel_id()?;
                let channel = self.channels.iter().find(|c| c.id == id)?;
                (
                    channel.name.clone(),
                    channel.subject.title(),
                    channel.members.clone(),
                )
            }
            _ => return None,
        };
        Some(ConversationSummary {
            conversation,
            kind,
            name,
            title,
            members,
            unread: 0,
            mentions: 0,
            last_seq: None,
            last_at: None,
            last_from: None,
            last_line: None,
        })
    }

    /// The screen sits inside the workspace's own scroll, so it takes a
    /// height of its own from the window rather than fill what has none:
    /// the window less the chrome above and below it.
    fn workspace_height(&self) -> f32 {
        (self.shell.height / self.scale_factor() - 250.0).max(320.0)
    }

    pub(super) fn messages_compact(&self) -> bool {
        self.narrow() || self.panes.compact_messages()
    }

    pub(super) fn messages_view(&self, c: Colors) -> Element<'_, Message> {
        let narrow = self.messages_compact();
        let height = self.workspace_height();
        if narrow {
            if self.shell.inbox_open && self.shell.conversation.is_some() {
                // A thread takes the whole window too, with its own way back.
                if self.shell.thread.is_some() {
                    let back = custom(
                        "close-thread",
                        "Back to the conversation",
                        row![text("‹ Conversation").size(14)],
                        Some(Message::CloseThread),
                        false,
                        Kind::Quiet,
                        [8, 10],
                    );
                    return column![back, container(self.thread_pane(c)).height(height)]
                        .spacing(12)
                        .into();
                }
                let back = custom(
                    "thread-back",
                    "Conversations",
                    row![text("‹ Conversations").size(14)],
                    Some(Message::InboxList),
                    false,
                    Kind::Quiet,
                    [8, 10],
                );
                return column![back, container(self.messages_pane(c)).height(height)]
                    .spacing(12)
                    .into();
            }
            return container(self.messages_sidebar(c)).height(height).into();
        }
        // Wide: three columns with draggable dividers between them. The
        // widths are the person's, kept in pixels and remembered across
        // runs; the thread column comes and goes with the thread.
        let grid = iced::widget::pane_grid(&self.panes.messages, |_, slot, _| {
            // A hairline where a column begins, so the divider is seen at
            // rest as well as when it is hovered.
            let hairline =
                || container(Space::new().width(1).height(height)).style(move |_| c.rule());
            let body: Element<'_, Message> = match slot {
                Slot::Sidebar => self.messages_sidebar(c),
                Slot::Thread => row![
                    hairline(),
                    container(self.thread_pane(c))
                        .width(Fill)
                        .padding(iced::Padding {
                            top: 0.0,
                            right: 0.0,
                            bottom: 0.0,
                            left: 12.0,
                        })
                ]
                .into(),
                _ => row![
                    hairline(),
                    container(self.messages_pane(c))
                        .width(Fill)
                        .padding(iced::Padding {
                            top: 0.0,
                            right: 0.0,
                            bottom: 0.0,
                            left: 16.0,
                        })
                ]
                .into(),
            };
            iced::widget::pane_grid::Content::new(
                container(body).width(Fill).height(height).clip(true),
            )
        })
        .on_resize(8, |event| Message::PaneResized(Grid::Messages, event))
        .style(move |_| split_style(c))
        .height(height);
        grid.into()
    }

    /// One row of the sidebar: mark, label, the last line, the unread count.
    fn conversation_row(
        &self,
        summary: &ConversationSummary,
        presence: Option<bool>,
        c: Colors,
    ) -> Element<'_, Message> {
        let label = self.conversation_label(summary);
        let selected = self.shell.conversation.as_deref() == Some(summary.conversation.as_str());
        let mark: Element<'_, Message> = match summary.kind {
            ConversationKind::Dm => {
                let seed = self
                    .counterpart(summary)
                    .unwrap_or(summary.conversation.as_str());
                monogram(&label, seed, 24.0, c)
            }
            ConversationKind::Notices => monogram("AgentDocker", "agentdocker", 24.0, c),
            _ => container(
                text("#")
                    .size(15)
                    .font(weight(iced::font::Weight::Semibold))
                    .color(c.muted),
            )
            .center(24.0)
            .into(),
        };
        let unread = self.row_unread(summary);
        let mut name = row![
            container(
                text(label.clone())
                    .size(14)
                    .font(weight(if unread > 0 {
                        iced::font::Weight::Semibold
                    } else {
                        iced::font::Weight::Normal
                    }))
                    .wrapping(iced::widget::text::Wrapping::None)
            )
            .width(Fill)
            .clip(true)
        ]
        .spacing(6)
        .align_y(Center);
        if let Some(live) = presence {
            name = name.push(dot(if live { c.green } else { c.faint }, 7.0, c));
        }
        let mut body = column![name].spacing(1).width(Fill);
        if let Some(line) = &summary.last_line {
            let who = summary
                .last_from
                .as_deref()
                .map(|from| {
                    if self.is_human(from) {
                        "You".to_owned()
                    } else if from == "agentd" {
                        "AgentDocker".to_owned()
                    } else {
                        self.name_of(from)
                    }
                })
                .unwrap_or_default();
            let preview = if matches!(
                summary.kind,
                ConversationKind::Dm | ConversationKind::Notices
            ) {
                first_line(line, 44)
            } else {
                first_line(&format!("{who}: {line}"), 44)
            };
            body = body.push(
                container(
                    text(preview)
                        .size(12)
                        .color(c.muted)
                        .wrapping(iced::widget::text::Wrapping::None),
                )
                .width(Fill)
                .clip(true),
            );
        }
        let mut content = row![mark, body].spacing(10).align_y(Center);
        // Unread, and how many of those name the person: the @ pill is
        // the one that asks for an answer.
        if unread > 0 && summary.mentions > 0 {
            content = content.push(pill(
                format!("@{}", summary.mentions),
                c.accent,
                iced::Color::WHITE,
                c,
            ));
        }
        if unread > 0 {
            content = content.push(pill(unread.to_string(), c.accent_soft, c.accent_ink, c));
        }
        // A direct conversation's row and composer keep the ids the inbox
        // used for the agent, so what drives one drives the other.
        let control_id = match self.counterpart(summary) {
            Some(agent) if summary.kind == ConversationKind::Dm => format!("thread-{agent}"),
            _ => format!("conversation-{}", summary.conversation),
        };
        custom(
            control_id,
            label,
            content,
            Some(Message::SelectConversation(
                summary.conversation.as_str().to_owned(),
            )),
            selected,
            Kind::Quiet,
            [7, 10],
        )
    }

    fn group_toggle<'a>(
        id: &str,
        label: String,
        open: bool,
        message: Message,
        c: Colors,
    ) -> Element<'a, Message> {
        custom(
            id.to_owned(),
            label.clone(),
            row![
                small(if open { "▾" } else { "▸" }, c),
                eyebrow(label, c).width(Fill)
            ]
            .spacing(6)
            .align_y(Center),
            Some(message),
            false,
            Kind::Quiet,
            [4, 6],
        )
    }

    fn messages_sidebar(&self, c: Colors) -> Element<'_, Message> {
        let filter = self.shell.messages_search.trim().to_lowercase();
        let matches = |summary: &ConversationSummary| {
            filter.is_empty()
                || self
                    .conversation_label(summary)
                    .to_lowercase()
                    .contains(&filter)
                || summary.title.to_lowercase().contains(&filter)
        };
        let mut channels: Vec<&ConversationSummary> = Vec::new();
        let mut collisions: Vec<&ConversationSummary> = Vec::new();
        let mut direct: Vec<&ConversationSummary> = Vec::new();
        let mut peers: Vec<&ConversationSummary> = Vec::new();
        let mut earlier: Vec<&ConversationSummary> = Vec::new();
        let mut notices: Vec<&ConversationSummary> = Vec::new();
        for summary in self.conversations.iter().filter(|s| matches(s)) {
            match summary.kind {
                ConversationKind::Everyone | ConversationKind::All | ConversationKind::Channel => {
                    channels.push(summary);
                }
                ConversationKind::Collision => collisions.push(summary),
                // The person's own direct messages are the list; what two
                // agents said to each other is a group of its own, folded.
                ConversationKind::Dm => match self.counterpart(summary) {
                    Some(id) if self.agent_live(id) => direct.push(summary),
                    Some(_) => earlier.push(summary),
                    None => peers.push(summary),
                },
                ConversationKind::Notices => notices.push(summary),
            }
        }
        // The broadcast first, then named rooms by their names.
        channels.sort_by_key(|s| {
            (
                match s.kind {
                    ConversationKind::Everyone => 0,
                    ConversationKind::All => 1,
                    _ => 2,
                },
                self.conversation_label(s),
            )
        });
        let mut list = column![].spacing(2);
        // Find one, or start one: the search and, beside it, the way to a
        // conversation that does not exist yet.
        let form_open = self.new_conversation.is_some();
        list = list.push(
            container(
                row![
                    input_enabled(
                        "messages-search",
                        "Find a conversation…",
                        &self.shell.messages_search,
                        Message::MessagesSearch,
                        true,
                    ),
                    custom(
                        "new-conversation",
                        if form_open {
                            "Close"
                        } else {
                            "New message or channel"
                        },
                        text(if form_open { "×" } else { "+" })
                            .size(18)
                            .color(if form_open { c.muted } else { c.accent }),
                        Some(Message::NewConversation),
                        form_open,
                        Kind::Quiet,
                        [4, 10],
                    ),
                ]
                .spacing(4)
                .align_y(Center),
            )
            .padding([2, 4]),
        );
        if let Some(form) = &self.new_conversation {
            list = list.push(container(self.new_conversation_form(form, c)).padding([4, 6]));
        }
        let unread_total = self.unread_total();
        if unread_total > 0 {
            let owed = self
                .conversations
                .iter()
                .filter(|s| s.unread > 0 && self.counts_for_person(s))
                .count();
            // The count, and one way to be done with it.
            list = list.push(
                container(
                    row![
                        small(
                            format!(
                                "{unread_total} unread in {owed} conversation{}",
                                if owed == 1 { "" } else { "s" }
                            ),
                            c,
                        )
                        .width(Fill),
                        custom(
                            "mark-all-read",
                            "Mark all read",
                            text("Mark all read").size(12).color(c.accent),
                            self.connected.is_ok().then_some(Message::MarkAllRead),
                            false,
                            Kind::Quiet,
                            [2, 4],
                        ),
                    ]
                    .spacing(6)
                    .align_y(Center),
                )
                .padding([2, 10]),
            );
        }
        list = list.push(container(eyebrow("Channels", c)).padding(iced::Padding {
            top: 10.0,
            right: 10.0,
            bottom: 4.0,
            left: 10.0,
        }));
        if channels.is_empty() {
            list = list.push(container(note("No channels yet.", c)).padding([2, 10]));
        }
        for summary in channels {
            list = list.push(self.conversation_row(summary, None, c));
        }
        if !collisions.is_empty() {
            let open = self.shell.collisions_open;
            list = list.push(Self::group_toggle(
                "collisions-toggle",
                format!("Collisions ({})", collisions.len()),
                open,
                Message::ToggleCollisions,
                c,
            ));
            if open {
                for summary in collisions {
                    list = list.push(self.conversation_row(summary, None, c));
                }
            }
        }
        list = list.push(
            container(eyebrow("Direct messages", c)).padding(iced::Padding {
                top: 12.0,
                right: 10.0,
                bottom: 4.0,
                left: 10.0,
            }),
        );
        if direct.is_empty() {
            list = list.push(container(note("No agent is running.", c)).padding([2, 10]));
        }
        for summary in direct {
            list = list.push(self.conversation_row(summary, Some(true), c));
        }
        for summary in notices {
            list = list.push(self.conversation_row(summary, None, c));
        }
        if !peers.is_empty() {
            let open = self.shell.peers_open;
            list = list.push(Self::group_toggle(
                "peers-toggle",
                format!("Between agents ({})", peers.len()),
                open,
                Message::TogglePeers,
                c,
            ));
            if open {
                for summary in peers {
                    list = list.push(self.conversation_row(summary, None, c));
                }
            }
        }
        if !earlier.is_empty() {
            let open = self.shell.earlier_open;
            list = list.push(Self::group_toggle(
                "earlier-toggle",
                format!("Earlier ({})", earlier.len()),
                open,
                Message::ToggleEarlier,
                c,
            ));
            if open {
                for summary in earlier {
                    list = list.push(self.conversation_row(summary, Some(false), c));
                }
            }
        }
        container(scrollable(list).height(Fill).id("conversations-scroll"))
            .padding(iced::Padding {
                top: 0.0,
                right: 8.0,
                bottom: 0.0,
                left: 0.0,
            })
            .height(Fill)
            .into()
    }

    fn day_label(at: chrono::DateTime<Utc>) -> String {
        let local = at.with_timezone(&chrono::Local);
        let today = chrono::Local::now().date_naive();
        let day = local.date_naive();
        if day == today {
            "Today".to_owned()
        } else if day == today - chrono::Duration::days(1) {
            "Yesterday".to_owned()
        } else {
            local.format("%A, %B %-d").to_string()
        }
    }

    fn divider<'a>(label: String, tint: iced::Color, c: Colors) -> Element<'a, Message> {
        row![
            container(Space::new().width(Fill).height(1)).style(move |_| c.rule()),
            text(label).size(11).color(tint),
            container(Space::new().width(Fill).height(1)).style(move |_| c.rule()),
        ]
        .spacing(10)
        .align_y(Center)
        .into()
    }

    /// One archived message: the sender's mark and name when the sender
    /// changes, the time, the kind when it is not plain talk, the text,
    /// and the thread under it when there is one.
    fn archived_message(
        &self,
        message: &ArchivedMessage,
        show_sender: bool,
        in_thread: bool,
        mention_names: &[String],
        c: Colors,
    ) -> Element<'_, Message> {
        let from = message.envelope.from.as_str();
        let name = if self.is_human(from) {
            "You".to_owned()
        } else if from == "agentd" {
            "AgentDocker".to_owned()
        } else {
            self.name_of(from)
        };
        let body_text = line_of(&message.envelope);
        let mentions_me = agentdocker_core::conversation::mentions_any(&body_text, mention_names);
        let id = message.envelope.id.clone();
        let expanded = self.shell.message_detail.as_ref() == Some(&id);
        let long = body_text.chars().count() > 600 || body_text.lines().count() > 10;
        let shown: String = if long && !expanded {
            let head: String = body_text.lines().take(10).collect::<Vec<_>>().join("\n");
            let head: String = head.chars().take(600).collect();
            format!("{head}…")
        } else {
            body_text
        };
        let mut head = row![].spacing(8).align_y(Center);
        if show_sender {
            head = head.push(
                text(name.clone())
                    .size(13)
                    .font(weight(iced::font::Weight::Semibold)),
            );
        }
        let kind = message.envelope.kind.as_str();
        if kind != "chat" && kind != "message" {
            head = head.push(pill(kind.to_owned(), c.raised, c.muted, c));
        }
        // A message that names the person says so where the eye lands.
        if mentions_me {
            head = head.push(pill("mentions you", c.accent, iced::Color::WHITE, c));
        }
        head = head.push(small(
            message
                .envelope
                .sent_at
                .with_timezone(&chrono::Local)
                .format("%H:%M")
                .to_string(),
            c,
        ));
        let mut body = column![head, text(shown).size(14)].spacing(3).width(Fill);
        if long {
            body = body.push(action(
                format!("message-detail-{id}"),
                if expanded { "Show less" } else { "Show more" },
                Some(Message::ExpandArchived(id.clone())),
                false,
            ));
        }
        if !in_thread {
            let replies = message.replies;
            let open = self.shell.thread.as_ref() == Some(&id);
            let label = match replies {
                0 => "Reply".to_owned(),
                1 => "1 reply".to_owned(),
                n => format!("{n} replies"),
            };
            // A quiet link under the words, not a button beside them.
            body = body.push(
                row![custom(
                    format!("thread-{id}"),
                    label.clone(),
                    text(label).size(12).color(c.accent),
                    Some(if open {
                        Message::CloseThread
                    } else {
                        Message::OpenThread(id.clone())
                    }),
                    open,
                    Kind::Quiet,
                    [2, 4],
                )]
                .spacing(6),
            );
        }
        let mark: Element<'_, Message> = if show_sender {
            monogram(&name, from, 28.0, c)
        } else {
            Space::new().width(28.0).height(1.0).into()
        };
        container(row![mark, body].spacing(10))
            .padding([4, 6])
            .width(Fill)
            .into()
    }

    /// Resolve only the person's direct recipient, never a broadcast or a
    /// read-only conversation between two agents.
    fn direct_input_recipient(&self, conversation: &str) -> Option<&AgentRecord> {
        let conversation = agentdocker_core::ConversationId::from(conversation.to_owned());
        let (one, other) = conversation.dm_parties()?;
        let recipient = if self.is_human(one) {
            other
        } else if self.is_human(other) {
            one
        } else {
            return None;
        };
        // Resolve retired IDs through the current alias map, just as sends do.
        let recipient = self.canonical_agent(recipient);
        self.agents
            .iter()
            .find(|agent| agent.id.as_str() == recipient)
    }

    /// Both composers and queued submit events obey the same current state.
    /// Retired IDs may resolve to a live session; a missing DM record cannot.
    pub(super) fn conversation_can_send(&self, conversation: &str) -> bool {
        let Some(destination) = self.conversation_destination(conversation) else {
            return false;
        };
        agentdocker_core::ConversationId::from(conversation)
            .dm_parties()
            .is_none()
            || self.agent_live(&destination)
    }

    /// The composer under a conversation or a thread: the draft, its
    /// receipt or error, and Send. Enter sends.
    /// The composer of a conversation, or of a thread in it when `root` is
    /// given: each keeps its own draft, and only the thread's sets `reply_to`.
    fn composer(
        &self,
        conversation: &str,
        root: Option<&MessageId>,
        placeholder: String,
        can_send: bool,
        c: Colors,
    ) -> Element<'_, Message> {
        let key = super::draft_key(conversation, root);
        let draft = self.shell.conversation_drafts.get(&key);
        let text_now = draft.map(|d| d.text.clone()).unwrap_or_default();
        let sending = draft.is_some_and(|d| d.sending.is_some());
        let ready = can_send && self.connected.is_ok() && !sending && !text_now.trim().is_empty();
        let owner = key.clone();
        let submit = ready.then(|| Message::SendConversation(key.clone()));
        // A direct conversation's composer keeps the inbox's `reply-<agent>`
        // id, so what drove the inbox drives it.
        let input_id = match root {
            Some(root) => format!("reply-thread-{root}"),
            None => agentdocker_core::ConversationId::from(conversation.to_owned())
                .dm_parties()
                .map(|(a, b)| if self.is_human(a) { b } else { a })
                .map(|agent| format!("reply-{agent}"))
                .unwrap_or_else(|| format!("compose-{conversation}")),
        };
        // Enter sends, as it does everywhere people type to each other;
        // the button beside it is the same action for the pointer.
        let mut composer = column![
            row![
                input_submitting(
                    input_id,
                    &placeholder,
                    &text_now,
                    move |t| Message::ConversationDraft(owner.clone(), t),
                    can_send && !sending,
                    submit.clone(),
                ),
                primary(
                    format!("send-{key}"),
                    if sending { "Sending…" } else { "Send" },
                    submit,
                )
            ]
            .spacing(8)
            .align_y(Center)
        ]
        .spacing(4);
        // Mention suggestions include only the conversation's recipients.
        // Inserting a name does not change the Send destination or membership.
        if let Some(prefix) = mention_prefix(&text_now) {
            let matches: Vec<&AgentRecord> = self
                .mention_recipients(conversation)
                .into_iter()
                .filter(|a| {
                    a.spec
                        .name
                        .to_lowercase()
                        .starts_with(&prefix.to_lowercase())
                        || self
                            .name_of(a.id.as_str())
                            .to_lowercase()
                            .starts_with(&prefix.to_lowercase())
                })
                .take(6)
                .collect();
            if !matches.is_empty() {
                let mut strip = row![small("Mention", c)].spacing(6).align_y(Center);
                for agent in matches {
                    let id = agent.id.as_str();
                    let completed = complete_mention(&text_now, &agent.spec.name);
                    let owner = key.clone();
                    strip = strip.push(custom(
                        format!("mention-{id}"),
                        format!("@{}", agent.spec.name),
                        row![
                            text(format!("@{}", agent.spec.name))
                                .size(13)
                                .color(c.accent),
                            small(self.name_of(id), c),
                        ]
                        .spacing(6)
                        .align_y(Center),
                        Some(Message::ConversationDraft(owner, completed)),
                        false,
                        Kind::Quiet,
                        [3, 8],
                    ));
                }
                composer = composer.push(strip);
            }
        }
        if let Some(error) = draft.and_then(|d| d.error.as_ref()) {
            composer = composer.push(text(error.clone()).size(13).color(c.amber));
        }
        // Put the receiver state where a person is about to send, including
        // thread replies. A working MCP/hook transport alone cannot wake it.
        if let Some(agent) = self.direct_input_recipient(conversation) {
            let status = self.input_readiness(agent);
            let mut readiness = row![small(status, c).width(Fill)]
                .spacing(6)
                .align_y(Center);
            if self
                .runtimes
                .iter()
                .any(|runtime| runtime.name == agent.spec.runtime)
            {
                readiness = readiness.push(action(
                    format!("input-connection-{key}"),
                    "Connection",
                    Some(Message::OpenConnection(agent.spec.runtime.clone())),
                    false,
                ));
            }
            composer = composer.push(readiness);
        }
        composer.into()
    }

    /// The live agents the person can talk to, in the project the sidebar
    /// is scoped to when it is: who a direct message can go to, who a
    /// channel can hold.
    fn agents_to_talk_to(&self) -> Vec<&AgentRecord> {
        let mut agents: Vec<&AgentRecord> = self
            .agents
            .iter()
            .filter(|a| a.status.is_live() && !self.is_human(a.id.as_str()))
            .filter(|a| self.has_project(a.project.as_ref()))
            .collect();
        agents.sort_by_key(|a| self.name_of(a.id.as_str()).to_lowercase());
        agents
    }

    fn mention_recipients(&self, conversation: &str) -> Vec<&AgentRecord> {
        if let Some(agent) = self.direct_input_recipient(conversation) {
            return agent
                .status
                .is_live()
                .then_some(agent)
                .into_iter()
                .collect();
        }
        let Some(summary) = self
            .conversations
            .iter()
            .find(|s| s.conversation.as_str() == conversation)
        else {
            return Vec::new();
        };
        self.agents_to_talk_to()
            .into_iter()
            .filter(|agent| {
                summary
                    .members
                    .iter()
                    .any(|id| self.canonical_agent(id.as_str()) == agent.id.as_str())
            })
            .collect()
    }

    /// Starting a conversation, the way Slack's New message does: a direct
    /// message is one pick from the agents here; a channel is a name,
    /// what it is for, and who is in it — everyone here when nobody is
    /// picked — and the person is in it as its opener.
    fn new_conversation_form(
        &self,
        form: &super::NewConversation,
        c: Colors,
    ) -> Element<'_, Message> {
        use super::NewKind;
        if let Some(channel) = &form.invite {
            return self.invite_members_form(form, channel, c);
        }
        let agents = self.agents_to_talk_to();
        let human = self
            .agents
            .iter()
            .find(|a| self.is_human(a.id.as_str()))
            .map(|a| a.id.as_str().to_owned());
        let mut body = column![
            row![
                segment(
                    "new-kind-direct",
                    "Direct message",
                    Some(Message::NewConversationKind(NewKind::Direct)),
                    form.kind == NewKind::Direct,
                ),
                segment(
                    "new-kind-channel",
                    "Channel",
                    Some(Message::NewConversationKind(NewKind::Channel)),
                    form.kind == NewKind::Channel,
                ),
            ]
            .spacing(4)
        ]
        .spacing(8);
        match form.kind {
            NewKind::Direct => {
                if agents.is_empty() {
                    body = body.push(note("No agent is running here to message.", c));
                }
                for agent in agents {
                    let id = agent.id.as_str();
                    let conversation = human.as_deref().map(|me| {
                        agentdocker_core::ConversationId::dm(me, id)
                            .as_str()
                            .to_owned()
                    });
                    body = body.push(custom(
                        format!("new-direct-{id}"),
                        self.name_of(id),
                        row![
                            monogram(&self.name_of(id), id, 24.0, c),
                            column![
                                text(self.name_of(id)).size(14).color(c.text),
                                small(self.tool_of(id), c),
                            ]
                            .spacing(1),
                        ]
                        .spacing(10)
                        .align_y(Center),
                        conversation.map(Message::NewDirect),
                        false,
                        Kind::Quiet,
                        [6, 8],
                    ));
                }
            }
            NewKind::Channel => {
                let ready = self.connected.is_ok() && !form.creating && !form.name.is_empty();
                let create = ready.then_some(Message::CreateChannel);
                body = body.push(input_submitting(
                    "new-channel-name",
                    "Name, like planning",
                    &form.name,
                    Message::NewChannelName,
                    !form.creating,
                    create.clone(),
                ));
                body = body.push(input_submitting(
                    "new-channel-purpose",
                    "What it is for",
                    &form.purpose,
                    Message::NewChannelPurpose,
                    !form.creating,
                    create.clone(),
                ));
                body = body.push(small(
                    if form.members.is_empty() {
                        "Members: everyone here. Pick some to narrow it.".to_owned()
                    } else {
                        format!("Members: you and {}", form.members.len())
                    },
                    c,
                ));
                for agent in agents {
                    let id = agent.id.as_str();
                    let picked = form.members.contains(&agent.id);
                    body = body.push(custom(
                        format!("new-member-{id}"),
                        self.name_of(id),
                        row![
                            text(if picked { "☑" } else { "☐" })
                                .size(14)
                                .color(if picked { c.accent } else { c.muted }),
                            text(self.name_of(id)).size(14).color(c.text),
                            small(self.tool_of(id), c),
                        ]
                        .spacing(8)
                        .align_y(Center),
                        (!form.creating).then_some(Message::NewChannelMember(agent.id.clone())),
                        picked,
                        Kind::Quiet,
                        [4, 8],
                    ));
                }
                body = body.push(primary(
                    "new-channel-create",
                    if form.creating {
                        "Opening…"
                    } else {
                        "Create channel"
                    },
                    create,
                ));
            }
        }
        if let Some(error) = &form.error {
            body = body.push(text(error.clone()).size(13).color(c.amber));
        }
        panel(body, c)
    }

    fn invite_members_form(
        &self,
        form: &super::NewConversation,
        channel: &str,
        c: Colors,
    ) -> Element<'_, Message> {
        let summary = self.conversations.iter().find(|s| {
            s.conversation
                .channel_id()
                .is_some_and(|id| id.as_str() == channel)
        });
        let Some(summary) = summary.filter(|s| {
            self.channels
                .iter()
                .any(|item| item.id.as_str() == channel && item.is_open())
                && s.members.iter().any(|id| self.is_human(id.as_str()))
        }) else {
            return panel(
                note("This channel is no longer available to add members.", c),
                c,
            );
        };
        let agents: Vec<_> = self
            .agents_to_talk_to()
            .into_iter()
            .filter(|agent| {
                !summary.members.contains(&agent.id) && !form.members.contains(&agent.id)
            })
            .collect();
        let mut body = column![
            text(format!("Add to {}", self.conversation_label(summary)))
                .size(15)
                .color(c.text)
        ]
        .spacing(8);
        if agents.is_empty() {
            body = body.push(note("All available agents are already members.", c));
        }
        for agent in agents {
            body = body.push(action(
                format!("invite-member-{}", agent.id),
                format!("Add {}", self.name_of(agent.id.as_str())),
                (!form.creating && self.connected.is_ok())
                    .then(|| Message::InviteMember(agent.id.to_string())),
                false,
            ));
        }
        if form.creating {
            body = body.push(note("Adding member…", c));
        }
        if let Some(error) = &form.error {
            body = body.push(text(error.clone()).size(13).color(c.amber));
        }
        panel(body, c)
    }

    fn messages_pane(&self, c: Colors) -> Element<'_, Message> {
        let mention_names = self.mention_names();
        let Some(summary) = self.open_summary() else {
            return empty(
                "Pick a conversation",
                "Channels, direct messages with your agents, and what AgentDocker told them are on the left.",
                None,
                c,
            );
        };
        let label = self.conversation_label(&summary);
        let key = summary.conversation.as_str().to_owned();
        // The name on one line, what the room is about on the next: a
        // channel's task or contested paths, a pair's branches, a
        // broadcast's members. Neither repeats the other.
        let members = summary.members.len();
        let plural = |n: usize, word: &str| format!("{n} {word}{}", if n == 1 { "" } else { "s" });
        let topic = match summary.kind {
            ConversationKind::Everyone => {
                format!("{} · {}", summary.title, plural(members, "live agent"))
            }
            ConversationKind::All => "every agent on this machine".to_owned(),
            ConversationKind::Channel => {
                format!("{} · {}", summary.title, plural(members, "member"))
            }
            ConversationKind::Collision => {
                format!(
                    "contested: {} · {}",
                    summary.title,
                    plural(members, "member")
                )
            }
            ConversationKind::Dm => match self.counterpart(&summary) {
                Some(id) if self.agent_live(id) => "direct message".to_owned(),
                Some(_) => "direct message · this session has ended".to_owned(),
                None => match summary.conversation.dm_parties() {
                    Some((a, b)) => format!(
                        "{} and {} · between two agents, read only",
                        self.name_of(a),
                        self.name_of(b)
                    ),
                    None => "between two agents · read only".to_owned(),
                },
            },
            ConversationKind::Notices => format!(
                "what AgentDocker told {}",
                summary
                    .conversation
                    .notices_agent()
                    .map(|a| self.name_of(a.as_str()))
                    .unwrap_or_else(|| "this agent".to_owned())
            ),
        };
        let mut title_row = row![
            container(
                text(label.clone())
                    .size(18)
                    .font(weight(iced::font::Weight::Semibold))
                    .wrapping(iced::widget::text::Wrapping::None)
            )
            .width(Fill)
            .clip(true),
        ]
        .spacing(10)
        .align_y(Center);
        if matches!(
            summary.kind,
            ConversationKind::Channel | ConversationKind::Collision
        ) {
            title_row = title_row.push(action(
                "open-channel-tools",
                "Reviews",
                Some(Message::Navigate(Screen::Channels)),
                false,
            ));
        }
        if summary.kind == ConversationKind::Channel
            && summary.members.iter().any(|id| self.is_human(id.as_str()))
            && let Some(channel) = summary.conversation.channel_id()
            && self
                .channels
                .iter()
                .any(|item| &item.id == channel && item.is_open())
        {
            title_row = title_row.push(action(
                "invite-channel",
                "Add members",
                Some(Message::InviteChannel(channel.to_string())),
                false,
            ));
        }
        let header = column![
            title_row,
            container(
                text(topic)
                    .size(12)
                    .color(c.muted)
                    .wrapping(iced::widget::text::Wrapping::None)
            )
            .width(Fill)
            .clip(true),
        ]
        .spacing(2);

        let history = self.history.get(&key);
        let mut list = column![].spacing(2).width(Fill);
        if history.is_some_and(|m| !m.is_empty()) && !self.history_complete.contains(&key) {
            list = list.push(
                container(custom(
                    format!("earlier-{key}"),
                    "Show earlier messages",
                    text("Show earlier messages").size(12).color(c.accent),
                    Some(Message::EarlierHistory(key.clone())),
                    false,
                    Kind::Quiet,
                    [4, 8],
                ))
                .center_x(Fill),
            );
        }
        match history {
            None => list = list.push(note("Loading…", c)),
            Some(messages) if messages.is_empty() => {
                list = list.push(note("Nothing said here yet.", c));
            }
            Some(messages) => {
                // The unread divider sits before the newest `unread` messages
                // that are not the person's own words.
                let unread_from = if summary.unread > 0 {
                    let mut remaining = summary.unread;
                    let mut index = messages.len();
                    for (i, m) in messages.iter().enumerate().rev() {
                        if !self.is_human(&m.envelope.from) {
                            remaining -= 1;
                            index = i;
                            if remaining == 0 {
                                break;
                            }
                        }
                    }
                    Some(index)
                } else {
                    None
                };
                let mut last_day: Option<chrono::NaiveDate> = None;
                let mut last_from: Option<&str> = None;
                for (i, message) in messages.iter().enumerate() {
                    let day = message
                        .envelope
                        .sent_at
                        .with_timezone(&chrono::Local)
                        .date_naive();
                    if last_day != Some(day) {
                        list = list.push(Self::divider(
                            Self::day_label(message.envelope.sent_at),
                            c.muted,
                            c,
                        ));
                        last_day = Some(day);
                        last_from = None;
                    }
                    if unread_from == Some(i) {
                        list = list.push(Self::divider("New".to_owned(), c.accent, c));
                        last_from = None;
                    }
                    let show_sender = last_from != Some(message.envelope.from.as_str());
                    last_from = Some(message.envelope.from.as_str());
                    // A question keeps its card, since the card carries the
                    // controls; the archive row says where it sits.
                    if message.envelope.kind == "question"
                        && let Some(question) =
                            self.questions.iter().find(|q| q.id == message.envelope.id)
                    {
                        list = list.push(self.question_card(question, c));
                        continue;
                    }
                    list = list.push(self.archived_message(
                        message,
                        show_sender,
                        false,
                        &mention_names,
                        c,
                    ));
                }
            }
        }
        let destination = self.conversation_destination(&key);
        let can_send = self.conversation_can_send(&key);
        let placeholder = match summary.kind {
            ConversationKind::Notices => "AgentDocker's notices; nothing to reply to".to_owned(),
            ConversationKind::Dm if destination.is_none() => {
                "A conversation between two agents; you are not in it".to_owned()
            }
            ConversationKind::Dm if !can_send => "This session has ended".to_owned(),
            _ => format!("Message {label}"),
        };
        // The conversation's own composer, whatever thread is open beside
        // it: the thread has one of its own.
        let composer = self.composer(&key, None, placeholder, can_send, c);
        column![
            header,
            rule(c),
            container(
                scrollable(list)
                    .height(Fill)
                    .anchor_bottom()
                    .id(format!("history-{key}"))
            )
            .height(Fill)
            .padding([6, 0]),
            rule(c),
            container(composer).padding(iced::Padding {
                top: 8.0,
                right: 0.0,
                bottom: 0.0,
                left: 0.0
            }),
        ]
        .spacing(6)
        .height(Fill)
        .into()
    }

    fn thread_pane(&self, c: Colors) -> Element<'_, Message> {
        let mention_names = self.mention_names();
        let Some(root_id) = self.shell.thread.as_ref() else {
            return Space::new().into();
        };
        let mut header = row![
            text("Thread")
                .size(15)
                .font(weight(iced::font::Weight::Semibold))
                .width(Fill),
        ]
        .spacing(8)
        .align_y(Center);
        // Narrow, the way back above the pane closes it; one control, one id.
        if !self.messages_compact() {
            header = header.push(action(
                "close-thread",
                "Close",
                Some(Message::CloseThread),
                false,
            ));
        }
        let mut list = column![].spacing(4).width(Fill);
        match &self.thread {
            Some((root, replies)) if root.envelope.id == *root_id => {
                list = list.push(self.archived_message(root, true, true, &mention_names, c));
                list = list.push(Self::divider(
                    match replies.len() {
                        0 => "No replies yet".to_owned(),
                        1 => "1 reply".to_owned(),
                        n => format!("{n} replies"),
                    },
                    c.muted,
                    c,
                ));
                let mut last_from: Option<&str> = None;
                for reply in replies {
                    let show = last_from != Some(reply.envelope.from.as_str());
                    last_from = Some(reply.envelope.from.as_str());
                    list = list.push(self.archived_message(reply, show, true, &mention_names, c));
                }
            }
            _ => list = list.push(note("Loading…", c)),
        }
        let key = self.shell.conversation.clone().unwrap_or_default();
        let can_send = self.conversation_can_send(&key);
        let placeholder = if can_send {
            "Reply in thread"
        } else if self.conversation_destination(&key).is_some() {
            "This session has ended"
        } else {
            "This conversation is read-only"
        };
        column![
            header,
            rule(c),
            container(scrollable(list).height(Fill).anchor_bottom())
                .height(Fill)
                .padding([6, 0]),
            rule(c),
            container(self.composer(&key, Some(root_id), placeholder.to_owned(), can_send, c))
                .padding(iced::Padding {
                    top: 8.0,
                    right: 0.0,
                    bottom: 0.0,
                    left: 0.0
                }),
        ]
        .spacing(6)
        .height(Fill)
        .into()
    }

    /// The conversations screen's own panel around the sidebar in a narrow
    /// window, so it reads as a list rather than loose rows.
    #[allow(dead_code)]
    fn boxed<'a>(&self, content: Element<'a, Message>, c: Colors) -> Element<'a, Message> {
        panel(content, c)
    }

    /// The time a conversation last moved, for tests and the rail.
    #[allow(dead_code)]
    fn last_moved(summary: &ConversationSummary) -> Option<String> {
        summary.last_at.map(|at| ago(Utc::now(), at))
    }
}

/// The name being typed after a trailing `@`, when the draft ends in one:
/// `ask @co` gives `co`, `ask @` gives an empty prefix (everyone), and a
/// draft whose last word is not a mention gives nothing.
fn mention_prefix(draft: &str) -> Option<&str> {
    let last = draft.rsplit(char::is_whitespace).next().unwrap_or("");
    let name = last.strip_prefix('@')?;
    name.chars()
        .all(|ch| ch.is_alphanumeric() || matches!(ch, '-' | '_' | '.'))
        .then_some(name)
}

/// The draft with the mention being typed finished as `@name `.
fn complete_mention(draft: &str, name: &str) -> String {
    let cut = draft.rfind('@').expect("a prefix was found");
    format!("{}@{name} ", &draft[..cut])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{MESSAGE_CAPACITY, queue, tests::record};

    /// `@` at the end of a word being typed offers names; a pick finishes
    /// it with a space after, leaving the words before it as they were.
    #[test]
    fn a_mention_is_offered_while_typed_and_finished_when_picked() {
        assert_eq!(mention_prefix("ask @co"), Some("co"));
        assert_eq!(mention_prefix("ask @"), Some(""));
        assert_eq!(mention_prefix("ask @codex-51242 to"), None);
        assert_eq!(mention_prefix("mail a@b"), None);
        assert_eq!(mention_prefix(""), None);
        assert_eq!(
            complete_mention("ask @co", "codex-51242"),
            "ask @codex-51242 "
        );
        assert_eq!(complete_mention("@", "user"), "@user ");
    }
    use std::sync::mpsc::sync_channel;

    /// The badge counts what the person owes: a channel, a broadcast and
    /// their own direct messages. A collision room's contested-path notices,
    /// a pair of agents' words and the daemon's notices to an agent are
    /// read without being owed.
    #[test]
    fn the_badge_counts_only_what_the_person_owes() {
        let (commands, _requests) = queue::channel();
        let (_messages, results) = sync_channel(MESSAGE_CAPACITY);
        let mut app = App::bare(commands, results);
        let mut human = record("user", agentdocker_core::HUMAN_RUNTIME, None);
        human.id = AgentId::from("human-id");
        let mut agent = record("codex-1", "codex", Some(1));
        agent.id = AgentId::from("agent-a");
        let mut other = record("codex-2", "codex", Some(2));
        other.id = AgentId::from("agent-b");
        app.agents = vec![human, agent, other];
        let summary = |conversation: &str, kind: &str, unread: u64| -> ConversationSummary {
            serde_json::from_value(serde_json::json!({
                "conversation": conversation, "kind": kind, "title": "",
                "members": [], "unread": unread,
            }))
            .unwrap()
        };
        app.conversations = vec![
            summary("everyone:project", "everyone", 3),
            summary("all", "all", 1),
            summary("channel:named", "channel", 11),
            summary("channel:contested", "collision", 218),
            summary(
                agentdocker_core::ConversationId::dm("human-id", "agent-a").as_str(),
                "dm",
                2,
            ),
            summary(
                agentdocker_core::ConversationId::dm("agent-a", "agent-b").as_str(),
                "dm",
                58,
            ),
            summary("notices:agent-a", "notices", 18),
        ];
        assert_eq!(app.unread_total(), 3 + 1 + 11 + 2);
        let owed: Vec<&str> = app
            .conversations
            .iter()
            .filter(|c| app.counts_for_person(c))
            .map(|c| c.conversation.as_str())
            .collect();
        assert_eq!(
            owed,
            [
                "everyone:project",
                "all",
                "channel:named",
                "dm:agent-a:human-id"
            ],
            "collision rooms, peers and notices are read, not owed"
        );
        // The room's own row still says what is unread in it.
        let contested = app
            .conversations
            .iter()
            .find(|c| c.kind == ConversationKind::Collision)
            .unwrap();
        assert!(!app.counts_for_person(contested));
        assert_eq!(app.row_unread(contested), 218);
    }

    #[test]
    fn mention_suggestions_include_only_live_conversation_recipients() {
        let (commands, _requests) = queue::channel();
        let (_messages, receiver) = sync_channel(MESSAGE_CAPACITY);
        let mut app = App::bare(commands, receiver);
        for id in ["member", "outsider"] {
            let mut agent =
                AgentRecord::new(agentdocker_core::AgentSpec::default(), false, Utc::now());
            agent.id = id.into();
            app.agents.push(agent);
        }
        let summary = serde_json::from_value(serde_json::json!({
            "conversation":"channel:room", "kind":"channel", "title":"room", "members":["member"],
            "unread":0, "mentions":0, "open":true
        }))
        .unwrap();
        app.conversations.push(summary);
        app.aliases.insert("retired".into(), "member".into());
        for conversation in ["channel:room", "dm:user:member", "dm:user:retired"] {
            let ids: Vec<&str> = app
                .mention_recipients(conversation)
                .iter()
                .map(|a| a.id.as_str())
                .collect();
            assert_eq!(ids, ["member"], "{conversation}");
        }
        assert!(app.mention_recipients("channel:missing").is_empty());
        app.agents[0].status = agentdocker_core::AgentStatus::Exited { code: Some(0) };
        assert!(app.mention_recipients("channel:room").is_empty());
        assert!(app.mention_recipients("dm:user:member").is_empty());
    }

    #[test]
    fn direct_input_status_uses_the_current_recipient_only() {
        let (commands, _requests) = queue::channel();
        let (_messages, receiver) = sync_channel(MESSAGE_CAPACITY);
        let mut app = App::bare(commands, receiver);
        let mut agent = AgentRecord::new(
            agentdocker_core::AgentSpec {
                runtime: "claude-code".into(),
                ..Default::default()
            },
            false,
            Utc::now(),
        );
        agent.id = "worker".into();
        app.agents.push(agent);
        app.aliases.insert("retired".into(), "worker".into());
        assert!(app.agent_live("retired"));
        assert!(!app.agent_live("missing"));
        for conversation in ["dm:user:worker", "dm:worker:user", "dm:retired:user"] {
            assert_eq!(
                app.direct_input_recipient(conversation)
                    .unwrap()
                    .id
                    .as_str(),
                "worker"
            );
        }
        for conversation in ["dm:worker:peer", "channel:room", "all", "dm:user:missing"] {
            assert!(
                app.direct_input_recipient(conversation).is_none(),
                "{conversation}"
            );
        }
        app.agents[0].status = agentdocker_core::AgentStatus::Exited { code: Some(0) };
        assert!(!app.agent_live("retired"));
    }

    #[test]
    fn ended_direct_threads_cannot_send_and_keep_their_drafts() {
        let (commands, requests) = queue::channel();
        let (_messages, receiver) = sync_channel(MESSAGE_CAPACITY);
        let mut app = App::bare(commands, receiver);
        app.connected = Ok(());
        let mut agent = AgentRecord::new(Default::default(), false, Utc::now());
        agent.id = "worker".into();
        app.agents.push(agent);
        app.aliases.insert("retired".into(), "worker".into());
        let conversation = "dm:retired:user";
        let root = MessageId::from("root".to_owned());
        let thread = crate::app::draft_key(conversation, Some(&root));
        let keys = [conversation.to_owned(), thread.clone()];
        assert!(app.conversation_can_send(conversation));
        assert!(!app.conversation_can_send("dm:missing:user"));
        assert!(!app.conversation_can_send("dm:peer:worker"));
        assert!(app.conversation_can_send("channel:room"));
        app.agents[0].status = agentdocker_core::AgentStatus::Exited { code: Some(0) };
        assert!(!app.conversation_can_send(conversation));
        for key in &keys {
            let _ = app.update(Message::ConversationDraft(key.clone(), "kept draft".into()));
            // A submit already queued before the status update is refused too.
            let _ = app.update(Message::SendConversation(key.clone()));
            let draft = &app.shell.conversation_drafts[key];
            assert_eq!(draft.text, "kept draft");
            assert!(draft.sending.is_none());
        }
        assert!(
            !requests
                .try_iter()
                .any(|cmd| matches!(cmd, Cmd::ConversationSend { .. }))
        );
        app.agents[0].status = agentdocker_core::AgentStatus::Running;
        assert!(app.conversation_can_send(conversation));
        let _ = app.update(Message::SendConversation(thread.clone()));
        assert!(requests.try_iter().any(|cmd| matches!(cmd,
            Cmd::ConversationSend { draft, reply_to: Some(parent), .. }
            if draft == thread && parent == root)));
        assert_eq!(
            app.shell.conversation_drafts[conversation].text,
            "kept draft"
        );
    }

    #[test]
    fn fallback_room_names_keep_unicode_boundaries() {
        for (id, expected) in [
            ("a", "room-a"),
            ("abcdefg", "room-abcdef"),
            ("abcde💬z", "room-abcde💬"),
            ("💬📦🌍1234", "room-💬📦🌍123"),
        ] {
            let summary: ConversationSummary = serde_json::from_value(serde_json::json!({
                "conversation": format!("channel:{id}"), "kind": "channel", "title": "",
                "members": [], "unread": 0,
            }))
            .unwrap();
            assert_eq!(App::short_room_name(&summary), expected);
        }
    }
}
