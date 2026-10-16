//! The board: a project's cards in the lanes its board defines, each
//! lane one of the card categories (Backlog, Ready, In progress, Review,
//! Done, Blocked, Cancelled). The person files a card in any lane, drags
//! one between lanes or along one, opens it to edit what it says, talk on
//! it and read its history, approves or sends back what waits for them,
//! and shapes the lanes; an agent pulls from the pull source and the card
//! says who has it.
//!
//! Nothing here is prose. A card's face is its key, its title and marks:
//! a red edge for high priority, a clock tinted by its due day, a mark
//! when it waits on something, a dot per tag, and the holder's mark with
//! its standing; the words for each are its tooltip and the card's
//! spoken name. Every control the pointer drags has a button beside it
//! the keyboard and the workflow driver press.
use super::icons::{Icon, icon};
use super::style::{Colors, RADIUS_MD, alpha, identity, mix, weight};
use super::view::{count_chip, dot, first_line, said, standing_glyph, status_dot, tip};
use super::*;
use crate::controls::{
    Kind, button as action, custom_sized, ghost, input_enabled, input_submitting, primary,
};
use agentdocker_core::board::{self, DueState, Owner};
use agentdocker_core::task::TaskAction;
use agentdocker_core::{Column, Lane, Priority, Size, Task, TaskId};
use iced::{
    Center, Element, Fill, Top,
    widget::{Space, column, container, mouse_area, row, text},
};

/// The least a lane needs beside the others — a card's words wrap below
/// this rather than the lanes sharing it.
const LANE_WIDTH: f32 = 150.0;

/// The well's corner: a step past a card's, so the cards nest in it.
const WELL_RADIUS: f32 = 12.0;

/// A lane's own tone, for its glyph.
fn lane_tone(kind: Column, c: Colors) -> iced::Color {
    match kind {
        Column::Backlog => c.faint,
        Column::Ready => c.accent,
        Column::InProgress => c.cyan,
        Column::Review => c.amber,
        Column::Done => c.green,
        Column::Blocked => c.red,
        Column::Cancelled => c.faint,
    }
}

/// A lane's glyph: a ring that fills as work moves right, as Linear
/// draws status; Blocked is the alert, Cancelled the cross.
fn lane_glyph(kind: Column) -> Icon {
    match kind {
        Column::Backlog => Icon::Ring,
        Column::Ready => Icon::LaneEmpty,
        Column::InProgress => Icon::LaneHalf,
        Column::Review => Icon::LaneMost,
        Column::Done => Icon::LaneDone,
        Column::Blocked => Icon::Alert,
        Column::Cancelled => Icon::Close,
    }
}

/// The well a lane's cards sit in: a step of tint off the page; a lane a
/// card is dragged over takes the accent edge, one over its limit amber.
fn well(c: Colors, target: bool, over: bool) -> container::Style {
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
            width: if target || over { 1.5 } else { 0.0 },
            color: if target {
                c.accent
            } else if over {
                c.amber
            } else {
                iced::Color::TRANSPARENT
            },
        },
        ..Default::default()
    }
}

/// A tag's colour: the project-mark wheel, keyed by the tag.
fn tag_tone(tag: &str, c: Colors) -> iced::Color {
    identity(tag, c.dark).1
}

/// A lane's name on a board, by slug; the slug when it is gone.
fn lane_name(board: &agentdocker_core::Board, slug: &str) -> String {
    board
        .lane(slug)
        .map(|l| l.name.clone())
        .unwrap_or_else(|| slug.to_owned())
}

impl App {
    /// The board on view for the selected project: the one chosen, else
    /// its main board, else (before the boards are read) the classic
    /// lanes every project starts with.
    pub(super) fn board_on_view(&self) -> Option<agentdocker_core::Board> {
        let project = self.selected_project_root()?;
        let chosen = self.shell.board_choice.get(&project).map(String::as_str);
        if let Some(boards) = self.boards.get(&project) {
            let wanted = chosen.unwrap_or(board::MAIN);
            if let Some(found) = boards
                .iter()
                .find(|b| b.slug == wanted)
                .or_else(|| boards.iter().find(|b| b.slug == board::MAIN))
            {
                return Some(found.clone());
            }
        }
        let id = self.selected_project_id().unwrap_or_default();
        let name = self
            .shell
            .catalog
            .selected()
            .map(|e| e.name())
            .unwrap_or_default();
        Some(agentdocker_core::Board::main(
            agentdocker_core::ProjectId::from(id.as_str()),
            &name,
            Utc::now(),
        ))
    }

    /// A card's fields as the editor starts them.
    pub(super) fn draft_of(&self, id: &TaskId) -> super::shell::CardDraft {
        let card = self
            .tasks
            .as_ref()
            .and_then(|b| b.cards.iter().find(|t| &t.id == id));
        match card {
            Some(task) => super::shell::CardDraft {
                task: Some(task.id.clone()),
                title: task.title.clone(),
                description: task.description.clone(),
                acceptance: task.acceptance.clone(),
                due: task.due.map(|d| d.to_string()).unwrap_or_default(),
                blocker: task.external_blocker.clone(),
                tags: task.tags.join(" "),
                comment: String::new(),
            },
            None => super::shell::CardDraft {
                task: Some(id.clone()),
                ..Default::default()
            },
        }
    }

