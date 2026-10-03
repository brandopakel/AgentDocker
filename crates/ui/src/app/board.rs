//! The board: the project's cards in five lanes, Backlog to Done. The
//! person files a card here — a title and what done means — and moves it
//! with a click; an agent pulls a Ready card and the lane says who has
//! it. No dragging: a card moves by the moves on it, which is also what
//! the smoke and the accessibility tree can drive.
//!
//! Each lane is a tinted well with no frame: a dot in the lane's tone,
//! its name and a count chip, then its cards. A card is a hairline card
//! with no shadow; the pointer darkens its edge and the open one carries
//! the accent edge on the selected tint. Filing lives in Ready's foot,
//! where a new card lands.
use super::icons::{Icon, icon};
use super::style::{Colors, RADIUS_MD, mix, weight};
use super::view::{count_chip, dot, first_line, note, small, status_word};
use super::*;
use crate::controls::{Kind, button as action, custom, ghost, input_enabled, primary};
use agentdocker_core::{Column, Task};
use iced::{
    Center, Element, Fill, Top,
    widget::{Space, column, container, row, text},
};

/// The least the five lanes need side by side — a card's words wrap
/// below this rather than the lanes sharing it — measured against the
/// page beside the rail, which a wide rail in a small window can leave
/// far narrower than the window itself.
const LANES_FIT: f32 = 5.0 * 150.0 + 4.0 * 12.0;

/// The well's corner: a step past a card's, so the cards nest in it.
const WELL_RADIUS: f32 = 12.0;

/// A ghost button's own side padding: the card's moves start this far
/// into their margin so the first move's words line up with the text.
const GHOST_INSET: f32 = 10.0;

/// A lane's own tone, for its dot. The lane's name says the same thing in
/// words beside it.
fn lane_tone(kind: Column, c: Colors) -> iced::Color {
    match kind {
        Column::Backlog => c.faint,
        Column::Ready => c.accent,
        Column::InProgress => c.cyan,
        Column::Review => c.amber,
        Column::Done => c.green,
    }
}

