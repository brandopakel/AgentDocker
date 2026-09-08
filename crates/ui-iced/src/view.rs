//! Focused project workspace, attention inbox, and connection settings.
use crate::{
    App, Message,
    model::{Activity, Detail, Page},
    style::Colors,
};
use iced::{
    Alignment, Element, Fill, Font,
    widget::{Space, button, column, container, row, scrollable, text, text_input},
};

fn strong<'a>(value: impl Into<String>, size: u32) -> iced::widget::Text<'a> {
    text(value.into()).size(size).font(Font {
        weight: iced::font::Weight::Semibold,
        ..Font::DEFAULT
    })
}

impl App {
    pub fn view(&self) -> Element<'_, Message> {
        let c = Colors::new(self.workspace.dark);
        let w = &self.workspace;
        let title = match w.page {
            Page::Projects => w.projects[w.project].name,
            Page::Inbox => "Inbox",
            Page::Connections => "Connections",
            Page::Settings => "Settings",
        };
        let subtitle = match w.page {
            Page::Projects => w.projects[w.project].description,
            Page::Inbox => "The things your agents need from you.",
            Page::Connections => "Bring your tools into the same workspace.",
            Page::Settings => "Make this space yours.",
        };
        let title = column![strong(title, 30), text(subtitle).size(14).color(c.muted)]
            .spacing(7)
            .width(Fill);
        let badge = container(text("Sample workspace").size(12).color(c.muted))
            .padding([7, 10])
            .style(move |_| c.surface(c.subtle, false));
        let heading: Element<'_, Message> = if self.width < 900.0 {
            column![title, badge].spacing(12).width(Fill).into()
        } else {
            row![title, badge]
                .spacing(18)
                .align_y(Alignment::Center)
                .into()
        };
        let body = match w.page {
            Page::Projects => self.projects(c),
            Page::Inbox => self.inbox(c),
            Page::Connections => self.connections(c),
            Page::Settings => self.settings(c),
        };
        let mut content = column![heading, iced::widget::rule::horizontal(1)]
            .spacing(24)
            .width(Fill);
        if w.offline {
            content = content.push(
                container(
                    column![
                        strong("Connection lost", 14).color(c.amber),
                        text(
                            "Showing the last snapshot. Your drafts are saved; actions are paused."
                        )
                        .size(13)
                    ]
                    .spacing(5),
                )
                .width(Fill)
                .padding(14)
                .style(move |_| c.surface(c.subtle, true)),
            );
        }
        content = content.push(body);
        row![
            self.sidebar(c),
            container(scrollable(content).spacing(12).width(Fill).height(Fill))
                .padding(if self.width < 900.0 { 20 } else { 28 })
                .width(Fill)
        ]
        .height(Fill)
        .into()
    }

    fn sidebar(&self, c: Colors) -> Element<'_, Message> {
        let w = &self.workspace;
        let mut nav = column![
            row![
                iced::widget::image(self.icon.clone()).width(29).height(29),
                strong("agentdocker", 20)
            ]
            .spacing(9)
            .align_y(Alignment::Center),
            Space::new().height(25),
        ]
        .spacing(5);
        for (page, label) in [
            (Page::Projects, "Projects"),
            (Page::Inbox, "Inbox"),
            (Page::Connections, "Connections"),
        ] {
            let mut line =
                row![text(label).size(14), Space::new().width(Fill)].align_y(Alignment::Center);
            if page == Page::Inbox && w.question_pending() {
                line = line.push(
                    container(text("1").size(12))
                        .padding([2, 7])
                        .style(move |_| c.surface(c.selected, false)),
                );
            }
            nav = nav.push(
                button(line)
                    .width(Fill)
                    .padding([11, 12])
                    .style(c.quiet(w.page == page))
                    .on_press(Message::Navigate(page)),
            );
        }
        nav = nav
            .push(Space::new().height(24))
            .push(container(text("Your projects").size(12).color(c.muted)).padding([0, 12]));
        for (id, project) in w.projects.iter().enumerate() {
            nav = nav.push(
                button(
                    row![text("·").size(19), text(project.name).size(13)]
                        .spacing(9)
                        .align_y(Alignment::Center),
                )
                .width(Fill)
                .padding([9, 12])
                .style(c.quiet(w.page == Page::Projects && w.project == id))
                .on_press(Message::Project(id)),
            );
        }
        nav = nav
            .push(Space::new().height(Fill))
            .push(
                button(text("Settings").size(14))
                    .padding([11, 12])
                    .width(Fill)
                    .style(c.quiet(w.page == Page::Settings))
                    .on_press(Message::Navigate(Page::Settings)),
            )
            .push(
                container(
                    column![
                        text("Design preview").size(12).color(c.muted),
                        text("Changes stay in this window").size(11).color(c.muted)
                    ]
                    .spacing(4),
                )
                .padding([12, 12]),
            );
        container(nav.height(Fill))
            .width(214)
            .padding([24, 12])
            .height(Fill)
            .style(move |_| c.surface(c.sidebar, false))
            .into()
    }

    fn projects(&self, c: Colors) -> Element<'_, Message> {
        let w = &self.workspace;
        let mut panel = column![].spacing(18).width(Fill);
        if w.project == 0 && w.question_pending() {
            panel = panel.push(
                button(
                    row![
                        text("●").size(10).color(c.amber),
                        text("Design review has a question for you")
                            .size(14)
                            .width(Fill),
                        text("View inbox  ›").size(13).color(c.accent),
                    ]
                    .spacing(12)
                    .align_y(Alignment::Center),
                )
                .width(Fill)
                .padding([15, 16])
                .style(c.quiet(false))
                .on_press(Message::Navigate(Page::Inbox)),
            );
        }
        panel = panel.push(
            row![
                strong("Sessions", 18),
                Space::new().width(Fill),
                text_input("Find a session…", &w.search)
                    .on_input(Message::Search)
                    .padding(10)
                    .size(13)
                    .width(210)
            ]
            .align_y(Alignment::Center),
        );
        let mut sessions = column![].spacing(3).width(Fill);
        let visible: Vec<_> = w.visible_agents().collect();
        for (id, agent) in &visible {
            let status_color = match agent.activity {
                Activity::Working => c.green,
                Activity::NeedsInput => c.amber,
                Activity::Unknown => c.muted,
            };
            let line = row![
                column![
                    strong(agent.name, 15),
                    text(format!("{}  ·  {}", agent.provider, agent.branch))
                        .size(12)
                        .color(c.muted)
                ]
                .spacing(6)
                .width(Fill),
                text(agent.activity.label()).size(12).color(status_color),
                text("›").size(20).color(c.muted)
            ]
            .spacing(14)
            .align_y(Alignment::Center);
            sessions = sessions.push(
                button(line)
                    .padding(16)
                    .width(Fill)
                    .style(c.quiet(w.selected == Some(*id)))
                    .on_press(Message::Select(*id)),
            );
        }
        if visible.is_empty() {
            sessions = sessions.push(
                container(
                    column![
                        strong(
                            if w.search.is_empty() {
                                "No sessions here yet"
                            } else {
                                "No matching sessions"
                            },
                            18
                        ),
                        text(if w.search.is_empty() {
                            "Start an agent in this project. It will appear here when discovered."
                        } else {
                            "Try another name, tool, or branch."
                        })
                        .size(14)
                        .color(c.muted),
                    ]
                    .spacing(10),
                )
                .padding(32)
                .width(Fill),
            );
        }
        panel = panel.push(
            container(sessions)
                .padding(4)
                .width(Fill)
                .style(move |_| c.surface(c.ground, true)),
        );
        let selected = w.selected.filter(|id| *id < w.agents.len());
        if self.width < 1080.0
            && let Some(id) = selected
        {
            panel = panel.push(self.inspector(id, c));
        }
        if !w.empty && w.project == 0 {
            panel = panel
                .push(Space::new().height(8))
                .push(strong("Recent activity", 18));
            for (when, what) in [
                ("12 sec", "Desktop cleanup reported tool activity"),
                ("28 sec", "Release checks reported tool activity"),
                ("2 min", "Design review asked a question"),
            ] {
                panel = panel.push(
                    row![
                        text(when).size(12).color(c.muted).width(58),
                        text(what).size(13).color(c.muted).width(Fill)
                    ]
                    .spacing(12),
                );
            }
        }
        if !visible.is_empty() {
            panel = panel.push(Space::new().height(12)).push(
                text("Select a session to see its context, activity, and terminal.")
                    .size(12)
                    .color(c.muted),
            );
        }
        if let Some(id) = selected
            && self.width >= 1080.0
        {
            return row![
                container(panel).width(Fill),
                container(self.inspector(id, c)).width(310)
            ]
            .spacing(24)
            .into();
        }
        panel.into()
    }

    fn inspector(&self, id: usize, c: Colors) -> Element<'_, Message> {
        let w = &self.workspace;
        let agent = &w.agents[id];
        let mut tabs = row![].spacing(4);
        for (detail, label) in [
            (Detail::Overview, "Overview"),
            (Detail::Activity, "Activity"),
            (Detail::Terminal, "Terminal"),
        ] {
            tabs = tabs.push(
                button(text(label).size(12))
                    .padding([8, 9])
                    .style(c.quiet(w.detail == detail))
                    .on_press(Message::Detail(detail)),
            );
        }
        let body: Element<'_, Message> = match w.detail {
            Detail::Overview => column![
                text("Tool").size(12).color(c.muted), strong(agent.provider, 14),
                text("Checkout").size(12).color(c.muted), text(w.projects[agent.project].path).size(13),
                text(agent.branch).size(13).color(c.muted),
                text("Last observation").size(12).color(c.muted), text(agent.observation).size(13),
                text("An available process does not establish what it is doing.").size(12).color(c.muted),
            ].spacing(10).into(),
            Detail::Activity => column![
                strong(agent.activity.label(), 15), text(agent.observation).size(13),
                iced::widget::rule::horizontal(1),
                text("Detailed events and resource coordination belong here, close to the session they describe.").size(13).color(c.muted),
            ].spacing(14).into(),
            Detail::Terminal => {
                let output = if agent.terminal { "$ agentdocker status\n\nSample terminal transcript\nNo commands are executed here.\n\nProject     AgentDocker\nConnection  Local\n\n$ " } else { "No terminal is attached to this session.\n\nA discovered process may be visible without allowing terminal access." };
                container(text(output).font(Font::MONOSPACE).size(12)).padding(14).width(Fill).style(move |_| c.surface(c.subtle, false)).into()
            }
        };
        container(
            column![
                row![
                    strong(agent.name, 17),
                    Space::new().width(Fill),
                    button(text("×").size(20))
                        .style(c.quiet(false))
                        .on_press(Message::CloseDetail)
                ]
                .align_y(Alignment::Center),
                tabs,
                iced::widget::rule::horizontal(1),
                body
            ]
            .spacing(18),
        )
        .padding(18)
        .width(Fill)
        .style(move |_| c.surface(c.subtle, true))
        .into()
    }

    fn inbox(&self, c: Colors) -> Element<'_, Message> {
        let w = &self.workspace;
        if !w.question_pending() {
            return container(
                column![
                    strong("You're all caught up", 24),
                    text(if w.answered {
                        "Your answer is recorded in this preview."
                    } else {
                        "Questions from your agents will appear here."
                    })
                    .size(14)
                    .color(c.muted),
                    button("Back to project")
                        .padding([10, 16])
                        .on_press(Message::Navigate(Page::Projects))
                ]
                .spacing(16),
            )
            .padding(28)
            .width(Fill)
            .style(move |_| c.surface(c.subtle, true))
            .into();
        }
        let answer = button("Record preview answer")
            .padding([10, 16])
            .on_press_maybe((!w.offline && !w.answer.trim().is_empty()).then_some(Message::Answer));
        let note = text("Only the sample workspace changes.")
            .size(12)
            .color(c.muted);
        let footer: Element<'_, Message> = if self.width < 900.0 {
            column![answer, note].spacing(12).into()
        } else {
            row![answer, note]
                .spacing(15)
                .align_y(Alignment::Center)
                .into()
        };
        container(column![
            text("AgentDocker  /  Design review").size(13).color(c.muted),
            strong("Where should we pick up next time?", 23),
            text("When agentdocker opens, should it restore your last project or show every project?").size(16),
            text("Claude Code · asked 2 minutes ago").size(12).color(c.muted),
            Space::new().height(8),
            text_input("Write an answer…", &w.answer).on_input(Message::Draft).on_submit(Message::Answer).padding(14),
            footer,
        ].spacing(16)).padding(26).width(Fill).max_width(780).style(move |_| c.surface(c.ground, true)).into()
    }

    fn connections(&self, c: Colors) -> Element<'_, Message> {
        let mut list = column![
            text("Installed tools and verified sessions are shown separately.")
                .size(14)
                .color(c.muted)
        ]
        .spacing(16);
        for (id, (name, status, description)) in [
            (
                "Claude Code",
                "Configured",
                "Activity hooks and MCP coordination",
            ),
            (
                "Codex CLI",
                "Configured",
                "MCP coordination and activity reports",
            ),
            (
                "Claude Desktop",
                "Available",
                "Installed application; no session activity claimed",
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let mut card = column![
                row![
                    column![strong(name, 17), text(description).size(13).color(c.muted)]
                        .spacing(7)
                        .width(Fill),
                    text(status).size(12).color(if status == "Configured" {
                        c.green
                    } else {
                        c.muted
                    }),
                    button("Details")
                        .padding([8, 12])
                        .style(c.quiet(self.workspace.connection == Some(id)))
                        .on_press(Message::Connection(id)),
                ]
                .spacing(16)
                .align_y(Alignment::Center)
            ]
            .spacing(18);
            if self.workspace.connection == Some(id) {
                card = card.push(iced::widget::rule::horizontal(1)).push(text(if id == 2 {
                    "Application inventory is available. This preview does not claim discovery or control of sessions inside the app."
                } else {
                    "Configuration is present in this sample. In the migrated app, setup will preview changes before applying them and retain an undo receipt. A fresh session must verify actual integration."
                }).size(13).color(c.muted));
            }
            list = list.push(
                container(card)
                    .padding(22)
                    .width(Fill)
                    .style(move |_| c.surface(c.ground, true)),
            );
        }
        list.into()
    }

    fn settings(&self, c: Colors) -> Element<'_, Message> {
        let w = &self.workspace;
        let appearance = row![
            button("Light")
                .padding([12, 28])
                .style(c.quiet(!w.dark))
                .on_press(Message::Dark(false)),
            button("Dark")
                .padding([12, 28])
                .style(c.quiet(w.dark))
                .on_press(Message::Dark(true)),
        ]
        .spacing(10);
        let populated = button("Populated")
            .padding([10, 18])
            .on_press(Message::Scene(false, false));
        let empty = button("Empty")
            .padding([10, 18])
            .on_press(Message::Scene(true, false));
        let disconnected = button("Disconnected")
            .padding([10, 18])
            .on_press(Message::Scene(false, true));
        let scenes: Element<'_, Message> = if self.width < 900.0 {
            column![populated, empty, disconnected].spacing(10).into()
        } else {
            row![populated, empty, disconnected].spacing(10).into()
        };
        column![
            strong("Appearance", 18), appearance, iced::widget::rule::horizontal(1),
            strong("Preview scenes", 18),
            text("Explore the proposed layout with sample data. Your real agents are unaffected.").size(14).color(c.muted), scenes,
            iced::widget::rule::horizontal(1), strong("Keyboard", 18),
            text("⌘ / Ctrl + 1–4   Switch sections\nTab / Shift + Tab   Focus text fields\nEsc   Close session details").size(14).color(c.muted),
            iced::widget::rule::horizontal(1), strong("App maintenance", 18),
            text("Installation, updates, retained versions, and connection diagnostics will move into Settings as the existing workflows are migrated.").size(14).color(c.muted),
        ].spacing(20).max_width(760).into()
    }
}