    /// What the editor changes on a card: the fields that differ from it.
    pub(super) fn card_change(&self, id: &TaskId) -> Option<super::CardChange> {
        let task = self
            .card
            .as_ref()
            .map(|d| &d.task)
            .filter(|t| &t.id == id)
            .or_else(|| {
                self.tasks
                    .as_ref()
                    .and_then(|b| b.cards.iter().find(|t| &t.id == id))
            })?;
        let draft = &self.shell.card_draft;
        if draft.task.as_ref() != Some(id) {
            return None;
        }
        let changed =
            |now: &str, was: &str| (now.trim() != was.trim()).then(|| now.trim().to_owned());
        let mut tags: Vec<String> = draft
            .tags
            .split([' ', ','])
            .map(agentdocker_core::task::tag_of)
            .filter(|t| !t.is_empty())
            .collect();
        tags.sort();
        tags.dedup();
        let change = super::CardChange {
            title: changed(&draft.title, &task.title).filter(|t| !t.is_empty()),
            description: changed(&draft.description, &task.description),
            acceptance: changed(&draft.acceptance, &task.acceptance),
            due: changed(
                &draft.due,
                &task.due.map(|d| d.to_string()).unwrap_or_default(),
            ),
            external_blocker: changed(&draft.blocker, &task.external_blocker),
            tags: (tags != task.tags).then_some(tags),
            ..Default::default()
        };
        (change != super::CardChange::default()).then_some(change)
    }

    /// Whether a card passes the board's search words and toggles.
    fn shown(&self, task: &Task, board: &agentdocker_core::Board) -> bool {
        let filters = self.shell.board_filters;
        let words = self.shell.board_query.trim().to_lowercase();
        let lane = board.lane_of(task);
        (words.is_empty()
            || task.key.to_lowercase() == words
            || task.title.to_lowercase().contains(&words)
            || task.description.to_lowercase().contains(&words)
            || task.tags.iter().any(|t| t.contains(&words)))
            && (!filters.high || task.priority == Priority::High)
            && (!filters.blocked
                || !task.external_blocker.trim().is_empty()
                || !task.blocked_by.is_empty())
            && (!filters.waiting || (lane.owner == Owner::Human && lane.category == Column::Review))
            && (!filters.unassigned || task.assignee.is_none())
    }

    /// Cards in lanes that wait for the person, on the board on view: the
    /// Board tab's count.
    pub(super) fn board_waiting(&self) -> usize {
        let Some(board) = self.board_on_view() else {
            return 0;
        };
        let Some(project) = self.selected_project_root() else {
            return 0;
        };
        self.tasks
            .as_ref()
            .filter(|b| b.project == project)
            .map_or(0, |b| {
                b.cards
                    .iter()
                    .filter(|t| t.board.as_deref().unwrap_or(board::MAIN) == board.slug)
                    .filter(|t| {
                        let lane = board.lane_of(t);
                        lane.owner == Owner::Human && lane.category == Column::Review
                    })
                    .count()
            })
    }

