//! Messages as a workspace: a sidebar of channels and direct conversations
//! with what is unread, one pane that always has a composer, and a thread
//! beside it. The shape people know from Slack and Discord, over the
//! daemon's conversations: the archive is what is shown, the queues stay
//! the agents' own.
use super::style::{Colors, weight};
use super::view::{dot, empty, eyebrow, first_line, monogram, note, panel, pill, rule, small};
use super::*;
use crate::controls::{Kind, button as action, custom, input_enabled, primary};
use agentdocker_core::conversation::line_of;
use agentdocker_core::journal::ago;
use agentdocker_core::{
    AgentId, ArchivedMessage, ConversationKind, ConversationSummary, MessageId,
};
use iced::{
    Center, Element, Fill,
    widget::{Space, column, container, row, scrollable, text},
};

/// The sidebar's width beside the pane, and the thread's.
const SIDEBAR: f32 = 272.0;
const THREAD: f32 = 340.0;

impl App {
    /// Whether the daemon lists conversations, so this screen can be shown
    /// instead of the inbox.
    pub(super) fn has_conversations(&self) -> bool {
        self.conversations_supported == Some(true)
    }

    /// Unread across the conversations that are the person's to answer:
    /// rooms, broadcasts and their own direct messages. What two agents say
    /// to each other and what AgentDocker told them is read here, not owed.
    pub(super) fn unread_total(&self) -> u64 {
        self.conversations
            .iter()
            .filter(|c| self.counts_for_person(c))
            .map(|c| c.unread)
            .sum()
    }

    pub(super) fn counts_for_person(&self, summary: &ConversationSummary) -> bool {
        match summary.kind {
            ConversationKind::Dm => self.counterpart(summary).is_some(),
            ConversationKind::Notices => false,
            _ => true,
        }
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

    pub(super) fn messages_view(&self, c: Colors) -> Element<'_, Message> {
        let narrow = self.narrow();
        let height = self.workspace_height();
        let sidebar = self.messages_sidebar(c);
        let pane = self.messages_pane(c);
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
                return column![back, container(pane).height(height)]
                    .spacing(12)
                    .into();
            }
            return container(sidebar).height(height).into();
        }
        let mut layout = row![
            container(sidebar).width(SIDEBAR).height(height),
            container(Space::new().width(1).height(height)).style(move |_| c.rule()),
            container(pane)
                .width(Fill)
                .height(height)
                .padding(iced::Padding {
                    top: 0.0,
                    right: 0.0,
                    bottom: 0.0,
                    left: 16.0,
                }),
        ]
        .spacing(0)
        .height(height);
        if self.shell.thread.is_some() {
            layout = layout
                .push(container(Space::new().width(1).height(height)).style(move |_| c.rule()));
            layout = layout.push(
                container(self.thread_pane(c))
                    .width(THREAD)
                    .height(height)
                    .padding(iced::Padding {
                        top: 0.0,
                        right: 0.0,
                        bottom: 0.0,
                        left: 12.0,
                    }),
            );
        }
        layout.into()
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
        // Unread is shown where it is the person's to answer; a room two
        // agents share or a notice is listed quietly with its last line.
        let unread = if self.counts_for_person(summary) {
            summary.unread
        } else {
            0
        };
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
        list = list.push(
            container(input_enabled(
                "messages-search",
                "Find a conversation…",
                &self.shell.messages_search,
                Message::MessagesSearch,
                true,
            ))
            .padding([2, 4]),
        );
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
        let mut composer = column![
            row![
                input_enabled(
                    input_id,
                    &placeholder,
                    &text_now,
                    move |t| Message::ConversationDraft(owner.clone(), t),
                    can_send && !sending,
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
        if let Some(error) = draft.and_then(|d| d.error.as_ref()) {
            composer = composer.push(text(error.clone()).size(13).color(c.amber));
        }
        composer.into()
    }

    fn messages_pane(&self, c: Colors) -> Element<'_, Message> {
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
                    list = list.push(self.archived_message(message, show_sender, false, c));
                }
            }
        }
        let destination = self.conversation_destination(&key);
        let can_send = destination.is_some()
            && match summary.kind {
                ConversationKind::Dm => self
                    .counterpart(&summary)
                    .is_some_and(|id| self.agent_live(id)),
                _ => true,
            };
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
        if !self.narrow() {
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
                list = list.push(self.archived_message(root, true, true, c));
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
                    list = list.push(self.archived_message(reply, show, true, c));
                }
            }
            _ => list = list.push(note("Loading…", c)),
        }
        let key = self.shell.conversation.clone().unwrap_or_default();
        let can_send = self.conversation_destination(&key).is_some();
        column![
            header,
            rule(c),
            container(scrollable(list).height(Fill).anchor_bottom())
                .height(Fill)
                .padding([6, 0]),
            rule(c),
            container(self.composer(
                &key,
                Some(root_id),
                "Reply in thread".to_owned(),
                can_send,
                c
            ))
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

#[cfg(test)]
mod tests {
    use super::*;

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
