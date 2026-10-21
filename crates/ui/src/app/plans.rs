//! Plans: a project's specs in Markdown, which cards carry out, and its
//! files. The list is names with a dot for where each stands; the one
//! open is its title, its status, its words to edit, and the cards that
//! carry it out. The project's files sit under it, each opened with the
//! system's app, and its memory under them: the facts its agents and the
//! person keep for each other, each with who kept it.
use super::icons::{Icon, icon};
use super::style::Colors;
use super::view::{first_line, status_dot, tip};
use super::*;
use crate::controls::{Kind, custom_sized, ghost, input_enabled, primary};
use agentdocker_core::PlanStatus;
use iced::{
    Center, Element, Fill, Top,
    widget::{Space, column, container, row, text, text_editor},
};

fn status_tone(status: PlanStatus, c: Colors) -> iced::Color {
    match status {
        PlanStatus::Draft => c.faint,
        PlanStatus::Active => c.accent,
        PlanStatus::Done => c.green,
        PlanStatus::Archived => c.line_strong,
    }
}

impl App {
    /// Who kept a memory, as the person knows them.
    fn memory_author(&self, author: &str) -> String {
        if author == agentdocker_core::HUMAN {
            return "you".to_owned();
        }
        self.agents
            .iter()
            .find(|a| a.id.as_str() == author)
            .map_or_else(
                || author.chars().take(12).collect(),
                |agent| self.display_name(agent),
            )
    }

