//! The menu-bar panel: what waits on the person, loudest first, then who
//! is working, across every project, in a window the size of a glance.
//!
//! It is a second Iced window of this same application, borderless and
//! placed under the menu-bar item, so it draws from the one state the main
//! window keeps: the same agents, questions, activity and leases, from the
//! one daemon connection, and the rows Needs you holds are the same
//! widgets sending the same messages (Answer, Allow once, Deny). Nothing
//! is asked of the daemon for it.
//!
//! The list is in two parts. First the Needs-you rows, every project's,
//! each led by its project's name; with none, one line says so. Then the
//! agents at work, grouped by project with a project line over each
//! group: one row each — mark, name, standing chip, the card it holds (the
//! note of its `task:` lease, which the daemon writes as the card's title)
//! and how long it has been at it. A row opens the window at that agent.
//! The foot is one way to the window for when nothing above is a reason
//! to open it.
//!
//! The pure part — which agents, in what order, under what project line —
//! is [`grouped`], tested on its own.
use super::style::{Colors, alpha, weight};
use super::view::{agent_mark, eyebrow, rule, standing_chip};
use super::{App, Message};
use crate::controls::{Kind, custom, ghost};
use agentdocker_core::standing::Standing;
use chrono::{DateTime, Utc};
use iced::widget::{column, container, row, scrollable, text};
use iced::{Center, Element, Fill, FillPortion};

/// The panel's size in logical points: wide enough for a row to say
/// who, what and how long, tall enough for a dozen rows.
pub(super) const WIDTH: f32 = 448.0;
pub(super) const HEIGHT: f32 = 600.0;

/// The project line over sessions that work in no listed project.
pub(super) const OTHER: &str = "Other sessions";

/// One agent at work, as its row says it.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Working {
    pub id: String,
    pub name: String,
    pub runtime: String,
    pub standing: Standing,
    /// The title of the card it holds, when it holds one.
    pub card: Option<String>,
    /// When its current work began: its turn's start when its provider
    /// reports one, else when it started.
    pub since: Option<DateTime<Utc>>,
}

/// The agents at work in one project.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Group {
    pub project: String,
    pub agents: Vec<Working>,
}

/// The working agents under their project lines: projects in name order,
/// the projectless last under [`OTHER`]; within a project the longest at
/// work first, those with no known start after them, then by name, so
/// the list reads the same whatever order the records came in.
pub(super) fn grouped(agents: Vec<(Option<String>, Working)>) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    for (project, agent) in agents {
        let project = project.unwrap_or_else(|| OTHER.to_owned());
        match groups.iter_mut().find(|g| g.project == project) {
            Some(group) => group.agents.push(agent),
            None => groups.push(Group {
                project,
                agents: vec![agent],
            }),
        }
    }
    groups.sort_by(|a, b| {
        (a.project == OTHER)
            .cmp(&(b.project == OTHER))
            .then_with(|| a.project.to_lowercase().cmp(&b.project.to_lowercase()))
            .then_with(|| a.project.cmp(&b.project))
    });
    for group in &mut groups {
        group.agents.sort_by(|a, b| match (a.since, b.since) {
            (Some(x), Some(y)) => x.cmp(&y).then_with(|| a.name.cmp(&b.name)),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.name.cmp(&b.name),
        });
    }
    groups
}

/// How long an agent has been at it, in the span words every row uses
/// (*42m*, *3h*).
pub(super) fn at_work_for(since: DateTime<Utc>, now: DateTime<Utc>) -> String {
    super::span((now - since).num_seconds().max(0))
}

impl App {
    /// The agents at work, from the records on hand: live, not a person,
    /// and not already a Needs-you row (a question open, or stopped at
    /// its own prompt). An agent whose delivery is held keeps its Needs
    /// you chip here, as its row in the window does.
    pub(super) fn working_groups(&self) -> Vec<Group> {
        let agents = self
            .agents
            .iter()
            .filter(|a| {
                a.status.is_live()
                    && !self.is_human(a.id.as_str())
                    && !self.waits_for_person(a)
                    && !self.needs_input(a.id.as_str())
            })
            .map(|a| {
                let id = a.id.to_string();
                let card = self
                    .leases
                    .iter()
                    .find(|l| {
                        l.holder == a.id
                            && l.mode == agentdocker_core::LeaseMode::Exclusive
                            && l.resource.as_str().starts_with("task:")
                    })
                    .and_then(|l| l.note.clone());
                let since = match self.activity.get(&id) {
                    Some(agentdocker_core::Activity::Working { since }) => Some(*since),
                    _ => a.started_at.or(a.process_started_at),
                };
                (
                    self.project_name_of(&id),
                    Working {
                        name: self.display_name(a),
                        runtime: a.spec.runtime.clone(),
                        standing: self.standing(a),
                        card,
                        since,
                        id,
                    },
                )
            })
            .collect();
        grouped(agents)
    }

