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

/// A catalog colour in the theme's tones.
pub(super) fn tone_of(color: agentdocker_core::TagColor, c: Colors) -> iced::Color {
    use agentdocker_core::TagColor as T;
    match color {
        T::Gray => c.faint,
        T::Red => c.red,
        T::Orange => mix(c.red, c.amber, 0.5),
        T::Yellow => c.amber,
        T::Green => c.green,
        T::Teal => mix(c.green, c.accent, 0.5),
        T::Blue => c.accent,
        T::Purple => mix(c.accent, c.red, 0.45),
    }
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
            Some(task) => {
                let words = super::shell::CardWords {
                    title: task.title.clone(),
                    description: task.description.clone(),
                    acceptance: task.acceptance.clone(),
                    due: task.due.map(|d| d.to_string()).unwrap_or_default(),
                    blocker: task.external_blocker.clone(),
                    tags: task.tags.join(" "),
                };
                super::shell::CardDraft {
                    task: Some(task.id.clone()),
                    // The caller numbers the opening.
                    generation: 0,
                    title: words.title.clone(),
                    description: words.description.clone(),
                    acceptance: words.acceptance.clone(),
                    due: words.due.clone(),
                    blocker: words.blocker.clone(),
                    tags: words.tags.clone(),
                    base: words,
                    comment: String::new(),
                }
            }
            None => super::shell::CardDraft {
                task: Some(id.clone()),
                ..Default::default()
            },
        }
    }

    /// What the person changed in the editor: the fields that differ from
    /// what the editor took (or last saved), never one they left alone,
    /// so a save does not put back what somebody else changed meanwhile.
    pub(super) fn card_change(&self, id: &TaskId) -> Option<super::CardChange> {
        let draft = &self.shell.card_draft;
        if draft.task.as_ref() != Some(id) {
            return None;
        }
        let base = &draft.base;
        let changed =
            |now: &str, was: &str| (now.trim() != was.trim()).then(|| now.trim().to_owned());
        let tags_of = |words: &str| {
            let mut tags: Vec<String> = words
                .split([' ', ','])
                .map(agentdocker_core::task::tag_of)
                .filter(|t| !t.is_empty())
                .collect();
            tags.sort();
            tags.dedup();
            tags
        };
        let tags = tags_of(&draft.tags);
        let change = super::CardChange {
            title: changed(&draft.title, &base.title).filter(|t| !t.is_empty()),
            description: changed(&draft.description, &base.description),
            acceptance: changed(&draft.acceptance, &base.acceptance),
            due: changed(&draft.due, &base.due),
            external_blocker: changed(&draft.blocker, &base.blocker),
            tags: (tags != tags_of(&base.tags)).then_some(tags),
            ..Default::default()
        };
        (change != super::CardChange::default()).then_some(change)
    }

    /// A save of `change` to card `id`, sent by editor opening
    /// `generation`, went through: what it set becomes what that opening's
    /// next save compares against. A later opening keeps its own baseline.
    pub(super) fn card_saved(&mut self, id: &TaskId, change: &super::CardChange, generation: u64) {
        let draft = &mut self.shell.card_draft;
        if draft.task.as_ref() != Some(id) || draft.generation != generation {
            return;
        }
        let base = &mut draft.base;
        for (sent, kept) in [
            (&change.title, &mut base.title),
            (&change.description, &mut base.description),
            (&change.acceptance, &mut base.acceptance),
            (&change.due, &mut base.due),
            (&change.external_blocker, &mut base.blocker),
        ] {
            if let Some(sent) = sent {
                *kept = sent.clone();
            }
        }
        if let Some(tags) = &change.tags {
            base.tags = tags.join(" ");
        }
    }

    /// Whether a card passes the board's search words and toggles.
    pub(super) fn shown(&self, task: &Task, board: &agentdocker_core::Board) -> bool {
        let filters = self.shell.board_filters;
        let words = self.shell.board_query.trim().to_lowercase();
        let lane = board.lane_of(task);
        let by_agent = task
            .assignee
            .as_ref()
            .is_some_and(|a| !self.is_human(a.as_str()));
        let mine = task
            .assignee
            .as_ref()
            .is_some_and(|a| self.persons(a.as_str()));
        (words.is_empty()
            || task.key.to_lowercase() == words
            || task.title.to_lowercase().contains(&words)
            || task.description.to_lowercase().contains(&words)
            || task.tags.iter().any(|t| t.contains(&words))
            || (words.chars().count() >= 2
                && agentdocker_core::search::score(&words, task).is_some()))
            && self.shell.board_tags.iter().all(|t| task.tags.contains(t))
            && (!filters.agent || by_agent)
            && (!filters.high || task.priority == Priority::High)
            && (!filters.blocked
                || !task.external_blocker.trim().is_empty()
                || !task.blocked_by.is_empty())
            && (!filters.waiting || (lane.owner == Owner::Human && lane.category == Column::Review))
            && (!filters.unassigned || task.assignee.is_none())
            && (!filters.mine || mine)
    }

    /// Whether a holder is the person's: the person, or an agent whose
    /// record says the person launched it (`launched_by`), as `list_tasks`
    /// counts `mine`. A holder this app has no record of is nobody's in
    /// particular.
    pub(super) fn persons(&self, id: &str) -> bool {
        let id = self.canonical_agent(id);
        self.is_human(id)
            || self
                .agents
                .iter()
                .any(|a| a.id.as_str() == id && a.persons())
    }

    /// A tag's colour: the catalog's, else one from its name.
    pub(super) fn tag_color(&self, tag: &str, c: Colors) -> iced::Color {
        self.selected_project_root()
            .and_then(|p| self.workspaces.get(&p))
            .and_then(|w| w.tags.iter().find(|t| t.name == tag))
            .map_or_else(|| tag_tone(tag, c), |t| tone_of(t.color, c))
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
        if self.shell.tags_menu {
            page = page.push(self.tags_menu(&root, c));
        }
        if let Some(settings) = &self.shell.lane_settings {
            page = page.push(self.lane_settings(settings, c));
        }
        if self.shell.board_layout == super::shell::BoardLayout::Table {
            page = page.push(self.board_table(all, c));
            if more && all.len() < BOARD_KEEP {
                page = page.push(action(
                    "board-more",
                    if loading { "Loading…" } else { "Show more" },
                    (self.connected.is_ok() && !loading).then_some(Message::TasksMore),
                    false,
                ));
            }
            return page.into();
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
            .push(toggle(
                "board-filter-agent",
                Icon::Sessions,
                "Held by an agent",
                filters.agent,
                F::Agent,
            ))
            .push(toggle(
                "board-filter-mine",
                Icon::Todo,
                "Yours and your agents'",
                filters.mine,
                F::Mine,
            ))
            .push(tip(
                custom_sized(
                    "board-tags",
                    "Tags",
                    row![
                        icon(
                            Icon::Tag,
                            if self.shell.board_tags.is_empty() {
                                c.muted
                            } else {
                                c.text
                            },
                            14.0
                        ),
                        if self.shell.board_tags.is_empty() {
                            Element::from(Space::new().width(0))
                        } else {
                            count_chip(self.shell.board_tags.len(), false, c)
                        },
                    ]
                    .spacing(4)
                    .align_y(Center),
                    Some(Message::TagsMenu),
                    self.shell.tags_menu,
                    Kind::Ghost,
                    [6, 6],
                    iced::Length::Shrink,
                ),
                "Tags",
                c,
            ))
            .push(Space::new().width(Fill))
            .push({
                let table = self.shell.board_layout == super::shell::BoardLayout::Table;
                let words = if table { "Lanes" } else { "Table" };
                tip(
                    custom_sized(
                        "board-layout",
                        words,
                        container(icon(
                            if table { Icon::Board } else { Icon::Rows },
                            c.muted,
                            14.0,
                        ))
                        .center(18),
                        Some(Message::BoardLayout),
                        false,
                        Kind::Ghost,
                        [6, 6],
                        iced::Length::Shrink,
                    ),
                    words,
                    c,
                )
            })
            .push({
                let words = if self.shell.board_dense {
                    "Roomy"
                } else {
                    "Compact"
                };
                tip(
                    custom_sized(
                        "board-density",
                        words,
                        container(icon(
                            if self.shell.board_dense {
                                Icon::ChevronDown
                            } else {
                                Icon::ChevronUp
                            },
                            c.muted,
                            14.0,
                        ))
                        .center(18),
                        Some(Message::BoardDense),
                        false,
                        Kind::Ghost,
                        [6, 6],
                        iced::Length::Shrink,
                    ),
                    words,
                    c,
                )
            })
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
        let hairline = || container(Space::new().width(Fill).height(1)).style(move |_| c.rule());
        container(
            column![
                rows,
                footer,
                hairline(),
                self.tag_settings(c),
                hairline(),
                self.rule_settings(settings, c),
            ]
            .spacing(12),
        )
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
        let draft = self
            .selected_project_root()
            .and_then(|project| self.shell.task_drafts.get(&project));
        let words = draft.map_or("", |d| d.title.as_str());
        let here = match &self.shell.quick_add {
            Some(open) => *open == slug,
            // Words saved before the window last closed show where filing
            // starts.
            None => !words.trim().is_empty() && board.lanes.first().is_some_and(|l| l.slug == slug),
        };
        if here {
            let sending = draft.is_some_and(|d| d.sending());
            body = body.push(input_submitting(
                format!("quick-add-{slug}"),
                "Title",
                words,
                Message::TaskTitle,
                !sending,
                (!words.trim().is_empty() && !sending).then_some(Message::QuickAddFile),
            ));
            if let Some(error) = draft.and_then(|d| d.error.as_deref()) {
                body = body.push(super::view::error_line("quick-add-error", error, c));
            }
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
            marks = marks.push(dot(self.tag_color(tag, c), 6.0, c));
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
        let dense = self.shell.board_dense;
        let mut head = column![
            text(task.key.clone())
                .size(11)
                .font(iced::Font::MONOSPACE)
                .color(c.faint),
            text(first_line(&task.title, 120))
                .size(13)
                .font(weight(iced::font::Weight::Medium))
                .wrapping(iced::widget::text::Wrapping::Word),
        ]
        .spacing(if dense { 2 } else { 5 })
        .width(Fill);
        if !dense {
            head = head.push(foot);
        }
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
                top: if dense { 5.0 } else { 9.0 },
                right: 11.0,
                bottom: if dense { 5.0 } else { 9.0 },
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
        body = body.push(self.card_extras(task, c));
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
                            format!("{name} · {}", crate::clock::moment(comment.created_at)),
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
                    crate::clock::moment(entry.at),
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

impl App {
    /// The project's tags as toggles: a card shows when it carries every
    /// tag turned on.
    fn tags_menu(&self, root: &str, c: Colors) -> Element<'_, Message> {
        let tags = self
            .workspaces
            .get(root)
            .map(|w| w.tags.clone())
            .unwrap_or_default();
        let mut chips = row![].spacing(6).align_y(Center);
        for tag in tags {
            let on = self.shell.board_tags.contains(&tag.name);
            chips = chips.push(custom_sized(
                format!("board-tag-{}", tag.name),
                tag.name.clone(),
                row![
                    dot(tone_of(tag.color, c), 7.0, c),
                    text(tag.name.clone())
                        .size(12)
                        .color(if on { c.text } else { c.muted }),
                ]
                .spacing(5)
                .align_y(Center),
                Some(Message::TagFilter(tag.name.clone())),
                on,
                Kind::Ghost,
                [4, 8],
                iced::Length::Shrink,
            ));
        }
        chips.wrap().into()
    }

    /// Every card of the project, each a row — key, title, its lane's
    /// mark, its holder, priority, due day and tags — through the same
    /// search and filters; a row opens its card under the table.
    fn board_table(&self, all: &[Task], c: Colors) -> Element<'_, Message> {
        let boards = self
            .selected_project_root()
            .and_then(|p| self.boards.get(&p))
            .cloned()
            .unwrap_or_default();
        let Some(main) = self.board_on_view() else {
            return super::view::empty(None, c);
        };
        let board_of = |task: &Task| -> agentdocker_core::Board {
            let slug = task.board.as_deref().unwrap_or(board::MAIN);
            boards
                .iter()
                .find(|b| b.slug == slug)
                .cloned()
                .unwrap_or_else(|| main.clone())
        };
        let mut cards: Vec<(&Task, agentdocker_core::Board)> = all
            .iter()
            .filter(|t| t.archived_at.is_none())
            .map(|t| (t, board_of(t)))
            .filter(|(t, b)| self.shown(t, b))
            .collect();
        cards.sort_by(|(a, x), (b, y)| {
            let at = |t: &Task, board: &agentdocker_core::Board| {
                board
                    .lanes
                    .iter()
                    .position(|l| l.slug == board.lane_of(t).slug)
                    .unwrap_or(0)
            };
            (a.board.as_deref().unwrap_or(board::MAIN) != board::MAIN)
                .cmp(&(b.board.as_deref().unwrap_or(board::MAIN) != board::MAIN))
                .then(at(a, x).cmp(&at(b, y)))
                .then(board::order(a, b))
        });
        let today = chrono::Local::now().date_naive();
        let mut rows = column![].spacing(2).width(Fill);
        for (task, board) in &cards {
            let lane = board.lane_of(task);
            let mut marks = row![].spacing(6).align_y(Center);
            if task.priority == Priority::High {
                marks = marks.push(tip(icon(Icon::Alert, c.red, 12.0), "High priority", c));
            }
            if let Some(due) = task.due {
                let tone = match board::due_state(due, today) {
                    DueState::Overdue => c.red,
                    DueState::Today => c.amber,
                    DueState::Later => c.faint,
                };
                marks = marks.push(tip(
                    icon(Icon::Clock, tone, 12.0),
                    due.format("%b %-d").to_string(),
                    c,
                ));
            }
            for tag in task.tags.iter().take(4) {
                marks = marks.push(tip(dot(self.tag_color(tag, c), 6.0, c), tag.clone(), c));
            }
            let holder: Element<'_, Message> = match &task.assignee {
                Some(agent) => {
                    let name = self.name_of(agent.as_str());
                    tip(self.agent_mark_for(agent.as_str(), &name, 16.0, c), name, c)
                }
                None => Space::new().width(16).into(),
            };
            let open = self.task_open.as_ref() == Some(&task.id);
            let mut label = format!("{} {} — {}", task.handle(), task.title, lane.name);
            if boards.len() > 1 {
                label.push_str(&format!(" on {}", board.name));
            }
            rows = rows.push(custom_sized(
                format!("task-row-{}", task.id),
                label,
                row![
                    container(
                        text(task.key.clone())
                            .size(11)
                            .font(iced::Font::MONOSPACE)
                            .color(c.faint)
                    )
                    .width(64),
                    tip(
                        icon(lane_glyph(lane.category), lane_tone(lane.category, c), 13.0),
                        lane.name.clone(),
                        c
                    ),
                    container(text(first_line(&task.title, 140)).size(13)).width(Fill),
                    marks,
                    holder,
                ]
                .spacing(10)
                .align_y(Center),
                Some(Message::TaskOpen(task.id.clone())),
                open,
                Kind::Ghost,
                [7, 10],
                Fill,
            ));
            if open {
                rows = rows.push(self.card_detail(task, board, c));
            }
        }
        container(rows)
            .padding(6)
            .width(Fill)
            .style(move |_| c.card_style())
            .into()
    }

    /// Only the opened card's full detail can authorize a start. The board
    /// page may omit prerequisites; missing detail keeps the action unavailable.
    pub(super) fn card_startable(&self, id: &TaskId) -> bool {
        self.connected.is_ok()
            && self.task_open.as_ref() == Some(id)
            && self
                .card
                .as_ref()
                .filter(|d| &d.task.id == id)
                .is_some_and(|detail| {
                    agentdocker_core::rule::startable(&detail.task, |blocker| {
                        detail
                            .blockers
                            .iter()
                            .find(|t| &t.id == blocker)
                            .is_none_or(Task::open)
                    })
                })
    }

    /// Below a card's fields: the plan it carries out, its files, its
    /// links, and the tools to change them or start an agent on it.
    fn card_extras(&self, task: &Task, c: Colors) -> Element<'_, Message> {
        use super::shell::CardMenu;
        use agentdocker_core::LinkKind;
        let connected = self.connected.is_ok();
        let icon_button = |control: String, words: String, glyph: Icon, tone, message| {
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
        let detail = self.card.as_ref().filter(|d| d.task.id == task.id);
        let workspace = self
            .selected_project_root()
            .and_then(|p| self.workspaces.get(&p));
        let menu = self.shell.card_menu;
        let mut tools = row![
            icon_button(
                "card-plan".into(),
                "Plan".into(),
                Icon::Note,
                if task.plan.is_some() { c.text } else { c.muted },
                connected.then_some(Message::CardMenu(CardMenu::Plan)),
            ),
            icon_button(
                "card-attach".into(),
                "Attach a file".into(),
                Icon::File,
                c.muted,
                connected.then_some(Message::CardMenu(CardMenu::Attach)),
            ),
            icon_button(
                "card-upload".into(),
                "Add a file from disk".into(),
                Icon::ArrowUp,
                c.muted,
                connected.then_some(Message::FileUpload),
            ),
            icon_button(
                "card-link".into(),
                "Add a link".into(),
                Icon::Link,
                if self.shell.link_kind.is_some() {
                    c.text
                } else {
                    c.muted
                },
                connected.then_some(Message::LinkKind(LinkKind::Url)),
            ),
        ]
        .spacing(2)
        .align_y(Center);
        // A card that waits on an open card, or on something off the
        // board, is not started on, as it is not pulled.
        let startable = self.card_startable(&task.id);
        if startable {
            tools = tools.push(icon_button(
                "card-start".into(),
                "Start an agent on it".into(),
                Icon::Play,
                c.accent,
                connected.then_some(Message::CardMenu(CardMenu::Start)),
            ));
        }
        let mut extras = column![tools].spacing(8).width(Fill);
        // A menu open on the card: its choices are names.
        let mut choices = row![].spacing(6).align_y(Center);
        match menu {
            Some(CardMenu::Start) if startable => {
                for runtime in self.runtimes.iter().filter(|r| {
                    r.cli.is_some() && matches!(r.name.as_str(), "codex" | "claude-code")
                }) {
                    choices = choices.push(ghost(
                        format!("card-start-{}", runtime.name),
                        runtime.label.clone(),
                        connected.then(|| Message::StartAgent(runtime.name.clone())),
                    ));
                }
            }
            Some(CardMenu::Plan) => {
                for plan in workspace
                    .map(|w| w.plans.as_slice())
                    .unwrap_or_default()
                    .iter()
                    .filter(|p| p.status != agentdocker_core::PlanStatus::Archived)
                {
                    choices = choices.push(ghost(
                        format!("card-plan-{}", plan.id),
                        plan.title.clone(),
                        connected.then(|| Message::CardPlan(Some(plan.id.clone()))),
                    ));
                }
                if task.plan.is_some() {
                    choices = choices.push(ghost(
                        "card-plan-none",
                        "No plan",
                        connected.then_some(Message::CardPlan(None)),
                    ));
                }
            }
            Some(CardMenu::Attach) => {
                for file in workspace
                    .map(|w| w.files.as_slice())
                    .unwrap_or_default()
                    .iter()
                    .filter(|f| !f.tasks.contains(&task.id))
                {
                    choices = choices.push(ghost(
                        format!("card-attach-{}", file.id),
                        file.name.clone(),
                        connected.then(|| Message::FileAttach(file.id.clone())),
                    ));
                }
            }
            Some(CardMenu::Start) | None => {}
        }
        if menu.is_some() {
            extras = extras.push(choices.wrap());
        }
        // The plan it carries out: opened on Plans.
        if let Some(plan) = detail.and_then(|d| d.plan.as_ref()) {
            extras = extras.push(
                row![
                    icon(Icon::Note, c.faint, 12.0),
                    ghost(
                        format!("card-open-plan-{}", plan.id),
                        plan.title.clone(),
                        Some(Message::PlanOpen(plan.id.clone())),
                    ),
                ]
                .spacing(6)
                .align_y(Center),
            );
        }
        // Its files: opened with the system's app, or unlinked.
        for file in detail.map(|d| d.files.as_slice()).unwrap_or_default() {
            extras = extras.push(
                row![
                    icon(Icon::File, c.faint, 12.0),
                    ghost(
                        format!("card-file-{}", file.id),
                        file.name.clone(),
                        Some(Message::FileOpen(file.id.clone())),
                    ),
                    Space::new().width(Fill),
                    icon_button(
                        format!("card-file-unlink-{}", file.id),
                        "Unlink".into(),
                        Icon::Close,
                        c.faint,
                        connected.then(|| Message::FileUnlink(file.id.clone())),
                    ),
                ]
                .spacing(6)
                .align_y(Center),
            );
        }
        // Its links: open (a card in place, a URL or path in its app),
        // copy, remove.
        for (index, link) in task.links.iter().enumerate() {
            let glyph = match link.kind {
                LinkKind::Path => Icon::File,
                LinkKind::Commit => Icon::Commit,
                LinkKind::Pr => Icon::Code,
                LinkKind::Url => Icon::Connections,
                LinkKind::Task => Icon::Board,
                LinkKind::Message => Icon::Channels,
                LinkKind::Memory => Icon::Note,
            };
            extras = extras.push(
                row![
                    tip(icon(glyph, c.faint, 12.0), link.kind.as_str().to_owned(), c),
                    ghost(
                        format!("card-link-open-{index}"),
                        link.target.clone(),
                        Some(Message::LinkOpen(index)),
                    ),
                    Space::new().width(Fill),
                    icon_button(
                        format!("card-link-copy-{index}"),
                        "Copy".into(),
                        Icon::Copy,
                        c.faint,
                        Some(Message::LinkCopy(index)),
                    ),
                    icon_button(
                        format!("card-link-remove-{index}"),
                        "Remove".into(),
                        Icon::Close,
                        c.faint,
                        connected.then_some(Message::LinkRemove(index)),
                    ),
                ]
                .spacing(6)
                .align_y(Center),
            );
        }
        if let Some(kind) = self.shell.link_kind {
            let segment = |label: &'static str, value: LinkKind| {
                crate::controls::segment(
                    format!("card-link-kind-{}", value.as_str()),
                    label,
                    Some(Message::LinkKind(value)),
                    kind == value,
                )
            };
            let can_add = connected && !self.shell.link_target.trim().is_empty();
            extras = extras.push(
                row![
                    super::view::segmented(
                        vec![
                            segment("URL", LinkKind::Url),
                            segment("PR", LinkKind::Pr),
                            segment("Path", LinkKind::Path),
                            segment("Commit", LinkKind::Commit),
                            segment("Card", LinkKind::Task),
                        ],
                        c
                    ),
                    container(input_submitting(
                        "card-link-target",
                        "Link",
                        &self.shell.link_target,
                        Message::LinkTarget,
                        connected,
                        can_add.then_some(Message::LinkAdd),
                    ))
                    .width(Fill),
                    action(
                        "card-link-add",
                        "Add",
                        can_add.then_some(Message::LinkAdd),
                        false
                    ),
                ]
                .spacing(8)
                .align_y(Center),
            );
        }
        extras.into()
    }
}