    /// The project's memory: a line to keep a fact, a line to search, then
    /// each fact kept, newest first (or the best answers to the search),
    /// with who kept it and when, and its forget. A daemon from before
    /// memory says so; a read that failed says so over what was there.
    fn memory_card<'a>(
        &'a self,
        workspace: Option<&'a super::Workspace>,
        connected: bool,
        c: Colors,
    ) -> Element<'a, Message> {
        let header = row![tip(icon(Icon::Note, c.faint, 14.0), "Memory", c)].align_y(Center);
        let card = |content: iced::widget::Column<'a, Message>| -> Element<'a, Message> {
            container(content)
                .padding(8)
                .width(Fill)
                .style(move |_| c.card_style())
                .into()
        };
        let read = workspace.map(|w| &w.memory);
        if read == Some(&super::MemoryRead::Unsupported) {
            return card(
                column![
                    header,
                    super::view::small(
                        "This AgentDocker keeps no project memory; update it to keep one.",
                        c
                    ),
                ]
                .spacing(6),
            );
        }
        let empty = super::shell::MemoryForm::default();
        let form = self
            .selected_project_root()
            .and_then(|root| self.shell.memory.get(&root))
            .unwrap_or(&empty);
        let draft = form.draft.trim();
        let mut memory = column![
            header,
            row![
                container(input_enabled(
                    "memory-draft",
                    "A fact for this project's agents",
                    &form.draft,
                    Message::MemoryDraft,
                    connected && form.adding.is_none(),
                ))
                .width(Fill),
                primary(
                    "memory-add",
                    if form.adding.is_some() {
                        "Keeping…"
                    } else {
                        "Keep"
                    },
                    (connected && !draft.is_empty() && form.adding.is_none())
                        .then_some(Message::MemoryAdd),
                ),
            ]
            .spacing(8)
            .align_y(Center),
            input_enabled(
                "memory-search",
                "Search memory",
                &form.query,
                Message::MemoryQuery,
                connected,
            ),
        ]
        .spacing(6)
        .width(Fill);
        if let Some(super::MemoryRead::Failed(error)) = read {
            memory = memory.push(
                super::view::small(format!("Memory could not be read: {error}"), c).color(c.amber),
            );
        }
        let query = form.query.trim();
        let (shown, limit_note): (&[agentdocker_core::memory::Memory], Option<&str>) =
            if query.is_empty() {
                let kept = workspace.map(|w| w.memories.as_slice()).unwrap_or_default();
                (
                    kept,
                    (kept.len() >= super::MEMORY_PAGE)
                        .then_some("The newest 100; search to find older ones."),
                )
            } else {
                match &form.results {
                    Some((words, found)) if words == query => (
                        found.as_slice(),
                        (found.len() >= super::MEMORY_PAGE)
                            .then_some("The best 100 answers; more words narrow them."),
                    ),
                    _ => (&[], None),
                }
            };
        let now = chrono::Utc::now();
        for kept in shown {
            memory = memory.push(
                row![
                    column![
                        text(kept.text.clone()).size(13),
                        super::view::small(
                            format!(
                                "{} · {}",
                                self.memory_author(&kept.author),
                                agentdocker_core::journal::ago(now, kept.created_at)
                            ),
                            c
                        ),
                    ]
                    .spacing(2)
                    .width(Fill),
                    tip(
                        custom_sized(
                            format!("memory-forget-{}", kept.id),
                            "Forget",
                            container(icon(Icon::Close, c.faint, 11.0)).center(14),
                            connected.then(|| Message::MemoryForget(kept.id.clone())),
                            false,
                            Kind::Ghost,
                            [3, 3],
                            iced::Length::Shrink,
                        ),
                        "Forget",
                        c,
                    ),
                ]
                .spacing(6)
                .align_y(Center),
            );
        }
        if let Some(note) = limit_note {
            memory = memory.push(super::view::small(note, c));
        }
        card(memory)
    }

    pub(super) fn plans_view(&self, c: Colors) -> Element<'_, Message> {
        let Some(root) = self.selected_project_root() else {
            return super::view::empty(None, c);
        };
        let connected = self.connected.is_ok();
        let workspace = self.workspaces.get(&root);
        let draft = &self.shell.plan_draft;
        let open_id = draft.id.clone();
        // The list: each plan a name, its dot saying where it stands.
        let mut list = column![
            row![
                tip(icon(Icon::Note, c.faint, 14.0), "Plans", c),
                Space::new().width(Fill),
                tip(
                    custom_sized(
                        "plan-new",
                        "New plan",
                        container(icon(Icon::Add, c.muted, 14.0)).center(18),
                        connected.then_some(Message::PlanNew),
                        false,
                        Kind::Ghost,
                        [6, 6],
                        iced::Length::Shrink,
                    ),
                    "New plan",
                    c,
                ),
            ]
            .align_y(Center)
        ]
        .spacing(2)
        .width(Fill);
        for plan in workspace.map(|w| w.plans.as_slice()).unwrap_or_default() {
            let selected = open_id.as_deref() == Some(plan.id.as_str());
            list = list.push(custom_sized(
                format!("plan-{}", plan.id),
                format!("{}, {}", plan.title, plan.status.label()),
                row![
                    status_dot(
                        format!("plan-status-{}", plan.id),
                        status_tone(plan.status, c),
                        plan.status.label(),
                        c
                    ),
                    text(first_line(&plan.title, 80)).size(13).color(
                        if plan.status == PlanStatus::Archived {
                            c.muted
                        } else {
                            c.text
                        }
                    ),
                ]
                .spacing(8)
                .align_y(Center),
                Some(Message::PlanOpen(plan.id.clone())),
                selected,
                Kind::Ghost,
                [6, 8],
                Fill,
            ));
        }
        let list = container(list)
            .padding(8)
            .width(240)
            .style(move |_| c.card_style());
        // The plan open, or being written.
        let editing = draft.id.is_some() || draft.dirty;
        let mut editor = column![].spacing(10).width(Fill);
        if editing {
            let status = |value: PlanStatus| {
                crate::controls::segment(
                    format!("plan-status-set-{}", value.as_str()),
                    value.label(),
                    Some(Message::PlanStatus(value)),
                    draft.status == value,
                )
            };
            editor = editor
                .push(
                    row![
                        container(input_enabled(
                            "plan-title",
                            "Title",
                            &draft.title,
                            Message::PlanTitle,
                            connected,
                        ))
                        .width(Fill),
                        super::view::segmented(
                            PlanStatus::ALL.iter().map(|s| status(*s)).collect(),
                            c
                        ),
                    ]
                    .spacing(8)
                    .align_y(Center),
                )
                .push(
                    text_editor(&draft.body)
                        .placeholder("Markdown")
                        .on_action(Message::PlanBody)
                        .height(360)
                        .size(13),
                )
                .push(
                    row![
                        Space::new().width(Fill),
                        primary(
                            "plan-save",
                            "Save",
                            (connected && draft.dirty).then_some(Message::PlanSave),
                        ),
                    ]
                    .align_y(Center),
                );
            // The cards that carry it out, each opening on its board.
            if let Some((plan, tasks)) = &self.plan_open
                && draft.id.as_deref() == Some(plan.id.as_str())
            {
                for task in tasks {
                    editor = editor.push(
                        row![
                            icon(Icon::Board, c.faint, 12.0),
                            ghost(
                                format!("plan-card-{}", task.id),
                                format!("{} {}", task.handle(), first_line(&task.title, 100)),
                                Some(Message::TodoOpen(root.clone(), task.id.clone())),
                            ),
                        ]
                        .spacing(6)
                        .align_y(Center),
                    );
                }
            }
        }
        // The project's files.
        let mut files = column![
            row![
                tip(icon(Icon::File, c.faint, 14.0), "Files", c),
                Space::new().width(Fill),
                tip(
                    custom_sized(
                        "file-add",
                        "Add a file",
                        container(icon(Icon::ArrowUp, c.muted, 14.0)).center(18),
                        connected.then_some(Message::FileUpload),
                        false,
                        Kind::Ghost,
                        [6, 6],
                        iced::Length::Shrink,
                    ),
                    "Add a file",
                    c,
                ),
            ]
            .align_y(Center)
        ]
        .spacing(2)
        .width(Fill);
        for file in workspace.map(|w| w.files.as_slice()).unwrap_or_default() {
            files = files.push(
                row![
                    ghost(
                        format!("file-{}", file.id),
                        file.name.clone(),
                        Some(Message::FileOpen(file.id.clone())),
                    ),
                    Space::new().width(Fill),
                    tip(
                        custom_sized(
                            format!("file-delete-{}", file.id),
                            format!("Delete {}", file.name),
                            container(icon(Icon::Close, c.faint, 11.0)).center(14),
                            connected.then(|| Message::FileDelete(file.id.clone())),
                            false,
                            Kind::Ghost,
                            [3, 3],
                            iced::Length::Shrink,
                        ),
                        "Delete",
                        c,
                    ),
                ]
                .spacing(6)
                .align_y(Center),
            );
        }
        let files = container(files)
            .padding(8)
            .width(Fill)
            .style(move |_| c.card_style());
        let memory = self.memory_card(workspace, connected, c);
        let side = column![list].spacing(12);
        let main = column![editor, files, memory].spacing(14).width(Fill);
        if self.narrow() {
            column![side, main].spacing(14).into()
        } else {
            row![side, main].spacing(16).align_y(Top).into()
        }
    }
}