/// The well a lane's cards sit in: a step of tint off the page, no frame.
fn well(c: Colors) -> container::Style {
    container::Style {
        background: Some(
            if c.dark {
                mix(c.ground, c.card, 0.6)
            } else {
                mix(c.ground, c.raised, 0.55)
            }
            .into(),
        ),
        text_color: Some(c.text),
        border: iced::Border {
            radius: WELL_RADIUS.into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// A card as a control: the card fill on a hairline; under the pointer
/// the hairline darkens; open, the accent edge on the selected tint.
/// `framed` is false for the head of a card opened in place, whose frame
/// is the open card around it.
fn card_look(
    c: Colors,
    status: iced::widget::button::Status,
    open: bool,
    framed: bool,
) -> iced::widget::button::Style {
    use iced::widget::button::Status;
    let hovered = matches!(status, Status::Hovered | Status::Pressed);
    // In dark the card steps up from the well a little more than the
    // card role alone would, or the two read as one surface.
    let fill = if c.dark {
        mix(c.card, c.raised, 0.5)
    } else {
        c.card
    };
    let (background, edge) = match (framed, open, hovered) {
        (false, _, _) => (None, iced::Color::TRANSPARENT),
        (true, true, _) => (Some(c.accent_soft), c.accent),
        (true, false, true) => (Some(fill), mix(c.line, c.text, 0.25)),
        (true, false, false) => (Some(fill), c.line),
    };
    iced::widget::button::Style {
        background: background.map(Into::into),
        text_color: c.text,
        border: iced::Border {
            color: edge,
            width: if framed { 1.0 } else { 0.0 },
            radius: RADIUS_MD.into(),
        },
        shadow: iced::Shadow::default(),
        snap: true,
    }
}

impl App {
    pub(super) fn board_view(&self, c: Colors) -> Element<'_, Message> {
        let Some(entry) = self.shell.catalog.selected() else {
            return super::view::empty(
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
        let mut page = column![].spacing(14).width(Fill);
        if tasks.is_empty() && self.tasks.is_some() {
            page = page.push(note(
                "Nothing on the board yet. File a task under Ready; agents take cards from there.",
                c,
            ));
        }
        let narrow = self.narrow() || self.panes.workspace_width() < LANES_FIT;
        // Wide, five lanes sharing the width, the open card's detail
        // beneath them where there is room to read it; narrow, one lane
        // below the other with the detail inside its card.
        let mut lanes = column![].spacing(12).width(Fill);
        let mut side_by_side = row![].spacing(12).align_y(Top).width(Fill);
        for kind in Column::ALL {
            let cards: Vec<&Task> = tasks.iter().filter(|t| t.column == kind).collect();
            let foot = (kind == Column::Ready).then(|| self.file_foot(&root, c));
            let lane = self.lane(kind, &cards, narrow, foot, c);
            if narrow {
                lanes = lanes.push(lane);
            } else {
                side_by_side = side_by_side.push(container(lane).width(Fill));
            }
        }
        if !narrow {
            lanes = lanes.push(side_by_side);
            if let Some(open) = self
                .task_open
                .as_ref()
                .and_then(|id| tasks.iter().find(|t| &t.id == id))
            {
                lanes = lanes.push(self.card_detail(open, c));
            }
        }
        page = page.push(lanes);
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
                .spacing(10)
                .align_y(Center),
            );
        } else if more {
            page = page.push(small(
                format!(
                    "The first {} cards are on view and the board goes on past them: archive what is done, or read a column at a time with agentdocker task list --column.",
                    tasks.len()
                ),
                c,
            ));
        }
        page.into()
    }

    /// Ready's foot: a title, when it counts as done, and where it goes —
    /// Ready for the next agent to take, or Backlog to think about. The
    /// words are the person's — a task, done when — not the lease's.
    fn file_foot(&self, project: &str, c: Colors) -> Element<'_, Message> {
        let blank = TaskDraft::default();
        let draft = self.shell.task_drafts.get(project).unwrap_or(&blank);
        let ready = self.connected.is_ok() && !draft.sending() && !draft.title.trim().is_empty();
        let mut form = column![
            row![
                icon(Icon::Add, c.muted, 13.0),
                text("Add a task")
                    .size(12)
                    .font(weight(iced::font::Weight::Medium))
                    .color(c.muted)
            ]
            .spacing(6)
            .align_y(Center),
            input_enabled(
                "task-title",
                "What needs doing",
                &draft.title,
                Message::TaskTitle,
                !draft.sending(),
            ),
            input_enabled(
                "task-acceptance",
                "Done when… (the agent reads this before it starts)",
                &draft.acceptance,
                Message::TaskAcceptance,
                !draft.sending(),
            ),
            row![
                primary(
                    "task-file-ready",
                    if draft.sending() {
                        "Adding…"
                    } else {
                        "Add to Ready"
                    },
                    ready.then_some(Message::TaskFile(Column::Ready)),
                ),
                ghost(
                    "task-file-backlog",
                    "Keep in Backlog",
                    ready.then_some(Message::TaskFile(Column::Backlog)),
                ),
            ]
            .spacing(4)
            .align_y(Center)
            .wrap(),
        ]
        .spacing(8)
        .width(Fill);
        if let Some(error) = &draft.error {
            form = form.push(text(error.clone()).size(12).color(c.amber));
        }
        container(form).padding([4, 2]).width(Fill).into()
    }

    fn lane<'a>(
        &'a self,
        kind: Column,
        cards: &[&'a Task],
        inline_detail: bool,
        foot: Option<Element<'a, Message>>,
        c: Colors,
    ) -> Element<'a, Message> {
        let header = row![
            dot(lane_tone(kind, c), 8.0, c),
            text(kind.label())
                .size(13)
                .font(weight(iced::font::Weight::Medium)),
            count_chip(cards.len(), false, c),
        ]
        .spacing(8)
        .align_y(Center);
        let mut lane = column![container(header).padding([2, 4])]
            .spacing(8)
            .width(Fill);
        if !cards.is_empty() {
            let mut list = column![].spacing(8).width(Fill);
            for card in cards {
                list = list.push(self.card(card, inline_detail, c));
            }
            lane = lane.push(Space::new().height(2)).push(list);
        }
        if let Some(foot) = foot {
            lane = lane.push(Space::new().height(2)).push(foot);
        }
        container(lane)
            .padding(10)
            .width(Fill)
            .style(move |_| well(c))
            .into()
    }

    /// One card: the title and who holds it; opened in place (a narrow
    /// board), what done means and the ways it can move, inside it.
    fn card(&self, task: &Task, inline_detail: bool, c: Colors) -> Element<'_, Message> {
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
            (self.name_of(a.as_str()), live, held, a.as_str())
        });
        let lapsed =
            |held: bool| !held && matches!(task.column, Column::InProgress | Column::Review);
        let mut head = column![
            text(first_line(&task.title, 80))
                .size(13)
                .font(weight(iced::font::Weight::Medium))
                .wrapping(iced::widget::text::Wrapping::Word),
        ]
        .spacing(6)
        .width(Fill);
        if let Some((name, live, held, id)) = &holder {
            // One line for the name, clipped: a long name never pushes
            // the state off the card or wraps under the dot. The holder's
            // tool mark sits between the dot and the name.
            head = head.push(
                row![
                    dot(if *live { c.green } else { c.faint }, 6.0, c),
                    self.agent_mark_for(id, name, 16.0, c),
                    container(
                        text(name.clone())
                            .size(12)
                            .color(c.muted)
                            .wrapping(iced::widget::text::Wrapping::None)
                    )
                    .width(Fill)
                    .clip(true),
                ]
                .spacing(6)
                .align_y(Center),
            );
            // Its own line: beside the name it clipped the name to a letter.
            if lapsed(*held) {
                head = head.push(status_word("not being worked on", c.amber, c));
            }
        } else if task.column == Column::Ready {
            head = head.push(
                row![
                    dot(c.faint, 6.0, c),
                    text("unassigned").size(12).color(c.muted)
                ]
                .spacing(6)
                .align_y(Center),
            );
        }
        // The control's name says the state too, so a screen reader — and
        // the smoke — hear who holds the card without opening it.
        let label = match &holder {
            Some((name, _, held, _)) => {
                if lapsed(*held) {
                    format!("{} — {name}'s, not being worked on", task.title)
                } else {
                    format!("{} — held by {name}", task.title)
                }
            }
            None if task.column == Column::Ready => format!("{} — unassigned", task.title),
            None => task.title.clone(),
        };
        let in_place = open && inline_detail;
        let message = Message::TaskOpen(id.clone());
        let face = iced::widget::button(head)
            .padding([10, 12])
            .width(Fill)
            .on_press(message.clone())
            .style(move |_, status| card_look(c, status, open, !in_place));
        let face: Element<'_, Message> = crate::controls::Control {
            content: face.into(),
            semantic: crate::accessibility::Semantic::button(
                format!("task-{id}"),
                label,
                Some(message),
            ),
            button: true,
        }
        .into();
        if !in_place {
            return face;
        }
        let mut body = column![face, container(self.acceptance(task, c)).padding([0, 12])]
            .spacing(2)
            .width(Fill);
        if !task.links.is_empty() {
            body = body.push(
                container(super::view::links(&task.links, c))
                    .padding([6, 12])
                    .width(Fill),
            );
        }
        body = body.push(
            container(self.card_actions(task, c))
                .padding(iced::Padding {
                    top: 10.0,
                    right: 12.0,
                    bottom: 10.0,
                    left: 12.0 - GHOST_INSET,
                })
                .width(Fill),
        );
        container(body)
            .width(Fill)
            .style(move |_| container::Style {
                background: Some(c.accent_soft.into()),
                text_color: Some(c.text),
                border: iced::Border {
                    color: c.accent,
                    width: 1.0,
                    radius: RADIUS_MD.into(),
                },
                ..Default::default()
            })
            .into()
    }

    /// When the task counts as done, or that nothing says.
    fn acceptance(&self, task: &Task, c: Colors) -> Element<'_, Message> {
        let acceptance = if task.acceptance.is_empty() {
            "Nothing says when this is done; the agent decides for itself.".to_owned()
        } else {
            format!("Done when: {}", task.acceptance)
        };
        text(acceptance)
            .size(13)
            .color(c.muted)
            .wrapping(iced::widget::text::Wrapping::Word)
            .into()
    }

    /// The open card beneath the lanes: where it is, its title, what done
    /// means and its moves, across the width — a lane is too narrow to
    /// read a paragraph in or to lay the moves out.
    fn card_detail(&self, task: &Task, c: Colors) -> Element<'_, Message> {
        let mut detail = column![
            row![
                dot(lane_tone(task.column, c), 8.0, c),
                text(task.column.label())
                    .size(12)
                    .font(weight(iced::font::Weight::Medium))
                    .color(c.muted)
            ]
            .spacing(8)
            .align_y(Center),
            text(task.title.clone())
                .size(15)
                .font(weight(iced::font::Weight::Semibold))
                .wrapping(iced::widget::text::Wrapping::Word),
            self.acceptance(task, c),
        ]
        .spacing(8)
        .width(Fill);
        if !task.links.is_empty() {
            detail = detail.push(super::view::links(&task.links, c));
        }
        container(column![
            container(detail).padding([14, 16]),
            super::view::rule(c),
            container(self.card_actions(task, c)).padding(iced::Padding {
                top: 10.0,
                right: 16.0,
                bottom: 12.0,
                left: 16.0 - GHOST_INSET,
            }),
        ])
        .width(Fill)
        .style(move |_| c.card_style())
        .into()
    }

    /// Where a card may go from here, who it may be handed to, and off
    /// the board: the person's moves, all of them. Forward is the
    /// outlined move, back a quiet one; the screen's one filled action is
    /// filing.
    fn card_actions(&self, task: &Task, c: Colors) -> Element<'_, Message> {
        let id = task.id.clone();
        let connected = self.connected.is_ok();
        let mut moves = row![].spacing(6).align_y(Center);
        let all = Column::ALL;
        let at = all.iter().position(|k| *k == task.column).unwrap_or(0);
        if at > 0 {
            let back = all[at - 1];
            moves = moves.push(ghost(
                format!("task-back-{id}"),
                format!("‹ {}", back.label()),
                connected.then_some(Message::TaskMove(id.clone(), back)),
            ));
        }
        if at + 1 < all.len() {
            let next = all[at + 1];
            moves = moves.push(action(
                format!("task-next-{id}"),
                format!("{} ›", next.label()),
                connected.then_some(Message::TaskMove(id.clone(), next)),
                false,
            ));
        }
        moves = moves.push(Space::new().width(Fill)).push(custom(
            format!("task-archive-{id}"),
            "Archive",
            text("Archive").size(12).color(c.muted),
            connected.then_some(Message::TaskArchive(id.clone())),
            false,
            Kind::Inline,
            [4, 6],
        ));
        let mut hand = row![text("Hand to").size(12).color(c.faint)]
            .spacing(2)
            .align_y(Center);
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
                Kind::Inline,
                [3, 6],
            ));
        }
        if task.assignee.is_some() {
            anyone = true;
            hand = hand.push(custom(
                format!("task-release-{id}"),
                "nobody",
                text("nobody").size(12).color(c.muted),
                connected.then_some(Message::TaskAssign(id, None)),
                false,
                Kind::Inline,
                [3, 6],
            ));
        }
        // The quiet back move's words sit on the card's text edge; its
        // padding hangs into the margin, so the hand-off line takes the
        // same inset to line up with them.
        let mut rows = column![moves].spacing(6).width(Fill);
        if anyone {
            rows = rows.push(container(hand.wrap()).padding(iced::Padding {
                left: GHOST_INSET,
                ..iced::Padding::ZERO
            }));
        }
        rows.into()
    }
}
