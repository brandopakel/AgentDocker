//! Messages as a workspace: a sidebar of channels and direct conversations
//! with what is unread, one pane that always has a composer, and a thread
//! beside it. The shape people know from Slack and Discord, over the
//! daemon's conversations: the archive is what is shown, the queues stay
//! the agents' own.
use super::icons::{Icon, icon};
use super::panes::{Grid, Slot};
use super::style::{Colors, alpha, weight};
use super::view::{empty, first_line, monogram, note, panel, pill, rule, split_style};
use super::*;
use crate::controls::{
    Kind, custom, custom_sized, framed_composer, input_enabled, input_submitting, popover, primary,
};
use agentdocker_core::conversation::line_of;
use agentdocker_core::journal::ago;
use agentdocker_core::{
    AgentId, AgentRecord, ArchivedMessage, ConversationKind, ConversationSummary, MessageId,
};
use iced::{
    Center, Element, Fill,
    widget::{Space, column, container, responsive, row, scrollable, text},
};
use look::{
    channel_mark, count_divider, day_divider, disc, group_header, guided, key_hint, link,
    section_label, status_line, tinted, unread_divider, with_presence,
};

pub(super) mod look;

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

    /// The tool an agent is: `Codex`, `Claude Code`. From the record's
    /// runtime, so a chosen name still says what runs under it.
    fn tool_of(&self, id: &str) -> String {
        match self.naming().record(id) {
            Some(agent) => super::runtime_label(&agent.spec.runtime),
            None => self.name_of(id),
        }
    }

    /// How many paths a collision room is about: the ones its title lists
    /// and the `(+n more)` it ends with. None when the title is not a
    /// path list.
    fn contested_paths(summary: &ConversationSummary) -> Option<usize> {
        let title = summary.title.trim();
        if title.is_empty() {
            return None;
        }
        let (listed, more) = match title.rsplit_once(" (+") {
            Some((listed, rest)) => (
                listed,
                rest.strip_suffix(" more)")
                    .and_then(|n| n.parse::<usize>().ok())?,
            ),
            None => (title, 0),
        };
        let listed = listed.split(", ").filter(|p| !p.trim().is_empty()).count();
        (listed > 0).then_some(listed + more)
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

    pub(super) fn agent_live(&self, id: &str) -> bool {
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
            // A room opened before names is called by a short name made
            // from its task, never by the whole task; the header carries
            // the rest.
            ConversationKind::Channel => match &summary.name {
                Some(name) if !name.is_empty() => format!("#{name}"),
                _ => format!("#{}", Self::short_room_name(summary)),
            },
            // A collision room is about paths: say how many, never which,
            // in a list. The header lists them.
            ConversationKind::Collision => match &summary.name {
                Some(name) if !name.is_empty() => format!("#{name}"),
                _ => match Self::contested_paths(summary) {
                    Some(n) => format!("Contested paths ({n})"),
                    None => format!("#{}", Self::short_room_name(summary)),
                },
            },
            // A pair of agents reads as the two names, so two pairs of the
            // same tools are told apart; what each is on belongs under
            // the header, not in the list.
            ConversationKind::Dm => match self.counterpart(summary) {
                Some(id) => self.name_of(id),
                None => match summary.conversation.dm_parties() {
                    Some((a, b)) => format!("{} ↔ {}", self.name_of(a), self.name_of(b)),
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
            ConversationKind::Everyone => {
                let project = self.shell.catalog.selected()?;
                if conversation.everyone_project()?.as_str() != project.project.id().as_str() {
                    return None;
                }
                (
                    None,
                    project.name(),
                    self.agents
                        .iter()
                        .filter(|a| {
                            a.status.is_live()
                                && a.spec.runtime != agentdocker_core::HUMAN_RUNTIME
                                && a.project
                                    .as_ref()
                                    .is_some_and(|p| p.id() == project.project.id())
                        })
                        .map(|a| a.id.clone())
                        .collect(),
                )
            }
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

    pub(super) fn messages_compact(&self) -> bool {
        if self.screen == Screen::Chat {
            // Chat has a different pair of side panes from Messages. Reserve
            // the same usable composer width before showing them together.
            let side_width = 250.0
                + 18.0
                + if self.shell.thread.is_some() {
                    318.0
                } else {
                    0.0
                };
            return self.narrow() || self.panes.workspace_width() < side_width + 320.0;
        }
        self.narrow() || self.panes.compact_messages()
    }

    /// The way back from a pane that has the whole of a narrow window: a
    /// chevron and where it leads, quiet until pointed at.
    pub(super) fn back_control<'a>(
        id: &str,
        spoken: &str,
        words: &str,
        message: Message,
        c: Colors,
    ) -> Element<'a, Message> {
        custom(
            id.to_owned(),
            spoken.to_owned(),
            row![
                icon(Icon::ChevronLeft, c.muted, 14.0),
                text(words.to_owned())
                    .size(13)
                    .line_height(iced::Pixels(crate::controls::LABEL_LINE))
                    .font(weight(iced::font::Weight::Medium)),
            ]
            .spacing(6)
            .align_y(Center),
            Some(message),
            false,
            Kind::Ghost,
            [7, 10],
        )
    }

    /// Messages has the page below its header to itself (as Chat does),
    /// so its columns run to the window's foot.
    pub(super) fn messages_view(&self, c: Colors) -> Element<'_, Message> {
        let narrow = self.messages_compact();
        if narrow {
            if self.shell.inbox_open && self.shell.conversation.is_some() {
                // A thread takes the whole window too, with its own way back.
                if self.shell.thread.is_some() {
                    let back = Self::back_control(
                        "close-thread",
                        "Back to the conversation",
                        "Conversation",
                        Message::CloseThread,
                        c,
                    );
                    return column![back, container(self.thread_pane(c)).height(Fill)]
                        .spacing(6)
                        .height(Fill)
                        .into();
                }
                let back = Self::back_control(
                    "thread-back",
                    "Conversations",
                    "Conversations",
                    Message::InboxList,
                    c,
                );
                return column![back, container(self.messages_pane(c)).height(Fill)]
                    .spacing(6)
                    .height(Fill)
                    .into();
            }
            return container(self.messages_sidebar(c)).height(Fill).into();
        }
        // Wide: three columns with draggable dividers between them. The
        // widths are the person's, kept in pixels and remembered across
        // runs; the thread column comes and goes with the thread.
        let grid = iced::widget::pane_grid(&self.panes.messages, |_, slot, _| {
            // A hairline where a column begins, so the divider is seen at
            // rest as well as when it is hovered.
            let hairline =
                || container(Space::new().width(1).height(Fill)).style(move |_| c.rule());
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
                            left: 14.0,
                        })
                ]
                .height(Fill)
                .into(),
                _ => row![
                    hairline(),
                    container(self.messages_pane(c))
                        .width(Fill)
                        .padding(iced::Padding {
                            top: 0.0,
                            right: 2.0,
                            bottom: 0.0,
                            left: 14.0,
                        })
                ]
                .height(Fill)
                .into(),
            };
            iced::widget::pane_grid::Content::new(
                container(body).width(Fill).height(Fill).clip(true),
            )
        })
        .on_resize(8, |event| Message::PaneResized(Grid::Messages, event))
        .style(move |_| split_style(c))
        .height(Fill);
        grid.into()
    }

    /// One row of the sidebar: the mark (with presence for a direct
    /// conversation), the name over its last line, and what is unread as
    /// a quiet count — the `@` count in the accent, since that one asks
    /// for an answer.
    fn conversation_row(
        &self,
        summary: &ConversationSummary,
        presence: Option<bool>,
        width: f32,
        c: Colors,
    ) -> Element<'_, Message> {
        let label = self.conversation_label(summary);
        let selected = self.shell.conversation.as_deref() == Some(summary.conversation.as_str());
        let unread = self.row_unread(summary);
        // The presence dot's ring is the colour the row is drawn on.
        let ring = if selected { c.accent_soft } else { c.ground };
        let mark: Element<'_, Message> = match summary.kind {
            ConversationKind::Dm => {
                let seed = self
                    .counterpart(summary)
                    .unwrap_or(summary.conversation.as_str());
                with_presence(
                    monogram(&label, seed, 28.0, c),
                    28.0,
                    presence.map(|live| if live { c.green } else { c.faint }),
                    ring,
                )
            }
            ConversationKind::Notices => monogram("AgentDocker", "agentdocker", 28.0, c),
            ConversationKind::Collision => channel_mark(28.0, c.amber, c),
            _ => channel_mark(28.0, c.muted, c),
        };
        // The tile already says channel; the name need not say it twice.
        let shown = match summary.kind {
            ConversationKind::Dm | ConversationKind::Notices => label.clone(),
            _ => label.strip_prefix('#').unwrap_or(&label).to_owned(),
        };
        // As many characters as the row has room for, ending in an
        // ellipsis rather than a glyph cut in half: what is left of the
        // row after its padding, mark and counts, at the width of an
        // average character of each face.
        let chips = if unread > 0 {
            if summary.mentions > 0 { 72.0 } else { 36.0 }
        } else {
            0.0
        };
        let room = (width - 16.0 - 30.0 - 10.0 - chips).max(60.0);
        let shown = fit(&shown, (room / 7.6) as usize);
        let mut name = text(shown)
            .size(13.5)
            .font(weight(if unread > 0 {
                iced::font::Weight::Semibold
            } else {
                iced::font::Weight::Medium
            }))
            .wrapping(iced::widget::text::Wrapping::None);
        if unread > 0 && !selected {
            name = name.color(c.text);
        }
        let mut body = column![container(name).width(Fill).clip(true)]
            .spacing(1)
            .width(Fill);
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
            let budget = ((room / 6.6) as usize).max(8);
            let preview = if matches!(
                summary.kind,
                ConversationKind::Dm | ConversationKind::Notices
            ) {
                first_line(line, budget)
            } else {
                first_line(&format!("{who}: {line}"), budget)
            };
            body = body.push(
                container(
                    text(preview)
                        .size(12)
                        .color(if selected { c.accent_ink } else { c.muted })
                        .wrapping(iced::widget::text::Wrapping::None),
                )
                .width(Fill)
                .clip(true),
            );
        }
        let mut content = row![mark, body].spacing(10).align_y(Center);
        // Unread, and how many of those name the person: the @ count is
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
            content = content.push(pill(
                unread.to_string(),
                alpha(c.text, if c.dark { 0.10 } else { 0.07 }),
                c.text,
                c,
            ));
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
            [6, 8],
        )
    }

    /// The list of conversations at whatever width its column has, so a
    /// row's last line is cut to fit with an ellipsis.
    fn messages_sidebar(&self, c: Colors) -> Element<'_, Message> {
        responsive(move |size| self.messages_sidebar_at(size.width, c)).into()
    }

    fn messages_sidebar_at(&self, width: f32, c: Colors) -> Element<'_, Message> {
        // The rows' own width: the column less the scrollbar's lane.
        let row_width = width - 14.0;
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
        // What AgentDocker itself writes — notices to an agent, the rooms
        // it opens between two checkouts — is one folded group, never in
        // among the person's conversations.
        let mut system: Vec<&ConversationSummary> = Vec::new();
        let mut direct: Vec<&ConversationSummary> = Vec::new();
        let mut peers: Vec<&ConversationSummary> = Vec::new();
        let mut earlier: Vec<&ConversationSummary> = Vec::new();
        for summary in self.conversations.iter().filter(|s| matches(s)) {
            match summary.kind {
                ConversationKind::Everyone | ConversationKind::All | ConversationKind::Channel => {
                    channels.push(summary);
                }
                ConversationKind::Collision | ConversationKind::Notices => system.push(summary),
                // The person's own direct messages are the list; what two
                // agents said to each other is a group of its own, folded.
                ConversationKind::Dm => match self.counterpart(summary) {
                    Some(id) if self.agent_live(id) => direct.push(summary),
                    Some(_) => earlier.push(summary),
                    None => peers.push(summary),
                },
            }
        }
        // An identity's older conversations — keyed by a former id — are
        // not hidden: they keep their unread, draft and history, so they
        // sit under Earlier, reachable, while the list shows the identity
        // once.
        let (direct, folded) = self.fold_direct(direct);
        earlier.extend(folded);
        system.sort_by_key(|s| {
            (
                matches!(s.kind, ConversationKind::Notices),
                self.conversation_label(s).to_lowercase(),
            )
        });
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
        // A reply from a notification that did not go, whose conversation
        // could not be opened either: reachable here whatever is on view.
        for recovery in self.shell.orphan_reply_recoveries() {
            list = list.push(
                container(status_line(
                    c.amber,
                    format!(
                        "A reply from a notification was not sent ({}) and its conversation could not be opened: {}",
                        recovery.reason,
                        first_line(&recovery.text, 80)
                    ),
                    c.muted,
                    vec![
                        link(
                            format!("reply-recovery-copy-{}", recovery.message),
                            "Copy",
                            "Copy",
                            Some(Message::ReplyRecoveryCopy(recovery.message.clone())),
                            c.accent,
                        ),
                        link(
                            format!("reply-recovery-dismiss-{}", recovery.message),
                            "Dismiss",
                            "Dismiss",
                            Some(Message::ReplyRecoveryDismiss(recovery.message.clone())),
                            c.muted,
                        ),
                    ],
                    c,
                ))
                .padding([4, 8]),
            );
        }
        // Find one, or start one: the search and, beside it, the way to a
        // conversation that does not exist yet.
        let form_open = self.new_conversation.is_some();
        list = list.push(
            row![
                input_enabled(
                    "messages-search",
                    "Find a conversation…",
                    &self.shell.messages_search,
                    Message::MessagesSearch,
                    true,
                ),
                custom_sized(
                    "new-conversation",
                    if form_open {
                        "Close"
                    } else {
                        "New message or channel"
                    },
                    container(icon(
                        if form_open { Icon::Close } else { Icon::Add },
                        if form_open { c.accent_ink } else { c.muted },
                        16.0,
                    ))
                    .center(18),
                    Some(Message::NewConversation),
                    form_open,
                    Kind::Ghost,
                    [7, 7],
                    iced::Length::Shrink,
                ),
            ]
            .spacing(6)
            .align_y(Center),
        );
        if let Some(form) = &self.new_conversation {
            list = list.push(container(self.new_conversation_form(form, c)).padding(
                iced::Padding {
                    top: 6.0,
                    right: 0.0,
                    bottom: 4.0,
                    left: 0.0,
                },
            ));
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
                        container(
                            text(if row_width >= 300.0 {
                                format!(
                                    "{unread_total} unread in {owed} conversation{}",
                                    if owed == 1 { "" } else { "s" }
                                )
                            } else {
                                format!("{unread_total} unread")
                            })
                            .size(12)
                            .color(c.muted)
                            .wrapping(iced::widget::text::Wrapping::None),
                        )
                        .width(Fill)
                        .clip(true),
                        link(
                            "mark-all-read",
                            "Mark all read",
                            "Mark all read",
                            self.connected.is_ok().then_some(Message::MarkAllRead),
                            c.accent,
                        ),
                    ]
                    .spacing(6)
                    .align_y(Center),
                )
                .padding(iced::Padding {
                    top: 8.0,
                    right: 2.0,
                    bottom: 0.0,
                    left: 8.0,
                }),
            );
        }
        list = list.push(section_label("Channels", c));
        if channels.is_empty() {
            list = list.push(container(note("No channels yet.", c).size(12)).padding([2, 8]));
        }
        for summary in channels {
            list = list.push(self.conversation_row(summary, None, row_width, c));
        }
        list = list.push(section_label("Direct messages", c));
        if direct.is_empty() {
            list = list.push(container(note("No agent is running.", c).size(12)).padding([2, 8]));
        }
        for summary in direct {
            list = list.push(self.conversation_row(summary, Some(true), row_width, c));
        }
        // What is read rather than answered folds away under the lists.
        let mut folds = column![].spacing(2);
        if !system.is_empty() {
            let open = self.shell.collisions_open;
            folds = folds.push(group_header(
                "collisions-toggle",
                "From AgentDocker",
                system.len(),
                open,
                Message::ToggleCollisions,
                c,
            ));
            if open {
                folds = folds.push(guided(
                    system
                        .into_iter()
                        .map(|summary| self.conversation_row(summary, None, row_width - 21.0, c))
                        .collect(),
                    c,
                ));
            }
        }
        if !peers.is_empty() {
            let open = self.shell.peers_open;
            folds = folds.push(group_header(
                "peers-toggle",
                "Between agents",
                peers.len(),
                open,
                Message::TogglePeers,
                c,
            ));
            if open {
                folds = folds.push(guided(
                    peers
                        .into_iter()
                        .map(|summary| self.conversation_row(summary, None, row_width - 21.0, c))
                        .collect(),
                    c,
                ));
            }
        }
        if !earlier.is_empty() {
            // Its own fold and page: the ended sessions' Earlier group on
            // the Agents screen is another list.
            let open = self.shell.earlier_conversations_open;
            folds = folds.push(group_header(
                "earlier-toggle",
                "Earlier",
                earlier.len(),
                open,
                Message::ToggleEarlierConversations,
                c,
            ));
            if open {
                // A page of the earlier conversations at a time, newest
                // first; the rest are a click away.
                let shown = self
                    .shell
                    .earlier_conversations_shown
                    .max(super::EARLIER_PAGE);
                let older = earlier.len().saturating_sub(shown);
                let mut rows: Vec<Element<'_, Message>> = earlier
                    .into_iter()
                    .take(shown)
                    .map(|summary| self.conversation_row(summary, Some(false), row_width - 21.0, c))
                    .collect();
                if older > 0 {
                    let words = format!("Show {} older", older.min(super::EARLIER_PAGE));
                    rows.push(
                        container(link(
                            "earlier-more",
                            words.clone(),
                            words,
                            Some(Message::MoreEarlierConversations),
                            c.accent,
                        ))
                        .padding([4, 2])
                        .into(),
                    );
                }
                folds = folds.push(guided(rows, c));
            }
        }
        list = list.push(container(folds).padding(iced::Padding {
            top: 10.0,
            right: 0.0,
            bottom: 8.0,
            left: 0.0,
        }));
        container(
            scrollable(list)
                .direction(look::slim_scrollbar(4.0))
                .height(Fill)
                .id("conversations-scroll"),
        )
        .padding(iced::Padding {
            top: 0.0,
            right: 4.0,
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

    /// The word a message's kind is shown as after the sender's name, or
    /// nothing for plain talk.
    fn kind_word(kind: &str) -> Option<&str> {
        (kind != "chat" && kind != "message").then_some(kind)
    }

    /// Whether `message` begins a new run under its own name and time
    /// rather than continuing `previous`'s: another sender, another kind,
    /// a pause of more than five minutes, or words that name the person.
    fn starts_run(
        previous: Option<&ArchivedMessage>,
        message: &ArchivedMessage,
        mentions_me: bool,
    ) -> bool {
        let Some(previous) = previous else {
            return true;
        };
        mentions_me
            || previous.envelope.from != message.envelope.from
            || Self::kind_word(&previous.envelope.kind) != Self::kind_word(&message.envelope.kind)
            || message.envelope.sent_at - previous.envelope.sent_at > chrono::Duration::minutes(5)
    }

    /// One archived message. The first of a run carries the sender's mark
    /// and a header — name, kind as a plain word, time — and the ones
    /// after it are its words alone, under the same name. What AgentDocker
    /// says is one quiet line with a small disc. Unread messages sit on a
    /// faint tint; the open thread's message and the one a notification
    /// led to carry an accent rail. Outside a thread, the way into its
    /// thread is a reply count under the words, or, before anyone has
    /// replied, a Reply that shows under the pointer or keyboard focus.
    fn archived_message(
        &self,
        message: &ArchivedMessage,
        previous: Option<&ArchivedMessage>,
        in_thread: bool,
        unread: bool,
        mention_names: &[String],
        c: Colors,
    ) -> Element<'_, Message> {
        let from = message.envelope.from.as_str();
        let system = from == "agentd";
        let name = if self.is_human(from) {
            "You".to_owned()
        } else if system {
            "AgentDocker".to_owned()
        } else {
            self.name_of(from)
        };
        let body_text = line_of(&message.envelope);
        let mentions_me = agentdocker_core::conversation::mentions_any(&body_text, mention_names);
        let head = Self::starts_run(previous, message, mentions_me);
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
        let mark_size = if in_thread { 28.0 } else { 32.0 };
        let time = message
            .envelope
            .sent_at
            .with_timezone(&chrono::Local)
            .format("%H:%M")
            .to_string();
        let open = self.shell.thread.as_ref() == Some(&id);
        let thread_message = if open {
            Message::CloseThread
        } else {
            Message::OpenThread(id.clone())
        };
        let replies = message.replies;
        let reply_label = match replies {
            0 => "Reply".to_owned(),
            1 => "1 reply".to_owned(),
            n => format!("{n} replies"),
        };

        let mut words = column![].spacing(3).width(Fill);
        if system {
            // A notice: the sentence, quiet, and its time after it.
            words = words.push(
                row![
                    text(shown)
                        .size(12.5)
                        .line_height(iced::widget::text::LineHeight::Relative(1.45))
                        .color(c.muted)
                        .width(Fill),
                    text(time.clone()).size(11).color(c.faint),
                ]
                .spacing(10)
                .align_y(iced::Alignment::Start),
            );
        } else {
            if head {
                let mut header = row![
                    text(name.clone())
                        .size(13.5)
                        .font(weight(iced::font::Weight::Semibold))
                        .color(c.text)
                ]
                .spacing(7)
                .align_y(Center);
                match Self::kind_word(&message.envelope.kind) {
                    Some("question") => {
                        header = header.push(
                            row![
                                super::view::dot(c.amber, 6.0, c),
                                text("question")
                                    .size(11.5)
                                    .font(weight(iced::font::Weight::Medium))
                                    .color(c.amber),
                            ]
                            .spacing(5)
                            .align_y(Center),
                        );
                    }
                    Some(kind) => {
                        header = header.push(
                            text(kind.to_owned()).size(11.5).color(if kind == "answer" {
                                c.muted
                            } else {
                                c.faint
                            }),
                        );
                    }
                    None => {}
                }
                header = header.push(text(time.clone()).size(11.5).color(c.faint));
                // A message that names the person says so where the eye
                // lands.
                if mentions_me {
                    header = header.push(
                        text("mentions you")
                            .size(11.5)
                            .font(weight(iced::font::Weight::Medium))
                            .color(c.amber),
                    );
                }
                words = words.push(header);
            }
            words = words.push(
                text(shown)
                    .size(14)
                    .line_height(iced::widget::text::LineHeight::Relative(1.5))
                    .color(c.text),
            );
        }
        if !message.envelope.links.is_empty() {
            words = words.push(super::view::links(&message.envelope.links, c));
        }
        if long {
            words = words.push(look::flush_link(
                format!("message-detail-{id}"),
                if expanded { "Show less" } else { "Show more" },
                Some(Message::ExpandArchived(id.clone())),
                c.accent,
            ));
        }
        // A thread with replies says so under the words, always.
        if !in_thread && replies > 0 {
            words = words.push(row![custom(
                format!("thread-{id}"),
                reply_label.clone(),
                row![
                    text(reply_label.clone())
                        .size(12)
                        .font(weight(iced::font::Weight::Medium))
                        .color(c.accent),
                    icon(Icon::ChevronRight, c.accent, 10.0),
                ]
                .spacing(4)
                .align_y(Center),
                Some(thread_message.clone()),
                open,
                Kind::Inline,
                [2, 0],
            )]);
        }
        let mark: Element<'_, Message> = if system {
            disc(Icon::Pulse, mark_size, c)
        } else if head {
            monogram(&name, from, mark_size, c)
        } else {
            Space::new().width(mark_size).height(1.0).into()
        };
        let pad = if system {
            iced::Padding {
                top: 5.0,
                right: 8.0,
                bottom: 5.0,
                left: 8.0,
            }
        } else if head {
            iced::Padding {
                top: 8.0,
                right: 8.0,
                bottom: 3.0,
                left: 8.0,
            }
        } else {
            iced::Padding {
                top: 1.0,
                right: 8.0,
                bottom: 1.0,
                left: 8.0,
            }
        };
        // The row a notification led to is marked, so a click shows its
        // message even in a conversation that was already open; the id is
        // what the route scrolls to.
        let routed = self.shell.notification_message.as_ref() == Some(&id);
        let (tint, rail) = if open || routed {
            (
                Some(alpha(c.accent, if c.dark { 0.13 } else { 0.07 })),
                Some(c.accent),
            )
        } else if mentions_me && !self.is_human(from) {
            (
                Some(alpha(c.amber, if c.dark { 0.10 } else { 0.07 })),
                Some(c.amber),
            )
        } else if unread {
            (
                Some(alpha(c.accent, if c.dark { 0.07 } else { 0.04 })),
                None,
            )
        } else {
            (None, None)
        };
        let base = tinted(
            container(row![mark, words].spacing(10).align_y(if system {
                Center
            } else {
                iced::Alignment::Start
            }))
            .padding(pad),
            tint,
            rail,
            Some(format!("notification-message-{id}")),
        );
        if in_thread || replies > 0 {
            return base;
        }
        // Before anyone has replied, Reply floats at the row's top right
        // while the pointer is over it or it has the keyboard; it is
        // always there for assistive technology and the workflow driver.
        let reply = container(custom(
            format!("thread-{id}"),
            reply_label.clone(),
            text(reply_label)
                .size(12)
                .line_height(iced::Pixels(16.0))
                .font(weight(iced::font::Weight::Medium)),
            Some(thread_message),
            open,
            Kind::Ghost,
            [2, 8],
        ))
        .padding(1)
        .style(move |_| iced::widget::container::Style {
            border: iced::Border {
                radius: super::style::RADIUS_SM.into(),
                ..c.overlay_style().border
            },
            ..c.overlay_style()
        });
        iced::widget::hover(
            base,
            container(reply)
                .width(Fill)
                .align_right(Fill)
                .padding(iced::Padding {
                    top: if head || system { 3.0 } else { 0.0 },
                    right: 8.0,
                    bottom: 0.0,
                    left: 0.0,
                }),
        )
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

    /// The tone of a recipient's readiness line: green while it is taking
    /// messages, the accent while words wait for it to take them, amber
    /// when it is paused, silent or held by its provider, quiet otherwise.
    fn readiness_tone(status: &str, c: Colors) -> iced::Color {
        match status {
            "Receiving messages" | "Ready for messages" => c.green,
            "Sent · waiting for the agent to take it" => c.accent,
            "Messages may wait for its next prompt" | "Session ended" | "Readiness unavailable" => {
                c.faint
            }
            _ => c.amber,
        }
    }

    /// The composer under a conversation or a thread: one frame with the
    /// draft over a footer of key hints and the send action, the names
    /// `@` offers floating over it, and under it one line per state — the
    /// error, a send that needs attention, a reply that could not be
    /// placed, and the recipient's readiness. Enter sends; Shift+Enter
    /// inserts a line. Each keeps its own draft, and only a thread's
    /// (`root` given) sets `reply_to`.
    fn composer(
        &self,
        conversation: &str,
        root: Option<&MessageId>,
        placeholder: String,
        can_send: bool,
        c: Colors,
        available: iced::Size,
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
        let recipients = self.mention_recipients(conversation);
        // The footer: what the keys do, as far as the width allows, and
        // the one send action. Enter sends, as it does everywhere people
        // type to each other; the arrow is the same action for the pointer.
        let mut hints = row![key_hint("Enter", "send", c)]
            .spacing(14)
            .align_y(Center);
        if available.width >= 400.0 {
            hints = hints.push(key_hint("Shift+Enter", "new line", c));
        }
        if available.width >= 540.0 && can_send && !recipients.is_empty() {
            hints = hints.push(key_hint("@", "mention", c));
        }
        let send = custom_sized(
            format!("send-{key}"),
            if sending { "Sending…" } else { "Send" },
            container(icon(Icon::ArrowUp, iced::Color::WHITE, 14.0)).center(16),
            submit.clone(),
            false,
            Kind::Primary,
            [6, 6],
            iced::Length::Shrink,
        );
        let footer = container(
            row![container(hints).width(Fill).clip(true), send]
                .spacing(8)
                .align_y(Center),
        )
        .padding(iced::Padding {
            top: 5.0,
            right: 6.0,
            bottom: 6.0,
            left: 14.0,
        });
        let field = framed_composer(
            input_id,
            key.clone(),
            &placeholder,
            &text_now,
            move |t| Message::ConversationDraft(owner.clone(), t),
            can_send && !sending,
            submit,
            footer.into(),
        );
        // Mention suggestions include only the conversation's recipients.
        // Inserting a name does not change the Send destination or
        // membership. Iced's editor does not say where its caret is, so
        // the list floats from the field itself.
        let mut offers: Option<Element<'_, Message>> = None;
        if let Some(prefix) = mention_prefix(&text_now) {
            let prefix = prefix.to_lowercase();
            let matches: Vec<&AgentRecord> = recipients
                .into_iter()
                .filter(|a| {
                    a.spec.name.to_lowercase().starts_with(&prefix)
                        || self
                            .name_of(a.id.as_str())
                            .to_lowercase()
                            .starts_with(&prefix)
                })
                .take(6)
                .collect();
            if !matches.is_empty() {
                let mut rows = column![
                    container(
                        text("Mention")
                            .size(11)
                            .font(weight(iced::font::Weight::Medium))
                            .color(c.faint)
                    )
                    .padding([4, 8])
                ]
                .spacing(1);
                for agent in matches {
                    let id = agent.id.as_str();
                    let completed = complete_mention(&text_now, &agent.spec.name);
                    let owner = key.clone();
                    let shown = self.name_of(id);
                    rows = rows.push(custom(
                        format!("mention-{id}"),
                        format!("@{}", agent.spec.name),
                        row![
                            monogram(&shown, id, 20.0, c),
                            container(
                                text(shown.clone())
                                    .size(13)
                                    .font(weight(iced::font::Weight::Medium))
                                    .wrapping(iced::widget::text::Wrapping::None),
                            )
                            .clip(true),
                            container(
                                text(format!("@{}", agent.spec.name))
                                    .size(12)
                                    .color(c.muted)
                                    .wrapping(iced::widget::text::Wrapping::None),
                            )
                            .width(Fill)
                            .clip(true),
                        ]
                        .spacing(8)
                        .align_y(Center),
                        Some(Message::ConversationDraft(owner, completed)),
                        false,
                        Kind::Quiet,
                        [5, 8],
                    ));
                }
                offers = Some(super::view::menu(rows, c));
            }
        }

        // Under the frame, one line per state.
        let mut status = column![].spacing(4);
        if let Some(error) = draft.and_then(|d| d.error.as_ref()) {
            status = status.push(status_line(c.amber, error.clone(), c.amber, Vec::new(), c));
        }
        if let Some(notice) = draft.and_then(|draft| {
            super::send_readiness::composer_notice(
                draft,
                super::shell::DeliveryTarget::Conversation(key.clone()),
                c,
            )
        }) {
            status = status.push(notice);
        }
        // A reply from a notification the draft could not take waits
        // here, its words the person's to copy or let go.
        for recovery in self
            .shell
            .reply_recoveries
            .iter()
            .filter(|r| r.conversation.as_deref() == Some(key.as_str()))
        {
            status = status.push(status_line(
                c.amber,
                format!(
                    "A reply from a notification was not placed ({}): {}",
                    recovery.reason,
                    first_line(&recovery.text, 80)
                ),
                c.muted,
                vec![
                    link(
                        format!("reply-recovery-copy-{}", recovery.message),
                        "Copy",
                        "Copy",
                        Some(Message::ReplyRecoveryCopy(recovery.message.clone())),
                        c.accent,
                    ),
                    link(
                        format!("reply-recovery-dismiss-{}", recovery.message),
                        "Dismiss",
                        "Dismiss",
                        Some(Message::ReplyRecoveryDismiss(recovery.message.clone())),
                        c.muted,
                    ),
                ],
                c,
            ));
        }
        // Put the receiver state where a person is about to send, including
        // thread replies. A working MCP/hook transport alone cannot wake it.
        if let Some(agent) = self.direct_input_recipient(conversation) {
            let readiness = self.input_readiness(agent);
            let mut actions = Vec::new();
            if self
                .runtimes
                .iter()
                .any(|runtime| runtime.name == agent.spec.runtime)
            {
                actions.push(link(
                    format!("input-connection-{key}"),
                    "Connection",
                    "Connection",
                    Some(Message::OpenConnection(agent.spec.runtime.clone())),
                    c.accent,
                ));
            }
            status = status.push(status_line(
                Self::readiness_tone(readiness, c),
                readiness,
                c.muted,
                actions,
                c,
            ));
        }
        // The pane supplies its actual remaining height after headers and
        // compact agent controls. Keep typing visible while feedback scrolls.
        let block = column![
            field,
            container(
                scrollable(container(status).padding([0, 4]))
                    .height(iced::Shrink)
                    .id(format!("composer-feedback-{key}")),
            )
            .max_height((available.height - 150.0).clamp(0.0, 180.0)),
        ]
        .spacing(6);
        // The names float from the whole composer, frame and status lines
        // together: it sits at the foot of its pane, so they open above
        // the words being typed and never over the lines under them.
        popover(block, offers).width(280.0).into()
    }

    /// The live agents the person can talk to, in the project the sidebar
    /// is scoped to when it is: who a direct message can go to, who a
    /// channel can hold.
    fn agents_to_talk_to(&self) -> Vec<&AgentRecord> {
        let naming = self.naming();
        let mut agents: Vec<&AgentRecord> = self
            .agents
            .iter()
            .filter(|a| a.status.is_live() && !self.is_human(a.id.as_str()))
            .filter(|a| self.has_project(a.project.as_ref()))
            // One identity once: a former id is not another agent to talk to.
            .filter(|a| !naming.folded(a))
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

    /// Rows in one hairline frame, a rule between each: the agents a
    /// new conversation can be with.
    fn bordered_list<'a>(rows: Vec<Element<'a, Message>>, c: Colors) -> Element<'a, Message> {
        let mut list = column![].width(Fill);
        for (index, item) in rows.into_iter().enumerate() {
            if index > 0 {
                list = list.push(container(rule(c)).padding([0, 4]));
            }
            list = list.push(item);
        }
        container(list)
            .padding(2)
            .width(Fill)
            .style(move |_| iced::widget::container::Style {
                background: Some(c.card.into()),
                border: iced::Border {
                    color: c.line,
                    width: 1.0,
                    radius: super::style::RADIUS_MD.into(),
                },
                ..Default::default()
            })
            .into()
    }

    /// A field's name over it, with an optional quieter note.
    fn field_label<'a>(name: &str, note_text: Option<&str>, c: Colors) -> Element<'a, Message> {
        let mut line = row![
            text(name.to_owned())
                .size(12)
                .font(weight(iced::font::Weight::Medium))
                .color(c.muted)
        ]
        .spacing(6)
        .align_y(Center);
        if let Some(note_text) = note_text {
            line = line.push(text(note_text.to_owned()).size(12).color(c.faint));
        }
        line.into()
    }

    /// A drawn checkbox: an accent square with a tick, or an empty edge.
    fn checkbox<'a>(checked: bool, c: Colors) -> Element<'a, Message> {
        let tick: Element<'a, Message> = if checked {
            icon(Icon::Check, iced::Color::WHITE, 11.0)
        } else {
            Space::new().width(11).height(11).into()
        };
        container(tick)
            .center(16)
            .style(move |_| iced::widget::container::Style {
                background: Some(if checked { c.accent } else { c.card }.into()),
                border: iced::Border {
                    color: if checked { c.accent } else { c.line_strong },
                    width: 1.0,
                    radius: super::style::RADIUS_XS.into(),
                },
                ..Default::default()
            })
            .into()
    }

    /// One choice of the Direct message | Channel switch, half the track.
    fn kind_segment<'a>(
        id: &str,
        label: &str,
        message: Message,
        selected: bool,
    ) -> Element<'a, Message> {
        custom_sized(
            id.to_owned(),
            label.to_owned(),
            container(
                text(label.to_owned())
                    .size(13)
                    .line_height(iced::Pixels(18.0))
                    .font(weight(iced::font::Weight::Medium)),
            )
            .center_x(Fill),
            Some(message),
            selected,
            Kind::Segment,
            [5, 10],
            Fill,
        )
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
        let kinds = container(
            row![
                Self::kind_segment(
                    "new-kind-direct",
                    "Direct message",
                    Message::NewConversationKind(NewKind::Direct),
                    form.kind == NewKind::Direct,
                ),
                Self::kind_segment(
                    "new-kind-channel",
                    "Channel",
                    Message::NewConversationKind(NewKind::Channel),
                    form.kind == NewKind::Channel,
                ),
            ]
            .spacing(2),
        )
        .padding(2)
        .width(Fill)
        .style(move |_| iced::widget::container::Style {
            background: Some(if c.dark { c.ground } else { c.raised }.into()),
            border: iced::Border {
                color: c.line,
                width: 1.0,
                radius: super::style::RADIUS_MD.into(),
            },
            ..Default::default()
        });
        let mut body = column![
            text("New conversation")
                .size(14)
                .font(weight(iced::font::Weight::Semibold)),
            kinds
        ]
        .spacing(10);
        match form.kind {
            NewKind::Direct => {
                if agents.is_empty() {
                    body = body.push(note("No agent is running here to message.", c).size(12));
                } else {
                    let rows = agents
                        .into_iter()
                        .map(|agent| {
                            let id = agent.id.as_str();
                            let conversation = human.as_deref().map(|me| {
                                agentdocker_core::ConversationId::dm(me, id)
                                    .as_str()
                                    .to_owned()
                            });
                            let shown = self.name_of(id);
                            custom(
                                format!("new-direct-{id}"),
                                shown.clone(),
                                row![
                                    with_presence(
                                        monogram(&shown, id, 24.0, c),
                                        24.0,
                                        Some(c.green),
                                        c.card,
                                    ),
                                    column![
                                        text(shown)
                                            .size(13)
                                            .font(weight(iced::font::Weight::Medium)),
                                        text(self.tool_of(id)).size(12).color(c.muted),
                                    ]
                                    .spacing(1),
                                ]
                                .spacing(10)
                                .align_y(Center),
                                conversation.map(Message::NewDirect),
                                false,
                                Kind::Quiet,
                                [6, 8],
                            )
                        })
                        .collect();
                    body = body.push(Self::bordered_list(rows, c));
                }
            }
            NewKind::Channel => {
                let ready = self.connected.is_ok() && !form.creating && !form.name.is_empty();
                let create = ready.then_some(Message::CreateChannel);
                body = body.push(
                    column![
                        Self::field_label("Name", None, c),
                        input_submitting(
                            "new-channel-name",
                            "Name, like planning",
                            &form.name,
                            Message::NewChannelName,
                            !form.creating,
                            create.clone(),
                        ),
                    ]
                    .spacing(5),
                );
                body = body.push(
                    column![
                        Self::field_label("Purpose", Some("optional"), c),
                        input_submitting(
                            "new-channel-purpose",
                            "What it is for",
                            &form.purpose,
                            Message::NewChannelPurpose,
                            !form.creating,
                            create.clone(),
                        ),
                    ]
                    .spacing(5),
                );
                let who = if form.members.is_empty() {
                    "everyone here; pick some to narrow it".to_owned()
                } else {
                    format!("you and {}", form.members.len())
                };
                let mut members = column![Self::field_label("Members", Some(&who), c)].spacing(5);
                if agents.is_empty() {
                    members = members.push(note("No agent is running here.", c).size(12));
                } else {
                    let rows = agents
                        .into_iter()
                        .map(|agent| {
                            let id = agent.id.as_str();
                            let picked = form.members.contains(&agent.id);
                            let shown = self.name_of(id);
                            custom(
                                format!("new-member-{id}"),
                                shown.clone(),
                                row![
                                    Self::checkbox(picked, c),
                                    monogram(&shown, id, 20.0, c),
                                    container(
                                        text(shown)
                                            .size(13)
                                            .font(weight(iced::font::Weight::Medium))
                                            .wrapping(iced::widget::text::Wrapping::None),
                                    )
                                    .width(Fill)
                                    .clip(true),
                                    text(self.tool_of(id)).size(12).color(c.faint),
                                ]
                                .spacing(9)
                                .align_y(Center),
                                (!form.creating)
                                    .then_some(Message::NewChannelMember(agent.id.clone())),
                                false,
                                Kind::Quiet,
                                [6, 8],
                            )
                        })
                        .collect();
                    members = members.push(Self::bordered_list(rows, c));
                }
                body = body.push(members);
                body = body.push(
                    row![
                        Space::new().width(Fill),
                        primary(
                            "new-channel-create",
                            if form.creating {
                                "Opening…"
                            } else {
                                "Create channel"
                            },
                            create,
                        )
                    ]
                    .align_y(Center),
                );
            }
        }
        if let Some(error) = &form.error {
            body = body.push(status_line(c.amber, error.clone(), c.amber, Vec::new(), c));
        }
        container(body)
            .padding(12)
            .width(Fill)
            .style(move |_| c.card_style())
            .into()
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
                container(note(
                    "This channel is no longer available to add members.",
                    c,
                ))
                .padding(8),
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
                .size(14)
                .font(weight(iced::font::Weight::Semibold))
        ]
        .spacing(10);
        if agents.is_empty() {
            body = body.push(note("All available agents are already members.", c).size(12));
        } else {
            let rows = agents
                .into_iter()
                .map(|agent| {
                    let id = agent.id.as_str();
                    let shown = self.name_of(id);
                    container(
                        row![
                            monogram(&shown, id, 24.0, c),
                            column![
                                text(shown.clone())
                                    .size(13)
                                    .font(weight(iced::font::Weight::Medium)),
                                text(self.tool_of(id)).size(12).color(c.muted),
                            ]
                            .spacing(1)
                            .width(Fill),
                            custom(
                                format!("invite-member-{}", agent.id),
                                format!("Add {shown}"),
                                text("Add")
                                    .size(13)
                                    .line_height(iced::Pixels(crate::controls::LABEL_LINE))
                                    .font(weight(iced::font::Weight::Medium)),
                                (!form.creating && self.connected.is_ok())
                                    .then(|| Message::InviteMember(agent.id.to_string())),
                                false,
                                Kind::Secondary,
                                [5, 12],
                            ),
                        ]
                        .spacing(10)
                        .align_y(Center),
                    )
                    .padding([6, 8])
                    .into()
                })
                .collect();
            body = body.push(Self::bordered_list(rows, c));
        }
        if form.creating {
            body = body.push(note("Adding member…", c).size(12));
        }
        if let Some(error) = &form.error {
            body = body.push(status_line(c.amber, error.clone(), c.amber, Vec::new(), c));
        }
        container(body)
            .padding(12)
            .width(Fill)
            .style(move |_| c.card_style())
            .into()
    }

    pub(super) fn messages_pane(&self, c: Colors) -> Element<'_, Message> {
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
                format!("{} · {}", summary.title, plural(members, "participant"))
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
            // A live session's branch belongs here, under the name.
            ConversationKind::Dm => match self.counterpart(&summary) {
                Some(id) if self.agent_live(id) => {
                    let naming = self.naming();
                    match naming.record(id).and_then(|a| naming.context(a)) {
                        Some(context) => format!("direct message · {context}"),
                        None => "direct message".to_owned(),
                    }
                }
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
        // The name on one line, what the room is about under it; a direct
        // conversation leads with the other party's mark and presence.
        let mut title_row = row![].spacing(10).align_y(Center);
        if summary.kind == ConversationKind::Dm
            && let Some(id) = self.counterpart(&summary)
        {
            title_row = title_row.push(with_presence(
                monogram(&label, id, 28.0, c),
                28.0,
                Some(if self.agent_live(id) {
                    c.green
                } else {
                    c.faint
                }),
                c.ground,
            ));
        }
        title_row = title_row.push(
            column![
                container(
                    text(label.clone())
                        .size(16)
                        .font(weight(iced::font::Weight::Semibold))
                        .wrapping(iced::widget::text::Wrapping::None)
                )
                .width(Fill)
                .clip(true),
                container(
                    text(topic)
                        .size(12)
                        .color(c.muted)
                        .wrapping(iced::widget::text::Wrapping::None)
                )
                .width(Fill)
                .clip(true),
            ]
            .spacing(1)
            .width(Fill),
        );
        if matches!(
            summary.kind,
            ConversationKind::Channel | ConversationKind::Collision
        ) {
            // An overlap room is folded on Channels: going to it on purpose
            // opens the fold, or Reviews would land where it is hidden.
            title_row = title_row.push(crate::controls::ghost(
                "open-channel-tools",
                "Reviews",
                Some(if summary.kind == ConversationKind::Collision {
                    Message::ReviewOverlaps
                } else {
                    Message::Navigate(Screen::Channels)
                }),
            ));
        }
        if summary.kind == ConversationKind::Channel
            && summary.members.iter().any(|id| self.is_human(id.as_str()))
            && let Some(channel) = summary.conversation.channel_id()
            && self
                .channels
                .iter()
                .any(|item| item.id == channel && item.is_open())
        {
            title_row = title_row.push(crate::controls::button(
                "invite-channel",
                "Add members",
                Some(Message::InviteChannel(channel.to_string())),
                false,
            ));
        }
        let header = container(title_row).padding(iced::Padding {
            top: 2.0,
            right: 0.0,
            bottom: 10.0,
            left: 0.0,
        });

        let history = self.history.get(&key);
        let mut list = column![].width(Fill);
        if history.is_some_and(|m| !m.is_empty()) && !self.history_complete.contains(&key) {
            list = list.push(
                container(link(
                    format!("earlier-{key}"),
                    "Show earlier messages",
                    "Show earlier messages",
                    Some(Message::EarlierHistory(key.clone())),
                    c.accent,
                ))
                .padding([6, 0])
                .center_x(Fill),
            );
        }
        match history {
            None => list = list.push(container(note("Loading…", c)).padding([16, 8])),
            Some(messages) if messages.is_empty() => {
                list = list.push(
                    container(note("Nothing said here yet.", c))
                        .padding([24, 8])
                        .center_x(Fill),
                );
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
                // The message a run continues from; a divider, a notice
                // or a question card ends the run before it.
                let mut previous: Option<&ArchivedMessage> = None;
                for (i, message) in messages.iter().enumerate() {
                    let day = message
                        .envelope
                        .sent_at
                        .with_timezone(&chrono::Local)
                        .date_naive();
                    if last_day != Some(day) {
                        list = list.push(day_divider(Self::day_label(message.envelope.sent_at), c));
                        last_day = Some(day);
                        previous = None;
                    }
                    if unread_from == Some(i) {
                        list = list.push(unread_divider(c));
                        previous = None;
                    }
                    // A question keeps its card, since the card carries the
                    // controls; the archive row says where it sits.
                    if message.envelope.kind == "question"
                        && let Some(question) =
                            self.questions.iter().find(|q| q.id == message.envelope.id)
                    {
                        list =
                            list.push(container(self.question_card(question, c)).padding([6, 8]));
                        previous = None;
                        continue;
                    }
                    let unread = unread_from.is_some_and(|from| i >= from)
                        && !self.is_human(&message.envelope.from);
                    list = list.push(self.archived_message(
                        message,
                        previous,
                        false,
                        unread,
                        &mention_names,
                        c,
                    ));
                    previous = (message.envelope.from != "agentd").then_some(message);
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
        let composer_key = key.clone();
        let composer = responsive(move |size| {
            self.composer(&composer_key, None, placeholder.clone(), can_send, c, size)
        })
        .height(iced::Shrink);
        column![
            header,
            rule(c),
            container(
                scrollable(container(list).padding(iced::Padding {
                    top: 4.0,
                    right: 0.0,
                    bottom: 8.0,
                    left: 0.0,
                }))
                .direction(look::slim_scrollbar(8.0))
                .height(Fill)
                .anchor_bottom()
                .id(format!("history-{key}"))
            )
            .height(Fill),
            container(composer).padding(iced::Padding {
                top: 4.0,
                right: 0.0,
                bottom: 2.0,
                left: 0.0
            }),
        ]
        .height(Fill)
        .into()
    }

    /// Where an open thread is: `in #everyone`, `with Codex · Heron`.
    fn thread_context(&self) -> Option<String> {
        let summary = self.open_summary()?;
        let label = self.conversation_label(&summary);
        Some(match summary.kind {
            ConversationKind::Dm => format!("with {label}"),
            _ => format!("in {label}"),
        })
    }

    pub(super) fn thread_pane(&self, c: Colors) -> Element<'_, Message> {
        let mention_names = self.mention_names();
        let Some(root_id) = self.shell.thread.as_ref() else {
            return Space::new().into();
        };
        let mut words = column![
            text("Thread")
                .size(14)
                .font(weight(iced::font::Weight::Semibold))
        ]
        .spacing(1)
        .width(Fill);
        if let Some(context) = self.thread_context() {
            words = words.push(
                container(
                    text(context)
                        .size(12)
                        .color(c.muted)
                        .wrapping(iced::widget::text::Wrapping::None),
                )
                .width(Fill)
                .clip(true),
            );
        }
        let mut header = row![words].spacing(8).align_y(Center);
        // Narrow, the way back above the pane closes it; one control, one id.
        if !self.messages_compact() {
            header = header.push(custom_sized(
                "close-thread",
                "Close",
                container(icon(Icon::Close, c.muted, 14.0)).center(18),
                Some(Message::CloseThread),
                false,
                Kind::Ghost,
                [7, 7],
                iced::Length::Shrink,
            ));
        }
        let mut list = column![].width(Fill);
        match &self.thread {
            Some((root, replies)) if root.envelope.id == *root_id => {
                // The message the thread answers, quoted on the accent
                // rail: it is the open thread's own message.
                list = list.push(self.archived_message(root, None, true, false, &mention_names, c));
                list = list.push(count_divider(
                    match replies.len() {
                        0 => "No replies yet".to_owned(),
                        1 => "1 reply".to_owned(),
                        n => format!("{n} replies"),
                    },
                    c,
                ));
                let mut previous: Option<&ArchivedMessage> = None;
                for reply in replies {
                    list = list.push(self.archived_message(
                        reply,
                        previous,
                        true,
                        false,
                        &mention_names,
                        c,
                    ));
                    previous = (reply.envelope.from != "agentd").then_some(reply);
                }
            }
            _ => list = list.push(container(note("Loading…", c)).padding([16, 8])),
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
            container(header).padding(iced::Padding {
                top: 2.0,
                right: 0.0,
                bottom: 10.0,
                left: 0.0,
            }),
            rule(c),
            container(
                scrollable(container(list).padding(iced::Padding {
                    top: 6.0,
                    right: 0.0,
                    bottom: 8.0,
                    left: 0.0,
                }))
                .direction(look::slim_scrollbar(8.0))
                .height(Fill)
                .anchor_bottom()
                // End-anchored: `history-` is how a reveal knows to
                // count its offset from the end.
                .id(format!("history-thread-{root_id}"))
            )
            .height(Fill),
            container(
                responsive(move |size| {
                    self.composer(
                        &key,
                        Some(root_id),
                        placeholder.to_owned(),
                        can_send,
                        c,
                        size,
                    )
                })
                .height(iced::Shrink)
            )
            .padding(iced::Padding {
                top: 4.0,
                right: 0.0,
                bottom: 2.0,
                left: 0.0
            }),
        ]
        .height(Fill)
        .into()
    }

    /// One identity, one row: of the direct conversations with one live
    /// agent — one per id it has had — the one written in last is the
    /// row, newest first among the rows. The others are returned too, for
    /// the Earlier group: the daemon keeps each conversation under its own
    /// key, with its own unread, draft and history, and none of that may
    /// be hidden.
    fn fold_direct<'s>(
        &self,
        mut direct: Vec<&'s ConversationSummary>,
    ) -> (Vec<&'s ConversationSummary>, Vec<&'s ConversationSummary>) {
        let naming = self.naming();
        direct.sort_by_key(|s| std::cmp::Reverse(s.last_at));
        let mut seen: Vec<&str> = Vec::new();
        let mut folded = Vec::new();
        direct.retain(|summary| {
            let Some(id) = self.counterpart(summary) else {
                return true;
            };
            let canonical = naming.canonical(id);
            if seen.contains(&canonical) {
                folded.push(*summary);
                return false;
            }
            seen.push(canonical);
            true
        });
        (direct, folded)
    }

    /// The direct rows as the sidebar lists them and the ones it moves to
    /// Earlier, by conversation id; for tests.
    #[cfg(test)]
    fn direct_rows(&self) -> (Vec<&str>, Vec<&str>) {
        let direct: Vec<&ConversationSummary> = self
            .conversations
            .iter()
            .filter(|s| {
                s.kind == ConversationKind::Dm
                    && self.counterpart(s).is_some_and(|id| self.agent_live(id))
            })
            .collect();
        let (direct, folded) = self.fold_direct(direct);
        (
            direct
                .into_iter()
                .map(|s| s.conversation.as_str())
                .collect(),
            folded
                .into_iter()
                .map(|s| s.conversation.as_str())
                .collect(),
        )
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

/// `text` cut to at most `budget` characters, the last one an ellipsis
/// when anything was cut.
fn fit(text: &str, budget: usize) -> String {
    let budget = budget.max(4);
    if text.chars().count() <= budget {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(budget - 1).collect();
    out.push('…');
    out
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

    #[test]
    fn a_row_name_is_cut_with_an_ellipsis_only_when_it_does_not_fit() {
        assert_eq!(fit("planning", 20), "planning");
        assert_eq!(fit("fixture-coordination", 12), "fixture-coo…");
        assert_eq!(fit("日本語のチャンネル名", 5), "日本語の…");
        assert_eq!(fit("abcdef", 0), "abc…");
    }

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

    /// The list reads by stable names: a pair of agents as its two names,
    /// a collision room as a count of paths, and a conversation keyed by a
    /// former id of a live agent stands with the one keyed by its id now —
    /// one identity, one row.
    #[test]
    fn rows_are_named_stably_and_one_identity_is_one_row() {
        let (commands, _requests) = queue::channel();
        let (_messages, results) = sync_channel(MESSAGE_CAPACITY);
        let mut app = App::bare(commands, results);
        let mut human = record("user", agentdocker_core::HUMAN_RUNTIME, None);
        human.id = AgentId::from("human-id");
        let mut first = record("codex-1", "codex", Some(1));
        first.id = AgentId::from("agent-a");
        let mut second = record("codex-2", "codex", Some(2));
        second.id = AgentId::from("agent-b");
        second.created_at = first.created_at + chrono::Duration::seconds(1);
        let mut claude = record("claude-code-3", "claude-code", Some(3));
        claude.id = AgentId::from("agent-c");
        app.agents = vec![human, first, second, claude];
        app.aliases =
            std::collections::BTreeMap::from([("agent-a-old".to_owned(), "agent-a".to_owned())]);
        let summary =
            |conversation: &str, kind: &str, title: &str, at: i64| -> ConversationSummary {
                serde_json::from_value(serde_json::json!({
                    "conversation": conversation, "kind": kind, "title": title,
                    "members": [], "unread": 0,
                    "last_at": Utc::now() + chrono::Duration::seconds(at),
                }))
                .unwrap()
            };
        let pair = summary(
            agentdocker_core::ConversationId::dm("agent-a", "agent-b").as_str(),
            "dm",
            "",
            0,
        );
        assert_eq!(
            app.conversation_label(&pair),
            format!(
                "Codex · {} ↔ Codex · {}",
                super::naming::word_for("agent-a"),
                super::naming::word_for("agent-b")
            )
        );
        let with_claude = summary(
            agentdocker_core::ConversationId::dm("agent-c", "agent-b").as_str(),
            "dm",
            "",
            0,
        );
        assert_eq!(
            app.conversation_label(&with_claude),
            format!(
                "Codex · {} ↔ Claude Code · {}",
                super::naming::word_for("agent-b"),
                super::naming::word_for("agent-c")
            )
        );
        let contested = summary(
            "channel:ee3cbc67d8b7",
            "collision",
            ".coderabbit.yaml, .config/nextest.toml, .github/workflows/ci.yml (+300 more)",
            0,
        );
        assert_eq!(app.conversation_label(&contested), "Contested paths (303)");
        assert_eq!(
            App::contested_paths(&summary("channel:x", "collision", "a.rs, b.rs", 0)),
            Some(2)
        );
        assert_eq!(
            App::contested_paths(&summary("channel:x", "collision", "", 0)),
            None
        );
        let notice = summary("notices:agent-b", "notices", "", 0);
        assert_eq!(
            app.conversation_label(&notice),
            format!(
                "AgentDocker → Codex · {}",
                super::naming::word_for("agent-b")
            )
        );
        assert_eq!(app.tool_of("agent-c"), "Claude Code");

        // Two direct conversations with one identity: the newer stands.
        let old_key = summary(
            agentdocker_core::ConversationId::dm("human-id", "agent-a-old").as_str(),
            "dm",
            "",
            -60,
        );
        let new_key = summary(
            agentdocker_core::ConversationId::dm("human-id", "agent-a").as_str(),
            "dm",
            "",
            0,
        );
        let agent_a = format!("Codex · {}", super::naming::word_for("agent-a"));
        assert_eq!(app.conversation_label(&old_key), agent_a);
        assert_eq!(app.conversation_label(&new_key), agent_a);
        // The older key keeps unread and a draft: it is not hidden, it is
        // moved under Earlier, and the badge still counts it.
        let mut old_key = old_key;
        old_key.unread = 2;
        let _ = app.update(Message::ConversationDraft(
            old_key.conversation.as_str().to_owned(),
            "half a reply".into(),
        ));
        app.conversations = vec![
            old_key.clone(),
            new_key.clone(),
            pair.clone(),
            notice.clone(),
        ];
        let (rows, folded) = app.direct_rows();
        assert_eq!(rows, vec![new_key.conversation.as_str()]);
        assert_eq!(folded, vec![old_key.conversation.as_str()]);
        assert_eq!(
            app.unread_total(),
            2,
            "the folded conversation's unread is still owed"
        );
        assert_eq!(
            app.shell.conversation_drafts[old_key.conversation.as_str()].text,
            "half a reply"
        );
        // Only the newest keyed row stays in the list; a different agent
        // keeps its own.
        let other = summary(
            agentdocker_core::ConversationId::dm("human-id", "agent-b").as_str(),
            "dm",
            "",
            -5,
        );
        app.conversations.push(other.clone());
        assert_eq!(
            app.direct_rows().0,
            vec![new_key.conversation.as_str(), other.conversation.as_str()]
        );
    }

    /// A run is one sender saying one kind of thing without a pause: the
    /// next message joins it under the same header; another sender,
    /// another kind, five quiet minutes or words that name the person
    /// start a new one.
    #[test]
    fn consecutive_messages_share_one_header_until_something_changes() {
        let at = Utc::now();
        let message = |from: &str, kind: &str, minutes: i64| ArchivedMessage {
            seq: 0,
            conversation: agentdocker_core::ConversationId::from("channel:room".to_owned()),
            envelope: {
                let mut envelope = agentdocker_core::Envelope::new(
                    from,
                    agentdocker_core::Destination::Broadcast,
                    kind,
                    serde_json::json!({ "text": "words" }),
                    None,
                    at + chrono::Duration::minutes(minutes),
                );
                envelope.id = MessageId::from(format!("{from}-{kind}-{minutes}"));
                envelope
            },
            replies: 0,
        };
        let first = message("agent-a", "chat", 0);
        assert!(App::starts_run(None, &first, false));
        assert!(!App::starts_run(
            Some(&first),
            &message("agent-a", "chat", 4),
            false
        ));
        // Plain talk is plain talk whichever word the sender used for it.
        assert!(!App::starts_run(
            Some(&first),
            &message("agent-a", "message", 1),
            false
        ));
        assert!(App::starts_run(
            Some(&first),
            &message("agent-b", "chat", 1),
            false
        ));
        assert!(App::starts_run(
            Some(&first),
            &message("agent-a", "answer", 1),
            false
        ));
        assert!(App::starts_run(
            Some(&first),
            &message("agent-a", "chat", 6),
            false
        ));
        assert!(App::starts_run(
            Some(&first),
            &message("agent-a", "chat", 1),
            true
        ));
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