    pub(super) fn board_view(&self, c: Colors) -> Element<'_, Message> {
        let Some(entry) = self.shell.catalog.selected() else {
            return super::view::empty(None, c);
        };
        let Some(board) = self.board_on_view() else {
            return super::view::empty(None, c);
        };
        let root = entry.project.root.display().to_string();
        let loaded = self.tasks.as_ref().filter(|b| b.project == root);
        let all: &[Task] = loaded.map_or(&[], |b| &b.cards);
        let more = loaded.is_some_and(|b| b.more);
        let loading = loaded.is_some_and(Board::loading_more)
            || self.board_asks.values().any(|(p, _)| *p == root);
        let on_board: Vec<&Task> = all
            .iter()
            .filter(|t| t.board.as_deref().unwrap_or(board::MAIN) == board.slug)
            .collect();
        let mut page = column![self.board_toolbar(&root, &board, c)]
            .spacing(14)
            .width(Fill);
        if let Some(settings) = &self.shell.lane_settings {
            page = page.push(self.lane_settings(settings, c));
        }
        let lanes_fit = board.lanes.len() as f32 * LANE_WIDTH
            + (board.lanes.len().saturating_sub(1)) as f32 * 12.0;
        let narrow = self.narrow() || self.panes.workspace_width() < lanes_fit;
        let mut lanes = column![].spacing(12).width(Fill);
        let mut side_by_side = row![].spacing(12).align_y(Top).width(Fill);
        for lane in &board.lanes {
            let mut cards: Vec<&Task> = on_board
                .iter()
                .copied()
                .filter(|t| board.lane_of(t).slug == lane.slug)
                .collect();
            cards.sort_by(|a, b| board::order(a, b));
            let count = cards.len();
            let shown: Vec<&Task> = cards
                .into_iter()
                .filter(|t| self.shown(t, &board))
                .collect();
            let column_view = self.lane(&board, lane, &shown, count, narrow, c);
            if narrow {
                lanes = lanes.push(column_view);
            } else {
                side_by_side = side_by_side.push(container(column_view).width(Fill));
            }
        }
        if !narrow {
            lanes = lanes.push(side_by_side);
            if let Some(open) = self
                .task_open
                .as_ref()
                .and_then(|id| on_board.iter().find(|t| &t.id == id))
            {
                lanes = lanes.push(self.card_detail(open, &board, c));
            }
        }
        // A drag ends wherever the pointer lets go: over a lane it lands
        // there, on the card it began at it opens it, elsewhere it is put
        // back.
        let lanes: Element<'_, Message> = if self.shell.dragging.is_some() {
            mouse_area(lanes)
                .on_release(Message::DragDrop)
                .on_exit(Message::DragCancel)
                .into()
        } else {
            lanes.into()
        };
        page = page.push(lanes);
        if more && all.len() < BOARD_KEEP {
            page = page.push(action(
                "board-more",
                if loading { "Loading…" } else { "Show more" },
                (self.connected.is_ok() && !loading).then_some(Message::TasksMore),
                false,
            ));
        }
        page.into()
    }

    /// Above the lanes: the boards to choose from, the search, the
    /// filters as marks, a new board and the lanes' settings.
    fn board_toolbar(
        &self,
        root: &str,
        board: &agentdocker_core::Board,
        c: Colors,
    ) -> Element<'_, Message> {
        let mut bar = row![].spacing(6).align_y(Center).width(Fill);
        let boards = self.boards.get(root).cloned().unwrap_or_default();
        if boards.len() > 1 {
            let mut chips = Vec::new();
            for each in &boards {
                chips.push(crate::controls::segment(
                    format!("board-{}", each.slug),
                    each.name.clone(),
                    Some(Message::BoardPick(each.slug.clone())),
                    each.slug == board.slug,
                ));
            }
            bar = bar.push(super::view::segmented(chips, c));
        }
        bar = bar.push(
            container(input_enabled(
                "board-search",
                "Search",
                &self.shell.board_query,
                Message::BoardQuery,
                true,
            ))
            .width(200),
        );
        let filters = self.shell.board_filters;
        let toggle = |id: &'static str, glyph: Icon, words: &'static str, on: bool, kind| {
            tip(
                custom_sized(
                    id,
                    words,
                    container(icon(glyph, if on { c.text } else { c.muted }, 14.0)).center(18),
                    Some(Message::BoardFilter(kind)),
                    on,
                    Kind::Ghost,
                    [6, 6],
                    iced::Length::Shrink,
                ),
                words,
                c,
            )
        };
        use super::shell::BoardFilterKind as F;
        bar = bar
            .push(toggle(
                "board-filter-high",
                Icon::Alert,
                "High priority",
                filters.high,
                F::High,
            ))
            .push(toggle(
                "board-filter-blocked",
                Icon::Hourglass,
                "Waiting on something",
                filters.blocked,
                F::Blocked,
            ))
            .push(toggle(
                "board-filter-waiting",
                Icon::Question,
                "Waiting for you",
                filters.waiting,
                F::Waiting,
            ))
            .push(toggle(
                "board-filter-unassigned",
                Icon::Ring,
                "Nobody holds it",
                filters.unassigned,
                F::Unassigned,
            ))
            .push(Space::new().width(Fill))
            .push(tip(
                custom_sized(
                    "board-new",
                    "New agent workflow board",
                    container(icon(Icon::Add, c.muted, 14.0)).center(18),
                    self.connected
                        .is_ok()
                        .then(|| Message::BoardNew("agents".into())),
                    false,
                    Kind::Ghost,
                    [6, 6],
                    iced::Length::Shrink,
                ),
                "New agent workflow board",
                c,
            ))
            .push(tip(
                custom_sized(
                    "board-lanes",
                    "Lanes",
                    container(icon(Icon::Settings, c.muted, 14.0)).center(18),
                    Some(Message::LaneSettings),
                    self.shell.lane_settings.is_some(),
                    Kind::Ghost,
                    [6, 6],
                    iced::Length::Shrink,
                ),
                "Lanes",
                c,
            ));
        bar.into()
    }

    /// The lanes as the person reshapes them: each its name, who it is
    /// for, its limit, its place, and whether agents pull from or into
    /// it; lanes to add by category; save.
    fn lane_settings<'a>(
        &'a self,
        settings: &'a agentdocker_core::Board,
        c: Colors,
    ) -> Element<'a, Message> {
        let small_icon = move |id: String, words: &'static str, glyph: Icon, message| {
            tip(
                custom_sized(
                    id,
                    words,
                    container(icon(glyph, c.muted, 12.0)).center(16),
                    Some(message),
                    false,
                    Kind::Ghost,
                    [4, 4],
                    iced::Length::Shrink,
                ),
                words,
                c,
            )
        };
        let mut rows = column![].spacing(6).width(Fill);
        for (index, lane) in settings.lanes.iter().enumerate() {
            let pull = lane.slug == settings.pull_source || lane.slug == settings.pull_target;
            let owner = |label: &'static str, value: Owner| {
                crate::controls::segment(
                    format!("lane-owner-{index}-{label}"),
                    label,
                    Some(Message::LaneOwner(index, value.clone())),
                    lane.owner == value,
                )
            };
            let mut line = row![
                tip(
                    icon(lane_glyph(lane.category), lane_tone(lane.category, c), 14.0),
                    lane.category.label(),
                    c
                ),
                container(input_enabled(
                    format!("lane-name-{index}"),
                    "Name",
                    &lane.name,
                    move |name| Message::LaneName(index, name),
                    true,
                ))
                .width(Fill),
                super::view::segmented(
                    vec![
                        owner("Anyone", Owner::Any),
                        owner("You", Owner::Human),
                        owner("Agents", Owner::Agent),
                    ],
                    c
                ),
                container(input_enabled(
                    format!("lane-wip-{index}"),
                    "Limit",
                    &lane.wip.map(|w| w.to_string()).unwrap_or_default(),
                    move |words| Message::LaneWip(index, words),
                    true,
                ))
                .width(64),
            ]
            .spacing(8)
            .align_y(Center);
            if matches!(lane.category, Column::Ready | Column::InProgress) {
                let words = if lane.category == Column::Ready {
                    "Agents pull from here"
                } else {
                    "A pull lands here"
                };
                line = line.push(tip(
                    custom_sized(
                        format!("lane-pull-{index}"),
                        words,
                        container(icon(
                            Icon::Handoff,
                            if pull { c.accent } else { c.faint },
                            12.0,
                        ))
                        .center(16),
                        Some(Message::LanePull(index)),
                        pull,
                        Kind::Ghost,
                        [4, 4],
                        iced::Length::Shrink,
                    ),
                    words,
                    c,
                ));
            }
            line = line
                .push(small_icon(
                    format!("lane-up-{index}"),
                    "Move up",
                    Icon::ChevronUp,
                    Message::LaneMove(index, true),
                ))
                .push(small_icon(
                    format!("lane-down-{index}"),
                    "Move down",
                    Icon::ChevronDown,
                    Message::LaneMove(index, false),
                ))
                .push(small_icon(
                    format!("lane-remove-{index}"),
                    "Remove lane",
                    Icon::Close,
                    Message::LaneRemove(index),
                ));
            rows = rows.push(line);
        }
        let mut add = row![].spacing(4).align_y(Center);
        for category in Column::CATEGORIES {
            add = add.push(tip(
                custom_sized(
                    format!("lane-add-{}", category.as_str()),
                    format!("Add a {} lane", category.label()),
                    row![
                        icon(Icon::Add, c.faint, 10.0),
                        icon(lane_glyph(category), lane_tone(category, c), 13.0)
                    ]
                    .spacing(2)
                    .align_y(Center),
                    Some(Message::LaneAdd(category)),
                    false,
                    Kind::Ghost,
                    [4, 6],
                    iced::Length::Shrink,
                ),
                category.label(),
                c,
            ));
        }
        let footer = row![
            add.wrap(),
            Space::new().width(Fill),
            ghost(
                "lane-settings-cancel",
                "Cancel",
                Some(Message::LaneSettings)
            ),
            primary(
                "lane-settings-save",
                "Save",
                self.connected.is_ok().then_some(Message::LaneSettingsSave)
            ),
        ]
        .spacing(8)
        .align_y(Center);
        container(column![rows, footer].spacing(12))
            .padding([12, 14])
            .width(Fill)
            .style(move |_| c.card_style())
            .into()
    }

    fn lane<'a>(
        &'a self,
        board: &agentdocker_core::Board,
        lane: &Lane,
        cards: &[&'a Task],
        count: usize,
        inline_detail: bool,
        c: Colors,
    ) -> Element<'a, Message> {
        let slug = lane.slug.clone();
        let over = lane.over(count);
        let mut header = row![
            tip(
                icon(lane_glyph(lane.category), lane_tone(lane.category, c), 14.0),
                format!(
                    "{}{}",
                    lane.category.label(),
                    if lane.slug == board.pull_source {
                        " · agents pull from here"
                    } else {
                        ""
                    }
                ),
                c
            ),
            text(lane.name.clone())
                .size(13)
                .font(weight(iced::font::Weight::Medium)),
            said(
                format!("lane-count-{slug}"),
                match lane.wip {
                    Some(wip) => format!("{count} of {wip}"),
                    None => count.to_string(),
                },
                count_chip(count, over, c),
            ),
            Space::new().width(Fill),
        ]
        .spacing(8)
        .align_y(Center);
        if lane.category.terminal() && count > 0 {
            header = header.push(tip(
                custom_sized(
                    format!("lane-archive-{slug}"),
                    format!("Archive every card in {}", lane.name),
                    container(icon(Icon::Download, c.faint, 12.0)).center(16),
                    self.connected
                        .is_ok()
                        .then(|| Message::ArchiveLane(slug.clone())),
                    false,
                    Kind::Ghost,
                    [4, 4],
                    iced::Length::Shrink,
                ),
                "Archive all",
                c,
            ));
        }
        header = header.push(tip(
            custom_sized(
                format!("lane-add-card-{slug}"),
                format!("Add a card to {}", lane.name),
                container(icon(Icon::Add, c.muted, 13.0)).center(16),
                self.connected
                    .is_ok()
                    .then(|| Message::QuickAdd(Some(slug.clone()))),
                false,
                Kind::Ghost,
                [4, 4],
                iced::Length::Shrink,
            ),
            "Add a card",
            c,
        ));
        let mut body = column![container(header).padding([2, 4])]
            .spacing(8)
            .width(Fill);
        if let Some((open, words)) = &self.shell.quick_add
            && *open == slug
        {
            body = body.push(input_submitting(
                format!("quick-add-{slug}"),
                "Title",
                words,
                Message::QuickAddText,
                true,
                (!words.trim().is_empty()).then_some(Message::QuickAddFile),
            ));
        }
        for card in cards {
            body = body.push(self.card(card, board, inline_detail, c));
        }
        let target = self
            .shell
            .drag_target
            .as_ref()
            .is_some_and(|(lane, _)| *lane == slug);
        let well_view = container(body)
            .padding(10)
            .width(Fill)
            .style(move |_| well(c, target, over));
        if self.shell.dragging.is_some() {
            mouse_area(well_view)
                .on_enter(Message::DragOver(slug.clone(), None))
                .into()
        } else {
            well_view.into()
        }
    }

    /// One card: its key, title and marks; opened in place on a narrow
    /// board, with its editor under it.
    fn card(
        &self,
        task: &Task,
        board: &agentdocker_core::Board,
        inline_detail: bool,
        c: Colors,
    ) -> Element<'_, Message> {
        let id = task.id.clone();
        let open = self.task_open.as_ref() == Some(&id);
        let lane = board.lane_of(task);
        // The holding is the task:<id> lease: while the assignee holds it
        // the card is theirs; once it has lapsed the card is still theirs
        // by name but the next agent to pull it takes it over.
        let resource = format!("task:{id}");
        let holder = task.assignee.as_ref().map(|a| {
            let held = self.leases.iter().any(|l| {
                l.resource.as_str() == resource
                    && &l.holder == a
                    && l.mode == agentdocker_core::LeaseMode::Exclusive
            });
            (self.name_of(a.as_str()), held, a.as_str())
        });
        let lapsed = |held: bool| !held && task.column.working();
        let today = chrono::Local::now().date_naive();
        let mut marks = row![].spacing(5).align_y(Center);
        let mut words: Vec<String> = Vec::new();
        if let Some(due) = task.due {
            let (tone, state) = match board::due_state(due, today) {
                DueState::Overdue => (c.red, "overdue"),
                DueState::Today => (c.amber, "due today"),
                DueState::Later => (c.faint, "due"),
            };
            marks = marks.push(icon(Icon::Clock, tone, 12.0));
            words.push(format!("{state} {}", due.format("%b %-d")));
        }
        if !task.external_blocker.trim().is_empty() || !task.blocked_by.is_empty() {
            marks = marks.push(icon(Icon::Hourglass, c.amber, 12.0));
            words.push("waiting on something".into());
        }
        if task.priority == Priority::High {
            words.push("high priority".into());
        }
        if let Some(size) = task.size {
            words.push(format!("size {}", size.as_str()));
        }
        for tag in task.tags.iter().take(4) {
            marks = marks.push(dot(tag_tone(tag, c), 6.0, c));
        }
        if !task.tags.is_empty() {
            words.push(task.tags.join(", "));
        }
        let mut foot = row![container(marks).width(Fill)]
            .spacing(6)
            .align_y(Center);
        match &holder {
            Some((name, held, agent)) => {
                let record = self.agents.iter().find(|r| r.id.as_str() == *agent);
                let standing: Element<'_, Message> = if lapsed(*held) {
                    tip(icon(Icon::Alert, c.amber, 11.0), "Not being worked on", c)
                } else {
                    match record {
                        Some(record) => standing_glyph(self.standing(record), c),
                        None => dot(c.faint, 6.0, c),
                    }
                };
                let mut who = row![
                    standing,
                    tip(self.agent_mark_for(agent, name, 16.0, c), name.clone(), c),
                ]
                .spacing(4)
                .align_y(Center);
                if let Some(chip) = record.and_then(|r| self.pull_request_chip(r, c)) {
                    who = who.push(chip);
                }
                foot = foot.push(who);
            }
            None if lane.slug == board.pull_source => {
                foot = foot.push(tip(dot(c.accent, 6.0, c), "Ready to pull", c));
            }
            None => {}
        }
        let head = column![
            text(task.key.clone())
                .size(11)
                .font(iced::Font::MONOSPACE)
                .color(c.faint),
            text(first_line(&task.title, 120))
                .size(13)
                .font(weight(iced::font::Weight::Medium))
                .wrapping(iced::widget::text::Wrapping::Word),
            foot,
        ]
        .spacing(5)
        .width(Fill);
        // The control's name says what the marks show, so a screen
        // reader — and the smoke — hear the card whole.
        let mut label = format!("{} {}", task.handle(), task.title);
        match &holder {
            Some((name, held, _)) if lapsed(*held) => {
                label.push_str(&format!(" — {name}'s, not being worked on"));
            }
            Some((name, _, _)) => label.push_str(&format!(" — held by {name}")),
            None if lane.slug == board.pull_source => label.push_str(" — unassigned"),
            None => {}
        }
        if !words.is_empty() {
            label.push_str(&format!(" · {}", words.join(" · ")));
        }
        let flashing = self
            .flashes
            .get(&id)
            .is_some_and(|at| at.elapsed() < FLASH_FOR);
        let dragged = self.shell.dragging.as_ref() == Some(&id);
        let marker = self
            .shell
            .drag_target
            .as_ref()
            .is_some_and(|(l, before)| *l == lane.slug && before.as_ref() == Some(&id));
        let high = task.priority == Priority::High;
        let in_place = open && inline_detail;
        let ready = task.assignee.is_none() && lane.slug == board.pull_source;
        let face = container(head)
            .padding(iced::Padding {
                top: 9.0,
                right: 11.0,
                bottom: 9.0,
                left: 11.0,
            })
            .width(Fill)
            .style(move |_| {
                let fill = if c.dark {
                    mix(c.card, c.raised, 0.5)
                } else {
                    c.card
                };
                container::Style {
                    background: Some(
                        if flashing || open {
                            mix(fill, c.accent, if c.dark { 0.12 } else { 0.07 })
                        } else if dragged {
                            alpha(fill, 0.5)
                        } else {
                            fill
                        }
                        .into(),
                    ),
                    text_color: Some(c.text),
                    border: iced::Border {
                        color: if high {
                            c.red
                        } else if open || ready {
                            alpha(c.accent, if open { 1.0 } else { 0.45 })
                        } else {
                            c.line
                        },
                        width: if high { 1.5 } else { 1.0 },
                        radius: RADIUS_MD.into(),
                    },
                    ..Default::default()
                }
            });
        // Pressed, the card is picked up; let go where it was, it opens.
        let message = Message::TaskOpen(id.clone());
        let pointer = mouse_area(face)
            .on_press(Message::DragStart(id.clone()))
            .on_enter(Message::DragOver(lane.slug.clone(), Some(id.clone())))
            .interaction(iced::mouse::Interaction::Pointer);
        let control: Element<'_, Message> = crate::controls::Control {
            content: pointer.into(),
            semantic: crate::accessibility::Semantic::button(
                format!("task-{id}"),
                label,
                Some(message),
            ),
            button: true,
        }
        .into();
        let control: Element<'_, Message> = if marker {
            column![
                container(Space::new().width(Fill).height(2)).style(move |_| {
                    container::Style {
                        background: Some(c.accent.into()),
                        ..Default::default()
                    }
                }),
                control
            ]
            .spacing(3)
            .into()
        } else {
            control
        };
        if !in_place {
            return control;
        }
        column![control, self.card_detail(task, board, c)]
            .spacing(6)
            .into()
    }

    /// The open card: its fields to edit, its moves, the approval when it
    /// waits for the person, its links, the cards around it, its comments
    /// and its history.
    fn card_detail(
        &self,
        task: &Task,
        board: &agentdocker_core::Board,
        c: Colors,
    ) -> Element<'_, Message> {
        use super::shell::CardFieldKind as K;
        let id = task.id.clone();
        let lane = board.lane_of(task);
        let connected = self.connected.is_ok();
        let draft = &self.shell.card_draft;
        let icon_button =
            |control: String, words: String, glyph: Icon, tone, message: Option<Message>| {
                tip(
                    custom_sized(
                        control,
                        words.clone(),
                        container(icon(glyph, tone, 14.0)).center(18),
                        message,
                        false,
                        Kind::Ghost,
                        [6, 6],
                        iced::Length::Shrink,
                    ),
                    words,
                    c,
                )
            };
        // Head: where it is, and what to do with it.
        let mut head = row![
            text(task.handle())
                .size(12)
                .font(iced::Font::MONOSPACE)
                .color(c.faint),
            icon(lane_glyph(lane.category), lane_tone(lane.category, c), 13.0),
            text(lane.name.clone())
                .size(12)
                .font(weight(iced::font::Weight::Medium))
                .color(c.muted),
            Space::new().width(Fill),
        ]
        .spacing(6)
        .align_y(Center);
        let at = board
            .lanes
            .iter()
            .position(|l| l.slug == lane.slug)
            .unwrap_or(0);
        if at > 0 {
            let back = &board.lanes[at - 1];
            head = head.push(icon_button(
                format!("task-back-{id}"),
                format!("To {}", back.name),
                Icon::ChevronLeft,
                c.muted,
                connected.then(|| Message::TaskMove(id.clone(), back.slug.clone())),
            ));
        }
        if at + 1 < board.lanes.len() {
            let next = &board.lanes[at + 1];
            head = head.push(icon_button(
                format!("task-next-{id}"),
                format!("To {}", next.name),
                Icon::ChevronRight,
                c.muted,
                connected.then(|| Message::TaskMove(id.clone(), next.slug.clone())),
            ));
        }
        // Waiting for the person: approve, or send back to be pulled
        // again with what was typed below as the reason.
        if lane.owner == Owner::Human && lane.category == Column::Review {
            head = head
                .push(icon_button(
                    format!("task-approve-{id}"),
                    "Approve".into(),
                    Icon::Check,
                    c.green,
                    connected.then(|| Message::TaskApprove(id.clone())),
                ))
                .push(icon_button(
                    format!("task-bounce-{id}"),
                    "Send back".into(),
                    Icon::ChevronLeft,
                    c.amber,
                    connected.then(|| Message::TaskBounce(id.clone())),
                ));
        }
        head = head
            .push(icon_button(
                format!("task-archive-{id}"),
                "Archive".into(),
                Icon::Download,
                c.faint,
                connected.then(|| Message::TaskArchive(id.clone())),
            ))
            .push(icon_button(
                format!("task-close-{id}"),
                "Close".into(),
                Icon::Close,
                c.muted,
                Some(Message::TaskOpen(id.clone())),
            ));
        // The words, edited in place; Save sends what changed.
        let changed = self.card_change(&id).is_some();
        let fields = column![
            input_enabled(
                "card-title",
                "Title",
                &draft.title,
                |words| Message::CardField(K::Title, words),
                connected
            ),
            crate::controls::composer(
                "card-description",
                format!("card-{id}"),
                "Description",
                &draft.description,
                |words| Message::CardField(K::Description, words),
                connected,
                None,
            ),
            input_enabled(
                "card-acceptance",
                "Done when…",
                &draft.acceptance,
                |words| Message::CardField(K::Acceptance, words),
                connected
            ),
            row![
                container(input_enabled(
                    "card-due",
                    "Due",
                    &draft.due,
                    |words| Message::CardField(K::Due, words),
                    connected
                ))
                .width(120),
                container(input_enabled(
                    "card-blocker",
                    "Waiting on…",
                    &draft.blocker,
                    |words| Message::CardField(K::Blocker, words),
                    connected
                ))
                .width(Fill),
                container(input_enabled(
                    "card-tags",
                    "Tags",
                    &draft.tags,
                    |words| Message::CardField(K::Tags, words),
                    connected
                ))
                .width(Fill),
            ]
            .spacing(8)
            .align_y(Center),
        ]
        .spacing(8);
        let priority = |label: &'static str, value: Priority| {
            crate::controls::segment(
                format!("card-priority-{}", value.as_str()),
                label,
                connected.then_some(Message::CardPriority(value)),
                task.priority == value,
            )
        };
        let size = |label: &'static str, value: Option<Size>| {
            crate::controls::segment(
                format!("card-size-{}", value.map(|s| s.as_str()).unwrap_or("none")),
                label,
                connected.then_some(Message::CardSize(value)),
                task.size == value,
            )
        };
        let mut settings = row![
            super::view::segmented(
                vec![
                    priority("High", Priority::High),
                    priority("Normal", Priority::Normal),
                    priority("Low", Priority::Low),
                ],
                c
            ),
            super::view::segmented(
                vec![
                    size("S", Some(Size::S)),
                    size("M", Some(Size::M)),
                    size("L", Some(Size::L)),
                    size("–", None),
                ],
                c
            ),
            Space::new().width(Fill),
        ]
        .spacing(8)
        .align_y(Center);
        if changed {
            settings = settings.push(primary(
                "card-save",
                "Save",
                connected.then_some(Message::CardSave),
            ));
        }
        // Hand it to a running agent, or to nobody: their marks, named on
        // the pointer.
        let mut hand = row![].spacing(2).align_y(Center);
        for agent in self
            .agents
            .iter()
            .filter(|a| a.status.is_live() && !self.is_human(a.id.as_str()))
            .filter(|a| self.has_project(a.project.as_ref()))
        {
            if task.assignee.as_ref() == Some(&agent.id) {
                continue;
            }
            let name = self.name_of(agent.id.as_str());
            hand = hand.push(tip(
                custom_sized(
                    format!("task-hand-{id}-{}", agent.id),
                    format!("Hand to {name}"),
                    self.agent_mark_for(agent.id.as_str(), &name, 18.0, c),
                    connected.then(|| Message::TaskAssign(id.clone(), Some(agent.id.clone()))),
                    false,
                    Kind::Ghost,
                    [3, 3],
                    iced::Length::Shrink,
                ),
                format!("Hand to {name}"),
                c,
            ));
        }
        if task.assignee.is_some() {
            hand = hand.push(icon_button(
                format!("task-release-{id}"),
                "Hand to nobody".into(),
                Icon::Leave,
                c.muted,
                connected.then(|| Message::TaskAssign(id.clone(), None)),
            ));
        }
        let mut body = column![head, fields, settings, hand.wrap()]
            .spacing(12)
            .width(Fill);
        if !task.links.is_empty() {
            body = body.push(super::view::links(&task.links, c));
        }
        // What the daemon knows beyond the card: the cards around it, the
        // talk on it and what happened to it.
        if let Some(detail) = self.card.as_ref().filter(|d| d.task.id == id) {
            let related =
                |list: &[Task], glyph: Icon, words: &'static str| -> Option<Element<'_, Message>> {
                    (!list.is_empty()).then(|| {
                        let mut rows = column![].spacing(3);
                        for other in list {
                            rows = rows.push(
                                row![
                                    tip(icon(glyph, c.faint, 12.0), words, c),
                                    text(other.handle())
                                        .size(11)
                                        .font(iced::Font::MONOSPACE)
                                        .color(c.faint),
                                    text(first_line(&other.title, 80)).size(12),
                                    status_dot(
                                        format!("related-{}", other.id),
                                        lane_tone(other.column, c),
                                        other.column.label(),
                                        c
                                    ),
                                ]
                                .spacing(6)
                                .align_y(Center),
                            );
                        }
                        rows.into()
                    })
                };
            if let Some(blockers) = related(&detail.blockers, Icon::Hourglass, "Waits on") {
                body = body.push(blockers);
            }
            if let Some(children) = related(&detail.children, Icon::Board, "Part of this") {
                body = body.push(children);
            }
            let mut thread = column![].spacing(8);
            for comment in &detail.comments {
                let name = self.name_of(&comment.author);
                let words = if comment.deleted_at.is_some() {
                    text("—").size(13).color(c.faint)
                } else {
                    text(comment.body.clone()).size(13)
                };
                thread = thread.push(
                    row![
                        tip(
                            self.agent_mark_for(&comment.author, &name, 18.0, c),
                            format!(
                                "{name} · {}",
                                comment
                                    .created_at
                                    .with_timezone(&chrono::Local)
                                    .format("%a %-d %b, %H:%M")
                            ),
                            c
                        ),
                        container(words).width(Fill),
                    ]
                    .spacing(8)
                    .align_y(Top),
                );
            }
            body = body.push(thread);
            let mut history = column![].spacing(3);
            for entry in detail.history.iter().rev().take(12) {
                let (glyph, words) = match &entry.action {
                    TaskAction::Created { lane } => (Icon::Add, lane_name(&detail.board, lane)),
                    TaskAction::Pulled { lane, .. } => {
                        (Icon::Handoff, lane_name(&detail.board, lane))
                    }
                    TaskAction::Moved { to, .. } => {
                        (Icon::ChevronRight, lane_name(&detail.board, to))
                    }
                    TaskAction::Updated { fields } => (Icon::Note, fields.join(", ")),
                    TaskAction::Assigned { to } => (
                        Icon::Handoff,
                        to.as_deref().map(|a| self.name_of(a)).unwrap_or_default(),
                    ),
                    TaskAction::Commented { .. } => (Icon::Channels, String::new()),
                    TaskAction::Archived => (Icon::Download, String::new()),
                    TaskAction::Unarchived => (Icon::Join, String::new()),
                };
                let who = self.name_of(&entry.by);
                history = history.push(tip(
                    row![
                        icon(glyph, c.faint, 11.0),
                        text(who).size(11).color(c.muted),
                        text(words).size(11).color(c.faint),
                    ]
                    .spacing(6)
                    .align_y(Center),
                    entry
                        .at
                        .with_timezone(&chrono::Local)
                        .format("%a %-d %b, %H:%M")
                        .to_string(),
                    c,
                ));
            }
            body = body.push(history);
        }
        // Say something on it; the agent holding it hears.
        let can_comment = connected && !draft.comment.trim().is_empty();
        body = body.push(
            row![
                container(input_submitting(
                    "card-comment",
                    "Comment",
                    &draft.comment,
                    |words| Message::CardField(K::Comment, words),
                    connected,
                    can_comment.then_some(Message::CardCommentSend),
                ))
                .width(Fill),
                action(
                    "card-comment-send",
                    "Send",
                    can_comment.then_some(Message::CardCommentSend),
                    false
                ),
            ]
            .spacing(8)
            .align_y(Center),
        );
        container(body)
            .padding([14, 16])
            .width(Fill)
            .style(move |_| c.card_style())
            .into()
    }
}