impl App {
    /// The project's tags: each its colour (pressed, the next one) and a
    /// delete; a new one by name.
    fn tag_settings(&self, c: Colors) -> Element<'_, Message> {
        let connected = self.connected.is_ok();
        let tags = self
            .selected_project_root()
            .and_then(|p| self.workspaces.get(&p))
            .map(|w| w.tags.clone())
            .unwrap_or_default();
        let mut chips = row![tip(icon(Icon::Tag, c.faint, 13.0), "Tags", c)]
            .spacing(8)
            .align_y(Center);
        for tag in tags {
            chips = chips.push(
                row![
                    tip(
                        custom_sized(
                            format!("tag-color-{}", tag.name),
                            format!("{}, {}", tag.name, tag.color.as_str()),
                            container(dot(tone_of(tag.color, c), 9.0, c)).center(14),
                            connected.then(|| Message::TagRecolor(tag.name.clone())),
                            false,
                            Kind::Ghost,
                            [3, 3],
                            iced::Length::Shrink,
                        ),
                        "Next colour",
                        c,
                    ),
                    text(tag.name.clone()).size(12),
                    tip(
                        custom_sized(
                            format!("tag-delete-{}", tag.name),
                            format!("Delete {}", tag.name),
                            container(icon(Icon::Close, c.faint, 10.0)).center(14),
                            connected.then(|| Message::TagDelete(tag.name.clone())),
                            false,
                            Kind::Ghost,
                            [2, 2],
                            iced::Length::Shrink,
                        ),
                        "Delete",
                        c,
                    ),
                ]
                .spacing(2)
                .align_y(Center),
            );
        }
        let can_add = connected && !self.shell.tag_draft.trim().is_empty();
        chips = chips.push(
            container(input_submitting(
                "tag-new",
                "New tag",
                &self.shell.tag_draft,
                Message::TagDraft,
                connected,
                can_add.then_some(Message::TagCreate),
            ))
            .width(140),
        );
        chips.wrap().into()
    }

    /// The board's rules: each in a line with its switch and a delete; a
    /// new one from when (into a lane, or idle there some days) and what
    /// (comment, move, archive, start an agent).
    fn rule_settings<'a>(
        &'a self,
        board: &'a agentdocker_core::Board,
        c: Colors,
    ) -> Element<'a, Message> {
        use super::shell::{RuleActionKind as A, RuleFieldKind as F};
        let connected = self.connected.is_ok();
        let rules: Vec<agentdocker_core::Rule> = self
            .selected_project_root()
            .and_then(|p| self.workspaces.get(&p))
            .map(|w| {
                w.rules
                    .iter()
                    .filter(|r| r.board == board.slug)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        let mut list = column![].spacing(4).width(Fill);
        for rule in rules {
            list = list.push(
                row![
                    tip(
                        icon(
                            Icon::Bolt,
                            if rule.enabled { c.accent } else { c.faint },
                            13.0
                        ),
                        rule.summary(board),
                        c
                    ),
                    container(text(rule.name.clone()).size(12).color(if rule.enabled {
                        c.text
                    } else {
                        c.muted
                    }))
                    .width(Fill),
                    crate::controls::segment(
                        format!("rule-on-{}", rule.id),
                        if rule.enabled { "On" } else { "Off" },
                        connected.then(|| Message::RuleToggle(rule.id.clone(), !rule.enabled)),
                        rule.enabled,
                    ),
                    tip(
                        custom_sized(
                            format!("rule-delete-{}", rule.id),
                            format!("Delete {}", rule.name),
                            container(icon(Icon::Close, c.faint, 11.0)).center(14),
                            connected.then(|| Message::RuleDelete(rule.id.clone())),
                            false,
                            Kind::Ghost,
                            [3, 3],
                            iced::Length::Shrink,
                        ),
                        "Delete",
                        c,
                    ),
                ]
                .spacing(8)
                .align_y(Center),
            );
        }
        let draft = &self.shell.rule_draft;
        let lane_chips = |field: F, chosen: &str| {
            let mut chips = Vec::new();
            for lane in &board.lanes {
                chips.push(crate::controls::segment(
                    format!(
                        "rule-{}-{}",
                        if field == F::Lane { "lane" } else { "to" },
                        lane.slug
                    ),
                    lane.name.clone(),
                    Some(Message::RuleField(field, lane.slug.clone())),
                    chosen == lane.slug,
                ));
            }
            super::view::segmented(chips, c)
        };
        let first = board
            .lanes
            .first()
            .map(|l| l.slug.clone())
            .unwrap_or_default();
        let lane = if draft.lane.is_empty() {
            first.as_str()
        } else {
            draft.lane.as_str()
        };
        let when = row![
            tip(icon(Icon::Bolt, c.faint, 13.0), "When", c),
            super::view::segmented(
                vec![
                    crate::controls::segment(
                        "rule-enter",
                        "Comes in",
                        Some(Message::RuleIdle(false)),
                        !draft.idle,
                    ),
                    crate::controls::segment(
                        "rule-idle",
                        "Sits",
                        Some(Message::RuleIdle(true)),
                        draft.idle,
                    ),
                ],
                c
            ),
            lane_chips(F::Lane, lane),
        ]
        .spacing(8)
        .align_y(Center);
        let mut when = when;
        if draft.idle {
            when = when.push(
                container(input_enabled(
                    "rule-days",
                    "Days",
                    &draft.days,
                    |words| Message::RuleField(F::Days, words),
                    true,
                ))
                .width(70),
            );
        }
        let kind = |id: &'static str, label: &'static str, value: A| {
            crate::controls::segment(
                id,
                label,
                Some(Message::RuleAction(value)),
                draft.action == value,
            )
        };
        let mut what = row![super::view::segmented(
            vec![
                kind("rule-comment", "Comment", A::Comment),
                kind("rule-move", "Move", A::Move),
                kind("rule-archive", "Archive", A::Archive),
                kind("rule-start", "Start agent", A::Start),
            ],
            c
        )]
        .spacing(8)
        .align_y(Center);
        match draft.action {
            A::Comment => {
                what = what.push(
                    container(input_enabled(
                        "rule-text",
                        "Comment",
                        &draft.value,
                        |words| Message::RuleField(F::Value, words),
                        true,
                    ))
                    .width(Fill),
                );
            }
            A::Move => what = what.push(lane_chips(F::Value, &draft.value)),
            A::Start => {
                let mut runtimes = Vec::new();
                for runtime in self.runtimes.iter().filter(|r| {
                    r.cli.is_some() && matches!(r.name.as_str(), "codex" | "claude-code")
                }) {
                    runtimes.push(crate::controls::segment(
                        format!("rule-runtime-{}", runtime.name),
                        runtime.label.clone(),
                        Some(Message::RuleField(F::Value, runtime.name.clone())),
                        draft.value == runtime.name,
                    ));
                }
                if !runtimes.is_empty() {
                    what = what.push(super::view::segmented(runtimes, c));
                }
            }
            A::Archive => {}
        }
        let add = row![
            container(input_enabled(
                "rule-name",
                "Name",
                &draft.name,
                |words| Message::RuleField(F::Name, words),
                true,
            ))
            .width(200),
            Space::new().width(Fill),
            action(
                "rule-add",
                "Add rule",
                connected.then_some(Message::RuleCreate),
                false
            ),
        ]
        .spacing(8)
        .align_y(Center);
        column![list, when.wrap(), what.wrap(), add]
            .spacing(8)
            .width(Fill)
            .into()
    }
}
