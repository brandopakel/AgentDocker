//! To-do: every project's open cards that are somebody's to do here —
//! the person's, the agents', and the ones the person filed that nobody
//! holds — grouped by project, each a row: a circle that ticks it done,
//! its key and title, and its holder. A row opens its card on its board.
use super::icons::{Icon, icon};
use super::style::Colors;
use super::view::{first_line, said, tip};
use super::*;
use crate::controls::{Kind, custom_sized};
use agentdocker_core::Task;
use iced::{
    Center, Element, Fill,
    widget::{Space, column, container, row, text},
};

/// Which of the three a card belongs to.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Mine,
    Agents,
    Nobody,
}

impl App {
    fn todo_section(&self, task: &Task) -> Option<Section> {
        match &task.assignee {
            Some(holder) if self.is_human(holder.as_str()) => Some(Section::Mine),
            Some(_) => Some(Section::Agents),
            None if task.created_by == agentdocker_core::HUMAN
                || self.is_human(&task.created_by) =>
            {
                Some(Section::Nobody)
            }
            None => None,
        }
    }

    pub(super) fn todo_view(&self, c: Colors) -> Element<'_, Message> {
        let Some(cards) = &self.todo else {
            return super::view::empty(None, c);
        };
        let mut page = column![].spacing(18).width(Fill);
        for (section, glyph, words) in [
            (Section::Mine, Icon::Todo, "Yours"),
            (Section::Agents, Icon::Sessions, "Your agents'"),
            (Section::Nobody, Icon::Ring, "Nobody holds"),
        ] {
            let mut mine: Vec<&Task> = cards
                .iter()
                .filter(|t| self.todo_section(t) == Some(section))
                .collect();
            if mine.is_empty() {
                continue;
            }
            // Grouped by project, in the sidebar's order; then by due day
            // and priority within one.
            mine.sort_by(|a, b| {
                a.project
                    .cmp(&b.project)
                    .then(a.due.is_none().cmp(&b.due.is_none()))
                    .then(a.due.cmp(&b.due))
                    .then(a.priority.cmp(&b.priority))
                    .then(b.updated_at.cmp(&a.updated_at))
            });
            let mut rows = column![].spacing(1).width(Fill);
            let mut last_project = None;
            for task in mine {
                if last_project != Some(&task.project) {
                    last_project = Some(&task.project);
                    let name = self
                        .shell
                        .catalog
                        .projects
                        .iter()
                        .find(|e| e.project.id() == task.project)
                        .map(|e| e.name())
                        .unwrap_or_else(|| task.project.short().to_owned());
                    rows =
                        rows.push(container(text(name).size(12).color(c.faint)).padding([6, 10]));
                }
                rows = rows.push(self.todo_row(task, c));
            }
            page = page.push(
                column![
                    said(
                        format!(
                            "todo-section-{}",
                            words.to_lowercase().replace(['\'', ' '], "")
                        ),
                        words,
                        icon(glyph, c.faint, 14.0)
                    ),
                    container(rows)
                        .padding(4)
                        .width(Fill)
                        .style(move |_| c.card_style()),
                ]
                .spacing(6),
            );
        }
        page.into()
    }

    fn todo_row(&self, task: &Task, c: Colors) -> Element<'_, Message> {
        let connected = self.connected.is_ok();
        let root = self
            .shell
            .catalog
            .projects
            .iter()
            .find(|e| e.project.id() == task.project)
            .map(|e| e.project.root.display().to_string());
        let today = chrono::Local::now().date_naive();
        let mut marks = row![].spacing(6).align_y(Center);
        if task.priority == agentdocker_core::Priority::High {
            marks = marks.push(tip(icon(Icon::Alert, c.red, 12.0), "High priority", c));
        }
        if let Some(due) = task.due {
            let tone = match agentdocker_core::board::due_state(due, today) {
                agentdocker_core::board::DueState::Overdue => c.red,
                agentdocker_core::board::DueState::Today => c.amber,
                agentdocker_core::board::DueState::Later => c.faint,
            };
            marks = marks.push(tip(
                icon(Icon::Clock, tone, 12.0),
                due.format("%b %-d").to_string(),
                c,
            ));
        }
        let holder: Element<'_, Message> = match &task.assignee {
            Some(agent) => {
                let name = self.name_of(agent.as_str());
                tip(self.agent_mark_for(agent.as_str(), &name, 16.0, c), name, c)
            }
            None => Space::new().width(16).into(),
        };
        let tick = tip(
            custom_sized(
                format!("todo-done-{}", task.id),
                format!("Done: {}", task.handle()),
                container(icon(Icon::Ring, c.muted, 15.0)).center(18),
                connected.then(|| Message::TodoDone(task.id.clone())),
                false,
                Kind::Ghost,
                [3, 3],
                iced::Length::Shrink,
            ),
            "Done",
            c,
        );
        let open = custom_sized(
            format!("todo-{}", task.id),
            format!("{} {}", task.handle(), task.title),
            row![
                container(
                    text(task.key.clone())
                        .size(11)
                        .font(iced::Font::MONOSPACE)
                        .color(c.faint)
                )
                .width(64),
                container(text(first_line(&task.title, 140)).size(13)).width(Fill),
                marks,
                holder,
            ]
            .spacing(10)
            .align_y(Center),
            root.map(|root| Message::TodoOpen(root, task.id.clone())),
            false,
            Kind::Ghost,
            [6, 8],
            Fill,
        );
        row![tick, open].spacing(4).align_y(Center).into()
    }
}