    /// The panel's whole window: the Needs-you rows, the agents at work
    /// under their project lines, and the way to the window at the foot.
    pub(super) fn panel_view(&self, c: Colors) -> Element<'_, Message> {
        let now = Utc::now();
        let mut list = column![].width(Fill);
        let waiting = self.needs_you_rows(true, true, c);
        if waiting.is_empty() {
            list = list.push(line("panel-nothing-waits", "Nothing waits on you", c));
        }
        for (index, item) in waiting.into_iter().enumerate() {
            if index > 0 {
                list = list.push(rule(c));
            }
            list = list.push(item);
        }
        let groups = self.working_groups();
        if groups.is_empty() && self.shell.catalog.projects.is_empty() {
            list = list.push(rule(c)).push(line(
                "panel-no-projects",
                "No project is listed yet. Open AgentDocker to add one.",
                c,
            ));
        }
        for group in groups {
            list = list.push(
                container(eyebrow(group.project.clone(), c))
                    .padding([10, 14])
                    .width(Fill)
                    .style(move |_| container::Style {
                        background: Some(alpha(c.text, 0.025).into()),
                        ..Default::default()
                    }),
            );
            for agent in group.agents {
                list = list.push(self.working_row(&group.project, agent, now, c));
            }
        }
        let foot = container(ghost(
            "panel-open-window",
            "Open AgentDocker",
            Some(Message::OpenWindow),
        ))
        .padding([6, 10])
        .width(Fill);
        container(column![scrollable(list).height(Fill), rule(c), foot])
            .width(Fill)
            .height(Fill)
            .style(move |_| container::Style {
                background: Some(c.ground.into()),
                text_color: Some(c.text),
                border: iced::Border {
                    color: c.line_strong,
                    width: 1.0,
                    radius: 0.0.into(),
                },
                ..Default::default()
            })
            .into()
    }

    /// One agent at work: mark, name, standing chip, the card it holds in
    /// the quiet ink and how long it has been at it, read as one sentence;
    /// the whole row opens the window at that agent.
    fn working_row(
        &self,
        project: &str,
        agent: Working,
        now: DateTime<Utc>,
        c: Colors,
    ) -> Element<'_, Message> {
        let span = agent.since.map(|since| at_work_for(since, now));
        let mut facts = vec![agent.standing.label().to_owned()];
        if let Some(card) = &agent.card {
            facts.push(card.clone());
        }
        if let Some(span) = &span {
            facts.push(format!("for {span}"));
        }
        let sentence = format!("{project} · {} · {}", agent.name, facts.join(" · "));
        let mut content = row![
            agent_mark(Some(&agent.runtime), &agent.name, &agent.id, 18.0, c),
            container(
                text(agent.name.clone())
                    .size(13.5)
                    .font(weight(iced::font::Weight::Medium))
                    .color(c.text)
                    .wrapping(iced::widget::text::Wrapping::None)
            )
            .width(FillPortion(2))
            .clip(true),
            standing_chip(format!("panel-standing-{}", agent.id), agent.standing, c),
        ]
        .spacing(8)
        .align_y(Center);
        content = content.push(
            container(
                text(agent.card.clone().unwrap_or_default())
                    .size(12)
                    .color(c.muted)
                    .wrapping(iced::widget::text::Wrapping::None),
            )
            .width(FillPortion(3))
            .clip(true),
        );
        if let Some(span) = span {
            content = content.push(
                text(span)
                    .size(12)
                    .color(c.faint)
                    .wrapping(iced::widget::text::Wrapping::None),
            );
        }
        custom(
            format!("panel-agent-{}", agent.id),
            sentence,
            content,
            Some(Message::OpenSession(agent.id)),
            false,
            Kind::Quiet,
            [6, 14],
        )
    }
}

/// One quiet line of the list, in the muted ink.
fn line<'a>(id: &str, words: &str, c: Colors) -> Element<'a, Message> {
    container(
        crate::controls::Control {
            content: text(words.to_owned()).size(13).color(c.muted).into(),
            semantic: crate::accessibility::Semantic {
                role: accesskit::Role::Label,
                ..crate::accessibility::Semantic::button(id.to_owned(), words.to_owned(), None)
            },
            button: false,
        }
        .into(),
    )
    .padding([10, 16])
    .width(Fill)
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn working(name: &str, since: Option<i64>) -> Working {
        let now = Utc::now();
        Working {
            id: name.to_owned(),
            name: name.to_owned(),
            runtime: "claude-code".into(),
            standing: Standing::Working,
            card: None,
            since: since.map(|minutes| now - chrono::Duration::minutes(minutes)),
        }
    }

    /// Projects come in name order with the projectless last; within a
    /// project the agent longest at work leads, one with no known start
    /// trails, and names break ties, whatever order the records came in.
    #[test]
    fn rows_are_grouped_by_project_and_the_longest_at_work_leads() {
        let groups = grouped(vec![
            (Some("orchard".into()), working("badger", Some(5))),
            (None, working("stray", None)),
            (Some("Apiary".into()), working("wren", None)),
            (Some("orchard".into()), working("otter", Some(50))),
            (Some("Apiary".into()), working("heron", Some(1))),
            (Some("orchard".into()), working("ant", None)),
        ]);
        let names: Vec<(String, Vec<String>)> = groups
            .into_iter()
            .map(|g| (g.project, g.agents.into_iter().map(|a| a.name).collect()))
            .collect();
        assert_eq!(
            names,
            vec![
                ("Apiary".to_owned(), vec!["heron".to_owned(), "wren".to_owned()]),
                (
                    "orchard".to_owned(),
                    vec!["otter".to_owned(), "badger".to_owned(), "ant".to_owned()]
                ),
                (OTHER.to_owned(), vec!["stray".to_owned()]),
            ]
        );
    }

    #[test]
    fn time_at_work_is_said_in_span_words() {
        let now = Utc::now();
        assert_eq!(at_work_for(now - chrono::Duration::minutes(42), now), "42m");
        assert_eq!(at_work_for(now - chrono::Duration::hours(3), now), "3h");
        assert_eq!(at_work_for(now + chrono::Duration::minutes(1), now), "0s");
    }
}
