//! The board: the project's cards in five columns, Backlog to Done. The
//! person files a card here — a title and what done means — and moves it
//! with a click; an agent pulls a Ready card and the column says who has
//! it. No dragging: a card moves by the arrows on it, which is also what
//! the smoke and the accessibility tree can drive.
use super::style::{Colors, alpha, weight};
use super::view::{dot, empty, eyebrow, first_line, note, panel, pill, small};
use super::*;
use crate::controls::{Kind, button as action, custom, input_enabled, primary};
use agentdocker_core::{Column, Task};
use iced::{
    Center, Element, Fill, Top,
    widget::{Space, column, container, row, scrollable, text},
};

impl App {
    pub(super) fn board_view(&self, c: Colors) -> Element<'_, Message> {
        let Some(entry) = self.shell.catalog.selected() else {
            return empty(
                "Pick a project",
                "The board is a project's: choose one on the left.",
                None,
                c,
            );
        };
        let root = entry.project.root.display().to_string();
        let board = self.tasks.as_ref().filter(|b| b.project == root);
        let tasks: &[Task] = board.map_or(&[], |b| &b.cards);
        let more = board.is_some_and(|b| b.more);
        // A page or a refresh on its way: the control says so and takes
        // no click, since a click now would be refused anyway.
        let loading = board.is_some_and(Board::loading_more)
            || self.board_asks.values().any(|(p, _)| *p == root);
        let mut page = column![self.file_card(&root, c)].spacing(14).width(Fill);
        if tasks.is_empty() && self.tasks.is_some() {
            page = page.push(note(
                "Nothing on the board yet. File a card above; agents pull from Ready.",
                c,
            ));
        }
        // The board goes on: the next page is a click away, up to what
        // the window keeps; past that, archiving is how it gets shorter.
        if more && tasks.len() < BOARD_KEEP {
            page = page.push(
                row![
                    small(
                        format!("{} cards, Backlog to Done; the board goes on.", tasks.len()),
                        c
                    ),
                    action(
                        "board-more",
                        if loading { "Loading…" } else { "Show more" },
                        (self.connected.is_ok() && !loading).then_some(Message::TasksMore),
                        false,
                    ),
                ]
                .spacing(8)
                .align_y(Center),
            );
        } else if more {
            page = page.push(note(
                format!(
                    "The first {} cards are on view and the board goes on past them: archive what is done, or read a column at a time with agentdocker task list --column.",
                    tasks.len()
                ),
                c,
            ));
        }
        let narrow = self.narrow();
        // Wide, five columns side by side; narrow, one below the other.
        let mut columns = if narrow {
            column![].spacing(12).width(Fill)
        } else {
            column![].spacing(0).width(Fill)
        };
        let mut lane = row![].spacing(12).align_y(Top);
        for column_kind in Column::ALL {
            let cards: Vec<&Task> = tasks.iter().filter(|t| t.column == column_kind).collect();
            let lane_body = self.lane(column_kind, &cards, c);
            if narrow {
                columns = columns.push(lane_body);
            } else {
                lane = lane.push(container(lane_body).width(Fill));
            }
        }
        if !narrow {
            columns = columns.push(
                scrollable(lane)
                    .id("board-lanes")
                    .direction(iced::widget::scrollable::Direction::Horizontal(
                        iced::widget::scrollable::Scrollbar::default(),
                    ))
                    .width(Fill),
            );
        }
        page.push(columns).into()
    }

    /// A title, what done means, and where it goes: Backlog to think
    /// about, or Ready for the next agent to pull.
    fn file_card(&self, project: &str, c: Colors) -> Element<'_, Message> {
        let blank = TaskDraft::default();
        let draft = self.task_drafts.get(project).unwrap_or(&blank);
        let ready = self.connected.is_ok() && !draft.sending() && !draft.title.trim().is_empty();
        let mut form = column![
            eyebrow("File a card", c),
            input_enabled(
                "task-title",
                "What needs doing",
                &draft.title,
                Message::TaskTitle,
                !draft.sending(),
            ),
            input_enabled(
                "task-acceptance",
                "What done means — an agent reads this before it starts",
                &draft.acceptance,
                Message::TaskAcceptance,
                !draft.sending(),
            ),
            row![
                primary(
                    "task-file-ready",
                    if draft.sending() {
                        "Filing…"
                    } else {
                        "File as Ready"
                    },
                    ready.then_some(Message::TaskFile(Column::Ready)),
                ),
                action(
                    "task-file-backlog",
                    "Keep in Backlog",
                    ready.then_some(Message::TaskFile(Column::Backlog)),
                    false,
                ),
            ]
            .spacing(8)
            .align_y(Center),
        ]
        .spacing(8);
        if let Some(error) = &draft.error {
            form = form.push(text(error.clone()).size(13).color(c.amber));
        }
        panel(form, c)
    }

    fn lane(&self, kind: Column, cards: &[&Task], c: Colors) -> Element<'_, Message> {
        let mut lane = column![
            row![eyebrow(kind.label(), c), small(cards.len().to_string(), c),]
                .spacing(6)
                .align_y(Center)
        ]
        .spacing(8)
        .width(Fill);
        if cards.is_empty() {
            lane = lane.push(container(small("—", c)).padding([2, 4]));
        }
        for card in cards {
            lane = lane.push(self.card(card, c));
        }
        container(lane)
            .padding(8)
            .width(Fill)
            .style(move |_| iced::widget::container::Style {
                background: Some(c.raised.into()),
                border: iced::Border {
                    radius: 10.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            })
            .into()
    }

    /// One card: the title, who holds it, and — opened — what done means
    /// and the ways it can move.
    fn card(&self, task: &Task, c: Colors) -> Element<'_, Message> {
        let id = task.id.clone();
        let open = self.task_open.as_ref() == Some(&id);
        // The holding is the task:<id> lease: while the assignee holds it
        // the card is theirs; once it has lapsed — expired, released, or
        // the agent exited — the card is still theirs by name but the
        // next agent to pull it takes it over, and the board says so.
        let resource = format!("task:{id}");
        let holder = task.assignee.as_ref().map(|a| {
            let live = self.agents.iter().any(|r| &r.id == a && r.status.is_live());
            let held = self.leases.iter().any(|l| {
                l.resource.as_str() == resource
                    && &l.holder == a
                    && l.mode == agentdocker_core::LeaseMode::Exclusive
            });
            (self.name_of(a.as_str()), live, held)
        });
        let mut head = column![
            text(first_line(&task.title, 80))
                .size(14)
                .font(weight(iced::font::Weight::Medium))
                .wrapping(iced::widget::text::Wrapping::Word),
        ]
        .spacing(4)
        .width(Fill);
        if let Some((name, live, held)) = &holder {
            let mut who = row![
                dot(if *live { c.green } else { c.faint }, 7.0, c),
                small(name.clone(), c),
            ]
            .spacing(6)
            .align_y(Center);
            if !held && matches!(task.column, Column::InProgress | Column::Review) {
                who = who.push(pill("hold lapsed", alpha(c.amber, 0.2), c.amber, c));
            }
            head = head.push(who);
        } else if task.column == Column::Ready {
            head = head.push(pill("for the taking", c.accent_soft, c.accent_ink, c));
        }
        // The control's name says the state too, so a screen reader — and
        // the smoke — hear who holds the card without opening it.
        let label = match &holder {
            Some((name, _, held)) => {
                if !held && matches!(task.column, Column::InProgress | Column::Review) {
                    format!("{} — {name}'s, hold lapsed", task.title)
                } else {
                    format!("{} — held by {name}", task.title)
                }
            }
            None if task.column == Column::Ready => format!("{} — for the taking", task.title),
            None => task.title.clone(),
        };
        let mut body = column![custom(
            format!("task-{id}"),
            label,
            head,
            Some(Message::TaskOpen(id.clone())),
            open,
            Kind::Quiet,
            [8, 10],
        )]
        .spacing(6)
        .width(Fill);
        if open {
            let acceptance = if task.acceptance.is_empty() {
                "No acceptance text. An agent will decide for itself what done means.".to_owned()
            } else {
                task.acceptance.clone()
            };
            body = body.push(
                container(text(acceptance).size(13).color(c.muted))
                    .padding([2, 10])
                    .width(Fill),
            );
            body = body.push(container(self.card_actions(task, c)).padding([2, 6]));
        }
        container(body)
            .width(Fill)
            .style(move |_| iced::widget::container::Style {
                background: Some(c.card.into()),
                border: iced::Border {
                    color: c.line,
                    width: 1.0,
                    radius: 8.0.into(),
                },
                ..Default::default()
            })
            .into()
    }

    /// Where a card may go from here, who it may be handed to, and off
    /// the board: the person's moves, all of them.
    fn card_actions(&self, task: &Task, c: Colors) -> Element<'_, Message> {
        let id = task.id.clone();
        let connected = self.connected.is_ok();
        let mut actions = row![].spacing(6).align_y(Center);
        let all = Column::ALL;
        let at = all.iter().position(|k| *k == task.column).unwrap_or(0);
        if at > 0 {
            let back = all[at - 1];
            actions = actions.push(action(
                format!("task-back-{id}"),
                format!("‹ {}", back.label()),
                connected.then_some(Message::TaskMove(id.clone(), back)),
                false,
            ));
        }
        if at + 1 < all.len() {
            let next = all[at + 1];
            actions = actions.push(primary(
                format!("task-next-{id}"),
                format!("{} ›", next.label()),
                connected.then_some(Message::TaskMove(id.clone(), next)),
            ));
        }
        let mut hand = row![small("Hand to", c)].spacing(4).align_y(Center);
        let mut anyone = false;
        for agent in self
            .agents
            .iter()
            .filter(|a| a.status.is_live() && !self.is_human(a.id.as_str()))
            .filter(|a| self.has_project(a.project.as_ref()))
        {
            if task.assignee.as_ref() == Some(&agent.id) {
                continue;
            }
            anyone = true;
            hand = hand.push(custom(
                format!("task-hand-{id}-{}", agent.id),
                self.name_of(agent.id.as_str()),
                text(self.name_of(agent.id.as_str()))
                    .size(12)
                    .color(c.accent),
                connected.then_some(Message::TaskAssign(id.clone(), Some(agent.id.clone()))),
                false,
                Kind::Quiet,
                [2, 6],
            ));
        }
        if task.assignee.is_some() {
            anyone = true;
            hand = hand.push(custom(
                format!("task-release-{id}"),
                "nobody",
                text("nobody").size(12).color(c.muted),
                connected.then_some(Message::TaskAssign(id.clone(), None)),
                false,
                Kind::Quiet,
                [2, 6],
            ));
        }
        let mut rows = column![actions].spacing(6);
        if anyone {
            rows = rows.push(hand);
        }
        rows = rows.push(
            row![
                Space::new().width(Fill),
                custom(
                    format!("task-archive-{id}"),
                    "Archive",
                    text("Archive").size(12).color(c.muted),
                    connected.then_some(Message::TaskArchive(id)),
                    false,
                    Kind::Quiet,
                    [2, 6],
                )
            ]
            .align_y(Center),
        );
        rows.into()
    }
}
